import re

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

# Define Vdp1Context
context_def = """struct Vdp1Context<'a> {
    state: &'a Vdp1State,
    cmd: &'a CmdTable,
    vram: &'a [u8],
    fb: &'a mut [u8],
    clipping: &'a ClippingState,
}"""

if "struct Vdp1Context" not in text:
    text = text.replace("fn draw_line_impl(", context_def + "\n\nfn draw_line_impl(")

# draw_line_impl signature
text = re.sub(
    r'fn draw_line_impl\(\n    state: &Vdp1State,\n    cmd: &CmdTable,\n    _vram: &\[u8\],\n    fb: &mut \[u8\],\n    clipping: &ClippingState,\n    x0: i32,\n    y0: i32,\n    x1: i32,\n    y1: i32,\n    is_poly_edge: bool,\n\)',
    r'fn draw_line_impl(ctx: &mut Vdp1Context, x0: i32, y0: i32, x1: i32, y1: i32, is_poly_edge: bool)',
    text
)

# draw_quad signature
text = re.sub(
    r'fn draw_quad\(\n    state: &Vdp1State,\n    cmd: &CmdTable,\n    vram: &\[u8\],\n    fb: &mut \[u8\],\n    clipping: &ClippingState,\n    tl: Point,\n    tr: Point,\n    bl: Point,\n    br: Point,\n\)',
    r'fn draw_quad(ctx: &mut Vdp1Context, tl: Point, tr: Point, bl: Point, br: Point)',
    text
)

# Replace variables in draw_line_impl and draw_quad
# Actually, it's easier to just alias them at the start of the functions
text = re.sub(
    r'fn draw_line_impl\(ctx: &mut Vdp1Context, x0: i32, y0: i32, x1: i32, y1: i32, is_poly_edge: bool\) \{',
    r'fn draw_line_impl(ctx: &mut Vdp1Context, x0: i32, y0: i32, x1: i32, y1: i32, is_poly_edge: bool) {\n    let state = ctx.state;\n    let cmd = ctx.cmd;\n    let fb = &mut *ctx.fb;\n    let clipping = ctx.clipping;',
    text
)

text = re.sub(
    r'fn draw_quad\(ctx: &mut Vdp1Context, tl: Point, tr: Point, bl: Point, br: Point\) \{',
    r'fn draw_quad(ctx: &mut Vdp1Context, tl: Point, tr: Point, bl: Point, br: Point) {\n    let state = ctx.state;\n    let cmd = ctx.cmd;\n    let vram = ctx.vram;\n    let fb = &mut *ctx.fb;\n    let clipping = ctx.clipping;',
    text
)

# Replace calls to draw_line_impl
text = re.sub(
    r'draw_line_impl\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*clipping,\n\s*(.*?),\n\s*(.*?),\n\s*(.*?),\n\s*(.*?),\n\s*(.*?),?\n\s*\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd, vram, fb, clipping }, \1, \2, \3, \4, \5);',
    text
)
# Note: sometimes they are on one line, or different formats.
# Let's replace specifically in execute_vdp1
# Actually, inside draw_command, it calls draw_quad:
text = re.sub(
    r'draw_quad\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*clipping,\n\s*Point \{ x: tl_x, y: tl_y \},\n\s*Point \{ x: tr_x, y: tr_y \},\n\s*Point \{ x: bl_x, y: bl_y \},\n\s*Point \{ x: br_x, y: br_y \},\n\s*\);',
    r'draw_quad(&mut Vdp1Context { state, cmd, vram, fb, clipping }, Point { x: tl_x, y: tl_y }, Point { x: tr_x, y: tr_y }, Point { x: bl_x, y: bl_y }, Point { x: br_x, y: br_y });',
    text
)

# And inside draw_command it calls draw_line_impl:
text = re.sub(
    r'draw_line_impl\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*clipping,\n\s*xa,\n\s*ya,\n\s*xb,\n\s*yb,\n\s*false,\n\s*\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd, vram, fb, clipping }, xa, ya, xb, yb, false);',
    text
)

text = re.sub(
    r'draw_line_impl\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*clipping,\n\s*xb,\n\s*yb,\n\s*xc,\n\s*yc,\n\s*false,\n\s*\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd, vram, fb, clipping }, xb, yb, xc, yc, false);',
    text
)
text = re.sub(
    r'draw_line_impl\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*clipping,\n\s*xc,\n\s*yc,\n\s*xd,\n\s*yd,\n\s*false,\n\s*\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd, vram, fb, clipping }, xc, yc, xd, yd, false);',
    text
)
text = re.sub(
    r'draw_line_impl\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*clipping,\n\s*xd,\n\s*yd,\n\s*xa,\n\s*ya,\n\s*false,\n\s*\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd, vram, fb, clipping }, xd, yd, xa, ya, false);',
    text
)

text = re.sub(
    r'draw_line_impl\(state, cmd, vram, fb, clipping, x0, y0, x1, y1, true\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd, vram, fb, clipping }, x0, y0, x1, y1, true);',
    text
)


with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

