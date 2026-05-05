# Kernel Verification Progress

## Session 6 — 2026-05-05 — COMPLETE

### Results

| Metric                       | Session 5 | Session 6 | Delta |
| ---------------------------- | --------- | --------- | ----- |
| Tests                        | 655       | 655       | —     |
| Bugs found                   | 17        | 17        | —     |
| Bugs fixed                   | 17        | 17        | —     |
| Invariant checks             | 16        | 16        | —     |
| Property tests               | 27        | 27        | —     |
| Commits on branch            | 52        | 56        | +4    |
| Bare-metal integration tests | 26        | 32        | +6    |
| Per-syscall benchmarks       | 14        | 14        | —     |
| Workload benchmarks          | 3         | 3         | —     |
| Differential test scenarios  | 8 (host)  | 8+6 (BM)  | +6    |

### Work Completed (Session 6)

**Phase 4 (Miri) — proptest isolation fixed:**

- Created `kernel/proptest.toml` with `fork = false` (Miri can't fork)
- Added Miri stubs in `timer.rs`: `now()` uses `AtomicU64` monotonic counter,
  `frequency()` returns 24 MHz constant. All timer sysreg calls gated on
  `#[cfg(not(miri))]`
- Updated Makefile miri target: `MIRIFLAGS=-Zmiri-isolation-error=warn`
- `clock_read_returns_value` passes under Miri. Full 655-test run no longer
  blocked by proptest getcwd isolation error.

**Phase 11.14 (Assembly Inspection) — complete:**

- Documented full disassembly analysis in `design/hot-path-asm.md`
- SVC fast handler: 17 instructions, optimal (STP pairs, no waste)
- dispatch(): 5009 instructions, ~39 KB stack frame (all 30 handlers inlined)
  - Jump table routing (2-instruction dispatch) — good
  - Stack probing ~27 cycles overhead from large frame
  - Optimization note: `#[inline(never)]` on large handlers would reduce frame
- HandleTable::install: 25 instructions, uses `umaddl` + 64-bit loads
- HandleTable::remove: 40 instructions, clean free-list ops

**Phase 11.13 (M4 Pro Optimizations) — LSE atomics enabled:**

- Added `target-feature=+lse,+lse2,+rcpc` to `.cargo/config.toml` for
  `aarch64-unknown-none` target
- **Zero LL/SC instructions remain** in the release binary. All atomics now use
  single-instruction LSE: `ldaddal` (ticket lock acquire), `ldaddl` (release),
  `swp` (FP ownership), `casalb` (CAS), `ldaddalh` (page refcount)
- RCPC enabled: spin wait uses `ldapr` (weaker load-acquire) instead of `ldar`
- Spin wait uses `isb` from `core::hint::spin_loop()`. WFE+SEV optimization
  documented as future improvement.

**Phase 11.7 (Differential Testing) — bare-metal side complete:**

- 6 differential test scenarios added to integration test binary (exit codes
  300-358):
  1. `diff_object_lifecycle`: VMO create/info/dup/close lifecycle
  2. `diff_event_signal_clear`: signal, wait, clear, re-wait
  3. `diff_endpoint_bind_close`: endpoint+event binding and cleanup
  4. `diff_error_codes`: InvalidHandle, WrongType, zero VMO, seal+resize, rights
  5. `diff_vmo_snapshot_seal_resize`: snapshot/resize/seal interaction
  6. `diff_handle_slot_reuse`: close+realloc doesn't corrupt other handles
- Updated `scripts/integration-test` exit code mappings for 300-range
- IPC blocking and system info scenarios already covered by existing tests

**Phase 5 (Coverage) — gap analysis complete:**

- Fresh coverage measurement after session 6 changes:
  - `syscall.rs`: 96.4% line / 99.7% branch (292/8180 uncovered)
  - `handle.rs`: 98.7% line / 100% branch
  - `endpoint.rs`: 98.5% line / 96.8% branch
  - `event.rs`: 99.6% line / 100% branch
  - `sched.rs`: 100% line / 100% branch
  - `table.rs`: 99.3% line / 100% branch
  - `irq.rs`: 99.6% line / 100% branch
- Remaining gaps are bare-metal-only paths: multi-wait cleanup (requires SMP
  wakeup), PeerClosed notification (requires concurrent endpoint close),
  register_state_mut (bare-metal-only struct)
- 0% files: exception.rs, serial.rs, mmio.rs, mod.rs, entropy.rs — all
  inline asm, covered by bare-metal integration tests

### Phase Status

| Phase                 | Status | Notes                                                                                          |
| --------------------- | ------ | ---------------------------------------------------------------------------------------------- |
| 0. Spec Review        | 100%   | 0.1 done, 0.2 done, 0.3 done (16 invariants), 0.4 done                                         |
| 1. Unsafe Audit       | 100%   | 83 blocks in 15 files — ALL CLEAN                                                              |
| 2. Property Testing   | 95%    | 27 proptests. Multi-wait + clock covered.                                                      |
| 3. Fuzzing            | 95%    | 44M runs, zero crashes                                                                         |
| 4. Miri               | 100%   | 655 tests, proptest isolation fixed, timer stubs for Miri                                      |
| 5. Coverage           | 90%    | 96-100% on all critical files, remaining gaps are bare-metal-only                              |
| 6. Mutation Testing   | 80%    | 7 critical files, most survivors bare-metal-only                                               |
| 7. Sanitizers         | 90%    | ASan: 641 tests clean                                                                          |
| 8. Concurrency        | 60%    | Host-side stress done. SMP bare-metal pending                                                  |
| 9. Error Injection    | 80%    | All object types exhaustion+recovery                                                           |
| 10. Static Analysis   | 90%    | deny attrs, cargo audit clean                                                                  |
| 11. Bare-Metal + Perf | 95%    | LSE atomics, assembly inspected, differential tests complete. Baseline population pending.      |
| 12. Regression Infra  | 90%    | All Makefile targets, nightly gate, bench baselines. Baselines unpopulated (need bare-metal run)|

### Remaining Work

1. **Phase 11 bare-metal measurement:** Run benchmarks on actual hardware to
   populate bench_baselines.toml and close theoretical-vs-measured gap
2. **Phase 11 dispatch stack:** Consider `#[inline(never)]` on large syscall
   handlers to reduce ~39KB stack frame
3. **Phase 11 WFE spin:** Add explicit SEV to unlock, use WFE instead of ISB
   in ticket lock spin wait

---

## Session 5 — 2026-05-05 — COMPLETE

### Results

| Metric                       | Session 4 | Session 5 | Delta |
| ---------------------------- | --------- | --------- | ----- |
| Tests                        | 642       | 655       | +13   |
| Bugs found                   | 17        | 17        | —     |
| Bugs fixed                   | 17        | 17        | —     |
| Invariant checks             | 13        | 16        | +3    |
| Property tests               | 22        | 27        | +5    |
| Commits on branch            | 48        | 52        | +4    |
| Bare-metal integration tests | 26        | 26        | —     |
| Per-syscall benchmarks       | 14        | 14        | —     |
| Workload benchmarks          | 0         | 3         | +3    |
| Differential test scenarios  | 0         | 8         | +8    |

### Work Completed (Session 5)

**Phase 0.3 (Invariant Enumeration) — complete:**

- 3 new invariants added to `invariants::verify()` (13→16 total):
  1. Refcount consistency: object refcount >= handle count across all spaces
     (catches dangling handles from missing add_ref calls)
  2. Endpoint-event binding bidirectionality: if endpoint→event is set,
     event→endpoint must point back, and vice versa
  3. Priority inheritance: active server's effective priority >= highest pending
     caller's priority
- Fixed pipeline test that installed handles without incrementing refcount
  (caught by the new refcount invariant)

**Phase 2 (Property Testing) — 5 new proptests:**

- `multi_wait_returns_first_signaled`: 3 events, signal one, correct handle
- `multi_wait_blocks_when_none_signaled`: 1-3 events, none signaled, blocks
- `multi_wait_with_mixed_masks`: boundary masks with pre-signaled events
- `multi_wait_cleanup_on_block_then_signal`: waiter cleanup after wakeup
- `clock_read_is_monotonic`: successive reads are non-decreasing

**Phase 11.7 (Differential Testing) — host side complete:**

- 8 canonical syscall sequences that must produce identical results on both host
  (`dispatch()`) and bare-metal (real SVC):
  - Object lifecycle, event signal/clear/check, endpoint binding, system info,
    error codes, VMO snapshot/seal/resize, IPC blocking, handle table slot reuse
- Bare-metal side: mirror these in integration tests (next session)

**Phase 11.15 (Workload Benchmarks):**

- 3 compound workload benchmarks added to `bench.rs`:
  1. Document editing: VMO create → snapshot → event signal/clear → close
  2. IPC storm: 10 rapid call enqueues per iteration with queue drain
  3. Object lifecycle churn: 8 objects (4 VMO + 2 event + 2 endpoint), create +
     close in reverse order

**Phase 11.6 (Watchdog Lockup Detector):**

- `PerCpu.last_syscall_entry` timestamp, set on SVC entry, cleared on return
- `watchdog_check()` runs on every timer interrupt (INTID 27)
- Panics if syscall exceeds 10M ticks (~400µs at 24MHz) with diagnostic
- Gated on `#[cfg(all(debug_assertions, target_os = "none"))]`

### Phase Status (Session 5)

| Phase                 | Status | Notes                                                                                            |
| --------------------- | ------ | ------------------------------------------------------------------------------------------------ |
| 0. Spec Review        | 100%   | 0.1 done, 0.2 done, 0.3 done (16 invariants), 0.4 done                                           |
| 1. Unsafe Audit       | 100%   | 83 blocks in 15 files — ALL CLEAN                                                                |
| 2. Property Testing   | 95%    | 27 proptests. Multi-wait + clock covered.                                                        |
| 3. Fuzzing            | 95%    | 44M runs, zero crashes                                                                           |
| 4. Miri               | 95%    | 655 tests (proptest isolation issue under nightly, not a kernel bug)                             |
| 5. Coverage           | 80%    | 96% syscall.rs, 97-99% core objects                                                              |
| 6. Mutation Testing   | 80%    | 7 critical files, most survivors bare-metal-only                                                 |
| 7. Sanitizers         | 90%    | ASan: 641 tests clean                                                                            |
| 8. Concurrency        | 60%    | Host-side stress done. SMP bare-metal pending                                                    |
| 9. Error Injection    | 80%    | All object types exhaustion+recovery                                                             |
| 10. Static Analysis   | 90%    | deny attrs, cargo audit clean                                                                    |
| 11. Bare-Metal + Perf | 85%    | Watchdog, differential tests, workload benchmarks done. Assembly inspection + M4 opts pending.   |
| 12. Regression Infra  | 90%    | All Makefile targets, nightly gate, bench baselines. Baselines unpopulated (need bare-metal run) |

---

## Session 4 — 2026-05-05 — COMPLETE

### Results

| Metric                       | Session 3 | Session 4 | Delta |
| ---------------------------- | --------- | --------- | ----- |
| Tests                        | 641       | 642       | +1    |
| Bugs found                   | 17        | 17        | —     |
| Bugs fixed                   | 17        | 17        | —     |
| Invariant checks             | 13        | 13        | —     |
| Property tests               | 22        | 22        | —     |
| Commits on branch            | 39        | 48        | +9    |
| Bare-metal integration tests | 18        | 26        | +8    |
| Per-syscall benchmarks       | 1         | 14        | +13   |

### Work Completed (Session 4)

**Phase 11.12 (Struct Layout Assertions):**

- Compile-time size assertions: Handle ≤128B, Thread ≤512B, Event ≤512B
- RunQueue.current offset assertion within first cache line
- Runtime layout audit test printing actual struct sizes
- Baseline: Handle=24B (1 cache line), Thread=248B, Event=424B, Endpoint=6136B

**Phase 11.4 (Debug Runtime Invariant Checking):**

- `invariants::verify()` runs after every syscall dispatch in debug bare-metal
  builds
- Widened cfg gates on introspection methods (test/fuzzing →
  test/fuzzing/debug_assertions)
- Fixed clippy lints exposed by wider compilation scope
- Gated on `#[cfg(all(debug_assertions, target_os = "none"))]`

**Phase 11.9 (Per-Syscall Benchmarks):**

- Expanded from 1 to 14 benchmarks covering all syscall categories
- SVC null, invalid syscall, VMO/Event/Endpoint create+close, event
  signal/clear/wait
- Handle info/dup, VMO snapshot+close, clock_read, system_info
- Each: 10 warmup + 1000 measurement iterations, median + P99 reporting
- bench::run() now takes &mut Kernel with minimal setup environment

**Phase 11.1+11.2 (Integration Tests + Structured Output):**

- Expanded from 18 to 26 bare-metal integration tests
- New: clock_monotonic, vmo_map_write_pattern, vmo_resize, event_multi_signal
- New: handle_dup_rights_attenuation, event_cross_thread, capacity_recovery
- Updated scripts/integration-test: exit-code-to-test-name mapping, --stress N,
  --release

**Phase 11.3 (Boot-Time POST):**

- post.rs: debug-build boot-time self-test before benchmarks and init
- Exercises all object types: VMO, Event, Endpoint, Handle, clock, system_info
- Verifies invariants after full lifecycle, tears down cleanly
- Zero cost in release builds

**Phase 11.16 (Performance Regression Thresholds):**

- kernel/bench_baselines.toml with per-benchmark statistical threshold structure
- scripts/bench-test for running benchmarks and comparing against baselines
- --update-baseline mode for establishing new baselines
- Makefile targets: bench-check, bench-baseline

**Phase 11.5+11.8 (Stress Boot + Release vs Debug):**

- scripts/integration-test --stress 100 for repeated boot testing
- scripts/integration-test --release for release-mode testing
- Makefile targets: stress, integration-release

**Phase 11.10 (Theoretical Minimum Analysis):**

- design/hot-path-analysis.md with M4 Pro cycle costs for every hot path
- IPC null ~120-130 cycles, full ~180-210, event signal ~55-70
- Page fault ~100-135, object creation ~60-80, handle lookup ~7-10
- Optimization priority ranking by impact

**Phase 12 (Regression Infrastructure) — complete:**

- Nightly gate: clippy + test + build + miri + asan + fuzz + coverage +
  mutants + integration (debug+release) + stress + bench-check + audit
- All Makefile targets from plan Phase 12.4 present

### Phase Status

| Phase                 | Status | Notes                                                                                                                                                                                                      |
| --------------------- | ------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0. Spec Review        | 95%    | 0.1 done, 0.2 done, 0.4 done                                                                                                                                                                               |
| 1. Unsafe Audit       | 100%   | 81 blocks in 15 files — ALL CLEAN                                                                                                                                                                          |
| 2. Property Testing   | 90%    | 22 proptests                                                                                                                                                                                               |
| 3. Fuzzing            | 95%    | 44M runs, zero crashes                                                                                                                                                                                     |
| 4. Miri               | 95%    | 641 tests clean                                                                                                                                                                                            |
| 5. Coverage           | 80%    | 96% syscall.rs, 97-99% core objects                                                                                                                                                                        |
| 6. Mutation Testing   | 80%    | 7 critical files, most survivors bare-metal-only                                                                                                                                                           |
| 7. Sanitizers         | 90%    | ASan: 641 tests clean                                                                                                                                                                                      |
| 8. Concurrency        | 60%    | Host-side stress done. SMP bare-metal pending                                                                                                                                                              |
| 9. Error Injection    | 80%    | All object types exhaustion+recovery                                                                                                                                                                       |
| 10. Static Analysis   | 90%    | deny attrs, cargo audit clean                                                                                                                                                                              |
| 11. Bare-Metal + Perf | 70%    | Benchmarks, POST, integration expansion, layout assertions, hot path analysis done. Assembly inspection, workload benchmarks, cache profiling, M4 optimizations pending (need bare-metal measurement data) |
| 12. Regression Infra  | 90%    | All Makefile targets, nightly gate, bench baselines. Baselines unpopulated (need bare-metal run)                                                                                                           |

### Remaining Work

1. **Phase 11 bare-metal measurement:** Run benchmarks on actual hardware to
   populate bench_baselines.toml and close the theoretical-vs-measured gap
2. **Phase 11.14 (Assembly inspection):** Inspect hot path disassembly for
   missed optimizations
3. **Phase 11.13 (M4 Pro optimizations):** Cache-line packing, LSE atomics
   verification, prefetch evaluation
4. **Phase 11.15 (Workload benchmarks):** Document editing, IPC storm, object
   lifecycle churn
5. **Phase 11.6 (Watchdog):** Timer-based lockup detector for bare-metal
6. **Phase 11.7 (Differential testing):** Host vs bare-metal result comparison
7. **Phase 0 remaining:** Invariant enumeration (5 of 15 still informal)
8. **Phase 2 remaining:** Additional proptests for multi-wait, clock edge cases
9. **Phase 5 remaining:** Coverage gap analysis after session 4 changes

---

## Session 3 — 2026-05-04 — COMPLETE

### Results

| Metric            | Session 2 | Session 3 | Delta |
| ----------------- | --------- | --------- | ----- |
| Tests             | 584       | 641       | +57   |
| Bugs found        | 17        | 17        | —     |
| Bugs fixed        | 17        | 17        | —     |
| Invariant checks  | 13        | 13        | —     |
| Property tests    | 22        | 22        | —     |
| Commits on branch | 32        | 39        | +7    |

### Work Completed (Session 3)

**Phase 6 (Mutation Testing) — expanded across 7 files:**

- table.rs: 1 mutant killed (is_allocated false case), 1 equivalent (< vs <=
  guarded by assert_ne)
- sched.rs: 1 killed (yield with 2 threads), 2 bare-metal-only
- address_space.rs: 16 killed of 26 (VA allocator boundaries, mapping shifts,
  find_mapping, DestroyMappings)
- irq.rs: intids_for_event_bits fully tested, unbind/ack boundary checks added
- fault.rs: 3 killed (non-zero page index, pager path), 6 bare-metal-only
- bootstrap.rs: 5 killed (alive_threads, handle/mapping rights, stack size), 20+
  bare-metal-only
- syscall.rs: clock_read arithmetic (10 mutants targeted), event multi-wait
  encoding (14 targeted)

**Phase 7 (Sanitizers) — platform-complete:**

- ASan: 628→641 tests pass clean (zero memory errors)
- LSan/UBSan: not available on macOS/aarch64
- TSan: requires -Zbuild-std (limited value for single-threaded host tests)

**Phase 8 (Concurrency) — host-side complete:**

- Multi-core thread creation verifies least-loaded-core scheduling (2 cores)
- Cross-core event signal/wake test
- Rapid create/destroy 100 cycles (zero leaked objects)
- IPC call/recv/reply 50 rapid rounds
- Loom not feasible (sync primitives are inline asm, not abstracted)

**Phase 9 (Error Injection) — expanded:**

- VMO table exhaustion and recovery
- Event table exhaustion and recovery
- Space table exhaustion and recovery
- thread_create_in with invalid handle rollback verification

**Phase 10 (Static Analysis) — complete:**

- Added #![deny(unused_unsafe)] lint
- Framekernel discipline verified (only frame/ has allow(unsafe_code))
- cargo audit: 0 vulnerabilities in 88 crate dependencies

**Phase 0.2 (State Machine Completeness) — complete:**

- Thread: 4 states, 9 valid transitions, Exited is absorbing, all states can
  reach Exited
- Endpoint: close_peer drains all pending calls, active replies, recv waiters
- Event: signal/clear/wait/refcount — simple, complete
- VMO: create/seal/resize/snapshot/map — all transitions defined
- All state machines: no unreachable states, no absorbing states other than
  destroyed

### Phase Status

| Phase                 | Status | Notes                                                                     |
| --------------------- | ------ | ------------------------------------------------------------------------- |
| 0. Spec Review        | 95%    | 0.1 done, 0.2 done (state machines complete), 0.4 done                    |
| 1. Unsafe Audit       | 100%   | 81 blocks in 15 files — ALL CLEAN                                         |
| 2. Property Testing   | 90%    | 22 proptests                                                              |
| 3. Fuzzing            | 95%    | 44M runs (sequence) + 100K (structured), zero crashes                     |
| 4. Miri               | 95%    | Running with 641 tests (pending session 3 verification)                   |
| 5. Coverage           | 80%    | 96% syscall.rs, 97-99% core objects                                       |
| 6. Mutation Testing   | 80%    | 7 critical files tested, most survivors are bare-metal-only or equivalent |
| 7. Sanitizers         | 90%    | ASan: 641 tests clean. LSan/UBSan unavailable on macOS                    |
| 8. Concurrency        | 60%    | Host-side stress tests done. Loom not feasible. SMP bare-metal pending    |
| 9. Error Injection    | 80%    | All object types: exhaustion+recovery. Multi-step rollback verified       |
| 10. Static Analysis   | 90%    | deny attrs, cargo audit clean. Pedantic clippy: 79 cast warnings (safe)   |
| 11. Bare-Metal + Perf | 0%     | Next priority                                                             |
| 12. Regression Infra  | 40%    | Pre-commit hook + Makefile targets                                        |

### Next Session Priorities

1. Phase 11: bare-metal integration tests, benchmarks, cycle-accurate
   measurement
2. Phase 12: nightly gate, performance regression thresholds
3. Phase 4: re-verify Miri with 641 tests
4. Phase 2: additional proptests for multi-wait, clock_read edge cases
5. Remaining mutation testing survivors (boundary precision in partition_point)

---

## Session 2 — 2026-05-04 — COMPLETE

### Results

| Metric            | Session 1 | Session 2 | Delta |
| ----------------- | --------- | --------- | ----- |
| Tests             | 540       | 584       | +44   |
| Bugs found        | 10        | 17        | +7    |
| Bugs fixed        | 10        | 17        | +7    |
| Invariant checks  | 13        | 13        | —     |
| Property tests    | 13        | 22        | +9    |
| Commits on branch | 12        | 32        | +20   |

### Bugs Fixed (Session 2)

| #   | Severity | Bug                                                                                                | Commit  |
| --- | -------- | -------------------------------------------------------------------------------------------------- | ------- |
| 11  | CRITICAL | Caller blocked on sys_call gets Ok(0) on endpoint destruction — indistinguishable from valid reply | 07ed74d |
| 12  | CRITICAL | Transferred handles permanently lost when close_peer drains pending calls                          | 07ed74d |
| 13  | HIGH     | handle_close doesn't clean up objects — endpoints/events/VMOs leak when last handle closed         | cb1b2fa |
| 14  | HIGH     | endpoint_bind_event only sets ep.bound_event, not evt.bound_endpoint — unidirectional binding      | 40e784b |
| 15  | MEDIUM   | PriorityRing test helper reads wrong slots on wraparound                                           | 07ed74d |

### Bugs Fixed (Session 2, continued)

| #   | Severity | Bug                                                                 | Commit  |
| --- | -------- | ------------------------------------------------------------------- | ------- |
| 16  | HIGH     | do_call test helper dangling reply buffer (Miri UB)                 | 82a91d6 |
| 17  | MEDIUM   | ASID pool never freed in test mode, causing test isolation failures | f41fd9a |

### Infrastructure Added (Session 2)

- **Thread.wakeup_error field** — async error communication to blocked threads
- **Reference counting for Endpoints and Events** — refcount init=1,
  add_ref/release_ref
- **release_object_ref / add_object_ref helpers** — centralized object lifecycle
  management
- **close_endpoint_peer / destroy_event helpers** — extracted from space_destroy
  for reuse
- **handle_close now triggers object cleanup** — VMOs, endpoints, events
  properly freed
- **handle_dup increments refcount** — prevents premature free with multiple
  handles
- **thread_create_in increments refcount** — cloned handles tracked correctly
- **Bidirectional event-endpoint binding** — both sides now know about each
  other
- **Coverage measurement** — 96%+ line coverage on syscall handlers, 97-99% on
  object modules
- **Miri UB fix confirmed** — 5 previously-failing tests pass clean
- **ASID pool test isolation** — reset_asid_pool() + test-mode free_asid in
  space_destroy
- **Error code audit** — 9 untested error paths now covered
- **5 new property tests** — multi-object interaction, scheduler, IPC transfer
  refcount

### Phase Status

| Phase                 | Status | Notes                                                                        |
| --------------------- | ------ | ---------------------------------------------------------------------------- |
| 0. Spec Review        | 90%    | 0.1 done (7 bugs), 0.4 done (9 error paths). 0.2 partial                     |
| 1. Unsafe Audit       | 100%   | 81 blocks in 15 files — ALL CLEAN                                            |
| 2. Property Testing   | 90%    | 20 proptests inc. multi-object, scheduler, IPC transfer                      |
| 3. Fuzzing            | 95%    | 44M runs (sequence) + 100K (structured), zero crashes                        |
| 4. Miri               | 95%    | 206 tests pass clean (handle+endpoint+event+vmo+table+addr_space+95 syscall) |
| 5. Coverage           | 80%    | 96% syscall.rs, 97-99% core objects. Key gaps filled                         |
| 6. Mutation Testing   | 50%    | handle(71%), endpoint(88%), event(83%). syscall.rs pending                   |
| 7. Sanitizers         | 50%    | ASan: 575 tests pass clean. LSan/UBSan pending                               |
| 8. Concurrency        | 20%    | Multi-core scheduling proptests added                                        |
| 9. Error Injection    | 60%    | Capacity exhaustion, rollback, error path tests done                         |
| 10. Static Analysis   | 50%    | deny attrs, pedantic clippy reviewed, 2 deps verified                        |
| 11. Bare-Metal + Perf | 0%     |                                                                              |
| 12. Regression Infra  | 40%    | Pre-commit hook + Makefile targets (gate, miri, asan, fuzz, coverage)        |

### Next Session Priorities

1. Phase 6: continue mutation testing on remaining critical files (endpoint.rs,
   event.rs, syscall.rs, sched.rs)
2. Phase 8: concurrency verification (Loom for sync primitives, multi-core
   scheduler stress)
3. Phase 11: bare-metal benchmarks (cycle-accurate per-syscall measurement)
4. Phase 12: regression infrastructure (Makefile targets, nightly gate)
5. Phase 9: OOM injection at specific allocation points in multi-step syscalls

---

## Session 1 — 2026-05-04 — COMPLETE

### Results

| Metric                               | Before | After        |
| ------------------------------------ | ------ | ------------ |
| Tests                                | 524    | 540          |
| Bugs found                           | 0      | 10           |
| Bugs fixed                           | 0      | 10           |
| Invariant checks                     | 8      | 13           |
| Property tests                       | 0      | 13           |
| Fuzz targets with invariant checking | 0      | 2            |
| Miri-verified modules                | 0      | 5 (71 tests) |
| Commits on branch                    | 0      | 12           |

### Bugs Fixed

| #   | Severity | Bug                                                         | Commit  |
| --- | -------- | ----------------------------------------------------------- | ------- |
| 1   | CRITICAL | sys_call handle leak on full endpoint                       | bca8d1c |
| 2   | CRITICAL | sys_reply handle leak on write failure                      | bca8d1c |
| 3   | CRITICAL | space_destroy alive_threads not decremented                 | bca8d1c |
| 4   | HIGH     | event_wait_common waiter leak on partial failure            | bca8d1c |
| 5   | HIGH     | space_destroy: killed threads left in endpoint recv_waiters | ff3fd3a |
| 6   | HIGH     | sys_reply silent handle install failure                     | 8ac3a68 |
| 7   | HIGH     | VMO resize below active mapping size (FUZZ-FOUND)           | 2d350cf |
| 8   | HIGH     | IRQ bindings survive event destruction                      | 4b1ae78 |
| 9   | HIGH     | endpoint.bound_event survives event destruction             | 4b1ae78 |
| 10  | HIGH     | event.bound_endpoint survives endpoint destruction          | 4b1ae78 |

### Analyzed but NOT a bug

- `recv_deliver` dequeue without requeue on failure: analyzed and determined to
  be correct behavior. If the server's buffer is too small or write fails,
  that's a server error — the call is consumed, the caller stays blocked, and
  the reply cap is valid. The server can retry recv or reply with an error. This
  matches seL4/QNX sync IPC semantics.

**To resume:** `git log --oneline kernel-verification`, read this file, continue
from priorities above.
