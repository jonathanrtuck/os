//! Tests for the compositor's frame scheduler state machine.
//!
//! Validates event coalescing, idle optimization, configurable cadence,
//! and render/present counting. The frame scheduler is a pure state machine
//! that can be tested without kernel syscalls.

#[path = "../../libraries/render/frame_scheduler.rs"]
mod frame_scheduler;

use frame_scheduler::FrameScheduler;

// ── VAL-FRAME-001: Frame timer fires at configured cadence ──────────

#[test]
fn frame_period_ns_60fps() {
    // 60fps → 16,666,666 ns (integer division of 1e9/60)
    let ns = frame_scheduler::frame_period_ns(60);
    assert_eq!(ns, 16_666_666);
}

#[test]
fn frame_period_ns_30fps() {
    let ns = frame_scheduler::frame_period_ns(30);
    assert_eq!(ns, 33_333_333);
}

#[test]
fn frame_period_ns_120fps() {
    let ns = frame_scheduler::frame_period_ns(120);
    assert_eq!(ns, 8_333_333);
}

#[test]
fn frame_period_ns_0fps_fallback() {
    // 0fps is invalid → defaults to 60fps period
    let ns = frame_scheduler::frame_period_ns(0);
    assert_eq!(ns, 16_666_666);
}

/// Simulate 1 second of timer ticks at 60fps with continuous dirty scene.
/// Should count 60 ticks (VAL-FRAME-001).
#[test]
fn frame_timer_60_ticks_per_second_with_dirty() {
    let mut sched = FrameScheduler::new(60);

    // Simulate 60 ticks with scene updates before each tick.
    for _ in 0..60 {
        sched.on_scene_update();
        let should_render = sched.on_timer_tick();
        assert!(should_render);
        sched.on_render_complete();
    }

    assert_eq!(sched.tick_count, 60);
    assert_eq!(sched.render_count, 60);
}

// ── VAL-FRAME-002: Event coalescing — multiple updates, one render ──

/// 5 scene updates within one frame period produce exactly 1 render.
#[test]
fn event_coalescing_five_updates_one_render() {
    let mut sched = FrameScheduler::new(60);

    // Five scene updates arrive before the timer tick.
    sched.on_scene_update();
    sched.on_scene_update();
    sched.on_scene_update();
    sched.on_scene_update();
    sched.on_scene_update();

    // Timer tick fires.
    let should_render = sched.on_timer_tick();
    assert!(should_render, "should render after scene updates");
    sched.on_render_complete();

    assert_eq!(sched.render_count, 1, "exactly 1 render after 5 updates");
    assert_eq!(sched.tick_count, 1);
}

/// Multiple updates followed by multiple ticks with no new updates.
/// Only the first tick should render (then scene is clean).
#[test]
fn coalescing_then_idle_ticks() {
    let mut sched = FrameScheduler::new(60);

    // 3 updates, then tick → should render
    sched.on_scene_update();
    sched.on_scene_update();
    sched.on_scene_update();
    assert!(sched.on_timer_tick());
    sched.on_render_complete();

    // Next 5 ticks with no updates → should NOT render
    for _ in 0..5 {
        assert!(!sched.on_timer_tick());
    }

    assert_eq!(sched.render_count, 1);
    assert_eq!(sched.idle_skip_count, 5);
}

// ── VAL-FRAME-003: Event coalescing — final state wins ──────────────

/// Two scene updates within one frame period — the rendered frame
/// reflects the latest state (last update wins). The scheduler itself
/// doesn't track content, but it ensures only one render happens.
#[test]
fn final_state_wins_one_render_per_tick() {
    let mut sched = FrameScheduler::new(60);

    // Two updates ("a" then "b") in same frame period.
    sched.on_scene_update(); // "a"
    sched.on_scene_update(); // "b" overwrites

    // Single tick → one render (compositor reads latest state from scene graph).
    let should_render = sched.on_timer_tick();
    assert!(should_render);
    sched.on_render_complete();

    assert_eq!(sched.render_count, 1);
    assert_eq!(sched.tick_count, 1);
}

// ── VAL-FRAME-005: Idle optimization — no render when nothing changed

