#!/bin/bash
# Environment setup for rendering foundations mission.
# Idempotent — safe to run multiple times.

set -e

# Verify Rust toolchain
if ! command -v cargo &> /dev/null; then
    echo "ERROR: cargo not found. Install Rust toolchain."
    exit 1
fi

# Verify QEMU
if ! command -v qemu-system-aarch64 &> /dev/null; then
    echo "ERROR: qemu-system-aarch64 not found."
    exit 1
fi

# Verify Python PIL for screenshot conversion
python3 -c "from PIL import Image" 2>/dev/null || {
    echo "WARNING: Python PIL not available. Screenshot conversion will fail."
}

# Verify aarch64-unknown-none target is installed
rustup target list --installed 2>/dev/null | grep -q aarch64-unknown-none || {
    echo "Installing aarch64-unknown-none target..."
    rustup target add aarch64-unknown-none
}

# Build to verify toolchain works
cd /Users/user/Sites/os/system
cargo build --release 2>&1 | tail -3

# Run tests to verify baseline
cd /Users/user/Sites/os/system/test
cargo test -- --test-threads=1 2>&1 | grep "^test result:" | tail -1

echo "Environment ready."
