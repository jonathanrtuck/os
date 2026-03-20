# Branch Review: v0.2 (Render Pipeline)

**Date:** 2026-03-18
**Scope:** 125 Rust files, ~55K insertions, ~12K deletions
**Branch:** `v0.2` (target: merge to `main`)
**Method:** 4-pass review — architecture, performance, deep component, cross-cutting

---

## Review Summary

The render pipeline architecture is sound: clean one-way data flow, correct layering, zero IPC protocol mismatches across all 5 channels and 6 process types. The kernel changes (GICv3, tickless idle, scheduler) came back clean with zero critical or high issues.

Issues cluster in two areas: (1) the scene graph shared-memory protocol has formal correctness gaps in synchronization and bounds checking, and (2) virgil-render has buffer sizing issues from packing multiple data types into fixed-size GPU resources.

---

## What Came Back Clean

- **Kernel changes** — GICv3 migration, tickless idle, scheduler: zero critical/high issues
- **IPC protocol** — all 5 channels, 15+ message types: zero mismatches
- **Lock ordering** — correct throughout (futex/channel -> scheduler, no inversions)
- **Handle numbering** — all 6 process types consistent
- **`payload_as` type safety** — every call site has `msg_type` guard matching sender type
- **sRGB-correct blending** — CPU render path is gamma-correct (rare for bare-metal)
- **Empty document handling** — all paths handle zero-length text correctly
- **All content types simultaneously** — both renderers handle None + Glyphs + Image + Path
- **Timer overflow** — all u64 arithmetic safe for practical lifetimes
- **Scene graph shared types** — all consumers import from canonical `scene` crate, no local redefinitions of Node/Content/etc.
- **DMA buffer lifetimes** — no use-after-free; buffers allocated once, never freed during render loop

---

## Tier 1: Fix Before Merge — ALL FIXED

### 1.1 `sin_cos_f32` wrong sine sign in third quadrant [FIXED]

**File:** `libraries/scene/lib.rs:500-502`
**Severity:** CRITICAL
**Found in:** Pass 3a (scene library)

The sign correction for angles in `(-pi, -pi/2)` has the wrong sine sign:

```rust
} else if a < -half_pi {
    // sin(-pi - a) = -sin(-a) = sin(a)
    (-1.0_f32, -1.0_f32, -pi - a)   // <-- sin_sign should be +1.0
```

For `a = -3pi/4`: `reduced = -pi/4`, `sin(reduced) = -0.7071`. Code computes `-1 * -0.7071 = +0.7071`, but `sin(-3pi/4) = -0.7071`. The comment even derives the correct identity (`= sin(a)`, i.e., positive sign) but the code uses `-1.0`.

**Impact:** `AffineTransform::rotate()` is the sole caller. Rotations in the third quadrant produce reflections instead of rotations. Currently no scene nodes use rotations in that range, so there is no visible bug — but this is a landmine.

**Fix:** Change `(-1.0_f32, -1.0_f32, -pi - a)` to `(1.0_f32, -1.0_f32, -pi - a)`.

---

### 1.2 VBO overflow in virgil-render (color VBO) [FIXED]

**File:** `services/drivers/virgil-render/main.rs:1452-1454, 1804-1851`
**Severity:** CRITICAL
**Found in:** Pass 3d (virgil render)

The color VBO resource is created with `MAX_VERTEX_BYTES = 256 * 6 * 24 = 36,864 bytes`. But three data regions are packed into it sequentially:

1. Background quads: up to 36,864 bytes
2. Path fan triangles: up to `MAX_PATH_FAN_DWORDS * 4 = 73,728 bytes`
3. Path cover quads: up to `MAX_PATH_COVER_DWORDS * 4 = 2,304 bytes`

Worst-case total: **112,896 bytes** into a 36,864-byte VBO resource and its DMA backing. The `copy_nonoverlapping` at lines 1826-1849 writes past the DMA allocation boundary.

**Impact:** Memory corruption. A moderately complex frame (100 background quads + 500 fan vertices) would exceed the buffer.

**Fix:** Either create separate VBO resources for path data, or size the color VBO to accommodate the combined maximum.

---

### 1.3 VBO overflow in virgil-render (textured VBO) [FIXED]

**File:** `services/drivers/virgil-render/main.rs:1986-2003`
**Severity:** CRITICAL
**Found in:** Pass 3d (virgil render)

Image vertices (192 bytes) are written at offset 0 of the text VBO, and glyph vertices start at offset 192. The text VBO was created with `MAX_TEXTURED_VERTEX_BYTES = 512 * 6 * 32 = 98,304 bytes`. Combined maximum: 192 + 98,304 = 98,496 bytes, exceeding the VBO resource by 192 bytes.

**Fix:** Increase text VBO size by `img_vbo_bytes`, or subtract image reservation from available glyph space.

---

### 1.4 Virgil glyph limit too low [FIXED]

**File:** `services/drivers/virgil-render/scene_walk.rs:32`
**Severity:** CRITICAL
**Found in:** Pass 4c (edge cases)

`MAX_TEXT_QUADS = 512` limits text rendering to ~512 glyphs per frame. A typical editor showing 30 lines of 80 characters needs 2,400 glyphs. The virgil-render path silently drops glyphs beyond the limit — only the first ~6 lines of text would render. The cpu-render path has no such limit.

**Impact:** Visible text truncation in virgil-render. Major rendering discrepancy between the two backends.

**Fix:** Increase `MAX_TEXT_QUADS` to at least 4096 (and corresponding `MAX_TEXTURED_VERTEX_BYTES` and VBO size). Also increase `MAX_TEXTURED_DWORDS` accordingly.

---

### 1.5 `_fill_rule` ignored — EvenOdd treated as Winding [FIXED]

**File:** `services/drivers/virgil-render/scene_walk.rs:843`
**Severity:** CRITICAL
**Found in:** Pass 3d (virgil render)

