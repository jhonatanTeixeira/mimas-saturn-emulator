# Implementation Plan — SH-2 CPU Cores (Master & Slave)

**Target:** bring `saturn-core/src/sh2.rs` to parity with
`docs/hardware-reference/sh2-cpu.md`, in service of booting the real Saturn BIOS further
and eventually completely.

**Ground truth:** `docs/hardware-reference/sh2-cpu.md` (cited below as **HR §n**). Every
claim in that document carries a `yabause/src/<file>:<line>` citation; this plan does not
re-derive hardware facts, it diffs them against the Rust.

**Current code:** `saturn-core/src/sh2.rs` (2377 lines, cited below as `sh2.rs:<line>`),
`saturn-core/src/throttle.rs`, `saturn-core/src/shared_buffers.rs`,
`saturn-core/src/lib.rs` (`SaturnSystem::start` wiring), `saturn-core/src/sync.rs`,
`saturn-core/src/bus_arbiter.rs`.

**Scope boundaries.** SMPC command execution, SCU DMA and the CD-block command handler
physically live inside `sh2.rs` today (`smpc_execute_command` `sh2.rs:826`,
`execute_scu_dma` `sh2.rs:1536`, `execute_cdrom_command` `sh2.rs:1641`). Those are *not*
SH-2 hardware and are owned by `docs/implementation-plans/smpc-peripheral.md`,
`scu.md` and `cs2-cdblock.md` respectively. This plan touches them only where the SH-2
side of the boundary (address decode, on-chip register dispatch, interrupt injection)
has to change.

**Tracking-doc status at time of writing.** `.development/current_blocker.md`,
`.development/current_bugs.md`, `.development/ROADMAP.md` and `.development/TASKS.md`
are all **zero-byte files**. There is therefore no existing "known blocker" signal to
complement — this plan is written from a fresh code-vs-reference diff, and §0.9 lists
exactly which items should be seeded back into `current_bugs.md` immediately.
`.development/phased_development_plan.md` is a coarse greenfield plan whose "Phase 2:
Master SH-2 Core and Instruction Interpreter" is marked ✅ Completed; §0 below is the
evidence that "completed" means "133 of 142 mnemonics decode, 4 of them incorrectly",
not parity.

---

## 0. Current-state assessment

### 0.1 What `sh2.rs` actually is today

| Area | Present | Notes |
|---|---|---|
| Register file | `registers[16]`, `pc`, `pr`, `sr`, `gbr`, `vbr`, `mach`, `macl` (`sh2.rs:65-173`) | Matches HR §2. No `S` bit accessor, no `interrupts[]` queue, no `onchip` struct, no cache. |
| SR flags | `SR_T`(bit0), `SR_M`(bit8), `SR_Q`(bit9), `SR_IMASK_SHIFT`=4 (`sh2.rs:179-184`) | Bit positions match HR §3. **`S` (bit 1) has no constant and is never read** — correct only because `MAC.L`/`MAC.W` don't exist yet. |
| `SR_WRITE_MASK` | `0x0000_03F3` (`sh2.rs:63`) | Matches HR §3.2. **Applied at exactly one of three sites** — see D-2/D-3. |
| Decode | Hand-written cascade of `match opcode & mask` blocks (`sh2.rs:975-1453`) | Not a 65536-entry table (HR §5.3) and not required to be; the cascade is verified non-aliasing (§0.3). |
| Memory map | `MemRegion` enum + `translate()` (`sh2.rs:12-37`, `:338-381`) | 16 regions. Address folded with `& 0x0FFF_FFFF`; see D-9/D-10. |
| On-chip regs | DIVU only (`read_onchip` `sh2.rs:1456`, `write_onchip` `sh2.rs:1470`) | 8 of ~60 documented registers. **Long-word access only** — see D-11. |
| Interrupts | 4 hardcoded `bool`/`AtomicBool` flags + fixed if/else priority chain (`service_pending_interrupt` `sh2.rs:908`) | No queue, no NMI, no on-chip sources, no IPRA/IPRB. |
| Cache | none | HR §12 entirely unimplemented; cache-array and purge address regions mis-decode (D-9). |
| Cycles | flat `+= 2` per `step()` (`sh2.rs:788`) | HR gives per-instruction costs plus memory wait states (HR §1.2, §9). |
| Exec loop | `run_loop` (`sh2.rs:1674`) — one instruction per iteration, wall-clock VBLANK scheduler, `ClockThrottle`, `LockStepSync::sync_core` | No `Exec(cycles)` batch entry point; consequently no place to hang `FRTExec`/`WDTExec`/`DMAProc` (HR §7). |
| Diagnostics | `REG_ACCESS_LOG` / `log_reg_access_once` (`sh2.rs:240-257`) | Dedup-logs SMPC/VDP1/VDP2/SCU/CS2 register byte accesses once each, master core only. Keep. |
| Tests | 35 `#[cfg(test)]` unit tests (`sh2.rs:1740-2377`) + ~14 SH-2-touching e2e tests | Inventory and breakage risk in §0.8 and Appendix B. |

### 0.2 Opcode coverage — headline numbers

HR §9 documents **142 distinct mnemonics**. Of those:

- **133 decode** in `execute()`.
- **9 do not decode at all** and fall through to the silent no-op at `sh2.rs:1445-1453`.
- **4 of the 133 decode but execute incorrectly** (D-1, D-2, D-3 below; plus TAS.B's
  atomicity gap D-4).

The silent-no-op fallthrough is itself the single most dangerous property of the current
interpreter: an unimplemented opcode does not stop, does not log, and does not raise the
illegal-instruction exception — PC simply advances 2 and execution continues into
whatever follows. Every one of the 9 gaps below is therefore a *silent* wrong-execution
bug, not a crash.

### 0.3 Opcode coverage — full enumeration

Legend: **OK** = present and semantics match HR. **MISSING** = not decoded, silently
no-ops. **WRONG** = decoded, wrong behaviour (has a D-number).

#### HR §9.1 — data transfer, register / immediate (11)

| Mnemonic | Encoding | Mimas site | Status |
|---|---|---|---|
| `MOV Rm,Rn` | `0110 nnnn mmmm 0011` | `sh2.rs:1400` | OK |
| `MOV #imm,Rn` | `1110 nnnn iiiiiiii` | `sh2.rs:1084` | OK |
| `MOVA @(disp,PC),R0` | `1100 0111 dddddddd` | `sh2.rs:1137` | OK (`(pc+2)&!3`, and `pc` is already `instr+2`) |
| `MOVT Rn` | `0000 nnnn 0010 1001` | `sh2.rs:1025` | OK |
| `SWAP.B Rm,Rn` | `0110 nnnn mmmm 1000` | `sh2.rs:1420` | OK |
| `SWAP.W Rm,Rn` | `0110 nnnn mmmm 1001` | `sh2.rs:1425` | OK (`rotate_left(16)` ≡ HR formula) |
| `XTRCT Rm,Rn` | `0010 nnnn mmmm 1101` | `sh2.rs:1296` | OK |
| `EXTS.B Rm,Rn` | `0110 nnnn mmmm 1110` | `sh2.rs:1430` | OK |
| `EXTS.W Rm,Rn` | `0110 nnnn mmmm 1111` | `sh2.rs:1431` | OK |
| `EXTU.B Rm,Rn` | `0110 nnnn mmmm 1100` | `sh2.rs:1428` | OK |
| `EXTU.W Rm,Rn` | `0110 nnnn mmmm 1101` | `sh2.rs:1429` | OK |

#### HR §9.2 — loads (17): all 17 OK

