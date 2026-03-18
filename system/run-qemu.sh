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

# Detect host display resolution (physical pixels) for Retina rendering.
# Falls back to 1280x800 if detection fails.
if RES=$(system_profiler SPDisplaysDataType 2>/dev/null | grep "Resolution:" | head -1); then
    SCREEN_W=$(echo "$RES" | sed 's/.*: \([0-9]*\) x .*/\1/')
    SCREEN_H=$(echo "$RES" | sed 's/.* x \([0-9]*\).*/\1/')
fi
SCREEN_W="${SCREEN_W:-1280}"
SCREEN_H="${SCREEN_H:-800}"

# Kill any lingering QEMU that holds our disk image lock.
pkill -f "qemu-system-aarch64.*${DISK_IMG}" 2>/dev/null && sleep 0.2

# Create test disk if it doesn't exist.
if [ ! -f "$DISK_IMG" ]; then
    dd if=/dev/zero of="$DISK_IMG" bs=1M count=1 2>/dev/null
    echo -n "HELLO VIRTIO BLK" | dd of="$DISK_IMG" bs=1 count=16 conv=notrunc 2>/dev/null
fi

# Virgl (GPU-accelerated) mode: VIRGL=1 uses a custom QEMU build with
# virtio-gpu-gl-device backed by virglrenderer + ANGLE (OpenGL ES via Metal).
# Built from https://github.com/akihikodaki/v — see docs/superpowers/plans/.
VIRGL_QEMU_DIR="${VIRGL_QEMU_DIR:-/Users/user/Sites/v}"
VIRGL_QEMU="${VIRGL_QEMU_DIR}/bin/qemu-system-aarch64"

# Default to virgl mode — virgil-render requires it. Set VIRGL=0 to force
# standard QEMU (only works if init is changed back to spawn virtio-gpu).
if [ "${VIRGL:-1}" = "1" ]; then
    if [ ! -x "$VIRGL_QEMU" ]; then
        echo "error: virgl QEMU not found at $VIRGL_QEMU" >&2
        echo "       build it from https://github.com/akihikodaki/v" >&2
        echo "       or set VIRGL_QEMU_DIR to the install path" >&2
        exit 1
    fi
    QEMU_BIN="$VIRGL_QEMU"
    GPU_DEVICE="virtio-gpu-gl-device,xres=${SCREEN_W},yres=${SCREEN_H}"
    DISPLAY_OPT="-display cocoa,gl=es"
else
    QEMU_BIN="qemu-system-aarch64"
    GPU_DEVICE="virtio-gpu-device,xres=${SCREEN_W},yres=${SCREEN_H}"
    DISPLAY_OPT="-display cocoa,full-screen=on,zoom-to-fit=on"
fi

# Use HVF (Apple Hypervisor.framework) when available for native-speed
# execution. Falls back to TCG (software emulation) on non-macOS or when
# HVF is unavailable. HVF requires the virtual timer (CNTV_*) and
# ISV-safe MMIO instructions — see timer.rs and memory_mapped_io.rs.
if "$QEMU_BIN" -accel help 2>&1 | grep -q hvf; then
    QEMU_MACHINE="virt,gic-version=3,accel=hvf"
    QEMU_CPU="host"
else
    QEMU_MACHINE="virt,gic-version=3"
    QEMU_CPU="cortex-a53"
fi
SHARE_DIR="${SCRIPT_DIR}/share"

QEMU_COMMON=(
    -cpu "$QEMU_CPU"
    -smp 4
    -m 256M
    -rtc base=localtime
    -global virtio-mmio.force-legacy=false
    -drive "file=$DISK_IMG,if=none,format=raw,id=hd0"
    -device virtio-blk-device,drive=hd0
    -device "$GPU_DEVICE"
    -device virtio-keyboard-device
    -device virtio-tablet-device
    -fsdev "local,id=fsdev0,path=$SHARE_DIR,security_model=none"
    -device "virtio-9p-device,fsdev=fsdev0,mount_tag=hostshare"
)

# Generate DTB if missing. Uses minimal machine config (no disk needed —
# virtio-mmio slots are part of the virt machine definition).
if [ ! -f "$DTB_FILE" ]; then
    "$QEMU_BIN" \
        -machine "${QEMU_MACHINE},dumpdtb=${DTB_FILE}" \
        -cpu "$QEMU_CPU" -smp 4 -m 256M -nographic 2>/dev/null

    # QEMU pads totalsize to 1MB; truncate to 64KB and fix header.
    dd if="$DTB_FILE" of="${DTB_FILE}.trim" bs=65536 count=1 2>/dev/null
    printf '\x00\x01\x00\x00' | dd of="${DTB_FILE}.trim" bs=1 seek=4 count=4 conv=notrunc 2>/dev/null
    mv "${DTB_FILE}.trim" "$DTB_FILE"
fi

# With virtio-gpu, we need a graphical display window. Use -nographic only
# when GPU_DISPLAY=0 (headless mode, e.g. CI). Default: display enabled.
if [ "${GPU_DISPLAY:-1}" = "0" ]; then
    exec "$QEMU_BIN" \
        -machine "$QEMU_MACHINE" \
        "${QEMU_COMMON[@]}" \
        -nographic \
        -serial mon:stdio \
        -device "loader,file=$DTB_FILE,addr=0x40000000,force-raw=on" \
        -kernel "$KERNEL"
else
    exec "$QEMU_BIN" \
        -machine "$QEMU_MACHINE" \
        "${QEMU_COMMON[@]}" \
        $DISPLAY_OPT \
        -serial mon:stdio \
        -device "loader,file=$DTB_FILE,addr=0x40000000,force-raw=on" \
        -kernel "$KERNEL"
fi
