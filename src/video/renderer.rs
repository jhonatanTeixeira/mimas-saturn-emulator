//! Renderizador OpenGL puro (headless). Dois passes:
//!   1) VDP1: percorre a lista de comandos e desenha sprites/polígonos num framebuffer R16UI;
//!   2) VDP2: um shader compõe NBG0-3 (células/bitmap), a camada de sprites (o framebuffer do
//!      VDP1) e o fundo, com prioridades e offset de cor, num alvo RGBA8 de 320x224.
//! A leitura (`glReadPixels`) devolve as linhas de baixo para cima — a mesma orientação das
//! capturas de referência (`stubs/captures`), então o PNG sai sem inverter.

use std::ffi::CString;

use super::egl::HeadlessGl;
use crate::devices::vdp1::Vdp1;
use crate::devices::vdp2::Vdp2;

pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 224;
const FB_W: i32 = 512;
const FB_H: i32 = 256;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
    gour: [f32; 3],
    prm: [u32; 4],
}

const VDP1_VS: &str = r#"#version 330 core
layout(location=0) in vec2 aPos;
layout(location=1) in vec2 aUV;
layout(location=2) in vec3 aGour;
layout(location=3) in uvec4 aPrm;
noperspective out vec2 vUV;
noperspective out vec3 vG;
flat out uvec4 vP;
void main() {
    vUV = aUV; vG = aGour; vP = aPrm;
    gl_Position = vec4(aPos.x / 256.0 - 1.0, aPos.y / 128.0 - 1.0, 0.0, 1.0);
}
"#;

const VDP1_FS: &str = r#"#version 330 core
uniform usampler2D uVram;
noperspective in vec2 vUV;
noperspective in vec3 vG;
flat in uvec4 vP;   // x=pmod, y=colr, z=srca(bytes), w=width | height<<8 | textured<<24 | gouraud<<25
layout(location=0) out uint outv;

uint v8(uint a) { a &= 0x7FFFFu; return texelFetch(uVram, ivec2(int(a & 1023u), int(a >> 10)), 0).r; }
uint v16(uint a) { return (v8(a) << 8) | v8(a + 1u); }

void main() {
    uint pmod = vP.x, colr = vP.y, srca = vP.z;
    uint w = vP.w & 0xFFu; uint h = (vP.w >> 8) & 0xFFu;
    bool textured = ((vP.w >> 24) & 1u) != 0u;
    bool gouraud = ((vP.w >> 25) & 1u) != 0u;
    uint mode = (pmod >> 3) & 7u;
    bool spd = (pmod & 0x40u) != 0u;
    uint val;
    if (textured) {
        int tx = clamp(int(floor(vUV.x)), 0, int(w) * 8 - 1);
        int ty = clamp(int(floor(vUV.y)), 0, int(h) - 1);
        uint tw = w * 8u;
        uint idx;
        if (mode == 0u || mode == 1u) {
            uint b = v8(srca + uint(ty) * (tw / 2u) + uint(tx) / 2u);
            idx = ((tx & 1) == 0) ? (b >> 4) : (b & 15u);
            if (idx == 0u && !spd) discard;
            if (mode == 0u) val = (colr & 0xFFF0u) | idx;
            else val = v16(colr * 8u + idx * 2u);
        } else if (mode == 5u) {
            val = v16(srca + (uint(ty) * tw + uint(tx)) * 2u);
            if (val == 0u && !spd) discard;
        } else {
            idx = v8(srca + uint(ty) * tw + uint(tx));
            if (idx == 0u && !spd) discard;
            uint m = (mode == 2u) ? 0x3Fu : (mode == 3u ? 0x7Fu : 0xFFu);
            val = (colr & ~m) | (idx & m);
        }
    } else {
        val = colr;
    }
    if (gouraud && (val & 0x8000u) != 0u) {
        ivec3 c = ivec3(int(val & 31u), int((val >> 5) & 31u), int((val >> 10) & 31u));
        c = clamp(c + ivec3(round(vG)) - ivec3(16), ivec3(0), ivec3(31));
        val = 0x8000u | uint(c.x) | (uint(c.y) << 5) | (uint(c.z) << 10);
    }
    outv = val;
}
"#;