/// 100 timer ticks with no scene updates → 0 renders.
#[test]
fn idle_optimization_100_ticks_no_renders() {
    let mut sched = FrameScheduler::new(60);

    for _ in 0..100 {
        let should_render = sched.on_timer_tick();
        assert!(!should_render, "no render when not dirty");
    }

    assert_eq!(sched.tick_count, 100);
    assert_eq!(sched.render_count, 0, "zero renders during idle");
    assert_eq!(sched.idle_skip_count, 100);
}

// ── VAL-FRAME-006: Idle-to-active wakeup latency ────────────────────

/// After idle period, a scene update produces a render on the next tick.
#[test]
fn idle_to_active_wakeup() {
    let mut sched = FrameScheduler::new(60);

    // 30 idle ticks (simulating 500ms at 60fps).
    for _ in 0..30 {
        assert!(!sched.on_timer_tick());
    }

    assert_eq!(sched.render_count, 0);

    // Scene update arrives.
    sched.on_scene_update();

    // Very next timer tick should trigger a render.
    assert!(
        sched.on_timer_tick(),
        "should render on first tick after update"
    );
    sched.on_render_complete();

    assert_eq!(sched.render_count, 1);
}

// ── VAL-FRAME-011: First frame renders within one frame period ──────

/// The first scene update from core renders on the next tick.
#[test]
fn first_scene_update_renders_on_next_tick() {
    let mut sched = FrameScheduler::new(60);

    // Core publishes initial scene.
    sched.on_scene_update();

    // Timer tick fires.
    assert!(sched.on_timer_tick());
    sched.on_render_complete();

    assert_eq!(sched.render_count, 1);
    assert_eq!(sched.tick_count, 1);
}

// ── VAL-FRAME-012: GPU present count equals render count ────────────

/// Over many frames of mixed idle and active, render_count == gpu_present_count.
#[test]
fn gpu_present_count_matches_render_count() {
    let mut sched = FrameScheduler::new(60);

    // 10 ticks with updates (active).
    for _ in 0..10 {
        sched.on_scene_update();
        let should_render = sched.on_timer_tick();
        assert!(should_render);
        sched.on_render_complete();
    }

    // 20 idle ticks.
    for _ in 0..20 {
        assert!(!sched.on_timer_tick());
    }

    // 5 more active ticks.
    for _ in 0..5 {
        sched.on_scene_update();
        assert!(sched.on_timer_tick());
        sched.on_render_complete();
    }

    // 15 idle ticks.
    for _ in 0..15 {
        assert!(!sched.on_timer_tick());
    }

    assert_eq!(sched.render_count, 15);
    assert_eq!(
        sched.render_count, sched.gpu_present_count,
        "render_count must equal gpu_present_count"
    );
    assert_eq!(sched.tick_count, 50);
}

// ── VAL-FRAME-013: Compositor does not busy-spin when idle ──────────

/// When no scene updates arrive, on_timer_tick returns false,
/// meaning the compositor blocks in sys::wait and does nothing between ticks.
/// This test verifies that the scheduler never returns true without a
/// preceding scene update.
#[test]
fn no_busy_spin_no_renders_without_update() {
    let mut sched = FrameScheduler::new(60);
    let mut spurious_renders = 0;

    // Simulate 500 ticks (about 8 seconds at 60fps) with NO scene updates.
    for _ in 0..500 {
        if sched.on_timer_tick() {
            spurious_renders += 1;
            sched.on_render_complete();
        }
    }

    assert_eq!(spurious_renders, 0, "no renders during idle — no busy spin");
    assert_eq!(sched.render_count, 0);
}

// ── Additional tests for robustness ─────────────────────────────────

/// Dirty flag persists across multiple ticks if on_render_complete is not called.
/// (This shouldn't happen in practice, but verifies the state machine.)
#[test]
fn dirty_flag_persists_until_render_complete() {
    let mut sched = FrameScheduler::new(60);

    sched.on_scene_update();

    // Multiple ticks without render_complete → still dirty.
    assert!(sched.on_timer_tick());
    // Note: if we don't call on_render_complete, dirty stays set.
    assert!(sched.on_timer_tick());
    assert!(sched.on_timer_tick());

    // All 3 ticks returned true because dirty was never cleared.
    assert_eq!(sched.tick_count, 3);
    assert_eq!(sched.render_count, 0); // never called on_render_complete
}

