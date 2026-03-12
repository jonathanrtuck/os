// TrueType font parser — zero-copy, no allocations.
//
// Parses a TTF file from a borrowed byte slice. Extracts glyph outlines,
// metrics, and codepoint mappings. All table access is bounds-checked.
//
// Only handles TrueType outlines (quadratic beziers in `glyf` table).
// Does NOT handle OpenType/CFF fonts.

// ---------------------------------------------------------------------------
// Big-endian readers
// ---------------------------------------------------------------------------

fn read_i16_be(data: &[u8], off: usize) -> Option<i16> {
    if off + 2 > data.len() {
        return None;
    }

    Some(i16::from_be_bytes([data[off], data[off + 1]]))
}
fn read_u8(data: &[u8], off: usize) -> Option<u8> {
    data.get(off).copied()
}
fn read_u16_be(data: &[u8], off: usize) -> Option<u16> {
    if off + 2 > data.len() {
        return None;
    }

    Some(u16::from_be_bytes([data[off], data[off + 1]]))
}
fn read_u32_be(data: &[u8], off: usize) -> Option<u32> {
    if off + 4 > data.len() {
        return None;
    }

    Some(u32::from_be_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[off + 3],
    ]))
}

// ---------------------------------------------------------------------------
// Table locations
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct TableLoc {
    offset: u32,
    length: u32,
}

impl TableLoc {
    fn slice<'a>(&self, data: &'a [u8]) -> Option<&'a [u8]> {
        let start = self.offset as usize;
        let end = start + self.length as usize;

        if end <= data.len() {
            Some(&data[start..end])
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Metrics for a single rasterized glyph.
#[derive(Clone, Copy, Debug)]
pub struct GlyphMetrics {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Horizontal offset from pen position to left edge of bitmap.
    pub bearing_x: i32,
    /// Vertical offset from baseline to top edge of bitmap (positive = up).
    pub bearing_y: i32,
    /// Horizontal advance to next glyph in pixels.
    pub advance: u32,
}
/// Caller-provided buffer for rasterization output (1 byte per pixel coverage).
pub struct RasterBuffer<'a> {
    pub data: &'a mut [u8],
    pub width: u32,
    pub height: u32,
}
/// A parsed TrueType font. Borrows the raw font data — zero-copy.
pub struct TrueTypeFont<'a> {
    data: &'a [u8],
    cmap: TableLoc,
    glyf: TableLoc,
    hmtx: TableLoc,
    loca: TableLoc,
    units_per_em: u16,
    #[allow(dead_code)]
    num_glyphs: u16,
    loca_format: i16, // 0 = short (u16 * 2), 1 = long (u32)
    num_h_metrics: u16,
}

// ---------------------------------------------------------------------------
// Glyph outline (intermediate, used during rasterization)
// ---------------------------------------------------------------------------

/// Maximum points per glyph outline.
const MAX_GLYPH_POINTS: usize = 512;
/// Maximum contours per glyph.
const MAX_CONTOURS: usize = 64;

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
// Font parsing
// ---------------------------------------------------------------------------

