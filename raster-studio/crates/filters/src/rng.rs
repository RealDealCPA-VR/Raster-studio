//! Deterministic pseudo-randomness and gradient noise.
//!
//! Every random-looking filter in this crate is a pure function of its seed
//! and the destination coordinate. That is not a stylistic preference: the
//! filters run in parallel over tile bands, so a sequential random *stream*
//! would give a different image depending on how rayon happened to schedule
//! the bands. [`hash2`] and [`Rng::at`] make the value at a pixel depend only
//! on `(seed, x, y)`, which is both reproducible in tests and identical
//! whatever the thread count.
//!
//! The generator is SplitMix64 and the gradient noise is a hand-written
//! Perlin; nothing here is ported from another project.

/// SplitMix64. Small, fast, and good enough for image dither and jitter —
/// it is not a cryptographic generator and is not used as one.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

impl Rng {
    /// Seed the generator directly.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Seed from a coordinate, so a filter's value at `(x, y)` is independent
    /// of evaluation order.
    pub fn at(seed: u64, x: i64, y: i64) -> Self {
        Self::new(hash2(seed, x, y))
    }

    /// Next raw 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`. Built from 24 bits so the result is exactly
    /// representable and never rounds up to `1.0`.
    pub fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) * (1.0 / 16_777_216.0)
    }

    /// Uniform in `[-1, 1)`.
    pub fn next_signed(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }

    /// Standard normal, via the polar Box-Muller transform.
    ///
    /// The rejection loop is bounded: after 16 tries it falls back to a
    /// triangular approximation, so a pathological seed cannot hang a filter.
    pub fn next_gaussian(&mut self) -> f32 {
        for _ in 0..16 {
            let u = self.next_f32() * 2.0 - 1.0;
            let v = self.next_f32() * 2.0 - 1.0;
            let s = u * u + v * v;
            if s > 0.0 && s < 1.0 {
                let f = (-2.0 * s.ln() / s).sqrt();
                return u * f;
            }
        }
        (self.next_f32() + self.next_f32() + self.next_f32() - 1.5) * 2.0
    }
}

/// Hash a seed and a 2D coordinate into 64 well-mixed bits.
pub fn hash2(seed: u64, x: i64, y: i64) -> u64 {
    let mut z = seed
        .wrapping_add((x as u64).wrapping_mul(0xA24B_AED4_963E_E407))
        .wrapping_add((y as u64).wrapping_mul(0x9FB2_1C65_1E98_DF25))
        .wrapping_add(GOLDEN);
    z = (z ^ (z >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 32)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 29)
}

/// Uniform `[0, 1)` value for a coordinate, without constructing an [`Rng`].
#[inline]
pub fn hash_unit(seed: u64, x: i64, y: i64) -> f32 {
    ((hash2(seed, x, y) >> 40) as f32) * (1.0 / 16_777_216.0)
}

/// Seeded 2D Perlin gradient noise.
///
/// A classic implementation: a seeded permutation of `0..256`, eight
/// unit-ish gradient directions, and the quintic fade `6t^5 - 15t^4 + 10t^3`
/// so the second derivative is continuous across cell boundaries (the cubic
/// fade leaves visible creases at integer coordinates).
///
/// [`Perlin::noise`] returns values in `[-1, 1]`; [`Perlin::fbm`] sums
/// octaves and renormalises so its range is also `[-1, 1]`.
#[derive(Debug, Clone)]
pub struct Perlin {
    perm: [u8; 256],
}

/// The eight gradient directions: the four axes and the four diagonals.
/// Diagonals are normalised so no direction has more reach than another,
/// which is what stops a Perlin field showing a faint square grid.
const GRADIENTS: [[f32; 2]; 8] = [
    [1.0, 0.0],
    [-1.0, 0.0],
    [0.0, 1.0],
    [0.0, -1.0],
    [
        core::f32::consts::FRAC_1_SQRT_2,
        core::f32::consts::FRAC_1_SQRT_2,
    ],
    [
        -core::f32::consts::FRAC_1_SQRT_2,
        core::f32::consts::FRAC_1_SQRT_2,
    ],
    [
        core::f32::consts::FRAC_1_SQRT_2,
        -core::f32::consts::FRAC_1_SQRT_2,
    ],
    [
        -core::f32::consts::FRAC_1_SQRT_2,
        -core::f32::consts::FRAC_1_SQRT_2,
    ],
];

