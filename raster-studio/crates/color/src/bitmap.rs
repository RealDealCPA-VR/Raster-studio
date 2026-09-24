//! W10-H: Image > Mode > Bitmap — a grayscale plane reduced to pure black
//! and white, Photoshop's four methods.
//!
//! The document keeps RGBA tiles (the Photopea approach every mode here
//! takes), so "1-bit" is a semantic: after the conversion every pixel is
//! exactly black `0` or white `255`, opaque. The plane is converted whole
//! (not per tile), so a diffusion's error, a pattern's phase and a screen's
//! cells run across tile edges without a seam.
//!
//! * [`BitmapMethod::Threshold`] — 50% threshold: `g >= 128` is white.
//! * [`BitmapMethod::Pattern`] — ordered dither against an 8x8 Bayer matrix
//!   anchored at the document origin.
//! * [`BitmapMethod::Diffusion`] — Floyd-Steinberg error diffusion in
//!   serpentine order.
//! * [`BitmapMethod::Halftone`] — a halftone screen: cells of
//!   [`Halftone::cell`] pixels (the screen frequency expressed in document
//!   pixels, since a document here carries no output resolution) rotated by
//!   [`Halftone::angle`], each inked from its "most central" point outwards
//!   by the dot shape until the inked fraction of the cell matches the
//!   pixel's darkness.

/// A halftone dot's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HalftoneShape {
    #[default]
    Round,
    Square,
    Diamond,
    Line,
}

impl HalftoneShape {
    pub const ALL: [HalftoneShape; 4] = [
        HalftoneShape::Round,
        HalftoneShape::Square,
        HalftoneShape::Diamond,
        HalftoneShape::Line,
    ];

    /// The shape's name, as Photoshop's Halftone Screen dialog lists it.
    pub const fn label(self) -> &'static str {
        match self {
            HalftoneShape::Round => "Round",
            HalftoneShape::Square => "Square",
            HalftoneShape::Diamond => "Diamond",
            HalftoneShape::Line => "Line",
        }
    }
}

/// A halftone screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Halftone {
    /// Cell size in document pixels (`MIN_CELL..=MAX_CELL`).
    pub cell: f32,
    /// Screen angle in degrees.
    pub angle: f32,
    pub shape: HalftoneShape,
}

/// The smallest and largest halftone cell, in pixels.
pub const MIN_CELL: f32 = 2.0;
pub const MAX_CELL: f32 = 200.0;

impl Default for Halftone {
    /// Photoshop's defaults are 53 lpi at 45 degrees, round; with no output
    /// resolution, an 8-pixel cell at 45 degrees.
    fn default() -> Self {
        Self {
            cell: 8.0,
            angle: 45.0,
            shape: HalftoneShape::Round,
        }
    }
}

/// How the grayscale plane becomes black and white.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BitmapMethod {
    /// 50% threshold.
    Threshold,
    /// Ordered (Bayer 8x8) dither.
    Pattern,
    /// Floyd-Steinberg error diffusion — Photoshop's default.
    #[default]
    Diffusion,
    /// A halftone screen.
    Halftone(Halftone),
}

impl BitmapMethod {
    /// Every method with its default parameters, in dialog order.
    pub const ALL: [BitmapMethod; 4] = [
        BitmapMethod::Threshold,
        BitmapMethod::Pattern,
        BitmapMethod::Diffusion,
        BitmapMethod::Halftone(Halftone {
            cell: 8.0,
            angle: 45.0,
            shape: HalftoneShape::Round,
        }),
    ];

    /// The method's name, as Photoshop's Bitmap dialog lists it.
    pub const fn label(self) -> &'static str {
        match self {
            BitmapMethod::Threshold => "50% Threshold",
            BitmapMethod::Pattern => "Pattern Dither",
            BitmapMethod::Diffusion => "Diffusion Dither",
            BitmapMethod::Halftone(_) => "Halftone Screen",
        }
    }

    /// Whether two methods are the same choice, whatever their parameters.
    pub fn same_kind(self, other: BitmapMethod) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }

    /// Whether the parameters are usable.
    pub fn is_valid(self) -> bool {
        match self {
            BitmapMethod::Halftone(h) => {
                h.cell.is_finite()
                    && (MIN_CELL..=MAX_CELL).contains(&h.cell)
                    && h.angle.is_finite()
            }
            _ => true,
        }
    }
}

/// The 8x8 Bayer matrix, values `0..64`.
const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Convert a `width x height` grayscale plane (`0` black, `255` white) to
/// black and white by `method`. Every output value is `0` or `255`. A plane
/// whose length is not `width * height` comes back unchanged.
pub fn to_bitmap(gray: &[u8], width: usize, height: usize, method: BitmapMethod) -> Vec<u8> {
    if gray.len() != width * height {
        return gray.to_vec();
    }
    let bw = |white: bool| if white { 255u8 } else { 0u8 };
    match method {
        BitmapMethod::Threshold => gray.iter().map(|&g| bw(g >= 128)).collect(),
        BitmapMethod::Pattern => gray
            .iter()
            .enumerate()
            .map(|(i, &g)| {
                let (x, y) = (i % width, i / width);
                // Thresholds at (k + 0.5) / 64 of the ramp.
                let t = (f32::from(BAYER8[y % 8][x % 8]) + 0.5) * 255.0 / 64.0;
                bw(f32::from(g) > t)
            })
            .collect(),
        BitmapMethod::Diffusion => diffuse(gray, width, height),
        BitmapMethod::Halftone(screen) => {
            let screen = if BitmapMethod::Halftone(screen).is_valid() {
                screen
            } else {
                Halftone::default()
            };
            let (sin, cos) = screen.angle.to_radians().sin_cos();
            gray.iter()
                .enumerate()
                .map(|(i, &g)| {
                    let (x, y) = ((i % width) as f32 + 0.5, (i / width) as f32 + 0.5);
                    // Into the screen's rotated frame, in cells.
                    let u = (x * cos + y * sin) / screen.cell;
                    let v = (-x * sin + y * cos) / screen.cell;
                    let (fu, fv) = (u - u.floor() - 0.5, v - v.floor() - 0.5);
                    let darkness = 1.0 - f32::from(g) / 255.0;
                    bw(spot_rank(screen.shape, fu, fv) >= darkness)
                })
                .collect()
        }
    }
}

