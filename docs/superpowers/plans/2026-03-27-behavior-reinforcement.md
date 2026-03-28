# Behavior Reinforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement a 5-layer defense-in-depth system that reinforces Claude's working behaviors through mechanical enforcement, advisory protocol, and signal optimization.

**Architecture:** Project-level hooks enforce critical behaviors (test-before-commit gate, visual verification advisory, first-edit reminder). A consolidated Working Protocol replaces 19 redundant memories at the top of CLAUDE.md. Project history moves to STATUS.md. Two targeted company-os claims reinforce key principles. Memory index is cleaned up.

**Tech Stack:** Node.js (hook scripts), Markdown (CLAUDE.md, STATUS.md, MEMORY.md), company-os MCP server (claims)

**Spec:** `docs/superpowers/specs/2026-03-27-behavior-reinforcement-design.md`

---

## File Structure

### Create
- `/Users/user/.claude/hooks/os-pre-commit-gate.js` — Combined pre-commit hook: test gate (1a) + visual advisory (1b)
- `/Users/user/.claude/hooks/os-post-edit-reminder.js` — First-edit-per-session reminder (1c)
- `/Users/user/.claude/hooks/os-post-bash-tracker.js` — Test pass/fail + visual verification marker writer (1d + visual)
- `/Users/user/.claude/projects/-Users-user-Sites-os/settings.json` — Project-level hook config
- `STATUS.md` (project root) — Extracted project history from CLAUDE.md

### Execution Order (IMPORTANT)

Tasks 1-3 create hook scripts but do NOT register them. Tasks 4-5 make documentation commits (STATUS.md, CLAUDE.md) while hooks are inactive. Task 6 registers hooks in settings.json, activating enforcement. Tasks 7-9 are independent cleanup. Task 10 verifies everything.

```
[1-3] Create hook scripts (not yet active)
  → [4-5] Documentation commits (no gate blocking)
    → [6] Register hooks (enforcement begins)
      → [7-9] Claims + memory cleanup
        → [10] End-to-end verification
```

### Modify
- `CLAUDE.md` — Add Working Protocol at top, remove history, trim
- `/Users/user/.claude/projects/-Users-user-Sites-os/memory/MEMORY.md` — Remove dead entries, reorganize

### Delete (19 redundant feedback memories)
- `feedback_perfect_foundation.md`, `feedback_thoroughness.md`, `feedback_correctness_over_speed.md`
- `feedback_quality_standard.md`, `feedback_production_grade_only.md`, `feedback_no_intermediate_goals.md`
- `feedback_complete_foundations.md`, `feedback_foundation_up.md`, `feedback_verification_discipline.md`
- `feedback_verification_gaps.md`, `feedback_close_verification_loop.md`, `feedback_test_dont_guess.md`
- `feedback_tooling_before_debugging.md`, `feedback_fractal_interfaces.md`, `feedback_prevention_over_debugging.md`
- `feedback_no_trial_and_error.md`, `feedback_root_cause_over_workaround.md`
- `feedback_thinking_partner.md`, `feedback_working_mode_gap.md`

### MCP calls
- 2x `propose_claim` via company-os MCP server

---

### Task 1: Create the post-bash-tracker hook (Layer 1d + visual verification)

This hook tracks two things: successful test runs (feeds pre-commit gate 1a) and imgdiff.py runs (feeds visual verification advisory 1b). Both write session-scoped marker files.

**Files:**
- Create: `/Users/user/.claude/hooks/os-post-bash-tracker.js`

- [ ] **Step 1: Write the hook script**

