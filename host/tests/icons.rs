//! Unit tests for the icon library.

use icons::{Icon, Layer};

// ── Lookup tests ───────────────────────────────────────────────────

#[test]
fn icons_base_document_lookup() {
    let icon = icons::get("document", None);
    assert_eq!(icon.name, "document");
    assert_eq!(icon.label, "Document");
}

#[test]
fn icons_text_plain_category_fallback() {
    // text/plain should match the text/* category variant.
    let icon = icons::get("document", Some("text/plain"));
    assert_eq!(icon.name, "document");
    assert_eq!(icon.label, "Text document");
}

#[test]
fn icons_text_rich_exact_match() {
    let icon = icons::get("document", Some("text/rich"));
    assert_eq!(icon.name, "document");
    assert_eq!(icon.label, "Rich text document");
}

#[test]
fn icons_text_markdown_exact_match() {
    let icon = icons::get("document", Some("text/markdown"));
    assert_eq!(icon.name, "document");
    assert_eq!(icon.label, "Markdown document");
}

#[test]
fn icons_image_category_fallback() {
    // image/png should match the image/* category variant.
    let icon = icons::get("document", Some("image/png"));
    assert_eq!(icon.name, "document");
    assert_eq!(icon.label, "Image");
}

#[test]
fn icons_image_jpeg_category_fallback() {
    let icon = icons::get("document", Some("image/jpeg"));
    assert_eq!(icon.label, "Image");
}

#[test]
fn icons_audio_category_fallback() {
    let icon = icons::get("document", Some("audio/mpeg"));
    assert_eq!(icon.label, "Audio");
}

#[test]
fn icons_video_category_fallback() {
    let icon = icons::get("document", Some("video/mp4"));
    assert_eq!(icon.label, "Video");
}

#[test]
fn icons_application_json_exact() {
    let icon = icons::get("document", Some("application/json"));
    assert_eq!(icon.label, "Source code");
}

#[test]
fn icons_text_csv_exact() {
    let icon = icons::get("document", Some("text/csv"));
    assert_eq!(icon.label, "Data table");
}

#[test]
fn icons_unknown_mimetype_falls_to_base() {
    // Unknown mimetype: no exact match, no category match → base document.
    let icon = icons::get("document", Some("application/octet-stream"));
    assert_eq!(icon.label, "Document");
}

#[test]
fn icons_unknown_name_falls_to_universal() {
    // Unknown name → universal fallback (base document).
    let icon = icons::get("nonexistent", None);
    assert_eq!(icon.name, "document");
}

// ── System UI icon lookup ──────────────────────────────────────────

#[test]
fn icons_system_ui_lookup() {
    let names = [
        ("search", "Search"),
        ("settings", "Settings"),
        ("alert", "Alert"),
        ("info", "Information"),
        ("check", "Confirm"),
        ("close", "Close"),
        ("plus", "Add"),
        ("minus", "Remove"),
        ("arrow-left", "Navigate left"),
        ("arrow-right", "Navigate right"),
        ("arrow-up", "Navigate up"),
        ("arrow-down", "Navigate down"),
        ("undo", "Undo"),
        ("redo", "Redo"),
        ("menu", "Menu"),
        ("loading", "Loading"),
    ];
    for (name, label) in names {
        let icon = icons::get(name, None);
        assert_eq!(icon.name, name, "icon name mismatch for {name}");
        assert_eq!(icon.label, label, "icon label mismatch for {name}");
    }
}

#[test]
fn icons_system_ui_ignore_mimetype() {
    // System UI icons should return the base icon regardless of mimetype.
    let icon = icons::get("search", Some("text/plain"));
    assert_eq!(icon.name, "search");
}

// ── Path data validity ─────────────────────────────────────────────

/// Verify that path commands are well-formed binary data.
fn validate_path_commands(commands: &[u8]) -> bool {
    let mut pos = 0;
    while pos < commands.len() {
        if pos + 4 > commands.len() {
            return false;
        }
        let tag = u32::from_le_bytes([
            commands[pos],
            commands[pos + 1],
            commands[pos + 2],
            commands[pos + 3],
        ]);
        match tag {
            0 | 1 => {
                // MoveTo / LineTo: tag(4) + x(4) + y(4) = 12
                if pos + 12 > commands.len() {
                    return false;
                }
                pos += 12;
            }
            2 => {
                // CubicTo: tag(4) + 6×f32 = 28
                if pos + 28 > commands.len() {
                    return false;
                }
                pos += 28;
            }
            3 => {
                // Close: tag(4) = 4
                pos += 4;
            }
            _ => return false,
        }
    }
    true
}

