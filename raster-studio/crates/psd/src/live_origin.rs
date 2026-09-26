//! W16-G: a shape layer's live-shape origination (`vogk`) for more than the
//! sharp rectangle [`crate::shape::encode_rect_origination`] writes: a
//! rectangle with per-corner radii (`keyOriginType` 2, its radii in
//! `keyOriginRRectRadii`), an ellipse (5) and a line (4, its ends in
//! `keyOriginLineStart` / `keyOriginLineEnd` and its weight in
//! `keyOriginLineWeight`, its arrowheads in `keyOriginLineArr*`) — the
//! keys Photoshop and Photopea (`H.ol.LC`) write — and the reader that turns such a block back into a
//! [`LiveShape`]. Every value is in canvas pixels.
//!
//! Polygons and stars have no origination here (Photoshop's polygon keys are
//! not reproduced), so they travel as their path, as a custom shape does.

use layer_model::LiveShape;

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::limits::ReadOptions;

fn px(v: f64) -> Value {
    Value::UnitFloat {
        unit: *b"#Pxl",
        value: v,
    }
}

fn point(x: f64, y: f64) -> Value {
    let mut d = Descriptor::new("Pnt ");
    let _ = d.push("Hrzn", Value::Double(x));
    let _ = d.push("Vrtc", Value::Double(y));
    Value::Descriptor(d)
}

/// The `vogk` block for `live`, in canvas pixels; `None` for a shape with no
/// origination here (polygon, star) or a non-finite record.
pub fn encode_live_origination(live: &LiveShape) -> Option<Vec<u8>> {
    if !live.is_finite() {
        return None;
    }
    let [x, y, w, h] = live.frame();
    let mut key = Descriptor::new("null");
    let kind = match live {
        LiveShape::Rectangle { radii, .. } if radii.iter().any(|r| *r > 0.0) => 2,
        LiveShape::Rectangle { .. } => 1,
        LiveShape::Ellipse { .. } => 5,
        LiveShape::Line { .. } => 4,
        LiveShape::Polygon { .. } | LiveShape::Star { .. } => return None,
    };
    let _ = key.push("keyOriginType", Value::Integer(kind));
    let _ = key.push("keyOriginResolution", Value::Double(72.0));
    let mut bbox = Descriptor::new("unitRect");
    let _ = bbox.push("unitValueQuadVersion", Value::Integer(1));
    let _ = bbox.push("Top ", px(y));
    let _ = bbox.push("Left", px(x));
    let _ = bbox.push("Btom", px(y + h));
    let _ = bbox.push("Rght", px(x + w));
    let _ = key.push("keyOriginShapeBBox", Value::Descriptor(bbox));
    match *live {
        LiveShape::Rectangle { radii, .. } if kind == 2 => {
            // Photoshop's radii descriptor names each corner.
            let mut r = Descriptor::new("radii");
            let _ = r.push("unitValueQuadVersion", Value::Integer(1));
            let _ = r.push("topRight", px(radii[1]));
            let _ = r.push("topLeft", px(radii[0]));
            let _ = r.push("bottomLeft", px(radii[3]));
            let _ = r.push("bottomRight", px(radii[2]));
            let _ = key.push("keyOriginRRectRadii", Value::Descriptor(r));
        }
        LiveShape::Line {
            x1,
            y1,
            x2,
            y2,
            weight,
            arrows,
        } => {
            let _ = key.push("keyOriginLineStart", point(x1, y1));
            let _ = key.push("keyOriginLineEnd", point(x2, y2));
            let _ = key.push("keyOriginLineWeight", Value::Double(weight));
            // Photopea's `H.ol.a1N`: the heads' switches, their width and
            // length in pixels and the concavity in whole percent.
            if let Some(h) = arrows {
                let _ = key.push("keyOriginLineArrowSt", Value::Bool(h.start));
                let _ = key.push("keyOriginLineArrowEnd", Value::Bool(h.end));
                let _ = key.push(
                    "keyOriginLineArrWdth",
                    Value::Double(weight * h.width_pct / 100.0),
                );
                let _ = key.push(
                    "keyOriginLineArrLngth",
                    Value::Double(weight * h.length_pct / 100.0),
                );
                let conc = h.concavity_pct.round().clamp(-50.0, 50.0) as i32;
                let _ = key.push("keyOriginLineArrConc", Value::Integer(conc));
            }
        }
        _ => {}
    }
    let _ = key.push("keyOriginIndex", Value::Integer(0));
    let mut top = Descriptor::new("null");
    let _ = top.push(
        "keyDescriptorList",
        Value::List(vec![Value::Descriptor(key)]),
    );
    let mut s = Sink::new();
    s.u32(1);
    s.u32(16);
    top.write(&mut s).ok()?;
    Some(s.into_inner())
}

