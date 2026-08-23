//! The rasteriser's output: a rectangle of 8-bit per-pixel coverage.
//!
//! # Why coverage and not colour
//! A fill, a stroke, a vector mask and a "make a selection from this path" all
//! ask the rasteriser the same question — *how much of this pixel is inside the
//! shape?* — and differ only in what they do with the answer. Producing
//! coverage keeps that one question in one place. Writing colour here would
//! force the rasteriser to know about blend modes, colour spaces and
//! premultiplication, and would have grown a second, subtly different scan
//! converter the first time a vector mask needed one.
//!
//! # Layout compatibility
//! [`CoverageMask`] is deliberately the same four fields, in the same order,
//! with the same row-major byte layout and the same invariants as
//! `editor_core::SelectionMask` and the buffers `selection` works in. Handing a
//! rasterised path to the selection engine is [`CoverageMask::into_parts`]
//! followed by that crate's constructor — no resampling, no reinterpretation,
//! no copy of anything but the bytes. This crate does not depend on either of
//! them, because `vector` is a leaf domain crate: the dependency would run the
//! wrong way through the layering and would drag the document model into text
//! outlines and shape primitives.

use glam::IVec2;
use serde::{Deserialize, Serialize};

use crate::error::VectorError;
use crate::point::{Bounds, Point};

/// The largest coordinate a pixel rectangle may carry.
///
/// Far below `i32::MAX`, and for the same reason the selection engine clamps:
/// rasterisation *grows* rectangles (a stroke pads by half its width, a fill
/// rounds bounds outward), and those additions have to be total. Two clamped
/// coordinates differ by at most `2^31`, which fits an `i64` sum with room to
/// spare.
pub const COORD_LIMIT: i32 = 1 << 30;

/// The largest coverage buffer this crate will produce: 2^32 samples.
///
/// The cap is what makes the size check bite at all. On a 64-bit target a
/// `u32 * u32` product always fits a `usize`, so without it a path whose bounds
/// were a billion pixels square would be accepted and the allocation would
/// abort the process rather than return an error.
pub const MAX_MASK_SAMPLES: u64 = 1 << 32;

/// A half-open rectangle of whole pixels.
///
/// `min` is the first covered pixel and `max` is one past the last, matching
/// `editor_core::Selection::bounds` and `selection::Rect`, so a rectangle can
/// cross the crate boundary without a convention change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRect {
    min: IVec2,
    max: IVec2,
}

fn clamp_coord(v: i64) -> i32 {
    v.clamp(-(COORD_LIMIT as i64), COORD_LIMIT as i64) as i32
}

impl PixelRect {
    /// The empty rectangle at the origin.
    pub const EMPTY: PixelRect = PixelRect {
        min: IVec2::ZERO,
        max: IVec2::ZERO,
    };

    /// Clamp both corners into the working grid and normalise an inside-out
    /// rectangle to an empty one.
    pub fn new(min: IVec2, max: IVec2) -> Self {
        let min = IVec2::new(clamp_coord(min.x as i64), clamp_coord(min.y as i64));
        let max = IVec2::new(clamp_coord(max.x as i64), clamp_coord(max.y as i64));
        Self {
            min,
            max: IVec2::new(max.x.max(min.x), max.y.max(min.y)),
        }
    }

    /// A rectangle from a corner and a size.
    pub fn from_xywh(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self::new(
            IVec2::new(x, y),
            IVec2::new(
                clamp_coord(x as i64 + w as i64),
                clamp_coord(y as i64 + h as i64),
            ),
        )
    }

    /// The smallest whole-pixel rectangle containing a floating-point box.
    ///
    /// Outward rounding, always: a shape that touches a pixel at all has to be
    /// inside the rectangle the rasteriser allocates, or its edge is clipped
    /// away and the result is one pixel short on two sides.
    pub fn enclosing(b: Bounds) -> Self {
        if b.is_empty() || !b.min.is_finite() || !b.max.is_finite() {
            return Self::EMPTY;
        }
        let min = IVec2::new(
            clamp_coord(b.min.x.floor() as i64),
            clamp_coord(b.min.y.floor() as i64),
        );
        let max = IVec2::new(
            clamp_coord(b.max.x.ceil() as i64),
            clamp_coord(b.max.y.ceil() as i64),
        );
        Self::new(min, max)
    }

    /// First covered pixel.
    pub fn min(&self) -> IVec2 {
        self.min
    }

