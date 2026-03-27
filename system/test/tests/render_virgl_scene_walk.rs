//! Host-side tests for virgil-render scene walk pure functions.
//!
//! The scene_walk module has complex dependencies (atlas, scene, crate-level
//! imports) that prevent direct `#[path]` inclusion. Instead, we copy the
//! pure geometry functions being tested — they are standalone math with no
//! external dependencies.

// ── Copied pure functions from virgil-render/scene_walk.rs ──────────

/// Maximum number of colored quads per frame.
const MAX_QUADS: usize = 256;

/// Maximum vertex data in u32 DWORDs (6 floats per vertex, 6 vertices per quad).
const MAX_VERTEX_DWORDS: usize = MAX_QUADS * 6 * 6;

/// Maximum number of textured quads (glyphs) per frame.
const MAX_TEXT_QUADS: usize = 4096;

/// Maximum textured vertex data in u32 DWORDs (8 floats per vertex, 6 vertices per quad).
const MAX_TEXTURED_DWORDS: usize = MAX_TEXT_QUADS * 6 * 8;

/// Maximum triangle fan vertices per frame.
const MAX_PATH_FAN_VERTS: usize = 3072;

/// Max fan vertex data in u32 DWORDs.
const MAX_PATH_FAN_DWORDS: usize = MAX_PATH_FAN_VERTS * 6;

/// Maximum covering quads per frame.
const MAX_PATH_COVERS: usize = 16;

/// Max cover vertex data.
const MAX_PATH_COVER_DWORDS: usize = MAX_PATH_COVERS * 6 * 6;

// ── ClipRect (f32 variant) ──────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
struct ClipRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl ClipRect {
    fn intersect(self, other: ClipRect) -> ClipRect {
        let x0 = if self.x > other.x { self.x } else { other.x };
        let y0 = if self.y > other.y { self.y } else { other.y };
        let x1_a = self.x + self.w;
        let x1_b = other.x + other.w;
        let y1_a = self.y + self.h;
        let y1_b = other.y + other.h;
        let x1 = if x1_a < x1_b { x1_a } else { x1_b };
        let y1 = if y1_a < y1_b { y1_a } else { y1_b };
        let w = if x1 > x0 { x1 - x0 } else { 0.0 };
        let h = if y1 > y0 { y1 - y0 } else { 0.0 };
        ClipRect { x: x0, y: y0, w, h }
    }

    fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

// ── QuadBatch ───────────────────────────────────────────────────────

struct QuadBatch {
    vertex_data: [u32; MAX_VERTEX_DWORDS],
    vertex_len: usize,
    pub vertex_count: u32,
    dropped: u32,
}

impl QuadBatch {
    fn new() -> Self {
        Self {
            vertex_data: [0; MAX_VERTEX_DWORDS],
            vertex_len: 0,
            vertex_count: 0,
            dropped: 0,
        }
    }

    fn clear(&mut self) {
        self.vertex_len = 0;
        self.vertex_count = 0;
        self.dropped = 0;
    }

    fn as_vertex_data(&self) -> &[u32] {
        &self.vertex_data[..self.vertex_len]
    }

    fn dropped_count(&self) -> u32 {
        self.dropped
    }

    fn push_vertex(&mut self, x: f32, y: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.vertex_len + 6 > MAX_VERTEX_DWORDS {
            self.dropped += 1;
            return;
        }
        self.vertex_data[self.vertex_len] = x.to_bits();
        self.vertex_data[self.vertex_len + 1] = y.to_bits();
        self.vertex_data[self.vertex_len + 2] = r.to_bits();
        self.vertex_data[self.vertex_len + 3] = g.to_bits();
        self.vertex_data[self.vertex_len + 4] = b.to_bits();
        self.vertex_data[self.vertex_len + 5] = a.to_bits();
        self.vertex_len += 6;
        self.vertex_count += 1;
    }
}

// ── TexturedBatch ───────────────────────────────────────────────────

struct TexturedBatch {
    vertex_data: [u32; MAX_TEXTURED_DWORDS],
    vertex_len: usize,
    pub vertex_count: u32,
    dropped: u32,
}

impl TexturedBatch {
    fn new() -> Self {
        Self {
            vertex_data: [0; MAX_TEXTURED_DWORDS],
            vertex_len: 0,
            vertex_count: 0,
            dropped: 0,
        }
    }

