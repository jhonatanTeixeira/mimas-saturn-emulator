//! Backend selection, picked at compile time: each target architecture gets its own module
//! implementing the same interface, `Compiler::compile(start: u32, fetch: &mut dyn
//! FnMut(u32) -> u16, trace: bool) -> CompiledBlock`. `Sh2Jit` (`jit/mod.rs`, the block
//! cache and dispatcher) only ever calls that interface and only ever touches
//! `CompiledBlock` as an opaque `buf`/`entry`/`start`/`end`/`insns` bundle — it is why
//! plugging in a second backend costs zero lines there, on purpose. This is a strategy
//! pattern resolved by `cfg(target_arch)`, not a trait object: only one backend is ever
//! compiled into a given binary, so there is nothing to dispatch at runtime, and a `Box<dyn
//! _>` here would just tax the hottest path in the emulator for a choice that is already
//! fixed at build time.
//!
//! **What is not yet shared across backends, and why:** the block-walking loop — decode,
//! cost accounting, `MAX_BLOCK_INSNS`, deciding where a block ends — lives inside `x64.rs`,
//! interleaved with code emission rather than factored out into an arch-independent
//! "plan the block, then emit it" step. That interleaving is not laziness: the branch
//! emitters compute a jump target and stash it in a register **before** emitting the delay
//! slot's code, because the delay slot runs on the pre-branch register state and can
//! overwrite the register the target came from (see the note on `emit_branch` in `x64.rs`).
//! A shared planner would need to preserve that ordering as data, not just the instruction
//! list, and getting that wrong would be a silent, hard-to-trace-check-catch correctness
//! bug, not a build error. Splitting it out safely is real work for whoever writes the
//! second backend, not assumed here.
//!
//! Everything else here is free to reuse without touching `x64.rs` at all:
//! `crate::cpu::decode` (opcode → `Insn`), `crate::cpu::state` (`Sh2State` layout and
//! `reg_offset`), and `crate::cpu::sh2_bus` (the `Sh2Bus`/`Sh2Runtime` contract every
//! backend's runtime helpers call into) are already architecture-independent.

#[cfg(target_arch = "x86_64")]
mod x64;
#[cfg(target_arch = "x86_64")]
pub use x64::{CompiledBlock, Compiler, MAX_BLOCK_INSNS};

#[cfg(not(target_arch = "x86_64"))]
compile_error!(
    "no SH-2 JIT backend for this target yet. AGENTS.md rule 2 is JIT-only, no interpreter \
     fallback, so this is a real gap for non-x86-64 targets (the weak handhelds this \
     project's README names as the point of the whole exercise are almost all ARM64). A new \
     backend needs a module here implementing Compiler::compile(start: u32, fetch: &mut dyn \
     FnMut(u32) -> u16, trace: bool) -> CompiledBlock — see src/cpu/jit/backend/x64.rs for \
     the interface and its module doc comment for the one piece of control flow (delay-slot \
     ordering) that is semantics, not x86-64 syntax, and has to be preserved."
);
