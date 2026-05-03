# Ideal Kernel for This OS on M4 Pro

What an ideal kernel for this document-centric OS would look like on this
specific machine (M4 Pro, Mac16,7, 48 GB LPDDR5X). How the hardware's
capabilities, idiosyncrasies, and constraints map to the OS's design goals.

Hardware reference: `~/Sites/kernel/design/research/m4-pro-hardware.md`

---

## The Core Insight

The M4 Pro's hardware characteristics and this OS's design goals are unusually
well-aligned. Apple Silicon was designed for a personal, document-centric
workstation (Apple's own ecosystem). This OS takes that premise to its logical
conclusion. An ideal kernel leans into this alignment instead of fighting it.

---

## 1. Scheduling: Cluster-Aware, Content-Pipeline-Shaped

### Hardware facts that matter

- 3 clusters: P-cluster0 (5 cores, 16 MB shared L2), P-cluster1 (5 cores, 16 MB
  shared L2), E-cluster (4 cores, 4 MB shared L2)
- Cross-cluster communication goes through SLC (~49 ns) or DRAM (~97 ns) -- an
  order of magnitude worse than intra-cluster L2 (~4 ns)
- No SMT -- 1 thread per core, no hyperthreading illusions
- FEAT_WFxT: WFE/WFI with hardware timeout (sleep until event OR timeout,
  whichever comes first)
- 24 MHz timer (41.7 ns resolution) -- coarse but fine for scheduling quanta

### The opportunity

This OS has a known, small, fixed topology of services with well-understood
latency requirements. This is radically different from a general-purpose OS
scheduling thousands of unknown processes. The scheduler can be _shaped to the
pipeline_.

The document pipeline has a natural latency hierarchy:

1. **Latency-critical** (< 1 ms response): input routing, cursor updates, scene
   graph swap -- the "feels instant" path
2. **Frame-critical** (< 16 ms budget): layout, scene graph build, compositor,
   GPU submit -- the render loop
3. **Throughput** (seconds OK): content decoding, translation, metadata
   indexing, snapshot pruning

### Cluster-to-pipeline mapping

| Cluster     | Cores | L2           | Pipeline role                                  | Why                                                                                                                        |
| ----------- | ----- | ------------ | ---------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| P-cluster 0 | 5     | 16 MB shared | OS service + compositor + input                | These share the scene graph VMO; keeping them on the same L2 means the double-buffer swap hits L2 (~4 ns) not SLC (~49 ns) |
| P-cluster 1 | 5     | 16 MB shared | Editors, translators, decoders                 | Throughput work that doesn't share hot data with the render path                                                           |
| E-cluster   | 4     | 4 MB shared  | Background: indexing, snapshot GC, COW pruning | Low-priority, power-efficient, no latency requirement                                                                      |

### Design consequences

**Work stealing should respect cluster boundaries.** Stealing within a cluster
costs an L2 hit. Cross-cluster stealing costs SLC/DRAM. The scheduler should
only steal cross-cluster when a cluster has been idle for multiple scheduling
rounds -- not eagerly.

**WFxT for event-driven idle.** This directly serves the philosophy's "react to
reality, don't poll" principle. An idle core executes `WFE` with a timeout. It
consumes zero cycles until either an event arrives (new work) or the timeout
fires (periodic housekeeping). No polling loops, no wasted power. Barrelfish's
scheduler uses a similar approach on heterogeneous hardware.

**Few-process optimization.** With ~10 services total, the kernel never needs to
scan large run queues. Per-core run queues with at most 2-3 entries each.
EEVDF's O(log n) is overkill when n <= 3; a simple sorted list per core would
suffice. The scheduler's hot path should fit entirely in L1 (~128 KB data cache
on P-cores).

**128-byte cache line constraint.** A `ThreadState` struct that fits in 128
bytes loads in a single cache miss. At 14 cores x 3 runnable threads, the entire
scheduler state is ~5 KB -- it lives permanently in L1. Compare this to Linux's
CFS which has per-CPU red-black trees that frequently spill to L2.

The no-SMT property is also a simplification: there's no need for the load
balancer to reason about logical vs. physical cores, or about contention for
shared execution resources between hyperthreads. Each core is independent.
Scheduler decisions are pure.

---

## 2. Memory: Unified Architecture as a First-Class Design Principle

### Hardware facts that matter

