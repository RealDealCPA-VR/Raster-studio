//! Filter dialogs, generated from a parameter schema.
//!
//! There is exactly one filter dialog. It is built from a [`FilterSpec`] —
//! a [`FilterId`], a list of [`OptionSpec`]s, and the function that runs the
//! filter — so adding a filter to [`FILTERS`] gives it a dialog, a live
//! preview, a preview toggle, range-checked fields and a confirm path with no
//! UI code at all. The schema type is `tools::OptionSpec`, the same one the
//! tool options bar is generated from, so the app has one vocabulary for "a
//! parameter" and not two.
//!
//! The preview runs the *real* filter function on a proxy buffer. There is no
//! second, approximate implementation to drift.
//!
//! # One filter taxonomy, not two
//!
//! A [`FilterSpec`] is keyed by [`crate::menu::FilterId`] — the same value the
//! Filter menu emits — and takes its display name and its group from that
//! value rather than restating them. This module used to carry a private
//! string id and a private `FilterGroup` enum that disagreed with the menu's
//! (it had no `Render` group), and a catalogue that reached 15 of the menu's
//! 41 entries, so most of the Filter menu opened nothing. Three tests now hold
//! the two together:
//!
//! * `every_filter_menu_entry_has_a_dialog` — every [`FilterId::ALL`] entry
//!   has a [`FilterSpec`], so no menu item has nothing behind it.
//! * `the_catalogue_is_exactly_the_menu` — and no more, so a spec cannot exist
//!   for something unreachable.
//! * `a_menu_label_promises_a_dialog_exactly_when_the_filter_has_parameters` —
//!   the trailing ellipsis in [`FilterId::label`] means what menu.rs says it
//!   means.

use std::collections::BTreeMap;

use design::tokens::{Radius, Space};
use egui::{vec2, Context, TextureHandle};
use filters::{blur, distort, noise, other, pixelate, render, sharpen, stylize};
use filters::{EdgeMode, FilterBuffer, Interpolation, Sampling};
use tools::{OptionKind, OptionSpec};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::color_edit::ColorEdit;
use super::color_picker::ScreenSampler;
use super::controls::{checkbox_row, combo, integer, numeric};
use super::{ids, sizes};

pub use crate::menu::{FilterGroup, FilterId};

/// One value a filter parameter can hold.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ParamValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    /// Index into the option's `choices`.
    Choice(usize),
    Color([f32; 4]),
}

/// A filter's parameters, always complete and always in range.
///
/// Built from a schema, and every write goes back through that schema, so a
/// value out of range or of the wrong kind cannot get in. That is what lets the
/// dialog hand the parameters straight to the filter without re-checking.
#[derive(Clone, PartialEq, Debug)]
pub struct FilterParams {
    schema: &'static [OptionSpec],
    values: BTreeMap<&'static str, ParamValue>,
}

impl FilterParams {
    /// Every parameter at its schema default.
    pub fn defaults(schema: &'static [OptionSpec]) -> Self {
        let values = schema
            .iter()
            .map(|spec| (spec.key, default_value(spec.kind)))
            .collect();
        Self { schema, values }
    }

    /// The schema these parameters belong to.
    pub fn schema(&self) -> &'static [OptionSpec] {
        self.schema
    }

    /// The specification for one key.
    pub fn spec(&self, key: &str) -> Option<&'static OptionSpec> {
        self.schema.iter().find(|s| s.key == key)
    }

    /// The raw value for `key`.
    pub fn get(&self, key: &str) -> Option<ParamValue> {
        self.values.get(key).copied()
    }

    /// Write `value` for `key`, clamped to the schema.
    ///
    /// Returns `false` — changing nothing — when the key is not in the schema
    /// or the value is of the wrong kind for it.
    pub fn set(&mut self, key: &str, value: ParamValue) -> bool {
        let Some(spec) = self.spec(key) else {
            return false;
        };
        let Some(clamped) = clamp_value(spec.kind, value) else {
            return false;
        };
        self.values.insert(spec.key, clamped);
        true
    }

    /// A key an `apply` closure is about to read must be in that filter's own
    /// schema.
    ///
    /// The readers below fall back to zero, `false` or black for a key that is
    /// not there, which is the right behaviour for a released build — a
    /// parameter that cannot be found is not worth killing the app over — and
    /// exactly the wrong behaviour while the catalogue is being written, where
    /// it turns a mistyped key into a filter that silently ignores one of its
    /// own controls. In a debug build it is a programmer error and says so;
    /// `every_apply_closure_reads_only_its_own_schema` runs every filter, so
    /// every key every closure reads goes past this line.
    fn expect_in_schema(&self, key: &str, kind: &str) {
        debug_assert!(
            self.spec(key).is_some(),
            "no {kind} parameter {key:?} in this filter's schema: {:?}",
            self.schema.iter().map(|s| s.key).collect::<Vec<_>>()
        );
    }

    /// A float parameter, or `0.0` when the key is not in the schema or holds
    /// a value of another kind.
    pub fn float(&self, key: &str) -> f32 {
        self.expect_in_schema(key, "float");
        match self.get(key) {
            Some(ParamValue::Float(v)) => v,
            _ => 0.0,
        }
    }

    /// An integer parameter.
    pub fn int(&self, key: &str) -> i32 {
        self.expect_in_schema(key, "integer");
        match self.get(key) {
            Some(ParamValue::Int(v)) => v,
            _ => 0,
        }
    }

    /// An integer parameter as a `u32`, negatives folded to zero.
    pub fn uint(&self, key: &str) -> u32 {
        self.int(key).max(0) as u32
    }

    /// A boolean parameter.
    pub fn flag(&self, key: &str) -> bool {
        self.expect_in_schema(key, "boolean");
        matches!(self.get(key), Some(ParamValue::Bool(true)))
    }

    /// A choice parameter, as its index.
    pub fn choice(&self, key: &str) -> usize {
        self.expect_in_schema(key, "choice");
        match self.get(key) {
            Some(ParamValue::Choice(v)) => v,
            _ => 0,
        }
    }

    /// A choice parameter mapped through `options`, saturating at the end.
    pub fn choose<T: Copy>(&self, key: &str, options: &[T]) -> Option<T> {
        options
            .get(self.choice(key).min(options.len().saturating_sub(1)))
            .copied()
    }

    /// A colour parameter as **straight** linear RGBA — the convention the
    /// colour picker hands back. Filters that want a premultiplied pixel say
    /// so and go through [`premultiplied`].
    pub fn color(&self, key: &str) -> [f32; 4] {
        self.expect_in_schema(key, "colour");
        match self.get(key) {
            Some(ParamValue::Color(c)) => c,
            _ => [0.0, 0.0, 0.0, 1.0],
        }
    }
}

/// Straight linear RGBA to premultiplied linear RGBA.
///
/// `filters` is premultiplied throughout; the colour picker is not. Exactly
/// one filter parameter needs the conversion — `pointillize`'s background,
/// which is documented as a premultiplied pixel — and doing it in the wrong
/// direction there paints a background that is too bright wherever the colour
/// is not opaque.
fn premultiplied(color: [f32; 4]) -> [f32; 4] {
    let a = color[3].clamp(0.0, 1.0);
    [color[0] * a, color[1] * a, color[2] * a, a]
}

/// Everything the generated dialog needs to know about a filter.
#[derive(Clone, Copy)]
pub struct FilterSpec {
    /// Which Filter-menu entry this dialog is for. The menu item and the
    /// dialog are the same value, so neither can drift from the other.
    pub id: FilterId,
    /// One line saying what the filter does, shown under the title.
    pub summary: &'static str,
    pub params: &'static [OptionSpec],
    /// The real filter, applied to a buffer with these parameters.
    pub apply: fn(&FilterBuffer, &FilterParams) -> FilterBuffer,
}

impl FilterSpec {
    /// The dialog's title: the menu label without the ellipsis that promises a
    /// dialog. Derived rather than restated, so the menu entry the user picked
    /// and the window that opens cannot disagree.
    pub fn name(&self) -> &'static str {
        self.id.label().trim_end_matches('…')
    }

    /// Which submenu the filter lives under.
    pub const fn group(&self) -> FilterGroup {
        self.id.group()
    }
}

impl std::fmt::Debug for FilterSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterSpec")
            .field("id", &self.id)
            .field("group", &self.group())
            .finish_non_exhaustive()
    }
}

