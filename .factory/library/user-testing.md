# User Testing

Testing surface, validation approach, and resource cost classification.

---

## Current Mission: Tickless Idle + Inter-Processor Interrupts

This mission replaces GICv2 with GICv3, adds IPI-driven wakeup, and converts the fixed 250Hz tick to tickless idle. Key testing considerations:
- After GICv3 migration, ALL QEMU scripts use gic-version=3
- Stress testing is critical — SMP timing changes may surface latent bugs
- Timer behavior changes require verifying deadline accuracy (not just "fires eventually")
- IPI delivery must be verified across all 4 SMP cores

---

## Validation Surface

**Primary surface:** Host-side unit tests
- Command: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
- 1764+ tests across 56 files, all on macOS aarch64
- Tests cover: drawing primitives, scene graph, font shaping/caching, compositor rendering, kernel modules

**Secondary surface:** QEMU framebuffer screenshots
- Build: `cd /Users/user/Sites/os/system && cargo build --release`
- Visual test script: `cd /Users/user/Sites/os/system && bash test-qemu.sh`
- Manual: launch QEMU, send keys via monitor socket, `screendump` to PPM, convert to PNG, view
- Confirms end-to-end pixel correctness that unit tests can't cover

**Tertiary surface:** QEMU serial output
- Diagnostic logs, panic messages, timing data
- Captured via `-serial file:/tmp/qemu-serial.log` or `-serial mon:stdio`

## Validation Concurrency

**Max concurrent validators: 1**
- QEMU instances are resource-heavy (~250MB RAM, ~200% CPU for 4 SMP cores)
- QEMU instances share test.img disk image — concurrent access would corrupt
- Sequential validation is the correct approach for this project

**Disk image locking workaround:** If the user's QEMU is running and holding a lock on `test.img`, worker QEMU instances must create a separate copy: `cp system/test.img /tmp/worker-test.img` and use that copy in their QEMU flags. Do not attempt to use the same `test.img` concurrently.

## Flow Validator Guidance: Unit Tests

**Surface:** Host-side Rust unit tests (macOS aarch64)
**Testing tool:** Direct `cargo test` invocation
**Isolation:** Each validator runs cargo test with specific test name filters. No shared mutable state — tests are pure functions operating on in-memory structures.

**How to verify assertions:**
1. Run the full test suite: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
2. For specific assertions, identify the relevant test names by searching the test source files
3. Confirm test passes (exit code 0, "ok" in output)
4. For assertions about specific behavior, read the test source to confirm it verifies the claimed property

**Key test files for pipeline-fixes:**
- `system/test/tests/scene.rs` — scene graph, SceneWriter, SceneReader, double-buffer tests
- `system/test/tests/scene_render.rs` — compositor rendering, damage tracking, clipped rendering
- `system/test/tests/core.rs` — core OS service logic, document updates, clock updates

**Note:** Test source files live in `system/test/tests/*.rs`, NOT `system/test/src/*.rs`.

**Shared state:** None. Tests create their own in-memory buffers. No file I/O, no network.
**Boundaries:** Do not modify any source files. Do not run QEMU. Only observe test results.

## Flow Validator Guidance: QEMU Visual

**Surface:** QEMU framebuffer screenshots
**Testing tool:** QEMU monitor socket + Python PIL for image conversion + Read tool for visual inspection
**Isolation:** Launch a separate QEMU instance with unique socket paths. Copy test.img to /tmp/ to avoid disk conflicts.

**How to verify assertions:**
1. Build: `cd /Users/user/Sites/os/system && cargo build --release`
2. Copy disk image: `cp /Users/user/Sites/os/system/test.img /tmp/val-test.img`
3. Launch QEMU with unique paths (IMPORTANT — include 9p and rtc flags for font loading and clock):
   ```sh
   cd /Users/user/Sites/os/system && qemu-system-aarch64 \
     -machine virt,gic-version=3 -cpu cortex-a53 -smp 4 -m 256M \
     -global virtio-mmio.force-legacy=false \
     -drive "file=/tmp/val-test.img,if=none,format=raw,id=hd0" \
     -device virtio-blk-device,drive=hd0 \
     -device virtio-gpu-device -device virtio-keyboard-device \
     -device virtio-tablet-device \
     -fsdev local,id=fsdev0,path=share,security_model=none \
     -device virtio-9p-device,fsdev=fsdev0,mount_tag=hostshare \
     -rtc base=localtime \
     -nographic \
     -serial file:/tmp/val-serial.log \
     -monitor unix:/tmp/val-mon.sock,server,nowait \
     -device "loader,file=virt.dtb,addr=0x40000000,force-raw=on" \
     -kernel target/aarch64-unknown-none/release/kernel &
   ```
   **Without 9p flags:** Compositor will report 'no font data' and crash.
   **Without -rtc:** Clock will not show correct time.
   **Keystroke delay:** Use ≥0.05s between keystrokes. Faster rates (0.02s) may trigger pre-existing kernel crashes.
