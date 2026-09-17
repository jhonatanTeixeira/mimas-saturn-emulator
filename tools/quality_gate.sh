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
OVERRIDDEN=()
show_cfg() { # name value default
    if [ "$2" != "$3" ]; then
        OVERRIDDEN+=("$1: $2 (default $3)")
        printf "  %-22s %-12s  ⚠️  OVERRIDDEN (default %s)\n" "$1" "$2" "$3"
    else
        printf "  %-22s %-12s\n" "$1" "$2"
    fi
}
echo ""
echo "Effective configuration:"
show_cfg "coverage min %"   "${MIMAS_COVERAGE_MIN:-90}"      "90"
show_cfg "speed floor %"    "${MIMAS_MIN_SPEED_PCT:-150}"    "150"
show_cfg "speed warn %"     "${MIMAS_WARN_SPEED_PCT:-170}"   "170"
show_cfg "expected boot PC" "${MIMAS_GATE_PC:-0x06001694}"   "0x06001694"
show_cfg "min WRAM accesses" "${MIMAS_GATE_MIN_WRAM:-2000000}" "2000000"
show_cfg "max source lines" "${MIMAS_LOC_MAX:-34000}"        "34000"
show_cfg "max binary MB"    "${MIMAS_BIN_MAX_MB:-16}"        "16"

# -----------------------------------------------------------------------------
echo ""
echo "1/8 🔨 Formatting"
if cargo fmt --all -- --check; then
    pass "Formatting"
else
    fail "Formatting — run 'cargo fmt --all'"
fi

# -----------------------------------------------------------------------------
echo ""
echo "2/8 🧹 Mess detect + complexity (clippy, READ-ONLY)"
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
echo "3/8 🏗️  Compilation"
if cargo build --release --workspace; then
    pass "Compilation"
else
    fail "Compilation"
fi

# -----------------------------------------------------------------------------
echo ""
echo "4/8 🧪 Tests"
if cargo test --workspace; then
    pass "Tests"
else
    fail "Tests"
fi

# -----------------------------------------------------------------------------
echo ""
echo "5/8 🕳️  Tests that assert nothing"
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
echo "6/8 📊 Coverage (target 90%)"
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
COVERAGE_MIN="${MIMAS_COVERAGE_MIN:-90}"
if ! command -v cargo-tarpaulin &> /dev/null; then
    fail "Coverage — cargo-tarpaulin not installed ('cargo install cargo-tarpaulin')"
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
echo "7/8 📏 Code size"
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
echo "8/8 💨 Smoke test (real BIOS boot)"
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
MIN_SPEED="${MIMAS_MIN_SPEED_PCT:-150}"
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
if [ ${#OVERRIDDEN[@]} -ne 0 ]; then
    echo "---------------------------------------------"
    echo "⚠️  ${#OVERRIDDEN[@]} threshold(s) overridden from the committed defaults:"
    for o in "${OVERRIDDEN[@]}"; do echo "     $o"; done
    echo "   A green result under lowered thresholds is not the same result."
fi
echo "---------------------------------------------"
if [ ${#FAILED[@]} -eq 0 ]; then
    echo "🎉 Quality Gate passed (${#PASSED[@]} checks)"
    exit 0
else
    echo "💥 Quality Gate FAILED — ${#FAILED[@]} of $(( ${#PASSED[@]} + ${#FAILED[@]} )) checks"
    exit 1
fi
