# Hot Path Analysis — M4 Pro Theoretical Minimums

Cycle costs computed from M4 Pro microarchitecture characteristics. These are
the floor: the minimum possible cycle cost given what the hardware requires. The
gap between measured and theoretical reveals optimization opportunities.

## Hardware Reference (M4 Pro)

| Parameter          | Value                                  |
| ------------------ | -------------------------------------- |
| Cache line         | 128 bytes                              |
| L1D latency        | 3 cycles                               |
| L2 latency         | 17–18 cycles                           |
| SLC latency        | ~49 ns (~220 cycles @ 4.5 GHz)         |
| DRAM random        | ~97 ns (~437 cycles)                   |
| DTLB entries       | ~160 (covers ~2.5 MB with 16 KB pages) |
| Mispredict penalty | ~13 cycles                             |
| STP throughput     | 2 per cycle (paired stores)            |
| ISB cost           | pipeline flush, ~8–15 cycles           |

## IPC Round-Trip (call → recv → reply → caller unblocks)

The single most important number. Every tool↔OS interaction takes this path.

### Null IPC (0-byte message, 0 handles)

| Step                               | Cycles       | Notes                         |
| ---------------------------------- | ------------ | ----------------------------- |
| SVC → exception entry              | ~20          | Pipeline flush + vector fetch |
| Register save (STP fast path)      | ~4           | 4× STP = 8 registers          |
| Syscall dispatch (match)           | ~5           | Jump table, 1 branch          |
| Handle lookup (caller)             | 3–6          | 1 L1D load if HandleTable hot |
| Endpoint.recv_waiters check        | 3–6          | 1–2 L1D loads                 |
| Thread state update (block caller) | ~3           | 1 store                       |
| Scheduler: dequeue server          | ~6           | RunQueue access + pop         |
| Context switch to server           | ~20          | Register restore + ERET       |
| **Server processes (user code)**   | —            | Not measured                  |
| SVC for reply                      | ~20          | Trap again                    |
| Reply dispatch                     | ~10          | Handle lookup + reply cap     |
| Caller wake + enqueue              | ~6           | Thread state + scheduler      |
| Context switch to caller           | ~20          | Register restore + ERET       |
| **Total**                          | **~120–130** | L1D-hot assumption            |

### Full IPC (128-byte message + 4 handles)

Add to null IPC:

| Step                           | Additional cycles |
| ------------------------------ | ----------------- | ------------------------ |
| Message copy (caller → kernel) | ~8                | 128B = 16× LDR or 8× LDP |
| Message copy (kernel → server) | ~8                | Same                     |
| Reply message copy (×2)        | ~16               | Both directions          |
| Handle validation (4×)         | ~12–24            | 4× handle lookup         |
| Handle staging + install       | ~16               | Array writes + alloc     |
| **Full IPC total**             | **~180–210**      |                          |

### IPC with Priority Inheritance

Add to null IPC:

| Step                      | Additional cycles |
| ------------------------- | ----------------- | --------------------------- |
| Priority comparison       | ~2                | Compare caller vs server    |
| Effective priority update | ~3                | If server inherits          |
| Scheduler re-priority     | ~6                | Remove + re-insert in queue |
| **Priority IPC total**    | **~130–140**      |                             |

## Event Signal-to-Wake

| Step                             | Cycles     | Notes                         |
| -------------------------------- | ---------- | ----------------------------- |
| SVC → exception entry            | ~20        |                               |
| Handle lookup                    | 3–6        |                               |
| Bit-OR operation                 | ~1         |                               |
| Waiter scan (check mask match)   | 3–16       | 1 waiter = 3, 16 waiters = 16 |
| Thread wake (mark Ready)         | ~3         |                               |
| Scheduler enqueue                | ~3         |                               |
| Return (register restore + ERET) | ~20        |                               |
| **Total (1 waiter, signaled)**   | **~55–70** |                               |

Note: if no waiter matches, skip wake+enqueue: ~30–40 cycles.

## Page Fault Resolution (Lazy Zero Page)

