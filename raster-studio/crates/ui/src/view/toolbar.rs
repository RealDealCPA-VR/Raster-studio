//! The tool palette and the tool options bar.

use design::{
    color32, current_tokens, egui_theme::rounding, ColorRole, Radius, Space, TextRole, TypeRole,
};
use egui::{Response, Sense, Ui, Vec2};
use tools::{OptionKind, OptionSpec, ToolId};

use crate::icons::icon_for;
use crate::intent::Intent;
use crate::menu::MenuAction;
use crate::palette::{group_label, tooltip, PaletteModel};
use crate::tool_options::{wants_gradient_stops, OptionValue, BLEND_MODE_KEY};
use crate::Workspace;

use super::{body, hint, overlay_frame, rgba_to_color32, swatch, text};

/// The vertical strip of tools down the left edge.
///
/// Two regions share the panel: the footer — the colour wells, swap and reset,
/// then quick mask and screen mode — pinned to the bottom, and the slot column
/// above it, which scrolls when the window is shorter than the registry
/// (Photopea's nineteen slots at 28 pt fit a 720 pt window with the footer;
/// the old twenty-four did not). The footer is laid out *first*, as a bottom
/// panel inside the side panel, so the column is given exactly the height
/// that is left rather than the two fighting over the flow.
///
/// History, so nobody re-learns it: for a year the footer was drawn after the
/// slots and opened with a `rect_filled(ui.max_rect(), ..)` — an opaque panel
/// colour over the whole column, on top of every icon already painted, with
/// the wells then placed at the top of that rect. Every product shot showed
/// two wells over an empty column, and the emptiness was blamed on the
/// renderer failing to rasterise a `ScrollArea` batch. The mesh was fine; it
/// was painted over. The footer now paints only inside its own reserved rect.
pub fn tool_palette(w: &mut Workspace, ctx: &egui::Context) {
    let model = PaletteModel::build();
    let t = design::current_theme(ctx).tokens();
    let strip = t.metrics.tool_palette_button + Space::Small.pt() * 2.0;
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
            egui::TopBottomPanel::bottom("raster-tools-footer")
                .resizable(false)
                .show_separator_line(false)
                .exact_height(footer_height(t))
                .frame(egui::Frame::none())
                .show_inside(ui, |ui| footer(w, ui));
            egui::ScrollArea::vertical()
                .id_salt("raster-tools-slots")
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    // Slots sit flush, as Photopea's do; a group ends with a
                    // hairline so the column reads as the registry's grouping
                    // rather than as one long run.
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (_, members) in model.groups() {
                        for slot in members {
                            slot_button(w, ui, &model, slot);
                        }
                        ui.add_space(Space::Hair.pt());
                        super::hairline(ui);
                        ui.add_space(Space::Hair.pt());
                    }
                });
        });
    flyout(w, ctx, &model);
}

/// The footer's reserved height: the overlapping well pair, a gap, the
/// swap/reset row, a gap, and the quick-mask/screen-mode row.
fn footer_height(t: &design::Tokens) -> f32 {
    well_pair_height(t) + (Space::XSmall.pt() + t.metrics.min_hit_target) * 2.0
}

/// The height of the two wells drawn overlapping — the front one is offset
/// down by [`Space::Small`].
fn well_pair_height(t: &design::Tokens) -> f32 {
    t.metrics.color_well + Space::Small.pt()
}

