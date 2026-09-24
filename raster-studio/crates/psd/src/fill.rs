//! W9-B: fill layers — the `SoCo` (Solid Color), `GdFl` (Gradient Fill) and
//! `PtFl` (Pattern Fill) adjustment keys — read into and written from
//! [`layer_model::FillSource`].
//!
//! Each payload is a `u32` descriptor version (16) and one descriptor:
//!
//! | key    | items this module reads and writes |
//! |--------|------------------------------------|
//! | `SoCo` | `Clr ` (`RGBC`: `Rd  ` `Grn ` `Bl  ` 0..=255) |
//! | `GdFl` | `Grad` (`Grdn`: `Nm  `, `GrdF`, `Intr`, `Clrs` colour stops, `Trns` opacity stops), `Angl`, `Type`, `Rvrs`, `Dthr`, `Scl `, `Ofst` |
//! | `PtFl` | `Ptrn` (`Nm  `, `Idnt`), `Scl `, `Angl`, `Algn`, `phase` |
//!
//! Stop locations are Photoshop's `0..=4096` integers and midpoints its
//! `0..=100` percentages. A pattern fill names its pattern; the pixels live in
//! the document's `Patt` block ([`crate::pattern::encode_block`]), which is
//! why [`encode_pattern_fill`] also returns the [`PsdPattern`] to carry there.

use layer_model::{
    FillSource, Gradient, GradientFill, GradientStop, GradientStyle, PatternFill, Rgba,
};

use crate::bytes::Sink;
use crate::descriptor::{Descriptor, Value};
use crate::limits::ReadOptions;
use crate::model::Adjustment;
use crate::pattern::PsdPattern;

/// Photoshop's stop-location scale.
const LOCATION_SCALE: f64 = 4096.0;

/// The fill a `SoCo` / `GdFl` layer carries, or `None` for any other key or a
/// payload this reader cannot parse. `PtFl` needs the file's pattern library
/// and is read by [`crate::pattern::pattern_fill_layer`].
pub fn fill_source(adjustment: &Adjustment, opts: &ReadOptions) -> Option<FillSource> {
    match &adjustment.key {
        b"SoCo" => {
            let rgb = adjustment.solid_color_rgb(opts)?;
            rgb.iter()
                .all(|v| v.is_finite())
                .then(|| FillSource::Solid {
                    color: [channel(rgb[0]), channel(rgb[1]), channel(rgb[2]), 1.0],
                })
        }
        b"GdFl" => gradient_fill(&adjustment.descriptor(opts)?).map(FillSource::Gradient),
        _ => None,
    }
}

fn channel(v: f64) -> f32 {
    (v / 255.0).clamp(0.0, 1.0) as f32
}

fn finite(v: f64, fallback: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        fallback
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

fn style_from(code: &str) -> Option<GradientStyle> {
    Some(match code {
        "Lnr " => GradientStyle::Linear,
        "Rdl " => GradientStyle::Radial,
        "Angl" => GradientStyle::Angle,
        "Rflc" => GradientStyle::Reflected,
        "Dmnd" => GradientStyle::Diamond,
        _ => return None,
    })
}

const fn style_code(style: GradientStyle) -> &'static str {
    match style {
        GradientStyle::Linear => "Lnr ",
        GradientStyle::Radial => "Rdl ",
        GradientStyle::Angle => "Angl",
        GradientStyle::Reflected => "Rflc",
        GradientStyle::Diamond => "Dmnd",
    }
}

