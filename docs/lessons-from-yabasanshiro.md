# Lessons from yabasanshiro's performance work

**Source**: the `main` branch of the yabasanshiro fork — the same `libretro`/devMiyax Yabause
fork this repo already treats as the hardware-behavior oracle (see `CLAUDE.md`'s "Working
methodology" section) — commits `750a561d`, `4bf503c9`, `378c1ebd`, `cd84c65c` (chronological,
2026-08-13 to 2026-08-15), plus the analysis docs committed alongside them:
`docs/once_a_frame.md`, `docs/per_deciline.md`, `docs/every_pixel.md` in that repo. Full
narrative of that work: that repo's `docs/historico_main.md`.

**Caveat on paths**: those four commits live on yabasanshiro's `main` branch. The `../yabause`
sibling checkout this repo's `CLAUDE.md` points at for hardware cross-checking is currently on
`perf/persistent-tile-cache` (off `perf/r36s-improvements`), which does **not** contain them —
`git log --all` there comes up empty for all four hashes. Check out `main` there (or use another
local yabasanshiro clone) to see the exact diffs; the `file:line` references below are otherwise
stable, being anchored to a specific commit hash regardless of which local checkout has it.

That effort's goal was cutting interpreter CPU from ~32% to ~10% of one core on a
single-threaded C build; Mimas is a from-scratch, already-threaded Rust rewrite, so most of its
fixes don't transplant as code. What follows is the subset that does — either as a near-identical
bug Mimas already has today (§1, §2), or as a technique/gotcha worth having on file for when the
relevant milestone lands (§3-§6). §1 and §2 are worth acting on now, independent of any phase;
the rest are pointed at from `.development/phased_development_plan.md` where they become
relevant.

## 1. Cores 2, 4, 5, 7 don't actually park when idle — same bug class as yabasanshiro's SCSP spin, worse in two of four cases

**What yabasanshiro found and fixed**: `ScspAsynMainCpuTime` (yabause `scsp.c`, ~line 5527) spun
on a plain busy-loop checking an atomic counter, with a "fall back to a real sleep after ~200,000
iterations" path that was permanently dead code in practice — the counter always changed sooner
than that. Measured at ~30% of the *entire emulator's* CPU, for zero work done. Fixed by waiting
on the condition that actually matters (a full audio sample's worth of cycles banked) and
yielding via a real sleep instead. Commit `750a561d`; see `once_a_frame.md`'s "Top findings #1".

**Where this shows up in Mimas today**: `SaturnSystem::start` (`saturn-core/src/lib.rs:205-401`)
spawns Cores 2, 3, 4, 5, 7 in a `while !shutdown { ...; sync_core(id, cycles);
thread::yield_now(); }` shape. `LockStepSync::park_while_inactive`/`set_thread_active`
(`sync.rs:99-156`) — the mechanism that actually parks a thread at ~zero host CPU — is used only
by Cores 1 and 6 today. `CLAUDE.md`'s own "Known architecture debt" section already names this
exact gap; `docs/mimas-performance-analysis.md` §3's "Elimination of Busy-Waiting... consuming
zero CPU cycles" claim describes the target architecture, not the current state.

Two of the five (Cores 2 and 7) are worse than yabasanshiro's bug: they have *zero* real work
today (VDP1 execution actually runs on Core 3; SMPC runs inline inside Core 0's `Sh2`; CD-ROM
isn't wired to any core), so every iteration of their loop is pure waste — not even
occasionally-useful polling.

**Recommendation**:
- **Core 7** (`smpc-cd-block`): call `set_thread_active(7, false)` + `park_while_inactive(7)`
  immediately — same shape already used by Core 1/6 — until real SMPC/CD-block work lands here
  and needs waking on something concrete (a CD command, an INTBACK poll). Zero correctness risk;
  it does nothing today either way.
- **Core 2** (`vdp1-draw`): same treatment, until `vdp1.md` Phase 9 actually moves
  `execute_vdp1` here.
- **Core 4** (`m68k-sound-cpu`): already has the right signal (`should_run`, gated on SMPC
  SNDON/SNDOFF) — wire its transitions to `set_thread_active(4, ...)` instead of falling through
  to `yield_now()` while stopped.
- **Core 3** (`vdp2-composite`): has real periodic work, so full parking doesn't fit — but the
  spin between `next_frame_due` ticks is the same "check a cheap condition, immediately loop
  back" shape as the original bug. Sleeping most of the remaining interval (waking a little
  early to fine-tune) would cut the waste without changing frame timing.
- **Core 5** (`scsp-synth`): does real continuous work every iteration (`synthesize`), so this
  isn't a "spin doing nothing" case — but unlike Core 4, it has no `ClockThrottle` of its own, so
  in a real 1x-speed session it's paced only by how far `LockStepSync` lets it drift ahead of
  other cores, not by wall clock. Worth giving it its own throttle (mirroring Core 4's
  `m68k_throttle`) once audio is far enough along to profile — flagged here, not urgent.

Since both this project and yabasanshiro target the same weak multi-core hardware (R36S, 4
cores), 3-5 host threads spinning at full tilt on placeholder work is a bigger problem here than
it would be on a desktop dev box.

**Status**: not started. Applies at: Milestone 3 Phase 1 / Milestone 4 Phase 6 (Core 7),
Milestone 5 Phase 9 (Core 2), Milestone 6 (Cores 4/5) — see `.development/phased_development_plan.md`.

## 2. `Cdrom`'s single-hunk cache is the exact design yabasanshiro already found thrashes

**What yabasanshiro found and fixed**: the CHD hunk cache in `cd-libretro.c` was a single slot
(`current_hunk_id`) — fine for straight sequential reads, but it forced a full LZMA
re-decompress of the same ~19.5KB hunk every time the read pattern alternated between two disc
locations — exactly the shape of CD-DA streaming interleaved with occasional data reads
elsewhere on the disc. Fixed with a small LRU (64 slots, later 128), plus, in a follow-up
commit, an async read-ahead worker thread pre-decompressing 1-2 hunks ahead of the current
position. Commits `750a561d` (LRU) and `cd84c65c` (read-ahead thread); see `once_a_frame.md`.

**Where this shows up in Mimas today**: `Cdrom` (`saturn-core/src/cdrom.rs:4-8, 97`) has
`current_hunk_num: u32` — a single slot, same shape, same thrash trigger. It hasn't bitten yet
only because `Cdrom` isn't wired to CS2 or to Core 7 yet (see `CLAUDE.md`'s architecture-debt
section), so nothing exercises an alternating access pattern against it today.