| Step                          | Cycles       | Notes                       |
| ----------------------------- | ------------ | --------------------------- |
| Exception entry (data abort)  | ~20          |                             |
| FAR/ESR decode                | ~3           | MRS instructions            |
| Address space mapping lookup  | ~6–20        | Binary search over mappings |
| Page allocate (bitmap scan)   | ~10–30       | Depends on fragmentation    |
| Zero page                     | ~32          | 128 × STP to 16 KB page     |
| Page table update (PTE write) | ~10          | Walk + write + TLBI         |
| Return (ERET)                 | ~20          |                             |
| **Total**                     | **~100–135** |                             |

## Object Creation (VMO)

| Step                              | Cycles     | Notes                      |
| --------------------------------- | ---------- | -------------------------- |
| SVC entry                         | ~20        |                            |
| ObjectTable alloc (free list pop) | ~6         |                            |
| Vmo::new (field init)             | ~5         |                            |
| HandleTable alloc (free list pop) | ~6         |                            |
| Handle install                    | ~3         |                            |
| Return                            | ~20        |                            |
| **Total**                         | **~60–80** | No heap alloc on fast path |

## Handle Lookup

| Step                        | Cycles    | Notes            |
| --------------------------- | --------- | ---------------- |
| Index bounds check          | ~1        |                  |
| Array access (entries[idx]) | 3–6       | 1 L1D load       |
| Generation check            | ~3        | Compare + branch |
| **Total**                   | **~7–10** |                  |

This is the innermost hot path — every syscall starts with at least one handle
lookup. Handle at 24 bytes fits in a single cache line.

## Clock Read

| Step                 | Cycles  | Notes                 |
| -------------------- | ------- | --------------------- |
| SVC entry            | ~20     |                       |
| MRS CNTVCT_EL0       | ~2      |                       |
| Frequency conversion | ~5      | Multiply + shift      |
| Return               | ~20     |                       |
| **Total**            | **~47** | Floor for any syscall |

## HVF Ground Truth (2026-05-07)

The numbers above are theoretical floors. Until 2026-05-07 we could only compare
them against blended `make bench` totals — guest cycles + macOS
Hypervisor.framework (HVF) emulation cycles fused into one number. To know which
floors are reachable in software, we need to subtract the HVF tax.

The hypervisor now stamps a per-vCPU counter page inside guest RAM (advertised
in the DTB as `arts,hvf-timing-v1`) updated at every VMEXIT. Each slot exposes
`guest_ticks` (time inside `hv_vcpu_run`), `host_ticks` (time in HVF dispatch +
emulation handlers), and per-class exit counters (data abort, HVC, sysreg trap,
WFI/WFE, vtimer). The kernel reads it via `frame::arch::aarch64::hvf_timing`,
and every line in `make bench` output — the per-syscall benchmarks, the workload
benchmarks, and the cycle estimates — carries a `guest / host / exits/k` split.
The benchmarks-section columns are per-iteration ticks; the cycle-estimates
columns are per-iteration cycles ×10 (4.5 GHz).

The instrumentation needs a fence: HVF only updates the counters at exit
boundaries, so a long bench that never traps would see a zero delta. Each
`estimate_with_hvf` call brackets the bench with an unrecognized HVC
(`hvf_timing::force_snapshot`) which forces the hypervisor to flush in-flight
`mach_absolute_time` deltas into the page before the kernel reads. The fence
costs one HVC-class exit per side.

### Sanity check

| Bench    | Total (cyc) | guest (cyc) | host (cyc) | exits/kop |
| -------- | ----------- | ----------- | ---------- | --------- |
| nop; nop | 1.1         | 1.2         | 0.0        | 0.0       |

Pure guest instructions, no traps. `guest ≈ total` and `host ≈ 0` confirms the
page is being read coherently and there is no host time-stealing during the
bench window.

### Four hot-path floors under HVF

| Path                | Theoretical floor | Measured |  guest | host | Ratio (measured/floor) |
| ------------------- | ----------------: | -------: | -----: | ---: | ---------------------: |
| svc null trap+eret  |                50 |    146.6 |  147.1 |  0.0 |                   2.9× |
| dispatch overhead   |                 5 |      4.1 |    4.2 |  0.0 |                   0.8× |
| IPC null round-trip |               150 |   6957.3 | 6997.9 |  0.2 |                  46.4× |
| fault lookup        |                15 |    147.3 |  147.5 |  0.0 |                   9.8× |

All numbers in 4.5 GHz cycles per op (one decimal of precision). Sample size 500
× 100 = 50,000 ops measured + 500 warmup, taken under HVF on M4 Pro (macOS
Tahoe-class host, 4 vCPUs, no GPU).

