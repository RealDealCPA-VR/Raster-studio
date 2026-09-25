//! W13-J: the Filter Gallery's Distort set (Diffuse Glow, Glass, Ocean Ripple)
//! and Stylize set (Glowing Edges).
//!
//! The sliders, their ranges and defaults are Photopea's (its `DfsG`, `Gls`,
//! `OcnR` and `GlwE` descriptors and gallery controls; Glass's seven textures
//! in its order, with Invert Texture). The pictures they make are this
//! build's: Photopea's gallery kernels are not ported, and Glass's textures
//! are procedural, not Photopea's texture images.
//!
//! These follow [`crate::gallery_sets`]' rules exactly: each is a pure
//! function of the pixels, its integer slider values and a fixed seed; colour
//! is handled as **straight** linear RGB in `[0, 1]`; and alpha is handed
//! through unchanged, pixel for pixel (the distorting effects move colour, not
//! coverage). The gallery calls these through
//! [`crate::gallery_sets::GalleryEffect::apply`], which resolves and clamps the
//! slider values first.

use color::{premultiply, unpremultiply};

use crate::blur::gaussian_blur;
use crate::buffer::FilterBuffer;
use crate::rng::{hash_unit, Perlin};
use crate::support::{fill_tiles, EdgeMode, Sampling};

fn luma(c: [f32; 4]) -> f32 {
    (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).clamp(0.0, 1.0)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `f` over every pixel's straight colour; alpha is the source's.
fn map_straight(
    src: &FilterBuffer,
    f: impl Fn(u32, u32, [f32; 4]) -> [f32; 3] + Sync,
) -> FilterBuffer {
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let s = unpremultiply(src.get(x, y));
        let c = f(x, y, s);
        premultiply([
            c[0].clamp(0.0, 1.0),
            c[1].clamp(0.0, 1.0),
            c[2].clamp(0.0, 1.0),
            s[3],
        ])
    });
    out
}

/// Displace colour by `offset(x, y)` (in pixels), keeping each pixel's own
/// alpha.
fn displace_straight(
    src: &FilterBuffer,
    offset: impl Fn(f32, f32) -> (f32, f32) + Sync,
) -> FilterBuffer {
    let sampling = Sampling::clamped();
    map_straight(src, |x, y, _| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let (ox, oy) = offset(px, py);
        let s = unpremultiply(src.sample(px + ox, py + oy, sampling));
        [s[0], s[1], s[2]]
    })
}

/// Diffuse Glow: a grainy glow of white (the default background colour) over
/// the highlights.
///
/// `graininess` (0..=10) scatters seeded noise through the glow; `glow`
/// (0..=20) is how strongly the highlights are washed towards white; `clear`
/// (0..=20) raises the brightness the glow starts at, clearing it from the
/// mid-tones.
pub fn diffuse_glow(
    src: &FilterBuffer,
    graininess: f32,
    glow: f32,
    clear: f32,
    seed: u64,
) -> FilterBuffer {
    let start = 0.9 * clear / 20.0;
    let amount = glow / 20.0;
    let grain = graininess / 10.0 * 0.35;
    map_straight(src, |x, y, c| {
        let n = (hash_unit(seed, i64::from(x), i64::from(y)) - 0.5) * grain;
        let g = (amount * smoothstep(start, 1.0, luma(c) + n)).clamp(0.0, 1.0);
        [0, 1, 2].map(|i| c[i] + n * 0.5 + (1.0 - c[i]) * g)
    })
}

