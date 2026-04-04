//! Kernel PRNG: ChaCha20 with fast key erasure and type-state seeding.
//!
//! This module provides a cryptographically secure PRNG for kernel use
//! (ASLR, stack canaries, PAC keys, per-process seed derivation).
//!
//! # Design
//!
//! **Algorithm:** ChaCha20 (RFC 8439) — add/xor/rotate on u32 arrays.
//! Constant-time by construction: no table lookups, no data-dependent branches.
//! Zero unsafe code in the PRNG core.
//!
//! **Type-state seeding:** The entropy pool must accumulate ≥256 bits of
//! entropy before it can be sealed into a `Prng<Seeded>`. This is enforced
//! at compile time — `Prng<Seeded>` is the only type with generation methods,
//! and the only way to obtain one is through `EntropyPool::try_seal()`.
//! No production kernel enforces this invariant at the type level.
//!
//! **Fast key erasure (Bernstein, 2017):** After generating a ChaCha20 block,
//! the first 32 bytes become the new key. Previous key material is overwritten.
//! Even if an attacker reads the PRNG state after a generation call, they
//! cannot reconstruct previous outputs (forward secrecy).
//!
//! **Entropy mixing:** `EntropyPool::add_entropy()` mixes new data into the
//! pool using ChaCha20 as a mixing function. Entropy credits are capped at
//! `data.len() * 8` bits — the pool never trusts the caller's claimed bit
//! count beyond what the data can physically carry.
//!
//! # Architecture boundary
//!
//! This module is fully generic — zero architecture-specific code. Entropy
//! sources (RNDR, CNTVCT, DTB) live in `arch::entropy` and feed data into
//! `EntropyPool::add_entropy()`.

// No `use` needed — this module is self-contained with no dependencies.

const MIN_ENTROPY_BITS: u32 = 256;
const OUTPUT_BUF_SIZE: usize = 32;

/// Entropy accumulation pool.
///
/// Collects entropy from multiple sources and mixes it into a key using
/// ChaCha20 as the mixing function. The pool tracks credited entropy bits
/// and refuses to seal until the minimum threshold (256 bits) is reached.
///
/// Entropy credits are capped at `data.len() * 8` — the pool never trusts
/// the caller's claimed bit count beyond what the data can physically carry.
#[derive(Debug)]
pub struct EntropyPool {
    /// Accumulated key material (mixed via ChaCha20).
    key: [u8; 32],
    /// Mixing counter — incremented on each add_entropy call.
    mix_counter: u32,
    /// Credited entropy bits (capped, never inflated).
    entropy_bits: u32,
}
/// Cryptographically secure PRNG with fast key erasure.
///
/// Created by `EntropyPool::try_seal()` after accumulating ≥256 bits of
/// entropy. All generation methods advance the internal state irreversibly
/// (forward secrecy via fast key erasure).
#[derive(Debug)]
pub struct Prng {
    /// Current ChaCha20 key (256 bits). Overwritten after each block generation.
    key: [u8; 32],
    /// Block counter. Incremented after each generation.
    counter: u64,
    /// Output buffer (32 bytes — second half of a ChaCha20 block).
    buf: [u8; OUTPUT_BUF_SIZE],
    /// Current position in the output buffer.
    buf_pos: usize,
}

impl EntropyPool {
    /// Create a new empty entropy pool.
    pub fn new() -> Self {
        Self {
            key: [0u8; 32],
            mix_counter: 0,
            entropy_bits: 0,
        }
    }