```js
#!/usr/bin/env node
// PostToolUse hook: tracks successful cargo test runs + imgdiff.py runs.
// Writes marker files that the pre-commit gate checks.

const fs = require('fs');
const path = require('path');

let input = '';
const stdinTimeout = setTimeout(() => process.exit(0), 3000);
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', () => {
  clearTimeout(stdinTimeout);
  try {
    const data = JSON.parse(input);

    if (data.tool_name !== 'Bash') {
      process.exit(0);
    }

    const command = data.tool_input?.command || '';
    const sessionId = data.session_id || 'default';
    const stdout = data.tool_result?.stdout || '';
    const stderr = data.tool_result?.stderr || '';
    const exitCode = data.tool_result?.exitCode ?? -1;

    // --- Track cargo test runs ---
    if (command.includes('cargo test')) {
      const markerPath = path.join('/tmp', `claude-os-tests-passed-${sessionId}`);
      const hasFailed = stdout.includes('FAILED') || stderr.includes('FAILED');

      if (exitCode === 0 && !hasFailed) {
        fs.writeFileSync(markerPath, new Date().toISOString());
      } else {
        try { fs.unlinkSync(markerPath); } catch {}
      }
    }

    // --- Track imgdiff.py runs (visual verification) ---
    if (command.includes('imgdiff.py')) {
      const markerPath = path.join('/tmp', `claude-os-visual-verified-${sessionId}`);
      fs.writeFileSync(markerPath, new Date().toISOString());
    }
  } catch {
    // Silent fail — never interfere with workflow
  }
  process.exit(0);
});
```

- [ ] **Step 2: Test — passing cargo test writes marker**

```bash
echo '{"tool_name":"Bash","tool_input":{"command":"cargo test -- --test-threads=1"},"tool_result":{"stdout":"test result: ok. 2257 passed","stderr":"","exitCode":0},"session_id":"test123"}' | node /Users/user/.claude/hooks/os-post-bash-tracker.js
test -f /tmp/claude-os-tests-passed-test123 && echo "PASS: marker created" || echo "FAIL: no marker"
```

- [ ] **Step 3: Test — failing cargo test removes marker**

```bash
echo '{"tool_name":"Bash","tool_input":{"command":"cargo test -- --test-threads=1"},"tool_result":{"stdout":"test result: FAILED. 1 passed; 1 failed","stderr":"","exitCode":101},"session_id":"test123"}' | node /Users/user/.claude/hooks/os-post-bash-tracker.js
test -f /tmp/claude-os-tests-passed-test123 && echo "FAIL: marker still exists" || echo "PASS: marker removed"
```

- [ ] **Step 4: Test — imgdiff.py run writes visual marker**

```bash
echo '{"tool_name":"Bash","tool_input":{"command":"python3 system/test/imgdiff.py /tmp/screenshot.png"},"tool_result":{"stdout":"page edges: left=1164 right=1836","stderr":"","exitCode":0},"session_id":"test123"}' | node /Users/user/.claude/hooks/os-post-bash-tracker.js
test -f /tmp/claude-os-visual-verified-test123 && echo "PASS: visual marker created" || echo "FAIL: no visual marker"
```

- [ ] **Step 5: Test — non-test/non-imgdiff command is ignored**

```bash
rm -f /tmp/claude-os-tests-passed-test999 /tmp/claude-os-visual-verified-test999
echo '{"tool_name":"Bash","tool_input":{"command":"ls -la"},"tool_result":{"stdout":"...","exitCode":0},"session_id":"test999"}' | node /Users/user/.claude/hooks/os-post-bash-tracker.js
test -f /tmp/claude-os-tests-passed-test999 && echo "FAIL" || echo "PASS: no test marker"
test -f /tmp/claude-os-visual-verified-test999 && echo "FAIL" || echo "PASS: no visual marker"
```

- [ ] **Step 6: Clean up test markers**

```bash
rm -f /tmp/claude-os-tests-passed-test123 /tmp/claude-os-visual-verified-test123 /tmp/claude-os-tests-passed-test999 /tmp/claude-os-visual-verified-test999
```

Note: Hook scripts live in the user's home directory. Do NOT commit them to the git repo.

---

### Task 2: Create the pre-commit gate hook (Layer 1a + 1b)

Combined script: hard gate for missing tests, advisory for display pipeline visual verification.

**Files:**
- Create: `/Users/user/.claude/hooks/os-pre-commit-gate.js`

- [ ] **Step 1: Write the hook script**

