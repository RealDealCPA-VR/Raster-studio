//! Shape layers: from a [`layer_model::ShapeLayer`]'s path to coverage.
//!
//! As with [`crate::text`], nothing here rasterises anything itself. `vector`
//! owns the scan converter, the fill rules, the stroker with its caps, joins,
//! miter limit and dashes, and the guarantee that no caller input panics; this
//! module parses the layer's stored path, scales it to the mip level, asks
//! `vector` for the two coverage masks a shape can have, and caches the answer.
//!
//! # Fill and stroke are one mask each, not one image
//!
//! Coverage is geometry, and geometry does not depend on the document's colour
//! space; the paint does. Caching the *masks* and colouring them per composite
//! is what keeps one cache correct for documents in different colour spaces,
//! and keeps the entry the size of two bytes per pixel rather than sixteen.
//!
//! # Known limits
//!
//! * A path whose coverage would exceed [`MAX_SHAPE_PIXELS`] is not drawn. That
//!   is 16 megapixels of mask — a shape four thousand pixels on a side — and
//!   the alternative is allocating a buffer sized by geometry a file can name
//!   but nobody can see.
//! * `vector` refuses malformed path data, and a shape whose `path_svg` does
//!   not parse contributes nothing rather than failing the frame.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use layer_model::{ShapeCap, ShapeFillRule, ShapeJoin, ShapeLayer, ShapeStroke};
use raster::PixelRect;
use vector::{Affine, Cap, Dash, FillOptions, FillRule, Join, Path, StrokeStyle};

/// Largest coverage buffer a single shape layer is rasterised into.
///
/// 2^24 pixels: one byte per pixel per mask, two masks, so 32 MiB at the
/// ceiling. Well inside [`crate::MAX_CANVAS_PIXELS`] on purpose — this is an
/// intermediate held once per shape layer, not a result a caller asked for.
pub const MAX_SHAPE_PIXELS: u64 = 1 << 24;

/// How many rasterised shapes are kept before the cache is dropped wholesale.
const MAX_CACHED_SHAPES: usize = 32;

/// The rasterised coverage of one shape layer, in its own pixel space at one
/// mip level.
///
/// Both slices are `rect.width * rect.height` bytes, row-major over `rect`, and
/// either may be empty when that half of the shape is not painted.
pub(crate) struct ShapeCoverage {
    pub rect: PixelRect,
    pub fill: Vec<u8>,
    pub stroke: Vec<u8>,
}

impl ShapeCoverage {
    fn is_empty(&self) -> bool {
        self.rect.is_empty() || (self.fill.is_empty() && self.stroke.is_empty())
    }
}

/// Everything about a shape that decides its coverage — the geometry and the
/// stroke's shape, but not either paint's colour.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ShapeKey {
    svg: String,
    level: u8,
    filled: bool,
    rule: u8,
    stroke: Option<StrokeKey>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct StrokeKey {
    width_bits: u32,
    cap: u8,
    join: u8,
    miter_bits: u32,
    dash_bits: Vec<u32>,
    offset_bits: u32,
}

fn cache() -> MutexGuard<'static, HashMap<ShapeKey, Arc<ShapeCoverage>>> {
    static CACHE: OnceLock<Mutex<HashMap<ShapeKey, Arc<ShapeCoverage>>>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn key_for(layer: &ShapeLayer, level: u8) -> ShapeKey {
    ShapeKey {
        svg: layer.path_svg.clone(),
        level,
        filled: layer.fill.is_some(),
        rule: match layer.fill_rule {
            ShapeFillRule::NonZero => 0,
            ShapeFillRule::EvenOdd => 1,
        },
        stroke: layer.stroke.as_ref().map(|s| StrokeKey {
            width_bits: s.width_px.to_bits(),
            cap: match s.cap {
                ShapeCap::Butt => 0,
                ShapeCap::Round => 1,
                ShapeCap::Square => 2,
            },
            join: match s.join {
                ShapeJoin::Miter => 0,
                ShapeJoin::Round => 1,
                ShapeJoin::Bevel => 2,
            },
            miter_bits: s.miter_limit.to_bits(),
            dash_bits: s.dash.iter().map(|v| v.to_bits()).collect(),
            offset_bits: s.dash_offset.to_bits(),
        }),
    }
}

/// The layer's coverage in its own space at `level`, or `None` when there is
/// nothing to draw.
pub(crate) fn coverage(layer: &ShapeLayer, level: u8) -> Option<Arc<ShapeCoverage>> {
    if !layer.is_drawable() {
        return None;
    }
    let key = key_for(layer, level);
    let mut cache = cache();
    if let Some(hit) = cache.get(&key) {
        return (!hit.is_empty()).then(|| Arc::clone(hit));
    }
    let built = Arc::new(rasterize(layer, level)?);
    if cache.len() >= MAX_CACHED_SHAPES {
        cache.clear();
    }
    cache.insert(key, Arc::clone(&built));
    (!built.is_empty()).then_some(built)
}

