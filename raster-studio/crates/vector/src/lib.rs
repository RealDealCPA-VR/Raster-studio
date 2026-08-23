//! Bezier path geometry and anti-aliased rasterisation.
//!
//! Shapes, the pen tool, vector masks and text outlines all reduce to the same
//! two operations: build a path, then turn it into coverage. This crate owns
//! both, and deliberately produces a **coverage mask** rather than colour, so
//! fills, strokes, masks and selections share one rasteriser instead of each
//! growing their own.
//!
//! It is a leaf crate: no document, no I/O, no GPU.
//!
//! # The five rules everything here follows
//!
//! * **One rasteriser.** A fill, a stroke, a boolean result, a vector mask and
//!   a path-shaped selection all end at [`fill::fill`], which answers exactly
//!   one question — *how much of this pixel is inside?* — and returns a
//!   [`CoverageMask`]. Anything that wanted colour instead would have to know
//!   about blend modes and premultiplication, and would have grown a second,
//!   subtly different scan converter within a release.
//! * **Strokes are outlines.** [`stroke::stroke`] converts a stroke into a
//!   closed, positively-oriented path and hands it to the same rasteriser. That
//!   is what guarantees a shape and its own outline agree along the boundary
//!   they share, and it is where hairline seams come from when they do not.
//! * **Coverage is compatible, not converted.** [`CoverageMask`] has the same
//!   four fields, the same row-major byte layout and the same invariants as
//!   `editor_core::SelectionMask`, so a rasterised path becomes a selection via
//!   [`CoverageMask::into_parts`] with no resampling and no reinterpretation.
//!   The dependency deliberately does not exist: `vector` sits below the
//!   document model, and importing it would run the layering backwards.
//! * **Caller input never panics.** NaN coordinates, absurd extents, malformed
//!   `d` strings, zero-length segments, empty paths, radii too small to span
//!   their endpoints and dash patterns finer than the path itself are all
//!   reachable from a file or a gesture. Each is a [`VectorError`] or a
//!   documented degenerate result — never an abort, and never a panic. That
//!   extends to memory, by one of two routes. The rasteriser's buffers — the
//!   coverage mask, the scanline accumulator it is built from, and a trimmed
//!   copy — are sized by a caller's extent multiplied out, so each is reserved
//!   with `try_reserve` and an unaffordable one is a
//!   [`VectorError::OutOfMemory`] rather than `handle_alloc_error`, which is an
//!   abort no editor can report. Everywhere else the *work* is capped instead,
//!   before the buffers it would size are allocated: [`fill::fill`] refuses a
//!   region over [`MAX_MASK_SAMPLES`], and [`boolean`] refuses an operand or a
//!   split over [`boolean::MAX_EDGES`] and a crossing search over
//!   [`boolean::MAX_PAIR_TESTS`]. Both routes end in a [`VectorError`] the
//!   editor can put in front of the user.
//! * **Precision is `f64`.** Subdivision, arc-length bisection and segment
//!   intersection compound rounding over many steps, and `f32` cannot separate
//!   two points a thousandth of a pixel apart at the far edge of a large canvas.
//!   Conversion to `glam`'s `f32` vectors happens at the boundary, not inside.
//!
//! # A tour
//!
//! ```
//! use vector::{
//!     boolean, fill, point, shapes, stroke, svg,
//!     Bounds, Cap, FillOptions, FillRule, Join, StrokeStyle,
//! };
//!
//! // A shape, and a hole cut out of it.
//! let plate = shapes::rounded_rect(
//!     Bounds::from_xywh(0.0, 0.0, 80.0, 50.0),
//!     shapes::CornerRadii::uniform(8.0),
//! );
//! let hole = shapes::circle(point(60.0, 25.0), 10.0);
//! let region = boolean::difference(&plate, &hole)?;
//!
//! // Rasterised, it is a coverage mask the selection engine accepts as-is.
//! let mask = fill::fill(&region, &FillOptions::with_rule(FillRule::NonZero))?;
//! let (origin, w, h, coverage) = mask.into_parts();
//! assert_eq!(coverage.len(), (w * h) as usize);
//! # let _ = origin;
//!
//! // A dashed outline is a path like any other, so it fills the same way.
//! let outline = stroke::stroke(
//!     &region,
//!     &StrokeStyle { width: 3.0, cap: Cap::Round, join: Join::Round, ..StrokeStyle::default() },
//! )?;
//! assert!(fill::fill(&outline, &FillOptions::default())?.area() > 0.0);
//!
//! // And any of it round-trips through SVG path data.
//! let text = svg::to_svg(&region);
//! assert_eq!(svg::parse(&text)?.subpaths().len(), region.subpaths().len());
//! # Ok::<(), vector::VectorError>(())
//! ```

#![forbid(unsafe_code)]

pub mod affine;
pub mod boolean;
pub mod error;
pub mod fill;
pub mod hit;
pub mod mask;
pub mod path;
pub mod point;
pub mod segment;
pub mod shapes;
pub mod stroke;
pub mod svg;

