# Rendering Pipeline Completion — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete rendering pipeline B (features), C (robustness), and D (code organization) items from `design/rendering-pipeline-completion.md`. Section A (performance/incremental rendering) is explicitly deferred.

**Architecture:** Four phases in dependency order: (1) drawing library fixes that don't overlap with files being split, (2) split 4 large files into well-scoped modules, (3) remaining feature fixes in the now-clean modules, (4) system robustness in init.

**Tech Stack:** Rust no_std, bare-metal aarch64, QEMU virt. Tests run host-side via `cd system/test && cargo test -- --test-threads=1`.

**Testing command:** `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`

**Build command:** `cd /Users/user/Sites/os/system && cargo build --release`

**Visual verification:** After display-affecting changes, use the QEMU screenshot workflow from CLAUDE.md's "Visual Testing" section.

---

## Phase 1: Drawing Library Fixes (B4, B3, B2)

These items live in `libraries/drawing/` which is NOT being split in Phase 2. Safe to do first.

---

### Task 1: B4 — Alpha rounding consistency

**Problem:** `draw_coverage` in `drawing/lib.rs` adds `+ 127` before calling `div255()` on lines 476, 509-511. `div255` already rounds via `(x + 1 + (x >> 8)) >> 8`. This is double-rounding. Meanwhile, `Color::blend_over` (line 117) and other call sites do NOT add `+ 127`. The pipeline is inconsistent.

**Decision:** Remove `+ 127` from lines 476, 509, 510, 511. The `div255` function's built-in `+ 1` rounding is sufficient and matches every other call site.

**Files:**

- Modify: `system/libraries/drawing/lib.rs` (lines 476, 509-511)
- Test: `system/test/tests/drawing.rs`

- [ ] **Step 1: Write a test that captures current vs corrected behavior**

In `system/test/tests/drawing.rs`, add a test that verifies `div255` rounding consistency:

```rust
#[test]
fn div255_no_double_rounding() {
    // div255 already rounds: (x + 1 + (x >> 8)) >> 8
    // Adding +127 before div255 causes double-rounding.
    // Verify that div255(a * b) matches the standard alpha formula
    // for representative values.
    let div255 = |x: u32| -> u32 { (x + 1 + (x >> 8)) >> 8 };

    // Full-coverage glyph with full-opacity color should give 255.
    assert_eq!(div255(255 * 255), 255);

    // Half-coverage, full-opacity → should be ~128.
    let result = div255(255 * 128);
    assert!(result == 128 || result == 127, "got {result}");

    // Verify no +127 needed: div255 alone matches standard integer division
    // for all values in the alpha blending range (0..=65025).
    for a in (0..=255).step_by(17) {
        for b in (0..=255).step_by(17) {
            let product = a * b;
            let fast = div255(product);
            let exact = (product + 127) / 255; // mathematically correct
            assert!(
                fast == exact || fast + 1 == exact || fast == exact + 1,
                "div255({product}) = {fast}, exact = {exact}"
            );
        }
    }
}
```

- [ ] **Step 2: Run test to verify it passes** (this test validates div255 behavior, not the bug)

Run: `cd /Users/user/Sites/os/system/test && cargo test div255_no_double_rounding -- --test-threads=1`

- [ ] **Step 3: Remove `+ 127` from draw_coverage**

In `system/libraries/drawing/lib.rs`:

Line 476 — change:

```rust
let alpha = div255(color_a * cov as u32 + 127);
```

to:

```rust
let alpha = div255(color_a * cov as u32);
```

Lines 509-511 — change:

```rust
let out_r_lin = div255(dst_r_lin * inv_a + src_r_lin * alpha + 127);
let out_g_lin = div255(dst_g_lin * inv_a + src_g_lin * alpha + 127);
let out_b_lin = div255(dst_b_lin * inv_a + src_b_lin * alpha + 127);
```

to:

```rust
let out_r_lin = div255(dst_r_lin * inv_a + src_r_lin * alpha);
let out_g_lin = div255(dst_g_lin * inv_a + src_g_lin * alpha);
let out_b_lin = div255(dst_b_lin * inv_a + src_b_lin * alpha);
```

