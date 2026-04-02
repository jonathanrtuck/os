// AUDIT: 2026-03-11 — 1 unsafe block verified (copy_segment_page), 6-category
// checklist applied. Bugs found and fixed: (1) integer overflow in page_count
// and VMA end computation for adversarial ELF mem_size — replaced with checked
// arithmetic. (2) Process slot leak on thread allocation failure — both
// create_from_user_elf and spawn_from_elf now call remove_empty_process on the
// error path to clean up orphaned process slots when spawn fails (OOM).

//! Process management.
//!
//! A process owns an address space and handle table. Threads within a
//! process share these resources. A process is alive while any of its
//! threads are alive. Last thread exit triggers full cleanup.

use alloc::boxed::Box;

use super::{
    address_space::{AddressSpace, PageAttrs},
    address_space_id, executable,
    handle::HandleTable,
    memory,
    memory_region::{Backing, Vma},
    page_allocator,
    paging::{PAGE_SIZE, USER_STACK_TOP, USER_STACK_VA},
    scheduler,
    thread::ThreadId,
};

/// A process — owns an address space and handle table shared by all its threads.
pub struct Process {
    pub(crate) address_space: Box<AddressSpace>,
    pub(crate) handles: HandleTable,
    /// Number of live threads in this process. Last thread exit triggers cleanup.
    pub(crate) thread_count: u32,
    /// Set to true by `process_start`. Prevents `handle_send` to running processes.
    pub(crate) started: bool,
    /// Set by `process_kill`. Triggers deferred address space cleanup when the
    /// last running thread is rescheduled away.
    pub(crate) killed: bool,
    /// Per-process syscall filter bitmask. Bit N set = syscall N allowed.
    /// Defaults to u64::MAX (all syscalls allowed). Set before process_start
    /// via the PROCESS_SET_SYSCALL_FILTER syscall.
    pub(crate) syscall_mask: u64,
}
/// Unique process identifier. Index into the scheduler's process table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessId(pub u32);

impl Process {
    pub fn new(address_space: Box<AddressSpace>) -> Self {
        Self {
            address_space,
            handles: HandleTable::new(),
            thread_count: 0,
            started: false,
            killed: false,
            syscall_mask: u64::MAX,
        }
    }
}
impl super::waitable::WaitableId for ProcessId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Compute the number of pages needed for a segment of `mem_size` bytes.
///
/// Returns `Err` if the arithmetic overflows (adversarial ELF `mem_size`).
fn checked_page_count(mem_size: u64) -> Result<u64, &'static str> {
    mem_size
        .checked_add(PAGE_SIZE - 1)
        .map(|n| n / PAGE_SIZE)
        .ok_or("segment mem_size overflow")
}
/// Compute the VMA end address from a base VA and page count.
///
/// Returns `Err` if the result overflows the 64-bit address space.
fn checked_vma_end(base_va: u64, page_count: u64) -> Result<u64, &'static str> {
    page_count
        .checked_mul(PAGE_SIZE)
        .and_then(|size| base_va.checked_add(size))
        .ok_or("segment VMA end overflow")
}
/// Copy pages from an ELF segment into a freshly allocated frame.
///
/// Shared helper used by both eager and demand-paged loading paths.
fn copy_segment_page(file_data: &[u8], file_size: u64, seg_offset: u64, pa: memory::Pa) {
    if seg_offset < file_size {
        let src_start = seg_offset as usize;
        let src_end = core::cmp::min((seg_offset + PAGE_SIZE) as usize, file_size as usize);
        let src = &file_data[src_start..src_end];
        let dst = memory::phys_to_virt(pa) as *mut u8;

        // SAFETY: `pa` was just allocated (zeroed by the page allocator). `dst`
        // is the kernel VA for that frame, valid for PAGE_SIZE bytes. `src` is
        // a subslice of `file_data`, bounded by min(seg_offset + PAGE_SIZE,
        // file_size), so `src.len() <= PAGE_SIZE`. The source and destination
        // do not overlap (kernel heap vs page frame regions are disjoint).
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
    }
}
/// Create a process from a user-provided ELF buffer, with a suspended thread.
///
/// Eagerly maps ALL segment pages (the ELF data is temporary and can't be
/// stored for demand paging). The initial thread is suspended — call
/// `scheduler::start_suspended_threads` to make it runnable.
pub fn create_from_user_elf(elf_bytes: &[u8]) -> Result<(ProcessId, ThreadId), &'static str> {
    let header = executable::parse_header(elf_bytes).map_err(|_| "bad ELF header")?;
    let (asid, _generation) = address_space_id::alloc();
    let mut addr_space = match AddressSpace::new(asid) {
        Some(a) => Box::new(a),
        None => {
            address_space_id::free(asid);
            return Err("out of frames for L0 page table");
        }
    };

    for i in 0..header.ph_count {
        let seg = match executable::load_segment(elf_bytes, &header, i)
            .map_err(|_| "bad program header")?
        {
            Some(seg) => seg,
            None => continue,
        };
        let file_data =
            executable::segment_data(elf_bytes, &seg).map_err(|_| "segment data out of bounds")?;
        let attrs = executable::segment_attrs(seg.flags);
        let base_va = seg.vaddr & !(PAGE_SIZE - 1);
        let page_count = checked_page_count(seg.mem_size)?;
        let vma_end = checked_vma_end(base_va, page_count)?;
        let is_exec = seg.flags & 1 != 0;
        let is_write = seg.flags & 2 != 0;

        // Anonymous backing — all pages are eagerly mapped below.
        addr_space.vmas.insert(Vma {
            start: base_va,
            end: vma_end,
            writable: is_write,
            executable: is_exec,
            backing: Backing::Anonymous,
        });

        // Eagerly map ALL pages (ELF data is temporary).
        for page in 0..page_count {
            let pa = page_allocator::alloc_frame().ok_or("out of frames for user segment")?;

            copy_segment_page(file_data, seg.file_size, page * PAGE_SIZE, pa);

            if !addr_space.map_page(base_va + page * PAGE_SIZE, pa.as_u64(), &attrs) {
                page_allocator::free_frame(pa);
                return Err("out of page table frames for ELF segment");
            }
        }
    }

    setup_stack(&mut addr_space)?;

    let process_id = scheduler::create_process(addr_space);
    let thread_id = match scheduler::spawn_user_suspended(process_id, header.entry, USER_STACK_TOP)
    {
        Some(tid) => tid,
        None => {
            // Clean up the orphaned process slot. The process has no threads
            // (spawn failed), so remove_empty_process drops the Box<AddressSpace>
            // which triggers AddressSpace::Drop (free all frames + ASID).
            scheduler::remove_empty_process(process_id);

            return Err("out of memory for initial thread stack");
        }
    };

    Ok((process_id, thread_id))
}
/// Set up the user stack VMA and eagerly map the top page.
fn setup_stack(addr_space: &mut AddressSpace) -> Result<(), &'static str> {
    addr_space.vmas.insert(Vma {
        start: USER_STACK_VA,
        end: USER_STACK_TOP,
        writable: true,
        executable: false,
        backing: Backing::Anonymous,
    });

    let top_stack_va = USER_STACK_TOP - PAGE_SIZE;
    let pa = page_allocator::alloc_frame().ok_or("out of frames for user stack")?;

    if !addr_space.map_page(top_stack_va, pa.as_u64(), &PageAttrs::user_rw()) {
        page_allocator::free_frame(pa);

        return Err("out of page table frames for user stack");
    }

    Ok(())
}
