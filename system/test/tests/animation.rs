//! Host-side tests for the animation easing library.

extern crate animation;

use animation::{ease, Easing, Lerp, LerpColor, Spring, Transform2D};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// All 24 easing variants, including one `CubicBezier` sample.
fn all_easings() -> [Easing; 24] {
    [
        Easing::Linear,
        Easing::Ease,
        Easing::EaseIn,
        Easing::EaseOut,
        Easing::EaseInOut,
        Easing::CubicBezier(0.17, 0.67, 0.83, 0.67),
        Easing::EaseInQuad,
        Easing::EaseOutQuad,
        Easing::EaseInOutQuad,
        Easing::EaseInCubic,
        Easing::EaseOutCubic,
        Easing::EaseInOutCubic,
        Easing::EaseInExpo,
        Easing::EaseOutExpo,
        Easing::EaseInOutExpo,
        Easing::EaseInBack,
        Easing::EaseOutBack,
        Easing::EaseInOutBack,
        Easing::EaseInElastic,
        Easing::EaseOutElastic,
        Easing::EaseInBounce,
        Easing::EaseOutBounce,
        Easing::StepStart,
        Easing::StepEnd,
    ]
}

// ── Boundary tests ────────────────────────────────────────────────────────────

#[test]
fn all_easings_start_at_zero() {
    for easing in all_easings() {
        let y = ease(easing, 0.0);
        assert!(
            y.abs() < 0.01,
            "{:?} at t=0 returned {}, expected ~0.0",
            easing,
            y
        );
    }
}

#[test]
fn all_easings_end_at_one() {
    for easing in all_easings() {
        let y = ease(easing, 1.0);
        assert!(
            (y - 1.0).abs() < 0.01,
            "{:?} at t=1 returned {}, expected ~1.0",
            easing,
            y
        );
    }
}

#[test]
fn linear_is_identity() {
    for i in 0..=100 {
        let t = i as f32 / 100.0;
        let y = ease(Easing::Linear, t);
        assert!(
            (y - t).abs() < 1e-6,
            "Linear at t={} returned {}, expected {}",
            t,
            y,
            t
        );
    }
}

// ── Clamp tests ───────────────────────────────────────────────────────────────

#[test]
fn negative_t_clamps_to_zero() {
    for easing in all_easings() {
        let y = ease(easing, -0.5);
        // Compare against t=0 result, which we already verified is ~0.
        let y0 = ease(easing, 0.0);
        assert!(
            (y - y0).abs() < 1e-6,
            "{:?}: ease(-0.5)={} != ease(0.0)={}",
            easing,
            y,
            y0
        );
    }
}

#[test]
fn t_above_one_clamps_to_one() {
    for easing in all_easings() {
        let y = ease(easing, 2.0);
        let y1 = ease(easing, 1.0);
        assert!(
            (y - y1).abs() < 1e-6,
            "{:?}: ease(2.0)={} != ease(1.0)={}",
            easing,
            y,
            y1
        );
    }
}

// ── NaN safety ────────────────────────────────────────────────────────────────

#[test]
fn nan_input_returns_finite_value() {
    for easing in all_easings() {
        let y = ease(easing, f32::NAN);
        assert!(
            y.is_finite(),
            "{:?}: ease(NaN) returned non-finite {}",
            easing,
            y
        );
    }
}

// ── Monotonicity tests ────────────────────────────────────────────────────────

/// Easings that must be strictly non-decreasing on (0, 1) — no overshoot.
fn monotone_easings() -> [Easing; 14] {
    [
        Easing::Linear,
        Easing::Ease,
        Easing::EaseIn,
        Easing::EaseOut,
        Easing::EaseInOut,
        Easing::EaseInQuad,
        Easing::EaseOutQuad,
        Easing::EaseInOutQuad,
        Easing::EaseInCubic,
        Easing::EaseOutCubic,
        Easing::EaseInOutCubic,
        Easing::EaseInExpo,
        Easing::EaseOutExpo,
        Easing::EaseInOutExpo,
    ]
}

