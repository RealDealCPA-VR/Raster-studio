//! W11-B: the five effect kinds beyond card 075's four — inner shadow
//! (`IrSh`), inner glow (`IrGl`), bevel and emboss (`ebbl`), satin (`ChFX`)
//! and gradient overlay (`GrFl`, accepted under `Grdf` too) — plus the
//! contours (`TrnS`) and Photoshop CC's repeated effects (`...Multi` lists).
//!
//! One mapping for both roads: [`map_rest`] runs inside
//! [`super::import_effects`] (a `.psd` layer's `lfx2` block) and so inside the
//! `.asl` style-library import, which re-frames each style's `Lefx` item as an
//! `lfx2` block. The writers below are its exact inverse, used by
//! [`super::export_effects`]. Key and value spellings are Photoshop's own.
//!
//! | Source key (unit) | Native field |
//! |---|---|
//! | `IrSh` | `inner_shadow`: a drop shadow's fields (see the module above), contour `TrnS` |
//! | `IrGl` | `inner_glow`: an outer glow's fields, source `glwS` (`SrcC` centre, `SrcE` edge), contour `TrnS` |
//! | `ebbl` | `bevel_emboss`: `bvlS` style, `bvlT` technique, `bvlD` direction (`In  `/`Out `), `srgR` depth %, `blur` size px, `Sftn` soften px, `lagl` angle, `Lald` altitude, `uglg`, `hglM`/`hglC`/`hglO` highlight, `sdwM`/`sdwC`/`sdwO` shadow, gloss contour `TrnS` |
//! | `ChFX` | `satin`: `Md  `, `Clr `, `Opct`, `lagl`, `Dstn`, `blur`, `Invr` |
//! | `GrFl`/`Grdf` | `gradient_overlay`: `Md  `, `Opct`, `Grad` (`Grdn`: `Intr` smoothness on 0..=4096, `Clrs`/`Trns` stops with `Lctn` on 0..=4096 and `Mdpn` %), `Type`, `Rvrs`, `Algn`, `Angl`, `Scl `, `Dthr`, `Ofst` (a `Pnt ` of percentages of the layer's box, or of the canvas when `Algn` is off) |
//! | `TrnS` (`ShpC`) | a contour: `Nm  ` names a picker preset, otherwise `Crv ` knots on 0..=255 |
//!
//! Pixel lengths are multiplied by the block's `Scl ` exactly as card 075's
//! four are. W13-B: the gradient overlay's `Ofst` is a percentage of a box
//! the descriptor does not carry; [`super::EffectsContext`] supplies it. With
//! no box, a non-zero offset is named (on read and on write) rather than
//! guessed, and the ramp stays centred.

use super::{
    angle_value, blnm, color_overlay, drop_shadow, enabled, enumerated, enumerated_value, flag,
    outer_glow, percent_value, px_value, rgbc, stroke, EffectsContext,
};
use crate::descriptor::{Descriptor, Value};
use layer_model::effects::{
    BevelDirection, BevelEffect, BevelStyle, BevelTechnique, ColorOverlayEffect, Contour,
    ContourPreset, GlowEffect, GlowSource, Gradient, GradientOverlayEffect, GradientStop,
    GradientStyle, SatinEffect, ShadowEffect, ShadowInstance,
};
use layer_model::{LayerEffects, Rgba};

/// The span Photoshop stores gradient stop locations (and the smoothness,
/// `Intr`) on.
const GRADIENT_SPAN: f32 = 4096.0;

/// Drop the first `name` from the "not imported" list.
pub fn struck(unmapped: &mut Vec<String>, name: &str) {
    if let Some(i) = unmapped.iter().position(|u| u == name) {
        unmapped.remove(i);
    }
}

fn unknown(key: &str) -> String {
    format!("an effect this build does not know ({key})")
}

fn num(d: &Descriptor, key: &str) -> Option<f32> {
    d.number(key).map(|v| v as f32).filter(|v| v.is_finite())
}

