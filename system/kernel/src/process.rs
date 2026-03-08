//! Process creation from ELF binaries.

use super::addr_space::{AddressSpace, PageAttrs};
use super::asid;
use super::elf;
use super::memory;
use super::page_alloc;
use super::paging::{PAGE_SIZE, USER_STACK_PAGES, USER_STACK_TOP, USER_STACK_VA};
use super::scheduler;
use super::thread::ThreadId;
use super::vma::{Backing, Vma};
use alloc::boxed::Box;

/// Parse an ELF binary and spawn a user process.
///
/// Creates VMAs for ELF segments and stack. Pre-maps the first code page
/// and first stack page eagerly; remaining pages are demand-paged on fault.
/// Returns an error string on failure (bad ELF, OOM); callers decide severity.
pub fn spawn_from_elf(elf_bytes: &'static [u8]) -> Result<ThreadId, &'static str> {
    let header = elf::parse_header(elf_bytes).map_err(|_| "bad ELF header")?;
    let (asid, generation) = asid::alloc();
    let mut addr_space = Box::new(AddressSpace::new(asid, generation));

    // Map each PT_LOAD segment from the ELF into the new address space.
    for i in 0..header.ph_count {
        let seg = match elf::load_segment(elf_bytes, &header, i).map_err(|_| "bad program header")? {
            Some(seg) => seg,
            None => continue,
        };
        let file_data = elf::segment_data(elf_bytes, &seg).map_err(|_| "segment data out of bounds")?;
        let attrs = elf::segment_attrs(seg.flags);
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
            let pa = page_alloc::alloc_frame().ok_or("out of frames for user segment")?;
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

            addr_space.map_page(va, pa as u64, &attrs);
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
    let pa = page_alloc::alloc_frame().ok_or("out of frames for user stack")?;

    addr_space.map_page(top_stack_va, pa as u64, &PageAttrs::user_rw());

    // Map remaining stack pages lazily via demand paging.
    // Guard page = gap below USER_STACK_VA (no VMA → fault → kill).

    Ok(scheduler::spawn_user(addr_space, header.entry, USER_STACK_TOP))
}
