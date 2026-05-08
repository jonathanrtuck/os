//! Kernel-level configuration constants.
//!
//! Policy decisions and capacity limits that are independent of the target
//! architecture. Platform-specific values (device addresses, RAM layout)
//! live in `frame/arch/aarch64/platform.rs`.

/// Kernel stack size per core. — link.ld sync: `.bss.stack`
pub const KERNEL_STACK_SIZE: usize = 64 * 1024;

/// Maximum number of CPU cores supported by this kernel build.
///
/// Compile-time upper bound for per-core array sizing (stacks, per-CPU
/// data). Must be >= the actual core count discovered from the DTB at
/// runtime. The current value targets QEMU virt / Apple HVF configurations.
pub const MAX_CORES: usize = 8;

/// Page size — 16 KiB (Apple Silicon native granule).
pub const PAGE_SIZE: usize = 16 * 1024;

/// Maximum supported physical memory: 48 GiB.
pub const MAX_PHYS_MEM: usize = 48 * 1024 * 1024 * 1024;

/// Maximum physical pages (MAX_PHYS_MEM / PAGE_SIZE).
pub const MAX_PHYS_PAGES: usize = MAX_PHYS_MEM / PAGE_SIZE;

/// Bitmap words needed for page allocator (MAX_PHYS_PAGES / 64).
pub const BITMAP_WORDS: usize = MAX_PHYS_PAGES / 64;

// Capacity limits derived from workload analysis:
// Target: 50 open documents, 10 services, 14-core M4 Pro
//
// MAX_VMOS (4096): ~423 VMOs at peak (50 docs × 5 undo + decoded +
//   scene graph + stacks + IPC buffers). 4096 = ~10x headroom.
// MAX_HANDLES (512): OS service at peak holds ~387 handles (50 docs ×
//   5 undo snapshots + endpoints + events). 512 = ~1.3x headroom.
//   Undo ring depth is the primary driver.
// MAX_THREADS (512): ~48 threads at peak (10 services + 4 workers +
//   14 idle + 10 decoder workers + spare). 512 = ~10x headroom.
// MAX_EVENTS (1024): ~50 events at peak. 1024 = ~20x headroom.
// MAX_ENDPOINTS (256): ~15 endpoints at peak. 256 = ~17x headroom.
// MAX_ADDRESS_SPACES (128): ~12 spaces at peak. 128 = ~10x headroom.
// MAX_PAGES_INLINE (32): covers VMOs up to 512 KiB (32 × 16 KiB).
//   Most documents are < 100 KiB. Overflow to heap for larger VMOs.
// MAX_VA_REGIONS (MAX_MAPPINGS + 1): per-space VA free list. With N
//   active mappings, at most N+1 free regions exist (one between each
//   pair of mappings, plus endpoints). Sized to the worst case.
// MAX_MAPPINGS (128): per-space mapping records. Covers OS service
//   worst case (50 docs + undo snapshots + scene graph + stacks).
// MAX_WAITERS_PER_EVENT (16): concurrent waiters on one event.
//   Typical: 1 (compositor on scene-ready). 16 = generous headroom.
// MAX_PENDING_PER_ENDPOINT (8): concurrent callers blocked on one
//   endpoint. Typical: 1-3 editors calling OS service. 8 = headroom.
// MAX_IPC_HANDLES (4): handles transferred per IPC call. Typical
//   calls transfer 0-2 handles (VMO + maybe event). 4 = headroom.
// MAX_BOOTSTRAP_HANDLES (8): handles passed via thread_create_in.
//   Includes code VMO, stack VMO, and service-specific caps. Drivers
//   need up to 5 (code, stack, name svc, device VMO, init ep).

pub const MAX_VMOS: usize = 4096;
pub const MAX_HANDLES: usize = 512;
pub const MAX_ADDRESS_SPACES: usize = 128;
pub const MAX_THREADS: usize = 512;
pub const MAX_EVENTS: usize = 1024;
pub const MAX_ENDPOINTS: usize = 256;
pub const MAX_RESOURCES: usize = 4;
pub const MAX_PAGES_INLINE: usize = 32;
pub const MAX_MAPPINGS: usize = 128;
pub const MAX_VA_REGIONS: usize = MAX_MAPPINGS * 2 + 1;
pub const MAX_WAITERS_PER_EVENT: usize = 16;
pub const MAX_PENDING_PER_ENDPOINT: usize = 8;
pub const MAX_RECV_WAITERS: usize = 4;
pub const MAX_MULTI_WAIT: usize = 32;
pub const MAX_IPC_HANDLES: usize = 4;
pub const MAX_BOOTSTRAP_HANDLES: usize = 8;
pub const KERNEL_STACK_PAGES: usize = 2;

/// Kernel stack size per thread (2 pages = 32 KiB).
/// Must hold: TrapFrame (832 bytes) + Rust call stack for deepest syscall path.
pub const THREAD_KERNEL_STACK_SIZE: usize = KERNEL_STACK_PAGES * PAGE_SIZE;

// IRQ binding table capacity. GICv3 supports INTIDs 0-1023.
// SGIs (0-15) and PPIs (16-31) are kernel-internal; SPIs (32-1019) are
// bindable to userspace events. 1024 covers the full INTID range.
pub const MAX_IRQS: usize = 1024;
