// Userspace syscall wrappers.
//
// Provides safe Rust functions for each kernel syscall. Compiled as an `rlib`
// and linked into user binaries by the kernel's build.rs.
//
// # Syscall ABI (aarch64)
//
// | Register | Role            |
// |----------|-----------------|
// | x8       | Syscall number  |
// | x0..x5   | Arguments       |
// | x0       | Return value    |
//
// Invoke via `svc #0`. All other registers are preserved across the call.
// Negative return values indicate errors — decoded into `SyscallError`.

#![no_std]

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

mod nr {
    pub const EXIT: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    pub const HANDLE_CLOSE: u64 = 3;
    pub const CHANNEL_SIGNAL: u64 = 4;
    pub const CHANNEL_CREATE: u64 = 5;
    pub const SCHEDULING_CONTEXT_CREATE: u64 = 6;
    pub const SCHEDULING_CONTEXT_BORROW: u64 = 7;
    pub const SCHEDULING_CONTEXT_RETURN: u64 = 8;
    pub const SCHEDULING_CONTEXT_BIND: u64 = 9;
    pub const FUTEX_WAIT: u64 = 10;
    pub const FUTEX_WAKE: u64 = 11;
    pub const WAIT: u64 = 12;
    pub const TIMER_CREATE: u64 = 13;
    pub const INTERRUPT_REGISTER: u64 = 14;
    pub const INTERRUPT_ACK: u64 = 15;
    pub const DEVICE_MAP: u64 = 16;
    pub const DMA_ALLOC: u64 = 17;
    pub const DMA_FREE: u64 = 18;
    pub const THREAD_CREATE: u64 = 19;
    pub const PROCESS_CREATE: u64 = 20;
    pub const PROCESS_START: u64 = 21;
    pub const HANDLE_SEND: u64 = 22;
    pub const PROCESS_KILL: u64 = 23;
    pub const MEMORY_SHARE: u64 = 24;
    pub const MEMORY_ALLOC: u64 = 25;
    pub const MEMORY_FREE: u64 = 26;
}

const MIN_BLOCK: usize = core::mem::size_of::<FreeBlock>();
const PAGE_SIZE: usize = 4096;

struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}
struct UserHeap {
    head: UnsafeCell<*mut FreeBlock>,
    lock: AtomicBool,
}

/// Convenience alias for syscall results.
pub type SyscallResult<T> = Result<T, SyscallError>;

/// Error codes returned by kernel syscalls.
///
/// Unifies the kernel's `syscall::Error` and `handle::HandleError` enums into
/// one flat enum for userspace. The numeric values match the kernel's `repr(i64)`
/// discriminants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum SyscallError {
    UnknownSyscall = -1,
    BadAddress = -2,
    BadLength = -3,
    InvalidArgument = -4,
    AlreadyBorrowing = -5,
    NotBorrowing = -6,
    AlreadyBound = -7,
    WouldBlock = -8,
    OutOfMemory = -9,
    InvalidHandle = -10,
    // -11 is unused in the kernel.
    InsufficientRights = -12,
    TableFull = -13,
    SlotOccupied = -14,
}

#[global_allocator]
static HEAP: UserHeap = UserHeap {
    head: UnsafeCell::new(core::ptr::null_mut()),
    lock: AtomicBool::new(false),
};

