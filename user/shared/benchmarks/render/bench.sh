#!/bin/bash
# Render benchmark — boots the OS, captures compositor metrics, compares
# against baselines in baselines.toml.
#
# Usage: user/shared/benchmarks/render/bench.sh [--update-baseline]
#
# Requires: hypervisor, disk.img, release kernel build.

set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KERNEL="target/aarch64-unknown-none/release/kernel"
EVENTS="$SCRIPT_DIR/render-bench.events"
BASELINES="$SCRIPT_DIR/baselines.toml"
DISK="disk.img"
RES="1440x900"
TIMEOUT=30
UPDATE=0

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
DIM='\033[2m'
BOLD='\033[1m'
RESET='\033[0m'

for arg in "$@"; do
    case "$arg" in
        --update-baseline) UPDATE=1 ;;
    esac
done

pass() { printf "  ${GREEN}pass${RESET}  %s\n" "$1"; }
fail() { printf "  ${RED}FAIL${RESET}  %s\n" "$1"; FAILURES=$((FAILURES + 1)); }
info() { printf "${DIM}info${RESET}  %s\n" "$1"; }

FAILURES=0

# ── Preflight ──────────────────────────────────────────────────────

for cmd in hypervisor; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "FAIL: $cmd not in PATH"
        exit 2
    fi
done

if [ ! -f "$KERNEL" ]; then
    echo "FAIL: $KERNEL not found — run 'cargo build --release' first"
    exit 2
fi

if [ ! -f "$DISK" ]; then
    echo "FAIL: $DISK not found"
    exit 2
fi

# ── Run ────────────────────────────────────────────────────────────

pkill -f "hypervisor.*kernel" 2>/dev/null || true
sleep 0.3

printf "${BOLD}Render benchmark${RESET}  (resolution=%s)\n" "$RES"

SERIAL_LOG=$(mktemp /tmp/bench-render-XXXXXX.log)
trap "rm -f $SERIAL_LOG; pkill -f 'hypervisor.*kernel' 2>/dev/null || true" EXIT

stdbuf -oL \
    hypervisor "$KERNEL" \
    --events "$EVENTS" \
    --drive "$DISK" \
    --background \
    --resolution "$RES" \
    --timeout "$TIMEOUT" \
    > "$SERIAL_LOG" 2>&1

# ── Parse compositor metrics ───────────────────────────────────────

# comp: 1001ms r=120 frame=774/2099us walk=6/15us atlas=7us*120 ...
# Extract all "comp:" lines
COMP_LINES=$(sed 's/comp:/\ncomp:/g' "$SERIAL_LOG" | grep '^comp:' || true)

if [ -z "$COMP_LINES" ]; then
    echo "FAIL: no compositor metrics in output"
    cat "$SERIAL_LOG"
    exit 1
fi

COMP_COUNT=$(echo "$COMP_LINES" | wc -l | tr -d ' ')
info "captured $COMP_COUNT metrics periods"

# Parse each line into structured values.
# Format: comp: <wall>ms r=<renders> frame=<avg>/<max>us walk=<avg>/<max>us ...
#         idle=<count>
parse_comp() {
    local line="$1"
    R=$(echo "$line" | sed -n 's/.* r=\([0-9]*\) .*/\1/p')
    FRAME_AVG=$(echo "$line" | sed -n 's/.* frame=\([0-9]*\)\/.*/\1/p')
    FRAME_MAX=$(echo "$line" | sed -n 's/.* frame=[0-9]*\/\([0-9]*\)us.*/\1/p')
    WALK_AVG=$(echo "$line" | sed -n 's/.* walk=\([0-9]*\)\/.*/\1/p')
    EMIT_AVG=$(echo "$line" | sed -n 's/.* emit=\([0-9]*\)us.*/\1/p')
    IDLE=$(echo "$line" | sed -n 's/.* idle=\([0-9]*\).*/\1/p')
}

# ── Report ─────────────────────────────────────────────────────────

echo ""

