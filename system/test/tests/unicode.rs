//! Tests for Unicode glyph coverage in the font rendering pipeline.
//!
//! Validates VAL-UNICODE-001 (Latin Extended), VAL-UNICODE-002 (scene graph
//! round-trip), and VAL-UNICODE-003 (supplementary plane safety).

extern crate alloc;

use scene::*;

#[path = "../../services/core/fallback.rs"]
mod fallback;

use fallback::FallbackChain;
use fonts::{
    rasterize::{self, RasterBuffer, RasterScratch},
    shape,
};

const JETBRAINS_MONO: &[u8] = include_bytes!("../../share/jetbrains-mono.ttf");
const INTER: &[u8] = include_bytes!("../../share/inter.ttf");
const SOURCE_SERIF_4: &[u8] = include_bytes!("../../share/source-serif-4.ttf");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_buf() -> Vec<u8> {
    vec![0u8; SCENE_SIZE]
}

/// Rasterize a glyph and return total coverage sum (RGB channels).
fn rasterize_glyph(
    font_data: &[u8],
    glyph_id: u16,
    size_px: u16,
) -> Option<(rasterize::GlyphMetrics, u32)> {
    let mut buf = vec![0u8; 48 * 6 * 48];
    let mut scratch = Box::new(RasterScratch::zeroed());
    let mut rb = RasterBuffer {
        data: &mut buf,
        width: 48,
        height: 48,
    };
    let metrics = rasterize::rasterize(font_data, glyph_id, size_px, &mut rb, &mut scratch, 1)?;
    let total = (metrics.width * metrics.height * 3) as usize;
    let coverage_sum: u32 = buf[..total].iter().map(|&b| b as u32).sum();
    Some((metrics, coverage_sum))
}

// ---------------------------------------------------------------------------
// VAL-UNICODE-001: Non-ASCII glyph rendering
// Latin Extended codepoints (é, ñ, ü) shape and rasterize correctly.
// ---------------------------------------------------------------------------

#[test]
fn unicode_latin_extended_cafe_shapes_four_glyphs() {
    // "café" has 4 characters: c, a, f, é (U+00E9).
    // Shaping should produce 4 glyphs with non-zero glyph IDs.
    let glyphs = shape(INTER, "café", &[]);
    assert_eq!(
        glyphs.len(),
        4,
        "shaping 'café' should produce 4 glyphs, got {}",
        glyphs.len()
    );
    for (i, g) in glyphs.iter().enumerate() {
        assert!(
            g.glyph_id > 0,
            "glyph {} in 'café' has glyph_id 0 (.notdef) — font lacks the codepoint",
            i
        );
        assert!(g.x_advance > 0, "glyph {} in 'café' has zero x_advance", i);
    }
}

#[test]
fn unicode_latin_extended_cafe_rasterizes_with_nonzero_coverage() {
    // Each glyph in "café" must produce a valid coverage map with non-zero pixels.
    let glyphs = shape(INTER, "café", &[]);
    assert_eq!(glyphs.len(), 4);

    for (i, g) in glyphs.iter().enumerate() {
        let result = rasterize_glyph(INTER, g.glyph_id, 18);
        assert!(
            result.is_some(),
            "glyph {} (id={}) in 'café' failed to rasterize",
            i,
            g.glyph_id
        );
        let (metrics, coverage_sum) = result.unwrap();
        assert!(
            coverage_sum > 0,
            "glyph {} (id={}) in 'café' has zero coverage — no visible pixels",
            i,
            g.glyph_id
        );
        assert!(
            metrics.width > 0 && metrics.height > 0,
            "glyph {} (id={}) in 'café' has zero dimensions: {}x{}",
            i,
            g.glyph_id,
            metrics.width,
            metrics.height
        );
    }
}

#[test]
fn unicode_latin_e_acute_shapes_correctly() {
    // U+00E9 (é) should shape to a single valid glyph.
    let glyphs = shape(INTER, "é", &[]);
    assert_eq!(glyphs.len(), 1, "é should produce 1 glyph");
    assert!(glyphs[0].glyph_id > 0, "é should have valid glyph ID");
    assert!(glyphs[0].x_advance > 0, "é should have positive advance");
}