**Reading the table.**

- `guest_ticks ≈ total` for every bench — the kernel's bench paths run entirely
  inside `hv_vcpu_run` with no trap-out. Optimizing kernel code pays back at the
  kernel rate; HVF is not the bottleneck.
- `host_ticks ≈ 0` because the bench window has no MMIO (no UART writes, no
  virtio descriptors). The 0.2 cyc/op on IPC round-trip is fence-HVC handler
  time amortized over 50,500 ops.
- `exits/kop ≈ 0` (1 exit per 50,000 ops = 0.02/k, rounds to zero). The fence is
  the only exit cause during measurement.

**Implication for `IPC null round-trip` (46.4× the floor).** This is the single
biggest gap and it is _all kernel work_, not HVF tax. The 6800 cyc delta from
the 150-cyc theoretical floor is software cost in the IPC fast path: handle
lookups, address-space switch, reply-cap allocation, priority-inheritance
bookkeeping, scheduler dequeue/enqueue. None of that has been profiled;
instrumentation is the next step.

**Implication for `svc null` (2.9× the floor).** Not zero, but the floor itself
is conservative — the 50-cycle estimate covered the abstract "trap+ERET"
round-trip but not the kernel's full trap-frame save sequence, the per-CPU
current-thread read, the ESR decode, or the dispatch table jump. The 146.6 cyc
figure is closer to "fully unloaded SVC" than "minimum possible."

**Implication for `fault lookup` (9.8× the floor).** Worth investigating — the
floor assumed a ~15-cycle hash lookup; the measured path includes a binary
search over `find_mapping`, a slot-lock acquire, and a VMO table read. Two of
three are eliminable with a per-thread cache.

### Limits of this technique

- One fence HVC per snapshot is the minimum perturbation but not zero. For
  benches that already include real exits the relative cost is negligible; for
  sub-cycle ops the fence still adds one VMEXIT to each interval.
- The host-side timer is `mach_absolute_time` (24 MHz on Apple Silicon, same
  source as guest `CNTVCT_EL0`). Host pauses (e.g., GCD scheduling the vCPU
  thread off-CPU) inflate `host_ticks`, not `guest_ticks` — that is the correct
  attribution.
- The classification surface is what HVF reports as `HV_EXIT_REASON_*` and
  `ESR.EC`. WFI vs WFE is collapsed into a single `exits_wfx` bucket; if finer
  detail is needed, decode the `ISS` field and add new slots.

## Where the Cycles Go — Measured Profiling (2026-05-07)

The profiling infrastructure (`feature = "profile"`) stamps CNTVCT_EL0 at each
stage of the syscall path. Combined with HVF timing, this decomposes wall-clock
time into kernel software stages.

### Optimizations Applied

#### Phase 1: PerCpu caching (2026-05-07 early)

Two PerCpu caching optimizations eliminated lock overhead from the syscall hot
path:

1. **`current_space` in PerCpu** — eliminated `thread_space_id()` which acquired
   a TicketLock on the thread table to read an immutable field. Was 128 cycles
   per syscall.

2. **`handle_table_ptr` in PerCpu** — cached a raw pointer to the current
   space's HandleTable during context switch. `lookup_handle()` now reads
   directly from the pointer, bypassing the per-space TicketLock and
   ConcurrentTable pointer chase (~35 cycles saved per lookup).

#### Phase 2: IPC lock batching (2026-05-07 late)

Systematic elimination of redundant slot lock acquisitions in the IPC
call→recv→reply round-trip. The original path did ~30 ConcurrentTable slot lock
acquire/releases per round-trip; each costs ~12-15 cycles (atomic fetch_add for
ticket, spin on comparison, store for release). Savings:

3. **Batched thread writes in `sys_call`** — combined 3 separate server-thread
   lock acquisitions (set_wakeup_value + boost_priority + state check) into 1.
   Also batched 2 post-switch reads (wakeup_error + wakeup_value) into 1.

4. **Batched thread reads in `sys_reply`** — combined caller state check +
   address_space extraction into 1 read. Combined wakeup_value set + priority
   read into 1 write.

