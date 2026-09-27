//! W16-J: a smart object's smart filters as Photoshop's `filterFX`
//! descriptor, both ways.
//!
//! Photoshop keeps a placed layer's smart filters inside its `SoLd`
//! descriptor, under `filterFX` (class `filterFXStyle`): a master switch
//! (`enab`), the filter mask's flags, and `filterFXList`, one `filterFX`
//! descriptor per filter in the order they are applied (index 0 first, the
//! bottom of the Layers panel list — the order Photopea's renderer runs them
//! in). Each entry carries its display name (`Nm  `), its blending
//! (`blendOptions`: `Opct` in per cent and `Md  ` a `BlnM` code), its
//! switch (`enab`), `hasoptions`, the foreground and background colours
//! (`FrgC`/`BckC`), the filter's own settings (`Fltr`, a descriptor whose
//! class is the filter's event code, e.g. `GsnB` with `Rds ` in pixels) and
//! `filterID` — the event code as a big-endian `u32` when it is four
//! characters, else 777 (what Photopea writes; the filter is then named by
//! the `Fltr` class instead).
//!
//! # What maps
//!
//! [`MAPPED`] lists the filters this build writes as Photoshop smart filters,
//! both ways: the 19 W16-J wrote (the blurs, sharpens, noise filters, Mosaic,
//! High Pass, Maximum and Minimum) and, since W18-J, every other filter with
//! a Photoshop or Photopea `filterFX` equivalent (Radial and Smart Blur, Smart
//! Sharpen, Reduce Noise, the distorts, pixelates, Clouds, Difference Clouds,
//! Fibers, Lens Flare, the stylizes, Custom, Offset and Photopea's own
//! filters). Their settings are written in Photoshop's keys on Photoshop's
//! grid; a setting Photoshop has no key for (an edge mode, a distortion's
//! radius, a seed) or holds only coarser (a threshold in whole levels, an
//! angle in whole degrees) also rides as an [`EXTRA_PREFIX`] key, so this
//! build reads it back exactly and Photoshop reads the nearest setting it
//! has. The edge mode: Wrap is Photoshop's `WrpA` undefined-area choice on
//! the filters that have one, Mirror (which Photoshop lacks) is written as
//! repeat-edge-pixels, and any edge mode those keys cannot hold (Mirror, or
//! Wrap on a filter with no undefined-area key) rides as the extra
//! `rasterStudio_edge`.
//!
//! [`RASTERISED`] names each filter that is still refused, with the reason
//! (Lens Blur, Displace, Shear, Gradient Fill, Lighting Effects, Oil Paint,
//! Tiles, Flame, Camera Raw, Lens Correction): the caller keeps the smart
//! object's rendered pixels instead ([`encode_stack`] says which filters).
//!
//! The smart-filter mask: its switch, link, density, feather and invert ride
//! in `filterFXStyle` ([`encode_stack_masked`], [`decode_mask_flags`]); its
//! pixels, with the object's unfiltered pixels, in the document-level `FEid`
//! block ([`encode_filter_effects`], [`decode_filter_effects`]).

use std::collections::BTreeMap;

use layer_model::{BlendMode, SmartFilter, SmartParam};

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::effects::blend_from_blnm;
use crate::error::{PsdError, PsdResult};
use crate::limits::ReadOptions;
use crate::model::{PsdLayer, Rect, TaggedBlock};

/// The `SoLd` descriptor key the stack lives under.
pub const FILTER_FX_KEY: &str = "filterFX";

/// The smart-filter keys (`ui::menu::FilterId` variant names) this module
/// writes as Photoshop smart filters, each with its Photoshop event code.
/// W16-J wrote the first 19; W18-J the rest (Photoshop's keys for each are
/// the ones Photopea's own descriptors and dialogs use, and for Smart Blur
/// and Extrude, which Photopea lacks, the Photoshop SDK's terminology).
pub const MAPPED: &[(&str, &str)] = &[
    ("Average", "Avrg"),
    ("Blur", "Blr "),
    ("BlurMore", "BlrM"),
    ("BoxBlur", "boxblur"),
    ("GaussianBlur", "GsnB"),
    ("MotionBlur", "MtnB"),
    ("SurfaceBlur", "surfaceBlur"),
    ("Sharpen", "Shrp"),
    ("SharpenMore", "ShrM"),
    ("SharpenEdges", "ShrE"),
    ("UnsharpMask", "UnsM"),
    ("AddNoise", "AdNs"),
    ("Despeckle", "Dspc"),
    ("DustAndScratches", "DstS"),
    ("Median", "Mdn "),
    ("Mosaic", "Msc "),
    ("HighPass", "HghP"),
    ("Maximum", "Mxm "),
    ("Minimum", "Mnm "),
    // W18-J
    ("RadialBlur", "RdlB"),
    ("SmartBlur", "SmrB"),
    ("SmartSharpen", "smartSharpen"),
    ("ReduceNoise", "denoise"),
    ("Pinch", "Pnch"),
    ("PolarCoordinates", "Plr "),
    ("Ripple", "Rple"),
    ("Spherize", "Sphr"),
    ("Twirl", "Twrl"),
    ("Wave", "Wave"),
    ("ZigZag", "ZgZg"),
    ("ColorHalftone", "ClrH"),
    ("Crystallize", "Crst"),
    ("Facet", "Fct "),
    ("Fragment", "Frgm"),
    ("Mezzotint", "Mztn"),
    ("Pointillize", "Pntl"),
    ("ShapeMosaic", "ShMs"),
    ("Clouds", "Clds"),
    ("DifferenceClouds", "DfrC"),
    ("Fibers", "Fbrs"),
    ("LensFlare", "LnsF"),
    ("Diffuse", "Dfs "),
    ("Emboss", "Embs"),
    ("Extrude", "Extr"),
    ("FindEdges", "FndE"),
    ("Solarize", "Slrz"),
    ("TraceContour", "TrcC"),
    ("Wind", "Wnd "),
    ("Custom", "Cstm"),
    ("Offset", "Ofst"),
    ("HsbHsl", "HsbP"),
    ("Kaleidoscope", "Kale"),
    ("Dents", "Dnts"),
    ("Repeat", "Rept"),
    ("ColorToAlpha", "Ctoa"),
    ("Dither", "Dthr"),
    ("Particles", "Part"),
    ("FourierTransform", "dDFT"),
    ("InverseFourierTransform", "iDFT"),
    ("NormalMap", "lightFilterGradient"),
    ("TextureDilation", "Dila"),
];

/// W18-J: the filters this build has that are still written as the smart
/// object's rendered pixels, each with the reason no `filterFX` entry is
/// written for it.
pub const RASTERISED: &[(&str, &str)] = &[
    (
        "LensBlur",
        "Photoshop's Lens Blur (Bokh) is a depth-of-field model whose iris radius is a 0-100 \
         setting with curvature, specular and noise; this build's is a pixel-radius polygon \
         bokeh, and no correspondence between the two has been verified",
    ),
    (
        "Displace",
        "Photoshop's Displace reads its map from a separate .psd file; this build's map is the \
         layer itself or generated clouds, which that setting cannot name",
    ),
    (
        "Shear",
        "Photoshop's Shear is a curve drawn on a grid; this build's is a linear skew by two \
         factors",
    ),
    ("GradientFill", "Photoshop has no Gradient Fill filter"),
    (
        "LightingEffects",
        "Photoshop's Lighting Effects is a descriptor of 3D lights and texture-channel bindings \
         this build does not map",
    ),
    (
        "OilPaint",
        "Photoshop's Oil Paint is a stylization/cleanliness/brush-scale/lighting model; this \
         build's is a radius and intensity-levels filter, and no setting corresponds",
    ),
    (
        "Tiles",
        "the Photoshop descriptor keys for the number of tiles and their offset are not verified",
    ),
    (
        "Flame",
        "Flame burns along a path, which Photoshop's Flame descriptor does not carry",
    ),
    (
        "CameraRaw",
        "the Camera Raw filter's settings are not mapped onto Adobe Camera Raw's descriptor",
    ),
    (
        "LensCorrection",
        "the Lens Correction settings are not mapped onto Photoshop's LnCr descriptor",
    ),
];

/// `true` when [`encode_stack`] can write the filter keyed `key`.
pub fn has_photoshop_equivalent(key: &str) -> bool {
    MAPPED.iter().any(|(k, _)| *k == key)
}

/// W18-J: why the filter keyed `key` is written as pixels, when it is.
pub fn rasterised_reason(key: &str) -> Option<&'static str> {
    RASTERISED.iter().find(|(k, _)| *k == key).map(|(_, r)| *r)
}

/// `GaussianBlur` as `Gaussian Blur`, for messages.
pub fn display_name(key: &str) -> String {
    let mut out = String::new();
    for (i, ch) in key.chars().enumerate() {
        if i > 0 && ch.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(ch);
    }
    out.replace("And ", "& ")
}

/// The `BlnM` codes Photoshop writes, one per blend mode.
const BLEND_CODES: [&str; 27] = [
    "Nrml",
    "Dslv",
    "Drkn",
    "Mltp",
    "CBrn",
    "Lmbs",
    "dkCl",
    "Lghn",
    "Scrn",
    "CDdg",
    "lddg",
    "lgCl",
    "Ovrl",
    "SftL",
    "HrdL",
    "vLit",
    "lLit",
    "pLit",
    "HrdM",
    "Dfrn",
    "Xclu",
    "Sbtr",
    "blendDivide",
    "H   ",
    "Strt",
    "Clr ",
    "Lmns",
];

fn blend_code(mode: BlendMode) -> &'static str {
    BLEND_CODES
        .iter()
        .copied()
        .find(|c| blend_from_blnm(c) == Some(mode))
        .unwrap_or("Nrml")
}

fn px(v: f64) -> Value {
    Value::UnitFloat {
        unit: *b"#Pxl",
        value: v,
    }
}

fn pct(v: f64) -> Value {
    Value::UnitFloat {
        unit: *b"#Prc",
        value: v,
    }
}

fn enumerated(type_id: &str, value: &str) -> Value {
    Value::Enumerated {
        type_id: type_id.into(),
        value: value.into(),
    }
}

fn level(v: f64) -> Value {
    Value::Integer((v * 255.0).round().clamp(0.0, 255.0) as i32)
}

/// A whole number, as Photoshop's `long` settings are.
fn long(v: f64) -> Value {
    Value::Integer(v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32)
}

/// A stored parameter as a number (`default` when it is absent or not a
/// number).
fn num(f: &SmartFilter, key: &str, default: f64) -> f64 {
    match f.params.get(key) {
        Some(SmartParam::Float(v)) => f64::from(*v),
        Some(SmartParam::Int(v)) => f64::from(*v),
        Some(SmartParam::Choice(v)) => f64::from(*v),
        Some(SmartParam::Bool(v)) => f64::from(u8::from(*v)),
        _ => default,
    }
}

fn flag(f: &SmartFilter, key: &str) -> bool {
    matches!(f.params.get(key), Some(SmartParam::Bool(true)))
}

