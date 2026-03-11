# Miri Compatibility

**What belongs here:** Miri test compatibility status, known limitations, performance characteristics.

---

## Compatibility Status (assessed 2026-03-11)

- **330/348 tests pass** clean under Miri across 15 of 18 test files
- **Miri toolchain:** nightly-aarch64-apple-darwin (component: miri)

### Passing Files (330 tests)

asid(7), channel(18), device_tree(15), drawing(83), eevdf(28), executable(17), futex(11), handle(32), heap(1), heap_routing(2), sched_context(17), scheduler_state(20), slab(22), vma(21), waitable(20), virtqueue(8)

### Failing Files

| File | Tests | Issue | Real UB? |
|------|-------|-------|----------|
| buddy | 2 | Miri provenance limitation with mmap-based memory simulation. `phys_to_virt` integer-to-pointer casts are expected for bare-metal but incompatible with Miri strict provenance. | No |
| ipc | 24 | Unaligned AtomicU32 reference at `libraries/ipc/lib.rs:198`. Real UB but in `system/libraries/` (out of kernel scope). | Yes (not kernel) |

### Performance Characteristics

- **scheduler_state:** ~1400 seconds under Miri (vs <1s normally) due to Miri interpretation overhead on randomized state machine iterations
- **Recommendation:** Reduce iteration count under `#[cfg(miri)]` for practical CI use

### Practical Usage

The blanket `cargo +nightly miri test` command exits on first failure (buddy). Run per-file for complete results:
```
cd system/test && cargo +nightly miri test --test <name> -- --test-threads=1
```
