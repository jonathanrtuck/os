//! Line drawing and rectangle outlines.
//!
//! Wu's anti-aliased line algorithm for smooth diagonal lines, plus
//! axis-aligned helpers for borders and outlines.

use crate::{abs, div255, Color, Surface};

impl<'a> Surface<'a> {
    /// Draw a horizontal line. Clips to surface bounds.
    pub fn draw_hline(&mut self, x: u32, y: u32, w: u32, color: Color) {
        self.fill_rect(x, y, w, 1, color);
    }

    /// Draw a vertical line. Clips to surface bounds.
    pub fn draw_vline(&mut self, x: u32, y: u32, h: u32, color: Color) {
        self.fill_rect(x, y, 1, h, color);
    }

    /// Draw a rectangle outline (1px border). Clips to surface bounds.
    ///
    /// The border is drawn inside the given bounds (the filled area is
    /// x..x+w, y..y+h including the border pixels).
    pub fn draw_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        if w == 0 || h == 0 {
            return;
        }

        // Top and bottom edges.
        self.draw_hline(x, y, w, color);

        if h > 1 {
            self.draw_hline(x, y + h - 1, w, color);
        }
        // Left and right edges (excluding corners already drawn).
        if h > 2 {
            self.draw_vline(x, y + 1, h - 2, color);

            if w > 1 {
                self.draw_vline(x + w - 1, y + 1, h - 2, color);
            }
        }
    }

    /// Draw an anti-aliased line using Wu's algorithm.
    ///
    /// Axis-aligned lines (horizontal or vertical) are drawn pixel-perfect
    /// with no anti-aliasing fringe. Diagonal lines use coverage-based
    /// sub-pixel blending for smooth edges. The algorithm produces two
    /// pixels per step along the major axis with complementary coverage
    /// values, resulting in a visually consistent 1px line width across
    /// all angles.
    ///
    /// Blending uses gamma-correct sRGB compositing via the existing LUT
    /// infrastructure. Clips to surface bounds; out-of-range coordinates
    /// are silently ignored.
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Color) {
        // Single point.
        if x0 == x1 && y0 == y1 {
            if x0 >= 0 && y0 >= 0 {
                self.set_pixel(x0 as u32, y0 as u32, color);
            }
            return;
        }

        // Axis-aligned lines: pixel-perfect, no AA fringe.
        if y0 == y1 {
            // Horizontal line.
            let (lx, rx) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
            for x in lx..=rx {
                if x >= 0 && y0 >= 0 {
                    self.set_pixel(x as u32, y0 as u32, color);
                }
            }
            return;
        }
        if x0 == x1 {
            // Vertical line.
            let (ty, by) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
            for y in ty..=by {
                if x0 >= 0 && y >= 0 {
                    self.set_pixel(x0 as u32, y as u32, color);
                }
            }
            return;
        }

        // Wu's anti-aliased line algorithm.
        // We work in 8.8 fixed-point for the gradient's fractional part.
        let mut ax0 = x0;
        let mut ay0 = y0;
        let mut ax1 = x1;
        let mut ay1 = y1;

        let steep = abs(ay1 - ay0) > abs(ax1 - ax0);

        // If steep, swap x and y so we always iterate along the longer axis.
        if steep {
            core::mem::swap(&mut ax0, &mut ay0);
            core::mem::swap(&mut ax1, &mut ay1);
        }

        // Ensure we draw from left to right.
        if ax0 > ax1 {
            core::mem::swap(&mut ax0, &mut ax1);
            core::mem::swap(&mut ay0, &mut ay1);
        }

        let dx = ax1 - ax0;
        let dy = ay1 - ay0;

        // Gradient in 8.8 fixed-point: dy/dx scaled by 256.
        // dx is guaranteed > 0 here (we ensured ax0 < ax1 and handled ax0==ax1).
        let gradient_fp = if dx == 0 {
            256i32 // Should not happen, but safe fallback.
        } else {
            (dy * 256) / dx
        };

        // Check for perfect 45-degree lines: gradient is exactly +/-256 (+/-1.0).
        // These pass through pixel centers, so no AA is needed.
        if gradient_fp == 256 || gradient_fp == -256 {
            // 45-degree line: draw solid pixels along the diagonal.
            let sy: i32 = if dy > 0 { 1 } else { -1 };
            let mut cy = ay0;
            for cx in ax0..=ax1 {
                if steep {
                    if cy >= 0 && cx >= 0 {
                        self.set_pixel(cy as u32, cx as u32, color);
                    }
                } else if cx >= 0 && cy >= 0 {
                    self.set_pixel(cx as u32, cy as u32, color);
                }
                cy += sy;
            }
            return;
        }

        // First endpoint.
        self.wu_endpoint(ax0, ay0, steep, color);

        // Last endpoint.
        self.wu_endpoint(ax1, ay1, steep, color);

        // y-intercept in 8.8 fixed-point, starting after the first endpoint.
        // The y value at x=ax0 is ay0 (pixel center). At x=ax0+1, y = ay0 + gradient.
        let mut y_fp = ay0 * 256 + 128 + gradient_fp;

        // Main loop: iterate along the major axis between the two endpoints.
        for x in (ax0 + 1)..ax1 {
            let y_int = if y_fp < 0 {
                // Arithmetic right shift for negative values.
                (y_fp - 255) / 256
            } else {
                y_fp / 256
            };
            let frac = ((y_fp - y_int * 256) & 0xFF) as u32; // Fractional part 0..255.

            // Two pixels at this x: one at y_int, one at y_int+1.
            // Coverage: pixel at y_int gets (255 - frac), pixel at y_int+1 gets frac.
            let cov_lo = (255 - frac) as u8;
            let cov_hi = frac as u8;

            if steep {
                self.wu_plot(y_int as i32, x, color, cov_lo);
                self.wu_plot((y_int + 1) as i32, x, color, cov_hi);
            } else {
                self.wu_plot(x, y_int as i32, color, cov_lo);
                self.wu_plot(x, (y_int + 1) as i32, color, cov_hi);
            }

            y_fp += gradient_fp;
        }
    }

    /// Plot a single pixel with coverage-weighted alpha blending (Wu's AA helper).
    ///
    /// `coverage` is 0..255 where 255 = fully covered. Skips if coverage is 0
    /// or coordinates are out of bounds.
    fn wu_plot(&mut self, x: i32, y: i32, color: Color, coverage: u8) {
        if coverage == 0 || x < 0 || y < 0 {
            return;
        }
        let ux = x as u32;
        let uy = y as u32;
        if ux >= self.width || uy >= self.height {
            return;
        }

        if coverage == 255 {
            // Fully covered: use source-over blend (handles color.a < 255).
            self.blend_pixel(ux, uy, color);
            return;
        }

        // Effective alpha = color.a * coverage / 255.
        let eff_a = div255(color.a as u32 * coverage as u32);
        if eff_a == 0 {
            return;
        }

        let aa_color = Color::rgba(color.r, color.g, color.b, eff_a as u8);
        self.blend_pixel(ux, uy, aa_color);
    }

    /// Draw an endpoint pixel for Wu's algorithm.
    fn wu_endpoint(&mut self, x: i32, y: i32, steep: bool, color: Color) {
        if steep {
            if y >= 0 && x >= 0 {
                self.blend_pixel(y as u32, x as u32, color);
            }
        } else if x >= 0 && y >= 0 {
            self.blend_pixel(x as u32, y as u32, color);
        }
    }
}
