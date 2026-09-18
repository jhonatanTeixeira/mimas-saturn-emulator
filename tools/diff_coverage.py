#!/usr/bin/env python3
"""Coverage of the lines a commit range actually changed.

Why this exists, and why it is not a lower bar:

The whole-tree floor is 90% and the tree measures ~65%. That gap is real debt,
recorded in `.development/current_bugs.md`, and lowering the floor to meet it
would measure nothing except how far the floor was lowered. But the gap also
makes the whole-tree number useless as a signal *for a change*: a commit that
adds 200 untested lines moves 65.03% to 64.8%, which no one notices, and a
commit that adds 200 well-tested lines is equally invisible.

Diff coverage asks the one question the whole-tree number cannot: **of the
executable lines this change touched, how many does a test reach?** Old debt
stays exactly as visible as it was, and new work has to carry its own tests.

Only lines tarpaulin considers executable are counted. Comments, blank lines,
`use` statements and type declarations never appear in the LCOV data, so they
are neither numerator nor denominator. `--ignore-tests` (which the gate passes)
keeps test bodies out too, so adding tests does not inflate the score -- the
tests show up as coverage of the product lines they exercise, which is the
point.

Usage:
    diff_coverage.py --lcov lcov.info --range HEAD~1..HEAD --min 90
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path


def repo_root() -> Path:
    out = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                         capture_output=True, text=True, check=True)
    return Path(out.stdout.strip())


def parse_lcov(path: Path, root: Path) -> dict[str, dict[int, int]]:
    """{repo-relative file: {line: hit count}} for executable lines only."""
    files: dict[str, dict[int, int]] = {}
    current: dict[int, int] | None = None
    for raw in path.read_text(errors="ignore").splitlines():
        if raw.startswith("SF:"):
            p = Path(raw[3:].strip())
            if not p.is_absolute():
                p = root / p
            try:
                rel = os.path.relpath(p.resolve(), root)
            except ValueError:
                rel = str(p)
            current = files.setdefault(rel, {})
        elif raw.startswith("DA:") and current is not None:
            line, _, hits = raw[3:].strip().partition(",")
            try:
                # A line can appear more than once (generics, inlining); any hit
                # counts as covered.
                n, h = int(line), int(hits.split(",")[0])
            except ValueError:
                continue
            current[n] = max(current.get(n, 0), h)
        elif raw.startswith("end_of_record"):
            current = None
    return files


HUNK = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")


def changed_lines(rng: str, root: Path) -> dict[str, set[int]]:
    """{repo-relative .rs file: set of line numbers added or modified}."""
    out = subprocess.run(
        ["git", "diff", "--unified=0", "--no-color", "--diff-filter=d", rng, "--", "*.rs"],
        capture_output=True, text=True, cwd=root,
    )
    if out.returncode != 0:
        sys.stderr.write(out.stderr)
        raise SystemExit(f"git diff failed for range '{rng}'")

    changed: dict[str, set[int]] = {}
    path: str | None = None
    for line in out.stdout.splitlines():
        if line.startswith("+++ "):
            target = line[4:].strip()
            path = None if target == "/dev/null" else re.sub(r"^b/", "", target)
            continue
        m = HUNK.match(line)
        if m and path:
            start = int(m.group(1))
            count = int(m.group(2)) if m.group(2) is not None else 1
            if count:  # count 0 is a pure deletion -- nothing new to cover
                changed.setdefault(path, set()).update(range(start, start + count))
    return changed


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lcov", required=True, help="LCOV file from `cargo tarpaulin --out Lcov`")
    ap.add_argument("--range", required=True,
                    help="git range ('A..B') or a single ref, which means '<ref>..HEAD'")
    ap.add_argument("--min", type=float, default=90.0, help="required percentage")
    args = ap.parse_args()

    root = repo_root()
    rng = args.range if ".." in args.range else f"{args.range}..HEAD"

    lcov = Path(args.lcov)
    if not lcov.is_absolute():
        lcov = root / lcov
    if not lcov.exists():
        print(f"❌ LCOV file not found: {lcov}")
        return 1

    cov = parse_lcov(lcov, root)
    changed = changed_lines(rng, root)

    rows, total, hit = [], 0, 0
    for f in sorted(changed):
        lines = cov.get(f)
        if not lines:
            # No executable lines reported for this file. Either it is not in
            # the coverage run's scope (a frontend binary, a build script) or
            # every changed line is a comment/type/`use`.
            continue
        touched = sorted(l for l in changed[f] if l in lines)
        if not touched:
            continue
        covered = [l for l in touched if lines[l] > 0]
        rows.append((f, len(covered), len(touched),
                     [l for l in touched if lines[l] == 0]))
        total += len(touched)
        hit += len(covered)

    print(f"   range: {rng}")
    if total == 0:
        print("   No executable product lines changed in this range "
              "(docs, tests, comments or type declarations only).")
        print(f"✅ Diff coverage — nothing to measure, so nothing to fail")
        return 0

    for f, c, t, missing in rows:
        pct = 100.0 * c / t
        mark = "  " if pct >= args.min else "❗"
        print(f"   {mark} {f}: {c}/{t} ({pct:.1f}%)")
        if missing:
            shown = ", ".join(str(l) for l in missing[:12])
            more = f" … +{len(missing) - 12} more" if len(missing) > 12 else ""
            print(f"        uncovered lines: {shown}{more}")

    pct = 100.0 * hit / total
    print(f"   TOTAL: {hit}/{total} ({pct:.2f}%) against a {args.min:g}% floor")
    if pct + 1e-9 >= args.min:
        print(f"✅ Diff coverage {pct:.2f}% >= {args.min:g}%")
        return 0
    print(f"❌ Diff coverage {pct:.2f}% < {args.min:g}% — the lines listed above "
          f"are new or changed product code that no test reaches")
    return 1


if __name__ == "__main__":
    sys.exit(main())