/// The rect the layer paints in its own space at `level`, empty when it paints
/// nothing.
pub(crate) fn ink_bounds(layer: &ShapeLayer, level: u8) -> PixelRect {
    coverage(layer, level).map_or(PixelRect::new(0, 0, 0, 0), |c| c.rect)
}

fn rasterize(layer: &ShapeLayer, level: u8) -> Option<ShapeCoverage> {
    let path = vector::parse_svg(&layer.path_svg).ok()?;
    // A transform is authored in level-0 pixels, and so is a path.
    let s = f64::from(2.0f32.powi(-(level as i32)));
    let path = if level == 0 {
        path
    } else {
        path.transform(&Affine::scale(s, s))
    };

    let rule = match layer.fill_rule {
        ShapeFillRule::NonZero => FillRule::NonZero,
        ShapeFillRule::EvenOdd => FillRule::EvenOdd,
    };
    let fill = layer
        .fill
        .is_some()
        .then(|| vector::fill(&path, &FillOptions::with_rule(rule)).ok())
        .flatten();
    let stroke = layer
        .stroke
        .as_ref()
        .and_then(|st| stroke_mask(&path, st, s));

    let rect = union(
        fill.as_ref().map(mask_rect).unwrap_or(EMPTY),
        stroke.as_ref().map(mask_rect).unwrap_or(EMPTY),
    );
    if rect.is_empty() || u64::from(rect.width) * u64::from(rect.height) > MAX_SHAPE_PIXELS {
        return None;
    }
    Some(ShapeCoverage {
        rect,
        fill: fill.map(|m| resample_into(&m, rect)).unwrap_or_default(),
        stroke: stroke.map(|m| resample_into(&m, rect)).unwrap_or_default(),
    })
}

/// The stroke's outline, rasterised. `s` scales document pixels to level ones.
fn stroke_mask(path: &Path, st: &ShapeStroke, s: f64) -> Option<vector::CoverageMask> {
    let width = f64::from(st.width_px) * s;
    if !width.is_finite() || width <= 0.0 {
        return None;
    }
    let dash = (!st.dash.is_empty()).then(|| Dash {
        pattern: st.dash.iter().map(|v| f64::from(*v) * s).collect(),
        offset: f64::from(st.dash_offset) * s,
    });
    let style = StrokeStyle {
        width,
        cap: match st.cap {
            ShapeCap::Butt => Cap::Butt,
            ShapeCap::Round => Cap::Round,
            ShapeCap::Square => Cap::Square,
        },
        join: match st.join {
            ShapeJoin::Miter => Join::Miter,
            ShapeJoin::Round => Join::Round,
            ShapeJoin::Bevel => Join::Bevel,
        },
        miter_limit: f64::from(st.miter_limit),
        dash,
        tolerance: vector::DEFAULT_TOLERANCE,
    };
    let outline = vector::stroke(path, &style).ok()?;
    // A stroke outline is a closed, positively-oriented region: it is filled
    // non-zero whatever rule the shape's *interior* uses, or a dash's two
    // overlapping caps would cancel each other out.
    vector::fill(&outline, &FillOptions::with_rule(FillRule::NonZero)).ok()
}

const EMPTY: PixelRect = PixelRect::new(0, 0, 0, 0);

fn mask_rect(m: &vector::CoverageMask) -> PixelRect {
    let o = m.origin();
    PixelRect::new(i64::from(o.x), i64::from(o.y), m.width(), m.height())
}

fn union(a: PixelRect, b: PixelRect) -> PixelRect {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    match (u32::try_from(x1 - x0), u32::try_from(y1 - y0)) {
        (Ok(w), Ok(h)) => PixelRect::new(x0, y0, w, h),
        _ => a,
    }
}

