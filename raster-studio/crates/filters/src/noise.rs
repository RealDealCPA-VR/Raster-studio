//! Noise: adding it, and taking it away.
//!
//! The rank filters here ([`median`], [`despeckle`], [`dust_and_scratches`])
//! are not linear in the pixel value, so they cannot run on premultiplied
//! channels — the median of `colour * alpha` is not `median(colour) * alpha`,
//! and running one on premultiplied data pulls colour out of transparent
//! pixels along every antialiased edge. They unpremultiply, rank each channel
//! independently, and premultiply back. [`add_noise`] does the same, for the
//! same reason: noise is a perturbation of *colour*, not of covered light.
//!
//! [`reduce_noise`] is a weighted average, so it stays in premultiplied space.
//!
//! Edge handling is the caller's [`EdgeMode`] for every neighbourhood filter.
//! [`add_noise`] is a point operation and has no boundary at all.

use color::{premultiply, unpremultiply};
use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::rng::Rng;
use crate::support::{accumulate, fill_bands, fill_tiles, max_abs_diff, scale, EdgeMode};

/// Shape of the random perturbation [`add_noise`] applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NoiseDistribution {
    /// Flat over `[-amount, amount]`. Reads as film-grain-free digital noise.
    #[default]
    Uniform,
    /// Normal with standard deviation `amount`. Closer to sensor noise;
    /// individual samples can exceed `amount`.
    Gaussian,
}

/// Add random noise to each pixel's colour.
///
/// Applied to **straight** (unpremultiplied) linear colour and clamped into
/// `[0, 1]` before re-premultiplying, so the result is always a valid pixel.
/// Alpha is never perturbed: noise in alpha would punch holes in the layer.
///
/// `monochromatic` adds the *same* sample to all three channels, which shifts
/// brightness without shifting hue; otherwise each channel gets its own,
/// producing colour speckle.
///
/// The value at a pixel is a pure function of `(seed, x, y)`, so the output is
/// bit-identical across runs and thread counts.
pub fn add_noise(
    src: &FilterBuffer,
    amount: f32,
    distribution: NoiseDistribution,
    monochromatic: bool,
    seed: u64,
) -> FilterBuffer {
    if src.is_empty() || !amount.is_finite() || amount == 0.0 {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let s = unpremultiply(src.get(x, y));
        let mut rng = Rng::at(seed, x as i64, y as i64);
        let draw = |rng: &mut Rng| match distribution {
            NoiseDistribution::Uniform => rng.next_signed() * amount,
            NoiseDistribution::Gaussian => rng.next_gaussian() * amount,
        };
        let mono = draw(&mut rng);
        let mut straight = s;
        for c in straight.iter_mut().take(3) {
            let n = if monochromatic { mono } else { draw(&mut rng) };
            *c = (*c + n).clamp(0.0, 1.0);
        }
        premultiply(straight)
    });
    out
}

/// Radius above which [`median`] switches to the histogram algorithm.
///
/// Below it, a direct selection over the window is cheaper than maintaining
/// histograms; above it, selection is `O(r^2 log r)` per pixel and the
/// histogram is flat in `r`.
const HISTOGRAM_RADIUS_THRESHOLD: u32 = 3;

/// Largest median radius accepted, so a runaway parameter costs bounded time.
pub const MAX_MEDIAN_RADIUS: u32 = 512;

/// Median filter over a `(2 * radius + 1)` square window.
///
/// Each channel is ranked independently on straight-alpha linear values. A
/// radius of zero is the identity.
///
/// # Cost
/// For `radius <= 3` this selects directly from the window. Above that it uses
/// the sliding two-level histogram of Perreault and Hébert: per output pixel it
/// adds one column histogram and removes another, which is a fixed 272 bin
/// updates **regardless of radius**, plus a 32-step search for the median bin.
/// A radius-2 and a radius-200 median therefore cost the same per pixel.
///
/// # Precision
/// The histogram path bins each channel. When a channel has at most 256
/// distinct values across the whole image — which is every 8-bit source, and
/// every flat or posterised region — the bins *are* those values and the
/// result is exact. Above that it falls back to 256 bins spaced evenly in
/// sRGB-encoded space (so the bins are perceptually, not linearly, uniform)
/// and the returned value is the bin's representative, within half a bin of
/// the true median. `histogram_median_matches_exact_median` pins the exact
/// case.
pub fn median(src: &FilterBuffer, radius: u32, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() || radius == 0 {
        return src.clone();
    }
    let r = radius.min(MAX_MEDIAN_RADIUS);
    let (w, h) = src.dimensions();
    let planes = split_straight_planes(src);
    let filtered: Vec<Vec<f32>> = planes
        .iter()
        .map(|p| {
            if r <= HISTOGRAM_RADIUS_THRESHOLD {
                median_plane_exact(p, w, h, r, edge)
            } else {
                median_plane_histogram(p, w, h, r, edge)
            }
        })
        .collect();
    merge_straight_planes(w, h, &filtered)
}

