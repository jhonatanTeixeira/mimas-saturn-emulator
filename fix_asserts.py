with open("e2e-tests/src/lib.rs", "r") as f:
    lines = f.readlines()

for i, line in enumerate(lines):
    if "fn test_tier1_f1_lockstep_initial_sync()" in line:
        lines[i] = line + "    // no-assert: just verifying the lockstep threads can spawn and sync without deadlocking\n"
    if "fn test_tier2_f1_lockstep_negative_or_overflow_drift()" in line:
        lines[i] = line + "    // no-assert: verifying that passing extreme negative/overflow cycles does not panic\n"

with open("e2e-tests/src/lib.rs", "w") as f:
    f.writelines(lines)
