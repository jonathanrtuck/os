#!/usr/bin/env bash
# QEMU smoke test — builds the kernel and checks for expected boot output.
#
# Usage: ./smoke-test.sh
# Returns: 0 on success, 1 on failure.

set -euo pipefail

TIMEOUT_SECS=10
EXPECTED=(
    "booting"
    "booted."
    "echo recv: ping"
    "init recv: pong"
)

echo "Building kernel…"

cargo build --release 2>&1 | tail -1

echo "Booting QEMU (${TIMEOUT_SECS}s timeout)…"

KERNEL="target/aarch64-unknown-none/release/kernel"
OUTPUT_FILE=$(mktemp)

qemu-system-aarch64 \
    -machine virt,gic-version=2 \
    -cpu cortex-a53 \
    -m 256M \
    -nographic \
    -serial mon:stdio \
    -kernel "$KERNEL" > "$OUTPUT_FILE" 2>&1 &
QEMU_PID=$!

sleep "$TIMEOUT_SECS"

kill "$QEMU_PID" 2>/dev/null
wait "$QEMU_PID" 2>/dev/null || true

echo ""
echo "--- QEMU output ---"

cat "$OUTPUT_FILE"

echo ""
echo "--- Checking expected output ---"

PASS=true

for pattern in "${EXPECTED[@]}"; do
    if grep -q "$pattern" "$OUTPUT_FILE"; then
        echo "  OK: $pattern"
    else
        echo "  FAIL: missing '$pattern'"
        PASS=false
    fi
done

rm -f "$OUTPUT_FILE"

if $PASS; then
    echo ""
    echo "Smoke test passed."
    exit 0
else
    echo ""
    echo "Smoke test FAILED."
    exit 1
fi
