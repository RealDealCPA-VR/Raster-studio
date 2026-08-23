//! The tool palette and the tool options bar.

use design::{
    color32, current_tokens, egui_theme::rounding, ColorRole, Radius, Space, TextRole, TypeRole,
};
use egui::{Response, Sense, Ui, Vec2};
use tools::{OptionKind, OptionSpec, ToolId};

use crate::icons::icon_for;
use crate::intent::Intent;
use crate::palette::{group_label, tooltip, PaletteModel};
use crate::tool_options::{
    normalise_gradient, schema_for, wants_gradient_stops, OptionValue, BLEND_MODE_KEY,
};
use crate::Workspace;

use super::{body, hint, overlay_frame, rgba_to_color32, swatch, text};

/// The vertical strip of tools down the left edge.
pub fn tool_palette(w: &mut Workspace, ctx: &egui::Context) {
    let model = PaletteModel::build();
    let t = design::current_theme(ctx).tokens();
    let strip = t.metrics.toolbar_button + Space::Small.pt() * 2.0;
    egui::SidePanel::left("raster-tools")
        .resizable(false)
        .exact_width(strip)
        .frame(
            egui::Frame::none()
                .fill(color32(t.palette.color(ColorRole::SurfacePanel)))
                .inner_margin(egui::Margin::symmetric(
                    Space::Small.pt(),
                    Space::Small.pt(),
                )),
        )
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = Space::Hair.pt();
                    // One divider per group run, so the palette reads as the
                    // registry's own grouping rather than as one long column.
                    for (_, members) in model.groups() {
                        for slot in members {
                            slot_button(w, ui, &model, slot);
                        }
                        ui.add_space(Space::XSmall.pt());
                        super::hairline(ui);
                        ui.add_space(Space::XSmall.pt());
                    }
                });
        });
    flyout(w, ctx, &model);
}

fn slot_button(w: &mut Workspace, ui: &mut Ui, model: &PaletteModel, slot: usize) {
    let tool = w.palette.representative(model, slot);
    let selected = w.palette.slot_is_active(model, slot);
    let has_variants = model.slots()[slot].has_variants();
    let response = icon_button(
        ui,
        tool,
        selected,
        has_variants,
        Some(super::ids::tool_slot(slot)),
    );

    let hover = match crate::palette::info(tool) {
        Some(i) => tooltip(i),
        None => String::new(),
    };
    // A fly-out nobody knows about is a fly-out nobody opens, and the corner
    // mark alone does not say what it means.
    let hover = match (hover.is_empty(), has_variants) {
        (true, _) => hover,
        (false, true) => format!("{hover}\nClick again, or right-click, for more tools"),
        (false, false) => hover,
    };
    let response = if hover.is_empty() {
        response
    } else {
        response.on_hover_text(hover)
    };

    if response.clicked() {
        // The whole decision — select, open the variants, or put them away —
        // is `PaletteState::click_slot`'s, so the left-click path and the
        // right-click path cannot drift apart again.
        if let crate::palette::SlotClick::Selected(tool) = w.palette.click_slot(model, slot) {
            w.emit(Intent::SelectTool(tool));
        }
    } else if has_variants && (response.secondary_clicked() || response.long_touched()) {
        w.palette.toggle_flyout(slot);
    }
}

