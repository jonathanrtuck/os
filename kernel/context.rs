//! Re-export of arch-specific Context.
//!
//! Context lives in arch/aarch64/context.rs — the register file is
//! architecture-defined. This module re-exports it so existing code
//! (`use super::context::Context`) continues to work without changing
//! every import path.

pub use super::arch::context::Context;
