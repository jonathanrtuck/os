//! Text editor — content-type-specific input-to-write translator.
//!
//! Receives key events from the presenter via sync call/reply and
//! translates them into document edit operations. Navigation (arrow
//! keys, Home/End) stays in the presenter because cursor movement
//! requires layout knowledge. The editor handles only content
//! mutations:
//!
//! - Printable character insertion
//! - Backspace (delete before cursor)
//! - Delete (delete after cursor)
//! - Return (insert newline)
//! - Tab (insert 4 spaces)
//! - Shift+Tab (dedent — remove up to 4 leading spaces)
//!
//! The editor holds a read-only shared memory mapping of the document
//! buffer (hardware-enforced via page table attributes). All writes go
//! through sync IPC to the document service.
//!
//! Bootstrap handles:
//!   Handle 2: name service endpoint
//!   Handle 3: service endpoint (pre-registered as "editor.text")

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, Rights};
use ipc::server::{Dispatch, Incoming};

const HANDLE_NS_EP: Handle = Handle(2);
const HANDLE_SVC_EP: Handle = Handle(3);

const EXIT_CONSOLE_NOT_FOUND: u32 = 0xE301;
const EXIT_DOC_NOT_FOUND: u32 = 0xE302;
const EXIT_DOC_SETUP: u32 = 0xE303;

// ── Document buffer access ───────────────────────────────────────

fn line_start(text: &[u8], pos: usize) -> usize {
    let mut i = pos;

    while i > 0 && text[i - 1] != b'\n' {
        i -= 1;
    }

    i
}

// ── Document service IPC helpers ─────────────────────────────────

fn doc_insert(doc_ep: Handle, offset: u64, data: &[u8]) -> Option<document_service::EditReply> {
    let header = document_service::InsertHeader { offset };
    let mut payload = [0u8; ipc::MAX_PAYLOAD];

    header.write_to(&mut payload);

    let copy_len = data.len().min(document_service::InsertHeader::MAX_INLINE);

    payload[document_service::InsertHeader::SIZE..document_service::InsertHeader::SIZE + copy_len]
        .copy_from_slice(&data[..copy_len]);

    let total = document_service::InsertHeader::SIZE + copy_len;

    match ipc::client::call_simple(doc_ep, document_service::INSERT, &payload[..total]) {
        Ok((0, reply_data)) => Some(document_service::EditReply::read_from(&reply_data)),
        _ => None,
    }
}

fn doc_delete(doc_ep: Handle, offset: u64, len: u64) -> Option<document_service::EditReply> {
    let req = document_service::DeleteRequest { offset, len };
    let mut data = [0u8; document_service::DeleteRequest::SIZE];

    req.write_to(&mut data);

    match ipc::client::call_simple(doc_ep, document_service::DELETE, &data) {
        Ok((0, reply_data)) => Some(document_service::EditReply::read_from(&reply_data)),
        _ => None,
    }
}

fn doc_replace(
    doc_ep: Handle,
    offset: u64,
    delete_len: u64,
    replacement: &[u8],
) -> Option<document_service::EditReply> {
    let header = document_service::ReplaceHeader { offset, delete_len };
    let mut payload = [0u8; ipc::MAX_PAYLOAD];

    header.write_to(&mut payload);

    let copy_len = replacement
        .len()
        .min(document_service::ReplaceHeader::MAX_INLINE);

    payload
        [document_service::ReplaceHeader::SIZE..document_service::ReplaceHeader::SIZE + copy_len]
        .copy_from_slice(&replacement[..copy_len]);

    let total = document_service::ReplaceHeader::SIZE + copy_len;

    match ipc::client::call_simple(doc_ep, document_service::REPLACE, &payload[..total]) {
        Ok((0, reply_data)) => Some(document_service::EditReply::read_from(&reply_data)),
        _ => None,
    }
}

// ── Editor server ────────────────────────────────────────────────

struct TextEditor {
    doc_ep: Handle,
    doc_va: usize,

    #[allow(dead_code)]
    console_ep: Handle,
}

