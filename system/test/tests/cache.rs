//! Tests for the LRU glyph cache.

use fonts::cache::{LruCachedGlyph, LruGlyphCache};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a dummy LruCachedGlyph with identifiable coverage data.
fn dummy_glyph(tag: u8) -> LruCachedGlyph {
    LruCachedGlyph {
        width: 10 + tag as u32,
        height: 12 + tag as u32,
        bearing_x: tag as i32,
        bearing_y: tag as i32 + 5,
        advance: 8 + tag as u32,
        coverage: vec![tag; 30], // identifiable by tag byte
    }
}

// ---------------------------------------------------------------------------
// VAL-CACHE-001: Glyph-ID-keyed cache — store/retrieve by (glyph_id, font_size)
// ---------------------------------------------------------------------------

#[test]
fn cache_store_and_retrieve_basic() {
    let mut cache = LruGlyphCache::new(64);
    let glyph = dummy_glyph(42);
    cache.insert(65, 18, glyph.clone());

    let retrieved = cache.get(65, 18);
    assert!(retrieved.is_some(), "inserted glyph must be retrievable");
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.width, 10 + 42);
    assert_eq!(retrieved.coverage, vec![42u8; 30]);
}

#[test]
fn cache_miss_returns_none() {
    let mut cache = LruGlyphCache::new(64);
    assert!(cache.get(65, 18).is_none(), "empty cache returns None");
}

#[test]
fn cache_different_font_sizes_are_independent() {
    // Same glyph ID at different font sizes = different cache entries
    let mut cache = LruGlyphCache::new(64);
    let glyph_18 = dummy_glyph(18);
    let glyph_24 = dummy_glyph(24);

    cache.insert(65, 18, glyph_18.clone());
    cache.insert(65, 24, glyph_24.clone());

    let r18 = cache.get(65, 18).unwrap();
    assert_eq!(r18.coverage, vec![18u8; 30]);
    let r24 = cache.get(65, 24).unwrap();
    assert_eq!(r24.coverage, vec![24u8; 30]);
}

#[test]
fn cache_glyph_id_above_127() {
    // VAL-CACHE-001: Glyph IDs beyond ASCII range (> 127) are cached correctly.
    let mut cache = LruGlyphCache::new(64);
    let glyph = dummy_glyph(99);
    cache.insert(500, 18, glyph.clone());

    let retrieved = cache.get(500, 18).unwrap();
    assert_eq!(retrieved.width, 10 + 99);
    assert_eq!(retrieved.coverage, vec![99u8; 30]);
}

#[test]
fn cache_glyph_id_zero() {
    // .notdef glyph (ID 0) can be cached
    let mut cache = LruGlyphCache::new(64);
    let glyph = dummy_glyph(0);
    cache.insert(0, 18, glyph.clone());

    let retrieved = cache.get(0, 18).unwrap();
    assert_eq!(retrieved.advance, 8);
}

#[test]
fn cache_glyph_id_max() {
    // Maximum glyph ID (u16::MAX - 1 = 65534) can be cached
    let mut cache = LruGlyphCache::new(64);
    let glyph = dummy_glyph(77);
    cache.insert(65534, 18, glyph.clone());

    let retrieved = cache.get(65534, 18).unwrap();
    assert_eq!(retrieved.coverage, vec![77u8; 30]);
}

// ---------------------------------------------------------------------------
// VAL-CACHE-002: LRU eviction
// ---------------------------------------------------------------------------

#[test]
fn cache_lru_evicts_least_recently_used() {
    let mut cache = LruGlyphCache::new(3);

    // Insert 3 entries (fills cache to capacity).
    cache.insert(1, 18, dummy_glyph(1));
    cache.insert(2, 18, dummy_glyph(2));
    cache.insert(3, 18, dummy_glyph(3));
    assert_eq!(cache.len(), 3);

    // Insert a 4th — should evict glyph 1 (oldest/LRU).
    cache.insert(4, 18, dummy_glyph(4));
    assert_eq!(cache.len(), 3);
    assert!(cache.get(1, 18).is_none(), "glyph 1 should be evicted");
    assert!(cache.get(2, 18).is_some(), "glyph 2 should survive");
    assert!(cache.get(3, 18).is_some(), "glyph 3 should survive");
    assert!(cache.get(4, 18).is_some(), "glyph 4 should be present");
}

