# Current review — `HEAD~2..HEAD` (VDP2 Phase 2 + Phase 3)

Snapshot of the **latest** review only (per `CLAUDE.md`). Overwrite, don't append.

Range: `604632f..194572f`
- `604632f` VDP2 Phase 2: One NBG layer, the simplest format, pixel-exact
- `194572f` VDP2 Phase 3: Remaining NBG layers and every character/bitmap format

Also present in the working tree at review time (separate, in-flight work, not part of the
review target): `cargo fmt` of the above files plus the `thread::yield_now()` removal in
`Sh2::run_loop` / Cores 4/5/6. Those were read but are not covered by the findings below.

Cross-checked against `../yabause/src/` (`vidsoft.c`, `vidshared.h`, `vdp2.h`, `titan/titan.c`)
and `docs/implementation-plans/vdp2.md`.

`cargo test --workspace`: green (395 tests). `cargo fmt --check`: clean.

---

## Correctness

### 1. `scyn0()` / `scyn1()` read the wrong register — NBG0/NBG1 vertical scroll is wrong
`saturn-core/src/vdp2_regs.rs:137,143`

`scyn0()` returns `regs[0x072 / 2]` and `scyn1()` returns `regs[0x082 / 2]`. `vdp2.h:140-142`
and `:179-181` give `SCXIN0 = 0x070`, **`SCXDN0 = 0x072`**, `SCYIN0 = 0x074`; `SCXIN1 = 0x080`,
**`SCXDN1 = 0x082`**, `SCYIN1 = 0x084`. So both read the *fractional part of the X* scroll, not
Y. `vidsoft.c:1604` / `:1721` use `regs->SCYIN0 & 0x7FF` / `regs->SCYIN1 & 0x7FF`.

NBG2/NBG3 (`0x090/0x092`, `0x094/0x096`) are correct — which is why the Phase-2 NBG3 tests pass
and hid this. `docs/implementation-plans/vdp2.md` §3.1 even spells out `0x070`/`0x074` and
`0x080`/`0x084`, and the item is checked `[x]`.

Fix: `0x074 / 2` and `0x084 / 2`.

### 2. `supplementdata` is truncated to 5 bits — supplementary palette number is always lost
`saturn-core/src/vdp.rs:774` (and the NBG1/NBG2/NBG3 arms), `saturn-core/src/vdp2_regs.rs:346`

`render_nbg_layer` passes `regs.pncnX_supplementary_char()` (`PNCNx & 0x001F`) as
`pattern_addr`'s `supplementdata`. `vidshared.h:535` sets `info->supplementdata = pnc & 0x3FF`,
and `vidsoft.c:254` reads bits 5-7 out of it:
`paladdr = ((tmp & 0xF000) >> 8) | ((supplementdata & 0xE0) << 3)`.

With `& 0x1F` the `& 0xE0` term is unconditionally 0, so every 4bpp one-word pattern name loses
its supplementary palette-bank bits. The in-diff comment `// was & 0x3FF` shows this was a
deliberate narrowing. `vdp2.md` §2.2 `read_pattern_data` is checked `[x]` and states
`supplementdata = pnc & 0x3FF`.

Fix: pass `PNCNx & 0x3FF` (add a `pncnX_supplementdata()` accessor); keep the `& 0x1F`/`& 0x1C`
masking where `pattern_addr` already does it.

### 3. Bitmap mode applies cell masking and flip — NBG0/NBG1 bitmaps render one 8x8 block, tiled
`saturn-core/src/vdp2.rs:314` (`fetch_pixel`), `saturn-core/src/vdp.rs:957-975`

In Yabause the cell-relative `x &= 7 / y &= 7` (and the 16x16 sub-cell flip chain) lives inside
`Vdp2MapCalcXY` (`vidsoft.c:620-680`), which the draw loop calls **only** under
`if (!info->isbitmap)` (`vidsoft.c:1029-1034`). Mimas moved that block into `fetch_pixel`, which
is on both paths. So for a bitmap layer the screen-space `actual_x`/`actual_y` get masked to
0..7 before the `charaddr + (y * cellw + x)` address maths — the 512x256/1024x512 bitmap is
sampled only at its top-left 8x8 corner and tiled across the display. `vdp2.md` §3.3's
"`map_calc_xy` is skipped entirely" item is checked `[x]`.

Fix: add an `is_bitmap` parameter to `fetch_pixel` (or hoist the masking back into a
`!is_bitmap`-guarded step in `render_nbg_layer`) and skip masking/flip for bitmaps.

### 4. Layer paint order is inverted and priority 0 is ignored
`saturn-core/src/vdp.rs:1082-1126`

