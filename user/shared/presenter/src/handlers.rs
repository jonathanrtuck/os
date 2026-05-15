//! Content viewers — per-mimetype components that produce view subtrees.

use alloc::vec;

use scene::{Color, Content, SceneWriter, pt, upt};
use view_tree::{
    Constraints, EventResponse, InputEvent, IntrinsicSize, LayoutBox, ViewContent, ViewNode,
    ViewSubtree, ViewTree, Viewer,
};

// ── Content commands — viewer → presenter IPC requests ──────────────

#[allow(dead_code)]
pub enum ContentCommand {
    MoveCursor(usize),
    Select { anchor: usize, cursor: usize },
    ForwardKey(text_editor::KeyDispatch),
    TogglePlayback,
    ScrollTo(i32),
    SetCursorShape(u8),
    Unhandled,
}

// ── Image viewer ───────────────────────────────────────────────────

pub struct ImageViewer {
    pub content_id: u32,
    pub pixel_width: u16,
    pub pixel_height: u16,
    subtree: ViewSubtree,
}

impl ImageViewer {
    pub fn new(content_id: u32, pixel_width: u16, pixel_height: u16) -> Self {
        let tree = ViewTree::new();

        Self {
            content_id,
            pixel_width,
            pixel_height,
            subtree: ViewSubtree {
                tree,
                root: scene::NULL,
                layout: vec![],
            },
        }
    }

    pub fn rebuild(&mut self, constraints: &Constraints) {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            intrinsic: IntrinsicSize::Fixed {
                width: upt(self.pixel_width as u32),
                height: upt(self.pixel_height as u32),
            },
            role: scene::ROLE_IMAGE,
            content: ViewContent::Image {
                content_id: self.content_id,
                src_width: self.pixel_width,
                src_height: self.pixel_height,
            },
            shadow_color: Color::rgba(0, 0, 0, 255),
            shadow_blur_radius: upt(presenter_service::SHADOW_BLUR_RADIUS as u32),
            shadow_spread: pt(presenter_service::SHADOW_SPREAD as i32),
            ..Default::default()
        });
        let layout = view_tree::layout(
            tree.nodes(),
            root,
            constraints.available_width,
            constraints.available_height,
            &NoMeasurer,
        );

        self.subtree = ViewSubtree { tree, root, layout };
    }
}

impl Viewer for ImageViewer {
    fn subtree(&self) -> &ViewSubtree {
        &self.subtree
    }

    fn event(&mut self, _event: &InputEvent) -> EventResponse {
        EventResponse::Unhandled
    }

    fn teardown(&mut self) {}
}

// ── Video viewer ───────────────────────────────────────────────────

pub struct VideoViewer {
    pub content_id: u32,
    pub pixel_width: u16,
    pub pixel_height: u16,
    pub playing: bool,
    pub decoder_ep: abi::types::Handle,
    pub frame_vmo: abi::types::Handle,
    pub frame_va: usize,
    pub total_frames: u32,
    subtree: ViewSubtree,
}

impl VideoViewer {
    pub fn new(content_id: u32, pixel_width: u16, pixel_height: u16) -> Self {
        let tree = ViewTree::new();

        Self {
            content_id,
            pixel_width,
            pixel_height,
            playing: false,
            decoder_ep: abi::types::Handle(0),
            frame_vmo: abi::types::Handle(0),
            frame_va: 0,
            total_frames: 0,
            subtree: ViewSubtree {
                tree,
                root: scene::NULL,
                layout: vec![],
            },
        }
    }

    pub fn rebuild(&mut self, constraints: &Constraints) {
        let mut tree = ViewTree::new();
        let (disp_w, disp_h) = aspect_fit(
            self.pixel_width as u32,
            self.pixel_height as u32,
            constraints.available_width / scene::MPT_PER_PT as u32,
            constraints.available_height / scene::MPT_PER_PT as u32,
        );
        let root = tree.add(ViewNode {
            display: view_tree::Display::FixedCanvas,
            width: view_tree::Dimension::Points(upt(disp_w)),
            height: view_tree::Dimension::Points(upt(disp_h)),
            ..Default::default()
        });
        let _frame = tree.add(ViewNode {
            intrinsic: IntrinsicSize::Fixed {
                width: upt(disp_w),
                height: upt(disp_h),
            },
            content: ViewContent::Image {
                content_id: self.content_id,
                src_width: self.pixel_width,
                src_height: self.pixel_height,
            },
            shadow_color: Color::rgba(0, 0, 0, 255),
            shadow_blur_radius: upt(presenter_service::SHADOW_BLUR_RADIUS as u32),
            shadow_spread: pt(presenter_service::SHADOW_SPREAD as i32),
            ..Default::default()
        });

        tree.append_child(root, _frame);

        let btn_size = 80u32;
        let btn_x = ((disp_w as i32 - btn_size as i32) / 2).max(0);
        let btn_y = ((disp_h as i32 - btn_size as i32) / 2).max(0);
        let icon_name = if self.playing {
            "player-pause"
        } else {
            "player-play"
        };
        let icon_data = icons::get(icon_name, None);
        let icon_padding = 16u32;
        let icon_size_pt = btn_size - icon_padding * 2;
        let icon_paths = scale_icon_view_paths(icon_data, icon_size_pt);
        let icon_hash = scene::fnv1a(&icon_paths);
        let icon_sw = icon_stroke_width_umpt(
            icon_data.stroke_width.to_bits(),
            icon_size_pt,
            icon_data.viewbox.to_bits(),
        );
        let btn = tree.add(ViewNode {
            offset_x: pt(btn_x),
            offset_y: pt(btn_y),
            intrinsic: IntrinsicSize::Fixed {
                width: upt(btn_size),
                height: upt(btn_size),
            },
            background: Color::rgba(0, 0, 0, 120),
            corner_radius: upt(40),
            role: scene::ROLE_BUTTON,
            state: scene::STATE_FOCUSABLE,
            cursor_shape: scene::CURSOR_PRESSABLE,
            name: Some(alloc::string::String::from("Play video")),
            ..Default::default()
        });

        tree.append_child(root, btn);

        let icon = tree.add(ViewNode {
            offset_x: pt(icon_padding as i32),
            offset_y: pt(icon_padding as i32),
            intrinsic: IntrinsicSize::Fixed {
                width: upt(icon_size_pt),
                height: upt(icon_size_pt),
            },
            content: ViewContent::Path {
                commands: icon_paths,
                color: Color::TRANSPARENT,
                stroke_color: Color::rgba(255, 255, 255, 200),
                fill_rule: scene::FillRule::Winding,
                stroke_width: icon_sw,
                content_hash: icon_hash,
            },
            ..Default::default()
        });

        tree.append_child(btn, icon);

        let layout = view_tree::layout(tree.nodes(), root, upt(disp_w), upt(disp_h), &NoMeasurer);

        self.subtree = ViewSubtree { tree, root, layout };
    }
}

impl Viewer for VideoViewer {
    fn subtree(&self) -> &ViewSubtree {
        &self.subtree
    }

