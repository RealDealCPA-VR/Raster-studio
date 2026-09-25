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
//! [`MAPPED`] lists the filters this build writes as Photoshop smart filters:
//! Average, Blur, Blur More, Box Blur, Gaussian Blur, Motion Blur, Surface
//! Blur, Sharpen, Sharpen More, Sharpen Edges, Unsharp Mask, Add Noise,
//! Despeckle, Dust & Scratches, Median, Mosaic, High Pass, Maximum and
//! Minimum. Their settings are Photoshop's, so a few are stored on
//! Photoshop's grid rather than this build's: a threshold (Unsharp Mask,
//! Surface Blur, Dust & Scratches) is whole levels `0..=255`, a Motion Blur
//! angle whole degrees, Add Noise's amount a percentage. The `edge` choice
//! this build's dialogs offer has no Photoshop setting: a filter set to Wrap
//! or Mirror is refused rather than written as Clamp.
//!
//! Every other filter (Lens Blur, Smart Blur, Radial Blur, Smart Sharpen,
//! Reduce Noise, the distort, render, stylize and other filters, Fourier,
//! Flame, ...) is refused by name: the caller keeps the smart object's
//! rendered pixels instead ([`encode_stack`] says which filters).

use std::collections::BTreeMap;

use layer_model::{BlendMode, SmartFilter, SmartParam};

use crate::bytes::Cursor;
use crate::descriptor::{Descriptor, Value};
use crate::effects::blend_from_blnm;
use crate::limits::ReadOptions;
use crate::model::PsdLayer;

/// The `SoLd` descriptor key the stack lives under.
pub const FILTER_FX_KEY: &str = "filterFX";

/// The smart-filter keys (`ui::menu::FilterId` variant names) this module
/// writes as Photoshop smart filters, each with its Photoshop event code.
pub const MAPPED: [(&str, &str); 19] = [
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
];

/// `true` when [`encode_stack`] can write the filter keyed `key`.
pub fn has_photoshop_equivalent(key: &str) -> bool {
    MAPPED.iter().any(|(k, _)| *k == key)
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

/// The four-character event code as Photoshop's `filterID`, or Photopea's
/// 777 for a longer code.
fn filter_id(code: &str) -> i32 {
    match <[u8; 4]>::try_from(code.as_bytes()) {
        Ok(b) => u32::from_be_bytes(b) as i32,
        Err(_) => 777,
    }
}

fn rgbc(v: f64) -> Value {
    let mut d = Descriptor::new("RGBC");
    for k in ["Rd  ", "Grn ", "Bl  "] {
        let _ = d.push(k, Value::Double(v));
    }
    Value::Descriptor(d)
}

/// The `Fltr` settings of `f`, or why it has none a `.psd` can hold. `None`
/// in the `Ok` is a filter without settings.
fn settings(f: &SmartFilter, code: &str) -> Result<Option<Descriptor>, String> {
    let has_edge = matches!(
        f.filter.as_str(),
        "BoxBlur"
            | "GaussianBlur"
            | "SurfaceBlur"
            | "UnsharpMask"
            | "DustAndScratches"
            | "Median"
            | "HighPass"
            | "Maximum"
            | "Minimum"
    );
    if has_edge && num(f, "edge", 0.0) != 0.0 {
        return Err(format!(
            "{}'s Wrap/Mirror edges have no Photoshop setting",
            display_name(&f.filter)
        ));
    }
    let items: Vec<(&str, Value)> = match f.filter.as_str() {
        "BoxBlur" => vec![("Rds ", px(num(f, "radius", 4.0)))],
        "GaussianBlur" => vec![("Rds ", px(num(f, "radius", 4.0)))],
        "MotionBlur" => vec![
            ("Angl", Value::Integer(num(f, "angle", 0.0).round() as i32)),
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
            ("FlRs", Value::Integer(num(f, "seed", 1.0) as i32)),
        ],
        "DustAndScratches" => vec![
            ("Rds ", Value::Integer(num(f, "radius", 2.0) as i32)),
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
        _ => return Ok(None),
    };
    let mut d = Descriptor::new(code);
    for (k, v) in items {
        let _ = d.push(k, v);
    }
    Ok(Some(d))
}

/// One `filterFX` entry for `f`, or why a `.psd` cannot hold it.
pub fn encode_filter(f: &SmartFilter) -> Result<Descriptor, String> {
    let Some((_, code)) = MAPPED.iter().find(|(k, _)| *k == f.filter) else {
        return Err(format!(
            "{} has no Photoshop smart filter",
            display_name(&f.filter)
        ));
    };
    let settings = settings(f, code)?;
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
    push("FrgC", rgbc(0.0));
    push("BckC", rgbc(255.0));
    if let Some(s) = settings {
        push("Fltr", Value::Descriptor(s));
    }
    push("filterID", Value::Integer(filter_id(code)));
    Ok(d)
}

/// The whole stack as the `filterFX` descriptor (class `filterFXStyle`), or
/// every reason a filter in it cannot be written. The stack's order is kept:
/// index 0 is applied first in both.
pub fn encode_stack(stack: &[SmartFilter]) -> Result<Descriptor, Vec<String>> {
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
    push("filterMaskEnable", Value::Bool(false));
    push("filterMaskLinked", Value::Bool(true));
    push("filterMaskExtendWithWhite", Value::Bool(true));
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
    let n = |k: &str, default: f64| s.number(k).filter(|v| v.is_finite()).unwrap_or(default);
    let float = |v: f64| SmartParam::Float(v as f32);
    let int =
        |v: f64| SmartParam::Int(v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32);
    let edge = ("edge", SmartParam::Choice(0));
    let params: Vec<(&str, SmartParam)> = match *key {
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
                SmartParam::Choice(u32::from(matches!(
                    s.get("Dstr"),
                    Some(Value::Enumerated { value, .. }) if value.trim_end() == "Gsn"
                ))),
            ),
            (
                "monochromatic",
                SmartParam::Bool(matches!(s.get("Mnch"), Some(Value::Bool(true)))),
            ),
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
        _ => Vec::new(),
    };
    let params: BTreeMap<String, SmartParam> = params
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
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
        vec![
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
        ]
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
    fn a_filter_photoshop_lacks_or_a_wrap_edge_is_refused_by_name() {
        let refused = encode_stack(&[
            filter("GaussianBlur", &[]),
            filter("LensBlur", &[]),
            filter("GaussianBlur", &[("edge", SmartParam::Choice(1))]),
        ])
        .unwrap_err();
        assert_eq!(refused.len(), 2, "{refused:?}");
        assert!(refused[0].contains("Lens Blur"), "{refused:?}");
        assert!(refused[1].contains("Wrap/Mirror"), "{refused:?}");
        assert!(!has_photoshop_equivalent("LensBlur"));
        assert!(has_photoshop_equivalent("GaussianBlur"));
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