impl PartialEq for FilterSpec {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

const EDGE_MODES: &[&str] = &["Clamp", "Wrap", "Mirror"];

fn edge_mode(params: &FilterParams, key: &str) -> EdgeMode {
    params
        .choose(key, &[EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror])
        .unwrap_or(EdgeMode::Clamp)
}

/// The resampling a distort filter should use: the user's edge mode, and the
/// crate's default reconstruction filter.
fn sampling(params: &FilterParams, key: &str) -> Sampling {
    Sampling::new(edge_mode(params, key), Interpolation::default())
}

const fn float(
    key: &'static str,
    label: &'static str,
    min: f32,
    max: f32,
    default: f32,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Float { min, max, default },
    }
}

const fn int(
    key: &'static str,
    label: &'static str,
    min: i32,
    max: i32,
    default: i32,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Int { min, max, default },
    }
}

const fn flag(key: &'static str, label: &'static str, default: bool) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Bool { default },
    }
}

const fn choice(
    key: &'static str,
    label: &'static str,
    choices: &'static [&'static str],
    default: usize,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Choice { choices, default },
    }
}

const fn color(key: &'static str, label: &'static str, default: [f32; 4]) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Color { default },
    }
}

const NOISE_DISTRIBUTIONS: &[&str] = &["Uniform", "Gaussian"];
const RADIAL_KINDS: &[&str] = &["Spin", "Zoom"];
const POLAR_MODES: &[&str] = &["Rectangular to polar", "Polar to rectangular"];
const WAVE_KINDS: &[&str] = &["Sine", "Triangle", "Square"];
const ZIGZAG_KINDS: &[&str] = &["Pond ripples", "Out from centre", "Around centre"];
const GRADIENT_KINDS: &[&str] = &["Linear", "Radial", "Angle", "Reflected", "Diamond"];
const DIFFUSE_MODES: &[&str] = &["Normal", "Darken only", "Lighten only"];
const WIND_DIRECTIONS: &[&str] = &["From the left", "From the right"];

