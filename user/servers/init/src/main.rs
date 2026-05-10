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
const HANDLE_RTC_VMO: Handle = Handle(7);

const PAGE_SIZE: usize = 16384;
const STACK_SIZE: usize = PAGE_SIZE * 8;
const MSG_SIZE: usize = 128;

const SERVICE_CODE_VA: usize = 0x0020_0000;
const SERVICE_STACK_VA: usize = 0x1_0000_0000;

const NAME_SVC: &[u8] = b"name";
const CONSOLE_SVC: &[u8] = b"console";
const INPUT_SVC: &[u8] = b"input";
const BLK_SVC: &[u8] = b"blk";
const RENDER_SVC: &[u8] = b"render";
const NINEP_SVC: &[u8] = b"9p";
const PRESENTER_SVC: &[u8] = b"presenter";
const LAYOUT_SVC: &[u8] = b"layout";

static FONT_MONO: &[u8] = include_bytes!("../../../../assets/jetbrains-mono.ttf");
static FONT_MONO_ITALIC: &[u8] = include_bytes!("../../../../assets/jetbrains-mono-italic.ttf");
static FONT_SANS: &[u8] = include_bytes!("../../../../assets/inter.ttf");
static FONT_SANS_ITALIC: &[u8] = include_bytes!("../../../../assets/inter-italic.ttf");
static FONT_SERIF: &[u8] = include_bytes!("../../../../assets/source-serif-4.ttf");
static FONT_SERIF_ITALIC: &[u8] = include_bytes!("../../../../assets/source-serif-4-italic.ttf");

fn create_font_vmo() -> Handle {
    let fonts: [&[u8]; init::FONT_PACK_COUNT] = [
        FONT_MONO,
        FONT_MONO_ITALIC,
        FONT_SANS,
        FONT_SANS_ITALIC,
        FONT_SERIF,
        FONT_SERIF_ITALIC,
    ];
    let total_data: usize = fonts.iter().map(|f| f.len()).sum();
    let vmo_size = (init::FONT_PACK_HEADER + total_data).next_multiple_of(PAGE_SIZE);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let vmo = abi::vmo::create(vmo_size, 0).unwrap_or_else(|_| abi::thread::exit(0xE010));
    let mut mapping =
        abi::vmo::map_region(vmo, vmo_size, rw).unwrap_or_else(|_| abi::thread::exit(0xE011));

    // Write header: magic + count
    mapping[0..4].copy_from_slice(&init::FONT_PACK_MAGIC.to_le_bytes());
    mapping[4..8].copy_from_slice(&(init::FONT_PACK_COUNT as u32).to_le_bytes());

    // Write entries and data
    let mut data_offset = init::FONT_PACK_HEADER;

    for (i, font) in fonts.iter().enumerate() {
        let entry_off = 8 + i * 8;

        mapping[entry_off..entry_off + 4].copy_from_slice(&(data_offset as u32).to_le_bytes());
        mapping[entry_off + 4..entry_off + 8].copy_from_slice(&(font.len() as u32).to_le_bytes());
        mapping[data_offset..data_offset + font.len()].copy_from_slice(font);
        data_offset += font.len();
    }

    drop(mapping);

    vmo
}

fn dup_ro(h: Handle) -> Handle {
    abi::handle::dup(h, Rights::READ_MAP).unwrap_or(h)
}

fn register_for(ns_ep: Handle, svc_name: &[u8]) -> Handle {
    let ep = abi::ipc::endpoint_create().unwrap_or_else(|_| abi::thread::exit(0xE020));
    let dup = abi::handle::dup(ep, Rights::ALL).unwrap_or_else(|_| abi::thread::exit(0xE021));

    // Inline name::register — avoids pulling in the name crate
    // (which brings heap/alloc deps unsuitable for init).
    let mut buf = [0u8; MSG_SIZE];
    let mut name_payload = [0u8; 32];
    let len = svc_name.len().min(32);

    name_payload[..len].copy_from_slice(&svc_name[..len]);

    ipc::message::write_request(&mut buf, 1, &name_payload); // 1 = REGISTER
    let _ = abi::ipc::call(
        ns_ep,
        &mut buf,
        ipc::message::HEADER_SIZE + 32,
        &[dup.0],
        &mut [],
    );

    ep
}

const STORE_SVC: &[u8] = b"store";
const DOCUMENT_SVC: &[u8] = b"document";
const EDITOR_TEXT_SVC: &[u8] = b"editor.text";
const PNG_DECODER_SVC: &[u8] = b"png-decoder";
const JPEG_DECODER_SVC: &[u8] = b"jpeg-decoder";
const FS_SVC: &[u8] = b"fs";

