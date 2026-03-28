# core

Central OS service: sole owner of document state, text layout, scene graph building, input routing, and editor communication. Reads input events, performs navigation/selection, delegates editing to the text editor, and publishes scene graphs to shared memory for the render service.

## Key Files

- `main.rs` — Entry point, event loop, CoreState struct, undo/redo state machine, IPC dispatch
- `documents.rs` — Document buffer operations (insert, delete, delete_range) over shared memory
- `input.rs` — Keyboard dispatch, cursor navigation (word/line/page), selection management, editor forwarding
- `blink.rs` — Four-phase cursor blink state machine (visible hold, fade out, hidden hold, fade in)
- `icons.rs` — Tabler icon rasterization (SVG path to BGRA pixels) and pointer cursor rendering
- `typography.rs` — Content-type-aware typography defaults (font family, OpenType features, weight)
- `fallback.rs` — Font fallback chain: tries fonts in order until a valid glyph is found
- `scene_state.rs` — Triple-buffered scene graph wrapper (acquire/publish lifecycle, incremental updates)
- `layout/` — Scene graph building and text layout (see below)

## layout/

- `mod.rs` — Well-known node indices (N_ROOT through N_DOC_IMAGE), SceneConfig, layout helpers
- `full.rs` — Full scene builds from scratch and compaction rebuilds of document content
- `incremental.rs` — Incremental updates: single-line edit, line insert (Enter), line delete (Backspace at BOL)
- `loading.rs` — Boot loading scene: Tabler loader-2 spinner (270° arc), CPU-rasterized as InlineImage each frame

## IPC Protocol

**Receives:**

- `MSG_KEY_EVENT` — Keyboard events from input driver (handle 1)
- `MSG_POINTER_BUTTON` — Mouse button events from input driver (handle 1)
- `MSG_WRITE_INSERT`, `MSG_WRITE_DELETE`, `MSG_WRITE_DELETE_RANGE` — Edit operations from text editor (handle 3)
- `MSG_CURSOR_MOVE`, `MSG_SELECTION_UPDATE` — Cursor sync from text editor (handle 3)
- `MSG_DOC_QUERY_RESULT`, `MSG_DOC_READ_DONE`, `MSG_DOC_SNAPSHOT_RESULT`, `MSG_DOC_RESTORE_RESULT`, `MSG_DOC_CREATE_RESULT` — Replies from document service (handle 5)
- `MSG_CORE_CONFIG`, `MSG_FRAME_RATE` — Configuration from init (handle 0)
- `MSG_IMAGE_CONFIG`, `MSG_RTC_CONFIG` — Image and RTC config from init

**Sends:**

- `MSG_SCENE_UPDATED` — Scene graph published signal to render service (handle 2)
- `MSG_KEY_EVENT`, `MSG_SET_CURSOR` — Input forwarding to text editor (handle 3)
- `MSG_DOC_COMMIT`, `MSG_DOC_QUERY`, `MSG_DOC_READ`, `MSG_DOC_SNAPSHOT`, `MSG_DOC_RESTORE`, `MSG_DOC_CREATE`, `MSG_DOC_DELETE_SNAPSHOT` — Document operations to document service (handle 5)
- Pointer position via shared atomic state register (not IPC ring)

## Dependencies

- `sys` — Syscalls, memory allocation
- `ipc` — Channel communication
- `protocol` — Wire format (compose, core_config, document, edit, input)
- `animation` — Timeline for cursor blink fades
- `drawing` — Math helpers, surface types
- `fonts` — Font shaping and rasterization
- `layout` — Text layout (line breaking, word boundaries, FontMetrics)
- `render` — Path rasterizer for icon rendering
- `scene` — Scene graph types, triple buffer, SVG path parser, stroke expansion
