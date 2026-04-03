//! Host-side tests for the kernel PRNG module.
//!
//! Tests ChaCha20 block function against RFC 8439 test vectors, type-state
//! enforcement (Unseeded → Seeded transition), entropy accumulation, fast key
//! erasure (forward secrecy), and PRNG fork independence.
//!
//! The PRNG is pure computation — no arch stubs needed.

#[path = "../../random.rs"]
mod random;

use random::*;

// =========================================================================
// ChaCha20 block function — RFC 8439 §2.3.2 test vector
// =========================================================================

#[test]
fn chacha20_quarter_round() {
    // RFC 8439 §2.1.1 — quarter round on (a, b, c, d) = (0x11111111, ...)
    let mut a: u32 = 0x11111111;
    let mut b: u32 = 0x01020304;
    let mut c: u32 = 0x9b8d6f43;
    let mut d: u32 = 0x01234567;
    random::quarter_round(&mut a, &mut b, &mut c, &mut d);
    assert_eq!(a, 0xea2a92f4);
    assert_eq!(b, 0xcb1cf8ce);
    assert_eq!(c, 0x4581472e);
    assert_eq!(d, 0x5881c4bb);
}

#[test]
fn chacha20_block_rfc8439_test_vector() {
    // RFC 8439 §2.3.2 — full block with known key, nonce, counter.
    let key: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a,
        0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15,
        0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    ];
    let nonce: [u8; 12] = [
        0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00, 0x4a, 0x00, 0x00, 0x00,
        0x00,
    ];
    let counter: u32 = 1;

    let output = chacha20_block(&key, counter, &nonce);

    // Expected output from RFC 8439 §2.3.2:
    let expected: [u32; 16] = [
        0xe4e7f110, 0x15593bd1, 0x1fdd0f50, 0xc47120a3, 0xc7f4d1c7,
        0x0368c033, 0x9aaa2204, 0x4e6cd4c3, 0x466482d2, 0x09aa9f07,
        0x05d7c214, 0xa2028bd9, 0xd19c12b5, 0xb94e16de, 0xe883d0cb,
        0x4e3c50a2,
    ];

    // Convert output bytes to u32 little-endian for comparison.
    for i in 0..16 {
        let got = u32::from_le_bytes([
            output[i * 4],
            output[i * 4 + 1],
            output[i * 4 + 2],
            output[i * 4 + 3],
        ]);
        assert_eq!(
            got, expected[i],
            "word {} mismatch: got {:#010x}, expected {:#010x}",
            i, got, expected[i]
        );
    }
}

#[test]
fn chacha20_block_counter_zero() {
    // RFC 8439 §2.3.2 also specifies counter=1 output. Verify that
    // counter=0 produces different (but deterministic) output.
    let key = [0u8; 32];
    let nonce = [0u8; 12];

    let block0 = chacha20_block(&key, 0, &nonce);
    let block1 = chacha20_block(&key, 1, &nonce);

    // Different counters must produce different blocks.
    assert_ne!(block0, block1);
}

#[test]
fn chacha20_block_different_keys_produce_different_output() {
    let nonce = [0u8; 12];
    let mut key_a = [0u8; 32];
    let mut key_b = [0u8; 32];
    key_a[0] = 1;
    key_b[0] = 2;

    let out_a = chacha20_block(&key_a, 0, &nonce);
    let out_b = chacha20_block(&key_b, 0, &nonce);

    assert_ne!(out_a, out_b);
}

#[test]
fn chacha20_block_deterministic() {
    // Same inputs must always produce the same output.
    let key = [42u8; 32];
    let nonce = [7u8; 12];

    let out1 = chacha20_block(&key, 5, &nonce);
    let out2 = chacha20_block(&key, 5, &nonce);

    assert_eq!(out1, out2);
}

// =========================================================================
// Type-state: Unseeded → Seeded transition
// =========================================================================

#[test]
fn unseeded_prng_cannot_seal_without_entropy() {
    let pool = EntropyPool::new();
    assert!(
        pool.try_seal().is_err(),
        "must not seal with zero entropy"
    );
}

