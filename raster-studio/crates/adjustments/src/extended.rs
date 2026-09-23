//! The five adjustments Photopea offers beyond the classic set: Desaturate,
//! Equalize, Shadows/Highlights, Replace Color and Color Lookup.
//!
//! Each is a pure function of a colour and validated parameters like the rest
//! of the crate, with two exceptions that are stated rather than hidden:
//!
//! * [`EqualizeMap`] is an *analysis*, like the auto commands: it is built from
//!   an [`ImageStats`] and is the identity until one has been seen.
//! * [`ShadowsHighlights`] is a *neighbourhood* operation in Photoshop — the
//!   shadow/highlight masks are read from a blurred luminance, `radius` pixels
//!   wide. [`ShadowsHighlights::apply_premultiplied_rgba_spatial`] is that
//!   whole-buffer form, and it is what Image ▸ Adjustments runs. The per-pixel
//!   [`ShadowsHighlights::apply`] reads each pixel's own luminance instead
//!   (radius zero), which is the form a per-pixel pipeline can evaluate.
//!
//! What clamps, and why (the crate-level list, extended):
//!
//! * [`EqualizeMap`] reads a bounded histogram, so it is defined on `0..=1`
//!   and maps anything outside onto the end of its table.
//! * [`Lut3d`] is sampled over its domain, `0..=1`; input outside it is held
//!   at the edge of the cube, which is what every `.cube` reader does.
//! * [`ReplaceColor`] shifts through [`HueSaturation`], which is
//!   display-referred for the reason given there.
//! * [`desaturate`] and [`ShadowsHighlights`] do not clamp the pixel.

use color::ColorSpace;

use crate::auto::{ImageStats, HISTOGRAM_BINS};
use crate::color_ops::HueSaturation;
use crate::error::{in_range, AdjustmentError};
use crate::space::{clamp01, EncodedRgb, LinearRgb};

// ---------------------------------------------------------------------------
// Desaturate
// ---------------------------------------------------------------------------

/// Desaturate, on **linear** light: every channel becomes the pixel's own
/// Rec. 709 relative luminance.
///
/// Doing it in linear light is what makes it *luminosity-preserving*: the
/// output's luminance is `Y · (0.2126 + 0.7152 + 0.0722) = Y`, exactly the
/// input's. The same weights on encoded values would darken saturated colours.
pub fn desaturate(px: LinearRgb) -> LinearRgb {
    let y = px.luminance();
    LinearRgb([y, y, y])
}

// ---------------------------------------------------------------------------
// Equalize
// ---------------------------------------------------------------------------

/// Histogram equalisation resolved against one image: a 256-entry remap of
/// encoded values built from the cumulative histogram of all three channels,
/// applied identically to each channel so the colour balance holds.
///
/// Built by [`EqualizeMap::from_stats`]; `None` there means the image gives
/// equalisation nothing to do (no pixels, one flat value, or a histogram that
/// is already flat).
#[derive(Debug, Clone, PartialEq)]
pub struct EqualizeMap {
    table: Box<[f32; HISTOGRAM_BINS]>,
}

impl EqualizeMap {
    /// The remap for the image `stats` measured, or `None` when it would change
    /// nothing.
    pub fn from_stats(stats: &ImageStats) -> Option<Self> {
        let mut pooled = [0u64; HISTOGRAM_BINS];
        for channel in &stats.channels {
            for (acc, count) in pooled.iter_mut().zip(channel.bins()) {
                *acc += u64::from(*count);
            }
        }
        let total: u64 = pooled.iter().sum();
        if total == 0 {
            return None;
        }
        let mut cdf = [0u64; HISTOGRAM_BINS];
        let mut running = 0u64;
        for (c, count) in cdf.iter_mut().zip(pooled) {
            running += count;
            *c = running;
        }
        let first = cdf.iter().copied().find(|c| *c > 0).unwrap_or(0);
        if total == first {
            return None;
        }
        let span = (total - first) as f64;
        let mut table = Box::new([0.0f32; HISTOGRAM_BINS]);
        for (t, c) in table.iter_mut().zip(cdf) {
            *t = (c.saturating_sub(first) as f64 / span) as f32;
        }
        let last = (HISTOGRAM_BINS - 1) as f32;
        let already_flat = table
            .iter()
            .enumerate()
            .all(|(i, t)| (t - i as f32 / last).abs() < 0.5 / last);
        (!already_flat).then_some(Self { table })
    }

    /// The remapped value of one encoded channel, interpolated between bins.
    pub fn eval(&self, v: f32) -> f32 {
        let last = HISTOGRAM_BINS - 1;
        let x = clamp01(v) * last as f32;
        if x.is_nan() {
            return v;
        }
        let i0 = (x.floor() as usize).min(last);
        let i1 = (i0 + 1).min(last);
        let t = x - i0 as f32;
        self.table[i0] + (self.table[i1] - self.table[i0]) * t
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        enc.map(|v| self.eval(v))
    }
}