/// Scene update between ticks re-dirties after a render.
#[test]
fn re_dirty_between_frames() {
    let mut sched = FrameScheduler::new(60);

    // Frame 1: dirty → tick → render.
    sched.on_scene_update();
    assert!(sched.on_timer_tick());
    sched.on_render_complete();

    // Idle tick.
    assert!(!sched.on_timer_tick());

    // Frame 3: new update → dirty again.
    sched.on_scene_update();
    assert!(sched.on_timer_tick());
    sched.on_render_complete();

    assert_eq!(sched.render_count, 2);
}

/// Reset counters works correctly.
#[test]
fn reset_counters_clears_all() {
    let mut sched = FrameScheduler::new(60);

    sched.on_scene_update();
    sched.on_timer_tick();
    sched.on_render_complete();

    assert_eq!(sched.render_count, 1);
    assert_eq!(sched.tick_count, 1);
    assert_eq!(sched.gpu_present_count, 1);

    sched.reset_counters();

    assert_eq!(sched.render_count, 0);
    assert_eq!(sched.tick_count, 0);
    assert_eq!(sched.gpu_present_count, 0);
    assert_eq!(sched.idle_skip_count, 0);
}

/// High-frequency scenario: many updates between each tick.
#[test]
fn burst_updates_coalesced() {
    let mut sched = FrameScheduler::new(60);

    // Simulate 10 frames, each with a burst of 50 updates.
    for _ in 0..10 {
        for _ in 0..50 {
            sched.on_scene_update();
        }
        assert!(sched.on_timer_tick());
        sched.on_render_complete();
    }

    assert_eq!(sched.render_count, 10, "10 renders for 500 updates");
    assert_eq!(sched.tick_count, 10);
}

/// Alternating idle and active periods.
#[test]
fn alternating_idle_active() {
    let mut sched = FrameScheduler::new(60);

    for cycle in 0..5 {
        // Active: 3 ticks with updates.
        for _ in 0..3 {
            sched.on_scene_update();
            assert!(sched.on_timer_tick(), "active tick in cycle {cycle}");
            sched.on_render_complete();
        }
        // Idle: 3 ticks without updates.
        for _ in 0..3 {
            assert!(!sched.on_timer_tick(), "idle tick in cycle {cycle}");
        }
    }

    assert_eq!(sched.render_count, 15); // 5 cycles × 3 active ticks
    assert_eq!(sched.tick_count, 30); // 5 cycles × 6 ticks total
    assert_eq!(sched.idle_skip_count, 15); // 5 cycles × 3 idle ticks
}

/// Period configuration is accessible.
#[test]
fn period_ns_accessible() {
    let sched = FrameScheduler::new(60);
    assert_eq!(sched.period_ns(), 16_666_666);
}

/// Default state is clean (not dirty).
#[test]
fn initial_state_clean() {
    let sched = FrameScheduler::new(60);
    assert!(!sched.is_dirty());
    assert_eq!(sched.tick_count, 0);
    assert_eq!(sched.render_count, 0);
    assert_eq!(sched.gpu_present_count, 0);
}

// ── VAL-FRAME-004: Frame budgeting — skip overdue frames ────────────

/// If rendering takes >2x frame period, the scheduler skips the missed
/// deadline and renders at the next cadence tick. No back-to-back renders.
#[test]
fn frame_budgeting_skip_overdue() {
    let period = frame_scheduler::frame_period_ns(60);
    let mut sched = FrameScheduler::new(60);

    // Frame 1: normal render at t=period.
    sched.on_scene_update();
    sched.on_timer_tick_at(period); // tick at t=period
                                    // Rendering takes 3x period (overrun).
    sched.on_render_complete_at(period + 3 * period);

    // Scene is still dirty (new update during render).
    sched.on_scene_update();

    // Next timer tick at t=2*period — this was MISSED (render ended at t=4*period).
    // The scheduler should detect overrun and skip it.
    let should = sched.on_timer_tick_at(2 * period);
    assert!(!should, "should skip overdue tick after render overrun");
    assert_eq!(sched.overrun_skip_count, 1);

    // Next timer tick at t=5*period — past the overrun window, should render.
    let should = sched.on_timer_tick_at(5 * period);
    assert!(should, "should render at tick after overrun window");
    sched.on_render_complete_at(5 * period + period / 2);

    assert_eq!(sched.render_count, 2);
}

