#!/usr/bin/env bash
# Automated crash test: boots the kernel, sends rapid keyboard input to the
# QEMU display window via AppleScript (macOS), and checks serial output.
#
# IMPORTANT: QEMU monitor `sendkey` does NOT route to virtio-keyboard.
# We must send real keystrokes to the QEMU window via the OS input system.
#
# Usage: ./crash-test.sh [duration_seconds]
#   Default duration: 30 seconds of rapid typing.
#   Exit 0 = no crash (pass), exit 1 = crash detected.

set -euo pipefail

DURATION="${1:-30}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL="${SCRIPT_DIR}/target/aarch64-unknown-none/release/kernel"
DTB_FILE="${SCRIPT_DIR}/virt.dtb"
DISK_IMG="${SCRIPT_DIR}/test.img"
SERIAL_LOG="/tmp/os-crash-test-serial-$$.log"

cleanup() {
    kill "$QEMU_PID" 2>/dev/null || true
    rm -f "$SERIAL_LOG"
}
trap cleanup EXIT

# Build.
echo "Building..."
cargo build --release 2>&1 | tail -1

if [ ! -f "$KERNEL" ]; then
    echo "ERROR: kernel not found"
    exit 2
fi

# Create test disk if needed.
if [ ! -f "$DISK_IMG" ]; then
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=1 2>/dev/null
    echo -n "HELLO VIRTIO BLK" | dd of="$DISK_IMG" bs=1 count=16 conv=notrunc 2>/dev/null
fi

# Generate DTB if needed.
if [ ! -f "$DTB_FILE" ]; then
    qemu-system-aarch64 \
        -machine "virt,gic-version=2,dumpdtb=${DTB_FILE}" \
        -cpu cortex-a53 -smp 4 -m 256M -nographic 2>/dev/null
    dd if="$DTB_FILE" of="${DTB_FILE}.trim" bs=65536 count=1 2>/dev/null
    printf '\x00\x01\x00\x00' | dd of="${DTB_FILE}.trim" bs=1 seek=4 count=4 conv=notrunc 2>/dev/null
    mv "${DTB_FILE}.trim" "$DTB_FILE"
fi

# Kill any lingering QEMU.
pkill -f "qemu-system-aarch64.*${DISK_IMG}" 2>/dev/null && sleep 0.2 || true

echo "Starting QEMU with display window (${DURATION}s test)..."

# Launch QEMU WITH a display window (required for virtio-keyboard input).
qemu-system-aarch64 \
    -machine "virt,gic-version=2" \
    -cpu cortex-a53 -smp 4 -m 256M \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$DISK_IMG,if=none,format=raw,id=hd0" \
    -device virtio-blk-device,drive=hd0 \
    -device virtio-gpu-device \
    -device virtio-keyboard-device \
    -serial "file:${SERIAL_LOG}" \
    -device "loader,file=$DTB_FILE,addr=0x40000000,force-raw=on" \
    -kernel "$KERNEL" &
QEMU_PID=$!

# Wait for boot.
echo -n "Waiting for boot"
for i in $(seq 1 30); do
    if [ -f "$SERIAL_LOG" ] && grep -q "booted" "$SERIAL_LOG" 2>/dev/null; then
        echo " OK"
        break
    fi
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        echo " QEMU exited early"
        cat "$SERIAL_LOG" 2>/dev/null || true
        exit 2
    fi
    echo -n "."
    sleep 0.5
done

if ! grep -q "booted" "$SERIAL_LOG" 2>/dev/null; then
    echo " TIMEOUT"
    cat "$SERIAL_LOG" 2>/dev/null || true
    exit 2
fi

sleep 2  # Wait for services to start.

# Send keystrokes to the QEMU window via AppleScript.
# This sends REAL keyboard events through the OS → QEMU → virtio-keyboard.
echo "Sending keyboard input to QEMU window for ${DURATION}s..."
python3 -u - "$DURATION" "$SERIAL_LOG" "$QEMU_PID" <<'PYTHON'
import subprocess, sys, time, os, signal

duration = int(sys.argv[1])
serial_path = sys.argv[2]
qemu_pid = int(sys.argv[3])

# AppleScript to send a burst of keystrokes to QEMU.
# keystroke sends the characters; we target the frontmost QEMU window.
def send_keys(chars):
    script = f'''
    tell application "System Events"
        set qemu to first process whose unix id is {qemu_pid}
        tell qemu
            set frontmost to true
            keystroke "{chars}"
        end tell
    end tell
    '''
    subprocess.run(["osascript", "-e", script],
                   capture_output=True, timeout=5)

def check_crash():
    try:
        with open(serial_path, 'r', errors='replace') as f:
            content = f.read()
        if 'panicking' in content or 'BUG:' in content:
            return content
    except FileNotFoundError:
        pass
    return None

start = time.time()
keys_sent = 0
last_report = start

# Bring QEMU to front first.
try:
    send_keys("")
except:
    pass
time.sleep(0.5)

while time.time() - start < duration:
    # Check if QEMU is still running.
    try:
        os.kill(qemu_pid, 0)
    except ProcessLookupError:
        break

    # Send a burst of characters.
    try:
        send_keys("abcdefghijklmnop")
        keys_sent += 16
    except:
        pass

    time.sleep(0.05)  # ~320 keys/sec

    # Check for crash.
    crash = check_crash()
    if crash:
        elapsed = time.time() - start
        print(f"\nCRASH after {keys_sent} keys ({elapsed:.1f}s)")
        in_crash = False
        for line in crash.split('\n'):
            if '💥' in line or 'kernel' in line.lower() or 'BUG' in line:
                in_crash = True
            if in_crash:
                print(f"  {line.rstrip()}")
        sys.exit(1)

    now = time.time()
    if now - last_report > 5:
        last_report = now
        print(f"  {now - start:.0f}s: {keys_sent} keys sent, no crash")

# Final check.
time.sleep(1)
crash = check_crash()
if crash:
    print(f"\nCRASH after {keys_sent} keys (cooldown)")
    for line in crash.split('\n'):
        if '💥' in line or 'kernel' in line.lower() or 'BUG' in line:
            print(f"  {line.rstrip()}")
    sys.exit(1)

print(f"\nPASS: {keys_sent} keys over {duration}s, no crash")
sys.exit(0)
PYTHON
RESULT=$?

echo "---"
if grep -q "panicking\|BUG:" "$SERIAL_LOG" 2>/dev/null; then
    echo "Serial log:"
    tail -20 "$SERIAL_LOG"
    exit 1
fi

tail -3 "$SERIAL_LOG" 2>/dev/null || true
echo "---"
exit $RESULT