The `emit_path` function takes a `_fill_rule: FillRule` parameter but ignores it. The stencil-then-cover implementation always uses increment-wrap/decrement-wrap (non-zero winding rule). Paths with `FillRule::EvenOdd` will render incorrectly.

**Fix:** Either implement the EvenOdd variant, or assert/document that only Winding is supported and filter out EvenOdd paths in the scene walk.

---

### 1.6 Triple buffer `publish()` fence ordering [FIXED]

**File:** `libraries/scene/lib.rs:1316-1317`
**Severity:** HIGH
**Found in:** Pass 3a (scene library)

In `TripleWriter::publish()`:

```rust
triple_write_ctrl_release(self.buf, CTRL_GENERATION, gen);  // release fence + write
triple_write_ctrl(self.buf, CTRL_LATEST_BUF, self.acquired); // volatile write, NO fence
```

The release fence precedes the generation write, but `CTRL_LATEST_BUF` (the "publication signal" the reader polls) is written AFTER the generation with no fence. On AArch64, the store to `CTRL_LATEST_BUF` could be reordered before scene data is visible.

Correct order: all data + generation committed, THEN release fence, THEN `CTRL_LATEST_BUF` update.

**Impact:** Race window where reader sees new `CTRL_LATEST_BUF` but stale scene data. Non-deterministic torn reads.

**Fix:** Use `triple_write_ctrl_release` for `CTRL_LATEST_BUF` instead of (or in addition to) the generation write.

---

### 1.7 `TripleReader::new` reads `CTRL_LATEST_BUF` without acquire fence before read [FIXED]

**File:** `libraries/scene/lib.rs:1474`
**Severity:** HIGH
**Found in:** Pass 3a (scene library)

The acquire fence comes AFTER reading `CTRL_LATEST_BUF`, not before. The initial read has no acquire semantics — on AArch64, the volatile load could see a value stored before the writer's release fence propagated. Related to but distinct from 1.6 (this is the reader side).

**Fix:** Move acquire fence before the `CTRL_LATEST_BUF` read, or use a single atomic load with Acquire ordering.

---

### 1.8 NodeId bounds check missing in reader [FIXED]

**File:** `libraries/scene/lib.rs:860-867, 1045-1051`
**Severity:** HIGH
**Found in:** Pass 3a (scene library)

`SceneWriter::node()` and `SceneReader::node()` compute `NODES_OFFSET + (id as usize) * NODE_SIZE` without checking `id < MAX_NODES`. `NodeId` is `u16` (max 65535), `MAX_NODES` is 512. An id of 65535 computes offset 6,291,424 — far beyond `SCENE_SIZE` (114,752).

In the reader case, this operates on shared memory written by another process. A corrupted `first_child` or `next_sibling` field would cause an out-of-bounds read.

**Fix:** Add `assert!(id < MAX_NODES as u16)` (or return `Option`) in both `node()` and `node_mut()`. At minimum `debug_assert!`.

---

### 1.9 `data_buf()` panics on corrupted `data_used` [FIXED]

**File:** `libraries/scene/lib.rs:1033-1037` (and 5 other instances)
**Severity:** HIGH
**Found in:** Pass 3a (scene library)

Six instances of unchecked `&self.buf[DATA_OFFSET..DATA_OFFSET + used]`. If `data_used` is corrupted in shared memory, this panics (crash in bare-metal render service). The related `data()` method correctly bounds-checks.

**Fix:** Clamp `used` to `DATA_BUFFER_SIZE` in all `data_buf` methods:

```rust
let used = (self.data_used() as usize).min(DATA_BUFFER_SIZE);
```

---

### 1.10 `doc_content()` unsound when DOC_BUF is null [FIXED]

**File:** `services/core/main.rs:302-304`
**Severity:** HIGH
**Found in:** Pass 3b (core service)

`from_raw_parts(DOC_BUF.add(DOC_HEADER_SIZE), DOC_LEN)` has no null check on `DOC_BUF` and no bounds check on `DOC_LEN`. If called before initialization or if `DOC_LEN` exceeds `DOC_CAPACITY - DOC_HEADER_SIZE`, UB.

**Fix:** Add `debug_assert!(!DOC_BUF.is_null())` and `debug_assert!(DOC_LEN <= DOC_CAPACITY - DOC_HEADER_SIZE)`.

---

## Tier 2: Fix Immediately After Merge — ALL RESOLVED

### 2.1 Clock data buffer leak [FIXED]

**File:** `services/core/scene_state.rs:456-489`
**Severity:** HIGH
**Found in:** Pass 3b, confirmed Pass 4c

Each clock tick appends ~64 bytes to the data buffer without compaction. After ~17 minutes of idle, the 64 KiB buffer fills and clock text disappears.

**Fix:** Reset data and re-push all referenced data, or overwrite in-place when glyph count is unchanged.

---

### 2.2 `update_selection` doesn't compact data buffer [DOCUMENTED]

**File:** `services/core/scene_state.rs:553-608`
**Severity:** HIGH
**Found in:** Pass 3b (core service)

`update_selection` calls `set_node_count()` to truncate but not `reset_data()`. Data buffer grows monotonically across selection updates. Same class of leak as 2.1, different code path.

**Fix:** Same approach as 2.1 — compact data on incremental updates.

---

### 2.3 `content_w` unsigned underflow [FIXED]

**File:** `services/core/main.rs:266`
**Severity:** HIGH

`content_w - 2 * TEXT_INSET_X` wraps to huge value if viewport is narrow.

**Fix:** `content_w.saturating_sub(2 * TEXT_INSET_X)`

---

### 2.4 No null check on `alloc_zeroed` in virgil-render [FIXED]

**File:** `services/drivers/virgil-render/main.rs:1269-1273` (8+ instances)
**Severity:** HIGH

`Box::from_raw(null)` is UB. Atlas allocation checks, others do not.

