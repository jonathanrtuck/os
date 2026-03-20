//! Solid and blended rectangle fills, including rounded rectangles.

use crate::{
    blend::{fill_rect_blend_scalar_1px, rounded_rect_write_aa_pixel},
    isqrt_fp, min, Color, Surface, SRGB_TO_LINEAR,
};

impl<'a> Surface<'a> {
    /// Fill a rectangle with a solid color. Clips to surface bounds.
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }

        let x2 = min(x.saturating_add(w), self.width);
        let y2 = min(y.saturating_add(h), self.height);
        let pixel_count = (x2 - x) as usize;

        if pixel_count == 0 {
            return;
        }

        assert!(self.is_valid(), "Surface invariant violated in fill_rect");

        let encoded = color.encode(self.format);
        let pixel_u32 = u32::from_ne_bytes(encoded);
        let bpp = self.format.bytes_per_pixel();
        let ptr = self.data.as_mut_ptr();

        for row in y..y2 {
            let row_offset = (row * self.stride + x * bpp) as usize;

            // SAFETY: bounds checked above -- x..x2 is within width, row is
            // within height, and stride * height <= data.len().
            unsafe {
                let row_ptr = ptr.add(row_offset) as *mut u32;

                #[cfg(target_arch = "aarch64")]
                {
                    // SAFETY: row_ptr points to pixel_count contiguous u32
                    // slots within the surface buffer. Bounds verified above.
                    crate::neon_fill_row(row_ptr, pixel_count, pixel_u32);
                }

                #[cfg(not(target_arch = "aarch64"))]
                {
                    for i in 0..pixel_count {
                        core::ptr::write(row_ptr.add(i), pixel_u32);
                    }
                }
            }
        }
    }

    /// Fill a rectangle with a vertical gradient from `color_top` to `color_bottom`.
    ///
    /// Each row linearly interpolates RGBA between the two colors. Row 0 gets
    /// `color_top`, row h-1 gets `color_bottom`. Clips to surface bounds.
    /// Integer math only. Useful for drop-shadow falloff effects.
    pub fn fill_gradient_v(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        color_top: Color,
        color_bottom: Color,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        if x >= self.width || y >= self.height {
            return;
        }

        let x2 = min(x.saturating_add(w), self.width);
        let y2 = min(y.saturating_add(h), self.height);

        // For h=1, just fill with color_top.
        if h == 1 {
            self.fill_rect(x, y, x2 - x, 1, color_top);

            return;
        }

        let denom = (h - 1) as u32;

        for row in y..y2 {
            let t = (row - y) as u32; // 0..h-1

            // Linearly interpolate each channel: c = top + (bottom - top) * t / denom.
            let r =
                (color_top.r as u32 * (denom - t) + color_bottom.r as u32 * t + denom / 2) / denom;
            let g =
                (color_top.g as u32 * (denom - t) + color_bottom.g as u32 * t + denom / 2) / denom;
            let b =
                (color_top.b as u32 * (denom - t) + color_bottom.b as u32 * t + denom / 2) / denom;
            let a =
                (color_top.a as u32 * (denom - t) + color_bottom.a as u32 * t + denom / 2) / denom;
            let row_color = Color {
                r: if r > 255 { 255 } else { r as u8 },
                g: if g > 255 { 255 } else { g as u8 },
                b: if b > 255 { 255 } else { b as u8 },
                a: if a > 255 { 255 } else { a as u8 },
            };

            self.fill_rect(x, row, x2 - x, 1, row_color);
        }
    }

    /// Fill a rectangle with alpha-blended color. Clips to surface bounds.
    ///
    /// Each destination pixel is blended with `color` using source-over.
    /// Opaque colors fast-path to `fill_rect`.
    pub fn fill_rect_blend(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        if color.a == 255 {
            self.fill_rect(x, y, w, h, color);

            return;
        }
        if color.a == 0 || w == 0 || h == 0 {
            return;
        }
        if x >= self.width || y >= self.height {
            return;
        }

        let x2 = min(x.saturating_add(w), self.width);
        let y2 = min(y.saturating_add(h), self.height);
        let pixel_count = (x2 - x) as usize;

        if pixel_count == 0 {
            return;
        }

        assert!(
            self.is_valid(),
            "Surface invariant violated in fill_rect_blend"
        );

        // Hoist src color linear conversion outside all loops.
        let sa = color.a as u32;
        let inv_sa = 255 - sa;
        let src_r_lin = SRGB_TO_LINEAR[color.r as usize] as u32;
        let src_g_lin = SRGB_TO_LINEAR[color.g as usize] as u32;
        let src_b_lin = SRGB_TO_LINEAR[color.b as usize] as u32;
        let bpp = self.format.bytes_per_pixel();
        let stride = self.stride;
        let ptr = self.data.as_mut_ptr();

        for row in y..y2 {
            let row_offset = (row * stride + x * bpp) as usize;

            // SAFETY: x/y clipped to surface bounds; x..x2 within width,
            // row within y..y2 < height. stride * height <= data.len().
            unsafe {
                let row_ptr = ptr.add(row_offset);

                #[cfg(target_arch = "aarch64")]
                {
                    let chunks = pixel_count / 4;
                    let tail_start = chunks * 4;

                    for chunk in 0..chunks {
                        let p = row_ptr.add(chunk * 16);
                        // SAFETY: p points to 16 writable bytes (4 dst pixels)
                        // within the clipped surface bounds.
                        crate::neon_blend_const_4px(
                            p,
                            src_r_lin as u16,
                            src_g_lin as u16,
                            src_b_lin as u16,
                            sa as u16,
                            inv_sa as u16,
                            &SRGB_TO_LINEAR,
                            &crate::LINEAR_TO_SRGB,
                        );
                    }

                    // Scalar tail.
                    for i in tail_start..pixel_count {
                        let p = row_ptr.add(i * 4);
                        fill_rect_blend_scalar_1px(p, src_r_lin, src_g_lin, src_b_lin, sa, inv_sa);
                    }
                }

                #[cfg(not(target_arch = "aarch64"))]
                {
                    for i in 0..pixel_count {
                        let p = row_ptr.add(i * 4);
                        fill_rect_blend_scalar_1px(p, src_r_lin, src_g_lin, src_b_lin, sa, inv_sa);
                    }
                }
            }
        }
    }

    /// Fill a rounded rectangle with a solid opaque color. Clips to surface bounds.
    ///
    /// Uses SDF-based approach: for each pixel in the corner arc regions,
    /// computes signed distance to the rounded corner and derives coverage
    /// (0.0-1.0) for anti-aliasing. Interior rows use `fill_rect` for speed.
    ///
    /// `radius` is clamped to `min(w, h) / 2`. Zero radius delegates to `fill_rect`.
    /// Anti-aliased edge pixels use gamma-correct sRGB blending.
    pub fn fill_rounded_rect(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        radius: u32,
        color: Color,
    ) {
        if w == 0 || h == 0 {
            return;
        }

        // Clamp radius to half the smallest dimension.
        let max_r = min(w, h) / 2;
        let r = min(radius, max_r);

        // Zero radius: delegate to fill_rect (no overhead).
        if r == 0 {
            self.fill_rect(x, y, w, h, color);
            return;
        }

        // Interior rows (between top and bottom arcs): fill_rect fast path.
        if h > 2 * r {
            self.fill_rect(x, y + r, w, h - 2 * r, color);
        }

        // Corner arc rows: top r rows and bottom r rows.
        // For each row in the arc region, compute the horizontal extent
        // of the rounded rect and fill with per-pixel AA at the edges.
        assert!(
            self.is_valid(),
            "Surface invariant violated in fill_rounded_rect"
        );

        let encoded = color.encode(self.format);
        let pixel_u32 = u32::from_ne_bytes(encoded);
        let bpp = self.format.bytes_per_pixel();
        let stride = self.stride;
        let surf_w = self.width;
        let surf_h = self.height;
        let ptr = self.data.as_mut_ptr();

        // Pre-convert color to linear for AA blending.
        let src_r_lin = SRGB_TO_LINEAR[color.r as usize] as u32;
        let src_g_lin = SRGB_TO_LINEAR[color.g as usize] as u32;
        let src_b_lin = SRGB_TO_LINEAR[color.b as usize] as u32;

        // Process top and bottom arc rows.
        for arc_row in 0..r {
            // Distance from arc row center to the circle center (at radius r from edge).
            // dy = r - arc_row - 0.5 (distance from pixel center to circle center y).
            // We use fixed-point: dy_fp = (r * 256) - (arc_row * 256) - 128
            let dy_fp: i64 = (r as i64 * 256) - (arc_row as i64 * 256) - 128;
            let dy_sq = (dy_fp * dy_fp) as u64;
            let r_sq = (r as u64 * 256) * (r as u64 * 256);

            // The arc at this row defines x extent: x_arc = sqrt(r^2 - dy^2)
            // This tells us how far the arc extends horizontally from the corner center.
            let x_arc_sq = if r_sq > dy_sq { r_sq - dy_sq } else { 0 };
            let x_arc_fp = isqrt_fp(x_arc_sq); // in 8.8 fixed point

            // Process both top row and bottom row.
            let rows: [u32; 2] = [y + arc_row, y + h - 1 - arc_row];
            for &py in &rows {
                if py >= surf_h {
                    continue;
                }

                // Left corner: center is at (x + r, py_center). Arc extends x_arc left.
                // The solid interior starts at x + r - floor(x_arc) and extends to x + w - r + floor(x_arc).
                let x_arc_int = (x_arc_fp >> 8) as u32;
                let x_arc_frac = (x_arc_fp & 0xFF) as u32; // 0..255

                // Left edge pixel: partial coverage.
                let left_solid = x + r - x_arc_int;
                let right_solid = x + w - r + x_arc_int;

                // Left AA pixel (if in bounds).
                if left_solid > 0 && x_arc_frac > 0 {
                    let lx = left_solid - 1;
                    if lx >= x && lx < surf_w {
                        // Coverage is x_arc_frac / 256.
                        let cov = x_arc_frac;
                        // SAFETY: lx < surf_w and py < surf_h (checked above).
                        // Pixel offset is within the surface data bounds.
                        unsafe {
                            rounded_rect_write_aa_pixel(
                                ptr,
                                lx,
                                py,
                                stride,
                                bpp,
                                src_r_lin,
                                src_g_lin,
                                src_b_lin,
                                color.a as u32,
                                cov,
                            );
                        }
                    }
                }

                // Right AA pixel (if in bounds).
                if right_solid < x + w && x_arc_frac > 0 {
                    let rx = right_solid;
                    if rx < surf_w {
                        let cov = x_arc_frac;
                        // SAFETY: rx < surf_w and py < surf_h (checked above).
                        unsafe {
                            rounded_rect_write_aa_pixel(
                                ptr,
                                rx,
                                py,
                                stride,
                                bpp,
                                src_r_lin,
                                src_g_lin,
                                src_b_lin,
                                color.a as u32,
                                cov,
                            );
                        }
                    }
                }

                // Solid interior pixels for this arc row.
                let fill_x0 = if left_solid < x { x } else { left_solid };
                let fill_x1 = if right_solid > x + w {
                    x + w
                } else {
                    right_solid
                };

                if fill_x0 < fill_x1 {
                    let clipped_x0 = if fill_x0 >= surf_w { continue } else { fill_x0 };
                    let clipped_x1 = min(fill_x1, surf_w);
                    let count = (clipped_x1 - clipped_x0) as usize;

                    if count > 0 {
                        let row_offset = (py * stride + clipped_x0 * bpp) as usize;
                        // SAFETY: py < surf_h, clipped_x0..clipped_x1 within [0, surf_w).
                        // row_offset + count * 4 <= surf_h * stride <= data.len().
                        unsafe {
                            let row_ptr = ptr.add(row_offset) as *mut u32;
                            #[cfg(target_arch = "aarch64")]
                            {
                                crate::neon_fill_row(row_ptr, count, pixel_u32);
                            }
                            #[cfg(not(target_arch = "aarch64"))]
                            {
                                for i in 0..count {
                                    core::ptr::write(row_ptr.add(i), pixel_u32);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Fill a rounded rectangle with alpha-blended color. Clips to surface bounds.
    ///
    /// Each destination pixel is blended using source-over. Corner pixels use
    /// per-pixel coverage derived from the SDF for anti-aliasing, combined with
    /// the source alpha. Interior rows use `fill_rect_blend` fast path.
    ///
    /// `radius` is clamped to `min(w, h) / 2`. Zero radius delegates to
    /// `fill_rect_blend`. Opaque colors fast-path to `fill_rounded_rect`.
    pub fn fill_rounded_rect_blend(
        &mut self,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        radius: u32,
        color: Color,
    ) {
        if color.a == 255 {
            self.fill_rounded_rect(x, y, w, h, radius, color);
            return;
        }
        if color.a == 0 || w == 0 || h == 0 {
            return;
        }

        // Clamp radius.
        let max_r = min(w, h) / 2;
        let r = min(radius, max_r);

        if r == 0 {
            self.fill_rect_blend(x, y, w, h, color);
            return;
        }

        // Interior rows: fill_rect_blend fast path.
        if h > 2 * r {
            self.fill_rect_blend(x, y + r, w, h - 2 * r, color);
        }

        assert!(
            self.is_valid(),
            "Surface invariant violated in fill_rounded_rect_blend"
        );

        // Pre-convert source color for blending.
        let sa = color.a as u32;
        let inv_sa = 255 - sa;
        let src_r_lin = SRGB_TO_LINEAR[color.r as usize] as u32;
        let src_g_lin = SRGB_TO_LINEAR[color.g as usize] as u32;
        let src_b_lin = SRGB_TO_LINEAR[color.b as usize] as u32;
        let bpp = self.format.bytes_per_pixel();
        let stride = self.stride;
        let surf_w = self.width;
        let surf_h = self.height;
        let ptr = self.data.as_mut_ptr();

        for arc_row in 0..r {
            let dy_fp: i64 = (r as i64 * 256) - (arc_row as i64 * 256) - 128;
            let dy_sq = (dy_fp * dy_fp) as u64;
            let r_sq = (r as u64 * 256) * (r as u64 * 256);
            let x_arc_sq = if r_sq > dy_sq { r_sq - dy_sq } else { 0 };
            let x_arc_fp = isqrt_fp(x_arc_sq);

            let rows: [u32; 2] = [y + arc_row, y + h - 1 - arc_row];
            for &py in &rows {
                if py >= surf_h {
                    continue;
                }

                let x_arc_int = (x_arc_fp >> 8) as u32;
                let x_arc_frac = (x_arc_fp & 0xFF) as u32;

                let left_solid = x + r - x_arc_int;
                let right_solid = x + w - r + x_arc_int;

                // Left AA pixel.
                if left_solid > 0 && x_arc_frac > 0 {
                    let lx = left_solid - 1;
                    if lx >= x && lx < surf_w {
                        // Effective alpha = color.a * coverage / 256.
                        let eff_a = (sa * x_arc_frac) >> 8;
                        if eff_a > 0 {
                            let eff_inv_a = 255 - eff_a;
                            // SAFETY: lx < surf_w, py < surf_h.
                            unsafe {
                                let p = ptr.add((py * stride + lx * bpp) as usize);
                                fill_rect_blend_scalar_1px(
                                    p, src_r_lin, src_g_lin, src_b_lin, eff_a, eff_inv_a,
                                );
                            }
                        }
                    }
                }

                // Right AA pixel.
                if right_solid < x + w && x_arc_frac > 0 {
                    let rx = right_solid;
                    if rx < surf_w {
                        let eff_a = (sa * x_arc_frac) >> 8;
                        if eff_a > 0 {
                            let eff_inv_a = 255 - eff_a;
                            // SAFETY: rx < surf_w, py < surf_h.
                            unsafe {
                                let p = ptr.add((py * stride + rx * bpp) as usize);
                                fill_rect_blend_scalar_1px(
                                    p, src_r_lin, src_g_lin, src_b_lin, eff_a, eff_inv_a,
                                );
                            }
                        }
                    }
                }

                // Solid interior pixels -- blend with full source alpha.
                let fill_x0 = if left_solid < x { x } else { left_solid };
                let fill_x1 = if right_solid > x + w {
                    x + w
                } else {
                    right_solid
                };

                if fill_x0 < fill_x1 {
                    let clipped_x0 = if fill_x0 >= surf_w { continue } else { fill_x0 };
                    let clipped_x1 = min(fill_x1, surf_w);
                    let count = (clipped_x1 - clipped_x0) as usize;

                    if count > 0 {
                        let row_offset = (py * stride + clipped_x0 * bpp) as usize;
                        // SAFETY: py < surf_h, clipped_x0..clipped_x1 within [0, surf_w).
                        unsafe {
                            let row_ptr = ptr.add(row_offset);
                            #[cfg(target_arch = "aarch64")]
                            {
                                let chunks = count / 4;
                                let tail_start = chunks * 4;
                                for chunk in 0..chunks {
                                    let p = row_ptr.add(chunk * 16);
                                    crate::neon_blend_const_4px(
                                        p,
                                        src_r_lin as u16,
                                        src_g_lin as u16,
                                        src_b_lin as u16,
                                        sa as u16,
                                        inv_sa as u16,
                                        &SRGB_TO_LINEAR,
                                        &crate::LINEAR_TO_SRGB,
                                    );
                                }
                                for i in tail_start..count {
                                    let p = row_ptr.add(i * 4);
                                    fill_rect_blend_scalar_1px(
                                        p, src_r_lin, src_g_lin, src_b_lin, sa, inv_sa,
                                    );
                                }
                            }
                            #[cfg(not(target_arch = "aarch64"))]
                            {
                                for i in 0..count {
                                    let p = row_ptr.add(i * 4);
                                    fill_rect_blend_scalar_1px(
                                        p, src_r_lin, src_g_lin, src_b_lin, sa, inv_sa,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