4. Wait for boot (5-8s), send keystrokes via `echo "sendkey h" | nc -U /tmp/val-mon.sock -w 1`
5. Capture screenshot: `echo "screendump /tmp/val-screen.ppm" | nc -U /tmp/val-mon.sock -w 2`
6. Convert: `python3 -c "from PIL import Image; Image.open('/tmp/val-screen.ppm').save('/tmp/val-screen.png')"`
7. View with Read tool to verify pixels
8. Kill QEMU when done

**Shared state:** Uses /tmp/ for all temporary files with `val-` prefix. Do not use `system/test.img` directly.
**Boundaries:** Do not modify source files. Do not interfere with user's QEMU (PID check before kill).

## Flow Validator Guidance: Coordinate Model (Unit Tests)

**Surface:** Host-side Rust unit tests (macOS aarch64)
**Testing tool:** Direct `cargo test` invocation with name filters
**Isolation:** No shared mutable state — tests are pure functions on in-memory buffers.
**Max concurrency:** 1 (cargo build lock contention)

**Assertion → Test mapping:**

| Assertion | Test name(s) | File |
|-----------|-------------|------|
| VAL-COORD-001 | `fractional_scale_1_0_matches_integer_scale_1` | scene_render.rs |
| VAL-COORD-001 | `fractional_scale_2_0_matches_integer_scale_2` | scene_render.rs |
| VAL-COORD-002 | `fractional_scale_1_5_correct_physical_dimensions` | scene_render.rs |
| VAL-COORD-003 | `fractional_scale_no_gap_between_adjacent_nodes` | scene_render.rs |
| VAL-COORD-004 | `fractional_scale_border_pixel_snapped` | scene_render.rs |
| VAL-COORD-005 | `font_physical_pixel_size_at_fractional_scale` | scene_render.rs |
| VAL-COORD-006 | `glyph_cache_keyed_on_physical_pixel_size` | scene_render.rs |
| VAL-COORD-007 | `f32_scale_factor_exact_representation` | scene_render.rs |
| VAL-COORD-008 | `compositor_config_fits_ipc_payload` | scene_render.rs |
| VAL-COORD-009 | `scroll_offset_fractional_scale` | scene_render.rs |
| VAL-COORD-010 | `dirty_rect_fractional_scale_full_coverage` | scene_render.rs |
| VAL-COORD-011 | `fractional_scale_zero_no_panic`, `fractional_scale_negative_treated_as_safe`, `fractional_scale_extreme_clamped` | scene_render.rs |
| VAL-COORD-012 | `scene_graph_node_struct_unchanged` + full test suite pass | scene_render.rs |
| VAL-COORD-013 | `abs_bounds_accounts_for_scroll_y`, `abs_bounds_nested_scroll_containers` | scene.rs |
| VAL-CROSS-009 | `fractional_scale_1_0_matches_integer_scale_1`, `fractional_scale_2_0_matches_integer_scale_2` (byte-for-byte comparison confirms text rendering unchanged) | scene_render.rs |

**Verification approach:**
1. Run `cargo test -- --test-threads=1` to confirm all tests pass
2. For each assertion, confirm the mapped test(s) verify the claimed property by reading test source
3. Run specific test filters to confirm individual assertion coverage
4. Record pass/fail per assertion

## Flow Validator Guidance: Compositing Model (Unit Tests)

**Surface:** Host-side Rust unit tests (macOS aarch64)
**Testing tool:** Direct `cargo test` invocation with name filters
**Isolation:** No shared mutable state — tests are pure functions on in-memory buffers.
**Max concurrency:** 1 (cargo build lock contention)

**Key test files:**
- `system/test/tests/scene_render.rs` — per-subtree opacity tests (VAL-COMP-001 through VAL-COMP-011)
- `system/test/tests/surface_pool.rs` — offscreen buffer pool tests (VAL-COMP-004 through VAL-COMP-007)

