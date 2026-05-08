//! Client framework — typed call over an endpoint.
//!
//! The `call` syscall supports receiving handles on reply: pass a mutable
//! slice for the server's reply handles. The returned `Reply` includes
//! `handle_count` indicating how many entries were written. Use
//! `call_simple` when no handle transfer is needed.

use abi::types::{Handle, Rights, SyscallError};

use crate::message::{self, Header, MAX_PAYLOAD, MSG_SIZE};

pub struct Reply<'a> {
    pub method: u32,
    pub status: u16,
    pub payload: &'a [u8],
    pub handle_count: usize,
}

impl Reply<'_> {
    pub fn is_error(&self) -> bool {
        self.status != 0
    }
}

pub fn call<'a>(
    endpoint: Handle,
    method: u32,
    data: &[u8],
    send_handles: &[u32],
    recv_handles: &mut [u32],
    reply_buf: &'a mut [u8; MSG_SIZE],
) -> Result<Reply<'a>, SyscallError> {
    let msg_len = message::write_request(reply_buf, method, data);
    let result = abi::ipc::call(endpoint, reply_buf, msg_len, send_handles, recv_handles)?;
    let header = Header::read_from(reply_buf);
    let payload_end = message::HEADER_SIZE + (header.len as usize).min(MAX_PAYLOAD);

    Ok(Reply {
        method: header.method,
        status: header.status,
        payload: &reply_buf[message::HEADER_SIZE..payload_end],
        handle_count: result.handle_count,
    })
}

/// Call a service's SETUP method, receive a VMO handle, and map it.
///
/// Sends `send_handles` (if any) along with the request. On success,
/// maps the received VMO with `map_rights` and returns the VA.
pub fn setup_map_vmo(
    endpoint: Handle,
    method: u32,
    send_handles: &[u32],
    map_rights: Rights,
) -> Result<usize, SyscallError> {
    let mut buf = [0u8; MSG_SIZE];
    let mut recv_handles = [0u32; 4];
    let reply = call(
        endpoint,
        method,
        &[],
        send_handles,
        &mut recv_handles,
        &mut buf,
    )?;

    if reply.is_error() || reply.handle_count == 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let vmo = Handle(recv_handles[0]);

    abi::vmo::map(vmo, 0, map_rights)
}

pub fn call_simple(
    endpoint: Handle,
    method: u32,
    data: &[u8],
) -> Result<(u16, [u8; MAX_PAYLOAD]), SyscallError> {
    let mut buf = [0u8; MSG_SIZE];
    let reply = call(endpoint, method, data, &[], &mut [], &mut buf)?;
    let status = reply.status;
    let mut out = [0u8; MAX_PAYLOAD];
    let len = reply.payload.len().min(MAX_PAYLOAD);

    out[..len].copy_from_slice(&reply.payload[..len]);

    Ok((status, out))
}