/// A mode / colour / opacity triple under the given keys, decoded the way a
/// colour overlay's is. `color` `None` stands in black.
fn paint(
    d: &Descriptor,
    mode: &str,
    color: Option<&str>,
    opacity: &str,
) -> Option<ColorOverlayEffect> {
    let mut sofi = Descriptor::new("SoFi");
    sofi.push("Md  ", d.get(mode)?.clone()).ok()?;
    let clr = match color {
        Some(key) => d.get(key)?.clone(),
        None => rgbc([0.0, 0.0, 0.0, 1.0]),
    };
    sofi.push("Clr ", clr).ok()?;
    sofi.push("Opct", d.get(opacity)?.clone()).ok()?;
    color_overlay(&sofi)
}

/// A contour object (`ShpC`): a named preset where the name is one the
/// picker lists, otherwise the file's own curve (knots on 0..=255).
fn contour(d: &Descriptor, key: &str) -> Contour {
    let Some(c) = d.descriptor(key) else {
        return Contour::default();
    };
    // Photoshop may write a localisation key: "$$$/Contours/...=Cone".
    let name = c.text("Nm  ").unwrap_or_default();
    let name = name.rsplit('=').next().unwrap_or_default().trim();
    if let Some(preset) = preset_named(name) {
        return Contour::preset(preset);
    }
    let points: Vec<[f32; 2]> = match c.get("Crv ") {
        Some(Value::List(knots)) => knots
            .iter()
            .filter_map(|k| match k {
                Value::Descriptor(p) => Some([
                    (num(p, "Hrzn")? / 255.0).clamp(0.0, 1.0),
                    (num(p, "Vrtc")? / 255.0).clamp(0.0, 1.0),
                ]),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    if points.len() < 2 {
        return Contour::default();
    }
    Contour {
        preset: ContourPreset::Custom,
        points,
    }
}

fn preset_named(name: &str) -> Option<ContourPreset> {
    match name {
        "Linear" => Some(ContourPreset::Linear),
        "Cone" => Some(ContourPreset::Cone),
        "Gaussian" => Some(ContourPreset::Gaussian),
        "Ring" => Some(ContourPreset::Ring),
        "Rounded Steps" => Some(ContourPreset::RoundedSteps),
        _ => None,
    }
}

fn shadow(d: &Descriptor, scale: f32) -> Option<ShadowInstance> {
    Some(ShadowInstance {
        effect: drop_shadow(d, scale)?,
        contour: contour(d, "TrnS"),
    })
}

fn inner_glow(d: &Descriptor, scale: f32) -> Option<GlowEffect> {
    let mut glow = outer_glow(d, scale)?;
    glow.source = match enumerated(d, "glwS") {
        Some("SrcC") => GlowSource::Center,
        _ => GlowSource::Edge,
    };
    Some(glow)
}

fn bevel(d: &Descriptor, scale: f32) -> Option<BevelEffect> {
    let hi = paint(d, "hglM", Some("hglC"), "hglO")?;
    let lo = paint(d, "sdwM", Some("sdwC"), "sdwO")?;
    let style = match enumerated(d, "bvlS") {
        Some("InrB") | None => BevelStyle::InnerBevel,
        Some("OtrB") => BevelStyle::OuterBevel,
        Some("Embs") => BevelStyle::Emboss,
        Some("PlEb") => BevelStyle::PillowEmboss,
        Some("strokeEmboss") => BevelStyle::StrokeEmboss,
        Some(_) => return None,
    };
    let technique = match enumerated(d, "bvlT") {
        Some("SfBL") | None => BevelTechnique::SmoothBevel,
        Some("PrBL") => BevelTechnique::ChiselHard,
        Some("Slmt") => BevelTechnique::ChiselSoft,
        Some(_) => return None,
    };
    let direction = match enumerated(d, "bvlD") {
        Some("Out ") => BevelDirection::Down,
        _ => BevelDirection::Up,
    };
    let defaults = BevelEffect::default();
    Some(BevelEffect {
        style,
        technique,
        direction,
        depth: num(d, "srgR").map_or(defaults.depth, |p| (p / 100.0).clamp(0.0, 10.0)),
        size_px: num(d, "blur").map_or(defaults.size_px, |v| v.max(0.0) * scale),
        soften_px: num(d, "Sftn").map_or(0.0, |v| v.max(0.0) * scale),
        angle_deg: num(d, "lagl").unwrap_or(defaults.angle_deg),
        altitude_deg: num(d, "Lald").map_or(defaults.altitude_deg, |v| v.clamp(0.0, 90.0)),
        use_global_light: flag(d, "uglg").unwrap_or(true),
        highlight_mode: hi.blend_mode,
        highlight_color: hi.color,
        highlight_opacity: hi.opacity,
        shadow_mode: lo.blend_mode,
        shadow_color: lo.color,
        shadow_opacity: lo.opacity,
    })
}

fn satin(d: &Descriptor, scale: f32) -> Option<SatinEffect> {
    let p = paint(d, "Md  ", Some("Clr "), "Opct")?;
    let defaults = SatinEffect::default();
    Some(SatinEffect {
        blend_mode: p.blend_mode,
        color: p.color,
        opacity: p.opacity,
        angle_deg: num(d, "lagl").unwrap_or(defaults.angle_deg),
        distance_px: num(d, "Dstn").map_or(defaults.distance_px, |v| v * scale),
        size_px: num(d, "blur").map_or(defaults.size_px, |v| v.max(0.0) * scale),
        invert: flag(d, "Invr").unwrap_or(defaults.invert),
    })
}

/// A gradient stop's colour: `RGBC` (0..=255) or `Grsc` (percent ink).
fn stop_color(c: &Descriptor) -> Option<Rgba> {
    if let (Some(r), Some(g), Some(b)) = (num(c, "Rd  "), num(c, "Grn "), num(c, "Bl  ")) {
        return Some([
            (r / 255.0).clamp(0.0, 1.0),
            (g / 255.0).clamp(0.0, 1.0),
            (b / 255.0).clamp(0.0, 1.0),
            1.0,
        ]);
    }
    let gray = 1.0 - (num(c, "Gry ")? / 100.0).clamp(0.0, 1.0);
    Some([gray, gray, gray, 1.0])
}

/// The descriptors of a list item.
fn descriptors<'a>(d: &'a Descriptor, key: &str) -> Vec<&'a Descriptor> {
    match d.get(key) {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Descriptor(d) => Some(d),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A `Grdn` object with its own stops (`CstS`); a noise gradient has no
/// stops to map. Stop locations are on 0..=4096 and `Intr` is the
/// smoothness on the same span (4096 = 100 %).
pub(super) fn gradient(g: &Descriptor) -> Option<Gradient> {
    if enumerated(g, "GrdF") == Some("ClNs") {
        return None;
    }
    let smoothness = num(g, "Intr").map_or(1.0, |v| (v / GRADIENT_SPAN).clamp(0.0, 1.0));
    let position = |d: &Descriptor| (num(d, "Lctn").unwrap_or(0.0) / GRADIENT_SPAN).clamp(0.0, 1.0);
    let midpoint = |d: &Descriptor| (num(d, "Mdpn").unwrap_or(50.0) / 100.0).clamp(0.0, 1.0);
    let mut stops: Vec<GradientStop> = descriptors(g, "Clrs")
        .into_iter()
        .map(|d| GradientStop {
            position: position(d),
            // Foreground/background stops carry no colour of their own.
            color: d.descriptor("Clr ").and_then(stop_color).unwrap_or(
                match enumerated(d, "Type") {
                    Some("BckC") => [1.0, 1.0, 1.0, 1.0],
                    _ => [0.0, 0.0, 0.0, 1.0],
                },
            ),
            midpoint: midpoint(d),
        })
        .collect();
    if stops.is_empty() {
        return None;
    }
    stops.sort_by(|a, b| a.position.total_cmp(&b.position));
    let mut alpha_stops: Vec<GradientStop> = descriptors(g, "Trns")
        .into_iter()
        .map(|d| GradientStop {
            position: position(d),
            color: [
                1.0,
                1.0,
                1.0,
                (num(d, "Opct").unwrap_or(100.0) / 100.0).clamp(0.0, 1.0),
            ],
            midpoint: midpoint(d),
        })
        .collect();
    alpha_stops.sort_by(|a, b| a.position.total_cmp(&b.position));
    Some(Gradient {
        stops,
        alpha_stops,
        smoothness,
    })
}

/// A gradient overlay's `Ofst` in pixels: `Ok(None)` when it is absent or
/// zero, `Err(())` when it is not zero and its box is not known.
fn gradient_offset(
    d: &Descriptor,
    ctx: EffectsContext<'_>,
    align: bool,
) -> Result<Option<[f32; 2]>, ()> {
    let Some(p) = d.descriptor("Ofst") else {
        return Ok(None);
    };
    let axis = |key: &str| match p.get(key) {
        Some(Value::UnitFloat { unit, value }) if unit.as_slice() == b"#Pxl" => {
            (Some(*value as f32), false)
        }
        Some(Value::UnitFloat { value, .. }) | Some(Value::Double(value)) => {
            (Some(*value as f32), true)
        }
        _ => (None, false),
    };
    let (x, y) = (axis("Hrzn"), axis("Vrtc"));
    let raw = [x.0.unwrap_or(0.0), y.0.unwrap_or(0.0)].map(|v| if v.is_finite() { v } else { 0.0 });
    if raw == [0.0, 0.0] {
        return Ok(None);
    }
    if !x.1 && !y.1 {
        return Ok(Some(raw));
    }
    let b = ctx.offset_box(align).ok_or(())?;
    Ok(Some([
        if x.1 { raw[0] / 100.0 * b[0] } else { raw[0] },
        if y.1 { raw[1] / 100.0 * b[1] } else { raw[1] },
    ]))
}

fn gradient_overlay(d: &Descriptor, ctx: EffectsContext<'_>) -> Option<GradientOverlayEffect> {
    let p = paint(d, "Md  ", None, "Opct")?;
    let style = match enumerated(d, "Type") {
        Some("Lnr ") | None => GradientStyle::Linear,
        Some("Rdl ") => GradientStyle::Radial,
        Some("Angl") => GradientStyle::Angle,
        Some("Rflc") => GradientStyle::Reflected,
        Some("Dmnd") => GradientStyle::Diamond,
        Some(_) => return None,
    };
    let defaults = GradientOverlayEffect::default();
    let align_with_layer = flag(d, "Algn").unwrap_or(true);
    Some(GradientOverlayEffect {
        blend_mode: p.blend_mode,
        opacity: p.opacity,
        gradient: gradient(d.descriptor("Grad")?)?,
        style,
        reverse: flag(d, "Rvrs").unwrap_or(false),
        align_with_layer,
        angle_deg: num(d, "Angl").unwrap_or(defaults.angle_deg),
        scale: num(d, "Scl ").map_or(1.0, |v| (v / 100.0).max(0.01)),
        // W13-B: `Ofst` through the box the caller knows; an offset whose
        // box is not known stays centred and is named by [`map_rest_in`].
        offset_px: gradient_offset(d, ctx, align_with_layer)
            .ok()
            .flatten()
            .unwrap_or([0.0, 0.0]),
        dither: flag(d, "Dthr").unwrap_or(false),
    })
}

/// Name an unreadable gradient overlay offset once.
fn name_offset(d: &Descriptor, ctx: EffectsContext<'_>, unmapped: &mut Vec<String>) {
    let align = flag(d, "Algn").unwrap_or(true);
    let name = "gradient overlay offset";
    if gradient_offset(d, ctx, align).is_err() && !unmapped.iter().any(|u| u == name) {
        unmapped.push(name.into());
    }
}

/// The enabled descriptors of a `...Multi` list, in file order.
fn multi(value: &Value) -> Vec<&Descriptor> {
    match value {
        Value::List(items) => items
            .iter()
            .filter_map(|v| match v {
                Value::Descriptor(d) if enabled(d) => Some(d),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Map everything in an effect block (`lfx2`'s descriptor, or an `.asl`
/// style's `Lefx`) that card 075's four-kind decode leaves out, striking
/// each kind that maps from `unmapped`.
///
/// [`super::import_effects`] calls this on every block it decodes, so a
/// `.psd` layer and an `.asl` style go through the same mapping.
pub fn map_rest(lefx: &Descriptor, fx: &mut LayerEffects, unmapped: &mut Vec<String>) {
    map_rest_in(lefx, fx, unmapped, EffectsContext::default());
}

/// W13-B: [`map_rest`] with what the caller knows about the layer (the
/// file's patterns, the layer's and canvas's boxes).
pub fn map_rest_in(
    lefx: &Descriptor,
    fx: &mut LayerEffects,
    unmapped: &mut Vec<String>,
    ctx: EffectsContext<'_>,
) {
    let scale = num(lefx, "Scl ").map_or(1.0, |s| (s / 100.0).clamp(0.0, 10.0));
    for (key, value) in &lefx.items {
        if let Value::Descriptor(d) = value {
            if !enabled(d) {
                continue;
            }
        }
        match (key.as_str(), value) {
            ("DrSh", Value::Descriptor(d)) => {
                fx.extras.contours.drop_shadow = contour(d, "TrnS");
            }
            ("OrGl" | "OglD", Value::Descriptor(d)) => {
                fx.extras.contours.outer_glow = contour(d, "TrnS");
            }
            ("IrSh", Value::Descriptor(d)) => {
                if let Some(s) = shadow(d, scale) {
                    fx.inner_shadow = Some(s.effect);
                    fx.extras.contours.inner_shadow = s.contour;
                    struck(unmapped, "inner shadow");
                }
            }
            ("IrGl", Value::Descriptor(d)) => {
                if let Some(glow) = inner_glow(d, scale) {
                    fx.inner_glow = Some(glow);
                    fx.extras.contours.inner_glow = contour(d, "TrnS");
                    struck(unmapped, "inner glow");
                }
            }
            ("ebbl", Value::Descriptor(d)) => {
                if let Some(b) = bevel(d, scale) {
                    fx.bevel_emboss = Some(b);
                    // The gloss contour shades the bevel.
                    fx.extras.contours.bevel = contour(d, "TrnS");
                    struck(unmapped, "bevel and emboss");
                }
            }
            ("ChFX", Value::Descriptor(d)) => {
                if let Some(s) = satin(d, scale) {
                    fx.satin = Some(s);
                    struck(unmapped, "satin");
                }
            }
            ("GrFl" | "Grdf", Value::Descriptor(d)) => {
                if let Some(g) = gradient_overlay(d, ctx) {
                    fx.gradient_overlay = Some(g);
                    struck(unmapped, "gradient overlay");
                    name_offset(d, ctx, unmapped);
                }
            }
            // Photoshop CC's repeated effects: the first instance is the
            // primary slot, the rest are the style's extra instances.
            ("dropShadowMulti" | "innerShadowMulti", list) => {
                let found: Vec<ShadowInstance> = multi(list)
                    .into_iter()
                    .filter_map(|d| shadow(d, scale))
                    .collect();
                let mut found = found.into_iter();
                if let Some(first) = found.next() {
                    if key == "dropShadowMulti" {
                        fx.drop_shadow = Some(first.effect);
                        fx.extras.contours.drop_shadow = first.contour;
                        fx.extras.drop_shadows = found.collect();
                    } else {
                        fx.inner_shadow = Some(first.effect);
                        fx.extras.contours.inner_shadow = first.contour;
                        fx.extras.inner_shadows = found.collect();
                    }
                }
                struck(unmapped, &unknown(key));
            }
            ("frameFXMulti", list) => {
                let items = multi(list);
                let total = items.len();
                let found: Vec<_> = items
                    .into_iter()
                    .filter_map(|d| stroke(d, scale, ctx))
                    .collect();
                // W13-B: an instance whose fill does not resolve (a pattern
                // the file does not carry) is named, not silently skipped.
                if found.len() < total {
                    unmapped.push("stroke".into());
                }
                let mut found = found.into_iter();
                if let Some(first) = found.next() {
                    fx.stroke = Some(first);
                    fx.extras.strokes = found.collect();
                }
                struck(unmapped, &unknown(key));
            }
            ("solidFillMulti", list) => {
                let mut found = multi(list).into_iter().filter_map(color_overlay);
                if let Some(first) = found.next() {
                    fx.color_overlay = Some(first);
                    fx.extras.color_overlays = found.collect();
                }
                struck(unmapped, &unknown(key));
            }
            ("gradientFillMulti", list) => {
                let items = multi(list);
                for d in &items {
                    name_offset(d, ctx, unmapped);
                }
                let mut found = items.into_iter().filter_map(|d| gradient_overlay(d, ctx));
                if let Some(first) = found.next() {
                    fx.gradient_overlay = Some(first);
                    fx.extras.gradient_overlays = found.collect();
                }
                struck(unmapped, &unknown(key));
            }
            _ => {}
        }
    }
}

// ----------------------------------------------------------------- writing

fn push(d: &mut Descriptor, key: &str, value: Value) {
    // Every key below is a literal the format can encode.
    let _ = d.push(key, value);
}

/// A contour as the `ShpC` object [`contour`] reads, or `None` for the
/// default (linear) contour, which an absent `TrnS` already means.
fn contour_value(c: &Contour) -> Option<Value> {
    if *c == Contour::default() {
        return None;
    }
    let mut shpc = Descriptor::new("ShpC");
    let name = match c.preset {
        ContourPreset::Linear => "Linear",
        ContourPreset::Cone => "Cone",
        ContourPreset::Gaussian => "Gaussian",
        ContourPreset::Ring => "Ring",
        ContourPreset::RoundedSteps => "Rounded Steps",
        ContourPreset::Custom => "Custom",
    };
    push(&mut shpc, "Nm  ", Value::Text(name.into()));
    if c.preset == ContourPreset::Custom {
        if c.points.len() < 2 {
            return None;
        }
        let knots = c
            .points
            .iter()
            .map(|p| {
                let mut k = Descriptor::new("CrPt");
                push(&mut k, "Hrzn", Value::Double(f64::from(p[0]) * 255.0));
                push(&mut k, "Vrtc", Value::Double(f64::from(p[1]) * 255.0));
                Value::Descriptor(k)
            })
            .collect();
        push(&mut shpc, "Crv ", Value::List(knots));
    }
    Some(Value::Descriptor(shpc))
}

fn push_contour(d: &mut Descriptor, c: &Contour) {
    if let Some(v) = contour_value(c) {
        push(d, "TrnS", v);
    }
}

/// A drop or inner shadow (`class` `DrSh` or `IrSh`): the inverse of
/// [`drop_shadow`] plus the contour.
pub(super) fn shadow_descriptor(class: &str, s: &ShadowEffect, c: &Contour) -> Descriptor {
    let mut d = Descriptor::new(class);
    push(&mut d, "enab", Value::Bool(true));
    push(&mut d, "Md  ", enumerated_value("BlnM", blnm(s.blend_mode)));
    push(&mut d, "Clr ", rgbc(s.color));
    push(&mut d, "opacity", percent_value(s.opacity));
    push(&mut d, "lagl", angle_value(s.angle_deg));
    push(&mut d, "uglg", Value::Bool(s.use_global_light));
    push(&mut d, "Dstn", px_value(s.distance_px));
    push(&mut d, "blur", px_value(s.size_px));
    push(&mut d, "Ckmt", px_value(s.spread * s.size_px));
    push(&mut d, "Nose", percent_value(s.noise));
    push(&mut d, "layerConceals", Value::Bool(s.knockout));
    push_contour(&mut d, c);
    d
}

/// An outer or inner glow with a solid colour: the inverse of
/// [`outer_glow`] (and [`inner_glow`], whose source is `glwS`).
pub(super) fn glow_descriptor(inner: bool, g: &GlowEffect, color: Rgba, c: &Contour) -> Descriptor {
    let mut d = Descriptor::new(if inner { "IrGl" } else { "OrGl" });
    push(&mut d, "enab", Value::Bool(true));
    push(&mut d, "Md  ", enumerated_value("BlnM", blnm(g.blend_mode)));
    push(&mut d, "Clr ", rgbc(color));
    push(&mut d, "Opct", percent_value(g.opacity));
    push(&mut d, "blur", px_value(g.size_px));
    push(&mut d, "Ckmt", px_value(g.spread * g.size_px));
    push(&mut d, "Nose", percent_value(g.noise));
    let technique = match g.technique {
        layer_model::effects::GlowTechnique::Precise => "PrBL",
        layer_model::effects::GlowTechnique::Softer => "SfBL",
    };
    push(&mut d, "GlwT", enumerated_value("BETE", technique));
    push(&mut d, "RngL", percent_value(g.range));
    push(&mut d, "Jitter", percent_value(g.jitter));
    if inner {
        let source = match g.source {
            GlowSource::Center => "SrcC",
            GlowSource::Edge => "SrcE",
        };
        push(&mut d, "glwS", enumerated_value("IGSr", source));
    } else {
        let source = match g.source {
            GlowSource::Center => "Ctr ",
            GlowSource::Edge => "Edgs",
        };
        push(&mut d, "Slct", enumerated_value("BESl", source));
    }
    push_contour(&mut d, c);
    d
}

/// Bevel and emboss: the inverse of [`bevel`].
pub(super) fn bevel_descriptor(b: &BevelEffect, c: &Contour) -> Descriptor {
    let mut d = Descriptor::new("ebbl");
    push(&mut d, "enab", Value::Bool(true));
    push(
        &mut d,
        "hglM",
        enumerated_value("BlnM", blnm(b.highlight_mode)),
    );
    push(&mut d, "hglC", rgbc(b.highlight_color));
    push(&mut d, "hglO", percent_value(b.highlight_opacity));
    push(
        &mut d,
        "sdwM",
        enumerated_value("BlnM", blnm(b.shadow_mode)),
    );
    push(&mut d, "sdwC", rgbc(b.shadow_color));
    push(&mut d, "sdwO", percent_value(b.shadow_opacity));
    let style = match b.style {
        BevelStyle::InnerBevel => "InrB",
        BevelStyle::OuterBevel => "OtrB",
        BevelStyle::Emboss => "Embs",
        BevelStyle::PillowEmboss => "PlEb",
        BevelStyle::StrokeEmboss => "strokeEmboss",
    };
    push(&mut d, "bvlS", enumerated_value("BESl", style));
    let technique = match b.technique {
        BevelTechnique::SmoothBevel => "SfBL",
        BevelTechnique::ChiselHard => "PrBL",
        BevelTechnique::ChiselSoft => "Slmt",
    };
    push(&mut d, "bvlT", enumerated_value("bvlT", technique));
    let direction = match b.direction {
        BevelDirection::Up => "In  ",
        BevelDirection::Down => "Out ",
    };
    push(&mut d, "bvlD", enumerated_value("BESs", direction));
    push(&mut d, "srgR", percent_value(b.depth));
    push(&mut d, "blur", px_value(b.size_px));
    push(&mut d, "Sftn", px_value(b.soften_px));
    push(&mut d, "lagl", angle_value(b.angle_deg));
    push(&mut d, "Lald", angle_value(b.altitude_deg));
    push(&mut d, "uglg", Value::Bool(b.use_global_light));
    push_contour(&mut d, c);
    d
}

/// Satin: the inverse of [`satin`].
pub(super) fn satin_descriptor(s: &SatinEffect) -> Descriptor {
    let mut d = Descriptor::new("ChFX");
    push(&mut d, "enab", Value::Bool(true));
    push(&mut d, "Md  ", enumerated_value("BlnM", blnm(s.blend_mode)));
    push(&mut d, "Clr ", rgbc(s.color));
    push(&mut d, "Opct", percent_value(s.opacity));
    push(&mut d, "lagl", angle_value(s.angle_deg));
    push(&mut d, "Dstn", px_value(s.distance_px));
    push(&mut d, "blur", px_value(s.size_px));
    push(&mut d, "Invr", Value::Bool(s.invert));
    d
}

fn span_position(position: f32) -> Value {
    Value::Integer((position.clamp(0.0, 1.0) * GRADIENT_SPAN).round() as i32)
}

fn midpoint_value(midpoint: f32) -> Value {
    Value::Integer((midpoint.clamp(0.0, 1.0) * 100.0).round() as i32)
}

/// A `Grdn` object with its own stops: the inverse of [`gradient`].
pub(super) fn gradient_descriptor(g: &Gradient) -> Descriptor {
    let mut d = Descriptor::new("Grdn");
    push(&mut d, "Nm  ", Value::Text("Custom".into()));
    push(&mut d, "GrdF", enumerated_value("GrdF", "CstS"));
    push(
        &mut d,
        "Intr",
        Value::Double(f64::from(g.smoothness.clamp(0.0, 1.0) * GRADIENT_SPAN)),
    );
    let stops = g
        .stops
        .iter()
        .map(|s| {
            let mut c = Descriptor::new("Clrt");
            push(&mut c, "Clr ", rgbc(s.color));
            push(&mut c, "Type", enumerated_value("Clry", "UsrS"));
            push(&mut c, "Lctn", span_position(s.position));
            push(&mut c, "Mdpn", midpoint_value(s.midpoint));
            Value::Descriptor(c)
        })
        .collect();
    push(&mut d, "Clrs", Value::List(stops));
    let alpha = g
        .alpha_stops
        .iter()
        .map(|s| {
            let mut t = Descriptor::new("TrnS");
            push(&mut t, "Opct", percent_value(s.color[3]));
            push(&mut t, "Lctn", span_position(s.position));
            push(&mut t, "Mdpn", midpoint_value(s.midpoint));
            Value::Descriptor(t)
        })
        .collect();
    push(&mut d, "Trns", Value::List(alpha));
    d
}

/// Gradient overlay: the inverse of [`gradient_overlay`], without `Ofst` —
/// the caller (`super::gradient_overlay_out`) adds it from the box it knows.
pub(super) fn gradient_overlay_descriptor(g: &GradientOverlayEffect) -> Descriptor {
    let mut d = Descriptor::new("GrFl");
    push(&mut d, "enab", Value::Bool(true));
    push(&mut d, "Md  ", enumerated_value("BlnM", blnm(g.blend_mode)));
    push(&mut d, "Opct", percent_value(g.opacity));
    push(
        &mut d,
        "Grad",
        Value::Descriptor(gradient_descriptor(&g.gradient)),
    );
    let style = match g.style {
        GradientStyle::Linear => "Lnr ",
        GradientStyle::Radial => "Rdl ",
        GradientStyle::Angle => "Angl",
        GradientStyle::Reflected => "Rflc",
        GradientStyle::Diamond => "Dmnd",
    };
    push(&mut d, "Type", enumerated_value("GrdT", style));
    push(&mut d, "Rvrs", Value::Bool(g.reverse));
    push(&mut d, "Algn", Value::Bool(g.align_with_layer));
    push(&mut d, "Angl", angle_value(g.angle_deg));
    push(&mut d, "Scl ", percent_value(g.scale));
    push(&mut d, "Dthr", Value::Bool(g.dither));
    d
}