pub use affine::Affine;
pub use boolean::{difference, intersection, union, xor, BoolOp};
pub use error::VectorError;
pub use fill::{fill, fill_polylines, FillOptions, FillRule, MAX_SAMPLES_PER_PIXEL};
pub use hit::{
    contains, contains_with, distance_to_outline, hit_stroke, nearest_point, EDGE_EPSILON,
};
pub use mask::{CoverageMask, PixelRect, COORD_LIMIT, MAX_MASK_SAMPLES};
pub use path::{Path, PathEl, Polyline, SubPath};
pub use point::{point, Bounds, Point};
pub use segment::Segment;
pub use shapes::{ArrowStyle, CornerRadii};
pub use stroke::{dash, stroke, Cap, Dash, Join, StrokeStyle};
pub use svg::{parse as parse_svg, to_svg};

/// Default flattening tolerance: the largest distance, in path units, that a
/// polyline is allowed to stray from the curve it replaces.
///
/// A tenth of a pixel. Below the rasteriser's own resolution — its coverage
/// steps are 1/255 of a pixel and its vertical sampling 1/16 — so at the
/// default the flattening is invisible in the output, and above it the polygon
/// starts to show on shallow arcs. A path that will be scaled up before it is
/// drawn should be flattened with a proportionally tighter value; see
/// [`Affine::max_scale`].
pub const DEFAULT_TOLERANCE: f64 = 0.1;

#[cfg(test)]
mod tests {
    use super::*;
    use glam::IVec2;

    /// The end-to-end shape of a real session: build a shape, combine it, stroke
    /// it, rasterise it, hit-test it, and store it as text — with every stage
    /// consuming the previous stage's output unchanged.
    #[test]
    fn a_shape_survives_a_full_round_of_editing() {
        // Draw a rounded plate, punch two holes, and keep the region.
        let plate = shapes::rounded_rect(
            Bounds::from_xywh(0.0, 0.0, 120.0, 60.0),
            CornerRadii::new(12.0, 12.0, 4.0, 4.0),
        );
        let region = boolean::difference(&plate, &shapes::circle(point(30.0, 30.0), 10.0)).unwrap();
        let region =
            boolean::difference(&region, &shapes::circle(point(90.0, 30.0), 10.0)).unwrap();
        assert_eq!(region.subpaths().len(), 3, "an outline and two holes");

        // Rasterise it. The holes really are holes.
        let m = fill(&region, &FillOptions::default()).unwrap();
        assert_eq!(m.coverage_at(IVec2::new(30, 30)), 0);
        assert_eq!(m.coverage_at(IVec2::new(60, 30)), 255);

        // Its area is the plate minus two discs, minus the rounded corners.
        let corners = (1.0 - std::f64::consts::FRAC_PI_4) * (2.0 * 144.0 + 2.0 * 16.0);
        let expected = 120.0 * 60.0 - corners - 2.0 * std::f64::consts::PI * 100.0;
        assert!(
            (m.area() - expected).abs() / expected < 0.01,
            "{}",
            m.area()
        );

        // Hit testing agrees with the pixels.
        assert!(contains(&region, point(60.0, 30.0), FillRule::NonZero));
        assert!(!contains(&region, point(30.0, 30.0), FillRule::NonZero));
        assert!(
            hit_stroke(&region, point(30.0, 20.5), 1.0),
            "on a hole's rim"
        );

        // Stroke it, and the stroke is a path that fills like any other.
        let outline = stroke(
            &region,
            &StrokeStyle {
                width: 2.0,
                join: Join::Round,
                cap: Cap::Round,
                ..StrokeStyle::default()
            },
        )
        .unwrap();
        let om = fill(&outline, &FillOptions::default()).unwrap();
        assert!(om.area() > 0.0);
        // Total ink of the outline is roughly perimeter times width.
        assert!(
            (om.area() / (region.length() * 2.0) - 1.0).abs() < 0.1,
            "outline area {} for perimeter {}",
            om.area(),
            region.length()
        );

        // Store it as text and read it back with the geometry intact.
        let text = svg::to_svg_with_precision(&region, 12);
        let reloaded = svg::parse(&text).unwrap();
        assert_eq!(reloaded.subpaths().len(), 3);
        let m2 = fill(&reloaded, &FillOptions::default()).unwrap();
        assert_eq!(m2.origin(), m.origin());
        assert_eq!(m2.coverage(), m.coverage());

        // And a transform moves all of it together.
        let t = Affine::translate(500.0, 500.0).then(Affine::scale(2.0, 2.0));
        let moved = fill(&region.transform(&t), &FillOptions::default()).unwrap();
        assert!((moved.area() / m.area() - 4.0).abs() < 0.05);
        assert_eq!(moved.origin(), IVec2::new(1000, 1000));
    }

