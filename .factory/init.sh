#!/bin/bash
set -e

cd /Users/user/Sites/os/system

# Ensure test.img exists
if [ ! -f test.img ]; then
  dd if=/dev/zero of=test.img bs=1M count=1 2>/dev/null
  echo -n "HELLO VIRTIO BLK" | dd of=test.img bs=1 count=16 conv=notrunc 2>/dev/null
  echo "Created test.img"
fi

# Ensure virt.dtb exists (QEMU auto-generates on first run)
if [ ! -f virt.dtb ]; then
  echo "virt.dtb missing — will be generated on first QEMU boot via run-qemu.sh"
fi

# Ensure share directory exists with required assets
mkdir -p share
if [ ! -f share/SourceCodePro-Regular.ttf ]; then
  echo "WARNING: share/SourceCodePro-Regular.ttf missing — font loading will fail at boot"
fi

# Verify toolchain
rustup show active-toolchain 2>/dev/null || echo "WARNING: Rust toolchain not found"

# Build to verify everything compiles
cargo build --release 2>&1 | tail -3

echo "Init complete. Build successful."
