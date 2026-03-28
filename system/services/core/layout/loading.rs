//! Loading scene: minimal scene graph shown during event-driven boot.
//!
//! Displays a spinning arc indicator centered on screen while core
//! completes async initialization (font metrics, document loading,
//! PNG decoding). No fonts or document content required — pure geometry.
//!
//! The spinner is the Tabler `loader-2` icon: a 270° circular arc,
//! stroke-expanded and CPU-rasterized to BGRA pixels each frame.
//! Displayed as `Content::InlineImage` to bypass the metal-render
//! stencil pipeline (which struggles with concave stroke geometry).

use alloc::{vec, vec::Vec};

use scene::{fnv1a, path_cubic_to, path_move_to, Color, Content, NodeFlags};

// ── Spinner constants ──────────────────────────────────────────────

/// Spinner node size in points.
const SPINNER_SIZE_PT: u32 = 32;

/// Pixel size for rasterization (2× for Retina).
const SPINNER_SIZE_PX: u32 = SPINNER_SIZE_PT * 2;

/// Stroke width in Tabler's 24×24 viewbox units (default = 2.0).
const SPINNER_STROKE_W: f32 = 2.0;

/// Spinner color: white on black background.
const SPINNER_COLOR: Color = Color::rgba(255, 255, 255, 255);

/// Loading screen background: pure black.
const LOADING_BG: Color = Color::rgba(0, 0, 0, 255);

/// Kappa constant for quarter-circle cubic Bézier approximation.
/// k = 4/3 × (√2 − 1) ≈ 0.5522847498.
const KAPPA: f32 = 0.5522847498;

/// Viewbox center (Tabler 24×24).
const VB_CENTER: f32 = 12.0;

/// Arc radius in viewbox units (24×24, center at 12).
const VB_RADIUS: f32 = 9.0;

// ── Trig helpers (no_std) ──────────────────────────────────────────

fn sin_cos(x: f32) -> (f32, f32) {
    let pi = core::f32::consts::PI;
    let two_pi = 2.0 * pi;
    let half_pi = core::f32::consts::FRAC_PI_2;
    let mut a = x;
    if a > pi || a < -pi {
        a -= (a / two_pi) as i32 as f32 * two_pi;
        if a > pi {
            a -= two_pi;
        }
        if a < -pi {
            a += two_pi;
        }
    }
    let (sin_sign, cos_sign, reduced) = if a > half_pi {
        (1.0_f32, -1.0_f32, pi - a)
    } else if a < -half_pi {
        (1.0_f32, -1.0_f32, -pi - a)
    } else {
        (1.0_f32, 1.0_f32, a)
    };
    let a2 = reduced * reduced;
    let sin_val = reduced
        * (1.0
            - a2 / 6.0
                * (1.0
                    - a2 / 20.0 * (1.0 - a2 / 42.0 * (1.0 - a2 / 72.0 * (1.0 - a2 / 110.0)))));
    let cos_val = 1.0
        - a2 / 2.0
            * (1.0 - a2 / 12.0 * (1.0 - a2 / 30.0 * (1.0 - a2 / 56.0 * (1.0 - a2 / 90.0))));
    (sin_sign * sin_val, cos_sign * cos_val)
}

