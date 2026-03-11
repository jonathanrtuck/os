#!/bin/bash
set -e

cd /Users/user/Sites/os/system

# Verify Rust nightly toolchain is available
rustc --version | grep -q nightly || {
    echo "ERROR: nightly Rust required. Run: rustup default nightly"
    exit 1
}

# Verify aarch64-unknown-none target is installed
rustup target list --installed | grep -q aarch64-unknown-none || {
    echo "Installing aarch64-unknown-none target..."
    rustup target add aarch64-unknown-none
}

# Verify kernel builds
echo "Verifying kernel build..."
cargo build 2>&1 | tail -5

# Verify test suite passes
echo "Verifying test suite..."
cd test && cargo test -- --test-threads=1 2>&1 | tail -5

echo "Environment ready."