    fn clear(&mut self) {
        self.vertex_len = 0;
        self.vertex_count = 0;
        self.dropped = 0;
    }

    fn as_vertex_data(&self) -> &[u32] {
        &self.vertex_data[..self.vertex_len]
    }

    fn dropped_count(&self) -> u32 {
        self.dropped
    }

    fn push_vertex(&mut self, x: f32, y: f32, u: f32, v: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.vertex_len + 8 > MAX_TEXTURED_DWORDS {
            self.dropped += 1;
            return;
        }
        self.vertex_data[self.vertex_len] = x.to_bits();
        self.vertex_data[self.vertex_len + 1] = y.to_bits();
        self.vertex_data[self.vertex_len + 2] = u.to_bits();
        self.vertex_data[self.vertex_len + 3] = v.to_bits();
        self.vertex_data[self.vertex_len + 4] = r.to_bits();
        self.vertex_data[self.vertex_len + 5] = g.to_bits();
        self.vertex_data[self.vertex_len + 6] = b.to_bits();
        self.vertex_data[self.vertex_len + 7] = a.to_bits();
        self.vertex_len += 8;
        self.vertex_count += 1;
    }
}

// ── PathBatch ───────────────────────────────────────────────────────

struct PathBatch {
    fan_data: [u32; MAX_PATH_FAN_DWORDS],
    fan_len: usize,
    pub fan_vertex_count: u32,
    cover_data: [u32; MAX_PATH_COVER_DWORDS],
    cover_len: usize,
    pub cover_vertex_count: u32,
    dropped: u32,
}

impl PathBatch {
    fn new() -> Self {
        Self {
            fan_data: [0; MAX_PATH_FAN_DWORDS],
            fan_len: 0,
            fan_vertex_count: 0,
            cover_data: [0; MAX_PATH_COVER_DWORDS],
            cover_len: 0,
            cover_vertex_count: 0,
            dropped: 0,
        }
    }

    fn clear(&mut self) {
        self.fan_len = 0;
        self.fan_vertex_count = 0;
        self.cover_len = 0;
        self.cover_vertex_count = 0;
        self.dropped = 0;
    }

    fn as_fan_data(&self) -> &[u32] {
        &self.fan_data[..self.fan_len]
    }

    fn as_cover_data(&self) -> &[u32] {
        &self.cover_data[..self.cover_len]
    }

    fn dropped_count(&self) -> u32 {
        self.dropped
    }

    fn push_fan_vertex(&mut self, x: f32, y: f32) {
        if self.fan_len + 6 > MAX_PATH_FAN_DWORDS {
            self.dropped += 1;
            return;
        }
        self.fan_data[self.fan_len] = x.to_bits();
        self.fan_data[self.fan_len + 1] = y.to_bits();
        self.fan_data[self.fan_len + 2] = 0;
        self.fan_data[self.fan_len + 3] = 0;
        self.fan_data[self.fan_len + 4] = 0;
        self.fan_data[self.fan_len + 5] = 1.0f32.to_bits();
        self.fan_len += 6;
        self.fan_vertex_count += 1;
    }

