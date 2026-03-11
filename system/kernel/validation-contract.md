# Kernel Bug Audit — Validation Contract

This contract defines the testable behavioral assertions that determine "done" for each audit milestone. Every assertion has a stable ID, clear pass/fail criteria, and evidence requirements that a validator can check by running terminal commands or inspecting files.

**Baseline snapshot:**

- 348 existing tests across 18 test files in `system/test/tests/`
- 112 unsafe sites across 17 kernel files (blocks + unsafe fn + unsafe impl)
- Zero `#[allow(...)]` attributes
- Build command: `cd system && cargo build`
- Test command: `cd system/test && cargo test -- --test-threads=1`

---

## Milestone 0: Test Infrastructure Verification

| ID            | Title                   | Pass/Fail Condition                                                                                      | Evidence                                                                                                                       |
| ------------- | ----------------------- | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| VAL-INFRA-001 | Existing tests pass     | `cd system/test && cargo test -- --test-threads=1` exits 0, reports 348 tests passing                    | Terminal output shows `test result: ok. 348 passed` (or current count) with exit code 0                                        |
| VAL-INFRA-002 | Kernel builds           | `cd system && cargo build` exits 0 with no errors                                                        | Terminal output shows `Finished` with exit code 0                                                                              |
| VAL-INFRA-003 | Miri viability assessed | Run `cd system/test && cargo +nightly miri test 2>&1` and document which tests pass, which fail, and why | File `system/kernel/AUDIT-MIRI-REPORT.md` or equivalent exists documenting Miri results. If Miri is unavailable, document that |

---

## Milestone 1: Boot + Memory Foundation

**Files:** boot.S, exception.S, link.ld, main.rs, memory.rs, paging.rs, page_allocator.rs, memory_region.rs, memory_mapped_io.rs, heap.rs, slab.rs
**Unsafe sites:** main.rs (19), memory.rs (21), page_allocator.rs (7), heap.rs (7), memory_mapped_io.rs (6), slab.rs (4) = 64 total

| ID          | Title                                         | Pass/Fail Condition                                                                                                                                                                                                       | Evidence                                                                                                                                                                                          |
| ----------- | --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| VAL-MEM-001 | Assembly + linker script audited              | Every register clobber, barrier, TLB op, and EL transition in boot.S and exception.S has been reviewed. link.ld section alignment and KEEP directives verified. Findings documented as inline comments or commit messages | `git diff origin/main -- system/kernel/boot.S system/kernel/exception.S system/kernel/link.ld` shows SAFETY/audit comments added or confirms no changes needed (with rationale in commit message) |
| VAL-MEM-002 | Memory + paging unsafe blocks audited         | All 21 unsafe sites in memory.rs and any in paging.rs have a `// SAFETY:` comment explaining why the invariant holds. New or updated comments visible in diff                                                             | `grep -c 'SAFETY' system/kernel/memory.rs` ≥ number of unsafe blocks. `git diff origin/main -- system/kernel/memory.rs system/kernel/paging.rs` shows additions                                   |
| VAL-MEM-003 | Page allocator unsafe blocks audited          | All 7 unsafe sites in page_allocator.rs have verified SAFETY comments                                                                                                                                                     | `grep -c 'SAFETY' system/kernel/page_allocator.rs` ≥ 7                                                                                                                                            |
| VAL-MEM-004 | Heap + slab unsafe blocks audited             | All 11 unsafe sites across heap.rs (7) and slab.rs (4) have verified SAFETY comments                                                                                                                                      | `grep -c 'SAFETY' system/kernel/heap.rs` ≥ 7 and `grep -c 'SAFETY' system/kernel/slab.rs` ≥ 4                                                                                                     |
| VAL-MEM-005 | MMIO unsafe blocks audited                    | All 6 unsafe sites in memory_mapped_io.rs have verified SAFETY comments                                                                                                                                                   | `grep -c 'SAFETY' system/kernel/memory_mapped_io.rs` ≥ 6                                                                                                                                          |
| VAL-MEM-006 | memory_region.rs readable=false TODO resolved | The TODO at memory_region.rs:23 is addressed — either enforced or documented as intentional                                                                                                                               | `grep -c 'TODO' system/kernel/memory_region.rs` = 0 for that specific TODO, or a `// NOTE:` explaining the design choice                                                                          |
| VAL-MEM-007 | TDD: new tests for findings                   | At least one new test file or new test functions exist targeting memory/allocator findings                                                                                                                                | `git diff origin/main --stat -- system/test/tests/` shows new or modified test files for buddy/heap/slab/vma areas                                                                                |
| VAL-MEM-008 | No regressions                                | Full test suite passes and kernel builds after all Milestone 1 changes                                                                                                                                                    | `cd system/test && cargo test -- --test-threads=1` exits 0. `cd system && cargo build` exits 0                                                                                                    |

