import re

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

ctx_def = """pub struct Vdp1Context<'a> {
    pub state: &'a Vdp1State,
    pub cmd: &'a CmdTable,
    pub vram: &'a [u8],
    pub fb: &'a mut [u8],
}
"""

if "pub struct Vdp1Context" not in text:
    text = text.replace("fn draw_line_impl(", ctx_def + "\nfn draw_line_impl(")

# draw_line_impl definition
text = re.sub(
    r'fn draw_line_impl\(\s*state: &Vdp1State,\s*cmd: &CmdTable,\s*_vram: &\[u8\],\s*fb: &mut \[u8\],\s*x1: i32,\s*y1: i32,\s*x2: i32,\s*y2: i32,\s*c_g1: u16,\s*c_g2: u16,\s*_is_poly_edge: bool,\s*\)',
    r'fn draw_line_impl(ctx: &mut Vdp1Context, p1: Point, p2: Point, c_g1: u16, c_g2: u16, _is_poly_edge: bool)',
    text
)
def repl_draw_line(m):
    body = m.group(1)
    body = "    let state = ctx.state;\n    let cmd = ctx.cmd;\n    let fb = &mut *ctx.fb;\n    let x1 = p1.x;\n    let y1 = p1.y;\n    let x2 = p2.x;\n    let y2 = p2.y;\n" + body
    return r'fn draw_line_impl(ctx: &mut Vdp1Context, p1: Point, p2: Point, c_g1: u16, c_g2: u16, _is_poly_edge: bool) {' + "\n" + body + "\n}"

text = re.sub(
    r'fn draw_line_impl\(ctx: &mut Vdp1Context, p1: Point, p2: Point, c_g1: u16, c_g2: u16, _is_poly_edge: bool\) \{([\s\S]*?)^\}',
    repl_draw_line,
    text,
    flags=re.MULTILINE
)

# draw_quad definition
text = re.sub(
    r'fn draw_quad\(\s*state: &Vdp1State,\s*cmd: &CmdTable,\s*vram: &\[u8\],\s*fb: &mut \[u8\],\s*tl: Point,\s*bl: Point,\s*tr: Point,\s*br: Point,\s*\)',
    r'fn draw_quad(ctx: &mut Vdp1Context, tl: Point, bl: Point, tr: Point, br: Point)',
    text
)
def repl_draw_quad(m):
    body = m.group(1)
    body = "    let state = ctx.state;\n    let cmd = ctx.cmd;\n    let vram = ctx.vram;\n    let fb = &mut *ctx.fb;\n" + body
    return r'fn draw_quad(ctx: &mut Vdp1Context, tl: Point, bl: Point, tr: Point, br: Point) {' + "\n" + body + "\n}"

text = re.sub(
    r'fn draw_quad\(ctx: &mut Vdp1Context, tl: Point, bl: Point, tr: Point, br: Point\) \{([\s\S]*?)^\}',
    repl_draw_quad,
    text,
    flags=re.MULTILINE
)

# Update the calls to draw_line_impl
text = re.sub(
    r'draw_line_impl\(\s*state,\s*&cmd,\s*&vram\[\.\.\],\s*&mut fb\[\.\.\],\s*([a-zA-Z0-9_]+) \+ width - 1,\s*([a-zA-Z0-9_]+) \+ height - 1,\s*([a-zA-Z0-9_]+) \+ width - 1,\s*([a-zA-Z0-9_]+),\s*0,\s*0,\s*false,\s*\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] }, Point { x: \1 + width - 1, y: \2 + height - 1 }, Point { x: \3 + width - 1, y: \4 }, 0, 0, false);',
    text
)
text = re.sub(
    r'draw_line_impl\(\s*state,\s*&cmd,\s*&vram\[\.\.\],\s*&mut fb\[\.\.\],\s*([a-zA-Z0-9_]+),\s*([a-zA-Z0-9_]+),\s*([a-zA-Z0-9_]+),\s*([a-zA-Z0-9_]+),\s*0,\s*0,\s*(true|false),\s*\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] }, Point { x: \1, y: \2 }, Point { x: \3, y: \4 }, 0, 0, \5);',
    text
)

text = re.sub(
    r'draw_line_impl\(\s*state,\s*&cmd,\s*&vram\[\.\.\],\s*&mut fb\[\.\.\],\s*([a-zA-Z0-9_]+),\s*([a-zA-Z0-9_]+),\s*([a-zA-Z0-9_]+),\s*([a-zA-Z0-9_]+),\s*grd\[(\d+)\],\s*grd\[(\d+)\],\s*(true|false),\s*\);',
    r'draw_line_impl(&mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] }, Point { x: \1, y: \2 }, Point { x: \3, y: \4 }, grd[\5], grd[\6], \7);',
    text
)

# Update the calls to draw_quad
text = re.sub(
    r'draw_quad\(\s*state,\s*&cmd,\s*&vram\[\.\.\],\s*&mut fb\[\.\.\],\s*tl,\s*bl,\s*tr,\s*br\s*\);',
    r'draw_quad(&mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] }, tl, bl, tr, br);',
    text
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)
