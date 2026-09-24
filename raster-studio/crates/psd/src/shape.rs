//! W9-M: vector shape layers — the path (`vmsk`/`vsms`), the fill (`SoCo`
//! or `vscg`), the stroke (`vstk`) and the live-shape origination (`vogk`).
//!
//! A shape layer is a fill layer clipped by a vector mask. Photoshop writes
//! the path as a run of 26-byte path records in a `vmsk` (or, since CS6,
//! `vsms`) block, the fill as a solid-colour (`SoCo`) fill-layer block or a
//! `vscg` content block, and the outline as a `strokeStyle` descriptor in
//! `vstk`. A reader that understands none of these still sees the layer's
//! channels, which carry the rendered appearance.
//!
//! # Path records (as implemented)
//!
//! `vmsk`/`vsms`: a `u32` version (3), `u32` flags (bit 0 invert, bit 1 not
//! linked, bit 2 disabled), then 26-byte records, each a `u16` selector and 24
//! bytes:
//!
//! | selector | record |
//! |---|---|
//! | 6 | path fill rule (24 zero bytes) |
//! | 8 | initial fill rule (`u16`, 22 zero bytes) |
//! | 0 / 3 | closed / open subpath length: `u16` knot count, `i16` operation (0 xor, 1 union, 2 subtract, 3 intersect), 20 more bytes |
//! | 1, 2 / 4, 5 | closed / open knot, linked / unlinked: three points — preceding control, anchor, leaving control — each `y, x` as signed 8.24 fixed-point fractions of the canvas height and width |
//! | 7 | clipboard record (ignored) |
//!
//! # Untrusted input
//!
//! Records are read through a [`Cursor`], so a truncated block ends the path
//! where it runs out; knot counts are bounded by the bytes actually present
//! before anything is reserved ([`MAX_PATH_KNOTS`] on top), and a knot outside
//! a subpath is ignored rather than trusted.

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::limits::ReadOptions;
use crate::model::{PsdLayer, TaggedBlock};

/// The most knots one path may carry; the rest are dropped.
pub const MAX_PATH_KNOTS: usize = 1 << 20;

/// One knot in document pixels: preceding control, anchor, leaving control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Knot {
    pub before: [f64; 2],
    pub anchor: [f64; 2],
    pub after: [f64; 2],
    /// The two handles move together (a "smooth" point).
    pub linked: bool,
}

/// One subpath.
#[derive(Debug, Clone, PartialEq)]
pub struct SubPath {
    pub closed: bool,
    /// 0 xor, 1 union, 2 subtract, 3 intersect — how this subpath combines
    /// with those before it.
    pub operation: i16,
    pub knots: Vec<Knot>,
}

/// A vector path in document pixels, plus the mask flags.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VectorPath {
    pub subpaths: Vec<SubPath>,
    pub invert: bool,
    pub not_linked: bool,
    pub disabled: bool,
}

const FIXED_ONE: f64 = 16_777_216.0; // 1 << 24

