with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace(
    "draw_quad(state, &cmd, &vram[..], &mut fb[..], tl, bl, tr, br);",
    "draw_quad(&mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] }, tl, bl, tr, br);"
)

text = text.replace(
    """draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xa,
                        ya,
                        xb,
                        yb,
                        grd[0],
                        grd[1],
                        true,
                    );""",
    """draw_line_impl(
                        &mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] },
                        Point { x: xa, y: ya },
                        Point { x: xb, y: yb },
                        grd[0],
                        grd[1],
                        true,
                    );"""
)

text = text.replace(
    """draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xb,
                        yb,
                        xc,
                        yc,
                        grd[1],
                        grd[2],
                        true,
                    );""",
    """draw_line_impl(
                        &mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] },
                        Point { x: xb, y: yb },
                        Point { x: xc, y: yc },
                        grd[1],
                        grd[2],
                        true,
                    );"""
)

text = text.replace(
    """draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xd,
                        yd,
                        xc,
                        yc,
                        grd[3],
                        grd[2],
                        true,
                    );""",
    """draw_line_impl(
                        &mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] },
                        Point { x: xd, y: yd },
                        Point { x: xc, y: yc },
                        grd[3],
                        grd[2],
                        true,
                    );"""
)

text = text.replace(
    """draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xa,
                        ya,
                        xd,
                        yd,
                        grd[0],
                        grd[3],
                        true,
                    );""",
    """draw_line_impl(
                        &mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] },
                        Point { x: xa, y: ya },
                        Point { x: xd, y: yd },
                        grd[0],
                        grd[3],
                        true,
                    );"""
)

text = text.replace(
    """draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xa,
                        ya,
                        xb,
                        yb,
                        grd[0],
                        grd[1],
                        false,
                    );""",
    """draw_line_impl(
                        &mut Vdp1Context { state, cmd: &cmd, vram: &vram[..], fb: &mut fb[..] },
                        Point { x: xa, y: ya },
                        Point { x: xb, y: yb },
                        grd[0],
                        grd[1],
                        false,
                    );"""
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

