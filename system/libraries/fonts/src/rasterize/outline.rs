//! Glyph outline extraction from TrueType fonts.
//!
//! Reads glyf/loca tables via read-fonts to produce a decoded `GlyphOutline`
//! (contours of on-curve and off-curve points in font units). Handles both
//! simple and composite glyphs.

use read_fonts::{FontRef, TableProvider};

// ---------------------------------------------------------------------------
// Glyph outline types
// ---------------------------------------------------------------------------

/// Maximum points per glyph outline.
pub(crate) const MAX_GLYPH_POINTS: usize = 512;
/// Maximum contours per glyph.
pub(crate) const MAX_CONTOURS: usize = 64;

/// Decoded glyph outline — contours of on-curve and off-curve points.
pub struct GlyphOutline {
    pub points: [GlyphPoint; MAX_GLYPH_POINTS],
    pub num_points: u16,
    pub contour_ends: [u16; MAX_CONTOURS],
    pub num_contours: u16,
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

/// A point in a glyph outline, in font units.
#[derive(Clone, Copy, Default)]
pub struct GlyphPoint {
    pub x: i32,
    pub y: i32,
    pub on_curve: bool,
}

impl GlyphOutline {
    pub const fn zeroed() -> Self {
        GlyphOutline {
            points: [GlyphPoint {
                x: 0,
                y: 0,
                on_curve: false,
            }; MAX_GLYPH_POINTS],
            num_points: 0,
            contour_ends: [0u16; MAX_CONTOURS],
            num_contours: 0,
            x_min: 0,
            y_min: 0,
            x_max: 0,
            y_max: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// read-fonts outline extraction
// ---------------------------------------------------------------------------

/// Extract glyph outline from font data using read-fonts.
///
/// Populates `outline` with contour points for the given glyph ID.
/// Returns `(advance_width_fu, lsb_fu, upem)` on success, or None if
/// the glyph has no outline (e.g., space) or the glyph ID is invalid.
pub(crate) fn extract_outline(
    font_data: &[u8],
    glyph_id: u16,
    outline: &mut GlyphOutline,
) -> Option<(u16, i16, u16)> {
    let font = FontRef::new(font_data).ok()?;

    // Get units_per_em
    let head = font.head().ok()?;
    let upem = head.units_per_em();

    // Get horizontal metrics
    let hmtx = font.hmtx().ok()?;
    let hhea = font.hhea().ok()?;
    let num_h_metrics = hhea.number_of_h_metrics();
    let gid = read_fonts::types::GlyphId::new(glyph_id as u32);

    let (advance_fu, lsb_fu) = if (glyph_id as u16) < num_h_metrics {
        let metrics = hmtx.h_metrics();
        let m = metrics.get(glyph_id as usize)?;
        (m.advance.get(), m.side_bearing.get())
    } else {
        // Glyphs beyond num_h_metrics share the last advance width
        let metrics = hmtx.h_metrics();
        let last = metrics.get(num_h_metrics as usize - 1)?;
        let advance = last.advance.get();
        let lsb_data = hmtx.left_side_bearings();
        let lsb_idx = (glyph_id as usize).checked_sub(num_h_metrics as usize)?;
        let lsb = lsb_data.get(lsb_idx).map(|v| v.get()).unwrap_or(0);
        (advance, lsb)
    };

    // Get glyph outline from glyf table
    let loca = font.loca(None).ok()?;
    let glyf = font.glyf().ok()?;

    outline.num_points = 0;
    outline.num_contours = 0;

    // Get the glyph data
    let glyph_data = loca.get_glyf(gid, &glyf).ok()??;

    match glyph_data {
        read_fonts::tables::glyf::Glyph::Simple(simple) => {
            // Extract bounding box
            outline.x_min = simple.x_min();
            outline.y_min = simple.y_min();
            outline.x_max = simple.x_max();
            outline.y_max = simple.y_max();

            // Extract contours and points
            let num_contours = simple.number_of_contours() as usize;
            if num_contours > MAX_CONTOURS {
                return None;
            }

            let end_pts = simple.end_pts_of_contours();
            for (i, ep) in end_pts.iter().enumerate() {
                if i >= MAX_CONTOURS {
                    return None;
                }
                outline.contour_ends[i] = ep.get();
            }
            outline.num_contours = num_contours as u16;

            // Iterate points
            let mut pt_idx = 0usize;
            let num_points = simple.num_points();
            if num_points > MAX_GLYPH_POINTS {
                return None;
            }

            for point in simple.points() {
                if pt_idx >= MAX_GLYPH_POINTS {
                    return None;
                }
                outline.points[pt_idx] = GlyphPoint {
                    x: point.x as i32,
                    y: point.y as i32,
                    on_curve: point.on_curve,
                };
                pt_idx += 1;
            }
            outline.num_points = pt_idx as u16;
        }
        read_fonts::tables::glyf::Glyph::Composite(composite) => {
            // Extract bounding box
            outline.x_min = composite.x_min();
            outline.y_min = composite.y_min();
            outline.x_max = composite.x_max();
            outline.y_max = composite.y_max();

            // For composite glyphs, recursively extract component outlines
            for component in composite.components() {
                let comp_gid = component.glyph.to_u32() as u16;
                let flags = component.flags;

                // Get component offsets
                let (dx, dy) = match component.anchor {
                    read_fonts::tables::glyf::Anchor::Offset { x, y } => (x as i32, y as i32),
                    _ => (0, 0),
                };

                // Recursively extract the component outline
                let pts_before = outline.num_points as usize;
                let contours_before = outline.num_contours as usize;

                // Get component glyph data
                let comp_gid_rf = read_fonts::types::GlyphId::new(comp_gid as u32);
                if let Ok(Some(comp_data)) = loca.get_glyf(comp_gid_rf, &glyf) {
                    match comp_data {
                        read_fonts::tables::glyf::Glyph::Simple(comp_simple) => {
                            let comp_nc = comp_simple.number_of_contours() as usize;
                            if contours_before + comp_nc > MAX_CONTOURS {
                                continue;
                            }

                            let comp_end_pts = comp_simple.end_pts_of_contours();
                            for (i, ep) in comp_end_pts.iter().enumerate() {
                                outline.contour_ends[contours_before + i] =
                                    ep.get() + pts_before as u16;
                            }
                            outline.num_contours = (contours_before + comp_nc) as u16;

                            let mut pt_idx = pts_before;
                            for point in comp_simple.points() {
                                if pt_idx >= MAX_GLYPH_POINTS {
                                    break;
                                }
                                outline.points[pt_idx] = GlyphPoint {
                                    x: point.x as i32 + dx,
                                    y: point.y as i32 + dy,
                                    on_curve: point.on_curve,
                                };
                                pt_idx += 1;
                            }
                            outline.num_points = pt_idx as u16;
                        }
                        _ => {
                            // Nested composites not supported — fall back to .notdef
                            return None;
                        }
                    }
                }

                let _ = flags; // flags used above for anchor type
            }

            if outline.num_points == 0 {
                return None;
            }
        }
    }

    Some((advance_fu, lsb_fu, upem))
}