fn fixed(v: f64, extent: f64) -> i32 {
    let f = if extent > 0.0 { v / extent } else { 0.0 };
    (f * FIXED_ONE)
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

fn unfixed(v: i32, extent: f64) -> f64 {
    f64::from(v) / FIXED_ONE * extent
}

impl VectorPath {
    /// The `vmsk`/`vsms` payload for a `width` x `height` canvas.
    pub fn encode(&self, width: u32, height: u32) -> Vec<u8> {
        let (w, h) = (f64::from(width), f64::from(height));
        let mut s = Sink::new();
        s.u32(3);
        s.u32(
            u32::from(self.invert)
                | (u32::from(self.not_linked) << 1)
                | (u32::from(self.disabled) << 2),
        );
        // Path fill rule record.
        s.u16(6);
        s.zeros(24);
        // Initial fill rule: 0 (the path does not start filled).
        s.u16(8);
        s.u16(0);
        s.zeros(22);
        for sp in &self.subpaths {
            s.u16(if sp.closed { 0 } else { 3 });
            s.u16(sp.knots.len().min(usize::from(u16::MAX)) as u16);
            s.i16(sp.operation);
            s.u16(1);
            s.u32(0);
            s.u32(0);
            s.zeros(10);
            for k in sp.knots.iter().take(usize::from(u16::MAX)) {
                let selector = match (sp.closed, k.linked) {
                    (true, true) => 1,
                    (true, false) => 2,
                    (false, true) => 4,
                    (false, false) => 5,
                };
                s.u16(selector);
                for p in [k.before, k.anchor, k.after] {
                    s.i32(fixed(p[1], h));
                    s.i32(fixed(p[0], w));
                }
            }
        }
        s.into_inner()
    }

    /// Parse a `vmsk`/`vsms` payload for a `width` x `height` canvas.
    pub fn decode(data: &[u8], width: u32, height: u32) -> Option<Self> {
        let (w, h) = (f64::from(width), f64::from(height));
        let mut cur = Cursor::new(data);
        let _version = cur.u32().ok()?;
        let flags = cur.u32().ok()?;
        let mut path = VectorPath {
            subpaths: Vec::new(),
            invert: flags & 1 != 0,
            not_linked: flags & 2 != 0,
            disabled: flags & 4 != 0,
        };
        let mut knots = 0usize;
        while cur.remaining() >= 26 {
            let selector = cur.u16().ok()?;
            let mut rec = cur.sub(24).ok()?;
            match selector {
                0 | 3 => {
                    let count = usize::from(rec.u16().ok()?);
                    let operation = rec.i16().ok()?;
                    // The count is a claim; the bytes after it are the truth.
                    let fits = (cur.remaining() / 26).min(count);
                    path.subpaths.push(SubPath {
                        closed: selector == 0,
                        operation,
                        knots: Vec::with_capacity(fits),
                    });
                }
                1 | 2 | 4 | 5 => {
                    if knots >= MAX_PATH_KNOTS {
                        continue;
                    }
                    let Some(sp) = path.subpaths.last_mut() else {
                        continue;
                    };
                    let mut pts = [[0.0f64; 2]; 3];
                    for p in &mut pts {
                        let y = rec.i32().ok()?;
                        let x = rec.i32().ok()?;
                        *p = [unfixed(x, w), unfixed(y, h)];
                    }
                    knots += 1;
                    sp.knots.push(Knot {
                        before: pts[0],
                        anchor: pts[1],
                        after: pts[2],
                        linked: selector == 1 || selector == 4,
                    });
                }
                _ => {}
            }
        }
        Some(path)
    }
}

/// How a shape's outline ends and joins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// Where the outline sits relative to the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineAlign {
    Inside,
    #[default]
    Center,
    Outside,
}

/// A shape's `vstk` stroke style.
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeStyle {
    pub stroke_enabled: bool,
    pub fill_enabled: bool,
    pub width_px: f64,
    /// 0..=255 per channel.
    pub color: [f64; 3],
    /// 0..=1.
    pub opacity: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub align: LineAlign,
    pub miter_limit: f64,
    /// Dash lengths in multiples of the stroke width (the format's unit).
    pub dash: Vec<f64>,
    pub dash_offset: f64,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        StrokeStyle {
            stroke_enabled: false,
            fill_enabled: true,
            width_px: 1.0,
            color: [0.0; 3],
            opacity: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            align: LineAlign::Center,
            miter_limit: 4.0,
            dash: Vec::new(),
            dash_offset: 0.0,
        }
    }
}

fn rgbc(color: [f64; 3]) -> Value {
    let mut c = Descriptor::new("RGBC");
    let _ = c.push("Rd  ", Value::Double(color[0]));
    let _ = c.push("Grn ", Value::Double(color[1]));
    let _ = c.push("Bl  ", Value::Double(color[2]));
    Value::Descriptor(c)
}

