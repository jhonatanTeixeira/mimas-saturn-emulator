# VDP1 — Implementation Plan

**Scope.** This document diffs Mimas's current VDP1 implementation (`saturn-core/src/vdp.rs`'s
`execute_vdp1`, plus the storage in `saturn-core/src/shared_buffers.rs` and the call site in
`saturn-core/src/lib.rs`) against `docs/hardware-reference/vdp1.md`, and lays out an ordered set of
phases to close the gap.

**Ground truth.** Every "hardware says" claim below points at a section of
`docs/hardware-reference/vdp1.md` (written from a full read of `yabause/src/vdp1.cpp`,
`vdp1.h`, `vidsoft.c`'s VDP1 half, and the erase/swap/interrupt consumers in `vdp2.cpp`/`scu.c`).
Where that reference marks something **[Ambiguous]** — an emulator guard, a game-specific hack, or
a place where Yabause contradicts itself — this plan says explicitly whether Mimas ports it,
skips it, or defers it. Do not "fix" one of those by inventing behaviour; §10 lists them all.

**Line numbers** are as of the commit this plan was written against (`vdp.rs` 253 lines,
`lib.rs` 390 lines). They will drift; the function/field names will not.

**Sibling plans this one depends on.** `docs/implementation-plans/vdp2.md` (framebuffer readout,
sprite-type decode, CRAM resolution) and `docs/implementation-plans/scu.md` (the interrupt
controller that Draw End is one source of, and DMA start factor 6). Phase 3 and Phase 7 below name
the exact contract each side owns.

---

## 0. Current-state assessment

### 0.1 What exists today

| Thing | Where | State |
|---|---|---|
| Command-list walker | `vdp.rs:87-124` | Linear scan from VRAM offset 0, `+= 32` per command, stops on CMDCTRL bit 15 |
| Polygon (COMM 4) | `vdp.rs:102-118` | Axis-aligned bounding box of 3 vertices, flat fill with a raw 16-bit word |
| Plot gate | `vdp.rs:76-81` | Reads `vdp1_regs[0..2]`, returns unless bit 0 set |
| Storage | `shared_buffers.rs:34-39` | `vdp1_vram` 512 KiB, `vdp1_framebuffer` **one flat 512 KiB region**, `vdp1_regs` 4 KiB plain byte array |
| CPU port | `sh2.rs:362-367`, `:429-440`, `:527-541` | VRAM/framebuffer/registers are plain read-write byte arrays, no side effects, no live reads |
| Invocation | `lib.rs:216` | Called from **Core 3** (`vdp2-composite`), inside the `now >= next_frame_due` wall-clock gate at `lib.rs:215`, immediately before `render_backdrop` |
| Core 2 (`vdp1-draw`) | `lib.rs:179-196` | Idle loop: `sync_core(2, cycles); thread::yield_now();`. Executes no VDP1 logic at all |
| Readout | `vdp.rs:141-156` | `render_backdrop` overlays the VDP1 framebuffer treating any non-zero word as opaque, with a **hardcoded 320-pixel stride** (`vdp.rs:145`) |
| Dead code | `vdp.rs:21-41` (`Vdp` + `front_buffer`), `shared_buffers.rs:190-218` (`DoubleBufferedFramebuffer`) | Constructed only by `e2e-tests/src/lib.rs:240-241`; superseded by `SaturnSystem::vdp2_frame` (`lib.rs:49`) |
| Tests | `vdp.rs:206-252` (`test_vdp1_polygon_drawing`) | One end-to-end polygon test — see §0.2, it encodes the bugs |

### 0.2 Defects in what exists (fix before adding anything)

These are not "missing features", they are wrong against the reference. All four are in the ~50
lines of `execute_vdp1`.

**D1 — the command table is read at the wrong offsets.** Reference §3 fixes the layout as
CMDCTRL `0x00`, CMDLINK `0x02`, CMDPMOD `0x04`, CMDCOLR `0x06`, CMDSRCA `0x08`, CMDSIZE `0x0A`,
CMDXA `0x0C`, CMDYA `0x0E`, CMDXB `0x10`, CMDYB `0x12`, CMDXC `0x14`, CMDYC `0x16`, CMDXD `0x18`,
CMDYD `0x1A`, CMDGRDA `0x1C`. The current code reads:

| `vdp.rs` line | Reads offset | Calls it | Actually is |
|---|---|---|---|
| `vdp.rs:92` | `+0x04` | `cmdcolr` | **CMDPMOD** |
| `vdp.rs:94` | `+0x08` | `xa` | **CMDSRCA** |
| `vdp.rs:95` | `+0x0A` | `ya` | **CMDSIZE** |
| `vdp.rs:96` | `+0x0C` | `xb` | **CMDXA** |
| `vdp.rs:97` | `+0x0E` | `yb` | **CMDYA** |
| `vdp.rs:98` | `+0x10` | `xc` | **CMDXB** |
| `vdp.rs:99` | `+0x12` | `yc` | **CMDYB** |

So the colour is off by one word and the vertices are off by two. CMDXD/CMDYD (`+0x18`/`+0x1A`)
and CMDGRDA (`+0x1C`) are never read at all. Against a real BIOS command list this draws a
rectangle at coordinates taken from the *previous two fields* filled with a colour taken from the
draw-mode word — i.e. geometrically and chromatically meaningless.

**D2 — the plot trigger reads the wrong register.** `vdp.rs:77` reads offset `0x00` and calls it
PTMR. Offset `0x00` is **TVMR** (reference §2.1); PTMR is at **`0x04`**. Testing bit 0 of TVMR
tests **TVM0** (the 8-bit-framebuffer select), not the plot trigger. And even at the right offset
the semantics are wrong: reference §2.2 says PTMR is a *strobe* — `1` = draw the list once
immediately (and shift EDSR first), `2` = draw at the next frame change, `0` = idle. The current
code re-walks the whole list on every frame tick for as long as the bit stays set, and clears
nothing.

**D3 — Draw End (CMDCTRL bit 15) is honoured after drawing, not before.** Reference §4.1/§4.2:
the walker reads the command word, and `if (command & 0x8000)` it sets status IDLE and **returns
without drawing**. Both at entry (`vdp1.cpp:561-565`) and at the top of each iteration
(`vdp1.cpp:570`, `681-684`). `vdp.rs:120-122` draws first and breaks after. A command word of
`0x8004` must therefore render **nothing**; today it renders a polygon.

**D4 — the existing test is self-consistent with D1/D2/D3.** `test_vdp1_polygon_drawing`
(`vdp.rs:206-252`) writes `0x0001` to `vdp1_regs[0..2]` (which is TVMR, not PTMR — D2), writes the
colour at `vram[4..6]` and the vertices at `vram[8..20]` (D1), and uses a single `0x8004` command
(D3). It passes because it feeds the buggy reader exactly the layout the buggy reader expects.
This is precisely the failure mode `CLAUDE.md` calls out ("a self-consistent-but-wrong test is
worse than no test"). It must be rewritten in Phase 0, not extended.

**D5 (secondary) — geometry, locks, and stride.**
- The fill clamps to `0..=319` / `0..=223` (`vdp.rs:104-106`) and strides by 320 (`vdp.rs:110`).
  VDP1's framebuffer is **512×256 / 1024×256 / 512×512** depending on TVMR bits 1-0 (reference
  §2.2 table) and has nothing to do with the VDP2 display resolution.
- `render_backdrop` reads the VDP1 framebuffer with the same hardcoded 320 stride (`vdp.rs:145`),
  so the two agree only by accident and both are wrong.
- `vdp.rs:83` takes a **write** lock on `vdp1_vram` although it only reads it. Harmless while
  everything is serial on Core 3; a hard blocker once VDP1 moves to its own thread (the SH-2
  cannot fill the next command list while VDP1 draws).
- `sh2.rs:429-440`/`:527-541` mask register offsets with `ram.len()-1` = `0xFFF`. Reference §1
  says VDP1 registers mirror every **256 bytes** (`& 0xFF`) and the framebuffer every **256 KiB**
  (`& 0x3FFFF`, not the current `& 0x7FFFF`).

### 0.3 Feature inventory diff

#### Draw commands (reference §4.3, §5)

| COMM | Name | Ref § | Status |
|---|---|---|---|
| 0 | Normal Sprite | §5.1 | **Missing** |
| 1 | Scaled Sprite | §5.2 | **Missing** (all 10 ZP anchor cases) |
| 2 | Distorted Sprite | §5.3 | **Missing** |
| 3 | Distorted Sprite (undocumented mirror of 2) | §4.3 | **Missing** |
| 4 | Polygon | §5.4 | Present but wrong (D1, D5): AABB not a quad, 3 of 4 vertices, no colour decode |
| 5 | Polyline | §5.5 | **Missing** |
| 6 | Line | §5.6 | **Missing** |
| 7 | Polyline (undocumented mirror of 5) | §4.3 | **Missing** |
| 8 | User Clipping Coordinates | §5.7 | **Missing** |
| 9 | System Clipping Coordinates | §5.8 | **Missing** |
| 10 | Local Coordinates | §5.9 | **Missing** |
| 11 | User Clipping (undocumented mirror of 8) | §4.3 | **Missing** |
| 12-15 | Bad command → `EDSR \|= 2`, LOPR/COPR ← `addr>>3`, abort | §4.3 | **Missing** (silently ignored) |

#### CMDCTRL fields (reference §3.1)

| Bits | Field | Status |
|---|---|---|
| 15 | END | Present, but applied *after* the draw (D3) |
| 14 | JP skip | **Missing** |
| 13-12 | JP (0 NEXT / 1 ASSIGN / 2 CALL / 3 RETURN) | **Missing** — always NEXT (`vdp.rs:123`) |
| 11-8 | ZP (scaled-sprite anchor) | **Missing** |
| 5-4 | Dir (character read direction / flip) | **Missing** |
| 3-0 | COMM | Present, only value 4 dispatched |

#### CMDPMOD fields (reference §3.3)

| Bits | Field | Status |
|---|---|---|
| 15 | MON (force framebuffer MSB) | **Missing** |
| 12 | HSS (high-speed shrink) | **Missing** — reference §3.3 says the software renderer ignores it too; deliberately not ported (§10) |
| 11 | PCLP (pre-clip disable) | **Missing** — never tested in the reference either; deliberately not ported (§10) |
| 10 | CLIP (user clipping enable) | **Missing** |
| 9 | CMOD (inside/outside user clip) | **Missing** |
| 8 | MESH | **Missing** |
| 7 | ECD (end-code disable) | **Missing** |
| 6 | SPD (transparent-pixel disable) | **Missing** |
| 5-3 | Colour mode 0-5 | **Missing** — all six |
| 2-0 | Colour calculation 0-7 | **Missing** — all eight; today's write is an unconditional raw store |

#### Colour modes (reference §6)

All six missing. None of `Vdp1ReadPattern16/64/128/256/64k` exists, so CMDSRCA/CMDSIZE are never
consulted and no texture byte is ever read from VRAM.

| Mode | Name | Value written to FB |
|---|---|---|
| 0 | 4 BPP, 16-colour bank | `(CMDCOLR & 0xFFF0) \| index` |
| 1 | 4 BPP, 16-colour LUT | `word[(index*2 + (CMDCOLR<<3)) & 0x7FFFF]` |
| 2 | 8 BPP, 64-colour bank | `(CMDCOLR & 0xFFC0) \| (byte & 0x3F)` |
| 3 | 8 BPP, 128-colour bank | `(CMDCOLR & 0xFF80) \| (byte & 0x7F)` |
| 4 | 8 BPP, 256-colour bank | `(CMDCOLR & 0xFF00) \| byte` |
| 5 | 16 BPP RGB | the word verbatim; transparent when bit 15 clear and SPD=0 |
| 6, 7 | no case in the reference | see §10 |

#### Register map (reference §2)

| Off | Name | Access | Status |
|---|---|---|---|
| `0x00` | TVMR (VBE / TVM2 / TVM1 / TVM0) | W | Stored only; no geometry decode, no VBlank erase |
| `0x02` | FBCR (EOS / DIE / DIL / FCM / FCT) | W | Stored only; no `manualchange`/`manualerase` side effect |
| `0x04` | PTMR | W | **Not read at all** — D2 reads offset `0x00` instead |
| `0x06` | EWDR | W | Stored only; never used |
| `0x08` | EWLR | W | Stored only; never used |
| `0x0A` | EWRR | W | Stored only; never used |
| `0x0C` | ENDR | W (strobe) | Stored only; no forced termination |
| `0x10` | EDSR (BEF/CEF) | R | Always reads 0 → BIOS polling CEF never completes |
| `0x12` | LOPR | R | Always reads 0 |
| `0x14` | COPR | R | Always reads 0 |
| `0x16` | MODR | R (synthesised) | Always reads 0, not `0x1000 \| ...` |

Also missing: byte and long register accesses must be no-ops returning 0 (reference §2.1 — *all*
meaningful access is 16-bit); writes to `0x10`-`0x16` and reads of `0x00`-`0x0C` are discarded /
return 0; offset `0x0E` and `0x18`+ decode to nothing.

#### Engine state (reference §2.4)

`addr`, `localX`, `localY`, `systemclipX1/Y1/X2/Y2`, `userclipX1/Y1/X2/Y2`, `status`
(IDLE/RUNNING), `returnAddr`, `current_frame`, `manualerase`, `manualchange`, `vbalnk_erase`,
`swap_frame_buffer`, `frame_change_plot` — **none of it exists**. There is no struct to hold it;
`execute_vdp1` is a free function over `&WorkRam` with no persistent state between calls.

#### Framebuffer erase / swap / draw end (reference §8, §9, §10)

Every one of these is missing: two-bank storage, the swap gated on FCM/FCT/`manualchange`, the
EWDR/EWLR/EWRR erase rectangle, the VBlank erase from TVMR bit 3, EDSR's `>>= 1` shift on plot
trigger and frame change, the `EDSR |= 2` on completion, LOPR/COPR maintenance, the Draw End
interrupt (vector `0x4D`, level 2), and the ENDR suppression of it. Practical consequence today:
the framebuffer is never cleared, so a drawn polygon persists forever (`render_backdrop` treats
every non-zero word as opaque, `vdp.rs:148`).

---

## Architectural decisions

These are referenced by the phases; read them first.

### AD-1 — Core 2 vs Core 3: move VDP1 to its own thread, but not first, and not as-is

**Recommendation: yes, VDP1 execution should end up on Core 2 (Phase 9) — but the current
placement is not merely "an acceptable simplification", it has a real behavioural bug that must be
fixed in Phase 1 regardless, without moving threads.**

The three positions and why the middle one wins:

*Keep it inline on Core 3, unchanged.* Rejected. `execute_vdp1` is called inside Core 3's
`now >= next_frame_due` gate (`lib.rs:215-216`), i.e. **at most once per 16.6 ms**. Reference §2.2
says a PTMR=1 write draws the list *immediately and synchronously* at the moment of the register
write. Under the current placement a plot trigger is serviced up to a full frame late, and — worse
— two plot triggers inside one frame period are **coalesced into one draw**. Any BIOS or game that
plots twice per frame (a common pattern: draw, swap, draw again) silently loses a frame's worth of
commands. That is a correctness bug caused purely by *where* the call sits, and it will produce
"missing sprites" symptoms that look like rasteriser bugs.

*Move to Core 2 now.* Rejected as the immediate step, for four reasons.
1. **There is nothing to time against.** Reference §4.2: the clock-budget block in Yabause is
   `#if 0`'d out and the entire command list executes atomically in zero emulated time; the 50-line
   Draw End delay (§2.2) is explicitly hand-tuned, not a hardware figure, and §12 item 13 states
   flatly that no VDP1 timing model exists in the source. A dedicated thread whose "timing" is
   invented would be worse than no thread: it would make boot behaviour depend on a number nobody
   can verify.
2. **The lock story is wrong until Phase 2.** `vdp.rs:83-84` write-locks both `vdp1_vram` and the
   single flat `vdp1_framebuffer`. On a separate thread, Core 3's `render_backdrop` read
   (`vdp.rs:142`) would block for the duration of an entire command list, every frame. The fix is
   the two-bank split (AD-2), which is Phase 2 work.
3. **It destroys test determinism before the rasteriser is correct.** Every unit test in
   `vdp.rs`'s `mod tests` is `execute_vdp1(&ram); render_backdrop(&ram);` — it works because they
   are serial. Debugging a from-scratch quad rasteriser against a real BIOS is hard enough without
   the results being nondeterministic.
4. **It's orthogonal to the biggest win.** D1 (wrong table offsets) is what actually prevents any
   real command list from rendering. Threading changes nothing about it.

*The recommended middle step (Phase 1): keep execution on Core 3, but make it event-driven rather
than frame-gated.* The SH-2's word write to VDP1 register `0x04` sets a trigger; Core 3 checks it
**every loop iteration** (outside the `next_frame_due` gate), not once per frame. PTMR=2 continues
to be serviced on the frame-change path, which genuinely is a per-frame event (reference §9.1).
This removes the latency and coalescing bug with no new thread, no new lock, and no invented
timing.

*Then Phase 9 moves it to Core 2*, once (a) the two-bank framebuffer exists so the two threads
touch disjoint memory, (b) EDSR/COPR/status exist so "VDP1 is busy" is an observable the SH-2 can
poll, and (c) the Draw End interrupt exists so completion is a signal and not just a return. At
that point the thread is buying something real: VDP1 rasterisation stops eating VDP2's 16.6 ms
budget (which matters as soon as VDP2 grows actual plane compositing per its own plan), and the
`vdp1-draw` thread finally matches its name and `docs/mimas-architecture-spec.md` §3.1.

The Core 2 wake mechanism must be a `Condvar`, not an `AtomicBool` spin — `mimas-architecture-spec.md`
§1.2 forbids busy-polling atomics and §1.5 requires idle threads to park. Core 2 should use
`LockStepSync::park_while_inactive(2)` exactly the way Core 6 does for the SCU DSP
(`lib.rs:320-341`), with the SH-2's PTMR=1 write calling `set_thread_active(2, true)` the way
SSHON does for Core 1 (`sh2.rs:826-838`). That also fixes half of `CLAUDE.md`'s second piece of
architecture debt ("only Core 1 and Core 6 truly park") for free.

### AD-2 — Two framebuffer banks vs the `ArcSwap` frame-publish pattern

These are two different double-buffers and must not be conflated.

`SaturnSystem::vdp2_frame: Arc<ArcSwap<vdp::Framebuffer>>` (`lib.rs:49`, stored at `lib.rs:231`) is
a **presentation** handoff: Core 3 builds a brand-new immutable host-format (XRGB8888) frame each
tick and publishes it by pointer swap; readers (`main.rs:204`, `mimas_window.rs:70`) load it on
their own schedule and never block. That pattern is correct and stays exactly as it is. VDP1 does
not participate in it — VDP1 writes into VDP2's *input*, not into the published frame.

The VDP1 bank pair is a different animal: both banks are **CPU-addressable** (the back one through
`0x05C80000`, reference §1), **mutated in place** byte by byte, and swapped by a *register* write
(FBCR) rather than by publishing a new value. `ArcSwap<T>` is the wrong primitive — you cannot
hand the SH-2 write path a pointer into an `Arc` whose contents another thread is mutating.

**Do not reuse `shared_buffers::DoubleBufferedFramebuffer` (`shared_buffers.rs:190-218`).** Beyond
being the wrong shape, its `swap()` (`:212-217`) is **not atomic**: it performs two independent
`store` calls, so a concurrent reader can observe `front` and `back` pointing at the same buffer.
It is dead code used only by the `e2e-tests/src/lib.rs:240-241` smoke test.

Recommended representation, replacing `shared_buffers.rs:35-37`'s single flat region (note the
current allocation is already exactly `2 × 0x40000`, so this is a repartition, not new memory):

