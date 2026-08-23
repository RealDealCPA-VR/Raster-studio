//! Test images, and the vocabulary for comparing two of them.

use std::path::Path;

use raster::{CodecError, EncodeOptions, EncodedPixels, ExportFormat};

/// A deterministic, structured, fully opaque image.
///
/// Structured on purpose: a flat colour survives almost any mistake — a
/// transposed row stride, a dropped edge tile, a resample — so a fixture that
/// is flat proves nothing about geometry. This one varies on both axes and in
/// all three channels, so a pixel that ends up in the wrong place is visible in
/// the comparison rather than invisible.
pub fn photo_rgba8(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let checker = if ((x / 17) + (y / 13)) % 2 == 0 {
                40
            } else {
                0
            };
            let r = ((x * 251 / width.max(1)) % 256) as u8;
            let g = ((y * 241 / height.max(1)) % 256) as u8;
            let b = (((x ^ y) % 200) as u8).saturating_add(checker);
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    out
}

/// The same pattern with a horizontal alpha ramp, for the paths where partial
/// transparency has to survive rather than merely be tolerated.
pub fn photo_rgba8_with_alpha(width: u32, height: u32) -> Vec<u8> {
    let mut out = photo_rgba8(width, height);
    for y in 0..height {
        for x in 0..width {
            let i = ((y as usize * width as usize) + x as usize) * 4 + 3;
            out[i] = ((x * 255) / width.max(1)) as u8;
        }
    }
    out
}

/// [`photo_rgba8`] with its colour planes cycled: `(r, g, b) -> (b, r, g)`.
///
/// A *different picture of the same size and the same statistics*, which is
/// what a negative control has to be. The three planes of [`photo_rgba8`] are
/// three unrelated functions — a ramp in `x`, a ramp in `y`, and an `x ^ y`
/// pattern with a checker on top — so cycling them moves every channel of
/// almost every pixel while leaving the histogram, the size and the full
/// opacity alone. A control that differed only in alpha would prove nothing
/// about a container that has no alpha channel.
pub fn photo_rgba8_channels_cycled(width: u32, height: u32) -> Vec<u8> {
    let mut out = photo_rgba8(width, height);
    for px in out.chunks_exact_mut(4) {
        let (r, g, b) = (px[0], px[1], px[2]);
        px[0] = b;
        px[1] = r;
        px[2] = g;
    }
    out
}

/// Encode an image and write it where a user would have one.
pub fn write_image(
    path: &Path,
    format: ExportFormat,
    width: u32,
    height: u32,
    rgba8: &[u8],
) -> Result<(), CodecError> {
    raster::encode_to_path(
        path,
        format,
        width,
        height,
        EncodedPixels::Rgba8(rgba8),
        &EncodeOptions::default(),
    )
}

/// One pixel of a row-major RGBA8 buffer.
pub fn pixel_at(buf: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y as usize * width as usize) + x as usize) * 4;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// Every pixel coordinate at which two RGBA8 buffers of the same size differ.
///
/// The whole list rather than a count, because "the stroke changed pixels it
/// should not have" is only a useful failure if the test can say *which*.
pub fn differing_pixels(a: &[u8], b: &[u8], width: u32) -> Vec<(u32, u32)> {
    assert_eq!(a.len(), b.len(), "buffers must be the same size to compare");
    let mut out = Vec::new();
    for (i, (pa, pb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        if pa != pb {
            out.push(((i as u32) % width, (i as u32) / width));
        }
    }
    out
}

/// The largest single-channel difference between two RGBA8 buffers.
pub fn max_channel_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len(), "buffers must be the same size to compare");
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// The mean single-channel difference between two RGBA8 buffers.
pub fn mean_channel_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "buffers must be the same size to compare");
    if a.is_empty() {
        return 0.0;
    }
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    total as f64 / a.len() as f64
}

/// A linear-light value as the 8-bit sRGB code the pipeline writes for it.
///
/// The same two steps `Canvas::to_rgba8` takes — encode, then round — so a
/// hand-computed expectation and the pipeline quantize identically.
pub fn srgb8_of_linear(v: f32) -> u8 {
    (color::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8
}

/// A linear-light value as the 8-bit code a document whose working space *is*
/// linear stores for it.
///
/// `color::from_linear` is the identity for [`color::ColorSpace::LinearSrgb`],
/// so `Canvas::to_rgba8` reduces to exactly this: clamp, scale, round. That is
/// what lets a hand-computed reference be compared to a composited frame as
/// bytes rather than within a tolerance.
pub fn linear8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}
