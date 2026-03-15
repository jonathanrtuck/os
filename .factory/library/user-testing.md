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

## Validation Concurrency

**Max concurrent validators:** 5

**Rationale:** No dev servers or browser instances needed. Each validator only needs the Rust compiler (already cached) and optionally QEMU (~200MB). On a 48GB/14-core machine with ~6GB baseline usage, headroom is ~29GB * 0.7 = ~20GB. 5 concurrent validators would use ~1GB total. Well within budget.

## Testing Notes

- This is a refactoring mission. The primary validation is "does it still compile, pass tests, and produce identical visual output."
- Visual verification (QEMU boot + screenshot) is needed only at milestone boundaries, not per-feature.
- Per-feature validation is compilation + test suite passing.