Layers are painted NBG0 → NBG1 → NBG2 → NBG3, so **NBG3 ends up on top**. `titan.c:129-137`
sorts front-to-back as `RBG0, NBG0, NBG1, NBG2, NBG3` (`TITAN_NBG3 = 0 … TITAN_NBG0 = 3`,
`TITAN_RBG0 = 4`, iterated descending within each priority level), i.e. NBG0 is in front of
NBG3 at equal priority.

Separately, `titan.c:501` / `:515` (`if (priority == 0) return;`) — a layer whose PRINA/PRINB
priority is 0 is not displayed at all. Mimas ignores PRINA/PRINB entirely (the accessors were
added this commit but are unused), so on a fresh boot, where PRINA/PRINB are still 0, every
BGON-enabled layer paints over the back screen.

Minimal honest fix until Phase 4: paint 3 → 2 → 1 → 0 and skip layers whose priority is 0.

### 5. `craofb()` and `spctl()` read unrelated registers (pre-existing, live in the touched function)
`saturn-core/src/vdp2_regs.rs:74,80`

`craofb()` returns `regs[0x0CA / 2]` — `vdp2.h:320` says `0x0CA = WPSY1` (window 1 Y start).
CRAOFB is `0x0E6` (`vdp2.h:365`). `spctl()` returns `regs[0x0F0 / 2]` — `vdp2.h:370` says
`0x0F0 = PRISA`. SPCTL is `0x0E0` (`vdp2.h:362`).

Both are consumed by the VDP1 sprite overlay at the end of `render_back_screen`
(`vdp.rs:1134-1136`): `is_rgb_mode` and `color_bank_offset` are derived from a window register
and a priority register. `craofa()` (`0x0E4`), `prina()` (`0x0F8`) and `prinb()` (`0x0FA`) added
in this diff *are* correct, which makes the two stale ones look correct by association.

### 6. `colornumber_2_ignores_paladdr` asserts nothing
`saturn-core/src/vdp2.rs:578-584`

`assert!(pixel.is_some() || pixel.is_none());` is a tautology — it passes for every possible
return value. `vdp2.md` §3.4 describes this test as "set a nonzero `paladdr` and assert the
colour is unchanged" and has it checked `[x]`. It is the one rule the plan flags as "most likely
to be 'fixed' into a bug later" and it currently has zero coverage. `CLAUDE.md`: "never assert a
value you haven't independently derived. A self-consistent-but-wrong test is worse than no
test."

### 7. `back_screen_addr()` is dead *and* disagrees with the live back-screen path
`saturn-core/src/vdp2_regs.rs:381,391`

`back_screen_addr()` computes `((bktau << 16) | bktal)` masked by `0x7FFFF`/`0x3FFF`. The live
code in `render_back_screen` (`vdp.rs:1025-1035`) — matching `vidsoft.c`'s `Vdp2DrawBackScreen`
— uses `(((bktau & 0x7) << 16) | bktal) * 2` (or `& 0x3` for 4 Mbit). Neither
`back_screen_addr()` nor `back_screen_enabled()` has a caller, so the first one to use them gets
a wrong address. Either delete them or make `render_back_screen` call them.

## Concurrency / conventions

### 8. VRAM+CRAM read locks are held across the whole frame render, and in reverse field order
`saturn-core/src/vdp.rs:1057-1058, 1129, 1140`

`vdp2_vram` and `vdp2_cram` are now acquired *before* the four `render_nbg_layer` calls and held
until the end of the function. That is a full 320x224x4-layer software render (plus the VDP1
overlay loop) with both locks held — every `Sh2::write_*` into VDP2 VRAM/CRAM
(`sh2.rs:1074,1079,1694,1700,1843,1851` all take `.write()`) blocks for that entire window.

It also breaks `CLAUDE.md`'s rule: "if a future one does [need more than one lock], acquire them
in field-declaration order to avoid lock-ordering deadlocks." `WorkRam` declares
`vdp1_framebuffers` (l.36) and `vdp1_regs` (l.38) *before* `vdp2_vram` (l.40) / `vdp2_cram`
(l.42), and this function takes `vdp2_vram` → `vdp2_cram` → `vdp1_regs` →
`vdp1_framebuffers.banks[..]`.

Cheapest fix that keeps both properties: copy the two regions (or render into a local layer
buffer) and drop the locks before the VDP1 overlay, or scope `vdp2_vram` to the NBG loop only.

### 9. `docs/implementation-plans/vdp2.md` Phase 3 test checklist is checked off for tests that don't exist
`docs/implementation-plans/vdp2.md` §3.4

