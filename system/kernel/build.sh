#!/bin/sh

echo "building…"

mkdir -p dist

cargo build --target aarch64-unknown-none --release

elf="target/aarch64-unknown-none/release/kernel"

cp -f "$elf" dist/kernel.elf

echo "built."
