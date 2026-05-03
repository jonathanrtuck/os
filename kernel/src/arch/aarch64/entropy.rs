//! Kernel entropy — RNDR with timer jitter fallback.
//!
//! Detects FEAT_RNG at boot. When available, [`random_u64`] uses the hardware
//! RNG (fast, cryptographic quality). Otherwise, it collects timer jitter
//! from the physical counter — slower but sufficient for KASLR seeds and
//! early-boot randomness.

use core::sync::atomic::{AtomicBool, Ordering};

use super::sysreg;

static HAS_RNDR: AtomicBool = AtomicBool::new(false);

/// Detect FEAT_RNG and cache the result.
pub fn init() {
    let rndr_field = (sysreg::id_aa64isar0_el1() >> 60) & 0xF;

    HAS_RNDR.store(rndr_field != 0, Ordering::Relaxed);
}

/// Return a random 64-bit value.
///
/// Uses RNDR when FEAT_RNG is present (with retry on transient failure).
/// Falls back to timer jitter otherwise.
pub fn random_u64() -> u64 {
    if HAS_RNDR.load(Ordering::Relaxed) {
        // RNDR can transiently fail (entropy exhausted). Retry a few times.
        for _ in 0..16 {
            if let Some(val) = sysreg::rndr() {
                return val;
            }
        }
    }
    jitter_u64()
}

/// Collect entropy from execution timing jitter.
///
/// Runs a memory workload (cache/TLB-exercising writes) between counter
/// reads and accumulates the timing deviation across 256 iterations. The
/// variation comes from microarchitectural non-determinism: cache state,
/// TLB state, pipeline timing, branch prediction, and under a hypervisor,
/// VM exit latency.
///
/// Based on the jitterentropy technique (Stephan Mueller, 2014). The
/// memory workload is significantly stronger than counter-only reads,
/// where variation is limited to VM-exit latency (often only 1–2 ticks).
///
/// The accumulated jitter is mixed through a SplitMix64 finalizer to
/// distribute entropy across all 64 bits.
fn jitter_u64() -> u64 {
    // Scratch buffer for memory workload. Writes create variable cache/TLB
    // latency — genuine nondeterminism even under a hypervisor.
    let mut scratch = [0u8; 64];
    let mut state: u64 = sysreg::cntpct_el0();
    let mut last_delta: u64 = 0;
    let mut last = state;

    for _ in 0..256 {
        // Memory workload: each byte depends on the accumulator state,
        // preventing the compiler from optimizing away the writes. Variable
        // cache/TLB latency from the access pattern is the primary entropy
        // source.
        for byte in scratch.iter_mut() {
            *byte = byte.wrapping_add(state as u8);
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(*byte as u64);
        }

        let now = sysreg::cntpct_el0();
        let delta = now.wrapping_sub(last);
        // XOR consecutive deltas: the first derivative is roughly constant
        // (counter frequency), so the XOR captures the deviation (jitter).
        let jitter = delta ^ last_delta;

        state = state.rotate_left(1) ^ jitter;

        last_delta = delta;
        last = now;
    }

    // SplitMix64 finalizer — ensures good distribution even if the
    // accumulated state has entropy concentrated in a few bits.
    state = (state ^ (state >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    state = (state ^ (state >> 27)).wrapping_mul(0x94d049bb133111eb);
    state ^ (state >> 31)
}
