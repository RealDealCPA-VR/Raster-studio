//! Colour quantisation for Image > Mode > Indexed Color: build a palette of at
//! most 256 colours, then map every pixel onto it, optionally with
//! Floyd-Steinberg error diffusion.
//!
//! The document keeps storing RGBA; an indexed document is one whose visible
//! colours all come from its palette. GIF export keeps those colours exactly
//! (the GIF encoder needs no more than 256); `raster::export::ink` can write
//! them as a PNG-8 palette, but the app's File > Export does not select that
//! writer yet, so PNG export of an indexed document is RGBA. Alpha is never quantised here (a GIF's 1-bit
//! transparency is the exporter's business).

use std::collections::HashMap;

/// Fewest colours an indexed palette may hold.
pub const MIN_COLORS: u16 = 2;
/// Most colours an indexed palette may hold.
pub const MAX_COLORS: u16 = 256;

/// Where the palette comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PaletteKind {
    /// The image's own colours, when there are no more than the limit.
    /// Refused ([`QuantizeError::TooManyColors`]) otherwise.
    Exact,
    /// The 216-colour web-safe cube (steps of 51); the colour count is not
    /// consulted.
    Web,
    /// Evenly spaced levels, never more than `colors` entries: a grey ramp
    /// of exactly `colors` steps below 8, otherwise an evenly spaced RGB box
    /// whose size fits (levels grown green, red, blue in turn).
    Uniform,
    /// Median cut over the image's own colour histogram.
    #[default]
    Adaptive,
}

impl PaletteKind {
    pub const ALL: [PaletteKind; 4] = [
        PaletteKind::Exact,
        PaletteKind::Web,
        PaletteKind::Uniform,
        PaletteKind::Adaptive,
    ];
}

/// How pixels are mapped onto the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Dither {
    /// Nearest palette colour.
    None,
    /// Floyd-Steinberg error diffusion.
    #[default]
    Diffusion,
}

impl Dither {
    pub const ALL: [Dither; 2] = [Dither::None, Dither::Diffusion];
}

/// Why a palette could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuantizeError {
    /// `Exact` over an image with more distinct colours than the limit.
    TooManyColors { found: usize, limit: u16 },
    /// A colour count outside [`MIN_COLORS`]`..=`[`MAX_COLORS`].
    BadColorCount(u16),
}

impl std::fmt::Display for QuantizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QuantizeError::TooManyColors { found, limit } => write!(
                f,
                "the image has {found} colours, more than the {limit} an Exact palette can hold"
            ),
            QuantizeError::BadColorCount(n) => write!(
                f,
                "{n} colours is outside the {MIN_COLORS}-{MAX_COLORS} an indexed palette holds"
            ),
        }
    }
}

impl std::error::Error for QuantizeError {}

/// How many times each opaque-enough colour occurs.
#[derive(Debug, Clone, Default)]
pub struct Histogram {
    counts: HashMap<[u8; 3], u64>,
}

impl Histogram {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count every visible (alpha > 0) pixel of a straight RGBA8 buffer.
    pub fn add_rgba8(&mut self, rgba: &[u8]) {
        for px in rgba.as_chunks::<4>().0 {
            if px[3] > 0 {
                *self.counts.entry([px[0], px[1], px[2]]).or_insert(0) += 1;
            }
        }
    }

    /// Distinct colours counted.
    pub fn distinct(&self) -> usize {
        self.counts.len()
    }
}