#[test]
fn icons_path_data_well_formed() {
    let all_names = [
        "document",
        "search",
        "settings",
        "alert",
        "info",
        "check",
        "close",
        "plus",
        "minus",
        "arrow-left",
        "arrow-right",
        "arrow-up",
        "arrow-down",
        "undo",
        "redo",
        "menu",
        "loading",
    ];
    for name in all_names {
        let icon = icons::get(name, None);
        assert!(!icon.paths.is_empty(), "{name} has no paths");
        for (i, path) in icon.paths.iter().enumerate() {
            assert!(
                !path.commands.is_empty(),
                "{name} path {i} has empty commands"
            );
            assert!(
                validate_path_commands(path.commands),
                "{name} path {i} has malformed commands"
            );
        }
    }
}

#[test]
fn icons_path_coordinates_in_viewbox() {
    // All path coordinates should be within the 24×24 viewbox (with margin).
    let icon = icons::get("document", None);
    for path in icon.paths {
        let cmds = path.commands;
        let mut pos = 0;
        while pos < cmds.len() {
            let tag = u32::from_le_bytes([cmds[pos], cmds[pos + 1], cmds[pos + 2], cmds[pos + 3]]);
            let coords = match tag {
                0 | 1 => {
                    let x = f32::from_le_bytes([
                        cmds[pos + 4],
                        cmds[pos + 5],
                        cmds[pos + 6],
                        cmds[pos + 7],
                    ]);
                    let y = f32::from_le_bytes([
                        cmds[pos + 8],
                        cmds[pos + 9],
                        cmds[pos + 10],
                        cmds[pos + 11],
                    ]);
                    pos += 12;
                    vec![(x, y)]
                }
                2 => {
                    let mut cs = Vec::new();
                    for ci in 0..3 {
                        let off = pos + 4 + ci * 8;
                        let x = f32::from_le_bytes([
                            cmds[off],
                            cmds[off + 1],
                            cmds[off + 2],
                            cmds[off + 3],
                        ]);
                        let y = f32::from_le_bytes([
                            cmds[off + 4],
                            cmds[off + 5],
                            cmds[off + 6],
                            cmds[off + 7],
                        ]);
                        cs.push((x, y));
                    }
                    pos += 28;
                    cs
                }
                3 => {
                    pos += 4;
                    vec![]
                }
                _ => break,
            };
            for (x, y) in coords {
                assert!(x >= -2.0 && x <= 26.0, "x={x} out of viewbox");
                assert!(y >= -2.0 && y <= 26.0, "y={y} out of viewbox");
            }
        }
    }
}

// ── Metadata tests ─────────────────────────────────────────────────

#[test]
fn icons_all_have_viewbox_24() {
    let icon = icons::get("document", None);
    assert_eq!(icon.viewbox, 24.0);

    let icon = icons::get("search", None);
    assert_eq!(icon.viewbox, 24.0);
}

#[test]
fn icons_all_have_stroke_width() {
    let icon = icons::get("document", None);
    assert_eq!(icon.stroke_width, 2.0);
}

#[test]
fn icons_layer_assignments() {
    // Document text variant: first two paths are Primary (page), rest Secondary (text lines).
    let icon = icons::get("document", Some("text/plain"));
    assert!(
        icon.paths.len() >= 3,
        "text document should have >= 3 paths"
    );
    assert_eq!(icon.paths[0].layer, Layer::Primary);
    assert_eq!(icon.paths[1].layer, Layer::Primary);
    // Text line paths should be Secondary.
    for path in &icon.paths[2..] {
        assert_eq!(path.layer, Layer::Secondary);
    }
}

// ── Mimetype category helper ───────────────────────────────────────

#[test]
fn icons_mimetype_category() {
    assert_eq!(icons::mimetype_category("text/plain"), Some("text/"));
    assert_eq!(icons::mimetype_category("image/png"), Some("image/"));
    assert_eq!(icons::mimetype_category("noSlash"), None);
}
