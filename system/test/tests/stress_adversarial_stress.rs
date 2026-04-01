//! Adversarial stress / fuzz tests targeting specific audit findings from
//! milestones 1-6 of the kernel bug audit.
//!
//! Each section targets a specific bug class discovered and fixed during the
//! audit. Tests exercise those code paths under pressure: rapid cycles,
//! boundary values, resource exhaustion, and random-ish inputs.
//!
//! Run with: cargo test --test adversarial_stress -- --test-threads=1
//!
//! NOTE: Tests that include kernel modules via #[path] are in separate
//! integration test files (adversarial_buddy.rs, etc.) because #[path]
//! does not compose with inline `mod` blocks. This file contains tests
//! that duplicate pure logic (no kernel includes needed).

use std::collections::HashMap;

// ============================================================
// 1. Handle table: exhaustion, recycling, and close pressure
//    (milestone 4: channel-handle audit)
// ============================================================

mod handle_deps {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct InterruptId(pub u8);
}
mod interrupt {
    pub use super::handle_deps::*;
}
mod process {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ProcessId(pub u32);
}
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}
mod vmo {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VmoId(pub u32);
}
#[path = "../../kernel/handle.rs"]
mod handle;
#[path = "../../kernel/scheduling_context.rs"]
mod scheduling_context;

use handle::*;

fn ch(id: u32) -> HandleObject {
    HandleObject::Channel(ChannelId(id))
}

/// Fill table to capacity, close all, refill — 100 cycles.
/// Verifies handle slot reuse under heavy churn.
#[test]
fn handle_exhaust_close_refill_cycles() {
    let mut t = HandleTable::new();

    for cycle in 0..100 {
        let mut handles = Vec::new();
        let cap = handle::MAX_HANDLES as u32;

        for i in 0..cap {
            let h = t.insert(ch(cycle * cap + i), Rights::READ_WRITE).unwrap();

            handles.push(h);
        }

        // Table is full.
        assert!(
            t.insert(ch(9999), Rights::READ).is_err(),
            "cycle {}: table must be full at {}",
            cycle,
            cap
        );

        // Close all.
        for h in handles {
            t.close(h).unwrap();
        }
    }
}

/// Close and immediately reinsert at the same slot, verifying the freed
/// slot is correctly recycled without corruption.
#[test]
fn handle_close_reinsert_same_slot() {
    let mut t = HandleTable::new();

    let mut handles: Vec<Handle> = Vec::new();
    for i in 0..256u32 {
        handles.push(t.insert(ch(i), Rights::READ).unwrap());
    }

    for i in 0..256u32 {
        let old = handles[i as usize];
        t.close(old).unwrap();

        let new = t.insert(ch(1000 + i), Rights::WRITE).unwrap();
        assert_eq!(
            new.0, old.0,
            "reinserted handle should reuse slot {}",
            old.0
        );

        let obj = t.get(new, Rights::WRITE).unwrap();
        assert!(
            matches!(obj, HandleObject::Channel(ChannelId(cid)) if cid == 1000 + i),
            "reinserted handle should contain new object"
        );

        handles[i as usize] = new;
    }

    for h in handles {
        t.close(h).unwrap();
    }
}

/// Double-close: repeated double-close attempts must be harmless.
#[test]
fn handle_repeated_double_close() {
    let mut t = HandleTable::new();
    let h = t.insert(ch(42), Rights::READ).unwrap();
    t.close(h).unwrap();

    for _ in 0..1000 {
        assert!(t.close(h).is_err(), "double-close must return error");
    }

    let h2 = t.insert(ch(99), Rights::WRITE).unwrap();
    assert_eq!(
        h2.0, 0,
        "slot 0 should be reusable after double-close storm"
    );
    t.close(h2).unwrap();
}

/// Use-after-close storm.
#[test]
fn handle_use_after_close_storm() {
    let mut t = HandleTable::new();

    for _ in 0..500 {
        let h = t.insert(ch(7), Rights::READ).unwrap();
        t.close(h).unwrap();
        assert!(t.get(h, Rights::READ).is_err(), "get after close must fail");
    }
}

