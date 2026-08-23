//! The mutable working buffer every algorithm builds its answer in.
//!
//! [`editor_core::SelectionMask`] is immutable and validated; it is the *result*
//! type. A [`CoverageBuf`] is the same rectangle of 8-bit coverage with the
//! bounds check hoisted out of the inner loop and with mutation allowed, plus
//! the one thing the mask cannot do: shrink to fit.
//!
//! # Coverage is a linear quantity
//! A coverage byte is a fraction of a pixel, exactly like alpha — 128 means
//! "half of this pixel is selected", not "the sRGB encoding of a half". So
//! every filter in this crate (feather, smooth, transform resampling) averages
//! coverage bytes *directly*. There is no transfer function to undo, and
//! applying one would be the bug: it would bend a straight geometric ramp.
//! Colour, which *is* gamma-encoded, is handled separately in
//! [`crate::metric`].
//!
//! # Nothing here allocates the canvas
//! A buffer is exactly the rectangle it is asked for, and
//! [`CoverageBuf::into_mask`] trims to the tight box of non-zero coverage
//! before building the mask. A 10×10 ellipse on a million-pixel-square canvas
//! costs 100 bytes.

use editor_core::{SelectionMask, MAX_MASK_SAMPLES};
use glam::IVec2;

use crate::error::SelectionOpError;
use crate::rect::Rect;

/// Sample count of a rect, or the reason it cannot be one.
pub(crate) fn checked_samples(rect: Rect) -> Result<usize, SelectionOpError> {
    let area = rect.area();
    if area > MAX_MASK_SAMPLES {
        return Err(SelectionOpError::RegionTooLarge {
            width: rect.width(),
            height: rect.height(),
        });
    }
    usize::try_from(area).map_err(|_| SelectionOpError::RegionTooLarge {
        width: rect.width(),
        height: rect.height(),
    })
}

/// `vec![value; n]` that reports failure instead of aborting the process.
///
/// A plain `vec!` on an extent this machine cannot hold calls
/// `handle_alloc_error`, which is an abort no caller can catch. Selection
/// extents come from drag gestures and from files, so that abort is reachable.
///
/// # What is guarded, exactly
/// Every buffer in this crate whose size grows with the **image or mask area** —
/// the extent a gesture or a file can make arbitrarily large — is allocated
/// through one of three guarded paths, so an unaffordable one is an
/// [`SelectionOpError::OutOfMemory`] the editor can report rather than an abort:
///
/// * **extent known up front** — coverage buffers, filter intermediates,
///   flood-fill visited flags, the quick-select seed bitmap, the magic wand's
///   colour cube, the magnetic lasso's Dijkstra tables, the transform's
///   summed-area table: this function and its [`alloc_bytes`] / [`alloc_f32`]
///   wrappers.
/// * **grown as it goes** — the flood-fill stacks in [`crate::wand`], the
///   magnetic lasso's snapped path, the marching-ants vertex and loop lists,
///   the mask-tile list: [`try_push`] / [`try_extend`], which reserve through
///   `Vec::try_reserve` instead of `push`'s `handle_alloc_error` abort.
/// * **a priority queue grown as it goes** — the magnetic lasso's Dijkstra
///   frontier: [`try_heap_push`].
///
/// Deliberately *not* separately guarded: the working copies of the caller's
/// own point list (a polygon's vertices localised to its bounding box, the
/// closed copy of a lasso's anchors, the per-scanline crossing list). Each is a
/// small constant multiple of a slice the caller already holds, so their
/// affordability is already proven by the caller's own allocation; guarding
/// them would buy nothing and put a branch in the scanline inner loop.
pub(crate) fn alloc_vec<T: Clone>(n: usize, value: T) -> Result<Vec<T>, SelectionOpError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| SelectionOpError::OutOfMemory {
            bytes: n.saturating_mul(std::mem::size_of::<T>()),
        })?;
    v.resize(n, value);
    Ok(v)
}

/// `Vec::push` that reports an unaffordable growth instead of aborting.
///
/// `push` on a full vector reallocates, and a reallocation this machine cannot
/// satisfy ends in `handle_alloc_error` — the same uncatchable abort
/// [`alloc_vec`] exists to avoid, reached from a container whose length is
/// caller input. The flood-fill stacks are exactly that: one entry per pixel
/// reached, at eight bytes each, against a visited map of one byte per pixel.
pub(crate) fn try_push<T>(v: &mut Vec<T>, value: T) -> Result<(), SelectionOpError> {
    if v.len() == v.capacity() {
        // `try_reserve(1)` on a full vector still grows geometrically, so this
        // costs an amortised comparison per push, not a reallocation per push.
        v.try_reserve(1)
            .map_err(|_| SelectionOpError::OutOfMemory {
                bytes: v
                    .len()
                    .saturating_add(1)
                    .saturating_mul(2)
                    .saturating_mul(std::mem::size_of::<T>()),
            })?;
    }
    v.push(value);
    Ok(())
}

