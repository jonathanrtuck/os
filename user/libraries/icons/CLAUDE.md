# Icons Library

Vector icon library pre-compiled from [Tabler Icons](https://tabler.io/icons)
(MIT license). Icons are stored as binary path commands in `src/data.rs` — no
SVG parsing at runtime.

## Adding a New Icon

1. Find the icon on [tabler.io/icons](https://tabler.io/icons) (outline style,
   24x24 viewbox, stroke-width 2).

2. Get the SVG source. Each `<path d="..."/>` (ignoring the `fill="none"`
   background rect) becomes one sub-path.

3. Convert SVG path commands to the binary encoding:

   | Tag (u32 LE) | Command | Payload              | Total bytes |
   | :----------: | ------- | -------------------- | :---------: |
   |    `0x00`    | MoveTo  | x, y (f32 LE each)   |     12      |
   |    `0x01`    | LineTo  | x, y                 |     12      |
   |    `0x02`    | CubicTo | x1, y1, x2, y2, x, y |     28      |
   |    `0x03`    | Close   | (none)               |      4      |
   - All coordinates are **absolute** (convert relative SVG commands).
   - SVG arcs (`a`/`A`) must be decomposed into cubic beziers.
   - Paths that form filled shapes **must** end with Close (`0x03`).

4. Add the static byte arrays and `Icon` struct in `src/data.rs`, following the
   existing pattern. Add an entry to the `lookup()` match table.

5. If the icon has mixed closed and open sub-paths (e.g., a filled arrow body
   with open stroke lines for a badge), the cursor renderer handles this
   automatically — closed paths get fill+stroke, open paths get stroke-only,
   results are composited.

## Icon Naming

Icon names are **semantic OS concepts** (`"pointer"`, `"cursor-text"`,
`"document"`), not Tabler source names. The mapping is implicit in `data.rs`.

## Layer System

Each sub-path has a `Layer` tag (`Primary` or `Secondary`). Secondary paths
render at reduced opacity. Currently all cursor/UI icons use `Primary` only.
