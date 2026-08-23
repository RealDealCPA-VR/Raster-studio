//! The pixel buffer every filter reads and writes.

use color::{linear_to_srgb, premultiply, srgb8_to_linear, unpremultiply};
use raster::{PixelFormat, TileGrid};

use crate::support::{EdgeMode, Interpolation, Sampling};

/// Failures a filter can report.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FilterError {
    #[error("pixel count mismatch: {width}x{height} needs {expected} pixels, got {got}")]
    BadLength {
        width: u32,
        height: u32,
        expected: usize,
        got: usize,
    },
    #[error("{width}x{height} does not fit in memory")]
    TooLarge { width: u32, height: u32 },
    #[error("buffers differ in size: {a:?} vs {b:?}")]
    SizeMismatch { a: (u32, u32), b: (u32, u32) },
    #[error("convolution kernel must be square and odd-sized, got size {size} with {len} weights")]
    BadKernel { size: u32, len: usize },
    #[error("only PixelFormat::Rgba8 tile grids can be converted, got {format:?}")]
    UnsupportedFormat { format: PixelFormat },
    #[error("a gradient needs at least one colour stop")]
    EmptyGradient,
    #[error("tile grid conversion failed: {0}")]
    Grid(String),
}

/// A rectangular plane of **premultiplied, linear** RGBA pixels.
///
/// This is the working representation for the whole crate. Two properties are
/// load-bearing and every filter relies on them:
///
/// * **Linear light.** Values are scene-referred linear sRGB, not gamma
///   encoded. Averaging two pixels here is averaging light, which is what
///   makes a blur of a red and a green pixel look like the eye expects rather
///   than muddy. The two filters that are *defined* on gamma-encoded values —
///   [`crate::stylize::solarize`] and [`crate::pixelate::color_halftone`] —
///   encode, transform, and decode explicitly, and say so.
/// * **Premultiplied alpha.** A weighted average of premultiplied pixels is
///   the correct composite; the same average on straight alpha bleeds the
///   colour of fully transparent pixels into the result. Filters that are
///   *not* linear in the pixel value (noise, median, solarize) unpremultiply
///   first, operate, and premultiply back — each one documents that it does.
///
/// Channel values are not clamped: highlights above `1.0` pass through. Only
/// filters that can overshoot re-clamp, and they say so.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterBuffer {
    width: u32,
    height: u32,
    pixels: Vec<[f32; 4]>,
}

impl FilterBuffer {
    /// Number of pixels a `width * height` buffer needs, or `None` on overflow.
    fn area(width: u32, height: u32) -> Option<usize> {
        (width as usize).checked_mul(height as usize)
    }

    /// A fully transparent buffer. A zero dimension yields an empty buffer,
    /// which every filter accepts and returns unchanged.
    pub fn transparent(width: u32, height: u32) -> Result<Self, FilterError> {
        Self::filled(width, height, [0.0; 4])
    }

    /// A buffer where every pixel is `px` (already premultiplied and linear).
    pub fn filled(width: u32, height: u32, px: [f32; 4]) -> Result<Self, FilterError> {
        let n = Self::area(width, height).ok_or(FilterError::TooLarge { width, height })?;
        Ok(Self {
            width,
            height,
            pixels: vec![px; n],
        })
    }

