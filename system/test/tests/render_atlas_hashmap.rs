//! Host-side unit tests for the hash-map glyph atlas.

#[path = "../../services/drivers/metal-render/atlas.rs"]
mod atlas;

use atlas::{AtlasEntry, GlyphAtlas};

#[test]
fn empty_lookup_returns_none() {
    let atlas = GlyphAtlas::new();
    assert!(atlas.lookup(42, 16, 0).is_none());
    assert!(atlas.lookup(0, 0, 0).is_none());
    assert!(atlas.lookup(100, 32, 7).is_none());
}

#[test]
fn insert_and_lookup() {
    let mut atlas = GlyphAtlas::new();
    let entry = AtlasEntry {
        u: 10,
        v: 20,
        width: 8,
        height: 12,
        bearing_x: 1,
        bearing_y: 10,
    };
    assert!(atlas.insert(42, 16, 0, entry));

    let found = atlas.lookup(42, 16, 0).expect("entry should exist");
    assert_eq!(found.u, 10);
    assert_eq!(found.v, 20);
    assert_eq!(found.width, 8);
    assert_eq!(found.height, 12);
    assert_eq!(found.bearing_x, 1);
    assert_eq!(found.bearing_y, 10);

    // Different key should not match.
    assert!(atlas.lookup(43, 16, 0).is_none());
}

#[test]
fn different_style_id_different_entry() {
    let mut atlas = GlyphAtlas::new();
    let entry_a = AtlasEntry {
        u: 0,
        v: 0,
        width: 10,
        height: 10,
        bearing_x: 0,
        bearing_y: 8,
    };
    let entry_b = AtlasEntry {
        u: 20,
        v: 0,
        width: 12,
        height: 14,
        bearing_x: 1,
        bearing_y: 12,
    };
    assert!(atlas.insert(65, 16, 0, entry_a));
    assert!(atlas.insert(65, 16, 1, entry_b));

    let a = atlas.lookup(65, 16, 0).expect("style 0");
    assert_eq!(a.width, 10);

    let b = atlas.lookup(65, 16, 1).expect("style 1");
    assert_eq!(b.width, 12);
}

#[test]
fn different_font_size_different_entry() {
    let mut atlas = GlyphAtlas::new();
    let entry_small = AtlasEntry {
        u: 0,
        v: 0,
        width: 6,
        height: 8,
        bearing_x: 0,
        bearing_y: 6,
    };
    let entry_large = AtlasEntry {
        u: 10,
        v: 0,
        width: 12,
        height: 16,
        bearing_x: 0,
        bearing_y: 12,
    };
    assert!(atlas.insert(65, 12, 0, entry_small));
    assert!(atlas.insert(65, 24, 0, entry_large));

    let small = atlas.lookup(65, 12, 0).expect("12px");
    assert_eq!(small.width, 6);

    let large = atlas.lookup(65, 24, 0).expect("24px");
    assert_eq!(large.width, 12);

    // Non-existent size.
    assert!(atlas.lookup(65, 18, 0).is_none());
}

#[test]
fn reset_clears_all() {
    let mut atlas = GlyphAtlas::new();
    let entry = AtlasEntry {
        u: 0,
        v: 0,
        width: 5,
        height: 5,
        bearing_x: 0,
        bearing_y: 4,
    };
    assert!(atlas.insert(1, 16, 0, entry));
    assert!(atlas.insert(2, 16, 0, entry));
    assert!(atlas.insert(3, 16, 1, entry));

    assert!(atlas.lookup(1, 16, 0).is_some());
    assert!(atlas.lookup(2, 16, 0).is_some());
    assert!(atlas.lookup(3, 16, 1).is_some());

    atlas.reset();

    assert!(atlas.lookup(1, 16, 0).is_none());
    assert!(atlas.lookup(2, 16, 0).is_none());
    assert!(atlas.lookup(3, 16, 1).is_none());
    assert_eq!(atlas.row_y, 0);
    assert_eq!(atlas.row_x, 0);
    assert_eq!(atlas.row_h, 0);
}

#[test]
fn handles_many_inserts() {
    let mut atlas = GlyphAtlas::new();
    for i in 0..1000u32 {
        let glyph_id = (i & 0xFFFF) as u16;
        let entry = AtlasEntry {
            u: glyph_id,
            v: 0,
            width: 1,
            height: 1,
            bearing_x: 0,
            bearing_y: 0,
        };
        assert!(atlas.insert(glyph_id, 16, i, entry), "insert {i} failed");
    }
    // Verify all are retrievable.
    for i in 0..1000u32 {
        let glyph_id = (i & 0xFFFF) as u16;
        let found = atlas.lookup(glyph_id, 16, i).expect("should exist");
        assert_eq!(found.u, glyph_id);
    }
}

