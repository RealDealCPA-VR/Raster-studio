//! W16-H: the Photoshop events a recorded action commonly holds beyond the
//! first eighteen — the tonal and colour adjustments with their parameters,
//! crop / trim / mode, free transform, the layer commands, the selection
//! modifiers, stroke, the clipboard, the common filters and Save As /
//! Export — read into [`StepOp`] and written back as the descriptor
//! Photoshop records for each.
//!
//! A child module of `atn` (declared there with `#[path]`): [`interpret`]
//! runs before the W13-E table and answers `None` for an event that is not
//! its business, and [`to_step`] writes every operation this module adds.
//!
//! Units are Photoshop's own, as the dialog shows them: levels and curve
//! points `0..=255`, percentages as percent, angles in degrees clockwise.

use psd::{Descriptor, RefItem, Value};

use layer_model::BlendMode;

use super::{desc, enum_value, enumerated, reference, target_class, unit, AtnStep, StepOp};

/// Which channel a Levels or Curves entry adjusts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneChannel {
    Composite,
    Red,
    Green,
    Blue,
}

/// One channel of a Levels step, in Photoshop's `0..=255` levels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelsEntry {
    pub channel: ToneChannel,
    /// Input black and white.
    pub input: [f64; 2],
    pub gamma: f64,
    /// Output black and white.
    pub output: [f64; 2],
}

/// One channel of a Curves step: `[input, output]` points in `0..=255`.
#[derive(Debug, Clone, PartialEq)]
pub struct CurvesEntry {
    pub channel: ToneChannel,
    pub points: Vec<[f64; 2]>,
}

/// Image ▸ Mode targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeTarget {
    Rgb,
    Grayscale,
    Lab,
    Cmyk,
    Indexed,
    Bitmap,
    Duotone,
}

/// What a Free Transform or Move step moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformTarget {
    Layer,
    Selection,
}

/// The transform's reference point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pivot {
    /// A point of the target's bounds, as fractions of its width and height
    /// (`[0.5, 0.5]` is the centre, Photoshop's default).
    Bounds([f64; 2]),
    /// A document position in pixels.
    Point([f64; 2]),
}

/// A layer named by a Select or Arrange step.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerRef {
    Name(String),
    /// Photoshop's 1-based index, counted from the bottom of the stack.
    Index(u32),
    /// The next layer up (select) / one step up (arrange).
    Forward,
    Backward,
    Front,
    Back,
}

/// Where Edit ▸ Stroke lays its band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrokeAt {
    Inside,
    Center,
    Outside,
}

/// What Image ▸ Trim trims away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimBasis {
    Transparent,
    TopLeft,
    BottomRight,
}

/// `(four-character code, string id)` of every event this module reads.
/// A string-only event has an empty code.
const MORE_EVENTS: &[(&str, &str)] = &[
    ("Lvls", "levels"),
    ("Crvs", "curves"),
    ("HStr", "hueSaturation"),
    ("ClrB", "colorBalance"),
    ("BanW", "blackAndWhite"),
    ("", "vibrance"),
    ("", "exposure"),
    ("Thrs", "thresholdClassEvent"),
    ("Pstr", "posterization"),
    ("GrMp", "gradientMapEvent"),
    ("", "photoFilter"),
    ("ChnM", "channelMixer"),
    ("Crop", "crop"),
    ("", "trim"),
    ("CnvM", "convertMode"),
    ("Trnf", "transform"),
    ("move", "move"),
    ("Dplc", "duplicate"),
    ("Dlt ", "delete"),
    ("Mrg2", "mergeLayersNew"),
    ("MrgV", "mergeVisible"),
    ("FltI", "flattenImage"),
    ("Mk  ", "make"),
    ("setd", "set"),
    ("Shw ", "show"),
    ("Hd  ", "hide"),
    ("slct", "select"),
    ("ClrR", "colorRange"),
    ("Fthr", "feather"),
    ("Expn", "expand"),
    ("Cntc", "contract"),
    ("Brdr", "border"),
    ("Smth", "smoothness"),
    ("Strk", "stroke"),
    ("copy", "copyEvent"),
    ("CpyM", "copyMerged"),
    ("past", "paste"),
    ("cut ", "cut"),
    ("CpTL", "copyToLayer"),
    ("CtTL", "cutToLayer"),
    ("AdNs", "addNoise"),
    ("MtnB", "motionBlur"),
    ("HghP", "highPass"),
    ("", "smartSharpen"),
    ("Expr", "export"),
    ("save", "save"),
];

/// The step's event as its string id, when [`MORE_EVENTS`] knows it.
fn event_id(step: &AtnStep) -> Option<&'static str> {
    MORE_EVENTS
        .iter()
        .find(|(code, id)| {
            if step.char_id {
                !code.is_empty() && *code == step.event
            } else {
                *id == step.event
            }
        })
        .map(|(_, id)| *id)
}

// ------------------------------------------------------------ colour

/// `[r, g, b]` in `0..=255` from Lab (D50), Photoshop's Lab.
pub fn lab_to_rgb(lab: [f64; 3]) -> [f64; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let inv = |t: f64| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            3.0 * (6.0f64 / 29.0).powi(2) * (t - 4.0 / 29.0)
        }
    };
    let (x, y, z) = (0.96422 * inv(fx), inv(fy), 0.82521 * inv(fz));
    let lin = [
        3.1338561 * x - 1.6168667 * y - 0.4906146 * z,
        -0.9787684 * x + 1.9161415 * y + 0.0334540 * z,
        0.0719453 * x - 0.2289914 * y + 1.4052427 * z,
    ];
    lin.map(|c| {
        let c = c.clamp(0.0, 1.0);
        let e = if c <= 0.0031308 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        e * 255.0
    })
}

