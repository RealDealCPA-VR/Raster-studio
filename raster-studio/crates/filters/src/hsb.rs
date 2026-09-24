//! Filter ▸ Other ▸ HSB/HSL: reinterpret the three colour channels.
//!
//! Photoshop's filter of the same name does not *adjust* anything. It reads
//! each pixel's three channels in one model (RGB, HSB or HSL) and writes the
//! same colour's coordinates in another model *into the R, G and B channels*.
//! "RGB in, HSB out" therefore stores hue in red, saturation in green and
//! brightness in blue — which is how the filter is used to edit hue or
//! saturation as a grey channel — and running it again with the models
//! swapped ("HSB in, RGB out") takes the image back.
//!
//! # Leaves linear space
//!
//! Hue, saturation, brightness and lightness are defined on gamma-encoded
//! values; that is what the numbers mean in every editor that shows them. So
//! this filter encodes each channel to sRGB, converts, and decodes again,
//! exactly as [`crate::stylize::solarize`] does. Converting the linear values
//! directly would give a different, darker "brightness" channel than every
//! other application and would not round-trip through them.
//!
//! Alpha is unpremultiplied before the conversion and premultiplied after it,
//! so a half-covered pixel is converted as its colour, not as a darker one.
//! Encoded values are clamped into `[0, 1]` first: hue is undefined outside
//! the unit cube.

use color::{linear_to_srgb, premultiply, srgb_to_linear, unpremultiply};
use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::support::fill_tiles;

/// A colour model the filter reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ChannelModel {
    /// The channels are plain red, green and blue.
    #[default]
    Rgb,
    /// Hue, saturation, brightness (value): the hexcone model.
    Hsb,
    /// Hue, saturation, lightness: the double-cone model.
    Hsl,
}

/// Convert every pixel's channels from `input` to `output`.
///
/// `input == output` is the identity (the buffer is returned untouched, not
/// round-tripped through sRGB).
pub fn hsb_hsl(src: &FilterBuffer, input: ChannelModel, output: ChannelModel) -> FilterBuffer {
    if src.is_empty() || input == output {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let s = unpremultiply(src.get(x, y));
        if s[3] <= 0.0 {
            return src.get(x, y);
        }
        let enc = [
            linear_to_srgb(s[0]).clamp(0.0, 1.0),
            linear_to_srgb(s[1]).clamp(0.0, 1.0),
            linear_to_srgb(s[2]).clamp(0.0, 1.0),
        ];
        let rgb = match input {
            ChannelModel::Rgb => enc,
            ChannelModel::Hsb => hsb_to_rgb(enc),
            ChannelModel::Hsl => hsl_to_rgb(enc),
        };
        let res = match output {
            ChannelModel::Rgb => rgb,
            ChannelModel::Hsb => rgb_to_hsb(rgb),
            ChannelModel::Hsl => rgb_to_hsl(rgb),
        };
        premultiply([
            srgb_to_linear(res[0]),
            srgb_to_linear(res[1]),
            srgb_to_linear(res[2]),
            s[3],
        ])
    });
    out
}

/// Hue in `[0, 1)` of an encoded RGB triple with the given max and chroma.
fn hue(rgb: [f32; 3], max: f32, chroma: f32) -> f32 {
    if chroma <= 0.0 {
        return 0.0;
    }
    let [r, g, b] = rgb;
    let h = if max == r {
        ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        (b - r) / chroma + 2.0
    } else {
        (r - g) / chroma + 4.0
    };
    (h / 6.0).rem_euclid(1.0)
}

/// Encoded RGB to hue, saturation, brightness, each in `[0, 1]`.
pub fn rgb_to_hsb(rgb: [f32; 3]) -> [f32; 3] {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let chroma = max - min;
    let s = if max > 0.0 { chroma / max } else { 0.0 };
    [hue(rgb, max, chroma), s, max]
}