#[test]
fn unseeded_prng_cannot_seal_with_insufficient_entropy() {
    let mut pool = EntropyPool::new();
    // Add 128 bits — not enough (need 256).
    pool.add_entropy(&[0xAA; 16], 128);
    assert!(
        pool.try_seal().is_err(),
        "must not seal with only 128 bits"
    );
}

#[test]
fn unseeded_prng_seals_at_256_bits() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0xDE; 32], 256);
    assert!(
        pool.try_seal().is_ok(),
        "should seal with exactly 256 bits"
    );
}

#[test]
fn unseeded_prng_seals_above_256_bits() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0xAB; 16], 128);
    pool.add_entropy(&[0xCD; 16], 128);
    // 256 bits total from two sources.
    assert!(
        pool.try_seal().is_ok(),
        "should seal with 256 bits from multiple sources"
    );
}

#[test]
fn entropy_accumulates_across_add_calls() {
    let mut pool = EntropyPool::new();
    // Add entropy in small increments.
    for _ in 0..32 {
        pool.add_entropy(&[0xFF; 4], 8); // 8 bits each → 256 total
    }
    assert!(pool.try_seal().is_ok());
}

#[test]
fn entropy_pool_mixes_not_replaces() {
    // Two pools seeded with different entropy in different order
    // must produce different PRNGs.
    let mut pool_a = EntropyPool::new();
    pool_a.add_entropy(&[0x11; 32], 128);
    pool_a.add_entropy(&[0x22; 32], 128);

    let mut pool_b = EntropyPool::new();
    pool_b.add_entropy(&[0x22; 32], 128);
    pool_b.add_entropy(&[0x11; 32], 128);

    let mut prng_a = pool_a.try_seal().unwrap();
    let mut prng_b = pool_b.try_seal().unwrap();

    // Different mixing order → different output.
    assert_ne!(prng_a.next_u64(), prng_b.next_u64());
}

// =========================================================================
// Prng<Seeded> output correctness
// =========================================================================

#[test]
fn seeded_prng_produces_nonzero_output() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0x42; 32], 256);
    let mut prng = pool.try_seal().unwrap();

    // Generate several values — at least one should be nonzero.
    let mut saw_nonzero = false;
    for _ in 0..16 {
        if prng.next_u64() != 0 {
            saw_nonzero = true;
            break;
        }
    }
    assert!(saw_nonzero, "PRNG should produce nonzero output");
}

#[test]
fn seeded_prng_deterministic_from_same_seed() {
    let seed = [0x55; 32];

    let mut pool_a = EntropyPool::new();
    pool_a.add_entropy(&seed, 256);
    let mut prng_a = pool_a.try_seal().unwrap();

    let mut pool_b = EntropyPool::new();
    pool_b.add_entropy(&seed, 256);
    let mut prng_b = pool_b.try_seal().unwrap();

    for _ in 0..100 {
        assert_eq!(prng_a.next_u64(), prng_b.next_u64());
    }
}

#[test]
fn seeded_prng_different_seeds_diverge() {
    let mut pool_a = EntropyPool::new();
    pool_a.add_entropy(&[0xAA; 32], 256);
    let mut prng_a = pool_a.try_seal().unwrap();

    let mut pool_b = EntropyPool::new();
    pool_b.add_entropy(&[0xBB; 32], 256);
    let mut prng_b = pool_b.try_seal().unwrap();

    // At least one of the first 16 values should differ.
    let mut diverged = false;
    for _ in 0..16 {
        if prng_a.next_u64() != prng_b.next_u64() {
            diverged = true;
            break;
        }
    }
    assert!(diverged, "different seeds should produce different streams");
}

#[test]
fn seeded_prng_fill_bytes() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0x99; 32], 256);
    let mut prng = pool.try_seal().unwrap();

    let mut buf = [0u8; 64];
    prng.fill_bytes(&mut buf);

    // Buffer should not be all zeros.
    assert!(buf.iter().any(|&b| b != 0), "fill_bytes should produce non-zero data");
}

