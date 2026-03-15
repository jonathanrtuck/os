# User Testing

Testing surface, validation approach, and resource cost classification.

---

## Validation Surface

**Primary surface:** Host-side unit tests
- Command: `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`
- 1555 tests across 56 files, all on macOS aarch64
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

## QEMU Test Scripts

- `test-qemu.sh` — interactive display pipeline test (safe, user's QEMU is closed)
- `test/smoke.sh` — boot smoke test, 17 assertions, 10s timeout
- `test/stress.sh` — headless stress test, 180s timeout
- `test/integration.sh` — full device pipeline, 15s timeout
- `test/crash.sh` — rapid input via AppleScript, 30s timeout