**Assertion → Test mapping:**

| Assertion | Test name(s) | File |
|-----------|-------------|------|
| VAL-COMP-001 | `group_opacity_differs_from_individual_opacity` | scene_render.rs |
| VAL-COMP-002 | `opacity_255_bypasses_offscreen` | scene_render.rs |
| VAL-COMP-003 | `opacity_zero_produces_no_output` | scene_render.rs |
| VAL-COMP-004 | `buffer_dimensions_match_node_bounds_times_scale` | surface_pool.rs |
| VAL-COMP-005 | `buffer_cleared_to_transparent_on_acquire`, `no_stale_data_across_frames` | surface_pool.rs |
| VAL-COMP-006 | `second_frame_reuses_first_frames_buffer` | surface_pool.rs |
| VAL-COMP-007 | `two_simultaneous_buffers_allocated_and_reused` | surface_pool.rs |
| VAL-COMP-008 | `srgb_correct_group_opacity` | scene_render.rs |
| VAL-COMP-009 | `nested_group_opacity` | scene_render.rs |
| VAL-COMP-010 | `offscreen_opacity_respects_clip` | scene_render.rs |
| VAL-COMP-011 | `offscreen_opacity_respects_scroll` | scene_render.rs |
| VAL-CROSS-010 | `opacity_change_detected_by_damage` | scene_render.rs |
| VAL-CROSS-015 | `double_buffer_swap_preserves_opacity` | scene_render.rs |

**Verification approach:**
1. Run full test suite to confirm all tests pass
2. For each assertion, confirm the mapped test(s) verify the claimed property by reading test source
3. Run specific test filters to confirm individual assertion coverage
4. Record pass/fail per assertion

## QEMU Test Scripts