#[test]
fn seeded_prng_fill_bytes_deterministic() {
    let seed = [0x77; 32];

    let mut pool_a = EntropyPool::new();
    pool_a.add_entropy(&seed, 256);
    let mut prng_a = pool_a.try_seal().unwrap();

    let mut pool_b = EntropyPool::new();
    pool_b.add_entropy(&seed, 256);
    let mut prng_b = pool_b.try_seal().unwrap();

    let mut buf_a = [0u8; 128];
    let mut buf_b = [0u8; 128];
    prng_a.fill_bytes(&mut buf_a);
    prng_b.fill_bytes(&mut buf_b);

    assert_eq!(buf_a, buf_b);
}

#[test]
fn seeded_prng_fill_bytes_various_sizes() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0x33; 32], 256);
    let mut prng = pool.try_seal().unwrap();

    // Fill various sizes — should not panic.
    for size in [0, 1, 7, 8, 15, 16, 31, 32, 33, 63, 64, 65, 128, 256] {
        let mut buf = vec![0u8; size];
        prng.fill_bytes(&mut buf);
    }
}

// =========================================================================
// Fast key erasure (forward secrecy)
// =========================================================================

#[test]
fn fast_key_erasure_after_generate() {
    // After generating output, the PRNG's internal state has changed
    // such that previous outputs cannot be reconstructed from current state.
    // We verify this indirectly: generating N values, then generating N more
    // from a checkpoint, the two sequences must not overlap.
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0xEE; 32], 256);
    let mut prng = pool.try_seal().unwrap();

    // Generate 16 values.
    let first_batch: Vec<u64> = (0..16).map(|_| prng.next_u64()).collect();

    // Continue generating — none should repeat the first batch.
    // (With a 64-bit output space, collision probability is ~2^-60 for 16 values.)
    let second_batch: Vec<u64> = (0..16).map(|_| prng.next_u64()).collect();

    for val in &second_batch {
        assert!(
            !first_batch.contains(val),
            "PRNG repeated a value: {:#018x}",
            val
        );
    }
}

#[test]
fn prng_state_advances_on_each_call() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0xDD; 32], 256);
    let mut prng = pool.try_seal().unwrap();

    // Each call should return a different value.
    let mut prev = prng.next_u64();
    for _ in 0..100 {
        let next = prng.next_u64();
        assert_ne!(prev, next, "consecutive calls returned same value");
        prev = next;
    }
}

// =========================================================================
// Fork (per-process PRNG derivation)
// =========================================================================

#[test]
fn fork_produces_independent_stream() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0xCC; 32], 256);
    let mut parent = pool.try_seal().unwrap();

    let mut child = parent.fork();

    // Parent and child should produce different streams.
    let mut diverged = false;
    for _ in 0..16 {
        if parent.next_u64() != child.next_u64() {
            diverged = true;
            break;
        }
    }
    assert!(diverged, "forked PRNG must produce different stream");
}

#[test]
fn fork_does_not_affect_parent_determinism() {
    let seed = [0xBB; 32];

    // Path A: generate, fork, generate more.
    let mut pool_a = EntropyPool::new();
    pool_a.add_entropy(&seed, 256);
    let mut prng_a = pool_a.try_seal().unwrap();
    let pre_fork: Vec<u64> = (0..4).map(|_| prng_a.next_u64()).collect();
    let _child = prng_a.fork();
    let post_fork_a: Vec<u64> = (0..4).map(|_| prng_a.next_u64()).collect();

    // Path B: same seed, same operations.
    let mut pool_b = EntropyPool::new();
    pool_b.add_entropy(&seed, 256);
    let mut prng_b = pool_b.try_seal().unwrap();
    let pre_fork_b: Vec<u64> = (0..4).map(|_| prng_b.next_u64()).collect();
    let _child_b = prng_b.fork();
    let post_fork_b: Vec<u64> = (0..4).map(|_| prng_b.next_u64()).collect();

    assert_eq!(pre_fork, pre_fork_b);
    assert_eq!(post_fork_a, post_fork_b);
}

