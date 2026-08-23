//! Marquee tools: rectangle, ellipse, single row, single column.
//!
//! The rectangle forms come in a pixel-aligned variant (the common case: a drag
//! snapped to the pixel grid) and a sub-pixel one, which anti-aliases its edges
//! like the ellipse does. All of them allocate only their own bounding box, so
//! a 10-pixel ellipse on a 100-megapixel canvas costs 100 bytes.

use editor_core::SelectionMask;
use glam::Vec2;

use crate::buf::{alloc_bytes, checked_samples, CoverageBuf};
use crate::error::SelectionOpError;
use crate::rect::Rect;
use crate::scan::{RowAccum, SUBSCANLINES};

fn check_finite(what: &'static str, v: f32) -> Result<(), SelectionOpError> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(SelectionOpError::NotFinite { what, value: v })
    }
}

fn check_point(what: &'static str, p: Vec2) -> Result<(), SelectionOpError> {
    check_finite(what, p.x)?;
    check_finite(what, p.y)
}

/// A pixel-aligned rectangular selection: every pixel inside is fully selected.
pub fn rectangle(rect: Rect) -> Result<SelectionMask, SelectionOpError> {
    if rect.is_empty() {
        return Ok(SelectionMask::new(rect.min(), 0, 0, Vec::new())?);
    }
    let n = checked_samples(rect)?;
    Ok(SelectionMask::new(
        rect.min(),
        rect.width(),
        rect.height(),
        alloc_bytes(n, 255)?,
    )?)
}

/// A rectangle with sub-pixel corners; the edge pixels get fractional coverage
/// equal to the fraction of their area inside the rectangle.
///
/// Exactly that fraction, on both axes: an axis-aligned rectangle's overlap
/// with a pixel row is a closed form, so unlike the ellipse — whose `y` is
/// integrated over 16 sub-scanlines — there is no quadrature error here at all.
pub fn rectangle_subpixel(a: Vec2, b: Vec2) -> Result<SelectionMask, SelectionOpError> {
    check_point("rectangle corner", a)?;
    check_point("rectangle corner", b)?;
    let bbox = Rect::enclosing(a, b);
    if bbox.is_empty() {
        return Ok(SelectionMask::new(bbox.min(), 0, 0, Vec::new())?);
    }
    let (lo, hi) = localise(bbox, a.min(b), a.max(b));
    fill_axis_rect(bbox, lo, hi)
}

/// Shift a pair of document-space points into the bounding box's local frame.
///
/// Everything is rasterised in local coordinates because `f32` runs out of
/// precision long before the coordinate grid does: at `x = 2^29` the spacing
/// between neighbouring `f32` values is 64 pixels, so a 10-pixel ellipse placed
/// there would collapse to nothing if its corners were rasterised as absolute
/// floats. Local coordinates are small by construction, and the placement is
/// carried by the buffer's integer origin instead.
fn localise(bbox: Rect, lo: Vec2, hi: Vec2) -> (Vec2, Vec2) {
    let o = Vec2::new(bbox.min().x as f32, bbox.min().y as f32);
    (lo - o, hi - o)
}

fn fill_axis_rect(bbox: Rect, lo: Vec2, hi: Vec2) -> Result<SelectionMask, SelectionOpError> {
    let mut buf = CoverageBuf::zeroed(bbox)?;
    let width = bbox.width() as usize;
    let mut accum = RowAccum::new(0, width)?;
    for row in 0..bbox.height() as usize {
        // The vertical overlap of this pixel row with [lo.y, hi.y) — exact, so
        // a rectangle 0.3 of a pixel tall really is 0.3 covered rather than the
        // nearest sixteenth of one.
        let top = lo.y.max(row as f32);
        let bottom = hi.y.min(row as f32 + 1.0);
        if bottom > top {
            accum.add_span(lo.x, hi.x, bottom - top);
        }
        accum.finish_into(buf.row_mut(row));
    }
    buf.into_mask()
}