/// No back-to-back catch-up renders after an overrun.
#[test]
fn no_catchup_renders_after_overrun() {
    let period = frame_scheduler::frame_period_ns(60);
    let mut sched = FrameScheduler::new(60);

    // Normal render at t=period.
    sched.on_scene_update();
    sched.on_timer_tick_at(period);
    sched.on_render_complete_at(period + period / 2);

    // Scene update arrives continuously.
    sched.on_scene_update();

    // Render that takes >2x period.
    sched.on_timer_tick_at(2 * period);
    // Render ends well past the next tick time.
    sched.on_render_complete_at(2 * period + 3 * period);

    sched.on_scene_update();

    // Two quick ticks that arrive during or after overrun:
    let r1 = sched.on_timer_tick_at(3 * period);
    let r2 = sched.on_timer_tick_at(4 * period);

    // At least one of these should be skipped (overrun detection).
    let total_skipped = (!r1 as u32) + (!r2 as u32);
    assert!(
        total_skipped >= 1,
        "at least one tick skipped during overrun, got {total_skipped} skips"
    );

    // Next tick well past overrun should render.
    sched.on_scene_update();
    let should = sched.on_timer_tick_at(6 * period);
    assert!(should, "should resume rendering after overrun");
}

// ── VAL-FRAME-007: Configurable cadence — 30fps ────────────────────

/// At 30fps, period is 33.3ms, and 30 consecutive dirty ticks produce 30 renders.
#[test]
fn configurable_cadence_30fps() {
    let mut sched = FrameScheduler::new(30);
    assert_eq!(sched.period_ns(), frame_scheduler::frame_period_ns(30));

    let period = sched.period_ns();
    for i in 0..30 {
        sched.on_scene_update();
        let t = (i + 1) as u64 * period;
        assert!(sched.on_timer_tick_at(t));
        sched.on_render_complete_at(t + period / 2);
    }
    assert_eq!(sched.render_count, 30, "30 renders at 30fps");
}

// ── VAL-FRAME-008: Configurable cadence — 120fps ───────────────────

/// At 120fps, period is 8.3ms, and 120 consecutive dirty ticks produce 120 renders.
#[test]
fn configurable_cadence_120fps() {
    let mut sched = FrameScheduler::new(120);
    assert_eq!(sched.period_ns(), frame_scheduler::frame_period_ns(120));

    let period = sched.period_ns();
    for i in 0..120 {
        sched.on_scene_update();
        let t = (i + 1) as u64 * period;
        assert!(sched.on_timer_tick_at(t));
        sched.on_render_complete_at(t + period / 2);
    }
    assert_eq!(sched.render_count, 120, "120 renders at 120fps");
}

/// Arbitrary cadence (e.g. 75fps) works.
#[test]
fn configurable_cadence_arbitrary() {
    let sched = FrameScheduler::new(75);
    assert_eq!(sched.period_ns(), 1_000_000_000 / 75);
}

/// set_cadence changes the period.
#[test]
fn set_cadence_updates_period() {
    let mut sched = FrameScheduler::new(60);
    assert_eq!(sched.period_ns(), frame_scheduler::frame_period_ns(60));

    sched.set_cadence(30);
    assert_eq!(sched.period_ns(), frame_scheduler::frame_period_ns(30));

    sched.set_cadence(120);
    assert_eq!(sched.period_ns(), frame_scheduler::frame_period_ns(120));
}

// ── VAL-FRAME-006: Idle-to-active wakeup — immediate render ────────

/// After 500ms idle (30 ticks at 60fps), a scene update renders
/// immediately if the timer hasn't fired within the last half-period.
#[test]
fn idle_to_active_immediate_wakeup() {
    let period = frame_scheduler::frame_period_ns(60);
    let mut sched = FrameScheduler::new(60);

    // Simulate idle: 30 timer ticks with no scene updates.
    for i in 0..30 {
        sched.on_timer_tick_at((i + 1) as u64 * period);
    }
    let last_tick_time = 30 * period;
    assert_eq!(sched.render_count, 0);

    // Scene update arrives well after the last tick (>half period).
    let update_time = last_tick_time + period; // one full period after last tick
    assert!(
        sched.should_render_immediately(update_time),
        "should render immediately when timer hasn't fired recently"
    );

    sched.on_scene_update();
    // Compositor renders immediately (doesn't wait for next tick).
    sched.on_render_complete_at(update_time + 1_000_000);

    assert_eq!(sched.render_count, 1);
}

