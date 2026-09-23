//! A naive but deterministic RGB <-> CMYK model, for Image > Mode > CMYK
//! Color, View > Proof Colors and View > Gamut Warning.
//!
//! # What this is, and what it is not
//!
//! This is **not** an ICC press profile. There is no measured characterisation
//! of a press, paper or ink set in this build. It is the same kind of
//! approach Photopea takes in a browser: the document keeps an RGB working
//! buffer, and "CMYK" is a documented, reproducible transform that the mode
//! conversion, the soft proof and the export all share, so the three can never
//! disagree about what a colour becomes.
//!
//! # The model
//!
//! Colours are 8-bit **sRGB-encoded** values normalised to `0..=1`. Printing
//! is modelled as subtractive filters: each ink transmits a fraction of each
//! RGB channel, coverage scales its absorption, and black darkens uniformly:
//!
//! ```text
//! out_j = (1 - k) * PRODUCT over inks i of (1 - coverage_i * absorption_ij)
//! absorption_ij = 1 - INK_RGB[i][j] / 255
//! ```
//!
//! [`INK_RGB`] holds sRGB approximations of process cyan, magenta and yellow
//! (the commonly quoted SWOP-coated screen values). Because real inks are not
//! ideal filters, a pure sRGB primary is *not* reachable: the round trip is how
//! the gamut boundary shows up.
//!
//! **Separation** (RGB -> CMYK):
//!
//! 1. Naive complements `c0 = 1 - r`, `m0 = 1 - g`, `y0 = 1 - b`.
//! 2. Grey component replacement: at most `k = GCR * min(c0, m0, y0)` with
//!    [`GCR`] `= 1.0` (maximum GCR — the whole grey component goes to black
//!    ink, which is also full under-colour removal). So every neutral is
//!    printed with black alone and round-trips exactly. Where the inks cannot
//!    print the chromatic remainder at that much black, the black is lowered
//!    (bisection) to the most that still can, and to none for an
//!    out-of-gamut colour.
//! 3. The chromatic remainder `target_j = rgb_j / (1 - k)` is solved for the
//!    three coverages by projected Gauss-Newton least squares on the model
//!    above, coverages bounded to `0..=1`. An in-gamut colour is solved
//!    exactly; an out-of-gamut one lands on the closest printable colour
//!    (least squares in encoded RGB).
//!
//! **Composition** (CMYK -> RGB) is the model itself, rounded to 8 bits.
//!
//! # Documented primaries
//!
//! The round trip `rgb -> cmyk -> rgb` of the sRGB primaries, pinned by
//! `the_primaries_round_trip_to_the_documented_values`:
//!
//! | sRGB in        | CMYK (%)          | sRGB out       |
//! |----------------|-------------------|----------------|
//! | (255, 0, 0)    | 0 / 99 / 100 / 0  | (236, 1, 0)    |
//! | (0, 255, 0)    | 90 / 0 / 99 / 0   | (25, 173, 3)   |
//! | (0, 0, 255)    | 100 / 68 / 0 / 0  | (0, 55, 166)   |
//! | (0, 255, 255)  | 91 / 0 / 0 / 0    | (24, 182, 241) |
//! | (255, 0, 255)  | 0 / 83 / 0 / 0    | (239, 44, 160) |
//! | (255, 255, 0)  | 0 / 0 / 100 / 0   | (255, 242, 1)  |
//! | (128, 128, 128)| 0 / 0 / 0 / 50    | (128, 128, 128)|
//!
//! # Speed
//!
//! The solve costs tens of microseconds a colour. Bulk callers (the mode
//! conversion, the canvas proof) go through [`ProofLut`], a 33^3 lattice of
//! exact solves read back with trilinear interpolation; the lattice is built
//! once per process.

use std::sync::OnceLock;

/// sRGB approximations of process cyan, magenta and yellow ink, in that order.
pub const INK_RGB: [[u8; 3]; 3] = [[0, 174, 239], [236, 0, 140], [255, 242, 0]];

