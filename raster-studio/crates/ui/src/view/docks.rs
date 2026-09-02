//! The docked panels: the rails, the headers, and each panel's body.
//!
//! A rail is drawn only when something is open on it — an empty strip of chrome
//! is worse than no strip. Each panel gets a header with a twirl-down and a
//! close button, and its body is dispatched by [`PanelId`] to a `fn(&mut
//! Workspace, &mut Ui, ...)` below.

use design::{
    color32, current_tokens, egui_theme::rounding, ColorRole, Radius, Space, TextRole, TypeRole,
};
use editor_core::{Command, Document, History, LayerPatch};
use egui::{Align, Layout, Sense, Ui, Vec2};
use layer_model::{BlendMode, LayerId};

use crate::dock::{DockSide, PanelId, MAX_DOCK_WIDTH, MIN_DOCK_WIDTH};
use crate::intent::Intent;
use crate::menu::AdjustmentId;
use crate::panels::channels::{ChannelKind, PathsState};
use crate::panels::color::{ColorNotation, ColorWell};
use crate::panels::history::HistoryModel;
use crate::panels::layers::{DropPosition, DropRejection, LayerRow, LayersModel};
use crate::panels::navigator::{format_zoom, ViewBox};
use crate::panels::properties::{
    AdjustmentsPanel, MaskProperties, PropertiesSubject, PropertyFocus,
};
use crate::panels::text as text_panel;
use crate::Workspace;

use super::{
    badge, body, empty_state, hairline, hint, icon_toggle, icon_toggle_id, panel_frame, row_layout,
    swatch, text,
};

/// Draw every rail.
pub fn docks(w: &mut Workspace, ctx: &egui::Context, doc: &Document, history: &History) {
    for side in [DockSide::Left, DockSide::Right] {
        rail(w, ctx, doc, history, side);
    }
    bottom_rail(w, ctx, doc, history);
}

fn rail(w: &mut Workspace, ctx: &egui::Context, doc: &Document, history: &History, side: DockSide) {
    if w.dock.side_is_empty(side) {
        return;
    }
    let t = design::current_theme(ctx).tokens();
    if w.dock.side_is_collapsed(side) {
        return icon_rail(w, ctx, side);
    }
    let id = match side {
        DockSide::Left => "raster-dock-left",
        DockSide::Right => "raster-dock-right",
        DockSide::Bottom => return,
    };
    let builder = match side {
        DockSide::Left => egui::SidePanel::left(id),
        _ => egui::SidePanel::right(id),
    };
    let response = builder
        .resizable(true)
        .default_width(w.dock.side_extent(side))
        .width_range(MIN_DOCK_WIDTH..=MAX_DOCK_WIDTH)
        .frame(egui::Frame::none().fill(color32(t.palette.color(ColorRole::SurfacePanel))))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (_, members) in w.dock.groups_on(side) {
                        panel_group(w, ui, doc, history, &members);
                    }
                });
        });
    commit_measure(w, ctx, side, response.response.rect.width());
}

/// Take a rail's measured extent, and commit it only if it is a drag.
///
/// See [`crate::dock::is_resize`] for why "the number changed" is not enough.
fn commit_measure(w: &mut Workspace, ctx: &egui::Context, side: DockSide, measured: f32) {
    let pointer_down = ctx.input(|i| i.pointer.any_down());
    if crate::dock::is_resize(w.rail_measure(side), measured, pointer_down) {
        // A drag: the arrangement is the user's now, not the preset's.
        w.dock.set_side_width(side, measured);
    } else {
        // A measurement. The stored extent still has to follow it — the canvas
        // camera is positioned from it — but the layout identity survives.
        w.dock.sync_side_width(side, measured);
    }
    w.set_rail_measure(side, measured);
}

/// The collapsed dock: one column of panel icons at [`RAIL_WIDTH_PT`].
/// Clicking an icon unfolds the side and brings that panel forward — the
/// round trip is `DockState::set_side_collapsed(false)` plus a raise, so
/// unfolding restores exactly the arrangement that was folded.
fn icon_rail(w: &mut Workspace, ctx: &egui::Context, side: DockSide) {
    let t = design::current_theme(ctx).tokens();
    let id = match side {
        DockSide::Left => "raster-dock-left-rail",
        _ => "raster-dock-right-rail",
    };
    let builder = match side {
        DockSide::Left => egui::SidePanel::left(id),
        _ => egui::SidePanel::right(id),
    };
    let _response = builder
        .resizable(false)
        .exact_width(crate::dock::RAIL_WIDTH_PT)
        .frame(egui::Frame::none().fill(color32(t.palette.color(ColorRole::SurfacePanel))))
        .show(ctx, |ui| {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = Space::Hair.pt();
                for panel in w.dock.panels_on(side).iter().copied() {
                    if !w.dock.is_open(panel) {
                        continue;
                    }
                    if icon_toggle_id(
                        ui,
                        "overflow",
                        false,
                        panel.title(),
                        Some(super::ids::rail_icon(panel)),
                    )
                    .clicked()
                    {
                        w.dock.set_side_collapsed(side, false);
                        w.dock.raise(panel);
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    if icon_toggle_id(
                        ui,
                        "chevron-right",
                        false,
                        crate::strings::tr("ui.docks.expand.the.dock"),
                        Some(super::ids::rail_expand(side)),
                    )
                    .clicked()
                    {
                        w.dock.set_side_collapsed(side, false);
                    }
                });
            });
        });
    // The canvas reads the rail's measure to lay itself out; the collapsed
    // rail commits its fixed width the same way the open dock commits a drag.
    commit_measure(w, ctx, side, crate::dock::RAIL_WIDTH_PT);
}

fn bottom_rail(w: &mut Workspace, ctx: &egui::Context, doc: &Document, history: &History) {
    if w.dock.side_is_empty(DockSide::Bottom) {
        return;
    }
    let t = design::current_theme(ctx).tokens();
    let response = egui::TopBottomPanel::bottom("raster-dock-bottom")
        .resizable(true)
        .default_height(w.dock.bottom_height())
        .frame(egui::Frame::none().fill(color32(t.palette.color(ColorRole::SurfacePanel))))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (_, members) in w.dock.groups_on(DockSide::Bottom) {
                        panel_group(w, ui, doc, history, &members);
                    }
                });
        });
    commit_measure(w, ctx, DockSide::Bottom, response.response.rect.height());
}

/// One tabbed group: a strip of tabs, then the active panel's body.
fn panel_group(
    w: &mut Workspace,
    ui: &mut Ui,
    doc: &Document,
    history: &History,
    members: &[PanelId],
) {
    let active = members
        .iter()
        .copied()
        .find(|p| w.dock.is_active(*p))
        .or(members.first().copied());
    let Some(active) = active else {
        return;
    };
    tab_strip(w, ui, members, active);
    if w.panel_menu == Some(active) {
        move_controls(w, ui, active);
    }
    panel_frame(ui).show(ui, |ui| {
        ui.push_id(active.key(), |ui| {
            body_of(w, ui, doc, history, active);
        });
    });
    hairline(ui);
}

/// The tab strip above one group's body: one tab per member, the active one
/// highlighted, plus the overflow menu for the active panel.
fn tab_strip(w: &mut Workspace, ui: &mut Ui, members: &[PanelId], active: PanelId) {
    let height = crate::dock::DockState::header_height();
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), height),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(Space::Small.pt());
            for panel in members {
                let is_active = *panel == active;
                let t = current_tokens(ui);
                // Drawn by hand with a stable id (`ids::panel_tab`), so a
                // right-click on a header can be named by a test — egui's
                // `Button` derives ids that shift with panel order.
                let font = design::egui_theme::font_id(t, TypeRole::Footnote);
                let galley = ui.painter().layout_no_wrap(
                    panel.title().to_string(),
                    font,
                    color32(t.palette.text(if is_active {
                        TextRole::Primary
                    } else {
                        TextRole::Secondary
                    })),
                );
                let tab_w = galley.size().x + 14.0;
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(tab_w, height - 2.0), Sense::hover());
                let response = ui.interact(rect, super::ids::panel_tab(*panel), Sense::click());
                if is_active {
                    ui.painter().rect_filled(
                        rect,
                        rounding(Radius::Small.resolve(&t.radii, height)),
                        color32(t.palette.color(ColorRole::SurfaceElevated)),
                    );
                }
                let pos = egui::pos2(rect.left() + 7.0, rect.center().y - galley.size().y * 0.5);
                // The galley carries its role colour; egui's fallback tint is
                // the same colour, so nothing is multiplied away.
                ui.painter().galley(
                    pos,
                    galley,
                    color32(t.palette.text(if is_active {
                        TextRole::Primary
                    } else {
                        TextRole::Secondary
                    })),
                );
                if response.clicked() {
                    w.dock.raise(*panel);
                }
                if response.secondary_clicked() {
                    w.panel_menu = Some(*panel);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(Space::XSmall.pt());
                if icon_toggle(
                    ui,
                    "close",
                    false,
                    crate::strings::tr("ui.docks.close.panel"),
                )
                .clicked()
                {
                    w.emit(Intent::SetPanelOpen {
                        panel: active,
                        open: false,
                    });
                }
                let open = w.panel_menu == Some(active);
                if icon_toggle_id(
                    ui,
                    "overflow",
                    open,
                    crate::strings::tr("ui.docks.move.this.panel"),
                    Some(super::ids::panel_menu(active)),
                )
                .clicked()
                {
                    w.panel_menu = if open { None } else { Some(active) };
                }
            });
        },
    );
}

