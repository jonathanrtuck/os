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
// Pedantic safety lints — catch real bugs at compile time.
// Cast truncation warnings audited: this kernel targets only aarch64 (usize=u64).
// All u64→u32 casts are on values bounded by MAX_* constants (≤1024).
#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cognitive_complexity)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::use_self)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::unnested_or_patterns)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::inline_always)]
#![allow(clippy::similar_names)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::return_self_not_must_use)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::unused_self)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::ptr_as_ptr)]
#![allow(clippy::single_match_else)]
#![allow(clippy::ref_as_ptr)]
#![allow(clippy::borrow_as_ptr)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::unreadable_literal)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::explicit_iter_loop)]

extern crate alloc;

#[cfg(test)]
#[macro_use]
extern crate std;

pub mod address_space;
#[cfg(target_os = "none")]
pub mod bench;
#[cfg(any(target_os = "none", test))]
pub mod bootstrap;
pub mod config;
#[cfg(test)]
mod differential;
pub mod endpoint;
pub mod event;
#[cfg(any(target_os = "none", test))]
pub mod fault;
#[allow(unsafe_code)]
pub mod frame;
pub mod handle;
#[cfg(any(test, fuzzing, all(target_os = "none", debug_assertions)))]
pub mod invariants;
pub mod irq;
#[cfg(test)]
mod pipeline;
#[cfg(any(
    all(target_os = "none", debug_assertions),
    feature = "integration-tests"
))]
pub mod post;
pub mod print;
#[cfg(test)]
mod proptests;
#[cfg(any(target_os = "none", test))]
pub mod sched;
#[cfg(any(target_os = "none", test))]
pub mod syscall;
pub mod table;
pub mod thread;
pub mod types;
#[cfg(test)]
mod verification;
pub mod vmo;
