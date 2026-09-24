//! W9-N: Photoshop / Photopea resource files — what File > Open does with a
//! `.pat`, `.grd`, `.csh`, `.aco`, `.ase` or `.icc`.
//!
//! A resource file is not a picture. Opening one adds what it carries to a
//! library: patterns to the pattern presets, gradients to the gradient
//! presets, custom shapes to the shape presets, swatches to the Swatches
//! panel, and an ICC profile is assigned to the active document. This module
//! is the parsing half — bytes in, plain data out — and knows nothing about
//! the application; `app-shell` routes the result.
//!
//! | Extension | Format | Parser |
//! |---|---|---|
//! | `.pat` | Photoshop patterns (`8BPT`), or a GIMP pattern (`GPAT`) | [`pat`] |
//! | `.grd` | Photoshop gradients, version 5 (`8BGR` + a descriptor) | [`grd`] |
//! | `.csh` | Photoshop custom shapes (`cush`, version 2) | [`csh`] |
//! | `.aco` | Photoshop colour swatches, version 1 and 2 | [`aco`] |
//! | `.ase` | Adobe Swatch Exchange (`ASEF`) | [`ase`] |
//! | `.icc`, `.icm` | an ICC colour profile | [`icc`] |
//!
//! `.atn` (Photoshop actions) is recognised by [`ResourceKind`] but has no
//! parser: see [`ATN_REFUSAL`].
//!
//! # Untrusted input
//!
//! These files come from other people. Every parser reads through
//! `psd::bytes::Cursor`, whose reads are bounds-checked and return errors, so
//! a truncated or lying file is an [`ResourceError`] and never a panic. Every
//! count a file declares is checked against a limit here *before* anything
//! is reserved for it, and the whole file is capped at
//! [`MAX_RESOURCE_BYTES`] before it is read. A damaged entry inside an
//! otherwise good library is refused by name (the `refused` list of a
//! [`Loaded`]) while the entries around it still load.

use serde::{Deserialize, Serialize};

pub mod aco;
pub mod ase;
pub mod csh;
pub mod grd;
pub mod icc;
pub mod pat;

#[cfg(test)]
mod tests;

pub use crate::presets::PatternPreset;

/// The largest resource file File > Open reads: 64 MiB, checked from the
/// file's metadata before a byte is read.
pub const MAX_RESOURCE_BYTES: u64 = 64 << 20;

/// Most entries (swatches, gradients, shapes, patterns) one file may define.
pub const MAX_ENTRIES: usize = 16_384;

/// Most stops one gradient may carry, colour and opacity ramps each.
pub const MAX_STOPS: usize = 1_024;

/// Most knots one custom-shape file may carry in total.
pub const MAX_KNOTS: usize = 1 << 20;

/// Why `.atn` files are recognised but not imported.
///
/// A Photoshop action is a list of *parametric* steps ("Gaussian Blur,
/// radius 4 px" on whatever document is active), each an event id plus an
/// action descriptor. This application's Actions library records *concrete*
/// edits — the commands a step produced and the tile bytes they wrote — and
/// replays those; it has no step that takes parameters and runs a filter or
/// adjustment afresh. A mapping from `.atn` events onto that library would
/// replay nothing but the handful of steps that need no pixels, so it is
/// refused by name instead of half-imported.
pub const ATN_REFUSAL: &str = "Photoshop actions (.atn) cannot be imported: \
    they are parametric steps, and this application's Actions library replays \
    recorded edits, not parametric filter or adjustment steps";

/// Which library a file extension feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Patterns,
    Gradients,
    Shapes,
    SwatchesAco,
    SwatchesAse,
    IccProfile,
    /// Recognised so the user is told why it does not import
    /// ([`ATN_REFUSAL`]); never parsed.
    Actions,
}