fn spawn_from_pack(pack_data: &[u8], ns_ep: Handle, init_ep: Handle, font_vmo: Handle) {
    let header = pack::read_header(pack_data);

    if !header.is_valid() {
        return;
    }

    // Phase 1: spawn the name service (it IS the registry).
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

    // Phase 2: spawn all other services with pre-registered endpoints.
    // Init creates the endpoint, registers it with the name service,
    // and passes it to the service. Services never self-register.
    for i in 0..header.count as usize {
        let entry = pack::read_entry(pack_data, i);
        let name = pack::read_name(pack_data, i);

        if entry.size == 0 || name == NAME_SVC {
            continue;
        }

        if name == CONSOLE_SVC {
            let ep = register_for(ns_ep, name);
            let _ = spawn_service(pack_data, &entry, &[ns_ep, HANDLE_UART_VMO, ep]);
        } else if name == RENDER_SVC {
            let ep = register_for(ns_ep, name);
            let _ = spawn_service(
                pack_data,
                &entry,
                &[ns_ep, HANDLE_VIRTIO_VMO, init_ep, dup_ro(font_vmo), ep],
            );
        } else if name == INPUT_SVC || name == BLK_SVC || name == NINEP_SVC {
            let ep = register_for(ns_ep, name);
            let _ = spawn_service(pack_data, &entry, &[ns_ep, HANDLE_VIRTIO_VMO, init_ep, ep]);
        } else if name == PRESENTER_SVC {
            let ep = register_for(ns_ep, name);
            let _ = spawn_service(
                pack_data,
                &entry,
                &[ns_ep, HANDLE_RTC_VMO, dup_ro(font_vmo), ep],
            );
        } else if name == LAYOUT_SVC {
            let ep = register_for(ns_ep, name);
            let _ = spawn_service(pack_data, &entry, &[ns_ep, dup_ro(font_vmo), ep]);
        } else if name == STORE_SVC
            || name == DOCUMENT_SVC
            || name == EDITOR_TEXT_SVC
            || name == PNG_DECODER_SVC
            || name == JPEG_DECODER_SVC
            || name == FS_SVC
        {
            let ep = register_for(ns_ep, name);
            let _ = spawn_service(pack_data, &entry, &[ns_ep, ep]);
        } else {
            // Test services and unknown services: self-register.
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

    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let rx = Rights(Rights::READ.0 | Rights::EXECUTE.0 | Rights::MAP.0);
    let space = abi::space::create()?;
    let code_vmo = if entry.data_offset > 0 {
        let text_size = (entry.data_offset as usize).next_multiple_of(PAGE_SIZE);
        let text_vmo = abi::vmo::create(text_size, 0)?;
        let mut text_mapping = abi::vmo::map_region(text_vmo, text_size, rw)?;
        let copy_len = (entry.size as usize).min(text_size);

        text_mapping[..copy_len]
            .copy_from_slice(&pack_data[entry.offset as usize..entry.offset as usize + copy_len]);

        drop(text_mapping);

        abi::vmo::map_into(text_vmo, space, SERVICE_CODE_VA, rx)?;

        let data_va = SERVICE_CODE_VA + entry.data_offset as usize;
        let data_size =
            (entry.mem_size as usize - entry.data_offset as usize).next_multiple_of(PAGE_SIZE);

        if data_size > 0 {
            let data_vmo = abi::vmo::create(data_size, 0)?;

            if entry.size as usize > entry.data_offset as usize {
                let data_file_len = entry.size as usize - entry.data_offset as usize;
                let mut data_mapping = abi::vmo::map_region(data_vmo, data_size, rw)?;

                data_mapping[..data_file_len].copy_from_slice(
                    &pack_data[entry.offset as usize + entry.data_offset as usize
                        ..entry.offset as usize + entry.size as usize],
                );

                drop(data_mapping);
            }

            abi::vmo::map_into(data_vmo, space, data_va, rw)?;
        }

        text_vmo
    } else {
        let code_size = (entry.size as usize).next_multiple_of(PAGE_SIZE);
        let code_vmo = abi::vmo::create(code_size, 0)?;
        let mut code_mapping = abi::vmo::map_region(code_vmo, code_size, rw)?;

        code_mapping[..entry.size as usize]
            .copy_from_slice(&pack_data[entry.offset as usize..binary_end]);

        drop(code_mapping);

        abi::vmo::map_into(code_vmo, space, SERVICE_CODE_VA, rx)?;

        code_vmo
    };

    let stack_vmo = abi::vmo::create(STACK_SIZE, 0)?;

    abi::vmo::map_into(stack_vmo, space, SERVICE_STACK_VA, rw)?;

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

        if method == init::DMA_ALLOC && recv.msg_len >= 8 {
            let req = init::DmaAllocRequest::read_from(&msg_buf[4..8]);
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

            let font_vmo = create_font_vmo();

            spawn_from_pack(pack_data, ns_ep, init_ep, font_vmo);
        }
    }

    serve_dma_requests(init_ep);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
