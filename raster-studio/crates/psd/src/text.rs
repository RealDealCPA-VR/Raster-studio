//! `TySh` — the type-tool object setting, which is where a text layer keeps its
//! string.
//!
//! ```text
//! u16 version (1)
//! f64 × 6      transform: xx xy yx yy tx ty
//! u16 text version (50)   u32 descriptor version (16)   descriptor
//! u16 warp version (1)    u32 descriptor version (16)   descriptor
//! i32 × 4      left top right bottom
//! ```
//!
//! The string lives at key `Txt ` in the first descriptor. Everything about how
//! it is *set* — fonts, runs, tracking, justification — lives in the
//! `EngineData` blob under `EngineData`, in the text engine's own
//! PostScript-like syntax, which [`crate::engine_data`] parses (bounded; it is
//! untrusted input) and writes.
//!
//! # Reading and writing
//!
//! [`parse`] extracts the transform and the string; [`engine_text`] reads the
//! engine data's style runs, paragraph runs and frame. [`crate::write`] writes
//! back the bytes that were read, which round-trips a type layer exactly.
//! [`build`] / [`build_styled`] synthesise a block for a layer this build
//! authored: the engine data they embed describes every character run (a
//! payload that does not makes Photoshop discard the layer).

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::engine_data::{self, EngineDataError, EngineText};
use crate::limits::ReadOptions;
use crate::model::TextData;

/// The identity transform, used when a block's transform cannot be read.
pub const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Parse a `TySh` payload, best-effort.
///
/// Always succeeds: `raw` is retained whatever happens, because a block this
/// crate cannot interpret still has to survive a save. Fields that could not be
/// read come back as [`IDENTITY`] and `None`.
pub fn parse(raw: &[u8], opts: &ReadOptions) -> TextData {
    let mut data = TextData {
        transform: IDENTITY,
        text: None,
        raw: raw.to_vec(),
    };
    let mut cur = Cursor::new(raw);
    if cur.u16().is_err() {
        return data;
    }
    let mut transform = [0.0f64; 6];
    for slot in transform.iter_mut() {
        match cur.f64() {
            Ok(v) => *slot = v,
            Err(_) => return data,
        }
    }
    data.transform = transform;

    // text version, then descriptor version.
    if cur.u16().is_err() || cur.u32().is_err() {
        return data;
    }
    if let Ok(desc) = Descriptor::read(&mut cur, opts) {
        data.text = desc.text("Txt ").map(str::to_owned);
    }
    data
}

/// The warp descriptor, when it can be read. Mostly useful for telling a warped
/// type layer from a flat one.
pub fn warp(raw: &[u8], opts: &ReadOptions) -> Option<Descriptor> {
    let mut cur = Cursor::new(raw);
    cur.u16().ok()?;
    for _ in 0..6 {
        cur.f64().ok()?;
    }
    cur.u16().ok()?;
    cur.u32().ok()?;
    Descriptor::read(&mut cur, opts).ok()?;
    cur.u16().ok()?;
    cur.u32().ok()?;
    Descriptor::read(&mut cur, opts).ok()
}

// -------------------------------------------------- card 079: synthesis

/// The engine data of a `TySh` block, read by [`crate::engine_data::extract`].
///
/// `Ok(None)` when the block has no readable text descriptor, no
/// `EngineData` key, or engine data without an `EngineDict` — nothing to read
/// is not an error. `Err` when the engine data is there but malformed or past
/// a cap; the caller reports it and falls back.
pub fn engine_text(raw: &[u8], opts: &ReadOptions) -> Result<Option<EngineText>, EngineDataError> {
    let mut cur = Cursor::new(raw);
    let header = (|| -> Option<()> {
        cur.u16().ok()?;
        for _ in 0..6 {
            cur.f64().ok()?;
        }
        cur.u16().ok()?;
        cur.u32().ok()?;
        Some(())
    })();
    if header.is_none() {
        return Ok(None);
    }
    let Ok(desc) = Descriptor::read(&mut cur, opts) else {
        return Ok(None);
    };
    match desc.get("EngineData") {
        Some(Value::RawData(bytes)) => engine_data::extract(bytes),
        _ => Ok(None),
    }
}

/// Build a complete `TySh` block for the supported text subset (card 079):
/// the string in the text descriptor, the layer's transform, a no-warp
/// block, the layer rectangle, and a complete engine-data payload — one
/// paragraph, one style run covering every character, the named font at the
/// named size with a black fill.
///
/// This is what makes a type layer this build writes *editable* in another
/// application rather than merely present: the engine data describes every
/// character run, which is what a re-typesetting reader demands (a partial
/// payload makes Photoshop discard the layer — see the module doc).
///
/// Only the subset is claimed. Styling this build does not encode here
/// (per-span styles, custom kerning, paragraph boxes) stays covered by the
/// layer's raster fallback pixels, and the export report names the subset.
pub fn build(
    text: &str,
    transform: [f64; 6],
    bounds: (i32, i32, i32, i32),
    font: &str,
    size_px: f64,
) -> Vec<u8> {
    let engine = EngineText {
        text: text.to_owned(),
        style_runs: vec![engine_data::StyleRun {
            length: text.encode_utf16().count(),
            style: engine_data::CharStyle {
                font: Some(font.to_owned()),
                size: Some(size_px),
                fill: Some([0.0, 0.0, 0.0, 1.0]),
                ..engine_data::CharStyle::default()
            },
        }],
        ..EngineText::default()
    };
    build_styled(&engine, transform, bounds)
}

