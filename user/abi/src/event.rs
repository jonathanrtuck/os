//! Event syscall wrappers.

use crate::{
    raw::{self, check, num},
    types::{Handle, SyscallError},
};

pub fn create() -> Result<Handle, SyscallError> {
    check(raw::syscall(num::EVENT_CREATE, 0, 0, 0, 0, 0, 0)).map(|v| Handle(v as u32))
}

pub fn signal(handle: Handle, bits: u64) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::EVENT_SIGNAL,
        handle.0 as u64,
        bits,
        0,
        0,
        0,
        0,
    ))
    .map(|_| ())
}

pub fn wait(items: &[(Handle, u64)]) -> Result<Handle, SyscallError> {
    let mut args = [0u64; 6];

    for (i, &(h, mask)) in items.iter().take(3).enumerate() {
        args[i * 2] = h.0 as u64;
        args[i * 2 + 1] = mask;
    }

    let handle_id = check(raw::syscall(
        num::EVENT_WAIT,
        args[0],
        args[1],
        args[2],
        args[3],
        args[4],
        args[5],
    ))?;

    Ok(Handle(handle_id as u32))
}

pub fn clear(handle: Handle, bits: u64) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::EVENT_CLEAR,
        handle.0 as u64,
        bits,
        0,
        0,
        0,
        0,
    ))
    .map(|_| ())
}

/// Wait on a single event with a deadline. Returns the event handle on
/// signal, or `Err(TimedOut)` if `deadline_tick` passes first.
/// `deadline_tick` is an absolute counter value from `clock_read`.
/// Pass 0 for infinite wait.
pub fn wait_deadline(
    handle: Handle,
    mask: u64,
    deadline_tick: u64,
) -> Result<Handle, SyscallError> {
    let handle_id = check(raw::syscall(
        num::EVENT_WAIT_DEADLINE,
        handle.0 as u64,
        mask,
        deadline_tick,
        0,
        0,
        0,
    ))?;

    Ok(Handle(handle_id as u32))
}

pub fn bind_irq(event: Handle, intid: u32, bits: u64) -> Result<(), SyscallError> {
    check(raw::syscall(
        num::EVENT_BIND_IRQ,
        event.0 as u64,
        intid as u64,
        bits,
        0,
        0,
        0,
    ))
    .map(|_| ())
}