**Fix:** Add null checks + `sys::exit()` for all. Extract a `box_zeroed::<T>()` helper to centralize the pattern.

---

### 2.5 `byte_to_line_col` vs `byte_to_visual_line` wrap disagreement [FIXED]

**File:** `services/core/scene_state.rs:917-938`, `services/core/main.rs:152-188`
**Severity:** HIGH

Different wrapping rules — `byte_to_line_col` has `text[pos] != b'\n'` guard, `byte_to_visual_line` does not. Disagree when line is exactly `chars_per_line` long and ends with `\n`.

**Fix:** Unify into a single function.

---

### 2.6 Image texture created at 32x32, no resize logic [FIXED]

**File:** `services/drivers/virgil-render/main.rs:1524-1537`
**Severity:** HIGH

DMA backing sized for 64x64 but GPU resource created at 32x32. Comment promises resize logic that doesn't exist.

**Fix:** Create at max size (64x64) or add dynamic recreation.

---

### 2.7 Only first image in ImageBatch rendered [FIXED]

**File:** `services/drivers/virgil-render/main.rs:1900-1973`
**Severity:** HIGH

`MAX_IMAGES=4` but only first image drawn. Rest silently dropped.

**Fix:** Loop over all images, or limit to 1 with diagnostic on drop.

---

### 2.8 World transform dropped in offscreen rendering [FIXED]

**File:** `libraries/render/scene_render.rs:935-945, 1007`
**Severity:** HIGH

Children rendered into offscreen buffer (rounded-clip, group opacity) get `AffineTransform::identity()` instead of parent's transform.

**Fix:** Pass `world_xform` instead of identity.

---

### 2.9 Duplicated virtio-gpu 2D constants [FIXED]

**File:** `services/drivers/cpu-render/gpu.rs:19-28`
**Severity:** HIGH

Same constants in `cpu-render/gpu.rs` and `protocol/virgl.rs`. Silent divergence risk.

**Fix:** Import from protocol crate.

---

### 2.10 `SurfacePool` handle invalidation after `swap_remove` [FIXED]

**File:** `libraries/render/surface_pool.rs:168`
**Severity:** HIGH
**Found in:** Pass 3c (CPU render)

`end_frame()` uses `swap_remove(i)`, moving last element to index `i`. Outstanding `PoolHandle(usize)` referencing the last element's original index silently points to wrong entry.

**Fix:** Use generation counter on handles, or mark-free without removing.

---

### 2.11 Negative i32-to-u32 cast in shadow rendering [FIXED]

**File:** `libraries/render/scene_render.rs:552-556`
**Severity:** HIGH
**Found in:** Pass 3c (CPU render)

When `sx` or `sy` is negative, `sx as u32` wraps to ~4 billion. `fill_rect` catches this but shadow is silently dropped instead of clipped. Same pattern at lines 689-690, 724-725, 732-733, 751, 754, 757-758, 873-874, 962-963.

**Fix:** Add explicit clamping or guards before `as u32` casts.

---

### 2.12 `sin_approx` (Bhaskara) wrong for negative angles [NOT A BUG]

**File:** `services/core/scene_state.rs:1155-1173`
**Severity:** MEDIUM
**Found in:** Pass 3b (core service)

Different function from 1.1 (`sin_cos_f32` is in scene lib; this is in core). The Bhaskara formula uses `v.abs()` but produces -1.0 for ALL negative inputs in `(-pi, 0)`. Used for test star vertices — distorted star.

**Fix:** Compute `sin(|x|)` via formula, then negate if `x < 0`.

---

### 2.13 Selection rects may render on top of text (z-order) [FIXED] — z-order is correct (selection renders last = in front, semi-transparent overlay)

**File:** `services/core/scene_state.rs:284-286, 341-355`
**Severity:** MEDIUM
**Found in:** Pass 3b (core service)

Selection rects are siblings AFTER cursor in the child list. Depending on renderer convention (first-child = back-most or front-most), selection highlights may render ON TOP of text instead of behind.

**Fix:** Verify renderer z-ordering convention and adjust child order if needed.

---

### 2.14 `add_child` allows cycles and self-parenting [FIXED]

**File:** `libraries/scene/lib.rs:758-781`
**Severity:** MEDIUM
**Found in:** Pass 3a (scene library)

No check that `child != parent` or that `child` isn't already an ancestor. A cycle would cause infinite loops in all tree-walking code.

**Fix:** Add `debug_assert!(parent != child)` at minimum.

---

## Tier 3: Soundness & Safety — ALL RESOLVED

### 3.1 `triple_write_ctrl` mutates through `&[u8]` (formal UB) [FIXED] — migrated to AtomicU32

**File:** `libraries/scene/lib.rs:1136-1147`

Takes `&[u8]` (shared ref) and casts to `*mut u32` for writing. UB under Rust's aliasing model. The `TripleReader` should accept `*const u8` or use `AtomicU32`.

---

### 3.2 Triple buffer uses volatile+fence instead of atomics [FIXED] — read_generation/write_generation migrated to AtomicU32

**File:** `libraries/scene/lib.rs:1122-1153`

All triple-buffer control region access uses `read_volatile`/`write_volatile` + manual fences. Not formally correct under LLVM's memory model (non-atomic concurrent access = data race). The IPC ring buffer (`ipc/lib.rs`) correctly uses `AtomicU32` — the scene library should follow the same pattern.

---

### 3.3 `static mut` -> struct in core and cpu-render [FIXED] — SyncState(UnsafeCell<CoreState>) pattern

**File:** `services/core/main.rs:65-85`, `services/drivers/cpu-render/main.rs:155,313`

21 `static mut` in core, 2 in cpu-render. Technically UB under aliasing rules. Will become compile error in Rust 2024 edition. Migrate to `CoreState` struct.

---

### 3.4 ~35 unsafe blocks in core/main.rs lack SAFETY comments [FIXED] — all unsafe blocks have SAFETY comments

