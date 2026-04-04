# Architecture Porting Guide

This document describes what you need to implement to port the kernel to a new CPU architecture.

The aarch64 implementation (1,696 lines across 13 modules) is the reference. All architecture-specific code lives under `arch/<arch>/`. The generic kernel calls into arch via `pub use arch::*` — compile-time dispatch, zero overhead.

## Overview

| Module               | Functions        | Purpose                                |
| -------------------- | ---------------- | -------------------------------------- |
| context              | 12 methods       | CPU register save/restore              |
| cpu                  | 7                | Barriers, idle, diagnostics            |
| interrupts           | 2                | IRQ mask/restore                       |
| interrupt_controller | 7 trait + 2 free | Interrupt controller (GIC, APIC, PLIC) |
| mmu                  | 7                | TLB invalidation, address translation  |
| per_core             | 1                | Core identity                          |
| power                | 2                | SMP boot, shutdown                     |
| scheduler            | 2                | Context switch, address space switch   |
| serial               | 7                | Debug UART                             |
| timer                | 6                | Deadline timer, counter                |
| entropy              | 5                | Hardware RNG, jitter                   |
| security             | 5                | PAC/BTI (or equivalent CFI)            |
| memory_mapped_io     | 3                | Volatile device access                 |

**Total: 48 public functions + 3 types (Context, PacKeys, IrqState).**

---

## 1. context — CPU Register State

The `Context` struct holds the full register file for one thread. It is `#[repr(C)]` and embedded at offset 0 of every thread's kernel stack. Exception entry assembly saves registers into it; exception return restores from it.

### Struct: Context

Define a `#[repr(C)]` struct containing all registers that must be saved across exceptions. On aarch64, this is 0x330 bytes: 31 GPRs, SP, ELR, SPSR, SP_EL0, TPIDR_EL0, 32 NEON/FP registers, FPCR, FPSR.

On x86_64 this would be: RAX–R15, RIP, RFLAGS, RSP, FS_BASE (TLS), XMM0–15 (or full AVX state via XSAVE).

### Required Methods

| Method                              | Signature                            | Purpose                                                                                            |
| ----------------------------------- | ------------------------------------ | -------------------------------------------------------------------------------------------------- |
| `new()`                             | `pub const fn new() -> Self`         | Zero-initialized context. Must be const.                                                           |
| `pc()`                              | `pub fn pc(&self) -> u64`            | Read program counter (ELR on ARM, RIP on x86).                                                     |
| `set_pc(&mut self, pc: u64)`        |                                      | Set program counter.                                                                               |
| `sp()`                              | `pub fn sp(&self) -> u64`            | Read kernel stack pointer.                                                                         |
| `set_sp(&mut self, sp: u64)`        |                                      | Set kernel stack pointer.                                                                          |
| `user_sp()`                         | `pub fn user_sp(&self) -> u64`       | Read user stack pointer (SP_EL0 on ARM, RSP from user on x86).                                     |
| `set_user_sp(&mut self, sp: u64)`   |                                      | Set user stack pointer.                                                                            |
| `arg(n: usize)`                     | `pub fn arg(&self, n: usize) -> u64` | Read syscall argument N (0–5). Panic if n >= 6.                                                    |
| `set_arg(n: usize, val: u64)`       |                                      | Set argument register N.                                                                           |
| `set_user_mode(&mut self)`          |                                      | Configure for user-mode return. On ARM: clear SPSR\[3:0\]. On x86: set CS/SS selectors for ring 3. |
| `user_tls()`                        | `pub fn user_tls(&self) -> u64`      | Read user TLS pointer (TPIDR_EL0 on ARM, FS_BASE on x86).                                          |
| `set_user_tls(&mut self, tls: u64)` |                                      | Set user TLS pointer.                                                                              |

### Invariants