/// A square button carrying a vector icon, plus the corner mark that says a
/// fly-out is hiding behind it.
fn icon_button(
    ui: &mut Ui,
    tool: ToolId,
    selected: bool,
    has_variants: bool,
    id: Option<egui::Id>,
) -> Response {
    let t = current_tokens(ui);
    let side = t.metrics.toolbar_button;
    let (rect, auto) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    // The palette's own buttons carry a stable id so a headless test can find
    // and click one; the copies inside a fly-out take egui's derived id, since
    // two controls for one tool must not share an id.
    let response = match id {
        Some(id) => ui.interact(rect, id, Sense::click()),
        None => auto,
    };
    if ui.is_rect_visible(rect) {
        let radius = Radius::Medium.resolve(&t.radii, side);
        let painter = ui.painter();
        if selected {
            painter.rect_filled(
                rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::AccentSubtle)),
            );
            painter.rect_stroke(
                rect,
                rounding(radius),
                egui::Stroke::new(
                    t.borders.hairline,
                    color32(t.palette.color(ColorRole::Accent)),
                ),
            );
        } else if response.hovered() {
            painter.rect_filled(
                rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::ControlFillHovered)),
            );
        }
        let glyph_color = color32(t.palette.text(if selected {
            TextRole::Primary
        } else {
            TextRole::Secondary
        }));
        let icon = crate::palette::info(tool)
            .map(|i| icon_for(i.icon))
            .unwrap_or(crate::icons::Icon::UNKNOWN);
        icon.paint(
            painter,
            rect.shrink(Space::Small.pt()),
            glyph_color,
            t.borders.hairline * 1.5,
        );
        if has_variants {
            let corner = rect.right_bottom() - Vec2::splat(Space::XSmall.pt());
            painter.add(egui::Shape::convex_polygon(
                vec![
                    corner,
                    corner - Vec2::new(Space::XSmall.pt(), 0.0),
                    corner - Vec2::new(0.0, Space::XSmall.pt()),
                ],
                color32(t.palette.text(TextRole::Tertiary)),
                egui::Stroke::NONE,
            ));
        }
    }
    response
}

/// The fly-out listing a slot's variants.
///
/// Drawn as a floating window rather than an `egui::popup`, because the slot
/// buttons live inside a scrolled side panel and a popup would be clipped by
/// it. That costs the popup's click-outside dismissal, so this puts it back by
/// hand — see [`dismissed_by_a_click_outside`]. Without it the fly-out has no
/// exit at all: with no title bar egui draws no close control, and a window is
/// not a popup.
fn flyout(w: &mut Workspace, ctx: &egui::Context, model: &PaletteModel) {
    let Some(slot) = w.palette.open_flyout else {
        return;
    };
    let Some(entry) = model.slots().get(slot).cloned() else {
        w.palette.close_flyout();
        return;
    };
    // The slot's own button, so the fly-out opens beside the thing it belongs
    // to — and so a click on that button is not also read as a click outside,
    // which would close and immediately re-open it.
    let anchor = ctx
        .read_response(super::ids::tool_slot(slot))
        .map(|r| r.rect);

    let mut window = egui::Window::new(group_label(entry.group))
        .id(egui::Id::new(("raster-tool-flyout", slot)))
        .collapsible(false)
        .resizable(false)
        .title_bar(false)
        .frame(egui::Frame::none());
    if let Some(rect) = anchor {
        window = window.fixed_pos(egui::pos2(rect.right(), rect.top()));
    }

    let mut picked: Option<ToolId> = None;
    let area = window.show(ctx, |ui| {
        overlay_frame(ui).show(ui, |ui| {
            ui.label(hint(ui, group_label(entry.group)));
            ui.add_space(Space::Hair.pt());
            for tool in &entry.tools {
                let Some(info) = crate::palette::info(*tool) else {
                    continue;
                };
                let selected = w.palette.active() == *tool;
                let row = ui
                    .horizontal(|ui| {
                        let r = icon_button(ui, *tool, selected, false, None);
                        let label = ui.label(body(ui, tooltip(info)));
                        r | label
                    })
                    .inner;
                // The whole row is the target, under an id a headless test can
                // name — see [`super::ids::flyout_tool`].
                let response = ui.interact(
                    row.rect,
                    super::ids::flyout_tool(slot, *tool),
                    Sense::click(),
                );
                if response.clicked() || row.clicked() {
                    picked = Some(*tool);
                }
            }
        });
    });

    if let Some(tool) = picked {
        if w.palette.activate(model, tool) {
            w.emit(Intent::SelectTool(tool));
        }
        // Picking always closes, changed or not: the fly-out has answered the
        // question it was opened to ask.
        w.palette.close_flyout();
        return;
    }

    let surface = area.map(|a| a.response.rect);
    let press = ctx.input(|i| {
        i.pointer
            .any_pressed()
            .then(|| i.pointer.interact_pos())
            .flatten()
    });
    if dismissed_by_a_click_outside(press, surface, anchor) {
        w.palette.close_flyout();
    }
}

