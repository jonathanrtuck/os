# Microkernel Principles

First-principles reasoning about what a microkernel is, what it must do, and
why. This document captures the foundational thinking that produces kernel
design decisions. It is not specific to this project's kernel — it is the
general framework.

---

## The fundamental question

**What must the kernel do because userspace literally cannot?**

The kernel exists because hardware restricts certain operations to a privileged
exception level. The kernel's irreducible responsibilities are exactly the
operations the silicon won't let userspace perform. Everything else is a design
choice about where to put the code.

---

## Three irreducible responsibilities

| #   | Responsibility                    | Hardware mechanism                                       | Why only the kernel can do it                                                                                                                                                              |
| --- | --------------------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1   | **Multiplex CPU and RAM**         | MMU (page tables), preemption timer                      | Hardware restricts these to the privileged exception level. Any entity that can program the timer can monopolize the CPU. Any entity that can modify page tables can read anyone's memory. |
| 2   | **Route interrupts and faults**   | Exception vectors (VBAR_EL1), interrupt controller (GIC) | Hardware delivers all exceptions to the privileged level. The kernel receives them first by physical necessity. It must acknowledge, classify, and forward to userspace.                   |
| 3   | **Manage the privilege boundary** | Register save/restore, `eret` instruction                | The kernel is the gatekeeper between exception levels. It saves userspace state on entry, dispatches, restores state, and returns. Without this, there is no kernel/userspace split.       |

Process lifecycle, IPC, capabilities, scheduling policy — these are all things
built on top of these three. They may earn their way into the kernel for good
reasons, but they are not irreducible.

### Mapping to ARM64 privilege-restricted operations

The three responsibilities map directly to hardware capabilities restricted to
EL1+:

- **MMU registers** (TTBR, TCR, MAIR) — responsibility 1
- **Exception vector register** (VBAR_EL1) and **GIC** access — responsibility 2
- **`eret` instruction** and system register save/restore — responsibility 3

---

## CPU multiplexing

### The nature of the problem

CPU multiplexing is **temporal**. At any instant, a core runs exactly one thing.
If there are more demands than cores, the kernel time-slices — gives each demand
a turn, switches fast enough that all make progress. The core resource is _time
on a core_.

The mechanics of a switch: save the current register state, load a different
one, resume. The CPU doesn't know it's "running something else" — it just has
different register values and a different page table pointer. The concept of
distinct execution contexts exists entirely in the kernel's bookkeeping.

Preemption (the timer interrupt) is the enforcement mechanism that makes sharing
involuntary. Without it, a demand could monopolize the CPU indefinitely.

### Design choice: kernel owns scheduling (mechanism and policy)

The kernel handles both the mechanism (context switching) and the policy
(deciding what runs when, including priorities, time slices, and preemption).
Alternatives exist (Scheduler Activations, Exokernel-style userspace scheduling)
but were rejected for these reasons:

**Arguments for userspace scheduling and why they don't hold:**

1. _"The kernel doesn't know what the application cares about."_ — The kernel
   can expose scheduling parameters (priority, deadline, affinity) that let
   applications communicate intent without owning the decision.

2. _"Lightweight concurrency pays a tax through the kernel."_ — This is a
   different level of scheduling. The kernel schedules kernel-level entities
   (threads). Userspace schedules lightweight tasks (coroutines, green threads,
   async tasks) within its own time slices. M:N threading. These coexist
   cleanly.

3. _"Every scheduling decision round-trips through the privilege boundary."_ —
   Cuts both ways. If the scheduler is in userspace, every kernel event that
   affects scheduling (thread blocks on IPC, interrupt arrives) requires an
   upcall. The kernel is already in the loop for all these events. Making the
   decision right there avoids the round trip.

4. _"Domain-specific schedulers can't exist."_ — In a microkernel, the kernel
   schedules a modest number of services, not thousands of application threads.
   A priority-based preemptive scheduler with deadline support covers the
   practical space.

5. _"Mechanism and policy should be separated."_ — Scheduling policy is so
   tightly coupled with the events that trigger it (timer, IPC block, fault,
   interrupt) that pushing policy out creates a chatty protocol costing more
   than the simplicity it buys.

**Historical evidence:** Scheduler Activations were implemented in NetBSD and
experimental systems. The industry moved toward kernel scheduling with userspace
M:N threading on top. Exokernel's userspace schedulers mostly reimplemented the
same priority-based preemptive algorithm the kernel would have used.

### The schedulable unit: threads

