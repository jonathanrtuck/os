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
- Do not run `cargo build` or `cargo test` — the parent validator has already confirmed build (exit 0) and tests (1462 pass, 0 fail). Your job is to verify structural assertions via grep/ls/file inspection.
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
