# TPIDR_EL1 Invariant Chain

Cross-module verification of the TPIDR_EL1 invariant: **on every core, TPIDR_EL1
always points to the current thread's `Context` struct (at offset 0 of the `Thread`).**

This invariant is the backbone of context save/restore — `exception.S` reads
TPIDR_EL1 to locate the save area for every exception entry. A stale or invalid
TPIDR_EL1 corrupts whichever thread's Context it points to, causing data loss,
privilege escalation, or an opaque EC=0x21 crash on eret.

**Verified:** 2026-03-14
**Scope:** boot.S, exception.S, scheduler.rs, main.rs, context.rs, thread.rs
**Method:** Enumerated every `msr tpidr_el1` and `mrs tpidr_el1` instruction
across all kernel source files (35 .rs + 2 .S). Verified the invariant holds
at each site by tracing the pointer's provenance.

---

## 1. Structural Foundation

### Thread layout (thread.rs:134–137)

```rust
/// `context` MUST be the first field — `TPIDR_EL1` points at the start of
/// the Thread, and exception.S expects the Context at offset 0.
#[repr(C)]
pub struct Thread {
    pub(crate) context: Context,
    // ... other fields
}
```

**Compile-time enforcement:** `offset_of!(Thread, context) == 0` (implicit from
`#[repr(C)]` with `context` as the first field; the Context struct has explicit
compile-time assertions in context.rs matching exception.S CTX\_\* offsets).

### Context struct (context.rs:20–31)

All 0x330 bytes of register state saved/restored by exception.S. Offsets verified
by compile-time assertions matching CTX_X, CTX_SP, CTX_ELR, CTX_SPSR, CTX_SP_EL0,
CTX_TPIDR_EL0, CTX_Q, CTX_FPCR, CTX_FPSR constants in exception.S.

**Critical:** TPIDR_EL1 is NOT saved in Context — it points _to_ the Context.
It is a per-core register, not a per-thread value (context.rs:6).

### Pointer stability (scheduler.rs:72–73)

```rust
// Box<Thread> is intentional: threads must have stable addresses because
// TPIDR_EL1 holds a raw pointer to the current thread's Context.
```

All threads are `Box<Thread>` — heap-allocated with a stable address for the
lifetime of the Thread. The pointer stored in TPIDR_EL1 remains valid as long
as the Thread exists.

### PerCpu vs TPIDR_EL1 (per_core.rs:7–9)

```rust
//! TPIDR_EL1 continues to point at the current Thread's Context — PerCpu is
//! a side table accessed via `core_id()`, not through TPIDR_EL1.
```

Per-core data uses a separate MPIDR-indexed array. TPIDR_EL1 is exclusively
for the current thread's Context.

---

## 2. All TPIDR_EL1 Write Sites (6 total)

### Write 1: `scheduler::init()` — Core 0 boot thread

**File:** scheduler.rs:1026–1033
**When:** Called once from `kernel_main`, after heap init, before any exceptions.
**IRQ state:** Disabled (boot — no GIC init yet, DAIF.I set by default).

```rust
// SAFETY: ctx_ptr points to the Context at offset 0 of the boot thread,
// which lives in a Box (stable address) stored in the scheduler state.
// TPIDR_EL1 is read by exception.S to locate the save area.
unsafe {
    core::arch::asm!(
        "msr tpidr_el1, {0}",
        in(reg) ctx_ptr as usize,
        options(nostack)
    );
}
```

**Provenance:** `ctx_ptr = boot_thread.context_ptr()` → `&self.context as *const Context`.
The boot thread is `Box::new(Thread::new_boot())`, stored in `s.cores[0].current`.

**Invariant holds:** TPIDR_EL1 = address of core 0's boot thread Context. No
other thread exists yet. IRQs are disabled, so no exception can read a stale value.

---

### Write 2: `scheduler::init_secondary()` — Secondary core boot threads

**File:** scheduler.rs:1054–1059
**When:** Called from `secondary_main` on each secondary core (1..7) during SMP boot.
**IRQ state:** Disabled (scheduler lock held via `STATE.lock()`, which masks IRQs).

```rust
// SAFETY: boot_ctx_ptr points to a stable Context in a Box.
unsafe {
    core::arch::asm!(
        "msr tpidr_el1, {0}",
        in(reg) boot_ctx_ptr as usize,
        options(nostack)
    );
}
```

**Provenance:** `boot_ctx_ptr = boot_thread.context_ptr()`. Each secondary core
gets its own `Thread::new_boot()`, stored in `s.cores[idx].current`.

