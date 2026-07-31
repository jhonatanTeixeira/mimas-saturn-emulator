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
