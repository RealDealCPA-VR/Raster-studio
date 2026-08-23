//! Exact-area scanline rasterisation, shared by the marquee and the lasso.
//!
//! Anti-aliasing here is **coverage**, not a filter: a pixel's value is the
//! fraction of its area the shape covers. That is computed exactly in `x` — a
//! span contributes its literal overlap with each pixel column — and by
//! [`SUBSCANLINES`]-fold integration in `y`. The error is therefore only the
//! `y` quadrature, which is what makes the area of a rasterised ellipse match
//! `π·rx·ry` to a fraction of a pixel instead of to a pixel or two.
//!
//! The interior of a span is added through a difference array rather than
//! written per column, so a row costs `O(subscanlines + width)` rather than
//! `O(subscanlines · width)`.

use crate::buf::{alloc_f32, to_byte};

/// Sub-scanlines integrated per pixel row.
///
/// 16 puts the worst-case coverage error (at a near-horizontal tangent) around
/// 1/32 of a pixel, well under one 8-bit level of the value it feeds.
pub(crate) const SUBSCANLINES: usize = 16;

/// Accumulates fractional coverage for one pixel row.
pub(crate) struct RowAccum {
    /// Document x of column 0.
    x0: i32,
    width: usize,
    /// Partial coverage written straight to a column.
    direct: Vec<f32>,
    /// Difference array for whole-column runs; `width + 1` long.
    diff: Vec<f32>,
}

impl RowAccum {
    pub(crate) fn new(x0: i32, width: usize) -> Result<Self, crate::error::SelectionOpError> {
        Ok(Self {
            x0,
            width,
            direct: alloc_f32(width)?,
            diff: alloc_f32(width + 1)?,
        })
    }

    /// Add `weight` coverage over the half-open x range `[xa, xb)`.
    pub(crate) fn add_span(&mut self, xa: f32, xb: f32, weight: f32) {
        if self.width == 0 || !(xa.is_finite() && xb.is_finite()) {
            return;
        }
        let lo = self.x0 as f32;
        let hi = (self.x0 as i64 + self.width as i64) as f32;
        let xa = xa.max(lo);
        let xb = xb.min(hi);
        if xb <= xa {
            return;
        }
        // Both ends are inside [x0, x0 + width] now, so these floors are in
        // range and the casts cannot wrap.
        let ia = (xa.floor() - lo) as usize;
        let ib_f = (xb.floor() - lo) as usize;
        let ib = ib_f.min(self.width);

        if ia == ib {
            // Wholly inside one column (or exactly at the right edge).
            if ia < self.width {
                self.direct[ia] += (xb - xa) * weight;
            }
            return;
        }
        self.direct[ia] += (lo + (ia + 1) as f32 - xa) * weight;
        if ib > ia + 1 {
            self.diff[ia + 1] += weight;
            self.diff[ib] -= weight;
        }
        if ib < self.width {
            self.direct[ib] += (xb - (lo + ib as f32)) * weight;
        }
    }

    /// Write the accumulated row into `out` and reset for the next row.
    pub(crate) fn finish_into(&mut self, out: &mut [u8]) {
        let mut running = 0.0f32;
        for (x, o) in out.iter_mut().enumerate().take(self.width) {
            running += self.diff[x];
            *o = to_byte(self.direct[x] + running);
            self.direct[x] = 0.0;
            self.diff[x] = 0.0;
        }
        self.diff[self.width] = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(x0: i32, width: usize, spans: &[(f32, f32, f32)]) -> Vec<u8> {
        let mut a = RowAccum::new(x0, width).unwrap();
        for &(xa, xb, w) in spans {
            a.add_span(xa, xb, w);
        }
        let mut out = vec![0u8; width];
        a.finish_into(&mut out);
        out
    }

    #[test]
    fn a_whole_pixel_span_is_solid_and_a_partial_one_is_proportional() {
        assert_eq!(row(0, 4, &[(1.0, 3.0, 1.0)]), vec![0, 255, 255, 0]);
        assert_eq!(row(0, 4, &[(1.0, 1.5, 1.0)]), vec![0, 128, 0, 0]);
        assert_eq!(row(0, 4, &[(0.25, 3.75, 1.0)]), vec![191, 255, 255, 191]);
    }

    #[test]
    fn spans_accumulate_and_clip_to_the_row() {
        // Two half-weight passes over the same span sum to full coverage,
        // which is exactly how the sub-scanline integration works.
        assert_eq!(row(0, 3, &[(0.0, 3.0, 0.5), (0.0, 3.0, 0.5)]), vec![255; 3]);
        // Off the ends: clipped, never a panic or a wrap.
        assert_eq!(row(0, 3, &[(-100.0, 100.0, 1.0)]), vec![255; 3]);
        assert_eq!(row(0, 3, &[(-100.0, -50.0, 1.0)]), vec![0; 3]);
        assert_eq!(row(0, 3, &[(50.0, 100.0, 1.0)]), vec![0; 3]);
        assert_eq!(row(0, 3, &[(f32::NAN, 2.0, 1.0)]), vec![0; 3]);
    }

    #[test]
    fn a_span_that_ends_exactly_on_the_right_edge_does_not_index_past_the_row() {
        // `floor(xb)` lands on `width` here; the guard is the difference
        // between a correct last column and an out-of-bounds write.
        assert_eq!(row(0, 3, &[(0.0, 3.0, 1.0)]), vec![255; 3]);
        assert_eq!(row(-5, 3, &[(-3.0, -2.0, 1.0)]), vec![0, 0, 255]);
    }

    #[test]
    fn an_accumulator_is_clean_after_finishing_a_row() {
        let mut a = RowAccum::new(0, 4).unwrap();
        a.add_span(0.0, 4.0, 1.0);
        let mut out = vec![0u8; 4];
        a.finish_into(&mut out);
        assert_eq!(out, vec![255; 4]);
        a.add_span(1.0, 2.0, 1.0);
        a.finish_into(&mut out);
        assert_eq!(out, vec![0, 255, 0, 0], "the previous row must not leak");
    }
}
