//! Blend modes: the full 27-mode Photoshop / Photopea set.
//!
//! Two evaluation entry points exist because the modes fall into two classes:
//!
//! * **Separable** modes act on one channel at a time — [`BlendMode::blend_channel`].
//! * **Non-separable** modes need all three channels at once because they
//!   transplant luminosity or saturation between colors — [`BlendMode::blend_rgb`].
//!
//! [`BlendMode::blend_rgb`] is total: it handles every mode, delegating to
//! `blend_channel` for the separable ones. Use it unless you are inside a
//! per-channel loop and have already checked [`BlendMode::is_separable`].
//!
//! Both blend entry points work in **straight-alpha, non-premultiplied** color
//! and are *total over all `f32`*: every input channel is first mapped into
//! `0.0..=1.0` by [`unit()`] (non-finite values become `0.0`) and every returned
//! channel is mapped the same way, so no caller can be handed a NaN, an
//! infinity, or an out-of-range channel. Alpha compositing (the
//! `Cr = (1 - ab)*Cs + ab*B(Cb, Cs)` outer layer of the W3C model) is the
//! compositor's job, not this module's.
//!
//! [`dissolve_keeps_source`] is the one exception to the color contract: it
//! lives in the alpha domain and returns a `bool`.

use serde::{Deserialize, Serialize};