/// Whether a press this frame lands outside both the fly-out and the button
/// that opened it, and so should shut the fly-out.
///
/// A press inside the fly-out is the user using it; a press on the slot button
/// is already handled by [`crate::palette::PaletteState::click_slot`], and
/// treating it as an outside click too would close the fly-out on the same
/// frame that button re-opened it.
pub(crate) fn dismissed_by_a_click_outside(
    press: Option<egui::Pos2>,
    surface: Option<egui::Rect>,
    anchor: Option<egui::Rect>,
) -> bool {
    let Some(at) = press else {
        return false;
    };
    let inside = |rect: Option<egui::Rect>| rect.is_some_and(|r| r.contains(at));
    !inside(surface) && !inside(anchor)
}

// ---------------------------------------------------------------------------
// Options bar
// ---------------------------------------------------------------------------

/// The horizontal strip under the menu bar: the active tool's settings.
pub fn tool_options(w: &mut Workspace, ctx: &egui::Context) {
    let tool = w.palette.active();
    let Some(info) = crate::palette::info(tool) else {
        return;
    };
    let specs = schema_for(info);
    let t = design::current_theme(ctx).tokens();

    egui::TopBottomPanel::top("raster-tool-options")
        .exact_height(t.metrics.toolbar_height)
        .frame(
            egui::Frame::none()
                .fill(color32(t.palette.color(ColorRole::SurfacePanel)))
                .inner_margin(egui::Margin::symmetric(
                    t.metrics.panel_padding,
                    Space::XSmall.pt(),
                )),
        )
        .show(ctx, |ui| {
            egui::ScrollArea::horizontal()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = Space::Small.pt();
                        ui.label(text(ui, info.name, TextRole::Primary, TypeRole::Headline));
                        separator(ui);
                        if specs.is_empty() && !wants_gradient_stops(info) {
                            ui.label(hint(ui, "This tool has no options"));
                            return;
                        }
                        for spec in &specs {
                            option_control(w, ui, tool, spec);
                        }
                        if wants_gradient_stops(info) {
                            separator(ui);
                            gradient_control(w, ui, tool);
                        }
                        separator(ui);
                        let at_defaults = w.options.is_default(tool);
                        let reset = super::labelled_button(
                            ui,
                            "Reset",
                            !at_defaults,
                            super::ids::tool_options_reset(tool),
                        );
                        let reset = reset.on_hover_text(if at_defaults {
                            "This tool is already at its defaults"
                        } else {
                            "Return this tool to its defaults"
                        });
                        if reset.clicked() && w.options.reset(tool) {
                            // Reset travels as an intent like every other
                            // control in this bar: an application following the
                            // intent stream has to learn the tool went back to
                            // its defaults, or it keeps painting with the size
                            // the user just cleared.
                            w.emit(Intent::ResetToolOptions(tool));
                        }
                    });
                });
        });
}

fn separator(ui: &mut Ui) {
    let t = current_tokens(ui);
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(t.borders.hairline, t.metrics.control_height),
        Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        ui.painter().vline(
            rect.center().x,
            rect.y_range(),
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::SeparatorHairline)),
            ),
        );
    }
}