    fn event(&mut self, _event: &InputEvent) -> EventResponse {
        EventResponse::Unhandled
    }

    fn teardown(&mut self) {
        if self.playing {
            let _ = ipc::client::call_simple(self.decoder_ep, video_decoder::PAUSE, &[]);

            self.playing = false;
        }
        if self.frame_va != 0 {
            let _ = abi::vmo::unmap(self.frame_va);

            self.frame_va = 0;
        }
        if self.decoder_ep.0 != 0 {
            let _ = abi::handle::close(self.decoder_ep);

            self.decoder_ep = abi::types::Handle(0);
        }
        if self.frame_vmo.0 != 0 {
            let _ = abi::handle::close(self.frame_vmo);

            self.frame_vmo = abi::types::Handle(0);
        }
    }
}

// ── Text viewer ────────────────────────────────────────────────────

pub const MAX_GLYPHS_PER_LINE: usize = 256;

pub struct TextRebuildContext<'a> {
    pub content: &'a [u8],
    pub doc_va: usize,
    pub results_buf: &'a [u8],
    pub layout_header: layout_service::LayoutHeader,
    pub page_w: u32,
    pub page_h: u32,
    pub page_padding: u32,
    pub cursor_pos: usize,
    pub sel_anchor: usize,
    pub content_len: usize,
    pub display_width: u32,
}

pub struct TextViewer {
    pub scroll_y: i32,
    pub blink_start: u64,
    pub cmap_mono: [u16; 128],
    pub cmap_sans: [u16; 128],
    pub char_width_mpt: scene::Mpt,
    glyphs: [scene::ShapedGlyph; MAX_GLYPHS_PER_LINE],
    subtree: ViewSubtree,
}

impl TextViewer {
    pub fn new(cmap_mono: [u16; 128], cmap_sans: [u16; 128], char_width_mpt: scene::Mpt) -> Self {
        Self {
            scroll_y: 0,
            blink_start: 0,
            cmap_mono,
            cmap_sans,
            char_width_mpt,
            glyphs: [scene::ShapedGlyph {
                glyph_id: 0,
                _pad: 0,
                x_advance: 0,
                x_offset: 0,
                y_offset: 0,
            }; MAX_GLYPHS_PER_LINE],
            subtree: ViewSubtree {
                tree: ViewTree::new(),
                root: scene::NULL,
                layout: vec![],
            },
        }
    }

    pub fn rebuild(&mut self, ctx: &TextRebuildContext<'_>) {
        let mut tree = ViewTree::new();
        let line_count = ctx.layout_header.line_count as usize;
        let is_rich = ctx.layout_header.format == 1;
        let has_selection = ctx.sel_anchor != ctx.cursor_pos;
        let sel_start = ctx.sel_anchor.min(ctx.cursor_pos);
        let sel_end = ctx.sel_anchor.max(ctx.cursor_pos);
        let text_area_w = ctx.page_w.saturating_sub(2 * ctx.page_padding);
        let text_area_h = ctx.page_h.saturating_sub(2 * ctx.page_padding);
        let page_bg = Color::rgb(
            presenter_service::PAGE_BG_R,
            presenter_service::PAGE_BG_G,
            presenter_service::PAGE_BG_B,
        );
        let sel_color = Color::rgba(
            presenter_service::SEL_R,
            presenter_service::SEL_G,
            presenter_service::SEL_B,
            presenter_service::SEL_A,
        );
        let text_color = Color::rgb(
            presenter_service::TEXT_R,
            presenter_service::TEXT_G,
            presenter_service::TEXT_B,
        );
        let cursor_color = Color::rgb(
            presenter_service::CURSOR_R,
            presenter_service::CURSOR_G,
            presenter_service::CURSOR_B,
        );
        let root = tree.add(ViewNode {
            display: view_tree::Display::FixedCanvas,
            width: view_tree::Dimension::Points(upt(ctx.page_w)),
            height: view_tree::Dimension::Points(upt(ctx.page_h)),
            background: page_bg,
            shadow_color: Color::rgba(0, 0, 0, 255),
            shadow_blur_radius: upt(presenter_service::SHADOW_BLUR_RADIUS as u32),
            shadow_spread: pt(presenter_service::SHADOW_SPREAD as i32),
            cursor_shape: scene::CURSOR_TEXT,
            ..Default::default()
        });
        let viewport = tree.add(ViewNode {
            display: view_tree::Display::FixedCanvas,
            offset_x: pt(ctx.page_padding as i32),
            offset_y: pt(ctx.page_padding as i32),
            width: view_tree::Dimension::Points(upt(text_area_w)),
            height: view_tree::Dimension::Points(upt(text_area_h)),
            clips_children: true,
            child_offset_y: -pt(self.scroll_y),
            role: scene::ROLE_DOCUMENT,
            ..Default::default()
        });

        tree.append_child(root, viewport);

        if has_selection && line_count > 0 {
            if is_rich {
                self.build_rich_selection_view_nodes(
                    &mut tree, viewport, ctx, sel_start, sel_end, sel_color,
                );
            } else {
                self.build_selection_view_nodes(
                    &mut tree, viewport, ctx, sel_start, sel_end, sel_color,
                );
            }
        }

        if is_rich {
            self.build_rich_text_view_nodes(&mut tree, viewport, ctx);
        } else {
            self.build_plain_text_view_nodes(&mut tree, viewport, ctx, text_color);
        }

        let rich_cursor = if is_rich {
            Some(super::build::compute_rich_cursor(
                ctx.results_buf,
                &ctx.layout_header,
                ctx.cursor_pos,
                ctx.doc_va,
            ))
        } else {
            None
        };

        self.build_cursor_view_node(
            &mut tree,
            viewport,
            ctx,
            cursor_color,
            has_selection,
            &rich_cursor,
        );

        let layout = view_tree::layout(
            tree.nodes(),
            root,
            upt(ctx.page_w),
            upt(ctx.page_h),
            &NoMeasurer,
        );

        self.subtree = ViewSubtree { tree, root, layout };
    }

    fn build_selection_view_nodes(
        &self,
        tree: &mut ViewTree,
        viewport: scene::NodeId,
        ctx: &TextRebuildContext<'_>,
        sel_start: usize,
        sel_end: usize,
        color: Color,
    ) {
        let line_count = ctx.layout_header.line_count as usize;
        let line_height = presenter_service::LINE_HEIGHT;

        for i in 0..line_count {
            let line_info = super::parse_line_at(ctx.results_buf, i);
            let line_byte_start = line_info.byte_offset as usize;
            let line_byte_end = line_byte_start + line_info.byte_length as usize;

            if sel_end <= line_byte_start || sel_start >= line_byte_end {
                continue;
            }

            let col_start = sel_start.saturating_sub(line_byte_start);
            let col_end = if sel_end < line_byte_end {
                sel_end - line_byte_start
            } else {
                line_info.byte_length as usize
            };

            if col_start >= col_end {
                continue;
            }

            let x_mpt = col_start as i32 * self.char_width_mpt;
            let w_mpt = (col_end - col_start) as i32 * self.char_width_mpt;
            let sel_node = tree.add(ViewNode {
                offset_x: x_mpt,
                offset_y: pt(line_info.y),
                intrinsic: IntrinsicSize::Fixed {
                    width: w_mpt as u32,
                    height: upt(line_height),
                },
                background: color,
                role: scene::ROLE_SELECTION,
                ..Default::default()
            });

            tree.append_child(viewport, sel_node);
        }
    }