/// Fill, drain, fill with different object types — verifies no type confusion.
#[test]
fn handle_mixed_object_type_churn() {
    let mut t = HandleTable::new();
    let objects: Vec<HandleObject> = (0..256u32)
        .map(|i| match i % 4 {
            0 => HandleObject::Channel(ChannelId(i)),
            1 => HandleObject::Timer(timer::TimerId((i % 256) as u8)),
            2 => HandleObject::Process(process::ProcessId(i)),
            _ => HandleObject::Thread(thread::ThreadId(i as u64)),
        })
        .collect();

    for cycle in 0..50 {
        let mut handles = Vec::new();
        for obj in &objects {
            handles.push(t.insert(*obj, Rights::READ_WRITE).unwrap());
        }

        for (h, obj) in handles.iter().zip(objects.iter()) {
            let entry = t.get(*h, Rights::READ).unwrap();
            assert_eq!(
                std::mem::discriminant(&entry),
                std::mem::discriminant(obj),
                "cycle {}: object type mismatch",
                cycle
            );
        }

        for h in handles {
            t.close(h).unwrap();
        }
    }
}

// ============================================================
// 2. Channel state machine: close interleaving + signal storms
//    (milestone 4: channel-handle audit, closed_count saturation)
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChanId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tid(u64);

fn chan_endpoint_index(id: ChanId) -> usize {
    id.0 as usize % 2
}

struct Channel {
    pending_signal: [bool; 2],
    waiter: [Option<Tid>; 2],
    closed_count: u8,
}

impl Channel {
    fn new() -> Self {
        Self {
            pending_signal: [false, false],
            waiter: [None, None],
            closed_count: 0,
        }
    }

    fn signal(&mut self, id: ChanId) -> Option<Tid> {
        let peer_ep = 1 - chan_endpoint_index(id);
        self.pending_signal[peer_ep] = true;
        self.waiter[peer_ep].take()
    }

    fn close_endpoint(&mut self, id: ChanId) -> (bool, Option<Tid>) {
        if self.closed_count >= 2 {
            return (false, None);
        }
        let ep = chan_endpoint_index(id);
        let peer_ep = 1 - ep;
        self.waiter[ep] = None;
        let peer_waiter = self.waiter[peer_ep].take();
        self.closed_count += 1;
        (self.closed_count == 2, peer_waiter)
    }

    fn register_waiter(&mut self, id: ChanId, waiter: Tid) {
        let ep = chan_endpoint_index(id);
        self.waiter[ep] = Some(waiter);
    }
}

/// Rapidly signal both endpoints many times, then close both.
#[test]
fn channel_rapid_bidirectional_signal_storm() {
    let ep_a = ChanId(0);
    let ep_b = ChanId(1);

    for _ in 0..200 {
        let mut ch = Channel::new();
        ch.register_waiter(ep_a, Tid(1));
        ch.register_waiter(ep_b, Tid(2));

        for round in 0..100 {
            ch.signal(ep_a);
            ch.signal(ep_b);
            assert!(
                ch.pending_signal[0] || ch.pending_signal[1],
                "round {}: at least one pending signal",
                round
            );
        }

        let (freed_a, _) = ch.close_endpoint(ep_a);
        assert!(!freed_a);
        let (freed_b, _) = ch.close_endpoint(ep_b);
        assert!(freed_b);

        // Triple-close must be harmless.
        let (freed_c, waiter) = ch.close_endpoint(ep_a);
        assert!(!freed_c);
        assert!(waiter.is_none());
    }
}

/// Close takes peer's waiter under pressure.
#[test]
fn channel_close_takes_peer_waiter_pressure() {
    for idx in 0..500u32 {
        let ep_a = ChanId(idx * 2);
        let ep_b = ChanId(idx * 2 + 1);
        let mut ch = Channel::new();

        ch.register_waiter(ep_b, Tid(idx as u64));
        let (_, peer_waiter) = ch.close_endpoint(ep_a);
        assert_eq!(peer_waiter, Some(Tid(idx as u64)));

        let (freed, _) = ch.close_endpoint(ep_b);
        assert!(freed);
    }
}

/// Closed_count saturation defense: bombardment must not exceed 2.
#[test]
fn channel_closed_count_saturation_bombardment() {
    let mut ch = Channel::new();
    ch.close_endpoint(ChanId(0));
    ch.close_endpoint(ChanId(1));

    for _ in 0..10_000 {
        let (freed, _) = ch.close_endpoint(ChanId(0));
        assert!(!freed);
    }
    for _ in 0..10_000 {
        let (freed, _) = ch.close_endpoint(ChanId(1));
        assert!(!freed);
    }
    assert_eq!(ch.closed_count, 2);
}

// ============================================================
// 3. EEVDF scheduling: extreme vruntime + weight combinations
//    (milestone 3: scheduler-algorithm audit, vruntime overflow)
// ============================================================

#[path = "../../kernel/scheduling_algorithm.rs"]
mod scheduling_algorithm;

use scheduling_algorithm::*;

fn eevdf_state(vruntime: u64, weight: u32, slice: u64, eligible_at: u64) -> SchedulingState {
    SchedulingState {
        vruntime,
        weight,
        requested_slice: slice,
        eligible_at,
    }
}