---

## Milestone 2: Process + Address Space

**Files:** process.rs, process_exit.rs, address_space.rs, address_space_id.rs, executable.rs, context.rs
**Unsafe sites:** address_space.rs (10), address_space_id.rs (1), process.rs (1) = 12 total

| ID           | Title                                 | Pass/Fail Condition                                                                                                                                     | Evidence                                                                                                               |
| ------------ | ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| VAL-PROC-001 | Address space unsafe blocks audited   | All 10 unsafe sites in address_space.rs have verified SAFETY comments. Break-before-make TLB maintenance verified correct                               | `grep -c 'SAFETY' system/kernel/address_space.rs` ≥ 10                                                                 |
| VAL-PROC-002 | Process lifecycle audited             | process.rs and process_exit.rs reviewed for resource cleanup completeness (handle drainage, ASID release, page table deallocation). Findings documented | `git diff origin/main -- system/kernel/process.rs system/kernel/process_exit.rs` shows audit comments or fixes         |
| VAL-PROC-003 | ASID exhaustion + ELF parsing audited | address_space_id.rs wrapping behavior verified. executable.rs bounds checking on all ELF header fields verified                                         | `git diff origin/main -- system/kernel/address_space_id.rs system/kernel/executable.rs` shows audit evidence           |
| VAL-PROC-004 | TDD: new tests for findings           | New test cases exist for any bugs found in process/address-space subsystem                                                                              | `git diff origin/main --stat -- system/test/tests/` shows changes in asid, vma, or executable test files, or new files |
| VAL-PROC-005 | No regressions                        | Full test suite passes and kernel builds after all Milestone 2 changes                                                                                  | `cd system/test && cargo test -- --test-threads=1` exits 0. `cd system && cargo build` exits 0                         |

---

## Milestone 3: Scheduling + Threads

**Files:** scheduler.rs, scheduling_algorithm.rs, scheduling_context.rs, thread.rs, thread_exit.rs, per_core.rs
**Unsafe sites:** scheduler.rs (4), thread.rs (3), per_core.rs (1) = 8 total

| ID            | Title                           | Pass/Fail Condition                                                                                                                                   | Evidence                                                                                                                                           |
| ------------- | ------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| VAL-SCHED-001 | Scheduler unsafe blocks audited | All 4 unsafe sites in scheduler.rs have verified SAFETY comments. Context switch asm correctness verified                                             | `grep -c 'SAFETY' system/kernel/scheduler.rs` ≥ 4                                                                                                  |
| VAL-SCHED-002 | Thread lifecycle audited        | thread.rs Send/Sync impls justified. Drop ordering verified. Deferred thread drop pattern (previously a bug source) re-verified sound                 | `git diff origin/main -- system/kernel/thread.rs system/kernel/thread_exit.rs` shows audit evidence                                                |
| VAL-SCHED-003 | EEVDF + per-core audited        | scheduling_algorithm.rs vruntime overflow behavior verified. per_core.rs state isolation under SMP verified. scheduling_context.rs edge cases checked | `git diff origin/main -- system/kernel/scheduling_algorithm.rs system/kernel/per_core.rs system/kernel/scheduling_context.rs` shows audit evidence |
| VAL-SCHED-004 | TDD: new tests for findings     | New test cases exist for any bugs found in scheduling/thread subsystem                                                                                | `git diff origin/main --stat -- system/test/tests/` shows changes in eevdf, scheduler_state, or sched_context test files                           |
| VAL-SCHED-005 | No regressions                  | Full test suite passes and kernel builds after all Milestone 3 changes                                                                                | `cd system/test && cargo test -- --test-threads=1` exits 0. `cd system && cargo build` exits 0                                                     |

---

## Milestone 4: Synchronization + IPC

