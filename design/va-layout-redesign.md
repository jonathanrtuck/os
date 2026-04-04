# VA Layout & ASLR Redesign

## Problem Statement

The kernel's virtual address space layout has three independently maintained sets of constants (`system_config.rs`, `paging.rs`, `aslr.rs`) with no formal relationship. ASLR was bolted onto the existing fixed layout, and the interaction is broken: randomizing a region's base within a fixed-ceiling region **shrinks the usable space** instead of sliding it. A process may get 4 MiB or 240 MiB of heap depending on the random roll.

Additionally, all VA regions use bump-only allocation with no reclamation. Freed VA is lost forever, which causes long-running processes to exhaust their VA space through alloc/free churn even when logical usage is small.

These two bugs compound: ASLR shrinks the VA budget, and churn wastes what remains.

## Root Causes

1. **No per-process region endpoints.** `map_heap` checks `paging::HEAP_END` (a global constant). When ASLR moves the base up, the ceiling stays fixed, reducing usable space. Same pattern exists in `map_device_mmio` (checks `DEVICE_MMIO_END`), `map_channel_page` (checks `CHANNEL_SHM_END`), and `map_vmo` (checks `SHARED_MEMORY_END`).

2. **Region sizes not derived from requirements.** The regions were hand-sized for the deterministic layout. ASLR needs `usable + slide` but the regions only have room for one or the other, not both.

3. **Bump-only VA allocation.** `next_heap_va` only advances. `unmap_heap` frees physical pages and removes the VMA, but the VA gap is never reusable. The userspace allocator's 16-entry exact-match cache delays but doesn't prevent exhaustion.

## Design: What the Ideal Kernel Looks Like

### Principle: Region Spec is the Source of Truth

Each VA region is defined by two parameters:

- **`usable`**: the fixed amount of VA every process gets for this region, regardless of ASLR.
- **`entropy_bits`**: how many bits of ASLR randomization the region provides.

Everything else is derived:

```text
slide_window = (1 << entropy_bits) * PAGE_SIZE
outer_region_size = usable + slide_window
region_start = previous_region_end   (or fixed, for constrained regions)
region_end = region_start + outer_region_size
```

The per-process layout is:

```text
process_base = region_start + random(0..slide_window, page_aligned)
process_end  = process_base + usable
```

`process_end` is what `map_heap` checks — not a global constant.

### Region Specifications

| Region       | Usable  | Entropy | Slide Window | Outer Size | Notes                                           |
| ------------ | ------- | ------- | ------------ | ---------- | ----------------------------------------------- |
| Code         | 12 MiB  | 10 bits | 16 MiB       | 28 MiB     | PIE. ELF loader maps at randomized base.        |
| Heap         | 240 MiB | 14 bits | 256 MiB      | 496 MiB    | Matches pre-ASLR deterministic size.            |
| DMA          | 256 MiB | 14 bits | 256 MiB      | 512 MiB    | Large enough for GPU DMA buffers.               |
| Channel SHM  | 768 MiB | 14 bits | 256 MiB      | 1024 MiB   | Bootstrap page passes layout to userspace.      |
| Device MMIO  | 512 MiB | 14 bits | 256 MiB      | 768 MiB    | Virtio MMIO register regions.                   |
| Stack        | 64 KiB  | 14 bits | 256 MiB      | 256 MiB    | Almost entirely slide; actual stack is 4 pages. |
| Shared (VMO) | 1 GiB   | 14 bits | 256 MiB      | 1280 MiB   | Bootstrap page passes layout to userspace.      |

### Concrete Layout

The layout is sized from requirements upward. Regions total ~4.3 GiB. Guard gaps between regions prevent overflow from reaching adjacent regions — 16 MiB each is more than sufficient (a buffer overrun that crosses 16 MiB of unmapped VA would fault millions of times first). Total with guards: ~4.4 GiB. T0SZ is set to the smallest power-of-two that contains the layout: **8 GiB (T0SZ=33)**. A smaller VA space means shallower page table walks and a smaller TLB footprint.

```text
0x0000_0000_0000_0000  ─── (unmapped below code) ──────────────
0x0000_0000_0040_0000  ─── Code region: 28 MiB ────────────────
                        [code_base ... code_base + 12 MiB)   per-process (PIE)
0x0000_0000_0200_0000  ─── (guard: 14 MiB) ────────────────────
0x0000_0000_0300_0000  ─── Heap region: 496 MiB ───────────────
                        [heap_base ... heap_base + 240 MiB)  per-process
0x0000_0000_2200_0000  ─── (guard: 16 MiB) ────────────────────
0x0000_0000_2300_0000  ─── DMA region: 512 MiB ────────────────
                        [dma_base ... dma_base + 256 MiB)    per-process
0x0000_0000_4300_0000  ─── (guard: 16 MiB) ────────────────────
0x0000_0000_4400_0000  ─── Channel SHM region: 1024 MiB ──────
                        [shm_base ... shm_base + 768 MiB)    per-process
0x0000_0000_8400_0000  ─── (guard: 16 MiB) ────────────────────
0x0000_0000_8500_0000  ─── Device MMIO region: 768 MiB ────────
                        [dev_base ... dev_base + 512 MiB)    per-process
0x0000_0000_B500_0000  ─── (guard: 16 MiB) ────────────────────
0x0000_0000_B600_0000  ─── Stack region: 256 MiB ──────────────
                        [stack_bottom ... stack_top)          per-process
0x0000_0000_C600_0000  ─── (guard: 16 MiB) ────────────────────
0x0000_0000_C700_0000  ─── Shared memory region: 1280 MiB ─────
                        [shared_base ... shared_base + 1 GiB) per-process
0x0000_0001_1700_0000  ─── (unmapped to USER_VA_END) ──────────
0x0000_0002_0000_0000  end of user VA (8 GiB, T0SZ=33)
```

