//! A Curves control curve: a **monotone cubic** interpolant (Fritsch–Carlson).
//!
//! The previous implementation joined control points with straight lines. That
//! is not what a Curves dialog does and it is visible: a piecewise-linear tone
//! curve has a slope discontinuity at every control point, and a slope
//! discontinuity in a tone mapping is a contour band across any smooth gradient
//! the curve is applied to. It also handled bad input by dividing by
//! `(b.x - a.x).max(1e-5)`, so two control points with the same x produced a
//! 100000x slope and returned values wildly outside `0..=1`.
//!
//! Fritsch–Carlson is the right family here because the property a tone curve
//! must have is *monotonicity*, not smoothness for its own sake: a natural
//! cubic spline through the same points overshoots, and an overshoot in a tone
//! curve means a highlight that gets darker as you raise it. Fritsch–Carlson
//! computes the tangents a Catmull–Rom spline would use and then limits them to
//! the region where the Hermite segment provably cannot overshoot, so the
//! interpolant is C¹ *and* monotone whenever the control points are.
//!
//! Reference: F. N. Fritsch and R. E. Carlson, "Monotone Piecewise Cubic
//! Interpolation", SIAM J. Numer. Anal. 17(2), 1980.

use crate::error::{finite, AdjustmentError};

/// A validated, evaluable tone curve.
///
/// Construction sorts the control points by x and merges any that share an x
/// (their y values are averaged), so an unsorted or duplicated point list from
/// a UI or a stored document is handled rather than producing garbage.
///
/// Outside `[x_first, x_last]` the curve continues **linearly** along the end
/// knot's (already limited, already monotone) tangent. It does not hold the
/// endpoint y, and it does not extrapolate the cubic off to infinity.
///
/// Holding the endpoint would be the Curves *dialog*'s behaviour, but a dialog
/// only ever sees display-referred input. Here the input is scene-referred: an
/// exposure lift earlier in the stack legitimately hands this curve encoded
/// `1.2`, `1.4` and `1.6`, and a hold would return `1.0` for all three,
/// collapsing three distinct highlights onto one value that no later
/// adjustment can separate. That is precisely the destructive flattening this
/// crate exists to avoid, so the linear extension is the correct choice:
/// in-range results are bit-identical to the held version, the interpolant
/// stays monotone (a limited Fritsch–Carlson tangent has the same sign as the
/// data), and distinct highlights stay distinct. See
/// `highlights_stay_distinct_through_curves`.
#[derive(Debug, Clone, PartialEq)]
pub struct Curve {
    xs: Vec<f32>,
    ys: Vec<f32>,
    /// Limited tangent at each knot.
    tangents: Vec<f32>,
    identity: bool,
}

impl Curve {
    /// The curve that returns its input unchanged, evaluated with no work at
    /// all.
    pub fn identity() -> Self {
        Self {
            xs: vec![0.0, 1.0],
            ys: vec![0.0, 1.0],
            tangents: vec![1.0, 1.0],
            identity: true,
        }
    }