/// The move controls a panel header's overflow button reveals: which side the
/// panel sits on, and where in that side's stack.
///
/// Drawn inline beneath the header rather than in a floating popover, for two
/// reasons: a rail is narrow and a popover over it hides the thing being moved,
/// and every control here gets a stable id so `moving_a_panel_across_sides_
/// through_the_header` can click the real thing.
fn move_controls(w: &mut Workspace, ui: &mut Ui, panel: PanelId) {
    let placement = w.dock.placement(panel);
    let order = w.dock.panels_on(placement.side);
    let at = order.iter().position(|p| *p == panel);
    let mut dock_to: Option<DockSide> = None;
    let mut reorder: Option<bool> = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = Space::Hair.pt();
        ui.label(hint(ui, crate::strings::tr("ui.docks.move.to")));
        for side in DockSide::ALL {
            let here = placement.side == *side;
            let response = super::labelled_button(
                ui,
                side_label(*side),
                !here,
                super::ids::panel_dock(panel, *side),
            )
            .on_hover_text(if here {
                crate::strings::tr("ui.docks.the.panel.is.already.on.this")
            } else {
                crate::strings::tr("ui.docks.dock.this.panel.here")
            });
            if response.clicked() {
                dock_to = Some(*side);
            }
        }
        for (up, key) in [(true, "chevron-up"), (false, "chevron-down")] {
            let can = match (at, up) {
                (Some(i), true) => i > 0,
                (Some(i), false) => i + 1 < order.len(),
                (None, _) => false,
            };
            let response =
                super::icon_button_id(ui, key, can, super::ids::panel_reorder(panel, up))
                    .on_hover_text(if can {
                        crate::strings::tr("ui.docks.move.this.panel.within.its.dock")
                    } else if up {
                        crate::strings::tr("ui.docks.this.panel.is.already.at.the")
                    } else {
                        crate::strings::tr("ui.docks.this.panel.is.already.at.the.2")
                    });
            if response.clicked() {
                reorder = Some(up);
            }
        }
    });
    hairline(ui);

    // Applied after the row is drawn: moving a panel changes the very list the
    // row is iterating, and a control that rearranges itself under the pointer
    // is how a click lands on the wrong thing.
    if let Some(side) = dock_to {
        if w.dock.dock(panel, side) {
            w.emit(Intent::DockPanel { panel, side });
        }
        w.panel_menu = None;
    }
    if let Some(up) = reorder {
        // The intent carries where the panel *landed*, not which way it went.
        // An application that absorbs what it drains is applying this a second
        // time, and "one place up" applied twice is two places up — which is
        // the bug this shape exists to make impossible.
        if let Some(to) = w.dock.reorder(panel, up) {
            w.emit(Intent::ReorderPanel { panel, to });
        }
    }
}

const fn side_label(side: DockSide) -> &'static str {
    match side {
        DockSide::Left => "Left",
        DockSide::Right => "Right",
        DockSide::Bottom => "Bottom",
    }
}

fn body_of(w: &mut Workspace, ui: &mut Ui, doc: &Document, history: &History, panel: PanelId) {
    match panel {
        PanelId::Layers => layers_body(w, ui, doc),
        PanelId::History => history_body(w, ui, history),
        PanelId::Adjustments => adjustments_body(w, ui),
        PanelId::Properties => properties_body(w, ui, doc),
        PanelId::Color => color_body(w, ui),
        PanelId::Swatches => swatches_body(w, ui),
        PanelId::Brushes => brushes_body(w, ui),
        PanelId::Character => character_body(w, ui, doc),
        PanelId::Paragraph => paragraph_body(w, ui, doc),
        PanelId::Navigator => navigator_body(w, ui, doc),
        PanelId::Info => info_body(w, ui, doc),
        PanelId::Channels => channels_body(w, ui, doc),
        PanelId::Paths => paths_body(w, ui, doc),
        PanelId::Actions => actions_body(w, ui),
    }
}

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

fn layers_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    let model = LayersModel::build(doc, &w.layers);
    let active = doc.active_layer();

    blend_and_opacity(w, ui, doc, active);
    lock_row(w, ui, doc, active);
    ui.add_space(Space::XSmall.pt());
    hairline(ui);

    if model.is_empty() {
        empty_state(ui, "No layers yet. Add one with the + button below.");
    } else {
        let rows = model.rows().to_vec();
        // The drop is decided by the drag *as a whole*, not by any one row's
        // response. egui reports `drag_stopped` only on the row the drag began
        // on, and by then the pointer is over some other row — so asking the
        // row under the pointer whether the drag stopped always answers no,
        // and the move is thrown away. Instead: whichever row currently holds
        // the pointer contributes the position, and the release — a fact about
        // the frame, not about a widget — commits it.
        let released = ui.input(|i| i.pointer.any_released());
        let mut hovered: Option<DropPosition> = None;
        for row in &rows {
            let response = layer_row(w, ui, row, &rows);
            if let Some(position) = row_drag_position(w, ui, doc, row, &response) {
                hovered = Some(position);
            }
            if response.secondary_clicked() {
                crate::context_menu::open(
                    w,
                    crate::context_menu::ContextTarget::LayerRow,
                    response
                        .interact_pointer_pos()
                        .unwrap_or_else(|| response.rect.center()),
                );
            }
        }
        if released {
            if let Some(dragged) = w.layers.end_drag() {
                if let Some(position) = hovered {
                    match LayersModel::resolve_drop(doc, dragged, position) {
                        Ok(command) => w.emit(Intent::Document(command)),
                        Err(DropRejection::NoChange) => {}
                        Err(_) => { /* the row already showed a crate::strings::tr("ui.docks.no.drop") cue */
                        }
                    }
                }
            }
        }
    }

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    layer_buttons(w, ui, doc, active);
}

fn blend_and_opacity(w: &mut Workspace, ui: &mut Ui, doc: &Document, active: Option<LayerId>) {
    let layer = active.and_then(|id| doc.layers.get(id));
    let enabled = layer.is_some();
    let mode = layer.map(|l| l.blend_mode).unwrap_or_default();
    let mut opacity = layer.map(|l| l.effective_opacity()).unwrap_or(1.0) * 100.0;
    let mut fill = layer.map(|l| l.effective_fill_opacity()).unwrap_or(1.0) * 100.0;

    ui.add_enabled_ui(enabled, |ui| {
        design::inspector_field(ui, "Blend", |ui| {
            let mut picked = mode;
            let combo = egui::ComboBox::from_id_salt("raster-layer-blend")
                .selected_text(body(ui, mode.label()))
                .show_ui(ui, |ui| {
                    for candidate in BlendMode::ALL {
                        let row =
                            ui.selectable_label(candidate == mode, body(ui, candidate.label()));
                        super::mark(ui, row.rect, super::ids::layer_blend_option(candidate));
                        if row.clicked() {
                            picked = candidate;
                        }
                    }
                });
            super::mark(ui, combo.response.rect, super::ids::layer_blend());
            if picked != mode {
                if let Some(id) = active {
                    w.emit(Intent::Document(LayersModel::set_blend_mode(id, picked)));
                }
            }
        });
        let opacity_row = design::slider_row(ui, "Opacity", &mut opacity, 0.0..=100.0);
        super::mark(ui, opacity_row.rect, super::ids::layer_opacity());
        if opacity_row.changed() {
            if let Some(id) = active {
                if let Some(c) = LayersModel::set_opacity(id, opacity / 100.0) {
                    w.emit(Intent::Document(c));
                }
            }
        }
        let fill_row = design::slider_row(ui, "Fill", &mut fill, 0.0..=100.0);
        super::mark(ui, fill_row.rect, super::ids::layer_fill());
        if fill_row.changed() {
            if let Some(id) = active {
                if let Some(c) = LayersModel::set_fill_opacity(id, fill / 100.0) {
                    w.emit(Intent::Document(c));
                }
            }
        }
    });
}

fn lock_row(w: &mut Workspace, ui: &mut Ui, doc: &Document, active: Option<LayerId>) {
    let Some(id) = active else { return };
    let Some(layer) = doc.layers.get(id) else {
        return;
    };
    let locks = layer.locked;
    ui.horizontal(|ui| {
        ui.label(hint(ui, "Lock"));
        let mut next = locks;
        for toggle in super::LockToggle::ALL {
            let (key, tip) = toggle.icon_and_tooltip();
            let on = toggle.get(locks);
            if icon_toggle_id(ui, key, on, tip, Some(super::ids::layer_lock(toggle))).clicked() {
                toggle.set(&mut next, !on);
            }
        }
        if next != locks {
            w.emit(Intent::Document(LayersModel::set_locks(id, next)));
        }
    });
}

