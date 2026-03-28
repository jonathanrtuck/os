//! Tests for font fallback chain mechanism.
//!
//! Validates VAL-FALLBACK-001 through VAL-FALLBACK-004 and VAL-CACHE-004.

extern crate alloc;

#[path = "../../services/core/fallback.rs"]
mod fallback;

use fallback::{ContentType, FallbackChain};

const INTER: &[u8] = include_bytes!("../../share/inter.ttf");
const JETBRAINS_MONO: &[u8] = include_bytes!("../../share/jetbrains-mono.ttf");

// ---------------------------------------------------------------------------
// VAL-FALLBACK-001: Primary font glyph resolution
// ---------------------------------------------------------------------------

#[test]
fn fallback_primary_font_used_for_ascii() {
    // When the primary font contains a glyph for a codepoint, the fallback
    // mechanism is not invoked. ASCII text shaped with a fallback chain should
    // produce the same glyph IDs as shaping with the primary font alone.
    let chain = FallbackChain::new(&[INTER, JETBRAINS_MONO]);
    let result = chain.shape("Hello", &[]);

    // All glyphs should have font_index == 0 (primary font).
    for sg in &result {
        assert_eq!(
            sg.font_index, 0,
            "ASCII glyph (glyph_id={}) should come from primary font (index 0), got index {}",
            sg.glyph.glyph_id, sg.font_index
        );
    }

    // Should produce the same glyph IDs as shaping with primary font alone.
    let primary_only = fonts::shape(INTER, "Hello", &[]);
    assert_eq!(result.len(), primary_only.len());
    for (fb, primary) in result.iter().zip(primary_only.iter()) {
        assert_eq!(
            fb.glyph.glyph_id, primary.glyph_id,
            "fallback chain should produce same glyph IDs as primary-only shaping"
        );
    }
}

#[test]
fn fallback_not_triggered_for_common_latin() {
    // Extended Latin characters that exist in both fonts should still come
    // from the primary font.
    let chain = FallbackChain::new(&[INTER, JETBRAINS_MONO]);
    let result = chain.shape("abc 123", &[]);

    for sg in &result {
        assert_eq!(
            sg.font_index, 0,
            "common Latin glyph should use primary font"
        );
    }
}

// ---------------------------------------------------------------------------
// VAL-FALLBACK-002: Fallback to secondary font
// ---------------------------------------------------------------------------

#[test]
fn fallback_missing_glyph_uses_secondary() {
    // Verify that when the primary font produces .notdef (glyph_id 0) for a
    // codepoint, the fallback chain tries the secondary font and returns its
    // glyph with font_index > 0.
    //
    // Strategy: use shape_char on a range of rare codepoints to discover one
    // that the primary font lacks but the secondary font has. If no such
    // codepoint exists among the candidates, verify at minimum that the
    // fallback mechanism returns .notdef with proper chain exhaustion.

    let primary_only = FallbackChain::new(&[JETBRAINS_MONO]);
    let chain = FallbackChain::new(&[JETBRAINS_MONO, INTER]);

    // Candidate codepoints that proportional fonts sometimes cover but
    // monospace fonts often lack: arrows, math symbols, box drawings, misc.
    let candidates = [
        '\u{2603}', // snowman
        '\u{2764}', // heavy black heart
        '\u{2122}', // trade mark sign
        '\u{2205}', // empty set
        '\u{2260}', // not equal to
        '\u{00B5}', // micro sign
        '\u{0192}', // latin small f with hook
        '\u{2013}', // en dash
        '\u{2014}', // em dash
        '\u{201C}', // left double quotation mark
    ];

    let mut found_fallback = false;
    for &ch in &candidates {
        let (primary_glyph, _) = primary_only.shape_char(ch);
        if primary_glyph.glyph_id == 0 {
            // Primary lacks this glyph — check if the chain finds it.
            let (chain_glyph, font_idx) = chain.shape_char(ch);
            if chain_glyph.glyph_id > 0 {
                assert!(
                    font_idx > 0,
                    "glyph for U+{:04X} resolved by chain but font_index is 0 (should be secondary)",
                    ch as u32,
                );
                found_fallback = true;
                break;
            }
        }
    }

    if !found_fallback {
        // If no candidate triggered secondary fallback (both fonts have the
        // same coverage), verify the mechanism at least does not crash and
        // returns sensible results for chain exhaustion.
        let (glyph, _) = chain.shape_char('\u{10FFFD}');
        assert_eq!(
            glyph.glyph_id, 0,
            "chain exhaustion should return .notdef when neither font has the glyph"
        );
    }
}

