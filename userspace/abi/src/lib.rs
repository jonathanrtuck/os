//! Kernel ABI — typed wrappers for all kernel syscalls.
//!
//! `#![no_std]`, zero dependencies. Provides safe Rust wrappers over the
//! raw SVC interface. Each wrapper validates arguments at the type level
//! and returns structured results.

#![no_std]

pub mod event;
pub mod handle;
pub mod ipc;
mod raw;
pub mod space;
pub mod system;
pub mod thread;
pub mod types;
pub mod vmo;