impl ResourceKind {
    /// Every extension File > Open routes to a library, lower case.
    pub const EXTENSIONS: &'static [&'static str] =
        &["pat", "grd", "csh", "aco", "ase", "icc", "icm", "atn"];

    /// The kind an extension names, case-insensitively, without the dot.
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "pat" => Self::Patterns,
            "grd" => Self::Gradients,
            "csh" => Self::Shapes,
            "aco" => Self::SwatchesAco,
            "ase" => Self::SwatchesAse,
            "icc" | "icm" => Self::IccProfile,
            "atn" => Self::Actions,
            _ => return None,
        })
    }

    /// The kind a path's extension names.
    pub fn of_path(path: &std::path::Path) -> Option<Self> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Self::from_extension)
    }
}

/// Why a resource file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceError {
    #[error("this is not a {what} file (its signature is wrong)")]
    BadSignature { what: &'static str },
    #[error("unsupported {what}: {detail}")]
    Unsupported { what: &'static str, detail: String },
    #[error("{what} is {value}, more than the {max} this reader accepts")]
    LimitExceeded {
        what: &'static str,
        value: u64,
        max: u64,
    },
    #[error("the file is damaged: {0}")]
    Malformed(String),
    #[error("the file defines nothing this application can use")]
    Empty,
}

impl From<psd::PsdError> for ResourceError {
    fn from(e: psd::PsdError) -> Self {
        ResourceError::Malformed(e.to_string())
    }
}

/// Refuse a declared count past `max` before anything is reserved for it.
pub(crate) fn check_count(
    what: &'static str,
    value: usize,
    max: usize,
) -> Result<(), ResourceError> {
    if value > max {
        return Err(ResourceError::LimitExceeded {
            what,
            value: value as u64,
            max: max as u64,
        });
    }
    Ok(())
}

/// What a library file held: the entries that read, and the ones that did
/// not (with why), in file order.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded<T> {
    pub items: Vec<T>,
    pub refused: Vec<String>,
}

impl<T> Default for Loaded<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            refused: Vec::new(),
        }
    }
}

impl<T> Loaded<T> {
    /// `Err(Empty)` when nothing at all read — a file of only damaged entries
    /// is reported as a failure, not as a silent no-op.
    fn non_empty(self) -> Result<Self, ResourceError> {
        if self.items.is_empty() {
            match self.refused.first() {
                Some(why) => Err(ResourceError::Malformed(why.clone())),
                None => Err(ResourceError::Empty),
            }
        } else {
            Ok(self)
        }
    }
}

/// One named colour, straight-alpha sRGB in `0.0..=1.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwatchResource {
    pub name: String,
    pub rgba: [f32; 4],
}

/// A colour stop of an imported gradient.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorStopResource {
    /// `0.0..=1.0` along the ramp.
    pub position: f32,
    /// `0.0..=1.0`: where between this stop and the next the blend is half
    /// way (0.5 is linear).
    pub midpoint: f32,
    /// sRGB, `0.0..=1.0`.
    pub rgb: [f32; 3],
}

/// An opacity stop of an imported gradient.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpacityStopResource {
    pub position: f32,
    pub midpoint: f32,
    /// `0.0..=1.0`.
    pub opacity: f32,
}

/// One gradient, as a `.grd` defines it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradientResource {
    pub name: String,
    /// Photoshop's Smoothness, `0.0..=1.0`.
    pub smoothness: f32,
    /// Colour stops, sorted by position; at least one.
    pub stops: Vec<ColorStopResource>,
    /// Opacity stops, sorted by position; may be empty (fully opaque).
    pub opacity_stops: Vec<OpacityStopResource>,
}

/// One Bézier knot of a custom shape: the incoming control point, the
/// anchor, and the outgoing control point, each `(x, y)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KnotResource {
    pub before: [f64; 2],
    pub anchor: [f64; 2],
    pub after: [f64; 2],
}

/// One subpath of a custom shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubpathResource {
    pub closed: bool,
    pub knots: Vec<KnotResource>,
}