/// Photopea's bottom-of-column controls: the foreground/background swatch
/// pair, with swap (X) and reset (D) beneath it, and quick mask (Q) and
/// screen mode (F) beneath those.
///
/// Everything is placed at absolute offsets inside the footer's rect rather
/// than through egui's layout: the footer must be exactly the column wide and
/// a known height ([`footer_height`]), and its caller has already reserved
/// that rect. It paints nothing outside it — see [`tool_palette`] for why that
/// sentence has to be written down.
///
/// Q routes to the Select menu's *Edit in Quick Mask Mode* — the same
/// [`Intent::Action`] the menu item and the `Q` chord raise — and reads as
/// engaged from [`crate::palette::PaletteState::quick_mask`], which only the
/// shell sets, from the editor, every frame.
///
/// F raises a cycle request ([`crate::palette::PaletteState::request_screen_mode_cycle`])
/// the chrome performs as the application's screen-mode action, and lights
/// the two full-screen modes from [`crate::palette::PaletteState::screen_mode`]
/// — mirrored from the editor, never cycled here, so the control cannot light
/// over an unchanged screen. `the_column_is_photopeas_and_q_and_f_sit_under_the_wells`
/// pins both contracts.
fn footer(w: &mut Workspace, ui: &mut Ui) {
    let tokens = design::current_theme(ui.ctx()).tokens();
    let area = ui.max_rect();
    let well = tokens.metrics.color_well;
    let hit = tokens.metrics.min_hit_target;

    let fg = w.color.well(crate::panels::color::ColorWell::Foreground);
    let bg = w.color.well(crate::panels::color::ColorWell::Background);
    let edge = egui::Stroke::new(
        tokens
            .borders
            .hairline_for_scale(ui.ctx().pixels_per_point()),
        color32(tokens.palette.text(design::TextRole::Tertiary)),
    );
    let rounding = design::egui_theme::rounding(design::Radius::Small.resolve(&tokens.radii, well));
    let well_color = |c: [f32; 4]| -> egui::Color32 {
        let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        egui::Color32::from_rgba_unmultiplied(to8(c[0]), to8(c[1]), to8(c[2]), to8(c[3]))
    };

    // Row 1: the swatch pair overlaps the way Photopea draws it — background
    // behind and offset up-right, foreground in front and down-left. The pair
    // is as wide as the column's content, so it is flush with both edges.
    let offset = (area.width() - well).max(0.0);
    let bg_rect = egui::Rect::from_min_size(
        egui::pos2(area.left() + offset, area.top()),
        egui::vec2(well, well),
    );
    let fg_rect = egui::Rect::from_min_size(
        egui::pos2(area.left(), area.top() + Space::Small.pt()),
        egui::vec2(well, well),
    );
    ui.painter().rect_filled(bg_rect, rounding, well_color(bg));
    ui.painter()
        .rect_stroke(bg_rect, egui::Rounding::ZERO, edge);
    ui.painter().rect_filled(fg_rect, rounding, well_color(fg));
    ui.painter()
        .rect_stroke(fg_rect, egui::Rounding::ZERO, edge);

    // A small square icon control at an absolute offset, with hover fill.
    // Placed by hand, because `icon_button_id` allocates through the layout
    // this footer deliberately avoids. The glyph is inset by a hair only: the
    // control is already hit-target sized, and the old `shrink(3.0)` on top
    // of `paint_ui_icon`'s own inset left a 4 pt glyph.
    // `engaged` draws the control the way a selected slot is drawn — the
    // accent fill and ring — so quick mask reads as a mode that is *on*.
    // `live == false` is a disabled control: hover only (so the tooltip can
    // say why), no hover fill, the glyph in the disabled role, and `clicked()`
    // is never true because the response has no click sense.
    let icon_control = |ui: &mut Ui,
                        left: f32,
                        top: f32,
                        id: egui::Id,
                        key: &str,
                        live: bool,
                        engaged: bool,
                        tooltip: &str| {
        let rect = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(hit, hit));
        let sense = if live {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let response = ui.interact(rect, id, sense);
        let radius =
            design::egui_theme::rounding(design::Radius::Small.resolve(&tokens.radii, hit));
        if live && engaged {
            ui.painter().rect_filled(
                rect,
                radius,
                color32(tokens.palette.color(ColorRole::AccentSubtle)),
            );
            ui.painter().rect_stroke(
                rect,
                radius,
                egui::Stroke::new(
                    tokens.borders.hairline,
                    color32(tokens.palette.color(ColorRole::Accent)),
                ),
            );
        } else if live && response.hovered() {
            ui.painter().rect_filled(
                rect,
                radius,
                color32(tokens.palette.color(ColorRole::ControlFillHovered)),
            );
        }
        let role = if !live {
            design::TextRole::Disabled
        } else if engaged {
            design::TextRole::Primary
        } else {
            design::TextRole::Secondary
        };
        crate::icons::paint_ui_icon_inset(ui, rect, Space::Hair, key, role);
        response.on_hover_text(tooltip)
    };

    // Row 2: swap (X) then reset (D), centred as a pair under the wells.
    let pair_w = hit * 2.0;
    let row2_left = area.left() + (area.width() - pair_w) * 0.5;
    let row2_top = area.top() + well_pair_height(tokens) + Space::XSmall.pt();
    let swap = icon_control(
        ui,
        row2_left,
        row2_top,
        super::ids::color_swap(),
        "swap",
        true,
        false,
        crate::strings::tr("ui.toolbar.swap.foreground.and.background.x"),
    );
    if swap.clicked() {
        w.emit(Intent::SetForeground(bg));
        w.emit(Intent::SetBackground(fg));
    }
    let reset = icon_control(
        ui,
        row2_left + hit,
        row2_top,
        super::ids::color_reset(),
        "reset-colors",
        true,
        false,
        crate::strings::tr("ui.toolbar.default.colours.d"),
    );
    if reset.clicked() {
        let (black, white) = (
            crate::panels::color::DEFAULT_FOREGROUND,
            crate::panels::color::DEFAULT_BACKGROUND,
        );
        w.emit(Intent::SetForeground(black));
        w.emit(Intent::SetBackground(white));
    }

    // Row 3: quick mask (Q) then screen mode (F), the same pair shape.
    // The tooltips are built from the menu item's own label and the mode's
    // name, so the footer cannot drift from what Select ▸ says.
    let row3_top = row2_top + hit + Space::XSmall.pt();
    let quick = icon_control(
        ui,
        row2_left,
        row3_top,
        crate::palette::quick_mask_control(),
        "quick-mask",
        true,
        w.palette.quick_mask,
        &format!("{} (Q)", MenuAction::ToggleQuickMask.label()),
    );
    if quick.clicked() {
        // Emit only. The engaged look is `w.palette.quick_mask`, which the
        // shell mirrors from the editor; flipping it here would light the
        // control on a refused toggle (no document open).
        w.emit(Intent::Action(MenuAction::ToggleQuickMask));
    }
    // F: the tooltip names the mode one press leads to; the engaged look is
    // the mirrored mode's, and the click is a request the chrome performs.
    let screen = icon_control(
        ui,
        row2_left + hit,
        row3_top,
        crate::palette::screen_mode_control(),
        "screen-mode",
        true,
        w.palette.screen_mode.fullscreen(),
        &format!("{} (F)", w.palette.screen_mode.next().label()),
    );
    if screen.clicked() {
        w.palette.request_screen_mode_cycle();
    }
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
        response.on_hover_text(hover.clone())
    };
    // The slot is painted by hand (an icon, no widget text), so without this
    // the accessibility tree carries an unnamed node where a screen reader
    // should say "Brush Tool (B)" (C14). The label is the tooltip's first
    // line — the name and its key; the multi-line hint is hover-only.
    if !hover.is_empty() {
        let name = hover.lines().next().unwrap_or_default().to_string();
        response.widget_info(move || {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone())
        });
    }

    if response.clicked() {
        // The whole decision — select, open the variants, or put them away —
        // is `PaletteState::click_slot`'s, so the left-click path and the
        // right-click path cannot drift apart again.
        if let crate::palette::SlotClick::Selected(tool) = w.palette.click_slot(model, slot) {
            w.emit(Intent::SelectTool(tool));
        }
        w.palette.hold = None;
    } else if has_variants && (response.secondary_clicked() || response.long_touched()) {
        w.palette.toggle_flyout(slot);
    } else if has_variants {
        // Photopea's press-and-hold: a variant tool's slot held past a beat
        // opens the fly-out without a click. Armed by the pointer being down
        // on the slot (a hold has no movement, so drag_started never fires).
        let now = ui.ctx().input(|i| i.time);
        let down_on_slot = response.is_pointer_button_down_on();
        match (w.palette.hold, down_on_slot) {
            (None, true) => w.palette.hold = Some((slot, now)),
            (Some((held, start)), true) if held == slot => {
                if now - start >= crate::palette::HOLD_SECONDS {
                    w.palette.hold = None;
                    if w.palette.open_flyout != Some(slot) {
                        w.palette.toggle_flyout(slot);
                    }
                }
            }
            (_, false) if w.palette.hold.map(|(held, _)| held) == Some(slot) => {
                w.palette.hold = None
            }
            _ => {}
        }
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
    let side = t.metrics.tool_palette_button;
    // Variant slots also sense drags: the press-and-hold that opens their
    // fly-out is a drag-shaped gesture.
    let sense = if has_variants {
        Sense::click_and_drag()
    } else {
        Sense::click()
    };
    let (rect, auto) = ui.allocate_exact_size(Vec2::splat(side), sense);
    // The palette's own buttons carry a stable id so a headless test can find
    // and click one; the copies inside a fly-out take egui's derived id, since
    // two controls for one tool must not share an id.
    let response = match id {
        Some(id) => ui.interact(rect, id, sense),
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
        // An XSmall inset on a 28 pt slot leaves a 20 pt glyph. The old
        // Small inset on a 24 pt button left 8 pt, which is why the icons
        // read as specks in every shot that did show them.
        icon.paint(
            painter,
            rect.shrink(Space::XSmall.pt()),
            glyph_color,
            crate::icons::icon_stroke_width(t),
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
    // W13-I: the Eyedropper's sampling ring rides the bar's frame.
    super::eyedropper_ring::paint(w, ctx);
    let tool = w.palette.active();
    let Some(info) = crate::palette::info(tool) else {
        return;
    };
    let specs = crate::tool_options::shown_schema(&w.options, info);
    let t = design::current_theme(ctx).tokens();

    egui::TopBottomPanel::top("raster-tool-options")
        .exact_height(t.metrics.toolbar_height)
        .frame(
            egui::Frame::none()
                // The header band, with the menu bar above it: one shade for
                // the two strips across the top, the panel shade for the
                // columns beneath them.
                .fill(color32(t.palette.color(ColorRole::SurfaceHeader)))
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
                        // W4-G: the Ruler has no settings, only its one
                        // action.
                        if tool == ToolId::Ruler {
                            straighten_button(w, ui);
                            return;
                        }
                        // W13-F: the Slice tool has no settings either, only
                        // Slices From Guides: the View menu's own row, so the
                        // bar and the menu cut the same grid.
                        if tool == ToolId::Slice {
                            let action = MenuAction::SlicesFromGuides;
                            let response = super::labelled_button(
                                ui,
                                &action.label(),
                                true,
                                super::ids::tool_option(ToolId::Slice, SLICES_FROM_GUIDES_KEY),
                            );
                            if response.clicked() {
                                w.emit(Intent::Action(action));
                            }
                            return;
                        }
                        // W16-C: the Commit button, beside the tool name
                        // where a long bar cannot scroll it away, while a
                        // transform, crop box or pen path is pending.
                        if pending_edit(w) {
                            commit_button(w, ui);
                            separator(ui);
                        }
                        // W16-K: Hand / Zoom / Rotate View have no settings,
                        // only their Fit / 100% / Reset buttons.
                        if w16k::view_row(w, ui, tool) {
                            return;
                        }
                        if specs.is_empty() && !wants_gradient_stops(info) {
                            ui.label(hint(
                                ui,
                                crate::strings::tr("ui.toolbar.this.tool.has.no.options"),
                            ));
                            return;
                        }
                        // Reset sits beside the tool name, where Photopea keeps
                        // its tool-preset reset: a long options row (the Brush
                        // with its pressure toggles) scrolls, and a Reset at its
                        // far end fell off a 1400-pt window.
                        let at_defaults = w.options.is_default(tool);
                        let reset = super::labelled_button(
                            ui,
                            "Reset",
                            !at_defaults,
                            super::ids::tool_options_reset(tool),
                        );
                        let reset = reset.on_hover_text(if at_defaults {
                            crate::strings::tr("ui.toolbar.this.tool.is.already.at.its")
                        } else {
                            crate::strings::tr("ui.toolbar.return.this.tool.to.its.defaults")
                        });
                        if reset.clicked() && w.options.reset(tool) {
                            // Reset travels as an intent like every other
                            // control in this bar: an application following the
                            // intent stream has to learn the tool went back to
                            // its defaults, or it keeps painting with the size
                            // the user just cleared.
                            w.emit(Intent::ResetToolOptions(tool));
                        }
                        separator(ui);
                        // W9-L: Free Transform's numeric row reads the live
                        // session back, so it is drawn by `transform_row`.
                        if tool == ToolId::FreeTransform {
                            transform_row(w, ui, &specs);
                        } else {
                            for spec in &specs {
                                option_control(w, ui, tool, spec);
                            }
                        }
                        // W9-L: the Move tool's Align / Distribute buttons.
                        if tool == ToolId::Move {
                            separator(ui);
                            move_align_row(w, ui);
                        }
                        // W16-F: Path Select's Arrange / Delete buttons.
                        if tool == ToolId::PathSelect {
                            separator(ui);
                            path_arrange_row(w, ui);
                        }
                        if wants_gradient_stops(info) {
                            separator(ui);
                            gradient_control(w, ui, tool);
                        }
                        // W16-K: Refine Edge / Select Subject, Warp / Convert,
                        // the brush presets, Crop by and the artboard + row.
                        w16k::trailing_row(w, ui, tool);
                    });
                });
        });
}

