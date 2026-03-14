---
name: font-rendering-worker
description: Implements font rendering pipeline features — shaping library, rasterizer, scene graph, variable fonts, perceptual rendering. TDD + QEMU visual verification.
---

# Font Rendering Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

All font rendering pipeline features:
- Build system changes for Cargo-managed library dependencies
- Shaping library (HarfRust integration)
- Rasterizer adaptation (read-fonts glyph outlines)
- Scene graph evolution (shaped glyph TextRuns)
- Glyph cache redesign (LRU, glyph-ID-keyed)
- Variable font support (axis parsing, interpolation)
- Font fallback and Unicode coverage
- Perceptual rendering (optical sizing, weight correction)
- Content-type-aware typography defaults
- Core service and compositor integration

## Key Technical Context

**HarfRust** (`harfrust` crate, v0.5) is a pure Rust port of HarfBuzz. Official HarfBuzz project. First-class `no_std + alloc` support via `default-features = false`. Uses `read-fonts` for font parsing (zero-copy, supports all OpenType tables including variable fonts).

**Build system constraint:** The bare-metal build uses `build.rs` with direct `rustc` invocations for each library. Libraries with Cargo dependency trees (like harfrust) need Cargo-based compilation for the `aarch64-unknown-none` target. The build system feature must solve this.

**Existing code to replace:** The custom TrueType parser in `truetype.rs` handles cmap format 4, GPOS pair adjustment, glyph outline extraction, and scanline rasterization. HarfRust + read-fonts replaces cmap and GPOS. The scanline rasterizer algorithm (~500 lines) is KEPT — only its input source changes (read-fonts outlines instead of custom parser).

**Scene graph:** `TextRun` in `libraries/scene/lib.rs` currently carries raw UTF-8 bytes. A `ShapedGlyph` struct is already stubbed but unused. TextRun must evolve to carry arrays of ShapedGlyph (glyph_id + fractional positions). The data buffer (64 KiB) is ample (~8K glyphs fit).

**Variable fonts:** Variable Nunito Sans (opsz, wght, wdth, YTLC axes, 556 KB) is available. read-fonts handles fvar/gvar/avar parsing and glyph interpolation — no custom implementation needed.

**Reference documents:**
- `design/research-font-rendering.md` — full research and design plan
- `system/DESIGN.md` — system architecture, component status, dependency map
- `.factory/library/architecture.md` — display pipeline architecture
- `.factory/library/font-rendering.md` — HarfRust API reference and integration guide

## Work Procedure

### 1. Understand the Feature

Read the feature description, preconditions, expectedBehavior, and verificationSteps. Check:
- Which source files need changes (consult `system/DESIGN.md` §1.3 for drawing library, §2.2 for compositor)
- Whether this is library work (host-testable) or integration work (needs QEMU)
- What existing tests cover adjacent functionality in `system/test/tests/drawing.rs` and `system/test/tests/scene.rs`
- Read `.factory/library/font-rendering.md` for HarfRust API details

### 2. Write Tests First (TDD)

**For library features** (shaping, rasterizer, glyph cache, variable fonts, perceptual math):
- Add test cases to the appropriate file in `system/test/tests/` (primarily `drawing.rs` and `scene.rs`, or create new files like `shaping.rs`)
- Tests run on the host: `cd system/test && cargo test -- --test-threads=1`
- Write the test, verify it FAILS (red), then implement to make it pass (green)
- Cover expected behaviors from the feature's `expectedBehavior` array
- Font files for tests: use `include_bytes!("../../share/source-code-pro.ttf")` and similar patterns (see existing drawing.rs tests)

**For integration features** (core service, compositor, end-to-end):
- Write unit tests for any testable logic (shaping output verification, glyph cache behavior)
- Document what you'll verify visually in QEMU

### 3. Implement

Key conventions for this mission:
- **no_std + alloc is OK.** The shaping library uses `alloc` (Vec, Box). This is acceptable — the OS has a heap allocator.
- **Float math is OK in the shaping library.** HarfRust uses floats internally via `core_maths`. The rasterizer should continue using fixed-point (20.12) for its internal math.
- **Kill the old way.** When HarfRust replaces custom code (cmap lookup, GPOS kerning), DELETE the old code. No parallel implementations.
- **Preserve the rasterizer.** The scanline rasterizer algorithm in `rasterizer.rs` is kept. Only change its input: accept glyph outlines from read-fonts instead of from the custom parser.
- **Match existing style.** Rust bare-metal idioms, descriptive names, SAFETY comments on every unsafe block.
- **IPC changes:** If modifying protocol messages, update `libraries/protocol/` and both sender and receiver.