```js
#!/usr/bin/env node
// PreToolUse hook: gates git commits on test evidence + advises on visual verification.
// 1a: BLOCKS commit if no cargo test has passed this session (exit 2)
// 1b: ADVISES if display pipeline files staged without visual verification (exit 0)

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

let input = '';
const stdinTimeout = setTimeout(() => process.exit(0), 3000);
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', () => {
  clearTimeout(stdinTimeout);
  try {
    const data = JSON.parse(input);

    // Only process Bash tool calls
    if (data.tool_name !== 'Bash') {
      process.exit(0);
    }

    const command = data.tool_input?.command || '';

    // Only inspect git commit commands
    if (!command.match(/git\s+commit/)) {
      process.exit(0);
    }

    const sessionId = data.session_id || 'default';

    // --- 1a: Test evidence gate (BLOCKING) ---
    const testMarker = path.join('/tmp', `claude-os-tests-passed-${sessionId}`);
    let testsRun = false;
    try {
      const stat = fs.statSync(testMarker);
      // Check marker is from today (not stale from previous session)
      const markerDate = new Date(stat.mtime).toDateString();
      const today = new Date().toDateString();
      testsRun = (markerDate === today);
    } catch {
      testsRun = false;
    }

    if (!testsRun) {
      const output = {
        hookSpecificOutput: {
          hookEventName: 'PreToolUse',
          permissionDecision: 'deny',
          permissionDecisionReason:
            'Tests have not been run this session. Run the full test suite ' +
            '(cd system/test && cargo test -- --test-threads=1) before committing.',
        },
      };
      process.stdout.write(JSON.stringify(output));
      process.exit(2);
    }

    // --- 1b: Display pipeline visual verification advisory ---
    const DISPLAY_PATHS = [
      'drawing/', 'render/', 'scene/', 'metal-render/',
      'cpu-render/', 'virgil-render/', 'core/',
    ];

    let stagedFiles = '';
    try {
      stagedFiles = execSync('git diff --cached --name-only', {
        encoding: 'utf8',
        timeout: 5000,
        cwd: '/Users/user/Sites/os',
      });
    } catch {
      // If git diff fails, skip the advisory
      process.exit(0);
    }

    const hasDisplayChanges = DISPLAY_PATHS.some(p => stagedFiles.includes(p));
    if (!hasDisplayChanges) {
      process.exit(0);
    }

    const visualMarker = path.join('/tmp', `claude-os-visual-verified-${sessionId}`);
    let visualVerified = false;
    try {
      const stat = fs.statSync(visualMarker);
      const markerDate = new Date(stat.mtime).toDateString();
      const today = new Date().toDateString();
      visualVerified = (markerDate === today);
    } catch {
      visualVerified = false;
    }

    if (!visualVerified) {
      const output = {
        hookSpecificOutput: {
          hookEventName: 'PreToolUse',
          additionalContext:
            'Display pipeline files are staged for commit. Have you visually ' +
            'verified the changes with imgdiff.py? If so, this is just a ' +
            'reminder. If not, capture screenshots and run numerical ' +
            'verification before committing.',
        },
      };
      process.stdout.write(JSON.stringify(output));
    }
  } catch {
    // Silent fail — never block on hook errors
  }
  process.exit(0);
});
```

- [ ] **Step 2: Test the gate — commit without tests (should BLOCK)**

```bash
rm -f /tmp/claude-os-tests-passed-test456
echo '{"tool_name":"Bash","tool_input":{"command":"git commit -m test"},"session_id":"test456"}' | node /Users/user/.claude/hooks/os-pre-commit-gate.js; echo "exit: $?"
# Expected: exit code 2, JSON with permissionDecision: "deny"
```

- [ ] **Step 3: Test the gate — commit WITH tests (should ALLOW)**

```bash
echo "$(date -Iseconds)" > /tmp/claude-os-tests-passed-test456
echo '{"tool_name":"Bash","tool_input":{"command":"git commit -m test"},"session_id":"test456"}' | node /Users/user/.claude/hooks/os-pre-commit-gate.js; echo "exit: $?"
# Expected: exit code 0 (allowed)
```

- [ ] **Step 4: Test non-commit command (should be ignored)**

```bash
echo '{"tool_name":"Bash","tool_input":{"command":"cargo build --release"},"session_id":"test456"}' | node /Users/user/.claude/hooks/os-pre-commit-gate.js; echo "exit: $?"
# Expected: exit code 0, no output
```

- [ ] **Step 5: Test the 1b visual advisory — display files staged without visual verification**

