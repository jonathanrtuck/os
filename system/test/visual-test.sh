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
DISK_ORIG="$SYSTEM_DIR/disk.img"
# Each test boots the OS which can mutate disk.img (document edits persist).
# Use a temporary copy so the original stays clean across test runs.
# The factory disk.img (built by mkdisk) includes the "Style Stress Test"
# rich text document with 32 styles — the visual tests depend on it.
DISK="$CAPTURE_DIR/disk-test.img"

# Boot wait: frames to wait before injecting events.
# Headless rendering (offscreen MTLTexture) has no vsync throttle, so
# frames process fast. The OS needs ~120 frames to fully boot and render
# the test document from a fresh factory disk; 150 gives comfortable margin.
BOOT_WAIT=150
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
    [ -f "$DISK_ORIG" ] || die "disk.img not found at $DISK_ORIG"
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
        --capture 150 "$CAPTURE_DIR/boot-idle.png" \
        --timeout "$TIMEOUT" >/dev/null 2>&1

    run_verify "$CAPTURE_DIR/boot-idle.png" "$SPEC_DIR/boot-idle.spec"
}

test_cursor_dark() {
    info "Test: cursor-dark (cursor on dark background)"
    cat > "$CAPTURE_DIR/cursor-dark.events" << 'EVENTS'
wait 150
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
wait 150
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
wait 150
type x
wait 30
capture /tmp/visual-tests/after-type.png
EVENTS
    run_hypervisor "$CAPTURE_DIR/after-type.events" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/after-type.png" "$SPEC_DIR/after-type.spec"
}

test_click_placement() {
    info "Test: click-placement (click mid-line places cursor correctly)"
    # At 800x600, "Style Stress Test" title is on line 0 near x=230,y=55.
    # Click mid-word, type 'Z', verify 'Z' appears in the title region.
    # Uses move+wait before click so pointer register is fresh.
    cat > "$CAPTURE_DIR/click-placement.events" << 'EVENTS'
wait 150
move 330 68
wait 5
click 330 68
wait 20
type Z
wait 20
capture /tmp/visual-tests/click-placement.png
EVENTS
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 800x600 \
        --events "$CAPTURE_DIR/click-placement.events" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/click-placement.png" "$SPEC_DIR/click-placement.spec"
}

test_dblclick_select() {
    info "Test: dblclick-select (double-click selects word with highlight)"
    # Double-click on "Style" at (280,60) in 800x600.
    # Selection highlight = blue-tinted pixels in the word region.
    cat > "$CAPTURE_DIR/dblclick-select.events" << 'EVENTS'
wait 150
move 280 60
wait 5
dblclick 280 60
wait 20
capture /tmp/visual-tests/dblclick-select.png
EVENTS
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 800x600 \
        --events "$CAPTURE_DIR/dblclick-select.events" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/dblclick-select.png" "$SPEC_DIR/dblclick-select.spec"
}

test_tripleclick_line() {
    info "Test: tripleclick-line (triple-click selects entire line in rich text)"
    # Triple-click on line 2 at (300,115) in 800x600.
    # After replacing selection with 'Z', the line content changes.
    # Verifies triple-click selects the full line (not just byte 0..0).
    cat > "$CAPTURE_DIR/tripleclick-line.events" << 'EVENTS'
wait 150
move 300 115
wait 5
click 300 115
wait 2
click 300 115
wait 2
click 300 115
wait 20
type Z
wait 20
capture /tmp/visual-tests/tripleclick-line.png
EVENTS
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 800x600 \
        --events "$CAPTURE_DIR/tripleclick-line.events" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/tripleclick-line.png" "$SPEC_DIR/tripleclick-line.spec"
}

test_font_weights() {
    info "Test: font-weights (weight variation in weight labels line)"
    # At 1600x1200, weight labels "Thin ExLt Light...Black" span y=320-336.
    # Verifies that different weight variants render with different stroke densities.
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 1600x1200 \
        --capture 150 "$CAPTURE_DIR/font-weights.png" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/font-weights.png" "$SPEC_DIR/font-weights.spec"
}

