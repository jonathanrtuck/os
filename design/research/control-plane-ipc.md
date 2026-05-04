# Control-Plane IPC: Design Space Research

**Researched:** 2026-05-03 **Domain:** Microkernel IPC mechanisms for small,
bidirectional control messages **Confidence:** HIGH (core claims verified
against official documentation and published papers)

---

## Summary

The current kernel spec uses Zircon-style async buffered channels for all
control-plane IPC. The Zircon bias audit (2026-04-04) flagged this as the
strongest concern: synchronous IPC may be more natural for request/reply
patterns in a document-centric OS with ~10 services. This research evaluates
five IPC models against the system's actual communication patterns.

**The evidence points clearly:** synchronous call/reply with asynchronous
notifications (Option 5, the hybrid) is the best fit for this system. It
eliminates kernel ring buffer allocation, gives the fastest possible
request/reply path, naturally inherits priority, and composes cleanly with the
existing Event objects for async signaling. The current async channel design is
not wrong in the sense of being broken, but it adds unnecessary complexity
(kernel-managed ring buffers, backpressure logic) for communication patterns
that are overwhelmingly synchronous request/reply. The concern is substantive,
not cosmetic.

**Primary recommendation:** Replace async buffered channels with synchronous
call/reply IPC using one-shot reply capabilities. Use existing Event objects for
async notifications. Reserve buffered channels only if a genuine async-buffering
use case emerges (none identified yet).

---

## The Communication Patterns This System Actually Has

Before evaluating mechanisms, inventory the actual control-plane traffic.

### Request/reply (synchronous by nature)

| Sender      | Receiver   | Message                   | Pattern                                |
| ----------- | ---------- | ------------------------- | -------------------------------------- |
| Editor      | OS service | "insert 'a' at offset 47" | Request, wait for ack                  |
| OS service  | Editor     | "key event: 'a'"          | Deliver, expect eventual write request |
| Shell       | OS service | "open document X"         | Request, wait for handle               |
| OS service  | Doc store  | "map document X"          | Request, wait for VMO handle           |
| Any service | Kernel     | syscall                   | Already synchronous                    |

Every one of these is naturally synchronous: the sender issues a request and
blocks until the reply arrives. Wrapping them in async channels adds buffering
that never gets used -- the sender immediately waits for the reply anyway.

### Async notifications (already handled by Events)

| Source     | Signal              | Pattern                    |
| ---------- | ------------------- | -------------------------- |
| OS service | "new frame ready"   | Bit signal on Event object |
| Kernel     | "interrupt arrived" | Bit signal on Event object |
| Channel    | "message available" | Implicit event on channel  |
| Thread     | "exited"            | Bit signal on Event object |

These are already modeled by the Event object in the kernel spec. They carry no
data -- just "something happened." Level-triggered, coalescable.

### Genuine async buffering

After exhaustive review of the ~10 service topology: **no control-plane traffic
requires kernel-managed async buffering.** The data plane (scene graph, document
content, decoded pixels) uses shared-memory VMOs with zero kernel involvement.
The control plane is request/reply. Async notifications are Events.

The 16-slot ring buffer in the current channel design is solving a problem this
system does not have.

---

## Evaluation: Five IPC Models

### Evaluation Criteria (in priority order)

1. **Fit for actual patterns** -- request/reply dominant, ~10 services
2. **Kernel complexity** -- fewer primitives = smaller TCB
3. **Latency** -- cycles per IPC round-trip
4. **Handle/capability transfer** -- must be supported
5. **Priority inversion handling** -- high-priority blocked on low-priority

### Option 1: Synchronous call/reply (seL4/L4 style)

**How it works.** Sender invokes `seL4_Call(endpoint, msg)`. The kernel
transfers the message in registers (no buffer). The sender blocks. The kernel
switches directly to the receiver (if it's waiting). The receiver processes the
request and replies via a one-shot reply capability. The sender unblocks.
[CITED: docs.sel4.systems/Tutorials/ipc.html]

The message is never buffered -- it exists only in registers during the
transfer. The kernel's IPC fastpath is ~100 instructions. [CITED:
flint.cs.yale.edu/cs428/doc/L3toseL4.pdf]

