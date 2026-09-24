//! Filter ▸ Render ▸ Lighting Effects.
//!
//! The image is lit as a surface lying in the picture plane: each pixel's
//! straight linear colour is multiplied by the light that reaches it, the sum
//! of an ambient term and every [`Light`]'s contribution. So a point light
//! over the middle of the image brightens the middle and leaves the rest at
//! the ambient level, and negative intensity takes light away.
//!
//! Three kinds of light, as in Photoshop:
//!
//! * [`LightKind::Point`] (Photoshop's *Omni*/*Point*): shines equally in
//!   every direction from above `position`, falling off to nothing at
//!   `radius`.
//! * [`LightKind::Spot`]: an elliptical pool centred on `position`, elongated
//!   along `angle`. `focus` sets how much of the ellipse is at full strength
//!   before it fades (`-100` fades from the centre, `100` is a hard edge).
//! * [`LightKind::Infinite`]: a sun — the same direction everywhere, from
//!   `angle` in the plane and 45 degrees up.
//!
//! A **texture channel** turns one channel of the image into a height map
//! (white high, or black high), whose slopes tilt the surface normal, so the
//! lights rake across it and it reads as embossed relief.
//!
//! Positions are fractions of the image (`0..=1`), radii fractions of its
//! longer side, so the same settings light a preview proxy and the full
//! image the same way. Alpha is untouched; colour is unpremultiplied to be
//! lit and premultiplied back. The result is not clamped above `1.0` (the
//! working space is scene-referred) but never goes negative.

use color::{linear_srgb_luminance, premultiply, unpremultiply};
use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::support::{fill_tiles, smoothstep, EdgeMode};

/// Which kind of light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum LightKind {
    #[default]
    Spot,
    Point,
    Infinite,
}

/// One light.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Light {
    pub kind: LightKind,
    /// Straight linear RGB.
    pub color: [f32; 3],
    /// `-100..=100`; `50` is unit strength.
    pub intensity: f32,
    /// Spot only: `-100..=100`, how much of the pool is at full strength.
    pub focus: f32,
    /// Centre of the light, as a fraction of the image width and height.
    pub position: [f32; 2],
    /// Reach, as a fraction of the image's longer side.
    pub radius: f32,
    /// Spot: the pool's long axis. Infinite: the direction the light comes
    /// from. Degrees, counter-clockwise from the +x axis.
    pub angle: f32,
}

impl Default for Light {
    fn default() -> Self {
        Self {
            kind: LightKind::Spot,
            color: [1.0, 1.0, 1.0],
            intensity: 50.0,
            focus: 60.0,
            position: [0.5, 0.5],
            radius: 0.5,
            angle: 45.0,
        }
    }
}

/// Which channel of the image is the bump map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TextureChannel {
    #[default]
    None,
    Red,
    Green,
    Blue,
    Luminance,
}

/// The whole Lighting Effects setup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightingEffects {
    pub lights: Vec<Light>,
    /// `-100..=100`: the light every pixel gets regardless of the lights.
    /// `-100` is none, `100` is full.
    pub ambience: f32,
    pub texture: TextureChannel,
    /// Bump height, `0..=100`.
    pub height: f32,
    /// Whether white is high in the texture channel (else black is).
    pub white_is_high: bool,
}

impl Default for LightingEffects {
    fn default() -> Self {
        Self {
            lights: vec![Light::default()],
            ambience: -50.0,
            texture: TextureChannel::None,
            height: 50.0,
            white_is_high: true,
        }
    }
}

fn finite(v: f32, default: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        default
    }
}

/// Largest number of lights one call evaluates.
pub const MAX_LIGHTS: usize = 16;

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-12 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

impl LightingEffects {
    fn height_at(&self, src: &FilterBuffer, x: i64, y: i64) -> f32 {
        let s = unpremultiply(src.at(x, y, EdgeMode::Clamp));
        let v = match self.texture {
            TextureChannel::None => return 0.0,
            TextureChannel::Red => s[0],
            TextureChannel::Green => s[1],
            TextureChannel::Blue => s[2],
            TextureChannel::Luminance => linear_srgb_luminance([s[0], s[1], s[2]]),
        }
        .clamp(0.0, 1.0);
        if self.white_is_high {
            v
        } else {
            1.0 - v
        }
    }

