//! Incremental scene graph updates — stub.
//!
//! With layout computation moved to the layout engine (B), the view engine
//! no longer performs local line-breaking. These functions always return
//! `false`, causing the caller to fall through to the full compaction
//! rebuild path which reads B's pre-computed layout results.

use super::SceneConfig;

/// Always returns `false` — fall through to compaction rebuild.
#[allow(clippy::too_many_arguments)]
pub fn update_single_line(
    _w: &mut scene::SceneWriter<'_>,
    _cfg: &SceneConfig,
    _doc_text: &[u8],
    _changed_line: usize,
    _cursor_pos: u32,
    _sel_start: u32,
    _sel_end: u32,
    _scroll_y: scene::Mpt,
    _clock_text: Option<&[u8]>,
    _cursor_opacity: u8,
) -> bool {
    false
}

/// Always returns `false` — fall through to compaction rebuild.
#[allow(clippy::too_many_arguments)]
pub fn insert_line(
    _w: &mut scene::SceneWriter<'_>,
    _cfg: &SceneConfig,
    _doc_text: &[u8],
    _cursor_pos: u32,
    _sel_start: u32,
    _sel_end: u32,
    _scroll_y: scene::Mpt,
    _clock_text: Option<&[u8]>,
    _cursor_opacity: u8,
) -> bool {
    false
}

/// Always returns `false` — fall through to compaction rebuild.
#[allow(clippy::too_many_arguments)]
pub fn delete_line(
    _w: &mut scene::SceneWriter<'_>,
    _cfg: &SceneConfig,
    _doc_text: &[u8],
    _cursor_pos: u32,
    _sel_start: u32,
    _sel_end: u32,
    _scroll_y: scene::Mpt,
    _clock_text: Option<&[u8]>,
    _cursor_opacity: u8,
) -> bool {
    false
}