**Systems that use it:** seL4, OKL4, L4Ka::Pistachio, L4/Fiasco, EROS/Coyotos,
original L4 (Liedtke). QNX uses the same pattern with different names
(MsgSend/MsgReceive/MsgReply). [VERIFIED: multiple official docs]

**Measured performance:**

- seL4 on ARM64 (Cortex-A57, Jetson TX1): IPC call = 416 cycles, IPC reply = 424
  cycles. Round-trip = ~840 cycles. Standard deviation = 0. [CITED:
  sel4.systems/performance.html]
- seL4 on ARM64 (MCS kernel): IPC call = 429, reply = 443. Round-trip = ~872
  cycles. [CITED: sel4.systems/performance.html]
- L4Ka::Pistachio on various platforms: ~100 cycles one-way for the original L4
  fastpath. [CITED: L3-to-seL4 paper, SOSP 2013]
- LionsOS (seL4-based, production workloads): 986 cycles intra-core round-trip.
  [CITED: arxiv.org/html/2501.06234v1]

**Handle/capability transfer:** seL4 transfers capabilities as part of the IPC
message. Each message can carry capability pointers (CPtrs) in the IPC buffer,
and the kernel resolves and transfers them atomically. The Grant right on an
endpoint capability controls whether capability transfer is allowed. [CITED:
docs.sel4.systems/Tutorials/ipc.html]

**Priority handling:** seL4 MCS introduces scheduling context donation. A
passive server runs on the calling client's scheduling context (and therefore
its priority and time budget). Budget is donated on `seL4_Call` and returned on
`seL4_ReplyRecv`. This provides natural priority inheritance without any
explicit mechanism -- the server always runs at the caller's priority. [CITED:
docs.sel4.systems/Tutorials/mcs.html]

QNX's approach is simpler: the server thread inherits the priority of the
sending thread on `MsgReceive`. No scheduling context donation, just direct
priority adjustment. This can be disabled per-channel with
`_NTO_CHF_FIXED_PRIORITY`. [CITED: qnx.com/developers/docs/8.0]

**Risks:**

- **Server crash = client deadlock.** If the server crashes while the client is
  blocked on `Call`, the client blocks forever unless the kernel detects the
  crash and unblocks it. seL4 handles this: the reply capability is consumed
  (one-shot), and if the server's TCB is destroyed, the kernel signals the
  client's fault handler. QNX handles it similarly: the kernel unblocks
  REPLY-blocked clients with an error when the server dies. [CITED:
  docs.sel4.systems/Tutorials/ipc.html, qnx.com IPC docs]
- **Deadlock risk.** If A calls B and B calls A, both block forever. Prevention:
  enforce a strict call hierarchy (calls go up, replies go down). QNX documents
  this explicitly: "never have two threads send to each other." [CITED:
  qnx.com/developers/docs/6.5.0SP1.update]

For this OS: the service topology is a tree. Editors call the OS service. The OS
service calls the document store. Nothing calls down. The strict hierarchy is a
natural property of the architecture, not an additional constraint.

### Option 2: Synchronous IPC + notifications (seL4 model)

**How it works.** Two distinct kernel objects:

1. **Endpoints** for synchronous request/reply (as in Option 1)
2. **Notification objects** for async signals -- a word of semaphore-like bits,
   signaled with `seL4_Signal`, waited on with `seL4_Wait` or `seL4_Poll`
   [CITED: docs.sel4.systems/Tutorials/notifications]

Notifications can be **bound** to a TCB. When a thread is waiting on an endpoint
for IPC, signals to its bound notification object are delivered as well. The
receiver distinguishes IPC from notification by checking the badge. [CITED:
docs.sel4.systems/Tutorials/notifications]

**Why two objects:** Synchronous IPC is optimal for request/reply. Notifications
are optimal for async events. Combining them in one mechanism (like Zircon
channels) forces both to be suboptimal.

**For this OS:** The kernel spec already has Event objects that are functionally
identical to seL4 notifications -- level-triggered bit signals, multi-wait, used
for interrupt delivery and thread exit. Option 2 is the natural composition:
synchronous IPC (replacing channels) + Events (already designed).

### Option 3: Async buffered channels (current design, Zircon-style)

