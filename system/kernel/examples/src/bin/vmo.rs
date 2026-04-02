//! Virtual Memory Objects — create, map, read, write.
//!
//! Demonstrates:
//! - VMO creation (syscall 16) — allocate a kernel-managed memory object
//! - VMO mapping (syscall 17) — map it into the process address space
//! - VMO read/write (syscalls 19, 20) — transfer data via handle (no mapping)
//! - Direct memory access via the mapped region
//!
//! VMOs are the kernel's primary memory abstraction. They support:
//! - Lazy allocation (demand-paged)
//! - COW snapshots (for undo/versioning)
//! - Sealing (immutable after seal)
//! - Content type tags (user-defined)
//! - Pager interface (for memory-mapped files)
//!
//! Build:
//!   cd kernel/examples && cargo build --release --bin vmo
//!
//! Run with kernel:
//!   cd kernel && OS_INIT_ELF=examples/target/aarch64-unknown-none/release/vmo \
//!     cargo build --release
//!   hypervisor target/aarch64-unknown-none/release/kernel

#![no_std]
#![no_main]

use kernel_examples::{
    exit, handle_close, print, print_hex, print_u64, unwrap_or_exit, vmo_create, vmo_map, vmo_read,
    vmo_write,
};

/// VMO map flags.
const MAP_READ: u64 = 1;
const MAP_WRITE: u64 = 2;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print(b"=== VMO Example ===\n\n");

    // --- Step 1: Create a VMO (1 page = 16 KiB) ---
    let type_tag = 0x4558414D; // "EXAM" as a content type tag
    let vmo = unwrap_or_exit(vmo_create(1, 0, type_tag), b"vmo_create");

    print(b"Created VMO: handle ");
    print_u64(vmo as u64);
    print(b", type_tag ");
    print_hex(type_tag);
    print(b"\n");

    // --- Step 2: Write data via handle (no mapping needed) ---
    let message = b"Hello from VMO handle write!";
    let written = unwrap_or_exit(vmo_write(vmo, 0, message), b"vmo_write");

    print(b"Wrote ");
    print_u64(written);
    print(b" bytes via handle\n");

    // --- Step 3: Read back via handle ---
    let mut buf = [0u8; 64];
    let read = unwrap_or_exit(vmo_read(vmo, 0, &mut buf[..message.len()]), b"vmo_read");

    print(b"Read ");
    print_u64(read);
    print(b" bytes via handle: \"");
    print(&buf[..read as usize]);
    print(b"\"\n\n");

    // --- Step 4: Map the VMO into our address space ---
    let mapped = unwrap_or_exit(vmo_map(vmo, MAP_READ | MAP_WRITE), b"vmo_map");

    print(b"Mapped VMO at ");
    print_hex(mapped as u64);
    print(b"\n");

    // Read the data we wrote earlier — it's visible through the mapping.
    let mapped_slice = unsafe { core::slice::from_raw_parts(mapped, message.len()) };

    print(b"Read via mapping: \"");
    print(mapped_slice);
    print(b"\"\n");

    // Write new data through the mapping.
    let new_msg = b"Written via direct memory!";

    // SAFETY: mapped points to a valid RW page from vmo_map.
    unsafe {
        core::ptr::copy_nonoverlapping(new_msg.as_ptr(), mapped, new_msg.len());
    }

    // Verify by reading back through the handle.
    let mut verify = [0u8; 64];
    let n = unwrap_or_exit(vmo_read(vmo, 0, &mut verify[..new_msg.len()]), b"vmo_read");

    print(b"Verified via handle: \"");
    print(&verify[..n as usize]);
    print(b"\"\n");

    // --- Cleanup ---
    unwrap_or_exit(handle_close(vmo), b"handle_close");

    print(b"\nVMO closed. Done.\n");

    exit()
}