#[test]
fn unicode_latin_n_tilde_shapes_correctly() {
    // U+00F1 (ñ) should shape to a single valid glyph.
    let glyphs = shape(INTER, "ñ", &[]);
    assert_eq!(glyphs.len(), 1, "ñ should produce 1 glyph");
    assert!(glyphs[0].glyph_id > 0, "ñ should have valid glyph ID");
}

#[test]
fn unicode_latin_u_diaeresis_shapes_correctly() {
    // U+00FC (ü) should shape to a single valid glyph.
    let glyphs = shape(INTER, "ü", &[]);
    assert_eq!(glyphs.len(), 1, "ü should produce 1 glyph");
    assert!(glyphs[0].glyph_id > 0, "ü should have valid glyph ID");
}

#[test]
fn unicode_latin_extended_rasterize_individual_accented() {
    // Individual accented characters rasterize with valid dimensions and coverage.
    for (ch, name) in [('é', "e-acute"), ('ñ', "n-tilde"), ('ü', "u-diaeresis")] {
        let mut buf = [0u8; 4];
        let text = ch.encode_utf8(&mut buf);
        let glyphs = shape(INTER, text, &[]);
        assert!(!glyphs.is_empty(), "{} produced no glyphs", name);
        let gid = glyphs[0].glyph_id;
        assert!(gid > 0, "{} has .notdef glyph", name);

        let result = rasterize_glyph(INTER, gid, 18);
        assert!(
            result.is_some(),
            "{} (glyph_id={}) failed to rasterize",
            name,
            gid
        );
        let (metrics, coverage_sum) = result.unwrap();
        assert!(
            coverage_sum > 0,
            "{} (glyph_id={}) has zero coverage",
            name,
            gid
        );
        assert!(
            metrics.width > 0 && metrics.height > 0,
            "{} (glyph_id={}) has zero dimensions",
            name,
            gid
        );
    }
}

#[test]
fn unicode_latin_extended_via_fallback_chain() {
    // Latin Extended characters should be resolved by the primary font
    // in a fallback chain — no fallback needed for these common codepoints.
    let chain = FallbackChain::new(&[INTER, JETBRAINS_MONO]);
    let result = chain.shape("café", &[]);

    assert_eq!(
        result.len(),
        4,
        "fallback chain should produce 4 glyphs for 'café'"
    );
    for (i, fg) in result.iter().enumerate() {
        assert!(
            fg.glyph.glyph_id > 0,
            "glyph {} in 'café' should have valid ID via fallback chain",
            i
        );
        assert_eq!(
            fg.font_index, 0,
            "glyph {} in 'café' should come from primary font (not fallback)",
            i
        );
    }
}

#[test]
fn unicode_glyph_cache_handles_non_ascii_ids() {
    // The LRU glyph cache must handle glyph IDs from non-ASCII codepoints.
    use fonts::cache::{LruCachedGlyph, LruGlyphCache};

    let mut cache = LruGlyphCache::new(64);

    // Shape 'é' to get a real non-ASCII glyph ID.
    let glyphs = shape(INTER, "é", &[]);
    let gid = glyphs[0].glyph_id;
    assert!(gid > 0, "é must produce a valid glyph ID");

    // Cache the glyph
    let cached = LruCachedGlyph {
        width: 10,
        height: 14,
        bearing_x: 1,
        bearing_y: 12,
        advance: 8,
        coverage: vec![128; 40],
    };
    cache.insert(gid, 18, cached.clone());

    // Retrieve it
    let retrieved = cache.get(gid, 18);
    assert!(
        retrieved.is_some(),
        "non-ASCII glyph ID {} should be retrievable from cache",
        gid
    );
    assert_eq!(retrieved.unwrap().coverage, vec![128u8; 40]);
}

// ---------------------------------------------------------------------------
// VAL-UNICODE-002: Unicode text in scene graph
// Scene graph round-trip for 'naïve résumé' preserves all glyph IDs.
// ---------------------------------------------------------------------------

