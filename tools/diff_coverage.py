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


def _rev(root: Path, ref: str) -> str:
    out = subprocess.run(["git", "rev-parse", "--verify", f"{ref}^{{commit}}"],
                         capture_output=True, text=True, cwd=root)
    if out.returncode != 0:
        raise SystemExit(f"not a commit: {ref}")
    return out.stdout.strip()


def resolve_base(rng: str, root: Path) -> str:
    """The commit to diff *from*. The diff always runs to the working tree.

    This is a correctness requirement, not a convenience. Coverage is measured
    by running the tests against the **working tree**, so the only line numbers
    the LCOV data can be intersected with are the working tree's. Asking for an
    arbitrary historical range `A..B` and intersecting B's line numbers with the
    working tree's coverage silently compares two different files: every line
    that moved reports the hit count of whatever now sits at that number.

    That is not theoretical -- this tool printed exactly that on its first real
    run, listing line numbers from a three-commit-old version of `vdp.rs` as
    "uncovered" while tarpaulin had measured the current one. So a range whose
    tip is not HEAD is refused rather than answered wrongly.
    """
    if ".." not in rng:
        return _rev(root, rng)
    base, _, tip = rng.partition("..")
    base = base.strip() or "HEAD"
    tip = tip.strip()
    if tip and _rev(root, tip) != _rev(root, "HEAD"):
        raise SystemExit(
            f"range tip '{tip}' is not HEAD.\n"
            f"Coverage is measured against the working tree, so the diff has to end\n"
            f"there too -- otherwise the line numbers refer to a different version of\n"
            f"the file than the one that was measured. Check out '{tip}', or pass just\n"
            f"the base ref (e.g. --range {base})."
        )
    return _rev(root, base)


def changed_lines(base: str, root: Path) -> dict[str, set[int]]:
    """{repo-relative .rs file: line numbers added or modified since `base`}.

    Diffs `base` against the working tree (no second endpoint), so uncommitted
    edits count -- they are what tarpaulin just measured.
    """
    out = subprocess.run(
        ["git", "diff", "--unified=0", "--no-color", "--diff-filter=d", base, "--", "*.rs"],
        capture_output=True, text=True, cwd=root,
    )
    if out.returncode != 0:
        sys.stderr.write(out.stderr)
        raise SystemExit(f"git diff failed against '{base}'")

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


def stale_sources(lcov: Path, root: Path) -> list[str]:
    """Tracked `.rs` files modified after the coverage data was written.

    Coverage is a snapshot. Intersecting it with a diff that includes edits made
    *after* the snapshot reports the old run's hit counts against the new file's
    line numbers -- the same class of mistake as diffing a historical tip, and
    just as silent. Editing file A can also change which lines of file B run, so
    this checks every tracked source file, not only the ones in the diff.
    """
    cutoff = lcov.stat().st_mtime
    out = subprocess.run(["git", "ls-files", "*.rs"],
                         capture_output=True, text=True, cwd=root)
    stale = []
    for rel in out.stdout.split():
        f = root / rel
        try:
            if f.stat().st_mtime > cutoff:
                stale.append(rel)
        except OSError:
            continue
    return stale


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lcov", required=True, help="LCOV file from `cargo tarpaulin --out Lcov`")
    ap.add_argument("--range", required=True,
                    help="base ref, or a range whose tip is HEAD; the diff always "
                         "runs from the base to the working tree (see resolve_base)")
    ap.add_argument("--min", type=float, default=90.0, help="required percentage")
    ap.add_argument("--allow-stale", action="store_true",
                    help="skip the check that the LCOV is newer than every tracked .rs file")
    args = ap.parse_args()

    root = repo_root()
    base = resolve_base(args.range, root)

    lcov = Path(args.lcov)
    if not lcov.is_absolute():
        lcov = root / lcov
    if not lcov.exists():
        print(f"❌ LCOV file not found: {lcov}")
        return 1

    if not args.allow_stale:
        stale = stale_sources(lcov, root)
        if stale:
            print(f"❌ Coverage data is older than {len(stale)} source file(s):")
            for f in stale[:8]:
                print(f"     {f}")
            if len(stale) > 8:
                print(f"     … +{len(stale) - 8} more")
            print("   The LCOV was written before these were edited, so its line numbers")
            print("   describe a different version of the code. Re-run:")
            print("     cargo tarpaulin --ignore-tests --skip-clean --out Lcov --out Html")
            return 1

    cov = parse_lcov(lcov, root)
    changed = changed_lines(base, root)

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

    print(f"   range: {base[:9]}..working tree")
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