/// A glass texture's height field at a point, in `[0, 1]`, for Photopea's
/// seven textures in its order: Blocks, Canvas, Frosted, Tiny Lens, Brick,
/// Burlap, Sandstone. `s` is the texture scale in pixels. The patterns are
/// this build's procedural ones, not Photopea's texture images.
fn glass_height(texture: u32, perlin: &Perlin, seed: u64, x: f32, y: f32, s: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    match texture {
        // Blocks: a random height per square cell.
        0 => {
            let c = s * 0.5;
            hash_unit(seed, (x / c).floor() as i64, (y / c).floor() as i64)
        }
        // Canvas: a woven cross-hatch.
        1 => {
            let k = tau / (s * 0.2);
            0.5 + 0.25 * ((x * k).sin() + (y * k).sin())
        }
        // Frosted: fine noise.
        2 => 0.5 + 0.5 * perlin.fbm(x / (s * 0.25), y / (s * 0.25), 2, 0.5),
        // Tiny Lens: a dome in each cell.
        3 => {
            let c = s * 0.4;
            let (u, v) = ((x / c).fract() - 0.5, (y / c).fract() - 0.5);
            (1.0 - 4.0 * (u * u + v * v)).max(0.0).sqrt()
        }
        // Brick: courses of bricks, every other one offset half a brick,
        // with sunken mortar.
        4 => {
            let (bw, bh) = (s * 0.66, s * 0.33);
            let row = (y / bh).floor();
            let shift = if row as i64 % 2 == 0 { 0.0 } else { bw * 0.5 };
            let (bx, by) = ((x + shift).rem_euclid(bw), y.rem_euclid(bh));
            if bx < bw * 0.08 || by < bh * 0.15 {
                0.0
            } else {
                1.0
            }
        }
        // Burlap: a coarse basket weave.
        5 => {
            let c = s * 0.25;
            let over = ((x / c).floor() as i64 + (y / c).floor() as i64) % 2 == 0;
            let t = if over { x } else { y };
            0.5 + 0.5 * (tau * t / c).sin()
        }
        // Sandstone: grainy, coarser noise.
        _ => 0.5 + 0.5 * perlin.fbm(x / (s * 0.12) + 0.37, y / (s * 0.12) + 0.61, 4, 0.6),
    }
}

/// Glass: look at the layer through a textured pane.
///
/// Photopea's controls: `distortion` (0..=20) is how far the texture bends
/// the image, `smoothness` (1..=15) blurs the texture's relief, `texture`
/// picks one of seven (0..=6, as [`glass_height`] lists them; Photopea's
/// default is Tiny Lens, 3), `scaling` (50..=200 %) sizes it, and `invert`
/// turns the relief inside out (Photopea negates the distortion).
pub fn glass(
    src: &FilterBuffer,
    distortion: f32,
    smoothness: f32,
    texture: f32,
    scaling: f32,
    invert: bool,
    seed: u64,
) -> FilterBuffer {
    let (w, h) = src.dimensions();
    let s = 24.0 * scaling / 100.0;
    let kind = texture.clamp(0.0, 6.0) as u32;
    let perlin = Perlin::new(seed);
    // The height field as a buffer, so the gallery's own Gaussian can smooth
    // it.
    let mut field = FilterBuffer::transparent(w, h).expect("same size as the source");
    fill_tiles(w, h, field.pixels_mut(), |x, y| {
        let v = glass_height(kind, &perlin, seed, x as f32 + 0.5, y as f32 + 0.5, s);
        [v, v, v, 1.0]
    });
    let field = gaussian_blur(&field, smoothness * 0.5, EdgeMode::Clamp);
    let at = |x: f32, y: f32| field.sample(x, y, Sampling::clamped())[0];
    let k = distortion * 1.5 * if invert { -1.0 } else { 1.0 };
    displace_straight(src, |x, y| {
        let gx = at(x + 1.0, y) - at(x - 1.0, y);
        let gy = at(x, y + 1.0) - at(x, y - 1.0);
        (gx * k * 4.0, gy * k * 4.0)
    })
}

/// Ocean Ripple: a random, rippling surface over the layer, as if under water.
///
/// `size` (1..=15) is the ripple size and `magnitude` (0..=20) how far the
/// ripples bend the image.
pub fn ocean_ripple(src: &FilterBuffer, size: f32, magnitude: f32, seed: u64) -> FilterBuffer {
    let s = 3.0 + size * 2.0;
    let a = Perlin::new(seed);
    let b = Perlin::new(seed ^ 0x0CEA_2177);
    let m = magnitude * 0.6;
    displace_straight(src, |x, y| {
        // Folding the noise through a sine makes crests, not blobs.
        let n1 = a.fbm(x / s, y / s, 2, 0.5);
        let n2 = b.fbm(x / s, y / s, 2, 0.5);
        (
            m * (n1 * std::f32::consts::PI * 2.0).sin(),
            m * (n2 * std::f32::consts::PI * 2.0).sin(),
        )
    })
}