/// One option, drawn according to its kind and written back through
/// [`crate::ToolOptions`] so the registry's range is always applied.
fn option_control(w: &mut Workspace, ui: &mut Ui, tool: ToolId, spec: &OptionSpec) {
    let t = current_tokens(ui);
    let field = t.metrics.numeric_field_width;
    let Some(current) = w.options.get(tool, spec.key) else {
        return;
    };

    let emit = |w: &mut Workspace, value: OptionValue| {
        if w.options.set(tool, spec.key, value) {
            w.emit(Intent::SetToolOption {
                tool,
                key: spec.key,
                value,
            });
        }
    };

    // Every control below is marked with `ids::tool_option(tool, key)` — one
    // stable id per option, whatever kind it is — so a headless test can drive
    // it and assert the `Intent::SetToolOption` that comes out. Without that
    // the whole bar could be unwired and the suite would not notice.
    let id = super::ids::tool_option(tool, spec.key);

    match (spec.kind, current) {
        (OptionKind::Float { min, max, .. }, OptionValue::Float(mut v)) => {
            ui.label(hint(ui, spec.label));
            let response = ui.add_sized(
                Vec2::new(field, t.metrics.control_height),
                egui::DragValue::new(&mut v)
                    .range(min..=max)
                    .speed((max - min) / 400.0)
                    .max_decimals(2),
            );
            super::mark(ui, response.rect, id);
            if response.changed() {
                emit(w, OptionValue::Float(v));
            }
        }
        (OptionKind::Int { min, max, .. }, OptionValue::Int(mut v)) => {
            ui.label(hint(ui, spec.label));
            let response = ui.add_sized(
                Vec2::new(field, t.metrics.control_height),
                egui::DragValue::new(&mut v).range(min..=max),
            );
            super::mark(ui, response.rect, id);
            if response.changed() {
                emit(w, OptionValue::Int(v));
            }
        }
        (OptionKind::Bool { .. }, OptionValue::Bool(mut v)) => {
            let response = ui.checkbox(&mut v, hint(ui, spec.label));
            super::mark(ui, response.rect, id);
            if response.changed() {
                emit(w, OptionValue::Bool(v));
            }
        }
        (OptionKind::Choice { choices, .. }, OptionValue::Choice(index)) => {
            ui.label(hint(ui, spec.label));
            let shown = choices.get(index).copied().unwrap_or("—");
            let mut picked = index;
            let combo = egui::ComboBox::from_id_salt(("raster-option", tool, spec.key))
                .selected_text(body(ui, shown))
                .show_ui(ui, |ui| {
                    for (i, choice) in choices.iter().enumerate() {
                        let row = ui.selectable_label(i == index, body(ui, *choice));
                        super::mark(
                            ui,
                            row.rect,
                            super::ids::tool_option_choice(tool, spec.key, i),
                        );
                        if row.clicked() {
                            picked = i;
                        }
                    }
                });
            super::mark(ui, combo.response.rect, id);
            if picked != index {
                emit(w, OptionValue::Choice(picked));
            }
        }
        (OptionKind::Color { .. }, OptionValue::Color(rgba)) => {
            ui.label(hint(ui, spec.label));
            let mut color = rgba_to_color32(rgba);
            let response = ui.color_edit_button_srgba(&mut color);
            super::mark(ui, response.rect, id);
            if response.changed() {
                let c = color.to_srgba_unmultiplied();
                emit(
                    w,
                    OptionValue::Color([
                        f32::from(c[0]) / 255.0,
                        f32::from(c[1]) / 255.0,
                        f32::from(c[2]) / 255.0,
                        f32::from(c[3]) / 255.0,
                    ]),
                );
            }
        }
        // A stored value whose kind does not match its spec cannot happen —
        // `ToolOptions::set` refuses it — but drawing nothing would be a silent
        // hole, so say so instead.
        _ => {
            ui.label(hint(ui, format!("{}: unavailable", spec.label)));
        }
    }

    if spec.key == BLEND_MODE_KEY {
        separator(ui);
    }
}