fn layer_row(w: &mut Workspace, ui: &mut Ui, row: &LayerRow, rows: &[LayerRow]) -> egui::Response {
    let t = current_tokens(ui);
    // The thumbnail-size control scales the whole row, not just the well.
    let height = t.metrics.list_row_height * w.layers.thumb_scale.height();
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    // An explicit id rather than the one egui derives from call order: it keeps
    // the row findable from a headless test, and it keeps the row's identity
    // tied to the *layer* rather than to its position, so reordering does not
    // hand one row another's interaction state.
    let response = ui.interact(rect, super::ids::layer_row(row.id), Sense::click_and_drag());

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = Radius::Medium.resolve(&t.radii, height);
        if row.selected || row.active {
            painter.rect_filled(
                rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::SelectionFill)),
            );
        } else if response.hovered() {
            painter.rect_filled(
                rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::ControlFillHovered)),
            );
        }
        if row.active {
            painter.rect_stroke(
                rect,
                rounding(radius),
                egui::Stroke::new(
                    t.borders.hairline,
                    color32(t.palette.color(ColorRole::SelectionStroke)),
                ),
            );
        }
    }

    // Overlay the interactive parts on the row we just reserved.
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(Space::XSmall.pt(), 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    content.add_space(row.depth as f32 * Space::Medium.pt());

    if row.is_group {
        let chevron = if row.expanded {
            "chevron-down"
        } else {
            "chevron-right"
        };
        if icon_toggle(&mut content, chevron, true, "").clicked() {
            w.emit(Intent::SetGroupExpanded {
                layer: row.id,
                expanded: !row.expanded,
            });
            w.layers.set_expanded(row.id, !row.expanded);
        }
    } else {
        content.add_space(current_tokens(&content).metrics.min_hit_target);
    }

    if icon_toggle_id(
        &mut content,
        "eye",
        row.visible,
        "Show / hide layer",
        Some(super::ids::layer_eye(row.id)),
    )
    .clicked()
    {
        w.emit(Intent::Document(LayersModel::set_visible(
            row.id,
            !row.visible,
        )));
    }

    thumbnail(&mut content, w, row);
    content.add_space(Space::XSmall.pt());
    if row.is_clipping {
        let side = current_tokens(&content).metrics.min_hit_target * 0.75;
        let (rect, _) = content.allocate_exact_size(Vec2::splat(side), Sense::hover());
        super::paint_icon(&content, rect, "clipping", TextRole::Tertiary);
    }
    content.label(body(&content, row.name.clone()));

    content.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if row.shows_lock_badge() {
            badge(ui, "lock", false);
        }
        if row.shows_effects_badge() {
            badge(ui, "fx", true);
        }
        if row.shows_mask_badge() {
            badge(
                ui,
                if row.mask_enabled {
                    "mask"
                } else {
                    crate::strings::tr("ui.docks.mask.off")
                },
                true,
            );
        }
    });

    if response.clicked() {
        let modifiers = ui.input(|i| i.modifiers);
        if modifiers.command {
            w.layers.toggle_selected(row.id);
        } else if modifiers.shift {
            // A shift-click ranges over the rows as *drawn*, which is why the
            // whole visible list is passed in rather than re-derived: a
            // collapsed group's children are not on screen and must not be
            // swept into the selection.
            w.layers.select_range(rows, row.id);
        } else {
            w.layers.select_only(row.id);
        }
        let selection = w.layers.selection().to_vec();
        w.emit(Intent::SelectLayers {
            layers: selection,
            active: Some(row.id),
        });
    }
    response
}

/// The 4:3 thumbnail well.
///
/// It shows the layer's real pixels when the application has uploaded a fitted
/// thumbnail ([`Workspace::layer_thumbs`]); otherwise it falls back to the
/// layer's *kind* glyph over the checkerboard. A well that reads "group" or
/// "adjustment" is honest; a blank well is not — and the glyph fallback is
/// also exactly what a headless draw sees, since no application has uploaded
/// textures there.
fn thumbnail(ui: &mut Ui, w: &Workspace, row: &LayerRow) {
    let t = current_tokens(ui);
    let height = (t.metrics.list_row_height * w.layers.thumb_scale.height()) - Space::XSmall.pt();
    let size = Vec2::new(height * 4.0 / 3.0, height);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let radius = Radius::Small.resolve(&t.radii, height);
    super::checkerboard(ui.painter(), rect, Space::XSmall.pt());
    ui.painter().rect_stroke(
        rect,
        rounding(radius),
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::ControlStroke)),
        ),
    );
    if let Some(tex) = w.layer_thumbs.get(&row.id) {
        let r = rect;
        ui.painter().image(
            tex.id(),
            r,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            crate::dialogs::controls::UNTINTED,
        );
        return;
    }
    // Square, centred: the well is 4:3, and an icon stretched to fill it would
    // stop being the same shape as the same icon anywhere else in the chrome.
    let side = rect.height() * 0.7;
    let icon_rect = egui::Rect::from_center_size(rect.center(), Vec2::splat(side));
    super::paint_icon(
        ui,
        icon_rect,
        super::kind_icon(row.class),
        TextRole::Secondary,
    );
}

/// Where in a row a pointer at `y` would drop.
///
/// Top third above, bottom third below, the middle into a group — or below,
/// when the row cannot hold children. Pure geometry, so
/// `the_bands_of_a_row_say_above_inside_and_below` can pin it without a window.
pub(crate) fn drop_position(
    row_is_group: bool,
    id: LayerId,
    rect: egui::Rect,
    y: f32,
) -> DropPosition {
    let f = ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
    if f < 0.33 {
        DropPosition::Above(id)
    } else if f > 0.67 || !row_is_group {
        DropPosition::Below(id)
    } else {
        DropPosition::Into(id)
    }
}

/// Start a drag on this row if one began here, and — when the pointer is over
/// this row mid-drag — return where a release would land and paint the cue.
///
/// The cue is painted only for a drop that would actually happen: an insertion
/// line over a drop the model refuses is a promise the panel cannot keep.
fn row_drag_position(
    w: &mut Workspace,
    ui: &mut Ui,
    doc: &Document,
    row: &LayerRow,
    response: &egui::Response,
) -> Option<DropPosition> {
    if response.drag_started() {
        w.layers.begin_drag(row.id);
    }
    let dragged = w.layers.dragging()?;
    let pointer = ui.ctx().pointer_interact_pos()?;
    let rect = response.rect;
    if !rect.contains(pointer) {
        return None;
    }
    let position = drop_position(row.is_group, row.id, rect, pointer.y);

    if LayersModel::resolve_drop(doc, dragged, position).is_ok() {
        let t = current_tokens(ui);
        let stroke = egui::Stroke::new(
            t.borders.thick,
            color32(t.palette.color(ColorRole::SelectionStroke)),
        );
        match position {
            DropPosition::Above(_) => {
                ui.painter().hline(rect.x_range(), rect.top(), stroke);
            }
            DropPosition::Below(_) => {
                ui.painter().hline(rect.x_range(), rect.bottom(), stroke);
            }
            DropPosition::Into(_) => {
                let radius = Radius::Medium.resolve(&t.radii, rect.height());
                ui.painter().rect_stroke(rect, rounding(radius), stroke);
            }
        }
    }
    Some(position)
}

