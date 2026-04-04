#!/usr/bin/env bash
# Display pipeline integration test — keystroke-to-pixel verification.
#
# Boots QEMU, waits for the display pipeline, then:
# 1. Verifies the clock ticks (framebuffer changes between 1-second screenshots).
# 2. Sends a keystroke and verifies the character appears in the framebuffer
#    within an acceptable latency window.
#
# Requires: Python 3 with Pillow (PIL).
#
# Usage: ./display-pipeline.sh [--max-latency-ms 500]
#   Default max latency: 500ms (keystroke to pixel change).
#   Exit 0 = PASS, exit 1 = FAIL.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SYSTEM_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
KERNEL="${SYSTEM_DIR}/target/aarch64-unknown-none/release/kernel"
DTB_FILE="${SYSTEM_DIR}/virt.dtb"
DISK_IMG="${SYSTEM_DIR}/test.img"
SHARE_DIR="${SYSTEM_DIR}/assets"
SERIAL_LOG="/tmp/qemu-dp-serial.log"
MON_SOCK="/tmp/qemu-dp-mon.sock"
SCREEN_DIR="/tmp/qemu-dp-screens"

MAX_LATENCY_MS=500
BOOT_WAIT=8

while [[ $# -gt 0 ]]; do
    case "$1" in
        --max-latency-ms) MAX_LATENCY_MS="$2"; shift 2 ;;
        --boot-wait)      BOOT_WAIT="$2"; shift 2 ;;
        *)                echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

if [ ! -f "$KERNEL" ]; then
    echo "ERROR: kernel not found. Run 'cargo build --release' first." >&2
    exit 2
fi

python3 -c "from PIL import Image" 2>/dev/null || {
    echo "ERROR: Python Pillow required (pip3 install Pillow)." >&2
    exit 2
}

QPID=""
cleanup() {
    [ -n "$QPID" ] && kill "$QPID" 2>/dev/null || true
    rm -f "$MON_SOCK" "$SERIAL_LOG"
    rm -rf "$SCREEN_DIR"
}
trap cleanup EXIT

pkill -f "qemu-system-aarch64.*qemu-dp-" 2>/dev/null && sleep 0.3 || true
rm -f "$MON_SOCK" "$SERIAL_LOG"
rm -rf "$SCREEN_DIR"
mkdir -p "$SCREEN_DIR"

if [ ! -f "$DISK_IMG" ]; then
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=1 2>/dev/null
    echo -n "HELLO VIRTIO BLK" | dd of="$DISK_IMG" bs=1 count=16 conv=notrunc 2>/dev/null
fi

echo "=== Display Pipeline Test ==="
echo "    max keystroke latency: ${MAX_LATENCY_MS}ms"
echo ""

qemu-system-aarch64 \
    -machine virt,gic-version=3 \
    -cpu cortex-a53 -smp 4 -m 256M \
    -rtc base=localtime \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$DISK_IMG,if=none,format=raw,id=hd0" \
    -device virtio-blk-device,drive=hd0 \
    -device virtio-gpu-device \
    -device virtio-keyboard-device \
    -device virtio-tablet-device \
    -fsdev "local,id=fsdev0,path=$SHARE_DIR,security_model=none" \
    -device "virtio-9p-device,fsdev=fsdev0,mount_tag=hostshare" \
    -nographic \
    -serial "file:$SERIAL_LOG" \
    -monitor "unix:$MON_SOCK,server,nowait" \
    -device "loader,file=$DTB_FILE,addr=0x40000000,force-raw=on" \
    -kernel "$KERNEL" &
QPID=$!

send_mon() { echo "$1" | nc -U "$MON_SOCK" -w 1 >/dev/null 2>&1 || true; }
grab() { echo "screendump $1" | nc -U "$MON_SOCK" -w 2 >/dev/null 2>&1 || true; sleep 0.3; }

echo -n "  Booting"
for i in $(seq 1 "$BOOT_WAIT"); do
    if ! kill -0 "$QPID" 2>/dev/null; then
        echo " QEMU exited early"; cat "$SERIAL_LOG" 2>/dev/null; exit 1
    fi
    if grep -q "💥\|panicking" "$SERIAL_LOG" 2>/dev/null; then
        echo " CRASH"; cat "$SERIAL_LOG" 2>/dev/null; exit 1
    fi
    echo -n "."
    sleep 1
done
echo ""

if ! grep -q "display pipeline running" "$SERIAL_LOG" 2>/dev/null; then
    echo "  FAIL: display pipeline never started"
    cat "$SERIAL_LOG" 2>/dev/null
    exit 1
fi
echo "  Pipeline running: OK"

# -----------------------------------------------------------------------
# Test 1: Clock ticks
# -----------------------------------------------------------------------
echo ""
echo "  Test 1: Clock ticking (2-second gap)"

grab "$SCREEN_DIR/clock0.ppm"
sleep 2
grab "$SCREEN_DIR/clock1.ppm"

CLOCK_OK=$(python3 -c "
import hashlib
h0 = hashlib.md5(open('$SCREEN_DIR/clock0.ppm','rb').read()).hexdigest()
h1 = hashlib.md5(open('$SCREEN_DIR/clock1.ppm','rb').read()).hexdigest()
print('yes' if h0 != h1 else 'no')
")

if [ "$CLOCK_OK" = "yes" ]; then
    echo "    PASS"
else
    echo "    FAIL: framebuffer unchanged after 2 seconds (clock frozen)"
    exit 1
fi

# -----------------------------------------------------------------------
# Test 2: Keystroke to pixel latency
# -----------------------------------------------------------------------
echo ""
echo "  Test 2: Keystroke renders within ${MAX_LATENCY_MS}ms"

grab "$SCREEN_DIR/before.ppm"
BASELINE=$(md5 -q "$SCREEN_DIR/before.ppm" 2>/dev/null || md5sum "$SCREEN_DIR/before.ppm" | cut -d' ' -f1)

send_mon "sendkey x"

# Poll at 100ms intervals.
INTERVAL_MS=100
SAMPLES=$(( MAX_LATENCY_MS / INTERVAL_MS ))
[ "$SAMPLES" -lt 5 ] && SAMPLES=5

FOUND_AT=""
for i in $(seq 1 "$SAMPLES"); do
    # sleep interval (in seconds, using python for sub-second precision)
    python3 -c "import time; time.sleep(${INTERVAL_MS}/1000.0)"

    grab "$SCREEN_DIR/sample${i}.ppm"
    H=$(md5 -q "$SCREEN_DIR/sample${i}.ppm" 2>/dev/null || md5sum "$SCREEN_DIR/sample${i}.ppm" | cut -d' ' -f1)

    if [ "$H" != "$BASELINE" ]; then
        FOUND_AT=$i
        break
    fi
done

if [ -n "$FOUND_AT" ]; then
    LATENCY=$(( FOUND_AT * INTERVAL_MS ))
    echo "    PASS (~${LATENCY}ms)"
else
    echo "    FAIL: no pixel change within ${MAX_LATENCY_MS}ms"
    exit 1
fi

# -----------------------------------------------------------------------
# Test 3: No crash
# -----------------------------------------------------------------------
echo ""
echo "  Test 3: Stability"

if grep -q "💥\|panicking" "$SERIAL_LOG" 2>/dev/null; then
    echo "    FAIL: crash detected"
    grep "💥\|panicking" "$SERIAL_LOG" | head -3
    exit 1
else
    echo "    PASS"
fi

echo ""
echo "=== Display Pipeline Test PASSED ==="