test_caret_height() {
    info "Test: caret-height (text caret has correct position and height)"
    # Capture baseline (no cursor), then click to place cursor on subtitle line.
    # At 1600x1200, subtitle at y~120. Caret should be 15-65px tall (font metrics).
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 1600x1200 \
        --capture 150 "$CAPTURE_DIR/caret-baseline.png" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    cat > "$CAPTURE_DIR/caret-height.events" << 'EVENTS'
wait 150
move 600 120
wait 5
click 600 120
wait 15
capture /tmp/visual-tests/caret-height.png
EVENTS
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 1600x1200 \
        --events "$CAPTURE_DIR/caret-height.events" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/caret-height.png" "$SPEC_DIR/caret-height.spec"
}

test_italic_slant() {
    info "Test: italic-slant (italic text has visual slant vs roman)"
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 1600x1200 \
        --capture 150 "$CAPTURE_DIR/italic-slant.png" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/italic-slant.png" "$SPEC_DIR/italic-slant.spec"
}

test_baseline_mixed() {
    info "Test: baseline-mixed (mixed-size text baseline alignment)"
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 1600x1200 \
        --capture 150 "$CAPTURE_DIR/baseline-mixed.png" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/baseline-mixed.png" "$SPEC_DIR/baseline-mixed.spec"
}

test_underline_below() {
    info "Test: underline-below (underline decoration below baseline)"
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 1600x1200 \
        --capture 150 "$CAPTURE_DIR/underline-below.png" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/underline-below.png" "$SPEC_DIR/underline-below.spec"
}

test_cursor_mixed() {
    info "Test: cursor-mixed (click on mixed-size line places cursor correctly)"
    # At 1600x1200, click on small text in right side of line 1.
    # The small text follows the large "Sans 36pt Bold Green".
    cat > "$CAPTURE_DIR/cursor-mixed.events" << 'EVENTS'
wait 150
move 850 160
wait 5
click 850 160
wait 20
type Z
wait 20
capture /tmp/visual-tests/cursor-mixed.png
EVENTS
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 1600x1200 \
        --events "$CAPTURE_DIR/cursor-mixed.events" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    run_verify "$CAPTURE_DIR/cursor-mixed.png" "$SPEC_DIR/cursor-mixed.spec"
}

test_cursor_italic() {
    info "Test: cursor-italic (caret skews near italic text)"
    # Single boot: capture baseline BEFORE clicking, then click and capture
    # with cursor. Same boot guarantees identical layout between frames,
    # so the diff isolates only the caret (no Y-position shift noise).
    # The hypervisor names multi-capture outputs as base-NNN.png using the
    # event file's basename. We sort the outputs to find baseline (lower
    # frame number) and cursor (higher frame number).
    rm -f "$CAPTURE_DIR"/cursor-italic-*.png
    cat > "$CAPTURE_DIR/cursor-italic.events" << 'EVENTS'
wait 150
capture /tmp/visual-tests/italic-baseline.png
move 500 325
wait 5
click 500 325
wait 15
capture /tmp/visual-tests/cursor-italic.png
EVENTS
    hypervisor "$KERNEL" --drive "$DISK" --background \
        --resolution 1600x1200 \
        --events "$CAPTURE_DIR/cursor-italic.events" \
        --timeout "$TIMEOUT" >/dev/null 2>&1
    # Map frame-numbered outputs to expected filenames.
    local baseline cursor
    baseline=$(ls "$CAPTURE_DIR"/cursor-italic-*.png 2>/dev/null | sort | head -1)
    cursor=$(ls "$CAPTURE_DIR"/cursor-italic-*.png 2>/dev/null | sort | tail -1)
    if [ -z "$baseline" ] || [ -z "$cursor" ] || [ "$baseline" = "$cursor" ]; then
        echo "  ERROR: expected 2 captures, got: $(ls "$CAPTURE_DIR"/cursor-italic-*.png 2>/dev/null | wc -l)"
        return 1
    fi
    cp "$baseline" "$CAPTURE_DIR/italic-baseline.png"
    cp "$cursor" "$CAPTURE_DIR/cursor-italic.png"
    run_verify "$CAPTURE_DIR/cursor-italic.png" "$SPEC_DIR/cursor-italic.spec"
}

# All test names in run order.
ALL_TESTS="boot-idle cursor-dark cursor-page after-type click-placement dblclick-select tripleclick-line font-weights caret-height italic-slant baseline-mixed underline-below cursor-mixed cursor-italic"

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

    # Fresh disk copy for each test — the OS persists edits to disk.img,
    # so each test must start from the factory state.
    cp "$DISK_ORIG" "$DISK"

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
