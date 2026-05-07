//! IPC syscall wrappers — call/recv/reply and endpoint management.

use crate::{
    raw::{self, check, num},
    types::{Handle, SyscallError},
};

pub fn endpoint_create() -> Result<Handle, SyscallError> {
    check(raw::syscall(num::ENDPOINT_CREATE, 0, 0, 0, 0, 0, 0)).map(|v| Handle(v as u32))
}

pub struct CallResult {
    pub reply_len: usize,
    pub handle_count: usize,
}

pub fn call(
    endpoint: Handle,
    msg_buf: &mut [u8],
    msg_len: usize,
    handles: &[u32],
    recv_handles: &mut [u32],
) -> Result<CallResult, SyscallError> {
    let recv_ptr = if recv_handles.is_empty() {
        0u64
    } else {
        recv_handles.as_mut_ptr() as u64
    };
    let r = check(raw::syscall(
        num::CALL,
        endpoint.0 as u64,
        msg_buf.as_mut_ptr() as u64,
        msg_len as u64,
        handles.as_ptr() as u64,
        handles.len() as u64,
        recv_ptr,
    ))?;

    Ok(CallResult {
        reply_len: 0,
        handle_count: r as usize,
    })
}

pub struct RecvResult {
    pub reply_cap: u32,
    pub badge: u32,
    pub msg_len: usize,
    pub handle_count: usize,
}

pub fn recv(
    endpoint: Handle,
    msg_buf: &mut [u8],
    handles_buf: &mut [u32],
) -> Result<RecvResult, SyscallError> {
    let mut reply_cap_val: u64 = 0;

    let packed = check(raw::syscall(
        num::RECV,
        endpoint.0 as u64,
        msg_buf.as_mut_ptr() as u64,
        msg_buf.len() as u64,
        handles_buf.as_mut_ptr() as u64,
        handles_buf.len() as u64,
        &mut reply_cap_val as *mut u64 as u64,
    ))?;

    Ok(RecvResult {
        reply_cap: reply_cap_val as u32,
        badge: (packed >> 32) as u32,
        handle_count: ((packed >> 16) & 0xFFFF) as usize,
        msg_len: (packed & 0xFFFF) as usize,
    })
}

pub fn reply(
    endpoint: Handle,
    reply_cap: u32,
    msg: &[u8],
    handles: &[u32],
) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::REPLY,
        endpoint.0 as u64,
        reply_cap as u64,
        msg.as_ptr() as u64,
        msg.len() as u64,
        handles.as_ptr() as u64,
        handles.len() as u64,
    ))
    .map(|_| ())
}

pub fn endpoint_bind_event(endpoint: Handle, event: Handle) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::ENDPOINT_BIND_EVENT,
        endpoint.0 as u64,
        event.0 as u64,
        0,
        0,
        0,
        0,
    ))
    .map(|_| ())
}
