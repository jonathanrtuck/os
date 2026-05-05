# Kernel Verification Progress

## Current Phase: 0 (Adversarial Spec Review)

### Session 1 — 2026-05-04

**Status:** Phase 0 in progress.

**Completed:**
- Read entire kernel codebase (all .rs files in kernel/src/)
- Identified 8 new invariants not checked by invariants.rs (see below)
- Three background analysis agents running on: spec, syscall.rs (5.6K lines),
  object lifecycle (endpoint/thread/event)

**Key Findings So Far:**

Missing invariants identified:
1. Handle table free list integrity (no cycles, no unreachable entries)
2. ObjectTable free list integrity (same)
3. VA allocator consistency (sorted, non-overlapping, sum = USER_VA_SIZE - mapped)
4. VMO mapping range validity (mapping within VMO's actual size)
5. IRQ binding consistency (bound event_id → live event)
6. IPC protocol invariant (blocked-on-call thread ↔ PendingCall)
7. Event waiter validity (registered waiter → blocked thread)
8. VMO refcount accuracy (refcount == handle count pointing to it)

Potential edge case bugs spotted (need verification from agents):
- VMO resize while mapped: `resize()` frees pages without checking mappings
- Scheduler: `switch_away()` sets current=None if no runnable thread exists
- block_current() doesn't check if there IS a next thread
- `wake()` silently ignores non-Blocked threads (intentional but needs spec)

**Next:**
- Receive agent analysis results
- Complete Phase 0.1 (syscall interaction matrix)
- Complete Phase 0.2 (state machine completeness)
- Implement Phase 0.3 (add all new invariants to invariants.rs)
- Complete Phase 0.4 (error code audit)

**To resume next session:** Read this file, check `[DONE]` markers in
`design/kernel-verification-plan.md`, run `git log --oneline kernel-verification`.