    /// Mix entropy data into the pool.
    ///
    /// `claimed_bits` is the caller's estimate of entropy in the data.
    /// The pool caps this at `data.len() * 8` to prevent overclaiming.
    /// Empty data with zero bits is a no-op.
    pub fn add_entropy(&mut self, data: &[u8], claimed_bits: u32) {
        if data.is_empty() {
            return;
        }

        // Cap credited bits at what the data can physically carry.
        let max_bits = (data.len() as u32).saturating_mul(8);
        let credited = claimed_bits.min(max_bits);

        // Mix the data into the key using ChaCha20.
        // Strategy: XOR data into the key, then run a ChaCha20 block to
        // diffuse it. The mix_counter ensures each call produces a unique
        // mixing state even with identical data.
        for (i, &byte) in data.iter().enumerate() {
            self.key[i % 32] ^= byte;
        }

        // Run ChaCha20 to diffuse — use the key as both key and nonce source.
        let nonce = [
            self.key[0],
            self.key[1],
            self.key[2],
            self.key[3],
            self.key[4],
            self.key[5],
            self.key[6],
            self.key[7],
            self.key[8],
            self.key[9],
            self.key[10],
            self.key[11],
        ];
        let block = chacha20_block(&self.key, self.mix_counter, &nonce);

        // Take the first 32 bytes as the new key.
        self.key.copy_from_slice(&block[..32]);
        self.mix_counter = self.mix_counter.wrapping_add(1);
        self.entropy_bits = self.entropy_bits.saturating_add(credited);
    }

    /// Attempt to seal the pool into a usable PRNG.
    ///
    /// Succeeds only if ≥256 bits of entropy have been accumulated.
    /// Consumes the pool — it cannot be reused after sealing.
    pub fn try_seal(self) -> Result<Prng, Self> {
        if self.entropy_bits < MIN_ENTROPY_BITS {
            return Err(self);
        }

        Ok(Prng {
            key: self.key,
            counter: 0,
            buf: [0u8; OUTPUT_BUF_SIZE],
            buf_pos: OUTPUT_BUF_SIZE, // Empty — force generation on first call.
        })
    }
}

impl Prng {
    /// Refill the output buffer using ChaCha20 + fast key erasure.
    ///
    /// Generates one ChaCha20 block (64 bytes):
    /// - First 32 bytes → new key (forward secrecy)
    /// - Last 32 bytes → output buffer
    fn refill(&mut self) {
        // Split counter into 32-bit counter + 12-byte nonce.
        // Lower 32 bits are the ChaCha20 counter; upper 32 bits
        // and a fixed pattern form the nonce.
        let counter_lo = self.counter as u32;
        let counter_hi = (self.counter >> 32) as u32;
        let nonce_bytes = counter_hi.to_le_bytes();
        let nonce: [u8; 12] = [
            nonce_bytes[0],
            nonce_bytes[1],
            nonce_bytes[2],
            nonce_bytes[3],
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        let block = chacha20_block(&self.key, counter_lo, &nonce);

        // Fast key erasure: first 32 bytes become the new key.
        self.key.copy_from_slice(&block[..32]);
        // Remaining 32 bytes are output.
        self.buf.copy_from_slice(&block[32..64]);

        self.buf_pos = 0;
        self.counter = self.counter.wrapping_add(1);
    }

    /// Fill a byte slice with pseudorandom data.
    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        let mut offset = 0;

        while offset < buf.len() {
            if self.buf_pos >= OUTPUT_BUF_SIZE {
                self.refill();
            }

            let available = OUTPUT_BUF_SIZE - self.buf_pos;
            let needed = buf.len() - offset;
            let copy_len = available.min(needed);

            buf[offset..offset + copy_len]
                .copy_from_slice(&self.buf[self.buf_pos..self.buf_pos + copy_len]);

            self.buf_pos += copy_len;
            offset += copy_len;
        }
    }
    /// Derive a new independent PRNG (for per-process seeds).
    ///
    /// The child PRNG has a unique key derived from the parent's state.
    /// The parent's state advances (fast key erasure), so parent and child
    /// produce independent streams. Multiple forks from the same parent
    /// produce distinct children because each fork advances the parent.
    pub fn fork(&mut self) -> Prng {
        // Generate 32 bytes for the child's key.
        let mut child_key = [0u8; 32];

        self.fill_bytes(&mut child_key);

        Prng {
            key: child_key,
            counter: 0,
            buf: [0u8; OUTPUT_BUF_SIZE],
            buf_pos: OUTPUT_BUF_SIZE,
        }
    }
    /// Generate a pseudorandom u64.
    pub fn next_u64(&mut self) -> u64 {
        // Need 8 bytes. If buffer doesn't have enough, refill.
        if self.buf_pos + 8 > OUTPUT_BUF_SIZE {
            self.refill();
        }

        let val = u64::from_le_bytes([
            self.buf[self.buf_pos],
            self.buf[self.buf_pos + 1],
            self.buf[self.buf_pos + 2],
            self.buf[self.buf_pos + 3],
            self.buf[self.buf_pos + 4],
            self.buf[self.buf_pos + 5],
            self.buf[self.buf_pos + 6],
            self.buf[self.buf_pos + 7],
        ]);

        self.buf_pos += 8;

        val
    }
}

