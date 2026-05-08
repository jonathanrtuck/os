//! Init — the first userspace process.
//!
//! Launched by the kernel bootstrap. Receives bootstrap handles at
//! well-known indices:
//!   Handle 0: own address space
//!   Handle 1: code VMO
//!   Handle 2: service pack VMO (SVPK format)
//!   Handle 3: device manifest VMO
//!   Handle 4: UART MMIO VMO (device)
//!   Handle 5: Virtio MMIO VMO (device)
//!   Handle 6: DMA Resource
//!
//! Parses the service pack, creates a name service endpoint, spawns all
//! services, then enters a persistent serve loop handling DMA allocation
//! requests from drivers.

#![no_std]
#![no_main]

mod pack;

use core::panic::PanicInfo;

use abi::types::{Handle, Rights, SyscallError};

const _HANDLE_SPACE: Handle = Handle(0);
const _HANDLE_CODE_VMO: Handle = Handle(1);
const HANDLE_PACK_VMO: Handle = Handle(2);
const _HANDLE_MANIFEST_VMO: Handle = Handle(3);
const HANDLE_UART_VMO: Handle = Handle(4);
const HANDLE_VIRTIO_VMO: Handle = Handle(5);
const HANDLE_DMA_RESOURCE: Handle = Handle(6);

const PAGE_SIZE: usize = 16384;
const STACK_SIZE: usize = PAGE_SIZE * 4;
const MSG_SIZE: usize = 128;

const SERVICE_CODE_VA: usize = 0x0020_0000;
const SERVICE_STACK_VA: usize = 0x1_0000_0000;

const NAME_SVC: &[u8] = b"name";
const CONSOLE_SVC: &[u8] = b"console";
const INPUT_SVC: &[u8] = b"input";
const BLK_SVC: &[u8] = b"blk";
const RENDER_SVC: &[u8] = b"render";

fn spawn_from_pack(pack_data: &[u8], ns_ep: Handle, init_ep: Handle) {
    let header = pack::read_header(pack_data);

    if !header.is_valid() {
        return;
    }

    for i in 0..header.count as usize {
        let entry = pack::read_entry(pack_data, i);
        let name = pack::read_name(pack_data, i);

        if entry.size == 0 {
            continue;
        }

        if name == NAME_SVC {
            let _ = spawn_service(pack_data, &entry, &[ns_ep]);
        }
    }

    for i in 0..header.count as usize {
        let entry = pack::read_entry(pack_data, i);
        let name = pack::read_name(pack_data, i);

        if entry.size == 0 || name == NAME_SVC {
            continue;
        }

        if name == CONSOLE_SVC {
            let _ = spawn_service(pack_data, &entry, &[ns_ep, HANDLE_UART_VMO]);
        } else if name == INPUT_SVC || name == BLK_SVC || name == RENDER_SVC {
            let _ = spawn_service(pack_data, &entry, &[ns_ep, HANDLE_VIRTIO_VMO, init_ep]);
        } else {
            let _ = spawn_service(pack_data, &entry, &[ns_ep]);
        }
    }
}

fn spawn_service(
    pack_data: &[u8],
    entry: &pack::PackEntry,
    extra_handles: &[Handle],
) -> Result<Handle, SyscallError> {
    let binary_end = entry.offset as usize + entry.size as usize;

    if binary_end > pack_data.len() {
        return Err(SyscallError::InvalidArgument);
    }

    let code_size = (entry.size as usize).next_multiple_of(PAGE_SIZE);
    let code_vmo = abi::vmo::create(code_size, 0)?;
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let mut code_mapping = abi::vmo::map_region(code_vmo, code_size, rw)?;

    code_mapping[..entry.size as usize]
        .copy_from_slice(&pack_data[entry.offset as usize..binary_end]);

    drop(code_mapping);

    let space = abi::space::create()?;
    let rx = Rights(Rights::READ.0 | Rights::EXECUTE.0 | Rights::MAP.0);

    abi::vmo::map_into(code_vmo, space, SERVICE_CODE_VA, rx)?;

    let stack_vmo = abi::vmo::create(STACK_SIZE, 0)?;
    let stack_rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);

    abi::vmo::map_into(stack_vmo, space, SERVICE_STACK_VA, stack_rw)?;

    let stack_top = SERVICE_STACK_VA + STACK_SIZE;
    let mut bootstrap_handles = [0u32; 8];

    bootstrap_handles[0] = code_vmo.0;
    bootstrap_handles[1] = stack_vmo.0;

    for (i, h) in extra_handles.iter().enumerate() {
        bootstrap_handles[2 + i] = h.0;
    }

    let handle_count = 2 + extra_handles.len();
    let thread = abi::thread::create_in(
        space,
        SERVICE_CODE_VA,
        stack_top,
        0,
        &bootstrap_handles[..handle_count],
    )?;

    Ok(thread)
}

fn serve_dma_requests(init_ep: Handle) -> ! {
    let mut msg_buf = [0u8; MSG_SIZE];
    let mut handles_buf = [0u32; 4];

    loop {
        let recv = match abi::ipc::recv(init_ep, &mut msg_buf, &mut handles_buf) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let method = if recv.msg_len >= 4 {
            u32::from_le_bytes(msg_buf[0..4].try_into().unwrap_or([0; 4]))
        } else {
            0
        };

        if method == protocol::bootstrap::DMA_ALLOC && recv.msg_len >= 8 {
            let req = protocol::bootstrap::DmaAllocRequest::read_from(&msg_buf[4..8]);
            let size = (req.size as usize).next_multiple_of(PAGE_SIZE);

            match abi::vmo::create_dma(size, HANDLE_DMA_RESOURCE) {
                Ok(vmo_handle) => {
                    let reply_data = [0u8; 4];
                    let _ = abi::ipc::reply(init_ep, recv.reply_cap, &reply_data, &[vmo_handle.0]);
                }
                Err(e) => {
                    let status = e as u16;
                    let mut reply_data = [0u8; 4];

                    reply_data[0..2].copy_from_slice(&status.to_le_bytes());

                    let _ = abi::ipc::reply(init_ep, recv.reply_cap, &reply_data, &[]);
                }
            }
        } else {
            let _ = abi::ipc::reply(init_ep, recv.reply_cap, &[0u8; 4], &[]);
        }
    }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let ro = Rights(Rights::READ.0 | Rights::MAP.0);
    let ns_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0xE001),
    };
    let init_ep = match abi::ipc::endpoint_create() {
        Ok(h) => h,
        Err(_) => abi::thread::exit(0xE002),
    };

    if let Ok(pack_va) = abi::vmo::map(HANDLE_PACK_VMO, 0, ro) {
        // SAFETY: pack VMO is at least one page; header is 16 bytes.
        let peek = unsafe { core::slice::from_raw_parts(pack_va as *const u8, pack::HEADER_SIZE) };
        let header = pack::read_header(peek);

        if header.is_valid() {
            // SAFETY: kernel mapped the full pack VMO at pack_va.
            let pack_data = unsafe {
                core::slice::from_raw_parts(pack_va as *const u8, header.total_size as usize)
            };

            spawn_from_pack(pack_data, ns_ep, init_ep);
        }
    }

    serve_dma_requests(init_ep);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