/// Encoded RGB to hue, saturation, lightness, each in `[0, 1]`.
pub fn rgb_to_hsl(rgb: [f32; 3]) -> [f32; 3] {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let chroma = max - min;
    let l = 0.5 * (max + min);
    let denom = 1.0 - (2.0 * l - 1.0).abs();
    let s = if denom > 1e-6 {
        (chroma / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    [hue(rgb, max, chroma), s, l]
}

/// Rebuild RGB from a hue in `[0, 1)`, a chroma and the offset added to all
/// three channels.
fn from_hue(h: f32, chroma: f32, m: f32) -> [f32; 3] {
    let hp = h.rem_euclid(1.0) * 6.0;
    let x = chroma * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// Hue, saturation, brightness back to encoded RGB.
pub fn hsb_to_rgb(hsb: [f32; 3]) -> [f32; 3] {
    let [h, s, v] = hsb;
    let chroma = v * s;
    from_hue(h, chroma, v - chroma)
}

/// Hue, saturation, lightness back to encoded RGB.
pub fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let [h, s, l] = hsl;
    let chroma = (1.0 - (2.0 * l - 1.0).abs()) * s;
    from_hue(h, chroma, l - 0.5 * chroma)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_image() -> FilterBuffer {
        let mut b = FilterBuffer::transparent(16, 16).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let px = [
                    srgb_to_linear(x as f32 / 15.0),
                    srgb_to_linear(y as f32 / 15.0),
                    srgb_to_linear(((x * 7 + y * 3) % 16) as f32 / 15.0),
                    if x == 3 { 0.5 } else { 1.0 },
                ];
                b.set(x, y, premultiply(px));
            }
        }
        b
    }

    fn max_diff(a: &FilterBuffer, b: &FilterBuffer) -> f32 {
        a.pixels()
            .iter()
            .zip(b.pixels())
            .flat_map(|(p, q)| (0..4).map(move |c| (p[c] - q[c]).abs()))
            .fold(0.0, f32::max)
    }

    #[test]
    fn rgb_to_hsb_and_back_round_trips() {
        let src = sample_image();
        let hsb = hsb_hsl(&src, ChannelModel::Rgb, ChannelModel::Hsb);
        assert!(max_diff(&hsb, &src) > 0.1, "the forward pass changes the image");
        let back = hsb_hsl(&hsb, ChannelModel::Hsb, ChannelModel::Rgb);
        let d = max_diff(&back, &src);
        assert!(d < 1.0 / 255.0, "RGB -> HSB -> RGB drifted by {d}");
    }

    #[test]
    fn rgb_to_hsl_and_back_round_trips() {
        let src = sample_image();
        let hsl = hsb_hsl(&src, ChannelModel::Rgb, ChannelModel::Hsl);
        let back = hsb_hsl(&hsl, ChannelModel::Hsl, ChannelModel::Rgb);
        let d = max_diff(&back, &src);
        assert!(d < 1.0 / 255.0, "RGB -> HSL -> RGB drifted by {d}");
    }

    #[test]
    fn pure_red_lands_as_hue_zero_full_saturation_full_brightness() {
        let red = FilterBuffer::filled(1, 1, [1.0, 0.0, 0.0, 1.0]).unwrap();
        let hsb = hsb_hsl(&red, ChannelModel::Rgb, ChannelModel::Hsb);
        let p = hsb.get(0, 0);
        assert!(p[0].abs() < 1e-5 && (p[1] - 1.0).abs() < 1e-5 && (p[2] - 1.0).abs() < 1e-5);
        // Blue is hue 2/3: stored encoded, so decode before comparing.
        let blue = FilterBuffer::filled(1, 1, [0.0, 0.0, 1.0, 1.0]).unwrap();
        let p = hsb_hsl(&blue, ChannelModel::Rgb, ChannelModel::Hsl).get(0, 0);
        assert!((linear_to_srgb(p[0]) - 2.0 / 3.0).abs() < 1e-4, "{p:?}");
        assert!((linear_to_srgb(p[2]) - 0.5).abs() < 1e-4, "{p:?}");
    }

    #[test]
    fn same_model_is_the_identity() {
        let src = sample_image();
        assert_eq!(hsb_hsl(&src, ChannelModel::Hsl, ChannelModel::Hsl), src);
    }
}
