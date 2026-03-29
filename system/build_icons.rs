//! Build-time icon converter: SVG → native path commands → generated Rust.
//!
//! Included by build.rs. Reads Tabler SVG files from resources/icons/,
//! parses <path d="..."> attributes, converts to native binary path commands,
//! and writes libraries/icons/data.rs with const arrays and lookup tables.

use std::path::Path;

// ── Path command encoding (mirrors scene/primitives.rs) ────────────

const PATH_MOVE_TO: u32 = 0;
const PATH_LINE_TO: u32 = 1;
const PATH_CUBIC_TO: u32 = 2;
const PATH_CLOSE: u32 = 3;

fn emit_move_to(out: &mut Vec<u8>, x: f32, y: f32) {
    out.extend_from_slice(&PATH_MOVE_TO.to_le_bytes());
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
}

fn emit_line_to(out: &mut Vec<u8>, x: f32, y: f32) {
    out.extend_from_slice(&PATH_LINE_TO.to_le_bytes());
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
}

fn emit_cubic_to(out: &mut Vec<u8>, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
    out.extend_from_slice(&PATH_CUBIC_TO.to_le_bytes());
    out.extend_from_slice(&c1x.to_le_bytes());
    out.extend_from_slice(&c1y.to_le_bytes());
    out.extend_from_slice(&c2x.to_le_bytes());
    out.extend_from_slice(&c2y.to_le_bytes());
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
}

fn emit_close(out: &mut Vec<u8>) {
    out.extend_from_slice(&PATH_CLOSE.to_le_bytes());
}

// ── SVG path parser (host-side, uses std math) ─────────────────────

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && matches!(s[i], b' ' | b',' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

fn parse_number(s: &[u8], start: usize) -> Option<(f32, usize)> {
    let mut i = skip_ws(s, start);
    if i >= s.len() {
        return None;
    }
    let begin = i;
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        i += 1;
    }
    if i >= s.len() || (!s[i].is_ascii_digit() && s[i] != b'.') {
        return None;
    }
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    if i < s.len() && s[i] == b'.' {
        i += 1;
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
        }
    }
    // Exponent (rare in SVG but valid).
    if i < s.len() && (s[i] == b'e' || s[i] == b'E') {
        i += 1;
        if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
            i += 1;
        }
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
        }
    }
    let text = std::str::from_utf8(&s[begin..i]).ok()?;
    let val: f32 = text.parse().ok()?;
    Some((val, i))
}

fn parse_flag(s: &[u8], start: usize) -> Option<(bool, usize)> {
    let i = skip_ws(s, start);
    if i >= s.len() {
        return None;
    }
    match s[i] {
        b'0' => Some((false, i + 1)),
        b'1' => Some((true, i + 1)),
        _ => None,
    }
}

// ── Arc-to-cubic (uses std::f32 trig) ──────────────────────────────

fn arc_to_cubics(
    out: &mut Vec<u8>,
    x1: f32,
    y1: f32,
    mut rx: f32,
    mut ry: f32,
    x_rot_deg: f32,
    large_arc: bool,
    sweep: bool,
    x2: f32,
    y2: f32,
) {
    use std::f32::consts::{FRAC_PI_2, PI};

    if rx.abs() < 1e-6 || ry.abs() < 1e-6 {
        emit_line_to(out, x2, y2);
        return;
    }
    rx = rx.abs();
    ry = ry.abs();

    let dx = x2 - x1;
    let dy = y2 - y1;
    if dx * dx + dy * dy < 1e-10 {
        return;
    }

    let phi = x_rot_deg.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();

    let dx2 = (x1 - x2) * 0.5;
    let dy2 = (y1 - y2) * 0.5;
    let x1p = cos_phi * dx2 + sin_phi * dy2;
    let y1p = -sin_phi * dx2 + cos_phi * dy2;

    let x1p2 = x1p * x1p;
    let y1p2 = y1p * y1p;
    let lambda = x1p2 / (rx * rx) + y1p2 / (ry * ry);
    if lambda > 1.0 {
        let sq = lambda.sqrt();
        rx *= sq;
        ry *= sq;
    }
    let rx2 = rx * rx;
    let ry2 = ry * ry;

    let num = (rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2).max(0.0);
    let den = rx2 * y1p2 + ry2 * x1p2;
    let sq = if den > 1e-10 { (num / den).sqrt() } else { 0.0 };
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let cxp = sign * sq * (rx * y1p / ry);
    let cyp = sign * sq * (-(ry * x1p / rx));

    let cx = cos_phi * cxp - sin_phi * cyp + (x1 + x2) * 0.5;
    let cy = sin_phi * cxp + cos_phi * cyp + (y1 + y2) * 0.5;

    let theta1 = ((y1p - cyp) / ry).atan2((x1p - cxp) / rx);
    let mut dtheta = ((-y1p - cyp) / ry).atan2((-x1p - cxp) / rx) - theta1;

    if sweep && dtheta < 0.0 {
        dtheta += 2.0 * PI;
    } else if !sweep && dtheta > 0.0 {
        dtheta -= 2.0 * PI;
    }

    let n_segs = ((dtheta.abs() / FRAC_PI_2 - 1e-4).ceil().max(1.0)) as usize;
    let seg_angle = dtheta / n_segs as f32;

    let mut angle = theta1;
    for _ in 0..n_segs {
        let a1 = angle;
        let a2 = angle + seg_angle;
        emit_arc_segment(out, cx, cy, rx, ry, cos_phi, sin_phi, a1, a2);
        angle = a2;
    }
}