/// The live shape a `vogk` block describes (its first origination), in
/// canvas pixels; `None` for a block that does not parse, is invalidated
/// (`keyShapeInvalidated`), or names a type this reader does not map.
pub fn decode_live_origination(data: &[u8], opts: &ReadOptions) -> Option<LiveShape> {
    let mut cur = Cursor::new(data);
    let _version = cur.u32().ok()?;
    let _descriptor_version = cur.u32().ok()?;
    let top = Descriptor::read(&mut cur, opts).ok()?;
    let Some(Value::List(items)) = top.get("keyDescriptorList") else {
        return None;
    };
    let Some(Value::Descriptor(key)) = items.first() else {
        return None;
    };
    if matches!(key.get("keyShapeInvalidated"), Some(Value::Bool(true))) {
        return None;
    }
    let kind = key.number("keyOriginType")?;
    let bbox = key.descriptor("keyOriginShapeBBox");
    let frame = || -> Option<[f64; 4]> {
        let b = bbox?;
        let (l, t) = (b.number("Left")?, b.number("Top ")?);
        let (r, bo) = (b.number("Rght")?, b.number("Btom")?);
        Some([l, t, r - l, bo - t])
    };
    let live = match kind as i64 {
        1 | 2 => {
            let [x, y, w, h] = frame()?;
            let radii = match key.descriptor("keyOriginRRectRadii") {
                Some(r) if kind as i64 == 2 => [
                    r.number("topLeft").unwrap_or(0.0),
                    r.number("topRight").unwrap_or(0.0),
                    r.number("bottomRight").unwrap_or(0.0),
                    r.number("bottomLeft").unwrap_or(0.0),
                ],
                _ => [0.0; 4],
            };
            LiveShape::Rectangle { x, y, w, h, radii }
        }
        5 => {
            let [x, y, w, h] = frame()?;
            LiveShape::Ellipse { x, y, w, h }
        }
        4 => {
            let start = key.descriptor("keyOriginLineStart")?;
            let end = key.descriptor("keyOriginLineEnd")?;
            let weight = key.number("keyOriginLineWeight").unwrap_or(1.0);
            // Photopea's `H.ol.as8`: all five arrow keys or none.
            let flag = |k: &str| match key.get(k) {
                Some(Value::Bool(b)) => Some(*b),
                _ => None,
            };
            let arrows = (|| {
                let (st, en) = (
                    flag("keyOriginLineArrowSt")?,
                    flag("keyOriginLineArrowEnd")?,
                );
                let wd = key.number("keyOriginLineArrWdth")?;
                let ln = key.number("keyOriginLineArrLngth")?;
                let conc = key.number("keyOriginLineArrConc")?;
                let pct = |px: f64| {
                    if weight > 0.0 {
                        px / weight * 100.0
                    } else {
                        0.0
                    }
                };
                (st || en).then(|| layer_model::LiveArrows {
                    start: st,
                    end: en,
                    width_pct: pct(wd),
                    length_pct: pct(ln),
                    concavity_pct: conc,
                })
            })();
            LiveShape::Line {
                x1: start.number("Hrzn")?,
                y1: start.number("Vrtc")?,
                x2: end.number("Hrzn")?,
                y2: end.number("Vrtc")?,
                weight,
                arrows,
            }
        }
        _ => return None,
    };
    let [_, _, w, h] = live.frame();
    let sized = matches!(live, LiveShape::Line { .. }) || (w > 0.0 && h > 0.0);
    (live.is_finite() && sized).then_some(live)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rounded_rectangle_ellipse_and_line_round_trip_through_vogk() {
        let opts = ReadOptions::default();
        for live in [
            LiveShape::Rectangle {
                x: 4.0,
                y: 6.0,
                w: 40.0,
                h: 20.0,
                radii: [1.0, 2.0, 3.0, 4.0],
            },
            LiveShape::Rectangle {
                x: 4.0,
                y: 6.0,
                w: 40.0,
                h: 20.0,
                radii: [0.0; 4],
            },
            LiveShape::Ellipse {
                x: 1.0,
                y: 2.0,
                w: 30.0,
                h: 10.0,
            },
            LiveShape::Line {
                x1: 1.0,
                y1: 2.0,
                x2: 30.0,
                y2: 12.0,
                weight: 3.0,
                arrows: None,
            },
            // W16-G: a line keeps its heads through `keyOriginLineArr*`.
            LiveShape::Line {
                x1: 1.0,
                y1: 2.0,
                x2: 30.0,
                y2: 12.0,
                weight: 4.0,
                arrows: Some(layer_model::LiveArrows {
                    start: true,
                    end: true,
                    width_pct: 500.0,
                    length_pct: 1000.0,
                    concavity_pct: 20.0,
                }),
            },
        ] {
            let bytes = encode_live_origination(&live).expect("encodes");
            assert_eq!(decode_live_origination(&bytes, &opts), Some(live));
        }
        // The sharp rectangle writer's block reads as a sharp rectangle.
        let sharp = crate::shape::encode_rect_origination(1.0, 2.0, 11.0, 7.0);
        assert_eq!(
            decode_live_origination(&sharp, &opts),
            Some(LiveShape::Rectangle {
                x: 1.0,
                y: 2.0,
                w: 10.0,
                h: 5.0,
                radii: [0.0; 4],
            })
        );
        // A polygon has no origination; garbage decodes to nothing.
        assert!(encode_live_origination(&LiveShape::Polygon {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
            sides: 5
        })
        .is_none());
        assert_eq!(decode_live_origination(&[0, 1, 2], &opts), None);
    }
}
