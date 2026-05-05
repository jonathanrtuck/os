# Kernel Verification Progress

## Session 2 — 2026-05-04 — IN PROGRESS

### Results

| Metric | Session 1 | Session 2 | Delta |
|--------|-----------|-----------|-------|
| Tests | 540 | 575 | +35 |
| Bugs found | 10 | 17 | +7 |
| Bugs fixed | 10 | 17 | +7 |
| Invariant checks | 13 | 13 | — |
| Property tests | 13 | 20 | +7 |
| Commits on branch | 12 | 20 | +8 |

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
| 3. Fuzzing | 90% | 44M runs in 1hr, zero crashes. Structured target pending |
| 4. Miri | 90% | 111+ tests pass clean. UB fix confirmed. No UB in any core module |
| 5. Coverage | 80% | 96% syscall.rs, 97-99% core objects. Key gaps filled |
| 6. Mutation Testing | 30% | handle.rs: 15 caught, 6 missed → 3 tests added. More files queued |
| 7. Sanitizers | 50% | ASan: 575 tests pass clean. LSan/UBSan pending |
| 8. Concurrency | 0% | |
| 9. Error Injection | 60% | Capacity exhaustion, rollback, error path tests done |
| 10. Static Analysis | 50% | deny attrs, pedantic clippy reviewed, 2 deps verified |
| 11. Bare-Metal + Perf | 0% | |
| 12. Regression Infra | 10% | Pre-commit hook works |

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
