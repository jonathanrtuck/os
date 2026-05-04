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
#[cfg(target_os = "none")]
pub mod bench;
pub mod bootstrap;
pub mod config;
pub mod endpoint;
pub mod event;
pub mod fault;
#[allow(unsafe_code)]
pub mod frame;
pub mod handle;
pub mod irq;
#[cfg(test)]
mod pipeline;
pub mod print;
pub mod syscall;
pub mod table;
pub mod thread;
pub mod types;
pub mod vmo;
