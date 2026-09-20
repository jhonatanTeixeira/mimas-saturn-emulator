#!/usr/bin/env python3
"""Minimal, dependency-free structural scanner for Rust source.

Not a parser. It does exactly the three things the golden-rule checks in
`golden_rules.py` need, and nothing else:

  1. blank out string/char literals and comments, so brace counting is reliable
  2. excise `#[cfg(test)]` modules, so "production code" means production code
  3. hand back the body of a named function, so a rule can say "inside `step`"

Why not regex, and why not tree-sitter:

A regex version of the "atomic consumed but never raised" check was written
first, and it **missed the bug that broke the BIOS boot** (`9354fd3`). The reason
is instructive: it treated "production code" as everything before the first
`#[cfg(test)]`, and `sh2.rs` has one at line 2576 -- some 4,700 lines before the
end of the file -- so half the production code was silently discarded. Regex does
not know where a module begins or ends, and every rule here is about *scope*.

tree-sitter would be more robust, and is the right upgrade if this file starts
creaking. It is deliberately avoided for now: the quality gate refuses to install
tools behind the user's back, so every dependency is one more way for the gate to
be unavailable on the machine that needs it. Brace counting over literal-stripped
source is exact for this codebase, and the checks are validated against known
answers (see `golden_rules.py`'s `--self-test`).
"""
from __future__ import annotations

import re
from pathlib import Path


def strip_literals(src: str) -> str:
    """Replace string/char literal contents and comments with spaces.

    Length and line structure are preserved, so byte offsets and line numbers
    computed on the result still point at the original source.
    """
    out = list(src)
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        # line comment
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            while i < n and src[i] != "\n":
                out[i] = " "
                i += 1
            continue
        # block comment (Rust nests them)
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            depth = 0
            while i < n:
                if src[i] == "/" and i + 1 < n and src[i + 1] == "*":
                    depth += 1
                    out[i] = out[i + 1] = " "
                    i += 2
                    continue
                if src[i] == "*" and i + 1 < n and src[i + 1] == "/":
                    depth -= 1
                    out[i] = out[i + 1] = " "
                    i += 2
                    if depth == 0:
                        break
                    continue
                if src[i] != "\n":
                    out[i] = " "
                i += 1
            continue
        # raw string: r"...", r#"..."#, r##"..."##
        if c == "r" and i + 1 < n and src[i + 1] in '#"':
            j = i + 1
            hashes = 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                close = '"' + "#" * hashes
                end = src.find(close, j + 1)
                end = n if end < 0 else end + len(close)
                for k in range(i, end):
                    if src[k] != "\n":
                        out[k] = " "
                i = end
                continue
        # normal string
        if c == '"':
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == '"':
                    j += 1
                    break
                j += 1
            for k in range(i, min(j, n)):
                if src[k] != "\n":
                    out[k] = " "
            i = j
            continue
        # char literal -- must not eat a lifetime like `'a`
        if c == "'":
            m = re.match(r"'(?:\\.|[^\\'])'", src[i:])
            if m:
                for k in range(i, i + m.end()):
                    out[k] = " "
                i += m.end()
                continue
        i += 1
    return "".join(out)


def _block_end(src: str, open_brace: int) -> int:
    """Index just past the `}` matching the `{` at `open_brace`."""
    depth, i, n = 0, open_brace, len(src)
    while i < n:
        if src[i] == "{":
            depth += 1
        elif src[i] == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def production_code(src: str) -> str:
    """Source with every `#[cfg(test)]` item blanked out.

    Handles the case that broke the regex version: a `#[cfg(test)]` module in the
    *middle* of a file, with real production code after it.
    """
    s = strip_literals(src)
    out = list(s)
    for m in re.finditer(r"#\[cfg\(test\)\]", s):
        brace = s.find("{", m.end())
        if brace < 0:
            continue
        # Only treat it as a block item if nothing but an item header sits
        # between the attribute and the brace (mod/fn/impl ...).
        header = s[m.end() : brace]
        if not re.fullmatch(r"[\sA-Za-z0-9_:<>,'()\[\]&+=-]*", header):
            continue
        for k in range(m.start(), _block_end(s, brace)):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def function_bodies(src: str, name_re: str) -> list[tuple[str, int, str, int]]:
    """Every `fn` whose name matches `name_re`, as (name, line, body, offset).

    `body` is literal-stripped, so a rule can search it without tripping over
    text inside strings or comments. `offset` is where the body starts in `src`,
    so a caller can map a hit back to the original text -- which is what the
    `// golden-rule-ok:` lookup needs, since comment bodies are blanked here.
    """
    s = strip_literals(src)
    found = []
    for m in re.finditer(r"\bfn\s+(" + name_re + r")\s*[(<]", s):
        brace = s.find("{", m.end())
        if brace < 0:
            continue
        # skip a trait method declaration with no body (`fn f(&self);`)
        semi = s.find(";", m.end())
        if 0 <= semi < brace:
            continue
        body = s[brace : _block_end(s, brace)]
        found.append((m.group(1), s.count("\n", 0, m.start()) + 1, body, brace))
    return found


def rust_files(root: Path = Path(".")):
    for p in sorted(root.rglob("*.rs")):
        parts = set(p.parts)
        if "target" in parts or "scratch" in parts:
            continue
        yield p
