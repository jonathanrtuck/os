# Ideal Kernel-Userspace Interface

What the kernel↔userspace boundary should look like for this document-centric
OS. Derived from first principles: what does userspace need to do, and what must
the kernel provide to make that possible?

This document ignores the current implementation entirely. It starts from the
OS's 10 defining features and derives the interface. Hardware-specific
optimizations are documented in "On this hardware" sections for the current
target (M4 Pro / ARM64) but do not define the interface — the interface is
hardware-agnostic, and the kernel is a swappable leaf node behind it.

Companion documents:

- `m4-pro-kernel-design.md` — hardware-specific kernel design opportunities
- `../philosophy.md` — the two root principles that guide all design
- `../microkernel-principles.md` — what a microkernel must do and why
- `../architecture.md` — the pipeline and component responsibilities

Cross-referenced research (from the kernel project):

- `ipc-models.md` — 26 IPC mechanisms across OS history, cross-cutting tensions
- `bleeding-edge-os-landscape.md` — 2022–2026 research: verified kernels,
  async-first designs, capability innovations, zero-copy patterns

---

## Starting Point: What Does Userspace Actually Do?

From the 10 defining features, userspace has exactly these activities:

1. **The OS service** owns all document state, computes layout, builds the scene
   graph, routes input, mediates all writes. It's the center of the system.
2. **The compositor** reads the scene graph and produces pixels. Pure consumer.
   Never writes upstream.
3. **Editors** read document content (zero-copy), send write requests, receive
   input events. Untrusted, restartable.
4. **Decoders** transform compressed bytes into rendered content (PNG -> pixels,
   font -> glyphs). Sandboxed, stateless.
5. **The document store** maps persistent content into memory, manages COW
   snapshots, answers metadata queries.
6. **Drivers** translate device protocols into OS primitives. Untrusted,
   restartable.

These activities tell us what the kernel must provide.

---

## The Data Plane vs. The Control Plane

The first and most important observation: **this pipeline has two fundamentally
different communication patterns, and they shouldn't use the same mechanism.**

**Data plane — bulk, one-directional, latency-critical:**

- OS service writes scene graph -> compositor reads it
- Document store maps content -> OS service reads it
- Decoder writes pixels -> compositor reads them
- These move kilobytes to megabytes per frame. The data is structured and large.

**Control plane — small, bidirectional, infrequent:**

- Editor sends "insert character at offset 47" to OS service
- OS service sends "key event: 'a'" to editor
- Shell sends "open document X" to OS service
- These are tiny messages — a few dozen bytes each. They carry intent, not
  content.

### How other systems handle this

Most microkernel designs pick one mechanism and use it for everything:

- **seL4/L4** chose synchronous message passing (everything goes through the
  kernel, sender blocks until receiver accepts). Elegant and formally verifiable
  but forces bulk data through a narrow pipe. IPC costs ~100 cycles per message;
  sending 10,000 scene graph nodes per frame means a million cycles in IPC
  overhead.
- **Mach** chose asynchronous message queues with out-of-line memory. More
  flexible but complex — the kernel must manage message buffers, out-of-line
  memory descriptors, port rights.
- **QNX** chose synchronous message passing but with "pulse" lightweight
  notifications alongside. Closer to what this OS needs but still
  message-centric.

The foundational result for the split comes from **URPC** (Bershad et al.,
1991): kernel involvement in IPC is needed only for (a) establishing
shared-memory regions and (b) rebalancing processors. All per-transfer data
movement and synchronization can occur at user level. This decomposition —
kernel for setup and policy, userspace for data flow — is exactly the
data/control plane split.

The closest existing validation is **LionsOS** (UNSW, 2025), an seL4-based
microkernel that uses exactly this pattern: lock-free SPSC queues in shared
memory for data, seL4 notifications for signaling, synchronous IPC only for
setup and teardown. LionsOS demonstrates the pattern works at OS scale with real
workloads.

### The split for this OS

| Plane           | Mechanism                                                | Kernel involvement                                                 |
| --------------- | -------------------------------------------------------- | ------------------------------------------------------------------ |
| Data            | Shared memory (VMOs mapped into multiple address spaces) | One-time setup only — map the pages, set permissions, done         |
| Control         | Channels (small message ring buffers)                    | Per-message — kernel copies message from sender to receiver buffer |
| Synchronization | Events (bit signals + thread wake)                       | Only when a thread sleeps or wakes                                 |

The critical insight: **on the hot path (OS service writes scene graph,
compositor reads it), the kernel is not involved at all.** The data is in shared
memory. The generation counter is an atomic write (LSE2 128-bit on this
hardware). If the compositor is already awake and spinning on the generation
counter, zero syscalls occur. The kernel only participates when the compositor
has nothing to do and wants to sleep.

This is fundamentally different from seL4-style IPC where every communication,
no matter how small, traps through the kernel. And it's a perfect fit for the M4
Pro's unified memory — "shared memory" means literally the same physical pages,
the same cache hierarchy, with hardware coherence doing the work.

### Why the hardware guides this split

The M4 Pro's memory hierarchy has a natural producer-consumer topology: CPU
writes to L1/L2, data flows to SLC, GPU reads from SLC. The kernel inserting
itself in this flow (copying data through kernel buffers) would break the
hardware's natural coherence path. The data plane should be invisible to the
kernel because the hardware already handles it.

Barrelfish (ETH Zürich, 2009) reached a similar conclusion for a different
reason — on NUMA systems, shared memory within a node is fast but cross-node
message passing is expensive. They split communication into "local shared memory

- remote messages." Same principle: let the hardware topology guide the
  communication model.

---

## The Kernel Object Model (5 Objects)