/// Scene update shortly after a timer tick does NOT render immediately.
#[test]
fn no_immediate_render_right_after_tick() {
    let period = frame_scheduler::frame_period_ns(60);
    let mut sched = FrameScheduler::new(60);

    // Timer tick just fired.
    sched.on_timer_tick_at(period);

    // Scene update arrives shortly after (within half-period).
    let update_time = period + period / 4;
    assert!(
        !sched.should_render_immediately(update_time),
        "should NOT render immediately when timer just fired"
    );
}

// ── VAL-FRAME-010: Clock-only updates between idle periods ──────────

/// With no user input for 3 seconds, exactly 3 renders occur
/// (one per clock tick). Zero renders between ticks.
#[test]
fn clock_only_3_renders_in_3_seconds() {
    let period = frame_scheduler::frame_period_ns(60);
    let mut sched = FrameScheduler::new(60);
    let ticks_per_second = 60;

    // Simulate 3 seconds: each second, the clock produces one scene update.
    for second in 0..3 {
        // The clock update arrives once per second.
        let update_tick = second * ticks_per_second;

        for tick in 0..ticks_per_second {
            let abs_tick = second * ticks_per_second + tick;
            let t = (abs_tick + 1) as u64 * period;

            if tick == update_tick - update_tick {
                // Clock fires at the start of each second.
                sched.on_scene_update();
            }

            if sched.on_timer_tick_at(t) {
                sched.on_render_complete_at(t + period / 4);
            }
        }
    }

    assert_eq!(sched.render_count, 3, "exactly 3 renders for 3 clock ticks");
}

// ── VAL-FRAME-009: Frame scheduler coexists with damage tracking ────

/// With frame scheduler active, a cursor-only update produces dirty rects
/// (not full-screen) in the GPU present payload.
///
/// NOTE: This test validates the contract at the scheduler level.
/// The actual dirty rect production is tested in scene_render.rs.
/// Here we verify the scheduler doesn't interfere with partial updates.
#[test]
fn scheduler_preserves_partial_update_path() {
    let period = frame_scheduler::frame_period_ns(60);
    let mut sched = FrameScheduler::new(60);

    // Single scene update (e.g., cursor blink).
    sched.on_scene_update();

    // Timer fires → should render.
    assert!(sched.on_timer_tick_at(period));

    // Compositor renders and presents (partial update with dirty rects).
    // The scheduler just says "render" or "don't render" — it doesn't
    // control the damage tracking path. After render, mark complete.
    sched.on_render_complete_at(period + period / 2);

    assert_eq!(sched.render_count, 1);
    assert_eq!(sched.gpu_present_count, 1);
}

// ── Timestamp-aware tests for backward compatibility ────────────────

/// on_timer_tick still works (delegates to on_timer_tick_at with now=0).
#[test]
fn timer_tick_without_timestamp_still_works() {
    let mut sched = FrameScheduler::new(60);
    sched.on_scene_update();
    assert!(sched.on_timer_tick());
    sched.on_render_complete();
    assert_eq!(sched.render_count, 1);
}

/// on_render_complete still works (delegates to on_render_complete_at with now=0).
#[test]
fn render_complete_without_timestamp_still_works() {
    let mut sched = FrameScheduler::new(60);
    sched.on_scene_update();
    sched.on_timer_tick();
    sched.on_render_complete();
    assert_eq!(sched.render_count, 1);
    assert_eq!(sched.gpu_present_count, 1);
}

/// Overrun counter tracks skipped frames.
#[test]
fn overrun_skip_counter() {
    let period = frame_scheduler::frame_period_ns(60);
    let mut sched = FrameScheduler::new(60);

    // Normal frame.
    sched.on_scene_update();
    sched.on_timer_tick_at(period);
    sched.on_render_complete_at(period + period / 2);

    // Overrun frame: render takes 3x period.
    sched.on_scene_update();
    sched.on_timer_tick_at(2 * period);
    sched.on_render_complete_at(2 * period + 3 * period);

    // Next tick is overdue.
    sched.on_scene_update();
    sched.on_timer_tick_at(3 * period);

    assert!(
        sched.overrun_skip_count >= 1,
        "should have at least one overrun skip"
    );

    // Reset clears the counter.
    sched.reset_counters();
    assert_eq!(sched.overrun_skip_count, 0);
}