// ---------------------------------------------------------------------------
// Shadows / Highlights
// ---------------------------------------------------------------------------

/// Largest radius, in pixels, Shadows/Highlights accepts — Photoshop's.
pub const MAX_SHADOWS_HIGHLIGHTS_RADIUS: f32 = 2500.0;

/// How much of the headroom the strongest setting may use: at amount 100%,
/// tonal width 100% and pure black, the lift is three quarters of the way to
/// white. Chosen so the control's full travel is strong but not a clip.
const SH_REACH: f32 = 0.75;

/// Shadows/Highlights on **gamma-encoded** values.
///
/// Each band is `[amount, tone, radius]`: `amount` and `tone` (the tonal
/// width) in `0.0..=1.0`, `radius` in pixels in
/// `0.0..=`[`MAX_SHADOWS_HIGHLIGHTS_RADIUS`]. A pixel's shadow weight is
/// `max(0, 1 - L / tone)²` and its highlight weight
/// `max(0, (L - (1 - tone)) / tone)²`, where `L` is the (local) encoded luma,
/// so a pixel brighter than the tonal width is not lifted at all and one darker
/// than it is not pulled down. The shift is added equally to the three
/// channels, which keeps the pixel's hue and leaves it unclamped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowsHighlights {
    shadows: [f32; 3],
    highlights: [f32; 3],
}

impl ShadowsHighlights {
    /// Photoshop's opening setting: shadows at 35%, tonal width 50%, radius
    /// 30 px; highlights off.
    pub const DEFAULT: Self = Self {
        shadows: [0.35, 0.5, 30.0],
        highlights: [0.0, 0.5, 30.0],
    };

    /// No change at all.
    pub const IDENTITY: Self = Self {
        shadows: [0.0, 0.5, 30.0],
        highlights: [0.0, 0.5, 30.0],
    };

    /// Validate both bands.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::OutOfRange`] or [`AdjustmentError::NotFinite`] for
    /// the first value outside its range.
    pub fn new(shadows: [f32; 3], highlights: [f32; 3]) -> Result<Self, AdjustmentError> {
        for (names, band) in [
            (
                ["shadows amount", "shadows tone", "shadows radius"],
                shadows,
            ),
            (
                ["highlights amount", "highlights tone", "highlights radius"],
                highlights,
            ),
        ] {
            in_range(names[0], band[0], 0.0, 1.0)?;
            in_range(names[1], band[1], 0.0, 1.0)?;
            in_range(names[2], band[2], 0.0, MAX_SHADOWS_HIGHLIGHTS_RADIUS)?;
        }
        Ok(Self {
            shadows,
            highlights,
        })
    }

    /// `[amount, tone, radius]` for the shadows.
    pub fn shadows(&self) -> [f32; 3] {
        self.shadows
    }

    /// `[amount, tone, radius]` for the highlights.
    pub fn highlights(&self) -> [f32; 3] {
        self.highlights
    }

    /// Whether neither band can move a pixel.
    pub fn is_identity(&self) -> bool {
        let off = |b: [f32; 3]| b[0] == 0.0 || b[1] == 0.0;
        off(self.shadows) && off(self.highlights)
    }

    /// The shift for a pixel whose shadow mask reads `local_s` and whose
    /// highlight mask reads `local_h` (both encoded lumas).
    fn delta(&self, local_s: f32, local_h: f32) -> f32 {
        let mut d = 0.0;
        let [amount, tone, _] = self.shadows;
        if amount > 0.0 && tone > 0.0 {
            let l = clamp01(local_s);
            let w = (1.0 - l / tone).max(0.0);
            d += amount * w * w * (1.0 - l) * SH_REACH;
        }
        let [amount, tone, _] = self.highlights;
        if amount > 0.0 && tone > 0.0 {
            let l = clamp01(local_h);
            let w = ((l - (1.0 - tone)) / tone).max(0.0);
            d -= amount * w * w * l * SH_REACH;
        }
        if d.is_finite() {
            d
        } else {
            0.0
        }
    }

