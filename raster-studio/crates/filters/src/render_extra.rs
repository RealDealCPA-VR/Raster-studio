//! W13-J: Filter ▸ Other ▸ Particles.
//!
//! It renders light *onto* the layer and **screens** it over the pixels, so
//! it only ever brightens, and alpha rises to at least the rendered coverage.
//! The same settings always render the same picture.
//!
//! **Particles** is Photopea's renderer, ported from its code with its
//! controls (the `Part` descriptor: Count, Size, Depth, Brightness, Color,
//! Time, Turbulence, Blink, Fall and a Random Seed its dialog does not show)
//! and its own random generator, so a seed scatters the particles where
//! Photopea's does.
//!
//! **Flame** lives in [`crate::flame`] (W13X-5): it draws along the active
//! path with Photopea's controls, and refuses without one.

use color::{linear_to_srgb, premultiply, srgb_to_linear, unpremultiply};

use crate::buffer::FilterBuffer;
use crate::support::fill_tiles;

/// Photopea's own random generator for Particles (a pair of
/// multiply-with-carry sequences), reproduced bit for bit so a seed places the
/// particles where Photopea's does. JavaScript's 32-bit integer arithmetic is
/// spelled out: `& 0xFFFFFFFF` there is a wrap to a signed 32-bit value.
struct PhotopeaRng {
    l1: i32,
    q7: i32,
}

impl PhotopeaRng {
    fn new(seed: u32) -> Self {
        let seed = i64::from(seed);
        PhotopeaRng {
            l1: (123_456_789 + seed) as i32,
            q7: (987_654_321 - seed) as i32,
        }
    }

    /// The next value in `[0, 1)`.
    fn get(&mut self) -> f64 {
        self.q7 = (36_969 * i64::from(self.q7 & 0xFFFF) + i64::from(self.q7 >> 16)) as i32;
        self.l1 = (18_000 * i64::from(self.l1 & 0xFFFF) + i64::from(self.l1 >> 16)) as i32;
        let w = (i64::from(self.q7.wrapping_shl(16)) + i64::from(self.l1 & 0xFFFF)) as u32;
        f64::from(w) / 4_294_967_296.0
    }
}

/// Photopea's Particles descriptor's Random Seed (its dialog does not show
/// one).
pub const PARTICLES_DEFAULT_SEED: u32 = 8_438_429;

/// Particles' settings — Photopea's `Part` descriptor, field for field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParticleSettings {
    /// *Count*, as a fraction: `0.1` (10 %) is ten particles per 100 x 100
    /// pixels.
    pub count: f32,
    /// *Size*, 1 to 50 px: a particle's diameter when in focus.
    pub size: u32,
    /// *Depth*, 0 to 1: how far out of focus a particle can be; each draws a
    /// random depth up to this and is blurred and shrunk by it.
    pub depth: f32,
    /// *Brightness*, 0.1 to 10 (10 % to 1000 %).
    pub brightness: f32,
    /// *Color*, straight linear RGB.
    pub color: [f32; 3],
    /// *Time*, 0 to 1: the animation phase turbulence and blink run on.
    pub time: f32,
    /// *Turbulence*, 0 to 1: how far particles wander from their seeds.
    pub turbulence: f32,
    /// *Blink*: brightness pulses with time, each particle out of phase.
    pub blink: bool,
    /// *Fall*: particles move down the image with time.
    pub fall: bool,
    /// Random Seed.
    pub seed: u32,
}

impl Default for ParticleSettings {
    /// Photopea's defaults: Count 10 %, Size 8, Depth 100 %, Brightness
    /// 800 %, white, Time 0, Turbulence 0, Blink on, Fall off.
    fn default() -> Self {
        ParticleSettings {
            count: 0.1,
            size: 8,
            depth: 1.0,
            brightness: 8.0,
            color: [1.0; 3],
            time: 0.0,
            turbulence: 0.0,
            blink: true,
            fall: false,
            seed: PARTICLES_DEFAULT_SEED,
        }
    }
}