/// Build a palette of at most `colors` entries (see [`PaletteKind`]).
pub fn build_palette(
    histogram: &Histogram,
    kind: PaletteKind,
    colors: u16,
) -> Result<Vec<[u8; 3]>, QuantizeError> {
    if !(MIN_COLORS..=MAX_COLORS).contains(&colors) {
        return Err(QuantizeError::BadColorCount(colors));
    }
    let mut own: Vec<[u8; 3]> = histogram.counts.keys().copied().collect();
    own.sort_unstable();
    match kind {
        PaletteKind::Exact => {
            if own.len() > usize::from(colors) {
                return Err(QuantizeError::TooManyColors {
                    found: own.len(),
                    limit: colors,
                });
            }
            if own.is_empty() {
                own.push([0, 0, 0]);
            }
            Ok(own)
        }
        PaletteKind::Web => Ok(cube(6)),
        PaletteKind::Uniform => Ok(uniform(usize::from(colors))),
        PaletteKind::Adaptive => {
            if own.len() <= usize::from(colors) {
                if own.is_empty() {
                    own.push([0, 0, 0]);
                }
                return Ok(own);
            }
            let entries: Vec<([u8; 3], u64)> =
                own.iter().map(|c| (*c, histogram.counts[c])).collect();
            Ok(median_cut(entries, usize::from(colors)))
        }
    }
}

/// An evenly spaced `n x n x n` cube, `0` and `255` included.
fn cube(n: usize) -> Vec<[u8; 3]> {
    grid([n; 3])
}

/// The Uniform palette for a count of `colors` (`2..=256`): never more
/// entries than asked for. Below 8 no colour box fits (two levels per channel
/// is already 8), so it is an evenly spaced grey ramp of exactly `colors`
/// steps, black and white included. From 8 up it is an evenly spaced RGB box
/// (levels per channel, `r x g x b`): starting at 2x2x2, rounds offer one more
/// level to green, then red, then blue, each taken only while the product
/// stays within `colors`, until a round grows nothing (16 -> 2x4x2 = 16,
/// 256 -> 6x7x6 = 252, since 6x8x6 and 7x7x6 exceed it).
fn uniform(colors: usize) -> Vec<[u8; 3]> {
    if colors < 8 {
        let step = |i: usize| ((i * 255) as f64 / (colors - 1) as f64).round() as u8;
        return (0..colors).map(|i| [step(i); 3]).collect();
    }
    // Axis order for the next level: green, red, blue.
    let mut levels = [2usize; 3];
    loop {
        let mut grew = false;
        for axis in [1, 0, 2] {
            let mut next = levels;
            next[axis] += 1;
            if next.iter().product::<usize>() <= colors {
                levels = next;
                grew = true;
            }
        }
        if !grew {
            return grid(levels);
        }
    }
}

/// An evenly spaced `r x g x b` box, `0` and `255` included on every axis.
fn grid(levels: [usize; 3]) -> Vec<[u8; 3]> {
    let level = |i: usize, n: usize| ((i * 255) as f64 / (n - 1) as f64).round() as u8;
    let mut out = Vec::with_capacity(levels.iter().product());
    for r in 0..levels[0] {
        for g in 0..levels[1] {
            for b in 0..levels[2] {
                out.push([
                    level(r, levels[0]),
                    level(g, levels[1]),
                    level(b, levels[2]),
                ]);
            }
        }
    }
    out
}