/// A `GdFl` descriptor as a gradient fill. `None` when it names no colour
/// stops (a noise gradient, which this model cannot hold).
fn gradient_fill(d: &Descriptor) -> Option<GradientFill> {
    let grad = d.descriptor("Grad")?;
    let Some(Value::List(colors)) = grad.get("Clrs") else {
        return None;
    };
    let mut stops: Vec<GradientStop> = colors
        .iter()
        .filter_map(|v| match v {
            Value::Descriptor(stop) => {
                let c = stop.descriptor("Clr ")?;
                let rgb = [c.number("Rd  ")?, c.number("Grn ")?, c.number("Bl  ")?];
                Some(GradientStop {
                    position: (finite(stop.number("Lctn")?, 0.0) / LOCATION_SCALE).clamp(0.0, 1.0)
                        as f32,
                    color: [
                        channel(finite(rgb[0], 0.0)),
                        channel(finite(rgb[1], 0.0)),
                        channel(finite(rgb[2], 0.0)),
                        1.0,
                    ],
                    midpoint: (finite(stop.number("Mdpn").unwrap_or(50.0), 50.0) / 100.0)
                        .clamp(0.0, 1.0) as f32,
                })
            }
            _ => None,
        })
        .collect();
    if stops.is_empty() {
        return None;
    }
    stops.sort_by(|a, b| a.position.total_cmp(&b.position));
    let mut alpha_stops: Vec<GradientStop> = match grad.get("Trns") {
        Some(Value::List(list)) => list
            .iter()
            .filter_map(|v| match v {
                Value::Descriptor(stop) => Some(GradientStop {
                    position: (finite(stop.number("Lctn")?, 0.0) / LOCATION_SCALE).clamp(0.0, 1.0)
                        as f32,
                    color: [
                        0.0,
                        0.0,
                        0.0,
                        (finite(stop.number("Opct").unwrap_or(100.0), 100.0) / 100.0)
                            .clamp(0.0, 1.0) as f32,
                    ],
                    midpoint: (finite(stop.number("Mdpn").unwrap_or(50.0), 50.0) / 100.0)
                        .clamp(0.0, 1.0) as f32,
                }),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    alpha_stops.sort_by(|a, b| a.position.total_cmp(&b.position));
    // An opacity ramp that only restates the colour stops' own alpha (one
    // stop at each colour stop, linear midpoints — what every writer that
    // has no separate alpha ramp emits) folds back into those stops.
    let implied = alpha_stops.len() == stops.len()
        && alpha_stops.iter().zip(&stops).all(|(a, s)| {
            (a.position - s.position).abs() < 1e-6 && (a.midpoint - 0.5).abs() < 1e-6
        });
    if implied {
        for (stop, alpha) in stops.iter_mut().zip(&alpha_stops) {
            stop.color[3] = alpha.color[3];
        }
        alpha_stops.clear();
    }
    let smoothness = (finite(
        grad.number("Intr").unwrap_or(LOCATION_SCALE),
        LOCATION_SCALE,
    ) / LOCATION_SCALE)
        .clamp(0.0, 1.0) as f32;
    let offset = d
        .descriptor("Ofst")
        .map(|p| {
            [
                finite(p.number("Hrzn").unwrap_or(0.0), 0.0) as f32,
                finite(p.number("Vrtc").unwrap_or(0.0), 0.0) as f32,
            ]
        })
        .unwrap_or([0.0, 0.0]);
    let scale = finite(d.number("Scl ").unwrap_or(100.0), 100.0) / 100.0;
    Some(GradientFill {
        gradient: Gradient {
            stops,
            alpha_stops,
            smoothness,
        },
        style: enum_value(d, "Type")
            .and_then(style_from)
            .unwrap_or(GradientStyle::Linear),
        angle_deg: finite(d.number("Angl").unwrap_or(90.0), 90.0) as f32,
        scale: if scale > 0.0 { scale as f32 } else { 1.0 },
        reverse: bool_value(d, "Rvrs").unwrap_or(false),
        dither: bool_value(d, "Dthr").unwrap_or(false),
        offset_px: offset,
    })
}

fn rgbc(color: Rgba) -> Value {
    let byte = |v: f32| f64::from(v.clamp(0.0, 1.0)) * 255.0;
    let mut c = Descriptor::new("RGBC");
    let _ = c.push("Rd  ", Value::Double(byte(color[0])));
    let _ = c.push("Grn ", Value::Double(byte(color[1])));
    let _ = c.push("Bl  ", Value::Double(byte(color[2])));
    Value::Descriptor(c)
}

fn enumerated(type_id: &str, value: &str) -> Value {
    Value::Enumerated {
        type_id: type_id.into(),
        value: value.into(),
    }
}

fn unit_float(unit: &[u8; 4], value: f64) -> Value {
    Value::UnitFloat { unit: *unit, value }
}

fn payload(key: [u8; 4], d: &Descriptor) -> Adjustment {
    let mut s = Sink::new();
    s.u32(16);
    // Every key here comes from this module's own constants, which are all
    // four characters, so the write cannot refuse one.
    let _ = d.write(&mut s);
    Adjustment {
        key,
        data: s.into_inner(),
    }
}

/// A `SoCo` payload for `color`. The format has no alpha: a translucent
/// colour's alpha is the caller's to carry (as the layer's fill opacity).
pub fn encode_solid_fill(color: Rgba) -> Adjustment {
    let mut d = Descriptor::new("null");
    let _ = d.push("Clr ", rgbc(color));
    payload(*b"SoCo", &d)
}

/// A `GdFl` payload for `g`.
pub fn encode_gradient_fill(g: &GradientFill) -> Adjustment {
    let location =
        |p: f32| Value::Integer((f64::from(p.clamp(0.0, 1.0)) * LOCATION_SCALE).round() as i32);
    let midpoint = |m: f32| Value::Integer((f64::from(m.clamp(0.0, 1.0)) * 100.0).round() as i32);
    let mut grad = Descriptor::new("Grdn");
    let _ = grad.push("Nm  ", Value::Text("Custom".into()));
    let _ = grad.push("GrdF", enumerated("GrdF", "CstS"));
    let _ = grad.push(
        "Intr",
        Value::Double(f64::from(g.gradient.smoothness.clamp(0.0, 1.0)) * LOCATION_SCALE),
    );
    let colors = g
        .gradient
        .stops
        .iter()
        .map(|s| {
            let mut stop = Descriptor::new("Clrt");
            let _ = stop.push("Clr ", rgbc(s.color));
            let _ = stop.push("Type", enumerated("Clry", "UsrS"));
            let _ = stop.push("Lctn", location(s.position));
            let _ = stop.push("Mdpn", midpoint(s.midpoint));
            Value::Descriptor(stop)
        })
        .collect();
    let _ = grad.push("Clrs", Value::List(colors));
    // Without separate alpha stops the colour stops' own alpha is the ramp's.
    let alpha_source = if g.gradient.alpha_stops.is_empty() {
        &g.gradient.stops
    } else {
        &g.gradient.alpha_stops
    };
    let alphas = alpha_source
        .iter()
        .map(|s| {
            let mut stop = Descriptor::new("TrnS");
            let _ = stop.push(
                "Opct",
                unit_float(b"#Prc", f64::from(s.color[3].clamp(0.0, 1.0)) * 100.0),
            );
            let _ = stop.push("Lctn", location(s.position));
            let _ = stop.push(
                "Mdpn",
                midpoint(if g.gradient.alpha_stops.is_empty() {
                    0.5
                } else {
                    s.midpoint
                }),
            );
            Value::Descriptor(stop)
        })
        .collect();
    let _ = grad.push("Trns", Value::List(alphas));

    let mut d = Descriptor::new("null");
    let _ = d.push("Grad", Value::Descriptor(grad));
    let _ = d.push("Angl", unit_float(b"#Ang", f64::from(g.angle_deg)));
    let _ = d.push("Type", enumerated("GrdT", style_code(g.style)));
    let _ = d.push("Rvrs", Value::Bool(g.reverse));
    let _ = d.push("Dthr", Value::Bool(g.dither));
    let _ = d.push("Scl ", unit_float(b"#Prc", f64::from(g.scale) * 100.0));
    let mut ofst = Descriptor::new("Pnt ");
    let _ = ofst.push("Hrzn", Value::Double(f64::from(g.offset_px[0])));
    let _ = ofst.push("Vrtc", Value::Double(f64::from(g.offset_px[1])));
    let _ = d.push("Ofst", Value::Descriptor(ofst));
    payload(*b"GdFl", &d)
}

/// A `PtFl` payload for `p`, and the pattern it names, for the caller to
/// carry in the document's `Patt` block. `None` when the fill has no pixels.
///
/// The pattern's id is derived from its content, so two layers filled with
/// the same pixels name one pattern.
pub fn encode_pattern_fill(p: &PatternFill) -> Option<(Adjustment, PsdPattern)> {
    let tile = p.tile.as_ref()?;
    let id = format!(
        "rs-{:016x}-{}x{}",
        tile.content_hash(),
        tile.width(),
        tile.height()
    );
    let name = if tile.name().trim().is_empty() {
        "Pattern".to_string()
    } else {
        tile.name().to_string()
    };
    let mut ptrn = Descriptor::new("Ptrn");
    let _ = ptrn.push("Nm  ", Value::Text(name.clone()));
    let _ = ptrn.push("Idnt", Value::Text(id.clone()));
    let mut d = Descriptor::new("null");
    let _ = d.push("Ptrn", Value::Descriptor(ptrn));
    let _ = d.push("Scl ", unit_float(b"#Prc", f64::from(p.scale) * 100.0));
    let _ = d.push("Angl", unit_float(b"#Ang", f64::from(p.angle_deg)));
    let _ = d.push("Algn", Value::Bool(p.link_with_layer));
    let mut phase = Descriptor::new("Pnt ");
    let _ = phase.push("Hrzn", Value::Double(f64::from(p.offset_px[0])));
    let _ = phase.push("Vrtc", Value::Double(f64::from(p.offset_px[1])));
    let _ = d.push("phase", Value::Descriptor(phase));
    let pattern = PsdPattern {
        name,
        id,
        width: tile.width(),
        height: tile.height(),
        rgba8: tile.rgba8().to_vec(),
    };
    Some((payload(*b"PtFl", &d), pattern))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{pattern_fill_layer, PatternLibrary};
    use layer_model::PatternTile;

    #[test]
    fn a_solid_fill_round_trips_through_soco() {
        let adj = encode_solid_fill([1.0, 0.5, 0.0, 1.0]);
        assert_eq!(adj.key, *b"SoCo");
        match fill_source(&adj, &ReadOptions::default()) {
            Some(FillSource::Solid { color }) => {
                assert_eq!(color[0], 1.0);
                assert!((color[1] - 0.5).abs() < 1.0 / 255.0);
                assert_eq!(color[2], 0.0);
                assert_eq!(color[3], 1.0);
            }
            other => panic!("read back as {other:?}"),
        }
    }

    #[test]
    fn a_gradient_fill_round_trips_through_gdfl() {
        let g = GradientFill {
            gradient: Gradient {
                stops: vec![
                    GradientStop {
                        position: 0.0,
                        color: [1.0, 0.0, 0.0, 1.0],
                        midpoint: 0.3,
                    },
                    GradientStop {
                        position: 1.0,
                        color: [0.0, 0.0, 1.0, 0.5],
                        midpoint: 0.5,
                    },
                ],
                alpha_stops: Vec::new(),
                smoothness: 1.0,
            },
            style: GradientStyle::Radial,
            angle_deg: 30.0,
            scale: 0.75,
            reverse: true,
            dither: true,
            offset_px: [4.0, -2.0],
        };
        let adj = encode_gradient_fill(&g);
        let Some(FillSource::Gradient(back)) = fill_source(&adj, &ReadOptions::default()) else {
            panic!("did not read back as a gradient fill");
        };
        assert_eq!(back.style, GradientStyle::Radial);
        assert_eq!(back.angle_deg, 30.0);
        assert!((back.scale - 0.75).abs() < 1e-6);
        assert!(back.reverse && back.dither);
        assert_eq!(back.offset_px, [4.0, -2.0]);
        assert_eq!(back.gradient.stops.len(), 2);
        assert_eq!(back.gradient.stops[0].color, [1.0, 0.0, 0.0, 1.0]);
        assert!((back.gradient.stops[0].midpoint - 0.3).abs() < 1e-6);
        assert_eq!(back.gradient.stops[1].position, 1.0);
        // The colour stop's own alpha travelled as an opacity stop and folds
        // back into the colour stop.
        assert!(back.gradient.alpha_stops.is_empty());
        assert_eq!(back.gradient.stops[1].color[3], 0.5);
    }

    #[test]
    fn a_pattern_fill_round_trips_through_ptfl_and_its_pattern_block() {
        let tile = PatternTile::new("Dots", 2, 2, vec![7u8; 16]).unwrap();
        let fill = PatternFill {
            tile: Some(tile.clone()),
            scale: 2.0,
            offset_px: [1.0, 3.0],
            angle_deg: 0.0,
            link_with_layer: false,
            ..PatternFill::default()
        };
        let (adj, pattern) = encode_pattern_fill(&fill).unwrap();
        let mut library = PatternLibrary::default();
        let block = crate::pattern::encode_block(&[pattern]);
        let mut budget = crate::limits::Budget::new(u64::MAX);
        library.read_block(&block, &ReadOptions::default(), &mut budget);
        let back = pattern_fill_layer(&adj, &ReadOptions::default(), &library)
            .expect("the PtFl names the pattern its block carries");
        assert_eq!(
            back.tile.as_ref().map(|t| t.rgba8().to_vec()),
            Some(vec![7u8; 16])
        );
        assert!((back.scale - 2.0).abs() < 1e-6);
        assert_eq!(back.offset_px, [1.0, 3.0]);
        assert!(!back.link_with_layer);
    }

    #[test]
    fn a_non_fill_key_is_not_a_fill() {
        let adj = Adjustment {
            key: *b"nvrt",
            data: Vec::new(),
        };
        assert_eq!(fill_source(&adj, &ReadOptions::default()), None);
    }
}