// ---------------------------------------------------------------------------
// W9-L: Free Transform's numeric row and the Move tool's align buttons
// ---------------------------------------------------------------------------

/// W9-L: the Move options bar's pseudo-keys for its six Align buttons, under
/// which each is marked (`ids::tool_option(ToolId::Move, ..)`), in
/// Photopea's order: left, centre, right, top, middle, bottom.
pub const MOVE_ALIGN_KEYS: [(crate::menu::AlignEdge, &str); 6] = [
    (crate::menu::AlignEdge::Left, "align_left"),
    (crate::menu::AlignEdge::HorizontalCenter, "align_centre"),
    (crate::menu::AlignEdge::Right, "align_right"),
    (crate::menu::AlignEdge::Top, "align_top"),
    (crate::menu::AlignEdge::VerticalCenter, "align_middle"),
    (crate::menu::AlignEdge::Bottom, "align_bottom"),
];

/// W9-L: the Move options bar's two Distribute buttons and their pseudo-keys.
pub const MOVE_DISTRIBUTE_KEYS: [(crate::menu::DistributeAxis, &str); 2] = [
    (
        crate::menu::DistributeAxis::Horizontal,
        "distribute_horizontal",
    ),
    (crate::menu::DistributeAxis::Vertical, "distribute_vertical"),
];

/// W9-L: Align left / centre / right / top / middle / bottom and Distribute
/// horizontally / vertically. Each button raises the Layer menu's own
/// action ([`MenuAction::AlignLayers`] / [`MenuAction::DistributeLayers`]),
/// so the bar and the menu run one command and cannot drift; the button
/// captions are the menu items' own labels.
fn move_align_row(w: &mut Workspace, ui: &mut Ui) {
    ui.label(hint(ui, crate::strings::tr("ui.toolbar.align")));
    for (edge, key) in MOVE_ALIGN_KEYS {
        let action = MenuAction::AlignLayers(edge);
        let response = super::labelled_button(
            ui,
            &action.label(),
            true,
            super::ids::tool_option(ToolId::Move, key),
        );
        if response.clicked() {
            w.emit(Intent::Action(action));
        }
    }
    separator(ui);
    ui.label(hint(ui, crate::strings::tr("ui.toolbar.distribute")));
    for (axis, key) in MOVE_DISTRIBUTE_KEYS {
        let action = MenuAction::DistributeLayers(axis);
        let response = super::labelled_button(
            ui,
            &action.label(),
            true,
            super::ids::tool_option(ToolId::Move, key),
        );
        if response.clicked() {
            w.emit(Intent::Action(action));
        }
    }
    // W13-I: Quick Export — the File menu's own action, so the bar and the
    // menu write the same PNG of the active layer.
    separator(ui);
    let action = MenuAction::QuickExportLayer;
    let response = super::labelled_button(
        ui,
        &action.label(),
        true,
        super::ids::tool_option(ToolId::Move, MOVE_QUICK_EXPORT_KEY),
    );
    if response.clicked() {
        w.emit(Intent::Action(action));
    }
}

/// W16-F: Path Select's Arrange and Delete buttons: each operation, its
/// pseudo-key (`ids::tool_option(ToolId::PathSelect, ..)`) and its caption's
/// string key, in Photopea's order.
pub const PATH_ARRANGE_KEYS: [(tools::path_select::ComponentOp, &str, &str); 5] = [
    (
        tools::path_select::ComponentOp::BringToFront,
        "path_bring_to_front",
        "ui.toolbar.path.bring.to.front",
    ),
    (
        tools::path_select::ComponentOp::BringForward,
        "path_bring_forward",
        "ui.toolbar.path.bring.forward",
    ),
    (
        tools::path_select::ComponentOp::SendBackward,
        "path_send_backward",
        "ui.toolbar.path.send.backward",
    ),
    (
        tools::path_select::ComponentOp::SendToBack,
        "path_send_to_back",
        "ui.toolbar.path.send.to.back",
    ),
    (
        tools::path_select::ComponentOp::Delete,
        "path_delete",
        "ui.toolbar.path.delete",
    ),
];

/// W16-F: reorder or delete the selected path components. A press parks the
/// operation for the live Path Select tool
/// (`tools::path_select::request_component_op`) and raises the confirm
/// Enter raises ([`Intent::ConfirmTool`]), which performs it on the
/// components the tool holds selected, as one step.
fn path_arrange_row(w: &mut Workspace, ui: &mut Ui) {
    ui.label(hint(ui, crate::strings::tr("ui.toolbar.path.arrange")));
    for (op, key, label) in PATH_ARRANGE_KEYS {
        let response = super::labelled_button(
            ui,
            crate::strings::tr(label),
            true,
            super::ids::tool_option(ToolId::PathSelect, key),
        );
        if response.clicked() {
            tools::path_select::request_component_op(op);
            w.emit(Intent::ConfirmTool);
        }
    }
}