/// Heckbert's median cut: split the box with the widest channel range at its
/// weighted median until there are `target` boxes, then average each.
fn median_cut(entries: Vec<([u8; 3], u64)>, target: usize) -> Vec<[u8; 3]> {
    let mut boxes: Vec<Vec<([u8; 3], u64)>> = vec![entries];
    while boxes.len() < target {
        // The splittable box with the widest single-channel range.
        let mut best: Option<(usize, usize, u8)> = None;
        for (i, b) in boxes.iter().enumerate() {
            if b.len() < 2 {
                continue;
            }
            for ch in 0..3 {
                let lo = b.iter().map(|e| e.0[ch]).min().unwrap_or(0);
                let hi = b.iter().map(|e| e.0[ch]).max().unwrap_or(0);
                let range = hi - lo;
                if best.is_none_or(|(_, _, r)| range > r) {
                    best = Some((i, ch, range));
                }
            }
        }
        let Some((i, ch, _)) = best else {
            break;
        };
        let mut b = boxes.swap_remove(i);
        b.sort_unstable_by_key(|e| (e.0[ch], e.0));
        let total: u64 = b.iter().map(|e| e.1).sum();
        let mut acc = 0u64;
        let mut cut = 1;
        for (j, e) in b.iter().enumerate() {
            acc += e.1;
            if acc * 2 >= total {
                cut = (j + 1).clamp(1, b.len() - 1);
                break;
            }
        }
        let tail = b.split_off(cut);
        boxes.push(b);
        boxes.push(tail);
    }
    let mut palette: Vec<[u8; 3]> = boxes
        .iter()
        .map(|b| {
            let total: u64 = b.iter().map(|e| e.1).sum::<u64>().max(1);
            let mut sum = [0u64; 3];
            for (c, n) in b {
                for ch in 0..3 {
                    sum[ch] += u64::from(c[ch]) * n;
                }
            }
            sum.map(|s| ((s + total / 2) / total).min(255) as u8)
        })
        .collect();
    palette.sort_unstable();
    palette.dedup();
    palette
}

