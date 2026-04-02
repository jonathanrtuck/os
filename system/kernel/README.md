# Kernel

A practical Rust microkernel for aarch64. Capabilities, VMOs, EEVDF scheduling, SMP work-stealing, KASLR.

~12,000 lines of Rust + assembly. 46 syscalls. 4 SMP cores. ~2,236 tests.

## Why This Kernel

seL4 wins on formal verification. Linux wins on hardware support. This kernel wins on **clarity** — Rust's type system + exhaustive testing + documented rationale for every decision. For the 99% of projects that aren't building avionics, "easy to understand, easy to verify, easy to build on" beats "formally proven but requires a PhD."

| Kernel | Strength               | Gap this kernel fills                |
| ------ | ---------------------- | ------------------------------------ |
| seL4   | Formally verified      | C, steep learning curve, limited SMP |
| Zircon | Production-grade       | C++, 200K+ lines, Google-coupled     |
| Redox  | Rust, active community | Unix-shaped (POSIX baggage)          |
| Linux  | Runs everywhere        | Monolithic, 30M+ lines               |

## Build

```sh
cargo build --release
```

Requires Rust nightly and the `aarch64-unknown-none` target (configured in `rust-toolchain.toml`). Produces a bootable ELF at `target/aarch64-unknown-none/release/kernel`.

## Boot

### QEMU

```sh
qemu-system-aarch64 \
    -machine virt,gic-version=3 \
    -cpu cortex-a53 \
    -smp 4 -m 256M \
    -nographic \
    -kernel target/aarch64-unknown-none/release/kernel
```

### Hypervisor (macOS, Apple Silicon)

```sh
hypervisor target/aarch64-unknown-none/release/kernel
```