const VDP2_VS: &str = r#"#version 330 core
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const VDP2_FS: &str = r#"#version 330 core
uniform usampler2D uVram;
uniform usampler2D uCram;
uniform usampler2D uSpr;
uniform uint uReg[144];
uniform int uOnly;
out vec4 fragColor;

uint reg(uint off) { return uReg[off >> 1]; }
uint vram8(uint a) { a &= 0x7FFFFu; return texelFetch(uVram, ivec2(int(a & 1023u), int(a >> 10)), 0).r; }
uint vram16(uint a) { return (vram8(a) << 8) | vram8(a + 1u); }
uint vram32(uint a) { return (vram16(a) << 16) | vram16(a + 2u); }
uint cram16(uint idx) {
    uint a = (idx & 0x3FFu) * 2u;
    return (texelFetch(uCram, ivec2(int(a & 1023u), int(a >> 10)), 0).r << 8) | texelFetch(uCram, ivec2(int((a + 1u) & 1023u), int((a + 1u) >> 10)), 0).r;
}
vec3 rgb555(uint c) {
    uint r = c & 31u, g = (c >> 5) & 31u, b = (c >> 10) & 31u;
    return vec3(float(r << 3), float(g << 3), float(b << 3));
}

// Camada de fundo de células/bitmap. Devolve prioridade (0 = sem pixel) e cor em 0..255.
bool nbg(int n, ivec2 p, out uint prio, out vec3 col) {
    prio = 0u; col = vec3(0.0);
    if (((reg(0x20u) >> uint(n)) & 1u) == 0u) return false;
    uint chctl = (n < 2) ? reg(0x28u) : reg(0x2Au);
    uint chsz = 0u, chcn = 0u, bmsz = 0u; bool bmen = false;
    if (n == 0) { chsz = chctl & 1u; bmen = ((chctl >> 1) & 1u) != 0u; bmsz = (chctl >> 2) & 3u; chcn = (chctl >> 4) & 7u; }
    else if (n == 1) { chsz = (chctl >> 8) & 1u; bmen = ((chctl >> 9) & 1u) != 0u; bmsz = (chctl >> 10) & 3u; chcn = (chctl >> 12) & 3u; }
    else if (n == 2) { chsz = chctl & 1u; chcn = (chctl >> 1) & 1u; }
    else { chsz = (chctl >> 4) & 1u; chcn = (chctl >> 5) & 1u; }
    uint pr = (n == 0) ? (reg(0xF8u) & 7u) : (n == 1) ? ((reg(0xF8u) >> 8) & 7u) : (n == 2) ? (reg(0xFAu) & 7u) : ((reg(0xFAu) >> 8) & 7u);
    if (pr == 0u) return false;
    int scx, scy;
    if (n == 0) { scx = int(reg(0x70u) & 0x7FFu); scy = int(reg(0x74u) & 0x7FFu); }
    else if (n == 1) { scx = int(reg(0x80u) & 0x7FFu); scy = int(reg(0x84u) & 0x7FFu); }
    else if (n == 2) { scx = int(reg(0x90u) & 0x7FFu); scy = int(reg(0x92u) & 0x7FFu); }
    else { scx = int(reg(0x94u) & 0x7FFu); scy = int(reg(0x96u) & 0x7FFu); }
    uint caos = (reg(0xE4u) >> (4u * uint(n))) & 7u;
    // TPON = 0 habilita a transparência do código de cor 0 (1 a desabilita).
    bool tpon = ((reg(0x20u) >> (8u + uint(n))) & 1u) == 0u;
    int mx = p.x + scx, my = p.y + scy;
    uint cidx = 0u; uint idx = 0u; bool direct = false; uint dcol = 0u;
    if (bmen) {
        uint bw = (bmsz & 2u) != 0u ? 1024u : 512u; uint bh = (bmsz & 1u) != 0u ? 512u : 256u;
        uint bx = uint(mx) % bw, by = uint(my) % bh;
        uint base = ((reg(0x3Cu) >> (4u * uint(n))) & 7u) << 17;
        uint pal = (n == 0) ? (reg(0x2Cu) & 7u) : ((reg(0x2Cu) >> 8) & 7u);
        if (chcn == 0u) { uint b = vram8(base + (by * bw + bx) / 2u); idx = ((bx & 1u) == 0u) ? (b >> 4) : (b & 15u); cidx = (pal << 4) | idx; }
        else if (chcn == 1u) { idx = vram8(base + by * bw + bx); cidx = (pal << 4) | idx; cidx = ((pal & 7u) << 8) | idx; }
        else if (chcn == 2u) { idx = vram16(base + (by * bw + bx) * 2u) & 0x7FFu; cidx = idx; }
        else { dcol = vram16(base + (by * bw + bx) * 2u); direct = true; idx = (dcol >> 15) & 1u; }
    } else {
        uint pncn = reg(0x30u + 2u * uint(n));
        bool pnb = (pncn & 0x8000u) != 0u;
        uint pnsz = pnb ? 2u : 4u;
        int cs = (chsz == 1u) ? 16 : 8;
        int cpp = 512 / cs;
        uint plsz = (reg(0x3Au) >> (2u * uint(n))) & 3u;
        int wp = ((plsz & 1u) != 0u) ? 2 : 1, hp = ((plsz & 2u) != 0u) ? 2 : 1;
        mx &= 512 * wp - 1; my &= 512 * hp - 1;
        int letter = (my >> 9) * 2 + (mx >> 9);
        uint mreg = reg(0x40u + 4u * uint(n) + uint(letter >> 1) * 2u);
        uint mp = ((letter & 1) != 0) ? ((mreg >> 8) & 0x3Fu) : (mreg & 0x3Fu);
        uint mapnum = (((reg(0x3Cu) >> (4u * uint(n))) & 7u) << 6) | mp;
        uint planeBytes = uint(cpp * cpp) * pnsz;
        int cx = (mx & 511) / cs, cy = (my & 511) / cs;
        uint pa = mapnum * planeBytes + uint(cy * cpp + cx) * pnsz;
        uint chr, pal; bool vf, hf;
        if (!pnb) { uint pn = vram32(pa); vf = ((pn >> 31) & 1u) != 0u; hf = ((pn >> 30) & 1u) != 0u; pal = (pn >> 16) & 0x7Fu; chr = pn & 0x7FFFu; }
        else {
            uint pn = vram16(pa); uint supp = pncn & 0x3FFu;
            vf = ((pn >> 11) & 1u) != 0u; hf = ((pn >> 10) & 1u) != 0u;
            chr = (pn & 0x3FFu) | ((supp & 0x1Fu) << 10);
            pal = ((supp >> 5) << 4) | (pn >> 12);
        }
        int dx = mx & (cs - 1), dy = my & (cs - 1);
        if (hf) dx = cs - 1 - dx;
        if (vf) dy = cs - 1 - dy;
        uint cellbytes = (chcn == 0u) ? 32u : (chcn == 1u ? 64u : 128u);
        uint cell = 0u;
        if (cs == 16) { cell = uint((dy >> 3) * 2 + (dx >> 3)); dx &= 7; dy &= 7; }
        uint base = chr * 32u + cell * cellbytes;
        uint udx = uint(dx), udy = uint(dy);
        if (chcn == 0u) { uint b = vram8(base + udy * 4u + udx / 2u); idx = ((udx & 1u) == 0u) ? (b >> 4) : (b & 15u); cidx = ((pal & 0x7Fu) << 4) | idx; }
        else if (chcn == 1u) { idx = vram8(base + udy * 8u + udx); cidx = ((pal & 0x70u) << 4) | idx; }
        else if (chcn == 2u) { idx = vram16(base + (udy * 8u + udx) * 2u) & 0x7FFu; cidx = idx; }
        else { dcol = vram16(base + (udy * 8u + udx) * 2u); direct = true; idx = (dcol >> 15) & 1u; }
    }
    if (direct) { if (idx == 0u) return false; col = rgb555(dcol); prio = pr; return true; }
    if (tpon && idx == 0u) return false;
    col = rgb555(cram16(cidx + caos * 256u));
    prio = pr;
    return true;
}

