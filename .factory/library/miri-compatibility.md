# Miri Compatibility

**What belongs here:** Miri test compatibility status, known limitations, performance characteristics.

---

## Compatibility Status (updated 2026-03-14, post M1 bug fixes + M2 re-analysis)

- **35 test binaries** in `system/test/tests/`
- **685/965 tests pass** clean under Miri across 31 verified test binaries
- **26 tests ignored** via `#[cfg_attr(miri, ignore)]` (documented below)
- **2 test binaries skipped** — exceed 120s Miri timeout (documented below)
- **Zero kernel UB found** — all Miri findings were either expected bare-metal patterns or out-of-scope (library code)
- **Miri toolchain:** nightly-aarch64-apple-darwin (component: miri)

### Per-File Miri Results

| File | Tests | Miri Status | Notes |
| ---- | ----- | ----------- | ----- |
| adversarial_buddy | 1 | 0 pass, 1 ignored | `#[cfg_attr(miri, ignore)]` — mmap provenance |
| adversarial_slab | 5 | 5 pass | Clean (90s) |
| adversarial_stress | 39 | **SKIPPED** | Exceeds 120s timeout |
| adversarial_vma | 6 | **SKIPPED** | Exceeds 120s timeout |
| asid | 7 | 7 pass | Clean |
| buddy | 2 | 1 pass, 1 ignored | `#[cfg_attr(miri, ignore)]` — phys_to_virt provenance |
| channel | 29 | 29 pass | Clean |
| channel_create_leak | 6 | 6 pass | Clean (new M1 file) |
| channel_create_pte_leak | 5 | 5 pass | Clean (new M1 file) |
| cross_module | 13 | 13 pass | Clean (new file since last run) |
| device_tree | 24 | 24 pass | Clean |
| drawing | 347 | 347 pass | Clean |
| eevdf | 36 | 36 pass | Clean |
| executable | 22 | 22 pass | Clean |
| futex | 18 | 18 pass | Clean |
| handle | 42 | 42 pass | Clean |
| handle_send_rollback | 7 | 7 pass | Clean (new M1 file) |
| heap | 1 | 1 pass | integer-to-pointer cast warnings (expected bare-metal pattern) |
| heap_routing | 2 | 2 pass | integer-to-pointer cast warnings (expected bare-metal pattern) |
| interrupt_timer | 26 | 26 pass | Clean |
| ipc | 24 | 0 pass, 24 ignored | `#[cfg_attr(miri, ignore)]` — unaligned AtomicU32 |
| mmio | 8 | 8 pass | Clean |
| oom | 1 | 0 pass, 1 ignored | `#[cfg_attr(miri, ignore)]` — mmap provenance |
| paging | 15 | 15 pass | Clean |
| process | 11 | 11 pass | Clean |
| scene | 47 | 47 pass | Clean |
| sched_context | 23 | 23 pass | Clean |
| sched_context_create_leak | 4 | 4 pass | Clean (new M1 file) |
| scheduler_state | 28 | 28 pass | Clean (~10s, cfg-gated iteration reduction) |
| slab | 22 | 22 pass | Clean |
| sync | 7 | 7 pass | Clean |
| syscall | 76 | 76 pass | Clean |
| virtqueue | 8 | 8 pass | Clean |
| vma | 21 | 21 pass | Clean |
| waitable | 32 | 32 pass | Clean |

### Ignored Tests (26 total)

| File | Count | Reason | Real UB? |
| ---- | ----- | ------ | -------- |
| adversarial_buddy | 1 | Miri provenance limitation with mmap-based memory simulation. `phys_to_virt` integer-to-pointer casts are expected for bare-metal but incompatible with Miri strict provenance. | No |
| buddy | 1 | Same provenance limitation as adversarial_buddy. | No |
| ipc | 24 | Unaligned AtomicU32 reference at `libraries/ipc/lib.rs:198`. Real UB but in `system/libraries/` (out of kernel scope). | Yes (not kernel) |

### Skipped Binaries (2 total, timeout >120s)

| File | Tests | Reason |
| ---- | ----- | ------ |
| adversarial_stress | 39 | Combinatorial stress tests with deep allocation/deallocation loops. Completed 19/39 tests before 120s timeout. |
| adversarial_vma | 6 | Mass insert/lookup with heavy iteration. Completed 2/6 tests before 120s timeout. |

These files use large iteration counts designed for stress testing. Under Miri's instrumented execution (~10-100x slower than native), they exceed practical time limits. The tests that did complete before timeout showed no UB.

### Performance Characteristics

- **scheduler_state:** ~10 seconds under Miri (cfg-gated iteration reduction from 10K→100 via `#[cfg(miri)]`)
- **sched_context_create_leak:** ~20 seconds under Miri
- **handle_send_rollback:** ~14 seconds under Miri
- **syscall:** ~9 seconds under Miri
- **adversarial_slab:** ~90 seconds under Miri (near timeout)
- **adversarial_stress / adversarial_vma:** exceed 120s (skipped)
- All other files complete in <3 seconds under Miri

### Practical Usage

The blanket `cargo +nightly miri test` command exits on first UB per test binary. Run per-file for complete results across the full suite:

```
cd system/test && cargo +nightly miri test --test <name> -- --test-threads=1
```

---

## Assembly-Dependent Blind Spots