fn colour(f: &SmartFilter, key: &str, default: [f32; 4]) -> [f32; 4] {
    match f.params.get(key) {
        Some(SmartParam::Color(c)) => *c,
        _ => default,
    }
}

/// The four-character event code as Photoshop's `filterID`, or Photopea's
/// 777 for a longer code.
fn filter_id(code: &str) -> i32 {
    match <[u8; 4]>::try_from(code.as_bytes()) {
        Ok(b) => u32::from_be_bytes(b) as i32,
        Err(_) => 777,
    }
}

/// W18-J: a linear channel (what this build's colour parameters hold) as
/// the sRGB `0..=255` value an `RGBC` descriptor carries.
fn srgb255(linear: f32) -> f64 {
    let l = f64::from(linear).clamp(0.0, 1.0);
    let s = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    s * 255.0
}

/// The inverse of [`srgb255`].
fn linear_of(v255: f64) -> f32 {
    let s = (v255 / 255.0).clamp(0.0, 1.0);
    let l = if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    };
    l as f32
}

fn rgbc_of(c: [f32; 4]) -> Value {
    let mut d = Descriptor::new("RGBC");
    for (k, v) in ["Rd  ", "Grn ", "Bl  "].into_iter().zip(c) {
        let _ = d.push(k, Value::Double(srgb255(v)));
    }
    Value::Descriptor(d)
}

/// An `RGBC` descriptor as a linear, opaque colour.
fn colour_of(d: &Descriptor) -> Option<[f32; 4]> {
    let ch = |k: &str| d.number(k).filter(|v| v.is_finite()).map(linear_of);
    Some([ch("Rd  ")?, ch("Grn ")?, ch("Bl  ")?, 1.0])
}

const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// W18-J: the prefix of the keys that carry a setting Photoshop has no key
/// for (an edge mode, a radius, a seed), or one its key holds only on a
/// coarser grid, exactly as this build stores it. Photoshop reads the keys
/// it knows and ignores these; this build reads them back in preference.
pub const EXTRA_PREFIX: &str = "rasterStudio_";

/// A stored parameter as the value an extra key carries.
fn extra_value(p: SmartParam) -> Value {
    match p {
        SmartParam::Float(v) => Value::Double(f64::from(v)),
        SmartParam::Int(v) => Value::Integer(v),
        SmartParam::Choice(v) => Value::LargeInteger(i64::from(v)),
        SmartParam::Bool(v) => Value::Bool(v),
        SmartParam::Color(c) => {
            Value::List(c.iter().map(|v| Value::Double(f64::from(*v))).collect())
        }
    }
}

/// An extra key's value back as a parameter: `Double` a float, `long` an
/// int, `comp` a choice, `bool` a switch, a list of four a colour.
fn param_of_extra(v: &Value) -> Option<SmartParam> {
    Some(match v {
        Value::Double(x) => SmartParam::Float(*x as f32),
        Value::Integer(x) => SmartParam::Int(*x),
        Value::LargeInteger(x) => SmartParam::Choice(u32::try_from(*x).ok()?),
        Value::Bool(x) => SmartParam::Bool(*x),
        Value::List(items) if items.len() == 4 => {
            let mut c = [0.0f32; 4];
            for (slot, item) in c.iter_mut().zip(items) {
                match item {
                    Value::Double(x) => *slot = *x as f32,
                    _ => return None,
                }
            }
            SmartParam::Color(c)
        }
        _ => return None,
    })
}

/// What one filter writes besides its blending: its `Fltr` settings in
/// Photoshop's keys, and the foreground and background colours it uses
/// (`None` writes black and white).
struct Written {
    settings: Option<Descriptor>,
    fg: Option<[f32; 4]>,
    bg: Option<[f32; 4]>,
}

/// The four-character spelling of a choice, from `list` (the last entry
/// when the stored index is past it).
fn pick<'a>(list: &[&'a str], index: f64) -> &'a str {
    let i = (index.max(0.0) as usize).min(list.len() - 1);
    list[i]
}

/// Photoshop's undefined-area choice for this build's edge mode: Wrap is
/// `WrpA`, Clamp (and Mirror, which rides as an extra) repeat edge pixels.
fn undefined_area(f: &SmartFilter) -> Value {
    let wrap = num(f, "edge", 0.0) == 1.0;
    enumerated("UndA", if wrap { "WrpA" } else { "RptE" })
}

