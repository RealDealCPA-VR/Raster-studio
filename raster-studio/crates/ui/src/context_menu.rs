//! Right-click context menus, following the menu bar's contract.
//!
//! Every menu is a pure function of a [`MenuContext`] to a list of items, and
//! every item resolves through [`MenuAction::resolve`] — the same gate the
//! menu bar applies, so a greyed-out entry always carries a reason sentence.
//!
//! The drawer is a small foreground [`egui::Area`], not egui's built-in
//! `context_menu`, so headless tests can name every item: the buttons carry
//! [`ids::context_item`] ids, a right-click arms the menu through
//! [`Workspace::context_menu`], and the test clicks an item by id like any
//! other control.

use crate::menu::{MenuAction, MenuContext, Resolution};
use crate::Workspace;
use design::{self, ColorRole, TextRole, TypeRole};

/// Which surface a context menu was opened on. `LayerRow` carries no payload:
/// the items resolve against the menu context — the same gates the bar applies
/// to the Layer menu — so the row menu acts on what they resolve to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ContextTarget {
    Canvas,
    LayerRow,
    DocumentTab,
}

/// One entry of a context menu: the label, what it does, and whether it may.
#[derive(Clone, Debug)]
pub struct MenuItem {
    pub label: String,
    pub action: MenuAction,
    pub resolution: Resolution,
}

fn items(ctx: &MenuContext, actions: &[MenuAction]) -> Vec<MenuItem> {
    actions
        .iter()
        .map(|action| MenuItem {
            label: action.label(),
            action: *action,
            resolution: action.resolve(ctx),
        })
        .collect()
}

/// The canvas menu: fill and stroke where you clicked, the transform family,
/// and the selection operations.
pub fn canvas_items(ctx: &MenuContext) -> Vec<MenuItem> {
    items(
        ctx,
        &[
            MenuAction::FillDialog,
            MenuAction::StrokeDialog,
            MenuAction::FreeTransform,
            MenuAction::TransformSelection,
            MenuAction::SelectAll,
            MenuAction::Deselect,
            MenuAction::InverseSelection,
        ],
    )
}

/// The layer-row menu: the operations Photopea's Layers panel offers on a row.
pub fn layer_items(ctx: &MenuContext) -> Vec<MenuItem> {
    items(
        ctx,
        &[
            MenuAction::DuplicateLayer,
            MenuAction::DeleteLayer,
            MenuAction::BlendingOptions,
            MenuAction::Rasterize(crate::menu::RasterizeTarget::Layer),
            MenuAction::MergeDown,
            MenuAction::CreateClippingMask,
        ],
    )
}

/// The document-tab menu: the close family from the File menu.
pub fn tab_items(ctx: &MenuContext) -> Vec<MenuItem> {
    items(
        ctx,
        &[
            MenuAction::CloseDocument,
            MenuAction::CloseOthers,
            MenuAction::CloseAll,
        ],
    )
}

/// Open the menu on `target` at `pos` — the pointer position of the
/// right-click, so the menu appears where the mouse is.
pub fn open(w: &mut Workspace, target: ContextTarget, pos: egui::Pos2) {
    w.context_menu = Some((target, pos));
    // The release click that opened the menu must not immediately close it.
    w.context_menu_fresh = true;
}

/// Draw the open menu, if any, and handle its clicks. Called once per frame
/// from [`Workspace::ui`], after everything else, so the menu floats above.
pub fn draw_open(w: &mut Workspace, ctx: &egui::Context, menu_ctx: &MenuContext) {
    let Some((target, pos)) = w.context_menu else {
        return;
    };
    let fresh = std::mem::take(&mut w.context_menu_fresh);
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        w.context_menu = None;
        return;
    }
    let all = match target {
        ContextTarget::Canvas => canvas_items(menu_ctx),
        ContextTarget::LayerRow => layer_items(menu_ctx),
        ContextTarget::DocumentTab => tab_items(menu_ctx),
    };
    let tokens = design::current_theme(ctx).tokens();
    let hover_fill = design::color32(tokens.palette.color(ColorRole::ControlFillHovered));
    let row_h = tokens.metrics.control_height;
    egui::Area::new(egui::Id::new("raster-context-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(150.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, item) in all.iter().enumerate() {
                    let enabled = item.resolution.is_enabled();
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::hover(),
                    );
                    // The interaction carries the deterministic id; the raw
                    // hover sense only tells egui the row exists for layout.
                    let response = ui.interact(rect, ids::context_item(i), egui::Sense::click());
                    if enabled && response.hovered() {
                        ui.painter()
                            .rect_filled(rect, egui::Rounding::ZERO, hover_fill);
                    }
                    let _tone = if enabled {
                        TextRole::Primary
                    } else {
                        TextRole::Tertiary
                    };
                    let font = design::egui_theme::text_style(TypeRole::Body).resolve(ui.style());
                    let color = design::color32(tokens.palette.text(if enabled {
                        TextRole::Primary
                    } else {
                        TextRole::Tertiary
                    }));
                    let pos = egui::pos2(
                        rect.left() + design::tokens::spacing::Space::Small.pt(),
                        rect.center().y - font.size * 0.5,
                    );
                    ui.painter().text(
                        pos,
                        egui::Align2::LEFT_TOP,
                        item.label.clone(),
                        font.clone(),
                        color,
                    );
                    if let Some(reason) = item.resolution.reason() {
                        let _ = response.clone().on_hover_text(reason);
                    }
                    if enabled && response.clicked() {
                        w.emit(crate::Intent::Action(item.action));
                        w.context_menu = None;
                    }
                }
            });
        });
    if fresh {
        return;
    }
    // Any click that is not on one of the menu's rows puts it away.
    if ctx.input(|i| i.pointer.any_click()) {
        let on_menu = all.iter().enumerate().any(|(i, _)| {
            ctx.read_response(ids::context_item(i))
                .is_some_and(|r| r.hovered())
        });
        if !on_menu {
            w.context_menu = None;
        }
    }
}

/// Stable ids for the menu's item buttons, so tests can click one by name.
pub mod ids {
    pub fn context_item(index: usize) -> egui::Id {
        egui::Id::new(("raster-context-item", index))
    }
}
