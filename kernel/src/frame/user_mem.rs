//! User-space memory access — the unsafe boundary for IPC message transfer.
//!
//! On bare metal: LDTR/STTR instructions perform loads/stores using EL0's
//! page table, with fault recovery via the data abort handler. If the user
//! address is unmapped or has wrong permissions, the copy returns an error
//! instead of crashing the kernel.
//!
//! On host (test target): direct pointer dereference (same address space).

use crate::{
    endpoint::{MSG_SIZE, Message},
    types::SyscallError,
};

#[cfg(target_os = "none")]
const USER_VA_END: usize = 1 << 36;

fn validate_user_range(ptr: usize, len: usize) -> Result<(), SyscallError> {
    if ptr == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    if ptr.checked_add(len).is_none() {
        return Err(SyscallError::InvalidArgument);
    }
    #[cfg(target_os = "none")]
    if ptr + len > USER_VA_END {
        return Err(SyscallError::InvalidArgument);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Bare-metal: LDTR/STTR with fault recovery
// ---------------------------------------------------------------------------

#[cfg(target_os = "none")]
fn copy_from_user(dst: &mut [u8], src_va: usize) -> Result<(), SyscallError> {
    use super::arch::exception::COPY_FAULT_RECOVERY;

    let len = dst.len();
    let fault: u64;

    // SAFETY: LDTR performs the load using EL0's translation regime. If the
    // address is unmapped or lacks read permission, a data abort (EC 0x25)
    // occurs at EL1. The handler checks COPY_FAULT_RECOVERY and, if set,
    // jumps to the recovery label (sets x0=1 in the TrapFrame). The `adr`
    // instruction computes the recovery label address without accessing
    // memory — safe with nomem omitted. `str` to COPY_FAULT_RECOVERY is a
    // kernel VA write (identity-mapped), not a user VA access.
    unsafe {
        core::arch::asm!(
            "adr {recovery}, 22f",
            "str {recovery}, [{flag}]",
            "11:",
            "cmp {len}, #8",
            "b.lt 13f",
            "ldtr {tmp}, [{src}]",
            "str {tmp}, [{dst}]",
            "add {src}, {src}, #8",
            "add {dst}, {dst}, #8",
            "sub {len}, {len}, #8",
            "b 11b",
            "13:",
            "cbz {len}, 14f",
            "15:",
            "ldtrb {tmp:w}, [{src}]",
            "strb {tmp:w}, [{dst}]",
            "add {src}, {src}, #1",
            "add {dst}, {dst}, #1",
            "sub {len}, {len}, #1",
            "cbnz {len}, 15b",
            "14:",
            "str xzr, [{flag}]",
            "mov {fault}, #0",
            "b 16f",
            "22:",
            "str xzr, [{flag}]",
            "mov {fault}, #1",
            "16:",
            src = inout(reg) src_va => _,
            dst = inout(reg) dst.as_mut_ptr() => _,
            len = inout(reg) len => _,
            tmp = out(reg) _,
            fault = lateout(reg) fault,
            recovery = out(reg) _,
            flag = in(reg) COPY_FAULT_RECOVERY.as_ptr(),
            options(nostack),
        );
    }

    if fault != 0 {
        Err(SyscallError::InvalidArgument)
    } else {
        Ok(())
    }
}

#[cfg(target_os = "none")]
fn copy_to_user(dst_va: usize, src: &[u8]) -> Result<(), SyscallError> {
    use super::arch::exception::COPY_FAULT_RECOVERY;

    let len = src.len();
    let fault: u64;

    // SAFETY: STTR performs the store using EL0's translation regime. Same
    // fault recovery mechanism as copy_from_user.
    unsafe {
        core::arch::asm!(
            "adr {recovery}, 22f",
            "str {recovery}, [{flag}]",
            "11:",
            "cmp {len}, #8",
            "b.lt 13f",
            "ldr {tmp}, [{src}]",
            "sttr {tmp}, [{dst}]",
            "add {src}, {src}, #8",
            "add {dst}, {dst}, #8",
            "sub {len}, {len}, #8",
            "b 11b",
            "13:",
            "cbz {len}, 14f",
            "15:",
            "ldrb {tmp:w}, [{src}]",
            "sttrb {tmp:w}, [{dst}]",
            "add {src}, {src}, #1",
            "add {dst}, {dst}, #1",
            "sub {len}, {len}, #1",
            "cbnz {len}, 15b",
            "14:",
            "str xzr, [{flag}]",
            "mov {fault}, #0",
            "b 16f",
            "22:",
            "str xzr, [{flag}]",
            "mov {fault}, #1",
            "16:",
            dst = inout(reg) dst_va => _,
            src = inout(reg) src.as_ptr() => _,
            len = inout(reg) len => _,
            tmp = out(reg) _,
            fault = lateout(reg) fault,
            recovery = out(reg) _,
            flag = in(reg) COPY_FAULT_RECOVERY.as_ptr(),
            options(nostack),
        );
    }

    if fault != 0 {
        Err(SyscallError::InvalidArgument)
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Host (test): direct pointer copy
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "none"))]
fn copy_from_user(dst: &mut [u8], src_va: usize) -> Result<(), SyscallError> {
    // SAFETY: On host tests, src_va is a pointer in the same address space.
    unsafe {
        core::ptr::copy_nonoverlapping(src_va as *const u8, dst.as_mut_ptr(), dst.len());
    }

    Ok(())
}

#[cfg(not(target_os = "none"))]
fn copy_to_user(dst_va: usize, src: &[u8]) -> Result<(), SyscallError> {
    // SAFETY: On host tests, dst_va is a pointer in the same address space.
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst_va as *mut u8, src.len());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Public API — delegates to copy_from_user/copy_to_user
// ---------------------------------------------------------------------------

pub fn read_user_message(ptr: usize, len: usize) -> Result<Message, SyscallError> {
    if len > MSG_SIZE {
        return Err(SyscallError::InvalidArgument);
    }
    if len == 0 {
        return Ok(Message::empty());
    }

    validate_user_range(ptr, len)?;

    let mut msg = Message::empty();

    copy_from_user(&mut msg.data_mut()[..len], ptr)?;
    msg.set_len(len);

    Ok(msg)
}

pub fn write_user_bytes(ptr: usize, data: &[u8]) -> Result<(), SyscallError> {
    if data.is_empty() {
        return Ok(());
    }

    validate_user_range(ptr, data.len())?;
    copy_to_user(ptr, data)
}

pub fn read_user_u32s(ptr: usize, count: usize, buf: &mut [u32]) -> Result<(), SyscallError> {
    if count == 0 {
        return Ok(());
    }
    if count > buf.len() {
        return Err(SyscallError::InvalidArgument);
    }

    let byte_len = count * core::mem::size_of::<u32>();

    validate_user_range(ptr, byte_len)?;

    // SAFETY: buf[..count] is valid for byte_len bytes, properly aligned.
    let dst_bytes = unsafe {
        core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, byte_len)
    };

    copy_from_user(dst_bytes, ptr)
}

