//! Transform a selection by an affine map, resampling its coverage.
//!
//! Coverage is alpha: a linear fraction of a pixel. So resampling it is the
//! same problem as resampling an alpha channel and has the same two rules —
//! filter in linear units (which coverage already is, so there is no transfer
//! curve to undo and nothing to premultiply, since there is no colour to
//! premultiply *into*), and **prefilter when minifying**. Skipping the second
//! is the usual bug: a bilinear tap per destination pixel throws away the
//! source pixels it steps over, so shrinking a fine pattern turns it into
//! aliased noise instead of the grey it should average to.
//!
//! # Prefiltering holds at every scale, not just gentle ones
//! A fixed number of sub-samples per destination pixel is only a prefilter
//! while the sub-samples are still about a source pixel apart. Past that they
//! are a *sparser* comb than the source itself, which is worse than useless: at
//! 32x, eight sub-positions land four source pixels apart, and against a
//! four-pixel-period pattern they can sit entirely on the unselected phase and
//! delete the whole selection. So the sub-sampled path is used only up to
//! [`MAX_PREFILTER`] source pixels per destination pixel; past that the
//! destination pixel's source footprint is box-averaged **exactly**, from a
//! [`SummedArea`] table, in constant time per sample however extreme the
//! minification. Pinned by `minification_conserves_coverage_at_every_scale`.
//!
//! The destination rectangle is the transformed bounding box, so nothing here
//! is proportional to the canvas either.
//!
//! # Everything is resampled in a local frame
//! Like the marquee and the lasso (see `marquee::localise`), the resampling
//! loop never puts a document coordinate into an `f32`. At `x = 2^24` the gap
//! between neighbouring `f32` values is already two pixels, so a half-pixel
//! shift would round away and a rotation would tear; the crate's working grid
//! runs to `±2^30`. Instead the destination rect's origin and the source rect's
//! origin are folded into the inverse transform once, in `f64`, and the inner
//! loop only ever sees offsets inside the two rectangles. Pinned by
//! `a_transform_rasterises_identically_wherever_it_is_placed`.

use editor_core::{Selection, SelectionMask};
use glam::{Affine2, DVec2, IVec2, Vec2};
use serde::{Deserialize, Serialize};

use crate::buf::{to_byte, CoverageBuf};
use crate::error::SelectionOpError;
use crate::rect::{clamp_coord, Rect};

/// How a transformed selection samples its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ResampleFilter {
    /// Nearest neighbour. Keeps a binary selection binary, at the cost of
    /// jagged edges; the right choice when the transform is a whole-pixel
    /// translation and the caller wants an exact copy.
    Nearest,
    /// Bilinear, with box prefiltering when the transform minifies.
    #[default]
    Bilinear,
}

/// Sub-samples per axis used to prefilter a *mildly* minifying transform.
///
/// Not a cap on how much minification is filtered — it is the point at which
/// sub-sampling stops being the right tool and the summed-area box average
/// takes over. Eight sub-positions resolve a footprint eight source pixels
/// wide; a wider one is handled exactly instead. See the module docs.
const MAX_PREFILTER: i32 = 8;

/// A summed-area table over a coverage buffer: the mean coverage of any
/// axis-aligned box, in constant time.
///
/// `sums[y][x]` is the total of every sample above and to the left of the
/// *corner* `(x, y)`, so the table is one row and one column larger than the
/// buffer and a box mass is the usual four-corner combination. Corners are
/// looked up bilinearly, which makes a box with fractional edges — the normal
/// case, since a destination pixel's footprint does not land on source pixel
/// boundaries — exact rather than snapped.
///
/// Sums are `u64`: a table over the largest legal mask holds `255 * 2^32`,
/// which overflows `u32` and loses whole coverage levels in `f32`.
struct SummedArea {
    w: usize,
    h: usize,
    sums: Vec<u64>,
}

impl SummedArea {
    fn build(src: &CoverageBuf) -> Result<Self, SelectionOpError> {
        let (w, h) = (src.width(), src.height());
        let stride = w + 1;
        let n = stride
            .checked_mul(h + 1)
            .ok_or(SelectionOpError::RegionTooLarge {
                width: src.rect().width(),
                height: src.rect().height(),
            })?;
        let mut sums = crate::buf::alloc_vec(n, 0u64)?;
        for y in 0..h {
            let row = src.row(y);
            let mut run = 0u64;
            for x in 0..w {
                run += row[x] as u64;
                sums[(y + 1) * stride + x + 1] = sums[y * stride + x + 1] + run;
            }
        }
        Ok(Self { w, h, sums })
    }