/// Charge with extreme elapsed values — must not panic.
#[test]
fn eevdf_charge_extreme_values() {
    let extreme_values = [0u64, 1, u64::MAX / 2, u64::MAX - 1, u64::MAX];

    for &initial_vruntime in &extreme_values {
        for &elapsed in &extreme_values {
            for &weight in &[1u32, DEFAULT_WEIGHT, u32::MAX] {
                let s = eevdf_state(initial_vruntime, weight, DEFAULT_SLICE_NS, 0);
                let charged = s.charge(elapsed);
                assert!(
                    charged.vruntime >= initial_vruntime,
                    "vruntime must not decrease: init={}, elapsed={}, w={}, result={}",
                    initial_vruntime,
                    elapsed,
                    weight,
                    charged.vruntime
                );
            }
        }
    }
}

/// Virtual deadline with extreme eligible_at and slice values.
#[test]
fn eevdf_virtual_deadline_extreme_values() {
    let extreme_values = [0u64, 1, u64::MAX / 2, u64::MAX - 1, u64::MAX];

    for &eligible_at in &extreme_values {
        for &slice in &extreme_values {
            for &weight in &[1u32, DEFAULT_WEIGHT, u32::MAX] {
                let s = eevdf_state(0, weight, slice, eligible_at);
                let deadline = s.virtual_deadline();
                assert!(
                    deadline >= eligible_at,
                    "deadline must be >= eligible_at: ea={}, slice={}, w={}, d={}",
                    eligible_at,
                    slice,
                    weight,
                    deadline
                );
            }
        }
    }
}

/// select_next with many threads near u64::MAX.
#[test]
fn eevdf_select_many_near_max_vruntimes() {
    let thread_count = 1000;
    let threads: Vec<(SchedulingState, bool)> = (0..thread_count)
        .map(|i| {
            let vruntime = u64::MAX - (i as u64 * 100);
            (
                eevdf_state(vruntime, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, vruntime),
                true,
            )
        })
        .collect();

    let states: Vec<SchedulingState> = threads.iter().map(|(s, _)| *s).collect();
    let avg = avg_vruntime(&states);

    let result = select_next(&threads, avg);
    assert!(result.is_some(), "must select a thread near-MAX vruntimes");
}

/// Fuzz: random-ish charge and select, verify no panics.
#[test]
fn eevdf_fuzz_charge_and_select_no_panic() {
    let mut seed: u64 = 0xDEADBEEF;
    let next = |s: &mut u64| -> u64 {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
        *s
    };

    for _ in 0..10_000 {
        let vruntime = next(&mut seed);
        let weight = ((next(&mut seed) % (u32::MAX as u64 - 1)) + 1) as u32;
        let slice = next(&mut seed);
        let eligible_at = next(&mut seed);
        let elapsed = next(&mut seed);

        let s = eevdf_state(vruntime, weight, slice, eligible_at);
        let _ = s.charge(elapsed);
        let _ = s.virtual_deadline();
        let _ = s.is_eligible(next(&mut seed));
    }
}

/// avg_vruntime with large thread counts.
#[test]
fn eevdf_avg_vruntime_large_count_no_overflow() {
    let states: Vec<SchedulingState> = (0..10_000)
        .map(|_| eevdf_state(u64::MAX, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0))
        .collect();

    let avg = avg_vruntime(&states);
    assert_eq!(avg, u64::MAX);
}

/// Weight=1: vruntime grows very fast (1024x multiplier). Verify it reaches
/// near-u64::MAX under extreme charge without wrapping to a small value.
#[test]
fn eevdf_minimum_weight_rapid_charge() {
    let mut s = eevdf_state(0, 1, DEFAULT_SLICE_NS, 0);
    // Each charge: delta = (elapsed as u128 * 1024 / 1) as u64.
    // With u64::MAX elapsed: intermediate = u64::MAX * 1024 (u128), truncated
    // to u64 → u64::MAX - 1023. Then saturating_add from 0 gives u64::MAX - 1023.
    // This is correct — u128 truncation, not wrapping arithmetic.
    s = s.charge(u64::MAX);
    assert!(
        s.vruntime > u64::MAX - 2048,
        "weight=1 with u64::MAX elapsed must be near MAX, got {}",
        s.vruntime
    );

    // Further charges keep pushing toward MAX.
    let prev = s.vruntime;
    s = s.charge(u64::MAX);
    assert!(
        s.vruntime >= prev,
        "vruntime must not decrease (saturating): {} -> {}",
        prev,
        s.vruntime
    );

    // Eventually saturates at u64::MAX.
    assert_eq!(s.vruntime, u64::MAX, "two MAX charges must saturate");
}

