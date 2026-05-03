//! Microkernel.
//!
//! Five kernel objects (VMO, Channel, Event, Thread, Address Space),
//! 25 syscalls, capability-based access control.
//!
//! See `design/research/kernel-userspace-interface.md` for the full spec.

#![no_std]

#[cfg(any(target_os = "none", test))]
pub mod arch;
pub mod config;
pub mod firmware;
#[cfg(any(target_os = "none", test))]
pub mod print;
