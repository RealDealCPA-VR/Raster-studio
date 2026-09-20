//! The `lfx2` layer-effect descriptor, decoded into
//! [`layer_model::LayerEffects`] parameters.
//!
//! Card 075: the four effects the thumbnail workflow requires — drop shadow
//! (`DrSh`), solid-colour stroke (`FrFX`), solid colour overlay (`SoFi`,
//! accepted under the older `SoCo` key too) and outer glow (`OrGl`, accepted
//! under `OglD` as well) — map into editable native parameters. Everything
//! else the descriptor lists is named and left behind rather than half-mapped.
//!
//! # Retention is not rendering
//!
//! [`crate::model::Effects`] still keeps the verbatim `lfx2` bytes on every
//! layer, mapped or not, and a save writes them back. That retention means a
//! round trip through this crate cannot lose the original file's styles — it
//! does **not** mean this editor renders them. The import report is the place
//! that distinction is stated; nothing here claims otherwise.
//!
//! # Key → field mapping (as implemented)
//!
//! The `lfx2` block is an object version word, a descriptor version word, then
//! one descriptor (class `Lfx2`) whose items are the effects, keyed by their
//! four-character tags, plus two block-level items:
//!
//! | Source key (unit) | Meaning | Native target |
//! |---|---|---|
//! | `masterFXSwitch` (`bool`) | master effects toggle | `LayerEffects::enabled` (absent ⇒ `true`) |
//! | `Scl ` (`#Prc`) | scale, 100 = 1.0 | multiplies every pixel length below (Photoshop scales distance, size and spread) |
//! | `DrSh` (`Objc`) | drop shadow | `LayerEffects::drop_shadow` |
//! | `FrFX` (`Objc`) | stroke | `LayerEffects::stroke` |
//! | `SoFi`/`SoCo` (`Objc`) | solid colour overlay | `LayerEffects::color_overlay` |
//! | `OrGl`/`OglD` (`Objc`) | outer glow | `LayerEffects::outer_glow` |
//!
//! Inside an effect descriptor:
//!
//! | Source key (unit) | Native field | Conversion |
//! |---|---|---|
//! | `enab` (`bool`) | presence | `false` leaves the slot empty — the model has no per-effect toggle, and an effect Photoshop does not draw cannot be lost visually |
//! | `Md  ` (`enum BlnM`) | `blend_mode` | `blend_from_blnm` below |
//! | `Clr ` (`Objc RGBC`) | `color` / `FillStyle::Solid` | `Rd  `/`Grn `/`Bl  ` are 0..=255 **gamma-encoded sRGB**; each channel is scaled to 0..=1 and stored gamma-encoded — document-space 0..1 (decoded to linear at render by the compositor, the same convention the 8-bit pixel path uses). The alpha channel is 1.0 — the effect's own alpha lives in `opacity` |
//! | `opacity`/`Opct` (`#Prc`) | `opacity` | value / 100 |
//! | `lagl` (`#Ang`) | `angle_deg` | degrees, unchanged — the compositor already uses Photoshop's convention (the angle names where the light is; the shadow falls the other way) |
//! | `uglg` (`bool`) | `use_global_light` | unchanged; absent ⇒ `true` |
//! | `Dstn` (`#Pxl`) | `distance_px` | value × scale |
//! | `blur` (`#Pxl`) | `size_px` | value × scale |
//! | `Ckmt` (`#Pxl`) | `spread` | the native model stores a *fraction of `size_px`*; the file stores pixels, so `Ckmt × scale / (blur × scale)`, clamped 0..=1 |
//! | `Nose` (`#Prc`) | `noise` | value / 100 |
//! | `layerConceals` (`bool`) | `knockout` | unchanged; absent ⇒ `false` (Photoshop's default) |
//! | `Sz  ` (`#Pxl`, stroke) | `size_px` | value × scale |
//! | `Styl` (`enum FStl`) | `position` | `OutF`→Outside, `InsF`→Inside, `Cntr`/`CtrF`→Center; absent ⇒ Outside |
//! | `PntT` (`enum FrFl`) | stroke fill | `SClr` maps; a gradient or pattern stroke does not, and is named as unmapped |
//! | `GlwT` (`enum BETe`, glow) | `technique` | `SfBL`→Softer, `PrBL`→Precise; absent ⇒ Softer |
//!
//! An effect whose *required* fields are missing is not half-mapped: it is
//! reported as unmapped under its kind name, and every field it did carry
//! stays only in the retained bytes.

use crate::descriptor::{Descriptor, Value};
use crate::model::Effects;
use crate::ReadOptions;
use layer_model::effects::{
    ColorOverlayEffect, FillStyle, GlowEffect, GlowSource, GlowTechnique, LayerEffects,
    ShadowEffect, StrokeEffect, StrokePosition,
};
use layer_model::{BlendMode, Rgba};