/// One particle sprite: a cropped square of coverage and where its first
/// cell sits relative to the particle's pixel.
struct Sprite {
    side: usize,
    offset: i64,
    values: Vec<f32>,
}

/// Photopea's sprite: a disc of radius `size / 2 / (1 + 0.2 depth)`,
/// anti-aliased by `clamp(radius + 0.5 - distance)`, on a canvas eight sizes
/// wide, centred a quarter or three quarters of a pixel off the canvas centre
/// (`sub_x`, `sub_y`), blurred by `floor(size * bucket / 10)` px and cropped
/// to where it is at least 0.005.
fn particle_sprite(size: u32, depth: f32, bucket: u32, sub_x: f32, sub_y: f32) -> Sprite {
    let k8 = (size * 8) as usize;
    let s = (k8 / 2) as i64;
    let radius = size as f32 / 2.0 / (1.0 + 0.2 * depth);
    let centre = k8 as f32 / 2.0;
    let mut plane = vec![0.0f32; k8 * k8];
    for row in 0..k8 {
        for col in 0..k8 {
            let dx = col as f32 + sub_x - centre;
            let dy = row as f32 + sub_y - centre;
            plane[row * k8 + col] = (radius + 0.5 - dx.hypot(dy)).clamp(0.0, 1.0);
        }
    }
    let sigma = (size as f32 * bucket as f32 * 0.1).floor();
    let plane = crate::three_d::box_gauss(&plane, k8, k8, sigma);
    let mut first = 0i64;
    while first < s && plane[s as usize * k8 + first as usize] < 0.005 {
        first += 1;
    }
    if first != 0 {
        first -= 1;
    }
    let side = (2 * (s - first)) as usize;
    let mut values = vec![0.0f32; side * side];
    for row in 0..side {
        for col in 0..side {
            values[row * side + col] = plane[(first as usize + row) * k8 + first as usize + col];
        }
    }
    Sprite {
        side,
        offset: first - s,
        values,
    }
}