This test requires being in a git repo. Run from the OS project root:
```bash
# Set up: tests passed (so 1a allows), but no visual verification
echo "$(date -Iseconds)" > /tmp/claude-os-tests-passed-test456
rm -f /tmp/claude-os-visual-verified-test456
# Stage a display pipeline file (use a harmless touch + add)
touch /tmp/test-1b-advisory.txt
cd /Users/user/Sites/os
echo '{"tool_name":"Bash","tool_input":{"command":"git commit -m test"},"session_id":"test456"}' | node /Users/user/.claude/hooks/os-pre-commit-gate.js
# Expected: exit 0 with JSON containing additionalContext about "Display pipeline files"
# Note: this test depends on whether display pipeline files are actually staged.
# If no display files are staged, the advisory won't fire (correct behavior).
# To fully test: stage a file in system/services/core/, then run the hook.
```

- [ ] **Step 6: Clean up test markers**

```bash
rm -f /tmp/claude-os-tests-passed-test456 /tmp/claude-os-visual-verified-test456
```

---

### Task 3: Create the post-first-edit reminder hook (Layer 1c)

**Files:**
- Create: `/Users/user/.claude/hooks/os-post-edit-reminder.js`

- [ ] **Step 1: Write the hook script**

```js
#!/usr/bin/env node
// PostToolUse hook: one-time reminder on first Edit/Write per session.
// Asks: did you read the files, trace effects, identify tests?

const fs = require('fs');
const path = require('path');

let input = '';
const stdinTimeout = setTimeout(() => process.exit(0), 3000);
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', () => {
  clearTimeout(stdinTimeout);
  try {
    const data = JSON.parse(input);

    // Only process Edit and Write tool completions
    if (data.tool_name !== 'Edit' && data.tool_name !== 'Write') {
      process.exit(0);
    }

    // Only fire for files in the OS project
    const filePath = data.tool_input?.file_path || '';
    if (!filePath.includes('/Sites/os/')) {
      process.exit(0);
    }

    const sessionId = data.session_id || 'default';
    const markerPath = path.join('/tmp', `claude-os-first-edit-fired-${sessionId}`);

    // Already fired this session — exit silently
    if (fs.existsSync(markerPath)) {
      process.exit(0);
    }

    // Mark as fired
    fs.writeFileSync(markerPath, new Date().toISOString());

    // Inject one-time reminder
    const output = {
      hookSpecificOutput: {
        hookEventName: 'PostToolUse',
        additionalContext:
          'WORKING PROTOCOL REMINDER: Before continuing, verify that you have ' +
          '(1) read all files this change affects, (2) traced all downstream ' +
          'effects, (3) identified or written tests that will verify correctness. ' +
          'If not, stop editing and do that now.',
      },
    };
    process.stdout.write(JSON.stringify(output));
  } catch {
    // Silent fail
  }
  process.exit(0);
});
```

- [ ] **Step 2: Test — first edit fires reminder**

```bash
rm -f /tmp/claude-os-first-edit-fired-test789
echo '{"tool_name":"Edit","tool_input":{"file_path":"/Users/user/Sites/os/system/foo.rs"},"session_id":"test789"}' | node /Users/user/.claude/hooks/os-post-edit-reminder.js
# Expected: JSON with additionalContext containing "WORKING PROTOCOL REMINDER"
```

- [ ] **Step 3: Test — second edit is silent**

```bash
echo '{"tool_name":"Edit","tool_input":{"file_path":"/Users/user/Sites/os/system/bar.rs"},"session_id":"test789"}' | node /Users/user/.claude/hooks/os-post-edit-reminder.js
# Expected: no output (marker already exists)
```

- [ ] **Step 4: Test — edit outside OS project is ignored**

```bash
rm -f /tmp/claude-os-first-edit-fired-test790
echo '{"tool_name":"Edit","tool_input":{"file_path":"/Users/user/Sites/other-project/foo.rs"},"session_id":"test790"}' | node /Users/user/.claude/hooks/os-post-edit-reminder.js
# Expected: no output, no marker created
ls /tmp/claude-os-first-edit-fired-test790 2>&1
# Expected: "No such file or directory"
```

- [ ] **Step 5: Clean up test markers**

```bash
rm -f /tmp/claude-os-first-edit-fired-test789 /tmp/claude-os-first-edit-fired-test790
```

---

### Task 4: Create STATUS.md from CLAUDE.md history

Extract the "Where We Left Off" section and related history into a standalone file.

**Files:**
- Create: `STATUS.md` (project root: `/Users/user/Sites/os/STATUS.md`)
- Reference: `CLAUDE.md` lines 74-145 (the content to extract)

- [ ] **Step 1: Write STATUS.md**