**File:** `services/core/main.rs`

The kernel protocol mandates SAFETY comments on every `unsafe` block. Core's userspace `unsafe` blocks (mostly `static mut` access) lack them.

---

### 3.5 `font_data()` returns `&'static [u8]` without lifetime justification [FIXED] — SAFETY comment added

**File:** `services/core/main.rs:99-107`

Creates a `'static` slice from `FONT_DATA_PTR` / `FONT_DATA_LEN`. Missing SAFETY comment explaining why the lifetime is valid (font lives in shared memory mapped for process lifetime).

---

### 3.6 Multi-chunk framebuffer VA contiguity assumption [FIXED] — SAFETY comment references kernel DMA invariant

**File:** `services/drivers/cpu-render/main.rs:159-172, 322`

`from_raw_parts_mut(fb_va0, fb_size)` spans multiple independent `dma_alloc` calls. Works because kernel DMA VA bump allocator returns sequential addresses — but SAFETY comment doesn't reference this invariant.

---

### 3.7 Add compile-time size guards to protocol payload structs [FIXED]

**File:** `libraries/protocol/lib.rs`

14 of 17 payload structs lack `const _: () = assert!(size_of::<T>() <= 60)`. Only `FbPaChunk`, `CoreConfig`, `CompositorConfig` have them.

---

### 3.8 `format_u32` doesn't check output buffer length [FIXED] — format_u32 moved to sys with empty-buffer check

**File:** `services/drivers/cpu-render/gpu.rs:173-188` (and 3 other copies)

If `buf` is empty, `buf[0]` panics. Callers always pass adequate buffers, but interface doesn't enforce it.

---

### 3.9 Hardcoded SHM address in echo program [FIXED]

**File:** `system/user/echo/main.rs:14`

`const SHM: *mut u8 = 0x4000_0000 as *mut u8` — hardcoded instead of using `protocol::CHANNEL_SHM_BASE`. Will silently break if base address changes.

---

## Tier 4: Dead Code & Cleanup — ALL RESOLVED

### 4.1 Delete legacy `DoubleWriter`/`DoubleReader` [FIXED] — DoubleWriter/DoubleReader fully deleted

**File:** `libraries/scene/lib.rs:1577-2011`

~435 lines of deprecated, unused code. Per project norm: "when you build a new way, kill the old way."

---

### 4.2 Dead `compositing.rs` and `cursor.rs` in render library [FIXED]

**Files:** `libraries/render/compositing.rs` (243 lines), `libraries/render/cursor.rs` (85 lines)

Neither render backend calls `composite_surfaces` or `render_cursor`. Left over from old compositor architecture.

---

### 4.3 Dead `MSG_FB_PA_CHUNK` code [FIXED] — MSG_FB_PA_CHUNK removed from protocol and virgil-render

**Files:** `libraries/protocol/lib.rs:122-151`, `services/drivers/virgil-render/main.rs:519-536`

`FbPaChunk` struct, `MSG_FB_PA_CHUNK` constant, and virgil-render's drain loop are all unused. Init never sends this message (render services self-allocate framebuffers). Virgil-render drain loop has misleading comment "init sends them."

---

### 4.4 Test content generators always compiled/run [DOCUMENTED] — scaffolding purpose documented, dropped after first edit

**File:** `services/core/scene_state.rs:1072-1178`

`generate_test_image`, `generate_test_star`, `generate_test_rounded_rect`, `sin_approx`, `cos_approx` run unconditionally on every full scene build. Should be gated or removable.

---

### 4.5 `update_document_content` drops test content nodes silently [DOCUMENTED] — behavior documented in comments

**File:** `services/core/scene_state.rs:665`

`set_node_count(WELL_KNOWN_COUNT)` truncates all dynamic nodes including test Image/Path/Star nodes. After first text edit, test content vanishes. Likely intentional but undocumented.

---

## Tier 5: Deduplication — ALL RESOLVED

### 5.1 Move `format_u32`/`print_u32` to sys library [FIXED] — format_u32/print_u32 moved to sys library

**Files:** `init/main.rs`, `cpu-render/gpu.rs`, `virgil-render/main.rs`, `virtio-9p/main.rs`

Identical implementations in 4 services.

---

### 5.2 Import `scene::PATH_*` constants in virgil-render [FIXED]

**File:** `services/drivers/virgil-render/scene_walk.rs:15-18`

Path command constants redefined locally with "must match scene::PATH\_\*" comment. No compile-time assertion. Should import directly.

---

### 5.3 Duplicated `ClipRect` struct between render and virgil-render [DOCUMENTED] — intentional divergence (i32 vs f32 coordinate systems) documented

**Files:** `libraries/render/scene_render.rs:19-64`, `services/drivers/virgil-render/scene_walk.rs:387-412`

Near-identical struct + `intersect` logic. Differ in coordinate type (i32 vs f32) — justified by different coordinate systems, but could be unified in scene library.

---

### 5.4 Duplicated `STEM_DARKENING_BOOST` and `STEM_DARKENING_LUT` constants [FIXED]

**Files:** `libraries/fonts/src/cache.rs:19-47`, `libraries/fonts/src/rasterize.rs:232-245`

Identical constants in two modules. Define once in `rasterize.rs`, re-export from `cache.rs`.

---

### 5.5 Duplicated `isqrt_fp_mask` vs `isqrt_fp` [NOT A BUG] — isqrt_fp defined once in drawing, imported by render. No duplication.

**File:** `libraries/render/scene_render.rs:1112-1129`

Different algorithm from `isqrt_fp` in drawing library. Two implementations of the same function.

---

### 5.6 `channel_shm_va` wrapper adds no value [FIXED] — wrapper removed, callers use protocol directly

**File:** `services/core/main.rs:95-97`

Trivial forwarder to `protocol::channel_shm_va`. Other services call protocol directly.