/// One custom shape, as a `.csh` defines it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeResource {
    pub name: String,
    /// Photoshop's unique id for the shape (may be empty).
    pub id: String,
    pub subpaths: Vec<SubpathResource>,
}

impl ShapeResource {
    /// The shape as SVG path data, scaled so its bounds (anchors and control
    /// points) are exactly the unit square `(0, 0)–(1, 1)`, y down — the
    /// shape the Custom Shape tool fits into the box the user drags.
    pub fn unit_svg_path(&self) -> String {
        let points = self
            .subpaths
            .iter()
            .flat_map(|s| s.knots.iter())
            .flat_map(|k| [k.before, k.anchor, k.after]);
        let (mut min, mut max) = ([f64::MAX; 2], [f64::MIN; 2]);
        for p in points {
            for axis in 0..2 {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
        }
        let span = |axis: usize| {
            let s = max[axis] - min[axis];
            if s.is_finite() && s > 0.0 {
                s
            } else {
                1.0
            }
        };
        let (sx, sy) = (span(0), span(1));
        let fit = |p: [f64; 2]| ((p[0] - min[0]) / sx, (p[1] - min[1]) / sy);
        let mut out = String::new();
        for sub in &self.subpaths {
            let Some(first) = sub.knots.first() else {
                continue;
            };
            let (x, y) = fit(first.anchor);
            out.push_str(&format!("M{} {} ", fmt(x), fmt(y)));
            let mut segments: Vec<(&KnotResource, &KnotResource)> =
                sub.knots.windows(2).map(|w| (&w[0], &w[1])).collect();
            if sub.closed && sub.knots.len() > 1 {
                segments.push((&sub.knots[sub.knots.len() - 1], first));
            }
            for (from, to) in segments {
                let (c1x, c1y) = fit(from.after);
                let (c2x, c2y) = fit(to.before);
                let (x, y) = fit(to.anchor);
                out.push_str(&format!(
                    "C{} {} {} {} {} {} ",
                    fmt(c1x),
                    fmt(c1y),
                    fmt(c2x),
                    fmt(c2y),
                    fmt(x),
                    fmt(y)
                ));
            }
            if sub.closed {
                out.push_str("Z ");
            }
        }
        out.trim_end().to_string()
    }
}

/// Six decimals, trailing zeros dropped — the precision the vector crate's
/// own SVG writer uses.
fn fmt(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// An ICC profile file, checked and described.
#[derive(Debug, Clone, PartialEq)]
pub struct IccResource {
    /// The whole profile, exactly as the file held it.
    pub bytes: Vec<u8>,
    /// The profile's data colour space signature: `RGB `, `GRAY`, `CMYK`…
    pub data_space: [u8; 4],
    /// The profile/device class signature: `mntr`, `prtr`, `scnr`…
    pub device_class: [u8; 4],
    /// The profile's own description (`desc` tag), when it has one.
    pub description: Option<String>,
}

impl IccResource {
    /// Whether this profile can be assigned to an RGB document.
    pub fn is_rgb(&self) -> bool {
        &self.data_space == b"RGB "
    }
}

/// Everything a resource file can hold.
#[derive(Debug, Clone, PartialEq)]
pub enum Resource {
    Patterns(Loaded<PatternPreset>),
    Gradients(Loaded<GradientResource>),
    Shapes(Loaded<ShapeResource>),
    Swatches(Loaded<SwatchResource>),
    Icc(IccResource),
}

/// Parse `bytes` as a resource file of `kind`.
pub fn parse(kind: ResourceKind, bytes: &[u8]) -> Result<Resource, ResourceError> {
    if bytes.len() as u64 > MAX_RESOURCE_BYTES {
        return Err(ResourceError::LimitExceeded {
            what: "resource file size",
            value: bytes.len() as u64,
            max: MAX_RESOURCE_BYTES,
        });
    }
    Ok(match kind {
        ResourceKind::Patterns => Resource::Patterns(pat::parse(bytes)?.non_empty()?),
        ResourceKind::Gradients => Resource::Gradients(grd::parse(bytes)?.non_empty()?),
        ResourceKind::Shapes => Resource::Shapes(csh::parse(bytes)?.non_empty()?),
        ResourceKind::SwatchesAco => Resource::Swatches(aco::parse(bytes)?.non_empty()?),
        ResourceKind::SwatchesAse => Resource::Swatches(ase::parse(bytes)?.non_empty()?),
        ResourceKind::IccProfile => Resource::Icc(icc::parse(bytes)?),
        ResourceKind::Actions => {
            return Err(ResourceError::Unsupported {
                what: "file",
                detail: ATN_REFUSAL.to_string(),
            })
        }
    })
}

/// Read and parse the resource file at `path`, refusing past
/// [`MAX_RESOURCE_BYTES`] before reading.
pub fn load(path: &std::path::Path) -> Result<Resource, ResourceError> {
    let kind = ResourceKind::of_path(path).ok_or(ResourceError::Unsupported {
        what: "file type",
        detail: path.display().to_string(),
    })?;
    if kind == ResourceKind::Actions {
        return parse(kind, &[]);
    }
    let io = |e: std::io::Error| ResourceError::Malformed(e.to_string());
    let size = std::fs::metadata(path).map_err(io)?.len();
    if size > MAX_RESOURCE_BYTES {
        return Err(ResourceError::LimitExceeded {
            what: "resource file size",
            value: size,
            max: MAX_RESOURCE_BYTES,
        });
    }
    parse(kind, &std::fs::read(path).map_err(io)?)
}

// ------------------------------------------------------------ colour maths

fn unit(v: f64) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0) as f32
    } else {
        0.0
    }
}

