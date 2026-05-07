//! Input protocol — keyboard and pointer events.
//!
//! Transport: event ring (SPSC ring buffer in shared VMO + event signal).
//! Direction: input driver → presenter.
//!
//! Events are fixed-size 16-byte slots written to the ring buffer.
//! The driver must not block on the presenter consuming events.

pub const EVENT_SIZE: usize = 16;

pub const KEY_DOWN: u8 = 1;
pub const KEY_UP: u8 = 2;
pub const POINTER_MOVE: u8 = 3;
pub const POINTER_DOWN: u8 = 4;
pub const POINTER_UP: u8 = 5;

pub const MOD_SHIFT: u8 = 1 << 0;
pub const MOD_CONTROL: u8 = 1 << 1;
pub const MOD_ALT: u8 = 1 << 2;
pub const MOD_SUPER: u8 = 1 << 3;
pub const MOD_CAPS_LOCK: u8 = 1 << 4;

pub const BUTTON_NONE: u8 = 0;
pub const BUTTON_LEFT: u8 = 1;
pub const BUTTON_RIGHT: u8 = 2;
pub const BUTTON_MIDDLE: u8 = 3;

/// 16-byte input event — tagged union over key and pointer events.
///
/// ```text
/// [0]:     event_type (KEY_DOWN..POINTER_UP)
/// [1]:     flags — modifiers for keys, button id for pointer
/// [2..4]:  key_code: u16 (key events only, USB HID usage code)
/// [4..8]:  x: i32 (pointer events only, device coordinates)
/// [8..12]: y: i32 (pointer events only, device coordinates)
/// [12..16]: reserved (zero)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    pub event_type: u8,
    pub flags: u8,
    pub key_code: u16,
    pub x: i32,
    pub y: i32,
}

impl InputEvent {
    pub const SIZE: usize = EVENT_SIZE;

    #[must_use]
    pub fn key_down(key_code: u16, modifiers: u8) -> Self {
        Self {
            event_type: KEY_DOWN,
            flags: modifiers,
            key_code,
            x: 0,
            y: 0,
        }
    }

    #[must_use]
    pub fn key_up(key_code: u16, modifiers: u8) -> Self {
        Self {
            event_type: KEY_UP,
            flags: modifiers,
            key_code,
            x: 0,
            y: 0,
        }
    }

    #[must_use]
    pub fn pointer_move(x: i32, y: i32) -> Self {
        Self {
            event_type: POINTER_MOVE,
            flags: BUTTON_NONE,
            key_code: 0,
            x,
            y,
        }
    }

    #[must_use]
    pub fn pointer_down(x: i32, y: i32, button: u8) -> Self {
        Self {
            event_type: POINTER_DOWN,
            flags: button,
            key_code: 0,
            x,
            y,
        }
    }

    #[must_use]
    pub fn pointer_up(x: i32, y: i32, button: u8) -> Self {
        Self {
            event_type: POINTER_UP,
            flags: button,
            key_code: 0,
            x,
            y,
        }
    }

    pub fn write_to(&self, buf: &mut [u8]) {
        buf[0] = self.event_type;
        buf[1] = self.flags;
        buf[2..4].copy_from_slice(&self.key_code.to_le_bytes());
        buf[4..8].copy_from_slice(&self.x.to_le_bytes());
        buf[8..12].copy_from_slice(&self.y.to_le_bytes());
        buf[12..16].copy_from_slice(&[0; 4]);
    }

    #[must_use]
    pub fn read_from(buf: &[u8]) -> Self {
        Self {
            event_type: buf[0],
            flags: buf[1],
            key_code: u16::from_le_bytes(buf[2..4].try_into().unwrap()),
            x: i32::from_le_bytes(buf[4..8].try_into().unwrap()),
            y: i32::from_le_bytes(buf[8..12].try_into().unwrap()),
        }
    }

    #[must_use]
    pub fn is_key(&self) -> bool {
        self.event_type == KEY_DOWN || self.event_type == KEY_UP
    }

