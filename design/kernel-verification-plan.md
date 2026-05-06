# Kernel Verification Plan

Every technique short of formal verification, applied to the full depth this
kernel deserves. No box-checking. Every item is designed to actually find bugs.

## Foundational Constraints

These are invariant and filter the space of acceptable implementations:

1. **ABI is settled.** 30 syscalls, their numbers, argument formats, and
   semantics are frozen. The question is not "is the ABI right?" but "does the
   implementation perfectly match the spec?"
2. **Framekernel boundary is settled.** `#![deny(unsafe_code)]` at crate root,
   `#[allow(unsafe_code)]` on `frame/` only. Enforced at compile time.
3. **Target hardware is M4 Pro**, running in the hypervisor. 128-byte cache
   lines, 16 KB pages, 14 cores (10P+4E), LSE atomics, 3-cycle L1D, 17-cycle L2,
   ~49ns SLC, ~97ns DRAM random access, 81ns full page walk.

## Optimization Objective

Correctness is the **filter**: from all possible implementations, discard every
one that has a bug, violates an invariant, or exhibits unwanted behavior.

Performance is the **objective function**: from the remaining correct
implementations, find and implement the one with the best possible performance
for this OS's anticipated workloads on M4 Pro silicon. Not the simpler option
with "good enough" performance.

The verification infrastructure exists to enable aggressive optimization — every
test, fuzzer, and checker is a safety net that lets us change the implementation
with confidence that correctness is preserved.

## Autonomy Protocol

This plan is designed for fully autonomous execution. No questions to the user.
No permission requests. No "here's what I found, what should I do?" Every
decision point has a default:

- **Spec edge case not addressed:** choose the safest correct behavior,
  implement it, document the reasoning in the spec, move on.
- **Ambiguous correctness question:** pick the stricter interpretation. False
  positives (rejecting valid input) are preferable to false negatives (accepting
  invalid input).
- **Performance vs complexity tradeoff:** choose performance. The plan already
  ensures correctness; within the correct implementations, optimize
  aggressively.
- **Dependency question (add a crate?):** add it if it's the right tool. Only
  dev-dependencies — the kernel binary has no new runtime deps.
- **Internal API change needed for optimization:** do it. The ABI is invariant;
  internals are not.
- **Bug found in the spec:** fix the spec AND the implementation. Document what
  changed and why.
- **Context exhaustion:** update progress file, commit everything, leave clear
  breadcrumbs. The next session reads the plan, reads the progress file, checks
  git log, and continues without asking anything.

## Continuity Protocol

Context will be exhausted before this plan is complete. The continuity
mechanism:

1. **Plan doc** (`design/kernel-verification-plan.md`): each sub-item gets
   marked `[DONE]` when complete. The plan is the source of truth for what's
   left.
2. **Progress file** (`design/kernel-verification-progress.md`): updated at
   every phase boundary and before every context exhaustion. Records: what was
   done, what was found, what was fixed, what's actively in progress, and what
   the next session should do first.
3. **Git history**: every meaningful change is a separate commit with a clear
   message. The next session can `git log --oneline` to see what happened.
4. **Memory**: cross-session decisions (spec edge cases resolved, optimization
   choices made, bugs found and their class) stored in memory files.
5. **Branch**: all work on `kernel-verification` branch. Merged to main when the
   entire plan is complete.

To resume: read this plan (check `[DONE]` markers), read the progress file, read
`git log kernel-verification`, continue from where it stopped.

## Current State (Baseline)

- **19,500 lines** of kernel code across 47 Rust files
- **524 tests** (unit + syscall-level + integration + pipeline + verification)
- **104 unsafe blocks**, all confined to `frame/` (framekernel discipline,
  `#![deny(unsafe_code)]` at crate root)
- **2 fuzz targets** (`syscall_single`, `syscall_sequence`) — basic, no
  invariant checking in harness, no structured input, no dictionary
- **Invariant checker** (`invariants.rs`) — 7 structural checks, runs after
  every syscall-level test
- **Verification suite** (`verification.rs`) — boundary values, failure paths,
  object lifecycles, generation revocation
- **Bare-metal integration tests** — hypervisor boot, serial output validation
- **Pre-commit gate** — clippy + full test suite + framekernel check
- **No property-based testing**, no Miri, no coverage measurement, no mutation
  testing, no sanitizers, no Loom, no structured error injection
- **One benchmark** (null syscall). No per-syscall performance data. No
  theoretical minimum analysis. Regression threshold is 10x (too loose).

---

## Phase 0: Adversarial Spec Review

**Goal:** Verify the spec itself before verifying the implementation against it.
A correct implementation of a buggy spec is a buggy kernel.

### 0.1 Syscall Interaction Matrix

For every pair of the 30 syscalls, ask: "what happens if syscall A is in
progress when syscall B executes on the same object?" This produces a 30×30
matrix. Most cells are trivially safe (independent objects), but the interesting
cells are:

- `call` × `endpoint_destroy` (caller blocked, endpoint destroyed)
- `event_wait` × `event_destroy` (waiter blocked, event destroyed)
- `thread_exit` × `reply` (thread exits while its reply is pending)
- `handle_close` × any syscall using that handle (concurrent use-after-close)
- `space_destroy` × any syscall in that space (rug-pull)
- `vmo_resize` × `vmo_map` (resize while mapping is in progress)
- `vmo_seal` × `vmo_resize` (seal while resize is in progress)
- `thread_set_priority` × `recv` (priority change during priority inheritance)

For each interesting cell: what does the spec say? What does the implementation
do? Do they match? If the spec is silent, define the behavior, implement it, and
document it.

### 0.2 State Machine Completeness

For each kernel object type, draw the complete state machine:

- **Thread:** Created → Ready → Running → Blocked (IPC/Event) → Ready → Exited
- **Endpoint:** Created → has pending calls → has recv waiters → destroyed
- **Event:** Created → bits set → waiters registered → destroyed
- **VMO:** Created → mapped → snapshotted → sealed → resized → destroyed
- **Handle:** Allocated → duplicated → rights-attenuated → closed

For each state machine:

- Is every transition defined?
- Are there unreachable states? (Dead code.)
- Are there absorbing states other than "destroyed"? (Potential hangs.)
- Can the object be destroyed from every non-destroyed state? (cleanup paths)
- What happens to threads blocked on a destroyed object? (Must be defined.)

### 0.3 Invariant Enumeration

Write down every invariant that must hold across the entire kernel lifetime. The
existing `invariants.rs` checks 7. The complete list should include:

1. Handle→object referential integrity (existing)
2. Generation-count consistency (existing)
3. Endpoint internal counts (existing)
4. Event internal counts (existing)
5. Thread-space linked list validity (existing)
6. Scheduler uniqueness (existing)
7. Thread state consistency (existing)
8. Mapping consistency (existing — added later)
9. **Object reference counting** — every object's reference count equals the
   number of handles pointing to it across all spaces
10. **No orphaned objects** — every object is reachable from at least one handle
    or is in the process of being destroyed
11. **Scheduler completeness** — every non-exited, non-blocked thread is in
    exactly one run queue
12. **IPC protocol** — every thread in BlockedOnCall has a matching PendingCall
    in some endpoint; every thread in BlockedOnRecv is registered on some
    endpoint's recv list
13. **Event waiter validity** — every registered waiter references a thread that
    is in BlockedOnEvent state
14. **VMO mapping validity** — every mapping references a VMO that is alive and
    has sufficient size for the mapped range
15. **Priority inheritance consistency** — if priority inheritance is active,
    the inherited priority is the max of all blocked callers' priorities

Add each missing invariant to `invariants.rs`. These are the properties that
every subsequent phase (property testing, fuzzing, error injection) will verify.

### 0.4 Error Code Audit

For every syscall, enumerate every possible error return. Verify:

- The spec documents it
- The implementation returns it
- A test triggers it
- No undocumented error codes are returned

---

## Phase 1: Unsafe Audit

**Goal:** Every unsafe block has a verified safety invariant. Every invariant is
either tested or mechanically enforced.

### 1.1 Exhaustive Unsafe Inventory

Produce a catalog of every `unsafe` block in `frame/`. For each:

