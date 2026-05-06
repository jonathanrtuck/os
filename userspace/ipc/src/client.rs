//! Client framework — typed call over an endpoint.
//!
//! Note: the kernel's `call` syscall does not expose reply handle IDs
//! to the caller. If a server transfers handles in its reply, they are
//! installed into the caller's handle table but the caller must learn
//! the IDs through the reply payload (e.g., the server includes them
//! in the message). For explicit handle-receiving, use `recv`/`reply`
//! directly via the `server` module or the raw ABI.

use abi::types::{Handle, SyscallError};

use crate::message::{self, Header, MAX_PAYLOAD, MSG_SIZE};

pub struct Reply<'a> {
    pub method: u32,
    pub status: u16,
    pub payload: &'a [u8],
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
    reply_buf: &'a mut [u8; MSG_SIZE],
) -> Result<Reply<'a>, SyscallError> {
    let msg_len = message::write_request(reply_buf, method, data);

    abi::ipc::call(endpoint, reply_buf, msg_len, send_handles)?;

    let header = Header::read_from(reply_buf);
    let payload_end = message::HEADER_SIZE + (header.len as usize).min(MAX_PAYLOAD);

    Ok(Reply {
        method: header.method,
        status: header.status,
        payload: &reply_buf[message::HEADER_SIZE..payload_end],
    })
}

pub fn call_simple(
    endpoint: Handle,
    method: u32,
    data: &[u8],
) -> Result<(u16, [u8; MAX_PAYLOAD]), SyscallError> {
    let mut buf = [0u8; MSG_SIZE];
    let reply = call(endpoint, method, data, &[], &mut buf)?;
    let status = reply.status;
    let mut out = [0u8; MAX_PAYLOAD];
    let len = reply.payload.len().min(MAX_PAYLOAD);

    out[..len].copy_from_slice(&reply.payload[..len]);

    Ok((status, out))
}