/// W13-F: the Slice options bar's Slices From Guides button's pseudo-key.
pub const SLICES_FROM_GUIDES_KEY: &str = "slices_from_guides";

/// W13-I: the Move options bar's Quick Export button's pseudo-key.
pub const MOVE_QUICK_EXPORT_KEY: &str = "quick_export";

/// W9-L: write a whole set of numeric fields for Free Transform and bump its
/// edit counter, so the tool applies them together
/// (`tools::transform::TransformTool::apply_pending_numeric`). Every field is
/// written, not only the edited one: the fields are absolute, and the ones
/// the user did not touch must say where the quad is now, not where it was
/// the last time somebody typed.
///
/// The counter only ever moves on to a number no earlier edit used: the
/// options-bar Reset puts the held counter back to its default 0, while a
/// live session still remembers the last number it applied, so the next
/// number is one past the higher of the held counter and the highest this
/// bar ever wrote (kept in egui memory). The tool applies on any change of
/// the counter, not only an increase.
pub(crate) fn commit_numeric(
    w: &mut Workspace,
    ctx: &egui::Context,
    n: tools::transform::NumericTransform,
) {
    use tools::transform::keys;
    let tool = ToolId::FreeTransform;
    let set = |w: &mut Workspace, key: &'static str, value: OptionValue| {
        if w.options.set(tool, key, value) {
            w.emit(Intent::SetToolOption { tool, key, value });
        }
    };
    set(w, keys::REFERENCE, OptionValue::Choice(n.reference));
    set(w, keys::X, OptionValue::Float(n.x));
    set(w, keys::Y, OptionValue::Float(n.y));
    set(w, keys::W, OptionValue::Float(n.w));
    set(w, keys::H, OptionValue::Float(n.h));
    set(w, keys::ANGLE, OptionValue::Float(n.angle));
    set(w, keys::SKEW_H, OptionValue::Float(n.skew_h));
    set(w, keys::SKEW_V, OptionValue::Float(n.skew_v));
    let held = w
        .options
        .get(tool, keys::NUMERIC_SEQ)
        .and_then(OptionValue::as_int)
        .unwrap_or(0);
    let high_id = egui::Id::new(("raster-w9l-transform-seq-high", tool));
    let high: i32 = ctx.data(|d| d.get_temp(high_id)).unwrap_or(0);
    let seq = held.max(high).saturating_add(1);
    ctx.data_mut(|d| d.insert_temp(high_id, seq));
    set(w, keys::NUMERIC_SEQ, OptionValue::Int(seq));
}