fn emit_arc_segment(
    out: &mut Vec<u8>,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    cos_phi: f32,
    sin_phi: f32,
    a1: f32,
    a2: f32,
) {
    let da = a2 - a1;
    let half = da * 0.5;
    let alpha = half.sin() * ((4.0 + 3.0 * half.tan().powi(2)).sqrt() - 1.0) / 3.0;

    let (sin1, cos1) = a1.sin_cos();
    let (sin2, cos2) = a2.sin_cos();

    let e1x = rx * cos1;
    let e1y = ry * sin1;
    let e2x = rx * cos2;
    let e2y = ry * sin2;

    let d1x = -rx * sin1;
    let d1y = ry * cos1;
    let d2x = -rx * sin2;
    let d2y = ry * cos2;

    let c1x = e1x + alpha * d1x;
    let c1y = e1y + alpha * d1y;
    let c2x = e2x - alpha * d2x;
    let c2y = e2y - alpha * d2y;

    let q1x = cos_phi * c1x - sin_phi * c1y + cx;
    let q1y = sin_phi * c1x + cos_phi * c1y + cy;
    let q2x = cos_phi * c2x - sin_phi * c2y + cx;
    let q2y = sin_phi * c2x + cos_phi * c2y + cy;
    let px = cos_phi * e2x - sin_phi * e2y + cx;
    let py = sin_phi * e2x + cos_phi * e2y + cy;

    emit_cubic_to(out, q1x, q1y, q2x, q2y, px, py);
}