bool sprite(ivec2 p, out uint prio, out vec3 col) {
    prio = 0u; col = vec3(0.0);
    uint sv = texelFetch(uSpr, p, 0).r;
    if (sv == 0u) return false;
    uint spctl = reg(0xE0u);
    bool mixed = (spctl & 0x20u) != 0u;
    uint pi = 0u;
    if (mixed && (sv & 0x8000u) != 0u) { col = rgb555(sv); }
    else {
        uint dc = sv & 0x7FFu;
        pi = mixed ? ((sv >> 13) & 3u) : ((sv >> 13) & 7u);
        uint spcaos = (reg(0xE6u) >> 4) & 7u;
        col = rgb555(cram16(dc + spcaos * 256u));
    }
    uint r = reg(0xF0u + (pi >> 1) * 2u);
    prio = ((pi & 1u) != 0u) ? ((r >> 8) & 7u) : (r & 7u);
    return prio != 0u;
}

vec3 coloroffset(uint bit, vec3 c) {
    if (((reg(0x110u) >> bit) & 1u) == 0u) return c;
    uint sel = (reg(0x112u) >> bit) & 1u;
    uint o = (sel == 0u) ? 0x114u : 0x11Au;
    vec3 off;
    for (int i = 0; i < 3; i++) {
        int v = int(reg(o + uint(i) * 2u) & 0x1FFu);
        if (v >= 256) v -= 512;
        off[i] = float(v);
    }
    return clamp(c + off, 0.0, 255.0);
}

