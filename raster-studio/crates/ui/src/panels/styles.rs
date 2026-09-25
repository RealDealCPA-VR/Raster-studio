//! W13-N: the Styles panel (Photopea's Window ▸ Style).
//!
//! Every saved layer style — defined here or with Layer ▸ Layer Style ▸ New
//! Style Preset, or loaded from an `.asl` library with File ▸ Open — drawn
//! as a swatch: a schematic of the style's drop shadow, fill (its colour or
//! gradient overlay) and stroke. A click on a swatch applies that style to
//! the active layer ([`MenuAction::ApplyStyleAt`], one undo step); the `+`
//! in the footer makes a new style from the active layer's own
//! ([`MenuAction::DefineStylePreset`]).
//!
//! The presets live in the application (they persist with the preset
//! store, which this crate never sees), so the two sides meet through
//! egui's frame data the way the Actions panel does: the application
//! publishes a [`StylesView`] each frame and the panel draws it.

use design::{color32, current_tokens, ColorRole, Space};
use editor_core::Document;
use egui::{Align, Layout, Rect, Sense, Ui};
use layer_model::{FillStyle, LayerEffects};

use crate::intent::Intent;
use crate::menu::MenuAction;
use crate::strings::tr;
use crate::view::{empty_state, hairline, hint, icon_action_id, rgba_to_color32, ActionState};
use crate::Workspace;

/// Stable ids for a headless test.
pub mod ids {
    /// Swatch `index`.
    pub fn tile(index: usize) -> egui::Id {
        egui::Id::new(("raster-styles-tile", index))
    }
    /// The footer's New Style button.
    pub fn new() -> egui::Id {
        egui::Id::new("raster-styles-new")
    }
}

/// The saved styles, oldest first, as the application publishes them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StylesView {
    pub styles: Vec<(String, LayerEffects)>,
}

impl StylesView {
    fn slot() -> egui::Id {
        egui::Id::new("raster-styles-view")
    }

    /// Hand the styles to the panel for this frame and the next.
    pub fn publish(self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(Self::slot(), self));
    }

    /// What the application last published (empty before it has).
    pub fn published(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(Self::slot()))
            .unwrap_or_default()
    }
}

const NO_DOCUMENT: &str = "ui.styles.no_document";
const NONE: &str = "ui.styles.none";
const NEW: &str = "ui.styles.new";
const NO_LAYER: &str = "ui.styles.no_layer";

/// Draw the panel.
pub(crate) fn styles_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let view = StylesView::published(ui.ctx());
    let active = doc.active_layer().and_then(|id| doc.layers.get(id));
    let t = current_tokens(ui);
    let side = 2.0 * t.metrics.control_height;
    if view.styles.is_empty() {
        ui.label(hint(ui, tr(NONE)));
    } else if active.is_none() {
        ui.label(hint(ui, tr(NO_LAYER)));
    }
    // W16-E: the panel menu's Tiles/List, and the style its Name Change,
    // Delete and Export act on (the one last clicked, ringed).
    use crate::panels::panel_menus_w16::{self as menus, Library, ViewMode};
    let chosen = menus::selected(ui.ctx(), Library::Styles);
    let mut clicked: Option<usize> = None;
    if menus::view_mode(ui.ctx(), Library::Styles) == ViewMode::List {
        for (index, (name, effects)) in view.styles.iter().enumerate() {
            let row = crate::view::list_row_layout(
                ui,
                menus::ids::list_row(Library::Styles, index),
                chosen == Some(index),
                |ui| {
                    let chip = t.metrics.list_row_height;
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(chip, chip), Sense::hover());
                    paint_tile(ui, rect, effects, false);
                    ui.add_space(Space::XSmall.pt());
                    ui.label(crate::view::body(ui, name.clone()));
                },
            );
            if row.response.clicked() {
                clicked = Some(index);
            }
        }
    } else {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(Space::XSmall.pt(), Space::XSmall.pt());
            for (index, (name, effects)) in view.styles.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), Sense::hover());
                let response = ui
                    .interact(rect, ids::tile(index), Sense::click())
                    .on_hover_text(name.clone());
                paint_tile(ui, rect, effects, response.hovered());
                if chosen == Some(index) {
                    ui.painter().rect_stroke(
                        rect,
                        0.0,
                        egui::Stroke::new(
                            t.borders.thick,
                            color32(t.palette.color(ColorRole::SelectionStroke)),
                        ),
                    );
                }
                if response.clicked() {
                    clicked = Some(index);
                }
            }
        });
    }
    if let Some(index) = clicked {
        menus::set_selected(ui.ctx(), Library::Styles, Some(index));
        if active.is_some() {
            w.emit(Intent::Action(MenuAction::ApplyStyleAt(index)));
        }
    }
    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    let can_define = active.is_some_and(|l| !l.effects.is_default());
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), t.metrics.control_height),
        Layout::right_to_left(Align::Center),
        |ui| {
            let state = if can_define {
                ActionState::Idle
            } else {
                ActionState::Disabled
            };
            if icon_action_id(ui, "plus", tr(NEW), state, Some(ids::new())).clicked() {
                w.emit(Intent::Action(MenuAction::DefineStylePreset));
            }
        },
    );
}