**How it works.** `channel_create()` produces two endpoints. Each has a
kernel-managed ring buffer. `channel_send()` copies the message into the buffer.
`channel_recv()` copies it out. Bidirectional, fully asynchronous. Sender never
blocks unless the buffer is full.

The current kernel spec adds a twist: the ring buffer is a shared-memory VMO
mapped into both endpoints, so message bytes are not kernel-copied. The kernel
only mediates handle transfer and thread waking. [CITED:
kernel-userspace-interface.md, Channel section]

**Systems that use it:** Zircon/Fuchsia (kernel-managed buffers, up to 64KB per
message, up to 64 handles per message). [VERIFIED:
fuchsia.dev/fuchsia-src/concepts/kernel/ipc_limits]

Zircon channels carry messages up to 64KB each (though the precise limit is not
formally disclosed to user applications). [CITED: fuchsia.dev IPC limits page]

**Measured performance:** Zircon IPC is significantly slower than seL4. Research
on hardware-accelerated IPC reports Zircon channel IPC at ~664 cycles for a
software-only call (without XPC hardware). Application launch requires 8 IPC
roundtrips. [CITED: TOCS 2022 XPC paper]

**Handle transfer:** Atomic with the message. All handles in a message are moved
to the channel on write, and moved to the receiver on read. If the write fails,
all handles are consumed (discarded). [VERIFIED: fuchsia.dev Channel
documentation]

**Priority handling:** None built into the IPC mechanism. Zircon's scheduler
does not do priority inheritance through channels. Priority inversion must be
handled at a higher level or not at all.

**Kernel complexity cost:**