/// What an `lfx2` block decoded to.
pub struct ImportedEffects {
    /// Every effect that mapped completely.
    pub effects: LayerEffects,
    /// The effects present in the descriptor that did **not** map — as the
    /// human kind names the import report shows, e.g. `"inner shadow"`.
    pub unmapped: Vec<String>,
}

/// Decode an effects block. `None` means the block is not `lfx2` (the legacy
/// `lrFX` layout, say) or its descriptor could not be read at all — the
/// caller's existing "layer effect(s) … were not imported" note covers that.
pub fn import_effects(effects: &Effects, opts: &ReadOptions) -> Option<ImportedEffects> {
    let descriptor = effects.descriptor(opts)?;

    let mut out = ImportedEffects {
        effects: LayerEffects::default(),
        unmapped: Vec::new(),
    };

    // Photoshop's master toggle; absent means on, like the model's default.
    if let Some(Value::Bool(on)) = descriptor.get("masterFXSwitch") {
        out.effects.enabled = *on;
    }
    // Scale is a percentage the format applies to every measurement in the
    // block. Kept as a fraction; absent (older writers) means 1.0.
    let scale = if descriptor.get("Scl ").is_some() {
        percent(&descriptor, "Scl ").map_or(1.0, |s| s.clamp(0.0, 10.0))
    } else {
        1.0
    };

    for (key, value) in &descriptor.items {
        let Value::Descriptor(effect) = value else {
            continue;
        };
        // An effect switched off is genuinely absent from the model — the
        // native model has no per-effect toggle, and an effect Photoshop
        // itself does not draw leaves nothing for the user to miss. (An
        // effect switched off *in the file* is different from an effect this
        // build does not model: the latter is named in the report; the former
        // Photoshop does not draw either, so its absence is honest, and the
        // verbatim bytes below still let a save restore it.)
        if !enabled(effect) {
            continue;
        }
        match key.as_str() {
            "DrSh" => match drop_shadow(effect, scale) {
                Some(s) => out.effects.drop_shadow = Some(s),
                None => out.unmapped.push("drop shadow".into()),
            },
            "FrFX" => match stroke(effect, scale) {
                Some(s) => out.effects.stroke = Some(s),
                None => out.unmapped.push("stroke".into()),
            },
            "SoFi" | "SoCo" => match color_overlay(effect) {
                Some(s) => out.effects.color_overlay = Some(s),
                None => out.unmapped.push("colour overlay".into()),
            },
            "OrGl" | "OglD" => match outer_glow(effect, scale) {
                Some(s) => out.effects.outer_glow = Some(s),
                None => out.unmapped.push("outer glow".into()),
            },
            // Known kinds this build does not model, named for the report.
            "IrSh" => out.unmapped.push("inner shadow".into()),
            "IrGl" => out.unmapped.push("inner glow".into()),
            "ebbl" => out.unmapped.push("bevel and emboss".into()),
            "ChFX" => out.unmapped.push("satin".into()),
            "GrFl" | "Grdf" => out.unmapped.push("gradient overlay".into()),
            "PtFl" | "PttR" => out.unmapped.push("pattern overlay".into()),
            // The two block-level keys above, and anything unrecognised.
            "Scl " | "masterFXSwitch" => {}
            other => out
                .unmapped
                .push(format!("an effect this build does not know ({other})")),
        }
    }
    Some(out)
}

/// The `BlnM` enumerated values a descriptor carries for blend modes.
///
/// Photoshop writes the short four-character codes; some exporters spell the
/// remaining modes out in words, so both spellings are accepted.
fn blend_from_blnm(value: &str) -> Option<BlendMode> {
    let mode = match value {
        "Nrml" | "normal" => BlendMode::Normal,
        "Dslv" | "dissolve" => BlendMode::Dissolve,
        "Drkn" | "darken" => BlendMode::Darken,
        "Mltp" | "multiply" => BlendMode::Multiply,
        "CBrn" | "colorBurn" => BlendMode::ColorBurn,
        "Lmbs" | "linearBurn" => BlendMode::LinearBurn,
        "dkCl" | "darkerColor" => BlendMode::DarkerColor,
        "Lghn" | "lighten" => BlendMode::Lighten,
        "Scrn" | "screen" => BlendMode::Screen,
        "CDdg" | "colorDodge" => BlendMode::ColorDodge,
        "lddg" | "linearDodge" => BlendMode::LinearDodge,
        "lgCl" | "lighterColor" => BlendMode::LighterColor,
        "Ovrl" | "Ovln" | "overlay" => BlendMode::Overlay,
        "SftL" | "softLight" => BlendMode::SoftLight,
        "HrdL" | "hardLight" => BlendMode::HardLight,
        "vLit" | "vividLight" => BlendMode::VividLight,
        "lLit" | "linearLight" => BlendMode::LinearLight,
        "pLit" | "pinLight" => BlendMode::PinLight,
        "HrdM" | "hardMix" => BlendMode::HardMix,
        "Dfrn" | "difference" => BlendMode::Difference,
        "Xclu" | "exclusion" => BlendMode::Exclusion,
        "Sbtr" | "blendSubtraction" => BlendMode::Subtract,
        "blendDivide" => BlendMode::Divide,
        "H   " | "hue" => BlendMode::Hue,
        "Strt" | "saturation" => BlendMode::Saturation,
        "Clr " | "color" => BlendMode::Color,
        "Lmns" | "luminosity" => BlendMode::Luminosity,
        _ => return None,
    };
    Some(mode)
}