/// Particles: Photopea's renderer, ported.
///
/// `round(count * w * h / 100)` particles, each from eight draws of
/// Photopea's generator: position, depth, two turbulence radii and phases,
/// and a blink phase. Turbulence moves a particle by up to `8 * size *
/// turbulence` pixels along two circles whose phases advance with time; Fall
/// moves it `time * h` down (Photopea wraps it with JavaScript's `%`, which
/// keeps the sign, and so does this). Each particle adds its sprite, times its
/// brightness (pulsing between half and all of it with Blink), into a light
/// field; the field `f`, read as a gamma-encoded value, becomes the coverage
/// `linear(floor(255 f) / 255)` of the colour, which is **screened** over the
/// layer (on encoded values, straight alpha), so it only ever brightens and
/// the alpha rises to at least that coverage.
///
/// Sprites are cached per depth tenth for one run, each shaped by the first
/// particle's depth in its tenth. Photopea keeps that cache between runs of
/// the same size, so there a later run can reuse an earlier run's shapes;
/// here every run starts afresh and the same settings always give the same
/// picture.
pub fn particles(src: &FilterBuffer, settings: &ParticleSettings) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let clean = |v: f32, lo: f32, hi: f32, fallback: f32| {
        if v.is_finite() {
            v.clamp(lo, hi)
        } else {
            fallback
        }
    };
    let count_frac = f64::from(clean(settings.count, 0.0, 1.0, 0.0));
    let n = (count_frac * f64::from(w) * f64::from(h) * 0.01).round() as u64;
    let size = settings.size.clamp(1, 50);
    let depth = f64::from(clean(settings.depth, 0.0, 1.0, 0.0));
    let bright = f64::from(clean(settings.brightness, 0.0, 10.0, 1.0));
    let time = f64::from(clean(settings.time, 0.0, 1.0, 0.0));
    let turb = f64::from(clean(settings.turbulence, 0.0, 1.0, 0.0));
    let (wf, hf) = (f64::from(w), f64::from(h));
    let tau = std::f64::consts::TAU;
    let mut rng = PhotopeaRng::new(settings.seed);
    let mut cache: std::collections::HashMap<u32, Vec<Sprite>> = Default::default();
    let mut field = vec![0.0f32; (w as usize) * (h as usize)];
    for _ in 0..n {
        let mut x = rng.get() * wf;
        let mut y = rng.get() * hf;
        let z = rng.get() * depth;
        let r1 = rng.get() * f64::from(size) * 4.0;
        let p1 = (rng.get() + time) * tau;
        let r2 = rng.get() * f64::from(size) * 4.0;
        let p2 = (rng.get() + 2.0 * time) * tau;
        x += turb * (r1 * p1.cos() + r2 * p2.cos());
        y += turb * (r1 * p1.sin() + r2 * p2.sin());
        if settings.fall {
            y += time * hf;
        }
        let y = y % hf;
        let bucket = (z * 10.0).floor() as u32;
        let sprites = cache.entry(bucket).or_insert_with(|| {
            let zf = z as f32;
            [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]
                .into_iter()
                .map(|(sx, sy)| particle_sprite(size, zf, bucket, sx, sy))
                .collect()
        });
        let (vx, vy) = (x.floor(), y.floor());
        let a = usize::from(x - vx < 0.5);
        let l = usize::from(y - vy < 0.5);
        let sprite = &sprites[l * 2 + a];
        let blink_phase = rng.get();
        let e = if settings.blink {
            0.5 + 0.5 * bright * (0.5 + 0.5 * ((2.0 * time + blink_phase) * tau).sin())
        } else {
            bright
        } as f32;
        let (ox, oy) = (vx as i64 + sprite.offset, vy as i64 + sprite.offset);
        for row in 0..sprite.side {
            let cy = oy + row as i64;
            if cy < 0 || cy >= i64::from(h) {
                continue;
            }
            for col in 0..sprite.side {
                let cx = ox + col as i64;
                if cx < 0 || cx >= i64::from(w) {
                    continue;
                }
                field[cy as usize * w as usize + cx as usize] +=
                    e * sprite.values[row * sprite.side + col];
            }
        }
    }
    let light = settings.color.map(|c| {
        linear_to_srgb(if c.is_finite() {
            c.clamp(0.0, 1.0)
        } else {
            1.0
        })
    });
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let f = field[(y * w + x) as usize];
        let px = src.get(x, y);
        if f <= 0.0 {
            return px;
        }
        let coverage = srgb_to_linear(((255.0 * f).floor() / 255.0).min(1.0));
        screen_encoded(px, light, coverage)
    });
    out
}