/// A straight-alpha colour at `opacity`.
fn faded(rgba: [f32; 4], opacity: f32) -> egui::Color32 {
    rgba_to_color32([rgba[0], rgba[1], rgba[2], rgba[3] * opacity.clamp(0.0, 1.0)])
}

/// A swatch: the style's drop shadow, its fill (colour overlay, else the
/// gradient overlay's ramp, else a neutral token) and its stroke.
fn paint_tile(ui: &Ui, rect: Rect, effects: &LayerEffects, hovered: bool) {
    let t = current_tokens(ui);
    let p = ui.painter_at(rect);
    let ground = if hovered {
        ColorRole::SurfaceElevated
    } else {
        ColorRole::SurfaceSunken
    };
    p.rect_filled(rect, 0.0, color32(t.palette.color(ground)));
    let shape = Rect::from_center_size(rect.center(), rect.size() * 0.55);
    if let Some(shadow) = &effects.drop_shadow {
        let a = shadow.angle_deg.to_radians();
        let d = shadow.distance_px.min(shape.width() * 0.25);
        let offset = egui::vec2(-a.cos() * d, a.sin() * d);
        p.rect_filled(
            shape.translate(offset),
            0.0,
            faded(shadow.color, shadow.opacity * 0.6),
        );
    }
    if let Some(overlay) = &effects.color_overlay {
        p.rect_filled(shape, 0.0, faded(overlay.color, overlay.opacity));
    } else if let Some(g) = effects
        .gradient_overlay
        .as_ref()
        .filter(|g| !g.gradient.stops.is_empty())
    {
        // The ramp in vertical bands, left to right.
        let bands = 8;
        for i in 0..bands {
            let u = (i as f32 + 0.5) / bands as f32;
            let color = ramp_at(&g.gradient, if g.reverse { 1.0 - u } else { u });
            let x0 = shape.left() + shape.width() * i as f32 / bands as f32;
            let x1 = shape.left() + shape.width() * (i + 1) as f32 / bands as f32;
            let band = Rect::from_x_y_ranges(x0..=x1, shape.y_range());
            p.rect_filled(band, 0.0, faded(color, g.opacity));
        }
    } else {
        p.rect_filled(
            shape,
            0.0,
            color32(t.palette.color(ColorRole::TextSecondary)),
        );
    }
    if let Some(stroke) = &effects.stroke {
        if let FillStyle::Solid(color) = stroke.fill {
            p.rect_stroke(
                shape,
                0.0,
                egui::Stroke::new(t.borders.hairline * 2.0, faded(color, stroke.opacity)),
            );
        }
    }
}

/// The gradient's colour at `u` in `0..=1`, linear between stops.
fn ramp_at(gradient: &layer_model::Gradient, u: f32) -> [f32; 4] {
    let stops = &gradient.stops;
    let first = &stops[0];
    if u <= first.position {
        return first.color;
    }
    for pair in stops.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if u <= b.position {
            let span = (b.position - a.position).max(f32::EPSILON);
            let k = ((u - a.position) / span).clamp(0.0, 1.0);
            let mut out = [0.0; 4];
            for (c, o) in out.iter_mut().enumerate() {
                *o = a.color[c] + (b.color[c] - a.color[c]) * k;
            }
            return out;
        }
    }
    stops[stops.len() - 1].color
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [NO_DOCUMENT, NONE, NEW, NO_LAYER] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn the_published_view_round_trips_through_the_context() {
        let ctx = egui::Context::default();
        assert!(StylesView::published(&ctx).styles.is_empty());
        let view = StylesView {
            styles: vec![("Red".into(), LayerEffects::default())],
        };
        view.clone().publish(&ctx);
        assert_eq!(StylesView::published(&ctx), view);
    }
}
