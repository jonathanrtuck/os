#!/usr/bin/env bash
# Full integration test — boots QEMU with ALL devices (GPU + keyboard + blk),
# verifies the complete boot + driver spawn + display pipeline.
#
# Unlike smoke.sh (blk only) and stress.sh (headless, no GPU),
# this tests the full device pipeline that a real boot would exercise.
#
# QEMU display is suppressed (-display none) so this runs in CI/headless.
# The keyboard driver starts but receives no input — compositor idles.
#
# Usage: ./integration-test.sh [timeout_seconds]
#   Default timeout: 15 seconds.
#   Exit 0 = PASS, exit 1 = FAIL (missing output or crash).

set -euo pipefail

TIMEOUT="${1:-15}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SYSTEM_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
KERNEL="${SYSTEM_DIR}/target/aarch64-unknown-none/release/kernel"
DTB_FILE="${SYSTEM_DIR}/virt.dtb"
DISK_IMG="${SYSTEM_DIR}/test.img"
SERIAL_LOG=$(mktemp)
QEMU_PID=""

cleanup() {
    [ -n "$QEMU_PID" ] && kill "$QEMU_PID" 2>/dev/null || true
    rm -f "$SERIAL_LOG"
}
trap cleanup EXIT

echo "Building (release)..."
(cd "$SYSTEM_DIR" && cargo build --release 2>&1 | tail -1)

if [ ! -f "$KERNEL" ]; then
    echo "ERROR: kernel not found at $KERNEL"
    exit 2
fi

# Create test disk if needed.
if [ ! -f "$DISK_IMG" ]; then
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=1 2>/dev/null
    echo -n "HELLO VIRTIO BLK" | dd of="$DISK_IMG" bs=1 count=16 conv=notrunc 2>/dev/null
fi

# Generate DTB with full device config (GPU + keyboard + blk).
INTEGRATION_DTB=$(mktemp)
qemu-system-aarch64 \
    -machine "virt,gic-version=2,dumpdtb=${INTEGRATION_DTB}" \
    -cpu cortex-a53 -smp 4 -m 256M \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$DISK_IMG,if=none,format=raw,id=hd0" \
    -device virtio-blk-device,drive=hd0 \
    -device virtio-gpu-device \
    -device virtio-keyboard-device 2>/dev/null

dd if="$INTEGRATION_DTB" of="${INTEGRATION_DTB}.trim" bs=65536 count=1 2>/dev/null
printf '\x00\x01\x00\x00' | dd of="${INTEGRATION_DTB}.trim" bs=1 seek=4 count=4 conv=notrunc 2>/dev/null
mv "${INTEGRATION_DTB}.trim" "$INTEGRATION_DTB"

# Kill any lingering QEMU.
pkill -f "qemu-system-aarch64.*integration" 2>/dev/null && sleep 0.2 || true

echo "Starting QEMU with full device set (GPU + keyboard + blk)..."

qemu-system-aarch64 \
    -machine "virt,gic-version=2" \
    -cpu cortex-a53 -smp 4 -m 256M \
    -display none \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$DISK_IMG,if=none,format=raw,id=hd0" \
    -device virtio-blk-device,drive=hd0 \
    -device virtio-gpu-device \
    -device virtio-keyboard-device \
    -serial "file:${SERIAL_LOG}" \
    -device "loader,file=$INTEGRATION_DTB,addr=0x40000000,force-raw=on" \
    -kernel "$KERNEL" &
QEMU_PID=$!

# Wait for boot + pipeline setup.
echo -n "Waiting for pipeline"
START=$(date +%s)
while true; do
    NOW=$(date +%s)
    ELAPSED=$((NOW - START))

    if [ "$ELAPSED" -ge "$TIMEOUT" ]; then
        echo " TIMEOUT (${TIMEOUT}s)"
        break
    fi

    if ! kill -0 "$QEMU_PID" 2>/dev/null; then
        echo " QEMU exited"
        break
    fi

    # Check for crash.
    if grep -q "panicking\|💥" "$SERIAL_LOG" 2>/dev/null; then
        echo " CRASH"
        echo "--- crash output ---"
        cat "$SERIAL_LOG"
        exit 1
    fi

    # Check for pipeline running (the last milestone).
    if grep -q "display pipeline running" "$SERIAL_LOG" 2>/dev/null; then
        echo " OK (${ELAPSED}s)"
        break
    fi

    echo -n "."
    sleep 0.5
done

# Kill QEMU.
kill "$QEMU_PID" 2>/dev/null
wait "$QEMU_PID" 2>/dev/null || true
QEMU_PID=""

rm -f "$INTEGRATION_DTB"

echo ""
echo "--- Serial output ---"
cat "$SERIAL_LOG"
echo ""
echo "--- Checking expected output ---"

# Boot sequence.
EXPECTED_BOOT=(
    "booting"
    "memory -"
    "heap -"
    "dtb -"
    "devices discovered"
    "scheduler - eevdf"
    "virtio - blk"
    "smp -"
    "booted."
)

# Init + driver spawn.
EXPECTED_INIT=(
    "init - proto-os-service"
    "devices in manifest"
    "spawning driver"
    "spawned driver: blk"
)

# Display pipeline.
EXPECTED_PIPELINE=(
    "setting up display pipeline"
    "starting gpu driver"
    "starting compositor"
    "display pipeline running"
)

PASS=true

for pattern in "${EXPECTED_BOOT[@]}"; do
    if grep -q "$pattern" "$SERIAL_LOG"; then
        echo "  OK: $pattern"
    else
        echo "  FAIL: missing '$pattern'"
        PASS=false
    fi
done

for pattern in "${EXPECTED_INIT[@]}"; do
    if grep -q "$pattern" "$SERIAL_LOG"; then
        echo "  OK: $pattern"
    else
        echo "  FAIL: missing '$pattern'"
        PASS=false
    fi
done

for pattern in "${EXPECTED_PIPELINE[@]}"; do
    if grep -q "$pattern" "$SERIAL_LOG"; then
        echo "  OK: $pattern"
    else
        echo "  FAIL: missing '$pattern'"
        PASS=false
    fi
done

# Must NOT contain crash markers.
if grep -q "panicking\|💥\|BUG:" "$SERIAL_LOG"; then
    echo "  FAIL: crash detected in output"
    PASS=false
else
    echo "  OK: no crashes"
fi

echo ""
if $PASS; then
    echo "Integration test PASSED."
    exit 0
else
    echo "Integration test FAILED."
    exit 1
fi