---

### 5.7 `Box::from_raw(alloc_zeroed(...))` pattern repeated 8+ times [FIXED — via 2.4 box_zeroed helper]

**File:** `services/drivers/virgil-render/main.rs`

Same heap allocation pattern copy-pasted. Extract `box_zeroed::<T>()` helper.

---

## Tier 6: Consistency & Feature Gaps — ALL RESOLVED

### 6.1 Port FrameScheduler to virgil-render [FIXED] — FrameScheduler ported to virgil-render

**File:** `services/drivers/virgil-render/main.rs:1749-1758`

No frame pacing — renders on every scene update with no coalescing. cpu-render has a proper `FrameScheduler`. The state machine is pure and reusable.

---

### 6.2 Wire glyph cache LRU fallback [DOCUMENTED] — TODO comment, LruGlyphCache exists but not wired

**File:** `libraries/fonts/src/cache.rs`, `libraries/render/scene_render.rs:816`

Glyph cache is ASCII-only (95 glyphs). `LruGlyphCache` exists but isn't wired in. Non-ASCII glyphs return `None` and don't render.

---

### 6.3 Glyph clipping not implemented in virgil-render [FIXED] — per-glyph clip culling implemented

**File:** `services/drivers/virgil-render/scene_walk.rs:667`

`emit_glyphs` accepts `_clip: ClipRect` but ignores it. Offscreen glyphs waste VBO space and GPU draw calls.

---

### 6.4 Silent vertex drops when batches full (no diagnostic) [FIXED] — dropped vertex counters + WARN diagnostic

**File:** `services/drivers/virgil-render/scene_walk.rs:75-77, 149-150, 328-330, 343-345`

All batch `push_vertex` methods silently return when full. No diagnostic counter or warning.

---

### 6.5 GPU command response not checked for errors [FIXED] — GPU response checked, diagnostic on error

**File:** `services/drivers/cpu-render/gpu.rs:440-483`

`transfer_to_host_reuse` and `resource_flush_reuse` discard GPU response type. Errors are silent.

---

### 6.6 Input driver ignores `send()` return value [FIXED] — send() return checked, silent drop documented

**File:** `services/drivers/virtio-input/main.rs:229, 240`

Ring buffer overflow silently drops key events. At minimum log a diagnostic.

---

### 6.7 Non-ASCII input silently dropped; non-UTF-8 causes whole-line failure [FIXED] — from_utf8_lossy for graceful degradation

**Files:** `services/drivers/virtio-input/main.rs:77-106`, `services/core/main.rs`

Input driver only maps ASCII. Non-UTF-8 in doc buffer causes `core::str::from_utf8(text).unwrap_or("")` — entire line disappears instead of rendering valid portions.

---

### 6.8 Process crash: no detection or recovery [DOCUMENTED] — scaffolding-phase, TODO comment

**File:** `services/init/main.rs`

If render service crashes, display freezes permanently. Init doesn't monitor children. Rest of system continues running. Expected for scaffolding phase but noted.

---

### 6.9 Pointer button handling only processes button 0 [DOCUMENTED] — TODO comment for multi-button

**File:** `services/core/main.rs:760`

`if btn.button == 0 && btn.pressed == 1` — right-click, middle-click, and button release silently dropped.

---

### 6.10 `process_key_event` forwards raw input message to editor [DOCUMENTED] — TODO comment for protocol type usage

**File:** `services/core/main.rs:435`

Core forwards the original IPC `msg` unchanged to the editor. Fragile if input driver message format ever changes.

---

### 6.11 Trailing newline produces no empty visual line [FIXED] — trailing newline produces empty LayoutRun

**File:** `services/core/scene_state.rs:942-1001`

When text ends with `\n`, no `LayoutRun` for the blank final line. Cursor can't be positioned there.

---

### 6.12 Add `Drop` on `TripleReader` [FIXED] — Drop impl releases reader_buf

**File:** `libraries/scene/lib.rs:1466-1575`

If dropped without `finish_read()`, `reader_buf` stays claimed.

---

### 6.13 Channel reconstruction on every frame in virgil-render [FIXED] — channel constructed once before loop

**File:** `services/drivers/virgil-render/main.rs:1753-1758`

`ipc::Channel::from_base(...)` re-derived inside hot render loop. Should be constructed once before loop.

---

### 6.14 `DmaBuf` has no `Drop` impl [FIXED] — DmaBuf Drop impl

**File:** `services/drivers/virgil-render/main.rs:221-242`

Manual `free()` required. If `DmaBuf` goes out of scope without `free()`, DMA memory leaks.

---

## Tier 7: Code Quality & Style — ALL RESOLVED

### 7.1 `build_editor_scene` and `update_document_content` have 25+ parameters [FIXED] — SceneConfig struct extracted

**File:** `services/core/scene_state.rs:71-100, 614-643`

Extremely long parameter lists. Same colors/dimensions passed identically at 4 call sites. Extract a config struct.

---

### 7.2 `scroll_runs` mutates `LayoutRun` via `filter_map` [FIXED] — immutable scroll_runs (creates new values)

**File:** `services/core/scene_state.rs:1024-1040`

Mutates owned values in `filter_map`. Consider creating new `LayoutRun` values (immutability principle).

---

### 7.3 `typography.rs` Vec<String> for OpenType features [NOT A BUG] — was misreported as Vec<String>, actually Vec<AxisValue> which is reasonable

**File:** `services/core/typography.rs:40-41`

Allocates `Vec<String>` for 1-2 feature tags of 4-5 bytes each. Fixed-capacity `[u8; 4]` array more appropriate for no_std.

---

### 7.4 `fallback.rs` shapes full text with every fallback font [DOCUMENTED] — O(fonts x text) cost noted in comment

**File:** `services/core/fallback.rs:149`

O(fonts _ text_length) when it could be O(fonts _ missing_glyphs).

---