#[test]
fn cache_lru_access_refreshes_entry() {
    let mut cache = LruGlyphCache::new(3);

    cache.insert(1, 18, dummy_glyph(1));
    cache.insert(2, 18, dummy_glyph(2));
    cache.insert(3, 18, dummy_glyph(3));

    // Access glyph 1 to refresh it (moves to most-recently-used).
    let _ = cache.get(1, 18);

    // Insert glyph 4 — should evict glyph 2 (now the LRU).
    cache.insert(4, 18, dummy_glyph(4));
    assert!(
        cache.get(1, 18).is_some(),
        "glyph 1 was accessed, should survive"
    );
    assert!(
        cache.get(2, 18).is_none(),
        "glyph 2 is LRU, should be evicted"
    );
    assert!(cache.get(3, 18).is_some(), "glyph 3 should survive");
    assert!(cache.get(4, 18).is_some(), "glyph 4 should be present");
}

#[test]
fn cache_insert_existing_key_updates_and_refreshes() {
    let mut cache = LruGlyphCache::new(3);

    cache.insert(1, 18, dummy_glyph(1));
    cache.insert(2, 18, dummy_glyph(2));
    cache.insert(3, 18, dummy_glyph(3));

    // Re-insert glyph 1 with new data — should update + refresh.
    cache.insert(1, 18, dummy_glyph(99));
    assert_eq!(cache.len(), 3);

    // Insert glyph 4 — should evict glyph 2 (LRU since 1 was refreshed).
    cache.insert(4, 18, dummy_glyph(4));
    let r = cache.get(1, 18).unwrap();
    assert_eq!(
        r.coverage,
        vec![99u8; 30],
        "glyph 1 should have updated data"
    );
    assert!(
        cache.get(2, 18).is_none(),
        "glyph 2 is LRU after 1 was refreshed"
    );
}

// ---------------------------------------------------------------------------
// VAL-CACHE-003: Bounded memory — cache.len() never exceeds max capacity
// ---------------------------------------------------------------------------

#[test]
fn cache_bounded_under_heavy_insertion() {
    let max_cap = 32;
    let mut cache = LruGlyphCache::new(max_cap);

    // Insert 10x capacity of unique glyphs.
    for i in 0..(max_cap * 10) as u16 {
        cache.insert(i, 18, dummy_glyph((i % 256) as u8));
        assert!(
            cache.len() <= max_cap,
            "cache.len() = {} exceeds max_capacity = {} after inserting glyph {}",
            cache.len(),
            max_cap,
            i
        );
    }
}

#[test]
fn cache_len_tracks_entries_correctly() {
    let mut cache = LruGlyphCache::new(64);
    assert_eq!(cache.len(), 0);

    cache.insert(1, 18, dummy_glyph(1));
    assert_eq!(cache.len(), 1);

    cache.insert(2, 18, dummy_glyph(2));
    assert_eq!(cache.len(), 2);

    // Re-insert same key — len stays the same.
    cache.insert(1, 18, dummy_glyph(11));
    assert_eq!(cache.len(), 2);
}

#[test]
fn cache_capacity_one() {
    // Edge case: cache with capacity 1.
    let mut cache = LruGlyphCache::new(1);

    cache.insert(1, 18, dummy_glyph(1));
    assert_eq!(cache.len(), 1);
    assert!(cache.get(1, 18).is_some());

    cache.insert(2, 18, dummy_glyph(2));
    assert_eq!(cache.len(), 1);
    assert!(cache.get(1, 18).is_none(), "glyph 1 evicted");
    assert!(cache.get(2, 18).is_some(), "glyph 2 present");
}

#[test]
fn cache_large_capacity_stress() {
    let max_cap = 256;
    let mut cache = LruGlyphCache::new(max_cap);

    // Fill to capacity.
    for i in 0..max_cap as u16 {
        cache.insert(i, 12, dummy_glyph((i % 256) as u8));
    }
    assert_eq!(cache.len(), max_cap);

    // Verify all entries present.
    for i in 0..max_cap as u16 {
        assert!(cache.get(i, 12).is_some(), "glyph {} should be cached", i);
    }

    // Insert one more — evicts glyph 0 (LRU).
    cache.insert(1000, 12, dummy_glyph(0));
    assert_eq!(cache.len(), max_cap);
    assert!(
        cache.get(0, 12).is_none(),
        "glyph 0 should be evicted (LRU)"
    );
    assert!(cache.get(1000, 12).is_some());
}

// ---------------------------------------------------------------------------
// Additional: API completeness
// ---------------------------------------------------------------------------

