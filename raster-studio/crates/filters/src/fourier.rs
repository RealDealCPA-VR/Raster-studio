//! W13-J: Filter ▸ Fourier ▸ Fourier Transform and Inverse Fourier Transform.
//!
//! [`fourier_transform`] replaces each colour channel with its 2-D discrete
//! Fourier spectrum written as an *editable image*: log magnitude on the left,
//! phase on the right. Paint on it — darken a frequency's magnitude to remove
//! a periodic pattern, say — and [`inverse_fourier_transform`] turns the
//! edited spectrum back into pixels.
//!
//! # The FFT
//!
//! A pure-Rust complex FFT of this module's own, in `f64`: iterative radix-2
//! for power-of-two lengths and Bluestein's chirp-z algorithm (three radix-2
//! transforms of the next power of two `>= 2n - 1`) for every other length, so
//! the image is transformed at its own size with no padding. No external crate
//! is involved.
//!
//! # The spectrum image, exactly
//!
//! The input is real, so its spectrum is Hermitian — `X[k, l]` is the
//! conjugate of `X[-k, -l]` — and exactly `W * H` real numbers describe it.
//! They are laid out in the `W x H` image as follows (column `x`, row `y`,
//! before the display shift):
//!
//! * **Magnitude block**, columns `0 ..= W/2`: column `k` holds `|X[k, l]|` at
//!   row `l`.
//! * **Phase block**, columns `W/2 + 1 .. W`: column `W/2 + k` holds
//!   `arg X[k, l]` at row `l`, for `k` in `1 ..= (W - 1)/2`.
//! * Column `0` (and column `W/2` when `W` is even) is its own mirror, so it
//!   holds both halves itself: magnitude at rows `1 ..= (H-1)/2`, the matching
//!   phase at the mirrored row `H - l`, and the self-conjugate bins (rows `0`
//!   and, for even `H`, `H/2`) — which are real numbers — stored *signed*
//!   around mid-grey.
//! * Every row is then shifted down by `H/2`, so the lowest vertical
//!   frequencies sit in the middle of the image rather than split between the
//!   top and bottom edges.
//!
//! Magnitudes are encoded as `ln(1 + |X|) / ln(1 + W*H)` — the largest
//! magnitude a `[0, 1]` image can have is `W * H`, so the scale depends only
//! on the image size and the inverse needs nothing but the pixels. Phases are
//! `(theta + pi) / 2pi`. Signed bins are `0.5 + 0.5 * sign * log magnitude`.
//!
//! # Colour, alpha and precision
//!
//! The transform works on the **gamma-encoded** channel values (the numbers a
//! file stores), and the spectrum is written back as encoded values, so an
//! 8-bit spectrum's bytes *are* the encoded magnitudes and phases. A
//! translucent layer is transformed as its colour over black; the spectrum is
//! opaque, and so is the inverse's output.
//!
//! On the float pipeline — the filter itself, the dialog preview and a smart
//! filter stack — forward then inverse restores the image to well within
//! 1/255 (`fourier_then_inverse_restores_the_image`), and so does a 16-bit
//! document through the menu (`app-shell`'s
//! `w13j_fourier_then_inverse_from_the_menu_restores_a_sixteen_bit_layer`).
//! An **8-bit** document stores the spectrum in 8 bits, and 256 levels of log
//! magnitude and phase cannot carry the whole image: through an 8-bit store
//! the round trip is off by several levels (on
//! `eight_bit_storage_loses_the_round_trip`'s picture, 7/255 at worst). How
//! lossy Photopea's own 8-bit round trip is has not been measured. The app
//! says so when Fourier Transform runs on an 8-bit document: convert to 16
//! bits first for a round trip within 1/255.

use color::{linear_to_srgb, srgb_to_linear};
use rayon::prelude::*;

use crate::buffer::FilterBuffer;

use core::f64::consts::PI;

/// A complex number in `f64`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Complex {
    re: f64,
    im: f64,
}

impl Complex {
    const ZERO: Complex = Complex { re: 0.0, im: 0.0 };

    fn new(re: f64, im: f64) -> Self {
        Complex { re, im }
    }

    /// `e^{i theta}`.
    fn cis(theta: f64) -> Self {
        Complex::new(theta.cos(), theta.sin())
    }

    fn polar(r: f64, theta: f64) -> Self {
        Complex::new(r * theta.cos(), r * theta.sin())
    }

    fn conj(self) -> Self {
        Complex::new(self.re, -self.im)
    }

    fn add(self, o: Complex) -> Self {
        Complex::new(self.re + o.re, self.im + o.im)
    }

