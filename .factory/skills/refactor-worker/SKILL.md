---
name: refactor-worker
description: Moves code between libraries/services while preserving all behavior and tests
---

# Refactor Worker

NOTE: Startup and cleanup are handled by `worker-base`. This skill defines the WORK PROCEDURE.

## When to Use This Skill

Use for features that move, rename, or delete code across library and service boundaries. The invariant is zero behavioral change: every test passes, every pixel is identical.

## Work Procedure

### 1. Understand the Move

Read the feature description carefully. Identify:
- **Source:** Where the code currently lives (file, line range, types/functions)
- **Target:** Where it should end up
- **Callers:** Every file that imports/uses the moved code (grep for type names, function names)
- **Build dependencies:** Whether build.rs, Cargo.toml, or extern crate declarations need updating

### 2. Plan the Edit Sequence

For each move, determine the correct order:
- If adding a new dependency between libraries, update build.rs FIRST (so the dependency exists when callers reference the new location)
- If removing a dependency, update build.rs LAST (after all callers stop using the old path)
- For renames: update the source (directory, Cargo.toml, lib.rs) and all callers atomically

### 3. Execute the Move (TDD-style)

For each logical unit of work:

a. **Verify baseline:** Run `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1` to confirm all tests pass before changes.

b. **Make the structural change:**
   - Copy code to target location (or rename directory)
   - Update imports in the target
   - Update all callers (grep thoroughly for every reference)
   - Remove from source
   - Update build.rs if needed
   - Update test/Cargo.toml if needed

c. **Build check:** Run `cd /Users/user/Sites/os/system && cargo build --release`. Fix any compilation errors immediately.

d. **Test check:** Run `cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1`. All tests must pass. If any fail, the move introduced a behavioral change -- investigate and fix.

### 4. Verify Completeness

After all moves in the feature:
- `grep` for the old import paths across the entire system/ directory to confirm no stragglers
- Check that no `#[allow(dead_code)]` or `#[allow(unused_imports)]` was added to suppress warnings
- Run the full build and test suite one final time

### 5. Commit

Commit all changes with a descriptive message. The commit should be atomic -- the codebase compiles and tests pass at the commit point.

## Critical Rules

1. **Never change function signatures.** When moving a function, its parameter types, return type, and behavior must be identical.
2. **Never change test assertions.** Tests must pass with the same assertions. Do not modify test expected values.
3. **Never add #[ignore] to tests.** If a test fails after a move, the move is wrong.
4. **Grep thoroughly.** When moving type `Foo` from crate `bar`, search for: `bar::Foo`, `use bar::Foo`, `bar::Foo::`, and bare `Foo` in files that have `use bar::*` or `use bar::Foo`.
5. **The include!() pattern:** Drawing uses `include!("file.rs")` to pull sub-files into lib.rs. When removing a file from drawing, remove both the file AND the include!() line. When the included file defines types used elsewhere, those callers must be updated first.
6. **build.rs is critical:** This custom build script compiles libraries via direct rustc invocation. When renaming a library, you must update: the cargo_lib/rustc_rlib call, the --extern flags for downstream crates, the rerun-if-changed directives, and the PROGRAMS extern list.

## Example Handoff

```json
{
  "salientSummary": "Moved 5 layout helpers (layout_mono_lines, byte_to_line_col, scroll_runs, line_bytes_for_run, bytes_to_shaped_glyphs) from scene lib to Core service's scene_state.rs. Updated Core's imports, removed from scene/lib.rs, updated 3 test files. All 1,462 tests pass, full build succeeds.",
  "whatWasImplemented": "Extracted layout_mono_lines, byte_to_line_col, scroll_runs, line_bytes_for_run, bytes_to_shaped_glyphs from libraries/scene/lib.rs into services/core/scene_state.rs as module-level functions. Updated scene_state.rs imports to use local functions instead of scene::. Updated test/tests/scene.rs to import from a new test helper that duplicates the layout functions for testing (since tests can't import from services). Removed all 5 functions and their helper types from scene/lib.rs.",
  "whatWasLeftUndone": "",
  "verification": {
    "commandsRun": [
      {
        "command": "cd /Users/user/Sites/os/system && cargo build --release",
        "exitCode": 0,
        "observation": "Full build succeeds including all rlibs and ELFs"
      },
      {
        "command": "cd /Users/user/Sites/os/system/test && cargo test -- --test-threads=1",
        "exitCode": 0,
        "observation": "1462 tests pass, 0 failures"
      },
      {
        "command": "grep -rn 'fn layout_mono_lines\\|fn byte_to_line_col\\|fn scroll_runs' system/libraries/scene/lib.rs",
        "exitCode": 1,
        "observation": "No layout helpers remain in scene lib"
      }
    ],
    "interactiveChecks": []
  },
  "tests": {
    "added": []
  },
  "discoveredIssues": []
}
```

## When to Return to Orchestrator

- A function being moved has callers you didn't expect (outside the documented scope)
- The build.rs changes cause cascading compilation failures you can't resolve
- Moving code reveals a circular dependency between libraries
- Test failures indicate the move changed behavior (not just import paths)
- The feature description is ambiguous about what exactly should move vs stay