/// Parse an SVG path `d` attribute into native path commands.
fn parse_svg_path(d: &str) -> Vec<u8> {
    let s = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut cx: f32 = 0.0;
    let mut cy: f32 = 0.0;
    let mut last_cmd = b'M';
    let mut last_c2x: f32 = 0.0;
    let mut last_c2y: f32 = 0.0;
    let mut subpath_x: f32 = 0.0;
    let mut subpath_y: f32 = 0.0;

    while i < s.len() {
        i = skip_ws(s, i);
        if i >= s.len() {
            break;
        }

        let cmd = if s[i].is_ascii_alphabetic() {
            let c = s[i];
            i += 1;
            c
        } else {
            match last_cmd {
                b'M' => b'L',
                b'm' => b'l',
                other => other,
            }
        };

        macro_rules! num {
            ($var:ident) => {
                let $var;
                if let Some((v, ni)) = parse_number(s, i) {
                    $var = v;
                    i = ni;
                } else {
                    last_cmd = cmd;
                    continue;
                }
            };
        }

        macro_rules! flag {
            ($var:ident) => {
                let $var;
                if let Some((v, ni)) = parse_flag(s, i) {
                    $var = v;
                    i = ni;
                } else {
                    last_cmd = cmd;
                    continue;
                }
            };
        }

        match cmd {
            b'M' => {
                num!(x);
                num!(y);
                cx = x;
                cy = y;
                subpath_x = cx;
                subpath_y = cy;
                emit_move_to(&mut out, cx, cy);
            }
            b'm' => {
                num!(dx);
                num!(dy);
                cx += dx;
                cy += dy;
                subpath_x = cx;
                subpath_y = cy;
                emit_move_to(&mut out, cx, cy);
            }
            b'L' => {
                num!(x);
                num!(y);
                cx = x;
                cy = y;
                emit_line_to(&mut out, cx, cy);
            }
            b'l' => {
                num!(dx);
                num!(dy);
                cx += dx;
                cy += dy;
                emit_line_to(&mut out, cx, cy);
            }
            b'H' => {
                num!(x);
                cx = x;
                emit_line_to(&mut out, cx, cy);
            }
            b'h' => {
                num!(dx);
                cx += dx;
                emit_line_to(&mut out, cx, cy);
            }
            b'V' => {
                num!(y);
                cy = y;
                emit_line_to(&mut out, cx, cy);
            }
            b'v' => {
                num!(dy);
                cy += dy;
                emit_line_to(&mut out, cx, cy);
            }
            b'C' => {
                num!(c1x);
                num!(c1y);
                num!(c2x);
                num!(c2y);
                num!(x);
                num!(y);
                last_c2x = c2x;
                last_c2y = c2y;
                cx = x;
                cy = y;
                emit_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'c' => {
                num!(dc1x);
                num!(dc1y);
                num!(dc2x);
                num!(dc2y);
                num!(dx);
                num!(dy);
                let c1x = cx + dc1x;
                let c1y = cy + dc1y;
                let c2x = cx + dc2x;
                let c2y = cy + dc2y;
                last_c2x = c2x;
                last_c2y = c2y;
                cx += dx;
                cy += dy;
                emit_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'S' => {
                let c1x = 2.0 * cx - last_c2x;
                let c1y = 2.0 * cy - last_c2y;
                num!(c2x);
                num!(c2y);
                num!(x);
                num!(y);
                last_c2x = c2x;
                last_c2y = c2y;
                cx = x;
                cy = y;
                emit_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b's' => {
                let c1x = 2.0 * cx - last_c2x;
                let c1y = 2.0 * cy - last_c2y;
                num!(dc2x);
                num!(dc2y);
                num!(dx);
                num!(dy);
                let c2x = cx + dc2x;
                let c2y = cy + dc2y;
                last_c2x = c2x;
                last_c2y = c2y;
                cx += dx;
                cy += dy;
                emit_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'Q' => {
                num!(qx);
                num!(qy);
                num!(x);
                num!(y);
                let c1x = cx + 2.0 / 3.0 * (qx - cx);
                let c1y = cy + 2.0 / 3.0 * (qy - cy);
                let c2x = x + 2.0 / 3.0 * (qx - x);
                let c2y = y + 2.0 / 3.0 * (qy - y);
                last_c2x = c2x;
                last_c2y = c2y;
                cx = x;
                cy = y;
                emit_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'q' => {
                num!(dqx);
                num!(dqy);
                num!(dx);
                num!(dy);
                let qx = cx + dqx;
                let qy = cy + dqy;
                let x = cx + dx;
                let y = cy + dy;
                let c1x = cx + 2.0 / 3.0 * (qx - cx);
                let c1y = cy + 2.0 / 3.0 * (qy - cy);
                let c2x = x + 2.0 / 3.0 * (qx - x);
                let c2y = y + 2.0 / 3.0 * (qy - y);
                last_c2x = c2x;
                last_c2y = c2y;
                cx = x;
                cy = y;
                emit_cubic_to(&mut out, c1x, c1y, c2x, c2y, cx, cy);
            }
            b'A' => {
                num!(arx);
                num!(ary);
                num!(rot);
                flag!(la);
                flag!(sw);
                num!(x);
                num!(y);
                arc_to_cubics(&mut out, cx, cy, arx, ary, rot, la, sw, x, y);
                cx = x;
                cy = y;
            }
            b'a' => {
                num!(arx);
                num!(ary);
                num!(rot);
                flag!(la);
                flag!(sw);
                num!(dx);
                num!(dy);
                let x = cx + dx;
                let y = cy + dy;
                arc_to_cubics(&mut out, cx, cy, arx, ary, rot, la, sw, x, y);
                cx = x;
                cy = y;
            }
            b'Z' | b'z' => {
                emit_close(&mut out);
                cx = subpath_x;
                cy = subpath_y;
            }
            _ => {
                i += 1;
                continue;
            }
        }

        match cmd {
            b'C' | b'c' | b'S' | b's' => {}
            _ => {
                last_c2x = cx;
                last_c2y = cy;
            }
        }
        last_cmd = cmd;
    }

    // Auto-close: if the current point is within tolerance of the subpath
    // start, emit PATH_CLOSE even without an explicit Z in the SVG.
    // Many icon sets (including Tabler) omit Z when the path traces back
    // to its origin. Without this, is_closed() returns false and the
    // cursor renderer can't distinguish filled from stroke-only shapes.
    if !out.is_empty() {
        let dx = cx - subpath_x;
        let dy = cy - subpath_y;
        // 0.01 in viewbox units ≈ 0.04% of a 24-unit viewbox.
        if dx * dx + dy * dy < 0.01 {
            // Only emit if last command isn't already Close.
            let last_tag = u32::from_le_bytes([
                out[out.len() - 4],
                out[out.len() - 3],
                out[out.len() - 2],
                out[out.len() - 1],
            ]);
            if last_tag != PATH_CLOSE {
                emit_close(&mut out);
            }
        }
    }

    out
}

// ── SVG file parser ────────────────────────────────────────────────

/// Extract all <path d="..."> attributes from an SVG file.
fn extract_svg_paths(svg_content: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut rest = svg_content;
    while let Some(pos) = rest.find("<path") {
        rest = &rest[pos..];
        // Find d="..." attribute.
        if let Some(d_pos) = rest.find("d=\"") {
            let d_start = d_pos + 3;
            if let Some(d_end) = rest[d_start..].find('"') {
                paths.push(rest[d_start..d_start + d_end].to_string());
            }
            rest = &rest[d_start..];
        } else {
            rest = &rest[5..]; // skip past "<path"
        }
    }
    paths
}

// ── Icon manifest ──────────────────────────────────────────────────

/// Layer assignment for a sub-path.
#[derive(Clone, Copy)]
enum Layer {
    Primary,
    Secondary,
}

/// One entry in the icon manifest.
struct IconEntry {
    /// OS semantic name.
    os_name: &'static str,
    /// Accessibility label.
    label: &'static str,
    /// Source SVG filename (without .svg extension).
    svg_file: &'static str,
    /// Mimetype this variant matches (None = base icon).
    mimetype: Option<&'static str>,
    /// Mimetype category this variant matches (e.g., "text/").
    /// Used for category fallback. None = exact match only.
    mime_category: Option<&'static str>,
    /// Layer assignments per sub-path index. If shorter than the
    /// number of paths, remaining paths get Primary.
    layers: &'static [Layer],
}

/// The full manifest of curated icons.
const MANIFEST: &[IconEntry] = &[
    // ── document variants ──────────────────────────────────────
    IconEntry {
        os_name: "document",
        label: "Document",
        svg_file: "file",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary, Layer::Primary],
    },
    IconEntry {
        os_name: "document",
        label: "Text document",
        svg_file: "file-text",
        mimetype: None,
        mime_category: Some("text/"),
        layers: &[
            Layer::Primary,
            Layer::Primary,
            Layer::Secondary,
            Layer::Secondary,
            Layer::Secondary,
        ],
    },
    IconEntry {
        os_name: "document",
        label: "Rich text document",
        svg_file: "file-pencil",
        mimetype: Some("text/rich"),
        mime_category: None,
        layers: &[
            Layer::Primary,
            Layer::Primary,
            Layer::Secondary,
            Layer::Secondary,
        ],
    },
    IconEntry {
        os_name: "document",
        label: "Markdown document",
        svg_file: "markdown",
        mimetype: Some("text/markdown"),
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "document",
        label: "Source code",
        svg_file: "file-code",
        mimetype: Some("application/json"),
        mime_category: None,
        layers: &[Layer::Primary, Layer::Primary, Layer::Secondary],
    },
    IconEntry {
        os_name: "document",
        label: "Image",
        svg_file: "photo",
        mimetype: None,
        mime_category: Some("image/"),
        layers: &[Layer::Primary, Layer::Primary, Layer::Secondary],
    },
    IconEntry {
        os_name: "document",
        label: "Audio",
        svg_file: "device-audio-tape",
        mimetype: None,
        mime_category: Some("audio/"),
        layers: &[
            Layer::Primary,
            Layer::Primary,
            Layer::Secondary,
            Layer::Secondary,
        ],
    },
    IconEntry {
        os_name: "document",
        label: "Video",
        svg_file: "movie",
        mimetype: None,
        mime_category: Some("video/"),
        layers: &[Layer::Primary, Layer::Secondary, Layer::Secondary],
    },
    IconEntry {
        os_name: "document",
        label: "Data table",
        svg_file: "table",
        mimetype: Some("text/csv"),
        mime_category: None,
        layers: &[Layer::Primary, Layer::Secondary],
    },
    // ── system UI icons ────────────────────────────────────────
    IconEntry {
        os_name: "search",
        label: "Search",
        svg_file: "search",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary, Layer::Primary],
    },
    IconEntry {
        os_name: "settings",
        label: "Settings",
        svg_file: "settings",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "alert",
        label: "Alert",
        svg_file: "alert-triangle",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary, Layer::Secondary],
    },
    IconEntry {
        os_name: "info",
        label: "Information",
        svg_file: "info-circle",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary, Layer::Secondary],
    },
    IconEntry {
        os_name: "check",
        label: "Confirm",
        svg_file: "check",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "close",
        label: "Close",
        svg_file: "x",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "plus",
        label: "Add",
        svg_file: "plus",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "minus",
        label: "Remove",
        svg_file: "minus",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "arrow-left",
        label: "Navigate left",
        svg_file: "arrow-left",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "arrow-right",
        label: "Navigate right",
        svg_file: "arrow-right",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "arrow-up",
        label: "Navigate up",
        svg_file: "arrow-up",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "arrow-down",
        label: "Navigate down",
        svg_file: "arrow-down",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "undo",
        label: "Undo",
        svg_file: "arrow-back-up",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "redo",
        label: "Redo",
        svg_file: "arrow-forward-up",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "menu",
        label: "Menu",
        svg_file: "menu-2",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "loading",
        label: "Loading",
        svg_file: "loader-2",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    // ── cursor icons ──────────────────────────────────────────
    IconEntry {
        os_name: "pointer",
        label: "Pointer",
        svg_file: "pointer",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary],
    },
    IconEntry {
        os_name: "cursor-text",
        label: "Text cursor",
        svg_file: "cursor-text",
        mimetype: None,
        mime_category: None,
        layers: &[Layer::Primary, Layer::Primary, Layer::Primary],
    },
];

// ── Code generator ─────────────────────────────────────────────────

/// Format a byte slice as a Rust array literal.
fn bytes_to_rust_array(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "b\"\"".to_string();
    }
    let mut s = String::from("&[\n");
    for (i, chunk) in bytes.chunks(16).enumerate() {
        if i > 0 {
            s.push('\n');
        }
        s.push_str("        ");
        for (j, &b) in chunk.iter().enumerate() {
            if j > 0 {
                s.push_str(", ");
            }
            s.push_str(&format!("0x{b:02X}"));
        }
        s.push(',');
    }
    s.push_str("\n    ]");
    s
}