/// Grey component replacement: the fraction of the naive grey component
/// moved to black ink. `1.0` is maximum GCR (and full UCR).
pub const GCR: f64 = 1.0;

/// How far (the largest 8-bit channel difference) a colour's round trip
/// through CMYK may move it before [`is_out_of_gamut`] calls it unprintable.
/// Above the one-code rounding of an in-gamut colour with room to spare.
pub const GAMUT_THRESHOLD: u8 = 6;

/// Newton iterations for the chromatic solve. Converges in well under this
/// for every in-gamut colour; the count is fixed so the result is
/// deterministic.
const NEWTON_ITERATIONS: usize = 60;

/// A CMYK colour: coverage of cyan, magenta, yellow and black, each `0..=1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cmyk {
    pub c: f32,
    pub m: f32,
    pub y: f32,
    pub k: f32,
}

impl Cmyk {
    /// Each ink as a whole percentage, the way Info and the Color panel show it.
    pub fn percentages(self) -> [u8; 4] {
        [self.c, self.m, self.y, self.k].map(|v| (v.clamp(0.0, 1.0) * 100.0).round() as u8)
    }
}

fn absorption() -> [[f64; 3]; 3] {
    let mut a = [[0.0; 3]; 3];
    for (i, ink) in INK_RGB.iter().enumerate() {
        for (j, &v) in ink.iter().enumerate() {
            a[i][j] = 1.0 - f64::from(v) / 255.0;
        }
    }
    a
}

/// The model's chromatic part: what three ink coverages transmit, per channel.
fn transmit(a: &[[f64; 3]; 3], u: [f64; 3]) -> [f64; 3] {
    let mut out = [1.0; 3];
    for (j, o) in out.iter_mut().enumerate() {
        for i in 0..3 {
            *o *= 1.0 - u[i] * a[i][j];
        }
    }
    out
}

/// Solve a 3x3 linear system by Cramer's rule; `None` when singular.
fn solve3(m: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    if d.abs() < 1e-12 {
        return None;
    }
    let mut x = [0.0; 3];
    for (col, xc) in x.iter_mut().enumerate() {
        let mut mc = m;
        for row in 0..3 {
            mc[row][col] = b[row];
        }
        *xc = det(mc) / d;
    }
    Some(x)
}

/// Least-squares coverages for a chromatic `target`, and the residual left.
///
/// Projected Gauss-Newton with an active set: a coverage sitting on a bound
/// whose gradient pushes it further out is frozen for the step, and the
/// remaining ones take the (lightly damped) normal-equation step. An in-gamut
/// target is solved to rounding; an out-of-gamut one lands on the closest
/// printable colour the bounds allow.
fn solve_chromatic(a: &[[f64; 3]; 3], target: [f64; 3]) -> ([f64; 3], f64) {
    // Start from the ideal-filter answer, which is exact for ideal inks.
    let mut u = target.map(|v| (1.0 - v).clamp(0.0, 1.0));
    for _ in 0..NEWTON_ITERATIONS {
        let f = transmit(a, u);
        let r = [f[0] - target[0], f[1] - target[1], f[2] - target[2]];
        // d out_j / d u_i = -a_ij * product over l != i of (1 - u_l a_lj).
        let mut jac = [[0.0; 3]; 3];
        for (j, row) in jac.iter_mut().enumerate() {
            for (i, cell) in row.iter_mut().enumerate() {
                let mut p = -a[i][j];
                for l in 0..3 {
                    if l != i {
                        p *= 1.0 - u[l] * a[l][j];
                    }
                }
                *cell = p;
            }
        }
        // Gradient of 0.5 * |r|^2.
        let grad: [f64; 3] = std::array::from_fn(|i| (0..3).map(|j| jac[j][i] * r[j]).sum());
        let free: [bool; 3] = std::array::from_fn(|i| {
            !((u[i] <= 0.0 && grad[i] > 0.0) || (u[i] >= 1.0 && grad[i] < 0.0))
        });
        // Normal equations over the free coverages; frozen ones get an
        // identity row and a zero right-hand side.
        let mut n = [[0.0; 3]; 3];
        let mut rhs = [0.0; 3];
        for i in 0..3 {
            if !free[i] {
                n[i][i] = 1.0;
                continue;
            }
            for l in 0..3 {
                if free[l] {
                    n[i][l] = (0..3).map(|j| jac[j][i] * jac[j][l]).sum();
                }
            }
            n[i][i] += 1e-12;
            rhs[i] = grad[i];
        }
        let Some(step) = solve3(n, rhs) else {
            break;
        };
        let mut moved = 0.0f64;
        for i in 0..3 {
            let next = (u[i] - step[i]).clamp(0.0, 1.0);
            moved = moved.max((next - u[i]).abs());
            u[i] = next;
        }
        if moved < 1e-12 {
            break;
        }
    }
    let f = transmit(a, u);
    let residual = (0..3)
        .map(|j| (f[j] - target[j]).abs())
        .fold(0.0f64, f64::max);
    (u, residual)
}