#[test]
fn monotone_easings_are_non_decreasing() {
    const SAMPLES: usize = 200;

    for easing in monotone_easings() {
        let mut prev = ease(easing, 0.0);

        for i in 1..=SAMPLES {
            let t = i as f32 / SAMPLES as f32;
            let y = ease(easing, t);

            assert!(
                y >= prev - 1e-5,
                "{:?} is not non-decreasing: ease({})={} < ease({})={}",
                easing,
                t,
                y,
                (i - 1) as f32 / SAMPLES as f32,
                prev
            );

            prev = y;
        }
    }
}

// ── Overshoot tests ───────────────────────────────────────────────────────────

#[test]
fn ease_in_back_undershoots_near_start() {
    // EaseInBack should dip below 0 somewhere in the early range.
    let min_y = (1..20)
        .map(|i| ease(Easing::EaseInBack, i as f32 / 100.0))
        .fold(f32::INFINITY, f32::min);

    assert!(
        min_y < 0.0,
        "EaseInBack should undershoot (go below 0) near t=0.2, min_y={}",
        min_y
    );
}

#[test]
fn ease_out_elastic_oscillates() {
    // EaseOutElastic should exceed 1.0 or go below 0.0 at some point.
    let oscillates = (1..99).any(|i| {
        let y = ease(Easing::EaseOutElastic, i as f32 / 100.0);
        y > 1.0 || y < 0.0
    });

    assert!(oscillates, "EaseOutElastic should oscillate outside [0, 1]");
}

#[test]
fn ease_out_back_overshoots_near_end() {
    // EaseOutBack should exceed 1.0 somewhere before the end.
    let max_y = (50..99)
        .map(|i| ease(Easing::EaseOutBack, i as f32 / 100.0))
        .fold(f32::NEG_INFINITY, f32::max);

    assert!(
        max_y > 1.0,
        "EaseOutBack should overshoot (go above 1) near t=0.9, max_y={}",
        max_y
    );
}

// ── CSS reference values ──────────────────────────────────────────────────────

#[test]
fn css_ease_at_half_matches_reference() {
    // CSS `ease` (0.25, 0.1, 0.25, 1.0) at t=0.5.
    // Reference value from Chrome DevTools: ≈ 0.8024.
    let y = ease(Easing::Ease, 0.5);
    assert!(
        (y - 0.8024).abs() < 0.005,
        "CSS ease at t=0.5: got {}, expected ~0.8024",
        y
    );
}

#[test]
fn css_ease_in_is_slow_at_start() {
    // EaseIn should be well below 0.5 at the midpoint.
    let y = ease(Easing::EaseIn, 0.5);
    assert!(y < 0.5, "EaseIn at t=0.5 should be < 0.5, got {}", y);
}

#[test]
fn css_ease_out_is_fast_at_start() {
    // EaseOut should be well above 0.5 at the midpoint.
    let y = ease(Easing::EaseOut, 0.5);
    assert!(y > 0.5, "EaseOut at t=0.5 should be > 0.5, got {}", y);
}

#[test]
fn css_ease_in_out_is_symmetric() {
    // EaseInOut should satisfy: ease(t) + ease(1-t) ≈ 1.
    for i in 1..50 {
        let t = i as f32 / 100.0;
        let a = ease(Easing::EaseInOut, t);
        let b = ease(Easing::EaseInOut, 1.0 - t);
        assert!(
            (a + b - 1.0).abs() < 0.01,
            "EaseInOut symmetry failed at t={}: {:.4} + {:.4} = {:.4}",
            t,
            a,
            b,
            a + b
        );
    }
}

// ── Polynomial shape tests ────────────────────────────────────────────────────

#[test]
fn ease_in_quad_matches_formula() {
    for i in 0..=10 {
        let t = i as f32 / 10.0;
        let expected = t * t;
        let got = ease(Easing::EaseInQuad, t);
        assert!(
            (got - expected).abs() < 1e-5,
            "EaseInQuad at t={}: got {}, expected {}",
            t,
            got,
            expected
        );
    }
}

#[test]
fn ease_in_cubic_matches_formula() {
    for i in 0..=10 {
        let t = i as f32 / 10.0;
        let expected = t * t * t;
        let got = ease(Easing::EaseInCubic, t);
        assert!(
            (got - expected).abs() < 1e-5,
            "EaseInCubic at t={}: got {}, expected {}",
            t,
            got,
            expected
        );
    }
}

