# Current bugs — SH-2 core

Full detail lives in `docs/implementation-plans/sh2-cpu.md` §0.9 ("Tracked defects") —
this file is the running index `CLAUDE.md` asks for, not a duplicate of the prose.

## Resolved (2026-08-08)

- **D-21** — `MAC.W` (`0x400F`) charged 1 cycle instead of 3; `MOV.L @(R0,Rm),Rn`
  (`0x000E`) charged 3 instead of 1 — wrong mask in `get_base_cycles` conflated the two.
  Fixed `saturn-core/src/sh2.rs`. Test: `test_cycles_d21_mac_w_and_movl_r0_indexed`.
- **D-22** — `STC.L {SR,GBR,VBR},@-Rn` (push) had no cost entry in `get_base_cycles` at
  all, silently charged the 1-cycle default instead of the real 2. Fixed. Test:
  `test_cycles_d22_stc_l_control_regs`.
- **D-23** — `RTS`/`RTE`/`SLEEP` cost functions used exact `opcode ==` instead of
  `& 0xF0FF`, so nibble B ≠ 0 dropped the cost to the 1-cycle default even though
  `execute()` already dispatches these correctly for any B (real hardware ignores B for
  these 0-operand forms — same class as the already-fixed D-5, but in the separate cost
  function, missed by Phase 1). Fixed. Test: `test_cycles_d23_nibble_b_is_dont_care`.

All three found via an exhaustive opcode-by-opcode audit of `get_base_cycles`
(`sh2.rs:654-753`) against `retroarch-cores/yabause/src/sh2int.c` (real cycle counts per
handler), prompted by a sibling project (`portal_to_another_world`) finding 8 analogous
decode-table bugs in its own SH-2 disassembler the same day using the same method. Full
citations and the audit methodology: `docs/implementation-plans/sh2-cpu.md` §0.2 status
update and §0.9. Verification: `cargo test --package saturn-core` → 79 passed in the `sh2`
module (was 76), 0 failed; `cargo test --workspace` → 226 passed, 0 failed. D-21's test was
confirmed to actually fail before the fix (reverted locally, saw `left: 1, right: 3`,
restored) — not a test that would have passed regardless.

The same audit also exhaustively proved `execute()`'s decode-set (as opposed to
cycle-cost) matches `sh2int.c` for all 65536 opcodes with zero divergence — see
`docs/hardware-reference/sh2-cpu.md` §9.11. It did **not** re-verify per-handler semantic
correctness beyond a handful of spot-checks — that's open work, not claimed as done.

## Not individually re-verified this pass (D-1 .. D-20)

`docs/implementation-plans/sh2-cpu.md` §0.9 has full detail on each. Whether each is
still open or was closed during Phase 1/2 (both marked `[x]` in the same document) was
**not** re-checked item-by-item in this pass — that cross-check is itself open work.
D-1 (`OR`/`XOR` swap) was independently spot-checked as fixed while investigating D-21/22/23
(`sh2.rs:2861/2865` map correctly today: `0xCA00`→XOR, `0xCB00`→OR); the rest (D-2 through
D-20) are unverified here.

## Known Unknowns

These are VDP1 register fields that are stored but deliberately left undecoded because their effect on real hardware is not determinable from the source (see `docs/hardware-reference/vdp1.md` §12 items 1-3):

- **TVM2** (TVMR bit 2)
- **EOS** (FBCR bit 4)
- **HSS** (CMDPMOD bit 12)
- **PCLP** (CMDPMOD bit 11)