#[test]
fn multiple_forks_produce_distinct_children() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0xAA; 32], 256);
    let mut parent = pool.try_seal().unwrap();

    let mut child1 = parent.fork();
    let mut child2 = parent.fork();
    let mut child3 = parent.fork();

    let v1 = child1.next_u64();
    let v2 = child2.next_u64();
    let v3 = child3.next_u64();

    // All three children should produce different first values.
    assert_ne!(v1, v2);
    assert_ne!(v2, v3);
    assert_ne!(v1, v3);
}

// =========================================================================
// Edge cases
// =========================================================================

#[test]
fn entropy_pool_handles_empty_data() {
    let mut pool = EntropyPool::new();
    // Empty data with zero bits — should be a no-op, not panic.
    pool.add_entropy(&[], 0);
    assert!(pool.try_seal().is_err()); // Still unseeded.
}

#[test]
fn entropy_pool_caps_claimed_bits() {
    // Caller claims 256 bits from 1 byte of data — the pool should cap
    // entropy credit at data.len() * 8, not trust the caller blindly.
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0xFF], 256); // Claim 256, but only 8 bits of data.
    // Pool should not seal — it should cap at 8 bits credited.
    assert!(
        pool.try_seal().is_err(),
        "pool must not trust overclaimed entropy"
    );
}

#[test]
fn seeded_prng_generates_many_values_without_panic() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0x12; 32], 256);
    let mut prng = pool.try_seal().unwrap();

    // Generate 10,000 values — no panic, no stuck state.
    for _ in 0..10_000 {
        let _ = prng.next_u64();
    }
}

#[test]
fn fill_bytes_zero_length_is_noop() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0x34; 32], 256);
    let mut prng = pool.try_seal().unwrap();

    let mut buf = [];
    prng.fill_bytes(&mut buf); // Should not panic.
}

// =========================================================================
// Statistical quality (basic sanity — not a NIST test suite)
// =========================================================================

#[test]
fn output_distribution_not_obviously_biased() {
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0x56; 32], 256);
    let mut prng = pool.try_seal().unwrap();

    // Generate 1000 u64 values and check bit distribution.
    // Each bit position should be set roughly 50% of the time.
    let n = 1000;
    let mut bit_counts = [0u32; 64];
    for _ in 0..n {
        let val = prng.next_u64();
        for bit in 0..64 {
            if val & (1u64 << bit) != 0 {
                bit_counts[bit] += 1;
            }
        }
    }

    // Each bit should be set between 40% and 60% of the time.
    // (For n=1000, this is a very loose bound — a good PRNG will be much closer to 50%.)
    for (bit, &count) in bit_counts.iter().enumerate() {
        assert!(
            count >= 400 && count <= 600,
            "bit {} set {}/{} times — bias detected",
            bit,
            count,
            n
        );
    }
}

#[test]
fn output_passes_basic_runs_test() {
    // Simple runs test: count transitions (0→1 and 1→0) in a bit stream.
    // A good PRNG should have roughly n/2 transitions in n bits.
    let mut pool = EntropyPool::new();
    pool.add_entropy(&[0x78; 32], 256);
    let mut prng = pool.try_seal().unwrap();

    let n_values = 100;
    let mut prev_bit = false;
    let mut transitions = 0u32;
    let total_bits = n_values * 64;

    for _ in 0..n_values {
        let val = prng.next_u64();
        for bit in 0..64 {
            let current_bit = val & (1u64 << bit) != 0;
            if current_bit != prev_bit {
                transitions += 1;
            }
            prev_bit = current_bit;
        }
    }

    // Expect roughly total_bits/2 transitions. Allow ±20% of expected.
    // For n=6400 bits, expected=3200, std_dev≈40, so ±640 is ~16 sigma — very generous.
    let expected = total_bits / 2;
    let lower = expected * 80 / 100;
    let upper = expected * 120 / 100;
    assert!(
        transitions >= lower as u32 && transitions <= upper as u32,
        "runs test: {} transitions in {} bits (expected ~{})",
        transitions,
        total_bits,
        expected
    );
}
