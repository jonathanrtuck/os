//! IPC framework — typed handles, message protocol, server/client
//! framework, and shared-memory data plane primitives.
//!
//! Built on the raw kernel ABI. Provides two communication planes:
//!
//! - **Control plane:** structured messages over synchronous call/reply
//!   (`message`, `server`, `client`)
//! - **Data plane:** lock-free shared memory (`ring` for discrete events,
//!   `register` for continuous state)
//!
//! The `kernel` feature (default) enables ABI-dependent modules. Without
//! it, only the pure data structures (`message`, `ring`, `register`) are
//! available — useful for host-target testing.

#![no_std]

pub mod message;
pub mod register;
pub mod ring;

#[cfg(feature = "kernel")]
pub mod client;
#[cfg(feature = "kernel")]
pub mod handle;
#[cfg(feature = "kernel")]
pub mod server;