/// Generate the data.rs file content.
pub fn generate_icon_data(icons_dir: &Path, output_path: &Path) {
    let mut code = String::new();
    code.push_str("//! Generated icon data — DO NOT EDIT.\n");
    code.push_str("//!\n");
    code.push_str("//! This file is generated by build.rs from resources/icons/*.svg.\n");
    code.push_str("//! To add or modify icons, edit the MANIFEST in build_icons.rs and\n");
    code.push_str("//! re-run the build.\n\n");

    code.push_str("use crate::{Icon, IconPath, Layer};\n\n");

    // Generate path data constants and Icon statics for each manifest entry.
    let mut icon_count = 0;
    let mut variant_count = 0;

    #[allow(dead_code)]
    struct GeneratedIcon {
        const_name: String,
        os_name: String,
        label: String,
        mimetype: Option<String>,
        mime_category: Option<String>,
    }
    let mut generated: Vec<GeneratedIcon> = Vec::new();

    for entry in MANIFEST {
        let svg_path = icons_dir.join(format!("{}.svg", entry.svg_file));
        let svg_content = std::fs::read_to_string(&svg_path)
            .unwrap_or_else(|e| panic!("Failed to read SVG {}: {e}", svg_path.display()));

        let d_attrs = extract_svg_paths(&svg_content);
        if d_attrs.is_empty() {
            panic!("No <path> elements found in {}", svg_path.display());
        }

        // Generate a unique constant name.
        let const_suffix = match (&entry.mimetype, &entry.mime_category) {
            (Some(mt), _) => format!(
                "{}_{}",
                entry.os_name.to_uppercase().replace('-', "_"),
                mt.replace('/', "_").replace('.', "_").to_uppercase()
            ),
            (_, Some(cat)) => format!(
                "{}_CAT_{}",
                entry.os_name.to_uppercase().replace('-', "_"),
                cat.replace('/', "").to_uppercase()
            ),
            _ => entry.os_name.to_uppercase().replace('-', "_"),
        };

        // Emit path data constants.
        for (pi, d) in d_attrs.iter().enumerate() {
            let path_bytes = parse_svg_path(d);
            let const_name = format!("PATH_{const_suffix}_{pi}");
            code.push_str(&format!(
                "static {const_name}: &[u8] = {};\n",
                bytes_to_rust_array(&path_bytes)
            ));
        }

        // Emit IconPath array.
        let paths_const = format!("PATHS_{const_suffix}");
        code.push_str(&format!("static {paths_const}: &[IconPath] = &[\n"));
        for (pi, _) in d_attrs.iter().enumerate() {
            let layer = entry.layers.get(pi).copied().unwrap_or(Layer::Primary);
            let layer_str = match layer {
                Layer::Primary => "Layer::Primary",
                Layer::Secondary => "Layer::Secondary",
            };
            code.push_str(&format!(
                "    IconPath {{ commands: PATH_{const_suffix}_{pi}, layer: {layer_str} }},\n"
            ));
        }
        code.push_str("];\n");

        // Emit Icon static.
        let icon_const = format!("ICON_{const_suffix}");
        code.push_str(&format!(
            "static {icon_const}: Icon = Icon {{\n    name: \"{}\",\n    label: \"{}\",\n    paths: {paths_const},\n    viewbox: 24.0,\n    stroke_width: 2.0,\n}};\n\n",
            entry.os_name, entry.label
        ));

        generated.push(GeneratedIcon {
            const_name: icon_const,
            os_name: entry.os_name.to_string(),
            label: entry.label.to_string(),
            mimetype: entry.mimetype.map(|s| s.to_string()),
            mime_category: entry.mime_category.map(|s| s.to_string()),
        });

        if entry.mimetype.is_some() || entry.mime_category.is_some() {
            variant_count += 1;
        } else {
            icon_count += 1;
        }
    }

    // Generate lookup function.
    code.push_str("/// Look up an icon by exact name and optional exact mimetype.\n");
    code.push_str(
        "pub(crate) fn lookup(name: &str, mimetype: Option<&str>) -> Option<&'static Icon> {\n",
    );
    code.push_str("    match (name, mimetype) {\n");

    // Exact mimetype matches first.
    for g in &generated {
        if let Some(ref mt) = g.mimetype {
            code.push_str(&format!(
                "        (\"{}\", Some(\"{}\")) => Some(&{}),\n",
                g.os_name, mt, g.const_name
            ));
        }
    }

    // Base icons (no mimetype).
    for g in &generated {
        if g.mimetype.is_none() && g.mime_category.is_none() {
            code.push_str(&format!(
                "        (\"{}\", None) => Some(&{}),\n",
                g.os_name, g.const_name
            ));
        }
    }

    code.push_str("        _ => None,\n");
    code.push_str("    }\n}\n\n");

    // Generate category lookup function.
    code.push_str("/// Look up an icon by name and mimetype category prefix.\n");
    code.push_str(
        "pub(crate) fn lookup_category(name: &str, category: &str) -> Option<&'static Icon> {\n",
    );
    code.push_str("    match (name, category) {\n");

    for g in &generated {
        if let Some(ref cat) = g.mime_category {
            code.push_str(&format!(
                "        (\"{}\", \"{}\") => Some(&{}),\n",
                g.os_name, cat, g.const_name
            ));
        }
    }

    code.push_str("        _ => None,\n");
    code.push_str("    }\n}\n\n");

    // Fallback.
    code.push_str("/// Universal fallback icon (base document).\n");
    code.push_str("pub(crate) fn fallback() -> &'static Icon {\n");
    // Find the base document icon.
    let fallback_name = generated
        .iter()
        .find(|g| g.os_name == "document" && g.mimetype.is_none() && g.mime_category.is_none())
        .map(|g| g.const_name.as_str())
        .unwrap_or("ICON_DOCUMENT");
    code.push_str(&format!("    &{fallback_name}\n"));
    code.push_str("}\n");

    // Write header comment with stats.
    let header = format!(
        "// Generated by: system/build.rs icon converter\n\
         // Source: system/resources/icons/ (Tabler Icons, MIT license)\n\
         // Icons: {icon_count}, Variants: {variant_count}\n\n"
    );

    let final_code = code.replacen(
        "use crate::{Icon, IconPath, Layer};\n\n",
        &format!("use crate::{{Icon, IconPath, Layer}};\n\n{header}"),
        1,
    );

    std::fs::write(output_path, final_code)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", output_path.display()));

    println!(
        "cargo:warning=icons: generated {icon_count} icons + {variant_count} variants from {} SVGs",
        MANIFEST.len()
    );
}