/// 3x3 median — the classic single-pixel-speckle remover.
pub fn despeckle(src: &FilterBuffer, edge: EdgeMode) -> FilterBuffer {
    median(src, 1, edge)
}

/// Replace only those pixels that differ from the local median by more than
/// `threshold`, leaving everything else bit-identical.
///
/// This is what separates it from a plain [`median`]: dust specks and scratch
/// lines are outliers and get replaced, while genuine texture — which is close
/// to its own median — survives untouched.
///
/// The difference is the largest per-channel gap in **straight** linear
/// values, so it is not confounded by alpha. A zero radius, or a non-positive
/// threshold, is the identity.
pub fn dust_and_scratches(
    src: &FilterBuffer,
    radius: u32,
    threshold: f32,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty() || radius == 0 || !threshold.is_finite() || threshold <= 0.0 {
        return src.clone();
    }
    let med = median(src, radius, edge);
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let s = src.get(x, y);
        let m = med.get(x, y);
        if max_abs_diff(unpremultiply(s), unpremultiply(m)) > threshold {
            m
        } else {
            s
        }
    });
    out
}

/// Bilateral (edge-preserving) noise reduction.
///
/// A Gaussian spatial kernel multiplied by a Gaussian *range* kernel: a
/// neighbour contributes in proportion to how close it is both in position and
/// in colour. Noise, being uncorrelated, averages away; edges, whose two sides
/// are far apart in colour, do not blend across.
///
/// * `strength` — spatial sigma in pixels, clamped to `0.1 ..= 16`. Larger
///   averages over more pixels.
/// * `preserve_detail` — `0 ..= 1`. At `0` the range kernel is wide and the
///   filter behaves almost like a Gaussian blur; at `1` it is narrow and only
///   near-identical pixels are merged.
///
/// The weights are normalised and the centre pixel always has weight one, so a
/// constant image is preserved exactly. Runs on premultiplied pixels, where a
/// weighted average is the correct composite. A non-positive strength is the
/// identity.
pub fn reduce_noise(
    src: &FilterBuffer,
    strength: f32,
    preserve_detail: f32,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty() || !strength.is_finite() || strength <= 0.0 {
        return src.clone();
    }
    let sigma_s = strength.clamp(0.1, 16.0);
    let detail = if preserve_detail.is_finite() {
        preserve_detail.clamp(0.0, 1.0)
    } else {
        0.5
    };
    // Range sigma from 0.30 (aggressive) down to 0.005 (barely merges).
    let sigma_r = 0.005 + (1.0 - detail) * 0.295;
    let r = (sigma_s * 2.0).ceil() as i64;
    let inv_s = -0.5 / (sigma_s * sigma_s);
    let inv_r = -0.5 / (sigma_r * sigma_r);
    let (w, h) = src.dimensions();
    let straight = crate::blur::straight_plane(src);
    let sw = w as usize;
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let center = straight[y as usize * sw + x as usize];
        let mut acc = [0.0f32; 4];
        let mut total = 0.0f32;
        for oy in -r..=r {
            for ox in -r..=r {
                let (Some(sx), Some(sy)) = (edge.map(x as i64 + ox, w), edge.map(y as i64 + oy, h))
                else {
                    continue;
                };
                let d2 = (ox * ox + oy * oy) as f32;
                let dc = max_abs_diff(straight[sy * sw + sx], center);
                let weight = (d2 * inv_s).exp() * (dc * dc * inv_r).exp();
                if weight <= 0.0 {
                    continue;
                }
                accumulate(&mut acc, src.get(sx as u32, sy as u32), weight);
                total += weight;
            }
        }
        if total > 0.0 {
            scale(acc, 1.0 / total)
        } else {
            src.get(x, y)
        }
    });
    out
}

// --- plane plumbing -------------------------------------------------------