#[test]
fn ease_in_out_quad_midpoint() {
    // At t=0.5, EaseInOutQuad should equal exactly 0.5.
    let y = ease(Easing::EaseInOutQuad, 0.5);
    assert!((y - 0.5).abs() < 1e-5, "EaseInOutQuad at 0.5: got {}", y);
}

#[test]
fn ease_in_out_cubic_midpoint() {
    // At t=0.5, EaseInOutCubic should equal exactly 0.5.
    let y = ease(Easing::EaseInOutCubic, 0.5);
    assert!((y - 0.5).abs() < 1e-5, "EaseInOutCubic at 0.5: got {}", y);
}

// ── Exponential shape tests ───────────────────────────────────────────────────

#[test]
fn ease_in_expo_exact_endpoints() {
    assert_eq!(ease(Easing::EaseInExpo, 0.0), 0.0);
    assert!(
        (ease(Easing::EaseInExpo, 1.0) - 1.0).abs() < 0.001,
        "EaseInExpo at t=1"
    );
}

#[test]
fn ease_out_expo_exact_endpoints() {
    assert!(
        (ease(Easing::EaseOutExpo, 0.0)).abs() < 0.001,
        "EaseOutExpo at t=0"
    );
    assert_eq!(ease(Easing::EaseOutExpo, 1.0), 1.0);
}

#[test]
fn ease_in_out_expo_exact_endpoints() {
    assert_eq!(ease(Easing::EaseInOutExpo, 0.0), 0.0);
    assert_eq!(ease(Easing::EaseInOutExpo, 1.0), 1.0);
}

#[test]
fn ease_in_expo_is_convex() {
    // Second half should advance more than first half.
    let first_half = ease(Easing::EaseInExpo, 0.5) - ease(Easing::EaseInExpo, 0.0);
    let second_half = ease(Easing::EaseInExpo, 1.0) - ease(Easing::EaseInExpo, 0.5);
    assert!(
        second_half > first_half,
        "EaseInExpo: second half ({}) should advance more than first half ({})",
        second_half,
        first_half
    );
}

// ── Step tests ────────────────────────────────────────────────────────────────

#[test]
fn step_start_jumps_at_t_gt_0() {
    assert_eq!(ease(Easing::StepStart, 0.0), 0.0, "StepStart at t=0");
    assert_eq!(ease(Easing::StepStart, 0.01), 1.0, "StepStart at t=0.01");
    assert_eq!(ease(Easing::StepStart, 0.5), 1.0, "StepStart at t=0.5");
    assert_eq!(ease(Easing::StepStart, 1.0), 1.0, "StepStart at t=1.0");
}

#[test]
fn step_end_jumps_at_t_eq_1() {
    assert_eq!(ease(Easing::StepEnd, 0.0), 0.0, "StepEnd at t=0");
    assert_eq!(ease(Easing::StepEnd, 0.5), 0.0, "StepEnd at t=0.5");
    assert_eq!(ease(Easing::StepEnd, 0.99), 0.0, "StepEnd at t=0.99");
    assert_eq!(ease(Easing::StepEnd, 1.0), 1.0, "StepEnd at t=1.0");
}

// ── Bounce tests ──────────────────────────────────────────────────────────────

#[test]
fn ease_out_bounce_has_multiple_bounces() {
    // The output should reach above 0.9 at least three times (three bounces
    // plus the final settlement).
    let peaks: usize = (1..99)
        .filter(|&i| {
            let t = i as f32 / 100.0;
            let prev = ease(Easing::EaseOutBounce, (i - 1) as f32 / 100.0);
            let curr = ease(Easing::EaseOutBounce, t);
            let next = ease(Easing::EaseOutBounce, (i + 1) as f32 / 100.0);
            curr > prev && curr > next && curr > 0.5
        })
        .count();

    assert!(
        peaks >= 2,
        "EaseOutBounce should have at least 2 detectable peaks above 0.5, found {}",
        peaks
    );
}