- Unified memory: CPU, GPU, and Neural Engine share the same physical LPDDR5X
- SLC is inclusive w.r.t. GPU -- GPU reads that miss the GPU cache check the
  SLC, which may hold data the CPU recently wrote
- 16 KB pages (hardware-enforced, no 4 KB option in host translation)
- ~160 DTLB L1 entries -> 2.5 MB TLB coverage
- 81 ns full page walk penalty (nearly as expensive as DRAM)
- 48 GB total RAM -- enormous for a personal OS
- 128-byte cache lines -- double the industry standard

### The opportunity

The entire document pipeline can be truly zero-copy. Not "zero-copy with DMA" --
zero-copy meaning the same physical pages, visible to CPU and GPU
simultaneously, with the SLC acting as a natural coherence bridge.

### VMO design for unified memory

The VMO model (create -> map -> share via capability) is the perfect abstraction
for unified memory. The ideal kernel makes the following guarantees:

1. **Scene graph VMO**: OS service writes, compositor reads, GPU reads -- all
   the same physical pages. The OS service does `DC CVAP` (clean to Point of
   Persistence) on dirty cache lines after scene graph build. The compositor
   reads from SLC or L2 (same cluster). The GPU reads from SLC (inclusive).
   Total copies: zero. Total DMA transfers: zero.

2. **Document content VMOs**: The document store maps file content into a VMO.
   The editor gets a read-only handle to the same VMO. When the OS service needs
   to render content, it reads from the same pages. COW snapshots create new
   page table entries pointing at the same physical pages until divergence --
   the 16 KB page size means even the page table overhead is modest.

3. **Content region for decoders**: A PNG decoder gets a VMO with the compressed
   bytes (read-only) and a VMO for the decoded pixels (write). The decoded
   pixels VMO is then mapped into the compositor's address space for rendering.
   Again, same physical pages -- the GPU can texture-sample directly from the
   decoded output.

### 16 KB pages are a gift for documents

Most documents are 1 KB-1 MB. A 16 KB page means:

- Documents < 16 KB: single page, single TLB entry, full coverage
- A typical rich text document (50 KB): 4 pages, 4 TLB entries
- The entire catalog (~128 KB at 1K files): 8 pages

With ~160 DTLB entries covering 2.5 MB, the kernel's own hot data structures
plus the active document's content can fit comfortably in the TLB. Compare to 4
KB pages where the same data would need 4x more TLB entries.

### TLB is the bottleneck, not bandwidth

At 273 GB/s peak bandwidth and ~10 services, bandwidth is never the constraint.
But a TLB miss costs 81 ns -- nearly a DRAM access. The ideal kernel minimizes
TLB pressure by:

- Contiguous allocation for large VMOs (4 contiguous 16 KB pages -> one TLB
  entry with contiguous hint, covering 64 KB)
- Keeping kernel structures compact -- the handle table, thread table, and page
  tables for all ~10 services should fit within the 2.5 MB DTLB footprint
- Never fragmenting the address space unnecessarily

### SLC as the pipeline's coherence bridge

The SLC's exclusivity w.r.t. CPU caches but inclusivity w.r.t. GPU is the
hardware equivalent of the double-buffered generation swap. When the OS service
writes scene graph data, it lives in L1/L2. When the OS service is done and the
compositor/GPU needs it, the data is evicted from CPU caches to the SLC, where
the GPU looks first. The hardware coherence protocol naturally implements the
pipeline's one-way data flow.

This is different from discrete-GPU systems where you must explicitly copy data
across a PCIe bus. On this machine, "shared memory" means actually, physically
shared. The kernel's job is to not get in the way -- map the same pages, set the
right permissions, done.

---

## 3. Cache-Line-Conscious Data Structures

### Hardware facts that matter

- 128-byte cache line (unique to Apple Silicon -- ARM Cortex and x86 use 64
  bytes)
- L1D: 3 cycles (~0.66 ns), L2: 17-18 cycles (~4 ns), SLC: ~220 cycles (~49 ns)
- L2 is inclusive of L1 (EXAM paper)
- 12-way set associative L2

### Ideal struct layouts

128 bytes is a lot of data. A well-designed kernel struct that packs hot fields
into a single cache line gets everything in one shot. But the flip side: false
sharing has double the blast radius.

**Handle table entry:** pack handle rights (u32), badge (u64), object pointer
(u64), PAC signature (u64) = 28 bytes. Two entries per cache line. Hot path
(lookup + verify + dispatch) touches one cache line.

