# Kernel Hardening & Maturation Gaps

Comparative analysis of this kernel against production and research microkernels (seL4, Zircon/Fuchsia, Redox, Asterinas, Theseus, Linux). Documents what the kernel lacks relative to those systems and how to fill each gap, ordered by priority.

Written 2026-03-14. Reference kernels at time of writing: seL4 (formally verified, C), Zircon (Fuchsia, C++), Redox (Rust, Unix-like), Asterinas (Rust framekernel, Linux ABI), Theseus (Rust, intralingual), Linux 6.6+ (EEVDF).

---

## What the Kernel Does Well (Context for Gaps)

These are genuine strengths — not just "works," but differentiated:

- **Two-layer scheduling (EEVDF + scheduling contexts with donation).** No other open kernel combines proportional-fair EEVDF selection with seL4-style scheduling contexts and context donation (borrowing a client's budget when servicing its IPC). QNX's partition inheritance is the closest commercial equivalent.
- **Content-type-aware scheduling policy.** The kernel provides temporal isolation mechanisms; the OS service sets budgets by document mimetype. No other kernel has this separation.
- **Capability-to-code ratio.** ~8,900 lines of Rust for: 4-core SMP, 28 syscalls, EEVDF+contexts, demand paging, buddy/slab/linked-list allocators, userspace drivers with interrupt forwarding + DMA, DTB parsing, unified wait multiplexing, process create/kill, handle-based access control.
- **Unsafe discipline.** ~124 SAFETY comments for ~115 unsafe blocks — every unsafe block has an accompanying SAFETY comment. 7-category taxonomy (DESIGN.md §7.1). No `static mut`, no `transmute`, no unsafe in EL0 code.
- **Clean-room EEVDF.** 146 lines, zero unsafe, pure stateless logic. Easier to study than Linux's (CFS legacy) or seL4's MCS (capability overhead).
- **Design documentation.** Every decision has rationale, alternatives considered, and rejection reasons. The repo is a teaching artifact for kernel design, not just an implementation.

---

## Gap 1: Capability Model

**Priority: Medium.** Matters when untrusted editors or compound documents arrive.

### Current State

256 fixed slots per process, read/write rights bitfield. `handle_send` moves handles (move semantics). Kernel validates handle + rights on every syscall.

### What's Missing

| Feature                                             | seL4                        | Zircon                              | This kernel             |
| --------------------------------------------------- | --------------------------- | ----------------------------------- | ----------------------- |
| Rights attenuation (reduce rights on transfer)      | Yes (mint)                  | Yes (duplicate with reduced rights) | No — full rights copied |
| Revocation (transitively kill derived capabilities) | Yes (revoke on CNode)       | No (close handle)                   | No                      |
| Badging (tag capabilities to identify sender)       | Yes (mint with badge)       | No                                  | No                      |
| Resource accounting (provable memory bounds)        | Yes (untyped memory)        | Partial (job limits)                | No                      |
| Rights granularity                                  | Full capability type system | ~15 distinct rights                 | 2 bits (read/write)     |
| Dynamic handle table                                | CNodes (arbitrary size)     | Dynamic growth                      | 256 fixed slots         |

### Remediation Path

**Phase 1 — Rights attenuation (low cost, high value):**

Widen the rights bitfield and add a mask parameter to `handle_send`. Recommended rights bits:

```text
READ     = 1 << 0   // read from channel, map read-only
WRITE    = 1 << 1   // write to channel, map writable
SIGNAL   = 1 << 2   // signal a channel/interrupt
WAIT     = 1 << 3   // include in wait set
MAP      = 1 << 4   // memory_share, device_map
TRANSFER = 1 << 5   // can be sent via handle_send
CREATE   = 1 << 6   // create child threads/processes
KILL     = 1 << 7   // process_kill
```

`handle_send(target, handle, rights_mask)` — new handle gets `original_rights & rights_mask`. An editor that only needs to send write requests gets `WRITE | SIGNAL | WAIT` — it can't `TRANSFER` or `KILL`.

Scope: ~50 lines changed in `handle.rs`, `syscall.rs`. No new modules. Userspace API gains one parameter.

**Phase 2 — Syscall filtering: ✅ IMPLEMENTED (2026-03-14)**

Per-process `syscall_mask: u32` bitmask. OS service sets it before `process_start` via `PROCESS_SET_SYSCALL_FILTER` (syscall 27). Checked at the top of `dispatch()`. EXIT always allowed regardless of mask. Returns `SyscallBlocked` (-15) for filtered syscalls. 15 tests.

An editor's recommended mask: `exit`, `write`, `yield`, `handle_close`, `channel_signal`, `wait`, `futex_wait`, `futex_wake`, `memory_alloc`, `memory_free`. That's 10 of 28. The other 18 (process_create, process_kill, interrupt_register, device_map, dma_alloc, etc.) are blocked.

**Phase 3 — Dynamic handle table (moderate cost):**

Replace fixed `[Option<HandleEntry>; 256]` with a growable structure. Two options:

- Vec-backed (simple, requires heap allocation on growth, O(1) lookup by index).
- Two-level table (like Zircon — base array of 256 + overflow page). Handles 0-255 are fast path, overflow is rare path.

Not urgent — 256 slots is generous for current use. Matters when compound documents create many channels.

**What to skip:**

- Full CNode trees (seL4-style). Massive complexity for minimal gain in a personal OS.
- Untyped memory (seL4-style). Beautiful for formal verification, unnecessary without it. The kernel's internal allocators are fine.
- Capability derivation trees. The sole-writer pattern (OS service mediates all access) provides userspace-level revocation: stop forwarding messages to a revoked editor. No kernel machinery needed.

---

## Gap 2: Security Hardening

**Priority: Low now, Medium when loading external documents.**

### Current State

W^X enforcement (kernel + user), split TTBR (kernel/user isolation), handle + rights validation on every syscall, user buffer validation via `AT S1E0R`, `validate_context_before_eret` (defense-in-depth on exception return), guard pages on stacks.

### What's Missing

| Feature                      | Linux              | Zircon                 | This kernel                  |
| ---------------------------- | ------------------ | ---------------------- | ---------------------------- |
| User ASLR                    | Yes                | Yes                    | No — deterministic VA layout |
| KASLR                        | Yes                | Yes                    | No — fixed kernel VA offset  |
| Stack canaries               | Yes (compiler)     | Yes                    | No                           |
| CFI (Control Flow Integrity) | Partial (clang)    | Partial                | No                           |
| Seccomp / syscall filtering  | Yes                | No (uses capabilities) | No (see Gap 1 Phase 2)       |
| KASAN / memory sanitizers    | Yes (debug builds) | Yes                    | No                           |
| Spectre/Meltdown mitigations | Yes                | Yes                    | No                           |

### Remediation Path (Ordered by Value/Cost)

**1. Syscall filtering — ✅ DONE (see Gap 1 Phase 2).** Implemented 2026-03-14.

**2. User ASLR (moderate cost, moderate value):**

Current user VA layout (all fixed):

```text
Code:          0x0040_0000
Heap:          0x0100_0000 – 0x1000_0000
DMA:           0x1000_0000 – 0x2000_0000
Channel SHM:   0x4000_0000
Stack:         0x8000_0000
Shared memory: 0xC000_0000
```

Randomize each region's base within its allowed range. Requires:

- Kernel PRNG (ChaCha20, ~50 lines). Seed from architectural counter + DTB-provided entropy.
- Per-process randomized bases stored in `AddressSpace`.
- Init and `handle_send` can't assume fixed SHM addresses — pass VAs via channel messages instead.

The bump allocators (`next_heap_va`, `next_dma_va`, `next_channel_shm_va`) already support arbitrary starting points. The coupling to `*_BASE` constants is the work.

Matters when: the OS loads external documents that untrusted editors process. A malicious document exploiting a parser bug in an editor can't predict where to jump.

**3. Stack canaries (low cost, low urgency):**

Rust's `-Z stack-protector=all` compiler flag. The kernel provides a per-thread random canary value and sets the TLS slot the compiler reads from. ~20 lines of kernel code. Low urgency because safe Rust prevents the buffer overflows that canaries detect. Only relevant for `unsafe` blocks.

**4. KASLR (moderate cost, low urgency):**

Randomize `KERNEL_VA_BASE` at boot. Requires: hardware RNG or DTB entropy, boot.S modification to compute a random offset before enabling MMU, adjusting all kernel VA calculations. Well-understood (Linux, Zircon do it) but fiddly in assembly.

Matters when: there's a threat model that includes kernel exploits. Currently no attack surface (no network, no filesystem loading arbitrary code).

**5. Spectre/Meltdown mitigations (high cost, very low urgency):**

These matter for multi-tenant systems where an attacker runs code alongside a victim. A personal OS with a single user and trusted editors doesn't have this threat model. If it ever matters: speculation barriers on syscall entry/exit, KPTI (already partially achieved by split TTBR), indirect branch prediction barriers.

**What to skip for now:**

- KASAN (kernel address sanitizer). Useful for kernel development debugging, not a runtime hardening feature. The existing test suite (1,462 tests) + manual audits cover this.
- CFI. Requires clang toolchain integration. Rust's type system already prevents most control flow attacks in safe code.

---

## Gap 3: SMP Scalability

**Priority: Low.** The current global lock works for 4 cores with ~5 processes.

### Current State

One `IrqMutex` (ticket spinlock + IRQ masking) protecting all scheduler state. Every timer tick (250 Hz × 4 cores = 1,000/sec), every `wait`, `signal`, `yield`, and context switch acquires it. `metrics::inc_lock_spins()` tracks contention.

Separate locks already exist for: channels, futexes, timers, interrupts, scheduling contexts. The scheduler lock is the remaining bottleneck.

### When It Matters

At 4 cores, 250 Hz, ~1 us critical section: ~1 ms/sec contention (0.1%). Negligible.

It starts to matter when: many processes do rapid IPC (multiple editors, compositor, OS service, multiple drivers all actively messaging). The symptom is increased tail scheduling latency, not starvation (scheduling contexts bound per-workload damage). The `lock_spins` metric will show it.

### How Other Kernels Handle This

| Kernel | Approach                                                      |
| ------ | ------------------------------------------------------------- |
| Linux  | Per-CPU run queues, work stealing, RCU for read paths         |
| Zircon | Per-CPU scheduler, IPI for cross-core wake                    |
| seL4   | Per-core scheduling, IPC fast path with direct context switch |
| Redox  | Global scheduler (same as us, similar scale)                  |

### Remediation Path

**Phase 1 — Per-core ready queues (moderate cost):**

Split the single ready queue into per-core queues. Each core selects from its own queue without locking other cores. Already partially in place: `PerCpu` holds `current_thread` and `idle_thread`.

Changes:

- `PerCpu` gains `ready_queue: VecDeque<Box<Thread>>` (or sorted EEVDF structure).
- `schedule_inner` selects from the local queue — no global lock for the common path.
- Wake-ups targeting the local core go directly to the local queue.

**Phase 2 — IPI for cross-core wake (moderate cost):**

When `channel_signal` wakes a thread that belongs to a different core, send a GIC SGI (software-generated interrupt) to that core to trigger reschedule. The `interrupt_controller.rs` already has GIC distributor access; SGI is a write to GICD_SGIR.

Changes:

- `try_wake_for_handle` / `set_wake_pending_for_handle` determine target core.
- If target != current core: enqueue thread on target's queue + send IPI.
- Target core's IRQ handler checks for new ready threads.

The two-phase wake pattern (collect under source lock, wake under scheduler lock) changes to: collect under source lock, enqueue on target core's queue (brief per-core lock), IPI.

**Phase 3 — Work stealing (lower priority):**

When a core's queue is empty, steal from the busiest core. Standard algorithm:

- Idle core scans other cores' queue lengths.
- Steal half the threads from the longest queue.
- Acquire the victim's per-core lock briefly.

Only matters under asymmetric load. With scheduling contexts assigning work to specific cores (future), stealing may be unnecessary.

**Phase 4 — Lock splitting for scheduler metadata (lower priority):**

The global scheduler lock currently also protects: thread state transitions, blocked list, suspended list. These could move to finer-grained locks:

- Per-thread state lock (for state transitions).
- Per-core blocked list.
- Global process table lock (already low contention).

Diminishing returns — only pursue if metrics show Phase 1-2 didn't resolve contention.

**Key invariant to maintain:**

The lost-wakeup prevention (`wake_pending` flag, store-before-check ordering) must work across cores. Currently: the flag is checked under the scheduler lock. With per-core locks: the flag must be set atomically and checked with acquire/release ordering before the target core's lock is acquired. The pattern becomes: set `wake_pending` (release store), IPI, target core reads `wake_pending` (acquire load) in its IRQ handler.

### What to Skip

- RCU. Overkill for this scale. RCU shines for read-heavy data structures with thousands of concurrent readers (Linux's process list, file descriptor table). Our kernel's read paths are short and infrequent.
- NUMA awareness. QEMU virt is UMA. If real hardware is ever a target, revisit.
- Lock-free scheduler. Possible (Zircon-ish) but the complexity isn't justified. Per-core locks with IPI is the standard microkernel approach.

---

## Decision Checklist: When to Invest

| Trigger                                              | Action                                                                    |
| ---------------------------------------------------- | ------------------------------------------------------------------------- |
| Start loading external documents / untrusted content | ~~Syscall filtering (Gap 1.2)~~ ✅, user ASLR (Gap 2.2)                   |
| Compound documents with multiple editors             | Rights attenuation (Gap 1.1), dynamic handle table (Gap 1.3)              |
| `metrics::lock_spins` climbing under real workloads  | Per-core ready queues (Gap 3.1), IPI wake (Gap 3.2)                       |
| Considering open-sourcing for others to build on     | All of Gap 2 (demonstrates security baseline)                             |
| Formal verification interest                         | Full capability model (Gap 1, all phases)                                 |
| None of the above                                    | Keep building the document-centric OS. These gaps don't block the design. |

---

## Non-Gaps (Things That Look Missing But Aren't)

- **Linux ABI compatibility.** Explicitly a non-goal (Decision #3). Own native APIs.
- **Networking / TCP/IP.** Future work, not a kernel gap — it's a userspace service.
- **Filesystem.** Blocked on Decision #16 (COW on-disk design), not a kernel gap. Kernel provides mechanics (Phase 8: COW page faults), filesystem is userspace.
- **Hardware support beyond QEMU virt.** Out of scope for design phase. virtio drivers are architecturally correct; real drivers would follow the same pattern.
- **Self-hosting.** Not a goal.