/// Build a complete `TySh` block from an [`EngineText`] — every style run,
/// the paragraph run, the point/box frame — so a styled layer's fonts, sizes
/// and fills travel per run. [`engine_text`] reads back what this writes.
pub fn build_styled(
    engine: &EngineText,
    transform: [f64; 6],
    bounds: (i32, i32, i32, i32),
) -> Vec<u8> {
    build_styled_warped(engine, transform, bounds, &WarpSpec::NONE)
}

/// W15-C: [`build_styled`] with the layer's warp written in the block's warp
/// descriptor ([`write_warp`]); [`warp_spec`] reads it back.
pub fn build_styled_warped(
    engine: &EngineText,
    transform: [f64; 6],
    bounds: (i32, i32, i32, i32),
    warp: &WarpSpec,
) -> Vec<u8> {
    let text = engine.text.as_str();
    let mut s = Sink::new();
    s.u16(1);
    for v in transform {
        s.f64(v);
    }
    s.u16(50);
    s.u32(16);
    let mut d = Descriptor::new("TxLr");
    d.push("Txt ", Value::from(text)).unwrap();
    d.push(
        "textGridding",
        Value::Enumerated {
            type_id: "textGridding".into(),
            value: "None".into(),
        },
    )
    .unwrap();
    d.push("EngineData", Value::RawData(engine_data::write(engine)))
        .unwrap();
    d.write(&mut s).unwrap();
    s.u16(1);
    s.u32(16);
    // W15-C: the layer's warp, not a fixed warpNone.
    write_warp(&mut s, warp);
    s.i32(bounds.0);
    s.i32(bounds.1);
    s.i32(bounds.2);
    s.i32(bounds.3);
    s.into_inner()
}

// ------------------------------------------ W15-C: the warp descriptor

use layer_model::text::{TextWarp, WarpStyle};

/// W15-C: Photoshop's `warpStyle` enumeration — every style the Warp Text
/// dialog offers, plus `warpCustom` (a free 4x4 mesh).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WarpKind {
    #[default]
    None,
    Arc,
    ArcLower,
    ArcUpper,
    Arch,
    Bulge,
    ShellLower,
    ShellUpper,
    Flag,
    Wave,
    Fish,
    Rise,
    Fisheye,
    Inflate,
    Squeeze,
    Twist,
    Custom,
}

impl WarpKind {
    /// Every style, `None` first.
    pub const ALL: [WarpKind; 17] = [
        WarpKind::None,
        WarpKind::Arc,
        WarpKind::ArcLower,
        WarpKind::ArcUpper,
        WarpKind::Arch,
        WarpKind::Bulge,
        WarpKind::ShellLower,
        WarpKind::ShellUpper,
        WarpKind::Flag,
        WarpKind::Wave,
        WarpKind::Fish,
        WarpKind::Rise,
        WarpKind::Fisheye,
        WarpKind::Inflate,
        WarpKind::Squeeze,
        WarpKind::Twist,
        WarpKind::Custom,
    ];

    /// The enumeration value the descriptor spells the style with.
    pub const fn id(self) -> &'static str {
        match self {
            WarpKind::None => "warpNone",
            WarpKind::Arc => "warpArc",
            WarpKind::ArcLower => "warpArcLower",
            WarpKind::ArcUpper => "warpArcUpper",
            WarpKind::Arch => "warpArch",
            WarpKind::Bulge => "warpBulge",
            WarpKind::ShellLower => "warpShellLower",
            WarpKind::ShellUpper => "warpShellUpper",
            WarpKind::Flag => "warpFlag",
            WarpKind::Wave => "warpWave",
            WarpKind::Fish => "warpFish",
            WarpKind::Rise => "warpRise",
            WarpKind::Fisheye => "warpFisheye",
            WarpKind::Inflate => "warpInflate",
            WarpKind::Squeeze => "warpSqueeze",
            WarpKind::Twist => "warpTwist",
            WarpKind::Custom => "warpCustom",
        }
    }

    /// The style an enumeration value names, `None` for one this build does
    /// not know.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }
}

/// W15-C: a `TySh` warp descriptor, read or to be written.
///
/// `value`, `perspective` and `perspective_other` are the dialog's percents
/// (`-100..=100`): Bend, Horizontal and Vertical Distortion. `vertical` is
/// `warpRotate` = `Vrtc` (the dialog's Vertical orientation). `bounds` is the
/// envelope's rectangle in the block's own (`TySh`) space as `[top, left,
/// bottom, right]`; `mesh` is the Custom style's sixteen control points in
/// that same space, row by row (top row first, left to right).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarpSpec {
    pub kind: WarpKind,
    pub value: f64,
    pub perspective: f64,
    pub perspective_other: f64,
    pub vertical: bool,
    pub bounds: [f64; 4],
    pub mesh: Option<[[f64; 2]; 16]>,
}