- Must be `const fn new()` (used for static initialization).
- Must be `#[repr(C)]` with documented field offsets — exception entry assembly writes to these offsets.
- `arg(n)` must panic for n >= 6 (the kernel never uses more than 6 arguments).

---

## 2. cpu — Barriers & Diagnostics

Pure operations — no state, no initialization.

| Function               | Purpose                                      | aarch64      | x86_64 equivalent         |
| ---------------------- | -------------------------------------------- | ------------ | ------------------------- |
| `dsb_ish()`            | Inner-shareable data synchronization barrier | `dsb ish`    | `mfence`                  |
| `wait_for_interrupt()` | Halt until IRQ                               | `wfi`        | `hlt`                     |
| `read_esr() -> u64`    | Exception syndrome (fault cause)             | ESR_EL1      | Error code from IDT frame |
| `read_far() -> u64`    | Fault address                                | FAR_EL1      | CR2                       |
| `read_elr() -> u64`    | Exception link (faulting PC)                 | ELR_EL1      | RIP from IDT frame        |
| `read_sp() -> u64`     | Current stack pointer (diagnostics)          | `mov x0, sp` | `mov rax, rsp`            |
| `read_tpidr() -> u64`  | Current thread pointer (kernel TLS)          | TPIDR_EL1    | GS_BASE (via SWAPGS)      |

### Invariant

`read_tpidr()` must NOT use `nomem`/`pure` — the value changes when the scheduler updates it, and the compiler must not cache or reorder reads.

---

## 3. interrupts — IRQ Masking

Two functions and one opaque type. Used by every spinlock.

| Function                 | Purpose                      | aarch64                      | x86_64                                |
| ------------------------ | ---------------------------- | ---------------------------- | ------------------------------------- |
| `mask_all() -> IrqState` | Save IRQ state, disable IRQs | Read DAIF, `msr daifset, #2` | `pushfq; cli` (save RFLAGS, clear IF) |
| `restore(IrqState)`      | Restore saved IRQ state      | `msr DAIF, saved`            | `popfq` or conditional `sti`          |

### IrqState

Opaque `#[repr(transparent)]` newtype. Must implement `Copy`. On aarch64 it wraps the saved DAIF register value. On x86_64 it would wrap the saved RFLAGS.

### Critical Invariant

**No `nomem` on the inline asm.** The compiler must not reorder memory operations past the IRQ mask/unmask boundary. This is the most common source of subtle SMP bugs — if LLVM hoists a load above `mask_all()`, the load executes with IRQs enabled and the "critical section" is illusory.

---

## 4. interrupt_controller — Platform Interrupt Controller

The most substantial arch module. Implements a trait with 7 methods.

| Method                               | Purpose                                                                                                  |
| ------------------------------------ | -------------------------------------------------------------------------------------------------------- |
| `init_distributor(&self)`            | One-time global setup (core 0 only). Configure all interrupt priorities, routing, enable the controller. |
| `init_per_core(&self, core_id: u32)` | Per-core setup. Enable the CPU interface, set priority mask, wake the redistributor, enable SGIs/PPIs.   |
| `acknowledge(&self) -> Option<u32>`  | Acknowledge an interrupt. Returns the IRQ ID, or None if spurious (ID 1023 on GICv3).                    |
| `end_of_interrupt(&self, irq: u32)`  | Signal EOI. Drops running priority, deactivates the interrupt. Must pair with `acknowledge`.             |
| `enable_irq(&self, irq: u32)`        | Unmask a specific IRQ.                                                                                   |
| `disable_irq(&self, irq: u32)`       | Mask a specific IRQ.                                                                                     |
| `send_ipi(&self, target_core: u32)`  | Send an inter-processor interrupt (SGI) to wake an idle core.                                            |

Plus two free functions:

| Function                                           | Purpose                                                |
| -------------------------------------------------- | ------------------------------------------------------ |
| `init()`                                           | Convenience: `init_distributor()` + `init_per_core(0)` |
| `set_base_addresses(dist_pa: u64, redist_pa: u64)` | Override defaults with addresses from DTB/ACPI         |

