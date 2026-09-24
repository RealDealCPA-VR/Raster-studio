//! W11-I: the CSS panel (Photopea's Window ▸ CSS).
//!
//! Shows the CSS that reproduces the active layer on a web page, with a Copy
//! button that puts it on the clipboard:
//!
//! * `position`, `left`, `top`, `width`, `height` — the layer's document-space
//!   frame, measured exactly as the Properties panel's Transform block
//!   measures it ([`props::Transform::frame`]; a raster layer's ink is
//!   measured by the application, which this panel asks for the way the
//!   Transform block does);
//! * `opacity` when the layer is not fully opaque;
//! * a fill layer's `background-color` or `background` gradient
//!   (`linear-gradient` / `radial-gradient` with its stops);
//! * a shape's `background-color` from its fill, `border` from its stroke and
//!   `border-radius` when it is a rounded rectangle
//!   ([`props::rect_corner_radius`]);
//! * `box-shadow` (or `text-shadow` on text) from an enabled drop shadow;
//! * on a text layer, `color`, `font-family`, `font-size`, `font-weight`,
//!   `font-style`, `text-align`, `text-decoration` and `letter-spacing`.
//!
//! [`layer_css`] is the whole answer as a string, so the panel, the Copy
//! button and a test all read the same text.

use std::fmt::Write as _;

use design::{TextRole, TypeRole};
use editor_core::Document;
use egui::Ui;
use layer_model::fill::FillSource;
use layer_model::text::{Alignment, Slant};
use layer_model::{GradientStyle, LayerId, LayerKind};

use crate::panels::properties as props;
use crate::strings::tr;
use crate::view::{empty_state, hairline, labelled_button, text};
use crate::Workspace;

/// Stable ids for a headless test.
pub mod ids {
    /// The Copy button.
    pub fn copy() -> egui::Id {
        egui::Id::new("raster-css-copy")
    }
    /// The code block.
    pub fn code() -> egui::Id {
        egui::Id::new("raster-css-code")
    }
}

/// `rgba` (straight alpha, **encoded** sRGB, each `0..=1`) as a CSS colour:
/// `#rrggbb` when opaque, `rgba(r, g, b, a)` otherwise.
pub fn css_color(rgba: [f32; 4]) -> String {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = rgba[3].clamp(0.0, 1.0);
    if a >= 1.0 {
        format!("#{:02x}{:02x}{:02x}", c(rgba[0]), c(rgba[1]), c(rgba[2]))
    } else {
        format!(
            "rgba({}, {}, {}, {})",
            c(rgba[0]),
            c(rgba[1]),
            c(rgba[2]),
            num(a, 2)
        )
    }
}