/// Declares `enum BlendMode` and `BlendMode::ALL` from a single variant list.
///
/// The point is enforcement, not brevity: because both items are emitted from
/// one invocation, a variant that exists in the enum necessarily has an entry
/// in `ALL`, in the same order. There is no way to add one without the other.
macro_rules! blend_modes {
    ($( $(#[$attr:meta])* $name:ident ),+ $(,)?) => {
        /// How a layer's pixels combine with the composite beneath it.
        ///
        /// Variant order matches the Photoshop menu grouping. Serialization is
        /// by variant name, so adding variants is backward compatible; renaming
        /// one is not.
        ///
        /// Declared together with [`BlendMode::ALL`] by the `blend_modes!`
        /// macro — see that macro for why the two cannot drift apart.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
        pub enum BlendMode {
            $( $(#[$attr])* $name, )+
        }

        impl BlendMode {
            /// Every mode, in menu order.
            ///
            /// Exhaustive by construction: this array and the `BlendMode`
            /// variant list are expanded from the same `blend_modes!`
            /// invocation, so a new variant lands here automatically and the
            /// array length grows with it. `all_covers_every_variant_and_the_count_is_pinned`
            /// then fails until the pinned count is updated deliberately.
            pub const ALL: [BlendMode; [$(BlendMode::$name),+].len()] =
                [$(BlendMode::$name),+];
        }
    };
}

blend_modes! {
    #[default]
    Normal,
    Dissolve,

    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    DarkerColor,

    Lighten,
    Screen,
    ColorDodge,
    /// Photoshop calls this "Linear Dodge (Add)".
    LinearDodge,
    LighterColor,

    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,

    Difference,
    Exclusion,
    Subtract,
    Divide,

    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    /// Stable index used to select the matching GPU pipeline / WGSL branch.
    /// Keep in sync with `render-shaders`.
    ///
    /// Indices 0..=5 are frozen at their original scaffold values; new modes
    /// were appended. Never renumber an existing mode — shipped documents and
    /// compiled shader tables both key off this number.
    pub const fn shader_index(self) -> u32 {
        match self {
            BlendMode::Normal => 0,
            BlendMode::Multiply => 1,
            BlendMode::Screen => 2,
            BlendMode::Overlay => 3,
            BlendMode::Darken => 4,
            BlendMode::Lighten => 5,
            BlendMode::Dissolve => 6,
            BlendMode::ColorBurn => 7,
            BlendMode::LinearBurn => 8,
            BlendMode::DarkerColor => 9,
            BlendMode::ColorDodge => 10,
            BlendMode::LinearDodge => 11,
            BlendMode::LighterColor => 12,
            BlendMode::SoftLight => 13,
            BlendMode::HardLight => 14,
            BlendMode::VividLight => 15,
            BlendMode::LinearLight => 16,
            BlendMode::PinLight => 17,
            BlendMode::HardMix => 18,
            BlendMode::Difference => 19,
            BlendMode::Exclusion => 20,
            BlendMode::Subtract => 21,
            BlendMode::Divide => 22,
            BlendMode::Hue => 23,
            BlendMode::Saturation => 24,
            BlendMode::Color => 25,
            BlendMode::Luminosity => 26,
        }
    }

    /// Human-readable name as shown in the blend-mode dropdown.
    pub const fn label(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Dissolve => "Dissolve",
            BlendMode::Darken => "Darken",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::LinearBurn => "Linear Burn",
            BlendMode::DarkerColor => "Darker Color",
            BlendMode::Lighten => "Lighten",
            BlendMode::Screen => "Screen",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::LinearDodge => "Linear Dodge (Add)",
            BlendMode::LighterColor => "Lighter Color",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::VividLight => "Vivid Light",
            BlendMode::LinearLight => "Linear Light",
            BlendMode::PinLight => "Pin Light",
            BlendMode::HardMix => "Hard Mix",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Subtract => "Subtract",
            BlendMode::Divide => "Divide",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    /// `true` when the mode is defined channel-by-channel and
    /// [`BlendMode::blend_channel`] is authoritative.
    ///
    /// Six modes are not: `Hue`, `Saturation`, `Color` and `Luminosity` move
    /// HSL components between colors, and `DarkerColor` / `LighterColor` pick a
    /// whole color by comparing luminosity. All six require
    /// [`BlendMode::blend_rgb`].
    ///
    /// `Dissolve` counts as separable here: its *color* result is `Normal`. The
    /// stochastic part lives in the alpha domain — see [`dissolve_keeps_source`].
    ///
    /// Written as an exhaustive positive match rather than a negative
    /// `matches!` list so that adding a variant is a compile error until it is
    /// deliberately classified. A mode silently defaulting to "separable" would
    /// be rendered per-channel and be wrong with no diagnostic.
    pub const fn is_separable(self) -> bool {
        match self {
            BlendMode::Normal
            | BlendMode::Dissolve
            | BlendMode::Darken
            | BlendMode::Multiply
            | BlendMode::ColorBurn
            | BlendMode::LinearBurn
            | BlendMode::Lighten
            | BlendMode::Screen
            | BlendMode::ColorDodge
            | BlendMode::LinearDodge
            | BlendMode::Overlay
            | BlendMode::SoftLight
            | BlendMode::HardLight
            | BlendMode::VividLight
            | BlendMode::LinearLight
            | BlendMode::PinLight
            | BlendMode::HardMix
            | BlendMode::Difference
            | BlendMode::Exclusion
            | BlendMode::Subtract
            | BlendMode::Divide => true,

            BlendMode::DarkerColor
            | BlendMode::LighterColor
            | BlendMode::Hue
            | BlendMode::Saturation
            | BlendMode::Color
            | BlendMode::Luminosity => false,
        }
    }

    /// Apply this blend mode to a single pair of **straight-alpha** color
    /// channels.
    ///
    /// Total over all `f32`: both inputs and the result pass through [`unit()`],
    /// so out-of-range inputs are clamped, non-finite inputs are treated as
    /// `0.0`, and the return value is always finite and within `0.0..=1.0`.
    ///
    /// This is the reference implementation the GPU shaders must match within
    /// tolerance.
    ///
    /// # Non-separable modes
    ///
    /// When [`BlendMode::is_separable`] is `false` the mode has no single-channel
    /// definition and this returns `src` (the `Normal` result). Callers driving a
    /// per-channel loop must branch on `is_separable` and call
    /// [`BlendMode::blend_rgb`] instead; the fallback exists so this function is
    /// total, not because it is correct for those modes.
    pub fn blend_channel(self, base: f32, src: f32) -> f32 {
        let b = unit(base);
        let s = unit(src);
        let out = match self {
            BlendMode::Normal | BlendMode::Dissolve => s,

            BlendMode::Darken => b.min(s),
            BlendMode::Multiply => b * s,
            BlendMode::ColorBurn => color_burn(b, s),
            BlendMode::LinearBurn => b + s - 1.0,

            BlendMode::Lighten => b.max(s),
            BlendMode::Screen => b + s - b * s,
            BlendMode::ColorDodge => color_dodge(b, s),
            BlendMode::LinearDodge => b + s,

            // Overlay is HardLight with the operands swapped.
            BlendMode::Overlay => hard_light(s, b),
            BlendMode::SoftLight => soft_light(b, s),
            BlendMode::HardLight => hard_light(b, s),
            BlendMode::VividLight => {
                if s <= 0.5 {
                    color_burn(b, 2.0 * s)
                } else {
                    color_dodge(b, 2.0 * s - 1.0)
                }
            }
            // LinearBurn(b, 2s) for s<=0.5 and LinearDodge(b, 2s-1) above both
            // reduce to the same expression.
            BlendMode::LinearLight => b + 2.0 * s - 1.0,
            BlendMode::PinLight => {
                if s <= 0.5 {
                    b.min(2.0 * s)
                } else {
                    b.max(2.0 * s - 1.0)
                }
            }
            // Threshold of VividLight at 0.5, which is exactly `b + s >= 1`.
            BlendMode::HardMix => {
                if b + s >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }

            BlendMode::Difference => (b - s).abs(),
            BlendMode::Exclusion => b + s - 2.0 * b * s,
            BlendMode::Subtract => b - s,
            BlendMode::Divide => {
                if s <= 0.0 {
                    1.0
                } else {
                    b / s
                }
            }

            // Undefined per channel — see the doc comment.
            BlendMode::DarkerColor
            | BlendMode::LighterColor
            | BlendMode::Hue
            | BlendMode::Saturation
            | BlendMode::Color
            | BlendMode::Luminosity => s,
        };
        unit(out)
    }

    /// Apply this blend mode to a full RGB triple.
    ///
    /// Total over all 27 modes *and* over all `f32` values: separable modes run
    /// [`BlendMode::blend_channel`] per channel, the four HSL modes use the W3C
    /// compositing `SetLum` / `SetSat` algorithm, and `DarkerColor` /
    /// `LighterColor` select one whole input color by luminosity.
    ///
    /// Every input channel and every output channel passes through [`unit()`].
    /// The output clamp is load-bearing, not defensive: `set_lum`'s
    /// `clip_color` only restores gamut for in-gamut inputs, and the
    /// `DarkerColor` / `LighterColor` arms return an input verbatim — an
    /// HDR/float caller would otherwise get channels outside `0.0..=1.0` back
    /// out of a function documented to produce display-referred color.
    pub fn blend_rgb(self, base: [f32; 3], src: [f32; 3]) -> [f32; 3] {
        let base = [unit(base[0]), unit(base[1]), unit(base[2])];
        let src = [unit(src[0]), unit(src[1]), unit(src[2])];
        let out = match self {
            BlendMode::DarkerColor => {
                if lum(base) <= lum(src) {
                    base
                } else {
                    src
                }
            }
            BlendMode::LighterColor => {
                if lum(base) >= lum(src) {
                    base
                } else {
                    src
                }
            }
            // B(Cb, Cs) = SetLum(SetSat(Cs, Sat(Cb)), Lum(Cb))
            BlendMode::Hue => set_lum(set_sat(src, sat(base)), lum(base)),
            // B(Cb, Cs) = SetLum(SetSat(Cb, Sat(Cs)), Lum(Cb))
            BlendMode::Saturation => set_lum(set_sat(base, sat(src)), lum(base)),
            // B(Cb, Cs) = SetLum(Cs, Lum(Cb))
            BlendMode::Color => set_lum(src, lum(base)),
            // B(Cb, Cs) = SetLum(Cb, Lum(Cs))
            BlendMode::Luminosity => set_lum(base, lum(src)),
            separable => [
                separable.blend_channel(base[0], src[0]),
                separable.blend_channel(base[1], src[1]),
                separable.blend_channel(base[2], src[2]),
            ],
        };
        [unit(out[0]), unit(out[1]), unit(out[2])]
    }
}

/// Dissolve's per-pixel decision: the source pixel is drawn fully opaque with
/// probability equal to its effective alpha, otherwise it is not drawn at all.
///
/// `noise` must be a uniform sample in `0.0..1.0` (a hash of the pixel
/// coordinate plus a seed, so the pattern is stable between frames). Returns
/// `true` when the source pixel wins.
pub fn dissolve_keeps_source(effective_alpha: f32, noise: f32) -> bool {
    noise < effective_alpha
}

/// Map any `f32` into `0.0..=1.0`.
///
/// Finite values are clamped. Non-finite values (NaN, ±inf) become `0.0`: a
/// plain `clamp` propagates NaN rather than substituting a bound, and one NaN
/// channel entering the compositor's accumulator poisons the whole composite.
/// `0.0` is the conservative substitution — a corrupt sample goes black instead
/// of destroying the frame.
pub fn unit(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn color_burn(b: f32, s: f32) -> f32 {
    if b >= 1.0 {
        1.0
    } else if s <= 0.0 {
        0.0
    } else {
        1.0 - (1.0f32).min((1.0 - b) / s)
    }
}

fn color_dodge(b: f32, s: f32) -> f32 {
    if b <= 0.0 {
        0.0
    } else if s >= 1.0 {
        1.0
    } else {
        (1.0f32).min(b / (1.0 - s))
    }
}

fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        // Multiply(b, 2s)
        b * (2.0 * s)
    } else {
        // Screen(b, 2s - 1)
        let s2 = 2.0 * s - 1.0;
        b + s2 - b * s2
    }
}

fn soft_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b - (1.0 - 2.0 * s) * b * (1.0 - b)
    } else {
        let d = if b <= 0.25 {
            ((16.0 * b - 12.0) * b + 4.0) * b
        } else {
            b.sqrt()
        };
        b + (2.0 * s - 1.0) * (d - b)
    }
}

/// W3C `Lum(C)`: the standard non-linear luminosity coefficients used by the
/// compositing spec (and by Photoshop's HSL modes) — *not* Rec.709 luma.
pub fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// W3C `Sat(C)`: the span between the largest and smallest channel.
pub fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// W3C `ClipColor(C)`: pull out-of-gamut channels back into `0..=1` by scaling
/// the color toward its own luminosity, which preserves hue.
fn clip_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    // Both branches read `n` and `x` from the *original* color, per the spec.
    if n < 0.0 {
        let d = l - n;
        if d.abs() > f32::EPSILON {
            for ch in &mut c {
                *ch = l + (*ch - l) * l / d;
            }
        } else {
            c = [l, l, l];
        }
    }
    if x > 1.0 {
        let d = x - l;
        if d.abs() > f32::EPSILON {
            for ch in &mut c {
                *ch = l + (*ch - l) * (1.0 - l) / d;
            }
        } else {
            c = [l, l, l];
        }
    }
    c
}

/// W3C `SetLum(C, l)`: shift every channel by the same amount so the result has
/// luminosity `l`, then clip back into gamut.
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

/// W3C `SetSat(C, s)`: rescale the color so its channel span is `s`, keeping the
/// relative position of the middle channel. The result is anchored at 0, so it
/// is only meaningful as input to [`set_lum`].
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    // Index of the min, mid and max channels.
    let (mut imin, mut imid, mut imax) = (0usize, 1usize, 2usize);
    if c[imin] > c[imid] {
        std::mem::swap(&mut imin, &mut imid);
    }
    if c[imid] > c[imax] {
        std::mem::swap(&mut imid, &mut imax);
    }
    if c[imin] > c[imid] {
        std::mem::swap(&mut imin, &mut imid);
    }

    let mut out = [0.0f32; 3];
    if c[imax] > c[imin] {
        out[imid] = (c[imid] - c[imin]) * s / (c[imax] - c[imin]);
        out[imax] = s;
    }
    // out[imin] stays 0.0; when max == min the whole color collapses to 0.
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absolute tolerance for f32 reference comparisons.
    const EPS: f32 = 1e-5;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= EPS
    }

    fn close3(a: [f32; 3], b: [f32; 3]) -> bool {
        close(a[0], b[0]) && close(a[1], b[1]) && close(a[2], b[2])
    }

    /// The count `ALL` and the enum must agree on. `ALL`'s length is derived
    /// from the variant list by the `blend_modes!` macro, so this constant is
    /// the *only* place a mode count is written by hand: adding a variant
    /// changes `BlendMode::ALL.len()` and turns this test red until the count,
    /// the shader table and the separability classification are all revisited.
    const EXPECTED_MODE_COUNT: usize = 27;

    #[test]
    fn all_covers_every_variant_and_the_count_is_pinned() {
        assert_eq!(
            BlendMode::ALL.len(),
            EXPECTED_MODE_COUNT,
            "the enum gained or lost a variant; update EXPECTED_MODE_COUNT, \
             shader_index, is_separable and the reference tables"
        );
        let mut seen = std::collections::HashSet::new();
        for m in BlendMode::ALL {
            assert!(seen.insert(m), "{:?} listed twice in ALL", m);
        }
        assert_eq!(seen.len(), EXPECTED_MODE_COUNT);
    }

    #[test]
    fn shader_indices_unique_and_dense() {
        let mut idx: Vec<u32> = BlendMode::ALL.iter().map(|m| m.shader_index()).collect();
        idx.sort_unstable();
        // Pinned to the literal count, not to `ALL.len()`: deriving both sides
        // from `ALL` would let the two shrink together and hide a gap.
        assert_eq!(idx, (0..EXPECTED_MODE_COUNT as u32).collect::<Vec<u32>>());
    }

    #[test]
    fn every_mode_is_classified_and_the_two_classes_partition_all() {
        // `is_separable` is an exhaustive positive match, so this counts the
        // classification the compiler forced someone to make.
        let sep = BlendMode::ALL.iter().filter(|m| m.is_separable()).count();
        let non = BlendMode::ALL.iter().filter(|m| !m.is_separable()).count();
        assert_eq!(sep + non, EXPECTED_MODE_COUNT);
        assert_eq!(non, 6, "exactly six modes need blend_rgb");
        // Every non-separable mode must actually differ from the per-channel
        // fallback for at least one input, or the classification is a lie.
        let per_channel = |m: BlendMode, b: [f32; 3], s: [f32; 3]| {
            [
                m.blend_channel(b[0], s[0]),
                m.blend_channel(b[1], s[1]),
                m.blend_channel(b[2], s[2]),
            ]
        };
        for &m in BlendMode::ALL.iter().filter(|m| !m.is_separable()) {
            let differs = m.blend_rgb(CB, CS) != per_channel(m, CB, CS)
                || m.blend_rgb(CS, CB) != per_channel(m, CS, CB);
            assert!(
                differs,
                "{m:?} is marked non-separable but matches the per-channel result"
            );
        }
    }

    #[test]
    fn legacy_shader_indices_are_frozen() {
        // Shipped documents and the WGSL branch table key off these numbers.
        assert_eq!(BlendMode::Normal.shader_index(), 0);
        assert_eq!(BlendMode::Multiply.shader_index(), 1);
        assert_eq!(BlendMode::Screen.shader_index(), 2);
        assert_eq!(BlendMode::Overlay.shader_index(), 3);
        assert_eq!(BlendMode::Darken.shader_index(), 4);
        assert_eq!(BlendMode::Lighten.shader_index(), 5);
    }

    #[test]
    fn labels_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in BlendMode::ALL {
            assert!(seen.insert(m.label()), "duplicate label for {:?}", m);
        }
    }

    #[test]
    fn every_mode_serde_roundtrips_by_name() {
        for m in BlendMode::ALL {
            let json = serde_json::to_string(&m).unwrap();
            let back: BlendMode = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
        assert_eq!(
            serde_json::to_string(&BlendMode::LinearDodge).unwrap(),
            "\"LinearDodge\""
        );
    }

    /// Check one full reference table: every separable mode's expected result
    /// at a single `(base, src)` sample, through both entry points.
    ///
    /// The 21 separable modes + the 6 `is_separable` classifies as needing
    /// `blend_rgb` = the 27 in `ALL`;
    /// `every_mode_is_classified_and_the_two_classes_partition_all` pins that
    /// split, and the set equality below pins each table against it, so a new
    /// mode cannot quietly skip its reference value.
    ///
    /// One sample per table is not enough on its own: `Overlay`, `SoftLight`,
    /// `HardLight`, `VividLight` and `PinLight` are piecewise, so a table that
    /// only ever lands on one side of the split leaves the other side pinned by
    /// nothing. That is why the callers below sample three points that between
    /// them cover both arms of every piecewise mode.
    fn assert_reference_table(b: f32, s: f32, cases: &[(BlendMode, f32)]) {
        let listed: std::collections::HashSet<BlendMode> = cases.iter().map(|&(m, _)| m).collect();
        let separable: std::collections::HashSet<BlendMode> = BlendMode::ALL
            .iter()
            .copied()
            .filter(|m| m.is_separable())
            .collect();
        assert_eq!(listed.len(), cases.len(), "a mode is listed twice");
        assert_eq!(
            listed, separable,
            "the table must cover exactly the separable modes"
        );
        assert_eq!(cases.len(), 21, "the doc comments name this count");

        for &(mode, expected) in cases {
            let got = mode.blend_channel(b, s);
            assert!(
                close(got, expected),
                "{mode:?}({b}, {s}): got {got}, expected {expected}"
            );
            // blend_rgb must agree channel-for-channel for separable modes.
            let rgb = mode.blend_rgb([b; 3], [s; 3]);
            assert!(
                close3(rgb, [expected; 3]),
                "{mode:?} blend_rgb disagrees at ({b}, {s}): got {rgb:?}"
            );
        }
    }

    /// Reference table for the 21 separable modes at base=0.25, src=0.75.
    ///
    /// This sample takes the `s > 0.5` arm of SoftLight/HardLight/VividLight/
    /// PinLight and the `b < 0.5` arm of Overlay; the darkening halves are
    /// covered by `separable_reference_values_at_three_quarters_and_a_quarter`.
    ///
    /// Values are hand-derived from the W3C Compositing 1 formulas (and the
    /// Photoshop definitions for the four not in that spec).
    #[test]
    fn separable_reference_values_at_quarter_and_three_quarters() {
        let b = 0.25f32;
        let s = 0.75f32;
        let cases: &[(BlendMode, f32)] = &[
            (BlendMode::Normal, 0.75),
            (BlendMode::Dissolve, 0.75),
            (BlendMode::Darken, 0.25),
            (BlendMode::Multiply, 0.1875),
            // 1 - min(1, (1-0.25)/0.75) = 1 - 1 = 0
            (BlendMode::ColorBurn, 0.0),
            // 0.25 + 0.75 - 1 = 0
            (BlendMode::LinearBurn, 0.0),
            (BlendMode::Lighten, 0.75),
            // 0.25 + 0.75 - 0.1875
            (BlendMode::Screen, 0.8125),
            // min(1, 0.25/(1-0.75)) = min(1, 1) = 1
            (BlendMode::ColorDodge, 1.0),
            (BlendMode::LinearDodge, 1.0),
            // base < 0.5 -> 2*0.25*0.75
            (BlendMode::Overlay, 0.375),
            // s > 0.5, b <= 0.25 -> D(b) = ((16b-12)b+4)b = ((4-12)*0.25+4)*0.25
            //                            = (-8*0.25+4)*0.25 = 2*0.25 = 0.5
            // b + (2s-1)*(D-b) = 0.25 + 0.5*(0.5-0.25) = 0.375
            (BlendMode::SoftLight, 0.375),
            // s > 0.5 -> Screen(0.25, 0.5) = 0.25 + 0.5 - 0.125
            (BlendMode::HardLight, 0.625),
            // s > 0.5 -> ColorDodge(0.25, 0.5) = min(1, 0.25/0.5)
            (BlendMode::VividLight, 0.5),
            // 0.25 + 1.5 - 1
            (BlendMode::LinearLight, 0.75),
            // s > 0.5 -> max(0.25, 0.5)
            (BlendMode::PinLight, 0.5),
            // b + s = 1.0 >= 1 -> 1
            (BlendMode::HardMix, 1.0),
            (BlendMode::Difference, 0.5),
            // 0.25 + 0.75 - 2*0.1875
            (BlendMode::Exclusion, 0.625),
            // 0.25 - 0.75 clamped
            (BlendMode::Subtract, 0.0),
            // min(1, 0.25/0.75) = 0.333..
            (BlendMode::Divide, 1.0 / 3.0),
        ];
        assert_reference_table(b, s, cases);
    }

    /// The mirror image of the table above: base=0.75, src=0.25.
    ///
    /// This is the sample that pins the *darkening* halves — the `s <= 0.5` arm
    /// of SoftLight, HardLight, VividLight and PinLight, and the `b > 0.5` arm
    /// of Overlay. Without it, inverting PinLight's `min` to a `max`, flipping
    /// soft light's darkening sign, or swapping VividLight's burn for a dodge
    /// all pass unnoticed.
    #[test]
    fn separable_reference_values_at_three_quarters_and_a_quarter() {
        let b = 0.75f32;
        let s = 0.25f32;
        let cases: &[(BlendMode, f32)] = &[
            (BlendMode::Normal, 0.25),
            (BlendMode::Dissolve, 0.25),
            (BlendMode::Darken, 0.25),
            (BlendMode::Multiply, 0.1875),
            // 1 - min(1, (1-0.75)/0.25) = 1 - min(1, 1) = 0
            (BlendMode::ColorBurn, 0.0),
            // 0.75 + 0.25 - 1 = 0
            (BlendMode::LinearBurn, 0.0),
            (BlendMode::Lighten, 0.75),
            // 0.75 + 0.25 - 0.1875
            (BlendMode::Screen, 0.8125),
            // min(1, 0.75/(1-0.25)) = min(1, 1) = 1
            (BlendMode::ColorDodge, 1.0),
            // 0.75 + 0.25 = 1
            (BlendMode::LinearDodge, 1.0),
            // Overlay = HardLight(s, b), so here the *source* 0.25 is the base
            // of a Screen(0.25, 2*0.75-1) = 0.25 + 0.5 - 0.125
            (BlendMode::Overlay, 0.625),
            // s <= 0.5 -> b - (1-2s)*b*(1-b) = 0.75 - 0.5*0.75*0.25
            //           = 0.75 - 0.09375
            (BlendMode::SoftLight, 0.65625),
            // s <= 0.5 -> Multiply(0.75, 0.5)
            (BlendMode::HardLight, 0.375),
            // s <= 0.5 -> ColorBurn(0.75, 0.5) = 1 - min(1, 0.25/0.5)
            (BlendMode::VividLight, 0.5),
            // 0.75 + 0.5 - 1
            (BlendMode::LinearLight, 0.25),
            // s <= 0.5 -> min(0.75, 0.5)
            (BlendMode::PinLight, 0.5),
            // b + s = 1.0 >= 1 -> 1. Still exactly on the threshold; the sample
            // below and `hard_mix_thresholds_at_unit_sum` cover it off-boundary.
            (BlendMode::HardMix, 1.0),
            (BlendMode::Difference, 0.5),
            // 0.75 + 0.25 - 2*0.1875
            (BlendMode::Exclusion, 0.625),
            // 0.75 - 0.25, no clamping this time
            (BlendMode::Subtract, 0.5),
            // 0.75/0.25 = 3, clamped
            (BlendMode::Divide, 1.0),
        ];
        assert_reference_table(b, s, cases);
    }

    /// A third table at base=0.4, src=0.55, off every threshold.
    ///
    /// The two tables above both sit exactly on `b + s == 1`, HardMix's
    /// threshold, and both make ColorDodge and ColorBurn saturate. This sample
    /// lands strictly below the HardMix threshold, keeps dodge and divide off
    /// their clamps, and is the only one to reach soft light's `s > 0.5,
    /// b > 0.25` arm — the `sqrt` branch.
    #[test]
    fn separable_reference_values_off_every_threshold() {
        let b = 0.4f32;
        let s = 0.55f32;
        let cases: &[(BlendMode, f32)] = &[
            (BlendMode::Normal, 0.55),
            (BlendMode::Dissolve, 0.55),
            (BlendMode::Darken, 0.4),
            (BlendMode::Multiply, 0.22),
            // (1-0.4)/0.55 = 1.0909.. -> min(1, ..) = 1 -> 0
            (BlendMode::ColorBurn, 0.0),
            // 0.4 + 0.55 - 1 = -0.05, clamped
            (BlendMode::LinearBurn, 0.0),
            (BlendMode::Lighten, 0.55),
            // 0.4 + 0.55 - 0.22
            (BlendMode::Screen, 0.73),
            // min(1, 0.4/(1-0.55)) = 0.4/0.45 = 0.8888.. — unclamped, so the
            // dodge quotient itself is pinned here and nowhere else.
            (BlendMode::ColorDodge, 0.888_888_9),
            (BlendMode::LinearDodge, 0.95),
            // Overlay = HardLight(0.55, 0.4): source arg 0.4 <= 0.5 ->
            // Multiply(0.55, 0.8) = 0.44
            (BlendMode::Overlay, 0.44),
            // s > 0.5 and b > 0.25 -> D(b) = sqrt(0.4) = 0.632455..
            // b + (2s-1)*(D-b) = 0.4 + 0.1*0.232455.. = 0.4232455..
            (BlendMode::SoftLight, 0.423_245_55),
            // s > 0.5 -> Screen(0.4, 0.1) = 0.4 + 0.1 - 0.04
            (BlendMode::HardLight, 0.46),
            // s > 0.5 -> ColorDodge(0.4, 0.1) = min(1, 0.4/0.9) = 0.4444..
            (BlendMode::VividLight, 0.444_444_45),
            // 0.4 + 1.1 - 1
            (BlendMode::LinearLight, 0.5),
            // s > 0.5 -> max(0.4, 0.1)
            (BlendMode::PinLight, 0.4),
            // b + s = 0.95 < 1 -> 0, the arm neither table above reaches
            (BlendMode::HardMix, 0.0),
            (BlendMode::Difference, 0.15),
            // 0.4 + 0.55 - 2*0.22
            (BlendMode::Exclusion, 0.51),
            // 0.4 - 0.55 clamped
            (BlendMode::Subtract, 0.0),
            // 0.4/0.55 = 0.7272..
            (BlendMode::Divide, 0.727_272_75),
        ];
        assert_reference_table(b, s, cases);
    }

    #[test]
    fn hard_mix_thresholds_at_unit_sum() {
        assert_eq!(BlendMode::HardMix.blend_channel(0.4, 0.5), 0.0);
        assert_eq!(BlendMode::HardMix.blend_channel(0.5, 0.5), 1.0);
        assert_eq!(BlendMode::HardMix.blend_channel(0.6, 0.5), 1.0);
    }

    #[test]
    fn burn_and_dodge_edge_cases() {
        // Burn with a black source annihilates.
        assert_eq!(BlendMode::ColorBurn.blend_channel(0.5, 0.0), 0.0);
        // Burn on a white base stays white.
        assert_eq!(BlendMode::ColorBurn.blend_channel(1.0, 0.5), 1.0);
        // Dodge on a black base stays black (W3C: Cb == 0 -> 0).
        assert_eq!(BlendMode::ColorDodge.blend_channel(0.0, 0.9), 0.0);
        // Dodge with a white source blows out.
        assert_eq!(BlendMode::ColorDodge.blend_channel(0.5, 1.0), 1.0);
        // Divide by zero saturates rather than producing inf/NaN.
        let d = BlendMode::Divide.blend_channel(0.5, 0.0);
        assert!(d.is_finite() && d == 1.0);
    }

    #[test]
    fn results_are_always_clamped_and_finite() {
        // In-range values plus the hostile ones an HDR/float pipeline or a
        // corrupt tile can hand us: out of gamut, huge, and non-finite.
        let samples = [
            0.0f32,
            0.001,
            0.25,
            0.5,
            0.75,
            0.999,
            1.0,
            -0.5,
            1.5,
            -1e30,
            1e30,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        for m in BlendMode::ALL {
            for &b in &samples {
                for &s in &samples {
                    let v = m.blend_channel(b, s);
                    assert!(
                        v.is_finite() && (0.0..=1.0).contains(&v),
                        "{:?}({b}, {s}) = {v}",
                        m
                    );
                    for c in m.blend_rgb([b, s, b], [s, b, s]) {
                        assert!(
                            c.is_finite() && (0.0..=1.0).contains(&c),
                            "{:?} blend_rgb produced {c}",
                            m
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_output_clamp_of_blend_rgb_is_load_bearing() {
        // `clip_color` restores gamut only up to float rounding: its rescale
        // computes `l + (c - l) * (1 - l) / d`, which lands one ulp outside
        // `0.0..=1.0` for some perfectly in-gamut inputs. These two colors were
        // found by an 8M-sample sweep of `set_lum(base, lum(src))` — exactly
        // the expression `blend_rgb`'s Luminosity arm evaluates.
        let base = [0.65027934, 0.63790095, 0.1418132];
        let src = [0.43634957, 0.2300853, 0.70300007];

        let raw = set_lum(base, lum(src));
        assert!(
            raw[2] < 0.0,
            "premise: the unclamped HSL arm is out of range, got {raw:?}"
        );

        let got = BlendMode::Luminosity.blend_rgb(base, src);
        assert_eq!(got[2], 0.0, "the clamp must absorb the undershoot");
        assert!(
            got.iter().all(|c| (0.0..=1.0).contains(c)),
            "blend_rgb leaked {got:?}"
        );
    }

    #[test]
    fn unit_maps_every_f32_into_range() {
        assert_eq!(unit(0.25), 0.25);
        assert_eq!(unit(-0.5), 0.0);
        assert_eq!(unit(1.5), 1.0);
        // A plain `clamp` would return NaN here, not a bound.
        assert_eq!(unit(f32::NAN), 0.0);
        assert_eq!(unit(f32::INFINITY), 0.0);
        assert_eq!(unit(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn blend_rgb_clamps_out_of_range_inputs_the_way_blend_channel_does() {
        // The exact cases that proved `blend_rgb` was not honouring the module
        // contract: the HSL arms returned `set_lum` output unbounded, and
        // DarkerColor / LighterColor returned an input triple verbatim.
        let hot_base = [1.5, -0.2, 0.3];
        let hot_src = [0.4, 2.0, 0.6];
        for m in BlendMode::ALL {
            let got = m.blend_rgb(hot_base, hot_src);
            for c in got {
                assert!(
                    c.is_finite() && (0.0..=1.0).contains(&c),
                    "{m:?} returned {got:?} for out-of-range input"
                );
            }
            // Clamping the inputs first must give the identical result, i.e.
            // the clamp happens on entry, not by mangling the math afterwards.
            let pre = |v: [f32; 3]| [unit(v[0]), unit(v[1]), unit(v[2])];
            assert_eq!(
                got,
                m.blend_rgb(pre(hot_base), pre(hot_src)),
                "{m:?} treats raw and pre-clamped input differently"
            );
        }
    }

    #[test]
    fn blend_rgb_never_propagates_a_non_finite_channel() {
        let nan_base = [f32::NAN, 0.5, 0.5];
        let inf_src = [0.5, f32::INFINITY, f32::NEG_INFINITY];
        for m in BlendMode::ALL {
            for (b, s) in [(nan_base, inf_src), (inf_src, nan_base)] {
                for c in m.blend_rgb(b, s) {
                    assert!(c.is_finite(), "{m:?} leaked a non-finite channel");
                }
            }
            assert!(m.blend_channel(f32::NAN, 0.5).is_finite());
            assert!(m.blend_channel(0.5, f32::NAN).is_finite());
        }
    }

    #[test]
    fn separability_classification() {
        let non_separable: Vec<BlendMode> = BlendMode::ALL
            .into_iter()
            .filter(|m| !m.is_separable())
            .collect();
        assert_eq!(
            non_separable,
            vec![
                BlendMode::DarkerColor,
                BlendMode::LighterColor,
                BlendMode::Hue,
                BlendMode::Saturation,
                BlendMode::Color,
                BlendMode::Luminosity,
            ]
        );
    }

    // ---- non-separable modes ------------------------------------------------

    const CB: [f32; 3] = [0.2, 0.6, 0.4];
    const CS: [f32; 3] = [0.8, 0.3, 0.5];

    #[test]
    fn lum_and_sat_helpers() {
        // 0.3*0.2 + 0.59*0.6 + 0.11*0.4
        assert!(close(lum(CB), 0.458));
        // 0.3*0.8 + 0.59*0.3 + 0.11*0.5
        assert!(close(lum(CS), 0.472));
        assert!(close(sat(CB), 0.4));
        assert!(close(sat(CS), 0.5));
    }

    #[test]
    fn luminosity_reference_value() {
        // SetLum(Cb, Lum(Cs)): d = 0.472 - 0.458 = 0.014, no clipping needed.
        let got = BlendMode::Luminosity.blend_rgb(CB, CS);
        assert!(close3(got, [0.214, 0.614, 0.414]), "got {got:?}");
    }

    #[test]
    fn color_reference_value() {
        // SetLum(Cs, Lum(Cb)): d = 0.458 - 0.472 = -0.014.
        let got = BlendMode::Color.blend_rgb(CB, CS);
        assert!(close3(got, [0.786, 0.286, 0.486]), "got {got:?}");
    }

    #[test]
    fn hue_reference_value() {
        // SetSat(Cs, 0.4) = (0.4, 0.0, 0.16); Lum of that = 0.1376;
        // d = 0.458 - 0.1376 = 0.3204.
        let got = BlendMode::Hue.blend_rgb(CB, CS);
        assert!(close3(got, [0.7204, 0.3204, 0.4804]), "got {got:?}");
    }

    /// Base color for the Saturation reference, chosen so the test can see the
    /// mid-channel term of `set_sat`.
    ///
    /// `CB`'s middle channel (0.4) sits exactly halfway between its min (0.2)
    /// and max (0.6), which makes `(mid - min)` and `(max - mid)` equal — the
    /// numerator of `set_sat`'s mid term could be inverted and `CB` would not
    /// notice. Here min = 0.1, mid = 0.3, max = 0.7: the two differences are
    /// 0.2 and 0.4, so only the correct numerator reproduces the values below.
    const CB_ASYM: [f32; 3] = [0.1, 0.7, 0.3];

    #[test]
    fn set_sat_pins_the_mid_channel_term() {
        // Sanity: the base really is asymmetric, or the test below proves less
        // than it claims.
        let (min, mid, max) = (CB_ASYM[0], CB_ASYM[2], CB_ASYM[1]);
        assert!(min < mid && mid < max);
        assert!(
            ((mid - min) - (max - mid)).abs() > 0.1,
            "the middle channel must not be equidistant"
        );

        // out[mid] = (mid - min) * s / (max - min) = 0.2 * 0.5 / 0.6 = 1/6.
        // Inverting the numerator to (max - mid) would give 0.4*0.5/0.6 = 1/3.
        let got = set_sat(CB_ASYM, 0.5);
        assert!(close3(got, [0.0, 0.5, 1.0 / 6.0]), "got {got:?}");
        assert!(
            close(sat(got), 0.5),
            "SetSat must produce the requested span"
        );
    }

    #[test]
    fn saturation_reference_value() {
        // Sat(Cs) = 0.8 - 0.3 = 0.5.
        // SetSat(CB_ASYM, 0.5) = (0.0, 0.5, 1/6)   [see set_sat_pins_...]
        //   Lum of that = 0.59*0.5 + 0.11/6 = 0.295 + 0.0183333 = 0.3133333
        // Lum(CB_ASYM) = 0.3*0.1 + 0.59*0.7 + 0.11*0.3 = 0.476
        //   d = 0.476 - 0.3133333 = 0.1626667, and nothing leaves gamut.
        let got = BlendMode::Saturation.blend_rgb(CB_ASYM, CS);
        assert!(
            close3(got, [0.1626667, 0.6626667, 0.3293333]),
            "got {got:?}"
        );
        // The mode's contract, restated independently of the arithmetic above.
        assert!(close(lum(got), lum(CB_ASYM)));
        assert!(close(sat(got), sat(CS)));
        assert!(
            !close3(got, CB_ASYM),
            "the base must actually change, or the test would pass for an identity"
        );
    }

    #[test]
    fn non_separable_modes_preserve_the_component_they_claim_to() {
        // Luminosity takes Cs's luminosity and Cb's color.
        let l = BlendMode::Luminosity.blend_rgb(CB, CS);
        assert!(close(lum(l), lum(CS)), "luminosity lum mismatch");
        assert!(close(sat(l), sat(CB)), "luminosity should keep Cb's sat");

        // Color takes Cb's luminosity and Cs's color.
        let c = BlendMode::Color.blend_rgb(CB, CS);
        assert!(close(lum(c), lum(CB)), "color lum mismatch");
        assert!(close(sat(c), sat(CS)), "color should keep Cs's sat");

        // Hue takes Cb's luminosity and saturation, Cs's hue.
        let h = BlendMode::Hue.blend_rgb(CB, CS);
        assert!(close(lum(h), lum(CB)), "hue lum mismatch");
        assert!(close(sat(h), sat(CB)), "hue sat mismatch");

        // Saturation takes Cb's luminosity and hue, Cs's saturation.
        let s = BlendMode::Saturation.blend_rgb(CB, CS);
        assert!(close(lum(s), lum(CB)), "saturation lum mismatch");
        assert!(close(sat(s), sat(CS)), "saturation sat mismatch");
    }

    #[test]
    fn set_lum_clips_high_out_of_gamut_toward_white() {
        // Pushing a saturated base to full luminosity must clip to white, not
        // wrap or exceed 1.
        let got = BlendMode::Luminosity.blend_rgb([0.9, 0.9, 0.1], [1.0, 1.0, 1.0]);
        assert!(close3(got, [1.0, 1.0, 1.0]), "got {got:?}");
    }

    #[test]
    fn set_lum_clips_low_out_of_gamut_toward_black() {
        let got = BlendMode::Luminosity.blend_rgb([0.9, 0.1, 0.1], [0.0, 0.0, 0.0]);
        assert!(close3(got, [0.0, 0.0, 0.0]), "got {got:?}");
    }

    #[test]
    fn set_sat_collapses_a_flat_color() {
        // A gray source has no hue to donate: Hue(gray base, gray src) is gray.
        let got = BlendMode::Hue.blend_rgb([0.5, 0.5, 0.5], [0.2, 0.2, 0.2]);
        assert!(close3(got, [0.5, 0.5, 0.5]), "got {got:?}");
    }

    #[test]
    fn darker_and_lighter_color_pick_a_whole_color() {
        // Per-channel min would give [0.2, 0.3, 0.4]; the real mode returns one
        // of the two inputs unchanged.
        let d = BlendMode::DarkerColor.blend_rgb(CB, CS);
        assert_eq!(d, CB, "Cb has the lower luminosity");
        let l = BlendMode::LighterColor.blend_rgb(CB, CS);
        assert_eq!(l, CS);
        assert_ne!(d, BlendMode::Darken.blend_rgb(CB, CS));
    }

    #[test]
    fn dissolve_color_is_normal_and_the_choice_is_alpha_domain() {
        assert_eq!(BlendMode::Dissolve.blend_channel(0.2, 0.9), 0.9);
        assert!(dissolve_keeps_source(0.5, 0.4));
        assert!(!dissolve_keeps_source(0.5, 0.6));
        assert!(!dissolve_keeps_source(0.0, 0.0));
        assert!(dissolve_keeps_source(1.0, 0.999));
    }
}
