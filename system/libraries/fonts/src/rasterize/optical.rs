//! Automatic optical sizing and dark mode weight correction.
//!
//! Provides automatic axis value computation for variable fonts:
//! - Optical size (`opsz` axis) based on rendered pixel size and display DPI.
//! - Weight correction (`wght` axis) for dark mode rendering, compensating
//!   for the irradiation illusion (light text on dark backgrounds appears
//!   heavier than equivalent dark-on-light text).

use super::metrics::{font_axes, AxisValue};

// ---------------------------------------------------------------------------
// Automatic optical sizing
// ---------------------------------------------------------------------------

/// Compute the optical size value from a rendered pixel size and display DPI.
///
/// Uses the traditional typographic formula: `opsz = font_size_px * 72 / dpi`,
/// converting the rendered pixel size to an equivalent point size. This maps
/// display pixels to the font's optical size axis, so small text gets the
/// small-optical-size cut (wider, sturdier letterforms) and large text gets
/// the display cut.
///
/// The result is NOT clamped to any font's opsz axis range -- the caller
/// should clamp to the font's declared min/max.
pub fn compute_optical_size(font_size_px: u16, dpi: u16) -> f32 {
    if dpi == 0 {
        return font_size_px as f32;
    }
    font_size_px as f32 * 72.0 / dpi as f32
}

/// Compute automatic optical size axis values for a font.
///
/// If the font has an `opsz` variation axis, returns an `AxisValue` array
/// with the optical size set to the computed value (clamped to the font's
/// declared opsz range). If the font has no `opsz` axis (e.g., Source Code
/// Pro variable), returns an empty Vec -- a no-op for the rendering pipeline.
///
/// This function is the main entry point for automatic optical sizing.
/// Callers pass the result directly to `rasterize_with_axes` or
/// `shape_with_variations` -- no explicit opsz parameter needed.
pub fn auto_axis_values_for_opsz(
    font_data: &[u8],
    font_size_px: u16,
    dpi: u16,
) -> alloc::vec::Vec<AxisValue> {
    let axes = font_axes(font_data);
    let opsz_axis = match axes.iter().find(|a| &a.tag == b"opsz") {
        Some(a) => a,
        None => return alloc::vec::Vec::new(), // No opsz axis -- no-op.
    };

    let raw_opsz = compute_optical_size(font_size_px, dpi);

    // Clamp to the font's declared opsz range.
    let clamped = if raw_opsz < opsz_axis.min_value {
        opsz_axis.min_value
    } else if raw_opsz > opsz_axis.max_value {
        opsz_axis.max_value
    } else {
        raw_opsz
    };

    alloc::vec![AxisValue {
        tag: *b"opsz",
        value: clamped,
    }]
}

// ---------------------------------------------------------------------------
// Dark mode weight correction
// ---------------------------------------------------------------------------

/// sRGB-to-linear lookup table (256 entries, 0.0-1.0 range as u16 fixed-point).
///
/// Pre-computed at build time for accuracy and speed. Each entry maps an sRGB
/// byte value (0-255) to its linear-light equivalent scaled to 0-65535 (u16).
/// This avoids needing a pow() function at runtime in no_std.
///
/// The sRGB transfer function is:
/// - For values <= 0.04045: linear = sRGB / 12.92
/// - For values > 0.04045: linear = ((sRGB + 0.055) / 1.055)^2.4
///
/// We approximate the 2.4 exponent using integer arithmetic at build time.
const SRGB_TO_LINEAR_LUT: [u16; 256] = {
    let mut lut = [0u16; 256];
    let mut i = 0u32;
    while i < 256 {
        let s = i as f64 / 255.0;
        let linear = if s <= 0.04045 {
            s / 12.92
        } else {
            // (s + 0.055) / 1.055 raised to 2.4.
            // In const context we can use f64 operations.
            let base = (s + 0.055) / 1.055;
            // x^2.4 = exp(2.4 * ln(x)) -- use the manual approach.
            // Since this is const-eval at compile time, we use a Taylor series.
            // More practically: x^2.4 = x^2 * x^0.4
            // x^0.4 = x^(2/5) = (x^2)^(1/5) = fifth_root(x^2)
            // Fifth root via iterative Newton refinement at compile time.
            let base_sq = base * base;
            // Newton: solve t^5 = base_sq -> t = ((4*t + base_sq / t^4) / 5)
            let mut t = base; // initial guess for fifth_root(base_sq)
            let mut iter = 0;
            while iter < 50 {
                let t2 = t * t;
                let t4 = t2 * t2;
                if t4 < 1e-15 {
                    break;
                }
                let t_new = (4.0 * t + base_sq / t4) / 5.0;
                let diff = t_new - t;
                if diff < 1e-12 && diff > -1e-12 {
                    break;
                }
                t = t_new;
                iter += 1;
            }
            base_sq * t // base^2 * base^0.4 = base^2.4
        };
        // Scale to u16 range (0-65535).
        let scaled = (linear * 65535.0 + 0.5) as u32;
        lut[i as usize] = if scaled > 65535 { 65535 } else { scaled as u16 };
        i += 1;
    }
    lut
};

/// Convert an sRGB component (0-255) to linear light (0.0-1.0).
///
/// Uses a pre-computed lookup table for accuracy and no_std compatibility.
fn srgb_to_linear(value: u8) -> f32 {
    SRGB_TO_LINEAR_LUT[value as usize] as f32 / 65535.0
}