    /// One past the last covered pixel.
    pub fn max(&self) -> IVec2 {
        self.max
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        (self.max.x as i64 - self.min.x as i64) as u32
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        (self.max.y as i64 - self.min.y as i64) as u32
    }

    /// Pixel count, exactly, in `u64` so it cannot overflow.
    pub fn area(&self) -> u64 {
        self.width() as u64 * self.height() as u64
    }

    /// `true` when the rectangle covers no pixel.
    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// `true` when `p` is one of the covered pixels.
    pub fn contains(&self, p: IVec2) -> bool {
        p.x >= self.min.x && p.x < self.max.x && p.y >= self.min.y && p.y < self.max.y
    }

    /// The overlap of two rectangles.
    pub fn intersection(self, other: PixelRect) -> PixelRect {
        PixelRect::new(
            IVec2::new(self.min.x.max(other.min.x), self.min.y.max(other.min.y)),
            IVec2::new(self.max.x.min(other.max.x), self.max.y.min(other.max.y)),
        )
    }

    /// This rectangle as a floating-point box.
    pub fn to_bounds(self) -> Bounds {
        if self.is_empty() {
            return Bounds::EMPTY;
        }
        Bounds::new(
            Point::new(self.min.x as f64, self.min.y as f64),
            Point::new(self.max.x as f64, self.max.y as f64),
        )
    }

    /// Sample count as a `usize`, or the reason this rectangle is too large.
    pub(crate) fn checked_samples(&self) -> Result<usize, VectorError> {
        let area = self.area();
        if area > MAX_MASK_SAMPLES {
            return Err(VectorError::RegionTooLarge {
                width: self.width() as u64,
                height: self.height() as u64,
                max: MAX_MASK_SAMPLES,
            });
        }
        usize::try_from(area).map_err(|_| VectorError::RegionTooLarge {
            width: self.width() as u64,
            height: self.height() as u64,
            max: MAX_MASK_SAMPLES,
        })
    }
}

/// `vec![value; n]` that reports failure instead of aborting the process.
///
/// The extent of a rasterised path follows caller input, so an unaffordable
/// allocation is reachable — and `vec![]`'s response to one is
/// `handle_alloc_error`, an abort no editor can catch or report.
pub(crate) fn alloc_vec<T: Clone>(n: usize, value: T) -> Result<Vec<T>, VectorError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| VectorError::OutOfMemory {
            bytes: n.saturating_mul(std::mem::size_of::<T>()),
        })?;
    v.resize(n, value);
    Ok(v)
}