    /// Apply to one encoded triple, reading the pixel's own luma as both masks
    /// (the radius-zero form).
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        let l = enc.luma();
        let d = self.delta(l, l);
        enc.map(|v| v + d)
    }

    /// Apply over a whole `width × height` buffer of linear **premultiplied**
    /// RGBA, reading each band's mask from the luma blurred over that band's
    /// radius — the Photoshop form.
    ///
    /// The blur is alpha-weighted, so transparent pixels neither darken nor
    /// brighten the neighbourhood of an opaque edge, and fully transparent
    /// pixels are left alone.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::BufferShape`] when `pixels` is not `width × height`.
    pub fn apply_premultiplied_rgba_spatial(
        &self,
        pixels: &mut [[f32; 4]],
        width: usize,
        height: usize,
        space: &ColorSpace,
    ) -> Result<(), AdjustmentError> {
        if pixels.len() != width * height {
            return Err(AdjustmentError::BufferShape {
                pixels: pixels.len(),
                width,
                height,
            });
        }
        if self.is_identity() {
            return Ok(());
        }
        let mut weighted = Vec::with_capacity(pixels.len());
        let mut alpha = Vec::with_capacity(pixels.len());
        for px in pixels.iter() {
            let a = px[3];
            if a <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                weighted.push(0.0);
                alpha.push(0.0);
                continue;
            }
            let s = color::unpremultiply(*px);
            let luma = EncodedRgb(color::from_linear(space, [s[0], s[1], s[2]])).luma();
            weighted.push(clamp01(luma) * a);
            alpha.push(a);
        }
        let local = |radius: f32| -> Vec<f32> {
            let mut num = weighted.clone();
            let mut den = alpha.clone();
            blur_plane(&mut num, width, height, radius);
            blur_plane(&mut den, width, height, radius);
            num.iter()
                .zip(&den)
                .map(|(n, d)| if *d > 1e-6 { n / d } else { 0.0 })
                .collect()
        };
        let shadows = local(self.shadows[2]);
        let highlights = if self.highlights[2] == self.shadows[2] {
            shadows.clone()
        } else {
            local(self.highlights[2])
        };
        for (i, px) in pixels.iter_mut().enumerate() {
            if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                continue;
            }
            let d = self.delta(shadows[i], highlights[i]);
            if d == 0.0 {
                continue;
            }
            let s = color::unpremultiply(*px);
            let enc = EncodedRgb(color::from_linear(space, [s[0], s[1], s[2]])).map(|v| v + d);
            let out = enc.decode(space).get();
            *px = color::premultiply([out[0], out[1], out[2], px[3]]);
        }
        Ok(())
    }
}

/// An approximately Gaussian blur of one plane: three box passes whose summed
/// support is about `radius` pixels either side. Edges are normalised by the
/// samples actually present, so the border is not darkened.
fn blur_plane(plane: &mut [f32], width: usize, height: usize, radius: f32) {
    let r = (radius / 3.0).round() as usize;
    if r == 0 || width == 0 || height == 0 {
        return;
    }
    let mut scratch = vec![0.0f32; width.max(height)];
    for _ in 0..3 {
        for y in 0..height {
            let row = &mut plane[y * width..(y + 1) * width];
            box_line(row, &mut scratch[..width], r);
        }
        let mut column = vec![0.0f32; height];
        for x in 0..width {
            for (y, c) in column.iter_mut().enumerate() {
                *c = plane[y * width + x];
            }
            box_line(&mut column, &mut scratch[..height], r);
            for (y, c) in column.iter().enumerate() {
                plane[y * width + x] = *c;
            }
        }
    }
}

/// One box pass over a line, in place, with a running sum.
fn box_line(line: &mut [f32], scratch: &mut [f32], r: usize) {
    let n = line.len();
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0.0f64);
    for v in line.iter() {
        let last = *prefix.last().unwrap_or(&0.0);
        prefix.push(last + f64::from(*v));
    }
    for (i, out) in scratch.iter_mut().enumerate().take(n) {
        let lo = i.saturating_sub(r);
        let hi = (i + r + 1).min(n);
        *out = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
    }
    line.copy_from_slice(&scratch[..n]);
}

// ---------------------------------------------------------------------------
// Replace Color
// ---------------------------------------------------------------------------

/// Replace Color on **gamma-encoded** values: a soft selection of every pixel
/// near one sampled colour, and a hue/saturation/lightness shift applied in
/// proportion to it.
///
/// Coverage is `1 - d / fuzziness` (floored at zero), where `d` is the
/// Euclidean distance between the pixel and the sampled colour in encoded RGB —
/// the falloff Select ▸ Color Range uses, so the dialog's selection preview
/// means the same thing. A pixel outside the fuzziness is returned untouched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplaceColor {
    color: [f32; 3],
    fuzziness: f32,
    shift: HueSaturation,
}

impl ReplaceColor {
    /// The largest fuzziness: Photoshop's 200 on a 0–255 scale.
    pub const MAX_FUZZINESS: f32 = 200.0 / 255.0;

    /// Photoshop's default fuzziness, 40.
    pub const DEFAULT_FUZZINESS: f32 = 40.0 / 255.0;

    /// Nothing selected, nothing shifted: the stored fallback for a Replace
    /// Color that will not build.
    pub const IDENTITY: Self = Self {
        color: [0.0; 3],
        fuzziness: 0.0,
        shift: HueSaturation::IDENTITY,
    };

    /// Validate a sampled `color` (encoded, `0..=1` per channel), a
    /// `fuzziness` in `0..=`[`Self::MAX_FUZZINESS`], and the shift, as
    /// [`HueSaturation::new`] takes it.
    ///
    /// # Errors
    ///
    /// The first parameter out of range.
    pub fn new(
        color: [f32; 3],
        fuzziness: f32,
        hue_degrees: f32,
        saturation: f32,
        lightness: f32,
    ) -> Result<Self, AdjustmentError> {
        for c in color {
            in_range("replace color", c, 0.0, 1.0)?;
        }
        in_range("fuzziness", fuzziness, 0.0, Self::MAX_FUZZINESS)?;
        Ok(Self {
            color,
            fuzziness,
            shift: HueSaturation::new(hue_degrees, saturation, lightness)?,
        })
    }