fn layer_buttons(w: &mut Workspace, ui: &mut Ui, doc: &Document, active: Option<LayerId>) {
    // The kind filter and the thumbnail size, the two controls Photopea puts
    // above the footer row.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Space::Hair.pt();
        if icon_toggle_id(
            ui,
            "overflow",
            w.layers.filter.is_none(),
            crate::strings::tr("ui.docks.show.every.layer"),
            Some(super::ids::layer_filter_all()),
        )
        .clicked()
        {
            w.layers.filter = None;
        }
        for class in crate::menu::LayerClass::ALL {
            let on = w.layers.filter == Some(class);
            if icon_toggle_id(
                ui,
                class_icon(class),
                on,
                &filter_tip(class),
                Some(super::ids::layer_filter(class)),
            )
            .clicked()
            {
                w.layers.filter = if on { None } else { Some(class) };
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_toggle_id(
                ui,
                "plus",
                false,
                crate::strings::tr("ui.docks.thumbnail.size"),
                Some(super::ids::layer_thumb_size()),
            )
            .clicked()
            {
                w.layers.thumb_scale = w.layers.thumb_scale.cycled();
            }
        });
    });

    // Photopea's footer row: link, fx, mask, adjustment, group, new, delete.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Space::Hair.pt();
        let has_layer = active.is_some();
        let selection: Vec<LayerId> = if w.layers.selection().is_empty() {
            active.into_iter().collect()
        } else {
            w.layers.selection().to_vec()
        };

        // Link: every selected layer carries the chain, as one patch per
        // layer. Linking wins if any selected layer is unlinked, so one click
        // on a mixed selection links them all.
        let link_on = selection
            .iter()
            .any(|id| doc.layers.get(*id).is_some_and(|l| l.linked));
        if icon_toggle_id(
            ui,
            "link",
            link_on,
            crate::strings::tr("ui.docks.link.selected.layers"),
            Some(super::ids::layer_link()),
        )
        .clicked()
        {
            for id in &selection {
                if doc.layers.get(*id).is_some() {
                    w.emit(Intent::Document(Command::SetLayerProperties {
                        layer_id: *id,
                        patch: LayerPatch {
                            linked: Some(!link_on),
                            ..LayerPatch::default()
                        },
                    }));
                }
            }
        }

        // Adjustment: the grid lives in its own panel.
        if icon_toggle_id(
            ui,
            "adjustment",
            false,
            crate::strings::tr("ui.docks.open.the.adjustments.panel"),
            Some(super::ids::layer_adjustment()),
        )
        .clicked()
        {
            w.emit(Intent::SetPanelOpen {
                panel: PanelId::Adjustments,
                open: true,
            });
        }

        // fx: the layer-style editor, which is the Properties panel.
        let fx = super::labelled_button(ui, "fx", has_layer, super::ids::layer_fx());
        let fx = if has_layer {
            fx.on_hover_text(crate::strings::tr("ui.docks.blending.options"))
        } else {
            fx.on_disabled_hover_text(crate::strings::tr("ui.docks.select.a.layer.first"))
        };
        if fx.clicked() {
            w.emit(Intent::Action(crate::menu::MenuAction::BlendingOptions));
        }

        // Mask: add one to the active layer.
        let has_mask = active
            .and_then(|id| doc.layers.get(id))
            .is_some_and(|l| l.mask.is_some());
        let mask = icon_toggle_id(
            ui,
            "mask",
            false,
            crate::strings::tr("ui.docks.add.a.layer.mask"),
            Some(super::ids::layer_mask()),
        );
        if has_layer && !has_mask && mask.clicked() {
            if let Some(id) = active {
                w.emit(Intent::Document(LayersModel::add_mask(id)));
            }
        }

        if icon_toggle_id(
            ui,
            "plus",
            true,
            crate::strings::tr("ui.docks.new.layer"),
            Some(super::ids::new_layer()),
        )
        .clicked()
        {
            w.emit(Intent::Document(LayersModel::new_layer(doc)));
        }
        if icon_toggle_id(
            ui,
            "new-group",
            true,
            crate::strings::tr("ui.docks.new.group"),
            Some(super::ids::new_group()),
        )
        .clicked()
        {
            w.emit(Intent::Document(LayersModel::new_group()));
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let can_delete = !selection.is_empty();
            let delete = icon_toggle_id(
                ui,
                "trash",
                false,
                crate::strings::tr("ui.docks.delete.selected.layers"),
                Some(super::ids::layer_delete()),
            );
            if can_delete && delete.clicked() {
                if let Some(command) = LayersModel::delete_selection(doc, &selection) {
                    w.emit(Intent::Document(command));
                    w.layers.clear_selection();
                }
            }
        });
    });
}

/// The tooltip a filter button shows.
fn filter_tip(class: crate::menu::LayerClass) -> String {
    format!("Show only {} layers", class.label().to_lowercase())
}

/// The icon key a layer class draws with, for the filter row.
fn class_icon(class: crate::menu::LayerClass) -> &'static str {
    match class {
        crate::menu::LayerClass::Raster => "layer-raster",
        crate::menu::LayerClass::Group => "layer-group",
        crate::menu::LayerClass::Adjustment => "layer-adjustment",
        crate::menu::LayerClass::Text => "layer-text",
        crate::menu::LayerClass::Shape => "layer-shape",
        crate::menu::LayerClass::SmartObject => "layer-smart-object",
        crate::menu::LayerClass::Generator => "layer-generator",
    }
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

fn history_body(w: &mut Workspace, ui: &mut Ui, history: &History) {
    let model = HistoryModel::new(history);
    let current = model.current();
    let mut jump = None;

    for step in model.steps() {
        let selected = step.index == current;
        let response = row_layout(ui, |ui| {
            ui.add_space(Space::XSmall.pt());
            let side = current_tokens(ui).metrics.min_hit_target * 0.8;
            let (marker, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
            super::paint_icon(
                ui,
                marker,
                step.kind.icon(),
                if step.undone {
                    TextRole::Disabled
                } else {
                    TextRole::Secondary
                },
            );
            ui.add_space(Space::XSmall.pt());
            ui.label(text(
                ui,
                step.label.clone(),
                if step.undone {
                    TextRole::Disabled
                } else {
                    TextRole::Primary
                },
                TypeRole::Body,
            ));
        })
        .response;
        // The row carries the id `view::ids` publishes for it, so a headless
        // test clicks the row a user would click rather than a number.
        let response = ui.interact(
            response.rect,
            super::ids::history_row(step.index),
            Sense::click(),
        );

        if ui.is_rect_visible(response.rect) && (selected || response.hovered()) {
            let t = current_tokens(ui);
            let radius = Radius::Medium.resolve(&t.radii, response.rect.height());
            let fill = if selected {
                ColorRole::SelectionFill
            } else {
                ColorRole::ControlFillHovered
            };
            ui.painter().rect_filled(
                response.rect,
                rounding(radius),
                color32(t.palette.color(fill)),
            );
        }
        if response.clicked() {
            jump = model.jump_to(step.index);
        }
    }

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    ui.horizontal(|ui| {
        if design::ghost_button(ui, "Snapshot")
            .on_hover_text(crate::strings::tr("ui.docks.mark.this.state.so.you.can"))
            .clicked()
        {
            let index = model.current();
            w.snapshots.push(crate::panels::history::Snapshot {
                name: format!("Snapshot {}", w.snapshots.len() + 1),
                index,
            });
        }
    });

    if !w.snapshots.is_empty() {
        design::section_header(ui, "SNAPSHOTS");
        let snapshots = w.snapshots.clone();
        for (i, snapshot) in snapshots.iter().enumerate() {
            let stale = model.snapshot_is_stale(snapshot);
            // `labelled_button` paints the disabled state and senses nothing
            // when it is off, so a stale row is inert on screen and a test can
            // read that off the response rather than trusting the colour.
            let response =
                super::labelled_button(ui, &snapshot.name, !stale, super::ids::history_snapshot(i));
            let response = if stale {
                response.on_hover_text(crate::strings::tr(
                    "ui.docks.the.steps.this.snapshot.named.have",
                ))
            } else {
                response
            };
            if response.clicked() {
                jump = model.jump_to_snapshot(snapshot);
            }
        }
    }

    if let Some(j) = jump {
        w.emit(Intent::HistoryJump(j));
    }
}

// ---------------------------------------------------------------------------
// Adjustments
// ---------------------------------------------------------------------------

fn adjustments_body(w: &mut Workspace, ui: &mut Ui) {
    ui.label(hint(
        ui,
        crate::strings::tr("ui.docks.add.an.adjustment.layer"),
    ));
    ui.add_space(Space::XSmall.pt());
    let t = current_tokens(ui);
    let cell = t.metrics.toolbar_button;
    let per_row = ((ui.available_width() / (cell + Space::Hair.pt())).floor() as usize).max(1);
    let mut created: Option<AdjustmentId> = None;
    for chunk in AdjustmentsPanel::entries().chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Space::Hair.pt();
            for id in chunk {
                if icon_toggle_id(
                    ui,
                    AdjustmentsPanel::icon(*id),
                    true,
                    id.label(),
                    Some(super::ids::adjustment_tile(*id)),
                )
                .clicked()
                {
                    created = Some(*id);
                }
            }
        });
    }
    if let Some(id) = created {
        w.emit(Intent::Document(AdjustmentsPanel::create(id)));
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

fn properties_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    let subject = PropertiesSubject::resolve(doc, doc.active_layer(), w.property_focus);
    ui.label(text(
        ui,
        subject.title(),
        TextRole::Secondary,
        TypeRole::Footnote,
    ));
    ui.add_space(Space::XSmall.pt());

    match subject {
        PropertiesSubject::Nothing => {
            empty_state(ui, crate::strings::tr("ui.docks.select.a.layer.to.see.its"));
        }
        PropertiesSubject::Layer(id) => layer_properties(w, ui, doc, id),
        PropertiesSubject::Mask(id) => mask_properties(w, ui, doc, id),
        PropertiesSubject::Adjustment { layer, id } => {
            adjustment_properties(w, ui, doc, layer, id);
        }
        PropertiesSubject::Text(id) => {
            layer_properties(w, ui, doc, id);
            ui.add_space(Space::XSmall.pt());
            ui.label(hint(
                ui,
                crate::strings::tr("ui.docks.type.is.edited.in.character.and"),
            ));
        }
        PropertiesSubject::Shape(id) => {
            layer_properties(w, ui, doc, id);
            ui.add_space(Space::XSmall.pt());
            ui.label(hint(
                ui,
                crate::strings::tr("ui.docks.path.editing.lives.in.the.paths"),
            ));
        }
    }

    ui.add_space(Space::Small.pt());
    hairline(ui);
    ui.horizontal(|ui| {
        let mut focus = w.property_focus;
        let mut index = usize::from(focus == PropertyFocus::Mask);
        if design::segmented_control(ui, "raster-property-focus", &mut index, &["Layer", "Mask"]) {
            focus = if index == 0 {
                PropertyFocus::Layer
            } else {
                PropertyFocus::Mask
            };
            w.property_focus = focus;
        }
    });
}

