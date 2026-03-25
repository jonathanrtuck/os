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

// --- Page geometry ---

/// Page size in bytes (16 KiB). Determines page table granule, IPC ring
/// buffer size, ELF segment alignment, and shared memory mapping granularity.
pub const PAGE_SIZE: u64 = 16384;

/// log2(PAGE_SIZE). Used in boot.S shift instructions and page table indexing.
pub const PAGE_SHIFT: u64 = 14;

// --- Physical memory ---

/// Start of RAM (QEMU virt machine). boot.S has `.equ RAM_START, 0x40000000`.
pub const RAM_START: u64 = 0x4000_0000;

// --- Kernel virtual address space ---

/// Offset added to physical addresses to produce kernel (TTBR1) virtual
/// addresses. T1SZ=28 => kernel VA starts at 0xFFFF_FFF0_0000_0000.
/// kernel/link.ld.in has `KERNEL_VA_OFFSET = @KERNEL_VA_OFFSET@`.
pub const KERNEL_VA_OFFSET: u64 = 0xFFFF_FFF0_0000_0000;

// --- User virtual address layout ---
// These define the fixed regions of every process's TTBR0 address space.
// Order: code | heap | DMA | device MMIO | channel SHM | stack gap | shared mem

/// Base VA for userspace ELF code. libraries/link.ld.in has `. = @USER_CODE_BASE@`.
pub const USER_CODE_BASE: u64 = 0x0000_0000_0040_0000; // 4 MiB

/// Base VA for IPC channel shared memory pages (1 GiB).
pub const CHANNEL_SHM_BASE: u64 = 0x0000_0000_4000_0000;

/// Top of user stack (stack grows downward from here) (2 GiB).
pub const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;

/// Number of pages allocated for each user stack (4 x 16 KiB = 64 KiB).
pub const USER_STACK_PAGES: u64 = 4;

/// Base VA for shared memory regions (memory_share syscall) (3 GiB).
pub const SHARED_MEMORY_BASE: u64 = 0x0000_0000_C000_0000;
