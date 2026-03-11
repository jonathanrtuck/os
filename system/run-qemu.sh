#!/usr/bin/env bash
# Launch QEMU with device tree injection.
#
# Used by `cargo run` (via .cargo/config.toml) and can be run directly.
# Generates a DTB matching the machine config and injects it into guest RAM
# at 0x40000000 (pre-kernel area). Required because QEMU HVF on macOS
# doesn't pass the DTB address in x0 for bare-metal ELF kernels.
#
# Usage: ./run-qemu.sh <kernel-binary>

set -euo pipefail

KERNEL="${1:?usage: run-qemu.sh <kernel-binary>}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DTB_FILE="${SCRIPT_DIR}/virt.dtb"
DISK_IMG="${SCRIPT_DIR}/test.img"

# Kill any lingering QEMU that holds our disk image lock.
pkill -f "qemu-system-aarch64.*${DISK_IMG}" 2>/dev/null && sleep 0.2

# Create test disk if it doesn't exist.
if [ ! -f "$DISK_IMG" ]; then
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=1 2>/dev/null
    echo -n "HELLO VIRTIO BLK" | dd of="$DISK_IMG" bs=1 count=16 conv=notrunc 2>/dev/null
fi

QEMU_MACHINE="virt,gic-version=2"
QEMU_COMMON=(
    -cpu cortex-a53
    -smp 4
    -m 256M
    -global virtio-mmio.force-legacy=false
    -drive "file=$DISK_IMG,if=none,format=raw,id=hd0"
    -device virtio-blk-device,drive=hd0
    -device virtio-gpu-device
)

# Generate DTB if missing. Uses minimal machine config (no disk needed —
# virtio-mmio slots are part of the virt machine definition).
if [ ! -f "$DTB_FILE" ]; then
    qemu-system-aarch64 \
        -machine "${QEMU_MACHINE},dumpdtb=${DTB_FILE}" \
        -cpu cortex-a53 -smp 4 -m 256M -nographic 2>/dev/null

    # QEMU pads totalsize to 1MB; truncate to 64KB and fix header.
    dd if="$DTB_FILE" of="${DTB_FILE}.trim" bs=65536 count=1 2>/dev/null
    printf '\x00\x01\x00\x00' | dd of="${DTB_FILE}.trim" bs=1 seek=4 count=4 conv=notrunc 2>/dev/null
    mv "${DTB_FILE}.trim" "$DTB_FILE"
fi

# With virtio-gpu, we need a graphical display window. Use -nographic only
# when GPU_DISPLAY=0 (headless mode, e.g. CI). Default: display enabled.
if [ "${GPU_DISPLAY:-1}" = "0" ]; then
    exec qemu-system-aarch64 \
        -machine "$QEMU_MACHINE" \
        "${QEMU_COMMON[@]}" \
        -nographic \
        -serial mon:stdio \
        -device "loader,file=$DTB_FILE,addr=0x40000000,force-raw=on" \
        -kernel "$KERNEL"
else
    exec qemu-system-aarch64 \
        -machine "$QEMU_MACHINE" \
        "${QEMU_COMMON[@]}" \
        -serial mon:stdio \
        -device "loader,file=$DTB_FILE,addr=0x40000000,force-raw=on" \
        -kernel "$KERNEL"
fi