Also remove the NOTE comment at line 474-475 about the inconsistency (it's now fixed).

- [ ] **Step 4: Run full test suite**

Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
Expected: All ~1,816 tests pass.

- [ ] **Step 5: Build release**

Run: `cd /Users/user/Sites/os/system && cargo build --release`
Expected: Clean build.

- [ ] **Step 6: Commit**

```text
fix: remove double-rounding in draw_coverage alpha blending
```

---

### Task 2: B3 — Rename blur_horizontal_neon to blur_horizontal_scalar

**Problem:** `blur_horizontal_neon()` in `drawing/neon.rs` (line 361) uses scalar u64 arithmetic, not NEON intrinsics. The name is misleading. The vertical counterpart (`blur_vertical_neon`, line 439) actually uses NEON.

**Files:**

- Modify: `system/libraries/drawing/neon.rs` (line 361 — function name + doc comment)
- Modify: `system/libraries/drawing/lib.rs` (line 2462 — call site in `blur_horizontal` dispatcher)

- [ ] **Step 1: Rename function in neon.rs**

In `system/libraries/drawing/neon.rs`, rename `blur_horizontal_neon` to `blur_horizontal_scalar`. Update the doc comment (lines 357-359) to remove the NOTE about the misnaming:

```rust
/// Horizontal blur pass using scalar u64 arithmetic.
///
/// Processes 4 pixels at a time via scalar accumulators. A NEON SIMD
/// port (matching `blur_vertical_neon`) would provide ~2-3x speedup
/// but is deferred — see `design/rendering-pipeline-completion.md` B3.
#[cfg(target_arch = "aarch64")]
pub fn blur_horizontal_scalar(
```

- [ ] **Step 2: Update call site in lib.rs**

In `system/libraries/drawing/lib.rs`, line 2462 inside `blur_horizontal()`:

Change:

```rust
    #[cfg(target_arch = "aarch64")]
    {
        blur_horizontal_neon(
```

to:

```rust
    #[cfg(target_arch = "aarch64")]
    {
        blur_horizontal_scalar(
```

Also update the import if `blur_horizontal_neon` is explicitly imported — search for `use` statements referencing it.

- [ ] **Step 3: Run tests**

Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
Expected: All tests pass (rename only, no behavior change).

- [ ] **Step 4: Build release**

Run: `cd /Users/user/Sites/os/system && cargo build --release`

- [ ] **Step 5: Commit**

```text
refactor: rename blur_horizontal_neon to blur_horizontal_scalar
```

---

### Task 3: B2 — Gamma-correct bilinear interpolation

**Problem:** `blit_transformed_bilinear` in `drawing/lib.rs` (lines 1461-1483) interpolates in sRGB (gamma-encoded) space. The rest of the pipeline is gamma-correct (linearize, blend, re-encode via `SRGB_TO_LINEAR` / `LINEAR_TO_SRGB` tables). This causes banding and color shifts on rotated or scaled content.

**Fix:** Linearize the 4 sampled pixels before bilinear interpolation, then convert back to sRGB. Use the existing gamma tables and `linear_to_idx` helper.

**Files:**

- Modify: `system/libraries/drawing/lib.rs` (lines 1461-1483 in `blit_transformed_bilinear`)
- Test: `system/test/tests/drawing.rs`

- [ ] **Step 1: Write a test for gamma-correct bilinear interpolation**

In `system/test/tests/drawing.rs`:

```rust
#[test]
fn bilinear_interpolation_is_gamma_correct() {
    // Create a 2x2 source with known sRGB values.
    // If interpolation is gamma-correct, the midpoint of two sRGB values
    // should match the linear-space average converted back to sRGB,
    // NOT the naive (sRGB_a + sRGB_b) / 2.
    let srgb_to_linear = drawing::gamma_tables::SRGB_TO_LINEAR;
    let linear_to_srgb = drawing::gamma_tables::LINEAR_TO_SRGB;
    let linear_to_idx = |v: u32| -> usize {
        let idx = v >> 4;
        if idx > 4095 { 4095 } else { idx as usize }
    };

    // sRGB 50 and sRGB 200: naive average = 125, linear average ≈ 150
    let a: u8 = 50;
    let b: u8 = 200;
    let lin_a = srgb_to_linear[a as usize] as u32;
    let lin_b = srgb_to_linear[b as usize] as u32;
    let lin_avg = (lin_a + lin_b) / 2;
    let expected_srgb = linear_to_srgb[linear_to_idx(lin_avg)];

    // Naive sRGB average would be 125; gamma-correct should be different.
    let naive_avg = ((a as u32 + b as u32) / 2) as u8;
    assert_ne!(expected_srgb, naive_avg, "test is meaningless if they match");

    // Create 2x2 BGRA source: top-left=(a,a,a,255), top-right=(b,b,b,255),
    // bottom-left=(a,a,a,255), bottom-right=(b,b,b,255).
    let mut src = [0u8; 2 * 2 * 4];
    // Row 0
    src[0..4].copy_from_slice(&[a, a, a, 255]); // (0,0)
    src[4..8].copy_from_slice(&[b, b, b, 255]); // (1,0)
    // Row 1
    src[8..12].copy_from_slice(&[a, a, a, 255]);  // (0,1)
    src[12..16].copy_from_slice(&[b, b, b, 255]); // (1,1)

    // Blit with identity transform into a 2x2 destination. The center
    // pixel (0,0) at sub-pixel offset (0.5, 0.5) should sample all 4
    // corners equally → the average.
    let mut dst_buf = [0u8; 4 * 4]; // 1x1 is enough
    let mut dst = Surface::new(&mut dst_buf, 1, 1, 4, PixelFormat::Bgra8888);

    // Identity transform but offset so pixel center (0.5, 0.5) maps to
    // source (0.5, 0.5) — the exact midpoint of all 4 texels.
    dst.blit_transformed_bilinear(
        &src, 2, 2, 8, // src data, w, h, stride
        0, 0, 1, 1,    // dst rect
        1.0, 0.0, 0.0, 1.0, 0.0, 0.0, // identity inv transform
        255, // full opacity
    );

    // Check the output pixel (B channel).
    let out_b = dst_buf[0];
    // Should be close to the gamma-correct average, not the naive one.
    let diff_correct = (out_b as i32 - expected_srgb as i32).unsigned_abs();
    assert!(
        diff_correct <= 2,
        "expected ~{expected_srgb} (gamma-correct), got {out_b} (naive would be {naive_avg})"
    );
}
```

- [ ] **Step 2: Run test — should FAIL (bilinear currently uses sRGB space)**

Run: `cd /Users/user/Sites/os/system/test && cargo test bilinear_interpolation_is_gamma_correct -- --test-threads=1`
Expected: FAIL — output will be close to naive average (125), not gamma-correct (~150).

- [ ] **Step 3: Fix bilinear interpolation to use linear space**

In `system/libraries/drawing/lib.rs`, replace lines 1461-1483 (the interpolation block inside `blit_transformed_bilinear`). Change from:

```rust
                // Bilinear blend in BGRA (sRGB gamma) space.
                // TODO: Linearize before interpolation, re-encode after (review 7.16).
                // Current approach introduces banding on rotated/scaled content.
                let inv_fx = 256 - fx;
                let inv_fy = 256 - fy;

                // Interpolate top row: lerp(p00, p10, fx).
                let top_b = (p00.0 as u32 * inv_fx + p10.0 as u32 * fx) >> 8;
                let top_g = (p00.1 as u32 * inv_fx + p10.1 as u32 * fx) >> 8;
                let top_r = (p00.2 as u32 * inv_fx + p10.2 as u32 * fx) >> 8;
                let top_a = (p00.3 as u32 * inv_fx + p10.3 as u32 * fx) >> 8;

                // Interpolate bottom row: lerp(p01, p11, fx).
                let bot_b = (p01.0 as u32 * inv_fx + p11.0 as u32 * fx) >> 8;
                let bot_g = (p01.1 as u32 * inv_fx + p11.1 as u32 * fx) >> 8;
                let bot_r = (p01.2 as u32 * inv_fx + p11.2 as u32 * fx) >> 8;
                let bot_a = (p01.3 as u32 * inv_fx + p10.3 as u32 * fx) >> 8;

                // Interpolate columns: lerp(top, bot, fy).
                let fin_b = ((top_b * inv_fy + bot_b * fy) >> 8) as u8;
                let fin_g = ((top_g * inv_fy + bot_g * fy) >> 8) as u8;
                let fin_r = ((top_r * inv_fy + bot_r * fy) >> 8) as u8;
                let mut fin_a = ((top_a * inv_fy + bot_a * fy) >> 8) as u8;
```

To:

```rust
                // Bilinear blend in linear light space (gamma-correct).
                // Convert sRGB samples → linear u16, interpolate, convert back.
                use crate::gamma_tables::{SRGB_TO_LINEAR, LINEAR_TO_SRGB};

                let inv_fx = 256 - fx;
                let inv_fy = 256 - fy;

                // Linearize all 4 samples (B, G, R channels).
                let l00_b = SRGB_TO_LINEAR[p00.0 as usize] as u32;
                let l00_g = SRGB_TO_LINEAR[p00.1 as usize] as u32;
                let l00_r = SRGB_TO_LINEAR[p00.2 as usize] as u32;
                let l10_b = SRGB_TO_LINEAR[p10.0 as usize] as u32;
                let l10_g = SRGB_TO_LINEAR[p10.1 as usize] as u32;
                let l10_r = SRGB_TO_LINEAR[p10.2 as usize] as u32;
                let l01_b = SRGB_TO_LINEAR[p01.0 as usize] as u32;
                let l01_g = SRGB_TO_LINEAR[p01.1 as usize] as u32;
                let l01_r = SRGB_TO_LINEAR[p01.2 as usize] as u32;
                let l11_b = SRGB_TO_LINEAR[p11.0 as usize] as u32;
                let l11_g = SRGB_TO_LINEAR[p11.1 as usize] as u32;
                let l11_r = SRGB_TO_LINEAR[p11.2 as usize] as u32;

                // Alpha stays in 0-255 (not gamma-encoded).
                let top_a = (p00.3 as u32 * inv_fx + p10.3 as u32 * fx) >> 8;
                let bot_a = (p01.3 as u32 * inv_fx + p11.3 as u32 * fx) >> 8;
                let mut fin_a = ((top_a * inv_fy + bot_a * fy) >> 8) as u8;

                // Bilinear interpolation in linear space.
                let top_b = (l00_b * inv_fx + l10_b * fx) >> 8;
                let top_g = (l00_g * inv_fx + l10_g * fx) >> 8;
                let top_r = (l00_r * inv_fx + l10_r * fx) >> 8;
                let bot_b = (l01_b * inv_fx + l11_b * fx) >> 8;
                let bot_g = (l01_g * inv_fx + l11_g * fx) >> 8;
                let bot_r = (l01_r * inv_fx + l11_r * fx) >> 8;
                let lin_b = (top_b * inv_fy + bot_b * fy) >> 8;
                let lin_g = (top_g * inv_fy + bot_g * fy) >> 8;
                let lin_r = (top_r * inv_fy + bot_r * fy) >> 8;

                // Convert back to sRGB.
                let fin_b = LINEAR_TO_SRGB[linear_to_idx(lin_b)];
                let fin_g = LINEAR_TO_SRGB[linear_to_idx(lin_g)];
                let fin_r = LINEAR_TO_SRGB[linear_to_idx(lin_r)];
```

Note: `linear_to_idx` is already defined in the same file. Ensure the `use` for gamma tables is accessible (it may already be imported at the top of the file; if not, add it).

- [ ] **Step 4: Run test — should now PASS**

Run: `cd /Users/user/Sites/os/system/test && cargo test bilinear_interpolation_is_gamma_correct -- --test-threads=1`
Expected: PASS

- [ ] **Step 5: Run full test suite**

Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`

- [ ] **Step 6: Build release + visual verification**

Run: `cd /Users/user/Sites/os/system && cargo build --release`

If any existing content uses bilinear interpolation (rotated images, scaled content), visually verify it still renders correctly via the QEMU screenshot workflow.

- [ ] **Step 7: Commit**

```text
fix: gamma-correct bilinear interpolation in drawing library
```

---

## Phase 2: File Splits (D)

Split 4 large files into well-scoped modules. Each split is a pure refactor — no behavior changes, all tests must pass identically before and after.

**Important:** These are `no_std` crates. Module files live alongside `main.rs` or `lib.rs` in the same directory. Use `mod foo;` declarations (Rust 2021 module resolution) or `#[path = "foo.rs"] mod foo;` if the crate uses non-standard paths. Check each crate's existing module pattern.

**Testing strategy for each split:** Run `cargo test -- --test-threads=1` and `cargo build --release` after each file. Both must pass identically.

---

### Task 4: Split virgil-render/main.rs (2,060 lines → 5 files)

**Current:** Monolithic GPU driver initialization with 5+ phases, FFI structs, resource management, and pipeline setup all in one file.

**Target modules:**

- `wire.rs` (~180 lines) — FFI wire-format structs (CtrlHeader, DisplayInfo, etc.), DmaBuf, helper constructors
- `device.rs` (~200 lines) — Phase A+B: device init, display query, init handshake
- `resources.rs` (~650 lines) — Phase C: virgl context creation, resource management, VBO setup
- `pipeline.rs` (~250 lines) — Phase D+E: GPU pipeline setup, submit_3d, clear_screen
- `main.rs` (~200 lines) — constants, entry point `_start()`, event loop, module declarations

**Files:**

- Modify: `system/services/drivers/virgil-render/main.rs`
- Create: `system/services/drivers/virgil-render/wire.rs`
- Create: `system/services/drivers/virgil-render/device.rs`
- Create: `system/services/drivers/virgil-render/resources.rs`
- Create: `system/services/drivers/virgil-render/pipeline.rs`

**Dependency chain (acyclic):** wire ← device ← resources ← pipeline ← main

- [ ] **Step 1: Extract wire.rs**

Move from main.rs to wire.rs:

- `box_zeroed()` helper (lines ~111-120)
- All `#[repr(C)]` structs: CtrlHeader, DisplayInfo, CtxCreate, ResourceCreate3d, AttachBacking, MemEntry, CtxResource, SetScanout, ResourceFlush, TransferToHost3d, Submit3dHeader (lines ~122-235)
- DmaBuf struct + impl + Drop (lines ~239-269)
- `ctrl_header()` and `ctrl_header_ctx()` helpers (lines ~273-291)

Make all items `pub(crate)`. Add necessary imports (`use alloc::boxed::Box; use sys;`).

In main.rs, add `mod wire;` and replace usages with `wire::CtrlHeader`, etc., or add `use wire::*;` at the top.

- [ ] **Step 2: Extract device.rs**

Move from main.rs to device.rs:

- `init_device()` (lines ~362-426)
- Display query logic (lines ~429-469)
- `init_handshake()` (lines ~471-523)

Add `use crate::wire;` and other necessary imports.

- [ ] **Step 3: Extract resources.rs**

Move from main.rs to resources.rs:

- All resource creation/management functions (lines ~525-1169): ctx_create, resource_create_3d, attach_backing, ctx_attach_resource, set_scanout, resource_create_vbo, attach_backing_vbo, transfer_vbo_to_host, ctx_attach_vbo, resource_create_3d_generic, attach_and_ctx_resource, transfer_texture_to_host, transfer_buffer_to_host, flush_resource
- `channel_shm_va()` helper (line ~294-296)
- `gpu_command()`, `gpu_cmd_ok()` helpers (lines ~300-342)

- [ ] **Step 4: Extract pipeline.rs**

Move from main.rs to pipeline.rs:

- `submit_3d()` (lines ~1172-1237)
- `setup_pipeline()` (lines ~1239-1335)
- `clear_screen()` (lines ~1339-1385)
- Print helpers: `print_u32()`, `print_hex_u32()` (lines ~344-359)

- [ ] **Step 5: Clean up main.rs**

main.rs should now contain only:

- Module declarations (`mod wire; mod device; mod resources; mod pipeline; mod atlas; mod frame_scheduler; mod scene_walk; mod shaders;`)
- Constants (handle IDs, resource IDs)
- `_start()` entry point + event loop
- Necessary re-imports from submodules

- [ ] **Step 6: Build and test**

Run: `cd /Users/user/Sites/os/system && cargo build --release`
Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
Expected: All tests pass, clean build.

- [ ] **Step 7: Commit**

```text
refactor: split virgil-render/main.rs into wire, device, resources, pipeline modules
```

---

### Task 5: Split scene/lib.rs (1,805 lines → 7 files)

**Current:** Primitive types, transforms, nodes, scene writer/reader, triple buffering, and diffing all in one file.

**Target modules:**

- `primitives.rs` (~380 lines) — Color, Border, DataRef, ShapedGlyph, FillRule, Content, path builders, bitflags, fnv1a
- `transform.rs` (~290 lines) — AffineTransform, trig helpers, F32Ext trait
- `node.rs` (~150 lines) — Node, NodeId, NodeFlags, SceneHeader, memory layout constants
- `writer.rs` (~300 lines) — SceneWriter
- `reader.rs` (~100 lines) — SceneReader
- `triple.rs` (~520 lines) — TripleWriter, TripleReader, control helpers
- `diff.rs` (~170 lines) — build_parent_map, abs_bounds, diff_scenes

**Dependency chain (acyclic):** primitives ← transform ← node ← writer/reader ← triple ← diff

**Files:**

- Modify: `system/libraries/scene/lib.rs` (becomes re-export hub, ~60 lines)
- Create: 6 new files in `system/libraries/scene/`
  - Note: scene is a library crate with `lib.rs`. New files go in `system/libraries/scene/` as sibling modules. The Cargo.toml has `path = "lib.rs"` (or default). Use `mod primitives;` in lib.rs — Rust 2021 resolves `primitives.rs` in the same directory.

- [ ] **Step 1: Extract primitives.rs**

Move: bitflags macro, Border, Color + impl, DataRef, ShapedGlyph, FNV1A constants + fnv1a(), FillRule, PATH\_\* constants, path_move_to/line_to/cubic_to/close, Content enum.

- [ ] **Step 2: Extract transform.rs**

Move: AffineTransform struct + all impl methods, F32Ext trait, min4, max4, sin_cos_f32, tan_f32, floor_f32, ceil_f32.

- [ ] **Step 3: Extract node.rs**

Move: NodeId type alias, NULL constant, NodeFlags (uses bitflags from primitives), Node struct + impl, SceneHeader struct, memory layout constants (MAX_NODES, DATA_BUFFER_SIZE, SCENE_SIZE, CHANGE_LIST_CAPACITY, NODES_OFFSET, DATA_OFFSET).

- [ ] **Step 4: Extract writer.rs**

Move: SceneWriter struct + all impl methods (~40 methods: new, clear, alloc*node, node, node_mut, set_root, push_shaped_glyphs, push_image_pixels, push_path_data, set_node_content*\*, mark_changed, etc.).

- [ ] **Step 5: Extract reader.rs**

Move: SceneReader struct + all impl methods (from_existing, nodes, data, header, node).

- [ ] **Step 6: Extract triple.rs**

Move: Triple-buffering control functions (triple_read_ctrl, triple_write_ctrl, etc.), TripleWriter struct + impl, TripleReader struct + impl, TRIPLE_CONTROL_SIZE, TRIPLE_SCENE_SIZE constants.

- [ ] **Step 7: Extract diff.rs**

Move: build_parent_map, abs_bounds, diff_scenes, and any helper functions they use.

- [ ] **Step 8: Update lib.rs to be re-export hub**

lib.rs should contain only:

```rust
#![no_std]
extern crate alloc;

mod primitives;
mod transform;
mod node;
mod writer;
mod reader;
mod triple;
mod diff;

// Re-export the full public API so downstream crates see no change.
pub use primitives::*;
pub use transform::*;
pub use node::*;
pub use writer::*;
pub use reader::*;
pub use triple::*;
pub use diff::*;
```

The goal is **zero changes to downstream crates** — everything they currently import from `scene::` must still work via re-exports.

- [ ] **Step 9: Build and test**

Run: `cd /Users/user/Sites/os/system && cargo build --release`
Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
Expected: All tests pass. Zero downstream changes needed.

- [ ] **Step 10: Commit**

```text
refactor: split scene/lib.rs into primitives, transform, node, writer, reader, triple, diff modules
```

---

### Task 6: Split render/scene_render.rs (1,824 lines → 4 files)

**Current:** Coordinate utilities, tree walk, content-type rendering, and path rasterization all in one file.

**Target modules:**

- `coords.rs` (~90 lines) — round_f32, scale_coord, scale_size, snap_border
- `walk.rs` (~800 lines) — ClipRect, RenderCtx, SceneGraph, tree walk (render_node, render_shadow), public API (render_scene, render_scene_with_pool, render_scene_clipped, render_scene_clipped_with_pool)
- `content.rs` (~470 lines) — per-content-type rendering (images, glyphs, paths), mask_rounded_rect
- `path_raster.rs` (~314 lines) — PathSegment, flatten_cubic, path_scanline_fill, render_path, scene_to_draw_color

**Files:**

- Modify: `system/libraries/render/scene_render.rs` (becomes re-export hub or is replaced by submodules)
- Create: 4 new files in `system/libraries/render/`

Note: The render crate likely has its own module structure. Check how `scene_render` is currently declared (as `mod scene_render;` in a `lib.rs` or similar). The new files should be submodules of `scene_render` or sibling modules — follow the existing pattern.

- [ ] **Step 1: Extract coords.rs**

Move: round_f32, scale_coord, scale_size, snap_border. These are pure math with no dependencies.

- [ ] **Step 2: Extract path_raster.rs**

Move: PathSegment enum, flatten_cubic, path_scanline_fill, path_fill_span, render_path (the path-specific one), read_f32_le, read_u32_le, f32_to_fp, scene_to_draw_color.

- [ ] **Step 3: Extract content.rs**

Move: All per-content-type rendering functions (render_image, render_glyphs, render_path dispatch), mask_rounded_rect.

- [ ] **Step 4: Update walk.rs (or scene_render.rs)**

The remaining code becomes the tree walk module: ClipRect, RenderCtx, SceneGraph, render*node, render_node_transformed, render_shadow, and all public render_scene*\* functions.

- [ ] **Step 5: Build and test**

Run: `cd /Users/user/Sites/os/system && cargo build --release`
Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`

- [ ] **Step 6: Commit**

```text
refactor: split render/scene_render.rs into coords, walk, content, path_raster modules
```

---

### Task 7: Split core/scene_state.rs (1,200 lines → 3 files)

**Current:** Scene state API, monospace text layout, cursor/selection positioning, and test content generators all in one file.

**Target modules:**

- `scene_state.rs` (~120 lines) — SceneState struct, public API (from_buf, build_editor_scene, update_clock, update_cursor, update_selection, update_document_content), re-exports
- `layout.rs` (~780 lines) — LayoutRun, byte_to_line_col, layout_mono_lines, shape_text, scroll_runs, update_clock_inline, allocate_selection_rects, build_editor_scene_impl
- `test_gen.rs` (~110 lines) — generate_test_image, generate_test_star, generate_test_rounded_rect, sin_approx, cos_approx

**Files:**

- Modify: `system/services/core/scene_state.rs`
- Create: `system/services/core/layout.rs`
- Create: `system/services/core/test_gen.rs`

Note: core is a service binary (`main.rs`). Check how `scene_state` is currently declared in `main.rs` and follow the pattern. New modules likely need to be declared in `main.rs` as well, or as submodules of scene_state.

- [ ] **Step 1: Extract test_gen.rs**

Move: generate_test_image, generate_test_star, generate_test_rounded_rect, sin_approx, cos_approx. These are scaffolding with no dependencies on scene_state internals.

- [ ] **Step 2: Extract layout.rs**

Move: LayoutRun struct, byte_to_line_col, layout_mono_lines, shape_text, scroll_runs, line_bytes_for_run, update_clock_inline, allocate_selection_rects, and the build_editor_scene_impl orchestrator.

- [ ] **Step 3: Update scene_state.rs**

scene_state.rs retains SceneState struct and delegates to layout module. Re-exports byte_to_line_col and layout_mono_lines for downstream use (tests import them).

- [ ] **Step 4: Build and test**

Run: `cd /Users/user/Sites/os/system && cargo build --release`
Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`

- [ ] **Step 5: Commit**

```text
refactor: split core/scene_state.rs into layout and test_gen modules
```

---

## Phase 3: Remaining Feature Fixes (B6, B5, B1)

These items touch files that were just split in Phase 2. They land in the clean modules.

---

### Task 8: B6 — Fix selection data buffer leak

**Problem:** `update_selection()` in `core/scene_state.rs` (now layout.rs after split) calls `update_clock_inline()` which calls `push_shaped_glyphs()`, allocating ~64 bytes in the data buffer per call. But `update_selection` never calls `reset_data()`, so this space is leaked until the next `update_document_content()`.

**Fix:** Skip the clock text update in `update_selection`. Clock updates only need to happen via `update_document_content()` (which is called by the timer-driven clock tick). The clock doesn't change on selection-only updates.

**Files:**

- Modify: `system/services/core/layout.rs` (or wherever `update_selection` landed after Task 7)

- [ ] **Step 1: Write a test verifying the fix**

In `system/test/tests/scene.rs`, add a test that verifies selection-only updates don't grow the data buffer:

```rust
#[test]
fn selection_update_does_not_leak_data_buffer() {
    // Build an initial scene, note the data buffer usage,
    // then call update_selection multiple times and verify
    // the data buffer doesn't grow.
    // (Exact implementation depends on SceneState API —
    //  may need to read data_offset from the scene header
    //  before and after update_selection calls.)
}
```

The exact test depends on how the data buffer offset is exposed after the Task 7 split. The key assertion: data buffer write position should not increase across `update_selection` calls.

- [ ] **Step 2: Remove the clock update from update_selection**

In the file containing `update_selection`, remove or skip the clock text block:

Change:

```rust
            if let Some(ct) = clock_text {
                update_clock_inline(&mut w, ct, cfg.font_data, cfg.font_size, cfg.upem, cfg.axes);
            }
```

To:

```rust
            // Clock text is updated only by update_document_content (timer-driven).
            // Skipping here prevents data buffer leak (~64 bytes per selection update).
            let _ = clock_text;
```

Also remove the TODO comment about the leak (lines 558-563) — it's now fixed.

- [ ] **Step 3: Verify the clock still updates correctly**

The clock is driven by a timer in core's event loop. Verify that `update_document_content` (which does call `update_clock_inline` and resets the data buffer) is still the clock's update path. No functional change expected.

- [ ] **Step 4: Run tests and build**

Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
Run: `cd /Users/user/Sites/os/system && cargo build --release`

- [ ] **Step 5: Commit**

```text
fix: prevent data buffer leak in selection-only scene updates
```

---

### Task 9: B5 — Font size and DPI in CompositorConfig

**Problem:** `FONT_SIZE` (18) and `SCREEN_DPI` (96) are hardcoded constants in cpu-render and virgil-render. The `CompositorConfig` struct in `protocol/lib.rs` doesn't include these fields. The pipeline can't adapt to different displays.

**Fix:** Add `font_size: u16` and `screen_dpi: u16` to `CompositorConfig`. Init populates them. Both render services read them instead of hardcoding.

**Files:**

- Modify: `system/libraries/protocol/lib.rs` (CompositorConfig struct, lines ~288-303)
- Modify: `system/services/init/main.rs` (where CompositorConfig is created, lines ~354-366)
- Modify: `system/services/drivers/cpu-render/main.rs` (read config instead of constants)
- Modify: `system/services/drivers/virgil-render/main.rs` (read config instead of hardcoded `18`)

- [ ] **Step 1: Add fields to CompositorConfig**

In `system/libraries/protocol/lib.rs`, add `font_size` and `screen_dpi` to `CompositorConfig`:

```rust
    pub struct CompositorConfig {
        pub fb_va: u64,
        pub fb_va2: u64,
        pub scene_va: u64,
        pub mono_font_va: u64,
        pub fb_width: u32,
        pub fb_height: u32,
        pub mono_font_len: u32,
        pub prop_font_len: u32,
        pub scale_factor: f32,
        pub frame_rate: u16,
        pub font_size: u16,     // NEW: logical font size in pixels (e.g., 18)
        pub screen_dpi: u16,    // NEW: display DPI (e.g., 96)
        pub _pad: u16,
    }
```

Replace `_pad: u16` with the two new fields + a new `_pad`. Verify the struct still fits in 60 bytes (currently 48 bytes with the old `_pad`; adding 4 bytes → 52 bytes, well within limit). Update the compile-time size assertion if it changes.

- [ ] **Step 2: Update init to populate new fields**

In `system/services/init/main.rs`, where `CompositorConfig` is constructed (lines ~354-366):

```rust
    let render_config = CompositorConfig {
        fb_va: 0,
        fb_va2: 0,
        scene_va: render_scene_va as u64,
        mono_font_va: render_font_va,
        fb_width,
        fb_height,
        mono_font_len,
        prop_font_len,
        scale_factor,
        frame_rate: 60,
        font_size: 18,
        screen_dpi: 96,
        _pad: 0,
    };
```

- [ ] **Step 3: Update cpu-render to read from config**

In `system/services/drivers/cpu-render/main.rs`:

- Remove `const FONT_SIZE: u32 = 18;` and `const SCREEN_DPI: u16 = 96;`
- After parsing the config message, extract: `let font_size = config.font_size as u32;` and `let screen_dpi = config.screen_dpi;`
- Pass these to `CpuBackend::new()` instead of the constants

- [ ] **Step 4: Update virgil-render to read from config**

In `system/services/drivers/virgil-render/main.rs` (after Task 4 split, this may be in `main.rs` or a submodule):

- Replace `let font_size_px: u32 = 18;` with reading from the config message
- Remove the comment `// must match core/main.rs FONT_SIZE`

- [ ] **Step 5: Write a test for CompositorConfig round-trip**

In `system/test/tests/virgl_protocol.rs` (or a new test file):

```rust
#[test]
fn compositor_config_includes_font_fields() {
    let config = CompositorConfig {
        fb_va: 0, fb_va2: 0, scene_va: 0, mono_font_va: 0,
        fb_width: 1024, fb_height: 768,
        mono_font_len: 100, prop_font_len: 0,
        scale_factor: 1.0, frame_rate: 60,
        font_size: 18, screen_dpi: 96, _pad: 0,
    };
    assert_eq!(config.font_size, 18);
    assert_eq!(config.screen_dpi, 96);
    assert!(core::mem::size_of::<CompositorConfig>() <= 60);
}
```

- [ ] **Step 6: Run tests and build**

Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
Run: `cd /Users/user/Sites/os/system && cargo build --release`

- [ ] **Step 7: Commit**

```text
feat: add font_size and screen_dpi to CompositorConfig
```

---

### Task 10: B1 — Wire LRU glyph cache for non-ASCII rendering

**Problem:** The glyph cache is ASCII-only (95 pre-rasterized glyphs). `LruGlyphCache` exists in `fonts/src/cache.rs` but isn't integrated. Non-ASCII glyphs (glyph IDs outside the pre-cached set) return `None` from `GlyphCache::get()` and silently don't render.

**Fix:** Add `LruGlyphCache` as a fallback in the CPU render backend. When `GlyphCache::get()` misses, check the LRU cache. On LRU miss, rasterize on demand and insert into LRU. The virgil-render path uses a GPU glyph atlas with a different architecture — its non-ASCII support is a separate task.

**Scope:** CPU render backend only (virgil-render atlas extension is deferred).

**Files:**

- Modify: `system/libraries/render/scene_render.rs` (or `content.rs` after Task 6 split) — the glyph rendering path (lines ~853-890)
- Modify: `system/libraries/render/lib.rs` (or wherever `CpuBackend` is defined) — add `LruGlyphCache` field
- Test: `system/test/tests/cache.rs` and `system/test/tests/scene_render.rs`

- [ ] **Step 1: Write a test for LRU cache fallback**

In `system/test/tests/cache.rs`:

```rust
#[test]
fn lru_cache_stores_and_retrieves_glyph() {
    let mut lru = fonts::cache::LruGlyphCache::new(64);
    let glyph = fonts::cache::LruCachedGlyph {
        width: 10,
        height: 12,
        bearing_x: 1,
        bearing_y: 10,
        coverage: alloc::vec![128u8; 120],
    };
    lru.insert(500, 18, glyph);
    assert!(lru.get(500, 18).is_some());
    assert!(lru.get(501, 18).is_none()); // not inserted
}
```

- [ ] **Step 2: Run test to verify LruGlyphCache works**

Run: `cd /Users/user/Sites/os/system/test && cargo test lru_cache_stores -- --test-threads=1`

- [ ] **Step 3: Add LruGlyphCache to CpuBackend**

In the render library, add an `LruGlyphCache` field to `CpuBackend` (or the equivalent render context). Initialize it in `CpuBackend::new()` with a reasonable capacity (e.g., 256 entries).

- [ ] **Step 4: Add LRU fallback in glyph rendering path**

In the glyph rendering code (the `Content::Glyphs` match arm), change from:

```rust
if let Some((glyph, coverage)) = cache.get(sg.glyph_id) {
    // render glyph
}
```

To:

```rust
if let Some((glyph, coverage)) = cache.get(sg.glyph_id) {
    // render from ASCII cache
    fb.draw_coverage(px, py, coverage, glyph.width, glyph.height, glyph_color);
} else if let Some(lru_glyph) = lru_cache.get(sg.glyph_id, font_size) {
    // render from LRU cache
    fb.draw_coverage(px, py, &lru_glyph.coverage, lru_glyph.width, lru_glyph.height, glyph_color);
} else {
    // Rasterize on demand, insert into LRU
    // (requires font data access from render context)
    // This is the on-demand rasterization path
}
```

The on-demand rasterization requires access to font data from the render context. The `CpuBackend` already holds font data for the ASCII cache rasterization — extend it to support on-demand rasterization for LRU misses. Use the existing `fonts::rasterize::rasterize_glyph()` function.

- [ ] **Step 5: Run full test suite**

Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
Run: `cd /Users/user/Sites/os/system && cargo build --release`

- [ ] **Step 6: Visual verification**

If possible, add non-ASCII content to the test scene and verify it renders via QEMU screenshot.

- [ ] **Step 7: Commit**

```text
feat: wire LRU glyph cache as fallback for non-ASCII rendering in cpu-render
```

---

## Phase 4: System Robustness (C)

---

### Task 11: C2 — Add boot-phase timeouts to init

**Problem:** Init's three startup wait loops use `u64::MAX` (infinite wait). If a render service or font driver fails during startup, init blocks forever with no indication.

**Loops to fix:**

1. Display info wait (line ~285-291): `sys::wait(&[gpu_ch_handle], u64::MAX)`
2. GPU ready wait (line ~342-348): `sys::wait(&[gpu_ch_handle], u64::MAX)`
3. Font file read wait (line ~900-907): `sys::wait(&[ch_handle], u64::MAX)`

**Fix:** Replace `u64::MAX` with finite timeouts. On timeout, log a diagnostic and either retry or continue with defaults.

**Files:**

- Modify: `system/services/init/main.rs`

- [ ] **Step 1: Define timeout constants**

Near the top of init/main.rs:

```rust
/// Boot-phase timeout: 10 seconds in nanoseconds.
const BOOT_TIMEOUT_NS: u64 = 10_000_000_000;
/// Font read timeout: 5 seconds in nanoseconds.
const FONT_READ_TIMEOUT_NS: u64 = 5_000_000_000;
```

- [ ] **Step 2: Add timeout to display info wait**

Replace the infinite wait loop with:

```rust
    let mut display_info_received = false;
    let mut retries = 0;

    loop {
        match sys::wait(&[gpu_ch_handle], BOOT_TIMEOUT_NS) {
            Ok(_) => {
                if gpu_ch.try_recv(&mut resp_msg) && resp_msg.msg_type == MSG_DISPLAY_INFO {
                    display_info_received = true;
                    break;
                }
            }
            Err(sys::SyscallError::WouldBlock) => {
                retries += 1;
                sys::print(b"init: display info timeout (");
                // print retry count
                sys::print(b")\n");
                if retries >= 3 {
                    sys::print(b"init: FATAL — render service not responding\n");
                    break;
                }
            }
            _ => break,
        }
    }
```

- [ ] **Step 3: Add timeout to GPU ready wait**

Same pattern as Step 2.

- [ ] **Step 4: Add timeout to font read wait**

Same pattern, using `FONT_READ_TIMEOUT_NS`. On timeout, set font length to 0 (render service falls back to built-in bitmap font).

Note: The font read loop currently has a duplicate `sys::wait` call (lines 900 AND 902 both call `sys::wait`). Fix this bug while adding the timeout.

- [ ] **Step 5: Build and test**

Run: `cd /Users/user/Sites/os/system && cargo build --release`
Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`

The timeout behavior can't be easily unit-tested (requires QEMU with a broken render service). Verify by code review that the timeout constants and retry logic are correct.

- [ ] **Step 6: Commit**

```text
fix: add boot-phase timeouts to init wait loops
```

---

### Task 12: C1 — Add crash detection to init idle loop

**Problem:** Init's idle loop (line ~1053) calls `sys::yield_now()` forever. If a child process crashes, init never notices. The display freezes permanently with no diagnostic.

**Fix:** Replace the yield loop with a `sys::wait()` on all child process handles. When a handle becomes ready (child exited), log the event and optionally restart the service.

**Files:**

- Modify: `system/services/init/main.rs`

- [ ] **Step 1: Collect process handles before the idle loop**

After all processes are spawned and started, collect their handles into an array. The exact handles depend on what init spawns — look for all `spawn_with_channel` return values where `proc_handle` is the first element.

```rust
    // Collect child process handles for monitoring.
    let mut child_handles: [u8; 8] = [0; 8];
    let mut child_names: [&[u8]; 8] = [b""; 8];
    let mut child_count: usize = 0;

    // Add each spawned process handle:
    // child_handles[child_count] = render_proc_handle;
    // child_names[child_count] = b"render";
    // child_count += 1;
    // ... repeat for core, input driver, 9p driver, etc.
```

- [ ] **Step 2: Replace yield loop with monitoring loop**

Replace:

```rust
    loop {
        sys::yield_now();
    }
```

With:

```rust
    sys::print(b"  init: monitoring child processes\n");

    loop {
        match sys::wait(&child_handles[..child_count], u64::MAX) {
            Ok(idx) => {
                // Child process at index `idx` has exited.
                sys::print(b"init: CHILD PROCESS EXITED: ");
                sys::print(child_names[idx]);
                sys::print(b"\n");

                // For now, log and continue monitoring remaining processes.
                // Future: restart the crashed service.

                // Remove exited handle from the array by swapping with last.
                child_count -= 1;
                if idx < child_count {
                    child_handles[idx] = child_handles[child_count];
                    child_names[idx] = child_names[child_count];
                }

                if child_count == 0 {
                    sys::print(b"init: all child processes exited\n");
                    break;
                }
            }
            Err(_) => {
                // Spurious wakeup or error — continue monitoring.
            }
        }
    }

    sys::print(b"init: no children remaining, halting\n");
    loop {
        sys::yield_now();
    }
```

- [ ] **Step 3: Build and test**

Run: `cd /Users/user/Sites/os/system && cargo build --release`
Run: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`

- [ ] **Step 4: Visual verification**

Boot QEMU, verify normal operation (processes running, display working). The monitoring loop should be transparent — no visible change during normal operation.

- [ ] **Step 5: Commit**

```text
feat: add child process crash detection to init monitoring loop
```

---

## Post-completion

- [ ] **Update rendering-pipeline-completion.md** — Mark completed items (B1-B6, C1-C2, D).
- [ ] **Run full test suite one final time** — `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
- [ ] **Visual verification** — Boot QEMU with full display pipeline, verify rendering is correct.