A **thread** is a schedulable execution context — a saved register set
(including program counter) that the kernel can load onto a core. This is what
the scheduler manages. A thread runs _in_ an address space but is conceptually
independent of it.

Key distinction:

- **Thread** — schedulable execution context (what the kernel switches between)
- **Address space** — isolation boundary (what the MMU enforces)
- **Process** — a _convention_ that bundles one or more threads with a shared
  address space, plus associated resources. The hardware doesn't know what a
  process is. It's a management abstraction.

Whether the kernel bundles threads and address spaces into "processes" or
exposes them independently is a design choice, not a hardware requirement. seL4
keeps them fully independent (TCB, VSpace, CNode as separate kernel objects).
QNX bundles them.

Userspace is free to build lightweight concurrency (green threads, coroutines,
fibers) on top of kernel threads using M:N threading. The kernel doesn't need to
know about these.

---

## Memory multiplexing

### The nature of the problem

Memory multiplexing is **spatial**. Unlike CPU time, multiple demands hold their
memory simultaneously — page A belonging to demand X and page B belonging to
demand Y coexist in physical RAM concurrently. There is no time-slicing.

The kernel's responsibility: **ensure that one unit of software cannot access
another's memory.** The MMU is the enforcement mechanism. The kernel configures
page tables (privilege-restricted) to control which virtual addresses map to
which physical pages in each address space.

### The abstraction: memory objects

The kernel exposes **memory objects** — byte-sized, kernel-managed entities that
represent memory. Userspace creates objects and maps them into address spaces.
The page-level mechanics are hidden.

**Two-step model: create, then map.**

1. **Create** a memory object (with a size and backing type)
2. **Map** it into an address space (with permissions)

These are independent operations. An object exists before it's mapped anywhere.
The same object can be mapped into multiple address spaces (sharing). Unmapping
doesn't destroy the object.

This separation is load-bearing:

- **Backing type** is a property of the object (anonymous, device MMIO,
  file-backed)
- **Permissions** attach to the mapping (read/write/execute), not the object —
  the same object can be mapped read-only in one place and read-write in another
- **Sharing** falls out naturally — hand another thread a handle to the same
  object, it maps it into its own address space, done. No copying.

### Page size is hidden

The MMU operates in pages (fixed-size units — 4KB, 16KB, or 64KB on ARM64). This
is an implementation detail of the memory isolation mechanism. It does not leak
into the kernel interface.

When userspace creates a 100-byte object, the kernel internally allocates the
minimum number of pages needed (e.g., one 16KB page) and records the logical
size as 100 bytes. Userspace never needs to know the page size.

**Arguments for leaking page size and why they don't hold:**

1. _"Shared memory mappings have alignment requirements."_ — Only for partial
   mapping of objects. If the unit of mapping is a whole object, alignment is
   the kernel's problem. Design the API to map whole objects.

2. _"Device MMIO regions are page-aligned."_ — Drivers need device register
   layouts, not page sizes. The kernel maps the device region; the driver
   accesses offsets within it. The kernel absorbs any padding.

3. _"Performance-sensitive code needs to control TLB pressure."_ — A hint-based
   API (`preference: LARGE_MAPPINGS`) covers 95% of cases. The remaining 5%
   (database buffer pools, VM hypervisors) could query page size through a
   separate expert-facing mechanism.

4. _"Protection granularity is page-sized."_ — If permissions are per-mapping
   rather than per-address-range, this doesn't apply. Set permissions when
   creating the mapping.

**The pattern:** every argument for leaking page size follows the same
structure: the MMU operates at page granularity → some operation is constrained
by that granularity → userspace must know the granularity. Every argument
collapses the same way: redesign the operation to work at object granularity,
and the leak disappears. Page size is essential complexity of the MMU leaf node;
the memory object interface absorbs it.

An optional `query_page_size()` mechanism can exist for the rare expert use
case.

### Slab packing and the isolation rule

Small objects waste memory if each gets its own page (a 100-byte object
consuming 16KB). The kernel uses slab packing to mitigate this, governed by one
clean rule:

**Within one address space, slab packing is safe.** Objects in the same address
space share a trust boundary — the owning program can already access all its own
memory. Packing multiple small objects into one page is purely a resource
optimization with no isolation implications.

**Across address spaces, slab packing is forbidden.** The MMU enforces isolation
at page granularity. Packing objects from different address spaces into one page
would give each access to the other's data. Objects that are shared (mapped into
multiple address spaces) must have their own page(s).