    /// Build a curve from control points given as `[x, y]`.
    ///
    /// The points may arrive in any order. Points sharing an x are merged into
    /// one whose y is the mean of the group — the alternative, keeping both,
    /// makes the curve a relation rather than a function.
    ///
    /// # Errors
    ///
    /// * [`AdjustmentError::NotFinite`] if any coordinate is `NaN` or infinite.
    ///   A single `NaN` x would make the sort order meaningless.
    /// * [`AdjustmentError::TooFewCurvePoints`] if fewer than two distinct x
    ///   values remain after merging.
    pub fn new(points: &[[f32; 2]]) -> Result<Self, AdjustmentError> {
        let mut pts: Vec<[f32; 2]> = Vec::with_capacity(points.len());
        for p in points {
            finite("curve point x", p[0])?;
            finite("curve point y", p[1])?;
            pts.push(*p);
        }
        pts.sort_by(|a, b| a[0].total_cmp(&b[0]));

        // Merge runs of equal x into their mean y.
        let mut xs: Vec<f32> = Vec::with_capacity(pts.len());
        let mut ys: Vec<f32> = Vec::with_capacity(pts.len());
        let mut i = 0;
        while i < pts.len() {
            let x = pts[i][0];
            let mut sum = 0.0f64;
            let mut n = 0u32;
            while i < pts.len() && pts[i][0] == x {
                sum += f64::from(pts[i][1]);
                n += 1;
                i += 1;
            }
            xs.push(x);
            ys.push((sum / f64::from(n)) as f32);
        }

        if xs.len() < 2 {
            return Err(AdjustmentError::TooFewCurvePoints { got: xs.len() });
        }

        // Every coordinate is finite, but a *span* between two of them need not
        // be: knots at -3e38 and 3e38 subtract to +inf. The tangent for that
        // interval is then `finite / inf` = 0, and `eval` multiplies the
        // interval width by it as `inf * 0.0` = NaN — which would travel into
        // the compositor and poison every blend it touched. Reject the span
        // rather than the magnitude, so ordinary out-of-display-range knots
        // still work.
        for k in 0..xs.len() - 1 {
            let span = xs[k + 1] - xs[k];
            if !span.is_finite() {
                return Err(AdjustmentError::NotFinite {
                    name: "curve point spacing",
                    value: span,
                });
            }
        }

        let tangents = fritsch_carlson_tangents(&xs, &ys);
        // An identity curve must be recognisable so it can be skipped exactly:
        // every knot on `y = x`, and the domain covering the whole display
        // range. The domain condition is deliberately conservative — the
        // linear extension outside a partial domain would *usually* agree with
        // `y = x` too, but only to within a rounding error, and this flag
        // licenses a bit-exact skip.
        let identity =
            xs[0] <= 0.0 && xs[xs.len() - 1] >= 1.0 && xs.iter().zip(&ys).all(|(x, y)| x == y);
        Ok(Self {
            xs,
            ys,
            tangents,
            identity,
        })
    }

    /// Whether this curve returns its input unchanged over the whole display
    /// range (and therefore may be skipped bit-exactly).
    pub fn is_identity(&self) -> bool {
        self.identity
    }

    /// The merged, sorted control points.
    pub fn points(&self) -> Vec<[f32; 2]> {
        self.xs
            .iter()
            .zip(&self.ys)
            .map(|(x, y)| [*x, *y])
            .collect()
    }

    /// Evaluate the curve.
    ///
    /// `NaN` in gives `NaN` out. Outside the control points' x range the curve
    /// continues linearly along the end knot's tangent, so two distinct
    /// out-of-range values stay distinct.
    pub fn eval(&self, x: f32) -> f32 {
        if self.identity {
            return x;
        }
        if x.is_nan() {
            return x;
        }
        let n = self.xs.len();
        if x <= self.xs[0] {
            // At exactly `xs[0]` the offset is zero, so the knot value is
            // returned bit-exactly.
            return self.ys[0] + (x - self.xs[0]) * self.tangents[0];
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1] + (x - self.xs[n - 1]) * self.tangents[n - 1];
        }
        // First knot strictly greater than x; `k` is the segment's left index.
        let hi = self.xs.partition_point(|&k| k <= x);
        let k = hi - 1;
        let h = self.xs[k + 1] - self.xs[k];
        let t = (x - self.xs[k]) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        // Cubic Hermite basis.
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        self.ys[k] * h00
            + h * self.tangents[k] * h10
            + self.ys[k + 1] * h01
            + h * self.tangents[k + 1] * h11
    }
}

impl Default for Curve {
    fn default() -> Self {
        Self::identity()
    }
}