    /// Light arriving at `(x, y)` (pixel centre coordinates) through normal
    /// `n`, per channel, ambient included.
    fn light_at(&self, px: [f32; 2], n: [f32; 3], w: f32, h: f32) -> [f32; 3] {
        let ambient = ((finite(self.ambience, -50.0) / 100.0).clamp(-1.0, 1.0) + 1.0) * 0.5;
        let mut total = [ambient; 3];
        let long = w.max(h).max(1.0);
        for light in self.lights.iter().take(MAX_LIGHTS) {
            let strength = (finite(light.intensity, 0.0) / 50.0).clamp(-2.0, 2.0);
            if strength == 0.0 {
                continue;
            }
            let pos = [
                finite(light.position[0], 0.5) * w,
                finite(light.position[1], 0.5) * h,
            ];
            let reach = (finite(light.radius, 0.5).clamp(0.01, 4.0) * long).max(1.0);
            let angle = finite(light.angle, 0.0).to_radians();
            let (sin, cos) = angle.sin_cos();
            let amount = match light.kind {
                LightKind::Infinite => {
                    let dir = normalize([cos, -sin, 1.0]);
                    dot(n, dir).max(0.0)
                }
                LightKind::Point | LightKind::Spot => {
                    let d = [pos[0] - px[0], pos[1] - px[1]];
                    // The light hangs above its centre at half its reach.
                    let to_light = normalize([d[0], d[1], 0.5 * reach]);
                    let diffuse = dot(n, to_light).max(0.0);
                    let falloff = if light.kind == LightKind::Point {
                        let r = (d[0] * d[0] + d[1] * d[1]).sqrt() / reach;
                        let f = (1.0 - r * r).max(0.0);
                        f * f
                    } else {
                        // Ellipse: long axis along `angle`, half as wide.
                        let a = (-d[0] * cos + d[1] * sin) / reach;
                        let b = (-d[0] * sin - d[1] * cos) / (0.5 * reach);
                        let e = (a * a + b * b).sqrt();
                        let hard = (finite(light.focus, 0.0) / 100.0).clamp(-1.0, 1.0);
                        let start = 0.45 * (hard + 1.0);
                        1.0 - smoothstep(start, 1.0, e)
                    };
                    diffuse * falloff
                }
            };
            for (c, t) in total.iter_mut().enumerate() {
                *t += strength * amount * finite(light.color[c], 1.0).max(0.0);
            }
        }
        total.map(|t| t.max(0.0))
    }

    /// Light the image.
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        if src.is_empty() {
            return src.clone();
        }
        let (w, h) = src.dimensions();
        let (wf, hf) = (w as f32, h as f32);
        let bump = self.texture != TextureChannel::None;
        let scale = (finite(self.height, 50.0) / 100.0).clamp(0.0, 1.0) * 8.0;
        let mut out = src.same_size_blank();
        fill_tiles(w, h, out.pixels_mut(), |x, y| {
            let s = unpremultiply(src.get(x, y));
            if s[3] <= 0.0 {
                return [0.0; 4];
            }
            let n = if bump && scale > 0.0 {
                let (xi, yi) = (i64::from(x), i64::from(y));
                let dx = self.height_at(src, xi + 1, yi) - self.height_at(src, xi - 1, yi);
                let dy = self.height_at(src, xi, yi + 1) - self.height_at(src, xi, yi - 1);
                normalize([-dx * 0.5 * scale, -dy * 0.5 * scale, 1.0])
            } else {
                [0.0, 0.0, 1.0]
            };
            let l = self.light_at([x as f32 + 0.5, y as f32 + 0.5], n, wf, hf);
            premultiply([s[0] * l[0], s[1] * l[1], s[2] * l[2], s[3]])
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, [0.4, 0.4, 0.4, 1.0]).unwrap()
    }

