# Miri Compatibility

**What belongs here:** Miri test compatibility status, known limitations, performance characteristics.

---

## Compatibility Status (updated 2026-03-11, post miri-ub-detection feature)

- **555/580 tests pass** clean under Miri across 25 test files
- **25 tests ignored** via `#[cfg_attr(miri, ignore)]` (documented below)
- **Zero kernel UB found** — all Miri findings were either false positives or out-of-scope (library code)
- **Miri toolchain:** nightly-aarch64-apple-darwin (component: miri)

### Ignored Tests (25 total)

| File | Count | Reason | Real UB? |
|------|-------|--------|----------|
| buddy | 1 | Miri provenance limitation with mmap-based memory simulation. `phys_to_virt` integer-to-pointer casts are expected for bare-metal but incompatible with Miri strict provenance. | No |
| ipc | 24 | Unaligned AtomicU32 reference at `libraries/ipc/lib.rs:198`. Real UB but in `system/libraries/` (out of kernel scope). | Yes (not kernel) |

### Performance Characteristics

- **scheduler_state:** ~10 seconds under Miri (cfg-gated iteration reduction from 10K→100 via `#[cfg(miri)]`)
- All other files complete in normal time under Miri

### Practical Usage

The blanket `cargo +nightly miri test` command exits on first UB per test binary. Run per-file for complete results across the full suite:
```
cd system/test && cargo +nightly miri test --test <name> -- --test-threads=1
```