Total: ~4.4 GiB of regions + guards. Fits in 8 GiB with ~3.6 GiB of trailing unmapped space.

All currently-fixed addresses (`CHANNEL_SHM_BASE`, `USER_STACK_TOP`, `SHARED_MEMORY_BASE`, `SERVICE_PACK_BASE`) move. This is intentional — the bootstrap page (see below) replaces hardcoded VA constants with a kernel-provided per-process layout. No userspace code should reference a VA directly; it reads the layout at startup.

`SERVICE_PACK_BASE` moves into the code region. It's read-only ELF data, same nature as code. Init's service pack is mapped at a fixed offset within the code region, after the init ELF itself.

### `AslrLayout` Gains Per-Process Endpoints

```rust
pub struct AslrLayout {
    pub code_base: u64,
    pub code_end: u64,
    pub heap_base: u64,
    pub heap_end: u64,
    pub dma_base: u64,
    pub dma_end: u64,
    pub channel_shm_base: u64,
    pub channel_shm_end: u64,
    pub device_base: u64,
    pub device_end: u64,
    pub stack_top: u64,         // grows downward
    pub shared_base: u64,
    pub shared_end: u64,
}
```

Every field is per-process. Every region is randomized. The `_end` fields are derived (`base + usable_size`) and stored for fast ceiling checks.

### `AddressSpace` Uses Per-Process Endpoints

Every bump allocator gets a per-process ceiling from `AslrLayout`:

```rust
pub struct AddressSpace {
    // ... existing fields ...
    heap_va_end: u64,
    dma_va_end: u64,
    device_va_end: u64,
    channel_shm_va_end: u64,
    shared_va_end: u64,
}
```

Every `map_*` function checks the per-process ceiling, not a global constant. Global constants in `paging.rs` become **outer bounds** for the region envelope — used only for page table coverage and fast-reject validation in syscalls.

### Heap VA Reclamation

The bump allocator's `next_heap_va` is replaced with a proper VA allocator:

1. **Free list of reclaimed ranges.** When `unmap_heap` frees an allocation, its VA range is added to a sorted free list. Adjacent ranges are coalesced.

2. **`map_heap` checks free list first.** Best-fit search for a range >= requested size. Split the remainder back into the free list. Fall back to bumping `next_heap_va` only if no free range fits.

3. **Physical pages remain demand-paged.** The VA allocator only manages virtual address ranges. Physical allocation still happens on first touch via the fault handler.

This eliminates the VA churn problem entirely. VA consumption equals logical allocation. The userspace large-alloc cache becomes an optimization (avoiding syscall round-trips) rather than a correctness requirement.

### `DEFAULT_HEAP_PAGE_LIMIT` Aligned with Usable VA

Currently `DEFAULT_HEAP_PAGE_LIMIT = RAM_SIZE_MAX / PAGE_SIZE / 4 = 4096 pages = 64 MiB`. But heap usable VA is 240 MiB. These should be consistent:

- Page limit should not exceed usable VA: `limit <= usable / PAGE_SIZE`
- 240 MiB / 16 KiB = 15,360 pages max
- Keep current 4,096 page limit (64 MiB) — it's within bounds and prevents any single process from consuming all RAM

### Bootstrap Page

The kernel maps a read-only page at a well-known VA (first page of the code region, before the ELF entry point) containing the process's `AslrLayout`. Userspace reads its region bases and endpoints at startup.

This replaces all hardcoded VA constants in userspace:

- `protocol::channel_shm_va(handle)` reads `layout.channel_shm_base + handle * PAGE_SIZE` from the bootstrap page
- `sys` crate initialization reads `layout.heap_base` etc. for allocator setup
- The linker script no longer needs `USER_STACK_TOP` — the kernel sets SP from the layout

The bootstrap page is the only fixed VA in the entire layout. Everything else is per-process and randomized.

### Code Base Randomization (PIE)

The ELF loader maps at a randomized base within the code region (same technique as kernel KASLR):

1. Services compiled with `-fPIE` / `--pie`
2. ELF loader maps segments at `code_base + segment_offset` where `code_base` is randomized
3. Dynamic relocations applied at load time
4. `SERVICE_PACK_BASE` mapped after the init ELF, within the same code region

## Summary of Changes

1. Region specs `(usable, entropy_bits)` are the source of truth in `aslr.rs` — all boundaries derived
2. New VA layout across 64 GiB with guard gaps between every region
3. `AslrLayout` stores per-process base + end for every region
4. `AddressSpace` uses per-process endpoints for all ceiling checks
5. Heap VA reclamation via coalescing free list in the kernel
6. Bootstrap page passes layout to userspace — no hardcoded VA constants
7. All regions ASLR'd (14 bits each), including channel SHM and shared memory
8. PIE support for userspace code base randomization (10 bits)
9. `SERVICE_PACK_BASE` folded into code region
