#!/bin/bash
#
# Mimas Quality Gate
#
# Steps do NOT bail on the first failure. Every step runs, and the summary at the
# end lists everything that failed. A gate that stops at the first red line hides
# the other four, and this one is expected to sit red on coverage for a while --
# see step 5.
#
# Nothing here installs tools. If something is missing the step says so and fails.

FAILED=()
PASSED=()
WARNED=()

pass() { PASSED+=("$1"); echo "✅ $1"; }
fail() { FAILED+=("$1"); echo "❌ $1"; }
warn() { WARNED+=("$1"); echo "⚠️  $1"; }

echo "============================================="
echo "🚀 Mimas Quality Gate"
echo "============================================="

# -----------------------------------------------------------------------------
# Effective configuration, printed every run.
#
# Every threshold below can be overridden from the environment, and that is a
# legitimate thing to do -- on the R36S the speed floor *must* be lowered, for
# instance. What is not legitimate is lowering a bar to turn a red check green
# and saying nothing. So the gate prints what it is actually enforcing, and
# marks anything that is not the committed default, which means a weakened run
# is visible in the log output itself rather than only in a diff nobody reads.
#
# There is exactly one honest way to turn each red check green:
#   coverage          -> write tests. Not --exclude-files, not a lower bar.
#   assert-nothing    -> write a real assertion, or justify it with
#                        `// no-assert: <reason>` at the test.
#   mess detect       -> fix the finding, or `#[allow(...)]` it *with a comment
#                        saying why clippy is wrong here*. NEVER `clippy --fix`:
#                        it was run once (37c85d4) and silently turned an m68k
#                        opcode guard into a no-op and muted the warnings that
#                        were flagging half-written VDP1 framebuffer code.
#   smoke test PC     -> if boot got *further*, verify and update the expected
#                        PC. If it got shorter, that is a regression, not a
#                        stale constant.
# -----------------------------------------------------------------------------
# Tightening a threshold is free. *Loosening* one requires a written reason,
# because a loosened threshold is how a red gate becomes green without anything
# being fixed -- observed here as `MIMAS_COVERAGE_MIN=80 ./tools/quality_gate.sh`.
#
# Printing a warning was not enough: it only makes the weakening *visible*, and
# whoever reports "the gate passed" can leave that line out. So a loosening with
# no `MIMAS_OVERRIDE_REASON` now FAILS the gate.
#
# The legitimate case still works. On the R36S the speed floor genuinely has to
# drop to ~15%; it just has to say so:
#
#   MIMAS_MIN_SPEED_PCT=15 MIMAS_OVERRIDE_REASON="measuring on R36S hardware" \
#       ./tools/quality_gate.sh
#
# Same discipline as `// no-assert:` and `// golden-rule-ok:`: the exception is
# allowed, the exception is justified where it is taken.
OVERRIDDEN=()
LOOSENED=()
OVERRIDE_REASON="${MIMAS_OVERRIDE_REASON:-}"

# show_cfg <name> <value> <default> <direction>
#   direction: min  -> lower is looser (floors: coverage, speed, WRAM)
#              max  -> higher is looser (ceilings: LOC, binary size)
#              exact-> any change is looser (the expected boot PC: swapping it
#                      for whatever a broken build produces is exactly how a
#                      functional regression turns green)
show_cfg() {
    local name="$1" val="$2" def="$3" dir="$4" loose=0
    if [ "$val" != "$def" ]; then
        case "$dir" in
            min)   awk -v v="$val" -v d="$def" 'BEGIN{exit !(v < d)}' && loose=1 ;;
            max)   awk -v v="$val" -v d="$def" 'BEGIN{exit !(v > d)}' && loose=1 ;;
            exact) loose=1 ;;
        esac
        OVERRIDDEN+=("$name: $val (default $def)")
        if [ "$loose" -eq 1 ]; then
            LOOSENED+=("$name: $val (default $def)")
            printf "  %-22s %-12s  ⚠️  LOOSENED (default %s)\n" "$name" "$val" "$def"
        else
            printf "  %-22s %-12s  ↑  tightened (default %s)\n" "$name" "$val" "$def"
        fi
    else
        printf "  %-22s %-12s\n" "$name" "$val"
    fi
}
echo ""
echo "Effective configuration:"
show_cfg "coverage min %"   "${MIMAS_COVERAGE_MIN:-90}"      "90"          min
show_cfg "speed floor %"    "${MIMAS_MIN_SPEED_PCT:-190}"    "190"         min
show_cfg "speed warn %"     "${MIMAS_WARN_SPEED_PCT:-170}"   "170"         min
show_cfg "expected boot PC" "${MIMAS_GATE_PC:-0x06001694}"   "0x06001694"  exact
show_cfg "min WRAM accesses" "${MIMAS_GATE_MIN_WRAM:-2000000}" "2000000"   min
show_cfg "max source lines" "${MIMAS_LOC_MAX:-34000}"        "34000"       max
show_cfg "max binary MB"    "${MIMAS_BIN_MAX_MB:-16}"        "16"          max

