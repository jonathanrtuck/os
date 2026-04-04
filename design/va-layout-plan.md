# VA Layout Redesign — Implementation Plan

Reference: `design/va-layout-redesign.md`

## Already Done (session 2026-04-03, uncommitted)

- `STATE_BUSY` on loading scene root node, `CMD_SCENE_READY` protocol command, hypervisor `sceneReady` handler — frame origin for event scripts
- Visual test failure screenshot archiving (`host/visual-test.sh`)
- Boot wait removal from event scripts (frame 0 = scene ready)
- Failing test `heap_va_size_is_constant_across_seeds` in `kernel/host/tests/kernel_aslr.rs`
- OOM diagnostic in `sys_memory_alloc` (TEMPORARY — remove before tagging)
- `heap_pages_used()`, `heap_pages_max()`, `heap_va_remaining()` accessors on AddressSpace (TEMPORARY)
- Hypervisor rebuilt with `sceneReady` support, installed to `~/.local/bin/`

### Files modified (OS repo, uncommitted)

- `libraries/protocol/metal.rs` — `CMD_SCENE_READY`, `scene_ready()` method
- `libraries/scene/node.rs` — (no change, `STATE_BUSY` already existed)
- `services/presenter/scene/loading.rs` — set `STATE_BUSY` on root node
- `services/drivers/metal-render/main.rs` — detect STATE_BUSY transition, emit `CMD_SCENE_READY`
- `kernel/syscall.rs` — OOM diagnostic (temporary)
- `kernel/address_space.rs` — heap accessor methods (temporary)
- `kernel/host/tests/kernel_aslr.rs` — failing test for heap VA size
- `host/visual-test.sh` — failure archiving, boot wait removal
- `.gitignore` — `host/visual/failures/`
- `design/va-layout-redesign.md` — design document
- `design/va-layout-plan.md` — this file

### Files modified (hypervisor repo, uncommitted)

- `Sources/MetalProtocol.swift` — `sceneReady` case
- `Sources/VirtioMetal.swift` — `startDisplayTimer` moved from `presentAndCommit` to `sceneReady`

## Phase 1: Region Spec + Per-Process Endpoints (kernel only)

The ASLR fix. No userspace changes.

1. Define region specs in `aslr.rs`: `(usable_size, entropy_bits)` for heap, DMA, device, stack
2. Compute outer region bounds from specs
3. Add `heap_end`, `dma_end`, `device_end` to `AslrLayout`
4. Add `heap_va_end`, `device_va_end` to `AddressSpace`, init from layout
5. `map_heap` checks `self.heap_va_end` instead of `paging::HEAP_END`
6. `map_device_mmio` checks `self.device_va_end` instead of `paging::DEVICE_MMIO_END`
7. `sys_memory_free` uses outer bound (fast reject), `unmap_heap` uses per-process
8. Update `paging.rs` constants for new outer bounds
9. `heap_va_size_is_constant_across_seeds` passes. All existing ASLR tests updated.
10. All kernel tests pass. All visual tests pass.

**Files:** `kernel/aslr.rs`, `kernel/address_space.rs`, `kernel/paging.rs`, `kernel/syscall.rs`, `kernel/system_config.rs`, `kernel/host/tests/kernel_aslr.rs`

## Phase 2: Heap VA Reclamation

Independent of layout changes. Can parallel with Phase 1.

1. Add sorted free list of `(va, page_count)` to `AddressSpace`
2. `unmap_heap` adds freed VA to free list, coalesces adjacent
3. `map_heap` searches free list first (best-fit), bumps only if no fit
4. Tests: alloc/free/realloc cycles that exhaust bump-only VA, verify they succeed

**Files:** `kernel/address_space.rs`, new or existing heap test file

## Phase 3: Bootstrap Page

Kernel-userspace contract. Enables Phases 4 and 5.

1. Define bootstrap page format at well-known VA (e.g., page zero, read-only)
2. Kernel maps bootstrap page during `process_create`
3. `sys` crate reads bootstrap page at init to discover region bases
4. `protocol::channel_shm_va()` reads from bootstrap page
5. Remove hardcoded `CHANNEL_SHM_BASE`, `SHARED_MEMORY_BASE`, `USER_STACK_TOP` from `system_config.rs`
6. Linker script no longer hardcodes stack top — kernel sets SP from layout

**Files:** `kernel/process.rs`, `kernel/system_config.rs`, `libraries/sys/`, `libraries/protocol/lib.rs`, `libraries/link.ld`, service `main.rs` files

## Phase 4: Full ASLR (Channel SHM + Shared Memory)

Requires Phase 3.

1. Add `channel_shm_base/end`, `shared_base/end` to `AslrLayout` with 14-bit entropy
2. `map_channel_page` and `map_vmo` use per-process endpoints
3. Update region specs. Recompute outer bounds.
4. Verify IPC works end-to-end

**Files:** `kernel/aslr.rs`, `kernel/address_space.rs`, `kernel/host/tests/kernel_aslr.rs`

## Phase 5: PIE for Userspace

Requires Phase 3.

1. Add `code_base/code_end` to `AslrLayout` with 10-bit entropy
2. ELF loader maps at `code_base + segment_vaddr`
3. Apply relocations at load time (ref: kernel's `relocate.rs`)
4. Services compiled with position-independent flags
5. `SERVICE_PACK_BASE` becomes offset within code region
6. Update `build.rs` and linker script

**Files:** `kernel/process.rs`, `kernel/aslr.rs`, `build.rs`, `libraries/link.ld`

## Phase 6: T0SZ + Guard Pages

After Phase 1 (can follow any phase).

1. Change T0SZ from 28 (64 GiB) to 33 (8 GiB) in `boot.S` and `paging.rs`
2. Verify page table walk depth
3. Guard gaps are naturally unmapped — verify clean faults
4. Update `USER_VA_END`

**Files:** `kernel/boot.S`, `kernel/paging.rs`, `kernel/system_config.rs`

## Dependency Graph

```text
Phase 1 ──→ Phase 2 (independent, can parallel)
Phase 1 ──→ Phase 3 ──→ Phase 4
                    └──→ Phase 5
Phase 1 ──→ Phase 6 (independent)
```

## Risks

- **HIGH:** Phase 3 changes kernel-userspace contract. Every service updated. Keep old constants as fallbacks during dev.
- **HIGH:** Phase 5 requires build system changes for PIE. Use kernel's `relocate.rs` as reference.
- **MEDIUM:** Phase 2 adds allocator complexity. Mitigate with property-based tests.
- **LOW:** Phase 6 T0SZ change — standard AArch64 config, smoke test catches issues.

## Before Starting

1. Commit the scene-ready and visual test work from this session (two separate commits: one for scene ready, one for failure archiving)
2. Remove the temporary OOM diagnostic and accessor methods from kernel
3. Keep the failing ASLR test — it's the Phase 1 entry point
