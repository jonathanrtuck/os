#!/bin/bash
# Visual regression test suite.
#
# Runs event scripts under the hypervisor, captures screenshots, and
# verifies rendered output with verify.py assertions (including OCR).
#
# Usage: make visual-test
#        ./test/visual-regression.sh

set -o pipefail

KERNEL=target/aarch64-unknown-none/release/kernel
VERIFY=".venv/bin/python3 test/verify.py"
DISK=disk.img
RES=1440x900
PASS=0
FAIL=0
TOTAL=0
ERRORS=""

RED='\033[0;31m'
GREEN='\033[0;32m'
DIM='\033[2m'
BOLD='\033[1m'
RESET='\033[0m'

cleanup() { pkill -f "hypervisor.*${KERNEL}" 2>/dev/null || true; sleep 0.3; }

assert() {
    local image=$1 assertion=$2; shift 2
    local args="$*"
    result=$($VERIFY "$image" --assert "$assertion" $args 2>&1)
    if [ $? -eq 0 ]; then
        printf "  ${GREEN}pass${RESET}  %s\n" "$result"
        return 0
    else
        printf "  ${RED}FAIL${RESET}  %s\n" "$result"
        return 1
    fi
}

begin_test() {
    TOTAL=$((TOTAL + 1))
    printf "\n${BOLD}%d. %s${RESET}\n" "$TOTAL" "$1"
    cleanup
}

end_test() {
    if $1; then
        PASS=$((PASS + 1))
    else
        FAIL=$((FAIL + 1))
        ERRORS="${ERRORS}\n  $2"
    fi
}

# ── Preflight ──────────────────────────────────────────────────────

if [ ! -f "$KERNEL" ]; then
    echo "Building kernel..."
    cargo build --release 2>&1 | tail -3
fi

for req in "$DISK" ".venv/bin/python3"; do
    if [ ! -f "$req" ]; then echo "FAIL: missing $req"; exit 2; fi
done

for cmd in hypervisor tesseract; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "FAIL: $cmd not in PATH"
        exit 2
    fi
done

printf "${BOLD}Visual regression tests${RESET}  (kernel=%s  resolution=%s)\n" "$KERNEL" "$RES"

# ── 1. Phase 5: basic glyph rendering + chrome ──────────────────

begin_test "glyph rendering + chrome (phase5: \"hello\")"
hypervisor "$KERNEL" --events test/phase5-hello.events \
    --drive "$DISK" --background --resolution "$RES" --timeout 30 \
    >/dev/null 2>&1
ok=true
assert /tmp/os-phase5-hello.png ocr_contains untitled || ok=false
# Text area has dark glyphs on white page (OCR unreliable at 14pt dark-on-white)
assert /tmp/os-phase5-hello.png region_variance 440,76,120,20,5 || ok=false
# Title bar text row (y=8) has glyphs
assert /tmp/os-phase5-hello.png row_has_text 8,20 || ok=false
# Shadow gradient region (left of page) has smooth color variation
assert /tmp/os-phase5-hello.png region_variance 380,380,60,40,10 || ok=false
end_test $ok "phase5: glyph + chrome"

# ── 2. Phase 8: keyboard navigation ──────────────────────────────

begin_test "keyboard navigation (phase8: arrows, home/end)"
hypervisor "$KERNEL" --events test/phase8-navigation.events \
    --drive "$DISK" --background --resolution "$RES" --timeout 30 \
    >/dev/null 2>&1
ok=true
# Multi-capture: last capture path "os-p8-sel-down" → -NNN.png
# Text renders on white page (centered, ~x=450)
assert /tmp/os-p8-sel-down-017.png ocr_contains untitled || ok=false
assert /tmp/os-p8-sel-down-017.png region_variance 440,76,120,20,5 || ok=false
end_test $ok "phase8: navigation"

# ── 3. Phase 9: paragraph rendering (280 glyphs, fast path) ──────

begin_test "paragraph rendering (phase9: 280 glyphs, vertex batching)"
hypervisor "$KERNEL" --events test/phase9-paragraphs.events \
    --drive "$DISK" --background --resolution "$RES" --timeout 60 \
    >/dev/null 2>&1
ok=true
assert /tmp/os-p9-para.png ocr_contains "quick brown fox" || ok=false
assert /tmp/os-p9-para.png ocr_contains "boxing wizards" || ok=false
assert /tmp/os-p9-para.png ocr_contains "second paragraph" || ok=false
assert /tmp/os-p9-para.png region_variance 450,78,400,20,5 || ok=false
end_test $ok "phase9: paragraphs"

# ── 4. Phase 9: scroll + viewport clipping ───────────────────────

begin_test "scroll + viewport clipping (phase9: 50 lines at 1440x900)"
hypervisor "$KERNEL" --events test/phase9-scroll.events \
    --drive "$DISK" --background --resolution "$RES" --timeout 60 \
    >/dev/null 2>&1
ok=true
# Multi-capture: "os-p9-pagedown" prefix → -199 through -203
# 199: after typing 50 lines (scrolled to bottom)
assert /tmp/os-p9-pagedown-199.png ocr_contains untitled || ok=false
# 200: after Cmd+Up (scrolled to top)
assert /tmp/os-p9-pagedown-200.png row_has_text 60,20 || ok=false
# 202: after Page Up
assert /tmp/os-p9-pagedown-202.png row_has_text 60,20 || ok=false
end_test $ok "phase9: scroll"

# ── 5. Phase 14: image space renders without black bars ──────────

begin_test "image space (phase14: three-space document switching)"
hypervisor "$KERNEL" --events test/phase14-three-spaces.events \
    --drive "$DISK" --background --resolution "$RES" --timeout 30 \
    >/dev/null 2>&1
ok=true
# Multi-capture: "p14-back" prefix → -NNN.png per frame_id.
# Frame 40: space 1 (image) after one Ctrl+Tab.
assert /tmp/p14-back-040.png no_black_bar 400,65,600,770,0 || ok=false
end_test $ok "phase14: image space"

# ── 6. Phase 15: showcase play button ────────────────────────────

begin_test "showcase play button (phase15: render, cursor, hit test)"
hypervisor "$KERNEL" --events test/phase15-showcase.events \
    --drive "$DISK" --audio --background --resolution "$RES" --timeout 30 \
    >/dev/null 2>&1
ok=true
# Multi-capture: "p15-clicked" prefix → -NNN.png per frame_id.
# Frame 50: play button visible (gray circle + white icon) at ~(196,730) in 1x.
assert /tmp/p15-clicked-050.png color_in_region 170,700,80,80,85,85,85 --tolerance 10 || ok=false
assert /tmp/p15-clicked-050.png color_in_region 180,710,60,60,231,231,231 --tolerance 30 || ok=false
# Button should be circular — corner pixel should be background, not button.
assert /tmp/p15-clicked-050.png pixel_at 174,704,32,32,32 --tolerance 5 || ok=false
end_test $ok "phase15: showcase play button"

# ── Summary ───────────────────────────────────────────────────────

cleanup

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [ $FAIL -eq 0 ]; then
    printf "${GREEN}all %d visual tests passed${RESET}\n" "$TOTAL"
else
    printf "${RED}%d/%d visual tests failed${RESET}\n" "$FAIL" "$TOTAL"
    printf "Failures:${ERRORS}\n"
fi
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

exit $FAIL