**Thread state:** register file pointer (u64), page table base (u64), EEVDF
vruntime (u64), priority (u8), state (u8), core affinity (u8), PAC keys (5 x u64
= 40 bytes) = ~75 bytes. Fits in one cache line with room for next/prev
pointers.

**Per-core scheduler state:** MUST be on a separate cache line from every other
core's state. With 128-byte lines, use 128-byte alignment. 14 cores x 128 bytes
= 1,792 bytes total. Lives permanently in each core's L1.

### IPC message alignment

Current design uses 64-byte fixed messages (fitting the 64-byte convention). On
this hardware, a 128-byte message would be more natural -- it loads in a single
cache miss and gives double the payload. Tradeoff: 128-byte messages waste more
space for small messages. Ideal design: 128-byte message slots (one cache line
each), with a compact header indicating actual payload length. A ring buffer of
128-byte-aligned slots means each message is exactly one cache line -- no false
sharing between adjacent messages, no split-line loads.

### Scene graph node sizing

The 128-byte cache line creates a constraint for the double-buffered scene
graph. Each node should either fit within 128 bytes (one cache line) or be an
exact multiple. A Node that spans a cache line boundary pulls in 256 bytes per
access. The current Node is 144 bytes (with a11y fields) -- that's 2 cache lines
on this hardware. Either shrink to 128 (one line) or expand to 256 (two lines,
no waste).

The 128-byte target would mean rethinking the a11y fields' layout -- maybe a11y
data belongs in a parallel array rather than inline with hot rendering data.
Classic hot/cold split: rendering data (position, size, color, opacity) is hot
during the compositor walk. A11y data (role, state, name) is cold during
rendering but hot during a11y traversal. Different access patterns -> different
cache lines.

---

## 4. Security: Hardware CFI + Capabilities = Deep Defense

### Hardware facts that matter