/// HSB (hue in degrees, saturation and brightness `0..=1`) to sRGB.
pub(crate) fn hsb_to_rgb(h: f64, s: f64, b: f64) -> [f32; 3] {
    let h = if h.is_finite() {
        h.rem_euclid(360.0)
    } else {
        0.0
    } / 60.0;
    let (s, b) = (f64::from(unit(s)), f64::from(unit(b)));
    let c = b * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, bl) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = b - c;
    [unit(r + m), unit(g + m), unit(bl + m)]
}

/// CMYK ink coverage (`0..=1`, 1 = full ink) to sRGB, uncalibrated.
pub(crate) fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> [f32; 3] {
    let k = 1.0 - f64::from(unit(k));
    [
        unit((1.0 - f64::from(unit(c))) * k),
        unit((1.0 - f64::from(unit(m))) * k),
        unit((1.0 - f64::from(unit(y))) * k),
    ]
}

/// CIE L*a*b* (D50, L `0..=100`) to sRGB, through XYZ with Bradford
/// adaptation to D65.
pub(crate) fn lab_to_rgb(l: f64, a: f64, b: f64) -> [f32; 3] {
    let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
    let (l, a, b) = (finite(l).clamp(0.0, 100.0), finite(a), finite(b));
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let inv = |t: f64| {
        let t3 = t * t * t;
        if t3 > 216.0 / 24389.0 {
            t3
        } else {
            (116.0 * t - 16.0) / (24389.0 / 27.0)
        }
    };
    // D50 white.
    let (x, y, z) = (inv(fx) * 0.9642, inv(fy), inv(fz) * 0.8251);
    // XYZ (D50) to linear sRGB (D65), Bradford-adapted.
    let r = 3.1338561 * x - 1.6168667 * y - 0.4906146 * z;
    let g = -0.9787684 * x + 1.9161415 * y + 0.0334540 * z;
    let bl = 0.0719453 * x - 0.2289914 * y + 1.4052427 * z;
    let encode = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    [unit(encode(r)), unit(encode(g)), unit(encode(bl))]
}

/// Grey as ink coverage (`0..=1`, 1 = black) to sRGB.
pub(crate) fn gray_ink_to_rgb(ink: f64) -> [f32; 3] {
    let v = 1.0 - unit(ink);
    [v, v, v]
}