#[test]
fn ease_in_bounce_mirrors_ease_out_bounce() {
    // ease_in(t) = 1 - ease_out(1 - t)
    for i in 0..=100 {
        let t = i as f32 / 100.0;
        let in_val = ease(Easing::EaseInBounce, t);
        let out_mirror = 1.0 - ease(Easing::EaseOutBounce, 1.0 - t);
        assert!(
            (in_val - out_mirror).abs() < 1e-5,
            "EaseInBounce({}) = {}, mirror = {}",
            t,
            in_val,
            out_mirror
        );
    }
}

#[test]
fn ease_out_bounce_stays_in_unit_interval() {
    for i in 0..=100 {
        let t = i as f32 / 100.0;
        let y = ease(Easing::EaseOutBounce, t);
        assert!(
            y >= -0.001 && y <= 1.001,
            "EaseOutBounce({}) = {} is outside [0, 1]",
            t,
            y
        );
    }
}

// ── CubicBezier tests ─────────────────────────────────────────────────────────

#[test]
fn cubic_bezier_identity_is_linear() {
    // CubicBezier(0,0,1,1) should behave identically to Linear.
    for i in 0..=20 {
        let t = i as f32 / 20.0;
        let linear = ease(Easing::Linear, t);
        let cb = ease(Easing::CubicBezier(0.0, 0.0, 1.0, 1.0), t);
        assert!(
            (cb - linear).abs() < 0.002,
            "CubicBezier(0,0,1,1) at t={}: got {}, expected {}",
            t,
            cb,
            linear
        );
    }
}

#[test]
fn cubic_bezier_custom_endpoints() {
    // Any valid CubicBezier must start at 0 and end at 1.
    let cb = Easing::CubicBezier(0.3, 0.8, 0.7, 0.2);
    assert!((ease(cb, 0.0)).abs() < 0.01);
    assert!((ease(cb, 1.0) - 1.0).abs() < 0.01);
}

// ── Elastic shape tests ───────────────────────────────────────────────────────

#[test]
fn ease_in_elastic_oscillates_then_arrives() {
    // Elastic-in oscillates (goes negative) through most of its range,
    // then arrives at 1.0 by t=1.0.
    //
    // At t=0.25 the amplitude of oscillation should be small (< 0.1).
    let y_at_quarter = ease(Easing::EaseInElastic, 0.25);
    assert!(
        y_at_quarter.abs() < 0.1,
        "EaseInElastic(0.25) = {} — amplitude should be small",
        y_at_quarter
    );

    // The curve oscillates — it will go negative somewhere in [0.5, 1.0).
    let goes_negative = (50..99u32).any(|i| ease(Easing::EaseInElastic, i as f32 / 100.0) < -0.1);
    assert!(
        goes_negative,
        "EaseInElastic should have negative oscillations before t=1"
    );

    // Despite oscillations, it must end exactly at 1.0.
    assert!(
        (ease(Easing::EaseInElastic, 1.0) - 1.0).abs() < 0.001,
        "EaseInElastic must end at 1.0"
    );
}

#[test]
fn ease_out_elastic_settles_at_one() {
    // EaseOutElastic should end exactly at 1.
    assert!(
        (ease(Easing::EaseOutElastic, 1.0) - 1.0).abs() < 0.001,
        "EaseOutElastic at t=1 should be 1.0"
    );
}

// ── Back shape tests ──────────────────────────────────────────────────────────

#[test]
fn ease_in_back_goes_negative_before_positive() {
    // Should dip below zero before rising.
    let dips = (1..50u32).any(|i| ease(Easing::EaseInBack, i as f32 / 100.0) < 0.0);
    assert!(dips, "EaseInBack should dip below 0");
}

#[test]
fn ease_out_back_overshoots_before_settling() {
    // Should exceed 1.0 before settling at exactly 1.0 at t=1.
    let overshoots = (50..99u32).any(|i| ease(Easing::EaseOutBack, i as f32 / 100.0) > 1.0);
    assert!(overshoots, "EaseOutBack should overshoot above 1.0");
}

// ── Spring physics tests ──────────────────────────────────────────────────────