Extract content from CLAUDE.md lines 74-145 ("Where We Left Off" through end of milestone roadmap) into STATUS.md with the structure defined in the spec:
- Heading with last-updated date
- Current State
- Architecture
- Completed Milestones
- Open Questions
- Known Issues

Preserve all the technical detail — this is the same content, reorganized.

- [ ] **Step 2: Verify STATUS.md has all the information from the extracted CLAUDE.md sections**

Spot-check: v0.4 Document Store details, Content Pipeline Architecture, Phase 4 details, Architecture section, IPC mechanisms, Open design questions, Milestone roadmap, System code paths.

- [ ] **Step 3: Commit STATUS.md**

```bash
git add STATUS.md
git commit -m "docs: extract project history from CLAUDE.md to STATUS.md"
```

---

### Task 5: Restructure CLAUDE.md

Add Working Protocol at top, remove extracted history, trim redundant sections.

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add Working Protocol as the first section after the title**

Insert immediately after `# Project: Document-Centric OS`, before `## What This Is`:

```markdown
## Working Protocol (MANDATORY)

These rules govern how you work on this project. They are not preferences — they are requirements. Violating them wastes the user's time and erodes trust.

### 1. Understand before acting

- Read every file you will modify, AND every file that depends on it
- Trace all downstream effects of the change before writing code
- If the problem has known algorithms or prior art, research them from authoritative sources (specs, papers, reference implementations) — never improvise when a solution exists
- Never guess an API, syscall, instruction encoding, or wire format — look it up in the actual source or documentation. Wrong assumptions cascade silently.

### 2. Build bottom-up

- Complete the current architectural layer before starting the next
- No scaffolding, no "good enough for now," no "fix later" — production-grade from the first line
- Each component should work as a standalone, world-class library behind a clean interface

### 3. Verify everything yourself

- Write or identify tests BEFORE implementing. Watch them fail. Implement. Watch them pass.
- Run the FULL test suite, not just tests you think are relevant
- For display changes: capture screenshots, run imgdiff.py, report numbers — never eyeball
- Trace every affected code path. Finding A bug is not the same as finding THE bug.
- Never declare "done" without evidence.
- If verification tooling doesn't exist for a change, STOP. Building the tooling becomes the immediate priority. Push the original task onto the stack, build what's needed to verify, then resume. Unverifiable work does not ship — no exceptions.

### 4. Fix root causes, not symptoms

- When something breaks, diagnose the actual cause — don't patch the surface
- When fixing a bug, check for the same class of bug in related code
- If an interface is confusing enough to cause a bug, STOP and flag it — interfaces are architectural decisions in this project. Propose the fix, don't silently apply it.
```

Also add after the Working Protocol: `Read STATUS.md at session start for current project state and session resume context.`

- [ ] **Step 2: Remove the "Where We Left Off" section**

Delete lines 74-145 of the current CLAUDE.md (everything from `## Where We Left Off` through the end of the milestone roadmap and system code listing). This content now lives in STATUS.md.

- [ ] **Step 3: Trim "Working Mode" to remove overlap with Working Protocol**

The Working Protocol now covers:
- "Research partner" → covered by Protocol 1 (understand before acting)
- Autonomous execution quality → covered by Protocol 2 (build bottom-up) and Protocol 3 (verify)

Keep the non-overlapping Working Mode bullets: explore don't push, hold context, connect the dots, guide gently, respect the pace.

- [ ] **Step 4: Merge "Project Phase" into "What This Is"**

Replace the separate `## Project Phase` section (lines 8-9 of current CLAUDE.md) by appending its content to `## What This Is`. Target result:

```markdown
## What This Is

A personal project exploring an alternative operating system design where documents (files) are first-class citizens and applications are interchangeable tools that attach to content. This is a learning/exploration project, not a product. Currently in the design phase with research spikes — code is written selectively to validate assumptions or flesh out settled decisions.
```

Delete the `## Project Phase` heading entirely.

- [ ] **Step 5: Update the "Hold context" bullet in Working Mode**

Change the reference from "Where We Left Off" to "STATUS.md":
```markdown
- **Hold context across sessions.** Use MEMORY.md, the exploration journal, and STATUS.md to resume seamlessly.
```

- [ ] **Step 6: Verify the restructured CLAUDE.md**