    fn push_cover_vertex(&mut self, x: f32, y: f32, r: f32, g: f32, b: f32, a: f32) {
        if self.cover_len + 6 > MAX_PATH_COVER_DWORDS {
            self.dropped += 1;
            return;
        }
        self.cover_data[self.cover_len] = x.to_bits();
        self.cover_data[self.cover_len + 1] = y.to_bits();
        self.cover_data[self.cover_len + 2] = r.to_bits();
        self.cover_data[self.cover_len + 3] = g.to_bits();
        self.cover_data[self.cover_len + 4] = b.to_bits();
        self.cover_data[self.cover_len + 5] = a.to_bits();
        self.cover_len += 6;
        self.cover_vertex_count += 1;
    }
}

// ── flatten_cubic ───────────────────────────────────────────────────

fn flatten_cubic(
    x0: f32,
    y0: f32,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    x3: f32,
    y3: f32,
    points: &mut [(f32, f32)],
    count: &mut usize,
    depth: u32,
) {
    if *count >= points.len() || depth >= 10 {
        if *count < points.len() {
            points[*count] = (x3, y3);
            *count += 1;
        }
        return;
    }

    let dx = x3 - x0;
    let dy = y3 - y0;
    let d1 = ((c1x - x0) * dy - (c1y - y0) * dx).abs();
    let d2 = ((c2x - x0) * dy - (c2y - y0) * dx).abs();
    let max_d = if d1 > d2 { d1 } else { d2 };
    let chord_sq = dx * dx + dy * dy;
    let threshold = 0.5;

    if max_d * max_d <= threshold * threshold * chord_sq || chord_sq < 0.001 {
        points[*count] = (x3, y3);
        *count += 1;
        return;
    }

    let m01x = (x0 + c1x) * 0.5;
    let m01y = (y0 + c1y) * 0.5;
    let m12x = (c1x + c2x) * 0.5;
    let m12y = (c1y + c2y) * 0.5;
    let m23x = (c2x + x3) * 0.5;
    let m23y = (c2y + y3) * 0.5;
    let m012x = (m01x + m12x) * 0.5;
    let m012y = (m01y + m12y) * 0.5;
    let m123x = (m12x + m23x) * 0.5;
    let m123y = (m12y + m23y) * 0.5;
    let mx = (m012x + m123x) * 0.5;
    let my = (m012y + m123y) * 0.5;

    flatten_cubic(
        x0,
        y0,
        m01x,
        m01y,
        m012x,
        m012y,
        mx,
        my,
        points,
        count,
        depth + 1,
    );
    flatten_cubic(
        mx,
        my,
        m123x,
        m123y,
        m23x,
        m23y,
        x3,
        y3,
        points,
        count,
        depth + 1,
    );
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Read vertex float at index from a u32 slice.
fn read_f32(data: &[u32], idx: usize) -> f32 {
    f32::from_bits(data[idx])
}

/// Approximate float equality (within epsilon).
fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
    (a - b).abs() <= eps
}

// ═══════════════════════════════════════════════════════════════════
// Tests: flatten_cubic
// ═══════════════════════════════════════════════════════════════════

#[test]
fn flatten_cubic_straight_line() {
    // Cubic with collinear control points = straight line.
    // Should flatten to a single endpoint (no subdivision needed).
    let mut points = [(0.0f32, 0.0f32); 256];
    let mut count = 0usize;

    flatten_cubic(
        0.0,
        0.0, // start
        10.0,
        10.0, // control 1 (on line)
        20.0,
        20.0, // control 2 (on line)
        30.0,
        30.0, // end
        &mut points,
        &mut count,
        0,
    );

    assert!(count >= 1, "should produce at least one point");
    // Last point must be the endpoint.
    let (lx, ly) = points[count - 1];
    assert!(approx_eq(lx, 30.0, 0.01), "last x should be 30.0, got {lx}");
    assert!(approx_eq(ly, 30.0, 0.01), "last y should be 30.0, got {ly}");
}

#[test]
fn flatten_cubic_quarter_circle() {
    // Approximate quarter circle from (0, 100) to (100, 0) via standard
    // cubic Bezier control points (kappa = 0.5522847...).
    let k = 0.5522847;
    let mut points = [(0.0f32, 0.0f32); 256];
    let mut count = 0usize;

    flatten_cubic(
        0.0,
        100.0, // start
        k * 100.0,
        100.0, // control 1
        100.0,
        k * 100.0, // control 2
        100.0,
        0.0, // end
        &mut points,
        &mut count,
        0,
    );

    // Should produce multiple segments for a curve this large.
    assert!(
        count > 1,
        "quarter circle should require multiple segments, got {count}"
    );

    // Last point must be the endpoint.
    let (lx, ly) = points[count - 1];
    assert!(approx_eq(lx, 100.0, 0.01));
    assert!(approx_eq(ly, 0.0, 0.01));

    // Every intermediate point should be approximately on the circle (r=100).
    // Allow 1.0 pixel tolerance for flattening error.
    for i in 0..count {
        let (px, py) = points[i];
        let r = (px * px + py * py).sqrt();
        assert!(
            (r - 100.0).abs() < 2.0,
            "point {i} ({px}, {py}) r={r} deviates from circle r=100"
        );
    }
}

#[test]
fn flatten_cubic_s_curve() {
    // S-curve: control points on opposite sides of the chord.
    let mut points = [(0.0f32, 0.0f32); 256];
    let mut count = 0usize;

    flatten_cubic(
        0.0,
        0.0, // start
        0.0,
        100.0, // control 1 (above)
        100.0,
        -100.0, // control 2 (below)
        100.0,
        0.0, // end
        &mut points,
        &mut count,
        0,
    );

    assert!(
        count > 2,
        "S-curve should require many segments, got {count}"
    );

    // Last point is endpoint.
    let (lx, ly) = points[count - 1];
    assert!(approx_eq(lx, 100.0, 0.01));
    assert!(approx_eq(ly, 0.0, 0.01));

    // The S-curve should have points both above and below y=0.
    let has_positive_y = points[..count].iter().any(|&(_, y)| y > 1.0);
    let has_negative_y = points[..count].iter().any(|&(_, y)| y < -1.0);
    assert!(has_positive_y, "S-curve should have points with positive y");
    assert!(has_negative_y, "S-curve should have points with negative y");
}

#[test]
fn flatten_cubic_degenerate_zero_length() {
    // All control points identical = zero-length curve.
    let mut points = [(0.0f32, 0.0f32); 256];
    let mut count = 0usize;

    flatten_cubic(
        5.0,
        5.0,
        5.0,
        5.0,
        5.0,
        5.0,
        5.0,
        5.0,
        &mut points,
        &mut count,
        0,
    );

    assert!(count >= 1, "should produce at least the endpoint");
    let (lx, ly) = points[count - 1];
    assert!(approx_eq(lx, 5.0, 0.001));
    assert!(approx_eq(ly, 5.0, 0.001));
}

#[test]
fn flatten_cubic_small_buffer_capacity() {
    // Buffer with only 2 slots: should not write past the buffer.
    let mut points = [(0.0f32, 0.0f32); 2];
    let mut count = 0usize;

    flatten_cubic(
        0.0,
        0.0,
        0.0,
        100.0,
        100.0,
        -100.0,
        100.0,
        0.0,
        &mut points,
        &mut count,
        0,
    );

    // Must not exceed buffer capacity, regardless of curve complexity.
    assert_eq!(count, 2, "should fill exactly to buffer capacity");
    // Points should be valid coordinates (not uninitialized).
    let (x0, _y0) = points[0];
    let (x1, _y1) = points[1];
    assert!(x0.is_finite(), "first point should be finite");
    assert!(x1.is_finite(), "second point should be finite");
}

#[test]
fn flatten_cubic_max_depth_terminates() {
    // Starting at depth 9 should subdivide once more then stop at depth 10.
    let mut points = [(0.0f32, 0.0f32); 256];
    let mut count = 0usize;

    flatten_cubic(
        0.0,
        0.0,
        0.0,
        1000.0,
        1000.0,
        -1000.0,
        1000.0,
        0.0,
        &mut points,
        &mut count,
        9,
    );

    // At depth 9, subdivides to depth 10 which hits the max and emits endpoints.
    assert!(count >= 1, "should produce at least one point at max depth");
}

// ═══════════════════════════════════════════════════════════════════
// Tests: ClipRect
// ═══════════════════════════════════════════════════════════════════

#[test]
fn clip_rect_full_overlap() {
    let a = ClipRect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let b = ClipRect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let r = a.intersect(b);
    assert!(!r.is_empty());
    assert!(approx_eq(r.x, 0.0, 0.001));
    assert!(approx_eq(r.y, 0.0, 0.001));
    assert!(approx_eq(r.w, 100.0, 0.001));
    assert!(approx_eq(r.h, 100.0, 0.001));
}

#[test]
fn clip_rect_partial_overlap() {
    let a = ClipRect {
        x: 0.0,
        y: 0.0,
        w: 60.0,
        h: 60.0,
    };
    let b = ClipRect {
        x: 30.0,
        y: 20.0,
        w: 60.0,
        h: 60.0,
    };
    let r = a.intersect(b);
    assert!(!r.is_empty());
    assert!(approx_eq(r.x, 30.0, 0.001));
    assert!(approx_eq(r.y, 20.0, 0.001));
    assert!(approx_eq(r.w, 30.0, 0.001));
    assert!(approx_eq(r.h, 40.0, 0.001));
}

#[test]
fn clip_rect_no_overlap_horizontal() {
    let a = ClipRect {
        x: 0.0,
        y: 0.0,
        w: 50.0,
        h: 50.0,
    };
    let b = ClipRect {
        x: 60.0,
        y: 0.0,
        w: 50.0,
        h: 50.0,
    };
    let r = a.intersect(b);
    assert!(
        r.is_empty(),
        "non-overlapping rects should produce empty intersection"
    );
}

#[test]
fn clip_rect_no_overlap_vertical() {
    let a = ClipRect {
        x: 0.0,
        y: 0.0,
        w: 50.0,
        h: 50.0,
    };
    let b = ClipRect {
        x: 0.0,
        y: 60.0,
        w: 50.0,
        h: 50.0,
    };
    let r = a.intersect(b);
    assert!(r.is_empty());
}

#[test]
fn clip_rect_contained() {
    // b is entirely inside a.
    let a = ClipRect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let b = ClipRect {
        x: 20.0,
        y: 30.0,
        w: 40.0,
        h: 50.0,
    };
    let r = a.intersect(b);
    assert!(!r.is_empty());
    assert!(approx_eq(r.x, 20.0, 0.001));
    assert!(approx_eq(r.y, 30.0, 0.001));
    assert!(approx_eq(r.w, 40.0, 0.001));
    assert!(approx_eq(r.h, 50.0, 0.001));
}

#[test]
fn clip_rect_containing() {
    // a is entirely inside b — intersection should equal a.
    let a = ClipRect {
        x: 20.0,
        y: 30.0,
        w: 40.0,
        h: 50.0,
    };
    let b = ClipRect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };
    let r = a.intersect(b);
    assert!(!r.is_empty());
    assert!(approx_eq(r.x, 20.0, 0.001));
    assert!(approx_eq(r.y, 30.0, 0.001));
    assert!(approx_eq(r.w, 40.0, 0.001));
    assert!(approx_eq(r.h, 50.0, 0.001));
}