#[test]
fn spring_default_settles_at_target() {
    let mut s = Spring::default_preset(1.0);
    for _ in 0..120 {
        s.tick(1.0 / 60.0);
    }
    assert!(
        (s.value() - 1.0).abs() < 0.001,
        "spring did not reach target: value = {}",
        s.value()
    );
    assert!(s.settled(), "spring is not settled after 2 seconds");
}

#[test]
fn spring_snappy_settles_faster_than_gentle() {
    let mut snappy = Spring::snappy(1.0);
    let mut gentle = Spring::gentle(1.0);
    let mut snappy_settled_at = None;
    let mut gentle_settled_at = None;

    for i in 0..300 {
        snappy.tick(1.0 / 60.0);
        gentle.tick(1.0 / 60.0);
        if snappy.settled() && snappy_settled_at.is_none() {
            snappy_settled_at = Some(i);
        }
        if gentle.settled() && gentle_settled_at.is_none() {
            gentle_settled_at = Some(i);
        }
    }

    let snappy_frame = snappy_settled_at.expect("snappy spring never settled");
    let gentle_frame = gentle_settled_at.expect("gentle spring never settled");
    assert!(
        snappy_frame < gentle_frame,
        "snappy settled at frame {} but gentle settled at frame {} — snappy should be faster",
        snappy_frame,
        gentle_frame
    );
}

#[test]
fn spring_bouncy_overshoots_target() {
    let mut s = Spring::bouncy(1.0);
    let mut max_value = 0.0f32;

    for _ in 0..120 {
        s.tick(1.0 / 60.0);
        if s.value() > max_value {
            max_value = s.value();
        }
    }

    assert!(
        max_value > 1.0,
        "bouncy spring should overshoot target 1.0, max_value = {}",
        max_value
    );
}

#[test]
fn spring_retarget_changes_destination() {
    let mut s = Spring::default_preset(1.0);
    for _ in 0..30 {
        s.tick(1.0 / 60.0);
    }
    s.set_target(2.0);
    for _ in 0..120 {
        s.tick(1.0 / 60.0);
    }
    assert!(
        (s.value() - 2.0).abs() < 0.001,
        "spring did not reach new target 2.0 after retarget: value = {}",
        s.value()
    );
}

#[test]
fn spring_zero_dt_is_noop() {
    let mut s = Spring::default_preset(1.0);
    let v_before = s.value();
    s.tick(0.0);
    assert_eq!(
        s.value(),
        v_before,
        "zero dt should not change value: before = {}, after = {}",
        v_before,
        s.value()
    );
}

#[test]
fn spring_negative_dt_is_noop() {
    let mut s = Spring::default_preset(1.0);
    // Advance a few frames so there is velocity to check.
    s.tick(1.0 / 60.0);
    s.tick(1.0 / 60.0);
    let val_before = s.value();
    let vel_before = s.velocity();
    s.tick(-0.016);
    assert_eq!(s.value(), val_before, "negative dt should not change value");
    assert_eq!(
        s.velocity(),
        vel_before,
        "negative dt should not change velocity"
    );
}

#[test]
fn spring_non_zero_initial_value() {
    // Start the spring with value already at target — it should stay settled.
    let s = Spring::new(1.0, 300.0, 20.0, 1.0);
    // Spring starts at value=0.0 with target=1.0 — not settled.
    assert!(
        !s.settled(),
        "spring starting at 0 with target 1 should not be settled"
    );

    // A spring created with target == initial value (both 0) should be settled
    // immediately.
    let s_at_rest = Spring::new(0.0, 300.0, 20.0, 1.0);
    assert!(
        s_at_rest.settled(),
        "spring with target == value == 0 should be settled immediately"
    );
}

#[test]
fn spring_custom_settle_threshold() {
    let mut s = Spring::default_preset(1.0);
    // Use a very large threshold — the spring should settle almost immediately.
    s.set_settle_threshold(10.0);
    s.tick(1.0 / 60.0);
    assert!(
        s.settled(),
        "with threshold=10 the spring should be settled after one tick"
    );

    // Use a tiny threshold — the spring should take many frames.
    let mut s2 = Spring::default_preset(1.0);
    s2.set_settle_threshold(0.00001);
    // After only 30 frames (~0.5s) it should NOT yet be settled.
    for _ in 0..30 {
        s2.tick(1.0 / 60.0);
    }
    assert!(
        !s2.settled(),
        "with threshold=0.00001 the spring should not be settled after 30 frames"
    );
}

