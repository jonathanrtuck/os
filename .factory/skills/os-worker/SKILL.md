---
name: os-worker
description: Implements features for the bare-metal Document OS — drawing library, compositor, drivers, editor, and content decoders. TDD + QEMU visual verification.
---

# OS Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

All implementation features for the Document OS project:
- Drawing library changes (font rendering, alpha blending, rasterizer, PNG decoder, SVG renderer)
- Compositor changes (multi-surface, chrome, shadows, damage tracking)
- GPU driver changes (double buffering, damage-tracked transfer, resolution)
- Text editor changes (selection, scrolling)
- Init changes (process orchestration, memory allocation, asset loading)
- Polish/refinement work

## Work Procedure

### 1. Understand the Feature

Read the feature description, preconditions, expectedBehavior, and verificationSteps carefully. Identify:
- Which source files need to change (check `system/DESIGN.md` and `.factory/library/architecture.md` for component map)
- Whether this is primarily library work (drawing, decoding) or system work (compositor, init, editor)
- What existing tests cover adjacent functionality

### 2. Write Tests First (TDD)

**For library/algorithmic features** (font rendering, PNG decoder, SVG parser, blending, damage rects):
- Add test cases to the appropriate file in `system/test/tests/` (e.g., `drawing.rs`, or create new test files)
- Tests run on the host (`aarch64-apple-darwin`) via `cd system/test && cargo test -- --test-threads=1`
- Write the test, verify it fails (red), then implement to make it pass (green)
- Cover the expected behaviors AND error cases from the feature description

**For system/integration features** (compositor changes, init orchestration):
- Write unit tests for any testable logic (damage rect calculation, surface ordering, etc.)
- Some behaviors can only be verified visually — document what you'll check in QEMU

### 3. Implement

- Match the existing code style (Rust, bare-metal idioms, no_std, no external crates)
- All code runs on `aarch64-unknown-none` target — no standard library, no heap allocator beyond the kernel's
- Check `.factory/library/architecture.md` for the display pipeline architecture
- When modifying IPC messages, update both sender and receiver
- When modifying init's process setup, ensure handle numbering is consistent

### 4. Run Unit Tests

```bash
cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1
```

ALL tests must pass. If existing tests fail, investigate whether:
- Your change broke existing behavior (fix it)
- The test was testing behavior you intentionally replaced (update the test, document in handoff)

### 5. Build

```bash
cd /Users/user/Sites/os/system && cargo build --release
```

Must compile with zero errors and zero warnings.

### 6. Visual Verification (MANDATORY for display pipeline changes)

**Every feature that affects what appears on screen MUST be visually verified.**

```bash
# Kill any existing QEMU
pkill -f qemu-system-aarch64 2>/dev/null; sleep 1
rm -f /tmp/qemu-mon.sock /tmp/qemu-serial.log /tmp/qemu-screen.ppm /tmp/qemu-screen.png

# Launch QEMU headless
cd /Users/user/Sites/os/system && qemu-system-aarch64 \
    -machine virt,gic-version=2 \
    -cpu cortex-a53 -smp 4 -m 256M \
    -global virtio-mmio.force-legacy=false \
    -drive "file=test.img,if=none,format=raw,id=hd0" \
    -device virtio-blk-device,drive=hd0 \
    -device virtio-gpu-device -device virtio-keyboard-device \
    -fsdev "local,id=fsdev0,path=share,security_model=none" \
    -device "virtio-9p-device,fsdev=fsdev0,mount_tag=hostshare" \
    -nographic \
    -serial file:/tmp/qemu-serial.log \
    -monitor unix:/tmp/qemu-mon.sock,server,nowait \
    -device "loader,file=virt.dtb,addr=0x40000000,force-raw=on" \
    -kernel target/aarch64-unknown-none/release/kernel &

# Wait for boot
sleep 8

# Check serial log for errors
cat /tmp/qemu-serial.log

# Send keystrokes to exercise the feature
echo "sendkey h" | nc -U /tmp/qemu-mon.sock -w 1 >/dev/null 2>&1
sleep 1

# Take screenshot
echo "screendump /tmp/qemu-screen.ppm" | nc -U /tmp/qemu-mon.sock -w 2 >/dev/null 2>&1
sleep 2
python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen.ppm').save('/tmp/qemu-screen.png')"
```

Then use the **Read tool** on `/tmp/qemu-screen.png` to VIEW the screenshot. You MUST see the result yourself. Do not declare visual changes done without viewing the screenshot.

For features requiring high-resolution verification, use `xres=1920,yres=1080` in the virtio-gpu-device flags.

**Always kill QEMU when done:** `pkill -f qemu-system-aarch64`

