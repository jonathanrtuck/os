//! Userspace syscall library — typed wrappers for all kernel syscalls.
//!
//! `#![no_std]`, zero dependencies. Provides safe Rust wrappers over the
//! raw SVC interface. Each wrapper validates arguments at the type level
//! and returns structured results.

#![no_std]

mod raw;
pub mod types;

pub mod event;
pub mod handle;
pub mod ipc;
pub mod space;
pub mod system;
pub mod thread;
pub mod vmo;
