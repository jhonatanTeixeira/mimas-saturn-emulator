# SMPC & Peripheral — implementation plan

Diffs Mimas's current Rust against `docs/hardware-reference/smpc-peripheral.md` (the
authoritative, Yabause-sourced register/command/protocol reference, cited below as **§n**) and
lays out an ordered path to full fidelity.

**Scope note.** `hardware-reference/smpc-peripheral.md` documents what *Yabause* does, with
every deviation from real hardware tagged `[QUIRK]`/`[BUG]`/`[HACK]`/`[DEAD]`. This plan targets
**real hardware semantics**, using the reference as the verified description of what real BIOS
and game code was actually tested against — and it explicitly declines to port the reference's
tagged bugs (see "Yabause defects deliberately not replicated" at the end). Per `CLAUDE.md`:
port *what the hardware does*, never transliterate the C.

**Progress: Phases 0-2 done** (see `history.md` Chapter 14 for the narrative and
`saturn-core/src/smpc.rs` for the code) — **Phases 3-7 not started.** One item from Phase 1
(SSHOFF resetting the slave core) was deliberately deferred; see that phase's notes.

---

## 0. Current-state assessment

### 0.1 Where the code actually is

| Thing | Where it lives today |
|---|---|
| SMPC register **storage** | `saturn-core/src/shared_buffers.rs:54-61` — `WorkRam::smpc_regs: RwLock<Box<[u8; 0x80]>>`, one independent region lock |
| Address decode | `sh2.rs:346-351` — `0x00100000..0x00180000` → `MemRegion::Smpc(a & 0x7F)` |
| Register **read** | `sh2.rs:477-497` — SF (`0x63`) special-cased to a hardcoded `0x00` at `sh2.rs:487`; every other offset is plain byte storage at `sh2.rs:494-497` |
| Register **write** | `sh2.rs:577-591` — unconditional byte store, then `if off == SMPC_COMREG_OFFSET { self.smpc_execute_command(val) }` at `sh2.rs:588-590` |
| Command **dispatch** | `sh2.rs:826-888` — `Sh2::smpc_execute_command`, a flat `if` chain, synchronous, inside the CPU's own store instruction |
| Completion interrupt | `sh2.rs:132-138` (`smpc_irq_pending`), `sh2.rs:212-217` (level 8 / vector `0x47`), raised at `sh2.rs:887`, serviced at `sh2.rs:908-952` |
| Offset constants | `sh2.rs:39-57` — only `SMPC_SF_OFFSET`, `SMPC_IREG1_OFFSET`, `SMPC_COMREG_OFFSET`, `SMPC_OREG_BASE_OFFSET`, `SMPC_SR_OFFSET`, `SMPC_CMD_INTBACK`, `SMPC_CMD_SNDON`, `SMPC_CMD_SNDOFF` |
| `saturn-core/src/smpc.rs` | **Dead code.** 25 lines, a `Vec<u8> command_buffer` + an `execute_command` returning a magic `0x55`. Never constructed by `SaturnSystem`; reachable only from `e2e-tests/src/lib.rs:234-236` and `:584-590`. Bears no relationship to any real SMPC behavior. |
| Core 7 (`smpc-cd-block`) | `lib.rs:343-360` — pure `sync_core` + `yield_now` idle loop. No SMPC logic runs on it. |
| Peripheral / controller input | **Does not exist anywhere in the workspace.** No pad type, no port state, no frontend plumbing. `saturn-frontend-libretro/src/lib.rs` is 5 lines of `retro_init`/`retro_deinit` stubs; `saturn-frontend-native/src/bin/mimas_window.rs:69` reads exactly one key (`Escape`, to close the window). |

`SR_WRITE_MASK` (`sh2.rs:63`) is the **SH-2 status register** write mask, not SMPC's SR — an
easy-to-confuse name collision, unrelated to `SMPC_SR_OFFSET`.

### 0.2 Command coverage — what `smpc_execute_command` actually handles

Reference inventory: 15 dispatchable commands (§3.4). Mimas today:

| COMREG | Name | §  | Mimas today | Location |
|---|---|---|---|---|
| `0x00` | MSHON | §4.2 | ✗ silently dropped | `sh2.rs:854-856` |
| `0x02` | SSHON | §4.3 | ✓ partial — `sync.set_thread_active(1, true)`; no OREG31 (correct: §4.1 says SSHON writes none) | `sh2.rs:827-832` |
| `0x03` | SSHOFF | §4.4 | ✓ partial — `set_thread_active(1, false)`. **Does not reset the slave**, which §4.4 requires | `sh2.rs:833-838` |
| `0x06` | SNDON | §4.5 | ✓ end-to-end (see §0.5). **Missing OREG31 = `0x06`** | `sh2.rs:839-847` |
| `0x07` | SNDOFF | §4.6 | ✓ end-to-end. **Missing OREG31 = `0x07`** | `sh2.rs:848-853` |
| `0x08` | CDON | §4.7 | ✗ silently dropped | `sh2.rs:854-856` |
| `0x09` | CDOFF | §4.7 | ✗ silently dropped | `sh2.rs:854-856` |
| `0x0D` | SYSRES | §4.8 | ✗ silently dropped | `sh2.rs:854-856` |
| `0x0E` | CKCHG352 | §4.9 | ✗ silently dropped | `sh2.rs:854-856` |
| `0x0F` | CKCHG320 | §4.10 | ✗ silently dropped | `sh2.rs:854-856` |
| `0x10` | INTBACK | §5 | ~ status block only, partial (§0.3) | `sh2.rs:854-888` |
| `0x17` | SETSMEM | §4.12 | ✗ silently dropped; no SMEM state exists | `sh2.rs:854-856` |
| `0x18` | NMIREQ | §4.13 | ✗ silently dropped; no NMI path in `Sh2` | `sh2.rs:854-856` |
| `0x19` | RESENAB | §4.14 | ✗ silently dropped; no `resd` state | `sh2.rs:854-856` |
| `0x1A` | RESDISA | §4.15 | ✗ silently dropped; no `resd` state | `sh2.rs:854-856` |
| unrecognised | — | §2.2 | ✗ no "clear SF immediately" path | — |

Also absent: the reset button (§4.16), gated on `resd`.

### 0.3 INTBACK, step by step, as implemented today (`sh2.rs:857-888`)

1. Take `work_ram.smpc_regs.write()` and hold it across the whole body (`:857`).
2. Read IREG1 (`:858`), compute `wants_peripheral = ireg1 & 0x8 != 0` (`:859`) — **and never use
   it**. This is a live `unused_variable` compiler warning today (verified via `cargo build -p
   saturn-core`), i.e. the peripheral request bit is read and discarded.
3. `OREG0 = 0x80` (`:867`).
4. `OREG1..OREG7 = 0` (`:868-870`) — the RTC block, zeroed.
5. `OREG8 = 0` cartridge (`:871`), `OREG9 = 1` region (`:872`), `OREG10 = 0x34` (`:873`),
   `OREG11 = 0` (`:874`).
6. `SR = 0x6F` unconditionally (`:880`).
7. Drop the lock, set `smpc_irq_pending = true` (`:887`).

Diff against §5/§5.6:

- **No IREG0 decode at all.** §5.2 branches three ways on `IREG0 & 1` and `IREG1 & 0x8`
  (status / peripheral-only / no-op). Mimas returns the status block unconditionally, whatever
  IREG0 says.
- **No continuation state machine.** No `intback` flag, no `firstPeri` (§0.1, §5.2). The IREG0
  write handler's break (bit 6) / continue (bit 7) decode (§5.2, `smpc.c:760-776`) does not
  exist — IREG0 is inert storage.
- **No `SmpcINTBACKEnd` at V-Blank IN** (§5.2). Mimas raises VBLANK-IN at `sh2.rs:1688-1699`;
  nothing hooks SMPC there.
- **No peripheral path whatsoever** — §5.4's 32-byte chunker and §5.5's report format are
  entirely absent.
- **`OREG0` is wrong.** §5.6 requires `0x80 | (resd << 6)`, and §0.4 step 4 sets `resd = 1` at
  reset. The faithful first-boot value is **`0xC0`**, not `0x80`.