### 7. Run Regression Tests

After visual verification:
```bash
cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1
```
Confirm all tests still pass.

### 8. Commit

Commit with a clear message describing what was implemented. Include test count in the message if new tests were added.

## Important Constraints

- **no_std**: No standard library. No `alloc` crate beyond the kernel's bump allocator. No external crate dependencies.
- **Integer math only**: The drawing library uses integer arithmetic throughout. No floating point (the target has no FPU guarantee). Use fixed-point (20.12 format is established).
- **Single-threaded userspace**: Each process is single-threaded. `static mut` is used for globals (technically UB but accepted pattern in this codebase).
- **IPC messages are 64 bytes**: 4-byte type + 60-byte payload. Messages larger than 60 bytes need a shared-memory reference pattern.
- **Font assets via 9p**: Put font files, PNG images, and SVG files in `system/share/`. They're loaded at runtime via the virtio-9p host filesystem passthrough.
- **Build embeds userspace**: `build.rs` compiles all userspace programs. Adding a new source file to an existing program just requires editing the `build.rs` compile command for that program. Adding a new library requires adding it to the rlib compilation list.

## Example Handoff

```json
{
  "salientSummary": "Implemented gamma-correct sRGB blending in the drawing library. Added srgb_to_linear/linear_to_srgb lookup tables and modified draw_coverage to blend in linear space. 6 new tests added (gamma curve accuracy, zero-coverage preservation, visual weight comparison). All 612 tests pass. QEMU screenshot confirms text strokes are visibly heavier and more legible than before.",
  "whatWasImplemented": "sRGB gamma correction in draw_coverage(): coverage values are now converted to linear space before blending, then converted back to sRGB for storage. Two 256-entry lookup tables (srgb_to_linear, linear_to_srgb) are computed at compile time. Zero-coverage fast path preserved. Also fixed a bug where the dirty rect calculation was off by one line height.",
  "whatWasLeftUndone": "",
  "verification": {
    "commandsRun": [
      {
        "command": "cd system/test && cargo test -- --test-threads=1",
        "exitCode": 0,
        "observation": "612 tests passed, 0 failed (6 new gamma tests + 606 existing)"
      },
      {
        "command": "cd system && cargo build --release",
        "exitCode": 0,
        "observation": "Clean build, no warnings"
      }
    ],
    "interactiveChecks": [
      {
        "action": "Booted QEMU, typed 'hello world' on two lines, took screenshot",
        "observed": "Text strokes are visibly heavier than pre-mission screenshots. Both lines have identical stroke weight. Thin stems in 'l' and 'i' are clearly visible. No artifacts around glyphs."
      },
      {
        "action": "Sent 30 rapid keystrokes, took screenshot",
        "observed": "All 30 characters present. No flicker visible in screenshot. Status bar shows correct count."
      }
    ]
  },
  "tests": {
    "added": [
      {
        "file": "system/test/tests/drawing.rs",
        "cases": [
          {"name": "test_srgb_to_linear_boundary_values", "verifies": "gamma lookup table correctness at 0, 128, 255"},
          {"name": "test_gamma_blend_zero_coverage_unchanged", "verifies": "zero coverage doesn't modify destination pixels"},
          {"name": "test_gamma_blend_full_coverage_replaces", "verifies": "full coverage replaces destination completely"},
          {"name": "test_gamma_blend_half_coverage_weight", "verifies": "50% coverage produces perceptually-correct midpoint"},
          {"name": "test_gamma_blend_produces_heavier_strokes", "verifies": "gamma blending at 50% coverage produces higher RGB values than linear"},
          {"name": "test_draw_coverage_uses_gamma", "verifies": "draw_coverage function applies gamma correction"}
        ]
      }
    ]
  },
  "discoveredIssues": [
    {
      "severity": "low",
      "description": "The GlyphCache allocates 95 * 48 * 48 = ~220KB for ASCII coverage maps. Adding a second font (proportional) will double this. May want to consider lazy glyph caching for the proportional font.",
      "suggestedFix": "Allocate proportional font cache lazily or with smaller max glyph dimensions since proportional fonts often have narrower glyphs."
    }
  ]
}
```

## When to Return to Orchestrator

- A feature requires changes to the IPC message protocol that affect multiple processes not listed in this feature's scope
- The document buffer (4 KiB shared page) is too small for the feature and needs to be enlarged (requires init + kernel changes)
- An existing bug unrelated to this feature is causing test failures
- The feature's preconditions are not met (e.g., a dependency feature hasn't been implemented yet)
- QEMU won't boot or crashes during testing in ways unrelated to this feature's changes
- The feature description is ambiguous about a design decision that could go multiple ways
