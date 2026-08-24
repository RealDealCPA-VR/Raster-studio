//! [`Canvas`] — the compositor's working buffer.
//!
//! Every pixel in a `Canvas` is **linear, premultiplied** `f32` RGBA. That is
//! the crate-wide invariant (see the [crate docs](crate)); nothing in this
//! module converts on your behalf except the two explicitly named entry points
//! [`Canvas::to_rgba8`] and [`Canvas::to_straight`].

use color::{from_linear, unpremultiply, ColorSpace};
use layer_model::blend::unit;
use raster::PixelRect;

use crate::error::CompositeError;

/// Largest number of pixels a single [`Canvas`] may hold: 2^26, about 67
/// million, which is 1 GiB of RGBA `f32`.
///
/// [`PixelRect`] dimensions are `u32`, so a caller — or an extreme layer
/// transform, whose pre-image rect the compositor has to allocate — can name
/// far more pixels than can be stored. The request is refused with
/// [`CompositeError::RegionTooLarge`] instead of attempting an allocation that
/// would abort the process.
pub const MAX_CANVAS_PIXELS: u64 = 1 << 26;

/// A rectangular block of linear, premultiplied `f32` RGBA pixels.
///
/// The rect carries the buffer's position in image space, so a canvas covering
/// one tile knows where that tile is and [`Canvas::blit_from`] can assemble a
/// region out of tiles without the caller tracking offsets.
#[derive(Debug, Clone, PartialEq)]
pub struct Canvas {
    rect: PixelRect,
    pixels: Vec<[f32; 4]>,
}

impl Canvas {
    /// Number of pixels `rect` covers, refusing anything past
    /// [`MAX_CANVAS_PIXELS`].
    pub fn area(rect: PixelRect) -> Result<usize, CompositeError> {
        let pixels = rect.width as u64 * rect.height as u64;
        if pixels > MAX_CANVAS_PIXELS {
            return Err(CompositeError::RegionTooLarge {
                pixels,
                max: MAX_CANVAS_PIXELS,
            });
        }
        Ok(pixels as usize)
    }

    /// A fully transparent canvas covering `rect`.
    pub fn transparent(rect: PixelRect) -> Result<Self, CompositeError> {
        let n = Self::area(rect)?;
        Ok(Self {
            rect,
            pixels: vec![[0.0; 4]; n],
        })
    }

    /// Wrap an existing buffer. The length must be exactly `rect.width *
    /// rect.height`.
    pub fn from_pixels(rect: PixelRect, pixels: Vec<[f32; 4]>) -> Result<Self, CompositeError> {
        let expected = Self::area(rect)?;
        if pixels.len() != expected {
            return Err(CompositeError::PixelCountMismatch {
                expected,
                got: pixels.len(),
            });
        }
        Ok(Self { rect, pixels })
    }

    /// The area this canvas covers in image pixel space.
    pub fn rect(&self) -> PixelRect {
        self.rect
    }

    pub fn width(&self) -> u32 {
        self.rect.width
    }

    pub fn height(&self) -> u32 {
        self.rect.height
    }

    /// Row-major pixels, linear and premultiplied.
    pub fn pixels(&self) -> &[[f32; 4]] {
        &self.pixels
    }

    /// Mutable row-major pixels. Whatever you write must stay linear and
    /// premultiplied.
    pub fn pixels_mut(&mut self) -> &mut [[f32; 4]] {
        &mut self.pixels
    }

    /// Index of an image-space coordinate, or `None` when it is outside.
    pub fn index_of(&self, x: i64, y: i64) -> Option<usize> {
        if x < self.rect.x || y < self.rect.y || x >= self.rect.right() || y >= self.rect.bottom() {
            return None;
        }
        let dx = (x - self.rect.x) as usize;
        let dy = (y - self.rect.y) as usize;
        Some(dy * self.rect.width as usize + dx)
    }

    /// The pixel at an image-space coordinate; fully transparent outside.
    pub fn get(&self, x: i64, y: i64) -> [f32; 4] {
        self.index_of(x, y).map_or([0.0; 4], |i| self.pixels[i])
    }

    /// Write one pixel by image-space coordinate. Out-of-range writes are
    /// ignored.
    pub fn set(&mut self, x: i64, y: i64, px: [f32; 4]) {
        if let Some(i) = self.index_of(x, y) {
            self.pixels[i] = px;
        }
    }

    /// Copy `other` into this canvas wherever the two rects overlap.
    pub fn blit_from(&mut self, other: &Canvas) {
        let x0 = self.rect.x.max(other.rect.x);
        let x1 = self.rect.right().min(other.rect.right());
        let y0 = self.rect.y.max(other.rect.y);
        let y1 = self.rect.bottom().min(other.rect.bottom());
        for y in y0..y1 {
            for x in x0..x1 {
                let (Some(d), Some(s)) = (self.index_of(x, y), other.index_of(x, y)) else {
                    continue;
                };
                self.pixels[d] = other.pixels[s];
            }
        }
    }

