//! Framekernel core — the unsafe boundary.
//!
//! All `unsafe` code in the kernel lives inside this module tree. Everything
//! outside `frame` is safe Rust built against the abstractions exported here.
//! The crate-level `#![deny(unsafe_code)]` enforces this at compile time.

#[cfg(any(target_os = "none", test))]
pub mod arch;
#[cfg(any(target_os = "none", test))]
pub mod concurrent_table;
pub mod fault_resolve;
pub mod firmware;
#[cfg(target_os = "none")]
pub mod heap;
#[cfg(any(target_os = "none", test))]
pub mod lockdep;
pub mod profile;
pub mod ring;
pub mod slab;
#[cfg(any(target_os = "none", test))]
pub mod state;
pub mod user_mem;
