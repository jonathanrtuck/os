//! DMA buffer allocation helper.

pub(crate) struct DmaBuf {
    pub(crate) va: usize,
    pub(crate) pa: u64,
    pub(crate) order: u32,
}

impl DmaBuf {
    pub(crate) fn alloc(order: u32) -> Self {
        let mut pa: u64 = 0;
        let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
            sys::print(b"metal-render: dma_alloc failed\n");
            sys::exit();
        });
        let bytes = (1usize << order) * ipc::PAGE_SIZE;
        // SAFETY: va points to freshly allocated DMA memory of `bytes` size.
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, bytes) };
        Self { va, pa, order }
    }

    pub(crate) fn size(&self) -> usize {
        (1usize << self.order) * ipc::PAGE_SIZE
    }
}
