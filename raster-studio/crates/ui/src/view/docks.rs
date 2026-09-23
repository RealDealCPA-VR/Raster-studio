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
    self as props, AdjustmentsPanel, MaskProperties, PropertiesSubject, PropertyFocus,
};
use crate::panels::text as text_panel;
use crate::Workspace;

use super::{
    badge, body, empty_state, hairline, hint, icon_action, icon_action_id, icon_toggle,
    icon_toggle_id, list_row_layout, panel_frame, panel_icon_side, row_layout, swatch, text,
    ActionState,
};

/// Draw every rail.
///
/// The wide right column is shown before the narrow one on purpose: egui
/// hands each `SidePanel::right` the space the previous one left, so the
/// column drawn second lands *inside* the first — which is where Photopea's
/// narrow column sits, between the canvas and the wide column.
pub fn docks(w: &mut Workspace, ctx: &egui::Context, doc: &Document, history: &History) {
    for side in [DockSide::Left, DockSide::Right, DockSide::RightNarrow] {
        rail(w, ctx, doc, history, side);
    }
    bottom_rail(w, ctx, doc, history);
}

/// The egui panel id of one column, open or folded.
fn column_panel_id(side: DockSide, folded: bool) -> &'static str {
    match (side, folded) {
        (DockSide::Left, false) => "raster-dock-left",
        (DockSide::Left, true) => "raster-dock-left-rail",
        (DockSide::RightNarrow, false) => "raster-dock-right-narrow",
        (DockSide::RightNarrow, true) => "raster-dock-right-narrow-rail",
        (DockSide::Right, false) => "raster-dock-right",
        (DockSide::Right, true) => "raster-dock-right-rail",
        (DockSide::Bottom, _) => "raster-dock-bottom",
    }
}

fn rail(w: &mut Workspace, ctx: &egui::Context, doc: &Document, history: &History, side: DockSide) {
    if w.dock.side_is_empty(side) {
        return;
    }
    let t = design::current_theme(ctx).tokens();
    if w.dock.side_is_collapsed(side) {
        return icon_rail(w, ctx, side);
    }
    if side == DockSide::Bottom {
        return;
    }
    let id = column_panel_id(side, false);
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
            let whole = ui.max_rect();
            super::mark(ui, whole, crate::dock::ids::column(side));
            column(w, ui, doc, history, side);
        });
    commit_measure(w, ctx, side, response.response.rect.width());
}

/// The last-frame height of the group at stack index `index` on `side`, as
/// the column remembers it between frames.
fn group_height_key(side: DockSide, index: usize) -> egui::Id {
    egui::Id::new("raster-dock-group-height")
        .with(side)
        .with(index)
}

/// One column's stack of tab groups.
///
/// # Fixed groups keep their height; one group takes the rest
///
/// Photopea stacks its panels so that the column has no dead space: every
/// group is as tall as its content except the Layers group, which is given
/// whatever is left and scrolls its rows inside. Before this the whole column
/// was one scroll area of natural-height groups, so a 900pt window ended in
/// ~140pt of bare panel colour under History while the Layers rows scrolled
/// out of sight above it.
///
/// egui lays out top to bottom in one pass, so the height the *following*
/// groups will take is not known when the flexible one is drawn. It is read
/// from the previous frame instead ([`group_height_key`]): the first frame
/// gives the flexible group everything, the second frame corrects it, and a
/// window resize is one frame behind — which is why the geometry tests settle
/// three frames before reading.
fn column(w: &mut Workspace, ui: &mut Ui, doc: &Document, history: &History, side: DockSide) {
    let groups = w.dock.groups_on(side);
    let flex = w.dock.flex_group(side);
    let viewport_h = ui.available_height();
    let spacing = ui.spacing().item_spacing.y;
    let t = current_tokens(ui);
    // A flexible group can never be squeezed below its header and a few
    // rows; past that the column scrolls instead.
    let floor = crate::dock::DockState::header_height() + t.metrics.list_row_height * 3.0;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, (_, members)) in groups.iter().enumerate() {
                // Every other group's last-frame height, or `None` on a frame
                // where one is not known yet: then this group takes its
                // natural height too, so the first frame never overflows the
                // column and grows a scroll bar the second frame removes.
                let others: Option<f32> = (0..groups.len())
                    .filter(|i| *i != index)
                    .map(|i| {
                        ui.ctx()
                            .memory(|m| m.data.get_temp::<f32>(group_height_key(side, i)))
                    })
                    .sum();
                let fill_bottom = match (flex == Some(index), others) {
                    (true, Some(others)) => {
                        let gaps = spacing * groups.len().saturating_sub(1) as f32;
                        let mine = (viewport_h - others - gaps).max(floor);
                        Some(ui.cursor().top() + mine)
                    }
                    _ => None,
                };
                let scope = ui.scope(|ui| {
                    panel_group(w, ui, doc, history, members, fill_bottom);
                });
                let rect = scope.response.rect;
                for panel in members {
                    super::mark(ui, rect, crate::dock::ids::group_of(*panel));
                }
                ui.ctx().memory_mut(|m| {
                    m.data
                        .insert_temp(group_height_key(side, index), rect.height())
                });
            }
        });
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
    let id = column_panel_id(side, true);
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
                    if icon_action_id(
                        ui,
                        "overflow",
                        panel.title(),
                        ActionState::Idle,
                        Some(super::ids::rail_icon(panel)),
                    )
                    .clicked()
                    {
                        w.dock.set_side_collapsed(side, false);
                        w.dock.raise(panel);
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    if icon_action_id(
                        ui,
                        "chevron-right",
                        crate::strings::tr("ui.docks.expand.the.dock"),
                        ActionState::Idle,
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
            let whole = ui.max_rect();
            super::mark(ui, whole, crate::dock::ids::column(DockSide::Bottom));
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (_, members) in w.dock.groups_on(DockSide::Bottom) {
                        panel_group(w, ui, doc, history, &members, None);
                    }
                });
        });
    commit_measure(w, ctx, DockSide::Bottom, response.response.rect.height());
}

/// One tabbed group: a strip of tabs, then the active panel's body.
///
/// `fill_bottom` is the absolute y this group must reach when it is the
/// column's flexible one (see [`column`]): the body is stretched to it, and a
/// body that lists rows — Layers — scrolls them inside the space instead of
/// growing past it.
fn panel_group(
    w: &mut Workspace,
    ui: &mut Ui,
    doc: &Document,
    history: &History,
    members: &[PanelId],
    fill_bottom: Option<f32>,
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
    let t = current_tokens(ui);
    // The rule under the group and the gap before it come off the fill, so
    // the group's *outer* edge — rule included — lands on `fill_bottom`.
    let frame_bottom = fill_bottom.map(|b| b - t.borders.hairline - ui.spacing().item_spacing.y);
    let body_bottom = frame_bottom.map(|b| b - t.metrics.panel_padding);
    panel_frame(ui).show(ui, |ui| {
        ui.push_id(active.key(), |ui| {
            body_of(w, ui, doc, history, active, body_bottom);
        });
        if let Some(bottom) = body_bottom {
            // Stretch the body to the fill even when its content is shorter:
            // the panel colour reaches the column's edge with nothing under it.
            let min = ui.min_rect();
            if bottom > min.bottom() {
                ui.expand_to_include_rect(egui::Rect::from_min_max(
                    min.min,
                    egui::pos2(min.right(), bottom),
                ));
            }
        }
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
                if icon_action(
                    ui,
                    "close",
                    crate::strings::tr("ui.docks.close.panel"),
                    ActionState::Idle,
                )
                .clicked()
                {
                    w.emit(Intent::SetPanelOpen {
                        panel: active,
                        open: false,
                    });
                }
                let open = w.panel_menu == Some(active);
                if icon_action_id(
                    ui,
                    "overflow",
                    crate::strings::tr("ui.docks.move.this.panel"),
                    ActionState::selected_if(open),
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

fn side_label(side: DockSide) -> &'static str {
    crate::strings::tr(match side {
        DockSide::Left => "ui.docks.side.left",
        DockSide::RightNarrow => "ui.docks.side.narrow",
        DockSide::Right => "ui.docks.side.right",
        DockSide::Bottom => "ui.docks.side.bottom",
    })
}

/// Dispatch to one panel's body. `fill_bottom` is the absolute y the body may
/// stretch to when its group is the column's flexible one, or `None` to take
/// its natural height.
fn body_of(
    w: &mut Workspace,
    ui: &mut Ui,
    doc: &Document,
    history: &History,
    panel: PanelId,
    fill_bottom: Option<f32>,
) {
    match panel {
        PanelId::Layers => layers_body(w, ui, doc, fill_bottom),
        PanelId::History => history_body(w, ui, history),
        PanelId::Adjustments => adjustments_body(w, ui),
        PanelId::Properties => properties_body(w, ui, doc, history),
        PanelId::Color => color_body(w, ui),
        PanelId::Swatches => swatches_body(w, ui),
        PanelId::Brushes => brushes_body(w, ui, fill_bottom),
        PanelId::Character => character_body(w, ui, doc),
        PanelId::Paragraph => paragraph_body(w, ui, doc),
        PanelId::Navigator => navigator_body(w, ui, doc),
        PanelId::Info => info_body(w, ui, doc),
        PanelId::Histogram => histogram_body(w, ui, doc),
        PanelId::Channels => channels_body(w, ui, doc, history),
        PanelId::Paths => paths_body(w, ui, doc),
        PanelId::Actions => actions_body(w, ui),
    }
}

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

fn layers_body(w: &mut Workspace, ui: &mut Ui, doc: &Document, fill_bottom: Option<f32>) {
    let model = LayersModel::build(doc, &w.layers);
    let active = doc.active_layer();

    // Photopea's order: the kind filter on top, then the compact blend /
    // opacity / lock / fill block, then the rows, then the footer.
    layer_filter_row(w, ui);
    blend_and_opacity(w, ui, doc, active);
    ui.add_space(Space::XSmall.pt());
    hairline(ui);

    // When the group is the column's flexible one, the rows get exactly the
    // height between the blend block and the footer and scroll inside it —
    // the footer stays pinned to the column's edge, Photopea's way.
    let t = current_tokens(ui);
    let footer_reserve = panel_icon_side(t).max(t.metrics.control_height)
        + Space::XSmall.pt()
        + t.borders.hairline
        + ui.spacing().item_spacing.y * 3.0;
    let rows_height = fill_bottom.map(|b| (b - ui.cursor().top() - footer_reserve).max(0.0));

    let rows_ui = |w: &mut Workspace, ui: &mut Ui| {
        if model.is_empty() {
            empty_state(ui, crate::strings::tr("ui.docks.no.layers.yet"));
            return;
        }
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
    };

    match rows_height {
        Some(height) => {
            egui::ScrollArea::vertical()
                .id_salt("raster-layer-rows")
                .auto_shrink([false, false])
                .max_height(height)
                .min_scrolled_height(height)
                .show(ui, |ui| rows_ui(w, ui));
        }
        None => rows_ui(w, ui),
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

    let locks = layer.map(|l| l.locked).unwrap_or_default();

    // Two rows, Photopea's way: `[Blend v | Opacity --- %]` over
    // `[Lock: icons | Fill --- %]`. The left column is one shared width so the
    // two sliders start on the same vertical line.
    let t = current_tokens(ui);
    let left = t.metrics.inspector_label_width + t.metrics.numeric_field_width;
    let height = panel_icon_side(t).max(t.metrics.control_height);

    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(left, height),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(hint(ui, "Blend"));
                    let mut picked = mode;
                    let combo = egui::ComboBox::from_id_salt("raster-layer-blend")
                        .width(ui.available_width())
                        .selected_text(body(ui, mode.label()))
                        .show_ui(ui, |ui| {
                            for candidate in BlendMode::ALL {
                                let row = ui.selectable_label(
                                    candidate == mode,
                                    body(ui, candidate.label()),
                                );
                                super::mark(
                                    ui,
                                    row.rect,
                                    super::ids::layer_blend_option(candidate),
                                );
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
                },
            );
            let opacity_row =
                percent_slider(ui, "Opacity", &mut opacity, super::ids::layer_opacity());
            if opacity_row.changed() {
                if let Some(id) = active {
                    if let Some(c) = LayersModel::set_opacity(id, opacity / 100.0) {
                        w.emit(Intent::Document(c));
                    }
                }
            }
        });

        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(left, height),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(hint(ui, "Lock"));
                    ui.spacing_mut().item_spacing.x = Space::Hair.pt();
                    let mut next = locks;
                    for toggle in super::LockToggle::ALL {
                        let (key, tip) = toggle.icon_and_tooltip();
                        let on = toggle.get(locks);
                        // A lock is a real on/off, drawn as the selected accent
                        // when engaged; without a layer there is nothing to
                        // lock, and only then does the row read as disabled.
                        let state = if enabled {
                            ActionState::selected_if(on)
                        } else {
                            ActionState::Disabled
                        };
                        if icon_action_id(ui, key, tip, state, Some(super::ids::layer_lock(toggle)))
                            .clicked()
                        {
                            toggle.set(&mut next, !on);
                        }
                    }
                    if next != locks {
                        if let Some(id) = active {
                            w.emit(Intent::Document(LayersModel::set_locks(id, next)));
                        }
                    }
                },
            );
            let fill_row = percent_slider(ui, "Fill", &mut fill, super::ids::layer_fill());
            if fill_row.changed() {
                if let Some(id) = active {
                    if let Some(c) = LayersModel::set_fill_opacity(id, fill / 100.0) {
                        w.emit(Intent::Document(c));
                    }
                }
            }
        });
    });
}