/// Weight=u32::MAX: vruntime grows very slowly.
#[test]
fn eevdf_maximum_weight_slow_charge() {
    let mut s = eevdf_state(0, u32::MAX, DEFAULT_SLICE_NS, 0);
    for _ in 0..1000 {
        s = s.charge(1_000_000_000);
    }
    assert!(
        s.vruntime < 1_000_000,
        "weight=MAX should be slow, got {}",
        s.vruntime
    );
}

/// All ineligible threads: fallback must work for all sizes.
#[test]
fn eevdf_all_ineligible_fallback_pressure() {
    for thread_count in 1..=50 {
        let threads: Vec<(SchedulingState, bool)> = (0..thread_count)
            .map(|i| {
                let vruntime = 1000 + i as u64 * 10;
                (
                    eevdf_state(vruntime, DEFAULT_WEIGHT, DEFAULT_SLICE_NS, 0),
                    true,
                )
            })
            .collect();

        let avg = 999; // Below all vruntimes.
        let result = select_next(&threads, avg);
        assert!(
            result.is_some(),
            "fallback must select (n={})",
            thread_count
        );
        assert_eq!(result.unwrap(), 0, "fallback picks smallest vruntime");
    }
}

// ============================================================
// 4. Futex: hash collision + heavy contention
//    (milestone 4: sync-primitives audit)
// ============================================================

const BUCKET_COUNT: usize = 64;

fn bucket_index(pa: u64) -> usize {
    ((pa >> 2) as usize) % BUCKET_COUNT
}

struct FutexWaiter {
    thread_id: u64,
    pa: u64,
}

struct FutexWaitTable {
    buckets: Vec<Vec<FutexWaiter>>,
}

impl FutexWaitTable {
    fn new() -> Self {
        Self {
            buckets: (0..BUCKET_COUNT).map(|_| Vec::new()).collect(),
        }
    }

    fn wait(&mut self, pa: u64, thread_id: u64) {
        let idx = bucket_index(pa);
        self.buckets[idx].push(FutexWaiter { thread_id, pa });
    }

    fn wake(&mut self, pa: u64, count: u32) -> Vec<u64> {
        let idx = bucket_index(pa);
        let bucket = &mut self.buckets[idx];
        let mut collected = Vec::new();
        let mut i = 0;
        while i < bucket.len() && collected.len() < count as usize {
            if bucket[i].pa == pa {
                let waiter = bucket.swap_remove(i);
                collected.push(waiter.thread_id);
            } else {
                i += 1;
            }
        }
        collected
    }

    fn wake_all(&mut self, pa: u64) -> Vec<u64> {
        self.wake(pa, u32::MAX)
    }

    fn total_waiters(&self) -> usize {
        self.buckets.iter().map(|b| b.len()).sum()
    }
}

/// Many threads waiting on hash-colliding addresses.
#[test]
fn futex_hash_collision_stress() {
    let mut table = FutexWaitTable::new();
    let colliding: Vec<u64> = (0..10_000u64)
        .filter(|a| bucket_index(*a) == 0)
        .take(500)
        .collect();
    assert!(colliding.len() >= 100);

    for (i, &pa) in colliding.iter().enumerate() {
        table.wait(pa, i as u64);
    }

    let mut woken = 0;
    for &pa in &colliding {
        woken += table.wake(pa, 1).len();
    }

    assert_eq!(woken, colliding.len());
    assert_eq!(table.total_waiters(), 0);
}

/// Rapid wait/wake cycles on same address.
#[test]
fn futex_rapid_wait_wake_same_address() {
    let mut table = FutexWaitTable::new();
    let pa = 0x1000;

    for cycle in 0..5_000 {
        table.wait(pa, cycle);
        let woken = table.wake(pa, 1);
        assert_eq!(woken.len(), 1);
        assert_eq!(woken[0], cycle);
    }
    assert_eq!(table.total_waiters(), 0);
}

/// Many addresses, wake_all for each.
#[test]
fn futex_many_addresses_wake_all() {
    let mut table = FutexWaitTable::new();

    for addr in 0..1000u64 {
        let pa = addr * 4;
        for t in 0..10 {
            table.wait(pa, addr * 10 + t);
        }
    }

    assert_eq!(table.total_waiters(), 10_000);

    for addr in 0..1000u64 {
        let woken = table.wake_all(addr * 4);
        assert_eq!(woken.len(), 10, "addr {}", addr);
    }
    assert_eq!(table.total_waiters(), 0);
}

