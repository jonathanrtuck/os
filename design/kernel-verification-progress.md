# Kernel Verification Progress

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

### Phase Status

| Phase | Status | Notes |
|-------|--------|-------|
| 0. Spec Review | 40% | 0.3 done (invariants). 0.1/0.2/0.4 remaining |
| 1. Unsafe Audit | 0% | 104 blocks in frame/ |
| 2. Property Testing | 80% | 13 proptests. More state machine tests needed |
| 3. Fuzzing | 70% | Invariant checking added. 2-min clean run. 1-hour pending |
| 4. Miri | 60% | 71 tests pass, no UB. Syscall tests blocked by asm |
| 5. Coverage | 0% | |
| 6. Mutation Testing | 0% | |
| 7. Sanitizers | 0% | |
| 8. Concurrency | 0% | |
| 9. Error Injection | 0% | |
| 10. Static Analysis | 30% | deny attrs added. Pedantic clippy, cargo-audit remaining |
| 11. Bare-Metal + Perf | 0% | |
| 12. Regression Infra | 10% | Pre-commit hook works. Makefile targets needed |

### Analyzed but NOT a bug

- `recv_deliver` dequeue without requeue on failure: analyzed and determined to
  be correct behavior. If the server's buffer is too small or write fails, that's
  a server error — the call is consumed, the caller stays blocked, and the reply
  cap is valid. The server can retry recv or reply with an error. This matches
  seL4/QNX sync IPC semantics.

### Next Session Priorities

1. Start the 1-hour fuzz run and check results
2. Phase 1: unsafe audit (104 blocks — highest remaining leverage)
3. Phase 5: coverage measurement (find what's untested)
4. Phase 6: mutation testing (find tests that don't actually test anything)
5. Continue fixing any bugs found by fuzzing
6. Phase 9: error injection (capacity exhaustion, OOM at every allocation point)

**To resume:** `git log --oneline kernel-verification`, check
`kernel/fuzz/artifacts/` for crash files, continue from priorities above.