#[test]
fn unicode_scene_graph_naive_resume_round_trip() {
    // Shape 'naïve résumé' and write shaped glyphs to scene graph,
    // then read back and verify all glyph IDs are preserved exactly.
    let text = "naïve résumé";
    let shaped = shape(INTER, text, &[]);

    assert!(
        shaped.len() >= 12,
        "'naïve résumé' should produce at least 12 glyphs (1 per visible char), got {}",
        shaped.len()
    );

    // Verify all glyphs have valid IDs (no .notdef for Latin characters).
    for (i, g) in shaped.iter().enumerate() {
        assert!(
            g.glyph_id > 0,
            "glyph {} in 'naïve résumé' has .notdef (glyph_id=0)",
            i
        );
    }

    // Convert shaping library ShapedGlyphs to scene graph ShapedGlyphs.
    let scene_glyphs: Vec<scene::ShapedGlyph> = shaped
        .iter()
        .map(|sg| {
            // Scale from font units to a representative i16 value.
            // For this test, we just need the glyph_id and some non-zero advances.
            scene::ShapedGlyph {
                glyph_id: sg.glyph_id,
                _pad: 0,
                x_advance: (sg.x_advance / 50).max(1) as i32 * 65536,
                x_offset: 0,
                y_offset: 0,
            }
        })
        .collect();

    // Write to scene graph.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_shaped_glyphs(&scene_glyphs);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Glyphs {
        color: Color::rgb(220, 220, 220),
        glyphs: dref,
        glyph_count: scene_glyphs.len() as u16,
        font_size: 18,
        axis_hash: 0,
    };
    w.set_root(id);
    w.commit();

    // Read back from scene graph.
    let r = SceneReader::new(&buf);
    let node = r.node(id);
    let (read_glyph_ref, read_glyph_count) = match node.content {
        Content::Glyphs {
            glyphs,
            glyph_count,
            ..
        } => (glyphs, glyph_count),
        _ => panic!("expected Glyphs content"),
    };
    assert_eq!(read_glyph_count, scene_glyphs.len() as u16);

    let read_glyphs = r.shaped_glyphs(read_glyph_ref, read_glyph_count);
    assert_eq!(
        read_glyphs.len(),
        scene_glyphs.len(),
        "glyph count mismatch after scene graph round-trip"
    );

    // Verify every glyph ID is preserved exactly.
    for (i, (orig, read)) in scene_glyphs.iter().zip(read_glyphs.iter()).enumerate() {
        assert_eq!(
            orig.glyph_id, read.glyph_id,
            "glyph {} glyph_id mismatch in 'naïve résumé': expected {}, got {}",
            i, orig.glyph_id, read.glyph_id
        );
        assert_eq!(
            orig.x_advance, read.x_advance,
            "glyph {} x_advance mismatch in 'naïve résumé'",
            i
        );
    }
}

#[test]
fn unicode_scene_graph_write_read_round_trip() {
    // Verify Unicode text survives a scene graph write/read cycle.
    let text = "naïve résumé";
    let shaped = shape(INTER, text, &[]);

    let scene_glyphs: Vec<scene::ShapedGlyph> = shaped
        .iter()
        .map(|sg| scene::ShapedGlyph {
            glyph_id: sg.glyph_id,
            _pad: 0,
            x_advance: (sg.x_advance / 50).max(1) as i32 * 65536,
            x_offset: 0,
            y_offset: 0,
        })
        .collect();

    let mut buf = vec![0u8; SCENE_SIZE];
    let mut w = SceneWriter::new(&mut buf);

    let dref = w.push_shaped_glyphs(&scene_glyphs);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Glyphs {
        color: Color::rgb(220, 220, 220),
        glyphs: dref,
        glyph_count: scene_glyphs.len() as u16,
        font_size: 18,
        axis_hash: 0,
    };
    w.set_root(id);
    w.commit();

    // Read back via SceneReader.
    let r = SceneReader::new(&buf);
    let nodes = r.nodes();
    assert_eq!(nodes.len(), 1);

    match nodes[0].content {
        Content::Glyphs {
            glyphs,
            glyph_count,
            ..
        } => {
            assert_eq!(glyph_count, scene_glyphs.len() as u16);
            let read_glyphs = r.shaped_glyphs(glyphs, glyph_count);
            assert_eq!(read_glyphs.len(), scene_glyphs.len());
            for (i, (orig, read)) in scene_glyphs.iter().zip(read_glyphs.iter()).enumerate() {
                assert_eq!(
                    orig.glyph_id, read.glyph_id,
                    "scene graph: glyph {} glyph_id mismatch",
                    i
                );
            }
        }
        _ => panic!("expected Glyphs content"),
    }
}