### Platform Mapping

| Platform        | Controller                             | IPI Mechanism           |
| --------------- | -------------------------------------- | ----------------------- |
| ARM + QEMU virt | GICv3 (GICD + GICR + system registers) | SGI 0 via ICC_SGI1R_EL1 |
| x86_64          | Local APIC + I/O APIC                  | IPI via ICR write       |
| RISC-V          | PLIC + CLINT/ACLINT                    | IPI via MSIP register   |

---

## 5. mmu — TLB & Address Translation

TLB invalidation primitives and hardware address translation checks. The kernel's page table code is generic — arch only provides the TLB flush and AT instructions.

| Function                            | Purpose                                          | aarch64              | x86_64                                     |
| ----------------------------------- | ------------------------------------------------ | -------------------- | ------------------------------------------ |
| `tlbi_page(va, asid)`               | Invalidate one page, one ASID, broadcast         | TLBI VALE1IS         | INVLPG (no ASID on x86; INVPCID with PCID) |
| `tlbi_asid(asid)`                   | Invalidate all pages for one ASID                | TLBI ASIDE1IS        | Flush entire TLB (or INVPCID type 1)       |
| `tlbi_bbm(va, asid)`                | Break-before-make: invalidate before overwrite   | TLBI + DSB (no ISB)  | INVLPG                                     |
| `tlbi_all()`                        | Invalidate everything, all ASIDs, broadcast      | TLBI VMALLE1IS       | MOV CR3, CR3 (flush all)                   |
| `tlbi_all_local()`                  | Invalidate everything, local core only           | TLBI VMALLE1 (no IS) | MOV CR3, CR3                               |
| `is_user_page_writable(va) -> bool` | Check if EL0 can write to this VA                | AT S1E0W + PAR_EL1   | Walk page tables in software               |
| `translate_user_read(va) -> u64`    | Check if EL0 can read; return translation result | AT S1E0R + PAR_EL1   | Walk page tables in software               |

### Break-Before-Make

ARM requires that you invalidate the old TLB entry BEFORE writing a new descriptor that changes the mapping. Writing a new descriptor while the old TLB entry is still live is architecturally UNPREDICTABLE. `tlbi_bbm` does the invalidation without the final ISB — the caller writes the new descriptor, then a subsequent barrier sequence makes it visible.

x86 does not have this constraint (descriptors can be overwritten freely), but `tlbi_bbm` should still flush the old mapping.

---

## 6. per_core — Core Identity

One function.

```rust
pub fn core_id() -> u32
```

Returns the current core's numeric ID (0 for the boot core). On aarch64: MPIDR_EL1 bits \[7:0\]. On x86_64: Local APIC ID. On RISC-V: `mhartid` CSR.

---

## 7. power — Boot & Shutdown

| Function                                        | Purpose               | aarch64                 | x86_64                           |
| ----------------------------------------------- | --------------------- | ----------------------- | -------------------------------- |
| `cpu_on(target, entry, ctx) -> Result<(), i64>` | Boot a secondary core | PSCI CPU_ON via HVC     | INIT-SIPI-SIPI sequence via APIC |
| `system_off() -> !`                             | Power off the system  | PSCI SYSTEM_OFF via HVC | ACPI S5 or triple fault          |

### cpu_on

- `target`: platform core ID (MPIDR on ARM, APIC ID on x86)
- `entry`: physical address of the entry trampoline
- `ctx`: value passed to the entry point (core ID on ARM, unused on x86)

Must return `Ok(())` if the core was already on.

---

## 8. scheduler — Context Switch Primitives

Two `unsafe` functions called by the generic scheduler during thread switches.