impl WarpSpec {
    /// No warp: what a flat type layer carries.
    pub const NONE: WarpSpec = WarpSpec {
        kind: WarpKind::None,
        value: 0.0,
        perspective: 0.0,
        perspective_other: 0.0,
        vertical: false,
        bounds: [0.0; 4],
        mesh: None,
    };

    /// The descriptor for a layer's [`TextWarp`], fitted to `bounds` (the
    /// text block's line-box bounds in `TySh` space, `[top, left, bottom,
    /// right]`). A Custom warp without a stored mesh writes the flat one.
    pub fn from_text_warp(warp: &TextWarp, bounds: [f64; 4]) -> WarpSpec {
        let kind = match warp.style {
            WarpStyle::None => WarpKind::None,
            WarpStyle::Arc => WarpKind::Arc,
            WarpStyle::ArcLower => WarpKind::ArcLower,
            WarpStyle::ArcUpper => WarpKind::ArcUpper,
            WarpStyle::Arch => WarpKind::Arch,
            WarpStyle::Bulge => WarpKind::Bulge,
            WarpStyle::Flag => WarpKind::Flag,
            WarpStyle::Wave => WarpKind::Wave,
            WarpStyle::Fish => WarpKind::Fish,
            WarpStyle::Rise => WarpKind::Rise,
            WarpStyle::Fisheye => WarpKind::Fisheye,
            WarpStyle::Inflate => WarpKind::Inflate,
            WarpStyle::Squeeze => WarpKind::Squeeze,
            WarpStyle::Twist => WarpKind::Twist,
            WarpStyle::Custom => WarpKind::Custom,
        };
        let pct = |v: f32| {
            let v = f64::from(v) * 100.0;
            if v.is_finite() {
                v
            } else {
                0.0
            }
        };
        let [top, left, bottom, right] = bounds;
        let (w, h) = (right - left, bottom - top);
        let mesh = (kind == WarpKind::Custom).then(|| {
            let mut out = [[0.0f64; 2]; 16];
            for (i, p) in out.iter_mut().enumerate() {
                let flat = [(i % 4) as f64 / 3.0, (i / 4) as f64 / 3.0];
                let f = warp.mesh.map_or(flat, |m| {
                    let [x, y] = m[i];
                    if x.is_finite() && y.is_finite() {
                        [f64::from(x), f64::from(y)]
                    } else {
                        flat
                    }
                });
                *p = [left + f[0] * w, top + f[1] * h];
            }
            out
        });
        WarpSpec {
            kind,
            value: pct(warp.bend),
            perspective: pct(warp.horizontal),
            perspective_other: pct(warp.vertical),
            vertical: false,
            bounds,
            mesh,
        }
    }

    /// The layer's [`TextWarp`] for this descriptor, and what of it the layer
    /// model cannot hold (named, for the import report): the Shell styles
    /// come in as their Arc counterparts, the Vertical orientation as
    /// Horizontal. A Custom mesh is taken as fractions of `bounds`; with
    /// empty bounds the flat mesh stands in.
    pub fn to_text_warp(&self) -> (TextWarp, Vec<&'static str>) {
        let mut unmapped = Vec::new();
        let style = match self.kind {
            WarpKind::None => WarpStyle::None,
            WarpKind::Arc => WarpStyle::Arc,
            WarpKind::ArcLower => WarpStyle::ArcLower,
            WarpKind::ArcUpper => WarpStyle::ArcUpper,
            WarpKind::Arch => WarpStyle::Arch,
            WarpKind::Bulge => WarpStyle::Bulge,
            WarpKind::ShellLower => {
                unmapped.push("the Shell Lower warp (imported as Arc Lower)");
                WarpStyle::ArcLower
            }
            WarpKind::ShellUpper => {
                unmapped.push("the Shell Upper warp (imported as Arc Upper)");
                WarpStyle::ArcUpper
            }
            WarpKind::Flag => WarpStyle::Flag,
            WarpKind::Wave => WarpStyle::Wave,
            WarpKind::Fish => WarpStyle::Fish,
            WarpKind::Rise => WarpStyle::Rise,
            WarpKind::Fisheye => WarpStyle::Fisheye,
            WarpKind::Inflate => WarpStyle::Inflate,
            WarpKind::Squeeze => WarpStyle::Squeeze,
            WarpKind::Twist => WarpStyle::Twist,
            WarpKind::Custom => WarpStyle::Custom,
        };
        if style == WarpStyle::None {
            return (TextWarp::default(), unmapped);
        }
        if self.vertical {
            unmapped.push("the vertical warp orientation");
        }
        let frac = |v: f64, default: f32| {
            let f = (v / 100.0) as f32;
            if f.is_finite() {
                f
            } else {
                default
            }
        };
        let [top, left, bottom, right] = self.bounds;
        let (w, h) = (right - left, bottom - top);
        let mesh = match self.mesh {
            Some(points) if style == WarpStyle::Custom && w > 0.0 && h > 0.0 => {
                let mut out = [[0.0f32; 2]; 16];
                for (o, [x, y]) in out.iter_mut().zip(points) {
                    *o = [((x - left) / w) as f32, ((y - top) / h) as f32];
                }
                Some(out)
            }
            _ => None,
        };
        let defaults = TextWarp::default();
        let warp = TextWarp {
            style,
            bend: frac(self.value, defaults.bend),
            horizontal: frac(self.perspective, 0.0),
            vertical: frac(self.perspective_other, 0.0),
            mesh,
        };
        (warp, unmapped)
    }
}