    /// The sampled colour, encoded.
    pub fn color(&self) -> [f32; 3] {
        self.color
    }

    /// The fuzziness.
    pub fn fuzziness(&self) -> f32 {
        self.fuzziness
    }

    /// The shift applied to the selected pixels.
    pub fn shift(&self) -> HueSaturation {
        self.shift
    }

    /// Whether the shift is the identity, so no pixel can change.
    pub fn is_identity(&self) -> bool {
        self.shift.is_identity()
    }

    /// How strongly `enc` is selected, in `0..=1`.
    pub fn coverage(&self, enc: EncodedRgb) -> f32 {
        let v = enc.get();
        let d = (0..3)
            .map(|i| {
                let e = clamp01(v[i]) - self.color[i];
                e * e
            })
            .sum::<f32>()
            .sqrt();
        if !d.is_finite() {
            return 0.0;
        }
        if self.fuzziness <= 0.0 {
            return if d <= 1e-6 { 1.0 } else { 0.0 };
        }
        (1.0 - d / self.fuzziness).clamp(0.0, 1.0)
    }

    /// Apply to one encoded triple.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        let w = self.coverage(enc);
        if w == 0.0 {
            return enc;
        }
        let shifted = self.shift.apply(enc).get();
        let v = enc.get();
        EncodedRgb([
            v[0] + (shifted[0] - v[0]) * w,
            v[1] + (shifted[1] - v[1]) * w,
            v[2] + (shifted[2] - v[2]) * w,
        ])
    }
}

// ---------------------------------------------------------------------------
// Color Lookup
// ---------------------------------------------------------------------------

/// Smallest and largest 3D LUT edge accepted.
pub const MIN_LUT_SIZE: usize = 2;
/// Largest 3D LUT edge accepted: 65 is the biggest `.cube` in common use, and
/// the cap keeps a stored document from carrying an unbounded table.
pub const MAX_LUT_SIZE: usize = 65;

/// A 3D colour lookup table on **gamma-encoded** values, sampled trilinearly.
///
/// The table is `size³` output colours in `.cube` order — red varies fastest,
/// then green, then blue.
#[derive(Debug, Clone, PartialEq)]
pub struct Lut3d {
    name: String,
    size: usize,
    table: Vec<[f32; 3]>,
}

impl Lut3d {
    /// Validate a table.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::InvalidLut`] for an edge outside
    /// [`MIN_LUT_SIZE`]`..=`[`MAX_LUT_SIZE`], a table that is not `size³`
    /// long, or a non-finite entry.
    pub fn new(
        name: impl Into<String>,
        size: usize,
        table: Vec<[f32; 3]>,
    ) -> Result<Self, AdjustmentError> {
        if !(MIN_LUT_SIZE..=MAX_LUT_SIZE).contains(&size) {
            return Err(AdjustmentError::InvalidLut {
                reason: format!(
                    "the cube edge must be {MIN_LUT_SIZE}..={MAX_LUT_SIZE}, got {size}"
                ),
            });
        }
        if table.len() != size * size * size {
            return Err(AdjustmentError::InvalidLut {
                reason: format!(
                    "a {size}-point cube needs {} entries, got {}",
                    size * size * size,
                    table.len()
                ),
            });
        }
        if table.iter().flatten().any(|v| !v.is_finite()) {
            return Err(AdjustmentError::InvalidLut {
                reason: "an entry is not a finite number".to_string(),
            });
        }
        Ok(Self {
            name: name.into(),
            size,
            table,
        })
    }

    /// The identity cube of edge `size` (clamped into the accepted range).
    pub fn identity(size: usize) -> Self {
        Self::from_fn("", size, |rgb| rgb)
    }