// ── Lerp trait ────────────────────────────────────────────────────────────────

#[test]
fn lerp_f32_midpoint() {
    assert_eq!(f32::lerp(0.0, 10.0, 0.5), 5.0);
}

#[test]
fn lerp_f32_boundaries() {
    assert_eq!(f32::lerp(0.0, 10.0, 0.0), 0.0);
    assert_eq!(f32::lerp(0.0, 10.0, 1.0), 10.0);
}

#[test]
fn lerp_i32() {
    assert_eq!(i32::lerp(0, 100, 0.5), 50);
    assert_eq!(i32::lerp(-10, 10, 0.5), 0);
}

#[test]
fn lerp_u8() {
    assert_eq!(u8::lerp(0, 255, 0.5), 128);
    assert_eq!(u8::lerp(0, 255, 0.0), 0);
    assert_eq!(u8::lerp(0, 255, 1.0), 255);
}

// ── Gamma-correct color interpolation ────────────────────────────────────────

#[test]
fn lerp_color_gamma_correct() {
    // Black to white at midpoint: in linear space 0.5 maps to sRGB ~188
    // (NOT 128, which is what naive byte lerp produces)
    let mid = LerpColor::lerp_srgb([0, 0, 0, 255], [255, 255, 255, 255], 0.5);
    assert!(
        mid[0] > 160 && mid[0] < 210,
        "Gamma-correct midpoint should be ~188, got {}",
        mid[0]
    );
    assert_eq!(mid[3], 255, "Alpha at t=0.5 of 255,255 should be 255");
}

#[test]
fn lerp_color_boundaries() {
    let a = [10, 20, 30, 40];
    let b = [200, 210, 220, 230];
    let at_zero = LerpColor::lerp_srgb(a, b, 0.0);
    let at_one = LerpColor::lerp_srgb(a, b, 1.0);
    assert_eq!(at_zero, a);
    assert_eq!(at_one, b);
}

#[test]
fn lerp_color_alpha_is_linear() {
    // Alpha is NOT gamma-corrected — it's linear.
    let mid = LerpColor::lerp_srgb([0, 0, 0, 0], [0, 0, 0, 200], 0.5);
    assert_eq!(mid[3], 100, "Alpha should be linearly interpolated");
}

// ── Transform2D lerp ─────────────────────────────────────────────────────────

#[test]
fn lerp_transform2d_midpoint() {
    let a = Transform2D::identity();
    let b = Transform2D {
        a: 2.0,
        b: 0.0,
        c: 0.0,
        d: 2.0,
        tx: 100.0,
        ty: 200.0,
    };
    let mid = Transform2D::lerp(a, b, 0.5);
    assert!((mid.a - 1.5).abs() < 0.001);
    assert!((mid.tx - 50.0).abs() < 0.001);
    assert!((mid.ty - 100.0).abs() < 0.001);
}

#[test]
fn lerp_transform2d_identity_at_zero() {
    let a = Transform2D::identity();
    let b = Transform2D {
        a: 3.0,
        b: 1.0,
        c: 1.0,
        d: 3.0,
        tx: 50.0,
        ty: 50.0,
    };
    let result = Transform2D::lerp(a, b, 0.0);
    assert_eq!(result, a);
}

// ── General quality checks ────────────────────────────────────────────────────

#[test]
fn all_easings_return_finite_values_across_range() {
    for easing in all_easings() {
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            let y = ease(easing, t);
            assert!(
                y.is_finite(),
                "{:?} at t={} returned non-finite {}",
                easing,
                t,
                y
            );
        }
    }
}

#[test]
fn all_easings_handle_exact_midpoint() {
    // Every easing must not crash or produce NaN at t=0.5.
    for easing in all_easings() {
        let y = ease(easing, 0.5);
        assert!(
            y.is_finite(),
            "{:?} at t=0.5 returned non-finite {}",
            easing,
            y
        );
    }
}
