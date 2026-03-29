#!/bin/bash
# Visual regression test suite for the document-centric OS.
#
# Boots the hypervisor, runs test scenarios, captures screenshots,
# and runs verify.py assertions.  Returns 0 if all tests pass.
#
# Usage:
#     ./visual-test.sh              # run all tests
#     ./visual-test.sh boot-idle    # run a single test
#     ./visual-test.sh --list       # list available tests
#
# Requires: hypervisor in PATH, kernel built (cargo build --release).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SYSTEM_DIR="$(dirname "$SCRIPT_DIR")"
VENV_PYTHON="$SCRIPT_DIR/.venv/bin/python3"
VERIFY="$SCRIPT_DIR/verify.py"
SPEC_DIR="$SCRIPT_DIR/visual"
CAPTURE_DIR="/tmp/visual-tests"
KERNEL="$SYSTEM_DIR/target/aarch64-unknown-none/release/kernel"
DISK="$SYSTEM_DIR/disk.img"

# Boot wait: frames to wait before injecting events.
# The OS needs ~25 frames to boot; 60 gives comfortable margin.
BOOT_WAIT=60
# Post-event wait: frames to wait after events before capture.
POST_WAIT=30
# Hypervisor timeout in seconds.
TIMEOUT=60

# ── Helpers ───────────────────────────────────────────────────────

die() { echo "FATAL: $*" >&2; exit 2; }
info() { echo "  $*"; }

check_deps() {
    command -v hypervisor >/dev/null 2>&1 || die "hypervisor not in PATH"
    [ -x "$VENV_PYTHON" ] || die "Python venv not found at $VENV_PYTHON (run: python3 -m venv $SCRIPT_DIR/.venv && $SCRIPT_DIR/.venv/bin/pip install numpy scikit-image Pillow)"
    [ -f "$KERNEL" ] || die "Kernel not found at $KERNEL (run: cargo build --release)"
    [ -f "$DISK" ] || die "disk.img not found at $DISK"
}

run_hypervisor() {
    local events_file="$1"
    local extra_args=""
    if [ -n "$events_file" ]; then
        extra_args="--events $events_file"
    fi
    hypervisor "$KERNEL" --drive "$DISK" --background \
        $extra_args --timeout "$TIMEOUT" 2>&1
}

run_verify() {
    local image="$1"
    local spec="$2"
    "$VENV_PYTHON" "$VERIFY" "$image" --spec "$spec" 2>&1
}

# ── Test definitions ──────────────────────────────────────────────

test_boot_idle() {
    info "Test: boot-idle (no input, verify basic rendering)"
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --capture 30 "$CAPTURE_DIR/boot-idle.png" \
        --timeout "$TIMEOUT" >/dev/null 2>&1

    run_verify "$CAPTURE_DIR/boot-idle.png" "$SPEC_DIR/boot-idle.spec"
}

test_cursor_dark() {
    info "Test: cursor-dark (cursor on dark background)"
    cat > "$CAPTURE_DIR/cursor-dark.events" << 'EVENTS'
wait 60
move 400 400
wait 30
capture /tmp/visual-tests/cursor-dark.png
EVENTS
    run_hypervisor "$CAPTURE_DIR/cursor-dark.events" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/cursor-dark.png" "$SPEC_DIR/cursor-dark.spec"
}

test_cursor_page() {
    info "Test: cursor-page (cursor on white page)"
    cat > "$CAPTURE_DIR/cursor-page.events" << 'EVENTS'
wait 60
move 2000 2000
wait 30
capture /tmp/visual-tests/cursor-page.png
EVENTS
    run_hypervisor "$CAPTURE_DIR/cursor-page.events" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/cursor-page.png" "$SPEC_DIR/cursor-page.spec"
}

test_after_type() {
    info "Test: after-type (keyboard input reaches document)"
    cat > "$CAPTURE_DIR/after-type.events" << 'EVENTS'
wait 60
type x
wait 30
capture /tmp/visual-tests/after-type.png
EVENTS
    run_hypervisor "$CAPTURE_DIR/after-type.events" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/after-type.png" "$SPEC_DIR/after-type.spec"
}

# All test names in run order.
ALL_TESTS="boot-idle cursor-dark cursor-page after-type"

# ── Main ──────────────────────────────────────────────────────────

if [ "${1:-}" = "--list" ]; then
    echo "Available visual tests:"
    for t in $ALL_TESTS; do
        echo "  $t"
    done
    exit 0
fi

check_deps

# Select tests to run.
if [ $# -gt 0 ] && [ "$1" != "--list" ]; then
    tests_to_run="$*"
else
    tests_to_run="$ALL_TESTS"
fi

# Clean capture directory.
rm -rf "$CAPTURE_DIR"
mkdir -p "$CAPTURE_DIR"

passed=0
failed=0
failed_names=""

echo "=== Visual Test Suite ==="
echo ""

for test_name in $tests_to_run; do
    fn="test_${test_name//-/_}"  # cursor-dark → test_cursor_dark

    if ! type "$fn" >/dev/null 2>&1; then
        echo "  [SKIP] $test_name (unknown test)"
        continue
    fi

    output=$("$fn" 2>&1) && result=0 || result=$?

    if [ $result -eq 0 ]; then
        echo "  [PASS] $test_name"
        passed=$((passed + 1))
    else
        echo "  [FAIL] $test_name"
        echo "$output" | sed 's/^/         /'
        failed=$((failed + 1))
        failed_names="$failed_names $test_name"
    fi
done

echo ""
total=$((passed + failed))
if [ $failed -eq 0 ]; then
    echo "PASSED — $total test(s)"
    exit 0
else
    echo "FAILED — $failed/$total test(s) failed:$failed_names"
    echo ""
    echo "Captures saved in $CAPTURE_DIR/"
    exit 1
fi