pub fn write_user_u32s(ptr: usize, data: &[u32]) -> Result<(), SyscallError> {
    if data.is_empty() {
        return Ok(());
    }

    validate_user_range(ptr, core::mem::size_of_val(data))?;

    // SAFETY: data is a valid slice of u32, reinterpreted as bytes.
    let src_bytes = unsafe {
        core::slice::from_raw_parts(data.as_ptr() as *const u8, core::mem::size_of_val(data))
    };

    copy_to_user(ptr, src_bytes)
}

/// Write data to a physical address. Bare-metal only.
#[cfg(target_os = "none")]
pub fn write_phys(pa: usize, offset: usize, data: &[u8]) {
    // SAFETY: pa is a physical address returned by page_alloc::alloc_page.
    // With identity-mapped kernel memory, PA == VA for RAM pages.
    unsafe {
        let dst = (pa + offset) as *mut u8;

        core::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
    }
}

/// Zero-fill a physical page. Bare-metal only.
#[cfg(target_os = "none")]
pub fn zero_phys(pa: usize, len: usize) {
    // SAFETY: same identity-map argument as write_phys.
    unsafe {
        let dst = pa as *mut u8;

        core::ptr::write_bytes(dst, 0, len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_roundtrip() {
        let src_data = b"hello kernel";
        let msg = read_user_message(src_data.as_ptr() as usize, src_data.len()).unwrap();

        assert_eq!(msg.as_bytes(), src_data);

        let mut out_buf = [0u8; MSG_SIZE];

        write_user_bytes(out_buf.as_mut_ptr() as usize, msg.as_bytes()).unwrap();

        assert_eq!(&out_buf[..src_data.len()], src_data);
    }

    #[test]
    fn read_empty_message() {
        let msg = read_user_message(0, 0).unwrap();

        assert_eq!(msg.as_bytes().len(), 0);
    }

    #[test]
    fn read_oversized_rejected() {
        let big = [0u8; MSG_SIZE + 1];

        assert_eq!(
            read_user_message(big.as_ptr() as usize, big.len()),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn write_empty_succeeds() {
        assert!(write_user_bytes(0, &[]).is_ok());
    }

    #[test]
    fn null_ptr_with_nonzero_len_rejected() {
        assert_eq!(read_user_message(0, 10), Err(SyscallError::InvalidArgument));
        assert_eq!(
            write_user_bytes(0, &[1, 2, 3]),
            Err(SyscallError::InvalidArgument)
        );
    }
}