const GAUSSIAN: &[OptionSpec] = &[
    float("radius", "Radius", 0.0, 250.0, 4.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const BOX_BLUR: &[OptionSpec] = &[
    int("radius", "Radius", 0, 250, 4),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const LENS_BLUR: &[OptionSpec] = &[
    float("radius", "Radius", 0.5, 64.0, 8.0),
    int("blades", "Blades", 3, 12, 6),
    float("rotation", "Blade rotation", -180.0, 180.0, 0.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const MOTION_BLUR: &[OptionSpec] = &[
    float("angle", "Angle", -180.0, 180.0, 0.0),
    float("distance", "Distance", 1.0, 500.0, 16.0),
];
const RADIAL_BLUR: &[OptionSpec] = &[
    choice("kind", "Method", RADIAL_KINDS, 0),
    float("amount", "Amount", 1.0, 100.0, 10.0),
    int("samples", "Quality", 1, 256, 16),
];
const SURFACE_BLUR: &[OptionSpec] = &[
    int("radius", "Radius", 1, 64, 5),
    float("threshold", "Threshold", 0.001, 1.0, 0.05),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const UNSHARP: &[OptionSpec] = &[
    float("amount", "Amount", 0.0, 5.0, 1.0),
    float("radius", "Radius", 0.1, 100.0, 2.0),
    float("threshold", "Threshold", 0.0, 1.0, 0.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const SMART_SHARPEN: &[OptionSpec] = &[
    float("amount", "Amount", 0.0, 5.0, 1.0),
    float("radius", "Radius", 0.1, 64.0, 2.0),
    float("noise_floor", "Noise floor", 0.0, 1.0, 0.02),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const MEDIAN: &[OptionSpec] = &[
    int("radius", "Radius", 1, 64, 2),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const ADD_NOISE: &[OptionSpec] = &[
    float("amount", "Amount", 0.0, 1.0, 0.1),
    choice("distribution", "Distribution", NOISE_DISTRIBUTIONS, 0),
    flag("monochromatic", "Monochromatic", false),
    int("seed", "Seed", 0, 9999, 1),
];
const DUST_AND_SCRATCHES: &[OptionSpec] = &[
    int("radius", "Radius", 1, 16, 2),
    float("threshold", "Threshold", 0.001, 1.0, 0.05),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const REDUCE_NOISE: &[OptionSpec] = &[
    float("strength", "Strength", 0.1, 16.0, 4.0),
    float("detail", "Preserve detail", 0.0, 1.0, 0.5),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const TWIRL: &[OptionSpec] = &[
    float("radius", "Radius", 1.0, 4096.0, 128.0),
    float("angle", "Angle", -720.0, 720.0, 90.0),
];
const PINCH: &[OptionSpec] = &[
    float("radius", "Radius", 1.0, 4096.0, 128.0),
    float("amount", "Amount", -0.99, 0.99, 0.5),
];
const SPHERIZE: &[OptionSpec] = &[
    float("radius", "Radius", 1.0, 4096.0, 128.0),
    float("amount", "Amount", -1.0, 1.0, 0.5),
];
const POLAR: &[OptionSpec] = &[
    choice("mode", "Mapping", POLAR_MODES, 0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const RIPPLE: &[OptionSpec] = &[
    float("amount", "Amount", -100.0, 100.0, 8.0),
    float("wavelength", "Wavelength", 1.0, 512.0, 40.0),
];
const SHEAR: &[OptionSpec] = &[
    float("x", "Horizontal", -2.0, 2.0, 0.2),
    float("y", "Vertical", -2.0, 2.0, 0.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const WAVE: &[OptionSpec] = &[
    choice("kind", "Waveform", WAVE_KINDS, 0),
    float("amplitude", "Amplitude", 0.0, 256.0, 8.0),
    float("wavelength", "Wavelength", 1.0, 512.0, 40.0),
    float("phase", "Phase", -360.0, 360.0, 0.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const ZIGZAG: &[OptionSpec] = &[
    choice("kind", "Style", ZIGZAG_KINDS, 0),
    float("radius", "Radius", 1.0, 4096.0, 128.0),
    float("amount", "Amount", -256.0, 256.0, 8.0),
    float("ridges", "Ridges", 1.0, 64.0, 5.0),
];
const COLOR_HALFTONE: &[OptionSpec] = &[
    float("radius", "Max radius", 1.0, 64.0, 4.0),
    float("angle_r", "Red screen angle", -360.0, 360.0, 108.0),
    float("angle_g", "Green screen angle", -360.0, 360.0, 162.0),
    float("angle_b", "Blue screen angle", -360.0, 360.0, 90.0),
];
const CRYSTALLIZE: &[OptionSpec] = &[
    int("cell", "Cell size", 2, 512, 16),
    int("seed", "Seed", 0, 9999, 1),
];
const MOSAIC: &[OptionSpec] = &[int("cell", "Cell size", 1, 512, 8)];
const POINTILLIZE: &[OptionSpec] = &[
    int("cell", "Cell size", 2, 512, 16),
    int("seed", "Seed", 0, 9999, 1),
    color("background", "Background", [1.0, 1.0, 1.0, 1.0]),
];
const FIBERS: &[OptionSpec] = &[
    float("variance", "Variance", 0.0, 1.0, 0.35),
    float("strength", "Strength", 0.0, 1.0, 0.5),
    int("seed", "Seed", 0, 9999, 1),
    color("from", "Fibre colour", [0.02, 0.02, 0.02, 1.0]),
    color("to", "Backing colour", [0.9, 0.9, 0.9, 1.0]),
];
const GRADIENT_FILL: &[OptionSpec] = &[
    choice("kind", "Style", GRADIENT_KINDS, 0),
    float("angle", "Angle", -180.0, 180.0, 0.0),
    color("from", "From", [0.0, 0.0, 0.0, 1.0]),
    color("to", "To", [1.0, 1.0, 1.0, 1.0]),
];
const LENS_FLARE: &[OptionSpec] = &[
    float("brightness", "Brightness", 0.0, 4.0, 1.0),
    float("radius", "Radius", 1.0, 512.0, 120.0),
    int("ghosts", "Ghosts", 0, 16, 5),
    int("streaks", "Streaks", 0, 32, 6),
];
const DIFFUSE: &[OptionSpec] = &[
    int("radius", "Radius", 1, 64, 4),
    choice("mode", "Mode", DIFFUSE_MODES, 0),
    int("seed", "Seed", 0, 9999, 1),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const EMBOSS: &[OptionSpec] = &[
    float("angle", "Angle", -180.0, 180.0, 135.0),
    float("height", "Height", 0.1, 100.0, 3.0),
    float("amount", "Amount", 0.0, 10.0, 1.0),
];
const OIL_PAINT: &[OptionSpec] = &[
    int("radius", "Radius", 1, 64, 4),
    int("levels", "Levels", 2, 256, 32),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const WIND: &[OptionSpec] = &[
    choice("direction", "Direction", WIND_DIRECTIONS, 0),
    float("strength", "Strength", 0.0, 1.0, 0.5),
    int("seed", "Seed", 0, 9999, 1),
    choice("edge", "Edges", EDGE_MODES, 0),
];
/// The nine taps of the Custom filter's 3x3 kernel, plus its divisor and bias.
///
/// Three by three rather than five by five because every weight is a field the
/// user has to fill in, and twenty-five of them is a spreadsheet rather than a
/// dialog. A divisor of zero means "the sum of the weights", which is how
/// convolution kernels are conventionally tabulated and what
/// `filters::other::Kernel::new` does on its own.
const CUSTOM: &[OptionSpec] = &[
    float("w00", "Top left", -99.0, 99.0, 0.0),
    float("w01", "Top", -99.0, 99.0, 0.0),
    float("w02", "Top right", -99.0, 99.0, 0.0),
    float("w10", "Left", -99.0, 99.0, 0.0),
    float("w11", "Centre", -99.0, 99.0, 1.0),
    float("w12", "Right", -99.0, 99.0, 0.0),
    float("w20", "Bottom left", -99.0, 99.0, 0.0),
    float("w21", "Bottom", -99.0, 99.0, 0.0),
    float("w22", "Bottom right", -99.0, 99.0, 0.0),
    float("divisor", "Divisor (0 = auto)", 0.0, 999.0, 0.0),
    float("bias", "Bias", -1.0, 1.0, 0.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const HIGH_PASS: &[OptionSpec] = &[
    float("radius", "Radius", 0.1, 250.0, 10.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const MORPHOLOGY: &[OptionSpec] = &[
    int("radius", "Radius", 1, 64, 2),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const OFFSET: &[OptionSpec] = &[
    int("dx", "Horizontal", -4096, 4096, 0),
    int("dy", "Vertical", -4096, 4096, 0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const NO_PARAMS: &[OptionSpec] = &[];

/// The Custom kernel's nine weights, in row-major order.
const CUSTOM_TAPS: &[&str] = &[
    "w00", "w01", "w02", "w10", "w11", "w12", "w20", "w21", "w22",
];

/// A gradient's start and end points for `angle` degrees across `w` x `h`.
///
/// Measured from the centre, because every style the `filters` crate offers —
/// linear, radial, angle, reflected and diamond — is defined *about* `start`,
/// so anchoring the start at a corner would put four of the five styles in the
/// corner too.
fn gradient_axis(w: u32, h: u32, angle_deg: f32) -> ((f32, f32), (f32, f32)) {
    let (cx, cy) = distort::center_of(w, h);
    let half = 0.5 * (f64::from(w) * f64::from(w) + f64::from(h) * f64::from(h)).sqrt() as f32;
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    ((cx, cy), (cx + cos * half, cy + sin * half))
}

/// Every filter that has a dialog: exactly the entries of [`FilterId::ALL`],
/// in the same order.
///
/// This list *is* the dialog set and the parameter contract for the Filter
/// menu. `every_filter_menu_entry_has_a_dialog` and
/// `the_catalogue_is_exactly_the_menu` hold the two together in both
/// directions, so neither a menu item with nothing behind it nor a dialog
/// nothing can open can survive.
pub const FILTERS: &[FilterSpec] = &[
    FilterSpec {
        id: FilterId::BoxBlur,
        summary: "A flat average over a square window.",
        params: BOX_BLUR,
        apply: |src, p| blur::box_blur(src, p.uint("radius"), edge_mode(p, "edge")),
    },
    FilterSpec {
        id: FilterId::GaussianBlur,
        summary: "A smooth, isotropic blur.",
        params: GAUSSIAN,
        apply: |src, p| blur::gaussian_blur(src, p.float("radius"), edge_mode(p, "edge")),
    },
    FilterSpec {
        id: FilterId::LensBlur,
        summary: "Blurs through a polygonal iris, so highlights bloom into its shape.",
        params: LENS_BLUR,
        apply: |src, p| {
            blur::lens_blur(
                src,
                p.float("radius"),
                p.uint("blades"),
                p.float("rotation"),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::MotionBlur,
        summary: "Smears the image along one direction.",
        params: MOTION_BLUR,
        apply: |src, p| {
            blur::motion_blur(
                src,
                p.float("angle"),
                p.float("distance"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: FilterId::RadialBlur,
        summary: "Spins or zooms the image about its centre. Amount is degrees, or per cent.",
        params: RADIAL_BLUR,
        apply: |src, p| {
            let (w, h) = src.dimensions();
            let kind = p
                .choose(
                    "kind",
                    &[blur::RadialBlurKind::Spin, blur::RadialBlurKind::Zoom],
                )
                .unwrap_or(blur::RadialBlurKind::Spin);
            let amount = p.float("amount");
            blur::radial_blur(
                src,
                &blur::RadialBlur {
                    kind,
                    center: distort::center_of(w, h),
                    amount: match kind {
                        blur::RadialBlurKind::Spin => amount,
                        blur::RadialBlurKind::Zoom => amount / 100.0,
                    },
                    samples: p.uint("samples"),
                    sampling: Sampling::clamped(),
                },
            )
        },
    },
    FilterSpec {
        id: FilterId::SurfaceBlur,
        summary: "Blurs flat areas and leaves edges alone.",
        params: SURFACE_BLUR,
        apply: |src, p| {
            blur::surface_blur(
                src,
                p.uint("radius"),
                p.float("threshold"),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::SmartSharpen,
        summary: "Unsharp masking that leaves flat, noisy areas alone.",
        params: SMART_SHARPEN,
        apply: |src, p| {
            sharpen::smart_sharpen(
                src,
                p.float("amount"),
                p.float("radius"),
                p.float("noise_floor"),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::UnsharpMask,
        summary: "Adds back the difference between the image and a blur of it.",
        params: UNSHARP,
        apply: |src, p| {
            sharpen::unsharp_mask(
                src,
                p.float("amount"),
                p.float("radius"),
                p.float("threshold"),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::AddNoise,
        summary: "Deterministic grain: the same seed gives the same grain.",
        params: ADD_NOISE,
        apply: |src, p| {
            noise::add_noise(
                src,
                p.float("amount"),
                p.choose(
                    "distribution",
                    &[
                        noise::NoiseDistribution::Uniform,
                        noise::NoiseDistribution::Gaussian,
                    ],
                )
                .unwrap_or_default(),
                p.flag("monochromatic"),
                p.uint("seed") as u64,
            )
        },
    },
    FilterSpec {
        id: FilterId::Despeckle,
        summary: "A fixed small median that removes isolated speckles.",
        params: NO_PARAMS,
        apply: |src, _| noise::despeckle(src, EdgeMode::Clamp),
    },
    FilterSpec {
        id: FilterId::DustAndScratches,
        summary: "Replaces pixels that differ from their neighbourhood median.",
        params: DUST_AND_SCRATCHES,
        apply: |src, p| {
            noise::dust_and_scratches(
                src,
                p.uint("radius"),
                p.float("threshold"),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::Median,
        summary: "Replaces each pixel with the median of its neighbourhood.",
        params: MEDIAN,
        apply: |src, p| noise::median(src, p.uint("radius"), edge_mode(p, "edge")),
    },
    FilterSpec {
        id: FilterId::ReduceNoise,
        summary: "An edge-preserving smooth: strength blurs, detail holds edges back.",
        params: REDUCE_NOISE,
        apply: |src, p| {
            noise::reduce_noise(
                src,
                p.float("strength"),
                p.float("detail"),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::Pinch,
        summary: "Pulls the image toward the centre, or pushes it out.",
        params: PINCH,
        apply: |src, p| {
            distort::pinch(
                src,
                distort::center_of(src.dimensions().0, src.dimensions().1),
                p.float("radius"),
                p.float("amount"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: FilterId::PolarCoordinates,
        summary: "Wraps the image into a disc, or unrolls a disc into a rectangle.",
        params: POLAR,
        apply: |src, p| {
            distort::polar_coordinates(
                src,
                p.choose(
                    "mode",
                    &[
                        distort::PolarMode::RectangularToPolar,
                        distort::PolarMode::PolarToRectangular,
                    ],
                )
                .unwrap_or(distort::PolarMode::RectangularToPolar),
                sampling(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::Ripple,
        summary: "A small sinusoidal displacement, as through water.",
        params: RIPPLE,
        apply: |src, p| {
            distort::ripple(
                src,
                p.float("amount"),
                p.float("wavelength"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: FilterId::Shear,
        summary: "Slants the image about its centre.",
        params: SHEAR,
        apply: |src, p| distort::shear(src, p.float("x"), p.float("y"), sampling(p, "edge")),
    },
    FilterSpec {
        id: FilterId::Spherize,
        summary: "Wraps the image onto a sphere.",
        params: SPHERIZE,
        apply: |src, p| {
            distort::spherize(
                src,
                distort::center_of(src.dimensions().0, src.dimensions().1),
                p.float("radius"),
                p.float("amount"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: FilterId::Twirl,
        summary: "Rotates the image about its centre, falling off with radius.",
        params: TWIRL,
        apply: |src, p| {
            distort::twirl(
                src,
                distort::center_of(src.dimensions().0, src.dimensions().1),
                p.float("radius"),
                p.float("angle"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: FilterId::Wave,
        summary: "Displaces the image along a repeating waveform.",
        params: WAVE,
        apply: |src, p| {
            distort::wave(
                src,
                &distort::Wave {
                    kind: p
                        .choose(
                            "kind",
                            &[
                                distort::WaveKind::Sine,
                                distort::WaveKind::Triangle,
                                distort::WaveKind::Square,
                            ],
                        )
                        .unwrap_or_default(),
                    amplitude: p.float("amplitude"),
                    wavelength: p.float("wavelength"),
                    phase_deg: p.float("phase"),
                },
                sampling(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::ZigZag,
        summary: "Concentric ripples about the centre.",
        params: ZIGZAG,
        apply: |src, p| {
            let (w, h) = src.dimensions();
            distort::zigzag(
                src,
                &distort::ZigZag {
                    kind: p
                        .choose(
                            "kind",
                            &[
                                distort::ZigZagKind::PondRipples,
                                distort::ZigZagKind::OutFromCenter,
                                distort::ZigZagKind::AroundCenter,
                            ],
                        )
                        .unwrap_or_default(),
                    center: distort::center_of(w, h),
                    radius: p.float("radius"),
                    amount: p.float("amount"),
                    ridges: p.float("ridges"),
                },
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: FilterId::ColorHalftone,
        summary: "Screens each channel into rotated dots, as in process printing.",
        params: COLOR_HALFTONE,
        apply: |src, p| {
            pixelate::color_halftone(
                src,
                p.float("radius"),
                [p.float("angle_r"), p.float("angle_g"), p.float("angle_b")],
            )
        },
    },
    FilterSpec {
        id: FilterId::Crystallize,
        summary: "Flattens the image into irregular polygonal facets.",
        params: CRYSTALLIZE,
        apply: |src, p| pixelate::crystallize(src, p.uint("cell"), u64::from(p.uint("seed"))),
    },
    FilterSpec {
        id: FilterId::Mosaic,
        summary: "Averages the image into square cells.",
        params: MOSAIC,
        apply: |src, p| pixelate::mosaic(src, p.uint("cell")),
    },
    FilterSpec {
        id: FilterId::Pointillize,
        summary: "Stipples the image into dots over a flat background.",
        params: POINTILLIZE,
        apply: |src, p| {
            pixelate::pointillize(
                src,
                p.uint("cell"),
                u64::from(p.uint("seed")),
                premultiplied(p.color("background")),
            )
        },
    },
    FilterSpec {
        id: FilterId::Clouds,
        summary: "Fills the layer with fractal Perlin cloud.",
        params: NO_PARAMS,
        apply: |src, _| {
            let (w, h) = src.dimensions();
            render::clouds(w, h, &render::CloudParams::default()).unwrap_or_else(|_| src.clone())
        },
    },
    FilterSpec {
        id: FilterId::DifferenceClouds,
        summary: "Blends fractal cloud into the layer by absolute difference.",
        params: NO_PARAMS,
        apply: |src, _| render::difference_clouds(src, &render::CloudParams::default()),
    },
    FilterSpec {
        id: FilterId::Fibers,
        summary: "Fills the layer with vertical fibres, as in woven material.",
        params: FIBERS,
        apply: |src, p| {
            let (w, h) = src.dimensions();
            render::fibers(
                w,
                h,
                &render::FiberParams {
                    seed: u64::from(p.uint("seed")),
                    variance: p.float("variance"),
                    strength: p.float("strength"),
                    color_a: p.color("from"),
                    color_b: p.color("to"),
                },
            )
            .unwrap_or_else(|_| src.clone())
        },
    },
    FilterSpec {
        id: FilterId::GradientFill,
        summary: "Fills the layer with a two-stop gradient.",
        params: GRADIENT_FILL,
        apply: |src, p| {
            let (w, h) = src.dimensions();
            let (start, end) = gradient_axis(w, h, p.float("angle"));
            let kind = p
                .choose(
                    "kind",
                    &[
                        render::GradientKind::Linear,
                        render::GradientKind::Radial,
                        render::GradientKind::Angle,
                        render::GradientKind::Reflected,
                        render::GradientKind::Diamond,
                    ],
                )
                .unwrap_or_default();
            let gradient =
                render::Gradient::two_stop(kind, start, end, p.color("from"), p.color("to"));
            render::gradient_fill(w, h, &gradient).unwrap_or_else(|_| src.clone())
        },
    },
    FilterSpec {
        id: FilterId::LensFlare,
        summary: "Adds an emissive core, streaks and ghost reflections.",
        params: LENS_FLARE,
        apply: |src, p| {
            let (w, h) = src.dimensions();
            render::lens_flare(
                src,
                &render::LensFlare {
                    center: distort::center_of(w, h),
                    brightness: p.float("brightness"),
                    radius: p.float("radius"),
                    ghosts: p.uint("ghosts"),
                    streaks: p.uint("streaks"),
                },
            )
        },
    },
    FilterSpec {
        id: FilterId::Diffuse,
        summary: "Shuffles each pixel with a random neighbour.",
        params: DIFFUSE,
        apply: |src, p| {
            stylize::diffuse(
                src,
                p.uint("radius"),
                p.choose(
                    "mode",
                    &[
                        stylize::DiffuseMode::Normal,
                        stylize::DiffuseMode::DarkenOnly,
                        stylize::DiffuseMode::LightenOnly,
                    ],
                )
                .unwrap_or_default(),
                u64::from(p.uint("seed")),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::Emboss,
        summary: "Lights the image from one side and keeps only the relief.",
        params: EMBOSS,
        apply: |src, p| {
            stylize::emboss(
                src,
                p.float("angle"),
                p.float("height"),
                p.float("amount"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: FilterId::FindEdges,
        summary: "Keeps the gradient magnitude and throws away the flat areas.",
        params: NO_PARAMS,
        apply: |src, _| stylize::find_edges(src, EdgeMode::Clamp),
    },
    FilterSpec {
        id: FilterId::OilPaint,
        summary: "Replaces each pixel with the most common tone nearby.",
        params: OIL_PAINT,
        apply: |src, p| {
            stylize::oil_paint(
                src,
                p.uint("radius"),
                p.uint("levels"),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::Solarize,
        summary: "Inverts the tones above mid grey.",
        params: NO_PARAMS,
        apply: |src, _| stylize::solarize(src),
    },
    FilterSpec {
        id: FilterId::Wind,
        summary: "Horizontal streaks trailing away from vertical edges.",
        params: WIND,
        apply: |src, p| {
            stylize::wind(
                src,
                p.choose(
                    "direction",
                    &[
                        stylize::WindDirection::FromLeft,
                        stylize::WindDirection::FromRight,
                    ],
                )
                .unwrap_or_default(),
                p.float("strength"),
                u64::from(p.uint("seed")),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::Custom,
        summary: "Your own 3x3 convolution kernel.",
        params: CUSTOM,
        apply: |src, p| {
            let weights: Vec<f32> = CUSTOM_TAPS.iter().map(|key| p.float(key)).collect();
            let Ok(kernel) = other::Kernel::new(3, weights) else {
                return src.clone();
            };
            let divisor = p.float("divisor");
            let kernel = if divisor == 0.0 {
                kernel
            } else {
                kernel.with_divisor(divisor)
            };
            other::convolve(
                src,
                &kernel.with_bias(p.float("bias")),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: FilterId::HighPass,
        summary: "Keeps only the detail finer than the radius.",
        params: HIGH_PASS,
        apply: |src, p| other::high_pass(src, p.float("radius"), edge_mode(p, "edge")),
    },
    FilterSpec {
        id: FilterId::Maximum,
        summary: "Spreads the brightest pixel in the neighbourhood: a dilate.",
        params: MORPHOLOGY,
        apply: |src, p| other::maximum(src, p.uint("radius"), edge_mode(p, "edge")),
    },
    FilterSpec {
        id: FilterId::Minimum,
        summary: "Spreads the darkest pixel in the neighbourhood: an erode.",
        params: MORPHOLOGY,
        apply: |src, p| other::minimum(src, p.uint("radius"), edge_mode(p, "edge")),
    },
    FilterSpec {
        id: FilterId::Offset,
        summary: "Slides the layer's contents, with a choice of what fills behind.",
        params: OFFSET,
        apply: |src, p| {
            other::offset(
                src,
                i64::from(p.int("dx")),
                i64::from(p.int("dy")),
                edge_mode(p, "edge"),
            )
        },
    },
];

/// Look a filter's dialog up by the menu entry that opens it.
///
/// Total over [`FilterId::ALL`] — `every_filter_menu_entry_has_a_dialog` is
/// the test that keeps it that way.
pub fn filter_by_id(id: FilterId) -> Option<&'static FilterSpec> {
    FILTERS.iter().find(|f| f.id == id)
}

/// What the dialog commits to: which filter, with which parameters.
#[derive(Clone, PartialEq, Debug)]
pub struct FilterInvocation {
    pub filter: &'static FilterSpec,
    pub params: FilterParams,
}

impl FilterInvocation {
    /// Whether the invocation's parameters match its filter's schema.
    pub fn is_valid(&self) -> bool {
        self.params.schema().len() == self.filter.params.len()
            && self
                .filter
                .params
                .iter()
                .all(|spec| self.params.get(spec.key).is_some())
    }

    /// Run the filter.
    pub fn run(&self, src: &FilterBuffer) -> FilterBuffer {
        (self.filter.apply)(src, &self.params)
    }
}

/// Largest side of the buffer the live preview filters.
pub const MAX_PREVIEW_SIDE: u32 = 192;

/// The one filter dialog.
pub struct FilterDialog {
    spec: &'static FilterSpec,
    params: FilterParams,
    preview_enabled: bool,
    source: FilterBuffer,
    texture: Option<TextureHandle>,
    cached_for: Option<FilterParams>,
    /// The nested colour picker, keyed by the parameter it edits.
    color_edit: ColorEdit<&'static str>,
}

impl std::fmt::Debug for FilterDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterDialog")
            .field("spec", &self.spec)
            .field("params", &self.params)
            .field("preview_enabled", &self.preview_enabled)
            .finish_non_exhaustive()
    }
}

impl FilterDialog {
    /// Open `spec`'s dialog over `source`, a small proxy of the layer.
    pub fn new(spec: &'static FilterSpec, source: FilterBuffer) -> Self {
        Self {
            spec,
            params: FilterParams::defaults(spec.params),
            preview_enabled: true,
            source,
            texture: None,
            cached_for: None,
            color_edit: ColorEdit::new(),
        }
    }

    /// Open `spec`'s dialog over a generated proxy, for a caller that has no
    /// pixels yet.
    pub fn with_placeholder(spec: &'static FilterSpec) -> Self {
        Self::new(spec, placeholder_buffer(96, 96))
    }

    /// The filter this dialog is for.
    pub fn spec(&self) -> &'static FilterSpec {
        self.spec
    }

    /// The parameters as edited.
    pub fn params(&self) -> &FilterParams {
        &self.params
    }

    /// Write one parameter, clamped to the schema. Returns `false` when the
    /// key or the kind does not belong to this filter.
    pub fn set_param(&mut self, key: &str, value: ParamValue) -> bool {
        let changed = self.params.set(key, value);
        if changed {
            self.cached_for = None;
        }
        changed
    }

    /// Put every parameter back to its schema default.
    pub fn reset(&mut self) {
        self.params = FilterParams::defaults(self.spec.params);
        self.cached_for = None;
    }

    /// Whether the live preview is on.
    pub fn preview_enabled(&self) -> bool {
        self.preview_enabled
    }

    /// Turn the live preview on or off. Off means the filter is not run at all
    /// per frame, which is the point of the toggle on a large image.
    pub fn set_preview_enabled(&mut self, on: bool) {
        self.preview_enabled = on;
        if !on {
            self.texture = None;
            self.cached_for = None;
        }
    }

    /// Run the filter on the proxy and return the result.
    pub fn preview_buffer(&self) -> FilterBuffer {
        (self.spec.apply)(&self.source, &self.params)
    }

    /// The invocation the dialog would commit.
    pub fn invocation(&self) -> FilterInvocation {
        FilterInvocation {
            filter: self.spec,
            params: self.params.clone(),
        }
    }

    /// The nested colour picker, when a colour parameter's swatch is clicked.
    pub fn color_edit(&self) -> &ColorEdit<&'static str> {
        &self.color_edit
    }

    /// Mutable access to it.
    pub fn color_edit_mut(&mut self) -> &mut ColorEdit<&'static str> {
        &mut self.color_edit
    }

    /// Draw the dialog for one frame.
    ///
    /// `sampler` reaches the nested colour picker's eyedropper; `None` draws
    /// that button disabled with its reason.
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        let nested = self.color_edit.is_open();
        let keys = if nested {
            DialogKeys::NONE
        } else {
            DialogKeys::read(ctx)
        };
        let mut outcome = super::chrome::resolve(self, keys);
        self.refresh_preview(ctx);
        let drawn = modal(
            ctx,
            ("filter", self.spec.id),
            self.spec.name(),
            Some(self.spec.summary),
            DialogWidth::Standard,
            |ui| self.body(ui),
        );
        if let Some((key, rgba)) = self.color_edit.show(ctx, "filter-param-color", sampler) {
            self.set_param(key, ParamValue::Color(rgba));
        }
        if nested {
            return DialogOutcome::Open;
        }
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => {
                    self.reset();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn refresh_preview(&mut self, ctx: &Context) {
        if !self.preview_enabled {
            return;
        }
        if self.cached_for.as_ref() == Some(&self.params) && self.texture.is_some() {
            return;
        }
        let filtered = self.preview_buffer();
        let (width, height) = filtered.dimensions();
        if width == 0 || height == 0 {
            self.texture = None;
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            &filtered.to_rgba8(),
        );
        self.texture =
            Some(ctx.load_texture("filter-preview", image, egui::TextureOptions::LINEAR));
        self.cached_for = Some(self.params.clone());
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        let mut preview = self.preview_enabled;
        if checkbox_row(ui, "Preview", &mut preview).changed() {
            self.set_preview_enabled(preview);
        }
        match (&self.texture, self.preview_enabled) {
            (Some(texture), true) => {
                let size = texture.size_vec2();
                let scale = (sizes::filter_preview_width() / size.x.max(1.0)).min(2.0);
                ui.image((texture.id(), size * scale));
            }
            (_, true) => {
                caption(ui, "Nothing to preview.");
            }
            (_, false) => {
                let (w, h) = self.source.dimensions();
                let width = sizes::filter_preview_width();
                let (rect, _) = ui.allocate_exact_size(
                    vec2(width, width * h as f32 / w.max(1) as f32),
                    egui::Sense::hover(),
                );
                let radius = {
                    let t = design::current_tokens(ui);
                    Radius::Small.resolve(&t.radii, rect.height())
                };
                super::controls::checkerboard(ui, rect, radius);
                caption(ui, "Preview is off.");
            }
        }

        hairline(ui);
        if self.spec.params.is_empty() {
            caption(ui, "This filter has no settings.");
        }
        for spec in self.spec.params {
            self.param_row(ui, spec);
        }
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &["Reset"],
        )
    }

    /// One row of the generated form. This match *is* the code generator.
    fn param_row(&mut self, ui: &mut egui::Ui, spec: &'static OptionSpec) {
        match spec.kind {
            OptionKind::Float { min, max, .. } => {
                let mut value = f64::from(self.params.float(spec.key));
                design::inspector_field(ui, spec.label, |ui| {
                    if numeric(ui, &mut value, f64::from(min)..=f64::from(max), 3, "").changed() {
                        self.set_param(spec.key, ParamValue::Float(value as f32));
                    }
                });
            }
            OptionKind::Int { min, max, .. } => {
                let mut value = i64::from(self.params.int(spec.key));
                design::inspector_field(ui, spec.label, |ui| {
                    if integer(ui, &mut value, i64::from(min)..=i64::from(max)).changed() {
                        self.set_param(spec.key, ParamValue::Int(value as i32));
                    }
                });
            }
            OptionKind::Bool { .. } => {
                let mut value = self.params.flag(spec.key);
                if checkbox_row(ui, spec.label, &mut value).changed() {
                    self.set_param(spec.key, ParamValue::Bool(value));
                }
            }
            OptionKind::Choice { choices, .. } => {
                let mut index = self.params.choice(spec.key);
                let options: Vec<usize> = (0..choices.len()).collect();
                design::inspector_field(ui, spec.label, |ui| {
                    if combo(
                        ui,
                        ("filter-choice", spec.key),
                        &mut index,
                        &options,
                        |i| choices.get(i).copied().unwrap_or("").to_string(),
                        |_| None,
                    ) {
                        self.set_param(spec.key, ParamValue::Choice(index));
                    }
                });
            }
            OptionKind::Color { .. } => {
                let value = match self.params.get(spec.key) {
                    Some(ParamValue::Color(c)) => c,
                    _ => [0.0, 0.0, 0.0, 1.0],
                };
                let mut open_picker = false;
                design::inspector_field(ui, spec.label, |ui| {
                    open_picker = super::controls::swatch(
                        ui,
                        ids::filter_param_color(spec.key),
                        value,
                        sizes::swatch(),
                    )
                    .clicked();
                });
                if open_picker {
                    self.color_edit.open(spec.key, value);
                }
            }
        }
    }
}

/// A proxy buffer for a caller with no pixels: a ramp with a hard checker, so
/// blurs, edges and pixelation all have something to act on.
pub fn placeholder_buffer(width: u32, height: u32) -> FilterBuffer {
    let width = width.clamp(1, MAX_PREVIEW_SIDE);
    let height = height.clamp(1, MAX_PREVIEW_SIDE);
    let mut buffer = FilterBuffer::transparent(width, height).expect("a non-empty proxy");
    for y in 0..height {
        for x in 0..width {
            let ramp = x as f32 / width as f32;
            let checker = if ((x / 8) + (y / 8)) % 2 == 0 {
                1.0
            } else {
                0.2
            };
            buffer.set(x, y, [ramp, checker, 1.0 - ramp, 1.0]);
        }
    }
    buffer
}

impl Dialog for FilterDialog {
    fn title(&self) -> &'static str {
        "Filter"
    }

    fn confirm_label(&self) -> &'static str {
        "Apply"
    }

    fn confirm(&self) -> Option<DialogAction> {
        let invocation = self.invocation();
        invocation
            .is_valid()
            .then(|| DialogAction::RunFilter(Box::new(invocation)))
    }
}

fn default_value(kind: OptionKind) -> ParamValue {
    match kind {
        OptionKind::Float { default, min, max } => ParamValue::Float(default.clamp(min, max)),
        OptionKind::Int { default, min, max } => ParamValue::Int(default.clamp(min, max)),
        OptionKind::Bool { default } => ParamValue::Bool(default),
        OptionKind::Choice { choices, default } => {
            ParamValue::Choice(default.min(choices.len().saturating_sub(1)))
        }
        OptionKind::Color { default } => ParamValue::Color(default),
    }
}

/// Fit a value to a schema entry, or reject it as the wrong kind.
fn clamp_value(kind: OptionKind, value: ParamValue) -> Option<ParamValue> {
    match (kind, value) {
        (OptionKind::Float { min, max, .. }, ParamValue::Float(v)) => {
            Some(ParamValue::Float(if v.is_nan() {
                min
            } else {
                v.clamp(min, max)
            }))
        }
        (OptionKind::Int { min, max, .. }, ParamValue::Int(v)) => {
            Some(ParamValue::Int(v.clamp(min, max)))
        }
        (OptionKind::Bool { .. }, ParamValue::Bool(v)) => Some(ParamValue::Bool(v)),
        (OptionKind::Choice { choices, .. }, ParamValue::Choice(v)) => {
            Some(ParamValue::Choice(v.min(choices.len().saturating_sub(1))))
        }
        (OptionKind::Color { .. }, ParamValue::Color(c)) => Some(ParamValue::Color([
            c[0].clamp(0.0, 1.0),
            c[1].clamp(0.0, 1.0),
            c[2].clamp(0.0, 1.0),
            c[3].clamp(0.0, 1.0),
        ])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::{frame_both_themes, Harness};

    #[test]
    fn every_filter_has_a_unique_id_a_name_and_a_summary() {
        let mut ids: Vec<FilterId> = FILTERS.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "two filters share an id");
        for filter in FILTERS {
            assert!(!filter.name().is_empty(), "{:?} has no name", filter.id);
            assert!(
                !filter.name().ends_with('…'),
                "{:?}'s title kept the menu's ellipsis",
                filter.id
            );
            assert!(!filter.summary.is_empty(), "{:?} has no summary", filter.id);
            assert_eq!(filter_by_id(filter.id), Some(filter));
        }
    }

    /// The defect this pins: `FILTERS` held 15 specs keyed by a private string
    /// id while the Filter menu offered 41 entries, so 26 menu items opened
    /// nothing at all — and menu.rs's own doc comment claimed the opposite.
    /// Nothing connected the two lists, so nothing noticed.
    #[test]
    fn every_filter_menu_entry_has_a_dialog() {
        let missing: Vec<FilterId> = FilterId::ALL
            .iter()
            .copied()
            .filter(|id| filter_by_id(*id).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "Filter-menu entries with nothing behind them: {missing:?}"
        );
    }

    /// And the other direction: a spec for something the menu cannot reach is
    /// a dialog no user can open.
    #[test]
    fn the_catalogue_is_exactly_the_menu() {
        let catalogue: Vec<FilterId> = FILTERS.iter().map(|f| f.id).collect();
        assert_eq!(
            catalogue,
            FilterId::ALL.to_vec(),
            "the catalogue and the menu disagree, in content or in order"
        );
    }

    /// menu.rs documents its own labelling rule: "A trailing ellipsis means
    /// the filter opens a dialog; a filter with no parameters applies
    /// immediately and carries none." That is a promise about *this* module's
    /// schemas, and only a test that reads both can keep it.
    #[test]
    fn a_menu_label_promises_a_dialog_exactly_when_the_filter_has_parameters() {
        for filter in FILTERS {
            assert_eq!(
                filter.id.label().ends_with('…'),
                !filter.params.is_empty(),
                "{:?} is labelled {:?} but has {} parameters",
                filter.id,
                filter.id.label(),
                filter.params.len()
            );
        }
    }

    #[test]
    fn a_filters_group_and_title_come_from_the_menu_entry() {
        // Not restated here: a second copy is a second thing to get wrong, and
        // the module used to carry a whole private `FilterGroup` enum that had
        // no `Render` variant, which is why the five Render filters could not
        // be listed at all.
        let flare = filter_by_id(FilterId::LensFlare).expect("Lens Flare has a dialog");
        assert_eq!(flare.group(), FilterGroup::Render);
        assert_eq!(flare.name(), "Lens Flare");
        assert_eq!(FilterId::LensFlare.label(), "Lens Flare…");
        for filter in FILTERS {
            assert_eq!(filter.group(), filter.id.group());
        }
    }

    #[test]
    fn every_schema_default_is_inside_its_own_range() {
        for filter in FILTERS {
            for spec in filter.params {
                match spec.kind {
                    OptionKind::Float { min, max, default } => {
                        assert!(min <= max, "{:?}/{}: {min} > {max}", filter.id, spec.key);
                        assert!(
                            (min..=max).contains(&default),
                            "{:?}/{}: default {default} outside {min}..={max}",
                            filter.id,
                            spec.key
                        );
                    }
                    OptionKind::Int { min, max, default } => {
                        assert!(min <= max, "{:?}/{}: {min} > {max}", filter.id, spec.key);
                        assert!(
                            (min..=max).contains(&default),
                            "{:?}/{}: default {default} outside {min}..={max}",
                            filter.id,
                            spec.key
                        );
                    }
                    OptionKind::Choice { choices, default } => {
                        assert!(
                            !choices.is_empty(),
                            "{:?}/{} has no choices",
                            filter.id,
                            spec.key
                        );
                        assert!(default < choices.len(), "{:?}/{}", filter.id, spec.key);
                    }
                    OptionKind::Bool { .. } | OptionKind::Color { .. } => {}
                }
                assert!(
                    !spec.label.is_empty(),
                    "{:?}/{} has no label",
                    filter.id,
                    spec.key
                );
            }
        }
    }

    #[test]
    fn a_new_filter_gets_a_complete_parameter_set_for_free() {
        for filter in FILTERS {
            let params = FilterParams::defaults(filter.params);
            for spec in filter.params {
                assert!(
                    params.get(spec.key).is_some(),
                    "{:?}/{} was not populated",
                    filter.id,
                    spec.key
                );
            }
        }
    }

    #[test]
    fn every_filter_runs_on_a_proxy_and_returns_the_same_size() {
        let source = placeholder_buffer(48, 48);
        for filter in FILTERS {
            let dialog = FilterDialog::new(filter, source.clone());
            let out = dialog.preview_buffer();
            assert_eq!(
                out.dimensions(),
                (48, 48),
                "{:?} changed the buffer size",
                filter.id
            );
            assert!(
                out.pixels().iter().all(|p| p.iter().all(|v| v.is_finite())),
                "{:?} produced a non-finite pixel",
                filter.id
            );
        }
    }

    /// A filter's `apply` closure is the one part of a `FilterSpec` the schema
    /// cannot check: it names its parameters as strings, and a mistyped one
    /// reads back a zero instead of failing. Forty-one closures naming a few
    /// hundred keys between them is exactly where that goes wrong, so every
    /// closure is run and every lookup goes through the debug assertion in
    /// `FilterParams::expect_in_schema`.
    #[test]
    fn every_apply_closure_reads_only_its_own_schema() {
        // The guard is a `debug_assert!`, so first prove it actually fires
        // here — a test whose whole mechanism is compiled out would pass
        // silently and say nothing about the closures.
        if cfg!(debug_assertions) {
            let hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let params = FilterParams::defaults(GAUSSIAN);
            let caught =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| params.float("radus")));
            std::panic::set_hook(hook);
            assert!(
                caught.is_err(),
                "reading a key outside the schema went unnoticed"
            );
        }
        let source = placeholder_buffer(8, 8);
        for filter in FILTERS {
            let mut dialog = FilterDialog::new(filter, source.clone());
            // Defaults first, then every choice arm and both ends of every
            // range, because a closure can name a key on only one branch.
            let _ = dialog.preview_buffer();
            for spec in filter.params {
                match spec.kind {
                    OptionKind::Float { min, max, .. } => {
                        for value in [min, max] {
                            dialog.set_param(spec.key, ParamValue::Float(value));
                            let _ = dialog.preview_buffer();
                        }
                    }
                    OptionKind::Int { min, max, .. } => {
                        for value in [min, max] {
                            dialog.set_param(spec.key, ParamValue::Int(value));
                            let _ = dialog.preview_buffer();
                        }
                    }
                    OptionKind::Bool { .. } => {
                        for value in [false, true] {
                            dialog.set_param(spec.key, ParamValue::Bool(value));
                            let _ = dialog.preview_buffer();
                        }
                    }
                    OptionKind::Choice { choices, .. } => {
                        for index in 0..choices.len() {
                            dialog.set_param(spec.key, ParamValue::Choice(index));
                            let _ = dialog.preview_buffer();
                        }
                    }
                    OptionKind::Color { .. } => {
                        dialog.set_param(spec.key, ParamValue::Color([0.2, 0.4, 0.6, 0.5]));
                        let _ = dialog.preview_buffer();
                    }
                }
                dialog.reset();
            }
        }
    }

    /// A proxy with *outliers* as well as structure.
    ///
    /// [`placeholder_buffer`] is a smooth ramp over a hard checker, and a
    /// smooth image is its own local median — so Dust & Scratches leaves it
    /// alone at every threshold, and a threshold control that works perfectly
    /// well looks dead. Grain gives the rank filters something to reject.
    fn busy_buffer(side: u32) -> FilterBuffer {
        noise::add_noise(
            &placeholder_buffer(side, side),
            0.6,
            noise::NoiseDistribution::Uniform,
            false,
            7,
        )
    }

    /// And the other half of the same worry: a parameter the closure never
    /// reads is a control that looks live and does nothing. Every parameter
    /// moved to the far end of its range has to change what the filter
    /// produces.
    /// Values to try for one parameter: both ends of its range, and the two
    /// quarter points between the default and each end.
    ///
    /// The ends alone are not enough. Lens Blur's blade rotation runs from
    /// -180 to 180 and a hexagonal iris turned by either end lands back on
    /// itself, so a control that works perfectly well looked dead.
    fn alternatives(kind: OptionKind) -> Vec<ParamValue> {
        match kind {
            OptionKind::Float { min, max, default } => {
                [min, max, 0.5 * (min + default), 0.5 * (default + max)]
                    .into_iter()
                    .filter(|v| *v != default)
                    .map(ParamValue::Float)
                    .collect()
            }
            OptionKind::Int { min, max, default } => {
                [min, max, (min + default) / 2, (default + max) / 2]
                    .into_iter()
                    .filter(|v| *v != default)
                    .map(ParamValue::Int)
                    .collect()
            }
            OptionKind::Bool { default } => vec![ParamValue::Bool(!default)],
            OptionKind::Choice { choices, default } => (0..choices.len())
                .filter(|i| *i != default)
                .map(ParamValue::Choice)
                .collect(),
            OptionKind::Color { default } => vec![ParamValue::Color([
                1.0 - default[0],
                1.0 - default[1],
                1.0 - default[2],
                1.0,
            ])],
        }
    }

    /// Whether moving `key` away from its default changes what the filter
    /// produces, for any of [`alternatives`].
    fn parameter_reaches_the_filter(filter: &'static FilterSpec, key: &str) -> bool {
        let spec = filter
            .params
            .iter()
            .find(|s| s.key == key)
            .unwrap_or_else(|| panic!("{:?} has no parameter {key:?}", filter.id));
        let candidates = alternatives(spec.kind);
        assert!(
            !candidates.is_empty(),
            "{:?}/{key} has only one possible value",
            filter.id
        );
        let mut dialog = FilterDialog::new(filter, busy_buffer(24));
        let base = dialog.preview_buffer().to_rgba8();
        candidates.into_iter().any(|value| {
            assert!(dialog.set_param(key, value), "{:?}/{key}", filter.id);
            let changed = dialog.preview_buffer().to_rgba8() != base;
            dialog.reset();
            changed
        })
    }

    #[test]
    fn every_parameter_reaches_the_filter() {
        for filter in FILTERS {
            for spec in filter.params {
                let exempt = EXPECTED_INERT
                    .iter()
                    .any(|(id, key, _)| *id == filter.id && *key == spec.key);
                assert!(
                    exempt || parameter_reaches_the_filter(filter, spec.key),
                    "{:?}/{} does not reach the filter: moving it changes nothing",
                    filter.id,
                    spec.key
                );
            }
        }
    }

    /// Parameters that cannot change the output *of the default parameter
    /// set*, with the reason. Both are real properties of the filter rather
    /// than dead controls, and both stop being true the moment a neighbouring
    /// parameter is moved — which is why the list exists instead of the test
    /// being weakened.
    const EXPECTED_INERT: &[(FilterId, &str, &str)] = &[
        (
            FilterId::Offset,
            "edge",
            "the default offset is (0, 0), which never reads outside the buffer",
        ),
        (
            FilterId::Custom,
            "w11",
            "the default kernel has one non-zero tap and an automatic divisor,              so scaling that tap normalises straight back to the identity",
        ),
        (
            FilterId::Custom,
            "edge",
            "the default kernel reads only the centre tap, so it never reaches              outside the buffer for the edge mode to have a say",
        ),
    ];

    #[test]
    fn the_inert_parameter_list_stays_honest() {
        for (id, key, reason) in EXPECTED_INERT {
            assert!(reason.len() > 20, "{id:?}/{key} needs a real reason");
            let filter = filter_by_id(*id).expect("an exempted filter still exists");
            // A control that has *started* reaching the filter must come off
            // the list, or the list becomes a place to hide a dead one.
            assert!(
                !parameter_reaches_the_filter(filter, key),
                "{id:?}/{key} reaches the filter now — take it off EXPECTED_INERT"
            );
        }
    }

    /// And the exemptions are about the *default* set only: each one comes
    /// alive as soon as the parameter it depends on moves.
    #[test]
    fn an_inert_parameter_comes_alive_once_its_neighbour_moves() {
        let source = busy_buffer(24);

        let offset = filter_by_id(FilterId::Offset).expect("Offset has a dialog");
        let mut dialog = FilterDialog::new(offset, source.clone());
        assert!(dialog.set_param("dx", ParamValue::Int(5)));
        let clamped = dialog.preview_buffer().to_rgba8();
        assert!(dialog.set_param("edge", ParamValue::Choice(1)));
        assert_ne!(
            dialog.preview_buffer().to_rgba8(),
            clamped,
            "Offset's edge mode does nothing even once the layer is moved"
        );

        let custom = filter_by_id(FilterId::Custom).expect("Custom has a dialog");
        let mut dialog = FilterDialog::new(custom, source);
        assert!(dialog.set_param("w01", ParamValue::Float(1.0)));
        let two_tap = dialog.preview_buffer().to_rgba8();
        assert!(dialog.set_param("w11", ParamValue::Float(8.0)));
        assert_ne!(
            dialog.preview_buffer().to_rgba8(),
            two_tap,
            "the Custom kernel's centre tap does nothing beside a second tap"
        );
        assert!(dialog.set_param("w11", ParamValue::Float(1.0)));
        let clamped_edges = dialog.preview_buffer().to_rgba8();
        assert!(dialog.set_param("edge", ParamValue::Choice(1)));
        assert_ne!(
            dialog.preview_buffer().to_rgba8(),
            clamped_edges,
            "the Custom kernel's edge mode does nothing once a tap reaches outside"
        );
    }

    #[test]
    fn every_filter_survives_a_one_pixel_buffer() {
        let source = placeholder_buffer(1, 1);
        for filter in FILTERS {
            let dialog = FilterDialog::new(filter, source.clone());
            assert_eq!(
                dialog.preview_buffer().dimensions(),
                (1, 1),
                "{:?}",
                filter.id
            );
        }
    }

    #[test]
    fn parameters_clamp_to_the_schema() {
        let spec = filter_by_id(FilterId::GaussianBlur).unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        assert!(dialog.set_param("radius", ParamValue::Float(1.0e9)));
        assert_eq!(dialog.params().float("radius"), 250.0);
        assert!(dialog.set_param("radius", ParamValue::Float(-5.0)));
        assert_eq!(dialog.params().float("radius"), 0.0);
        assert!(dialog.set_param("radius", ParamValue::Float(f32::NAN)));
        assert_eq!(dialog.params().float("radius"), 0.0);
    }

    #[test]
    fn a_choice_never_escapes_its_option_list() {
        let spec = filter_by_id(FilterId::GaussianBlur).unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        assert!(dialog.set_param("edge", ParamValue::Choice(99)));
        assert_eq!(dialog.params().choice("edge"), EDGE_MODES.len() - 1);
        assert_eq!(edge_mode(dialog.params(), "edge"), EdgeMode::Mirror);
    }

    #[test]
    fn a_parameter_of_the_wrong_kind_is_refused() {
        let spec = filter_by_id(FilterId::GaussianBlur).unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        let before = dialog.params().clone();
        assert!(!dialog.set_param("radius", ParamValue::Bool(true)));
        assert!(!dialog.set_param("no-such-key", ParamValue::Float(1.0)));
        assert_eq!(dialog.params(), &before);
    }

    #[test]
    fn reset_restores_every_default() {
        let spec = filter_by_id(FilterId::UnsharpMask).unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        let defaults = dialog.params().clone();
        dialog.set_param("amount", ParamValue::Float(4.0));
        dialog.set_param("radius", ParamValue::Float(9.0));
        assert_ne!(dialog.params(), &defaults);
        dialog.reset();
        assert_eq!(dialog.params(), &defaults);
    }

    #[test]
    fn a_parameter_change_actually_changes_the_preview() {
        let spec = filter_by_id(FilterId::GaussianBlur).unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(48, 48));
        let sharp = dialog.preview_buffer().to_rgba8();
        dialog.set_param("radius", ParamValue::Float(12.0));
        let blurred = dialog.preview_buffer().to_rgba8();
        assert_ne!(sharp, blurred, "the radius did not reach the filter");
    }

    #[test]
    fn the_preview_toggle_stops_the_work() {
        let spec = filter_by_id(FilterId::GaussianBlur).unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        assert!(dialog.preview_enabled());
        dialog.set_preview_enabled(false);
        assert!(!dialog.preview_enabled());
        assert!(dialog.texture.is_none());
        assert!(dialog.cached_for.is_none());
    }

    #[test]
    fn an_invocation_carries_the_filter_and_its_own_schema() {
        for filter in FILTERS {
            let dialog = FilterDialog::new(filter, placeholder_buffer(16, 16));
            let invocation = dialog.invocation();
            assert!(invocation.is_valid(), "{:?}", filter.id);
            assert_eq!(invocation.filter.id, filter.id);
            // And it can actually run.
            let out = invocation.run(&placeholder_buffer(16, 16));
            assert_eq!(out.dimensions(), (16, 16));
        }
    }

    #[test]
    fn confirm_produces_a_valid_invocation_and_cancel_produces_nothing() {
        for filter in FILTERS {
            let dialog = FilterDialog::new(filter, placeholder_buffer(16, 16));
            let action = dialog
                .confirm()
                .unwrap_or_else(|| panic!("{:?}", filter.id));
            assert!(action.is_valid(), "{:?}", filter.id);
            assert_eq!(
                super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
                DialogOutcome::Cancelled
            );
        }
    }

    #[test]
    fn every_generated_dialog_draws_in_both_appearances() {
        for filter in FILTERS {
            frame_both_themes(|ctx| {
                let mut dialog = FilterDialog::new(filter, placeholder_buffer(32, 32));
                assert!(dialog.show(ctx, None).is_open(), "{:?}", filter.id);
                dialog.set_preview_enabled(false);
                assert!(dialog.show(ctx, None).is_open(), "{:?}", filter.id);
            });
        }
    }

    /// A schema carrying one parameter of every kind, in one form.
    ///
    /// Three shipping filters now take a colour — Pointillize, Fibers and
    /// Gradient — so the `Color` arm is no longer exercised only here. What is
    /// still only here is *all five kinds at once*: no single shipping filter
    /// mixes a float, an int, a flag, a choice and a colour, and the arm that
    /// used to be a dead control is worth drawing beside its neighbours.
    const EVERY_KIND: &[OptionSpec] = &[
        float("amount", "Amount", 0.0, 1.0, 0.5),
        int("count", "Count", 1, 8, 2),
        flag("invert", "Invert", false),
        choice("edge", "Edges", EDGE_MODES, 0),
        color("tint", "Tint", [0.0, 0.0, 0.0, 1.0]),
    ];

    /// A `FilterSpec` over [`EVERY_KIND`]. Its id is a real menu entry because
    /// a spec is *keyed* by one — this one ships nowhere, so which entry it
    /// borrows only decides the window title.
    static COLOR_FILTER: FilterSpec = FilterSpec {
        id: FilterId::Custom,
        summary: "A test filter carrying one parameter of every kind.",
        params: EVERY_KIND,
        apply: |src, _| src.clone(),
    };

    #[test]
    fn the_generated_form_covers_every_option_kind() {
        let params = FilterParams::defaults(EVERY_KIND);
        let kinds: Vec<&str> = EVERY_KIND
            .iter()
            .map(|spec| match spec.kind {
                OptionKind::Float { .. } => "float",
                OptionKind::Int { .. } => "int",
                OptionKind::Bool { .. } => "bool",
                OptionKind::Choice { .. } => "choice",
                OptionKind::Color { .. } => "color",
            })
            .collect();
        assert_eq!(kinds, ["float", "int", "bool", "choice", "color"]);
        for spec in EVERY_KIND {
            assert!(params.get(spec.key).is_some(), "{}", spec.key);
        }
    }

    #[test]
    fn clicking_a_colour_parameters_swatch_sets_that_parameter() {
        // The defect this pins: the generated form's Color arm drew a swatch
        // and dropped its Response, so a filter with a colour parameter got a
        // dialog in which that parameter could never leave its schema default.
        let h = Harness::new();
        let mut dialog = FilterDialog::new(&COLOR_FILTER, placeholder_buffer(16, 16));
        assert_eq!(
            dialog.params().get("tint"),
            Some(ParamValue::Color([0.0, 0.0, 0.0, 1.0]))
        );

        h.click_widget(ids::filter_param_color("tint"), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(
            dialog.color_edit().target(),
            Some("tint"),
            "the swatch opened nothing"
        );

        let chosen = crate::dialogs::ColorValue::new([0.9, 0.1, 0.4, 1.0]);
        dialog
            .color_edit_mut()
            .picker_mut()
            .expect("the picker is up")
            .set_color(chosen);
        h.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            assert!(dialog.show(ctx, None).is_open());
        });

        match dialog.params().get("tint") {
            Some(ParamValue::Color(rgba)) => {
                assert_eq!(
                    crate::dialogs::ColorValue::new(rgba).to_bytes(),
                    chosen.to_bytes()
                );
            }
            other => panic!("expected a colour, got {other:?}"),
        }
        // And it reaches the invocation the dialog commits.
        match dialog.confirm() {
            Some(DialogAction::RunFilter(invocation)) => {
                assert_eq!(invocation.params.get("tint"), dialog.params().get("tint"));
            }
            other => panic!("expected an invocation, got {other:?}"),
        }
    }

    #[test]
    fn a_filter_without_a_colour_parameter_draws_no_swatch() {
        let h = Harness::new();
        let spec = filter_by_id(FilterId::GaussianBlur).unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        h.frame(Vec::new(), |ctx| {
            dialog.show(ctx, None);
        });
        assert!(!h.was_drawn(ids::filter_param_color("tint")));
    }

    #[test]
    fn the_colour_filter_dialog_draws_in_both_appearances() {
        frame_both_themes(|ctx| {
            let mut dialog = FilterDialog::new(&COLOR_FILTER, placeholder_buffer(16, 16));
            assert!(dialog.show(ctx, None).is_open());
        });
    }
}
