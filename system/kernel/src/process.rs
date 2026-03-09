//! Process management.
//!
//! A process owns an address space and handle table. Threads within a
//! process share these resources. Currently each process has exactly one
//! thread (multi-threading is planned for Phase 2b).

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
}
/// Unique process identifier. Index into the scheduler's process table.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ProcessId(pub u32);

impl Process {
    pub fn new(id: ProcessId, address_space: Box<AddressSpace>) -> Self {
        Self {
            id,
            address_space,
            handles: HandleTable::new(),
        }
    }

    pub fn id(&self) -> ProcessId {
        self.id
    }
}

/// Parse an ELF binary and spawn a user process with one thread.
///
/// Creates a Process (address space + handle table) and one initial Thread.
/// Returns both IDs. The thread starts in Ready state.
pub fn spawn_from_elf(elf_bytes: &'static [u8]) -> Result<(ProcessId, ThreadId), &'static str> {
    let header = executable::parse_header(elf_bytes).map_err(|_| "bad ELF header")?;
    let (asid, generation) = address_space_id::alloc();
    let mut addr_space = Box::new(AddressSpace::new(asid, generation));

    // Map each PT_LOAD segment from the ELF into the new address space.
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
        // Create VMA for this segment.
        let is_exec = seg.flags & 1 != 0; // PF_X
        let is_write = seg.flags & 2 != 0; // PF_W

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
            let va = base_va + page * PAGE_SIZE;
            let seg_offset = page * PAGE_SIZE;

            if seg_offset < seg.file_size {
                let src_start = seg_offset as usize;
                let src_end =
                    core::cmp::min((seg_offset + PAGE_SIZE) as usize, seg.file_size as usize);
                let src = &file_data[src_start..src_end];
                let dst = memory::phys_to_virt(pa) as *mut u8;

                // SAFETY: `pa` was just allocated. `src` is bounded by ELF data.
                unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
            }

            addr_space.map_page(va, pa.as_u64(), &attrs);
        }
    }

    // Create stack VMA (anonymous, writable).
    addr_space.vmas.insert(Vma {
        start: USER_STACK_VA,
        end: USER_STACK_TOP,
        readable: true,
        writable: true,
        executable: false,
        backing: Backing::Anonymous,
    });

    // Eagerly map the top stack page (first page the SP points into).
    let top_stack_va = USER_STACK_TOP - PAGE_SIZE;
    let pa = page_allocator::alloc_frame().ok_or("out of frames for user stack")?;

    addr_space.map_page(top_stack_va, pa.as_u64(), &PageAttrs::user_rw());

    // Map remaining stack pages lazily via demand paging.
    // Guard page = gap below USER_STACK_VA (no VMA → fault → kill).

    // Create process and spawn its initial thread.
    let process_id = scheduler::create_process(addr_space);
    let thread_id = scheduler::spawn_user(process_id, header.entry, USER_STACK_TOP);

    Ok((process_id, thread_id))
}
