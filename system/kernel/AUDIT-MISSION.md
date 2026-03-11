# Kernel Bug Audit Mission Prompt

Paste the content below the `---` line into `/missions`:

---

## Mission: Comprehensive Kernel Bug Audit

### Objective

Audit every source file in `system/kernel/` (33 .rs files + 2 .S files + link.ld) against a comprehensive bug category checklist. Every finding gets a failing test written first (TDD), then a fix. All existing 304 tests must still pass. code-reviewer and security-reviewer droids validate each milestone. A new adversarial stress/fuzz test suite exercises the fixes.

### Scope

Kernel only: `system/kernel/`. Not libraries, services, or user programs.

### Bug Category Checklist (applied per file)

1. **Memory safety:** UB (aliasing, uninitialized, transmute), use-after-free, buffer overflows, integer overflow/underflow
2. **Concurrency:** data races, missing barriers (DMB/DSB/ISB), lock ordering violations, SMP hazards, interrupt-safety
3. **Error handling:** unchecked unwrap/expect in fallible paths, missing error propagation, OOM handling
4. **Edge cases:** off-by-one, boundary conditions, zero-length inputs, maximum values
5. **Resource leaks:** unfreed pages, orphaned handles, missing Drop impls, ASID leaks
6. **AArch64 correctness:** TLB maintenance (break-before-make), cache coherency, register constraints in asm, exception level transitions
7. **Suppressed warnings:** Every `#[allow(...)]` attribute must be justified or removed

### Special Audit Targets

- **Every `unsafe` block:** Enumerate all unsafe blocks per file. For each one, verify the safety invariant is documented and actually holds. Flag any unsound unsafe blocks.
- **link.ld:** Audit for symbol ordering, section alignment, missing KEEP directives, guard page placement.
- **Cross-file invariants:** After per-file audits, audit cross-module assumptions: lock ordering across modules, lifetime guarantees across subsystems (e.g., handle.rs <-> channel.rs, scheduler.rs <-> thread_exit.rs).

### Known Issues (do not re-discover)

- `memory_region.rs:23` has a TODO for enforcing `readable=false` for no-access guard mappings. This should be addressed during the audit.
- QMP `input-send-event` not routing to virtio-keyboard is a QEMU limitation, not a kernel bug.
- The kernel recently had 11 bugs fixed (aliasing UB, nomem on DAIF asm, deferred thread drop, idle thread park, etc.). The audit should verify these fixes are complete and look for any related issues that may have been missed.

### Milestones and Features

**Milestone 0: Test Infrastructure Verification** (no code changes)

- Feature 0: Verify `cd system/test && cargo test` runs and all 304 tests pass. Verify `cd system && cargo build` succeeds. Document exact commands and any environment requirements. Check if any tests can run under Miri (`cargo +nightly miri test`) for automatic UB detection.

**Milestone 1: Boot + Memory Foundation** (10 files: boot.S, exception.S, link.ld, main.rs, memory.rs, paging.rs, page_allocator.rs, memory_region.rs, memory_mapped_io.rs, heap.rs, slab.rs)

- Feature 1: Audit boot.S + exception.S + link.ld for AArch64 correctness (register clobbers, barrier ordering, TLB invalidation, EL transitions, linker section alignment)
- Feature 2: Audit memory.rs + paging.rs + page_allocator.rs (overflow, alignment, W^X enforcement, off-by-one in frame ranges)
- Feature 3: Audit heap.rs + slab.rs + memory_region.rs + memory_mapped_io.rs (allocator routing, double-free, fragmentation, MMIO safety, the readable=false TODO)

**Milestone 2: Process + Address Space** (6 files: process.rs, process_exit.rs, address_space.rs, address_space_id.rs, executable.rs, context.rs)