impl UserHeap {
    fn acquire(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }
    /// Request pages from the kernel and add them to the free list.
    ///
    /// Allocates enough pages to satisfy `min_size` bytes. Returns true
    /// on success, false if the kernel refuses (out of memory / budget).
    unsafe fn grow(&self, min_size: usize) -> bool {
        let pages = (min_size + PAGE_SIZE - 1) / PAGE_SIZE;
        let va = match memory_alloc(pages as u64) {
            Ok(va) => va,
            Err(_) => return false,
        };
        let block = va as *mut FreeBlock;
        let head = &mut *self.head.get();

        (*block).size = pages * PAGE_SIZE;
        (*block).next = *head;
        *head = block;

        true
    }
    fn release(&self) {
        self.lock.store(false, Ordering::Release);
    }
}
// ---------------------------------------------------------------------------
// Heap allocator — linked-list first-fit with coalescing.
//
// Grows on demand by calling `memory_alloc`. Programs use this by adding
// `extern crate alloc;` to get Vec, String, Box, etc. Programs that never
// import `alloc` pay no cost.
// ---------------------------------------------------------------------------
unsafe impl GlobalAlloc for UserHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.acquire();

        let size = align_up(layout.size().max(MIN_BLOCK), MIN_BLOCK);
        let align = layout.align().max(MIN_BLOCK);
        let head = &mut *self.head.get();
        let mut prev = head as *mut *mut FreeBlock;

        // First-fit search with alignment handling.
        loop {
            let current = *prev;

            if current.is_null() {
                break;
            }

            let block_addr = current as usize;
            let block_size = (*current).size;
            let alloc_start = align_up(block_addr, align);
            let front_pad = alloc_start - block_addr;

            // Front padding must fit a free block header, or be zero.
            if front_pad > 0 && front_pad < MIN_BLOCK {
                prev = &mut (*current).next;
                continue;
            }
            if front_pad + size > block_size {
                prev = &mut (*current).next;
                continue;
            }

            let back_left = block_size - front_pad - size;

            // Unlink this block.
            *prev = (*current).next;

            // Return front padding as a smaller free block.
            if front_pad >= MIN_BLOCK {
                let front = block_addr as *mut FreeBlock;

                (*front).size = front_pad;
                (*front).next = *prev;
                *prev = front;
                prev = &mut (*front).next;
            }
            // Return back leftover as a free block.
            if back_left >= MIN_BLOCK {
                let back = (alloc_start + size) as *mut FreeBlock;

                (*back).size = back_left;
                (*back).next = *prev;
                *prev = back;
            }

            self.release();

            return alloc_start as *mut u8;
        }

        // Free list exhausted — grow and retry once.
        if self.grow(size) {
            // Retry from the head (new block was prepended).
            prev = &mut *self.head.get() as *mut *mut FreeBlock;

            loop {
                let current = *prev;

                if current.is_null() {
                    break;
                }

                let block_addr = current as usize;
                let block_size = (*current).size;
                let alloc_start = align_up(block_addr, align);
                let front_pad = alloc_start - block_addr;

                if front_pad > 0 && front_pad < MIN_BLOCK {
                    prev = &mut (*current).next;
                    continue;
                }
                if front_pad + size > block_size {
                    prev = &mut (*current).next;
                    continue;
                }

                let back_left = block_size - front_pad - size;

                *prev = (*current).next;

                if front_pad >= MIN_BLOCK {
                    let front = block_addr as *mut FreeBlock;

                    (*front).size = front_pad;
                    (*front).next = *prev;
                    *prev = front;
                    prev = &mut (*front).next;
                }
                if back_left >= MIN_BLOCK {
                    let back = (alloc_start + size) as *mut FreeBlock;

                    (*back).size = back_left;
                    (*back).next = *prev;
                    *prev = back;
                }

                self.release();

                return alloc_start as *mut u8;
            }
        }

        self.release();

        core::ptr::null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.acquire();

        let size = align_up(layout.size().max(MIN_BLOCK), MIN_BLOCK);
        let addr = ptr as usize;
        let head = &mut *self.head.get();

        // Walk to the sorted insertion point.
        let mut prev_block: *mut FreeBlock = core::ptr::null_mut();
        let mut current = *head;

        while !current.is_null() && (current as usize) < addr {
            prev_block = current;
            current = (*current).next;
        }

        // Insert freed region.
        let block = addr as *mut FreeBlock;

        (*block).size = size;
        (*block).next = current;

        if prev_block.is_null() {
            *head = block;
        } else {
            (*prev_block).next = block;
        }

        // Coalesce with next neighbor.
        if !current.is_null() && addr + size == current as usize {
            (*block).size += (*current).size;
            (*block).next = (*current).next;
        }

        // Coalesce with previous neighbor.
        if !prev_block.is_null() {
            let prev_end = prev_block as usize + (*prev_block).size;

            if prev_end == addr {
                (*prev_block).size += (*block).size;
                (*prev_block).next = (*block).next;
            }
        }

        self.release();
    }
}
// SAFETY: All free list access is protected by a spinlock (AtomicBool CAS).
unsafe impl Sync for UserHeap {}