impl<'a> TrueTypeFont<'a> {
    /// Parse a TrueType font from raw data. Returns `None` if the data is
    /// not a valid TrueType font or required tables are missing.
    pub fn new(data: &'a [u8]) -> Option<Self> {
        // Minimum: 12-byte offset table header.
        if data.len() < 12 {
            return None;
        }

        // Check sfVersion: must be 0x00010000 (TrueType).
        let sf_version = read_u32_be(data, 0)?;

        if sf_version != 0x0001_0000 {
            return None;
        }

        let num_tables = read_u16_be(data, 4)? as usize;
        // Locate required tables.
        let mut head = TableLoc::default();
        let mut maxp = TableLoc::default();
        let mut cmap = TableLoc::default();
        let mut hhea = TableLoc::default();
        let mut hmtx = TableLoc::default();
        let mut loca = TableLoc::default();
        let mut glyf = TableLoc::default();
        let mut found: u8 = 0;

        for i in 0..num_tables {
            let entry = 12 + i * 16;

            if entry + 16 > data.len() {
                return None;
            }

            let tag = read_u32_be(data, entry)?;
            let offset = read_u32_be(data, entry + 8)?;
            let length = read_u32_be(data, entry + 12)?;
            let loc = TableLoc { offset, length };

            // Validate table doesn't exceed data.
            if (offset as usize) + (length as usize) > data.len() {
                return None;
            }

            match &tag.to_be_bytes() {
                b"head" => {
                    head = loc;
                    found |= 1 << 0;
                }
                b"maxp" => {
                    maxp = loc;
                    found |= 1 << 1;
                }
                b"cmap" => {
                    cmap = loc;
                    found |= 1 << 2;
                }
                b"hhea" => {
                    hhea = loc;
                    found |= 1 << 3;
                }
                b"hmtx" => {
                    hmtx = loc;
                    found |= 1 << 4;
                }
                b"loca" => {
                    loca = loc;
                    found |= 1 << 5;
                }
                b"glyf" => {
                    glyf = loc;
                    found |= 1 << 6;
                }
                _ => {}
            }
        }

        // All 7 required tables present?
        if found != 0x7F {
            return None;
        }

        // Parse scalar values from head, maxp, hhea.
        let head_data = head.slice(data)?;
        let maxp_data = maxp.slice(data)?;
        let hhea_data = hhea.slice(data)?;
        let units_per_em = read_u16_be(head_data, 18)?;
        let loca_format = read_i16_be(head_data, 50)?;
        let num_glyphs = read_u16_be(maxp_data, 4)?;
        let num_h_metrics = read_u16_be(hhea_data, 34)?;

        if units_per_em == 0 || num_glyphs == 0 || num_h_metrics == 0 {
            return None;
        }

        Some(TrueTypeFont {
            data,
            cmap,
            glyf,
            hmtx,
            loca,
            units_per_em,
            num_glyphs,
            loca_format,
            num_h_metrics,
        })
    }

    /// Map a Unicode codepoint to a glyph index via the cmap table.
    /// Returns `None` if the codepoint has no mapping (falls to .notdef).
    pub fn glyph_index(&self, codepoint: char) -> Option<u16> {
        let cmap_data = self.cmap.slice(self.data)?;
        let cp = codepoint as u32;
        // Walk encoding records to find a format 4 subtable.
        let num_subtables = read_u16_be(cmap_data, 2)? as usize;

        for i in 0..num_subtables {
            let rec = 4 + i * 8;
            let platform = read_u16_be(cmap_data, rec)?;
            let encoding = read_u16_be(cmap_data, rec + 2)?;
            let sub_offset = read_u32_be(cmap_data, rec + 4)? as usize;
            // Accept Unicode BMP subtables.
            let is_unicode = platform == 0 || (platform == 3 && (encoding == 1 || encoding == 10));

            if !is_unicode {
                continue;
            }

            if sub_offset >= cmap_data.len() {
                continue;
            }

            let sub = &cmap_data[sub_offset..];
            let format = read_u16_be(sub, 0)?;

            if format == 4 {
                return self.cmap_format4(sub, cp);
            }
        }

        None
    }