`MOV.B/W/L @Rm,Rn` (`sh2.rs:1397-1399`); `MOV.B/W/L @Rm+,Rn` (`:1401-1418`, all three
carry the `if n != m` guard HR §9.2 requires); `MOV.B/W/L @(R0,Rm),Rn` (`:1037-1039`);
`MOV.B @(disp,Rm),R0` / `MOV.W @(disp,Rm),R0` (`:1440-1441`, correctly read the register
from nibble **C** per HR §9.2's `m is nibble C` note, with ×1/×2 scaling);
`MOV.L @(disp,Rm),Rn` (`:1049`, ×4); `MOV.B/W/L @(disp,GBR),R0` (`:1145-1147`, ×1/×2/×4);
`MOV.W @(disp,PC),Rn` (`:1059`); `MOV.L @(disp,PC),Rn` (`:1078`, `&!3` alignment present).

> Note on `MOV.W @(disp,PC),Rn`: HR §9.2 says the interpreter applies **no** PC alignment
> mask; `sh2.rs:1060` applies `& !1`. Equivalent for all even PCs, which is every real PC.
> Not a defect; recorded so a future reader doesn't "fix" it into a `& !3`.

#### HR §9.3 — stores (15): all 15 OK

`MOV.B/W/L Rm,@Rn` (`sh2.rs:1257-1259`); `MOV.B/W/L Rm,@-Rn` (`:1260-1277`, write-then-
decrement, so `MOV.L Rn,@-Rn` stores the *un*decremented value exactly as HR §9.8's note
requires); `MOV.B/W/L Rm,@(R0,Rn)` (`:1030-1032`); `MOV.B/W R0,@(disp,Rn)` (`:1438-1439`,
register from nibble C); `MOV.L Rm,@(disp,Rn)` (`:1044`); `MOV.B/W/L R0,@(disp,GBR)`
(`:1142-1144`).

#### HR §9.4 — arithmetic (30)

| Mnemonic | Encoding | Mimas site | Status |
|---|---|---|---|
| `ADD Rm,Rn` | `0011 nnnn mmmm 1100` | `sh2.rs:1371` | OK |
| `ADD #imm,Rn` | `0111 nnnn iiiiiiii` | `sh2.rs:1054` | OK |
| `ADDC Rm,Rn` | `0011 nnnn mmmm 1110` | `sh2.rs:1378` | OK (`overflowing_add` pair ≡ HR two-step carry) |
| `ADDV Rm,Rn` | `0011 nnnn mmmm 1111` | `sh2.rs:1385` | OK (`i32::overflowing_add` ≡ HR sign-count method) |
| `SUB Rm,Rn` | `0011 nnnn mmmm 1000` | `sh2.rs:1357` | OK |
| `SUBC Rm,Rn` | `0011 nnnn mmmm 1010` | `sh2.rs:1358` | OK |
| `SUBV Rm,Rn` | `0011 nnnn mmmm 1011` | `sh2.rs:1365` | OK |
| `NEG Rm,Rn` | `0110 nnnn mmmm 1011` | `sh2.rs:1427` | OK |
| `NEGC Rm,Rn` | `0110 nnnn mmmm 1010` | `sh2.rs:1426` | OK |
| `DT Rn` | `0100 nnnn 0001 0000` | `sh2.rs:1164` | OK |
| `CMP/EQ Rm,Rn` | `0011 nnnn mmmm 0000` | `sh2.rs:1302` | OK |
| `CMP/HS Rm,Rn` | `0011 nnnn mmmm 0010` | `sh2.rs:1303` | OK (unsigned) |
| `CMP/GE Rm,Rn` | `0011 nnnn mmmm 0011` | `sh2.rs:1304` | OK (signed) |
| `CMP/HI Rm,Rn` | `0011 nnnn mmmm 0110` | `sh2.rs:1355` | OK |
| `CMP/GT Rm,Rn` | `0011 nnnn mmmm 0111` | `sh2.rs:1356` | OK |
| `CMP/PL Rn` | `0100 nnnn 0001 0101` | `sh2.rs:1170` | OK |
| `CMP/PZ Rn` | `0100 nnnn 0001 0001` | `sh2.rs:1169` | OK |
| `CMP/STR Rm,Rn` | `0010 nnnn mmmm 1100` | `sh2.rs:1290` | OK |
| `CMP/EQ #imm,R0` | `1000 1000 iiiiiiii` | `sh2.rs:1092` | OK (sign-extended imm) |
| `DIV0S Rm,Rn` | `0010 nnnn mmmm 0111` | `sh2.rs:1278` | OK |
| `DIV0U` | `0000 xxxx 0001 1001` | `sh2.rs:984` | OK semantics, **exact-match only** (see D-5) |
| `DIV1 Rm,Rn` | `0011 nnnn mmmm 0100` | `sh2.rs:1305` | OK single-step; multi-step convention unvalidated (P1-T7) |
| `MUL.L Rm,Rn` | `0000 nnnn mmmm 0111` | `sh2.rs:1033` | OK (MACH untouched, per HR) |
| `MULS.W Rm,Rn` | `0010 nnnn mmmm 1111` | `sh2.rs:1301` | OK |
| `MULU.W Rm,Rn` | `0010 nnnn mmmm 1110` | `sh2.rs:1300` | OK |
| `DMULS.L Rm,Rn` | `0011 nnnn mmmm 1101` | `sh2.rs:1372` | OK |
| `DMULU.L Rm,Rn` | `0011 nnnn mmmm 0101` | `sh2.rs:1391` | OK |
| `MAC.L @Rm+,@Rn+` | `0000 nnnn mmmm 1111` | — | **MISSING** |
| `MAC.W @Rm+,@Rn+` | `0100 nnnn mmmm 1111` | — | **MISSING** |
| `CLRMAC` | `0000 xxxx 0010 1000` | `sh2.rs:985` | OK semantics, exact-match only (D-5) |

#### HR §9.5 — logic (14)

| Mnemonic | Encoding | Mimas site | Status |
|---|---|---|---|
| `AND Rm,Rn` | `0010 nnnn mmmm 1001` | `sh2.rs:1287` | OK |
| `AND #imm,R0` | `1100 1001 iiiiiiii` | `sh2.rs:1134` | OK |
| `AND.B #imm,@(R0,GBR)` | `1100 1101 iiiiiiii` | — | **MISSING** |
| `OR Rm,Rn` | `0010 nnnn mmmm 1011` | `sh2.rs:1289` | OK |
| `OR #imm,R0` | `1100 1011 iiiiiiii` | `sh2.rs:1136` | **WRONG — D-1** (0xCB executes XOR) |
| `OR.B #imm,@(R0,GBR)` | `1100 1111 iiiiiiii` | — | **MISSING** |
| `XOR Rm,Rn` | `0010 nnnn mmmm 1010` | `sh2.rs:1288` | OK |
| `XOR #imm,R0` | `1100 1010 iiiiiiii` | `sh2.rs:1135` | **WRONG — D-1** (0xCA executes OR) |
| `XOR.B #imm,@(R0,GBR)` | `1100 1110 iiiiiiii` | — | **MISSING** |
| `NOT Rm,Rn` | `0110 nnnn mmmm 0111` | `sh2.rs:1419` | OK |
| `TST Rm,Rn` | `0010 nnnn mmmm 1000` | `sh2.rs:1286` | OK |
| `TST #imm,R0` | `1100 1000 iiiiiiii` | `sh2.rs:1133` | OK (zero-extended) |
| `TST.B #imm,@(R0,GBR)` | `1100 1100 iiiiiiii` | — | **MISSING** |
| `TAS.B @Rn` | `0100 nnnn 0001 1011` | `sh2.rs:1173` | Semantics OK; **not atomic — D-4** |

#### HR §9.6 — shift and rotate (14): all 14 OK

`SHLL`/`SHLR` (`sh2.rs:1152-1153`), `SHAL`/`SHAR` (`:1196-1207`, `SHAR` correctly
arithmetic), `SHLL2/8/16` (`:1156,1171,1222`), `SHLR2/8/16` (`:1157,1172,1223` — all
logical, **all correctly leave T untouched** per HR §3.1), `ROTL`/`ROTR`
(`:1154-1155`, `rotate_left/right(1)` ≡ HR), `ROTCL`/`ROTCR` (`:1208-1221`).

#### HR §9.7 — branch (12) + SLEEP

| Mnemonic | Encoding | Mimas site | Status |
|---|---|---|---|
| `BF disp` | `1000 1011 dddddddd` | `sh2.rs:1110` | OK (`+4+disp*2`, fixed by `bt_bf_no_delay_slot`) |
| `BF/S disp` | `1000 1111 dddddddd` | `sh2.rs:1125` | OK |
| `BT disp` | `1000 1001 dddddddd` | `sh2.rs:1097` | OK |
| `BT/S disp` | `1000 1101 dddddddd` | `sh2.rs:1117` | OK |
| `BRA disp12` | `1010 dddddddddddd` | `sh2.rs:1065` | OK |
| `BRAF Rm` | `0000 mmmm 0010 0011` | — | **MISSING** |
| `BSR disp12` | `1011 dddddddddddd` | `sh2.rs:1071` | OK (`PR = PC_br + 4`) |
| `BSRF Rm` | `0000 mmmm 0000 0011` | — | **MISSING** |
| `JMP @Rm` | `0100 mmmm 0010 1011` | `sh2.rs:1224` | OK |
| `JSR @Rm` | `0100 mmmm 0000 1011` | `sh2.rs:1158` | OK (`PR = PC_br + 4`) |
| `RTS` | `0000 xxxx 0000 1011` | `sh2.rs:977` | OK semantics, exact-match only (D-5) |
| `RTE` | `0000 xxxx 0010 1011` | `sh2.rs:986` | OK (PC then SR, masked), exact-match only (D-5) |
| `SLEEP` | `0000 xxxx 0001 1011` | — | **MISSING** — silently no-ops, so the CPU runs *past* every idle loop into whatever follows |

#### HR §9.8 — LDC / LDS / STC / STS (24)

| Mnemonic | Encoding | Mimas site | Status |
|---|---|---|---|
| `LDC Rm,SR` | `0100 mmmm 0000 1110` | `sh2.rs:1229` | **WRONG — D-2** (no `& 0x3F3`, no interrupt re-check) |
| `LDC Rm,GBR` | `0100 mmmm 0001 1110` | `sh2.rs:1230` | OK |
| `LDC Rm,VBR` | `0100 mmmm 0010 1110` | `sh2.rs:1231` | OK |
| `LDC.L @Rm+,SR` | `0100 mmmm 0000 0111` | `sh2.rs:1247` | **WRONG — D-3** (no `& 0x3F3`) |
| `LDC.L @Rm+,GBR` | `0100 mmmm 0001 0111` | `sh2.rs:1248` | OK |
| `LDC.L @Rm+,VBR` | `0100 mmmm 0010 0111` | `sh2.rs:1249` | OK |
| `LDS Rm,MACH` | `0100 mmmm 0000 1010` | `sh2.rs:1232` | OK |
| `LDS Rm,MACL` | `0100 mmmm 0001 1010` | `sh2.rs:1233` | OK |
| `LDS Rm,PR` | `0100 mmmm 0010 1010` | `sh2.rs:1234` | OK |
| `LDS.L @Rm+,MACH` | `0100 mmmm 0000 0110` | `sh2.rs:1241` | OK |
| `LDS.L @Rm+,MACL` | `0100 mmmm 0001 0110` | `sh2.rs:1242` | OK |
| `LDS.L @Rm+,PR` | `0100 mmmm 0010 0110` | `sh2.rs:1243` | OK |
| `STC SR,Rn` | `0000 nnnn 0000 0010` | `sh2.rs:1019` | OK (unmasked store, per HR §3.2) |
| `STC GBR,Rn` | `0000 nnnn 0001 0010` | `sh2.rs:1020` | OK |
| `STC VBR,Rn` | `0000 nnnn 0010 0010` | `sh2.rs:1021` | OK |
| `STC.L SR,@-Rn` | `0100 nnnn 0000 0011` | `sh2.rs:1250` | OK |
| `STC.L GBR,@-Rn` | `0100 nnnn 0001 0011` | `sh2.rs:1251` | OK |
| `STC.L VBR,@-Rn` | `0100 nnnn 0010 0011` | `sh2.rs:1252` | OK |
| `STS MACH,Rn` | `0000 nnnn 0000 1010` | `sh2.rs:1022` | OK |
| `STS MACL,Rn` | `0000 nnnn 0001 1010` | `sh2.rs:1023` | OK |
| `STS PR,Rn` | `0000 nnnn 0010 1010` | `sh2.rs:1024` | OK |
| `STS.L MACH,@-Rn` | `0100 nnnn 0000 0010` | `sh2.rs:1244` | OK |
| `STS.L MACL,@-Rn` | `0100 nnnn 0001 0010` | `sh2.rs:1245` | OK |
| `STS.L PR,@-Rn` | `0100 nnnn 0010 0010` | `sh2.rs:1246` | OK |

#### HR §9.9 — system control, other (5 new)

| Mnemonic | Encoding | Mimas site | Status |
|---|---|---|---|
| `NOP` | `0000 xxxx 0000 1001` | `sh2.rs:976` | OK semantics, exact-match only (D-5, harmless for NOP) |
| `CLRT` | `0000 xxxx 0000 1000` | `sh2.rs:983` | OK semantics, exact-match only (D-5) |
| `SETT` | `0000 xxxx 0001 1000` | `sh2.rs:982` | OK semantics, exact-match only (D-5) |
| `SLEEP` | `0000 xxxx 0001 1011` | — | **MISSING** (also listed in §9.7 above) |
| `TRAPA #imm` | `1100 0011 iiiiiiii` | `sh2.rs:1001` | OK (pushes SR then `PC+2`, `VBR + imm*4`, I mask untouched — matches HR) |
| *(undecoded)* | — | `sh2.rs:1445` | **MISSING** — see D-6; no vector-4 exception, no flag, no log |

#### Dispatch-cascade integrity

Verified by hand across all 9 `match` blocks in `execute()` (`sh2.rs:975-1443`): no
earlier block's mask/value pair captures an opcode belonging to a later block. The
sequence is exact → `0xC300`(TRAPA) → group-0 `0xF0FF` → group-0 `0xF00F` → `0xF000` →
`0xFF00`(A=8 branch/imm + A=12) → group-4 `0xF0FF` → groups-2/3/6 `0xF00F` → A=8
`0xFF00` displacement forms. The two `0xFF00` blocks are disjoint (`0x88/89/8B/8D/8F`
and `0xC0-0xCB` vs `0x80/81/84/85`). **This structure is fine; do not rewrite it into a
65536-entry table** — HR §5.3 describes Yabause's table as an implementation choice, not
hardware.

### 0.4 On-chip peripheral coverage (HR §11)

`translate()` (`sh2.rs:339-341`) routes `address >= 0xFFFF_FE00` to `MemRegion::OnChip(addr
& 0x1FF)` — the mask matches HR §1.3. But the region is only *handled* in `read_long`
(`sh2.rs:650`) and `write_long` (`sh2.rs:699`). Byte and word accesses land in
`raw_read_byte_region`'s `MemRegion::Unmapped | MemRegion::OnChip(_) => 0` arm
(`sh2.rs:498`) and `raw_write_byte`'s discard arm (`sh2.rs:593`). **Every 8-bit and
16-bit on-chip register in the machine is therefore unreachable** — which is most of
them (SCI, FRT, WDT, CCR, SBYCR are byte/word only; INTC and BSC are word/long).

| Offset | Address | Register | Width | Reset (HR §11.1) | Mimas status |
|---|---|---|---|---|---|
| `0x000` | `FFFFFE00` | `SMR` | 8 | `0x00` | missing |
| `0x001` | `FFFFFE01` | `BRR` | 8 | `0xFF` | missing |
| `0x002` | `FFFFFE02` | `SCR` | 8 | `0x00` | missing |
| `0x003` | `FFFFFE03` | `TDR` | 8 | `0xFF` | missing |
| `0x004` | `FFFFFE04` | `SSR` | 8 | `0x84` | missing |
| `0x005` | `FFFFFE05` | `RDR` | 8 | `0x00` | missing |
| `0x010` | `FFFFFE10` | `TIER` | 8 | `0x01` | missing |
| `0x011` | `FFFFFE11` | `FTCSR` | 8 | `0x00` | missing |
| `0x012`-`0x013` | `FFFFFE12` | `FRC` | 16 | `0x0000` | missing |
| `0x014`-`0x015` | `FFFFFE14` | `OCRA`/`OCRB` | 16 | `0xFFFF` each | missing |
| `0x016` | `FFFFFE16` | `TCR` | 8 | `0x00` | missing |
| `0x017` | `FFFFFE17` | `TOCR` | 8 | `0xE0` | missing |
| `0x018`-`0x019` | `FFFFFE18` | `FICR` | 16 | `0x0000` | missing |
| `0x060` | `FFFFFE60` | `IPRB` | 16 | `0x0000` | missing |
| `0x062` | `FFFFFE62` | `VCRA` | 16 | `0x0000` | missing |
| `0x064` | `FFFFFE64` | `VCRB` | 16 | `0x0000` | missing |
| `0x066` | `FFFFFE66` | `VCRC` | 16 | `0x0000` | missing |
| `0x068` | `FFFFFE68` | `VCRD` | 16 | `0x0000` | missing |
| `0x071` | `FFFFFE71` | `DRCR0` | 8 | `0x00` | missing |
| `0x072` | `FFFFFE72` | `DRCR1` | 8 | `0x00` | missing |
| `0x080` | `FFFFFE80` | `WTCSR` | 8 | `0x18` | missing |
| `0x081` | `FFFFFE81` | `WTCNT` | 8 | `0x00` | missing |
| `0x083` | `FFFFFE83` | `RSTCSR` | 8 | `0x1F` | missing |
| `0x091` | `FFFFFE91` | `SBYCR` | 8 | `0x60` | missing |
| `0x092` | `FFFFFE92` | `CCR` | 8 | `0x00` | missing |
| `0x0E0` | `FFFFFEE0` | `ICR` | 16 | `0x0000` | missing |
| `0x0E2` | `FFFFFEE2` | `IPRA` | 16 | `0x0000` | missing |
| `0x0E4` | `FFFFFEE4` | `VCRWDT` | 16 | `0x0000` | missing |
| `0x100`/`0x120` | `FFFFFF00` | `DVSR` | 32 | not reset | **present** `sh2.rs:1458,1472` |
| `0x104`/`0x124` | `FFFFFF04` | `DVDNT` | 32 | not reset | **present**, read returns `dvdntl` ✓, write triggers 32÷32 ✓ |
| `0x108`/`0x128` | `FFFFFF08` | `DVCR` | 32 | `0x0000_0000` | **present**, `& 3` ✓ |
| `0x10C`/`0x12C` | `FFFFFF0C` | `VCRDIV` | 32 | `0x0000_0000` | **present**, `& 0xFFFF` ✓ |
| `0x110`/`0x130` | `FFFFFF10` | `DVDNTH` | 32 | not reset | **present** |
| `0x114`/`0x134` | `FFFFFF14` | `DVDNTL` | 32 | not reset | **present**, write triggers 64÷32 ✓ |
| `0x118`/`0x138` | `FFFFFF18` | `DVDNTUH` | 32 | not reset | read-only in Mimas; **write path missing** (D-14) |
| `0x11C`/`0x13C` | `FFFFFF1C` | `DVDNTUL` | 32 | not reset | read-only in Mimas; **write path missing** (D-14) |
| `0x140` | `FFFFFF40` | `BARA` | 32 | `0` | missing |
| `0x144` | `FFFFFF44` | `BAMRA` | 32 | `0` | missing |
| `0x148` | `FFFFFF48` | `BBRA` | 16 | `0` | missing |
| `0x160` | `FFFFFF60` | `BARB` | 32 | `0` | missing (no handler in Yabause either — HR §11.11) |
| `0x164` | `FFFFFF64` | `BAMRB` | 32 | `0` | missing (ditto) |
| `0x168` | `FFFFFF68` | `BBRB` | 16 | `0` | missing (ditto) |
| `0x170` | `FFFFFF70` | `BDRB` | 32 | `0` | missing (ditto) |
| `0x174` | `FFFFFF74` | `BDMRB` | 32 | `0` | missing (ditto) |
| `0x178` | `FFFFFF78` | `BRCR` | 32 | `0` | missing |
| `0x180` | `FFFFFF80` | `SAR0` | 32 | not reset | missing |
| `0x184` | `FFFFFF84` | `DAR0` | 32 | not reset | missing |
| `0x188` | `FFFFFF88` | `TCR0` | 32 | not reset | missing |
| `0x18C` | `FFFFFF8C` | `CHCR0` | 32 | `0` | missing |
| `0x190` | `FFFFFF90` | `SAR1` | 32 | not reset | missing |
| `0x194` | `FFFFFF94` | `DAR1` | 32 | not reset | missing |
| `0x198` | `FFFFFF98` | `TCR1` | 32 | not reset | missing |
| `0x19C` | `FFFFFF9C` | `CHCR1` | 32 | `0` | missing |
| `0x1A0` | `FFFFFFA0` | `VCRDMA0` | 32 | not reset | missing |
| `0x1A8` | `FFFFFFA8` | `VCRDMA1` | 32 | not reset | missing |
| `0x1B0` | `FFFFFFB0` | `DMAOR` | 32 | `0` | missing |
| `0x1E0`/`0x1E2` | `FFFFFFE0` | `BCR1` | 16 | `(bit15) \| 0x03F0` | missing |
| `0x1E4`/`0x1E6` | `FFFFFFE4` | `BCR2` | 16 | `0x00FC` | missing |
| `0x1E8`/`0x1EA` | `FFFFFFE8` | `WCR` | 16 | `0xAAFF` | missing |
| `0x1EC`/`0x1EE` | `FFFFFFEC` | `MCR` | 16 | `0x0000` | missing |
| `0x1F0`/`0x1F2` | `FFFFFFF0` | `RTCSR` | 16 | `0x0000` | missing |
| `0x1F4`/`0x1F6` | `FFFFFFF4` | `RTCNT` | 16 | `0x0000` | missing |
| `0x1F8`/`0x1FA` | `FFFFFFF8` | `RTCOR` | 16 | `0x0000` | missing |

**8 of 63 documented on-chip registers exist**, all DIVU, all long-access-only.

### 0.5 Interrupt handling diff (HR §10)

| HR feature | Mimas |
|---|---|
| `interrupts[50]` queue, dedup by vector, bubble-sort ascending by level, highest at `count-1` (HR §10.1) | **absent** — 4 hardcoded flags: `vblank_pending` (`sh2.rs:117`), `vblank_out_pending` (`:125`), `smpc_irq_pending` (`:138`), `sound_req_irq: Option<Arc<AtomicBool>>` (`:148`) |
| Delivery: push SR, push PC, `SR.I = level` (`0x10 → 0xF`), `PC = [VBR + vector*4]` (HR §10.3) | **matches** (`sh2.rs:945-951`), except the `0x10 → 0xF` NMI clamp (no NMI exists) |
| Strictly-greater-than mask test (HR §10.2) | matches (`sh2.rs:926`) |
| Delivered entry removed at delivery time | matches (flags cleared at `sh2.rs:929-939`) |
| Polled once per `Exec` batch + inside `LDC Rm,SR` (HR §7, §3.3) | polled **once per instruction** at `step()`'s head (`sh2.rs:784`). More frequent than Yabause; see A-3 |
| No interrupt between branch and delay slot (HR §8.2 item 4) | **holds** — `delay_slot_and_jump` (`sh2.rs:957`) calls `execute()` directly, never `step()` |
| Interrupt entry charges 0 cycles (HR §13) | matches (nothing added) |
| NMI: `ICR \|= 0x8000`, vector `0xB`, level `0x10` (HR §10.4) | **absent** |
| On-chip sources: FRT OCIA/OCIB/OVI/ICI, WDT ITI, DIVU OVF, DMAC TE ×2, UBC (HR §10.6) | **all absent** |
| Vector/level from `VCRx`/`IPRx` | **absent** — Mimas hardcodes 4 (level, vector) pairs (`sh2.rs:193-222`), all of which are *SCU-side* interrupts, not SH-2 on-chip |

The 4 implemented sources (VBLANK-IN 0x40/15, VBLANK-OUT 0x41/14, Sound Request 0x46/9,
System Manager 0x47/8) are correct per `docs/hardware-reference/scu.md`'s territory and
should keep working; they just need to migrate onto a real queue (Phase 5).

### 0.6 Cache and address-space diff (HR §6, §12)

`translate()` folds every address with `a = address & 0x0FFF_FFFF` (`sh2.rs:345`). That
correctly aliases the cached (`0x0…`), cache-through (`0x2…`) and `0x8…` mirrors. It
**incorrectly** aliases three regions HR §6 gives distinct behaviour:

| Address `>> 29` | HR §6 behaviour | Mimas today |
|---|---|---|
| 2 (`0x4…`), 5 (`0xA…`) | associative purge area — **reads return `0xFFFFFFFF`** | folded to `0x0…`: `0x40000000` reads BIOS byte 0 |
| 3 (`0x6…`) | cache **address array** (HR §12.4) | folded: `0x60000000` → `MemRegion::HighRam(0)` — collides with real High Work RAM |
| 6 (`0xC…`) | cache **data array**, 4 KiB, `EXEC_FROM_CACHE` runs code from here (HR §5.2, §12.5) | folded: `0xC0000000` → `MemRegion::Bios(0)`; writes silently discarded, reads return BIOS bytes |

No cache model exists at all: no `CCR`, no 4-way×64-entry×16-byte array, no LRU, no
purge, no `cache_enable`/`cache_disable`. HR §12.6's save-state interaction is
irrelevant (Mimas has no save states).

### 0.7 Cycle accounting and exec-loop diff (HR §1.2, §7, §13)

- `step()` charges a flat `+= 2` (`sh2.rs:788`) regardless of opcode. HR §9 gives 1, 2,
  3, 4 or 8 base cycles plus a region-dependent memory penalty.
- No memory wait-state model. HR §1.2's `addr & 0xDFF00000` table (BIOS 16r/0w, Low WRAM
  12r/7w, CS0+CS2 24r/0w, Sound 50r/7w, VDP1 50r/2w, VDP2 `getVramCycle`, High WRAM
  0r/2w) is unimplemented.
- No `Exec(cycles)` batch entry point (HR §7), therefore no `pre_cycle` overshoot carry
  and — more importantly — **nowhere to hang `FRTExec`/`WDTExec`/`DMAProc`**, which HR §7
  calls once per batch.
- `ClockThrottle` (`throttle.rs`) is correct and tested and needs **no change** — it
  consumes whatever cycle count `run_loop` hands it (`sh2.rs:1710`). Its accuracy is
  bounded entirely by the flat `+= 2`: real SH-2 code averages closer to ~1.3-1.5
  cycles/instruction plus wait states, so Mimas currently paces roughly 1.4× too *slow*
  in cycle terms at any given `Multiplier`. `throttle.rs:29-40`'s own doc comment
  already names the flat `Sh2::step()` charge as an accepted simplification of the same
  tier as the M68K's; Phase 8 retires that.
- `run_loop`'s `LockStepSync` batching heuristic (`sh2.rs:1716-1720`) derives
  `batch_mask` from `slack_limit` and compares `self.cycles & !batch_mask` before/after —
  it assumes a small, near-constant per-step cycle delta. Variable per-opcode costs
  (1..8+ plus wait states up to 50) will make some steps skip the sync entirely. Phase 8
  must revisit it.

### 0.8 Reset diff (HR §4)

| HR §4 | Mimas `reset()` (`sh2.rs:317`) |
|---|---|
| `R0..R14 = 0`, **R15 deliberately not zeroed** | **does not zero any GPR** |
| `SR = 0x000000F0` | ✓ `sh2.rs:325` |
| `GBR = VBR = MACH = MACL = PR = 0` | **not reset** |
| `cycles = 0` | **not reset** |
| `frc.leftover=0, frc.shift=3`; `wdt.isenable=0, isinterval=1, shift=1, leftover=0` | n/a (no FRT/WDT) |
| `interrupts[]` zeroed | n/a (no queue); the 4 pending flags are **not** cleared either |
| `OnchipReset()` | Mimas zeroes all 8 DIVU fields (`sh2.rs:328-335`); HR says only `DVCR` and `VCRDIV` are reset, the rest are *not reset* |
| `cache_clear()` | n/a |
| `PC = [VBR+0]`, `R15 = [VBR+4]` (from `SH2PowerOn`) | ✓ `sh2.rs:318-319`, hardcoded to `0x0`/`0x4` rather than `VBR+0`/`VBR+4` (identical while VBR is 0, which it is at reset) |
| `MSH2.BCR1 = 0x0000`, `SSH2.BCR1 = 0x8000` (permanent MASTER bit) | **absent** — `is_slave` exists (`sh2.rs:66`) but drives nothing except `core_id` |

### 0.9 Tracked defects (implemented but wrong)

Same rigour as the hardware-reference "known deviations" sections. **Seed all of these
into `.development/current_bugs.md`.**

- **D-1 — `OR #imm,R0` and `XOR #imm,R0` are swapped.** `sh2.rs:1135` maps `0xCA00` to
  `R0 |= imm`; `sh2.rs:1136` maps `0xCB00` to `R0 ^= imm`. HR §9.5 and
  `yabause/src/sh2int.c:2910-2911` (`case 10: return &SH2xori; case 11: return &SH2ori;`)
  both say `0xCA` = XOR, `0xCB` = OR. **Severity: critical.** `OR #imm,R0` is one of the
  most common instructions in compiled Saturn code (bit-set on a flag byte); every such
  site currently *clears* the bits it means to set whenever they were already set, and
  every `XOR #imm,R0` sets bits it means to toggle. Comments at `sh2.rs:1135-1136`
  currently assert the wrong mapping, so a reader cross-checking the comment against the
  code finds them consistent — exactly the "self-consistent but wrong" failure mode
  `CLAUDE.md` warns about.
- **D-2 — `LDC Rm,SR` writes SR unmasked and skips the interrupt re-check.**
  `sh2.rs:1229` is `self.sr = self.registers[n]`. HR §9.8/§3.2 require
  `SR = Rm & 0x000003F3`, and HR §3.3 notes `SH2ldcsr` is the *only* handler that calls
  `SH2HandleInterrupts` immediately. Consequence of the missing mask: reserved bits 2-3
  and 10-31 stick in SR, so a later `STC SR,Rn` returns a value the BIOS never wrote, and
  `(sr >> 4) & 0xF` stays right only by luck. The missing re-check is *benign in Mimas*
  because `service_pending_interrupt` already runs at the head of every `step()`
  (`sh2.rs:784`) — the interrupt is delivered one instruction later with the same pushed
  PC. Record the reasoning; don't add the call for its own sake.
