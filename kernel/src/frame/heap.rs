//! Kernel heap — talc allocator for rare variable-size allocations.
//!
//! Kernel objects use flat-array ObjectTable, NOT the heap. This allocator
//! handles only overflow page lists and init bootstrap.

#[cfg(target_os = "none")]
mod inner {
    use talc::source::Claim;

    use crate::frame::arch::sync::RawTicketLock;

    const HEAP_SIZE: usize = 4 * 16 * 1024; // 64 KiB

    #[global_allocator]
    static ALLOCATOR: talc::TalcLock<RawTicketLock, Claim> = {
        // SAFETY: HEAP_MEM is a static mut only accessed through the allocator.
        // The Claim source calls Talc::claim on first allocation.
        // Using raw pointer via addr_of_mut! to avoid Rust 2024 static_mut_refs.
        unsafe {
            talc::TalcLock::new(Claim::new(
                core::ptr::addr_of_mut!(HEAP_MEM) as *mut u8,
                HEAP_SIZE,
            ))
        }
    };

    #[repr(C, align(16))]
    struct AlignedHeap([u8; HEAP_SIZE]);

    static mut HEAP_MEM: AlignedHeap = AlignedHeap([0; HEAP_SIZE]);
}