impl SyscallError {
    /// Decode a raw negative return value into a `SyscallError`.
    ///
    /// Returns `UnknownSyscall` for unrecognized codes (defensive — kernel
    /// shouldn't produce codes outside this set, but userspace shouldn't panic
    /// if it does).
    pub fn from_raw(val: i64) -> Self {
        match val {
            -1 => Self::UnknownSyscall,
            -2 => Self::BadAddress,
            -3 => Self::BadLength,
            -4 => Self::InvalidArgument,
            -5 => Self::AlreadyBorrowing,
            -6 => Self::NotBorrowing,
            -7 => Self::AlreadyBound,
            -8 => Self::WouldBlock,
            -9 => Self::OutOfMemory,
            -10 => Self::InvalidHandle,
            -12 => Self::InsufficientRights,
            -13 => Self::TableFull,
            -14 => Self::SlotOccupied,
            _ => Self::UnknownSyscall,
        }
    }
}

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
/// Convert a raw syscall return value to a `SyscallResult`.
///
/// Non-negative → `Ok(value as u64)`. Negative → `Err(SyscallError)`.
fn result(raw: i64) -> SyscallResult<u64> {
    if raw >= 0 {
        Ok(raw as u64)
    } else {
        Err(SyscallError::from_raw(raw))
    }
}