/// Index of the palette colour nearest `rgb` (squared RGB distance; ties go
/// to the lower index). `palette` must not be empty.
pub fn nearest(palette: &[[u8; 3]], rgb: [i32; 3]) -> usize {
    let mut best = 0;
    let mut best_d = i64::MAX;
    for (i, p) in palette.iter().enumerate() {
        let d: i64 = (0..3)
            .map(|c| {
                let e = i64::from(rgb[c] - i32::from(p[c]));
                e * e
            })
            .sum();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// Map every visible pixel of a `width`-wide straight RGBA8 buffer onto
/// `palette`, in place. Alpha is kept; fully transparent pixels are skipped
/// (and neither receive nor spread diffusion error).
pub fn remap_rgba8(rgba: &mut [u8], width: usize, palette: &[[u8; 3]], dither: Dither) {
    if palette.is_empty() || width == 0 {
        return;
    }
    let mut cache: HashMap<[i32; 3], usize> = HashMap::new();
    let mut pick = |rgb: [i32; 3]| *cache.entry(rgb).or_insert_with(|| nearest(palette, rgb));
    match dither {
        Dither::None => {
            for px in rgba.as_chunks_mut::<4>().0 {
                if px[3] == 0 {
                    continue;
                }
                let p = palette[pick([px[0], px[1], px[2]].map(i32::from))];
                px[..3].copy_from_slice(&p);
            }
        }
        Dither::Diffusion => {
            let height = rgba.len() / 4 / width;
            // Error carried into this row and the next, in 1/16ths.
            let mut this_row = vec![[0i32; 3]; width + 2];
            let mut next_row = vec![[0i32; 3]; width + 2];
            for y in 0..height {
                for x in 0..width {
                    let at = (y * width + x) * 4;
                    if rgba[at + 3] == 0 {
                        continue;
                    }
                    let carried = this_row[x + 1];
                    let want: [i32; 3] = std::array::from_fn(|c| {
                        (i32::from(rgba[at + c]) + carried[c] / 16).clamp(0, 255)
                    });
                    let p = palette[pick(want)];
                    let err: [i32; 3] = std::array::from_fn(|c| want[c] - i32::from(p[c]));
                    rgba[at..at + 3].copy_from_slice(&p);
                    for c in 0..3 {
                        this_row[x + 2][c] += err[c] * 7;
                        next_row[x][c] += err[c] * 3;
                        next_row[x + 1][c] += err[c] * 5;
                        next_row[x + 2][c] += err[c];
                    }
                }
                std::mem::swap(&mut this_row, &mut next_row);
                next_row.iter_mut().for_each(|e| *e = [0; 3]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gradient with far more than 16 colours.
    fn gradient(w: usize, h: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                out.extend_from_slice(&[
                    (x * 255 / (w - 1)) as u8,
                    (y * 255 / (h - 1)) as u8,
                    ((x + y) * 255 / (w + h - 2)) as u8,
                    255,
                ]);
            }
        }
        out
    }

    fn distinct(rgba: &[u8]) -> usize {
        let mut h = Histogram::new();
        h.add_rgba8(rgba);
        h.distinct()
    }

    #[test]
    fn sixteen_adaptive_colours_leave_at_most_sixteen_distinct() {
        for dither in Dither::ALL {
            let mut img = gradient(64, 48);
            assert!(distinct(&img) > 16);
            let mut h = Histogram::new();
            h.add_rgba8(&img);
            let palette = build_palette(&h, PaletteKind::Adaptive, 16).unwrap();
            assert!(palette.len() <= 16);
            remap_rgba8(&mut img, 64, &palette, dither);
            let n = distinct(&img);
            assert!(n <= 16, "{dither:?}: {n} colours");
            assert!(n > 1, "{dither:?}: collapsed to one colour");
        }
    }

    #[test]
    fn a_uniform_palette_never_holds_more_colours_than_asked_for() {
        let h = Histogram::new();
        for colors in MIN_COLORS..=MAX_COLORS {
            let palette = build_palette(&h, PaletteKind::Uniform, colors).unwrap();
            let n = palette.len();
            assert!(n <= usize::from(colors), "{colors} asked, {n} built");
            let unique: std::collections::HashSet<_> = palette.iter().collect();
            assert_eq!(unique.len(), n, "{colors}: duplicate entries");
            assert!(palette.contains(&[0, 0, 0]) && palette.contains(&[255, 255, 255]));
        }
        // Below 8 the ramp holds exactly the count asked for.
        assert_eq!(
            build_palette(&h, PaletteKind::Uniform, 4).unwrap(),
            vec![[0; 3], [85; 3], [170; 3], [255; 3]]
        );
        assert_eq!(build_palette(&h, PaletteKind::Uniform, 2).unwrap().len(), 2);
        assert_eq!(build_palette(&h, PaletteKind::Uniform, 8).unwrap().len(), 8);
    }

    #[test]
    fn every_palette_kind_respects_its_count() {
        let img = gradient(40, 40);
        let mut h = Histogram::new();
        h.add_rgba8(&img);
        assert_eq!(build_palette(&h, PaletteKind::Web, 256).unwrap().len(), 216);
        assert_eq!(
            build_palette(&h, PaletteKind::Uniform, 16).unwrap().len(),
            16
        );
        assert_eq!(
            build_palette(&h, PaletteKind::Uniform, 256).unwrap().len(),
            252
        );
        assert!(build_palette(&h, PaletteKind::Adaptive, 2).unwrap().len() <= 2);
        assert!(matches!(
            build_palette(&h, PaletteKind::Exact, 256),
            Err(QuantizeError::TooManyColors { .. })
        ));
        assert_eq!(
            build_palette(&h, PaletteKind::Adaptive, 1),
            Err(QuantizeError::BadColorCount(1))
        );
        assert_eq!(
            build_palette(&h, PaletteKind::Adaptive, 257),
            Err(QuantizeError::BadColorCount(257))
        );
    }

    #[test]
    fn an_image_with_few_colours_keeps_them_exactly() {
        let mut img = vec![
            10, 20, 30, 255, 200, 100, 0, 255, 10, 20, 30, 255, 1, 2, 3, 0,
        ];
        let before = img.clone();
        let mut h = Histogram::new();
        h.add_rgba8(&img);
        let palette = build_palette(&h, PaletteKind::Exact, 8).unwrap();
        assert_eq!(palette, vec![[10, 20, 30], [200, 100, 0]]);
        remap_rgba8(&mut img, 2, &palette, Dither::Diffusion);
        assert_eq!(
            img, before,
            "exact colours and transparent pixels are untouched"
        );
    }
}
