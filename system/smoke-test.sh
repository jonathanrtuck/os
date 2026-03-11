#!/usr/bin/env bash
# QEMU smoke test — builds the kernel and checks for expected boot output.
#
# Usage: ./smoke-test.sh
# Returns: 0 on success, 1 on failure.

set -euo pipefail

TIMEOUT_SECS=10
EXPECTED=(
    "booting"
    "memory -"
    "heap -"
    "dtb -"
    "devices discovered"
    "frames -"
    "interrupts -"
    "scheduler - eevdf"
    "virtio - blk"
    "sector 0 - HELLO"
    "processes -"
    "smp -"
    "core 1 online"
    "core 2 online"
    "core 3 online"
    "timer -"
    "booted."
)

echo "Building kernel…"

cargo build --release 2>&1 | tail -1

echo "Booting QEMU (${TIMEOUT_SECS}s timeout)…"

KERNEL="target/aarch64-unknown-none/release/kernel"
OUTPUT_FILE=$(mktemp)
DISK_IMG=$(mktemp)
DTB_FILE=$(mktemp)

# Create a 1 MiB test disk with known content at sector 0.
dd if=/dev/zero of="$DISK_IMG" bs=1M count=1 2>/dev/null

echo -n "HELLO VIRTIO BLK" | dd of="$DISK_IMG" bs=1 count=16 conv=notrunc 2>/dev/null

# Generate the device tree blob for this machine configuration.
# QEMU HVF on macOS doesn't inject the DTB into guest RAM automatically,
# so we dump it and load it explicitly at 0x40000000 (pre-kernel area).
qemu-system-aarch64 \
    -machine virt,gic-version=2,dumpdtb="$DTB_FILE" \
    -cpu cortex-a53 \
    -smp 4 \
    -m 256M \
    -global virtio-mmio.force-legacy=false \
    -drive file="$DISK_IMG",if=none,format=raw,id=hd0 \
    -device virtio-blk-device,drive=hd0 2>/dev/null

# QEMU's dumpdtb sets totalsize to 1MB (full allocation), but actual content
# is ~8KB. Truncate to 64KB (safe upper bound, fits in pre-kernel 512KB area)
# and patch the totalsize header field to match.
dd if="$DTB_FILE" of="${DTB_FILE}.trim" bs=65536 count=1 2>/dev/null
printf '\x00\x01\x00\x00' | dd of="${DTB_FILE}.trim" bs=1 seek=4 count=4 conv=notrunc 2>/dev/null
mv "${DTB_FILE}.trim" "$DTB_FILE"

qemu-system-aarch64 \
    -machine virt,gic-version=2 \
    -cpu cortex-a53 \
    -smp 4 \
    -m 256M \
    -nographic \
    -serial mon:stdio \
    -global virtio-mmio.force-legacy=false \
    -drive file="$DISK_IMG",if=none,format=raw,id=hd0 \
    -device virtio-blk-device,drive=hd0 \
    -device loader,file="$DTB_FILE",addr=0x40000000,force-raw=on \
    -kernel "$KERNEL" > "$OUTPUT_FILE" 2>&1 &
QEMU_PID=$!

sleep "$TIMEOUT_SECS"

kill "$QEMU_PID" 2>/dev/null
wait "$QEMU_PID" 2>/dev/null || true

echo ""
echo "--- QEMU output ---"

cat "$OUTPUT_FILE"

echo ""
echo "--- Checking expected output ---"

PASS=true

for pattern in "${EXPECTED[@]}"; do
    if grep -q "$pattern" "$OUTPUT_FILE"; then
        echo "  OK: $pattern"
    else
        echo "  FAIL: missing '$pattern'"
        PASS=false
    fi
done

rm -f "$OUTPUT_FILE" "$DISK_IMG" "$DTB_FILE"

if $PASS; then
    echo ""
    echo "Smoke test passed."
    exit 0
else
    echo ""
    echo "Smoke test FAILED."
    exit 1
fi