| Field            | Description                                           |
| ---------------- | ----------------------------------------------------- |
| Location         | `file:line`                                           |
| Operation        | What the unsafe operation is (raw pointer deref, asm, |
|                  | FFI, `Sync`/`Send` impl, `from_raw_parts`, etc.)      |
| Safety invariant | The precondition that makes this sound                |
| Violation mode   | What specific UB occurs if the invariant breaks       |
| Enforcement      | How the invariant is upheld (type system, runtime     |
|                  | check, caller contract, hardware guarantee)           |
| Testable?        | Can we write a test that would catch a violation?     |
| Miri-eligible?   | Can this code path run under Miri on the host?        |

**Files to audit (by unsafe density, highest first):**

1. `frame/arch/aarch64/exception.rs` — 15 unsafe (trap entry/exit, register
   save/restore, COPY_FAULT_RECOVERY)
2. `frame/arch/aarch64/cpu.rs` — 14 unsafe (PerCpu access, TPIDR_EL1,
   CoreStacks)
3. `frame/arch/aarch64/sysreg.rs` — 13 unsafe (MSR/MRS inline asm for system
   registers: DAIF, TTBR, TPIDR, timers, CPACR, VBAR, TCR, MAIR)
4. `frame/arch/aarch64/mmu.rs` — 13 unsafe (page table walks, TLB invalidation,
   TTBR writes)
5. `frame/arch/aarch64/sync.rs` — 12 unsafe (spinlock, atomic operations, memory
   barriers)
6. `frame/user_mem.rs` — 11 unsafe (LDTR/STTR user-space memory access, fault
   recovery, host-mode raw pointer derefs)
7. `frame/arch/aarch64/page_table.rs` — 7 unsafe (page table entry construction,
   physical-to-virtual address conversion)
8. `frame/arch/aarch64/context.rs` — 4 unsafe (context switch, register
   save/restore)
9. `frame/arch/aarch64/mod.rs` — 3 unsafe
10. `frame/arch/aarch64/mmio.rs` — 3 unsafe (volatile reads/writes to device
    registers)
11. `frame/firmware/dtb.rs` — 2 unsafe (`from_raw_parts` on DTB blob)
12. `frame/fault_resolve.rs` — 2 unsafe (physical page access for COW/lazy)
13. `frame/arch/aarch64/psci.rs` — 2 unsafe (HVC calls for power management)
14. `frame/mod.rs` — 1 unsafe
15. `frame/heap.rs` — 1 unsafe (global allocator setup)
16. `frame/arch/aarch64/register_state.rs` — 1 unsafe

### 1.2 SAFETY Comment Verification

For every block in the inventory:

- If the SAFETY comment is missing: **add it**.
- If the SAFETY comment exists: **re-verify it against the actual code**. Does
  the stated invariant actually hold? Has a subsequent edit invalidated it?
- Cross-reference `asm!` blocks against the ARM Architecture Reference Manual:
  - Every `options(nomem)` must be justified — does the instruction truly not
    access memory? (Per CLAUDE.md: `msr`, `dsb`, `isb`, `hvc`, `tlbi` must NOT
    have `nomem`.)
  - Every `options(nostack)` must be verified — does the asm clobber the stack?
  - Register clobbers must be complete — unclobbered registers that the asm
    modifies silently corrupt Rust state.

### 1.3 Unsafe Verification and Optimization

For each block, ask: **is this the fastest correct implementation for M4 Pro?**

The unsafe code exists because it does something that safe Rust cannot express
(inline asm, raw hardware access) or because the safe alternative is slower
(bounds checks on a hot path, abstraction overhead on MMIO). The goal is not to
eliminate unsafe — it's to verify that each unsafe block is both correct AND
optimal.

For each block:

1. **Is the safe alternative equally fast?** If yes, prefer safe (fewer
   invariants to maintain). If no, keep unsafe and verify thoroughly.
2. **Is this the fastest correct sequence for M4 Pro specifically?**
   - Does the inline asm use the optimal instruction sequence? (e.g., LSE
     atomics instead of LL/SC loops, STP/LDP pairs instead of single STR/LDR for
     16-byte aligned data on 128-byte cache lines)
   - Does the memory access pattern respect M4 Pro's 128-byte cache line size?
     (e.g., is the LDTR/STTR loop in `user_mem.rs` copying 8 bytes at a time
     when it could copy 16 with LDP/STP pairs?)
   - Are barriers (DMB/DSB/ISB) at the minimum strength required? (e.g.,
     `dmb ishld` instead of `dmb ish` when only load ordering is needed)
3. **Does the code exploit M4 Pro features?**
   - LSE2 for single-copy-atomic 128-bit operations
   - FEAT_WFxT for bounded spin waits
   - Cache line size awareness in struct layout (hot fields in one 128-byte
     line, cold fields in another)

Each change is a standalone commit. The invariant checker + test suite runs
before and after — correctness is the gate, performance is the objective.

---

## Phase 2: Property-Based Testing (Proptest)

**Goal:** Test invariants over the _space of inputs_, not individual examples.
Hand-written tests check the cases you thought of. Property tests check the
cases you didn't.

### 2.1 Dependency Setup

Add to `kernel/Cargo.toml`:

```toml
[dev-dependencies]
proptest = "1"
```

### 2.2 State Machine Properties

These are the highest-value property tests because they exercise multi-step
sequences that hand-written tests rarely cover.

**Kernel state machine model:**

Define an `Arbitrary`-derivable enum of all kernel operations:

```rust
enum SyscallOp {
    VmoCreate { size: usize, flags: u32 },
    VmoMap { vmo_handle: HandleRef, offset: usize, size: usize },
    VmoSnapshot { vmo_handle: HandleRef },
    VmoSeal { vmo_handle: HandleRef },
    VmoResize { vmo_handle: HandleRef, new_size: usize },
    EndpointCreate,
    EventCreate { initial_bits: u64 },
    EventSignal { event_handle: HandleRef, bits: u64 },
    EventWait { event_handle: HandleRef, mask: u64 },
    EventClear { event_handle: HandleRef, bits: u64 },
    ThreadCreate { entry: usize, stack: usize },
    ThreadExit,
    SpaceCreate,
    HandleDup { handle: HandleRef, new_rights: u32 },
    HandleClose { handle: HandleRef },
    // IPC operations (call/recv/reply) tested via dedicated IPC property tests
}
```

Where `HandleRef` is a small index (0..8) that maps to the nth valid handle in
the test's tracking state, so proptest generates _valid-shaped_ sequences rather
than mostly-invalid garbage.

**Properties to verify after every operation sequence:**

1. **Referential integrity** — every handle points to a live object with
   matching generation (already checked by `invariants::verify`, integrate it)
2. **No resource leaks** — the sum of live objects equals the number of
   successful creates minus successful destroys
3. **Handle table monotonicity** — closing a handle makes that specific slot
   reusable; it never invalidates other handles
4. **Rights attenuation** — a duplicated handle's rights are always a subset of
   the source handle's rights
5. **Generation revocation** — after an object is destroyed and its slot reused,
   handles with the old generation fail with `InvalidHandle`, not silently
   access the new object
6. **Idempotent close** — closing an already-closed handle returns
   `InvalidHandle`, never panics, never corrupts
7. **Capacity limits** — creating objects beyond `MAX_*` returns `OutOfMemory`,
   never panics, never corrupts existing state

### 2.3 Per-Object Property Tests

**VMO properties:**

- `size(snapshot(v)) == size(v)` — snapshots preserve size
- `seal(v); resize(v, _) == Err(Sealed)` — sealed VMOs reject resize
- `snapshot(v)` yields independent copy — write to original doesn't affect
  snapshot, write to snapshot doesn't affect original
- `resize(v, smaller)` then `map(v, offset_in_trimmed_region)` — maps still work
  for valid offsets, fail for truncated offsets

**Event properties:**

- `signal(e, bits); bits(e) == old_bits | bits` — signal is OR-accumulative
- `clear(e, bits); bits(e) == old_bits & !bits` — clear is AND-NOT
- `signal` + `clear` are commutative within the same mask
- Multi-wait with N events: if exactly one fires, returned handle matches the
  fired event; if none fire, thread blocks; if multiple fire, one is returned

**Handle table properties:**