    fn sub(self, o: Complex) -> Self {
        Complex::new(self.re - o.re, self.im - o.im)
    }

    fn mul(self, o: Complex) -> Self {
        Complex::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }

    fn scale(self, s: f64) -> Self {
        Complex::new(self.re * s, self.im * s)
    }

    fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }

    fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
}

/// A forward, unnormalised DFT of one fixed length:
/// `X[k] = sum_j x[j] e^{-2 pi i jk / n}`.
enum Fft {
    /// Lengths 0 and 1: the identity.
    Trivial,
    /// Power-of-two lengths. `twiddles[k] = e^{-2 pi i k / n}`, `k < n/2`.
    Radix2 { twiddles: Vec<Complex> },
    /// Every other length, as a circular convolution of length `m`.
    Bluestein {
        n: usize,
        /// `chirp[k] = e^{i pi k^2 / n}`.
        chirp: Vec<Complex>,
        /// The forward transform of the chirp kernel, length `m`.
        kernel: Vec<Complex>,
        inner: Box<Fft>,
    },
}

impl Fft {
    fn new(n: usize) -> Self {
        if n <= 1 {
            return Fft::Trivial;
        }
        if n.is_power_of_two() {
            let twiddles = (0..n / 2)
                .map(|k| Complex::cis(-2.0 * PI * k as f64 / n as f64))
                .collect();
            return Fft::Radix2 { twiddles };
        }
        let m = (2 * n - 1).next_power_of_two();
        // k^2 mod 2n keeps the angle small, so it stays exact for large n.
        let two_n = 2 * n as u128;
        let chirp: Vec<Complex> = (0..n)
            .map(|k| {
                let k = k as u128;
                let r = (k * k) % two_n;
                Complex::cis(PI * r as f64 / n as f64)
            })
            .collect();
        let mut kernel = vec![Complex::ZERO; m];
        kernel[0] = chirp[0];
        for k in 1..n {
            kernel[k] = chirp[k];
            kernel[m - k] = chirp[k];
        }
        let inner = Fft::new(m);
        inner.forward(&mut kernel);
        Fft::Bluestein {
            n,
            chirp,
            kernel,
            inner: Box::new(inner),
        }
    }

    /// Transform `buf` in place (forward, unnormalised).
    fn forward(&self, buf: &mut [Complex]) {
        match self {
            Fft::Trivial => {}
            Fft::Radix2 { twiddles } => radix2(buf, twiddles),
            Fft::Bluestein {
                n,
                chirp,
                kernel,
                inner,
            } => {
                let m = kernel.len();
                let mut a = vec![Complex::ZERO; m];
                for j in 0..*n {
                    a[j] = buf[j].mul(chirp[j].conj());
                }
                inner.forward(&mut a);
                for (v, k) in a.iter_mut().zip(kernel) {
                    *v = v.mul(*k);
                }
                // Inverse of length m through the conjugation identity.
                for v in a.iter_mut() {
                    *v = v.conj();
                }
                inner.forward(&mut a);
                let inv_m = 1.0 / m as f64;
                for k in 0..*n {
                    buf[k] = a[k].conj().scale(inv_m).mul(chirp[k].conj());
                }
            }
        }
    }

    /// The unnormalised inverse: `conj(DFT(conj(x)))`.
    fn inverse(&self, buf: &mut [Complex]) {
        for v in buf.iter_mut() {
            *v = v.conj();
        }
        self.forward(buf);
        for v in buf.iter_mut() {
            *v = v.conj();
        }
    }
}

fn radix2(buf: &mut [Complex], twiddles: &[Complex]) {
    let n = buf.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            buf.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let step = n / len;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let w = twiddles[k * step];
                let u = buf[start + k];
                let v = buf[start + k + half].mul(w);
                buf[start + k] = u.add(v);
                buf[start + k + half] = u.sub(v);
            }
        }
        len <<= 1;
    }
}

fn transpose(src: &[Complex], w: usize, h: usize) -> Vec<Complex> {
    let mut out = vec![Complex::ZERO; w * h];
    out.par_chunks_mut(h.max(1))
        .enumerate()
        .for_each(|(x, col)| {
            for (y, v) in col.iter_mut().enumerate() {
                *v = src[y * w + x];
            }
        });
    out
}

