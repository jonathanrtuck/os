//! Tests for the compositor's frame scheduler state machine.
//!
//! Validates event coalescing, idle optimization, configurable cadence,
//! and render/present counting. The frame scheduler is a pure state machine
//! that can be tested without kernel syscalls.

#[path = "../../services/compositor/frame_scheduler.rs"]
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
    assert_eq!(ns, 16_666_667);
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
    assert!(sched.on_timer_tick(), "should render on first tick after update");
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