- PAC (Pointer Authentication) with FPAC (faults on failure, not silent
  corruption) and PACIMP (Apple's proprietary, stronger algorithm)
- BTI (Branch Target Identification) -- forward-edge CFI
- CSV2 + CSV3 -- branch predictor context-switched per process, exception entry
  drains speculative loads
- SB (Speculation Barrier) -- drain speculation pipeline on demand
- DIT (Data Independent Timing) -- constant-time execution for crypto
- CTRR v3 -- hardware-enforced code immutability
- **NO MTE** -- no hardware memory tagging
- **NO SSBS** -- no per-context speculative store bypass control
- Load Value Predictor -- security liability unique to Apple Silicon

### PAC for capability integrity

Every handle table entry gets a PAC signature computed over (handle index,
rights mask, object pointer). On syscall, the kernel verifies the PAC before
dispatching. A corrupted handle table -- whether from a kernel bug or an exploit
-- produces an immediate fault (FPAC). This is different from how macOS uses PAC
(mostly for return addresses). The kernel can use PAC as a _capability token_ --
the hardware becomes part of the capability verification path.

Per-process PAC keys loaded on context switch mean that a forged pointer in
process A's handle table can't be used in process B -- the PAC check fails
because the key is different. This is hardware-enforced process isolation at the
pointer level, on top of MMU page-level isolation.

### Speculation mitigations

**CSV2 for speculation isolation.** Set CONTEXTIDR_EL1 to a unique value per
address space on context switch. The branch predictor is hardware-partitioned --
process A's branch history can't influence process B's speculative execution.
This eliminates Spectre v2 (branch target injection) between processes without
software retpolines.

**SB at trust boundaries.** Place speculation barriers at:

- Kernel entry (syscall handler, exception vector)
- Before accessing the handle table after validating a user-provided handle
  index
- Before returning to userspace after touching kernel secrets

This mitigates Spectre v1 (bounds check bypass) and the Load Value Predictor
attacks (SLAP/FLOP) unique to this hardware. The LVP is the most concerning
Apple-specific threat -- it speculatively predicts load values, which can leak
data across trust boundaries. SB is the primary mitigation.

**DIT for kernel crypto.** Enable PSTATE.DIT during:

- ASLR randomization (ChaCha20 PRNG)
- Capability token computation
- Any hash or comparison involving security-sensitive data

This prevents timing side-channels on these operations. The hardware makes DIT a
single-instruction toggle -- zero overhead to enable, and the CPU guarantees
constant-time execution while it's set.

### No MTE means Rust is load-bearing

On Cortex-X4/A720, MTE provides runtime use-after-free and buffer overflow
detection -- a hardware safety net. This machine doesn't have it. The
implication: Rust's compile-time memory safety isn't just a nice-to-have, it's
the _only_ memory safety mechanism. The `unsafe` budget must be strictly
controlled, and every `unsafe` block must earn its keep with a verified
invariant.

ARM server chips (Cortex-X4, Neoverse V2) have MTE, which provides 4-bit tags on
every memory allocation -- the hardware checks the tag on every access, catching
use-after-free and overflow at runtime with negligible performance cost. Apple
chose not to implement it (probably because their own codebase is increasingly
Swift/ObjC with ARC, making MTE less valuable to them).

For this Rust kernel this is manageable -- Rust's ownership model prevents the
bugs MTE would catch. But for userspace services written in other languages (if
that ever happens), or for the `unsafe` corners of Rust code, there's no
hardware safety net. The capability model becomes even more important: even if a
service has a memory corruption bug, capabilities constrain what it can do with
corrupted pointers.

### CTRR for kernel text

The kernel's text segment can be hardware-locked read-only after boot. Once set,
even the kernel itself can't modify its own code. This prevents code injection
even if the kernel is compromised. Under the hypervisor framework, this would
need to be coordinated with the host -- but the mechanism exists.

---

## 5. IPC: Lock-Free, Cache-Line-Aligned, Atomically Visible

### Hardware facts that matter

- LSE atomics: CAS, LDADD, SWP as single instructions (~4-6 cycles uncontended)
- LSE2: single-copy-atomic 128-bit loads/stores on naturally-aligned addresses
- P-core TSO-like memory model (stronger than ARM spec requires)
- E-cores may NOT be TSO -- must use acquire/release semantics for correctness
- 128-byte cache lines

### LSE2 for the scene graph swap

The scene graph double-buffer swap -- where the OS service writes to one buffer
and atomically makes it visible to the compositor -- is a textbook case for LSE2
128-bit atomics.

```
struct SceneSwap {
    generation: u64,    // monotonically increasing
    root_offset: u64,   // offset to root node in the VMO
}
// Naturally aligned at 16 bytes -> LSE2 guarantees atomic visibility
```

The OS service writes this with a single `STP` (store pair) instruction. The
compositor reads it with `LDP` (load pair). LSE2 guarantees the compositor sees
either the old pair or the new pair, never a torn read. No locks, no CAS retry
loops, no memory barriers beyond the natural acquire/release.

### Ring buffer atomics

For ring buffer IPC, LSE atomics provide single-instruction FIFO operations:

- Producer: `LDADD` to atomically increment the write index
- Consumer: `LDADD` to atomically increment the read index
- No LL/SC retry loops, no live-lock under contention

### The TSO trap

P-cores have TSO-like ordering -- loads are not reordered past loads, stores are
not reordered past stores. This means many programs "accidentally" work
correctly without explicit barriers. **The kernel must not rely on this.**
E-cores may use the weaker ARM ordering model. All synchronization must use
`LDAR`/`STLR` (acquire/release) or `DMB` barriers. Testing only on P-cores and
then running on E-cores would create heisenbugs.

### LRCPC for producer-consumer paths

FEAT*LRCPC (Load-Acquire RCpc) and LRCPC2 provide a \_weaker* form of
load-acquire that's sufficient for many producer-consumer patterns (like the
ring buffer) at lower cost than full sequential consistency. "RCpc" stands for
"Release Consistency, processor consistent" -- it guarantees that an acquiring
load sees all stores released by the same thread, but doesn't provide a total
ordering across all threads. For the pipeline where data flows in one direction
(OS service -> compositor), RCpc is sufficient and cheaper than full SC. seL4's
verified kernel uses similar weakened ordering for its fastpath IPC.

---

## 6. Neural Engine and SME: Content Understanding at Hardware Speed

### Hardware facts that matter

- 20 GPU cores (Metal 4)
- SME/SME2 with 512-bit streaming SVL (Scalable Matrix Extension)
- BF16, I8MM, DotProd -- ML inference accelerators
- Neural Engine (not directly accessible at EL1, but accessible through the
  hypervisor framework)
- Hardware AES, SHA-256, SHA-512, SHA-3, CRC32

### Kernel's role (narrow but important)