fn rgb_of(d: &Descriptor) -> Option<[f64; 3]> {
    let c = d.descriptor("Clr ")?;
    let v = [c.number("Rd  ")?, c.number("Grn ")?, c.number("Bl  ")?];
    v.iter().all(|x| x.is_finite()).then_some(v)
}

fn enumerated(type_id: &str, value: &str) -> Value {
    Value::Enumerated {
        type_id: type_id.into(),
        value: value.into(),
    }
}

fn enum_value<'a>(d: &'a Descriptor, key: &str) -> Option<&'a str> {
    match d.get(key)? {
        Value::Enumerated { value, .. } => Some(value),
        _ => None,
    }
}

fn bool_value(d: &Descriptor, key: &str) -> Option<bool> {
    match d.get(key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

impl StrokeStyle {
    /// The `vstk` payload: a descriptor version (16) and a `strokeStyle`
    /// descriptor.
    pub fn encode(&self) -> Vec<u8> {
        let mut d = Descriptor::new("strokeStyle");
        let mut push = |k: &str, v: Value| {
            let _ = d.push(k, v);
        };
        push("strokeStyleVersion", Value::Integer(2));
        push("strokeEnabled", Value::Bool(self.stroke_enabled));
        push("fillEnabled", Value::Bool(self.fill_enabled));
        push(
            "strokeStyleLineWidth",
            Value::UnitFloat {
                unit: *b"#Pxl",
                value: self.width_px,
            },
        );
        push(
            "strokeStyleLineDashOffset",
            Value::UnitFloat {
                unit: *b"#Pnt",
                value: self.dash_offset,
            },
        );
        push("strokeStyleMiterLimit", Value::Double(self.miter_limit));
        push(
            "strokeStyleLineCapType",
            enumerated(
                "strokeStyleLineCapType",
                match self.cap {
                    LineCap::Butt => "strokeStyleButtCap",
                    LineCap::Round => "strokeStyleRoundCap",
                    LineCap::Square => "strokeStyleSquareCap",
                },
            ),
        );
        push(
            "strokeStyleLineJoinType",
            enumerated(
                "strokeStyleLineJoinType",
                match self.join {
                    LineJoin::Miter => "strokeStyleMiterJoin",
                    LineJoin::Round => "strokeStyleRoundJoin",
                    LineJoin::Bevel => "strokeStyleBevelJoin",
                },
            ),
        );
        push(
            "strokeStyleLineAlignment",
            enumerated(
                "strokeStyleLineAlignment",
                match self.align {
                    LineAlign::Inside => "strokeStyleAlignInside",
                    LineAlign::Center => "strokeStyleAlignCenter",
                    LineAlign::Outside => "strokeStyleAlignOutside",
                },
            ),
        );
        push("strokeStyleScaleLock", Value::Bool(false));
        push("strokeStyleStrokeAdjust", Value::Bool(false));
        push(
            "strokeStyleLineDashSet",
            Value::List(
                self.dash
                    .iter()
                    .map(|v| Value::UnitFloat {
                        unit: *b"#Nne",
                        value: *v,
                    })
                    .collect(),
            ),
        );
        push("strokeStyleBlendMode", enumerated("BlnM", "Nrml"));
        push(
            "strokeStyleOpacity",
            Value::UnitFloat {
                unit: *b"#Prc",
                value: self.opacity * 100.0,
            },
        );
        let mut content = Descriptor::new("solidColorLayer");
        let _ = content.push("Clr ", rgbc(self.color));
        push("strokeStyleContent", Value::Descriptor(content));
        push("strokeStyleResolution", Value::Double(72.0));
        let mut s = Sink::new();
        s.u32(16);
        let _ = d.write(&mut s);
        s.into_inner()
    }

    /// Parse a `vstk` payload. Fields the block leaves out keep their
    /// [`StrokeStyle::default`] values.
    pub fn decode(data: &[u8], opts: &ReadOptions) -> Option<Self> {
        let mut cur = Cursor::new(data);
        let _version = cur.u32().ok()?;
        let d = Descriptor::read(&mut cur, opts).ok()?;
        let mut s = StrokeStyle::default();
        if let Some(b) = bool_value(&d, "strokeEnabled") {
            s.stroke_enabled = b;
        }
        if let Some(b) = bool_value(&d, "fillEnabled") {
            s.fill_enabled = b;
        }
        if let Some(v) = d.number("strokeStyleLineWidth").filter(|v| v.is_finite()) {
            s.width_px = v.max(0.0);
        }
        if let Some(v) = d
            .number("strokeStyleLineDashOffset")
            .filter(|v| v.is_finite())
        {
            s.dash_offset = v;
        }
        if let Some(v) = d.number("strokeStyleMiterLimit").filter(|v| v.is_finite()) {
            s.miter_limit = v;
        }
        if let Some(v) = d.number("strokeStyleOpacity").filter(|v| v.is_finite()) {
            s.opacity = (v / 100.0).clamp(0.0, 1.0);
        }
        s.cap = match enum_value(&d, "strokeStyleLineCapType") {
            Some("strokeStyleRoundCap") => LineCap::Round,
            Some("strokeStyleSquareCap") => LineCap::Square,
            _ => LineCap::Butt,
        };
        s.join = match enum_value(&d, "strokeStyleLineJoinType") {
            Some("strokeStyleRoundJoin") => LineJoin::Round,
            Some("strokeStyleBevelJoin") => LineJoin::Bevel,
            _ => LineJoin::Miter,
        };
        s.align = match enum_value(&d, "strokeStyleLineAlignment") {
            Some("strokeStyleAlignInside") => LineAlign::Inside,
            Some("strokeStyleAlignOutside") => LineAlign::Outside,
            _ => LineAlign::Center,
        };
        if let Some(Value::List(items)) = d.get("strokeStyleLineDashSet") {
            s.dash = items
                .iter()
                .filter_map(|v| match v {
                    Value::UnitFloat { value, .. } | Value::Double(value) => Some(*value),
                    _ => None,
                })
                .filter(|v| v.is_finite())
                .take(64)
                .collect();
        }
        if let Some(c) = d.descriptor("strokeStyleContent").and_then(rgb_of) {
            s.color = c;
        }
        Some(s)
    }
}

/// A solid-colour fill layer's `SoCo` payload: 0..=255 RGB.
pub fn encode_solid_color(rgb: [f64; 3]) -> Vec<u8> {
    let mut d = Descriptor::new("null");
    let _ = d.push("Clr ", rgbc(rgb));
    let mut s = Sink::new();
    s.u32(16);
    let _ = d.write(&mut s);
    s.into_inner()
}

/// The fill colour of a `vscg` content block when it is a solid colour.
fn vscg_color(data: &[u8], opts: &ReadOptions) -> Option<[f64; 3]> {
    let mut cur = Cursor::new(data);
    let key = cur.tag().ok()?;
    if key != *b"SoCo" {
        return None;
    }
    let _version = cur.u32().ok()?;
    let d = Descriptor::read(&mut cur, opts).ok()?;
    rgb_of(&d)
}

/// The `vogk` origination block for an axis-aligned rectangle: a live
/// rectangle (`keyOriginType` 1) with its box in canvas pixels.
pub fn encode_rect_origination(left: f64, top: f64, right: f64, bottom: f64) -> Vec<u8> {
    let px = |v: f64| Value::UnitFloat {
        unit: *b"#Pxl",
        value: v,
    };
    let mut bbox = Descriptor::new("unitRect");
    let _ = bbox.push("unitValueQuadVersion", Value::Integer(1));
    let _ = bbox.push("Top ", px(top));
    let _ = bbox.push("Left", px(left));
    let _ = bbox.push("Btom", px(bottom));
    let _ = bbox.push("Rght", px(right));
    let mut key = Descriptor::new("null");
    let _ = key.push("keyOriginType", Value::Integer(1));
    let _ = key.push("keyOriginResolution", Value::Double(72.0));
    let _ = key.push("keyOriginShapeBBox", Value::Descriptor(bbox));
    let _ = key.push("keyOriginIndex", Value::Integer(0));
    let mut top_d = Descriptor::new("null");
    let _ = top_d.push(
        "keyDescriptorList",
        Value::List(vec![Value::Descriptor(key)]),
    );
    let mut s = Sink::new();
    s.u32(1);
    s.u32(16);
    let _ = top_d.write(&mut s);
    s.into_inner()
}

/// The vector-shape parts of a layer, as read.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeData {
    pub path: VectorPath,
    /// The solid fill colour (0..=255 RGB), from `SoCo` or `vscg`. `None`
    /// when the fill is a gradient (`GdFl`) or pattern (`PtFl`) fill layer,
    /// which the caller reads from [`PsdLayer::adjustment`].
    pub fill: Option<[f64; 3]>,
    pub stroke: Option<StrokeStyle>,
}

/// The fill-layer keys a shape layer's interior can be painted by.
pub const SHAPE_FILL_KEYS: [[u8; 4]; 3] = [*b"SoCo", *b"GdFl", *b"PtFl"];

impl ShapeData {
    /// The shape of `layer` on a `width` x `height` canvas: a layer is a
    /// shape layer when it carries a path (`vmsk`/`vsms`) *and* a fill — a
    /// fill-layer block ([`SHAPE_FILL_KEYS`]) or a solid `vscg`. `None`
    /// otherwise — a vector mask on a pixel layer is not a shape.
    pub fn of(layer: &PsdLayer, width: u32, height: u32, opts: &ReadOptions) -> Option<Self> {
        let find = |key: &[u8; 4]| layer.extra.iter().find(|b| &b.key == key);
        let path_block = find(b"vsms").or_else(|| find(b"vmsk"))?;
        let path = VectorPath::decode(&path_block.data, width, height)?;
        let fill_layer = layer
            .adjustment
            .as_ref()
            .filter(|a| SHAPE_FILL_KEYS.contains(&a.key));
        let fill = fill_layer
            .and_then(|a| a.solid_color_rgb(opts))
            .or_else(|| find(b"vscg").and_then(|b| vscg_color(&b.data, opts)));
        if fill.is_none() && fill_layer.is_none_or(|a| a.key == *b"SoCo") {
            return None;
        }
        let stroke = find(b"vstk").and_then(|b| StrokeStyle::decode(&b.data, opts));
        Some(ShapeData { path, fill, stroke })
    }

    /// The layer-level blocks for this shape: `SoCo` goes in the layer's
    /// adjustment slot, the rest (`vmsk`, `vstk`) are returned as tagged
    /// blocks for [`PsdLayer::extra`].
    pub fn blocks(&self, width: u32, height: u32) -> (Vec<u8>, Vec<TaggedBlock>) {
        let soco = encode_solid_color(self.fill.unwrap_or([0.0; 3]));
        let mut extra = vec![TaggedBlock::new(*b"vmsk", self.path.encode(width, height))];
        let stroke = self.stroke.clone().unwrap_or_else(|| StrokeStyle {
            fill_enabled: self.fill.is_some(),
            ..StrokeStyle::default()
        });
        extra.push(TaggedBlock::new(*b"vstk", stroke.encode()));
        (soco, extra)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Adjustment, Rect};

    fn square() -> VectorPath {
        let k = |x: f64, y: f64| Knot {
            before: [x, y],
            anchor: [x, y],
            after: [x, y],
            linked: false,
        };
        VectorPath {
            subpaths: vec![SubPath {
                closed: true,
                operation: 1,
                knots: vec![k(8.0, 8.0), k(32.0, 8.0), k(32.0, 24.0), k(8.0, 24.0)],
            }],
            ..VectorPath::default()
        }
    }

    #[test]
    fn a_path_round_trips_through_fixed_point_records() {
        let path = square();
        let bytes = path.encode(96, 64);
        // Header, fill-rule and initial-fill records, one length record and
        // four knots: every record 26 bytes.
        assert_eq!(bytes.len(), 8 + 26 * 7);
        let back = VectorPath::decode(&bytes, 96, 64).unwrap();
        assert_eq!(back.subpaths.len(), 1);
        for (a, b) in back.subpaths[0].knots.iter().zip(&path.subpaths[0].knots) {
            for i in 0..2 {
                assert!((a.anchor[i] - b.anchor[i]).abs() < 1e-5);
            }
        }
        assert!(back.subpaths[0].closed);
    }

    #[test]
    fn a_path_whose_counts_lie_reads_only_what_is_there() {
        let mut bytes = square().encode(96, 64);
        // Claim 60 000 knots in the length record (after 8 + 2 * 26 bytes
        // and its own selector).
        let at = 8 + 2 * 26 + 2;
        bytes[at..at + 2].copy_from_slice(&60_000u16.to_be_bytes());
        // ...and cut the block mid-record.
        bytes.truncate(bytes.len() - 5);
        let back = VectorPath::decode(&bytes, 96, 64).unwrap();
        assert_eq!(back.subpaths[0].knots.len(), 3);
    }

    #[test]
    fn a_stroke_style_round_trips() {
        let s = StrokeStyle {
            stroke_enabled: true,
            fill_enabled: false,
            width_px: 3.5,
            color: [10.0, 20.0, 30.0],
            opacity: 0.5,
            cap: LineCap::Round,
            join: LineJoin::Bevel,
            align: LineAlign::Inside,
            miter_limit: 7.0,
            dash: vec![2.0, 1.0],
            dash_offset: 0.5,
        };
        let back = StrokeStyle::decode(&s.encode(), &ReadOptions::default()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn a_layer_with_a_path_and_a_solid_fill_reads_as_a_shape() {
        let data = ShapeData {
            path: square(),
            fill: Some([255.0, 0.0, 0.0]),
            stroke: None,
        };
        let (soco, extra) = data.blocks(96, 64);
        let mut layer = PsdLayer::raster("Badge", Rect::sized(1, 1));
        assert!(ShapeData::of(&layer, 96, 64, &ReadOptions::default()).is_none());
        layer.extra = extra;
        // A path alone is a vector mask, not a shape.
        assert!(ShapeData::of(&layer, 96, 64, &ReadOptions::default()).is_none());
        layer.adjustment = Some(Adjustment {
            key: *b"SoCo",
            data: soco,
        });
        let back = ShapeData::of(&layer, 96, 64, &ReadOptions::default()).unwrap();
        assert_eq!(back.fill, Some([255.0, 0.0, 0.0]));
        assert!(back
            .stroke
            .as_ref()
            .is_some_and(|s| s.fill_enabled && !s.stroke_enabled));
    }

    #[test]
    fn a_rect_origination_block_parses_as_a_descriptor() {
        let bytes = encode_rect_origination(1.0, 2.0, 3.0, 4.0);
        let mut cur = Cursor::new(&bytes);
        assert_eq!(cur.u32().unwrap(), 1);
        assert_eq!(cur.u32().unwrap(), 16);
        let d = Descriptor::read(&mut cur, &ReadOptions::default()).unwrap();
        let Some(Value::List(items)) = d.get("keyDescriptorList") else {
            panic!("no key list");
        };
        let Value::Descriptor(k) = &items[0] else {
            panic!("not a descriptor");
        };
        assert_eq!(k.number("keyOriginType"), Some(1.0));
        assert_eq!(
            k.descriptor("keyOriginShapeBBox").unwrap().number("Rght"),
            Some(3.0)
        );
    }
}