/// The gradient ramp preview and its stop editor.
fn gradient_control(w: &mut Workspace, ui: &mut Ui, tool: ToolId) {
    let t = current_tokens(ui);
    ui.label(hint(ui, "Gradient"));
    let gradient = w.options.gradient(tool);
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(t.metrics.inspector_label_width, t.metrics.control_height),
        Sense::hover(),
    );
    let response = ui.interact(rect, super::ids::gradient_swatch(tool), Sense::click());
    if ui.is_rect_visible(rect) {
        super::checkerboard(ui.painter(), rect, Space::XSmall.pt());
        // A ramp is drawn as a run of thin quads: enough to read as a gradient,
        // cheap enough to redraw every frame.
        let steps = (rect.width().round() as usize).clamp(2, 256);
        for i in 0..steps {
            let a = i as f32 / steps as f32;
            let b = (i + 1) as f32 / steps as f32;
            let slice = egui::Rect::from_min_max(
                egui::pos2(rect.left() + a * rect.width(), rect.top()),
                egui::pos2(rect.left() + b * rect.width(), rect.bottom()),
            );
            ui.painter().rect_filled(
                slice,
                egui::Rounding::ZERO,
                rgba_to_color32(sample_ramp(&gradient, (a + b) * 0.5)),
            );
        }
        let radius = Radius::Small.resolve(&t.radii, rect.height());
        ui.painter().rect_stroke(
            rect,
            rounding(radius),
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::ControlStroke)),
            ),
        );
    }
    let response = response.on_hover_text("Edit gradient stops");

    let popup = egui::Id::new(("raster-gradient-stops", tool));
    if response.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    egui::popup_below_widget(
        ui,
        popup,
        &response,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(current_tokens(ui).metrics.inspector_label_width * 3.0);
            let mut edited = gradient.clone();
            let mut changed = false;
            let mut remove: Option<usize> = None;
            // Gated on how many stops would be *left*, not on the stop's own
            // index: with four stops the index test made the first two
            // permanently un-removable while the tooltip claimed a ramp needs
            // two stops — a disabled reason that was not the real reason.
            let can_remove = crate::tool_options::can_remove_gradient_stop(edited.stops.len());
            for (i, stop) in edited.stops.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let mut color = rgba_to_color32(stop.color);
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        let c = color.to_srgba_unmultiplied();
                        stop.color = [
                            f32::from(c[0]) / 255.0,
                            f32::from(c[1]) / 255.0,
                            f32::from(c[2]) / 255.0,
                            f32::from(c[3]) / 255.0,
                        ];
                        changed = true;
                    }
                    if ui
                        .add(egui::Slider::new(&mut stop.position, 0.0..=1.0).show_value(false))
                        .changed()
                    {
                        changed = true;
                    }
                    ui.label(hint(ui, format!("{:.0}%", stop.position * 100.0)));
                    // An empty button for the framing and the disabled-hover
                    // text, with the drawing painted into the rect it reports.
                    let side = current_tokens(ui).metrics.min_hit_target;
                    let response = ui.add_enabled(
                        can_remove,
                        egui::Button::new("").min_size(Vec2::splat(side)),
                    );
                    super::paint_icon(
                        ui,
                        response.rect,
                        "minus",
                        if can_remove {
                            TextRole::Primary
                        } else {
                            TextRole::Disabled
                        },
                    );
                    if response
                        .on_disabled_hover_text("A ramp needs at least two stops")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                edited.stops.remove(i);
                changed = true;
            }
            if super::labelled_button(ui, "Add stop", true, super::ids::gradient_add_stop(tool))
                .on_hover_text("Add a stop in the middle of the ramp")
                .clicked()
            {
                let mid = layer_model::GradientStop {
                    position: 0.5,
                    color: sample_ramp(&edited, 0.5),
                    midpoint: 0.5,
                };
                edited.stops.push(mid);
                changed = true;
            }
            if changed {
                let ramp = normalise_gradient(edited);
                if w.options.set_gradient(tool, ramp.clone()) {
                    // A ramp is not an `OptionValue`, so it has an intent of
                    // its own rather than being written into the workspace and
                    // nowhere else.
                    w.emit(Intent::SetToolGradient {
                        tool,
                        gradient: Box::new(ramp),
                    });
                }
            }
        },
    );
}

