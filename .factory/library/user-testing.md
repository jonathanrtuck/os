# User Testing

**What belongs here:** Testing surface, tools, URLs, setup steps, isolation notes, known quirks.

---

## Testing Surface

This is a bare-metal kernel. There is no web UI, API, or interactive application to test. "User testing" for this mission means running automated verification commands.

## Tools

- **Terminal commands** (cargo test, cargo build) — primary verification method
- **QEMU** (stress-test.sh, crash-test.sh) — for on-target verification
- No browser testing, no TUI testing needed

## Verification Commands

```bash
# Run all tests (must pass after every feature)
cd system/test && cargo test -- --test-threads=1

# Build kernel (must succeed after every feature)
cd system && cargo build

# Run specific test
cd system/test && cargo test <test_name> -- --test-threads=1

# Headless stress test (30 seconds)
cd system && ./stress-test.sh 30

# Miri (if installed)
cd system/test && cargo +nightly miri test -- --test-threads=1
```

## Test Count Baseline

348 tests across 18 files as of mission start. This count should only increase.

## Known Quirks

- Tests require `--test-threads=1` (some tests use global state)
- Tests duplicate/stub kernel logic rather than importing it directly
- 31 compiler warnings (dead code) are pre-existing and expected
- Miri is not installed by default; needs `rustup component add miri`

## Flow Validator Guidance: Terminal

**Surface:** All assertions for this mission are verified via terminal commands (cargo test, cargo build, cargo miri test, shell scripts).

**Isolation:** Terminal commands are inherently isolated — no shared user accounts, sessions, or mutable state between runs. Multiple flow validators can run in parallel since they only read build artifacts and run tests.

**Boundaries:**
- Do NOT modify any source files — validators only verify current state
- Run commands from the repo root at `/Users/user/Sites/os`
- Always use `--test-threads=1` for cargo test
- Report exact exit codes and output excerpts as evidence

**Miri notes:**
- Miri has been assessed during infra verification
- 330/348 tests pass clean under Miri
- buddy test fails due to Miri provenance limitation (not real UB)
- ipc test finds UB in library code (out of kernel scope)
- scheduler_state takes ~1400s under Miri — may need reduced iterations