fn layer_properties(w: &mut Workspace, ui: &mut Ui, doc: &Document, id: LayerId) {
    let Some(layer) = doc.layers.get(id) else {
        return;
    };
    let mut renamed: Option<String> = None;
    design::inspector_field(ui, "Name", |ui| {
        renamed = super::text_field(ui, super::ids::layer_name(id), &layer.name).committed;
    });
    if let Some(name) = renamed {
        if name.trim() != layer.name {
            if let Some(command) = LayersModel::rename(id, &name) {
                w.emit(Intent::Document(command));
            }
        }
    }
    design::inspector_field(ui, "Kind", |ui| {
        ui.label(body(ui, crate::menu::LayerClass::of(&layer.kind).label()));
    });
    let mut clipping = layer.is_clipping();
    design::inspector_field(ui, "Clipping", |ui| {
        if ui
            .checkbox(
                &mut clipping,
                hint(ui, crate::strings::tr("ui.docks.clip.to.layer.below")),
            )
            .changed()
        {
            w.emit(Intent::Document(LayersModel::set_clipping(id, clipping)));
        }
    });
    if !layer.effects.is_empty() {
        let mut enabled = layer.effects.enabled;
        design::inspector_field(ui, "Effects", |ui| {
            if ui
                .checkbox(
                    &mut enabled,
                    hint(ui, format!("{} effect(s)", layer.effects.count())),
                )
                .changed()
            {
                if let Some(command) = LayersModel::set_effects_enabled(doc, id, enabled) {
                    w.emit(Intent::Document(command));
                }
            }
        });
    }
}

fn mask_properties(w: &mut Workspace, ui: &mut Ui, doc: &Document, id: LayerId) {
    let Some(mask) = MaskProperties::of(doc, id) else {
        empty_state(ui, crate::strings::tr("ui.docks.this.layer.has.no.mask"));
        return;
    };
    let (mut density, mut feather) = (mask.density() * 100.0, mask.feather_px());
    let (mut inverted, mut enabled, mut linked) = (mask.inverted, mask.enabled, mask.linked);

    if design::slider_row(ui, "Density", &mut density, 0.0..=100.0).changed() {
        if let Some(c) = MaskProperties::set_density(doc, id, density / 100.0) {
            w.emit(Intent::Document(c));
        }
    }
    if design::slider_row(ui, "Feather", &mut feather, 0.0..=250.0).changed() {
        if let Some(c) = MaskProperties::set_feather(doc, id, feather) {
            w.emit(Intent::Document(c));
        }
    }
    design::inspector_field(ui, "Invert", |ui| {
        if ui
            .checkbox(
                &mut inverted,
                hint(ui, crate::strings::tr("ui.docks.invert.coverage")),
            )
            .changed()
        {
            if let Some(c) = MaskProperties::set_inverted(doc, id, inverted) {
                w.emit(Intent::Document(c));
            }
        }
    });
    design::inspector_field(ui, "Enabled", |ui| {
        if ui
            .checkbox(
                &mut enabled,
                hint(ui, crate::strings::tr("ui.docks.apply.this.mask")),
            )
            .changed()
        {
            if let Some(c) = MaskProperties::set_enabled(doc, id, enabled) {
                w.emit(Intent::Document(c));
            }
        }
    });
    design::inspector_field(ui, "Linked", |ui| {
        if ui
            .checkbox(
                &mut linked,
                hint(ui, crate::strings::tr("ui.docks.move.with.the.layer")),
            )
            .changed()
        {
            if let Some(c) = MaskProperties::set_linked(doc, id, linked) {
                w.emit(Intent::Document(c));
            }
        }
    });
}

/// The action the Properties panel offers for an adjustment layer whose
/// parameter set is too large for a dock — curves, channel mixers, selective
/// colour.
///
/// Deliberately **not** `ApplyAdjustment`, which bakes a *new* adjustment into
/// a pixel layer: that is gated on `need_editable_pixels`, and this branch is
/// drawn in exactly the state where the active layer is an adjustment and so
/// has no pixels of its own. Emitting it here would either do nothing (if the
/// application honours the menu contract) or edit the wrong target.
/// `whatever_the_properties_panel_offers_for_an_adjustment_is_enabled_there`
/// pins that this action resolves to `Enabled` in the very context the button
/// appears in.
pub(crate) const OPEN_ADJUSTMENT_EDITOR: crate::menu::MenuAction =
    crate::menu::MenuAction::EditAdjustmentLayer;

fn adjustment_properties(
    w: &mut Workspace,
    ui: &mut Ui,
    doc: &Document,
    layer: LayerId,
    id: Option<AdjustmentId>,
) {
    let Some(id) = id else {
        ui.label(hint(
            ui,
            crate::strings::tr("ui.docks.this.adjustment.has.no.panel.controls"),
        ));
        return;
    };
    ui.label(body(ui, id.label()));
    ui.add_space(Space::XSmall.pt());

    use layer_model::AdjustmentKind as K;
    let Some(layer_model::LayerKind::Adjustment(current)) = doc.layers.get(layer).map(|l| &l.kind)
    else {
        return;
    };
    let mut next = current.kind.clone();
    let mut changed = false;

    match &mut next {
        K::BrightnessContrast {
            brightness,
            contrast,
        } => {
            let (mut b, mut c) = (*brightness * 100.0, *contrast * 100.0);
            changed |= design::slider_row(ui, "Brightness", &mut b, -100.0..=100.0).changed();
            changed |= design::slider_row(ui, "Contrast", &mut c, -100.0..=100.0).changed();
            *brightness = b / 100.0;
            *contrast = c / 100.0;
        }
        K::Levels {
            black,
            white,
            gamma,
        } => {
            changed |= design::slider_row(ui, "Black", black, 0.0..=1.0).changed();
            changed |= design::slider_row(ui, "White", white, 0.0..=1.0).changed();
            changed |= design::slider_row(ui, "Gamma", gamma, 0.1..=10.0).changed();
        }
        K::Exposure { stops } => {
            changed |= design::slider_row(ui, "Exposure", stops, -10.0..=10.0).changed();
        }
        K::Vibrance {
            vibrance,
            saturation,
        } => {
            changed |= design::slider_row(ui, "Vibrance", vibrance, -1.0..=1.0).changed();
            changed |= design::slider_row(ui, "Saturation", saturation, -1.0..=1.0).changed();
        }
        K::HueSaturation {
            hue,
            saturation,
            lightness,
        } => {
            changed |= design::slider_row(ui, "Hue", hue, -180.0..=180.0).changed();
            changed |= design::slider_row(ui, "Saturation", saturation, -1.0..=1.0).changed();
            changed |= design::slider_row(ui, "Lightness", lightness, -1.0..=1.0).changed();
        }
        K::Posterize { levels } => {
            let mut v = *levels as f32;
            changed |= design::slider_row(ui, "Levels", &mut v, 2.0..=256.0).changed();
            *levels = v.round().clamp(2.0, 256.0) as u32;
        }
        K::Threshold { level } => {
            changed |= design::slider_row(ui, "Level", level, 0.0..=1.0).changed();
        }
        K::Invert => {
            ui.label(hint(
                ui,
                crate::strings::tr("ui.docks.invert.has.no.parameters"),
            ));
        }
        _ => {
            // Every remaining adjustment has a parameter set too large for a
            // dock — curves, channel mixers, selective colour. They open in
            // their own dialog rather than being half-editable here.
            if super::labelled_button(
                ui,
                crate::strings::tr("ui.docks.open.editor"),
                true,
                super::ids::adjustment_editor(),
            )
            .on_hover_text(format!("Edit this {} layer", id.label()))
            .clicked()
            {
                w.emit(Intent::Action(OPEN_ADJUSTMENT_EDITOR));
            }
        }
    }

    if changed {
        if let Some(intent) = crate::panels::properties::edit_adjustment(doc, layer, next) {
            w.emit(intent);
        }
    }
}

// ---------------------------------------------------------------------------
// Colour and swatches
// ---------------------------------------------------------------------------

