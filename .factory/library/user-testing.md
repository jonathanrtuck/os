# User Testing

Validation surface discovery and resource cost classification for mission validation.

## Validation Surface

**Primary surface:** QEMU framebuffer screenshots (PPM→PNG via PIL, viewed with Read tool).

**Secondary surface:** Host-side unit test results (cargo test output).

**Tools:**
- QEMU 10.2.1 (`qemu-system-aarch64`) for bare-metal kernel boot
- Python3 PIL for PPM→PNG conversion
- `nc` (netcat) for QEMU monitor socket commands (sendkey, screendump)

**Setup for visual testing:**
1. Build: `cd /Users/user/Sites/os/system && cargo build --release`
2. Generate DTB: requires `dtc` (device tree compiler) — `run-qemu.sh` handles this
3. Launch QEMU with virtio-gpu, virtio-keyboard, virtio-9p (for font loading), GICv3
4. Wait ~5s for boot
5. Send keystrokes via monitor socket
6. Capture framebuffer via `screendump` command
7. Convert PPM→PNG and view

**QEMU launch (headless, with font sharing):**
```sh
qemu-system-aarch64 \
    -machine virt,gic-version=3 -cpu cortex-a53 -smp 4 -m 256M \
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
```

**Known limitations:**
- QEMU QMP `input-send-event` doesn't route to virtio-keyboard in headless (`-nographic`) mode
- QEMU monitor `sendkey` command also does NOT route to virtio-keyboard in headless mode — interactive keystroke injection is not possible in headless/nographic mode. Workers can only verify initial-frame rendering; interactive testing (typing, cursor, selection) requires the QEMU display window
- QEMU framebuffer is 1× scale (not Retina) — visual quality assessment at 1× only
- Font loading requires `share/` directory with font files + 9p device

## Flow Validator Guidance: code-inspection

**Surface:** Source code files and build output.
**Tool:** Shell commands (`cargo build`, `rg` via Grep tool, Read tool for file inspection).
**Isolation:** Read-only. Multiple subagents can inspect code simultaneously.
**Boundaries:** Do not modify source files. Do not run tests (separate surface).

## Flow Validator Guidance: unit-tests

**Surface:** Host-side test suite output (`cargo test`).
**Tool:** `cargo test` with specific test name filters.
**Isolation:** Single-threaded test runner (`--test-threads=1`). Only ONE test runner at a time due to shared global state.
**Boundaries:** Do not modify source files or test files. Only run specific tests by name filter to verify assertions.

## Flow Validator Guidance: qemu-visual

**Surface:** QEMU framebuffer screenshots (PPM→PNG).
**Tool:** QEMU monitor socket (sendkey, screendump), Python3 PIL for conversion, Read tool for PNG viewing.
**Isolation:** Each QEMU instance on separate monitor socket and serial log file. Ensure different filenames per subagent.
**Boundaries:** Kill stale QEMU instances before starting. Use unique socket/log paths. Clean up QEMU process after testing.
**Known limitation:** `sendkey` does NOT route to virtio-keyboard in headless (`-nographic`) mode. Interactive keystroke injection is not possible. Subagents can only verify initial-frame rendering (clock, background, default text). For interactive testing (typing, cursor, selection), the initial boot frame must be assessed for elements visible from the boot state.

## Validation Concurrency

**Machine:** 48 GB RAM, 14 CPU cores, macOS (aarch64-apple-darwin).

**QEMU instances:** Each uses ~256 MB RAM + ~50 MB overhead. Max concurrent: **5** (1.5 GB total, well within 70% of ~30 GB available headroom).

**Unit tests:** Single-threaded (`--test-threads=1`) due to shared global state in some test modules. One test runner at a time.
