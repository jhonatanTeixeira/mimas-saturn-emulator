#!/usr/bin/env python3
"""Find `#[test]` functions that assert nothing.

Why this exists alongside coverage and clippy, rather than instead of either:

  * Coverage cannot see this at all. A test that runs code and asserts nothing
    reports 100% coverage of that code. The two checks are complementary.
  * Clippy's `assertions_on_constants` catches `assert!(true)` -- it found four
    in `vdp.rs` -- but only literal constants. It does not catch a test whose
    body has no assertion at all, and it does not catch a semantic tautology
    like `assert!(pixel.is_some() || pixel.is_none())` (`vdp2.rs`), which was
    found by reading, not by a tool.

What this still cannot do: judge whether an assertion is *meaningful*. A test
asserting a value copy-pasted out of the implementation's own output passes
every structural check and is still worthless -- CLAUDE.md's "never assert a
value you haven't independently derived". Mutation testing (`cargo-mutants`)
closes more of that gap; reading closes the rest.

A test that legitimately only proves "this does not panic" opts out with a
`// no-assert: <reason>` comment in its body. The marker sits at the call site
on purpose: an allowlist in a separate file drifts out of date silently.
"""
import re
import sys
from pathlib import Path

ASSERTION = re.compile(r"assert|panic!|unwrap|expect\(")
OPT_OUT = re.compile(r"//\s*no-assert:")

# Assertions that are textually present but assert nothing. Checked here, not
# left to clippy's `assertions_on_constants`, because a crate-level
# `#![allow(clippy::assertions_on_constants)]` switches that lint off for the
# whole crate in one line -- which is exactly what happened on 2026-09-17, and
# it silently re-hid the four `assert!(true)` placeholders in vdp.rs. A check
# that can be disabled by the code it checks is not a check.
CONSTANT_ASSERT = re.compile(
    r"assert!\s*\(\s*(?:true|false)\s*[,)]"          # assert!(true)
    r"|assert_eq!\s*\(\s*([A-Za-z0-9_]+)\s*,\s*\1\s*[,)]"  # assert_eq!(x, x)
    r"|debug_assert!\s*\(\s*(?:true|false)\s*[,)]"
)


def find(root: Path):
    for path in sorted(root.rglob("*.rs")):
        if "target" in path.parts:
            continue
        lines = path.read_text(errors="ignore").split("\n")
        for i, line in enumerate(lines):
            if line.strip() != "#[test]":
                continue
            # Attributes between #[test] and the fn -- #[should_panic] lives
            # here, and it *is* the assertion.
            j = i + 1
            attrs = []
            while j < len(lines) and "fn " not in lines[j]:
                attrs.append(lines[j])
                j += 1
            if j >= len(lines) or any("should_panic" in a for a in attrs):
                continue
            depth, body, k = 0, [], j
            while k < len(lines):
                depth += lines[k].count("{") - lines[k].count("}")
                body.append(lines[k])
                if depth <= 0 and k > j:
                    break
                k += 1
            text = "\n".join(body)
            name_m = re.search(r"fn\s+(\w+)", lines[j])
            name = name_m.group(1) if name_m else "?"
            if OPT_OUT.search(text):
                continue
            if CONSTANT_ASSERT.search(text):
                yield path, j + 1, name, "asserts a constant"
                continue
            if not ASSERTION.search(text):
                yield path, j + 1, name, "no assertion at all"


def main() -> int:
    hits = list(find(Path(".")))
    if not hits:
        print("✅ No assertion-free tests")
        return 0
    print(f"❌ {len(hits)} test(s) assert nothing:")
    for path, line, name, why in hits:
        print(f"   {path}:{line}  {name}  ({why})")
    print()
    print("   Each either needs a real assertion, or a `// no-assert: <reason>`")
    print("   comment if proving 'this does not panic' is genuinely the point.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