# Scope, not a threshold, so it is not run through show_cfg: the 90% floor is
# unchanged either way, what changes is the set of lines it applies to. Empty
# (the default) means the whole tree.
COVERAGE_COMMITS="${MIMAS_COVERAGE_COMMITS:-}"
if [ -n "$COVERAGE_COMMITS" ]; then
    printf "  %-22s %-12s  ◑  SCOPED (default: whole tree)\n" "coverage scope" "$COVERAGE_COMMITS"
else
    printf "  %-22s %-12s\n" "coverage scope" "whole tree"
fi

if [ ${#LOOSENED[@]} -ne 0 ] && [ -z "$OVERRIDE_REASON" ]; then
    echo ""
    echo "❌ ${#LOOSENED[@]} threshold(s) loosened with no reason given:"
    for l in "${LOOSENED[@]}"; do echo "     $l"; done
    echo ""
    echo "   Loosening a threshold does not make the underlying problem go away,"
    echo "   so it has to be stated. Re-run with, for example:"
    echo "     MIMAS_OVERRIDE_REASON=\"why this is legitimate\" ./tools/quality_gate.sh"
    echo ""
    echo "   If the goal was to make a red step green: fix the step instead."
    exit 1
fi
if [ -n "$OVERRIDE_REASON" ] && [ ${#LOOSENED[@]} -ne 0 ]; then
    echo ""
    echo "  Override reason: $OVERRIDE_REASON"
fi

# -----------------------------------------------------------------------------
echo ""
echo "1/9 🔨 Formatting"
if cargo fmt --all -- --check; then
    pass "Formatting"
else
    fail "Formatting — run 'cargo fmt --all'"
fi

# -----------------------------------------------------------------------------
echo ""
echo "2/9 🧹 Mess detect + complexity (clippy, READ-ONLY)"
#
# NEVER run `cargo clippy --fix` against this codebase from here or anywhere else.
# It was run once and auto-committed (37c85d4). Most of it was harmless, but
# `needless_return` rewrote m68k.rs's
#     if (opcode & 0xFFF0) == 0x4E60 || (opcode & 0xFFF0) == 0x4E68 { return; }
# into
#     if (opcode & 0xFFF0) == 0x4E60 || (opcode & 0xFFF0) == 0x4E68 {}
# in a file that is otherwise a chain of `if <opcode matches> { ...; return; }`
# guards, and silenced six `unused variable` warnings in the VDP1 FBCR handler by
# prefixing them with `_` -- warnings that were the only signal that the
# framebuffer-swap logic there is half-written. In an emulator, "redundant" is
# frequently load-bearing intent.
#
# Reading clippy's output, by contrast, pays for itself: this same run currently
# reports three M68K opcode masks that can never match (dead decode branches) and
# a `|| true` that makes a VDP1 mask check pointless. Triage by hand, fix by hand.
#
# Complexity thresholds live in clippy.toml, calibrated against this codebase.
if cargo clippy --workspace --all-targets -- \
        -D warnings -W clippy::cognitive_complexity; then
    pass "Mess detect + complexity"
else
    fail "Mess detect + complexity — triage by hand, do NOT run 'clippy --fix'"
fi

# -----------------------------------------------------------------------------
echo ""
echo "3/9 🏗️  Compilation"
if cargo build --release --workspace; then
    pass "Compilation"
else
    fail "Compilation"
fi

# -----------------------------------------------------------------------------
echo ""
echo "4/9 🧪 Tests"
if cargo test --workspace; then
    pass "Tests"
else
    fail "Tests"
fi

# -----------------------------------------------------------------------------
echo ""
echo "5/9 🏛️  Golden rules (architecture spec)"
#
# Checks the invariants `docs/mimas-architecture-spec.md` states and that no
# compiler enforces. Every rule in `tools/golden_rules.py` was broken for real on
# 2026-09-17, across two agents, in one day -- while `cargo build`, `cargo test`
# (396 green), `cargo clippy` and the coverage step all stayed happy. One of the
# breakages stopped the BIOS booting.
#
# `--self-test` first: it replays the rules against the commits where those bugs
# existed and fails if a rule stopped catching the thing it was written for. A
# check nobody has watched fail is not a check.
if ! python3 tools/golden_rules.py --self-test; then
    fail "Golden rules — SELF-TEST failed, the checks themselves are broken"
elif python3 tools/golden_rules.py; then
    pass "Golden rules"
else
    fail "Golden rules — see the spec section cited on each finding"
fi

# -----------------------------------------------------------------------------
echo ""
echo "6/9 🕳️  Tests that assert nothing"
#
# Deliberately separate from coverage, because coverage is blind to this: a test
# that runs code and asserts nothing reports 100% coverage of that code. It is
# also separate from clippy, which catches `assert!(true)` (it found four in
# vdp.rs) but not a test body with no assertion at all.
#
# Neither catches a semantic tautology -- `assert!(pixel.is_some() ||
# pixel.is_none())` in vdp2.rs passes every structural check and was found by
# reading. Mutation testing (cargo-mutants) would catch that class; it is too
# slow for a per-commit gate and is not wired here.
if python3 tools/assertionless_tests.py; then
    pass "No assertion-free tests"
else
    fail "Tests that assert nothing — add a real assertion or a '// no-assert: <reason>'"
fi

# -----------------------------------------------------------------------------
echo ""
echo "7/9 📊 Coverage (target 90%)"
#
# 90%, enforced, with NO --exclude-files. Adding exclusions until the number
# reaches the target measures nothing except how many exclusions were added.
# Whatever is below 90% is technical debt and is recorded as such in
# .development/current_bugs.md -- not hidden behind a flag.
#
# Measured 2026-09-17: 65.03% (5853/9000 lines). The gap is roughly 2,500 lines
# of coverage, concentrated in m68k.rs (24%), vdp2_regs.rs (35%), scsp.rs (46%),
# scu_dsp.rs (61%), lib.rs (61%) and the untested frontend binaries (0%).
# `--ignore-tests` excludes the test functions' own bodies from the denominator;
# that is what "coverage" means, not an exclusion of product code.
#
# MIMAS_COVERAGE_COMMITS scopes the same floor to one commit range instead of
# the tree. That is not a lower bar -- it is the same 90%, asked of the lines a
# change actually touched. It exists because the whole-tree number cannot answer
# the question that matters during review: a commit adding 200 untested lines
# moves 65.03% to 64.8%, which nobody notices. Old debt stays exactly as visible
# as it was; new work has to carry its own tests.
#
# It is reported separately everywhere, because "the gate passed" must not be
# able to mean "the gate passed on eleven lines".
COVERAGE_MIN="${MIMAS_COVERAGE_MIN:-90}"
if ! command -v cargo-tarpaulin &> /dev/null; then
    fail "Coverage — cargo-tarpaulin not installed ('cargo install cargo-tarpaulin')"
elif [ -n "$COVERAGE_COMMITS" ]; then
    echo "   scope: $COVERAGE_COMMITS (tree total measured but NOT enforced)"
    if ! cargo tarpaulin --ignore-tests --skip-clean --out Lcov --out Html; then
        fail "Coverage — cargo-tarpaulin failed to produce a report"
    elif python3 tools/diff_coverage.py --lcov lcov.info \
            --range "$COVERAGE_COMMITS" --min "$COVERAGE_MIN"; then
        pass "Diff coverage >= ${COVERAGE_MIN}% on ${COVERAGE_COMMITS} (whole-tree total NOT enforced)"
    else
        fail "Diff coverage below ${COVERAGE_MIN}% on ${COVERAGE_COMMITS} — new or changed lines no test reaches"
    fi
else
    if cargo tarpaulin --ignore-tests --skip-clean \
            --fail-under "$COVERAGE_MIN" --out Html; then
        pass "Coverage >= ${COVERAGE_MIN}%"
    else
        fail "Coverage below ${COVERAGE_MIN}% — known technical debt, see .development/current_bugs.md"
    fi
fi

# -----------------------------------------------------------------------------
echo ""
echo "8/9 📏 Code size"
#
# The previous version of this step ran `find . -name "*.rs" | xargs wc -l`,
# which walks target/ and reported 243,067 lines against a real source tree of
# 28,275 -- an 8.6x inflation that made the metric useless. Source only now.
LOC="$(find . -path ./target -prune -o -name '*.rs' -print \
       | grep -v '/target/' | xargs wc -l | tail -1 | awk '{print $1}')"
LOC_MAX="${MIMAS_LOC_MAX:-34000}"   # 28,275 on 2026-09-17, +20% headroom
BIN="target/release/saturn-frontend-native"
BIN_MAX_MB="${MIMAS_BIN_MAX_MB:-16}"  # 1.3 MB on 2026-09-17; the R36S cares

echo "Rust source lines (excluding target/): $LOC"
if [ "$LOC" -gt "$LOC_MAX" ]; then
    fail "Code size — $LOC source lines exceeds $LOC_MAX; raise MIMAS_LOC_MAX deliberately if the growth is real"
else
    pass "Code size — $LOC / $LOC_MAX source lines"
fi

if [ -f "$BIN" ]; then
    BIN_BYTES="$(stat -c%s "$BIN")"
    BIN_MB=$(( BIN_BYTES / 1048576 ))
    echo "Release binary: $(( BIN_BYTES / 1024 )) KB"
    if [ "$BIN_MB" -gt "$BIN_MAX_MB" ]; then
        fail "Binary size — ${BIN_MB}MB exceeds ${BIN_MAX_MB}MB"
    else
        pass "Binary size — ${BIN_MB}MB / ${BIN_MAX_MB}MB"
    fi
else
    warn "Binary size — $BIN not built, skipping"
fi

# -----------------------------------------------------------------------------
echo ""
echo "9/9 💨 Smoke test (real BIOS boot)"
#
# The previous version ran the binary and checked only its exit code -- so when
# 9354fd3 broke the emulator badly enough that Core 0 died at 0x2B0 after 4
# memory accesses, the binary still exited 0 and this gate went green. A smoke
# test that cannot fail is not a smoke test. It now asserts what the boot
# actually produces: the settle PC, real memory traffic, and emulated speed.
#
# ---------------------------------------------------------------------------
# WHEN BOOT PROGRESSES PAST THIS POINT, UPDATE THESE VALUES.
# A further settle PC is real progress, not a regression. Update MIMAS_GATE_PC
# and MIMAS_GATE_MIN_WRAM and say so in history.md. Do not delete the check to
# make it pass.
# ---------------------------------------------------------------------------
EXPECTED_PC="${MIMAS_GATE_PC:-0x06001694}"
MIN_WRAM="${MIMAS_GATE_MIN_WRAM:-2000000}"

# Speed floor as a percentage of a real 28.636 MHz SH-2, measured on the host
# running this gate. Reference: 186-189% on a Ryzen 5 3500X (Zen 2), 2026-09-17;
# 150 leaves ~20% headroom for noise and slower dev machines while still catching
# a serious regression (9354fd3 measured 0%). On the R36S target this same binary
# is expected to measure ~13-21%, so lower it there via the environment.
#
# Where this is going, for context, NOT a gate condition: the Master SH-2 needs
# ~100% of real time on ONE Cortex-A53 core for the R36S to be playable, and an
# A53 core is worth roughly 1/9 to 1/14 of a Zen 2 core on this workload. That
# puts the real target near 1000% on desktop x86 against today's 188% -- a 5-8x
# gap. See docs/mimas-performance-analysis.md 3.2.
MIN_SPEED="${MIMAS_MIN_SPEED_PCT:-190}"
WARN_SPEED="${MIMAS_WARN_SPEED_PCT:-170}"

BIOS_PATH="${MIMAS_BIOS_PATH:-}"
if [ -z "$BIOS_PATH" ]; then
    for candidate in "scratch/ra_system/saturn_bios.bin" "bios.bin" \
                     "../yabause/bios/saturn_bios.bin"; do
        [ -f "$candidate" ] && { BIOS_PATH="$candidate"; break; }
    done
fi

if [ -z "$BIOS_PATH" ] || [ ! -f "$BIOS_PATH" ]; then
    fail "Smoke test — no BIOS found. This is the only step that exercises the real emulator, so it fails rather than skipping. Set MIMAS_BIOS_PATH."
elif [ ! -f "target/release/saturn-frontend-native" ]; then
    fail "Smoke test — release binary missing (compilation step failed)"
else
    echo "Booting real BIOS: $BIOS_PATH"
    SMOKE_OUT="$(mktemp)"
    trap 'rm -f "$SMOKE_OUT"' EXIT
    # Stops itself once Core 0's PC settles (~2-3s); 30s only bounds a hang.
    if ! MIMAS_BOOT_WATCH_SECS=30 ./target/release/saturn-frontend-native \
            --bios "$BIOS_PATH" > "$SMOKE_OUT" 2>&1; then
        fail "Smoke test — emulator exited non-zero"
        cat "$SMOKE_OUT"
    else
        FINAL_PC="$(grep -oE 'PC moved from 0x[0-9A-Fa-f]+ to 0x[0-9A-Fa-f]+' "$SMOKE_OUT" \
                    | grep -oE '0x[0-9A-Fa-f]+$' | tail -1)"
        WRAM_TOTAL="$(grep -oE 'Reads=[0-9]+, Writes=[0-9]+' "$SMOKE_OUT" | tail -1 \
                      | grep -oE '[0-9]+' | paste -sd+ | bc)"
        SPEED_PCT="$(grep -oE '\(([0-9.]+)% of real' "$SMOKE_OUT" | grep -oE '[0-9.]+' | tail -1)"

        if [ "$FINAL_PC" != "$EXPECTED_PC" ]; then
            fail "Smoke test — boot reached ${FINAL_PC:-nothing}, expected $EXPECTED_PC (a *further* PC may be progress: verify, then update MIMAS_GATE_PC)"
        else
            pass "Smoke test — settle PC $FINAL_PC"
        fi

        if [ -z "$WRAM_TOTAL" ] || [ "$WRAM_TOTAL" -lt "$MIN_WRAM" ]; then
            fail "Smoke test — only ${WRAM_TOTAL:-0} WRAM accesses, expected >= $MIN_WRAM (exited cleanly without doing the work)"
        else
            pass "Smoke test — WRAM accesses $WRAM_TOTAL"
        fi

        if [ -z "$SPEED_PCT" ]; then
            fail "Smoke test — could not read emulated speed from run output"
        elif awk -v s="$SPEED_PCT" -v m="$MIN_SPEED" 'BEGIN{exit !(s < m)}'; then
            fail "Smoke test — emulated speed ${SPEED_PCT}% of real, below the ${MIN_SPEED}% floor"
        elif awk -v s="$SPEED_PCT" -v w="$WARN_SPEED" 'BEGIN{exit !(s < w)}'; then
            warn "Smoke test — emulated speed ${SPEED_PCT}%, above the floor but below the ${WARN_SPEED}% warning line"
        else
            pass "Smoke test — emulated speed ${SPEED_PCT}% of real SH-2"
        fi
    fi
fi

# -----------------------------------------------------------------------------
echo ""
echo "============================================="
echo "Summary"
echo "============================================="
for p in "${PASSED[@]}"; do echo "  ✅ $p"; done
for w in "${WARNED[@]}"; do echo "  ⚠️  $w"; done
for f in "${FAILED[@]}"; do echo "  ❌ $f"; done
if [ ${#LOOSENED[@]} -ne 0 ]; then
    echo "---------------------------------------------"
    echo "⚠️  ${#LOOSENED[@]} threshold(s) LOOSENED from the committed defaults:"
    for l in "${LOOSENED[@]}"; do echo "     $l"; done
    echo "     reason: $OVERRIDE_REASON"
    echo "   A green result under loosened thresholds is not the same result,"
    echo "   and should not be reported as one."
elif [ ${#OVERRIDDEN[@]} -ne 0 ]; then
    echo "---------------------------------------------"
    echo "↑  ${#OVERRIDDEN[@]} threshold(s) tightened from the committed defaults:"
    for o in "${OVERRIDDEN[@]}"; do echo "     $o"; done
fi
if [ -n "$COVERAGE_COMMITS" ]; then
    echo "---------------------------------------------"
    echo "◑  Coverage was checked on ${COVERAGE_COMMITS} only, not the whole tree."
    echo "   The whole-tree figure is still below its floor and is still debt;"
    echo "   this run did not measure it. Say so when reporting the result."
fi
echo "---------------------------------------------"
if [ ${#FAILED[@]} -eq 0 ]; then
    echo "🎉 Quality Gate passed (${#PASSED[@]} checks)"
    exit 0
else
    echo "💥 Quality Gate FAILED — ${#FAILED[@]} of $(( ${#PASSED[@]} + ${#FAILED[@]} )) checks"
    exit 1
fi