/// Linear interpolation along a ramp, used only to preview it.
///
/// The authoritative rasteriser is `tools::gradient`; this is the swatch, and
/// it deliberately does not reimplement midpoint bias — a preview that needed
/// to be exact would be the renderer, not a preview.
fn sample_ramp(gradient: &layer_model::Gradient, at: f32) -> [f32; 4] {
    let stops = &gradient.stops;
    if stops.is_empty() {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let at = at.clamp(0.0, 1.0);
    if at <= stops[0].position {
        return stops[0].color;
    }
    for pair in stops.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if at <= b.position {
            let span = b.position - a.position;
            let f = if span > 0.0 {
                (at - a.position) / span
            } else {
                0.0
            };
            let mut out = [0.0f32; 4];
            for (channel, slot) in out.iter_mut().enumerate() {
                *slot = a.color[channel] + (b.color[channel] - a.color[channel]) * f;
            }
            return out;
        }
    }
    stops[stops.len() - 1].color
}

/// A colour well pair, used by the palette foot and by the Color panel.
pub(crate) fn color_wells(w: &mut Workspace, ui: &mut Ui) {
    let t = current_tokens(ui);
    let side = t.metrics.toolbar_button;
    ui.horizontal(|ui| {
        if swatch(ui, w.color.foreground(), side, Sense::click())
            .on_hover_text("Foreground")
            .clicked()
        {
            w.color.editing = crate::panels::color::ColorWell::Foreground;
        }
        if swatch(ui, w.color.background(), side, Sense::click())
            .on_hover_text("Background")
            .clicked()
        {
            w.color.editing = crate::panels::color::ColorWell::Background;
        }
        if super::icon_toggle(ui, "swap", false, "Swap colours  (X)").clicked() {
            w.color.swap();
            w.emit(Intent::SetForeground(w.color.foreground()));
            w.emit(Intent::SetBackground(w.color.background()));
        }
        if super::icon_toggle(ui, "colors-default", false, "Default colours  (D)").clicked() {
            w.color.reset();
            w.emit(Intent::SetForeground(w.color.foreground()));
            w.emit(Intent::SetBackground(w.color.background()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(100.0, 40.0))
    }

    #[test]
    fn a_press_inside_the_flyout_or_on_its_button_does_not_dismiss_it() {
        let surface = rect(100.0, 0.0);
        let anchor = rect(0.0, 0.0);
        assert!(!dismissed_by_a_click_outside(
            Some(surface.center()),
            Some(surface),
            Some(anchor)
        ));
        // The button that opened it is *not* outside: treating it as outside
        // would close the fly-out on the very frame the button re-opened it.
        assert!(!dismissed_by_a_click_outside(
            Some(anchor.center()),
            Some(surface),
            Some(anchor)
        ));
    }

    #[test]
    fn a_press_anywhere_else_dismisses_the_flyout() {
        let surface = rect(100.0, 0.0);
        let anchor = rect(0.0, 0.0);
        assert!(dismissed_by_a_click_outside(
            Some(egui::pos2(600.0, 400.0)),
            Some(surface),
            Some(anchor)
        ));
    }

    #[test]
    fn a_frame_with_no_press_never_dismisses_anything() {
        assert!(!dismissed_by_a_click_outside(
            None,
            Some(rect(0.0, 0.0)),
            Some(rect(200.0, 0.0))
        ));
    }

    #[test]
    fn an_unmeasured_surface_still_lets_a_click_dismiss() {
        // Before the fly-out has been laid out once there is no rectangle to
        // test against. A press then is outside it by definition rather than
        // being swallowed, so the fly-out can never get stuck open.
        assert!(dismissed_by_a_click_outside(
            Some(egui::pos2(10.0, 10.0)),
            None,
            None
        ));
    }
}
