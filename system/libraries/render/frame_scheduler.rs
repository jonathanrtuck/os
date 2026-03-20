//! Frame scheduler — configurable-cadence rendering with event coalescing.
//!
//! Replaces the compositor's immediate-render-on-every-event pattern with
//! a timer-driven cadence that provides:
//!
//! - **Event coalescing:** Multiple scene updates between ticks produce one render.
//! - **Idle optimization:** No renders when nothing changed.
//! - **Configurable cadence:** Default 60fps, adjustable at runtime.
//! - **Frame budgeting:** If rendering takes >2× frame period, skip the missed
//!   deadline — no back-to-back "catch-up" renders.
//! - **Idle-to-active wakeup:** When a scene update arrives after an idle period
//!   and the timer hasn't fired recently, render immediately.
//!
//! The frame scheduler is a pure state machine. The compositor drives it:
//!
//! 1. Call `on_scene_update()` when core signals a scene change → sets dirty flag.
//! 2. Call `on_timer_tick()` / `on_timer_tick_at(now)` when the frame timer fires
//!    → returns whether to render.
//! 3. Call `on_render_complete()` / `on_render_complete_at(now)` after rendering.
//! 4. Call `should_render_immediately(now)` when a scene update arrives to check
//!    if an idle-to-active immediate render is warranted.
//!
//! The compositor creates and recreates one-shot kernel timers at the configured
//! cadence. The scheduler itself has no knowledge of syscalls or handles.

/// Nanoseconds per frame at a given FPS. Computed as `1_000_000_000 / fps`.
pub const fn frame_period_ns(fps: u32) -> u64 {
    if fps == 0 {
        return 1_000_000_000 / 60; // fallback to 60fps
    }
    1_000_000_000 / fps as u64
}

/// Frame scheduling state machine.
///
/// Tracks whether the scene is dirty (needs rendering), timestamps for
/// frame budgeting and idle-to-active wakeup, and counts timer ticks,
/// renders, and GPU presents for instrumentation.
pub struct FrameScheduler {
    /// Whether the scene has been updated since the last render.
    dirty: bool,
    /// Configured frame period in nanoseconds.
    period_ns: u64,
    /// Timestamp (ns) of the last timer tick.
    last_tick_ns: u64,
    /// Timestamp (ns) when the last render completed.
    last_render_end_ns: u64,
    /// Number of frame timer ticks observed.
    pub tick_count: u32,
    /// Number of renders performed (dirty tick → render).
    pub render_count: u32,
    /// Number of GPU present commands sent.
    pub gpu_present_count: u32,
    /// Number of timer ticks that were skipped (not dirty).
    pub idle_skip_count: u32,
    /// Number of timer ticks skipped due to frame budget overrun.
    pub overrun_skip_count: u32,
}

impl FrameScheduler {
    /// Create a new frame scheduler at the given FPS (default 60).
    pub fn new(fps: u32) -> Self {
        Self {
            dirty: false,
            period_ns: frame_period_ns(fps),
            last_tick_ns: 0,
            last_render_end_ns: 0,
            tick_count: 0,
            render_count: 0,
            gpu_present_count: 0,
            idle_skip_count: 0,
            overrun_skip_count: 0,
        }
    }

    /// The configured frame period in nanoseconds.
    pub fn period_ns(&self) -> u64 {
        self.period_ns
    }

    /// Change the frame rate at runtime. Updates the period used for
    /// budgeting and idle-wakeup calculations. The compositor must
    /// also recreate the kernel timer at the new period.
    pub fn set_cadence(&mut self, fps: u32) {
        self.period_ns = frame_period_ns(fps);
    }

    /// Mark the scene as dirty (a scene update arrived from core).
    ///
    /// The compositor calls this when it receives a signal on the core
    /// channel. The dirty flag persists until the next render clears it.
    pub fn on_scene_update(&mut self) {
        self.dirty = true;
    }

    /// Called when the frame timer fires (without timestamp).
    ///
    /// Returns `true` if the compositor should render (scene is dirty),
    /// `false` if it should skip (nothing changed — idle optimization).
    pub fn on_timer_tick(&mut self) -> bool {
        self.on_timer_tick_at(0)
    }

    /// Called when the frame timer fires, with the current timestamp (ns).
    ///
    /// Returns `true` if the compositor should render, `false` to skip.
    /// Handles both idle optimization (not dirty) and frame budgeting
    /// (skip ticks that arrive before the overrun window closes).
    pub fn on_timer_tick_at(&mut self, now: u64) -> bool {
        self.tick_count = self.tick_count.wrapping_add(1);
        self.last_tick_ns = now;

        if !self.dirty {
            self.idle_skip_count = self.idle_skip_count.wrapping_add(1);
            return false;
        }

        // Frame budgeting: if the last render ended after this tick's
        // expected start time, the tick is overdue — skip it to avoid
        // back-to-back catch-up renders.
        if now > 0 && self.last_render_end_ns > now {
            self.overrun_skip_count = self.overrun_skip_count.wrapping_add(1);
            return false;
        }

        true
    }

    /// Called after the compositor has rendered and presented a frame
    /// (without timestamp).
    ///
    /// Clears the dirty flag and increments render/present counters.
    pub fn on_render_complete(&mut self) {
        self.on_render_complete_at(0);
    }

    /// Called after the compositor has rendered and presented a frame,
    /// with the current timestamp (ns).
    pub fn on_render_complete_at(&mut self, now: u64) {
        self.dirty = false;
        self.render_count = self.render_count.wrapping_add(1);
        self.gpu_present_count = self.gpu_present_count.wrapping_add(1);
        self.last_render_end_ns = now;
    }

    /// Check whether the compositor should render immediately on a
    /// scene update (idle-to-active wakeup).
    ///
    /// Returns `true` if the last timer tick was more than half a period
    /// ago, meaning we're far from the next tick and the user would
    /// perceive noticeable latency waiting for it.
    pub fn should_render_immediately(&self, now: u64) -> bool {
        if now == 0 || self.last_tick_ns == 0 {
            return false;
        }
        let elapsed = now.saturating_sub(self.last_tick_ns);
        elapsed > self.period_ns / 2
    }

    /// Whether the scene is currently dirty (updated since last render).
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Reset all counters (for testing / instrumentation periods).
    pub fn reset_counters(&mut self) {
        self.tick_count = 0;
        self.render_count = 0;
        self.gpu_present_count = 0;
        self.idle_skip_count = 0;
        self.overrun_skip_count = 0;
    }
}
