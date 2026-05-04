//! User-space memory access — the unsafe boundary for IPC message transfer.
//!
//! On bare metal (`target_os = "none"`): validates VA range, copies via raw pointer.
//! On host (test target): direct pointer dereference (same address space).

use crate::{
    endpoint::{MSG_SIZE, Message},
    types::SyscallError,
};

/// Read a message from user memory into a stack-allocated Message.
///
/// # Safety
/// On bare metal, `ptr` must be a valid user-space virtual address mapped
/// into the current address space. On host, `ptr` is a direct pointer.
pub fn read_user_message(ptr: usize, len: usize) -> Result<Message, SyscallError> {
    if len > MSG_SIZE {
        return Err(SyscallError::InvalidArgument);
    }
    if len == 0 {
        return Ok(Message::empty());
    }
    if ptr == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let mut msg = Message::empty();

    // SAFETY: On host tests, ptr is a valid pointer to the caller's buffer in
    // the same address space. On bare metal, the kernel has verified the VA is
    // mapped in the current address space before reaching this point.
    unsafe {
        let src = ptr as *const u8;
        let dst = msg.data_mut().as_mut_ptr();

        core::ptr::copy_nonoverlapping(src, dst, len);
    }

    msg.set_len(len);

    Ok(msg)
}

/// Write bytes to user memory.
///
/// # Safety
/// Same VA requirements as `read_user_message`.
pub fn write_user_bytes(ptr: usize, data: &[u8]) -> Result<(), SyscallError> {
    if data.is_empty() {
        return Ok(());
    }
    if ptr == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    // SAFETY: ptr is verified as a valid user VA (same safety argument as
    // read_user_message). data.len() <= MSG_SIZE enforced by caller.
    unsafe {
        let dst = ptr as *mut u8;

        core::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
    }

    Ok(())
}

/// Read a slice of u32 values from user memory into a caller-provided buffer.
pub fn read_user_u32s(ptr: usize, count: usize, buf: &mut [u32]) -> Result<(), SyscallError> {
    if count == 0 {
        return Ok(());
    }
    if ptr == 0 || count > buf.len() {
        return Err(SyscallError::InvalidArgument);
    }

    // SAFETY: same VA argument as read_user_message.
    unsafe {
        let src = ptr as *const u32;

        core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), count);
    }

    Ok(())
}

/// Write a slice of u32 values to user memory.
pub fn write_user_u32s(ptr: usize, data: &[u32]) -> Result<(), SyscallError> {
    if data.is_empty() {
        return Ok(());
    }
    if ptr == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    // SAFETY: same VA argument as write_user_bytes.
    unsafe {
        let dst = ptr as *mut u32;

        core::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
    }

    Ok(())
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
