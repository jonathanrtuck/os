//! Store integration test — exercises the store service over IPC.
//!
//! Bootstrap handles:
//!   Handle 0: code VMO
//!   Handle 1: stack VMO
//!   Handle 2: name service endpoint
//!
//! Sequence:
//!   1. Watch for "console" and "store"
//!   2. Set up a shared VMO with the store
//!   3. Create a document
//!   4. Write test data via shared VMO
//!   5. Read it back and verify
//!   6. Snapshot the document
//!   7. Overwrite with different data
//!   8. Restore from snapshot
//!   9. Read and verify original data is back
//!  10. Exit 0 on success

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};

const HANDLE_NS_EP: Handle = Handle(2);

const PAGE_SIZE: usize = 16384;
const SHARED_VMO_SIZE: usize = PAGE_SIZE * 4;

const EXIT_CONSOLE_WATCH: u32 = 1;
const EXIT_STORE_WATCH: u32 = 2;
const EXIT_SHARED_VMO_CREATE: u32 = 3;
const EXIT_SHARED_VMO_MAP: u32 = 4;
const EXIT_SHARED_VMO_DUP: u32 = 5;
const EXIT_SETUP: u32 = 6;
const EXIT_CREATE: u32 = 10;
const EXIT_WRITE: u32 = 11;
const EXIT_READ: u32 = 12;
const EXIT_DATA_MISMATCH: u32 = 13;
const EXIT_SNAPSHOT: u32 = 20;
const EXIT_RESTORE: u32 = 22;
const EXIT_RESTORE_READ: u32 = 23;
const EXIT_RESTORE_MISMATCH: u32 = 24;
const EXIT_COMMIT: u32 = 30;

const TEST_DATA: &[u8] = b"Hello from test-store!";
const OVERWRITE_DATA: &[u8] = b"This should be undone.";

fn setup_shared_vmo(store_ep: Handle) -> (usize, usize) {
    let vmo = match abi::vmo::create(SHARED_VMO_SIZE, 0) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_SHARED_VMO_CREATE),
    };
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let va = match abi::vmo::map(vmo, 0, rw) {
        Ok(v) => v,
        Err(_) => abi::thread::exit(EXIT_SHARED_VMO_MAP),
    };
    let dup = match abi::handle::dup(vmo, Rights::ALL) {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_SHARED_VMO_DUP),
    };
    let mut buf = [0u8; ipc::message::MSG_SIZE];

    ipc::message::write_request(&mut buf, store_service::SETUP, &[]);

    let _ = abi::ipc::call(
        store_ep,
        &mut buf,
        ipc::message::HEADER_SIZE,
        &[dup.0],
        &mut [],
    );
    let header = ipc::message::Header::read_from(&buf);

    if header.is_error() {
        abi::thread::exit(EXIT_SETUP);
    }

    (va, SHARED_VMO_SIZE)
}

fn create_document(store_ep: Handle, media_type: &[u8]) -> u64 {
    let req = store_service::CreateRequest {
        media_type_len: media_type.len() as u16,
    };
    let mut payload = [0u8; ipc::MAX_PAYLOAD];

    req.write_to(&mut payload);

    let mt_len = media_type
        .len()
        .min(ipc::MAX_PAYLOAD - store_service::CreateRequest::SIZE);

    payload[store_service::CreateRequest::SIZE..store_service::CreateRequest::SIZE + mt_len]
        .copy_from_slice(&media_type[..mt_len]);

    let (status, reply_data) =
        match ipc::client::call_simple(store_ep, store_service::CREATE, &payload[..2 + mt_len]) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(EXIT_CREATE),
        };

    if status != 0 {
        abi::thread::exit(EXIT_CREATE);
    }

    let reply = store_service::CreateReply::read_from(&reply_data);

    reply.file_id
}

fn write_doc(store_ep: Handle, file_id: u64, offset: u64, shared_va: usize, data: &[u8]) {
    // SAFETY: shared_va is a valid mapping of SHARED_VMO_SIZE bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), shared_va as *mut u8, data.len());
    }

    let req = store_service::WriteRequest {
        file_id,
        offset,
        vmo_offset: 0,
        len: data.len() as u32,
    };
    let mut req_data = [0u8; store_service::WriteRequest::SIZE];

    req.write_to(&mut req_data);

    let (status, _) = match ipc::client::call_simple(store_ep, store_service::WRITE_DOC, &req_data)
    {
        Ok(r) => r,
        Err(_) => abi::thread::exit(EXIT_WRITE),
    };

    if status != 0 {
        abi::thread::exit(EXIT_WRITE);
    }
}

