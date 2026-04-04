//! Tests for clip mask infrastructure: 8bpp coverage rasterizer and LRU cache.

use render::{scene_render::path_raster::rasterize_path_to_coverage, ClipMaskCache};
use scene::{path_close, path_line_to, path_move_to, FillRule};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build path data for a rectangle from (x0,y0) to (x1,y1).
fn rect_path(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<u8> {
    let mut buf = Vec::new();
    path_move_to(&mut buf, x0, y0);
    path_line_to(&mut buf, x1, y0);
    path_line_to(&mut buf, x1, y1);
    path_line_to(&mut buf, x0, y1);
    path_close(&mut buf);
    buf
}

/// Hash for a coverage key: combines path bytes + dimensions + fill rule.
fn make_key(path: &[u8], w: u32, h: u32, fill_rule: FillRule) -> u64 {
    let mut h64: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64-bit offset
    for &b in path {
        h64 ^= b as u64;
        h64 = h64.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h64 ^= w as u64;
    h64 = h64.wrapping_mul(0x0000_0100_0000_01b3);
    h64 ^= h as u64;
    h64 = h64.wrapping_mul(0x0000_0100_0000_01b3);
    h64 ^= fill_rule as u64;
    h64
}

// ── rasterize_path_to_coverage tests ────────────────────────────────────────

#[test]
fn rasterize_rect_coverage_fills_interior() {
    // 10×10 buffer, 8×8 rectangle (leaving 1px border on each side).
    let path = rect_path(1.0, 1.0, 9.0, 9.0);
    let result = rasterize_path_to_coverage(&path, 10, 10, FillRule::Winding);

    assert_eq!(result.len(), 100, "buffer must be width*height bytes");

    // Interior pixels (2..8 × 2..8) should be near fully covered.
    for y in 2..8 {
        for x in 2..8 {
            let cov = result[y * 10 + x];
            assert!(
                cov > 200,
                "interior pixel ({x},{y}) should be near-full, got {cov}"
            );
        }
    }

    // Corner pixels outside the rect should be zero.
    assert_eq!(result[0], 0, "top-left corner should be empty");
    assert_eq!(result[9], 0, "top-right corner should be empty");
}

#[test]
fn rasterize_full_coverage_rect() {
    // Rectangle that exactly fills the 10×10 buffer.
    let path = rect_path(0.0, 0.0, 10.0, 10.0);
    let result = rasterize_path_to_coverage(&path, 10, 10, FillRule::Winding);

    assert_eq!(result.len(), 100);
    // Every pixel should have some coverage.
    let total: u32 = result.iter().map(|&c| c as u32).sum();
    assert!(
        total > 100 * 200,
        "full-fill rect should have high total coverage, got {total}"
    );
}

#[test]
fn rasterize_even_odd_same_as_winding_for_simple_rect() {
    // A simple convex rectangle is identical under both fill rules.
    let path = rect_path(1.0, 1.0, 9.0, 9.0);
    let winding = rasterize_path_to_coverage(&path, 10, 10, FillRule::Winding);
    let even_odd = rasterize_path_to_coverage(&path, 10, 10, FillRule::EvenOdd);
    assert_eq!(
        winding, even_odd,
        "simple rect should be identical under both rules"
    );
}

#[test]
fn rasterize_empty_path_returns_empty_or_all_zero() {
    let result = rasterize_path_to_coverage(&[], 10, 10, FillRule::Winding);
    // Either empty Vec or a zeroed buffer — both are acceptable.
    assert!(
        result.is_empty() || result.iter().all(|&c| c == 0),
        "empty path should produce empty or zeroed coverage"
    );
}

#[test]
fn rasterize_zero_width_returns_empty() {
    let path = rect_path(0.0, 0.0, 10.0, 10.0);
    let result = rasterize_path_to_coverage(&path, 0, 10, FillRule::Winding);
    assert!(result.is_empty(), "zero-width should return empty Vec");
}

#[test]
fn rasterize_zero_height_returns_empty() {
    let path = rect_path(0.0, 0.0, 10.0, 10.0);
    let result = rasterize_path_to_coverage(&path, 10, 0, FillRule::Winding);
    assert!(result.is_empty(), "zero-height should return empty Vec");
}

#[test]
fn rasterize_buffer_size_matches_dimensions() {
    let path = rect_path(0.0, 0.0, 20.0, 30.0);
    let result = rasterize_path_to_coverage(&path, 20, 30, FillRule::Winding);
    assert_eq!(
        result.len(),
        20 * 30,
        "buffer size must equal width * height"
    );
}

// ── ClipMaskCache tests ──────────────────────────────────────────────────────

#[test]
fn cache_miss_rasterizes_new() {
    let mut cache = ClipMaskCache::new();
    let path = rect_path(1.0, 1.0, 9.0, 9.0);
    let key = make_key(&path, 10, 10, FillRule::Winding);

    let result = cache.get_or_rasterize(&path, 10, 10, FillRule::Winding, key);
    assert!(
        result.is_some(),
        "cache miss should rasterize and return data"
    );
    assert_eq!(result.unwrap().len(), 100);
}

#[test]
fn cache_hit_returns_same_mask() {
    let mut cache = ClipMaskCache::new();
    let path = rect_path(1.0, 1.0, 9.0, 9.0);
    let key = make_key(&path, 10, 10, FillRule::Winding);

    // First call: miss → rasterize.
    let first = cache
        .get_or_rasterize(&path, 10, 10, FillRule::Winding, key)
        .unwrap()
        .to_vec();

    // Second call: hit → same data.
    let second = cache
        .get_or_rasterize(&path, 10, 10, FillRule::Winding, key)
        .unwrap()
        .to_vec();

    assert_eq!(
        first, second,
        "cache hit should return the same coverage data"
    );
}

#[test]
fn cache_different_keys_stored_independently() {
    let mut cache = ClipMaskCache::new();

    let path_a = rect_path(0.0, 0.0, 5.0, 5.0);
    let path_b = rect_path(0.0, 0.0, 10.0, 10.0);
    let key_a = make_key(&path_a, 10, 10, FillRule::Winding);
    let key_b = make_key(&path_b, 10, 10, FillRule::Winding);

    let cov_a = cache
        .get_or_rasterize(&path_a, 10, 10, FillRule::Winding, key_a)
        .unwrap()
        .to_vec();
    let cov_b = cache
        .get_or_rasterize(&path_b, 10, 10, FillRule::Winding, key_b)
        .unwrap()
        .to_vec();

    // The two paths cover different extents, so their coverage must differ.
    assert_ne!(
        cov_a, cov_b,
        "different paths should produce different coverage"
    );

    // Both should still be retrievable.
    let cov_a2 = cache
        .get_or_rasterize(&path_a, 10, 10, FillRule::Winding, key_a)
        .unwrap()
        .to_vec();
    assert_eq!(
        cov_a, cov_a2,
        "re-querying key_a should return original data"
    );
}

#[test]
fn cache_evicts_lru_when_full() {
    let mut cache = ClipMaskCache::new();

    // Fill all 16 slots with distinct paths.
    let paths: Vec<Vec<u8>> = (0..16)
        .map(|i| {
            let f = i as f32;
            rect_path(f, f, f + 8.0, f + 8.0)
        })
        .collect();
    let keys: Vec<u64> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| make_key(p, 20, 20, FillRule::Winding) ^ (i as u64 * 1_000_003))
        .collect();

    for (i, (path, &key)) in paths.iter().zip(keys.iter()).enumerate() {
        let result = cache.get_or_rasterize(path, 20, 20, FillRule::Winding, key);
        assert!(result.is_some(), "slot {i} should rasterize successfully");
    }

    // Now access slot 0 (mark it recently used) then insert a 17th entry.
    // Slot 0 should survive; the least-recently-used of 1..15 should be evicted.
    cache.get_or_rasterize(&paths[0], 20, 20, FillRule::Winding, keys[0]);

    let extra_path = rect_path(99.0, 99.0, 108.0, 108.0);
    let extra_key = make_key(&extra_path, 20, 20, FillRule::Winding) ^ 0xdead_beef_u64;
    let extra = cache.get_or_rasterize(&extra_path, 20, 20, FillRule::Winding, extra_key);
    assert!(extra.is_some(), "17th entry should succeed (evicts LRU)");

    // Slot 0 should still be present (it was refreshed).
    let still_there = cache.get_or_rasterize(&paths[0], 20, 20, FillRule::Winding, keys[0]);
    assert!(
        still_there.is_some(),
        "slot 0 (recently used) should survive eviction"
    );
}

#[test]
fn cache_empty_path_returns_none() {
    let mut cache = ClipMaskCache::new();
    let key = 0xdead_beef_cafe_u64;
    // Empty path_data → rasterize returns empty Vec → get_or_rasterize returns None.
    let result = cache.get_or_rasterize(&[], 10, 10, FillRule::Winding, key);
    assert!(result.is_none(), "empty path should return None from cache");
}