/// A number with at most `places` decimals and no trailing zeros.
fn num(v: f32, places: usize) -> String {
    let s = format!("{v:.places$}");
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// A CSS gradient of `gradient`'s stops. `angle_deg` is the editor's
/// direction (counter-clockwise from +x, 90 = bottom to top); CSS measures
/// clockwise from "to top", so `css = 90 - angle`.
fn css_gradient(
    gradient: &layer_model::Gradient,
    style: GradientStyle,
    angle_deg: f32,
    reverse: bool,
) -> String {
    let mut stops: Vec<(f32, [f32; 4])> = gradient
        .stops
        .iter()
        .map(|s| (s.position.clamp(0.0, 1.0), s.color))
        .collect();
    if reverse {
        stops = stops.into_iter().rev().map(|(p, c)| (1.0 - p, c)).collect();
    }
    let list = stops
        .iter()
        .map(|(p, c)| format!("{} {}%", css_color(*c), num(p * 100.0, 1)))
        .collect::<Vec<_>>()
        .join(", ");
    match style {
        GradientStyle::Radial => format!("radial-gradient(circle, {list})"),
        _ => {
            let css_angle = (90.0 - angle_deg).rem_euclid(360.0);
            format!("linear-gradient({}deg, {list})", num(css_angle, 1))
        }
    }
}

/// The CSS for layer `id`, one declaration per line; `None` for a layer that
/// is not in the document. `inks` is the application's raster measurement
/// ([`props::RasterInks::published`]); without it a raster layer's position
/// and size are left out rather than guessed.
pub fn layer_css(doc: &Document, id: LayerId, inks: &props::RasterInks) -> Option<String> {
    let layer = doc.layers.get(id)?;
    let mut out = String::new();
    let mut line = |k: &str, v: String| {
        let _ = writeln!(out, "{k}: {v};");
    };
    if let Some(frame) = props::Transform::frame(doc, id, inks) {
        line("position", "absolute".to_string());
        line("left", format!("{}px", num(frame.x, 0)));
        line("top", format!("{}px", num(frame.y, 0)));
        line("width", format!("{}px", num(frame.width, 0)));
        line("height", format!("{}px", num(frame.height, 0)));
    }
    let opacity = layer.opacity.clamp(0.0, 1.0);
    if opacity < 1.0 {
        line("opacity", num(opacity, 2));
    }
    let is_text = matches!(layer.kind, LayerKind::Text(_));
    match &layer.kind {
        LayerKind::Fill(fill) => match &fill.source {
            FillSource::Solid { color } => line("background-color", css_color(*color)),
            FillSource::Gradient(g) => line(
                "background",
                css_gradient(&g.gradient, g.style, g.angle_deg, g.reverse),
            ),
            FillSource::Pattern(_) => {}
        },
        LayerKind::Shape(shape) => {
            if let Some(fill) = shape.fill {
                line("background-color", css_color(fill));
            }
            if let Some(stroke) = &shape.stroke {
                if stroke.width_px > 0.0 {
                    line(
                        "border",
                        format!(
                            "{}px solid {}",
                            num(stroke.width_px, 1),
                            css_color(stroke.color)
                        ),
                    );
                }
            }
            if let Some((_, radius)) = props::rect_corner_radius(&shape.path_svg) {
                if radius > 0.0 {
                    line("border-radius", format!("{}px", num(radius as f32, 1)));
                }
            }
        }
        LayerKind::Text(t) => {
            // The text fill is linear light; CSS colours are encoded.
            let f = t.style.fill;
            let enc = color::linear_to_srgb3([f[0], f[1], f[2]]);
            line("color", css_color([enc[0], enc[1], enc[2], f[3]]));
            if !t.font_family.is_empty() {
                line("font-family", format!("\"{}\"", t.font_family));
            }
            line("font-size", format!("{}px", num(t.size_px, 1)));
            if t.style.weight.0 != 400 {
                line("font-weight", t.style.weight.0.to_string());
            }
            if t.style.slant == Slant::Italic {
                line("font-style", "italic".to_string());
            }
            let align = match t.paragraph.alignment {
                Alignment::Left => None,
                Alignment::Center => Some("center"),
                Alignment::Right => Some("right"),
                _ => Some("justify"),
            };
            if let Some(align) = align {
                line("text-align", align.to_string());
            }
            let mut deco = Vec::new();
            if t.style.underline {
                deco.push("underline");
            }
            if t.style.strikethrough {
                deco.push("line-through");
            }
            if !deco.is_empty() {
                line("text-decoration", deco.join(" "));
            }
            if t.style.tracking != 0.0 {
                line(
                    "letter-spacing",
                    format!("{}em", num(t.style.tracking / 1000.0, 3)),
                );
            }
        }
        _ => {}
    }
    if layer.effects.enabled {
        if let Some(shadow) = &layer.effects.drop_shadow {
            // The angle is where the light comes FROM; the shadow falls the
            // other way (screen y grows downward).
            let a = shadow.angle_deg.to_radians();
            let dx = -a.cos() * shadow.distance_px;
            let dy = a.sin() * shadow.distance_px;
            let size = shadow.size_px.max(0.0);
            let spread = shadow.spread.clamp(0.0, 1.0) * size;
            let mut c = shadow.color;
            c[3] = (c[3] * shadow.opacity).clamp(0.0, 1.0);
            let (key, value) = if is_text {
                (
                    "text-shadow",
                    format!(
                        "{}px {}px {}px {}",
                        num(dx, 0),
                        num(dy, 0),
                        num(size - spread, 0),
                        css_color(c)
                    ),
                )
            } else {
                (
                    "box-shadow",
                    format!(
                        "{}px {}px {}px {}px {}",
                        num(dx, 0),
                        num(dy, 0),
                        num(size - spread, 0),
                        num(spread, 0),
                        css_color(c)
                    ),
                )
            };
            line(key, value);
        }
    }
    Some(out)
}

/// The panel's catalogue keys.
const NO_DOCUMENT: &str = "ui.css.no_document";
const NO_LAYER: &str = "ui.css.no_layer";
const COPY: &str = "ui.css.copy";

/// Draw the panel.
pub(crate) fn css_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let Some(id) = doc.active_layer() else {
        empty_state(ui, tr(NO_LAYER));
        return;
    };
    // A raster layer's ink is measured by the application, which does it
    // while the Transform block (or this panel) is on screen.
    w.note_transform_block_drawn();
    let inks = props::RasterInks::published(ui.ctx());
    let Some(css) = layer_css(doc, id, &inks) else {
        return;
    };
    let code = ui.add(
        egui::Label::new(text(
            ui,
            css.trim_end(),
            TextRole::Primary,
            TypeRole::Footnote,
        ))
        .selectable(true),
    );
    // The code block under a stable id, for a headless test.
    let _ = ui.interact(code.rect, ids::code(), egui::Sense::hover());
    hairline(ui);
    if labelled_button(ui, tr(COPY), !css.is_empty(), ids::copy()).clicked() {
        ui.ctx().copy_text(css);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every string the panel shows is a catalogue key that resolves.
    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [NO_DOCUMENT, NO_LAYER, COPY] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }
}
