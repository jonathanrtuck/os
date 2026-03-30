# layout

Pure function: (document content + viewport + focus) -> positioned elements.
Reads the document buffer (RO) and Content Region (fonts, RO), receives
viewport parameters from presenter, and writes layout results to shared
memory.

## Responsibilities

- Line-breaking and word-wrapping
- Glyph shaping and font fallback
- Computing positioned runs (glyph x/y positions)
- Computing content dimensions and line map
- Writing layout results to shared memory for presenter to read

## Boundary: What layout Does NOT Do

- No cursors, selection, or caret positioning
- No animation or scroll state
- No input handling
- No scene graph knowledge
- No editing or document mutation

## Key Files

- `main.rs` -- entry point, IPC loop, layout computation, shared memory output

## IPC Protocol

### Receives

- `MSG_LAYOUT_ENGINE_CONFIG` -- Init config (doc buffer VA, Content Region, layout results VA)
- `MSG_LAYOUT_RECOMPUTE` -- Recompute signal from presenter (viewport changed or document changed)

### Sends

- `MSG_LAYOUT_READY` -- Layout results written, presenter can read them

## Shared Memory

- **Reads:** Document buffer (RO), Content Region (fonts, RO), Viewport state register (RO)
- **Writes:** Layout results buffer (positioned runs, line map, content dimensions)

## Dependencies

Libraries: sys, ipc, protocol, fonts, layout