/// Wake with count=0 should wake nobody.
#[test]
fn futex_wake_zero_count() {
    let mut table = FutexWaitTable::new();
    let pa = 0x2000;

    for i in 0..100 {
        table.wait(pa, i);
    }

    let woken = table.wake(pa, 0);
    assert!(woken.is_empty());
    assert_eq!(table.total_waiters(), 100);
    table.wake_all(pa);
}

// ============================================================
// 5. Timer deadline: overflow and boundary values
//    (milestone 5: interrupt-timer audit, saturating_add fix)
// ============================================================

fn compute_deadline(now: u64, timeout_ns: u64, freq: u64) -> u64 {
    if timeout_ns == 0 {
        now
    } else {
        let delta = (timeout_ns as u128 * freq as u128 / 1_000_000_000) as u64;
        now.saturating_add(delta)
    }
}

fn timer_is_expired(now: u64, deadline: u64) -> bool {
    now >= deadline
}

fn counter_to_ns(ticks: u64, freq: u64) -> u64 {
    if freq == 0 {
        return 0;
    }
    (ticks as u128 * 1_000_000_000 / freq as u128) as u64
}

/// Fuzz deadline with extreme values — must never panic.
#[test]
fn timer_fuzz_deadline_extreme() {
    let extreme_now = [0u64, 1, u64::MAX / 2, u64::MAX - 1, u64::MAX];
    let extreme_timeout = [0u64, 1, 1_000_000_000, u64::MAX / 2, u64::MAX];
    let extreme_freq = [1u64, 1_000_000, 62_500_000, u64::MAX / 2, u64::MAX];

    for &now in &extreme_now {
        for &timeout_ns in &extreme_timeout {
            for &freq in &extreme_freq {
                let deadline = compute_deadline(now, timeout_ns, freq);
                assert!(
                    deadline >= now,
                    "now={}, t={}, f={}, d={}",
                    now,
                    timeout_ns,
                    freq,
                    deadline
                );
                if timeout_ns == 0 {
                    assert_eq!(deadline, now);
                }
            }
        }
    }
}

/// Large timeout near wrap: saturates instead of wrapping (the bug).
#[test]
fn timer_large_timeout_near_wrap_saturates() {
    let freq = 62_500_000u64;
    let now = u64::MAX - 1_000_000;
    let timeout_ns = 60_000_000_000u64;

    let deadline = compute_deadline(now, timeout_ns, freq);
    assert_eq!(deadline, u64::MAX);
    assert!(!timer_is_expired(now, deadline));
}

/// counter_to_ns: no panics with extreme values.
#[test]
fn timer_fuzz_counter_to_ns() {
    let extreme = [0u64, 1, u64::MAX / 2, u64::MAX];
    for &ticks in &extreme {
        for &freq in &extreme {
            let _ = counter_to_ns(ticks, freq);
        }
    }
}

/// Rapid timers near wrap point.
#[test]
fn timer_rapid_near_wrap_point() {
    let freq = 62_500_000u64;
    for offset in 0..1000u64 {
        let now = u64::MAX - offset;
        let deadline = compute_deadline(now, 1_000, freq);
        assert!(deadline >= now);
        let deadline = compute_deadline(now, 1_000_000_000, freq);
        assert!(deadline >= now);
    }
}

// ============================================================
// 6. Integer overflow in page_count / VMA end
//    (milestone 2: process-lifecycle audit)
// ============================================================

const PAGE_SIZE: u64 = 16384;

fn checked_page_count(mem_size: u64) -> Option<u64> {
    mem_size.checked_add(PAGE_SIZE - 1).map(|n| n / PAGE_SIZE)
}

fn checked_vma_end(base_va: u64, page_count: u64) -> Option<u64> {
    page_count
        .checked_mul(PAGE_SIZE)
        .and_then(|size| base_va.checked_add(size))
}

/// Fuzz boundary between overflow and non-overflow.
#[test]
fn process_fuzz_page_count_boundary() {
    let boundary = u64::MAX - PAGE_SIZE + 2;

    for offset in 0..1000u64 {
        let mem_size = boundary.saturating_sub(offset + 1);
        assert!(
            checked_page_count(mem_size).is_some(),
            "mem_size {} below boundary",
            mem_size
        );
    }

    for offset in 0..1000u64 {
        let mem_size = boundary.saturating_add(offset);
        if mem_size >= boundary {
            assert!(
                checked_page_count(mem_size).is_none(),
                "mem_size {} at/above boundary",
                mem_size
            );
        }
    }
}