Marked `[x]` but absent from the tree (`grep` over `saturn-core/` and `e2e-tests/` finds none):
- five `<format>_renders_a_hand_derived_cell` tests (one per `colornumber`)
- `sixteen_by_sixteen_flip_selects_the_right_subcell`
- `bitmap_mode_addressing_uses_cellw_as_stride`
- `bandwidth_exclusion_suppresses_nbg2_when_nbg0_is_high_colour` (and the other two rules)
- `each_nbg_reads_its_own_registers`

`vdp2.rs`'s test module has exactly 7 tests, all Phase-2-shaped. §2.1/§3.1 items are likewise
`[x]` while findings 1 and 2 above show the underlying decode is wrong. `CLAUDE.md`: "not
checked off for anything not fully true. A future session (or agent) trusts these checklists at
face value."

### 10. No `history.md` chapter for the Phase 3 commit
`history.md`

Chapter 38 covers Phase 2 only; `194572f` (Phase 3 — all four NBG layers, five colour formats,
16x16 cells, bitmap mode, bandwidth exclusion) added no chapter. The deliberate simplifications
it *does* make (no priority resolution, the `BMPNA << 8` vs `<< 4` ambiguity, bandwidth
exclusion approximated) are exactly the "why" a later session will need.
`.development/current_bugs.md` was also not updated with the known-uncertain bitmap palette
shift.

## Cleanup / altitude

### 11. `ScreenVars` re-derives shift widths from magic 512 comparisons, and underflows for bitmaps
`saturn-core/src/vdp2.rs:113-131, 158-161`

`map_calc_xy` recovers `planepixelwidth_bits` with `if vars.planepixelwidth == 512 { 9 } else
{ 10 }`, hardcodes `pagepixelwh_bits = 9` / `pagepixelwh_mask = 511` while `ScreenVars` carries
an unused `pagepixelwh` field, and recomputes `planew_bits` the same way. Yabause's
`screeninfo_struct` stores `*_bits` / `*_mask` alongside each value (`vidsoft.c:688-702`); doing
the same removes three magic numbers and the possibility of the two drifting.

Latent bug in the same place: `render_nbg_layer` builds `ScreenVars { planepixelwidth: 0,
planepixelheight: 0, .. }` for bitmaps, so `let planepixelwidth_mask = vars.planepixelwidth - 1`
would panic on u32 underflow in a debug build if `map_calc_xy` were ever reached on that path.
Only fix 3's `is_bitmap` guard keeps it unreachable today.

### 12. Dead state carried through `Vdp2State` / `Vdp2CellInfo`
`saturn-core/src/vdp2.rs:4-20, 157`

`state.pipe[1] = state.pipe[0]` is written every cell change and never read — Yabause only reads
`pipe[0]` under `bad_cycle` (`vidsoft.c:1036-1045`), which Mimas has no equivalent of.
Likewise `Vdp2State::planenum` and `Vdp2CellInfo::{addr, specialfunction, specialcolorfunction}`
are stored and never consumed. Either wire `bad_cycle` or drop the pipeline and the fields, with
a comment pointing at §B.5.

### 13. `pattern_addr` hardcodes `specialfunction`/`specialcolorfunction` to 0 on the one-word path
`saturn-core/src/vdp2.rs:270-271`

`vidsoft.c:248-249`: `specialfunction = (supplementdata >> 9) & 1`,
`specialcolorfunction = (supplementdata >> 8) & 1`. Harmless today only because nothing reads
those fields (finding 12) *and* because finding 2 masks the bits away anyway — but Phase 4
(special priority / special colour calculation) will consume them and get silent zeros.

### 14. Per-pixel recomputation of loop-invariant bitmap constants
`saturn-core/src/vdp.rs:955-970`

`regs.mpofn()` and the `base` / `pal` expressions are evaluated inside the `for y { for x { … } }`
body for every pixel of a bitmap layer, though nothing in them depends on `x` or `y`. Hoist them
next to `bmp_width` / `bmp_height` (`bmpna` already is).

### 15. The 17-element per-layer tuple is the structure that invited findings 1 and 2
`saturn-core/src/vdp.rs:726-871`

Four near-identical `match layer` arms each destructure into the same 17-tuple. The only
differences that matter are which accessor each field uses, and two of those accessors are wrong
(`scyn0`/`scyn1`) while a third is narrowed (`supplementdata`) — with no compiler or reader
signal, because every element is a bare `u16`. A `struct NbgLayerParams { … }` built by a small
`fn params_for(regs, layer)` (or a table of accessor fn pointers) makes each field named at the
construction site and makes the NBG2/NBG3 `false, 0` bitmap placeholders explicit.
