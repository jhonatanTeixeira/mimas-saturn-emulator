import re

with open("docs/mimas-architecture-spec.md", "r") as f:
    content = f.read()

# Add 1.2b
new_1_2b = """
---

### 1.2b. Active IRQ Sampling via Single `Relaxed` Load
* **Decision**: For sampling hardware signals *within* a CPU's execution loop (where parking on a `Condvar` is impossible because the CPU is actively executing instructions), polling is restricted to a maximum of one `Relaxed` load per instruction.
* **Details**:
  * Real hardware samples the IRQ lines (like the SH-2's IRL pins) at instruction boundaries. A CPU cannot park while it is running code.
  * To model this without violating the spirit of §1.2 or destroying performance, the CPU loop may read a single summary word (e.g., an `AtomicU8` representing the highest pending interrupt level) using `Ordering::Relaxed` exactly once per instruction.
  * Read-Modify-Write (RMW) operations, locking, or notifying must only occur *after* this relaxed load indicates actionable work. This serves as a low-overhead, honest representation of a physical pin check, complementing §1.2's rule for thread-to-thread wakeup.
"""

content = re.sub(r'(### 1\.3\. Region-Granular Shared Memory)', new_1_2b.strip() + '\\n\\n---\\n\\n\\1', content)

# Update 1.5
note_1_5 = """  * **Known gap, not a rule exception**: Core 5 (SCSP) still runs a continuous loop today...
  * **Known violations**: `SLEEP` opcodes currently spin without parking, and Core 1 fails to park correctly after an `SSHOFF` command. See `CLAUDE.md`'s "Known architecture debt" for the current, honest state of every component thread against this rule."""

content = re.sub(r'  \* \*\*Known gap, not a rule exception\*\*: Core 5 \(SCSP\) still runs a continuous loop today, because real hardware\'s audio synthesizer genuinely never stops regardless of what any CPU is doing -- this is tracked as follow-up work \(moving its own pacing onto the same cycle-batched model, or an equivalent\), not treated as satisfying this section\'s decision\. See `CLAUDE\.md`\'s "Known architecture debt" for the current, honest state of every component thread against this rule\.', note_1_5, content)

with open("docs/mimas-architecture-spec.md", "w") as f:
    f.write(content)