```rust
/// VDP1's two 256 KiB framebuffer banks (reference §9). VDP1 draws into the
/// *back* bank, VDP2 scans out the *front* one, and the CPU port at
/// 0x05C80000 sees the back one.
pub struct Vdp1Framebuffers {
    banks: [RwLock<Box<[u8; 0x40000]>>; 2],
    /// Index of the bank VDP1 is drawing into. Flipped by FBCR frame change.
    back: AtomicUsize,
}
```

Why this and not something clever:
- It keeps `shared_buffers.rs`'s existing invariant (one `RwLock` per independent region — now two,
  one per bank) rather than reintroducing a lock that two threads with disjoint work contend on.
- Core 2 write-locks the back bank for a whole command list while Core 3 read-locks the front bank
  for a whole scanout. **Genuinely disjoint** — no contention, which is the precondition AD-1
  Phase 9 needs.
- The only shared mutable datum is the `back` index. It flips exactly once per frame change; store
  it `Release` and load it `Acquire`, which publishes every drawing write made before the swap.
  This is the same discipline already used for `m68k_control` (`lib.rs:55-60`, `sh2.rs:844`).
- `shared_buffers.rs`'s field-declaration lock-order rule still holds: if a call site ever needs
  both banks at once (it should not), take index 0 then index 1.

### AD-3 — Draw End interrupt delivery, and the SCU boundary

Reference §10.2 (`scu.c:3382-3385`) and `docs/hardware-reference/scu.md` §4.1 give the exact
numbers: **SH-2 exception vector `0x4D`, interrupt level `2`, SCU `IMS` mask bit `0x2000` (bit 13),
SCU `IST` status bit `0x00002000`, SCU DMA start factor `6`.**

`sh2.rs` already has both interrupt shapes:
- **CPU-local** flags set by the SH-2 itself (`vblank_pending` `sh2.rs:117`, `smpc_irq_pending`
  `sh2.rs:138`).
- **Cross-thread** flags: `sound_req_irq: Option<Arc<AtomicBool>>` (`sh2.rs:148`), owned by
  `SaturnSystem` (`lib.rs:60`), set by Core 3's `M68k`, observed by Core 0.