Working backwards from what userspace needs, the kernel manages exactly five
object types. Each maps to a hardware mechanism or to a consequence of
isolation.

### 1. Memory Object (VMO)

**What it is:** A sized region of physical memory that can be mapped into one or
more address spaces with per-mapping permissions.

**Why the kernel must own it:** Only the kernel can modify page tables (MMU is
privilege-restricted). Memory objects are the kernel's abstraction over page
table entries.

**Operations userspace needs:**

| Operation                 | What it does               | Why                                                                                       |
| ------------------------- | -------------------------- | ----------------------------------------------------------------------------------------- |
| `create(size, flags)`     | Allocate a memory object   | Documents, scene graphs, decode buffers, IPC ring buffers — everything is a memory object |
| `map(vmo, perms)`         | Map into my address space  | An editor maps a document VMO read-only; the OS service maps it read-write                |
| `unmap(addr)`             | Remove mapping             | Cleanup when done with a document                                                         |
| `snapshot(vmo)`           | COW clone                  | **The undo primitive.** O(1), shares all physical pages until divergence                  |
| `seal(vmo)`               | Make permanently immutable | Decoded content that will never change. Invalidates all writable PTEs atomically          |
| `set_pager(vmo, channel)` | Assign a userspace pager   | Document store supplies pages on demand from disk. Decoders supply decoded pages lazily.  |

**What's different from existing kernels:**

- **COW snapshot is a first-class operation**, not a filesystem feature. Zircon
  has `vmo_create_child(SNAPSHOT)` which is similar. seL4 doesn't have this —
  you'd build it entirely in userspace over Untyped memory, which is possible
  but pushes COW bookkeeping into a userspace allocator. Since undo is so
  fundamental to this OS (every edit creates a snapshot), making it a kernel
  primitive keeps the hot path fast — a snapshot is just "increment page
  refcounts, mark PTEs copy-on-write." The kernel is already managing these data
  structures.

- **Seal is hardware-enforced.** When a VMO is sealed, the kernel walks all page
  table entries that map it and clears the write bit. Any future write fault on
  a sealed VMO is an immediate error, not a COW trigger. This is stronger than
  userspace-only immutability — even a kernel bug can't accidentally allow
  writes through a stale TLB entry, because the PTEs themselves are read-only.
  EROS/Coyotos had a similar concept ("sensory capabilities" that are
  permanently read-only), but at the capability level rather than the memory
  level.

- **Pagers are assigned per-VMO, not per-thread.** Mach assigns an external
  pager per memory region. seL4 assigns a fault handler per-thread. Per-VMO is
  the right granularity for this OS because different VMOs have different
  backing stores: document content comes from the document store, decoded pixels
  come from decoders, anonymous memory is kernel-managed. The kernel routes
  faults based on which VMO was accessed, not which thread faulted.

**On this hardware (M4 Pro):** The 16 KB page size means COW snapshots are
coarser-grained (16 KB minimum copy unit) but require fewer page table entries.
A 50 KB document is 4 pages — snapshotting it touches 4 PTEs. The 81 ns page
walk penalty makes TLB misses expensive, so contiguous physical allocation for
VMOs (reducing TLB entries) matters. A `HINT_CONTIGUOUS` flag on `create` would
let the kernel attempt contiguous allocation, potentially mapping 4 contiguous
16 KB pages with a single TLB entry via the contiguous bit.

### 2. Channel

**What it is:** A bidirectional message pipe connecting exactly two endpoints.
Small fixed-size messages. Handle transfer capability.

**Why the kernel must own it:** Channels cross isolation boundaries. The kernel
must mediate because it enforces the isolation (MMU). The kernel provides the
door because it built the wall.

**Operations:**

| Operation                        | What it does                                    | Why                                                                                           |
| -------------------------------- | ----------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `create()`                       | Create a channel pair (two endpoint handles)    | Every service-to-service connection starts with a channel                                     |
| `send(endpoint, msg, handles[])` | Send a message, optionally transferring handles | Editor sends write request; OS service sends input event; init distributes handles at startup |
| `recv(endpoint, timeout)`        | Receive a message                               | The other end of send                                                                         |
| `close(endpoint)`                | Destroy one endpoint                            | Peer gets a notification (peer-closed event)                                                  |

**Naming model.** Channels use point-to-point naming: `create()` produces a
matched pair, each endpoint is a capability to the other end. This is closer to
Singularity/Midori's typed channels than to seL4's shared endpoints. The IPC
models survey identifies two models: Model A (capability IS the destination,
EROS/KeyKOS) and Model B (capability TO a destination, seL4 endpoints). This
design is Model A — the channel endpoint IS the destination. No separate
endpoint object. The tradeoff: seL4's shared endpoints allow many-to-one
multiplexing (multiple clients waiting on one endpoint with badge-based
identification). Point-to-point channels achieve the same thing differently: the
server holds N channel endpoints and uses `event_wait` to know which has data.
This is more explicit — the server knows exactly who it's talking to.