// Razão de cálculo de cor (0..31) da camada `n` (0-3 = NBG0-3) — 5 bits nos registradores CCRNA/CCRNB.
uint ccratio(uint n) {
    uint r = (n < 2u) ? reg(0x108u) : reg(0x10Au);
    return ((n & 1u) == 0u) ? (r & 31u) : ((r >> 8) & 31u);
}

void main() {
    ivec2 p = ivec2(int(gl_FragCoord.x), 223 - int(gl_FragCoord.y));
    if ((reg(0x00u) & 0x8000u) == 0u) { fragColor = vec4(0.0, 0.0, 0.0, 1.0); return; }
    uint baddr = (((reg(0xACu) & 7u) << 16) | reg(0xAEu)) << 1;
    vec3 back = rgb555(vram16(baddr));
    // topo e segunda camada (a segunda entra na mistura de cálculo de cor).
    uint tp = 0u, sp = 0u; vec3 tc = back, sc = back; uint tid = 5u, sid = 5u;
    uint lp; vec3 lc;
    for (uint id = 0u; id < 5u; id++) {
        bool hit = false;
        if (id == 0u) { if (uOnly < 0 || uOnly == 4) hit = sprite(p, lp, lc); }
        else { if (uOnly < 0 || uOnly == int(id) - 1) hit = nbg(int(id) - 1, p, lp, lc); }
        if (!hit) continue;
        uint lid = (id == 0u) ? 6u : (id - 1u);
        if (lp > tp) { sp = tp; sc = tc; sid = tid; tp = lp; tc = lc; tid = lid; }
        else if (lp > sp) { sp = lp; sc = lc; sid = lid; }
    }
    vec3 c = tc;
    if (tid < 4u && ((reg(0xECu) >> tid) & 1u) != 0u) {
        float r = float(ccratio(tid));
        c = floor((tc * (32.0 - r) + sc * r) / 32.0);
    }
    c = coloroffset(tid, c);
    fragColor = vec4(c / 255.0, 1.0);
}
"#;

