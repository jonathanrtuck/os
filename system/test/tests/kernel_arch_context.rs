//! Host-side tests for Context accessor methods.
//!
//! The arch extraction (v0.6 Phase 1) adds generic accessor methods to Context
//! (pc, sp, arg, set_user_mode, etc.). These tests verify the accessors map
//! correctly to the underlying aarch64 register fields.
//!
//! Context is pure data (no unsafe, no asm) — includable via #[path].

#[path = "../../kernel/arch/aarch64/context.rs"]
mod context;

use context::Context;

// --- Round-trip accessors ---

#[test]
fn pc_round_trip() {
    let mut ctx = Context::new();
    assert_eq!(ctx.pc(), 0);
    ctx.set_pc(0xDEAD_BEEF);
    assert_eq!(ctx.pc(), 0xDEAD_BEEF);
}

#[test]
fn sp_round_trip() {
    let mut ctx = Context::new();
    assert_eq!(ctx.sp(), 0);
    ctx.set_sp(0xFFFF_0000_1234_5678);
    assert_eq!(ctx.sp(), 0xFFFF_0000_1234_5678);
}

#[test]
fn user_sp_round_trip() {
    let mut ctx = Context::new();
    assert_eq!(ctx.user_sp(), 0);
    ctx.set_user_sp(0x0000_0000_7FFF_0000);
    assert_eq!(ctx.user_sp(), 0x0000_0000_7FFF_0000);
}

#[test]
fn user_tls_round_trip() {
    let mut ctx = Context::new();
    assert_eq!(ctx.user_tls(), 0);
    ctx.set_user_tls(0xABCD_1234);
    assert_eq!(ctx.user_tls(), 0xABCD_1234);
}

// --- Argument registers (x0-x5 on aarch64) ---

#[test]
fn arg_round_trip_all_six() {
    let mut ctx = Context::new();
    for i in 0..6 {
        ctx.set_arg(i, (i as u64 + 1) * 100);
    }
    for i in 0..6 {
        assert_eq!(ctx.arg(i), (i as u64 + 1) * 100, "arg({i}) mismatch");
    }
}

#[test]
fn arg_independence() {
    let mut ctx = Context::new();
    // Set only arg 3, verify others remain zero.
    ctx.set_arg(3, 0x42);
    for i in 0..6 {
        if i == 3 {
            assert_eq!(ctx.arg(i), 0x42);
        } else {
            assert_eq!(ctx.arg(i), 0, "arg({i}) should be 0");
        }
    }
}

// --- set_user_mode ---

#[test]
fn set_user_mode_clears_exception_level() {
    let mut ctx = Context::new();
    // Manually set SPSR to EL1h (0x5) to simulate kernel context.
    ctx.spsr = 0x5;
    ctx.set_user_mode();
    // EL0t = bits [3:0] == 0x0. Other bits (DAIF, NZCV) may be set.
    assert_eq!(ctx.spsr & 0xF, 0x0, "SPSR should be EL0t after set_user_mode");
}

#[test]
fn set_user_mode_from_zero() {
    let mut ctx = Context::new();
    ctx.set_user_mode();
    assert_eq!(ctx.spsr & 0xF, 0x0);
}

// --- new() zeroing ---

#[test]
fn new_context_is_zeroed() {
    let ctx = Context::new();
    assert_eq!(ctx.pc(), 0);
    assert_eq!(ctx.sp(), 0);
    assert_eq!(ctx.user_sp(), 0);
    assert_eq!(ctx.user_tls(), 0);
    for i in 0..6 {
        assert_eq!(ctx.arg(i), 0, "arg({i}) should be 0 in new context");
    }
    assert_eq!(ctx.spsr, 0);
}

// --- Compile-time offset assertions still hold ---
// (These are in context.rs itself via const assertions, but verify here that
// the struct size is what we expect — catches accidental field changes.)

#[test]
fn context_size_is_0x330() {
    assert_eq!(core::mem::size_of::<Context>(), 0x330);
}
