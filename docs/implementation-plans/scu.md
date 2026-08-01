# SCU implementation plan

Closes the gap between `docs/hardware-reference/scu.md` (the exhaustive,
Yabause-derived ground truth for the System Control Unit: DMA controller, DSP
co-processor, interrupt controller, timers) and what `saturn-core` actually
executes today.

**How to read this.** Section 1 is a real diff — every register, every DSP DMA
variant, every interrupt source, with its current status and the exact
`file:line` where it does or doesn't exist. Section 2 makes the one
architectural decision this plan can't defer (`scu.rs` is dead code — what
happens to it). Sections 3+ are ordered phases; each phase names exact
registers/opcodes/vectors, points at both the hardware-reference section and
the current code location, and states its own testing strategy and done-signal.

**Conventions.** `§n` references are into `docs/hardware-reference/scu.md`
unless prefixed with a filename. Yabause `file:line` citations are quoted from
the hardware reference rather than re-derived; re-verify against
`../yabause/src/scu.c` before implementing anything whose citation looks
suspect (that's the documented working loop in `CLAUDE.md`).

---

## 1. Current state: reference vs. what actually runs

### 1.1 Where SCU logic lives today

| Concern | Where it actually is | Runs on | Status |
|---|---|---|---|
| SCU DSP interpreter | `saturn-core/src/scu_dsp.rs` (897 lines) | Core 6 (`scu-dma-dsp`), `lib.rs:315-341` | Live, mostly complete (§1.3) |
| DSP register ports `0x80`/`0x84`/`0x88`/`0x8C` | `sh2.rs:645-649` (read_long), `sh2.rs:669-698` (write_long), `sh2.rs:718-744` | Core 0 (inline, under `Arc<Mutex<ScuDsp>>`) | Live, long-access only |
| "SCU DMA" | `Sh2::execute_scu_dma`, `sh2.rs:1536-1639`, triggered from `sh2.rs:684-696` | **Core 0**, synchronously, inline in a CPU memory write | Exists but is substantially wrong (§1.6) |
| Every other SCU register | Plain byte array `work_ram.scu_regs` (`shared_buffers.rs:47`, 4 KiB), read `sh2.rs:465-468`, write `sh2.rs:557-561` | Core 0 | Dead storage — nothing interprets it |
| SCU interrupt controller (IMS/IST/AIACK/queue) | **Nowhere** | — | Absent (§1.5) |
| SCU timers 0/1 | **Nowhere** | — | Absent (§1.7) |
| Interrupt delivery to the SH-2 | Four ad hoc bools in `Sh2`: `vblank_pending` (`sh2.rs:117`), `vblank_out_pending` (`:125`), `smpc_irq_pending` (`:138`), `sound_req_irq` (`:148`); dispatched by `service_pending_interrupt` (`sh2.rs:908-952`) | Core 0 | Live, but bypasses the SCU entirely |
| VBLANK-IN/OUT generation | Wall clock inside `Sh2::run_loop` (`sh2.rs:1688-1706`) | Core 0 | Live, but is a *second* frame clock independent of Core 3's 16.6 ms tick (`lib.rs:207-233`) |
| `saturn-core/src/scu.rs` (`Scu`) | 34-line scaffold | Nothing | **Dead code** — see §2 |

**Confirmed by direct verification, not assumption:**

- There is **no** independent SCU DMA controller. The only DMA-like code is
  `Sh2::execute_scu_dma`, which runs on the CPU thread that wrote `DnEN`, has
  no notion of the three levels beyond a `channel * 0x20` base offset, no
  start factors, no completion interrupt, and no time slicing.
- There is **no** SCU interrupt controller. `grep -riE "\bims\b|\bist\b|aiack"`
  over `saturn-core/` and `e2e-tests/` returns only SH-2/M68K `SR_IMASK_SHIFT`
  hits. `IMS`, `IST`, `AIACK` writes land in `work_ram.scu_regs` and are never
  read by anything.
- There are **no** SCU timers. `grep -riE "timer0|timer1|hblank"` returns one
  comment in `sh2.rs:227` and nothing executable. There is no H-Blank IN event
  anywhere in the codebase, so Timer 0 has no tick source even in principle.
- The DSP End interrupt (`ScuSendDSPEnd`, vector `0x45`) is explicitly not
  raised — `scu_dsp.rs:473-476` says so in a comment.

### 1.2 Register-by-register inventory (diff vs §1.1)

"Byte array" means: the value is stored in `work_ram.scu_regs` and read back
faithfully, but no logic consumes it — functionally identical to a scratchpad.

| Offset | Name | §ref | Today | Gap |
|---|---|---|---|---|
| `0x00` | D0R | §1.2 | Byte array; snapshotted by `execute_scu_dma` (`sh2.rs:1539`) | Used as the *indirect table* pointer, which is wrong (§1.6 D-DMA-2) |
| `0x04` | D0W | §1.2 | Byte array; read at `sh2.rs:1540` | Ignored in indirect mode; it is the descriptor-table pointer on real hardware |
| `0x08` | D0C | §1.2 | Read at `sh2.rs:1541`, masked `0x00FFFFFF` | Wrong clamp; no "0 means 0x100000" rule |
| `0x0C` | D0AD | §1.2 | Bits 2:0 decoded (`sh2.rs:1594-1604`) | Bit 8 (read-add / fill-mode selector) ignored entirely |
| `0x10` | D0EN | §1.2 | Bit 0 triggers (`sh2.rs:684-687`) | No `DnMD[2:0]==7` guard; bit 8 (armed) unimplemented |
| `0x14` | D0MD | §1.2 | Read at `sh2.rs:1543`; indirect tested as bit **16** | Indirect is bit **24** (`0x1000000`); start-factor field 2:0 unimplemented |
| `0x20`-`0x34` | D1R/W/C/AD/EN/MD | §1.2 | Same as level 0 via `channel*0x20` | Same gaps; plus level-1 count clamp `&0xFFF` missing |
| `0x40`-`0x54` | D2R/W/C/AD/EN/MD | §1.2 | Same | Same |
| `0x60` | DSTP | §2.6 | Byte array | Inert in the reference too (deviation #13) — decide deliberately |
| `0x7C` | DSTA | §1.3 | Byte array | Busy bits 4/8/12 never recomputed from engine state |
| `0x80` | PPAF (DSP Program Control Port) | §3.9 | **Implemented** — `scu_dsp.rs:129-144`, masks `0x00FD00FF` read / `0x060380FF` write | Long access only; byte/word reads hit the stale array |
| `0x84` | PPD (Program RAM data) | §3.9 | **Implemented** — `scu_dsp.rs:150-154` | — |
| `0x88` | PDA (Data RAM address) | §3.9 | **Implemented** — `scu_dsp.rs:157-160` | — |
| `0x8C` | PDD (Data RAM data) | §3.9 | **Implemented** — `scu_dsp.rs:164-181` | Mimas re-masks the cursor to 6 bits; the reference does not (deviation #6) — deliberate, see §9 |
| `0x90` | T0C | §5.1 | Byte array | Timer 0 absent |
| `0x94` | T1S | §5.1 | Byte array | Write side effects (`timer1_set=1`, `timer1_preset=val`) absent |
| `0x98` | T1MD | §5.1 | Byte array | Bit 0 (global enable) / bit 7 (mode) absent |
| `0xA0` | IMS | §4 | Byte array | Interrupt mask absent; write must run the queue drain |
| `0xA4` | IST | §4.4 | Byte array (plain store) | Must be `IST &= val` on write, and must latch masked interrupts |
| `0xA7` | IST low byte | §1.4 | Plain byte store | Must be `IST &= (0xFFFFFF00 \| val)` + drain |
| `0xA8` | AIACK | §4.5 | Byte array | External-interrupt gate absent |
| `0xB0`/`0xB4` | ASR0/ASR1 | §1.1 | Byte array | Stored-never-used in the reference too — parity is fine |
| `0xB8` | AREF | §1.1 | Byte array | Same |
| `0xC4` | RSEL | §1.1 | Byte array | Same |
| `0xC8` | VER | §1.1 | Byte array (reads 0) | Must read `0x04` |

Also missing at the access level:

- **Mirroring.** §1.1: every accessor masks the address with `0xFF`, so the
  0x100-byte file mirrors 256× across `0x05FE0000-0x05FEFFFF`. Mimas masks
  with `0xFFF` (`sh2.rs:466`, 4 KiB array), and `write_long`'s SCU path
  silently drops writes with `off + 3 >= 0x1000` (`sh2.rs:677`). A BIOS access
  through any mirror above `0x05FE0100` behaves differently from the reference.
- **Reset values.** §0.1's table (`DnAD=0x101`, `DnMD=0x7`, `IMS=0xBFFF`,
  `VER=0x04`, …) is not applied anywhere — the byte array starts all-zero.

### 1.3 DSP instruction-group inventory (§3.2-§3.12)

| Group | §ref | Status | Location |
|---|---|---|---|
| Class `00` Operation Commands (bit layout, exec order) | §3.4 | Implemented, field extraction matches | `scu_dsp.rs:384-414` |
| ALU ops `0x0`-`0x6`, `0x8`-`0xB`, `0xF` | §3.3 | Implemented | `scu_dsp.rs:302-373` |
| ALU ops `0x7`, `0xC`, `0xD`, `0xE` | §3.3 | Absent in Mimas **and** in the reference (deviation #9) | parity — leave |
| Source select `readgensrc` `0x0`-`0xA` | §3.5 | Implemented | `scu_dsp.rs:194-209` |
| Deferred `incFlg` CT-increment model | §3.5 | **Not implemented** — Mimas increments inline (defect D-DSP-2) | `scu_dsp.rs:198-199` |
| D1-bus dest `writed1busdest` `0x0`-`0xF` | §3.6 | Implemented | `scu_dsp.rs:212-230` |
| Class `10` MVI, both forms, all 10 condition codes | §3.7 | Implemented | `scu_dsp.rs:418-448` |
| `writeloadimdest` `0x0`-`0x7`, `0xA`, `0xC` | §3.7.1 | Implemented except the `MC0`-`MC3` CT increment (defect D-DSP-3) | `scu_dsp.rs:235-253` |
| Class `11`/`01` JMP, all 11 condition codes + pending guard | §3.10 | Implemented | `scu_dsp.rs:485-513` |
| Delay slot / `jmpaddr` sentinel | §3.10 | Implemented (`Option<u8>` instead of `0xFFFFFFFF`) | `scu_dsp.rs:290-297` |
| Class `11`/`10` LPS / BTM | §3.11 | Implemented | `scu_dsp.rs:456-468` |
| Class `11`/`11` END / ENDI | §3.12 | Implemented except `P = PC+1` and `ScuSendDSPEnd` | `scu_dsp.rs:469-479` |
| Class `11`/`00` DSP DMA | §3.8 | 2 of 8 variants — see §1.4 | `scu_dsp.rs:517-648` |
| `T0` flag + two-step `dsp_dma_wait` deferral | §3.8.7 | Implemented | `scu_dsp.rs:524, 546, 555-562` |
| DMA force-completion from `readgensrc`/`writed1busdest`/`writeloadimdest` | §3.8.7 | **Not implemented** (defect D-DSP-4) | — |

### 1.4 DSP DMA addressing-mode variants (§3.8.2) — the already-scoped gap

`scu_dsp.rs`'s own module doc comment (`scu_dsp.rs:19-25`) states this scope
precisely. Confirmed by reading `step_dma` (`scu_dsp.rs:563-575`):

| Variant | Dispatch test (§3.8.2) | H | CS | DIR | Form | Mimas |
|---|---|---|---|---|---|---|
| `dsp_dma01` | `(i>>10)&0x1F == 0x00` | 0 | imm | read | `DMA D0, MCn, #imm` | **missing** |
| `dsp_dma02` | `(i>>10)&0x1F == 0x04` | 0 | imm | write | `DMA MCn, D0, #imm` | **missing** |
| `dsp_dma03` | `(i>>11)&0x0F == 0x04` | 0 | RAM | read | `DMA D0, {MCn\|PRG}, [s]` | implemented — `scu_dsp.rs:583-603` |
| `dsp_dma04` | `(i>>10)&0x1F == 0x0C` | 0 | RAM | write | `DMA MCn, D0, [s]` | implemented — `scu_dsp.rs:610-648` |
| `dsp_dma05` | `(i>>11)&0x0F == 0x08` | 1 | imm | read | `DMAH D0, MCn, #imm` | **missing** |
| `dsp_dma06` | `(i>>10)&0x1F == 0x14` | 1 | imm | write | `DMAH MCn, D0, #imm` | **missing** |
| `dsp_dma07` | `(i>>11)&0x0F == 0x0C` | 1 | RAM | read | `DMAH D0, {MCn\|PRG}, [s]` | **missing** |
| `dsp_dma08` | `(i>>10)&0x1F == 0x1C` | 1 | RAM | write | `DMAH MCn, D0, [s]` | **missing** |

The transfer-count computation at issue time (§3.8.3) *does* already cover all
eight dispatch shapes (`scu_dsp.rs:526-545`) — only the execution handlers are
missing. `RA0M`/`WA0M` are already seeded at issue (`scu_dsp.rs:547-548`), so
the four `H` (hold) wrappers have everything they need.

### 1.5 Interrupt-source inventory (§4.1)

Thirty sources in the reference. Mimas delivers four of them, none through an
SCU:

| Source | Vector | Level | IMS bit | IST bit | DMA factor | Mimas today |
|---|---|---|---|---|---|---|
| V-Blank IN | `0x40` | 15 | `0x0001` | b0 | 0 | `Sh2::vblank_pending`, self-timed on Core 0 (`sh2.rs:193-194, 1688-1698`) |
| V-Blank OUT | `0x41` | 14 | `0x0002` | b1 | 1 | `Sh2::vblank_out_pending` (`sh2.rs:210-211, 1700-1706`) |
| H-Blank IN | `0x42` | 13 | `0x0004` | b2 | 2 | **absent** |
| Timer 0 | `0x43` | 12 | `0x0008` | b3 | 3 | **absent** |
| Timer 1 | `0x44` | 11 | `0x0010` | b4 | 4 | **absent** |
| DSP End | `0x45` | 10 | `0x0020` | b5 | — | **absent** (`scu_dsp.rs:473-476` notes it) |
| Sound Request | `0x46` | 9 | `0x0040` | b6 | 5 | `Sh2::sound_req_irq` atomic bool (`sh2.rs:218-222`) |
| System Manager (SMPC) | `0x47` | 8 | `0x0080` | b7 | — | `Sh2::smpc_irq_pending` (`sh2.rs:212-217`) |
| Pad Interrupt | `0x48` | 8 | `0x0100` | b8 | — | **absent** |
| Level 2 DMA End | `0x49` | 6 | `0x0200` | b9 | — | **absent** |
| Level 1 DMA End | `0x4A` | 6 | `0x0400` | b10 | — | **absent** |
| Level 0 DMA End | `0x4B` | 5 | `0x0800` | b11 | — | **absent** |
| DMA Illegal | `0x4C` | 3 | `0x1000` | b12 | — | **absent** (never raised in the reference either, #14) |
| Sprite Draw End | `0x4D` | 2 | `0x2000` | b13 | 6 | **absent** |
| External 00-03 | `0x50`-`0x53` | 7 | `0x8000` | b16-b19 | — | **absent** |
| External 04-07 | `0x54`-`0x57` | 4 | `0x8000` | b20-b23 | — | **absent** |
| External 08-15 | `0x58`-`0x5F` | 1 | `0x8000` | b24-b31 | — | **absent** |

Also absent: the mask/latch semantics themselves (§4.2 — `IST` latches only
*masked* interrupts), the 30-entry pending queue with dedupe-by-vector and
level sort (§4.3), the `ScuTestInterruptMask` drain (one delivery per
`IMS`/`IST`/`AIACK` write), the `AIACK` gate (§4.5), and the slave-SH-2 vector
mirrors (`0x40`→`0x43` level 2, `0x42`→`0x41` level 1, §4.2).

### 1.6 Defects in what already exists

These are *wrong*, not merely *missing* — they need fixing inside the phases
below rather than being left in place under new code.

**SCU DMA (`Sh2::execute_scu_dma`, `sh2.rs:1536-1639`):**

- **D-DMA-1** — indirect mode is detected as `mode & 0x0001_0000` (bit 16,
  `sh2.rs:1546`). §1.2 says bit **24** (`0x1000000`). Any real indirect DMA is
  mis-classified as direct and vice versa.
- **D-DMA-2** — the descriptor table is read from `read_addr` (`D0R`,
  `sh2.rs:1551`). §2.5: the table pointer is `DnW`.
- **D-DMA-3** — the end-of-list marker is tested as bit 31 of the *count* word
  at `+0x0` (`sh2.rs:1564`). §2.5: it is bit 31 of the *source address* at
  `+0x8`. The loop also breaks on `size == 0` (`sh2.rs:1561`), which has no
  basis in the reference.
- **D-DMA-4** — the count is masked `0x00FFFFFF` (`sh2.rs:1541`). §1.2: level 0
  keeps all 32 bits and `0` means `0x100000`; levels 1/2 clamp to `0xFFF` and
  `0` means `0x1000`; indirect skips the clamp entirely.
- **D-DMA-5** — `DnAD` bit 8 is never read, so **fill mode does not exist**
  (§2.4). Every transfer is a byte-pair copy.
- **D-DMA-6** — no B-Bus split (§2.4): the reference does two 16-bit writes for
  B-Bus destinations and 32-bit accesses otherwise; Mimas always does 8-bit
  pairs, and the source always advances by 2 regardless of bus class.
- **D-DMA-7** — the trigger ignores `DnMD[2:0] == 7` (`sh2.rs:684-696`), so a
  factor-configured level starts immediately on any `DnEN` bit-0 write.
- **D-DMA-8** — no completion interrupt, no `DSTA` update, no re-trigger-while-
  busy flush (§2.2), no `SH2WriteNotify` equivalent.
- **D-DMA-9** — architectural: the whole transfer runs on Core 0 inside a
  single `write_long`, holding `BusArbiter::lock_for_dma` for its entire
  duration (`sh2.rs:1548`, `:1631`). A 1 MiB level-0 transfer stalls Core 1 for
  the whole copy and stalls Core 0 by construction. Core 6 — the thread named
  `scu-dma-dsp` — is not involved at all.

**SCU DSP (`scu_dsp.rs`):**

- **D-DSP-1 (highest value)** — the ALU switch masks to 4 bits:
  `let op = (instruction >> 26) & 0xF;` (`scu_dsp.rs:303`). The reference
  switches on the *unmasked* `instruction >> 26` (§3.3), which is ≥ `0x10` for
  classes `01`/`10`/`11` and therefore falls to `default` — **no ALU op runs
  for non-Operation instructions.** Under Mimas's mask, every JMP
  (`0xD…`, `>>26` = `0x34`-`0x37` → `&0xF` = `4`-`7`) executes a phantom
  ADD/SUB/AD2, every LPS/BTM (`0xE…` → `8`-`0xB`) executes a phantom
  SR/RR/SL/RL, and every MVI's *destination* field doubles as an ALU opcode.
  All of these clobber `ALU` and the `Z`/`S`/`C` flags that the *next*
  conditional jump reads. The real captured BIOS program contains
  `0xd308001f`, `0xd3400015`, `0xd3400018`, `0xd340001b`, `0xd0000003`
  (`scu_dsp.rs:872-879`) — every one of them currently runs a phantom ADD.
- **D-DSP-2** — `readgensrc`'s CT post-increment is applied inline
  (`scu_dsp.rs:198-199`) instead of via the deferred `incFlg` model (§3.5:
  `incFlg[num & 3] |= (num >> 2) & 1`, applied after the instruction body).
  Consequence: an instruction that reads `MCn` on the X bus and again on the Y
  bus reads two *different* Data RAM words in Mimas and the *same* word in the
  reference; an instruction that reads `MCn` and writes `MCn` advances `CT[n]`
  twice instead of once. `write_d1_bus_dest` has the same inline-increment
  shape (`scu_dsp.rs:214-217`), and its `CT0`-`CT3` arms (`0xC`-`0xF`) do not
  clear the pending flag the way §3.6 requires.
- **D-DSP-3** — `write_load_im_dest` arms `0x0`-`0x3` write `MD[n][CT[n]]`
  without incrementing `CT[n]` (`scu_dsp.rs:237-240`). §3.7.1 sets `incFlg[n]`.
  MVI-to-Data-RAM is the single most common DSP idiom, so this silently
  corrupts any program that streams immediates into a page.
- **D-DSP-4** — no force-completion of a pending DSP DMA from
  `readgensrc`/`writed1busdest`/`writeloadimdest` (§3.8.7). `start_dma` sets
  `dsp_dma_wait = 0` on a back-to-back DMA (`scu_dsp.rs:520-522`) but never
  *executes* the pending transfer, so the first one is dropped.
- **D-DSP-5** — `END`/`ENDI` write `P = PC` (`scu_dsp.rs:478`); §3.12 writes
  `P = PC + 1` (the increment at `:1954` has not happened yet at that point).
  BIOS code that reads the Program Control Port after completion sees an
  off-by-one program address.
- **D-DSP-6** — the DSP DMA memory window is too narrow. `write_word`
  (`scu_dsp.rs:696-705`) handles only Low WRAM, Sound RAM and High WRAM — but
  it is the function the **B-Bus path** calls, and the B-Bus range
  (`0x05A00000`-`0x06000000`, §3.8.6) contains VDP1 VRAM/framebuffer/registers,
  VDP2 VRAM, CRAM and VDP2 registers. **Every DSP DMA to a VDP target is
  silently discarded today.** `read_long`/`write_long` (`scu_dsp.rs:664-694`)
  likewise omit VDP1 VRAM (`0x05C00000`), VDP1 framebuffer (`0x05C80000`),
  VDP1 regs (`0x05D00000`), SCSP regs (`0x05B00000`), VDP2 regs (`0x05F80000`),
  CS2 (`0x05800000`) and the A-Bus/cartridge space.
- **D-DSP-7** — `dma_read_from_main_ram` always writes `RA0` back
  (`scu_dsp.rs:602`); §3.8.5 does so only on the non-A-Bus branch (quirk #5).
  Minor, but it is exactly the kind of thing the hold variants interact with.
- **D-DSP-8** — DSP DMA takes no bus lock at all, while `Sh2::execute_scu_dma`
  takes one. Two different DMA engines with two different bus policies.

### 1.7 Timers (§5)

Nothing exists: no `T0C`/`T1S`/`T1MD` interpretation, no `timer0` scanline
counter, no `timer1_counter` down-counter, no `timer0_set`/`timer1_set`
handshake, and — the blocking prerequisite — **no H-Blank IN event in the
emulator at all**, so Timer 0 has no clock and Timer 1 has no reload trigger.

---

## 2. Architectural decision: repurpose `scu.rs`, don't extend `scu_dsp.rs`

**`saturn-core/src/scu.rs` is dead code.** Its `Scu` struct (`dma_active`,
`dsp_pc`, `dsp_program`) is exported from `lib.rs:20` and constructed only by
two tests — `e2e-tests/src/lib.rs:229` (`test_tier1_f4_scu_initialization`) and
`:579` (`test_tier2_f4_scu_dma_channel_bounds`). `SaturnSystem` never touches
it. Its two methods are scaffold guesses that contradict the reference:
`start_dma(channel) -> Result` has no hardware analogue (the register map
simply doesn't decode a fourth level, §1.1), and `run_dsp_instruction` is an
empty stub whose real implementation already exists elsewhere.

**Decision: repurpose `scu.rs` as the real SCU, keep `scu_dsp.rs` as its DSP
submodule.** Concretely:

- Replace `scu.rs`'s contents wholesale. The new `Scu` owns the register file,
  the three DMA levels, the interrupt controller, the two timers, and a
  `ScuDsp`.
- `scu_dsp.rs` stays exactly where it is, as the DSP-only module, reached as
  `Scu::dsp`. It does not absorb DMA/IRQ/timer logic.
- Delete `Scu::start_dma` and `Scu::run_dsp_instruction` outright.

**Why not the alternatives:**

- *Delete `scu.rs` and grow everything inside `scu_dsp.rs`* — would produce one
  ~2500-line module named after one of the SCU's four functions, and would
  break the project's 1:1 mapping between `docs/hardware-reference/<block>.md`
  and `saturn-core/src/<block>.rs`. `scu_dsp.rs` is currently coherent and
  well-tested; keep it that way.
- *Leave `scu.rs` dead and add a third module* — leaves a known-dead export in
  `lib.rs` that the next session has to re-discover (`CLAUDE.md`'s "Known
  architecture debt" already flags it once).
- *Keep the DMA engine inside `sh2.rs`* — it is not CPU logic. It is on the
  wrong thread (§1.6 D-DMA-9) and it is the reason there is no place to hang
  start factors, `DSTA`, or completion interrupts.

**Lock granularity — mirror `WorkRam`'s per-region design, not one big mutex.**
Core 0 touches SCU registers on *every* SCU access and on every interrupt
service; Core 6 holds DSP/DMA state for long stretches. One `Mutex<Scu>` would
serialize those against each other. Structure it as:

```
pub struct Scu {
    regs:   Mutex<ScuRegisters>,   // the memory-mapped register file
    irq:    Mutex<ScuIrq>,         // IMS / IST / AIACK / pending queue
    dma:    Mutex<[DmaLevel; 3]>,  // scudmainfo_struct working copies (§2.1)
    timers: Mutex<ScuTimers>,
    pub dsp: Mutex<ScuDsp>,        // unchanged; already Arc<Mutex<..>>-shaped
}
```

`SaturnSystem::scu_dsp: Arc<Mutex<ScuDsp>>` (`lib.rs:73`) becomes
`SaturnSystem::scu: Arc<Scu>`; `Sh2::scu_dsp` (`sh2.rs:163`) becomes
`Sh2::scu: Option<Arc<Scu>>`, set through a field assignment in
`SaturnSystem::start` exactly like `scu_dsp` is today (`lib.rs:151`) —
**`Sh2::new()`'s 3-argument signature must not change** (`CLAUDE.md`,
"Stability constraints").

**Lock-ordering rule this introduces** (`CLAUDE.md` notes no call site needs
two `WorkRam` locks today; the DMA engine will need an `Scu` lock *and* a
`WorkRam` lock): **always `Scu` before `WorkRam`, never the reverse**, and
never hold an `Scu` lock across a whole DMA burst. §2.1 makes this natural: the
hardware itself snapshots `DnR/DnW/DnC/DnAD/DnMD` into a working copy at
trigger time and never writes them back — so Core 6 takes `regs` briefly,
copies into its `DmaLevel`, drops the lock, and then moves bytes holding only
`WorkRam` region locks.

---

## 3. Phase ordering

```
Phase 1  DSP: 6 missing DMA variants + 8 interpreter defects   (self-contained)
Phase 2  Real SCU register file; repurpose scu.rs              (scaffolding)
Phase 3  Interrupt controller                                  (unblocks 4 & 5)
Phase 4  Independent DMA controller on Core 6                  (retires sh2.rs's)
Phase 5  Timers 0/1 + H-Blank IN source
Phase 6  Start factors, DSP End, Draw End — closing the loop
```

**Phases 3 and 4 are deliberately swapped** relative to the obvious
"DMA-then-interrupts" order:

1. The DMA controller cannot be *finished* without the interrupt controller —
   DMA-end vectors `0x49`/`0x4A`/`0x4B` (§2.8) and start factors 0-6 (§2.3)
   both go through it. Building DMA first forces either a stub or a second ad
   hoc interrupt path, which is exactly the failure mode this plan is supposed
   to avoid.
2. Today's DMA path is wrong but *does move bytes* for immediate-mode
   triggers. Today's interrupt path ignores `IMS`/`IST` completely — a BIOS
   masking, unmasking, or acknowledging through those registers is talking to
   dead storage. The absolute gap is larger on the interrupt side.
3. Phase 3 is a pure state machine (no bus arbitration, no cross-thread
   hazards). Phase 4 is the riskiest phase in this document. Sequencing the
   safe one first while Phase 2's scaffolding settles is cheaper.

**The one measurement that can reorder 3 and 4.** Before starting Phase 3, run
the existing diagnostic (`CLAUDE.md`, "Diagnostic recipes") and grep the
output:

```bash
MIMAS_BOOT_WATCH_SECS=280 ./target/release/saturn-frontend-native --bios <bios.bin> 2>&1 \
  | grep '\[REGACCESS\]' | grep -i scu
```

- If SCU offsets `0xA0`/`0xA4`/`0xA7` dominate → Phase 3 first, as planned.
- If `0x10`/`0x30`/`0x50` (`DnEN`) and the `DnR`/`DnW`/`DnC` triples dominate,
  or the stall at `0x060131A8` (`.development/current_blocker.md`) turns out to
  be waiting on a `DSTA` busy bit → **swap 3 and 4**, and land only the minimum
  interrupt plumbing Phase 4 needs (the `ScuIrq::send` entry point plus the
  three DMA-end sources) as a Phase 4 prerequisite, rather than the full
  controller.

Phases 1 and 2 are unaffected by that measurement — start them regardless.

---

## Phase 1 — Finish the SCU DSP

**Status:** done except one testing item explicitly deferred below — see
`history.md` Chapter 26.

Smallest, highest-value, already-scoped gap (`scu_dsp.rs:19-25`). Entirely
inside `scu_dsp.rs`; touches no threading and no other module.

### 1a. Interpreter defects (do these first — the DMA variants are built on top)

- [x] **D-DSP-1**: guard the ALU switch to Operation Commands only. Match §3.3
      exactly: switch on the unmasked `instruction >> 26` so classes
      `01`/`10`/`11` (values ≥ `0x10`) fall through with no ALU effect. Fixes
      phantom ALU ops on every JMP/LPS/BTM/MVI. `scu_dsp.rs:303`.
- [x] **D-DSP-2**: adopt the deferred `incFlg` model (§3.5). Add
      `inc_flg: [bool; 4]`, cleared at the top of `step()`
      (reference `:1384-1387`), OR-ed by `read_gen_src` for `num` `0x4`-`0x7`,
      set by `write_d1_bus_dest` arms `0x0`-`0x3`, **cleared** by arms
      `0xC`-`0xF` (§3.6), applied after the instruction body
      (reference `:1949-1952`) — plus the one early-apply case: `MOV SImm,[d]`
      applies pending increments *before* the store (§3.5, reference
      `:1691-1694`). `scu_dsp.rs:194-230, 259-298`.
- [x] **D-DSP-3**: `write_load_im_dest` arms `0x0`-`0x3` must set `inc_flg[n]`
      (§3.7.1). `scu_dsp.rs:237-240`.
- [x] **D-DSP-4**: force-complete a pending DSP DMA on entry to `read_gen_src`,
      `write_d1_bus_dest` and `write_load_im_dest` — set `dsp_dma_wait = 0`
      then run the dispatch, per §3.8.7. Also make `start_dma`'s
      "previous DMA still pending" path actually execute it before latching the
      new instruction (§3.8.7 step 1). `scu_dsp.rs:517-522`.
- [x] **D-DSP-5**: `END`/`ENDI` writes `P = PC + 1` (§3.12). `scu_dsp.rs:478`.
      **Clarification**: this is the Program Control Port's `P` field (bits
      7:0, the PC-reload/readback snapshot), not the DSP's arithmetic `P`
      accumulator register (`ScuDsp->P`/`self.p`) — same name, two distinct
      pieces of state in real hardware. Confirmed via `scu.c:1932`
      (`ScuDsp->ProgControlPort.part.P = ScuDsp->PC+1;`).
- [x] **D-DSP-6**: widen `read_long`/`write_long`/`write_word`
      (`scu_dsp.rs:664-705`) to the full set of regions `Sh2::translate`
      (`sh2.rs:355-380`) decodes: add SCSP registers `0x05B00000`, VDP1 VRAM
      `0x05C00000`, VDP1 framebuffer `0x05C80000`, VDP1 registers
      `0x05D00000`, VDP2 registers `0x05F80000`, CS2 `0x05800000`, and give
      `write_word` the same coverage as `write_long` (it currently has three
      regions to `write_long`'s five). Note in the doc comment that unmapped
      A-Bus/cartridge space still reads `0` / discards.
      **Preferred**: factor the region decode into one shared helper rather
      than three parallel `if` chains, and re-state in its doc comment that
      `sh2.rs`'s `translate` remains the source of truth. Done as a
      `decode(address) -> Option<(DspRegion, usize)>` helper.
- [x] **D-DSP-7**: apply §3.8.5's A-Bus branch rule — `dsp_dma03` writes back
      `RA0` only on the non-A-Bus path (`abus_check = (RA0M << 2) & 0x0FF00000`,
      A-Bus iff `0x02000000 ≤ abus_check < 0x05900000`). `scu_dsp.rs:602`.

### 1b. The six missing DMA addressing-mode variants (§3.8)

Dispatch order matters — reproduce §3.8.2's if-else chain in the reference's
order so overlapping tests resolve identically. Encodings that match none of
the eight (bit 11 set, or bit 10 set on variants 01/02/04/06/08) must still
clear `T0`/`dsp_dma_instruction`/`dsp_dma_wait` and move no data (§3.8.2
quirk / deviation #2).

- [x] **`dsp_dma01`** — non-hold, immediate count, D0-bus → `MD[sel]`,
      `sel = (inst >> 8) & 0x03`. Per-iteration: read long at `RA0M << 2`,
      store to `MD[sel][CT[sel] & 0x3F]`, `CT[sel]` post-increment,
      `RA0M += add >> 2` where `add = (1 << (mode & 0x2)) & ~1` — i.e.
      **instruction bit 16**, not bit 15 (§3.8.4, deviation #1). Afterwards
      `T0 = 0`, `RA0 = RA0M`. Count from `inst & 0xFF`, re-derived in the
      handler (§3.8.3), *not* from `dsp_dma_size`.
- [x] **`dsp_dma02`** — non-hold, immediate count, `MD[sel]` → D0-bus, through
      the shared write path below. Count from `inst & 0xFF`.
- [x] **`dsp_dma05`** — `dsp_dma01` wrapped: save `RA0M`, run `dsp_dma01`,
      restore `RA0 = saved`. Note §3.8.2's quirk: this variant accepts
      `RAMsel == 4` but `dsp_dma01` masks `sel` to 2 bits, so `PRG` degrades to
      `MD0` (deviation #3) — reproduce it, and say so in the code comment.
- [x] **`dsp_dma06`** — `dsp_dma02` wrapped, restoring `WA0`.
- [x] **`dsp_dma07`** — `dsp_dma03` wrapped, restoring `RA0`.
- [x] **`dsp_dma08`** — `dsp_dma04` wrapped, restoring `WA0`.
- [x] Extract the existing write body (`scu_dsp.rs:610-648`) into a shared
      `dsp_dma_write_d0bus(sel, add, count)` matching §3.8.6, so `dsp_dma02`
      and `dsp_dma04` share it exactly as the reference does. Preserve all
      three destination classes and their differing `add` fixups: A-Bus
      (`0x02000000..0x05A00000`, `add` clamped to 1, `WA0M += add`), B-Bus
      (`0x05A00000..0x06000000`, `add = 1` if 0, two 16-bit writes,
      `Adr += add << 2` per iteration and `WA0M += add * count` **once at the
      end**), CPU bus (everything else, `WA0M += 1` if `add == 1` else
      `add >> 1`, and the write is redirected into High WRAM with mask
      `0xFFFFC` — deviation #4, already reproduced at `scu_dsp.rs:641-642`).
- [x] Keep the write-side `add` table (bits 17:15 → 0/1/2/4/8/16/32/64
      long-words, §3.8.4) distinct from the read-side single-bit rule. They are
      *different decodes of the same field*; a shared helper here would be a
      bug. Done as separate `dma_read_add`/`dma_write_add` helpers.

### 1c. Testing (Phase 1)

Independently-derived values only (`CLAUDE.md`: never assert a value you
haven't derived separately from the code under test).

- [x] Per variant, one test whose expected result is computed **outside**
      Mimas: write a throwaway Python script that walks §3.8.4's `add` table
      and §3.8.6's three destination classes to produce the exact
      (address, value) list for a chosen instruction word, seed `WorkRam` /
      `MD[]` with a distinguishable pattern (e.g. `0xA5A50000 | index`), run
      the DSP, and assert the full expected image. Keep the script's output as
      a literal table in the test, and cite the instruction word's field
      decomposition in a comment.
- [x] **Hold-variant test** (`dsp_dma05`-`08`): assert two things at once —
      the destination image advanced exactly as the non-hold variant's, *and*
      `RA0`/`WA0` read back their pre-transfer values. A test that only checks
      the register would pass against a no-op implementation.
- [x] **D-DSP-1 regression**: a two-instruction program — one that sets `Z`/`S`
      to a known state, then a `JMP` word such as `0xD3400015` — asserting the
      flags are *unchanged* by the JMP. Derive the expected flags by hand from
      §3.3, and add a comment naming the phantom-ADD bug this guards.
- [x] **D-DSP-2/3 regression**: a program that reads `MC0` on both X and Y
      buses in one instruction, asserting both reads returned the *same* Data
      RAM word and `CT[0]` advanced by exactly 1; and an `MVI …,MC0` sequence
      of three immediates asserting they land in `MD[0][0..3]`.
- [x] **D-DSP-6 regression**: a DSP DMA writing into VDP2 VRAM, asserting the
      bytes actually appear in `work_ram.vdp2_vram`. This fails today.
- [ ] **Strengthen the existing anchor test.** `real_bios_dsp_program_runs_to_
      completion` (`scu_dsp.rs:862-896`) currently only asserts termination.
      Hand-trace the 32-word program offline (a Python model of §3.3-§3.12,
      seeded with the same `MD[0][0..3] = [0, 0x09694000, 0x000002AB]`) and
      extend the test to assert the final `MD[]` contents, `RA0`/`WA0`, and the
      Program Control Port's flag bits. This is the single strongest signal the
      D-DSP-1/2/3 fixes are right, because that program is real BIOS code.
      **Deliberately deferred**: decoding the program (`history.md` Chapter 26)
      showed it exercises conditional `MVI`, conditional `JMP` with the
      delayed-branch/loop-back timing, and `dsp_dma03`/`dsp_dma04` — a fully
      independent Python re-implementation of all of that (not just the
      simpler ALU/DMA-variant mechanics the other new tests cover) is real,
      separately-scoped work with its own real risk of being a self-
      consistent-but-wrong replica rather than genuine independent
      verification. The D-DSP-1/2/3 mechanisms it would exercise are already
      covered by dedicated regression tests above; the anchor test itself
      still passes (real BIOS program still reaches End, not stuck).
- [x] `cargo test --workspace` green; `cargo fmt`.

**Done signal**: all eight variants dispatch, the anchor test asserts real
state (not just termination), and `scu_dsp.rs`'s module doc comment
(`:19-25`) is rewritten — it currently advertises the 2-of-8 gap.
**Partial**: all eight variants dispatch and the module doc comment is
rewritten; the anchor test's *termination* assertion is unchanged (see the
deferred item above) rather than strengthened to assert final state.

---

## Phase 2 — Real SCU register file (repurpose `scu.rs`)

Pure scaffolding: no new behavior beyond exact register semantics. Everything
after this phase plugs into it.

- [ ] Replace `saturn-core/src/scu.rs` wholesale per §2 of this document. New
      `Scu` with `regs`/`irq`/`dma`/`timers`/`dsp` sub-locks; delete
      `start_dma` and `run_dsp_instruction`.
- [ ] `ScuRegisters`: typed fields for every offset in §1.1's table —
      `d0r d0w d0c d0ad d0en d0md`, `d1*`, `d2*`, `dstp`, `dsta`, `t0c`, `t1s`,
      `t1md`, `ims`, `ist`, `aiack`, `asr0`, `asr1`, `aref`, `rsel`, `ver`.
- [ ] Apply §0.1's reset table in `Scu::new()`/`reset()`: `DnAD = 0x101`,
      `DnEN = 0`, `DnMD = 0x7`, `DSTP = 0`, `DSTA = 0`, DSP
      `ProgControlPort = 0`, `PDA = 0`, `T1MD = 0`, `IMS = 0xBFFF`, `IST = 0`,
      `AIACK = 0`, `ASR0/1 = 0`, `AREF = 0`, `RSEL = 0`, `VER = 0x04`, timers
      and DMA working copies zeroed. Note §0.1's explicit exception: reset does
      **not** clear DSP `ProgramRam`, `MD[][]`, `PC`, `CT[]`, `RA0`/`WA0` or
      `jmpaddr`.
- [ ] Address decode: mask the offset with `0xFF` (§1.1) so the 256-byte file
      mirrors across the whole 64 KiB page. This replaces `sh2.rs:466`'s
      `off & 0xFFF` and removes the silent-drop at `sh2.rs:677`.
- [ ] Long read: implement exactly the R column of §1.1 — `D0R`/`D0W`/`D0C`
      (`0x00`/`0x04`/`0x08`) and their level-1/2 equivalents, `DSTA` (`0x7C`,
      with the live busy-bit recompute of §1.3), `0x80` and `0x8C` (already in
      `ScuDsp`), `IST` (`0xA4`), `AIACK` (`0xA8`), `RSEL` (`0xC4`),
      `VER` (`0xC8` → `0x04`).
- [ ] Long write: implement the W column — all `DnR/W/C/AD/EN/MD`, `DSTP`,
      `DSTA`, the four DSP ports, `T0C`/`T1S`/`T1MD`, `IMS`, `IST`, `AIACK`,
      `ASR0`/`ASR1`/`AREF`, `RSEL`.
- [ ] Byte access: `0xA7` read/write per §1.4. Every other byte offset →
      log once via the existing `log_reg_access_once` machinery
      (`sh2.rs:242-257`) and fall through to a raw byte fallback rather than
      the reference's hard "unhandled, return 0" — see §9, deviation note 2.
- [ ] Word access: §1.1 quirk / deviation #20 says all 16-bit SCU accesses are
      no-ops in the reference. That is tagged **[QUIRK]** (an emulator
      shortcut), not hardware behavior, so **do not copy it** — keep 16-bit
      access as two byte accesses over the modelled register file and log it
      once. Revisit only if a real BIOS trace shows a 16-bit SCU access whose
      behavior matters.
- [ ] Route `MemRegion::ScuRegs` in `sh2.rs` (read `:465-468`, write
      `:557-561`, long read `:645-649`, long write `:669-698`) through
      `Scu` instead of `work_ram.scu_regs`. Keep the existing DSP-port
      early-return shape; it already works.
- [ ] Retire `WorkRam::scu_regs` (`shared_buffers.rs:47`, `:80`) once nothing
      reads it. Check `vdp.rs` and the frontends first.
- [ ] `Sh2::scu: Option<Arc<Scu>>` replaces `Sh2::scu_dsp` (`sh2.rs:163`);
      `SaturnSystem::scu: Arc<Scu>` replaces `scu_dsp` (`lib.rs:73`, `:102`,
      `:139`, `:151`, `:319`). **Do not change `Sh2::new()`'s signature.**
- [ ] Rewrite the two dead-`Scu` tests instead of deleting them silently:
      `e2e-tests/src/lib.rs:228` becomes a real assertion of §0.1's reset
      values; `e2e-tests/src/lib.rs:578` (`start_dma(3)` → `Err`) is deleted —
      "invalid channel" is not a hardware concept, the register map decodes
      exactly three levels and everything else falls into the unhandled
      default.

### Testing (Phase 2)

- [ ] Reset-value test asserting every field in §0.1's table, with the
      hardware-reference line cited per value.
- [ ] Mirroring test: write at `0x05FE0000 + 0x00`, read back at
      `0x05FE0100`, `0x05FE0200`, `0x05FE1000` — all must alias (§1.1).
- [ ] `VER` reads `0x04` at `0xC8`; `IMS` long-read is unhandled (§1.1's
      write-only quirk) — pick one behavior, assert it, and record it in §9.
- [ ] Round-trip test for the `0xA7` byte window: `IST = 0xFFFF_FFFF`, byte
      write `0x0F` at `0xA7` → `IST == 0xFFFF_FF0F` (§1.4's AND semantics).
- [ ] `cargo test --workspace` green — this phase touches `sh2.rs`'s memory
      path, which many existing tests exercise.

**Done signal**: `work_ram.scu_regs` is gone, `Scu` is constructed by
`SaturnSystem`, and behavior is otherwise unchanged (same real-BIOS boot PC
trajectory as before the phase).

---

## Phase 3 — SCU interrupt controller

**The single-source-of-truth requirement is the point of this phase.** Today
four unrelated flags on `Sh2` (`vblank_pending`, `vblank_out_pending`,
`smpc_irq_pending`, `sound_req_irq`) are each their own miniature interrupt
controller, prioritized by a hardcoded if-chain (`sh2.rs:908-952`). Adding an
`Scu`-owned controller alongside them would make five. The migration below
replaces them; it does not run in parallel with them.

### 3a. Controller core (§4)

- [ ] `ScuIrq { ims: u32, ist: u32, aiack: u32, queue: Vec<QueuedIrq>, }` with
      `QueuedIrq { vector: u8, level: u8, mask: u16, statusbit: u32 }`.
- [ ] `send(vector, level, mask, statusbit)` implementing §4.2 exactly:
      - `mask & 0x8000` (external/A-Bus): deliver only if `AIACK != 0`, clear
        `AIACK`, and only if `IMS` bit 15 is clear. Otherwise **drop** — not
        queued, `IST` untouched (deviation #16).
      - else if `!(IMS & mask)`: deliver immediately, **`IST` untouched**.
      - else: queue + `IST |= statusbit`.
      - Slave mirrors, regardless of mask state, when the slave is running:
        vector `0x42` → slave vector `0x41` level 1; vector `0x40` → slave
        vector `0x43` level 2.
- [ ] `queue_interrupt` per §4.3: dedupe by vector (return early, but the
      caller's `IST |= statusbit` still happens), then sort ascending by level.
      Cap at 30 entries — the reference has no bounds check; Mimas should
      saturate and log rather than grow unboundedly.
- [ ] `test_interrupt_mask` (the drain) per §4.3: walk from the end
      (highest level first); A-Bus entries consume `AIACK`; non-external
      entries whose `IST` bit was already cleared by the CPU are skipped;
      otherwise deliver, clear the `IST` bit, compact, and **break** — at most
      one non-external delivery per call. Called on writes to `IMS` (`0xA0`),
      `IST` (`0xA4` and the `0xA7` byte window) and `AIACK` (`0xA8`).
- [ ] `IST` write semantics: `IST &= val` (long, `0xA4`) and
      `IST &= (0xFFFFFF00 | val)` (byte, `0xA7`) — §4.4.
- [ ] **Deviation to fix, not copy**: §4.4's `ScuRemoveInterruptByCPU` is dead
      code from a C precedence bug (`0x01 == 0` binds first), which leaks stale
      queue entries forever. Implement the *intended* behavior — when the CPU
      clears an `IST` bit, drop the matching queue entry — and record it in §9.
      Do **not** transliterate the bug.
- [ ] Interrupt *withdrawal* (§4.6) is unimplemented in the reference (all the
      `ScuRemove*` bodies are commented out). Match that; note it in §9.

### 3b. Source entry points (all 30, §4.1)

- [ ] One method per source, each a thin call into `send` with the exact
      (vector, level, mask, statusbit) tuple from §4.1's table:
      `vblank_in` `0x40`/15/`0x0001`/b0 · `vblank_out` `0x41`/14/`0x0002`/b1 ·
      `hblank_in` `0x42`/13/`0x0004`/b2 · `timer0` `0x43`/12/`0x0008`/b3 ·
      `timer1` `0x44`/11/`0x0010`/b4 · `dsp_end` `0x45`/10/`0x0020`/b5 ·
      `sound_request` `0x46`/9/`0x0040`/b6 · `system_manager`
      `0x47`/8/`0x0080`/b7 · `pad` `0x48`/8/`0x0100`/b8 · `level2_dma_end`
      `0x49`/6/`0x0200`/b9 · `level1_dma_end` `0x4A`/6/`0x0400`/b10 ·
      `level0_dma_end` `0x4B`/5/`0x0800`/b11 · `dma_illegal`
      `0x4C`/3/`0x1000`/b12 (never called — parity with deviation #14, keep the
      entry point so it is reachable when illegal-DMA detection is added) ·
      `draw_end` `0x4D`/2/`0x2000`/b13 · `external(n)` for n=0..15, vectors
      `0x50`-`0x5F`, levels 7/7/7/7/4/4/4/4/1×8, all mask `0x8000`, status bits
      16-31.
- [ ] Every `send` call site for a source with a DMA start factor must also run
      the factor check — deferred to Phase 6, but leave the hook where §2.3
      puts it (after dispatch, unconditional on mask).

### 3c. Migration of the four existing ad hoc paths

- [ ] `Sh2::service_pending_interrupt` (`sh2.rs:908-952`) stops consulting
      bools and instead asks the SCU for the highest-priority *deliverable*
      request. Keep the SR-mask check and the exception-entry sequence (push SR
      then PC, VBR-relative vector, raise mask to the interrupt's level) on the
      SH-2 side — that is genuinely CPU behavior, and it is already correct.
- [ ] Delete `Sh2::vblank_pending` (`:117`), `vblank_out_pending` (`:125`),
      `smpc_irq_pending` (`:138`), `sound_req_irq` (`:148`) and the
      `VBLANK_IN_*`/`VBLANK_OUT_*`/`SMPC_IRQ_*`/`SOUND_REQ_IRQ_*` constant pairs
      (`sh2.rs:193-222`) — the tuples now live in the SCU's source table.
- [ ] `Sh2::request_vblank_interrupt` / `request_vblank_out_interrupt`
      (`sh2.rs:794-801`) become thin shims onto `Scu::vblank_in()` /
      `vblank_out()` so the ~8 existing SH-2 interrupt tests
      (`sh2.rs:2002`, `:2058`, `:2076`, `:2111`, `:2120`, `:2156`, `:2177`)
      keep compiling; migrate them to drive the SCU directly in the same
      commit, then drop the shims.
- [ ] **Move VBLANK generation off Core 0.** `Sh2::run_loop`'s wall-clock
      VBLANK timer (`sh2.rs:1688-1706`) is a second, independent frame clock
      racing Core 3's own 16.6 ms tick (`lib.rs:207-233`). Core 3 should call
      `Scu::vblank_in()` / `vblank_out()` on its existing frame edge; Core 0
      then only *services* interrupts. This also fixes `tvstat_word`
      (`sh2.rs:897-906`), which derives TVSTAT's VBLANK bit from Core 0's timer
      rather than from the renderer's.
- [ ] `M68k`'s sound-request path (`SaturnSystem::sound_req_irq`, `lib.rs:60`,
      `m68k.rs`'s MCIPD write) targets `Scu::sound_request()` instead of the
      shared `AtomicBool`.

### Testing (Phase 3)

- [ ] Source table test: for each of the 30 sources, assert the
      (vector, level, mask, statusbit) tuple against §4.1. This is legitimate
      independently-derived data — §4.1 was extracted from a different codebase
      (`yabause/src/scu.c:3236-3481`), not from Mimas.
- [ ] **Masked-latch test** (§4.2 consequence 1): `IMS = 0xFFFF`, send V-Blank
      IN → no delivery to the SH-2, `IST` bit 0 **set**. Then send it again →
      still one queue entry (dedupe, §4.3). Then `IMS = 0` → exactly **one**
      delivery, `IST` bit 0 cleared.
- [ ] **Unmasked-no-latch test** (§4.2 consequence 1): `IMS = 0`, send V-Blank
      IN → delivered, `IST` bit 0 stays **0**. This asymmetry is the single
      most surprising thing about the reference and the easiest to get wrong.
- [ ] **AIACK-drop test** (§4.2 consequence 2): `AIACK = 0`, send External 00 →
      dropped entirely; `IST` bit 16 stays 0 and nothing is queued.
- [ ] **Level-ordering test**: queue Timer 1 (11), V-Blank IN (15) and Draw End
      (2) while masked, then unmask — assert V-Blank IN is delivered first
      (drain walks from the end of a level-ascending sort, §4.3).
- [ ] **Stale-entry test** (the deviation in 3a): queue a masked interrupt, have
      the CPU clear its `IST` bit, then unmask — assert the queue is empty and
      nothing is delivered. Under the reference's buggy behavior the entry
      would linger forever; the test documents the deliberate divergence.
- [ ] End-to-end: a `SaturnSystem` boot run where Core 3's frame edge drives
      VBLANK through the SCU and Core 0's PC still enters the BIOS's VBLANK
      handler — the same observable that proved the original VBLANK-IN and
      VBLANK-OUT work (`.development/ROADMAP.md` M2).

**Done signal**: exactly one interrupt mechanism exists in the codebase; `grep
-n "pending" saturn-core/src/sh2.rs` no longer finds per-source bools; the real
BIOS reaches at least the same PC as before.

---

## Phase 4 — Independent SCU DMA controller on Core 6

Retires `Sh2::execute_scu_dma` (`sh2.rs:1536-1639`) entirely and moves the
engine onto the thread that is already named for it.

### 4a. Engine state and trigger

- [ ] `DmaLevel` mirroring §2.1's `scudmainfo_struct`: `mode` (the level
      number 0/1/2, used only to pick the completion interrupt), `read_address`,
      `write_address`, `transfer_number` (**`> 0` is the busy predicate**),
      `add_value`, `mode_address_update`, `read_add`, `write_add`,
      `indirect_address`. It is a *working copy* — the memory-mapped registers
      are snapshotted at trigger and never written back (§2.1).
- [ ] `set_add_value` per §1.2: `read_add = if DnAD & 0x100 { 4 } else { 0 }`;
      `write_add` from `DnAD[2:0]` → 0/2/4/8/16/32/64/128 bytes.
- [ ] Count clamps per §1.2 (fixes **D-DMA-4**): level 0 keeps all 32 bits and
      `0` means `0x100000`; levels 1 and 2 clamp `&= 0xFFF` and `0` means
      `0x1000`; **indirect mode skips the clamp entirely**.
- [ ] Trigger path (a), immediate (§2.2, fixes **D-DMA-7**): CPU writes `DnEN`
      with bit 0 set **and** `DnMD[2:0] == 7`. If the level is already busy,
      first flush *all three levels* to completion (§2.2's
      `ScuDmaProc(scu, 0x7FFFFFFF)` — note it flushes every level, not just the
      re-triggered one), then snapshot and start with a 128-unit budget.
- [ ] Trigger path (b), factor (§2.2/§2.3): `DnEN & 0x100` (armed) and
      `DnMD[2:0] == factor_id`; after starting, `DnEN = 0` (one-shot). Wired in
      Phase 6.
- [ ] Indirect select is `DnMD & 0x1000000` — **bit 24** (§1.2, fixes
      **D-DMA-1**).

### 4b. Transfer semantics (§2.4, §2.5)

- [ ] **Fill mode** (`read_add == 0`, §2.4, fixes **D-DMA-5**): the
      constant-source test — source in Low WRAM
      (`(src & 0x1FF00000) == 0x00200000`), High WRAM
      (`(src & 0x1E000000) == 0x06000000`), Sound RAM
      (`(src & 0x1FF00000) == 0x05A00000`) or VDP1/VDP2 RAM
      (`(src & 0x1DF00000) == 0x05C00000`) → read the source long once;
      anything else → re-read every iteration (it is treated as a register).
      Destination on the B-Bus → two 16-bit writes with `dst += write_add`
      **twice** and `count -= 4`; otherwise one 32-bit write with
      `dst += write_add`, `count -= 4`.
- [ ] **Copy mode** (`read_add != 0`, §2.4, fixes **D-DMA-6**): destination on
      B-Bus → 16-bit unit, `src += 2`, `dst += write_add`, `count -= 2`; source
      on B-Bus → 16-bit unit, `src += 2`, `dst += (write_add >> 1)`,
      `count -= 2`; neither → 32-bit unit, `src += 4`, `dst += write_add`,
      `count -= 4`. Note deviation #11: `read_add` never functions as a source
      stride, only as the fill/copy selector.
- [ ] B-Bus range constant: `0x05A00000 ≤ (addr & 0x1FFFFFFF) < 0x05FF0000`
      (§2.4). Note this is a *different* boundary from the DSP DMA's B-Bus test
      (`< 0x06000000`, §3.8.6) — do not unify them.
- [ ] Source/destination accesses mask with `0x0FFFFFFF` (§1.2).
- [ ] **Indirect mode** (§2.5, fixes **D-DMA-2** and **D-DMA-3**): the
      descriptor table lives at **`DnW`**, descriptors are 12 bytes,
      `[+0x0] = count`, `[+0x4] = destination`, `[+0x8] = source` with **bit 31
      of the source** marking the last descriptor. `indirect_address` starts at
      `DnW + 0xC`; after each descriptor completes, test the end bit, then load
      the next three long-words and advance by `0xC`. The end bit is not
      stripped — every access masks `0x0FFFFFFF` instead.
- [ ] `DSTA` (§1.3): busy bits 4/8/12 recomputed live from
      `dmaN.transfer_number > 0` on every read; all other bits are
      last-written storage.
- [ ] `DSTP` (§2.6, deviation #13): inert in the reference. Keep it inert but
      log a `[SCU] DSTP written` line once, so a real BIOS attempt to abort a
      DMA is visible rather than silent.
- [ ] Completion interrupts (§2.8) via Phase 3's controller:
      level 0 → vector `0x4B` level 5 mask `0x0800`;
      level 1 → vector `0x4A` level 6 mask `0x0400`;
      level 2 → vector `0x49` level 6 mask `0x0200`.
      Raised from both the indirect end-of-list path and the direct completion
      path.
- [ ] Delete `Sh2::execute_scu_dma` and its three call sites
      (`sh2.rs:684-696`, `:1536-1639`).

### 4c. Threading — the part that needs the most care

- [ ] **Scheduling** (§2.6): all three levels are serviced per pass with their
      own private copy of the same time budget, in textual order 0 → 1 → 2.
      There is **no priority arbitration between levels** in the reference —
      implement it that way and record it in §9 rather than inventing a
      priority scheme Mimas can't validate.
- [ ] **Cost model** (§2.6): one loop iteration (2 or 4 bytes) costs one unit.
      Core 6's per-pass budget replaces the reference's `timing << 4`; a fresh
      trigger gets +128 units; a re-trigger-while-busy flush runs to
      completion.
- [ ] **`BusArbiter` integration.** The reference has no bus locking at all
      (deviation #12), but Mimas models it deliberately (`bus_arbiter.rs`), and
      the CPU side already honors it: every `Sh2` memory access calls
      `bus_wait()` → `BusArbiter::acquire_bus_sync(core_id, sync)`
      (`sh2.rs:383-391`, `bus_arbiter.rs:38-52`), which *also deactivates the
      blocked core in `LockStepSync`* so a DMA-stalled CPU doesn't drag the
      slack window down for everyone. The DMA engine must be the counterpart:
      - Call `lock_for_dma()` / `unlock_from_dma()` **per time-slice burst**,
        not once around a whole transfer. Holding it across a 1 MiB level-0
        copy would stall both SH-2s for the entire copy — which is exactly what
        `execute_scu_dma` does today (`sh2.rs:1548`/`:1631`) and a large part of
        why it must move off Core 0.
      - `BusArbiter`'s lock is a plain `AtomicBool`, not a count
        (`bus_arbiter.rs:5`). With DMA levels *and* DSP DMA both able to run on
        Core 6, either serialize them on Core 6 (simplest — one thread, one
        lock at a time) or make the lock a counter. Prefer serializing; note
        the choice in the code.
      - Core 6 must **never** call `acquire_bus`/`acquire_bus_sync` — it is the
        lock holder; doing so would self-deadlock. This is why
        `execute_scu_dma` uses `raw_read_byte`/`raw_write_byte` today
        (`sh2.rs:1553`); the new engine should reach `WorkRam` directly instead,
        the way `scu_dsp.rs`'s `read_long`/`write_long` already do.
      - Decide and state whether DSP DMA also takes the bus lock (it does not
        today — **D-DSP-8**). Recommended: yes, same policy, since both engines
        contend for the same physical bus.
- [ ] **`LockStepSync` integration.** Core 6 currently parks unless the DSP is
      executing (`lib.rs:320-341`: `set_thread_active(6, false)` →
      `park_while_inactive(6)` → step while `is_executing()` → re-park).
      Extend, don't replace, that shape:
      - Wake condition becomes "DSP executing **or** any DMA level busy". A
        `DnEN` write that starts a transfer calls `sync.set_thread_active(6,
        true)` from `Sh2::write_long`, exactly as the `EX` write does today
        (`sh2.rs:732-736`).
      - Re-park only when *both* are idle.
      - Keep calling `sync_core(6, cycles)` inside the work loop so the DMA
        engine participates in the slack window rather than running unbounded
        ahead.
      - Note the deliberate ordering hazard: Core 6 holds the bus lock while
        Cores 0/1 block in `acquire_bus_sync`, which deactivates them in
        `LockStepSync`. Core 6 must not then block waiting on *them* in
        `sync_core`. Verify with a targeted adversarial test (see below);
        `saturn-core/tests/adversarial_tests.rs` already hosts this class of
        test.

### Testing (Phase 4)

- [ ] **Direct copy**, hand-derived: fill High WRAM with a known generator
      (`byte[i] = (i * 7 + 3) & 0xFF`), program `D0R`/`D0W`/`D0C`/`D0AD`,
      trigger, and assert the destination image against the same generator
      computed in the test — with the expected stride derived from §1.2's
      `write_add` table by hand, not by reading the implementation.
- [ ] **Fill mode**, hand-derived: `DnAD` bit 8 clear, a source in Low WRAM
      (constant-source region) → assert the destination is `count/4` copies of
      one long. Then repeat with a source in a non-constant region and assert
      the re-read behavior differs.
- [ ] **B-Bus split**: destination in VDP2 VRAM → assert the two-16-bit-write
      ordering (high half first) produced the right byte order, and that the
      destination pointer advanced by `write_add` **twice** per iteration.
- [ ] **Count clamps**: level 1 with `DnC = 0` moves exactly `0x1000` bytes;
      level 0 with `DnC = 0` moves exactly `0x100000`; level 1 with
      `DnC = 0x12345` moves `0x345`. Derived from §1.2, not from the code.
- [ ] **Indirect**, hand-built: construct a 3-descriptor chain in real Work RAM
      with the last one's source carrying bit 31, place the table at `DnW`, and
      assert all three destinations. Add a negative test that the *old* buggy
      layout (end bit in the count word, table at `DnR`) produces the wrong
      answer — this is the regression guard for D-DMA-2/3.
- [ ] **Completion interrupt**: assert vector `0x4B` / level 5 fires exactly
      once at the end of a level-0 direct transfer, and once at end-of-list
      (not per descriptor) for an indirect one.
- [ ] **`DSTA` liveness**: mid-transfer read shows bit 4 set; post-transfer
      read shows it clear.
- [ ] **Threading**: a `SaturnSystem`-level test where Core 0 triggers a large
      level-0 transfer and continues executing — assert Core 0 makes forward
      progress (its PC advances) and the transfer still completes correctly,
      i.e. the bus lock is being released between bursts. Plus a shutdown-
      during-DMA test (`arbiter.abort()` must unblock everyone —
      `bus_arbiter.rs:67-71`, `PanicGuard`).

**Done signal**: `execute_scu_dma` is gone from `sh2.rs`; a DMA in flight is
visible as Core 6 activity in `LockStepSync`; `cargo test --workspace` green.

---

## Phase 5 — SCU timers 0 and 1

Blocked on Phase 3 (both timers only exist to raise interrupts) and on an
H-Blank IN source, which does not exist anywhere today.

- [ ] **H-Blank IN source first.** Core 3 (`lib.rs:204-239`) already paces
      frames at 16.6 ms. Add a scanline tick — NTSC 263 lines per frame — that
      calls `Scu::hblank_in()`. Keep it derived from the same frame edge as
      VBLANK (Phase 3c) so the two cannot drift apart the way Core 0's and
      Core 3's clocks do today.
- [ ] `T0C` (`0x90`, §5.1): Timer 0 compare value, interpreted as a **scanline
      number**.
- [ ] `T1S` (`0x94`, §5.1): store `T1S`, and — the write side effect — set
      `timer1_set = 1` and `timer1_preset = val`.
- [ ] `T1MD` (`0x98`, §5.1): bit 0 = global timer enable (gates all Timer 0
      comparisons and the Timer 1 reload); bit 7 = Timer 1 mode (0 = fire every
      line, 1 = fire only when Timer 0 also matched this line). No other bit is
      read anywhere.
- [ ] `ScuTimers { timer0, timer1, timer1_counter, timer0_set, timer1_set,
      timer1_preset }` (§5.1).
- [ ] **Timer 0** (§5.2): `timer0++` on every H-Blank IN; `timer0 = 0` at
      V-Blank OUT; compare on both the H-Blank path and immediately after the
      V-Blank OUT reset (so `T0C == 0` fires at V-Blank OUT), both gated on
      `T1MD & 1`. On match → `Scu::timer0()` (vector `0x43`, level 12, mask
      `0x0008`) and `timer0_set = 1`; on mismatch → `timer0_set = 0`.
- [ ] **Timer 1** (§5.3): reloaded at H-Blank IN when `T1MD & 1` and
      `timer1_set == 1` (`timer1_set = 0; timer1_counter = timer1_preset`).
      Counts down by `timing >> 1` per tick; on reaching ≤ 0, `timer1_set = 1`
      and — if `T1MD & 0x80 == 0`, or if `timer0_set == 1` — `Scu::timer1()`
      (vector `0x44`, level 11, mask `0x0010`).
- [ ] **Deviation to fix, not copy** (§5.4, deviation #18): the outer gate
      `if (T1MD & 0x80 == 0)` in `ScuExec` is a C precedence bug that makes the
      branch unreachable, so the reference only ticks Timer 1 on the scanline
      where `LineCount == T0C` (or unconditionally when `T0C > 500`). Implement
      the *intended* reading — tick every time when `T1MD` bit 7 is clear — and
      record it in §9. Keep the correctly-written inner test inside
      `ScuTimer1Exec` (§5.3) as-is.
- [ ] Pick the tick unit deliberately. The reference's `timing` is
      `sh2cycles >> 1` (§0), and Timer 1 decrements by `timing >> 1`, i.e. one
      unit per ~4 SH-2 cycles. Mimas has no global cycle bus; derive it from
      Core 6's own `sync_core` cycle accounting and document the conversion in
      the code, rather than inheriting an unexplained shift.

### Testing (Phase 5)

- [ ] Hand-traced scanline sequence: `T1MD = 1`, `T0C = 100`; drive 263
      H-Blank INs plus one V-Blank OUT and assert Timer 0 fires exactly once,
      on line 100. Derive the expected count offline.
- [ ] `T0C = 0` fires Timer 0 at V-Blank OUT (§5.2's second compare path).
- [ ] `T1MD` bit 0 clear → no timer interrupt ever fires, regardless of
      `T0C`/`T1S`.
- [ ] `T1MD` bit 7 set → Timer 1 fires only on lines where Timer 0 matched;
      bit 7 clear → every line (this is the deviation above; assert the
      *intended* behavior and cite §5.4).
- [ ] Timer 1 reload: write `T1S`, let it expire, assert `timer1_set` re-arms
      the reload at the next H-Blank IN.

**Done signal**: both timers raise their vectors through the Phase 3
controller; H-Blank IN exists as a first-class event other subsystems (VDP2
line-based effects, later) can consume.

---

## Phase 6 — Start factors, DSP End, Draw End: closing the loop

The pieces that only make sense once Phases 3-5 exist.

- [ ] **DMA start factors** (§2.3) — wire `ScuChekIntrruptDMA(id)`'s equivalent
      into each source, *after* the interrupt dispatch and **unconditional on
      the interrupt mask** (§2.3: a masked V-Blank IN still starts a DMA armed
      on factor 0):
      | id | Factor | Raised from |
      |---|---|---|
      | 0 | V-Blank IN | Phase 3 `vblank_in` |
      | 1 | V-Blank OUT | Phase 3 `vblank_out` |
      | 2 | H-Blank IN | Phase 5 `hblank_in` |
      | 3 | Timer 0 | Phase 5 `timer0` |
      | 4 | Timer 1 | Phase 5 `timer1` |
      | 5 | Sound Request | Phase 3 `sound_request` |
      | 6 | Sprite Draw End | `draw_end`, below |
      | 7 | Immediate | Phase 4's `DnEN` bit-0 path |
      Note §2.3's exclusions: DSP End, System Manager, Pad, the three DMA-end
      senders, DMA Illegal and all 16 externals do **not** start a DMA.
- [ ] For each level independently: `if (DnEN & 0x100) && (DnMD & 0x7) == id` →
      snapshot + start + `DnEN = 0` (§2.2 path b). Re-trigger-while-busy uses
      the same all-levels flush as the immediate path.
- [ ] **DSP End interrupt** (§3.12): `ENDI` sets the `E` flag *and* raises
      vector `0x45`, level 10, mask `0x0020`. Replaces the TODO comment at
      `scu_dsp.rs:473-476`. Note `E` is sticky — the Program Control Port write
      mask preserves bit 18 (§3.9.1), so nothing but a reset clears it.
- [ ] **Sprite Draw End** (`0x4D`, level 2, mask `0x2000`, factor 6): raised by
      VDP1 when a command list completes. `vdp::execute_vdp1` runs on Core 3
      (`lib.rs:216`) and raises no interrupt today. Coordinate with
      `docs/implementation-plans/vdp1.md` — this plan owns the SCU-side entry
      point, not the VDP1-side trigger condition.
- [ ] **Pad interrupt** (`0x48`, level 8, mask `0x0100`): SMPC-side trigger;
      coordinate with `docs/implementation-plans/smpc-peripheral.md`.
- [ ] **External interrupts 00-15** (`0x50`-`0x5F`): no A-Bus device is
      emulated, so these have no producer. Leave the entry points unreachable
      and say so — do not invent a caller.
- [ ] **DMA Illegal** (`0x4C`): illegal-DMA detection is unimplemented in the
      reference too (deviation #14). Leave the entry point unreachable; if
      Phase 4 ever grows address-validity checks, this is where they report.

### Testing (Phase 6)

- [ ] Per factor 0-6: arm a level with `DnEN = 0x100` and `DnMD = id`, raise
      the source, assert the transfer ran and `DnEN` was cleared to 0
      (one-shot).
- [ ] **Masked-factor test**: `IMS` masking V-Blank IN, armed level on factor
      0 → the DMA still starts (§2.3). This is counterintuitive and worth a
      dedicated test.
- [ ] `ENDI` raises vector `0x45` and leaves `E` set across a subsequent
      Program Control Port write (sticky, §3.9.1).

---

## 7. Architectural call-outs (cross-phase)

1. **One interrupt mechanism, not two.** Phase 3 is explicitly a *replacement*
   of `Sh2`'s four ad hoc pending flags, not an addition beside them. If a
   phase ships with both alive, that is a regression regardless of test
   results — `CLAUDE.md`'s "Known architecture debt" section exists because
   exactly this kind of duplication accumulated before.
2. **The DMA engine belongs on Core 6.** Core 6 is named `scu-dma-dsp`
   (`lib.rs:320`) and today runs only the DSP. Anything that keeps DMA on
   Core 0 keeps the thread-per-hardware-component model a fiction for this
   block (`docs/mimas-architecture-spec.md` §1.1, §3.1).
3. **`BusArbiter` is Mimas's own model, not the reference's.** §2.6 explicitly
   says Yabause has no bus locking. Mimas does, deliberately, and the CPU side
   already honors it via `acquire_bus_sync` (`bus_arbiter.rs:38-52`). Keeping
   the DMA engine's lock/unlock symmetric with that is a Mimas-specific
   requirement with no counterpart to copy — design it, test it, and document
   it in `history.md`.
4. **Lock ordering: `Scu` → `WorkRam`, never the reverse; never hold `Scu`
   across a burst.** §2.1's working-copy design makes snapshot-then-release the
   natural shape. `CLAUDE.md`'s existing rule ("acquire in field-declaration
   order") extends to the new `Scu` sub-locks.
5. **Park semantics.** Only Cores 1 and 6 genuinely park today. Whatever Phase
   4 does to Core 6's loop must preserve that — a DMA controller that
   busy-loops when idle regresses the measured fix in `history.md` Chapter 10.
6. **Don't unify the two B-Bus range tests.** SCU DMA uses
   `0x05A00000..0x05FF0000` (§2.4); DSP DMA uses `0x05A00000..0x06000000`
   (§3.8.6). They differ in the reference and must differ here.
7. **Don't unify the two `add` decodes.** DSP DMA reads use instruction bit 16
   only; DSP DMA writes use the full 3-bit field; SCU DMA uses `DnAD` bits 2:0
   plus bit 8. Three different decodes of superficially similar fields.

---

## 8. Testing philosophy applied to the SCU

From `CLAUDE.md`: *write regression tests from independently-derived values —
never assert a value you haven't independently derived.* For this subsystem
that means, concretely:

- **`docs/hardware-reference/scu.md` counts as independent.** It was extracted
  from `yabause/src/scu.c` — a different codebase — with `file:line` citations.
  Encoding its tables (interrupt tuples, `add` tables, descriptor layout) into
  tests is legitimate. Re-verify a citation before trusting it if the code
  disagrees.
- **Mimas's own output does not count.** No test may be written by running the
  new code and pasting what it produced. For every DMA-shaped test, compute the
  expected (address, value) list in a throwaway Python script from the
  reference's tables first, then paste *that* into the test.
- **Real BIOS bytes are the strongest anchor.** The existing
  `real_bios_dsp_program_runs_to_completion` (`scu_dsp.rs:862`) is the model:
  real captured program words, real parameter values. Phase 1c strengthens it
  from "terminates" to "terminates with this exact state". Prefer this shape
  whenever a real trace makes it available.
- **Negative tests for fixed defects.** Every `D-*` defect in §1.6 should leave
  behind a test that fails against the old behavior — especially D-DSP-1
  (phantom ALU ops), D-DMA-2/3 (indirect descriptor layout) and D-DSP-6
  (silently-discarded VDP writes), all three of which are currently invisible.
- **`cargo test --workspace` stays green after every phase**, not just the
  phase's own tests (`CLAUDE.md`, "Stability constraints"). Phases 2 and 3
  touch `sh2.rs`'s memory and interrupt paths, which many existing tests cover.
- **Real-BIOS check per phase**: `MIMAS_BOOT_WATCH_SECS=280` run, compare
  Core 0's settled PC against the previous phase's. A phase that moves it
  forward is progress; a phase that moves it *backward* is a regression even if
  every unit test passes.

---

## 9. Deliberate divergences from the reference (maintain this list)

`docs/hardware-reference/scu.md` §7 lists 20 divergences to be aware of when
porting. Mimas's position on each:

| # | Reference behavior | Mimas decision | Phase |
|---|---|---|---|
| 1 | DSP read-DMA uses instruction bit 16 (disassembler says 15) | **Match the interpreter** (bit 16) | 1b |
| 2 | DSP DMA with bit 11 set silently does nothing | Match — still clear `T0`/`wait` | 1b |
| 3 | `dsp_dma05` degrades `PRG` → `MD0` | Match, with a comment | 1b |
| 4 | DSP DMA CPU-bus writes go straight into High WRAM | Match (already at `scu_dsp.rs:641`) | — |
| 5 | `dsp_dma03` skips `RA0` writeback on A-Bus | Match (D-DSP-7) | 1a |
| 6 | Data RAM host port never re-masks its 6-bit offset | **Diverge** — Mimas masks (`scu_dsp.rs:169`). The reference walks off the end of a `[4][64]` array, which is a C bug, not hardware behavior | already |
| 7 | DSP `V` flag never set | Match (no ALU op sets it) | — |
| 8 | `RR`/`RL` are plain rotates | Match | — |
| 9 | ALU ops `0x7`/`0xC`/`0xD`/`0xE` unimplemented | Match — do not guess semantics | — |
| 10 | `EP`/`PR`/`ES` stored but never acted on | Match | — |
| 11 | DMA copy mode ignores `ReadAdd` for source stride | Match | 4b |
| 12 | No DMA priority arbitration, no bus locking | **Diverge on locking** (Mimas models the bus via `BusArbiter`); **match on priority** (textual 0→1→2) | 4c |
| 13 | `DSTP` inert | Match, but log the write | 4b |
| 14 | `ScuSendDMAIllegal` never raised | Match; keep the entry point | 6 |
| 15 | `IST` latches only masked interrupts | Match — this is load-bearing | 3a |
| 16 | External interrupts dropped when `AIACK == 0` | Match | 3a |
| 17 | `ScuRemoveInterruptByCPU` dead (C precedence bug), leaks queue entries | **Diverge** — implement the intended behavior | 3a |
| 18 | Timer 1 gate inverted by a C precedence bug | **Diverge** — implement the intended reading | 5 |
| 19 | `IMS`/`DnAD`/`DnEN`/`DnMD`/`DSTP`/`T0C`/`T1S`/`T1MD`/`ASR0/1`/`AREF` have no read handlers | Match by default; revisit if a real BIOS read shows up in `[REGACCESS]` | 2 |
| 20 | All 16-bit SCU accesses are no-ops | **Diverge** — tagged `[QUIRK]` (emulator shortcut), so keep byte-pair access and log | 2 |

Every "diverge" row must also be stated in the code, at the site, with a
pointer back to this table — same convention `scu_dsp.rs` already uses for its
per-method Yabause citations.

---

## 10. Tracking-doc updates each phase owes

Per `CLAUDE.md`'s "Tracking docs — update these as you go":

- [ ] `.development/TASKS.md` — move the SCU items between Done / In-progress /
      Not-started as each phase lands.
- [ ] `.development/ROADMAP.md` — M3's SCU DSP bullet currently says "2 of 8
      DMA addressing-mode variants"; Phase 1 retires that sentence. The DMA
      controller, interrupt controller and timers are not represented in any
      milestone today — add them.
- [ ] `.development/current_blocker.md` — rewrite when a phase clears the
      `0x060131A8` wall, or when the Phase 0 measurement identifies its cause.
      This file is not a log.
- [ ] `.development/current_bugs.md` — the `D-*` defects in §1.6 that aren't
      fixed in the phase currently underway belong here, so they are not
      re-discovered.
- [ ] `history.md` — one chapter per non-obvious decision: why `scu.rs` was
      repurposed rather than deleted (§2), why the DMA engine locks the bus per
      burst rather than per transfer (§7.3), and why divergences 6/17/18 were
      chosen over parity (§9).
- [ ] `CLAUDE.md`'s "Known architecture debt" — the bullet reading
      "`saturn-core/src/scu.rs` (`Scu`) … are dead code … A real SCU DMA
      controller (levels 0-2, independent of the DSP) does not exist yet"
      becomes false at Phases 2 and 4 respectively. Update it in the same
      commit.
- [ ] `docs/mimas_emu_engineering_draft.md` §3 — its "Current implementation
      status" paragraph and its "target design, not yet implemented" caption on
      the DMA sequence diagram both need revising as Phase 4 lands.
