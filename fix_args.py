import re

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

# For draw_line_impl and draw_quad, let's just group the coordinates into an array or struct!
# Actually, the quickest way to reduce arguments is to group `state, cmd, vram, fb, clipping` into `ctx: &mut Vdp1Context`
text = text.replace("fn draw_line_impl(", "pub struct Vdp1Context<'a> { pub state: &'a Vdp1State, pub cmd: &'a CmdTable, pub vram: &'a [u8], pub fb: &'a mut [u8], pub clipping: &'a ClippingState }\n\nfn draw_line_impl(")

text = re.sub(r'fn draw_line_impl\([\s\S]*?_is_poly_edge: bool,\n\)', r'fn draw_line_impl(ctx: &mut Vdp1Context, x1: i32, y1: i32, x2: i32, y2: i32, c_g1: u16, c_g2: u16, _is_poly_edge: bool)', text)
text = re.sub(r'fn draw_quad\([\s\S]*?br: Point,\n\)', r'fn draw_quad(ctx: &mut Vdp1Context, tl: Point, bl: Point, tr: Point, br: Point)', text)

# Inside the functions, unpack the ctx
text = text.replace("fn draw_line_impl(ctx: &mut Vdp1Context, x1: i32, y1: i32, x2: i32, y2: i32, c_g1: u16, c_g2: u16, _is_poly_edge: bool) {", "fn draw_line_impl(ctx: &mut Vdp1Context, x1: i32, y1: i32, x2: i32, y2: i32, c_g1: u16, c_g2: u16, _is_poly_edge: bool) {\n    let state = ctx.state;\n    let cmd = ctx.cmd;\n    let _vram = ctx.vram;\n    let fb = &mut *ctx.fb;\n    let clipping = ctx.clipping;")
text = text.replace("fn draw_quad(ctx: &mut Vdp1Context, tl: Point, bl: Point, tr: Point, br: Point) {", "fn draw_quad(ctx: &mut Vdp1Context, tl: Point, bl: Point, tr: Point, br: Point) {\n    let state = ctx.state;\n    let cmd = ctx.cmd;\n    let vram = ctx.vram;\n    let fb = &mut *ctx.fb;\n    let clipping = ctx.clipping;")

# Fix the calls
# draw_line_impl(state, cmd, vram, fb, clipping, ...
text = re.sub(r'draw_line_impl\(\n\s*state,\n\s*&cmd,\n\s*&vram\[\.\.\],\n\s*&mut fb\[\.\.\],\n\s*&clipping,', r'draw_line_impl(&mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..], clipping: &clipping },', text)

text = re.sub(r'draw_line_impl\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*clipping,', r'draw_line_impl(\&mut Vdp1Context { state, cmd, vram, fb, clipping },', text)

text = re.sub(r'draw_quad\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*clipping,', r'draw_quad(\&mut Vdp1Context { state, cmd, vram, fb, clipping },', text)

text = re.sub(r'draw_quad\(\n\s*state,\n\s*&cmd,\n\s*&vram\[\.\.\],\n\s*&mut fb\[\.\.\],\n\s*&clipping,', r'draw_quad(&mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..], clipping: &clipping },', text)

text = text.replace("draw_line_impl(state, cmd, vram, fb, clipping, x0, y0, x1, y1, 0, 0, true);", "draw_line_impl(&mut Vdp1Context { state, cmd, vram, fb, clipping }, x0, y0, x1, y1, 0, 0, true);")

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