/// The `TySh` warp descriptor as a [`WarpSpec`], read by a bounded reader of
/// its own (the Custom mesh is an `ObAr` of `UnFl` values, which the generic
/// [`Descriptor`] reader does not model). `None` when the block has no
/// readable warp descriptor — nothing to read, or a malformed one.
pub fn warp_spec(raw: &[u8], opts: &ReadOptions) -> Option<WarpSpec> {
    let mut cur = Cursor::new(raw);
    cur.u16().ok()?;
    for _ in 0..6 {
        cur.f64().ok()?;
    }
    cur.u16().ok()?;
    cur.u32().ok()?;
    Descriptor::read(&mut cur, opts).ok()?;
    cur.u16().ok()?;
    cur.u32().ok()?;
    let items = wd::read_object(&mut cur, opts, 0)?;
    let get = |k: &str| items.iter().find(|(key, _)| key == k).map(|(_, v)| v);
    let num = |k: &str| match get(k) {
        Some(wd::V::Num(v)) if v.is_finite() => *v,
        _ => 0.0,
    };
    let kind = match get("warpStyle") {
        Some(wd::V::Enum(id)) => WarpKind::from_id(id)?,
        _ => WarpKind::None,
    };
    let vertical = matches!(get("warpRotate"), Some(wd::V::Enum(v)) if v == "Vrtc");
    let mut bounds = [0.0f64; 4];
    if let Some(wd::V::Obj(r)) = get("bounds") {
        for (slot, key) in bounds.iter_mut().zip(["Top ", "Left", "Btom", "Rght"]) {
            if let Some((_, wd::V::Num(v))) = r.iter().find(|(k, _)| k == key) {
                if v.is_finite() {
                    *slot = *v;
                }
            }
        }
    }
    let mesh = match get("customEnvelopeWarp") {
        Some(wd::V::Obj(env)) => env
            .iter()
            .find(|(k, _)| k == "meshPoints")
            .and_then(|(_, v)| wd::mesh_of(v)),
        _ => None,
    };
    Some(WarpSpec {
        kind,
        value: num("warpValue"),
        perspective: num("warpPerspective"),
        perspective_other: num("warpPerspectiveOther"),
        vertical,
        bounds,
        mesh,
    })
}

/// Write `spec` as the warp descriptor, in Photoshop's layout: the five keys
/// every warp carries, and for Custom the `bounds` rectangle, the mesh order
/// and `customEnvelopeWarp` with its `meshPoints` (an `ObAr` of one
/// `rationalPoint` holding `Hrzn` and `Vrtc` as `UnFl` pixel lists).
pub fn write_warp(s: &mut Sink, spec: &WarpSpec) {
    let custom = spec.kind == WarpKind::Custom;
    s.unicode_string("");
    wd::key(s, "warp");
    s.u32(if custom { 9 } else { 5 });
    wd::put_enum(s, "warpStyle", "warpStyle", spec.kind.id());
    wd::put_doub(s, "warpValue", spec.value);
    wd::put_doub(s, "warpPerspective", spec.perspective);
    wd::put_doub(s, "warpPerspectiveOther", spec.perspective_other);
    let orient = if spec.vertical { "Vrtc" } else { "Hrzn" };
    wd::put_enum(s, "warpRotate", "Ornt", orient);
    if !custom {
        return;
    }
    wd::key(s, "bounds");
    s.tag(b"Objc");
    s.unicode_string("");
    wd::key(s, "Rctn");
    s.u32(4);
    for (key, v) in ["Top ", "Left", "Btom", "Rght"]
        .into_iter()
        .zip(spec.bounds)
    {
        wd::key(s, key);
        s.tag(b"UntF");
        s.tag(b"#Pxl");
        s.f64(v);
    }
    for key in ["uOrder", "vOrder"] {
        wd::key(s, key);
        s.tag(b"long");
        s.i32(4);
    }
    let mesh = spec.mesh.unwrap_or_else(|| {
        let [top, left, bottom, right] = spec.bounds;
        let mut m = [[0.0f64; 2]; 16];
        for (i, p) in m.iter_mut().enumerate() {
            *p = [
                left + (right - left) * (i % 4) as f64 / 3.0,
                top + (bottom - top) * (i / 4) as f64 / 3.0,
            ];
        }
        m
    });
    wd::key(s, "customEnvelopeWarp");
    s.tag(b"Objc");
    s.unicode_string("");
    wd::key(s, "customEnvelopeWarp");
    s.u32(1);
    wd::key(s, "meshPoints");
    s.tag(b"ObAr");
    s.u32(16);
    s.unicode_string("");
    wd::key(s, "rationalPoint");
    s.u32(2);
    for (key, axis) in [("Hrzn", 0), ("Vrtc", 1)] {
        wd::key(s, key);
        s.tag(b"UnFl");
        s.tag(b"#Pxl");
        s.u32(16);
        for p in &mesh {
            s.f64(p[axis]);
        }
    }
}

