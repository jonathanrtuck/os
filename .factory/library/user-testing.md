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