**Invariant holds:** TPIDR_EL1 = address of this core's boot thread Context.
The scheduler lock is held (IRQs masked), so no timer IRQ can fire during setup.
After this returns, `secondary_main` enables the timer and enters the idle loop.
The idle loop executes `wfe` — when the first timer IRQ arrives, `save_context`
reads TPIDR_EL1 and correctly saves to this core's boot thread Context.

---

### Write 3: `schedule_inner()` — Context switch (Fix 17, primary write)

**File:** scheduler.rs:412–429
**When:** Every context switch (timer IRQ, syscall yield/block/exit, user fault).
**IRQ state:** Disabled (scheduler lock held by `STATE.lock()`).

```rust
// Update TPIDR_EL1 to point at the new thread's Context while the
// scheduler lock is held and IRQs are masked. This is critical: when the
// lock drops, IRQs are re-enabled. If a timer IRQ fires before the caller
// (exception.S) updates TPIDR, save_context would write to the OLD
// thread's Context — which has been parked in the ready queue. [...]
//
// SAFETY: `result` is a valid Context pointer from context_ptr() (stable
// heap address). TPIDR_EL1 is always valid to write at EL1. `nostack`
// is correct. No `nomem` — the compiler must not reorder this past the
// lock release (which restores DAIF and re-enables IRQs).
unsafe {
    core::arch::asm!(
        "msr tpidr_el1, {ctx}",
        ctx = in(reg) result,
        options(nostack),
    );
}
```

**Provenance:** `result` comes from one of three paths in `schedule_inner`:

1. `new_thread.context_ptr()` — EEVDF-selected thread from ready queue.
2. `old_thread.context_ptr()` — same thread continues (no switch).
3. `idle.context_ptr()` — idle thread fallback.

All three are `Box<Thread>` with stable heap addresses.

**Invariant holds:** TPIDR_EL1 is updated to the new thread BEFORE the scheduler
lock drops. The lock's `Drop` impl restores DAIF, re-enabling IRQs. Because the
write happens inside the lock, there is no window where a timer IRQ could read a
stale TPIDR_EL1.

**Why no `nomem`:** The asm block omits `nomem` intentionally. This prevents LLVM
from reordering the `msr tpidr_el1` past the lock release (which is a DAIF restore
via `msr daifset`/`msr daifclr`). With `nomem`, LLVM could legally move the
TPIDR write after the lock drop, recreating the Fix 17 race.

**Fix 17 context (2026-03-14):** This write was added to fix an intermittent
EC=0x21 crash under SMP. Root cause: originally, TPIDR_EL1 was only updated by
exception.S after the Rust handler returned. But `schedule_inner` returns a
`*const Context`, and the `IrqMutex` guard drops (re-enabling IRQs) between
the Rust return and the exception.S `msr tpidr_el1`. A timer IRQ in that
~3-instruction window caused `save_context` to write kernel-mode registers
(SPSR=EL1h, ELR=kernel address) into the OLD thread's Context, corrupting it.

---

### Writes 4–6: `exception.S` defense-in-depth (redundant)