**Messages are small and fixed-size.** The control plane carries intent ("insert
'a' at offset 47"), not content (the actual document bytes). Each message slot
is one hardware cache line (queryable via `system_info(CHANNEL_MSG_SIZE)` — 128
bytes on ARM64/Apple Silicon, 64 bytes on x86-64). One message = one cache line
write = no false sharing. The message contains: a type tag, a small payload, and
optionally handle indices being transferred.

**Request/reply pattern.** Channels are bidirectional — both endpoints can send
and receive. Request/reply is the natural use: editor sends a write request, OS
service sends the acknowledgment back on the same channel. No separate reply
object (unlike seL4's one-shot reply capabilities) or explicit MsgReply step
(unlike QNX's triad). EROS's resume keys solve the same problem differently —
the kernel creates a one-shot reply capability per CALL. The bidirectional
channel is simpler: both directions are always available. The tradeoff: a
misbehaving editor could send unsolicited messages on its "reply" direction.
Since editors are untrusted and the OS service validates all input anyway, this
is acceptable.

**Handle transfer is the key capability operation.** When the OS service creates
a read-only VMO handle for an editor, it sends the handle over the channel. The
kernel moves the handle from the sender's handle table to the receiver's,
attenuating rights in transit. This is how capabilities propagate — through
explicit, auditable channels. No ambient authority, no global namespace.
Singularity/Midori achieved similar zero-copy handle transfer through linear
types in the exchange heap — Rust's ownership semantics could enforce the same
invariant at the language level, with kernel-mediated transfer as the runtime
mechanism.

**What's different:**

- **No synchronous send/receive coupling.** seL4's IPC requires the sender and
  receiver to rendezvous — the sender blocks until the receiver calls receive.
  This creates tight coupling that doesn't match the pipeline model. Channels
  here are asynchronous with a small kernel-managed buffer (maybe 16 message
  slots). If the buffer fills, the sender blocks — that's backpressure, not a
  design flaw.

- **Handle transfer is atomic with the message.** The message and the handles
  arrive together or not at all. This prevents the inconsistency where a message
  references handles that haven't arrived yet. QNX has a similar property with
  its pulse/message model.

**Implementation model: shared-memory ring with kernel-mediated handles.** The
channel ring buffer is a VMO mapped into both endpoints' address spaces. Message
data is written directly by the sender and read directly by the receiver — no
kernel copy of message bytes. The kernel is involved only when: (a) a message
carries handles (the kernel moves handle table entries atomically), (b) the
receiver is sleeping (the kernel wakes it via the channel's associated event),
or (c) the buffer is full and the sender must block.

This is the same hybrid that LionsOS uses: shared-memory for data transfer,
kernel for notification and capability mediation. The data/control split applies
within the control plane itself — message data is "data plane" (shared memory),
handle transfer is "control plane" (kernel-mediated).

**On any hardware:** Each message slot is one cache line by construction. A ring
buffer of 16 slots fits in L1 on any modern CPU (1-2 KB depending on line size).
Sending is: write message to slot (one cache line write), increment write index
(atomic), optionally wake receiver (syscall if receiver is sleeping). Handle
table entry manipulation is the only kernel work — the message bytes never cross
the privilege boundary.

**On this hardware (M4 Pro):** 128-byte cache lines, so 16 slots = 2 KB. LSE
atomics for the write index. The ring buffer is permanently in L1.

### 3. Event

**What it is:** A set of signal bits (32 or 64) that can be waited on. The
universal synchronization primitive.

**Why the kernel must own it:** Waking a sleeping thread requires the scheduler
(privilege-restricted). Events bridge the gap between "something happened" and
"a thread that cares should run."

**Operations:**

| Operation                         | What it does                           | Why                                                                              |
| --------------------------------- | -------------------------------------- | -------------------------------------------------------------------------------- |
| `create()`                        | Create an event object                 | Compositor creates an event the OS service signals when the scene graph is ready |
| `signal(event, bits)`             | Set bits, wake waiters                 | OS service signals "new frame ready" (bit 0), "document changed" (bit 1), etc.   |
| `wait(events[], bits[], timeout)` | Sleep until any event fires or timeout | Compositor waits on scene-ready event; editor waits on input-event event         |
| `clear(event, bits)`              | Clear bits after handling              | Reset after processing                                                           |

**Events unify all asynchronous notifications.** Interrupts, timer expiry,
channel readability, thread exit, peer-closed — the kernel delivers all of these
as event signals. There's no separate `wait_for_interrupt` or `wait_for_child`
or `select`/`poll`/`epoll` API. Just events.

**What's different:**

- **Multi-wait is first-class.** `wait(events[], ...)` takes an array of events
  and returns when any fires. The compositor might wait on both "scene-ready"
  and "window-resized." The OS service might wait on "input-event" OR
  "editor-write-request" OR "timer-tick." This is similar to Zircon's
  `port_wait` but simpler — no port object needed, just an array of event
  handles.

- **Events replace Unix signals entirely.** No SIGTERM, no SIGCHLD, no signal
  handlers. If you want to know when a child exits, you wait on its exit event.
  If you want to ask a process to shut down, you signal its shutdown event. The
  process checks the event when it's ready (cooperative) or the kernel preempts
  it after a deadline (non-cooperative). This avoids the entire mess of signal
  safety, reentrant handlers, and the async-signal-safe function list.

- **Events are level-triggered, not edge-triggered.** If a bit is set and you
  wait on it, you wake immediately. This prevents missed wakeups without
  requiring the waiter to be in a `wait()` call at the exact moment the signal
  fires. Edge-triggered events (Linux's eventfd with EFD_SEMAPHORE) have a class
  of bugs where signals are missed if the consumer is between processing and
  waiting. The tradeoff: level-triggered bits coalesce — two signals before the
  consumer wakes produce one wakeup. For continuous-state signals ("new frame
  ready") this is correct — you only care about the latest. For discrete events
  that must be counted ("N documents changed"), use a shared-memory counter
  alongside the event bit, or send individual channel messages.

**Channel-event binding.** Every channel has an implicit event: the kernel
signals the channel's event when a message arrives. This bridges channels and
the event wait model — a service that handles messages from multiple channels
includes each channel's event in its `event_wait` array. When any fires, the
service calls `channel_recv` on the corresponding channel. seL4 solves this with
notification-endpoint binding. QNX delivers pulses through MsgReceive. Zircon
uses port-based waiting. This design keeps it simpler: events are the universal
waitable, and channels generate events automatically. No additional object type
or binding step required.

**On this hardware (M4 Pro):** `wait()` with a timeout maps directly to `WFE` +
timer. The core sleeps in a power-efficient state (WFE) until either an event
arrives (another core signals it via `SEV`) or the timer fires. Zero polling,
zero wasted power. The 24 MHz timer (41.7 ns resolution) is fine for scheduling
granularity.

### 4. Thread

**What it is:** A schedulable execution context — a saved register set
(including program counter) that the kernel loads onto a core.

**Operations:**

| Operation                             | What it does                                 | Why                                                  |
| ------------------------------------- | -------------------------------------------- | ---------------------------------------------------- |
| `create(entry, stack_vmo, arg)`       | Create a thread in the current address space | OS service spawns worker threads for parallel layout |
| `exit(code)`                          | Terminate the calling thread                 | Cleanup                                              |
| `set_priority(thread, priority)`      | Set scheduling priority                      | Pipeline stages have different latency requirements  |
| `set_affinity(thread, topology_hint)` | Hint preferred placement                     | Co-locate producer/consumer for cache locality       |

**Threads are independent of address spaces in the kernel's model.** A thread
runs _in_ an address space but is conceptually separate. Creating a thread
doesn't create an address space. Multiple threads share an address space. This
follows seL4's model (TCB is independent of VSpace) rather than the POSIX model
(fork creates both).

**The scheduler is priority-based, topology-aware.** Three priority tiers
matching the pipeline:

| Priority | Pipeline role                    | Placement intent                                    |
| -------- | -------------------------------- | --------------------------------------------------- |
| High     | Input routing, cursor updates    | Performance cores, shared cache with render threads |
| Medium   | Layout, scene build, compositing | Performance cores (co-located with high-priority)   |
| Low      | Indexing, snapshot GC, pruning   | Efficiency cores / lowest-power available           |

The placement column is intent, not prescription. The kernel maps intent to
hardware topology internally — userspace never names specific cores or clusters.
On heterogeneous hardware (Apple Silicon, Alder Lake), the kernel places
high-priority threads on performance cores and low-priority on efficiency cores.
On homogeneous hardware, placement is irrelevant and the kernel ignores it.

**Topology hints.** `set_affinity(thread, topology_hint)` takes an opaque value
obtained from `system_info(TOPOLOGY)`. The topology descriptor tells userspace
what the hardware looks like (clusters, NUMA nodes, or "flat"), and userspace
picks a hint meaning "same group as thread X" or "prefer efficiency cores." The
kernel interprets the hint per-platform. This is a one-time-at-init query — zero
per-frame cost.

**What's different:**

- **No EEVDF, no CFS, no complex fairness algorithm.** Those are designed for
  dozens-to-thousands of competing processes where fairness matters. With ~10
  services and clear priority relationships, a simple fixed-priority preemptive
  scheduler (like QNX or seL4) is correct and much simpler. Higher-priority
  thread preempts lower-priority. Same-priority threads round-robin. Done.

- **Topology affinity is a first-class hint.** Most schedulers treat CPU
  affinity as an afterthought (`sched_setaffinity` on Linux). Here, topology
  awareness is integral because cache locality can have an order-of-magnitude
  impact on inter-thread communication latency. The scheduler should prefer the
  affinity hint, only violating it under sustained load imbalance.

- **No thread-level capabilities/handle tables.** Handles belong to address
  spaces, not threads. All threads in an address space share the same handle
  table. This simplifies the model — access control is spatial
  (per-address-space), not temporal (per-thread). seL4 disagrees here
  (capabilities are per-CNode, which is per-thread), but for this OS's service
  topology, per-address-space is the right granularity.

- **Priority inversion is bounded by topology.** QNX provides automatic priority
  inheritance through its MsgSend/MsgReceive/MsgReply triad — the server's
  effective priority rises to match the highest-priority blocked client.
  Composite OS gets natural priority inheritance from thread migration (the
  calling thread keeps its priority in the server's domain). With shared- memory
  channels (no blocking send in the common case) and a small fixed service
  topology, priority inversion is less likely — but not impossible. If a
  low-priority background task holds a lock that a high-priority path needs,
  classic inversion occurs. For now, the pipeline's strict priority ordering
  (input > render > background) and the absence of shared locks between priority
  tiers makes this manageable. If it becomes a problem, channel-level priority
  inheritance (boost the receiver when a higher-priority sender blocks on a full
  buffer) is the minimal mechanism.

**On this hardware (M4 Pro):** Context switch saves/restores ~34 general-purpose
registers + ~32 SIMD registers + a few system registers. PAC keys (5 x 64 bits)
are loaded per-address-space, not per-thread (threads sharing an address space
share PAC keys). SME state is lazily managed — the kernel only saves/restores
the ZA array (up to 4 KB) when the next thread actually uses SME instructions,
which most threads never do.

### 5. Address Space

**What it is:** An isolation boundary — a set of page table mappings that
defines what memory a group of threads can access.

**Operations:**

| Operation                                         | What it does                     | Why                                         |
| ------------------------------------------------- | -------------------------------- | ------------------------------------------- |
| `create()`                                        | Create a new empty address space | Starting a new service or sandboxed decoder |
| `destroy(space)`                                  | Tear down the address space      | Cleaning up after a crashed editor          |
| `exec(space, code_vmo, entry, initial_handles[])` | Populate and start a thread      | The compound "launch a service" operation   |

**Address spaces are thin.** They're just a page table root pointer + a handle
table. Creating one is fast — allocate a root page table page (16 KB),
initialize it with the kernel's upper-half mappings, done. Destroying one is:
unmap all VMOs, close all handles, free page table pages.

**What's different:**

- **No process object in the kernel.** "Process" is a userspace convention — an
  address space + its threads + its handles. The kernel never bundles these.
  This is seL4's model. Zircon disagrees (Process is a kernel object that owns
  threads and a handle table). The argument for no-process: it's simpler, more
  flexible, and the kernel doesn't need to know what a "process" means. The
  argument against: bookkeeping is harder when cleanup requires finding all
  associated threads and handles.

  **Lifecycle management without a process object.** The address space IS the
  lifecycle boundary. The kernel already tracks which threads run in each
  address space (it must — context switch loads the page table root from the
  address space). It already tracks which handles belong to each address space
  (the handle table is per-address-space). When `space_destroy(space)` is
  called: (1) kill all threads whose address space is `space`, (2) close all
  handles in the address space's handle table (triggering peer-closed events on
  the remote ends of channels), (3) unmap all VMO mappings, (4) free page table
  pages. All the bookkeeping data structures are already per-address-space — no
  separate "process" object needed to aggregate them. The address space is the
  natural owner because it's the entity the hardware knows about (the MMU
  enforces it).

- **The 128 address space limit (from the hypervisor) is a feature.** The kernel
  uses a flat `[AddressSpace; 128]` array. Lookup is O(1) by index. No hash
  table, no allocation. The array is 128 x 128 bytes = 16 KB = one page,
  permanently in the TLB.

**On this hardware (M4 Pro):** Each address space gets a unique CONTEXTIDR_EL1
value (for CSV2 branch predictor isolation) and unique PAC keys (for pointer
authentication isolation). These are loaded on context switch between address
spaces but not on thread switches within the same address space. ASID (Address
Space ID) tags TLB entries so that switching between address spaces doesn't
require a TLB flush — the hardware filters entries by ASID.

### Object count and comparison

The five kernel objects (VMO, Channel, Event, Thread, Address Space) correspond
directly to the three irreducible responsibilities plus their consequences:

- **Multiplex CPU** -> Thread, and the scheduler that manages them
- **Multiplex RAM** -> VMO and Address Space (the memory objects and the
  isolation boundary)
- **Route interrupts/faults** -> Event (the universal notification mechanism)
- **Bridge isolation** -> Channel (the door through the wall the kernel created)

Every object earns its existence by tracing back to a hardware restriction.
There's no sixth object because there's no sixth hardware reason. If something
feels like it should be a kernel object but can't trace back to one of these, it
belongs in userspace.

For comparison: Zircon has ~20 kernel object types (Process, Thread, VMO, VMAR,
Channel, Socket, Port, Event, EventPair, Timer, Fifo, Interrupt, ...). seL4 has
~8 (TCB, CNode, VSpace, Endpoint, Notification, Reply, Untyped, Frame). EROS has
3 (Page, Node, Process). Fewer object types means a smaller trusted computing
base, fewer interactions to reason about, and a smaller attack surface.

**Verification tractability.** Five object types and 25 syscalls is small enough
to be verification-tractable. Atmosphere (SOSP 2025) verified a Rust L4-style
microkernel with comparable complexity using Verus, achieving a 7.5:1
proof-to-code ratio (vs. seL4's ~20:1 in Isabelle/HOL) in ~2 person-years. Since
this kernel is already in Rust, the Verus path is structurally available. The
Asterinas "framekernel" approach (ATC 2025) is also relevant: confine all
`unsafe` to a small core library (~14% of codebase) and write the remaining
kernel services in safe Rust. The 25-syscall interface becomes the safe API
surface; the unsafe core handles MMU manipulation, context switch, and exception
entry. These are not immediate priorities — but the interface's small surface
area preserves the option.

---

## The Syscall Table

Derived from the five objects and their operations:

```text
-- Memory Objects ------------------------------------------
vmo_create(size, flags)                -> handle
vmo_map(vmo, addr_hint, perms)         -> addr
vmo_unmap(addr)
vmo_snapshot(vmo)                      -> handle
vmo_seal(vmo)
vmo_resize(vmo, new_size)
vmo_set_pager(vmo, channel)

-- Channels ------------------------------------------------
channel_create()                       -> (handle, handle)
channel_send(endpoint, msg, handles[])
channel_recv(endpoint, timeout)        -> (msg, handles[])

-- Events --------------------------------------------------
event_create()                         -> handle
event_signal(event, bits)
event_wait(events[], bits[], timeout)  -> (which, fired_bits)
event_clear(event, bits)

-- Threads -------------------------------------------------
thread_create(entry, stack_vmo, arg)   -> handle
thread_exit(code)
thread_set_priority(thread, priority)
thread_set_affinity(thread, topology_hint)

-- Address Spaces ------------------------------------------
space_create()                         -> handle
space_destroy(space)
space_exec(space, code_vmo, entry,
           initial_handles[])          -> thread_handle

-- Handles -------------------------------------------------
handle_dup(handle, reduced_rights)     -> handle
handle_close(handle)
handle_info(handle)                    -> (type, rights)

-- System --------------------------------------------------
clock_read()                           -> timestamp
system_info(what)                      -> info
```

**25 syscalls.** Compare: Linux has ~450. Zircon has ~170. seL4 has ~12 (but
those 12 are highly polymorphic — `seL4_Call` does different things depending on
the capability type).

### Notable absences

- No `fork` or `exec` — use `space_create` + `space_exec`
- No `open/read/write/close` for files — the filesystem is a userspace service
  accessed through channels
- No `socket/bind/listen/accept` — the network stack is userspace
- No `ioctl` — device drivers are userspace, accessed through channels + VMOs
- No `mmap` as a general-purpose call — `vmo_map` is scoped to kernel-managed
  objects
- No `kill` or signals — use `event_signal` on a shutdown event
- No `wait/waitpid` — wait on a thread-exit event

### Design notes

`clock_read` could be replaced by a mapped page (the timer counter CNTVCT_EL0 is
readable from EL0 on this hardware). But having it as a syscall provides a
fallback and lets the kernel virtualize time if needed.

`space_exec` is the interesting one — it combines "populate an address space
with code" and "start its first thread." This is the only compound operation.
The alternative is separate `vmo_map` into the new space + `thread_create` in
the new space, but that requires the creating process to be able to map into
another address space — a privilege that shouldn't be general-purpose (it breaks
isolation). `space_exec` keeps the privilege in the kernel: the kernel maps the
code VMO into the new space and starts the thread. The creator never has write
access to the new space.

---

## The Fast Path: Zero Syscalls on the Hot Path

Here's how the render pipeline works with this interface. The pattern is
hardware-agnostic — it relies only on cache-coherent shared memory and atomic
operations, both universal on modern SMP systems.

```text
OS service (co-located core)          Compositor (same cache group)
----------------------------          ----------------------------
1. Write scene nodes to
   scene_graph_vmo
   (regular memory writes,
   hits L1/L2, no syscall)

2. Atomic store to generation
   counter (no syscall)
                                      3. Sees new generation
                                         (atomic load, no syscall)

                                      4. Reads scene nodes
                                         (shared cache hit,
                                         no syscall)

                                      5. Submits to GPU
                                         (writes to GPU command VMO,
                                         no syscall)
```

**Zero syscalls for the entire frame.** The kernel isn't involved. The data
flows through the cache hierarchy. The hardware coherence protocol ensures
visibility. This works on any SMP system with coherent caches — the specific
cache topology affects latency (L2-local vs cross-node), not correctness.

The only time a syscall occurs is when the compositor has no work and wants to
sleep:

```text
compositor: event_wait([scene_ready_event], timeout=16ms)
            // kernel puts core in low-power wait

os_service: event_signal(scene_ready_event, NEW_FRAME)
            // kernel wakes compositor's core

compositor: // wakes, reads new generation, renders
```

The **steady-state frame cost is: memory writes + one atomic store + one atomic
load.** No privilege transitions, no TLB flushes, no register saves. The kernel
becomes invisible during active rendering.

### The data plane synchronization protocol

The generation counter is a **userspace convention**, not a kernel interface.
The kernel provides the shared memory (`vmo_create` + `vmo_map`) and the
notification mechanism (`event_signal` / `event_wait`). What userspace writes
into the shared memory is a library concern.

Two implementation strategies, both producing identical performance on their
respective hardware:

**Wide atomic (ARM64 with LSE2, x86-64 with AVX):** A single atomic store/load
of generation + metadata. One instruction each way. Usable when the hardware
provides single-copy-atomic wide loads/stores.

**Seqlock (universal fallback):** Producer increments a 64-bit sequence counter
(odd = writing), writes metadata, increments again (even = valid). Consumer
reads counter, reads metadata, reads counter again — if both reads match and
even, data is consistent. Three loads instead of one, plus one compare-branch.

Cost difference at display refresh rates: effectively zero. The seqlock's extra
loads are L1 hits (the consumer is spinning on a hot line), adding ~1 cycle per
check. The retry path fires only when the consumer reads during the producer's
write window — a few nanoseconds in a 16.6ms frame, probability ~0.000006%. The
"generic" path produces the same performance as the hardware-specific path
because the bottleneck (cache line transfer between cores) dominates both.

**On this hardware (M4 Pro):** LSE2 guarantees single-copy-atomic 128-bit
STP/LDP for naturally aligned addresses. The generation counter uses the wide
atomic path. WFE/SEV map to `event_wait`/`event_signal` for power-efficient
sleep/wake.

### Why this differs from seL4-style kernels

In seL4, every cross-process communication goes through the kernel — even if
it's just a 4-byte notification. That's by design (the kernel is the single
enforcement point for all information flow). The cost is ~100 cycles per IPC x
thousands of IPCs per frame = significant overhead.

This design says: the kernel enforces isolation at _setup time_ (when VMOs are
mapped and handles are transferred). After setup, the pipeline runs at hardware
speed with no kernel involvement. The kernel is a bouncer at the door, not a
courier carrying every message.

The tradeoff: the kernel can't monitor or rate-limit data plane traffic after
setup. If a compromised OS service writes garbage to the scene graph VMO, the
compositor will render garbage. But in this OS, the OS service IS the trusted
core — if it's compromised, the system is already lost. The security boundary is
between the OS service and untrusted editors/decoders, and that boundary is
enforced by VMO permissions (editors get read-only handles).

---

## What's Genuinely Novel

Most of this interface has precedent in existing systems. But the specific
combination — and the things it enables for this OS — is interesting:

### 1. COW snapshots as the undo mechanism, at the kernel level

EROS had persistent memory. Zircon has VMO snapshots. But nobody has used
kernel-level COW snapshots specifically as the undo primitive for a
document-centric OS. Every `beginOperation` call in the edit protocol becomes:
`vmo_snapshot(document_vmo)` -> done. The "undo stack" is a ring of VMO handles
pointing at COW snapshots. Restoring a snapshot is: `vmo_unmap` current +
`vmo_map` snapshot. The kernel does the page table manipulation; userspace just
holds handles.

### 2. Seal as the immutability guarantee for the pipeline

When a decoder finishes decoding a PNG, it seals the output VMO. The kernel
hardware-enforces that no one — not the decoder, not the OS service, not a
kernel bug — can modify those pixels. The compositor can cache the texture
knowing it will never change. This is stronger than a "const pointer" or a
"read-only mapping" — it's an irrevocable property of the memory object itself.

The principle — immutability substitutes for copying as a protection mechanism —
was first articulated by Druschel & Peterson in **fbufs** (SOSP 1993) and
generalized by **IO-Lite** (OSDI 1999). Fbufs are read-only shared buffers
mapped into multiple address spaces simultaneously; once created, contents are
frozen. The key insight: if no domain can modify a shared buffer, sharing does
not compromise isolation. Sealed VMOs are this same mechanism at the memory
object level, with hardware PTE enforcement instead of software convention.

### 3. The data plane is invisible to the kernel

In most microkernels, the kernel sees every cross-process interaction. Here, the
vast majority of data movement (scene graph, document content, decoded pixels)
happens in shared memory with no kernel involvement after initial setup. The
kernel manages the capability (who can map what) but not the data flow (what
gets written when).

### 4. Events as the universal async primitive

No separate API for interrupts, timers, child-process exit, or channel
readability. One `wait()` call handles all of them. A service that needs to
respond to input events, timer ticks, and editor messages does a single
multi-wait on three event handles. This is cleaner than
`select`/`poll`/`epoll`/`kqueue` because there's only one kind of waitable
thing.

### 5. No process object — "process" is a userspace pattern

The kernel manages threads and address spaces independently. What userspace
calls a "process" (the OS service, the compositor, an editor) is an address
space + its threads + its handles. But this bundling is userspace's concern. The
kernel just schedules threads and enforces isolation at address space
boundaries.

---

## Hardware Portability

The interface is hardware-agnostic. Userspace ports to new hardware by swapping
the kernel — the kernel is a leaf node behind the 25-syscall interface, and the
interface absorbs all hardware variation.

### What the interface requires from hardware

| Requirement                  | Why                                      | How universal                    |
| ---------------------------- | ---------------------------------------- | -------------------------------- |
| MMU with page tables         | VMOs, address spaces, isolation          | Every modern application CPU     |
| Preemption timer             | Thread scheduling, `event_wait` timeout  | Universal                        |
| Privilege levels             | Kernel/userspace split                   | Universal (EL0/EL1, ring 0/3)    |
| Cache-coherent shared memory | Zero-syscall data plane                  | Every modern SMP workstation CPU |
| Atomic load/store (≥64-bit)  | Generation counters, ring buffer indices | Universal                        |

No ARM64-specific, Apple-specific, or M4-specific features are required. The
interface works on x86-64, RISC-V, or any future ISA that provides the above.

### What varies per hardware (kernel-internal, not interface)

| Parameter              | Interface                                        | Kernel implementation                            |
| ---------------------- | ------------------------------------------------ | ------------------------------------------------ |
| Cache line size        | `system_info(CHANNEL_MSG_SIZE)`                  | 128 bytes (M4 Pro), 64 bytes (x86-64)            |
| Core topology          | `system_info(TOPOLOGY)` + opaque `topology_hint` | P/E clusters, NUMA nodes, flat                   |
| Atomic width           | Userspace library chooses wide-atomic or seqlock | LSE2 128-bit, CMPXCHG16B, 64-bit fallback        |
| Page size              | Hidden — never exposed to userspace              | 16 KB (ARM64), 4 KB (x86-64)                     |
| TLB management         | Hidden — kernel handles flushes                  | ASID (ARM64), PCID (x86-64)                      |
| Wait/wake mechanism    | `event_wait` / `event_signal`                    | WFE/SEV (ARM64), MONITOR/MWAIT (x86), futex-like |
| Pointer authentication | Hidden — kernel loads keys on context switch     | PAC (ARM64), CET (x86-64), none                  |

### Why there is no performance tradeoff

The three hardware-specific parameters in the original document — cache line
size, cluster topology, and atomic width — are all resolved at init time or
compile time, never at per-operation time:

1. **Message slot size** is a compile-time platform constant (like calling
   conventions). On M4 Pro it's 128 bytes; on x86-64 it's 64 bytes. Each
   platform gets cache-line-aligned messages. Zero per-message overhead from the
   abstraction.

2. **Topology placement** is queried once at init via `system_info(TOPOLOGY)`.
   The kernel also applies topology-aware scheduling internally — high-priority
   threads go to performance cores, low-priority to efficiency cores — without
   any userspace involvement. Zero per-schedule overhead.

3. **Generation counter protocol** is a userspace library concern, not a kernel
   interface. On hardware with wide atomics, use them. On hardware without, use
   a seqlock — cost is ~1 extra cycle per check (L1-hot compare-and-branch),
   with retry probability ~0.000006% at display refresh rates. Effectively zero.

The generic interface on M4 Pro produces bit-for-bit identical machine code on
the hot path. The genericization happens at init time or compile time, never at
per-frame time.

### The one genuine constraint: cache coherence

The zero-syscall data plane assumes hardware cache coherence between producer
and consumer. On non-coherent hardware (some embedded, FPGA, CPU→GPU without
unified memory), shared-memory writes aren't automatically visible to the
reader. Explicit cache maintenance may require privilege.

Even here, the interface absorbs it: `event_signal` is where the kernel can
inject cache maintenance on non-coherent hardware. On coherent hardware it's a
lightweight wake. On non-coherent hardware it adds a cache flush before the
wake. Same interface, different kernel implementation, but with a real
per-signal cost. For the target platform (personal workstations), this is moot —
coherent caches are universal.

---

## Open Design Tensions

These are the places where reasonable designs diverge. Each represents a real
tradeoff, not a clear winner.

### 1. Pager complexity

Per-VMO pagers are powerful (the document store can supply pages on demand) but
add kernel complexity. The fault path becomes: thread faults -> kernel checks if
VMO has pager -> kernel sends fault message to pager via channel -> pager
supplies page -> kernel maps page -> thread resumes. That's a multi-step
protocol in the kernel's fault handler. seL4 keeps this simpler (fault handler
is just another endpoint capability). EROS avoids it entirely (all pages are
always in memory, swapped by the kernel).

**Is a pager worth the complexity?** For a personal OS with 48 GB of RAM and ~1
GB of documents, you could map the entire document corpus at boot and never
page. The pager is only essential if lazy decoding matters (don't decode a 50 MB
JPEG until the user scrolls to it). That's a real use case — but is it common
enough to justify kernel complexity? This deserves a spike.

### 2. Channel buffering

How deep should the channel buffer be? A 16-slot ring buffer means a fast sender
can enqueue 16 messages before blocking. That's fine for editor -> OS service
(editors don't produce bursts). But for interrupt delivery (driver -> OS
service), a burst of interrupts could overflow a shallow buffer. The
alternative: dynamically-sized buffers, which adds kernel allocation on the IPC
hot path.

### 3. The `space_exec` compound operation

Is it the right abstraction? It bundles code loading + thread creation + initial
handle distribution. An alternative: give the parent explicit (but restricted)
access to map into the child's address space during setup, then start the thread
as a separate step. This is more flexible but grants a transient "write into
another process" capability — a wider privilege surface. Zircon gives the parent
explicit VMAR access to the child's address space; seL4 uses CNode manipulation.

### 4. Event dispatch model

Should `event_wait` return which event fired (poll model), or should events be
callback-based (the kernel dispatches directly to a handler)? The poll model is
simpler but has one extra dispatch step in userspace. The callback model is
faster but creates re-entrancy complexity. For ~3 events per service, the
dispatch cost is negligible — simplicity wins.

### 5. Capability revocation

The interface provides `handle_dup(handle, reduced_rights)` to attenuate and
`handle_close(handle)` to release, but no mechanism to **retroactively revoke**
a handle that has been given away. When the OS service gives an editor a
read-only VMO handle to a document, can it later revoke that access?

The current answer is coarse: kill the editor's address space (`space_destroy`),
which destroys all its handles and mappings. For untrusted, restartable editors,
this is sufficient — revocation means restart.

But fine-grained revocation (revoke one VMO handle without killing the entire
editor) is harder. The research landscape offers several approaches:

- **seL4 CSpace delete:** Walk the capability derivation tree, O(tree depth).
  Requires maintaining a derivation tree in the kernel.
- **Cornucopia Reloaded (ASPLOS 2024):** Per-page load barriers, inspired by GC.
  No stop-the-world pauses. Requires hardware page load barrier support (Arm
  Morello / CHERI).
- **Epoch-based (Barrelfish):** Deferred revocation, eventually complete. No
  hardware requirements but weaker timeliness guarantees.

For this OS, coarse revocation (space_destroy) may be enough given the ~10
service topology. If fine-grained revocation becomes necessary, a
**generation-count approach** is lightweight: each VMO has a generation counter.
Handles carry a generation stamp. The kernel rejects operations on handles whose
generation doesn't match the VMO's current generation. Revocation is: increment
the VMO's generation. All outstanding handles become stale without walking any
derivation tree. The downside: stale handles are only detected on use, not
eagerly invalidated — but for a cooperative (non-adversarial-between-services)
system, detection on use is fine.

### 6. Bootstrap protocol

The syscall table defines
`space_exec(space, code_vmo, entry, initial_handles[])` but doesn't specify how
the **first** address space gets its handles. The bootstrap sequence:

1. Kernel starts. It owns all physical memory and the hardware.
2. Kernel creates the init address space, loads init's code VMO.
3. Kernel creates a set of **bootstrap handles** and passes them to init via
   `initial_handles[]`: a handle to its own address space, a handle to a "root
   VMO" (all physical memory or a designated region), and handles to interrupt
   events for hardware the init service will manage.
4. Init uses these bootstrap handles to create channels, spawn services via
   `space_exec`, and distribute handles to each service via their respective
   `initial_handles[]`.

After bootstrap, the kernel has no special relationship with init. Init is just
another address space that happens to hold the root handles. Capability
authority flows entirely through explicit handle transfers.

This is similar to seL4's boot protocol (initial thread gets all Untyped
capabilities) and Hubris's build-time-determined model (all tasks and their
handles are specified at compile time). The key design question: is handle
distribution fully dynamic (init decides at runtime) or build-time-determined
(the set of services and their handles is fixed at compile time, like LionsOS
and Hubris)? For ~10 known services, build-time determination is feasible and
enables static verification of the capability graph — a structural advantage
CHERIoT exploits for CI/CD security auditing.

### 7. Error model

Each syscall needs a clear error reporting strategy. The current table lists
operations and return values but not failure modes. Key decisions:

- **Return convention:** A status code (like seL4's `seL4_Error` enum) or a
  result type (success value OR error code). Since the kernel communicates via
  registers, a single "status" register plus "value" register is natural.
- **Defined failure modes per syscall.** `vmo_map` can fail (out of virtual
  address space, invalid handle, insufficient rights). `channel_send` can fail
  (buffer full, peer closed, invalid handle). `event_wait` can time out. Each
  failure is an explicit, enumerated case — no catch-all "error" that hides the
  cause.
- **No partial failure.** Every syscall either fully succeeds or fully fails
  with no side effects. `channel_send` with handles either transfers the message
  and all handles atomically, or does nothing.

This is the "validate at system boundaries" principle applied to the kernel
boundary itself. The syscall interface IS a system boundary — the most critical
one.