/// An 8-bit sRGB channel, 0..=255 gamma-encoded, as document-space 0..=1.
///
/// The model's convention is straight-alpha RGBA in the document colour
/// space, *gamma-encoded* — the same as the 8-bit pixel path — so the raw
/// byte over 255 is already the value to store. The compositor decodes
/// stored effect colours to linear light at render time
/// (`compositor::effects::linear_rgb`); linearising here too would darken
/// every effect colour twice.
fn srgb_channel(channel: f64) -> f32 {
    (channel / 255.0).clamp(0.0, 1.0) as f32
}

fn enumerated<'a>(d: &'a Descriptor, key: &str) -> Option<&'a str> {
    match d.get(key)? {
        Value::Enumerated { value, .. } => Some(value),
        _ => None,
    }
}

fn flag(d: &Descriptor, key: &str) -> Option<bool> {
    match d.get(key)? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

/// A 0..=1 fraction from a `#Prc` unit float.
///
/// A bare number is accepted as a fallback only when it is already a
/// fraction (≤ 1.0); a bare value above 1.0 is refused rather than guessed
/// to be a percentage — guessing 1.5 into 0.015 corrupts the value silently.
fn percent(d: &Descriptor, key: &str) -> Option<f32> {
    match d.get(key)? {
        Value::UnitFloat { unit, value } if unit.as_slice() == b"#Prc" => {
            Some((*value / 100.0) as f32)
        }
        Value::Double(v) if *v <= 1.0 => Some(*v as f32),
        Value::Integer(v) if *v <= 1 => Some(*v as f32),
        _ => None,
    }
}

/// A length in document pixels. A unit other than `#Pxl` is refused rather
/// than misread — the format also stores angles and percentages in `UntF`s.
fn pixels(d: &Descriptor, key: &str) -> Option<f32> {
    match d.get(key)? {
        Value::UnitFloat { unit, value } if unit.as_slice() == b"#Pxl" => Some(*value as f32),
        Value::Double(v) => Some(*v as f32),
        Value::Integer(v) => Some(*v as f32),
        _ => None,
    }
}

fn degrees(d: &Descriptor, key: &str) -> Option<f32> {
    match d.get(key)? {
        Value::UnitFloat { unit, value }
            if unit.as_slice() == b"#Ang" || unit.as_slice() == b"#Deg" =>
        {
            Some(*value as f32)
        }
        Value::Double(v) => Some(*v as f32),
        Value::Integer(v) => Some(*v as f32),
        _ => None,
    }
}

fn color(d: &Descriptor) -> Option<Rgba> {
    let c = d.descriptor("Clr ")?;
    Some([
        srgb_channel(c.number("Rd  ")?),
        srgb_channel(c.number("Grn ")?),
        srgb_channel(c.number("Bl  ")?),
        1.0,
    ])
}

fn blend_mode(d: &Descriptor) -> Option<BlendMode> {
    blend_from_blnm(enumerated(d, "Md  ")?)
}

/// The effect's opacity, under either spelling Photoshop has used.
fn opacity_of(d: &Descriptor) -> Option<f32> {
    if d.get("opacity").is_some() {
        percent(d, "opacity")
    } else {
        percent(d, "Opct")
    }
}

/// `enab` defaults to on when the descriptor does not carry it.
fn enabled(d: &Descriptor) -> bool {
    flag(d, "enab").unwrap_or(true)
}

fn drop_shadow(d: &Descriptor, scale: f32) -> Option<ShadowEffect> {
    let color = color(d)?;
    let blend_mode = blend_mode(d)?;
    let opacity = opacity_of(d)?;
    let size_px = pixels(d, "blur")? * scale;
    let distance_px = pixels(d, "Dstn")? * scale;
    let spread_px = pixels(d, "Ckmt").unwrap_or(0.0) * scale;
    Some(ShadowEffect {
        blend_mode,
        color,
        opacity,
        angle_deg: degrees(d, "lagl")?,
        use_global_light: flag(d, "uglg").unwrap_or(true),
        distance_px,
        spread: if size_px > 0.0 {
            (spread_px / size_px).clamp(0.0, 1.0)
        } else {
            0.0
        },
        size_px,
        noise: percent(d, "Nose").unwrap_or(0.0),
        knockout: flag(d, "layerConceals").unwrap_or(false),
    })
}

fn stroke(d: &Descriptor, scale: f32) -> Option<StrokeEffect> {
    // Only a solid-colour stroke maps; a gradient or pattern fill is a
    // different effect this build does not model.
    if enumerated(d, "PntT")? != "SClr" {
        return None;
    }
    let position = match enumerated(d, "Styl") {
        Some("InsF") => StrokePosition::Inside,
        Some("Cntr" | "CtrF") => StrokePosition::Center,
        Some("OutF") | None => StrokePosition::Outside,
        Some(_) => return None,
    };
    Some(StrokeEffect {
        size_px: pixels(d, "Sz  ")? * scale,
        position,
        blend_mode: blend_mode(d)?,
        opacity: opacity_of(d)?,
        fill: FillStyle::Solid(color(d)?),
        overprint: flag(d, "overprint").unwrap_or(false),
    })
}

fn color_overlay(d: &Descriptor) -> Option<ColorOverlayEffect> {
    Some(ColorOverlayEffect {
        blend_mode: blend_mode(d)?,
        color: color(d)?,
        opacity: opacity_of(d)?,
    })
}

fn outer_glow(d: &Descriptor, scale: f32) -> Option<GlowEffect> {
    let size_px = pixels(d, "blur")? * scale;
    let spread_px = pixels(d, "Ckmt").unwrap_or(0.0) * scale;
    Some(GlowEffect {
        blend_mode: blend_mode(d)?,
        fill: FillStyle::Solid(color(d)?),
        opacity: opacity_of(d)?,
        noise: if d.get("Nose").is_some() {
            percent(d, "Nose")
        } else {
            percent(d, "ShdN")
        }
        .unwrap_or(0.0),
        technique: match enumerated(d, "GlwT") {
            Some("PrBL") => GlowTechnique::Precise,
            Some("SfBL") | None => GlowTechnique::Softer,
            Some(_) => return None,
        },
        spread: if size_px > 0.0 {
            (spread_px / size_px).clamp(0.0, 1.0)
        } else {
            0.0
        },
        size_px,
        // `RngL` (contour range) and `Jitter` are read when the file carries
        // them. Absent, the values are the documented Photoshop defaults —
        // 50 % range, no jitter, edge source — deliberate documented
        // defaults, not inventions.
        range: percent(d, "RngL").unwrap_or(0.5),
        jitter: percent(d, "Jitter").unwrap_or(0.0),
        source: match enumerated(d, "Slct") {
            Some("Ctr ") => GlowSource::Center,
            Some("Edgs") | None => GlowSource::Edge,
            Some(_) => GlowSource::Edge,
        },
    })
}

// ------------------------------------------------------- card 080: writing

/// The `BlnM` code for a blend mode — the inverse of [`blend_from_blnm`],
/// spelling the short four-character codes Photoshop itself writes.
fn blnm(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "Nrml",
        BlendMode::Dissolve => "Dslv",
        BlendMode::Darken => "Drkn",
        BlendMode::Multiply => "Mltp",
        BlendMode::ColorBurn => "CBrn",
        BlendMode::LinearBurn => "Lmbs",
        BlendMode::DarkerColor => "dkCl",
        BlendMode::Lighten => "Lghn",
        BlendMode::Screen => "Scrn",
        BlendMode::ColorDodge => "CDdg",
        BlendMode::LinearDodge => "lddg",
        BlendMode::LighterColor => "lgCl",
        BlendMode::Overlay => "Ovrl",
        BlendMode::SoftLight => "SftL",
        BlendMode::HardLight => "HrdL",
        BlendMode::VividLight => "vLit",
        BlendMode::LinearLight => "lLit",
        BlendMode::PinLight => "pLit",
        BlendMode::HardMix => "HrdM",
        BlendMode::Difference => "Dfrn",
        BlendMode::Exclusion => "Xclu",
        BlendMode::Subtract => "Sbtr",
        BlendMode::Divide => "blendDivide",
        BlendMode::Hue => "H   ",
        BlendMode::Saturation => "Strt",
        BlendMode::Color => "Clr ",
        BlendMode::Luminosity => "Lmns",
    }
}

