---
name: os-worker
description: Implements rendering pipeline features for the bare-metal AArch64 OS — drawing library, scene graph, compositor, core service, and kernel changes.
---

# OS Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

Features involving:
- Drawing library primitives (rounded rects, blur, transforms, anti-aliased lines, resampling)
- Scene graph types and double-buffer protocol (Node fields, Content variants, change list)
- Compositor rendering logic (render_node, damage tracking, frame scheduling, offscreen compositing)
- Core (OS service) scene building and event loop changes
- GPU driver transfer path changes
- Protocol/IPC message changes
- Kernel timer or syscall changes related to the rendering pipeline

## Work Procedure

### 1. Understand the Feature

Read the feature description, preconditions, expectedBehavior, and verificationSteps carefully.

Read the relevant source files. The rendering pipeline code lives in:
- `system/libraries/drawing/lib.rs` (+ `neon.rs`, `gamma_tables.rs`) — drawing primitives
- `system/libraries/scene/lib.rs` — scene graph types, double-buffer protocol
- `system/libraries/fonts/src/` — font rasterizer, glyph cache, shaping
- `system/services/compositor/` — thin event loop, frame scheduler, present signaling
- `system/services/core/` — OS service, scene building, event loop
- `system/services/drivers/virtio-gpu/` — GPU transfer
- `system/services/init/` — startup, config
- `system/libraries/protocol/` — IPC message types
- `system/kernel/` — syscalls, timer, scheduler (only if feature touches kernel)

Read `.factory/library/architecture.md` for the rendering pipeline overview and key types.

### 2. Write Tests First (TDD)

**For pure refactoring features** (where existing tests define the behavioral contract and the feature description says no new tests are needed), skip writing new tests. Instead, verify the baseline: run the full test suite to confirm all existing tests pass before making changes. The existing tests ARE your specification.

**For implementation features**, write failing tests before any implementation code in the appropriate test file:
- Drawing primitives → `system/test/tests/drawing.rs`
- NEON SIMD → `system/test/tests/neon.rs`
- Scene graph → `system/test/tests/scene.rs`
- Compositor rendering → `system/test/tests/scene_render.rs`
- Font/glyph → `system/test/tests/cache.rs` or `shaping.rs`
- Render backend → `system/test/tests/scene_render.rs`

Run `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1` to verify tests FAIL (red).

Follow existing test patterns in each file. Tests import library types directly via Cargo dependencies. For kernel code, tests use `#[path]` includes with stub dependencies.

### 3. Implement

Implement the feature to make tests pass (green).

**Critical conventions:**
- All `#[repr(C)]` structs in scene/lib.rs: if you add or change fields on Node, update the compile-time size assertion and verify both core and compositor agree on layout.
- `CompositorConfig` in protocol/src/lib.rs must fit in 60 bytes (IPC payload limit). There is a compile-time assertion. If adding fields, check the size.
- Before running QEMU visual tests, kill any stale QEMU instances from previous worker runs: `pkill -f "qemu-system-aarch64.*test-" 2>/dev/null || true`
- sRGB gamma-correct blending for ALL alpha compositing — use the existing `SRGB_TO_LINEAR` / `LINEAR_TO_SRGB` LUTs in drawing/lib.rs.
- NEON SIMD paths: write scalar reference first, then NEON optimization. Both must produce identical output.
- `unsafe` blocks require `// SAFETY:` comments. Inline asm: never use `nomem` without explicit ARM manual justification.
- Drawing library functions take `u32` or `i32` for physical coordinates. The compositor translates from logical (i16/u16 from scene graph) × scale factor.

### 4. Run Full Test Suite

```
cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1
```

All tests must pass — both your new tests and all existing ones. Fix any regressions before proceeding.

### 5. Build the Kernel

```
cd /Users/user/Sites/os/system && cargo build --release
```

Must compile cleanly. The kernel binary includes all services and libraries.

### 6. Visual Verification (if display pipeline changed)

If your feature affects anything visible on screen (compositor, drawing, core scene building):

```
cd /Users/user/Sites/os/system && bash test-qemu.sh
```

