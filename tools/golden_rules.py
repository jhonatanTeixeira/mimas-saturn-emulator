#!/usr/bin/env python3
"""Check the architectural "golden rules" that `docs/mimas-architecture-spec.md`
states, and that a normal build says nothing about.

Every rule here was broken for real on 2026-09-17, in one day, across two agents.
None of them were caught by `cargo build`, `cargo test` (396 green), `cargo
clippy`, or the coverage gate. They were found by hand-auditing the spec against
the code -- which is exactly the expensive, repeatable work a tool should be
doing instead.

Each rule cites the spec section it enforces, and each has an allowlist in the
code rather than in a side file: a list of exceptions that lives away from the
rule drifts out of date silently.

An exception at a specific site is taken with a `// golden-rule-ok: <reason>`
comment on the offending line or the line above -- same discipline as
`// no-assert:` in `assertionless_tests.py`. The marker makes the exception
visible and reviewable; it does not make it correct.

Run `--self-test` to check the checks: it replays them against the commits where
these bugs actually existed, and fails if a rule no longer catches the thing it
was written for.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rustscan import _block_end, function_bodies, production_code, rust_files, strip_literals

OK_MARKER = re.compile(r"//\s*golden-rule-ok:")


@dataclass
class Finding:
    rule: str
    spec: str
    path: str
    line: int
    detail: str


@dataclass
class Tree:
    """The source under test: either the working tree or a git revision."""

    rev: str | None = None
    virtual: dict[str, str] | None = None
    _cache: dict[str, str] = field(default_factory=dict)

    def files(self) -> list[str]:
        if self.virtual is not None:
            return sorted(self.virtual)
        if self.rev is None:
            return [str(p) for p in rust_files(Path("."))]
        out = subprocess.run(
            ["git", "ls-tree", "-r", "--name-only", self.rev],
            capture_output=True, text=True, check=True,
        ).stdout.split()
        return [f for f in out if f.endswith(".rs") and not f.startswith("scratch/")]

    def read(self, path: str) -> str:
        if self.virtual is not None:
            return self.virtual.get(path, "")
        if path not in self._cache:
            if self.rev is None:
                self._cache[path] = Path(path).read_text(errors="ignore")
            else:
                self._cache[path] = subprocess.run(
                    ["git", "show", f"{self.rev}:{path}"],
                    capture_output=True, text=True,
                ).stdout
        return self._cache[path]


def _line_of(src: str, idx: int) -> int:
    return src.count("\n", 0, idx) + 1


def _excused(src: str, idx: int) -> bool:
    """True if a `// golden-rule-ok:` covers this hit.

    Looked for on the hit's own line, or anywhere in the contiguous comment block
    immediately above it. Scanning the whole block matters: a real justification
    usually needs a paragraph, and requiring the marker to sit on the last line
    before the code would push the reason away from the marker -- which is the
    opposite of what the marker is for. Note `src` here is already
    literal-stripped, which blanks comment *bodies*, so this deliberately
    re-reads the raw text.
    """
    lines = src[:idx].split("\n")
    if lines and OK_MARKER.search(lines[-1]):
        return True
    end_of_hit = src.find("\n", idx)
    hit_line = src[src.rfind("\n", 0, idx) + 1 : end_of_hit if end_of_hit > 0 else len(src)]
    if OK_MARKER.search(hit_line):
        return True
    for line in reversed(lines[:-1]):
        stripped = line.strip()
        if not stripped.startswith("//"):
            break
        if OK_MARKER.search(line):
            return True
    return False


def _test_only_files(tree: Tree) -> set[str]:
    """Files reachable only through a `#[cfg(test)]` module declaration.

    `production_code()` removes inline `#[cfg(test)] mod tests { ... }` blocks,
    but a module declared as `#[cfg(test)] mod integration_tests;` lives in
    *other files* that look like ordinary source. Without this, every
    `thread::sleep` and `Instant::now()` in the test suite is reported as a
    golden-rule violation -- which is how this check first ran: 27 findings, 16
    of them test code.

    Resolved from the declarations themselves rather than from a filename
    convention, so moving or renaming a test module cannot silently re-include
    it.
    """
    out: set[str] = set()
    files = tree.files()
    pending: list[tuple[str, str]] = []
    for f in files:
        src = strip_literals(tree.read(f))
        for m in re.finditer(r"#\[cfg\(test\)\]\s*(?:pub\s+)?mod\s+(\w+)\s*;", src):
            pending.append((str(Path(f).parent), m.group(1)))
    seen = set()
    while pending:
        parent, name = pending.pop()
        if (parent, name) in seen:
            continue
        seen.add((parent, name))
        as_file = f"{parent}/{name}.rs"
        as_dir = f"{parent}/{name}"
        for f in files:
            if f == as_file or f == f"{as_dir}/mod.rs" or f.startswith(as_dir + "/"):
                out.add(f)
        # a gated module may declare further modules; those are test-only too
        for f in (as_file, f"{as_dir}/mod.rs"):
            if f in files:
                src = strip_literals(tree.read(f))
                for m in re.finditer(r"(?:pub\s+)?mod\s+(\w+)\s*;", src):
                    pending.append((str(Path(f).parent), m.group(1)))
    return out


def _core_files(tree: Tree) -> list[str]:
    skip = _test_only_files(tree)
    return [f for f in tree.files()
            if f.startswith("saturn-core/src/") and f not in skip]


# ---------------------------------------------------------------- rules


def rule_no_wall_clock(tree: Tree) -> list[Finding]:
    """Spec 1.5: component threads "may not reference the host wall clock at all".

    `throttle.rs` is the one sanctioned user (spec 1.4 scopes wall-clock batching
    to the CPU cores' own pacing). Everything else in `saturn-core/src` is a
    violation. This finds `sync.rs`'s `Instant::now()`, which is still open today.
    """
    allowed = {"saturn-core/src/throttle.rs"}
    out = []
    for f in _core_files(tree):
        if f in allowed:
            continue
        prod = production_code(tree.read(f))
        for m in re.finditer(r"Instant::now\s*\(", prod):
            if _raw_excused(tree.read(f), prod, m.start()):
                continue
            out.append(Finding("no-wall-clock", "1.5", f, _line_of(prod, m.start()),
                               "Instant::now() on a component thread"))
    return out


def rule_throttle_is_cpu_only(tree: Tree) -> list[Finding]:
    """Spec 1.4: wall-clock batching is scoped to "the CPU cores' own pacing, and
    nowhere else". Core 5 (SCSP) had its own `ClockThrottle` until today.

    Allowed: `sh2.rs` (Master/Slave), `lib.rs` (Core 4 is a real CPU, the M68K),
    and `throttle.rs` itself.
    """
    allowed = {
        "saturn-core/src/sh2.rs",
        "saturn-core/src/lib.rs",
        "saturn-core/src/throttle.rs",
    }
    out = []
    for f in _core_files(tree):
        if f in allowed:
            continue
        prod = production_code(tree.read(f))
        for m in re.finditer(r"ClockThrottle::new\s*\(", prod):
            if _raw_excused(tree.read(f), prod, m.start()):
                continue
            out.append(Finding("throttle-cpu-only", "1.4", f, _line_of(prod, m.start()),
                               "ClockThrottle on a non-CPU component thread"))
    return out


def rule_no_yield_now(tree: Tree) -> list[Finding]:
    """No `thread::yield_now()` in production.

    It ran once per emulated instruction from the initial commit until it was
    measured and removed (10.01s -> 4.13s to the same boot PC, a 2.4x speedup),
    and was then reintroduced by an automated pass the same day. A rule is
    cheaper than re-measuring it every time.
    """
    out = []
    skip = _test_only_files(tree)
    for f in tree.files():
        if f in skip:
            continue
        if not (f.startswith("saturn-core/src/") or f.startswith("saturn-frontend")):
            continue
        prod = production_code(tree.read(f))
        for m in re.finditer(r"yield_now\s*\(", prod):
            if _raw_excused(tree.read(f), prod, m.start()):
                continue
            out.append(Finding("no-yield-now", "1.5", f, _line_of(prod, m.start()),
                               "thread::yield_now() is a syscall per call site"))
    return out


def _raw_excused(raw: str, prod: str, idx: int) -> bool:
    """`_excused` against the raw source, aligned by offset.

    `strip_literals` preserves length and line structure, so an offset found in
    the stripped text points at the same place in the original -- which is where
    the comment text still exists.
    """
    return _excused(raw, idx)


def _while_condition(src: str, at: int) -> tuple[str, int] | None:
    """Split a `while` at `at` into (condition text, index of the body's `{`).

    Deliberately not a regex. The first version of this rule matched
    `while\s+[^\n{]*\.load\s*\([^\n{]*\{` -- i.e. it assumed the condition
    sits on one line and contains no braces. Both assumptions are wrong in Rust,
    and the failure is not hypothetical: rewriting

        while !shutdown.load(Ordering::Relaxed) {

    as the semantically identical

        while {
            !shutdown.load(Ordering::Relaxed)
        } {

    made the rule match nothing at all. A `// golden-rule-ok:` comment was left
    at the site, so the code read as though the check had fired and been
    excused, when in fact the check had gone blind -- and would have stayed
    blind for every future loop written that way. A rule that reformatting can
    defeat is not a rule; see `rustscan.py`'s header for the same lesson learned
    against `#[cfg(test)]`.

    So: scan forward, ignoring `{` nested inside `(`/`[` (closures in arguments),
    and treat a top-level `{...}` as part of the *condition* when another `{`
    follows it -- which is exactly how the compiler reads a block-expression
    condition.
    """
    n = len(src)
    cond_start = i = at
    while i < n:
        paren = 0
        j = i
        while j < n:
            c = src[j]
            if c in "([":
                paren += 1
            elif c in ")]":
                paren -= 1
            elif c == ";":          # ran past the statement -- not a `while` body
                return None
            elif c == "{" and paren <= 0:
                break
            j += 1
        if j >= n:
            return None
        close = _block_end(src, j)
        k = close
        while k < n and src[k].isspace():
            k += 1
        if k < n and src[k] == "{":
            # `{...}` was a block-expression condition; the real body starts at k.
            return src[cond_start:close], k
        return src[cond_start:j], j
    return None


def rule_no_atomic_poll_loop(tree: Tree) -> list[Finding]:
    """Spec 1.2: "We forbid busy-polling of atomic variables (`AtomicBool`) in
    tight loops."

    Catches Core 4's `while m68k_control.load(Ordering::Acquire) {`, in any
    formatting -- see `_while_condition`.
    """
    out = []
    for f in _core_files(tree):
        raw = tree.read(f)
        prod = production_code(raw)
        for m in re.finditer(r"\bwhile\b", prod):
            split = _while_condition(prod, m.end())
            if split is None:
                continue
            cond, _body = split
            if not re.search(r"\.load\s*\(", cond):
                continue
            if _raw_excused(raw, prod, m.start()):
                continue
            flat = " ".join(cond.split())
            out.append(Finding("no-atomic-poll-loop", "1.2", f, _line_of(prod, m.start()),
                               f"atomic load as loop condition: while {flat[:60]}"))
    return out


def rule_instruction_path_is_thin(tree: Tree) -> list[Finding]:
    """Spec 1.2b: at most one relaxed load of shared state on the per-instruction
    path; locks, read-modify-writes and notifies belong behind it.

    `Sh2::step`, `run_loop` and `service_pending_interrupt` run once per emulated
    instruction, so a mutex or an atomic read-modify-write there is paid tens of
    millions of times a second. Five `swap()` calls plus `is_shutdown()`'s global
    mutex on this path cost the interpreter measurably, and removing them was
    most of a 4.3x speedup.

    **Nesting depth is used as a proxy for "unconditional".** A lock behind a
    batching guard -- `if line_event == VBlankIn`, which fires once per 1814
    cycles -- is not on the per-instruction path, and the first version of this
    rule flagged all five of those. Depth <= 2 approximates "runs every time";
    anything deeper is assumed to sit behind a condition. It is a heuristic, and
    it is the weakest part of this file: it will miss a lock buried inside an
    `if` that is in fact always true, and it depends on how the code is braced.
    A deeper check would need real control-flow analysis.
    """
    banned = re.compile(r"\.(lock|swap|fetch_add|fetch_sub|fetch_or|fetch_and)\s*\(")
    out = []
    for f in ["saturn-core/src/sh2.rs"]:
        if f not in tree.files():
            continue
        prod = production_code(tree.read(f))
        for name, line, body, body_at in function_bodies(prod, r"step|run_loop|service_pending_interrupt"):
            for m in banned.finditer(body):
                if _raw_excused(tree.read(f), prod, body_at + m.start()):
                    continue
                depth = body.count("{", 0, m.start()) - body.count("}", 0, m.start())
                if depth > 2:
                    continue  # behind a guard -- see the note on depth above
                out.append(Finding("thin-instruction-path", "1.2b", f,
                                   line + body.count("\n", 0, m.start()),
                                   f"`{m.group(1)}()` at depth {depth} inside `{name}`"))
    return out


def rule_spawned_threads_park(tree: Tree) -> list[Finding]:
    """Spec 1.5: "Every other component thread is parked-until-woken."

    Only the two SH-2 cores may loop continuously. Every other spawned thread has
    to reach `park_while_inactive`. Core 5 (SCSP) never did -- the spec records it
    as a known gap, so it is allowlisted here *by name*, which keeps the exception
    visible instead of making the rule silent.
    """
    allowed_names = {"sh2-master", "sh2-slave", "scsp-synth"}
    out = []
    f = "saturn-core/src/lib.rs"
    if f not in tree.files():
        return out
    prod = production_code(tree.read(f))
    # Thread names are string literals, which `strip_literals` blanks -- so they
    # are read from the raw source, while the closure body is read from the
    # literal-stripped production code where brace counting is reliable.
    raw = tree.read(f)
    for m in re.finditer(r'\.name\s*\(\s*"([a-z0-9-]+)"', raw):
        name = m.group(1)
        if name in allowed_names:
            continue
        spawn = prod.find("spawn(", m.end())
        if spawn < 0:
            continue
        brace = prod.find("{", spawn)
        if brace < 0:
            continue
        depth, i = 0, brace
        while i < len(prod):
            if prod[i] == "{":
                depth += 1
            elif prod[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        body = prod[brace : i + 1]
        if "park_while_inactive" not in body:
            out.append(Finding("threads-park", "1.5", f, _line_of(raw, m.start()),
                               f"thread `{name}` never reaches park_while_inactive"))
    return out


def rule_atomic_has_producer(tree: Tree) -> list[Finding]:
    """A cross-thread flag that is consumed but never raised in production code.

    This is the shape that stopped the BIOS booting on 2026-09-17: a summary word
    (`hardware_events_any`) was read once per instruction to gate five hardware
    events, and every `store(true)` for it lived in `#[cfg(test)]`. Production
    never raised it, so SMPC IRQ, NMI, system reset, clock change and VDP1 draw
    end were dropped silently and permanently, while 396 tests stayed green.

    "Raised" is defined as the *inverse* of clearing: any `store`/`swap` whose
    value is not literally `false` or `0`. Matching truthy literals instead was
    the second wrong version of this check -- it missed
    `store(if is_352 { 2 } else { 1 }, ...)` and reported a healthy commit as
    broken. Matching any store at all was the first wrong version -- the consumer
    clears the flag with `store(false)`, so every flag looked produced. Both were
    caught by `--self-test`, which is the entire reason it exists.
    """
    out = []
    files = tree.files()
    decl = re.compile(r"\bpub\s+(\w+)\s*:\s*(?:std::sync::atomic::)?Atomic(?:Bool|U8|U16|U32|U64)")
    names: dict[str, str] = {}
    for f in _core_files(tree):
        prod = production_code(tree.read(f))
        for m in decl.finditer(prod):
            names[m.group(1)] = f
    prods = {f: production_code(tree.read(f)) for f in files}
    for name, where in sorted(names.items()):
        store_re = re.compile(r"\.\s*" + name + r"\s*\.\s*(?:store|swap)\s*\(\s*([^,]*)", re.S)
        use_re = re.compile(r"\.\s*" + name + r"\s*\.\s*(?:load|swap)\s*\(", re.S)
        raises = 0
        for p in prods.values():
            for m in store_re.finditer(p):
                if not re.fullmatch(r"\s*(?:false|0)\s*", m.group(1)):
                    raises += 1
        uses = sum(len(use_re.findall(p)) for p in prods.values())
        if uses and not raises:
            out.append(Finding("atomic-has-producer", "1.2b", where, 0,
                               f"`{name}` is read {uses}x in production but never raised there"))
    return out


def rule_field_is_written(tree: Tree) -> list[Finding]:
    """A struct field that is declared and read but never assigned.

    `LockStepSync::shutdown_flag` shipped like this: declared, initialised to
    `false`, read by `is_shutdown()`, and stored nowhere. Every `is_shutdown()`
    check in the system became dead code, including `PanicGuard`'s -- whose whole
    purpose is to stop one core's panic from hanging the others.
    """
    out = []
    for f in _core_files(tree):
        prod = production_code(tree.read(f))
        for m in re.finditer(r"^\s{4}(\w+)\s*:\s*(?:std::sync::atomic::)?Atomic\w+\s*,",
                             prod, re.M):
            name = m.group(1)
            writes = len(re.findall(
                r"\.\s*" + name + r"\s*\.\s*(?:store|swap|fetch_\w+)\s*\(", prod, re.S))
            reads = len(re.findall(r"\.\s*" + name + r"\s*\.\s*load\s*\(", prod, re.S))
            if reads and not writes:
                out.append(Finding("field-is-written", "-", f, _line_of(prod, m.start()),
                                   f"`{name}` is read but never written outside the constructor"))
    return out


def rule_no_thread_pool_or_async(tree: Tree) -> list[Finding]:
    """Spec 1.1: "We reject arbitrary thread pools tied to host core count,
    `tokio`/async runtimes, or OS-level process isolation (`fork`)."

    Each piece of silicon on the real board runs concurrently, so each gets its
    own dedicated OS thread. A pool would multiplex components onto fewer
    threads and reintroduce exactly the scheduling coupling the design exists to
    avoid; an async runtime would do the same with a different vocabulary.

    The cheapest rule here, and the only one of the six spec decisions with no
    check at all until now.
    """
    banned = [
        (r"\btokio\b", "tokio runtime"),
        (r"\brayon\b", "rayon thread pool"),
        (r"\basync\s+fn\b", "async fn"),
        (r"\.await\b", "async await"),
        (r"\bThreadPool\b", "thread pool"),
        (r"\bfork\s*\(", "process fork"),
        (r"available_parallelism", "thread count tied to host cores"),
    ]
    skip = _test_only_files(tree)
    out = []
    for f in tree.files():
        if f in skip or f.startswith("scratch/"):
            continue
        prod = production_code(tree.read(f))
        for pat, what in banned:
            for m in re.finditer(pat, prod):
                if _raw_excused(tree.read(f), prod, m.start()):
                    continue
                out.append(Finding("no-pool-or-async", "1.1", f,
                                   _line_of(prod, m.start()), what))
    return out


def rule_no_blanket_allow(tree: Tree) -> list[Finding]:
    """No crate- or module-wide `#![allow(...)]`.

    An inner attribute switches a lint off for everything below it, which makes
    the check weaker in a way nothing downstream can see. On 2026-09-17 a
    15-lint block was added at the top of `saturn-core/src/lib.rs` and took the
    clippy step from 37 errors to 3 -- not by fixing anything, but by silencing
    `bad_bit_mask` (three M68K opcode masks that can never match),
    `assertions_on_constants` (four `assert!(true)` tests naming VDP1 behaviour
    they do not verify), `overly_complex_bool_expr` and `if_same_then_else`. The
    defects stayed; only the reporting went away.

    Item-level `#[allow(...)]` is untouched and remains the supported escape
    hatch: it is scoped to one place, visible in review next to the code it
    excuses, and cannot silence a future instance somewhere else in the crate.
    """
    out = []
    skip = _test_only_files(tree)
    for f in tree.files():
        if f.startswith("scratch/"):
            continue
        raw = tree.read(f)
        prod = strip_literals(raw)
        for m in re.finditer(r"#!\[allow\(", prod):
            if _raw_excused(raw, prod, m.start()):
                continue
            end = prod.find(")]", m.start())
            body = prod[m.start() : end if end > 0 else m.start() + 60]
            lints = [x for x in re.findall(r"clippy::\w+|\b\w+\b", body)
                     if x not in ("allow", "clippy")]
            where = " (test module)" if f in skip else ""
            out.append(Finding("no-blanket-allow", "-", f, _line_of(prod, m.start()),
                               f"crate/module-wide allow of {len(lints)} lint(s){where}"
                               " -- use an item-level #[allow] with a comment instead"))
    return out


RULES = [
    rule_no_blanket_allow,
    rule_no_thread_pool_or_async,
    rule_no_wall_clock,
    rule_throttle_is_cpu_only,
    rule_no_yield_now,
    rule_no_atomic_poll_loop,
    rule_instruction_path_is_thin,
    rule_spawned_threads_park,
    rule_atomic_has_producer,
    rule_field_is_written,
]


def run(tree: Tree) -> list[Finding]:
    out = []
    for r in RULES:
        try:
            out.extend(r(tree))
        except Exception as e:  # a broken rule must not look like a clean tree
            out.append(Finding(r.__name__, "?", "<rule crashed>", 0, str(e)))
    return out


def self_test() -> int:
    """Replay the rules against the commits where these bugs actually existed.

    A check nobody has watched fail is not a check. Each case below is a known
    answer taken from `history.md` / `docs/current_review.md`.
    """
    cases = [
        ("9354fd3", "atomic-has-producer", True,
         "summary word never raised in production -- this broke the BIOS boot"),
        ("9354fd3", "field-is-written", True,
         "LockStepSync::shutdown_flag declared, read, never stored"),
        ("194572f", "atomic-has-producer", False,
         "the commit before -- boot worked, so the rule must stay quiet"),
        ("194572f", "thin-instruction-path", True,
         "five unconditional swap() calls per instruction in service_pending_interrupt"),
    ]
    # Cases with no commit to point at: the shapes a rule must not be blind to.
    # `while { cond } {` is not hypothetical -- it was written into
    # `lib.rs`'s Core 5 loop and silently matched nothing.
    POLL_PLAIN = """
        pub fn spawn_it(flag: AtomicBool) {
            while !flag.load(Ordering::Relaxed) { work(); }
        }
    """
    POLL_BLOCK = """
        pub fn spawn_it(flag: AtomicBool) {
            while {
                !flag.load(Ordering::Relaxed)
            } {
                work();
            }
        }
    """
    POLL_EXCUSED = """
        pub fn spawn_it(flag: AtomicBool) {
            // golden-rule-ok: this is the sanctioned exception, for <reason>
            while !flag.load(Ordering::Relaxed) { work(); }
        }
    """
    NO_POLL = """
        pub fn spawn_it(v: Vec<u8>) {
            while let Some(x) = v.pop() { work(x); }
        }
    """
    synthetic = [
        (POLL_PLAIN, "no-atomic-poll-loop", True, "plain one-line poll -- the original shape"),
        (POLL_BLOCK, "no-atomic-poll-loop", True,
         "same poll wrapped in a block-expression condition; the regex version saw nothing"),
        (POLL_EXCUSED, "no-atomic-poll-loop", False, "excused at the site, so the rule stays quiet"),
        (NO_POLL, "no-atomic-poll-loop", False, "`while let` with no atomic load"),
    ]

    ok = True
    for src, rule, expected, why in synthetic:
        tree = Tree(virtual={"saturn-core/src/synthetic.rs": src})
        hits = [f for f in run(tree) if f.rule == rule]
        got = bool(hits)
        mark = "\u2705" if got == expected else "\u274c"
        if got != expected:
            ok = False
        print(f"  {mark} <synthetic> / {rule}: expected {'a hit' if expected else 'silence'}, "
              f"got {len(hits)} \u2014 {why}")

    for rev, rule, expected, why in cases:
        hits = [f for f in run(Tree(rev=rev)) if f.rule == rule]
        got = bool(hits)
        mark = "✅" if got == expected else "❌"
        if got != expected:
            ok = False
        print(f"  {mark} {rev} / {rule}: expected {'a hit' if expected else 'silence'}, "
              f"got {len(hits)} — {why}")
    return 0 if ok else 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--rev", help="check a git revision instead of the working tree")
    ap.add_argument("--self-test", action="store_true",
                    help="replay the rules against commits with known answers")
    args = ap.parse_args()

    if args.self_test:
        print("Self-test (do the checks still catch what they were written for?):")
        return self_test()

    findings = run(Tree(rev=args.rev))
    if not findings:
        print("✅ No golden-rule violations")
        return 0
    print(f"❌ {len(findings)} golden-rule violation(s):")
    for f in sorted(findings, key=lambda x: (x.rule, x.path, x.line)):
        loc = f"{f.path}:{f.line}" if f.line else f.path
        print(f"   [{f.rule}] spec §{f.spec}  {loc}")
        print(f"      {f.detail}")
    print()
    print("   Fix the violation, or mark the specific site with")
    print("   `// golden-rule-ok: <reason>` if the rule is genuinely wrong there.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