/// [`try_push`] for a whole slice.
pub(crate) fn try_extend<T: Clone>(v: &mut Vec<T>, items: &[T]) -> Result<(), SelectionOpError> {
    v.try_reserve(items.len())
        .map_err(|_| SelectionOpError::OutOfMemory {
            bytes: v
                .len()
                .saturating_add(items.len())
                .saturating_mul(std::mem::size_of::<T>()),
        })?;
    v.extend_from_slice(items);
    Ok(())
}

/// [`try_push`] for the magnetic lasso's Dijkstra frontier.
pub(crate) fn try_heap_push<T: Ord>(
    h: &mut std::collections::BinaryHeap<T>,
    value: T,
) -> Result<(), SelectionOpError> {
    if h.len() == h.capacity() {
        h.try_reserve(1)
            .map_err(|_| SelectionOpError::OutOfMemory {
                bytes: h
                    .len()
                    .saturating_add(1)
                    .saturating_mul(2)
                    .saturating_mul(std::mem::size_of::<T>()),
            })?;
    }
    h.push(value);
    Ok(())
}

/// [`alloc_vec`] for coverage bytes.
pub(crate) fn alloc_bytes(n: usize, value: u8) -> Result<Vec<u8>, SelectionOpError> {
    alloc_vec(n, value)
}

/// Same guarantee for the `f32` intermediates the filters need.
pub(crate) fn alloc_f32(n: usize) -> Result<Vec<f32>, SelectionOpError> {
    alloc_vec(n, 0.0f32)
}

/// Fractional coverage in `0.0..=1.0` rounded to the nearest byte.
///
/// Round-to-nearest, not truncation: truncating loses half a level on every
/// filter pass, which is visible after two or three of them.
pub(crate) fn to_byte(v: f32) -> u8 {
    if v.is_nan() {
        return 0;
    }
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// A rectangle of mutable 8-bit coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageBuf {
    rect: Rect,
    data: Vec<u8>,
}

impl CoverageBuf {
    /// An all-zero (nothing selected) buffer.
    pub fn zeroed(rect: Rect) -> Result<Self, SelectionOpError> {
        Self::filled_with(rect, 0)
    }

    /// A buffer where every sample is `value`.
    pub fn filled_with(rect: Rect, value: u8) -> Result<Self, SelectionOpError> {
        let n = checked_samples(rect)?;
        Ok(Self {
            rect,
            data: alloc_bytes(n, value)?,
        })
    }

    /// Copy a mask's samples into a working buffer.
    pub fn from_mask(mask: &SelectionMask) -> Result<Self, SelectionOpError> {
        let rect = Rect::of_mask(mask)?;
        let n = checked_samples(rect)?;
        let mut data = alloc_bytes(n, 0)?;
        data.copy_from_slice(mask.coverage());
        Ok(Self { rect, data })
    }