#[test]
fn collision_handling() {
    // 100 entries with the same glyph_id and font_size_px but different style_ids.
    // These will hash to nearby (or identical) initial slots, exercising linear probing.
    let mut atlas = GlyphAtlas::new();
    for style in 0..100u32 {
        let entry = AtlasEntry {
            u: style as u16,
            v: 0,
            width: 1,
            height: 1,
            bearing_x: 0,
            bearing_y: 0,
        };
        assert!(
            atlas.insert(42, 16, style, entry),
            "insert style {style} failed"
        );
    }
    // Verify all are retrievable with correct values.
    for style in 0..100u32 {
        let found = atlas
            .lookup(42, 16, style)
            .unwrap_or_else(|| panic!("style {style} missing"));
        assert_eq!(found.u, style as u16);
    }
}

#[test]
fn pack_and_lookup_by_style_id() {
    let mut atlas = GlyphAtlas::new();
    let data = [128u8; 4]; // 2x2 glyph
    assert!(atlas.pack(65, 16, 0, 2, 2, 0, 2, &data));
    assert!(atlas.pack(65, 16, 1, 2, 2, 0, 2, &data));

    // Lookup with style_id=0 should find the first entry.
    let e0 = atlas.lookup(65, 16, 0).expect("style_id 0");
    assert_eq!(e0.width, 2);
    assert_eq!(e0.u, 0);

    // Lookup with style_id=1 should find a different entry.
    let e1 = atlas.lookup(65, 16, 1).expect("style_id 1");
    assert_eq!(e1.width, 2);
    assert_eq!(e1.u, 2); // packed after the first glyph
}

#[test]
fn pack_writes_pixels_and_creates_entry() {
    let mut atlas = GlyphAtlas::new();
    let data = [0xAA, 0xBB, 0xCC, 0xDD]; // 2x2 glyph
    assert!(atlas.pack(100, 16, 0, 2, 2, 1, 10, &data));

    let entry = atlas.lookup(100, 16, 0).expect("should exist after pack");
    assert_eq!(entry.u, 0);
    assert_eq!(entry.v, 0);
    assert_eq!(entry.width, 2);
    assert_eq!(entry.height, 2);
    assert_eq!(entry.bearing_x, 1);
    assert_eq!(entry.bearing_y, 10);

    // Verify pixel data was written correctly.
    assert_eq!(atlas.pixels[0], 0xAA);
    assert_eq!(atlas.pixels[1], 0xBB);
    assert_eq!(
        atlas.pixels[atlas::ATLAS_WIDTH as usize],
        0xCC
    );
    assert_eq!(
        atlas.pixels[atlas::ATLAS_WIDTH as usize + 1],
        0xDD
    );
}

#[test]
fn pack_advances_row_packer() {
    let mut atlas = GlyphAtlas::new();

    // Pack a 4x6 glyph.
    let data_a = [0u8; 24]; // 4*6
    assert!(atlas.pack(1, 16, 0, 4, 6, 0, 5, &data_a));
    assert_eq!(atlas.row_x, 4);
    assert_eq!(atlas.row_h, 6);

    // Pack another 3x8 glyph on the same row.
    let data_b = [0u8; 24]; // 3*8
    assert!(atlas.pack(2, 16, 0, 3, 8, 0, 7, &data_b));
    assert_eq!(atlas.row_x, 7);
    assert_eq!(atlas.row_h, 8); // updated to taller glyph

    let e1 = atlas.lookup(1, 16, 0).expect("glyph 1");
    assert_eq!(e1.u, 0);
    assert_eq!(e1.v, 0);

    let e2 = atlas.lookup(2, 16, 0).expect("glyph 2");
    assert_eq!(e2.u, 4);
    assert_eq!(e2.v, 0);
}

#[test]
fn insert_overwrites_existing_key() {
    let mut atlas = GlyphAtlas::new();
    let entry_a = AtlasEntry {
        u: 0,
        v: 0,
        width: 5,
        height: 5,
        bearing_x: 0,
        bearing_y: 4,
    };
    let entry_b = AtlasEntry {
        u: 10,
        v: 10,
        width: 8,
        height: 8,
        bearing_x: 1,
        bearing_y: 7,
    };
    assert!(atlas.insert(42, 16, 0, entry_a));
    assert!(atlas.insert(42, 16, 0, entry_b));

    let found = atlas.lookup(42, 16, 0).expect("should exist");
    assert_eq!(found.width, 8);
    assert_eq!(found.u, 10);
}