/// Residual below which a chromatic solve counts as exact (a tenth of an
/// 8-bit code).
const EXACT: f64 = 0.1 / 255.0;

/// Bisection steps for the black-generation search.
const K_STEPS: usize = 24;

/// Separate an 8-bit sRGB colour into CMYK (see the module header).
///
/// Black generation: the naive grey component `GCR * min(1-r, 1-g, 1-b)` is
/// the most black this colour may take. Real inks are not ideal filters, so
/// the chromatic remainder at that much black is not always printable; the
/// black is then lowered (bisection) to the most that still leaves an exact
/// chromatic solve, and to none when even that is out of gamut.
pub fn rgb8_to_cmyk(rgb: [u8; 3]) -> Cmyk {
    let t = rgb.map(|v| f64::from(v) / 255.0);
    let naive = [1.0 - t[0], 1.0 - t[1], 1.0 - t[2]];
    let k_max = GCR * naive[0].min(naive[1]).min(naive[2]);
    if k_max >= 1.0 - 1e-9 {
        return Cmyk {
            c: 0.0,
            m: 0.0,
            y: 0.0,
            k: 1.0,
        };
    }
    let a = absorption();
    let target_at = |k: f64| t.map(|v| (v / (1.0 - k)).clamp(0.0, 1.0));
    let (mut u, residual) = solve_chromatic(&a, target_at(k_max));
    let mut k = k_max;
    if residual > EXACT {
        let (u0, r0) = solve_chromatic(&a, target_at(0.0));
        u = u0;
        k = 0.0;
        if r0 <= EXACT {
            // Exact at no black, not at full black: the most black that stays
            // exact lies between.
            let (mut lo, mut hi) = (0.0, k_max);
            for _ in 0..K_STEPS {
                let mid = 0.5 * (lo + hi);
                let (um, rm) = solve_chromatic(&a, target_at(mid));
                if rm <= EXACT {
                    lo = mid;
                    u = um;
                    k = mid;
                } else {
                    hi = mid;
                }
            }
        }
    }
    Cmyk {
        c: u[0] as f32,
        m: u[1] as f32,
        y: u[2] as f32,
        k: k as f32,
    }
}

