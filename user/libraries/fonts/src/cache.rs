//! Glyph cache types for pre-rasterized glyphs.
//!
//! Fixed-size ASCII cache (`GlyphCache`) for fast path rendering, and an
//! LRU cache (`LruGlyphCache`) for arbitrary glyph IDs with bounded memory.
//!
//! Coverage data is 1 byte per pixel (grayscale). No subpixel (LCD) rendering.

use alloc::{collections::BTreeMap, vec, vec::Vec};

use crate::{metrics, rasterize};
// ---------------------------------------------------------------------------
// axis_values_hash
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Fixed ASCII glyph cache
// ---------------------------------------------------------------------------

const GLYPH_MAX_W: usize = 50;
const GLYPH_MAX_H: usize = 50;
/// Number of printable ASCII glyphs cached (0x20..=0x7E).
const ASCII_CACHE_COUNT: usize = 95;
/// Per-glyph coverage buffer size. 1 byte per pixel (grayscale coverage).
const GLYPH_BUF_SIZE: usize = GLYPH_MAX_W * GLYPH_MAX_H;

/// Pre-rasterized metrics for one cached glyph.
#[derive(Clone, Copy)]
pub struct CachedGlyph {
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: u32,
    buf_offset: usize,
}

/// Fixed-size glyph cache for printable ASCII (0x20–0x7E).
/// Coverage maps are stored in a single contiguous buffer.
/// Total size: ~238 KiB (95 glyphs × 2,500 bytes coverage + metadata).
/// Each glyph buffer is GLYPH_MAX_W × GLYPH_MAX_H bytes (1 byte per pixel
/// grayscale coverage).
/// Maximum glyph ID that the fixed cache supports. Real fonts typically
/// have up to a few thousand glyphs; 2048 covers most common fonts.
const MAX_GLYPH_ID: usize = 2048;

/// Pre-rasterized glyph cache for printable ASCII (0x20-0x7E). Non-ASCII
/// glyphs fall through to `LruGlyphCache` in the render pipeline.
pub struct GlyphCache {
    glyphs: [CachedGlyph; ASCII_CACHE_COUNT],
    coverage: [u8; ASCII_CACHE_COUNT * GLYPH_BUF_SIZE],
    /// Maps a font glyph ID → cache index (0..94), or 0xFF if not cached.
    /// Indexed by glyph_id. Allows O(1) lookup from real font glyph IDs
    /// (produced by HarfBuzz shaping) to cached glyph data.
    glyph_id_map: [u8; MAX_GLYPH_ID],
    pub line_height: u32,
    /// Distance from top of line to baseline, in pixels. Derived from hhea ascent.
    pub ascent: u32,
    /// Distance from baseline to bottom of line, in pixels. Derived from hhea descent.
    /// Stored as a positive value (descent below baseline).
    pub descent: u32,
    /// The font size in pixels used to rasterize this cache (for kerning scaling).
    pub size_px: u32,
}

