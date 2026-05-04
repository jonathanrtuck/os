# Apple M4 Pro: Hardware Reference for Kernel Development

Hardware characteristics of the development and test machine — an Apple M4 Pro
(Mac16,7, target J616s). Cache hierarchy, latencies, TLB geometry, memory
subsystem, ISA extensions, security features, and virtualization constraints.

All values marked **measured** come from `sysctl` on this machine or from
published benchmarks run on M4/M4 Pro silicon. Values marked **M1 extrapolated**
come from cycle-accurate measurements on M1 Firestorm cores (same
microarchitecture family — Avalanche/Everest lineage) and are expected to be
within 1–2 cycles of M4 values but have not been independently confirmed on M4.

---

## Table of Contents

1. [Core Topology](#1-core-topology)
2. [Cache Hierarchy](#2-cache-hierarchy)
3. [TLB Geometry](#3-tlb-geometry)
4. [Memory Subsystem](#4-memory-subsystem)
5. [Latency Summary](#5-latency-summary)
6. [Memory Bandwidth](#6-memory-bandwidth)
7. [Microarchitectural Resources](#7-microarchitectural-resources)
8. [Branch Prediction](#8-branch-prediction)
9. [Atomic Operations (LSE)](#9-atomic-operations-lse)
10. [ISA Extensions](#10-isa-extensions)
11. [Security Features](#11-security-features)
12. [Virtualization Constraints](#12-virtualization-constraints)
13. [Absent Features](#13-absent-features)
14. [References](#14-references)

---

## 1. Core Topology

**Measured** on this machine.

| Property              | Value                           |
| --------------------- | ------------------------------- |
| Chip                  | Apple M4 Pro                    |
| Model                 | Mac16,7 (MacBook Pro)           |
| Total cores           | 14 (no SMT — 1 thread per core) |
| P-cores (Performance) | 10, split into 2 clusters of 5  |
| E-cores (Efficiency)  | 4, single cluster               |
| GPU cores             | 20 (Metal 4)                    |
| Packages              | 1 (unified SoC)                 |

The P-core and E-core clusters are asymmetric — different cache sizes, different
pipeline widths, different power envelopes. The kernel sees all 14 as uniform
CPUs at the ISA level, but scheduling decisions that ignore the asymmetry pay
real latency costs when data migrates between clusters.

The two P-core clusters (5 cores each) share separate L2 caches. Cross-cluster
communication goes through the System Level Cache (SLC) or DRAM — a
qualitatively different latency tier than intra-cluster L2 hits.

## 2. Cache Hierarchy

**Measured** on this machine unless noted.

### Per-core L1

|                                  | P-core                                | E-core    |
| -------------------------------- | ------------------------------------- | --------- |
| L1 instruction cache             | 192 KB                                | 128 KB    |
| L1 data cache                    | 128 KB                                | 64 KB     |
| Cache line size                  | 128 bytes                             | 128 bytes |
| Associativity                    | 8-way (M1 extrapolated)               | —         |
| L1D latency                      | 3 cycles / ~0.66 ns (M1 extrapolated) | —         |
| L1D latency (complex addressing) | 4 cycles / ~0.89 ns (M1 extrapolated) | —         |

The 128-byte cache line is a defining characteristic of Apple Silicon, double
the 64-byte line used by Cortex-A and x86. This means:

- A single cache miss pulls 128 bytes from memory — spatial prefetch is
  aggressive by default
- False sharing has a wider blast radius (two unrelated fields 100 bytes apart
  still share a line)
- Struct layout that packs hot fields into 128 bytes gets one-shot loading
- Conversely, cold data interleaved with hot data wastes more bandwidth per miss

### Shared L2

|                  | P-core cluster                               | E-core cluster            |
| ---------------- | -------------------------------------------- | ------------------------- |
| L2 size          | 16 MB shared among 5 cores                   | 4 MB shared among 4 cores |
| L2 associativity | 12-way (EXAM paper)                          | —                         |
| L2 latency       | 17–18 cycles / ~3.8–4.0 ns (M1 extrapolated) | —                         |
| L2 set indexing  | lowest 13 address bits (EXAM paper)          | —                         |
| Inclusivity      | Inclusive of L1 (EXAM paper)                 | —                         |

The 10 P-cores form two independent L2 domains of 5 cores each. A load that hits
in the local L2 costs ~4 ns. A load that must reach the other P-cluster's L2
goes through the interconnect and SLC — an order of magnitude slower.

### System Level Cache (SLC)

The SLC is a last-level cache shared across the entire SoC (CPU, GPU, Neural
Engine, media engines). It sits between the per-cluster L2 caches and DRAM.

| Property           | Value                                                     | Confidence                                |
| ------------------ | --------------------------------------------------------- | ----------------------------------------- |
| Size               | ~8 MB (base M4); M4 Pro likely 8–16 MB                    | Low — inferred from EXAM paper line count |
| Latency            | ~220 cycles / ~49 ns (M1 extrapolated, scaled to 4.5 GHz) | Low                                       |
| Exclusivity        | Exclusive w.r.t. CPU caches, inclusive w.r.t. GPU         | EXAM paper                                |
| Replacement policy | Pseudo-random                                             | EXAM paper                                |
| Set indexing       | Address bits above bit 13 of physical address             | EXAM paper                                |

The SLC is the boundary between "fast" and "slow" in the memory hierarchy. Hits
cost ~49 ns; misses go to DRAM at ~97 ns. The exclusive relationship with CPU
caches means evicting a line from L2 does not guarantee it lands in the SLC — it
may go straight to DRAM.

## 3. TLB Geometry

**M1 extrapolated** (7-cpu.com measurements). Apple does not publish TLB sizes.

| Level                    | Entries     | Miss Penalty               | Coverage (16 KB pages)       |
| ------------------------ | ----------- | -------------------------- | ---------------------------- |
| DTLB L1                  | ~160        | 6 cycles / ~1.3 ns         | ~2.5 MB                      |
| DTLB L2                  | ~3,000      | 26 cycles / ~5.8 ns        | ~48 MB                       |
| Page directory cache     | ~24 entries | —                          | ~768 MB (24 × 32 MB regions) |
| Full page walk (to DRAM) | —           | **81 ns** (measured on M4) | —                            |

The 81 ns full page walk penalty is nearly as expensive as a DRAM access itself.
With 16 KB pages, ~160 DTLB entries cover only 2.5 MB — a kernel that touches
scattered memory across large data structures will pay this penalty constantly.

**Page size:** 16,384 bytes (16 KB). This is hardware-enforced on Apple Silicon
— the MMU does not support 4 KB pages in the host translation regime. The
hypervisor framework does support 4 KB pages in stage-2 translation
(`kern.hv.ipa_size_4k`), but at reduced IPA range (40 bits vs 42 bits for 16 KB
pages).

## 4. Memory Subsystem

**Measured** on this machine.

| Property                                        | Value                                        |
| ----------------------------------------------- | -------------------------------------------- |
| Total RAM                                       | 48 GB (51,539,607,552 bytes)                 |
| Usable RAM                                      | 47.14 GB (50,626,854,912 bytes)              |
| Memory type                                     | LPDDR5X                                      |
| Manufacturer                                    | Hynix                                        |
| Bus width                                       | 256-bit                                      |
| Data rate                                       | 8533 MT/s                                    |
| Random access latency (pointer chase, TLB miss) | **96–97 ns** (measured on M4)                |
| Random access latency (TLB hit path)            | ~15 ns (measured on M4, prefetcher-friendly) |
| Page size                                       | 16 KB                                        |
| Virtual address bits                            | 47 (128 TiB VA space)                        |

The 96–97 ns random-access latency is the kernel-critical number. Sequential
access with hardware prefetching sees ~15 ns, but kernel data structures
(capability tables, page tables, scheduling queues) are rarely accessed
sequentially.

## 5. Latency Summary

Cycle counts at P-core max frequency (~4.5 GHz). Nanoseconds are approximate and
depend on actual operating frequency under load.

| Level                  | Cycles | Nanoseconds | Confidence                         |
| ---------------------- | ------ | ----------- | ---------------------------------- |
| L1D hit (simple addr)  | 3      | 0.66        | High (M1 measured, arch unchanged) |
| L1D hit (complex addr) | 4      | 0.89        | High                               |
| L1D hit (FP/SIMD)      | 5      | 1.11        | High                               |
| L2 hit                 | 17–18  | 3.8–4.0     | Medium (M1 measured)               |
| SLC hit                | ~220   | ~49         | Low (M1 scaled)                    |
| DRAM (TLB hit)         | —      | ~15         | High (M4 measured)                 |
| DRAM (TLB miss)        | —      | 96–97       | High (M4 measured)                 |
| TLB L1 miss            | 6      | 1.3         | Medium (M1)                        |
| TLB L2 miss            | 26     | 5.8         | Medium (M1)                        |
| Full page walk         | —      | 81          | High (M4 measured)                 |
| Branch mispredict      | 13–14  | 2.9–3.1     | Medium (M1)                        |

**Ratios worth internalizing:**

- L2 is ~6× slower than L1
- SLC is ~13× slower than L2
- DRAM is ~2× slower than SLC
- A TLB miss page walk costs as much as a DRAM access
- A branch mispredict costs as much as ~4 L1 hits

## 6. Memory Bandwidth

| Metric                         | GB/s                 | Source                                         |
| ------------------------------ | -------------------- | ---------------------------------------------- |
| Theoretical peak               | 273                  | Apple spec (M4 Pro)                            |
| Sequential read (multi-thread) | ~190–200 (estimated) | Scaled from M4 base (115/120 = 96% efficiency) |
| Sequential write               | ~100–110 (estimated) | Scaled from M4 base proportionally             |
| Sequential copy                | ~170 (estimated)     | Scaled from M4 base                            |
| Random uniform read            | ~10–14 (estimated)   | ~5% of peak (M4 base showed 5.3%)              |
| Per-core L2 read (streaming)   | ~120                 | Geekerwan analysis                             |

Random access bandwidth is 5% of sequential — a 20× penalty. Kernel workloads
are dominated by random access patterns (pointer chasing through capability
tables, page table walks, scheduling queue traversals). The effective bandwidth
the kernel can sustain is closer to 10–14 GB/s than 273 GB/s.

## 7. Microarchitectural Resources

**M1 Firestorm extrapolated** (Dougall Johnson measurements). M4 is expected to
be equal or larger but no public measurements exist.

| Resource                   | Count                                       |
| -------------------------- | ------------------------------------------- |
| Reorder buffer             | ~630 entries (~330 groups of ≤7 µops)       |
| Physical integer registers | ~380–394                                    |
| Physical FP/SIMD registers | ~432                                        |
| In-flight loads            | ~130                                        |
| In-flight stores           | ~60                                         |
| In-flight branches         | ~144                                        |
| Decode width               | 8 instructions/cycle                        |
| Execution ports            | 6 integer + 4 FP/SIMD (M1); M4 may be wider |
| Hardware breakpoints       | 6 (measured)                                |
| Hardware watchpoints       | 4 (measured)                                |

The 630-entry ROB and 130-entry load queue mean the M4 can have enormous amounts
of speculative work in flight. For security-sensitive kernel code, this is a
large speculation window that must be drained at trust boundaries.

## 8. Branch Prediction

| Property              | Value                             | Confidence                    |
| --------------------- | --------------------------------- | ----------------------------- |
| Misprediction penalty | 13–14 cycles / ~3 ns              | Medium (M1 measured)          |
| L1 BTB entries        | ~1,024                            | M1 measured                   |
| Decode width          | 8 instructions/cycle              | High                          |
| Load Value Predictor  | Present (unique to Apple Silicon) | Confirmed by SLAP/FLOP papers |

Apple's P-cores include a Load Value Predictor (LVP) that speculatively predicts
the value a load will return, not just whether a branch will be taken. The SLAP
and FLOP vulnerability papers (2024–2025) demonstrated that the LVP can be
exploited for speculative data leakage. The LVP is active during kernel
execution — it cannot be disabled.

## 9. Atomic Operations (LSE)

This machine supports both LSE and LSE2.

| Property                        | Value                                                 |
| ------------------------------- | ----------------------------------------------------- |
| FEAT_LSE                        | Yes — LDADD, LDCLR, LDSET, LDEOR, SWP, CAS            |
| FEAT_LSE2                       | Yes — single-copy-atomic 64-byte loads/stores         |
| Uncontended CAS latency         | ~4–6 cycles (estimated from store-to-load turnaround) |
| Cross-cache-line atomic penalty | +1 cycle (unaligned), ~28 cycles (page-crossing)      |

LSE atomics are the default code generation target for `aarch64-apple-darwin`.
They avoid the LL/SC retry loop and its associated cache-line bouncing under
contention. LSE2 guarantees that naturally-aligned 128-bit loads/stores are
single-copy-atomic, which simplifies lock-free data structure design.

Apple Silicon P-cores implement a TSO-like memory model (stronger than the ARM
specification requires), meaning acquire/release semantics on atomics have lower
overhead than on cores with the weak default ordering. However, the E-cores may
not share this property — code correctness must not depend on TSO behavior.

## 10. ISA Extensions

**Measured** on this machine. Grouped by kernel relevance.

### Security and speculation control

| Feature                                        | Status | Kernel relevance                                                         |
| ---------------------------------------------- | ------ | ------------------------------------------------------------------------ |
| FEAT_PAuth (Pointer Authentication)            | Yes    | Sign/verify return addresses and data pointers                           |
| FEAT_PAuth2                                    | Yes    | Extended PAC algorithm                                                   |
| FEAT_PACIMP (implementation-defined algorithm) | Yes    | QARMA or Apple-proprietary PAC algorithm                                 |
| FEAT_FPAC (faulting PAC)                       | Yes    | PAC failure raises fault instead of corrupting pointer                   |
| FEAT_FPACCOMBINE                               | Yes    | Combined PAC+auth in single instruction                                  |
| FEAT_BTI (Branch Target Identification)        | Yes    | Forward-edge CFI: indirect branches must land on BTI                     |
| FEAT_SB (Speculation Barrier)                  | Yes    | SB instruction drains speculation pipeline                               |
| FEAT_CSV2 (Cache Speculation Variant 2)        | Yes    | Branch predictor context-switched on CONTEXTIDR                          |
| FEAT_CSV3 (Cache Speculation Variant 3)        | Yes    | Fault/exception drains prior speculative loads                           |
| FEAT_DIT (Data Independent Timing)             | Yes    | Forces constant-time execution for crypto                                |
| CTRR v3                                        | Yes    | Configurable Text Read-only Region — hardware-enforced code immutability |
| `ptrauth_enabled`                              | Yes    | macOS kernel uses PAC                                                    |

### Atomics and synchronization

| Feature                          | Status |
| -------------------------------- | ------ |
| FEAT_LSE                         | Yes    |
| FEAT_LSE2                        | Yes    |
| FEAT_LRCPC (Load-Acquire RCpc)   | Yes    |
| FEAT_LRCPC2                      | Yes    |
| FEAT_WFxT (WFE/WFI with Timeout) | Yes    |

### Cache maintenance

| Feature                              | Status |
| ------------------------------------ | ------ |
| FEAT_DPB (DC CVAP — clean to PoP)    | Yes    |
| FEAT_DPB2 (DC CVADP — clean to PoDP) | Yes    |

### Cryptography (hardware acceleration)

| Feature     | Status |
| ----------- | ------ |
| FEAT_AES    | Yes    |
| FEAT_PMULL  | Yes    |
| FEAT_SHA1   | Yes    |
| FEAT_SHA256 | Yes    |
| FEAT_SHA3   | Yes    |
| FEAT_SHA512 | Yes    |
| FEAT_CRC32  | Yes    |

### SIMD and compute

| Feature                              | Status              |
| ------------------------------------ | ------------------- |
| AdvSIMD (NEON)                       | Yes                 |
| FEAT_FP16                            | Yes                 |
| FEAT_BF16                            | Yes                 |
| FEAT_I8MM                            | Yes                 |
| FEAT_DotProd                         | Yes                 |
| FEAT_SME (Scalable Matrix Extension) | Yes                 |
| FEAT_SME2                            | Yes                 |
| SME max streaming SVL                | 512 bits (64 bytes) |
| FEAT_FRINTTS                         | Yes                 |
| FEAT_FlagM / FlagM2                  | Yes                 |
| FEAT_JSCVT                           | Yes                 |
| FEAT_FCMA                            | Yes                 |

### Virtualization

| Feature                                    | Status |
| ------------------------------------------ | ------ |
| FEAT_ECV (Enhanced Counter Virtualization) | Yes    |
| Hypervisor support (`kern.hv_support`)     | Yes    |

## 11. Security Features

### Hardware present

- **Pointer Authentication (PAuth + FPAC):** Full implementation. FPAC means a
  failed PAC check raises a synchronous exception rather than silently
  corrupting the pointer — the kernel can trap and kill the faulting context
  immediately. PACIMP means Apple uses their own PAC algorithm (stronger than
  QARMA3).

- **Branch Target Identification (BTI):** Hardware forward-edge CFI. Indirect
  branches that land on a non-BTI instruction fault. Combined with PAC
  (backward-edge), this provides full hardware CFI.

- **Speculation Barrier (SB):** The `sb` instruction drains the speculation
  pipeline. Required at kernel entry points and trust boundary crossings to
  prevent Spectre-class attacks.

- **CSV2 + CSV3:** The branch predictor is context-switched when CONTEXTIDR_EL1
  changes (CSV2), and exception entry drains speculative loads (CSV3). Together
  these mitigate Spectre v2 (branch target injection) across context switches
  and the user/kernel boundary.

- **Data Independent Timing (DIT):** PSTATE.DIT forces the core to execute
  instructions in constant time regardless of data values. Essential for
  cryptographic operations in the kernel (key comparison, HMAC, etc.).

- **CTRR v3:** Hardware-enforced read-only region for code. Once set, the region
  cannot be modified even by EL1. Apple uses this to protect the kernel text
  segment. Available for custom kernel use via the hypervisor framework.

### Hardware absent (see §13)

MTE, SSBS, and SPECRES are not implemented. See "Absent Features" for
implications.

## 12. Virtualization Constraints

This kernel runs under the Apple Hypervisor framework, not on bare metal. The
framework imposes specific constraints.

| Property                  | Value                       | Source                          |
| ------------------------- | --------------------------- | ------------------------------- |
| Hypervisor support        | Yes                         | `kern.hv_support: 1`            |
| IPA size (16 KB pages)    | 42 bits (4 TiB)             | `kern.hv.ipa_size_16k`          |
| IPA size (4 KB pages)     | 40 bits (1 TiB)             | `kern.hv.ipa_size_4k`           |
| Max address spaces        | 128                         | `kern.hv.max_address_spaces`    |
| Available exception level | EL1 (guest kernel)          | Hypervisor framework constraint |
| Timer frequency           | 24 MHz (41.7 ns resolution) | `hw.tbfrequency`                |
| VMM currently present     | No                          | `kern.hv_vmm_present: 0`        |

**IPA limit:** The 42-bit IPA with 16 KB pages limits the guest physical address
space to 4 TiB. With 48 GB of RAM, this is not a constraint for memory mapping,
but it limits the total addressable space for device MMIO regions. The 128
address space limit means the kernel can maintain at most 128 distinct stage-2
translation contexts — relevant for process isolation design.

**Timer resolution:** The 24 MHz timer gives 41.7 ns resolution. This is coarser
than the ~0.22 ns cycle counter resolution but adequate for scheduling quanta
(typically milliseconds). The timer value is virtualized (FEAT_ECV) — the
hypervisor can offset it without trapping.

## 13. Absent Features

Features commonly expected on ARMv9 server parts that this hardware does not
implement. Each absence has design implications.

### MTE (Memory Tagging Extension) — NOT PRESENT

All MTE features report 0: FEAT_MTE, MTE2, MTE3, MTE4, MTE_ASYNC,
MTE_STORE_ONLY, MTE_CANONICAL_TAGS, MTE_NO_ADDRESS_TAGS.

**Implication:** Hardware-assisted memory safety tagging is unavailable. The
kernel cannot use MTE to detect use-after-free, buffer overflow, or type
confusion at runtime. Memory safety must come entirely from software: Rust's
type system, capability-based access control, and software guard pages. This is
a significant absence — MTE is one of the most powerful runtime memory safety
features on ARMv9. It is implemented on Cortex-X4/A720 and later ARM designs but
Apple has chosen not to include it in any Apple Silicon generation to date.

### SSBS (Speculative Store Bypass Safe) — NOT PRESENT

**Implication:** The kernel cannot use the SSBS PSTATE bit to control
speculative store bypass per-context. Spectre v4 (speculative store bypass)
mitigation must use other mechanisms — full speculation barriers (SB) or
data-dependent branch patterns.

### SPECRES (Speculation Restriction) — NOT PRESENT

Neither FEAT_SPECRES nor FEAT_SPECRES2.

**Implication:** No CFPRCTX/DVPRCTX/CPPRCTX instructions for fine-grained
speculation context management. The kernel must rely on CSV2/CSV3 (which are
present) and full SB barriers rather than targeted speculation restriction.

### FEAT_CSSC (Common Short Sequence Compression) — NOT PRESENT

**Implication:** No hardware ABS, MIN, MAX, SMAX, UMAX instructions. These must
be synthesized from compare-and-select sequences. Minor impact on kernel code
but relevant for any in-kernel data processing.

### FEAT_HBC (Hinted Conditional Branches) — NOT PRESENT

**Implication:** No BC.c hint instructions for branch prediction guidance. The
branch predictor is entirely hardware-managed.

## 14. References

### Direct measurements (this machine)

All `sysctl` values obtained from this machine: Apple M4 Pro, Mac16,7, macOS
Darwin 25.4.0, 48 GB LPDDR5X.

### Cache and latency measurements

- 7-cpu.com, "Apple M1 Firestorm" — L1/L2/TLB/branch latency cycle counts.
  [www.7-cpu.com/cpu/Apple_M1.html]
- Dougall Johnson, "Apple M1 Firestorm Overview" — ROB, register files,
  execution unit counts, instruction latencies.
  [dougallj.github.io/applecpu/firestorm.html]
- Dougall Johnson, "Apple M1 Load and Store Queue Measurements" — in-flight
  load/store capacity.
- ocxtal/insn_bench_aarch64, "Optimization Notes: Apple M1" — L1 latency
  variants, branch misprediction penalty, execution port mapping.
  [github.com/ocxtal/insn_bench_aarch64]

### SLC and cache microarchitecture

- EXAM: "Exploiting Exclusive SLC in Apple M-Series" (2025) — SLC size,
  associativity, replacement policy, exclusivity properties, L2 set indexing.
  [arxiv.org/html/2504.13385v1]

### Memory bandwidth and DRAM latency

- timoheimonen/macOS-memory-benchmark — M4 DRAM random-access latency (96.23
  ns), page walk penalty (80.94 ns), sequential bandwidth.
  [github.com/timoheimonen/macOS-memory-benchmark]
- geerlingguy, "M4 Mac Mini Benchmarks" — tinymembench results across working
  set sizes. [github.com/geerlingguy/sbc-reviews]
- Daniel Lemire, "Memory-Level Parallelism: Apple M2 vs M4" — bandwidth scaling
  comparison.

### Security vulnerabilities (LVP)

- SLAP: "Data Speculation Attacks via Load Address Prediction on Apple Silicon"
  (2024) — documents the Load Value Predictor.
- FLOP: "Breaking the Apple M3's Load Value Predictor" (2025) — exploitation of
  LVP for speculative data leakage. [arxiv.org/abs/2411.13900]

### Microarchitectural ROB measurements

- complang.tuwien.ac.at, "ROB Size Measurements" — M1 Firestorm ROB = 630
  entries. [www.complang.tuwien.ac.at/anton/robsize/]

### Apple specifications

- Apple, "M4 Pro Technical Specifications" — official core counts, memory
  bandwidth, GPU cores. [support.apple.com/en-us/121553]
- Eclectic Light Company, "Inside M4 Chips: P-cores" — frequency, power, cluster
  topology analysis.