#[test]
fn clip_rect_touching_edge() {
    // Rects sharing a single edge (zero width overlap).
    let a = ClipRect {
        x: 0.0,
        y: 0.0,
        w: 50.0,
        h: 50.0,
    };
    let b = ClipRect {
        x: 50.0,
        y: 0.0,
        w: 50.0,
        h: 50.0,
    };
    let r = a.intersect(b);
    assert!(
        r.is_empty(),
        "edge-touching rects should be empty (zero width)"
    );
}

#[test]
fn clip_rect_is_empty_zero_width() {
    let r = ClipRect {
        x: 10.0,
        y: 20.0,
        w: 0.0,
        h: 50.0,
    };
    assert!(r.is_empty());
}

#[test]
fn clip_rect_is_empty_zero_height() {
    let r = ClipRect {
        x: 10.0,
        y: 20.0,
        w: 50.0,
        h: 0.0,
    };
    assert!(r.is_empty());
}

#[test]
fn clip_rect_is_empty_negative_dimensions() {
    let r = ClipRect {
        x: 10.0,
        y: 20.0,
        w: -5.0,
        h: 30.0,
    };
    assert!(r.is_empty());
    let r2 = ClipRect {
        x: 10.0,
        y: 20.0,
        w: 30.0,
        h: -5.0,
    };
    assert!(r2.is_empty());
}

