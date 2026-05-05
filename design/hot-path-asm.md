# Hot Path Assembly Inspection — Release Build on M4 Pro

Inspected with `cargo objdump --release -p kernel -- --disassemble-symbols=<sym>`.
Build: `aarch64-unknown-none` with `target-feature=+lse,+lse2,+rcpc`.

## SVC Fast Handler (17 instructions)

The SVC entry point for syscalls from EL0. This is the absolute fast path —
every syscall transits through here.

```asm
svc_fast_handler:
  sub  sp, sp, #0x40          ; 64-byte frame (1/2 cache line)
  stp  x5, x30, [sp, #0x28]  ; save x5 + return address
  stp  x0, x1, [sp]           ; save syscall args (x0=num, x1=a0)
  stp  x2, x3, [sp, #0x10]   ; save args (x2=a1, x3=a2)
  mov  x3, x6                 ; a5 → x3
  str  x4, [sp, #0x20]        ; save a3
  mov  x4, sp                 ; args pointer → x4
  mrs  x8, TPIDR_EL1          ; load PerCpu pointer
  mrs  x9, CNTVCT_EL0         ; watchdog: read counter
  str  x9, [x8, #0x18]        ; watchdog: store entry timestamp
  ldp  w2, w1, [x8]           ; load core_id + thread_id from PerCpu
  ldr  x0, [x8, #0x8]         ; load kernel pointer from PerCpu
  bl   dispatch                ; → Kernel::dispatch
  mrs  x8, TPIDR_EL1          ; reload PerCpu
  str  xzr, [x8, #0x18]       ; watchdog: clear entry timestamp
  ldr  x30, [sp, #0x30]       ; restore return address
  add  sp, sp, #0x40           ; deallocate frame
  ret                          ; → exception return stub
```

**Assessment: Excellent.** Tight 64-byte frame (half a cache line). STP pairs
for all saves. No wasted instructions. Watchdog adds 3 instructions (mrs +
str + str xzr on exit) — acceptable.

**Potential optimization:** The watchdog timestamp is debug-only but currently
compiled into release. Gating on `debug_assertions` would save 3 instructions
on the hot path. (Current code already uses cfg for the check, but the timestamp
write is unconditional.)

## Kernel::dispatch (5009 instructions, ~20 KB)

All 30 syscall handlers are inlined by LLVM into a single monolithic function.

**Entry sequence:**
```asm
dispatch:
  str  d8, [sp, #-0x70]!      ; save FP callee-saved
  stp  x29, x30, [sp, #0x10]  ; save frame pointer + LR
  stp  x28-x19, [sp, ...]     ; save callee-saved GPRs (6 × STP = 12 regs)
  sub  x9, sp, #0x9000        ; stack probe target (36 KB!)
  sub  sp, sp, #0x1000         ; probe loop: touch each page
  cmp  sp, x9
  str  xzr, [sp]
  b.ne <probe_loop>
  sub  sp, sp, #0xbb0          ; remaining 2992 bytes
  cmp  x3, #0x1d               ; syscall number check (30 syscalls)
  b.hi <invalid>
  ; jump table dispatch
  ldrsw x10, [x8, x3, lsl #2] ; load offset from jump table
  br   x9                      ; jump to handler
```

**Key findings:**

1. **Stack frame: ~39 KB.** This is large. The stack probe loop iterates ~9
   times (touching each 4 KB page), costing ~27 cycles. Root cause: LLVM
   inlines all syscall handlers, and the largest (endpoint operations) need
   ~6 KB for inline Endpoint construction.

2. **Jump table dispatch: good.** `ldrsw` + `br` is a 2-instruction dispatch
   from a PC-relative offset table. Branch predictor can learn this pattern.

3. **Callee-saved register pressure:** 7 STP operations at entry (14 registers
   + d8). This is the cost of a monolithic function — LLVM needs all these
   registers for the inlined handlers.

**Optimization opportunity:** Marking the largest syscall handlers (sys_call,
sys_recv, sys_reply, sys_thread_create_in) as `#[inline(never)]` would let
LLVM give them separate, smaller stack frames. The dispatch function itself
would shrink dramatically, and the rarely-used handlers wouldn't inflate the
common-case stack. Tradeoff: one extra `bl`/`ret` pair per syscall (~2 cycles),
but saves ~25 cycles of stack probing for the common case.

