#!/usr/bin/env bash
# Headless kernel stress test.
#
# Boots QEMU without display devices (no GPU, no keyboard). Init detects
# the absence of GPU and spawns the stress test program, which saturates
# the kernel's IPC, scheduler, and timer paths across 4 SMP cores.
#
# Exit 0 = PASS (stress test completed), exit 1 = CRASH or timeout.
#
# Usage: ./stress-test.sh [timeout_seconds]
#   Default timeout: 120 seconds.

set -euo pipefail

TIMEOUT="${1:-180}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL="${SCRIPT_DIR}/target/aarch64-unknown-none/release/kernel"
DTB_FILE="${SCRIPT_DIR}/virt.dtb"
DISK_IMG="${SCRIPT_DIR}/test.img"
SERIAL_LOG="/tmp/os-stress-test-serial-$$.log"
QEMU_PID=""

cleanup() {
    [ -n "$QEMU_PID" ] && kill "$QEMU_PID" 2>/dev/null || true
    rm -f "$SERIAL_LOG"
}
trap cleanup EXIT

# Build.
echo "Building (release)..."
(cd "$SCRIPT_DIR" && cargo build --release 2>&1 | tail -3)

if [ ! -f "$KERNEL" ]; then
    echo "ERROR: kernel not found at $KERNEL"
    exit 2
fi

# Create test disk if needed (blk driver expects it).
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
pkill -f "qemu-system-aarch64.*os-stress" 2>/dev/null && sleep 0.2 || true

echo "Starting headless QEMU (timeout ${TIMEOUT}s)..."

# Launch QEMU headless — NO GPU, NO keyboard. Only blk device.
# Init will detect no GPU and run the stress test automatically.
qemu-system-aarch64 \
    -machine "virt,gic-version=2" \
    -cpu cortex-a53 -smp 4 -m 256M \
    -nographic \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$DISK_IMG,if=none,format=raw,id=hd0" \
    -device virtio-blk-device,drive=hd0 \
    -serial "file:${SERIAL_LOG}" \
    -device "loader,file=$DTB_FILE,addr=0x40000000,force-raw=on" \
    -kernel "$KERNEL" &
QEMU_PID=$!

# Monitor serial output for PASS, CRASH, or timeout.
START=$(date +%s)

while true; do
    NOW=$(date +%s)
    ELAPSED=$((NOW - START))

    # Timeout check.
    if [ "$ELAPSED" -ge "$TIMEOUT" ]; then
        echo "TIMEOUT after ${TIMEOUT}s — no PASS or crash detected"
        echo "--- last 20 lines of serial ---"
        tail -20 "$SERIAL_LOG" 2>/dev/null || true
        exit 1
    fi

    # QEMU still alive?
    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        # QEMU exited — check what happened.
        if grep -q "stress test PASS" "$SERIAL_LOG" 2>/dev/null; then
            echo "ALL PASS (QEMU exited after tests completed)"
            exit 0
        fi
        echo "QEMU exited unexpectedly"
        echo "--- serial output ---"
        cat "$SERIAL_LOG" 2>/dev/null || true
        exit 1
    fi

    # Check for crash.
    if grep -q "panicking" "$SERIAL_LOG" 2>/dev/null; then
        echo "CRASH detected after ${ELAPSED}s"
        echo "--- crash output ---"
        grep -A 20 "panicking\|kernel sync\|BUG:" "$SERIAL_LOG" 2>/dev/null || tail -30 "$SERIAL_LOG"
        exit 1
    fi

    # Check for FAIL (fuzz test failure).
    if grep -q "FAIL fuzz" "$SERIAL_LOG" 2>/dev/null; then
        echo "FUZZ TEST FAILED after ${ELAPSED}s"
        echo "--- output ---"
        cat "$SERIAL_LOG" 2>/dev/null || true
        exit 1
    fi

    # Check for stress test PASS (runs after fuzz test).
    if grep -q "stress test PASS" "$SERIAL_LOG" 2>/dev/null; then
        echo "ALL PASS after ${ELAPSED}s"
        echo "--- summary ---"
        grep -E "(fuzz|stress|PASS|FAIL)" "$SERIAL_LOG" 2>/dev/null || tail -20 "$SERIAL_LOG"
        # Kill QEMU (tests passed but init is still idle-looping).
        kill "$QEMU_PID" 2>/dev/null || true
        exit 0
    fi

    sleep 0.5
done
