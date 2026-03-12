# User Testing

Testing surface, tools, setup steps, and known quirks for manual/visual testing.

**What belongs here:** How to test the running OS, screenshot workflow, known quirks, isolation notes.

---

## Testing Surface

The only user-facing surface is the **QEMU framebuffer**. Testing means:
1. Boot the OS in QEMU (headless mode with monitor socket)
2. Send keystrokes via `sendkey` on the monitor socket
3. Capture framebuffer screenshots via `screendump`
4. Convert PPM→PNG and view with the Read tool

## Screenshot Workflow

```bash
# Prerequisites: QEMU running with monitor socket at /tmp/qemu-mon.sock

# Send a keystroke
echo "sendkey h" | nc -U /tmp/qemu-mon.sock -w 1 >/dev/null 2>&1
sleep 1

# Capture screenshot
echo "screendump /tmp/qemu-screen.ppm" | nc -U /tmp/qemu-mon.sock -w 2 >/dev/null 2>&1
sleep 2

# Convert and view
python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen.ppm').save('/tmp/qemu-screen.png')"
# Use Read tool on /tmp/qemu-screen.png
```

## Timing Notes

- 8 second boot wait is reliable
- 1 second between keystrokes is reliable
- 2 seconds after screendump before reading the PPM file
- Multiple rapid sendkeys may need cumulative sleep

## Known Quirks

- `sendkey` via monitor socket goes to PS/2 emulation, but virtio-keyboard still receives it in QEMU 10.2.1
- Serial output is interleaved from concurrent driver processes (normal)
- The 9p share directory (`system/share/`) MUST be included in the QEMU command for font loading
- QEMU monitor commands are case-sensitive

## Key Names for sendkey

Common: `a`-`z`, `0`-`9`, `spc` (space), `ret` (enter), `backspace`, `tab`, `shift` (modifier), `shift-a` (capital A), `left`, `right`, `up`, `down`, `esc`

## Flow Validator Guidance: QEMU Framebuffer

### Isolation Rules

Each flow validator subagent gets its own QEMU instance with unique socket/log paths:
- **Monitor socket:** `/tmp/qemu-mon-{flow-id}.sock`
- **Serial log:** `/tmp/qemu-serial-{flow-id}.log`
- **Screenshot PPM:** `/tmp/qemu-screen-{flow-id}.ppm`
- **Screenshot PNG:** `/tmp/qemu-screen-{flow-id}.png`

### QEMU Launch Template (Standard 1024x768)

```bash
cd /Users/user/Sites/os/system && \
pkill -f "qemu.*mon-{FLOW_ID}" 2>/dev/null; sleep 1; \
rm -f /tmp/qemu-mon-{FLOW_ID}.sock /tmp/qemu-serial-{FLOW_ID}.log; \
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
  -serial file:/tmp/qemu-serial-{FLOW_ID}.log \
  -monitor unix:/tmp/qemu-mon-{FLOW_ID}.sock,server,nowait \
  -device "loader,file=virt.dtb,addr=0x40000000,force-raw=on" \
  -kernel target/aarch64-unknown-none/release/kernel &
```

### QEMU Launch Template (1920x1080)

Same as above but with `-device virtio-gpu-device,xres=1920,yres=1080` instead of `-device virtio-gpu-device`.

### Keystroke + Screenshot Template

```bash
# Send keystroke
echo "sendkey {key}" | nc -U /tmp/qemu-mon-{FLOW_ID}.sock -w 1 >/dev/null 2>&1
sleep 1

# Capture screenshot
echo "screendump /tmp/qemu-screen-{FLOW_ID}.ppm" | nc -U /tmp/qemu-mon-{FLOW_ID}.sock -w 2 >/dev/null 2>&1
sleep 2
python3 -c "from PIL import Image; Image.open('/tmp/qemu-screen-{FLOW_ID}.ppm').save('/tmp/qemu-screen-{FLOW_ID}.png')"
```

### Cleanup Template

```bash
# Kill by matching the flow-specific socket path
pkill -f "qemu.*mon-{FLOW_ID}" 2>/dev/null
rm -f /tmp/qemu-mon-{FLOW_ID}.sock /tmp/qemu-serial-{FLOW_ID}.log
rm -f /tmp/qemu-screen-{FLOW_ID}.ppm /tmp/qemu-screen-{FLOW_ID}.png
```

### Shared Resources (Off-Limits for Modification)

- `system/share/` — shared asset directory, read-only during testing
- `test.img` — block device image, read-only during testing
- Source code — do NOT modify source code during validation
- Build artifacts — do NOT rebuild during testing; all validators share the same pre-built binary

### Boundaries

- Each QEMU instance is fully isolated via unique sockets/logs
- Multiple QEMU instances CAN run in parallel (different socket paths)
- **However**: QEMU instances share the same `test.img` file in read-only mode — this is safe for parallel reads
- **However**: Ensure each QEMU PID is tracked so cleanup works correctly