/// Compose a CMYK colour back into 8-bit sRGB (the model, rounded).
pub fn cmyk_to_rgb8(cmyk: Cmyk) -> [u8; 3] {
    let a = absorption();
    let u = [cmyk.c, cmyk.m, cmyk.y].map(|v| f64::from(v.clamp(0.0, 1.0)));
    let k = f64::from(cmyk.k.clamp(0.0, 1.0));
    transmit(&a, u).map(|v| ((1.0 - k) * v * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// The exact round trip `rgb -> cmyk -> rgb`: what the colour looks like once
/// it is printable.
pub fn proof_rgb8(rgb: [u8; 3]) -> [u8; 3] {
    cmyk_to_rgb8(rgb8_to_cmyk(rgb))
}

/// The largest channel difference between two colours.
pub fn max_channel_delta(a: [u8; 3], b: [u8; 3]) -> u8 {
    (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0)
}

/// Whether the round trip moves the colour by more than [`GAMUT_THRESHOLD`].
pub fn is_out_of_gamut(rgb: [u8; 3]) -> bool {
    max_channel_delta(rgb, ProofLut::shared().proof(rgb)) > GAMUT_THRESHOLD
}

/// Lattice points per axis of [`ProofLut`].
const LUT_SIDE: usize = 33;

/// The round trip sampled on a 33^3 lattice, read with trilinear
/// interpolation. Built once per process ([`ProofLut::shared`]).
pub struct ProofLut {
    nodes: Vec<[f32; 3]>,
}

impl ProofLut {
    fn build() -> Self {
        let mut nodes = Vec::with_capacity(LUT_SIDE * LUT_SIDE * LUT_SIDE);
        for r in 0..LUT_SIDE {
            for g in 0..LUT_SIDE {
                for b in 0..LUT_SIDE {
                    let at = |i: usize| ((i * 255) as f64 / (LUT_SIDE - 1) as f64).round() as u8;
                    let out = proof_rgb8([at(r), at(g), at(b)]);
                    nodes.push(out.map(f32::from));
                }
            }
        }
        Self { nodes }
    }

    /// The process-wide lattice.
    pub fn shared() -> &'static ProofLut {
        static LUT: OnceLock<ProofLut> = OnceLock::new();
        LUT.get_or_init(ProofLut::build)
    }

    fn node(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.nodes[(r * LUT_SIDE + g) * LUT_SIDE + b]
    }

    /// The proofed colour, interpolated from the lattice.
    pub fn proof(&self, rgb: [u8; 3]) -> [u8; 3] {
        let scale = (LUT_SIDE - 1) as f32 / 255.0;
        let pos = rgb.map(|v| f32::from(v) * scale);
        let lo = pos.map(|p| (p.floor() as usize).min(LUT_SIDE - 2));
        let f = [
            pos[0] - lo[0] as f32,
            pos[1] - lo[1] as f32,
            pos[2] - lo[2] as f32,
        ];
        let mut out = [0.0f32; 3];
        for dr in 0..2 {
            for dg in 0..2 {
                for db in 0..2 {
                    let w = (if dr == 1 { f[0] } else { 1.0 - f[0] })
                        * (if dg == 1 { f[1] } else { 1.0 - f[1] })
                        * (if db == 1 { f[2] } else { 1.0 - f[2] });
                    if w == 0.0 {
                        continue;
                    }
                    let n = self.node(lo[0] + dr, lo[1] + dg, lo[2] + db);
                    for c in 0..3 {
                        out[c] += w * n[c];
                    }
                }
            }
        }
        out.map(|v| v.round().clamp(0.0, 255.0) as u8)
    }

    /// Proof every pixel of a straight RGBA8 buffer in place (alpha kept).
    pub fn proof_rgba8(&self, rgba: &mut [u8]) {
        for px in rgba.as_chunks_mut::<4>().0 {
            let out = self.proof([px[0], px[1], px[2]]);
            px[..3].copy_from_slice(&out);
        }
    }

    /// Paint `warning` over every pixel whose round trip moves it by more
    /// than [`GAMUT_THRESHOLD`] (alpha kept). Returns how many were marked.
    pub fn mark_out_of_gamut(&self, rgba: &mut [u8], warning: [u8; 3]) -> usize {
        let mut marked = 0;
        for px in rgba.as_chunks_mut::<4>().0 {
            if px[3] == 0 {
                continue;
            }
            let rgb = [px[0], px[1], px[2]];
            if max_channel_delta(rgb, self.proof(rgb)) > GAMUT_THRESHOLD {
                px[..3].copy_from_slice(&warning);
                marked += 1;
            }
        }
        marked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primaries_round_trip_to_the_documented_values() {
        let table: [([u8; 3], [u8; 4], [u8; 3]); 7] = [
            ([255, 0, 0], [0, 99, 100, 0], [236, 1, 0]),
            ([0, 255, 0], [90, 0, 99, 0], [25, 173, 3]),
            ([0, 0, 255], [100, 68, 0, 0], [0, 55, 166]),
            ([0, 255, 255], [91, 0, 0, 0], [24, 182, 241]),
            ([255, 0, 255], [0, 83, 0, 0], [239, 44, 160]),
            ([255, 255, 0], [0, 0, 100, 0], [255, 242, 1]),
            ([128, 128, 128], [0, 0, 0, 50], [128, 128, 128]),
        ];
        for (rgb, inks, back) in table {
            let cmyk = rgb8_to_cmyk(rgb);
            assert_eq!(cmyk.percentages(), inks, "separation of {rgb:?}");
            assert_eq!(proof_rgb8(rgb), back, "round trip of {rgb:?}");
        }
    }

    #[test]
    fn white_and_black_are_paper_and_black_ink() {
        assert_eq!(rgb8_to_cmyk([255; 3]).percentages(), [0, 0, 0, 0]);
        assert_eq!(rgb8_to_cmyk([0; 3]).percentages(), [0, 0, 0, 100]);
        assert_eq!(proof_rgb8([255; 3]), [255; 3]);
        assert_eq!(proof_rgb8([0; 3]), [0; 3]);
    }

    #[test]
    fn every_neutral_round_trips_exactly() {
        for v in 0..=255u8 {
            assert_eq!(proof_rgb8([v; 3]), [v; 3], "grey {v}");
        }
    }

    #[test]
    fn an_in_gamut_colour_round_trips_within_a_code() {
        // Printable by construction: compose it from coverages first.
        let inked = cmyk_to_rgb8(Cmyk {
            c: 0.3,
            m: 0.5,
            y: 0.2,
            k: 0.1,
        });
        assert!(max_channel_delta(inked, proof_rgb8(inked)) <= 1);
        assert!(!is_out_of_gamut(inked));
    }

    #[test]
    fn the_separation_is_deterministic() {
        for rgb in [[12u8, 200, 77], [250, 3, 90], [40, 40, 41]] {
            assert_eq!(rgb8_to_cmyk(rgb), rgb8_to_cmyk(rgb));
        }
    }

    #[test]
    fn gamut_warning_marks_saturated_green_but_not_grey() {
        assert!(is_out_of_gamut([0, 255, 0]));
        assert!(!is_out_of_gamut([128, 128, 128]));
        let mut rgba = vec![0, 255, 0, 255, 128, 128, 128, 255, 0, 255, 0, 0];
        let marked = ProofLut::shared().mark_out_of_gamut(&mut rgba, [255, 0, 255]);
        assert_eq!(marked, 1);
        assert_eq!(&rgba[0..4], &[255, 0, 255, 255], "green is marked");
        assert_eq!(&rgba[4..8], &[128, 128, 128, 255], "grey is not");
        assert_eq!(&rgba[8..12], &[0, 255, 0, 0], "transparent is left alone");
    }

    #[test]
    fn the_lattice_agrees_with_the_exact_round_trip() {
        let lut = ProofLut::shared();
        for rgb in [[0u8, 255, 0], [128, 128, 128], [200, 100, 50], [10, 20, 30]] {
            let exact = proof_rgb8(rgb);
            let fast = lut.proof(rgb);
            assert!(
                max_channel_delta(exact, fast) <= 3,
                "{rgb:?}: exact {exact:?} vs lattice {fast:?}"
            );
        }
    }
}
