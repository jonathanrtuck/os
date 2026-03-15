#!/bin/bash
set -e

cd /Users/user/Sites/os/system

# Verify Rust toolchain is available
rustc --version >/dev/null 2>&1 || { echo "rustc not found"; exit 1; }
cargo --version >/dev/null 2>&1 || { echo "cargo not found"; exit 1; }

# Verify aarch64 target is installed
rustup target list --installed 2>/dev/null | grep -q aarch64-unknown-none || {
    echo "aarch64-unknown-none target not installed, adding..."
    rustup target add aarch64-unknown-none
}

# Run initial build to ensure everything compiles
echo "Running initial build verification..."
cargo build --release 2>&1

echo "Running initial test verification..."
cd test && cargo test -- --test-threads=1 2>&1

echo "Environment ready."
