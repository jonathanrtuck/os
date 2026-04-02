// System-wide constants — single source of truth.
//
// This file is `include!`'d by kernel, userspace libraries, and tests.
// build.rs also includes it to generate linker scripts and linker flags.
//
// RULES:
// - All types are `u64` (consumers cast to `usize` where needed).
// - Only root constants belong here — derived constants stay in their module.
// - Changing a value here rebuilds everything. That is the point.
// - boot.S has manual copies of PAGE_SIZE and RAM_START — the kernel
//   enforces consistency via `const_assert!` (see kernel/paging.rs).

/// Base VA for IPC channel shared memory pages (1 GiB).
pub const CHANNEL_SHM_BASE: u64 = 0x0000_0000_4000_0000;
/// Default scheduling context budget (ns). CPU time granted per period.
pub const DEFAULT_BUDGET_NS: u64 = 50_000_000; // 50 ms
/// Default scheduling context period (ns). Budget replenished every period.
pub const DEFAULT_PERIOD_NS: u64 = 50_000_000; // 50 ms
/// EEVDF time slice (ns). Shorter = lower latency, more context switches.
pub const DEFAULT_SLICE_NS: u64 = 4_000_000; // 4 ms
/// EEVDF default weight. Higher-weight threads get proportionally more CPU.
pub const DEFAULT_WEIGHT: u64 = 1024;
/// Futex hash table bucket count. Power of two for fast modular hashing.
pub const FUTEX_BUCKET_COUNT: u64 = 64;
/// Inline handle table slots (first level, no allocation).
pub const HANDLE_TABLE_BASE_SIZE: u64 = 256;
/// Kernel heap size in bytes. Backs the slab allocator and linked-list heap.
pub const HEAP_SIZE: u64 = 16 * 1024 * 1024; // 16 MiB
/// Per-thread kernel stack size in bytes.
pub const KERNEL_STACK_SIZE: u64 = 16 * 1024; // 16 KiB
/// Offset added to physical addresses to produce kernel (TTBR1) virtual
/// addresses. T1SZ=28 => kernel VA starts at 0xFFFF_FFF0_0000_0000.
/// kernel/link.ld.in has `KERNEL_VA_OFFSET = @KERNEL_VA_OFFSET@`.
pub const KERNEL_VA_OFFSET: u64 = 0xFFFF_FFF0_0000_0000;
/// Maximum SMP cores. Determines per-core data structure sizing.
/// Actual core count discovered from DTB; this is the compile-time upper bound.
pub const MAX_CORES: u64 = 8;
/// Maximum handles per process. Two-level table: BASE inline + overflow pages.
/// 4096 handles × 16 bytes/entry = 64 KiB worst-case per process.
pub const MAX_HANDLES: u64 = 4096;
/// Maximum concurrent interrupt handles (global).
pub const MAX_INTERRUPTS: u64 = 32;
/// Maximum concurrent timer objects (global).
pub const MAX_TIMERS: u64 = 32;
/// Maximum handles in a single `wait()` syscall.
pub const MAX_WAIT_HANDLES: u64 = 16;
/// log2(PAGE_SIZE). Used in boot.S shift instructions and page table indexing.
pub const PAGE_SHIFT: u64 = 14;
/// Page size in bytes (16 KiB). Determines page table granule, IPC ring
/// buffer size, ELF segment alignment, and shared memory mapping granularity.
pub const PAGE_SIZE: u64 = 16384;
/// Maximum RAM the kernel can manage. Determines buddy allocator bitmap size
/// and per-process allocation limits. QEMU virt default: 256 MiB.
pub const RAM_SIZE_MAX: u64 = 256 * 1024 * 1024;
/// Start of RAM (QEMU virt machine). boot.S has `.equ RAM_START, 0x40000000`.
pub const RAM_START: u64 = 0x4000_0000;
/// Base VA for the service pack mapped into init's address space (512 MiB).
/// Read-only. Between USER_CODE_BASE (4 MiB) and CHANNEL_SHM_BASE (1 GiB).
pub const SERVICE_PACK_BASE: u64 = 0x0000_0000_2000_0000;
/// Base VA for shared memory regions (memory_share syscall) (3 GiB).
pub const SHARED_MEMORY_BASE: u64 = 0x0000_0000_C000_0000;
/// Base VA for userspace ELF code. libraries/link.ld.in has `. = @USER_CODE_BASE@`.
pub const USER_CODE_BASE: u64 = 0x0000_0000_0040_0000; // 4 MiB
/// Number of pages allocated for each user stack (4 x 16 KiB = 64 KiB).
pub const USER_STACK_PAGES: u64 = 4;
/// Top of user stack (stack grows downward from here) (2 GiB).
pub const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