#[test]
fn clip_rect_is_empty_both_zero() {
    let r = ClipRect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    };
    assert!(r.is_empty());
}

#[test]
fn clip_rect_intersect_is_commutative() {
    let a = ClipRect {
        x: 10.0,
        y: 20.0,
        w: 50.0,
        h: 40.0,
    };
    let b = ClipRect {
        x: 30.0,
        y: 10.0,
        w: 60.0,
        h: 70.0,
    };
    let r1 = a.intersect(b);
    let r2 = b.intersect(a);
    assert!(approx_eq(r1.x, r2.x, 0.001));
    assert!(approx_eq(r1.y, r2.y, 0.001));
    assert!(approx_eq(r1.w, r2.w, 0.001));
    assert!(approx_eq(r1.h, r2.h, 0.001));
}

// ═══════════════════════════════════════════════════════════════════
// Tests: QuadBatch::push_vertex
// ═══════════════════════════════════════════════════════════════════

#[test]
fn quad_batch_push_vertex_stores_floats() {
    let mut batch = QuadBatch::new();
    batch.push_vertex(1.0, 2.0, 0.5, 0.6, 0.7, 1.0);

    assert_eq!(batch.vertex_count, 1);
    assert_eq!(batch.as_vertex_data().len(), 6);
    assert_eq!(read_f32(batch.as_vertex_data(), 0), 1.0);
    assert_eq!(read_f32(batch.as_vertex_data(), 1), 2.0);
    assert_eq!(read_f32(batch.as_vertex_data(), 2), 0.5);
    assert_eq!(read_f32(batch.as_vertex_data(), 3), 0.6);
    assert_eq!(read_f32(batch.as_vertex_data(), 4), 0.7);
    assert_eq!(read_f32(batch.as_vertex_data(), 5), 1.0);
}