- **D-3 — `LDC.L @Rm+,SR` writes SR unmasked.** `sh2.rs:1247`. Same missing `& 0x3F3`
  (HR §9.8). `RTE` (`sh2.rs:993`) *does* apply it, so the three sites disagree with each
  other.
- **D-4 — `TAS.B @Rn` is not atomic.** `sh2.rs:1173-1195` performs `read_byte` then
  `write_byte`: two independent `bus_wait()` + `RwLock` acquisitions with a real window
  between them. The existing comment documents this honestly and correctly identifies
  that splitting `WorkRam`'s monolithic lock made the race *more* likely, not less. Real
  dual-SH-2 Saturn code uses `TAS.B` precisely as a Work-RAM spinlock. Currently dormant
  only because Core 1 stays parked until SMPC `SSHON` — which `smpc_execute_command`
  (`sh2.rs:827-832`) already implements, so it is one BIOS command away from live.
- **D-5 — nibble-B is not treated as don't-care for the 8 group-0 no-operand opcodes.**
  `sh2.rs:975-999` matches `NOP`, `CLRT`, `SETT`, `CLRMAC`, `DIV0U`, `RTS`, `RTE` (and,
  once added, `SLEEP`) on the **exact** 16-bit value. `yabause/src/sh2int.c:2669-2699`
  switches on nibble D then nibble C, never inspecting B — so `0x0F0B` is `RTS` on
  hardware and a silent no-op in Mimas. Low probability from a compiler, arbitrarily bad
  when it happens (a missed `RTS` runs off the end of a function).
- **D-6 — no illegal-instruction exception, and unimplemented opcodes are silent.**
  `execute()` falls off the end (`sh2.rs:1445-1453`) with no state change. HR §9.9
  requires: push SR, push `PC + 2`, `PC = [VBR + 16]` (vector 4), `cycles += 1`. Only the
  literal `0xFFFF` sets `illegal_instruction_flag` (`sh2.rs:997`). Beyond correctness,
  this is a *diagnostics* defect: every one of the 9 missing opcodes and every genuinely
  illegal encoding from HR §9.10 currently produces silent wrong execution instead of a
  loud, greppable event.
- **D-7 — delay-slot PC semantics diverge from the reference.**
  `delay_slot_and_jump` (`sh2.rs:957-963`) sets `self.pc = slot_pc + 2` before executing
  the slot, so a PC-relative instruction in the slot sees its own natural address. HR
  §8.1/§8.2 item 1 documents Yabause committing the branch target first and then doing
  `PC -= 2`, so the slot instruction sees `PC = target - 2` and `MOV.L @(disp,PC)` /
  `MOVA` resolve relative to `target + 2`. HR explicitly marks the hardware truth as
  **UNCLEAR**. Mimas's behaviour is the architecturally-natural one and is probably what
  silicon does — but it is *not* what real BIOS/game code was validated against. Needs an
  explicit, documented decision (Phase 1), not a silent divergence.