The standalone kernel ships with a minimal stub init that exits immediately. To build a useful system, provide your own init (see [Building on the Kernel](#building-on-the-kernel)).

## Features

### Scheduling — EEVDF with SMP Work-Stealing

- **EEVDF** (Earliest Eligible Virtual Deadline First) — proportional-fair CPU sharing with latency differentiation
- **Per-core ready queues** with cache-affine thread placement (prefer last core)
- **Work-stealing** with EEVDF virtual lag normalization — fairness position preserved across migration
- **Workload-granularity migration** — steal by scheduling context group (novel: no other microkernel does this)
- **Concurrent Work Conservation (CWC)** — property-tested guarantee that no idle core coexists with an overloaded core
- **Scheduling contexts** — handle-based kernel objects (budget/period) for deadline-based CPU bandwidth allocation
- **Context donation** — borrow another thread's scheduling context to bill work correctly
- **Tickless idle** — timer reprogrammed to next deadline, IPI for cross-core wakeup

### Memory — VMOs with COW Snapshots

- **Virtual Memory Objects** — demand-paged, lazy-backed, content-typed (u64 tag)
- **COW snapshots** with bounded ring — generation-based snapshot/restore for undo
- **Sealing** — freeze a VMO immutably (PTE invalidation across all mappings)
- **User-level pagers** — attach a channel to a VMO; kernel forwards page faults to userspace
- **Split TTBR** — TTBR1 for kernel (shared), TTBR0 per-process (swapped on context switch)
- **W^X enforcement** — no page is both writable and executable
- **16 KiB page granule** — 2-level page tables (L2+L3, 64 GiB VA)

### Security

- **Capability-based access** — 10 named rights (READ, WRITE, SIGNAL, WAIT, MAP, TRANSFER, CREATE, KILL, SEAL, APPEND), monotonic attenuation
- **Per-process ASLR** — randomized heap, DMA, device, stack bases (~14 bits each)
- **KASLR** — 8-bit entropy, 32 MiB slide, PIE relocations processed at boot
- **PAC** (Pointer Authentication) — per-process keys loaded on context switch
- **Execute-only code pages** — prevents code disclosure attacks
- **Syscall filtering** — per-process bitmask to restrict available syscalls

### IPC

- **Channels** — bidirectional shared-memory ring buffers (64-byte slots, SPSC)
- **Events** — manual-reset signaling primitives
- **Futexes** — PA-keyed compare-and-sleep (works across shared memory)
- **Wait** — multiplex up to 16 handles with optional timeout

### Processes

- **ELF64 loading** — demand-paged segments, per-process address space
- **Handle table** — two-level (256 base + overflow pages, 4096 cap), Handle u16
- **Badges** — u64 per-handle for server-side demultiplexing
- **Handle transfer** — send handles between processes with rights attenuation

## Syscalls

46 syscalls covering threads, processes, handles, channels, VMOs, events, scheduling, timers, devices, and interrupts. Full reference: **[SYSCALLS.md](SYSCALLS.md)**.

| Range | Subsystem                                                      |
| ----- | -------------------------------------------------------------- |
| 0–2   | Thread lifecycle (exit, write, yield)                          |
| 3–6   | Handles (close, send, set/get badge)                           |
| 7–8   | Channels (create, signal)                                      |
| 9–11  | Synchronization (wait, futex_wait/wake)                        |
| 12–13 | Timers & clock (timer_create, clock_get)                       |
| 14–15 | Memory (alloc, free)                                           |
| 16–27 | VMOs (create, map, read/write, snapshot, restore, seal, pager) |
| 28–30 | Events (create, signal, reset)                                 |
| 31–34 | Processes (create, start, kill, syscall filter)                |
| 35–38 | Threads (create, suspend, resume, read state)                  |
| 39–42 | Scheduling contexts (create, borrow, return, bind)             |
| 43–45 | Devices & interrupts (map, register, ack)                      |

## Porting

The architecture abstraction is 13 modules, 48 functions. The generic kernel requires **zero changes** to port. See **[PORTING.md](PORTING.md)**.

## Testing

Tests live in a separate host-compiled crate (`../test/`):

```sh
cd ../test && cargo test -- --test-threads=1
```

~2,236 tests covering: handle table, ELF parser, buddy allocator, slab allocator, scheduler state machine, EEVDF fairness, work-stealing, CWC property, VMO operations, channel IPC, capability rights, pager protocol, ASLR entropy, KASLR relocations, and more.

## Building on the Kernel

The kernel spawns exactly one process: **init**. Init is an ELF binary embedded at build time. To build your own system:

1. Write an init program targeting `aarch64-unknown-none` using raw syscalls (ABI: `svc #0`, number in x8, args in x0–x5, result in x0)
2. Set `OS_INIT_ELF=/path/to/init.elf` before building
3. Optionally, pack additional ELFs into a service archive and set `OS_SERVICE_PACK=/path/to/services.o`

Minimal init (assembly):

```asm
.global _start
_start:
    mov x8, #1              // write
    adr x0, msg
    mov x1, #11
    svc #0
    mov x8, #0              // exit
    svc #0
msg:
    .ascii "init alive\n"
```

## Design Documents

- **[DESIGN.md](DESIGN.md)** — Rationale for every subsystem (~1,500 lines)
- **[SYSCALLS.md](SYSCALLS.md)** — Syscall reference (46 syscalls, manpage-style)
- **[PORTING.md](PORTING.md)** — Architecture porting guide (13 modules, 48 functions)
- **[LOCK-ORDERING.md](LOCK-ORDERING.md)** — Lock sites and acquisition order
- **[SAFETY-MAP.md](SAFETY-MAP.md)** — Cross-cutting safety invariants

## Source Structure

```text
kernel/
├── main.rs                  — kernel entry, IRQ/SVC dispatch, boot, init spawn
├── syscall.rs               — 46-syscall dispatcher and handlers
├── scheduler.rs             — SMP EEVDF, per-core queues, work-stealing
├── scheduling_algorithm.rs  — pure EEVDF math (vruntime, eligibility, deadline)
├── scheduling_context.rs    — budget/period accounting
├── process.rs               — process creation, address spaces, handle tables
├── thread.rs                — thread state machine (Ready/Running/Blocked/Exited)
├── channel.rs               — IPC channels (shared memory + signal/wait)
├── handle.rs                — capability handle table (two-level, rights attenuation)
├── vmo.rs                   — virtual memory objects (demand paging, COW, sealing)
├── pager.rs                 — user-level pager interface
├── event.rs                 — event objects (manual-reset signals)
├── futex.rs                 — PA-keyed futex wait table
├── executable.rs            — pure functional ELF64 parser
├── device_tree.rs           — FDT parser (discover hardware from DTB)
├── memory.rs                — TTBR1 refinement, PA/VA conversion, KASLR slide
├── address_space.rs         — per-process TTBR0 page tables, demand paging
├── address_space_id.rs      — 8-bit ASID allocator (generation-based recycling)
├── memory_region.rs         — virtual memory area tracking
├── paging.rs                — page table constants, memory layout
├── page_allocator.rs        — buddy allocator (16 KiB – 8 MiB)
├── heap.rs                  — linked-list allocator + slab routing
├── slab.rs                  — power-of-two slab caches (64 – 2048 bytes)
├── sync.rs                  — IrqMutex (ticket spinlock + IRQ masking)
├── interrupt.rs             — IRQ forwarding to userspace handles
├── random.rs                — ChaCha20 PRNG with fast key erasure
├── aslr.rs                  — per-process address space randomization
├── relocate.rs              — KASLR relocation support
├── metrics.rs               — atomic counters (syscalls, faults, switches)
├── process_exit.rs          — process exit notification
├── thread_exit.rs           — thread exit notification
├── waitable.rs              — generic WaitableRegistry<Id>
├── arch/
│   ├── mod.rs               — architecture abstraction (compile-time dispatch)
│   └── aarch64/
│       ├── boot.S           — entry, EL2→EL1, MMU enable, KASLR relocation
│       ├── exception.S      — exception vectors, context save/restore
│       ├── context.rs       — CPU register state (x0–x30, SP, ELR, SPSR, NEON)
│       ├── cpu.rs           — barriers, idle, diagnostics
│       ├── interrupts.rs    — IRQ mask/restore (IrqState)
│       ├── interrupt_controller.rs — GICv3 (distributor, redistributor, IPI)
│       ├── mmu.rs           — TLB invalidation, address translation
│       ├── per_core.rs      — MPIDR core identity
│       ├── power.rs         — PSCI (cpu_on, system_off)
│       ├── scheduler.rs     — TPIDR_EL1, TTBR0 switch
│       ├── serial.rs        — PL011 UART (locked + panic-safe)
│       ├── timer.rs         — ARM virtual timer (CNTV_*)
│       ├── entropy.rs       — RNDR/RNDRRS + jitter extraction
│       ├── security.rs      — PAC keys, BTI detection
│       └── memory_mapped_io.rs — volatile MMIO (HVF-compatible)
├── system_config.rs         — 9 root constants (PAGE_SIZE, VA layout, etc.)
├── link.ld.in               — linker script template
├── build.rs                 — linker script generation, PIE flags, init stub
├── Cargo.toml               — standalone package
└── DESIGN.md                — subsystem rationale (~1,500 lines)
```

## Audit

Every `unsafe` block in the kernel has a `// SAFETY:` comment explaining the invariant. The kernel has been through multiple comprehensive audits:

- Every `.rs` and `.S` file reviewed against a 6-category checklist
- Cross-file analyses: [LOCK-ORDERING.md](LOCK-ORDERING.md) (13 lock sites, no circular dependencies) and [SAFETY-MAP.md](SAFETY-MAP.md) (TPIDR chain, handle lifecycle, process exit, ASID, allocator routing)
- `nomem` audit on all inline asm (ARM architecture manual verification for each instruction)
- OOM fault injection testing via `page_allocator::set_fail_after()`

## References

- EEVDF: adapted from [Linux 6.6+](https://lwn.net/Articles/925371/) (Zijlstra, 2023). Original algorithm: [Stoica & Abdel-Wahab, 1996](https://doi.org/10.1109/real.1996.563725). Per-core queue decomposition and vlag normalization integrated with scheduling contexts.
- CWC property testing: inspired by [Ipanema](https://doi.org/10.1145/3342195.3387544) (Lepers et al., EuroSys 2020), which proved Linux CFS violates CWC.
- ChaCha20 PRNG: [RFC 8439](https://datatracker.ietf.org/doc/html/rfc8439) test vectors. Fast key erasure: [Bernstein 2017](https://blog.cr.yp.to/20170723-random.html).

## license

[Unlicense](UNLICENSE) — public domain.