    /// Format 4 cmap lookup (segmented mapping for BMP codepoints).
    fn cmap_format4(&self, sub: &[u8], cp: u32) -> Option<u16> {
        if cp > 0xFFFF {
            return None;
        }

        let cp = cp as u16;
        let seg_count = read_u16_be(sub, 6)? / 2;
        let seg_count_usize = seg_count as usize;
        // Array offsets within the subtable.
        let end_codes = 14;
        let start_codes = end_codes + seg_count_usize * 2 + 2; // +2 for reservedPad
        let id_deltas = start_codes + seg_count_usize * 2;
        let id_range_offsets = id_deltas + seg_count_usize * 2;
        // Binary search for the segment containing cp.
        let mut lo: usize = 0;
        let mut hi: usize = seg_count_usize;

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let end_code = read_u16_be(sub, end_codes + mid * 2)?;

            if end_code < cp {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        if lo >= seg_count_usize {
            return None;
        }

        let end_code = read_u16_be(sub, end_codes + lo * 2)?;
        let start_code = read_u16_be(sub, start_codes + lo * 2)?;

        if cp < start_code || cp > end_code {
            return None;
        }

        let id_delta = read_i16_be(sub, id_deltas + lo * 2)?;
        let range_offset_pos = id_range_offsets + lo * 2;
        let id_range_offset = read_u16_be(sub, range_offset_pos)?;
        let glyph_index = if id_range_offset == 0 {
            // Simple delta mapping.
            (cp as i32 + id_delta as i32) as u16
        } else {
            // Offset into glyphIdArray. The offset is relative to its own
            // position in the idRangeOffset array.
            let glyph_offset =
                range_offset_pos + id_range_offset as usize + (cp - start_code) as usize * 2;
            let gi = read_u16_be(sub, glyph_offset)?;

            if gi == 0 {
                return None;
            }

            (gi as i32 + id_delta as i32) as u16
        };

        if glyph_index == 0 {
            None
        } else {
            Some(glyph_index)
        }
    }

    /// Get horizontal metrics for a glyph: (advance_width, left_side_bearing).
    pub fn glyph_h_metrics(&self, glyph_index: u16) -> Option<(u16, i16)> {
        let hmtx_data = self.hmtx.slice(self.data)?;

        if glyph_index < self.num_h_metrics {
            let off = glyph_index as usize * 4;
            let advance = read_u16_be(hmtx_data, off)?;
            let lsb = read_i16_be(hmtx_data, off + 2)?;

            Some((advance, lsb))
        } else {
            // Glyphs beyond num_h_metrics share the last advance width.
            let last_adv_off = (self.num_h_metrics as usize - 1) * 4;
            let advance = read_u16_be(hmtx_data, last_adv_off)?;
            let lsb_off =
                self.num_h_metrics as usize * 4 + (glyph_index - self.num_h_metrics) as usize * 2;
            let lsb = read_i16_be(hmtx_data, lsb_off)?;

            Some((advance, lsb))
        }
    }

    /// Locate a glyph's data in the glyf table. Returns the byte offset and
    /// length within the glyf table, or `None` for empty glyphs (e.g. space).
    fn glyph_location(&self, glyph_index: u16) -> Option<(usize, usize)> {
        let loca_data = self.loca.slice(self.data)?;
        let (off_this, off_next) = if self.loca_format == 0 {
            // Short format: offsets are u16, multiplied by 2.
            let a = read_u16_be(loca_data, glyph_index as usize * 2)? as usize * 2;
            let b = read_u16_be(loca_data, (glyph_index as usize + 1) * 2)? as usize * 2;
            (a, b)
        } else {
            // Long format: offsets are u32.
            let a = read_u32_be(loca_data, glyph_index as usize * 4)? as usize;
            let b = read_u32_be(loca_data, (glyph_index as usize + 1) * 4)? as usize;
            (a, b)
        };

        if off_next <= off_this {
            return None; // Empty glyph (space, .notdef with no outline).
        }

        Some((off_this, off_next - off_this))
    }

    /// Extract the outline for a glyph (simple or compound).
    pub fn glyph_outline(&self, glyph_index: u16, out: &mut GlyphOutline) -> bool {
        out.num_points = 0;
        out.num_contours = 0;

        let (glyf_off, glyf_len) = match self.glyph_location(glyph_index) {
            Some(v) => v,
            None => return false,
        };
        let glyf_data = match self.glyf.slice(self.data) {
            Some(d) => d,
            None => return false,
        };

        if glyf_off + glyf_len > glyf_data.len() {
            return false;
        }

        let g = &glyf_data[glyf_off..glyf_off + glyf_len];
        let num_contours = match read_i16_be(g, 0) {
            Some(n) => n,
            None => return false,
        };

        out.x_min = read_i16_be(g, 2).unwrap_or(0);
        out.y_min = read_i16_be(g, 4).unwrap_or(0);
        out.x_max = read_i16_be(g, 6).unwrap_or(0);
        out.y_max = read_i16_be(g, 8).unwrap_or(0);

        if num_contours < 0 {
            self.glyph_outline_compound(g, out)
        } else if num_contours > 0 {
            Self::glyph_outline_simple(g, num_contours as usize, out)
        } else {
            false
        }
    }

    /// Parse a simple glyph's contours, points, and flags from raw glyf data.
    fn glyph_outline_simple(g: &[u8], nc: usize, out: &mut GlyphOutline) -> bool {
        if nc > MAX_CONTOURS {
            return false;
        }

        let base_contours = out.num_contours as usize;
        let base_points = out.num_points as usize;

        if base_contours + nc > MAX_CONTOURS {
            return false;
        }

        for i in 0..nc {
            out.contour_ends[base_contours + i] = match read_u16_be(g, 10 + i * 2) {
                Some(v) => v + base_points as u16,
                None => return false,
            };
        }

        let local_last = match read_u16_be(g, 10 + (nc - 1) * 2) {
            Some(v) => v as usize,
            None => return false,
        };
        let num_points = local_last + 1;

        if base_points + num_points > MAX_GLYPH_POINTS {
            return false;
        }

        // Skip instructions.
        let instr_off = 10 + nc * 2;
        let instr_len = match read_u16_be(g, instr_off) {
            Some(v) => v as usize,
            None => return false,
        };

        let mut flags = [0u8; MAX_GLYPH_POINTS];
        let mut cursor = instr_off + 2 + instr_len;
        let mut fi = 0;

        while fi < num_points {
            let flag = match read_u8(g, cursor) {
                Some(v) => v,
                None => return false,
            };

            cursor += 1;
            flags[fi] = flag;
            fi += 1;

            if flag & 0x08 != 0 && fi < num_points {
                let repeat = match read_u8(g, cursor) {
                    Some(v) => v as usize,
                    None => return false,
                };

                cursor += 1;

                let end = min_usize(fi + repeat, num_points);

                while fi < end {
                    flags[fi] = flag;
                    fi += 1;
                }
            }
        }

        let mut x: i32 = 0;

        for i in 0..num_points {
            let f = flags[i];

            if f & 0x02 != 0 {
                let dx = read_u8(g, cursor).unwrap_or(0) as i32;

                cursor += 1;
                x += if f & 0x10 != 0 { dx } else { -dx };
            } else if f & 0x10 == 0 {
                let dx = match read_i16_be(g, cursor) {
                    Some(v) => v as i32,
                    None => return false,
                };

                cursor += 2;
                x += dx;
            }

            out.points[base_points + i].x = x;
        }

        let mut y: i32 = 0;

        for i in 0..num_points {
            let f = flags[i];

            if f & 0x04 != 0 {
                let dy = read_u8(g, cursor).unwrap_or(0) as i32;

                cursor += 1;
                y += if f & 0x20 != 0 { dy } else { -dy };
            } else if f & 0x20 == 0 {
                let dy = match read_i16_be(g, cursor) {
                    Some(v) => v as i32,
                    None => return false,
                };

                cursor += 2;
                y += dy;
            }

            out.points[base_points + i].y = y;
        }

        for i in 0..num_points {
            out.points[base_points + i].on_curve = flags[i] & 0x01 != 0;
        }

        out.num_contours = (base_contours + nc) as u16;
        out.num_points = (base_points + num_points) as u16;

        true
    }

    /// Parse a compound glyph: read components and recursively extract
    /// simple outlines, applying x/y offsets to each component's points.
    fn glyph_outline_compound(&self, g: &[u8], out: &mut GlyphOutline) -> bool {
        const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
        const ARGS_ARE_XY_VALUES: u16 = 0x0002;
        const MORE_COMPONENTS: u16 = 0x0020;
        const WE_HAVE_A_SCALE: u16 = 0x0008;
        const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
        const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

        let mut cursor = 10; // skip header (numContours + bbox)
        let mut any = false;

        loop {
            if cursor + 4 > g.len() {
                break;
            }

            let flags = match read_u16_be(g, cursor) {
                Some(v) => v,
                None => break,
            };
            let component_idx = match read_u16_be(g, cursor + 2) {
                Some(v) => v,
                None => break,
            };

            cursor += 4;

            // Read offset arguments.
            let (dx, dy) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                let a = read_i16_be(g, cursor).unwrap_or(0) as i32;
                let b = read_i16_be(g, cursor + 2).unwrap_or(0) as i32;

                cursor += 4;
                (a, b)
            } else {
                if cursor + 2 > g.len() {
                    break;
                }

                let a = g[cursor] as i8 as i32;
                let b = g[cursor + 1] as i8 as i32;

                cursor += 2;
                (a, b)
            };

            // Skip optional scale/transform data (we don't apply transforms).
            if flags & WE_HAVE_A_SCALE != 0 {
                cursor += 2;
            } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                cursor += 4;
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                cursor += 8;
            }

            // Recursively extract the component's outline.
            let (comp_off, comp_len) = match self.glyph_location(component_idx) {
                Some(v) => v,
                None => {
                    if flags & MORE_COMPONENTS == 0 {
                        break;
                    }
                    continue;
                }
            };
            let glyf_data = match self.glyf.slice(self.data) {
                Some(d) => d,
                None => break,
            };

            if comp_off + comp_len > glyf_data.len() {
                if flags & MORE_COMPONENTS == 0 {
                    break;
                }
                continue;
            }

            let comp_g = &glyf_data[comp_off..comp_off + comp_len];
            let comp_nc = match read_i16_be(comp_g, 0) {
                Some(n) if n > 0 => n as usize,
                _ => {
                    if flags & MORE_COMPONENTS == 0 {
                        break;
                    }
                    continue;
                }
            };

            let pts_before = out.num_points as usize;

            if !Self::glyph_outline_simple(comp_g, comp_nc, out) {
                if flags & MORE_COMPONENTS == 0 {
                    break;
                }
                continue;
            }

            // Apply offset to the newly added points.
            if (flags & ARGS_ARE_XY_VALUES != 0) && (dx != 0 || dy != 0) {
                let pts_after = out.num_points as usize;

                for i in pts_before..pts_after {
                    out.points[i].x += dx;
                    out.points[i].y += dy;
                }
            }

            any = true;

            if flags & MORE_COMPONENTS == 0 {
                break;
            }
        }