/// Glowing Edges: the layer's edges glow in their own colour on black.
///
/// `width` (1..=14) is the edge detector's reach in pixels, `brightness`
/// (0..=20) the glow's strength, and `smoothness` (1..=15) how much the image
/// is smoothed before the edges are found, which drops fine detail.
pub fn glowing_edges(
    src: &FilterBuffer,
    width: f32,
    brightness: f32,
    smoothness: f32,
) -> FilterBuffer {
    let soft = gaussian_blur(src, smoothness * 0.4, EdgeMode::Clamp);
    let r = width.max(1.0);
    let gain = brightness / 4.0 + 0.25;
    let at = |x: f32, y: f32| unpremultiply(soft.sample(x, y, Sampling::clamped()));
    map_straight(src, |x, y, _| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let (l, rt) = (at(px - r, py), at(px + r, py));
        let (u, d) = (at(px, py - r), at(px, py + r));
        [0, 1, 2].map(|c| {
            let gx = rt[c] - l[c];
            let gy = d[c] - u[c];
            (gx.hypot(gy) * gain).min(1.0)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(v: f32) -> FilterBuffer {
        FilterBuffer::filled(12, 10, [v, v, v, 1.0]).unwrap()
    }

    #[test]
    fn known_outputs_on_flat_fields() {
        // A flat field has no edges: Glowing Edges is black.
        let out = glowing_edges(&flat(0.6), 2.0, 6.0, 5.0);
        assert!(out.pixels().iter().all(|p| p[..3] == [0.0, 0.0, 0.0]));
        // Distorting a flat field moves nothing visible.
        let src = flat(0.4);
        for t in 0..7 {
            let g = glass(&src, 20.0, 1.0, t as f32, 100.0, t % 2 == 0, 3);
            for p in g.pixels() {
                assert!((p[0] - 0.4).abs() < 1e-5, "texture {t}: {p:?}");
            }
        }
        for p in ocean_ripple(&src, 9.0, 20.0, 3).pixels() {
            assert!((p[0] - 0.4).abs() < 1e-5);
        }
        // With no glow, Diffuse Glow is the grain alone; with no grain and
        // full glow from the bottom, white stays white and black stays black.
        let white = diffuse_glow(&flat(1.0), 0.0, 20.0, 0.0, 1);
        assert!(white.pixels().iter().all(|p| p[0] == 1.0));
        let black = diffuse_glow(&flat(0.0), 0.0, 20.0, 20.0, 1);
        assert!(black.pixels().iter().all(|p| p[0] == 0.0));
        let none = diffuse_glow(&flat(0.3), 0.0, 0.0, 15.0, 1);
        assert!(none.pixels().iter().all(|p| (p[0] - 0.3).abs() < 1e-6));
    }

    #[test]
    fn a_hard_edge_glows_and_alpha_is_handed_through() {
        let px = (0..120)
            .map(|i| {
                if i % 12 < 6 {
                    [0.0, 0.0, 0.0, 1.0]
                } else {
                    [0.0, 0.5, 0.0, 0.5]
                }
            })
            .collect();
        let src = FilterBuffer::from_pixels(12, 10, px).unwrap();
        let out = glowing_edges(&src, 2.0, 6.0, 1.0);
        // The edge column glows green, the far columns stay dark.
        assert!(unpremultiply(out.get(6, 5))[1] > 0.5);
        assert!(unpremultiply(out.get(0, 5))[1] < 0.05);
        assert_eq!(unpremultiply(out.get(0, 5))[0], 0.0);
        for (a, b) in out.pixels().iter().zip(src.pixels()) {
            assert_eq!(a[3], b[3]);
        }
    }

    #[test]
    fn glass_has_photopeas_seven_textures_and_inverts() {
        let px = (0..24 * 20)
            .map(|i| {
                let (x, y) = ((i % 24) as f32 / 23.0, (i / 24) as f32 / 19.0);
                [x, y, 0.5, 1.0]
            })
            .collect();
        let src = FilterBuffer::from_pixels(24, 20, px).unwrap();
        let outs: Vec<FilterBuffer> = (0..7)
            .map(|t| glass(&src, 10.0, 1.0, t as f32, 100.0, false, 5))
            .collect();
        for (i, a) in outs.iter().enumerate() {
            assert_ne!(*a, src, "texture {i} moved nothing");
            for (j, b) in outs.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "textures {i} and {j} are the same");
            }
        }
        let inverted = glass(&src, 10.0, 1.0, 3.0, 100.0, true, 5);
        assert_ne!(inverted, outs[3], "Invert Texture changed nothing");
        assert_eq!(outs[3], glass(&src, 10.0, 1.0, 3.0, 100.0, false, 5));
    }
}