impl GlyphCache {
    /// Get cached glyph data for a font glyph ID.
    ///
    /// Accepts a real font glyph ID (from HarfBuzz shaping / cmap lookup).
    /// Returns `None` if the glyph is not in the ASCII cache.
    ///
    /// Returns 1-byte-per-pixel grayscale coverage, stored row-major.
    /// Total length = width * height.
    pub fn get(&self, glyph_id: u16) -> Option<(&CachedGlyph, &[u8])> {
        let id = glyph_id as usize;

        if id >= MAX_GLYPH_ID {
            return None;
        }

        let idx = self.glyph_id_map[id];

        if idx == 0xFF {
            return None;
        }

        let i = idx as usize;
        let g = &self.glyphs[i];
        let len = (g.width as usize) * (g.height as usize);

        if g.buf_offset + len > self.coverage.len() {
            return None; // defensive: glyph data doesn't fit in coverage buffer
        }

        let cov = &self.coverage[g.buf_offset..g.buf_offset + len];

        Some((g, cov))
    }
    /// Rasterize all printable ASCII glyphs into this cache in place.
    ///
    /// Uses the fonts library's rasterizer (read-fonts for outline extraction,
    /// scanline algorithm for coverage generation). The `font_data` is raw font
    /// file bytes.
    ///
    /// The rasterizer writes 1-byte-per-pixel grayscale coverage:
    /// width × height bytes per glyph.
    pub fn populate(&mut self, font_data: &[u8], size_px: u32) {
        self.populate_with_dpi(font_data, size_px, 96);
    }
    /// Rasterize all printable ASCII glyphs with automatic optical sizing.
    ///
    /// `dpi` is the display DPI (hardcoded for QEMU, configurable in
    /// principle). For fonts with an `opsz` axis, the optical size is
    /// automatically set to match the rendered pixel size (clamped to
    /// the font's opsz range). For fonts without an opsz axis, this
    /// behaves identically to `populate()`.
    pub fn populate_with_dpi(&mut self, font_data: &[u8], size_px: u32, dpi: u16) {
        self.populate_with_axes(font_data, size_px, dpi, &[], 1);
    }
    /// Rasterize all printable ASCII glyphs with explicit axis values.
    ///
    /// `extra_axes` provides explicit variation axis values (e.g., wght
    /// for weight, opsz for optical size). These are merged with any
    /// automatic axis values (opsz, wght correction). Explicit values
    /// take precedence over automatic ones when the same axis tag appears
    /// in both. Font selection is by font family (content type maps to a
    /// separate font file), not by axis switching.
    /// `scale_factor` is the display scale (1 for standard, 2 for Retina).
    /// Used to compute stem darkening dilation strength — the rasterizer
    /// needs both `size_px` (device resolution) and `scale_factor` (display
    /// density) because dilation is specified in device pixels but converted
    /// back to glyph units via `size_px * scale_factor`.
    pub fn populate_with_axes(
        &mut self,
        font_data: &[u8],
        size_px: u32,
        dpi: u16,
        extra_axes: &[metrics::AxisValue],
        scale_factor: u16,
    ) {
        let metrics = match metrics::font_metrics(font_data) {
            Some(m) => m,
            None => return,
        };
        let upem = metrics.units_per_em;
        let asc_fu = metrics.ascent as i32;
        let desc_fu = metrics.descent as i32;
        let gap_fu = metrics.line_gap as i32;
        let ascent_px = rasterize::scale_fu_ceil(asc_fu, size_px, upem);
        let descent_px = rasterize::scale_fu_ceil(-desc_fu, size_px, upem);
        let gap_px = rasterize::scale_fu(gap_fu, size_px, upem);
        let gap_px = if gap_px < 0 { 0 } else { gap_px as u32 };

        self.ascent = ascent_px as u32;
        self.descent = descent_px as u32;
        self.size_px = size_px;
        self.line_height = self.ascent + self.descent + gap_px;

        // Merge automatic axes (opsz) with caller-provided explicit axes.
        // Explicit axes take precedence over auto-computed ones.
        let auto_opsz = metrics::auto_axis_values_for_opsz(font_data, size_px as u16, dpi);
        let mut axes: alloc::vec::Vec<metrics::AxisValue> = alloc::vec::Vec::new();

        for av in &auto_opsz {
            if !extra_axes.iter().any(|e| e.tag == av.tag) {
                axes.push(*av);
            }
        }

        axes.extend_from_slice(extra_axes);

        // Heap-allocate rasterization scratch space (~39 KiB).
        let mut scratch: alloc::boxed::Box<rasterize::RasterScratch> = unsafe {
            let layout = alloc::alloc::Layout::new::<rasterize::RasterScratch>();
            let ptr = alloc::alloc::alloc_zeroed(layout) as *mut rasterize::RasterScratch;

            if ptr.is_null() {
                return;
            }

            // SAFETY: alloc_zeroed returns a valid, zero-initialized pointer
            // with the correct size and alignment. RasterScratch::zeroed() is
            // all-zeros, so the zero-initialized memory is a valid instance.
            alloc::boxed::Box::from_raw(ptr)
        };

        // Reset glyph ID → cache index mapping (0xFF = not cached).
        for slot in self.glyph_id_map.iter_mut() {
            *slot = 0xFF;
        }

        for i in 0..ASCII_CACHE_COUNT {
            let codepoint = (0x20u8 + i as u8) as char;
            let glyph_id = match metrics::glyph_id_for_char(font_data, codepoint) {
                Some(id) => id,
                None => continue,
            };
            let buf_offset = i * GLYPH_BUF_SIZE;
            let buf = &mut self.coverage[buf_offset..buf_offset + GLYPH_BUF_SIZE];
            let mut raster = rasterize::RasterBuffer {
                data: buf,
                width: GLYPH_MAX_W as u32,
                height: GLYPH_MAX_H as u32,
            };

            if let Some(m) = rasterize::rasterize_with_axes(
                font_data,
                glyph_id,
                size_px as u16,
                &mut raster,
                &mut scratch,
                &axes,
                scale_factor,
            ) {
                self.glyphs[i] = CachedGlyph {
                    width: m.width,
                    height: m.height,
                    bearing_x: m.bearing_x,
                    bearing_y: m.bearing_y,
                    advance: m.advance,
                    buf_offset,
                };

                // Record the real font glyph ID → cache index mapping.
                if (glyph_id as usize) < MAX_GLYPH_ID {
                    self.glyph_id_map[glyph_id as usize] = i as u8;
                }
            }
        }
    }
    /// Zero-initialize the cache. The struct is ~238 KiB -- callers with
    /// limited stack should allocate on the heap first, then call `populate`.
    pub const fn zeroed() -> Self {
        GlyphCache {
            glyphs: [CachedGlyph {
                width: 0,
                height: 0,
                bearing_x: 0,
                bearing_y: 0,
                advance: 0,
                buf_offset: 0,
            }; ASCII_CACHE_COUNT],
            coverage: [0u8; ASCII_CACHE_COUNT * GLYPH_BUF_SIZE],
            glyph_id_map: [0xFF; MAX_GLYPH_ID],
            line_height: 0,
            ascent: 0,
            descent: 0,
            size_px: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// LRU glyph cache
// ---------------------------------------------------------------------------

/// Cache key: (glyph_id, font_size, style_id).
/// The style_id is 0 for default style (no variation).
type CacheKey = (u16, u16, u32);

/// Sentinel value meaning "no linked-list neighbor."
const NONE: usize = usize::MAX;

/// Pre-rasterized glyph data stored in the LRU cache.
///
/// Contains the same metrics as `CachedGlyph` plus an owned coverage buffer.
/// The coverage buffer holds 1-byte-per-pixel grayscale data: `width * height`
/// bytes, matching the format produced by the scanline rasterizer.
#[derive(Clone, Debug)]
pub struct LruCachedGlyph {
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: u32,
    /// Grayscale coverage, row-major. Length = width * height.
    pub coverage: Vec<u8>,
}

/// Internal slot in the cache's entry array.
struct Slot {
    key: CacheKey,
    glyph: LruCachedGlyph,
    /// Index of the more-recently-used entry (toward head), or NONE.
    prev: usize,
    /// Index of the less-recently-used entry (toward tail), or NONE.
    next: usize,
}

/// An LRU cache for pre-rasterized glyphs, keyed by `(glyph_id, font_size)`.
///
/// Bounded: `len()` never exceeds `max_capacity`. When full, inserting a new
/// entry evicts the least-recently-used one. Accessing an entry via `get()`
/// promotes it to most-recently-used.
pub struct LruGlyphCache {
    /// Maximum number of entries before eviction.
    max_capacity: usize,
    /// All cache entries (may contain gaps after eviction — but we compact by
    /// reusing evicted slots via `free_list`).
    entries: Vec<Slot>,
    /// Key → index into `entries`.
    index: BTreeMap<CacheKey, usize>,
    /// Indices of freed slots available for reuse.
    free_list: Vec<usize>,
    /// Index of the most-recently-used entry, or NONE.
    head: usize,
    /// Index of the least-recently-used entry, or NONE.
    tail: usize,
}

impl LruGlyphCache {
    /// Create a new LRU glyph cache with the given maximum entry count.
    ///
    /// `max_capacity` must be at least 1.
    pub fn new(max_capacity: usize) -> Self {
        let cap = if max_capacity == 0 { 1 } else { max_capacity };

        LruGlyphCache {
            max_capacity: cap,
            entries: Vec::with_capacity(cap),
            index: BTreeMap::new(),
            free_list: Vec::new(),
            head: NONE,
            tail: NONE,
        }
    }

    /// Number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Look up a cached glyph by `(glyph_id, font_size)` at default axis values.
    ///
    /// Returns a reference to the cached glyph data if present, and promotes
    /// the entry to most-recently-used. Returns `None` on cache miss.
    pub fn get(&mut self, glyph_id: u16, font_size: u16) -> Option<&LruCachedGlyph> {
        self.get_with_axes(glyph_id, font_size, 0)
    }

    /// Look up a cached glyph by `(glyph_id, font_size, style_id)`.
    ///
    /// The `style_id` distinguishes glyphs rasterized at different variable
    /// font axis positions. Use 0 for default style.
    pub fn get_with_axes(
        &mut self,
        glyph_id: u16,
        font_size: u16,
        style_id: u32,
    ) -> Option<&LruCachedGlyph> {
        let key = (glyph_id, font_size, style_id);
        let &idx = self.index.get(&key)?;

        self.move_to_head(idx);

        Some(&self.entries[idx].glyph)
    }

    /// Insert a glyph into the cache at default axis values.
    ///
    /// If an entry with the same `(glyph_id, font_size)` already exists, it is
    /// updated with the new data and promoted to most-recently-used. If the
    /// cache is at capacity and the key is new, the least-recently-used entry
    /// is evicted first.
    pub fn insert(&mut self, glyph_id: u16, font_size: u16, glyph: LruCachedGlyph) {
        self.insert_with_axes(glyph_id, font_size, 0, glyph);
    }

    /// Insert a glyph into the cache with style identifier.
    ///
    /// The `style_id` distinguishes glyphs rasterized at different variable
    /// font axis positions. Use 0 for default style.
    pub fn insert_with_axes(
        &mut self,
        glyph_id: u16,
        font_size: u16,
        style_id: u32,
        glyph: LruCachedGlyph,
    ) {
        let key = (glyph_id, font_size, style_id);

        // Update existing entry.
        if let Some(&idx) = self.index.get(&key) {
            self.entries[idx].glyph = glyph;

            self.move_to_head(idx);

            return;
        }

        // Evict LRU if at capacity.
        if self.index.len() >= self.max_capacity {
            self.evict_tail();
        }

        // Allocate a slot (reuse freed slot or push new).
        let idx = if let Some(free_idx) = self.free_list.pop() {
            self.entries[free_idx] = Slot {
                key,
                glyph,
                prev: NONE,
                next: NONE,
            };

            free_idx
        } else {
            let idx = self.entries.len();

            self.entries.push(Slot {
                key,
                glyph,
                prev: NONE,
                next: NONE,
            });

            idx
        };

        self.index.insert(key, idx);
        self.push_head(idx);
    }

    // -----------------------------------------------------------------------
    // Internal linked-list operations
    // -----------------------------------------------------------------------

    /// Unlink an entry from its current position in the LRU list.
    fn unlink(&mut self, idx: usize) {
        let prev = self.entries[idx].prev;
        let next = self.entries[idx].next;

        if prev != NONE {
            self.entries[prev].next = next;
        } else {
            // This was the head.
            self.head = next;
        }

        if next != NONE {
            self.entries[next].prev = prev;
        } else {
            // This was the tail.
            self.tail = prev;
        }

        self.entries[idx].prev = NONE;
        self.entries[idx].next = NONE;
    }

    /// Push an entry to the head (most-recently-used) of the LRU list.
    /// The entry must not currently be in the list.
    fn push_head(&mut self, idx: usize) {
        self.entries[idx].prev = NONE;
        self.entries[idx].next = self.head;

        if self.head != NONE {
            self.entries[self.head].prev = idx;
        }

        self.head = idx;

        if self.tail == NONE {
            self.tail = idx;
        }
    }

    /// Move an existing entry to the head (most-recently-used).
    fn move_to_head(&mut self, idx: usize) {
        if self.head == idx {
            return; // Already at head.
        }

        self.unlink(idx);
        self.push_head(idx);
    }

    /// Evict the tail (least-recently-used) entry.
    fn evict_tail(&mut self) {
        if self.tail == NONE {
            return;
        }

        let idx = self.tail;
        let key = self.entries[idx].key;

        self.unlink(idx);
        self.index.remove(&key);

        // Clear the slot's coverage to free memory, then add to free list.
        self.entries[idx].glyph.coverage = vec![];

        self.free_list.push(idx);
    }
}
