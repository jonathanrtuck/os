# Kernel Audit — CONVERGED

## Result
**19 bugs fixed. 9 performance improvements. 522 tests (was 472). Zero clippy warnings.**
**Convergence: 2 consecutive zero-finding passes across 7 review techniques.**

## All Bugs Fixed

1. `cargo run` crash — debug-mode stack overflow (Box IrqTable + opt-level=1)
2. ObjectTable: unnecessary atomic CAS loops replaced with plain fields
3. HandleTable: unnecessary atomic CAS loops replaced with plain fields
4. Event: unnecessary AtomicU64 replaced with plain u64
5. PSCI cpu_on: missing SMCCC register clobbers (x1-x17)
6. alloc_kernel_stack: non-contiguous pages (now uses alloc_contiguous)
7. VMO resize: heap allocation on hot path (callback pattern)
8. space_destroy: page table + ASID leak (added destroy_page_table call)
9. space_destroy: heap allocation for handle cleanup (fixed stack array)
10. Secondary core entry: missing EL2→EL1 transition
11. sys_thread_create: scheduler enqueue before handle alloc (dangling ThreadId)
12. sys_thread_create_in: same scheduler ordering bug
13. sys_reply: handle install loop could leak handles on mid-loop failure
14. sys_recv: returned PeerClosed on spurious wakeup (now checks is_peer_closed)
15. Endpoint: reply cap ID wraparound could alias active caps (collision skip)
16. sys_thread_create: kernel stack pages leaked on handle-table-full
17. sys_thread_create_in: missing kernel stack allocation on bare metal
18. sys_thread_create/create_in: thread leaked in ObjectTable on alloc_kernel_stack OOM
19. sys_space_destroy: killed threads' kernel stacks not freed

## Performance Improvements

1-3. Atomic removal from ObjectTable, HandleTable, Event (plain field ops)
4. VecDeque → FixedRing in scheduler RunQueue (128-slot inline ring buffer)
5. Scheduler::remove uses FixedRing directly
6. Vec<MappingRecord> → fixed MappingArray (128 entries, zero heap)
7. VaAllocator Vec → fixed array (64 entries, zero heap)
8. VMO resize callback eliminates Vec return
9. space_destroy uses stack array instead of Vec

**Result: zero heap allocations on ALL syscall hot paths.**

## Tests: 522 (was 472)

50 new tests including:
- 18 adversarial tests (all-zero args, all-max args, use-after-close, type confusion)
- 9 boundary tests (handle table fill/recover, multi-wait, bit 63, max handles)
- 11 state machine tests (every valid + invalid thread transition)
- 3 FixedRing edge cases (fill to capacity, wraparound, remove first/middle/last)
- 3 regression tests for critical fixes
- 6 stress tests (handle churn, IPC drain, mixed lifecycle, scheduler fairness)
- Mapping consistency invariant checker (runs on all tests)

## Convergence Passes

- Pass 1: 7 techniques. 1 finding (clippy clone-on-Copy).
- Pass 2: Deep scan agent. 0 findings.
- Pass 3: Independent code reviewer. 4 findings (2 critical, 2 high). Fixed.
- Pass 4: Review of pass-3 fixes. 2 findings (kernel stack leak, missing alloc). Fixed.
- Pass 5: Review of pass-4 fixes. 0 findings (I found alloc_kernel_stack OOM leak proactively).
- **Pass 6: 0 findings. Convergence count: 1.**
- **Pass 7: 0 findings (different review angle: IPC semantics, multi-wait, handle transfer, faults). Convergence count: 2.**

## Verification

- [x] `cargo build` — passes
- [x] `cargo test --target aarch64-apple-darwin` — 522 tests, 0 failures
- [x] `cargo run` — boots, 4/4 cores online, init exits 16384
- [x] `cargo clippy` — 0 warnings

## Files Modified

- `Cargo.toml` — opt-level=1 dev profile
- `kernel/link.ld` — stack size sync
- `kernel/src/main.rs` — Box Kernel on heap
- `kernel/src/syscall.rs` — IrqTable boxing, IPC fixes, scheduler ordering, kernel stack cleanup, 50 new tests
- `kernel/src/table.rs` — atomics → plain fields
- `kernel/src/handle.rs` — atomics → plain fields
- `kernel/src/event.rs` — AtomicU64 → u64
- `kernel/src/thread.rs` — VecDeque → FixedRing, state machine tests
- `kernel/src/address_space.rs` — Vec → fixed arrays, Copy MappingRecord
- `kernel/src/vmo.rs` — resize callback pattern
- `kernel/src/endpoint.rs` — reply cap collision avoidance
- `kernel/src/invariants.rs` — mapping consistency checker
- `kernel/src/frame/arch/aarch64/context.rs` — alloc_contiguous for kernel stacks
- `kernel/src/frame/arch/aarch64/psci.rs` — SMCCC clobbers
- `kernel/src/frame/arch/aarch64/secondary_entry.S` — EL2→EL1 transition
