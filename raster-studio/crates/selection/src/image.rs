//! The pixel source the colour-driven tools read.
//!
//! The wand, quick select, colour range, grow, similar and the magnetic lasso
//! all need to look at pixels; nothing else in this crate does. They read an
//! [`ImageView`]: a rectangle of straight-alpha, sRGB-encoded RGBA8 placed at a
//! document position, which is what [`raster::TileGrid::to_rgba8`] produces.
//!
//! The view borrows, so a caller that already holds a flattened composite pays
//! nothing extra; [`ImageBuffer`] owns one for callers that do not.

use glam::IVec2;
use raster::{PixelFormat, TileGrid};

use crate::error::SelectionOpError;
use crate::metric::{ColorCoords, ColorMetric};
use crate::rect::Rect;

/// Bytes a `width * height` RGBA8 image occupies, or why it is not one.
fn rgba_len(rect: Rect) -> Result<usize, SelectionOpError> {
    let n = rect
        .area()
        .checked_mul(4)
        .ok_or(SelectionOpError::RegionTooLarge {
            width: rect.width(),
            height: rect.height(),
        })?;
    usize::try_from(n).map_err(|_| SelectionOpError::RegionTooLarge {
        width: rect.width(),
        height: rect.height(),
    })
}

/// A borrowed rectangle of RGBA8 pixels in document space.
#[derive(Debug, Clone, Copy)]
pub struct ImageView<'a> {
    rect: Rect,
    pixels: &'a [u8],
}

impl<'a> ImageView<'a> {
    /// Wrap `width * height * 4` bytes whose first pixel sits at `origin`.
    pub fn new(
        origin: IVec2,
        width: u32,
        height: u32,
        pixels: &'a [u8],
    ) -> Result<Self, SelectionOpError> {
        let rect = Rect::from_xywh(origin.x, origin.y, width, height);
        if rect.width() != width || rect.height() != height {
            return Err(SelectionOpError::CoordOutOfRange {
                x: origin.x,
                y: origin.y,
                width,
                height,
                limit: crate::COORD_LIMIT,
            });
        }
        let expected = rgba_len(rect)?;
        if pixels.len() != expected {
            return Err(SelectionOpError::ImageSizeMismatch {
                width,
                height,
                expected,
                got: pixels.len(),
            });
        }
        Ok(Self { rect, pixels })
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

    pub fn contains(&self, p: IVec2) -> bool {
        self.rect.contains(p)
    }

    /// One pixel; fully transparent black outside the view, so every algorithm
    /// here is total over the document plane.
    pub fn pixel(&self, p: IVec2) -> [u8; 4] {
        if !self.rect.contains(p) {
            return [0, 0, 0, 0];
        }
        let lx = (p.x - self.rect.min().x) as usize;
        let ly = (p.y - self.rect.min().y) as usize;
        let i = (ly * self.width() + lx) * 4;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// A pixel already mapped into a metric's coordinates.
    pub fn coords_at(&self, p: IVec2, metric: ColorMetric) -> ColorCoords {
        metric.coords(self.pixel(p))
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.pixels
    }
}

/// An owned RGBA8 image, for callers that do not already hold one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBuffer {
    rect: Rect,
    pixels: Vec<u8>,
}

impl ImageBuffer {
    pub fn from_rgba8(
        origin: IVec2,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<Self, SelectionOpError> {
        // Validate through the borrowed form so both paths share one check.
        let rect = ImageView::new(origin, width, height, &pixels)?.rect();
        Ok(Self { rect, pixels })
    }

    /// Flatten a tile grid. The grid's own extent becomes the view rectangle,
    /// with `origin` placing it in the document.
    pub fn from_tile_grid(origin: IVec2, grid: &TileGrid) -> Result<Self, SelectionOpError> {
        if grid.format() != PixelFormat::Rgba8 {
            return Err(SelectionOpError::Image(format!(
                "selection tools read RGBA8; this grid is {:?}",
                grid.format()
            )));
        }
        let (w, h) = grid.dimensions();
        let pixels = grid
            .to_rgba8()
            .map_err(|e| SelectionOpError::Image(e.to_string()))?;
        Self::from_rgba8(origin, w, h, pixels)
    }

    pub fn view(&self) -> ImageView<'_> {
        ImageView {
            rect: self.rect,
            pixels: &self.pixels,
        }
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster::TILE_SIZE;

    #[test]
    fn a_view_reads_transparent_black_outside_itself() {
        let px = vec![9u8; 2 * 2 * 4];
        let v = ImageView::new(IVec2::new(5, 5), 2, 2, &px).unwrap();
        assert_eq!(v.pixel(IVec2::new(5, 5)), [9, 9, 9, 9]);
        assert_eq!(v.pixel(IVec2::new(4, 5)), [0, 0, 0, 0]);
        assert_eq!(v.pixel(IVec2::new(7, 7)), [0, 0, 0, 0]);
        assert_eq!(v.pixel(IVec2::new(i32::MIN, i32::MAX)), [0, 0, 0, 0]);
    }

    #[test]
    fn a_byte_count_that_does_not_match_the_extent_is_refused() {
        let px = vec![0u8; 15];
        assert!(matches!(
            ImageView::new(IVec2::ZERO, 2, 2, &px),
            Err(SelectionOpError::ImageSizeMismatch {
                expected: 16,
                got: 15,
                ..
            })
        ));
    }

    #[test]
    fn a_tile_grid_flattens_into_a_view_at_a_document_position() {
        let (w, h) = (TILE_SIZE + 3, 5);
        let mut src = vec![0u8; (w * h * 4) as usize];
        // One distinctive pixel in the partial edge tile.
        let i = ((3 * w + TILE_SIZE + 1) * 4) as usize;
        src[i..i + 4].copy_from_slice(&[10, 20, 30, 255]);
        let grid = TileGrid::from_rgba8(w, h, &src).unwrap();

        let buf = ImageBuffer::from_tile_grid(IVec2::new(100, 200), &grid).unwrap();
        let v = buf.view();
        assert_eq!(v.rect(), Rect::from_xywh(100, 200, w, h));
        assert_eq!(
            v.pixel(IVec2::new(100 + TILE_SIZE as i32 + 1, 203)),
            [10, 20, 30, 255],
            "edge-tile padding must not shift the image"
        );
    }
}