/// `f`'s settings in Photoshop's keys, on Photoshop's grid. What this cannot
/// hold exactly is added as [`EXTRA_PREFIX`] keys by [`encode_filter`].
fn photoshop_settings(f: &SmartFilter, code: &str) -> Written {
    let mut fg = None;
    let mut bg = None;
    let items: Vec<(&str, Value)> = match f.filter.as_str() {
        "BoxBlur" => vec![("Rds ", px(num(f, "radius", 4.0)))],
        "GaussianBlur" => vec![("Rds ", px(num(f, "radius", 4.0)))],
        "MotionBlur" => vec![
            ("Angl", long(num(f, "angle", 0.0))),
            ("Dstn", px(num(f, "distance", 16.0))),
        ],
        "SurfaceBlur" => vec![
            ("Rds ", px(num(f, "radius", 5.0))),
            ("Thsh", level(num(f, "threshold", 0.05))),
        ],
        "UnsharpMask" => vec![
            ("Amnt", pct(num(f, "amount", 1.0) * 100.0)),
            ("Rds ", px(num(f, "radius", 2.0))),
            ("Thsh", level(num(f, "threshold", 0.0))),
        ],
        "AddNoise" => vec![
            (
                "Dstr",
                enumerated(
                    "Dstr",
                    if num(f, "distribution", 0.0) == 1.0 {
                        "Gsn "
                    } else {
                        "Unfr"
                    },
                ),
            ),
            ("Nose", pct(num(f, "amount", 0.1) * 100.0)),
            ("Mnch", Value::Bool(flag(f, "monochromatic"))),
            ("FlRs", long(num(f, "seed", 1.0))),
        ],
        "DustAndScratches" => vec![
            ("Rds ", long(num(f, "radius", 2.0))),
            ("Thsh", level(num(f, "threshold", 0.05))),
        ],
        "Median" | "HighPass" => vec![(
            "Rds ",
            px(num(
                f,
                "radius",
                if f.filter == "Median" { 2.0 } else { 10.0 },
            )),
        )],
        "Mosaic" => vec![("ClSz", px(num(f, "cell", 8.0)))],
        "Maximum" | "Minimum" => vec![
            ("Rds ", px(num(f, "radius", 2.0))),
            ("preserveShape", enumerated("preserveShape", "squareness")),
        ],
        // ------------------------------------------------------ W18-J
        "RadialBlur" => {
            let samples = num(f, "samples", 16.0);
            let quality = if samples <= 8.0 {
                "Drft"
            } else if samples <= 32.0 {
                "Gd  "
            } else {
                "Bst "
            };
            let mut centre = Descriptor::new("Pnt ");
            let _ = centre.push("Hrzn", Value::Double(0.5));
            let _ = centre.push("Vrtc", Value::Double(0.5));
            vec![
                ("Amnt", long(num(f, "amount", 10.0))),
                (
                    "BlrM",
                    enumerated("BlrM", pick(&["Spn ", "Zm  "], num(f, "kind", 0.0))),
                ),
                ("BlrQ", enumerated("BlrQ", quality)),
                ("Cntr", Value::Descriptor(centre)),
            ]
        }
        "SmartBlur" => vec![
            ("Rds ", Value::Double(num(f, "radius", 3.0))),
            ("Thsh", Value::Double(num(f, "threshold", 0.1) * 100.0)),
            ("SmBQ", enumerated("SmBQ", "SBQM")),
            (
                "SmBM",
                enumerated("SmBM", pick(&["SBMN", "SBME", "SBMO"], num(f, "mode", 0.0))),
            ),
        ],
        "SmartSharpen" => vec![
            (
                "presetKind",
                enumerated("presetKindType", "presetKindCustom"),
            ),
            ("useLegacy", Value::Bool(false)),
            ("Amnt", pct(num(f, "amount", 1.0) * 100.0)),
            ("Rds ", px(num(f, "radius", 2.0))),
            ("noiseReduction", pct(num(f, "noise_floor", 0.02) * 100.0)),
            ("blur", enumerated("blurType", "GsnB")),
        ],
        "ReduceNoise" => {
            let mut channel = Descriptor::new("channelDenoiseParams");
            let _ = channel.push(
                "Chnl",
                Value::Reference(vec![crate::descriptor::RefItem::Enumerated {
                    name: String::new(),
                    class_id: "Chnl".into(),
                    type_id: "Chnl".into(),
                    value: "Cmps".into(),
                }]),
            );
            let _ = channel.push("Amnt", long(num(f, "strength", 4.0)));
            let _ = channel.push("EdgF", long(num(f, "detail", 0.5) * 100.0));
            vec![
                ("ClNs", pct(0.0)),
                ("Shrp", pct(0.0)),
                ("removeJPEGArtifact", Value::Bool(false)),
                (
                    "channelDenoise",
                    Value::List(vec![Value::Descriptor(channel)]),
                ),
                ("preset", Value::Text("Default".into())),
            ]
        }
        "Pinch" => vec![("Amnt", long(num(f, "amount", 0.5) * 100.0))],
        "PolarCoordinates" => vec![(
            "Cnvr",
            enumerated("Cnvr", pick(&["RctP", "PlrR"], num(f, "mode", 0.0))),
        )],
        "Ripple" => {
            let wavelength = num(f, "wavelength", 40.0);
            let size = if wavelength <= 20.0 {
                "Sml "
            } else if wavelength <= 60.0 {
                "Mdm "
            } else {
                "Lrg "
            };
            vec![
                ("Amnt", long(num(f, "amount", 8.0))),
                ("RplS", enumerated("RplS", size)),
            ]
        }
        "Spherize" => vec![
            ("Amnt", long(num(f, "amount", 0.5) * 100.0)),
            ("SphM", enumerated("SphM", "Nrml")),
        ],
        "Twirl" => vec![("Angl", long(num(f, "angle", 90.0)))],
        "Wave" => {
            let wavelength = long(num(f, "wavelength", 40.0).max(1.0));
            let amplitude = long(num(f, "amplitude", 8.0).max(1.0));
            vec![
                (
                    "Wvtp",
                    enumerated("Wvtp", pick(&["WvSn", "WvTr", "WvSq"], num(f, "kind", 0.0))),
                ),
                ("NmbG", Value::Integer(1)),
                ("WLMn", wavelength.clone()),
                ("WLMx", wavelength),
                ("AmMn", amplitude.clone()),
                ("AmMx", amplitude),
                ("SclH", Value::Integer(100)),
                ("SclV", Value::Integer(100)),
                ("UndA", undefined_area(f)),
                ("RndS", Value::Integer(1)),
            ]
        }
        "ZigZag" => vec![
            ("Amnt", long(num(f, "amount", 8.0))),
            ("NmbR", long(num(f, "ridges", 5.0))),
            (
                "ZZTy",
                enumerated("ZZTy", pick(&["PndR", "OtFr", "ArnC"], num(f, "kind", 0.0))),
            ),
        ],
        "ColorHalftone" => vec![
            ("Rds ", long(num(f, "radius", 4.0))),
            ("Ang1", long(num(f, "angle_r", 108.0))),
            ("Ang2", long(num(f, "angle_g", 162.0))),
            ("Ang3", long(num(f, "angle_b", 90.0))),
            ("Ang4", Value::Integer(45)),
        ],
        "Crystallize" | "Pointillize" => {
            if f.filter == "Pointillize" {
                bg = Some(colour(f, "background", WHITE));
            }
            vec![
                ("ClSz", long(num(f, "cell", 16.0))),
                ("FlRs", long(num(f, "seed", 1.0))),
            ]
        }
        "Mezzotint" => vec![
            (
                "MztT",
                enumerated(
                    "MztT",
                    pick(
                        &[
                            "FnDt", "MdmD", "GrnD", "CrsD", "ShrL", "MdmL", "LngL", "ShSt", "MdmS",
                            "LngS",
                        ],
                        num(f, "kind", 1.0),
                    ),
                ),
            ),
            ("FlRs", long(num(f, "seed", 1.0))),
        ],
        "ShapeMosaic" => vec![
            ("ClSz", px(num(f, "cell_size", 12.0))),
            ("Shap", long(num(f, "shape", 0.0))),
            ("Sprd", long(num(f, "spread", 0.0))),
            ("Mono", Value::Bool(flag(f, "monochromatic"))),
            ("Invr", Value::Bool(flag(f, "invert"))),
        ],
        "Clouds" | "DifferenceClouds" => vec![("FlRs", Value::Integer(1))],
        "Fibers" => {
            fg = Some(colour(f, "from", [0.02, 0.02, 0.02, 1.0]));
            bg = Some(colour(f, "to", [0.9, 0.9, 0.9, 1.0]));
            vec![
                (
                    "Vrnc",
                    long((num(f, "variance", 0.35) * 64.0).clamp(1.0, 64.0)),
                ),
                (
                    "Strg",
                    long((num(f, "strength", 0.5) * 64.0).clamp(1.0, 64.0)),
                ),
                ("RndS", long(num(f, "seed", 1.0))),
            ]
        }
        "LensFlare" => {
            let mut centre = Descriptor::new("Pnt ");
            let _ = centre.push("Hrzn", Value::Double(0.5));
            let _ = centre.push("Vrtc", Value::Double(0.5));
            vec![
                ("Brgh", long(num(f, "brightness", 1.0) * 100.0)),
                ("FlrC", Value::Descriptor(centre)),
                ("Lns ", enumerated("Lns ", "Zm  ")),
            ]
        }
        "Diffuse" => vec![
            (
                "Md  ",
                enumerated("DfsM", pick(&["Nrml", "DrkO", "LghO"], num(f, "mode", 0.0))),
            ),
            ("FlRs", long(num(f, "seed", 1.0))),
        ],
        "Emboss" => vec![
            ("Angl", long(num(f, "angle", 135.0))),
            ("Hght", long(num(f, "height", 3.0))),
            ("Amnt", long(num(f, "amount", 1.0) * 100.0)),
        ],
        "Extrude" => vec![
            (
                "ExtT",
                enumerated("ExtT", pick(&["Blks", "Pyrm"], num(f, "kind", 0.0))),
            ),
            ("ExtS", long(num(f, "size", 30.0))),
            ("ExtD", long(num(f, "depth", 30.0))),
            (
                "ExtR",
                enumerated("ExtR", pick(&["Rndm", "LvlB"], num(f, "depth_mode", 0.0))),
            ),
            ("ExtF", Value::Bool(false)),
            ("ExtM", Value::Bool(false)),
            ("FlRs", long(num(f, "seed", 1.0))),
        ],
        "TraceContour" => vec![
            ("Lvl ", long(num(f, "level", 128.0))),
            (
                "Edg ",
                enumerated("CntE", pick(&["Lwr ", "Upr "], num(f, "side", 0.0))),
            ),
        ],
        "Wind" => {
            let strength = num(f, "strength", 0.5);
            let method = if strength <= 0.625 {
                "Wnd "
            } else if strength <= 0.875 {
                "Blst"
            } else {
                "Stgr"
            };
            vec![
                ("WndM", enumerated("WndM", method)),
                (
                    "Drct",
                    enumerated("Drct", pick(&["Left", "Rght"], num(f, "direction", 0.0))),
                ),
            ]
        }
        "Custom" => {
            let taps = [
                "w00", "w01", "w02", "w10", "w11", "w12", "w20", "w21", "w22",
            ];
            let mut matrix = vec![Value::Integer(0); 25];
            let mut sum = 0.0;
            for (i, tap) in taps.iter().enumerate() {
                let w = num(f, tap, if i == 4 { 1.0 } else { 0.0 });
                sum += w;
                matrix[(i / 3 + 1) * 5 + i % 3 + 1] = long(w);
            }
            let divisor = num(f, "divisor", 0.0);
            let scale = if divisor == 0.0 {
                sum.round().max(1.0)
            } else {
                divisor
            };
            vec![
                ("Mtrx", Value::List(matrix)),
                ("Scl ", long(scale)),
                ("Ofst", long(num(f, "bias", 0.0) * 255.0)),
            ]
        }
        "Offset" => {
            let wrap = num(f, "edge", 0.0) == 1.0;
            vec![
                ("Hrzn", long(num(f, "dx", 0.0))),
                ("Vrtc", long(num(f, "dy", 0.0))),
                (
                    "Fl  ",
                    enumerated("FlMd", if wrap { "Wrp " } else { "Rpt " }),
                ),
            ]
        }
        "HsbHsl" => {
            let models = ["RGBC", "HSBl", "HSLC"];
            vec![
                (
                    "Inpt",
                    enumerated("ClrS", pick(&models, num(f, "input", 0.0))),
                ),
                (
                    "Otpt",
                    enumerated("ClrS", pick(&models, num(f, "output", 1.0))),
                ),
            ]
        }
        "Kaleidoscope" => vec![
            ("Mirr", long(num(f, "mirrors", 6.0))),
            ("MRot", long(num(f, "angle", 0.0))),
        ],
        "Dents" => vec![
            ("Scl ", Value::Double(num(f, "scale", 25.0))),
            ("Refr", Value::Double(num(f, "refraction", 50.0))),
            ("Dtl ", Value::Double(10.0)),
            ("Trbl", Value::Double(num(f, "turbulence", 10.0))),
            ("RndS", Value::Integer(8_438_429)),
        ],
        "Repeat" => vec![
            ("Scl ", pct(num(f, "scale", 100.0))),
            ("Rsft", pct(num(f, "row_shift", 0.0))),
            ("SpcX", pct(num(f, "space_x", 0.0))),
            ("SpcY", pct(num(f, "space_y", 0.0))),
            ("SpcC", Value::Bool(flag(f, "auto_color"))),
            ("Angl", long(num(f, "angle", 0.0))),
        ],
        "ColorToAlpha" => vec![
            ("Trsp", pct(num(f, "transparency", 0.0))),
            ("Opct", pct(num(f, "opacity", 100.0))),
            ("Clr ", rgbc_of(colour(f, "color", BLACK))),
        ],
        "Dither" => vec![
            ("Plte", long(num(f, "palette", 0.0))),
            ("Mthd", long(num(f, "method", 1.0))),
        ],
        "Particles" => vec![
            ("Cont", long(num(f, "count", 10.0))),
            ("Size", long(num(f, "size", 8.0))),
            ("Dpth", long(num(f, "depth", 100.0))),
            ("Brgh", long(num(f, "brightness", 800.0))),
            ("Clr ", rgbc_of(colour(f, "color", WHITE))),
            ("Time", Value::Double(num(f, "time", 0.0))),
            ("Turb", long(num(f, "turbulence", 0.0))),
            (
                "Blnk",
                Value::Bool(f.params.get("blink") != Some(&SmartParam::Bool(false))),
            ),
            ("Fall", Value::Bool(flag(f, "fall"))),
            ("RndS", Value::Integer(8_438_429)),
        ],
        "NormalMap" => {
            let detail = |k: &str| Value::Double(num(f, k, 100.0) / 100.0);
            vec![
                ("blur", Value::Double(num(f, "blur", 0.0))),
                (
                    "textureScale",
                    Value::Double(num(f, "scale", 100.0) / 100.0),
                ),
                (
                    "Scl ",
                    Value::Double(if flag(f, "invert") { -1.0 } else { 1.0 }),
                ),
                (
                    "Dtl ",
                    Value::List(vec![detail("high"), detail("medium"), detail("low")]),
                ),
            ]
        }
        "TextureDilation" => vec![
            ("Crop", px(num(f, "crop", 0.0))),
            ("Rds ", px(num(f, "radius", 10.0))),
        ],
        _ => Vec::new(),
    };
    let settings = (!items.is_empty()).then(|| {
        let mut d = Descriptor::new(code);
        for (k, v) in items {
            let _ = d.push(k, v);
        }
        d
    });
    Written { settings, fg, bg }
}