/// 2-D transform of a row-major `w x h` grid, in place.
fn fft2(data: &mut Vec<Complex>, w: usize, h: usize, inverse: bool) {
    let run = |plan: &Fft, row: &mut [Complex]| {
        if inverse {
            plan.inverse(row);
        } else {
            plan.forward(row);
        }
    };
    let rows = Fft::new(w);
    data.par_chunks_mut(w).for_each(|row| run(&rows, row));
    let mut t = transpose(data, w, h);
    let cols = Fft::new(h);
    t.par_chunks_mut(h).for_each(|col| run(&cols, col));
    *data = transpose(&t, h, w);
}

/// The fixed geometry of one image size's spectrum layout.
#[derive(Clone, Copy)]
struct Layout {
    w: usize,
    h: usize,
    /// `ln(1 + W*H)`, the log-magnitude scale.
    log_scale: f64,
}

impl Layout {
    fn new(w: usize, h: usize) -> Self {
        Layout {
            w,
            h,
            log_scale: (1.0 + (w * h) as f64).ln(),
        }
    }

    /// Index of spectrum-layout cell (`col`, `row`) in the displayed image.
    fn cell(&self, col: usize, row: usize) -> usize {
        ((row + self.h / 2) % self.h) * self.w + col
    }

    fn self_mirror(&self, k: usize) -> bool {
        k == 0 || (self.w.is_multiple_of(2) && k == self.w / 2)
    }

    fn enc_mag(&self, m: f64) -> f32 {
        ((1.0 + m.max(0.0)).ln() / self.log_scale).clamp(0.0, 1.0) as f32
    }

    fn dec_mag(&self, e: f32) -> f64 {
        (f64::from(e.clamp(0.0, 1.0)) * self.log_scale).exp() - 1.0
    }

    fn enc_signed(&self, v: f64) -> f32 {
        let l = f64::from(self.enc_mag(v.abs()));
        (0.5 + 0.5 * v.signum() * l).clamp(0.0, 1.0) as f32
    }

    fn dec_signed(&self, e: f32) -> f64 {
        let s = (f64::from(e.clamp(0.0, 1.0)) - 0.5) * 2.0;
        s.signum() * ((s.abs() * self.log_scale).exp() - 1.0)
    }

    /// Write a spectrum as the encoded image plane.
    fn encode(&self, x: &[Complex]) -> Vec<f32> {
        let (w, h) = (self.w, self.h);
        let half = w / 2;
        let mut out = vec![0.0f32; w * h];
        for k in 0..=half {
            if !self.self_mirror(k) {
                for l in 0..h {
                    let z = x[l * w + k];
                    out[self.cell(k, l)] = self.enc_mag(z.abs());
                    out[self.cell(half + k, l)] = enc_phase(z.arg());
                }
                continue;
            }
            for l in 0..h {
                let ml = (h - l) % h;
                let z = x[l * w + k];
                if ml == l {
                    out[self.cell(k, l)] = self.enc_signed(z.re);
                } else if l < ml {
                    out[self.cell(k, l)] = self.enc_mag(z.abs());
                    out[self.cell(k, ml)] = enc_phase(z.arg());
                }
            }
        }
        out
    }

    /// Read the encoded image plane back into the full Hermitian spectrum.
    fn decode(&self, e: &[f32]) -> Vec<Complex> {
        let (w, h) = (self.w, self.h);
        let half = w / 2;
        let mut x = vec![Complex::ZERO; w * h];
        for k in 0..=half {
            if !self.self_mirror(k) {
                for l in 0..h {
                    let z = Complex::polar(
                        self.dec_mag(e[self.cell(k, l)]),
                        dec_phase(e[self.cell(half + k, l)]),
                    );
                    x[l * w + k] = z;
                    x[((h - l) % h) * w + (w - k)] = z.conj();
                }
                continue;
            }
            for l in 0..h {
                let ml = (h - l) % h;
                if ml == l {
                    x[l * w + k] = Complex::new(self.dec_signed(e[self.cell(k, l)]), 0.0);
                } else if l < ml {
                    let z = Complex::polar(
                        self.dec_mag(e[self.cell(k, l)]),
                        dec_phase(e[self.cell(k, ml)]),
                    );
                    x[l * w + k] = z;
                    x[ml * w + k] = z.conj();
                }
            }
        }
        x
    }
}

fn enc_phase(theta: f64) -> f32 {
    ((theta + PI) / (2.0 * PI)).clamp(0.0, 1.0) as f32
}

fn dec_phase(e: f32) -> f64 {
    f64::from(e.clamp(0.0, 1.0)) * 2.0 * PI - PI
}