/// `label  [slider] [ 100 %]`, filling the rest of the row.
///
/// The inspector's `design::slider_row` reserves a full label column, which is
/// the right shape for the Properties panel and the wrong one for a row that
/// already spent its left half on the blend combo or the lock icons. The
/// returned response is the union of the slider and the field, and it is
/// marked under `id` so a headless test can drag the control by name.
fn percent_slider(ui: &mut Ui, label: &str, value: &mut f32, id: egui::Id) -> egui::Response {
    let t = current_tokens(ui);
    let height = t.metrics.control_height;
    let field_width = t.metrics.numeric_field_width;
    ui.label(hint(ui, label));
    let remaining =
        (ui.available_width() - field_width - Space::Small.pt()).max(t.metrics.min_hit_target);
    let slider = ui.add_sized(
        Vec2::new(remaining, height),
        egui::Slider::new(value, 0.0..=100.0).show_value(false),
    );
    let field = ui.add_sized(
        Vec2::new(field_width, height),
        egui::DragValue::new(value)
            .range(0.0..=100.0)
            .max_decimals(0)
            .suffix("%"),
    );
    let response = slider | field;
    super::mark(ui, response.rect, id);
    response
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
    // The row paints its own thumbnail and name, so the accessibility tree
    // needs the layer's name stated explicitly — this is what a screen
    // reader says when the rows list is read (C14).
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, row.name.clone())
    });

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
        crate::strings::tr("ui.docks.show.hide.layer"),
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
    // W3-J: the name is its own control. A double-click on it opens the
    // inline rename in its place; a single click selects like the row does
    // (the label takes the click from the row underneath, so it has to be
    // forwarded). Elsewhere on a text row a double-click still enters the
    // layer (card 026).
    let mut name_clicked = false;
    if w.layers.renaming() == Some(row.id) {
        rename_field(w, &mut content, row);
    } else {
        let label = content.label(body(&content, row.name.clone()));
        let name = content.interact(
            label.rect,
            crate::panels::layers::ids::name_label(row.id),
            Sense::click(),
        );
        if name.double_clicked() {
            w.layers.begin_rename(row.id, &row.name);
        } else if name.clicked() {
            name_clicked = true;
        }
        name.on_hover_text(crate::strings::tr("ui.docks.layers.rename.tip"));
    }

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

    // Card 026: double-clicking a TEXT row enters that layer for editing —
    // the shell opens the session; other classes keep plain selection.
    if response.double_clicked() && row.class == crate::menu::LayerClass::Text {
        w.emit(Intent::EnterTextLayer { layer: row.id });
    }
    if response.clicked() || name_clicked {
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

/// W3-J: the inline rename, drawn where the name label was.
///
/// The field takes focus and selects its text on the frame it opens. Enter
/// (which makes a single-line edit surrender focus) or a click elsewhere
/// commits: one [`LayersModel::rename`] command, so one undo step. Escape
/// cancels and the name is untouched. An unchanged or all-space name emits
/// nothing — the same rule the Properties Name field follows.
fn rename_field(w: &mut Workspace, content: &mut Ui, row: &LayerRow) {
    let t = current_tokens(content);
    let id = crate::panels::layers::ids::rename_field(row.id);
    let width = (content.available_width() - Space::Medium.pt()).max(t.metrics.numeric_field_width);
    let fresh = w.layers.take_rename_fresh();
    let seed = w.layers.rename_buffer_mut().clone();
    let edit = super::text_field_sized(content, id, &seed, width);
    if fresh {
        edit.response.request_focus();
        // Select the whole name, so typing replaces it (Photopea's rename).
        let mut state = egui::text_edit::TextEditState::load(content.ctx(), id).unwrap_or_default();
        let end = seed.chars().count();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(end),
            )));
        state.store(content.ctx(), id);
        return;
    }
    if content.input(|i| i.key_pressed(egui::Key::Escape)) {
        w.layers.cancel_rename();
        return;
    }
    if let Some(name) = edit.committed {
        w.layers.finish_rename();
        if name.trim() != row.name {
            if let Some(command) = LayersModel::rename(row.id, &name) {
                w.emit(Intent::Document(command));
            }
        }
    }
}

/// The 4:3 thumbnail well.
///
/// It shows the layer's real pixels when the application has uploaded a fitted
/// thumbnail ([`Workspace::layer_thumbs`]); otherwise it falls back to the
/// layer's *kind* glyph over the checkerboard. A well that reads "group" or
/// "adjustment" is honest; a blank well is not — and the glyph fallback is
/// also exactly what a headless draw sees, since no application has uploaded
/// textures there.
fn thumbnail(ui: &mut Ui, w: &mut Workspace, row: &LayerRow) {
    let t = current_tokens(ui);
    let height = (t.metrics.list_row_height * w.layers.thumb_scale.height()) - Space::XSmall.pt();
    let size = Vec2::new(height * 4.0 / 3.0, height);
    content_well(ui, w, row, size);
    if row.has_mask {
        // Card 055: a layer with a mask carries a second well — clicking it
        // aims edits at the mask coverage, the same state the Properties
        // Layer/Mask control mirrors.
        mask_well(ui, w, row, Vec2::new(size.x * 0.6, size.y));
    }
}

/// The content thumbnail well (card 055): shows the layer's pixels and is
/// the click target that aims edits at CONTENT. A stroke border marks the
/// well the current edit target points at.
fn content_well(ui: &mut Ui, w: &mut Workspace, row: &LayerRow, size: Vec2) {
    let t = current_tokens(ui);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    // Clicking the well (not dragging the row) aims at content.
    let response = ui.interact(
        rect,
        super::ids::layer_content_thumb(row.id),
        Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            crate::strings::tr("ui.docks.content.thumbnail"),
        )
    });
    if response.clicked() {
        // Card 055: the click aims at THIS row — selecting it first, exactly
        // as the row's own click does. Without the selection the target
        // would resolve against the previously active layer while the
        // clicked row's well showed the border.
        w.layers.select_only(row.id);
        let selection = w.layers.selection().to_vec();
        w.emit(Intent::SelectLayers {
            layers: selection,
            active: Some(row.id),
        });
        w.property_focus = crate::panels::properties::PropertyFocus::Layer;
        w.emit(crate::Intent::SetEditTarget { mask: false });
    }
    let radius = Radius::Small.resolve(&t.radii, size.y);
    super::checkerboard(ui.painter(), rect, Space::XSmall.pt());
    // Card 055: the target border — THE target row's well (the active
    // layer, which is what the edit target resolves against) of the focused
    // kind, and nothing else.
    let border =
        if w.property_focus == crate::panels::properties::PropertyFocus::Layer && row.active {
            egui::Stroke::new(
                t.borders.thick,
                color32(t.palette.color(ColorRole::SelectionStroke)),
            )
        } else {
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::ControlStroke)),
            )
        };
    ui.painter().rect_stroke(rect, rounding(radius), border);
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

/// The mask thumbnail well (card 055): shown only on layers that HAVE a
/// mask; clicking it aims edits at the mask coverage. Card 059: it draws the
/// mask's REAL coverage thumbnail when the application supplied one, carries
/// an obvious active-target badge, and a secondary-click popup with the mask
/// view modes (Composite/Grayscale/Overlay) plus the mask ops.
fn mask_well(ui: &mut Ui, w: &mut Workspace, row: &LayerRow, size: Vec2) {
    let t = current_tokens(ui);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    // Card 059: the popup's lifecycle runs BEFORE the visibility cull — a
    // popup whose well is scrolled out of view must still close on Escape
    // or an outside click, or it resurrects on the way back.
    if w.layers.mask_menu == Some(row.id) {
        if !row.has_mask {
            // The mask went away (deleted, applied, undone): the popup has
            // nothing to present and must not re-anchor later.
            w.layers.mask_menu = None;
            w.layers.mask_menu_fresh = false;
        } else {
            mask_view_popup(w, ui, row, rect, w.layers.mask_menu_fresh);
            // The fresh flag guards exactly one frame (the opening one).
            w.layers.mask_menu_fresh = false;
        }
    }
    if !ui.is_rect_visible(rect) {
        return;
    }
    let response = ui.interact(rect, super::ids::layer_mask_thumb(row.id), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            crate::strings::tr("ui.docks.mask.thumbnail"),
        )
    });
    if response.clicked() {
        // Card 055: the click aims at THIS row's mask — selecting the row
        // first, so the target cannot resolve against another layer.
        w.layers.select_only(row.id);
        let selection = w.layers.selection().to_vec();
        w.emit(Intent::SelectLayers {
            layers: selection,
            active: Some(row.id),
        });
        w.property_focus = crate::panels::properties::PropertyFocus::Mask;
        w.emit(crate::Intent::SetEditTarget { mask: true });
    }
    if response.secondary_clicked() {
        // Card 059: the well's own popup — view modes + the mask ops. The
        // fresh flag keeps the opening right-click's own release from
        // closing it the same frame (the shared context menu's trick).
        //
        // The right-click also SELECTS the row, exactly as the left click
        // does: the mask ops ride MenuOp::Toggle, which acts on the ACTIVE
        // layer, and a popup over a selected-but-not-active row would
        // otherwise silently hit another layer's mask.
        w.layers.select_only(row.id);
        let selection = w.layers.selection().to_vec();
        w.emit(Intent::SelectLayers {
            layers: selection,
            active: Some(row.id),
        });
        w.layers.mask_menu = Some(row.id);
        w.layers.mask_menu_fresh = true;
    }
    let radius = Radius::Small.resolve(&t.radii, size.y);
    super::checkerboard(ui.painter(), rect, Space::XSmall.pt());
    let border = if w.property_focus == crate::panels::properties::PropertyFocus::Mask && row.active
    {
        egui::Stroke::new(
            t.borders.thick,
            color32(t.palette.color(ColorRole::SelectionStroke)),
        )
    } else {
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::ControlStroke)),
        )
    };
    ui.painter().rect_stroke(rect, rounding(radius), border);
    if let Some(tex) = w.mask_thumbs.get(&row.id) {
        // Card 059: the real coverage thumbnail — grayscale bytes straight
        // from the mask's pose-sampled store.
        ui.painter().image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            crate::dialogs::controls::UNTINTED,
        );
    } else {
        let side = rect.height() * 0.7;
        let icon_rect = egui::Rect::from_center_size(rect.center(), Vec2::splat(side));
        if row.mask_enabled {
            super::paint_icon(ui, icon_rect, "mask", TextRole::Secondary);
        } else {
            super::paint_icon(ui, icon_rect, "mask", TextRole::Tertiary);
        }
    }
    // Card 059: the active-target badge — a filled accent dot on the corner
    // of THE well the edit target is aimed at, so the indicator survives a
    // thumbnail that fills the whole well (the border alone reads as
    // selection, not as "edits land here").
    if w.property_focus == crate::panels::properties::PropertyFocus::Mask && row.active {
        let r = 2.5_f32.min(rect.width() * 0.15);
        let centre = egui::pos2(rect.right() - r - 2.0, rect.top() + r + 2.0);
        // A hover-sensed interact keeps the badge discoverable (tooltip) and
        // gives tests a stable id to assert the indicator by.
        let badge = ui.interact(
            egui::Rect::from_center_size(centre, Vec2::splat(r * 2.0)),
            super::ids::mask_target_badge(row.id),
            Sense::hover(),
        );
        badge.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                crate::strings::tr("ui.docks.mask.target.badge"),
            )
        });
        badge.on_hover_text(crate::strings::tr("ui.docks.mask.target.badge"));
        ui.painter()
            .circle_filled(centre, r, color32(t.palette.color(ColorRole::Accent)));
    }
}