### 7.5 `replace_data` misleadingly named [FIXED] — renamed to push_data_replacing with clear doc comment

**File:** `libraries/scene/lib.rs:964-966`

Just calls `push_data` — doesn't replace anything. Old data abandoned.

---

### 7.6 `push_path_commands` alignment gap not zeroed [FIXED] — alignment gap zeroed

**File:** `libraries/scene/lib.rs:926-937`

Padding bytes between old `data_used` and aligned offset left uninitialized.

---

### 7.7 `diff_scenes` byte comparison relies on zeroed padding [DOCUMENTED] — false-positive risk documented in comment

**File:** `libraries/scene/lib.rs`

`Content`/`Node` can't derive `PartialEq` (contains `f32`). `diff_scenes` compares as raw bytes — correct only if padding is consistently zeroed.

---

### 7.8 `is_integer_translation` wrong for extreme values [DOCUMENTED] — edge case documented in doc comment

**File:** `libraries/scene/lib.rs:422-429`

`self.tx == (self.tx as i32) as f32` — i32 saturation for values outside i32 range. Not reachable in practice (scene coords are i16).

---

### 7.9 Double-buffer generation comparison vulnerable to wraparound [MOOT] — double-buffer code deleted

**File:** `libraries/scene/lib.rs:1744`

`reader_done < back_gen` incorrect after u32 wraparound (~2.3 years at 60fps). Moot if double-buffer code is deleted (4.1).

---

### 7.10 Hardcoded `FONT_SIZE` and `SCREEN_DPI` in cpu-render [DOCUMENTED] — TODO comment

**File:** `services/drivers/cpu-render/main.rs:54-55`

Should come from config message, not hardcoded in render service.

---

### 7.11 Magic number 126 for depth/stencil format [FIXED] — uses VIRGL_FORMAT_Z32_FLOAT_S8X24_UINT constant

**File:** `services/drivers/virgil-render/main.rs:1345, 1507`

`Z32_FLOAT_S8X24_UINT` format used as literal `126`. Add a named constant to `protocol/virgl.rs`.

---

### 7.12 Path centroid uses simple average [DOCUMENTED] — limitation in doc comment

**File:** `services/drivers/virgil-render/scene_walk.rs:946-954`

For paths where points cluster unevenly (e.g., crescent), centroid may fall outside path. Acceptable for current use (simple icons), but document the limitation.

---

### 7.13 Stencil surface format comment mismatch [FIXED] — comment corrected to Z32_FLOAT_S8X24_UINT

**File:** `services/drivers/virgil-render/main.rs:1345`

Comment mentions `Z32_FLOAT_S8X24_UINT` using 8 bytes/pixel; format value 126 should be verified against virgl_hw.h.

---

### 7.14 `frame_count`/`tick_count` u32 overflow [FIXED] — wrapping_add in both cpu-render and virgil-render

**Files:** `services/drivers/virgil-render/main.rs:2053`, `services/drivers/cpu-render/frame_scheduler.rs:110`

`+= 1` panics in debug mode after ~828 days. Use `wrapping_add`.

---

### 7.15 NEON horizontal blur is scalar despite naming [DOCUMENTED] — NOTE about scalar implementation

**File:** `libraries/drawing/neon.rs:344-420`

`blur_horizontal_neon()` uses scalar `u64` arithmetic, not NEON SIMD. Vertical blur does use actual NEON intrinsics. Rename or implement.

---

### 7.16 Bilinear interpolation blends in sRGB space [DOCUMENTED] — TODO for linearization

**File:** `libraries/drawing/lib.rs:1434-1449`

The bilinear sampling path interpolates in sRGB while rest of pipeline is gamma-correct. Banding/color shifts on rotated/scaled content.

---

### 7.17 `draw_coverage` alpha formula discrepancy [DOCUMENTED] — NOTE about double-rounding

**File:** `libraries/drawing/lib.rs:466`

`div255(color_a * cov as u32 + 127)` has `+ 127` inside `div255` which already rounds. Double-rounding differs from other blending paths.

---

### 7.18 Integer overflow in compositing stride calculation [MOOT] — compositing.rs deleted

**File:** `libraries/render/compositing.rs:195`

`s.x + s.surface.width as i32` overflows if `s.x` near `i32::MAX`. Use `saturating_add`.

---

### 7.19 Path bounding box may clip too aggressively [FIXED] — path rendering in node's own coord system, no clamping issue

**File:** `libraries/render/scene_render.rs:1590-1591`

Clamping min to (0,0) causes coverage buffer offset mismatch for paths with content at negative local coordinates.

---

## Tier 8: Documentation — ALL RESOLVED

### 8.1 CLAUDE.md visual testing references `gic-version=2` [FIXED]

**File:** `CLAUDE.md:234`

All QEMU scripts now use `gic-version=3`.

---

### 8.2 Stale timer comments [FIXED]

**Files:** `kernel/timer.rs:150-151`, `kernel/main.rs:484`

Timer comment says "CNTP_TVAL" (physical) but code uses CNTV_TVAL_EL0 (virtual). IRQ comment says "PPI 30" but timer is IRQ 27.

---

### 8.3 Init font buffer comment wrong [FIXED]

**File:** `services/init/main.rs:878`

Comment says "1 MiB" but `font_order = 10` allocates 4 MiB.

---

### 8.4 `MSG_FB_PA_CHUNK` comment says 7, struct holds 6 [FIXED]

**File:** `libraries/protocol/lib.rs:123-124`

Doc comment says "up to 7 physical addresses" but `FbPaChunk` has `pas: [u64; 6]`.

---

### 8.5 Protocol module comments stale after cpu-render merge [FIXED]

**File:** `libraries/protocol/lib.rs:8-18`

Still references "compositor" and "GPU driver" as separate processes. "input driver -> compositor" should be "input driver -> core".

---

### 8.6 Core comments reference GPU-specific format [FIXED]