    /// The cumulative sum at a fractional corner, clamped to the table.
    fn corner(&self, x: f64, y: f64) -> f64 {
        let stride = self.w + 1;
        let cx = x.clamp(0.0, self.w as f64);
        let cy = y.clamp(0.0, self.h as f64);
        let (x0, y0) = (cx.floor(), cy.floor());
        let (fx, fy) = (cx - x0, cy - y0);
        let (xi, yi) = (x0 as usize, y0 as usize);
        let (xj, yj) = ((xi + 1).min(self.w), (yi + 1).min(self.h));
        let s = |a: usize, b: usize| self.sums[b * stride + a] as f64;
        let top = s(xi, yi) + (s(xj, yi) - s(xi, yi)) * fx;
        let bot = s(xi, yj) + (s(xj, yj) - s(xi, yj)) * fx;
        top + (bot - top) * fy
    }

    /// Mean coverage over a box in the source buffer's own frame.
    ///
    /// The mass is clamped to the table but the **area is not**: everything
    /// outside the source is unselected, so a footprint hanging off the edge
    /// correctly averages down rather than reporting the mean of the part that
    /// happens to overlap. That is what keeps the total coverage conserved.
    fn box_mean(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> f32 {
        let area = (x1 - x0) * (y1 - y0);
        // A degenerate or non-finite footprint selects nothing rather than
        // dividing by zero; `is_finite` also excludes the NaN case.
        if !area.is_finite() || area <= 0.0 {
            return 0.0;
        }
        let mass =
            self.corner(x1, y1) - self.corner(x0, y1) - self.corner(x1, y0) + self.corner(x0, y0);
        ((mass / area) / 255.0).clamp(0.0, 1.0) as f32
    }
}

/// Coverage of one sample of `src`, addressed **relative to `src`'s own
/// origin**; 0 outside it.
fn coverage_at(src: &CoverageBuf, x: i64, y: i64) -> f32 {
    if x < 0 || y < 0 || x >= src.width() as i64 || y >= src.height() as i64 {
        return 0.0;
    }
    src.data()[y as usize * src.width() + x as usize] as f32 / 255.0
}

/// Sample at a continuous position in the source buffer's local frame; pixel
/// centres are at integer+0.5.
fn sample(src: &CoverageBuf, p: Vec2, filter: ResampleFilter) -> f32 {
    if !p.x.is_finite() || !p.y.is_finite() {
        return 0.0;
    }
    match filter {
        ResampleFilter::Nearest => coverage_at(src, p.x.floor() as i64, p.y.floor() as i64),
        ResampleFilter::Bilinear => {
            let u = p.x - 0.5;
            let v = p.y - 0.5;
            let x0 = u.floor();
            let y0 = v.floor();
            let fx = u - x0;
            let fy = v - y0;
            let (xi, yi) = (x0 as i64, y0 as i64);
            let c00 = coverage_at(src, xi, yi);
            let c10 = coverage_at(src, xi + 1, yi);
            let c01 = coverage_at(src, xi, yi + 1);
            let c11 = coverage_at(src, xi + 1, yi + 1);
            let top = c00 + (c10 - c00) * fx;
            let bot = c01 + (c11 - c01) * fx;
            top + (bot - top) * fy
        }
    }
}

/// Apply an affine transform to a selection mask.
///
/// The transform maps *source document coordinates to destination document
/// coordinates*, so `Affine2::from_translation(Vec2::new(3.0, 0.0))` moves the
/// selection three pixels right. A singular transform collapses the selection
/// to nothing rather than producing infinities.
pub fn transform(
    mask: &SelectionMask,
    xf: Affine2,
    filter: ResampleFilter,
) -> Result<SelectionMask, SelectionOpError> {
    let src = CoverageBuf::from_mask(mask)?;
    let content = src.trimmed()?;
    let rect = content.rect();
    if rect.is_empty() {
        return content.into_mask();
    }
    for v in [
        xf.matrix2.x_axis.x,
        xf.matrix2.x_axis.y,
        xf.matrix2.y_axis.x,
        xf.matrix2.y_axis.y,
        xf.translation.x,
        xf.translation.y,
    ] {
        if !v.is_finite() {
            return Err(SelectionOpError::NotFinite {
                what: "selection transform",
                value: v,
            });
        }
    }
    // Column-major, in `f64`: every entry converts exactly, and the products
    // below reach document scale even when the matrix itself does not.
    let (a, b) = (xf.matrix2.x_axis.x as f64, xf.matrix2.x_axis.y as f64);
    let (c, d) = (xf.matrix2.y_axis.x as f64, xf.matrix2.y_axis.y as f64);
    let (tx, ty) = (xf.translation.x as f64, xf.translation.y as f64);
    let det = a * d - c * b;
    if det.abs() < 1e-12 {
        return Ok(SelectionMask::new(rect.min(), 0, 0, Vec::new())?);
    }

    // The destination bounding box, also in f64: at 2^29 an f32 corner is only
    // accurate to 64 pixels, which would clip the shape it is supposed to hold.
    let forward = |p: DVec2| DVec2::new(a * p.x + c * p.y + tx, b * p.x + d * p.y + ty);
    let (lo, hi) = (rect.min(), rect.max());
    let corners = [
        forward(DVec2::new(lo.x as f64, lo.y as f64)),
        forward(DVec2::new(hi.x as f64, lo.y as f64)),
        forward(DVec2::new(lo.x as f64, hi.y as f64)),
        forward(DVec2::new(hi.x as f64, hi.y as f64)),
    ];
    let mut dmin = corners[0];
    let mut dmax = corners[0];
    for p in corners {
        dmin = dmin.min(p);
        dmax = dmax.max(p);
    }
    // One pixel of slack so a bilinear tap at the very edge is not clipped.
    let dst_rect = Rect::new(
        IVec2::new(
            clamp_coord(dmin.x.floor() as i64),
            clamp_coord(dmin.y.floor() as i64),
        ),
        IVec2::new(
            clamp_coord(dmax.x.ceil() as i64),
            clamp_coord(dmax.y.ceil() as i64),
        ),
    )
    .inflate(1);
    if dst_rect.is_empty() {
        return Ok(SelectionMask::new(dst_rect.min(), 0, 0, Vec::new())?);
    }

    // The inverse's linear part. These entries are of transform scale, never of
    // document scale, so `f32` holds them as well as it holds the matrix.
    let (inv_a64, inv_c64) = (d / det, -c / det);
    let (inv_b64, inv_d64) = (-b / det, a / det);
    let inv_a = inv_a64 as f32;
    let inv_c = inv_c64 as f32;
    let inv_b = inv_b64 as f32;
    let inv_d = inv_d64 as f32;

    // ...and its constant part, which is where the two origins go. This is the
    // source-local position that the destination rect's own corner maps back
    // to, so it is bounded by the source rect's size however far from the
    // origin the pair of them sit. Computed once, in f64.
    let src_o = content.rect().min();
    let dst_o = dst_rect.min();
    let (ox, oy) = (dst_o.x as f64 - tx, dst_o.y as f64 - ty);
    let const_x64 = (d * ox - c * oy) / det - src_o.x as f64;
    let const_y64 = (-b * ox + a * oy) / det - src_o.y as f64;
    let const_x = const_x64 as f32;
    let const_y = const_y64 as f32;

    // How many source pixels one destination pixel spans; more than one means
    // the transform minifies and a single tap would alias.
    let span = Vec2::new(inv_a, inv_b)
        .length()
        .max(Vec2::new(inv_c, inv_d).length())
        .max(1.0);

    let mut out = CoverageBuf::zeroed(dst_rect)?;

    // Past `MAX_PREFILTER` source pixels per destination pixel the sub-sample
    // comb is sparser than the source and can miss a fine pattern entirely, so
    // the footprint is box-averaged exactly instead. The box is the footprint's
    // axis-aligned bounding box; for a rotation that is wider than the true
    // parallelogram, which over-blurs slightly but still conserves total
    // coverage, because the boxes stay on the destination lattice.
    if filter == ResampleFilter::Bilinear && span > MAX_PREFILTER as f32 {
        let sat = SummedArea::build(&content)?;
        let hx = 0.5 * (inv_a64.abs() + inv_c64.abs());
        let hy = 0.5 * (inv_b64.abs() + inv_d64.abs());
        for y in 0..dst_rect.height() as usize {
            let py = y as f64 + 0.5;
            let row = out.row_mut(y);
            for (x, o) in row.iter_mut().enumerate() {
                let px = x as f64 + 0.5;
                let sx = inv_a64 * px + inv_c64 * py + const_x64;
                let sy = inv_b64 * px + inv_d64 * py + const_y64;
                *o = to_byte(sat.box_mean(sx - hx, sy - hy, sx + hx, sy + hy));
            }
        }
        return out.into_mask();
    }

    let k = if filter == ResampleFilter::Nearest {
        1
    } else {
        (span.ceil() as i64).clamp(1, MAX_PREFILTER as i64) as i32
    };

    let inv_k = 1.0 / k as f32;
    let weight = 1.0 / (k * k) as f32;

    for y in 0..dst_rect.height() as usize {
        let ly = y as f32;
        let row = out.row_mut(y);
        for (x, o) in row.iter_mut().enumerate() {
            let lx = x as f32;
            let mut acc = 0.0f32;
            for sy in 0..k {
                for sx in 0..k {
                    // Destination position in the destination rect's own frame.
                    let px = lx + (sx as f32 + 0.5) * inv_k;
                    let py = ly + (sy as f32 + 0.5) * inv_k;
                    // ...mapped straight into the source buffer's own frame.
                    let s = Vec2::new(
                        inv_a * px + inv_c * py + const_x,
                        inv_b * px + inv_d * py + const_y,
                    );
                    acc += sample(&content, s, filter);
                }
            }
            *o = to_byte(acc * weight);
        }
    }
    out.into_mask()
}

/// [`transform`] for a document selection.
///
/// [`Selection::None`] has no geometry to move — every pixel is selected
/// wherever the transform puts it — so it is returned unchanged.
pub fn transform_selection(
    sel: &Selection,
    canvas: Rect,
    xf: Affine2,
    filter: ResampleFilter,
) -> Result<Selection, SelectionOpError> {
    if sel.is_none() {
        return Ok(sel.clone());
    }
    let mask = crate::boolean::to_mask(sel, canvas)?;
    Ok(Selection::Mask(transform(&mask, xf, filter)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marquee::{ellipse, rectangle};
    use glam::Mat2;

    fn total(m: &SelectionMask) -> f64 {
        m.coverage().iter().map(|&v| v as f64 / 255.0).sum()
    }

    #[test]
    fn the_identity_transform_returns_the_same_coverage() {
        let src = ellipse(Rect::from_xywh(3, 4, 17, 11)).unwrap();
        for filter in [ResampleFilter::Nearest, ResampleFilter::Bilinear] {
            let out = transform(&src, Affine2::IDENTITY, filter).unwrap();
            assert_eq!(out.bounds(), src.bounds(), "{filter:?}");
            for y in 4..15 {
                for x in 3..20 {
                    let p = IVec2::new(x, y);
                    assert_eq!(
                        out.coverage_at(p),
                        src.coverage_at(p),
                        "{filter:?} at {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_whole_pixel_translation_is_exact() {
        let src = ellipse(Rect::from_xywh(0, 0, 15, 15)).unwrap();
        let d = IVec2::new(7, -3);
        for filter in [ResampleFilter::Nearest, ResampleFilter::Bilinear] {
            let out = transform(
                &src,
                Affine2::from_translation(Vec2::new(d.x as f32, d.y as f32)),
                filter,
            )
            .unwrap();
            assert_eq!(
                out.bounds(),
                src.bounds().map(|(a, b)| (a + d, b + d)),
                "{filter:?}"
            );
            for y in 0..15 {
                for x in 0..15 {
                    let p = IVec2::new(x, y);
                    assert_eq!(
                        out.coverage_at(p + d),
                        src.coverage_at(p),
                        "{filter:?} at {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_quarter_turn_is_exact() {
        // Exact 0/1 matrix entries, so this checks the resampler and not the
        // rounding of cos(pi/2).
        let src = rectangle(Rect::from_xywh(0, 0, 4, 2)).unwrap();
        let rot = Affine2::from_mat2(Mat2::from_cols(Vec2::new(0.0, 1.0), Vec2::new(-1.0, 0.0)));
        let out = transform(&src, rot, ResampleFilter::Bilinear).unwrap();
        assert_eq!(out.bounds(), Some((IVec2::new(-2, 0), IVec2::new(0, 4))));
        assert!(out.coverage().iter().all(|&v| v == 255));
        assert_eq!(total(&out), 8.0);
    }

    #[test]
    fn scaling_up_multiplies_the_selected_area() {
        let src = rectangle(Rect::from_xywh(0, 0, 8, 8)).unwrap();
        let out = transform(
            &src,
            Affine2::from_scale(Vec2::splat(3.0)),
            ResampleFilter::Bilinear,
        )
        .unwrap();
        let area = total(&out);
        assert!(
            (area - 576.0).abs() < 30.0,
            "8x8 scaled 3x should cover ~576 px, got {area}"
        );
        assert_eq!(out.coverage_at(IVec2::new(12, 12)), 255);
        assert_eq!(out.coverage_at(IVec2::new(25, 12)), 0);
    }

    #[test]
    fn minifying_prefilters_instead_of_aliasing() {
        // One selected column in every four, shrunk 4x. A quarter of the area
        // is selected, so every output pixel must come out at a quarter
        // coverage. A single bilinear tap per output pixel lands between two
        // *unselected* columns every time and reports zero — a whole selection
        // silently deleted, which is exactly the failure prefiltering exists to
        // prevent. A checkerboard would not catch it: a single tap at a
        // half-integer position averages its four neighbours and accidentally
        // gives the right answer.
        let rect = Rect::from_xywh(0, 0, 64, 64);
        let mut b = CoverageBuf::zeroed(rect).unwrap();
        for y in 0..64 {
            for x in (0..64).step_by(4) {
                b.set(IVec2::new(x, y), 255);
            }
        }
        let src = b.into_mask().unwrap();
        let out = transform(
            &src,
            Affine2::from_scale(Vec2::splat(0.25)),
            ResampleFilter::Bilinear,
        )
        .unwrap();
        assert!(!out.is_empty(), "the whole selection was aliased away");
        // Interior samples, away from the one-pixel edge slack.
        for y in 3..13 {
            for x in 3..13 {
                let v = out.coverage_at(IVec2::new(x, y));
                assert!(
                    (54..=74).contains(&v),
                    "aliased at {x},{y}: expected ~64, got {v}"
                );
            }
        }
        // And the total coverage is conserved: 1/16 of the area, same mass.
        let before = total(&src);
        let after = total(&out);
        assert!(
            (after - before / 16.0).abs() / (before / 16.0) < 0.05,
            "{before} / 16 vs {after}"
        );
    }

    /// The same fine pattern, shrunk far past the point where a fixed number
    /// of sub-samples can resolve it.
    ///
    /// With eight sub-positions per axis and nothing beyond, 1/32 puts them
    /// exactly four source pixels apart — the period of this pattern — so every
    /// one of them lands on an unselected column and the whole selection comes
    /// back empty, and 1/64 aliases the other way and comes back at twice the
    /// mass. Coverage is a *quantity*: shrinking by `s` must scale the total by
    /// `s * s` at every `s`, not only at the gentle ones.
    #[test]
    fn minification_conserves_coverage_at_every_scale() {
        let rect = Rect::from_xywh(0, 0, 256, 256);
        let mut b = CoverageBuf::zeroed(rect).unwrap();
        for y in 0..256 {
            for x in (0..256).step_by(4) {
                b.set(IVec2::new(x, y), 255);
            }
        }
        let src = b.into_mask().unwrap();
        let before = total(&src);
        assert!((before - 16384.0).abs() < 1.0, "fixture mass is {before}");

        for s in [1.0f32 / 16.0, 1.0 / 32.0, 1.0 / 64.0] {
            let out = transform(
                &src,
                Affine2::from_scale(Vec2::splat(s)),
                ResampleFilter::Bilinear,
            )
            .unwrap();
            assert!(!out.is_empty(), "scale {s}: the whole selection was lost");
            let expected = before * (s * s) as f64;
            let after = total(&out);
            assert!(
                (after - expected).abs() / expected < 0.05,
                "scale {s}: {after} covered, but shrinking {before} by {s} must leave {expected}"
            );
            // ...and it is the *mean* everywhere, not a few surviving stripes.
            let (lo, hi) = out.bounds().unwrap();
            for y in lo.y + 1..hi.y - 1 {
                for x in lo.x + 1..hi.x - 1 {
                    let v = out.coverage_at(IVec2::new(x, y));
                    assert!(
                        (58..=70).contains(&v),
                        "scale {s}: aliased at {x},{y}: expected ~64, got {v}"
                    );
                }
            }
        }
    }

    /// The box-average path has to hold for transforms that are not a uniform
    /// shrink too: one that minifies hard on one axis while leaving the other
    /// alone (its footprint is a quarter of a pixel tall, so the box has to
    /// cope with edges inside a single source pixel), and one that rotates as
    /// it shrinks (its footprint is a parallelogram, and the box that bounds it
    /// overlaps its neighbours — which is fine for the total only because the
    /// boxes stay on the destination lattice).
    #[test]
    fn heavy_minification_conserves_coverage_when_anisotropic_or_rotated() {
        let src = ellipse(Rect::from_xywh(0, 0, 129, 129)).unwrap();
        let before = total(&src);

        let squashed = transform(
            &src,
            Affine2::from_scale(Vec2::new(1.0 / 32.0, 1.0)),
            ResampleFilter::Bilinear,
        )
        .unwrap();
        let expected = before / 32.0;
        let after = total(&squashed);
        assert!(
            (after - expected).abs() / expected < 0.05,
            "squashed 32x on one axis: {after} vs {expected}"
        );

        let turned = transform(
            &src,
            Affine2::from_scale_angle_translation(Vec2::splat(1.0 / 16.0), 0.7, Vec2::ZERO),
            ResampleFilter::Bilinear,
        )
        .unwrap();
        let expected = before / 256.0;
        let after = total(&turned);
        assert!(
            (after - expected).abs() / expected < 0.06,
            "rotated and shrunk 16x: {after} vs {expected}"
        );

        // A quarter turn while shrinking: the inverse's diagonal entries are
        // exactly zero, so the footprint's extent lives entirely in the *cross*
        // terms. A box built from the diagonal alone is a box of zero width,
        // and the whole selection would come back empty.
        let quarter = Affine2::from_mat2(Mat2::from_cols(
            Vec2::new(0.0, 1.0 / 16.0),
            Vec2::new(-1.0 / 16.0, 0.0),
        ));
        let sideways = transform(&src, quarter, ResampleFilter::Bilinear).unwrap();
        assert!(!sideways.is_empty(), "a quarter turn shrank to nothing");
        let after = total(&sideways);
        assert!(
            (after - expected).abs() / expected < 0.06,
            "quarter-turned and shrunk 16x: {after} vs {expected}"
        );
        // A 129-px disc shrunk 16x is an 8-px disc, so its middle is solid.
        let (lo, hi) = sideways.bounds().unwrap();
        let mid = (lo + hi) / 2;
        assert_eq!(
            sideways.coverage_at(mid),
            255,
            "the middle of the shrunk disc is not solid"
        );
    }

    #[test]
    fn nearest_neighbour_keeps_a_binary_selection_binary() {
        let src = rectangle(Rect::from_xywh(0, 0, 9, 9)).unwrap();
        let out = transform(&src, Affine2::from_angle(0.4), ResampleFilter::Nearest).unwrap();
        assert!(
            out.coverage().iter().all(|&v| v == 0 || v == 255),
            "nearest must not invent intermediate coverage"
        );
        // Bilinear on the same rotation does produce a soft edge.
        let smooth = transform(&src, Affine2::from_angle(0.4), ResampleFilter::Bilinear).unwrap();
        assert!(smooth.coverage().iter().any(|&v| v > 0 && v < 255));
    }

    #[test]
    fn a_rotation_preserves_the_selected_area() {
        let src = ellipse(Rect::from_xywh(0, 0, 41, 41)).unwrap();
        let before = total(&src);
        for angle in [0.3f32, 0.9, 2.0, -1.1] {
            let out =
                transform(&src, Affine2::from_angle(angle), ResampleFilter::Bilinear).unwrap();
            let after = total(&out);
            assert!(
                (after - before).abs() / before < 0.02,
                "angle {angle}: {before} -> {after}"
            );
        }
    }

    /// The sibling of `marquee::a_subpixel_shape_rasterises_identically_wherever_it_is_placed`
    /// and `lasso::a_polygon_rasterises_identically_wherever_it_is_placed`.
    /// Resampling in absolute document coordinates loses a half-pixel shift
    /// entirely once the `f32` ulp exceeds a pixel — 2 px at 2^24, 64 px at
    /// 2^29 — and the crate's working grid runs to 2^30.
    #[test]
    fn a_transform_rasterises_identically_wherever_it_is_placed() {
        let shift = Affine2::from_translation(Vec2::new(0.5, 0.25));
        let base = transform(
            &rectangle(Rect::from_xywh(0, 0, 8, 8)).unwrap(),
            shift,
            ResampleFilter::Bilinear,
        )
        .unwrap();
        assert!(
            base.coverage().iter().any(|&v| v > 0 && v < 255),
            "the fixture needs sub-pixel edges to lose"
        );

        for far in [1i32 << 22, 1 << 26, 1 << 29] {
            let out = transform(
                &rectangle(Rect::from_xywh(far, far, 8, 8)).unwrap(),
                shift,
                ResampleFilter::Bilinear,
            )
            .unwrap();
            assert_eq!(
                out.coverage(),
                base.coverage(),
                "the same transform at {far} resampled differently"
            );
            assert_eq!(
                out.origin(),
                base.origin() + IVec2::splat(far),
                "and it must land exactly where it was sent"
            );
        }
    }

    #[test]
    fn a_half_pixel_shift_splits_the_edge_column_at_every_document_position() {
        let shift = Affine2::from_translation(Vec2::new(0.5, 0.0));
        for o in [0i32, 1 << 16, 1 << 24, 1 << 26, 1 << 28] {
            let out = transform(
                &rectangle(Rect::from_xywh(o, o, 8, 8)).unwrap(),
                shift,
                ResampleFilter::Bilinear,
            )
            .unwrap();
            let y = o + 3;
            assert_eq!(
                out.coverage_at(IVec2::new(o, y)),
                128,
                "at {o}: the left edge column should be half covered"
            );
            assert_eq!(
                out.coverage_at(IVec2::new(o + 8, y)),
                128,
                "at {o}: the right edge column should be half covered"
            );
            assert_eq!(out.coverage_at(IVec2::new(o + 4, y)), 255, "at {o}");
            let area = total(&out);
            assert!(
                (area - 64.0).abs() < 0.5,
                "at {o}: a shifted 8x8 still covers 64 px, got {area}"
            );
        }
    }

    #[test]
    fn a_rotation_about_a_distant_shapes_own_centre_keeps_the_shape() {
        // A quarter turn about (2^24, 2^24), where an f32 document coordinate
        // is only accurate to two pixels. Every entry of this affine is exact,
        // so anything lost here is lost by the resampler.
        let c = 1i32 << 24;
        let rot = Affine2::from_mat2_translation(
            Mat2::from_cols(Vec2::new(0.0, 1.0), Vec2::new(-1.0, 0.0)),
            Vec2::new((2 * c) as f32, 0.0),
        );
        let src = rectangle(Rect::from_xywh(c - 4, c - 4, 8, 8)).unwrap();
        let out = transform(&src, rot, ResampleFilter::Bilinear).unwrap();
        assert_eq!(
            out.bounds(),
            src.bounds(),
            "the square left its own footprint"
        );
        assert_eq!(
            total(&out),
            64.0,
            "a square turned a quarter about its own centre is the same square"
        );
        assert!(out.coverage().iter().all(|&v| v == 255));
    }

    #[test]
    fn a_degenerate_transform_yields_nothing_rather_than_infinities() {
        let src = rectangle(Rect::from_xywh(0, 0, 4, 4)).unwrap();
        let flat = Affine2::from_mat2(Mat2::from_cols(Vec2::new(1.0, 0.0), Vec2::new(2.0, 0.0)));
        assert!(transform(&src, flat, ResampleFilter::Bilinear)
            .unwrap()
            .is_empty());
        assert!(matches!(
            transform(
                &src,
                Affine2::from_translation(Vec2::new(f32::NAN, 0.0)),
                ResampleFilter::Bilinear
            ),
            Err(SelectionOpError::NotFinite { .. })
        ));
    }

    #[test]
    fn transforming_no_selection_leaves_it_as_no_selection() {
        let out = transform_selection(
            &Selection::None,
            Rect::from_xywh(0, 0, 8, 8),
            Affine2::from_translation(Vec2::new(2.0, 2.0)),
            ResampleFilter::Bilinear,
        )
        .unwrap();
        assert!(out.is_none(), "everything, moved, is still everything");
    }
}
