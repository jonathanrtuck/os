# Syscall Quick Reference

35 syscalls via `SVC #0`. Registers: `x8` = syscall number, `x0`–`x5` =
arguments, returns `x0` = error (0 = success), `x1` = value. All caller-saved
registers are clobbered.

Canonical source: `user/abi/src/raw.rs` (numbers), `kernel/src/syscall.rs`
(dispatch).

## Error codes

| Code | Name               | Meaning                             |
| ---- | ------------------ | ----------------------------------- |
| 0    | (success)          | No error                            |
| 1    | InvalidHandle      | Handle doesn't exist or wrong space |
| 2    | WrongHandleType    | Handle exists but wrong object type |
| 3    | InsufficientRights | Handle lacks required rights        |
| 4    | OutOfMemory        | Kernel allocation failed            |
| 5    | InvalidArgument    | Bad parameter value                 |
| 6    | PeerClosed         | Other end of endpoint gone          |
| 7    | TimedOut           | Deadline elapsed before event       |
| 8    | BufferFull         | Message or handle buffer too small  |
| 9    | WouldDeadlock      | Call on own endpoint                |
| 10   | AlreadySealed      | VMO is sealed, mutation rejected    |
| 11   | GenerationMismatch | Stale generation in pager response  |
| 12   | NotFound           | Lookup failed (name service, etc.)  |

## Rights

| Bit | Name     | Value |
| --- | -------- | ----- |
| 0   | READ     | 0x001 |
| 1   | WRITE    | 0x002 |
| 2   | EXECUTE  | 0x004 |
| 3   | MAP      | 0x008 |
| 4   | DUP      | 0x010 |
| 5   | TRANSFER | 0x020 |
| 6   | SIGNAL   | 0x040 |
| 7   | WAIT     | 0x080 |
| 8   | SPAWN    | 0x100 |

## Object types

| Code | Type         |
| ---- | ------------ |
| 0    | VMO          |
| 1    | Endpoint     |
| 2    | Event        |
| 3    | Thread       |
| 4    | AddressSpace |
| 5    | Resource     |

## Syscall table

### VMO (0–8)

| #   | Name          | Args                                  | Returns     |
| --- | ------------- | ------------------------------------- | ----------- |
| 0   | VMO_CREATE    | size, flags, resource_handle (if DMA) | handle      |
| 1   | VMO_MAP       | handle, addr_hint, perms              | mapped_addr |
| 2   | VMO_MAP_INTO  | vmo, space, addr, perms               | mapped_addr |
| 3   | VMO_UNMAP     | addr                                  | —           |
| 4   | VMO_SNAPSHOT  | handle                                | new_handle  |
| 5   | VMO_SEAL      | handle                                | —           |
| 6   | VMO_RESIZE    | handle, new_size                      | —           |
| 7   | VMO_SET_PAGER | vmo, endpoint                         | —           |
| 8   | VMO_INFO      | handle                                | size        |

Flags for VMO_CREATE: `FLAG_DMA = 1 << 2` (requires Resource handle in x2).

### Endpoint / IPC (9–14)

| #   | Name                | Args                                                                   | Returns                     |
| --- | ------------------- | ---------------------------------------------------------------------- | --------------------------- |
| 9   | ENDPOINT_CREATE     | —                                                                      | handle                      |
| 10  | CALL                | endpoint, msg_buf, msg_len, handles_ptr, handles_len, recv_handles_ptr | handle_count                |
| 11  | RECV                | endpoint, msg_buf, buf_len, handles_buf, handles_cap, reply_cap_ptr    | packed(badge, hcount, mlen) |
| 12  | REPLY               | endpoint, reply_cap, msg_ptr, msg_len, handles_ptr, handles_len        | —                           |
| 13  | ENDPOINT_BIND_EVENT | endpoint, event                                                        | —                           |
| 14  | RECV_TIMED          | endpoint, msg_buf, buf_len, handles_buf, handles_cap, extra_ptr        | packed(badge, hcount, mlen) |

RECV packed return: `badge << 32 | handle_count << 16 | msg_len`. RECV_TIMED
extra_ptr points to `[reply_cap_ptr, deadline_tick]`.

### Event (15–21)

| #   | Name                | Args                            | Returns         |
| --- | ------------------- | ------------------------------- | --------------- |
| 15  | EVENT_CREATE        | —                               | handle          |
| 16  | EVENT_SIGNAL        | handle, bits                    | —               |
| 17  | EVENT_WAIT          | h0, mask0, h1, mask1, h2, mask2 | signaled_handle |
| 18  | EVENT_CLEAR         | handle, bits                    | —               |
| 19  | EVENT_BIND_IRQ      | event, intid, bits              | —               |
| 20  | EVENT_WAIT_DEADLINE | handle, mask, deadline_tick     | signaled_handle |
| 21  | EVENT_BIND_THREAD   | event, thread                   | —               |

EVENT_WAIT multiplexes up to 3 events via register pairs. deadline_tick = 0
means infinite wait. EVENT_BIND_THREAD registers a thread death notification on
an event object.

### Thread (22–27)

| #   | Name                | Args                                                   | Returns    |
| --- | ------------------- | ------------------------------------------------------ | ---------- |
| 22  | THREAD_CREATE       | entry, stack_top, arg                                  | handle     |
| 23  | THREAD_CREATE_IN    | space, entry, stack_top, arg, handles_ptr, handles_len | handle     |
| 24  | THREAD_EXIT         | code                                                   | (noreturn) |
| 25  | THREAD_SET_PRIORITY | handle, priority                                       | —          |
| 26  | THREAD_SET_AFFINITY | handle, hint                                           | —          |
| 27  | THREAD_YIELD        | —                                                      | —          |

Priority: 0=Idle, 1=Low, 2=Medium, 3=High.

### Address Space (28–29)

| #   | Name          | Args   | Returns |
| --- | ------------- | ------ | ------- |
| 28  | SPACE_CREATE  | —      | handle  |
| 29  | SPACE_DESTROY | handle | —       |

### Handle (30–32)

| #   | Name         | Args           | Returns                     |
| --- | ------------ | -------------- | --------------------------- |
| 30  | HANDLE_DUP   | handle, rights | new_handle                  |
| 31  | HANDLE_CLOSE | handle         | —                           |
| 32  | HANDLE_INFO  | handle         | packed(object_type, rights) |

HANDLE_INFO packed return: `object_type << 32 | rights`.

### System (33–34)

| #   | Name        | Args | Returns       |
| --- | ----------- | ---- | ------------- |
| 33  | CLOCK_READ  | —    | counter_ticks |
| 34  | SYSTEM_INFO | key  | value         |

SYSTEM_INFO keys: 0=PAGE_SIZE, 1=MSG_SIZE, 2=NUM_CORES. Timer frequency: 24 MHz.
`ticks / 24_000_000 = seconds`.