/// Rotate point (px, py) around (cx, cy) by (sin, cos).
fn rotate_point(px: f32, py: f32, cx: f32, cy: f32, sin: f32, cos: f32) -> (f32, f32) {
    let dx = px - cx;
    let dy = py - cy;
    (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
}

// ── Spinner rasterization ──────────────────────────────────────────

/// CPU-rasterize the Tabler loader-2 spinner at a given rotation angle.
///
/// Builds a 270° arc (3 quarter-circle cubic Bézier segments) in the
/// 24×24 Tabler viewbox, rotated by `angle` around center. Stroke-expands
/// and rasterizes to BGRA pixels at `SPINNER_SIZE_PX × SPINNER_SIZE_PX`.
fn rasterize_spinner_frame(angle: f32) -> Vec<u8> {
    let path_cmds = build_viewbox_arc(angle);
    let expanded = scene::stroke::expand_stroke(&path_cmds, SPINNER_STROKE_W);

    let w = SPINNER_SIZE_PX;
    let h = SPINNER_SIZE_PX;
    let scale = w as f32 / 24.0;
    let stride = w * 4;
    let mut pixels = vec![0u8; (stride * h) as usize];

    if !expanded.is_empty() {
        let mut surface = drawing::Surface {
            data: &mut pixels,
            width: w,
            height: h,
            stride,
            format: drawing::PixelFormat::Bgra8888,
        };

        render::scene_render::path_raster::render_path_data(
            &mut surface,
            &expanded,
            scale,
            SPINNER_COLOR,
            scene::FillRule::Winding,
            0,
            0,
            w as i32,
            h as i32,
        );
    }

    pixels
}

/// Build the Tabler loader-2 arc (270° CCW) in the 24×24 viewbox,
/// rotated by `angle` around center (12, 12).
///
/// The arc traces: top → left → bottom → right (counterclockwise),
/// matching the Tabler `loader-2` icon path `M12 3a9 9 0 1 0 9 9`.
/// Three quarter-circle cubic Bézier segments with KAPPA control points.
fn build_viewbox_arc(angle: f32) -> Vec<u8> {
    let r = VB_RADIUS;
    let k = KAPPA * r;
    let cx = VB_CENTER;
    let cy = VB_CENTER;
    let (sin, cos) = sin_cos(angle);

    let mut path = Vec::with_capacity(128);

    // Cardinal points (unrotated):
    //   Top:    (cx, cy - r) = (12, 3)
    //   Left:   (cx - r, cy) = (3, 12)
    //   Bottom: (cx, cy + r) = (12, 21)
    //   Right:  (cx + r, cy) = (21, 12)

    // Start at top.
    let (sx, sy) = rotate_point(cx, cy - r, cx, cy, sin, cos);
    path_move_to(&mut path, sx, sy);

    // Segment 1: Top → Left (90° CCW).
    let (c1x, c1y) = rotate_point(cx - k, cy - r, cx, cy, sin, cos);
    let (c2x, c2y) = rotate_point(cx - r, cy - k, cx, cy, sin, cos);
    let (ex, ey) = rotate_point(cx - r, cy, cx, cy, sin, cos);
    path_cubic_to(&mut path, c1x, c1y, c2x, c2y, ex, ey);

    // Segment 2: Left → Bottom (90° CCW).
    let (c1x, c1y) = rotate_point(cx - r, cy + k, cx, cy, sin, cos);
    let (c2x, c2y) = rotate_point(cx - k, cy + r, cx, cy, sin, cos);
    let (ex, ey) = rotate_point(cx, cy + r, cx, cy, sin, cos);
    path_cubic_to(&mut path, c1x, c1y, c2x, c2y, ex, ey);

    // Segment 3: Bottom → Right (90° CCW).
    let (c1x, c1y) = rotate_point(cx + k, cy + r, cx, cy, sin, cos);
    let (c2x, c2y) = rotate_point(cx + r, cy + k, cx, cy, sin, cos);
    let (ex, ey) = rotate_point(cx + r, cy, cx, cy, sin, cos);
    path_cubic_to(&mut path, c1x, c1y, c2x, c2y, ex, ey);

    path
}

// ── Loading scene builder ──────────────────────────────────────────

/// Node index for the spinner (second node after root).
pub const N_LOADING_SPINNER: u16 = 1;

/// Build a minimal 2-node loading scene: root background + spinning arc.
///
/// The arc is CPU-rasterized at angle 0 and displayed as an InlineImage.
/// Subsequent frames update the image data via `update_spinner_angle`.
pub fn build_loading_scene(w: &mut scene::SceneWriter<'_>, fb_width: u32, fb_height: u32) {
    w.clear();

    // ── Root node (full-screen black background) ────────────────────

    let root = w.alloc_node().unwrap();
    debug_assert!(root == 0);

    {
        let n = w.node_mut(root);
        n.width = scene::upt(fb_width);
        n.height = scene::upt(fb_height);
        n.background = LOADING_BG;
        n.flags = NodeFlags::VISIBLE;
    }
    w.set_root(root);

    // ── Spinner node (centered, rasterized at angle 0) ──────────────

    let spinner = w.alloc_node().unwrap();
    debug_assert!(spinner == N_LOADING_SPINNER);

    let pixels = rasterize_spinner_frame(0.0);
    let data_ref = w.push_data(&pixels);
    let hash = fnv1a(&pixels);

    {
        let n = w.node_mut(spinner);
        n.x = scene::pt((fb_width / 2) as i32 - (SPINNER_SIZE_PT as i32 / 2));
        n.y = scene::pt((fb_height / 2) as i32 - (SPINNER_SIZE_PT as i32 / 2));
        n.width = scene::upt(SPINNER_SIZE_PT);
        n.height = scene::upt(SPINNER_SIZE_PT);
        n.content = Content::InlineImage {
            data: data_ref,
            src_width: SPINNER_SIZE_PX as u16,
            src_height: SPINNER_SIZE_PX as u16,
        };
        n.content_hash = hash;
        n.flags = NodeFlags::VISIBLE;
    }

    w.add_child(root, spinner);
}

/// Rebuild the spinner image at a new rotation angle.
///
/// Re-rasterizes the 270° arc at the given angle, replaces the pixel
/// data in the data buffer, and marks the node dirty.
pub fn update_spinner_angle(w: &mut scene::SceneWriter<'_>, angle: f32) {
    w.reset_data();

    let pixels = rasterize_spinner_frame(angle);
    let data_ref = w.push_data(&pixels);
    let hash = fnv1a(&pixels);

    let n = w.node_mut(N_LOADING_SPINNER);
    n.content = Content::InlineImage {
        data: data_ref,
        src_width: SPINNER_SIZE_PX as u16,
        src_height: SPINNER_SIZE_PX as u16,
    };
    n.content_hash = hash;
    w.mark_dirty(N_LOADING_SPINNER);
}