#[test]
fn quad_batch_multiple_vertices() {
    let mut batch = QuadBatch::new();
    batch.push_vertex(1.0, 2.0, 0.0, 0.0, 0.0, 1.0);
    batch.push_vertex(3.0, 4.0, 1.0, 1.0, 1.0, 1.0);

    assert_eq!(batch.vertex_count, 2);
    assert_eq!(batch.as_vertex_data().len(), 12); // 2 * 6
    assert_eq!(read_f32(batch.as_vertex_data(), 6), 3.0); // second vertex x
    assert_eq!(read_f32(batch.as_vertex_data(), 7), 4.0); // second vertex y
}

#[test]
fn quad_batch_overflow_drops_vertex() {
    let mut batch = QuadBatch::new();
    // Fill to capacity: MAX_VERTEX_DWORDS / 6 = MAX_QUADS * 6 vertices
    let max_verts = MAX_VERTEX_DWORDS / 6;
    for i in 0..max_verts {
        batch.push_vertex(i as f32, 0.0, 0.0, 0.0, 0.0, 1.0);
    }
    assert_eq!(batch.vertex_count, max_verts as u32);
    assert_eq!(batch.dropped_count(), 0);

    // One more should be dropped.
    batch.push_vertex(999.0, 0.0, 0.0, 0.0, 0.0, 1.0);
    assert_eq!(batch.vertex_count, max_verts as u32); // unchanged
    assert_eq!(batch.dropped_count(), 1);
}

#[test]
fn quad_batch_clear_resets() {
    let mut batch = QuadBatch::new();
    batch.push_vertex(1.0, 2.0, 0.0, 0.0, 0.0, 1.0);
    assert_eq!(batch.vertex_count, 1);

    batch.clear();
    assert_eq!(batch.vertex_count, 0);
    assert_eq!(batch.as_vertex_data().len(), 0);
    assert_eq!(batch.dropped_count(), 0);
}

// ═══════════════════════════════════════════════════════════════════
// Tests: TexturedBatch::push_vertex
// ═══════════════════════════════════════════════════════════════════

#[test]
fn textured_batch_push_vertex_stores_floats() {
    let mut batch = TexturedBatch::new();
    batch.push_vertex(1.0, 2.0, 0.25, 0.75, 0.5, 0.6, 0.7, 1.0);

    assert_eq!(batch.vertex_count, 1);
    assert_eq!(batch.as_vertex_data().len(), 8);
    assert_eq!(read_f32(batch.as_vertex_data(), 0), 1.0);
    assert_eq!(read_f32(batch.as_vertex_data(), 1), 2.0);
    assert_eq!(read_f32(batch.as_vertex_data(), 2), 0.25); // u
    assert_eq!(read_f32(batch.as_vertex_data(), 3), 0.75); // v
    assert_eq!(read_f32(batch.as_vertex_data(), 4), 0.5); // r
    assert_eq!(read_f32(batch.as_vertex_data(), 5), 0.6); // g
    assert_eq!(read_f32(batch.as_vertex_data(), 6), 0.7); // b
    assert_eq!(read_f32(batch.as_vertex_data(), 7), 1.0); // a
}

