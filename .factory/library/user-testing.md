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
- QEMU QMP `input-send-event` doesn't route to virtio-keyboard — must use monitor `sendkey` command
- QEMU framebuffer is 1× scale (not Retina) — visual quality assessment at 1× only
- Font loading requires `share/` directory with font files + 9p device

## Validation Concurrency

**Machine:** 48 GB RAM, 14 CPU cores, macOS (aarch64-apple-darwin).

**QEMU instances:** Each uses ~256 MB RAM + ~50 MB overhead. Max concurrent: **5** (1.5 GB total, well within 70% of ~30 GB available headroom).

**Unit tests:** Single-threaded (`--test-threads=1`) due to shared global state in some test modules. One test runner at a time.
