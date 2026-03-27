# layout

Unified text layout engine. One function handles both monospace and proportional text -- the difference is captured by the `FontMetrics` trait (constant vs per-character advance widths). No dependencies on scene graph, rendering, or core. `no_std` with `alloc`.

## Key Files

- `lib.rs` -- `FontMetrics` trait (char_width, line_height), `LineBreaker` trait (can_break_before, trim_whitespace), `CharBreaker` (character-level wrapping), `WordBreaker` (word-level wrapping with whitespace trimming), `Alignment` enum, layout output types

## Dependencies

- None

## Conventions

- All dimensions are in points (f32); callers convert to pixels at the render boundary
- Line breaking is pluggable via the `LineBreaker` trait
- `CharBreaker` produces identical results to fixed-column character wrapping
- `WordBreaker` breaks after spaces, tabs, and hyphens; trims whitespace at soft breaks
