#!/usr/bin/env bash
# Launch the kernel in a VM.
#
# Default: native hypervisor with Metal GPU (requires `hypervisor` on PATH).
# QEMU=1: use QEMU instead (virgl or software rendering).
#
# Usage: ./run.sh <kernel-binary>

set -euo pipefail

KERNEL="${1:?usage: run.sh <kernel-binary>}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

DISK_IMG="${SCRIPT_DIR}/disk.img"

# Rebuild factory disk image if missing or if format sources changed.
# The disk embeds the catalog format from libraries/store and libraries/fs —
# if those change, the old image becomes incompatible (boot succeeds but
# catalog queries return nothing → "font not found" → blank display).
MKDISK_BIN="${SCRIPT_DIR}/../tools/mkdisk/target/release/mkdisk"
STALE=0
if [ ! -f "$DISK_IMG" ]; then
    STALE=1
elif [ -f "$DISK_IMG" ]; then
    # Check if any format-defining source file is newer than disk.img.
    for src_dir in \
        "${SCRIPT_DIR}/../tools/mkdisk" \
        "${SCRIPT_DIR}/libraries/store" \
        "${SCRIPT_DIR}/libraries/fs" \
        "${SCRIPT_DIR}/share"; do
        if [ -d "$src_dir" ] && [ -n "$(find "$src_dir" -newer "$DISK_IMG" -type f 2>/dev/null | head -1)" ]; then
            echo "disk.img: stale (changed: $src_dir)" >&2
            STALE=1
            break
        fi
    done
fi
if [ "$STALE" = "1" ]; then
    rm -f "$DISK_IMG"
    cd "${SCRIPT_DIR}/../tools/mkdisk" && cargo build --release --message-format=short 2>&1 | tail -1
    cd "$SCRIPT_DIR"
    if [ ! -x "$MKDISK_BIN" ]; then
        echo "error: failed to build mkdisk" >&2
        exit 1
    fi
    "$MKDISK_BIN" "$DISK_IMG" "${SCRIPT_DIR}/share"
fi

# Default: native hypervisor with Metal GPU passthrough.
# Set QEMU=1 to use QEMU instead.
if [ "${QEMU:-0}" != "1" ]; then
    HYPERVISOR="${HYPERVISOR_BIN:-$(command -v hypervisor 2>/dev/null || true)}"
    if [ -z "$HYPERVISOR" ] || [ ! -x "$HYPERVISOR" ]; then
        echo "error: hypervisor not found on PATH" >&2
        echo "       cd ~/Sites/hypervisor && make install" >&2
        echo "       or set QEMU=1 to use QEMU instead" >&2
        exit 1
    fi
    # Native boot: disk image only, no 9p host share needed.
    # Pass --share for development use (e.g. SHARE=1 ./run.sh kernel).
    if [ "${SHARE:-0}" = "1" ]; then
        exec "$HYPERVISOR" "$KERNEL" --share "${SCRIPT_DIR}/share" --drive "$DISK_IMG"
    else
        exec "$HYPERVISOR" "$KERNEL" --drive "$DISK_IMG"
    fi
fi
DTB_FILE="${SCRIPT_DIR}/virt.dtb"

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

# Virgl (GPU-accelerated) mode: VIRGL=1 uses a custom QEMU build with
# virtio-gpu-gl-device backed by virglrenderer + ANGLE (OpenGL ES via Metal).
# Built from https://github.com/akihikodaki/v — see docs/superpowers/plans/.
VIRGL_QEMU_DIR="${VIRGL_QEMU_DIR:-${SCRIPT_DIR}/bin/qemu}"
VIRGL_QEMU="${VIRGL_QEMU_DIR}/qemu-system-aarch64"

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
    DISPLAY_OPT="-display cocoa,gl=es,full-screen=on,zoom-to-fit=on"
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