impl TextEditor {
    fn handle_key(&mut self, dispatch: text_editor::KeyDispatch) -> text_editor::KeyReply {
        // SAFETY: doc_va is a valid RO mapping of the document buffer.
        let (content_len, cursor_pos, sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };
        let has_selection = sel_anchor != cursor_pos;

        if has_selection {
            return self.handle_key_with_selection(dispatch, content_len, sel_anchor, cursor_pos);
        }

        match dispatch.key_code {
            text_editor::HID_KEY_BACKSPACE => {
                if cursor_pos > 0
                    && let Some(reply) = doc_delete(self.doc_ep, (cursor_pos - 1) as u64, 1)
                {
                    return text_editor::KeyReply {
                        action: text_editor::ACTION_DELETED,
                        _pad: 0,
                        content_len: reply.content_len,
                        cursor_pos: reply.cursor_pos,
                    };
                }

                self.no_op(content_len, cursor_pos)
            }
            text_editor::HID_KEY_DELETE => {
                if cursor_pos < content_len
                    && let Some(reply) = doc_delete(self.doc_ep, cursor_pos as u64, 1)
                {
                    return text_editor::KeyReply {
                        action: text_editor::ACTION_DELETED,
                        _pad: 0,
                        content_len: reply.content_len,
                        cursor_pos: reply.cursor_pos,
                    };
                }

                self.no_op(content_len, cursor_pos)
            }
            text_editor::HID_KEY_RETURN => {
                if let Some(reply) = doc_insert(self.doc_ep, cursor_pos as u64, b"\n") {
                    text_editor::KeyReply {
                        action: text_editor::ACTION_INSERTED,
                        _pad: 0,
                        content_len: reply.content_len,
                        cursor_pos: reply.cursor_pos,
                    }
                } else {
                    self.no_op(content_len, cursor_pos)
                }
            }
            text_editor::HID_KEY_TAB => {
                if dispatch.modifiers & text_editor::MOD_SHIFT != 0 {
                    self.handle_dedent(content_len, cursor_pos)
                } else {
                    self.handle_tab(content_len, cursor_pos)
                }
            }
            _ => {
                if dispatch.character != 0
                    && dispatch.character != 0x08
                    && let Some(reply) =
                        doc_insert(self.doc_ep, cursor_pos as u64, &[dispatch.character])
                {
                    return text_editor::KeyReply {
                        action: text_editor::ACTION_INSERTED,
                        _pad: 0,
                        content_len: reply.content_len,
                        cursor_pos: reply.cursor_pos,
                    };
                }

                self.no_op(content_len, cursor_pos)
            }
        }
    }

    fn handle_key_with_selection(
        &mut self,
        dispatch: text_editor::KeyDispatch,
        content_len: usize,
        sel_anchor: usize,
        cursor_pos: usize,
    ) -> text_editor::KeyReply {
        let sel_start = sel_anchor.min(cursor_pos);
        let sel_len = sel_anchor.max(cursor_pos) - sel_start;

        match dispatch.key_code {
            text_editor::HID_KEY_BACKSPACE | text_editor::HID_KEY_DELETE => {
                if let Some(reply) = doc_replace(self.doc_ep, sel_start as u64, sel_len as u64, &[])
                {
                    text_editor::KeyReply {
                        action: text_editor::ACTION_DELETED,
                        _pad: 0,
                        content_len: reply.content_len,
                        cursor_pos: reply.cursor_pos,
                    }
                } else {
                    self.no_op(content_len, cursor_pos)
                }
            }
            text_editor::HID_KEY_RETURN => {
                if let Some(reply) =
                    doc_replace(self.doc_ep, sel_start as u64, sel_len as u64, b"\n")
                {
                    text_editor::KeyReply {
                        action: text_editor::ACTION_REPLACED,
                        _pad: 0,
                        content_len: reply.content_len,
                        cursor_pos: reply.cursor_pos,
                    }
                } else {
                    self.no_op(content_len, cursor_pos)
                }
            }
            text_editor::HID_KEY_TAB => {
                let replacement = if dispatch.modifiers & text_editor::MOD_SHIFT != 0 {
                    &b""[..]
                } else {
                    &b"    "[..]
                };

                if let Some(reply) =
                    doc_replace(self.doc_ep, sel_start as u64, sel_len as u64, replacement)
                {
                    text_editor::KeyReply {
                        action: text_editor::ACTION_REPLACED,
                        _pad: 0,
                        content_len: reply.content_len,
                        cursor_pos: reply.cursor_pos,
                    }
                } else {
                    self.no_op(content_len, cursor_pos)
                }
            }
            _ => {
                if dispatch.character != 0
                    && dispatch.character != 0x08
                    && let Some(reply) = doc_replace(
                        self.doc_ep,
                        sel_start as u64,
                        sel_len as u64,
                        &[dispatch.character],
                    )
                {
                    return text_editor::KeyReply {
                        action: text_editor::ACTION_REPLACED,
                        _pad: 0,
                        content_len: reply.content_len,
                        cursor_pos: reply.cursor_pos,
                    };
                }

                self.no_op(content_len, cursor_pos)
            }
        }
    }