Miri **cannot analyze inline assembly** (`core::arch::asm!`) or global assembly (`core::arch::global_asm!`). The kernel contains 34 inline asm blocks and 2 global asm includes across 11 files. Since all tests run on the host (aarch64-apple-darwin) rather than the kernel target (aarch64-unknown-none), assembly-dependent paths are **stubbed or excluded** in tests. This means Miri provides zero coverage for code paths that depend on these operations.

### Inline Assembly by Category

#### 1. IRQ Masking / DAIF Management (sync.rs, timer.rs)
- `msr daif, {saved}` — restore saved interrupt state
- `mrs {}, daif` — read interrupt mask
- `msr daifset, #2` — disable IRQs
- `msr daifclr, #2` — enable IRQs

**Test coverage:** Tests stub `IrqGuard`/`IrqMutex` with no-op implementations. The actual IRQ enable/disable logic (which prevents data races with interrupt handlers) is **not exercised under Miri**. Concurrency correctness between interrupt handlers and kernel code relies on manual audit (completed in M5 — lock ordering DAG verified cycle-free).

#### 2. TLB Invalidation (address_space.rs, memory.rs, address_space_id.rs)
- `tlbi vale1is, {}` — invalidate TLB entry by VA + ASID
- `tlbi aside1is, {}` — invalidate all TLB entries for an ASID
- `dsb ish` / `isb` — barriers after TLB invalidation

**Test coverage:** Tests use simulated page tables (arrays of `u64`) without real TLB. Break-before-make sequences (clear PTE → TLBI → write new PTE) are **structurally present** in test models but the actual TLB invalidation is a no-op. Correct barrier placement was verified via manual AArch64 audit (all 34 asm blocks audited, no issues found).

#### 3. Address Translation (syscall.rs)
- `at s1e0r, {}` — address translation for user VA
- `mrs {}, par_el1` — read translation result

**Test coverage:** Tests stub `range_readable()` and `validate_user_ptr()` with bounds-check-only implementations. The actual translation instruction verifies page table mappings exist and are accessible from EL0. This is a **Miri blind spot** — pointer validation in production uses hardware AT, while tests use arithmetic range checks.

#### 4. Context Switch (scheduler.rs, main.rs)
- `msr tpidr_el1, {}` — set current thread pointer
- `mrs {}, tpidr_el1` — get current thread pointer
- `mrs {}, esr_el1/far_el1/elr_el1` — read exception syndrome registers
- Full register save/restore in `exception.S`

**Test coverage:** Tests use a `SchedulerState` model that tracks thread state transitions without actual register manipulation. The TPIDR_EL1 invariant (always points to current thread's Context) was verified via manual trace across all write sites (Fix 17 + M5 cross-module audit). Context switch correctness relies on manual audit of `exception.S`.

#### 5. System Register Access (timer.rs, per_core.rs, main.rs)
- `mrs {}, cntpct_el0` — read physical counter (timer)
- `mrs {}, cntfrq_el0` — read counter frequency
- `mrs {}, mpidr_el1` — read core ID
- `msr cntp_ctl_el0, {}` — timer control
- `msr cntp_tval_el0, {}` — timer compare value
- `dsb ish` / `dsb sy` / `isb` — memory/instruction barriers

**Test coverage:** Tests use simulated time (`now_ns` parameters) rather than hardware counters. Timer firing logic is exercised through the model but actual hardware timer programming is **not tested under Miri**. Barrier correctness verified via AArch64 audit.

#### 6. Power Management (power.rs)
- `hvc #0` — hypervisor call for PSCI (power state coordination)
- `wfe` — wait for event (idle loop)

**Test coverage:** Not tested. Power management is terminal (shutdown/reboot) or idle (wfe loop). No correctness implications for kernel data structures.

#### 7. GIC / Interrupt Controller (interrupt_controller.rs)
- `dsb sy` / `isb` — barriers for MMIO register access ordering

**Test coverage:** Interrupt controller is MMIO-based with barriers. Tests don't exercise GIC hardware. The barrier pattern (DSB before/after MMIO writes) was audited manually.

### Summary of Blind Spots

| Category | Files | Asm Blocks | Test Model | Risk |
| -------- | ----- | ---------- | ---------- | ---- |
| IRQ masking | sync.rs, timer.rs | 4 | No-op stubs | Low — verified via lock ordering audit |
| TLB invalidation | address_space.rs, memory.rs, address_space_id.rs | 6 | Simulated page tables | Low — break-before-make audited |
| Address translation | syscall.rs | 2 | Bounds-check stubs | Medium — production validates hardware PTE; tests use arithmetic |
| Context switch | scheduler.rs, main.rs, exception.S | 8 + global asm | State machine model | Low — TPIDR invariant manually traced |
| System registers | timer.rs, per_core.rs, main.rs | 10 | Simulated values | Low — timing logic tested, hardware access audited |
| Power management | power.rs | 2 | Not tested | None — terminal operations |
| GIC barriers | interrupt_controller.rs | 6 | Not tested | Low — standard MMIO barrier pattern |

**Overall assessment:** Miri provides strong UB coverage for pure logic (allocators, handle tables, schedulers, channel state machines, syscall validation) which is where most kernel bugs live. The assembly-dependent blind spots are mitigated by manual AArch64 audit (all 34 blocks verified) and model-based testing that exercises the surrounding logic. The highest-risk blind spot is address translation stubbing — production uses hardware AT instructions while tests use arithmetic bounds checks.
