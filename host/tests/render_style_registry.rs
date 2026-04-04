//! Tests for the style registry (protocol::content).

use protocol::content::{
    read_style_registry, write_style_registry, StyleAxisValue, StyleRegistryEntry, MAX_STYLE_AXES,
    STYLE_REGISTRY_MAGIC,
};

/// Helper: create a test entry with given style_id and content_id.
fn make_entry(style_id: u32, content_id: u32) -> StyleRegistryEntry {
    let mut axes = [StyleAxisValue {
        tag: [0; 4],
        value: 0.0,
    }; MAX_STYLE_AXES];
    axes[0] = StyleAxisValue {
        tag: *b"wght",
        value: 400.0,
    };
    axes[1] = StyleAxisValue {
        tag: *b"ital",
        value: 1.0,
    };
    StyleRegistryEntry {
        style_id,
        content_id,
        ascent_fu: 900,
        descent_fu: 200,
        upem: 1000,
        axis_count: 2,
        _pad: 0,
        weight: 400,
        caret_skew: 0,
        axes,
    }
}

#[test]
fn empty_registry_round_trip() {
    let mut buf = [0u8; 1024];
    let written = write_style_registry(&mut buf, &[]);
    assert!(written > 0, "should write header even for empty registry");

    let entries = read_style_registry(&buf).expect("should parse empty registry");
    assert_eq!(entries.len(), 0);
}

#[test]
fn single_entry_round_trip() {
    let mut buf = [0u8; 1024];
    let entry = make_entry(1, 42);
    let written = write_style_registry(&mut buf, &[entry]);
    assert!(written > 0);

    let entries = read_style_registry(&buf).expect("should parse single entry");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].style_id, 1);
    assert_eq!(entries[0].content_id, 42);
    assert_eq!(entries[0].ascent_fu, 900);
    assert_eq!(entries[0].descent_fu, 200);
    assert_eq!(entries[0].upem, 1000);
    assert_eq!(entries[0].axis_count, 2);
    assert_eq!(entries[0].weight, 400);
    assert_eq!(entries[0].caret_skew, 0);
    assert_eq!(&entries[0].axes[0].tag, b"wght");
    assert_eq!(entries[0].axes[0].value, 400.0);
    assert_eq!(&entries[0].axes[1].tag, b"ital");
    assert_eq!(entries[0].axes[1].value, 1.0);
    // Remaining axes should be zeroed.
    for i in 2..MAX_STYLE_AXES {
        assert_eq!(entries[0].axes[i].value, 0.0);
    }
}

#[test]
fn weight_and_caret_skew_round_trip() {
    let mut buf = [0u8; 1024];
    let mut entry = make_entry(0, 10);
    entry.weight = 700;
    entry.caret_skew = -1655; // Inter Italic: -(339/2048) * 10000
    let written = write_style_registry(&mut buf, &[entry]);
    assert!(written > 0);

    let entries = read_style_registry(&buf).expect("should parse");
    assert_eq!(entries[0].weight, 700);
    assert_eq!(entries[0].caret_skew, -1655);
    // Verify decode to f32
    let skew_f32 = entries[0].caret_skew as f32 / 10_000.0;
    assert!((skew_f32 - (-0.1655)).abs() < 0.001);
}

#[test]
fn magic_validation() {
    let mut buf = [0u8; 1024];
    // Write a valid registry, then verify the magic in the buffer.
    let entry = make_entry(1, 10);
    let written = write_style_registry(&mut buf, &[entry]);
    assert!(written > 0);

    // First 4 bytes should be STYLE_REGISTRY_MAGIC (little-endian).
    let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    assert_eq!(magic, STYLE_REGISTRY_MAGIC);

    // Corrupt magic byte — read should fail.
    buf[0] = 0xFF;
    assert!(
        read_style_registry(&buf).is_none(),
        "corrupted magic should return None"
    );

    // Completely zeroed buffer should also fail.
    let zeroed = [0u8; 1024];
    assert!(
        read_style_registry(&zeroed).is_none(),
        "zeroed buffer should return None"
    );
}

#[test]
fn nine_entries_round_trip() {
    // 9 entries exercise multi-entry serialization.
    let mut buf = [0u8; 4096];
    let entries: [StyleRegistryEntry; 9] =
        core::array::from_fn(|i| make_entry(i as u32, 100 + i as u32));
    let written = write_style_registry(&mut buf, &entries);
    assert!(written > 0);

    let read = read_style_registry(&buf).expect("should parse 9 entries");
    assert_eq!(read.len(), 9);
    for i in 0..9 {
        assert_eq!(read[i].style_id, i as u32);
        assert_eq!(read[i].content_id, 100 + i as u32);
        assert_eq!(read[i].ascent_fu, 900);
        assert_eq!(read[i].axis_count, 2);
    }
}

#[test]
fn buffer_too_small() {
    let mut buf = [0u8; 4]; // too small for even the header
    let entry = make_entry(1, 10);
    assert_eq!(write_style_registry(&mut buf, &[entry]), 0);
}

#[test]
fn buffer_too_small_for_entries() {
    // Buffer fits header but not the entry.
    let header_size = core::mem::size_of::<protocol::content::StyleRegistryHeader>();
    let mut buf = vec![0u8; header_size + 1]; // 1 byte short of an entry
    let entry = make_entry(1, 10);
    assert_eq!(write_style_registry(&mut buf, &[entry]), 0);
}

#[test]
fn read_buffer_too_small_for_header() {
    let buf = [0u8; 2]; // smaller than StyleRegistryHeader
    assert!(read_style_registry(&buf).is_none());
}

#[test]
fn read_buffer_too_small_for_entries() {
    // Write a valid single-entry registry, then try reading from a truncated buffer.
    let mut buf = [0u8; 1024];
    let entry = make_entry(1, 10);
    let written = write_style_registry(&mut buf, &[entry]);
    assert!(written > 0);

    // Truncate to just the header — entry_count says 1 but no entry data.
    let header_size = core::mem::size_of::<protocol::content::StyleRegistryHeader>();
    assert!(read_style_registry(&buf[..header_size]).is_none());
}