/// The parameters Photoshop's keys in `s` (and the entry's foreground and
/// background colours) spell for the filter keyed `key`, in the complete
/// form this build's dialogs store — before any [`EXTRA_PREFIX`] key.
fn photoshop_params(
    key: &str,
    s: &Descriptor,
    fg: [f32; 4],
    bg: [f32; 4],
) -> BTreeMap<String, SmartParam> {
    let n = |k: &str, default: f64| s.number(k).filter(|v| v.is_finite()).unwrap_or(default);
    let en = |k: &str| match s.get(k) {
        Some(Value::Enumerated { value, .. }) => Some(value.trim_end().to_string()),
        _ => None,
    };
    let index = |k: &str, list: &[&str], default: u32| {
        SmartParam::Choice(
            en(k)
                .and_then(|v| list.iter().position(|c| c.trim_end() == v))
                .map_or(default, |i| i as u32),
        )
    };
    let b = |k: &str, default: bool| match s.get(k) {
        Some(Value::Bool(v)) => *v,
        _ => default,
    };
    let rgb = |k: &str, default: [f32; 4]| {
        SmartParam::Color(s.descriptor(k).and_then(colour_of).unwrap_or(default))
    };
    let float = |v: f64| SmartParam::Float(v as f32);
    let int =
        |v: f64| SmartParam::Int(v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32);
    let edge = ("edge", SmartParam::Choice(0));
    let wrapped = |k: &str, wrap: &str| {
        (
            "edge",
            SmartParam::Choice(u32::from(en(k).as_deref() == Some(wrap))),
        )
    };
    let params: Vec<(&str, SmartParam)> = match key {
        "BoxBlur" => vec![("radius", int(n("Rds ", 4.0))), edge],
        "GaussianBlur" => vec![("radius", float(n("Rds ", 4.0))), edge],
        "MotionBlur" => vec![
            ("angle", float(n("Angl", 0.0))),
            ("distance", float(n("Dstn", 16.0))),
        ],
        "SurfaceBlur" => vec![
            ("radius", int(n("Rds ", 5.0))),
            ("threshold", float(n("Thsh", 13.0) / 255.0)),
            edge,
        ],
        "UnsharpMask" => vec![
            ("amount", float(n("Amnt", 100.0) / 100.0)),
            ("radius", float(n("Rds ", 2.0))),
            ("threshold", float(n("Thsh", 0.0) / 255.0)),
            edge,
        ],
        "AddNoise" => vec![
            ("amount", float(n("Nose", 10.0) / 100.0)),
            (
                "distribution",
                SmartParam::Choice(u32::from(en("Dstr").as_deref() == Some("Gsn"))),
            ),
            ("monochromatic", SmartParam::Bool(b("Mnch", false))),
            ("seed", int(n("FlRs", 1.0))),
        ],
        "DustAndScratches" => vec![
            ("radius", int(n("Rds ", 2.0))),
            ("threshold", float(n("Thsh", 13.0) / 255.0)),
            edge,
        ],
        "Median" => vec![("radius", int(n("Rds ", 2.0))), edge],
        "HighPass" => vec![("radius", float(n("Rds ", 10.0))), edge],
        "Mosaic" => vec![("cell", int(n("ClSz", 8.0)))],
        "Maximum" | "Minimum" => vec![("radius", int(n("Rds ", 2.0))), edge],
        // ------------------------------------------------------ W18-J
        "RadialBlur" => vec![
            ("kind", index("BlrM", &["Spn", "Zm"], 0)),
            ("amount", float(n("Amnt", 10.0))),
            (
                "samples",
                SmartParam::Int(match en("BlrQ").as_deref() {
                    Some("Drft") => 8,
                    Some("Bst") => 64,
                    _ => 16,
                }),
            ),
        ],
        "SmartBlur" => vec![
            ("radius", int(n("Rds ", 3.0))),
            ("threshold", float(n("Thsh", 10.0) / 100.0)),
            ("mode", index("SmBM", &["SBMN", "SBME", "SBMO"], 0)),
            edge,
        ],
        "SmartSharpen" => vec![
            ("amount", float(n("Amnt", 100.0) / 100.0)),
            ("radius", float(n("Rds ", 2.0))),
            ("noise_floor", float(n("noiseReduction", 2.0) / 100.0)),
            edge,
        ],
        "ReduceNoise" => {
            let channel = match s.get("channelDenoise") {
                Some(Value::List(list)) => match list.first() {
                    Some(Value::Descriptor(d)) => d.clone(),
                    _ => Descriptor::default(),
                },
                _ => Descriptor::default(),
            };
            let c = |k: &str, default: f64| {
                channel
                    .number(k)
                    .filter(|v| v.is_finite())
                    .unwrap_or(default)
            };
            vec![
                ("strength", float(c("Amnt", 4.0))),
                ("detail", float(c("EdgF", 50.0) / 100.0)),
                edge,
            ]
        }
        "Pinch" => vec![
            ("radius", float(128.0)),
            ("amount", float(n("Amnt", 50.0) / 100.0)),
        ],
        "PolarCoordinates" => vec![("mode", index("Cnvr", &["RctP", "PlrR"], 0)), edge],
        "Ripple" => vec![
            ("amount", float(n("Amnt", 8.0))),
            (
                "wavelength",
                float(match en("RplS").as_deref() {
                    Some("Sml") => 10.0,
                    Some("Lrg") => 100.0,
                    _ => 40.0,
                }),
            ),
        ],
        "Spherize" => vec![
            ("radius", float(128.0)),
            ("amount", float(n("Amnt", 50.0) / 100.0)),
        ],
        "Twirl" => vec![("radius", float(128.0)), ("angle", float(n("Angl", 90.0)))],
        "Wave" => vec![
            ("kind", index("Wvtp", &["WvSn", "WvTr", "WvSq"], 0)),
            ("amplitude", float((n("AmMn", 8.0) + n("AmMx", 8.0)) / 2.0)),
            (
                "wavelength",
                float((n("WLMn", 40.0) + n("WLMx", 40.0)) / 2.0),
            ),
            ("phase", float(0.0)),
            wrapped("UndA", "WrpA"),
        ],
        "ZigZag" => vec![
            ("kind", index("ZZTy", &["PndR", "OtFr", "ArnC"], 0)),
            ("radius", float(128.0)),
            ("amount", float(n("Amnt", 8.0))),
            ("ridges", float(n("NmbR", 5.0))),
        ],
        "ColorHalftone" => vec![
            ("radius", float(n("Rds ", 4.0))),
            ("angle_r", float(n("Ang1", 108.0))),
            ("angle_g", float(n("Ang2", 162.0))),
            ("angle_b", float(n("Ang3", 90.0))),
        ],
        "Crystallize" => vec![
            ("cell", int(n("ClSz", 16.0))),
            ("seed", int(n("FlRs", 1.0))),
        ],
        "Pointillize" => vec![
            ("cell", int(n("ClSz", 16.0))),
            ("seed", int(n("FlRs", 1.0))),
            ("background", SmartParam::Color(bg)),
        ],
        "Mezzotint" => vec![
            (
                "kind",
                index(
                    "MztT",
                    &[
                        "FnDt", "MdmD", "GrnD", "CrsD", "ShrL", "MdmL", "LngL", "ShSt", "MdmS",
                        "LngS",
                    ],
                    1,
                ),
            ),
            ("seed", int(n("FlRs", 1.0))),
        ],
        "ShapeMosaic" => vec![
            ("cell_size", int(n("ClSz", 12.0))),
            ("shape", SmartParam::Choice(n("Shap", 0.0).max(0.0) as u32)),
            ("spread", SmartParam::Choice(n("Sprd", 0.0).max(0.0) as u32)),
            ("monochromatic", SmartParam::Bool(b("Mono", false))),
            ("invert", SmartParam::Bool(b("Invr", false))),
        ],
        "Fibers" => vec![
            ("variance", float(n("Vrnc", 22.4) / 64.0)),
            ("strength", float(n("Strg", 32.0) / 64.0)),
            ("seed", int(n("RndS", 1.0))),
            ("from", SmartParam::Color(fg)),
            ("to", SmartParam::Color(bg)),
        ],
        "LensFlare" => vec![
            ("brightness", float(n("Brgh", 100.0) / 100.0)),
            ("radius", float(120.0)),
            ("ghosts", SmartParam::Int(5)),
            ("streaks", SmartParam::Int(6)),
        ],
        "Diffuse" => vec![
            ("radius", SmartParam::Int(4)),
            ("mode", index("Md  ", &["Nrml", "DrkO", "LghO"], 0)),
            ("seed", int(n("FlRs", 1.0))),
            edge,
        ],
        "Emboss" => vec![
            ("angle", float(n("Angl", 135.0))),
            ("height", float(n("Hght", 3.0))),
            ("amount", float(n("Amnt", 100.0) / 100.0)),
        ],
        "Extrude" => vec![
            ("kind", index("ExtT", &["Blks", "Pyrm"], 0)),
            ("size", int(n("ExtS", 30.0))),
            ("depth", float(n("ExtD", 30.0))),
            ("depth_mode", index("ExtR", &["Rndm", "LvlB"], 0)),
            ("seed", int(n("FlRs", 1.0))),
        ],
        "TraceContour" => vec![
            ("level", int(n("Lvl ", 128.0))),
            ("side", index("Edg ", &["Lwr", "Upr"], 0)),
            edge,
        ],
        "Wind" => vec![
            ("direction", index("Drct", &["Left", "Rght"], 0)),
            (
                "strength",
                float(match en("WndM").as_deref() {
                    Some("Blst") => 0.75,
                    Some("Stgr") => 1.0,
                    _ => 0.5,
                }),
            ),
            ("seed", SmartParam::Int(1)),
            edge,
        ],
        "Custom" => {
            let matrix: Vec<f64> = match s.get("Mtrx") {
                Some(Value::List(list)) if list.len() == 25 => list
                    .iter()
                    .map(|v| match v {
                        Value::Integer(i) => f64::from(*i),
                        Value::Double(d) => *d,
                        _ => 0.0,
                    })
                    .collect(),
                _ => {
                    let mut m = vec![0.0; 25];
                    m[12] = 1.0;
                    m
                }
            };
            let taps = [
                "w00", "w01", "w02", "w10", "w11", "w12", "w20", "w21", "w22",
            ];
            let mut out: Vec<(&str, SmartParam)> = taps
                .iter()
                .enumerate()
                .map(|(i, tap)| (*tap, float(matrix[(i / 3 + 1) * 5 + i % 3 + 1])))
                .collect();
            out.push(("divisor", float(n("Scl ", 1.0))));
            out.push(("bias", float(n("Ofst", 0.0) / 255.0)));
            out.push(edge);
            out
        }
        "Offset" => vec![
            ("dx", int(n("Hrzn", 0.0))),
            ("dy", int(n("Vrtc", 0.0))),
            wrapped("Fl  ", "Wrp"),
        ],
        "HsbHsl" => vec![
            ("input", index("Inpt", &["RGBC", "HSBl", "HSLC"], 0)),
            ("output", index("Otpt", &["RGBC", "HSBl", "HSLC"], 1)),
        ],
        "Kaleidoscope" => vec![
            ("mirrors", int(n("Mirr", 6.0))),
            ("angle", int(n("MRot", 0.0))),
        ],
        "Dents" => vec![
            ("scale", float(n("Scl ", 25.0))),
            ("refraction", float(n("Refr", 50.0))),
            ("turbulence", float(n("Trbl", 10.0))),
        ],
        "Repeat" => vec![
            ("scale", float(n("Scl ", 100.0))),
            ("row_shift", float(n("Rsft", 0.0))),
            ("space_x", float(n("SpcX", 0.0))),
            ("space_y", float(n("SpcY", 0.0))),
            ("auto_color", SmartParam::Bool(b("SpcC", false))),
            ("angle", float(n("Angl", 0.0))),
        ],
        "ColorToAlpha" => vec![
            ("color", rgb("Clr ", BLACK)),
            ("transparency", float(n("Trsp", 0.0))),
            ("opacity", float(n("Opct", 100.0))),
        ],
        "Dither" => vec![
            (
                "palette",
                SmartParam::Choice(n("Plte", 0.0).max(0.0) as u32),
            ),
            ("method", SmartParam::Choice(n("Mthd", 1.0).max(0.0) as u32)),
        ],
        "Particles" => vec![
            ("count", float(n("Cont", 10.0))),
            ("size", int(n("Size", 8.0))),
            ("depth", float(n("Dpth", 100.0))),
            ("brightness", float(n("Brgh", 800.0))),
            ("color", rgb("Clr ", WHITE)),
            ("time", float(n("Time", 0.0))),
            ("turbulence", float(n("Turb", 0.0))),
            ("blink", SmartParam::Bool(b("Blnk", true))),
            ("fall", SmartParam::Bool(b("Fall", false))),
        ],
        "NormalMap" => {
            let detail: Vec<f64> = match s.get("Dtl ") {
                Some(Value::List(list)) => list
                    .iter()
                    .map(|v| match v {
                        Value::Double(d) => *d,
                        _ => 1.0,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let d = |i: usize| float(detail.get(i).copied().unwrap_or(1.0) * 100.0);
            vec![
                ("blur", float(n("blur", 0.0))),
                ("scale", float(n("textureScale", 1.0) * 100.0)),
                ("invert", SmartParam::Bool(n("Scl ", 1.0) < 0.0)),
                ("high", d(0)),
                ("medium", d(1)),
                ("low", d(2)),
            ]
        }
        "TextureDilation" => vec![
            ("crop", int(n("Crop", 0.0))),
            ("radius", int(n("Rds ", 10.0))),
        ],
        _ => Vec::new(),
    };
    params
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// One `filterFX` entry for `f`, or why a `.psd` cannot hold it.
///
/// W18-J: every parameter Photoshop's keys cannot hold exactly — a setting
/// Photoshop lacks (an edge mode, a distortion's radius, a seed), a value
/// finer than its grid, a colour outside `0..=1` — is also written under an
/// [`EXTRA_PREFIX`] key, found by reading the Photoshop keys back and
/// comparing; so this build reads every parameter back exactly, and
/// Photoshop sees the nearest setting it has.
pub fn encode_filter(f: &SmartFilter) -> Result<Descriptor, String> {
    let Some((_, code)) = MAPPED.iter().find(|(k, _)| *k == f.filter) else {
        let why = rasterised_reason(&f.filter)
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        return Err(format!(
            "{} has no Photoshop smart filter{why}",
            display_name(&f.filter)
        ));
    };
    let written = photoshop_settings(f, code);
    let fg = written.fg.unwrap_or(BLACK);
    let bg = written.bg.unwrap_or(WHITE);
    let mut settings = written.settings;
    let spelled = photoshop_params(
        &f.filter,
        settings.as_ref().unwrap_or(&Descriptor::default()),
        colour_of_value(&rgbc_of(fg)).unwrap_or(fg),
        colour_of_value(&rgbc_of(bg)).unwrap_or(bg),
    );
    for (key, value) in &f.params {
        if spelled.get(key) != Some(value) {
            let d = settings.get_or_insert_with(|| Descriptor::new(code));
            let _ = d.push(&format!("{EXTRA_PREFIX}{key}"), extra_value(*value));
        }
    }
    let mut blend = Descriptor::new("blendOptions");
    let _ = blend.push("Opct", pct(f64::from(f.effective_opacity()) * 100.0));
    let _ = blend.push("Md  ", enumerated("BlnM", blend_code(f.blend_mode)));
    let mut d = Descriptor::new("filterFX");
    let mut push = |k: &str, v: Value| {
        let _ = d.push(k, v);
    };
    push("Nm  ", Value::Text(display_name(&f.filter)));
    push("blendOptions", Value::Descriptor(blend));
    push("enab", Value::Bool(f.enabled));
    push("hasoptions", Value::Bool(settings.is_some()));
    push("FrgC", rgbc_of(fg));
    push("BckC", rgbc_of(bg));
    if let Some(s) = settings {
        push("Fltr", Value::Descriptor(s));
    }
    push("filterID", Value::Integer(filter_id(code)));
    Ok(d)
}

fn colour_of_value(v: &Value) -> Option<[f32; 4]> {
    match v {
        Value::Descriptor(d) => colour_of(d),
        _ => None,
    }
}

/// W18-J: the smart filters' shared mask settings, as `filterFXStyle`
/// carries them beside the mask's pixels (which ride in the document's
/// `FEid` block, see [`FilterEffects`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterMaskFlags {
    /// `filterMaskEnable`.
    pub enabled: bool,
    /// `filterMaskLinked`: the mask moves with the object.
    pub linked: bool,
    /// Photopea's `filterMaskDensity`, `0..=255` (255, the default, is not
    /// written).
    pub density: u8,
    /// Photopea's `filterMaskFeather` in pixels (0 is not written).
    pub feather: f64,
    /// This build's invert switch. Photoshop's filter mask has none, so the
    /// written pixels are already inverted and this extra key says so.
    pub inverted: bool,
}

impl Default for FilterMaskFlags {
    fn default() -> Self {
        FilterMaskFlags {
            enabled: true,
            linked: true,
            density: 255,
            feather: 0.0,
            inverted: false,
        }
    }
}

/// The mask settings in a decoded `filterFX` (defaults for what is absent).
pub fn decode_mask_flags(fx: &Descriptor) -> FilterMaskFlags {
    let b = |k: &str, default: bool| match fx.get(k) {
        Some(Value::Bool(v)) => *v,
        _ => default,
    };
    FilterMaskFlags {
        enabled: b("filterMaskEnable", true),
        linked: b("filterMaskLinked", true),
        density: fx
            .number("filterMaskDensity")
            .filter(|v| v.is_finite())
            .map_or(255, |v| v.round().clamp(0.0, 255.0) as u8),
        feather: fx
            .number("filterMaskFeather")
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(0.0),
        inverted: b(&format!("{EXTRA_PREFIX}inverted"), false),
    }
}

/// The whole stack as the `filterFX` descriptor (class `filterFXStyle`), or
/// every reason a filter in it cannot be written. The stack's order is kept:
/// index 0 is applied first in both.
pub fn encode_stack(stack: &[SmartFilter]) -> Result<Descriptor, Vec<String>> {
    encode_stack_masked(stack, None)
}

/// W18-J: [`encode_stack`] with the smart filters' shared mask settings
/// (`None`: the object has no filter mask). The mask's pixels go in the
/// document's `FEid` block ([`encode_filter_effects`]).
pub fn encode_stack_masked(
    stack: &[SmartFilter],
    mask: Option<FilterMaskFlags>,
) -> Result<Descriptor, Vec<String>> {
    let mut list = Vec::with_capacity(stack.len());
    let mut refused = Vec::new();
    for f in stack {
        match encode_filter(f) {
            Ok(d) => list.push(Value::Descriptor(d)),
            Err(why) => refused.push(why),
        }
    }
    if !refused.is_empty() {
        return Err(refused);
    }
    let mut d = Descriptor::new("filterFXStyle");
    let mut push = |k: &str, v: Value| {
        let _ = d.push(k, v);
    };
    push("enab", Value::Bool(true));
    push("validAtPosition", Value::Bool(true));
    let flags = mask.unwrap_or_default();
    push(
        "filterMaskEnable",
        Value::Bool(mask.is_some() && flags.enabled),
    );
    push("filterMaskLinked", Value::Bool(flags.linked));
    push("filterMaskExtendWithWhite", Value::Bool(true));
    if flags.density != 255 {
        push(
            "filterMaskDensity",
            Value::Integer(i32::from(flags.density)),
        );
    }
    if flags.feather != 0.0 {
        push("filterMaskFeather", Value::Double(flags.feather));
    }
    if flags.inverted {
        push(&format!("{EXTRA_PREFIX}inverted"), Value::Bool(true));
    }
    push("filterFXList", Value::List(list));
    Ok(d)
}

/// A decoded `filterFX`: the filters this build runs, in order, and the
/// names of those it has no equivalent for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecodedStack {
    pub filters: Vec<SmartFilter>,
    pub unmapped: Vec<String>,
}

/// The event code a `filterFX` entry names: `filterID` when it spells four
/// characters, else the `Fltr` class. Trailing spaces are trimmed.
fn code_of(item: &Descriptor) -> Option<String> {
    let from_id = match item.get("filterID") {
        Some(Value::Integer(v)) if (*v as u32) > 0x00FF_FFFF => {
            let b = (*v as u32).to_be_bytes();
            b.iter()
                .all(|c| c.is_ascii_graphic() || *c == b' ')
                .then(|| b.iter().map(|&c| c as char).collect::<String>())
        }
        _ => None,
    };
    from_id
        .or_else(|| item.descriptor("Fltr").map(|f| f.class_id.clone()))
        .map(|c| c.trim_end().to_string())
}

fn decode_filter(item: &Descriptor) -> Result<SmartFilter, String> {
    let name = || {
        item.text("Nm  ")
            .map(|s| s.trim_end_matches('\0').to_string())
            .unwrap_or_else(|| "an unnamed filter".into())
    };
    let code = code_of(item).ok_or_else(name)?;
    let (key, _) = MAPPED
        .iter()
        .find(|(_, c)| c.trim_end() == code)
        .ok_or_else(name)?;
    let s = item.descriptor("Fltr").cloned().unwrap_or_default();
    let fg = item.descriptor("FrgC").and_then(colour_of).unwrap_or(BLACK);
    let bg = item.descriptor("BckC").and_then(colour_of).unwrap_or(WHITE);
    let mut params = photoshop_params(key, &s, fg, bg);
    // W18-J: what Photoshop's keys cannot hold, exactly as it was stored.
    for (k, v) in &s.items {
        if let Some(param) = k.strip_prefix(EXTRA_PREFIX) {
            if let Some(value) = param_of_extra(v) {
                params.insert(param.to_string(), value);
            }
        }
    }
    let mut f = SmartFilter::new(*key, params);
    f.enabled = !matches!(item.get("enab"), Some(Value::Bool(false)));
    if let Some(b) = item.descriptor("blendOptions") {
        if let Some(o) = b.number("Opct").filter(|v| v.is_finite()) {
            f.opacity = (o / 100.0).clamp(0.0, 1.0) as f32;
        }
        if let Some(Value::Enumerated { value, .. }) = b.get("Md  ") {
            f.blend_mode = blend_from_blnm(value).unwrap_or(BlendMode::Normal);
        }
    }
    Ok(f)
}

/// Decode a `filterFX` descriptor. A master switch that is off turns every
/// filter off (each keeps its settings). An entry this build has no filter
/// for is named in [`DecodedStack::unmapped`] and left out.
pub fn decode_stack(fx: &Descriptor) -> DecodedStack {
    let mut out = DecodedStack::default();
    let master = !matches!(fx.get("enab"), Some(Value::Bool(false)));
    let Some(Value::List(items)) = fx.get("filterFXList") else {
        return out;
    };
    for item in items {
        let Value::Descriptor(item) = item else {
            out.unmapped.push("an entry that is not a filter".into());
            continue;
        };
        match decode_filter(item) {
            Ok(mut f) => {
                f.enabled &= master;
                out.filters.push(f);
            }
            Err(name) => out.unmapped.push(name),
        }
    }
    out
}

/// The `filterFX` descriptor in `layer`'s `SoLd` (or `SoLE`) block, when it
/// has one.
pub fn filter_fx_of(layer: &PsdLayer, opts: &ReadOptions) -> Option<Descriptor> {
    let block = layer
        .extra
        .iter()
        .find(|b| &b.key == b"SoLd")
        .or_else(|| layer.extra.iter().find(|b| &b.key == b"SoLE"))?;
    let mut cur = Cursor::new(&block.data);
    let _key = cur.tag().ok()?;
    let _version = cur.u32().ok()?;
    let _descriptor_version = cur.u32().ok()?;
    let d = Descriptor::read(&mut cur, opts).ok()?;
    d.descriptor(FILTER_FX_KEY).cloned()
}

// ------------------------------------------ W18-J: the smart-filter mask

/// The document-level tagged block holding each smart object's smart-filter
/// mask pixels (Photoshop's "Filter Effects"; `FXid` is read too).
pub const FILTER_EFFECTS_KEY: [u8; 4] = *b"FEid";

/// The mask's pixels in one [`FilterEffects`] entry: a rectangle in canvas
/// pixels and one sample per pixel. Outside the rectangle the mask is white
/// (the filters show), as Photopea reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterEffectsMask {
    pub rect: Rect,
    /// Row-major, `rect.width() * rect.height()` samples at the entry's
    /// depth (big-endian pairs at 16 bits).
    pub data: Vec<u8>,
}

/// One smart object's entry in the `FEid` block, laid out as Photopea
/// writes and reads it: the placed layer's id (the `placed` key of its
/// `SoLd`), the object's unfiltered pixels (red, green, blue and
/// transparency planes over `rect`, in canvas pixels) and its smart-filter
/// mask.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterEffects {
    pub id: String,
    pub rect: Rect,
    /// 8 or 16.
    pub depth: u16,
    /// Red, green, blue and transparency planes, each `rect.width() *
    /// rect.height()` samples at `depth`; an absent plane was not written.
    pub planes: [Option<Vec<u8>>; 4],
    pub mask: Option<FilterEffectsMask>,
}

