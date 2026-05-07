//! Init — the first userspace process.
//!
//! Launched by the kernel bootstrap. Receives bootstrap handles at
//! well-known indices:
//!   Handle 0: own address space
//!   Handle 1: code VMO
//!   Handle 2: service pack VMO (SVPK format)
//!
//! Parses the service pack, creates a name service endpoint, and spawns
//! all services. The name service gets the endpoint to recv on; other
//! services get it for register/lookup calls.

#![no_std]
#![no_main]

mod pack;

use core::panic::PanicInfo;

use abi::types::{Handle, Rights, SyscallError};

const _HANDLE_SPACE: Handle = Handle(0);
const _HANDLE_CODE_VMO: Handle = Handle(1);
const HANDLE_PACK_VMO: Handle = Handle(2);

const PAGE_SIZE: usize = 16384;
const STACK_SIZE: usize = PAGE_SIZE * 4;

const SERVICE_CODE_VA: usize = 0x0020_0000;
const SERVICE_STACK_VA: usize = 0x4000_0000;

const NAME_SVC: &[u8] = b"name";

fn spawn_from_pack(pack_base: *const u8, pack_len: usize) {
    let header = pack::read_header(pack_base);

    if !header.is_valid() {
        return;
    }

    // Create the name service endpoint. The name service recvs on it;
    // all other services call through it.
    let ns_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => return,
    };

    // First pass: spawn the name service (entry named "name").
    for i in 0..header.count as usize {
        let entry = pack::read_entry(pack_base, i);
        let name = pack::read_name(pack_base, i);

        if entry.size == 0 {
            continue;
        }

        if name == NAME_SVC {
            // Name service gets handle[2] = the endpoint to recv on.
            let _ = spawn_service(pack_base, &entry, pack_len, ns_ep);
        }
    }

    // Second pass: spawn all other services with handle[2] = name service ep.
    for i in 0..header.count as usize {
        let entry = pack::read_entry(pack_base, i);
        let name = pack::read_name(pack_base, i);

        if entry.size == 0 || name == NAME_SVC {
            continue;
        }

        let _ = spawn_service(pack_base, &entry, pack_len, ns_ep);
    }
}

fn spawn_service(
    pack_base: *const u8,
    entry: &pack::PackEntry,
    pack_len: usize,
    extra_handle: Handle,
) -> Result<Handle, SyscallError> {
    let binary_end = entry.offset as usize + entry.size as usize;

    if binary_end > pack_len {
        return Err(SyscallError::InvalidArgument);
    }

    let code_size = (entry.size as usize).next_multiple_of(PAGE_SIZE);
    let code_vmo = abi::vmo::create(code_size, 0)?;
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let code_local = abi::vmo::map(code_vmo, 0, rw)?;

    // SAFETY: The kernel mapped code_local as RW for code_size bytes.
    // pack_base + entry.offset is within the pack VMO mapping (checked above).
    unsafe {
        core::ptr::copy_nonoverlapping(
            pack_base.add(entry.offset as usize),
            code_local as *mut u8,
            entry.size as usize,
        );
    }

    abi::vmo::unmap(code_local)?;

    let space = abi::space::create()?;
    let rx = Rights(Rights::READ.0 | Rights::EXECUTE.0 | Rights::MAP.0);

    abi::vmo::map_into(code_vmo, space, SERVICE_CODE_VA, rx)?;

    let stack_vmo = abi::vmo::create(STACK_SIZE, 0)?;
    let stack_rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

    abi::vmo::map_into(stack_vmo, space, SERVICE_STACK_VA, stack_rw)?;

    let stack_top = SERVICE_STACK_VA + STACK_SIZE;
    // handle[0] = code VMO, handle[1] = stack VMO, handle[2] = extra
    // (name service endpoint for recv, or name service endpoint for calling)
    let bootstrap_handles = [code_vmo.0, stack_vmo.0, extra_handle.0];
    let thread = abi::thread::create_in(space, SERVICE_CODE_VA, stack_top, 0, &bootstrap_handles)?;

    Ok(thread)
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let ro = Rights(Rights::READ.0 | Rights::MAP.0);

    if let Ok(pack_va) = abi::vmo::map(HANDLE_PACK_VMO, 0, ro) {
        let header = pack::read_header(pack_va as *const u8);

        if header.is_valid() {
            spawn_from_pack(pack_va as *const u8, header.total_size as usize);
        }
    }

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
