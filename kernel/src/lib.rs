//! Microkernel.
//!
//! Five kernel objects (VMO, Channel, Event, Thread, Address Space),
//! 25 syscalls, capability-based access control.
//!
//! See `design/research/kernel-userspace-interface.md` for the full spec.
//!
//! ## Framekernel discipline
//!
//! All `unsafe` is confined to the `frame` module. The `deny(unsafe_code)`
//! lint enforces this at compile time — any `unsafe` outside `frame/` is a
//! build error.

#![no_std]
#![deny(unsafe_code)]

extern crate alloc;

#[cfg(test)]
#[macro_use]
extern crate std;

pub mod address_space;
pub mod config;
#[allow(unsafe_code)]
pub mod frame;
pub mod handle;
#[cfg(any(target_os = "none", test))]
pub mod print;
pub mod table;
pub mod types;
pub mod vmo;