    #[test]
    fn a_point_light_brightens_its_centre_more_than_the_edges() {
        let src = grey(64, 64);
        let fx = LightingEffects {
            lights: vec![Light {
                kind: LightKind::Point,
                position: [0.5, 0.5],
                radius: 0.6,
                ..Light::default()
            }],
            ..LightingEffects::default()
        };
        let out = fx.apply(&src);
        let centre = out.get(32, 32)[0];
        let edge = out.get(0, 32)[0];
        let corner = out.get(0, 0)[0];
        assert!(centre > 0.4, "the centre is lit: {centre}");
        assert!(centre > edge + 0.1, "centre {centre} vs edge {edge}");
        assert!(edge >= corner, "edge {edge} vs corner {corner}");
    }

    #[test]
    fn moving_the_light_moves_the_bright_spot() {
        let src = grey(64, 64);
        let at = |pos: [f32; 2]| {
            LightingEffects {
                lights: vec![Light {
                    kind: LightKind::Point,
                    position: pos,
                    radius: 0.3,
                    ..Light::default()
                }],
                ..LightingEffects::default()
            }
            .apply(&src)
        };
        let left = at([0.2, 0.5]);
        let right = at([0.8, 0.5]);
        assert!(left.get(12, 32)[0] > left.get(51, 32)[0]);
        assert!(right.get(51, 32)[0] > right.get(12, 32)[0]);
    }

    #[test]
    fn spot_focus_and_infinite_lights_behave() {
        let src = grey(64, 64);
        let spot = |focus: f32| {
            LightingEffects {
                lights: vec![Light {
                    focus,
                    ..Light::default()
                }],
                ..LightingEffects::default()
            }
            .apply(&src)
        };
        // A harder focus keeps more of the pool at full strength.
        let soft = spot(-100.0);
        let hard = spot(100.0);
        assert!(hard.get(40, 24)[0] >= soft.get(40, 24)[0]);
        assert!(soft.get(32, 32)[0] > 0.4);
        // An infinite light lights everything the same on a flat surface.
        let sun = LightingEffects {
            lights: vec![Light {
                kind: LightKind::Infinite,
                ..Light::default()
            }],
            ..LightingEffects::default()
        }
        .apply(&src);
        assert!((sun.get(0, 0)[0] - sun.get(40, 50)[0]).abs() < 1e-5);
        // Coloured light tints.
        let red = LightingEffects {
            lights: vec![Light {
                kind: LightKind::Infinite,
                color: [1.0, 0.0, 0.0],
                ..Light::default()
            }],
            ..LightingEffects::default()
        }
        .apply(&src)
        .get(5, 5);
        assert!(red[0] > red[1] && (red[1] - red[2]).abs() < 1e-6);
    }

    #[test]
    fn a_texture_channel_makes_relief() {
        let mut src = grey(32, 32);
        for y in 0..32 {
            for x in 12..20 {
                src.set(x, y, [0.9, 0.9, 0.9, 1.0]);
            }
        }
        let lit = |texture| {
            LightingEffects {
                lights: vec![Light {
                    kind: LightKind::Infinite,
                    angle: 0.0,
                    ..Light::default()
                }],
                texture,
                ..LightingEffects::default()
            }
            .apply(&src)
        };
        let flat = lit(TextureChannel::None);
        let bumped = lit(TextureChannel::Luminance);
        // The ridge's two flanks face towards and away from the light.
        let rising = bumped.get(11, 16)[0] / flat.get(11, 16)[0];
        let falling = bumped.get(20, 16)[0] / flat.get(20, 16)[0];
        assert!((rising - falling).abs() > 0.05, "{rising} vs {falling}");
    }

    #[test]
    fn no_lights_is_ambient_only_and_alpha_is_kept() {
        let src = FilterBuffer::filled(4, 4, [0.2, 0.2, 0.2, 0.5]).unwrap();
        let out = LightingEffects {
            lights: Vec::new(),
            ambience: 100.0,
            ..LightingEffects::default()
        }
        .apply(&src);
        assert_eq!(out, src);
    }
}
