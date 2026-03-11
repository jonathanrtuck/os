# Environment

**What belongs here:** Required env vars, toolchain details, dependency quirks, platform-specific notes.
**What does NOT belong here:** Service ports/commands (use `.factory/services.yaml`).

---

## Toolchain

- **Rust:** nightly-aarch64-apple-darwin (1.96.0-nightly, 2026-03-05)
- **Kernel target:** `aarch64-unknown-none` (bare-metal ARM64)
- **Test target:** `aarch64-apple-darwin` (host macOS)
- **QEMU:** 10.2.1 at `/opt/homebrew/bin/qemu-system-aarch64`

## Project Structure

- `system/kernel/` — 33 .rs files + 2 .S + link.ld (audit scope)
- `system/test/` — Host-side test suite (348 tests across 18 files)
- `system/libraries/` — Shared libs (ipc, sys, virtio, drawing) — OUT OF SCOPE
- `system/services/` — Userspace services — OUT OF SCOPE

## Test Architecture

Tests in `system/test/` CANNOT import kernel modules directly. The kernel compiles for `aarch64-unknown-none` with bare-metal deps (inline asm, MMIO) that don't run on the host. Tests duplicate/stub the pure algorithmic logic for host-side testing. Follow existing test file patterns.

## Unsafe Code Profile

- 100 `unsafe {}` blocks + 5 `unsafe fn` across 17 of 33 kernel files
- Top 5: memory.rs (20), main.rs (19), syscall.rs (13), address_space.rs (10), memory_mapped_io.rs/page_allocator.rs (6 each)
- Zero `#[allow(...)]` attributes in the kernel

## Recent Bug History (2026-03-11)

11 bugs fixed in crash debugging session. Key fixes:
- Fix 5: Aliasing UB in syscall dispatch (noalias mutable references)
- Fix 6: `nomem` on DAIF asm (primary fix for SMP race)
- Fix 4: Deferred thread drop (use-after-free)
- Fix 9: Systematic `nomem` removal across all inline asm