- After `alloc` succeeds, the handle is retrievable with correct type/rights
- After `close`, the handle is not retrievable
- `dup` with `rights & ~source_rights != 0` fails (can't escalate)
- Capacity: after `MAX_HANDLES` allocs, next alloc fails; after one close, next
  alloc succeeds

**Endpoint (IPC) properties:**

- `call` without a receiver blocks the caller
- `recv` without a pending call blocks the receiver
- After `call` + `recv` + `reply`: caller unblocks with reply data, reply cap is
  consumed (double-reply fails)
- Message data integrity: data sent == data received (byte-for-byte)
- Handle transfer integrity: transferred handles are removed from sender,
  installed in receiver with correct type/rights
- Priority inheritance: receiver inherits max(own priority, caller priority)

**Scheduler properties:**

- Every `Ready` thread is in exactly one run queue
- Every `Blocked` thread is in zero run queues
- Every `Running` thread is `current` on exactly one core
- `yield` from a single-thread system: thread remains running (no panic)
- Priority ordering: higher-priority ready threads are scheduled before lower

### 2.4 Boundary Value Generators

Custom `proptest` strategies for values that cluster at boundaries:

```rust
fn boundary_size() -> impl Strategy<Value = usize> {
    prop_oneof![
        Just(0),                              // zero
        Just(1),                              // minimum
        Just(PAGE_SIZE - 1),                  // just under page boundary
        Just(PAGE_SIZE),                      // exact page
        Just(PAGE_SIZE + 1),                  // just over page boundary
        Just(MAX_PHYS_MEM),                   // maximum
        Just(MAX_PHYS_MEM + 1),               // overflow
        Just(usize::MAX),                     // u64 max
        Just(usize::MAX - PAGE_SIZE + 1),     // near-overflow
        1..=(PAGE_SIZE * 4),                  // small range
    ]
}

fn boundary_u64() -> impl Strategy<Value = u64> {
    prop_oneof![
        Just(0),
        Just(1),
        Just(u32::MAX as u64),       // 32-bit boundary
        Just(u32::MAX as u64 + 1),   // bit 32
        Just(u64::MAX),              // all bits
        Just(1u64 << 63),            // sign bit
        0..=u64::MAX,                // full range
    ]
}
```

### 2.5 Regression Corpus

Every property test failure generates a minimal failing case. These are
persisted in `kernel/proptest-regressions/` and run as part of the normal test
suite. The corpus grows over time — each bug found becomes a permanent
regression test.

---

## Phase 3: Fuzzing Overhaul

**Goal:** Transform the existing fuzz targets from "check it doesn't crash" to
"check it maintains every invariant across every reachable state."

### 3.1 Invariant-Checking Fuzz Harness

The existing fuzz targets only assert `error <= 12`. This catches panics but
misses silent corruption. Replace with:

```rust
fuzz_target!(|data: &[u8]| {
    let ops = parse_operations(data);
    let mut k = setup_kernel();
    let mut tracker = HandleTracker::new();

    for op in ops {
        let (err, val) = execute_op(&mut k, &mut tracker, &op);
        // Error code must be valid
        assert!(err <= MAX_ERROR_CODE);
        // If success, track the result
        if err == 0 { tracker.record_success(&op, val); }
    }

    // THE KEY: verify all invariants after the sequence
    let violations = invariants::verify(&k);
    assert!(violations.is_empty(), "{:?}", violations);

    // Verify our tracker matches reality
    tracker.verify_against_kernel(&k);
});
```

### 3.2 Structured Fuzzing with Arbitrary

Instead of interpreting raw bytes as u64s (which generates ~97% invalid syscall
numbers), implement `Arbitrary` for a structured operation type. This makes the
fuzzer spend its time exploring _meaningful_ syscall sequences instead of mostly
hitting the `_ => InvalidArgument` default arm.

Use `arbitrary` crate (libfuzzer-compatible) instead of proptest's strategy:

```rust
#[derive(Debug, Arbitrary)]
enum FuzzOp {
    VmoCreate(u16, u8),          // size_pages, flags
    VmoMap(u8, u16, u16),        // handle_idx, offset, size
    EndpointCreate,
    EventCreate(u64),
    EventSignal(u8, u64),        // handle_idx, bits
    HandleDup(u8, u8),           // handle_idx, rights_mask
    HandleClose(u8),             // handle_idx
    Call(u8, u8, [u8; 16]),      // ep_handle, msg_len, msg_prefix
    // ... all 30 syscalls represented
}
```

### 3.3 Multi-Thread Fuzz Target

Add a fuzz target that creates multiple threads across multiple address spaces,
then interleaves operations between them. This finds bugs in:

- Handle table isolation between spaces
- Thread lifecycle (create in one space, exit, another space's handles still
  valid)
- Scheduler state after thread exit (no dangling references in run queues)
- IPC across spaces (call from space A to endpoint in space B)

### 3.4 Dictionary and Seed Corpus

**Dictionary (`kernel/fuzz/syscall.dict`):**

Provide the fuzzer with known-interesting byte patterns:

- All 30 valid syscall numbers (as little-endian u64)
- Common rights masks
- Page-boundary sizes
- Boundary u64 values (0, 1, u32::MAX, u64::MAX)

**Seed corpus (`kernel/fuzz/corpus/`):**

Extract syscall sequences from existing passing tests. Each test that uses
`dispatch()` becomes a seed. The fuzzer mutates from working sequences rather
than random bytes — dramatically faster coverage convergence.

### 3.5 Coverage-Guided Corpus Management

- Run `cargo fuzz` with `--sanitizer=none` initially for speed, then
  `--sanitizer=address` for memory bugs
- Use `cargo fuzz coverage` to generate LLVM coverage data
- Identify uncovered code paths → add targeted seeds
- Set minimum runtime: 24-hour initial run, then nightly 1-hour runs
- Track corpus size and coverage percentage over time

### 3.6 Crash Triage Protocol

When the fuzzer finds a crash:

1. Minimize: `cargo fuzz tmin <target> <crash_artifact>`
2. Analyze: what sequence of operations triggered it?
3. Write a deterministic test that reproduces it
4. Fix the bug
5. Add the minimized input to the seed corpus (so the fuzzer can explore
   variations of the pattern that led to this bug)
6. Check: does the same class of bug exist in similar code?

---

## Phase 4: Miri

**Goal:** Use Rust's interpreter-level UB detector on every code path that can
run on the host target.

### 4.1 What Miri Can Check

Miri detects undefined behavior that the compiled binary might "work" with today
but break on a different optimization level, compiler version, or target.
Specifically:

- Use-after-free
- Out-of-bounds memory access
- Uninitialized memory reads
- Invalid pointer alignment
- Data races (with `-Zmiri-check-data-races`)
- Integer overflow (with `-Zmiri-overflow-checks`)
- Stacked borrows violations (aliasing rules)
- Dangling references
- Memory leaks (with `-Zmiri-leak-check`)

### 4.2 What Miri Cannot Check (in this kernel)

- Inline assembly (all `asm!` blocks) — Miri can't interpret ARM instructions
- Hardware MMIO — no device registers on the host
- Bare-metal-only code paths (`#[cfg(target_os = "none")]`)

But: the framekernel discipline already isolates all of these in `frame/`.
Everything outside `frame/` is safe Rust. The `#[cfg(not(target_os = "none"))]`
host stubs in `user_mem.rs` _are_ Miri-eligible and exercise the same logic
paths as the bare-metal code (minus the actual LDTR/STTR instructions).

### 4.3 Miri Test Execution

```bash
# Run all host-target tests under Miri
cargo +nightly miri test -p kernel --target aarch64-apple-darwin

# With leak checking and race detection
MIRIFLAGS="-Zmiri-leak-check -Zmiri-check-data-races" \
    cargo +nightly miri test -p kernel --target aarch64-apple-darwin
```

### 4.4 Miri-Specific Test Additions

Write tests that specifically target Miri's strengths:

- **Stacked borrows stress:** create and destroy objects in patterns that
  exercise the `ObjectTable`'s internal storage — alloc/dealloc/realloc
  sequences that might create aliasing violations
- **user_mem host paths:** the `#[cfg(not(target_os = "none"))]`
  `copy_from_user` / `copy_to_user` do raw pointer derefs — Miri can verify
  these are sound
- **Handle table aliasing:** get a `&Handle` reference, then do an operation
  that modifies the table (close, alloc) — verify no dangling reference
- **VMO page storage:** exercise the inline→heap overflow transition in VMO page
  arrays

### 4.5 Makefile Target

```makefile
miri:
  MIRIFLAGS="-Zmiri-leak-check" \
      cargo +nightly miri test -p kernel --target aarch64-apple-darwin
```

---

## Phase 5: Coverage Measurement

**Goal:** Know exactly which lines, branches, and functions are exercised by
tests. Not as a vanity metric — as a map of what's untested.

### 5.1 Source-Based Coverage (LLVM)

```bash
# Build with coverage instrumentation
RUSTFLAGS="-C instrument-coverage" \
    cargo test -p kernel --lib --target aarch64-apple-darwin

# Generate coverage report
grcov . -s kernel/src --binary-path target/aarch64-apple-darwin/debug \
    -t html --branch -o coverage/
```

### 5.2 Coverage Targets

Not "80% and done." Instead, categorize uncovered code by risk:

| Category                        | Coverage Target  | Why                         |
| ------------------------------- | ---------------- | --------------------------- |
| Syscall handlers (`syscall.rs`) | 100% line+branch | The entire attack surface   |
| Object operations (VMO, Event,  | 100% line+branch | Core correctness            |
| Endpoint, Thread, Handle)       |                  |                             |
| Invariant checker               | 100% line        | It must check everything    |
| Error paths                     | 100% line        | Error handling bugs are the |
|                                 |                  | majority of security vulns  |
| Scheduler (`sched.rs`)          | 100% line+branch | Subtle state machine        |
| Fault resolution                | 95%+ line        | Some paths need bare-metal  |
| Bootstrap                       | 80%+ line        | Some paths are boot-only    |
| Config / types                  | 60%+ line        | Mostly constants            |

### 5.3 Branch Coverage Analysis

Line coverage hides bugs in branches. Specifically measure:

- Every `match` arm in `dispatch()` — all 30 syscalls + the default arm
- Every `if let` / `match` on `Result` — both `Ok` and `Err` paths
- Every early-return guard — both the return and the fall-through
- Capacity limit checks — both under-limit and at-limit paths

### 5.4 Uncovered Code → Test or Delete

For every uncovered line:

1. **Is it reachable?** If not, delete it (dead code is a liability).
2. **Is it an error path?** Write a test that triggers it.
3. **Is it a bare-metal-only path?** Mark it explicitly and verify it's covered
   by integration tests or has a host-target stub.
4. **Is it a defensive check that "can't happen"?** Either prove it can't happen
   (and delete the check) or write a test that triggers it (because if you can't
   trigger it, you can't verify the check works).

---

## Phase 6: Mutation Testing

**Goal:** Verify that tests actually detect bugs, not just exercise code.
Coverage tells you what runs; mutation testing tells you what's _tested_.

### 6.1 Setup

```bash
cargo install cargo-mutants
```

### 6.2 Execution

```bash
# Run mutation testing on the kernel
cargo mutants -p kernel --test-target aarch64-apple-darwin -- --lib
```

### 6.3 What Mutation Testing Reveals

`cargo-mutants` modifies the source code (replaces `+` with `-`, `true` with
`false`, deletes function bodies, changes return values) and checks whether any
test fails. A **surviving mutant** means a mutation was introduced and no test
caught it. This is more informative than coverage:

- A function can have 100% line coverage but 0% mutation kill rate if the tests
  only check for "no panic" and never verify the return value.
- The fuzz target's `assert!(error <= 12)` will have many surviving mutants
  because it doesn't check _which_ error code is returned.

### 6.4 Surviving Mutant Triage

For each surviving mutant:

1. **Is the mutation semantically equivalent?** (e.g., replacing `>=` with `>`
   where the value is always an integer — the boundary case doesn't arise) →
   Skip, but note the assumption.
2. **Does the mutation change observable behavior?** → Write a test that
   observes it.
3. **Is the mutation in an error path?** → Write a test that triggers the error
   path and verifies the exact error code.

### 6.5 Priority Order

Run mutation testing on files in order of criticality:

1. `syscall.rs` — the entire syscall dispatch surface
2. `handle.rs` — capability enforcement
3. `endpoint.rs` — IPC correctness
4. `event.rs` — synchronization correctness
5. `vmo.rs` — memory object semantics
6. `thread.rs` — thread lifecycle and scheduling
7. `sched.rs` — scheduler correctness
8. `address_space.rs` — VA allocator, mapping management
9. `table.rs` — object table (foundation for everything)
10. `irq.rs` — interrupt-to-event bridge
11. `fault.rs` — fault resolution
12. `bootstrap.rs` — init environment setup

---

## Phase 7: Sanitizers

**Goal:** Catch memory errors and concurrency bugs at runtime, even in
"correct-looking" code.

### 7.1 AddressSanitizer (ASan)

Detects out-of-bounds access, use-after-free, double-free, buffer overflow.
Particularly valuable for the `user_mem.rs` host stubs which do raw pointer
arithmetic.

```bash
RUSTFLAGS="-Z sanitizer=address" \
    cargo +nightly test -p kernel --lib --target aarch64-apple-darwin
```

### 7.2 LeakSanitizer (LSan)

Detects memory leaks. In a kernel, every allocation must be freed on the
corresponding destroy path. LSan verifies that `space_destroy`, `handle_close`,
`thread_exit` actually free everything.

```bash
RUSTFLAGS="-Z sanitizer=leak" \
    cargo +nightly test -p kernel --lib --target aarch64-apple-darwin
```

### 7.3 UndefinedBehaviorSanitizer (UBSan)

Catches integer overflow, null pointer dereference, misaligned access.

```bash
RUSTFLAGS="-Z sanitizer=undefined" \
    cargo +nightly test -p kernel --lib --target aarch64-apple-darwin
```

### 7.4 Fuzz Under Sanitizers

Run the fuzz targets under ASan — this is where the real bugs hide:

```bash
cargo +nightly fuzz run syscall_sequence -- \
    -max_len=4096 -runs=1000000 \
    -dict=fuzz/syscall.dict
```

(`cargo-fuzz` uses ASan by default. Make sure it's not disabled.)

---

## Phase 8: Concurrency Verification

**Goal:** Prove the scheduler, synchronization primitives, and multi-core code
paths are free of data races, deadlocks, and priority inversion.

### 8.1 Loom (Concurrency Model Checker)

Loom exhaustively explores all possible thread interleavings for small
concurrent programs. It's feasible for:

- **Spinlock correctness** (`frame/arch/aarch64/sync.rs`): verify mutual
  exclusion, no deadlock, progress guarantee
- **Per-CPU data access patterns**: verify no data race when core 0 reads PerCpu
  while core 1 writes its own PerCpu
- **Scheduler operations**: enqueue/dequeue/yield under concurrent access

Loom can't test 19K lines, but it _can_ exhaustively verify the ~200 lines of
synchronization primitives that everything else depends on. Extract the sync
primitives into a standalone module with a Loom-compatible abstraction layer.

### 8.2 Multi-Core Scheduler Stress Tests

Write tests that create the maximum number of threads across multiple address
spaces, assign them to different cores, and exercise:

- Rapid create/exit cycles (thread thrashing)
- All four priority levels simultaneously
- `yield` under load (all run queue slots full)
- `set_affinity` while the thread is running
- `set_priority` to trigger priority inheritance chains
- Thread exit while blocked on IPC

These already exist partially in the syscall tests. The gap: they run
single-core (core_id=0) even when testing multi-core scenarios. Create tests
that alternate `core_id` between dispatches to simulate actual multi-core
scheduling.

### 8.3 IPC Stress Tests

- N callers simultaneously calling the same endpoint (fill the priority ring)
- Caller calls → timeout → caller exits → server tries to reply (dangling)
- Server recv → reply → recv again (rapid turnaround)
- Handle transfer during endpoint destroy (race condition test)
- Call with full message (128 bytes) + max handles (8) — maximum-complexity IPC

### 8.4 Deadlock Detection

Static analysis of lock ordering:

- Enumerate every lock in the system (spinlocks in `sync.rs`)
- For each code path that acquires multiple locks, verify the acquisition order
  is consistent
- Document the lock ordering hierarchy
- Add a debug-mode lock order checker that records the current lock set per core
  and panics on out-of-order acquisition

### 8.5 Preemption Stress Testing

Force a timer interrupt at every possible point during every syscall. The kernel
must handle preemption at any instruction — if there's a window where state is
inconsistent (handle allocated but not yet installed, thread state updated but
scheduler not notified, pending call staged but not committed), a preemption at
that exact point will corrupt state.

On bare-metal: configure the timer to fire after a randomized 1-100 instruction
delay. Run each integration test scenario 100 times with randomized preemption
points. Check invariants after each run.

On host tests: simulate preemption by injecting `yield` points at every state
mutation in the kernel. This is less thorough than real timer interrupts but
catches the most common atomicity bugs — multi-step operations where the
intermediate state is visible to a concurrent observer.

This is the single most effective technique for finding atomicity bugs in kernel
code. Linux's `CONFIG_PREEMPT_DEBUG` + randomized timer has found hundreds of
such bugs.

### 8.6 Real SMP Bare-Metal Stress

Host tests simulate multi-core by alternating `core_id` between sequential
`dispatch()` calls. That tests interleaving, not concurrency. Real concurrency
bugs (torn reads, cache coherency races, interrupt delivery during context
switch) require actual simultaneous execution.

Bare-metal SMP stress test (runs in hypervisor):

- Boot all available cores (hypervisor configuration determines count)
- Each core runs a randomized loop: create objects, IPC to other cores'
  services, signal events, destroy objects
- After N iterations, all cores synchronize via a shared event and check
  invariants
- Run for 10,000 iterations per core
- Any panic, hang, or invariant violation is a failure

This requires extending the bare-metal test infrastructure to support multi-core
workloads. The hypervisor already supports multiple vCPUs.

---

## Phase 9: Structured Error Injection

**Goal:** Verify every error path actually works, not just that error paths
exist.

### 9.1 OOM Injection

The kernel allocates via `ObjectTable::alloc()`. Create a test mode that fails
allocation after N successful allocations. Then run each syscall with allocation
failure at every possible point:

```rust
fn test_vmo_create_oom_at_every_point() {
    for fail_at in 0..10 {
        let mut k = setup_kernel_with_oom_at(fail_at);
        let result = call(&mut k, VMO_CREATE, &[PAGE_SIZE as u64, 0, 0, 0, 0, 0]);
        // Either succeeds or returns OutOfMemory — never panics, never corrupts
        assert!(result.0 == 0 || result.0 == SyscallError::OutOfMemory as u64);
        invariants::assert_valid(&k);
    }
}
```

### 9.2 Capacity Exhaustion

For every object type, fill to `MAX_*`:

- Create `MAX_VMOS` VMOs, then try to create one more → `OutOfMemory`
- Close one, create one → succeeds
- Verify all previously created objects are still accessible
- Fill `MAX_HANDLES`, try to dup → `OutOfMemory`
- Fill `MAX_THREADS`, try to create → `OutOfMemory`

Some of these exist in `verification.rs`. Systematize them: every object type,
every allocation path, verified with invariant checker.

### 9.3 Partial Failure Rollback

Syscalls that perform multiple allocations (e.g., `thread_create_in` allocates a
thread + handles + schedules) must roll back cleanly if any step fails:

- For each multi-step syscall, identify every allocation point
- Inject failure at each point
- Verify the kernel state is identical to before the syscall (no partial state)
- Use the invariant checker to verify no orphaned objects

### 9.4 Input Boundary Injection

For every syscall argument that is a size, offset, or count:

- 0, 1, MAX-1, MAX, MAX+1, usize::MAX
- For pointers: 0 (null), 1 (misaligned), valid, usize::MAX
- For handle IDs: 0 (valid range start), MAX_HANDLES-1 (valid range end),
  MAX_HANDLES (just past valid), u32::MAX
- For rights: 0 (no rights), ALL, individual bit flags, all combinations of two
  flags, invalid bits set

---

## Phase 10: Static Analysis

**Goal:** Catch bugs at compile time that tests catch at runtime.

### 10.1 Clippy — Full Pedantic

Upgrade from `-D warnings` to pedantic + nursery lints that catch real bugs:

```bash
cargo clippy -p kernel -- \
    -D warnings \
    -W clippy::pedantic \
    -W clippy::nursery \
    -A clippy::module_name_repetitions \
    -A clippy::must_use_candidate \
    -W clippy::cast_possible_truncation \
    -W clippy::cast_sign_loss \
    -W clippy::cast_possible_wrap
```

The cast warnings are critical in a kernel — truncation of a `u64` to `u32` is a
common source of bugs when dealing with addresses and sizes.

### 10.2 Custom Deny Attributes

Add to `lib.rs`:

```rust
#![deny(unused_must_use)]    // Ignoring Results is a bug in a kernel
#![deny(unreachable_patterns)]
#![deny(unused_unsafe)]      // Unnecessary unsafe blocks rot
#![warn(missing_docs)]       // Undocumented public API is a design smell
```

### 10.3 cargo-audit / cargo-deny

Check dependencies for known vulnerabilities:

```bash
cargo audit
cargo deny check
```

Only two dependencies (`lock_api`, `talc`), but they must be verified.

### 10.4 Framekernel Discipline Verification

The existing `#![deny(unsafe_code)]` + `#[allow(unsafe_code)]` on `frame`
enforces the discipline at compile time. Verify:

- No other module has `#[allow(unsafe_code)]`
- `frame/` doesn't re-export unsafe functions as safe without proper
  precondition enforcement
- Every public function in `frame/` that has unsafe preconditions is either
  `unsafe fn` or enforces the preconditions internally

---

## Phase 11: Bare-Metal Verification and Performance Characterization

**Goal:** Verify the kernel behaves correctly on M4 Pro silicon (via the
hypervisor), measure cycle-accurate performance of every hot path, and establish
regression thresholds tight enough to catch real regressions without false
positives.

Host-target tests verify logic; bare-metal tests verify the system. This is the
primary verification environment, not a supplement to host testing.

### 11.1 Integration Test Expansion [DONE]

The current integration test boots the kernel and checks a single exit marker.
Expand to:

- **IPC round-trip:** init spawns a service, sends an IPC call, verifies the
  reply data matches
- **VMO mapping:** init maps a VMO, writes data, reads it back, verifies
  integrity
- **Multi-service:** init spawns N services, each performs IPC with init, all
  exit cleanly
- **Fault recovery:** init triggers a page fault (access unmapped memory),
  kernel resolves it (COW/lazy), init continues
- **Event notification:** init creates an event, signals it from a second
  thread, first thread waits and receives
- **Capacity limit:** init creates objects until `OutOfMemory`, verifies it can
  still operate (system isn't wedged)
- **Clean shutdown:** all threads exit, kernel issues PSCI SYSTEM_OFF, exit code
  0

### 11.2 Serial Output Verification [DONE]

Each integration test prints a structured line to serial:

```console
TEST test_name: PASS
TEST test_name: FAIL reason
```

The test script (`scripts/integration-test`) parses these lines and reports
per-test results, not just pass/fail for the whole boot.

### 11.3 Boot-Time Self-Test (POST) [DONE]

Before accepting userspace work, the kernel runs a smoke test at boot in debug
builds:

1. Create one of each object type (VMO, Endpoint, Event, Thread, AddressSpace)
2. Perform a null IPC round-trip (call → recv → reply)
3. Signal an event, wait for it
4. Map and unmap a VMO
5. Duplicate and close a handle
6. Destroy everything
7. Run `invariants::verify()` — must return zero violations

If anything fails, halt with a diagnostic serial message and PSCI SYSTEM_OFF.
This catches hardware-specific initialization bugs (page table misconfiguration,
GIC setup errors, PerCpu corruption) before they can corrupt userspace state.

Cost: ~10,000 cycles (~2µs at 4.5 GHz). Negligible compared to boot time.

Enabled by: `#[cfg(debug_assertions)]` — zero cost in release builds. Can
optionally be a `--self-test` boot flag for release builds.

### 11.4 Debug-Build Runtime Invariant Checking [DONE]

In debug bare-metal builds, run `invariants::verify()` after every syscall
dispatch — not just in `#[cfg(test)]`. This transforms bare-metal execution from
"check the exit code" to "check every structural invariant on every operation on
real hardware."

```rust
pub fn dispatch(&mut self, ...) -> (u64, u64) {
    let result = match syscall_num { ... };

    #[cfg(debug_assertions)]
    {
        let violations = crate::invariants::verify(self);
        if !violations.is_empty() {
            // Print violations to serial, halt
        }
    }

    result
}
```

This catches bugs that host tests can never find — timing-dependent state
corruption, interrupt-during-syscall races, hardware-specific memory ordering
effects. The cost is significant (full kernel state scan after every syscall),
which is why it's debug-only. But the bugs it catches are the ones that survive
everything else.

### 11.5 Stress Boot [DONE]

Boot the kernel 100 times in a loop. Any non-zero exit, hang (timeout), or
unexpected serial output is a failure. This catches initialization races, stack
corruption, and non-deterministic bugs that only manifest occasionally.

### 11.6 Watchdog / Lockup Detector

Configure the timer interrupt to fire every 10ms. On each tick, check whether a
syscall has been in progress longer than the threshold. If so, print a stack
trace to serial and panic.

This catches:

- Infinite loops in kernel code
- Livelock (two code paths alternating without progress)
- Priority inversion deadlocks (high-priority thread blocked on low-priority
  thread that can't run because the high-priority thread holds the CPU)
- Spin waits that never terminate

No test catches these because tests don't have time bounds. A syscall that takes
100ms is functionally hung, even if it would eventually complete.

Implementation: the timer interrupt handler already exists for scheduling. Add a
`last_syscall_entry` timestamp to PerCpu, set it on syscall entry, check it on
timer tick. Bare-metal only.

### 11.7 Host vs Bare-Metal Differential Testing

Run the same syscall sequences on both targets and compare results:

1. Extract test scenarios from the host test suite: each test's sequence of
   `dispatch()` calls and expected `(error, value)` returns
2. Encode these as a portable format (e.g., a sequence of
   `(syscall_num, args, expected_error, expected_value)` tuples)
3. On bare-metal (init binary), replay the same sequences and compare results
4. Any divergence is a `#[cfg]` bug — the host stub and real implementation
   disagree

Particular focus on `user_mem.rs`, which has completely different
implementations for host (`copy_nonoverlapping`) and bare-metal (LDTR/STTR with
fault recovery). The logic is the same but the mechanism is different — exactly
where bugs hide.

### 11.8 Release vs Debug [DONE]

Run all integration tests in both debug and release mode. Optimizer bugs
(especially around `unsafe` code and inline assembly) often only manifest in
release builds. The `options(nomem)` misuse described in CLAUDE.md is exactly
this class of bug.

### 11.9 Cycle-Accurate Benchmarks (Every Syscall) [DONE]

The current bench suite has one benchmark (null syscall). For each of the 30
syscalls, measure cycle cost on the actual M4 Pro P-core using CNTVCT_EL0 with
ISB barriers for serialization. 10 warmup iterations, 10,000 measurement
iterations, report median and P99.

**Per-syscall benchmarks:**

| Syscall                   | What to measure                               | Expected order of magnitude |
| ------------------------- | --------------------------------------------- | --------------------------- |
| Null (invalid)            | Trap + dispatch + return (overhead floor)     | ~100-200 cycles             |
| `vmo_create`              | Table alloc + handle install                  | ~200-400 cycles             |
| `vmo_map`                 | VA allocator + mapping record + handle lookup | ~300-600 cycles             |
| `vmo_snapshot`            | COW snapshot creation                         | ~400-800 cycles             |
| `endpoint_create`         | Table alloc + handle install                  | ~200-400 cycles             |
| `call` + `recv` + `reply` | **Full IPC round-trip** — the critical path   | Target: minimize            |
| `event_create`            | Table alloc + handle install                  | ~200-400 cycles             |
| `event_signal`            | Bit-OR + waiter wake check                    | ~100-300 cycles             |
| `event_wait`              | Mask check + (block or immediate return)      | ~100-300 cycles (signaled)  |
| `handle_dup`              | Handle table scan + rights computation        | ~100-200 cycles             |
| `handle_close`            | Handle removal + reference counting           | ~100-200 cycles             |
| `clock_read`              | CNTVCT_EL0 read                               | ~20-50 cycles               |
| `system_info`             | Static data return                            | ~50-100 cycles              |

The IPC round-trip (call → recv → reply → caller unblocks) is the single most
important number. This is the path every tool↔OS interaction takes. Measure:

1. **Null IPC:** call with 0-byte message, 0 handles → recv → reply with 0 bytes
   — pure scheduling + dispatch overhead
2. **Full IPC:** call with 128-byte message + 4 handles → recv → reply with
   128-byte message — maximum data transfer
3. **IPC with priority inheritance:** caller at Low, server at High — measures
   the priority boost path

Reference points (published numbers, for context — not necessarily comparable
due to different hardware and measurement methodology):

- seL4 on Cortex-A53: ~230 cycles null IPC
- seL4 on Cortex-A57: ~240 cycles null IPC
- Zircon: ~1500-2000 cycles channel_call (much heavier abstraction)

### 11.10 Theoretical Minimum Analysis [DONE]

For each hot path, compute the minimum possible cycle cost given what the
hardware requires. This is the floor we're optimizing toward.

**IPC round-trip theoretical minimum on M4 Pro:**

1. `svc` instruction → exception entry: ~20 cycles (pipeline flush + vector)
2. Register save (SVC fast path: ~8 registers): ~4 cycles (2× STP)
3. Syscall dispatch (match + function call): ~5 cycles
4. Handle lookup (array index + generation check): ~3-6 cycles (1 L1D load if
   handle table is hot)
5. Endpoint dequeue (check pending queue): ~3-6 cycles (1-2 L1D loads)
6. Thread state update (mark blocked/ready): ~3 cycles
7. Context switch to server: ~20 cycles (register restore + eret)
8. Server processes (user code — not measured)
9. `svc` for recv: ~20 cycles
10. Reply dispatch: ~10 cycles (handle lookup + reply cap check)
11. Caller wake + context switch back: ~20 cycles

Theoretical minimum: ~110-130 cycles for null IPC round-trip, assuming all data
structures are L1D-hot. The gap between this and measured performance reveals
optimization opportunities.

**Event signal-to-wake theoretical minimum:**

1. Handle lookup: ~3-6 cycles
2. Bit-OR operation: ~1 cycle
3. Waiter check: ~3 cycles
4. Thread wake (mark Ready, enqueue): ~6 cycles
5. Return: ~20 cycles (register restore + eret) Minimum: ~35-40 cycles (no
   context switch, just signal and return)

**Page fault resolution theoretical minimum:**

1. Exception entry: ~20 cycles
2. FAR/ESR decode: ~3 cycles
3. Address space lookup: ~6 cycles (mapping binary search)
4. Page alloc: ~10 cycles (bitmap scan)
5. Page table update: ~10 cycles (PTE write + TLBI)
6. Return: ~20 cycles Minimum: ~70 cycles (lazy zero page, no copy)

### 11.11 Cache and TLB Behavior Profiling

Measure the actual cache/TLB behavior of hot paths:

- **Working set size of IPC path:** How many unique cache lines does a full
  call/recv/reply touch? On M4 Pro with 128-byte lines, each unnecessary cache
  line miss costs 17-18 cycles (L2) or ~220 cycles (SLC).
- **Handle table locality:** Is the handle table laid out so that the common
  case (handle 0-7) fits in one or two cache lines? With 128-byte lines, this is
  feasible if handles are packed.
- **Scheduler queue locality:** Are the per-priority run queues in the same
  cache line as the core's current-thread pointer?
- **TLB pressure:** How many TLB entries does the kernel use during a syscall?
  With only ~160 DTLB entries covering 2.5 MB, the kernel must minimize its VA
  footprint.

### 11.12 Compile-Time Struct Layout Assertions [DONE]

The `TrapFrame` already uses `offset_of!` + `size_of` assertions to catch layout
drift against the assembly. Extend this pattern to every performance- critical
struct:

```rust
// Assert hot fields are in the first cache line (128 bytes on M4 Pro).
const _: () = {
    assert!(core::mem::offset_of!(Kernel, core_id) < 128);
    assert!(core::mem::offset_of!(Kernel, scheduler) < 128);
    // ... every field accessed on the IPC fast path
};
```

Structs to assert:

- `Kernel` — `core_id`, `scheduler`, `endpoints`, `threads` offsets
- `Thread` — `state`, `priority`, `address_space`, `space_next/prev`
- `Endpoint` — `priority_rings`, `recv_waiters`, `bound_event`
- `Event` — `bits`, `waiters`, `waiter_count`
- `Handle` — entire struct should fit in one cache line
- `PerCpu` — `current_thread`, `kernel_ptr`, `reschedule_flag`

These assertions break the build if anyone adds a field that pushes hot data
past the cache line boundary. The optimization from Phase 11.9 becomes
permanently enforced, not a one-time arrangement that silently degrades.

### 11.13 M4 Pro-Specific Optimization Opportunities

Based on the hardware reference, specific optimizations to evaluate:

1. **128-byte cache line packing:** Restructure `Kernel`, `Thread`, `Endpoint`,
   `Event` so that hot fields (accessed on every syscall) are in the first 128
   bytes. Cold fields (accessed rarely) go in subsequent lines. This is not a
   generic optimization — it's specific to Apple Silicon's 128-byte lines.

2. **LSE atomics for spinlocks:** Verify `sync.rs` uses LDADD/SWP/CAS instead of
   LL/SC loops. On M4 Pro, LSE atomics avoid the cache-line bounce that LL/SC
   causes under contention.

3. **FEAT_WFxT for bounded spin:** Use WFE with timeout instead of unbounded
   spin loops. This saves power and avoids livelock on E-cores (which may have
   different scheduling characteristics).

4. **STP/LDP pairs for register save:** The SVC fast path saves ~8 registers.
   Verify these use STP (store pair) for 2-register-per-instruction throughput.
   Single STR wastes a store port.

5. **Branch prediction awareness:** The syscall dispatch `match` compiles to a
   jump table or branch chain. Measure whether reordering arms by frequency (IPC
   syscalls first) improves branch predictor hit rate. With a 13-cycle
   mispredict penalty, this matters.

6. **Prefetch on IPC path:** When a call arrives and the server is blocked on
   recv, the kernel could prefetch the server's register state before actually
   switching. M4 Pro's hardware prefetcher handles sequential access well, but
   kernel data structures are pointer-chased — explicit `PRFM` might help.

7. **Per-core kernel data alignment:** Ensure each core's PerCpu struct starts
   on a 128-byte boundary (cache line aligned) to eliminate false sharing
   between cores.

### 11.14 Assembly Inspection

For every function on the IPC hot path (`sys_call`, `sys_recv`, `sys_reply`,
`dispatch`, and the SVC entry/exit assembly), inspect the generated machine
code:

```bash
cargo objdump -p kernel --release -- --disassemble-symbols=<symbol> \
    --no-show-raw-insn
```

Check for:

- Unnecessary register spills (Rust/LLVM putting values on the stack instead of
  keeping them in registers)
- Redundant loads (loading the same value multiple times because the compiler
  doesn't know it hasn't changed)
- Missed STP/LDP pairing (using two STR/LDR where one STP/LDP would suffice)
- Branch chains that could be a jump table (or vice versa)
- Unnecessary barrier instructions (DSB/ISB where the architecture doesn't
  require them)

Log the disassembly in `design/hot-path-asm.md` alongside benchmarks. When a
performance regression occurs, diff the assembly first — the cause is usually
visible as a new spill or a missed optimization.

### 11.15 Workload-Level Benchmarks

Per-syscall microbenchmarks measure operations in isolation. Real performance
depends on how operations interact — cache eviction pressure, TLB contention,
branch predictor pollution between different syscall types.

Build a "representative workload" bare-metal test that simulates the OS's
anticipated usage pattern:

1. **Document editing workload:** init spawns a "compositor" service and an
   "editor" service. The editor creates VMOs (document content), maps them,
   writes data, creates COW snapshots (undo points), signals the compositor via
   events, and does IPC to request re-render. 1000 iterations. Measure total
   cycles and per-operation breakdown.

2. **IPC storm:** init spawns 5 services, each sends 100 IPC round-trips to
   every other service. Measures sustained IPC throughput under realistic
   multiplexing, not just single-pair latency.

3. **Object lifecycle churn:** rapid create/use/destroy cycles for all object
   types simultaneously. Measures allocator performance under fragmentation
   pressure.

These workloads reveal performance characteristics that microbenchmarks miss —
especially cache conflicts between the handle table, object tables, scheduler
queues, and page tables when they're all active simultaneously.

### 11.16 Performance Regression Thresholds [DONE]

Replace the current 10x threshold with statistically grounded per-benchmark
thresholds:

1. Run each benchmark 1,000 times
2. Compute the distribution: median, P95, P99, standard deviation
3. Set the regression threshold at **P99 + 3σ** — this is the boundary beyond
   which a result is almost certainly a real regression, not hypervisor jitter
4. Store the full distribution in `kernel/bench_baselines.toml`

```toml
# Generated by `make bench-baseline`. Do not edit manually.
# Re-run after any deliberate performance change.

[null_syscall]
median = 148
p95 = 162
p99 = 178
stddev = 12
threshold = 214   # p99 + 3*stddev

[ipc_null_roundtrip]
median = 0        # placeholder — measured during Phase 11
p95 = 0
p99 = 0
stddev = 0
threshold = 0
```

When a measured value exceeds the threshold:

- Run the benchmark 1,000 times again (rule out transient hypervisor load)
- If still above threshold: investigate, bisect, fix
- If an optimization improves a benchmark: re-run baseline, ratchet forward

---

## Phase 12: Regression Infrastructure

**Goal:** Every bug found becomes a permanent guard against recurrence.

### 12.1 Commit Gate (Pre-Commit) [DONE]

Expand the existing pre-commit gate:

```bash
#!/bin/bash
set -e

# 1. Format check
cargo +nightly fmt -- --check

# 2. Clippy (pedantic)
cargo clippy -p kernel -- -D warnings -W clippy::pedantic ...

# 3. Framekernel discipline
grep -r "allow(unsafe_code)" kernel/src/ | grep -v "frame/" && exit 1 || true

# 4. Full test suite
cargo test -p kernel --lib --target aarch64-apple-darwin

# 5. Miri (on a subset — full Miri is slow)
MIRIFLAGS="-Zmiri-leak-check" \
    cargo +nightly miri test -p kernel --lib --target aarch64-apple-darwin \
    -- invariant verification handle vmo event endpoint

# 6. Build for target (catches #[cfg] mistakes)
cargo build -p kernel
```

### 12.2 Nightly Gate [DONE]

Run nightly (or weekly, depending on machine budget):

```bash
# Full Miri
cargo +nightly miri test -p kernel --lib --target aarch64-apple-darwin

# Mutation testing
cargo mutants -p kernel

# Fuzzing (1 hour per target)
# 1 hour per target
timeout 3600 cargo +nightly fuzz run syscall_sequence
timeout 3600 cargo +nightly fuzz run syscall_multi_thread
timeout 3600 cargo +nightly fuzz run syscall_structured

# Coverage report
RUSTFLAGS="-C instrument-coverage" cargo test -p kernel --lib ...
grcov ...

# Bare-metal integration + performance
scripts/integration-test
scripts/bench-test          # runs all benchmarks, compares to baselines

# Stress boot (100 iterations)
for i in $(seq 1 100); do scripts/integration-test || exit 1; done
```

### 12.3 Performance as a Regression Gate [DONE]

A performance regression is treated like a correctness regression — it is
investigated, root-caused, and fixed before moving on. The process:

1. `scripts/bench-test` runs all benchmarks, loads baselines from
   `kernel/bench_baselines.toml`, compares against per-benchmark statistical
   thresholds (P99 + 3σ)
2. If any benchmark exceeds its threshold: FAIL — printed to stdout with the
   baseline, measured, and ratio
3. If a regression is real (not hypervisor jitter): bisect to the commit that
   caused it, understand why, fix it
4. If an optimization improves a benchmark: update the baseline (ratchet forward
   — never allow regression from the new level)
5. After major optimization work: re-run all benchmarks 1,000× to establish
   stable new baselines

### 12.4 Makefile Targets [DONE]

```makefile
test:           # Fast: unit + syscall + pipeline + verification tests
miri:           # Miri on all host-target tests
fuzz:           # 1-hour fuzz run on all targets
coverage:       # Generate HTML coverage report
mutants:        # Mutation testing
integration:    # Bare-metal boot test (correctness)
bench:          # Bare-metal benchmarks (performance)
bench-check:    # Benchmarks + baseline comparison (regression gate)
stress:         # 100x boot test
audit:          # cargo-audit + cargo-deny
gate:           # Full pre-commit gate (correctness + build)
nightly:        # Everything (miri + fuzz + mutants + coverage + stress + bench)
```

---

## Execution Order

The phases are ordered by **leverage** — each phase's output feeds the next. The
correctness phases build the safety net; the performance phase uses it.

**Verify the foundation (phase 0):**

0. **Spec review** — verify the spec itself before verifying the implementation.
   Produces the invariant list that every subsequent phase uses.

**Build the safety net (phases 1-10):**

1. **Unsafe audit** — understand the risk surface AND identify optimization
   opportunities in the asm/unsafe code
2. **Property-based testing** — find correctness bugs fast (proptest generates
   thousands of inputs per second)
3. **Fuzzing overhaul** — find bugs that even property tests miss (deeper state
   space exploration, invariant checking in harness, 1-hour runs)
4. **Miri** — catch UB in the unsafe code identified by the audit
5. **Coverage measurement** — identify what phases 2-4 missed
6. **Mutation testing** — verify that tests actually detect bugs, not just
   exercise code
7. **Sanitizers** — catch memory bugs that Miri misses (different detection
   approach)
8. **Concurrency verification** — verify the multi-core paths, including real
   SMP bare-metal stress
9. **Error injection** — verify every error path works
10. **Static analysis** — catch remaining bugs at compile time

**Use the safety net (phase 11):**

11. **Bare-metal verification + performance characterization** — verify
    everything on actual M4 Pro silicon, measure every hot path, compute
    theoretical minimums, close the gap. The correctness infrastructure from
    phases 1-10 is what makes aggressive optimization safe — every change runs
    through the full verification pipeline.

**Lock it down (phase 12):**

12. **Regression infrastructure** — performance AND correctness regression
    gates. A performance regression is a bug, not a tradeoff.

All 12 phases are required. There is no "if you only do these four" shortcut.
The safety net has holes if any phase is skipped, and the optimization can't be
aggressive if the safety net has holes.

---

## What This Does NOT Cover

- **Formal verification** — excluded per request, but if ever reconsidered: seL4
  took ~20 person-years for ~10K LOC. This kernel at ~19.5K LOC would be a
  multi-year effort, but the framekernel discipline (safe Rust outside `frame/`)
  means you'd only need to verify the ~4K lines in `frame/`.
- **Hardware errata** — Apple Silicon M4 Pro specific. Apple does not publish
  errata sheets like ARM does for Cortex cores. Bugs would surface as
  non-deterministic failures in stress testing (Phase 11.3).
- **Side channels** — the M4 Pro's Load Value Predictor (SLAP/FLOP
  vulnerabilities) is active during kernel execution and cannot be disabled.
  FEAT_DIT provides constant-time execution for crypto operations. FEAT_CSV2/3
  provides branch predictor and speculative load isolation at exception
  boundaries. Full side-channel analysis is a separate effort, but the hardware
  mitigations are noted here for awareness.
- **Compiler bugs** — Rust/LLVM can miscompile, especially around unsafe code
  and inline assembly. Cross-referencing debug and release behavior (Phase 11.4)
  partially mitigates this. Inspecting generated assembly for hot paths (Phase
  11.6) catches the most impactful cases.
- **Asymmetric core scheduling** — The M4 Pro has 10 P-cores and 4 E-cores with
  different performance characteristics. The kernel currently treats all cores
  as uniform. P/E-aware scheduling is a future optimization, not a correctness
  concern — the ISA is identical, only performance differs.

---

## Remediation Protocol

Every bug found by any phase follows this protocol:

1. **Reproduce:** write a minimal deterministic test that triggers the bug.
2. **Root-cause:** identify the actual cause, not just the symptom. Trace the
   bug to the specific line(s) and the invariant they violate.
3. **Class check:** does the same class of bug exist elsewhere? Search for
   analogous code patterns. Fix all instances.
4. **Fix:** implement the fix. The fix must be the smallest correct change — no
   drive-by refactoring.
5. **Verify:** the reproduction test passes. The full test suite passes. The
   invariant checker passes. If applicable, re-fuzz the area.
6. **Harden:** if the bug reveals a missing invariant, add it to
   `invariants.rs`. If it reveals a missing property test, add it. If it reveals
   a missing fuzz pattern, add a seed.
7. **Commit:** atomic commit with message: `fix(kernel): <description>`.

---

## Iteration Protocol

The plan is not "execute phases 0-12 once." It is "execute phases 0-12, then
re-verify." Specifically:

### After phases 0-10 (safety net built)

- Run all property tests, fuzzing, Miri, sanitizers, mutation testing
- Fix every bug found (remediation protocol)
- Re-run all phases that found bugs until they find zero new issues

### After phase 11 (optimizations applied)

- **Simplification pass:** review every function modified during optimization.
  If a function got longer than ~50 lines, or if the optimization made the logic
  harder to follow without a proportional performance gain, simplify it. The
  test suite verifies the simplification doesn't break anything. This catches
  the "clever code that's correct today but will break next time someone touches
  it" class of problem that tests don't catch.
- Also check for: duplicated patterns across syscall handlers that should be
  consistent, any new unsafe blocks that crept in outside `frame/`, any
  optimization that introduced a non-obvious invariant without documenting it.
- Re-run phases 2-10 on the optimized+simplified code
- Fix anything found
- Re-benchmark to verify the optimization actually helped
- If the optimization didn't help (within measurement noise), revert it

### Convergence criterion

The iteration loop terminates when:

1. A full re-run of phases 2-10 finds zero new bugs
2. Fuzz run (1 hour per target) produces zero crashes or invariant violations
3. Mutation testing on critical files has zero surviving mutants (excluding
   provably equivalent mutations)
4. All benchmarks are baselined and stable (< 3σ variance across 1,000 runs)
5. Coverage targets are met (100% branch on syscall handlers and object ops)

---

## Exit Criteria

"Done" means ALL of the following are true simultaneously:

### Correctness

- [ ] Phase 0 complete: spec reviewed, all edge cases defined and implemented
- [ ] Phase 1 complete: every unsafe block audited, SAFETY comments verified
- [ ] Phase 2 complete: property tests for all state machine properties
- [ ] Phase 3 complete: structured fuzz targets with invariant checking, 1-hour
      clean run per target
- [ ] Phase 4 complete: Miri passes on all host-target tests
- [ ] Phase 5 complete: 100% branch coverage on syscall handlers and object ops
- [ ] Phase 6 complete: zero surviving mutants on critical files
- [ ] Phase 7 complete: all tests pass under ASan, LSan, UBSan
- [ ] Phase 8 complete: concurrency primitives verified, preemption stress
      tested, SMP stress test passes
- [ ] Phase 9 complete: every error path tested via injection
- [ ] Phase 10 complete: pedantic clippy clean, deny attributes in place
- [ ] Iteration loop converged: full re-run finds zero new issues

### Performance

- [ ] Phase 11 complete: every syscall benchmarked and baselined
- [ ] Boot-time self-test (POST) implemented and passing
- [ ] Debug-build runtime invariant checking enabled
- [ ] Watchdog / lockup detector implemented
- [ ] Host vs bare-metal differential testing passing
- [ ] Hot path assembly inspected and documented
- [ ] Workload benchmarks measured
- [ ] Theoretical minimums computed, gap documented
- [ ] Struct layout assertions in place for all hot structs
- [ ] M4 Pro optimizations evaluated and applied where beneficial
- [ ] All optimizations verified correct via phases 2-10
- [ ] Post-optimization simplification pass complete

### Infrastructure

- [ ] Phase 12 complete: pre-commit gate, nightly gate, Makefile targets
- [ ] Baseline file checked in and ratcheted to current performance
- [ ] All invariants from Phase 0.3 implemented in `invariants.rs`
- [ ] STATUS.md updated to reflect current state

When all boxes are checked, commit the final state, merge to main, and report
done.