/// One channel's encoded values: the premultiplied colour (the colour over
/// black), gamma encoded and clamped to `[0, 1]`.
fn channel(src: &FilterBuffer, c: usize) -> Vec<f64> {
    src.pixels()
        .iter()
        .map(|p| f64::from(linear_to_srgb(p[c].clamp(0.0, 1.0))))
        .collect()
}

/// Assemble three encoded planes into an opaque linear buffer.
fn opaque_from_planes(w: u32, h: u32, planes: [Vec<f32>; 3]) -> FilterBuffer {
    let px = (0..planes[0].len())
        .map(|i| {
            [
                srgb_to_linear(planes[0][i]),
                srgb_to_linear(planes[1][i]),
                srgb_to_linear(planes[2][i]),
                1.0,
            ]
        })
        .collect();
    FilterBuffer::from_pixels(w, h, px).expect("same size as the source")
}

/// Filter ▸ Fourier ▸ Fourier Transform: each colour channel becomes its
/// spectrum image (see the module documentation for the exact layout).
pub fn fourier_transform(src: &FilterBuffer) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let layout = Layout::new(w as usize, h as usize);
    let planes = [0, 1, 2].map(|c| {
        let mut data: Vec<Complex> = channel(src, c)
            .into_iter()
            .map(|v| Complex::new(v, 0.0))
            .collect();
        fft2(&mut data, w as usize, h as usize, false);
        layout.encode(&data)
    });
    opaque_from_planes(w, h, planes)
}

