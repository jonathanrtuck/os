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

# ── 1. Phase 5: basic glyph rendering ────────────────────────────

begin_test "glyph rendering (phase5: \"hello\")"
hypervisor "$KERNEL" --events test/phase5-hello.events \
    --drive "$DISK" --background --resolution "$RES" --timeout 30 \
    >/dev/null 2>&1
ok=true
assert /tmp/os-phase5-hello.png region_variance 16,12,100,20,5 || ok=false
assert /tmp/os-phase5-hello.png ocr_contains hello || ok=false
end_test $ok "phase5: glyph rendering"

# ── 2. Phase 8: keyboard navigation ──────────────────────────────

begin_test "keyboard navigation (phase8: arrows, home/end)"
hypervisor "$KERNEL" --events test/phase8-navigation.events \
    --drive "$DISK" --background --resolution "$RES" --timeout 30 \
    >/dev/null 2>&1
ok=true
# Multi-capture: last capture path "os-p8-sel-down" → -NNN.png
# Frame 17 = after up arrow, frame 19 = after down, frame 23 = after shift+down
assert /tmp/os-p8-sel-down-017.png row_has_text 12,20 || ok=false
assert /tmp/os-p8-sel-down-017.png region_variance 16,12,100,20,5 || ok=false
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
assert /tmp/os-p9-para.png region_variance 16,12,200,20,5 || ok=false
end_test $ok "phase9: paragraphs"

# ── 4. Phase 9: scroll + viewport clipping ───────────────────────

begin_test "scroll + viewport clipping (phase9: 50 lines at 1440x900)"
hypervisor "$KERNEL" --events test/phase9-scroll.events \
    --drive "$DISK" --background --resolution "$RES" --timeout 60 \
    >/dev/null 2>&1
ok=true
# Multi-capture: "os-p9-pagedown" prefix → -199 through -203
# 199: after typing 50 lines (scrolled to bottom)
assert /tmp/os-p9-pagedown-199.png row_has_text 860,20 || ok=false
# 200: after Cmd+Up (scrolled to top, L01 visible)
assert /tmp/os-p9-pagedown-200.png row_has_text 12,20 || ok=false
assert /tmp/os-p9-pagedown-200.png row_is_bg 892,6 || ok=false
# 201: after Cmd+Down (scrolled to bottom again)
assert /tmp/os-p9-pagedown-201.png row_has_text 860,20 || ok=false
# 202: after Page Up
assert /tmp/os-p9-pagedown-202.png row_has_text 12,20 || ok=false
end_test $ok "phase9: scroll"

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
