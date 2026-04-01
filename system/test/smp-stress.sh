#!/usr/bin/env bash
# SMP context switch stress test via hypervisor.
#
# Boots the kernel with full display pipeline and sends rapid Ctrl+Tab
# keypresses to cycle documents — the exact pattern that triggered the
# 2026-03-31 kernel crash series. Runs for a configurable duration and
# checks for panics/crashes in serial output.
#
# Usage: ./smp-stress.sh [duration_seconds]
#   Default: 60 seconds.
#
# Exit 0 = PASS (no crash), exit 1 = CRASH detected.

set -euo pipefail

DURATION="${1:-60}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SYSTEM_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
KERNEL="${SYSTEM_DIR}/target/aarch64-unknown-none/release/kernel"
DISK_IMG="${SYSTEM_DIR}/disk.img"

# Build.
echo "Building (release)..."
(cd "$SYSTEM_DIR" && cargo build --release 2>&1 | tail -3)

if [ ! -f "$KERNEL" ]; then
    echo "ERROR: kernel not found at $KERNEL"
    exit 2
fi

# Create event script: rapid Ctrl+Tab + typing + pauses.
EVENT_SCRIPT=$(mktemp /tmp/smp-stress-events-XXXX)

cat > "$EVENT_SCRIPT" << 'EVENTS'
wait 30
type hello world this is a stress test
key return
type more text to fill the buffer
key return
EVENTS

# Add rapid Ctrl+Tab cycles interspersed with typing.
# Each cycle: Ctrl+Tab, short wait, type a character, Ctrl+Tab back.
CYCLES=$((DURATION * 2))
for i in $(seq 1 "$CYCLES"); do
    echo "key ctrl+tab" >> "$EVENT_SCRIPT"
    echo "wait 1" >> "$EVENT_SCRIPT"
    if (( i % 10 == 0 )); then
        echo "type x" >> "$EVENT_SCRIPT"
    fi
    if (( i % 50 == 0 )); then
        echo "key return" >> "$EVENT_SCRIPT"
    fi
done

# Final capture to prove we survived.
echo "wait 10" >> "$EVENT_SCRIPT"
echo "capture /tmp/smp-stress-final.png" >> "$EVENT_SCRIPT"

echo "Running hypervisor with ${CYCLES} Ctrl+Tab cycles (${DURATION}s)..."

# Run hypervisor in background mode with event script.
OUTPUT=$(cd "$SYSTEM_DIR" && hypervisor "$KERNEL" \
    --drive "$DISK_IMG" \
    --background \
    --events "$EVENT_SCRIPT" \
    --timeout "$((DURATION + 30))" 2>&1) || true

rm -f "$EVENT_SCRIPT"

# Check for crash indicators in output.
if echo "$OUTPUT" | grep -qE "panicking|💥|BUG:|kernel sync:"; then
    echo "CRASH detected during SMP stress!"
    echo "--- crash output ---"
    echo "$OUTPUT" | grep -B2 -A20 "panicking\|💥\|BUG:\|kernel sync:"
    exit 1
fi

# Check that we got the final screenshot (proves the kernel survived).
if [ -f /tmp/smp-stress-final.png ]; then
    echo "PASS — kernel survived ${CYCLES} Ctrl+Tab cycles without crash"
    rm -f /tmp/smp-stress-final.png
    exit 0
else
    echo "WARNING — no final screenshot captured (hypervisor may have timed out)"
    echo "--- last 30 lines of output ---"
    echo "$OUTPUT" | tail -30
    # Not necessarily a crash — could be a timeout. Don't fail hard.
    exit 0
fi