/// Filter ▸ Fourier ▸ Inverse Fourier Transform: read a spectrum image made by
/// [`fourier_transform`] (edited or not) back into pixels.
pub fn inverse_fourier_transform(src: &FilterBuffer) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let layout = Layout::new(w as usize, h as usize);
    let n = f64::from(w) * f64::from(h);
    let planes = [0, 1, 2].map(|c| {
        let plane: Vec<f32> = channel(src, c).into_iter().map(|v| v as f32).collect();
        let mut data = layout.decode(&plane);
        fft2(&mut data, w as usize, h as usize, true);
        data.iter()
            .map(|z| (z.re / n).clamp(0.0, 1.0) as f32)
            .collect()
    });
    opaque_from_planes(w, h, planes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_dft(x: &[Complex]) -> Vec<Complex> {
        let n = x.len();
        (0..n)
            .map(|k| {
                x.iter().enumerate().fold(Complex::ZERO, |acc, (j, v)| {
                    let a = -2.0 * PI * (j * k) as f64 / n as f64;
                    acc.add(v.mul(Complex::cis(a)))
                })
            })
            .collect()
    }

    #[test]
    fn the_fft_matches_the_textbook_dft_at_every_length() {
        for n in [1usize, 2, 3, 5, 7, 8, 12, 16, 17, 31, 48] {
            let x: Vec<Complex> = (0..n)
                .map(|i| Complex::new((i as f64 * 0.37).sin(), (i as f64 * 0.11).cos()))
                .collect();
            let want = naive_dft(&x);
            let mut got = x.clone();
            Fft::new(n).forward(&mut got);
            for (a, b) in got.iter().zip(&want) {
                assert!(a.sub(*b).abs() < 1e-9, "n = {n}: {a:?} vs {b:?}");
            }
            Fft::new(n).inverse(&mut got);
            for (a, b) in got.iter().zip(&x) {
                assert!(a.scale(1.0 / n as f64).sub(*b).abs() < 1e-9, "n = {n}");
            }
        }
    }

    fn picture(w: u32, h: u32) -> FilterBuffer {
        let px = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let r = ((x * 7 + y * 3) % 23) as f32 / 22.0;
                let g = if (x / 3 + y / 2) % 2 == 0 { 0.9 } else { 0.1 };
                let b = (x as f32 / w as f32) * (y as f32 / h as f32);
                [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), 1.0]
            })
            .collect();
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn fourier_then_inverse_restores_the_image() {
        // Even and odd sides in both directions, and the degenerate strips,
        // so every self-mirror column and row of the layout is exercised.
        for (w, h) in [(32, 24), (17, 9), (16, 15), (1, 1), (1, 7), (6, 1), (2, 2)] {
            let src = picture(w, h);
            let spectrum = fourier_transform(&src);
            assert_eq!(spectrum.dimensions(), (w, h));
            assert_ne!(spectrum, src, "{w}x{h}: the spectrum is the image");
            let back = inverse_fourier_transform(&spectrum);
            let (a, b) = (src.to_rgba8(), back.to_rgba8());
            for (i, (p, q)) in a.iter().zip(&b).enumerate() {
                assert!(
                    (i32::from(*p) - i32::from(*q)).abs() <= 1,
                    "{w}x{h} byte {i}: {p} vs {q}"
                );
            }
            // And in the float domain, far inside one 8-bit level.
            for (p, q) in src.pixels().iter().zip(back.pixels()) {
                for c in 0..3 {
                    let d = (linear_to_srgb(p[c]) - linear_to_srgb(q[c])).abs();
                    assert!(d < 1e-4, "{w}x{h}: {p:?} vs {q:?}");
                }
            }
        }
    }

    /// Why the app warns on an 8-bit document: a spectrum stored in 8 bits
    /// does not carry the image back within 1/255. Measured, not assumed.
    #[test]
    fn eight_bit_storage_loses_the_round_trip() {
        let src = picture(64, 64);
        let stored = FilterBuffer::from_rgba8(64, 64, &fourier_transform(&src).to_rgba8()).unwrap();
        let back = inverse_fourier_transform(&stored);
        let worst = src
            .to_rgba8()
            .iter()
            .zip(back.to_rgba8())
            .map(|(p, q)| (i32::from(*p) - i32::from(q)).abs())
            .max()
            .unwrap();
        assert_eq!(worst, 7, "worst channel error through an 8-bit spectrum");
    }

    #[test]
    fn a_flat_image_has_only_a_dc_term() {
        // Encoded 0.5 everywhere: X[0,0] = 0.5 * N and every other bin is 0.
        let (w, h) = (8u32, 6u32);
        let src = FilterBuffer::filled(w, h, [srgb_to_linear(0.5), 0.0, 0.0, 1.0]).unwrap();
        let spectrum = fourier_transform(&src);
        let n = f64::from(w * h);
        let dc = 0.5 + 0.5 * ((1.0 + 0.5 * n).ln() / (1.0 + n).ln());
        let enc = |x: u32, y: u32, c: usize| linear_to_srgb(spectrum.get(x, y)[c]);
        // DC lives at column 0, row 0 of the layout: displayed at row h/2.
        assert!((f64::from(enc(0, h / 2, 0)) - dc).abs() < 1e-5);
        // Every magnitude column is black elsewhere.
        for y in 0..h {
            let layout_row = (y + h - h / 2) % h;
            for x in 1..=w / 2 {
                if x == w / 2 && (layout_row == 0 || layout_row == h / 2) {
                    // Signed real bins of the Nyquist column: zero is mid-grey.
                    assert!((enc(x, y, 0) - 0.5).abs() < 1e-5);
                    continue;
                }
                if x == w / 2 && layout_row > h / 2 {
                    continue; // phase half of the self-mirror column
                }
                assert!(enc(x, y, 0) < 1e-5, "({x}, {y}) = {}", enc(x, y, 0));
            }
        }
        // A black channel is a spectrum of zeros: black magnitudes, mid-grey
        // signed bins.
        assert!(enc(1, 0, 1) < 1e-6);
        assert!((enc(0, h / 2, 1) - 0.5).abs() < 1e-6);
        assert!(spectrum.pixels().iter().all(|p| p[3] == 1.0));
    }

    #[test]
    fn editing_the_spectrum_filters_the_image() {
        // Zero every magnitude except the DC term: the inverse is the mean.
        let src = picture(12, 10);
        let mut spectrum = fourier_transform(&src);
        let (w, h) = spectrum.dimensions();
        let keep = (0, h / 2);
        for y in 0..h {
            for x in 0..=w / 2 {
                let self_mirror = x == 0 || x == w / 2;
                let layout_row = (y + h - h / 2) % h;
                let is_phase = self_mirror && layout_row > h / 2;
                let is_signed = self_mirror && (layout_row == 0 || layout_row == h / 2);
                if (x, y) == keep || is_phase {
                    continue;
                }
                let v = if is_signed { srgb_to_linear(0.5) } else { 0.0 };
                spectrum.set(x, y, [v, v, v, 1.0]);
            }
        }
        let back = inverse_fourier_transform(&spectrum);
        let mean: f32 = src
            .pixels()
            .iter()
            .map(|p| linear_to_srgb(p[1]))
            .sum::<f32>()
            / (w * h) as f32;
        for p in back.pixels() {
            assert!((linear_to_srgb(p[1]) - mean).abs() < 1e-3);
        }
    }

    #[test]
    fn empty_input_is_returned_unchanged() {
        let empty = FilterBuffer::transparent(0, 0).unwrap();
        assert!(fourier_transform(&empty).is_empty());
        assert!(inverse_fourier_transform(&empty).is_empty());
    }
}
