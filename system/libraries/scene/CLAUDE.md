# scene

Scene graph data structures for the compositor interface. Core builds a tree of `Node` values in shared memory; render services read and draw it to pixels. Includes triple-buffered shared memory transport, SVG path parsing, and stroke expansion. `no_std` with `alloc`.

## Key Files

- `lib.rs` -- Re-exports all public types from submodules
- `node.rs` -- `Node` type (136 bytes, one type with optional content), `NodeId` (u16), `SceneHeader`, millipoint coordinate types (`Mpt`/`Umpt`, 1/1024 pt), memory layout constants (`MAX_NODES` = 512, `DATA_BUFFER_SIZE` = 64 KiB, `SCENE_SIZE`), cursor shape constants (`CURSOR_INHERIT`/`CURSOR_POINTER`/`CURSOR_TEXT`)
- `primitives.rs` -- `Color`, `Border`, `Content` enum (None, InlineImage, Image, Path, Glyphs), `DataRef`, `ShapedGlyph` (16 bytes with 16.16 fixed-point advances), `FillRule`, path command encoding/decoding, `path_winding_number()` (point-in-path via winding rule with cubic Bézier subdivision), `bitflags` macro, content hashing
- `writer.rs` -- `SceneWriter`: API for building scene graphs (add nodes, set properties, write data buffer)
- `reader.rs` -- `SceneReader`: read-only accessor for scene graph shared memory
- `triple.rs` -- `TripleScene`: lock-free mailbox with 3 buffers (writer never blocks, reader gets latest). `TRIPLE_SCENE_SIZE` = 3 scenes + 16-byte control region
- `diff.rs` -- `build_parent_map`, `abs_bounds` for computing absolute node positions
- `transform.rs` -- `AffineTransform` (6-element 2D affine matrix)
- `stroke.rs` -- Stroke expansion: convert stroked paths to filled outlines with round joins/caps (cubic Bezier arc approximation)
- `svg_path.rs` -- SVG path `d` attribute parser: M/L/H/V/C/S/Q/A/Z commands to native path format. Arc-to-cubic conversion. Subset parser for icon sets

## Dependencies

- None

## Conventions

- Millipoint coordinates (`Mpt` = 1/1024 pt) are the internal spatial unit. i32 range covers +/-2,097,151 pt
- One node type with optional content (Core Animation model) -- avoids wrapper nodes
- Nodes have: position, size, background, border, corner radius, opacity, clip, content, transform, cursor_shape (for hit-testing), children (first_child/next_sibling linked list)
- `Content::Image` references Content Region via `content_id`; `Content::InlineImage` carries per-frame pixel data
- Triple buffer uses atomic u32 indices with acquire/release ordering
- Dirty bitmap in scene header marks changed nodes for incremental rendering
