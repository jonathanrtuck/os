//! Virtqueue submission helpers.

use protocol::metal;

use crate::{dma::DmaBuf, VIRTQ_RENDER, VIRTQ_SETUP};

pub(crate) fn submit_setup(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    dma: &DmaBuf,
    len: usize,
) {
    vq.push_chain(&[(dma.pa, len as u32, false)]);
    device.notify(VIRTQ_SETUP);
    let _ = sys::wait(&[irq_handle.0], u64::MAX);
    device.ack_interrupt();
    vq.pop_used();
    let _ = sys::interrupt_ack(irq_handle);
}

pub(crate) fn submit_render(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    dma: &DmaBuf,
    len: usize,
) {
    vq.push_chain(&[(dma.pa, len as u32, false)]);
    device.notify(VIRTQ_RENDER);
    let _ = sys::wait(&[irq_handle.0], u64::MAX);
    device.ack_interrupt();
    vq.pop_used();
    let _ = sys::interrupt_ack(irq_handle);
}

/// Copy a CommandBuffer's bytes into DMA memory and submit.
pub(crate) fn send_setup(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    dma: &DmaBuf,
    cmdbuf: &metal::CommandBuffer,
) {
    let data = cmdbuf.as_bytes();
    assert!(data.len() <= dma.size());
    // SAFETY: dma.va is valid DMA memory of dma.size() bytes.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dma.va as *mut u8, data.len()) };
    submit_setup(device, vq, irq_handle, dma, data.len());
}

pub(crate) fn send_render(
    device: &virtio::Device,
    vq: &mut virtio::Virtqueue,
    irq_handle: sys::InterruptHandle,
    dma: &DmaBuf,
    cmdbuf: &metal::CommandBuffer,
) {
    let data = cmdbuf.as_bytes();
    assert!(data.len() <= dma.size());
    // SAFETY: dma.va is valid DMA memory of dma.size() bytes.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dma.va as *mut u8, data.len()) };
    submit_render(device, vq, irq_handle, dma, data.len());
}

pub(crate) fn alloc_virtqueue(device: &virtio::Device, index: u32, size: u32) -> virtio::Virtqueue {
    let order = virtio::Virtqueue::allocation_order(size);
    let mut pa: u64 = 0;
    let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
        sys::print(b"metal-render: dma_alloc (vq) failed\n");
        sys::exit();
    });
    let bytes = (1usize << order) * ipc::PAGE_SIZE;
    // SAFETY: va is freshly allocated DMA memory of `bytes` size.
    unsafe { core::ptr::write_bytes(va as *mut u8, 0, bytes) };
    let vq = virtio::Virtqueue::new(size, va, pa);
    device.setup_queue(index, size, vq.desc_pa(), vq.avail_pa(), vq.used_pa());
    vq
}