- `test-qemu.sh` — interactive display pipeline test (safe, user's QEMU is closed)
- `test/smoke.sh` — boot smoke test, 17 assertions, 10s timeout
- `test/stress.sh` — headless stress test, 180s timeout
- `test/integration.sh` — full device pipeline, 15s timeout
- `test/crash.sh` — rapid input via AppleScript, 30s timeout

## Flow Validator Guidance: Blur and Shadows (Unit Tests)

**Surface:** Host-side Rust unit tests (macOS aarch64)
**Testing tool:** Direct `cargo test` invocation with name filters
**Isolation:** No shared mutable state — tests are pure functions on in-memory buffers.
**Max concurrency:** 1 (cargo build lock contention)

**Key test files:**
- `system/test/tests/drawing.rs` — blur primitives, shadow compositing (VAL-BLUR-001 through VAL-BLUR-015)
- `system/test/tests/neon.rs` — NEON SIMD blur tests (VAL-BLUR-006)
- `system/test/tests/scene_render.rs` — compositor shadow rendering, cross-area tests (VAL-CROSS-004, VAL-CROSS-006, VAL-CROSS-011)
- `system/test/tests/scene.rs` — shadow field preservation in double-buffer (VAL-BLUR-012 partial)

**Assertion → Test mapping:**

| Assertion | Test name(s) | File |
|-----------|-------------|------|
| VAL-BLUR-001 | `blur_single_pixel_symmetric` | drawing.rs |
| VAL-BLUR-002 | `blur_two_pass_matches_2d_reference` | drawing.rs |
| VAL-BLUR-003 | `blur_radius_zero_identity` | drawing.rs |
| VAL-BLUR-004 | `blur_edge_clamping_small_surface` | drawing.rs |
| VAL-BLUR-005 | `blur_large_radius_capped` | drawing.rs |
| VAL-BLUR-006 | `neon_blur_matches_scalar` | neon.rs |
| VAL-BLUR-007 | `blur_sigma_varies_spread` | drawing.rs |
| VAL-BLUR-008 | `shadow_renders_behind_source_with_offset` | scene_render.rs |
| VAL-BLUR-009 | `shadow_spread_expands_footprint` | scene_render.rs |
| VAL-BLUR-010 | `shadow_zero_blur_is_hard_shadow` | scene_render.rs |
| VAL-BLUR-011 | `shadow_color_applied_correctly` | scene_render.rs |
| VAL-BLUR-012 | `default_shadow_fields_no_shadow` | scene_render.rs |
| VAL-BLUR-013 | `blur_trait_defined_cpublur_implements` | drawing.rs |
| VAL-BLUR-014 | `blur_preserves_alpha_channel` | drawing.rs |
| VAL-BLUR-015 | `shadow_falloff_is_smooth_gradient` | scene_render.rs |
| VAL-CROSS-004 | `fractional_scale_preserves_blur_radius` | scene_render.rs |
| VAL-CROSS-006 | `layer_opacity_applies_to_shadow` | scene_render.rs |
| VAL-CROSS-011 | `shadow_overflow_in_damage_rects` | scene_render.rs |

**Verification approach:**
1. Run full test suite to confirm all tests pass
2. For each assertion, run the specific test(s) with name filter
3. Read test source to confirm it verifies the claimed property
4. Record pass/fail per assertion

## Flow Validator Guidance: Transforms and Resampling (Unit Tests)

**Surface:** Host-side Rust unit tests (macOS aarch64)
**Testing tool:** Direct `cargo test` invocation with name filters
**Isolation:** No shared mutable state — tests are pure functions on in-memory buffers.
**Max concurrency:** 1 (cargo build lock contention)

**Key test files:**
- `system/test/tests/scene.rs` — AffineTransform type, composition, AABB, Node fields, double-buffer preservation
- `system/test/tests/scene_render.rs` — transform rendering, clipping, text, opacity+transform, DPI composition, damage tracking
- `system/test/tests/drawing.rs` — bilinear resampling, ResamplingMethod enum

**Assertion → Test mapping:**

| Assertion | Test name(s) | File |
|-----------|-------------|------|
| VAL-XFORM-001 | `identity_transform_pixel_identical` | scene_render.rs |
| VAL-XFORM-002 | `translate_shifts_content` | scene_render.rs |
| VAL-XFORM-003 | `affine_transform_aabb_90_rotation`, `rotation_90_aabb_clip` | scene.rs, scene_render.rs |
| VAL-XFORM-004 | `affine_transform_rotate_180` | scene.rs |
| VAL-XFORM-005 | `scale_doubles_area` | scene_render.rs |
| VAL-XFORM-006 | `non_uniform_scale` | scene_render.rs |
| VAL-XFORM-007 | `child_transform_composes_with_parent`, `affine_transform_compose_translations` | scene_render.rs, scene.rs |
| VAL-XFORM-008 | `affine_transform_compose_three_levels` | scene.rs |
| VAL-XFORM-009 | `affine_transform_aabb_45_rotation` | scene.rs |
| VAL-XFORM-010 | `clip_rect_intersected_with_transformed_aabb` | scene_render.rs |
| VAL-XFORM-011 | `affine_transform_skew_x`, `affine_transform_skew_x_parallelogram` | scene.rs |
| VAL-XFORM-012 | `transformed_text_uses_axis_aligned_glyph_rendering` | scene_render.rs |
| VAL-XFORM-013 | `bilinear_resampling_for_rotated_content` | scene_render.rs |
| VAL-XFORM-014 | `bilinear_downscale_checkerboard_produces_gray`, `bilinear_downscale_checkerboard_no_aliased_pixels` | drawing.rs |
| VAL-XFORM-015 | `resampling_method_enum_exists` | drawing.rs |
| VAL-XFORM-016 | `diff_scenes_rotated_node_aabb_damage` | scene_render.rs |
| VAL-XFORM-017 | `diff_scenes_compound_transform_aabb` | scene_render.rs |
| VAL-XFORM-018 | `affine_transform_scale_zero_no_panic`, `scale_zero_no_output_no_panic` | scene.rs, scene_render.rs |
| VAL-XFORM-019 | `affine_transform_negative_scale_mirror` | scene.rs |
| VAL-XFORM-020 | `transform_plus_opacity_no_double_application` | scene_render.rs |
| VAL-XFORM-021 | `transform_does_not_affect_siblings` | scene_render.rs |
| VAL-XFORM-022 | `node_size_assertion_with_transform`, `node_size_compile_time_assertion_exists` | scene.rs, scene_render.rs |
| VAL-CROSS-005 | `dpi_scale_composes_with_affine_as_single_matrix` | scene_render.rs |
| VAL-CROSS-007 | `group_opacity_on_rotated_content` | scene_render.rs |
| VAL-CROSS-008 | `full_feature_composition` | scene_render.rs |

**Verification approach:**
1. Run full test suite to confirm all tests pass
2. For each assertion, run the specific test(s) with name filter
3. Read test source to confirm it verifies the claimed property
4. Record pass/fail per assertion
