# view-engine

View Engine (C) — event loop, input routing, and scene graph building. Owns all view state (cursor, selection, scroll, focus, animation). Reads document buffer (RO) from A (document-model) and layout results (RO) from B (layout-engine). Sole writer to the scene graph. Routes input to editors.

## Key Files

- `main.rs` — Entry point, event loop, ViewState struct, IPC dispatch, `resolve_cursor_shape()` (scene graph hit-testing with winding number + cursor shape inheritance)
- `documents.rs` — Read-only document buffer access (doc_content, rich_buf_ref) and header sync
- `input.rs` — Keyboard dispatch, cursor navigation (word/line/page), selection management, editor forwarding
- `blink.rs` — Four-phase cursor blink state machine (visible hold, fade out, hidden hold, fade in)
- `scene_state.rs` — Triple-buffered scene graph wrapper (acquire/publish lifecycle), `latest_nodes()`/`latest_data_buf()` for hit-testing read-back
- `layout/` — Scene graph building (see below)

## layout/

- `mod.rs` — Well-known node indices (N_ROOT through N_DOC_IMAGE), SceneConfig, layout helpers
- `full.rs` — Full scene builds from scratch and compaction rebuilds of document content; reads pre-computed layout from B's shared memory
- `incremental.rs` — Stub: always falls through to compaction (B computes layout)
- `loading.rs` — Boot loading scene: Tabler loader-2 spinner (270° arc), CPU-rasterized as InlineImage each frame

## IPC Protocol

**Receives:**

- `MSG_KEY_EVENT` — Keyboard events from input driver (handle 1)
- `MSG_POINTER_BUTTON` — Mouse button events from input driver (handle 1)
- `MSG_CURSOR_MOVE`, `MSG_SELECTION_UPDATE` — Cursor sync from editor (handle 3)
- `MSG_DOC_CHANGED`, `MSG_DOC_LOADED`, `MSG_IMAGE_DECODED` — Notifications from A (handle 4)
- `MSG_UNDO_REQUEST`, `MSG_REDO_REQUEST` — Undo/redo from A (handle 4)
- `MSG_LAYOUT_READY` — Layout results available from B (handle 5)
- `MSG_CORE_CONFIG`, `MSG_FRAME_RATE`, `MSG_CORE_LAYOUT_CONFIG` — Configuration from init (handle 0)
- `MSG_RTC_CONFIG` — RTC config from init

**Sends:**

- `MSG_SCENE_UPDATED` — Scene graph published signal to render service (handle 2)
- `MSG_KEY_EVENT`, `MSG_SET_CURSOR` — Input forwarding to editor (handle 3)
- `MSG_LAYOUT_RECOMPUTE` — Request layout recompute from B (handle 5)
- Pointer position via shared atomic state register (not IPC ring)
- Cursor shape via shared CursorState page

## Dependencies

- `sys` — Syscalls, memory allocation
- `ipc` — Channel communication
- `protocol` — Wire format (core_config, document_model, edit, input, layout, cursor)
- `animation` — Timeline for cursor blink fades, scroll/slide springs
- `drawing` — Math helpers, surface types
- `fonts` — Font shaping and rasterization (for chrome text)
- `layout` — Text layout (line breaking, word boundaries, FontMetrics)
- `piecetable` — Rich text piece table (read access for navigation)
- `render` — Path rasterizer for icon rendering
- `scene` — Scene graph types, triple buffer, SVG path parser
- `icons` — Pre-compiled icon path data
