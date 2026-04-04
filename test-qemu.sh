#!/usr/bin/env bash
# Automated QEMU test harness for the display pipeline.
#
# Sends key events programmatically via the QEMU monitor socket and captures
# serial output to a log file. Separates monitor traffic from serial output
# so debug prints are clean.
#
# Usage:
#   ./test-qemu.sh                    # boot + send default test keys
#   ./test-qemu.sh --keys "a b c"     # boot + send specific keys
#   ./test-qemu.sh --boot-only        # boot, wait, dump serial log
#   ./test-qemu.sh --delay 0.1        # set delay between keys (default: 0.05)
#   ./test-qemu.sh --wait 3           # seconds to wait after last key (default: 3)
#   ./test-qemu.sh --boot-wait 4      # seconds to wait for boot (default: 4)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL="${SCRIPT_DIR}/target/aarch64-unknown-none/release/kernel"
DTB_FILE="${SCRIPT_DIR}/virt.dtb"
DISK_IMG="${SCRIPT_DIR}/test.img"
SERIAL_LOG="/tmp/qemu-serial.log"
MON_SOCK="/tmp/qemu-mon.sock"

# Defaults.
KEYS="a b c d e"
KEY_DELAY=0.05
WAIT_AFTER=3
BOOT_WAIT=4
BOOT_ONLY=false
VIRGL="${VIRGL:-0}"

# Parse arguments.
while [[ $# -gt 0 ]]; do
    case "$1" in
        --keys)      KEYS="$2"; shift 2 ;;
        --delay)     KEY_DELAY="$2"; shift 2 ;;
        --wait)      WAIT_AFTER="$2"; shift 2 ;;
        --boot-wait) BOOT_WAIT="$2"; shift 2 ;;
        --boot-only) BOOT_ONLY=true; shift ;;
        --virgl)     VIRGL=1; shift ;;
        *)           echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# Check kernel exists.
if [ ! -f "$KERNEL" ]; then
    echo "error: kernel not found at $KERNEL" >&2
    echo "       run 'cargo build --release' first" >&2
    exit 1
fi

# Clean up stale QEMU processes and sockets.
cleanup() {
    if [ -n "${QPID:-}" ]; then
        kill "$QPID" 2>/dev/null || true
        wait "$QPID" 2>/dev/null || true
    fi
    rm -f "$MON_SOCK"
}
trap cleanup EXIT

pkill -f "qemu-system-aarch64.*${DISK_IMG}" 2>/dev/null && sleep 0.3 || true
rm -f "$MON_SOCK" "$SERIAL_LOG"

# Launch QEMU:
#   - Serial output → file (clean, no monitor noise)
#   - Monitor → Unix socket (for sending keys)
#   - No display window (-nographic) unless virgl mode
SHARE_DIR="${SCRIPT_DIR}/assets"

# Virgl mode: use custom QEMU build with GPU acceleration.
VIRGL_QEMU_DIR="${VIRGL_QEMU_DIR:-${SCRIPT_DIR}/vendor/qemu}"
VIRGL_QEMU="${VIRGL_QEMU_DIR}/qemu-system-aarch64"

if [ "$VIRGL" = "1" ]; then
    if [ ! -x "$VIRGL_QEMU" ]; then
        echo "error: virgl QEMU not found at $VIRGL_QEMU" >&2
        exit 1
    fi
    QEMU_BIN="$VIRGL_QEMU"
    GPU_DEV="virtio-gpu-gl-device"
else
    QEMU_BIN="qemu-system-aarch64"
    GPU_DEV="virtio-gpu-device"
fi

"$QEMU_BIN" \
    -machine virt,gic-version=3 \
    -cpu cortex-a53 -smp 4 -m 256M \
    -rtc base=localtime \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$DISK_IMG,if=none,format=raw,id=hd0" \
    -device virtio-blk-device,drive=hd0 \
    -device "$GPU_DEV" \
    -device virtio-keyboard-device \
    -device virtio-tablet-device \
    -fsdev "local,id=fsdev0,path=$SHARE_DIR,security_model=none" \
    -device "virtio-9p-device,fsdev=fsdev0,mount_tag=hostshare" \
    $(if [ "$VIRGL" = "1" ]; then echo "-display cocoa,gl=es"; else echo "-nographic"; fi) \
    -serial file:"$SERIAL_LOG" \
    -monitor unix:"$MON_SOCK",server,nowait \
    -device "loader,file=$DTB_FILE,addr=0x40000000,force-raw=on" \
    -kernel "$KERNEL" &
QPID=$!

echo "=== QEMU started (pid $QPID), waiting ${BOOT_WAIT}s for boot ==="
sleep "$BOOT_WAIT"

# Verify QEMU is still running.
if ! kill -0 "$QPID" 2>/dev/null; then
    echo "error: QEMU exited early" >&2
    echo "--- serial log ---"
    cat "$SERIAL_LOG" 2>/dev/null || true
    exit 1
fi

# Helper: send a monitor command via the socket.
send_mon() {
    echo "$1" | nc -U "$MON_SOCK" -w 1 >/dev/null 2>&1 || true
}

if [ "$BOOT_ONLY" = true ]; then
    echo "=== boot-only mode, waiting ${WAIT_AFTER}s ==="
    sleep "$WAIT_AFTER"
else
    echo "=== sending keys: $KEYS (delay=${KEY_DELAY}s) ==="
    for key in $KEYS; do
        send_mon "sendkey $key"
        sleep "$KEY_DELAY"
    done

    echo "=== keys sent, waiting ${WAIT_AFTER}s for processing ==="
    sleep "$WAIT_AFTER"
fi

# Dump serial output.
echo ""
echo "=== serial output ==="
cat "$SERIAL_LOG" 2>/dev/null || echo "(empty)"
echo ""
echo "=== done ==="
