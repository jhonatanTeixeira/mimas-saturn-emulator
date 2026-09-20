//! Contexto OpenGL headless via EGL "surfaceless" (sem janela, sem X/Wayland) e carga das
//! funções GL cruas. É a única parte que fala com o EGL; o resto do renderizador só usa `gl`.

use std::ffi::{CString, c_void};
use std::os::raw::{c_char, c_int};
use std::ptr::null_mut;

type EglDisplay = *mut c_void;
type EglConfig = *mut c_void;
type EglContext = *mut c_void;

const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_OPENGL_API: u32 = 0x30A2;
const EGL_NONE: i32 = 0x3038;
const EGL_SURFACE_TYPE: i32 = 0x3033;
const EGL_PBUFFER_BIT: i32 = 0x0001;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_OPENGL_BIT: i32 = 0x0008;
const EGL_CONTEXT_MAJOR_VERSION: i32 = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: i32 = 0x30FB;
const EGL_CONTEXT_OPENGL_PROFILE_MASK: i32 = 0x30FD;
const EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT: i32 = 0x0001;

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetPlatformDisplay(
        platform: u32,
        native: *mut c_void,
        attribs: *const isize,
    ) -> EglDisplay;
    fn eglInitialize(dpy: EglDisplay, major: *mut c_int, minor: *mut c_int) -> u32;
    fn eglBindAPI(api: u32) -> u32;
    fn eglChooseConfig(
        dpy: EglDisplay,
        attribs: *const i32,
        configs: *mut EglConfig,
        size: c_int,
        n: *mut c_int,
    ) -> u32;
    fn eglCreateContext(
        dpy: EglDisplay,
        cfg: EglConfig,
        share: EglContext,
        attribs: *const i32,
    ) -> EglContext;
    fn eglMakeCurrent(
        dpy: EglDisplay,
        draw: *mut c_void,
        read: *mut c_void,
        ctx: EglContext,
    ) -> u32;
    fn eglGetProcAddress(name: *const c_char) -> *const c_void;
    fn eglGetError() -> i32;
}

/// Mantém vivo o display/contexto EGL enquanto o renderizador existir.
pub struct HeadlessGl {
    _dpy: EglDisplay,
    _ctx: EglContext,
    pub renderer: String,
    pub version: String,
}

impl HeadlessGl {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let dpy =
                eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA, null_mut(), std::ptr::null());
            if dpy.is_null() {
                return Err(format!(
                    "eglGetPlatformDisplay falhou (erro {:#X})",
                    eglGetError()
                ));
            }
            let (mut maj, mut min) = (0, 0);
            if eglInitialize(dpy, &mut maj, &mut min) == 0 {
                return Err(format!("eglInitialize falhou (erro {:#X})", eglGetError()));
            }
            if eglBindAPI(EGL_OPENGL_API) == 0 {
                return Err("eglBindAPI(OpenGL) falhou".into());
            }
            let cfg_attribs = [
                EGL_SURFACE_TYPE,
                EGL_PBUFFER_BIT,
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_BIT,
                EGL_NONE,
            ];
            let mut cfg: EglConfig = null_mut();
            let mut n = 0;
            if eglChooseConfig(dpy, cfg_attribs.as_ptr(), &mut cfg, 1, &mut n) == 0 || n == 0 {
                return Err(format!(
                    "eglChooseConfig sem configuração (erro {:#X})",
                    eglGetError()
                ));
            }
            let ctx_attribs = [
                EGL_CONTEXT_MAJOR_VERSION,
                3,
                EGL_CONTEXT_MINOR_VERSION,
                3,
                EGL_CONTEXT_OPENGL_PROFILE_MASK,
                EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
                EGL_NONE,
            ];
            let ctx = eglCreateContext(dpy, cfg, null_mut(), ctx_attribs.as_ptr());
            if ctx.is_null() {
                return Err(format!(
                    "eglCreateContext falhou (erro {:#X})",
                    eglGetError()
                ));
            }
            if eglMakeCurrent(dpy, null_mut(), null_mut(), ctx) == 0 {
                return Err(format!(
                    "eglMakeCurrent (surfaceless) falhou (erro {:#X})",
                    eglGetError()
                ));
            }
            gl::load_with(|s| {
                let c = CString::new(s).unwrap();
                eglGetProcAddress(c.as_ptr())
            });
            let get = |name| {
                let p = gl::GetString(name);
                if p.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(p as *const c_char)
                        .to_string_lossy()
                        .into_owned()
                }
            };
            Ok(Self {
                _dpy: dpy,
                _ctx: ctx,
                renderer: get(gl::RENDERER),
                version: get(gl::VERSION),
            })
        }
    }
}
