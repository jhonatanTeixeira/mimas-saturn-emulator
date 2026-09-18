#!/usr/bin/env python3
"""How far through a real BIOS boot did Mimas actually get?

The settle PC answers "which instruction was hottest when we gave up", which in
a BIOS this full of counted delay loops is nearly always a delay loop. It cannot
say how far the boot progressed. This can.

**The reference** (`bios_reference/bios_boot_frames.json`) is a real
Yabause/YabaSanshiro capture of a Saturn BIOS boot with a disc inserted, taken
from the `portal_to_another_world` project. It is *novelty-filtered*: each frame
lists the instruction addresses never executed before that frame, so frames with
no new code are simply absent. Two consequences worth internalising before
reading any output:

  - the union of frames 0..713 is the complete instruction-address set of a
    successful BIOS boot -- 9,882 addresses;
  - frame numbers order first-execution, they do not order time, and two
    captured frames are not adjacent.

**The measurement** is a set comparison, not a trace diff. Mimas records every
distinct PC the Master SH-2 executes (`MIMAS_PC_TRACE=<file>`); this intersects
that with each reference frame in order. A frame counts as reached when every
address it introduces has been executed. The first address of the first
unreached frame is where the two diverge, and that is the actionable output --
it names one instruction to go and look at, rather than a symptom.

**Two limits of this instrument, both of which have already produced a wrong
reading here.** Neither is a defect in the emulator, and both look exactly like
one:

  - **Delay slots are invisible.** The recorder fires once per `run_loop`
    iteration, but a delay-slot instruction executes *inside* `step()` via
    `delay_slot_and_jump`. So `0x06001608`, the `MOV.B R1,@R7` in the `BSR` at
    `0x06001606` that actually writes SNDON to COMREG, is never recorded even
    though it certainly ran. A missing address immediately after a branch is a
    delay slot until proven otherwise.
  - **It is a set, not a count.** An address executed on one path and not on a
    later one still reads as executed. `0x06001698` (an `RTS`) being present
    does not mean the *second* call through it returned.

For "did this specific call return", a sequential ring buffer of the last N PCs
is the right instrument, and this is not it.

Deliberately not a pass/fail on exact equality: Mimas will legitimately execute
addresses the reference never did (different disc, different timing, our own
error paths). Extra coverage is reported but never fails; only *missing*
reference addresses count against progress.

Usage:
    MIMAS_PC_TRACE=/tmp/pcs.txt ./target/release/saturn-frontend-native --bios <bios>
    python3 tools/bios_progress.py /tmp/pcs.txt
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

REF = Path(__file__).parent / "bios_reference" / "bios_boot_frames.json"


def norm(a: str) -> str:
    """Fold an address to its physical form.

    `0x20000388` and `0x00000388` are the same ROM instruction reached through
    the cache-through mirror; the SH-2's reset vector uses the mirrored form and
    later code does not. The reference capture recorded whichever form the PC
    happened to carry (2,581 addresses at `0x0000xxxx`, 59 at `0x2000xxxx`), so
    both sides have to be folded or the reset vector itself reads as "never
    executed" -- which is exactly what the first version of this tool reported,
    against a run whose own log printed that PC.
    """
    return f"{int(a, 16) & 0x0FFF_FFFF:08X}"


def load_pcs(path: Path) -> set[str]:
    out = set()
    for line in path.read_text(errors="ignore").split():
        t = line.strip().upper().removeprefix("0X")
        if t:
            out.add(norm(t))
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("pc_trace", help="file of executed PCs, one per line (MIMAS_PC_TRACE)")
    ap.add_argument("--reference", default=str(REF))
    ap.add_argument("--require-frame", type=int, default=None,
                    help="exit non-zero unless this reference frame was fully reached")
    ap.add_argument("--min-coverage", type=float, default=None,
                    help="exit non-zero unless at least this %% of reference addresses ran")
    args = ap.parse_args()

    ref = json.loads(Path(args.reference).read_text())
    ours = load_pcs(Path(args.pc_trace))

    frames = [{"frame": f["frame"], "new": [norm(a) for a in f["new"]]}
              for f in ref["frames"]]
    total = {a for f in frames for a in f["new"]}
    hit = total & ours

    reached, first_gap = [], None
    for f in frames:
        missing = [a for a in f["new"] if a not in ours]
        if missing:
            if first_gap is None:
                first_gap = (f["frame"], missing, len(f["new"]))
        else:
            reached.append(f["frame"])

    print(f"Reference: {ref['game']}")
    print(f"  {len(frames)} novelty frames, {len(total)} instruction addresses, "
          f"through frame {ref['last_bios_frame']}")
    print(f"Mimas executed {len(ours)} distinct PCs, {len(hit)} of them in the reference "
          f"({100.0*len(hit)/len(total):.1f}%)")
    print()

    if first_gap is None:
        print(f"✅ Every reference address was executed — the BIOS boot completed "
              f"through frame {ref['last_bios_frame']}")
        return 0


    frame_n, missing, size = first_gap
    done = [f["frame"] for f in frames if f["frame"] < frame_n]
    print(f"Fully reached: {len(done)} of {len(frames)} frames "
          f"(last complete: frame {max(done) if done else 'none'})")
    print(f"First gap: frame {frame_n} — {len(missing)} of {size} addresses never executed")
    print()
    print("  The divergence starts at:")
    for a in missing[:10]:
        print(f"     0x{a}")
    if len(missing) > 10:
        print(f"     … +{len(missing)-10} more")
    print()
    print("  Disassemble the first one to see what the BIOS does there that we do not:")
    a = missing[0]
    if a.startswith("06") or a.startswith("20") or a.startswith("00"):
        rom = int(a, 16) & 0x000FFFFF
        print(f"     python3 tools/sh2dis.py <(dd if=<bios.bin> bs=1 skip={rom-16} "
              f"count=64 2>/dev/null) 0x{(int(a,16)-16):08X}")

    if args.min_coverage is not None:
        pct = 100.0 * len(hit) / len(total)
        print()
        if pct + 1e-9 >= args.min_coverage:
            print(f"\u2705 reference coverage {pct:.2f}% >= {args.min_coverage:g}%")
        else:
            print(f"\u274c reference coverage {pct:.2f}% < {args.min_coverage:g}% "
                  f"\u2014 the boot reaches less of the real BIOS than it used to")
            return 1

    if args.require_frame is not None:
        ok = args.require_frame in reached and (first_gap[0] > args.require_frame)
        print()
        if ok:
            print(f"✅ frame {args.require_frame} reached")
            return 0
        print(f"❌ frame {args.require_frame} not reached")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