/// Fritsch–Carlson tangent selection: three-point differences, then the
/// monotonicity limiter.
fn fritsch_carlson_tangents(xs: &[f32], ys: &[f32]) -> Vec<f32> {
    let n = xs.len();
    let mut deltas = Vec::with_capacity(n - 1);
    for k in 0..n - 1 {
        deltas.push((ys[k + 1] - ys[k]) / (xs[k + 1] - xs[k]));
    }

    let mut m = Vec::with_capacity(n);
    m.push(deltas[0]);
    for k in 1..n - 1 {
        // Averaging across a sign change would introduce a tangent pointing the
        // wrong way at a local extremum; zero is the monotone choice.
        if deltas[k - 1] * deltas[k] <= 0.0 {
            m.push(0.0);
        } else {
            m.push((deltas[k - 1] + deltas[k]) * 0.5);
        }
    }
    m.push(deltas[n - 2]);

    for k in 0..n - 1 {
        let d = deltas[k];
        if d == 0.0 {
            // A flat segment must stay flat at both ends or it bulges.
            m[k] = 0.0;
            m[k + 1] = 0.0;
            continue;
        }
        let alpha = m[k] / d;
        let beta = m[k + 1] / d;
        let s = alpha * alpha + beta * beta;
        // Outside the circle of radius 3 the Hermite segment can overshoot;
        // project the tangent pair back onto it.
        if s > 9.0 {
            let tau = 3.0 / s.sqrt();
            m[k] = tau * alpha * d;
            m[k + 1] = tau * beta * d;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s_curve() -> Curve {
        Curve::new(&[[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]]).unwrap()
    }

    #[test]
    fn passes_through_every_control_point() {
        let c = s_curve();
        for [x, y] in c.points() {
            assert!((c.eval(x) - y).abs() < 1e-5, "at {x}: {} vs {y}", c.eval(x));
        }
    }

    #[test]
    fn is_smooth_where_the_linear_version_had_a_kink() {
        // A steep-then-shallow pair of segments. Piecewise-linear interpolation
        // has a slope discontinuity at the middle knot; the monotone cubic does
        // not. Compare the numerical slope just either side of the knot.
        let c = Curve::new(&[[0.0, 0.0], [0.5, 0.9], [1.0, 1.0]]).unwrap();
        let e = 1e-3;
        let left = (c.eval(0.5) - c.eval(0.5 - e)) / e;
        let right = (c.eval(0.5 + e) - c.eval(0.5)) / e;
        assert!(
            (left - right).abs() < 0.05,
            "slope jumps from {left} to {right} at the knot"
        );
        // And the linear interpolant really would have kinked: its slopes are
        // 1.8 and 0.2.
        assert!((left - 1.8).abs() > 0.5 || (right - 0.2).abs() > 0.5);
    }

    #[test]
    fn monotone_input_gives_a_monotone_curve() {
        // Points that make a natural cubic spline overshoot badly: a long flat
        // run followed by a jump.
        let c = Curve::new(&[
            [0.0, 0.0],
            [0.2, 0.02],
            [0.4, 0.03],
            [0.6, 0.9],
            [0.8, 0.95],
            [1.0, 1.0],
        ])
        .unwrap();
        let mut prev = f32::NEG_INFINITY;
        for i in 0..=2000 {
            let x = i as f32 / 2000.0;
            let y = c.eval(x);
            assert!(
                y >= prev - 1e-6,
                "curve decreased at x={x}: {y} after {prev}"
            );
            prev = y;
        }
    }

    #[test]
    fn never_overshoots_the_control_point_range() {
        let c = Curve::new(&[
            [0.0, 0.0],
            [0.2, 0.02],
            [0.4, 0.03],
            [0.6, 0.9],
            [0.8, 0.95],
            [1.0, 1.0],
        ])
        .unwrap();
        for i in 0..=2000 {
            let y = c.eval(i as f32 / 2000.0);
            assert!((0.0..=1.0).contains(&y), "overshot to {y}");
        }
    }

    #[test]
    fn identity_points_are_the_identity_bit_for_bit() {
        let c = Curve::new(&[[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]).unwrap();
        assert!(c.is_identity());
        for v in [0.0f32, 0.1234_5678, 0.5, 0.9999, 1.0, 2.5, -0.3] {
            assert_eq!(c.eval(v), v, "identity curve moved {v}");
        }
    }

    #[test]
    fn a_partial_domain_identity_is_not_flagged_as_identity() {
        // y == x at every knot, but the domain does not cover 0..1. The flag
        // licenses a *bit-exact* skip, and the linear extension outside the
        // domain only agrees with `y = x` to within a rounding error, so the
        // flag stays conservative.
        let c = Curve::new(&[[0.25, 0.25], [0.75, 0.75]]).unwrap();
        assert!(!c.is_identity());
        assert!((c.eval(0.1) - 0.1).abs() < 1e-6, "{}", c.eval(0.1));
    }

    #[test]
    fn unsorted_points_are_sorted() {
        let a = Curve::new(&[[1.0, 1.0], [0.25, 0.6], [0.0, 0.0]]).unwrap();
        let b = Curve::new(&[[0.0, 0.0], [0.25, 0.6], [1.0, 1.0]]).unwrap();
        assert_eq!(a.points(), b.points());
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            assert_eq!(a.eval(x), b.eval(x));
        }
    }

    /// The specific regression: the old `curve` divided by
    /// `(b.x - a.x).max(1e-5)`, so a duplicated x produced a 100000x slope.
    #[test]
    fn duplicate_x_is_merged_not_divided_by_1e_minus_5() {
        let c = Curve::new(&[[0.0, 0.0], [0.5, 0.2], [0.5, 0.8], [1.0, 1.0]]).unwrap();
        assert_eq!(c.points().len(), 3, "duplicate x was not merged");
        // Merged to the mean of 0.2 and 0.8.
        assert!((c.eval(0.5) - 0.5).abs() < 1e-5, "{}", c.eval(0.5));
        // And nothing anywhere is out of range, which the old code was not.
        for i in 0..=1000 {
            let y = c.eval(i as f32 / 1000.0);
            assert!(
                (-0.01..=1.01).contains(&y),
                "duplicate-x curve blew up: {y}"
            );
        }
    }

    #[test]
    fn all_points_at_one_x_is_rejected() {
        assert_eq!(
            Curve::new(&[[0.5, 0.1], [0.5, 0.9]]),
            Err(AdjustmentError::TooFewCurvePoints { got: 1 })
        );
        assert_eq!(
            Curve::new(&[]),
            Err(AdjustmentError::TooFewCurvePoints { got: 0 })
        );
        assert_eq!(
            Curve::new(&[[0.3, 0.3]]),
            Err(AdjustmentError::TooFewCurvePoints { got: 1 })
        );
    }

    #[test]
    fn non_finite_points_are_rejected() {
        match Curve::new(&[[0.0, 0.0], [f32::NAN, 0.5], [1.0, 1.0]]) {
            Err(AdjustmentError::NotFinite { name, .. }) => assert_eq!(name, "curve point x"),
            other => panic!("expected rejection, got {other:?}"),
        }
        match Curve::new(&[[0.0, 0.0], [0.5, f32::INFINITY], [1.0, 1.0]]) {
            Err(AdjustmentError::NotFinite { name, value }) => {
                assert_eq!(name, "curve point y");
                assert_eq!(value, f32::INFINITY);
            }
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    /// The end knots are returned bit-exactly, and beyond them the curve
    /// continues along the end tangent rather than flattening.
    #[test]
    fn outside_the_domain_the_end_tangents_extend_the_curve() {
        let c = s_curve();
        // The knot values themselves are exact.
        assert_eq!(c.eval(0.0), 0.0);
        assert_eq!(c.eval(1.0), 1.0);
        // Both end tangents are 0.6 for this curve (the end segment's own
        // slope, unchanged by the limiter).
        assert!(
            (c.eval(5.0) - (1.0 + 4.0 * 0.6)).abs() < 1e-5,
            "{}",
            c.eval(5.0)
        );
        assert!(
            (c.eval(-5.0) - (-5.0 * 0.6)).abs() < 1e-5,
            "{}",
            c.eval(-5.0)
        );
    }

    /// The regression this replaced: holding the endpoint y outside the
    /// control points' x range crushes every scene-referred highlight onto one
    /// output value. Three encoded highlights 0.2 apart must still be strictly
    /// ordered coming out, and at least one must still be above `1.0`, or a
    /// later exposure pull-back returns a flat white patch.
    #[test]
    fn highlights_stay_distinct_past_the_last_knot() {
        let c = Curve::new(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]).unwrap();
        let out = [1.2f32, 1.4, 1.6, 3.0].map(|v| c.eval(v));
        for w in out.windows(2) {
            assert!(w[0] < w[1], "highlights collapsed: {out:?}");
        }
        assert!(out.iter().any(|v| *v > 1.0), "{out:?}");
        // The last segment's slope is 0.8, so the extension is exact arithmetic.
        assert!((out[0] - 1.16).abs() < 1e-5, "{out:?}");
        assert!((out[3] - (1.0 + 2.0 * 0.8)).abs() < 1e-5, "{out:?}");

        // And symmetrically below the first knot: negatives do not all collapse
        // onto `ys[0]`.
        let below = [-0.1f32, -0.3, -0.6].map(|v| c.eval(v));
        for w in below.windows(2) {
            assert!(w[0] > w[1], "below-black values collapsed: {below:?}");
        }
    }

    /// The extension must not break the property the whole spline is chosen
    /// for: monotone control points give a monotone curve *everywhere*, not
    /// just between the knots.
    #[test]
    fn the_linear_extension_stays_monotone() {
        let c = Curve::new(&[[0.0, 0.05], [0.3, 0.2], [0.7, 0.9], [1.0, 0.95]]).unwrap();
        let mut prev = f32::NEG_INFINITY;
        for i in 0..=3000 {
            // -1.0 ..= 2.0, well past both knots.
            let x = -1.0 + i as f32 / 1000.0;
            let y = c.eval(x);
            assert!(y >= prev - 1e-6, "decreased at x={x}: {y} after {prev}");
            prev = y;
        }
    }

    #[test]
    fn a_flat_segment_stays_flat() {
        // Two knots at the same y: the limiter must zero both tangents or the
        // segment bulges above or below them.
        let c = Curve::new(&[[0.0, 0.0], [0.3, 0.5], [0.7, 0.5], [1.0, 1.0]]).unwrap();
        for i in 0..=100 {
            let x = 0.3 + 0.4 * (i as f32 / 100.0);
            assert!((c.eval(x) - 0.5).abs() < 1e-5, "at {x}: {}", c.eval(x));
        }
    }

    #[test]
    fn a_decreasing_curve_is_monotone_too() {
        let c = Curve::new(&[[0.0, 1.0], [0.4, 0.8], [0.6, 0.15], [1.0, 0.0]]).unwrap();
        let mut prev = f32::INFINITY;
        for i in 0..=2000 {
            let y = c.eval(i as f32 / 2000.0);
            assert!(
                y <= prev + 1e-6,
                "inverted curve increased: {y} after {prev}"
            );
            prev = y;
        }
    }

    #[test]
    fn nan_in_nan_out() {
        assert!(s_curve().eval(f32::NAN).is_nan());
    }

    #[test]
    fn extreme_knot_spacing_is_rejected_rather_than_evaluating_to_nan() {
        // Both coordinates are finite, but their difference is not, which used
        // to produce `inf * 0.0` = NaN inside `eval` and send a NaN pixel into
        // the compositor.
        let err = Curve::new(&[[-3.0e38, 0.0], [3.0e38, 1.0]])
            .expect_err("an infinite knot span must be refused");
        assert!(
            matches!(err, AdjustmentError::NotFinite { .. }),
            "expected NotFinite, got {err:?}"
        );
    }

    #[test]
    fn a_curve_that_is_accepted_never_evaluates_to_nan() {
        // The guard is only worth having if everything that survives it is
        // total, so sweep the accepted curve across and beyond its domain.
        let c = Curve::new(&[[-1.0e30, 0.0], [0.5, 0.25], [1.0e30, 1.0]]).unwrap();
        for i in -20..=20 {
            let x = i as f32 * 1.0e29;
            assert!(c.eval(x).is_finite(), "eval({x}) was not finite");
        }
    }
}