**Files:** sync.rs, futex.rs, waitable.rs, channel.rs, handle.rs
**Unsafe sites:** sync.rs (5) = 5 total (other files use IrqMutex but no raw unsafe)

| ID           | Title                       | Pass/Fail Condition                                                                                                                         | Evidence                                                                                                            |
| ------------ | --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| VAL-SYNC-001 | Sync primitives audited     | All 5 unsafe sites in sync.rs verified (ticket spinlock correctness, IRQ masking, `unsafe impl Sync`). IrqMutex guard soundness confirmed   | `grep -c 'SAFETY' system/kernel/sync.rs` ≥ 5                                                                        |
| VAL-SYNC-002 | Futex + waitable audited    | futex.rs spurious wakeup handling, timeout paths, missing barriers checked. waitable.rs registry correctness verified                       | `git diff origin/main -- system/kernel/futex.rs system/kernel/waitable.rs` shows audit evidence                     |
| VAL-SYNC-003 | Channel + handle audited    | channel.rs shared memory safety, close-while-waiting races, handle lifecycle in handle.rs (close ordering, cross-process transfer) verified | `git diff origin/main -- system/kernel/channel.rs system/kernel/handle.rs` shows audit evidence                     |
| VAL-SYNC-004 | TDD: new tests for findings | New test cases for any bugs found in sync/IPC subsystem                                                                                     | `git diff origin/main --stat -- system/test/tests/` shows changes in channel, handle, futex, or waitable test files |
| VAL-SYNC-005 | No regressions              | Full test suite passes and kernel builds after all Milestone 4 changes                                                                      | `cd system/test && cargo test -- --test-threads=1` exits 0. `cd system && cargo build` exits 0                      |

---

## Milestone 5: Hardware + Syscall Interface

**Files:** interrupt.rs, interrupt_controller.rs, timer.rs, device_tree.rs, serial.rs, power.rs, metrics.rs, syscall.rs
**Unsafe sites:** syscall.rs (13), timer.rs (5), interrupt_controller.rs (4), power.rs (1) = 23 total

| ID         | Title                                          | Pass/Fail Condition                                                                                                                                                                  | Evidence                                                                                                                                            |
| ---------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| VAL-HW-001 | Interrupt + timer unsafe blocks audited        | All 4 unsafe sites in interrupt_controller.rs and 5 in timer.rs have verified SAFETY comments. GIC barrier ordering (DSB/ISB) confirmed correct                                      | `grep -c 'SAFETY' system/kernel/interrupt_controller.rs` ≥ 4. `grep -c 'SAFETY' system/kernel/timer.rs` ≥ 5                                         |
| VAL-HW-002 | Device tree + serial + power + metrics audited | device_tree.rs parsing edge cases (malformed blobs, truncated data) reviewed. serial.rs MMIO safety confirmed. power.rs shutdown path verified. metrics.rs overflow behavior checked | `git diff origin/main -- system/kernel/device_tree.rs system/kernel/serial.rs system/kernel/power.rs system/kernel/metrics.rs` shows audit evidence |
| VAL-HW-003 | Syscall unsafe blocks audited                  | All 13 unsafe sites in syscall.rs have verified SAFETY comments. User pointer validation, privilege escalation vectors, and the previously-fixed aliasing patterns re-verified       | `grep -c 'SAFETY' system/kernel/syscall.rs` ≥ 13                                                                                                    |
| VAL-HW-004 | TDD: new tests for findings                    | New test cases for any bugs found in hardware/syscall subsystem                                                                                                                      | `git diff origin/main --stat -- system/test/tests/` shows new or modified test files                                                                |
| VAL-HW-005 | No regressions                                 | Full test suite passes and kernel builds after all Milestone 5 changes                                                                                                               | `cd system/test && cargo test -- --test-threads=1` exits 0. `cd system && cargo build` exits 0                                                      |

---

## Milestone 6: Cross-File Invariants

