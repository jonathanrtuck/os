//! Kernel heap — talc allocator for ObjectTable metadata and live objects.
//!
//! ObjectTable entries use `Option<Box<T>>` — only live objects consume heap.
//! At init, each table allocates MAX × 8 bytes (pointers) plus metadata.
//! Actual objects (Endpoint ~7 KB, Thread ~300 B) are allocated individually
//! when created via syscall, keeping init cost proportional to metadata, not
//! max capacity × object size.

#[cfg(target_os = "none")]
mod inner {
    use talc::source::Claim;

    use crate::frame::arch::sync::RawTicketLock;

    const HEAP_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

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