The kernel doesn't do content understanding -- that's the OS service and
decoders. But the kernel must:

1. **Manage SME state on context switch.** SME state is large -- the ZA array
   alone is up to 4 KB for 512-bit SVL, plus streaming SVE registers. Saving and
   restoring this on every context switch would be ruinous. **Lazy state
   management** is essential: trap on first SME use after a context switch, save
   the previous owner's state, load the new owner's state. Most context switches
   won't touch SME at all (only decoders and ML inference use it), so the common
   path is free.

2. **Expose the Neural Engine as a device.** A driver can provide a
   capability-controlled interface for neural network inference. Content
   classification, OCR, image understanding -- these can run on the Neural
   Engine while the CPU handles the interactive pipeline. The kernel provides
   the memory mapping (VMOs for model weights and inference buffers) and the
   scheduling (Neural Engine requests as asynchronous operations with completion
   notifications).

3. **Hardware crypto for the filesystem.** AES-256 encryption of document
   content at rest, SHA-256 for integrity checking of COW blocks -- all at
   hardware speed via NEON/crypto extensions. The COW filesystem can encrypt
   every block transparently with negligible performance cost. This is
   meaningful for a personal OS -- documents are encrypted on disk without the
   user noticing.

### SME for userspace decoders

SME2's 512-bit matrix operations are designed for exactly the kind of data
processing the decoders do:

- Image decoding (JPEG DCT, PNG filtering)
- Font rasterization (coverage calculation)
- Audio processing (FFT for spectrum analysis)
- Text analysis (string matching for metadata extraction)

The kernel's job is to make SME available efficiently (lazy state management)
and to ensure the 512-bit registers are saved/restored correctly. The actual SME
code lives in userspace decoders behind clean interfaces.

### Three compute domains

The M4 Pro has three compute domains that map well to the pipeline:

- **CPU** (14 cores): Interactive path -- input routing, layout, scene graph
  build, edit protocol. Latency-sensitive, low throughput.
- **GPU** (20 cores): Rendering path -- compositor, rasterization, compositing.
  High throughput, parallel.
- **Neural Engine**: Content understanding -- classification, transcription,
  OCR. Asynchronous, batch-oriented.

Each domain has its own scheduling and its own workload character, but they
share the same physical memory. A document can flow through all three without a
single copy: the CPU lays it out, the GPU renders it, the Neural Engine
classifies it for metadata indexing. This is uniquely possible on unified memory
architecture -- discrete GPU + separate NPU systems would need multiple DMA
transfers.

---

## 7. The 128 Address Space Limit: A Feature, Not a Constraint

The hypervisor framework limits the guest to 128 stage-2 translation contexts.

This OS has ~10 services (kernel, OS service, compositor, GPU driver, document
store, and a handful of editors/translators). Even with generous sandboxing --
say, each content-type decoder in its own address space -- the realistic ceiling
is maybe 30-40. The 128 limit is 3-4x that.

**But it shapes the design.** 128 means the kernel can use a flat array instead
of a hash table for address space lookup. `AddressSpace[128]` at 128 bytes each
is 16 KB -- one page, permanently in the TLB. Compare to a general-purpose OS
that must handle thousands of processes with a dynamically-sized process table.
The kernel's address space management is inherently O(1) with no allocation
overhead.

---

## 8. Power and Thermal: A Personal Machine That Runs on Battery

### Hardware facts

- LPDDR5X (low-power memory)
- E-cores exist specifically for low-power background work
- WFxT for power-efficient idle
- Unified memory means no discrete GPU power draw

### Power-aware scheduling

For a personal workstation, the user isn't always actively editing. The document
is often just being viewed -- the pipeline is idle. The kernel should:

- Park P-cores aggressively when the pipeline is idle (no input events, no
  pending layout)
- Run background work (metadata indexing, snapshot GC) exclusively on E-cores
- Use WFE with long timeouts during view mode (seconds, not milliseconds)
- Wake a single P-core on input, run the interactive pipeline, then re-park

This maps directly to the "view is default, edit is deliberate" principle. View
mode is the power-efficient steady state. Edit mode wakes the full pipeline.

---

## 9. What's Unique About This Combination

Most of the above could apply to any OS on Apple Silicon. Here's what's specific
to the intersection of _this OS design_ and _this hardware_:

### The pipeline IS the memory hierarchy

