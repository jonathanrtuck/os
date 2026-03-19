//! FFI wire-format structs and DMA helpers for virtio-gpu commands.
//!
//! All structs are `#[repr(C)]` for direct memory-mapped use with the
//! virtio-gpu device. `DmaBuf` wraps DMA page allocation with RAII cleanup.

use alloc::boxed::Box;

use crate::VIRGL_CTX_ID;

// ── Wire-format structs ──────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CtrlHeader {
    pub(crate) cmd_type: u32,
    pub(crate) flags: u32,
    pub(crate) fence_id: u64,
    pub(crate) ctx_id: u32,
    pub(crate) _padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct DisplayInfo {
    pub(crate) rect_x: u32,
    pub(crate) rect_y: u32,
    pub(crate) rect_width: u32,
    pub(crate) rect_height: u32,
    pub(crate) enabled: u32,
    pub(crate) flags: u32,
}

#[repr(C)]
pub(crate) struct CtxCreate {
    pub(crate) hdr: CtrlHeader,
    pub(crate) nlen: u32,
    pub(crate) context_init: u32,
    pub(crate) debug_name: [u8; 64],
}

#[repr(C)]
pub(crate) struct ResourceCreate3d {
    pub(crate) hdr: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) target: u32,
    pub(crate) format: u32,
    pub(crate) bind: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) depth: u32,
    pub(crate) array_size: u32,
    pub(crate) last_level: u32,
    pub(crate) nr_samples: u32,
    pub(crate) flags: u32,
    pub(crate) _pad: u32,
}

#[repr(C)]
pub(crate) struct AttachBacking {
    pub(crate) hdr: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) nr_entries: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct MemEntry {
    pub(crate) addr: u64,
    pub(crate) length: u32,
    pub(crate) _padding: u32,
}

#[repr(C)]
pub(crate) struct CtxResource {
    pub(crate) hdr: CtrlHeader,
    pub(crate) resource_id: u32,
    pub(crate) _pad: [u32; 3],
}

#[repr(C)]
pub(crate) struct SetScanout {
    pub(crate) hdr: CtrlHeader,
    pub(crate) rect_x: u32,
    pub(crate) rect_y: u32,
    pub(crate) rect_width: u32,
    pub(crate) rect_height: u32,
    pub(crate) scanout_id: u32,
    pub(crate) resource_id: u32,
}

#[repr(C)]
pub(crate) struct ResourceFlush {
    pub(crate) hdr: CtrlHeader,
    pub(crate) rect_x: u32,
    pub(crate) rect_y: u32,
    pub(crate) rect_width: u32,
    pub(crate) rect_height: u32,
    pub(crate) resource_id: u32,
    pub(crate) _padding: u32,
}

#[repr(C)]
pub(crate) struct TransferToHost3d {
    pub(crate) hdr: CtrlHeader,
    pub(crate) box_x: u32,
    pub(crate) box_y: u32,
    pub(crate) box_z: u32,
    pub(crate) box_w: u32,
    pub(crate) box_h: u32,
    pub(crate) box_d: u32,
    pub(crate) offset: u64,
    pub(crate) resource_id: u32,
    pub(crate) level: u32,
    pub(crate) stride: u32,
    pub(crate) layer_stride: u32,
}

#[repr(C)]
pub(crate) struct Submit3dHeader {
    pub(crate) hdr: CtrlHeader,
    pub(crate) size: u32,
    pub(crate) _pad: u32,
}

// ── DMA buffer helper ────────────────────────────────────────────────────

pub(crate) struct DmaBuf {
    pub(crate) va: usize,
    pub(crate) pa: u64,
    pub(crate) order: u32,
}

impl DmaBuf {
    pub(crate) fn alloc(order: u32) -> DmaBuf {
        let mut pa: u64 = 0;
        let va = sys::dma_alloc(order, &mut pa).unwrap_or_else(|_| {
            sys::print(b"virgil-render: dma_alloc failed\n");
            sys::exit();
        });
        // SAFETY: va is valid DMA memory of (1 << order) pages, freshly allocated.
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, (1usize << order) * 4096) };
        DmaBuf { va, pa, order }
    }

    pub(crate) fn free(&mut self) {
        if self.va != 0 {
            let _ = sys::dma_free(self.va as u64, self.order);
            self.va = 0;
        }
    }
}

impl Drop for DmaBuf {
    fn drop(&mut self) {
        self.free();
    }
}

// ── Helper functions ─────────────────────────────────────────────────────

/// Heap-allocate a zeroed `T` via `alloc_zeroed`, aborting on null.
///
/// Many types in this driver exceed the 16 KiB user stack, so `Box::new()`
/// cannot be used.  `alloc_zeroed` produces valid initial state (all zeros)
/// and this helper adds the null check that bare `Box::from_raw` would skip.
pub(crate) fn box_zeroed<T>() -> Box<T> {
    unsafe {
        let ptr = alloc::alloc::alloc_zeroed(alloc::alloc::Layout::new::<T>());
        if ptr.is_null() {
            sys::print(b"FATAL: alloc_zeroed returned null\n");
            sys::exit();
        }
        Box::from_raw(ptr as *mut T)
    }
}

pub(crate) fn ctrl_header(cmd_type: u32) -> CtrlHeader {
    CtrlHeader {
        cmd_type,
        flags: 0,
        fence_id: 0,
        ctx_id: 0,
        _padding: 0,
    }
}

pub(crate) fn ctrl_header_ctx(cmd_type: u32) -> CtrlHeader {
    CtrlHeader {
        cmd_type,
        flags: 0,
        fence_id: 0,
        ctx_id: VIRGL_CTX_ID,
        _padding: 0,
    }
}