/// The slot each of [`FilterEffects::planes`] takes among the 26 the block
/// lists (24 colour channels, then two more; the transparency is the last).
const PLANE_SLOTS: [usize; 4] = [0, 1, 2, 25];

fn put_rect(s: &mut Sink, r: Rect) {
    s.i32(r.top);
    s.i32(r.left);
    s.i32(r.bottom);
    s.i32(r.right);
}

/// An 8-byte length Photopea writes as a zero word and the length.
fn put_len64(s: &mut Sink, len: usize) {
    s.u32(0);
    s.u32(u32::try_from(len).unwrap_or(u32::MAX));
}

/// A plane: its length, raw compression (0) and samples.
fn put_plane(s: &mut Sink, data: &[u8]) {
    put_len64(s, data.len() + 2);
    s.u16(0);
    s.bytes(data);
}

/// The whole `FEid` payload for `entries`.
pub fn encode_filter_effects(entries: &[FilterEffects]) -> Vec<u8> {
    let mut s = Sink::new();
    s.u32(3);
    for e in entries {
        let mut body = Sink::new();
        body.pascal_string(&e.id, 1);
        body.u32(1);
        let mut inner = Sink::new();
        put_rect(&mut inner, e.rect);
        inner.u32(u32::from(e.depth));
        inner.u32(24);
        for slot in 0..26 {
            let plane = PLANE_SLOTS
                .iter()
                .position(|&p| p == slot)
                .and_then(|i| e.planes[i].as_deref());
            match plane {
                Some(data) => {
                    inner.u32(1);
                    put_plane(&mut inner, data);
                }
                None => inner.u32(0),
            }
        }
        let inner = inner.into_inner();
        put_len64(&mut body, inner.len());
        body.bytes(&inner);
        match &e.mask {
            Some(m) => {
                body.u8(1);
                put_rect(&mut body, m.rect);
                put_plane(&mut body, &m.data);
            }
            None => body.u8(0),
        }
        let body = body.into_inner();
        put_len64(&mut s, body.len());
        s.bytes(&body);
        s.zeros((4 - body.len() % 4) % 4);
    }
    s.into_inner()
}