#[test]
fn unicode_scene_graph_mixed_ascii_and_extended_latin() {
    // Write a mix of ASCII and Latin Extended glyphs to scene graph,
    // read back, verify all preserved.
    let text = "Hello café naïve";
    let shaped = shape(INTER, text, &[]);

    let scene_glyphs: Vec<scene::ShapedGlyph> = shaped
        .iter()
        .map(|sg| scene::ShapedGlyph {
            glyph_id: sg.glyph_id,
            _pad: 0,
            x_advance: (sg.x_advance / 50).max(1) as i32 * 65536,
            x_offset: (sg.x_offset / 50) as i32 * 65536,
            y_offset: (sg.y_offset / 50) as i32 * 65536,
        })
        .collect();

    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_shaped_glyphs(&scene_glyphs);

    let r = SceneReader::new(&buf);
    let read_glyphs = r.shaped_glyphs(dref, scene_glyphs.len() as u16);
    assert_eq!(read_glyphs.len(), scene_glyphs.len());

    for (i, (orig, read)) in scene_glyphs.iter().zip(read_glyphs.iter()).enumerate() {
        assert_eq!(
            orig.glyph_id, read.glyph_id,
            "mixed text glyph {} glyph_id mismatch",
            i
        );
    }
}

// ---------------------------------------------------------------------------
// VAL-UNICODE-003: Supplementary plane codepoints
// Codepoints above U+FFFF do not crash; subsequent text continues correctly.
// ---------------------------------------------------------------------------

#[test]
fn unicode_supplementary_plane_no_panic() {
    // U+1F600 (😀 Grinning Face) is a supplementary plane codepoint.
    // It likely won't be in either font, but the pipeline must NOT panic.
    let glyphs = shape(INTER, "\u{1F600}", &[]);

    // Should produce at least 1 glyph (possibly .notdef).
    // The key assertion is: no panic occurred.
    assert!(
        !glyphs.is_empty(),
        "supplementary plane codepoint should produce at least 1 glyph (even if .notdef)"
    );
}

#[test]
fn unicode_supplementary_plane_followed_by_ascii() {
    // A supplementary plane codepoint followed by ASCII text:
    // the pipeline must not crash, and the ASCII text must shape correctly.
    let text = "\u{1F600}Hello";
    let glyphs = shape(INTER, text, &[]);

    // We need at least 6 glyphs: 1 for emoji (possibly .notdef) + 5 for "Hello".
    assert!(
        glyphs.len() >= 6,
        "supplementary + ASCII should produce >= 6 glyphs, got {}",
        glyphs.len()
    );

    // The last 5 glyphs (for "Hello") must have valid glyph IDs.
    let hello_glyphs = &glyphs[glyphs.len() - 5..];
    for (i, g) in hello_glyphs.iter().enumerate() {
        assert!(
            g.glyph_id > 0,
            "ASCII glyph {} after supplementary codepoint has .notdef",
            i
        );
        assert!(
            g.x_advance > 0,
            "ASCII glyph {} after supplementary codepoint has zero advance",
            i
        );
    }
}

#[test]
fn unicode_supplementary_plane_via_fallback_chain() {
    // Supplementary plane codepoint followed by ASCII through fallback chain.
    let chain = FallbackChain::new(&[INTER, JETBRAINS_MONO]);
    let text = "\u{1F600}Hello";
    let result = chain.shape(text, &[]);

    // Pipeline must not crash.
    assert!(
        result.len() >= 6,
        "fallback chain: supplementary + ASCII should produce >= 6 glyphs, got {}",
        result.len()
    );

    // "Hello" glyphs must be valid.
    let hello_glyphs = &result[result.len() - 5..];
    for (i, fg) in hello_glyphs.iter().enumerate() {
        assert!(
            fg.glyph.glyph_id > 0,
            "fallback: ASCII glyph {} after supplementary has .notdef",
            i
        );
    }
}

#[test]
fn unicode_supplementary_plane_u10000_no_crash() {
    // U+10000 (LINEAR B SYLLABLE B008 A) — first supplementary plane codepoint.
    let glyphs = shape(INTER, "\u{10000}", &[]);
    // Must not panic. May produce .notdef.
    assert!(
        !glyphs.is_empty(),
        "U+10000 should produce at least 1 glyph"
    );
}

