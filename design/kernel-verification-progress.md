# Kernel Verification Progress

## Session 3 — 2026-05-04 — COMPLETE

### Results

| Metric | Session 2 | Session 3 | Delta |
|--------|-----------|-----------|-------|
| Tests | 584 | 641 | +57 |
| Bugs found | 17 | 17 | — |
| Bugs fixed | 17 | 17 | — |
| Invariant checks | 13 | 13 | — |
| Property tests | 22 | 22 | — |
| Commits on branch | 32 | 39 | +7 |

### Work Completed (Session 3)

**Phase 6 (Mutation Testing) — expanded across 7 files:**
- table.rs: 1 mutant killed (is_allocated false case), 1 equivalent (< vs <= guarded by assert_ne)
- sched.rs: 1 killed (yield with 2 threads), 2 bare-metal-only
- address_space.rs: 16 killed of 26 (VA allocator boundaries, mapping shifts, find_mapping, DestroyMappings)
- irq.rs: intids_for_event_bits fully tested, unbind/ack boundary checks added
- fault.rs: 3 killed (non-zero page index, pager path), 6 bare-metal-only
- bootstrap.rs: 5 killed (alive_threads, handle/mapping rights, stack size), 20+ bare-metal-only
- syscall.rs: clock_read arithmetic (10 mutants targeted), event multi-wait encoding (14 targeted)

**Phase 7 (Sanitizers) — platform-complete:**
- ASan: 628→641 tests pass clean (zero memory errors)
- LSan/UBSan: not available on macOS/aarch64
- TSan: requires -Zbuild-std (limited value for single-threaded host tests)

**Phase 8 (Concurrency) — host-side complete:**
- Multi-core thread creation verifies least-loaded-core scheduling (2 cores)
- Cross-core event signal/wake test
- Rapid create/destroy 100 cycles (zero leaked objects)
- IPC call/recv/reply 50 rapid rounds
- Loom not feasible (sync primitives are inline asm, not abstracted)

**Phase 9 (Error Injection) — expanded:**
- VMO table exhaustion and recovery
- Event table exhaustion and recovery
- Space table exhaustion and recovery
- thread_create_in with invalid handle rollback verification

**Phase 10 (Static Analysis) — complete:**
- Added #![deny(unused_unsafe)] lint
- Framekernel discipline verified (only frame/ has allow(unsafe_code))
- cargo audit: 0 vulnerabilities in 88 crate dependencies

**Phase 0.2 (State Machine Completeness) — complete:**
- Thread: 4 states, 9 valid transitions, Exited is absorbing, all states can reach Exited
- Endpoint: close_peer drains all pending calls, active replies, recv waiters
- Event: signal/clear/wait/refcount — simple, complete
- VMO: create/seal/resize/snapshot/map — all transitions defined
- All state machines: no unreachable states, no absorbing states other than destroyed

### Phase Status

| Phase | Status | Notes |
|-------|--------|-------|
| 0. Spec Review | 95% | 0.1 done, 0.2 done (state machines complete), 0.4 done |
| 1. Unsafe Audit | 100% | 81 blocks in 15 files — ALL CLEAN |
| 2. Property Testing | 90% | 22 proptests |
| 3. Fuzzing | 95% | 44M runs (sequence) + 100K (structured), zero crashes |
| 4. Miri | 95% | Running with 641 tests (pending session 3 verification) |
| 5. Coverage | 80% | 96% syscall.rs, 97-99% core objects |
| 6. Mutation Testing | 80% | 7 critical files tested, most survivors are bare-metal-only or equivalent |
| 7. Sanitizers | 90% | ASan: 641 tests clean. LSan/UBSan unavailable on macOS |
| 8. Concurrency | 60% | Host-side stress tests done. Loom not feasible. SMP bare-metal pending |
| 9. Error Injection | 80% | All object types: exhaustion+recovery. Multi-step rollback verified |
| 10. Static Analysis | 90% | deny attrs, cargo audit clean. Pedantic clippy: 79 cast warnings (safe) |
| 11. Bare-Metal + Perf | 0% | Next priority |
| 12. Regression Infra | 40% | Pre-commit hook + Makefile targets |

### Next Session Priorities

1. Phase 11: bare-metal integration tests, benchmarks, cycle-accurate measurement
2. Phase 12: nightly gate, performance regression thresholds
3. Phase 4: re-verify Miri with 641 tests
4. Phase 2: additional proptests for multi-wait, clock_read edge cases
5. Remaining mutation testing survivors (boundary precision in partition_point)

---

## Session 2 — 2026-05-04 — COMPLETE

### Results

| Metric | Session 1 | Session 2 | Delta |
|--------|-----------|-----------|-------|
| Tests | 540 | 584 | +44 |
| Bugs found | 10 | 17 | +7 |
| Bugs fixed | 10 | 17 | +7 |
| Invariant checks | 13 | 13 | — |
| Property tests | 13 | 22 | +9 |
| Commits on branch | 12 | 32 | +20 |

### Bugs Fixed (Session 2)

| # | Severity | Bug | Commit |
|---|----------|-----|--------|
| 11 | CRITICAL | Caller blocked on sys_call gets Ok(0) on endpoint destruction — indistinguishable from valid reply | 07ed74d |
| 12 | CRITICAL | Transferred handles permanently lost when close_peer drains pending calls | 07ed74d |
| 13 | HIGH | handle_close doesn't clean up objects — endpoints/events/VMOs leak when last handle closed | cb1b2fa |
| 14 | HIGH | endpoint_bind_event only sets ep.bound_event, not evt.bound_endpoint — unidirectional binding | 40e784b |
| 15 | MEDIUM | PriorityRing test helper reads wrong slots on wraparound | 07ed74d |