/// Compute relative luminance of an sRGB color per WCAG 2.0.
///
/// Returns a value between 0.0 (black) and 1.0 (white).
/// Formula: L = 0.2126 * R_lin + 0.7152 * G_lin + 0.0722 * B_lin
fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    let rl = srgb_to_linear(r);
    let gl = srgb_to_linear(g);
    let bl = srgb_to_linear(b);
    0.2126 * rl + 0.7152 * gl + 0.0722 * bl
}

/// Compute the weight correction factor for dark mode text rendering.
///
/// When light text is rendered on a dark background, human vision causes
/// bright areas to perceptually spread into dark areas (irradiation). This
/// makes light-on-dark text appear heavier than the same weight rendered
/// dark-on-light. To compensate, we reduce the font weight proportionally
/// to the foreground/background luminance contrast.
///
/// # Behavior
///
/// - **Foreground lighter than background**: returns a factor < 1.0
///   (weight reduction). Higher contrast -> smaller factor (more reduction).
/// - **Foreground darker than background**: returns exactly 1.0
///   (no reduction needed).
/// - **Same foreground and background**: returns exactly 1.0.
///
/// The correction is **continuous and proportional** -- NOT a binary
/// light/dark switch. The factor is computed from the WCAG contrast ratio:
///
/// ```text
/// contrast = (L_lighter + 0.05) / (L_darker + 0.05)
/// reduction = (contrast - 1.0) / 20.0  // maps contrast 1-21 to 0.0-1.0
/// factor = 1.0 - clamp(reduction, 0.0, 0.15)  // max 15% weight reduction
/// ```
///
/// # Arguments
///
/// * `fg_r`, `fg_g`, `fg_b` -- foreground color in sRGB (0-255)
/// * `bg_r`, `bg_g`, `bg_b` -- background color in sRGB (0-255)
///
/// # Returns
///
/// A correction factor in the range [0.85, 1.0]. Multiply the font's
/// base weight by this factor to get the adjusted weight.
pub fn weight_correction_factor(fg_r: u8, fg_g: u8, fg_b: u8, bg_r: u8, bg_g: u8, bg_b: u8) -> f32 {
    let fg_lum = relative_luminance(fg_r, fg_g, fg_b);
    let bg_lum = relative_luminance(bg_r, bg_g, bg_b);

    // Only reduce weight when foreground is lighter than background.
    if fg_lum <= bg_lum {
        return 1.0;
    }

    // WCAG contrast ratio: (lighter + 0.05) / (darker + 0.05).
    // Range is [1.0, 21.0] where 21:1 is maximum (white on black).
    let contrast = (fg_lum + 0.05) / (bg_lum + 0.05);

    // Map contrast range [1, 21] to a weight reduction factor.
    // We use a continuous curve: factor = 1.0 - max_reduction * (contrast - 1) / 20.
    // Max reduction is 15% (factor = 0.85) at maximum contrast (21:1).
    // This ensures monotonic decrease across the full contrast range.
    let normalized = (contrast - 1.0) / 20.0; // 0.0 at contrast=1, 1.0 at contrast=21
    let clamped = if normalized < 0.0 {
        0.0
    } else if normalized > 1.0 {
        1.0
    } else {
        normalized
    };

    1.0 - 0.15 * clamped
}

/// Compute automatic weight correction axis values for dark mode rendering.
///
/// For a variable font with a `wght` axis, computes the corrected weight
/// value based on foreground/background luminance contrast. Returns an
/// `AxisValue` array with the adjusted weight, clamped to the font's
/// declared wght axis range.
///
/// For fonts **without** a `wght` axis (non-variable fonts, or variable
/// fonts that lack a weight axis), returns an empty Vec -- a no-op for
/// the rendering pipeline. No error is produced.
///
/// # Arguments
///
/// * `font_data` -- raw font file bytes
/// * `fg_r`, `fg_g`, `fg_b` -- foreground color in sRGB (0-255)
/// * `bg_r`, `bg_g`, `bg_b` -- background color in sRGB (0-255)
///
/// # Returns
///
/// A `Vec<AxisValue>` containing the corrected `wght` axis value, or
/// empty if the font has no `wght` axis or no correction is needed.
pub fn auto_weight_correction_axes(
    font_data: &[u8],
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
) -> alloc::vec::Vec<AxisValue> {
    let axes = font_axes(font_data);
    let wght_axis = match axes.iter().find(|a| &a.tag == b"wght") {
        Some(a) => a,
        None => return alloc::vec::Vec::new(), // No wght axis -- no-op.
    };

    let factor = weight_correction_factor(fg_r, fg_g, fg_b, bg_r, bg_g, bg_b);

    // If factor is 1.0, no correction needed -- return empty to avoid
    // unnecessary axis variation overhead.
    if (factor - 1.0).abs() < f32::EPSILON {
        return alloc::vec::Vec::new();
    }

    // Apply correction to the font's default weight.
    let adjusted = wght_axis.default_value * factor;

    // Clamp to the font's declared wght axis range.
    let clamped = if adjusted < wght_axis.min_value {
        wght_axis.min_value
    } else if adjusted > wght_axis.max_value {
        wght_axis.max_value
    } else {
        adjusted
    };

    alloc::vec![AxisValue {
        tag: *b"wght",
        value: clamped,
    }]
}