/// A document-space straight RGBA as the descriptor's `RGBC` sub-descriptor:
/// 8-bit sRGB, exactly inverting [`srgb_channel`].
fn rgbc(color: Rgba) -> Value {
    let mut c = crate::Descriptor::new("RGBC");
    let _ = c.push("Rd  ", Value::Double(f64::from(color[0]) * 255.0));
    let _ = c.push("Grn ", Value::Double(f64::from(color[1]) * 255.0));
    let _ = c.push("Bl  ", Value::Double(f64::from(color[2]) * 255.0));
    Value::Descriptor(c)
}

fn percent_value(fraction: f32) -> Value {
    Value::UnitFloat {
        unit: *b"#Prc",
        value: f64::from(fraction) * 100.0,
    }
}

fn px_value(px: f32) -> Value {
    Value::UnitFloat {
        unit: *b"#Pxl",
        value: f64::from(px),
    }
}

fn angle_value(deg: f32) -> Value {
    Value::UnitFloat {
        unit: *b"#Ang",
        value: f64::from(deg),
    }
}

fn enumerated_value(type_id: &str, value: &str) -> Value {
    Value::Enumerated {
        type_id: type_id.into(),
        value: value.into(),
    }
}

/// Encode the model's layer effects as an `lfx2` descriptor payload (card
/// 080) — the exact inverse of [`import_effects`] for the four kinds this
/// build maps, with the same keys and units its parsers read.
///
/// Returns the bytes plus the human kind names that could NOT be written
/// (an effect the model carries but whose descriptor form this writer does
/// not produce, e.g. a gradient-filled glow). An all-unmapped result
/// returns `None` so the caller keeps its existing "not imported" note
/// instead of writing a meaningless block.
pub fn export_effects(effects: &LayerEffects) -> Option<(Vec<u8>, Vec<String>)> {
    let mut top = crate::Descriptor::new("Lfx2");
    let _ = top.push("masterFXSwitch", Value::Bool(effects.enabled));
    let _ = top.push("Scl ", percent_value(1.0));
    let mut unmapped: Vec<String> = Vec::new();
    let mut wrote = false;

    if let Some(s) = &effects.drop_shadow {
        let mut d = crate::Descriptor::new("DrSh");
        let _ = d.push("enab", Value::Bool(true));
        let _ = d.push("Md  ", enumerated_value("BlnM", blnm(s.blend_mode)));
        let _ = d.push("Clr ", rgbc(s.color));
        let _ = d.push("opacity", percent_value(s.opacity));
        let _ = d.push("lagl", angle_value(s.angle_deg));
        let _ = d.push("uglg", Value::Bool(s.use_global_light));
        let _ = d.push("Dstn", px_value(s.distance_px));
        let _ = d.push("blur", px_value(s.size_px));
        let _ = d.push("Ckmt", px_value(s.spread * s.size_px));
        let _ = d.push("Nose", percent_value(s.noise));
        let _ = d.push("layerConceals", Value::Bool(s.knockout));
        let _ = top.push("DrSh", Value::Descriptor(d));
        wrote = true;
    }
    if effects.inner_shadow.is_some() {
        unmapped.push("inner shadow".into());
    }
    if let Some(s) = &effects.stroke {
        match &s.fill {
            FillStyle::Solid(color) => {
                let mut d = crate::Descriptor::new("FrFX");
                let _ = d.push("enab", Value::Bool(true));
                let _ = d.push("Md  ", enumerated_value("BlnM", blnm(s.blend_mode)));
                let _ = d.push("Clr ", rgbc(*color));
                let _ = d.push("Opct", percent_value(s.opacity));
                let _ = d.push("Sz  ", px_value(s.size_px));
                let _ = d.push("PntT", enumerated_value("FrFl", "SClr"));
                let _ = d.push(
                    "Styl",
                    enumerated_value(
                        "FStl",
                        match s.position {
                            StrokePosition::Inside => "InsF",
                            StrokePosition::Center => "CtrF",
                            StrokePosition::Outside => "OutF",
                        },
                    ),
                );
                let _ = d.push("overprint", Value::Bool(s.overprint));
                let _ = top.push("FrFX", Value::Descriptor(d));
                wrote = true;
            }
            FillStyle::Gradient(_) | FillStyle::Pattern(_) => {
                unmapped.push("stroke".into());
            }
        }
    }
    if let Some(s) = &effects.color_overlay {
        let mut d = crate::Descriptor::new("SoFi");
        let _ = d.push("enab", Value::Bool(true));
        let _ = d.push("Md  ", enumerated_value("BlnM", blnm(s.blend_mode)));
        let _ = d.push("Clr ", rgbc(s.color));
        let _ = d.push("Opct", percent_value(s.opacity));
        let _ = top.push("SoFi", Value::Descriptor(d));
        wrote = true;
    }
    if let Some(s) = &effects.outer_glow {
        match &s.fill {
            FillStyle::Solid(color) => {
                let mut d = crate::Descriptor::new("OrGl");
                let _ = d.push("enab", Value::Bool(true));
                let _ = d.push("Md  ", enumerated_value("BlnM", blnm(s.blend_mode)));
                let _ = d.push("Clr ", rgbc(*color));
                let _ = d.push("Opct", percent_value(s.opacity));
                let _ = d.push("blur", px_value(s.size_px));
                let _ = d.push("Ckmt", px_value(s.spread * s.size_px));
                let _ = d.push("Nose", percent_value(s.noise));
                let _ = d.push(
                    "GlwT",
                    enumerated_value(
                        "BETE",
                        match s.technique {
                            GlowTechnique::Precise => "PrBL",
                            GlowTechnique::Softer => "SfBL",
                        },
                    ),
                );
                let _ = d.push("RngL", percent_value(s.range));
                let _ = d.push("Jitter", percent_value(s.jitter));
                let _ = d.push(
                    "Slct",
                    enumerated_value(
                        "BESl",
                        match s.source {
                            GlowSource::Center => "Ctr ",
                            GlowSource::Edge => "Edgs",
                        },
                    ),
                );
                let _ = top.push("OrGl", Value::Descriptor(d));
                wrote = true;
            }
            FillStyle::Gradient(_) | FillStyle::Pattern(_) => {
                unmapped.push("outer glow".into());
            }
        }
    }
    if effects.inner_glow.is_some() {
        unmapped.push("inner glow".into());
    }
    if effects.bevel_emboss.is_some() {
        unmapped.push("bevel and emboss".into());
    }
    if effects.satin.is_some() {
        unmapped.push("satin".into());
    }
    if effects.gradient_overlay.is_some() {
        unmapped.push("gradient overlay".into());
    }
    if effects.pattern_overlay.is_some() {
        unmapped.push("pattern overlay".into());
    }

    if !wrote {
        return None;
    }
    let mut s = crate::bytes::Sink::new();
    s.u32(1); // object version
    s.u32(16); // descriptor version
    top.write(&mut s).ok()?;
    Some((s.into_inner(), unmapped))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::bytes::Sink;

    /// Build an `lfx2` payload the way the tests do: object version,
    /// descriptor version, then one descriptor.
    pub(crate) fn lfx2(body: &Descriptor) -> Vec<u8> {
        let mut s = Sink::new();
        s.u32(1); // object version
        s.u32(16); // descriptor version
        body.write(&mut s).unwrap();
        s.into_inner()
    }

    fn rgb(r: f64, g: f64, b: f64) -> Value {
        let mut c = crate::Descriptor::new("RGBC");
        c.push("Rd  ", Value::Double(r)).unwrap();
        c.push("Grn ", Value::Double(g)).unwrap();
        c.push("Bl  ", Value::Double(b)).unwrap();
        Value::Descriptor(c)
    }

    fn unit_float(unit: &str, value: f64) -> Value {
        Value::UnitFloat {
            unit: unit.as_bytes().try_into().unwrap(),
            value,
        }
    }

    fn blnm(v: &str) -> Value {
        Value::Enumerated {
            type_id: "BlnM".into(),
            value: v.into(),
        }
    }

    fn parsed(body: &Descriptor) -> ImportedEffects {
        let effects = Effects {
            key: *b"lfx2",
            data: lfx2(body),
        };
        import_effects(&effects, &ReadOptions::default()).unwrap()
    }

    #[test]
    fn a_bare_percent_value_above_one_is_refused_not_divided() {
        // Photoshop writes `#Prc`; a unit-less Double above 1.0 is refused
        // rather than silently read as 150% — the whole effect becomes
        // unmapped and is named, never half-mapped with a guessed opacity.
        let mut d = crate::Descriptor::new("DrSh");
        d.push("enab", crate::descriptor::Value::Bool(true))
            .unwrap();
        d.push("opacity", crate::descriptor::Value::Double(1.5))
            .unwrap();
        assert_eq!(percent(&d, "opacity"), None);
        // And a bare fraction at or below 1.0 is the honest fallback.
        let mut d = crate::Descriptor::new("DrSh");
        d.push("enab", crate::descriptor::Value::Bool(true))
            .unwrap();
        d.push("opacity", crate::descriptor::Value::Double(0.5))
            .unwrap();
        assert_eq!(percent(&d, "opacity"), Some(0.5));
    }

    #[test]
    fn a_full_drop_shadow_maps_every_required_field() {
        let mut drsh = crate::Descriptor::new("DrSh");
        drsh.push("enab", Value::Bool(true)).unwrap();
        drsh.push("Md  ", blnm("Mltp")).unwrap();
        drsh.push("Clr ", rgb(255.0, 128.0, 0.0)).unwrap();
        drsh.push("opacity", unit_float("#Prc", 75.0)).unwrap();
        drsh.push("lagl", unit_float("#Ang", 120.0)).unwrap();
        drsh.push("uglg", Value::Bool(false)).unwrap();
        drsh.push("Dstn", unit_float("#Pxl", 10.0)).unwrap();
        drsh.push("blur", unit_float("#Pxl", 20.0)).unwrap();
        drsh.push("Ckmt", unit_float("#Pxl", 5.0)).unwrap();
        drsh.push("Nose", unit_float("#Prc", 12.5)).unwrap();
        drsh.push("layerConceals", Value::Bool(true)).unwrap();

        let out = parsed(&{
            let mut top = crate::Descriptor::new("Lfx2");
            top.push("DrSh", Value::Descriptor(drsh)).unwrap();
            top
        });
        assert!(out.unmapped.is_empty(), "{:?}", out.unmapped);
        let s = out.effects.drop_shadow.unwrap();
        assert_eq!(s.blend_mode, BlendMode::Multiply);
        assert_eq!(s.opacity, 0.75);
        assert_eq!(s.angle_deg, 120.0);
        assert!(!s.use_global_light);
        assert_eq!(s.distance_px, 10.0);
        assert_eq!(s.size_px, 20.0);
        assert_eq!(s.spread, 0.25);
        assert_eq!(s.noise, 0.125);
        assert!(s.knockout);
        // 255, 128, 0 stored gamma-encoded — the raw bytes over 255, the
        // same convention the 8-bit pixel path uses.
        assert_eq!(s.color[3], 1.0);
        assert!((s.color[0] - 1.0).abs() < 1e-6);
        assert!((s.color[1] - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(s.color[2], 0.0);
    }

    #[test]
    fn a_disabled_effect_is_absent_and_silent_and_the_master_toggle_is_kept() {
        let mut top = crate::Descriptor::new("Lfx2");
        let mut drsh = crate::Descriptor::new("DrSh");
        drsh.push("enab", Value::Bool(false)).unwrap();
        drsh.push("Md  ", blnm("Mltp")).unwrap();
        drsh.push("Clr ", rgb(0.0, 0.0, 0.0)).unwrap();
        drsh.push("opacity", unit_float("#Prc", 50.0)).unwrap();
        drsh.push("lagl", unit_float("#Ang", 90.0)).unwrap();
        drsh.push("Dstn", unit_float("#Pxl", 4.0)).unwrap();
        drsh.push("blur", unit_float("#Pxl", 8.0)).unwrap();
        top.push("DrSh", Value::Descriptor(drsh)).unwrap();
        top.push("masterFXSwitch", Value::Bool(false)).unwrap();
        top.push("Scl ", unit_float("#Prc", 50.0)).unwrap();

        let out = parsed(&top);
        assert!(out.unmapped.is_empty(), "{:?}", out.unmapped);
        assert!(!out.effects.enabled);
        assert!(out.effects.drop_shadow.is_none());
        assert!(out.effects.is_empty());
    }

    #[test]
    fn scale_multiplies_the_pixel_lengths_and_spread_stays_a_fraction() {
        let mut top = crate::Descriptor::new("Lfx2");
        let mut drsh = crate::Descriptor::new("DrSh");
        drsh.push("Md  ", blnm("Nrml")).unwrap();
        drsh.push("Clr ", rgb(0.0, 0.0, 0.0)).unwrap();
        drsh.push("opacity", unit_float("#Prc", 100.0)).unwrap();
        drsh.push("lagl", unit_float("#Ang", 30.0)).unwrap();
        drsh.push("Dstn", unit_float("#Pxl", 6.0)).unwrap();
        drsh.push("blur", unit_float("#Pxl", 12.0)).unwrap();
        drsh.push("Ckmt", unit_float("#Pxl", 3.0)).unwrap();
        top.push("DrSh", Value::Descriptor(drsh)).unwrap();
        top.push("Scl ", unit_float("#Prc", 200.0)).unwrap();

        let out = parsed(&top);
        let s = out.effects.drop_shadow.unwrap();
        assert_eq!(s.distance_px, 12.0);
        assert_eq!(s.size_px, 24.0);
        assert_eq!(s.spread, 0.25, "the ratio is scale-invariant");
    }

    #[test]
    fn missing_required_fields_leave_the_effect_unmapped_and_named() {
        let mut top = crate::Descriptor::new("Lfx2");
        let mut partial = crate::Descriptor::new("DrSh");
        partial.push("enab", Value::Bool(true)).unwrap();
        partial.push("Md  ", blnm("Nrml")).unwrap();
        // No colour, no geometry: nothing may be invented for it.
        partial.push("opacity", unit_float("#Prc", 50.0)).unwrap();
        top.push("DrSh", Value::Descriptor(partial)).unwrap();
        // A kind this build knows it does not model at all…
        top.push("ChFX", Value::Descriptor(crate::Descriptor::new("ChFX")))
            .unwrap();
        // …and one it has no name for.
        top.push("ZzzZ", Value::Descriptor(crate::Descriptor::new("ZzzZ")))
            .unwrap();

        let out = parsed(&top);
        assert!(out.effects.is_empty());
        assert_eq!(
            out.unmapped,
            vec![
                "drop shadow".to_string(),
                "satin".to_string(),
                "an effect this build does not know (ZzzZ)".to_string(),
            ]
        );
    }

    #[test]
    fn overlay_glow_and_solid_stroke_map_their_own_field_sets() {
        let mut top = crate::Descriptor::new("Lfx2");

        let mut sofi = crate::Descriptor::new("SoFi");
        sofi.push("Md  ", blnm("Ovrl")).unwrap();
        sofi.push("Clr ", rgb(64.0, 64.0, 64.0)).unwrap();
        sofi.push("Opct", unit_float("#Prc", 80.0)).unwrap();
        top.push("SoFi", Value::Descriptor(sofi)).unwrap();

        let mut ogl = crate::Descriptor::new("OrGl");
        ogl.push("Md  ", blnm("Scrn")).unwrap();
        ogl.push("Clr ", rgb(255.0, 255.0, 0.0)).unwrap();
        ogl.push("Opct", unit_float("#Prc", 60.0)).unwrap();
        ogl.push("blur", unit_float("#Pxl", 10.0)).unwrap();
        ogl.push("Ckmt", unit_float("#Pxl", 0.0)).unwrap();
        ogl.push("GlwT", {
            Value::Enumerated {
                type_id: "BETe".into(),
                value: "PrBL".into(),
            }
        })
        .unwrap();
        top.push("OrGl", Value::Descriptor(ogl)).unwrap();

        let mut frfx = crate::Descriptor::new("FrFX");
        frfx.push("Md  ", blnm("Nrml")).unwrap();
        frfx.push("Clr ", rgb(0.0, 0.0, 255.0)).unwrap();
        frfx.push("Opct", unit_float("#Prc", 100.0)).unwrap();
        frfx.push("Sz  ", unit_float("#Pxl", 3.0)).unwrap();
        frfx.push("PntT", {
            Value::Enumerated {
                type_id: "FrFl".into(),
                value: "SClr".into(),
            }
        })
        .unwrap();
        frfx.push("Styl", {
            Value::Enumerated {
                type_id: "FStl".into(),
                value: "InsF".into(),
            }
        })
        .unwrap();
        frfx.push("overprint", Value::Bool(true)).unwrap();
        top.push("FrFX", Value::Descriptor(frfx)).unwrap();

        let out = parsed(&top);
        assert!(out.unmapped.is_empty(), "{:?}", out.unmapped);
        let o = out.effects.color_overlay.unwrap();
        assert_eq!(o.blend_mode, BlendMode::Overlay);
        assert_eq!(o.opacity, 0.8);
        let g = out.effects.outer_glow.unwrap();
        assert_eq!(g.blend_mode, BlendMode::Screen);
        assert_eq!(g.opacity, 0.6);
        assert_eq!(g.size_px, 10.0);
        assert_eq!(g.technique, GlowTechnique::Precise);
        assert!(matches!(g.fill, FillStyle::Solid(_)));
        let s = out.effects.stroke.unwrap();
        assert_eq!(s.size_px, 3.0);
        assert_eq!(s.position, StrokePosition::Inside);
        assert!(s.overprint);
    }

    #[test]
    fn a_gradient_stroke_is_not_solid_so_it_is_named_rather_than_half_mapped() {
        let mut top = crate::Descriptor::new("Lfx2");
        let mut frfx = crate::Descriptor::new("FrFX");
        frfx.push("Md  ", blnm("Nrml")).unwrap();
        frfx.push("Opct", unit_float("#Prc", 100.0)).unwrap();
        frfx.push("Sz  ", unit_float("#Pxl", 3.0)).unwrap();
        frfx.push("PntT", {
            Value::Enumerated {
                type_id: "FrFl".into(),
                value: "GrFl".into(),
            }
        })
        .unwrap();
        top.push("FrFX", Value::Descriptor(frfx)).unwrap();

        let out = parsed(&top);
        assert!(out.effects.stroke.is_none());
        assert_eq!(out.unmapped, vec!["stroke".to_string()]);
    }

    #[test]
    fn the_blnm_table_covers_every_blend_mode_the_model_has() {
        // Every short-form key must decode, and every mode the model defines
        // must be reachable by at least one spelling.
        for (key, mode) in [
            ("Nrml", BlendMode::Normal),
            ("Dslv", BlendMode::Dissolve),
            ("Drkn", BlendMode::Darken),
            ("Mltp", BlendMode::Multiply),
            ("CBrn", BlendMode::ColorBurn),
            ("Lmbs", BlendMode::LinearBurn),
            ("dkCl", BlendMode::DarkerColor),
            ("Lghn", BlendMode::Lighten),
            ("Scrn", BlendMode::Screen),
            ("CDdg", BlendMode::ColorDodge),
            ("lddg", BlendMode::LinearDodge),
            ("lgCl", BlendMode::LighterColor),
            ("Ovrl", BlendMode::Overlay),
            ("SftL", BlendMode::SoftLight),
            ("HrdL", BlendMode::HardLight),
            ("vLit", BlendMode::VividLight),
            ("lLit", BlendMode::LinearLight),
            ("pLit", BlendMode::PinLight),
            ("HrdM", BlendMode::HardMix),
            ("Dfrn", BlendMode::Difference),
            ("Xclu", BlendMode::Exclusion),
            ("Sbtr", BlendMode::Subtract),
            ("H   ", BlendMode::Hue),
            ("Strt", BlendMode::Saturation),
            ("Clr ", BlendMode::Color),
            ("Lmns", BlendMode::Luminosity),
        ] {
            assert_eq!(blend_from_blnm(key), Some(mode), "{key}");
        }
        assert_eq!(blend_from_blnm("nonsense"), None);
    }

    #[test]
    fn a_non_lfx2_block_still_reads_as_none() {
        let effects = Effects {
            key: *b"lrFX",
            data: Vec::new(),
        };
        assert!(import_effects(&effects, &ReadOptions::default()).is_none());
    }
}