### 4. Run Tests

```bash
cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1
```

ALL tests must pass. If existing tests fail:
- Your change broke existing behavior → fix it
- The test tested behavior you intentionally replaced (e.g., old cmap lookup) → update the test, note in handoff

### 5. Build

```bash
cd /Users/user/Sites/os/system && cargo build --release
```

Zero errors, zero warnings.

### 6. Visual Verification (for display pipeline changes)

**MANDATORY** when the feature affects what appears on the QEMU display (compositor, core service rendering, scene graph text path).

```bash
pkill -f qemu-system-aarch64 2>/dev/null; sleep 1
rm -f /tmp/qemu-mon.sock /tmp/qemu-serial.log /tmp/qemu-screen.ppm /tmp/qemu-screen.png

cd /Users/user/Sites/os/system && qemu-system-aarch64 \
    -machine virt,gic-version=2 -cpu cortex-a53 -smp 4 -m 256M \
    -rtc base=localtime \
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

sleep 8
cat /tmp/qemu-serial.log

echo "sendkey h" | nc -U /tmp/qemu-mon.sock -w 1 >/dev/null 2>&1
sleep 1
echo "screendump /tmp/qemu-screen.ppm" | nc -U /tmp/qemu-mon.sock -w 2 >/dev/null 2>&1
sleep 2
python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen.ppm').save('/tmp/qemu-screen.png')"
```

Use the **Read tool** on `/tmp/qemu-screen.png` to VIEW the screenshot. You MUST see the result yourself.

**Always kill QEMU when done:** `pkill -f qemu-system-aarch64`

### 7. Commit

Commit with a message describing what was implemented. Include new test count if tests were added.

## Example Handoff

```json
{
  "salientSummary": "Created the shaping library at libraries/shaping/ with HarfRust integration. API: ShapedText::shape(font_data, text, features) returns Vec<ShapedGlyph> with glyph IDs and positions. Modified build.rs to compile Cargo-managed libraries for bare-metal target. 12 new tests covering Latin shaping, ligatures, kerning, and feature control. All 1,363 tests pass.",
  "whatWasImplemented": "New libraries/shaping/ crate with harfrust dependency (no_std + alloc). ShapedGlyph struct: { glyph_id: u16, x_advance: i32, y_advance: i32, x_offset: i32, y_offset: i32, cluster: u32 }. shape() function wraps harfrust's UnicodeBuffer + Shaper pipeline. build.rs updated to use `cargo build --target aarch64-unknown-none` for libraries with Cargo deps. Test crate updated with shaping as dev-dependency.",
  "whatWasLeftUndone": "",
  "verification": {
    "commandsRun": [
      {
        "command": "cd system/test && cargo test -- --test-threads=1",
        "exitCode": 0,
        "observation": "1,363 tests passed (12 new shaping tests + 1,351 existing)"
      },
      {
        "command": "cd system && cargo build --release",
        "exitCode": 0,
        "observation": "Clean build including new shaping library, no warnings"
      }
    ],
    "interactiveChecks": []
  },
  "tests": {
    "added": [
      {
        "file": "system/test/tests/shaping.rs",
        "cases": [
          {"name": "test_shape_hello_world", "verifies": "Basic Latin text produces correct glyph count and non-zero advances"},
          {"name": "test_shape_ligatures_fi_fl", "verifies": "fi/fl ligatures produce fewer glyphs than input characters"},
          {"name": "test_shape_kerning_av", "verifies": "AV kerning produces different advance than sum of individual advances"},
          {"name": "test_shape_feature_liga_on_off", "verifies": "Enabling/disabling liga feature changes output glyphs"}
        ]
      }
    ]
  },
  "discoveredIssues": []
}
```

## When to Return to Orchestrator

- The build system change for Cargo-managed dependencies requires kernel or init modifications beyond what's described
- HarfRust fails to compile for aarch64-unknown-none (dependency issue, missing no_std support in a transitive dep)
- A feature's preconditions are not met (previous feature's output not available)
- IPC message protocol changes are needed that affect processes outside this feature's scope
- QEMU won't boot or crashes in ways unrelated to this feature
- The feature description is ambiguous about a design decision with multiple valid approaches
- Font files are missing or corrupted