    /// The sub-rect of this canvas as a new canvas. Pixels of `rect` that lie
    /// outside this canvas come back fully transparent.
    pub fn sub(&self, rect: PixelRect) -> Result<Canvas, CompositeError> {
        let mut out = Canvas::transparent(rect)?;
        out.blit_from(self);
        Ok(out)
    }

    /// Convert to straight-alpha, still-linear `f32` RGBA.
    pub fn to_straight(&self) -> Vec<[f32; 4]> {
        self.pixels.iter().copied().map(unpremultiply).collect()
    }

    /// Convert to packed, straight-alpha 8-bit RGBA encoded in `space`.
    ///
    /// This is the far end of the pipeline: unpremultiply, encode out of the
    /// linear working space, quantize. Channels are clamped into `0.0..=1.0`
    /// on the way (a scene-referred highlight cannot survive 8 bits) and
    /// rounded rather than truncated, so a value that came in as an exact 8-bit
    /// code goes back out as the same code.
    pub fn to_rgba8(&self, space: &ColorSpace) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for px in &self.pixels {
            let straight = unpremultiply(*px);
            let enc = from_linear(space, [straight[0], straight[1], straight[2]]);
            out.push(quantize(enc[0]));
            out.push(quantize(enc[1]));
            out.push(quantize(enc[2]));
            out.push(quantize(straight[3]));
        }
        out
    }

    /// Convert to packed, straight-alpha 16-bit RGBA encoded in `space`.
    ///
    /// The 8-bit twin of [`Canvas::to_rgba8`]: unpremultiply, encode out of the
    /// linear working space, quantize to a 16-bit code. The composite is always
    /// `f32`, so a 16-bit-capable destination (PNG, TIFF) can carry more of it
    /// than eight bits can.
    pub fn to_rgba16(&self, space: &ColorSpace) -> Vec<u16> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for px in &self.pixels {
            let straight = unpremultiply(*px);
            let enc = from_linear(space, [straight[0], straight[1], straight[2]]);
            out.push(quantize16(enc[0]));
            out.push(quantize16(enc[1]));
            out.push(quantize16(enc[2]));
            out.push(quantize16(straight[3]));
        }
        out
    }
}

/// One channel of display-referred float to an 8-bit code.
fn quantize(v: f32) -> u8 {
    // `unit` maps non-finite to 0.0 rather than letting a NaN reach `as u8`,
    // which would be an unspecified value rather than a black pixel.
    (unit(v) * 255.0).round() as u8
}