    fn build_rich_selection_view_nodes(
        &self,
        tree: &mut ViewTree,
        viewport: scene::NodeId,
        ctx: &TextRebuildContext<'_>,
        sel_start: usize,
        sel_end: usize,
        color: Color,
    ) {
        let line_count = ctx.layout_header.line_count as usize;
        let run_count = ctx.layout_header.visible_run_count as usize;
        let doc_buf = unsafe {
            core::slice::from_raw_parts(
                (ctx.doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
                ctx.content_len,
            )
        };

        if !piecetable::validate(doc_buf) {
            return;
        }

        for i in 0..line_count {
            let line = super::parse_line_at(ctx.results_buf, i);
            let line_start = line.byte_offset as usize;
            let line_end = line_start + line.byte_length as usize;

            if sel_end <= line_start || sel_start >= line_end {
                continue;
            }

            let x_start = if sel_start <= line_start {
                0.0
            } else {
                super::build::byte_to_x_rich(doc_buf, ctx.results_buf, run_count, sel_start, i)
            };
            let x_end = if sel_end >= line_end {
                super::build::byte_to_x_rich(doc_buf, ctx.results_buf, run_count, line_end, i)
            } else {
                super::build::byte_to_x_rich(doc_buf, ctx.results_buf, run_count, sel_end, i)
            };
            let w = x_end - x_start;

            if w <= 0.0 {
                continue;
            }

            let mut max_font = 14u16;

            for run_i in 0..run_count {
                let vr = super::parse_visible_run_at(ctx.results_buf, run_i);

                if vr.line_index as usize == i && vr.font_size > max_font {
                    max_font = vr.font_size;
                }
            }

            let line_h = (max_font as u32 * 14) / 10;
            let sel_node = tree.add(ViewNode {
                offset_x: scene::f32_to_mpt(x_start),
                offset_y: pt(line.y),
                intrinsic: IntrinsicSize::Fixed {
                    width: scene::f32_to_mpt(w) as u32,
                    height: upt(line_h),
                },
                background: color,
                role: scene::ROLE_SELECTION,
                ..Default::default()
            });

            tree.append_child(viewport, sel_node);
        }
    }

    fn build_rich_text_view_nodes(
        &mut self,
        tree: &mut ViewTree,
        viewport: scene::NodeId,
        ctx: &TextRebuildContext<'_>,
    ) {
        let run_count = ctx.layout_header.visible_run_count as usize;

        if run_count == 0 {
            return;
        }

        let doc_buf = unsafe {
            core::slice::from_raw_parts(
                (ctx.doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
                ctx.content_len,
            )
        };

        if !piecetable::validate(doc_buf) {
            return;
        }

        let text_len = piecetable::text_len(doc_buf) as usize;
        let mut text_scratch = alloc::vec![0u8; text_len + 1];
        let copied = piecetable::text_slice(doc_buf, 0, text_len as u32, &mut text_scratch);

        for run_i in 0..run_count {
            let vr = super::parse_visible_run_at(ctx.results_buf, run_i);
            let byte_start = vr.byte_offset as usize;
            let byte_len = vr.byte_length as usize;

            if byte_start + byte_len > copied || byte_len == 0 {
                continue;
            }

            let run_text = &text_scratch[byte_start..byte_start + byte_len];
            let font_data = super::font_data_for_style(vr.font_family, vr.flags);
            let sid = super::pack_run_style_id(vr.font_family, vr.weight, vr.flags);
            let upem = fonts::metrics::font_metrics(font_data)
                .map(|m| m.units_per_em)
                .unwrap_or(1000);
            let (axes_buf, axis_count) = super::build::build_font_axes(vr.weight, vr.font_size);
            let axes = &axes_buf[..axis_count];
            let mut glyph_count = 0usize;

            for ch in core::str::from_utf8(run_text).unwrap_or("").chars() {
                if glyph_count >= MAX_GLYPHS_PER_LINE {
                    break;
                }

                let gid = fonts::metrics::glyph_id_for_char(font_data, ch).unwrap_or(0);
                let advance_fu = fonts::metrics::glyph_h_advance_with_axes(font_data, gid, axes)
                    .unwrap_or(upem as i32 / 2);
                let advance_fp =
                    (advance_fu as i64 * vr.font_size as i64 * 65536 / upem as i64) as i32;

                self.glyphs[glyph_count] = scene::ShapedGlyph {
                    glyph_id: gid,
                    _pad: 0,
                    x_advance: advance_fp,
                    x_offset: 0,
                    y_offset: 0,
                };
                glyph_count += 1;
            }

            if glyph_count == 0 {
                continue;
            }

            let color = Color::rgba(
                ((vr.color_rgba >> 24) & 0xFF) as u8,
                ((vr.color_rgba >> 16) & 0xFF) as u8,
                ((vr.color_rgba >> 8) & 0xFF) as u8,
                (vr.color_rgba & 0xFF) as u8,
            );
            let line_height = (vr.font_size as u32 * 14) / 10;
            let run_node = tree.add(ViewNode {
                offset_x: scene::f32_to_mpt(vr.x),
                offset_y: pt(vr.y),
                intrinsic: IntrinsicSize::Fixed {
                    width: upt(ctx.display_width),
                    height: upt(line_height),
                },
                content: ViewContent::Glyphs {
                    glyphs: self.glyphs[..glyph_count].to_vec(),
                    color,
                    font_size: vr.font_size,
                    style_id: sid,
                },
                role: scene::ROLE_PARAGRAPH,
                ..Default::default()
            });

            tree.append_child(viewport, run_node);

            if vr.flags & piecetable::FLAG_UNDERLINE != 0 {
                let run_width: f32 = self.glyphs[..glyph_count]
                    .iter()
                    .map(|g| (g.x_advance as f32) / 65536.0)
                    .sum();
                let baseline_y = vr.y + (vr.font_size as i32 * 11) / 10;
                let thickness = (vr.font_size as u32 / 14).max(1);
                let ul_node = tree.add(ViewNode {
                    offset_x: scene::f32_to_mpt(vr.x),
                    offset_y: pt(baseline_y),
                    intrinsic: IntrinsicSize::Fixed {
                        width: upt(run_width as u32 + 1),
                        height: upt(thickness),
                    },
                    background: color,
                    ..Default::default()
                });

                tree.append_child(viewport, ul_node);
            }
        }
    }

    fn build_plain_text_view_nodes(
        &mut self,
        tree: &mut ViewTree,
        viewport: scene::NodeId,
        ctx: &TextRebuildContext<'_>,
        text_color: Color,
    ) {
        let line_count = ctx.layout_header.line_count as usize;
        let char_advance = self.char_width_mpt * 64;

        for i in 0..line_count.min(scene::MAX_NODES - 4) {
            let line_info = super::parse_line_at(ctx.results_buf, i);
            let line_start = line_info.byte_offset as usize;
            let line_len = line_info.byte_length as usize;

            if line_len == 0 {
                continue;
            }

            let line_bytes = if line_start + line_len <= ctx.content_len {
                &ctx.content[line_start..line_start + line_len]
            } else {
                continue;
            };
            let glyph_count = line_len.min(MAX_GLYPHS_PER_LINE);
            let mut needs_fallback = false;

            for (j, &byte) in line_bytes.iter().enumerate().take(glyph_count) {
                let mono_gid = if byte < 128 {
                    self.cmap_mono[byte as usize]
                } else {
                    0
                };

                if mono_gid > 0 {
                    self.glyphs[j] = scene::ShapedGlyph {
                        glyph_id: mono_gid,
                        _pad: super::STYLE_MONO as u16,
                        x_advance: char_advance,
                        x_offset: 0,
                        y_offset: 0,
                    };
                } else {
                    let sans_gid = if byte < 128 {
                        self.cmap_sans[byte as usize]
                    } else {
                        0
                    };

                    if sans_gid > 0 {
                        self.glyphs[j] = scene::ShapedGlyph {
                            glyph_id: sans_gid,
                            _pad: super::STYLE_SANS as u16,
                            x_advance: char_advance,
                            x_offset: 0,
                            y_offset: 0,
                        };
                        needs_fallback = true;
                    } else {
                        self.glyphs[j] = scene::ShapedGlyph {
                            glyph_id: 0,
                            _pad: super::STYLE_MONO as u16,
                            x_advance: char_advance,
                            x_offset: 0,
                            y_offset: 0,
                        };
                    }
                }
            }

            if !needs_fallback {
                for j in 0..glyph_count {
                    self.glyphs[j]._pad = 0;
                }

                let line_node = tree.add(ViewNode {
                    offset_x: line_info.x_mpt,
                    offset_y: pt(line_info.y),
                    intrinsic: IntrinsicSize::Fixed {
                        width: (line_info.width_mpt as u32).saturating_add(upt(1)),
                        height: upt(presenter_service::LINE_HEIGHT),
                    },
                    content: ViewContent::Glyphs {
                        glyphs: self.glyphs[..glyph_count].to_vec(),
                        color: text_color,
                        font_size: presenter_service::FONT_SIZE,
                        style_id: super::STYLE_MONO,
                    },
                    role: scene::ROLE_PARAGRAPH,
                    ..Default::default()
                });

                tree.append_child(viewport, line_node);
            } else {
                let mut run_start = 0;

                while run_start < glyph_count {
                    let run_style = self.glyphs[run_start]._pad as u32;
                    let mut run_end = run_start + 1;

                    while run_end < glyph_count && self.glyphs[run_end]._pad as u32 == run_style {
                        run_end += 1;
                    }

                    let run_len = run_end - run_start;

                    for j in run_start..run_end {
                        self.glyphs[j]._pad = 0;
                    }

                    let run_x_mpt = line_info.x_mpt + run_start as i32 * self.char_width_mpt;
                    let run_node = tree.add(ViewNode {
                        offset_x: run_x_mpt,
                        offset_y: pt(line_info.y),
                        intrinsic: IntrinsicSize::Fixed {
                            width: (run_len as u32 * self.char_width_mpt as u32)
                                .saturating_add(upt(1)),
                            height: upt(presenter_service::LINE_HEIGHT),
                        },
                        content: ViewContent::Glyphs {
                            glyphs: self.glyphs[run_start..run_end].to_vec(),
                            color: text_color,
                            font_size: presenter_service::FONT_SIZE,
                            style_id: run_style,
                        },
                        role: scene::ROLE_PARAGRAPH,
                        ..Default::default()
                    });

                    tree.append_child(viewport, run_node);

                    run_start = run_end;
                }
            }
        }
    }

    fn build_cursor_view_node(
        &self,
        tree: &mut ViewTree,
        viewport: scene::NodeId,
        ctx: &TextRebuildContext<'_>,
        cursor_color: Color,
        has_selection: bool,
        rich_cursor: &Option<super::build::RichCursorInfo>,
    ) {
        let line_count = ctx.layout_header.line_count as usize;
        let mut cursor_line = 0u32;
        let mut cursor_col = 0u32;

        if line_count > 0 {
            for i in 0..line_count {
                let line = super::parse_line_at(ctx.results_buf, i);
                let start = line.byte_offset as usize;
                let next_start = if i + 1 < line_count {
                    super::parse_line_at(ctx.results_buf, i + 1).byte_offset as usize
                } else {
                    usize::MAX
                };

                if ctx.cursor_pos < next_start {
                    cursor_line = i as u32;
                    cursor_col = ctx.cursor_pos.saturating_sub(start) as u32;

                    break;
                }
            }

            if ctx.cursor_pos >= ctx.content_len && line_count > 0 {
                let last = super::parse_line_at(ctx.results_buf, line_count - 1);
                let last_end = last.byte_offset as usize + last.byte_length as usize;
                let last_byte_offset = last.byte_offset as usize;

                if ctx.cursor_pos >= last_end && ctx.cursor_pos > last_byte_offset {
                    cursor_line = (line_count - 1) as u32;
                    cursor_col = (ctx.cursor_pos - last_byte_offset) as u32;
                }
            }
        }

        let (cursor_x_mpt, cursor_y, cursor_h, cursor_style_color, cursor_weight, cursor_skew_bits) =
            if let Some(ci) = rich_cursor {
                (
                    scene::f32_to_mpt(ci.x),
                    ci.y,
                    ci.height,
                    Some(ci.color_rgba),
                    ci.weight,
                    ci.caret_skew.to_bits(),
                )
            } else {
                (
                    cursor_col as i32 * self.char_width_mpt,
                    cursor_line as i32 * presenter_service::LINE_HEIGHT as i32,
                    presenter_service::LINE_HEIGHT,
                    None,
                    400u16,
                    0u32,
                )
            };
        let effective_cursor_color = match cursor_style_color {
            Some(rgba) => Color::rgba(
                ((rgba >> 24) & 0xFF) as u8,
                ((rgba >> 16) & 0xFF) as u8,
                ((rgba >> 8) & 0xFF) as u8,
                (rgba & 0xFF) as u8,
            ),
            None => cursor_color,
        };
        let cursor_w_mpt = scene::MPT_PER_PT as u32
            + (cursor_weight.saturating_sub(100) as u32) * 3 * scene::MPT_PER_PT as u32 / 800;
        let transform = if cursor_skew_bits != 0 {
            scene::AffineTransform {
                a: 1.0,
                b: 0.0,
                c: f32::from_bits(cursor_skew_bits),
                d: 1.0,
                tx: 0.0,
                ty: 0.0,
            }
        } else {
            scene::AffineTransform::identity()
        };
        let animation = if !has_selection {
            scene::Animation::cursor_blink(self.blink_start)
        } else {
            scene::Animation::NONE
        };
        let cursor_node = tree.add(ViewNode {
            offset_x: cursor_x_mpt,
            offset_y: pt(cursor_y),
            intrinsic: IntrinsicSize::Fixed {
                width: cursor_w_mpt,
                height: upt(cursor_h),
            },
            background: effective_cursor_color,
            transform,
            animation,
            role: scene::ROLE_CARET,
            ..Default::default()
        });

        tree.append_child(viewport, cursor_node);
    }
}

impl Viewer for TextViewer {
    fn subtree(&self) -> &ViewSubtree {
        &self.subtree
    }

    fn event(&mut self, _event: &InputEvent) -> EventResponse {
        EventResponse::Unhandled
    }

    fn teardown(&mut self) {}
}

// ── Viewer kind — enum dispatch ─────────────────────────────────────

#[allow(clippy::large_enum_variant)]
pub enum ViewerKind {
    Image(ImageViewer),
    Text(TextViewer),
    Video(VideoViewer),
}

pub struct ChildViewer {
    pub viewer: ViewerKind,
    pub mimetype: &'static [u8],
}

// ── Workspace viewer ────────────────────────────────────────────────

#[allow(dead_code)]
pub struct WorkspaceRebuildContext {
    pub display_width: u32,
    pub display_height: u32,
    pub now_ns: u64,
    pub rtc_secs: u64,
    pub active_mimetype: Option<&'static str>,
}

pub struct WorkspaceViewer {
    pub children: alloc::vec::Vec<ChildViewer>,
    pub active: usize,
    pub slide_spring: animation::SpringI32,
    pub slide_animating: bool,
    pub last_anim_tick: u64,
    glyphs: [scene::ShapedGlyph; MAX_GLYPHS_PER_LINE],
    subtree: ViewSubtree,
}

impl WorkspaceViewer {
    pub fn new() -> Self {
        let mut spring = animation::SpringI32::new(0, 600, 49, 1);

        spring.set_settle_threshold(512);

        Self {
            children: alloc::vec::Vec::new(),
            active: 0,
            slide_spring: spring,
            slide_animating: false,
            last_anim_tick: 0,
            glyphs: [scene::ShapedGlyph {
                glyph_id: 0,
                _pad: 0,
                x_advance: 0,
                x_offset: 0,
                y_offset: 0,
            }; MAX_GLYPHS_PER_LINE],
            subtree: ViewSubtree {
                tree: ViewTree::new(),
                root: scene::NULL,
                layout: vec![],
            },
        }
    }

    pub fn child_subtrees(&self) -> alloc::vec::Vec<&ViewSubtree> {
        self.children
            .iter()
            .map(|c| match &c.viewer {
                ViewerKind::Image(v) => v.subtree(),
                ViewerKind::Text(v) => v.subtree(),
                ViewerKind::Video(v) => v.subtree(),
            })
            .collect()
    }

    pub fn visible_range(&self, display_width: u32) -> (usize, usize) {
        let last = self.children.len().saturating_sub(1);

        if self.slide_animating {
            let current = self.slide_spring.value();
            let target = self.slide_spring.target();
            let dw_mpt = pt(display_width as i32);
            let lo = (current.min(target) / dw_mpt) as usize;
            let hi_val = current.max(target);
            let hi = if hi_val % dw_mpt != 0 {
                (hi_val / dw_mpt) as usize + 1
            } else {
                (hi_val / dw_mpt) as usize
            };

            (lo.min(last), hi.min(last))
        } else {
            (self.active, self.active)
        }
    }

    pub fn rebuild(&mut self, ctx: &WorkspaceRebuildContext) {
        let mut tree = ViewTree::new();
        let bg = Color::rgb(
            presenter_service::BG_R,
            presenter_service::BG_G,
            presenter_service::BG_B,
        );
        let title_color = Color::rgb(
            presenter_service::CHROME_TITLE_R,
            presenter_service::CHROME_TITLE_G,
            presenter_service::CHROME_TITLE_B,
        );
        let clock_color = Color::rgb(
            presenter_service::CHROME_CLOCK_R,
            presenter_service::CHROME_CLOCK_G,
            presenter_service::CHROME_CLOCK_B,
        );
        let title_bar_h = presenter_service::TITLE_BAR_H;
        let content_h = ctx.display_height.saturating_sub(title_bar_h);
        let page_margin = presenter_service::PAGE_MARGIN_V;
        let root = tree.add(ViewNode {
            display: view_tree::Display::FixedCanvas,
            width: view_tree::Dimension::Points(upt(ctx.display_width)),
            height: view_tree::Dimension::Points(upt(ctx.display_height)),
            background: bg,
            ..Default::default()
        });
        // Document icon.
        let icon_size_pt = presenter_service::LINE_HEIGHT + 2;
        let icon = icons::get("document", ctx.active_mimetype);
        let icon_paths = scale_icon_view_paths(icon, icon_size_pt);
        let icon_hash = scene::fnv1a(&icon_paths);
        let icon_sw = icon_stroke_width_umpt(
            icon.stroke_width.to_bits(),
            icon_size_pt,
            icon.viewbox.to_bits(),
        );
        let icon_x: i32 = 8;
        let icon_y = ((title_bar_h.saturating_sub(icon_size_pt)) / 2).saturating_sub(1) as i32;
        let icon_node = tree.add(ViewNode {
            offset_x: pt(icon_x),
            offset_y: pt(icon_y),
            intrinsic: IntrinsicSize::Fixed {
                width: upt(icon_size_pt),
                height: upt(icon_size_pt),
            },
            content: ViewContent::Path {
                commands: icon_paths,
                color: Color::TRANSPARENT,
                stroke_color: title_color,
                fill_rule: scene::FillRule::Winding,
                stroke_width: icon_sw,
                content_hash: icon_hash,
            },
            ..Default::default()
        });

        tree.append_child(root, icon_node);

        // Title text.
        let title_text_y = (title_bar_h.saturating_sub(presenter_service::LINE_HEIGHT)) / 2;
        let (title_count, title_width) = super::shape_text(
            super::font(init::FONT_IDX_SANS),
            "untitled",
            presenter_service::FONT_SIZE,
            &[],
            &mut self.glyphs,
        );
        let title_node = tree.add(ViewNode {
            offset_x: pt(36),
            offset_y: pt(title_text_y as i32),
            intrinsic: IntrinsicSize::Fixed {
                width: (title_width as u32).saturating_add(upt(1)),
                height: upt(presenter_service::LINE_HEIGHT),
            },
            content: ViewContent::Glyphs {
                glyphs: self.glyphs[..title_count].to_vec(),
                color: title_color,
                font_size: presenter_service::FONT_SIZE,
                style_id: super::STYLE_SANS,
            },
            role: scene::ROLE_LABEL,
            ..Default::default()
        });

        tree.append_child(root, title_node);

        // Clock text.
        let clock_secs = ctx.rtc_secs;
        let hours = (clock_secs / 3600) % 24;
        let minutes = (clock_secs / 60) % 60;
        let seconds = clock_secs % 60;
        let clock_chars: [u8; 8] = [
            b'0' + (hours / 10) as u8,
            b'0' + (hours % 10) as u8,
            b':',
            b'0' + (minutes / 10) as u8,
            b'0' + (minutes % 10) as u8,
            b':',
            b'0' + (seconds / 10) as u8,
            b'0' + (seconds % 10) as u8,
        ];
        let clock_text = core::str::from_utf8(&clock_chars).unwrap_or("00:00:00");
        let tnum = fonts::Feature::new(fonts::Tag::new(b"tnum"), 1, ..);
        let (clock_count, clock_width) = super::shape_text(
            super::font(init::FONT_IDX_SANS),
            clock_text,
            presenter_service::FONT_SIZE,
            &[tnum],
            &mut self.glyphs,
        );
        let clock_text_w_mpt = (clock_width as u32).saturating_add(upt(1));
        let clock_x =
            ctx.display_width as i32 - 12 - (clock_text_w_mpt / scene::MPT_PER_PT as u32) as i32;
        let clock_node = tree.add(ViewNode {
            offset_x: pt(clock_x),
            offset_y: pt(title_text_y as i32),
            intrinsic: IntrinsicSize::Fixed {
                width: clock_text_w_mpt,
                height: upt(presenter_service::LINE_HEIGHT),
            },
            content: ViewContent::Glyphs {
                glyphs: self.glyphs[..clock_count].to_vec(),
                color: clock_color,
                font_size: presenter_service::FONT_SIZE,
                style_id: super::STYLE_SANS,
            },
            role: scene::ROLE_LABEL,
            ..Default::default()
        });

        tree.append_child(root, clock_node);

        // Content area.
        let content_area = tree.add(ViewNode {
            display: view_tree::Display::FixedCanvas,
            offset_y: pt(title_bar_h as i32),
            width: view_tree::Dimension::Points(upt(ctx.display_width)),
            height: view_tree::Dimension::Points(upt(content_h)),
            clips_children: true,
            ..Default::default()
        });

        tree.append_child(root, content_area);

        // Strip — holds all document spaces side by side.
        let strip = tree.add(ViewNode {
            display: view_tree::Display::FixedCanvas,
            width: view_tree::Dimension::Points(upt(
                ctx.display_width * self.children.len().max(1) as u32
            )),
            height: view_tree::Dimension::Points(upt(content_h)),
            child_offset_x: -self.slide_spring.value(),
            ..Default::default()
        });

        tree.append_child(content_area, strip);

        // Portal nodes for each child viewer, centered in their strip slot.
        let (vis_lo, vis_hi) = self.visible_range(ctx.display_width);

        for (i, child) in self.children.iter().enumerate() {
            if i < vis_lo || i > vis_hi {
                continue;
            }

            let base_x = (ctx.display_width * i as u32) as i32;
            let child_subtree = match &child.viewer {
                ViewerKind::Image(v) => v.subtree(),
                ViewerKind::Text(v) => v.subtree(),
                ViewerKind::Video(v) => v.subtree(),
            };

            if child_subtree.root == scene::NULL {
                continue;
            }

            let child_root_box = if (child_subtree.root as usize) < child_subtree.layout.len() {
                &child_subtree.layout[child_subtree.root as usize]
            } else {
                &LayoutBox::EMPTY
            };
            let child_w_pt = child_root_box.width / scene::MPT_PER_PT as u32;
            let child_h_pt = child_root_box.height / scene::MPT_PER_PT as u32;
            let center_x = base_x + (ctx.display_width as i32 - child_w_pt as i32) / 2;
            let center_y = (content_h as i32 - child_h_pt as i32) / 2;
            let portal = tree.add(ViewNode {
                offset_x: pt(center_x),
                offset_y: pt(center_y.max(page_margin as i32)),
                intrinsic: IntrinsicSize::Fixed {
                    width: child_root_box.width,
                    height: child_root_box.height,
                },
                content: ViewContent::Portal {
                    child_idx: i as u16,
                },
                ..Default::default()
            });

            tree.append_child(strip, portal);
        }

        let layout = view_tree::layout(
            tree.nodes(),
            root,
            upt(ctx.display_width),
            upt(ctx.display_height),
            &NoMeasurer,
        );

        self.subtree = ViewSubtree { tree, root, layout };
    }
}

impl Viewer for WorkspaceViewer {
    fn subtree(&self) -> &ViewSubtree {
        &self.subtree
    }

    fn event(&mut self, _event: &InputEvent) -> EventResponse {
        EventResponse::Unhandled
    }

    fn teardown(&mut self) {}
}

// ── Viewer registry ─────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerKindTag {
    Image,
    Text,
    Video,
    Workspace,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum MimetypePattern {
    Exact(&'static [u8]),
    Subtype(&'static [u8]),
    Universal,
}

#[allow(dead_code)]
impl MimetypePattern {
    fn specificity(&self) -> u8 {
        match self {
            Self::Exact(_) => 0,
            Self::Subtype(_) => 1,
            Self::Universal => 2,
        }
    }

    fn matches(&self, mimetype: &[u8]) -> bool {
        match self {
            Self::Exact(pat) => mimetype == *pat,
            Self::Subtype(prefix) => {
                if let Some(slash) = mimetype.iter().position(|&b| b == b'/') {
                    &mimetype[..slash] == *prefix
                } else {
                    false
                }
            }
            Self::Universal => true,
        }
    }
}

#[allow(dead_code)]
struct ViewerEntry {
    pattern: MimetypePattern,
    kind: ViewerKindTag,
    priority: u8,
}

#[allow(dead_code)]
pub struct ViewerRegistry {
    entries: alloc::vec::Vec<ViewerEntry>,
}

#[allow(dead_code)]
impl ViewerRegistry {
    pub fn new() -> Self {
        let mut entries = alloc::vec::Vec::new();

        for &(pat, kind) in ImageViewer::SUPPORTED_TYPES {
            entries.push(ViewerEntry {
                pattern: pat,
                kind,
                priority: pat.specificity(),
            });
        }

        for &(pat, kind) in TextViewer::SUPPORTED_TYPES {
            entries.push(ViewerEntry {
                pattern: pat,
                kind,
                priority: pat.specificity(),
            });
        }

        for &(pat, kind) in VideoViewer::SUPPORTED_TYPES {
            entries.push(ViewerEntry {
                pattern: pat,
                kind,
                priority: pat.specificity(),
            });
        }

        entries.sort_by_key(|e| e.priority);

        Self { entries }
    }

    pub fn lookup(&self, mimetype: &[u8]) -> Option<ViewerKindTag> {
        for entry in &self.entries {
            if entry.pattern.matches(mimetype) {
                return Some(entry.kind);
            }
        }

        None
    }

    pub fn alternatives(&self, mimetype: &[u8]) -> alloc::vec::Vec<ViewerKindTag> {
        self.entries
            .iter()
            .filter(|e| e.pattern.matches(mimetype))
            .map(|e| e.kind)
            .collect()
    }
}

#[allow(dead_code)]
impl ImageViewer {
    const SUPPORTED_TYPES: &[(MimetypePattern, ViewerKindTag)] = &[
        (MimetypePattern::Exact(b"image/jpeg"), ViewerKindTag::Image),
        (MimetypePattern::Exact(b"image/png"), ViewerKindTag::Image),
    ];
}

#[allow(dead_code)]
impl TextViewer {
    const SUPPORTED_TYPES: &[(MimetypePattern, ViewerKindTag)] = &[
        (MimetypePattern::Exact(b"text/rich"), ViewerKindTag::Text),
        (MimetypePattern::Subtype(b"text"), ViewerKindTag::Text),
    ];
}

#[allow(dead_code)]
impl VideoViewer {
    const SUPPORTED_TYPES: &[(MimetypePattern, ViewerKindTag)] = &[
        (MimetypePattern::Exact(b"video/mp4"), ViewerKindTag::Video),
        (MimetypePattern::Exact(b"video/avi"), ViewerKindTag::Video),
    ];
}

// ── Shared helpers ──────────────────────────────────────────────────

pub fn aspect_fit(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (0, 0);
    }

    let w_scaled_h = max_w as u64 * src_h as u64;
    let h_scaled_w = max_h as u64 * src_w as u64;

    if w_scaled_h <= h_scaled_w {
        let h = (max_w as u64 * src_h as u64 / src_w as u64) as u32;

        (max_w.min(src_w), h.min(src_h))
    } else {
        let w = (max_h as u64 * src_w as u64 / src_h as u64) as u32;

        (w.min(src_w), max_h.min(src_h))
    }
}

fn scale_icon_view_paths(icon: &icons::Icon, size_pt: u32) -> alloc::vec::Vec<u8> {
    let viewbox_bits = icon.viewbox.to_bits();
    let mut buf = alloc::vec::Vec::new();

    for icon_path in icon.paths {
        let cmds = icon_path.commands;
        let mut pos = 0;

        while pos + 4 <= cmds.len() {
            let tag = u32::from_le_bytes([cmds[pos], cmds[pos + 1], cmds[pos + 2], cmds[pos + 3]]);

            match tag {
                scene::PATH_MOVE_TO | scene::PATH_LINE_TO => {
                    if pos + scene::PATH_MOVE_TO_SIZE > cmds.len() {
                        break;
                    }

                    buf.extend_from_slice(&tag.to_le_bytes());
                    buf.extend_from_slice(
                        &scale_f32_bits(read_u32_le(cmds, pos + 4), size_pt, viewbox_bits)
                            .to_le_bytes(),
                    );
                    buf.extend_from_slice(
                        &scale_f32_bits(read_u32_le(cmds, pos + 8), size_pt, viewbox_bits)
                            .to_le_bytes(),
                    );

                    pos += scene::PATH_MOVE_TO_SIZE;
                }
                scene::PATH_CUBIC_TO => {
                    if pos + scene::PATH_CUBIC_TO_SIZE > cmds.len() {
                        break;
                    }

                    buf.extend_from_slice(&scene::PATH_CUBIC_TO.to_le_bytes());

                    for ci in 0..6 {
                        let off = pos + 4 + ci * 4;

                        buf.extend_from_slice(
                            &scale_f32_bits(read_u32_le(cmds, off), size_pt, viewbox_bits)
                                .to_le_bytes(),
                        );
                    }

                    pos += scene::PATH_CUBIC_TO_SIZE;
                }
                scene::PATH_CLOSE => {
                    buf.extend_from_slice(&scene::PATH_CLOSE.to_le_bytes());

                    pos += scene::PATH_CLOSE_SIZE;
                }
                _ => break,
            }
        }
    }

    buf
}

fn icon_stroke_width_umpt(stroke_bits: u32, size_pt: u32, viewbox_bits: u32) -> u16 {
    let scaled = scale_f32_bits(stroke_bits, size_pt * 256, viewbox_bits);
    let exp = ((scaled >> 23) & 0xFF) as i32;
    let frac = (scaled & 0x7F_FFFF) as u64 | 0x80_0000;
    let shift = exp - 150;

    if exp == 0 {
        return 0;
    }

    let val = if shift >= 0 {
        frac << shift.min(16)
    } else {
        frac >> (-shift).min(24)
    };

    val.min(u16::MAX as u64) as u16
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn scale_f32_bits(bits: u32, num: u32, den_bits: u32) -> u32 {
    if bits & 0x7FFF_FFFF == 0 {
        return bits;
    }

    let sign = bits & 0x8000_0000;
    let exp = (bits >> 23) & 0xFF;
    let frac = (bits & 0x7F_FFFF) as u64 | 0x80_0000;

    if exp == 0 || exp == 255 {
        return bits;
    }

    let den_exp = (den_bits >> 23) & 0xFF;
    let den_frac = (den_bits & 0x7F_FFFF) as u64 | 0x80_0000;

    if den_exp == 0 {
        return bits;
    }

    let product = frac * num as u64;
    let scaled = (product << 23) / den_frac;

    if scaled == 0 {
        return 0;
    }

    let leading = 63 - scaled.leading_zeros();
    let new_exp = exp as i32 - den_exp as i32 + leading as i32 + 104;

    if new_exp <= 0 {
        return 0;
    }
    if new_exp >= 255 {
        return sign | 0x7F80_0000;
    }

    let new_frac = if leading > 23 {
        (scaled >> (leading - 23)) as u32 & 0x7F_FFFF
    } else {
        ((scaled << (23 - leading)) as u32) & 0x7F_FFFF
    };

    sign | ((new_exp as u32) << 23) | new_frac
}

// ── No-op measurer for viewers that don't need content measurement ──

struct NoMeasurer;

impl view_tree::ContentMeasurer for NoMeasurer {
    fn measure(&self, _: scene::NodeId, _: scene::Umpt) -> (scene::Umpt, scene::Umpt) {
        (0, 0)
    }
}

// ── View node → scene node conversion ───────────────────────────────

pub fn write_view_node(
    node: &ViewNode,
    scene: &mut SceneWriter,
    parent: scene::NodeId,
    x: scene::Mpt,
    y: scene::Mpt,
    width: scene::Umpt,
    height: scene::Umpt,
) {
    if matches!(node.content, ViewContent::Portal { .. }) {
        return;
    }

    let scene_id = match scene.alloc_node() {
        Some(id) => id,
        None => return,
    };
    let n = scene.node_mut(scene_id);

    n.x = x;
    n.y = y;
    n.width = width;
    n.height = height;
    n.background = node.background;
    n.opacity = node.opacity;
    n.corner_radius = (node.corner_radius / scene::MPT_PER_PT as u32).min(255) as u8;
    n.shadow_color = node.shadow_color;
    n.shadow_blur_radius = (node.shadow_blur_radius / scene::MPT_PER_PT as u32).min(255) as u8;
    n.shadow_spread = (node.shadow_spread / scene::MPT_PER_PT).clamp(-128, 127) as i8;
    n.shadow_offset_x = (node.shadow_offset_x / scene::MPT_PER_PT).clamp(-32768, 32767) as i16;
    n.shadow_offset_y = (node.shadow_offset_y / scene::MPT_PER_PT).clamp(-32768, 32767) as i16;
    n.role = node.role;
    n.level = node.level;
    n.cursor_shape = node.cursor_shape;

    if node.clips_children {
        n.flags = scene::NodeFlags::VISIBLE.union(scene::NodeFlags::CLIPS_CHILDREN);
    }
    if !node.transform.is_identity() {
        n.transform = node.transform;
    }

    match &node.content {
        ViewContent::Image {
            content_id,
            src_width,
            src_height,
        } => {
            n.content = Content::Image {
                content_id: *content_id,
                src_width: *src_width,
                src_height: *src_height,
            };
        }
        ViewContent::Glyphs {
            glyphs,
            color,
            font_size,
            style_id,
        } => {
            let glyph_ref = scene.push_shaped_glyphs(glyphs);
            let n = scene.node_mut(scene_id);

            n.content = Content::Glyphs {
                color: *color,
                glyphs: glyph_ref,
                glyph_count: glyphs.len() as u16,
                font_size: *font_size,
                style_id: *style_id,
            };
        }
        ViewContent::Path {
            commands,
            color,
            stroke_color,
            fill_rule,
            stroke_width,
            content_hash,
        } => {
            let path_ref = scene.push_path_commands(commands);
            let n = scene.node_mut(scene_id);

            n.content = Content::Path {
                color: *color,
                stroke_color: *stroke_color,
                fill_rule: *fill_rule,
                stroke_width: *stroke_width,
                contours: path_ref,
            };
            n.content_hash = *content_hash;
        }
        ViewContent::Gradient {
            color_start,
            color_end,
            kind,
            angle_fp,
        } => {
            n.content = Content::Gradient {
                color_start: *color_start,
                color_end: *color_end,
                kind: *kind,
                _pad: 0,
                angle_fp: *angle_fp,
            };
        }
        ViewContent::GradientPath {
            color_start,
            color_end,
            kind,
            angle_fp,
            commands,
        } => {
            let path_ref = scene.push_path_commands(commands);
            let n = scene.node_mut(scene_id);

            n.content = Content::GradientPath {
                color_start: *color_start,
                color_end: *color_end,
                kind: *kind,
                _pad: 0,
                angle_fp: *angle_fp,
                contours: path_ref,
            };
        }
        ViewContent::None | ViewContent::Portal { .. } => {}
    }

    scene.add_child(parent, scene_id);
}

// ── write_subtree — recursive ViewTree → scene graph writer ─────────

pub fn write_subtree(
    subtree: &ViewSubtree,
    children: &[&ViewSubtree],
    scene: &mut SceneWriter,
    parent: scene::NodeId,
    offset_x: scene::Mpt,
    offset_y: scene::Mpt,
) {
    if subtree.root == scene::NULL {
        return;
    }

    write_subtree_node(
        subtree,
        children,
        subtree.root,
        scene,
        parent,
        offset_x,
        offset_y,
    );
}

pub fn write_subtree_as_root(
    subtree: &ViewSubtree,
    children: &[&ViewSubtree],
    scene: &mut SceneWriter,
    offset_x: scene::Mpt,
    offset_y: scene::Mpt,
) {
    if subtree.root == scene::NULL {
        return;
    }

    let node = subtree.tree.get(subtree.root);
    let lb = if (subtree.root as usize) < subtree.layout.len() {
        &subtree.layout[subtree.root as usize]
    } else {
        &LayoutBox::EMPTY
    };
    let x = offset_x + lb.x;
    let y = offset_y + lb.y;
    let scene_id = match scene.alloc_node() {
        Some(id) => id,
        None => return,
    };

    {
        let n = scene.node_mut(scene_id);

        n.x = x;
        n.y = y;
        n.width = lb.width;
        n.height = lb.height;
        n.background = node.background;
        n.opacity = node.opacity;
        n.corner_radius = (node.corner_radius / scene::MPT_PER_PT as u32).min(255) as u8;
        n.role = node.role;
        n.level = node.level;

        if node.clips_children {
            n.flags = scene::NodeFlags::VISIBLE.union(scene::NodeFlags::CLIPS_CHILDREN);
        }
    }

    scene.set_root(scene_id);

    let pad_x = lb.padding.left + lb.border.left;
    let pad_y = lb.padding.top + lb.border.top;
    let mut child = node.first_child;

    while child != scene::NULL {
        write_subtree_node(subtree, children, child, scene, scene_id, pad_x, pad_y);

        child = subtree.tree.get(child).next_sibling;
    }
}

fn write_subtree_node(
    subtree: &ViewSubtree,
    children: &[&ViewSubtree],
    node_id: scene::NodeId,
    scene: &mut SceneWriter,
    parent: scene::NodeId,
    offset_x: scene::Mpt,
    offset_y: scene::Mpt,
) {
    let node = subtree.tree.get(node_id);
    let lb = if (node_id as usize) < subtree.layout.len() {
        &subtree.layout[node_id as usize]
    } else {
        &LayoutBox::EMPTY
    };
    let x = offset_x + lb.x;
    let y = offset_y + lb.y;

    if let ViewContent::Portal { child_idx } = &node.content {
        let idx = *child_idx as usize;

        if idx < children.len() {
            write_subtree(children[idx], &[], scene, parent, x, y);
        }

        return;
    }

    write_view_node(node, scene, parent, x, y, lb.width, lb.height);

    let scene_id = scene.node_count().saturating_sub(1) as scene::NodeId;

    if node.child_offset_x != 0 || node.child_offset_y != 0 {
        let child_ox = node.child_offset_x;
        let child_oy = node.child_offset_y;
        let pad_x = lb.padding.left + lb.border.left;
        let pad_y = lb.padding.top + lb.border.top;
        let mut child = node.first_child;

        while child != scene::NULL {
            write_subtree_node(
                subtree,
                children,
                child,
                scene,
                scene_id,
                child_ox + pad_x,
                child_oy + pad_y,
            );

            child = subtree.tree.get(child).next_sibling;
        }
    } else {
        let pad_x = lb.padding.left + lb.border.left;
        let pad_y = lb.padding.top + lb.border.top;
        let mut child = node.first_child;

        while child != scene::NULL {
            write_subtree_node(subtree, children, child, scene, scene_id, pad_x, pad_y);

            child = subtree.tree.get(child).next_sibling;
        }
    }
}