fn read_rect(c: &mut Cursor<'_>) -> PsdResult<Rect> {
    let top = c.i32()?;
    let left = c.i32()?;
    let bottom = c.i32()?;
    let right = c.i32()?;
    Ok(Rect {
        top,
        left,
        bottom,
        right,
    })
}

fn read_len64(c: &mut Cursor<'_>) -> PsdResult<usize> {
    let hi = c.u32()?;
    let lo = c.u32()?;
    if hi != 0 {
        return Err(PsdError::InvalidDocument(
            "a filter-effects length past 4 GiB".into(),
        ));
    }
    Ok(lo as usize)
}

/// A plane's samples: raw (0) or PackBits (1, with a row-length table of
/// 16-bit words, or 32-bit ones in a large document).
fn read_plane(bytes: &[u8], rows: usize, row_bytes: usize) -> PsdResult<Vec<u8>> {
    let mut c = Cursor::new(bytes);
    let compression = c.u16()?;
    let expected = rows * row_bytes;
    match compression {
        0 => Ok(c.take(expected)?.to_vec()),
        1 => {
            let rest = c.peek_rest();
            for word in [2usize, 4] {
                let table = rows * word;
                if rest.len() < table {
                    continue;
                }
                let counts: Vec<usize> = rest[..table]
                    .chunks(word)
                    .map(|w| w.iter().fold(0usize, |a, &b| (a << 8) | usize::from(b)))
                    .collect();
                if table + counts.iter().sum::<usize>() != rest.len() {
                    continue;
                }
                let mut out = Vec::with_capacity(expected);
                let mut at = table;
                for (row, n) in counts.iter().enumerate() {
                    out.extend(crate::packbits::decode_exact(
                        &rest[at..at + n],
                        row_bytes,
                        row,
                    )?);
                    at += n;
                }
                return Ok(out);
            }
            Err(PsdError::InvalidDocument(
                "a filter-effects plane's PackBits rows do not add up".into(),
            ))
        }
        other => Err(PsdError::InvalidDocument(format!(
            "filter-effects compression {other} is not read"
        ))),
    }
}

/// Parse an `FEid`/`FXid` payload. An entry that does not parse ends the
/// list (the entries before it are kept).
pub fn decode_filter_effects(data: &[u8]) -> Vec<FilterEffects> {
    let mut out = Vec::new();
    let mut c = Cursor::new(data);
    if c.u32().is_err() {
        return out;
    }
    while c.remaining() >= 8 {
        let Ok(len) = read_len64(&mut c) else { break };
        let Ok(body) = c.take(len) else { break };
        let _ = c.skip(((4 - len % 4) % 4).min(c.remaining()));
        match decode_entry(body) {
            Ok(e) => out.push(e),
            Err(_) => break,
        }
    }
    out
}

fn decode_entry(body: &[u8]) -> PsdResult<FilterEffects> {
    let mut c = Cursor::new(body);
    let n = usize::from(c.u8()?);
    let id: String = c.take(n)?.iter().map(|&b| b as char).collect();
    let _version = c.u32()?;
    let inner_len = read_len64(&mut c)?;
    let mut inner = Cursor::new(c.take(inner_len)?);
    let rect = read_rect(&mut inner)?;
    let depth = u16::try_from(inner.u32()?).unwrap_or(0);
    if depth != 8 && depth != 16 {
        return Err(PsdError::InvalidDocument(format!(
            "filter-effects depth {depth}"
        )));
    }
    let bytes_per = usize::from(depth / 8);
    let max = inner.u32()? as usize;
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let mut planes: [Option<Vec<u8>>; 4] = [None, None, None, None];
    for slot in 0..max.saturating_add(2).min(1024) {
        if inner.u32()? == 0 {
            continue;
        }
        let len = read_len64(&mut inner)?;
        let bytes = inner.take(len)?;
        if let Some(i) = PLANE_SLOTS.iter().position(|&p| p == slot) {
            planes[i] = Some(read_plane(bytes, h, w * bytes_per)?);
        }
    }
    let mask = if c.remaining() > 0 && c.u8()? != 0 {
        let rect = read_rect(&mut c)?;
        let len = read_len64(&mut c)?;
        let data = read_plane(
            c.take(len)?,
            rect.height() as usize,
            rect.width() as usize * bytes_per,
        )?;
        Some(FilterEffectsMask { rect, data })
    } else {
        None
    };
    Ok(FilterEffects {
        id,
        rect,
        depth,
        planes,
        mask,
    })
}