impl Perlin {
    /// Build the permutation table by Fisher-Yates shuffling `0..256` with a
    /// seeded [`Rng`]. Same seed, same table, on every machine.
    pub fn new(seed: u64) -> Self {
        let mut perm = [0u8; 256];
        for (i, slot) in perm.iter_mut().enumerate() {
            *slot = i as u8;
        }
        let mut rng = Rng::new(seed ^ 0x5DEE_CE66_D125_1B68);
        for i in (1..256usize).rev() {
            let j = (rng.next_u64() % (i as u64 + 1)) as usize;
            perm.swap(i, j);
        }
        Self { perm }
    }

    #[inline]
    fn gradient(&self, xi: i32, yi: i32) -> [f32; 2] {
        let a = self.perm[(xi & 255) as usize];
        let b = self.perm[((yi & 255) as usize + a as usize) & 255];
        GRADIENTS[(b & 7) as usize]
    }

    /// Noise at a point, in `[-1, 1]`. The lattice has period 256.
    pub fn noise(&self, x: f32, y: f32) -> f32 {
        if !x.is_finite() || !y.is_finite() {
            return 0.0;
        }
        let x0f = x.floor();
        let y0f = y.floor();
        let xi = wrap_lattice(x0f);
        let yi = wrap_lattice(y0f);
        let fx = x - x0f;
        let fy = y - y0f;
        let u = fade(fx);
        let v = fade(fy);

        let dot = |gx: i32, gy: i32, dx: f32, dy: f32| {
            let g = self.gradient(gx, gy);
            g[0] * dx + g[1] * dy
        };
        let n00 = dot(xi, yi, fx, fy);
        let n10 = dot(xi + 1, yi, fx - 1.0, fy);
        let n01 = dot(xi, yi + 1, fx, fy - 1.0);
        let n11 = dot(xi + 1, yi + 1, fx - 1.0, fy - 1.0);

        let a = n00 + u * (n10 - n00);
        let b = n01 + u * (n11 - n01);
        // Gradient dot products of 2D Perlin span +-sqrt(2)/2; scale to +-1.
        ((a + v * (b - a)) * core::f32::consts::SQRT_2).clamp(-1.0, 1.0)
    }