fn split_straight_planes(src: &FilterBuffer) -> [Vec<f32>; 4] {
    let n = src.len();
    let mut planes = [
        vec![0.0f32; n],
        vec![0.0f32; n],
        vec![0.0f32; n],
        vec![0.0f32; n],
    ];
    for (i, px) in src.pixels().iter().enumerate() {
        let s = unpremultiply(*px);
        for (c, plane) in planes.iter_mut().enumerate() {
            plane[i] = s[c];
        }
    }
    planes
}

fn merge_straight_planes(width: u32, height: u32, planes: &[Vec<f32>]) -> FilterBuffer {
    let n = planes[0].len();
    let mut px = Vec::with_capacity(n);
    for (((r, g), b), a) in planes[0]
        .iter()
        .zip(planes[1].iter())
        .zip(planes[2].iter())
        .zip(planes[3].iter())
    {
        px.push(premultiply([*r, *g, *b, *a]));
    }
    FilterBuffer::from_pixels(width, height, px)
        .expect("plane length always matches the source dimensions")
}

// --- exact median ---------------------------------------------------------

fn median_plane_exact(
    src: &[f32],
    width: u32,
    height: u32,
    radius: u32,
    edge: EdgeMode,
) -> Vec<f32> {
    let mut out = vec![0.0f32; src.len()];
    let r = radius as i64;
    let w = width as usize;
    fill_bands(width, height, &mut out, |y0, band| {
        let mut window: Vec<f32> = Vec::with_capacity(((2 * r + 1) * (2 * r + 1)) as usize);
        for (ly, row) in band.chunks_mut(w).enumerate() {
            let y = y0 as i64 + ly as i64;
            for (x, slot) in row.iter_mut().enumerate() {
                window.clear();
                for oy in -r..=r {
                    let Some(sy) = edge.map(y + oy, height) else {
                        continue;
                    };
                    for ox in -r..=r {
                        let Some(sx) = edge.map(x as i64 + ox, width) else {
                            continue;
                        };
                        window.push(src[sy * w + sx]);
                    }
                }
                let k = window.len() / 2;
                let (_, m, _) = window.select_nth_unstable_by(k, |a: &f32, b: &f32| a.total_cmp(b));
                *slot = *m;
            }
        }
    });
    out
}

// --- histogram median -----------------------------------------------------

/// Fine bins in the median histogram. 256 keeps a column histogram at one
/// kilobyte and makes an 8-bit-sourced channel exactly representable.
const FINE_BINS: usize = 256;
/// Coarse bins, each summarising `FINE_BINS / COARSE_BINS` fine bins.
const COARSE_BINS: usize = 16;
const FINE_PER_COARSE: usize = FINE_BINS / COARSE_BINS;

/// Maps channel values to bin indices and back.
struct Quantizer {
    /// Representative value of each bin, ascending.
    palette: Vec<f32>,
    /// True when the palette is an even sRGB-encoded ramp rather than the
    /// image's own distinct values, i.e. the lossy fallback.
    uniform: bool,
}

impl Quantizer {
    fn build(plane: &[f32]) -> Self {
        let mut vals = plane.to_vec();
        vals.sort_by(|a: &f32, b: &f32| a.total_cmp(b));
        vals.dedup();
        if vals.len() <= FINE_BINS && !vals.is_empty() {
            Self {
                palette: vals,
                uniform: false,
            }
        } else {
            let palette = (0..FINE_BINS)
                .map(|i| color::srgb_to_linear(i as f32 / (FINE_BINS - 1) as f32))
                .collect();
            Self {
                palette,
                uniform: true,
            }
        }
    }

    #[inline]
    fn index(&self, v: f32) -> usize {
        if self.uniform {
            let e = color::linear_to_srgb(v.clamp(0.0, 1.0));
            // A NaN here casts to 0 rather than panicking; NaN cannot be
            // ranked meaningfully and must not take the process down.
            ((e * (FINE_BINS - 1) as f32 + 0.5) as usize).min(FINE_BINS - 1)
        } else {
            match self.palette.binary_search_by(|p| p.total_cmp(&v)) {
                Ok(i) => i,
                Err(i) => i.min(self.palette.len() - 1),
            }
        }
    }

    #[inline]
    fn value(&self, i: usize) -> f32 {
        self.palette[i.min(self.palette.len() - 1)]
    }
}

/// Two-level histogram: a coarse summary over `COARSE_BINS` groups lets the
/// median search skip most of the fine array.
#[derive(Clone)]
struct Hist {
    fine: [u32; FINE_BINS],
    coarse: [u32; COARSE_BINS],
    count: u32,
}