/// Fractional coverage in `0.0..=1.0` rounded to the nearest byte.
///
/// Round to nearest, not truncate: truncation loses half a level every time a
/// mask is quantised, which is visible after two or three operations.
pub(crate) fn to_byte(v: f32) -> u8 {
    if v.is_nan() {
        return 0;
    }
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// A rectangle of per-pixel coverage: 0 is outside the shape, 255 is fully
/// inside, and everything between is a partially covered edge pixel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageMask {
    origin: IVec2,
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

impl CoverageMask {
    /// Build from raw samples, row-major, `width * height` of them.
    pub fn new(
        origin: IVec2,
        width: u32,
        height: u32,
        coverage: Vec<u8>,
    ) -> Result<Self, VectorError> {
        let rect = PixelRect::from_xywh(origin.x, origin.y, width, height);
        if rect.width() != width || rect.height() != height {
            return Err(VectorError::RegionTooLarge {
                width: width as u64,
                height: height as u64,
                max: MAX_MASK_SAMPLES,
            });
        }
        let expected = rect.checked_samples()?;
        if coverage.len() != expected {
            return Err(VectorError::InvalidParameter {
                what: "coverage length",
                expected: "width * height",
                value: coverage.len() as f64,
            });
        }
        Ok(Self {
            origin,
            width,
            height,
            coverage,
        })
    }

    /// A mask covering no pixel at all.
    pub fn empty(origin: IVec2) -> Self {
        Self {
            origin,
            width: 0,
            height: 0,
            coverage: Vec::new(),
        }
    }

    /// Document-space position of the first sample.
    pub fn origin(&self) -> IVec2 {
        self.origin
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Raw samples, row-major.
    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    /// The storage rectangle.
    pub fn rect(&self) -> PixelRect {
        PixelRect::from_xywh(self.origin.x, self.origin.y, self.width, self.height)
    }

    /// Coverage of one document pixel; 0 outside the mask.
    ///
    /// The local coordinates are computed in `i64`, so a query far from the
    /// origin answers 0 rather than wrapping or panicking on the subtraction.
    pub fn coverage_at(&self, p: IVec2) -> u8 {
        let lx = p.x as i64 - self.origin.x as i64;
        let ly = p.y as i64 - self.origin.y as i64;
        if lx < 0 || ly < 0 || lx >= self.width as i64 || ly >= self.height as i64 {
            return 0;
        }
        self.coverage[ly as usize * self.width as usize + lx as usize]
    }

    /// Coverage of one document pixel as a fraction in `0.0..=1.0`.
    pub fn coverage_f32(&self, p: IVec2) -> f32 {
        self.coverage_at(p) as f32 / 255.0
    }

    /// `true` when no pixel has any coverage.
    pub fn is_empty(&self) -> bool {
        self.coverage.iter().all(|&v| v == 0)
    }

    /// Total covered area in pixels: the sum of every sample, in units of whole
    /// pixels.
    ///
    /// This is the number an anti-aliased rasteriser is judged by — a shape of
    /// area *A* must produce coverage summing to *A*, whatever the quantisation
    /// of its edges.
    pub fn area(&self) -> f64 {
        self.coverage.iter().map(|&v| v as f64).sum::<f64>() / 255.0
    }

    /// Tight half-open box of the non-zero samples, or `None` when empty.
    pub fn bounds(&self) -> Option<(IVec2, IVec2)> {
        let (mut min_x, mut min_y) = (u32::MAX, u32::MAX);
        let (mut max_x, mut max_y) = (0u32, 0u32);
        let mut any = false;
        for y in 0..self.height {
            let row = &self.coverage
                [y as usize * self.width as usize..(y as usize + 1) * self.width as usize];
            for (x, &v) in row.iter().enumerate() {
                if v != 0 {
                    any = true;
                    let x = x as u32;
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        if !any {
            return None;
        }
        Some((
            IVec2::new(self.origin.x + min_x as i32, self.origin.y + min_y as i32),
            IVec2::new(
                self.origin.x + max_x as i32 + 1,
                self.origin.y + max_y as i32 + 1,
            ),
        ))
    }

    /// A copy shrunk to the tight box of its non-zero coverage.
    ///
    /// This is what keeps a small shape on a huge canvas small: the rasteriser
    /// may work in a clip rectangle the size of the document, but the mask that
    /// survives is only the part that is actually covered.
    pub fn trimmed(&self) -> Result<Self, VectorError> {
        let Some((min, max)) = self.bounds() else {
            return Ok(Self::empty(self.origin));
        };
        if min == self.origin
            && max
                == IVec2::new(
                    self.origin.x + self.width as i32,
                    self.origin.y + self.height as i32,
                )
        {
            return Ok(self.clone());
        }
        let w = (max.x - min.x) as u32;
        let h = (max.y - min.y) as u32;
        let mut out = alloc_vec((w as usize) * (h as usize), 0u8)?;
        let sx = (min.x - self.origin.x) as usize;
        let sy = (min.y - self.origin.y) as usize;
        for y in 0..h as usize {
            let src = (sy + y) * self.width as usize + sx;
            out[y * w as usize..(y + 1) * w as usize]
                .copy_from_slice(&self.coverage[src..src + w as usize]);
        }
        Self::new(min, w, h, out)
    }

    /// Take the mask apart into exactly the arguments
    /// `editor_core::SelectionMask::new` and
    /// `selection::channel_to_selection` take.
    ///
    /// This is the whole compatibility contract: origin, size, and row-major
    /// coverage bytes. There is no conversion step, because there is nothing to
    /// convert.
    pub fn into_parts(self) -> (IVec2, u32, u32, Vec<u8>) {
        (self.origin, self.width, self.height, self.coverage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pixel_rect_rounds_outward_so_no_edge_is_clipped() {
        let r = PixelRect::enclosing(Bounds::from_xywh(0.2, -0.3, 1.1, 2.2));
        assert_eq!(r.min(), IVec2::new(0, -1));
        assert_eq!(r.max(), IVec2::new(2, 2));
        assert!(r.contains(IVec2::new(1, 1)));
        assert!(!r.contains(IVec2::new(2, 1)));
        assert_eq!(r.area(), 6);
        assert_eq!(PixelRect::enclosing(Bounds::EMPTY), PixelRect::EMPTY);
        // `Bounds::new` normalises a NaN corner away, so the guard inside
        // `enclosing` is reached through a directly-built box.
        let nan = Bounds {
            min: Point::new(f64::NAN, 0.0),
            max: Point::new(1.0, 1.0),
        };
        assert_eq!(PixelRect::enclosing(nan), PixelRect::EMPTY);
    }

    #[test]
    fn an_inside_out_rect_normalises_to_empty_rather_than_a_negative_size() {
        let r = PixelRect::new(IVec2::new(10, 10), IVec2::new(0, 0));
        assert!(r.is_empty());
        assert_eq!(r.width(), 0);
        assert_eq!(r.area(), 0);
    }

    #[test]
    fn a_mask_larger_than_the_limit_is_refused_instead_of_aborting() {
        // 2^31 x 2^31 fits inside the clamped coordinate grid but is 2^62
        // samples. `vec![]` of that is an abort, not an error.
        let huge = PixelRect::new(
            IVec2::new(-COORD_LIMIT, -COORD_LIMIT),
            IVec2::new(COORD_LIMIT, COORD_LIMIT),
        );
        assert!(matches!(
            huge.checked_samples(),
            Err(VectorError::RegionTooLarge { .. })
        ));
        assert!(matches!(
            alloc_vec::<u8>(usize::MAX, 0),
            Err(VectorError::OutOfMemory { .. })
        ));
        assert!(matches!(
            alloc_vec::<f32>(usize::MAX / 2, 0.0),
            Err(VectorError::OutOfMemory { .. })
        ));
    }

    #[test]
    fn a_mask_reports_area_and_trims_to_its_content() {
        let mut cov = vec![0u8; 16];
        cov[5] = 255;
        cov[6] = 128;
        let m = CoverageMask::new(IVec2::new(3, 3), 4, 4, cov).unwrap();
        assert_eq!(m.coverage_at(IVec2::new(4, 4)), 255);
        assert_eq!(m.coverage_at(IVec2::new(5, 4)), 128);
        assert_eq!(m.coverage_at(IVec2::new(0, 0)), 0);
        assert_eq!(m.coverage_at(IVec2::new(i32::MIN, i32::MAX)), 0);
        assert!((m.area() - (255.0 + 128.0) / 255.0).abs() < 1e-12);

        let t = m.trimmed().unwrap();
        assert_eq!(t.origin(), IVec2::new(4, 4));
        assert_eq!((t.width(), t.height()), (2, 1));
        assert_eq!(t.coverage(), &[255, 128]);
        // trimming does not move the pixels
        assert_eq!(t.coverage_at(IVec2::new(4, 4)), 255);
        assert_eq!(t.coverage_at(IVec2::new(5, 4)), 128);
    }

    #[test]
    fn an_all_zero_mask_trims_to_nothing_rather_than_staying_huge() {
        let m = CoverageMask::new(IVec2::ZERO, 100, 100, vec![0; 10_000]).unwrap();
        assert!(m.is_empty());
        assert_eq!(m.bounds(), None);
        let t = m.trimmed().unwrap();
        assert_eq!(t.coverage().len(), 0);
        assert!(t.is_empty());
    }

    #[test]
    fn a_mismatched_sample_count_is_refused() {
        assert!(matches!(
            CoverageMask::new(IVec2::ZERO, 4, 4, vec![0; 15]),
            Err(VectorError::InvalidParameter { .. })
        ));
        assert!(CoverageMask::new(IVec2::ZERO, 4, 4, vec![0; 16]).is_ok());
    }

    #[test]
    fn into_parts_is_the_selection_constructors_argument_list() {
        // The compatibility contract, held as a shape check: a rasterised path
        // hands `(origin, width, height, coverage)` straight to the selection
        // engine with no resampling and no reinterpretation.
        let m = CoverageMask::new(IVec2::new(-2, 7), 2, 3, vec![1, 2, 3, 4, 5, 6]).unwrap();
        let (origin, w, h, cov) = m.into_parts();
        assert_eq!(origin, IVec2::new(-2, 7));
        assert_eq!((w, h), (2, 3));
        assert_eq!(cov.len(), (w * h) as usize);
        assert_eq!(cov, vec![1, 2, 3, 4, 5, 6]);
        // and it round-trips back into a mask unchanged
        let back = CoverageMask::new(origin, w, h, cov).unwrap();
        assert_eq!(back.coverage_at(IVec2::new(-1, 8)), 4);
    }

    #[test]
    fn byte_rounding_is_to_nearest() {
        assert_eq!(to_byte(0.0), 0);
        assert_eq!(to_byte(1.0), 255);
        assert_eq!(to_byte(0.5), 128);
        assert_eq!(to_byte(-1.0), 0);
        assert_eq!(to_byte(2.0), 255);
        assert_eq!(to_byte(f32::NAN), 0);
    }
}