    /// Wrap an existing row-major pixel vector.
    pub fn from_pixels(
        width: u32,
        height: u32,
        pixels: Vec<[f32; 4]>,
    ) -> Result<Self, FilterError> {
        let expected = Self::area(width, height).ok_or(FilterError::TooLarge { width, height })?;
        if pixels.len() != expected {
            return Err(FilterError::BadLength {
                width,
                height,
                expected,
                got: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Decode packed straight-alpha sRGB8 into linear premultiplied pixels.
    pub fn from_rgba8(width: u32, height: u32, src: &[u8]) -> Result<Self, FilterError> {
        let n = Self::area(width, height).ok_or(FilterError::TooLarge { width, height })?;
        let expected = n
            .checked_mul(4)
            .ok_or(FilterError::TooLarge { width, height })?;
        if src.len() != expected {
            return Err(FilterError::BadLength {
                width,
                height,
                expected,
                got: src.len(),
            });
        }
        let pixels = src
            .chunks_exact(4)
            .map(|p| {
                premultiply([
                    srgb8_to_linear(p[0]),
                    srgb8_to_linear(p[1]),
                    srgb8_to_linear(p[2]),
                    p[3] as f32 / 255.0,
                ])
            })
            .collect();
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Encode back to packed straight-alpha sRGB8.
    ///
    /// Colour goes through the sRGB transfer curve; alpha does not — alpha is
    /// a coverage fraction, never gamma encoded.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for px in &self.pixels {
            let s = unpremultiply(*px);
            for c in s.iter().take(3) {
                out.push(quantize8(linear_to_srgb(*c)));
            }
            out.push(quantize8(s[3]));
        }
        out
    }

    /// Decode an 8-bit tile grid into a filter buffer.
    ///
    /// Absent tiles and edge-tile padding decode as transparent black, exactly
    /// as [`TileGrid::to_rgba8`] reports them.
    pub fn from_tile_grid(grid: &TileGrid) -> Result<Self, FilterError> {
        if grid.format() != PixelFormat::Rgba8 {
            return Err(FilterError::UnsupportedFormat {
                format: grid.format(),
            });
        }
        let (w, h) = grid.dimensions();
        let bytes = grid
            .to_rgba8()
            .map_err(|e| FilterError::Grid(e.to_string()))?;
        Self::from_rgba8(w, h, &bytes)
    }

    /// Encode into a fresh 8-bit tile grid at mip level 0.
    pub fn to_tile_grid(&self) -> Result<TileGrid, FilterError> {
        TileGrid::from_rgba8(self.width, self.height, &self.to_rgba8())
            .map_err(|e| FilterError::Grid(e.to_string()))
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// True when the buffer covers no pixels. Every filter returns early on
    /// an empty buffer rather than dividing by a zero dimension.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Number of pixels.
    pub fn len(&self) -> usize {
        self.pixels.len()
    }

    pub fn pixels(&self) -> &[[f32; 4]] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [[f32; 4]] {
        &mut self.pixels
    }

    /// A same-sized buffer of transparent pixels, for a filter's destination.
    pub(crate) fn same_size_blank(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            pixels: vec![[0.0; 4]; self.pixels.len()],
        }
    }

    /// Read an in-bounds pixel. Panics only on a programming error inside the
    /// crate; every public path goes through [`FilterBuffer::at`].
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> [f32; 4] {
        self.pixels[y as usize * self.width as usize + x as usize]
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, px: [f32; 4]) {
        let w = self.width as usize;
        self.pixels[y as usize * w + x as usize] = px;
    }

    /// Read a pixel at integer coordinates, resolving out-of-bounds through
    /// `edge`. Returns transparent black only for an empty buffer.
    #[inline]
    pub fn at(&self, x: i64, y: i64, edge: EdgeMode) -> [f32; 4] {
        let (Some(sx), Some(sy)) = (edge.map(x, self.width), edge.map(y, self.height)) else {
            return [0.0; 4];
        };
        self.pixels[sy * self.width as usize + sx]
    }

    /// Sample at continuous coordinates, where `(0.5, 0.5)` is the centre of
    /// the top-left pixel.
    ///
    /// Non-finite coordinates resolve to the clamped edge rather than
    /// panicking on an out-of-range cast: a distort whose maths degenerates
    /// must produce a pixel, not a crash.
    #[inline]
    pub fn sample(&self, x: f32, y: f32, s: Sampling) -> [f32; 4] {
        if self.is_empty() {
            return [0.0; 4];
        }
        match s.interp {
            // Pixel centres sit at `i + 0.5`, so the pixel containing a
            // continuous coordinate is simply its floor.
            Interpolation::Nearest => self.at(floor_i64(x), floor_i64(y), s.edge),
            Interpolation::Bilinear => self.sample_bilinear(x, y, s.edge),
            Interpolation::Bicubic => self.sample_bicubic(x, y, s.edge),
        }
    }

    /// Bilinear sample. Never overshoots the input range, so premultiplied
    /// pixels stay valid without clamping.
    pub fn sample_bilinear(&self, x: f32, y: f32, edge: EdgeMode) -> [f32; 4] {
        if self.is_empty() {
            return [0.0; 4];
        }
        let fx = x - 0.5;
        let fy = y - 0.5;
        let x0 = floor_i64(fx);
        let y0 = floor_i64(fy);
        let tx = fract_or_zero(fx, x0);
        let ty = fract_or_zero(fy, y0);
        let p00 = self.at(x0, y0, edge);
        let p10 = self.at(x0 + 1, y0, edge);
        let p01 = self.at(x0, y0 + 1, edge);
        let p11 = self.at(x0 + 1, y0 + 1, edge);
        let mut out = [0.0f32; 4];
        for c in 0..4 {
            let a = p00[c] + (p10[c] - p00[c]) * tx;
            let b = p01[c] + (p11[c] - p01[c]) * tx;
            out[c] = a + (b - a) * ty;
        }
        out
    }

    /// Catmull-Rom bicubic sample.
    ///
    /// Sharper than bilinear but the outer taps carry negative weight, so the
    /// result can ring past the input range. Alpha is re-clamped into
    /// `[0, 1]`; colour is left alone because the working space is
    /// scene-referred and legitimately exceeds `1.0`.
    pub fn sample_bicubic(&self, x: f32, y: f32, edge: EdgeMode) -> [f32; 4] {
        if self.is_empty() {
            return [0.0; 4];
        }
        let fx = x - 0.5;
        let fy = y - 0.5;
        let x0 = floor_i64(fx);
        let y0 = floor_i64(fy);
        let tx = fract_or_zero(fx, x0);
        let ty = fract_or_zero(fy, y0);
        let wx = catmull_rom_weights(tx);
        let wy = catmull_rom_weights(ty);
        let mut out = [0.0f32; 4];
        for (j, &wyj) in wy.iter().enumerate() {
            let mut row = [0.0f32; 4];
            for (i, &wxi) in wx.iter().enumerate() {
                let p = self.at(x0 - 1 + i as i64, y0 - 1 + j as i64, edge);
                for c in 0..4 {
                    row[c] += p[c] * wxi;
                }
            }
            for c in 0..4 {
                out[c] += row[c] * wyj;
            }
        }
        out[3] = out[3].clamp(0.0, 1.0);
        out
    }

    /// Transpose the buffer.
    ///
    /// The separable passes run vertically by transposing, running the
    /// horizontal pass, and transposing back. That keeps exactly one 1D
    /// implementation — a second, column-major copy is where separable blurs
    /// usually pick up an edge-handling bug that only shows on one axis.
    pub(crate) fn transposed(&self) -> Self {
        let mut out = Self {
            width: self.height,
            height: self.width,
            pixels: vec![[0.0; 4]; self.pixels.len()],
        };
        if self.is_empty() {
            return out;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        for y in 0..h {
            for x in 0..w {
                out.pixels[x * h + y] = self.pixels[y * w + x];
            }
        }
        out
    }
}

/// Clamp a premultiplied pixel back into a valid range.
///
/// Alpha into `[0, 1]`, and each colour channel into `[0, alpha]` — the
/// premultiplied form of "a channel cannot be more opaque than the pixel".
/// Only filters that can overshoot (sharpening, noise) apply this, and they
/// document it, because it would otherwise clip legitimate scene-referred
/// highlights.
#[inline]
pub(crate) fn clamp_premultiplied(px: [f32; 4]) -> [f32; 4] {
    let a = px[3].clamp(0.0, 1.0);
    [
        px[0].clamp(0.0, a),
        px[1].clamp(0.0, a),
        px[2].clamp(0.0, a),
        a,
    ]
}

#[inline]
pub(crate) fn floor_i64(v: f32) -> i64 {
    if v.is_nan() {
        return 0;
    }
    let f = v.floor();
    if f <= i64::MIN as f32 {
        i64::MIN / 2
    } else if f >= i64::MAX as f32 {
        i64::MAX / 2
    } else {
        f as i64
    }
}

/// Fractional part of a sample coordinate, or zero when the coordinate is not
/// finite. A `NaN` coordinate would otherwise poison every interpolation
/// weight and hand back a `NaN` pixel; a degenerate distort must produce a
/// pixel, not garbage.
#[inline]
fn fract_or_zero(v: f32, floor: i64) -> f32 {
    if v.is_finite() {
        (v - floor as f32).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[inline]
fn catmull_rom_weights(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ]
}

#[inline]
fn quantize8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::Interpolation;

    fn ramp(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                px.push([x as f32 * 0.01, y as f32 * 0.02, 0.25, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn rgba8_round_trips_within_one_code() {
        let mut bytes = Vec::new();
        for i in 0..256u32 {
            bytes.extend_from_slice(&[i as u8, (255 - i) as u8, (i / 2) as u8, 255]);
        }
        let buf = FilterBuffer::from_rgba8(16, 16, &bytes).unwrap();
        assert_eq!(buf.to_rgba8(), bytes);
    }

    #[test]
    fn transparent_pixels_decode_to_zero_and_re_encode_to_zero() {
        let bytes = vec![0u8; 4 * 4];
        let buf = FilterBuffer::from_rgba8(2, 2, &bytes).unwrap();
        assert_eq!(buf.pixels(), &[[0.0; 4]; 4]);
        assert_eq!(buf.to_rgba8(), bytes);
    }

    #[test]
    fn decoding_premultiplies() {
        // 50% alpha, full red. Premultiplied red must be halved.
        let buf = FilterBuffer::from_rgba8(1, 1, &[255, 0, 0, 128]).unwrap();
        let px = buf.get(0, 0);
        let a = 128.0 / 255.0;
        assert!((px[3] - a).abs() < 1e-6);
        assert!((px[0] - a).abs() < 1e-6, "{px:?}");
    }

    #[test]
    fn bad_length_is_reported_not_panicked() {
        let err = FilterBuffer::from_rgba8(2, 2, &[0; 3]).unwrap_err();
        assert!(matches!(err, FilterError::BadLength { .. }));
        let err = FilterBuffer::from_pixels(2, 2, vec![[0.0; 4]; 3]).unwrap_err();
        assert!(matches!(err, FilterError::BadLength { .. }));
    }

    #[test]
    fn zero_sized_buffers_are_legal_and_empty() {
        let b = FilterBuffer::transparent(0, 8).unwrap();
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
        assert_eq!(b.sample(3.0, 3.0, Sampling::clamped()), [0.0; 4]);
        assert_eq!(b.at(0, 0, EdgeMode::Clamp), [0.0; 4]);
    }

    #[test]
    fn bilinear_at_a_pixel_centre_is_that_pixel() {
        let b = ramp(5, 4);
        for y in 0..4 {
            for x in 0..5 {
                let s = b.sample_bilinear(x as f32 + 0.5, y as f32 + 0.5, EdgeMode::Clamp);
                assert_eq!(s, b.get(x, y), "at {x},{y}");
            }
        }
    }

    #[test]
    fn bilinear_midpoint_is_the_average_of_two_neighbours() {
        let b = ramp(5, 4);
        let s = b.sample_bilinear(2.0, 1.5, EdgeMode::Clamp);
        let a = b.get(1, 1);
        let c = b.get(2, 1);
        for i in 0..4 {
            assert!((s[i] - 0.5 * (a[i] + c[i])).abs() < 1e-6);
        }
    }

    #[test]
    fn bicubic_at_a_pixel_centre_is_that_pixel() {
        let b = ramp(6, 6);
        let s = b.sample_bicubic(3.5, 2.5, EdgeMode::Clamp);
        let e = b.get(3, 2);
        for i in 0..4 {
            assert!((s[i] - e[i]).abs() < 1e-5, "{s:?} vs {e:?}");
        }
    }

    /// Catmull-Rom reproduces a linear ramp exactly — the classic check that
    /// the weights are right and are applied to the right taps.
    #[test]
    fn bicubic_reproduces_a_linear_ramp() {
        let b = ramp(8, 8);
        let s = b.sample_bicubic(4.25, 4.5, EdgeMode::Clamp);
        assert!((s[0] - 3.75 * 0.01).abs() < 1e-5, "{s:?}");
    }

    #[test]
    fn sampling_a_constant_buffer_is_that_constant_in_every_mode() {
        let c = [0.3, 0.4, 0.5, 0.8];
        let b = FilterBuffer::filled(3, 3, c).unwrap();
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            for interp in [
                Interpolation::Nearest,
                Interpolation::Bilinear,
                Interpolation::Bicubic,
            ] {
                let s = b.sample(-4.3, 9.1, Sampling::new(edge, interp));
                for i in 0..4 {
                    assert!((s[i] - c[i]).abs() < 1e-6, "{edge:?}/{interp:?}: {s:?}");
                }
            }
        }
    }

    #[test]
    fn non_finite_sample_coordinates_do_not_panic() {
        let b = ramp(4, 4);
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30] {
            for interp in [
                Interpolation::Nearest,
                Interpolation::Bilinear,
                Interpolation::Bicubic,
            ] {
                let s = b.sample(v, v, Sampling::new(EdgeMode::Clamp, interp));
                assert!(s[3].is_finite(), "{v} {interp:?} -> {s:?}");
            }
        }
    }

    #[test]
    fn transpose_is_an_involution() {
        let b = ramp(7, 3);
        let t = b.transposed();
        assert_eq!(t.dimensions(), (3, 7));
        assert_eq!(t.get(2, 5), b.get(5, 2));
        assert_eq!(t.transposed(), b);
    }

    #[test]
    fn clamp_premultiplied_keeps_colour_under_alpha() {
        assert_eq!(
            clamp_premultiplied([1.5, -0.2, 0.3, 0.5]),
            [0.5, 0.0, 0.3, 0.5]
        );
        assert_eq!(
            clamp_premultiplied([0.1, 0.1, 0.1, 2.0]),
            [0.1, 0.1, 0.1, 1.0]
        );
    }

    #[test]
    fn tile_grid_round_trip_preserves_bytes() {
        let mut bytes = Vec::new();
        for i in 0..(300 * 300) {
            bytes.extend_from_slice(&[(i % 251) as u8, (i % 253) as u8, (i % 249) as u8, 255]);
        }
        let grid = TileGrid::from_rgba8(300, 300, &bytes).unwrap();
        let buf = FilterBuffer::from_tile_grid(&grid).unwrap();
        assert_eq!(buf.dimensions(), (300, 300));
        let back = buf.to_tile_grid().unwrap();
        assert_eq!(back.to_rgba8().unwrap(), bytes);
    }

    #[test]
    fn non_rgba8_grids_are_refused() {
        let grid = TileGrid::new(4, 4, PixelFormat::RgbaF32);
        assert_eq!(
            FilterBuffer::from_tile_grid(&grid).unwrap_err(),
            FilterError::UnsupportedFormat {
                format: PixelFormat::RgbaF32
            }
        );
    }
}
