# kernel

Bare-metal aarch64 microkernel. 33 .rs files + 2 .S + link.ld, 28 syscalls, ~5000 lines.

## Build & Test

```sh
cd system && cargo build                              # Cross-compile for aarch64-unknown-none
cd system && cargo clippy                             # Must be zero warnings
cd system/test && cargo test -- --test-threads=1      # ~2,152 host-side tests
```

## Architecture

- **Boot:** `boot.S` → EL2→EL1 transition → MMU enable → `main.rs:kernel_main`
- **Memory:** Split TTBR (kernel TTBR1, user TTBR0), W^X enforcement, buddy allocator, slab+linked-list heap
- **Interrupts:** GICv3 interrupt controller, tickless idle with IPI cross-core wakeup
- **Scheduling:** Preemptive EEVDF on 4 SMP cores, handle-based scheduling contexts, tickless deadline reprogramming
- **IPC:** Kernel-managed channels (two shared pages per channel, SPSC ring buffers)
- **Processes:** Microkernel pattern — kernel spawns init, init spawns everything else
- **Panic:** Diagnostic output via UART → pvpanic MMIO write (0x0902_0000) → PSCI SYSTEM_OFF → spin loop. pvpanic signals the hypervisor to capture registers and write a crash report. Device discovered from DTB; address in `PVPANIC_ADDR` (AtomicUsize).

## Key Design Docs

- `DESIGN.md` — Rationale for every subsystem (~1517 lines)
- `LOCK-ORDERING.md` — All lock sites and acquisition order constraints
- `CROSS-MODULE-LIFETIMES.md` — Cross-file ownership invariants
- `AUDIT-MISSION.md` — Bug audit methodology and results

## Conventions

- Every `unsafe` block has a `// SAFETY:` comment
- Zero `#[allow(dead_code)]` except for test-only APIs (marked with comment)
- `clippy::all` must be clean (zero warnings)
- OOM fault injection available via `page_allocator::set_fail_after()`
- Tests live in `system/test/tests/`, not in-file — kernel can't link `#[test]` harness on `aarch64-unknown-none`