fn color_body(w: &mut Workspace, ui: &mut Ui) {
    // The layout test for P1.19 asserts the numeric fields sit inside this
    // panel; the rect is recorded under a stable id for it.
    ui.ctx().memory_mut(|m| {
        m.data
            .insert_temp(egui::Id::new("raster-color-panel-rect"), ui.max_rect())
    });
    super::toolbar::color_wells(w, ui);
    ui.add_space(Space::XSmall.pt());
    spectrum(w, ui);
    ui.add_space(Space::XSmall.pt());

    let mut index = ColorNotation::ALL
        .iter()
        .position(|n| *n == w.color.notation)
        .unwrap_or(0);
    let labels: Vec<&str> = ColorNotation::ALL.iter().map(|n| n.label()).collect();
    if design::segmented_control(ui, "raster-color-notation", &mut index, &labels) {
        w.color.notation = ColorNotation::ALL[index];
    }
    ui.add_space(Space::XSmall.pt());

    match w.color.notation {
        ColorNotation::Hsb => {
            let mut hsv = w.color.hsv();
            let mut changed = false;
            changed |= design::slider_row(ui, "H", &mut hsv[0], 0.0..=360.0).changed();
            changed |= design::slider_row(ui, "S", &mut hsv[1], 0.0..=1.0).changed();
            changed |= design::slider_row(ui, "B", &mut hsv[2], 0.0..=1.0).changed();
            if changed && w.color.set_hsv(hsv) {
                emit_color(w);
            }
        }
        ColorNotation::Rgb => {
            let rgb = w.color.rgb8();
            let mut values = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32];
            let mut changed = false;
            for (i, label) in ["R", "G", "B"].into_iter().enumerate() {
                changed |= design::slider_row(ui, label, &mut values[i], 0.0..=255.0).changed();
            }
            if changed {
                let next = [
                    values[0].round() as u8,
                    values[1].round() as u8,
                    values[2].round() as u8,
                ];
                if w.color.set_rgb8(next) {
                    emit_color(w);
                }
            }
        }
        ColorNotation::Hex => {
            let current = w.color.hex();
            let mut committed: Option<String> = None;
            // The hint is a correction, so it waits until there is something to
            // correct: showing it against the colour the panel itself put in
            // the field reads as the user's mistake.
            let mut show_hint = false;
            design::inspector_field(ui, "Hex", |ui| {
                let edit = super::text_field(ui, super::ids::color_hex(), &current);
                show_hint = crate::panels::color::hex_hint_is_warranted(edit.editing, &edit.text);
                committed = edit.committed;
            });
            if let Some(text) = committed {
                if w.color.set_hex(&text) {
                    emit_color(w);
                }
            }
            if show_hint {
                ui.label(hint(ui, "Enter a colour like #3366CC"));
            }
        }
        ColorNotation::Lab => {
            let mut lab = w.color.lab();
            let mut changed = false;
            changed |= design::slider_row(ui, "L", &mut lab[0], 0.0..=100.0).changed();
            changed |= design::slider_row(ui, "a", &mut lab[1], -128.0..=127.0).changed();
            changed |= design::slider_row(ui, "b", &mut lab[2], -128.0..=127.0).changed();
            if changed && w.color.set_lab(lab) {
                emit_color(w);
            }
        }
    }

    let mut alpha = w.color.current()[3] * 100.0;
    if design::slider_row(ui, "Alpha", &mut alpha, 0.0..=100.0).changed() {
        let mut rgba = w.color.current();
        rgba[3] = alpha / 100.0;
        if w.color.set_current(rgba) {
            emit_color(w);
        }
    }

    ui.horizontal(|ui| {
        if icon_toggle(
            ui,
            "target",
            w.color.eyedropper_armed,
            crate::strings::tr("ui.docks.sample.a.colour.from.the.canvas"),
        )
        .clicked()
        {
            w.color.eyedropper_armed = !w.color.eyedropper_armed;
            if w.color.eyedropper_armed {
                w.emit(Intent::SelectTool(tools::ToolId::Eyedropper));
            }
        }
        if w.color.is_out_of_gamut() {
            ui.label(hint(ui, crate::strings::tr("ui.docks.out.of.gamut")));
        }
    });
}

fn emit_color(w: &mut Workspace) {
    let intent = match w.color.editing {
        ColorWell::Foreground => Intent::SetForeground(w.color.foreground()),
        ColorWell::Background => Intent::SetBackground(w.color.background()),
    };
    w.emit(intent);
}

/// The saturation/brightness square plus the hue strip beneath it.
fn spectrum(w: &mut Workspace, ui: &mut Ui) {
    let t = current_tokens(ui);
    let side = (ui.available_width()).min(t.metrics.inspector_label_width * 2.0);
    let hsv = w.color.hsv();

    let (square, square_response) =
        ui.allocate_exact_size(Vec2::new(side, side * 0.6), Sense::click_and_drag());
    if ui.is_rect_visible(square) {
        let steps = 24;
        for row in 0..steps {
            for col in 0..steps {
                let s = col as f32 / (steps - 1) as f32;
                let v = 1.0 - row as f32 / (steps - 1) as f32;
                let rgb = color::hsv_to_rgb([hsv[0], s, v]);
                let cell = egui::Rect::from_min_size(
                    square.min
                        + Vec2::new(
                            col as f32 * square.width() / steps as f32,
                            row as f32 * square.height() / steps as f32,
                        ),
                    Vec2::new(
                        square.width() / steps as f32 + 1.0,
                        square.height() / steps as f32 + 1.0,
                    ),
                );
                ui.painter().rect_filled(
                    cell.intersect(square),
                    egui::Rounding::ZERO,
                    super::rgba_to_color32([rgb[0], rgb[1], rgb[2], 1.0]),
                );
            }
        }
        let marker =
            square.min + Vec2::new(hsv[1] * square.width(), (1.0 - hsv[2]) * square.height());
        ui.painter().circle_stroke(
            marker,
            Space::XSmall.pt(),
            egui::Stroke::new(
                t.borders.thick,
                color32(t.palette.color(ColorRole::SelectionStroke)),
            ),
        );
    }
    if square_response.dragged() || square_response.clicked() {
        if let Some(p) = ui.ctx().pointer_interact_pos() {
            let s = ((p.x - square.left()) / square.width().max(1.0)).clamp(0.0, 1.0);
            let v = 1.0 - ((p.y - square.top()) / square.height().max(1.0)).clamp(0.0, 1.0);
            if w.color.set_hsv([hsv[0], s, v]) {
                emit_color(w);
            }
        }
    }

    let (strip, strip_response) = ui.allocate_exact_size(
        Vec2::new(side, t.metrics.control_height),
        Sense::click_and_drag(),
    );
    if ui.is_rect_visible(strip) {
        let steps = 48;
        for i in 0..steps {
            let h = i as f32 / steps as f32 * 360.0;
            let rgb = color::hsv_to_rgb([h, 1.0, 1.0]);
            let cell = egui::Rect::from_min_size(
                strip.min + Vec2::new(i as f32 * strip.width() / steps as f32, 0.0),
                Vec2::new(strip.width() / steps as f32 + 1.0, strip.height()),
            );
            ui.painter().rect_filled(
                cell.intersect(strip),
                egui::Rounding::ZERO,
                super::rgba_to_color32([rgb[0], rgb[1], rgb[2], 1.0]),
            );
        }
        let x = strip.left() + hsv[0] / 360.0 * strip.width();
        ui.painter().vline(
            x,
            strip.y_range(),
            egui::Stroke::new(
                t.borders.thick,
                color32(t.palette.color(ColorRole::SelectionStroke)),
            ),
        );
    }
    if strip_response.dragged() || strip_response.clicked() {
        if let Some(p) = ui.ctx().pointer_interact_pos() {
            let h = ((p.x - strip.left()) / strip.width().max(1.0)).clamp(0.0, 1.0) * 360.0;
            if w.color.set_hsv([h, hsv[1], hsv[2]]) {
                emit_color(w);
            }
        }
    }
}

fn swatches_body(w: &mut Workspace, ui: &mut Ui) {
    let t = current_tokens(ui);
    let side = t.metrics.min_hit_target;
    let per_row = ((ui.available_width() / (side + Space::Hair.pt())).floor() as usize).max(1);
    let entries: Vec<(usize, [f32; 4], String)> = w
        .swatches
        .swatches()
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s.rgba, s.name.clone()))
        .collect();
    let mut picked: Option<[f32; 4]> = None;
    let mut remove: Option<usize> = None;
    for chunk in entries.chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Space::Hair.pt();
            for (i, rgba, name) in chunk {
                let response = swatch(ui, *rgba, side, Sense::click()).on_hover_text(name);
                if response.clicked() {
                    picked = Some(*rgba);
                }
                if response.secondary_clicked() {
                    remove = Some(*i);
                }
            }
        });
    }
    if let Some(rgba) = picked {
        if w.color.set_current(rgba) {
            emit_color(w);
        }
    }
    if let Some(i) = remove {
        w.swatches.remove(i);
    }
    ui.add_space(Space::XSmall.pt());
    if design::secondary_button(ui, crate::strings::tr("ui.docks.add.current.colour")).clicked() {
        let rgba = w.color.current();
        let name = crate::panels::color::format_hex(rgba);
        w.swatches.add(name, rgba);
    }
    ui.label(hint(
        ui,
        crate::strings::tr("ui.docks.right.click.a.swatch.to.remove"),
    ));
}