/// Screen straight, encoded `light` at `coverage` over a premultiplied linear
/// pixel, with the W3C separable-blend compositing formula on encoded
/// straight values.
fn screen_encoded(px: [f32; 4], light: [f32; 3], coverage: f32) -> [f32; 4] {
    let s = unpremultiply(px);
    let ab = s[3].clamp(0.0, 1.0);
    let a_s = coverage.clamp(0.0, 1.0);
    let ao = a_s + ab * (1.0 - a_s);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let mut rgb = [0.0f32; 3];
    for c in 0..3 {
        let cb = linear_to_srgb(s[c].clamp(0.0, 1.0));
        let cs = light[c];
        let blended = 1.0 - (1.0 - cb) * (1.0 - cs);
        let cs2 = (1.0 - ab) * cs + ab * blended;
        let co = a_s * cs2 + (1.0 - a_s) * ab * cb;
        rgb[c] = (co / ao).clamp(0.0, 1.0);
    }
    premultiply([
        srgb_to_linear(rgb[0]),
        srgb_to_linear(rgb[1]),
        srgb_to_linear(rgb[2]),
        ao.clamp(0.0, 1.0),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn black(w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, [0.0, 0.0, 0.0, 1.0]).unwrap()
    }

    #[test]
    fn photopeas_generator_is_reproduced_bit_for_bit() {
        // The first draws of Photopea's own generator, run verbatim in Node
        // from its source: seed 8438429 (the descriptor's) and seed 0.
        let mut a = PhotopeaRng::new(PARTICLES_DEFAULT_SEED);
        let want = [
            0.595_971_792_005_002_5,
            0.504_526_508_739_218_1,
            0.810_427_063_144_743_4,
            0.879_201_704_403_385_5,
        ];
        for w in want {
            assert_eq!(a.get(), w);
        }
        let mut b = PhotopeaRng::new(0);
        for w in [
            0.732_297_654_030_844_6,
            0.058_062_777_621_671_56,
            0.821_864_804_020_151_5,
        ] {
            assert_eq!(b.get(), w);
        }
    }

    #[test]
    fn one_particle_lands_where_the_seed_puts_it() {
        // 20x20 at Count 25 %: round(0.25 * 400 / 100) = 1 particle. Its
        // first two draws put it at (0.596 * 20, 0.505 * 20) = (11.92,
        // 10.09); x's fraction is over a half and y's under, so its disc is
        // centred at (11 - 0.25, 10 - 0.75) = (10.75, 9.25) in pixel indices,
        // radius size / 2 = 2 with no depth. Red on black, Blink off,
        // Brightness 100 %: inside the disc the red is full.
        let src = black(20, 20);
        let settings = ParticleSettings {
            count: 0.25,
            size: 4,
            depth: 0.0,
            brightness: 1.0,
            color: [1.0, 0.0, 0.0],
            blink: false,
            ..ParticleSettings::default()
        };
        let out = particles(&src, &settings);
        assert_eq!(out.get(11, 9), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(out.get(10, 9), [1.0, 0.0, 0.0, 1.0]);
        for y in 0..20u32 {
            for x in 0..20u32 {
                let d = (x as f32 - 10.75).hypot(y as f32 - 9.25);
                let p = out.get(x, y);
                assert!(p[1] == 0.0 && p[2] == 0.0);
                if d > 2.5 {
                    assert_eq!(p, [0.0, 0.0, 0.0, 1.0], "({x}, {y}) is {d} away");
                }
            }
        }
        // Count 0 draws nothing.
        let none = ParticleSettings {
            count: 0.0,
            ..settings
        };
        assert_eq!(particles(&src, &none), src);
    }

    #[test]
    fn particles_are_seeded_screen_and_follow_their_controls() {
        let src = black(40, 30);
        let base = ParticleSettings::default();
        let a = particles(&src, &base);
        assert_eq!(a, particles(&src, &base), "deterministic");
        assert_ne!(a, src, "the defaults render");
        let reseeded = ParticleSettings { seed: 1, ..base };
        assert_ne!(a, particles(&src, &reseeded), "the seed places them");
        for changed in [
            ParticleSettings { time: 0.3, ..base },
            ParticleSettings {
                turbulence: 0.5,
                ..base
            },
            ParticleSettings {
                blink: false,
                ..base
            },
            ParticleSettings {
                fall: true,
                time: 0.5,
                ..base
            },
            ParticleSettings { depth: 0.0, ..base },
            ParticleSettings { size: 3, ..base },
        ] {
            assert_ne!(particles(&src, &changed), a, "{changed:?} changed nothing");
        }
        // Screening white over white changes nothing.
        let white = FilterBuffer::filled(8, 8, [1.0; 4]).unwrap();
        let dense = ParticleSettings { count: 1.0, ..base };
        assert_eq!(particles(&white, &dense).to_rgba8(), white.to_rgba8());
        // A clear layer gains coverage where a particle lands, and stays a
        // valid premultiplied pixel.
        let clear = FilterBuffer::transparent(20, 20).unwrap();
        let lit = particles(&clear, &dense);
        assert!(lit.pixels().iter().any(|p| p[3] > 0.3));
        assert!(lit.pixels().iter().all(|p| p[0] <= p[3] + 1e-6));
    }
}