    /// A cube of edge `size` sampled from `f` at each grid point.
    pub fn from_fn(name: &str, size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> Self {
        let size = size.clamp(MIN_LUT_SIZE, MAX_LUT_SIZE);
        let step = (size - 1) as f32;
        let mut table = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    table.push(f([r as f32 / step, g as f32 / step, b as f32 / step]));
                }
            }
        }
        Self {
            name: name.to_string(),
            size,
            table,
        }
    }

    /// Parse the text of an Adobe/Resolve `.cube` file.
    ///
    /// `TITLE` names the table when present (otherwise `fallback_name` does);
    /// comments and blank lines are skipped. A 1D LUT, or a domain other than
    /// `0..1`, is refused rather than silently misread.
    ///
    /// # Errors
    ///
    /// [`AdjustmentError::InvalidLut`] naming the first problem.
    pub fn parse_cube(fallback_name: &str, text: &str) -> Result<Self, AdjustmentError> {
        let bad = |reason: String| AdjustmentError::InvalidLut { reason };
        let mut name = fallback_name.to_string();
        let mut size: Option<usize> = None;
        let mut table = Vec::new();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut words = line.split_whitespace();
            let first = words.next().unwrap_or("");
            match first {
                "TITLE" => {
                    let title = line["TITLE".len()..].trim().trim_matches('"').trim();
                    if !title.is_empty() {
                        name = title.to_string();
                    }
                }
                "LUT_3D_SIZE" => {
                    let n = words
                        .next()
                        .and_then(|w| w.parse::<usize>().ok())
                        .ok_or_else(|| {
                            bad(format!("line {}: LUT_3D_SIZE needs a number", index + 1))
                        })?;
                    size = Some(n);
                }
                "LUT_1D_SIZE" => {
                    return Err(bad(
                        "this is a 1D LUT; Color Lookup takes a 3D .cube".to_string()
                    ));
                }
                "DOMAIN_MIN" | "DOMAIN_MAX" | "LUT_3D_INPUT_RANGE" => {
                    let expected: &[f32] = match first {
                        "DOMAIN_MIN" => &[0.0, 0.0, 0.0],
                        "DOMAIN_MAX" => &[1.0, 1.0, 1.0],
                        _ => &[0.0, 1.0],
                    };
                    let values: Vec<f32> = words.filter_map(|w| w.parse().ok()).collect();
                    if values.len() != expected.len()
                        || values
                            .iter()
                            .zip(expected)
                            .any(|(v, e)| (v - e).abs() > 1e-6)
                    {
                        return Err(bad(format!(
                            "line {}: only the 0..1 input domain is supported",
                            index + 1
                        )));
                    }
                }
                _ if first.starts_with(|c: char| c.is_ascii_alphabetic()) => {
                    // An unknown keyword (LUT_IN_VIDEO_RANGE and friends):
                    // nothing that changes how the table is read.
                }
                _ => {
                    let values: Vec<f32> = line
                        .split_whitespace()
                        .map(|w| w.parse::<f32>())
                        .collect::<Result<_, _>>()
                        .map_err(|_| bad(format!("line {}: expected three numbers", index + 1)))?;
                    if values.len() != 3 {
                        return Err(bad(format!(
                            "line {}: expected three numbers, got {}",
                            index + 1,
                            values.len()
                        )));
                    }
                    table.push([values[0], values[1], values[2]]);
                }
            }
        }
        let size = size.ok_or_else(|| bad("the file has no LUT_3D_SIZE".to_string()))?;
        Self::new(name, size, table)
    }

    /// The name shown for this table.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The cube edge.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The table, in `.cube` order.
    pub fn table(&self) -> &[[f32; 3]] {
        &self.table
    }

    /// Whether every entry sits on its own grid point, so sampling is the
    /// identity.
    pub fn is_identity(&self) -> bool {
        let step = (self.size - 1) as f32;
        let n = self.size;
        self.table.iter().enumerate().all(|(i, out)| {
            let grid = [
                (i % n) as f32 / step,
                ((i / n) % n) as f32 / step,
                (i / (n * n)) as f32 / step,
            ];
            (0..3).all(|c| (out[c] - grid[c]).abs() <= 1e-6)
        })
    }

    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.table[(b * self.size + g) * self.size + r]
    }

    /// Sample the cube at an encoded colour, trilinearly.
    pub fn apply(&self, enc: EncodedRgb) -> EncodedRgb {
        let v = enc.get();
        if v.iter().any(|c| c.is_nan()) {
            return enc;
        }
        let last = self.size - 1;
        let pos = v.map(|c| clamp01(c) * last as f32);
        let lo = pos.map(|p| (p.floor() as usize).min(last));
        let hi = lo.map(|l| (l + 1).min(last));
        let t = [
            pos[0] - lo[0] as f32,
            pos[1] - lo[1] as f32,
            pos[2] - lo[2] as f32,
        ];
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| {
            [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ]
        };
        let c00 = lerp(
            self.at(lo[0], lo[1], lo[2]),
            self.at(hi[0], lo[1], lo[2]),
            t[0],
        );
        let c10 = lerp(
            self.at(lo[0], hi[1], lo[2]),
            self.at(hi[0], hi[1], lo[2]),
            t[0],
        );
        let c01 = lerp(
            self.at(lo[0], lo[1], hi[2]),
            self.at(hi[0], lo[1], hi[2]),
            t[0],
        );
        let c11 = lerp(
            self.at(lo[0], hi[1], hi[2]),
            self.at(hi[0], hi[1], hi[2]),
            t[0],
        );
        let c0 = lerp(c00, c10, t[1]);
        let c1 = lerp(c01, c11, t[1]);
        EncodedRgb(lerp(c0, c1, t[2]))
    }
}

/// The looks Color Lookup offers without a file. Each is generated, so they
/// cost nothing to ship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinLut {
    /// Every channel inverted.
    Invert,
    /// A warm look: reds up, blues down.
    Warm,
    /// A cool look: blues up, reds down.
    Cool,
    /// Sepia toning over the luma.
    Sepia,
    /// A strong S-curve on every channel.
    HighContrast,
}