/// An anti-aliased ellipse inscribed in `rect`.
///
/// Coverage is the fraction of each pixel's area inside the ellipse, so the
/// mask's total coverage equals the ellipse's area to well under a pixel — see
/// `an_ellipse_covers_its_analytic_area`.
pub fn ellipse(rect: Rect) -> Result<SelectionMask, SelectionOpError> {
    if rect.is_empty() {
        return Ok(SelectionMask::new(rect.min(), 0, 0, Vec::new())?);
    }
    fill_ellipse(
        rect,
        Vec2::ZERO,
        Vec2::new(rect.width() as f32, rect.height() as f32),
    )
}

/// [`ellipse`] with sub-pixel bounds.
pub fn ellipse_subpixel(a: Vec2, b: Vec2) -> Result<SelectionMask, SelectionOpError> {
    check_point("ellipse corner", a)?;
    check_point("ellipse corner", b)?;
    let bbox = Rect::enclosing(a, b);
    if bbox.is_empty() {
        return Ok(SelectionMask::new(bbox.min(), 0, 0, Vec::new())?);
    }
    let (lo, hi) = localise(bbox, a.min(b), a.max(b));
    fill_ellipse(bbox, lo, hi)
}

fn fill_ellipse(bbox: Rect, lo: Vec2, hi: Vec2) -> Result<SelectionMask, SelectionOpError> {
    let c = (lo + hi) * 0.5;
    let r = (hi - lo) * 0.5;
    if r.x <= 0.0 || r.y <= 0.0 {
        return Ok(SelectionMask::new(bbox.min(), 0, 0, Vec::new())?);
    }
    let mut buf = CoverageBuf::zeroed(bbox)?;
    let width = bbox.width() as usize;
    let mut accum = RowAccum::new(0, width)?;
    let sub = 1.0 / SUBSCANLINES as f32;
    for row in 0..bbox.height() as usize {
        for s in 0..SUBSCANLINES {
            let y = row as f32 + (s as f32 + 0.5) * sub;
            let dy = (y - c.y) / r.y;
            let t = 1.0 - dy * dy;
            if t <= 0.0 {
                continue;
            }
            let dx = r.x * t.sqrt();
            accum.add_span(c.x - dx, c.x + dx, sub);
        }
        accum.finish_into(buf.row_mut(row));
    }
    buf.into_mask()
}

/// A one-pixel-tall selection spanning `[x, x + width)`.
pub fn single_row(y: i32, x: i32, width: u32) -> Result<SelectionMask, SelectionOpError> {
    rectangle(Rect::from_xywh(x, y, width, 1))
}

