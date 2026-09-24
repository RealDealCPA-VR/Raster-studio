//! Edit ▸ Auto-Blend Layers (W10-G): several aligned layers into one image.
//!
//! The layers arrive already in document space (the shell draws each one
//! through its own transform first, so an Auto-Align beforehand is honoured).
//! Two methods, as in Photoshop:
//!
//! # Panorama — seam finding and feathered masks
//!
//! The layers are laid down in order onto an accumulator. Where the next
//! layer overlaps what is already there, a **seam** is found through the
//! overlap: a minimum-cost path, by dynamic programming, over the per-pixel
//! colour difference of the two, running along the overlap's long axis (a
//! vertical seam through a tall overlap, a horizontal one through a wide
//! overlap). Each path step may move one pixel sideways. The side of the seam
//! that faces the new layer's own content takes the new layer, the other side
//! keeps the accumulator — so the cut runs where the two agree and a moving
//! object or a parallax difference is not sliced through. The hard mask that
//! results is **feathered** (a box blur of radius [`SEAM_FEATHER`] twice,
//! i.e. a tent) inside the overlap only; outside it each layer is kept as it
//! is.
//!
//! # Stack Images — focus stacking by per-pixel sharpness
//!
//! Each layer's sharpness is the local energy of its Laplacian: the squared
//! 4-neighbour Laplacian of its gamma-encoded luminance, box-averaged over a
//! [`FOCUS_WINDOW`]-radius window and weighted by coverage. Every pixel is
//! claimed by the layer that is sharpest there; those hard masks are
//! feathered by [`FOCUS_FEATHER`] and renormalised, and the result is the
//! mask-weighted sum of the layers. Two photographs focused at different
//! depths therefore come back with each one's sharp region.

use crate::FilterBuffer;

/// The seam mask's feather radius, in pixels (applied twice).
pub const SEAM_FEATHER: usize = 4;
/// The focus measure's averaging radius, in pixels.
pub const FOCUS_WINDOW: usize = 4;
/// The focus masks' feather radius, in pixels (applied twice).
pub const FOCUS_FEATHER: usize = 2;

/// How Auto-Blend combines the layers.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum BlendMethod {
    /// Stitch overlapping layers along the seams where they agree.
    #[default]
    Panorama,
    /// Keep the sharpest layer at every pixel.
    StackImages,
}

impl BlendMethod {
    pub const ALL: [BlendMethod; 2] = [BlendMethod::Panorama, BlendMethod::StackImages];
}

/// Why no blend came back.
#[derive(Clone, PartialEq, Debug, thiserror::Error)]
pub enum BlendError {
    #[error("Auto-Blend needs at least two layers")]
    TooFewLayers,
    #[error("the layers are not all the same size")]
    SizeMismatch,
}

/// Blend `layers` (bottom first) by `method`.
pub fn auto_blend(
    layers: &[FilterBuffer],
    method: BlendMethod,
) -> Result<FilterBuffer, BlendError> {
    match method {
        BlendMethod::Panorama => panorama(layers),
        BlendMethod::StackImages => focus_stack(layers),
    }
}

fn check(layers: &[FilterBuffer]) -> Result<(usize, usize), BlendError> {
    if layers.len() < 2 {
        return Err(BlendError::TooFewLayers);
    }
    let (w, h) = layers[0].dimensions();
    if layers.iter().any(|l| l.dimensions() != (w, h)) {
        return Err(BlendError::SizeMismatch);
    }
    Ok((w as usize, h as usize))
}