#[inline(always)]
unsafe fn syscall0(nr: u64) -> u64 {
    let ret: u64;

    core::arch::asm!(
        "svc #0",
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );

    ret
}
#[inline(always)]
unsafe fn syscall1(nr: u64, a0: u64) -> u64 {
    let ret: u64;

    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );

    ret
}
#[inline(always)]
unsafe fn syscall2(nr: u64, a0: u64, a1: u64) -> u64 {
    let ret: u64;

    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x1") a1,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );

    ret
}
#[inline(always)]
unsafe fn syscall3(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;

    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x1") a1,
        in("x2") a2,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );

    ret
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a channel with two endpoints.
///
/// Returns `(handle_a, handle_b)` — both handles refer to endpoints of the
/// same shared-memory channel. The shared page is automatically mapped into
/// the calling process.
pub fn channel_create() -> SyscallResult<(u8, u8)> {
    let raw = unsafe { syscall0(nr::CHANNEL_CREATE) as i64 };
    let val = result(raw)?;

    Ok((val as u8, (val >> 8) as u8))
}
/// Signal the peer on a channel (write direction).
pub fn channel_signal(handle: u8) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::CHANNEL_SIGNAL, handle as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Map a device's MMIO region into this process's address space.
///
/// The kernel maps `size` bytes starting at physical address `pa` with device
/// memory attributes (non-cacheable). Returns the user VA on success.
pub fn device_map(pa: u64, size: u64) -> SyscallResult<usize> {
    let raw = unsafe { syscall2(nr::DEVICE_MAP, pa, size) as i64 };

    result(raw).map(|v| v as usize)
}
/// Allocate a DMA-capable buffer (2^order contiguous physical pages).
///
/// Returns the user VA of the mapped buffer. The physical address is written
/// to `pa_out` for programming device DMA registers. Order 0–4 (4 KiB – 64 KiB).
pub fn dma_alloc(order: u32, pa_out: &mut u64) -> SyscallResult<usize> {
    let raw = unsafe { syscall2(nr::DMA_ALLOC, order as u64, pa_out as *mut u64 as u64) as i64 };

    result(raw).map(|v| v as usize)
}
/// Free a DMA buffer previously allocated with `dma_alloc`.
///
/// `va` must be the value returned by `dma_alloc`. `order` must match the
/// original allocation.
pub fn dma_free(va: u64, order: u32) -> SyscallResult<()> {
    let raw = unsafe { syscall2(nr::DMA_FREE, va, order as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Terminate the calling process. Does not return.
pub fn exit() -> ! {
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") nr::EXIT,
            options(noreturn, nostack),
        );
    }
}
/// Wait on a futex. Blocks if the 32-bit value at `addr` equals `expected`.
///
/// Returns `Ok(())` on success (was woken). Returns `Err(WouldBlock)` if the
/// value at `addr` != `expected` (no block occurred).
pub fn futex_wait(addr: *const u32, expected: u32) -> SyscallResult<()> {
    let raw = unsafe { syscall2(nr::FUTEX_WAIT, addr as u64, expected as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Wake up to `count` threads waiting on a futex at `addr`.
///
/// Returns the number of threads actually woken.
pub fn futex_wake(addr: *const u32, count: u32) -> SyscallResult<u32> {
    let raw = unsafe { syscall2(nr::FUTEX_WAKE, addr as u64, count as u64) as i64 };

    result(raw).map(|v| v as u32)
}
/// Close a handle, releasing the associated kernel resource.
pub fn handle_close(handle: u8) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::HANDLE_CLOSE, handle as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Send a handle to a suspended child process.
///
/// Copies the handle at `source_handle` (in the caller's table) into the
/// target process identified by `target_handle` (which must be a Process
/// handle). The target must not have been started yet. For channel handles,
/// the shared page is also mapped into the target's address space.
pub fn handle_send(target_handle: u8, source_handle: u8) -> SyscallResult<()> {
    let raw =
        unsafe { syscall2(nr::HANDLE_SEND, target_handle as u64, source_handle as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Acknowledge an interrupt, allowing the device to fire again.
///
/// Clears the pending flag and re-enables the IRQ in the GIC. Must be called
/// after processing each interrupt.
pub fn interrupt_ack(handle: u8) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::INTERRUPT_ACK, handle as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Register for a hardware interrupt. Returns a waitable handle.
///
/// The handle becomes ready when the IRQ fires. Use `wait` to block until
/// the interrupt occurs, then call `interrupt_ack` after processing.
pub fn interrupt_register(irq: u32) -> SyscallResult<u8> {
    let raw = unsafe { syscall1(nr::INTERRUPT_REGISTER, irq as u64) as i64 };

    result(raw).map(|v| v as u8)
}
/// Allocate anonymous heap memory (demand-paged, zero-filled on first touch).
///
/// Returns the user VA of the start of the allocated region. The region is
/// `page_count * 4096` bytes. Pages are not physically allocated until touched.
pub fn memory_alloc(page_count: u64) -> SyscallResult<usize> {
    let raw = unsafe { syscall1(nr::MEMORY_ALLOC, page_count) as i64 };

    result(raw).map(|v| v as usize)
}
/// Free a heap allocation previously obtained from `memory_alloc`.
///
/// `va` must be the value returned by `memory_alloc`. Frees all physical
/// pages that were demand-paged in the region and reclaims the virtual
/// address range.
pub fn memory_free(va: usize, page_count: u64) -> SyscallResult<()> {
    let raw = unsafe { syscall2(nr::MEMORY_FREE, va as u64, page_count) as i64 };

    result(raw)?;

    Ok(())
}
/// Map physical pages into a target process's shared memory region.
///
/// Maps `page_count` contiguous physical pages starting at `pa` into the
/// target process (identified by `target_handle`, a Process handle). The
/// target must not have been started yet. Returns the VA in the target's
/// address space.
pub fn memory_share(target_handle: u8, pa: u64, page_count: u64) -> SyscallResult<usize> {
    let raw = unsafe { syscall3(nr::MEMORY_SHARE, target_handle as u64, pa, page_count) as i64 };

    result(raw).map(|v| v as usize)
}
/// Write to the kernel console, ignoring errors.
///
/// Convenience wrapper around `write` for debug logging where the caller
/// doesn't need (or want) to handle UART failures.
pub fn print(buf: &[u8]) {
    let _ = write(buf);
}
/// Create a process from an ELF binary in memory. Returns a waitable handle.
///
/// The child process starts suspended — call `process_start` with the returned
/// handle to make its thread runnable. The handle becomes ready when the child's
/// last thread exits.
pub fn process_create(elf_ptr: *const u8, elf_len: usize) -> SyscallResult<u8> {
    let raw = unsafe { syscall2(nr::PROCESS_CREATE, elf_ptr as u64, elf_len as u64) as i64 };

    result(raw).map(|v| v as u8)
}
/// Kill a process, terminating all its threads.
///
/// The handle must be a Process handle with write rights. All threads in the
/// target process are terminated and full cleanup runs.
pub fn process_kill(handle: u8) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::PROCESS_KILL, handle as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Start a suspended child process.
///
/// Makes all suspended threads in the process identified by `handle` runnable.
pub fn process_start(handle: u8) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::PROCESS_START, handle as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Bind a scheduling context to the calling thread.
///
/// The thread must not already have a context bound.
pub fn scheduling_context_bind(handle: u8) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::SCHEDULING_CONTEXT_BIND, handle as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Borrow another scheduling context (context donation).
///
/// Saves the current context and switches to the one identified by `handle`.
pub fn scheduling_context_borrow(handle: u8) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::SCHEDULING_CONTEXT_BORROW, handle as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Create a scheduling context with the given budget and period (both in ns).
///
/// Returns the handle index.
pub fn scheduling_context_create(budget: u64, period: u64) -> SyscallResult<u8> {
    let raw = unsafe { syscall2(nr::SCHEDULING_CONTEXT_CREATE, budget, period) as i64 };

    result(raw).map(|v| v as u8)
}
/// Return a borrowed scheduling context, restoring the saved one.
pub fn scheduling_context_return() -> SyscallResult<()> {
    let raw = unsafe { syscall0(nr::SCHEDULING_CONTEXT_RETURN) as i64 };

    result(raw)?;

    Ok(())
}
/// Create a new thread in the calling process.
///
/// The thread starts at `entry_va` with user stack pointer `stack_top`.
/// Returns a waitable handle (becomes ready on thread exit).
pub fn thread_create(entry_va: u64, stack_top: u64) -> SyscallResult<u8> {
    let raw = unsafe { syscall2(nr::THREAD_CREATE, entry_va, stack_top) as i64 };

    result(raw).map(|v| v as u8)
}
/// Create a one-shot timer that fires after `timeout_ns` nanoseconds.
///
/// Returns a waitable handle. Wait on it via `wait` to block until the deadline.
pub fn timer_create(timeout_ns: u64) -> SyscallResult<u8> {
    let raw = unsafe { syscall1(nr::TIMER_CREATE, timeout_ns) as i64 };

    result(raw).map(|v| v as u8)
}
/// Wait for an event on one or more handles.
///
/// Blocks until any handle in `handles` has a pending event or the timeout
/// expires. Returns the index of the first ready handle (0-based).
/// Timeout of `u64::MAX` waits forever; `0` polls without blocking.
pub fn wait(handles: &[u8], timeout_ns: u64) -> SyscallResult<usize> {
    let raw = unsafe {
        syscall3(
            nr::WAIT,
            handles.as_ptr() as u64,
            handles.len() as u64,
            timeout_ns,
        ) as i64
    };

    result(raw).map(|v| v as usize)
}
/// Write `buf` to the kernel console (UART).
///
/// Returns the number of bytes written. For fire-and-forget logging, use
/// `print` instead.
pub fn write(buf: &[u8]) -> SyscallResult<usize> {
    let raw = unsafe { syscall2(nr::WRITE, buf.as_ptr() as u64, buf.len() as u64) as i64 };

    result(raw).map(|v| v as usize)
}
/// Yield the current timeslice to the scheduler.
pub fn yield_now() {
    unsafe {
        syscall0(nr::YIELD);
    }
}

// ---------------------------------------------------------------------------
// Panic handler — exits the process instead of spinning.
// ---------------------------------------------------------------------------
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    exit()
}