/// A one-pixel-wide selection spanning `[y, y + height)`.
pub fn single_column(x: i32, y: i32, height: u32) -> Result<SelectionMask, SelectionOpError> {
    rectangle(Rect::from_xywh(x, y, 1, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::IVec2;

    fn total_coverage(m: &SelectionMask) -> f64 {
        m.coverage().iter().map(|&v| v as f64 / 255.0).sum()
    }

    #[test]
    fn a_rectangle_selects_exactly_its_half_open_extent() {
        let m = rectangle(Rect::from_xywh(4, 5, 3, 2)).unwrap();
        assert_eq!(m.bounds(), Some((IVec2::new(4, 5), IVec2::new(7, 7))));
        assert_eq!(m.coverage(), &[255; 6]);
        assert_eq!(m.coverage_at(IVec2::new(6, 6)), 255);
        assert_eq!(m.coverage_at(IVec2::new(7, 6)), 0, "max is exclusive");
    }

    #[test]
    fn a_single_row_and_column_are_one_pixel_thick() {
        let r = single_row(9, 2, 5).unwrap();
        assert_eq!(r.height(), 1);
        assert_eq!(r.width(), 5);
        assert_eq!(r.coverage_at(IVec2::new(6, 9)), 255);
        assert_eq!(r.coverage_at(IVec2::new(6, 10)), 0);

        let c = single_column(3, -2, 4).unwrap();
        assert_eq!(c.width(), 1);
        assert_eq!(c.height(), 4);
        assert_eq!(c.coverage_at(IVec2::new(3, 1)), 255);
        assert_eq!(c.coverage_at(IVec2::new(4, 1)), 0);
    }

    #[test]
    fn an_ellipse_is_antialiased_at_its_edge() {
        let m = ellipse(Rect::from_xywh(0, 0, 40, 40)).unwrap();
        // Centre is solid, well outside is empty, and the rim carries values
        // that are neither.
        assert_eq!(m.coverage_at(IVec2::new(20, 20)), 255);
        assert_eq!(m.coverage_at(IVec2::new(0, 0)), 0, "the corner is outside");
        let partial = m.coverage().iter().filter(|&&v| v > 0 && v < 255).count();
        assert!(
            partial > 40,
            "a 40px circle should have a fractional rim, found {partial} partial samples"
        );
        // The rim runs all the way around: one sample per quadrant, taken at
        // 45 degrees where the boundary genuinely crosses a pixel.
        for p in [
            IVec2::new(5, 5),
            IVec2::new(34, 5),
            IVec2::new(5, 34),
            IVec2::new(34, 34),
        ] {
            let v = m.coverage_at(p);
            assert!(v > 0 && v < 255, "{p:?} should be a partial edge, got {v}");
        }
        // At the very top of the circle the boundary is horizontal, so the
        // centre column really is fully covered there — an anti-aliased shape
        // is not required to be fuzzy everywhere along its rim.
        assert_eq!(m.coverage_at(IVec2::new(20, 0)), 255);
    }

    #[test]
    fn an_ellipse_covers_its_analytic_area() {
        for (w, h) in [(40u32, 40u32), (31, 17), (9, 9), (100, 60)] {
            let m = ellipse(Rect::from_xywh(0, 0, w, h)).unwrap();
            let expected = std::f64::consts::PI * (w as f64 / 2.0) * (h as f64 / 2.0);
            let got = total_coverage(&m);
            // Half a percent, and never worse than a pixel: exact-in-x span
            // integration is what buys this.
            let tol = (expected * 0.005).max(1.0);
            assert!(
                (got - expected).abs() <= tol,
                "{w}x{h} ellipse: area {got:.3}, expected {expected:.3}"
            );
        }
    }

    #[test]
    fn an_ellipse_is_symmetric() {
        let m = ellipse(Rect::from_xywh(0, 0, 32, 32)).unwrap();
        for y in 0..32 {
            for x in 0..32 {
                let v = m.coverage_at(IVec2::new(x, y));
                assert_eq!(
                    v,
                    m.coverage_at(IVec2::new(31 - x, y)),
                    "x mirror at {x},{y}"
                );
                assert_eq!(
                    v,
                    m.coverage_at(IVec2::new(x, 31 - y)),
                    "y mirror at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn a_subpixel_rectangle_gets_fractional_edges() {
        let m = rectangle_subpixel(Vec2::new(0.5, 0.0), Vec2::new(2.25, 1.0)).unwrap();
        assert_eq!(m.origin(), IVec2::new(0, 0));
        assert_eq!((m.width(), m.height()), (3, 1));
        assert_eq!(m.coverage_at(IVec2::new(0, 0)), 128, "half of column 0");
        assert_eq!(m.coverage_at(IVec2::new(1, 0)), 255);
        assert_eq!(m.coverage_at(IVec2::new(2, 0)), 64, "a quarter of column 2");
    }

    #[test]
    fn a_subpixel_rectangles_coverage_is_the_exact_area_fraction_on_both_axes() {
        // 0.3 of a pixel tall. Integrating y over sixteen sub-scanlines instead
        // gives 5/16 = 0.3125 -> 80, a 1.4% error on a value the doc calls the
        // fraction of the pixel's area.
        let tall = rectangle_subpixel(Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.3)).unwrap();
        assert_eq!(tall.coverage_at(IVec2::new(0, 0)), 77);
        // The same rectangle turned on its side, where x was always exact.
        let wide = rectangle_subpixel(Vec2::new(0.0, 0.0), Vec2::new(0.3, 1.0)).unwrap();
        assert_eq!(wide.coverage_at(IVec2::new(0, 0)), 77);

        // A rectangle straddling three rows: the two partial rows are exact and
        // the whole one is solid.
        let m = rectangle_subpixel(Vec2::new(0.0, 0.75), Vec2::new(1.0, 2.5)).unwrap();
        assert_eq!(m.coverage_at(IVec2::new(0, 0)), 64, "a quarter of row 0");
        assert_eq!(m.coverage_at(IVec2::new(0, 1)), 255);
        assert_eq!(m.coverage_at(IVec2::new(0, 2)), 128, "half of row 2");

        // And the total is the analytic area, to within the byte quantisation.
        let area = total_coverage(
            &rectangle_subpixel(Vec2::new(1.4, 2.35), Vec2::new(6.9, 5.05)).unwrap(),
        );
        let expected = (6.9 - 1.4) * (5.05 - 2.35);
        assert!(
            (area - expected).abs() < 0.1,
            "area {area:.4}, expected {expected:.4}"
        );
    }

    #[test]
    fn a_small_shape_on_a_huge_canvas_allocates_only_its_own_box() {
        // The property that makes selections usable on giant documents: nothing
        // here is proportional to the canvas.
        let far = 1 << 29;
        let m = ellipse(Rect::from_xywh(far, far, 10, 10)).unwrap();
        assert_eq!(m.origin(), IVec2::new(far, far));
        assert_eq!(m.coverage().len(), 100);

        let r = rectangle(Rect::from_xywh(-far, far, 4, 4)).unwrap();
        assert_eq!(r.coverage().len(), 16);
    }

    #[test]
    fn a_subpixel_shape_rasterises_identically_wherever_it_is_placed() {
        // At x = 2^22 the gap between neighbouring f32 values is half a pixel,
        // so rasterising in absolute coordinates would collapse the sixteen
        // sub-scanline offsets inside a row onto two and coarsen the
        // anti-aliasing with distance from the origin.
        // Half-pixel corners, which are exactly representable at 2^22, so the
        // fixture itself loses nothing and only the rasteriser is under test.
        let far = (1i32 << 22) as f32;
        let far_i = IVec2::splat(far as i32);
        let corners = |o: f32| (Vec2::new(o + 0.5, o + 0.5), Vec2::new(o + 20.5, o + 13.5));

        let (a0, b0) = corners(0.0);
        let (a1, b1) = corners(far);
        let e0 = ellipse_subpixel(a0, b0).unwrap();
        let e1 = ellipse_subpixel(a1, b1).unwrap();
        assert!(
            e0.coverage().iter().any(|&v| v > 0 && v < 255),
            "the fixture needs an anti-aliased rim to lose"
        );
        assert_eq!(
            e1.coverage(),
            e0.coverage(),
            "a sub-pixel ellipse lost precision far from the origin"
        );
        assert_eq!(e1.origin(), e0.origin() + far_i);

        let r0 = rectangle_subpixel(a0, b0).unwrap();
        let r1 = rectangle_subpixel(a1, b1).unwrap();
        assert_eq!(
            r1.coverage(),
            r0.coverage(),
            "a sub-pixel rectangle lost precision far from the origin"
        );
        assert_eq!(r1.origin(), r0.origin() + far_i);
    }

    #[test]
    fn a_degenerate_marquee_selects_nothing_instead_of_failing() {
        assert!(rectangle(Rect::from_xywh(3, 3, 0, 9)).unwrap().is_empty());
        assert!(ellipse(Rect::from_xywh(3, 3, 0, 9)).unwrap().is_empty());
        assert!(ellipse(Rect::from_xywh(3, 3, 9, 0)).unwrap().is_empty());
        assert!(matches!(
            ellipse_subpixel(Vec2::new(f32::NAN, 0.0), Vec2::new(4.0, 4.0)),
            Err(SelectionOpError::NotFinite { .. })
        ));
    }
}
