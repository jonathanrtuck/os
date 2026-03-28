# Behavior Reinforcement: Defense-in-Depth Design

**Date:** 2026-03-27
**Status:** Approved
**Scope:** Project-scoped (OS project), with future extraction to global

## Problem

Claude's default behavior drifts toward quick implementation without sufficient research, verification, or bottom-up discipline. This pattern has been corrected ~23 times across separate memory files, but the behavior doesn't persist because:

1. Each conversation starts stateless — no persistent neural change between sessions
2. Training priors (median developer behavior) are strong and override weak advisory signals
3. Scattered feedback memories dilute each other rather than reinforcing
4. No mechanical enforcement exists for the highest-cost behaviors

The most expensive failure mode: declaring work done without verifying it actually works (not running tests, eyeballing screenshots, not tracing downstream effects).

## Approach: Defense-in-Depth

Five reinforcement layers, each covering different failure modes through different mechanisms. If one layer fails, the others catch it.

## Layer 1: Mechanical Enforcement (Hooks)

Create a new project-level settings file at `.claude/projects/-Users-user-Sites-os/settings.json`. This file does not currently exist. Project-level hooks merge with (not replace) global hooks from `~/.claude/settings.json`.

### Hook implementation pattern

All hooks use `matcher` to match the **tool name** (e.g., `"Bash"`, `"Edit|Write"`). The matcher is a regex against the tool name only — it cannot inspect tool input. To inspect the actual command or file path, the hook script must parse `tool_input` from the JSON received on stdin.

**Blocking (gate):** Script exits with code 2 and writes JSON to stdout:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Reason shown to Claude"
  }
}
```

**Advisory (context injection):** Script exits with code 0 and writes JSON to stdout:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "additionalContext": "Message injected into conversation"
  }
}
```

**Implementation risk:** Exit code 2 blocking has no working reference in this codebase (all existing hooks use exit 0). Test the gate contract early in implementation.

### 1a. Pre-commit gate: require test evidence

- **Trigger:** `PreToolUse`, matcher: `"Bash"`. Script inspects `tool_input.command` for substring `git commit`.
- **Action:** Check for `/tmp/claude-os-tests-passed-{session_id}` marker file. If missing, block (exit 2) with: "Tests have not been run this session. Run the full test suite first."
- **Rationale:** Commits without test evidence are always wrong. No false positive risk.

### 1b. Pre-commit advisory: display pipeline visual verification

- **Trigger:** `PreToolUse`, matcher: `"Bash"`. Script inspects `tool_input.command` for substring `git commit`.
- **Action:** Run `git diff --cached --name-only` to check if staged files include display pipeline paths (`drawing/`, `render/`, `scene/`, `metal-render/`, `cpu-render/`, `virgil-render/`, `core/`). If yes, check for `/tmp/claude-os-visual-verified-{session_id}` marker. If missing, inject advisory via `additionalContext` (exit 0, not exit 2): "Display pipeline files changed — have you visually verified with imgdiff.py?"
- **Rationale:** Advisory, not gate, because not all changes to these files are visual. But the reminder catches the common case.

### 1c. Post-first-edit reminder

- **Trigger:** `PostToolUse`, matcher: `"Edit|Write"`. Script checks for `/tmp/claude-os-first-edit-fired-{session_id}` — if it exists, exit 0 silently (already fired this session).
- **Action:** Create the marker file, then inject one-time context via `additionalContext`: "REMINDER: Have you (1) read all files this change affects, (2) traced downstream effects, (3) identified or written tests? If not, stop and do that now."
- **Rationale:** One-time per session to avoid noise. Catches the most common failure: diving into code before understanding context.

### 1d. Post-test tracker

- **Trigger:** `PostToolUse`, matcher: `"Bash"`. Script inspects `tool_input.command` for substring `cargo test` (catches `cargo test`, `cargo test -- --test-threads=1`, `cargo test <specific>`, and chained commands like `cd system/test && cargo test`).
- **Action:** Inspect `tool_result.stdout` or exit status. If tests passed (exit code 0, no "FAILED" in output), write marker file `/tmp/claude-os-tests-passed-{session_id}`. If tests failed, remove the marker if it exists.
- **Rationale:** Feeds the pre-commit gate (1a). Only successful test runs count.
- **Note:** The exact field name for exit status in `PostToolUse` JSON needs to be verified during implementation by examining the actual payload.

