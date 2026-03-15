//! Frame scheduler — configurable-cadence rendering with event coalescing.
//!
//! Replaces the compositor's immediate-render-on-every-event pattern with
//! a timer-driven cadence that provides:
//!
//! - **Event coalescing:** Multiple scene updates between ticks produce one render.
//! - **Idle optimization:** No renders when nothing changed.
//! - **Configurable cadence:** Default 60fps, adjustable.
//!
//! The frame scheduler is a pure state machine. The compositor drives it:
//!
//! 1. Call `on_scene_update()` when core signals a scene change → sets dirty flag.
//! 2. Call `on_timer_tick()` when the frame timer fires → returns whether to render.
//! 3. Call `on_render_complete()` after rendering + presenting.
//!
//! The compositor creates and recreates one-shot kernel timers at the configured
//! cadence. The scheduler itself has no knowledge of syscalls or handles.

/// Nanoseconds per frame at a given FPS. Computed as `1_000_000_000 / fps`.
pub const fn frame_period_ns(fps: u32) -> u64 {
    if fps == 0 {
        return 16_666_667; // fallback to 60fps
    }
    1_000_000_000 / fps as u64
}

/// Frame scheduling state machine.
///
/// Tracks whether the scene is dirty (needs rendering) and counts
/// timer ticks, renders, and GPU presents for instrumentation.
pub struct FrameScheduler {
    /// Whether the scene has been updated since the last render.
    dirty: bool,
    /// Configured frame period in nanoseconds.
    period_ns: u64,
    /// Number of frame timer ticks observed.
    pub tick_count: u32,
    /// Number of renders performed (dirty tick → render).
    pub render_count: u32,
    /// Number of GPU present commands sent.
    pub gpu_present_count: u32,
    /// Number of timer ticks that were skipped (not dirty).
    pub idle_skip_count: u32,
}

impl FrameScheduler {
    /// Create a new frame scheduler at the given FPS (default 60).
    pub fn new(fps: u32) -> Self {
        Self {
            dirty: false,
            period_ns: frame_period_ns(fps),
            tick_count: 0,
            render_count: 0,
            gpu_present_count: 0,
            idle_skip_count: 0,
        }
    }

    /// The configured frame period in nanoseconds.
    pub fn period_ns(&self) -> u64 {
        self.period_ns
    }

    /// Mark the scene as dirty (a scene update arrived from core).
    ///
    /// The compositor calls this when it receives a signal on the core
    /// channel. The dirty flag persists until the next render clears it.
    pub fn on_scene_update(&mut self) {
        self.dirty = true;
    }

    /// Called when the frame timer fires.
    ///
    /// Returns `true` if the compositor should render (scene is dirty),
    /// `false` if it should skip (nothing changed — idle optimization).
    pub fn on_timer_tick(&mut self) -> bool {
        self.tick_count += 1;
        if self.dirty {
            true
        } else {
            self.idle_skip_count += 1;
            false
        }
    }

    /// Called after the compositor has rendered and presented a frame.
    ///
    /// Clears the dirty flag and increments render/present counters.
    pub fn on_render_complete(&mut self) {
        self.dirty = false;
        self.render_count += 1;
        self.gpu_present_count += 1;
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
    }
}
