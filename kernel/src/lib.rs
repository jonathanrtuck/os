//! Microkernel.
//!
//! Five kernel objects (VMO, Endpoint, Event, Thread, Address Space),
//! 30 syscalls, capability-based access control.
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
#![deny(unused_must_use)]
#![deny(unreachable_patterns)]
#![deny(unused_unsafe)]

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
#[cfg(any(test, fuzzing, debug_assertions))]
pub mod invariants;
pub mod irq;
#[cfg(test)]
mod pipeline;
#[cfg(all(target_os = "none", debug_assertions))]
pub mod post;
pub mod print;
#[cfg(test)]
mod proptests;
pub mod sched;
pub mod syscall;
pub mod table;
pub mod thread;
pub mod types;
#[cfg(test)]
mod differential;
#[cfg(test)]
mod verification;
pub mod vmo;
