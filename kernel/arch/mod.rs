//! Architecture abstraction layer.
//!
//! The arch module is a compile-time driver: it translates hardware specifics
//! into kernel-internal abstractions. Selected by `#[cfg(target_arch)]`,
//! monomorphized at compile time, zero overhead.
//!
//! # What belongs in arch
//!
//! Anything that differs between ISAs (aarch64 vs x86_64 vs riscv64):
//! boot sequence, context save/restore, page tables, interrupt controller,
//! timer, per-core identity, serial console, power management.
//!
//! # What does NOT belong in arch
//!
//! - Device discovery (DTB/ACPI) — consumed by userspace, not the kernel core
//! - OS policy (VA layout, stack sizes, scheduling algorithm) — generic
//! - Platform/board specifics (GIC base addresses, UART model) — currently
//!   co-located with arch for simplicity; extract to `platform::` when a
//!   second board target arrives (v0.14)

#[cfg(target_arch = "aarch64")]
mod aarch64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