fn read_doc(store_ep: Handle, file_id: u64, offset: u64, max_len: u32, exit_code: u32) -> u32 {
    let req = store_service::ReadRequest {
        file_id,
        offset,
        vmo_offset: 0,
        max_len,
    };
    let mut req_data = [0u8; store_service::ReadRequest::SIZE];

    req.write_to(&mut req_data);

    let (status, reply_data) =
        match ipc::client::call_simple(store_ep, store_service::READ_DOC, &req_data) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(exit_code),
        };

    if status != 0 {
        abi::thread::exit(exit_code);
    }

    store_service::ReadReply::read_from(&reply_data).bytes_read
}

fn snapshot(store_ep: Handle, file_id: u64) -> u64 {
    let req = store_service::SnapshotRequest { file_id };
    let mut data = [0u8; store_service::SnapshotRequest::SIZE];

    req.write_to(&mut data);

    let (status, reply_data) =
        match ipc::client::call_simple(store_ep, store_service::SNAPSHOT, &data) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(EXIT_SNAPSHOT),
        };

    if status != 0 {
        abi::thread::exit(EXIT_SNAPSHOT);
    }

    store_service::SnapshotReply::read_from(&reply_data).snapshot_id
}

fn restore(store_ep: Handle, snapshot_id: u64) {
    let req = store_service::RestoreRequest {
        file_id: 0,
        snapshot_id,
    };
    let mut data = [0u8; store_service::RestoreRequest::SIZE];

    req.write_to(&mut data);

    let (status, _) = match ipc::client::call_simple(store_ep, store_service::RESTORE, &data) {
        Ok(r) => r,
        Err(_) => abi::thread::exit(EXIT_RESTORE),
    };

    if status != 0 {
        abi::thread::exit(EXIT_RESTORE);
    }
}

fn commit(store_ep: Handle) {
    let (status, _) = match ipc::client::call_simple(store_ep, store_service::COMMIT, &[]) {
        Ok(r) => r,
        Err(_) => abi::thread::exit(EXIT_COMMIT),
    };

    if status != 0 {
        abi::thread::exit(EXIT_COMMIT);
    }
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE_WATCH),
    };

    console::write(console_ep, b"test-store: starting\n");

    let store_ep = match name::watch(HANDLE_NS_EP, b"store") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"test-store: store not found\n");
            abi::thread::exit(EXIT_STORE_WATCH);
        }
    };

    console::write(console_ep, b"test-store: connected to store\n");

    let (shared_va, _shared_len) = setup_shared_vmo(store_ep);
    // 1. Create a document.
    let file_id = create_document(store_ep, b"text/plain");

    console::write(console_ep, b"test-store: created doc\n");

    // 2. Write test data.
    write_doc(store_ep, file_id, 0, shared_va, TEST_DATA);

    // 3. Read it back and verify.
    let n = read_doc(store_ep, file_id, 0, TEST_DATA.len() as u32, EXIT_READ);

    if n as usize != TEST_DATA.len() {
        abi::thread::exit(EXIT_DATA_MISMATCH);
    }

    // SAFETY: shared_va is valid for SHARED_VMO_SIZE bytes.
    let read_back = unsafe { core::slice::from_raw_parts(shared_va as *const u8, TEST_DATA.len()) };

    if read_back != TEST_DATA {
        abi::thread::exit(EXIT_DATA_MISMATCH);
    }

    console::write(console_ep, b"test-store: write/read OK\n");

    // 4. Snapshot.
    let snap_id = snapshot(store_ep, file_id);

    // 5. Overwrite with different data.
    write_doc(store_ep, file_id, 0, shared_va, OVERWRITE_DATA);
    // 6. Restore from snapshot.
    restore(store_ep, snap_id);

    // 7. Read and verify original data is restored.
    let n2 = read_doc(
        store_ep,
        file_id,
        0,
        TEST_DATA.len() as u32,
        EXIT_RESTORE_READ,
    );

    if n2 as usize != TEST_DATA.len() {
        abi::thread::exit(EXIT_RESTORE_MISMATCH);
    }

    let restored = unsafe { core::slice::from_raw_parts(shared_va as *const u8, TEST_DATA.len()) };

    if restored != TEST_DATA {
        console::write(console_ep, b"test-store: FAIL restore mismatch\n");
        abi::thread::exit(EXIT_RESTORE_MISMATCH);
    }

    console::write(console_ep, b"test-store: snapshot/restore OK\n");

    // 8. Commit to disk.
    commit(store_ep);

    console::write(console_ep, b"test-store: PASS\n");

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