pub struct GlRenderer {
    _gl: HeadlessGl,
    prog1: u32,
    prog2: u32,
    vao: u32,
    vbo: u32,
    empty_vao: u32,
    tex_v1: u32,
    tex_v2: u32,
    tex_cram: u32,
    tex_spr: u32,
    fbo_spr: u32,
    tex_out: u32,
    fbo_out: u32,
    pub pixels: Vec<u8>,
}

unsafe fn compile(kind: u32, src: &str) -> Result<u32, String> {
    unsafe {
        let s = gl::CreateShader(kind);
        let c = CString::new(src).unwrap();
        gl::ShaderSource(s, 1, &c.as_ptr(), std::ptr::null());
        gl::CompileShader(s);
        let mut ok = 0;
        gl::GetShaderiv(s, gl::COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut buf = vec![0u8; 4096];
            let mut len = 0;
            gl::GetShaderInfoLog(s, 4096, &mut len, buf.as_mut_ptr() as *mut _);
            return Err(format!(
                "shader: {}",
                String::from_utf8_lossy(&buf[..len as usize])
            ));
        }
        Ok(s)
    }
}

unsafe fn link(vs: &str, fs: &str) -> Result<u32, String> {
    unsafe {
        let (v, f) = (
            compile(gl::VERTEX_SHADER, vs)?,
            compile(gl::FRAGMENT_SHADER, fs)?,
        );
        let p = gl::CreateProgram();
        gl::AttachShader(p, v);
        gl::AttachShader(p, f);
        gl::LinkProgram(p);
        let mut ok = 0;
        gl::GetProgramiv(p, gl::LINK_STATUS, &mut ok);
        if ok == 0 {
            return Err("link do programa falhou".into());
        }
        Ok(p)
    }
}

unsafe fn tex_u8(w: i32, h: i32) -> u32 {
    unsafe {
        let mut t = 0;
        gl::GenTextures(1, &mut t);
        gl::BindTexture(gl::TEXTURE_2D, t);
        gl::TexImage2D(
            gl::TEXTURE_2D,
            0,
            gl::R8UI as i32,
            w,
            h,
            0,
            gl::RED_INTEGER,
            gl::UNSIGNED_BYTE,
            std::ptr::null(),
        );
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
        t
    }
}

fn u16_at(m: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([m[o & (m.len() - 1)], m[(o + 1) & (m.len() - 1)]])
}