Draw End is cross-thread (raised by whichever core runs VDP1, consumed by Core 0), so it takes the
`sound_req_irq` shape exactly:

- [ ] `SaturnSystem.draw_end_irq: Arc<AtomicBool>` alongside `sound_req_irq` (`lib.rs:60`).
- [ ] `Sh2.draw_end_irq: Option<Arc<AtomicBool>>` — set via a field, **not** by changing
      `Sh2::new()`'s 3-argument signature (`CLAUDE.md`, "Stability constraints").
- [ ] `const DRAW_END_LEVEL: u32 = 2; const DRAW_END_VECTOR: u32 = 0x4D;` next to the existing
      constants at `sh2.rs:193-222`, with the same "confirmed against `ScuSendDrawEnd`" comment
      style.
- [ ] A branch in `service_pending_interrupt` (`sh2.rs:908-952`), **last** in the priority chain:
      VBLANK-IN (15) > VBLANK-OUT (14) > Sound Request (9) > SMPC (8) > **Draw End (2)**.

Note that level 2 is masked by any SR I-mask ≥ 2, which is the normal state through most of BIOS
init. The existing code already handles this correctly — `sh2.rs:926-928` returns without clearing
the pending flag, so it stays latched until SR's mask drops. No change needed there; just don't
"optimise" it away.

**What belongs to the SCU plan, not this one.** Reference `scu.md` §4.1/§4.2 shows the real path is
`SendInterrupt(vector, level, mask, statusbit)`: if `IMS & mask` is clear the interrupt is
delivered to the master SH-2 now; if it is set the interrupt is *not* delivered and `IST |=
statusbit` latches it instead. Today `work_ram.scu_regs` is a plain byte array with no IMS/IST
logic at all. So:

- VDP1 exposes exactly **one** entry point, `raise_draw_end()`, and knows nothing about IMS/IST.
- Until the SCU controller exists, `raise_draw_end()` sets `draw_end_irq` directly (an explicit,
  documented simplification: it behaves as if IMS bit 13 were always clear).
- When the SCU plan lands, `raise_draw_end()` routes through the SCU's `send_interrupt(0x4D, 2,
  0x2000, 0x0000_2000)` and the SCU decides deliver-vs-latch and additionally kicks DMA start
  factor 6. **Coordinate the signature with `implementation-plans/scu.md` before writing it** so
  there is one `send_interrupt` and not two.

Two ordering details from the reference that must survive the refactor: CEF is set **after**
`ScuSendDrawEnd()` (§10.3), so a handler reading EDSR immediately sees CEF still clear; and an
ENDR write **suppresses** a pending Draw End (§2.2, `wait_line_count = -1`).

### AD-4 — Rasteriser model: port the edge-walk, not a scanline filler

Reference §5.0 quotes `vidsoft.c:3066-3069`: *"a real vdp1 draws with arbitrary lines / this is why
endcodes are possible / this is also the reason why half-transparent shading causes moire patterns
/ and the reason why gouraud shading can be applied to a single line draw command"*. That is a
statement about the **hardware**, not about Yabause's convenience, so per `CLAUDE.md` ("port what
the hardware does") Mimas ports the model:

walk the left edge (tl→bl) and right edge (tr→br) with Bresenham, normalise the shorter edge's step
count against the longer, then draw one span per step index connecting the two — with texture V a
function of the **span index**, not of screen Y, and texture U a function of position along the
span. Affine, no perspective correction, no filtering.

What *not* to port (these are Yabause's guards, reference §12 item 12): the fixed 1000-entry
`xleft/yleft/xright/yright` arrays and the resulting `abs(dx) > 999 || abs(dy) > 999 → INT_MAX`
bail-out (`vidsoft.c:2843-2844`, commented "burning rangers tries to draw huge shapes"). Use a
`Vec` sized from the actual edge length, clipped against the system clip rectangle. Same for the
4096-command cap (`vdp1.cpp:570`, marked `// fix me`) — Mimas needs *some* runaway guard, but it
should be documented as Mimas's own guard with a chosen limit, not silently inherited as if it were
hardware.

Do port the **greedy** hole-filling (§5.0: an extra pixel emitted whenever the minor axis steps,
commented "If the line isn't greedy here, we end up with gaps that don't occur on the Saturn").
Span fills are greedy; edge walks are not. Skipping it produces visible gaps in rotated quads.

### AD-5 — VDP1 registers must stop being a plain byte array

Four of the eleven registers have write side effects (TVMR, FBCR, PTMR, ENDR) and four are
read-side computed (EDSR, LOPR, COPR, MODR). A `RwLock<Box<[u8; 0x1000]>>` (`shared_buffers.rs:39`)
can model neither.

The codebase already has both hooks:
- **Live reads**: `sh2.rs:457-460` special-cases VDP2's TVSTAT (offset `0x004`/`0x005`) to compute
  it rather than read stored bytes, with a long comment explaining exactly why. EDSR/LOPR/COPR/MODR
  take the same treatment.
- **Write side effects**: SMPC's COMREG dispatch is inline in `Sh2`'s write path
  (`smpc_execute_command`, `sh2.rs:826`).

**Gotcha worth planning around:** `Sh2::write_word` (`sh2.rs:625-637`) decomposes into two
`raw_write_byte` calls, so a byte-level hook would see PTMR half-written. Since reference §2.1 says
*all* meaningful VDP1 register access is 16-bit, add the dispatch at the **word level** in
`write_word`, after both bytes land, for `MemRegion::Vdp1Regs`. Byte and long writes stay no-ops
(matching `Vdp1WriteByte`/`Vdp1WriteLong`, reference §2.1).