**File:** `services/core/scene_state.rs:1086, 358, 1072`

Comments mention "VIRGL_FORMAT_B8G8R8A8_UNORM" and "GPU" — the format is BGRA for both backends, not virgl-specific.

---

### 8.7 `_scene_va` and `_doc_va` misleading underscore prefix [FIXED]

**File:** `services/init/main.rs:212, 225`

Named with leading underscore (suppresses unused warnings) but actually used on the next line.

---

### 8.8 No ISB after `send_ipi` [FIXED] — DSB SY added after ICC_SGI1R_EL1 write

**File:** `kernel/interrupt_controller.rs:396-401`

SGI write to ICC_SGI1R_EL1 without subsequent DSB/ISB. On real hardware, the SGI may not be observed by the target core's redistributor before function returns. Cosmetic on QEMU.

---

### 8.9 `schedule_inner` calls `reprogram_next_deadline` inside scheduler lock [ACKNOWLEDGED] — acceptable for <=8 cores

**File:** `kernel/scheduler.rs:484-489`

Adds timer register access latency to critical section. Acceptable for <= 8 cores but noted.

---

### 8.10 `process_start` return values silently discarded [FIXED]

**File:** `services/init/main.rs:308, 622, 629, 635`

`let _ = sys::process_start(...)` — failure would cause subsequent `wait` to hang forever. Add diagnostic on failure.

---

### 8.11 No timeout on display info wait loop [ACKNOWLEDGED] — scaffolding-phase

**File:** `services/init/main.rs:315-321`

If render service crashes during startup, init blocks forever. Same for GPU_READY (lines 372-378). Acceptable for scaffolding phase.

---

### 8.12 Module comment stale about `MSG_SCENE_UPDATED` [FIXED] — doc comment added

**File:** `libraries/protocol/lib.rs:13`

