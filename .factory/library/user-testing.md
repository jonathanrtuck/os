# User Testing

Testing surface, validation approach, and resource classification for the font rendering pipeline mission.

## Validation Surface

**Primary surface: Host-side unit tests** — the main verification mechanism. Most font rendering behavior (shaping output, glyph cache, rasterizer, variable font parsing, perceptual calculations) is testable via pure-function unit tests on the host.

**Secondary surface: QEMU framebuffer** — for end-to-end display pipeline verification. Used to confirm text actually appears on screen after integration changes.

### QEMU Testing Workflow

```sh
# Build
cd /Users/user/Sites/os/system && cargo build --release

# Launch QEMU headless
pkill -f qemu-system-aarch64 2>/dev/null; sleep 1
rm -f /tmp/qemu-mon.sock /tmp/qemu-serial.log /tmp/qemu-screen.ppm /tmp/qemu-screen.png

qemu-system-aarch64 \
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

# Wait for boot
sleep 8

# Verify boot
cat /tmp/qemu-serial.log | grep -c "compositor"

# Send keystrokes
echo "sendkey h" | nc -U /tmp/qemu-mon.sock -w 1 >/dev/null 2>&1

# Capture screenshot
echo "screendump /tmp/qemu-screen.ppm" | nc -U /tmp/qemu-mon.sock -w 2 >/dev/null 2>&1
sleep 2
python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen.ppm').save('/tmp/qemu-screen.png')"

# View screenshot
# Use Read tool on /tmp/qemu-screen.png

# Kill QEMU
pkill -f qemu-system-aarch64
```

### What's verifiable via screenshots

- Text appears on screen (presence/absence)
- No visual corruption (garbled pixels, missing regions)
- No crash (serial output clean)
- Basic layout sanity (text not overlapping, positioned correctly)

### What's NOT reliably verifiable via screenshots

- Kerning quality (sub-pixel differences)
- Optical sizing differences (subtle)
- Weight correction (subtle luminance-dependent changes)
- Ligature correctness (hard to distinguish from screenshots)

These subtle behaviors are verified via host-side unit tests instead.

## Flow Validator Guidance: Host-side tests

**Testing tool:** Direct shell commands (`cargo test`, `cargo build`, `grep`).

**Isolation rules:**
- Each validator can freely run `cargo build --release` and grep commands (read-only, no conflicts).
- Only ONE validator at a time should run `cargo test -- --test-threads=1` to avoid Cargo lock contention.
- If a validator needs to run cargo test, it should use specific test name filters (e.g., `cargo test shaping -- --test-threads=1`) to minimize runtime.
- Font files in `system/share/` are read-only test fixtures — do not modify them.
- Test source files in `system/test/tests/` are read-only — do not modify them.

**What to check for each assertion:**
- Unit test assertions: Run `cargo test <filter> -- --test-threads=1` and verify the named test(s) pass.
- Grep assertions: Run grep/rg commands to verify code migration (presence/absence of patterns).
- Build assertions: Run `cargo build --release` and verify exit code 0.

**Working directory:** `/Users/user/Sites/os/system` for builds, `/Users/user/Sites/os/system/test` for tests.

## Flow Validator Guidance: QEMU framebuffer

**Testing tool:** QEMU monitor socket + screenshots. No browser automation needed.

**Isolation rules:**
- Only ONE QEMU instance at a time (uses fixed socket paths `/tmp/qemu-mon.sock`, `/tmp/qemu-serial.log`).
- Kill any existing QEMU before starting: `pkill -f qemu-system-aarch64 2>/dev/null; sleep 1`
- Clean up socket/log files before each test: `rm -f /tmp/qemu-mon.sock /tmp/qemu-serial.log /tmp/qemu-screen.ppm /tmp/qemu-screen.png`
- Always kill QEMU after testing: `pkill -f qemu-system-aarch64`

**QEMU launch command:** Use the `qemu` service definition in `.factory/services.yaml`.

**Screenshot workflow:**
1. Build: `cd /Users/user/Sites/os/system && cargo build --release`
2. Launch QEMU (see services.yaml `qemu.start`)
3. Wait 8-10 seconds for boot
4. Check serial log: `cat /tmp/qemu-serial.log`
5. Send keystrokes: `echo "sendkey h" | nc -U /tmp/qemu-mon.sock -w 1 >/dev/null 2>&1`
6. Wait 1-2s between keystrokes
7. Capture screenshot: `echo "screendump /tmp/qemu-screen.ppm" | nc -U /tmp/qemu-mon.sock -w 2 >/dev/null 2>&1`
8. Wait 2s, convert: `python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen.ppm').save('/tmp/qemu-screen.png')"`
9. View with Read tool on `/tmp/qemu-screen.png`
10. Kill QEMU: `pkill -f qemu-system-aarch64`

**Evidence:** Save screenshots as PNG files to the evidence directory. Save serial output snippets as text files.

## Validation Concurrency

**Machine:** 48 GB RAM, 14 CPU cores (Apple Silicon)

**Host-side tests:** Max concurrent validators: **5**. Tests are lightweight (~1.3s total, minimal memory). Limited by `--test-threads=1` requirement per test run, but multiple independent test runs can execute in parallel.

**QEMU instances:** Max concurrent validators: **2**. Each QEMU instance uses ~256 MB RAM. Multiple instances need unique socket/serial paths (configured per validator). Conservative limit due to bare-metal testing requiring exclusive hardware resource access per instance.

**Combined:** The user-testing validator should prioritize host-side unit test assertions (fast, reliable, high coverage) and use QEMU only for the ~5 assertions that require visual/integration verification (VAL-E2E-001, VAL-E2E-002, VAL-E2E-003, VAL-CROSS-001, VAL-CROSS-002).