    /// Adopt an existing buffer of exactly `rect.area()` samples.
    pub fn from_parts(rect: Rect, data: Vec<u8>) -> Result<Self, SelectionOpError> {
        let n = checked_samples(rect)?;
        if data.len() != n {
            return Err(SelectionOpError::ImageSizeMismatch {
                width: rect.width(),
                height: rect.height(),
                expected: n,
                got: data.len(),
            });
        }
        Ok(Self { rect, data })
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn width(&self) -> usize {
        self.rect.width() as usize
    }

    pub fn height(&self) -> usize {
        self.rect.height() as usize
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Coverage of one document pixel; 0 outside the buffer.
    pub fn get(&self, p: IVec2) -> u8 {
        if !self.rect.contains(p) {
            return 0;
        }
        let lx = (p.x - self.rect.min().x) as usize;
        let ly = (p.y - self.rect.min().y) as usize;
        self.data[ly * self.width() + lx]
    }

    /// Write one document pixel; a point outside the buffer is dropped.
    pub fn set(&mut self, p: IVec2, v: u8) {
        if !self.rect.contains(p) {
            return;
        }
        let w = self.width();
        let lx = (p.x - self.rect.min().x) as usize;
        let ly = (p.y - self.rect.min().y) as usize;
        self.data[ly * w + lx] = v;
    }

    /// Raise one document pixel to `v` if it is currently lower.
    pub fn raise(&mut self, p: IVec2, v: u8) {
        if v > self.get(p) {
            self.set(p, v);
        }
    }

    pub fn row(&self, y: usize) -> &[u8] {
        let w = self.width();
        &self.data[y * w..(y + 1) * w]
    }

    pub fn row_mut(&mut self, y: usize) -> &mut [u8] {
        let w = self.width();
        &mut self.data[y * w..(y + 1) * w]
    }

    /// Tight box of the non-zero samples, [`Rect::EMPTY`] when there are none.
    pub fn content_rect(&self) -> Rect {
        let (w, h) = (self.width(), self.height());
        if w == 0 || h == 0 {
            return Rect::EMPTY;
        }
        let (mut min_x, mut min_y) = (usize::MAX, usize::MAX);
        let (mut max_x, mut max_y) = (0usize, 0usize);
        let mut any = false;
        for y in 0..h {
            for (x, &v) in self.row(y).iter().enumerate() {
                if v != 0 {
                    any = true;
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        if !any {
            return Rect::EMPTY;
        }
        let o = self.rect.min();
        Rect::new(
            IVec2::new(o.x + min_x as i32, o.y + min_y as i32),
            IVec2::new(o.x + max_x as i32 + 1, o.y + max_y as i32 + 1),
        )
    }

    /// A copy of this buffer over `rect`, sampling 0 where the two do not
    /// overlap.
    pub fn resized(&self, rect: Rect) -> Result<Self, SelectionOpError> {
        let mut out = Self::zeroed(rect)?;
        let overlap = self.rect.intersection(rect);
        if overlap.is_empty() {
            return Ok(out);
        }
        let ow = overlap.width() as usize;
        for y in 0..overlap.height() as usize {
            let sy = (overlap.min().y - self.rect.min().y) as usize + y;
            let sx = (overlap.min().x - self.rect.min().x) as usize;
            let dy = (overlap.min().y - rect.min().y) as usize + y;
            let dx = (overlap.min().x - rect.min().x) as usize;
            let src_row = &self.data[sy * self.width() + sx..sy * self.width() + sx + ow];
            let dw = out.width();
            out.data[dy * dw + dx..dy * dw + dx + ow].copy_from_slice(src_row);
        }
        Ok(out)
    }

    /// Shrink to the tight box of non-zero coverage.
    pub fn trimmed(&self) -> Result<Self, SelectionOpError> {
        let content = self.content_rect();
        if content == self.rect {
            return Ok(self.clone());
        }
        if content.is_empty() {
            return Ok(Self {
                rect: Rect::new(self.rect.min(), self.rect.min()),
                data: Vec::new(),
            });
        }
        self.resized(content)
    }

    /// Freeze into a mask, trimmed to the tight box of its coverage.
    ///
    /// Trimming is what keeps a small selection on a huge canvas small: the
    /// working buffer may be canvas-sized (a global magic wand has to look at
    /// every pixel), but the mask that survives is only the part that is
    /// actually selected.
    pub fn into_mask(self) -> Result<SelectionMask, SelectionOpError> {
        let content = self.content_rect();
        let trimmed = if content == self.rect {
            self
        } else if content.is_empty() {
            return Ok(SelectionMask::new(self.rect.min(), 0, 0, Vec::new())?);
        } else {
            self.resized(content)?
        };
        Ok(SelectionMask::new(
            trimmed.rect.min(),
            trimmed.rect.width(),
            trimmed.rect.height(),
            trimmed.data,
        )?)
    }

    /// Freeze without trimming — the mask keeps this exact storage rectangle.
    pub fn into_mask_untrimmed(self) -> Result<SelectionMask, SelectionOpError> {
        Ok(SelectionMask::new(
            self.rect.min(),
            self.rect.width(),
            self.rect.height(),
            self.data,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_only_ever_holds_its_own_rectangle() {
        let mut b = CoverageBuf::zeroed(Rect::from_xywh(-3, 7, 4, 2)).unwrap();
        assert_eq!(b.data().len(), 8);
        b.set(IVec2::new(-3, 7), 200);
        b.set(IVec2::new(0, 8), 100);
        b.set(IVec2::new(99, 99), 255); // outside: dropped, not a panic
        assert_eq!(b.get(IVec2::new(-3, 7)), 200);
        assert_eq!(b.get(IVec2::new(0, 8)), 100);
        assert_eq!(b.get(IVec2::new(99, 99)), 0);
        assert_eq!(b.get(IVec2::new(i32::MIN, i32::MAX)), 0);
    }

    #[test]
    fn into_mask_trims_to_the_covered_pixels() {
        let mut b = CoverageBuf::zeroed(Rect::from_xywh(0, 0, 64, 64)).unwrap();
        b.set(IVec2::new(10, 20), 255);
        b.set(IVec2::new(12, 21), 128);
        let m = b.into_mask().unwrap();
        assert_eq!(m.origin(), IVec2::new(10, 20));
        assert_eq!((m.width(), m.height()), (3, 2));
        assert_eq!(m.coverage().len(), 6);
        assert_eq!(m.coverage_at(IVec2::new(10, 20)), 255);
        assert_eq!(m.coverage_at(IVec2::new(12, 21)), 128);
    }

    #[test]
    fn an_all_zero_buffer_becomes_an_empty_mask_rather_than_a_huge_one() {
        let b = CoverageBuf::zeroed(Rect::from_xywh(5, 5, 200, 200)).unwrap();
        let m = b.into_mask().unwrap();
        assert!(m.is_empty());
        assert_eq!(m.coverage().len(), 0);
    }

    #[test]
    fn a_region_larger_than_a_mask_may_be_is_refused_without_allocating() {
        // 2^31 x 2^31 is representable inside COORD_LIMIT*2 but is 2^62
        // samples. A `vec![]` of that is an abort, not an error.
        let huge = Rect::new(
            IVec2::new(-crate::COORD_LIMIT, -crate::COORD_LIMIT),
            IVec2::new(crate::COORD_LIMIT, crate::COORD_LIMIT),
        );
        assert!(matches!(
            CoverageBuf::zeroed(huge),
            Err(SelectionOpError::RegionTooLarge { .. })
        ));
    }

    #[test]
    fn resized_samples_zero_outside_the_overlap() {
        let mut b = CoverageBuf::filled_with(Rect::from_xywh(0, 0, 2, 2), 255).unwrap();
        b.set(IVec2::new(1, 1), 7);
        let grown = b.resized(Rect::from_xywh(-1, -1, 4, 4)).unwrap();
        assert_eq!(grown.get(IVec2::new(-1, -1)), 0);
        assert_eq!(grown.get(IVec2::new(0, 0)), 255);
        assert_eq!(grown.get(IVec2::new(1, 1)), 7);
        assert_eq!(grown.get(IVec2::new(2, 2)), 0);
    }

    #[test]
    fn an_unaffordable_working_buffer_is_an_error_not_an_abort() {
        // Sized so the byte count cannot fit the address space at all, which
        // `try_reserve` reports and `vec![v; n]` turns into an uncatchable
        // `handle_alloc_error` abort.
        assert!(matches!(
            alloc_vec::<u32>(usize::MAX / 2, 0),
            Err(SelectionOpError::OutOfMemory { .. })
        ));
        assert!(matches!(
            alloc_vec::<usize>(usize::MAX / 4, 0),
            Err(SelectionOpError::OutOfMemory { .. })
        ));
        assert!(matches!(
            alloc_bytes(usize::MAX, 0),
            Err(SelectionOpError::OutOfMemory { .. })
        ));
        assert!(matches!(
            alloc_f32(usize::MAX / 2),
            Err(SelectionOpError::OutOfMemory { .. })
        ));
        // And a size this machine certainly can hold still works.
        assert_eq!(alloc_vec::<bool>(4, true).unwrap(), vec![true; 4]);
    }

    /// The growing containers are guarded by the same `try_reserve` the fixed
    /// ones are, so no container in this crate whose length follows caller
    /// input can reach `handle_alloc_error`.
    ///
    /// The error arm itself is one delegation to `Vec::try_reserve` /
    /// `BinaryHeap::try_reserve`, whose failure is what
    /// `an_unaffordable_working_buffer_is_an_error_not_an_abort` exercises; it
    /// cannot be reached from here without an allocator hook, because a
    /// reservation only fails once the machine is genuinely out of memory.
    /// What is checkable here is that the guard is transparent — same order,
    /// same contents, still amortised — so there is no reason to reach for a
    /// plain `push` anywhere.
    #[test]
    fn a_guarded_push_behaves_exactly_like_push() {
        let mut v: Vec<u32> = Vec::new();
        for i in 0..1000u32 {
            try_push(&mut v, i).unwrap();
        }
        assert_eq!(v.len(), 1000);
        assert!(v.iter().copied().eq(0..1000));
        // Geometric growth, not one reallocation per element.
        assert!(
            v.capacity() < 4000,
            "growth is not amortised: capacity {} for 1000 pushes",
            v.capacity()
        );

        let mut h: std::collections::BinaryHeap<u32> = std::collections::BinaryHeap::new();
        for i in [5u32, 1, 9, 3] {
            try_heap_push(&mut h, i).unwrap();
        }
        assert_eq!(h.pop(), Some(9));
        assert_eq!(h.into_sorted_vec(), vec![1, 3, 5]);
    }

    #[test]
    fn rounding_to_a_byte_is_to_nearest_not_truncating() {
        assert_eq!(to_byte(0.0), 0);
        assert_eq!(to_byte(1.0), 255);
        assert_eq!(to_byte(0.5), 128);
        assert_eq!(to_byte(1.0 / 255.0 * 0.6), 1, "0.6 of a level rounds up");
        assert_eq!(to_byte(-3.0), 0);
        assert_eq!(to_byte(9.0), 255);
        assert_eq!(to_byte(f32::NAN), 0);
    }
}