    fn handle_tab(&mut self, content_len: usize, cursor_pos: usize) -> text_editor::KeyReply {
        if let Some(reply) = doc_insert(self.doc_ep, cursor_pos as u64, b"    ") {
            text_editor::KeyReply {
                action: text_editor::ACTION_INSERTED,
                _pad: 0,
                content_len: reply.content_len,
                cursor_pos: reply.cursor_pos,
            }
        } else {
            self.no_op(content_len, cursor_pos)
        }
    }

    fn handle_dedent(&mut self, content_len: usize, cursor_pos: usize) -> text_editor::KeyReply {
        if content_len == 0 {
            return self.no_op(content_len, cursor_pos);
        }

        let text = unsafe { document_service::doc_content_slice(self.doc_va, content_len) };
        let ls = line_start(text, cursor_pos);
        let mut spaces = 0usize;

        while spaces < 4 && ls + spaces < content_len && text[ls + spaces] == b' ' {
            spaces += 1;
        }

        if spaces > 0
            && let Some(reply) = doc_delete(self.doc_ep, ls as u64, spaces as u64)
        {
            return text_editor::KeyReply {
                action: text_editor::ACTION_DELETED,
                _pad: 0,
                content_len: reply.content_len,
                cursor_pos: reply.cursor_pos,
            };
        }

        self.no_op(content_len, cursor_pos)
    }

    fn no_op(&self, content_len: usize, cursor_pos: usize) -> text_editor::KeyReply {
        text_editor::KeyReply {
            action: text_editor::ACTION_NONE,
            _pad: 0,
            content_len: content_len as u64,
            cursor_pos: cursor_pos as u64,
        }
    }
}

// ── Dispatch ─────────────────────────────────────────────────────

impl Dispatch for TextEditor {
    fn dispatch(&mut self, msg: Incoming<'_>) {
        match msg.method {
            text_editor::DISPATCH_KEY => {
                if msg.payload.len() < text_editor::KeyDispatch::SIZE {
                    let _ = msg.reply_error(ipc::STATUS_INVALID);

                    return;
                }

                let dispatch = text_editor::KeyDispatch::read_from(msg.payload);
                let reply = self.handle_key(dispatch);
                let mut data = [0u8; text_editor::KeyReply::SIZE];

                reply.write_to(&mut data);

                let _ = msg.reply_ok(&data, &[]);
            }
            _ => {
                let _ = msg.reply_error(ipc::STATUS_UNSUPPORTED);
            }
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let console_ep = match name::watch(HANDLE_NS_EP, b"console") {
        Ok(h) => h,
        Err(_) => abi::thread::exit(EXIT_CONSOLE_NOT_FOUND),
    };

    console::write(console_ep, b"text-editor: starting\n");

    let doc_ep = match name::watch(HANDLE_NS_EP, b"document") {
        Ok(h) => h,
        Err(_) => {
            console::write(console_ep, b"text-editor: doc not found\n");

            abi::thread::exit(EXIT_DOC_NOT_FOUND);
        }
    };
    let doc_va =
        match ipc::client::setup_map_vmo(doc_ep, document_service::SETUP, &[], Rights::READ_MAP) {
            Ok(va) => va,
            Err(_) => abi::thread::exit(EXIT_DOC_SETUP),
        };

    console::write(console_ep, b"text-editor: doc connected\n");

    console::write(console_ep, b"text-editor: ready\n");

    let mut server = TextEditor {
        doc_ep,
        doc_va,
        console_ep,
    };

    ipc::server::serve(HANDLE_SVC_EP, &mut server);

    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