#[test]
fn cache_preserves_all_metrics_fields() {
    let mut cache = LruGlyphCache::new(16);
    let glyph = LruCachedGlyph {
        width: 15,
        height: 20,
        bearing_x: -3,
        bearing_y: 18,
        advance: 12,
        coverage: vec![128, 64, 32, 16],
    };
    cache.insert(42, 24, glyph.clone());

    let r = cache.get(42, 24).unwrap();
    assert_eq!(r.width, 15);
    assert_eq!(r.height, 20);
    assert_eq!(r.bearing_x, -3);
    assert_eq!(r.bearing_y, 18);
    assert_eq!(r.advance, 12);
    assert_eq!(r.coverage, vec![128, 64, 32, 16]);
}

// ---------------------------------------------------------------------------
// VAL-CACHE-004: Axis hash keying — same glyph at different axes are independent
// ---------------------------------------------------------------------------

#[test]
fn cache_axis_hash_independent_entries() {
    let mut cache = LruGlyphCache::new(64);
    let g_default = LruCachedGlyph {
        width: 10,
        height: 12,
        bearing_x: 1,
        bearing_y: 10,
        advance: 8,
        coverage: vec![100; 120],
    };
    let g_bold = LruCachedGlyph {
        width: 11,
        height: 13,
        bearing_x: 2,
        bearing_y: 11,
        advance: 9,
        coverage: vec![200; 143],
    };

    // Same glyph ID + font size, different axis hashes.
    cache.insert_with_axes(42, 18, 0, g_default.clone());
    cache.insert_with_axes(42, 18, 0xABCD, g_bold.clone());

    let r_default = cache.get_with_axes(42, 18, 0).unwrap();
    assert_eq!(r_default.width, 10);
    assert_eq!(r_default.coverage[0], 100);

    let r_bold = cache.get_with_axes(42, 18, 0xABCD).unwrap();
    assert_eq!(r_bold.width, 11);
    assert_eq!(r_bold.coverage[0], 200);
}

#[test]
fn cache_axis_hash_miss_different_hash() {
    let mut cache = LruGlyphCache::new(64);
    let glyph = dummy_glyph(50);
    cache.insert_with_axes(42, 18, 0, glyph);

    // Same glyph ID + font size, different axis hash = miss.
    assert!(cache.get_with_axes(42, 18, 1).is_none());
}

// ---------------------------------------------------------------------------
// VAL-CACHE-005: LruRasterizer integration
// ---------------------------------------------------------------------------

#[test]
fn lru_rasterizer_struct_fields() {
    // Verify LruRasterizer can be constructed and its cache accessed.
    let lru = render::LruRasterizer::new_test(64);
    assert_eq!(lru.cache.len(), 0);
}

#[test]
fn lru_rasterizer_manual_insert_and_get() {
    // Pre-populate the LRU cache manually and verify retrieval.
    let mut lru = render::LruRasterizer::new_test(64);
    let glyph = LruCachedGlyph {
        width: 8,
        height: 10,
        bearing_x: 1,
        bearing_y: 9,
        advance: 7,
        coverage: vec![255; 80],
    };
    lru.cache.insert(500, 18, glyph);
    assert_eq!(lru.cache.len(), 1);
    assert!(lru.cache.get(500, 18).is_some());
}

// ---------------------------------------------------------------------------
// VAL-CACHE-006: CpuBackend has LRU cache initialized
// ---------------------------------------------------------------------------

#[test]
fn cpu_backend_lru_starts_empty() {
    // Load the JetBrains Mono font embedded in the test binary.
    let font_data = include_bytes!("../../share/jetbrains-mono.ttf");
    let backend =
        render::CpuBackend::new(font_data, None, 18, 96, 1.0, 1024, 768).expect("backend init");
    assert_eq!(backend.lru.cache.len(), 0, "LRU cache should start empty");
}

#[test]
fn cpu_backend_ascii_cache_populated() {
    // Verify the fixed ASCII cache has entries after construction.
    let font_data = include_bytes!("../../share/jetbrains-mono.ttf");
    let backend =
        render::CpuBackend::new(font_data, None, 18, 96, 1.0, 1024, 768).expect("backend init");

    // 'A' (0x41) should be in the ASCII cache.
    let glyph_id = fonts::rasterize::glyph_id_for_char(font_data, 'A');
    assert!(glyph_id.is_some(), "font should have glyph for 'A'");
    let glyph_id = glyph_id.unwrap();
    assert!(
        backend.mono_cache.get(glyph_id).is_some(),
        "ASCII 'A' should be in the fixed cache"
    );
}