```bash
wc -l CLAUDE.md
# Target: ~250 lines (down from 350)

head -5 CLAUDE.md
# Expected: line 1 = "# Project: Document-Centric OS", line 3 = "## Working Protocol (MANDATORY)"

grep -c "Where We Left Off" CLAUDE.md
# Expected: 0 (section removed)

grep -c "STATUS.md" CLAUDE.md
# Expected: at least 1 (reference exists)

grep -c "MANDATORY" CLAUDE.md
# Expected: at least 5 (Working Protocol, Kernel, Rust Formatting, Visual Testing, Rendering Pipeline)

grep -c "Settled Decisions" CLAUDE.md
# Expected: at least 1 (preserved)
```

- [ ] **Step 7: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: restructure CLAUDE.md — Working Protocol at top, history to STATUS.md"
```

---

### Task 6: Create project-level settings.json with hook registration

All documentation commits (Tasks 4-5) are complete. Now safe to activate enforcement.

**Files:**
- Create: `/Users/user/.claude/projects/-Users-user-Sites-os/settings.json`

- [ ] **Step 1: Write the project settings file**

Use absolute paths (matching the pattern in global `~/.claude/settings.json`):

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "node /Users/user/.claude/hooks/os-pre-commit-gate.js",
            "timeout": 10
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "node /Users/user/.claude/hooks/os-post-edit-reminder.js",
            "timeout": 5
          }
        ]
      },
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "node /Users/user/.claude/hooks/os-post-bash-tracker.js",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

- [ ] **Step 2: Verify the file is valid JSON**

```bash
python3 -m json.tool /Users/user/.claude/projects/-Users-user-Sites-os/settings.json > /dev/null && echo "valid" || echo "INVALID"
```

- [ ] **Step 3: Verify hooks don't conflict with global settings**

The global `~/.claude/settings.json` has:
- `PreToolUse` matcher `"Write|Edit"` for GSD prompt guard
- `PostToolUse` matcher `"Bash|Edit|Write|Agent|Task"` for GSD context monitor

Project hooks add:
- `PreToolUse` on `"Bash"` (new — global only has `"Write|Edit"`)
- `PostToolUse` on `"Edit|Write"` and `"Bash"` (merges with existing GSD hooks)

Project-level hooks merge with (not replace) global hooks. No conflicts.

---

### Task 7: Add targeted company-os claims

**Files:** None (MCP calls only)

- [ ] **Step 1: Propose claim — research from authoritative sources**

Use the company-os MCP tool `propose_claim`:
```
statement: "Never implement from general knowledge when a specification exists. Research algorithms from authoritative sources. Look up APIs, syscalls, instruction encodings, and wire formats in actual documentation or source code. Wrong assumptions cascade silently."
claim_type: normative
status: signal
confidence: certain
scope: [engineering]
```

- [ ] **Step 2: Propose claim — verification tooling gaps block progress**

Use the company-os MCP tool `propose_claim`:
```
statement: "If a change cannot be verified with existing tooling, building the verification tooling becomes the immediate priority. Unverifiable work does not ship."
claim_type: normative
status: signal
confidence: certain
scope: [engineering]
```

- [ ] **Step 3: Verify claims appear in the database**

Use `search_claims` to confirm both new claims are present and will be picked up by the SessionStart hook.

---

### Task 8: Delete redundant feedback memories

**Files:**
- Delete: 19 files in `/Users/user/.claude/projects/-Users-user-Sites-os/memory/`

- [ ] **Step 1: Delete the 19 redundant feedback memory files**

```bash
cd /Users/user/.claude/projects/-Users-user-Sites-os/memory
rm -f feedback_perfect_foundation.md feedback_thoroughness.md \
  feedback_correctness_over_speed.md feedback_quality_standard.md \
  feedback_production_grade_only.md feedback_no_intermediate_goals.md \
  feedback_complete_foundations.md feedback_foundation_up.md \
  feedback_verification_discipline.md feedback_verification_gaps.md \
  feedback_close_verification_loop.md feedback_test_dont_guess.md \
  feedback_tooling_before_debugging.md feedback_fractal_interfaces.md \
  feedback_prevention_over_debugging.md feedback_no_trial_and_error.md \
  feedback_root_cause_over_workaround.md feedback_thinking_partner.md \
  feedback_working_mode_gap.md