impl GlRenderer {
    pub fn new() -> Result<Self, String> {
        let egl = HeadlessGl::new()?;
        unsafe {
            let prog1 = link(VDP1_VS, VDP1_FS)?;
            let prog2 = link(VDP2_VS, VDP2_FS)?;
            let (mut vao, mut vbo, mut empty_vao) = (0, 0, 0);
            gl::GenVertexArrays(1, &mut vao);
            gl::GenVertexArrays(1, &mut empty_vao);
            gl::GenBuffers(1, &mut vbo);
            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            let stride = std::mem::size_of::<Vertex>() as i32;
            gl::EnableVertexAttribArray(0);
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, stride, 0 as *const _);
            gl::EnableVertexAttribArray(1);
            gl::VertexAttribPointer(1, 2, gl::FLOAT, gl::FALSE, stride, 8 as *const _);
            gl::EnableVertexAttribArray(2);
            gl::VertexAttribPointer(2, 3, gl::FLOAT, gl::FALSE, stride, 16 as *const _);
            gl::EnableVertexAttribArray(3);
            gl::VertexAttribIPointer(3, 4, gl::UNSIGNED_INT, stride, 28 as *const _);

            let tex_v1 = tex_u8(1024, 512);
            let tex_v2 = tex_u8(1024, 512);
            let tex_cram = tex_u8(1024, 4);
            let mut tex_spr = 0;
            gl::GenTextures(1, &mut tex_spr);
            gl::BindTexture(gl::TEXTURE_2D, tex_spr);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::R16UI as i32,
                FB_W,
                FB_H,
                0,
                gl::RED_INTEGER,
                gl::UNSIGNED_SHORT,
                std::ptr::null(),
            );
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
            let mut fbo_spr = 0;
            gl::GenFramebuffers(1, &mut fbo_spr);
            gl::BindFramebuffer(gl::FRAMEBUFFER, fbo_spr);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                tex_spr,
                0,
            );
            if gl::CheckFramebufferStatus(gl::FRAMEBUFFER) != gl::FRAMEBUFFER_COMPLETE {
                return Err("FBO do VDP1 incompleto".into());
            }
            let mut tex_out = 0;
            gl::GenTextures(1, &mut tex_out);
            gl::BindTexture(gl::TEXTURE_2D, tex_out);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA8 as i32,
                WIDTH as i32,
                HEIGHT as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            let mut fbo_out = 0;
            gl::GenFramebuffers(1, &mut fbo_out);
            gl::BindFramebuffer(gl::FRAMEBUFFER, fbo_out);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                tex_out,
                0,
            );
            if gl::CheckFramebufferStatus(gl::FRAMEBUFFER) != gl::FRAMEBUFFER_COMPLETE {
                return Err("FBO de saída incompleto".into());
            }
            Ok(Self {
                _gl: egl,
                prog1,
                prog2,
                vao,
                vbo,
                empty_vao,
                tex_v1,
                tex_v2,
                tex_cram,
                tex_spr,
                fbo_spr,
                tex_out,
                fbo_out,
                pixels: vec![0; WIDTH * HEIGHT * 4],
            })
        }
    }

    pub fn gpu(&self) -> String {
        format!("{} | {}", self._gl.renderer, self._gl.version)
    }

    unsafe fn upload(tex: u32, w: i32, h: i32, data: &[u8]) {
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, tex);
            gl::TexSubImage2D(
                gl::TEXTURE_2D,
                0,
                0,
                0,
                w,
                h,
                gl::RED_INTEGER,
                gl::UNSIGNED_BYTE,
                data.as_ptr() as *const _,
            );
        }
    }

    /// Renderiza um quadro completo e deixa o resultado em `self.pixels` (RGBA, linhas de baixo para cima).
    pub fn render(&mut self, v1: &Vdp1, v2: &Vdp2) {
        unsafe {
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            Self::upload(self.tex_v1, 1024, 512, &v1.vram);
            Self::upload(self.tex_v2, 1024, 512, &v2.vram);
            Self::upload(self.tex_cram, 1024, 4, &v2.cram);
            self.draw_vdp1(v1);
            self.compose_vdp2(v2);
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo_out);
            gl::ReadPixels(
                0,
                0,
                WIDTH as i32,
                HEIGHT as i32,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                self.pixels.as_mut_ptr() as *mut _,
            );
        }
    }

    unsafe fn draw_vdp1(&mut self, v1: &Vdp1) {
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo_spr);
            gl::Viewport(0, 0, FB_W, FB_H);
            gl::Disable(gl::SCISSOR_TEST);
            gl::ClearBufferuiv(gl::COLOR, 0, [v1.ewdr as u32, 0, 0, 0].as_ptr());
            gl::UseProgram(self.prog1);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.tex_v1);
            gl::Uniform1i(
                gl::GetUniformLocation(self.prog1, b"uVram\0".as_ptr() as *const _),
                0,
            );
            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::Enable(gl::SCISSOR_TEST);

            let vram = &v1.vram;
            let (mut lx, mut ly) = (0i32, 0i32);
            let (mut sys_x, mut sys_y) = (FB_W - 1, FB_H - 1);
            let (mut usr_x0, mut usr_y0, mut usr_x1, mut usr_y1) = (0, 0, FB_W - 1, FB_H - 1);
            let mut addr = 0usize;
            let mut return_to: Option<usize> = None;
            for _ in 0..8192 {
                if addr + 32 > vram.len() {
                    break;
                }
                let w = |o: usize| u16_at(vram, addr + o);
                let s = |o: usize| u16_at(vram, addr + o) as i16 as i32;
                let ctrl = w(0);
                if ctrl & 0x8000 != 0 {
                    break;
                }
                if ctrl & 0x4000 == 0 {
                    let kind = ctrl & 0xF;
                    let pmod = w(4) as u32;
                    let colr = w(6) as u32;
                    let srca = (w(8) as u32) * 8;
                    let size = w(0xA) as u32;
                    let tw = (size >> 8) & 0x3F;
                    let th = size & 0xFF;
                    let grda = (w(0x1C) as usize) * 8;
                    let clip_on = ctrl & 0x0400 != 0;
                    let (sx, sy) = (lx, ly);
                    let mut quad: Option<[(f32, f32); 4]> = None;
                    let mut textured = false;
                    match kind {
                        0 => {
                            let (x, y) = (s(0xC) + sx, s(0xE) + sy);
                            let (ww, hh) = ((tw * 8) as i32, th as i32);
                            quad = Some([
                                (x as f32, y as f32),
                                ((x + ww) as f32, y as f32),
                                ((x + ww) as f32, (y + hh) as f32),
                                (x as f32, (y + hh) as f32),
                            ]);
                            textured = true;
                        }
                        1 => {
                            let (xa, ya) = (s(0xC) + sx, s(0xE) + sy);
                            let zp = (ctrl >> 8) & 0xF;
                            let (x0, y0, x1, y1);
                            if zp == 0 {
                                let (xc, yc) = (s(0x14) + sx, s(0x16) + sy);
                                x0 = xa.min(xc);
                                y0 = ya.min(yc);
                                x1 = xa.max(xc);
                                y1 = ya.max(yc);
                            } else {
                                let (dw, dh) = (s(0x10), s(0x12));
                                let ax = match zp & 3 {
                                    1 => 0,
                                    2 => -dw / 2,
                                    _ => -dw,
                                };
                                let ay = match zp >> 2 {
                                    1 => 0,
                                    2 => -dh / 2,
                                    _ => -dh,
                                };
                                x0 = xa + ax;
                                y0 = ya + ay;
                                x1 = x0 + dw;
                                y1 = y0 + dh;
                            }
                            quad = Some([
                                (x0 as f32, y0 as f32),
                                ((x1 + 1) as f32, y0 as f32),
                                ((x1 + 1) as f32, (y1 + 1) as f32),
                                (x0 as f32, (y1 + 1) as f32),
                            ]);
                            textured = true;
                        }
                        2 => {
                            let p = |i: usize| {
                                (
                                    (s(0xC + 4 * i) + sx) as f32 + 0.5,
                                    (s(0xE + 4 * i) + sy) as f32 + 0.5,
                                )
                            };
                            quad = Some([p(0), p(1), p(2), p(3)]);
                            textured = true;
                        }
                        4 => {
                            let p = |i: usize| {
                                (
                                    (s(0xC + 4 * i) + sx) as f32 + 0.5,
                                    (s(0xE + 4 * i) + sy) as f32 + 0.5,
                                )
                            };
                            quad = Some([p(0), p(1), p(2), p(3)]);
                        }
                        8 => {
                            usr_x0 = s(0xC);
                            usr_y0 = s(0xE);
                            usr_x1 = s(0x14);
                            usr_y1 = s(0x16);
                        }
                        9 => {
                            sys_x = s(0x14);
                            sys_y = s(0x16);
                        }
                        10 => {
                            lx = s(0xC);
                            ly = s(0xE);
                        }
                        _ => {}
                    }
                    if let Some(q) = quad {
                        let (mut cx0, mut cy0, mut cx1, mut cy1) = (0, 0, sys_x, sys_y);
                        if clip_on {
                            cx0 = cx0.max(usr_x0);
                            cy0 = cy0.max(usr_y0);
                            cx1 = cx1.min(usr_x1);
                            cy1 = cy1.min(usr_y1);
                        }
                        if cx1 >= cx0 && cy1 >= cy0 {
                            gl::Scissor(
                                cx0.max(0),
                                cy0.max(0),
                                (cx1 - cx0 + 1).max(0),
                                (cy1 - cy0 + 1).max(0),
                            );
                            let hflip = ctrl & 0x10 != 0;
                            let vflip = ctrl & 0x20 != 0;
                            let (ww, hh) = ((tw * 8) as f32, th as f32);
                            let (u0, u1) = if hflip { (ww, 0.0) } else { (0.0, ww) };
                            let (v0, v1) = if vflip { (hh, 0.0) } else { (0.0, hh) };
                            let uvs = [(u0, v0), (u1, v0), (u1, v1), (u0, v1)];
                            let gouraud = pmod & 4 != 0;
                            let mut gour = [[16.0f32; 3]; 4];
                            if gouraud {
                                for (i, g) in gour.iter_mut().enumerate() {
                                    let c = u16_at(vram, grda + i * 2) as u32;
                                    *g = [
                                        (c & 31) as f32,
                                        ((c >> 5) & 31) as f32,
                                        ((c >> 10) & 31) as f32,
                                    ];
                                }
                            }
                            let flags = (tw * 8 / 8)
                                | (th << 8)
                                | ((textured as u32) << 24)
                                | ((gouraud as u32) << 25);
                            let mk = |i: usize| Vertex {
                                pos: [q[i].0, q[i].1],
                                uv: [uvs[i].0, uvs[i].1],
                                gour: gour[i],
                                prm: [pmod, colr, srca, flags],
                            };
                            let tri = [mk(0), mk(1), mk(2), mk(0), mk(2), mk(3)];
                            gl::BufferData(
                                gl::ARRAY_BUFFER,
                                std::mem::size_of_val(&tri) as isize,
                                tri.as_ptr() as *const _,
                                gl::STREAM_DRAW,
                            );
                            gl::DrawArrays(gl::TRIANGLES, 0, 6);
                        }
                    }
                }
                let link = w(2) as usize * 8;
                let next = addr + 32;
                addr = match (ctrl >> 12) & 3 {
                    0 => next,
                    1 => link,
                    2 => {
                        return_to = Some(next);
                        link
                    }
                    _ => match return_to.take() {
                        Some(r) => r,
                        None => next,
                    },
                };
            }
            gl::Disable(gl::SCISSOR_TEST);
        }
    }

    unsafe fn compose_vdp2(&mut self, v2: &Vdp2) {
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo_out);
            gl::Viewport(0, 0, WIDTH as i32, HEIGHT as i32);
            gl::UseProgram(self.prog2);
            for (unit, tex, name) in [
                (0, self.tex_v2, "uVram"),
                (1, self.tex_cram, "uCram"),
                (2, self.tex_spr, "uSpr"),
            ] {
                gl::ActiveTexture(gl::TEXTURE0 + unit);
                gl::BindTexture(gl::TEXTURE_2D, tex);
                let n = CString::new(name).unwrap();
                gl::Uniform1i(gl::GetUniformLocation(self.prog2, n.as_ptr()), unit as i32);
            }
            let regs: Vec<u32> = (0..144).map(|i| v2.reg16(i * 2) as u32).collect();
            gl::Uniform1uiv(
                gl::GetUniformLocation(self.prog2, b"uReg\0".as_ptr() as *const _),
                144,
                regs.as_ptr(),
            );
            let only: i32 = std::env::var("VDP_LAYER")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(-1);
            gl::Uniform1i(
                gl::GetUniformLocation(self.prog2, b"uOnly\0".as_ptr() as *const _),
                only,
            );
            gl::BindVertexArray(self.empty_vao);
            gl::DrawArrays(gl::TRIANGLES, 0, 3);
        }
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        let _ = (self.tex_out, self.prog1, self.prog2);
    }
}
