# Real hardware fixtures

Binary fixtures in this directory are captured from a genuine **Yabause libretro core**, built from a disposable clone of the reference Yabause source (`scratch/yabause/` — never the pristine `../yabause/` checkout that `docs/hardware-reference/` cites line-by-line), patched with a small, temporary dump hook, and run against a **real Saturn BIOS and a real boot disc image** via RetroArch. They are not self-consistent guesses — see `docs/yabause_test_fixtures_extraction_plan.md` for the original plan and `docs/implementation-plans/*.md` for how each fixture is used.

## `smpc_intback_status.bin`

**What it is**: the full real 0x80-byte SMPC register window (odd-byte-per-register layout — matches `WorkRam::smpc_regs` exactly), captured the instant the real BIOS's first INTBACK call (status-only, `IREG1 = 0x02`, no peripheral data requested) finishes inside `SmpcINTBACKStatus()`/`SmpcINTBACK()` (`yabause/src/smpc.c`).

**How it was captured**:
1. Added a small hook to `scratch/yabause/yabause/src/smpc.c`: a `MimasDumpSmpcRegs(tag)` function (guarded by the `MIMAS_FIXTURE_DIR` env var, so it's inert unless explicitly enabled) that rebuilds the real 0x80-byte address-mapped register window from Yabause's internal `SmpcRegs` struct and writes it to `$MIMAS_FIXTURE_DIR/smpc_<tag>.bin`. Called once, right after `SmpcINTBACKStatus()` inside `SmpcINTBACK()`, tagged `"intback_status"`.
2. Rebuilt the libretro core: `cd scratch/yabause/yabause/src/libretro && make -j$(nproc)`.
3. Ran it headlessly (no window, no audio, no menu — `video_driver`/`audio_driver`/`input_driver`/`menu_driver` all set to `"null"` via an `--appendconfig` override) via RetroArch against a real BIOS (`saturn_bios.bin`, copied into an isolated `system_directory`) and a real boot disc (`SS - Boot Disc.ccd`):
   ```bash
   MIMAS_FIXTURE_DIR=saturn-core/tests/fixtures \
     timeout 25 retroarch -L scratch/yabause/yabause/src/libretro/yabause_libretro.so \
     --appendconfig=scratch/ra_override.cfg "<path to SS - Boot Disc.ccd>"
   ```
4. Copied the resulting `smpc_intback_status.bin` into this directory.

**Known issue while capturing**: RetroArch itself (not the Yabause core) hit a `stack smashing detected` abort a few seconds into the run, during `SET_INPUT_DESCRIPTORS` handling — after the fixture had already been written, so it didn't block this capture, but it may need root-causing (or working around, e.g. by exiting the core deterministically right after the dump instead of running until a crash) before capturing fixtures that need a longer run (e.g. a full VDP2 frame, or SCSP voices after key-on).

**To regenerate**: repeat the steps above. The hook in `smpc.c` is intentionally guarded by an unset-by-default env var and left in place in the scratch clone for reuse — it is not part of the pristine reference tree and must never be ported into `../yabause/`.

## `bios_post_smpc_regs.bin`, `bios_post_smpc_wram_high.bin`, `bios_post_vdp2_regs.bin`, `bios_post_vdp2_vram.bin`

**What they are**: a broader snapshot taken later in the same boot run than `smpc_intback_status.bin` above — the full 0x80-byte SMPC register window (this capture happened to land mid a peripheral-report INTBACK: `OREG0-3` show a real connected digital pad, port status `0xF1`/id `0x02`/idle buttons `0xFF 0xFF`, not just the status-only response), the full 1MB High Work RAM, the full 512KB VDP2 VRAM, and the full VDP2 register file (0x200 bytes, byte-swapped to big-endian to match `WorkRam::vdp2_regs`). `TVMD = 0x8000` (display enabled) and real non-zero bytes throughout VRAM/WRAM confirm the BIOS has made real progress by this point (128,396 non-zero bytes in VDP2 VRAM, 449,046 in High WRAM) — these are **not** all-zero placeholder captures.

**How they were captured** — and a real bug fixed along the way:
1. A `MimasDumpAllFixtures()` hook lives in `scratch/yabause/yabause/src/cs2.c`, gated the same way (`MIMAS_FIXTURE_DIR`).
2. **First attempt fired on the very first `Cs2Exec()` call** — which runs from the very first cycle of the whole emulation loop, long before the BIOS has done anything. Every byte in all four files was `0x00`. Fixed to instead fire once `MIMAS_FIXTURE_DELAY_SECS` (default 25) of real wall-clock time has elapsed since the process started, so a `timeout N retroarch ...` run captures state as late as possible within its own window instead of at T=0. Always sanity-check a new fixture for non-zero content before trusting it — a mechanically-successful capture (right file, right size) says nothing about whether the *trigger point* was meaningful.
3. A real, unrelated `stack smashing detected` crash (a buffer overflow in `Cs2ReadFileSystem`, `cs2.c`, copying a directory-entry name longer than its 32-byte destination) was hit and fixed (name copy now truncates at 31 chars) while chasing a longer capture window.
4. Rebuilt (`cd scratch/yabause/yabause/src/libretro && make -j$(nproc)`), then captured with a 30-second run and a 27-second dump delay (2s buffer before `timeout` kills the process):
   ```bash
   MIMAS_FIXTURE_DIR=saturn-core/tests/fixtures MIMAS_FIXTURE_DELAY_SECS=27 \
     timeout 30 retroarch -L scratch/yabause/yabause/src/libretro/yabause_libretro.so \
     --appendconfig=scratch/ra_override.cfg "<path to SS - Boot Disc.ccd>"
   ```

**Not yet consumed by any Rust test** — these four are raw material for VDP2's `docs/implementation-plans/vdp2.md` Phase 1-2 work (real register/CRAM/VRAM state to assert against) once that implementation starts; nothing currently loads them.

**Still missing**: a VDP1 fixture (command list + VRAM + framebuffer) and a CS2 register (CR1-4/HIRQ) fixture — the latter matters most given `.development/phased_development_plan.md`'s Milestone 3 targets CS2 next. Neither has been attempted yet.