impl Hist {
    fn new() -> Self {
        Self {
            fine: [0; FINE_BINS],
            coarse: [0; COARSE_BINS],
            count: 0,
        }
    }

    #[inline]
    fn add(&mut self, i: usize) {
        self.fine[i] += 1;
        self.coarse[i / FINE_PER_COARSE] += 1;
        self.count += 1;
    }

    #[inline]
    fn remove(&mut self, i: usize) {
        self.fine[i] -= 1;
        self.coarse[i / FINE_PER_COARSE] -= 1;
        self.count -= 1;
    }

    fn add_all(&mut self, o: &Hist) {
        for (a, b) in self.fine.iter_mut().zip(o.fine.iter()) {
            *a += *b;
        }
        for (a, b) in self.coarse.iter_mut().zip(o.coarse.iter()) {
            *a += *b;
        }
        self.count += o.count;
    }

    fn remove_all(&mut self, o: &Hist) {
        for (a, b) in self.fine.iter_mut().zip(o.fine.iter()) {
            *a -= *b;
        }
        for (a, b) in self.coarse.iter_mut().zip(o.coarse.iter()) {
            *a -= *b;
        }
        self.count -= o.count;
    }

    /// Index of the `count / 2`-th sample in ascending order. The window is
    /// always odd-sized, so that is the true median.
    fn median_index(&self) -> usize {
        let target = self.count / 2;
        let mut cum = 0u32;
        for (ci, &c) in self.coarse.iter().enumerate() {
            if cum + c > target {
                let base = ci * FINE_PER_COARSE;
                for (fi, &f) in self.fine[base..base + FINE_PER_COARSE].iter().enumerate() {
                    cum += f;
                    if cum > target {
                        return base + fi;
                    }
                }
                return base;
            }
            cum += c;
        }
        FINE_BINS - 1
    }
}