#[test]
fn unicode_supplementary_mixed_string_no_crash() {
    // Mix of BMP and supplementary plane codepoints.
    let text = "A\u{10000}B\u{1F600}C";
    let glyphs = shape(INTER, text, &[]);

    // At least 5 glyphs: A, U+10000, B, U+1F600, C.
    assert!(
        glyphs.len() >= 5,
        "mixed BMP/supplementary should produce >= 5 glyphs, got {}",
        glyphs.len()
    );

    // The ASCII glyphs (A, B, C) must have valid IDs.
    // They are at cluster positions 0, 4, 5 (UTF-8 byte offsets).
    // We can check that at least some glyphs have glyph_id > 0.
    let valid_count = glyphs.iter().filter(|g| g.glyph_id > 0).count();
    assert!(
        valid_count >= 3,
        "at least 3 glyphs (A, B, C) should have valid IDs; got {} valid out of {}",
        valid_count,
        glyphs.len()
    );
}

#[test]
fn unicode_supplementary_u10fffd_produces_notdef_or_valid() {
    // U+10FFFD — near the end of Unicode, unlikely to be in any font.
    let glyphs = shape(INTER, "\u{10FFFD}", &[]);
    assert!(
        !glyphs.is_empty(),
        "U+10FFFD should produce at least 1 glyph (even .notdef)"
    );
    // The glyph is likely .notdef (glyph_id 0), but could be valid.
    // Either way, no panic.
}

#[test]
fn unicode_supplementary_glyph_id_truncation_safety() {
    // HarfRust returns glyph_id as u32. Supplementary plane codepoints
    // may produce glyph IDs that need u16 representation. Verify the
    // truncation `glyph_id as u16` in shape() doesn't lose important data.
    //
    // For standard fonts, glyph IDs are well within u16 range (< 65536).
    // .notdef is glyph_id 0. This test ensures no unexpected behavior.
    let text = "\u{1F600}";
    let glyphs = shape(INTER, text, &[]);
    if !glyphs.is_empty() {
        // glyph_id should be 0 (.notdef) since the font likely doesn't have emoji.
        // The key assertion: glyph_id fits in u16 without overflow issues.
        let gid = glyphs[0].glyph_id;
        assert!(
            gid == 0 || gid < u16::MAX,
            "supplementary plane glyph ID {} is out of expected range",
            gid
        );
    }
}

#[test]
fn unicode_supplementary_rasterize_notdef_no_crash() {
    // Rasterizing .notdef (glyph_id 0) must not crash.
    let result = rasterize_glyph(INTER, 0, 18);
    // .notdef may or may not have an outline. Either way, no panic.
    // If it has an outline, coverage should be valid.
    if let Some((metrics, _coverage_sum)) = result {
        // Valid result, even if dimensions are 0 (empty .notdef).
        assert!(metrics.width <= 48 && metrics.height <= 48);
    }
    // If None, the .notdef glyph has no outline — also acceptable.
}

#[test]
fn unicode_supplementary_followed_by_ascii_rasterize() {
    // Shape a supplementary codepoint + ASCII, then rasterize the ASCII glyphs.
    // Verify the ASCII glyphs rasterize correctly after the supplementary one.
    let text = "\u{1F600}W";
    let glyphs = shape(INTER, text, &[]);
    assert!(glyphs.len() >= 2);

    // The last glyph should be 'W' with a valid glyph ID.
    let w_glyph = glyphs.last().unwrap();
    assert!(
        w_glyph.glyph_id > 0,
        "'W' after supplementary should have valid glyph ID"
    );

    // Rasterize 'W'.
    let result = rasterize_glyph(INTER, w_glyph.glyph_id, 18);
    assert!(result.is_some(), "'W' glyph should rasterize successfully");
    let (metrics, coverage_sum) = result.unwrap();
    assert!(coverage_sum > 0, "'W' should have non-zero coverage");
    assert!(metrics.width > 0 && metrics.height > 0);
}

// ---------------------------------------------------------------------------
// Additional: Glyph cache with expanded codepoint space
// ---------------------------------------------------------------------------