**Recommendation**: replace the single slot with a small LRU (a handful of slots, indexed by
age, evict-oldest-on-miss) before or while landing `cs2-cdblock.md` Phase 5 (playback/CDDA) —
that is the phase that starts actually interleaving CD-DA reads with data reads. An async
read-ahead thread is a reasonable follow-up once basic sector reads are wired and correct, not a
Phase-1 requirement.

**Status**: not started. Applies at: Milestone 3 Phase 1 (heads-up) / Phase 5 (the actual
trigger condition).

## 3. Memory-bus wait-state accounting: Mimas already has the right end-state

**What yabasanshiro found and fixed**: `GET_MEM_CYCLE_R`/`_W` (yabause `memory.c`) wrote a
wait-state cost into a `u32 *cycle` output parameter, which every one of ~40 call sites in
`sh2int.c` then had to read back and manually add to the running cycle count. Fixed (commit
`378c1ebd`, "P2") by making the macro accumulate directly into `CurrentSH2->cycles` when the
caller passes `NULL` instead of an output pointer.

**Where this shows up in Mimas today**: `add_wait_states_r`/`_w` (`sh2.rs:836-854`) already
accumulate directly into `self.cycles` — no output-parameter round-trip. This is the destination
design yabasanshiro had to retrofit; Mimas already has it, likely because an output-pointer
parameter fights the borrow checker in Rust more than it does in C. No action needed — noted
here only so it isn't mistaken for an open gap.

One related, much lower-priority idea: yabause additionally precomputes a per-region lookup
table (`ReadCycleList`/`WriteCycleList`, filled once at init) instead of re-deriving "which bus
region is this address in" via a branch chain on every access (same commit). `mem_cycles_r/w` in
`sh2.rs` still does the equivalent of the old branch-chain approach — but Rust's `match` already
compiles to something considerably cheaper than yabause's original C `switch`, and this is not
remotely the bottleneck at Mimas's current bring-up stage. Worth a profiler check only if/when
`memory-bus.md` Phase 8 (access cost model, already flagged optional) is revisited for
performance rather than correctness.

**Status**: informational / no action. Applies at: `memory-bus.md` Phase 8 (optional, low
priority).

## 4. VDP1/VDP2 frame-to-frame render caching: a technique to have on file, not something to port