fn median_plane_histogram(
    src: &[f32],
    width: u32,
    height: u32,
    radius: u32,
    edge: EdgeMode,
) -> Vec<f32> {
    let quant = Quantizer::build(src);
    let idx: Vec<u16> = src.iter().map(|v| quant.index(*v) as u16).collect();
    let w = width as usize;
    let r = radius as i64;
    let mut out = vec![0.0f32; src.len()];
    fill_bands(width, height, &mut out, |y0, band| {
        // Column histograms, each covering rows [y - r, y + r] of one column.
        let mut cols = vec![Hist::new(); w];
        for (cx, col) in cols.iter_mut().enumerate() {
            for oy in -r..=r {
                if let Some(sy) = edge.map(y0 as i64 + oy, height) {
                    col.add(idx[sy * w + cx] as usize);
                }
            }
        }
        for (ly, row) in band.chunks_mut(w).enumerate() {
            let y = y0 as i64 + ly as i64;
            if ly > 0 {
                // Slide the columns down one row: drop y-1-r, take y+r.
                for (cx, col) in cols.iter_mut().enumerate() {
                    if let Some(sy) = edge.map(y - 1 - r, height) {
                        col.remove(idx[sy * w + cx] as usize);
                    }
                    if let Some(sy) = edge.map(y + r, height) {
                        col.add(idx[sy * w + cx] as usize);
                    }
                }
            }
            // Seed the kernel histogram for x = 0, then slide it right. Each
            // step is two whole-histogram updates — a fixed cost that does not
            // grow with the radius.
            let mut kernel = Hist::new();
            for ox in -r..=r {
                if let Some(sx) = edge.map(ox, width) {
                    kernel.add_all(&cols[sx]);
                }
            }
            for (x, slot) in row.iter_mut().enumerate() {
                if x > 0 {
                    if let Some(sx) = edge.map(x as i64 + r, width) {
                        kernel.add_all(&cols[sx]);
                    }
                    if let Some(sx) = edge.map(x as i64 - 1 - r, width) {
                        kernel.remove_all(&cols[sx]);
                    }
                }
                *slot = quant.value(kernel.median_index());
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONST_PX: [f32; 4] = [0.21, 0.34, 0.55, 0.8];

    fn constant(w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, CONST_PX).unwrap()
    }

    /// An opaque image whose channels take few distinct values, so the
    /// histogram median's palette path is exact.
    fn palette_image(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let k = ((x * 37 + y * 11) % 17) as f32 / 16.0;
                px.push([k, 1.0 - k, (k * 4.0).fract(), 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    fn assert_constant(buf: &FilterBuffer, what: &str) {
        for (i, px) in buf.pixels().iter().enumerate() {
            for c in 0..4 {
                assert!(
                    (px[c] - CONST_PX[c]).abs() < 1e-5,
                    "{what}: pixel {i} channel {c}: {px:?}"
                );
            }
        }
    }

    #[test]
    fn add_noise_is_deterministic_for_a_fixed_seed() {
        let src = palette_image(23, 17);
        let a = add_noise(&src, 0.2, NoiseDistribution::Uniform, false, 99);
        let b = add_noise(&src, 0.2, NoiseDistribution::Uniform, false, 99);
        assert_eq!(a, b, "same seed must give bit-identical output");
        let c = add_noise(&src, 0.2, NoiseDistribution::Uniform, false, 100);
        assert_ne!(a, c, "a different seed must give a different image");
    }

    /// The value at a pixel must depend only on `(seed, x, y)` — not on the
    /// buffer's size, and therefore not on which parallel band it landed in.
    /// A sequential random stream would fail this.
    #[test]
    fn noise_at_a_pixel_depends_only_on_its_coordinate() {
        // 300 rows spans more than one TILE_SIZE band; 40 spans one.
        let big = palette_image(300, 300);
        let mut px = Vec::new();
        for y in 0..40u32 {
            for x in 0..40u32 {
                px.push(big.get(x, y));
            }
        }
        let small = FilterBuffer::from_pixels(40, 40, px).unwrap();

        let out_big = add_noise(&big, 0.3, NoiseDistribution::Gaussian, false, 7);
        let out_small = add_noise(&small, 0.3, NoiseDistribution::Gaussian, false, 7);
        for y in 0..40 {
            for x in 0..40 {
                assert_eq!(out_big.get(x, y), out_small.get(x, y), "at {x},{y}");
            }
        }
    }

    #[test]
    fn monochromatic_noise_shifts_all_channels_equally() {
        let src = FilterBuffer::filled(32, 32, [0.4, 0.4, 0.4, 1.0]).unwrap();
        let mono = add_noise(&src, 0.1, NoiseDistribution::Uniform, true, 3);
        for p in mono.pixels() {
            assert!((p[0] - p[1]).abs() < 1e-6, "{p:?}");
            assert!((p[1] - p[2]).abs() < 1e-6, "{p:?}");
        }
        let colour = add_noise(&src, 0.1, NoiseDistribution::Uniform, false, 3);
        let spread = colour
            .pixels()
            .iter()
            .fold(0.0f32, |m, p| m.max((p[0] - p[1]).abs()));
        assert!(spread > 0.01, "colour noise should decorrelate channels");
    }

    #[test]
    fn add_noise_never_leaves_the_valid_range_and_never_touches_alpha() {
        let src = FilterBuffer::filled(40, 40, [0.05, 0.5, 0.45, 0.5]).unwrap();
        let out = add_noise(&src, 5.0, NoiseDistribution::Gaussian, false, 12);
        for p in out.pixels() {
            assert!((p[3] - 0.5).abs() < 1e-6, "alpha changed: {p:?}");
            for c in 0..3 {
                assert!(p[c] >= -1e-6 && p[c] <= p[3] + 1e-6, "{p:?}");
            }
        }
    }

    /// The perturbation is applied to **straight** colour and re-premultiplied.
    /// Bounds alone cannot see whether that round trip happened: skipping the
    /// unpremultiply leaves the trailing premultiply in place, so every channel
    /// is multiplied by alpha twice and a 50%-alpha layer comes out at half its
    /// colour — an output that satisfies `colour <= alpha` *more* easily than
    /// the correct one. This pins the value instead of the bound.
    #[test]
    fn add_noise_perturbs_straight_colour_not_premultiplied_colour() {
        let colour = [0.6f32, 0.3, 0.2];
        let alpha = 0.5f32;
        let src = FilterBuffer::filled(
            64,
            64,
            premultiply([colour[0], colour[1], colour[2], alpha]),
        )
        .unwrap();
        let amount = 0.02f32;
        let out = add_noise(&src, amount, NoiseDistribution::Uniform, false, 7);
        let mut sums = [0.0f64; 3];
        for p in out.pixels() {
            assert!((p[3] - alpha).abs() < 1e-6, "alpha changed: {p:?}");
            let s = unpremultiply(*p);
            for c in 0..3 {
                assert!(
                    (s[c] - colour[c]).abs() <= amount + 1e-4,
                    "straight channel {c} moved by more than the noise width: \
                     {s:?} against {colour:?}"
                );
                sums[c] += s[c] as f64;
            }
        }
        for c in 0..3 {
            let mean = sums[c] / out.len() as f64;
            assert!(
                (mean - colour[c] as f64).abs() < 0.005,
                "straight channel {c} mean drifted to {mean}, expected {}",
                colour[c]
            );
        }
        // The premultiplied buffer is still premultiplied, not doubly so.
        let mean_premul: f64 =
            out.pixels().iter().map(|p| p[0] as f64).sum::<f64>() / out.len() as f64;
        assert!(
            (mean_premul - (colour[0] * alpha) as f64).abs() < 0.005,
            "premultiplied mean {mean_premul}, expected {}",
            colour[0] * alpha
        );
    }

    #[test]
    fn add_noise_mean_stays_put() {
        let src = FilterBuffer::filled(128, 128, [0.5, 0.5, 0.5, 1.0]).unwrap();
        let out = add_noise(&src, 0.1, NoiseDistribution::Uniform, true, 5);
        let mean: f64 = out.pixels().iter().map(|p| p[0] as f64).sum::<f64>() / out.len() as f64;
        assert!((mean - 0.5).abs() < 0.005, "mean drifted to {mean}");
    }

    #[test]
    fn rank_filters_preserve_a_constant_image() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let src = constant(41, 29);
            assert_constant(&median(&src, 2, edge), "median r2");
            assert_constant(&median(&src, 7, edge), "median r7 (histogram)");
            assert_constant(&despeckle(&src, edge), "despeckle");
            assert_constant(&dust_and_scratches(&src, 4, 0.1, edge), "dust");
            assert_constant(&reduce_noise(&src, 2.0, 0.5, edge), "reduce noise");
        }
    }

    /// The two median implementations must agree exactly on an image whose
    /// channels fit the histogram palette. If they diverge, one of them has an
    /// off-by-one in the window or in the rank.
    #[test]
    fn histogram_median_matches_exact_median() {
        let src = palette_image(37, 31);
        let planes = split_straight_planes(&src);
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            for radius in [4u32, 5, 9] {
                for (c, plane) in planes.iter().enumerate() {
                    let exact = median_plane_exact(plane, 37, 31, radius, edge);
                    let hist = median_plane_histogram(plane, 37, 31, radius, edge);
                    assert_eq!(exact, hist, "{edge:?} r{radius} channel {c}");
                }
            }
        }
    }

    /// Radius is meant to be free in the histogram path; at minimum, growing
    /// it must not change the answer's correctness.
    #[test]
    fn median_matches_a_brute_force_reference() {
        let src = palette_image(15, 13);
        let planes = split_straight_planes(&src);
        let edge = EdgeMode::Mirror;
        let radius = 5i64;
        let got = median_plane_histogram(&planes[0], 15, 13, radius as u32, edge);
        for y in 0..13i64 {
            for x in 0..15i64 {
                let mut window = Vec::new();
                for oy in -radius..=radius {
                    for ox in -radius..=radius {
                        let sx = edge.map(x + ox, 15).unwrap();
                        let sy = edge.map(y + oy, 13).unwrap();
                        window.push(planes[0][sy * 15 + sx]);
                    }
                }
                window.sort_by(|a: &f32, b: &f32| a.total_cmp(b));
                let expect = window[window.len() / 2];
                assert_eq!(got[(y * 15 + x) as usize], expect, "at {x},{y}");
            }
        }
    }

    /// A single bright speck in a flat field is exactly what a median is for.
    #[test]
    fn median_removes_an_isolated_speck() {
        let mut src = FilterBuffer::filled(9, 9, [0.2, 0.2, 0.2, 1.0]).unwrap();
        src.set(4, 4, [1.0, 1.0, 1.0, 1.0]);
        let out = despeckle(&src, EdgeMode::Clamp);
        assert!((out.get(4, 4)[0] - 0.2).abs() < 1e-6, "{:?}", out.get(4, 4));
    }

    /// The distinguishing behaviour: outliers go, everything else is returned
    /// bit-identical (a plain median would rewrite every pixel).
    #[test]
    fn dust_and_scratches_only_replaces_outliers() {
        let (w, h) = (21u32, 21u32);
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = 0.3 + x as f32 * 0.01;
                px.push([v, v * 0.5, v * 0.25, 1.0]);
            }
        }
        let mut src = FilterBuffer::from_pixels(w, h, px).unwrap();
        src.set(10, 10, [1.0, 1.0, 1.0, 1.0]);

        let out = dust_and_scratches(&src, 2, 0.3, EdgeMode::Clamp);
        assert_ne!(out.get(10, 10), src.get(10, 10), "the speck should be gone");

        let mut touched = 0;
        for y in 0..h {
            for x in 0..w {
                if out.get(x, y) != src.get(x, y) {
                    touched += 1;
                }
            }
        }
        assert!(
            touched <= 4,
            "a smooth ramp must survive; {touched} pixels were rewritten"
        );

        // The gate itself, measured directly: on textured material with a
        // threshold above the local spread, dust & scratches must be a
        // bit-exact identity. Without the `> threshold` test it degenerates
        // into a plain median and rewrites nearly every pixel — which is the
        // comparison immediately below.
        let textured = palette_image(21, 21);
        let gated = dust_and_scratches(&textured, 2, 0.9, EdgeMode::Clamp);
        let gated_changed = (0..textured.len())
            .filter(|&i| gated.pixels()[i] != textured.pixels()[i])
            .count();
        assert_eq!(
            gated_changed,
            0,
            "a threshold above the local spread must change nothing; {gated_changed} of {} pixels moved",
            textured.len()
        );

        // A plain median, by contrast, rewrites textured material wholesale —
        // which is exactly the damage this filter exists to avoid.
        let plain = median(&textured, 2, EdgeMode::Clamp);
        let rewritten = (0..textured.len())
            .filter(|&i| plain.pixels()[i] != textured.pixels()[i])
            .count();
        assert!(
            rewritten > textured.len() / 2,
            "expected the plain median to rewrite most of the texture, got {rewritten}"
        );
        // And the two must genuinely disagree — if they did not, the gate is
        // doing nothing at all on this image.
        assert!(
            rewritten > gated_changed,
            "gated {gated_changed} vs ungated {rewritten}: the threshold is inert"
        );

        // A threshold *below* the local spread lets the same filter act, so
        // the identity above is the gate working, not the filter being inert.
        let ungated = dust_and_scratches(&textured, 2, 0.01, EdgeMode::Clamp);
        let ungated_changed = (0..textured.len())
            .filter(|&i| ungated.pixels()[i] != textured.pixels()[i])
            .count();
        assert!(
            ungated_changed > textured.len() / 2,
            "a near-zero threshold should behave like the median, got {ungated_changed}"
        );
    }

    #[test]
    fn reduce_noise_smooths_noise_but_keeps_a_step() {
        let (w, h) = (24u32, 8u32);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let base = if x < w / 2 { 0.15f32 } else { 0.85 };
                let n = if (x + y) % 2 == 0 { 0.02 } else { -0.02 };
                let v = base + n;
                px.push([v, v, v, 1.0]);
            }
        }
        let src = FilterBuffer::from_pixels(w, h, px).unwrap();
        let out = reduce_noise(&src, 2.0, 0.4, EdgeMode::Clamp);

        // Noise on the flat left side is reduced...
        let ripple_before = (src.get(2, 4)[0] - src.get(3, 4)[0]).abs();
        let ripple_after = (out.get(2, 4)[0] - out.get(3, 4)[0]).abs();
        assert!(
            ripple_after < ripple_before * 0.5,
            "{ripple_after} vs {ripple_before}"
        );
        // ... while the 0.7-wide step across the middle survives.
        let step = out.get(w / 2, 4)[0] - out.get(w / 2 - 1, 4)[0];
        assert!(step > 0.5, "the edge was smeared: {step}");
    }

    /// The range kernel is documented to run on **straight** linear values.
    /// The case that proves it is the *same* colour step at two coverages: on
    /// premultiplied values a 0.7 step at 25% coverage measures 0.175, well
    /// inside a range sigma that the identical step at full coverage sits far
    /// outside, so the faint layer would blend across an edge the opaque one
    /// keeps. Measuring straight values makes the output *straight* colour
    /// identical at both coverages. Every other `reduce_noise` test uses alpha
    /// 1.0, where the two spaces coincide.
    #[test]
    fn reduce_noise_is_not_confounded_by_alpha() {
        let (w, h) = (16u32, 5u32);
        let opaque = colour_step(w, h, 1.0);
        let faint = colour_step(w, h, 0.25);
        // preserve_detail 0.4 gives a range sigma of 0.182: a 0.7 straight
        // step is 3.8 sigma away, a 0.175 premultiplied one is under one.
        let a = reduce_noise(&opaque, 2.0, 0.4, EdgeMode::Clamp);
        let b = reduce_noise(&faint, 2.0, 0.4, EdgeMode::Clamp);
        for y in 0..h {
            for x in 0..w {
                let sa = unpremultiply(a.get(x, y));
                let sb = unpremultiply(b.get(x, y));
                for c in 0..3 {
                    assert!(
                        (sa[c] - sb[c]).abs() < 1e-5,
                        "at {x},{y} ch{c}: 25% coverage gave {} where full coverage gave {}",
                        sb[c],
                        sa[c]
                    );
                }
                assert!((b.get(x, y)[3] - 0.25).abs() < 1e-6, "coverage moved");
            }
        }
        // The shared decision keeps the step: column 7 stays near 0.1 rather
        // than being pulled towards the 0.8 side.
        let straight_7 = unpremultiply(b.get(7, 2))[0];
        assert!(
            (straight_7 - 0.1).abs() < 0.05,
            "the edge was smeared: {straight_7}"
        );
    }

    /// A step in straight colour at a uniform coverage. The premultiplied
    /// values scale with `alpha`; the straight ones do not.
    fn colour_step(w: u32, h: u32, alpha: f32) -> FilterBuffer {
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.1f32 } else { 0.8 };
                px.push([v * alpha, v * alpha, v * alpha, alpha]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn zero_parameters_are_the_identity() {
        let src = palette_image(8, 8);
        assert_eq!(median(&src, 0, EdgeMode::Clamp), src);
        assert_eq!(dust_and_scratches(&src, 0, 0.5, EdgeMode::Clamp), src);
        assert_eq!(dust_and_scratches(&src, 3, 0.0, EdgeMode::Clamp), src);
        assert_eq!(reduce_noise(&src, 0.0, 0.5, EdgeMode::Clamp), src);
        assert_eq!(
            add_noise(&src, 0.0, NoiseDistribution::Uniform, false, 1),
            src
        );
    }

    #[test]
    fn noise_filters_survive_one_pixel_and_empty_images() {
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let one = constant(1, 1);
            assert_constant(&median(&one, 9, edge), "1x1 median (histogram)");
            assert_constant(&median(&one, 1, edge), "1x1 median (exact)");
            assert_constant(&dust_and_scratches(&one, 6, 0.2, edge), "1x1 dust");
            assert_constant(&reduce_noise(&one, 8.0, 0.2, edge), "1x1 reduce noise");
            assert!(!add_noise(&one, 0.4, NoiseDistribution::Gaussian, true, 1).is_empty());

            let empty = FilterBuffer::transparent(0, 4).unwrap();
            assert!(median(&empty, 5, edge).is_empty());
            assert!(despeckle(&empty, edge).is_empty());
            assert!(dust_and_scratches(&empty, 5, 0.2, edge).is_empty());
            assert!(reduce_noise(&empty, 3.0, 0.5, edge).is_empty());
            assert!(add_noise(&empty, 0.5, NoiseDistribution::Uniform, false, 1).is_empty());
        }
    }

    #[test]
    fn absurd_parameters_are_clamped_not_fatal() {
        let src = palette_image(6, 6);
        assert!(!median(&src, u32::MAX, EdgeMode::Wrap).is_empty());
        assert!(!reduce_noise(&src, 1e30, 0.5, EdgeMode::Clamp).is_empty());
        assert!(!reduce_noise(&src, 2.0, f32::NAN, EdgeMode::Clamp).is_empty());
        assert_eq!(reduce_noise(&src, f32::NAN, 0.5, EdgeMode::Clamp), src);
    }

    /// The fallback path (more distinct values than bins) must still return a
    /// plausible median rather than garbage.
    #[test]
    fn continuous_data_falls_back_to_uniform_bins() {
        let n = 4096;
        let plane: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
        let q = Quantizer::build(&plane);
        assert!(q.uniform, "1024+ distinct values must use the ramp");
        // Monotone in, monotone out.
        assert!(q.index(0.0) <= q.index(0.25));
        assert!(q.index(0.25) <= q.index(0.9));
        assert!(q.value(q.index(0.5)) > 0.4 && q.value(q.index(0.5)) < 0.6);
    }

    #[test]
    fn quantizer_is_exact_for_a_small_palette() {
        let plane = vec![0.0f32, 0.25, 0.5, 0.75, 1.0, 0.25, 0.5];
        let q = Quantizer::build(&plane);
        assert!(!q.uniform);
        for v in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            assert_eq!(q.value(q.index(v)), v, "{v} did not round-trip");
        }
    }
}