/// The fraction of a cell inked before the point at `(fu, fv)` (each in
/// `-0.5..0.5`, the cell centre at the origin) is: a point is inked when
/// this is below the pixel's darkness, so a flat tone inks about that
/// fraction of every cell.
fn spot_rank(shape: HalftoneShape, fu: f32, fv: f32) -> f32 {
    match shape {
        // Area of the disc through the point, then its complement's mirror
        // past the inscribed circle (the dots join at 50%-ish and the white
        // shrinks to the corners).
        HalftoneShape::Round => {
            let r2 = fu * fu + fv * fv;
            (std::f32::consts::PI * r2).min(1.0)
        }
        HalftoneShape::Square => {
            let r = fu.abs().max(fv.abs());
            (4.0 * r * r).min(1.0)
        }
        HalftoneShape::Diamond => {
            let r = fu.abs() + fv.abs();
            if r <= 0.5 {
                2.0 * r * r
            } else {
                1.0 - 2.0 * (1.0 - r) * (1.0 - r)
            }
        }
        HalftoneShape::Line => (2.0 * fv.abs()).min(1.0),
    }
}

/// Floyd-Steinberg in serpentine order.
fn diffuse(gray: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut work: Vec<f32> = gray.iter().map(|&g| f32::from(g)).collect();
    let mut out = vec![0u8; gray.len()];
    for y in 0..height {
        let forward = y % 2 == 0;
        for step in 0..width {
            let x = if forward { step } else { width - 1 - step };
            let i = y * width + x;
            let old = work[i];
            let new = if old >= 127.5 { 255.0 } else { 0.0 };
            out[i] = new as u8;
            let err = old - new;
            let mut push = |dx: isize, dy: usize, w: f32| {
                let dx = if forward { dx } else { -dx };
                let nx = x as isize + dx;
                let ny = y + dy;
                if nx >= 0 && (nx as usize) < width && ny < height {
                    work[ny * width + nx as usize] += err * w;
                }
            };
            push(1, 0, 7.0 / 16.0);
            push(-1, 1, 3.0 / 16.0);
            push(0, 1, 5.0 / 16.0);
            push(1, 1, 1.0 / 16.0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Vec<u8> {
        (0..w * h)
            .map(|i| ((i % w) * 255 / (w - 1)) as u8)
            .collect()
    }

    fn mean(v: &[u8]) -> f32 {
        v.iter().map(|&g| f32::from(g)).sum::<f32>() / v.len() as f32
    }

    #[test]
    fn every_method_leaves_only_black_and_white() {
        let g = ramp(64, 64);
        for method in BitmapMethod::ALL {
            let out = to_bitmap(&g, 64, 64, method);
            assert_eq!(out.len(), g.len());
            assert!(
                out.iter().all(|&v| v == 0 || v == 255),
                "{method:?} left a grey"
            );
        }
    }

    #[test]
    fn threshold_splits_at_half() {
        let out = to_bitmap(&[0, 127, 128, 255], 4, 1, BitmapMethod::Threshold);
        assert_eq!(out, vec![0, 0, 255, 255]);
    }

    #[test]
    fn the_dithers_and_the_screen_keep_the_tone_of_a_flat_grey() {
        for &level in &[64u8, 128, 192] {
            let g = vec![level; 96 * 96];
            for method in [
                BitmapMethod::Pattern,
                BitmapMethod::Diffusion,
                BitmapMethod::Halftone(Halftone::default()),
                BitmapMethod::Halftone(Halftone {
                    shape: HalftoneShape::Square,
                    angle: 0.0,
                    cell: 6.0,
                }),
            ] {
                let m = mean(&to_bitmap(&g, 96, 96, method));
                assert!(
                    (m - f32::from(level)).abs() < 255.0 * 0.06,
                    "{method:?} at {level}: mean {m}"
                );
            }
            // Threshold does not: a flat 64 is all black.
            assert_eq!(mean(&to_bitmap(&g, 96, 96, BitmapMethod::Threshold)), {
                if level >= 128 {
                    255.0
                } else {
                    0.0
                }
            });
        }
    }

    #[test]
    fn the_screen_angle_and_shape_change_the_dots() {
        let g = vec![100u8; 64 * 64];
        let a = to_bitmap(&g, 64, 64, BitmapMethod::Halftone(Halftone::default()));
        let b = to_bitmap(
            &g,
            64,
            64,
            BitmapMethod::Halftone(Halftone {
                angle: 0.0,
                ..Halftone::default()
            }),
        );
        let c = to_bitmap(
            &g,
            64,
            64,
            BitmapMethod::Halftone(Halftone {
                shape: HalftoneShape::Line,
                ..Halftone::default()
            }),
        );
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(!BitmapMethod::Halftone(Halftone {
            cell: 1.0,
            ..Halftone::default()
        })
        .is_valid());
    }

    #[test]
    fn a_wrong_length_plane_is_returned_unchanged() {
        assert_eq!(
            to_bitmap(&[7, 8, 9], 2, 2, BitmapMethod::Threshold),
            vec![7, 8, 9]
        );
    }
}