Or launch QEMU manually, send keystrokes, capture framebuffer screenshots:
```
# Launch QEMU (MUST include -fsdev/-device virtio-9p for font loading)
qemu-system-aarch64 \
    -machine virt,gic-version=2 -cpu cortex-a53 -smp 4 -m 256M \
    -global virtio-mmio.force-legacy=false \
    -drive "file=test.img,if=none,format=raw,id=hd0" \
    -device virtio-blk-device,drive=hd0 \
    -device virtio-gpu-device -device virtio-keyboard-device \
    -fsdev local,id=fsdev0,path=share,security_model=none \
    -device virtio-9p-device,fsdev=fsdev0,mount_tag=hostshare \
    -nographic \
    -serial file:/tmp/qemu-serial.log \
    -monitor unix:/tmp/qemu-mon.sock,server,nowait \
    -device "loader,file=virt.dtb,addr=0x40000000,force-raw=on" \
    -kernel target/aarch64-unknown-none/release/kernel &
QPID=$!
sleep 5

# Send keys and screenshot
echo "sendkey h" | nc -U /tmp/qemu-mon.sock -w 1 >/dev/null 2>&1
sleep 1
echo "screendump /tmp/qemu-screen.ppm" | nc -U /tmp/qemu-mon.sock -w 2 >/dev/null 2>&1
sleep 1
python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen.ppm').save('/tmp/qemu-screen.png')"
# View with Read tool

# Cleanup
kill $QPID 2>/dev/null
```

**View the screenshot with the Read tool.** Do not declare visual changes correct without seeing the pixels yourself.

### 7. Stress Test (if timing/scheduling/IPC changed)

For frame scheduler, timer, or double-buffer changes:
```
cd /Users/user/Sites/os/system/test && bash stress.sh
```

Verify no crash in serial output. Run for at least 60 seconds.

## Example Handoff

```json
{
  "salientSummary": "Implemented separable Gaussian blur in drawing library with NEON SIMD inner loop. Two-pass horizontal/vertical, capped at radius 16. Added blur_surface() function and BlurStrategy trait for future GPU path. 14 new tests in drawing.rs, all 1569 tests pass. QEMU screenshot confirms blurred shadow behind content panel.",
  "whatWasImplemented": "drawing::blur_surface(surface, radius, sigma) with scalar and NEON paths. BlurStrategy trait with CpuBlur implementation. Temporary buffer allocation via alloc::vec for the intermediate pass. Edge clamping at surface boundaries. Radius capped at 16 with graceful clamp for larger values.",
  "whatWasLeftUndone": "",
  "verification": {
    "commandsRun": [
      {"command": "cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1", "exitCode": 0, "observation": "1569 tests pass (14 new blur tests + 1555 existing)"},
      {"command": "cd /Users/user/Sites/os/system && cargo build --release", "exitCode": 0, "observation": "kernel builds cleanly"},
      {"command": "cd /Users/user/Sites/os/system && bash test-qemu.sh", "exitCode": 0, "observation": "QEMU boots, display pipeline functional, shadow visible behind panel"}
    ],
    "interactiveChecks": [
      {"action": "QEMU screenshot after boot", "observed": "Content panel has soft shadow with Gaussian falloff behind it. Shadow opacity decreases smoothly with distance. No hard edges."},
      {"action": "Typed 50 characters rapidly", "observed": "No visual glitches, text renders correctly, shadow stays in place during typing"}
    ]
  },
  "tests": {
    "added": [
      {"file": "system/test/tests/drawing.rs", "cases": [
        {"name": "blur_single_pixel_symmetric", "verifies": "VAL-BLUR-001"},
        {"name": "blur_matches_reference_2d", "verifies": "VAL-BLUR-002"},
        {"name": "blur_radius_zero_identity", "verifies": "VAL-BLUR-003"},
        {"name": "blur_edge_clamping_small_surface", "verifies": "VAL-BLUR-004"},
        {"name": "blur_large_radius_capped", "verifies": "VAL-BLUR-005"},
        {"name": "blur_neon_matches_scalar", "verifies": "VAL-BLUR-006"},
        {"name": "blur_sigma_varies_spread", "verifies": "VAL-BLUR-007"},
        {"name": "blur_preserves_alpha", "verifies": "VAL-BLUR-014"}
      ]}
    ]
  },
  "discoveredIssues": []
}
```

## When to Return to Orchestrator

- Feature requires changes to the kernel timer or syscall interface that aren't specified
- Scene graph Node size change breaks IPC payload constraints (CompositorConfig > 60 bytes)
- QEMU visual test shows unexpected behavior not explained by the feature's changes
- Existing test failures unrelated to the feature's changes
- Feature depends on code from a previous milestone that hasn't been implemented yet
- Memory allocation in compositor exceeds 32 MiB heap budget