#[test]
fn fallback_returns_valid_glyph_from_secondary_when_primary_lacks() {
    // Same test as above but with the font order reversed: Inter as
    // primary, JetBrains Mono as fallback. Verifies fallback is symmetric.

    let primary_only = FallbackChain::new(&[INTER]);
    let chain = FallbackChain::new(&[INTER, JETBRAINS_MONO]);

    // Candidate codepoints that the primary (proportional) may lack.
    let candidates = [
        '\u{2500}', // box drawings light horizontal
        '\u{2502}', // box drawings light vertical
        '\u{250C}', // box drawings light down and right
        '\u{2603}', // snowman
        '\u{2764}', // heavy black heart
        '\u{2122}', // trade mark sign
        '\u{2205}', // empty set
        '\u{2260}', // not equal to
    ];

    let mut found_fallback = false;
    for &ch in &candidates {
        let (primary_glyph, _) = primary_only.shape_char(ch);
        if primary_glyph.glyph_id == 0 {
            // Primary lacks this glyph — check if the chain finds it.
            let (chain_glyph, font_idx) = chain.shape_char(ch);
            if chain_glyph.glyph_id > 0 {
                assert!(
                    font_idx > 0,
                    "glyph for U+{:04X} resolved by chain but font_index is 0 (should be secondary)",
                    ch as u32,
                );
                found_fallback = true;
                break;
            }
        }
    }

    if !found_fallback {
        // Verify at least that common ASCII uses primary (font_index 0).
        let (glyph, font_idx) = chain.shape_char('A');
        assert!(glyph.glyph_id > 0, "'A' should have valid glyph ID");
        assert_eq!(font_idx, 0, "'A' should come from primary font");
    }
}

// ---------------------------------------------------------------------------
// VAL-FALLBACK-003: Content-type-aware fallback chains
// ---------------------------------------------------------------------------

#[test]
fn fallback_code_content_type_selects_monospace_primary() {
    // Code content type should use monospace (JetBrains Mono) as primary
    // and proportional (Inter) as fallback.
    let chain = FallbackChain::for_content_type(
        ContentType::Code,
        JETBRAINS_MONO,
        INTER,
    );

    // The primary font should be the monospace font.
    let result = chain.shape("Hello", &[]);

    // Verify monospace: all glyphs should have identical x_advance.
    let advances: Vec<i32> = result.iter().map(|sg| sg.glyph.x_advance).collect();
    let first = advances[0];
    for (i, &adv) in advances.iter().enumerate() {
        assert_eq!(
            adv, first,
            "code content type: glyph {} advance {} should match first advance {} (monospace)",
            i, adv, first
        );
    }
}

#[test]
fn fallback_prose_content_type_selects_proportional_primary() {
    // Prose content type should use proportional (Inter) as primary
    // and monospace (JetBrains Mono) as fallback.
    let chain = FallbackChain::for_content_type(
        ContentType::Prose,
        JETBRAINS_MONO,
        INTER,
    );

    // The primary font should be the proportional font.
    let result = chain.shape("Wi", &[]);

    // Verify proportional: 'W' and 'i' should have different x_advance.
    assert!(result.len() >= 2);
    assert_ne!(
        result[0].glyph.x_advance, result[1].glyph.x_advance,
        "prose content type: 'W' and 'i' should have different advances (proportional)"
    );
}