- Feature 4: Audit process.rs + process_exit.rs (resource cleanup completeness, handle drainage, ASID release)
- Feature 5: Audit address_space.rs + address_space_id.rs + executable.rs + context.rs (VMA edge cases, ASID exhaustion, ELF parsing bounds, context save/restore completeness)

**Milestone 3: Scheduling + Threads** (6 files: scheduler.rs, scheduling_algorithm.rs, scheduling_context.rs, thread.rs, thread_exit.rs, per_core.rs)

- Feature 6: Audit scheduler.rs + scheduling_algorithm.rs + scheduling_context.rs (EEVDF correctness, vruntime overflow, priority inversion)
- Feature 7: Audit thread.rs + thread_exit.rs + per_core.rs (lifecycle races, drop ordering, per-core state isolation under SMP)

**Milestone 4: Synchronization + IPC** (5 files: sync.rs, futex.rs, waitable.rs, channel.rs, handle.rs)

- Feature 8: Audit sync.rs + futex.rs + waitable.rs (deadlock potential, spurious wakeup handling, missing barriers)
- Feature 9: Audit channel.rs + handle.rs (shared memory safety, handle lifecycle, cross-process correctness)

**Milestone 5: Hardware + Syscall Interface** (8 files: interrupt.rs, interrupt_controller.rs, timer.rs, device_tree.rs, serial.rs, power.rs, metrics.rs, syscall.rs)

- Feature 10: Audit interrupt.rs + interrupt_controller.rs + timer.rs (GIC configuration, timer overflow, interrupt masking during critical sections)
- Feature 11: Audit device_tree.rs + serial.rs + power.rs + metrics.rs (parsing edge cases, MMIO safety, shutdown paths)
- Feature 12: Audit syscall.rs (input validation, privilege escalation vectors, error propagation, the recently-fixed aliasing patterns)

**Milestone 6: Cross-File Invariants**

- Feature 13: Audit cross-module lock ordering (map all locks, verify consistent acquisition order across call paths, check for deadlock potential under SMP)
- Feature 14: Audit cross-module lifetime/ownership assumptions (handle table <-> channel, scheduler <-> thread_exit, process_exit <-> address_space, timer <-> scheduler)

**Milestone 7: Adversarial Stress Test Suite + Miri**

- Feature 15: Run any Miri-compatible tests under Miri for automatic UB detection. Fix any findings.
- Feature 16: Design and implement new stress/fuzz tests targeting all findings from milestones 1-6
- Feature 17: Run full test suite (existing 304 + new adversarial tests) and verify all pass

### Workflow per Feature

1. Read the file(s) thoroughly
2. Enumerate all `unsafe` blocks and `#[allow(...)]` attributes
3. Apply the 7-category bug checklist
4. For each finding: write a failing test first (TDD)
5. Fix the bug
6. Verify the test passes
7. Verify no existing tests regressed
8. Commit with descriptive message

### Testing

- Host test suite: `cd system/test && cargo test` (runs on macOS, tests kernel logic in isolation)
- Cross-compilation: `cd system && cargo build` (builds for `aarch64-unknown-none`)
- Miri (if available): `cd system/test && cargo +nightly miri test` (automatic UB detection, may not work for all tests)
- Stress tests: `./stress-test.sh 30` (headless, no display needed)
- Crash test: `./crash-test.sh 120` (needs QEMU + display)

### Key Context

- `system/kernel/DESIGN.md` has architectural rationale for every subsystem (1462 lines)
- Recent session (2026-03-11) fixed 11 bugs including aliasing UB in syscall dispatch, deferred thread drop use-after-free, idle thread park bug, nomem on DAIF asm
- The project already has an adversarial fuzzer at `system/user/fuzz/` and stress test at `system/user/stress/`
- 27 syscalls, 4 SMP cores, EEVDF scheduler, QEMU `virt` target
- code-reviewer and security-reviewer are available as custom droids
- The kernel uses `#![no_std]` bare-metal Rust, `alloc` crate, no external dependencies