**File:** exception.S:347, 372, 390
**When:** After each Rust handler (irq_handler, svc_handler, user_fault_handler)
returns a non-null Context pointer.
**IRQ state:** Disabled (we're in the exception handler, between save_context and eret).

```asm
// TPIDR_EL1 already set by schedule_inner (before lock drop).
// Redundant write here for defense-in-depth.
msr tpidr_el1, x0
```

Three identical patterns:

| Site | Handler        | Line |
| ---- | -------------- | ---- |
| 4    | exc_irq        | 347  |
| 5    | exc_lower_sync | 372  |
| 6    | exc_user_fault | 390  |

**Provenance:** `x0` = return value from the Rust handler = `*const Context`.
This is the same pointer that `schedule_inner` already wrote to TPIDR_EL1
(Write 3). In the no-reschedule case (handler returns `ctx` unchanged), `x0`
equals the current TPIDR_EL1 value — the write is a no-op.

**Invariant holds:** These writes are always correct because `x0` is the
Context pointer the system is about to restore via `restore_context_and_eret`.
They are redundant with Write 3 but provide defense-in-depth: if a future
code change accidentally removed the Write 3 path, these writes would still
maintain the invariant.

**Defense-in-depth analysis:** These writes are sound even if `schedule_inner`
did NOT set TPIDR_EL1, because:

1. IRQs are disabled throughout exception handling (PSTATE.I = 1 on exception
   entry, restored by eret from SPSR).
2. The `msr tpidr_el1, x0` executes before `restore_context_and_eret`.
3. No nested exception can occur between the `msr` and `eret` (IRQs masked).
4. The next `save_context` (on the next exception) will read the correct value.

The only scenario where these writes would be the _sole_ correctness mechanism
is if a handler returns without calling `schedule_inner` (e.g., irq_handler
returns the current context without rescheduling). In that case, TPIDR_EL1
was already correct from the previous context switch, and the redundant write
is harmless.

---

## 3. All TPIDR_EL1 Read Sites (4 total)

### Read 1: `save_context` macro — Exception entry

**File:** exception.S:223
**When:** Every exception entry (IRQ, SVC, user fault).

```asm
mrs x9, tpidr_el1
stp x0, x1, [x9, #(CTX_X + 0*8)]
// ... saves all registers to [x9 + offset]
```

**Relies on:** TPIDR_EL1 being valid and pointing to the current thread's Context.
This is the critical consumer of the invariant.

**Why it's always valid:** At this point, the current thread is the one that was
running when the exception occurred. TPIDR_EL1 was set to this thread's Context
by one of Writes 1–6 before the thread last returned to execution (via eret).

---

### Read 2: `exc_fatal` — EL1 fault diagnostics

**File:** exception.S:193
**When:** EL1 synchronous/FIQ/SError exceptions (kernel faults — fatal).

```asm
mrs x8, tpidr_el1  // current thread Context pointer
// ... passed as x6 to kernel_fault_handler for diagnostics
```

**Pure diagnostic:** Value passed to `kernel_fault_handler` for panic output.
Not used for state mutation. Even if TPIDR_EL1 were corrupted (e.g., the fault
was caused by TPIDR corruption), the diagnostic read is harmless — the handler
validates the range before dereferencing (main.rs:541: `if tpidr >= 0xFFFF_0000_0000_0000`).

---

### Read 3: `handler_returned_null` — Fatal bug diagnostics

**File:** exception.S:402
**When:** A Rust handler returned null (bug — should never happen).

```asm
mrs x8, tpidr_el1
// ... printed as diagnostic
```

**Pure diagnostic:** Same analysis as Read 2.

---

### Read 4: `kernel_fault_handler` — Context state inspection

**File:** main.rs:541–555
**When:** During kernel fault handling, reads Context fields via TPIDR_EL1.

```rust
if tpidr >= 0xFFFF_0000_0000_0000 {
    let ctx_elr = unsafe { core::ptr::read_volatile((tpidr + 0x100) as *const u64) };
    let ctx_spsr = unsafe { core::ptr::read_volatile((tpidr + 0x108) as *const u64) };
    // ... more diagnostic reads
}
```

**Pure diagnostic:** Range-validated before dereference. Only reads, never writes.

---

## 4. Boot → Steady State Timeline

```text
Core 0:
  boot.S: _start
    │  TPIDR_EL1 = UNKNOWN (firmware-set, typically 0)
    │  No exceptions possible (early_vectors installed, but no devices/timers)
    ↓
  kernel_main:
    │  heap::init(), page_allocator::init(), interrupt_controller::init(), ...
    ↓
  scheduler::init()                              ← Write 1
    │  TPIDR_EL1 = &boot_thread.context
    │  IRQs still disabled
    ↓
  timer::init()
    │  Timer starts — IRQs will fire after this
    │  TPIDR_EL1 is valid → save_context will work correctly
    ↓
  boot_secondaries()
    │  PSCI CPU_ON for cores 1..n
    ↓
  [event loop / spawns processes]
    │  Timer IRQs → exc_irq → save_context (Read 1) → irq_handler →
    │  schedule_inner (Write 3) → exception.S defense-in-depth (Write 4) →
    │  restore_context_and_eret
    ↓
  Steady state: TPIDR_EL1 updated on every context switch (Write 3),
  reinforced by defense-in-depth (Writes 4–6) on every exception return.

Core N (1..7):
  boot.S: secondary_entry
    │  TPIDR_EL1 = UNKNOWN
    │  No exceptions possible (no timer yet)
    ↓
  secondary_main:
    │  interrupt_controller::init_cpu_interface()
    ↓
  scheduler::init_secondary(core_id)             ← Write 2
    │  TPIDR_EL1 = &secondary_boot_thread.context
    │  IRQs still disabled (scheduler lock held)
    ↓
  timer::init_secondary()
    │  Timer starts for this core
    │  TPIDR_EL1 is valid → save_context will work correctly
    ↓
  idle loop (wfe)
    │  Timer IRQ → exc_irq → save_context → schedule_inner → ...
    ↓
  Steady state (same as core 0)
```

---

## 5. Fix 17 Soundness Verification

### The Original Bug

**Symptom:** Intermittent EC=0x21 (instruction abort) under SMP, triggered by
rapid keyboard input.

**Root cause:** `schedule_inner` returned the new thread's Context pointer in x0.
Exception.S executed `msr tpidr_el1, x0` AFTER the Rust handler returned. But
the `IrqMutex` guard's `Drop` restored DAIF (re-enabling IRQs) before the Rust
function's epilogue completed. A pending timer IRQ could fire in the ~3-instruction
window between lock release and the `msr tpidr_el1`:

```text
schedule_inner:
  ... selects new_thread, stores result ...
  msr tpidr_el1, {ctx}          ← Fix 17: Write 3 (inside lock)
  ret                            ← IrqMutex guard Drop here: restores DAIF
                                   *** IRQ WINDOW OPENS ***
exception.S:
  cbz x0, handler_returned_null
  msr tpidr_el1, x0             ← Too late if IRQ fired above
```

In the old code (before Fix 17), if a timer IRQ fired in the window:

1. `save_context` reads stale TPIDR_EL1 (points to OLD thread's Context)
2. Saves kernel-mode state (SPSR=EL1h, ELR=kernel address, SP=kernel stack)
   into the OLD thread's Context
3. OLD thread's user-mode state is destroyed
4. When OLD thread is later restored, eret sees SPSR=EL1h + ELR in user range
   → EC=0x21 instruction abort

### The Fix

Move the `msr tpidr_el1` inside `schedule_inner`, under the scheduler lock, before
the lock drops. The key constraint: **no `nomem` option**, so LLVM cannot reorder
the write past the lock release.

### Why the Fix is Sound

1. **Timing:** The `msr tpidr_el1` executes while the scheduler lock is held.
   The lock masks IRQs (DAIF.I = 1). No IRQ can fire until the lock drops.

2. **Ordering:** Without `nomem`, LLVM treats the asm block as potentially
   accessing memory. The lock release (a store to the ticket spinlock's
   `now_serving` field + DAIF restore) is a memory operation. LLVM cannot
   reorder a potentially-memory-accessing asm block past a memory store.

3. **Completeness:** `schedule_inner` is the ONLY function that changes which
   thread is current. Every path through `schedule_inner` sets `result` to the
   new thread's Context pointer and writes it to TPIDR_EL1 at the end. There
   is no early return that skips the write.

4. **Defense-in-depth:** The exception.S writes (4–6) remain as a safety net.
   They are correct (x0 = scheduler's return value = same pointer written by
   Write 3) and harmless (redundant write of the same value).

5. **Stress test validation:** 3000-key stress test passes with 4 SMP cores.
   Previous crash was reproducible within ~30 seconds of rapid typing.

---

## 6. Invariant Violations — What Would Break

| Scenario                        | Symptom                                         | How prevented                                                                                          |
| ------------------------------- | ----------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| TPIDR_EL1 = 0 (null)            | Data abort on save_context (store to address 0) | Writes 1–2 before any timer; Write 3 on every switch                                                   |
| TPIDR_EL1 = stale (old thread)  | Old thread's Context corrupted with kernel regs | Fix 17: Write 3 under lock, no IRQ window                                                              |
| TPIDR_EL1 = freed memory        | Use-after-free on save_context                  | Box<Thread> guarantees stable address; deferred_drops ensures thread isn't freed while stack is in use |
| TPIDR_EL1 = wrong core's thread | Cross-core Context corruption                   | Each core's init sets its own TPIDR_EL1; schedule_inner runs per-core with core-local `current`        |
| Context not at offset 0         | All register offsets wrong, total corruption    | `#[repr(C)]` + first field; compile-time assertions in context.rs                                      |

---

## 7. No boot.S TPIDR_EL1 Writes

`boot.S` does NOT write TPIDR_EL1. This is correct because:

1. Core 0: TPIDR_EL1 is undefined at boot. No exceptions are possible until
   `scheduler::init()` sets it (Write 1) and `timer::init()` enables the timer.
   The early_vectors handler (boot.S) just prints '!' and hangs — it doesn't
   save context.

2. Secondary cores: TPIDR_EL1 is undefined when `secondary_entry` runs. No
   exceptions are possible until `scheduler::init_secondary()` sets it (Write 2)
   and `timer::init_secondary()` enables the timer.

The gap between boot and Write 1/2 is safe because no exception source is
enabled during that window.
