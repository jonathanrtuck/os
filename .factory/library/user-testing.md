# User Testing Guide — Document OS

## Testing Surface

QEMU framebuffer screenshots. No browser, no web UI. The OS boots as a bare-metal kernel in QEMU `virt` machine.

## Environment Setup

### Prerequisites
- Rust nightly with `aarch64-unknown-none` target
- `qemu-system-aarch64` installed
- Python 3 with PIL/Pillow for PPM→PNG conversion

### Build
```bash
cd /Users/user/Sites/os/system && cargo build --release
```

### Unit Tests
```bash
cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1
```

### QEMU Services
Services are defined in `.factory/services.yaml`. Three QEMU configs:
- `qemu` — default 1280x800 display, keyboard only
- `qemu-hires` — 1920x1080 display
- `qemu-mouse` — adds `virtio-tablet-device` for mouse support

### QEMU Launch (default)
```bash
cd /Users/user/Sites/os/system && \
pkill -f qemu-system-aarch64 2>/dev/null; sleep 1; \
rm -f /tmp/qemu-mon.sock /tmp/qemu-serial.log; \
qemu-system-aarch64 \
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
```

### Boot Wait
Wait **8 seconds** after launch for boot to complete. Check `grep -q "compositor" /tmp/qemu-serial.log` for readiness.

### Sending Keystrokes
```bash
echo "sendkey {key}" | nc -U /tmp/qemu-mon.sock -w 1 >/dev/null 2>&1
```
Keys: `a`-`z`, `0`-`9`, `spc` (space), `ret` (enter), `backspace`, `shift-a` (uppercase A), `ctrl-tab`, etc.

**Important:** Add a `sleep 0.3` between keystrokes for reliable processing.

### Capturing Screenshots
```bash
echo "screendump /tmp/qemu-screen.ppm" | nc -U /tmp/qemu-mon.sock -w 2 >/dev/null 2>&1
sleep 2
python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen.ppm').save('/tmp/qemu-screen.png')"
```
View with the Read tool on `/tmp/qemu-screen.png`.

### Serial Log
```bash
cat /tmp/qemu-serial.log
```

### Teardown
```bash
pkill -f qemu-system-aarch64 2>/dev/null
```

## Flow Validator Guidance: QEMU Display

### Isolation Rules
- Only one QEMU instance can run at a time (shared monitor socket at `/tmp/qemu-mon.sock`)
- Flow validators must NOT run in parallel — they share the QEMU framebuffer and serial log
- Each flow validator must: start QEMU → test → stop QEMU → report
- Clear `/tmp/qemu-serial.log` between test runs

### Testing Boundaries
- Screenshots are the primary evidence for visual assertions
- Serial log is primary evidence for performance/transfer assertions
- The Read tool can inspect PNG files for pixel-level verification
- For pixel inspection, use Python to extract specific pixel values from the PPM/PNG

### Timing
- Boot: 8 seconds minimum
- Between keystrokes: 0.3 seconds minimum
- After last keystroke before screenshot: 2 seconds
- After screendump command: 2 seconds before converting PPM

### Known Quirks
- `screendump` saves PPM format, needs PIL conversion to PNG
- QEMU monitor socket commands need `nc -U` (Unix socket)
- Serial log is append-only within a session; clear between runs by restarting QEMU
- GPU transfer messages in serial log show rect dimensions for dirty-rect verification