Recommended shape: a `Vdp1State` struct (registers + §2.4 engine state) behind its own `Mutex`
hung off `SaturnSystem` and handed to `Sh2` as an `Option<Arc<...>>` field — the same pattern
`scu_dsp` already uses (`lib.rs:73`, `sh2.rs`'s `scu_dsp` field). Do **not** widen `Sh2::new()`.

---

## Phase 0 — Fix what's already there

Nothing below works until the command table is read correctly. This phase adds no features.

- [ ] Introduce a `CmdTable` reader with named fields at the offsets from reference §3
      (CMDCTRL/CMDLINK/CMDPMOD/CMDCOLR/CMDSRCA/CMDSIZE/CMDXA..CMDYD/CMDGRDA), replacing the
      ad-hoc reads at `vdp.rs:91-99`. Word 15 (`0x1E`) is reserved and not read.
- [ ] Coordinates are `i16` reinterpretations of the raw 16-bit word — **no narrowing to 11 or 12
      bits and no sign-extension from a narrower field** (reference §3.7: the code never masks).
- [ ] Move the CMDCTRL bit-15 test **before** dispatch, both at walker entry and at the top of each
      iteration (reference §4.1, §4.2), replacing `vdp.rs:120-122`.
- [ ] Honour CMDCTRL bit 14 (JP skip): skip the draw, still follow the link (reference §3.1).
- [ ] Read PTMR from offset `0x04`, not `0x00` (D2). Wire `1` → draw now, `2` → draw on frame
      change, `0` → nothing (reference §2.2). PTMR=1 must shift `EDSR >>= 1` *before* drawing.
- [ ] Downgrade `vdp.rs:83`'s `vdp1_vram.write()` to `.read()`.
- [ ] Detect COMM 12-15 as bad commands: `EDSR |= 2`, `LOPR = addr>>3`, `COPR = addr>>3`, status
      IDLE, return (reference §4.3). Even before EDSR is a real register, wire the abort.
- [ ] Add Mimas's own runaway guard on the command loop with a comment saying it is Mimas's guard,
      not hardware (AD-4).
- [ ] Rewrite `test_vdp1_polygon_drawing` (`vdp.rs:206-252`) against the real layout (D4).

**Testing.**
- [ ] `vdp1_cmdtable_field_offsets`: write a table with a distinct sentinel word in each of the 15
      defined slots and assert the reader returns each at the right field. Purely structural, but
      it is the test that would have caught D1.
- [ ] `vdp1_polygon_draws_at_correct_offsets` (the rewrite of the existing test): two-entry list —
      entry at `0x00` with CMDCTRL `0x0004`, CMDPMOD `0x0000`, CMDCOLR `0x801F`, CMDXA/YA `(10,10)`,
      CMDXB/YB `(20,10)`, CMDXC/YC `(20,20)`, CMDXD/YD `(10,20)`; entry at `0x20` with CMDCTRL
      `0x8000`. Hand-derived: an untextured polygon with colour-calc mode 0 writes
      `currentPixel = CMDCOLR = 0x801F` (reference §5.4, §7.1 mode 0); `render_backdrop`'s
      `rgb555_to_xrgb8888(0x801F)` gives r5 = `0x1F`, g5 = `0`, b5 = `0` → **`0xFF0000`** at
      `(15,15)`, and the backdrop's `0x0000FF` at `(0,0)`.
- [ ] `vdp1_end_bit_on_first_command_draws_nothing`: single command `0x8004` with the same colour
      and vertices → framebuffer stays all-zero. Straight from reference §4.1
      (`vdp1.cpp:561-565`). This is the assertion the old test got backwards.
- [ ] `vdp1_skip_bit_suppresses_draw_not_link`: CMDCTRL `0x4004` at `0x00`, `0x8000` at `0x20` →
      nothing drawn, walker still reaches `0x20`.
- [ ] `vdp1_bad_command_aborts`: COMM `0x000C` → `EDSR == 2`, `LOPR == COPR == 0`, and a following
      polygon at `0x20` is not drawn.

---

## Phase 1 — Engine state, live registers, and the state-setting commands

This phase creates the object the rest of the plan hangs off, and implements the three commands
that draw nothing but that every subsequent draw depends on.

- [ ] `Vdp1State` (AD-5) holding: TVMR, FBCR, PTMR, EWDR, EWLR, EWRR, ENDR, EDSR, LOPR, COPR, plus
      §2.4 engine state `addr`, `localX`/`localY` (`i16`), `systemclipX1/Y1/X2/Y2` and
      `userclipX1/Y1/X2/Y2` (**`u16` — unsigned**, reference §2.4/§8.2), `status` (IDLE/RUNNING),
      `return_addr: Option<u32>`.
- [ ] Reset defaults (reference §2.5, taking the values that *win* after `vdp1.cpp:392-399`
      overwrites the renderer's): userclip `(0,0)-(1024,1024)`, systemclip `(0,0)-(1024,1024)`,
      everything else 0, MODR synthesised. **Do not** port `Vdp1Reset`'s `sizeof(pointer)` memset
      bug (§2.5 item 1) or the `T1WriteWord(Vdp1Ram, 0x40000, 0x8000)` Radiant-Silvergun terminator
      (§2.5 item 3) — §10.
- [ ] Word-level write dispatch (AD-5) for offsets `0x00` TVMR, `0x02` FBCR, `0x04` PTMR, `0x06`
      EWDR, `0x08` EWLR, `0x0A` EWRR, `0x0C` ENDR. Writes to `0x0E` and `0x10`-`0x16` are
      discarded (reference §2.1).
- [ ] FBCR write side effect (reference §2.2): `(FBCR & 3) == 3` → `manualchange = true`;
      `(FBCR & 3) == 2` → `manualerase = true`.
- [ ] ENDR write: any value → `status = IDLE`, cancel any pending Draw End. Does **not** touch
      EDSR/COPR/LOPR (reference §2.2).
- [ ] Live register reads for `0x10` EDSR, `0x12` LOPR, `0x14` COPR, following the TVSTAT
      precedent at `sh2.rs:457-460`.
- [ ] MODR (`0x16`) synthesised on every read, never stored (reference §2.3):
      `0x1000 | ((PTMR & 2) << 7) | ((FBCR & 0x1E) << 3) | (TVMR & 0xF)`. FBCR bit 0 (FCT) and
      PTMR bit 0 are deliberately invisible.
- [ ] Register reads at `0x00`-`0x0C` return 0; byte reads and long reads of *any* VDP1 register
      return 0; byte/long writes are no-ops (reference §2.1).
- [ ] Mirror VDP1 register offsets with `& 0xFF` and framebuffer offsets with `& 0x3FFFF` in
      `sh2.rs:429-440`/`:527-541` (reference §1; D5).
- [ ] COPR maintenance: set at walker entry and at the top of **every** iteration before dispatch,
      as `addr >> 3` (reference §2.3). Consecutive tables are `0x20` apart → COPR steps by 4.
- [ ] LOPR maintenance: written **only** on the two abort paths (bad opcode, and `EDSR & 2` already
      set at loop top). Reference §2.3 flags that real hardware probably maintains it more
      generally — do not invent that; keep the abort-only behaviour and note it.
- [ ] The "Batsugun escape": if `EDSR & 0x02` is set at the top of a loop iteration, write
      LOPR/COPR, set status IDLE and return (reference §4.2 step 3).
- [ ] Jump/link processing (reference §4.4), replacing `vdp.rs:123`'s unconditional `+= 32`:
      - JP 0 NEXT → `addr += 0x20`
      - JP 1 ASSIGN → `addr = CMDLINK * 8`
      - JP 2 CALL → if no return address pending, `return_addr = addr + 0x20`; then jump to
        `CMDLINK * 8`. A nested CALL **does not** overwrite a pending return address.
      - JP 3 RETURN → if a return address is pending, take it and clear it; else `addr += 0x20`.
      - For JP 1/2/3 only, `addr == 0` aborts (reference §4.4). This is a Yabause guard, not
        hardware — port it (it prevents a real hang) but comment it as such (§10).
- [ ] Walker entry guard: `addr > 0x7FFFF` → status IDLE, return, **no error flag** (reference
      §4.1).
- [ ] Draw start (reference §4.6): `addr` and COPR reset to 0 **only if status is IDLE** — a
      RUNNING engine *resumes* where it left off. The command list root is fixed at VRAM offset 0;
      there is no pointer register.
- [ ] **COMM 10 Local Coordinates** (reference §5.9): `localX = word[addr+0x0C]`,
      `localY = word[addr+0x0E]`, reinterpreted **signed**. Draws nothing.
- [ ] **COMM 9 System Clipping** (reference §5.8): `systemclipX1 = systemclipY1 = 0` (hard-wired —
      CMDXA/CMDYA ignored), `systemclipX2 = word[addr+0x14]`, `systemclipY2 = word[addr+0x16]`.
- [ ] **COMM 8 and COMM 11 User Clipping** (reference §5.7): `userclipX1 = word[addr+0x0C]`,
      `userclipY1 = word[addr+0x0E]`, `userclipX2 = word[addr+0x14]`, `userclipY2 =
      word[addr+0x16]`. Stored **unsigned**; `localX`/`localY` are **not** applied. CMDXB/YB and
      CMDXD/YD unused.
- [ ] Clip rectangles and local coordinates **persist across command lists and frames** — they are
      not reset at draw start (reference §9.5's "night warriors" note).
- [ ] AD-1's event-driven trigger: PTMR=1 sets a trigger the Core 3 loop checks every iteration,
      moved out of the `now >= next_frame_due` gate at `lib.rs:215`.

**Testing.**
- [ ] `vdp1_modr_is_synthesised`: TVMR = `0x000B`, FBCR = `0x001A`, PTMR = `0x0002`. Hand-derived:
      `0x1000 | ((2 & 2) << 7) | ((0x1A & 0x1E) << 3) | (0x0B & 0xF)` = `0x1000 | 0x0100 | 0x00D0 |
      0x000B` = **`0x11DB`**. Also assert a write to `0x16` is discarded and the next read still
      returns `0x11DB`.
- [ ] `vdp1_write_only_registers_read_zero` / `vdp1_byte_and_long_register_access_is_dead`: reads of
      `0x00`-`0x0C` → 0; byte read of `0x10` → 0; long read of `0x10` → 0; byte write to `0x04`
      does not trigger a plot.
- [ ] `vdp1_copr_tracks_command_index`: three commands at `0x00`, `0x20`, `0x40` (the last being
      Draw End). Hand-derived COPR values `0x00>>3 = 0`, `0x20>>3 = 4`, `0x40>>3 = 8`; assert final
      COPR `== 8`.
- [ ] `vdp1_jp_assign_follows_cmdlink`: CMDCTRL `0x1004` at `0x00` with CMDLINK `0x0010` → next
      command address `0x10 * 8 = 0x80`. Place the Draw End there and assert COPR `== 0x80>>3 = 16`.
- [ ] `vdp1_jp_call_and_return`: CALL (`0x2004`) at `0x00`, CMDLINK `0x0010` → executes at `0x80`;
      RETURN (`0x3004`) at `0x80` → back to `0x20`. Assert the command at `0x20` executes.
- [ ] `vdp1_nested_call_loses_inner_return`: CALL at `0x00` → `0x80`; CALL at `0x80` → `0x100`;
      RETURN at `0x100` → returns to `0x20`, **not** `0xA0` (reference §4.4).
- [ ] `vdp1_return_without_call_acts_as_next`: RETURN at `0x00` → `0x20`.
- [ ] `vdp1_local_coordinate_offsets_subsequent_draws`: COMM 10 with CMDXA `100`, CMDYA `50`, then a
      polygon with CMDXA `10`, CMDYA `10` → the fill starts at `(110, 60)`.
- [ ] `vdp1_local_coordinate_persists_across_lists`: run the list above, then run a second list with
      no COMM 10 → the offset is still `(100, 50)` (reference §9.5).
- [ ] `vdp1_system_clip_ignores_xa_ya`: COMM 9 with CMDXA `40`, CMDYA `40`, CMDXC `100`, CMDYC `50`
      → `systemclipX1 == 0`, `systemclipY1 == 0`, `systemclipX2 == 100`, `systemclipY2 == 50`.
- [ ] `vdp1_endr_forces_idle_without_touching_edsr`: seed EDSR `= 2`, COPR `= 7`; write ENDR → status
      IDLE, EDSR still `2`, COPR still `7`.
- [ ] `vdp1_addr_error_aborts_silently`: force `addr = 0x80000` → status IDLE, EDSR unchanged.

---

## Phase 2 — Framebuffer geometry, two banks, erase and swap

- [ ] `Vdp1Geometry::from_tvmr(tvmr) -> (width, height, pixel_size)` per reference §2.2's table:

      | TVM1 | TVM0 | width | height | pixel size |
      |---|---|---|---|---|
      | 0 | 0 | 512 | 256 | 2 |
      | 1 | 0 | 512 | 256 | 2 |
      | 0 | 1 | 1024 | 256 | 1 |
      | 1 | 1 | 512 | 512 | 1 |

      All four are exactly `0x40000` bytes. Recomputed at every draw start (reference §9.5), never
      cached across frames.
- [ ] Replace `shared_buffers.rs:35-37`'s flat `vdp1_framebuffer` with the two-bank
      `Vdp1Framebuffers` from AD-2.
- [ ] Route the CPU port (`sh2.rs:433-436`, `:532-536`) at `0x05C80000` to the **back** bank, masked
      `& 0x3FFFF` (reference §1). Reference §1 notes Yabause byte-swaps 16/32-bit CPU-port accesses
      and additionally swaps halfwords on 32-bit; Mimas stores big-endian natively
      (`sh2.rs:625-637` decomposes MSB-first), so **no swap is needed** — that is a host-endianness
      artefact of the C code, not hardware. Record that reasoning in a comment.
- [ ] Do **not** port the `#if 0`'d 32-bit framebuffer read returning 0 (reference §1, "enable when
      burning rangers is fixed"). Return real data — §10.
- [ ] Frame-change decision, once per frame (reference §9.1): `manualchange` → `swap_frame_buffer`;
      `(FBCR & 3) == 0 || (FBCR & 3) == 1` → `swap_frame_buffer`; `frame_change_plot = (PTMR == 2)`.
      Port the `== 1` case with a comment naming it as the *Sonic R* workaround it is (§10).
- [ ] VBlank erase: at VBlank-IN, `vbalnk_erase = (TVMR >> 3) & 1` (reference §9.1). Erase runs when
      `vbalnk_erase || (FBCR & 2) == 0`.
- [ ] Erase implementation (reference §9.4), 16-bit path:
      - `h = (EWRR & 0x1FF) + 1`, clamped to height
      - `w = ((EWRR >> 6) & 0x3F8) + 8`, clamped to width
      - rows from `EWLR & 0x1FF`, columns from `(EWLR >> 6) & 0x1F8`
      - fill value `EWDR` (full 16-bit word), into the **back** bank
      - X granularity 8 pixels on both ends, Y granularity 1 line; end coordinates exclusive after
        the `+1`/`+8`
- [ ] Erase 8-bit path (reference §9.4 / §2.2): `w = (EWRR >> 9) * 16` recomputed **after** the
      clamp, fill byte `EWDR & 0xFF`, bounds-guarded. Reference §2.2 flags the missing re-clamp as
      an apparent oversight and the one-byte-per-pixel fill as **[Ambiguous]**; port as documented
      and comment both (§10).
- [ ] Swap (reference §9.3): gated on `(FBCR & 2) == 0 || manualchange`, flip the `back` index
      `Release`, clear `manualchange`.
- [ ] Frame ordering (reference §9.2): **VBlank erase → manual erase → swap → `EDSR >>= 1` →
      conditional draw start**. Reference §9.2/§12 item 15 says the two Yabause renderers disagree
      about *when* erase and swap happen; adopt the `vdp2.cpp` ordering above (it is the one the
      register semantics are written against) and record the choice.
- [ ] After a swap, if `frame_change_plot` or status is RUNNING: `addr = 0`, `COPR = 0`, draw. If no
      swap happened and status is RUNNING, resume the draw (reference §9.2).
- [ ] Fix `render_backdrop`'s VDP1 overlay (`vdp.rs:141-156`) to read the **front** bank at the real
      `vdp1width` stride instead of the hardcoded 320 (`vdp.rs:145`). Expose the geometry so
      `implementation-plans/vdp2.md`'s readout-scaling work (reference §11's 1024/512 vs hi/lo-res
      matching) consumes it rather than re-deriving it.
- [ ] Contract to hand VDP2: reference §11 — a framebuffer word of `0x0000` is transparent; a word
      with bit 15 set is direct RGB *when VDP2's `SPCTL & 0x20` is set*; otherwise it is a
      colour-bank index offset by `(CRAOFB & 0x70) << 4`. VDP1 guarantees only *what it writes*;
      the interpretation is VDP2's. Today `vdp.rs:148` treats every non-zero word as direct RGB —
      correct only for mode-5 sprites and untextured shapes with bit 15 set.

**Testing.**
- [ ] `vdp1_geometry_from_tvmr`: all four TVM1/TVM0 combinations against the table above; assert
      each product is `0x40000` bytes at the stated pixel size.
- [ ] `vdp1_erase_rect_decode`: `EWDR = 0x1234`, `EWLR = 0x0000`, `EWRR = 0x0203`. Hand-derived:
      `h = (0x0203 & 0x1FF) + 1 = 3 + 1 = 4`; `w = ((0x0203 >> 6) & 0x3F8) + 8 = (8 & 0x3F8) + 8 =
      16`. Cross-check by the other form in §2.2: `((0x0203 >> 9) & 0x7F) * 8 + 8 = 1 * 8 + 8 = 16`.
      Assert back-bank word at `(15, 3)` `== 0x1234`, at `(16, 3)` `== 0`, at `(0, 4)` `== 0`.
- [ ] `vdp1_erase_start_coordinate`: `EWLR = 0x0402`. Hand-derived `X1 = ((0x0402 >> 9) & 0x3F) * 8
      = 2 * 8 = 16`, cross-checked as `(0x0402 >> 6) & 0x1F8 = 0x10 & 0x1F8 = 16`;
      `Y1 = 0x0402 & 0x1FF = 2`. Assert `(15, 2)` untouched, `(16, 2)` filled.
- [ ] `vdp1_erase_targets_back_bank_only`: prefill both banks with a sentinel; erase; assert only
      the back bank changed.
- [ ] `vdp1_manual_change_swaps_banks`: draw a known word into the back bank, write FBCR `0x0003`,
      run the frame-change path → the *front* bank now holds it and the CPU port (back bank) sees
      the other one.
- [ ] `vdp1_one_cycle_mode_erases_and_swaps_every_frame`: FBCR `0x0000`, two frame ticks → erase
      ran twice, bank index returned to its starting value.
- [ ] `vdp1_manual_erase_runs_just_before_swap`: FBCR `0x0002` then `0x0003`; assert the erase
      applied to the bank that is about to become the front one.
- [ ] `vdp1_cpu_port_reads_back_bank`: write through `0x05C80000` and assert it lands in the back
      bank; swap; assert the same address now reads the other bank's contents.
- [ ] `vdp1_framebuffer_mirrors_every_256k`: write at `0x05C80000` and read at `0x05CC0000`.

---

## Phase 3 — Draw End status and interrupt

- [ ] EDSR transitions, the complete list from reference §10.1:
      - bad command opcode → `EDSR |= 2`
      - `EDSR & 2` already set at loop top → LOPR/COPR written, no further EDSR change
      - PTMR written `1` → `EDSR >>= 1` **before** drawing
      - frame change executed → `EDSR >>= 1`
      - draw completed → `EDSR |= 2` **and** raise Draw End
      - display-off path (`Vdp1NoDraw` equivalent) → `EDSR |= 2` **and** raise Draw End immediately
- [ ] Bit semantics: bit 1 = CEF (current end), bit 0 = BEF (before end, the previous CEF shifted
      down).
- [ ] `raise_draw_end()` per AD-3, plus the `Sh2` side (`draw_end_irq` field, `DRAW_END_VECTOR
      0x4D`, `DRAW_END_LEVEL 2`, lowest-priority branch in `service_pending_interrupt`).
- [ ] Order: raise Draw End **then** set CEF (reference §10.3's note).
- [ ] Display-off path: reset COPR to 0, walk the list executing **only** COMM 8/9/10/11 (the
      "fake draw" of reference §4.5, which keeps clip and local-coordinate state current for the
      CPU to read back), then signal completion unconditionally.
- [ ] The five paths that leave **no** Draw End signalled (reference §10.3): ENDR written; runaway
      guard hit; `addr > 0x7FFFF` at entry; first command is Draw End; bad jump to 0 (status goes
      IDLE, so an already-armed deadline still fires).
- [ ] **Deliberately not ported**: the `wait_line_count = LineCount + 50` scheduling and its
      `+10`-line retry (reference §2.2, §10.3). It is explicitly a hand-tuned emulator figure
      ("not a hardware timing figure"), and Mimas has no scanline counter today. Signal Draw End at
      the end of the command list. Revisit only if a real BIOS/game turns out to depend on the
      delay — record that as the trigger condition rather than pre-building it.

**Testing.**
- [ ] `vdp1_edsr_shift_on_plot_trigger`: seed EDSR `= 0b10` (CEF set); write PTMR `= 1`; assert EDSR
      reads `0b01` before completion (BEF set, CEF clear) and `0b11 == 3` after.
- [ ] `vdp1_edsr_shift_on_frame_change`: seed EDSR `= 0b11`; run a frame change → `0b01`.
- [ ] `vdp1_bad_command_sets_cef_and_lopr`: bad command at `0x20` → `EDSR == 2`, `LOPR == 0x20>>3 ==
      4`, `COPR == 4`.
- [ ] `vdp1_draw_end_raises_interrupt`: with a `draw_end_irq` flag wired, a completed list sets it.
- [ ] `vdp1_endr_suppresses_draw_end`: arm a draw, write ENDR → flag stays clear.
- [ ] `draw_end_enters_through_vector_0x4d_at_level_2` in `sh2.rs`'s test module, modelled on the
      existing `sound_req_irq_enters_through_its_own_vector_at_level_9` (`sh2.rs:2037`): install a
      handler address at `VBR + 0x4D*4`, set the flag, step, assert PC.
- [ ] `draw_end_stays_pending_while_masked`: SR I-mask `= 2` → level 2 is **not** taken (the test is
      `level <= mask`), flag stays set; drop the mask to 1 → taken.
- [ ] `draw_end_yields_to_every_higher_interrupt`: Draw End plus VBLANK-IN pending → VBLANK-IN
      first, Draw End still pending.

---

## Phase 4 — The rasteriser, Normal Sprite, and colour-mode decode

The first phase that puts a *correct* textured pixel on screen. Reference §5.0, §5.1, §6, §7.1,
§8. Priority reasoning: Normal Sprite (COMM 0) is the unscaled, unrotated, axis-aligned blit — the
form a boot logo's static glyphs, a BIOS menu's text and icons, and any early-game HUD element use.
It needs the full colour-mode decode to render at all, so the two land together.

- [ ] The shared quad rasteriser per AD-4 and reference §5.0: `draw_quad(tl, bl, tr, br)` — note the
      argument order is top-left, **bottom-left**, top-right, bottom-right.
      - [ ] Pre-clip trivial reject (reference §8.4): reject only when *all four* X `< 0`, or all X
            `> systemclipX2`, or all Y `< 0`, or all Y `> systemclipY2`. System rect only, never
            the user rect. Runs unconditionally (CMDPMOD bit 11 is ignored — §10).
      - [ ] `characterWidth = ((CMDSIZE >> 8) & 0x3F) * 8`, `characterHeight = CMDSIZE & 0xFF`
            (reference §3.6).
      - [ ] Walk left edge tl→bl and right edge tr→br non-greedily; `total = max(len_left,
            len_right)`; sub-sample the shorter edge by `len_short / len_long`.
      - [ ] Per span index `i`: measure the span length, `xtexturestep = characterWidth /
            span_length`, `ytexturestep = characterHeight / total` (computed **once per quad**), row
            = `ytexturestep * i`. Draw greedily.
      - [ ] `interpolate(start, end, n) = (end - start) / n`, returning **1** when `n == 0`
            (reference §5.0).
      - [ ] The gouraud accumulator steps **before** the first pixel of a span is drawn (reference
            §5.0) — the first pixel already carries one step of offset. Stub the accumulator here;
            Phase 6 fills it in.
- [ ] Texture fetch helpers (reference §3.5):
      - `read_pattern_16(base, off)` → byte at `base + (off >> 1)`, **high nibble when `off` is
        even**, low nibble when odd
      - `read_pattern_64(base, off)` → byte `& 0x3F`
      - `read_pattern_128(base, off)` → byte `& 0x7F`
      - `read_pattern_256(base, off)` → byte
      - `read_pattern_64k(base, off)` → word at `base + 2*off`
      - all masked `& 0x7FFFF`
- [ ] `characterAddress = CMDSRCA << 3` (reference §3.5). Row stride: `width/2` for modes 0-1,
      `width` for modes 2-4, `width*2` for mode 5.
- [ ] Colour mode decode, all six (reference §6), producing `(current_pixel, visibility_mask)`:
      - mode 0 → `(CMDCOLR & 0xFFF0) | index`, mask `0x000F`
      - mode 1 → `word[(index*2 + (CMDCOLR << 3)) & 0x7FFFF]`, mask `0xFFFF`
      - mode 2 → `(CMDCOLR & 0xFFC0) | index`, mask `0x003F`
      - mode 3 → `(CMDCOLR & 0xFF80) | index`, mask `0x007F`
      - mode 4 → `(CMDCOLR & 0xFF00) | index`, mask `0x00FF`
      - mode 5 → the word verbatim, mask `0xFFFF`
      - modes 6/7 → no case; §10
- [ ] Transparency, modes 0-4 (reference §6): when `index == 0 && !SPD` the colour bank is **not**
      OR'd in, leaving `current_pixel == 0`; the write is suppressed later in the pixel writer.
- [ ] Transparency, mode 5 (reference §6): `if !(pixel & 0x8000) && !SPD { pixel = 0 }` — *any* word
      without bit 15, not just `0x0000`. Carry the reference's explanatory comment.
- [ ] `SPD = (CMDPMOD & 0x40) != 0` (reference §3.3) — set means draw index/colour 0 opaquely.
- [ ] The untextured override (reference §3.4, §5.4): `currentShape = CMDCTRL & 0x7`; COMM 4, 5 and
      6 are untextured and take `current_pixel = CMDCOLR` **after** the colour-mode switch has run.
      COMM 7 is treated as *textured* because `7 & 7 == 7` is not in the untextured set (reference
      §3.1, **[Ambiguous]**) — port as documented, comment it.
- [ ] Per-pixel writer, 16-bit framebuffer (reference §7.1), in this exact order: interlace
      rejection → address + upper bound check → mesh → clip → MSB-on → visibility gate
      (`SPD || (current_pixel & visibility_mask)`) → colour calculation. Phase 4 implements only
      colour-calc mode 0 (`if !(current_pixel == 0 && !SPD) { *pix = current_pixel }`); Phase 6
      adds the rest.
- [ ] Per-pixel clipping (reference §8.3):
      - system clip always applies: `!(x >= 0 && x <= systemclipX2 && y >= 0 && y <= systemclipY2)`
        — **inclusive** upper bound, `systemclipX1`/`Y1` never read
      - CMDPMOD bit 10 set → also user clip, inclusive on both bounds, clip bounds compared as
        **unsigned** (reference §8.2)
      - CMDPMOD bits 10+9 both set → the user test is inverted (draw *outside* the rect)
- [ ] Mimas's own lower-bound guard on the framebuffer index. Reference §7.1 step 3 notes Yabause
      has an **upper bound only**, so a negative `x` can index into the previous row. Do not port
      that; comment the divergence (§10).
- [ ] **COMM 0 Normal Sprite** (reference §5.1): only CMDXA/CMDYA are used.
      `tl = (CMDXA + localX, CMDYA + localY)`, `tr = (tl.x + spriteWidth - 1, tl.y)`,
      `br = (tl.x + spriteWidth - 1, tl.y + spriteHeight - 1)`, `bl = (tl.x, tl.y + spriteHeight -
      1)`, then `draw_quad(tl, bl, tr, br)`. Coordinates are `s16` here (unlike Scaled Sprite's
      `s32`).

**Testing.** Every expected value below is hand-derived from reference §6's table; put the
derivation in the test comment, per `CLAUDE.md`.

- [ ] `vdp1_normal_sprite_geometry`: CMDSIZE `0x0204` → width `((0x0204 >> 8) & 0x3F) * 8 = 2 * 8 =
      16`, height `0x04 = 4`. CMDXA `32`, CMDYA `8` → corners `(32,8)`, `(47,8)`, `(47,11)`,
      `(32,11)`. Assert written at `(32,8)` and `(47,11)`, not at `(48,8)` or `(32,12)`.
- [ ] `vdp1_colour_mode_0_bank`: CMDPMOD `0x0000`, CMDCOLR `0x0120`, CMDSRCA `0x0010` → character
      address `0x10 << 3 = 0x80`; CMDSIZE `0x0101` → 8×1; VRAM byte at `0x80` `= 0x37` → pixel 0 is
      the high nibble `3`, pixel 1 the low nibble `7`. Expected framebuffer words
      `(0x0120 & 0xFFF0) | 3 = 0x0123` and `0x0127`.
- [ ] `vdp1_colour_mode_0_index_zero_is_transparent`: same setup, VRAM byte `0x07`; prefill the
      framebuffer with `0xBEEF`; with SPD=0 pixel 0 keeps `0xBEEF` and pixel 1 becomes `0x0127`.
      With CMDPMOD `0x0040` (SPD=1) pixel 0 becomes `0x0120`.
- [ ] `vdp1_colour_mode_1_lut`: CMDPMOD `0x0008`, CMDCOLR `0x0100` → LUT base `0x100 << 3 = 0x800`;
      texture index `3` → entry at `0x800 + 3*2 = 0x806`; put `0x1234` there → framebuffer word
      `0x1234` verbatim.
- [ ] `vdp1_colour_mode_2_64_colour`: CMDPMOD `0x0010`, CMDCOLR `0x0A3F`, texture byte `0xC5` →
      `0xC5 & 0x3F = 0x05`; `(0x0A3F & 0xFFC0) | 0x05 = 0x0A00 | 0x05 = 0x0A05`.
- [ ] `vdp1_colour_mode_3_128_colour`: CMDPMOD `0x0018`, same inputs → `0xC5 & 0x7F = 0x45`;
      `(0x0A3F & 0xFF80) | 0x45 = 0x0A00 | 0x45 = 0x0A45`.
- [ ] `vdp1_colour_mode_4_256_colour`: CMDPMOD `0x0020`, same inputs → `(0x0A3F & 0xFF00) | 0xC5 =
      0x0A00 | 0xC5 = 0x0AC5`.
- [ ] `vdp1_colour_mode_5_rgb`: CMDPMOD `0x0028`; texture word `0x8ABC` → written verbatim; texture
      word `0x0ABC` (bit 15 clear) with SPD=0 → transparent, framebuffer unchanged; the same word
      with CMDPMOD `0x0068` (SPD=1) → written as `0x0ABC`.
- [ ] `vdp1_4bpp_is_high_nibble_first`: a one-byte 2×1 character `0x37` renders `3` then `7`, not
      `7` then `3` (reference §3.5).
- [ ] `vdp1_row_stride_per_colour_mode`: a 2-row character in modes 0, 2 and 5 with distinct row
      bytes → assert row 1 comes from `+width/2`, `+width` and `+width*2` respectively.
- [ ] `vdp1_system_clip_is_inclusive`: COMM 9 with CMDXC `100`, CMDYC `50`; a polygon covering
      `(90,45)-(110,60)` → `(100,50)` drawn, `(101,50)` not, `(100,51)` not.
- [ ] `vdp1_user_clip_inside`: COMM 8 with `(20,20)-(30,30)`; polygon `(10,10)-(40,40)` with CMDPMOD
      `0x0400` → only `[20,30]×[20,30]` written.
- [ ] `vdp1_user_clip_outside`: same, CMDPMOD `0x0600` → everything within the system rect *except*
      `[20,30]×[20,30]`.
- [ ] `vdp1_preclip_rejects_fully_offscreen_quad`: all four X `> systemclipX2` → nothing drawn, and
      (to prove the *trivial-reject* semantics) a quad with three corners off-screen and one on
      **is** drawn.

---

## Phase 5 — The remaining textured shape commands

- [ ] **COMM 1 Scaled Sprite** (reference §5.2). `x0, y0 = CMDXA + localX, CMDYA + localY`; the ZP
      field `(CMDCTRL >> 8) & 0xF` selects the anchor. Implement all ten cases the reference
      documents:

      | ZP | Anchor | Extent |
      |---|---|---|
      | `0x0` / default | two-point | `x1 = CMDXC - x0 + localX + 1`, `y1 = CMDYC - y0 + localY + 1` |
      | `0x5` | upper-left | `x1 = CMDXB + 1`, `y1 = CMDYB + 1` |
      | `0x6` | upper-centre | `x1 = CMDXB`, `y1 = CMDYB`, `x0 -= x1/2`, then `x1++`, `y1++` |
      | `0x7` | upper-right | `x0 -= x1`, then `x1++`, `y1++` |
      | `0x9` | centre-left | `y0 -= y1/2`, then `x1++`, `y1++` |
      | `0xA` | centre-centre | `x0 -= x1/2`, `y0 -= y1/2`, then `x1++`, `y1++` |
      | `0xB` | centre-right | `x0 -= x1`, `y0 -= y1/2`, then `x1++`, `y1++` |
      | `0xD` | lower-left | `y0 -= y1`, then `x1++`, `y1++` |
      | `0xE` | lower-centre | `x0 -= x1/2`, `y0 -= y1`, then `x1++`, `y1++` |
      | `0xF` | lower-right | `x0 -= x1`, `y0 -= y1`, then `x1++`, `y1++` |

      For every non-zero ZP, `x1`/`y1` start as **CMDXB/CMDYB** (a size, not a corner). Values
      `0x1`-`0x4`, `0x8`, `0xC` fall through to two-point mode. Reference §5.2 flags a
      **[Ambiguous]** debugger/renderer disagreement over `0xC` vs `0xD` for lower-left; follow the
      renderer (`0xD`), because the bit-field decomposition (bits 11-10 = vertical anchor, bits 9-8
      = horizontal) supports it. Comment the choice.
- [ ] Final quad computed in `s32` (unlike Normal Sprite): `tl = (x0, y0)`,
      `tr = (x0+x1-1, y0)`, `br = (x0+x1-1, y0+y1-1)`, `bl = (x0, y0+y1-1)`.
- [ ] **COMM 2 and COMM 3 Distorted Sprite** (reference §5.3). All four vertices, each offset by
      local coordinates and widened to `s32`, then `draw_quad(A, D, B, C)` — **A top-left, D
      bottom-left, B top-right, C bottom-right**, so the left edge is A→D and the right edge B→C.
      COMM 3 is the undocumented mirror (Hardcore 4x4 uses it); dispatch it to the same code.
- [ ] **COMM 4 Polygon** (reference §5.4) shares the geometry with Distorted Sprite exactly; the
      only difference is the untextured override already added in Phase 4. Reference §5.4 flags as
      **[Ambiguous]** that the colour-mode switch still runs for polygons (so an untextured polygon
      still reads texture VRAM and still computes a visibility mask from CMDPMOD's colour-mode
      field). Port as documented — it changes observable behaviour via the visibility gate — and
      comment it as a suspected code-sharing artefact.
- [ ] Delete the AABB fill (`vdp.rs:102-118`) once the quad path covers COMM 4.

**Testing.**
- [ ] `vdp1_scaled_sprite_zp_upper_left`: CMDCTRL `0x0501`, CMDXA `10`, CMDYA `10`, CMDXB `31`,
      CMDYB `15`. Hand-derived `x1 = 32`, `y1 = 16` → quad `(10,10)`, `(41,10)`, `(41,25)`,
      `(10,25)`.
- [ ] `vdp1_scaled_sprite_zp_two_point_matches_upper_left`: CMDCTRL `0x0001`, CMDXA `10`, CMDYA
      `10`, CMDXC `41`, CMDYC `25` → `x1 = 41 - 10 + 0 + 1 = 32`, `y1 = 25 - 10 + 0 + 1 = 16`, the
      *same* quad as above. Two independent derivations of one result.
- [ ] `vdp1_scaled_sprite_zp_two_point_ignores_local`: because `x0` already includes `localX` and
      the formula adds `localX` back, the local offset cancels out of the *extent* (but not the
      origin). Assert with `localX = 100`: origin moves, size does not.
- [ ] `vdp1_scaled_sprite_zp_centre_centre`: CMDCTRL `0x0A01`, CMDXA `10`, CMDYA `10`, CMDXB `31`,
      CMDYB `15`. Hand-derived: `x1 = 31`, `x0 = 10 - 15 = -5`; `y1 = 15`, `y0 = 10 - 7 = 3`; then
      `x1 = 32`, `y1 = 16` → quad `(-5,3)`, `(26,3)`, `(26,18)`, `(-5,18)`. Also exercises negative
      coordinates against the clip path.
- [ ] `vdp1_scaled_sprite_unimplemented_zp_falls_back_to_two_point`: ZP `0x4` and ZP `0xC` produce
      the two-point quad.
- [ ] `vdp1_distorted_sprite_vertex_order`: four vertices forming a *non*-axis-aligned quad; assert
      a pixel that is inside the true quad but outside another plausible vertex ordering is drawn,
      and one that is inside the wrong ordering's quad but outside the true one is not. This is the
      test that catches an A/B/C/D → tl/bl/tr/br mix-up, which an axis-aligned test cannot.
- [ ] `vdp1_polygon_and_distorted_sprite_have_identical_geometry`: the same four vertices as COMM 2
      (textured, 1×1 character of a known colour) and COMM 4 (untextured, CMDCOLR that same colour)
      → identical framebuffer contents.
- [ ] `vdp1_scaled_sprite_magnifies_texture`: an 8-wide character stretched across a 32-wide span →
      each source column occupies four destination columns. **Before asserting exact columns,
      re-verify against `vidsoft.c:2932` whether `currentStep = (int)i * texturestep` truncates
      per-pixel or accumulates in floating point** — reference §5.0 does not settle it, and an
      assertion built on the wrong one is exactly the self-consistent-but-wrong test to avoid.

---

## Phase 6 — Colour calculation, gouraud, mesh, MSB

Reference §7. Everything here operates on the framebuffer word as though it were XBGR-1555,
regardless of whether it actually holds an index (reference §11 explains why).

- [ ] Colour-calculation modes, CMDPMOD bits 2-0 (reference §7.1):
      - 0 **Replace** — `if !(current_pixel == 0 && !SPD) { *pix = current_pixel }`
      - 1 **Shadow / cannot overwrite** — `if *pix & 0x8000 { *pix = alphablend(*pix, 0, 128) | 0x8000 }`
      - 2 **Half luminance** — `*pix = ((current_pixel & !0x8421) >> 1) | 0x8000`
      - 3 **Replace / half transparent** — `if *pix & 0x8000 { *pix = alphablend(*pix, current_pixel, 128) | 0x8000 } else { *pix = current_pixel }`
      - 4 **Gouraud** — below
      - 5, 6, 7 — collapsed into one `default:` in the reference:
        `*pix = alphablend(gouraud_colour, current_pixel, 128) | 0x8000`. Modes 6 and 7 are
        **not** distinguished and mode 6's half-luminance component is not applied (reference
        §7.1, **[Ambiguous]**). Port as documented; §10.
- [ ] `alphablend16(d, s, level)` per 5-bit channel: `(s*level + d*(256-level)) >> 8`, result has
      bit 15 clear (callers OR it back).
- [ ] Gouraud table fetch (reference §3.8): `table = CMDGRDA << 3`; four consecutive words at `+0`,
      `+2`, `+4`, `+6` = corners A, B, C, D. Each is XBGR-1555 — **bits 4-0 = r, 9-5 = g, 14-10 =
      b, 15 = x**. `drawQuad` fetches the table only when CMDPMOD bit 2 is set (colour-calc modes
      4-7); Line and Polyline fetch it **unconditionally** (reference §5.5).
- [ ] `gouraud_adjust(colour, table_value) = clamp(colour + (table_value - 0x10), 0, 0x1F)` — a
      table channel of `0x10` is neutral, below darkens, above brightens, saturating (reference
      §7.1).
- [ ] The sgl-chrome-demo special case (reference §7.1): in mode 4, when the colour mode is neither
      1 nor 5 and the gouraud g and b are *both* exactly `0x10`, add `max(r - 0x10, 0)` to the
      **palette index** and write it raw. Comment it as the documented special case it is.
- [ ] MSB-on, CMDPMOD bit 15 (reference §7.1 step 6): if `current_pixel` is non-zero, `*pix |=
      0x8000` and **return** — nothing else is written.
- [ ] Mesh, CMDPMOD bit 8 (reference §7.1 step 4): skip the pixel when `(x ^ y) & 1`.
- [ ] `putpixel8` for the 8-bit framebuffer (reference §7.2): `current_pixel &= 0xFF`; mesh uses
      `y / interlace` rather than raw `y`; **only colour-calc mode 0 is implemented** and MSB-on is
      not handled at all. Both are documented reference limitations — port as-is and comment
      (§10).
- [ ] The gouraud accumulator advances **before** the first pixel of each span (reference §5.0).

**Testing.**
- [ ] `vdp1_colour_calc_2_half_luminance`: `current_pixel = 0x7FFF`. Hand-derived:
      `!0x8421 = 0x7BDE`; `0x7FFF & 0x7BDE = 0x7BDE`; `>> 1 = 0x3DEF`; `| 0x8000` → **`0xBDEF`**.
- [ ] `vdp1_colour_calc_3_half_transparent`: existing `*pix = 0x83FF` (r `0x1F`, g `0x1F`, b `0x00`,
      bit 15 set), `current_pixel = 0x7C00` (r `0`, g `0`, b `0x1F`). Per channel at level 128 this
      is `(s + d) >> 1`: r `(0+31)>>1 = 15`, g `(0+31)>>1 = 15`, b `(31+0)>>1 = 15` →
      `15 | (15<<5) | (15<<10) = 0x3DEF`; `| 0x8000` → **`0xBDEF`**.
- [ ] `vdp1_colour_calc_3_replaces_when_msb_clear`: existing `*pix = 0x03FF` (bit 15 clear) → result
      is `current_pixel` verbatim, no blend.
- [ ] `vdp1_colour_calc_1_shadow_only_where_msb_set`: two adjacent pixels, one prefilled `0x83FF`
      and one `0x03FF` → the first is halved and keeps bit 15, the second is untouched.
- [ ] `vdp1_gouraud_neutral_table_is_identity`: all four corners `= 0x4210` (r `0x10`, g `0x10`, b
      `0x10`: `0x10 | (0x10<<5) | (0x10<<10) = 0x10 | 0x200 | 0x4000`). Every channel adjust is
      `+0`, so the output equals the input pixel with bit 15 forced set by `COLOR()`.
- [ ] `vdp1_gouraud_darkens_red`: all four corners `= 0x4208` (r `0x08`, g/b `0x10`), input pixel
      r `= 0x1F` → `0x1F + (0x08 - 0x10) = 0x17`; g and b unchanged.
- [ ] `vdp1_gouraud_clamps`: table r `= 0x00` with input r `= 0x02` → `max(0x02 - 0x10, 0) = 0`;
      table r `= 0x1F` with input r `= 0x1D` → `min(0x1D + 0x0F, 0x1F) = 0x1F`.
- [ ] `vdp1_gouraud_index_special_case`: colour mode 0, table `= 0x4218` (r `0x18`, g/b `0x10`),
      `current_pixel = 0x0123` → `c = 0x18 - 0x10 = 8`, written raw as **`0x012B`**, *not* run
      through the RGB path.
- [ ] `vdp1_mesh_stipples`: fill `(0,0)-(3,3)` with CMDPMOD bit 8 → `(0,0)` and `(1,1)` written,
      `(1,0)` and `(0,1)` not.
- [ ] `vdp1_msb_on_ors_existing_pixel`: prefill `(5,5)` with `0x1234`, draw over it with CMDPMOD
      `0x8000` and a non-zero source → `0x9234`, and the source colour is *not* written.
- [ ] `vdp1_gouraud_table_only_fetched_when_bit2_set`: a quad with colour-calc mode 0 and a CMDGRDA
      pointing at a poisoned table renders unshaded.

---

## Phase 7 — Line, Polyline, end codes, flip

Lowest priority for boot: these are effects/vector-art commands, not the blits a logo or menu
needs.

- [ ] **COMM 5 and COMM 7 Polyline** (reference §5.5). Vertices read **directly from VRAM** with
      explicit signed casts (`localX + (s16)[addr+0x0C]` … `[addr+0x1A]`), not through the shared
      command struct. Four edges, each measured greedily first, then drawn with `greedy = 0`,
      `linenumber = 0`, `texturestep = 0` — so `getpixel` always samples texture column 0.
      Gouraud endpoint pairs per edge: A→B `(gA, gB)`, B→C `(gB, gC)`, C→D drawn **D→C** with
      `(gD, gC)`, D→A drawn **A→D** with `(gA, gD)`. COMM 7 is the undocumented mirror.
- [ ] **COMM 6 Line** (reference §5.6). Two vertices, same direct read. Colour from CMDCOLR
      (untextured, `currentShape == 6`).
- [ ] **Do not** port `VIDSoftVdp1LineDraw`'s swapped green/blue gouraud out-parameters
      (reference §5.6, §12 item 10) — it is an identified Yabause bug, not hardware. §10.
- [ ] Both Line and Polyline fetch the gouraud table **unconditionally**, regardless of CMDPMOD bit
      2 (reference §5.5) — unlike `drawQuad`.
- [ ] End codes (reference §6.1), enabled when CMDPMOD bit 7 (ECD) is **clear**, applied only when
      textured:

      | Mode | End code | Action |
      |---|---|---|
      | 0 | `0xF` | signal end code |
      | 1 | `0xF` | signal end code |
      | 2 | `63` | **transparent instead** — the termination is disabled in the reference |
      | 3 | `0xFF` | signal end code — but `read_pattern_128` masks to `0x7F`, so it can **never** match |
      | 4 | `0xFF` | signal end code |
      | 5 | `0x7FFF` | signal end code |

      Two end codes in a span terminate that span. Count an end code only when the texture column
      differs from the previous one, so a magnified texture does not double-count. Reference §6.1
      marks mode 2 and mode 3 **[Ambiguous]** with an explicit "this needs more hardware testing"
      comment — port as documented and reproduce the comment; §10.
- [ ] Character read direction, CMDCTRL bits 5-4 (reference §6.2), applied *before* any fetch:
      1 → `column = characterWidth - column - 1`; 2 → `row = characterHeight - row - 1`;
      3 → both.

**Testing.**
- [ ] `vdp1_line_endpoints`: a horizontal line `(10,20)`-`(20,20)` writes exactly 11 pixels
      inclusive of both endpoints (reference §5.0: the endpoint is always emitted).
- [ ] `vdp1_line_is_not_greedy`: a 45-degree line `(0,0)`-`(4,4)` writes exactly 5 pixels — `DrawLine`
      for COMM 6 passes `greedy = 0` — whereas the same edge used as a *span* inside `draw_quad`
      is greedy. Contrast the two in one test.
- [ ] `vdp1_polyline_draws_four_edges`: a rectangle outline → perimeter written, interior not.
- [ ] `vdp1_polyline_edge_direction`: with a gouraud table whose four corners are distinguishable,
      assert the C→D edge's gradient runs from D's colour to C's (reference §5.5's drawn-direction
      table). This is the only way to catch a transposed edge.
- [ ] `vdp1_end_code_mode_0_terminates_span`: an 8-wide 4bpp character with nibble `0xF` at columns
      3 and 5, ECD clear → columns 0-2 and 4 written, column 6+ not.
- [ ] `vdp1_end_code_disabled_by_ecd`: same character with CMDPMOD bit 7 set → all 8 columns
      written, `0xF` treated as an ordinary index.
- [ ] `vdp1_end_code_mode_3_never_matches`: an 8bpp-128 character containing `0xFF` → masked to
      `0x7F`, drawn as an ordinary index, span not terminated (reference §6.1).
- [ ] `vdp1_end_code_mode_2_is_transparent_not_terminal`: index `63` in mode 2 → that pixel is
      transparent, the span continues.
- [ ] `vdp1_character_flip`: a 16×1 4bpp character with bytes `0x01 0x23 0x45 0x67 0x89 0xAB 0xCD
      0xEF` → nibbles `0..0xF` at columns 0..15. With CMDPMOD bit 7 set (**required** — otherwise
      index `0xF` is an end code in mode 0) and SPD set (otherwise index 0 is transparent):
      unflipped, destination column 0 gets index `0`; with Dir `= 1`, column 0 gets index `0xF`.
      Dir `= 2` on a 1×16 character flips rows; Dir `= 3` flips both.

---

## Phase 8 — 8-bit framebuffer, interlace, and the long tail

- [ ] 8-bit framebuffer mode (TVM0 = 1) end to end: geometry (1024×256 or 512×512), the erase path,
      `putpixel8`, and the CPU port.
- [ ] Double-interlace: `vdp1interlace = (FBCR & 8) ? 2 : 1` (DIE); `CheckDil` rejects even `y` when
      DIL (FBCR bit 2) is set and odd `y` when it is clear, active only when interlaced (reference
      §7.3). The clip test uses the **pre-division** `y` while the framebuffer index uses `y /
      interlace` (reference §7.1 steps 2 and 5) — an easy off-by-one to get wrong.
- [ ] Pre-clip's `y` bound doubles when interlaced (reference §8.4).
- [ ] TVM1 rotation-mode framebuffer readout (reference §2.2): VDP2 addresses the framebuffer
      through a rotation parameter table rather than linearly. This is **VDP2's** readout path —
      coordinate with `implementation-plans/vdp2.md`; VDP1's only obligation is to expose the
      `vdp1width` mask.
- [ ] Leave TVM2 (TVMR bit 2), EOS (FBCR bit 4), HSS (CMDPMOD bit 12) and PCLP (CMDPMOD bit 11)
      **stored but undecoded** — reference §12 items 1-3 say no behavioural decode is determinable
      from the source. Implementing a guess would be worse than the documented no-op. Record them
      in `.development/current_bugs.md` as known unknowns rather than silently omitting them.
- [ ] Colour modes 6 and 7: reference §6/§12 says there is no case, and the effect in Yabause is
      that the previous pixel's values leak through — undefined behaviour, not a mode. Mimas should
      treat them as fully transparent (write nothing) and log once, rather than reproducing
      uninitialised-memory semantics. Document the divergence.

**Testing.**
- [ ] `vdp1_8bit_framebuffer_geometry_and_erase`: TVMR `= 0x0001` → 1024×256×1; erase writes
      `EWDR & 0xFF` one byte per pixel with `w = (EWRR >> 9) * 16`.
- [ ] `vdp1_8bit_only_implements_replace`: colour-calc mode 3 in 8-bit mode behaves as mode 0.
- [ ] `vdp1_dil_rejects_alternate_lines`: FBCR `0x0008` (DIE, DIL clear) → odd `y` rejected;
      FBCR `0x000C` (DIE + DIL) → even `y` rejected.
- [ ] `vdp1_interlace_halves_framebuffer_row`: with DIE set, a quad spanning `y` 0-3 writes
      framebuffer rows 0-1.

---

## Phase 9 — Move VDP1 to Core 2

Only after Phases 0-4 (correct rendering), Phase 2 (two banks), and Phase 3 (Draw End as a real
signal). See AD-1 for why this ordering and not the reverse.

- [ ] Move the command-list executor out of Core 3's body (`lib.rs:216`) into Core 2's
      (`lib.rs:183-195`).
- [ ] Core 2 parks with `LockStepSync::park_while_inactive(2)`, mirroring Core 6's SCU DSP loop
      (`lib.rs:320-341`). This closes half of `CLAUDE.md`'s "only Core 1 and Core 6 truly park"
      debt.
- [ ] The SH-2's PTMR=1 word write calls `sync.set_thread_active(2, true)`, the way SSHON activates
      Core 1 (`sh2.rs:826-838`). PTMR=2 is activated from Core 3's frame-change path instead.
- [ ] Core 2 sets `status = RUNNING` before it starts and `IDLE` when it finishes, so the SH-2's
      EDSR/COPR reads observe genuine in-progress state rather than an atomic all-or-nothing.
- [ ] Core 2 write-locks **only the back bank**; Core 3 read-locks **only the front bank**
      (AD-2). Verify no code path holds both.
- [ ] Core 2 read-locks `vdp1_vram` (Phase 0 already downgraded it from a write lock), so the SH-2
      can keep filling the *next* command list. Reference §1.1 notes Yabause's "delay the frame if
      VRAM was written" heuristic (`vdp1_clock`) — that is an emulator hack for exactly this race
      (§10). Mimas should not port it; the bank split plus the PTMR strobe is the real
      serialisation point.
- [ ] Frame-change ownership stays on **Core 3** (it is a display-timing event, reference §9.2),
      which means the swap and the draw-start-on-frame-change must be a cross-thread handshake, not
      a direct call. Use the same `Condvar` as the PTMR trigger.
- [ ] Update `CLAUDE.md`'s thread table and "Known architecture debt" (Core 2 vs Core 3 entry),
      `docs/mimas_emu_engineering_draft.md` §1.1's divergence list and §4's status paragraph, and
      `docs/mimas-architecture-spec.md`'s preamble note.
- [ ] Decide the fate of the dead `Vdp` struct (`vdp.rs:21-41`) and `DoubleBufferedFramebuffer`
      (`shared_buffers.rs:190-218`). Both are constructed only by `e2e-tests/src/lib.rs:240-241`;
      removing them means editing that test and the `pub use` lines at `lib.rs:16` and `lib.rs:23`.
      Note also that `vdp::Framebuffer` and `shared_buffers::Framebuffer` are **different types with
      the same name**, both re-exported — worth resolving while touching this.

**Testing.**
- [ ] `vdp1_thread_parks_when_idle`: after `start()` with no PTMR write, Core 2 reports inactive.
- [ ] `vdp1_ptmr_write_wakes_core2`: a PTMR=1 write from Core 0 activates Core 2 and the framebuffer
      changes within a bounded wait.
- [ ] `vdp1_core2_and_core3_touch_disjoint_banks`: a long command list on Core 2 concurrent with
      repeated `render_backdrop` on Core 3 completes without either blocking on the other's lock
      (assert via elapsed time or a lock-attempt counter, not by absence of deadlock alone).
- [ ] Re-run the whole `vdp.rs` unit suite against a synchronous entry point that bypasses the
      thread, so Phases 0-8's determinism is preserved. `cargo test --workspace` must stay green
      (`CLAUDE.md`, "Stability constraints").

---

## 10. Deliberately not ported, or ported-with-a-comment

Consolidated so a future session does not "fix" one of these by accident. Every item cites
`docs/hardware-reference/vdp1.md`.

**Not ported (Yabause bugs / host artefacts):**
- `Vdp1Reset`'s `memset(Vdp1Regs, 0, sizeof(Vdp1Regs))` clearing only 8 bytes (§2.5 item 1, §12 item 9).
- `VIDSoftVdp1LineDraw`'s swapped green/blue gouraud out-parameters (§5.6, §12 item 10).
- The `#if 0`'d 32-bit framebuffer read returning 0 (§1, "enable when burning rangers is fixed").
- CPU-port byte/halfword swapping (§1) — a little-endian-host artefact; Mimas stores big-endian.
- `putpixel`'s missing lower bound on the framebuffer index (§7.1 step 3).
- The `T1WriteWord(Vdp1Ram, 0x40000, 0x8000)` Radiant Silvergun terminator (§2.5 item 3, §12 item 12).
- The `vdp1_clock` "delay the frame if VRAM was written" heuristic (§1.1) and the 64-byte-page
  dirty bitmap (§1.1) — both explicitly not hardware.
- The `wait_line_count = LineCount + 50` Draw End delay and its `+10` retry (§2.2, §10.3, §12 item
  13) — explicitly hand-tuned, not a hardware figure.

**Ported, but must carry a comment saying what it is:**
- `FBCR & 3 == 1` treated as one-cycle mode — a *Sonic R* workaround (§2.2, §12 item 11).
- COMM 3 as a Distorted Sprite mirror (*Hardcore 4x4*) and COMM 7/11 as undocumented mirrors (§4.3).
- The "jump to address 0 aborts" guard (§4.4) — an emulator infinite-loop guard; address 0 is a
  legal command table address.
- The runaway command-count cap (§4.2) — Mimas's own guard, with Mimas's own chosen limit.
- The "Batsugun escape" on `EDSR & 2` at loop top (§4.2).
- COMM 7 treated as textured because `7 & 7 == 7` (§3.1, **[Ambiguous]**).
- Scaled-sprite ZP `0xD` (not `0xC`) as lower-left (§5.2, **[Ambiguous]**).
- CMDCOLR used **unshifted** as a bank in modes 0/2/3/4 and shifted `<< 3` only for the mode-1 LUT
  — the debugger and renderer disagree; the renderer is authoritative (§3.4, §12 item 5).
- Colour-calc modes 5/6/7 collapsed into one blend (§7.1, §12 item 6).
- 8-bit framebuffer implementing only colour-calc mode 0 and ignoring MSB-on (§7.2, §12 item 7).
- End codes in modes 2 and 3 (§6.1, §12 item 8) — reproduce the "needs more hardware testing"
  caveat.
- The gouraud sgl-chrome-demo index special case (§7.1).
- The erase path's 8-bit `w` recomputed after the clamp (§9.4).
- The polygon path still running the colour-mode switch (§5.4, **[Ambiguous]**).

**Left undecoded because the source cannot settle them** (§12 items 1-3): TVM2, EOS, HSS, PCLP.
LOPR maintained only on error paths (§2.3, §12 item 14).

---

## 11. Tracking-doc updates this plan implies

Per `CLAUDE.md`, these are updated *as work lands*, not at the end:

- [ ] `.development/current_bugs.md` — currently empty. Seed it with D1-D5 from §0.2 (D1 in
      particular is a live, boot-blocking bug, not a missing feature) and with §10's
      "left undecoded" list as known unknowns.
- [ ] `.development/TASKS.md` / `.development/ROADMAP.md` — currently empty. `.development/
      phased_development_plan.md` marks "Phase 5: Display Composition & Video Subsystems"
      ✅ **Completed**, including "Support drawing primitives: normal sprites, scaled sprites,
      distorted sprites, polygons, polylines, and lines" and "Implement bank swapping (FBCR)".
      §0.3 shows none of that is true. Correct that status line before it misleads another session.
- [ ] `history.md` — add a chapter for the AD-1 (Core 2 timing) and AD-2 (two banks vs `ArcSwap`)
      decisions, since both are the kind of non-obvious choice that reads as accidental later.
- [ ] `CLAUDE.md` — the Core 2/Core 3 debt entry and the thread table, once Phase 9 lands.
- [ ] `docs/mimas_emu_engineering_draft.md` §1.2 (the "one flat 512KB window" note) and §4 (the
      current-status paragraph), once Phase 2 and Phase 4 land respectively.
