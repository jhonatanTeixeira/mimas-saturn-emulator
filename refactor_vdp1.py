import re

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

context_def = """pub struct Vdp1Context<'a> {
    pub state: &'a Vdp1State,
    pub cmd: &'a CmdTable,
    pub vram: &'a [u8],
    pub fb: &'a mut [u8],
}"""

if "pub struct Vdp1Context" not in text:
    text = text.replace("fn draw_line_impl(", context_def + "\n\nfn draw_line_impl(")

# Change signatures
text = re.sub(
    r'fn draw_line_impl\(\n    state: &Vdp1State,\n    cmd: &CmdTable,\n    _vram: &\[u8\],\n    fb: &mut \[u8\],\n    x1: i32,\n    y1: i32,\n    x2: i32,\n    y2: i32,\n    c_g1: u16,\n    c_g2: u16,\n    _is_poly_edge: bool,\n\)',
    r'fn draw_line_impl(ctx: &mut Vdp1Context, p1: Point, p2: Point, c_g1: u16, c_g2: u16, _is_poly_edge: bool)',
    text
)

text = re.sub(
    r'fn draw_quad\(\n    state: &Vdp1State,\n    cmd: &CmdTable,\n    vram: &\[u8\],\n    fb: &mut \[u8\],\n    tl: Point,\n    bl: Point,\n    tr: Point,\n    br: Point,\n\)',
    r'fn draw_quad(ctx: &mut Vdp1Context, tl: Point, bl: Point, tr: Point, br: Point)',
    text
)

# Insert variable unwraps inside the functions
text = re.sub(
    r'fn draw_line_impl\(ctx: &mut Vdp1Context, p1: Point, p2: Point, c_g1: u16, c_g2: u16, _is_poly_edge: bool\) \{',
    r'fn draw_line_impl(ctx: &mut Vdp1Context, p1: Point, p2: Point, c_g1: u16, c_g2: u16, _is_poly_edge: bool) {\n    let state = ctx.state;\n    let cmd = ctx.cmd;\n    let fb = &mut *ctx.fb;\n    let x1 = p1.x;\n    let y1 = p1.y;\n    let x2 = p2.x;\n    let y2 = p2.y;',
    text
)

text = re.sub(
    r'fn draw_quad\(ctx: &mut Vdp1Context, tl: Point, bl: Point, tr: Point, br: Point\) \{',
    r'fn draw_quad(ctx: &mut Vdp1Context, tl: Point, bl: Point, tr: Point, br: Point) {\n    let state = ctx.state;\n    let cmd = ctx.cmd;\n    let vram = ctx.vram;\n    let fb = &mut *ctx.fb;',
    text
)

# Update the calls to draw_line_impl and draw_quad
# In draw_quad, we need to pass ctx to draw_line_impl
text = re.sub(
    r'draw_line_impl\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*(.*?)_x, (.*?)_y,\n\s*(.*?)_x, (.*?)_y,\n\s*0,\n\s*0,\n\s*false,\n\s*\);',
    r'draw_line_impl(ctx, Point { x: \1_x, y: \1_y }, Point { x: \3_x, y: \3_y }, 0, 0, false);',
    text
)

# Update draw_command calls to draw_quad
text = re.sub(
    r'draw_quad\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*Point \{ x: tl_x, y: tl_y \},\n\s*Point \{ x: bl_x, y: bl_y \},\n\s*Point \{ x: tr_x, y: tr_y \},\n\s*Point \{ x: br_x, y: br_y \},\n\s*\);',
    r'draw_quad(&mut Vdp1Context { state, cmd, vram, fb }, Point { x: tl_x, y: tl_y }, Point { x: bl_x, y: bl_y }, Point { x: tr_x, y: tr_y }, Point { x: br_x, y: br_y });',
    text
)

text = re.sub(
    r'draw_quad\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*tl,\n\s*bl,\n\s*tr,\n\s*br,\n\s*\);',
    r'draw_quad(&mut Vdp1Context { state, cmd, vram, fb }, tl, bl, tr, br);',
    text
)


# In draw_command, calls to draw_line_impl
# e.g. draw_line_impl(state, cmd, vram, fb, x0, y0, x1, y1, 0, 0, true);
text = re.sub(
    r'draw_line_impl\(state, cmd, vram, fb, (.*?), (.*?), (.*?), (.*?), (.*?), (.*?), (.*?)\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd, vram, fb }, Point { x: \1, y: \2 }, Point { x: \3, y: \4 }, \5, \6, \7);',
    text
)

# And polyline calls:
# draw_line_impl( state, cmd, vram, fb, xa, ya, xb, yb, c_ga, c_gb, false, );
text = re.sub(
    r'draw_line_impl\(\n\s*state,\n\s*cmd,\n\s*vram,\n\s*fb,\n\s*([a-z]+),\n\s*([a-z]+),\n\s*([a-z]+),\n\s*([a-z]+),\n\s*(.*?),\n\s*(.*?),\n\s*false,\n\s*\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd, vram, fb }, Point { x: \1, y: \2 }, Point { x: \3, y: \4 }, \5, \6, false);',
    text
)



with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

