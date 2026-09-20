//! Sonda: cria o contexto GL headless, renderiza num FBO e lê um pixel de volta.
#[path = "../video/egl.rs"]
mod egl;

fn main() {
    let gl_ctx = match egl::HeadlessGl::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FALHA: {e}");
            std::process::exit(1);
        }
    };
    println!("OpenGL: {} | {}", gl_ctx.renderer, gl_ctx.version);
    unsafe {
        let (mut fbo, mut tex) = (0, 0);
        gl::GenTextures(1, &mut tex);
        gl::BindTexture(gl::TEXTURE_2D, tex);
        gl::TexImage2D(
            gl::TEXTURE_2D,
            0,
            gl::RGBA8 as i32,
            320,
            224,
            0,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            std::ptr::null(),
        );
        gl::GenFramebuffers(1, &mut fbo);
        gl::BindFramebuffer(gl::FRAMEBUFFER, fbo);
        gl::FramebufferTexture2D(
            gl::FRAMEBUFFER,
            gl::COLOR_ATTACHMENT0,
            gl::TEXTURE_2D,
            tex,
            0,
        );
        println!(
            "FBO completo: {}",
            gl::CheckFramebufferStatus(gl::FRAMEBUFFER) == gl::FRAMEBUFFER_COMPLETE
        );
        gl::ClearColor(0.25, 0.5, 0.75, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        let mut px = [0u8; 4];
        gl::ReadPixels(
            10,
            10,
            1,
            1,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            px.as_mut_ptr() as *mut _,
        );
        println!("pixel lido: {:?} (esperado ~[64,128,191,255])", px);
    }
}