/// Copy a `vector` mask into a buffer covering `rect`, zero outside it.
fn resample_into(m: &vector::CoverageMask, rect: PixelRect) -> Vec<u8> {
    let src = mask_rect(m);
    let mut out = vec![0u8; rect.width as usize * rect.height as usize];
    let stride = m.width() as usize;
    let x0 = rect.x.max(src.x);
    let x1 = rect.right().min(src.right());
    let y0 = rect.y.max(src.y);
    let y1 = rect.bottom().min(src.bottom());
    for y in y0..y1 {
        let drow = (y - rect.y) as usize * rect.width as usize;
        let srow = (y - src.y) as usize * stride;
        for x in x0..x1 {
            out[drow + (x - rect.x) as usize] = m.coverage()[srow + (x - src.x) as usize];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> ShapeLayer {
        ShapeLayer::from_svg("M10 10 L40 10 L40 40 L10 40 Z")
    }

    #[test]
    fn a_filled_square_covers_its_interior_and_nothing_else() {
        let cov = coverage(&square(), 0).expect("coverage");
        assert_eq!(cov.rect, PixelRect::new(10, 10, 30, 30));
        assert!(cov.stroke.is_empty(), "no stroke asked for");
        let at = |x: i64, y: i64| {
            cov.fill
                [(y - cov.rect.y) as usize * cov.rect.width as usize + (x - cov.rect.x) as usize]
        };
        assert_eq!(at(25, 25), 255);
        assert_eq!(at(10, 10), 255, "the top-left pixel is fully inside");
        assert_eq!(at(39, 39), 255);
    }

    #[test]
    fn the_even_odd_rule_punches_a_hole_the_non_zero_rule_does_not() {
        // Two nested squares wound the same way: non-zero merges them, even-odd
        // makes the inner one a hole.
        let svg = "M0 0 L40 0 L40 40 L0 40 Z M10 10 L30 10 L30 30 L10 30 Z";
        let mut nonzero = ShapeLayer::from_svg(svg);
        nonzero.fill_rule = ShapeFillRule::NonZero;
        let mut evenodd = ShapeLayer::from_svg(svg);
        evenodd.fill_rule = ShapeFillRule::EvenOdd;

        let a = coverage(&nonzero, 0).unwrap();
        let b = coverage(&evenodd, 0).unwrap();
        let centre = |c: &ShapeCoverage| {
            c.fill[(20 - c.rect.y) as usize * c.rect.width as usize + (20 - c.rect.x) as usize]
        };
        assert_eq!(centre(&a), 255, "non-zero fills the middle");
        assert_eq!(centre(&b), 0, "even-odd leaves a hole");
    }

    #[test]
    fn a_stroke_lands_on_the_path_and_widens_the_covered_rect() {
        let mut s = square();
        s.fill = None;
        s.stroke = Some(ShapeStroke {
            width_px: 4.0,
            ..Default::default()
        });
        let cov = coverage(&s, 0).expect("coverage");
        assert!(cov.fill.is_empty());
        // Half the width either side of the path, so the rect grows by 2.
        assert_eq!(cov.rect, PixelRect::new(8, 8, 34, 34));
        let at = |x: i64, y: i64| {
            cov.stroke
                [(y - cov.rect.y) as usize * cov.rect.width as usize + (x - cov.rect.x) as usize]
        };
        assert_eq!(at(10, 25), 255, "on the left edge of the square");
        assert_eq!(at(25, 25), 0, "the interior is not stroked");
        assert_eq!(at(8, 8), 255, "the outer corner of the join");
    }

    #[test]
    fn a_dashed_stroke_leaves_gaps_a_solid_one_does_not() {
        let mut solid = square();
        solid.fill = None;
        solid.stroke = Some(ShapeStroke {
            width_px: 2.0,
            ..Default::default()
        });
        let mut dashed = solid.clone();
        dashed.stroke.as_mut().unwrap().dash = vec![4.0, 4.0];

        let ink = |s: &ShapeLayer| -> u64 {
            coverage(s, 0)
                .unwrap()
                .stroke
                .iter()
                .map(|v| u64::from(*v))
                .sum()
        };
        let (a, b) = (ink(&solid), ink(&dashed));
        assert!(b > 0, "a dashed stroke still draws");
        assert!(
            b < a * 3 / 4,
            "half on / half off must cost much less ink: {b} vs {a}"
        );
    }

    #[test]
    fn a_mip_level_scales_the_geometry_rather_than_the_output() {
        let full = coverage(&square(), 0).unwrap();
        let half = coverage(&square(), 1).unwrap();
        assert_eq!(full.rect, PixelRect::new(10, 10, 30, 30));
        assert_eq!(half.rect, PixelRect::new(5, 5, 15, 15));
    }

    #[test]
    fn unparseable_or_unpainted_shapes_draw_nothing_instead_of_failing() {
        assert!(coverage(&ShapeLayer::from_svg("not a path at all"), 0).is_none());
        assert!(coverage(&ShapeLayer::default(), 0).is_none(), "no geometry");
        let mut bare = square();
        bare.fill = None;
        assert!(coverage(&bare, 0).is_none(), "nothing to paint it with");
        // Geometry too large to rasterise is declined, not allocated.
        let huge = ShapeLayer::from_svg("M0 0 L100000 0 L100000 100000 Z");
        assert!(coverage(&huge, 0).is_none());
    }

    #[test]
    fn coverage_is_cached_by_geometry_not_by_colour() {
        let mut a = square();
        a.fill = Some([1.0, 0.0, 0.0, 1.0]);
        let mut b = square();
        b.fill = Some([0.0, 1.0, 0.0, 1.0]);
        assert!(
            Arc::ptr_eq(&coverage(&a, 0).unwrap(), &coverage(&b, 0).unwrap()),
            "two colours of one shape share one rasterisation"
        );
        // A different stroke is different geometry, so a different entry.
        let mut c = a.clone();
        c.stroke = Some(ShapeStroke::default());
        assert!(!Arc::ptr_eq(
            &coverage(&a, 0).unwrap(),
            &coverage(&c, 0).unwrap()
        ));
    }
}