/// W9-L: Free Transform's options bar — Mode, the reference-point grid,
/// X / Y, W / H %, Link, Angle, H / V Skew, Interpolation, and the Warp
/// preset with its Bend.
///
/// The numeric fields show the LIVE session read back at the chosen
/// reference point (`NumericTransform::read` over the published
/// `canvas.sessions.transform`), and are off while no transform is live.
/// Editing one writes the whole set plus the edit counter
/// ([`commit_numeric`]). Picking a Warp preset (or moving Bend under one)
/// does the same and puts the Mode on Warp.
fn transform_row(w: &mut Workspace, ui: &mut Ui, specs: &[OptionSpec]) {
    use tools::transform::{keys, NumericTransform, REFERENCE_CENTRE};
    let tool = ToolId::FreeTransform;
    let spec = |key: &str| specs.iter().find(|s| s.key == key).copied();
    if let Some(s) = spec("mode") {
        option_control(w, ui, tool, &s);
    }
    separator(ui);

    let reference = w
        .options
        .get(tool, keys::REFERENCE)
        .and_then(OptionValue::as_choice)
        .unwrap_or(REFERENCE_CENTRE)
        .min(8);
    let live = w
        .canvas
        .sessions
        .transform
        .as_ref()
        .map(|(state, _)| NumericTransform::read(state, reference));
    // The set this bar last wrote, the edit counter it wrote it under and
    // the read-back of the quad it was written over. While the published
    // quad is still that one (the application has not applied the edit
    // yet), the fields show — and the next edit builds on — what was
    // typed, so a second edit never overwrites the first with the stale
    // read-back. Once the quad moves (applied, or dragged) it is the truth.
    let pending_id = egui::Id::new(("raster-w9l-transform-pending", tool));
    let held_seq = w
        .options
        .get(tool, keys::NUMERIC_SEQ)
        .and_then(OptionValue::as_int)
        .unwrap_or(0);
    let pending: Option<(i32, NumericTransform, NumericTransform)> =
        ui.data(|d| d.get_temp(pending_id));
    let shown = match (live, pending) {
        (Some(now), Some((seq, typed, over))) if seq == held_seq && now == over => Some(typed),
        _ => live,
    };
    let remember = |w: &Workspace, ui: &Ui, typed: NumericTransform| {
        if let Some(over) = live {
            let seq = w
                .options
                .get(tool, keys::NUMERIC_SEQ)
                .and_then(OptionValue::as_int)
                .unwrap_or(0);
            ui.data_mut(|d| d.insert_temp(pending_id, (seq, typed, over)));
        }
    };
    reference_grid(w, ui, reference);

    type Field = fn(&mut NumericTransform) -> &mut f32;
    let fields: [(&'static str, Field); 7] = [
        (keys::X, |n| &mut n.x),
        (keys::Y, |n| &mut n.y),
        (keys::W, |n| &mut n.w),
        (keys::H, |n| &mut n.h),
        (keys::ANGLE, |n| &mut n.angle),
        (keys::SKEW_H, |n| &mut n.skew_h),
        (keys::SKEW_V, |n| &mut n.skew_v),
    ];
    for (key, field) in fields {
        let Some(s) = spec(key) else { continue };
        if key == keys::ANGLE {
            if let Some(link) = spec(keys::LINK) {
                option_control(w, ui, tool, &link);
            }
        }
        let OptionKind::Float { min, max, default } = s.kind else {
            continue;
        };
        let mut value = match shown {
            Some(mut n) => *field(&mut n),
            None => w
                .options
                .get(tool, key)
                .and_then(OptionValue::as_float)
                .unwrap_or(default),
        };
        ui.label(hint(ui, s.label));
        let response = ui.add_enabled(
            live.is_some(),
            egui::DragValue::new(&mut value)
                .range(min..=max)
                .speed(0.5)
                .max_decimals(2),
        );
        super::mark(ui, response.rect, super::ids::tool_option(tool, key));
        if response.changed() {
            if let Some(mut n) = shown {
                *field(&mut n) = value;
                commit_numeric(w, ui.ctx(), n);
                remember(w, ui, n);
            }
        }
    }
    separator(ui);
    if let Some(s) = spec(keys::INTERPOLATION) {
        option_control(w, ui, tool, &s);
    }
    // W10-J: Content-Aware Scale's Amount; `shown_schema` keeps it only
    // while the Mode is Content-Aware (`TransformMode::option_shown`).
    if let Some(s) = spec(keys::CA_AMOUNT) {
        option_control(w, ui, tool, &s);
    }
    separator(ui);
    let warp_before = w.options.get(tool, keys::WARP);
    let bend_before = w.options.get(tool, keys::BEND);
    for key in [keys::WARP, keys::BEND] {
        if let Some(s) = spec(key) {
            option_control(w, ui, tool, &s);
        }
    }
    let warp = w
        .options
        .get(tool, keys::WARP)
        .and_then(OptionValue::as_choice)
        .unwrap_or(0);
    let warp_changed = w.options.get(tool, keys::WARP) != warp_before;
    let bend_changed = w.options.get(tool, keys::BEND) != bend_before;
    if let Some(n) = shown {
        if warp_changed || (bend_changed && warp > 0) {
            if warp > 0 {
                // The Warp mode's index in the registry's Mode choice.
                let warp_mode = tools::transform::TransformMode::ALL
                    .iter()
                    .position(|m| *m == tools::transform::TransformMode::Warp)
                    .unwrap_or(0);
                if w.options.set(tool, "mode", OptionValue::Choice(warp_mode)) {
                    w.emit(Intent::SetToolOption {
                        tool,
                        key: "mode",
                        value: OptionValue::Choice(warp_mode),
                    });
                }
            }
            commit_numeric(w, ui.ctx(), n);
            remember(w, ui, n);
        }
    }
}

/// W9-L: the reference cells' hover text, index for index with
/// `tools::transform::REFERENCE_LABELS` (row-major from the top left).
const REFERENCE_TIP_KEYS: [&str; 9] = [
    "ui.toolbar.reference.top.left",
    "ui.toolbar.reference.top",
    "ui.toolbar.reference.top.right",
    "ui.toolbar.reference.left",
    "ui.toolbar.reference.centre",
    "ui.toolbar.reference.right",
    "ui.toolbar.reference.bottom.left",
    "ui.toolbar.reference.bottom",
    "ui.toolbar.reference.bottom.right",
];

/// W9-L: the 3 x 3 reference-point grid. Each cell is marked
/// `ids::tool_option_choice(FreeTransform, "reference", i)`; a click picks
/// that point, and the X / Y fields then read the quad at it.
fn reference_grid(w: &mut Workspace, ui: &mut Ui, reference: usize) {
    use tools::transform::keys;
    let tool = ToolId::FreeTransform;
    let t = current_tokens(ui);
    let side = t.metrics.control_height;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    let cell = side / 3.0;
    let stroke = egui::Stroke::new(
        t.borders.hairline,
        color32(t.palette.color(ColorRole::ControlStroke)),
    );
    for (i, tip_key) in REFERENCE_TIP_KEYS.iter().enumerate() {
        let min = rect.min + Vec2::new((i % 3) as f32 * cell, (i / 3) as f32 * cell);
        let r = egui::Rect::from_min_size(min, Vec2::splat(cell));
        let response = ui.interact(
            r,
            super::ids::tool_option_choice(tool, keys::REFERENCE, i),
            Sense::click(),
        );
        if ui.is_rect_visible(r) {
            let dot = r.shrink(Space::Hair.pt());
            if i == reference {
                ui.painter().rect_filled(
                    dot,
                    egui::Rounding::ZERO,
                    color32(t.palette.color(ColorRole::Accent)),
                );
            } else if response.hovered() {
                ui.painter().rect_filled(
                    dot,
                    egui::Rounding::ZERO,
                    color32(t.palette.color(ColorRole::ControlFillHovered)),
                );
            }
            ui.painter().rect_stroke(dot, egui::Rounding::ZERO, stroke);
        }
        let response = response.on_hover_text(crate::strings::tr(tip_key));
        if response.clicked() && i != reference {
            let value = OptionValue::Choice(i);
            if w.options.set(tool, keys::REFERENCE, value) {
                w.emit(Intent::SetToolOption {
                    tool,
                    key: keys::REFERENCE,
                    value,
                });
            }
        }
    }
    super::mark(ui, rect, super::ids::tool_option(tool, keys::REFERENCE));
}

/// The Ruler options bar's pseudo-key for its Straighten Layer button, under
/// which the button is marked (`ids::tool_option(ToolId::Ruler, ..)`).
pub const STRAIGHTEN_KEY: &str = "straighten";

/// W4-G: the Ruler's **Straighten Layer**: rotate the active layer so the
/// measured line is level, as one undo step. It confirms the ruler's held
/// measurement exactly as Enter does ([`Intent::ConfirmTool`]), and is off
/// while there is no measurement to confirm (the Info panel's Distance row is
/// the same value).
fn straighten_button(w: &mut Workspace, ui: &mut Ui) {
    let measured = w.info.measure.is_some();
    let response = super::labelled_button(
        ui,
        crate::strings::tr("ui.toolbar.straighten.layer"),
        measured,
        super::ids::tool_option(ToolId::Ruler, STRAIGHTEN_KEY),
    )
    .on_hover_text(crate::strings::tr(if measured {
        "ui.toolbar.straighten.layer.hint"
    } else {
        "ui.toolbar.straighten.layer.nothing"
    }));
    if measured && response.clicked() {
        w.emit(Intent::ConfirmTool);
    }
}

// W16-K: the rows Photopea's bars carry beyond the tool's options.
#[path = "toolbar_w16k.rs"]
pub mod w16k;
#[cfg(test)]
#[path = "toolbar_w16k_tests.rs"]
mod w16k_tests;

/// W16-C: the options bar's pseudo-key for its Commit (check) button, under
/// which it is marked (`ids::tool_option(tool, COMMIT_KEY)`).
pub const COMMIT_KEY: &str = "commit";

/// W16-C: whether a held edit is waiting for Commit — a Free Transform quad
/// (Warp and Perspective are its modes), a crop box or a pen path, as the
/// application publishes them into the canvas sessions.
pub(crate) fn pending_edit(w: &Workspace) -> bool {
    let s = &w.canvas.sessions;
    s.transform.is_some() || s.crop.is_some() || s.path.is_some()
}

/// W16-C: Photopea's check-mark Commit: the held edit is confirmed exactly
/// as Enter does ([`Intent::ConfirmTool`], the route the Ruler's Straighten
/// Layer already takes).
fn commit_button(w: &mut Workspace, ui: &mut Ui) {
    let tool = w.palette.active();
    let response =
        super::icon_button_id(ui, "check", true, super::ids::tool_option(tool, COMMIT_KEY))
            .on_hover_text(crate::strings::tr("ui.toolbar.commit.hint"));
    if response.clicked() {
        w.emit(Intent::ConfirmTool);
    }
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
        (OptionKind::Float { min, max, .. }, OptionValue::Float(v)) => {
            // W16-C: the field shows and accepts Photopea's numbers
            // (Tolerance 0-255, Opacity 0-100%, Size in px); the tool keeps
            // its own stored value, so what is typed is divided back.
            let display = spec
                .float_display()
                .unwrap_or_else(|| tools::registry::float_display(spec.key, spec.label, min, max));
            let (lo, hi) = (display.shown(min), display.shown(max));
            let mut shown = display.shown(v);
            ui.label(hint(ui, spec.label));
            let response = ui.add_sized(
                Vec2::new(field, t.metrics.control_height),
                egui::DragValue::new(&mut shown)
                    .range(lo..=hi)
                    .speed((hi - lo) / 400.0)
                    .max_decimals(display.decimals)
                    .suffix(unit_suffix(display.unit, spec.label)),
            );
            super::mark(ui, response.rect, id);
            if response.changed() {
                emit(w, OptionValue::Float(display.stored(shown).clamp(min, max)));
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

/// W16-C: the unit a float field carries after its number: `%`, ` px` or
/// `°`, nothing for a bare number — and nothing when the label already names
/// the unit (Free Transform's `W %`).
fn unit_suffix(unit: tools::registry::FloatUnit, label: &str) -> &'static str {
    use tools::registry::FloatUnit;
    if label.contains('%') {
        return "";
    }
    match unit {
        FloatUnit::Plain => "",
        FloatUnit::Percent => crate::strings::tr("ui.toolbar.unit.percent"),
        FloatUnit::Pixels => crate::strings::tr("ui.toolbar.unit.px"),
        FloatUnit::Degrees => crate::strings::tr("ui.toolbar.unit.degrees"),
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
    let response = response.on_hover_text(crate::strings::tr("ui.toolbar.gradient.stops"));
    if response.clicked() {
        w.emit(Intent::OpenGradientEditor);
    }
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
        // W5-E: one swatch per well, read for both gestures. Drawing a second
        // swatch for the double-click painted every well twice, side by side.
        use crate::panels::color::ColorWell;
        for (well, rgba, tip) in [
            (
                ColorWell::Foreground,
                w.color.foreground(),
                "ui.toolbar.foreground.picker",
            ),
            (
                ColorWell::Background,
                w.color.background(),
                "ui.toolbar.background.picker",
            ),
        ] {
            let response =
                swatch(ui, rgba, side, Sense::click()).on_hover_text(crate::strings::tr(tip));
            if response.clicked() {
                w.color.editing = well;
            }
            if response.double_clicked() {
                w.emit(Intent::OpenColorPicker(well));
            }
        }
        if super::icon_toggle(
            ui,
            "swap",
            false,
            crate::strings::tr("ui.toolbar.swap.colours.x"),
        )
        .clicked()
        {
            w.color.swap();
            w.emit(Intent::SetForeground(w.color.foreground()));
            w.emit(Intent::SetBackground(w.color.background()));
        }
        if super::icon_toggle(
            ui,
            "colors-default",
            false,
            crate::strings::tr("ui.toolbar.default.colours.d.2"),
        )
        .clicked()
        {
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

    /// W5-E: each colour well is one swatch. The palette foot and the Color
    /// panel used to draw a second swatch per well for the double-click, so
    /// four squares were painted where there are two wells.
    #[test]
    fn each_colour_well_is_one_swatch_that_answers_a_click_and_a_double_click() {
        use crate::panels::color::ColorWell;
        let mut w = Workspace::new();
        let fg = [1.0, 0.0, 0.0, 1.0];
        let bg = [0.0, 0.0, 1.0, 1.0];
        w.color.set_well(ColorWell::Foreground, fg);
        w.color.set_well(ColorWell::Background, bg);
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 200.0));
        let run = |w: &mut Workspace, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| color_wells(w, ui));
            })
        };
        let _ = run(&mut w, Vec::new());
        let out = run(&mut w, Vec::new());
        let rects_of = |rgba: [f32; 4]| -> Vec<egui::Rect> {
            out.shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Rect(r) if r.fill == rgba_to_color32(rgba) => Some(r.rect),
                    _ => None,
                })
                .collect()
        };
        let fg_rects = rects_of(fg);
        let bg_rects = rects_of(bg);
        assert_eq!(fg_rects.len(), 1, "foreground swatches: {fg_rects:?}");
        assert_eq!(bg_rects.len(), 1, "background swatches: {bg_rects:?}");

        // The one background swatch takes the click and the double-click.
        let at = bg_rects[0].center();
        let click = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = w.drain_intents();
        let _ = run(&mut w, vec![egui::Event::PointerMoved(at), click(true)]);
        let _ = run(&mut w, vec![click(false)]);
        assert_eq!(w.color.editing, ColorWell::Background);
        let _ = run(&mut w, vec![click(true)]);
        let _ = run(&mut w, vec![click(false)]);
        let intents = w.drain_intents();
        assert!(
            intents.contains(&Intent::OpenColorPicker(ColorWell::Background)),
            "the double-click opened no picker: {intents:?}"
        );
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