- **`OREG1-7` (RTC) are zeroed**, not BCD date/time (§5.6, §7.1).
- **`OREG12-15` (SMEM) are never written** (§5.6, §7.3).
- **`OREG31` (command echo `0x10`) is never written** (§4.1, §5.6).
- **`SR` is wrong.** §5.3 requires `0x4F | (intback << 5)` where `intback = (IREG1 & 8) >> 3`.
  Mimas hardcodes `0x6F` — correct only when the caller *did* request peripheral data. The
  in-code justification (`sh2.rs:875-879`: "Real BIOS boot code polls PDE waiting for it to be
  1 … leaving it 0 … hangs the boot loop at 0x338C") is a deliberate deviation that must be
  re-verified, because the live trace in §0.6 shows the real BIOS issuing INTBACK with
  **IREG1 = 0x02** (bit 3 clear), for which the spec-correct SR is **`0x4F`**.
- **No `intback_wait_for_line` / scanline gate, no timing at all** (§3.2, §3.3).

### 0.4 Registers with no behavior

`PDR1` (`0x75`), `PDR2` (`0x77`), `DDR1` (`0x79`), `DDR2` (`0x7B`), `IOSEL` (`0x7D`),
`EXLE` (`0x7F`) all exist **only as plain bytes** in the generic `MemRegion::Smpc` arms
(`sh2.rs:494-497` read, `sh2.rs:582-587` write). None of §6 exists: no DDR-as-mode-selector
(§6.1), no PDR response synthesis (§6.2), no DDR1 ID-nibble table (§6.3), no EXLE→VDP2 external
latch (§6.4), no light-gun path (§6.5).

`SF` is the one register with read logic, and it is a **hardcoded `0x00`** (`sh2.rs:487`), not
§1.3's `(bustmp & 0xFE) | SF`. There is no `bustmp`. There is no "SMPC sets SF busy" state at
all — commands complete inside the store instruction that arms them.

RTC: no host clock read, no `clocksync`/`basetime`, no SETTIME (correctly — §3.4 confirms no
RTC-write command exists on this hardware). SMEM: absent. Region: hardcoded `1` at
`sh2.rs:872`; no `regionsetting`/autodetect (§0.3, §0.2 `SmpcRecheckRegion`). `dotsel`,
`mshnmi`, `sysres`, `sndres`, `cdres`, `resd`: none exist as state.

Two structural divergences from §1.1 worth recording rather than "fixing" blindly:

- Yabause indexes `SmpcRegsT[addr >> 1]`, making even offset `2n` a storage alias of odd
  `2n+1`. Mimas stores one independent byte per offset in a `[u8; 0x80]`. Mimas's model is
  *closer to hardware intent* (registers live only on odd addresses); the alias is tagged
  `[QUIRK]` in §1.1. **Keep Mimas's model**, document it.
- §1.4: Yabause's word/long reads return 0 and word/long writes are dropped. Mimas decomposes
  every access into `raw_read_byte`/`raw_write_byte` (`sh2.rs:613-707`), so a *word* write
  spanning `0x1E`/`0x1F` would arm a command. The live trace (§0.6) shows the real BIOS using
  **only `MOV.B`** against SMPC, so this is untested territory either way. **Keep Mimas's
  byte-decomposition**, add an explicit test pinning the behavior.

### 0.5 SNDON/SNDOFF — confirmed working end to end

Traced through: `sh2.rs:839-853` stores into `Sh2::m68k_control` (`sh2.rs:109`, an
`Option<Arc<AtomicBool>>`) with `Ordering::Release`. `SaturnSystem` owns the `Arc`
(`lib.rs:55`), hands it to Core 0 at `lib.rs:148`, and **Core 4** (`m68k-sound-cpu`,
`lib.rs:250-289`) loads it with `Ordering::Acquire` at `lib.rs:264`, calling `m68k.reset()` on
the false→true edge and `m68k.stop()` on true→false. Covered by
`sh2.rs:2014-2034` (`sndon_sndoff_flip_the_m68k_control_flag`) and the cross-thread stress test
`saturn-core/tests/adversarial_tests.rs:369-431`. This handshake is **sound and needs no
work** beyond adding the missing OREG31 echoes.

Stale doc note: `lib.rs:50-54`'s comment says "Core 3 … owns the actual `M68k`". It is Core 4
(`lib.rs:250`). Worth correcting while in the file, not a plan item.

### 0.6 What the real BIOS actually does — live trace (empirical grounding)

Captured with `MIMAS_BOOT_WATCH_SECS=240 ./target/release/saturn-frontend-native --bios
<real BIOS>` (image md5 `3240872c70984b6cbfda1586cab68dbe`, 512 KiB) and the existing
`[REGACCESS]` recipe (`sh2.rs:242-257`). Every SMPC access, in order, over a full 240 s run:

| # | Offset | Register | Dir | Value |
|---|---|---|---|---|
| 1 | `0x1F` | COMREG | W | `0x1A` — **RESDISA** |
| 2 | `0x63` | SF | W | `0x01` — *the BIOS sets SF busy itself* |
| 3 | `0x01` | IREG0 | W | `0x01` — status block requested |
| 4 | `0x03` | IREG1 | W | `0x02` — **bit 3 clear: no peripheral data** |
| 5 | `0x05` | IREG2 | W | `0xF0` |
| 6 | `0x1F` | COMREG | W | `0x10` — **INTBACK** |
| 7 | `0x63` | SF | R | `0x00` — poll exits |
| 8 | `0x33` | **OREG9** | R | `0x01` (region) |
| 9 | `0x21` | **OREG0** | R | `0x80` |
| 10 | `0x61` | SR | R | `0x4F` |
| 11 | `0x1F` | COMREG | W | `0x19` — **RESENAB** |
| 12 | `0x79` | DDR1 | W | `0x00` |
| 13 | `0x7B` | DDR2 | W | `0x00` |
| 14 | `0x7D` | IOSEL | W | `0x00` |
| 15 | `0x7F` | EXLE | W | `0x00` |

Cross-checked against the BIOS ROM itself (`python3 tools/sh2dis.py`), which pins the exact
code:

- **BIOS `0x00000252-0x0000025C`** — the machine's *first* hardware access of any kind:
  `MOV.L @(0x24,PC),R3` loads the literal at `0x2E4` = `0x2010001F` (COMREG through the
  cache-through window), `MOV #26,R1` (`26 == 0x1A == RESDISA`), `MOV.B R1,@R3`. Mimas drops it
  (`sh2.rs:854-856`).
- **BIOS `0x00001D38-0x00001D66`** — the complete INTBACK boot handshake, verbatim:

  ```
  1D38  MOV.L @(0xc,PC),R3   ; R3 = 0x0036EE80  (3,600,000-iteration settle delay)
  1D3C  DT R3
  1D3E  BF 0x1D3C
  1D40  MOV.L @(0xb,PC),R1   ; R1 = 0x20100001  IREG0
  1D42  MOV.L @(0xc,PC),R5   ; R5 = 0x2010001F  COMREG
  1D44  MOV.L @(0xc,PC),R7   ; R7 = 0x20100063  SF
  1D46  MOV #1,R0
  1D48  MOV.B R0,@R7         ; SF    = 0x01
  1D4A  MOV #1,R0
  1D4C  MOV.B R0,@R1         ; IREG0 = 0x01
  1D4E  MOV #2,R0
  1D50  MOV.B R0,@(2,R1)     ; IREG1 = 0x02   (@0x20100003)
  1D52  MOV #-16,R0
  1D54  MOV.B R0,@(4,R1)     ; IREG2 = 0xF0   (@0x20100005)
  1D56  MOV #16,R0
  1D58  MOV.B R0,@R5         ; COMREG = 0x10  INTBACK
  1D5A  MOV #70,R4
  1D5C  DT R4
  1D5E  BF 0x1D5C            ; ~70-iteration inter-poll delay
  1D60  MOV.B @R7,R0         ; read SF
  1D62  TST #0x1,R0
  1D64  BF 0x1D5A            ; spin while SF bit 0 == 1
  1D66  RTS
  ```

  This independently confirms the entire §1.2 offset table (IREG0 `0x01`, IREG1 `0x03`,
  IREG2 `0x05`, COMREG `0x1F`, SF `0x63`) against real silicon-targeted code, and §2.1's
  handshake order.

Five conclusions that drive the phase ordering below:

1. **SMPC is not today's boot blocker.** Mimas's minimal INTBACK-status path already satisfies
   this BIOS revision's handshake; the run proceeds past it into VDP2 register initialisation
   and then CS2/CD-block polling (`Cs2Regs(0x90018)` and friends), finishing the 240 s window
   cycling in a CD header string-compare at BIOS `0x22F8-0x2310` / `0x42EC`. The next real wall
   is the CD block, not the SMPC. Phase ordering below therefore optimises for **fidelity +
   not regressing a working path**, not for "unblock boot today".
2. **The BIOS writes SF = 1 itself before every command** (trace #2, BIOS `0x1D48`) and polls
   it (BIOS `0x1D60-0x1D64`). Today this works *by accident* because SF's read is hardwired to
   `0x00` (`sh2.rs:487`). The moment SF becomes real storage without an unconditional
   "clear SF after every dispatched command" (§2.2, `smpc.c:628`) **and** an "unrecognised
   COMREG clears SF immediately" path (§2.2, `smpc.c:727`), this loop hangs forever. This is
   the single highest-risk change in the whole plan and must land as one atomic step.
3. **`IREG1 = 0x02`** ⇒ spec-correct `SR = 0x4F` (§5.3). Mimas writes `0x6F` (`sh2.rs:880`).
4. **The BIOS does exercise DDR1/DDR2/IOSEL/EXLE** (trace #12-15) — the §6 direct-access port
   is not dead surface, it is real boot-path code. It writes them all to `0x00`, which per §6.1
   selects control method `0x00` on both ports.
5. **Open question for a Phase 1 probe:** trace #10 reads SR and gets `0x4F`, but Mimas's
   INTBACK wrote `0x6F` (`sh2.rs:880`) and no SR *write* appears in the trace. Something
   clobbers SR between the INTBACK and that read. Diagnose with `CLAUDE.md`'s surgical-probe
   recipe: a temporary `eprintln!` in the `MemRegion::Smpc` write arm (`sh2.rs:582`) gated on
   `off == 0x61`, logging `self.pc`. Do **not** build SR semantics on top of this until it is
   explained.

### 0.7 The architectural question: where should SMPC logic live?

**Recommendation: extract a real `Smpc` type in `saturn-core/src/smpc.rs`, owned by
`SaturnSystem` as `Arc<Mutex<Smpc>>`, wired into `Sh2` through a new optional field + the
existing setter pattern. Keep register *storage* in `WorkRam::smpc_regs`. Keep *execution* on
the SH-2 thread for now; move the timer to Core 7 only in Phase 6.**

Why not keep growing it inline in `sh2.rs`:

- SMPC is about to acquire ~14 pieces of state that have nothing to do with a CPU: `resd`,
  `dotsel`, `mshnmi`, `sysres`, `sndres`, `cdres`, `regionid`, `regionsetting`, `SMEM[4]`,
  `clocksync`/`basetime`, `intback`, `firstPeri`, `bustmp`, plus two `PortData` snapshots with
  read cursors (§0.1). Bolting those onto `Sh2` makes the CPU struct the owner of peripheral
  state it has no business knowing about.
- The port data must be reachable by **two** parties: the SMPC command path (SH-2 thread today,
  Core 7 later) and the frontend's input feed (Phase 4). That requires a shared, lockable owner
  — `Arc<Mutex<Smpc>>` — which `Sh2`-private fields cannot be.
- Phase 6 wants the command timer on Core 7. If the state is already behind an `Arc<Mutex<_>>`,
  that becomes a scheduling change, not a rewrite.
- SMPC is genuinely testable in isolation (register file in, OREG bytes out). `Sh2`-private
  methods are only testable by driving a whole CPU.

Why not go straight to a Core-7-owned actor:

- Today's "COMREG write → command completes before the store retires" is precisely what makes
  the boot handshake in §0.6 work. Introducing cross-thread dispatch *and* real timing *and*
  real SF simultaneously is three risky changes at once. `CLAUDE.md`'s one-wall-at-a-time
  discipline applies.

Concrete shape, mirroring the **`scu_dsp` precedent exactly** (`sh2.rs:157-163`, `lib.rs:73`,
`lib.rs:151`, and the `read_scu_dsp_port`/`write_scu_dsp_port` interception at
`sh2.rs:709-744`):

- `saturn-core/src/smpc.rs`: `pub struct Smpc { … }` replacing the dead stub, holding **only
  non-register state** + the command semantics.
- `Sh2` gains `pub smpc: Option<Arc<Mutex<crate::smpc::Smpc>>>` (default `None`), set by plain
  field assignment in `SaturnSystem::start` next to `cpu.scu_dsp = Some(...)` at `lib.rs:151`.
  **`Sh2::new()`'s 3-argument signature is untouched** (`CLAUDE.md` stability constraint;
  `sh2.rs:260`). `None` ⇒ the existing inline fallback, so every bare-`Sh2` unit test keeps
  working unchanged during the transition.
- `SaturnSystem` gains `pub smpc: Arc<Mutex<Smpc>>` alongside `scu_dsp` (`lib.rs:73`).
- `Smpc` methods take `&WorkRam` (exactly like `ScuDsp::step(&work_ram)`, `lib.rs:332`) and do
  their own `smpc_regs` locking internally. **Lock order, documented once and enforced by that
  API: `Smpc` mutex first, then `WorkRam::smpc_regs`, never the reverse, and never call back
  into `Sh2` while holding either.** This is the first call site in the codebase needing two
  locks at once; `CLAUDE.md`'s memory-model note says to establish and document an order when
  that happens.
- `Smpc` must not depend on `Sh2`, `LockStepSync` or `BusArbiter`. Commands that have effects
  outside the SMPC return them:

  ```rust
  #[derive(Default, Debug, PartialEq, Eq)]
  pub struct SmpcEffects {
      pub start_slave: bool,      // SSHON  §4.3
      pub stop_slave: bool,       // SSHOFF §4.4 (reset, not just halt)
      pub sound_on: bool,         // SNDON  §4.5
      pub sound_off: bool,        // SNDOFF §4.6
      pub system_manager_irq: bool, // §2.3 — INTBACK only
      pub nmi: bool,              // NMIREQ §4.13, reset button §4.16
      pub system_reset: bool,     // SYSRES §4.8
      pub clock_change: Option<DotClock>, // CKCHG352/320 §4.9/§4.10
  }
  ```

  `sh2.rs`'s `MemRegion::Smpc` write arm applies them (`set_thread_active(1, …)`,
  `m68k_control.store(…, Release)`, `smpc_irq_pending = true`, …). That keeps every existing
  cross-thread handshake byte-for-byte identical while moving the *decision* into `Smpc`.

Test fallout to handle in Phase 0: `e2e-tests/src/lib.rs:4` imports `Smpc`;
`:234-236` (`test_tier1_f4_smpc_initialization`) and `:584-590`
(`test_tier2_f4_smpc_command_buffer_overflow`) exercise the stub's fictional
`write_command`/`execute_command`/`0x55`. Both assert nothing about real hardware and must be
replaced, not preserved. `cargo test --workspace` must stay green (`CLAUDE.md`).

---

## Phase 0 — Extract a real `Smpc` type (pure refactor, zero behavior change)

**Status: done.** See `history.md` Chapter 14.

Goal: move today's exact behavior out of `sh2.rs` and into `smpc.rs` behind
`Arc<Mutex<Smpc>>`, with the register file still in `WorkRam::smpc_regs`, so every later phase
edits one focused module. **No observable behavior may change in this phase.**

- [x] Replace `saturn-core/src/smpc.rs`'s stub entirely. New `Smpc` holds only non-register
      state; start with the fields today's code implies: none. Add `pub fn new() -> Self`.
- [x] Add the full offset constant table to `smpc.rs` (moving the five from `sh2.rs:39-49` and
      adding the rest), each derived from §1.2's "Register address = `0x00100000 + 2*index + 1`":
      `IREG0 0x01`, `IREG1 0x03`, `IREG2 0x05`, `IREG3 0x07`, `IREG4 0x09`, `IREG5 0x0B`,
      `IREG6 0x0D`, `COMREG 0x1F`, `OREG_BASE 0x21` (stride 2, `OREGn = 0x21 + 2n`, OREG31 =
      `0x5F`), `SR 0x61`, `SF 0x63`, `PDR1 0x75`, `PDR2 0x77`, `DDR1 0x79`, `DDR2 0x7B`,
      `IOSEL 0x7D`, `EXLE 0x7F`. Also `PADDING 0x0F..=0x1D` and `PADDING2 0x65..=0x73` (§1.2) as
      named comments so nobody re-derives them.
- [x] Add the full command-ID constant table (§3.4): `MSHON 0x00`, `SSHON 0x02`, `SSHOFF 0x03`,
      `SNDON 0x06`, `SNDOFF 0x07`, `CDON 0x08`, `CDOFF 0x09`, `SYSRES 0x0D`, `CKCHG352 0x0E`,
      `CKCHG320 0x0F`, `INTBACK 0x10`, `SETSMEM 0x17`, `NMIREQ 0x18`, `RESENAB 0x19`,
      `RESDISA 0x1A`.
- [x] Add the region constant table (§0.3): `AUTODETECT 0`, `JAPAN 1`, `ASIA_NTSC 2`,
      `NORTH_AMERICA 4`, `CENTRAL_SOUTH_AMERICA_NTSC 5`, `KOREA 6`, `ASIA_PAL 0xA`,
      `EUROPE 0xC`, `CENTRAL_SOUTH_AMERICA_PAL 0xD`.
- [x] Add `SmpcEffects` (shape in §0.7) and
      `pub fn execute_command(&mut self, cmd: u8, work_ram: &WorkRam) -> SmpcEffects`, porting
      `sh2.rs:826-888` verbatim — including, for now, the wrong `OREG0 = 0x80` and
      `SR = 0x6F`, so this phase is provably behavior-preserving.
- [x] Delete the dead `wants_peripheral` local (`sh2.rs:859`) or make it load-bearing in
      Phase 1; either way the `unused_variable` warning must be gone.
- [x] Add `pub smpc: Option<Arc<Mutex<Smpc>>>` to `Sh2` (default `None` in `Sh2::new`,
      `sh2.rs:260-299`). **Do not touch the constructor signature.**
- [x] Rewrite the `MemRegion::Smpc` write arm (`sh2.rs:582-591`): store the byte, then if
      `off == COMREG` either call through `self.smpc` and apply the returned `SmpcEffects`, or
      (when `None`) run the existing inline fallback.
- [x] Add `pub smpc: Arc<Mutex<Smpc>>` to `SaturnSystem` (`lib.rs:32-75`), construct in
      `SaturnSystem::new`, and wire `cpu.smpc = Some(smpc_c0)` in Core 0's closure next to
      `cpu.scu_dsp = Some(scu_dsp_c0)` (`lib.rs:151`).
- [x] Delete `e2e-tests/src/lib.rs:234-236` and `:584-590`; replace with a real test (below).
- [x] Update `lib.rs:8`/`lib.rs:22` re-exports as needed; correct the stale "Core 3 owns the
      M68k" comment at `lib.rs:50-54` → Core 4.
- [x] Update `CLAUDE.md`'s "Known architecture debt" bullet that says `smpc.rs` is dead code.

**Testing (Phase 0)**

- [x] `cargo test --workspace` green, with **no test's expected values changed** — the whole
      point of this phase. `sh2.rs:1950-2034`'s four SMPC tests and
      `sh2.rs:2305-2313` must pass untouched.
- [x] New `smpc.rs` unit test: driving `Smpc::execute_command(0x10, &work_ram)` directly
      produces byte-identical `smpc_regs` contents to driving
      `cpu.write_byte(0x0010_001F, 0x10)` on a bare `Sh2`. This is the refactor's own proof.
- [x] Replacement e2e test for the two deleted ones: `SaturnSystem::new()` exposes a
      `smpc` handle whose freshly constructed register view is all-zero (§0.4 step 1:
      `SmpcReset` zeroes all 64 bytes).

---

## Phase 1 — Register-file discipline, the SF handshake, and the commands the real BIOS issues

**Status: done, except the one item below flagged as deliberately deferred.** See `history.md`
Chapter 14.

Highest-value phase: it makes the path the real BIOS already walks (§0.6) faithful, and adds
the three commands the trace proves the BIOS issues that Mimas silently drops (RESDISA,
RESENAB, and correct INTBACK status bits).

**Land the SF change and the "clear SF on dispatch" change in a single commit.** Splitting them
hangs the BIOS at `0x1D5A` (§0.6 conclusion 2).

- [x] Add `Smpc` state: `resd: bool` (**`true` at reset**, §0.4 step 4), `bustmp: u8`.
- [x] **SF write** (§1.3): a byte write to `0x63` sets SF to the written value verbatim. Note
      §1.3's `[DEAD]` finding — Yabause's `SF &= val` is a no-op; do not port the mask.
- [x] **SF read** (§1.3): return `(bustmp & 0xFE) | (SF & 1)`, and apply
      `bustmp = (bustmp & !1) | (SF & 1)` as part of the read, matching the bus-hold model.
      Replaces the hardcoded `0x00` at `sh2.rs:487`.
- [x] **`bustmp` update**: every byte write to *any* SMPC offset sets `bustmp = val` (§1.3,
      `smpc.c:756`) — including OREG/SR/padding writes.
- [x] **Clear SF after every dispatched command** (§2.2, `smpc.c:628`) — unconditionally,
      including for commands whose handler does nothing.
- [x] **Unrecognised COMREG clears SF immediately and does not dispatch** (§2.2,
      `smpc.c:727`). Log once via the existing `log_reg_access_once`-style dedup rather than
      per-write spam.
- [x] **`0x19` RESENAB** (§4.14): `resd = false`, `OREG31 = 0x19`.
- [x] **`0x1A` RESDISA** (§4.15): `resd = true`, `OREG31 = 0x1A`.
- [x] **OREG31 command echoes** (§4.1) for exactly: SNDON `0x06`, SNDOFF `0x07`, INTBACK
      `0x10`, SETSMEM `0x17`, NMIREQ `0x18`, RESENAB `0x19`, RESDISA `0x1A`. Explicitly **not**
      for SSHON, SSHOFF, CKCHG320, CKCHG352, MSHON, CDON, CDOFF, SYSRES.
- [x] **Fix `OREG0`** (§5.6): `0x80 | ((resd as u8) << 6)`. Replaces `sh2.rs:867`'s hardcoded
      `0x80`.
- [x] **Fix `SR` for the status path** (§5.3): `0x4F | (intback << 5)` where
      `intback = (IREG1 >> 3) & 1`. Replaces `sh2.rs:880`'s hardcoded `0x6F`. Makes
      `wants_peripheral` (`sh2.rs:859`) load-bearing at last.
- [x] **Decode IREG0** (§5.2): `IREG0 & 1` selects the status path; `IREG0 & 1 == 0` with
      `IREG1 & 8 == 0` is a genuine no-op (the fall-through at `smpc.c:499`). No continuation
      machine yet — that is Phase 4.
- [x] **Write `OREG31 = 0x10`** on the INTBACK status path (§5.6).
- [ ] **`0x03` SSHOFF must reset the slave**, not just deactivate it (§4.4: "fully reset, not
      merely halted"). Today `sh2.rs:833-838` only calls `set_thread_active(1, false)`; Core 1
      (`lib.rs:163-177`) does `Sh2::new` + `reset()` on *first* activation only, inside
      `park_while_inactive`. Restructure Core 1's loop so a re-activation re-enters
      `reset()`, mirroring Core 6's re-park loop shape (`lib.rs:320-341`).
      **Deliberately deferred**: investigating this surfaced that `Sh2::run_loop` never checks
      whether its own core is still active at all (it only checks the global `shutdown` flag),
      so a deactivated Core 1 keeps executing instructions today regardless of `SmpcEffects::
      stop_slave` — a deeper, pre-existing concurrency gap than this checklist item assumed.
      Left for a dedicated session rather than folded into a register-level fidelity pass; see
      `history.md` Chapter 14.
- [ ] Run the §0.6 probe for the unexplained `SR R 0x4F` before trusting any SR assertion.
      **Not run as a dedicated probe** — superseded in practice: the real-BIOS smoke test after
      this phase landed showed the wired-in `Smpc` path reading back exactly `0x4F`, matching
      both this section's formula and the real captured fixture
      (`saturn-core/tests/fixtures/smpc_intback_status.bin`).
- [ ] Re-verify the `sh2.rs:875-879` "PDE / boot loop at 0x338C" claim against a real boot run
      after the `0x4F` change. If a BIOS revision genuinely needs `0x6F` for `IREG1 = 0x02`,
      that is a *different* bug (most likely a missing System Manager IRQ edge or a missing
      OREG) and must be root-caused, not papered over again.
      **Not re-verified** — that comment lives in the old inline fallback path (`Sh2::
      smpc_execute_command`, only reachable when no `Smpc` is wired in), which real, running
      systems no longer use at all once `Sh2::smpc` is `Some(_)`.

**Testing (Phase 1)** — every expected value below is hand-derived from the cited section, not
read off the current implementation.

- [x] `oreg0_reports_reset_disable_state`: three-point derivation from §5.6 (`0x80 | (resd<<6)`)
      × §0.4 step 4 (`resd = 1` at reset) × §4.14/§4.15.
      Fresh `Smpc` → INTBACK status → **OREG0 (`0x21`) == `0xC0`**.
      Then COMREG `0x19` (RESENAB) → INTBACK → **`0x80`**.
      Then COMREG `0x1A` (RESDISA) → INTBACK → **`0xC0`**.
      Today's code returns `0x80` in all three cases, so this test fails before the fix.
- [x] `intback_status_sr_tracks_ireg1_bit3`: §5.3, `0x4F | (intback<<5)`.
      `IREG1 = 0x02` → **SR (`0x61`) == `0x4F`** (this is the value the *real BIOS* asks for,
      §0.6 #4). `IREG1 = 0x0A` → **`0x6F`**.
- [x] `oreg31_echo_matrix`: §4.1. Assert `OREG31` (`0x5F`) equals the command byte for
      `0x06, 0x07, 0x10, 0x17, 0x18, 0x19, 0x1A`, and is left **unchanged** (pre-seed it with a
      sentinel like `0x5A`) for `0x02, 0x03, 0x0E, 0x0F, 0x00, 0x08, 0x09, 0x0D`.
- [x] `sf_handshake_matches_real_bios_sequence`: replay §0.6's BIOS `0x1D48-0x1D64` byte for
      byte — write SF=`0x01`, IREG0=`0x01`, IREG1=`0x02`, IREG2=`0xF0`, COMREG=`0x10`, then
      read SF and assert **bit 0 is clear**. This is the exact loop that hangs the machine if
      "clear SF after dispatch" is missing.
- [x] `sf_read_returns_bustmp_high_bits`: §1.3. Write `0xF0` to a padding offset (`0x0F`) →
      `bustmp = 0xF0`. Write `0x01` to SF. Read SF → **`0xF1`** (`(0xF0 & 0xFE) | 1`).
- [x] `unrecognised_comreg_clears_sf_without_dispatching`: §2.2. SF=`0x01`, COMREG=`0x05`
      (§3.4: no handler), then SF reads **bit 0 clear**, and OREG0 is **unchanged** from its
      pre-seeded sentinel.
- [ ] `sshoff_resets_the_slave`: assert the slave core re-enters `reset()` on the next SSHON
      (observe via its PC returning to the reset vector), not merely un-parks mid-stream.
      **Deferred along with the feature itself** (see above).
- [x] Real-BIOS smoke: re-run the §0.6 capture and diff the `[REGACCESS]` SMPC sequence. It
      must still reach trace lines #11-#15 (RESENAB, DDR1/DDR2/IOSEL/EXLE) — i.e. the boot did
      not regress behind where it is today.

---

## Phase 2 — Complete the INTBACK status block: RTC, SMEM, region, system flags

**Status: done, with two intentional simplifications noted inline below.** See `history.md`
Chapter 14.

Everything §5.6 specifies that Phase 1 did not cover. None of this is known to gate boot on the
traced revision (it reads only OREG9 and OREG0, §0.6 #8/#9), but a status block that is 15
bytes of stale garbage plus 7 bytes of zeroed RTC is a latent failure for any BIOS/game that
reads the clock or SMEM.

- [x] Add `Smpc` state: `smem: [u8; 4]`, `regionid: u8`, `regionsetting: u8`, `dotsel: bool`,
      `mshnmi: bool`, `sysres: bool`, `sndres: bool`, `cdres: bool`, `clock: ClockSource`.
      **Simplification**: no separate `regionsetting` field was added — `regionid` alone (with
      the `AUTODETECT`→`JAPAN` fallback applied at OREG9-write time) covers every behavior this
      phase's tests actually exercise. Revisit if a later phase needs to distinguish "what was
      configured" from "what was reported."
- [x] **Reset state** per §0.4: zero all 64 register bytes; `SMEM = [0, 0, 0, syslanguageid]`
      (note §0.4 step 2's double-`memset` derivation — the net effect is *not* four copies of
      the language id); `resd = true`; every other flag `false`; `intback = firstPeri = false`.
- [x] **`ClockSource`** enum with two variants, mirroring §7.1's `clocksync` but named honestly:
      `HostWallClock` (re-read on every INTBACK) and `Fixed(u64 /* unix seconds */)` for
      deterministic tests and future deterministic replay. Skip Yabause's
      `basetime + frame_count * 1001/60000` formula for now — §7.1 flags it as NTSC-only and
      ~20 % slow in PAL; if a deterministic *advancing* clock is wanted later, derive it from
      the real frame period per video mode, not from that constant.
- [x] **Decide and document UTC vs. local time.** Yabause uses `localtime_r` (§7.1). A host
      timezone dependency makes every RTC test non-reproducible. Recommend **UTC**, stated
      explicitly as a deliberate deviation in the module doc comment, per `CLAUDE.md`'s
      "state simplifications honestly" rule.
- [x] **OREG1** (`0x23`) — year thousands/hundreds BCD: `((y / 1000) << 4) | ((y % 1000) / 100)`
      (§5.6).
- [x] **OREG2** (`0x25`) — year tens/units BCD: `(((y % 100) / 10) << 4) | (y % 10)` (§5.6).
- [x] **OREG3** (`0x27`) — `(weekday << 4) | (month)`, weekday **0 = Sunday**, month **1-12**,
      **not BCD** (§5.6 explicitly: months 10-12 appear as nibbles `0xA`-`0xC`).
- [x] **OREG4** (`0x29`) day-of-month BCD, **OREG5** (`0x2B`) hour 24 h BCD, **OREG6** (`0x2D`)
      minute BCD, **OREG7** (`0x2F`) second BCD (§5.6).
- [x] **OREG8** (`0x31`) cartridge code. §5.6 records Yabause's hardcoded `0` with a
      `// FIXME : random value`. Keep `0` **and say so in a comment** until a real cartridge
      model exists — that is the honest simplification, not a guess.
- [x] **OREG9** (`0x33`) `regionid`, from the region table added in Phase 0. Add
      `Smpc::set_region(u8)` and a `SaturnSystem::set_region` passthrough so a frontend can
      choose; keep `JAPAN (1)` as the no-CD fallback (§0.2 `SmpcRecheckRegion`), which is what
      `sh2.rs:872` already effectively does. Autodetect-from-CD is deferred until the CD block
      is integrated at all (it currently is not — `CLAUDE.md`).
      **Partial**: `Smpc::set_region(u8)` exists and is tested; the `SaturnSystem::set_region`
      frontend passthrough does not yet exist (no frontend needs it yet either). Small,
      mechanical follow-up when one does.
- [ ] **OREG10** (`0x35`): `0x34 | ((dotsel as u8) << 6) | ((mshnmi as u8) << 3) |
      ((sysres as u8) << 1) | (sndres as u8)` (§5.6 — bits 5, 4, 2 hard-wired 1, bit 7
      hard-wired 0).
- [ ] **OREG11** (`0x37`): `(cdres as u8) << 6` (§5.6).
- [x] **OREG12-15** (`0x39`, `0x3B`, `0x3D`, `0x3F`) ← `SMEM[0..3]` (§5.6, §7.3).
- [x] **OREG16-30** (`0x41`…`0x5D`): §5.6 tags Yabause leaving these stale as `[QUIRK]` #20 —
      "15 bytes of stale data where hardware would supply defined values". Since the defined
      values are unknown from this source, **zero them** and comment that this is a deliberate
      divergence chosen over propagating garbage. Revisit if a BIOS is found to depend on it.
- [x] **`0x17` SETSMEM** (§4.12): copy IREG0..IREG3 (`0x01, 0x03, 0x05, 0x07`) into `SMEM[0..3]`;
      `OREG31 = 0x17`. No validity check (§4.12). SMEM is not persisted to disk (§7.3) — it
      lives in `Smpc` only; note that in the doc comment.

**Testing (Phase 2)**

- [x] `intback_rtc_bcd_layout`: drive `ClockSource::Fixed(946_684_800)` (UNIX epoch for
      **2000-01-01 00:00:00 UTC**, a **Saturday** ⇒ `tm_wday = 6`). Hand-derived from §5.6's
      formulas: **OREG1 == `0x20`** (`(2000/1000)<<4 | (2000%1000)/100` = `2<<4 | 0`),
      **OREG2 == `0x00`**, **OREG3 == `0x61`** (`6<<4 | (0+1)`), **OREG4 == `0x01`**,
      **OREG5 == `0x00`**, **OREG6 == `0x00`**, **OREG7 == `0x00`**.
      Add a second timestamp that exercises the awkward cases: a month ≥ 10 (asserting the
      nibble is `0xA`-`0xC`, *not* BCD-carrying) and a two-digit day/hour/minute/second so the
      BCD packing is actually tested (e.g. 2001-12-25 13:45:59 → OREG3 low nibble `0xC`,
      OREG4 `0x25`, OREG5 `0x13`, OREG6 `0x45`, OREG7 `0x59`).
- [x] `oreg10_encodes_dot_clock`: §5.6. All flags clear → **`0x34`**. With `dotsel = true`
      (i.e. after CKCHG352, Phase 3) → **`0x74`**.
- [x] `setsmem_round_trips_through_oreg12_15`: write IREG0-3 = `DE AD BE EF`, COMREG `0x17`,
      assert `OREG31 == 0x17`; then INTBACK status and assert OREG12-15 (`0x39`, `0x3B`,
      `0x3D`, `0x3F`) == `DE AD BE EF`.
- [x] `smem_reset_state`: after `Smpc::reset()` with language id `5` (Japanese, §0.4 step 2's
      comment block), SMEM must be `[0x00, 0x00, 0x00, 0x05]` — **not** `[5,5,5,5]`. This is
      exactly the kind of double-`memset` detail a plausible-looking reimplementation gets
      wrong.
- [x] `region_reported_in_oreg9`: set each of the nine §0.3 constants, assert OREG9 matches,
      and assert `AUTODETECT (0)` with no CD present falls back to `JAPAN (1)`.

---

## Phase 3 — The remaining commands: NMIREQ, MSHON, CDON/CDOFF, SYSRES, CKCHG320/352

Fills out the §3.4 dispatch table. Ordered after Phase 2 because none of these appear in the
§0.6 boot trace, but before peripherals because they are small, self-contained, and CKCHG is
already anticipated in the code (`throttle.rs:18`).

- [x] **`0x18` NMIREQ** (§4.13): raise the SH-2 NMI — **vector `0x0B`, level `16`**;
      `OREG31 = 0x18`. §4.13 flags a `[QUIRK]`: Yabause does *not* set `ICR` bit 15 here (unlike
      the CKCHG path, §4.9). Real hardware's NMI does. Recommend **setting ICR bit 15**, and
      documenting the divergence from the reference.
- [x] **`Sh2` NMI plumbing**: add `pub nmi_pending: bool` and give it top priority in
      `service_pending_interrupt` (`sh2.rs:908-952`), above `VBLANK_IN_LEVEL`.
      **Watch out for a real bug this creates**: `sh2.rs:950` does
      `self.sr = (self.sr & !(0xF << SR_IMASK_SHIFT)) | (level << SR_IMASK_SHIFT)`. With
      `level = 16`, `16 << 4` overflows the 4-bit mask field and writes **`0`** — the exact
      opposite of masking. Clamp the value written into the mask field to `15` while keeping
      `16` as the acceptance-comparison level, and add a regression test for it.
- [x] **Reset button** (§4.16): `SaturnSystem::press_reset_button()` → no-op when `resd`
      (§4.16: inert until the game issues RESENAB, because `resd = 1` at reset), else the same
      NMI path as NMIREQ. Wire nothing in the frontends yet; the API is the deliverable.
- [x] **`0x00` MSHON** (§4.2): no inputs, no outputs, no OREG31. Real hardware turns the master
      SH-2 on; there is no state in which Mimas's master is off. Implement as an explicit,
      commented accepted-no-op — **not** a silent fall-through — so it is distinguishable from
      an unimplemented command in the log.
- [x] **`0x08` CDON / `0x09` CDOFF** (§4.7): accepted no-ops with a comment pointing at
      `docs/implementation-plans/cs2-cdblock.md`; revisit when the CD block is integrated (it
      currently is not wired into the system at all — `CLAUDE.md`). No OREG31 (§4.1).
- [x] **`0x0D` SYSRES** (§4.8): §4.8 records that Yabause does nothing here. Real hardware
      performs a full system reset. Implement `SmpcEffects::system_reset` and have `sh2.rs`
      re-run `Sh2::reset()` (`sh2.rs:317-336`) on the master; leave the wider system reset
      (VDPs, SCU, SCSP) to whenever those gain reset entry points. No OREG31 (§4.1).
- [x] **`0x0E` CKCHG352 / `0x0F` CKCHG320** (§4.9, §4.10), in §4.9's exact order:
      1. reset VDP1, VDP2, SCU, SCSP (as those reset entry points exist; stub + `TODO` where
         they do not, listing which);
      2. §4.9 notes the "Clear VDP1/VDP2 ram" comment is **not implemented** in the reference —
         do not implement it on that basis alone;
      3. stop the slave (same reset semantics as SSHOFF, Phase 1);
      4. change the SH-2 clock: `CLKTYPE_28MHZ` = `freq_base * 1.0`, `CLKTYPE_26MHZ` =
         `freq_base * 15/16`, with `freq_base` = **28.63636 MHz NTSC / 28.4375 MHz PAL** (§4.9);
      5. `dotsel = true` (352) / `false` (320);
      6. NMI the master (with ICR bit 15, §4.9).
      No OREG31 (§4.1).
- [x] **Clock plumbing**: `throttle.rs:18` already flags this as the reason `SH2_CLOCK_HZ` is a
      constant. Turn the SH-2 rate into live state the `ClockThrottle` reads (it already holds
      `Arc<Mutex<ThrottleSpeed>>`, `sh2.rs:156`, `lib.rs:68`) rather than a `const`. Also
      thread `dotsel` to VDP2's dot-clock selection when VDP2 gains one.

**Testing (Phase 3)**

- [x] `nmireq_enters_vector_0x0b`: mirror `sh2.rs:1969-2003`'s structure — set VBR, seed
      `[VBR + 0x0B*4]`, issue COMREG `0x18`, step, assert PC entered the handler and
      `OREG31 == 0x18`.
- [x] `nmi_mask_field_does_not_wrap_to_zero`: the `16 << 4` bug above. After NMI entry, assert
      `(sr >> 4) & 0xF == 15`, **not** `0`. Derived from the SH-2 SR layout (`sh2.rs:182-184`),
      independent of the SMPC.
- [x] `reset_button_is_inert_until_resenab`: §4.16 + §0.4 step 4. Fresh system → press → no NMI.
      COMREG `0x19` (RESENAB) → press → NMI. COMREG `0x1A` (RESDISA) → press → no NMI.
- [x] `ckchg352_sets_dotsel_and_ckchg320_clears_it`: assert via **OREG10** on a following
      INTBACK: `0x74` after `0x0E`, `0x34` after `0x0F` (§5.6 formula, hand-derived in Phase 2).
- [x] `ckchg_stops_the_slave`: SSHON, then CKCHG352, assert Core 1 is inactive/reset (§4.9
      step 3).
- [x] `ckchg_clock_rates`: assert the throttle's target Hz becomes `28_636_360` after CKCHG352
      and `28_636_360 * 15 / 16 == 26_846_587` (integer, ±1) after CKCHG320 — derived from
      §4.9's `freq_base`/`freq_mult`, not from `throttle.rs`'s current constant.

---

## Phase 4 — Peripherals: the digital pad, the INTBACK peripheral path, and frontend input

The single largest missing feature. Nothing in the workspace can express "a button is pressed"
today. This phase is deliberately scoped to **one directly-connected digital pad per port** —
the 95 % case — with the report-building code shaped so §9.3's other types drop in at Phase 7.

### 4a — The peripheral data model (in `saturn-core`)

- [x] `saturn-core/src/peripheral.rs` (new module, `pub mod peripheral;` in `lib.rs`).
- [x] `PortData { data: [u8; 256], size: usize, offset: usize }` — §0.1's `PortData_struct`. The
      `offset` cursor is the chunker's read position (§5.4).
- [x] `PeripheralId` constants with their data-byte counts from `ID & 0x0F` (§5.5):
      `PAD 0x02` (2), `WHEEL 0x13` (3), `MISSION_STICK 0x15` (5), `PAD_3D 0x16` (6),
      `TWIN_STICKS 0x19` (9), `GUN 0x25` (5), `KEYBOARD 0x34` (4), `MOUSE 0xE3` (3),
      `EMPTY_TAP_SLOT 0xFF` (written as a **single** byte, §5.5).
- [x] Port status bytes (§5.5): `NOT_CONNECTED 0xF0` (size 1), `DIRECT 0xF1`, `GUN_DIRECT 0xA0`
      (low nibble 0 ⇒ no entries follow, size forced to 1), `MULTITAP_6 0x16`.
      §5.5 also lists status values the reference *never produces* (`0x04` Sega-tap,
      `0x21`-`0x2F` clock-serial) — record them as constants with a comment, do not synthesise.
- [x] `PadState` — the frontend-facing type. A plain `bool`-per-button struct (or a bitflags
      `u16`), **not** the wire format. Buttons: `up, down, left, right, start, a, b, c, x, y,
      z, l, r` (13, matching §9.8's `perpadbaseconfig[13]`).
- [x] `PadState::to_report_bytes(&self) -> [u8; 2]` implementing §9.4's **active-low** layout
      exactly:
      - byte 0 bit 7 Right, 6 Left, 5 Down, 4 Up, 3 Start, 2 A, 1 C, 0 B;
      - byte 1 bit 7 R-trigger, 6 X, 5 Y, 4 Z, 3 L-trigger, bits 2:0 **unused, always 1**
        (§9.4: initialised to 1 and never touched);
      - a released button is `1`, a pressed button is `0`; idle is `FF FF` (§9.3).
      Note the **non-obvious ordering** in byte 0: `Start, A, C, B` — `C` before `B`. Getting
      this from intuition rather than §9.4 is exactly how this goes wrong.
- [x] `PeripheralPorts { port1: PortData, port2: PortData }` owned by `Smpc`, plus the two
      INTBACK snapshots `snap1`/`snap2` (§0.1).
- [x] `PeripheralPorts::rebuild(port, config)` producing the byte stream per §9.2/§9.3. For
      Phase 4: nothing connected → `[0xF0]`, size 1 (§9.3 / `PerPortReset`); one pad → `[0xF1,
      0x02, b0, b1]`, size 4.
- [x] `Smpc::set_pad_state(port: usize, state: PadState)` — writes the two live report bytes
      in place. This is the **only** mutation path the frontend gets.

### 4b — The INTBACK peripheral path

- [x] `Smpc` state: `intback: bool`, `first_peri: bool` (§0.1).
- [x] **`SmpcINTBACKPeripheral` equivalent** (§5.4), in order:
      1. **SR** (§5.3): `0xC0 | (IREG1 >> 4)` when `first_peri`, else `0x80 | (IREG1 >> 4)`;
         then `first_peri = false`.
      2. **Snapshot**, only when *both* snapshots are drained (`snap1.size == 0 &&
         snap2.size == 0`): copy the live ports into the snapshots, reset both cursors to 0,
         and run the mouse-delta flush (Phase 7; a no-op with only pads).
         Do **not** port `LagFrameFlag` (§5.4 `[HACK]` — TAS bookkeeping, not hardware).
      3. **Copy port 1** into OREG0.. up to 32 bytes, advancing `offset`; mark drained when
         exhausted.
      4. **Copy port 2** into the remaining OREGs.
      **Fix §5.4's `[BUG]` #15 while implementing**: copy from `data[offset..]`, not `data[..]`.
      Yabause re-sends the first 32 bytes on chunks 2+; that is a defect, not behavior to
      reproduce.
      **Fix §5.4's `[QUIRK]`** where `port2.size` is not cleared on an exact-fit boundary,
      forcing a spurious empty continuation round.
- [x] **Peripheral-only INTBACK path** (§5.2, `IREG0 & 1 == 0 && IREG1 & 8 != 0`):
      `first_peri = true`, `intback = true`, run the chunker, raise the System Manager IRQ.
      **Do not port `OREG[31] = 0x10`** here — §5.4 `[BUG]` #16: it clobbers the 32nd byte of
      real peripheral data and is applied inconsistently (not on continuations). Also skip
      §5.2's `SR = 0x40` dead store (`[DEAD]` #18).
- [x] **Status-then-peripherals path** (§5.2, `IREG0 & 1 == 1`): `first_peri = true`,
      `intback = (IREG1 & 8) != 0`, emit the status block, `SR = 0x4F | (intback << 5)`, raise
      the IRQ. Already partly in place from Phase 1; this adds the `intback` latch.
- [x] **IREG0 write-side continue/break decode** (§5.2, `smpc.c:760-776`) — in the *register
      write* path, not the command dispatcher, and only while `intback` is set:
      - bit 6 (`0x40`) → **break**: `intback = false`, `SR &= 0x0F`;
      - else bit 7 (`0x80`) → **continue**: internally synthesise `COMREG = 0x10`, arm the
        timing, `SF = 1`.
      Break is tested **first**, so `IREG0 = 0xC0` breaks (§5.2).
- [x] **`SmpcINTBACKEnd` at V-Blank IN** (§5.2): `intback = false`. Hook where VBLANK-IN is
      raised (`sh2.rs:1691`).
      **Fix §5.4's `[BUG]` #17 while doing it**: also clear `snap1.size`/`snap2.size`, so a
      partially drained frame-old snapshot cannot be resumed by the next INTBACK. Do the same
      on break.
- [x] §5.3 `[QUIRK]` #19 (SR carries no "last chunk" bit) is **real hardware behavior** — the
      game infers exhaustion from the port-status/size bytes. Do not invent a bit.

### 4c — Where input comes from (crosses the `saturn-core` / `saturn-frontend-*` boundary)

Design decision: **the frontend pushes the latest pad state into `saturn-core`; `saturn-core`
latches it at INTBACK time.** Justification: §5.4 step 2 *is* a latch — the SMPC snapshots port
data when a peripheral sequence begins. A push-latest model matches that exactly; a channel
would need draining, could backlog, and has no natural "latest wins" semantics.

- [x] `SaturnSystem::set_pad_state(port: usize, state: PadState)` — locks `Arc<Mutex<Smpc>>`,
      calls `Smpc::set_pad_state`, returns. Non-blocking, safe to call from the frontend's
      present loop at 60 Hz. The mutex is contended only against INTBACK (≤ a few times per
      frame), so it will not show up in a profile.
- [x] `SaturnSystem::set_port_peripheral(port: usize, kind: Option<PeripheralKind>)` — connect /
      disconnect, rebuilding the port report (§9.2). Default: pad on port 1, nothing on port 2.
- [x] **Rejected alternative, recorded so it is not re-litigated:** `ArcSwap<PortsState>`
      (the pattern used for `vdp2_frame`, `lib.rs:49`). Cheaper, but the mouse-delta flush
      (§9.6) is a read-modify-write on the same state at snapshot time, which `ArcSwap` makes
      racy. A `Mutex` at 60 Hz is the right trade.
- [x] **Rejected alternative:** putting `PadState` in `WorkRam`. `WorkRam` models *physical
      memory regions* (`shared_buffers.rs:1-61`); controller state is not memory.
- [x] `saturn-frontend-native/src/bin/mimas_window.rs`: the `minifb` loop already runs at
      `mimas_window.rs:69`. Map keys → `PadState` and call `set_pad_state` once per present
      iteration. Suggested default binding (arrows = D-pad; `Z/X/C` = A/B/C; `A/S/D` = X/Y/Z;
      `Q/W` = L/R; `Enter` = Start) — document it in the file, not here.
- [x] `saturn-frontend-libretro/src/lib.rs`: currently 5 lines of stubs. When it is built out,
      `retro_run` calls `input_poll_cb()` then `input_state_cb(port, RETRO_DEVICE_JOYPAD, 0,
      id)` for each of the 13 buttons and feeds the same `set_pad_state`. §9.8's note that
      libretro's key namespace is `(player << 8) | control_id` is a Yabause implementation
      detail; Mimas's `(port, PadState)` API does not need it.
- [x] Deliberately **not** ported: §9.8's whole `PerBaseConfig_struct`/`perkeyconfig` callback
      registry. It is a host-key-binding layer (with an uninitialised-memory bug, §9.8
      `[BUG]` #45) that belongs in a frontend, not in `saturn-core`.

**Testing (Phase 4)** — pad bit layouts hand-derived from §9.4, report layouts from §5.5's
worked examples.

- [x] `pad_report_bytes_idle`: all buttons released → **`[0xFF, 0xFF]`** (§9.3's `PERPAD`
      initial values).
- [x] `pad_report_bytes_a_and_right`: §9.4 — Right is bit 7 (`& 0x7F`), A is bit 2 (`& 0xFB`).
      Byte 0 = `0xFF & 0x7F & 0xFB` = **`0x7B`**; byte 1 = **`0xFF`**.
- [x] `pad_report_bytes_start_and_l`: Start is byte 0 bit 3, L-trigger is byte 1 bit 3 →
      **`[0xF7, 0xF7]`**.
- [x] `pad_report_unused_bits_stay_set`: press every one of the 13 buttons → byte 1 must be
      **`0x07`** (bits 7-3 cleared, bits 2:0 still 1 per §9.4), byte 0 **`0x00`**.
- [x] `intback_peripheral_one_pad_port1`: §5.5's worked example verbatim. One idle digital pad
      on port 1, nothing on port 2, `IREG0 = 0x00`, `IREG1 = 0x08` → OREG0..OREG4
      (`0x21, 0x23, 0x25, 0x27, 0x29`) = **`F1 02 FF FF F0`**, and `port1.size == 4`.
- [x] `intback_peripheral_both_ports_empty`: → OREG0 = **`0xF0`**, OREG1 = **`0xF0`** (§5.5).
- [x] `intback_peripheral_sr_first_vs_subsequent`: §5.3 with `IREG1 = 0x5A` (bit 3 set, high
      nibble `0x5`) → first chunk **SR == `0xC5`** (`0xC0 | (0x5A >> 4)`), a continuation →
      **`0x85`**.
- [x] `intback_break_clears_sr_high_nibble`: §5.2 — mid-sequence write `IREG0 = 0x40`, assert
      `SR == (previous SR) & 0x0F` and that `intback` is cleared. Then `IREG0 = 0xC0` (both
      bits) must also **break**, not continue (§5.2: break is tested first).
- [x] `intback_multi_chunk_does_not_resend_first_32_bytes`: build a >32-byte port 1 report
      (six pads on a tap, Phase 7 shape, or a hand-constructed `PortData`), run one chunk plus
      one continuation, and assert the second chunk starts at report byte 32. **This test fails
      against a faithful port of the reference** — that is the point (§5.4 `[BUG]` #15).
- [x] `vblank_abandons_an_in_progress_intback_and_drops_the_snapshot`: §5.2 + §5.4 `[BUG]` #17.
- [ ] Cross-crate smoke: `SaturnSystem::set_pad_state(0, PadState { a: true, .. })`, drive an
      INTBACK from a real `Sh2`, and read OREG2 back through `Sh2::read_byte(0x00100025)`.
      Proves the whole frontend → `Smpc` → `WorkRam::smpc_regs` → CPU path.

---

## Phase 5 — The direct-access port: PDR1/PDR2, DDR1/DDR2, IOSEL, EXLE

§6 in full. Promoted above timing/threading because the §0.6 trace proves the real BIOS writes
all four of DDR1/DDR2/IOSEL/EXLE during boot (trace #12-15), so this is live boot-path surface,
not speculative game-compat work.

- [x] **Control-method selection** (§6.1): every PDR/DDR write switches on `DDR[n] & 0x7F`.
      Recognised: `0x00` (all-input), `0x40` (TH control / acquire ID), `0x60` (TH-TR control).
      Anything else: log once, no synthesis. Note §6.1/§10.2's honest framing — Yabause treats
      DDR as a 7-bit **mode enum**, not per-pin direction bits. Mimas may eventually model real
      pins; §6.3's `[QUIRK]` says a pin-accurate model **must still produce the same observable
      ID nibbles**. Start with the enum, comment the intent.
- [x] **PDR1/PDR2 write, mode `0x00`** (§6.2): if the port's first peripheral is a gun and
      `(val & 0x7F) == 0x7F`, `PDR[n] = data[2]` (the gun button byte). Otherwise nothing.
- [x] **PDR1 write, mode `0x40`** — `do_th_mode` (§6.2): `val & 0x40 == 0x40` →
      `0x70 | (data[3] & 0x0C)`; `val & 0x40 == 0x00` → `0x30 | ((data[2] >> 4) & 0x0F)`.
      §6.2 tags this `[HACK]` (it exists for *World Heroes Perfect*'s Mega Drive-ID probe), and
      §6.2 notes **PDR2 has no `0x40` case** (§10.1 #29). Implement port 1 only, and comment
      why the asymmetry is intentional rather than a copy-paste slip.
- [x] **PDR1/PDR2 write, mode `0x60`** — the four-phase TH/TR handshake (§6.2). `val & 0x60`
      selects the nibble; bit 7 of the written value is preserved; bit 4 is forced:
      | `val & 0x60` | Phase | Result |
      |---|---|---|
      | `0x60` | 1st | `(val & 0x80) \| 0x14 \| (data[3] & 0x08)` — L trigger in bit 3 |
      | `0x20` | 2nd | `(val & 0x80) \| 0x10 \| ((data[2] >> 4) & 0x0F)` — Right/Left/Down/Up |
      | `0x40` | 3rd | `(val & 0x80) \| 0x10 \| (data[2] & 0x0F)` — Start/A/C/B |
      | `0x00` | 4th | `(val & 0x80) \| 0x10 \| ((data[3] >> 4) & 0x0F)` — R/X/Y/Z |
- [x] **DDR1 write** — the peripheral-ID nibble table (§6.3), switching on port 1's **status
      byte** then its first peripheral ID:
      | `data[0]` | `data[1]` | `PDR1 ←` |
      |---|---|---|
      | `0xA0` (gun) | `GUN 0x25` | `0x7C` |
      | `0xA0` | other | unchanged |
      | `0xF0` (nothing) | — | `0x7F` |
      | `0xF1` | `PAD 0x02` | `0x7C` |
      | `0xF1` | `PAD_3D 0x16`, `KEYBOARD 0x34` | `0x71` |
      | `0xF1` | `MOUSE 0xE3` | `0x70` |
      | `0xF1` | wheel / mission stick / twin sticks / other | unchanged + log once |
      | anything else (incl. `0x16` tap) | — | `0x71` |
      §6.3's formatting trap (`break` outside the `if`, so case `0xA0` never falls through into
      `0xF0`) is a C artifact; the table above is the intended behavior and is what to write.
- [x] **DDR2 write** (`0x7B`): §1.2/§6.3/§10.1 #28 record that Yabause has **no `case 0x7B`**,
      so port 2 gets no ID handshake at all. That is a defect, not hardware. **Implement the
      symmetric DDR2 handler** against port 2, and flag the divergence from the reference in
      the doc comment. (The BIOS writes DDR2 during boot — trace #13 — so the path is live.)
- [x] **IOSEL** (`0x7D`): §6.4/§10.1 #27 — Yabause stores it and never reads it, behaving as
      though both the SMPC-managed and direct-access paths are permanently enabled. Keep it as
      stored-only for now, with a comment naming the real meaning (per-port select between the
      two access paths) so a future gate has an obvious home. Do not invent gating semantics
      this source cannot supply.
- [x] **EXLE** (`0x7F`) bit 0 (§6.4): when VDP2's `EXTEN & 0x200` is set **and** `EXLE & 1`, at
      **V-Blank OUT** latch `HCNT = (port1.data[3] << 8 | port1.data[4]) << 1`,
      `VCNT = (port1.data[5] << 8 | port1.data[6])`, `TVSTAT |= 0x200`. §6.4 records the
      reference's own admission that this should fire at the beam position, not once per frame,
      and that only port 1 is ever latched (§10.1 #33). Gate this behind Phase 7's gun support
      — with no gun, `port1.data[3..7]` is meaningless — but add the EXLE storage + the VDP2
      hook point now, since VBLANK-OUT already exists (`sh2.rs:1700-1706`).

**Testing (Phase 5)** — every value hand-computed from §6.2/§6.3's formulas.

- [x] `pdr1_four_phase_idle_pad`: one idle pad (`data[2] = data[3] = 0xFF`), `DDR1 = 0x60`.
      Write `0x60` → PDR1 **`0x1C`** (`0x14 | (0xFF & 0x08)`); `0x20` → **`0x1F`**;
      `0x40` → **`0x1F`**; `0x00` → **`0x1F`**.
- [x] `pdr1_four_phase_a_and_right`: `data[2] = 0x7B` (from Phase 4's derivation),
      `data[3] = 0xFF`. `0x60` → **`0x1C`**; `0x20` → **`0x17`** (`0x10 | (0x7B >> 4)`);
      `0x40` → **`0x1B`** (`0x10 | (0x7B & 0xF)`); `0x00` → **`0x1F`**.
- [x] `pdr1_preserves_written_bit7`: write `0xE0` (`0x80 | 0x60`) with an idle pad →
      **`0x9C`**.
- [x] `ddr1_id_nibble_table`: pad → **`0x7C`**; nothing connected → **`0x7F`**; multi-tap →
      **`0x71`**; mouse → **`0x70`**; 3D pad → **`0x71`**. Assert for `DDR1 & 0x7F` of both
      `0x00` and `0x40` (§6.3: shared body).
- [x] `ddr2_mirrors_ddr1_against_port2`: the deliberate divergence from §10.1 #28.
- [x] `unknown_control_method_leaves_pdr_untouched`: `DDR1 = 0x20`, write PDR1, assert PDR1 is
      unchanged from a pre-seeded sentinel (§6.1's default arm).
- [x] Real-BIOS smoke: the §0.6 capture must still show trace #12-#15 and must not gain any new
      `Smpc(…) R` that returns a value the BIOS then loops on.

---

## Phase 6 — Real command timing, and moving the SMPC onto Core 7

Only now, once the semantics are right and covered by tests, change *when* and *where* commands
run. Doing this earlier risks breaking the working §0.6 handshake with no test net.

- [ ] **`timing` in microseconds** (§3.1/§3.3). Per-command delays from §3.3's table: every
      command is `1 µs` except INTBACK. INTBACK sub-cases (§3.3):
      | Condition | `timing` | `wait_for_line` |
      |---|---|---|
      | continuation (`intback` already set) | `16000` | yes |
      | `IREG0 == 0x01 && (IREG1 & 0x08)` | `250` | no |
      | `IREG0 == 0x01 && !(IREG1 & 0x08)` | `250` | no |
      | `IREG0 == 0x00 && (IREG1 & 0x08)` | `16000` | yes |
      **Do not port §3.3's `[BUG]` #4** (the "anything else" row leaves `timing` unassigned, so
      the command never dispatches and SF never clears — an infinite spin for any
      `IREG0 ∉ {0x00, 0x01}`). Mimas must assign a timing on every path.
- [x] **`intback_wait_for_line`** (§3.2): the peripheral fetch is additionally gated on reaching
      **scanline 207** (18 lines before the default V-Blank IN at line 225). Mimas has no
      scanline counter — VBLANK is wall-clock (`sh2.rs:1688-1706`, `VBLANK_INTERVAL`
      `16_666 µs`, `VBLANK_DURATION` `2_417 µs`). Either derive an equivalent wall-clock instant
      (line 207 of 262 ⇒ `207/262 × 16_666 µs ≈ 13_167 µs` into the frame) or introduce a real
      line counter. Prefer the line counter — VDP2 will need one anyway
      (`docs/implementation-plans/vdp2.md`) — but a documented wall-clock approximation is
      acceptable as an intermediate step.
- [x] **SF becomes genuinely busy**: set SF=1 on COMREG write, clear it when the timer expires
      and the command dispatches. This is only safe with the whole Phase 1 SF test set green.
- [x] **Move execution to Core 7** (`lib.rs:343-360`, currently a pure idle spin). Core 7 owns
      the countdown and calls `Smpc::tick(elapsed_us, &work_ram)`; the SH-2's COMREG write only
      *arms* it. Per `CLAUDE.md`'s zero-polling rule and `history.md`'s distilled principle 1,
      Core 7 must **park on a `Condvar` when no command is armed** and be woken by the arming
      write — the same `park_while_inactive` / `set_thread_active` mechanism Core 1 and Core 6
      already use (`lib.rs:165-168`, `lib.rs:322-338`, and `sh2.rs:732-736`'s
      `set_thread_active(6, true)` on the DSP `EX` write). Do **not** add another
      `yield_now` spinner.
- [x] **`SmpcEffects` must now cross a thread boundary.** `system_manager_irq` becomes an
      `Arc<AtomicBool>` that Core 0 polls in `service_pending_interrupt`, exactly like
      `sound_req_irq` (`sh2.rs:148`, `lib.rs:60`, `lib.rs:254`) — replacing today's
      `Sh2`-private `smpc_irq_pending` bool (`sh2.rs:138`). Use `Release`/`Acquire`, per the
      ordering contract documented at `sh2.rs:97-108`; `Relaxed` was a measured real bug in
      this codebase before (`history.md` Chapter 7).
- [x] `sh2.rs`'s `MemRegion::Smpc` arms shrink to: store byte, update `bustmp`, and on COMREG
      arm the SMPC + wake Core 7. No command logic left in `sh2.rs`.
- [x] Update `CLAUDE.md`'s thread table (Core 7 row) and
      `docs/mimas_emu_engineering_draft.md` §1.1 / §6's divergence notes.

**Testing (Phase 6)**

- [x] `sf_is_busy_between_arm_and_dispatch`: arm INTBACK, assert SF bit 0 == 1 before the
      timer expires, 0 after. Drive the clock deterministically (inject elapsed µs), never
      `thread::sleep`.
- [x] `intback_timing_table`: assert the armed delay is `250 µs` for `IREG0=0x01` (both IREG1
      variants) and `16000 µs` for `IREG0=0x00, IREG1 & 0x08` — §3.3, hand-transcribed.
- [x] `intback_with_malformed_ireg0_still_dispatches`: `IREG0 = 0x03` — the §3.3 `[BUG]` #4
      case. Must complete and clear SF, not spin.
- [x] `every_other_command_takes_1us`: table-driven over §3.3's 1 µs list.
- [x] `core7_parks_when_no_command_is_armed`: reuse the parking-measurement approach from
      `history.md` Chapter 10 / the existing parking tests — assert Core 7 is not woken at high
      frequency by unrelated `sync_core` traffic (the exact bug Chapter 10 found).
- [x] Full real-BIOS re-run and `[REGACCESS]` diff against the Phase 5 baseline. The BIOS
      `0x1D5A-0x1D64` poll loop must still exit.

---

## Phase 7 — Extended peripheral types, multi-tap, mouse, keyboard, light gun

Game-compatibility work; nothing here affects BIOS boot. Ordered last deliberately.

**Status: mostly done.** `PeripheralState` (`peripheral.rs`) replaced the old pad-only
`port1`/`port2` typing; every single-peripheral-per-port type below is implemented, byte-exact
against §9.3-9.7, and tested. Multi-tap and the live gun/VDP2 latch path are explicitly deferred
(see their own items) -- both are genuinely separate, large pieces of work, not partial
implementations of what's listed here.

- [ ] **Multi-tap promotion** (§9.2): first peripheral on an empty port → `data[0] = 0xF1` (or
      `0xA0` for a gun); a second → promote to `data[0] = 0x16`, pad the previously-direct
      peripheral out to 6 slots with single `0xFF` bytes, then walk to the first free slot.
      Trailing unused slots each contribute one `0xFF` byte and increment `size` (§9.2).
      **Do not port** §9.2's `[BUG]` #36 (adding to a gun-holding port overwrites the gun),
      `[BUG]` #37 (a gun accepted into a promoted tap truncates the whole report to 1 byte), or
      `[DEAD]` #38 (the unreachable `pernum == 0xF` guard). **Deliberately deferred** -- `PortData`
      as designed (one `id` + one flat `data[]`) only models a single peripheral per port; a real
      6-slot tap needs a genuinely different shape (multiple IDs, dynamic slot management, the
      >32-byte chunking edge cases §5.4 documents), not a small extension of what's here.
- [x] **Per-type report initialisers** (§9.3), `size = first_data_byte_index + (id & 0x0F)`:
      | Type | ID | Bytes | Initial |
      |---|---|---|---|
      | Pad | `0x02` | 2 | `FF FF` |
      | Wheel | `0x13` | 3 | `FF FF 7F` |
      | Mission stick | `0x15` | 5 | `FF FF 7F 7F 7F` |
      | 3D pad | `0x16` | 6 | `FF FF 7F 7F 7F 7F` |
      | Twin sticks | `0x19` | 9 | `FF FF 7F 7F 7F 7F 7F 7F 7F` |
      | Gun | `0x25` | 5 | `7C FF FF FF FF`, then contributes only its status byte to the report |
      | Keyboard | `0x34` | 4 | `FF F8 06 00` |
      | Mouse | `0xE3` | 3 | `00 00 00` |
      Fixed §9.3's `[BUG]` #39 while transcribing, per this table: twin sticks' 9th byte
      (axis 7) defaults to the neutral `0x7F`, not the real hardware's un-initialized `0x00`.
      Gun's real `port->size == 1` (a flat-array byte count that includes the status byte
      itself) has no direct equivalent in `PortData`'s split status/id/data model -- the
      equivalent there is `size == 0` (zero *extra* bytes past the status byte
      `chunk_port_data` already writes unconditionally); getting this wrong was a real bug
      caught while implementing (`docs/hardware-reference/smpc-peripheral.md` §5.5).
- [x] **Analog axes** (§9.5): `analogbits[0..1]` are the two digital bytes; `analogbits[2..8]`
      are axes 1-7, one byte each, neutral `0x7F`. Reported count depends on the declared data
      length: wheel = axis 1; mission stick = axes 1-3; 3D pad = axes 1-4; twin sticks = axes
      1-7. Axis 3 (mission stick) / axis 7 (twin sticks) are where real hardware's setter
      applies an inversion to a *live* joystick reading before storing -- since no live analog
      input frontend exists to drive that setter, `MissionStickState::axis3`/
      `TwinSticksState::axis7` are treated as already-encoded wire bytes instead (documented on
      each field), avoiding a double-inversion bug an earlier draft had.
- [x] **Digital synthesis from analog axes with hysteresis** (§9.5), exact thresholds:
      | Device | Axis | Press | Release | Bit |
      |---|---|---|---|---|
      | Wheel | 1 | `≤ 0x67` | `≥ 0x6F` | 6 (Left) |
      | Wheel | 1 | `≥ 0x97` | `≤ 0x8F` | 7 (Right) |
      | Mission stick, twin sticks | 1 | `≤ 0x56` | `≥ 0x6A` | 6 (Left) |
      | Mission stick, twin sticks | 1 | `≥ 0xAB` | `≤ 0x95` | 7 (Right) |
      | Mission stick, twin sticks | 2 | `≤ 0x65` | `≥ 0x6A` | 4 (Up) |
      | Mission stick, twin sticks | 2 | `≥ 0xA9` | `≤ 0x94` | 5 (Down) |
      3D pad gets **no** synthesis — its D-pad is driven through the ordinary pad path (§9.5).
      **Simplification**: only the press thresholds are implemented -- `to_port_data(&self)` is a
      pure function of the current axis value with no memory of the previous digital bit, so true
      hysteresis (which needs that memory to pick the *release* threshold once already pressed)
      isn't representable without a stateful setter. No live analog input caller exists yet to
      need the difference; revisit if one is ever wired.
- [x] **Mouse** (§9.6), ID `0xE3`, 3 data bytes, buttons **active-high** (opposite of the pad):
      `mousebits[0]` bit 0 Left, 1 Right, 2 Middle, 3 Start, 4 X-sign, 5 Y-sign, 6 X-overflow,
      7 Y-overflow; `mousebits[1]` X displacement, `mousebits[2]` Y displacement. Magnitudes are
      stored as the **one's complement** when the sign bit is set (already-encoded wire byte,
      same convention as the analog axes above). **Deliberately not done**: overflow-bit
      *generation*. §10.1 #41 and this table both ask for it, but
      `docs/hardware-reference/smpc-peripheral.md` §9.6 is explicit that the reference itself has
      "no saturation and no overflow-flag generation" -- there is no citable formula anywhere in
      this project's source material for what threshold/condition real silicon would use, and
      guessing one would violate `CLAUDE.md`'s "never assert a value you haven't independently
      derived" rule. Revisit if an authoritative source for the real trigger condition surfaces.
- [x] **Mouse flush at snapshot** (§9.6, the `PerFlush` equivalent): clear sign + overflow bits
      (`mousebits[0] &= 0x0F`), zero both displacement bytes. Fixed §10.1 #42 while implementing:
      flushes whichever peripheral is actually on each port by matching on the `Mouse` variant
      itself (not a literal ID byte), so it isn't limited to "slot 1" the way the reference's
      pointer-arithmetic version is -- moot today since multi-tap (multiple slots per port) is
      deferred, but the fix costs nothing extra and removes a known real bug outright rather than
      porting it into a design where it would otherwise resurface once multi-tap lands.
- [x] **Keyboard** (§9.7), ID `0x34`, 4 bytes, initial `FF F8 06 00`. §10.1 #44 records that the
      reference registers no input callbacks at all — the keyboard can be enumerated but no key
      can ever be pressed. Matches: `KeyboardState` is a unit struct, DDR1 reports `0x71` (§6.3)
      when connected, and no key-input API exists -- documented as report-shape-only, not guessed.
- [ ] **Light gun** (§6.5, §9.3): buttons in `data[2]`, active-low, bit 4 Trigger, bit 5 Start,
      initial `0x7C` (which is also the DDR1 ID value, §6.3). Absolute position big-endian
      across `data[3..7]`. **Partial**: `GunState` stores trigger/start/x/y and PDR1/PDR2 mode
      `0x00`'s gun-button read (§6.2) is wired and tested. The VDP2 external-latch position path
      (§6.4/§6.5, `/4` scale, inverted Y, resolution-derived clamp) is **deliberately deferred**
      -- it needs both a live gun-position input source (no frontend supplies pointer/gun input
      today, mirroring the pad's own "only player 1 via keyboard" limitation) and VDP2
      external-latch register wiring, neither of which this phase's scope reaches.
- [x] **Gun and the report** (§5.5, §10.1 #35): a gun forces its report down to the status byte
      alone (`PortData::size == 0` in this crate's model, see above) -- confirmed by a dedicated
      end-to-end test (`gun_contributes_only_its_status_byte_to_the_intback_stream`), not just
      asserted in isolation.
- [x] Extend `PeripheralKind` and `SaturnSystem::set_port_peripheral`/`set_peripheral_state` to
      cover every type above.

**Testing (Phase 7)**

- [ ] `multitap_two_pads_report`: deferred with multi-tap itself.
- [x] `per_type_initial_reports`: table-driven over §9.3's initial-value table, asserting both
      the bytes **and** the resulting `size` (including the gun's `size == 0` in this crate's
      model).
- [x] `twin_sticks_axis7_is_neutral` (folded into `per_type_initial_reports`): **`0x7F`**, not
      `0x00` — the §9.3 `[BUG]` #39 fix.
- [x] `wheel_hysteresis_press_thresholds_match_the_reference_table`: the press-threshold subset
      of §9.5's table (see the "Simplification" note on digital synthesis above for why the gap/
      release-threshold cases aren't meaningfully testable against a stateless implementation).
- [x] `ddr_id_nibble_covers_every_connected_type`: all nine `PeripheralState` variants against
      §6.3's full table, including the "unsupported, PDR left untouched" row.
- [x] `mouse_buttons_are_active_high`: the opposite polarity from the pad — press Left → bit 0
      **set** (§9.6), directly contrasted against the pad test from Phase 4.
- [x] `mouse_negative_displacement_is_ones_complement`: move `-1` in X → sign bit 4 set and
      `mousebits[1] == 0xFE` (`!1`), per §9.6.
- [x] `mouse_flush_clears_deltas_but_keeps_buttons`: §9.6 — after a snapshot, `mousebits[0]`
      retains its low nibble and loses bits 4-7; bytes 1-2 are zero.
- [x] `gun_contributes_only_its_status_byte_to_the_intback_stream`: gun on port 1, pad on port 2
      → OREG0 == `0xA0`, OREG1 == port 2's own status byte immediately following (§5.5's worked
      example shape), no ID/data bytes for the gun at all.
- [ ] `exle_latches_gun_position_at_vblank_out`: deferred with the live gun/VDP2 latch path.
- [x] Also added beyond the plan's own list, once real bugs were found while restoring Phase 5's
      PDR/DDR handling (a regression from an earlier pass on this phase, unrelated to Phase 7's
      own scope but caught and fixed in the same pass): `pdr1_four_phase_idle_pad`,
      `pdr1_four_phase_a_and_right`, `pdr1_preserves_written_bit7`,
      `pdr1_gun_button_read_at_mode_0x00`, `unknown_control_method_leaves_pdr_untouched`,
      `ckchg352_sets_dotsel_and_ckchg320_clears_it`, `port1_defaults_to_a_connected_pad_port2_to_disconnected`,
      `set_port_peripheral_and_set_peripheral_state_both_reach_ddr_and_intback`,
      `intback_peripheral_one_pad_port1_matches_the_worked_example`,
      `intback_peripheral_both_ports_empty_matches_the_worked_example`,
      `intback_peripheral_sr_first_vs_subsequent`,
      `intback_break_clears_sr_high_nibble_and_drops_the_snapshot`.

---

## Cross-cutting rules for every phase

- [ ] **Never assert a value that was read off the current implementation.** Every expected
      byte in this plan is derived from a cited `hardware-reference` formula, from a real BIOS
      byte in §0.6, or hand-computed and shown. `CLAUDE.md` names `bt_bf_no_delay_slot` and the
      first `DIV1` test as the precedents for why.
- [ ] **`cargo test --workspace` green after every phase**, not just the phase's own tests
      (`CLAUDE.md` stability constraint).
- [ ] **`Sh2::new()` stays a 3-argument constructor** (`sh2.rs:260`, `CLAUDE.md`). New capability
      arrives via optional fields set after construction — the `pc_reporter` / `m68k_control` /
      `sound_req_irq` / `speed` / `scu_dsp` pattern (`lib.rs:147-151`).
- [ ] **`cargo fmt`** before considering any phase done.
- [ ] **Update the tracking docs as you go**, not at session end: `.development/current_blocker.md`,
      `current_bugs.md`, `TASKS.md`, `ROADMAP.md` (all four are currently **empty files**), and
      add a `history.md` chapter explaining *why* the `Smpc` extraction was shaped the way it
      was.
- [ ] **Re-run the §0.6 real-BIOS capture at the end of every phase** and diff the `[REGACCESS]`
      SMPC sequence against the previous phase's. It is the cheapest possible regression net for
      "did I just break the boot handshake", and the recipe already exists (`sh2.rs:242-257`).
- [ ] **Every simplification gets a comment at the exact place it is made** — the practice
      `history.md`'s closing section credits with making the M68K wall findable at all.

---

## Yabause defects deliberately **not** replicated

Each is tagged in the reference; each is a real defect rather than hardware behavior. Listed so
a future reader does not "fix" Mimas back into agreement with the C.

| Ref # | § | Defect | Mimas's choice |
|---|---|---|---|
| 4 | §3.3 | INTBACK with `IREG0 ∉ {0x00,0x01}` never dispatches; SF never clears | Always assign a timing (Phase 6) |
| 5 | §2.2 | `SF = 1` at `SmpcINTBACK` entry immediately undone | Real busy window (Phase 6) |
| 6 | §1.3 | `SF &= val` is a no-op after the unconditional store | Plain verbatim store (Phase 1) |
| 14 | §0.2 | `SmpcSetClockSync` falls off the end without returning | N/A in Rust |
| 15 | §5.4 | Multi-chunk copies read `data`, not `data + offset` | Copy from the cursor (Phase 4) |
| 16 | §5.2 | `OREG[31] = 0x10` clobbers peripheral data, inconsistently | Not written on the peripheral path (Phase 4) |
| 17 | §5.4 | Break / `INTBACKEnd` leave a stale snapshot to be resumed | Clear both snapshot sizes (Phase 4) |
| 18 | §5.2 | `SR = 0x40` dead store | Omitted (Phase 4) |
| 20 | §5.6 | OREG16-30 left stale | Zeroed, documented (Phase 2) |
| 28 | §6.3 | No `case 0x7B` — port 2 has no ID handshake | Symmetric DDR2 handler (Phase 5) |
| 36 | §9.2 | Second peripheral overwrites a gun | Rejected properly (Phase 7) |
| 37 | §9.2 | Gun accepted into a promoted tap truncates the report | Rejected properly (Phase 7) |
| 38 | §9.2 | Unreachable `pernum == 0xF` guard | Omitted (Phase 7) |
| 39 | §9.3 | Twin sticks' 9th byte uninitialised at `0x00` | Neutral `0x7F` (Phase 7) |
| 41 | §9.6 | Mouse overflow bits never generated | Generated (Phase 7) |
| 42 | §9.6 | `PerFlush` only clears slot 1 and hard-codes `0xE3` | All slots, by constant (Phase 7) |
| 43 | §9.6 | `mousebits[0] &= 0xFFFB` against a `u8` | Correct mask (Phase 7) |
| 45 | §9.8 | `PerConfig_struct.key` left uninitialised by `realloc` | Whole binding layer stays out of `saturn-core` (Phase 4) |
| 25, 26 | §7.2, §5.4 | Movie/TAS RTC override and `LagFrameFlag` | Not ported |
| 51 | §0.1 | `ste`, `resb`, `intbackIreg0`, `syslngid` written and never read | Not modelled |

Also **not** ported: §1.1's even/odd storage alias (`[QUIRK]` #47) and §1.4's word/long
drop-to-zero (`[QUIRK]` #46) — see §0.4 above for why Mimas's existing behavior is kept in both
cases, with tests pinning it.