impl BuiltinLut {
    /// Every built-in, in the order the dialog lists them.
    pub const ALL: [BuiltinLut; 5] = [
        BuiltinLut::Invert,
        BuiltinLut::Warm,
        BuiltinLut::Cool,
        BuiltinLut::Sepia,
        BuiltinLut::HighContrast,
    ];

    /// The name the table carries.
    pub const fn name(self) -> &'static str {
        match self {
            BuiltinLut::Invert => "Invert",
            BuiltinLut::Warm => "Warm",
            BuiltinLut::Cool => "Cool",
            BuiltinLut::Sepia => "Sepia",
            BuiltinLut::HighContrast => "High Contrast",
        }
    }

    /// The table, at a 17-point edge.
    pub fn lut(self) -> Lut3d {
        const EDGE: usize = 17;
        let name = self.name();
        match self {
            BuiltinLut::Invert => Lut3d::from_fn(name, EDGE, |c| c.map(|v| 1.0 - v)),
            BuiltinLut::Warm => Lut3d::from_fn(name, EDGE, |[r, g, b]| {
                [clamp01(r * 1.08 + 0.02), g, clamp01(b * 0.88)]
            }),
            BuiltinLut::Cool => Lut3d::from_fn(name, EDGE, |[r, g, b]| {
                [clamp01(r * 0.9), g, clamp01(b * 1.08 + 0.02)]
            }),
            BuiltinLut::Sepia => Lut3d::from_fn(name, EDGE, |[r, g, b]| {
                let y = EncodedRgb([r, g, b]).luma();
                [
                    clamp01(y * 1.07 + 0.03),
                    clamp01(y * 0.95),
                    clamp01(y * 0.75),
                ]
            }),
            BuiltinLut::HighContrast => Lut3d::from_fn(name, EDGE, |c| {
                c.map(|v| {
                    let s = v * v * (3.0 - 2.0 * v);
                    s * s * (3.0 - 2.0 * s)
                })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto::ImageStats;

    const SRGB: ColorSpace = ColorSpace::Srgb;

    #[test]
    fn desaturate_makes_r_g_b_equal_and_keeps_the_luminance() {
        for px in [
            [0.8f32, 0.1, 0.05],
            [0.05, 0.6, 0.9],
            [0.3, 0.3, 0.3],
            [2.0, 0.5, 0.0],
        ] {
            let before = LinearRgb(px).luminance();
            let out = desaturate(LinearRgb(px)).get();
            assert_eq!(out[0], out[1], "{px:?} -> {out:?}");
            assert_eq!(out[1], out[2], "{px:?} -> {out:?}");
            let after = LinearRgb(out).luminance();
            assert!(
                (after - before).abs() < 1e-5,
                "{px:?}: luminance {before} became {after}"
            );
        }
        // A saturated colour really loses its chroma.
        assert_ne!(
            desaturate(LinearRgb([0.8, 0.1, 0.05])).get(),
            [0.8, 0.1, 0.05]
        );
    }

    #[test]
    fn equalize_flattens_a_skewed_histogram() {
        // Every pixel crowded into the darkest quarter.
        let pixels: Vec<EncodedRgb> = (0..4096)
            .map(|i| {
                let v = (i % 64) as f32 / 255.0;
                EncodedRgb([v, v, v])
            })
            .collect();
        let stats = ImageStats::from_encoded(&pixels);
        let map = EqualizeMap::from_stats(&stats).expect("a skewed image is equalised");
        let out: Vec<f32> = pixels.iter().map(|p| map.apply(*p).get()[0]).collect();
        let max_in = pixels.iter().map(|p| p.get()[0]).fold(0.0f32, f32::max);
        let max_out = out.iter().copied().fold(0.0f32, f32::max);
        assert!(max_in < 0.26, "the input really is skewed: {max_in}");
        assert!(max_out > 0.99, "equalised values reach white: {max_out}");
        // Flat: each quarter of the output range holds about a quarter of
        // the pixels, where the input held all of them in the first.
        for q in 0..4 {
            let lo = q as f32 / 4.0;
            let hi = lo + 0.25;
            let n = out
                .iter()
                .filter(|v| **v >= lo && (**v < hi || (q == 3 && **v <= 1.0)))
                .count();
            let share = n as f32 / out.len() as f32;
            assert!(
                (share - 0.25).abs() < 0.06,
                "quarter {q} holds {share} of the pixels"
            );
        }
        // Order is kept: equalisation is monotone.
        let mut previous = -1.0f32;
        for i in 0..64 {
            let v = map.eval(i as f32 / 255.0);
            assert!(v >= previous, "not monotone at {i}");
            previous = v;
        }
    }

    #[test]
    fn equalize_leaves_an_already_flat_image_and_an_empty_one_alone() {
        let flat: Vec<EncodedRgb> = (0..256)
            .map(|i| {
                let v = i as f32 / 255.0;
                EncodedRgb([v, v, v])
            })
            .collect();
        assert!(EqualizeMap::from_stats(&ImageStats::from_encoded(&flat)).is_none());
        assert!(EqualizeMap::from_stats(&ImageStats::new()).is_none());
    }

    #[test]
    fn shadows_lifts_dark_pixels_only() {
        let sh = ShadowsHighlights::new([0.5, 0.5, 0.0], [0.0, 0.5, 0.0]).unwrap();
        let dark = sh.apply(EncodedRgb([0.1, 0.1, 0.1])).get();
        assert!(dark[0] > 0.15, "a dark pixel was not lifted: {dark:?}");
        for v in [0.5f32, 0.7, 0.9, 1.0] {
            let out = sh.apply(EncodedRgb([v, v, v])).get();
            assert_eq!(out, [v, v, v], "a pixel at {v} moved");
        }
        // The hue is kept: the lift is the same on every channel.
        let tinted = sh.apply(EncodedRgb([0.12, 0.08, 0.04])).get();
        let lift = tinted[0] - 0.12;
        assert!(lift > 0.0);
        assert!((tinted[1] - 0.08 - lift).abs() < 1e-6);
        assert!((tinted[2] - 0.04 - lift).abs() < 1e-6);
    }

    #[test]
    fn highlights_darken_bright_pixels_only() {
        let sh = ShadowsHighlights::new([0.0, 0.5, 0.0], [0.6, 0.5, 0.0]).unwrap();
        let bright = sh.apply(EncodedRgb([0.95, 0.95, 0.95])).get();
        assert!(bright[0] < 0.9, "{bright:?}");
        let dark = sh.apply(EncodedRgb([0.2, 0.2, 0.2])).get();
        assert_eq!(dark, [0.2, 0.2, 0.2]);
    }

    #[test]
    fn the_spatial_form_reads_the_neighbourhood_and_matches_at_radius_zero() {
        // A dark square in a bright field.
        let (w, h) = (40usize, 40usize);
        let mut pixels = vec![[0.9f32, 0.9, 0.9, 1.0]; w * h];
        for y in 10..30 {
            for x in 10..30 {
                pixels[y * w + x] = [0.01, 0.01, 0.01, 1.0];
            }
        }
        let point = ShadowsHighlights::new([0.8, 0.6, 0.0], [0.0, 0.5, 0.0]).unwrap();
        let mut a = pixels.clone();
        point
            .apply_premultiplied_rgba_spatial(&mut a, w, h, &SRGB)
            .unwrap();
        let prepared = crate::PreparedAdjustment::new(&crate::Adjustment::ShadowsHighlights(point));
        let mut b = pixels.clone();
        prepared.apply_premultiplied_rgba(&mut b, &SRGB);
        for (x, y) in a.iter().zip(&b) {
            for c in 0..4 {
                assert!((x[c] - y[c]).abs() < 1e-5, "{x:?} vs {y:?}");
            }
        }
        // With a radius, the dark square's edge reads the bright surround
        // and is lifted less than its centre.
        let wide = ShadowsHighlights::new([0.8, 0.6, 6.0], [0.0, 0.5, 0.0]).unwrap();
        let mut c = pixels.clone();
        wide.apply_premultiplied_rgba_spatial(&mut c, w, h, &SRGB)
            .unwrap();
        let centre = c[20 * w + 20][0];
        let edge = c[10 * w + 10][0];
        assert!(centre > edge, "centre {centre} vs edge {edge}");
        assert!(centre > 0.01);
        assert!(matches!(
            wide.apply_premultiplied_rgba_spatial(&mut c, w + 1, h, &SRGB),
            Err(AdjustmentError::BufferShape { .. })
        ));
    }

    #[test]
    fn replace_color_shifts_only_the_sampled_hue_range() {
        let red = [0.85f32, 0.1, 0.1];
        let rc = ReplaceColor::new(red, ReplaceColor::DEFAULT_FUZZINESS, 120.0, 0.0, 0.0).unwrap();
        // The sampled colour itself rotates to green.
        let out = rc.apply(EncodedRgb(red)).get();
        assert!(out[1] > out[0], "red did not move toward green: {out:?}");
        // A near red is shifted partly.
        let near = rc.apply(EncodedRgb([0.8, 0.12, 0.12])).get();
        assert_ne!(near, [0.8, 0.12, 0.12]);
        // Far colours are untouched, bit for bit.
        for far in [[0.1f32, 0.2, 0.85], [0.1, 0.8, 0.1], [0.5, 0.5, 0.5]] {
            assert_eq!(rc.apply(EncodedRgb(far)).get(), far, "{far:?} moved");
        }
        assert!(ReplaceColor::new(red, 0.2, 0.0, 0.0, 0.0)
            .unwrap()
            .is_identity());
    }

    fn cube_text(lut: &Lut3d) -> String {
        let mut s = format!(
            "# a comment\nTITLE \"{}\"\nLUT_3D_SIZE {}\n",
            lut.name(),
            lut.size()
        );
        s.push_str("DOMAIN_MIN 0 0 0\nDOMAIN_MAX 1.0 1.0 1.0\n\n");
        for e in lut.table() {
            s.push_str(&format!("{} {} {}\n", e[0], e[1], e[2]));
        }
        s
    }

    #[test]
    fn an_identity_cube_is_the_identity_and_an_invert_cube_inverts() {
        let identity = Lut3d::parse_cube("file", &cube_text(&Lut3d::identity(9))).unwrap();
        assert!(identity.is_identity());
        let invert = Lut3d::parse_cube("file", &cube_text(&BuiltinLut::Invert.lut())).unwrap();
        assert_eq!(invert.name(), "Invert");
        assert!(!invert.is_identity());
        for px in [
            [0.0f32, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.2, 0.55, 0.9],
            [0.33, 0.01, 0.77],
        ] {
            let same = identity.apply(EncodedRgb(px)).get();
            let inv = invert.apply(EncodedRgb(px)).get();
            for c in 0..3 {
                assert!((same[c] - px[c]).abs() < 1e-5, "identity moved {px:?}");
                assert!(
                    (inv[c] - (1.0 - px[c])).abs() < 1e-5,
                    "invert of {px:?} is {inv:?}"
                );
            }
        }
    }

    #[test]
    fn a_bad_cube_is_refused_with_a_reason() {
        for text in [
            "LUT_1D_SIZE 4\n0 0 0\n",
            "0 0 0\n1 1 1\n",
            "LUT_3D_SIZE 2\n0 0 0\n",
            "LUT_3D_SIZE 2\nDOMAIN_MAX 2 2 2\n",
            "LUT_3D_SIZE 2\n0 0 zero\n",
            "LUT_3D_SIZE 1\n0 0 0\n",
        ] {
            assert!(
                matches!(
                    Lut3d::parse_cube("x", text),
                    Err(AdjustmentError::InvalidLut { .. })
                ),
                "{text:?} was accepted"
            );
        }
    }

    #[test]
    fn every_builtin_changes_a_pixel() {
        for b in BuiltinLut::ALL {
            let lut = b.lut();
            assert!(!lut.is_identity(), "{b:?}");
            assert_ne!(
                lut.apply(EncodedRgb([0.3, 0.5, 0.6])).get(),
                [0.3, 0.5, 0.6]
            );
        }
    }

    #[test]
    fn the_new_adjustments_round_trip_through_the_stored_vocabulary() {
        use crate::Adjustment;
        let all = vec![
            Adjustment::Desaturate,
            Adjustment::Equalize,
            Adjustment::ShadowsHighlights(ShadowsHighlights::DEFAULT),
            Adjustment::ReplaceColor(
                ReplaceColor::new([0.8, 0.1, 0.1], 0.2, 90.0, 0.1, -0.1).unwrap(),
            ),
            Adjustment::ColorLookup(BuiltinLut::Warm.lut()),
        ];
        for adj in all {
            let kind = adj.to_layer_kind();
            let json = serde_json::to_string(&kind).unwrap();
            let back: layer_model::AdjustmentKind = serde_json::from_str(&json).unwrap();
            assert_eq!(Adjustment::try_from_layer_kind(&back).unwrap(), adj);
            assert_eq!(Adjustment::from(&back), adj);
        }
    }

    #[test]
    fn a_corrupt_stored_lookup_opens_as_no_lookup_and_is_reported_strictly() {
        use crate::Adjustment;
        let corrupt = layer_model::AdjustmentKind::ColorLookup {
            name: "broken".to_string(),
            size: 3,
            table: vec![[0.0; 3]; 8],
        };
        let Adjustment::ColorLookup(lenient) = Adjustment::from(&corrupt) else {
            panic!("a lookup came back as something else");
        };
        assert!(lenient.is_identity());
        assert!(matches!(
            Adjustment::try_from_layer_kind(&corrupt),
            Err(AdjustmentError::InvalidLut { .. })
        ));
        // A huge stored edge is refused, not cubed.
        let huge = layer_model::AdjustmentKind::ColorLookup {
            name: String::new(),
            size: u32::MAX,
            table: vec![],
        };
        assert!(Adjustment::try_from_layer_kind(&huge).is_err());
    }

    #[test]
    fn equalize_is_an_analysis_like_the_auto_commands() {
        use crate::{Adjustment, PreparedAdjustment};
        assert!(Adjustment::Equalize.needs_stats());
        assert!(!Adjustment::Desaturate.needs_stats());
        assert!(PreparedAdjustment::new(&Adjustment::Equalize).is_identity());
        let skewed: Vec<EncodedRgb> = (0..512)
            .map(|i| {
                let v = (i % 40) as f32 / 255.0;
                EncodedRgb([v, v * 0.9, v * 0.8])
            })
            .collect();
        let prepared = PreparedAdjustment::with_stats(
            &Adjustment::Equalize,
            &ImageStats::from_encoded(&skewed),
        );
        assert!(!prepared.is_identity());
    }
}