- Ring buffer allocation and management (per channel, per direction)
- Backpressure logic (what happens when the buffer fills)
- Buffer accounting (Zircon has had production issues with kernel buffer
  exhaustion: "system can run out of kernel buffers to service even critical
  tasks") [CITED: fuchsia.dev IPC limits page]
- The current spec's shared-memory ring variant reduces kernel copying but still
  requires kernel-managed ring buffer metadata (head/tail indices, slot count)

### Option 4: Thread migration (Composite OS, LRPC)

**How it works.** The calling thread does not send a message to a separate
server thread. Instead, the calling thread _migrates into the server's address
space_ and executes the server code directly, keeping its own priority and
scheduling context. On return, it migrates back. There is no context switch in
the traditional sense -- just a domain crossing (change page table root, verify
capability, continue execution). [CITED: Composite component invocations,
Bershad LRPC paper]

LRPC (Bershad et al., 1990) achieved a 3x improvement over traditional RPC: 157
ps vs 464 ps for the simplest cross-domain call on the DEC Firefly. [CITED: LRPC
paper, ACM TOCS 1990]

Composite OS decouples the execution context from the scheduling context. During
IPC, the thread abstraction moves between components. Server code is "passively
executed" -- there is no server thread. The scheduler only manages one entity
per logical flow. [CITED: Composite component invocations, Parmer]

**Performance:** The fastest possible IPC -- there is no message copy, no
buffer, no context switch, no scheduler involvement. The cost is a domain
crossing: save a few registers, switch page table root, verify capability. On
modern ARM64, this would be dominated by the TLB refill cost after the TTBR
switch (~81 ns on M4 Pro for a full page walk, but ASIDs prevent full TLB
flushes).

**Handle/capability transfer:** In Composite, capabilities govern component
invocations. The calling thread can pass register arguments (4 registers on
x86-32). Passing complex capabilities would require a more elaborate mechanism
-- the pure thread-migration model is best for simple call/return with small
arguments.

**Priority handling:** Perfect natural priority inheritance. The calling thread
keeps its own priority while executing server code. No mechanism needed -- it's
a structural property.

**Risks:**

- **Kernel complexity.** Thread migration requires the kernel to manage threads
  that can exist in multiple address spaces over their lifetime. Stack
  management is complex: either one stack per server per thread (memory cost =
  threads x servers) or a contention-mediated shared stack (~0.2 us overhead).
  [CITED: Composite component invocations]
- **Blocking in the server.** If the "server" code blocks (e.g., waiting on disk
  I/O), the calling thread is stuck in the server's domain. This complicates the
  execution model. Composite handles this with split execution contexts, but it
  adds significant kernel complexity.
- **Reentrancy.** If thread A migrates into server S, and S needs to call
  another server T, thread A migrates again. Deep call chains create deep stack
  chains -- debuggability suffers.

### Option 5: Hybrid (synchronous request/reply + Event notifications)

**How it works.** Compose two mechanisms:

1. **Synchronous call/reply** for the dominant pattern (request/reply between
   services). Sender blocks, kernel transfers message in registers, receiver
   processes, replies via one-shot reply capability. Essentially seL4's
   `Call`/`ReplyRecv` pattern.

2. **Event objects** (already in the kernel spec) for async notifications. A
   server waiting for IPC on an endpoint can simultaneously receive event
   signals (same as seL4's bound notification mechanism).

3. **No buffered channels.** If a genuine async buffering use case emerges, it
   can be built in userspace on top of shared-memory VMOs + Events -- exactly as
   the data plane already works.

**This is not a novel design.** It is exactly what seL4 does (endpoints +
notifications), what QNX does (MsgSend + pulses), and what LionsOS does in
practice (synchronous IPC for setup/control, shared memory + notifications for
data). The novelty is recognizing that the current async channel design is the
outlier, not the standard.

---

## Comparison Table

| Criterion                | 1. Sync call/reply                                                               | 2. Sync + notifications                                                          | 3. Async channels (current)                                                                                                      | 4. Thread migration                                                                  | 5. Hybrid (recommended)                                             |
| ------------------------ | -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------- |
| **Fit for patterns**     | Excellent -- request/reply is the only pattern                                   | Excellent -- covers both request/reply and async events                          | Adequate -- works but adds unnecessary buffering                                                                                 | Excellent for call/return; awkward for notifications                                 | Excellent -- each mechanism matches its pattern                     |
| **Kernel complexity**    | Small -- no buffers, no backpressure, no ring management                         | Small -- adds only notification object (equivalent to existing Event)            | Medium -- ring buffers per channel per direction, backpressure, buffer accounting, exhaustion handling                           | Large -- multi-domain thread tracking, stack management, split execution contexts    | Small -- sync IPC + Events (both already designed or near-designed) |
| **Latency (round-trip)** | ~840 cycles on ARM64 (seL4, A57)                                                 | Same as #1 for request/reply; notifications are lighter (~100 cycles for signal) | Higher -- kernel ring buffer management adds overhead; ~664 cycles per call on Zircon but without direct scheduling optimization | Lowest possible -- no message copy, no context switch; dominated by TTBR switch cost | Same as #1 for request/reply                                        |
| **Handle transfer**      | Kernel transfers capabilities atomically as part of IPC message                  | Same as #1                                                                       | Atomic with message -- handles moved on write, delivered on read                                                                 | Limited -- registers only in pure form; needs augmentation for capability passing    | Same as #1                                                          |
| **Priority inversion**   | Natural via scheduling context donation (seL4 MCS) or priority inheritance (QNX) | Same as #1                                                                       | None built-in -- requires application-level mitigation                                                                           | Perfect -- calling thread keeps its priority                                         | Same as #1                                                          |

### Verdict by criterion

1. **Fit for patterns:** Options 1, 2, 5 win. Option 3 adds machinery for a
   pattern (async buffering) this system does not use. Option 4 wins for
   call/return but needs a second mechanism for notifications anyway.

2. **Kernel complexity:** Options 1 and 5 win. Option 3 adds ring buffer
   management per channel. Option 4 adds the most kernel complexity (thread
   migration across domains). Option 5 composes two simple mechanisms.

3. **Latency:** Option 4 theoretically wins but at enormous kernel complexity
   cost. Options 1, 2, 5 are competitive at ~840 cycles round-trip on ARM64.
   Option 3 is slower due to buffer management overhead. On the M4 Pro (wider
   out-of-order, deeper caches than A57), expect even lower cycle counts for the
   sync fastpath.

4. **Handle transfer:** Options 1, 2, 3, 5 all support it naturally. Option 4 is
   weakest -- register-only transfer doesn't scale to complex capability
   distribution.

5. **Priority inversion:** Options 1, 2, 4, 5 handle it naturally. Option 3 does
   not -- Zircon has no priority inheritance through channels.

---

## Experience Reports From Production Systems

### seL4 + LionsOS (UNSW, production since 2025)

LionsOS uses seL4's synchronous IPC for setup and control, shared-memory SPSC
queues with notifications for data transfer. This is exactly the hybrid
(Option 5) applied at OS scale. Performance: full 10 Gb/s network bandwidth on
two cores, outperforming Linux. The architecture validates that synchronous IPC
for control + shared memory for data is not just theoretically clean but
practically fast. [CITED: arxiv.org/html/2501.06234v1]

### QNX Neutrino (production since 2001, safety-critical)

QNX has used synchronous MsgSend/MsgReceive/MsgReply as its sole message-passing
IPC for 25+ years in automotive, medical, and nuclear applications. The
three-call triad with automatic priority inheritance is QNX's defining feature.
Pulses (lightweight async notifications) complement the synchronous path.
[CITED: qnx.com/developers/docs/8.0]

QNX's experience confirms that synchronous IPC works at scale in long-lived
production systems -- including systems with far more services than this OS's
~10.

### Zircon/Fuchsia (production since 2019)

Zircon chose async channels for pragmatic reasons: Fuchsia must support
thousands of components, many with complex async event handling (UI frameworks,
network stacks, device drivers with interrupt coalescing). Zircon channels are a
general-purpose mechanism for a general-purpose OS. The buffer exhaustion
problem is documented and real -- Zircon's IPC limits page acknowledges that
"the system can run out of kernel buffers to service even critical tasks."
[CITED: fuchsia.dev IPC limits]

For a personal OS with ~10 services, Zircon's generality is unnecessary. Its
costs (kernel buffer management, no priority inheritance, higher latency) are
paid for flexibility this system does not need.

### Composite OS (research, GWU)

Composite demonstrates that thread migration is viable and offers the best
possible latency. But Composite's kernel is significantly more complex than an
L4-style kernel, and the complexity is in the thread management and stack
allocation paths -- exactly where bugs are hardest to find and verify. For a
kernel targeting verification tractability (the spec mentions Verus and
Atmosphere's 7.5:1 proof-to-code ratio), thread migration adds the most
verification burden. [CITED: Composite invocation docs, Atmosphere SOSP 2025]

---

## The Honest Assessment

**Is the current async channel design genuinely worse for this OS?**

Yes, but not catastrophically. It works. It's not going to cause the system to
fail. But it is the wrong abstraction for the communication patterns this system
actually has:

1. **Every control-plane interaction is request/reply.** Async buffering is
   overhead with no benefit. The sender always waits for the reply immediately.

2. **The kernel manages ring buffers it doesn't need.** Each channel has a 16-
   slot ring buffer per direction. With ~10 services, that's ~20 ring buffers
   the kernel allocates and manages for communication that could be a direct
   register transfer.

3. **No priority inheritance.** The async model decouples sender and receiver
   scheduling. When a high-priority editor sends a write request to the OS
   service, the OS service doesn't inherit the editor's priority. It processes
   the request at its own priority, whenever it gets to it. With synchronous IPC
   and priority inheritance/donation, the OS service processes the high-
   priority request at the editor's priority.

4. **More kernel code = more attack surface.** Ring buffer management,
   backpressure logic, and buffer accounting are kernel code paths that don't
   need to exist. Each removed code path is a removed bug surface.

5. **Zircon bias is real.** The kernel spec's channel description reads almost
   identically to Zircon's. The shared-memory ring optimization is clever but
   it's optimizing the wrong mechanism. The spec even acknowledges this tension:
   "No synchronous send/receive coupling... Channels here are asynchronous with
   a small kernel-managed buffer."

**How much work is the change?** The syscall interface barely changes. Replace:

```text
channel_create()           -> (handle, handle)
channel_send(ep, msg, h[])
channel_recv(ep, timeout)  -> (msg, h[])
```

With:

```text
endpoint_create()          -> handle
call(ep, msg, h[])         -> (msg, h[])     // send + block + receive reply
reply(msg, h[])                               // reply to current caller
recv(ep, timeout)          -> (msg, h[], reply_cap)  // server waits for client
```

Same number of syscalls (3 -> 3, plus create). The semantics change from "write
to a buffer, read from a buffer" to "call and block, receive and reply." The
kernel implementation gets simpler, not more complex.

Handle transfer works identically: capabilities are passed as part of the IPC
message, transferred atomically by the kernel.

---

## The Recommendation

**Use synchronous call/reply IPC (Option 5: the hybrid).**

### Kernel objects after the change

| Object                     | Role                | Changed?                    |
| -------------------------- | ------------------- | --------------------------- |
| VMO                        | Shared memory       | No                          |
| **Endpoint** (was Channel) | Synchronous IPC     | **Yes -- replaces Channel** |
| Event                      | Async notifications | No                          |
| Thread                     | Execution           | No                          |
| Address Space              | Isolation           | No                          |

Still 5 kernel objects. Still 25 syscalls (adjust channel*\* to endpoint*\*).
The kernel gets simpler, not larger.

### Syscall mapping

| Old (async channel)          | New (sync endpoint)                          | Notes                                          |
| ---------------------------- | -------------------------------------------- | ---------------------------------------------- |
| `channel_create()` -> (h, h) | `endpoint_create()` -> h                     | Single endpoint, many-to-one or point-to-point |
| `channel_send(ep, msg, h[])` | `call(ep, msg, h[])` -> (reply_msg, h[])     | Blocks until reply                             |
| `channel_recv(ep, timeout)`  | `recv(ep, timeout)` -> (msg, h[], reply_cap) | Server waits; gets one-shot reply cap          |
| (implicit in recv)           | `reply(reply_cap, msg, h[])`                 | One-shot, consumed on use                      |

### Naming model change

The current spec uses bidirectional point-to-point channels (Model A: capability
IS the destination). Synchronous IPC uses endpoints that can be either:

- **Point-to-point** (like the current channels): one sender, one receiver.
  Created by handing out a single endpoint capability to each party.
- **Many-to-one** (like seL4 endpoints): multiple clients share an endpoint
  capability. The server calls `recv` and gets whichever client called first.
  Badge values distinguish callers.

For this OS's topology (editors call OS service, OS service calls doc store),
many-to-one endpoints are natural: the OS service has one endpoint that all
editors call. No need to manage N separate channel pairs.

### Priority inheritance

Two viable approaches:

1. **QNX-style priority inheritance.** On `recv`, the server thread's effective
   priority is boosted to match the highest-priority blocked caller. Simple,
   proven, 25 years of production use.

2. **seL4 MCS-style scheduling context donation.** The server runs as a passive
   entity with no scheduling context. On `call`, the client's scheduling context
   (including priority and time budget) is donated to the server. On `reply`,
   it's returned.

For this OS: QNX-style is simpler and sufficient. The OS has ~10 services with
clear priority relationships and no mixed-criticality requirements. Scheduling
context donation solves a harder problem (temporal isolation of shared servers
in mixed-criticality systems) that doesn't apply here.

### What happens to async use cases

The kernel spec already has Event objects for all async needs:

| Async need                      | Mechanism                                    |
| ------------------------------- | -------------------------------------------- |
| "New frame ready"               | `event_signal(scene_ready, BIT_0)`           |
| "Interrupt arrived"             | `event_signal(irq_event, BIT_N)`             |
| "Endpoint has a waiting caller" | Bind event to endpoint (seL4 pattern)        |
| "Peer disconnected"             | `event_signal(lifecycle_event, PEER_CLOSED)` |

No async channels needed. If a future use case genuinely needs buffered async
messaging, build it in userspace: allocate a VMO, implement a ring buffer in
shared memory, use Events for signaling. This is exactly what the data plane
already does -- and what LionsOS does in production.

---

## Open Questions

1. **Endpoint naming model: point-to-point vs many-to-one?**
   - What we know: many-to-one is simpler for the "multiple editors calling OS
     service" pattern. Badging distinguishes callers.
   - What's unclear: does this OS need both models, or is many-to-one
     sufficient?
   - Recommendation: start with many-to-one (one endpoint per server), add
     point-to-point only if needed. Many-to-one is what seL4 uses and it covers
     the common case.

2. **Reply capability lifetime and crash handling.**
   - What we know: one-shot reply capabilities prevent reply misuse. seL4's
     `Reply` object (MCS) or internal TCB storage (non-MCS) both work.
   - What's unclear: what exactly happens when a server crashes with outstanding
     reply capabilities?
   - Recommendation: when a server's address space is destroyed, the kernel
     signals all clients blocked on that endpoint's reply capabilities with a
     PEER_CLOSED event. The client unblocks with an error. This is what both
     seL4 and QNX do.

3. **Message size for register-based transfer.**
   - What we know: the kernel spec currently uses 128-byte messages (one M4 Pro
     cache line). seL4 uses ~64 bytes of register-based transfer in the
     fastpath.
   - What's unclear: should the sync IPC fastpath transfer the full 128-byte
     cache line in registers, or a smaller register set with overflow to a
     shared IPC buffer page?
   - Recommendation: on ARM64, use the ~30 general-purpose registers to transfer
     up to ~240 bytes in registers on the fastpath (same as seL4's virtual
     message registers). Fall back to an IPC buffer page (mapped into both
     address spaces) for larger messages or capability transfer metadata. The
     128-byte cache-line-aligned message slot concept from the current spec can
     become the IPC buffer format.

4. **Interaction with the data plane.**
   - What we know: the data plane (shared-memory VMOs with generation counters)
     is settled and does not change.
   - What's unclear: does the initial setup of data-plane shared memory require
     any changes to work with sync IPC instead of channels?
   - Recommendation: no change needed. Setup is: OS service creates a VMO, calls
     the compositor's endpoint with a message containing the VMO handle. The
     kernel transfers the handle atomically as part of the sync IPC. Same
     capability-passing semantics, different transport.

---

## Sources

### Primary (HIGH confidence)

- [seL4 IPC tutorial](https://docs.sel4.systems/Tutorials/ipc.html) -- sync IPC
  mechanism, capability transfer, reply capabilities
- [seL4 Notifications tutorial](https://docs.sel4.systems/Tutorials/notifications)
  -- notification objects, binding to TCB
- [seL4 MCS tutorial](https://docs.sel4.systems/Tutorials/mcs.html) --
  scheduling context donation, passive servers, reply objects
- [seL4 Performance](https://sel4.systems/performance.html) -- ARM64 cycle
  counts (416/424 call/reply on A57)
- [QNX Synchronous messaging](https://www.qnx.com/developers/docs/8.0/com.qnx.doc.neutrino.sys_arch/topic/ipc.html)
  -- MsgSend/MsgReceive/MsgReply, priority inheritance
- [LionsOS paper](https://arxiv.org/html/2501.06234v1) -- shared memory +
  notifications architecture, SPSC queues, 10 Gb/s validation
- [Fuchsia IPC limits](https://fuchsia.dev/fuchsia-src/concepts/kernel/ipc_limits)
  -- Zircon channel buffer management, exhaustion risks
- [kernel-userspace-interface.md](../kernel-userspace-interface.md) -- current
  channel design

### Secondary (MEDIUM confidence)

- [From L3 to seL4](https://flint.cs.yale.edu/cs428/doc/L3toseL4.pdf) -- 20
  years of L4 IPC evolution, fastpath design
- [LRPC paper](https://people.eecs.berkeley.edu/~prabal/resources/osprelim/BAL+90.pdf)
  -- thread migration / LRPC performance (3x improvement)
- [Composite component invocations](https://www2.seas.gwu.edu/~gparmer/posts/2016-01-17-composite-component-invocation.html)
  -- thread migration IPC design and tradeoffs
- [Parmer: Communication in Systems](https://www2.seas.gwu.edu/~parmer/posts/2016-01-05-communication-in-systems.html)
  -- sync vs async IPC convergence analysis
- [XPC paper](https://ipads.se.sjtu.edu.cn/_media/publications/2022_-_a_-_tocs_-_xpc.pdf)
  -- hardware-accelerated IPC, Zircon channel overhead (~664 cycles)
- [Atmosphere (SOSP 2025)](https://dl.acm.org/doi/10.1145/3731569.3764821) --
  verified Rust L4-style kernel, verification tractability

### Tertiary (LOW confidence)

- [Shapiro: Vulnerabilities in synchronous IPC](https://www.researchgate.net/publication/4015956_Vulnerabilities_in_synchronous_IPC_designs)
  -- asymmetric trust and dynamic payload concerns
- [Singularity/Midori typed channels](https://courses.cs.washington.edu/courses/cse551/15sp/papers/singularity-osr07.pdf)
  -- linear types for zero-copy IPC (language-level approach)