# Period 1: typing burst (first metrics line with >50 renders)
TYPING_LINE=$(echo "$COMP_LINES" | awk -F'r=' '{split($2,a," "); if (a[1]+0 > 50) {print; exit}}')
if [ -n "$TYPING_LINE" ]; then
    parse_comp "$TYPING_LINE"
    printf "  ${BOLD}Typing${RESET}  r=%s  frame=%s/%sus  walk=%sus  emit=%sus  idle=%s\n" \
        "$R" "$FRAME_AVG" "$FRAME_MAX" "$WALK_AVG" "$EMIT_AVG" "$IDLE"

    # Read baselines from [typing] section
    T_MAX_AVG=$(awk '/^\[typing\]/{f=1;next}/^\[/{f=0}f' "$BASELINES" | grep 'max_frame_avg_us' | grep -oE '[0-9]+')
    T_MAX_MAX=$(awk '/^\[typing\]/{f=1;next}/^\[/{f=0}f' "$BASELINES" | grep 'max_frame_max_us' | grep -oE '[0-9]+')
    T_MAX_WALK=$(awk '/^\[typing\]/{f=1;next}/^\[/{f=0}f' "$BASELINES" | grep 'max_walk_avg_us' | grep -oE '[0-9]+')

    if [ -n "$T_MAX_AVG" ] && [ "$FRAME_AVG" -le "$T_MAX_AVG" ]; then
        pass "frame avg ${FRAME_AVG}us <= ${T_MAX_AVG}us"
    elif [ -n "$T_MAX_AVG" ]; then
        fail "frame avg ${FRAME_AVG}us > ${T_MAX_AVG}us"
    fi

    if [ -n "$T_MAX_MAX" ] && [ "$FRAME_MAX" -le "$T_MAX_MAX" ]; then
        pass "frame max ${FRAME_MAX}us <= ${T_MAX_MAX}us"
    elif [ -n "$T_MAX_MAX" ]; then
        fail "frame max ${FRAME_MAX}us > ${T_MAX_MAX}us"
    fi

    if [ -n "$T_MAX_WALK" ] && [ "$WALK_AVG" -le "$T_MAX_WALK" ]; then
        pass "walk avg ${WALK_AVG}us <= ${T_MAX_WALK}us"
    elif [ -n "$T_MAX_WALK" ]; then
        fail "walk avg ${WALK_AVG}us > ${T_MAX_WALK}us"
    fi
else
    info "no typing period detected (no line with >50 renders)"
fi

# Idle periods: lines where idle > renders (idle-dominant) and s<=1
echo ""
IDLE_LINE=$(echo "$COMP_LINES" | awk -F'[ =]' '{
    for(i=1;i<=NF;i++){
        if($i=="r") r=$(i+1)+0
        if($i=="idle") idle=$(i+1)+0
        if($i=="s") s=$(i+1)+0
    }
    if(idle>r && s<=1){print; exit}
}')
if [ -n "$IDLE_LINE" ]; then
    parse_comp "$IDLE_LINE"
    TOTAL=$((R + IDLE))
    if [ "$TOTAL" -gt 0 ]; then
        IDLE_PCT=$((IDLE * 100 / TOTAL))
    else
        IDLE_PCT=0
    fi
    printf "  ${BOLD}Idle${RESET}    r=%s  idle=%s (%s%%)  frame=%s/%sus\n" \
        "$R" "$IDLE" "$IDLE_PCT" "$FRAME_AVG" "$FRAME_MAX"

    I_MAX_R=$(awk '/^\[idle\]/{f=1;next}/^\[/{f=0}f' "$BASELINES" | grep 'max_renders_per_sec' | grep -oE '[0-9]+')
    I_MIN_IDLE=$(awk '/^\[idle\]/{f=1;next}/^\[/{f=0}f' "$BASELINES" | grep 'min_idle_pct' | grep -oE '[0-9]+')

    if [ -n "$I_MAX_R" ] && [ "$R" -le "$I_MAX_R" ]; then
        pass "renders $R <= $I_MAX_R/s"
    elif [ -n "$I_MAX_R" ]; then
        fail "renders $R > $I_MAX_R/s"
    fi

    if [ -n "$I_MIN_IDLE" ] && [ "$IDLE_PCT" -ge "$I_MIN_IDLE" ]; then
        pass "idle ${IDLE_PCT}% >= ${I_MIN_IDLE}%"
    elif [ -n "$I_MIN_IDLE" ]; then
        fail "idle ${IDLE_PCT}% < ${I_MIN_IDLE}%"
    fi
else
    info "no idle period detected (no line with idle > 50)"
fi

# ── Summary ────────────────────────────────────────────────────────

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [ "$FAILURES" -eq 0 ]; then
    printf "${GREEN}render benchmark passed${RESET}\n"
else
    printf "${RED}%d render benchmark failures${RESET}\n" "$FAILURES"
fi
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

exit "$FAILURES"