| Function                                    | Purpose                                             | aarch64                             | x86_64                               |
| ------------------------------------------- | --------------------------------------------------- | ----------------------------------- | ------------------------------------ |
| `set_current_thread(ctx_ptr: usize)`        | Store current thread pointer in kernel TLS register | MSR TPIDR_EL1                       | WRGSBASE or write to GS_BASE MSR     |
| `switch_address_space(old_asid, new_ttbr0)` | Switch user page tables with TLB flush              | TLBI old ASID + MSR TTBR0_EL1 + ISB | MOV CR3 (with PCID in bits \[11:0\]) |

### Critical Invariant

`set_current_thread` must NOT use `nomem`. The compiler must not reorder memory accesses past this write — the value determines which thread's data the kernel is accessing.

---

## 9. serial — Debug Console

Seven functions split into two categories: locked (SMP-safe, for normal operation) and lockless (for panic handlers, where the lock may be held by the faulting core).

| Function                  | Locked? | Purpose                  |
| ------------------------- | ------- | ------------------------ |
| `puts(s: &str)`           | Yes     | Write string             |
| `write_bytes(buf: &[u8])` | Yes     | Write bytes atomically   |
| `put_u32(n: u32)`         | Yes     | Write decimal number     |
| `panic_puts(s: &str)`     | No      | Panic-safe string write  |
| `panic_putc(c: u8)`       | No      | Panic-safe byte write    |
| `panic_put_hex(v: u64)`   | No      | Panic-safe hex write     |
| `panic_put_u32(n: u32)`   | No      | Panic-safe decimal write |

The locked functions use an `IrqMutex` to prevent interleaved output from multiple cores. The panic functions bypass the lock entirely — correctness is less important than getting diagnostic output.

Platform mapping: PL011 on ARM QEMU, 8250/16550 on x86, SBI console on RISC-V.

---

## 10. timer — Deadline Timer & Counter

| Function                  | Purpose                             | aarch64                  | x86_64                                |
| ------------------------- | ----------------------------------- | ------------------------ | ------------------------------------- |
| `program_tval(tval: u64)` | Set timer countdown (ticks)         | MSR CNTV_TVAL_EL0        | Program APIC timer or HPET comparator |
| `counter() -> u64`        | Read hardware counter               | MRS CNTVCT_EL0           | RDTSC or HPET main counter            |
| `read_frequency() -> u64` | Counter frequency in Hz             | MRS CNTFRQ_EL0           | Calibrate from PIT/HPET               |
| `enable_el0_counter()`    | Allow userspace to read the counter | Set CNTKCTL_EL1.EL0VCTEN | Map HPET or expose TSC via VDSO       |
| `enable_virtual_timer()`  | Enable timer interrupts             | MSR CNTV_CTL_EL0         | Enable APIC timer                     |
| `unmask_irqs()`           | Final global IRQ unmask             | MSR DAIFCLR #2           | `sti`                                 |

### Key Detail

`counter()` must NOT use `nomem` — repeated reads must return different values. The compiler must not CSE (common subexpression eliminate) counter reads.

`read_frequency()` CAN use `nomem` — the frequency is set by firmware at boot and never changes.

---

## 11. entropy — Hardware RNG

| Function                                    | Purpose                             | aarch64                      | x86_64                      |
| ------------------------------------------- | ----------------------------------- | ---------------------------- | --------------------------- |
| `has_hardware_rng() -> bool`                | Check for hardware RNG              | FEAT_RNG (ID_AA64ISAR0_EL1)  | CPUID for RDRAND/RDSEED     |
| `hardware_random() -> Option<u64>`          | Read 64 random bits                 | MRS RNDR                     | RDRAND (retry on CF=0)      |
| `hardware_random_reseeded() -> Option<u64>` | Read 64 bits with guaranteed reseed | MRS RNDRRS                   | RDSEED                      |
| `collect_jitter(scratch) -> [u8; 8]`        | Extract entropy from timing jitter  | Memory access + CNTVCT delta | Memory access + RDTSC delta |
| `timing_counter() -> u64`                   | High-resolution counter for jitter  | MRS CNTVCT_EL0               | RDTSC                       |

