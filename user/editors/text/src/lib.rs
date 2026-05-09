//! Text editor protocol — key event dispatch and edit results.
//!
//! Transport: sync call/reply. The presenter dispatches key events to
//! the editor; the editor translates them into document operations and
//! replies with the outcome.
//!
//! The editor handles only content mutations (character insert, delete,
//! tab, enter). Navigation (arrow keys, Home/End, Page Up/Down) stays
//! in the presenter because cursor movement requires layout knowledge.

#![no_std]

pub use ipc::MAX_PAYLOAD;

// ── Methods served by the text editor ────────────────────────────

pub const DISPATCH_KEY: u32 = 1;

// ── USB HID key codes for special keys ───────────────────────────

pub const HID_KEY_RETURN: u16 = 0x28;
pub const HID_KEY_BACKSPACE: u16 = 0x2A;
pub const HID_KEY_TAB: u16 = 0x2B;
pub const HID_KEY_HOME: u16 = 0x4A;
pub const HID_KEY_PAGE_UP: u16 = 0x4B;
pub const HID_KEY_DELETE: u16 = 0x4C;
pub const HID_KEY_END: u16 = 0x4D;
pub const HID_KEY_PAGE_DOWN: u16 = 0x4E;
pub const HID_KEY_RIGHT: u16 = 0x4F;
pub const HID_KEY_LEFT: u16 = 0x50;
pub const HID_KEY_DOWN: u16 = 0x51;
pub const HID_KEY_UP: u16 = 0x52;

// ── Input modifier flags (shared with input protocol) ────────────

pub const MOD_SHIFT: u8 = 1 << 0;
pub const MOD_CONTROL: u8 = 1 << 1;
pub const MOD_ALT: u8 = 1 << 2;
pub const MOD_SUPER: u8 = 1 << 3;

// ── Key dispatch (presenter → editor) ────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyDispatch {
    pub key_code: u16,
    pub modifiers: u8,
    pub character: u8,
}

impl KeyDispatch {
    pub const SIZE: usize = 4;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0..2].copy_from_slice(&self.key_code.to_le_bytes());
        buf[2] = self.modifiers;
        buf[3] = self.character;
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            key_code: u16::from_le_bytes(buf[0..2].try_into().unwrap()),
            modifiers: buf[2],
            character: buf[3],
        }
    }
}

// ── Key reply (editor → presenter) ──────────────────────────────

pub const ACTION_NONE: u8 = 0;
pub const ACTION_INSERTED: u8 = 1;
pub const ACTION_DELETED: u8 = 2;
pub const ACTION_REPLACED: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyReply {
    pub action: u8,
    pub _pad: u8,
    pub content_len: u64,
    pub cursor_pos: u64,
}

impl KeyReply {
    pub const SIZE: usize = 18;

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0] = self.action;
        buf[1] = 0;
        buf[2..10].copy_from_slice(&self.content_len.to_le_bytes());
        buf[10..18].copy_from_slice(&self.cursor_pos.to_le_bytes());
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            action: buf[0],
            _pad: 0,
            content_len: u64::from_le_bytes(buf[2..10].try_into().unwrap()),
            cursor_pos: u64::from_le_bytes(buf[10..18].try_into().unwrap()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_dispatch_round_trip() {
        let dispatch = KeyDispatch {
            key_code: HID_KEY_TAB,
            modifiers: MOD_SHIFT,
            character: 0,
        };
        let mut buf = [0u8; KeyDispatch::SIZE];

        dispatch.write_to(&mut buf);

        assert_eq!(KeyDispatch::read_from(&buf), dispatch);
    }

    #[test]
    fn key_dispatch_printable() {
        let dispatch = KeyDispatch {
            key_code: 0x04,
            modifiers: 0,
            character: b'a',
        };
        let mut buf = [0u8; KeyDispatch::SIZE];

        dispatch.write_to(&mut buf);

        let decoded = KeyDispatch::read_from(&buf);

        assert_eq!(decoded.character, b'a');
        assert_eq!(decoded.modifiers, 0);
    }

    #[test]
    fn key_reply_round_trip() {
        let reply = KeyReply {
            action: ACTION_INSERTED,
            _pad: 0,
            content_len: 42,
            cursor_pos: 10,
        };
        let mut buf = [0u8; KeyReply::SIZE];

        reply.write_to(&mut buf);

        assert_eq!(KeyReply::read_from(&buf), reply);
    }

    #[test]
    fn key_reply_deleted() {
        let reply = KeyReply {
            action: ACTION_DELETED,
            _pad: 0,
            content_len: 100,
            cursor_pos: 50,
        };
        let mut buf = [0u8; KeyReply::SIZE];

        reply.write_to(&mut buf);

        let decoded = KeyReply::read_from(&buf);

        assert_eq!(decoded.action, ACTION_DELETED);
        assert_eq!(decoded.content_len, 100);
    }

    #[test]
    fn method_id_nonzero() {
        assert_ne!(DISPATCH_KEY, 0);
    }

    #[test]
    fn all_sizes_fit_payload() {
        assert!(KeyDispatch::SIZE <= MAX_PAYLOAD);
        assert!(KeyReply::SIZE <= MAX_PAYLOAD);
    }

    #[test]
    fn hid_key_codes_distinct() {
        let codes = [
            HID_KEY_RETURN,
            HID_KEY_BACKSPACE,
            HID_KEY_TAB,
            HID_KEY_HOME,
            HID_KEY_PAGE_UP,
            HID_KEY_DELETE,
            HID_KEY_END,
            HID_KEY_PAGE_DOWN,
            HID_KEY_RIGHT,
            HID_KEY_LEFT,
            HID_KEY_DOWN,
            HID_KEY_UP,
        ];

        for i in 0..codes.len() {
            for j in (i + 1)..codes.len() {
                assert_ne!(codes[i], codes[j]);
            }
        }
    }

    #[test]
    fn action_values_distinct() {
        let actions = [
            ACTION_NONE,
            ACTION_INSERTED,
            ACTION_DELETED,
            ACTION_REPLACED,
        ];

        for i in 0..actions.len() {
            for j in (i + 1)..actions.len() {
                assert_ne!(actions[i], actions[j]);
            }
        }
    }
}