### Bugs Fixed (Session 2, continued)

| # | Severity | Bug | Commit |
|---|----------|-----|--------|
| 16 | HIGH | do_call test helper dangling reply buffer (Miri UB) | 82a91d6 |
| 17 | MEDIUM | ASID pool never freed in test mode, causing test isolation failures | f41fd9a |

### Infrastructure Added (Session 2)

- **Thread.wakeup_error field** — async error communication to blocked threads
- **Reference counting for Endpoints and Events** — refcount init=1, add_ref/release_ref
- **release_object_ref / add_object_ref helpers** — centralized object lifecycle management
- **close_endpoint_peer / destroy_event helpers** — extracted from space_destroy for reuse
- **handle_close now triggers object cleanup** — VMOs, endpoints, events properly freed
- **handle_dup increments refcount** — prevents premature free with multiple handles
- **thread_create_in increments refcount** — cloned handles tracked correctly
- **Bidirectional event-endpoint binding** — both sides now know about each other
- **Coverage measurement** — 96%+ line coverage on syscall handlers, 97-99% on object modules
- **Miri UB fix confirmed** — 5 previously-failing tests pass clean
- **ASID pool test isolation** — reset_asid_pool() + test-mode free_asid in space_destroy
- **Error code audit** — 9 untested error paths now covered
- **5 new property tests** — multi-object interaction, scheduler, IPC transfer refcount

### Phase Status

| Phase | Status | Notes |
|-------|--------|-------|
| 0. Spec Review | 90% | 0.1 done (7 bugs), 0.4 done (9 error paths). 0.2 partial |
| 1. Unsafe Audit | 100% | 81 blocks in 15 files — ALL CLEAN |
| 2. Property Testing | 90% | 20 proptests inc. multi-object, scheduler, IPC transfer |
| 3. Fuzzing | 95% | 44M runs (sequence) + 100K (structured), zero crashes |
| 4. Miri | 95% | 206 tests pass clean (handle+endpoint+event+vmo+table+addr_space+95 syscall) |
| 5. Coverage | 80% | 96% syscall.rs, 97-99% core objects. Key gaps filled |
| 6. Mutation Testing | 50% | handle(71%), endpoint(88%), event(83%). syscall.rs pending |
| 7. Sanitizers | 50% | ASan: 575 tests pass clean. LSan/UBSan pending |
| 8. Concurrency | 20% | Multi-core scheduling proptests added |
| 9. Error Injection | 60% | Capacity exhaustion, rollback, error path tests done |
| 10. Static Analysis | 50% | deny attrs, pedantic clippy reviewed, 2 deps verified |
| 11. Bare-Metal + Perf | 0% | |
| 12. Regression Infra | 40% | Pre-commit hook + Makefile targets (gate, miri, asan, fuzz, coverage) |

### Next Session Priorities

1. Phase 6: continue mutation testing on remaining critical files (endpoint.rs, event.rs, syscall.rs, sched.rs)
2. Phase 8: concurrency verification (Loom for sync primitives, multi-core scheduler stress)
3. Phase 11: bare-metal benchmarks (cycle-accurate per-syscall measurement)
4. Phase 12: regression infrastructure (Makefile targets, nightly gate)
5. Phase 9: OOM injection at specific allocation points in multi-step syscalls

---

## Session 1 — 2026-05-04 — COMPLETE

### Results

| Metric | Before | After |
|--------|--------|-------|
| Tests | 524 | 540 |
| Bugs found | 0 | 10 |
| Bugs fixed | 0 | 10 |
| Invariant checks | 8 | 13 |
| Property tests | 0 | 13 |
| Fuzz targets with invariant checking | 0 | 2 |
| Miri-verified modules | 0 | 5 (71 tests) |
| Commits on branch | 0 | 12 |

### Bugs Fixed

| # | Severity | Bug | Commit |
|---|----------|-----|--------|
| 1 | CRITICAL | sys_call handle leak on full endpoint | bca8d1c |
| 2 | CRITICAL | sys_reply handle leak on write failure | bca8d1c |
| 3 | CRITICAL | space_destroy alive_threads not decremented | bca8d1c |
| 4 | HIGH | event_wait_common waiter leak on partial failure | bca8d1c |
| 5 | HIGH | space_destroy: killed threads left in endpoint recv_waiters | ff3fd3a |
| 6 | HIGH | sys_reply silent handle install failure | 8ac3a68 |
| 7 | HIGH | VMO resize below active mapping size (FUZZ-FOUND) | 2d350cf |
| 8 | HIGH | IRQ bindings survive event destruction | 4b1ae78 |
| 9 | HIGH | endpoint.bound_event survives event destruction | 4b1ae78 |
| 10 | HIGH | event.bound_endpoint survives endpoint destruction | 4b1ae78 |

### Analyzed but NOT a bug

- `recv_deliver` dequeue without requeue on failure: analyzed and determined to
  be correct behavior. If the server's buffer is too small or write fails, that's
  a server error — the call is consumed, the caller stays blocked, and the reply
  cap is valid. The server can retry recv or reply with an error. This matches
  seL4/QNX sync IPC semantics.

**To resume:** `git log --oneline kernel-verification`, read this file, continue
from priorities above.