```

- [ ] **Step 2: Verify exactly 5 feedback files remain**

```bash
ls /Users/user/.claude/projects/-Users-user-Sites-os/memory/feedback_*.md
# Expected exactly 5 files:
# feedback_a11y_first_class.md
# feedback_comprehensive_test_content.md
# feedback_hypervisor_visual_testing.md
# feedback_rust_formatting.md
# feedback_virgl_visual_testing.md
```

- [ ] **Step 3: Also remove the phantom entry — feedback_most_correct.md doesn't exist but is referenced**

Verify it doesn't exist (it shouldn't — it was only in MEMORY.md, never created):
```bash
ls /Users/user/.claude/projects/-Users-user-Sites-os/memory/feedback_most_correct.md 2>&1
# Expected: "No such file or directory"
```

---

### Task 9: Update MEMORY.md index

**Files:**
- Modify: `/Users/user/.claude/projects/-Users-user-Sites-os/memory/MEMORY.md`

- [ ] **Step 1: Rewrite MEMORY.md**

Remove all 19 deleted feedback entries + 1 phantom entry. Remove inline feedback bullets (lines 107-119 of current MEMORY.md that reference deleted files). Reorganize remaining entries by type. Keep the non-feedback sections intact.

Target structure:
```markdown
# OS Project Memory

## Project Overview
(keep as-is)

## Milestone Status
(keep v0.4, v0.3, v0.5 sections as-is)

## Hypervisor
(keep as-is)

## Architecture Decisions
(keep all decision_*.md entries as-is)

## Design Principles
(keep a11y, system-as-compound-doc entries)

## Build Infrastructure
(keep as-is)

## References
(keep virgl-qemu, pipeline-completion, points-pixels, path-antialiasing, font-rendering)

## Process & Preferences
(keep 5 surviving feedback files + inline non-linked preferences)

## Working Mode
(keep as-is)

## Development Paths
(keep as-is)

## Known Issues
(keep as-is)
```

- [ ] **Step 2: Verify no dead links**

```bash
# Extract all .md links from MEMORY.md and check each exists
grep -oP '\(([^)]+\.md)\)' /Users/user/.claude/projects/-Users-user-Sites-os/memory/MEMORY.md | tr -d '()' | while read f; do
  ls /Users/user/.claude/projects/-Users-user-Sites-os/memory/"$f" 2>/dev/null || echo "DEAD LINK: $f"
done
# Expected: no "DEAD LINK" output
```

- [ ] **Step 3: Count lines**

```bash
wc -l /Users/user/.claude/projects/-Users-user-Sites-os/memory/MEMORY.md
# Target: ~60 lines (down from ~144)
```

---

### Task 10: End-to-end verification

- [ ] **Step 1: Verify hook scripts are executable and parse valid JSON**

```bash
for f in os-pre-commit-gate.js os-post-edit-reminder.js os-post-bash-tracker.js; do
  echo "--- $f ---"
  node -c ~/.claude/hooks/$f && echo "syntax OK" || echo "SYNTAX ERROR"
done
```

- [ ] **Step 2: Verify project settings.json is valid**

```bash
python3 -m json.tool /Users/user/.claude/projects/-Users-user-Sites-os/settings.json > /dev/null && echo "valid" || echo "INVALID"
```

- [ ] **Step 3: Verify CLAUDE.md structure**

- Working Protocol is the FIRST section (after title line)
- No "Where We Left Off" section
- STATUS.md reference present
- All MANDATORY sections present (Kernel, Rust Formatting, Visual Testing, Rendering Pipeline)
- Line count ~250

- [ ] **Step 4: Verify STATUS.md exists and has all extracted content**

- v0.4 details present
- Architecture summary present
- Open questions present
- Milestone roadmap present

- [ ] **Step 5: Verify memory cleanup**

- Exactly 5 feedback_*.md files remain
- MEMORY.md has ~60 lines
- No dead links in MEMORY.md

- [ ] **Step 6: Verify claims were added**

Use `search_claims` to confirm 44 total normative claims, including the 2 new ones.

- [ ] **Step 7: Summary report**

Print a summary of all changes made:
- Hook files created (3)
- Project settings.json created
- CLAUDE.md restructured (line count before/after)
- STATUS.md created (line count)
- Memories deleted (19) / remaining feedback (5)
- MEMORY.md trimmed (line count before/after)
- Claims added (2, total now 44)