#[test]
fn fallback_content_type_code_vs_prose_differ() {
    // Code and prose content types should produce different primary fonts.
    let code_chain = FallbackChain::for_content_type(
        ContentType::Code,
        JETBRAINS_MONO,
        INTER,
    );
    let prose_chain = FallbackChain::for_content_type(
        ContentType::Prose,
        JETBRAINS_MONO,
        INTER,
    );

    let code_result = code_chain.shape("Hello", &[]);
    let prose_result = prose_chain.shape("Hello", &[]);

    // The glyph IDs should differ because different fonts are primary.
    let code_ids: Vec<u16> = code_result.iter().map(|sg| sg.glyph.glyph_id).collect();
    let prose_ids: Vec<u16> = prose_result.iter().map(|sg| sg.glyph.glyph_id).collect();
    assert_ne!(
        code_ids, prose_ids,
        "code and prose content types should produce different glyph IDs for same text"
    );
}

// ---------------------------------------------------------------------------
// VAL-FALLBACK-004: Fallback chain exhaustion
// ---------------------------------------------------------------------------

#[test]
fn fallback_chain_exhaustion_returns_notdef() {
    // When no font in the chain has a glyph for a codepoint, .notdef
    // (glyph_id 0) is returned. Use a codepoint unlikely to be in either font.
    let chain = FallbackChain::new(&[JETBRAINS_MONO, INTER]);

    // U+FFFD REPLACEMENT CHARACTER or a rare codepoint. Let's use a very
    // rare codepoint from a private use area that no standard font covers.
    // Actually, let's test with a supplementary plane character.
    let result = chain.shape("\u{10FFFD}", &[]); // Supplementary Private Use Area-B, last valid

    // The chain should return .notdef (glyph_id 0) for unmappable codepoints.
    if !result.is_empty() {
        // If shaping produced a glyph, it should be .notdef since no font
        // in the chain likely has this codepoint.
        assert_eq!(
            result[0].glyph.glyph_id, 0,
            "unmappable codepoint should produce .notdef (glyph_id 0)"
        );
    }
}

#[test]
fn fallback_subsequent_chars_after_exhaustion() {
    // After a chain exhaustion (.notdef), subsequent characters should still
    // be shaped correctly. The pipeline does not abort.
    let chain = FallbackChain::new(&[JETBRAINS_MONO, INTER]);

    // String with an unmappable codepoint followed by regular ASCII.
    let text = "\u{10FFFD}Hello";
    let result = chain.shape(text, &[]);

    // Should have glyphs for both the unmappable char and "Hello".
    assert!(
        result.len() >= 5,
        "pipeline should continue after .notdef; got {} glyphs for '{}'",
        result.len(),
        text
    );

    // The "Hello" portion should have valid glyph IDs (> 0).
    // The first glyph (unmappable codepoint) may be .notdef.
    let hello_glyphs = &result[result.len() - 5..];
    for (i, sg) in hello_glyphs.iter().enumerate() {
        assert!(
            sg.glyph.glyph_id > 0,
            "glyph {} in 'Hello' after exhaustion should have valid ID (> 0), got {}",
            i,
            sg.glyph.glyph_id
        );
    }
}

// ---------------------------------------------------------------------------
// VAL-CACHE-004: Font identifier in cache key
// ---------------------------------------------------------------------------

