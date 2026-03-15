//! Multi-surface compositing — CompositeSurface and back-to-front compositing.

use drawing::Surface;

/// A compositing surface: a pixel buffer with position, z-order, and visibility.
///
/// The compositor manages a set of these. On each frame, surfaces are composited
/// back-to-front (lowest z first) into the framebuffer using alpha blending.
///
/// Z-ordering convention (bottom to top):
///   0  = background
///   10 = content area
///   15 = shadows
///   20 = chrome (title bar)
pub struct CompositeSurface<'a> {
    pub surface: Surface<'a>,
    /// X position in framebuffer coordinates. Can be negative (partially offscreen).
    pub x: i32,
    /// Y position in framebuffer coordinates. Can be negative (partially offscreen).
    pub y: i32,
    /// Z-order: lower = further back. Composited in ascending z order.
    pub z: u16,
    /// Whether this surface participates in compositing.
    pub visible: bool,
}

fn min(a: u32, b: u32) -> u32 {
    if a < b {
        a
    } else {
        b
    }
}

/// Composite surfaces back-to-front onto a destination framebuffer.
///
/// Surfaces are sorted by z-order (ascending) and blitted with alpha blending.
/// Invisible surfaces are skipped. Surfaces may overlap and may extend outside
/// the destination bounds (clipped automatically by blit_blend).
///
/// The destination is NOT cleared — the caller should clear it beforehand if
/// needed, or include a full-screen background surface at z=0.
pub fn composite_surfaces(dst: &mut Surface, surfaces: &[&CompositeSurface]) {
    // Sort indices by z-order. We use a simple insertion sort since the number
    // of surfaces is small (typically 3-6).
    const MAX_SURFACES: usize = 16;

    let count = if surfaces.len() > MAX_SURFACES {
        MAX_SURFACES
    } else {
        surfaces.len()
    };
    let mut order: [usize; MAX_SURFACES] = [0; MAX_SURFACES];
    let mut i = 0;

    while i < count {
        order[i] = i;
        i += 1;
    }

    // Insertion sort by z-order.
    let mut j = 1;

    while j < count {
        let key = order[j];
        let key_z = surfaces[key].z;
        let mut k = j;

        while k > 0 && surfaces[order[k - 1]].z > key_z {
            order[k] = order[k - 1];
            k -= 1;
        }

        order[k] = key;
        j += 1;
    }

    // Composite back-to-front.
    let mut idx = 0;

    while idx < count {
        let s = surfaces[order[idx]];

        idx += 1;

        if !s.visible {
            continue;
        }

        // Handle negative offsets by computing source clip region.
        let src_x_start: u32 = if s.x < 0 { (-s.x) as u32 } else { 0 };
        let src_y_start: u32 = if s.y < 0 { (-s.y) as u32 } else { 0 };
        let dst_x: u32 = if s.x < 0 { 0 } else { s.x as u32 };
        let dst_y: u32 = if s.y < 0 { 0 } else { s.y as u32 };
        let src_w = s.surface.width;
        let src_h = s.surface.height;

        if src_x_start >= src_w || src_y_start >= src_h {
            continue; // Entirely off-screen to the left/top.
        }

        let visible_w = src_w - src_x_start;
        let visible_h = src_h - src_y_start;
        // Build a sub-region of the source data for blit_blend.
        // blit_blend takes src_data, src_width, src_height, src_stride.
        // We offset into the source buffer to skip the clipped rows/cols.
        let src_offset = (src_y_start * s.surface.stride
            + src_x_start * s.surface.format.bytes_per_pixel()) as usize;

        if src_offset < s.surface.data.len() {
            dst.blit_blend(
                &s.surface.data[src_offset..],
                visible_w,
                visible_h,
                s.surface.stride,
                dst_x,
                dst_y,
            );
        }
    }
}
/// Composite surfaces back-to-front onto a rectangular sub-region of the
/// destination framebuffer. Only pixels within `(rx, ry, rw, rh)` are
/// written. Surfaces are sorted by z-order as in `composite_surfaces`.
///
/// This is the damage-tracked variant: instead of re-compositing the entire
/// framebuffer, only the dirty region is updated.
pub fn composite_surfaces_rect(
    dst: &mut Surface,
    surfaces: &[&CompositeSurface],
    rx: u32,
    ry: u32,
    rw: u32,
    rh: u32,
) {
    if rw == 0 || rh == 0 {
        return;
    }

    // Sort indices by z-order (same insertion sort as composite_surfaces).
    const MAX_SURFACES: usize = 16;

    let count = if surfaces.len() > MAX_SURFACES {
        MAX_SURFACES
    } else {
        surfaces.len()
    };
    let mut order: [usize; MAX_SURFACES] = [0; MAX_SURFACES];
    let mut i = 0;

    while i < count {
        order[i] = i;
        i += 1;
    }

    let mut j = 1;

    while j < count {
        let key = order[j];
        let key_z = surfaces[key].z;
        let mut k = j;

        while k > 0 && surfaces[order[k - 1]].z > key_z {
            order[k] = order[k - 1];
            k -= 1;
        }

        order[k] = key;
        j += 1;
    }

    // Clamp the rect to destination bounds.
    let rx_end = min(rx + rw, dst.width);
    let ry_end = min(ry + rh, dst.height);

    if rx >= rx_end || ry >= ry_end {
        return;
    }

    // For each surface, composite only the intersection with the dirty rect.
    let mut idx = 0;
    while idx < count {
        let s = surfaces[order[idx]];

        idx += 1;

        if !s.visible {
            continue;
        }

        // Compute the region of the surface that overlaps the dirty rect in
        // framebuffer coordinates.
        let surf_fb_x0 = if s.x < 0 { 0i32 } else { s.x };
        let surf_fb_y0 = if s.y < 0 { 0i32 } else { s.y };
        let surf_fb_x1 = s.x + s.surface.width as i32;
        let surf_fb_y1 = s.y + s.surface.height as i32;
        // Intersect surface's FB region with the dirty rect.
        let ix0 = if surf_fb_x0 > rx as i32 {
            surf_fb_x0
        } else {
            rx as i32
        };
        let iy0 = if surf_fb_y0 > ry as i32 {
            surf_fb_y0
        } else {
            ry as i32
        };
        let ix1 = if surf_fb_x1 < rx_end as i32 {
            surf_fb_x1
        } else {
            rx_end as i32
        };
        let iy1 = if surf_fb_y1 < ry_end as i32 {
            surf_fb_y1
        } else {
            ry_end as i32
        };

        if ix0 >= ix1 || iy0 >= iy1 {
            continue; // No overlap.
        }

        // Compute source coordinates in the surface's local space.
        let src_x = (ix0 - s.x) as u32;
        let src_y = (iy0 - s.y) as u32;
        let blit_w = (ix1 - ix0) as u32;
        let blit_h = (iy1 - iy0) as u32;
        let src_offset =
            (src_y * s.surface.stride + src_x * s.surface.format.bytes_per_pixel()) as usize;

        if src_offset < s.surface.data.len() {
            dst.blit_blend(
                &s.surface.data[src_offset..],
                blit_w,
                blit_h,
                s.surface.stride,
                ix0 as u32,
                iy0 as u32,
            );
        }
    }
}
