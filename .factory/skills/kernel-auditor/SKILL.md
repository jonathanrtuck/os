---
name: kernel-auditor
description: Audits kernel source files for bugs using a comprehensive checklist, writes failing tests first (TDD), then fixes.
---

# Kernel Auditor

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

Use for any feature that involves auditing kernel source files against the bug category checklist. This covers per-file audits, cross-file invariant analysis, Miri runs, and adversarial test creation.

## Work Procedure

### Step 1: Read the Feature Description

Read the assigned feature from `features.json`. Note which files to audit and which bug categories are most relevant.

### Step 2: Read Architectural Context

Read `system/kernel/DESIGN.md` sections relevant to the files being audited. Understand the design intent before judging correctness.

### Step 3: Read Each File Thoroughly

For each file in scope:

- Read the entire file
- Enumerate every `unsafe` block and `unsafe fn`
- For each unsafe block, identify the safety invariant it relies on
- Check if the invariant is documented in a `// SAFETY:` comment
- Determine if the invariant actually holds

**IMPORTANT:** After reading each file, add an `// AUDIT:` comment at the top of the file (or near the first `unsafe` block) confirming the audit was performed. Example: `// AUDIT: 2026-03-11 — 21 unsafe blocks verified, 6-category checklist applied. No bugs found.`

### Step 4: Apply the Bug Category Checklist

For each file, systematically check all 6 categories:

1. **Memory safety:** Look for aliasing UB (multiple `&mut` to same data), uninitialized reads, transmute soundness, buffer overflows, integer overflow in pointer arithmetic
2. **Concurrency:** Check for data races (shared mutable state without locks), missing memory barriers (DMB/DSB/ISB after MMIO or TLB ops), lock ordering violations, interrupt-safety (code that runs with interrupts both enabled and disabled)
3. **Error handling:** Find unchecked `.unwrap()` or `.expect()` on fallible operations in non-panic paths, missing OOM handling, error codes that could be silently dropped
4. **Edge cases:** Test boundary conditions — zero-length inputs, maximum values (u64::MAX), off-by-one in ranges, empty collections
5. **Resource leaks:** Missing Drop impls, pages allocated but never freed on error paths, handles not closed, ASID not released
6. **AArch64 correctness:** TLB invalidation after page table changes (break-before-make), cache maintenance for DMA, correct register constraints in inline asm, proper EL transition sequences

### Step 5: Write Failing Tests First (TDD)

For EVERY **code bug** finding:

1. Write a test that demonstrates the bug (the test must FAIL before the fix)
2. Add the test to the appropriate file in `system/test/tests/`

**Note:** TDD does not apply to documentation-only changes (adding `// SAFETY:` comments, `// AUDIT:` markers). Just add them directly.

**CRITICAL: Test architecture.** Tests in `system/test/` cannot import kernel modules directly (the kernel targets `aarch64-unknown-none`, tests target the host). Tests must duplicate/stub the pure logic they need. Follow existing patterns — read similar test files first.

Run the test to confirm it fails: `cd system/test && cargo test <test_name> -- --test-threads=1`

### Step 6: Fix the Bug

Apply the minimal correct fix. Prefer fixes that:

- Preserve the existing API/behavior
- Add safety comments explaining the invariant
- Handle errors explicitly rather than panicking

### Step 7: Verify

1. Run the new test to confirm it passes: `cd system/test && cargo test <test_name> -- --test-threads=1`
2. Run the full test suite to check for regressions: `cd system/test && cargo test -- --test-threads=1`
3. Build the kernel to verify it still compiles: `cd system && cargo build`

### Step 8: Commit

Commit each logical fix with a descriptive message:

```
fix(<subsystem>): <what was wrong>

<brief explanation of the bug and why the fix is correct>
```

### Special Cases

**If no bugs are found in a file:** This is a valid outcome. Document it in the handoff: "Audited <file>, N unsafe blocks verified sound, no bugs found." Still add any missing `// SAFETY:` comments.

**For cross-file invariant features (milestone 6):** Instead of per-file audit, trace specific cross-cutting concerns (lock ordering, lifetime assumptions) across all relevant files. Create a document or code comments mapping the invariants.

**For Miri features:** Install Miri (`rustup component add miri`), then run `cd system/test && cargo +nightly miri test -- --test-threads=1 2>&1`. Not all tests may be Miri-compatible. Fix what Miri finds; mark incompatible tests with `#[cfg_attr(miri, ignore)]`.

**For stress test features:** Design tests that exercise audit findings under pressure. Follow patterns in existing `system/user/fuzz/` and `system/user/stress/`. New host-side stress tests go in `system/test/tests/`.

## Example Handoff

```json
{
  "salientSummary": "Audited memory.rs (20 unsafe blocks) and paging.rs (0 unsafe). Found 3 bugs: integer overflow in frame_range calculation at line 142, missing DSB after TLB invalidation in remap(), and unchecked page_count * PAGE_SIZE overflow. Wrote 3 failing tests, applied fixes, all 351 tests pass, kernel builds.",
  "whatWasImplemented": "Audited memory.rs and paging.rs against 6-category bug checklist. Enumerated all 20 unsafe blocks in memory.rs, verified soundness of 17, fixed 3. Added SAFETY comments to all blocks. Added 3 new test cases in system/test/tests/vma.rs.",
  "whatWasLeftUndone": "",
  "verification": {
    "commandsRun": [
      {
        "command": "cd system/test && cargo test test_frame_range_overflow -- --test-threads=1",
        "exitCode": 0,
        "observation": "New test passes after fix (failed before fix as expected)"
      },
      {
        "command": "cd system/test && cargo test -- --test-threads=1",
        "exitCode": 0,
        "observation": "351 tests passed, 0 failed (3 new tests added to 348 baseline)"
      },
      {
        "command": "cd system && cargo build",
        "exitCode": 0,
        "observation": "Kernel builds successfully"
      }
    ],
    "interactiveChecks": []
  },
  "tests": {
    "added": [
      {
        "file": "system/test/tests/vma.rs",
        "cases": [
          {
            "name": "test_frame_range_overflow",
            "verifies": "frame_range handles u64 overflow without panic"
          },
          {
            "name": "test_remap_requires_barrier",
            "verifies": "remap issues DSB after TLB invalidation"
          },
          {
            "name": "test_page_count_overflow",
            "verifies": "page_count * PAGE_SIZE checked for overflow"
          }
        ]
      }
    ]
  },
  "discoveredIssues": []
}
```

## When to Return to Orchestrator

- A finding requires changing a syscall API or public interface
- A bug fix in one file requires coordinated changes in files outside this feature's scope
- The test suite has pre-existing failures unrelated to this feature
- A finding is ambiguous — unclear whether the behavior is intentional or a bug (check DESIGN.md first)
- Miri reports issues in code outside kernel scope (libraries, test infrastructure)