/// One channel of display-referred float to a 16-bit code.
fn quantize16(v: f32) -> u16 {
    // Round, not truncate, so a value that came in as an exact 16-bit code
    // goes back out as the same code; `unit` keeps NaN/±inf deterministic
    // rather than relying on `as u16` saturation.
    (unit(v) * 65535.0).round() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i64, y: i64, w: u32, h: u32) -> PixelRect {
        PixelRect::new(x, y, w, h)
    }

    #[test]
    fn a_fresh_canvas_is_transparent_and_correctly_sized() {
        let c = Canvas::transparent(rect(3, 4, 5, 6)).unwrap();
        assert_eq!(c.pixels().len(), 30);
        assert!(c.pixels().iter().all(|p| *p == [0.0; 4]));
        assert_eq!(c.get(3, 4), [0.0; 4]);
        assert_eq!(c.get(0, 0), [0.0; 4], "outside the rect reads transparent");
        assert_eq!(c.index_of(0, 0), None);
        assert_eq!(c.index_of(3, 4), Some(0));
        assert_eq!(c.index_of(7, 9), Some(29));
        assert_eq!(c.index_of(8, 9), None);
    }

    #[test]
    fn a_canvas_larger_than_the_ceiling_is_refused_rather_than_allocated() {
        let err = Canvas::transparent(rect(0, 0, u32::MAX, u32::MAX)).unwrap_err();
        assert!(matches!(err, CompositeError::RegionTooLarge { .. }));
        // And the ceiling itself is expressible.
        assert_eq!(
            Canvas::area(rect(0, 0, 8192, 8192)).unwrap(),
            8192 * 8192,
            "a 64 Mpx canvas is inside the ceiling"
        );
    }

    #[test]
    fn from_pixels_rejects_a_buffer_that_does_not_fit_its_rect() {
        let err = Canvas::from_pixels(rect(0, 0, 2, 2), vec![[0.0; 4]; 3]).unwrap_err();
        assert_eq!(
            err,
            CompositeError::PixelCountMismatch {
                expected: 4,
                got: 3
            }
        );
        assert!(Canvas::from_pixels(rect(0, 0, 2, 2), vec![[0.0; 4]; 4]).is_ok());
    }

    #[test]
    fn blit_copies_only_the_overlap() {
        let mut dst = Canvas::transparent(rect(0, 0, 4, 4)).unwrap();
        let mut src = Canvas::transparent(rect(2, 2, 4, 4)).unwrap();
        for p in src.pixels_mut() {
            *p = [1.0, 0.0, 0.0, 1.0];
        }
        dst.blit_from(&src);
        assert_eq!(dst.get(2, 2), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(dst.get(3, 3), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(dst.get(1, 1), [0.0; 4], "outside the overlap is untouched");
        assert_eq!(dst.get(0, 3), [0.0; 4]);
    }

    #[test]
    fn sub_extracts_a_window_and_pads_outside_with_transparency() {
        let mut c = Canvas::transparent(rect(0, 0, 4, 4)).unwrap();
        c.set(3, 3, [0.25, 0.5, 0.75, 1.0]);
        let s = c.sub(rect(2, 2, 4, 4)).unwrap();
        assert_eq!(s.rect(), rect(2, 2, 4, 4));
        assert_eq!(s.get(3, 3), [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(s.get(5, 5), [0.0; 4]);
    }

    #[test]
    fn to_rgba8_unpremultiplies_and_encodes() {
        // Linear 0.5 premultiplied by alpha 0.5 -> stored 0.25.
        let c = Canvas::from_pixels(rect(0, 0, 1, 1), vec![[0.25, 0.0, 0.0, 0.5]]).unwrap();
        let bytes = c.to_rgba8(&ColorSpace::Srgb);
        let expect = (color::linear_to_srgb(0.5) * 255.0).round() as u8;
        assert_eq!(bytes, vec![expect, 0, 0, 128]);
        // Straight alpha 0.5 quantizes to 128 by rounding, not 127 by truncation.
        assert_eq!(bytes[3], 128);
    }

    #[test]
    fn a_fully_transparent_pixel_encodes_to_zero_rather_than_dividing() {
        let c = Canvas::from_pixels(rect(0, 0, 1, 1), vec![[0.0, 0.0, 0.0, 0.0]]).unwrap();
        assert_eq!(c.to_rgba8(&ColorSpace::Srgb), vec![0, 0, 0, 0]);
    }

    #[test]
    fn to_rgba16_premultiplies_encodes_and_rounds_to_a_16_bit_code() {
        // Linear 0.5 premultiplied by alpha 0.5 -> stored 0.25, the same
        // starting point the 8-bit twin uses.
        let c = Canvas::from_pixels(rect(0, 0, 1, 1), vec![[0.25, 0.0, 0.0, 0.5]]).unwrap();
        let bytes = c.to_rgba16(&ColorSpace::Srgb);
        let expect = (color::linear_to_srgb(0.5) * 65535.0).round() as u16;
        assert_eq!(bytes, vec![expect, 0, 0, 32768]);
        // Straight alpha 0.5 quantizes to 32768 by rounding (0.5 * 65535 =
        // 32767.5), not to 32767 by truncation.
        assert_eq!(bytes[3], 32768);
        // A NaN channel stays dark, like the 8-bit quantizer.
        let c = Canvas::from_pixels(rect(0, 0, 1, 1), vec![[f32::NAN, 0.0, 0.0, 1.0]]).unwrap();
        assert_eq!(c.to_rgba16(&ColorSpace::Srgb)[0], 0);
    }

    #[test]
    fn a_non_finite_channel_quantizes_to_black_instead_of_an_unspecified_byte() {
        // `f32::NAN as u8` is 0 in Rust but +inf as u8 saturates to 255; going
        // through `unit` makes both deterministic and dark rather than relying
        // on cast semantics.
        let c = Canvas::from_pixels(
            rect(0, 0, 2, 1),
            vec![[f32::NAN, 0.0, 0.0, 1.0], [f32::INFINITY, 0.0, 0.0, 1.0]],
        )
        .unwrap();
        let bytes = c.to_rgba8(&ColorSpace::Srgb);
        assert_eq!(bytes[0], 0);
        assert_eq!(bytes[4], 0);
    }

    #[test]
    fn an_encoded_byte_survives_a_decode_encode_round_trip() {
        for code in [0u8, 1, 17, 128, 200, 254, 255] {
            let lin = color::srgb_to_linear(code as f32 / 255.0);
            let c = Canvas::from_pixels(rect(0, 0, 1, 1), vec![[lin, lin, lin, 1.0]]).unwrap();
            assert_eq!(c.to_rgba8(&ColorSpace::Srgb)[0], code, "code {code}");
        }
    }
}
