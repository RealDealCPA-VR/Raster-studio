//! Photoshop gradients (`.grd`, version 5).
//!
//! `8BGR`, a `u16` version (5), a `u32` descriptor version (16) and one
//! action descriptor whose `GrdL` list holds the gradients. Each entry is a
//! `Grdn` object (sometimes wrapped in another under `Grad`) with a name
//! (`Nm  `), a form (`GrdF`: `CstS` custom — noise gradients, `ClNs`, are
//! refused by name), a smoothness (`Intr`, `0..=4096`), colour stops
//! (`Clrs`: `Clr ` colour, `Type` user/foreground/background, `Lctn`
//! `0..=4096`, `Mdpn` `0..=100`) and opacity stops (`Trns`: `Opct` percent,
//! `Lctn`, `Mdpn`). The descriptor is read by the `psd` crate's bounded
//! descriptor reader (depth, item-count and string limits).
//!
//! A stop that takes the foreground or background colour has no colour in
//! the file; it imports as Photoshop's default foreground (black) or
//! background (white). The legacy version 3 layout is refused by name.

use psd::bytes::Cursor;
use psd::{Descriptor, Value};

use super::{
    check_count, cmyk_to_rgb, gray_ink_to_rgb, hsb_to_rgb, lab_to_rgb, ColorStopResource,
    GradientResource, Loaded, OpacityStopResource, ResourceError, MAX_ENTRIES, MAX_STOPS,
};

/// Photoshop's full-scale stop location and smoothness.
const SCALE: f64 = 4096.0;

/// Parse a `.grd` file.
pub fn parse(bytes: &[u8]) -> Result<Loaded<GradientResource>, ResourceError> {
    let mut cur = Cursor::new(bytes);
    if cur.tag().ok().as_ref() != Some(b"8BGR") {
        return Err(ResourceError::BadSignature { what: "gradient" });
    }
    let version = cur.u16()?;
    if version != 5 {
        return Err(ResourceError::Unsupported {
            what: "gradient file version",
            detail: format!("{version} (only version 5, Photoshop 6 and later, is read)"),
        });
    }
    let descriptor_version = cur.u32()?;
    if descriptor_version != 16 {
        return Err(ResourceError::Unsupported {
            what: "descriptor version",
            detail: descriptor_version.to_string(),
        });
    }
    let root = Descriptor::read(&mut cur, &psd::ReadOptions::default())?;
    let Some(Value::List(entries)) = root.get("GrdL") else {
        return Err(ResourceError::Malformed("no gradient list (GrdL)".into()));
    };
    check_count("gradient count", entries.len(), MAX_ENTRIES)?;
    let mut loaded = Loaded::default();
    for (index, entry) in entries.iter().enumerate() {
        let Value::Descriptor(outer) = entry else {
            loaded
                .refused
                .push(format!("gradient {} is not an object", index + 1));
            continue;
        };
        let gradient = outer.descriptor("Grad").unwrap_or(outer);
        let name = gradient
            .text("Nm  ")
            .map(|n| n.trim_end_matches('\0').to_string())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("Gradient {}", index + 1));
        match read_gradient(gradient, name.clone()) {
            Ok(g) => loaded.items.push(g),
            Err(why) => loaded.refused.push(format!("{name}: {why}")),
        }
    }
    Ok(loaded)
}

fn list<'a>(d: &'a Descriptor, key: &str) -> &'a [Value] {
    match d.get(key) {
        Some(Value::List(items)) => items,
        _ => &[],
    }
}

fn enum_value<'a>(d: &'a Descriptor, key: &str) -> Option<&'a str> {
    match d.get(key)? {
        Value::Enumerated { value, .. } => Some(value),
        _ => None,
    }
}

fn fraction(v: Option<f64>, full: f64, default: f64) -> f32 {
    let v = v.filter(|v| v.is_finite()).unwrap_or(default) / full;
    v.clamp(0.0, 1.0) as f32
}

fn read_gradient(g: &Descriptor, name: String) -> Result<GradientResource, String> {
    if enum_value(g, "GrdF") == Some("ClNs") {
        return Err("noise gradients are not supported".into());
    }
    let smoothness = fraction(g.number("Intr"), SCALE, SCALE);
    let colors = list(g, "Clrs");
    let opacities = list(g, "Trns");
    if colors.len() > MAX_STOPS || opacities.len() > MAX_STOPS {
        return Err(format!("more than {MAX_STOPS} stops"));
    }
    let mut stops = Vec::with_capacity(colors.len());
    for value in colors {
        let Value::Descriptor(stop) = value else {
            return Err("a colour stop is not an object".into());
        };
        let rgb = match enum_value(stop, "Type") {
            Some("FrgC") => [0.0, 0.0, 0.0],
            Some("BckC") => [1.0, 1.0, 1.0],
            _ => color_of(
                stop.descriptor("Clr ")
                    .ok_or("a colour stop has no colour")?,
            )?,
        };
        stops.push(ColorStopResource {
            position: fraction(stop.number("Lctn"), SCALE, 0.0),
            midpoint: fraction(stop.number("Mdpn"), 100.0, 50.0),
            rgb,
        });
    }
    if stops.is_empty() {
        return Err("no colour stops".into());
    }
    let mut opacity_stops = Vec::with_capacity(opacities.len());
    for value in opacities {
        let Value::Descriptor(stop) = value else {
            return Err("an opacity stop is not an object".into());
        };
        opacity_stops.push(OpacityStopResource {
            position: fraction(stop.number("Lctn"), SCALE, 0.0),
            midpoint: fraction(stop.number("Mdpn"), 100.0, 50.0),
            opacity: fraction(stop.number("Opct"), 100.0, 100.0),
        });
    }
    stops.sort_by(|a, b| a.position.total_cmp(&b.position));
    opacity_stops.sort_by(|a, b| a.position.total_cmp(&b.position));
    Ok(GradientResource {
        name,
        smoothness,
        stops,
        opacity_stops,
    })
}

/// A Photoshop colour object in sRGB.
pub(crate) fn color_of(c: &Descriptor) -> Result<[f32; 3], String> {
    let n = |key: &str| c.number(key).filter(|v| v.is_finite()).unwrap_or(0.0);
    Ok(match c.class_id.as_str() {
        "RGBC" => {
            if c.get("redFloat").is_some() {
                let f = |v: f64| v.clamp(0.0, 1.0) as f32;
                [f(n("redFloat")), f(n("greenFloat")), f(n("blueFloat"))]
            } else {
                let f = |v: f64| (v / 255.0).clamp(0.0, 1.0) as f32;
                [f(n("Rd  ")), f(n("Grn ")), f(n("Bl  "))]
            }
        }
        "HSBC" => hsb_to_rgb(n("H   "), n("Strt") / 100.0, n("Brgh") / 100.0),
        "CMYC" => cmyk_to_rgb(
            n("Cyn ") / 100.0,
            n("Mgnt") / 100.0,
            n("Ylw ") / 100.0,
            n("Blck") / 100.0,
        ),
        "Grsc" => gray_ink_to_rgb(n("Gry ") / 100.0),
        "LbCl" => lab_to_rgb(n("Lmnc"), n("A   "), n("B   ")),
        other => return Err(format!("colour model {other:?} is not supported")),
    })
}
