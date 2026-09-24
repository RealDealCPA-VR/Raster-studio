//! The `lfx2` layer-effect descriptor, decoded into
//! [`layer_model::LayerEffects`] parameters.
//!
//! Card 075: the four effects the thumbnail workflow requires — drop shadow
//! (`DrSh`), solid-colour stroke (`FrFX`), solid colour overlay (`SoFi`,
//! accepted under the older `SoCo` key too) and outer glow (`OrGl`, accepted
//! under `OglD` as well) — map into editable native parameters. W11-B: inner
//! shadow, inner glow, bevel and emboss, satin, gradient overlay, contours and
//! any `...Multi` repeated-effect lists present in this descriptor map too,
//! through [`map_rest`] (`effects_rest.rs`), the one mapping the `.asl`
//! import shares (the separate `lmfx` block Photoshop CC writes for repeated
//! effects is not read). [`export_effects`] writes the ten primary slots back;
//! the extra instances of a repeated effect are named, not written. Anything
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

#[path = "effects_rest.rs"]
mod rest;
pub use rest::{map_rest, struck};

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
            // W11-B: named here, then mapped (and struck) by [`map_rest`]
            // below — one mapping shared with the `.asl` import.
            "IrSh" => out.unmapped.push("inner shadow".into()),
            "IrGl" => out.unmapped.push("inner glow".into()),
            "ebbl" => out.unmapped.push("bevel and emboss".into()),
            "ChFX" => out.unmapped.push("satin".into()),
            "GrFl" | "Grdf" => out.unmapped.push("gradient overlay".into()),
            // W8-D: `patternFill` is the key Photoshop writes; a caller
            // resolves it against the file's patterns with
            // [`crate::pattern::pattern_overlay`].
            "patternFill" | "PtFl" | "PttR" => out.unmapped.push("pattern overlay".into()),
            // The two block-level keys above, and anything unrecognised.
            "Scl " | "masterFXSwitch" => {}
            other => out
                .unmapped
                .push(format!("an effect this build does not know ({other})")),
        }
    }
    map_rest(&descriptor, &mut out.effects, &mut out.unmapped);
    Some(out)
}