/// W15-C: the warp descriptor's own writer helpers and bounded reader.
mod wd {
    use crate::bytes::{Cursor, Sink};
    use crate::limits::ReadOptions;

    pub(super) fn key(s: &mut Sink, k: &str) {
        s.u32(if k.len() == 4 { 0 } else { k.len() as u32 });
        s.bytes(k.as_bytes());
    }

    pub(super) fn put_enum(s: &mut Sink, k: &str, type_id: &str, value: &str) {
        key(s, k);
        s.tag(b"enum");
        key(s, type_id);
        key(s, value);
    }

    pub(super) fn put_doub(s: &mut Sink, k: &str, v: f64) {
        key(s, k);
        s.tag(b"doub");
        s.f64(if v.is_finite() { v } else { 0.0 });
    }

    /// The values the warp descriptor holds; anything else is skipped.
    #[derive(Debug)]
    pub(super) enum V {
        Num(f64),
        Enum(String),
        Obj(Vec<(String, V)>),
        /// An `ObAr`: its keyed `UnFl` lists.
        Array(Vec<(String, Vec<f64>)>),
        Other,
    }

    fn read_key(cur: &mut Cursor<'_>, opts: &ReadOptions) -> Option<String> {
        let len = cur.u32().ok()?;
        let n = if len == 0 { 4 } else { len as usize };
        if n > opts.max_name_units {
            return None;
        }
        Some(cur.take(n).ok()?.iter().map(|&b| b as char).collect())
    }

    fn count(cur: &mut Cursor<'_>, opts: &ReadOptions, each: usize) -> Option<usize> {
        let n = cur.u32().ok()? as usize;
        (n <= opts.max_descriptor_items && n.saturating_mul(each) <= cur.remaining()).then_some(n)
    }