/// W9-L: the options bar drawn headless — the real `tool_options` — and
/// driven by pointer events: Free Transform's numeric row against a live
/// session, its reference grid and Warp presets, the Move tool's Align /
/// Distribute buttons, the Mode combo on the Gradient and the Paint Bucket,
/// and the marquee's Exclude and Style.
#[cfg(test)]
mod w9l_tests {
    use super::*;
    use raster::PixelRect;
    use tools::transform::{keys, NumericTransform, TransformMode, TransformState};

    struct Bar {
        ctx: egui::Context,
        w: Workspace,
    }

    impl Bar {
        fn new(tool: ToolId) -> Self {
            let ctx = egui::Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            let mut w = Workspace::new();
            w.absorb(&Intent::SelectTool(tool));
            Self { ctx, w }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Intent> {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(4000.0, 400.0),
                )),
                events,
                ..Default::default()
            };
            let w = &mut self.w;
            let _ = self.ctx.run(input, |ctx| tool_options(w, ctx));
            self.w.drain_intents()
        }

        fn rect(&mut self, id: egui::Id) -> Option<egui::Rect> {
            for _ in 0..3 {
                self.frame(Vec::new());
            }
            self.ctx.read_response(id).map(|r| r.rect)
        }

        fn press(at: egui::Pos2, pressed: bool) -> egui::Event {
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            }
        }

        fn click(&mut self, id: egui::Id) -> Vec<Intent> {
            let at = self
                .rect(id)
                .unwrap_or_else(|| panic!("{id:?} was not drawn"))
                .center();
            self.frame(vec![
                egui::Event::PointerMoved(at),
                Self::press(at, true),
                Self::press(at, false),
            ])
        }

        fn drag(&mut self, id: egui::Id, by: egui::Vec2) -> Vec<Intent> {
            let from = self
                .rect(id)
                .unwrap_or_else(|| panic!("{id:?} was not drawn"))
                .center();
            let mut out = self.frame(vec![
                egui::Event::PointerMoved(from),
                Self::press(from, true),
            ]);
            for step in 1..=4 {
                let at = from + by * (step as f32 / 4.0);
                out.extend(self.frame(vec![egui::Event::PointerMoved(at)]));
            }
            out.extend(self.frame(vec![
                egui::Event::PointerMoved(from + by),
                Self::press(from + by, false),
            ]));
            out
        }
    }

    fn writes(intents: &[Intent]) -> Vec<(&'static str, OptionValue)> {
        intents
            .iter()
            .filter_map(|i| match i {
                Intent::SetToolOption { key, value, .. } => Some((*key, *value)),
                _ => None,
            })
            .collect()
    }

    fn last_write(intents: &[Intent], key: &str) -> Option<OptionValue> {
        writes(intents)
            .into_iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    fn with_session(bar: &mut Bar) -> TransformState {
        let state = TransformState::new(PixelRect::new(0, 0, 200, 100));
        bar.w.canvas.sessions.transform = Some((state.clone(), TransformMode::Scale));
        state
    }

    fn tool_setting(value: OptionValue) -> tools::ToolSetting {
        match value {
            OptionValue::Float(v) => tools::ToolSetting::Float(v),
            OptionValue::Int(v) => tools::ToolSetting::Int(v),
            OptionValue::Bool(v) => tools::ToolSetting::Bool(v),
            OptionValue::Choice(v) => tools::ToolSetting::Choice(v),
            OptionValue::Color(v) => tools::ToolSetting::Color(v),
        }
    }

    #[test]
    fn the_free_transform_row_draws_every_field_and_the_grid() {
        let mut bar = Bar::new(ToolId::FreeTransform);
        with_session(&mut bar);
        let tool = ToolId::FreeTransform;
        for key in [
            "mode",
            keys::REFERENCE,
            keys::X,
            keys::Y,
            keys::W,
            keys::H,
            keys::LINK,
            keys::ANGLE,
            keys::SKEW_H,
            keys::SKEW_V,
            keys::INTERPOLATION,
            keys::WARP,
            keys::BEND,
        ] {
            let rect = bar.rect(super::super::ids::tool_option(tool, key));
            assert!(rect.is_some_and(|r| r.width() > 0.0), "{key} is not drawn");
        }
        // The nine reference cells tile the grid's square, row-major.
        let grid = bar
            .rect(super::super::ids::tool_option(tool, keys::REFERENCE))
            .unwrap();
        for i in 0..9 {
            let cell = bar
                .rect(super::super::ids::tool_option_choice(
                    tool,
                    keys::REFERENCE,
                    i,
                ))
                .unwrap_or_else(|| panic!("reference cell {i}"));
            assert!(
                grid.expand(0.5).contains_rect(cell),
                "cell {i} outside the grid"
            );
            let (col, row) = ((i % 3) as f32, (i / 3) as f32);
            assert!((cell.left() - grid.left() - col * cell.width()).abs() < 0.5);
            assert!((cell.top() - grid.top() - row * cell.height()).abs() < 0.5);
        }
        // The edit counter is never drawn: it is the bar's own bookkeeping.
        assert!(bar
            .rect(super::super::ids::tool_option(tool, keys::NUMERIC_SEQ))
            .is_none());
    }

    #[test]
    fn dragging_w_writes_the_whole_set_and_the_tool_lands_on_it() {
        let mut bar = Bar::new(ToolId::FreeTransform);
        let state = with_session(&mut bar);
        let tool = ToolId::FreeTransform;
        let intents = bar.drag(
            super::super::ids::tool_option(tool, keys::W),
            egui::vec2(40.0, 0.0),
        );
        let w = last_write(&intents, keys::W)
            .and_then(OptionValue::as_float)
            .expect("W was written");
        assert!(w > 100.0, "dragging right grew W: {w}");
        // The untouched fields went with it, read back from the live quad —
        // the centre of 0..200 x 0..100 — and the counter moved.
        assert_eq!(
            last_write(&intents, keys::X),
            Some(OptionValue::Float(100.0))
        );
        assert_eq!(
            last_write(&intents, keys::Y),
            Some(OptionValue::Float(50.0))
        );
        assert!(matches!(
            last_write(&intents, keys::NUMERIC_SEQ),
            Some(OptionValue::Int(n)) if n >= 1
        ));

        // Those writes, converted at the shell's boundary and handed to a
        // live Free Transform the way a press hands them, move its quad to
        // exactly the W the bar wrote.
        let mut tiles = tools::MemoryTiles::new();
        let mut ctx = tools::ToolContext::new(&mut tiles, state.source);
        let mut ft = tools::registry::make(tool);
        let far = tools::PointerEvent::at(900.0, 900.0);
        ft.on_pointer_down(&mut ctx, far).unwrap();
        ft.on_pointer_up(&mut ctx, far).unwrap();
        for (key, value) in bar.w.options.held(tool) {
            ft.set_setting(&key, tool_setting(value)).unwrap();
        }
        ft.on_pointer_down(&mut ctx, far).unwrap();
        let Some(tools::SessionGeometry::Transform { state: live, .. }) = ft.live_geometry() else {
            panic!("no live transform")
        };
        let back = NumericTransform::read(&live, tools::transform::REFERENCE_CENTRE);
        assert!((back.w - w).abs() < 1e-3, "tool W {} vs bar W {w}", back.w);
        assert!((back.h - 100.0).abs() < 1e-3, "H untouched: {}", back.h);
    }

    /// Round 2: an edit the application has not applied yet (the published
    /// quad is still the old one) is what the fields show and what the next
    /// edit builds on: W then H, with the quad never moving, writes both.
    #[test]
    fn a_second_edit_before_the_quad_moves_keeps_the_first() {
        let mut bar = Bar::new(ToolId::FreeTransform);
        with_session(&mut bar);
        let tool = ToolId::FreeTransform;
        bar.drag(
            super::super::ids::tool_option(tool, keys::W),
            egui::vec2(-40.0, 0.0),
        );
        let typed_w = bar
            .w
            .options
            .get(tool, keys::W)
            .and_then(OptionValue::as_float)
            .expect("W was written");
        assert!(typed_w < 100.0, "{typed_w}");
        let intents = bar.drag(
            super::super::ids::tool_option(tool, keys::H),
            egui::vec2(-40.0, 0.0),
        );
        let h = last_write(&intents, keys::H)
            .and_then(OptionValue::as_float)
            .expect("H was written");
        assert!(h < 100.0, "{h}");
        assert_eq!(
            bar.w.options.get(tool, keys::W),
            Some(OptionValue::Float(typed_w)),
            "the H edit overwrote the typed W with the stale read-back"
        );
        // Once the quad moves (the application applied it), the live
        // read-back is the truth again.
        let mut moved = TransformState::new(PixelRect::new(0, 0, 200, 100));
        moved.corners = [
            glam::Vec2::new(10.0, 0.0),
            glam::Vec2::new(210.0, 0.0),
            glam::Vec2::new(210.0, 100.0),
            glam::Vec2::new(10.0, 100.0),
        ];
        bar.w.canvas.sessions.transform = Some((moved, TransformMode::Scale));
        let intents = bar.drag(
            super::super::ids::tool_option(tool, keys::ANGLE),
            egui::vec2(40.0, 0.0),
        );
        assert_eq!(
            last_write(&intents, keys::X),
            Some(OptionValue::Float(110.0)),
            "the moved quad's centre"
        );
        assert_eq!(
            last_write(&intents, keys::W),
            Some(OptionValue::Float(100.0)),
            "the moved quad's W"
        );
    }

    #[test]
    fn with_no_live_transform_the_numeric_fields_do_nothing() {
        let mut bar = Bar::new(ToolId::FreeTransform);
        let intents = bar.drag(
            super::super::ids::tool_option(ToolId::FreeTransform, keys::W),
            egui::vec2(40.0, 0.0),
        );
        assert!(writes(&intents).is_empty(), "{intents:?}");
    }

    #[test]
    fn a_reference_cell_click_picks_that_point() {
        let mut bar = Bar::new(ToolId::FreeTransform);
        with_session(&mut bar);
        let tool = ToolId::FreeTransform;
        let intents = bar.click(super::super::ids::tool_option_choice(
            tool,
            keys::REFERENCE,
            0,
        ));
        assert_eq!(
            writes(&intents),
            vec![(keys::REFERENCE, OptionValue::Choice(0))]
        );
        // X now reads the top-left corner: a W drag writes X = 0, not 100.
        let intents = bar.drag(
            super::super::ids::tool_option(tool, keys::W),
            egui::vec2(40.0, 0.0),
        );
        assert_eq!(last_write(&intents, keys::X), None, "X = 0 is the default");
        assert_eq!(
            bar.w.options.get(tool, keys::X),
            Some(OptionValue::Float(0.0))
        );
    }

    #[test]
    fn a_warp_preset_pick_writes_the_preset_puts_the_mode_on_warp_and_bumps_the_counter() {
        let mut bar = Bar::new(ToolId::FreeTransform);
        with_session(&mut bar);
        let tool = ToolId::FreeTransform;
        let arc = tools::transform::WARP_PRESET_LABELS
            .iter()
            .position(|l| *l == "Arc")
            .unwrap();
        bar.click(super::super::ids::tool_option(tool, keys::WARP));
        let intents = bar.click(super::super::ids::tool_option_choice(tool, keys::WARP, arc));
        assert_eq!(
            last_write(&intents, keys::WARP),
            Some(OptionValue::Choice(arc))
        );
        let warp_mode = TransformMode::ALL
            .iter()
            .position(|m| *m == TransformMode::Warp)
            .unwrap();
        assert_eq!(
            last_write(&intents, "mode"),
            Some(OptionValue::Choice(warp_mode))
        );
        assert_eq!(
            last_write(&intents, keys::NUMERIC_SEQ),
            Some(OptionValue::Int(1))
        );
    }

    #[test]
    fn the_move_bar_align_and_distribute_buttons_raise_the_layer_menu_actions() {
        use crate::menu::{AlignEdge, DistributeAxis};
        let mut bar = Bar::new(ToolId::Move);
        let mut lefts = Vec::new();
        for (edge, key) in MOVE_ALIGN_KEYS {
            let id = super::super::ids::tool_option(ToolId::Move, key);
            lefts.push(bar.rect(id).expect("drawn").left());
            let intents = bar.click(id);
            assert_eq!(
                intents,
                vec![Intent::Action(MenuAction::AlignLayers(edge))],
                "{key}"
            );
        }
        assert!(
            lefts.windows(2).all(|p| p[0] < p[1]),
            "left to right: {lefts:?}"
        );
        assert_eq!(
            MOVE_ALIGN_KEYS.map(|(e, _)| e),
            [
                AlignEdge::Left,
                AlignEdge::HorizontalCenter,
                AlignEdge::Right,
                AlignEdge::Top,
                AlignEdge::VerticalCenter,
                AlignEdge::Bottom
            ]
        );
        for (axis, key) in MOVE_DISTRIBUTE_KEYS {
            let intents = bar.click(super::super::ids::tool_option(ToolId::Move, key));
            assert_eq!(
                intents,
                vec![Intent::Action(MenuAction::DistributeLayers(axis))]
            );
        }
        assert_eq!(
            MOVE_DISTRIBUTE_KEYS.map(|(a, _)| a),
            [DistributeAxis::Horizontal, DistributeAxis::Vertical]
        );
        // W13-I: Quick Export, right of the Distribute pair, raises the File
        // menu's Quick Export Layer as PNG.
        let quick = super::super::ids::tool_option(ToolId::Move, MOVE_QUICK_EXPORT_KEY);
        let last = super::super::ids::tool_option(ToolId::Move, MOVE_DISTRIBUTE_KEYS[1].1);
        assert!(
            bar.rect(quick).expect("Quick Export is drawn").left()
                > bar.rect(last).expect("drawn").left()
        );
        assert_eq!(
            bar.click(quick),
            vec![Intent::Action(MenuAction::QuickExportLayer)]
        );
        // The buttons are the Move tool's alone.
        let mut brush = Bar::new(ToolId::Brush);
        assert!(brush
            .rect(super::super::ids::tool_option(ToolId::Move, "align_left"))
            .is_none());
    }

    /// W16-F: the Path Select bar draws Arrange (front, forward, backward,
    /// back) and Delete, left to right; a press parks the operation for the
    /// live tool and raises the confirm Enter raises.
    #[test]
    fn the_path_select_bar_arranges_and_deletes_through_the_confirm() {
        use tools::path_select::{pending_component_op, ComponentOp};
        let mut bar = Bar::new(ToolId::PathSelect);
        let mut lefts = Vec::new();
        for (op, key, label) in PATH_ARRANGE_KEYS {
            assert_ne!(crate::strings::tr(label), "", "{label} has a caption");
            let id = super::super::ids::tool_option(ToolId::PathSelect, key);
            lefts.push(bar.rect(id).expect("drawn").left());
            let intents = bar.click(id);
            assert_eq!(intents, vec![Intent::ConfirmTool], "{key}");
            assert_eq!(pending_component_op(), Some(op), "{key}");
        }
        assert!(
            lefts.windows(2).all(|p| p[0] < p[1]),
            "left to right: {lefts:?}"
        );
        assert_eq!(
            PATH_ARRANGE_KEYS.map(|(op, _, _)| op),
            ComponentOp::ALL,
            "every operation has a button"
        );
    }

    #[test]
    fn the_gradient_and_the_bucket_are_offered_the_mode_and_pattern_fill_is_not() {
        let multiply = layer_model::BlendMode::ALL
            .iter()
            .position(|m| *m == layer_model::BlendMode::Multiply)
            .unwrap();
        for tool in [ToolId::Gradient, ToolId::PaintBucket] {
            let mut bar = Bar::new(tool);
            let id = super::super::ids::tool_option(tool, BLEND_MODE_KEY);
            assert!(bar.rect(id).is_some(), "{tool:?} has no Mode combo");
            bar.click(id);
            let intents = bar.click(super::super::ids::tool_option_choice(
                tool,
                BLEND_MODE_KEY,
                multiply,
            ));
            assert_eq!(
                writes(&intents),
                vec![(BLEND_MODE_KEY, OptionValue::Choice(multiply))],
                "{tool:?}"
            );
            // ...and the tool the registry builds answers the forwarded key.
            let mut t = tools::registry::make(tool);
            for (key, value) in bar.w.options.held(tool) {
                t.set_setting(&key, tool_setting(value))
                    .unwrap_or_else(|e| panic!("{tool:?} refused {key}: {e}"));
            }
        }
        let mut bar = Bar::new(ToolId::PatternFill);
        assert!(bar
            .rect(super::super::ids::tool_option(
                ToolId::PatternFill,
                BLEND_MODE_KEY
            ))
            .is_none());
    }

    #[test]
    fn the_marquee_offers_exclude_and_a_style_with_its_width_and_height() {
        let tool = ToolId::RectMarquee;
        let mut bar = Bar::new(tool);
        for key in ["style", "style_width", "style_height"] {
            assert!(
                bar.rect(super::super::ids::tool_option(tool, key))
                    .is_some(),
                "{key} is not drawn"
            );
        }
        let exclude = tools::select::SELECTION_MODE_LABELS
            .iter()
            .position(|l| *l == "Exclude")
            .unwrap();
        bar.click(super::super::ids::tool_option(tool, "mode"));
        let intents = bar.click(super::super::ids::tool_option_choice(tool, "mode", exclude));
        assert_eq!(
            writes(&intents),
            vec![("mode", OptionValue::Choice(exclude))]
        );
        let fixed = tools::select::MARQUEE_STYLE_LABELS
            .iter()
            .position(|l| *l == "Fixed Size")
            .unwrap();
        bar.click(super::super::ids::tool_option(tool, "style"));
        let intents = bar.click(super::super::ids::tool_option_choice(tool, "style", fixed));
        assert_eq!(
            writes(&intents),
            vec![("style", OptionValue::Choice(fixed))]
        );
    }
}

// W16-C: the float units, the bucket's Fill source and the Commit check,
// through headless frames of the bar. Declared last so the style gate's
// shipping-code scan (it stops at the first test marker) reads the whole bar.
#[cfg(test)]
#[path = "toolbar_w16c_tests.rs"]
mod w16c_tests;