// ---------------------------------------------------------------------------
// Brushes
// ---------------------------------------------------------------------------

fn brushes_body(w: &mut Workspace, ui: &mut Ui) {
    let tool = w.palette.active();
    w.brushes.sync(&w.options, tool);
    let active = w.brushes.active();
    let presets: Vec<(usize, String, f32)> = w
        .brushes
        .presets()
        .iter()
        .enumerate()
        .map(|(i, p)| (i, p.name.clone(), p.settings.size))
        .collect();

    let mut apply: Option<usize> = None;
    let mut remove: Option<usize> = None;
    for (i, name, size) in &presets {
        let response = row_layout(ui, |ui| {
            ui.add_space(Space::XSmall.pt());
            ui.label(body(ui, name.clone()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(hint(ui, format!("{size:.0} px")));
            });
        })
        .response
        .interact(Sense::click());
        if ui.is_rect_visible(response.rect) && (Some(*i) == active || response.hovered()) {
            let t = current_tokens(ui);
            let radius = Radius::Medium.resolve(&t.radii, response.rect.height());
            let fill = if Some(*i) == active {
                ColorRole::SelectionFill
            } else {
                ColorRole::ControlFillHovered
            };
            ui.painter().rect_filled(
                response.rect,
                rounding(radius),
                color32(t.palette.color(fill)),
            );
        }
        if response.clicked() {
            apply = Some(*i);
        }
        if response.secondary_clicked() {
            remove = Some(*i);
        }
    }

    if let Some(i) = apply {
        let writes = w.brushes.apply(i, &mut w.options, tool);
        for (key, value) in writes {
            w.emit(Intent::SetToolOption { tool, key, value });
        }
    }
    if let Some(i) = remove {
        w.brushes.remove(i);
    }

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    if design::secondary_button(ui, crate::strings::tr("ui.docks.edit.brush")).clicked() {
        w.emit(Intent::OpenBrushEditor);
    }
    if design::secondary_button(ui, crate::strings::tr("ui.docks.save.current.brush")).clicked() {
        let name = format!("Brush {}", w.brushes.len() + 1);
        w.brushes.capture(&name, &w.options, tool);
    }
}

// ---------------------------------------------------------------------------
// Character and Paragraph
// ---------------------------------------------------------------------------

fn character_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    let Some((layer, mut run)) = text_panel::active_text(doc, doc.active_layer()) else {
        empty_state(ui, text_panel::no_text_layer_reason());
        return;
    };
    let mut changed = false;
    let mut family: Option<String> = None;
    design::inspector_field(ui, "Family", |ui| {
        family =
            super::text_field(ui, super::ids::character_family(layer), &run.style.family).committed;
    });
    if let Some(family) = family {
        changed |= text_panel::Character::set_family(&mut run, &family);
    }
    let mut size = run.style.size_px;
    if design::slider_row(
        ui,
        "Size",
        &mut size,
        text_panel::MIN_SIZE_PX..=text_panel::MAX_SIZE_PX.min(400.0),
    )
    .changed()
    {
        changed |= text_panel::Character::set_size(&mut run, size);
    }
    design::inspector_field(ui, "Weight", |ui| {
        let current = text_panel::weight_label(run.style.weight);
        let mut picked = run.style.weight.0;
        egui::ComboBox::from_id_salt("raster-char-weight")
            .selected_text(body(ui, current))
            .show_ui(ui, |ui| {
                for (name, value) in text_panel::WEIGHTS {
                    if ui
                        .selectable_label(run.style.weight.0 == *value, body(ui, *name))
                        .clicked()
                    {
                        picked = *value;
                    }
                }
            });
        if picked != run.style.weight.0 {
            changed |= text_panel::Character::set_weight(&mut run, picked);
        }
    });
    let mut italic = run.style.slant != text_engine::FontSlant::Normal;
    let mut underline = run.style.underline;
    let mut strike = run.style.strikethrough;
    ui.horizontal(|ui| {
        if ui.checkbox(&mut italic, hint(ui, "Italic")).changed() {
            changed |= text_panel::Character::set_italic(&mut run, italic);
        }
        if ui.checkbox(&mut underline, hint(ui, "Underline")).changed() {
            changed |= text_panel::Character::set_underline(&mut run, underline);
        }
        if ui.checkbox(&mut strike, hint(ui, "Strike")).changed() {
            changed |= text_panel::Character::set_strikethrough(&mut run, strike);
        }
    });
    let mut tracking = run.style.tracking;
    if design::slider_row(ui, "Tracking", &mut tracking, -100.0..=400.0).changed() {
        changed |= text_panel::Character::set_tracking(&mut run, tracking);
    }
    design::inspector_field(ui, "Leading", |ui| {
        ui.label(hint(
            ui,
            format!(
                "{:.1} px",
                text_panel::Character::leading_px(&run.style, &run.paragraph)
            ),
        ));
    });

    if changed {
        if let Some(intent) = text_panel::commit(doc, layer, &run) {
            w.emit(intent);
        }
    }
}

fn paragraph_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    let Some((layer, mut run)) = text_panel::active_text(doc, doc.active_layer()) else {
        empty_state(ui, text_panel::no_text_layer_reason());
        return;
    };
    let mut changed = false;

    let mut index = text_panel::ALIGNMENTS
        .iter()
        .position(|a| *a == run.paragraph.alignment)
        .unwrap_or(0);
    let labels: Vec<&str> = text_panel::ALIGNMENTS
        .iter()
        .map(|a| text_panel::alignment_label(*a))
        .collect();
    if design::segmented_control(ui, "raster-paragraph-align", &mut index, &labels) {
        changed |= text_panel::Paragraph::set_alignment(&mut run, text_panel::ALIGNMENTS[index]);
    }

    let mut leading = text_panel::Character::leading_px(&run.style, &run.paragraph);
    if design::slider_row(ui, "Leading", &mut leading, 1.0..=400.0).changed() {
        changed |= text_panel::Paragraph::set_leading_px(&mut run, leading);
    }
    if design::ghost_button(ui, crate::strings::tr("ui.docks.auto.leading")).clicked() {
        changed |= text_panel::Paragraph::set_leading_auto(&mut run, 1.2);
    }

    let mut indent = run.paragraph.first_line_indent;
    if design::slider_row(ui, "Indent", &mut indent, -200.0..=200.0).changed() {
        changed |= text_panel::Paragraph::set_first_line_indent(&mut run, indent);
    }
    let mut before = run.paragraph.space_before;
    if design::slider_row(ui, "Before", &mut before, 0.0..=200.0).changed() {
        changed |= text_panel::Paragraph::set_space_before(&mut run, before);
    }
    let mut after = run.paragraph.space_after;
    if design::slider_row(ui, "After", &mut after, 0.0..=200.0).changed() {
        changed |= text_panel::Paragraph::set_space_after(&mut run, after);
    }

    if changed {
        if let Some(intent) = text_panel::commit(doc, layer, &run) {
            w.emit(intent);
        }
    }
}

// ---------------------------------------------------------------------------
// Navigator and Info
// ---------------------------------------------------------------------------