    /// A descriptor body (name, class, items), bounded in depth and count.
    pub(super) fn read_object(
        cur: &mut Cursor<'_>,
        opts: &ReadOptions,
        depth: usize,
    ) -> Option<Vec<(String, V)>> {
        if depth > opts.max_descriptor_depth {
            return None;
        }
        cur.unicode_string(opts.max_name_units).ok()?;
        read_key(cur, opts)?;
        let n = count(cur, opts, 8)?;
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            let k = read_key(cur, opts)?;
            let v = read_value(cur, opts, depth + 1)?;
            items.push((k, v));
        }
        Some(items)
    }

    fn read_value(cur: &mut Cursor<'_>, opts: &ReadOptions, depth: usize) -> Option<V> {
        if depth > opts.max_descriptor_depth {
            return None;
        }
        Some(match &cur.tag().ok()? {
            b"Objc" | b"GlbO" => V::Obj(read_object(cur, opts, depth)?),
            b"VlLs" => {
                let n = count(cur, opts, 4)?;
                for _ in 0..n {
                    read_value(cur, opts, depth + 1)?;
                }
                V::Other
            }
            b"doub" => V::Num(cur.f64().ok()?),
            b"UntF" => {
                cur.tag().ok()?;
                V::Num(cur.f64().ok()?)
            }
            b"long" => V::Num(f64::from(cur.i32().ok()?)),
            b"comp" => {
                cur.skip(8).ok()?;
                V::Other
            }
            b"bool" => {
                cur.skip(1).ok()?;
                V::Other
            }
            b"enum" => {
                read_key(cur, opts)?;
                V::Enum(read_key(cur, opts)?)
            }
            b"TEXT" => {
                cur.unicode_string(opts.max_tagged_block_bytes / 2 + 1)
                    .ok()?;
                V::Other
            }
            b"type" | b"GlbC" => {
                cur.unicode_string(opts.max_name_units).ok()?;
                read_key(cur, opts)?;
                V::Other
            }
            b"alis" | b"tdta" => {
                let len = cur.u32().ok()? as usize;
                cur.skip(len).ok()?;
                V::Other
            }
            b"ObAr" => {
                let items = cur.u32().ok()? as usize;
                if items > opts.max_descriptor_items {
                    return None;
                }
                cur.unicode_string(opts.max_name_units).ok()?;
                read_key(cur, opts)?;
                let n = count(cur, opts, 8)?;
                let mut lists = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = read_key(cur, opts)?;
                    if &cur.tag().ok()? != b"UnFl" {
                        return None;
                    }
                    cur.tag().ok()?;
                    let m = count(cur, opts, 8)?;
                    let mut vals = Vec::with_capacity(m);
                    for _ in 0..m {
                        vals.push(cur.f64().ok()?);
                    }
                    lists.push((k, vals));
                }
                V::Array(lists)
            }
            _ => return None,
        })
    }

    /// The sixteen mesh points of a `meshPoints` array, when it holds exactly
    /// a 4x4 mesh of finite values.
    pub(super) fn mesh_of(v: &V) -> Option<[[f64; 2]; 16]> {
        let V::Array(lists) = v else {
            return None;
        };
        let axis = |k: &str| {
            lists
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_slice())
                .filter(|v| v.len() == 16 && v.iter().all(|x| x.is_finite()))
        };
        let (h, w) = (axis("Hrzn")?, axis("Vrtc")?);
        let mut out = [[0.0f64; 2]; 16];
        for (i, p) in out.iter_mut().enumerate() {
            *p = [h[i], w[i]];
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Sink;
    use crate::descriptor::Value;

    /// Build a `TySh` payload shaped exactly like the one Photoshop writes.
    fn fixture(text: &str, transform: [f64; 6]) -> Vec<u8> {
        let mut s = Sink::new();
        s.u16(1);
        for v in transform {
            s.f64(v);
        }
        s.u16(50);
        s.u32(16);
        let mut d = Descriptor::new("TxLr");
        d.push("Txt ", Value::from(text)).unwrap();
        d.push(
            "textGridding",
            Value::Enumerated {
                type_id: "textGridding".into(),
                value: "None".into(),
            },
        )
        .unwrap();
        d.push("EngineData", Value::RawData(b"<< /EngineDict >>".to_vec()))
            .unwrap();
        d.write(&mut s).unwrap();
        s.u16(1);
        s.u32(16);
        let mut warp = Descriptor::new("warp");
        warp.push(
            "warpStyle",
            Value::Enumerated {
                type_id: "warpStyle".into(),
                value: "warpNone".into(),
            },
        )
        .unwrap();
        warp.write(&mut s).unwrap();
        s.i32(0);
        s.i32(0);
        s.i32(200);
        s.i32(40);
        s.into_inner()
    }

    #[test]
    fn the_string_and_the_transform_come_back_out() {
        let t = [1.0, 0.0, 0.0, 1.0, 24.5, -8.0];
        let raw = fixture("Hello, world", t);
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text.as_deref(), Some("Hello, world"));
        assert_eq!(parsed.transform, t);
        assert_eq!(parsed.raw, raw, "the block is preserved verbatim");
    }

    #[test]
    fn non_ascii_text_survives_the_utf16_round_trip() {
        let raw = fixture("Ελλάδα — 日本語 🎨", IDENTITY);
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text.as_deref(), Some("Ελλάδα — 日本語 🎨"));
    }

    #[test]
    fn the_warp_descriptor_is_reachable_after_the_text_descriptor() {
        let raw = fixture("x", IDENTITY);
        let w = warp(&raw, &ReadOptions::default()).unwrap();
        assert_eq!(w.class_id, "warp");
        assert_eq!(
            w.get("warpStyle"),
            Some(&Value::Enumerated {
                type_id: "warpStyle".into(),
                value: "warpNone".into()
            })
        );
    }

    #[test]
    fn a_truncated_block_yields_defaults_and_keeps_its_bytes() {
        let raw = fixture("Hello", IDENTITY);
        for cut in 0..raw.len() {
            let parsed = parse(&raw[..cut], &ReadOptions::default());
            assert_eq!(parsed.raw, raw[..cut].to_vec());
            if cut < 2 {
                assert_eq!(parsed.transform, IDENTITY);
            }
            // Never a panic, and never a wrong string.
            if let Some(t) = &parsed.text {
                assert_eq!(t, "Hello");
            }
        }
    }

    #[test]
    fn garbage_bytes_parse_to_defaults_rather_than_panicking() {
        let junk: Vec<u8> = (0..512u32).map(|i| (i * 97 % 256) as u8).collect();
        let parsed = parse(&junk, &ReadOptions::default());
        assert_eq!(parsed.raw, junk);
        // Whether a warp descriptor happens to parse out of noise is not the
        // point; that the call returns at all is.
        let _ = warp(&junk, &ReadOptions::default());
    }

    #[test]
    fn a_descriptor_without_a_text_key_reports_no_text() {
        let mut s = Sink::new();
        s.u16(1);
        for v in IDENTITY {
            s.f64(v);
        }
        s.u16(50);
        s.u32(16);
        Descriptor::new("TxLr").write(&mut s).unwrap();
        let raw = s.into_inner();
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text, None);
        assert_eq!(parsed.transform, IDENTITY);
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;

    /// Card 079: the synthesized block round-trips through the parser — the
    /// string, the transform and the bounds come back exactly, and the
    /// engine data names the font and covers every character.
    #[test]
    fn the_built_type_block_round_trips_through_the_parser() {
        let t = [1.0, 0.0, 0.0, 1.0, 12.0, 34.0];
        let raw = build("SOLD TODAY", t, (5, 6, 105, 46), "Montserrat", 24.0);
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text.as_deref(), Some("SOLD TODAY"));
        assert_eq!(parsed.transform, t);
        assert!(String::from_utf8_lossy(&parsed.raw).contains("/Name (Montserrat)"));
        assert!(String::from_utf8_lossy(&parsed.raw).contains("/FontSize 24"));
        assert!(String::from_utf8_lossy(&parsed.raw).contains("/RunLength 10"));
        // A reader that re-typesets needs every character covered: the run
        // lengths cover the string plus the engine's trailing ``, and the
        // engine data's own text is the string.
        let engine = String::from_utf8_lossy(&parsed.raw);
        assert_eq!(engine.matches("/RunLengthArray [ 11 ]").count(), 2);
        let e = engine_text(&raw, &ReadOptions::default())
            .unwrap()
            .expect("the engine data reads back");
        assert_eq!(e.text, "SOLD TODAY");
        assert_eq!(e.style_runs.len(), 1);
        assert_eq!(e.style_runs[0].length, 11);
        assert_eq!(e.style_runs[0].style.font.as_deref(), Some("Montserrat"));
        assert_eq!(e.style_runs[0].style.size, Some(24.0));
        assert_eq!(e.style_runs[0].style.fill, Some([0.0, 0.0, 0.0, 1.0]));
    }

    /// W9-C: a styled layer's runs travel through the block — two fonts, two
    /// sizes, two fills — and [`engine_text`] reads them back per run.
    #[test]
    fn a_styled_block_carries_every_run() {
        use crate::engine_data::{CharStyle, StyleRun};
        let engine = EngineText {
            text: "Red blue".into(),
            style_runs: vec![
                StyleRun {
                    length: 4,
                    style: CharStyle {
                        font: Some("Montserrat".into()),
                        size: Some(30.0),
                        fill: Some([1.0, 0.0, 0.0, 1.0]),
                        ..CharStyle::default()
                    },
                },
                StyleRun {
                    length: 4,
                    style: CharStyle {
                        font: Some("DejaVu Serif".into()),
                        size: Some(12.5),
                        fill: Some([0.0, 0.0, 1.0, 1.0]),
                        ..CharStyle::default()
                    },
                },
            ],
            ..EngineText::default()
        };
        let raw = build_styled(&engine, IDENTITY, (0, 0, 10, 10));
        assert_eq!(
            parse(&raw, &ReadOptions::default()).text.as_deref(),
            Some("Red blue")
        );
        let back = engine_text(&raw, &ReadOptions::default()).unwrap().unwrap();
        assert_eq!(back.text, "Red blue");
        let fonts: Vec<_> = back
            .style_runs
            .iter()
            .map(|r| (r.length, r.style.font.clone(), r.style.size, r.style.fill))
            .collect();
        assert_eq!(
            fonts,
            vec![
                (
                    4,
                    Some("Montserrat".into()),
                    Some(30.0),
                    Some([1.0, 0.0, 0.0, 1.0])
                ),
                (
                    5,
                    Some("DejaVu Serif".into()),
                    Some(12.5),
                    Some([0.0, 0.0, 1.0, 1.0])
                ),
            ],
            "the last run also covers the engine's trailing \r"
        );
    }

    /// A block whose engine data is malformed reports an error from
    /// [`engine_text`] while [`parse`] still returns the string.
    #[test]
    fn malformed_engine_data_is_an_error_not_a_panic() {
        let mut s = Sink::new();
        s.u16(1);
        for v in IDENTITY {
            s.f64(v);
        }
        s.u16(50);
        s.u32(16);
        let mut d = Descriptor::new("TxLr");
        d.push("Txt ", Value::from("x")).unwrap();
        d.push(
            "EngineData",
            Value::RawData(b"<< /EngineDict << /Editor (".to_vec()),
        )
        .unwrap();
        d.write(&mut s).unwrap();
        let raw = s.into_inner();
        assert_eq!(
            parse(&raw, &ReadOptions::default()).text.as_deref(),
            Some("x")
        );
        assert!(engine_text(&raw, &ReadOptions::default()).is_err());
    }

    /// Non-ASCII survives: the engine data escapes the bytes, the descriptor
    /// carries UTF-16.
    #[test]
    fn the_built_block_survives_non_ascii_text() {
        let raw = build("Ελλάδα", IDENTITY, (0, 0, 10, 10), "Sans", 12.0);
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text.as_deref(), Some("Ελλάδα"));
        let engine = String::from_utf8_lossy(&parsed.raw);
        assert!(engine.contains("/RunLength 6"));
    }
}

