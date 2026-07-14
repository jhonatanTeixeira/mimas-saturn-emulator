//! Vision-based milestone test: has a headless BIOS boot run visually
//! reached the real Saturn BIOS's CD Player screen?
//!
//! Not a pixel-diff (too brittle against a renderer that's still being
//! built) and not OCR (the milestone is a whole scene, not a specific
//! string) -- CLIP image-embedding cosine similarity against a real
//! reference screenshot (`fixtures/cd_player_screen.jpg`) is a holistic
//! "does this image depict the same scene" check that tolerates the
//! color/scale noise a pixel-diff would choke on.
//!
//! Deliberately outside the main workspace (see `Cargo.toml`'s comment) --
//! run explicitly:
//!   MIMAS_BIOS_PATH=/path/to/real/bios.bin cargo test
//! Skips gracefully (no panic) if `MIMAS_BIOS_PATH` isn't set.
//!
//! Expected to fail today: VDP1 sprite rendering and VDP2 NBG tile
//! decoding (`ROADMAP.md` M4/M5) don't exist yet, only the flat backdrop
//! color renders. This is a target regression test, not a check that's
//! meant to pass immediately -- it gives M4/M5 work a concrete, objective
//! "done" signal to aim at.

use std::time::{Duration, Instant};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::clip::{ClipConfig, ClipModel};
use saturn_core::SaturnSystem;

const WINDOW: Duration = Duration::from_secs(60);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// Uncalibrated placeholder -- there's no real passing frame yet to
/// calibrate against (M4/M5 aren't implemented), and this pipeline's
/// preprocessing (a flat [0,255]->[-1,1] affine, matching candle's own
/// CLIP example) isn't official CLIP mean/std normalization, so don't
/// assume this threshold matches published CLIP similarity benchmarks.
/// Recalibrate once a real passing frame exists to compare against.
const SIMILARITY_THRESHOLD: f32 = 0.85;

#[test]
fn reaches_cd_player_screen() {
    let Ok(bios_path) = std::env::var("MIMAS_BIOS_PATH") else {
        eprintln!("MIMAS_BIOS_PATH not set -- skipping (needs a real, user-supplied Saturn BIOS)");
        return;
    };

    let device = Device::Cpu;
    let model = load_clip_model(&device);

    let reference_path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/cd_player_screen.jpg");
    let reference = image::open(reference_path).expect("load reference milestone image");
    let reference_embedding = embed_image(&model, &reference, &device);

    // Same SaturnSystem calls saturn-frontend-native/src/main.rs already
    // uses (new/load_bios/start), same vdp2_frame ArcSwap Core 3's loop
    // (saturn-core/src/lib.rs) publishes into every ~16.6ms.
    let mut system = SaturnSystem::new();
    system.load_bios(std::fs::read(&bios_path).expect("read real BIOS file"));
    system.start();

    let mut best_similarity = f32::MIN;
    let start = Instant::now();
    while start.elapsed() < WINDOW {
        std::thread::sleep(SAMPLE_INTERVAL);
        let frame = system.vdp2_frame.load();
        let frame_img = frame_to_dynamic_image(&frame);
        let embedding = embed_image(&model, &frame_img, &device);
        let similarity = cosine_similarity(&embedding, &reference_embedding);
        if similarity > best_similarity {
            best_similarity = similarity;
        }
    }
    system.shutdown();

    assert!(
        best_similarity > SIMILARITY_THRESHOLD,
        "CD Player milestone not reached: best similarity {best_similarity} over {WINDOW:?} \
         did not exceed {SIMILARITY_THRESHOLD} (expected today -- VDP1/VDP2 tile rendering, \
         ROADMAP.md M4/M5, isn't implemented yet)"
    );
}

fn load_clip_model(device: &Device) -> ClipModel {
    let api = hf_hub::api::sync::Api::new().expect("hf-hub API init");
    let repo = api.repo(hf_hub::Repo::with_revision(
        "openai/clip-vit-base-patch32".to_string(),
        hf_hub::RepoType::Model,
        "refs/pr/15".to_string(),
    ));
    let model_file = repo
        .get("model.safetensors")
        .expect("fetch/cache CLIP model weights from Hugging Face Hub");
    // Safe: model.safetensors comes from the pinned, known-good HF repo
    // revision above, not arbitrary user input.
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(&[model_file], DType::F32, device)
            .expect("load CLIP weights into VarBuilder")
    };
    ClipModel::new(vb, &ClipConfig::vit_base_patch32()).expect("construct CLIP model")
}

fn frame_to_dynamic_image(frame: &saturn_core::vdp::Framebuffer) -> image::DynamicImage {
    let mut rgb8 = Vec::with_capacity(frame.pixels.len() * 3);
    for &p in &frame.pixels {
        rgb8.push(((p >> 16) & 0xFF) as u8);
        rgb8.push(((p >> 8) & 0xFF) as u8);
        rgb8.push((p & 0xFF) as u8);
    }
    let img = image::RgbImage::from_raw(frame.width as u32, frame.height as u32, rgb8)
        .expect("frame dimensions must match pixel buffer length");
    image::DynamicImage::ImageRgb8(img)
}

/// `ClipModel::get_image_features` does not L2-normalize its output
/// (normalization only happens inside `ClipModel::forward`, confirmed
/// against candle's source) -- callers must do the full cosine formula,
/// not a plain dot product; see `cosine_similarity` below.
fn embed_image(model: &ClipModel, img: &image::DynamicImage, device: &Device) -> Vec<f32> {
    let image_size = 224; // ClipConfig::vit_base_patch32()'s image_size
    let resized = img
        .resize_to_fill(image_size, image_size, image::imageops::FilterType::Triangle)
        .to_rgb8()
        .into_raw();
    let tensor = Tensor::from_vec(resized, (image_size as usize, image_size as usize, 3), device)
        .expect("build tensor from resized image")
        .permute((2, 0, 1))
        .expect("permute HWC -> CHW")
        .to_dtype(DType::F32)
        .expect("convert to f32")
        .affine(2. / 255., -1.)
        .expect("normalize pixel range")
        .unsqueeze(0)
        .expect("add batch dimension");
    model
        .get_image_features(&tensor)
        .expect("compute CLIP image features")
        .squeeze(0)
        .expect("remove batch dimension")
        .to_vec1::<f32>()
        .expect("extract embedding as Vec<f32>")
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm_a * norm_b)
}