/// Fuzz VMA end with extreme combos.
#[test]
fn process_fuzz_vma_end_extreme() {
    let extreme = [
        0u64,
        1,
        PAGE_SIZE,
        u64::MAX / 2,
        u64::MAX - PAGE_SIZE,
        u64::MAX,
    ];
    for &base_va in &extreme {
        for &page_count in &extreme {
            let result = checked_vma_end(base_va, page_count);
            if let Some(end) = result {
                assert!(end >= base_va);
            }
        }
    }
}

/// Adversarial ELF segments: no panics.
#[test]
fn process_adversarial_elf_segments() {
    let segments = [
        (0x400000u64, 0u64),
        (0x400000, 1),
        (0x400000, PAGE_SIZE),
        (0x400000, u64::MAX),
        (u64::MAX, 1),
        (u64::MAX, u64::MAX),
        (0, u64::MAX - PAGE_SIZE + 1),
        (1, u64::MAX - PAGE_SIZE + 1),
    ];

    for (base_va, mem_size) in &segments {
        if let Some(pc) = checked_page_count(*mem_size) {
            let _ = checked_vma_end(*base_va, pc);
        }
    }
}

// ============================================================
// 7. Scheduler state machine: rapid transitions
//    (milestones 3+6: thread-lifecycle, cross-module)
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SchedThreadState {
    Ready,
    Running,
    Blocked,
    Exited,
}

struct SchedThread {
    id: u64,
    state: SchedThreadState,
}

impl SchedThread {
    fn new(id: u64) -> Self {
        Self {
            id,
            state: SchedThreadState::Ready,
        }
    }

    fn activate(&mut self) {
        assert_eq!(self.state, SchedThreadState::Ready);
        self.state = SchedThreadState::Running;
    }

    fn deschedule(&mut self) {
        if self.state == SchedThreadState::Running {
            self.state = SchedThreadState::Ready;
        }
    }

    fn block(&mut self) {
        assert_eq!(self.state, SchedThreadState::Running);
        self.state = SchedThreadState::Blocked;
    }

    fn wake(&mut self) -> bool {
        if self.state == SchedThreadState::Blocked {
            self.state = SchedThreadState::Ready;
            true
        } else {
            false
        }
    }

    fn mark_exited(&mut self) {
        self.state = SchedThreadState::Exited;
    }
}

struct SchedState {
    ready: Vec<SchedThread>,
    blocked: Vec<SchedThread>,
    running: Option<SchedThread>,
    deferred_drops: Vec<SchedThread>,
}

impl SchedState {
    fn new() -> Self {
        Self {
            ready: Vec::new(),
            blocked: Vec::new(),
            running: None,
            deferred_drops: Vec::new(),
        }
    }

    fn add_thread(&mut self, t: SchedThread) {
        self.ready.push(t);
    }

    fn schedule(&mut self) {
        self.deferred_drops.clear();
        self.ready.retain(|t| t.state != SchedThreadState::Exited);

        if let Some(mut current) = self.running.take() {
            current.deschedule();
            match current.state {
                SchedThreadState::Ready => self.ready.push(current),
                SchedThreadState::Blocked => self.blocked.push(current),
                SchedThreadState::Exited => self.deferred_drops.push(current),
                SchedThreadState::Running => unreachable!(),
            }
        }

        if !self.ready.is_empty() {
            let mut next = self.ready.remove(0);
            next.activate();
            self.running = Some(next);
        }
    }

    fn block_current(&mut self) {
        if let Some(ref mut t) = self.running {
            t.block();
        }
    }

    fn wake_thread(&mut self, id: u64) -> bool {
        if let Some(pos) = self.blocked.iter().position(|t| t.id == id) {
            let mut t = self.blocked.swap_remove(pos);
            t.wake();
            self.ready.push(t);
            true
        } else {
            false
        }
    }

    fn exit_current(&mut self) {
        if let Some(ref mut t) = self.running {
            t.mark_exited();
        }
    }

    fn total_threads(&self) -> usize {
        self.ready.len()
            + self.blocked.len()
            + if self.running.is_some() { 1 } else { 0 }
            + self.deferred_drops.len()
    }
}

/// Rapid create → schedule → exit.
#[test]
fn sched_rapid_create_schedule_exit() {
    let mut s = SchedState::new();
    for i in 0..5_000 {
        s.add_thread(SchedThread::new(i));
        s.schedule(); // Picks thread, activates it. Also drains deferred from prev iteration.
        s.exit_current(); // Marks running thread as Exited.
    }
    // Final schedule to move last exited into deferred_drops, then one more to drain.
    s.schedule();
    s.schedule();
    assert_eq!(s.total_threads(), 0);
}