/// The `BlnM` enumerated values a descriptor carries for blend modes.
///
/// Photoshop writes the short four-character codes; some exporters spell the
/// remaining modes out in words, so both spellings are accepted.
pub(crate) fn blend_from_blnm(value: &str) -> Option<BlendMode> {
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
/// 080, W11-B) — the exact inverse of [`import_effects`] for every kind this
/// build maps, with the same keys and units its parsers read.
///
/// Returns the bytes plus the human kind names that could NOT be written
/// (an effect the model carries but whose descriptor form this writer does
/// not produce, e.g. a gradient-filled glow). An all-unmapped result
/// returns `None` so the caller keeps its existing "not imported" note
/// instead of writing a meaningless block.
pub fn export_effects(effects: &LayerEffects) -> Option<(Vec<u8>, Vec<String>)> {
    export_impl(effects, None)
}

/// W9-M: [`export_effects`] that also writes a pattern overlay whose pattern
/// carries its own pixels, as a `patternFill` descriptor naming the pattern,
/// and returns that pattern so the caller can put it in the document's `Patt`
/// block (the block the reference resolves against —
/// [`crate::pattern::encode_block`]). A pattern overlay with no pixels (only
/// an asset id) is still named as unmapped.
pub fn export_effects_with_patterns(
    effects: &LayerEffects,
) -> Option<(Vec<u8>, Vec<String>, Vec<crate::pattern::PsdPattern>)> {
    let mut patterns = Vec::new();
    let (data, unmapped) = export_impl(effects, Some(&mut patterns))?;
    Some((data, unmapped, patterns))
}

/// A stable id for a pattern, from its content, so the same pattern used
/// twice is written once.
///
/// The same spelling [`crate::fill::encode_pattern_fill`] uses, so a pattern
/// that fills one layer and overlays another is written once.
pub fn pattern_id(tile: &layer_model::effects::PatternTile) -> String {
    format!(
        "rs-{:016x}-{}x{}",
        tile.content_hash(),
        tile.width(),
        tile.height()
    )
}

fn export_impl(
    effects: &LayerEffects,
    patterns: Option<&mut Vec<crate::pattern::PsdPattern>>,
) -> Option<(Vec<u8>, Vec<String>)> {
    let mut top = crate::Descriptor::new("Lfx2");
    let _ = top.push("masterFXSwitch", Value::Bool(effects.enabled));
    let _ = top.push("Scl ", percent_value(1.0));
    let mut unmapped: Vec<String> = Vec::new();
    let mut wrote = false;

    let contours = &effects.extras.contours;
    if let Some(s) = &effects.drop_shadow {
        let d = rest::shadow_descriptor("DrSh", s, &contours.drop_shadow);
        let _ = top.push("DrSh", Value::Descriptor(d));
        wrote = true;
    }
    // W11-B: the five kinds past card 080's four, written as the exact
    // inverse of [`map_rest`].
    if let Some(s) = &effects.inner_shadow {
        let d = rest::shadow_descriptor("IrSh", s, &contours.inner_shadow);
        let _ = top.push("IrSh", Value::Descriptor(d));
        wrote = true;
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
                let d = rest::glow_descriptor(false, s, *color, &contours.outer_glow);
                let _ = top.push("OrGl", Value::Descriptor(d));
                wrote = true;
            }
            FillStyle::Gradient(_) | FillStyle::Pattern(_) => {
                unmapped.push("outer glow".into());
            }
        }
    }
    if let Some(s) = &effects.inner_glow {
        match &s.fill {
            FillStyle::Solid(color) => {
                let d = rest::glow_descriptor(true, s, *color, &contours.inner_glow);
                let _ = top.push("IrGl", Value::Descriptor(d));
                wrote = true;
            }
            FillStyle::Gradient(_) | FillStyle::Pattern(_) => {
                unmapped.push("inner glow".into());
            }
        }
    }
    if let Some(b) = &effects.bevel_emboss {
        let d = rest::bevel_descriptor(b, &contours.bevel);
        let _ = top.push("ebbl", Value::Descriptor(d));
        wrote = true;
    }
    if let Some(s) = &effects.satin {
        let _ = top.push("ChFX", Value::Descriptor(rest::satin_descriptor(s)));
        wrote = true;
    }
    if let Some(g) = &effects.gradient_overlay {
        let _ = top.push(
            "GrFl",
            Value::Descriptor(rest::gradient_overlay_descriptor(g)),
        );
        if g.offset_px != [0.0, 0.0] {
            // `Ofst` is a percentage of the layer's box, which this writer
            // does not know; the offset is named rather than dropped.
            unmapped.push("gradient overlay offset".into());
        }
        wrote = true;
    }
    if let Some(overlay) = &effects.pattern_overlay {
        match (patterns, overlay.pattern.tile.as_ref()) {
            (Some(out), Some(tile)) => {
                let id = pattern_id(tile);
                let fill = &overlay.pattern;
                let mut d = crate::Descriptor::new("patternFill");
                let _ = d.push("enab", Value::Bool(true));
                let _ = d.push("Md  ", enumerated_value("BlnM", blnm(overlay.blend_mode)));
                let _ = d.push("Opct", percent_value(overlay.opacity));
                let mut p = crate::Descriptor::new("Ptrn");
                let _ = p.push("Nm  ", Value::Text(tile.name().to_string()));
                let _ = p.push("Idnt", Value::Text(id.clone()));
                let _ = d.push("Ptrn", Value::Descriptor(p));
                let _ = d.push("Angl", angle_value(fill.angle_deg));
                let _ = d.push("Scl ", percent_value(fill.scale));
                let _ = d.push("Algn", Value::Bool(fill.link_with_layer));
                let mut phase = crate::Descriptor::new("Pnt ");
                let _ = phase.push("Hrzn", Value::Double(f64::from(fill.offset_px[0])));
                let _ = phase.push("Vrtc", Value::Double(f64::from(fill.offset_px[1])));
                let _ = d.push("phase", Value::Descriptor(phase));
                let _ = top.push("patternFill", Value::Descriptor(d));
                if !out.iter().any(|p| p.id == id) {
                    out.push(crate::pattern::PsdPattern {
                        name: tile.name().to_string(),
                        id,
                        width: tile.width(),
                        height: tile.height(),
                        rgba8: tile.rgba8().to_vec(),
                    });
                }
                wrote = true;
            }
            _ => unmapped.push("pattern overlay".into()),
        }
    }

    // W11-B round 2: the import decodes Photoshop CC's `...Multi` lists
    // into `extras`, but this writer emits one instance per kind. The
    // extra instances are named, never silently dropped.
    let extras = &effects.extras;
    for (count, name) in [
        (extras.drop_shadows.len(), "repeated drop shadow"),
        (extras.inner_shadows.len(), "repeated inner shadow"),
        (extras.strokes.len(), "repeated stroke"),
        (extras.color_overlays.len(), "repeated colour overlay"),
        (extras.gradient_overlays.len(), "repeated gradient overlay"),
    ] {
        if count > 0 {
            unmapped.push(name.into());
        }
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

#[cfg(test)]
mod w9m_pattern_export_tests {
    use super::*;
    use layer_model::effects::{PatternFill, PatternOverlayEffect, PatternTile};

    #[test]
    fn a_pattern_overlay_exports_as_a_pattern_fill_its_library_resolves() {
        let tile = PatternTile::new("Dots", 1, 2, vec![1, 2, 3, 255, 4, 5, 6, 128]).unwrap();
        let effects = LayerEffects {
            pattern_overlay: Some(PatternOverlayEffect {
                opacity: 0.25,
                pattern: PatternFill {
                    tile: Some(tile.clone()),
                    scale: 1.5,
                    angle_deg: 30.0,
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        // Without a pattern sink the overlay stays named, as before.
        assert!(export_effects(&effects).is_none());
        let (data, unmapped, patterns) = export_effects_with_patterns(&effects).unwrap();
        assert!(unmapped.is_empty(), "{unmapped:?}");
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].id, pattern_id(&tile));
        let library = crate::pattern::PatternLibrary {
            patterns,
            refused: Vec::new(),
        };
        let fx = Effects {
            key: *b"lfx2",
            data,
        };
        let overlay =
            crate::pattern::pattern_overlay(&fx, &ReadOptions::default(), &library).unwrap();
        assert!((overlay.opacity - 0.25).abs() < 1e-6);
        assert!((overlay.pattern.scale - 1.5).abs() < 1e-6);
        assert!((overlay.pattern.angle_deg - 30.0).abs() < 1e-6);
        assert_eq!(overlay.pattern.tile.unwrap().rgba8(), tile.rgba8());
    }
}

#[cfg(test)]
mod w11b_rest_effect_tests {
    //! W11-B: inner shadow, inner glow, bevel and emboss, satin and gradient
    //! overlay through a whole `.psd` file (written by [`export_effects`],
    //! saved by [`crate::write`], read back by [`crate::read`] and decoded by
    //! [`import_effects`]) with every parameter intact.
    use super::*;
    use crate::{PsdFile, PsdHeader, PsdLayer, Rect, TaggedBlock};
    use layer_model::effects::{
        BevelDirection, BevelEffect, BevelStyle, BevelTechnique, Contour, ContourPreset, Gradient,
        GradientOverlayEffect, GradientStop, GradientStyle, PatternFill, PatternOverlayEffect,
        PatternTile, SatinEffect,
    };

    /// Save `effects` on a layer of a real `.psd`, read the file back, and
    /// decode the layer's block, the pattern overlay resolved against the
    /// file's `Patt` block as the import path does.
    fn through_psd(effects: &LayerEffects) -> (LayerEffects, Vec<String>, Vec<String>) {
        let (data, not_written, patterns) =
            export_effects_with_patterns(effects).expect("the block is written");
        let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
        let mut layer = PsdLayer::raster("fx", Rect::new(0, 0, 4, 4));
        layer.set_rgba8(&[200u8; 4 * 4 * 4]).unwrap();
        layer.effects = Some(Effects {
            key: *b"lfx2",
            data,
        });
        file.layers.push(layer);
        if !patterns.is_empty() {
            file.extra.push(TaggedBlock::new(
                *b"Patt",
                crate::pattern::encode_block(&patterns),
            ));
        }
        let bytes = crate::write(&file).unwrap();
        let back = crate::read(&bytes).unwrap();
        let opts = ReadOptions::default();
        let fx = back.layers[0].effects.as_ref().expect("the lfx2 block");
        let imported = import_effects(fx, &opts).expect("the block decodes");
        let mut decoded = imported.effects;
        let mut unmapped = imported.unmapped;
        let library = crate::pattern::PatternLibrary::read(&back, &opts);
        if let Some(overlay) = crate::pattern::pattern_overlay(fx, &opts, &library) {
            decoded.pattern_overlay = Some(overlay);
            struck(&mut unmapped, "pattern overlay");
        }
        (decoded, not_written, unmapped)
    }

    fn custom_contour() -> Contour {
        Contour {
            preset: ContourPreset::Custom,
            points: vec![[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]],
        }
    }

    fn inner_shadow() -> ShadowEffect {
        ShadowEffect {
            blend_mode: BlendMode::ColorBurn,
            color: [0.5, 0.0, 1.0, 1.0],
            opacity: 0.4,
            angle_deg: 45.0,
            use_global_light: false,
            distance_px: 6.0,
            spread: 0.25,
            size_px: 8.0,
            noise: 0.1,
            knockout: false,
        }
    }

    fn inner_glow() -> GlowEffect {
        GlowEffect {
            blend_mode: BlendMode::Screen,
            fill: FillStyle::Solid([1.0, 0.5, 0.0, 1.0]),
            opacity: 0.6,
            noise: 0.2,
            technique: GlowTechnique::Precise,
            spread: 0.5,
            size_px: 12.0,
            source: GlowSource::Center,
            range: 0.75,
            jitter: 0.25,
        }
    }

    fn bevel() -> BevelEffect {
        BevelEffect {
            style: BevelStyle::PillowEmboss,
            technique: BevelTechnique::ChiselSoft,
            direction: BevelDirection::Down,
            depth: 2.5,
            size_px: 9.0,
            soften_px: 3.0,
            angle_deg: 60.0,
            altitude_deg: 45.0,
            use_global_light: false,
            highlight_mode: BlendMode::LinearDodge,
            highlight_color: [1.0, 1.0, 0.5, 1.0],
            highlight_opacity: 0.8,
            shadow_mode: BlendMode::Darken,
            shadow_color: [0.0, 0.0, 0.5, 1.0],
            shadow_opacity: 0.3,
        }
    }

    fn satin() -> SatinEffect {
        SatinEffect {
            blend_mode: BlendMode::Overlay,
            color: [0.0, 1.0, 0.5, 1.0],
            opacity: 0.7,
            angle_deg: 33.0,
            distance_px: 5.0,
            size_px: 7.0,
            invert: false,
        }
    }

    fn gradient_overlay() -> GradientOverlayEffect {
        GradientOverlayEffect {
            blend_mode: BlendMode::SoftLight,
            opacity: 0.8,
            gradient: Gradient {
                stops: vec![
                    GradientStop {
                        position: 0.0,
                        color: [1.0, 0.0, 0.0, 1.0],
                        midpoint: 0.5,
                    },
                    GradientStop {
                        position: 0.25,
                        color: [0.0, 0.5, 1.0, 1.0],
                        midpoint: 0.4,
                    },
                    GradientStop {
                        position: 1.0,
                        color: [0.0, 0.0, 1.0, 1.0],
                        midpoint: 0.5,
                    },
                ],
                alpha_stops: vec![
                    GradientStop {
                        position: 0.0,
                        color: [1.0, 1.0, 1.0, 1.0],
                        midpoint: 0.5,
                    },
                    GradientStop {
                        position: 0.75,
                        color: [1.0, 1.0, 1.0, 0.4],
                        midpoint: 0.6,
                    },
                ],
                smoothness: 0.5,
            },
            style: GradientStyle::Radial,
            reverse: true,
            align_with_layer: false,
            angle_deg: 30.0,
            scale: 1.5,
            offset_px: [0.0, 0.0],
            dither: true,
        }
    }

    #[test]
    fn an_inner_shadow_round_trips_through_a_psd_with_its_contour() {
        let mut fx = LayerEffects {
            inner_shadow: Some(inner_shadow()),
            ..Default::default()
        };
        fx.extras.contours.inner_shadow = Contour::preset(ContourPreset::Cone);
        let (back, not_written, unmapped) = through_psd(&fx);
        assert!(not_written.is_empty(), "{not_written:?}");
        assert!(unmapped.is_empty(), "{unmapped:?}");
        assert_eq!(back, fx);
    }

    #[test]
    fn an_inner_glow_round_trips_through_a_psd_with_its_source_and_contour() {
        let mut fx = LayerEffects {
            inner_glow: Some(inner_glow()),
            ..Default::default()
        };
        fx.extras.contours.inner_glow = custom_contour();
        let (back, not_written, unmapped) = through_psd(&fx);
        assert!(not_written.is_empty(), "{not_written:?}");
        assert!(unmapped.is_empty(), "{unmapped:?}");
        assert_eq!(back, fx);
        // The edge source, the other value, survives too.
        let mut edge = fx.clone();
        edge.inner_glow.as_mut().unwrap().source = GlowSource::Edge;
        assert_eq!(through_psd(&edge).0, edge);
    }

    #[test]
    fn a_bevel_and_emboss_round_trips_through_a_psd_with_its_gloss_contour() {
        let mut fx = LayerEffects {
            bevel_emboss: Some(bevel()),
            ..Default::default()
        };
        fx.extras.contours.bevel = Contour::preset(ContourPreset::Gaussian);
        let (back, not_written, unmapped) = through_psd(&fx);
        assert!(not_written.is_empty(), "{not_written:?}");
        assert!(unmapped.is_empty(), "{unmapped:?}");
        assert_eq!(back, fx);
        // Every style, technique and direction value survives.
        for style in [
            BevelStyle::InnerBevel,
            BevelStyle::OuterBevel,
            BevelStyle::Emboss,
            BevelStyle::PillowEmboss,
            BevelStyle::StrokeEmboss,
        ] {
            for technique in [
                BevelTechnique::SmoothBevel,
                BevelTechnique::ChiselHard,
                BevelTechnique::ChiselSoft,
            ] {
                for direction in [BevelDirection::Up, BevelDirection::Down] {
                    let mut one = fx.clone();
                    let b = one.bevel_emboss.as_mut().unwrap();
                    (b.style, b.technique, b.direction) = (style, technique, direction);
                    assert_eq!(through_psd(&one).0, one);
                }
            }
        }
    }

    #[test]
    fn a_satin_round_trips_through_a_psd() {
        let fx = LayerEffects {
            satin: Some(satin()),
            ..Default::default()
        };
        let (back, not_written, unmapped) = through_psd(&fx);
        assert!(not_written.is_empty(), "{not_written:?}");
        assert!(unmapped.is_empty(), "{unmapped:?}");
        assert_eq!(back, fx);
    }

    #[test]
    fn a_gradient_overlay_round_trips_through_a_psd_with_its_stops() {
        let fx = LayerEffects {
            gradient_overlay: Some(gradient_overlay()),
            ..Default::default()
        };
        let (back, not_written, unmapped) = through_psd(&fx);
        assert!(not_written.is_empty(), "{not_written:?}");
        assert!(unmapped.is_empty(), "{unmapped:?}");
        assert_eq!(back, fx);
        // An offset has no descriptor form here: it is named, not dropped
        // silently.
        let mut offset = fx.clone();
        offset.gradient_overlay.as_mut().unwrap().offset_px = [4.0, 0.0];
        let (_, not_written, _) = through_psd(&offset);
        assert_eq!(not_written, ["gradient overlay offset"]);
    }

    #[test]
    fn a_layer_with_all_ten_effects_reimports_all_ten() {
        let tile = PatternTile::new("Dots", 1, 2, vec![1, 2, 3, 255, 4, 5, 6, 128]).unwrap();
        let mut fx = LayerEffects {
            enabled: true,
            drop_shadow: Some(ShadowEffect {
                blend_mode: BlendMode::Multiply,
                color: [0.0, 0.0, 0.0, 1.0],
                opacity: 0.75,
                angle_deg: 120.0,
                use_global_light: true,
                distance_px: 5.0,
                spread: 0.5,
                size_px: 4.0,
                noise: 0.0,
                knockout: true,
            }),
            inner_shadow: Some(inner_shadow()),
            outer_glow: Some(GlowEffect {
                source: GlowSource::Edge,
                ..inner_glow()
            }),
            inner_glow: Some(inner_glow()),
            bevel_emboss: Some(bevel()),
            satin: Some(satin()),
            color_overlay: Some(ColorOverlayEffect {
                blend_mode: BlendMode::Color,
                color: [1.0, 0.5, 0.0, 1.0],
                opacity: 0.5,
            }),
            gradient_overlay: Some(gradient_overlay()),
            pattern_overlay: Some(PatternOverlayEffect {
                blend_mode: BlendMode::Normal,
                opacity: 0.25,
                pattern: PatternFill {
                    tile: Some(tile),
                    scale: 1.5,
                    angle_deg: 30.0,
                    ..Default::default()
                },
            }),
            stroke: Some(StrokeEffect {
                size_px: 3.0,
                position: StrokePosition::Center,
                blend_mode: BlendMode::Normal,
                opacity: 1.0,
                fill: FillStyle::Solid([0.0, 0.5, 0.0, 1.0]),
                overprint: false,
            }),
            ..Default::default()
        };
        fx.extras.contours.drop_shadow = Contour::preset(ContourPreset::Ring);
        fx.extras.contours.inner_shadow = Contour::preset(ContourPreset::Cone);
        fx.extras.contours.outer_glow = Contour::preset(ContourPreset::RoundedSteps);
        fx.extras.contours.inner_glow = custom_contour();
        fx.extras.contours.bevel = Contour::preset(ContourPreset::Gaussian);

        let (back, not_written, unmapped) = through_psd(&fx);
        assert!(not_written.is_empty(), "{not_written:?}");
        assert!(unmapped.is_empty(), "{unmapped:?}");
        let slots = [
            back.drop_shadow.is_some(),
            back.inner_shadow.is_some(),
            back.outer_glow.is_some(),
            back.inner_glow.is_some(),
            back.bevel_emboss.is_some(),
            back.satin.is_some(),
            back.color_overlay.is_some(),
            back.gradient_overlay.is_some(),
            back.pattern_overlay.is_some(),
            back.stroke.is_some(),
        ];
        assert_eq!(slots, [true; 10], "{back:#?}");
        let pattern = back.pattern_overlay.clone().unwrap();
        let want = fx.pattern_overlay.clone().unwrap();
        assert_eq!(pattern.opacity, want.opacity);
        assert_eq!(pattern.pattern.scale, want.pattern.scale);
        assert_eq!(pattern.pattern.angle_deg, want.pattern.angle_deg);
        assert_eq!(
            pattern.pattern.tile.as_ref().map(|t| t.rgba8().to_vec()),
            want.pattern.tile.as_ref().map(|t| t.rgba8().to_vec())
        );
        // Everything else comes back field for field.
        let (mut back, mut fx) = (back, fx);
        back.pattern_overlay = None;
        fx.pattern_overlay = None;
        assert_eq!(back, fx);
    }
    #[test]
    fn the_extra_instances_of_a_repeated_effect_are_named_not_silently_dropped() {
        // Review round 2: the import decodes Photoshop CC's `...Multi`
        // lists into `extras`; the writer emits one instance per kind, so
        // every extra kind must be named in the export report.
        use layer_model::effects::ShadowInstance;
        let extra_shadow = ShadowInstance {
            effect: inner_shadow(),
            contour: Contour::default(),
        };
        let mut fx = LayerEffects {
            drop_shadow: Some(ShadowEffect::default()),
            ..Default::default()
        };
        fx.extras.drop_shadows = vec![extra_shadow.clone(), extra_shadow.clone()];
        fx.extras.inner_shadows = vec![extra_shadow];
        fx.extras.strokes = vec![StrokeEffect::default()];
        fx.extras.color_overlays = vec![ColorOverlayEffect::default()];
        fx.extras.gradient_overlays = vec![GradientOverlayEffect::default()];

        let (back, not_written, _unmapped) = through_psd(&fx);
        assert_eq!(
            not_written,
            vec![
                "repeated drop shadow".to_string(),
                "repeated inner shadow".to_string(),
                "repeated stroke".to_string(),
                "repeated colour overlay".to_string(),
                "repeated gradient overlay".to_string(),
            ]
        );
        // The primary slot is still written and read back.
        assert!(back.drop_shadow.is_some());
    }
    #[test]
    fn a_multi_list_inside_the_lfx2_descriptor_maps_through_a_psd() {
        // Review round 3: the parity matrix says repeated effects map when
        // their `...Multi` list sits inside the `lfx2` descriptor (the
        // separate `lmfx` block is not read). This drives that through a
        // whole `.psd`: written, read back, decoded by `import_effects`.
        let first = ShadowEffect {
            opacity: 0.3,
            ..ShadowEffect::default()
        };
        let second = inner_shadow();
        let mut top = crate::Descriptor::new("Lfx2");
        top.push("masterFXSwitch", Value::Bool(true)).unwrap();
        top.push("Scl ", percent_value(1.0)).unwrap();
        top.push(
            "dropShadowMulti",
            Value::List(vec![
                Value::Descriptor(rest::shadow_descriptor("DrSh", &first, &Contour::default())),
                Value::Descriptor(rest::shadow_descriptor("DrSh", &second, &custom_contour())),
            ]),
        )
        .unwrap();
        let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
        let mut layer = PsdLayer::raster("fx", Rect::new(0, 0, 4, 4));
        layer.set_rgba8(&[200u8; 4 * 4 * 4]).unwrap();
        layer.effects = Some(Effects {
            key: *b"lfx2",
            data: super::tests::lfx2(&top),
        });
        file.layers.push(layer);
        let back = crate::read(&crate::write(&file).unwrap()).unwrap();
        let fx = back.layers[0].effects.as_ref().expect("the lfx2 block");
        let imported = import_effects(fx, &ReadOptions::default()).expect("decodes");
        assert!(imported.unmapped.is_empty(), "{:?}", imported.unmapped);
        let shadow = imported.effects.drop_shadow.expect("the first instance");
        assert!((shadow.opacity - 0.3).abs() < 1e-6);
        let extras = &imported.effects.extras.drop_shadows;
        assert_eq!(extras.len(), 1, "the second instance is an extra");
        assert_eq!(extras[0].effect, second);
        assert_eq!(extras[0].contour, custom_contour());
    }
}