fn navigator_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    let t = current_tokens(ui);
    let doc_size = (doc.width(), doc.height());
    let aspect = if doc_size.0 == 0 {
        1.0
    } else {
        doc_size.1 as f32 / doc_size.0 as f32
    };
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(width, width * aspect.clamp(0.2, 2.0)),
        Sense::hover(),
    );
    let response = ui.interact(rect, super::ids::navigator_proxy(), Sense::click_and_drag());
    if ui.is_rect_visible(rect) {
        super::checkerboard(ui.painter(), rect, Space::Small.pt());
        let radius = Radius::Small.resolve(&t.radii, rect.height());
        ui.painter().rect_stroke(
            rect,
            rounding(radius),
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::ControlStroke)),
            ),
        );
        let view = ViewBox::from_viewport(w.view_center, w.viewport, w.status.zoom);
        let (x, y, vw, vh) = view.normalised(doc_size);
        let box_rect = egui::Rect::from_min_size(
            rect.min + Vec2::new(x * rect.width(), y * rect.height()),
            Vec2::new(vw * rect.width(), vh * rect.height()),
        );
        ui.painter().rect_stroke(
            box_rect,
            egui::Rounding::ZERO,
            egui::Stroke::new(
                t.borders.thick,
                color32(t.palette.color(ColorRole::SelectionStroke)),
            ),
        );
    }
    if response.dragged() || response.clicked() {
        if let Some(p) = ui.ctx().pointer_interact_pos() {
            let f = (
                (p.x - rect.left()) / rect.width().max(1.0),
                (p.y - rect.top()) / rect.height().max(1.0),
            );
            let center = ViewBox::center_for_click(f, doc_size);
            // The proxy is a camera control, not a picture: moving the box has
            // to move the canvas, which means posting an intent rather than
            // only writing the field this panel reads back.
            if w.view_center != center {
                w.view_center = center;
                w.emit(Intent::SetViewCenter(center));
            }
        }
    }

    ui.add_space(Space::XSmall.pt());
    ui.horizontal(|ui| {
        if icon_toggle(ui, "minus", true, crate::strings::tr("ui.docks.zoom.out")).clicked() {
            w.emit(Intent::SetZoom(crate::panels::navigator::zoom_out(
                w.status.zoom,
            )));
        }
        ui.label(body(ui, format_zoom(w.status.zoom)));
        if icon_toggle(ui, "plus", true, crate::strings::tr("ui.docks.zoom.in")).clicked() {
            w.emit(Intent::SetZoom(crate::panels::navigator::zoom_in(
                w.status.zoom,
            )));
        }
        if super::labelled_button(ui, "Fit", true, super::ids::navigator_fit())
            .on_hover_text(crate::strings::tr("ui.docks.fit.the.whole.image.in.the"))
            .clicked()
        {
            // `w.viewport` is the canvas rectangle the last drawn frame
            // measured, not a constructed guess — see `Workspace::record_viewport`.
            w.emit(Intent::SetZoom(crate::panels::navigator::fit_zoom(
                doc_size, w.viewport,
            )));
        }
    });
}

fn info_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    for readout in w.info.readouts(doc) {
        design::inspector_field(ui, readout.label, |ui| {
            ui.label(body(ui, readout.value.clone()));
        });
    }
}

// ---------------------------------------------------------------------------
// Channels and Paths
// ---------------------------------------------------------------------------

fn channels_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    let rows = w.channels.rows(doc);
    let mode = doc.meta.color_space.clone();
    let mut toggle: Option<(ChannelKind, bool)> = None;
    let mut select: Option<ChannelKind> = None;
    for (index, row) in rows.iter().enumerate() {
        let response = row_layout(ui, |ui| {
            if icon_toggle_id(
                ui,
                "eye",
                row.visible,
                "Show / hide this channel",
                Some(super::ids::channel_eye(index)),
            )
            .clicked()
            {
                toggle = Some((row.kind, !row.visible));
            }
            ui.label(body(ui, row.name.clone()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // The chord comes from the row itself, and `keys::channel_for_
                // key` answers it from the same list — a hint here is a promise
                // that module keeps.
                if let Some(chord) = row.shortcut_label() {
                    ui.label(hint(ui, chord));
                }
            });
        })
        .response
        .interact(Sense::click());
        if response.clicked() {
            select = Some(row.kind);
        }
        if w.channels.selected == row.kind && ui.is_rect_visible(response.rect) {
            let t = current_tokens(ui);
            let radius = Radius::Medium.resolve(&t.radii, response.rect.height());
            ui.painter().rect_stroke(
                response.rect,
                rounding(radius),
                egui::Stroke::new(
                    t.borders.hairline,
                    color32(t.palette.color(ColorRole::SelectionStroke)),
                ),
            );
        }
    }
    if let Some((kind, visible)) = toggle {
        match kind {
            ChannelKind::Composite => {
                w.channels.set_composite_visible(&mode, visible);
                w.emit(Intent::SetChannelVisible {
                    channel: kind,
                    visible,
                });
            }
            ChannelKind::Component(i) => {
                w.channels.set_component_visible(i, visible);
                w.emit(Intent::SetChannelVisible {
                    channel: kind,
                    visible,
                });
            }
            // A mask channel's visibility *is* the mask's `enabled` flag, so
            // this one is a document edit and travels through history.
            ChannelKind::Mask { layer, .. } => {
                if let Some(command) = LayersModel::set_mask_enabled(doc, layer, visible) {
                    w.emit(Intent::Document(command));
                }
            }
        }
    }
    if let Some(kind) = select {
        if w.channels.selected != kind {
            w.channels.selected = kind;
            w.emit(Intent::SelectChannel(kind));
        }
    }
}

fn paths_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    let rows = PathsState::rows(doc);
    if rows.is_empty() {
        empty_state(ui, PathsState::empty_message());
        return;
    }
    let mut select = None;
    let mut toggle: Option<(LayerId, bool)> = None;
    for row in &rows {
        let response = row_layout(ui, |ui| {
            // A path is drawn by its shape layer, so the eye here *is* that
            // layer's visibility rather than a second, parallel switch.
            if icon_toggle(ui, "eye", row.visible, "Show / hide this path").clicked() {
                toggle = Some((row.layer, !row.visible));
            }
            ui.label(body(ui, row.name.clone()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !row.has_geometry {
                    ui.label(hint(ui, "empty"));
                }
            });
        })
        .response
        .interact(Sense::click());
        if response.clicked() {
            select = Some(row.layer);
        }
        if w.paths.selected == Some(row.layer) && ui.is_rect_visible(response.rect) {
            let t = current_tokens(ui);
            let radius = Radius::Medium.resolve(&t.radii, response.rect.height());
            ui.painter().rect_filled(
                response.rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::SelectionFill)),
            );
        }
    }
    if let Some((layer, visible)) = toggle {
        w.emit(Intent::Document(LayersModel::set_visible(layer, visible)));
    }
    if let Some(layer) = select {
        w.paths.selected = Some(layer);
        w.emit(Intent::SelectLayers {
            layers: vec![layer],
            active: Some(layer),
        });
    }
}

/// The Actions panel: record, stop, and replay a command sequence.
///
/// The recording itself lives on the [`crate::Editor`](super) — the shell
/// owns it; the panel only speaks. Three buttons, always enabled: the shell
/// refuses what makes no sense (starting a second recording restarts it;
/// replaying with nothing captured reports it in the status bar) and says so
/// through the same channel every other panel answer uses.
fn actions_body(w: &mut Workspace, ui: &mut Ui) {
    ui.add_space(Space::XSmall.pt());
    ui.horizontal(|ui| {
        if super::labelled_button(ui, "Record", true, egui::Id::new("raster-actions-record"))
            .clicked()
        {
            w.emit(Intent::StartRecording);
        }
        if super::labelled_button(ui, "Stop", true, egui::Id::new("raster-actions-stop")).clicked()
        {
            w.emit(Intent::StopRecording);
        }
        if super::labelled_button(ui, "Replay", true, egui::Id::new("raster-actions-replay"))
            .clicked()
        {
            w.emit(Intent::ReplayRecording);
        }
    });
    ui.add_space(Space::XSmall.pt());
    empty_state(
        ui,
        "Record an edit, then replay the whole sequence on any \
         document with at least as many layers.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::LayerTree;

    fn row_rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(240.0, 30.0))
    }

    fn an_id() -> LayerId {
        let mut tree = LayerTree::new();
        tree.push_root(layer_model::Layer::raster("A")).unwrap()
    }

    #[test]
    fn the_bands_of_a_row_say_above_inside_and_below() {
        let rect = row_rect();
        let id = an_id();
        // A group row has three bands.
        assert_eq!(
            drop_position(true, id, rect, rect.top() + rect.height() * 0.1),
            DropPosition::Above(id)
        );
        assert_eq!(
            drop_position(true, id, rect, rect.center().y),
            DropPosition::Into(id)
        );
        assert_eq!(
            drop_position(true, id, rect, rect.top() + rect.height() * 0.9),
            DropPosition::Below(id)
        );
    }

    #[test]
    fn a_row_that_cannot_hold_children_has_only_two_bands() {
        let rect = row_rect();
        let id = an_id();
        assert_eq!(
            drop_position(false, id, rect, rect.center().y),
            DropPosition::Below(id),
            "a raster row offered an 'inside' that cannot exist"
        );
        assert_eq!(
            drop_position(false, id, rect, rect.top()),
            DropPosition::Above(id)
        );
    }

    #[test]
    fn a_pointer_outside_the_row_is_clamped_to_its_nearest_band() {
        let rect = row_rect();
        let id = an_id();
        assert_eq!(
            drop_position(true, id, rect, rect.top() - 500.0),
            DropPosition::Above(id)
        );
        assert_eq!(
            drop_position(true, id, rect, rect.bottom() + 500.0),
            DropPosition::Below(id)
        );
    }

    #[test]
    fn a_row_of_no_height_does_not_divide_by_zero() {
        let id = an_id();
        let flat = egui::Rect::from_min_size(egui::pos2(0.0, 10.0), egui::vec2(240.0, 0.0));
        // Any answer will do; not panicking is the assertion.
        let _ = drop_position(true, id, flat, 10.0);
    }
}
