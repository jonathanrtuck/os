# Kernel Verification Progress

## Current Session: 1 — 2026-05-04

**Active work:** Phase 3 (fuzz overhaul) and Phase 4 (Miri) running as
background agents. Phases 0, 2, 10 partially complete.

### Completed This Session

**Phase 0 (Spec Review) — partially complete:**
- [DONE] 0.3: 5 new invariants added to `invariants.rs` (IPC blocked thread
  consistency, event waiter validity, IRQ binding consistency, VMO mapping
  range validity, object reachability/leak detection)
- [DONE] 0.1 partial: adversarial analysis via 3 agents identified 4 critical
  + 4 high-severity bugs

**4 critical bugs found and FIXED:**
1. `sys_call` handle leak on full endpoint → pre-check endpoint state
2. `sys_reply` handle leak on write failure → reinstall handles on error
3. `sys_space_destroy` alive_threads not decremented → fixed
4. `event_wait_common` waiter leak on partial failure → rollback on error

**3 regression tests written** for bugs 1, 3, 4.

**Phase 2 (Property Testing) — DONE:**
- 13 proptest property tests added (`kernel/src/proptests.rs`)
- Covers: VMO create/seal/snapshot, handle dup/close/info, event
  signal/clear, syscall dispatch, multi-step create/close cycles,
  generation revocation
- Boundary value generators for sizes, u64s, handle IDs, rights

**Phase 10 (Static Analysis) — partially complete:**
- deny(unused_must_use, unreachable_patterns) added to lib.rs
- `fuzzing` feature flag and cfg lint added
- invariants module accessible under `#[cfg(any(test, fuzzing))]`

### In Progress (Background Agents)

- **Phase 3:** Fuzz target overhaul — adding invariant checking, fixing
  dispatch signature, creating dictionary
- **Phase 4:** Miri analysis — checking for UB in host-target tests

### Test Count

540 tests passing (was 524 at session start).

### Remaining Phases

- Phase 0: 0.1 (interaction matrix), 0.2 (state machines), 0.4 (error audit)
- Phase 1: Unsafe audit (104 blocks)
- Phase 3: Fuzz overhaul (agent in progress)
- Phase 4: Miri (agent in progress)
- Phase 5: Coverage measurement
- Phase 6: Mutation testing
- Phase 7: Sanitizers
- Phase 8: Concurrency verification
- Phase 9: Error injection
- Phase 10: Pedantic clippy, cargo-audit
- Phase 11: Bare-metal verification + performance
- Phase 12: Regression infrastructure

### Known Remaining Bugs (found but not yet fixed)

From agent analysis — HIGH severity:
5. `recv_deliver`: dequeue without requeue on install_handles failure
6. `space_destroy`: does not remove killed threads from endpoint recv_waiters
7. IRQ bindings and endpoint↔event cross-refs survive object destruction
8. `sys_reply` line 1263: `let _ = caller_ht.install(h)` silently discards errors

### Git Log

```
3cbeb2f feat(kernel): deny(unused_must_use, unreachable_patterns) + fuzzing cfg
6d415f2 feat(kernel): 13 property-based tests via proptest (Phase 2)
d2da40a test(kernel): 3 regression tests for Phase 0 bug fixes
bca8d1c fix(kernel): 4 bugs fixed (spec review Phase 0)
31c308e feat(kernel): verification plan + 5 new invariant checks
```

**To resume next session:** Read this file, read
`design/kernel-verification-plan.md` (check `[DONE]` markers), run
`git log --oneline kernel-verification`, continue from "Remaining Phases".