/// Card 059: the mask well's popup — the three view modes (a direct write to
/// the panel-owned [`Workspace::mask_view`], exactly like the Channels
/// panel's toggles) above the enable/disable and link ops, which resolve
/// through the same menu route the Layer ▸ Layer Mask items use.
fn mask_view_popup(
    w: &mut Workspace,
    ui: &mut Ui,
    row: &LayerRow,
    anchor: egui::Rect,
    fresh: bool,
) {
    let t = current_tokens(ui);
    let popup_id = super::ids::layer_mask_thumb(row.id).with("menu");
    let mut close = false;
    // Clamp the anchor so a well near the panel's bottom still shows the
    // whole popup: five rows (three modes + two ops) at the control height.
    let estimated_h = t.metrics.control_height * 5.0 + 8.0;
    let screen_bottom = ui.ctx().screen_rect().bottom();
    let top = (anchor.bottom() + 2.0).min(screen_bottom - estimated_h);
    egui::Area::new(popup_id)
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(anchor.left(), top.max(0.0)))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(120.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                // The rows are drawn the way the shared context menu draws
                // its items — allocate + interact under a STABLE id — so a
                // test can click a row by name the same way it clicks any
                // other drawn control.
                let row_h = t.metrics.control_height;
                let font =
                    design::egui_theme::text_style(design::TypeRole::Body).resolve(ui.style());
                let menu_row = |ui: &mut egui::Ui,
                                id: egui::Id,
                                label: &str,
                                checked: bool,
                                enabled: bool|
                 -> bool {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::hover(),
                    );
                    let response = ui.interact(rect, id, egui::Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            enabled,
                            label.to_string(),
                        )
                    });
                    if enabled && response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            egui::Rounding::ZERO,
                            color32(t.palette.color(ColorRole::ControlFillHovered)),
                        );
                    }
                    // The checked row reads in the accent (a drawing would
                    // need an inline glyph; the accent does the job the typed
                    // check mark must not).
                    let (text, color) = if checked && enabled {
                        (
                            label.to_string(),
                            color32(t.palette.color(ColorRole::Accent)),
                        )
                    } else {
                        (
                            label.to_string(),
                            color32(t.palette.text(if enabled {
                                TextRole::Secondary
                            } else {
                                TextRole::Tertiary
                            })),
                        )
                    };
                    ui.painter().text(
                        egui::pos2(
                            rect.left() + design::tokens::spacing::Space::Small.pt(),
                            rect.center().y - font.size * 0.5,
                        ),
                        egui::Align2::LEFT_TOP,
                        text,
                        font.clone(),
                        color,
                    );
                    enabled && response.clicked()
                };
                // ---- view modes ---------------------------------
                for mode in crate::MaskViewMode::ALL {
                    let key = match mode {
                        crate::MaskViewMode::Composite => "ui.docks.mask.view.composite",
                        crate::MaskViewMode::Grayscale => "ui.docks.mask.view.grayscale",
                        crate::MaskViewMode::Overlay => "ui.docks.mask.view.overlay",
                    };
                    if menu_row(
                        ui,
                        super::ids::mask_view_item(*mode),
                        crate::strings::tr(key),
                        *mode == w.mask_view,
                        true,
                    ) {
                        w.mask_view = *mode;
                        close = true;
                    }
                }
                // ---- mask ops (the same gates the Layer menu applies) --
                let toggle_enabled = row.active || w.layers.is_selected(row.id);
                if menu_row(
                    ui,
                    super::ids::mask_toggle_item(row.id),
                    if row.mask_enabled {
                        crate::strings::tr("ui.docks.mask.disable")
                    } else {
                        crate::strings::tr("ui.docks.mask.enable")
                    },
                    false,
                    toggle_enabled,
                ) {
                    w.emit(crate::Intent::Action(crate::menu::MenuAction::Mask(
                        crate::menu::MaskOp::Toggle,
                    )));
                    close = true;
                }
                if menu_row(
                    ui,
                    super::ids::mask_link_item(row.id),
                    crate::strings::tr("ui.docks.mask.toggle.link"),
                    false,
                    toggle_enabled,
                ) {
                    w.emit(crate::Intent::Action(crate::menu::MenuAction::Mask(
                        crate::menu::MaskOp::ToggleLink,
                    )));
                    close = true;
                }
            });
        });
    // Close on Escape, or on any click that did not land inside the popup
    // (the popup's own controls set `close` themselves) — but never on the
    // release that opened it.
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        close = true;
    } else if !fresh && ui.input(|i| i.pointer.any_click()) {
        let popup_rect = ui.ctx().read_response(popup_id).map(|r| r.rect);
        let pointer = ui.input(|i| i.pointer.interact_pos()).unwrap_or_default();
        let clicked_inside = popup_rect.is_some_and(|r| r.contains(pointer));
        if !clicked_inside {
            close = true;
        }
    }
    if close {
        w.layers.mask_menu = None;
    }
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

/// The kind filter and the thumbnail size: the row Photopea puts at the *top*
/// of the Layers panel, above the blend block.
///
/// The one filter that is on is the selected action; every other button is a
/// plain action, ready to be pressed. None of them is ever disabled.
fn layer_filter_row(w: &mut Workspace, ui: &mut Ui) {
    let search_drawn = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Space::Hair.pt();
            if icon_action_id(
                ui,
                "overflow",
                crate::strings::tr("ui.docks.show.every.layer"),
                ActionState::selected_if(w.layers.filter.is_none()),
                Some(super::ids::layer_filter_all()),
            )
            .clicked()
            {
                w.layers.filter = None;
            }
            for class in crate::menu::LayerClass::ALL {
                let on = w.layers.filter == Some(class);
                if icon_action_id(
                    ui,
                    class_icon(class),
                    &filter_tip(class),
                    ActionState::selected_if(on),
                    Some(super::ids::layer_filter(class)),
                )
                .clicked()
                {
                    w.layers.filter = if on { None } else { Some(class) };
                }
            }
            let mut search_drawn = false;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_action_id(
                    ui,
                    "plus",
                    crate::strings::tr("ui.docks.thumbnail.size"),
                    ActionState::Idle,
                    Some(super::ids::layer_thumb_size()),
                )
                .clicked()
                {
                    w.layers.thumb_scale = w.layers.thumb_scale.cycled();
                }
                // W3-J: the name search fills what the icons left of the row.
                // When a narrow column leaves less than a numeric field's width,
                // it drops to its own line below instead of overflowing.
                let t = current_tokens(ui);
                let width = ui.available_width() - Space::XSmall.pt();
                if width >= t.metrics.numeric_field_width {
                    layer_search_field(w, ui, width);
                    search_drawn = true;
                }
            });
            search_drawn
        })
        .inner;
    if !search_drawn {
        let t = current_tokens(ui);
        let width = ui.available_width() - Space::XSmall.pt();
        layer_search_field(w, ui, width.max(t.metrics.numeric_field_width));
    }
}

/// W3-J: the Layers panel's name search. Typing narrows the rows to those
/// whose name contains the text (`panels::layers::matches_search`); clearing
/// it shows every row again. Panel state, never document state.
fn layer_search_field(w: &mut Workspace, ui: &mut Ui, width: f32) {
    let current = w.layers.search.clone();
    let edit = super::text_field_sized(
        ui,
        crate::panels::layers::ids::search_field(),
        &current,
        width,
    );
    // Live: the rows narrow with every keystroke, not only on Enter.
    let next = edit.committed.unwrap_or(edit.text);
    if next != current {
        w.layers.search = next;
    }
    edit.response
        .on_hover_text(crate::strings::tr("ui.docks.layers.search"));
}