/// Many threads round-robin.
#[test]
fn sched_many_threads_round_robin() {
    let mut s = SchedState::new();
    let n = 1_000u64;
    for i in 0..n {
        s.add_thread(SchedThread::new(i));
    }
    for _ in 0..n * 3 {
        s.schedule();
    }
    for _ in 0..n {
        s.schedule();
        s.exit_current();
    }
    // Two final schedules: one to move last exited to deferred_drops, one to drain.
    s.schedule();
    s.schedule();
    assert_eq!(s.total_threads(), 0);
}

/// Block/wake interleaving.
#[test]
fn sched_block_wake_interleaving() {
    let mut s = SchedState::new();
    s.add_thread(SchedThread::new(1));
    s.add_thread(SchedThread::new(2));

    for _ in 0..2_000 {
        s.schedule();
        if let Some(ref t) = s.running {
            let id = t.id;
            s.block_current();
            s.schedule(); // Switches to other thread.
            s.wake_thread(id); // Wake the blocked one.
        }
    }

    // Exit both threads.
    s.schedule();
    s.exit_current();
    s.schedule(); // Moves exited to deferred_drops, picks second thread.
    s.exit_current();
    s.schedule(); // Moves second to deferred_drops.
    s.schedule(); // Drains deferred_drops.
    assert_eq!(s.total_threads(), 0);
}

/// Deferred drop: exited threads are drained at the start of next schedule.
#[test]
fn sched_deferred_drop_lifecycle() {
    let mut s = SchedState::new();
    for i in 0..500 {
        s.add_thread(SchedThread::new(i));
        s.schedule(); // Picks thread i, also drains any previous deferred_drops.
        s.exit_current(); // Marks as Exited.
                          // The thread is still in 'running' as Exited. Next schedule will
                          // move it to deferred_drops and drain the PREVIOUS deferred_drops.
    }
    // After loop: last exited thread is still in running slot.
    s.schedule(); // Moves last exited to deferred_drops, nothing to pick.
    s.schedule(); // Drains deferred_drops.
    assert!(s.deferred_drops.is_empty());
    assert_eq!(s.total_threads(), 0);
}

/// Kill threads in ready queue (mirrors kill_process).
#[test]
fn sched_kill_threads_in_ready_queue() {
    let mut s = SchedState::new();
    for i in 0..100 {
        s.add_thread(SchedThread::new(i));
    }

    for i in (0..100u64).step_by(2) {
        if let Some(t) = s.ready.iter_mut().find(|t| t.id == i) {
            t.mark_exited();
        }
    }

    for _ in 0..60 {
        s.schedule();
        if s.running.is_some() {
            s.exit_current();
        }
    }

    s.schedule();
    assert_eq!(s.total_threads(), 0);
}

// ============================================================
// 8. Cross-module: process slot lifecycle
//    (milestone 6: cross-module-lifetime audit, process slot leak)
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcId(u32);

struct ProcessEntry {
    id: ProcId,
    threads: Vec<u64>,
}

struct ProcessTable {
    slots: Vec<Option<ProcessEntry>>,
}

impl ProcessTable {
    fn new(capacity: usize) -> Self {
        Self {
            slots: (0..capacity).map(|_| None).collect(),
        }
    }

    fn create_process(&mut self) -> Option<ProcId> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                let pid = ProcId(i as u32);
                *slot = Some(ProcessEntry {
                    id: pid,
                    threads: Vec::new(),
                });
                return Some(pid);
            }
        }
        None
    }

    fn remove_empty_process(&mut self, pid: ProcId) -> bool {
        if let Some(slot) = self.slots.get_mut(pid.0 as usize) {
            if let Some(process) = slot {
                if process.threads.is_empty() {
                    *slot = None;
                    return true;
                }
            }
        }
        false
    }

    fn add_thread(&mut self, pid: ProcId, tid: u64) -> bool {
        if let Some(Some(process)) = self.slots.get_mut(pid.0 as usize) {
            process.threads.push(tid);
            true
        } else {
            false
        }
    }

    fn remove_process(&mut self, pid: ProcId) {
        if let Some(slot) = self.slots.get_mut(pid.0 as usize) {
            *slot = None;
        }
    }

    fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

/// Process slot leak defense: create, fail spawn, clean up.
#[test]
fn cross_module_process_slot_leak_defense() {
    let mut table = ProcessTable::new(64);

    for i in 0..10_000u64 {
        let pid = table.create_process().expect("table should not be full");
        let spawn_success = i % 3 != 0;

        if spawn_success {
            table.add_thread(pid, i);
            table.remove_process(pid);
        } else {
            assert!(table.remove_empty_process(pid));
        }
    }

    assert_eq!(table.active_count(), 0);
}