- **D-8 — a branch inside a delay slot is clobbered by the outer branch.**
  `delay_slot_and_jump` assigns `self.pc = target` *after* `execute()` returns
  (`sh2.rs:962`), so if the slot instruction is itself a branch (HR §8.2 item 2: "not
  blocked, executes normally, recursively"), the inner target is discarded and the outer
  wins. In Yabause the inner branch wins. Pathological code only, but it is a real
  divergence.
- **D-9 — cache-array / purge address regions are folded into ordinary memory.** See
  §0.6. `0xC0000000` (data array, usable as 4 KiB scratch and executable per HR §5.2)
  aliases to BIOS ROM; `0x60000000` (address array) aliases to High Work RAM offset 0;
  `0x40000000`/`0xA0000000` (associative purge, must read `0xFFFFFFFF`) alias to BIOS.
  Any BIOS or game that uses the cache data array as scratch RAM silently reads BIOS
  bytes and loses every write.
- **D-10 — `Bios` region masks with `self.bios.len() - 1`.** `sh2.rs:411`. Correct only
  for a power-of-two image. A 512 KB BIOS gives `& 0x7FFFF`, matching HR §5.1's
  `T2ReadWord(BiosRom, addr & 0x7FFFF)`; any other size silently mis-mirrors. Also
  `self.bios.is_empty()` → `0`, which decodes as an illegal instruction on hardware —
  fine, but only once D-6 lands.
- **D-11 — on-chip registers are long-word-access-only.** See §0.4. Byte and word
  accesses to `0xFFFFFE00`+ read as 0 and write nowhere. This makes the entire SCI, FRT,
  WDT, CCR and SBYCR blocks unreachable *by construction*, independently of whether they
  are implemented.
- **D-12 — DIVU can panic the process.** `sh2.rs:1490` (`dividend / divisor` on `i32`)
  and `sh2.rs:1524` (`dividend / (divisor as i64)`) are unguarded Rust integer divisions.
  `i32::MIN / -1` and `i64::MIN / -1` **panic** in Rust in both debug and release
  ("attempt to divide with overflow") — they are not UB as they are in Yabause's C.
  Per `docs/mimas-architecture-spec.md` §2.3 a panic in any component is meant to take
  the whole process down, so a BIOS or game issuing `0x80000000 ÷ -1` kills the emulator.
  HR §11.6 notes Yabause performs **no** overflow check on the non-zero-divisor path.
- **D-13 — DIVU 64÷32 has no quotient-overflow handling.** `sh2.rs:1524-1529` truncates
  `quotient as u32`. HR §11.6 requires: `quotient > 0x7FFFFFFF` → `DVCR |= 1`,
  `DVDNTL = 0x7FFFFFFF`, `DVDNTH = 0xFFFFFFFE`; `(s32)(quotient >> 32) < -1` → `DVCR |= 1`,
  `DVDNTL = 0x80000000`, `DVDNTH = 0xFFFFFFFE`. (HR flags both `DVDNTH` values as
  Yabause `// fix me` comments and states the true hardware value is not deducible.)
- **D-14 — `DVDNTUH`/`DVDNTUL` have no write path.** `read_onchip` (`sh2.rs:1464-1465`)
  returns them; `write_onchip` (`sh2.rs:1470-1533`) has no `0x118`/`0x138`/`0x11C`/`0x13C`
  arms. HR §11.6 says they are independently writable.
- **D-15 — DIVU never raises its overflow interrupt.** `sh2.rs:1488` and `:1522` set
  `DVCR |= 1` but never check `DVCR & 0x2` (OVFIE) or send an interrupt. HR §10.6/§11.6:
  vector `VCRDIV & 0x7F`, level `(IPRA >> 12) & 0xF`. Blocked on Phase 5 anyway (no
  queue) and Phase 4 (no IPRA).
- **D-16 — `reset()` leaves most of the register file untouched.** See §0.8. In
  particular `R0..R14`, `GBR`, `VBR`, `MACH`, `MACL`, `PR`, `cycles` and all four pending
  interrupt flags survive a reset.
- **D-17 — unaligned access handling is inconsistent and unlike the reference.**
  `read_word`/`write_word` (`sh2.rs:613-636`) set `unaligned_access_flag` and **abort**
  (returning 0 / writing nothing). `read_long`/`write_long` (`sh2.rs:639-707`) set the
  flag and **proceed**. HR §12.2 records that Yabause has *no* alignment enforcement
  anywhere and simply performs the access. An aborted word read is a fabricated 0 the
  BIOS can act on. The existing e2e test `test_tier2_f3_sh2_unaligned_memory_access`
  pins the flag-setting behaviour (not the abort).
- **D-18 — `S` (SR bit 1) is modelled only as a bit inside `SR_WRITE_MASK`.** No
  accessor, never read. Becomes load-bearing the moment `MAC.L`/`MAC.W` land (HR §9.4).
- **D-19 — `is_slave` drives no hardware difference.** HR §Preamble names exactly three
  master/slave differences: the `BCR1` bit-15 MASTER flag, the input-capture write hooks,
  and the DIVU-interrupt-level quirk (level always read from `MSH2->onchip.IPRA`, HR
  §10.6 DEVIATION). None are modelled.
- **D-20 — no `SH2NMI` path.** HR §10.4 (`ICR |= 0x8000`, vector `0xB`, level `0x10`,
  clamped to `SR.I = 0xF` on entry per HR §10.3). SMPC's `MSHNMI`/`SYSRES` commands have
  nowhere to land.

---

## Phase ordering rationale

Ordered by *what unblocks real BIOS boot progress soonest*, not by subsystem tidiness.

| Phase | Content | Why here |
|---|---|---|
| 1 | Fix wrong-today opcodes | D-1 alone can corrupt any flag-manipulating BIOS routine. Zero new surface; pure correctness. |
| 2 | Add the 9 missing opcodes | `SLEEP` and `BRAF`/`BSRF` are boot-path instructions today; the rest close the ISA. |
| 3 | Exceptions + address-space holes | Turns every remaining gap from *silent* into *observable*; makes phases 4-13 debuggable. |
| 4 | On-chip register file (storage + byte/word/long dispatch) | Unblocks *all* remaining on-chip work. BIOS writes BCR1/BCR2/WCR/MCR/CCR/SBYCR very early in reset; today those reads return 0 and any read-back verify fails. |
| 5 | Real interrupt controller (queue + INTC + NMI) | Prerequisite for every on-chip interrupt source (FRT, WDT, DIVU, DMAC) and for SMPC NMI. |
| 6 | DIVU hardening | Stops a live panic (D-12) and completes a peripheral the BIOS provably uses. |
| 7 | FRT | The on-chip timer the BIOS uses for delays and for its own tick; needs 5 and 4. |
| 8 | Per-opcode cycles + wait states | Needed for FRT/WDT/DMAC to advance at plausible rates and for `ClockThrottle` honesty. |
| 9 | On-chip DMAC | Game compatibility, not boot. |
| 10 | WDT | Rarely load-bearing; interval mode only, per HR. |
| 11 | Cache model + CCR + cache arrays | Correctness-relevant only for code that uses the arrays as scratch; a genuine perf lever for the `WorkRam` contention story. |
| 12 | SCI, SBYCR, BSC | Storage-only fidelity; HR says none of them affect behaviour. |
| 13 | UBC | Debugger facility, compiled out even in the reference. |
| 14 | Dual-core specifics | Gated on Core 1 actually running real code. |

---

## Phase 1 — Fix opcodes that are wrong today

**Unblocks:** anything. Highest expected value per line changed. No new API, no new
fields, no `Sh2::new()` change.

- [x] **P1-1 (D-1).** Swap `sh2.rs:1135` and `sh2.rs:1136` so `0xCA00` is
      `R0 ^= imm8` (`XOR #imm,R0`) and `0xCB00` is `R0 |= imm8` (`OR #imm,R0`). Fix the
      trailing comments too — they currently assert the wrong mapping. Reference:
      HR §9.5, `yabause/src/sh2int.c:2910-2911`.
- [x] **P1-2 (D-2).** `sh2.rs:1229`: `self.sr = self.registers[n] & SR_WRITE_MASK;`.
      Add a comment recording *why* the immediate `SH2HandleInterrupts` call
      (HR §3.3) is not replicated: `service_pending_interrupt` already runs at the head
      of every `step()` (`sh2.rs:784`), so the pending interrupt is taken one instruction
      later with an identical pushed PC. Reference: HR §9.8, §3.2, §3.3.
- [x] **P1-3 (D-3).** `sh2.rs:1247`: apply `& SR_WRITE_MASK` to the loaded value.
      Reference: HR §9.8.
- [x] **P1-4 (D-4).** Make `TAS.B` a single atomic bus transaction. Two viable shapes,
      both compatible with `WorkRam`'s per-region locks:
      (a) add `WorkRam::tas_byte(region_selector, off) -> u8` methods that take the
      *write* lock once and do read-modify-write under it, mirroring the existing
      `read_high_ram_byte`/`write_high_ram_byte` pair (`shared_buffers.rs:99-111`);
      (b) hold `BusArbiter` for the whole read-modify-write. (a) is preferred — it is the
      narrower lock and it matches the striping design. Note `TAS.B` must work on Low
      WRAM *and* High WRAM (both are legitimate spinlock homes), so the helper needs the
      same `MemRegion` dispatch `raw_read_byte_region` uses. HR §9.5 notes the reference
      does *not* lock the bus, so this is a deliberate improvement on the reference, not
      a port — say so in the comment.
- [x] **P1-5 (D-5).** Restructure `sh2.rs:975-999`'s exact-match block into a
      `match opcode & 0xF0FF` arm set so nibble B is ignored for `NOP` (`0x0009`),
      `CLRT` (`0x0008`), `SETT` (`0x0018`), `CLRMAC` (`0x0028`), `DIV0U` (`0x0019`),
      `RTS` (`0x000B`), `RTE` (`0x002B`) and (from Phase 2) `SLEEP` (`0x001B`). These
      can merge into the existing group-0 `0xF0FF` block at `sh2.rs:1018-1027` (which
      already holds `STC`/`STS`/`MOVT`) — verify no arm collision first. Keep the
      literal `0xFFFF` arm where it is until P3-1 replaces it. Reference: HR §5.3,
      `yabause/src/sh2int.c:2669-2699`.
- [x] **P1-6 (D-7).** Decide and document the delay-slot PC-relative semantics. Two
      options: (i) keep Mimas's natural behaviour and add a `DEVIATION` comment on
      `delay_slot_and_jump` (`sh2.rs:957`) citing HR §8.2 item 1 and stating that HR
      marks the hardware truth UNCLEAR; (ii) reproduce Yabause bit-for-bit by setting
      `self.pc = target - 2` before `execute()` and letting the slot instruction's own
      `+= 2` land on `target`, then dropping the trailing `self.pc = target`.
      **Recommendation: (ii)** — `CLAUDE.md`'s methodology is explicitly "the exact
      behaviour real BIOS/game code was tested against", and (ii) additionally fixes D-8
      for free (an inner branch's own PC assignment then survives). Whichever is chosen,
      the reasoning goes in `history.md`.
- [x] **P1-7 (D-8).** Falls out of P1-6(ii). If P1-6(i) is chosen instead, guard the
      trailing `self.pc = target` so it does not clobber a PC the slot instruction set.
- [x] **P1-8 (D-16).** Extend `reset()` (`sh2.rs:317`) to zero `R0..=R14` (**not R15** —
      HR §4's loop bound is `i < 15`, and R15 is then loaded from the vector), `gbr`,
      `vbr`, `mach`, `macl`, `pr`, `cycles`, and all four pending interrupt flags. Change
      the DIVU reset to match HR §11.1: reset **only** `dvcr` and `vcrdiv`; leave `dvsr`,
      `dvdnt`, `dvdnth`, `dvdntl`, `dvdntuh`, `dvdntul` untouched. Read the vectors from
      `self.vbr + 0` / `self.vbr + 4` rather than the hardcoded `0x0`/`0x4` (identical at
      reset, correct if anyone ever calls `reset()` with a non-zero VBR).
- [x] **P1-9 (D-17).** Make word and long alignment handling consistent. Recommended:
      keep setting `unaligned_access_flag` (an e2e test depends on it) but **perform the
      access anyway** in `read_word`/`write_word`, matching `read_long`/`write_long` and
      HR §12.2's "no alignment enforcement exists anywhere". Returning a fabricated 0 is
      strictly worse than returning the bytes that are actually there.
- [x] **P1-10 (D-10).** Replace `self.bios[off & (self.bios.len() - 1)]`
      (`sh2.rs:411`) with an explicit `off & 0x7FFFF` bounds-checked against
      `self.bios.len()`, matching HR §5.1's `T2ReadWord(BiosRom, addr & 0x7FFFF)`.

### Phase 1 testing

Every assertion below must come from a value derived *outside* `sh2.rs`.

- [ ] **P1-T1 (D-1).** Two tests, one per direction, with hand-computed constants:
      `R0 = 0b1010_1010` then `OR #0b0101_0101,R0` (`0xCB55`) must give `0xFF`;
      `R0 = 0b1010_1010` then `XOR #0b1111_1111,R0` (`0xCAFF`) must give `0x55`. The
      point of choosing operands where `|` and `^` *differ* is that the current
      implementation must fail both. Cross-check the encodings against
      `yabause/src/sh2int.c:2910-2911` in the test comment, and against
      `python3 tools/sh2dis.py` on a 4-byte scratch file containing `CB 55 CA FF`.
- [ ] **P1-T2 (D-1, stronger).** Find a real `OR #imm,R0` in the actual BIOS: dump the
      first 64 KB of the loaded image, disassemble with `tools/sh2dis.py`, grep the
      output for `OR #`, and pin one real (address, opcode, operand) triple in a test.
      This is the same technique that produced `bf_s_matches_real_bios_wait_loop`
      (`sh2.rs:1875`) and is what turns a plausible test into a derived one.
- [ ] **P1-T3 (D-2/D-3).** `LDC R0,SR` with `R0 = 0xFFFF_FFFF` must leave
      `sr == 0x0000_03F3`; the same for `LDC.L @R15+,SR` reading `0xFFFF_FFFF` from the
      stack; and `RTE` popping `0xFFFF_FFFF` (already correct — include it so the three
      sites are pinned together and can never drift apart again). Constant derived from
      HR §3.2 / `yabause/src/sh2int.c:1090,1113,2081`, not from running the code.
- [ ] **P1-T4 (D-4).** Spawn two threads, each executing `TAS.B @Rn` against the same
      Low WRAM byte 100k times with the byte reset to 0 between rounds by the harness;
      assert that across all iterations exactly one thread ever observes `T == 1` per
      round. A non-atomic implementation fails this probabilistically — so give it enough
      iterations that failure is near-certain, and mark the test `#[ignore]`-free but
      time-bounded. Complement it with a deterministic single-threaded test asserting the
      HR §9.5 semantics: `T` is set from the value read *before* the `|= 0x80`.
- [ ] **P1-T5 (D-5).** `execute(0x0F0B)` must behave exactly as `execute(0x000B)` (RTS);
      likewise `0x0F09`/`0x0009`, `0x0F2B`/`0x002B`. Encoding rationale cited to
      `yabause/src/sh2int.c:2693-2699`.
- [ ] **P1-T6 (D-7/D-8).** Whichever branch of P1-6 is taken, pin it: place
      `MOV.L @(disp,PC),R0` in the delay slot of a `BRA` whose target is *far* from the
      branch, and assert the exact literal loaded. Compute the expected address by hand
      in the test comment under both interpretations, and state which one the project
      chose and why. Also pin D-8 with a `BRA` whose delay slot is a `JMP @Rn`.
- [ ] **P1-T7 (pre-existing debt).** Resolve the `DIV1` multi-step question flagged at
      `sh2.rs:1305-1318` and `sh2.rs:2231-2238`. Derive expected values by compiling (or
      hand-assembling) the canonical SH-2 unsigned-divide sequence — `DIV0U`, 32×
      `(ROTCL Rn; DIV1 Rm,Rn)`, final `ROTCL` — and computing the expected quotient with
      an independent script, then pin `1000 / 7` and at least one signed case seeded by
      `DIV0S`. The current single-step test is correct but explicitly disclaims the
      multi-step convention; leaving that open means no caller can trust hardware
      division. Reference: HR §9.4's `DIV1` block.
- [ ] **P1-T8 (D-16).** After `reset()` on a CPU with every register pre-poisoned to
      `0xDEAD_BEEF`: `R0..R14 == 0`, `R15 == [vbr+4]`, `pc == [vbr+0]`, `sr == 0xF0`,
      `gbr/vbr/mach/macl/pr == 0`, `dvcr == 0`, `vcrdiv == 0`, and `dvsr` **still**
      `0xDEAD_BEEF` (the HR §11.1 "not reset" columns). The last assertion is the one
      that proves the test was written against HR and not against the implementation.

---

## Phase 2 — Add the 9 missing opcodes

**Unblocks:** `SLEEP` immediately (BIOS idle loops currently fall through and execute
whatever follows). `BRAF`/`BSRF` are the standard PC-relative long-branch and
position-independent-call forms and appear in real BIOS dispatch tables. The GBR byte
forms are the standard idiom for touching a flag byte through a global base pointer.

- [x] **P2-1 `SLEEP`** — `0000 xxxx 0001 1011`. HR §9.7: `cycles += 3`, **PC not
      advanced**, `isSleeping` not set. Since `step()` advances PC *before* `execute()`
      (`sh2.rs:786`), the handler must do `self.pc = self.pc.wrapping_sub(2)`.
      **Architectural decision required** — see A-2: the literal port is a 3-cycle busy
      spin, which directly contradicts `docs/mimas-architecture-spec.md` §1.5 ("If a CPU
      core executes a `SLEEP` opcode … it yields execution"). Recommended: implement the
      literal semantics in `execute()` (so single-stepping and unit tests match HR
      exactly) and handle the *parking* in `run_loop`, which already owns
      `LockStepSync`. See Phase 2's architecture note below.
- [x] **P2-2 `BRAF Rm`** — `0000 mmmm 0010 0011`, mask `0xF0FF` value `0x0023`. HR §9.7:
      `PC = PC_br + Rm + 4`, delay slot, 2 cycles. In Mimas terms with `self.pc` already
      at `PC_br + 2`: `target = self.pc.wrapping_add(2).wrapping_add(self.registers[m])`,
      then `delay_slot_and_jump(target)`. Note the register is nibble **B** here, which
      `execute()` binds as `n` (`sh2.rs:967`), not `m` — read carefully.
- [x] **P2-3 `BSRF Rm`** — `0000 mmmm 0000 0011`, mask `0xF0FF` value `0x0003`. HR §9.7:
      `PR = PC_br + 4` (i.e. `self.pc + 2`), `PC = PC_br + Rm + 4`, delay slot, 2 cycles.
      Same nibble-B caveat.
- [x] **P2-4 `MAC.L @Rm+,@Rn+`** — `0000 nnnn mmmm 1111`, mask `0xF00F` value `0x000F`.
      HR §9.4 exact order: read `[Rn]` **first** as `s32`, `Rn += 4`; then `[Rm]` as
      `s32`, `Rm += 4`; `a = MACL | (MACH << 32)`; `b = m0 * m1` as `s64`; `sum = a + b`;
      if `S == 1` and `sum > 0x00007FFFFFFFFFFF` and `sum < 0xFFFF800000000000`, saturate
      to `0xFFFF800000000000` when `b < 0` else `0x00007FFFFFFFFFFF`; `MACL = sum as u32`,
      `MACH = (sum >> 32) as u32`. **No `if n != m` guard** — `MAC.L @R4+,@R4+` increments
      R4 twice. Cycles `3 + rcycle1 + rcycle2`.
- [x] **P2-5 `MAC.W @Rm+,@Rn+`** — `0100 nnnn mmmm 1111`, mask `0xF00F` value `0x400F`.
      HR §9.4: read `[Rm]` **first** as `s16` (opposite order from `MAC.L`), `Rm += 2`;
      then `[Rn]` as `s16`, `Rn += 2`; `b = m0 * m1` as `s32`;
      `sum = (MACL as i32 as i64) + b` — **MACH is not part of the accumulator**. If
      `S == 1`: on `sum > 0x7FFFFFFF && sum < 0xFFFFFFFF80000000` set `MACH |= 1` and
      saturate `sum` to `0x80000000` (when `b < 0`) or `0x7FFFFFFF`; then `MACL = sum`,
      MACH otherwise untouched. If `S == 0`: `MACL = sum`, `MACH = sum >> 32` — HR §13
      item 1 flags the non-accumulating `MACH` overwrite as a **DEVIATION** whose
      hardware truth is not deducible. Port it as-is and copy that DEVIATION note into
      the Rust comment verbatim.
- [x] **P2-6 `S` bit plumbing (D-18).** Add `const SR_S: u32 = 1 << 1;` beside
      `SR_T`/`SR_M`/`SR_Q` (`sh2.rs:179-181`) plus an `s()` accessor beside `t()`/`q()`/
      `m()` (`sh2.rs:746-780`). HR §3.1: **no instruction sets or clears S** — there is no
      `SETS`/`CLRS` on SH-2; the only writers are `LDC …,SR`, `LDC.L @Rm+,SR` and `RTE`,
      all of which already go through `SR_WRITE_MASK` after Phase 1. Add a comment saying
      exactly that, so nobody later "adds the missing SETS opcode".
- [x] **P2-7 `TST.B #imm,@(R0,GBR)`** — `1100 1100 iiiiiiii`. HR §9.5:
      `T = ([GBR+R0] & imm) == 0`. Cycles `3 + rcycle`.
- [x] **P2-8 `AND.B #imm,@(R0,GBR)`** — `1100 1101 iiiiiiii`. `[GBR+R0] &= imm`. Cycles
      `3 + rcycle + wcycle`.
- [x] **P2-9 `XOR.B #imm,@(R0,GBR)`** — `1100 1110 iiiiiiii`. `[GBR+R0] ^= imm`. Cycles
      `3 + rcycle + wcycle`.
- [x] **P2-10 `OR.B #imm,@(R0,GBR)`** — `1100 1111 iiiiiiii`. `[GBR+R0] |= imm`. Cycles
      **`3`** — HR §9.5 and §13 record that Yabause computes `rcycle`/`wcycle` and
      discards them, unlike `AND.B`/`XOR.B`. Decide in Phase 8 whether to match the
      irregularity; for now note it.
      All four go in the `match opcode & 0xFF00` block at `sh2.rs:1091-1149`, beside the
      existing `0xC800`-`0xCB00` arms. The comment at `sh2.rs:1445-1453` naming these
      four as the known gap can then be deleted.
- [x] **P2-11.** Delete the now-stale "known real coverage gap" comment
      (`sh2.rs:1445-1453`) and replace it with the D-6 exception (Phase 3).

### Phase 2 architecture note (A-2): `SLEEP` and the no-polling rule

`docs/mimas-architecture-spec.md` §1.5 forbids busy loops and requires a core with no
work to park on a `Condvar`. HR §9.7 says the reference `SLEEP` is a 3-cycle spin that
re-executes itself until an interrupt overwrites PC. Reconciliation:

- `execute()` implements HR's literal semantics (rewind PC, charge 3 cycles). Unit tests
  and any future single-step debugger then match the reference exactly.
- `run_loop` (`sh2.rs:1674`) gains a `sleeping: bool` check after `step()`. When set and
  no interrupt is pending, it calls `sync.set_thread_active(self.core_id, false)` and
  blocks in `LockStepSync::park_while_inactive` — the *exact* mechanism Core 1 and Core 6
  already use (`lib.rs:165`, `:322`). The wake path is `set_thread_active(core, true)`
  from whoever raises the interrupt.
- **Do not** merge this into the drift `Condvar`. `sync.rs`'s own doc comment records
  that merging the drift-waiter and park-waiter condvars was tried and measured as a real
  bug (a parked core woken millions of times a second). `park_while_inactive`'s separate
  condvar is the correct one.
- **Caveat to resolve before shipping:** today the only thing that can wake Core 0 is its
  own wall-clock VBLANK scheduler inside `run_loop` (`sh2.rs:1688-1706`). If Core 0 parks,
  that scheduler stops running and nothing raises VBLANK — a self-deadlock. So the park
  must either be time-bounded (park until the next scheduled VBLANK instant) or wait
  until VBLANK generation moves out of `Sh2::run_loop` and onto the VDP2 thread, where
  `docs/implementation-plans/vdp2.md` says it belongs. **Recommended: implement the
  `execute()` semantics in Phase 2, and gate the parking on the VBLANK-source move.**
  Until then a `SLEEP` that spins correctly is still strictly better than one that
  silently falls through.

### Phase 2 testing

- [ ] **P2-T1 `SLEEP`.** `pc` unchanged across `step()`, `cycles` advanced by the
      `SLEEP` cost, and — the load-bearing assertion — a pending unmasked interrupt
      taken on the *next* `step()` moves PC to the vector. Build it from the existing
      `vblank_interrupt_enters_and_returns` (`sh2.rs:2088`) shape.
- [ ] **P2-T2 `BRAF`/`BSRF`.** Hand-compute the target: place the branch at
      `0x0600_1000` with `Rm = 0x40`; target must be `0x0600_1044`
      (`PC_br + Rm + 4`), and `BSRF` must leave `PR == 0x0600_1004`. Verify the delay
      slot ran (put a `MOV #imm` in it). Derive the two constants in the test comment
      arithmetically from HR §9.7's formula, and cross-check the encodings against
      `yabause/src/sh2int.c:2656-2660`.
- [ ] **P2-T3 `MAC.L`.** Three cases, all with operands chosen so a wrong read order or
      a wrong accumulator width gives a different answer:
      (a) `S = 0`, `MACH:MACL = 0`, two small positive longs — assert the exact 64-bit
      product split across MACH/MACL, computed by hand in the comment;
      (b) `S = 0`, a non-zero starting `MACH:MACL`, proving the full 64-bit accumulate;
      (c) `S = 1` with operands whose product pushes `sum` past `0x00007FFFFFFFFFFF` —
      assert the exact saturation constant. Derive (c)'s trigger operands with a throwaway
      Python script, not by running the emulator. Also assert `MAC.L @R4+,@R4+` leaves
      `R4` incremented by **8** (HR §9.4: no `n != m` guard).
- [ ] **P2-T4 `MAC.W`.** Mirror of P2-T3 plus the two behaviours that distinguish it:
      with `S = 0`, a pre-set `MACH` must be **overwritten** by the sign extension of the
      33-bit sum, not added (HR §13 item 1); with `S = 1` and a saturating sum,
      `MACH` must come back with bit 0 set (HR §13 item 2). Both assertions must cite HR
      §9.4's code block, and the test name should carry `yabause_deviation` so nobody
      "fixes" it later.
- [ ] **P2-T5 GBR byte-logic forms.** For each of `TST.B`/`AND.B`/`XOR.B`/`OR.B`: set
      `GBR = 0x0600_0000`, `R0 = 0x10`, write a known byte at `0x0600_0010`, execute, and
      assert the resulting memory byte (and `T` for `TST.B`). Choose an immediate and a
      memory byte where `&`, `|` and `^` all give three different results, so a
      copy-paste error between the four arms cannot pass. Encoding cross-check:
      `yabause/src/sh2int.c:2912-2915`.
- [ ] **P2-T6 `S` bit.** `LDC R0,SR` with bit 1 set must leave `s() == true`; no other
      opcode may change it. A cheap exhaustive guard: loop over all 65536 opcodes,
      execute each on a fresh CPU with `S = 1` and again with `S = 0`, and assert `S` is
      unchanged for every opcode except `0x4n0E`, `0x4n07` and `0x002B`. That is a real,
      independently-derived invariant straight from HR §3.1 and it costs nothing to run.

---

## Phase 3 — Exceptions and the address-space holes

**Unblocks:** observability. After this phase, a BIOS path that reaches an unimplemented
opcode or an unmodelled address stops being silent.

- [x] **P3-1 (D-6) Illegal-instruction exception.** Replace the fall-through at
      `sh2.rs:1445-1453` with HR §9.9's sequence: `R15 -= 4; [R15] = SR`;
      `R15 -= 4; [R15] = PC` (which is already `instr + 2` in Mimas's convention —
      matching HR's `PC + 2`); `PC = [VBR + (4 << 2)]`; `cycles += 1`; set
      `illegal_instruction_flag`. HR §9.9 records Yabause's own `// Fix me`: it always
      uses vector 4 and never vector 6 for the delay-slot case. Match Yabause (vector 4
      unconditionally) and copy the UNCLEAR note into the comment. **Do not** replicate
      Yabause's BIOS HLE hooks (`BiosBUPInit`, `BiosHandleFunc`) — HR §9.9 explicitly
      labels them "emulator HLE, not hardware".
- [x] **P3-2.** Keep the existing `0xFFFF` arm's observable behaviour (an e2e test
      depends on `illegal_instruction_flag`) but route it through P3-1's common path so
      `0xFFFF` and every other illegal encoding behave identically.
- [x] **P3-3.** Add a `log_illegal_once`-style dedup log (same shape as
      `log_reg_access_once`, `sh2.rs:242`) printing `[ILLOP] pc=… opcode=…` exactly once
      per distinct `(pc, opcode)`. This is the diagnostic that makes the remaining ISA
      and peripheral gaps greppable from a boot run, the same way `[REGACCESS]` already
      is. Keep it permanently, like `REG_ACCESS_LOG`.
- [x] **P3-4 (D-9) Associative purge region.** In `translate()` (`sh2.rs:338`), before
      the `& 0x0FFF_FFFF` fold, dispatch on `address >> 29`: value **2** (`0x4…`)
      becomes a new `MemRegion::CachePurge(usize)`. Reads return
      `0xFF` per byte (so `read_long` yields `0xFFFFFFFF`, HR §6). Writes: only a
      **longword** write performs the associative purge (HR §12.3.1); byte and word
      writes fall through to an ordinary uncached write. Until Phase 11 exists, a
      longword write is a no-op with a comment. (Note: area 5 `0xA...` behaves as
      cache-through normal memory per Yabause, not cache purge).
- [x] **P3-5 (D-9) Cache address array region.** `address >> 29 == 3` (`0x6…`) becomes
      `MemRegion::CacheAddressArray(usize)`. Until Phase 11, back it with a flat
      `[u32; 0x100]` field on `Sh2` indexed `(addr & 0x3FC) >> 2` — this is exactly what
      Yabause does when `CACHE_ENABLE` is undefined (HR §12.4's closing paragraph), so
      it is a faithful port of the shipped configuration, not a stub.
- [x] **P3-6 (D-9) Cache data array region.** `address >> 29 == 6` (`0xC…`) becomes
      `MemRegion::CacheDataArray(usize)`. Back it with a flat `[u8; 0x1000]` field
      indexed `addr & 0xFFF` — again exactly Yabause's non-cache build (HR §12.5). This
      makes the region usable as the 4 KiB scratch RAM real code treats it as, and
      unblocks HR §5.2's `EXEC_FROM_CACHE`: `step()`'s fetch must additionally test
      `(pc & 0xC0000000) == 0xC0000000` and read the opcode from the data array. Note
      HR §5.2: that test also matches `0xE…` (the on-chip region), and
      `sh2int.c:55` defines `EXEC_FROM_CACHE` **unconditionally**, so this path is always
      live in the reference.
- [x] **P3-7.** Both new arrays are per-CPU state. Add them as `pub` fields with
      `Default`-style initialisation inside the existing `Sh2::new()` body — **do not
      change `Sh2::new()`'s 3-argument signature** (`CLAUDE.md` stability constraint;
      many tests in `e2e-tests` and `saturn-core` depend on it).
- [x] **P3-8 (part of D-11 prep).** While in `translate()`, confirm the on-chip test
      `address >= 0xFFFF_FE00` (`sh2.rs:339`) stays *before* the new `>> 29` dispatch —
      `0xFFFFFE00 >> 29 == 7`, which HR §6 also routes to on-chip, so both orders agree,
      but the explicit check must win to preserve the `& 0x1FF` offset.

### Phase 3 testing

- [x] **P3-T1.** Execute an opcode from HR §9.10's hole list — pick one per group so the
      whole list is covered: A=0 D=0 (`0x0000`); A=0 D=2 C=3 (`0x0032`); A=2 D=3
      (`0x2003`); A=3 D=1 (`0x3001`); A=3 D=9 (`0x3009`); A=4 D=12 (`0x400C`); A=4 D=13
      (`0x400D`); A=4 D=4 C=1 (`0x4014`); A=8 B=2 (`0x8200`); A=15 (`0xF000`). For each,
      assert `PC == [VBR + 16]`, that `SR` and `PC + 2` are on the stack in that order,
      and that `R15` moved by exactly 8. The hole list is derived from HR §9.10, which is
      derived from `decode()` — so the *inputs* are independently sourced even though the
      expected *behaviour* is one formula.
- [x] **P3-T2.** `read_long(0x4000_0000)` must return `0xFFFF_FFFF` (HR §6) while `0xA000_0000` behaves as cache-through (normal memory). Assert too that the same physical offset read through
      `0x0000_0000` still returns real BIOS bytes — proving the fold was removed for the
      purge region only.
- [x] **P3-T3.** Write a long to `0xC000_0010`, read it back from `0xC000_0010`, and
      **separately** assert that `read_long(0x0000_0010)` is unchanged — the bug being
      fixed is precisely the aliasing of the two.
- [x] **P3-T4.** Same for `0x6000_0000` vs High Work RAM offset 0: write `0xAAAA_AAAA` to
      the address array, assert `read_long(0x0600_0000)` is still 0.
- [x] **P3-T5.** Set `PC = 0xC000_0000`, place `MOV #0x2A,R0` (`0xE02A`) in the data
      array via `write_word(0xC000_0000, 0xE02A)`, `step()`, assert `R0 == 0x2A` — the
      `EXEC_FROM_CACHE` path of HR §5.2.
- [x] **Existing-test breakage to fix in the same commit:** `e2e-tests`'s
      `test_tier1_f3_sh2_pc_increment` steps opcode `0x0000` from `0x0600_0000` and
      asserts `pc == 0x0600_0002`; `test_tier2_f3_sh2_max_pc_overflow` steps `0x0000`
      from `0xFFFF_FFFF` and asserts `pc == 1`. Both currently pass *because* illegal
      opcodes are silent no-ops. P3-1 breaks both. Update them to use `NOP` (`0x0009`)
      — which is what they clearly meant, given `test_tier1_f3_sh2_nop_execution`
      already does exactly that — and add the illegal-opcode assertions as new tests.
      `cargo test --workspace` must be - [x] **P4-1.** Introduce an `OnChip` struct in `sh2.rs` (or a new
      `saturn-core/src/sh2_onchip.rs` module re-exported from `sh2.rs`) holding every
      register in §0.4's table with its HR §11.1 reset value. Port the *layout and reset
      values*, not Yabause's `Onchip_struct` C shape (`CLAUDE.md`: "Port what the
      hardware does, never transliterate Yabause's C data structures").
- [x] **P4-2.** Move the 8 existing DIVU fields (`sh2.rs:165-172`) into it. They are
      `pub` today and read by `test_onchip_division` only through `read_long`/`write_long`,
      so no test depends on the field names — verify with a grep before moving.
- [x] **P4-3 (D-11).** Route `MemRegion::OnChip` through byte, word **and** long paths:
      - `raw_read_byte_region` (`sh2.rs:405`): replace the
        `MemRegion::Unmapped | MemRegion::OnChip(_) => 0` arm (`sh2.rs:498`) with a call
        to `read_onchip_byte(off)`.
      - `raw_write_byte` (`sh2.rs:503`): split `OnChip` out of the discard arm
        (`sh2.rs:593`) into `write_onchip_byte(off, val)`.
      - `read_word`/`write_word` (`sh2.rs:613`, `:625`): add an `OnChip` interception
        mirroring the existing `read_long`/`write_long` ones (`sh2.rs:650`, `:699`), so a
        16-bit register write is *one* transaction rather than two byte writes — this
        matters because several registers have word-only write semantics (WDT's magic
        protocol, INTC's `IPRB = val & 0xFF00`).
      - Keep `read_onchip`/`write_onchip` (`sh2.rs:1456`, `:1470`) as the long path.
      HR §11.1 is explicit that some registers exist at both a long offset and a word
      offset (the BSC block at `0x1E0` long / `0x1E2` word), and that DIVU is mirrored at
      `0x100`- and `0x120`-blocks. Preserve both.
- [x] **P4-4 INTC storage + write masking** (HR §11.4). Exact per-register behaviour:
      - `IPRA` `0x0E2`: byte@`0x0E2` → `IPRA = (val << 8) | (IPRA & 0x00FF)`;
        byte@`0x0E3` → `IPRA = (IPRA & 0xFF00) | (val & 0xF0)`; word → `IPRA = val & 0xFFF0`.
      - `IPRB` `0x060`: byte@`0x060` → `IPRB = val << 8` (**low byte destroyed** — HR calls
        this out explicitly); byte@`0x061` → ignored; word and long → `IPRB = val & 0xFF00`.
      - `VCRA` `0x062`, `VCRB` `0x064`, `VCRC` `0x066`: byte → merge `val & 0x7F` into the
        selected half; word → `val & 0x7F7F`.
      - `VCRD` `0x068`: byte@`0x068` → `VCRD = (val & 0x7F) << 8` (**low byte cleared**);
        byte@`0x069` → ignored; word → `val & 0x7F7F`.
      - `VCRWDT` `0x0E4`: byte → merge `val & 0x7F`; word at `0x0E4` **or** `0x0E5` →
        `val & 0x7F7F`.
      - `ICR` `0x0E0`: byte@`0x0E0` → `ICR = ((val & 1) << 8) | (ICR & 0xFEFF)`;
        byte@`0x0E1` → `ICR = (ICR & 0xFFFE) | (val & 1)`; word → `ICR = val & 0x0101`.
        Bit 15 is set only by NMI (Phase 5).
- [x] **P4-5 CCR storage** (HR §11.7). Offset `0x092`, readable as byte and as word,
      written identically by both paths: `CCR = val & 0xCF`; `if (val & 0x10) purge`;
      `if (CCR & 1) enable else disable`. Until Phase 11 the purge/enable/disable calls
      are no-ops with a comment; the **stored value and its `0xCF` mask must be right
      now**, because bit 5 always reading back 0 is exactly the kind of thing a BIOS
      read-back verify checks.
- [x] **P4-6 SBYCR** (HR §11.10). Offset `0x091`, write `SBYCR = val & 0xDF`, reset
      `0x60`. HR: **there is no read path** in the reference — decide explicitly whether
      to match (returns whatever the "unhandled" default is) or to add one. Recommended:
      add a read path returning the stored value, and comment that the reference lacks
      one; a register the BIOS can write but not read is far more likely to be a Yabause
      omission than hardware behaviour, and HR does not claim otherwise.
- [x] **P4-7 BSC storage** (HR §11.9). All seven registers, long offsets `0x1E0`,
      `0x1E4`, `0x1E8`, `0x1EC`, `0x1F0`, `0x1F4`, `0x1F8`; word **read** aliases at
      base+2 (`0x1E2`, `0x1E6`, `0x1EA`, `0x1EE`, `0x1F2`, `0x1F6`, `0x1FA`); writes exist
      only on the long path. Masks: `BCR1 = (BCR1 & 0x8000) | (val & 0x1FF7)` (bit 15
      preserved, bit 3 not writable); `BCR2 = val & 0xFC`; `WCR = val` raw;
      `MCR = val & 0xFEFC`; `RTCSR = val & 0xF8`; `RTCNT` **no write path**;
      `RTCOR = val & 0xFF`. HR §11.9 is explicit that these are pure storage — no wait
      states, no refresh counting — so implement storage and say so.
- [x] **P4-8 BCR1 MASTER bit (D-19, part 1).** Set `BCR1 = 0x0000` for the master and
      `0x8000` for the slave at construction, and have the on-chip reset preserve bit 15
      while forcing `| 0x03F0` (HR §4, §11.1). This is the first thing `is_slave`
      (`sh2.rs:66`) actually drives.
- [x] **P4-9 SCI storage** (HR §11.2). Six byte registers `0x000`-`0x005` with reset
      values `0x00`/`0xFF`/`0x00`/`0xFF`/`0x84`/`0x00`. Two behaviours beyond storage:
      writing `SCR` with bit 5 clear sets `SSR |= 0x80` (TDRE); writing `SSR` with bit 7
      clear while `SCR & 0x20` calls the (no-op) transmit. `RDR` is not writable. HR §11.2
      says the SCI is fully stubbed in the reference — match, and say so.
- [x] **P4-10 DRCR0/DRCR1** (HR §11.8). Offsets `0x071`/`0x072`, `val & 0x3`, stored and
      **never read by anything**. Pure storage, one line each.
- [x] **P4-11 "Unhandled onchip" logging.** Any offset with no arm should hit the P3-3
      dedup logger with an `[ONCHIP]` tag. HR §11.11 notes the reference itself logs
      unhandled on-chip accesses; that log is how the next boot run tells you which
      register to implement next.
 
 ### Phase 4 testing
 
- [x] **P4-T1 reset values.** One test asserting the reset value of every register in
      §0.4's table, read back through the width HR says it is readable at. The expected
      column comes verbatim from HR §11.1 (which cites `sh2core.c:1076-1129`) — a table
      transcribed from a document, not from the implementation. This single test is worth
      more than any other in the phase: it catches every transposed constant at once.
- [x] **P4-T2 write masking.** For each masked register, write `0xFFFFFFFF` (or `0xFF`)
      and assert the exact read-back: `IPRA → 0xFFF0`; `IPRB → 0xFF00`;
      `VCRA/B/C/D → 0x7F7F`; `VCRWDT → 0x7F7F`; `ICR → 0x0101`; `CCR → 0xCF`;
      `SBYCR → 0xDF`; `BCR1 → 0x9FF7` on master / `0x9FF7` with bit 15 forced on slave
      (compute from `(0x8000 & existing) | (0xFFFF & 0x1FF7)`); `BCR2 → 0x00FC`;
      `MCR → 0xFEFC`; `RTCSR → 0xF8`; `RTCOR → 0xFF`; `DVCR → 0x3`; `TCR → 0x83`;
      `TOCR → 0xF3`; `DRCR0/1 → 0x3`; `BBRA → 0xFF`; `BRCR → 0xF4DC`; `TCR0 → 0xFFFFFF`;
      `DMAOR → 0xF`; `VCRDMA0/1 → 0xFFFF`. All derived from HR §11's tables.
- [x] **P4-T3 destructive-byte-write quirks.** `IPRB` byte-write at `0x060` must destroy
      the low byte; `VCRD` byte-write at `0x068` must clear the low byte; byte writes at
      `0x061` and `0x069` must be ignored. These are HR-documented irregularities that a
      "reasonable" implementation would get wrong, so pin them individually with the HR
      §11.4 citation in the test name or comment.
- [x] **P4-T4 access-width matrix.** For a representative register of each width, prove
      all three widths behave per HR: `CCR` (byte and word both write); `BCR1` (long
      write, word read at `+2`, **no** word write); `DVSR` (long only, mirrored at
      `0x120`); `WTCNT` (byte only). A generic "every register readable at every width"
      test would be wrong — the widths are part of the hardware contract.
- [x] **P4-T5 master/slave BCR1.** `Sh2::new(false, …)` → `BCR1 & 0x8000 == 0`;
      `Sh2::new(true, …)` → `BCR1 & 0x8000 != 0`; after `reset()`, bit 15 survives and
      bits `0x03F0` are set. HR §4.

---

## Phase 5 — Real interrupt controller

**Unblocks:** every on-chip interrupt source (FRT, WDT, DIVU, DMAC, UBC) and SMPC NMI.
Also removes the hardcoded if/else chain that cannot express more than 4 sources.

- [x] **P5-1 Pending queue** (HR §10.1). `Vec`-or-array-backed queue of
      `{ vector: u8, level: u8 }`, capacity 50 (`MAX_INTERRUPTS`). Semantics to match
      exactly:
      - `send(vector, level)`: **dedupe by vector only** — if a queued entry already has
        this vector, return immediately and **do not upgrade its level**.
      - Append, then keep the queue sorted **ascending by level**, so the
        highest-priority entry is last. Vector is not a tiebreaker; equal levels keep
        insertion-adjacent order.
      - `remove(vector)`: find the first entry with that vector (level ignored), remove,
        compact.
      - HR §10.1 notes the reference has **no bounds check** and overruns the array at 51
        distinct vectors. **Deliberately diverge**: in Rust, cap the queue and log via the
        P3-3 dedup logger. Record the divergence in a comment — this is a case where
        matching the reference means reproducing a buffer overrun.
- [x] **P5-2 Delivery** (HR §10.2, §10.3). Replace
      `service_pending_interrupt` (`sh2.rs:908-952`) body: inspect **only** the last queue
      entry; deliver iff `level > (sr >> 4) & 0xF`; deliver exactly **one** per call. The
      push/vector/mask sequence at `sh2.rs:945-951` is already correct — keep it verbatim,
      including the "0 cycles charged" property (HR §13). Add the `level == 0x10 → SR.I = 0xF`
      clamp for NMI.
- [x] **P5-3 Migrate the four existing sources.** `vblank_pending`, `vblank_out_pending`,
      `smpc_irq_pending` and `sound_req_irq` become `send(vector, level)` calls with the
      existing constants (`sh2.rs:193-222`): `(0x40, 15)`, `(0x41, 14)`, `(0x46, 9)`,
      `(0x47, 8)`. **Keep the public `request_vblank_interrupt()` /
      `request_vblank_out_interrupt()` methods and the `sound_req_irq`/`smpc_irq_pending`
      fields as thin wrappers** — `lib.rs`, `m68k.rs` and 6+ unit tests reference them
      directly. Deleting them is a needless API break; `CLAUDE.md`'s stability rule is
      about `Sh2::new()` specifically but the spirit applies.
- [x] **P5-4 NMI (D-20)** (HR §10.4). `pub fn nmi(&mut self)`: `ICR |= 0x8000`, then
      `send(0xB, 0x10)`. Wire nothing to it yet — SMPC's `MSHNMI`/`SYSRES` commands belong
      to `docs/implementation-plans/smpc-peripheral.md`; leave a pointer there.
- [x] **P5-5 Cross-thread source injection (A-1).** The four migrated sources arrive from
      *other threads* today (`sound_req_irq` is an `Arc<AtomicBool>` written by Core 4's
      M68K; VBLANK is generated in `Sh2::run_loop` itself but belongs on Core 3). A shared
      queue therefore needs interior mutability. Use the existing pattern:
      `pub irq_in: Option<Arc<Mutex<InterruptQueue>>>` on `Sh2`, defaulted `None` in
      `Sh2::new()` and wired by `SaturnSystem::start` — exactly how `scu_dsp`
      (`sh2.rs:163`, `lib.rs:139`) and `sound_req_irq` (`sh2.rs:148`, `lib.rs:137`) already
      work. **Do not change `Sh2::new()`'s signature.** When `None`, fall back to a
      CPU-local queue so bare unit tests keep working unchanged.
- [x] **P5-6 Waking a parked core.** Delivery to a core that is parked in
      `LockStepSync::park_while_inactive` must call `sync.set_thread_active(core_id, true)`.
      Relevant today for Core 1 (parked until `SSHON`) and, after P2-1's parking lands,
      for a sleeping Core 0. Use `LockStepSync`'s park condvar, never the drift condvar —
      see `sync.rs`'s doc comment on why.
- [x] **P5-7 Do not couple this to `BusArbiter`.** Interrupt delivery performs two
      `write_long`s and one `read_long`, which already go through `bus_wait()`
      (`sh2.rs:383`) like any other access. No new arbitration is needed. Record that,
      because "interrupts must coordinate through the arbiter" is a plausible-sounding
      wrong turn.

### Phase 5 testing

- [x] **P5-T1 sort/dedupe invariants.** Push `(vector=0x40, level=15)`,
      `(0x47, 8)`, `(0x41, 14)`, then `(0x40, 2)` again. Assert: the queue holds 3
      entries (the duplicate vector rejected), the last entry is level 15, and the level
      of the first `0x40` entry is **still 15** (HR §10.1: "the level of the existing
      entry is *not* upgraded"). That last assertion is the one that distinguishes a
      faithful port from a "sensible" one.
- [x] **P5-T2 strictly-greater masking.** With `SR.I = 8`, a level-8 interrupt must
      **not** be delivered and a level-9 one must be (HR §10.2's `>` not `>=`). Also
      assert a level-0 interrupt is never deliverable at any mask.
- [x] **P5-T3 one per call.** Two deliverable interrupts queued → one `step()` takes
      exactly one, and the second is still queued.
- [x] **P5-T4 NMI clamp.** `nmi()` then deliver: `PC == [VBR + 0x2C]` (vector 11),
      `SR.I == 0xF` (not 16), `ICR & 0x8000 != 0`. Vector arithmetic `0xB * 4 = 0x2C`
      computed in the comment.
- [x] **P5-T5 no regression.** All six existing interrupt tests
      (`vblank_interrupt_masked_stays_pending` `sh2.rs:2076`,
      `vblank_interrupt_enters_and_returns` `:2088`,
      `vblank_out_interrupt_masked_stays_pending` `:2120`,
      `vblank_out_interrupt_enters_and_returns` `:2132`,
      `vblank_in_outranks_vblank_out_when_both_pending` `:2165`,
      `sound_req_irq_enters_through_its_own_vector_at_level_9` `:2037`, plus
      `intback_populates_real_status_and_fires_system_manager_irq` `:1970`) must pass
      **unmodified**. If any needs editing, the migration changed observable behaviour and
      that needs justifying.
- [x] **P5-T6 no interrupt in a delay slot.** With an unmasked interrupt pending, execute
      a `BRA` whose delay slot is a `MOV #imm`: assert the delay slot ran *and* the
      interrupt was taken only afterwards, with the pushed PC equal to the branch target
      (HR §8.2 item 4). This property holds today by construction; pin it before the

---

## Phase 6 — DIVU hardening

**Unblocks:** removes a live process-killing panic; completes a peripheral the BIOS and
essentially every 3D game uses.

- [x] **P6-1 (D-12) Guard both divisions.** In the 32÷32 path (`sh2.rs:1490-1491`) and
      the 64÷32 path (`sh2.rs:1524-1525`), use `checked_div`/`checked_rem` (or an explicit
      `divisor == -1 && dividend == MIN` pre-test). HR §11.6 records that the reference
      performs **no** overflow check on the non-zero-divisor path and leaves it to C's UB.
      Rust's behaviour there is a hard panic, and `docs/mimas-architecture-spec.md` §2.3
      makes a panic fatal to the whole emulator — so this must diverge. Decide the result
      explicitly and comment it: the natural choice is the two's-complement wrap
      (`i32::MIN`, remainder 0), which is what the hardware's own overflow path would
      approximate; whatever is chosen, mark it a DEVIATION-by-necessity, not a guess at
      hardware.
- [x] **P6-2 (D-13) 64÷32 quotient overflow.** After computing `quotient: i64`:
      `if quotient > 0x7FFF_FFFF { DVCR |= 1; DVDNTL = 0x7FFF_FFFF; DVDNTH = 0xFFFF_FFFE; }`
      `else if ((quotient >> 32) as i32) < -1 { DVCR |= 1; DVDNTL = 0x8000_0000; DVDNTH = 0xFFFF_FFFE; }`
      `else { DVDNTL = quotient as u32; DVDNTH = remainder as u32; }`, then
      `DVDNTUL = DVDNTL; DVDNTUH = DVDNTH;` unconditionally. HR §11.6 flags both
      `0xFFFF_FFFE` values as Yabause `// fix me` and states the true hardware value is
      **not deducible** — copy that note verbatim into the Rust comment so nobody
      "corrects" it from a guess. Note also HR's remark that the negative test is
      `(s32)(quotient >> 32) < -1`, not a comparison against `0x80000000`.
- [x] **P6-3 (D-14) `DVDNTUH`/`DVDNTUL` write paths.** Add `0x118`/`0x138` and
      `0x11C`/`0x13C` arms to `write_onchip` (`sh2.rs:1470`). HR §11.6: they are
      independently writable and are additionally overwritten by every division.
- [x] **P6-4 (D-15) Overflow interrupt.** At each of the four `DVCR |= 1` sites, add
      `if DVCR & 0x2 { send(VCRDIV & 0x7F, (IPRA >> 12) & 0xF) }`. HR §10.6 records the
      **DEVIATION** that the reference reads the level from `MSH2->onchip.IPRA` even on
      the slave. Implement the correct-looking per-CPU `IPRA` and leave a comment naming
      the reference's quirk plus what symptom would indicate the quirk is load-bearing
      (slave-side DIVU interrupts firing at the wrong priority). Depends on Phase 4
      (IPRA) and Phase 5 (queue).
- [x] **P6-5.** Confirm the mirrored offsets. HR §11.1: every DIVU register has a
      `0x100`-block and a `0x120`-block alias and both behave identically. `sh2.rs:1458-1465`
      and `:1472-1531` already list both — keep them when the fields move into `OnChip`.
- [x] **P6-6.** DIVU is instantaneous and charges no cycles (HR §11.6). Keep it that way;
      note it explicitly so Phase 8 does not "fix" it.

### Phase 6 testing

- [x] **P6-T1 (D-12).** `DVSR = 0xFFFF_FFFF` (−1), `DVDNT = 0x8000_0000` — must not
      panic. Same for the 64÷32 path with `DVDNTH:DVDNTL = 0x8000_0000_0000_0000`,
      `DVSR = −1`. Assert the documented divergent result, and name the test so it is
      obviously a crash-regression test.
- [x] **P6-T2 (D-13).** Derive the trigger inputs with a throwaway script, not by
      running the emulator: pick `DVDNTH:DVDNTL` and `DVSR` such that the true quotient
      is exactly `0x8000_0000` (one past positive overflow) and assert
      `DVDNTL == 0x7FFF_FFFF`, `DVDNTH == 0xFFFF_FFFE`, `DVCR & 1 == 1`. Mirror for the
      negative case.
- [x] **P6-T3 (D-14).** Write `DVDNTUH`/`DVDNTUL` directly, read back; then trigger a
      division and assert both were overwritten with the new `DVDNTH`/`DVDNTL`.
- [x] **P6-T4 (D-15).** With `DVCR = 0x2` and `VCRDIV = 0x53`, `IPRA = 0xC000`, force a
      divide-by-zero and assert an interrupt with vector `0x53` and level `0xC` is queued.
      Vector/level extraction (`& 0x7F`, `>> 12 & 0xF`) taken from HR §10.6.
- [x] **P6-T5.** Extend the existing `test_onchip_division` (`sh2.rs:2316`) rather than
      replacing it — it already pins `100 / 3 = 33 r 1` and the `2^33 / 4 = 2^31` 64-bit
      case with independently-obvious values. Add the divide-by-zero `DVDNTH` formulas
      (`0xFFFFFFFC | ((val >> 29) & 3)` for negative dividends, `val >> 29` for
      non-negative) with hand-computed constants, since the current test only checks the
      `DVCR` bit.

---

## Phase 7 — Free-Running Timer (FRT)

**Unblocks:** the on-chip timer the BIOS uses for delay loops and periodic ticks. Depends
on Phase 4 (registers) and Phase 5 (interrupts).

- [x] **P7-1 State.** `frc_leftover: u32` and `frc_shift: u32` on the CPU, reset to `0`
      and `3` (÷8) per HR §4.
- [x] **P7-2 Register behaviour** (HR §11.3), exactly:
      - `TIER` `0x010`: write → `TIER = (val & 0x8E) | 0x1` (bit 0 always forced set).
        Additionally, if `val & 0x80` and `FTCSR & 0x80`, raise the input-capture
        interrupt immediately. Also writable as a **long** at offset `0x010`.
      - `FTCSR` `0x011`: write → `FTCSR = (FTCSR & (val & 0xFE)) | (val & 0x1)` —
        bits 7..1 are write-0-to-clear (survive only if already set *and* written 1),
        bit 0 (CCLRA) directly assignable.
      - `FRC` `0x012`/`0x013`: byte writes set the H/L halves independently; a word read
        at `0x012` returns the full 16-bit value.
      - `OCRA`/`OCRB` `0x014`/`0x015`: selected by `TOCR & 0x10` — clear selects OCRA.
        Same selector on read and write; byte writes merge into the selected register's
        half; word read at `0x014` likewise.
      - `TCR` `0x016`: write → `TCR = val & 0x83`; `val & 3` selects the prescaler:
        `0 → frc_shift = 3` (÷8), `1 → 5` (÷32), `2 → 7` (÷128), `3 → external clock`
        (log "not implemented", leave `frc_shift` unchanged — HR is explicit).
      - `TOCR` `0x017`: write → `TOCR = 0xE0 | (val & 0x13)`.
      - `FICR` `0x018`/`0x019`: read-only from software; written only by the input-capture
        hook.
- [x] **P7-3 `FRTExec(cycles)`** (HR §11.3), exactly:
      `frcold = FRC; mask = (1 << shift) - 1; frctemp = FRC + ((cycles + leftover) >> shift);`
      `leftover = (cycles + leftover) & mask;`
      then the three **crossing** tests in this order — OCRA (`frctemp >= OCRA && frcold < OCRA`),
      OCRB (same shape), overflow (`frctemp > 0xFFFF`) — with `FTCSR |= 0x8 / 0x4 / 0x2`
      respectively, `CCLRA` (`FTCSR & 1`) zeroing `frctemp` and `leftover` on an OCRA
      match, and finally `FRC = frctemp as u16` (truncating). HR notes explicitly that
      the crossing test can *miss* a compare if the counter jumps past OCR in the same
      step it wraps — preserve that, and comment it.
- [x] **P7-4 Interrupts** (HR §10.6): OCIA → vector `VCRC & 0x7F`, level `(IPRB & 0xF00) >> 8`,
      gated on `TIER & 0x8`. OCIB → **the same vector field** `VCRC & 0x7F`, same level,
      gated on `TIER & 0x4` (HR §10.6 flags this as possibly wrong and says whether
      hardware has a distinct OCIB vector is not deducible — copy that note). OVI →
      vector `(VCRD >> 8) & 0x7F`, same level, gated on `TIER & 0x2`. ICI → vector
      `(VCRC >> 8) & 0x7F`, level `(IPRB >> 8) & 0xF`, gated on `TIER & 0x80`.
- [x] **P7-5 Input capture** (HR §11.3). `pub fn frt_input_capture(&mut self)`:
      `FTCSR |= 0x80; FICR = FRC; if TIER & 0x80 { send(...) }`. Nothing calls it yet
      (it is driven by an external memory-mapped write hook in the reference). **Do not**
      port the SSH2 variant's "run the other CPU for 32 cycles" block — HR §11.3 calls it
      "a synchronisation hack with no hardware counterpart", and it is meaningless in a
      thread-per-core design where the other CPU is already running.
- [x] **P7-6 Driving it (A-4).** HR §7 calls `FRTExec(cycles)` once per `SH2Exec` batch,
      **with the requested cycle count, not the retired one** — HR §13 item 9 flags that
      as a deviation. Mimas has no batch entry point; `run_loop` steps one instruction at
      a time (`sh2.rs:1708`). Drive `FRTExec` from `step()` with the **retired** cycle
      count of that instruction. That is strictly more accurate than the reference and
      needs no new loop structure — but it changes the FRT's effective rate relative to
      Yabause, so record it as a deliberate divergence. Guard the per-step call behind a
      cheap `if !frt_enabled` early-out so the common case costs nothing.

### Phase 7 testing

- [x] **P7-T1 prescaler.** For each `TCR & 3` value in `{0,1,2,3}`, assert `frc_shift`
      becomes `{3,5,7,unchanged}` (HR §11.3's table) and that `TCR` reads back
      `val & 0x83`.
- [x] **P7-T2 counter advance.** With `frc_shift = 3` and `leftover = 0`, feed exactly
      `8 * k + r` cycles and assert `FRC == k` and `leftover == r` for a few
      hand-computed `(k, r)` pairs. The fractional accumulator is the part an
      implementation gets wrong.
- [x] **P7-T3 `FTCSR` write-0-to-clear.** Set `FTCSR = 0x0E` (OCFA|OCFB|OVF) by forcing
      the flags via a compare, then write `0x0A` and assert the result is `0x0A` — bit 2
      cleared because the written bit was 0, bits 3 and 1 surviving because both the
      stored and written bits were 1. Then write `0x01` and assert `FTCSR == 0x01`. All
      three expected values computed by hand from
      `FTCSR = (FTCSR & (val & 0xFE)) | (val & 0x1)`.
- [x] **P7-T4 OCRA/OCRB selector.** With `TOCR & 0x10 == 0`, a word write at `0x014`
      must land in OCRA and leave OCRB at `0xFFFF`; set bit 4 and repeat for OCRB.
- [x] **P7-T5 compare match + CCLRA.** `OCRA = 0x100`, `TIER = 0x08`, `FTCSR = 0x01`;
      advance past 0x100 and assert: an interrupt with vector `VCRC & 0x7F` was queued,
      `FTCSR & 0x8` set, and `FRC` reset to 0 by CCLRA. Then repeat with `FTCSR & 1`
      clear and assert `FRC` kept counting.
- [x] **P7-T6 missed-compare deviation.** Construct the case HR warns about — advance by
      enough cycles in one call to jump from below OCRA past 0xFFFF — and assert the OCRA
      match is **missed** and only OVF is set. A test that asserts a *bug* must say so in
      its name and cite HR §11.3.
- [x] **P7-T7 TIER ICI re-arm.** With `FTCSR & 0x80` already set, writing `TIER` with
      bit 7 must raise the input-capture interrupt immediately (HR §11.3,
      `sh2core.c:1416-1423`).

---

## Phase 8 — Per-opcode cycle costs and memory wait states

**Unblocks:** honest `ClockThrottle` pacing, plausible FRT/WDT/DMAC rates, and a
`LockStepSync` slack window that means something.

- [x] **P8-1 Base costs.** Replace the flat `cycles += 2` (`sh2.rs:788`) with a per-handler
      charge. Full enumeration from HR §9:
      - **1 cycle:** every §9.1 instruction; every §9.6 shift/rotate; `ADD`, `ADD #imm`,
        `ADDC`, `ADDV`, `SUB`, `SUBC`, `SUBV`, `NEG`, `NEGC`, `DT`, all nine `CMP` forms,
        `DIV0S`, `DIV0U`, `DIV1`, `MULS.W`, `MULU.W`, `CLRMAC`; `AND`, `AND #imm`, `OR`,
        `OR #imm`, `XOR`, `XOR #imm`, `NOT`, `TST`, `TST #imm`; `LDC Rm,{SR,GBR,VBR}`,
        `LDS Rm,{MACH,MACL,PR}`, `STC {SR,GBR,VBR},Rn`, `STS {MACH,MACL,PR},Rn`;
        `NOP`, `CLRT`, `SETT`; the illegal-instruction handler.
      - **2 cycles:** `MUL.L`, `DMULS.L`, `DMULU.L`; `BRA`, `BRAF`, `BSR`, `BSRF`, `JMP`,
        `JSR`, `RTS`; `BF/S` and `BT/S` **when taken**.
      - **3 / 1 cycles:** `BT`, `BF` — 3 taken, 1 not taken. `BF/S`/`BT/S` not taken: 1.
      - **3 cycles:** `SLEEP`.
      - **`1 + cycle`:** `MOV.B/W/L @Rm,Rn`, `MOV.B/W/L @Rm+,Rn`, `MOV.W/L @(R0,Rm),Rn`,
        `MOV.B/W/L @(disp,Rm),R0|Rn`, `MOV.B/W/L @(disp,GBR),R0`, `MOV.W/L @(disp,PC),Rn`;
        `MOV.W/L Rm,@Rn`, `MOV.B/W/L Rm,@-Rn`, `MOV.B/W/L Rm,@(R0,Rn)`,
        `MOV.B/W R0,@(disp,Rn)`, `MOV.L Rm,@(disp,Rn)`, `MOV.B/W/L R0,@(disp,GBR)`;
        `LDS.L @Rm+,MACH`; `STS.L {MACH,MACL,PR},@-Rn`.
      - **`2 + cycle`:** `STC.L {SR,GBR,VBR},@-Rn`.
      - **`3 + rcycle`:** `LDC.L @Rm+,SR`, `LDC.L @Rm+,GBR`; `TST.B #imm,@(R0,GBR)`.
      - **`3 + rcycle + wcycle`:** `AND.B`, `XOR.B`.
      - **`3 + rcycle1 + rcycle2`:** `MAC.L`, `MAC.W`.
      - **`4 + cycle + wcycle`:** `TAS.B`.
      - **`4 + rcycle + wcycle`:** `RTE`.
      - **`8 + cycle + wcycle + wcycle2`:** `TRAPA`.
      - **0 cycles:** interrupt entry (HR §13).
      - **Reference cycle irregularities (HR §13's table) — decide per item whether to
        match:** `MOV.B @(R0,Rm),Rn` charges `rcycle` with **no base cycle**;
        `MOV.L Rm,@Rn` charges `cycle` with no base; `LDC.L @Rm+,VBR` charges a flat `3`
        (rcycle discarded); `LDS.L @Rm+,MACL` and `LDS.L @Rm+,PR` charge a flat `1`;
        `OR.B #imm,@(R0,GBR)` charges a flat `3`. **Recommendation: do not reproduce
        these six.** They are transcription slips in the reference with no hardware
        meaning, and unlike the semantic deviations they cannot change program results —
        only pacing. Document the choice in one comment listing all six, so a future
        reader knows it was a decision, not an oversight.
- [x] **P8-2 Memory wait states** (HR §1.2). Add a `mem_cycles_r(addr) -> u32` /
      `mem_cycles_w(addr) -> u32` pair keyed on `addr & 0xDFF0_0000`:
      | Region | Read | Write |
      |---|---|---|
      | `0x00000000` BIOS ROM, `0x00100000` backup | 16 | 0 |
      | `0x00200000` Low Work RAM | 12 | 7 |
      | `0x02000000` CS0, `0x05800000` CS2 | 24 | 0 |
      | `0x05A00000` Sound RAM, `0x05B00000` Sound regs | 50 | 7 (sound RAM only) |
      | `0x05C00000` VDP1 RAM | 50 | 2 |
      | `0x05E00000` VDP2 RAM | `getVramCycle(addr)` | `getVramCycle(addr)` |
      | `0x06000000` High Work RAM | 0 | 2 |
      | anything else | 0 | 0 |
      `getVramCycle` lives outside the SH-2 files — treat it as a constant (document the
      placeholder) until `docs/implementation-plans/vdp2.md` provides it.
- [x] **P8-3 Fetch cost.** HR §7's loop charges nothing extra for the fetch itself
      beyond what the handler adds; the fetch's own wait state is not modelled in the
      reference either. Match, and note it.
- [x] **P8-4 `run_loop` batching (A-5).** `sh2.rs:1716-1720` derives `batch_mask` from
      `slack_limit` and compares `cycles & !batch_mask` across a step. With costs now
      ranging 1..~60, a step can jump the whole mask and skip a `sync_core` call, or hit
      it every time. Replace with an explicit accumulator: `pending_sync += delta; if
      pending_sync >= batch { sync_core(...); pending_sync = 0; }`.
- [x] **P8-5 `ClockThrottle`.** No change to `throttle.rs`. Once P8-1 lands, delete the
      "an existing, accepted simplification — see `Sh2::step()`" clause from
      `throttle.rs:29-40`'s doc comment (it will no longer be true of the SH-2, only of
      the M68K) and point it at `docs/implementation-plans/scsp.md` for the M68K side.
- [x] **P8-6 `bus_wait` interaction.** `bus_wait()` (`sh2.rs:383`) can overwrite
      `self.cycles` wholesale with `caught_up` from `acquire_bus_sync`. Adding wait-state
      cycles must not be lost across that assignment — audit the ordering, and add a test.

### Phase 8 testing

- [x] **P8-T1.** For a representative instruction of each cost class, assert the exact
      `cycles` delta across one `step()`. Expected values transcribed from HR §9's Cycles
      column, which is itself the literal expression in the reference — so the source is
      a document, not the code under test.
- [x] **P8-T2.** Assert the wait-state table by reading the same instruction (e.g.
      `MOV.L @Rm,Rn`) from each region and comparing deltas: High WRAM (`1 + 0`),
      Low WRAM (`1 + 12`), BIOS (`1 + 16`), Sound RAM (`1 + 50`). The *differences*
      between regions are what the table encodes.
- [x] **P8-T3 conditional branch cost.** `BT` taken = 3, not taken = 1; `BT/S` taken = 2
      + delay-slot cost, not taken = 1 (HR §9.7). The taken/not-taken asymmetry is a
      common miss.
- [x] **P8-T4 breakage.** `e2e-tests`'s `test_tier3_combination_f1_f3_lockstep_cpu_stepping`
      asserts `cpu.cycles == 2` after one `step()` of opcode `0x0000`. After P3-1 that
      opcode raises an exception (1 cycle) and after P8-1 nothing costs a flat 2. Rewrite
      it around a `NOP` and the documented 1-cycle cost, in the same commit.
- [x] **P8-T5 throttle end-to-end.** With `ThrottleSpeed::Multiplier(1.0)`, run a tight
      known loop (e.g. 1000 iterations of `DT`+`BF`) and assert wall time lands within a
      wide band of `cycles / SH2_CLOCK_HZ`. Wide, because this is a smoke test that the
      units line up, not a precision measurement — `throttle.rs`'s own tests already
      cover the pacer itself.

---

## Phase 9 — On-chip DMA controller (DMAC)

**Status:** done — see `history.md` Chapter 15.

**Unblocks:** game compatibility. Not believed to be on the BIOS boot path. Depends on
Phases 4, 5, 8.

- [x] **P9-1 Registers** (HR §11.8). `SAR0`/`DAR0` raw; `TCR0 = val & 0xFFFFFF`;
      `CHCR0`; `SAR1`/`DAR1`/`TCR1`/`CHCR1`; `VCRDMA0`/`VCRDMA1 = val & 0xFFFF` (used
      **unmasked** when sending the interrupt); `DMAOR = val & 0xF`.
- [x] **P9-2 `CHCR` write protocol** (HR §11.8): if `TCRn != 0`, flush any in-flight
      transfer first (`DMAProc(0x7FFFFFFF)`); then `CHCRn = val & 0xFFFF`; then
      `CHCRn = (val & !2) | (CHCRn & (val | CHCRnM) & 2)` — TE (bit 1) is write-0-to-clear
      through a shadow register `CHCRnM`; then arm if `(DMAOR & 7) == 1` (DME set, NMIF
      clear, AE clear) and DE set with TE clear. HR notes the channel-0 arm test uses the
      raw `val & 3` while channel 1 uses `CHCR1 & 3` — an inconsistency in the reference;
      pick one, note it. **Implemented as documented**: channel 0 arms on the raw written
      `val & 3`, channel 1 arms on `new_CHCR1 & 3` (`sh2.rs:3465`/`:3484`) — matches the
      reference's own inconsistency rather than papering over it.
- [x] **P9-3 `CHCR` read side effect.** Reading `CHCR0`/`CHCR1` clears the corresponding
      `CHCRnM` shadow to 0 (HR §11.8). Implemented in the 32-bit `read_onchip` path
      (`sh2.rs:3301`/`:3309`, via `Cell<u32>` shadows so the clear can happen through `&self`).
- [x] **P9-4 `CHCR` bit fields** (HR §11.8): bit 0 DE; bit 1 TE; bit 2 IE; bit 3
      "dual channel" (**clear** doubles the cycle budget); bits 11-10 transfer size
      (`0` byte, `1` word, `2` longword, `3` 16-byte burst implemented as longwords at a
      quarter cost); bits 13-12 source address mode (`0` fixed, `1` increment, `2`
      decrement, `3` treated as fixed); bits 15-14 destination address mode (`0` fixed,
      `1` increment, `2` decrement, `3` treated as fixed).
- [x] **P9-5 Budgeted transfer engine.** `DMAExec()` = `DMAProc(200)`. `DMAProc(cycles)`
      checks `DMAOR & 0x6` (AE/NMIF abort), picks channels per `DMAOR & 0x8`
      (round-robin vs channel-0 priority), applies the dual-channel doubling, then
      `DMATransferCycles`. That accumulates `copy_clock += cycles` and moves one unit per
      `eat` cycles, where `eat = getEatClock(SAR, DAR)` is HR §11.8's full source×dest
      latency table (CS2 source → 1; VDP2-RAM source → 44/50/427/1/50/44; VDP1-RAM source
      → 50/570/225/44; everything else → 14/20/30/82/14). Port the table verbatim.
      **Note:** arming a channel (the DE/DME transition, `sh2.rs:3476`/`:3494`/`:3512`)
      synchronously calls `DMAExec()` = a free 200-cycle burst, exactly as the reference
      does (`sh2core.c:2140`) — this surprised the initial P9-T5 test (see Chapter 15) but
      is real, faithfully-ported behavior, not a bug.
- [x] **P9-6 Completion.** On `TCR <= 0`: if `CHCR & 0x4`, send `VCRDMA` (unmasked) at
      level `(IPRA & 0xF00) >> 8`; set `CHCR |= 2` and `CHCRM |= 2`.
- [x] **P9-7 Cache bypass.** HR §11.8: all DMAC memory access goes through the `…Nocache`
      accessors, so DMA neither hits nor invalidates cache lines. Relevant once Phase 11
      lands — write it down now. Satisfied trivially today: `dma_transfer_cycles` uses
      `raw_read_*`/`raw_write_*` (`sh2.rs:2097`-`2106`), which don't touch the (still
      stub-level, pre-Phase-11) cache model at all.
- [x] **P9-8 Architecture (A-6).** This is a **third** DMA engine alongside the SCU DSP's
      2-of-8 addressing modes (`scu_dsp.rs`) and `Sh2::execute_scu_dma` (`sh2.rs:1536`).
      Two hard constraints:
      (i) it must take `BusArbiter::lock_for_dma()` for the transfer, exactly as
      `execute_scu_dma` does (`sh2.rs:1548`, `:1631`);
      (ii) inside the lock it must use `raw_read_byte`/`raw_write_byte`, **not**
      `read_byte`/`write_byte` — the latter call `bus_wait()` and would self-deadlock
      against the lock the same engine just took. `execute_scu_dma` already does exactly
      this and says why (`sh2.rs:1553`); mirror it.
      Do **not** give the DMAC its own thread. It is on-chip SH-2 silicon and belongs to
      the core that owns it; the thread-per-component model maps threads to chips, and the
      SH-2 is one chip. Confirmed: `dma_transfer_cycles` takes/releases the arbiter lock
      around the transfer loop (`sh2.rs:2090`-`2142`) and runs inline from `step()`
      (`sh2.rs:2179`), no dedicated thread.
- [x] **P9-9.** Do **not** port `DMATransfer()` (the legacy instantaneous engine) — HR
      §11.8 says it is unreachable (`OLD_DMA` is 0). Confirmed absent from `sh2.rs`.

### Phase 9 testing

- [x] **P9-T1.** For each of the 3 usable source-address modes × 3 destination modes ×
      4 transfer sizes, run a small transfer between two Work-RAM windows pre-filled with
      a known pattern and assert the exact resulting bytes. The patterns should make an
      off-by-one stride visible (e.g. an incrementing byte sequence).
- [x] **P9-T2 TE write-0-to-clear.** Set TE via a completed transfer; write `CHCR` with
      bit 1 = 1 and assert TE **survives**; write with bit 1 = 0 and assert it clears.
      Then assert reading `CHCR` zeroes the shadow. Derived from HR §11.8's expression.
- [x] **P9-T3 arming gate.** With `DMAOR = 0` (DME clear), setting DE must not start a
      transfer; with `DMAOR = 1` it must; with `DMAOR = 3` (NMIF set) it must not. The
      `(DMAOR & 7) == 1` test is the point.
- [x] **P9-T4 completion interrupt.** `CHCR & 0x4` set, `VCRDMA0 = 0x1234`,
      `IPRA = 0x0500` → on completion, an interrupt with vector `0x1234` (**unmasked** —
      HR §11.8 is explicit) at level 5 must be queued.
- [x] **P9-T5 `eat` table.** Rather than asserting absolute timings, assert *relative*
      ones: a VDP1-RAM→VDP1-RAM transfer (570) must take more than 10× the cycles of a
      WRAM→WRAM one (14) for the same byte count. Ratios derived from HR §11.8's table.
      **Fixed in Chapter 15**: the original version used `TCR0 = 10` (140 cycles), which
      the arm-time 200-cycle `DMAExec()` burst (P9-5's note above) finishes on its own
      before the test's explicit `dma_proc()` call ever runs, so the assertion it was
      trying to make (139 cycles shouldn't finish, 140 should) never actually exercised —
      it just happened to already be `0` either way. Raised to `TCR0 = 20` (280 cycles,
      more than the free burst can cover) so the boundary is real.
- [x] **P9-T6 arbiter interaction.** With the DMAC mid-transfer, a second thread's
      `Sh2::read_byte` must block until it finishes (mirrors the existing e2e test
      `test_tier3_combination_f2_f3_bus_arbiter_blocks_cpu`).

---

## Phase 10 — Watchdog Timer (WDT)

**Unblocks:** little. HR §11.5 says watchdog mode generates no reset in the reference and
only interval mode does anything. Include it for completeness and because `WTCSR`'s magic
write protocol is the kind of thing a BIOS read-back verify will catch.

- [ ] **P10-1 State.** `wdt_isenable: bool`, `wdt_isinterval: bool`, `wdt_shift: u32`,
      `wdt_leftover: u32`, plus the `WTCSRM` shadow. Reset: `false`, `true`, `1`, `0`
      (HR §4).
- [ ] **P10-2 The magic write protocol** (HR §11.5). A **word** write to offset `0x080`:
      - high byte `0xA5` → low byte targets `WTCSR`:
        `val & 7` selects `wdt_shift` from the table `{0→1, 1→6, 2→7, 3→8, 4→9, 5→10,
        6→12, 7→13}`; `wdt_isenable = val & 0x20` (TME); `wdt_isinterval = !(val & 0x40)`
        (**cleared bit 6 means interval mode**); `WTCSR = (WTCSR & (WTCSRM | val) & 0x80)
        | (val & 0x67)` then `WTCSR &= !0x80`; if `WTCSR & 0x20` then `SBYCR &= 0x7F`
        else `WTCSR &= !0x80` and `WTCNT = 0`.
      - high byte `0x5A` → low byte targets `WTCNT`: `if WTCSR & 0x20 { WTCNT = val as u8 }`
        (writable only while enabled).
      A **word** write to offset `0x082` targets `RSTCSR`: exactly `0xA500` → `RSTCSR &= 0x7F`;
      high byte `0x5A` → `RSTCSR = (RSTCSR & 0x80) | (val & 0x60) | 0x1F`.
      Reads: `WTCSR` at `0x080` and `WTCNT` at `0x081` are plain byte reads; **there is no
      read path for `RSTCSR`** — match, and note it.
- [ ] **P10-3 `WDTExec(cycles)`** (HR §11.5): early-return if `!isenable || (WTCSR & 0x80)
      || (RSTCSR & 0x80)`; then the same shift/leftover accumulate as the FRT; on
      `wdttemp > 0xFF`, if interval mode set `WTCSR |= 0x80` and send vector
      `(VCRWDT >> 8) & 0x7F` at level `(IPRA >> 4) & 0xF`; if watchdog mode, **nothing
      happens** (HR: logged as "not implemented"). Finally `WTCNT = wdttemp as u8`.
- [ ] **P10-4.** Drive from `step()` with the retired cycle count, same decision and
      same rationale as P7-6.

### Phase 10 testing

- [ ] **P10-T1 magic gate.** A word write to `0x080` with a high byte that is neither
      `0xA5` nor `0x5A` must change nothing. A `0x5A` write to `WTCNT` while `TME` is
      clear must be ignored. Both are HR-documented and both are what a naive
      implementation gets wrong.
- [ ] **P10-T2 divisor table.** All 8 `val & 7` values → the 8 `wdt_shift` values in
      HR §11.5's table, asserted individually.
- [ ] **P10-T3 interval overflow.** Enable in interval mode with `shift = 1`, advance
      `0x100 * 2` cycles, assert `WTCSR & 0x80` set and the vector/level from
      `VCRWDT`/`IPRA` queued.
- [ ] **P10-T4 watchdog mode does nothing.** Same setup in watchdog mode → no interrupt,
      no reset, `WTCSR & 0x80` **not** set. Name the test after the fact that this is the
      reference's documented non-implementation, so it is not read as a bug.
- [ ] **P10-T5 `RSTCSR` bits 4-0 always read 1** after a `0x5A` write (HR §11.5), and
      `0xA500` exactly clears WOVF.

---

## Phase 11 — Cache model

**Unblocks:** correctness for code using the cache arrays with real cache semantics, plus
the single biggest available `WorkRam` contention win. This is where the architectural
tension is sharpest — read A-7 before writing code.

- [ ] **P11-1 Geometry** (HR §12.1). 4 ways × 64 entries × 16-byte lines = 4 KiB,
      per-line `tag: u32` and `valid: bool`, per-entry 6-bit `lru` shared by all 4 ways,
      **no dirty bit** (write-through). Address decomposition: `AREA_MASK = 0xE0000000`,
      `TAG_MASK = 0x1FFFFC00` (bits 28-10), `ENTRY = (addr & 0x3F0) >> 4` (bits 9-4),
      `LINE = addr & 0xF`.
- [ ] **P11-2 Region dispatch** (HR §12.1): `CACHE_USE = 0x00000000`,
      `CACHE_THROUGH = 0x20000000`, `CACHE_PURGE = 0x40000000`,
      `CACHE_ADDRES_ARRAY = 0x60000000`, `CACHE_DATA_ARRAY = 0xC0000000`,
      `CACHE_IO = 0xE0000000`. `0x80000000` and `0xA0000000` have no constant and behave
      as `CACHE_THROUGH`.
- [ ] **P11-3 Read path** (HR §12.2): if disabled → uncached read, **no lookup, no fill**;
      else probe ways 0,1,2,3 **in that fixed order** for `valid && tag == tagaddr`; on
      hit `update_lru(way)` and return from the line (big-endian assembly for word/long);
      on miss `select_way_to_replace`, `update_lru`, set the tag, fill 16 bytes as **four
      longword reads of the aligned line** that bypass the cache, set valid, return.
- [ ] **P11-4 Write path** (HR §12.3): if disabled → uncached write; else probe for a hit
      and update the line bytes + LRU if found; then **unconditionally** write through to
      memory; **no write-allocate** on a miss.
- [ ] **P11-5 Associative purge** (HR §12.3.1). Only on a **longword** write to the purge
      region. Two DEVIATIONs to reproduce or reject explicitly: the loop bound is
      `i < 3` so **way 3 is never purged**, and the tag comparison **does not check
      valid** so a stale-tag invalid line consumes the `break`. Both are plainly visible
      bugs in the reference. **Recommendation: reproduce them**, because purge behaviour
      is exactly the kind of thing real code was validated against; mark both loudly.
      Byte/word writes to the purge region are ordinary uncached writes; reads never
      reach here (P3-4 already returns `0xFFFFFFFF`).
- [ ] **P11-6 Pseudo-LRU** (HR §12.3.3). `update_lru(way)`: way 3 → `lru |= 0xB`; way 2 →
      `lru &= 0x3E; lru |= 0x14`; way 1 → `lru |= 0x20; lru &= 0x39`; way 0 → `lru &= 0x07`.
      `select_way_to_replace(lru)`: if `CCR & 0x08` (two-way mode) return
      `if lru & 1 == 1 { 2 } else { 3 }`; else `(lru & 0x38) == 0x38 → 0`;
      `(lru & 0x26) == 0x06 → 1`; `(lru & 0x15) == 0x01 → 2`; `(lru & 0x0B) == 0x00 → 3`;
      fallback `0`. Note HR: two-way mode affects **replacement only** — lookups still
      probe all four ways.
- [ ] **P11-7 clear/enable/disable** (HR §12.3.2). `cache_clear` sets enable = 0 then
      zeroes every tag, every data byte and every valid bit and all LRU state.
      `cache_enable` sets enable = 1 and **does not clear** (HR quotes the reference's own
      comment). `cache_disable` clears enable and **preserves contents**, so re-enabling
      exposes the old lines. Wire to P4-5's `CCR` handler in HR's order: purge first
      (because `cache_clear` also clears `enable`), then enable/disable.
- [ ] **P11-8 Real cache arrays.** Replace P3-5/P3-6's flat arrays with HR §12.4/§12.5's
      views onto the real cache. Address array read: `way = (CCR >> 6) & 3`,
      `entry = (addr & 0x3FC) >> 4`, returns `tag | (lru << 4) | (valid << 2)`. Write:
      **tag and valid come from the ADDRESS, not the value** —
      `tag = addr & 0x1FFFFC00`, `valid = (addr >> 2) & 1`, `lru = (val >> 4) & 0x3F`.
      HR §12.4 marks the entry-index expression `(addr & 0x3FC) >> 4` as **UNCLEAR** and
      inconsistent with the data array's `(addr >> 4) & 0x3F` — copy that note.
      Data array: `way = (addr >> 10) & 3`, `entry = (addr >> 4) & 0x3F`,
      byte offset `addr & 0xF`, big-endian assembly.
- [ ] **P11-9 Fetch path.** `step()`'s fetch goes through the cache for cacheable
      regions, matching HR §5.1's fetch table: BIOS (`0x000`), Low WRAM (`0x002`), CS0
      (`0x020`), High WRAM (`0x060`-`0x06F`) are cached; **VDP1 VRAM (`0x05C`) is never
      cached** (HR §5.1's `FetchVram`, with its `// Fighting Viper` comment). Everything
      else fetches `0xFFFF` → illegal instruction.

### A-7 — Architectural call-out: cache vs. the lock-striped `WorkRam`

`docs/mimas_emu_engineering_draft.md` §1.3 names "instruction caching bypassing the lock"
as mitigation #2 for High-WRAM contention and marks it not implemented. Real numbers: a
single instruction fetch today costs `bus_wait()` plus **two** `raw_read_byte` calls
(`sh2.rs:619-620`), each doing a full `translate()` and a full `RwLock::read()` on a
64 KB stripe (`shared_buffers.rs:99-104`). A cache hit would eliminate all of that for
the overwhelming majority of fetches. That is a genuinely large win and it is the main
reason to do Phase 11 at all.

**But Yabause's cache is single-threaded and Mimas's is not.** In the reference, nothing
can write memory behind the cache's back. In Mimas, High Work RAM is concurrently written
by Core 3 (VDP), Core 4 (M68K, via Sound RAM only), Core 6 (SCU DSP, via
`ScuDsp::step(&work_ram)`), `Sh2::execute_scu_dma` running on whichever core issued it,
and the *other* SH-2. A naive port produces a cache that silently serves stale
instructions and stale data.

Options, in increasing order of cost:

1. **Cache BIOS ROM only.** BIOS is immutable (`Arc<Vec<u8>>`, `sh2.rs:82`, writes
   discarded at `sh2.rs:593`), so no coherence problem exists. This captures most of the
   win *during boot*, which is the current objective, at essentially zero risk. **Start
   here.**
2. **Global write epoch.** A single `AtomicU64` in `WorkRam` bumped by every write path
   (`write_high_ram_byte`, `low_ram` writes, …); each cache line stores the epoch at fill
   time and is treated as invalid if the counter has moved. Correct but coarse — any
   write anywhere invalidates everything, which is close to no cache under load.
3. **Per-stripe write epochs.** One `AtomicU64` per 64 KB High-WRAM stripe, matching the
   existing 32-way striping exactly. A cache line records `(stripe, epoch)`. Precise
   enough to be useful, cheap to maintain (one relaxed increment per write, which
   `telemetry::record_wram_write` already does anyway at `shared_buffers.rs:107`), and it
   fits the striping design rather than fighting it.
4. **A real `SH2WriteNotify` equivalent** — writers actively invalidate matching lines in
   every core's cache. This is what the reference does (HR §11.8 mentions
   `SH2WriteNotify`), but it requires each writer to reach into every CPU's cache, which
   means either a shared `Mutex<Cache>` per core (contention on the hot path — a net
   loss) or lock-free line invalidation. Not worth it.

**Recommendation:** implement the full HR §12 model behind an off-by-default flag for
correctness testing, and enable it in `SaturnSystem` only for the BIOS region (option 1)
until option 3 is measured. Do not enable a coherence-unsafe cache on Work RAM to chase
a benchmark — a stale-instruction bug is far more expensive than the fetch it saves.

### Phase 11 testing

- [ ] **P11-T1 fill and hit.** Read a byte from a cacheable address, mutate the backing
      memory *directly* through `WorkRam` (bypassing the CPU), read again through the CPU
      and assert the **stale** cached value comes back. This is the test that proves a
      cache exists at all, and it must be written knowing it asserts staleness on purpose.
- [ ] **P11-T2 write-through, no write-allocate.** Write to an uncached line and assert
      memory changed but the line is still absent (probe via the address array). Write to
      a cached line and assert both the line and memory changed.
- [ ] **P11-T3 LRU replacement order.** Touch 5 distinct tags mapping to the same entry
      and assert the eviction order follows HR §12.3.3's `select_way_to_replace` table.
      Derive the expected sequence by hand-executing the four `update_lru` bit operations
      on paper, in the test comment.
- [ ] **P11-T4 two-way mode.** With `CCR & 0x08`, assert only ways 2 and 3 ever receive
      new lines, and that lines already resident in ways 0/1 are **still hit** (HR §12.3.3's
      closing note).
- [ ] **P11-T5 purge deviations.** A line resident in way 3 must **survive** an
      associative purge; an invalid line with a matching stale tag must **prevent** a
      valid lower-way line from being purged. Two tests, both named to make clear they
      pin reference bugs, both citing HR §12.3.1.
- [ ] **P11-T6 `cache_disable` preserves contents.** Disable, mutate memory behind the
      cache, re-enable, read → the stale line must reappear (HR §12.3.2).
- [ ] **P11-T7 address array from the address.** Write to the address array with a value
      whose bits would be a plausible tag, and assert the tag actually stored came from
      the **address**, not the value (HR §12.4). This is the single most surprising line
      in the whole cache implementation.

---

## Phase 12 — Storage-only peripherals: SCI, SBYCR, BSC refresh

Mostly folded into Phase 4. What remains here is the honesty pass.

- [ ] **P12-1.** Confirm nothing reads `WCR`, `MCR`, `BCR2`, `RTCSR`, `RTCNT` or `RTCOR`
      to change behaviour, and that `RTCNT` never increments — HR §11.9 is explicit that
      the reference inserts **no wait states** and **never counts the refresh counter**.
      Add a module-level comment saying so, so the absence reads as a decision.
- [ ] **P12-2.** Confirm `SBYCR`'s only functional use is that enabling the watchdog
      clears its bit 7 (HR §11.10) and that no standby/module-stop behaviour is emulated.
- [ ] **P12-3.** Confirm the SCI stays stubbed: `SCIReceiveByte` returns 0,
      `SCITransmitByte` does nothing (HR §11.2). The Saturn's SCI is only wired to the
      inter-CPU link on some hardware revisions; state that this is out of scope.
- [ ] **P12-4.** If a boot run's `[ONCHIP]` log (P4-11) ever shows the BIOS *reading*
      `RTCNT` in a loop expecting it to advance, this phase's status changes from
      "storage only" to a real blocker. Note the tell.

### Phase 12 testing

- [ ] **P12-T1.** Reset values and masks for all of these are already covered by P4-T1/
      P4-T2; add only the negative assertions: after advancing 100 000 cycles, `RTCNT` is
      unchanged; after writing `WCR`, memory access timing is unchanged.

---

## Phase 13 — User Break Controller (UBC)

HR §11.11: the matching logic exists only under `#ifdef SH2_UBC`, **which is not defined
in the build**, and channel B's registers have no read or write handler at all. Lowest
priority in the document.

- [ ] **P13-1.** Storage + masks for the paths that exist in the reference: `BARA` `0x140`
      raw 32-bit; `BAMRA` `0x144` raw 32-bit; `BBRA` `0x148` word `val & 0xFF`;
      `BRCR` `0x178` word `val & 0xF4DC`. HR notes there is **no read path** for any of
      these four.
- [ ] **P13-2.** `BARB`, `BAMRB`, `BBRB`, `BDRB`, `BDMRB` have no handler in the
      reference at all. Decide: match (log as unhandled) or implement plain storage.
      Recommended: plain storage plus a comment, since a write that vanishes is worse for
      a BIOS read-back than one that round-trips, and HR does not claim hardware discards
      them.
- [ ] **P13-3.** Bit constants from HR §11.11: `BBR` bits 7-6 CPU/peripheral select
      (`0`/`1<<6`/`2<<6`), bits 5-4 instruction/data select (`0`/`1<<4`/`2<<4`), bits 3-2
      read/write select (`0`/`1<<2`/`2<<2`), bits 1-0 size (`0`/`1`/`2`/`3`);
      `BRCR` `CMFCA` `1<<15`, `CMFPA` `1<<14`, `EBBA` `1<<13`, `UMD` `1<<12`,
      `PCBA` `1<<10`, `CMFCB` `1<<7`, `CMFPB` `1<<6`, `SEQ` `1<<4`, `DBEB` `1<<3`,
      `PCBB` `1<<2`.
- [ ] **P13-4.** Optional break matching: `BARx == (PC & !BAMRx)` for a break condition
      of `CPU | INST | READ`, **instruction fetch only** (data breakpoints are not
      implemented in the reference). Interrupt on match: vector 12, level 15, always —
      plus `BRCR |= flag` **regardless** of whether the interrupt was taken (HR §10.5).
      HR §11.11 states the semantics of `EBBA`, `UMD`, `SEQ` and `DBEB` are **not
      deducible from the source** — do not invent them.
- [ ] **P13-5.** If implemented at all, gate it behind an opt-in flag on `Sh2` (default
      off), the way the reference gates it behind `SH2_UBC`. A per-instruction address
      compare on the hot path for a debugger feature nobody has asked for is not a good
      trade.

### Phase 13 testing

- [ ] **P13-T1.** Register storage/masks (`BBRA → 0xFF`, `BRCR → 0xF4DC`), covered by the
      P4-T2 pattern.
- [ ] **P13-T2.** If P13-4 lands: `BARA = 0x0600_1000`, `BAMRA = 0`, `BBRA` set to
      `CPU|INST|READ`, execute at that address → `PC == [VBR + 48]`, `SR.I == 15`,
      `BRCR & CMFCA` set. Then repeat with `SR.I` already 15 and assert `BRCR & CMFCA` is
      **still** set even though no interrupt was taken (HR §10.5's "regardless").

---

## Phase 14 — Dual-core specifics

Gated on Core 1 actually executing real code, which SMPC `SSHON`
(`sh2.rs:827-832`, wired to `LockStepSync::set_thread_active(1, true)`) already enables.

- [ ] **P14-1.** `TAS.B` atomicity — already scheduled as P1-4 because it is a defect,
      not a feature. Re-verify here under a real two-core run.
- [ ] **P14-2 (D-19).** `BCR1` MASTER bit — already scheduled as P4-8.
- [ ] **P14-3 (D-19).** DIVU interrupt level: HR §10.6's DEVIATION says the reference
      reads it from `MSH2->onchip.IPRA` even on the slave, unlike every other source.
      P6-4 implements the per-CPU version; add a slave-side test and a comment naming the
      symptom that would indicate the quirk is load-bearing.
- [ ] **P14-4.** Input-capture write hooks: HR §Preamble names these as the second
      master/slave difference. The SSH2 variant's `depth < 4`-guarded "run the other CPU
      for 32 cycles" block is a synchronisation hack with no hardware counterpart
      (HR §11.3) — explicitly **do not** port it; `LockStepSync` is Mimas's answer to the
      same problem.
- [ ] **P14-5.** Slave reset/power-on. Core 1's `Sh2` is constructed and reset the same
      way as Core 0 (`lib.rs:163-174`). Confirm both read the same reset vector — on real
      hardware they do (`SH2PowerOn` reads `VBR+0`/`VBR+4` for both) and the slave then
      spins until the master hands it work.
- [ ] **P14-6.** Interrupt routing per core. The four SCU-side interrupts currently only
      reach Core 0 (`lib.rs:137-151` wires `sound_req_irq` and the VBLANK generator to
      Core 0 only). Once P5-5's shared queue exists, decide per source which core(s) it
      targets — that decision belongs to `docs/implementation-plans/scu.md`, since the SCU
      is the thing that routes them. Leave a pointer.

### Phase 14 testing

- [ ] **P14-T1.** Two real `Sh2` instances sharing one `WorkRam` and one `BusArbiter`,
      running a genuine `TAS.B` spinlock acquire/release around a shared counter
      incremented 100 000 times from each side. Final counter must be exactly 200 000.
      This is the test P1-T4's probabilistic version is a proxy for.
- [ ] **P14-T2.** `BCR1` differs between the two instances (already P4-T5, re-assert in
      a two-core context).
- [ ] **P14-T3.** With both cores running, a `LockStepSync` slack violation must still be
      bounded — reuse the existing `test_tier2_f1_lockstep_drift_limit` shape from
      `e2e-tests`.

---

## Appendix A — Architectural call-outs, collected

| ID | Constraint | Where it binds |
|---|---|---|
| **A-1** | New cross-thread state goes in as `Option<Arc<…>>` fields with setters, defaulted `None` in `Sh2::new()`. `Sh2::new()`'s 3-argument signature must not change — many `e2e-tests` and `saturn-core` tests depend on it (`CLAUDE.md` stability constraint). Precedents: `pc_reporter`, `m68k_control`, `sound_req_irq`, `speed`, `scu_dsp`. | P3-7, P5-5 |
| **A-2** | `SLEEP` parking must use `LockStepSync::park_while_inactive`'s **own** condvar, never the drift condvar — `sync.rs`'s doc comment records that merging them was measured as a real bug. And Core 0 cannot park while it is also the VBLANK source. | P2-1 |
| **A-3** | Mimas polls interrupts once per **instruction** (`sh2.rs:784`), not once per `Exec` batch (HR §7). This is a deliberate divergence and it is why HR §3.3's `LDC Rm,SR` interrupt re-check does not need porting. Do not "fix" it into batch polling. | P1-2, P5-2 |
| **A-4** | HR §7 advances `FRTExec`/`WDTExec`/`DMAProc` by the *requested* cycle count (HR §13 item 9). Mimas has no batch; drive them from `step()` by the *retired* count. More accurate, but a divergence — write it down. | P7-6, P10-4 |
| **A-5** | `run_loop`'s `batch_mask` sync heuristic (`sh2.rs:1716-1720`) assumes near-constant per-step cycle deltas and breaks under per-opcode costs. | P8-4 |
| **A-6** | The on-chip DMAC is a **third** DMA engine. It must take `BusArbiter::lock_for_dma()` and, inside it, use `raw_read_byte`/`raw_write_byte` — `read_byte`/`write_byte` call `bus_wait()` and self-deadlock. `execute_scu_dma` (`sh2.rs:1548-1631`) is the existing precedent. It does **not** get its own thread. | P9-8 |
| **A-7** | A cache in Mimas is not a cache in Yabause: High Work RAM has concurrent writers on other threads. Cache BIOS first (immutable, zero risk); anything more needs per-stripe write epochs matching `WorkRam`'s existing 32-way striping. See A-7's full discussion in Phase 11. | P11-* |
| **A-8** | `throttle.rs` needs no changes. Its accuracy is bounded by `Sh2::step()`'s flat cycle charge, which Phase 8 fixes; only its doc comment (`throttle.rs:29-40`) needs updating afterwards. | P8-5 |
| **A-9** | Where Mimas must diverge from the reference because Rust semantics differ (the DIVU division panic, the `MAX_INTERRUPTS` overrun), the divergence is *forced*, not a hardware judgement. Label those comments differently from the ones recording a deliberate fidelity choice. | P5-1, P6-1 |

## Appendix B — Test inventory and breakage register

### Existing SH-2 tests (35 in `sh2.rs`, ~14 in `e2e-tests/src/lib.rs`)

`sh2.rs` unit tests worth knowing about before touching anything:
`mov_imm_rn`, `add_imm`, `add_reg_reg`, `sub_reg_reg`, `cmp_eq_sets_t`, `and_or_xor_reg`,
`shift_ops`, `bra_takes_delay_slot_then_jumps`, `bsr_and_rts_roundtrip`,
`bt_bf_no_delay_slot`, `bf_s_matches_real_bios_wait_loop`,
`mov_l_load_store_register_indirect`, `reset_reads_vector_from_bios`,
`lds_l_pop_pr_then_rts`, `smpc_status_moved_off_high_ram_start`, `smpc_sf_reads_idle`,
`intback_populates_real_status_and_fires_system_manager_irq`,
`intback_requesting_peripheral_data_sets_sr_bit5`,
`sndon_sndoff_flip_the_m68k_control_flag`,
`sound_req_irq_enters_through_its_own_vector_at_level_9`,
`sound_ram_is_real_readwrite_memory`, `vblank_interrupt_masked_stays_pending`,
`vblank_interrupt_enters_and_returns`, `vblank_out_interrupt_masked_stays_pending`,
`vblank_out_interrupt_enters_and_returns`, `vblank_in_outranks_vblank_out_when_both_pending`,
`tvstat_vblank_bit_reflects_real_frame_timing`,
`tvstat_byte_split_matches_real_bios_access_pattern`,
`div1_single_step_matches_hand_traced_algorithm`, `div0s_seeds_qm_for_div1`,
`trapa_pushes_sr_then_pc_and_jumps_through_vbr`,
`peripheral_regions_are_real_readwrite_memory`, `smpc_register_window_mirrors_every_512kb`,
`test_onchip_division`, `test_scu_dma_direct`, `test_cdrom_handshake`.

Note that `and_or_xor_reg` tests the **register** forms (`0x2009`/`0x200A`/`0x200B`),
which are correct — it does not cover the swapped immediate forms. That is precisely how
D-1 survived.

### Known breakage register

| Change | Breaks | Fix in the same commit |
|---|---|---|
| P3-1 illegal-instruction exception | `e2e-tests::test_tier1_f3_sh2_pc_increment` (steps opcode `0x0000`, asserts `pc += 2`) | switch the fixture to `NOP` (`0x0009`) |
| P3-1 | `e2e-tests::test_tier2_f3_sh2_max_pc_overflow` (steps `0x0000` from `0xFFFF_FFFF`, asserts `pc == 1`) | switch to `NOP`, keep the wrap assertion |
| P8-1 per-opcode cycles | `e2e-tests::test_tier3_combination_f1_f3_lockstep_cpu_stepping` (asserts `cycles == 2` after one step) | rewrite around a `NOP` and its documented 1-cycle cost |
| P1-9 unaligned access performs the access | `e2e-tests::test_tier2_f3_sh2_unaligned_memory_access` asserts only the flag, so it should survive — verify | — |
| P4-2 moving DIVU fields into `OnChip` | any test touching `cpu.dvsr` etc. directly — grep before moving; `test_onchip_division` goes through `read_long`/`write_long` and is safe | — |
| P5-3 interrupt migration | the six interrupt tests must pass **unmodified**; if not, behaviour changed | investigate, don't edit the test |

`cargo test --workspace` must be green after every commit, not just at the end of a phase
(`CLAUDE.md`).

### Testing method reminders (`CLAUDE.md`)

Never assert a value the implementation produced. The four legitimate derivation methods,
in rough order of strength:

1. **Real BIOS bytes.** Dump a region, disassemble with `python3 tools/sh2dis.py <dump>
   <base>`, pin a real (address, opcode, operand, expected outcome) tuple. This is what
   produced `bf_s_matches_real_bios_wait_loop` and it is the strongest evidence available.
   Note `tools/sh2dis.py` is kept in sync with `sh2.rs`'s opcode table **by hand** —
   every opcode added in Phase 2 must be added there too, or the disassembler starts
   lying in exactly the situation you most need it.
2. **Hand-tracing, written into the test comment.** `div1_single_step_matches_hand_traced_algorithm`
   (`sh2.rs:2219`) is the model: the trace is in the comment, step by step, so a reader
   can check the arithmetic without re-running anything.
3. **A throwaway script.** For anything with awkward arithmetic (MAC saturation bounds,
   DIVU overflow triggers, LRU sequences), compute the expected value in Python and paste
   it with the script snippet in the comment.
4. **A value transcribed from `docs/hardware-reference/sh2-cpu.md`,** which itself carries
   a `yabause/src/<file>:<line>` citation. Weakest of the four (it is one document
   removed from the source) but perfectly legitimate for reset values and bit masks —
   just cite the HR section so the chain is auditable.

## Appendix C — Index: hardware-reference section → phase

| HR section | Topic | Phase(s) |
|---|---|---|
| §1.2 | memory wait-state table | 8 |
| §1.3 | on-chip register offsets | 4 |
| §2 | register file | 1 (reset) |
| §3 | SR layout, flag writers, `0x3F3` mask, `LDC` re-check | 1, 2 (S bit) |
| §4 | reset and power-on | 1, 4 (BCR1) |
| §5.1 | fetch table / cacheable regions | 11 |
| §5.2 | `EXEC_FROM_CACHE` | 3, 11 |
| §5.3 | dispatch, don't-care nibbles | 1 |
| §6 | address-space decode, purge/array regions | 3, 11 |
| §7 | exec loop, cycle carry, peripheral advance | 7, 8, 10 |
| §8 | delay-slot semantics | 1 |
| §9.1-9.3 | data transfer | — (complete) |
| §9.4 | arithmetic, `DIV1`, `MAC.L`, `MAC.W` | 1 (DIV1 chain), 2 (MAC) |
| §9.5 | logic, GBR byte forms, `TAS.B` | 1, 2 |
| §9.6 | shift/rotate | — (complete) |
| §9.7 | branch, `SLEEP` | 2 |
| §9.8 | LDC/LDS/STC/STS | 1 |
| §9.9 | `TRAPA`, illegal instruction | 3 |
| §9.10 | illegal encodings | 3 |
| §10.1-10.3 | interrupt queue, priority, delivery | 5 |
| §10.4 | NMI | 5 |
| §10.5 | UBC interrupt | 13 |
| §10.6 | interrupt source table | 5, 6, 7, 9, 10 |
| §11.1 | on-chip register map + reset values | 4 |
| §11.2 | SCI | 4, 12 |
| §11.3 | FRT | 7 |
| §11.4 | INTC | 4 |
| §11.5 | WDT | 10 |
| §11.6 | DIVU | 6 |
| §11.7 | CCR | 4, 11 |
| §11.8 | DMAC | 9 |
| §11.9 | BSC | 4, 12 |
| §11.10 | SBYCR | 4, 12 |
| §11.11 | UBC | 13 |
| §12.1-12.3 | cache geometry, read/write/purge/LRU | 11 |
| §12.4-12.5 | cache address/data arrays | 3, 11 |
| §12.6 | save states | n/a (Mimas has none) |
| §13 | deviation summary | see Appendix D |

## Appendix D — HR §13's 20 deviations: Mimas's disposition

| # | HR §13 deviation | Mimas disposition | Phase |
|---|---|---|---|
| — | 7 cycle-count irregularities | **Do not reproduce** — transcription slips with no hardware meaning; cannot change program results, only pacing | 8 |
| 1 | `MAC.W` with `S == 0` overwrites `MACH` | **Reproduce** — changes results, real code was validated against it | 2 |
| 2 | `MAC.W` with `S == 1` sets `MACH \|= 1` | **Reproduce** | 2 |
| 3 | Illegal instruction always uses vector 4 | **Reproduce** (HR: the delay-slot vector-6 case is the reference's own `// Fix me`) | 3 |
| 4 | `SLEEP` doesn't advance PC, is a 3-cycle spin | **Reproduce** in `execute()`; add parking in `run_loop` | 2 |
| 5 | Watchdog mode generates no reset | **Reproduce** (no reset path exists in Mimas either) | 10 |
| 6 | DIVU instantaneous, no cycles, three `// fix me` values | **Reproduce**, copying the `fix me` notes verbatim | 6 |
| 7 | DIVU level always from `MSH2->IPRA` | **Diverge** — use per-CPU IPRA, comment the quirk and its symptom | 6, 14 |
| 8 | FRT OCIB shares OCIA's vector field | **Reproduce**, with HR's "not deducible" note | 7 |
| 9 | Peripherals advance by requested, not retired, cycles | **Diverge** — Mimas has no batch; drive from `step()` by retired cycles | 7, 10 |
| 10 | `SH2SendInterrupt` has no bounds check, no level upgrade | **Split**: reproduce the no-upgrade semantics; **diverge** on the overrun (a Rust port must not reproduce a buffer overrun) | 5 |
| 11 | Associative purge skips way 3, ignores valid | **Reproduce**, loudly marked | 11 |
| 12 | Cache address-array indexing inconsistent with data array | **Reproduce**, with HR's UNCLEAR note | 11 |
| 13 | BSC/refresh registers are storage only | **Reproduce** | 4, 12 |
| 14 | `SBYCR` has no read path | **Diverge** — add a read path; more likely a Yabause omission than hardware | 4 |
| 15 | UBC channel B has no handlers; matching compiled out | **Diverge** on storage (add it); **reproduce** on matching (opt-in, default off) | 13 |
| 16 | SCI fully stubbed | **Reproduce** | 4, 12 |
| 17 | DMA bypasses the cache in both directions | **Reproduce** | 9, 11 |
| 18 | `SH2Reset` doesn't reset R15, doesn't set PC | **Reproduce** (R15 comes from the vector immediately after) | 1 |
| 19 | `SH2_struct.delay` is dead | n/a — no equivalent field | — |
| 20 | PC-relative in a delay slot resolves from `target - 2` | **Decision required** — recommended: reproduce, per `CLAUDE.md`'s "the exact behaviour real BIOS/game code was tested against" | 1 |

**Explicitly not deducible from the reference — do not guess** (HR §13's closing
paragraph): the true hardware `DVDNTH` after a divide-by-zero or overflow; the hardware
behaviour of a PC-relative load in a delay slot; whether OCIB has its own vector field;
the semantics of UBC `EBBA`/`UMD`/`SEQ`/`DBEB`; the real illegal-instruction vector
selection between 4 and 6; and any actual bus wait-state timing. Where the plan above
takes a position on one of these, it says so and says why.

---

## Immediate next actions

1. **Seed `.development/current_bugs.md`** with D-1 through D-20 (all four tracking docs
   are currently zero bytes).
2. **Rewrite `.development/current_blocker.md`** to name D-1 (`OR`/`XOR #imm` swapped)
   as the current wall — it is the highest-severity, lowest-effort defect found, and it
   silently corrupts flag manipulation in arbitrary BIOS code.
3. **Do Phase 1.** It is ten small edits, needs no new state, changes no API, and closes
   four correctness bugs and two long-standing open questions (P1-6, P1-T7).
4. Re-run a real BIOS boot (`MIMAS_BOOT_WATCH_SECS=280 ./target/release/saturn-frontend-native
   --bios <real-bios.bin>`) after Phase 1 and again after Phase 3, and diff the
   `[REGACCESS]` / new `[ILLOP]` / new `[ONCHIP]` output. That diff — not this document —
   determines whether Phase 4 or Phase 5 comes next in practice.