/// Photopea's footer row, in Photopea's order: link, fx, mask, adjustment,
/// group, new layer — and delete on its own at the right-hand end.
///
/// Every button here is an *action*, not a toggle: it was `icon_toggle` with
/// `on = false` before, which painted the whole row in the disabled colour
/// while every button worked. A button is drawn disabled only when it really
/// cannot act — fx and mask without a layer, delete without a selection — and
/// then it senses nothing as well.
fn layer_buttons(w: &mut Workspace, ui: &mut Ui, doc: &Document, active: Option<LayerId>) {
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
        // on a mixed selection links them all. The button shows the accent
        // while the selection is chained.
        let link_on = selection
            .iter()
            .any(|id| doc.layers.get(*id).is_some_and(|l| l.linked));
        let link_state = if has_layer {
            ActionState::selected_if(link_on)
        } else {
            ActionState::Disabled
        };
        if icon_action_id(
            ui,
            "link",
            crate::strings::tr("ui.docks.link.selected.layers"),
            link_state,
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
        let mask = icon_action_id(
            ui,
            "mask",
            crate::strings::tr("ui.docks.add.a.layer.mask"),
            ActionState::enabled_if(has_layer && !has_mask),
            Some(super::ids::layer_mask()),
        );
        if has_layer && !has_mask && mask.clicked() {
            if let Some(id) = active {
                w.emit(Intent::Document(LayersModel::add_mask(id)));
            }
        }

        // Adjustment: the grid lives in its own panel.
        if icon_action_id(
            ui,
            "adjustment",
            crate::strings::tr("ui.docks.open.the.adjustments.panel"),
            ActionState::Idle,
            Some(super::ids::layer_adjustment()),
        )
        .clicked()
        {
            w.emit(Intent::SetPanelOpen {
                panel: PanelId::Adjustments,
                open: true,
            });
        }

        if icon_action_id(
            ui,
            "new-group",
            crate::strings::tr("ui.docks.new.group"),
            ActionState::Idle,
            Some(super::ids::new_group()),
        )
        .clicked()
        {
            w.emit(Intent::Document(LayersModel::new_group()));
        }
        if icon_action_id(
            ui,
            "plus",
            crate::strings::tr("ui.docks.new.layer"),
            ActionState::Idle,
            Some(super::ids::new_layer()),
        )
        .clicked()
        {
            w.emit(Intent::Document(LayersModel::new_layer(doc)));
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let can_delete = !selection.is_empty();
            let delete = icon_action_id(
                ui,
                "trash",
                crate::strings::tr("ui.docks.delete.selected.layers"),
                ActionState::enabled_if(can_delete),
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
        // A full-width row under the id `view::ids` publishes for it: the
        // whole line is the click target and the whole line takes the
        // selection fill, painted *under* the label rather than over it.
        let response = list_row_layout(ui, super::ids::history_row(step.index), selected, |ui| {
            ui.add_space(Space::XSmall.pt());
            let side = current_tokens(ui).metrics.min_hit_target;
            let (marker, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
            super::paint_panel_icon(
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
        if response.clicked() {
            jump = model.jump_to(step.index);
        }
    }

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    // The footer: icon actions under the list, Photopea's way, so a snapshot
    // control no longer reads as one more history row. New snapshot first;
    // "new document from this state" and "delete snapshot" join it later.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Space::Hair.pt();
        if icon_action_id(
            ui,
            "plus",
            crate::strings::tr("ui.docks.mark.this.state.so.you.can"),
            ActionState::Idle,
            Some(super::ids::history_new_snapshot()),
        )
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
                if icon_action_id(
                    ui,
                    AdjustmentsPanel::icon(*id),
                    id.label(),
                    ActionState::Idle,
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

fn properties_body(w: &mut Workspace, ui: &mut Ui, doc: &Document, history: &History) {
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
        PropertiesSubject::Layer(id) => {
            layer_properties(w, ui, doc, id);
            transform_block(w, ui, doc, id);
        }
        PropertiesSubject::Mask(id) => mask_properties(w, ui, doc, id),
        PropertiesSubject::Adjustment { layer, id } => {
            adjustment_properties(w, ui, doc, layer, id);
        }
        // W3-J: each kind gets its own page under the common block and the
        // transform, in place of the hint that used to send the user away.
        PropertiesSubject::Text(id) => {
            layer_properties(w, ui, doc, id);
            transform_block(w, ui, doc, id);
            text_properties(w, ui, doc, id);
        }
        PropertiesSubject::Shape(id) => {
            layer_properties(w, ui, doc, id);
            transform_block(w, ui, doc, id);
            shape_properties(w, ui, doc, id);
        }
        PropertiesSubject::SmartObject(id) => {
            layer_properties(w, ui, doc, id);
            transform_block(w, ui, doc, id);
            smart_object_properties(w, ui, doc, history, id);
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
            // Card 007: the shell owns the validated edit target; this control
            // shows the choice and reports it. Not a workspace intent — the
            // target lives on the editor, so it travels as its own intent and
            // is applied by the shell (`shell.rs::apply_chrome`).
            w.emit(crate::Intent::SetEditTarget {
                mask: focus == PropertyFocus::Mask,
            });
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
        ui.with_layout(Layout::right_to_left(Align::Center), transform_toggle);
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

// ---------------------------------------------------------------------------
// W3-J: the Transform block, the Align row and the per-kind pages
// ---------------------------------------------------------------------------

/// Where the Transform block's open state lives: panel view state, one for
/// the session, never document state.
fn transform_open_key() -> egui::Id {
    egui::Id::new("raster-properties-transform-open")
}

fn transform_open(ui: &Ui) -> bool {
    ui.memory(|m| {
        m.data
            .get_temp::<bool>(transform_open_key())
            .unwrap_or(false)
    })
}

/// The Transform block's disclosure, drawn at the end of the Kind row so it
/// costs no line of its own: the Properties group shares the rail with
/// Layers, and every line it takes is a Layers row the user loses. Closed
/// until opened; the choice holds for the session.
fn transform_toggle(ui: &mut Ui) {
    let open = transform_open(ui);
    let chevron = if open {
        "chevron-down"
    } else {
        "chevron-right"
    };
    if icon_toggle_id(
        ui,
        chevron,
        true,
        crate::strings::tr("ui.docks.properties.transform.toggle"),
        Some(props::ids::transform_toggle()),
    )
    .clicked()
    {
        ui.memory_mut(|m| m.data.insert_temp(transform_open_key(), !open));
    }
    ui.label(text(ui, "Transform", TextRole::Secondary, TypeRole::Body));
}

/// A number the way the fields show it: up to two decimals, no trailing zeros.
fn px_text(value: f32) -> String {
    let text = format!("{value:.2}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// One `label: [ field ]` pair of the Transform block. Returns the value the
/// user committed (Enter or focus loss), parsed, or `None`.
fn px_field(ui: &mut Ui, label: &str, id: egui::Id, value: f32, enabled: bool) -> Option<f32> {
    let t = current_tokens(ui);
    ui.label(text(ui, label, TextRole::Secondary, TypeRole::Body));
    let committed = ui
        .add_enabled_ui(enabled, |ui| {
            super::text_field_sized(ui, id, &px_text(value), t.metrics.numeric_field_width)
        })
        .inner
        .committed?;
    let parsed: f32 = committed.trim().parse().ok()?;
    parsed.is_finite().then_some(parsed)
}

/// X / Y / W / H, editable, and the Align row — for every layer that has a
/// measurable frame. Each commit is one `Command::TransformLayer`
/// ([`props::Transform`]), so one undo step.
fn transform_block(w: &mut Workspace, ui: &mut Ui, doc: &Document, id: LayerId) {
    // Opened by the disclosure on the Kind row ([`transform_toggle`]).
    if !transform_open(ui) {
        return;
    }
    design::section_header(ui, "Transform");
    // Raster ink is measured by the application, which holds the tile
    // bytes, and published (`RasterInks::publish`) only while this block is
    // drawn and only when the layer changed: the note tells it the block is
    // on screen.
    w.note_transform_block_drawn();
    let inks = props::RasterInks::published(ui.ctx());
    let Some(frame) = props::Transform::frame(doc, id, &inks) else {
        // Just opened, or the layer changed while the block was closed: the
        // measurement arrives on the next frame, so ask for one rather than
        // waiting for the pointer to move.
        if ink_pending(doc, id, &inks) {
            ui.ctx().request_repaint();
        }
        ui.label(hint(
            ui,
            crate::strings::tr("ui.docks.properties.nothing.to.measure"),
        ));
        return;
    };
    let locked = props::Transform::is_locked(doc, id);
    if locked {
        ui.label(hint(
            ui,
            crate::strings::tr("ui.docks.properties.position.locked"),
        ));
    }
    let enabled = !locked;
    let mut commands: Vec<Option<Command>> = Vec::new();
    ui.horizontal(|ui| {
        if let Some(x) = px_field(ui, "X", props::ids::transform_x(id), frame.x, enabled) {
            commands.push(props::Transform::set_x(doc, id, &inks, x));
        }
        ui.add_space(Space::Small.pt());
        if let Some(y) = px_field(ui, "Y", props::ids::transform_y(id), frame.y, enabled) {
            commands.push(props::Transform::set_y(doc, id, &inks, y));
        }
    });
    ui.horizontal(|ui| {
        if let Some(width) = px_field(ui, "W", props::ids::transform_w(id), frame.width, enabled) {
            commands.push(props::Transform::set_width(doc, id, &inks, width));
        }
        ui.add_space(Space::Small.pt());
        if let Some(height) = px_field(ui, "H", props::ids::transform_h(id), frame.height, enabled)
        {
            commands.push(props::Transform::set_height(doc, id, &inks, height));
        }
    });
    // One row: a picker rather than six buttons, so the block stays short
    // enough that the Layers panel sharing the rail keeps its rows.
    design::inspector_field(ui, "Align", |ui| {
        ui.add_enabled_ui(enabled, |ui| {
            let combo = egui::ComboBox::from_id_salt(("raster-properties-align", id))
                .selected_text(body(ui, crate::strings::tr("ui.docks.align.pick")))
                .show_ui(ui, |ui| {
                    for edge in props::AlignEdge::ALL {
                        let row = ui
                            .selectable_label(false, body(ui, edge.label()))
                            .on_hover_text(crate::strings::tr(edge.tip_key()));
                        super::mark(ui, row.rect, props::ids::align(*edge));
                        if row.clicked() {
                            commands.push(props::Transform::align(doc, id, &inks, *edge));
                        }
                    }
                });
            super::mark(ui, combo.response.rect, props::ids::align_picker(id));
        });
    });
    for command in commands.into_iter().flatten() {
        w.emit(Intent::Document(command));
    }
}

/// Whether a pixel-owning layer at or under `id` has no raster-ink
/// measurement for the tiles it holds now — the one case a next frame fixes.
/// An empty layer is measured (`rect: None`), so it is never pending.
fn ink_pending(doc: &Document, id: LayerId, inks: &props::RasterInks) -> bool {
    const MAX_DEPTH: usize = 64;
    let mut stack = vec![(id, 0usize)];
    while let Some((id, depth)) = stack.pop() {
        let Some(layer) = doc.layers.get(id) else {
            continue;
        };
        match &layer.kind {
            layer_model::LayerKind::Group(g) if depth < MAX_DEPTH => {
                stack.extend(g.children.iter().map(|&c| (c, depth + 1)));
            }
            kind if props::RasterInk::owns_pixels(kind) && inks.current(doc, id).is_none() => {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// The text page: family, size, weight and fill, mirroring the Character
/// panel's controls through the same setters, so both surfaces agree.
fn text_properties(w: &mut Workspace, ui: &mut Ui, doc: &Document, id: LayerId) {
    let Some((layer, mut run)) = text_panel::active_text(doc, Some(id)) else {
        return;
    };
    design::section_header(ui, "Type");
    let mut changed = false;
    let mut family: Option<String> = None;
    design::inspector_field(ui, "Family", |ui| {
        family = super::text_field(ui, props::ids::text_family(layer), &run.style.family).committed;
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
        let mut picked = run.style.weight.0;
        egui::ComboBox::from_id_salt("raster-properties-weight")
            .selected_text(body(ui, text_panel::weight_label(run.style.weight)))
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
    design::inspector_field(ui, "Fill", |ui| {
        let mut picked = text_panel::fill_to_swatch(run.style.color);
        if ui.color_edit_button_srgba(&mut picked).changed() {
            changed |=
                text_panel::Character::set_color(&mut run, text_panel::swatch_to_fill(picked));
        }
    });
    if changed {
        if let Some(intent) = text_panel::commit(doc, layer, &run) {
            w.emit(intent);
        }
    }
}

/// The shape page: fill on/off and colour, stroke on/off, colour and width.
///
/// Corner radius: a rectangle's path is recognised and re-rounded in place
/// (`ShapeProperties::set_corner_radius`); any other path gets a note instead
/// of a slider that could not act.
fn shape_properties(w: &mut Workspace, ui: &mut Ui, doc: &Document, id: LayerId) {
    let (Some(fill), Some(stroke)) = (
        props::ShapeProperties::fill(doc, id),
        props::ShapeProperties::stroke(doc, id),
    ) else {
        return;
    };
    design::section_header(ui, "Shape");
    let mut intents: Vec<Option<Intent>> = Vec::new();
    let mut filled = fill.is_some();
    design::inspector_field(ui, "Fill", |ui| {
        let toggle = ui.checkbox(
            &mut filled,
            hint(ui, crate::strings::tr("ui.docks.shape.filled")),
        );
        super::mark(ui, toggle.rect, props::ids::shape_fill_enabled(id));
        if toggle.changed() {
            intents.push(props::ShapeProperties::set_fill_enabled(doc, id, filled));
        }
        if let Some(color) = fill {
            let mut picked = props::shape_to_swatch(color);
            if ui.color_edit_button_srgba(&mut picked).changed() {
                intents.push(props::ShapeProperties::set_fill(
                    doc,
                    id,
                    Some(props::swatch_to_shape(picked)),
                ));
            }
        }
    });
    let mut stroked = stroke.is_some();
    design::inspector_field(ui, "Stroke", |ui| {
        let toggle = ui.checkbox(
            &mut stroked,
            hint(ui, crate::strings::tr("ui.docks.shape.stroked")),
        );
        super::mark(ui, toggle.rect, props::ids::shape_stroke_enabled(id));
        if toggle.changed() {
            intents.push(props::ShapeProperties::set_stroke_enabled(doc, id, stroked));
        }
        if let Some(stroke) = &stroke {
            let mut picked = props::shape_to_swatch(stroke.color);
            if ui.color_edit_button_srgba(&mut picked).changed() {
                intents.push(props::ShapeProperties::set_stroke_color(
                    doc,
                    id,
                    props::swatch_to_shape(picked),
                ));
            }
        }
    });
    if let Some(stroke) = &stroke {
        let mut width = stroke.width_px;
        if design::slider_row(ui, "Width", &mut width, 0.0..=100.0).changed() {
            intents.push(props::ShapeProperties::set_stroke_width(doc, id, width));
        }
    }
    // W3-J: corner radius, for a rectangle or rounded rectangle - the path
    // is re-rounded in place. Any other path has no corners to round.
    match props::ShapeProperties::corner_radius(doc, id) {
        Some(mut radius) => {
            let response = design::slider_row(
                ui,
                crate::strings::tr("ui.docks.shape.radius"),
                &mut radius,
                0.0..=500.0,
            );
            super::mark(ui, response.rect, props::ids::shape_radius(id));
            if response.changed() {
                intents.push(props::ShapeProperties::set_corner_radius(doc, id, radius));
            }
        }
        None => {
            ui.label(hint(ui, crate::strings::tr("ui.docks.shape.no.radius")));
        }
    }
    for intent in intents.into_iter().flatten() {
        w.emit(intent);
    }
}

/// The smart-object page: the source's name and kind, and the two actions
/// the Layer ▸ Smart Objects menu offers, routed to the very same actions.
fn smart_object_properties(
    w: &mut Workspace,
    ui: &mut Ui,
    doc: &Document,
    history: &History,
    id: LayerId,
) {
    let Some(source) = props::smart_object_source(doc, id) else {
        return;
    };
    design::section_header(ui, "Source");
    design::inspector_field(ui, "Source", |ui| {
        if source.name.is_empty() {
            ui.label(hint(ui, crate::strings::tr("ui.docks.smart.no.source")));
        } else {
            ui.label(body(ui, source.name.clone()));
        }
    });
    design::inspector_field(ui, "Kind", |ui| {
        let key = if source.linked {
            "ui.docks.smart.linked"
        } else {
            "ui.docks.smart.embedded"
        };
        ui.label(body(ui, crate::strings::tr(key)));
    });
    // Enablement comes from the menu's own resolver, so the buttons and the
    // Layer > Smart Objects rows cannot disagree.
    let context = w.menu_context(doc, history);
    let replace = props::REPLACE_CONTENTS.resolve(&context);
    let edit = props::EDIT_CONTENTS.resolve(&context);
    ui.horizontal_wrapped(|ui| {
        if super::labelled_button(
            ui,
            &props::REPLACE_CONTENTS.label(),
            replace.is_enabled(),
            props::ids::replace_contents(),
        )
        .clicked()
        {
            w.emit(Intent::Action(props::REPLACE_CONTENTS));
        }
        if super::labelled_button(
            ui,
            &props::EDIT_CONTENTS.label(),
            edit.is_enabled(),
            props::ids::edit_contents(),
        )
        .clicked()
        {
            w.emit(Intent::Action(props::EDIT_CONTENTS));
        }
    });
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
                ui.label(hint(ui, crate::strings::tr("ui.docks.enter.a.colour")));
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
        if icon_action(
            ui,
            "target",
            crate::strings::tr("ui.docks.sample.a.colour.from.the.canvas"),
            ActionState::selected_if(w.color.eyedropper_armed),
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

/// The Brushes panel: the preset list, then Edit / Save.
///
/// `fill_bottom` is set when this group is its column's flexible one — in
/// Essentials the Brushes group is the narrow column's last, so it is — and
/// then the list gets exactly the height between the top of the body and the
/// two footer buttons and scrolls inside it, the way the Layers rows do. Left
/// to its natural height the list pushed the group past the column's bottom
/// at 900pt, and the footer with it.
fn brushes_body(w: &mut Workspace, ui: &mut Ui, fill_bottom: Option<f32>) {
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
    let list_ui = |ui: &mut Ui, apply: &mut Option<usize>, remove: &mut Option<usize>| {
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
                *apply = Some(*i);
            }
            if response.secondary_clicked() {
                *remove = Some(*i);
            }
        }
    };

    // The footer is two stacked buttons under a rule; the list gets what is
    // left above them when the group is stretched to the column's edge.
    let t = current_tokens(ui);
    let footer_reserve = t.metrics.control_height * 2.0
        + Space::XSmall.pt()
        + t.borders.hairline
        + ui.spacing().item_spacing.y * 4.0;
    let list_height = fill_bottom.map(|b| (b - ui.cursor().top() - footer_reserve).max(0.0));
    match list_height {
        Some(height) => {
            egui::ScrollArea::vertical()
                .id_salt("raster-brush-presets")
                .auto_shrink([false, false])
                .max_height(height)
                .min_scrolled_height(height)
                .show(ui, |ui| list_ui(ui, &mut apply, &mut remove));
        }
        None => list_ui(ui, &mut apply, &mut remove),
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
        type_tool_defaults(w, ui, DefaultsPage::Character);
        return;
    };
    let mut changed = false;
    let mut picked_family: Option<String> = None;
    let mut picked_face: Option<text_engine::FaceRecord> = None;
    design::inspector_field(ui, "Family", |ui| {
        // Card 022: the field stays free-text — a name the machine does not
        // have is kept in the document and reported below, never rewritten —
        // and while it is edited the installed families narrow to the search.
        // Picking one commits it exactly like typing it would.
        let edit = super::text_field(ui, super::ids::character_family(layer), &run.style.family);
        if let Some(committed) = edit.committed {
            picked_family = Some(committed);
        }
        if edit.editing {
            let families = compositor::font_families();
            let candidates = text_panel::family_candidates(&edit.text, &families);
            let picked = &mut picked_family;
            egui::popup_below_widget(
                ui,
                super::ids::character_family(layer).with("candidates"),
                &edit.response,
                egui::PopupCloseBehavior::CloseOnClickOutside,
                |popup_ui| {
                    egui::ScrollArea::vertical()
                        .max_height(12.0 * popup_ui.spacing().interact_size.y)
                        .show(popup_ui, |list_ui| {
                            if candidates.is_empty() {
                                list_ui.label(hint(
                                    ui,
                                    crate::strings::tr("ui.docks.character.no.matching.family"),
                                ));
                            }
                            for name in &candidates {
                                if list_ui
                                    .selectable_label(
                                        *name == run.style.family,
                                        body(ui, name.clone()),
                                    )
                                    .clicked()
                                {
                                    // A candidate click beats the same frame's
                                    // commit of the search text, and the edit's
                                    // in-progress buffer is dropped so a later
                                    // Enter re-seeds from the picked family
                                    // instead of committing the search string
                                    // over it.
                                    *picked = Some(name.clone());
                                    ui.memory_mut(|m| {
                                        m.data.remove::<String>(
                                            super::ids::character_family(layer).with("in-progress"),
                                        );
                                    });
                                }
                            }
                        });
                },
            );
        }
    });
    if let Some(family) = picked_family {
        changed |= text_panel::Character::set_family(&mut run, &family);
    }
    // Card 022: an uninstalled family is reported with the substitute the
    // shaper will use — the same rule `attrs_for` applies, so the report and
    // the render agree. The requested name stays in the document.
    if let Some(substitute) = text_panel::substitution(&run.style.family) {
        ui.label(hint(
            ui,
            format!(
                "{} {}",
                crate::strings::tr("ui.docks.character.family.not.installed"),
                substitute
            ),
        ));
    }
    let faces = compositor::font_family_faces(&run.style.family);
    design::inspector_field(ui, "Face", |ui| {
        if faces.is_empty() {
            // Nothing to list — the control cannot act, so instead of a combo
            // that would go nowhere, the row states the reason (the note above
            // names the substitute doing the shaping).
            ui.label(hint(ui, crate::strings::tr("ui.docks.character.face.none")));
        } else {
            let current = faces.iter().find(|f| {
                f.weight == run.style.weight
                    && f.slant == run.style.slant
                    && f.stretch == run.style.stretch
            });
            let selected = current.map_or_else(
                || text_panel::weight_label(run.style.weight).to_string(),
                |f| text_panel::face_label(f.weight, f.slant, f.stretch),
            );
            egui::ComboBox::from_id_salt(super::ids::character_face(layer))
                .selected_text(body(ui, selected))
                .show_ui(ui, |combo_ui| {
                    for face in &faces {
                        let label = text_panel::face_label(face.weight, face.slant, face.stretch);
                        if combo_ui
                            .selectable_label(
                                current
                                    .is_some_and(|c| c.post_script_name == face.post_script_name),
                                body(combo_ui, label),
                            )
                            .clicked()
                        {
                            picked_face = Some(face.clone());
                        }
                    }
                });
        }
    });
    if let Some(face) = picked_face {
        changed |= text_panel::Character::set_face(&mut run, face.weight, face.slant, face.stretch);
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
    character_basics(ui, &mut run, &mut changed);
    character_typography(ui, &mut run, &mut changed);

    if changed {
        if let Some(intent) = text_panel::commit(doc, layer, &run) {
            w.emit(intent);
        }
    }
}

/// W3-J: the Character panel's everyday controls - weight, fill, italic,
/// underline, strike, tracking and leading - drawn on a run. Shared by the
/// text-layer page and the Type tool's defaults page, so the two offer the
/// same controls with the same setters.
fn character_basics(ui: &mut Ui, run: &mut text_engine::TextRun, changed: &mut bool) {
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
            *changed |= text_panel::Character::set_weight(run, picked);
        }
    });
    // Card 021: the direct fill colour control. The model stores linear
    // straight RGBA; egui's picker edits gamma-space RGBA8, so the value is
    // converted on the way in (`swatch_to_fill`) and shown converted back
    // (`fill_to_swatch`). The picker drag reaches the shell as one intent per
    // frame and folds into one undo step under the established gesture
    // contract, exactly like the sliders.
    let mut fill = run.style.color;
    design::inspector_field(ui, "Fill", |ui| {
        let mut picked = text_panel::fill_to_swatch(fill);
        if ui.color_edit_button_srgba(&mut picked).changed() {
            fill = text_panel::swatch_to_fill(picked);
            *changed |= text_panel::Character::set_color(run, fill);
        }
    });
    let mut italic = run.style.slant != text_engine::FontSlant::Normal;
    let mut underline = run.style.underline;
    let mut strike = run.style.strikethrough;
    ui.horizontal(|ui| {
        if ui.checkbox(&mut italic, hint(ui, "Italic")).changed() {
            *changed |= text_panel::Character::set_italic(run, italic);
        }
        if ui.checkbox(&mut underline, hint(ui, "Underline")).changed() {
            *changed |= text_panel::Character::set_underline(run, underline);
        }
        if ui.checkbox(&mut strike, hint(ui, "Strike")).changed() {
            *changed |= text_panel::Character::set_strikethrough(run, strike);
        }
    });
    let mut tracking = run.style.tracking;
    if design::slider_row(ui, "Tracking", &mut tracking, -100.0..=400.0).changed() {
        *changed |= text_panel::Character::set_tracking(run, tracking);
    }
    // W3-J: leading is editable here as well as in Paragraph — it was a
    // read-only readout. Same setters, same run, so the two panels agree.
    let mut leading = text_panel::Character::leading_px(&run.style, &run.paragraph);
    if design::slider_row(ui, "Leading", &mut leading, 1.0..=400.0)
        .on_hover_text(crate::strings::tr("ui.docks.character.leading.tip"))
        .changed()
    {
        *changed |= text_panel::Paragraph::set_leading_px(run, leading);
    }
    if design::ghost_button(ui, crate::strings::tr("ui.docks.auto.leading")).clicked() {
        *changed |= text_panel::Paragraph::set_leading_auto(run, 1.2);
    }
}

/// W3-J: the Character panel's typography block - scale, baseline shift,
/// super/subscript, caps, kerning, ligatures and anti-alias. Every control
/// writes a `TextRun` field that the layout or the rasteriser consumes
/// (`text_engine::shape` / `rasterize`), so none of them is decorative.
fn character_typography(ui: &mut Ui, run: &mut text_engine::TextRun, changed: &mut bool) {
    let mut h_scale = run.style.horizontal_scale * 100.0;
    if design::slider_row(
        ui,
        crate::strings::tr("ui.docks.character.hscale"),
        &mut h_scale,
        text_panel::MIN_SCALE_PERCENT..=text_panel::MAX_SCALE_PERCENT,
    )
    .on_hover_text(crate::strings::tr("ui.docks.character.hscale.tip"))
    .changed()
    {
        *changed |= text_panel::Character::set_horizontal_scale(run, h_scale);
    }
    let mut v_scale = run.style.vertical_scale * 100.0;
    if design::slider_row(
        ui,
        crate::strings::tr("ui.docks.character.vscale"),
        &mut v_scale,
        text_panel::MIN_SCALE_PERCENT..=text_panel::MAX_SCALE_PERCENT,
    )
    .on_hover_text(crate::strings::tr("ui.docks.character.vscale.tip"))
    .changed()
    {
        *changed |= text_panel::Character::set_vertical_scale(run, v_scale);
    }
    let mut shift = run.style.baseline_shift;
    if design::slider_row(
        ui,
        crate::strings::tr("ui.docks.character.baseline.shift"),
        &mut shift,
        -200.0..=200.0,
    )
    .changed()
    {
        *changed |= text_panel::Character::set_baseline_shift(run, shift);
    }
    let mut script_index = text_panel::SCRIPTS
        .iter()
        .position(|s| *s == run.style.script)
        .unwrap_or(0);
    let script_labels: Vec<&str> = text_panel::SCRIPTS
        .iter()
        .map(|s| text_panel::script_label(*s))
        .collect();
    design::inspector_field(ui, "Position", |ui| {
        if design::segmented_control(ui, "raster-char-script", &mut script_index, &script_labels) {
            *changed |= text_panel::Character::set_script(run, text_panel::SCRIPTS[script_index]);
        }
    })
    .response
    .on_hover_text(crate::strings::tr("ui.docks.character.script.tip"));
    let mut caps_index = text_panel::CAPS
        .iter()
        .position(|c| *c == run.style.caps)
        .unwrap_or(0);
    let caps_labels: Vec<&str> = text_panel::CAPS
        .iter()
        .map(|c| text_panel::caps_label(*c))
        .collect();
    design::inspector_field(ui, "Caps", |ui| {
        if design::segmented_control(ui, "raster-char-caps", &mut caps_index, &caps_labels) {
            *changed |= text_panel::Character::set_caps(run, text_panel::CAPS[caps_index]);
        }
    })
    .response
    .on_hover_text(crate::strings::tr("ui.docks.character.caps.tip"));
    let (mode, manual) = text_panel::Character::kerning_mode(run);
    // Manual kerning sits between two characters; on shorter text (and on
    // the Type tool's defaults, which have no text) it is not offered, and
    // the tooltip says why.
    let modes: &[text_panel::KerningMode] = if text_panel::manual_kerning_available(run) {
        text_panel::KerningMode::ALL
    } else {
        &text_panel::KerningMode::ALL[..2]
    };
    let mut kern_index = modes.iter().position(|m| *m == mode).unwrap_or(0);
    let kern_labels: Vec<&str> = modes.iter().map(|m| m.label()).collect();
    design::inspector_field(ui, "Kerning", |ui| {
        if design::segmented_control(ui, "raster-char-kerning", &mut kern_index, &kern_labels) {
            *changed |= text_panel::Character::set_kerning_mode(
                run,
                modes[kern_index],
                manual.unwrap_or(0.0),
            );
        }
    })
    .response
    .on_hover_text(crate::strings::tr("ui.docks.character.kerning.tip"));
    if mode == text_panel::KerningMode::Manual {
        let mut amount = manual.unwrap_or(0.0);
        if design::slider_row(
            ui,
            crate::strings::tr("ui.docks.character.kerning.amount"),
            &mut amount,
            -1000.0..=1000.0,
        )
        .changed()
        {
            *changed |= text_panel::Character::set_kerning_mode(
                run,
                text_panel::KerningMode::Manual,
                amount,
            );
        }
    }
    let mut ligatures = run.style.ligatures;
    if ui
        .checkbox(
            &mut ligatures,
            hint(ui, crate::strings::tr("ui.docks.character.ligatures")),
        )
        .changed()
    {
        *changed |= text_panel::Character::set_ligatures(run, ligatures);
    }
    let mut aa_index = text_panel::ANTI_ALIAS
        .iter()
        .position(|a| *a == run.style.anti_alias)
        .unwrap_or(0);
    let aa_labels: Vec<&str> = text_panel::ANTI_ALIAS
        .iter()
        .map(|a| text_panel::anti_alias_label(*a))
        .collect();
    design::inspector_field(ui, "Edges", |ui| {
        if design::segmented_control(ui, "raster-char-antialias", &mut aa_index, &aa_labels) {
            *changed |=
                text_panel::Character::set_anti_alias(run, text_panel::ANTI_ALIAS[aa_index]);
        }
    })
    .response
    .on_hover_text(crate::strings::tr("ui.docks.character.antialias.tip"));
}

/// W3-J: alignment (with the four justify variants), leading, the three
/// indents and paragraph spacing, drawn on a run. Shared by the text-layer
/// page and the Type tool's defaults page.
fn paragraph_style_controls(ui: &mut Ui, run: &mut text_engine::TextRun, changed: &mut bool) {
    let mut index = text_panel::alignment_index(run.paragraph.alignment);
    let labels: Vec<&str> = text_panel::ALIGNMENTS
        .iter()
        .map(|a| text_panel::alignment_label(*a))
        .collect();
    if design::segmented_control(ui, "raster-paragraph-align", &mut index, &labels) {
        *changed |= text_panel::Paragraph::set_alignment(run, text_panel::ALIGNMENTS[index]);
    }
    // W3-J: the four justify variants - where a justified paragraph's last
    // line goes - as a second row, shown while Justify is on.
    if run.paragraph.alignment.is_justified() {
        let mut last = text_panel::JUSTIFY_VARIANTS
            .iter()
            .position(|a| *a == run.paragraph.alignment)
            .unwrap_or(0);
        let last_labels: Vec<&str> = text_panel::JUSTIFY_VARIANTS
            .iter()
            .map(|a| text_panel::last_line_label(*a))
            .collect();
        design::inspector_field(
            ui,
            crate::strings::tr("ui.docks.paragraph.last.line"),
            |ui| {
                if design::segmented_control(
                    ui,
                    "raster-paragraph-last-line",
                    &mut last,
                    &last_labels,
                ) {
                    *changed |= text_panel::Paragraph::set_alignment(
                        run,
                        text_panel::JUSTIFY_VARIANTS[last],
                    );
                }
            },
        );
    }

    let mut leading = text_panel::Character::leading_px(&run.style, &run.paragraph);
    if design::slider_row(ui, "Leading", &mut leading, 1.0..=400.0).changed() {
        *changed |= text_panel::Paragraph::set_leading_px(run, leading);
    }
    if design::ghost_button(ui, crate::strings::tr("ui.docks.auto.leading")).clicked() {
        *changed |= text_panel::Paragraph::set_leading_auto(run, 1.2);
    }

    // W3-J: left and right indents move every line; the box wraps inside
    // them. The first-line indent adds to the left one.
    let mut left = run.paragraph.left_indent;
    if design::slider_row(
        ui,
        crate::strings::tr("ui.docks.paragraph.indent.left"),
        &mut left,
        -200.0..=1000.0,
    )
    .changed()
    {
        *changed |= text_panel::Paragraph::set_left_indent(run, left);
    }
    let mut right = run.paragraph.right_indent;
    if design::slider_row(
        ui,
        crate::strings::tr("ui.docks.paragraph.indent.right"),
        &mut right,
        -200.0..=1000.0,
    )
    .changed()
    {
        *changed |= text_panel::Paragraph::set_right_indent(run, right);
    }
    let mut indent = run.paragraph.first_line_indent;
    if design::slider_row(
        ui,
        crate::strings::tr("ui.docks.paragraph.indent.first"),
        &mut indent,
        -200.0..=200.0,
    )
    .changed()
    {
        *changed |= text_panel::Paragraph::set_first_line_indent(run, indent);
    }
    let mut before = run.paragraph.space_before;
    if design::slider_row(ui, "Before", &mut before, 0.0..=200.0).changed() {
        *changed |= text_panel::Paragraph::set_space_before(run, before);
    }
    let mut after = run.paragraph.space_after;
    if design::slider_row(ui, "After", &mut after, 0.0..=200.0).changed() {
        *changed |= text_panel::Paragraph::set_space_after(run, after);
    }
}

/// W3-J: which panel is drawing the Type tool's defaults.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DefaultsPage {
    Character,
    Paragraph,
}

/// W3-J: with no text layer selected, the Character and Paragraph panels
/// edit the Type tool's DEFAULT style - held on the workspace as the Type
/// tool's options (`Workspace::options`) and seeded into the next layer the
/// tool creates (`tools::text::TypeTool::seed`). The controls are the very
/// ones the text-layer page draws, run on a run built from those options
/// (`text_panel::type_defaults`); every value the edit changed goes back
/// through the same `SetToolOption` route the options bar uses, so the panels
/// and the bar can never disagree.
fn type_tool_defaults(w: &mut Workspace, ui: &mut Ui, page: DefaultsPage) {
    let tool = tools::ToolId::Type;
    design::section_header(ui, crate::strings::tr("ui.docks.character.type.defaults"));
    let emit = |w: &mut Workspace, key: &'static str, value: crate::OptionValue| {
        if w.options.set(tool, key, value) {
            w.emit(Intent::SetToolOption { tool, key, value });
        }
    };
    if page == DefaultsPage::Character {
        if let (Some(spec), Some(crate::OptionValue::Choice(index))) = (
            crate::ToolOptions::spec_for_test(tool, "font_family"),
            w.options.get(tool, "font_family"),
        ) {
            if let tools::OptionKind::Choice { choices, .. } = spec.kind {
                let mut picked = index;
                design::inspector_field(ui, "Family", |ui| {
                    egui::ComboBox::from_id_salt("raster-char-default-family")
                        .selected_text(body(ui, choices.get(index).copied().unwrap_or_default()))
                        .show_ui(ui, |ui| {
                            for (i, choice) in choices.iter().enumerate() {
                                if ui.selectable_label(i == index, body(ui, *choice)).clicked() {
                                    picked = i;
                                }
                            }
                        });
                });
                if picked != index {
                    emit(w, "font_family", crate::OptionValue::Choice(picked));
                }
            }
        }
        if let Some(crate::OptionValue::Float(mut size)) = w.options.get(tool, "size_px") {
            if design::slider_row(ui, "Size", &mut size, 4.0..=512.0).changed() {
                emit(w, "size_px", crate::OptionValue::Float(size));
            }
        }
    }
    let before = text_panel::type_defaults(&w.options);
    let mut run = before.clone();
    let mut changed = false;
    match page {
        DefaultsPage::Character => {
            character_basics(ui, &mut run, &mut changed);
            character_typography(ui, &mut run, &mut changed);
        }
        DefaultsPage::Paragraph => paragraph_style_controls(ui, &mut run, &mut changed),
    }
    if changed {
        for (key, value) in text_panel::type_default_writes(&before, &run) {
            emit(w, key, value);
        }
    }
    ui.label(hint(
        ui,
        crate::strings::tr("ui.docks.character.type.defaults.note"),
    ));
}

fn paragraph_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    let Some((layer, mut run)) = text_panel::active_text(doc, doc.active_layer()) else {
        empty_state(ui, text_panel::no_text_layer_reason());
        type_tool_defaults(w, ui, DefaultsPage::Paragraph);
        return;
    };
    let mut changed = false;

    // Card 023: point text vs a wrapping box. The box is the paragraph's own
    // geometry: changing it reflows without touching the em size, and the
    // layer transform stays the layer transform. A fixed height that is too
    // small never clips — the engine reports the overset and the panel shows
    // it below.
    let mut boxed = matches!(run.frame, text_engine::TextFrame::Box { .. });
    let mut want_boxed = boxed;
    design::inspector_field(ui, "Frame", |ui| {
        ui.checkbox(
            &mut want_boxed,
            hint(ui, crate::strings::tr("ui.docks.paragraph.boxed")),
        );
    });
    if want_boxed != boxed {
        changed |= text_panel::Paragraph::set_boxed(&mut run, want_boxed);
        boxed = want_boxed;
    }
    if boxed {
        if let Some((width, mut height)) = text_panel::Paragraph::box_size(&run) {
            let mut w = width;
            if design::slider_row(
                ui,
                "Width",
                &mut w,
                text_panel::MIN_BOX_SIZE_PX..=text_panel::MAX_BOX_SIZE_PX,
            )
            .changed()
            {
                changed |= text_panel::Paragraph::set_box_width(&mut run, w);
            }
            let mut fixed = height.is_some();
            design::inspector_field(ui, "Height", |ui| {
                if ui
                    .checkbox(
                        &mut fixed,
                        hint(ui, crate::strings::tr("ui.docks.paragraph.fixed.height")),
                    )
                    .changed()
                {
                    // Seeding from the laid-out content height: fixing the
                    // height starts where the auto box already is, so the
                    // switch itself never oversets.
                    height = fixed.then(|| compositor::text_content_height(&run));
                    changed |= text_panel::Paragraph::set_box_height(&mut run, height);
                }
            });
            if let Some(h) = height {
                let mut vh = h;
                if design::slider_row(
                    ui,
                    crate::strings::tr("ui.docks.paragraph.box.height"),
                    &mut vh,
                    text_panel::MIN_BOX_SIZE_PX..=text_panel::MAX_BOX_SIZE_PX,
                )
                .changed()
                {
                    changed |= text_panel::Paragraph::set_box_height(&mut run, Some(vh));
                }
            }
        }
    }
    if let Some(count) = text_panel::overset_lines(&run) {
        if count > 0 {
            ui.label(hint(
                ui,
                format!(
                    "{} — {} {}",
                    crate::strings::tr("ui.docks.paragraph.overset"),
                    count,
                    crate::strings::tr("ui.docks.paragraph.overset.lines"),
                ),
            ));
        }
    }

    paragraph_style_controls(ui, &mut run, &mut changed);

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
        // The composite the application uploaded, when there is one; the
        // checkerboard alone is what a headless draw (no application) sees.
        if let Some(tex) = w.navigator_texture.as_ref() {
            ui.painter().image(
                tex.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                crate::dialogs::controls::UNTINTED,
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
        if icon_action(
            ui,
            "minus",
            crate::strings::tr("ui.docks.zoom.out"),
            ActionState::Idle,
        )
        .clicked()
        {
            w.emit(Intent::SetZoom(crate::panels::navigator::zoom_out(
                w.status.zoom,
            )));
        }
        // Photopea's slider between the two steppers, logarithmic so the
        // ladder's small rungs get as much travel as its large ones. It is
        // bound to the same `SetZoom` intent the steppers post, so the
        // application moves the one camera for all three.
        let mut zoom = crate::panels::navigator::clamp_zoom(w.status.zoom);
        let slider_w = (ui.available_width()
            - t.metrics.numeric_field_width
            - panel_icon_side(t)
            - t.metrics.control_height * 2.0)
            .max(t.metrics.min_hit_target);
        let slider = ui.add_sized(
            Vec2::new(slider_w, t.metrics.control_height),
            egui::Slider::new(
                &mut zoom,
                crate::panels::navigator::MIN_ZOOM..=crate::panels::navigator::MAX_ZOOM,
            )
            .logarithmic(true)
            .show_value(false),
        );
        super::mark(ui, slider.rect, crate::dock::ids::navigator_zoom());
        let slider = slider.on_hover_text(crate::strings::tr("ui.docks.zoom.slider"));
        if slider.changed() && zoom != w.status.zoom {
            w.emit(Intent::SetZoom(zoom));
        }
        ui.label(body(ui, format_zoom(w.status.zoom)));
        if icon_action(
            ui,
            "plus",
            crate::strings::tr("ui.docks.zoom.in"),
            ActionState::Idle,
        )
        .clicked()
        {
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
    // W3-G: the Document row reads in the Units preference, which the shell
    // pushes into the workspace as the rulers' unit.
    for readout in w.info.readouts_in(doc, w.canvas.unit) {
        design::inspector_field(ui, readout.label, |ui| {
            let label = ui.label(body(ui, readout.value.clone()));
            // Named so a test can read the value the row shows — the RGB and
            // Hex rows were "—" for the life of a session before
            // `Workspace::set_info_sample` existed, and nothing measured it.
            super::mark(ui, label.rect, crate::dock::ids::info_value(readout.label));
        });
    }
}

// ---------------------------------------------------------------------------
// Histogram
// ---------------------------------------------------------------------------

/// The Histogram panel: the RGB or luminosity distribution of the composite
/// the application last handed to [`Workspace::set_composite_preview`].
///
/// The plot is one filled bar per bin over a sunken well, marked under
/// `dock::ids::histogram_plot` so a headless frame can count the bars it
/// painted. The channel curves are drawn in the theme's data roles
/// ([`crate::panels::histogram::channel_role`]: red counts red, in the red
/// the appearance can show over the well); the luminosity curve is
/// `ColorRole::Luminance`.
fn histogram_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    use crate::panels::histogram::{HistogramMode, BINS};
    let t = current_tokens(ui);
    let mut index = HistogramMode::ALL
        .iter()
        .position(|m| *m == w.histogram.mode)
        .unwrap_or(0);
    let labels = [
        crate::strings::tr("ui.docks.histogram.rgb"),
        crate::strings::tr("ui.docks.histogram.luminosity"),
    ];
    if design::segmented_control(ui, "raster-histogram-mode", &mut index, &labels) {
        w.histogram.mode = HistogramMode::ALL[index];
    }
    ui.add_space(Space::XSmall.pt());

    let width = ui.available_width();
    let height = t.metrics.control_height * 4.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    super::mark(ui, rect, crate::dock::ids::histogram_plot());
    if ui.is_rect_visible(rect) {
        let radius = Radius::Small.resolve(&t.radii, rect.height());
        ui.painter().rect_filled(
            rect,
            rounding(radius),
            color32(t.palette.color(ColorRole::SurfaceSunken)),
        );
        ui.painter().rect_stroke(
            rect,
            rounding(radius),
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::ControlStroke)),
            ),
        );
        if let Some(bins) = w.histogram.bins() {
            let mode = w.histogram.mode;
            let peak = bins.peak(mode).max(1) as f32;
            let plot = rect.shrink(t.borders.hairline);
            let bar_w = plot.width() / BINS as f32;
            // The curve colours are the theme's data roles (see
            // `panels::histogram::channel_role`: red counts red, in the red
            // this appearance can show over the well) — translucent so
            // three channels that agree stack towards white where the image
            // is neutral, Photoshop's "Colors" reading.
            use crate::panels::histogram::curve_tint;
            let channels: Vec<(&[u32; BINS], egui::Color32)> = match mode {
                HistogramMode::Rgb => vec![
                    (&bins.red, color32(curve_tint(&t.palette, 0))),
                    (&bins.green, color32(curve_tint(&t.palette, 1))),
                    (&bins.blue, color32(curve_tint(&t.palette, 2))),
                ],
                HistogramMode::Luminosity => vec![(
                    &bins.luminosity,
                    color32(t.palette.color(ColorRole::Luminance)),
                )],
            };
            for (counts, colour) in channels {
                for (bin, count) in counts.iter().enumerate() {
                    if *count == 0 {
                        continue;
                    }
                    let h = (*count as f32 / peak) * plot.height();
                    let x = plot.left() + bin as f32 * bar_w;
                    let bar = egui::Rect::from_min_max(
                        egui::pos2(x, plot.bottom() - h),
                        egui::pos2(x + bar_w, plot.bottom()),
                    );
                    ui.painter().rect_filled(bar, egui::Rounding::ZERO, colour);
                }
            }
        }
    }

    match w.histogram.bins() {
        None => {
            ui.add_space(Space::XSmall.pt());
            // With a document open the composite is on its way — the
            // application hands it over after its next composite — so the
            // panel says it is waiting, not that there is nothing to see.
            // The chrome is drawn against a 0×0 placeholder when nothing is
            // open (see the application's dock host), which is the one case
            // the other string is for.
            let has_document = doc.width() > 0 && doc.height() > 0;
            empty_state(
                ui,
                crate::strings::tr(if has_document {
                    "ui.docks.histogram.waiting"
                } else {
                    "ui.docks.histogram.no.composite"
                }),
            );
        }
        Some(bins) if bins.samples == 0 => {
            ui.add_space(Space::XSmall.pt());
            empty_state(ui, crate::strings::tr("ui.docks.histogram.empty"));
        }
        Some(bins) => {
            ui.add_space(Space::XSmall.pt());
            design::inspector_field(ui, crate::strings::tr("ui.docks.histogram.mean"), |ui| {
                let mean = bins.mean_luminosity().unwrap_or(0.0);
                ui.label(body(ui, format!("{mean:.1}")));
            });
            design::inspector_field(ui, crate::strings::tr("ui.docks.histogram.pixels"), |ui| {
                // The count is of the bounded sample, scaled back to the
                // document so the number means the image, not the thumbnail.
                let total = u64::from(doc.width()) * u64::from(doc.height());
                ui.label(body(ui, total.to_string()));
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Channels and Paths
// ---------------------------------------------------------------------------

/// The Channels footer's actions, in Photopea's order.
const CHANNEL_ACTIONS: [&str; 4] = ["load", "save", "new", "delete"];

fn channels_body(w: &mut Workspace, ui: &mut Ui, doc: &Document, history: &History) {
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
                crate::strings::tr("ui.docks.show.hide.channel"),
                Some(super::ids::channel_eye(index)),
            )
            .clicked()
            {
                toggle = Some((row.kind, !row.visible));
            }
            channel_thumbnail(w, ui, row.kind);
            ui.add_space(Space::XSmall.pt());
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
    saved_selection_rows(w, ui, doc);
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

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    channel_footer(w, ui, doc, history);
}

/// The document's saved selections (Select > Save Selection), listed under
/// the colour and mask channels as Photopea lists its alpha channels: one row
/// per name, oldest first, each with the mask glyph in its well. A click
/// opens Select > Load Selection -- the dialog that restores a saved
/// selection by name with New / Add / Subtract / Intersect -- on the row that
/// was clicked ([`Workspace::pending_selection_load`]); a Ctrl+click loads
/// that row as the new selection directly. The row never promises an action
/// no route performs.
fn saved_selection_rows(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    for (index, (name, _)) in doc.saved_selections.iter().enumerate() {
        let response = row_layout(ui, |ui| {
            let t = current_tokens(ui);
            let height = t.metrics.list_row_height - Space::XSmall.pt();
            // The eye column's width, left empty: an alpha row has no
            // visibility of its own in this build.
            ui.add_space(height);
            let size = Vec2::new(height * 4.0 / 3.0, height);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            if ui.is_rect_visible(rect) {
                super::checkerboard(ui.painter(), rect, Space::XSmall.pt());
                let side = rect.height() * 0.7;
                let icon_rect = egui::Rect::from_center_size(rect.center(), Vec2::splat(side));
                super::paint_icon(ui, icon_rect, "mask", TextRole::Tertiary);
            }
            ui.add_space(Space::XSmall.pt());
            ui.label(body(ui, name.clone()));
        })
        .response
        .rect;
        let response = ui
            .interact(response, saved_selection_row_id(index), Sense::click())
            .on_hover_text(crate::strings::tr("ui.docks.channels.saved.hint"));
        if response.clicked() {
            // W3-X: the row that was clicked, not the most recent entry. A
            // Ctrl+click loads it as the selection without asking, as
            // Photopea's Ctrl+click on a channel thumbnail does.
            let direct = ui.input(|i| i.modifiers.command);
            w.pending_selection_load = Some(crate::SelectionLoadRequest { index, direct });
            w.emit(Intent::Action(crate::menu::MenuAction::LoadSelection));
        }
    }
}

/// The id of the `index`th saved-selection row in the Channels panel.
pub(crate) fn saved_selection_row_id(index: usize) -> egui::Id {
    egui::Id::new(("channels-saved-selection", index))
}

/// The 4:3 well beside a channel row.
///
/// A component row is the composite preview *tinted to its primary* — egui
/// multiplies the texture by the tint, so a red tint leaves exactly the red
/// channel's contribution, which is what Photoshop's colour-channel
/// thumbnails show. The composite row is the preview untinted; a mask row is
/// the mask's own coverage thumbnail. With no texture (a headless draw, or a
/// mask the application has not thumbnailed yet) the well shows the
/// checkerboard and a glyph, never a blank.
fn channel_thumbnail(w: &mut Workspace, ui: &mut Ui, kind: ChannelKind) {
    let t = current_tokens(ui);
    let height = t.metrics.list_row_height - Space::XSmall.pt();
    let size = Vec2::new(height * 4.0 / 3.0, height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.on_hover_text(crate::strings::tr("ui.docks.channels.thumbnail"));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let radius = Radius::Small.resolve(&t.radii, size.y);
    super::checkerboard(ui.painter(), rect, Space::XSmall.pt());
    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
    let painted = match kind {
        ChannelKind::Composite => w.navigator_texture.as_ref().map(|tex| {
            ui.painter()
                .image(tex.id(), rect, uv, crate::dialogs::controls::UNTINTED);
        }),
        ChannelKind::Component(i) => w.navigator_texture.as_ref().map(|tex| {
            // The channel's own colour role (the first component is red in
            // every RGB-primaried space this build supports) — the same red
            // the Histogram's curve is drawn in, read from the same slot.
            let tint = t.palette.color(crate::panels::histogram::channel_role(i));
            ui.painter().image(tex.id(), rect, uv, color32(tint));
        }),
        ChannelKind::Mask { layer, .. } => w.mask_thumbs.get(&layer).map(|tex| {
            ui.painter()
                .image(tex.id(), rect, uv, crate::dialogs::controls::UNTINTED);
        }),
    };
    if painted.is_none() {
        let side = rect.height() * 0.7;
        let icon_rect = egui::Rect::from_center_size(rect.center(), Vec2::splat(side));
        let key = match kind {
            ChannelKind::Mask { .. } => "mask",
            _ => "layer-raster",
        };
        super::paint_icon(ui, icon_rect, key, TextRole::Tertiary);
    }
    ui.painter().rect_stroke(
        rect,
        rounding(radius),
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::ControlStroke)),
        ),
    );
}

/// Photopea's Channels footer: load the channel as a selection, save the
/// selection as a channel, new channel, delete channel.
///
/// Every action is either routed or *greyed with its reason* — never a live
/// button that does nothing, and never a button that does something other
/// than its label. Save goes through the Select menu's own `SaveSelection`,
/// resolved against the same context the menu bar uses so the gate and the
/// reason are the menu's. Load is greyed on every row, and its reason names
/// what is missing: no `Intent` or `editor_core::Command` builds a selection
/// from a channel's coverage in this build (`Command::SetSelection` exists,
/// but the workspace cannot read a mask's tiles to fill one), and the Select
/// menu's `LoadSelection` restores the last *saved* selection, which is a
/// different thing — routing the button there fired the wrong command under
/// the right label, which `tests/panel_chrome_geometry.rs` now pins against.
/// New and Delete have no store to act on — channels live on layers here —
/// and say so.
fn channel_footer(w: &mut Workspace, ui: &mut Ui, doc: &Document, history: &History) {
    let context = w.menu_context(doc, history);
    let selected = w.channels.selected;
    let is_mask = matches!(selected, ChannelKind::Mask { .. });

    // Load: greyed, with the reason that fits the row. A component has no
    // selection to load; a mask *is* a selection's shape, but no command
    // loads a mask as the selection yet, so the button names the missing
    // command rather than firing `LoadSelection` (a saved selection).
    let load: Result<Intent, &'static str> = Err(crate::strings::tr(if is_mask {
        "ui.docks.channels.no.mask.route"
    } else {
        "ui.docks.channels.not.a.mask"
    }));
    let save = match crate::menu::MenuAction::SaveSelection.resolve(&context) {
        crate::menu::Resolution::Enabled(intent) => Ok(intent),
        crate::menu::Resolution::Disabled(_) => Err(crate::strings::tr(if context.has_document {
            "ui.docks.channels.no.selection"
        } else {
            "ui.docks.channels.no.document"
        })),
    };
    let no_store: Result<Intent, &'static str> =
        Err(crate::strings::tr("ui.docks.channels.no.alpha.store"));

    let actions: [(
        &'static str,
        &'static str,
        &'static str,
        Result<Intent, &'static str>,
    ); 4] = [
        (
            CHANNEL_ACTIONS[0],
            "target",
            crate::strings::tr("ui.docks.channels.load.selection"),
            load,
        ),
        (
            CHANNEL_ACTIONS[1],
            "mask",
            crate::strings::tr("ui.docks.channels.save.selection"),
            save,
        ),
        (
            CHANNEL_ACTIONS[2],
            "plus",
            crate::strings::tr("ui.docks.channels.new"),
            no_store.clone(),
        ),
        (
            CHANNEL_ACTIONS[3],
            "trash",
            crate::strings::tr("ui.docks.channels.delete"),
            no_store,
        ),
    ];
    let mut fire: Option<Intent> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Space::Hair.pt();
        for (name, icon, tip, route) in actions {
            let (state, tooltip): (ActionState, String) = match &route {
                Ok(_) => (ActionState::Idle, tip.to_string()),
                Err(reason) => (ActionState::Disabled, format!("{tip} — {reason}")),
            };
            let response = icon_action_id(
                ui,
                icon,
                &tooltip,
                state,
                Some(crate::dock::ids::channel_action(name)),
            );
            if let Ok(intent) = route {
                if response.clicked() {
                    fire = Some(intent);
                }
            }
        }
    });
    if let Some(intent) = fire {
        w.emit(intent);
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
            if icon_toggle(
                ui,
                "eye",
                row.visible,
                crate::strings::tr("ui.docks.show.hide.path"),
            )
            .clicked()
            {
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
    empty_state(ui, crate::strings::tr("actions.hint"));
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

    /// A headless frame of the Channels body with two saved selections:
    /// both names are painted as rows under the channels, in order, and a
    /// click on a row asks for Select > Load Selection.
    #[test]
    fn saved_selections_are_listed_as_alpha_rows_that_open_load_selection() {
        let mut doc = Document::new(8, 8, "alpha");
        doc.saved_selections
            .push(("Alpha 1".to_string(), editor_core::Selection::None));
        doc.saved_selections
            .push(("Keep".to_string(), editor_core::Selection::None));
        let history = History::default();
        let mut w = Workspace::new();
        let ctx = egui::Context::default();
        let frame = |w: &mut Workspace, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    channels_body(w, ui, &doc, &history);
                });
            })
        };
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 600.0));
        let raw = || egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let _ = frame(&mut w, raw());
        let out = frame(&mut w, raw());
        let mut texts: Vec<(f32, String)> = Vec::new();
        for clipped in &out.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                texts.push((text.pos.y, text.galley.text().to_string()));
            }
        }
        let y_of = |needle: &str| {
            texts
                .iter()
                .find(|(_, s)| s == needle)
                .map(|(y, _)| *y)
                .unwrap_or_else(|| panic!("{needle:?} not painted: {texts:?}"))
        };
        let first = y_of("Alpha 1");
        let second = y_of("Keep");
        assert!(first < second, "rows out of order: {texts:?}");
        let rect = ctx
            .read_response(saved_selection_row_id(1))
            .expect("the second row is interactive")
            .rect;
        assert!(rect.contains(egui::pos2(rect.center().x, second + 1.0)));

        let _ = w.drain_intents();
        let click = |pressed: bool| egui::Event::PointerButton {
            pos: rect.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut input = raw();
        input.events = vec![egui::Event::PointerMoved(rect.center()), click(true)];
        let _ = frame(&mut w, input);
        let mut input = raw();
        input.events = vec![click(false)];
        let _ = frame(&mut w, input);
        assert_eq!(
            w.drain_intents(),
            vec![Intent::Action(crate::menu::MenuAction::LoadSelection)]
        );
        // W3-X: the click names the row it landed on, so the dialog can open
        // on "Keep" rather than on the most recent entry.
        assert_eq!(
            w.take_pending_selection_load(),
            Some(crate::SelectionLoadRequest {
                index: 1,
                direct: false
            })
        );

        // Ctrl+click on the first row asks for a direct load of that row.
        let first_rect = ctx
            .read_response(saved_selection_row_id(0))
            .expect("the first row is interactive")
            .rect;
        let ctrl_click = |pressed: bool| egui::Event::PointerButton {
            pos: first_rect.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::COMMAND,
        };
        let mut input = raw();
        input.modifiers = egui::Modifiers::COMMAND;
        input.events = vec![
            egui::Event::PointerMoved(first_rect.center()),
            ctrl_click(true),
        ];
        let _ = frame(&mut w, input);
        let mut input = raw();
        input.modifiers = egui::Modifiers::COMMAND;
        input.events = vec![ctrl_click(false)];
        let _ = frame(&mut w, input);
        assert_eq!(
            w.drain_intents(),
            vec![Intent::Action(crate::menu::MenuAction::LoadSelection)]
        );
        assert_eq!(
            w.take_pending_selection_load(),
            Some(crate::SelectionLoadRequest {
                index: 0,
                direct: true
            })
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
