# User Testing

## Validation Surface

**Primary surface:** QEMU framebuffer via screendump command.

The OS boots in QEMU virt machine (aarch64, 4 SMP cores) and displays a text editor UI with:
- Chrome title bar with document icon, title text, clock
- Shadow below title bar
- Document content area with monospace text
- Blinking cursor
- Text selection highlighting (if active)

**Tool:** QEMU monitor socket + screendump command -> PPM -> PNG -> Read tool for visual inspection.

**Setup:** `cd system && cargo build --release` then launch QEMU with the test-qemu.sh script or equivalent manual command.

**Limitation:** QEMU's QMP sendkey does NOT route to virtio-keyboard. For input testing, must type into the QEMU display window (macOS only via AppleScript in crash.sh).

**Critical:** QEMU boot requires 9p filesystem share (`-fsdev` + `virtio-9p-device`) for font loading. Without it, compositor reports 'no font data' and display is blank. Always reference `system/test-qemu.sh` for the full QEMU command with all required flags.

## Validation Concurrency

**Max concurrent validators:** 5

**Rationale:** No dev servers or browser instances needed. Each validator only needs the Rust compiler (already cached) and optionally QEMU (~200MB). On a 48GB/14-core machine with ~6GB baseline usage, headroom is ~29GB * 0.7 = ~20GB. 5 concurrent validators would use ~1GB total. Well within budget.

## Testing Notes

- This is a refactoring/optimization mission. The primary validation is "does it still compile, pass tests, and produce identical visual output."
- Visual verification (QEMU boot + screenshot) is needed for features that change display output and at milestone boundaries.
- Per-feature validation is compilation + test suite passing.
- For drawing optimizations: pixel-identical comparison tests (reference output from safe implementation vs optimized output).
- NEON SIMD: ±1 LSB tolerance for alpha blending, exact match for fill_rect.

## Flow Validator Guidance: CLI

**Testing surface:** Shell commands (grep, ls, file inspection) against the codebase at `/Users/user/Sites/os/system/`.

**Tool:** Direct shell execution via the Execute tool. No browser or TUI skill needed.

**Isolation rules:**
- All validation is **read-only**. Do not modify any source files.
- Do not run `cargo build` or `cargo test` — the parent validator has already confirmed build (exit 0) and tests (1555 pass, 0 fail). Your job is to verify structural assertions via grep/ls/file inspection.
- Stay within `/Users/user/Sites/os/system/` for all checks.

**Evidence collection:** For each assertion, capture the command output as evidence. Write it to your flow report JSON.

**Assertion verdict criteria:**
- `pass`: The grep/ls/file check matches the assertion's evidence specification exactly.
- `fail`: The check does NOT match — e.g., a pattern that should return 0 matches returns >0, or a file that should exist doesn't.
- `blocked`: Cannot evaluate (e.g., file doesn't exist that should, prerequisite broken).

**Important paths:**
- `system/build.rs` — build orchestration
- `system/libraries/fonts/` — renamed from shaping
- `system/libraries/scene/lib.rs` — scene data types
- `system/libraries/drawing/` — pixel primitives
- `system/services/core/` — Core OS service (main.rs, scene_state.rs)
- `system/services/compositor/` — compositor service (main.rs, scene_render.rs)
- `system/test/` — test crate (Cargo.toml, tests/*.rs)

## Flow Validator Guidance: QEMU Visual

**Testing surface:** QEMU framebuffer via monitor socket screendump.

**Tool:** QEMU monitor socket + screendump → PPM → PNG → Read tool. No browser or TUI skill needed.

**Setup:**
The parent validator has already built the kernel (`cargo build --release`). The subagent should:
1. Kill any stale QEMU processes: `pkill -f "qemu-system-aarch64" 2>/dev/null || true`
2. Clean up stale sockets: `rm -f /tmp/qemu-mon.sock /tmp/qemu-serial.log`
3. Launch QEMU using the test-qemu.sh script at `/Users/user/Sites/os/system/test-qemu.sh`
4. Use `--boot-only` mode first to verify boot, then send keys manually via monitor socket
5. Capture screenshots: `echo "screendump /tmp/qemu-screen.ppm" | nc -U /tmp/qemu-mon.sock -w 2`
6. Convert: `python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen.ppm').save('/tmp/qemu-screen.png')"`
7. View with Read tool on the PNG file
8. Kill QEMU when done: `pkill -f "qemu-system-aarch64" 2>/dev/null || true`

**Isolation rules:**
- Only ONE QEMU instance at a time (exclusive display resources).
- Do not modify any source files.
- Do not run `cargo build` — already done by parent.
- Kill QEMU before exiting.

**Evidence collection:** Save screenshots as PNG files in the evidence directory. For each assertion, describe what was observed in the screenshot.

**Assertion verdict criteria:**
- `pass`: Screenshot shows expected visual output (text, cursor, clock, etc.) matching the assertion description.
- `fail`: Screenshot does not match — missing content, visual artifacts, wrong rendering.
- `blocked`: QEMU fails to boot or screenshot cannot be captured.

**Expected visual elements:**
- Title bar: document icon (left), "Untitled" text (center-left), clock "HH:MM" (right)
- Shadow: horizontal gradient below title bar
- Content area: white/light background with monospace text in black
- Cursor: visible blinking vertical bar
- After typing: characters appear left-to-right, cursor advances
