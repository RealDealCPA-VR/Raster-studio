//! Durations and easing curves.
//!
//! Four durations only. A UI that animates at a dozen different speeds feels
//! unowned; the discipline is to pick the shortest one that still reads.

/// Named animation durations.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Motion {
    /// Hover and press feedback. Must not be perceptible as an animation.
    Micro,
    /// Toggles, checkmarks, segment slides.
    Quick,
    /// Panels opening, popovers appearing.
    Standard,
    /// Full-window transitions.
    Slow,
}

impl Motion {
    /// Every duration, ascending.
    pub const ALL: &'static [Motion] = &[Self::Micro, Self::Quick, Self::Standard, Self::Slow];

    /// Duration in milliseconds.
    pub const fn millis(self) -> u32 {
        match self {
            Self::Micro => 90,
            Self::Quick => 150,
            Self::Standard => 250,
            Self::Slow => 400,
        }
    }

    /// Duration in seconds, which is the unit egui's animation helpers take.
    pub fn secs(self) -> f32 {
        self.millis() as f32 / 1000.0
    }
}

/// A cubic Bézier easing curve with endpoints pinned at (0,0) and (1,1).
///
/// Control point x coordinates are clamped to 0..=1 on construction, which is
/// what keeps `x(s)` monotonic and therefore invertible.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Easing {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

impl Easing {
    /// No easing.
    pub const LINEAR: Self = Self {
        x1: 0.0,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    };

    /// The default: leaves quickly, settles slowly. Use unless another curve
    /// is clearly indicated.
    pub const STANDARD: Self = Self {
        x1: 0.4,
        y1: 0.0,
        x2: 0.2,
        y2: 1.0,
    };

    /// For things entering the screen: fast in, gentle stop.
    pub const DECELERATE: Self = Self {
        x1: 0.0,
        y1: 0.0,
        x2: 0.2,
        y2: 1.0,
    };

    /// For things leaving the screen: gentle start, fast out.
    pub const ACCELERATE: Self = Self {
        x1: 0.4,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    };

    /// Long travel that must still feel crisp: nearly all the distance is
    /// covered in the first third of the duration.
    pub const EMPHASIZED: Self = Self {
        x1: 0.2,
        y1: 0.0,
        x2: 0.0,
        y2: 1.0,
    };

    /// Every named curve.
    pub const ALL: &'static [Easing] = &[
        Self::LINEAR,
        Self::STANDARD,
        Self::DECELERATE,
        Self::ACCELERATE,
        Self::EMPHASIZED,
    ];

    /// A custom curve. `x1`/`x2` are clamped to 0..=1; `y` values are free, so
    /// overshoot curves are expressible.
    pub fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self {
            x1: x1.clamp(0.0, 1.0),
            y1,
            x2: x2.clamp(0.0, 1.0),
            y2,
        }
    }

    /// The four control-point coordinates, as a CSS `cubic-bezier()` would
    /// order them.
    pub const fn control_points(&self) -> [f32; 4] {
        [self.x1, self.y1, self.x2, self.y2]
    }

    /// Eased progress for a normalized time `t` in 0..=1.
    ///
    /// `eval(0.0) == 0.0` and `eval(1.0) == 1.0` exactly; `t` outside the range
    /// is clamped.
    pub fn eval(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        bezier_axis(self.y1, self.y2, self.solve_for_s(t))
    }

    /// Invert `x(s) = t` by bisection.
    ///
    /// `x` is monotonic non-decreasing because both x control points are
    /// clamped into 0..=1, so bisection always converges. 40 halvings put the
    /// answer well inside f32 precision.
    fn solve_for_s(&self, t: f32) -> f32 {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if bezier_axis(self.x1, self.x2, mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }
}

impl Default for Easing {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// One axis of a cubic Bézier with endpoints 0 and 1, evaluated at `s`.
fn bezier_axis(c1: f32, c2: f32, s: f32) -> f32 {
    let u = 1.0 - s;
    3.0 * u * u * s * c1 + 3.0 * u * s * s * c2 + s * s * s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_ascend_and_stay_under_half_a_second() {
        for pair in Motion::ALL.windows(2) {
            assert!(pair[1].millis() > pair[0].millis(), "{pair:?}");
        }
        assert!(Motion::Slow.millis() <= 500);
        assert_eq!(Motion::Quick.secs(), 0.15);
    }

    #[test]
    fn every_curve_is_pinned_at_both_ends() {
        for e in Easing::ALL {
            assert_eq!(e.eval(0.0), 0.0, "{e:?}");
            assert_eq!(e.eval(1.0), 1.0, "{e:?}");
        }
    }

    #[test]
    fn out_of_range_time_is_clamped() {
        assert_eq!(Easing::STANDARD.eval(-3.0), 0.0);
        assert_eq!(Easing::STANDARD.eval(7.5), 1.0);
    }

    #[test]
    fn every_curve_is_monotonic_in_time() {
        for e in Easing::ALL {
            let mut prev = 0.0;
            for i in 0..=200 {
                let v = e.eval(i as f32 / 200.0);
                assert!(v >= prev - 1e-4, "{e:?} dipped at t={i}: {prev} -> {v}");
                prev = v;
            }
        }
    }

    #[test]
    fn linear_is_the_identity() {
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            assert!((Easing::LINEAR.eval(t) - t).abs() < 1e-3, "t={t}");
        }
    }

    #[test]
    fn decelerate_is_ahead_of_linear_and_accelerate_is_behind() {
        for i in 1..20 {
            let t = i as f32 / 20.0;
            assert!(Easing::DECELERATE.eval(t) > t, "t={t}");
            assert!(Easing::ACCELERATE.eval(t) < t, "t={t}");
        }
    }

    #[test]
    fn custom_curves_clamp_their_x_controls() {
        let e = Easing::new(-4.0, 0.0, 9.0, 1.0);
        assert_eq!(e.control_points()[0], 0.0);
        assert_eq!(e.control_points()[2], 1.0);
        assert_eq!(e.eval(1.0), 1.0);
    }
}
