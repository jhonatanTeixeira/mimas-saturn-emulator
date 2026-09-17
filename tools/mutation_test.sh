#!/bin/bash
#
# Mutation testing over the architecturally interesting code.
#
# Mutation testing asks the only question coverage cannot: does a test actually
# *verify* the code, or merely execute it? It changes the source -- `&` to `|`,
# `>=` to `<`, a return value to its default -- and checks whether any test
# fails. A mutant that survives is a line no test is really checking.
#
# Measured here on `fetch_pixel` (vdp2.rs), whose only test asserts
# `pixel.is_some() || pixel.is_none()`: **121 of 159 mutants survived**. Coverage
# called that function covered. Replacing `&` with `|` in its pixel-bit
# extraction changed nothing any test could see.
#
# Usage:
#   tools/mutation_test.sh            # default percentage of the hot paths
#   tools/mutation_test.sh 25         # 25% of them
#   tools/mutation_test.sh 100        # everything (~97 min)
#   tools/mutation_test.sh --file saturn-core/src/vdp2.rs
set -u

HOTPATHS="tools/mutation_hotpaths.txt"
RE="$(paste -sd'|' "$HOTPATHS")"
export TMPDIR="${TMPDIR:-$PWD/.mutants-tmp}"
mkdir -p "$TMPDIR"

if ! command -v cargo-mutants &>/dev/null; then
    echo "❌ cargo-mutants not installed:  cargo install --locked cargo-mutants"
    exit 1
fi

# `--gitignore=true` is not optional here: without it cargo-mutants copies the
# whole tree into TMPDIR, including .venv (5.6 GB of CUDA libraries) and
# .models, and the run dies with ENOSPC partway through.
COMMON=(-p saturn-core --gitignore=true -j 4 --no-shuffle --timeout 30)

if [ "${1:-}" = "--file" ]; then
    echo "Mutating $2 in full"
    exec cargo mutants "${COMMON[@]}" --file "$2"
fi

PCT="${1:-${MIMAS_MUTANTS_PCT:-100}}"
TOTAL=$(cargo mutants --list "${COMMON[@]:0:1}" "${COMMON[@]:1:1}" --re "$RE" 2>/dev/null | wc -l)

if [ "$PCT" -ge 100 ]; then
    echo "Mutating all $TOTAL hot-path mutants (~$((TOTAL * 4 / 60)) min)"
    exec cargo mutants "${COMMON[@]}" --re "$RE"
fi

SHARDS=$(( 100 / PCT ))
# Rotating by commit count, not at random: re-running on the same commit gives
# the same shard, so a red result stays reproducible while you fix it, and the
# next commit sweeps a different slice. Over SHARDS commits the whole hot-path
# set is covered.
K=$(( $(git rev-list --count HEAD) % SHARDS ))
echo "Mutating shard $K/$SHARDS -- about $((TOTAL / SHARDS)) of $TOTAL hot-path mutants"
echo "(deterministic for this commit; the next commit takes a different slice)"
exec cargo mutants "${COMMON[@]}" --re "$RE" --shard "$K/$SHARDS"
