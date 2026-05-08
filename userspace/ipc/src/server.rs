//! Server framework — recv/dispatch/reply loop over a typed endpoint.

use abi::types::{Handle, SyscallError};

use crate::message::{self, Header, MAX_PAYLOAD, MSG_SIZE};

pub const MAX_HANDLES: usize = 4;

pub struct Incoming<'a> {
    pub method: u32,
    pub payload: &'a [u8],
    pub handles: &'a [u32],
    reply_cap: u32,
    endpoint: Handle,
}

impl<'a> Incoming<'a> {
    pub fn reply_ok(self, data: &[u8], handles: &[u32]) -> Result<(), SyscallError> {
        let mut buf = [0u8; MSG_SIZE];

        message::write_reply(&mut buf, self.method, data);

        let len = message::HEADER_SIZE + data.len().min(MAX_PAYLOAD);

        abi::ipc::reply(self.endpoint, self.reply_cap, &buf[..len], handles)
    }

    pub fn reply_error(self, status: u16) -> Result<(), SyscallError> {
        let mut buf = [0u8; MSG_SIZE];

        message::write_error(&mut buf, self.method, status);

        abi::ipc::reply(
            self.endpoint,
            self.reply_cap,
            &buf[..message::HEADER_SIZE],
            &[],
        )
    }

    pub fn reply_empty(self) -> Result<(), SyscallError> {
        self.reply_ok(&[], &[])
    }

    pub fn defer(self) -> DeferredReply {
        DeferredReply {
            endpoint: self.endpoint,
            reply_cap: self.reply_cap,
            method: self.method,
        }
    }
}

pub struct DeferredReply {
    pub endpoint: Handle,
    pub reply_cap: u32,
    pub method: u32,
}

impl DeferredReply {
    pub fn reply_ok(&self, data: &[u8], handles: &[u32]) -> Result<(), SyscallError> {
        let mut buf = [0u8; MSG_SIZE];

        message::write_reply(&mut buf, self.method, data);

        let len = message::HEADER_SIZE + data.len().min(MAX_PAYLOAD);

        abi::ipc::reply(self.endpoint, self.reply_cap, &buf[..len], handles)
    }

    pub fn reply_error(&self, status: u16) -> Result<(), SyscallError> {
        let mut buf = [0u8; MSG_SIZE];

        message::write_error(&mut buf, self.method, status);

        abi::ipc::reply(
            self.endpoint,
            self.reply_cap,
            &buf[..message::HEADER_SIZE],
            &[],
        )
    }
}

pub trait Dispatch {
    fn dispatch(&mut self, msg: Incoming<'_>);
}

pub fn serve_one(endpoint: Handle, handler: &mut impl Dispatch) -> Result<(), SyscallError> {
    let mut buf = [0u8; MSG_SIZE];
    let mut handle_buf = [0u32; MAX_HANDLES];
    let recv = abi::ipc::recv(endpoint, &mut buf, &mut handle_buf)?;
    let header = Header::read_from(&buf);
    let raw_payload_len = recv.msg_len.saturating_sub(message::HEADER_SIZE);
    let payload_end = message::HEADER_SIZE + (header.len as usize).min(raw_payload_len);
    let payload = &buf[message::HEADER_SIZE..payload_end];
    let handles = &handle_buf[..recv.handle_count];
    let msg = Incoming {
        method: header.method,
        payload,
        handles,
        reply_cap: recv.reply_cap,
        endpoint,
    };

    handler.dispatch(msg);

    Ok(())
}

pub fn serve_one_timed(
    endpoint: Handle,
    handler: &mut impl Dispatch,
    deadline_ns: u64,
) -> Result<(), SyscallError> {
    let mut buf = [0u8; MSG_SIZE];
    let mut handle_buf = [0u32; MAX_HANDLES];
    let recv = abi::ipc::recv_timed(endpoint, &mut buf, &mut handle_buf, deadline_ns)?;
    let header = Header::read_from(&buf);
    let raw_payload_len = recv.msg_len.saturating_sub(message::HEADER_SIZE);
    let payload_end = message::HEADER_SIZE + (header.len as usize).min(raw_payload_len);
    let payload = &buf[message::HEADER_SIZE..payload_end];
    let handles = &handle_buf[..recv.handle_count];
    let msg = Incoming {
        method: header.method,
        payload,
        handles,
        reply_cap: recv.reply_cap,
        endpoint,
    };

    handler.dispatch(msg);

    Ok(())
}

pub fn serve(endpoint: Handle, handler: &mut impl Dispatch) -> SyscallError {
    let mut buf = [0u8; MSG_SIZE];
    let mut handle_buf = [0u32; MAX_HANDLES];

    loop {
        let recv = match abi::ipc::recv(endpoint, &mut buf, &mut handle_buf) {
            Ok(r) => r,
            Err(e) => return e,
        };
        let header = Header::read_from(&buf);
        let raw_payload_len = recv.msg_len.saturating_sub(message::HEADER_SIZE);
        let payload_end = message::HEADER_SIZE + (header.len as usize).min(raw_payload_len);
        let payload = &buf[message::HEADER_SIZE..payload_end];
        let handles = &handle_buf[..recv.handle_count];
        let msg = Incoming {
            method: header.method,
            payload,
            handles,
            reply_cap: recv.reply_cap,
            endpoint,
        };

        handler.dispatch(msg);
    }
}