    /// The compatibility contract, held as a test rather than as a paragraph:
    /// a rasterised path is exactly the arguments the selection engine's
    /// constructors take, with no conversion between.
    #[test]
    fn a_rasterised_path_is_a_selection_mask_in_all_but_name() {
        let m = fill(
            &shapes::ellipse(point(20.0, 20.0), point(15.0, 10.0)),
            &FillOptions::default(),
        )
        .unwrap();
        let bounds = m.bounds();
        let (origin, w, h, coverage) = m.into_parts();

        // The four fields `SelectionMask::new(origin, width, height, coverage)`
        // and `channel_to_selection(origin, width, height, &coverage)` want.
        assert_eq!(coverage.len(), (w as usize) * (h as usize));
        assert!(w > 0 && h > 0);
        assert!(coverage.iter().any(|&v| v > 0 && v < 255), "anti-aliased");

        // Rebuilding from the parts is the identity.
        let back = CoverageMask::new(origin, w, h, coverage).unwrap();
        assert_eq!(back.bounds(), bounds);
    }

    /// Nothing in this crate is proportional to the canvas: a small shape
    /// far from the origin costs its own box and nothing else.
    #[test]
    fn a_small_shape_on_a_huge_canvas_allocates_only_its_own_box() {
        let s = shapes::circle(point(1_000_000.0, 1_000_000.0), 5.0);
        let m = fill(&s, &FillOptions::default()).unwrap();
        assert_eq!(m.coverage().len(), 100, "an 10x10 box, not a canvas");
        assert_eq!(m.origin(), IVec2::new(999_995, 999_995));
        assert!((m.area() - std::f64::consts::PI * 25.0).abs() < 0.5);
    }

    /// The "absurd extents" half of the crate-level promise, at the one
    /// magnitude where arithmetic on the coordinates themselves gives up:
    /// `1e308 - (-1e308)` is infinity, so every direction, length and normal
    /// derived from that segment is gone. Legal path data, straight out of a
    /// file, and not one entry point may abort on it.
    #[test]
    fn coordinates_whose_own_difference_overflows_are_answered_not_aborted() {
        let p = parse_svg("M-1e308 0 L1e308 0 L0 1e308 Z").unwrap();
        assert!(p.is_finite());
        assert!(p.control_bounds().width().is_infinite());

        for cap in [Cap::Butt, Cap::Round, Cap::Square] {
            for join in [Join::Miter, Join::Round, Join::Bevel] {
                let o = stroke(
                    &p,
                    &StrokeStyle {
                        cap,
                        join,
                        ..StrokeStyle::new(4.0)
                    },
                )
                .unwrap();
                assert!(o.is_finite(), "{cap:?}/{join:?}");
            }
        }

        // A mask that big is refused by the sample cap rather than allocated.
        assert!(matches!(
            fill(&p, &FillOptions::default()),
            Err(VectorError::RegionTooLarge { .. })
        ));

        // Hit testing, lengths and transforms all answer.
        let _ = contains(&p, point(0.0, 1.0), FillRule::NonZero);
        let _ = hit_stroke(&p, point(0.0, 1.0), 1.0);
        assert!(p.length().is_infinite());
        assert!(p.transform(&Affine::translate(1.0, 1.0)).is_finite());

        // And so do the combining operations, whether by answering or by
        // refusing - never by panicking.
        let small = shapes::circle(point(0.0, 0.0), 3.0);
        for op in [
            BoolOp::Union,
            BoolOp::Intersection,
            BoolOp::Difference,
            BoolOp::Xor,
        ] {
            let r = boolean::boolean(&p, &small, op, FillRule::NonZero, 0.1);
            assert!(r.is_ok(), "{op:?}: {r:?}");
        }
        assert!(matches!(
            dash(&p, &Dash::new(vec![4.0, 2.0]), 0.1),
            Err(VectorError::TooComplex { .. })
        ));
    }

    /// Every public entry point survives the degenerate inputs a real editor
    /// produces: an empty path, a single point, a zero-length segment.
    #[test]
    fn the_degenerate_cases_go_all_the_way_through_without_panicking() {
        let empty = Path::new();
        let mut dot = Path::new();
        dot.move_to(point(5.0, 5.0));
        let mut zero = Path::new();
        zero.move_to(point(1.0, 1.0))
            .line_to(point(1.0, 1.0))
            .close();

        for p in [&empty, &dot, &zero] {
            assert_eq!(fill(p, &FillOptions::default()).unwrap().area(), 0.0);
            assert!(!contains(p, point(1.0, 1.0), FillRule::NonZero));
            assert!(p.is_finite());
            assert!(stroke(p, &StrokeStyle::new(2.0)).unwrap().is_finite());
            assert_eq!(
                svg::parse(&svg::to_svg(p)).unwrap().subpaths().len(),
                p.subpaths().len()
            );
            assert!(union(p, &shapes::circle(point(0.0, 0.0), 3.0))
                .unwrap()
                .is_finite());
            assert!(dash(p, &Dash::new(vec![1.0, 1.0]), 0.1)
                .unwrap()
                .is_finite());
            assert_eq!(
                p.transform(&Affine::rotate(0.5)).subpaths().len(),
                p.subpaths().len()
            );
        }
    }
}