## HandleTable::install (25 instructions)

On the hot path of every object-creating syscall.

```asm
install:
  ldr  w0, [x0, #0x3808]     ; load free_head
  cmn  w0, #0x1               ; check sentinel (no free slots)
  b.eq <full>
  cmp  w0, #0x1ff             ; bounds check (MAX_HANDLES)
  b.hi <panic>
  add  x11, x9, x0, lsl #2   ; free list pointer
  ldr  d0, [x1, #0x8]         ; 64-bit load of handle fields
  umaddl x12, w0, w12, x9     ; slot address: base + idx * 24
  ; ... copy handle data, update free list, return
  ret
```

**Assessment: Good.** Uses `umaddl` (multiply-add-long) for address calc,
`ldr d0` for 64-bit field copy, free list traversal is O(1). No unnecessary
barriers.

## HandleTable::remove (40 instructions)

```asm
remove:
  cmp  w1, #0x1ff             ; bounds check
  b.ls <valid>
  ; ... invalid handle fast path (5 instructions) → ret
<valid>:
  umaddl x12, w11, w9, x0     ; slot address
  ldrb w9, [x12, #0x14]       ; load type tag
  ldr  x10, [x12]             ; load object pointer
  strb w13, [x12, #0x14]      ; mark slot as Free
  ; ... copy out removed handle, update free list
  ret
```

**Assessment: Adequate.** Small stack allocation mid-function for byte
shuffling of handle fields. This could be eliminated with better struct layout
(packing type/gen/rights into a single u32 instead of individual bytes), but
the impact is ~2 cycles — not worth the refactor.

## LSE Atomics — Fully Enabled

After adding `target-feature=+lse,+lse2,+rcpc`:

| Operation | Before (LL/SC) | After (LSE) | Savings |
| --- | --- | --- | --- |
| Ticket lock acquire | ldaxr + add + stlxr + cbnz (4 insn loop) | ldaddal (1 insn) | 3 insn, no retry |
| Ticket lock release | ldaxr + add + stlxr + cbnz (4 insn loop) | ldaddl (1 insn) | 3 insn, no retry |
| FP ownership swap | ldxr + stxr + cbnz (3 insn loop) | swp (1 insn) | 2 insn, no retry |
| Page refcount | ldaxrh + op + stlxrh (3+ insn) | ldaddalh (1 insn) | 2+ insn |
| CAS operations | ldaxr + cmp + stlxr (3+ insn) | casalb (1 insn) | 2+ insn |

**Zero LL/SC instructions remain in the release binary.** This eliminates
cache-line bouncing under SMP contention entirely — LSE atomics are
performed in the cache/interconnect without retry loops.

## Spin Wait Pattern

The ticket lock spin uses `isb` (from `core::hint::spin_loop()`):

```asm
  ldapr  w10, [x19]           ; load-acquire now_serving (RCPC)
  cmp    w10, w9              ; compare with our ticket
  b.eq   <acquired>
  isb                         ; pipeline flush (~8-15 cycles) as spin pause
  ldapr  w10, [x19]           ; retry
  cmp    w10, w9
  b.ne   <spin>
```

**Note:** `ldapr` (RCPC load-acquire) is used instead of `ldar` — enabled by
the `+rcpc` target feature. RCPC provides weaker ordering than full `ldar`
(allows reordering with prior non-dependent loads), which is safe here since
we only care about the `now_serving` value.

**Potential optimization:** Replace `isb` with `wfe` + add explicit `sev` to
unlock. WFE puts the core in a low-power state until an event arrives, vs ISB
which just flushes the pipeline. Requires adding `sev` instruction to the
unlock path. Impact: power savings under contention, slight latency improvement
(WFE wakes on SEV vs ISB re-executes immediately).

## Summary

| Area | Status | Notes |
| --- | --- | --- |
| SVC fast path | Optimal | 17 instructions, STP pairs |
| LSE atomics | Fixed | Was LL/SC, now single-instruction LSE |
| Jump table dispatch | Good | 2-instruction syscall routing |
| Stack frame | Improvable | ~39 KB due to full inlining |
| HandleTable ops | Good | O(1) free list, umaddl addressing |
| Spin wait | Acceptable | ISB-based, WFE possible future opt |
| FP save/restore | Optimal | STP/LDP Q-register pairs |
| Register save | Good | STP pairs for callee-saved |
