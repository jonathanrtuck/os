//! Cursor blink state machine.
//!
//! Four-phase cycle: visible hold -> fade out -> hidden hold -> fade in.
//! The state machine is advanced each frame by `advance_blink()` and
//! reset to fully-visible on user input by `reset_blink()`.

use super::CoreState;

/// Phase of the cursor blink cycle: visible hold -> fade out -> hidden hold -> fade in.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum BlinkPhase {
    /// Cursor fully visible for 500ms.
    VisibleHold,
    /// Fading from opacity 255->0 over 150ms.
    FadeOut,
    /// Cursor fully hidden for 300ms.
    HiddenHold,
    /// Fading from opacity 0->255 over 150ms.
    FadeIn,
}

/// Duration of each blink phase in milliseconds.
pub(crate) const BLINK_VISIBLE_MS: u64 = 500;
pub(crate) const BLINK_FADE_OUT_MS: u64 = 150;
pub(crate) const BLINK_HIDDEN_MS: u64 = 300;
pub(crate) const BLINK_FADE_IN_MS: u64 = 150;

/// Advance the blink state machine. Returns `true` if `cursor_opacity` changed.
pub(crate) fn advance_blink(state: &mut CoreState, now_ms: u64) -> bool {
    let elapsed = now_ms.saturating_sub(state.blink_phase_start_ms);
    let mut changed = false;

    match state.blink_phase {
        BlinkPhase::VisibleHold => {
            state.cursor_opacity = 255;
            if elapsed >= BLINK_VISIBLE_MS {
                state.cursor_blink_id = state
                    .timeline
                    .start(255.0, 0.0, 150, animation::Easing::EaseInOut, now_ms)
                    .ok();
                state.blink_phase = BlinkPhase::FadeOut;
                state.blink_phase_start_ms = now_ms;
                changed = true;
            }
        }
        BlinkPhase::FadeOut => {
            if let Some(id) = state.cursor_blink_id {
                let new_opacity = if state.timeline.is_active(id) {
                    state.timeline.value(id) as u8
                } else {
                    0
                };
                if new_opacity != state.cursor_opacity {
                    state.cursor_opacity = new_opacity;
                    changed = true;
                }
            }
            if elapsed >= BLINK_FADE_OUT_MS {
                state.blink_phase = BlinkPhase::HiddenHold;
                state.blink_phase_start_ms = now_ms;
                state.cursor_opacity = 0;
                changed = true;
            }
        }
        BlinkPhase::HiddenHold => {
            state.cursor_opacity = 0;
            if elapsed >= BLINK_HIDDEN_MS {
                state.cursor_blink_id = state
                    .timeline
                    .start(0.0, 255.0, 150, animation::Easing::EaseInOut, now_ms)
                    .ok();
                state.blink_phase = BlinkPhase::FadeIn;
                state.blink_phase_start_ms = now_ms;
                changed = true;
            }
        }
        BlinkPhase::FadeIn => {
            if let Some(id) = state.cursor_blink_id {
                let new_opacity = if state.timeline.is_active(id) {
                    state.timeline.value(id) as u8
                } else {
                    255
                };
                if new_opacity != state.cursor_opacity {
                    state.cursor_opacity = new_opacity;
                    changed = true;
                }
            }
            if elapsed >= BLINK_FADE_IN_MS {
                state.blink_phase = BlinkPhase::VisibleHold;
                state.blink_phase_start_ms = now_ms;
                state.cursor_opacity = 255;
                changed = true;
            }
        }
    }
    changed
}

/// Reset blink to fully visible (called on user input).
pub(crate) fn reset_blink(state: &mut CoreState, now_ms: u64) {
    if let Some(id) = state.cursor_blink_id {
        state.timeline.cancel(id);
    }
    state.blink_phase = BlinkPhase::VisibleHold;
    state.blink_phase_start_ms = now_ms;
    state.cursor_opacity = 255;
}