The one-way data flow (Editor -> OS Service -> Scene Graph -> Compositor -> GPU)
maps directly to L2 -> SLC -> GPU cache. Data flows through the memory hierarchy
in the same direction it flows through the software pipeline. This isn't true
for most OS designs -- a general-purpose OS has chaotic memory access patterns.
The deterministic pipeline means the hardware prefetcher and cache hierarchy
work _with_ the software, not against it.

### Few processes means the scheduler is trivial

General-purpose kernels like Linux spend enormous effort on scheduler complexity
(CFS, BPF scheduler hooks, NUMA awareness for hundreds of cores). This kernel
has 14 cores and 10 services. The scheduling problem is nearly degenerate --
most cores are idle most of the time. This means the scheduler can be simple,
correct, and fast rather than complex and heuristic.

### Capability-based security + PAC = hardware-enforced capabilities

Most capability systems (seL4, EROS) enforce capabilities in software -- the
kernel checks a table on every operation. With PAC, the hardware does part of
the verification. A PAC-signed capability handle is unforgeable without the key,
and FPAC faults instantly on a forged handle. This moves part of the security
enforcement from software (kernel check) to hardware (PAC verify), making the
hot path faster.

### COW filesystem + 16 KB pages = natural alignment

The COW filesystem operates at block granularity. The MMU operates at page
granularity. When both are 16 KB, they're the same thing. A COW snapshot is
literally "point a new page table entry at the old physical page." The
filesystem's logical operation and the MMU's physical operation are one and the
same. This is simpler and faster than systems where the filesystem block size
and page size differ.

### Unified memory + document VMOs = the OS service IS a database

The OS service owns document state and mediates all writes. Documents live in
VMOs backed by the COW filesystem. The OS service maps them into its address
space and operates on them directly -- no serialization, no IPC to read document
content. It's a memory-mapped database with hardware-enforced access control
(VMO capabilities). The 48 GB of unified memory means the entire document corpus
(at personal scale, 100K files x ~10 KB average = ~1 GB) can be memory-mapped
simultaneously. The OS service never needs to "open" a file -- they're all
mapped.

### No MTE makes Rust non-negotiable

On hardware with MTE, you could write an unsafe kernel in C and get runtime
memory safety from hardware tagging. On this machine, Rust's compile-time safety
is the _only_ safety mechanism. This isn't a preference -- it's a hardware
constraint that makes the language choice load-bearing.

---

## 10. What the Ideal Kernel Does NOT Do

Following the microkernel principles -- "what must the kernel do because
userspace literally cannot?" -- the ideal kernel explicitly avoids:

- **Content understanding** (mimetype detection, layout, rendering) -- all
  userspace
- **Filesystem logic** (COW snapshots, catalog, metadata queries) -- userspace,
  behind the `Files` trait
- **Display management** (scene graph, compositing) -- userspace, sharing VMOs
- **Network** -- userspace, when it arrives at v0.12
- **Device protocol translation** -- userspace drivers, behind trait interfaces

The kernel does exactly three things (the irreducible responsibilities:
multiplex CPU/RAM, route interrupts/faults, manage the privilege boundary) plus
IPC (because the kernel created the isolation wall, so it must provide the
door). Everything else is userspace processes communicating through the kernel's
primitives. The hardware features (PAC, LSE, unified memory, WFxT) make those
primitives faster and more secure, but they don't change what belongs in the
kernel.

---

## Open Questions for Further Exploration

- **Cluster affinity as a formal constraint:** Should the kernel expose cluster
  placement as a capability right? "This VMO must only be mapped in address
  spaces whose threads are pinned to cluster 0" would formalize the
  pipeline-to-cluster mapping.

- **PAC-signed capabilities in detail:** How exactly to compute the PAC modifier
  for handle table entries. What to sign (index + rights + object pointer vs.
  index + generation counter). How PAC key rotation interacts with handle
  transfer.

- **Scene graph node sizing:** The 128 vs. 144 byte question. Hot/cold split for
  a11y data -- parallel array vs. inline. Implications for the compositor's
  cache walk pattern.

- **SME lazy state management protocol:** How the trap-on-first-use mechanism
  interacts with the scheduler. What happens if an SME-using decoder is migrated
  across clusters by work stealing.

- **Neural Engine access model:** Whether the hypervisor framework exposes
  enough of the Neural Engine for meaningful use. What a capability-controlled
  ANE interface would look like.