/// Every filter-effects entry in `file`'s document-level `FEid`/`FXid`
/// blocks.
pub fn filter_effects_of(extra: &[TaggedBlock]) -> Vec<FilterEffects> {
    extra
        .iter()
        .filter(|b| &b.key == b"FEid" || &b.key == b"FXid")
        .flat_map(|b| decode_filter_effects(&b.data))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Sink;
    use crate::header::PsdHeader;
    use crate::model::{PsdFile, Rect};
    use crate::placed::PlacedLayer;

    fn filter(key: &str, params: &[(&str, SmartParam)]) -> SmartFilter {
        SmartFilter::new(
            key,
            params.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        )
    }

    /// Every mapped filter, with settings on Photoshop's grid, in the
    /// complete form the decoder produces.
    fn every_mapped() -> Vec<SmartFilter> {
        use SmartParam::{Bool, Choice, Float, Int};
        let edge = ("edge", Choice(0));
        let mut blur = filter("GaussianBlur", &[("radius", Float(3.5)), edge]);
        blur.opacity = 0.5;
        blur.blend_mode = BlendMode::Multiply;
        let mut off = filter("Median", &[("radius", Int(3)), edge]);
        off.enabled = false;
        let mut out = vec![
            filter("Average", &[]),
            filter("Blur", &[]),
            filter("BlurMore", &[]),
            filter("BoxBlur", &[("radius", Int(6)), edge]),
            blur,
            filter(
                "MotionBlur",
                &[("angle", Float(-30.0)), ("distance", Float(12.5))],
            ),
            filter(
                "SurfaceBlur",
                &[("radius", Int(7)), ("threshold", Float(51.0 / 255.0)), edge],
            ),
            filter("Sharpen", &[]),
            filter("SharpenMore", &[]),
            filter("SharpenEdges", &[]),
            filter(
                "UnsharpMask",
                &[
                    ("amount", Float(1.5)),
                    ("radius", Float(2.25)),
                    ("threshold", Float(10.0 / 255.0)),
                    edge,
                ],
            ),
            filter(
                "AddNoise",
                &[
                    ("amount", Float(0.25)),
                    ("distribution", Choice(1)),
                    ("monochromatic", Bool(true)),
                    ("seed", Int(42)),
                ],
            ),
            filter("Despeckle", &[]),
            filter(
                "DustAndScratches",
                &[("radius", Int(4)), ("threshold", Float(20.0 / 255.0)), edge],
            ),
            off,
            filter("Mosaic", &[("cell", Int(9))]),
            filter("HighPass", &[("radius", Float(6.5)), edge]),
            filter("Maximum", &[("radius", Int(2)), edge]),
            filter("Minimum", &[("radius", Int(5)), edge]),
        ];
        out.extend(w18_mapped());
        out
    }

    /// W18-J: every filter W18-J maps, with settings on Photoshop's grid
    /// and the settings Photoshop lacks at the values its keys decode to, in
    /// the complete form the decoder produces.
    fn w18_mapped() -> Vec<SmartFilter> {
        use SmartParam::{Bool, Choice, Color, Float, Int};
        let edge = ("edge", Choice(0));
        let taps = |centre: f32| {
            let mut t: Vec<(&str, SmartParam)> = [
                "w00", "w01", "w02", "w10", "w11", "w12", "w20", "w21", "w22",
            ]
            .iter()
            .map(|k| (*k, Float(0.0)))
            .collect();
            t[1].1 = Float(-1.0);
            t[3].1 = Float(-1.0);
            t[4].1 = Float(centre);
            t[5].1 = Float(-1.0);
            t[7].1 = Float(-1.0);
            t.push(("divisor", Float(1.0)));
            t.push(("bias", Float(0.0)));
            t.push(("edge", Choice(0)));
            t
        };
        vec![
            filter(
                "RadialBlur",
                &[
                    ("kind", Choice(1)),
                    ("amount", Float(25.0)),
                    ("samples", Int(16)),
                ],
            ),
            filter(
                "SmartBlur",
                &[
                    ("radius", Int(5)),
                    ("threshold", Float(0.25)),
                    ("mode", Choice(2)),
                    edge,
                ],
            ),
            filter(
                "SmartSharpen",
                &[
                    ("amount", Float(1.5)),
                    ("radius", Float(2.5)),
                    ("noise_floor", Float(0.25)),
                    edge,
                ],
            ),
            filter(
                "ReduceNoise",
                &[("strength", Float(6.0)), ("detail", Float(0.25)), edge],
            ),
            filter(
                "Pinch",
                &[("radius", Float(128.0)), ("amount", Float(0.25))],
            ),
            filter("PolarCoordinates", &[("mode", Choice(1)), edge]),
            filter(
                "Ripple",
                &[("amount", Float(-20.0)), ("wavelength", Float(100.0))],
            ),
            filter(
                "Spherize",
                &[("radius", Float(128.0)), ("amount", Float(-0.5))],
            ),
            filter(
                "Twirl",
                &[("radius", Float(128.0)), ("angle", Float(-45.0))],
            ),
            filter(
                "Wave",
                &[
                    ("kind", Choice(2)),
                    ("amplitude", Float(12.0)),
                    ("wavelength", Float(60.0)),
                    ("phase", Float(0.0)),
                    ("edge", Choice(1)),
                ],
            ),
            filter(
                "ZigZag",
                &[
                    ("kind", Choice(1)),
                    ("radius", Float(128.0)),
                    ("amount", Float(-12.0)),
                    ("ridges", Float(7.0)),
                ],
            ),
            filter(
                "ColorHalftone",
                &[
                    ("radius", Float(6.0)),
                    ("angle_r", Float(10.0)),
                    ("angle_g", Float(40.0)),
                    ("angle_b", Float(70.0)),
                ],
            ),
            filter("Crystallize", &[("cell", Int(20)), ("seed", Int(7))]),
            filter("Facet", &[]),
            filter("Fragment", &[]),
            filter("Mezzotint", &[("kind", Choice(6)), ("seed", Int(3))]),
            filter(
                "Pointillize",
                &[
                    ("cell", Int(12)),
                    ("seed", Int(5)),
                    ("background", Color([1.0, 0.0, 1.0, 1.0])),
                ],
            ),
            filter(
                "ShapeMosaic",
                &[
                    ("cell_size", Int(20)),
                    ("shape", Choice(2)),
                    ("spread", Choice(1)),
                    ("monochromatic", Bool(true)),
                    ("invert", Bool(true)),
                ],
            ),
            filter("Clouds", &[]),
            filter("DifferenceClouds", &[]),
            filter(
                "Fibers",
                &[
                    ("variance", Float(0.5)),
                    ("strength", Float(0.25)),
                    ("seed", Int(9)),
                    ("from", Color([0.0, 0.0, 0.0, 1.0])),
                    ("to", Color([1.0, 1.0, 1.0, 1.0])),
                ],
            ),
            filter(
                "LensFlare",
                &[
                    ("brightness", Float(1.5)),
                    ("radius", Float(120.0)),
                    ("ghosts", Int(5)),
                    ("streaks", Int(6)),
                ],
            ),
            filter(
                "Diffuse",
                &[
                    ("radius", Int(4)),
                    ("mode", Choice(1)),
                    ("seed", Int(11)),
                    edge,
                ],
            ),
            filter(
                "Emboss",
                &[
                    ("angle", Float(45.0)),
                    ("height", Float(5.0)),
                    ("amount", Float(2.5)),
                ],
            ),
            filter(
                "Extrude",
                &[
                    ("kind", Choice(1)),
                    ("size", Int(40)),
                    ("depth", Float(20.0)),
                    ("depth_mode", Choice(1)),
                    ("seed", Int(2)),
                ],
            ),
            filter("FindEdges", &[]),
            filter("Solarize", &[]),
            filter(
                "TraceContour",
                &[("level", Int(100)), ("side", Choice(1)), edge],
            ),
            filter(
                "Wind",
                &[
                    ("direction", Choice(1)),
                    ("strength", Float(0.75)),
                    ("seed", Int(1)),
                    edge,
                ],
            ),
            filter("Custom", &taps(5.0)),
            filter(
                "Offset",
                &[("dx", Int(-12)), ("dy", Int(30)), ("edge", Choice(1))],
            ),
            filter("HsbHsl", &[("input", Choice(2)), ("output", Choice(0))]),
            filter("Kaleidoscope", &[("mirrors", Int(8)), ("angle", Int(30))]),
            filter(
                "Dents",
                &[
                    ("scale", Float(40.5)),
                    ("refraction", Float(60.0)),
                    ("turbulence", Float(20.0)),
                ],
            ),
            filter(
                "Repeat",
                &[
                    ("scale", Float(150.5)),
                    ("row_shift", Float(-10.0)),
                    ("space_x", Float(5.0)),
                    ("space_y", Float(20.0)),
                    ("auto_color", Bool(true)),
                    ("angle", Float(30.0)),
                ],
            ),
            filter(
                "ColorToAlpha",
                &[
                    ("color", Color([1.0, 0.0, 0.0, 1.0])),
                    ("transparency", Float(12.5)),
                    ("opacity", Float(90.0)),
                ],
            ),
            filter("Dither", &[("palette", Choice(2)), ("method", Choice(2))]),
            filter(
                "Particles",
                &[
                    ("count", Float(20.0)),
                    ("size", Int(10)),
                    ("depth", Float(50.0)),
                    ("brightness", Float(400.0)),
                    ("color", Color([1.0, 1.0, 1.0, 1.0])),
                    ("time", Float(0.5)),
                    ("turbulence", Float(25.0)),
                    ("blink", Bool(false)),
                    ("fall", Bool(true)),
                ],
            ),
            filter("FourierTransform", &[]),
            filter("InverseFourierTransform", &[]),
            filter(
                "NormalMap",
                &[
                    ("blur", Float(2.5)),
                    ("scale", Float(150.0)),
                    ("invert", Bool(true)),
                    ("high", Float(50.0)),
                    ("medium", Float(75.0)),
                    ("low", Float(25.0)),
                ],
            ),
            filter("TextureDilation", &[("crop", Int(3)), ("radius", Int(20))]),
        ]
    }

    fn through_bytes(fx: &Descriptor) -> Descriptor {
        let mut sink = Sink::new();
        fx.write(&mut sink).unwrap();
        let bytes = sink.into_inner();
        Descriptor::read(&mut Cursor::new(&bytes), &ReadOptions::default()).unwrap()
    }

    fn extras_of(entry: &Descriptor) -> Vec<String> {
        entry
            .descriptor("Fltr")
            .map(|s| {
                s.items
                    .iter()
                    .filter(|(k, _)| k.starts_with(EXTRA_PREFIX))
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// W18-J: each newly mapped filter is carried by Photoshop's own keys
    /// alone — no extra key is needed for settings on Photoshop's grid — and
    /// reads back with equal parameters.
    #[test]
    fn every_newly_mapped_filter_round_trips_in_photoshops_keys_alone() {
        let stack = w18_mapped();
        assert_eq!(stack.len(), MAPPED.len() - 19, "one fixture per new filter");
        for f in &stack {
            let entry = encode_filter(f).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(extras_of(&entry), Vec::<String>::new(), "{}", f.filter);
            let fx = encode_stack(std::slice::from_ref(f)).unwrap();
            let decoded = decode_stack(&through_bytes(&fx));
            assert!(decoded.unmapped.is_empty(), "{:?}", decoded.unmapped);
            assert_eq!(decoded.filters, vec![f.clone()], "{}", f.filter);
        }
    }

    /// W18-J: settings Photoshop has no key for, or holds only on a coarser
    /// grid, ride as extra keys and read back exactly; the Photoshop keys
    /// still carry the nearest setting.
    #[test]
    fn settings_photoshop_lacks_ride_as_extras_and_round_trip_exactly() {
        use SmartParam::{Choice, Color, Float, Int};
        let stack = vec![
            filter(
                "Twirl",
                &[("radius", Float(300.0)), ("angle", Float(12.25))],
            ),
            filter(
                "Wave",
                &[
                    ("kind", Choice(0)),
                    ("amplitude", Float(8.5)),
                    ("wavelength", Float(40.0)),
                    ("phase", Float(30.0)),
                    ("edge", Choice(2)),
                ],
            ),
            filter(
                "GaussianBlur",
                &[("radius", Float(3.0)), ("edge", Choice(1))],
            ),
            filter("Median", &[("radius", Int(3)), ("edge", Choice(2))]),
            filter(
                "MotionBlur",
                &[("angle", Float(12.5)), ("distance", Float(10.0))],
            ),
            filter(
                "Pointillize",
                &[
                    ("cell", Int(12)),
                    ("seed", Int(5)),
                    ("background", Color([0.25, 0.5, 0.75, 0.5])),
                ],
            ),
            filter(
                "Offset",
                &[("dx", Int(4)), ("dy", Int(-4)), ("edge", Choice(2))],
            ),
        ];
        let fx = encode_stack(&stack).expect("every one encodes");
        let back = through_bytes(&fx);
        let decoded = decode_stack(&back);
        assert_eq!(decoded.filters, stack);
        let Some(Value::List(list)) = back.get("filterFXList") else {
            panic!("no list");
        };
        let entry = |i: usize| match &list[i] {
            Value::Descriptor(d) => d.clone(),
            _ => panic!("not a descriptor"),
        };
        assert!(extras_of(&entry(0)).contains(&"rasterStudio_radius".to_string()));
        assert_eq!(
            entry(0).descriptor("Fltr").unwrap().get("Angl"),
            Some(&Value::Integer(12))
        );
        let wave = entry(1);
        let s = wave.descriptor("Fltr").unwrap();
        // Mirror has no Photoshop spelling: repeat edge pixels, plus the extra.
        assert_eq!(s.get("UndA"), Some(&enumerated("UndA", "RptE")));
        assert!(extras_of(&wave).contains(&"rasterStudio_edge".to_string()));
        // Wrap is Photoshop's WrpA on the distortions that have it.
        let wrapped = encode_filter(&filter("Wave", &[("edge", Choice(1))])).unwrap();
        assert_eq!(
            wrapped.descriptor("Fltr").unwrap().get("UndA"),
            Some(&enumerated("UndA", "WrpA"))
        );
        assert!(extras_of(&entry(2)).contains(&"rasterStudio_edge".to_string()));
        assert!(extras_of(&entry(4)).contains(&"rasterStudio_angle".to_string()));
        // The background colour is the entry's BckC, the alpha an extra.
        assert!(extras_of(&entry(5)).contains(&"rasterStudio_background".to_string()));
        assert!(entry(5).descriptor("BckC").is_some());
    }

    /// W18-J: an entry as Photopea writes it (its `iL.Lr` defaults) decodes.
    #[test]
    fn a_photopea_written_entry_decodes() {
        let mut twirl = Descriptor::new("Twrl");
        twirl.push("Angl", Value::Integer(90)).unwrap();
        let mut item = Descriptor::new("filterFX");
        item.push("Nm  ", Value::Text("Twirl".into())).unwrap();
        item.push("Fltr", Value::Descriptor(twirl)).unwrap();
        item.push("filterID", Value::Integer(filter_id("Twrl")))
            .unwrap();
        let f = decode_filter(&item).unwrap();
        assert_eq!(f.filter, "Twirl");
        assert_eq!(f.params.get("angle"), Some(&SmartParam::Float(90.0)));
        let mut wind = Descriptor::new("Wnd ");
        wind.push("WndM", enumerated("WndM", "Stgr")).unwrap();
        wind.push("Drct", enumerated("Drct", "Rght")).unwrap();
        let mut item = Descriptor::new("filterFX");
        item.push("Fltr", Value::Descriptor(wind)).unwrap();
        item.push("filterID", Value::Integer(filter_id("Wnd ")))
            .unwrap();
        let f = decode_filter(&item).unwrap();
        assert_eq!(f.filter, "Wind");
        assert_eq!(f.params.get("direction"), Some(&SmartParam::Choice(1)));
        assert_eq!(f.params.get("strength"), Some(&SmartParam::Float(1.0)));
    }

    #[test]
    fn a_filter_photoshop_lacks_is_refused_with_its_reason_and_edges_are_written() {
        let refused = encode_stack(&[
            filter("GaussianBlur", &[]),
            filter("LensBlur", &[]),
            filter("GaussianBlur", &[("edge", SmartParam::Choice(1))]),
            filter("Displace", &[]),
        ])
        .unwrap_err();
        assert_eq!(refused.len(), 2, "{refused:?}");
        assert!(refused[0].contains("Lens Blur"), "{refused:?}");
        assert!(refused[1].contains("Displace") && refused[1].contains(".psd file"));
        assert!(!has_photoshop_equivalent("LensBlur"));
        assert!(has_photoshop_equivalent("GaussianBlur"));
        for (key, _) in RASTERISED {
            assert!(!has_photoshop_equivalent(key), "{key}");
            assert!(rasterised_reason(key).is_some());
        }
    }

    /// W18-J: the smart filters' mask settings ride in `filterFXStyle`.
    #[test]
    fn the_filter_mask_settings_round_trip() {
        let flags = FilterMaskFlags {
            enabled: false,
            linked: false,
            density: 128,
            feather: 2.5,
            inverted: true,
        };
        let fx = encode_stack_masked(&[filter("Blur", &[])], Some(flags)).unwrap();
        let back = through_bytes(&fx);
        assert_eq!(decode_mask_flags(&back), flags);
        assert_eq!(back.get("filterMaskDensity"), Some(&Value::Integer(128)));
        // No mask: Photoshop's off switch, the defaults otherwise.
        let bare = encode_stack(&[filter("Blur", &[])]).unwrap();
        assert_eq!(bare.get("filterMaskEnable"), Some(&Value::Bool(false)));
        assert_eq!(bare.get("filterMaskDensity"), None);
    }

    fn mask_entry(depth: u16) -> FilterEffects {
        let bytes = usize::from(depth / 8);
        let rect = Rect::new(2, 3, 6, 5);
        let n = 4 * 2 * bytes;
        let plane = |seed: u8| (0..n).map(|i| seed.wrapping_add(i as u8 * 7)).collect();
        FilterEffects {
            id: "so-1".into(),
            rect,
            depth,
            planes: [
                Some(plane(1)),
                Some(plane(2)),
                Some(plane(3)),
                Some(plane(4)),
            ],
            mask: Some(FilterEffectsMask {
                rect: Rect::new(1, 1, 4, 3),
                data: (0..3 * 2 * bytes).map(|i| (i * 40) as u8).collect(),
            }),
        }
    }

    /// W18-J: a smart-filter mask's pixels round-trip through the `FEid`
    /// block, alone and through a whole file.
    #[test]
    fn a_smart_filter_mask_round_trips_through_feid() {
        let entries = vec![
            mask_entry(8),
            FilterEffects {
                id: "odd-length".into(),
                mask: None,
                ..mask_entry(8)
            },
        ];
        assert_eq!(
            decode_filter_effects(&encode_filter_effects(&entries)),
            entries
        );
        let deep = vec![mask_entry(16)];
        assert_eq!(decode_filter_effects(&encode_filter_effects(&deep)), deep);
        // Through a whole file, as a document-level block.
        let mut file = PsdFile::new(PsdHeader::rgba8(8, 8));
        let mut layer = PsdLayer::raster("L", Rect::sized(1, 1));
        layer.set_rgba8(&[7u8; 4]).unwrap();
        file.layers.push(layer);
        file.extra.push(TaggedBlock::new(
            FILTER_EFFECTS_KEY,
            encode_filter_effects(&entries),
        ));
        let back = crate::read(&crate::write(&file).unwrap()).unwrap();
        assert_eq!(filter_effects_of(&back.extra), entries);
    }

    /// A PackBits plane (what Photoshop writes) reads as well as a raw one.
    #[test]
    fn a_packbits_plane_is_read() {
        let rows = [[9u8, 9, 9, 9, 1], [0, 0, 0, 0, 0]];
        let mut packed = Vec::new();
        let mut table = Vec::new();
        for row in rows {
            let p = crate::packbits::encode(&row);
            table.extend((p.len() as u16).to_be_bytes());
            packed.extend(p);
        }
        let mut bytes = vec![0, 1];
        bytes.extend(table);
        bytes.extend(packed);
        assert_eq!(read_plane(&bytes, 2, 5).unwrap(), rows.concat().to_vec());
    }

    #[test]
    fn every_mapped_filter_round_trips_through_the_descriptor_bytes() {
        let stack = every_mapped();
        assert_eq!(stack.len(), MAPPED.len());
        let fx = encode_stack(&stack).expect("every mapped filter encodes");
        let mut sink = Sink::new();
        fx.write(&mut sink).unwrap();
        let bytes = sink.into_inner();
        let back = Descriptor::read(&mut Cursor::new(&bytes), &ReadOptions::default()).unwrap();
        let decoded = decode_stack(&back);
        assert!(decoded.unmapped.is_empty(), "{:?}", decoded.unmapped);
        for (a, b) in stack.iter().zip(&decoded.filters) {
            let mut b = b.clone();
            // `num` reads a quantised float back through f64: compare on the
            // grid it was written on.
            for (k, v) in b.params.iter_mut() {
                if let (SmartParam::Float(x), Some(SmartParam::Float(y))) = (v, a.params.get(k)) {
                    assert!((*x - *y).abs() < 1e-6, "{} {k}: {x} vs {y}", a.filter);
                    *x = *y;
                }
            }
            assert_eq!(a, &b);
        }
        assert_eq!(decoded.filters.len(), stack.len());
    }

    /// The entry has Photoshop's shape, the one Photopea writes and reads:
    /// `filterID` is the event code, `Fltr` its class, `Rds ` in pixels.
    #[test]
    fn a_gaussian_blur_entry_is_photoshops_gsnb() {
        let d = encode_filter(&filter(
            "GaussianBlur",
            &[("radius", SmartParam::Float(4.0))],
        ))
        .unwrap();
        assert_eq!(d.class_id, "filterFX");
        assert_eq!(d.get("filterID"), Some(&Value::Integer(0x4773_6E42)));
        let s = d.descriptor("Fltr").unwrap();
        assert_eq!(s.class_id, "GsnB");
        assert_eq!(s.get("Rds "), Some(&px(4.0)));
        let blend = d.descriptor("blendOptions").unwrap();
        assert_eq!(blend.get("Opct"), Some(&pct(100.0)));
        assert_eq!(blend.get("Md  "), Some(&enumerated("BlnM", "Nrml")));
        // A code longer than four characters is named by the Fltr class.
        let boxed = encode_filter(&filter("BoxBlur", &[])).unwrap();
        assert_eq!(boxed.get("filterID"), Some(&Value::Integer(777)));
        assert_eq!(code_of(&boxed).as_deref(), Some("boxblur"));
    }

    #[test]
    fn every_blend_mode_has_a_code_that_reads_back() {
        for mode in BlendMode::ALL {
            assert_eq!(blend_from_blnm(blend_code(mode)), Some(mode), "{mode:?}");
        }
    }

    #[test]
    fn an_unknown_entry_is_named_and_a_master_switch_off_turns_all_off() {
        let mut fx = encode_stack(&[filter("GaussianBlur", &[])]).unwrap();
        let mut odd = Descriptor::new("filterFX");
        odd.push("Nm  ", Value::Text("Oil Paint".into())).unwrap();
        odd.push("Fltr", Value::Descriptor(Descriptor::new("oilPaint")))
            .unwrap();
        odd.push("filterID", Value::Integer(777)).unwrap();
        for (k, v) in fx.items.iter_mut() {
            match (k.as_str(), v) {
                ("filterFXList", Value::List(list)) => list.push(Value::Descriptor(odd.clone())),
                ("enab", v) => *v = Value::Bool(false),
                _ => {}
            }
        }
        let decoded = decode_stack(&fx);
        assert_eq!(decoded.unmapped, ["Oil Paint"]);
        assert_eq!(decoded.filters.len(), 1);
        assert!(!decoded.filters[0].enabled);
    }

    #[test]
    fn the_stack_rides_in_sold_through_a_whole_file() {
        let placed = PlacedLayer {
            id: "so".into(),
            corners: [0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0],
            size: Some((4.0, 4.0)),
        };
        let stack = vec![filter(
            "GaussianBlur",
            &[
                ("radius", SmartParam::Float(2.0)),
                ("edge", SmartParam::Choice(0)),
            ],
        )];
        let mut layer = PsdLayer::raster("SO", Rect::sized(4, 4));
        layer.set_rgba8(&[7u8; 64]).unwrap();
        layer
            .extra
            .push(placed.to_block_with(Some(encode_stack(&stack).unwrap())));
        let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
        file.layers.push(layer);
        let back = crate::read(&crate::write(&file).unwrap()).unwrap();
        let opts = ReadOptions::default();
        assert_eq!(PlacedLayer::of(&back.layers[0], &opts), Some(placed));
        let fx = filter_fx_of(&back.layers[0], &opts).expect("filterFX is in SoLd");
        assert_eq!(decode_stack(&fx).filters, stack);
        // A layer without filters has none.
        let mut bare = PsdLayer::raster("bare", Rect::sized(1, 1));
        bare.extra.push(
            PlacedLayer {
                id: "b".into(),
                corners: [0.0; 8],
                size: None,
            }
            .to_block(),
        );
        assert_eq!(filter_fx_of(&bare, &opts), None);
    }
}
