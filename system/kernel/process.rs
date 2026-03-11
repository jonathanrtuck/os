//! Process management.
//!
//! A process owns an address space and handle table. Threads within a
//! process share these resources. A process is alive while any of its
//! threads are alive. Last thread exit triggers full cleanup.

use super::address_space::{AddressSpace, PageAttrs};
use super::address_space_id;
use super::executable;
use super::handle::HandleTable;
use super::memory;
use super::memory_region::{Backing, Vma};
use super::page_allocator;
use super::paging::{PAGE_SIZE, USER_STACK_TOP, USER_STACK_VA};
use super::scheduler;
use super::thread::ThreadId;
use alloc::boxed::Box;

/// A process — owns an address space and handle table shared by all its threads.
pub struct Process {
    id: ProcessId,
    pub(crate) address_space: Box<AddressSpace>,
    pub(crate) handles: HandleTable,
    /// Number of live threads in this process. Last thread exit triggers cleanup.
    pub(crate) thread_count: u32,
    /// Set to true by `process_start`. Prevents `handle_send` to running processes.
    pub(crate) started: bool,
    /// Set by `process_kill`. Triggers deferred address space cleanup when the
    /// last running thread is rescheduled away.
    pub(crate) killed: bool,
}
/// Unique process identifier. Index into the scheduler's process table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessId(pub u32);

impl super::waitable::WaitableId for ProcessId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

impl Process {
    pub fn new(id: ProcessId, address_space: Box<AddressSpace>) -> Self {
        Self {
            id,
            address_space,
            handles: HandleTable::new(),
            thread_count: 0,
            started: false,
            killed: false,
        }
    }

    pub fn id(&self) -> ProcessId {
        self.id
    }
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

        // SAFETY: `pa` was just allocated (zeroed). `src` is bounded by ELF data.
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
    }
}
/// Set up the user stack VMA and eagerly map the top page.
fn setup_stack(addr_space: &mut AddressSpace) -> Result<(), &'static str> {
    addr_space.vmas.insert(Vma {
        start: USER_STACK_VA,
        end: USER_STACK_TOP,
        readable: true,
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

/// Create a process from a user-provided ELF buffer, with a suspended thread.
///
/// Eagerly maps ALL segment pages (the ELF data is temporary and can't be
/// stored for demand paging). The initial thread is suspended — call
/// `scheduler::start_suspended_threads` to make it runnable.
pub fn create_from_user_elf(elf_bytes: &[u8]) -> Result<(ProcessId, ThreadId), &'static str> {
    let header = executable::parse_header(elf_bytes).map_err(|_| "bad ELF header")?;
    let (asid, generation) = address_space_id::alloc();
    let mut addr_space = Box::new(
        AddressSpace::new(asid, generation).ok_or("out of frames for L0 page table")?,
    );

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
        let page_count = (seg.mem_size + PAGE_SIZE - 1) / PAGE_SIZE;
        let is_exec = seg.flags & 1 != 0;
        let is_write = seg.flags & 2 != 0;

        // Anonymous backing — all pages are eagerly mapped below.
        addr_space.vmas.insert(Vma {
            start: base_va,
            end: base_va + page_count * PAGE_SIZE,
            readable: true,
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
    let thread_id = scheduler::spawn_user_suspended(process_id, header.entry, USER_STACK_TOP)
        .ok_or("out of memory for initial thread stack")?;

    Ok((process_id, thread_id))
}

/// Parse an ELF binary and spawn a user process with one thread.
///
/// Creates a Process (address space + handle table) and one initial Thread.
/// Returns both IDs. The thread starts in Ready state. Segments use ELF-backed
/// VMAs for demand paging.
pub fn spawn_from_elf(elf_bytes: &'static [u8]) -> Result<(ProcessId, ThreadId), &'static str> {
    let header = executable::parse_header(elf_bytes).map_err(|_| "bad ELF header")?;
    let (asid, generation) = address_space_id::alloc();
    let mut addr_space = Box::new(
        AddressSpace::new(asid, generation).ok_or("out of frames for L0 page table")?,
    );

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
        let page_count = (seg.mem_size + PAGE_SIZE - 1) / PAGE_SIZE;
        let is_exec = seg.flags & 1 != 0;
        let is_write = seg.flags & 2 != 0;

        addr_space.vmas.insert(Vma {
            start: base_va,
            end: base_va + page_count * PAGE_SIZE,
            readable: true,
            writable: is_write,
            executable: is_exec,
            backing: Backing::Elf {
                data: file_data,
                data_len: seg.file_size,
            },
        });

        // Eagerly map the first page of code segments (avoids faulting on
        // first instruction) and all pages of small segments.
        let eager_pages = if page_count <= 2 { page_count } else { 1 };

        for page in 0..eager_pages {
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
    let thread_id = scheduler::spawn_user(process_id, header.entry, USER_STACK_TOP)
        .ok_or("out of memory for initial thread stack")?;

    Ok((process_id, thread_id))
}