/// Lab (D50) from `[r, g, b]` in `0..=255`: the inverse of [`lab_to_rgb`].
pub fn rgb_to_lab(rgb: [f64; 3]) -> [f64; 3] {
    let lin = rgb.map(|v| {
        let c = (v / 255.0).clamp(0.0, 1.0);
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let x = 0.4360747 * lin[0] + 0.3850649 * lin[1] + 0.1430804 * lin[2];
    let y = 0.2225045 * lin[0] + 0.7168786 * lin[1] + 0.0606169 * lin[2];
    let z = 0.0139322 * lin[0] + 0.0971045 * lin[1] + 0.7141733 * lin[2];
    let f = |t: f64| {
        if t > (6.0f64 / 29.0).powi(3) {
            t.cbrt()
        } else {
            t / (3.0 * (6.0f64 / 29.0).powi(2)) + 4.0 / 29.0
        }
    };
    let (fx, fy, fz) = (f(x / 0.96422), f(y), f(z / 0.82521));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// A colour descriptor (`RGBC`, `LbCl` / `LbCC`, `HSBC`, `Grsc`) as
/// `[r, g, b]` in `0..=255`.
fn color_of(c: &Descriptor) -> Option<[f64; 3]> {
    let n = |k: &str| c.number(k).unwrap_or(0.0);
    match c.class_id.as_str() {
        "RGBC" => Some([
            n("Rd  "),
            n("Grn "),
            c.number("Bl  ")
                .or_else(|| c.number("blueFloat"))
                .unwrap_or(0.0),
        ]),
        "LbCl" | "LbCC" => Some(lab_to_rgb([n("Lmnc"), n("A   "), n("B   ")])),
        "Grsc" => {
            let v = (100.0 - n("Gry ")) / 100.0 * 255.0;
            Some([v; 3])
        }
        "HSBC" => {
            let (h, s, v) = (n("H   ") / 60.0, n("Strt") / 100.0, n("Brgh") / 100.0);
            let i = h.floor().rem_euclid(6.0);
            let f = h - h.floor();
            let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
            let rgb = match i as u8 {
                0 => [v, t, p],
                1 => [q, v, p],
                2 => [p, v, t],
                3 => [p, q, v],
                4 => [t, p, v],
                _ => [v, p, q],
            };
            Some(rgb.map(|x| x * 255.0))
        }
        _ => None,
    }
}

fn rgbc(rgb: [f64; 3]) -> Value {
    Value::Descriptor(desc(
        "RGBC",
        vec![
            ("Rd  ", Value::Double(rgb[0])),
            ("Grn ", Value::Double(rgb[1])),
            ("Bl  ", Value::Double(rgb[2])),
        ],
    ))
}

fn lab_value(class: &str, rgb: [f64; 3]) -> Value {
    let lab = rgb_to_lab(rgb);
    Value::Descriptor(desc(
        class,
        vec![
            ("Lmnc", Value::Double(lab[0])),
            ("A   ", Value::Double(lab[1])),
            ("B   ", Value::Double(lab[2])),
        ],
    ))
}

// ------------------------------------------------------------ blend modes

/// The `BlnM` enumeration of a blend mode, both spellings Photoshop uses.
pub fn blend_from_blnm(value: &str) -> Option<BlendMode> {
    BLNM.iter()
        .find(|(code, id, _)| *code == value || *id == value)
        .map(|(_, _, m)| *m)
}

/// The `BlnM` code Photoshop writes for `mode`.
pub fn blnm(mode: BlendMode) -> &'static str {
    BLNM.iter()
        .find(|(_, _, m)| *m == mode)
        .map_or("Nrml", |(code, _, _)| *code)
}

const BLNM: &[(&str, &str, BlendMode)] = &[
    ("Nrml", "normal", BlendMode::Normal),
    ("Dslv", "dissolve", BlendMode::Dissolve),
    ("Drkn", "darken", BlendMode::Darken),
    ("Mltp", "multiply", BlendMode::Multiply),
    ("CBrn", "colorBurn", BlendMode::ColorBurn),
    ("linearBurn", "linearBurn", BlendMode::LinearBurn),
    ("darkerColor", "darkerColor", BlendMode::DarkerColor),
    ("Lghn", "lighten", BlendMode::Lighten),
    ("Scrn", "screen", BlendMode::Screen),
    ("CDdg", "colorDodge", BlendMode::ColorDodge),
    ("linearDodge", "linearDodge", BlendMode::LinearDodge),
    ("lighterColor", "lighterColor", BlendMode::LighterColor),
    ("Ovrl", "overlay", BlendMode::Overlay),
    ("SftL", "softLight", BlendMode::SoftLight),
    ("HrdL", "hardLight", BlendMode::HardLight),
    ("vividLight", "vividLight", BlendMode::VividLight),
    ("linearLight", "linearLight", BlendMode::LinearLight),
    ("pinLight", "pinLight", BlendMode::PinLight),
    ("hardMix", "hardMix", BlendMode::HardMix),
    ("Dfrn", "difference", BlendMode::Difference),
    ("Xclu", "exclusion", BlendMode::Exclusion),
    ("blendSubtraction", "subtract", BlendMode::Subtract),
    ("blendDivide", "divide", BlendMode::Divide),
    ("H   ", "hue", BlendMode::Hue),
    ("Strt", "saturation", BlendMode::Saturation),
    ("Clr ", "color", BlendMode::Color),
    ("Lmns", "luminosity", BlendMode::Luminosity),
];

// ------------------------------------------------------------ reading

fn numbers(v: &Value) -> Option<Vec<f64>> {
    match v {
        Value::List(items) => items
            .iter()
            .map(|i| match i {
                Value::Double(x) | Value::UnitFloat { value: x, .. } => Some(*x),
                Value::Integer(x) => Some(f64::from(*x)),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn pair(d: &Descriptor, key: &str, default: [f64; 2]) -> [f64; 2] {
    match d.get(key).and_then(numbers) {
        Some(v) if v.len() == 2 => [v[0], v[1]],
        _ => default,
    }
}

fn flag(d: &Descriptor, key: &str) -> Option<bool> {
    match d.get(key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn tone_channel(d: &Descriptor) -> Result<ToneChannel, String> {
    let Some(Value::Reference(items)) = d.get("Chnl") else {
        return Ok(ToneChannel::Composite);
    };
    match items.first() {
        Some(RefItem::Enumerated { value, .. }) => match value.as_str() {
            "Cmps" => Ok(ToneChannel::Composite),
            "Rd  " => Ok(ToneChannel::Red),
            "Grn " => Ok(ToneChannel::Green),
            "Bl  " => Ok(ToneChannel::Blue),
            other => Err(format!("the {other} channel has no equivalent here")),
        },
        _ => Err("a channel other than RGB or one of its three".to_string()),
    }
}

fn adjustment_list<'a>(d: &'a Descriptor, class: &str) -> Result<Vec<&'a Descriptor>, String> {
    match d.get("Adjs") {
        Some(Value::List(items)) => items
            .iter()
            .map(|v| match v {
                Value::Descriptor(x) if x.class_id == class => Ok(x),
                _ => Err(format!("an entry that is not {class}")),
            })
            .collect(),
        _ => Err("it uses a preset file rather than its own settings".to_string()),
    }
}

fn layer_target(d: &Descriptor) -> bool {
    matches!(target_class(d), None | Some("Lyr "))
}

fn layer_ref(items: &[RefItem]) -> Option<LayerRef> {
    match items.first()? {
        RefItem::Name {
            class_id, value, ..
        } if class_id == "Lyr " => Some(LayerRef::Name(value.clone())),
        RefItem::Index(i) => Some(LayerRef::Index(*i)),
        RefItem::Offset {
            class_id, value, ..
        } if class_id == "Lyr " => Some(LayerRef::Index(*value)),
        RefItem::Enumerated {
            class_id, value, ..
        } if class_id == "Lyr " => match value.as_str() {
            "Frwr" | "Nxt " => Some(LayerRef::Forward),
            "Bckw" | "Prvs" => Some(LayerRef::Backward),
            "Frnt" => Some(LayerRef::Front),
            "Back" => Some(LayerRef::Back),
            _ => None,
        },
        _ => None,
    }
}

const QCS: &[(&str, [f64; 2])] = &[
    ("Qcsa", [0.5, 0.5]),
    ("Qcs0", [0.0, 0.0]),
    ("Qcs1", [1.0, 0.0]),
    ("Qcs2", [1.0, 1.0]),
    ("Qcs3", [0.0, 1.0]),
    ("Qcs4", [0.5, 0.0]),
    ("Qcs5", [1.0, 0.5]),
    ("Qcs6", [0.5, 1.0]),
    ("Qcs7", [0.0, 0.5]),
];

fn offset_of(d: &Descriptor, key: &str) -> [f64; 2] {
    d.descriptor(key).map_or([0.0; 2], |o| {
        [
            o.number("Hrzn").unwrap_or(0.0),
            o.number("Vrtc").unwrap_or(0.0),
        ]
    })
}

fn transform_target(d: &Descriptor) -> Result<TransformTarget, String> {
    match d.get("null") {
        None => Ok(TransformTarget::Layer),
        Some(Value::Reference(items)) => match items.first() {
            Some(RefItem::Property {
                class_id, key_id, ..
            }) if class_id == "Chnl" && key_id == "fsel" => Ok(TransformTarget::Selection),
            Some(
                RefItem::Enumerated { class_id, .. }
                | RefItem::Class { class_id, .. }
                | RefItem::Name { class_id, .. },
            ) if class_id == "Lyr " => Ok(TransformTarget::Layer),
            _ => Err("transforms something other than a layer or the selection".to_string()),
        },
        Some(_) => Err("has a target this application cannot read".to_string()),
    }
}

/// What `step` does, when it is one of this module's events; `None` hands
/// it to the W13-E interpreter.
pub(super) fn interpret(step: &AtnStep) -> Option<Result<StepOp, String>> {
    let id = event_id(step)?;
    let empty = Descriptor::default();
    let d = step.descriptor.as_ref().unwrap_or(&empty);
    let name = &step.name;
    let num = |k: &str, default: f64| d.number(k).unwrap_or(default);
    let why = |reason: String| Some(Err(format!("“{name}” {reason}")));
    let op = match id {
        "levels" => {
            if flag(d, "Auto") == Some(true) {
                return why("is Auto Levels".to_string());
            }
            let list = match adjustment_list(d, "LvlA") {
                Ok(l) => l,
                Err(e) => return why(e),
            };
            let mut entries = Vec::new();
            for e in list {
                let channel = match tone_channel(e) {
                    Ok(c) => c,
                    Err(r) => return why(r),
                };
                entries.push(LevelsEntry {
                    channel,
                    input: pair(e, "Inpt", [0.0, 255.0]),
                    gamma: e.number("Gmm ").unwrap_or(1.0),
                    output: pair(e, "Otpt", [0.0, 255.0]),
                });
            }
            StepOp::Levels(entries)
        }
        "curves" => {
            let list = match adjustment_list(d, "CrvA") {
                Ok(l) => l,
                Err(e) => return why(e),
            };
            let mut entries = Vec::new();
            for e in list {
                let channel = match tone_channel(e) {
                    Ok(c) => c,
                    Err(r) => return why(r),
                };
                let Some(Value::List(points)) = e.get("Crv ") else {
                    return why("draws a curve by hand (a mapping), not by points".to_string());
                };
                let points = points
                    .iter()
                    .filter_map(|p| match p {
                        Value::Descriptor(p) => Some([
                            p.number("Hrzn").unwrap_or(0.0),
                            p.number("Vrtc").unwrap_or(0.0),
                        ]),
                        _ => None,
                    })
                    .collect();
                entries.push(CurvesEntry { channel, points });
            }
            StepOp::Curves(entries)
        }
        "hueSaturation" => {
            let list = match adjustment_list(d, "Hst2") {
                Ok(l) => l,
                Err(e) => return why(e),
            };
            if list.iter().any(|e| e.get("LclR").is_some()) {
                return why(
                    "edits a single colour range, which Hue/Saturation here does not have"
                        .to_string(),
                );
            }
            let master = list.first().copied().unwrap_or(&empty);
            StepOp::HueSaturation {
                hue: master.number("H   ").unwrap_or(0.0),
                saturation: master.number("Strt").unwrap_or(0.0),
                lightness: master.number("Lght").unwrap_or(0.0),
                colorize: flag(d, "Clrz").unwrap_or(false),
            }
        }
        "colorBalance" => {
            let three = |k: &str| match d.get(k).and_then(numbers) {
                Some(v) if v.len() == 3 => [v[0], v[1], v[2]],
                _ => [0.0; 3],
            };
            StepOp::ColorBalance {
                shadows: three("ShdL"),
                midtones: three("MdtL"),
                highlights: three("HghL"),
                preserve_luminosity: flag(d, "PrsL").unwrap_or(true),
            }
        }
        "blackAndWhite" => StepOp::BlackAndWhite {
            weights: [
                num("Rd  ", 40.0),
                num("Yllw", 60.0),
                num("Grn ", 40.0),
                num("Cyn ", 60.0),
                num("Bl  ", 20.0),
                num("Mgnt", 80.0),
            ],
            tint: if flag(d, "useTint") == Some(true) {
                d.descriptor("tintColor").and_then(color_of)
            } else {
                None
            },
        },
        "vibrance" => StepOp::Vibrance {
            vibrance: num("vibrance", 0.0),
            saturation: num("Strt", 0.0),
        },
        "exposure" => StepOp::Exposure {
            exposure: num("Exps", 0.0),
            offset: num("Ofst", 0.0),
            gamma: num("gammaCorrection", 1.0),
        },
        "thresholdClassEvent" => StepOp::Threshold {
            level: num("Lvl ", 128.0),
        },
        "posterization" => StepOp::Posterize {
            levels: num("Lvls", 4.0).round().clamp(2.0, 256.0) as u32,
        },
        "gradientMapEvent" => {
            let Some(grad) = d.descriptor("Grad") else {
                return why("has no gradient".to_string());
            };
            let Some(Value::List(stops)) = grad.get("Clrs") else {
                return why("uses a noise gradient, which has no equivalent here".to_string());
            };
            let mut out = Vec::new();
            for s in stops {
                let Value::Descriptor(s) = s else { continue };
                let Some(rgb) = s.descriptor("Clr ").and_then(color_of) else {
                    return why(
                        "has a stop in the foreground or background colour, not a colour of its own"
                            .to_string(),
                    );
                };
                out.push((s.number("Lctn").unwrap_or(0.0) / 4096.0, rgb));
            }
            StepOp::GradientMap {
                stops: out,
                reverse: flag(d, "Rvrs").unwrap_or(false),
            }
        }
        "photoFilter" => {
            let Some(color) = d.descriptor("Clr ").and_then(color_of) else {
                return why("has no filter colour".to_string());
            };
            StepOp::PhotoFilter {
                color,
                density: num("Dnst", 25.0),
                preserve_luminosity: flag(d, "PrsL").unwrap_or(true),
            }
        }
        "channelMixer" => {
            let row = |k: &str, own: usize| {
                let mut r = [0.0; 4];
                r[own] = 100.0;
                if let Some(m) = d.descriptor(k) {
                    r = [
                        m.number("Rd  ").unwrap_or(r[0]),
                        m.number("Grn ").unwrap_or(r[1]),
                        m.number("Bl  ").unwrap_or(r[2]),
                        m.number("Cnst").unwrap_or(0.0),
                    ];
                }
                r
            };
            let monochrome = flag(d, "Mnch").unwrap_or(false);
            if monochrome {
                let gray = row("Gry ", 0);
                StepOp::ChannelMixer {
                    red: gray,
                    green: gray,
                    blue: gray,
                    monochrome,
                }
            } else {
                StepOp::ChannelMixer {
                    red: row("Rd  ", 0),
                    green: row("Grn ", 1),
                    blue: row("Bl  ", 2),
                    monochrome,
                }
            }
        }
        "crop" => {
            if d.number("Angl").is_some_and(|a| a.abs() > 1e-9) {
                return why("crops at an angle, which Crop here does not".to_string());
            }
            let rect = match d.descriptor("T   ") {
                Some(r) => {
                    let e = |k: &str| r.number(k).unwrap_or(0.0);
                    Some([e("Left"), e("Top "), e("Rght"), e("Btom")])
                }
                None => None,
            };
            StepOp::Crop { rect }
        }
        "trim" => StepOp::Trim {
            basis: match enum_value(d, "trimBasedOn") {
                Some("TpLf") | Some("topLeftPixelColor") => TrimBasis::TopLeft,
                Some("BtRg") | Some("bottomRightPixelColor") => TrimBasis::BottomRight,
                _ => TrimBasis::Transparent,
            },
            top: flag(d, "Top ").unwrap_or(true),
            left: flag(d, "Left").unwrap_or(true),
            bottom: flag(d, "Btom").unwrap_or(true),
            right: flag(d, "Rght").unwrap_or(true),
        },
        "convertMode" => match d.get("T   ") {
            Some(Value::Class { class_id, .. }) => StepOp::ConvertMode {
                mode: match class_id.as_str() {
                    "RGBM" | "RGBColorMode" => ModeTarget::Rgb,
                    "Grys" | "Grsc" | "grayscaleMode" => ModeTarget::Grayscale,
                    "LbCM" | "labColorMode" => ModeTarget::Lab,
                    "CMYM" | "CMYKColorMode" => ModeTarget::Cmyk,
                    "IndC" | "indexedColorMode" => ModeTarget::Indexed,
                    "BtmM" | "bitmapMode" => ModeTarget::Bitmap,
                    "DtnM" | "duotoneMode" => ModeTarget::Duotone,
                    other => return why(format!("converts to {other}, which has no equivalent")),
                },
            },
            _ => match d.number("Dpth") {
                Some(bits) if [8.0, 16.0, 32.0].contains(&bits) => {
                    StepOp::BitDepth { bits: bits as u8 }
                }
                _ => return why("names no mode and no bit depth".to_string()),
            },
        },
        "transform" => {
            let target = match transform_target(d) {
                Ok(t) => t,
                Err(e) => return why(e),
            };
            let pivot = match enum_value(d, "FTcs") {
                Some("Qcsi") => Pivot::Point(offset_of(d, "Pstn")),
                Some(code) => match QCS.iter().find(|(c, _)| *c == code) {
                    Some((_, at)) => Pivot::Bounds(*at),
                    None => return why(format!("turns about {code}, which has no equivalent")),
                },
                None => Pivot::Bounds([0.5, 0.5]),
            };
            let skew = d.descriptor("Skew").map_or([0.0; 2], |s| {
                [
                    s.number("Hrzn").unwrap_or(0.0),
                    s.number("Vrtc").unwrap_or(0.0),
                ]
            });
            StepOp::Transform {
                target,
                pivot,
                offset: offset_of(d, "Ofst"),
                scale: [num("Wdth", 100.0), num("Hght", 100.0)],
                angle: num("Angl", 0.0),
                skew,
            }
        }
        "move" => match d.get("T   ") {
            Some(Value::Descriptor(o)) if o.class_id == "Ofst" => {
                let target = match transform_target(d) {
                    Ok(t) => t,
                    Err(e) => return why(e),
                };
                StepOp::Transform {
                    target,
                    pivot: Pivot::Bounds([0.5, 0.5]),
                    offset: offset_of(d, "T   "),
                    scale: [100.0; 2],
                    angle: 0.0,
                    skew: [0.0; 2],
                }
            }
            Some(Value::Reference(items)) => match layer_ref(items) {
                Some(LayerRef::Name(_)) | None => {
                    return why("moves a layer to a place this application cannot read".to_string())
                }
                Some(r) => StepOp::ArrangeLayer(r),
            },
            _ => return why("moves by nothing this application can read".to_string()),
        },
        "duplicate" => {
            if !layer_target(d) {
                return why("duplicates something other than a layer".to_string());
            }
            StepOp::DuplicateLayer {
                name: d.text("Nm  ").map(str::to_string),
            }
        }
        "delete" => match target_class(d) {
            Some("Lyr ") => StepOp::DeleteLayer,
            _ => return why("deletes something other than a layer".to_string()),
        },
        "mergeLayersNew" => StepOp::MergeDown,
        "mergeVisible" => StepOp::MergeVisible,
        "flattenImage" => StepOp::Flatten,
        "make" => match target_class(d) {
            Some("layerSection") => {
                if d.get("From").is_some() {
                    StepOp::GroupLayers
                } else {
                    StepOp::MakeGroup
                }
            }
            _ => return None,
        },
        "set" => {
            if target_class(d) != Some("Lyr ") {
                return None;
            }
            let Some(props) = d.descriptor("T   ") else {
                return why("sets no layer property".to_string());
            };
            let blend = match enum_value(props, "Md  ") {
                Some(v) => match blend_from_blnm(v) {
                    Some(m) => Some(m),
                    None => return why(format!("sets blend mode {v}, which has no equivalent")),
                },
                None => None,
            };
            let op = StepOp::SetLayer {
                name: props.text("Nm  ").map(str::to_string),
                opacity: props.number("Opct"),
                blend,
            };
            if op
                == (StepOp::SetLayer {
                    name: None,
                    opacity: None,
                    blend: None,
                })
            {
                return why(
                    "sets only layer properties this application has no equivalent for".to_string(),
                );
            }
            op
        }
        "show" | "hide" => StepOp::SetVisibility {
            visible: id == "show",
        },
        "select" => match d.get("null") {
            Some(Value::Reference(items)) => match layer_ref(items) {
                Some(r) => StepOp::SelectLayer(r),
                None => return why("selects something other than a layer".to_string()),
            },
            _ => return why("selects nothing this application can read".to_string()),
        },
        "colorRange" => {
            let (Some(lo), Some(hi)) = (d.descriptor("Mnm "), d.descriptor("Mxm ")) else {
                return why(
                    "selects a preset range (reds, highlights, …), not sampled colours".to_string(),
                );
            };
            let lab = |c: &Descriptor| {
                [
                    c.number("Lmnc").unwrap_or(0.0),
                    c.number("A   ").unwrap_or(0.0),
                    c.number("B   ").unwrap_or(0.0),
                ]
            };
            let (a, b) = (lab(lo), lab(hi));
            StepOp::ColorRange {
                color: lab_to_rgb([
                    (a[0] + b[0]) / 2.0,
                    (a[1] + b[1]) / 2.0,
                    (a[2] + b[2]) / 2.0,
                ]),
                fuzziness: num("Fzns", 40.0),
                invert: flag(d, "Invr").unwrap_or(false),
            }
        }
        "feather" => StepOp::Feather {
            radius: num("Rds ", 0.0),
        },
        "expand" => StepOp::Expand {
            by: num("By  ", 0.0),
        },
        "contract" => StepOp::Contract {
            by: num("By  ", 0.0),
        },
        "border" => StepOp::Border {
            width: num("Wdth", 0.0),
        },
        "smoothness" => StepOp::Smooth {
            radius: num("Rds ", 0.0),
        },
        "stroke" => StepOp::Stroke {
            width: num("Wdth", 1.0),
            location: match enum_value(d, "Lctn") {
                Some("Otsd") | Some("outside") => StrokeAt::Outside,
                Some("CntW") | Some("Cntr") | Some("center") => StrokeAt::Center,
                _ => StrokeAt::Inside,
            },
            opacity: num("Opct", 100.0),
            color: d.descriptor("Clr ").and_then(color_of),
            blend: enum_value(d, "Md  ")
                .and_then(blend_from_blnm)
                .unwrap_or(BlendMode::Normal),
        },
        "copyEvent" => StepOp::Copy,
        "copyMerged" => StepOp::CopyMerged,
        "paste" => StepOp::Paste,
        "cut" => StepOp::Cut,
        "copyToLayer" => StepOp::LayerVia { cut: false },
        "cutToLayer" => StepOp::LayerVia { cut: true },
        "addNoise" => StepOp::AddNoise {
            amount: num("Nose", 10.0),
            gaussian: matches!(
                enum_value(d, "Dstr"),
                Some("Gsn ") | Some("gaussianDistribution")
            ),
            monochromatic: flag(d, "Mnch").unwrap_or(false),
        },
        "motionBlur" => StepOp::MotionBlur {
            angle: num("Angl", 0.0),
            distance: num("Dstn", 10.0),
        },
        "highPass" => StepOp::HighPass {
            radius: num("Rds ", 10.0),
        },
        "smartSharpen" => StepOp::SmartSharpen {
            amount: num("Amnt", 100.0),
            radius: num("Rds ", 1.0),
            noise_reduction: num("noiseReduction", 0.0),
        },
        "export" => StepOp::Export,
        // Save with a format ("As") is Save As; plain Save is W13-E's.
        "save" if d.get("As  ").is_some() => StepOp::Export,
        _ => return None,
    };
    Some(Ok(op))
}

// ------------------------------------------------------------ writing

fn channel_ref(c: ToneChannel) -> Value {
    let value = match c {
        ToneChannel::Composite => "Cmps",
        ToneChannel::Red => "Rd  ",
        ToneChannel::Green => "Grn ",
        ToneChannel::Blue => "Bl  ",
    };
    Value::Reference(vec![RefItem::Enumerated {
        name: String::new(),
        class_id: "Chnl".to_string(),
        type_id: "Chnl".to_string(),
        value: value.to_string(),
    }])
}

fn int(v: f64) -> Value {
    Value::Integer(v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32)
}

fn list(values: impl IntoIterator<Item = Value>) -> Value {
    Value::List(values.into_iter().collect())
}

fn custom_preset() -> (&'static str, Value) {
    (
        "presetKind",
        enumerated("presetKindType", "presetKindCustom"),
    )
}

fn selection_ref() -> Value {
    Value::Reference(vec![RefItem::Property {
        name: String::new(),
        class_id: "Chnl".to_string(),
        key_id: "fsel".to_string(),
    }])
}

fn layer_ref_value(r: &LayerRef) -> Value {
    let item = match r {
        LayerRef::Name(n) => RefItem::Name {
            name: String::new(),
            class_id: "Lyr ".to_string(),
            value: n.clone(),
        },
        LayerRef::Index(i) => RefItem::Index(*i),
        LayerRef::Forward | LayerRef::Backward | LayerRef::Front | LayerRef::Back => {
            RefItem::Enumerated {
                name: String::new(),
                class_id: "Lyr ".to_string(),
                type_id: "Ordn".to_string(),
                value: match r {
                    LayerRef::Forward => "Frwr",
                    LayerRef::Backward => "Bckw",
                    LayerRef::Front => "Frnt",
                    _ => "Back",
                }
                .to_string(),
            }
        }
    };
    Value::Reference(vec![item])
}

fn point(class: &str, unit_code: &[u8; 4], xy: [f64; 2]) -> Value {
    Value::Descriptor(desc(
        class,
        vec![
            ("Hrzn", unit(unit_code, xy[0])),
            ("Vrtc", unit(unit_code, xy[1])),
        ],
    ))
}

/// The step Photoshop records for `op`, for every operation this module
/// adds; `None` for the W13-E ones, which `StepOp::to_step` writes.
pub(super) fn to_step(op: &StepOp) -> Option<AtnStep> {
    let mut char_id = true;
    let (event, name, items): (&str, &str, Vec<(&str, Value)>) = match op {
        StepOp::Levels(entries) => (
            "Lvls",
            "Levels",
            vec![
                custom_preset(),
                (
                    "Adjs",
                    list(entries.iter().map(|e| {
                        Value::Descriptor(desc(
                            "LvlA",
                            vec![
                                ("Chnl", channel_ref(e.channel)),
                                ("Inpt", list(e.input.map(int))),
                                ("Gmm ", Value::Double(e.gamma)),
                                ("Otpt", list(e.output.map(int))),
                            ],
                        ))
                    })),
                ),
            ],
        ),
        StepOp::Curves(entries) => (
            "Crvs",
            "Curves",
            vec![
                custom_preset(),
                (
                    "Adjs",
                    list(entries.iter().map(|e| {
                        Value::Descriptor(desc(
                            "CrvA",
                            vec![
                                ("Chnl", channel_ref(e.channel)),
                                (
                                    "Crv ",
                                    list(e.points.iter().map(|p| {
                                        Value::Descriptor(desc(
                                            "Pnt ",
                                            vec![
                                                ("Hrzn", Value::Double(p[0])),
                                                ("Vrtc", Value::Double(p[1])),
                                            ],
                                        ))
                                    })),
                                ),
                            ],
                        ))
                    })),
                ),
            ],
        ),
        StepOp::HueSaturation {
            hue,
            saturation,
            lightness,
            colorize,
        } => (
            "HStr",
            "Hue/Saturation",
            vec![
                custom_preset(),
                ("Clrz", Value::Bool(*colorize)),
                (
                    "Adjs",
                    list([Value::Descriptor(desc(
                        "Hst2",
                        vec![
                            ("H   ", int(*hue)),
                            ("Strt", int(*saturation)),
                            ("Lght", int(*lightness)),
                        ],
                    ))]),
                ),
            ],
        ),
        StepOp::ColorBalance {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        } => (
            "ClrB",
            "Color Balance",
            vec![
                ("ShdL", list(shadows.map(int))),
                ("MdtL", list(midtones.map(int))),
                ("HghL", list(highlights.map(int))),
                ("PrsL", Value::Bool(*preserve_luminosity)),
            ],
        ),
        StepOp::BlackAndWhite { weights, tint } => {
            let mut items = vec![custom_preset()];
            for (k, w) in ["Rd  ", "Yllw", "Grn ", "Cyn ", "Bl  ", "Mgnt"]
                .into_iter()
                .zip(weights)
            {
                items.push((k, int(*w)));
            }
            items.push(("useTint", Value::Bool(tint.is_some())));
            if let Some(t) = tint {
                items.push(("tintColor", rgbc(*t)));
            }
            ("BanW", "Black & White", items)
        }
        StepOp::Vibrance {
            vibrance,
            saturation,
        } => {
            char_id = false;
            (
                "vibrance",
                "Vibrance",
                vec![("vibrance", int(*vibrance)), ("Strt", int(*saturation))],
            )
        }
        StepOp::Exposure {
            exposure,
            offset,
            gamma,
        } => {
            char_id = false;
            (
                "exposure",
                "Exposure",
                vec![
                    custom_preset(),
                    ("Exps", Value::Double(*exposure)),
                    ("Ofst", Value::Double(*offset)),
                    ("gammaCorrection", Value::Double(*gamma)),
                ],
            )
        }
        StepOp::Threshold { level } => ("Thrs", "Threshold", vec![("Lvl ", int(*level))]),
        StepOp::Posterize { levels } => (
            "Pstr",
            "Posterize",
            vec![("Lvls", Value::Integer(*levels as i32))],
        ),
        StepOp::GradientMap { stops, reverse } => {
            let clrs = list(stops.iter().map(|(at, rgb)| {
                Value::Descriptor(desc(
                    "Clrt",
                    vec![
                        ("Clr ", rgbc(*rgb)),
                        ("Type", enumerated("Clry", "UsrS")),
                        ("Lctn", int(at * 4096.0)),
                        ("Mdpn", Value::Integer(50)),
                    ],
                ))
            }));
            let trns = list([0.0, 4096.0].map(|at| {
                Value::Descriptor(desc(
                    "TrnS",
                    vec![
                        ("Opct", unit(b"#Prc", 100.0)),
                        ("Lctn", int(at)),
                        ("Mdpn", Value::Integer(50)),
                    ],
                ))
            }));
            let mut grad = desc(
                "Grdn",
                vec![
                    ("GrdF", enumerated("GrdF", "CstS")),
                    ("Intr", Value::Double(4096.0)),
                    ("Clrs", clrs),
                    ("Trns", trns),
                ],
            );
            grad.items
                .insert(0, ("Nm  ".to_string(), Value::Text("Custom".into())));
            (
                "GrMp",
                "Gradient Map",
                vec![
                    ("Grad", Value::Descriptor(grad)),
                    ("Rvrs", Value::Bool(*reverse)),
                    ("Dthr", Value::Bool(false)),
                ],
            )
        }
        StepOp::PhotoFilter {
            color,
            density,
            preserve_luminosity,
        } => {
            char_id = false;
            (
                "photoFilter",
                "Photo Filter",
                vec![
                    ("Clr ", rgbc(*color)),
                    ("Dnst", int(*density)),
                    ("PrsL", Value::Bool(*preserve_luminosity)),
                ],
            )
        }
        StepOp::ChannelMixer {
            red,
            green,
            blue,
            monochrome,
        } => {
            let row = |r: &[f64; 4]| {
                Value::Descriptor(desc(
                    "ChMx",
                    vec![
                        ("Rd  ", unit(b"#Prc", r[0])),
                        ("Grn ", unit(b"#Prc", r[1])),
                        ("Bl  ", unit(b"#Prc", r[2])),
                        ("Cnst", unit(b"#Prc", r[3])),
                    ],
                ))
            };
            let mut items = vec![custom_preset(), ("Mnch", Value::Bool(*monochrome))];
            if *monochrome {
                items.push(("Gry ", row(red)));
            } else {
                items.push(("Rd  ", row(red)));
                items.push(("Grn ", row(green)));
                items.push(("Bl  ", row(blue)));
            }
            ("ChnM", "Channel Mixer", items)
        }
        StepOp::Crop { rect } => {
            let mut items = Vec::new();
            if let Some([l, t, r, b]) = rect {
                items.push((
                    "T   ",
                    Value::Descriptor(desc(
                        "Rctn",
                        vec![
                            ("Top ", unit(b"#Pxl", *t)),
                            ("Left", unit(b"#Pxl", *l)),
                            ("Btom", unit(b"#Pxl", *b)),
                            ("Rght", unit(b"#Pxl", *r)),
                        ],
                    )),
                ));
                items.push(("Angl", unit(b"#Ang", 0.0)));
            }
            items.push(("Dlt ", Value::Bool(true)));
            ("Crop", "Crop", items)
        }
        StepOp::Trim {
            basis,
            top,
            left,
            bottom,
            right,
        } => {
            char_id = false;
            let b = match basis {
                TrimBasis::Transparent => "Trns",
                TrimBasis::TopLeft => "TpLf",
                TrimBasis::BottomRight => "BtRg",
            };
            (
                "trim",
                "Trim",
                vec![
                    ("trimBasedOn", enumerated("trimBasedOn", b)),
                    ("Top ", Value::Bool(*top)),
                    ("Btom", Value::Bool(*bottom)),
                    ("Left", Value::Bool(*left)),
                    ("Rght", Value::Bool(*right)),
                ],
            )
        }
        StepOp::ConvertMode { mode } => {
            let class = match mode {
                ModeTarget::Rgb => "RGBM",
                ModeTarget::Grayscale => "Grys",
                ModeTarget::Lab => "LbCM",
                ModeTarget::Cmyk => "CMYM",
                ModeTarget::Indexed => "IndC",
                ModeTarget::Bitmap => "BtmM",
                ModeTarget::Duotone => "DtnM",
            };
            (
                "CnvM",
                "Convert Mode",
                vec![(
                    "T   ",
                    Value::Class {
                        name: String::new(),
                        class_id: class.to_string(),
                    },
                )],
            )
        }
        StepOp::BitDepth { bits } => (
            "CnvM",
            "Convert Mode",
            vec![("Dpth", Value::Integer(i32::from(*bits)))],
        ),
        StepOp::Transform {
            target,
            pivot,
            offset,
            scale,
            angle,
            skew,
        } => {
            let null = match target {
                TransformTarget::Layer => reference("Lyr ", true),
                TransformTarget::Selection => selection_ref(),
            };
            let mut items = vec![("null", null)];
            match pivot {
                Pivot::Point(at) => {
                    items.push(("FTcs", enumerated("QCSt", "Qcsi")));
                    items.push(("Pstn", point("Pnt ", b"#Pxl", *at)));
                }
                Pivot::Bounds(at) => {
                    let code = QCS
                        .iter()
                        .find(|(_, p)| p == at)
                        .map_or("Qcsa", |(c, _)| *c);
                    items.push(("FTcs", enumerated("QCSt", code)));
                }
            }
            items.push(("Ofst", point("Ofst", b"#Pxl", *offset)));
            items.push(("Wdth", unit(b"#Prc", scale[0])));
            items.push(("Hght", unit(b"#Prc", scale[1])));
            items.push(("Angl", unit(b"#Ang", *angle)));
            items.push(("Skew", point("Pnt ", b"#Ang", *skew)));
            items.push(("Intr", enumerated("Intp", "Bcbc")));
            ("Trnf", "Transform", items)
        }
        StepOp::ArrangeLayer(r) => (
            "move",
            "Move",
            vec![
                ("null", reference("Lyr ", true)),
                ("T   ", layer_ref_value(r)),
            ],
        ),
        StepOp::DuplicateLayer { name } => {
            let mut items = vec![("null", reference("Lyr ", true))];
            if let Some(n) = name {
                items.push(("Nm  ", Value::Text(n.clone())));
            }
            items.push(("Vrsn", Value::Integer(5)));
            ("Dplc", "Duplicate", items)
        }
        StepOp::DeleteLayer => ("Dlt ", "Delete", vec![("null", reference("Lyr ", true))]),
        StepOp::MergeDown => ("Mrg2", "Merge Layers", vec![]),
        StepOp::MergeVisible => ("MrgV", "Merge Visible", vec![]),
        StepOp::Flatten => ("FltI", "Flatten Image", vec![]),
        StepOp::MakeGroup => (
            "Mk  ",
            "Make",
            vec![("null", reference("layerSection", false))],
        ),
        StepOp::GroupLayers => (
            "Mk  ",
            "Make",
            vec![
                ("null", reference("layerSection", false)),
                ("From", reference("Lyr ", true)),
            ],
        ),
        StepOp::SetLayer {
            name,
            opacity,
            blend,
        } => {
            let mut props = Vec::new();
            if let Some(n) = name {
                props.push(("Nm  ", Value::Text(n.clone())));
            }
            if let Some(o) = opacity {
                props.push(("Opct", unit(b"#Prc", *o)));
            }
            if let Some(m) = blend {
                props.push(("Md  ", enumerated("BlnM", blnm(*m))));
            }
            (
                "setd",
                "Set",
                vec![
                    ("null", reference("Lyr ", true)),
                    ("T   ", Value::Descriptor(desc("Lyr ", props))),
                ],
            )
        }
        StepOp::SetVisibility { visible } => (
            if *visible { "Shw " } else { "Hd  " },
            if *visible { "Show" } else { "Hide" },
            vec![("null", list([reference("Lyr ", true)]))],
        ),
        StepOp::SelectLayer(r) => (
            "slct",
            "Select",
            vec![("null", layer_ref_value(r)), ("MkVs", Value::Bool(false))],
        ),
        StepOp::ColorRange {
            color,
            fuzziness,
            invert,
        } => (
            "ClrR",
            "Color Range",
            vec![
                ("Fzns", int(*fuzziness)),
                ("Mnm ", lab_value("LbCC", *color)),
                ("Mxm ", lab_value("LbCC", *color)),
                ("Invr", Value::Bool(*invert)),
            ],
        ),
        StepOp::Feather { radius } => ("Fthr", "Feather", vec![("Rds ", unit(b"#Pxl", *radius))]),
        StepOp::Expand { by } => ("Expn", "Expand", vec![("By  ", unit(b"#Pxl", *by))]),
        StepOp::Contract { by } => ("Cntc", "Contract", vec![("By  ", unit(b"#Pxl", *by))]),
        StepOp::Border { width } => ("Brdr", "Border", vec![("Wdth", unit(b"#Pxl", *width))]),
        StepOp::Smooth { radius } => ("Smth", "Smooth", vec![("Rds ", unit(b"#Pxl", *radius))]),
        StepOp::Stroke {
            width,
            location,
            opacity,
            color,
            blend,
        } => {
            let at = match location {
                StrokeAt::Inside => "Insd",
                StrokeAt::Center => "CntW",
                StrokeAt::Outside => "Otsd",
            };
            let mut items = vec![
                ("Wdth", int(*width)),
                ("Lctn", enumerated("StrL", at)),
                ("Opct", unit(b"#Prc", *opacity)),
                ("Md  ", enumerated("BlnM", blnm(*blend))),
            ];
            if let Some(c) = color {
                items.push(("Clr ", rgbc(*c)));
            }
            ("Strk", "Stroke", items)
        }
        StepOp::Copy => ("copy", "Copy", vec![]),
        StepOp::CopyMerged => ("CpyM", "Copy Merged", vec![]),
        StepOp::Paste => ("past", "Paste", vec![]),
        StepOp::Cut => ("cut ", "Cut", vec![]),
        StepOp::LayerVia { cut } => {
            if *cut {
                ("CtTL", "Layer Via Cut", vec![])
            } else {
                ("CpTL", "Layer Via Copy", vec![])
            }
        }
        StepOp::AddNoise {
            amount,
            gaussian,
            monochromatic,
        } => (
            "AdNs",
            "Add Noise",
            vec![
                (
                    "Dstr",
                    enumerated("Dstr", if *gaussian { "Gsn " } else { "Unfr" }),
                ),
                ("Nose", unit(b"#Prc", *amount)),
                ("Mnch", Value::Bool(*monochromatic)),
            ],
        ),
        StepOp::MotionBlur { angle, distance } => (
            "MtnB",
            "Motion Blur",
            vec![("Angl", int(*angle)), ("Dstn", unit(b"#Pxl", *distance))],
        ),
        StepOp::HighPass { radius } => {
            ("HghP", "High Pass", vec![("Rds ", unit(b"#Pxl", *radius))])
        }
        StepOp::SmartSharpen {
            amount,
            radius,
            noise_reduction,
        } => {
            char_id = false;
            (
                "smartSharpen",
                "Smart Sharpen",
                vec![
                    ("Amnt", unit(b"#Prc", *amount)),
                    ("Rds ", unit(b"#Pxl", *radius)),
                    ("noiseReduction", unit(b"#Prc", *noise_reduction)),
                ],
            )
        }
        StepOp::Export => ("Expr", "Export", vec![]),
        _ => return None,
    };
    let descriptor = (!items.is_empty()).then(|| desc("null", items));
    let mut step = AtnStep::new(event, name, descriptor);
    step.char_id = char_id;
    Some(step)
}
