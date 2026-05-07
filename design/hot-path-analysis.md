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

## Optimization Priorities (by impact)

1. **IPC round-trip** — dominates all tool↔OS interaction. Every cycle saved
   here multiplies across every document operation.
2. **Event signal-to-wake** — the compositor path. Determines UI latency between
   "content changed" and "screen updated."
3. **Page fault resolution** — governs startup and first-access latency. Lazy
   allocation means every mapped page faults on first touch.
4. **Handle lookup** — appears in every syscall. Already near theoretical
   minimum at 24 bytes per handle.
5. **Object creation** — less frequent but governs document-open latency when
   many VMOs are created.