        any
    }

    /// Rasterize a glyph into the provided coverage buffer.
    ///
    /// `size_px` is the font size in pixels (height of the em square).
    /// The buffer receives a coverage map (0–255 per pixel). Returns metrics
    /// describing the bitmap dimensions and positioning, or `None` if the
    /// glyph cannot be rasterized (missing, empty, or exceeds buffer).
    pub fn rasterize(
        &self,
        codepoint: char,
        size_px: u32,
        buffer: &mut RasterBuffer,
        scratch: &mut RasterScratch,
    ) -> Option<GlyphMetrics> {
        let glyph_index = self.glyph_index(codepoint)?;
        let (advance_fu, lsb_fu) = self.glyph_h_metrics(glyph_index)?;

        if !self.glyph_outline(glyph_index, &mut scratch.outline) {
            // Empty glyph (e.g., space). Return metrics with zero bitmap.
            let advance = scale_fu(advance_fu as i32, size_px, self.units_per_em) as u32;

            return Some(GlyphMetrics {
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
                advance,
            });
        }

        // Read bounding box values before passing scratch mutably.
        let upem = self.units_per_em;
        let x_min_fu = scratch.outline.x_min;
        let y_min_fu = scratch.outline.y_min;
        let x_max_fu = scratch.outline.x_max;
        let y_max_fu = scratch.outline.y_max;
        // Scale bounding box to pixels.
        let x_min_px = scale_fu_floor(x_min_fu as i32, size_px, upem);
        let y_min_px = scale_fu_floor(y_min_fu as i32, size_px, upem);
        let x_max_px = scale_fu_ceil(x_max_fu as i32, size_px, upem);
        let y_max_px = scale_fu_ceil(y_max_fu as i32, size_px, upem);
        let _ = y_min_px; // used for height calc via y_max - y_min
        let bmp_w = (x_max_px - x_min_px) as u32;
        let bmp_h = (y_max_px - y_min_px) as u32;

        if bmp_w == 0 || bmp_h == 0 {
            let advance = scale_fu(advance_fu as i32, size_px, self.units_per_em) as u32;

            return Some(GlyphMetrics {
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
                advance,
            });
        }

        if bmp_w > buffer.width || bmp_h > buffer.height {
            return None; // Exceeds caller's buffer.
        }

        // 2D oversampling: rasterize at OVERSAMPLE_X × width, then
        // downsample horizontally for smoother edges on vertical/diagonal
        // strokes. Vertical oversampling (OVERSAMPLE_Y) is already handled
        // by sub-scanlines within rasterize_segments.
        let over_w = bmp_w * OVERSAMPLE_X as u32;
        let over_total = (over_w * bmp_h) as usize;

        if over_total > buffer.data.len() {
            return None;
        }

        // Clear the oversampled coverage region.
        for b in buffer.data[..over_total].iter_mut() {
            *b = 0;
        }

        // Flatten outline into line segments (at 1× pixel coordinates).
        scratch.num_segments = 0;

        flatten_outline_from_scratch(scratch, size_px, upem, x_min_px, y_max_px);

        // Scale segment x-coordinates by OVERSAMPLE_X for wider rasterization.
        for i in 0..scratch.num_segments {
            scratch.segments[i].x0 *= OVERSAMPLE_X;
            scratch.segments[i].x1 *= OVERSAMPLE_X;
        }

        // Rasterize at oversampled width.
        rasterize_segments(scratch, &mut buffer.data[..over_total], over_w, bmp_h);

        // Downsample horizontally: average OVERSAMPLE_X adjacent samples per
        // output pixel. The oversampled buffer is over_w × bmp_h; the output
        // is bmp_w × bmp_h written into the beginning of the same buffer.
        // Safe to write in-place because dst_idx <= src_base for all pixels
        // (output row is narrower than oversampled row, and we process
        // left-to-right, top-to-bottom).
        let ox = OVERSAMPLE_X as u32;

        for row in 0..bmp_h {
            for col in 0..bmp_w {
                let src_base = (row * over_w + col * ox) as usize;
                let mut sum = 0u32;

                for s in 0..ox {
                    sum += buffer.data[src_base + s as usize] as u32;
                }

                let dst_idx = (row * bmp_w + col) as usize;

                buffer.data[dst_idx] = (sum / ox) as u8;
            }
        }

        let advance = scale_fu(advance_fu as i32, size_px, self.units_per_em) as u32;
        let bearing_x = scale_fu(lsb_fu as i32, size_px, upem);
        let bearing_y = y_max_px;

        Some(GlyphMetrics {
            width: bmp_w,
            height: bmp_h,
            bearing_x,
            bearing_y,
            advance,
        })
    }

    /// Returns units-per-em for external use (e.g., computing line height).
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }
}

// ---------------------------------------------------------------------------
// Coordinate scaling helpers (integer only)
// ---------------------------------------------------------------------------

fn min_usize(a: usize, b: usize) -> usize {
    if a < b {
        a
    } else {
        b
    }
}
/// Scale a value in font units to pixels: `val * size_px / units_per_em`.
fn scale_fu(val: i32, size_px: u32, upem: u16) -> i32 {
    ((val as i64 * size_px as i64) / upem as i64) as i32
}
/// Scale and round toward positive infinity (ceil).
fn scale_fu_ceil(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;

    if n > 0 {
        ((n + d - 1) / d) as i32
    } else {
        (n / d) as i32
    }
}
/// Scale and round toward negative infinity (floor).
fn scale_fu_floor(val: i32, size_px: u32, upem: u16) -> i32 {
    let n = val as i64 * size_px as i64;
    let d = upem as i64;

    if n < 0 {
        ((n - d + 1) / d) as i32
    } else {
        (n / d) as i32
    }
}