5. **`switch_to_space_by_id(AddressSpaceId)`** — replaced
   `switch_to_space_of(ThreadId)` which did thread read → space read → TTBR0
   switch. The new function takes the space ID directly (already known from
   RecvState or a prior thread read), skipping the thread table read. Used 4
   times across call + reply paths.

6. **`set_current_thread_fast`** — takes pre-extracted space_id / ht_ptr /
   pt_root / asid instead of looking them up from thread + space tables. Saves
   2 lock acquisitions per context switch. PerCpu now also caches `pt_root` and
   `pt_asid`.

7. **`switch_threads_set_states`** — batches set_state + RegisterState pointer
   extraction into one locked section per thread. Eliminates 2 separate thread
   table writes that the old pattern (set_state then switch_threads) required.

8. **`switch_to_page_table(pt_root, asid)`** — direct TTBR0 switch from known
   values, bypassing the 2-read lookup chain in `maybe_switch_page_table`.

Net: ~12 slot lock acquisitions eliminated per IPC round-trip.

### Results After All Optimizations

| Syscall             | Phase 1 | Phase 2 | Speedup | Floor | Multiple |
| ------------------- | ------: | ------: | ------: | ----: | -------: |
| handle_info         |    15.0 |    15.0 |      — |    15 | **1.0×** |
| event_signal        |   101.6 |   103.5 |      — |    15 |     6.9× |
| event_wait          |    93.7 |    96.0 |      — |    15 |     6.4× |
| event_clear         |   159.0 |   161.6 |      — |    15 |    10.7× |
| handle_dup+close    |   327.7 |   348.0 |      — |    30 |    11.6× |
| endpoint create     | 1,156.8 | 1,168.1 |      — |    50 |    23.3× |
| IPC null round-trip | 6,606.0 | 6,368.0 |    −4% |   150 |    42.4× |

EL1 bench numbers are conservative — the EL1 IPC bench doesn't exercise full
context switches. The EL0 SMP bench shows the real gain: 4874 → 4114 cyc/rtt
(−15.6%) for 2-core IPC round-trip.

### Remaining Cost Breakdown (by profiler stages)

**handle_info** (15 cyc, at floor): MRS TPIDR_EL1 → load handle_table_ptr →
array index → clone → generation atomic load → return. No further optimization
possible without changing the syscall ABI.

**event_signal** (104 cyc): ~15 handle lookup + ~45 event read+signal+waiter
scan + ~40 scheduler enqueue. Waiter scan is O(n) with n=max waiters.

**endpoint_create** (1,168 cyc): dominated by `alloc_shared` (433 cyc) — the
Endpoint struct is large (multiple arrays for pending calls, recv waiters, reply
caps). Each array field touches a fresh cache line on construction.

**IPC null round-trip** (6,368 cyc EL1 / 4,114 cyc EL0): remaining cost is in
the context switch assembly (~60 cyc × 2), TTBR0 switches (~200+ cyc × 2-3),
endpoint table accesses (~2 locks), remaining thread table accesses (~18 locks),
and message copy / user memory writes.

### Profiling Limitations

- IPC profiling from EL1 cannot measure through `direct_switch` because the
  context switch suspends the bench thread. Full IPC stage decomposition
  requires a userspace bench (bench-el0).
- Exception entry/exit overhead (save/restore ~30 GPRs + FP) is measured by the
  assembly stamps in exception.S but not yet decomposed further.

## Optimization Priorities (by impact, updated 2026-05-07)

1. **IPC round-trip** (42× floor EL1, 27× EL0) — still the biggest gap. The
   remaining ~18 slot lock acquisitions per round-trip cost ~200-270 cycles.
   TTBR0 switches cost ~400-600 cycles (2-3 per round-trip). Next targets:
   same-space TTBR0 skip, endpoint table access reduction, and evaluating
   whether the remaining thread table accesses can be further batched or cached.
2. **Endpoint create** (23× floor) — alloc_shared dominates. The Endpoint struct
   initialization touches many cache lines.
3. **Event signal/clear/wait** (6–11× floor) — handle lookup is now fast; the
   remaining cost is in the event/scheduler operations (waiter scan, enqueue).
4. **Handle dup+close** (12× floor) — the `write()` path still takes the
   TicketLock for mutations. Consider a lock-free close path.
5. **Fault resolution** (10× floor) — binary search over mappings + VMO table
   read. Per-thread mapping cache would help.