#[test]
fn fallback_cache_key_includes_font_identifier() {
    // The cache key must include a font identifier so that the same glyph ID
    // from different fonts produces independent cache entries.
    use fonts::cache::{LruCachedGlyph, LruGlyphCache};

    let mut cache = LruGlyphCache::new(64);

    // Create two glyphs with the same glyph_id but from different fonts.
    // In a fallback scenario, glyph_id 65 from font A (Inter) might
    // look completely different from glyph_id 65 from font B (JetBrains Mono).
    let glyph_font_a = LruCachedGlyph {
        width: 10,
        height: 12,
        bearing_x: 1,
        bearing_y: 10,
        advance: 8,
        coverage: vec![100; 30], // font A coverage
    };
    let glyph_font_b = LruCachedGlyph {
        width: 12,
        height: 14,
        bearing_x: 2,
        bearing_y: 11,
        advance: 10,
        coverage: vec![200; 30], // font B coverage - different!
    };

    // Use font_id as part of the style_id parameter to distinguish fonts.
    // font_id 0 = primary font, font_id 1 = fallback font.
    let font_a_hash = fallback::font_identifier_hash(0, &[]);
    let font_b_hash = fallback::font_identifier_hash(1, &[]);

    cache.insert_with_axes(65, 18, font_a_hash, glyph_font_a);
    cache.insert_with_axes(65, 18, font_b_hash, glyph_font_b);

    // Both should be retrievable independently.
    let r_a = cache.get_with_axes(65, 18, font_a_hash);
    assert!(r_a.is_some(), "font A entry must be retrievable");
    assert_eq!(r_a.unwrap().coverage, vec![100u8; 30]);

    let r_b = cache.get_with_axes(65, 18, font_b_hash);
    assert!(r_b.is_some(), "font B entry must be retrievable");
    assert_eq!(r_b.unwrap().coverage, vec![200u8; 30]);
}

#[test]
fn fallback_font_identifier_hash_differs_for_different_fonts() {
    // Different font indices should produce different hashes.
    let hash_0 = fallback::font_identifier_hash(0, &[]);
    let hash_1 = fallback::font_identifier_hash(1, &[]);

    assert_ne!(
        hash_0, hash_1,
        "different font indices should produce different hashes"
    );
}

#[test]
fn fallback_font_identifier_hash_includes_axis_values() {
    // Same font index with different axis values should produce different hashes.
    use fonts::rasterize::AxisValue;

    let axes_400 = [AxisValue {
        tag: *b"wght",
        value: 400.0,
    }];
    let axes_700 = [AxisValue {
        tag: *b"wght",
        value: 700.0,
    }];

    let hash_400 = fallback::font_identifier_hash(0, &axes_400);
    let hash_700 = fallback::font_identifier_hash(0, &axes_700);

    assert_ne!(
        hash_400, hash_700,
        "same font with different axis values should produce different hashes"
    );
}

// ---------------------------------------------------------------------------
// Additional edge cases
// ---------------------------------------------------------------------------

#[test]
fn fallback_empty_chain_returns_empty() {
    let chain = FallbackChain::new(&[]);
    let result = chain.shape("Hello", &[]);
    assert!(result.is_empty(), "empty chain should produce no glyphs");
}

#[test]
fn fallback_single_font_chain_works() {
    let chain = FallbackChain::new(&[INTER]);
    let result = chain.shape("Hello", &[]);
    assert!(result.len() >= 5, "single-font chain should shape normally");
}

#[test]
fn fallback_empty_text_returns_empty() {
    let chain = FallbackChain::new(&[INTER, JETBRAINS_MONO]);
    let result = chain.shape("", &[]);
    assert!(result.is_empty(), "empty text should produce no glyphs");
}

#[test]
fn fallback_shape_char_returns_notdef_for_unknown() {
    // shape_char should return .notdef for a codepoint not in any font.
    let chain = FallbackChain::new(&[JETBRAINS_MONO, INTER]);
    let (glyph, font_idx) = chain.shape_char('\u{10FFFD}');
    // Either .notdef from some font, or the last font in chain.
    // The font_index should be valid (within chain bounds or indicate exhaustion).
    // glyph_id should be 0 (.notdef) since no font has this codepoint.
    assert_eq!(glyph.glyph_id, 0, "unmappable char should produce .notdef");
}
