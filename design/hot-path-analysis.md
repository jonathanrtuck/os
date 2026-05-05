# Hot Path Analysis — M4 Pro Theoretical Minimums

Cycle costs computed from M4 Pro microarchitecture characteristics. These
are the floor: the minimum possible cycle cost given what the hardware
requires. The gap between measured and theoretical reveals optimization
opportunities.

## Hardware Reference (M4 Pro)

| Parameter | Value |
|-----------|-------|
| Cache line | 128 bytes |
| L1D latency | 3 cycles |
| L2 latency | 17–18 cycles |
| SLC latency | ~49 ns (~220 cycles @ 4.5 GHz) |
| DRAM random | ~97 ns (~437 cycles) |
| DTLB entries | ~160 (covers ~2.5 MB with 16 KB pages) |
| Mispredict penalty | ~13 cycles |
| STP throughput | 2 per cycle (paired stores) |
| ISB cost | pipeline flush, ~8–15 cycles |

## IPC Round-Trip (call → recv → reply → caller unblocks)

The single most important number. Every tool↔OS interaction takes this path.

### Null IPC (0-byte message, 0 handles)

| Step | Cycles | Notes |
|------|--------|-------|
| SVC → exception entry | ~20 | Pipeline flush + vector fetch |
| Register save (STP fast path) | ~4 | 4× STP = 8 registers |
| Syscall dispatch (match) | ~5 | Jump table, 1 branch |
| Handle lookup (caller) | 3–6 | 1 L1D load if HandleTable hot |
| Endpoint.recv_waiters check | 3–6 | 1–2 L1D loads |
| Thread state update (block caller) | ~3 | 1 store |
| Scheduler: dequeue server | ~6 | RunQueue access + pop |
| Context switch to server | ~20 | Register restore + ERET |
| **Server processes (user code)** | — | Not measured |
| SVC for reply | ~20 | Trap again |
| Reply dispatch | ~10 | Handle lookup + reply cap |
| Caller wake + enqueue | ~6 | Thread state + scheduler |
| Context switch to caller | ~20 | Register restore + ERET |
| **Total** | **~120–130** | L1D-hot assumption |

### Full IPC (128-byte message + 4 handles)

Add to null IPC:

| Step | Additional cycles |
|------|-------------------|
| Message copy (caller → kernel) | ~8 | 128B = 16× LDR or 8× LDP |
| Message copy (kernel → server) | ~8 | Same |
| Reply message copy (×2) | ~16 | Both directions |
| Handle validation (4×) | ~12–24 | 4× handle lookup |
| Handle staging + install | ~16 | Array writes + alloc |
| **Full IPC total** | **~180–210** | |

### IPC with Priority Inheritance

Add to null IPC:

| Step | Additional cycles |
|------|-------------------|
| Priority comparison | ~2 | Compare caller vs server |
| Effective priority update | ~3 | If server inherits |
| Scheduler re-priority | ~6 | Remove + re-insert in queue |
| **Priority IPC total** | **~130–140** | |

## Event Signal-to-Wake

| Step | Cycles | Notes |
|------|--------|-------|
| SVC → exception entry | ~20 | |
| Handle lookup | 3–6 | |
| Bit-OR operation | ~1 | |
| Waiter scan (check mask match) | 3–16 | 1 waiter = 3, 16 waiters = 16 |
| Thread wake (mark Ready) | ~3 | |
| Scheduler enqueue | ~3 | |
| Return (register restore + ERET) | ~20 | |
| **Total (1 waiter, signaled)** | **~55–70** | |

Note: if no waiter matches, skip wake+enqueue: ~30–40 cycles.

## Page Fault Resolution (Lazy Zero Page)

| Step | Cycles | Notes |
|------|--------|-------|
| Exception entry (data abort) | ~20 | |
| FAR/ESR decode | ~3 | MRS instructions |
| Address space mapping lookup | ~6–20 | Binary search over mappings |
| Page allocate (bitmap scan) | ~10–30 | Depends on fragmentation |
| Zero page | ~32 | 128 × STP to 16 KB page |
| Page table update (PTE write) | ~10 | Walk + write + TLBI |
| Return (ERET) | ~20 | |
| **Total** | **~100–135** | |

## Object Creation (VMO)

| Step | Cycles | Notes |
|------|--------|-------|
| SVC entry | ~20 | |
| ObjectTable alloc (free list pop) | ~6 | |
| Vmo::new (field init) | ~5 | |
| HandleTable alloc (free list pop) | ~6 | |
| Handle install | ~3 | |
| Return | ~20 | |
| **Total** | **~60–80** | No heap alloc on fast path |

## Handle Lookup

| Step | Cycles | Notes |
|------|--------|-------|
| Index bounds check | ~1 | |
| Array access (entries[idx]) | 3–6 | 1 L1D load |
| Generation check | ~3 | Compare + branch |
| **Total** | **~7–10** | |

This is the innermost hot path — every syscall starts with at least one
handle lookup. Handle at 24 bytes fits in a single cache line.

## Clock Read

| Step | Cycles | Notes |
|------|--------|-------|
| SVC entry | ~20 | |
| MRS CNTVCT_EL0 | ~2 | |
| Frequency conversion | ~5 | Multiply + shift |
| Return | ~20 | |
| **Total** | **~47** | Floor for any syscall |

## Optimization Priorities (by impact)

1. **IPC round-trip** — dominates all tool↔OS interaction. Every cycle
   saved here multiplies across every document operation.
2. **Event signal-to-wake** — the compositor path. Determines UI latency
   between "content changed" and "screen updated."
3. **Page fault resolution** — governs startup and first-access latency.
   Lazy allocation means every mapped page faults on first touch.
4. **Handle lookup** — appears in every syscall. Already near theoretical
   minimum at 24 bytes per handle.
5. **Object creation** — less frequent but governs document-open latency
   when many VMOs are created.