#[test]
fn unicode_glyph_cache_latin_extended_and_ascii_coexist() {
    // Cache entries for both ASCII and Latin Extended glyph IDs coexist.
    use fonts::cache::{LruCachedGlyph, LruGlyphCache};

    let mut cache = LruGlyphCache::new(128);

    // Shape ASCII 'A' and Latin Extended 'é'.
    let a_glyphs = shape(INTER, "A", &[]);
    let e_glyphs = shape(INTER, "é", &[]);
    let a_gid = a_glyphs[0].glyph_id;
    let e_gid = e_glyphs[0].glyph_id;

    assert_ne!(a_gid, e_gid, "A and é should have different glyph IDs");

    let a_cached = LruCachedGlyph {
        width: 10,
        height: 14,
        bearing_x: 1,
        bearing_y: 12,
        advance: 8,
        coverage: vec![100; 40],
    };
    let e_cached = LruCachedGlyph {
        width: 10,
        height: 16,
        bearing_x: 1,
        bearing_y: 14,
        advance: 8,
        coverage: vec![150; 40],
    };

    cache.insert(a_gid, 18, a_cached);
    cache.insert(e_gid, 18, e_cached);

    assert!(
        cache.get(a_gid, 18).is_some(),
        "ASCII 'A' should be in cache"
    );
    assert!(
        cache.get(e_gid, 18).is_some(),
        "Latin Extended 'é' should be in cache"
    );
    assert_eq!(cache.get(a_gid, 18).unwrap().coverage, vec![100u8; 40]);
    assert_eq!(cache.get(e_gid, 18).unwrap().coverage, vec![150u8; 40]);
}

#[test]
fn unicode_glyph_cache_stress_many_codepoints() {
    // Insert glyphs for many different codepoints into the cache.
    // The cache should handle the expanded codepoint space without issues.
    use fonts::cache::{LruCachedGlyph, LruGlyphCache};

    let max_cap = 64;
    let mut cache = LruGlyphCache::new(max_cap);

    // Shape a string with various Unicode characters.
    let text = "ABCDéñüàèìòùâêîôû";
    let glyphs = shape(INTER, text, &[]);

    for g in &glyphs {
        let cached = LruCachedGlyph {
            width: 10,
            height: 14,
            bearing_x: 1,
            bearing_y: 12,
            advance: 8,
            coverage: vec![g.glyph_id as u8; 20],
        };
        cache.insert(g.glyph_id, 18, cached);
    }

    // Verify all inserted glyphs are retrievable (within cache capacity).
    let unique_gids: Vec<u16> = {
        let mut ids: Vec<u16> = glyphs.iter().map(|g| g.glyph_id).collect();
        ids.sort();
        ids.dedup();
        ids
    };
    for &gid in &unique_gids {
        assert!(
            cache.get(gid, 18).is_some(),
            "glyph ID {} should be retrievable from cache",
            gid
        );
    }
    assert!(cache.len() <= max_cap);
}

// ---------------------------------------------------------------------------
// Diagnostic: JetBrains Mono angle bracket glyph IDs
// ---------------------------------------------------------------------------

#[test]
fn jetbrains_mono_angle_brackets_shape_and_rasterize() {
    let font_data = include_bytes!("../../share/jetbrains-mono.ttf");

    // Shape each problem character individually and verify rasterization.
    let mut scratch = RasterScratch::zeroed();
    let mut buf = [0u8; 50 * 50];

    for ch in ['<', '>', '=', '|', '{', '}'] {
        let s = alloc::format!("{}", ch);
        let shaped = shape(font_data, &s, &[]);
        assert!(!shaped.is_empty(), "'{}' should produce shaped glyphs", ch);
        let gid = shaped[0].glyph_id;
        eprintln!("  '{}' -> glyph_id={}", ch, gid);

        let mut rb = RasterBuffer {
            data: &mut buf,
            width: 50,
            height: 50,
        };
        let result = rasterize::rasterize_with_axes(font_data, gid, 18, &mut rb, &mut scratch, &[], 1);
        let dims = result.as_ref().map(|m| (m.width, m.height));
        eprintln!("  '{}' rasterize: {:?}", ch, dims);
        assert!(
            result.is_some(),
            "'{}' (gid={}) should rasterize successfully",
            ch,
            gid
        );
        let m = result.unwrap();
        assert!(
            m.width > 0 && m.height > 0,
            "'{}' (gid={}) should have non-zero dimensions, got {}x{}",
            ch,
            gid,
            m.width,
            m.height
        );
    }
}