fn luma(p: [f32; 4]) -> f32 {
    (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
        .max(0.0)
        .powf(1.0 / 2.2)
}

/// A separable box blur of radius `r` over a `w x h` plane, clamped edges.
fn box_blur(plane: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 || w == 0 || h == 0 {
        return plane.to_vec();
    }
    let r = r as i64;
    let n = (2 * r + 1) as f32;
    let mut tmp = vec![0.0f32; plane.len()];
    for y in 0..h {
        let row = &plane[y * w..(y + 1) * w];
        let at = |x: i64| row[x.clamp(0, w as i64 - 1) as usize];
        let mut s: f32 = (-r..=r).map(at).sum();
        for x in 0..w as i64 {
            tmp[y * w + x as usize] = s / n;
            s += at(x + r + 1) - at(x - r);
        }
    }
    let mut out = vec![0.0f32; plane.len()];
    for x in 0..w {
        let at = |y: i64| tmp[y.clamp(0, h as i64 - 1) as usize * w + x];
        let mut s: f32 = (-r..=r).map(at).sum();
        for y in 0..h as i64 {
            out[y as usize * w + x] = s / n;
            s += at(y + r + 1) - at(y - r);
        }
    }
    out
}

/// Stack Images: keep the sharpest layer at every pixel. See the module
/// documentation.
pub fn focus_stack(layers: &[FilterBuffer]) -> Result<FilterBuffer, BlendError> {
    let (w, h) = check(layers)?;
    let energy: Vec<Vec<f32>> = layers
        .iter()
        .map(|layer| {
            let px = layer.pixels();
            let l: Vec<f32> = px.iter().map(|&p| luma(p)).collect();
            let at = |x: i64, y: i64| {
                l[y.clamp(0, h as i64 - 1) as usize * w + x.clamp(0, w as i64 - 1) as usize]
            };
            let mut lap = vec![0.0f32; w * h];
            for y in 0..h as i64 {
                for x in 0..w as i64 {
                    let v =
                        at(x - 1, y) + at(x + 1, y) + at(x, y - 1) + at(x, y + 1) - 4.0 * at(x, y);
                    lap[y as usize * w + x as usize] = v * v;
                }
            }
            let mut e = box_blur(&lap, w, h, FOCUS_WINDOW);
            for (e, p) in e.iter_mut().zip(px) {
                *e *= p[3].clamp(0.0, 1.0);
            }
            e
        })
        .collect();
    let mut masks = vec![vec![0.0f32; w * h]; layers.len()];
    for i in 0..w * h {
        let mut best = 0;
        for k in 1..layers.len() {
            if energy[k][i] > energy[best][i] {
                best = k;
            }
        }
        masks[best][i] = 1.0;
    }
    let masks: Vec<Vec<f32>> = masks
        .iter()
        .map(|m| box_blur(&box_blur(m, w, h, FOCUS_FEATHER), w, h, FOCUS_FEATHER))
        .collect();
    let mut out = layers[0].clone();
    for (i, o) in out.pixels_mut().iter_mut().enumerate() {
        let total: f32 = masks.iter().map(|m| m[i]).sum();
        let mut acc = [0.0f32; 4];
        for (k, layer) in layers.iter().enumerate() {
            let wgt = if total > 1e-6 {
                masks[k][i] / total
            } else {
                1.0 / layers.len() as f32
            };
            let p = layer.pixels()[i];
            for c in 0..4 {
                acc[c] += wgt * p[c];
            }
        }
        *o = acc;
    }
    Ok(out)
}

/// Panorama: stitch the layers along minimum-difference seams. See the
/// module documentation.
pub fn panorama(layers: &[FilterBuffer]) -> Result<FilterBuffer, BlendError> {
    let (w, h) = check(layers)?;
    let mut acc = layers[0].clone();
    for layer in &layers[1..] {
        let a = acc.pixels();
        let b = layer.pixels();
        let has_a: Vec<bool> = a.iter().map(|p| p[3] > 0.5).collect();
        let has_b: Vec<bool> = b.iter().map(|p| p[3] > 0.5).collect();
        // The overlap's bounding box.
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if has_a[i] && has_b[i] {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        // The new layer's weight, 1 where it wins.
        let mut take_b: Vec<f32> = (0..w * h)
            .map(|i| f32::from(u8::from(has_b[i] && !has_a[i])))
            .collect();
        if x0 != usize::MAX {
            let vertical = (x1 - x0) <= (y1 - y0);
            // Walk the seam in "along/across" coordinates.
            let (along0, along1, across0, across1) = if vertical {
                (y0, y1, x0, x1)
            } else {
                (x0, x1, y0, y1)
            };
            let idx = |along: usize, across: usize| {
                if vertical {
                    along * w + across
                } else {
                    across * w + along
                }
            };
            let cost = |along: usize, across: usize| -> f32 {
                let i = idx(along, across);
                if !(has_a[i] && has_b[i]) {
                    return 1.0e3;
                }
                (0..3).map(|c| (a[i][c] - b[i][c]).abs()).sum()
            };
            let span = across1 - across0 + 1;
            let rows = along1 - along0 + 1;
            let mut cum = vec![0.0f32; rows * span];
            for r in 0..rows {
                for c in 0..span {
                    let here = cost(along0 + r, across0 + c);
                    cum[r * span + c] = if r == 0 {
                        here
                    } else {
                        let prev = &cum[(r - 1) * span..r * span];
                        let lo = c.saturating_sub(1);
                        let hi = (c + 1).min(span - 1);
                        here + prev[lo..=hi].iter().copied().fold(f32::INFINITY, f32::min)
                    };
                }
            }
            // Backtrack.
            let mut seam = vec![0usize; rows];
            let last = &cum[(rows - 1) * span..];
            seam[rows - 1] = (0..span)
                .min_by(|&i, &j| last[i].total_cmp(&last[j]))
                .unwrap_or(0);
            for r in (0..rows - 1).rev() {
                let c = seam[r + 1];
                let lo = c.saturating_sub(1);
                let hi = (c + 1).min(span - 1);
                let row = &cum[r * span..(r + 1) * span];
                seam[r] = (lo..=hi)
                    .min_by(|&i, &j| row[i].total_cmp(&row[j]))
                    .unwrap_or(c);
            }
            // Which side faces the new layer's own content: compare the
            // centroid (across the seam) of the new-only pixels with the
            // accumulator-only ones.
            let (mut sb, mut nb, mut sa, mut na) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    let across = if vertical { x } else { y } as f64;
                    if has_b[i] && !has_a[i] {
                        sb += across;
                        nb += 1.0;
                    } else if has_a[i] && !has_b[i] {
                        sa += across;
                        na += 1.0;
                    }
                }
            }
            let b_after = match (nb > 0.0, na > 0.0) {
                (true, true) => sb / nb > sa / na,
                (true, false) => sb / nb > (across0 + across1) as f64 / 2.0,
                (false, true) => sa / na < (across0 + across1) as f64 / 2.0,
                (false, false) => true,
            };
            for (r, &s) in seam.iter().enumerate() {
                for c in 0..span {
                    let i = idx(along0 + r, across0 + c);
                    if has_a[i] && has_b[i] {
                        let after = c > s || (c == s && b_after);
                        take_b[i] = f32::from(u8::from(after == b_after));
                    }
                }
            }
            let feathered = box_blur(&box_blur(&take_b, w, h, SEAM_FEATHER), w, h, SEAM_FEATHER);
            for i in 0..w * h {
                if has_a[i] && has_b[i] {
                    take_b[i] = feathered[i];
                }
            }
        }
        let mut next = acc.clone();
        for (i, o) in next.pixels_mut().iter_mut().enumerate() {
            let (pa, pb) = (a[i], b[i]);
            *o = if has_a[i] || has_b[i] {
                let t = take_b[i];
                if !has_a[i] && !has_b[i] {
                    pa
                } else if has_a[i] && has_b[i] {
                    std::array::from_fn(|c| pa[c] + (pb[c] - pa[c]) * t)
                } else if has_b[i] {
                    // Only the new layer covers it (the old may be faint).
                    std::array::from_fn(|c| pb[c] + pa[c] * (1.0 - pb[3]))
                } else {
                    std::array::from_fn(|c| pa[c] + pb[c] * (1.0 - pa[3]))
                }
            } else {
                // Neither is opaque enough to claim it: plain "over".
                std::array::from_fn(|c| pb[c] + pa[c] * (1.0 - pb[3]))
            };
        }
        acc = next;
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blur::gaussian_blur;
    use crate::support::EdgeMode;

    /// Fine detail everywhere: a hashed grey per pixel.
    fn detail(w: u32, h: u32) -> FilterBuffer {
        let mut b = FilterBuffer::transparent(w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let v = crate::rng::hash_unit(9, i64::from(x / 2), i64::from(y / 2));
                b.set(x, y, [v, v, v, 1.0]);
            }
        }
        b
    }

    fn halves(left: &FilterBuffer, right: &FilterBuffer) -> FilterBuffer {
        let (w, h) = left.dimensions();
        let mut out = left.clone();
        for y in 0..h {
            for x in w / 2..w {
                out.set(x, y, right.get(x, y));
            }
        }
        out
    }

    fn mean_error(a: &FilterBuffer, b: &FilterBuffer, xs: std::ops::Range<u32>) -> f32 {
        let (_, h) = a.dimensions();
        let mut s = 0.0;
        let mut n = 0.0;
        for y in 0..h {
            for x in xs.clone() {
                s += (a.get(x, y)[0] - b.get(x, y)[0]).abs();
                n += 1.0;
            }
        }
        s / n
    }

    #[test]
    fn focus_stacking_picks_the_sharp_half_of_each_image() {
        let sharp = detail(96, 64);
        let soft = gaussian_blur(&sharp, 3.0, EdgeMode::Clamp);
        // One photograph focused on the left, the other on the right.
        let near = halves(&sharp, &soft);
        let far = halves(&soft, &sharp);
        let out = focus_stack(&[near.clone(), far.clone()]).unwrap();
        // Away from the join, the stack is the sharp original.
        let left = 0..40;
        let right = 56..96;
        let e_left = mean_error(&out, &sharp, left.clone());
        let e_right = mean_error(&out, &sharp, right.clone());
        assert!(e_left < 0.01, "left half error {e_left}");
        assert!(e_right < 0.01, "right half error {e_right}");
        // ...and each input was clearly wrong on its blurred half.
        assert!(mean_error(&near, &sharp, right) > 0.05);
        assert!(mean_error(&far, &sharp, left) > 0.05);
    }

    #[test]
    fn a_panorama_of_two_overlapping_crops_rebuilds_the_scene() {
        let scene = detail(120, 50);
        let crop = |x0: u32, x1: u32| {
            let mut b = FilterBuffer::transparent(120, 50).unwrap();
            for y in 0..50 {
                for x in x0..x1 {
                    b.set(x, y, scene.get(x, y));
                }
            }
            b
        };
        let out = panorama(&[crop(0, 75), crop(45, 120)]).unwrap();
        for (a, b) in out.pixels().iter().zip(scene.pixels()) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 1e-4, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn the_panorama_seam_runs_where_the_layers_agree() {
        // Two crops that disagree on the left third of their overlap (a
        // moving object in one): the seam must avoid it, so the result there
        // is the bottom layer's.
        let scene = detail(120, 50);
        let mut a = FilterBuffer::transparent(120, 50).unwrap();
        let mut b = FilterBuffer::transparent(120, 50).unwrap();
        for y in 0..50 {
            for x in 0..80 {
                a.set(x, y, scene.get(x, y));
            }
            for x in 40..120 {
                let mut p = scene.get(x, y);
                if x < 52 {
                    p = [1.0, 0.0, 0.0, 1.0];
                }
                b.set(x, y, p);
            }
        }
        let out = panorama(&[a, b]).unwrap();
        for y in 0..50 {
            // The feather reaches 8 pixels either side of the seam.
            for x in 40..44 {
                assert_eq!(out.get(x, y), scene.get(x, y), "({x},{y}) took the object");
            }
        }
    }

    #[test]
    fn fewer_than_two_layers_is_refused() {
        let one = detail(8, 8);
        assert_eq!(
            focus_stack(std::slice::from_ref(&one)),
            Err(BlendError::TooFewLayers)
        );
        let other = detail(9, 8);
        assert_eq!(panorama(&[one, other]), Err(BlendError::SizeMismatch));
    }
}
