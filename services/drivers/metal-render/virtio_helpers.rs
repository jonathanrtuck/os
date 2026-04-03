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
    if data.len() <= dma.size() {
        // Fast path: fits in one DMA submission.
        // SAFETY: dma.va is valid DMA memory of dma.size() bytes.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dma.va as *mut u8, data.len()) };
        submit_render(device, vq, irq_handle, dma, data.len());
    } else {
        // Oversized: split into chunks at command boundaries (8-byte aligned headers).
        // Each chunk is submitted and processed before the next, maintaining order.
        let max = dma.size();
        let mut offset = 0;
        while offset < data.len() {
            // Find the largest prefix of complete commands that fits.
            let end = split_at_command_boundary(data, offset, max);
            let chunk = &data[offset..end];
            // SAFETY: dma.va is valid DMA memory of dma.size() bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(chunk.as_ptr(), dma.va as *mut u8, chunk.len())
            };
            submit_render(device, vq, irq_handle, dma, chunk.len());
            offset = end;
        }
    }
}

/// Find the end offset of the largest run of complete commands starting
/// at `start` that fits within `max_bytes`. Commands have an 8-byte
/// header: [cmd_type: u32] [payload_size: u32], followed by `payload_size`
/// bytes of payload. Returns `start` if no complete command fits (should
/// not happen with a reasonably sized DMA buffer).
fn split_at_command_boundary(data: &[u8], start: usize, max_bytes: usize) -> usize {
    let mut pos = start;
    let mut last_good = start;
    while pos + 8 <= data.len() {
        let payload_size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let cmd_end = pos + 8 + payload_size;
        if cmd_end > data.len() {
            break; // Truncated command — shouldn't happen.
        }
        if cmd_end - start > max_bytes {
            break; // This command would exceed the chunk limit.
        }
        last_good = cmd_end;
        pos = cmd_end;
    }
    if last_good == start && pos < data.len() {
        // Single command larger than DMA buffer — shouldn't happen with
        // 256 KiB DMA and 4 KiB max inline vertex data, but handle gracefully.
        // Submit what we can and hope the hypervisor handles the partial command.
        let fallback = (start + max_bytes).min(data.len());
        return fallback;
    }
    last_good
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