**What yabasanshiro found and fixed**: the shared VDP1/VDP2 GPU texture atlas was rebuilt from
scratch every frame, even for 100%-static content. Fixed with a dirty-tracking gate
(`VIDOGLVdp2DrawStart`, `vidogl.c` ~line 6142) refined over three commits: a single global
"something changed" flag → per-subsystem flags → a 4KB-page dirty bitmap for VDP1 specifically.
Along the way, two non-obvious traps: (a) a naive `memcmp` of the whole register struct to
detect "did anything change" has to explicitly zero out fields that are live hardware *status*
rather than draw *configuration* (`EDSR`, the VDP1 command-list walk pointer, `TVSTAT`) — those
change every frame by design and would permanently defeat the cache otherwise; (b) a color-RAM
write does *not* need to invalidate a cache that bakes palette indices (not resolved colors)
into texels — except specifically when a layer's special-color-calculation mode reads the raw
palette value into the alpha channel.

**Where this shows up in Mimas today**: it doesn't, yet — and that's correct as-is. `vdp1.md`'s
own implementation plan already explicitly calls yabause's "`vdp1_clock` delay-the-frame
heuristic" and its dirty bitmap "**explicitly not hardware**", correctly excluded from what gets
ported (`docs/implementation-plans/vdp1.md`, the "Not ported" list). VDP1/VDP2 today render
backdrop-only (`vdp.rs`, driven from Core 3's loop) — there's no texture atlas or per-tile cache
to have this problem yet.

**Recommendation**: nothing now. If, once real VDP1 sprite/VDP2 tile rendering lands (`vdp1.md`
Phases 4-8, `vdp2.md` Phases 2-8) it turns out to need its own frame-to-frame cache for
performance, build it as an explicitly-labeled Mimas-original optimization — same convention
already used for e.g. the runaway-command-count cap — and reuse the two traps above rather than
rediscovering them across three commits the way yabasanshiro did.

**Status**: no action; keep on file for `vdp2.md` Phase 9 ("VRAM access cycle patterns: what to
actually build") if that phase ends up wanting one.

## 5. GPU compute-shader fallback pattern

**What yabasanshiro found and fixed**: a GLES 3.1 compute-shader path for VDP2 rotation
background rendering used to `abort()` the whole process on a shader compile/link failure.
Fixed to log the error, tear down the shader/program, flip a capability flag off, and fall back
to the existing (non-compute-shader) path — which is what made it safe to default the feature
*on* across varied device GPU drivers. Commit `378c1ebd`, "P0".

**Where this shows up in Mimas today**: nowhere — there's no GPU rendering backend in this
project yet (`saturn-frontend-native` is `minifb`-backed, CPU framebuffer only).

**Recommendation**: nothing now. If a GPU-accelerated renderer is ever added, treat any
shader-compile/link path the same way: never let a driver-specific compile failure take down
the process — detect it and fall back.

**Status**: not applicable yet; no phase exists for this.

## 6. Per-subsystem dispatch granularity should match that subsystem's real timing, not a generic constant

**What yabasanshiro found and fixed**: `SmpcExec`/`Cs2Exec` used to be called every deciline
(~2600-3130×/frame) regardless of their own, far coarser internal thresholds (SMPC ~83µs;
CD-block 333ms and 6.6-16.6ms). Batched to once per scanline instead — still finer than either
threshold, ~10x fewer calls, no observable timing change. One of the two threshold checks in
`cs2.c` (`_statuscycles`/`_periodiccycles`) had to change from `if` to `while` to correctly
handle a batched delta crossing more than one period in a single call — flagged in that
project's own docs as the riskier of the two changes, given CD-block timing's history of
regressions there. Commit `750a561d`.

**Where this shows up in Mimas today**: every "nothing real to do yet" core's dispatch step is
the same generic `(slack_limit / 2).max(2).min(500)` (`lib.rs`, Cores 2/3/4/5/7) — not derived
from any subsystem's actual hardware cadence. This is fine while those cores have no real
timing-sensitive logic; it stops being fine once real SMPC command timing (`smpc-peripheral.md`
Phase 6) and CD-block's free-running engines (`cs2-cdblock.md` Phase 3) land on Core 7.

**Recommendation**: when implementing those phases, size Core 7's dispatch cadence to
SMPC/CD-block's actual thresholds rather than reusing the generic clamp — and if a threshold
check is expressed as a single `if`, double-check whether a coarser dispatch interval can make
it need to fire more than once, the same `if`-vs-`while` mistake yabasanshiro's own history
flags as easy to get wrong here specifically.

**Status**: not started. Applies at: Milestone 3 Phase 3 (free-running engines) and Milestone 4
Phase 6 (SMPC command timing on Core 7).

---

Keep this file's "Status" lines in sync with `.development/phased_development_plan.md` as these
get picked up — same convention as `docs/implementation-plans/*.md`'s own status tracking (see
`CLAUDE.md`'s "Tracking docs" section).