    /// Fractal sum of `octaves` doublings, each at half the amplitude.
    ///
    /// Normalised by the total amplitude, so the range stays `[-1, 1]`
    /// whatever the octave count — that is what keeps a 1-octave and an
    /// 8-octave cloud at the same average brightness.
    pub fn fbm(&self, x: f32, y: f32, octaves: u32, persistence: f32) -> f32 {
        let octaves = octaves.clamp(1, 12);
        let p = if persistence.is_finite() {
            persistence.clamp(0.01, 1.0)
        } else {
            0.5
        };
        let mut sum = 0.0;
        let mut amp = 1.0;
        let mut total = 0.0;
        let mut freq = 1.0;
        for _ in 0..octaves {
            sum += self.noise(x * freq, y * freq) * amp;
            total += amp;
            amp *= p;
            freq *= 2.0;
        }
        if total > 0.0 {
            (sum / total).clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }
}

/// Reduce a floor'd coordinate to the lattice period without overflowing the
/// `f32 -> i32` cast for very large inputs.
#[inline]
fn wrap_lattice(v: f32) -> i32 {
    let m = v.rem_euclid(256.0);
    if m.is_finite() {
        m as i32
    } else {
        0
    }
}

#[inline]
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_stream() {
        let a: Vec<u64> = (0..8).map(|_| Rng::new(7).next_u64()).collect();
        let mut r = Rng::new(7);
        assert_eq!(r.next_u64(), a[0]);
        let mut b = Rng::new(7);
        let mut c = Rng::new(7);
        for _ in 0..32 {
            assert_eq!(b.next_u64(), c.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        assert_ne!(Rng::new(1).next_u64(), Rng::new(2).next_u64());
    }

    #[test]
    fn unit_floats_stay_in_range() {
        let mut r = Rng::new(0xDEAD_BEEF);
        let mut lo = 1.0f32;
        let mut hi = 0.0f32;
        for _ in 0..20_000 {
            let v = r.next_f32();
            assert!((0.0..1.0).contains(&v), "{v}");
            lo = lo.min(v);
            hi = hi.max(v);
        }
        // Sanity: the generator actually covers the range.
        assert!(lo < 0.01 && hi > 0.99, "{lo} {hi}");
    }

    #[test]
    fn uniform_mean_is_near_a_half() {
        let mut r = Rng::new(11);
        let mean: f64 = (0..50_000).map(|_| r.next_f32() as f64).sum::<f64>() / 50_000.0;
        assert!((mean - 0.5).abs() < 0.01, "{mean}");
    }

    #[test]
    fn gaussian_has_unit_variance_and_terminates() {
        let mut r = Rng::new(99);
        let n = 40_000;
        let vals: Vec<f64> = (0..n).map(|_| r.next_gaussian() as f64).collect();
        let mean = vals.iter().sum::<f64>() / n as f64;
        let var = vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n as f64;
        assert!(mean.abs() < 0.03, "mean {mean}");
        assert!((var - 1.0).abs() < 0.06, "var {var}");
    }

    #[test]
    fn coordinate_hash_is_order_independent_and_position_dependent() {
        assert_eq!(hash2(5, 3, 4), hash2(5, 3, 4));
        assert_ne!(hash2(5, 3, 4), hash2(5, 4, 3));
        assert_ne!(hash2(5, 3, 4), hash2(6, 3, 4));
        assert_ne!(hash2(5, 3, 4), hash2(5, 3, 5));
        let v = hash_unit(1, 2, 3);
        assert!((0.0..1.0).contains(&v));
    }

    #[test]
    fn hash_handles_negative_coordinates() {
        assert_ne!(hash2(0, -1, -1), hash2(0, 1, 1));
        assert!((0.0..1.0).contains(&hash_unit(0, i64::MIN, i64::MAX)));
    }

    #[test]
    fn perlin_is_zero_on_the_lattice() {
        let p = Perlin::new(3);
        for y in -4..4 {
            for x in -4..4 {
                let v = p.noise(x as f32, y as f32);
                assert!(v.abs() < 1e-5, "at {x},{y}: {v}");
            }
        }
    }

    #[test]
    fn perlin_is_deterministic_and_seed_dependent() {
        let a = Perlin::new(42);
        let b = Perlin::new(42);
        let c = Perlin::new(43);
        let mut differs = false;
        for i in 0..200 {
            let (x, y) = (i as f32 * 0.37, i as f32 * 0.11);
            assert_eq!(a.noise(x, y), b.noise(x, y));
            differs |= a.noise(x, y) != c.noise(x, y);
        }
        assert!(differs, "a different seed must give a different field");
    }

    #[test]
    fn perlin_stays_in_range_and_varies() {
        let p = Perlin::new(1234);
        let mut lo = 1.0f32;
        let mut hi = -1.0f32;
        for i in 0..500 {
            for j in 0..50 {
                let v = p.noise(i as f32 * 0.13, j as f32 * 0.29);
                assert!((-1.0..=1.0).contains(&v), "{v}");
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        assert!(hi - lo > 0.8, "field is too flat: {lo}..{hi}");
    }

    #[test]
    fn perlin_is_continuous_across_a_cell_boundary() {
        let p = Perlin::new(8);
        let e = 1e-3;
        for k in -3..3 {
            let x = k as f32;
            let a = p.noise(x - e, 0.37);
            let b = p.noise(x + e, 0.37);
            assert!((a - b).abs() < 1e-2, "seam at {x}: {a} vs {b}");
        }
    }

    #[test]
    fn fbm_is_bounded_for_every_octave_count() {
        let p = Perlin::new(5);
        for oct in 1..=10 {
            for i in 0..300 {
                let v = p.fbm(i as f32 * 0.07, i as f32 * 0.13, oct, 0.5);
                assert!((-1.0..=1.0).contains(&v), "octaves {oct}: {v}");
            }
        }
    }

    #[test]
    fn noise_rejects_non_finite_input_without_panicking() {
        let p = Perlin::new(0);
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(p.noise(v, 0.0), 0.0);
            assert_eq!(p.noise(0.0, v), 0.0);
        }
        // A magnitude past i32 range must wrap, not overflow the cast.
        assert!(p.noise(1e30, 1e30).is_finite());
        assert!(p.fbm(0.5, 0.5, 0, f32::NAN).is_finite());
    }
}
