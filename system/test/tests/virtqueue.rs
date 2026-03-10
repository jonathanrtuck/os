//! Host-side tests for virtqueue descriptor validation.
//!
//! Tests the boundary validation logic added to free_descriptor_chain
//! and pop_used. Uses a simplified in-memory virtqueue layout to verify
//! that out-of-bounds descriptor IDs from a device are rejected without
//! kernel memory corruption.

/// Simplified Descriptor matching the kernel's layout.
#[repr(C)]
#[derive(Clone, Copy)]
struct Descriptor {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

const DESC_F_NEXT: u16 = 1;

/// Simplified UsedElem matching the kernel's layout.
#[repr(C)]
#[derive(Clone, Copy)]
struct UsedElem {
    id: u32,
    len: u32,
}

/// Minimal virtqueue for testing descriptor chain validation.
struct TestVirtqueue {
    size: u32,
    descriptors: Vec<Descriptor>,
    free_head: u16,
    num_free: u32,
}

impl TestVirtqueue {
    fn new(size: u32) -> Self {
        let mut descriptors = Vec::with_capacity(size as usize);

        for i in 0..size {
            descriptors.push(Descriptor {
                addr: 0,
                len: 0,
                flags: 0,
                next: if i + 1 < size {
                    (i + 1) as u16
                } else {
                    0xFFFF
                },
            });
        }

        Self {
            size,
            descriptors,
            free_head: 0,
            num_free: size,
        }
    }

    /// Mirror of kernel's free_descriptor_chain with bounds check.
    fn free_descriptor_chain(&mut self, mut idx: u16) {
        loop {
            if idx as u32 >= self.size {
                // Out-of-bounds — truncate chain.
                return;
            }

            let desc = &mut self.descriptors[idx as usize];
            let has_next = desc.flags & DESC_F_NEXT != 0;
            let next = desc.next;

            desc.flags = 0;
            desc.addr = 0;
            desc.len = 0;
            desc.next = self.free_head;
            self.free_head = idx;
            self.num_free += 1;

            if !has_next {
                break;
            }

            idx = next;
        }
    }
}

// --- Tests ---

#[test]
fn free_chain_valid_single() {
    let mut vq = TestVirtqueue::new(8);

    // Simulate: allocate descriptor 0, mark it used.
    vq.free_head = 1;
    vq.num_free = 7;
    vq.descriptors[0].flags = 0; // No NEXT flag.

    vq.free_descriptor_chain(0);

    assert_eq!(vq.num_free, 8, "should restore one descriptor");
    assert_eq!(vq.free_head, 0, "freed descriptor should be at head");
}

#[test]
fn free_chain_valid_multi() {
    let mut vq = TestVirtqueue::new(8);

    // Simulate a 3-descriptor chain: 0 → 1 → 2.
    vq.descriptors[0].flags = DESC_F_NEXT;
    vq.descriptors[0].next = 1;
    vq.descriptors[1].flags = DESC_F_NEXT;
    vq.descriptors[1].next = 2;
    vq.descriptors[2].flags = 0; // End of chain.

    vq.free_head = 3;
    vq.num_free = 5;

    vq.free_descriptor_chain(0);

    assert_eq!(vq.num_free, 8, "should restore three descriptors");
}

#[test]
fn free_chain_oob_head() {
    let mut vq = TestVirtqueue::new(8);

    let before_free = vq.num_free;

    // Out-of-bounds head descriptor (from a malicious device).
    vq.free_descriptor_chain(100);

    assert_eq!(
        vq.num_free, before_free,
        "OOB head should be rejected without freeing"
    );
}

#[test]
fn free_chain_oob_in_chain() {
    let mut vq = TestVirtqueue::new(8);

    // Chain: 0 → 255 (OOB). Should free 0 then stop.
    vq.descriptors[0].flags = DESC_F_NEXT;
    vq.descriptors[0].next = 255; // OOB for size=8.

    vq.free_head = 1;
    vq.num_free = 7;

    vq.free_descriptor_chain(0);

    // Should have freed descriptor 0 but stopped at the OOB next.
    assert_eq!(vq.num_free, 8, "should free valid prefix of chain");
}

#[test]
fn free_chain_exact_boundary() {
    let mut vq = TestVirtqueue::new(8);

    // Descriptor index = size - 1 (valid).
    vq.descriptors[7].flags = 0;
    vq.free_head = 0;
    vq.num_free = 7;

    vq.free_descriptor_chain(7);

    assert_eq!(vq.num_free, 8, "last valid index should be accepted");
}

#[test]
fn free_chain_at_boundary() {
    let mut vq = TestVirtqueue::new(8);

    // Descriptor index = size (first invalid).
    vq.free_descriptor_chain(8);

    assert_eq!(
        vq.num_free, 8,
        "index == size should be rejected"
    );
}

#[test]
fn pop_used_oob_id_skips_free() {
    // Simulates the pop_used logic: if elem.id is out of bounds,
    // we skip freeing rather than corrupting memory.
    let mut vq = TestVirtqueue::new(8);
    let initial_free = vq.num_free;

    let elem = UsedElem {
        id: 999,
        len: 512,
    };

    // Mirror pop_used's validation logic.
    if (elem.id as u32) < vq.size {
        vq.free_descriptor_chain(elem.id as u16);
    }
    // Else: skip free (descriptors leak, kernel safe).

    assert_eq!(
        vq.num_free, initial_free,
        "OOB used element should not free any descriptors"
    );
}

#[test]
fn wrapping_used_idx() {
    // Verify that wrapping used index arithmetic works correctly.
    let size: u16 = 128;
    let mut last_used_idx: u16 = u16::MAX - 2; // Near wrapping point.

    // Simulate 5 completions that wrap around u16.
    for _ in 0..5 {
        let used_idx = last_used_idx.wrapping_add(1);

        assert_ne!(last_used_idx, used_idx, "should detect new completion");

        let ring_idx = (last_used_idx % size) as usize;

        assert!(ring_idx < size as usize, "ring index must be in bounds");

        last_used_idx = used_idx;
    }

    // Verify we wrapped around u16.
    assert!(
        last_used_idx < 5,
        "should have wrapped past u16::MAX"
    );
}
