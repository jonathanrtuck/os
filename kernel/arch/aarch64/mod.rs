//! AArch64 architecture implementation.
//!
//! Reference implementation for the arch interface. Everything in this module
//! is ARM64-specific: system registers, page table descriptors, GICv3,
//! generic timer, PL011 UART, PSCI power management.
//!
//! Platform-specific details (GIC base addresses, UART base, RAM geometry)
//! are co-located here for now. Extract to a `platform::` module when a
//! second board target arrives.

pub mod context;
pub mod cpu;
pub mod entropy;
pub mod interrupt_controller;
pub mod interrupts;
pub mod memory_mapped_io;
pub mod mmu;
pub mod per_core;
pub mod power;
pub mod scheduler;
pub mod security;
pub mod serial;
pub mod timer;
