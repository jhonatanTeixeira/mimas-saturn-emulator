// Standalone smoke test for the windowing infrastructure, independent of
// the emulator itself: proves minifb can open a real window and blit
// frames on this machine before the real VDP2 pipeline exists to feed it.
use minifb::{Window, WindowOptions};

const WIDTH: usize = 352;
const HEIGHT: usize = 240;

fn main() {
    let mut window = Window::new(
        "Mimas window smoke test",
        WIDTH,
        HEIGHT,
        WindowOptions::default(),
    )
    .expect("failed to open window");

    let mut buffer: Vec<u32> = vec![0; WIDTH * HEIGHT];
    let mut frame: u32 = 0;
    window.set_target_fps(60);

    while window.is_open() && frame < 180 {
        // Simple animated gradient so it's obvious frames are actually
        // updating, not just a static image left over from init.
        let offset = frame % 256;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let r = ((x as u32 + offset) % 256) as u32;
                let g = ((y as u32 + offset) % 256) as u32;
                let b = 128u32;
                buffer[y * WIDTH + x] = (r << 16) | (g << 8) | b;
            }
        }
        window
            .update_with_buffer(&buffer, WIDTH, HEIGHT)
            .expect("failed to present frame");
        frame += 1;
    }
    println!("window smoke test: presented {frame} frames successfully");
}
