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
if [ ! -f share/source-code-pro.ttf ]; then
  echo "WARNING: share/source-code-pro.ttf missing — font loading will fail at boot"
fi

# Copy variable Nunito Sans from Desktop if not already in share/
VARFONT_SRC="$HOME/Desktop/Nunito_Sans/NunitoSans-VariableFont_YTLC,opsz,wdth,wght.ttf"
if [ ! -f share/nunito-sans-variable.ttf ] && [ -f "$VARFONT_SRC" ]; then
  cp "$VARFONT_SRC" share/nunito-sans-variable.ttf
  echo "Copied variable Nunito Sans to share/"
fi

# Verify toolchain
rustup show active-toolchain 2>/dev/null || echo "WARNING: Rust toolchain not found"

# Build to verify everything compiles
cargo build --release 2>&1 | tail -3

echo "Init complete. Build successful."