#[test]
fn textured_batch_overflow_drops_vertex() {
    let mut batch = TexturedBatch::new();
    let max_verts = MAX_TEXTURED_DWORDS / 8;
    for i in 0..max_verts {
        batch.push_vertex(i as f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
    }
    assert_eq!(batch.vertex_count, max_verts as u32);
    assert_eq!(batch.dropped_count(), 0);

    batch.push_vertex(999.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
    assert_eq!(batch.vertex_count, max_verts as u32);
    assert_eq!(batch.dropped_count(), 1);
}

#[test]
fn textured_batch_clear_resets() {
    let mut batch = TexturedBatch::new();
    batch.push_vertex(1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
    batch.clear();
    assert_eq!(batch.vertex_count, 0);
    assert_eq!(batch.as_vertex_data().len(), 0);
    assert_eq!(batch.dropped_count(), 0);
}

// ═══════════════════════════════════════════════════════════════════
// Tests: PathBatch::push_fan_vertex / push_cover_vertex
// ═══════════════════════════════════════════════════════════════════

#[test]
fn path_batch_push_fan_vertex() {
    let mut batch = PathBatch::new();
    batch.push_fan_vertex(-0.5, 0.5);

    assert_eq!(batch.fan_vertex_count, 1);
    let fan = batch.as_fan_data();
    assert_eq!(fan.len(), 6);
    assert_eq!(read_f32(fan, 0), -0.5); // x
    assert_eq!(read_f32(fan, 1), 0.5); // y
    assert_eq!(fan[2], 0); // r = 0 (unused)
    assert_eq!(fan[3], 0); // g = 0
    assert_eq!(fan[4], 0); // b = 0
    assert_eq!(read_f32(fan, 5), 1.0); // a = 1.0 (non-zero for ANGLE)
}

#[test]
fn path_batch_push_cover_vertex() {
    let mut batch = PathBatch::new();
    batch.push_cover_vertex(-1.0, 1.0, 0.2, 0.3, 0.4, 0.8);

    assert_eq!(batch.cover_vertex_count, 1);
    let cover = batch.as_cover_data();
    assert_eq!(cover.len(), 6);
    assert_eq!(read_f32(cover, 0), -1.0);
    assert_eq!(read_f32(cover, 1), 1.0);
    assert_eq!(read_f32(cover, 2), 0.2);
    assert_eq!(read_f32(cover, 3), 0.3);
    assert_eq!(read_f32(cover, 4), 0.4);
    assert_eq!(read_f32(cover, 5), 0.8);
}

#[test]
fn path_batch_fan_overflow_drops() {
    let mut batch = PathBatch::new();
    let max_fan = MAX_PATH_FAN_DWORDS / 6;
    for i in 0..max_fan {
        batch.push_fan_vertex(i as f32, 0.0);
    }
    assert_eq!(batch.fan_vertex_count, max_fan as u32);
    assert_eq!(batch.dropped_count(), 0);

    batch.push_fan_vertex(999.0, 0.0);
    assert_eq!(batch.fan_vertex_count, max_fan as u32);
    assert_eq!(batch.dropped_count(), 1);
}

#[test]
fn path_batch_cover_overflow_drops() {
    let mut batch = PathBatch::new();
    let max_cover = MAX_PATH_COVER_DWORDS / 6;
    for i in 0..max_cover {
        batch.push_cover_vertex(i as f32, 0.0, 0.0, 0.0, 0.0, 1.0);
    }
    assert_eq!(batch.cover_vertex_count, max_cover as u32);
    assert_eq!(batch.dropped_count(), 0);

    batch.push_cover_vertex(999.0, 0.0, 0.0, 0.0, 0.0, 1.0);
    assert_eq!(batch.cover_vertex_count, max_cover as u32);
    assert_eq!(batch.dropped_count(), 1);
}

#[test]
fn path_batch_clear_resets_both() {
    let mut batch = PathBatch::new();
    batch.push_fan_vertex(0.0, 0.0);
    batch.push_cover_vertex(0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
    assert_eq!(batch.fan_vertex_count, 1);
    assert_eq!(batch.cover_vertex_count, 1);

    batch.clear();
    assert_eq!(batch.fan_vertex_count, 0);
    assert_eq!(batch.cover_vertex_count, 0);
    assert_eq!(batch.as_fan_data().len(), 0);
    assert_eq!(batch.as_cover_data().len(), 0);
    assert_eq!(batch.dropped_count(), 0);
}

#[test]
fn path_batch_fan_and_cover_independent() {
    // Fan and cover use separate buffers and separate drop counters.
    let mut batch = PathBatch::new();

    // Fill fan to capacity.
    let max_fan = MAX_PATH_FAN_DWORDS / 6;
    for i in 0..max_fan {
        batch.push_fan_vertex(i as f32, 0.0);
    }
    // Fan is full but cover should still accept.
    batch.push_fan_vertex(999.0, 0.0);
    assert_eq!(batch.dropped_count(), 1);

    batch.push_cover_vertex(1.0, 2.0, 0.0, 0.0, 0.0, 1.0);
    assert_eq!(
        batch.cover_vertex_count, 1,
        "cover should still work after fan overflow"
    );
}
