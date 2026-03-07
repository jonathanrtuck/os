#!/bin/sh

qemu-system-aarch64 \
  -machine virt,gic-version=2 \
  -cpu cortex-a53 \
  -m 256M \
  -nographic \
  -serial mon:stdio \
  -kernel target/aarch64-unknown-none/release/kernel