### 1a+1b combination

Hooks 1a and 1b both trigger on the same matcher (`PreToolUse` on `Bash`) and both inspect for `git commit`. They can be implemented as a single script that performs both checks sequentially: first the hard gate (1a — tests), then the advisory (1b — visual verification). If 1a blocks, 1b never fires.

### Marker file lifecycle

All markers use `/tmp/claude-os-*-{session_id}` prefix, where `session_id` comes from the hook's stdin JSON. This prevents collision between concurrent Claude Code sessions on the same project. Markers persist within a session but are naturally cleaned on reboot. The pre-commit gate checks existence and age (must be from current day) to avoid stale markers from previous sessions.

## Layer 2: Working Protocol (CLAUDE.md)

A new mandatory section at the **top** of CLAUDE.md, replacing 19 redundant feedback memories with 4 specific, actionable principles.

```markdown
## Working Protocol (MANDATORY)

These rules govern how you work on this project. They are not preferences —
they are requirements. Violating them wastes the user's time and erodes trust.

### 1. Understand before acting

- Read every file you will modify, AND every file that depends on it
- Trace all downstream effects of the change before writing code
- If the problem has known algorithms or prior art, research them from
  authoritative sources (specs, papers, reference implementations) —
  never improvise when a solution exists
- Never guess an API, syscall, instruction encoding, or wire format —
  look it up in the actual source or documentation. Wrong assumptions
  cascade silently.

### 2. Build bottom-up

- Complete the current architectural layer before starting the next
- No scaffolding, no "good enough for now," no "fix later" —
  production-grade from the first line
- Each component should work as a standalone, world-class library
  behind a clean interface

### 3. Verify everything yourself

- Write or identify tests BEFORE implementing. Watch them fail.
  Implement. Watch them pass.
- Run the FULL test suite, not just tests you think are relevant
- For display changes: capture screenshots, run imgdiff.py, report
  numbers — never eyeball
- Trace every affected code path. Finding A bug is not the same as
  finding THE bug.
- Never declare "done" without evidence.
- If verification tooling doesn't exist for a change, STOP. Building
  the tooling becomes the immediate priority. Push the original task
  onto the stack, build what's needed to verify, then resume.
  Unverifiable work does not ship — no exceptions.

### 4. Fix root causes, not symptoms

- When something breaks, diagnose the actual cause — don't patch the
  surface
- When fixing a bug, check for the same class of bug in related code
- If an interface is confusing enough to cause a bug, STOP and flag
  it — interfaces are architectural decisions in this project. Propose
  the fix, don't silently apply it.
```

## Layer 3: CLAUDE.md Restructure

### Current structure (~350 lines, behavioral rules buried)

1. What This Is
2. Project Phase
3. Working Mode
4. Key Design Documents
5. Settled Decisions (×14)
6. Key Architectural Principles
7. Decision Dependencies
8. **Where We Left Off** (~85 lines of project history, plus ~80 lines of architecture/phase summaries)
9. Kernel Change Protocol
10. Rust Formatting Convention
11. Visual Testing
12. Rendering Pipeline Changes
13. Design Discussion Rules
14. Reference Influences

### Target structure (~250 lines, behavioral rules dominate)

1. **Working Protocol (MANDATORY)** — NEW, top position
2. What This Is — trimmed
3. Working Mode — trimmed, overlap with Working Protocol removed
4. Key Design Documents
5. Settled Decisions (×14)
6. Key Architectural Principles
7. Decision Dependencies
8. Kernel Change Protocol (MANDATORY)
9. Rust Formatting Convention (MANDATORY)
10. Visual Testing (MANDATORY)
11. Rendering Pipeline Changes (MANDATORY)
12. Design Discussion Rules — trimmed
13. Reference Influences

### Extracted to STATUS.md

The "Where We Left Off" section and all phase-by-phase history moves to `STATUS.md` in the project root. CLAUDE.md gets a one-liner: "Read STATUS.md at session start for current project state."