**The kernel never enforces sub-page isolation boundaries.** The hardware MMU is
the sole isolation enforcement mechanism. The kernel doesn't try to replicate
memory protection at a finer granularity in software — that would be a weaker
guarantee competing with a stronger one. Kernel sub-page bookkeeping is for
resource management (tracking sizes, cleanup), never for security.

If an object is initially slab-packed and later needs to be shared, the kernel
transparently promotes it to its own page(s). This migration is hidden behind
the interface.

### Lazy allocation (demand paging)

Creating a memory object does not necessarily allocate physical pages
immediately. The kernel may defer physical allocation until userspace actually
touches each page (a page fault triggers allocation). This avoids wasting
physical RAM on memory that was allocated but never used.

This means page faults are a normal part of memory management, not just errors.
The fault handler must distinguish "valid lazy allocation" from "illegal
access."

Open policy question: what happens when physical RAM is exhausted but lazy
commitments remain? (Deny allocation up front to prevent overcommit, kill
something to free memory, block until memory is available, or report an error.)
This requires a deliberate policy decision.

---

## Cross-boundary communication

Commonly called "IPC" (inter-process communication), but the name is misleading
— the kernel may have no concept of "process." It is more precisely
**cross-isolation-boundary communication**: getting data from a thread in one
address space to a thread in another.

### Why it's a kernel concern

The kernel enforces memory isolation via the MMU. Threads in different address
spaces cannot see each other's memory. Therefore they **cannot communicate
without kernel involvement** — the isolation the kernel enforces is the barrier
the kernel must bridge.

The kernel created the wall, so the kernel must provide the door.

### Irreducible kernel involvement

Cross-boundary communication reduces to primitives already required by the
kernel's other responsibilities:

1. **Setting up shared memory regions** — requires modifying page tables (MMU,
   responsibility 1)
2. **Waking a sleeping thread** — requires the scheduler (CPU multiplexing,
   responsibility 1)
3. **Transferring capabilities (access rights)** — requires a trusted authority
   that both parties trust. In a microkernel, the kernel is the only universally
   trusted entity.

Everything else — message format, buffering, protocols — can live in userspace
libraries built on these primitives.

### Types of doors

| Mechanism                   | How it works                                                                  | Kernel involvement                                                       |
| --------------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| Shared memory               | Kernel maps same physical pages into both address spaces                      | One-time setup only — after that, threads communicate without the kernel |
| Synchronous message passing | Thread A traps to kernel, kernel transfers message to thread B, switches to B | Every message goes through the kernel                                    |
| Async notification          | Thread A traps to kernel, kernel sets a flag / wakes thread B                 | Every notification goes through the kernel, but minimal data transferred |

These compose: shared memory for bulk data, lightweight notification for "new
data is ready."

Design of the specific communication mechanism is deferred — it depends on the
memory object model and threading model being settled first.

---

## Principles that emerged

### The kernel is a leaf node behind an interface

From the perspective of interface design, the kernel is a service hiding behind
an API — the most complex leaf node in the system, but still a leaf node.
Userspace doesn't reach into the kernel's internals any more than it reaches
into a database engine's B-tree.

The interface is wider than just syscalls: **syscalls + ABI + fault delivery +
boot protocol + any shared-memory conventions** (like mapping a clock page for
fast time reads). All of these are contracts between the kernel and userspace.

The kernel is a _special_ leaf node — the one whose interface shapes all other
interfaces in the system. It defines what isolation means, what communication
looks like, and what a schedulable entity is. But encapsulation still applies:
implementation details (scheduling algorithm internals, page table format, slab
allocator design) should not leak across the boundary.

### Don't compete with the hardware, complement it

The MMU enforces page-granular isolation. The kernel builds on top of that — it
manages _which_ page table entries exist, but the CPU enforces them. Each
component does what it's best at. The kernel never tries to replicate hardware
protection at a different granularity in software.

### Mechanism is irreducible; policy is a design choice

The kernel must provide the mechanism for context switching (hardware
necessity). Whether it also owns the scheduling policy is a design choice. We
chose yes — the coupling between policy and the events that trigger it (timer,
IPC, faults) makes separation more costly than it's worth. But this is a
reasoned choice, not a necessity.

### Hardware assumptions

- Fixed number of CPU cores (no hot-swap)
- Fixed amount of physical RAM (no hot-add)
- ARM64 with MMU (page-based virtual memory)
- These are safe assumptions for a personal workstation target