    #[must_use]
    pub fn is_pointer(&self) -> bool {
        matches!(self.event_type, POINTER_MOVE | POINTER_DOWN | POINTER_UP)
    }

    #[must_use]
    pub fn modifiers(&self) -> u8 {
        self.flags
    }

    #[must_use]
    pub fn button(&self) -> u8 {
        self.flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_down_round_trip() {
        let event = InputEvent::key_down(0x04, MOD_SHIFT | MOD_CONTROL);
        let mut buf = [0u8; EVENT_SIZE];

        event.write_to(&mut buf);

        let decoded = InputEvent::read_from(&buf);

        assert_eq!(event, decoded);
    }

    #[test]
    fn key_up_round_trip() {
        let event = InputEvent::key_up(0x1E, MOD_ALT);
        let mut buf = [0u8; EVENT_SIZE];

        event.write_to(&mut buf);

        let decoded = InputEvent::read_from(&buf);

        assert_eq!(event, decoded);
    }

    #[test]
    fn pointer_move_round_trip() {
        let event = InputEvent::pointer_move(512, 384);
        let mut buf = [0u8; EVENT_SIZE];

        event.write_to(&mut buf);

        let decoded = InputEvent::read_from(&buf);

        assert_eq!(event, decoded);
    }

    #[test]
    fn pointer_down_round_trip() {
        let event = InputEvent::pointer_down(100, 200, BUTTON_LEFT);
        let mut buf = [0u8; EVENT_SIZE];

        event.write_to(&mut buf);

        let decoded = InputEvent::read_from(&buf);

        assert_eq!(event, decoded);
    }

    #[test]
    fn pointer_up_round_trip() {
        let event = InputEvent::pointer_up(100, 200, BUTTON_RIGHT);
        let mut buf = [0u8; EVENT_SIZE];

        event.write_to(&mut buf);

        let decoded = InputEvent::read_from(&buf);

        assert_eq!(event, decoded);
    }

    #[test]
    fn negative_coordinates() {
        let event = InputEvent::pointer_move(-100, -200);
        let mut buf = [0u8; EVENT_SIZE];

        event.write_to(&mut buf);

        let decoded = InputEvent::read_from(&buf);

        assert_eq!(decoded.x, -100);
        assert_eq!(decoded.y, -200);
    }

    #[test]
    fn is_key_classification() {
        assert!(InputEvent::key_down(0, 0).is_key());
        assert!(InputEvent::key_up(0, 0).is_key());
        assert!(!InputEvent::pointer_move(0, 0).is_key());
    }

    #[test]
    fn is_pointer_classification() {
        assert!(InputEvent::pointer_move(0, 0).is_pointer());
        assert!(InputEvent::pointer_down(0, 0, 0).is_pointer());
        assert!(InputEvent::pointer_up(0, 0, 0).is_pointer());
        assert!(!InputEvent::key_down(0, 0).is_pointer());
    }

    #[test]
    fn reserved_bytes_zeroed() {
        let event = InputEvent::key_down(0xFF, 0xFF);
        let mut buf = [0xAA; EVENT_SIZE];

        event.write_to(&mut buf);

        assert_eq!(buf[12..16], [0, 0, 0, 0]);
    }

    #[test]
    fn modifier_flags_independent() {
        assert_eq!(MOD_SHIFT & MOD_CONTROL, 0);
        assert_eq!(MOD_ALT & MOD_SUPER, 0);
        assert_eq!(MOD_CAPS_LOCK & MOD_SHIFT, 0);

        let all = MOD_SHIFT | MOD_CONTROL | MOD_ALT | MOD_SUPER | MOD_CAPS_LOCK;

        assert_eq!(all.count_ones(), 5);
    }

    #[test]
    fn size_matches_event_size() {
        assert_eq!(InputEvent::SIZE, EVENT_SIZE);
        assert_eq!(EVENT_SIZE, 16);
    }
}