#[cfg(test)]
mod warp_tests {
    use super::*;
    use layer_model::text::{TextWarp, WarpStyle};

    fn block(spec: &WarpSpec) -> Vec<u8> {
        let engine = EngineText {
            text: "Warp".into(),
            ..EngineText::default()
        };
        build_styled_warped(&engine, IDENTITY, (0, 0, 10, 10), spec)
    }

    fn mesh() -> [[f64; 2]; 16] {
        let mut m = [[0.0f64; 2]; 16];
        for (i, p) in m.iter_mut().enumerate() {
            let (c, r) = ((i % 4) as f64, (i / 4) as f64);
            *p = [
                -3.0 + c * 41.5 + r * 2.25,
                -30.0 + r * 12.0 + (c * 1.7).sin() * 9.0,
            ];
        }
        m
    }

    /// W15-C: every `warpStyle` — the sixteen styles and warpNone — goes out
    /// and comes back with its bend, both distortions, its orientation and
    /// (Custom) its bounds and mesh equal.
    #[test]
    fn every_warp_style_and_a_custom_mesh_round_trip() {
        for (i, kind) in WarpKind::ALL.into_iter().enumerate() {
            let custom = kind == WarpKind::Custom;
            let spec = WarpSpec {
                kind,
                value: -100.0 + 12.5 * i as f64,
                perspective: 7.0 - i as f64,
                perspective_other: -33.0 + 2.0 * i as f64,
                vertical: i % 2 == 1,
                bounds: if custom {
                    [-30.0, -3.0, 6.0, 121.5]
                } else {
                    [0.0; 4]
                },
                mesh: custom.then(mesh),
            };
            let raw = block(&spec);
            let back = warp_spec(&raw, &ReadOptions::default()).expect("the warp reads back");
            assert_eq!(back, spec, "{}", kind.id());
            // The block around it is intact: the string still parses.
            assert_eq!(
                parse(&raw, &ReadOptions::default()).text.as_deref(),
                Some("Warp")
            );
        }
    }