| ID            | Title                                      | Pass/Fail Condition                                                                                                                                                                                                                                                                                                                                                     | Evidence                                                                                                                                   |
| ------------- | ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| VAL-CROSS-001 | Lock ordering map produced                 | A documented lock ordering exists listing every `IrqMutex`/`static STATE` across the kernel (at least 16 lock sites identified: page_allocator, slab, scheduler, heap, memory, futex, channel, timer, interrupt, serial, process_exit, thread_exit, address_space_id, plus any nested acquisitions). Map shows which locks can be held simultaneously and in what order | A file or section in a commit message contains the lock ordering map. `git log --oneline origin/main..HEAD` references lock ordering audit |
| VAL-CROSS-002 | No deadlock potential                      | Lock ordering map verified: no circular dependencies exist. Any code path that acquires multiple locks acquires them in documented order                                                                                                                                                                                                                                | Commit message or audit document explicitly states "no circular lock dependencies found" or documents the fix if one was found             |
| VAL-CROSS-003 | Cross-module lifetime assumptions verified | Relationships verified: (a) handle.rs ↔ channel.rs — handle close while channel blocked, (b) scheduler.rs ↔ thread_exit.rs — thread drop after exit notification, (c) process_exit.rs ↔ address_space.rs — address space deallocation after process exit, (d) timer.rs ↔ scheduler.rs — timer callback on dead thread                                                   | `git log --oneline origin/main..HEAD` references cross-module lifetime audit. Commit messages describe specific relationships verified     |
| VAL-CROSS-004 | TDD: new tests for cross-module findings   | If any cross-module bugs were found, corresponding test cases exist                                                                                                                                                                                                                                                                                                     | `git diff origin/main --stat -- system/test/tests/` shows evidence, or commit message documents "no cross-module bugs found"               |
| VAL-CROSS-005 | No regressions                             | Full test suite passes and kernel builds after all Milestone 6 changes                                                                                                                                                                                                                                                                                                  | `cd system/test && cargo test -- --test-threads=1` exits 0. `cd system && cargo build` exits 0                                             |

---

## Milestone 7: Adversarial Stress Tests + Miri

| ID             | Title                                       | Pass/Fail Condition                                                                                                                                                                      | Evidence                                                                                                                                |
| -------------- | ------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| VAL-STRESS-001 | Miri-compatible tests run clean             | All tests that can run under Miri pass without UB reports. Tests that cannot run under Miri are documented with reason                                                                   | Terminal output of `cargo +nightly miri test` (or documented as unavailable). Any Miri UB findings have corresponding fixes             |
| VAL-STRESS-002 | New adversarial tests target audit findings | At least one new stress/fuzz test file exists exercising patterns discovered during milestones 1-6 (e.g., concurrent allocation/free, ASID wrap, handle exhaustion, channel close races) | `git diff origin/main --stat -- system/test/tests/ system/user/fuzz/ system/user/stress/` shows new test files or significant additions |
| VAL-STRESS-003 | Full suite green                            | All existing tests + all new tests pass in a single run                                                                                                                                  | `cd system/test && cargo test -- --test-threads=1` exits 0. Total test count ≥ 348 (baseline)                                           |
| VAL-STRESS-004 | Kernel builds at all optimization levels    | `cd system && cargo build` and `cd system && cargo build --release` both succeed                                                                                                         | Both commands exit 0                                                                                                                    |
| VAL-STRESS-005 | Headless stress test passes                 | If stress-test.sh exists and is runnable, `./stress-test.sh 30` completes without crashes                                                                                                | Terminal output shows successful completion, or document that the script is not available in the current environment                    |

---

## Summary Counts

| Area                         | Assertions | Unsafe Sites Covered |
| ---------------------------- | ---------- | -------------------- |
| Infrastructure (M0)          | 3          | —                    |
| Boot + Memory (M1)           | 8          | 64                   |
| Process + Address Space (M2) | 5          | 12                   |
| Scheduling + Threads (M3)    | 5          | 8                    |
| Synchronization + IPC (M4)   | 5          | 5                    |
| Hardware + Syscall (M5)      | 5          | 23                   |
| Cross-File Invariants (M6)   | 5          | —                    |
| Stress Tests + Miri (M7)     | 5          | —                    |
| **Total**                    | **41**     | **112**              |

## Validation Procedure

A validator determines pass/fail for each assertion by:

1. **Terminal commands:** Run the exact command specified in the Evidence column. Check exit code and output against the pass/fail condition.
2. **Git inspection:** Run `git diff origin/main -- <path>` or `git log --oneline origin/main..HEAD` to verify changes were made and committed.
3. **Grep counts:** Run `grep -c 'SAFETY' <file>` to verify SAFETY comment coverage meets the threshold.
4. **File existence:** Use `ls` or `test -f` to verify expected files exist.

**The audit is complete when all 41 assertions pass.**