/// Table exhaustion recovery.
#[test]
fn cross_module_process_table_exhaustion_recovery() {
    let capacity = 64;
    let mut table = ProcessTable::new(capacity);

    for cycle in 0..100 {
        let mut pids = Vec::new();
        while let Some(pid) = table.create_process() {
            pids.push(pid);
        }
        assert_eq!(pids.len(), capacity, "cycle {}", cycle);
        assert!(table.create_process().is_none());

        for pid in pids {
            table.remove_process(pid);
        }
        assert_eq!(table.active_count(), 0);
    }
}

// ============================================================
// 9. Ticket spinlock: counter wrapping stress
//    (milestone 4: sync-primitives audit)
// ============================================================

use std::sync::atomic::{AtomicU32, Ordering};

struct TicketLock {
    next_ticket: AtomicU32,
    now_serving: AtomicU32,
}

impl TicketLock {
    const fn new() -> Self {
        Self {
            next_ticket: AtomicU32::new(0),
            now_serving: AtomicU32::new(0),
        }
    }

    fn lock(&self) -> u32 {
        let my_ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        while self.now_serving.load(Ordering::Acquire) != my_ticket {
            core::hint::spin_loop();
        }
        my_ticket
    }

    fn unlock(&self) {
        self.now_serving.fetch_add(1, Ordering::Release);
    }
}

/// Rapid lock/unlock: 100k cycles.
#[test]
fn spinlock_rapid_lock_unlock() {
    let lock = TicketLock::new();
    for i in 0..100_000u32 {
        let ticket = lock.lock();
        assert_eq!(ticket, i);
        lock.unlock();
    }
    assert_eq!(lock.next_ticket.load(Ordering::Relaxed), 100_000);
    assert_eq!(lock.now_serving.load(Ordering::Relaxed), 100_000);
}

/// Counter wrapping near u32::MAX.
#[test]
fn spinlock_counter_wrapping() {
    let lock = TicketLock {
        next_ticket: AtomicU32::new(u32::MAX - 100),
        now_serving: AtomicU32::new(u32::MAX - 100),
    };

    for _ in 0..200 {
        let _ = lock.lock();
        lock.unlock();
    }

    let next = lock.next_ticket.load(Ordering::Relaxed);
    let serving = lock.now_serving.load(Ordering::Relaxed);
    assert_eq!(next, serving);
    assert_eq!(next, (u32::MAX - 100).wrapping_add(200));
}

// ============================================================
// 10. Waitable registry: large-scale register/notify/destroy
//     (milestone 4: sync-primitives audit)
// ============================================================

struct WaitableEntry {
    waiter: Option<u64>,
    ready: bool,
}

struct WaitableRegistry {
    entries: HashMap<u32, WaitableEntry>,
}

impl WaitableRegistry {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn create(&mut self, id: u32) {
        self.entries.insert(
            id,
            WaitableEntry {
                waiter: None,
                ready: false,
            },
        );
    }

    fn register_waiter(&mut self, id: u32, tid: u64) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.waiter = Some(tid);
        }
    }

    fn notify(&mut self, id: u32) -> Option<u64> {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.ready = true;
            entry.waiter.take()
        } else {
            None
        }
    }

    fn destroy(&mut self, id: u32) {
        self.entries.remove(&id);
    }
}

/// Mass create/notify/destroy.
#[test]
fn waitable_mass_create_notify_destroy() {
    let mut reg = WaitableRegistry::new();

    for cycle in 0..50u32 {
        let count = 1000u32;
        for i in 0..count {
            let id = cycle * count + i;
            reg.create(id);
            reg.register_waiter(id, i as u64);
        }

        for i in 0..count {
            let woken = reg.notify(cycle * count + i);
            assert_eq!(woken, Some(i as u64));
        }
    }

    let keys: Vec<u32> = reg.entries.keys().cloned().collect();
    for id in keys {
        reg.destroy(id);
    }
    assert!(reg.entries.is_empty());
}

/// Notify without waiter — must return None.
#[test]
fn waitable_notify_without_waiter_storm() {
    let mut reg = WaitableRegistry::new();
    for i in 0..5_000 {
        reg.create(i);
        assert!(reg.notify(i).is_none());
        reg.destroy(i);
    }
}

/// Double notify returns None on second call.
#[test]
fn waitable_double_notify() {
    let mut reg = WaitableRegistry::new();
    for i in 0..2_000 {
        reg.create(i);
        reg.register_waiter(i, i as u64);
        assert!(reg.notify(i).is_some());
        assert!(reg.notify(i).is_none());
        reg.destroy(i);
    }
}