    /// The style ids are Photoshop's spelling and a parametric warp stays a
    /// descriptor the generic reader takes (the five keys Photoshop writes).
    #[test]
    fn a_parametric_warp_is_photoshops_five_key_descriptor() {
        let spec = WarpSpec {
            kind: WarpKind::Flag,
            value: 40.0,
            ..WarpSpec::NONE
        };
        let d = warp(&block(&spec), &ReadOptions::default()).expect("generic read");
        assert_eq!(d.class_id, "warp");
        assert_eq!(
            d.get("warpStyle"),
            Some(&Value::Enumerated {
                type_id: "warpStyle".into(),
                value: "warpFlag".into()
            })
        );
        assert_eq!(d.get("warpValue"), Some(&Value::Double(40.0)));
        assert_eq!(
            d.get("warpRotate"),
            Some(&Value::Enumerated {
                type_id: "Ornt".into(),
                value: "Hrzn".into()
            })
        );
        assert_eq!(
            WarpKind::from_id("warpShellLower"),
            Some(WarpKind::ShellLower)
        );
        assert_eq!(WarpKind::from_id("warpNope"), None);
    }

    /// Every layer-model style (Custom with a dragged mesh) maps to the
    /// descriptor and back to an equal `TextWarp`.
    #[test]
    fn every_layer_warp_maps_through_the_descriptor_and_back() {
        let mut dragged = [[0.0f32; 2]; 16];
        for (i, p) in dragged.iter_mut().enumerate() {
            *p = [(i % 4) as f32 / 3.0 + 0.125, (i / 4) as f32 / 3.0 - 0.25];
        }
        let styles = std::iter::once(WarpStyle::Custom).chain(WarpStyle::ALL);
        for style in styles {
            let warp = TextWarp {
                style,
                bend: -0.375,
                horizontal: 0.25,
                vertical: -0.5,
                mesh: (style == WarpStyle::Custom).then_some(dragged),
            };
            let bounds = [-24.0, 0.0, 8.0, 128.0];
            let raw = block(&WarpSpec::from_text_warp(&warp, bounds));
            let spec = warp_spec(&raw, &ReadOptions::default()).unwrap();
            let (back, unmapped) = spec.to_text_warp();
            assert!(unmapped.is_empty(), "{style:?}: {unmapped:?}");
            assert_eq!(back, warp, "{style:?}");
        }
        // No warp stays the default value (serialises as before).
        let raw = block(&WarpSpec::from_text_warp(&TextWarp::default(), [0.0; 4]));
        let (back, _) = warp_spec(&raw, &ReadOptions::default())
            .unwrap()
            .to_text_warp();
        assert!(back.is_default());
    }

    /// What the layer cannot hold is named: Shell as Arc, Vertical as
    /// Horizontal.
    #[test]
    fn shell_styles_and_vertical_orientation_are_reported() {
        let spec = WarpSpec {
            kind: WarpKind::ShellLower,
            value: 50.0,
            vertical: true,
            ..WarpSpec::NONE
        };
        let (w, unmapped) = spec.to_text_warp();
        assert_eq!(w.style, WarpStyle::ArcLower);
        assert_eq!(unmapped.len(), 2, "{unmapped:?}");
    }

    /// Truncated or noisy warp bytes never panic, and never invent a warp
    /// from a cut mesh.
    #[test]
    fn a_truncated_warp_is_none_never_a_panic() {
        let spec = WarpSpec {
            kind: WarpKind::Custom,
            value: 10.0,
            bounds: [0.0, 0.0, 10.0, 10.0],
            mesh: Some(mesh()),
            ..WarpSpec::NONE
        };
        let raw = block(&spec);
        // The trailing rectangle is not part of the descriptor.
        let end = raw.len() - 16;
        for cut in 0..end {
            if let Some(w) = warp_spec(&raw[..cut], &ReadOptions::default()) {
                panic!("a cut at {cut} read {w:?}");
            }
        }
        let junk: Vec<u8> = (0..2048u32).map(|i| (i * 131 % 251) as u8).collect();
        let _ = warp_spec(&junk, &ReadOptions::default());
    }
}