If the platform has no hardware RNG (`has_hardware_rng()` returns false), the kernel falls back to jitter-only entropy. The PRNG seeds from whatever entropy is available.

---

## 12. security — Control Flow Integrity

| Function                          | Purpose                         | aarch64                                  | x86_64                                            |
| --------------------------------- | ------------------------------- | ---------------------------------------- | ------------------------------------------------- |
| `PacKeys::generate(prng) -> Self` | Generate per-process keys       | 5 × 128-bit keys                         | No-op on x86 (or generate CET shadow stack token) |
| `PacKeys::zero() -> Self`         | Zero keys (feature unavailable) | All zeros                                | All zeros                                         |
| `set_pac_keys(keys: &PacKeys)`    | Load keys on context switch     | Write APIA/APDA/APIB/APDB/APGA registers | No-op on x86 (or configure CET)                   |

The security module is **optional** in the sense that returning `false` from all feature checks and providing no-op implementations is valid. The kernel adapts — when PAC is unavailable, `Process::new` calls `PacKeys::zero()` and `set_pac_keys` writes zeros.

---

## 13. memory_mapped_io — Volatile Device Access

| Function                         | Purpose           | Why inline asm?                                                            |
| -------------------------------- | ----------------- | -------------------------------------------------------------------------- |
| `read32(addr: usize) -> u32`     | Read 32-bit MMIO  | Prevents LLVM from emitting addressing modes that hypervisors can't decode |
| `write8(addr: usize, val: u8)`   | Write 8-bit MMIO  |                                                                            |
| `write32(addr: usize, val: u32)` | Write 32-bit MMIO |                                                                            |

On aarch64, these use plain `ldr`/`str`/`strb` (no writeback, no pair instructions). The reason: Apple's HVF and QEMU's KVM trap MMIO via stage-2 faults, and they need the ISV (Instruction Syndrome Valid) bit to decode the access. Complex addressing modes don't provide ISV.

On x86_64, use `in`/`out` for port I/O or plain `mov` for MMIO. The hypervisor constraint doesn't apply (x86 MMIO trapping uses different mechanisms).

---

## Assembly Files

In addition to the Rust modules, each architecture needs two assembly files:

### boot.S — Entry Point

Responsibilities:

1. Set up exception level (EL2→EL1 on ARM, long mode on x86)
2. Zero BSS
3. Set up initial page tables (identity map + kernel VA map)
4. Enable the MMU
5. Process KASLR relocations (.rela.dyn section, R_AARCH64_RELATIVE entries)
6. Jump to `kernel_main` at kernel VA
7. Secondary core entry trampoline (waits for primary, then jumps to `secondary_main`)

### exception.S — Exception/Interrupt Vector

Responsibilities:

1. Save full register context into the Context struct at TPIDR_EL1
2. Call the appropriate Rust handler (`kernel_exception_handler` or `kernel_irq_handler`)
3. On return: validate context, restore registers, return to user/kernel mode

The assembly must save registers at the exact offsets defined by the Context struct. Use compile-time assertions to verify offset consistency between Rust and assembly.

---

## Checklist

When porting to a new architecture:

1. Create `arch/<arch>/mod.rs` re-exporting all submodules
2. Add `#[cfg(target_arch = "<arch>")]` to `arch/mod.rs`
3. Implement all 13 modules (48 functions + 3 types)
4. Write `boot.S` and `exception.S`
5. Create a linker script (`link.ld.in`) for the new memory layout
6. Update `.cargo/config.toml` with the new target triple
7. Run the test suite — all ~2,236 tests must pass
8. Verify `cargo doc` builds cleanly

The generic kernel (scheduler, channels, handle table, VMOs, process model, ELF loader, allocators, futex) requires **zero changes**. If you find yourself modifying generic code, you've likely found a missing abstraction — add it to the arch interface instead.