**STATUS.md structure:**

- Heading: "Project Status" with last-updated date
- "Current State" — the current milestone focus and what's in progress (equivalent to the first paragraph of "Where We Left Off")
- "Architecture" — settled architecture summary (IPC, rendering pipeline, content pipeline)
- "Completed Milestones" — collapsed summaries of v0.3 and v0.4 work
- "Open Questions" — unresolved design questions carried forward
- "Known Issues" — open bugs and workarounds

This is reference information that Claude reads on-demand for context, not behavioral instructions that need to be in CLAUDE.md's high-attention position.

## Layer 4: Targeted Claims (2 new)

Added via the company-os MCP server's `propose_claim` tool.

### Claim 1: Research from authoritative sources

```text
statement: Never implement from general knowledge when a specification exists.
           Research algorithms from authoritative sources. Look up APIs, syscalls,
           instruction encodings, and wire formats in actual documentation or
           source code. Wrong assumptions cascade silently.
claim_type: normative
status: signal
confidence: certain
scope: [engineering]
```

### Claim 2: Verification tooling gaps block progress

```text
statement: If a change cannot be verified with existing tooling, building the
           verification tooling becomes the immediate priority. Unverifiable
           work does not ship.
claim_type: normative
status: signal
confidence: certain
scope: [engineering]
```

Total claims: 42 → 44.

**Integration:** New claims are added via `propose_claim` through the company-os MCP server. The existing `company-os-claims.mjs` SessionStart hook automatically queries all normative claims and injects them into conversation context. No additional wiring is needed — new claims appear in every subsequent session automatically.

## Layer 5: Memory Cleanup

### Delete (19 redundant feedback memories)

All now consolidated into the Working Protocol:

- `feedback_perfect_foundation.md`
- `feedback_thoroughness.md`
- `feedback_correctness_over_speed.md`
- `feedback_quality_standard.md`
- `feedback_production_grade_only.md`
- `feedback_no_intermediate_goals.md`
- `feedback_complete_foundations.md`
- `feedback_foundation_up.md`
- `feedback_verification_discipline.md`
- `feedback_verification_gaps.md`
- `feedback_close_verification_loop.md`
- `feedback_test_dont_guess.md`
- `feedback_tooling_before_debugging.md`
- `feedback_fractal_interfaces.md`
- `feedback_prevention_over_debugging.md`
- `feedback_no_trial_and_error.md`
- `feedback_root_cause_over_workaround.md`
- `feedback_thinking_partner.md`
- `feedback_working_mode_gap.md`

### Keep (5 unique feedback memories)

- `feedback_a11y_first_class.md` — unique: a11y as first-class principle
- `feedback_rust_formatting.md` — unique: project-specific tooling
- `feedback_virgl_visual_testing.md` — unique: driver-specific testing method
- `feedback_hypervisor_visual_testing.md` — unique: hypervisor capture method
- `feedback_comprehensive_test_content.md` — unique: factory document guidance

### MEMORY.md index

- Remove 19 dead entries + 1 phantom (`feedback_most_correct.md`)
- Reorganize remaining entries by type (decisions, project state, references, feedback)
- Net: ~100 lines → ~60 lines

## What's NOT Changing

- Global rules files (`~/.claude/rules/common/`) — defer to global extraction phase
- Existing hooks (GSD, company-os claims, notifications)
- Non-feedback memories (decisions, project state, references)
- Kernel Change Protocol, Visual Testing, Rendering Pipeline sections of CLAUDE.md
- Any code in the OS project itself

## Success Criteria

Mechanically verifiable:

1. No conversation where I commit code without having run the test suite (enforced by hook 1a)
2. Display pipeline changes include imgdiff.py numbers, not eyeballed screenshots (reminded by hook 1b)
3. Feedback memory count stays at 5, not creeping back up with duplicates (auditable via file count)

Observationally verifiable (require human review of conversation): 4. First-edit reminder fires and I demonstrably pause to read context before continuing 5. When verification tooling is missing, I flag it as a blocker rather than shipping anyway 6. CLAUDE.md Working Protocol is cited in my reasoning when making methodology decisions
