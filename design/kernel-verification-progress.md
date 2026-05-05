# Kernel Verification Progress

## Current Session: 1 — 2026-05-04

**Active:** 1-hour fuzz run (syscall_sequence) + Miri on syscall tests, both
in background.

### Completed This Session

**Bugs Fixed: 7 total**

| # | Severity | Bug | Fix |
|---|----------|-----|-----|
| 1 | CRITICAL | sys_call handle leak on full endpoint | Pre-check endpoint state |
| 2 | CRITICAL | sys_reply handle leak on write failure | Reinstall on error |
| 3 | CRITICAL | space_destroy alive_threads not decremented | Fixed |
| 4 | HIGH | event_wait_common waiter leak on partial failure | Rollback |
| 5 | HIGH | space_destroy: killed threads left in endpoint recv_waiters | Scan+remove |
| 6 | HIGH | sys_reply silent handle install failure | debug_assert |
| 7 | HIGH | VMO resize below active mapping size (FUZZ-FOUND) | Reject resize |

**Phases Completed/In Progress:**

- Phase 0 (Spec Review): 0.3 done (5 new invariants), 4 bugs found+fixed.
  Remaining: 0.1 interaction matrix, 0.2 state machines, 0.4 error audit
- Phase 2 (Property Testing): DONE — 13 proptest property tests
- Phase 3 (Fuzzing): Fuzz targets overhauled with invariant checking. 1-hour
  run in progress. Already found 1 real bug (VMO resize-while-mapped).
- Phase 4 (Miri): 71 tests pass under Miri (handle, vmo, event, endpoint,
  table modules). No UB found. Syscall tests running in background.
- Phase 10 (Static Analysis): deny(unused_must_use, unreachable_patterns),
  fuzzing feature flag

**Test Count: 540** (was 524 at session start, +16 tests)

### Git Log (kernel-verification branch)

```
e1dc38a fix(kernel): fuzz harness skip scheduling-changing syscalls
2d350cf fix(kernel): VMO resize-while-mapped bug + fuzz target hardening
8ac3a68 fix(kernel): sys_reply debug_assert on handle install
ff3fd3a fix(kernel): space_destroy removes killed threads from endpoint recv_waiters
3cbeb2f feat(kernel): deny(unused_must_use, unreachable_patterns) + fuzzing cfg
7334f7e feat(kernel): fuzz targets overhauled with invariant checking (Phase 3)
6d415f2 feat(kernel): 13 property-based tests via proptest (Phase 2)
d2da40a test(kernel): 3 regression tests for Phase 0 bug fixes
bca8d1c fix(kernel): 4 bugs fixed (spec review Phase 0)
31c308e feat(kernel): verification plan + 5 new invariant checks
```

### Known Remaining Bugs

From agent analysis — not yet fixed:
- recv_deliver: dequeue without requeue on install_handles failure
- IRQ bindings survive event destruction (stale event_id)
- endpoint.bound_event survives event destruction (stale EventId)
- event.bound_endpoint survives endpoint destruction (stale EndpointId)

### Remaining Phases

- Phase 0: 0.1 interaction matrix, 0.2 state machines, 0.4 error audit
- Phase 1: Unsafe audit (104 blocks in frame/)
- Phase 5: Coverage measurement
- Phase 6: Mutation testing
- Phase 7: Sanitizers (ASan/LSan/UBSan on test suite)
- Phase 8: Concurrency verification
- Phase 9: Error injection
- Phase 10: Pedantic clippy, cargo-audit (remaining)
- Phase 11: Bare-metal verification + performance
- Phase 12: Regression infrastructure

**To resume:** Read this file, `git log --oneline kernel-verification`,
continue with remaining phases. Check if fuzz artifacts exist at
`kernel/fuzz/artifacts/syscall_sequence/` — any crash file = new bug to fix.