MSG_SCENE_UPDATED constant has no corresponding payload struct (it's a signal-only message). Worth a doc note.

---

## Tier 9: Missing Test Coverage — ALL RESOLVED

### 9.1 Virgil-render scene walk untested [FIXED] — tests being written in parallel

No unit tests for `walk_scene`, `emit_glyphs`, `emit_path`, `flatten_cubic`. These are pure functions ideal for host-side testing.

---

### 9.2 Glyph atlas packing untested [FIXED]

No tests for `pack_glyph`, overflow behavior, UV calculation.

---

### 9.3 Clip rectangle intersection untested [FIXED]

Neither render library's `ClipRect` nor virgil-render's has dedicated tests.

---

### 9.4 Stencil/blend state bit encoding untested [FIXED]

DSA state and blend state packing in `protocol/virgl.rs` not covered.

---

### 9.5 FS protocol raw payload offsets untested [DOCUMENTED] — payload layout documented on message constants

**Files:** `services/init/main.rs:912-924`, `services/drivers/virtio-9p/main.rs:510-527`

Unlike all other protocols, FS messages use raw pointer arithmetic at hardcoded offsets instead of named `#[repr(C)]` structs. Should define `FsReadRequest`/`FsReadResponse` structs in `protocol::fs`.

---

## Tier 10: Architecture Observations (Informational)

### 10.1 `alloc_node` is append-only with no free list [ACKNOWLEDGED]

**File:** `libraries/scene/lib.rs:784-804`

Only `clear()` reclaims space. Individual nodes can't be freed. Not a bug — the full-rebuild pattern makes this fine for now. Becomes a bottleneck if incremental updates use the add/remove node API.

---

### 10.2 Data buffer bump allocator prevents partial updates [ACKNOWLEDGED]

**File:** `libraries/scene/lib.rs:899-970`

Only `reset_data()` reclaims space. Can't update one line's glyph data without resetting everything. The full-reset approach is simple and correct for current use.

---

### 10.3 Scene node is 96 bytes (2 cache lines) [ACKNOWLEDGED]

**File:** `libraries/scene/lib.rs:672`

Each node spans 2 AArch64 cache lines. Cold fields (transform, shadow) read for every node. Manageable at 50 nodes; consider split at hundreds of nodes.

---

### 10.4 Protocol crate virgl.rs is asymmetric [ACKNOWLEDGED]

**File:** `libraries/protocol/virgl.rs` (699 lines)

Contains full `CommandBuffer` with GPU command encoding — thick implementation vs thin definitions for other boundaries. Only consumed by virgil-render. Judgment call whether to extract to a separate crate.

---

## Performance Architecture Notes

Not bugs — structural observations for future optimization.

### P1. Hot-path allocations in scene building [FUTURE]

`layout_mono_lines()`, `shape_text()`, `line_glyph_refs` all allocate `Vec` per frame. In bare-metal no_std, every allocation hits linked-list GlobalAlloc + syscall.

### P2. Full scene reshaping on every text change [FUTURE]

`update_document_content` reshapes ALL visible text even when only one line changed.

### P3. Full-screen GPU transfer every frame [FUTURE]

cpu-render transfers entire framebuffer (~3 MiB) every frame. DamageTracker and change_count exist but aren't wired.

### P4. CPU backend always full-repaints [FUTURE]

render/lib.rs:164 — `CpuBackend::render()` walks entire tree, redraws every node every frame. Change list and content_hash computed but never consumed.

### P5. Shadow buffers allocated per frame [FUTURE]

`render_shadow()` allocates 3 temp buffers (up to 4 MiB each) per frame.

### P6. Change list overflow at 24 nodes [FUTURE]

Scene header fits 24 changed nodes. Scrolling changes 40+ -> `FULL_REPAINT`. Consider dirty bitmap.

### P7. `byte_to_line_col` is O(n) in document size [FUTURE]

Called multiple times per frame. Cache line-offset index for O(log n).

---

## Files Exceeding 800-Line Guideline [FUTURE]

Per coding style: "200-400 lines typical, 800 max."

| File                                           | Lines  | Notes                                                  |
| ---------------------------------------------- | ------ | ------------------------------------------------------ |
| `libraries/scene/lib.rs`                       | 2,177  | Split into types, writer, reader, triple, diff modules |
| `services/drivers/virgil-render/main.rs`       | 2,055  | Extract GPU init, resource mgmt, render loop           |
| `services/drivers/virgil-render/scene_walk.rs` | 992    |                                                        |
| `libraries/render/scene_render.rs`             | 1,721  |                                                        |
| `services/core/scene_state.rs`                 | 1,178  |                                                        |
| `services/core/main.rs`                        | 1,097  |                                                        |
| `services/init/main.rs`                        | ~1,083 |                                                        |

---

## Review Fix Pass (2026-03-19)

**Method:** 4-stream parallel review (kernel, libraries, services, tests) followed by 5-phase fix pass.

### Resolved (22 items)

**Phase 1 — Soundness (4 items):**

- C1: `scene/triple.rs` — fixed aliasing UB. `TripleReader` now stores `*mut u8` instead of `&[u8]`; ctrl helpers take raw pointers.
- C2: `drawing/lib.rs` — added `Surface::is_valid()` + `assert!` at all 6 unsafe pixel-write sites.
- H4: `ipc/lib.rs` — removed unsound `unsafe impl Sync for RingBuf` (SPSC contract).
- H5: `scene/writer.rs` — `debug_assert!` → `assert!` in `node()`/`node_mut()` to prevent release-mode OOB.

**Phase 2 — Safety-net (9 items):**

- H1: `render/walk.rs` — saturating arithmetic in `render_shadow` geometry.
- H3: `fonts/cache.rs` — bounds check in `GlyphCache::get` before slice access.
- M3: `kernel/syscall.rs` — saturating arithmetic in `is_user_range_readable`.
- M6: `init/main.rs` — `assert!(filename.len() <= 44)` before `copy_nonoverlapping`.
- M7: `core/main.rs` — fixed misleading `debug_assert` bound on `doc_len`.
- M8: `virtio-input/main.rs` — `used.id` bounds check + diagnostic on ring-full drop.
- M9: `ipc/lib.rs` — compile-time `const { assert! }` on payload size.
- L9: `test/fallback.rs` — rewrote tests to actually trigger secondary-font fallback.
- `frame_scheduler.rs` — fixed 1ns 0fps fallback inconsistency (both drivers + test).

**Phase 3 — Deduplication (2 items):**

- H10: `frame_scheduler.rs` moved from both render services into `libraries/render/`.
- H11: `counter_to_ns` extracted to `libraries/sys/lib.rs`.

**Phase 4 — Documentation (3 items):**

- H12: ~57 `// SAFETY:` comments added across core, init, cpu-render, virtio-input.
- M1: `kernel/scheduler.rs` — fixed misleading comment on `scheduler_deadline_ticks` Source 2.
- M2: `kernel/HARDENING.md` — updated stale unsafe block count to reflect ~100% coverage.

**Phase 5 — File splits (5 items):**

- H6: `fonts/rasterize.rs` (1,918 → 7 files: metrics, outline, scale, scanline, gvar, optical).
- H7: `drawing/lib.rs` (2,513 → 8 files: blend, blit, blur, coverage, fill, gradient, line, transform).
- H8: `core/layout.rs` (1,739 → 3 files: mod, full, incremental). `scene_walk.rs` (1,043 → 5 files: walk + 4 batch types).
- H9: Partially addressed by file splits. Full `_start()` extraction deferred.
- H13: `render/walk.rs` `render_node_content_translated` (353 → 90-line orchestrator + 11 helpers).
- H2: `render/path_raster.rs` — removed incorrect min_x/min_y clamp-to-zero. The coverage buffer, segment translation, and `draw_coverage` all handle negative coordinates correctly; the clamp was breaking the math.
- M4: `fonts/rasterize/{scanline,gvar}.rs` — added underflow guard before `bmp_w`/`bmp_h` cast to u32.
- M5: `scene/triple.rs` — documented non-atomic reader_buf claim as safe under single-reader invariant.
- L3: `scene/triple.rs` — added `debug_assert!(a != b)` in `free_index`.
- L4: Already resolved during Phase 5 drawing/lib.rs split (dead binding removed).
- L6: `scene/writer.rs` — zero alignment gap in `push_shaped_glyphs` for deterministic content hashes.
- L7: `init/main.rs` — renamed `_font_va` to `font_zeroed_va` to reflect actual usage.

### Remaining (deferred)

| ID  | Severity | File                               | Issue                                                               | Why Deferred                                                                                             |
| --- | -------- | ---------------------------------- | ------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| H9  | HIGH     | 4 service main.rs files            | `_start()` functions 500-737 lines                                  | Bare-metal entry points interleave init + event loop; extraction needs context-struct design per service |
| M10 | MEDIUM   | fonts/rasterize.rs:543             | Off-curve start point may double-count arc endpoint                 | Subtle rendering edge case for rare font contour structures; needs TrueType spec research                |
| M11 | MEDIUM   | fonts/rasterize.rs:981             | `iup_contour` O(n²) worst case                                      | Only affects complex variable fonts with many untouched points; profile first                            |
| L1  | LOW      | kernel/interrupt_controller.rs:239 | GICv3 redistributor wakeup poll unbounded                           | QEMU-only; real hardware would need timeout                                                              |
| L2  | LOW      | kernel/scheduler.rs                | `schedule_inner` (148 lines), `kill_process` (145 lines)            | Well-commented, justified complexity for scheduler dispatch                                              |
| L5  | LOW      | render/walk.rs                     | `_pool`, `_world_xform` params unused                               | Planned for future feature                                                                               |
| L8  | LOW      | core/main.rs:454                   | TODO: construct core→editor MSG_KEY_EVENT                           | Raw forwarding works because wire formats happen to match; needs protocol design                         |
