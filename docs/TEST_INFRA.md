# E2E Test Infra: Mimas Sega Saturn Emulator

## Test Philosophy
- Opaque-box, requirement-driven. No dependency on implementation design.
- Methodology: Category-Partition + Boundary Value Analysis (BVA) + Pairwise Combinatorial Testing + Real-World Workload Testing.

## Feature Inventory
| # | Feature | Source (requirement) | Tier 1 | Tier 2 | Tier 3 |
|---|---------|---------------------|:------:|:------:|:------:|
| 1 | Core Distribution & Lockstep Sync | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ |
| 2 | Bus Arbitration (BusArbiter) | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ |
| 3 | SH-2 CPU Emulation | ORIGINAL_REQUEST §R2 | 5 | 5 | ✓ |
| 4 | Saturn Peripherals & SCU DSP | ORIGINAL_REQUEST §R2 | 5 | 5 | ✓ |
| 5 | CD-ROM (CS2) & CHD Streaming | ORIGINAL_REQUEST §R3 | 5 | 5 | ✓ |
| 6 | Frontend Loader & Target Loading | ORIGINAL_REQUEST §R4 | 5 | 5 | ✓ |

## Test Architecture
- **Test Harness/Runner**: Located in `mimas/e2e-tests/`. Invoked via `cargo test -p e2e-tests`.
- **Pass/Fail Semantics**: All test cases are standard Rust tests. Standard exit code 0 indicates success. Any panic or assertion failure indicates test failure.
- **Testing Entry Points**:
  - Emulation logic/components are tested using the public interface contracts defined in `PROJECT.md` (e.g. `BusArbiter`, `LockStepSync`, `Sh2`, `Cdrom`).
  - Overall system execution, boot process, and path validation are tested by running the standalone native CLI wrapper (`saturn-frontend-native`) as a subprocess, providing test arguments.

## Real-World Application Scenarios (Tier 4)
| # | Scenario | Features Exercised | Complexity |
|---|----------|--------------------|------------|
| 1 | BIOS Verification & Setup | F1, F3, F6 | Medium |
| 2 | Magic Knight Rayearth Boot Sequence | F3, F5, F6 | High |
| 3 | DMA-Heavy Graphic Stream | F1, F2, F4 | High |
| 4 | Continuous Audio-Video Emulation | F1, F4, F6 | High |
| 5 | Graceful System Termination & Recovery | F1, F6 | Medium |

## Coverage Thresholds
- Tier 1: 30 test cases (5 per feature)
- Tier 2: 30 test cases (5 per feature)
- Tier 3: 6 test cases (pairwise combination tests)
- Tier 4: 5 test cases (real-world application scenarios)
- **Total: 71 test cases**