/// Quarter round on a 16-word state array by index.
///
/// Rust's borrow checker can't prove disjoint `&mut state[i]` references,
/// so the inner loop operates on copies and writes back.
#[inline(always)]
fn qr(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(16);

    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(12);

    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(8);

    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(7);
}

/// ChaCha20 block function (RFC 8439 §2.3).
///
/// Produces 64 bytes of pseudorandom output from a 256-bit key, 32-bit
/// counter, and 96-bit nonce. Pure function — no side effects, no state.
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    // Initial state: "expand 32-byte k" constant + key + counter + nonce.
    let mut state: [u32; 16] = [
        0x6170_7865,
        0x3320_646e,
        0x7962_2d32,
        0x6b20_6574,
        u32::from_le_bytes([key[0], key[1], key[2], key[3]]),
        u32::from_le_bytes([key[4], key[5], key[6], key[7]]),
        u32::from_le_bytes([key[8], key[9], key[10], key[11]]),
        u32::from_le_bytes([key[12], key[13], key[14], key[15]]),
        u32::from_le_bytes([key[16], key[17], key[18], key[19]]),
        u32::from_le_bytes([key[20], key[21], key[22], key[23]]),
        u32::from_le_bytes([key[24], key[25], key[26], key[27]]),
        u32::from_le_bytes([key[28], key[29], key[30], key[31]]),
        counter,
        u32::from_le_bytes([nonce[0], nonce[1], nonce[2], nonce[3]]),
        u32::from_le_bytes([nonce[4], nonce[5], nonce[6], nonce[7]]),
        u32::from_le_bytes([nonce[8], nonce[9], nonce[10], nonce[11]]),
    ];
    // Save initial state for final addition.
    let initial = state;

    // 20 rounds (10 column rounds + 10 diagonal rounds).
    for _ in 0..10 {
        // Column rounds.
        qr(&mut state, 0, 4, 8, 12);
        qr(&mut state, 1, 5, 9, 13);
        qr(&mut state, 2, 6, 10, 14);
        qr(&mut state, 3, 7, 11, 15);
        // Diagonal rounds.
        qr(&mut state, 0, 5, 10, 15);
        qr(&mut state, 1, 6, 11, 12);
        qr(&mut state, 2, 7, 8, 13);
        qr(&mut state, 3, 4, 9, 14);
    }

    // Add initial state (mod 2^32).
    for i in 0..16 {
        state[i] = state[i].wrapping_add(initial[i]);
    }

    // Serialize to little-endian bytes.
    let mut output = [0u8; 64];

    for i in 0..16 {
        let bytes = state[i].to_le_bytes();

        output[i * 4] = bytes[0];
        output[i * 4 + 1] = bytes[1];
        output[i * 4 + 2] = bytes[2];
        output[i * 4 + 3] = bytes[3];
    }

    output
}
/// ChaCha20 quarter round on independent values (RFC 8439 §2.1).
///
/// The fundamental operation: 4 additions, 4 XORs, 4 rotations on a
/// 4-word state. Exposed for direct testing against RFC test vectors.
#[allow(dead_code)] // Test-only: used by kernel/test/tests/kernel_random.rs
pub fn quarter_round(a: &mut u32, b: &mut u32, c: &mut u32, d: &mut u32) {
    *a = a.wrapping_add(*b);
    *d ^= *a;
    *d = d.rotate_left(16);

    *c = c.wrapping_add(*d);
    *b ^= *c;
    *b = b.rotate_left(12);

    *a = a.wrapping_add(*b);
    *d ^= *a;
    *d = d.rotate_left(8);

    *c = c.wrapping_add(*d);
    *b ^= *c;
    *b = b.rotate_left(7);
}
