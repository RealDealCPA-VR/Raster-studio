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

// W18-A: the canvas menu each tool builds, and Divide Slices.
#[path = "dialogs/divide_slice.rs"]
pub mod divide_slice;
#[path = "context_menu_w18.rs"]
pub mod w18;

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
    /// W16-D: a rule is drawn under this row (Photopea's separators).
    pub separator_after: bool,
    /// W16-D: a row with no menu action of its own (Photopea's dialog-free
    /// Duplicate Layer): a click asks the application through the Layers
    /// panel instead of emitting `action`, whose gate it shares.
    pub request: Option<crate::panels::layers::w16::LayersRequest>,
    /// W18-A: a canvas-menu row with no menu action of its own (a layer
    /// under the pointer, Make Work Path, a path or slice row): a click
    /// performs this ([`w18::perform`]) instead of emitting `action`.
    pub w18: Option<w18::CanvasRow>,
}

/// W18-G: the rows a submenu row opens beside it (Photopea's row-menu
/// submenus): a smart object's Stack Mode row opens the eleven stack modes.
/// Empty for an ordinary row. A submenu row wears its first child's action
/// and gate, so it greys out, with the same reason, exactly when they do.
pub fn children_of(item: &MenuItem, ctx: &MenuContext) -> Vec<MenuItem> {
    use crate::menu::{LayerExtraOp, StackMode};
    match item.action {
        MenuAction::LayerExtra(LayerExtraOp::StackMode(_))
            if item.label == LAYER_ROW_STACK_MODE && item.request.is_none() =>
        {
            let modes: Vec<MenuAction> = StackMode::ALL
                .iter()
                .map(|m| MenuAction::LayerExtra(LayerExtraOp::StackMode(*m)))
                .collect();
            items(ctx, &modes)
        }
        // W18-A: the selection tools' Modify row opens Photopea's five.
        MenuAction::Modify(_)
            if item.label == w18::modify_label()
                && item.request.is_none()
                && item.w18.is_none() =>
        {
            let modify: Vec<MenuAction> = crate::menu::ModifySelection::ALL
                .iter()
                .map(|m| MenuAction::Modify(*m))
                .collect();
            items(ctx, &modify)
        }
        _ => Vec::new(),
    }
}

fn items(ctx: &MenuContext, actions: &[MenuAction]) -> Vec<MenuItem> {
    actions
        .iter()
        .map(|action| MenuItem {
            // W11-E: the frame's own wording, so Merge Down reads Merge
            // Layers over a multi-selection, as on the menu bar.
            label: action.label_in(ctx),
            action: *action,
            resolution: action.resolve(ctx),
            separator_after: false,
            request: None,
            w18: None,
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

/// The layer-row menu (W16-D): Photopea's Layers-panel row menu, its rows
/// and its order and separators — Blending Options, Select Pixels | Duplicate
/// Layer, Duplicate Into…, Delete | Convert to Smart Object, (on a smart
/// object, its rows), Rasterize, Rasterize Layer Style, Convert to Shape |
/// (on a text layer, the point / paragraph conversion) | Clipping Mask | the
/// Layer Style clipboard (Copy, Paste, Clear), Merge Down (Merge Layers over a
/// multi-selection), Flatten Image | the colour labels.
///
/// Photopea's submenus (Smart Object, Layer Style, Color) are flat here, the
/// way the colours already were; the smart-object rows appear only when the
/// active layer is a smart object. Clipping shows the one row that applies
/// (Release on a clipped layer, Create otherwise). Duplicate Layer copies at
/// once, as Photopea's does; Duplicate Into… is the dialog with the
/// destination document.
pub fn layer_items(ctx: &MenuContext) -> Vec<MenuItem> {
    use crate::menu::{LayerClass, LayerExtraOp, RasterizeTarget, SmartObjectOp};
    let clipped = ctx.active.is_some_and(|l| l.is_clipping);
    let class = ctx.active.map(|l| l.class);
    let mut rows: Vec<MenuItem> = Vec::new();
    let group = |rows: &mut Vec<MenuItem>, actions: &[MenuAction]| {
        let mut part = items(ctx, actions);
        if let Some(last) = part.last_mut() {
            last.separator_after = true;
        }
        rows.extend(part);
    };
    group(
        &mut rows,
        &[
            MenuAction::BlendingOptions,
            // W9-A: Photopea's "Select Pixels" — the active layer's
            // transparency as a new selection.
            MenuAction::SelectLayerPixels {
                layer: None,
                mask: false,
                op: crate::dialogs::LoadOperation::New,
            },
        ],
    );
    group(
        &mut rows,
        &[
            MenuAction::DuplicateLayer,
            MenuAction::DuplicateLayer,
            MenuAction::DeleteLayer,
        ],
    );
    let mut middle = vec![MenuAction::ConvertToSmartObject];
    if class == Some(LayerClass::SmartObject) {
        middle.extend([
            MenuAction::SmartObject(SmartObjectOp::NewViaCopy),
            MenuAction::EditSmartObjectContents,
            MenuAction::LayerExtra(LayerExtraOp::ResetTransform),
            MenuAction::ReplaceContents,
            MenuAction::SmartObject(SmartObjectOp::ExportContents),
            MenuAction::SmartObject(SmartObjectOp::ConvertToLayers),
        ]);
    }
    middle.extend([
        MenuAction::Rasterize(RasterizeTarget::Layer),
        MenuAction::Rasterize(RasterizeTarget::LayerStyle),
        MenuAction::ConvertTextToShape,
    ]);
    // W18-G: the rest of Photopea's Smart Object rows — its Stack Mode
    // submenu ([`children_of`]) and Turn into JPG — close the group on a
    // smart object.
    if class == Some(LayerClass::SmartObject) {
        middle.extend([
            MenuAction::LayerExtra(LayerExtraOp::StackMode(crate::menu::StackMode::ALL[0])),
            MenuAction::TurnIntoJpg,
        ]);
    }
    group(&mut rows, &middle);
    if class == Some(LayerClass::Text) {
        group(
            &mut rows,
            &[
                MenuAction::ConvertToPointText,
                MenuAction::ConvertToParagraphText,
            ],
        );
    }
    // Photopea rules the clipping row off on its own.
    group(
        &mut rows,
        &[if clipped {
            MenuAction::ReleaseClippingMask
        } else {
            MenuAction::CreateClippingMask
        }],
    );
    group(
        &mut rows,
        &[
            MenuAction::CopyLayerStyle,
            MenuAction::PasteLayerStyle,
            MenuAction::ClearLayerStyle,
        ],
    );
    // Photopea's Layer Style submenu carries the separator-after flag
    // (hs.aep FH:!0), so a rule sits between it and Merge.
    group(
        &mut rows,
        &[MenuAction::MergeDown, MenuAction::FlattenImage],
    );
    // W11-E: the colour labels.
    let colors: Vec<MenuAction> = layer_model::ColorLabel::ALL
        .iter()
        .copied()
        .map(MenuAction::SetLayerColor)
        .collect();
    rows.extend(items(ctx, &colors));
    let mut duplicates = 0;
    for row in &mut rows {
        match row.action {
            MenuAction::DuplicateLayer => {
                // The first is Photopea's Duplicate Layer (no dialog); the
                // second its Duplicate Into…, the dialog with the destination.
                if duplicates == 0 {
                    row.label = LAYER_ROW_DUPLICATE.to_string();
                    row.request = Some(crate::panels::layers::w16::LayersRequest::DuplicateLayer);
                } else {
                    row.label = LAYER_ROW_DUPLICATE_INTO.to_string();
                }
                duplicates += 1;
            }
            MenuAction::DeleteLayer => row.label = LAYER_ROW_DELETE.to_string(),
            MenuAction::Rasterize(RasterizeTarget::Layer) => {
                row.label = LAYER_ROW_RASTERIZE.to_string();
            }
            MenuAction::Rasterize(RasterizeTarget::LayerStyle) => {
                row.label = LAYER_ROW_RASTERIZE_STYLE.to_string();
            }
            MenuAction::EditSmartObjectContents => {
                row.label = LAYER_ROW_EDIT_CONTENTS.to_string();
            }
            MenuAction::SmartObject(SmartObjectOp::NewViaCopy) => {
                row.label = LAYER_ROW_SO_VIA_COPY.to_string();
            }
            // W18-G: the submenu row.
            MenuAction::LayerExtra(LayerExtraOp::StackMode(_)) => {
                row.label = LAYER_ROW_STACK_MODE.to_string();
            }
            _ => {}
        }
    }
    rows
}

/// The layer-row menu's own wording (Photopea's) for the rows whose
/// menu-bar label leans on a submenu's name or asks through a dialog.
const LAYER_ROW_DUPLICATE: &str = "Duplicate Layer";
const LAYER_ROW_DUPLICATE_INTO: &str = "Duplicate Into…";
const LAYER_ROW_DELETE: &str = "Delete";
const LAYER_ROW_RASTERIZE: &str = "Rasterize";
const LAYER_ROW_RASTERIZE_STYLE: &str = "Rasterize Layer Style";
const LAYER_ROW_EDIT_CONTENTS: &str = "Open (Edit Contents)";
const LAYER_ROW_SO_VIA_COPY: &str = "New Smart Obj. via Copy";
const LAYER_ROW_STACK_MODE: &str = "Stack Mode";

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
    // W18-A: Divide Slices floats over the canvas while it is open.
    w18::draw_divide_slice(w, ctx);
    // W18-G: File > Save PSD/PSB's options while open, and the row that
    // writes an Export As job cut per artboard / per slice.
    crate::dialogs::export_as::draw_w18g(w, ctx);
    let Some((target, pos)) = w.context_menu else {
        return;
    };
    let fresh = std::mem::take(&mut w.context_menu_fresh);
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        w.context_menu = None;
        return;
    }
    let all = match target {
        // W18-A: the list the acting tool builds.
        ContextTarget::Canvas => w18::canvas_rows(ctx, menu_ctx, pos),
        ContextTarget::LayerRow => layer_items(menu_ctx),
        ContextTarget::DocumentTab => tab_items(menu_ctx),
    };
    let tokens = design::current_theme(ctx).tokens();
    let hover_fill = design::color32(tokens.palette.color(ColorRole::ControlFillHovered));
    let row_h = tokens.metrics.control_height;
    // W18-G: the submenu row whose rows are open beside it, if any.
    // W18-A: read back as the `Option<usize>` it is stored as (a bare
    // `usize` read never matched, so the submenu closed as the pointer left
    // its row for it).
    let mut open_sub: Option<usize> = ctx
        .data(|d| d.get_temp::<Option<usize>>(ids::open_submenu()))
        .flatten();
    let mut sub_anchor: Option<(Vec<MenuItem>, egui::Rect)> = None;
    // W18-A: kept on screen here, not by egui's constraint, so the menu
    // does not move between its first frames (a click aimed at a row laid
    // out on one frame would land beside it on the next).
    // The area is also told its size up front: egui lays a new area out
    // once at a default size (600 wide) and constrains that to the screen,
    // which put the first frame's rows somewhere else near the right edge.
    let menu_size = menu_outer_size(ctx, &all, row_h);
    let menu_at = {
        let screen = ctx.screen_rect();
        egui::pos2(
            pos.x.min(screen.right() - menu_size.x).max(screen.left()),
            pos.y,
        )
    };
    egui::Area::new(egui::Id::new("raster-context-menu"))
        .order(egui::Order::Foreground)
        .default_size(menu_size)
        .fixed_pos(menu_at)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                // W18-A: as wide as its widest row, so a submenu opens
                // beside it rather than over it (the rows used to run to
                // the screen's edge).
                let width = menu_width(ui.ctx(), &all, row_h);
                ui.set_min_width(width);
                ui.set_max_width(width);
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
                    // W18-G: a submenu row opens its rows beside it on hover
                    // or click, and emits nothing itself.
                    let children = children_of(item, menu_ctx);
                    if !children.is_empty() {
                        let side = egui::Rect::from_min_max(
                            egui::pos2(rect.right() - row_h, rect.top()),
                            rect.right_bottom(),
                        );
                        crate::icons::paint_ui_icon(
                            ui,
                            side,
                            "chevron-right",
                            if enabled {
                                TextRole::Primary
                            } else {
                                TextRole::Tertiary
                            },
                        );
                        if enabled && (response.hovered() || response.clicked()) {
                            open_sub = Some(i);
                        }
                        if open_sub == Some(i) && enabled {
                            sub_anchor = Some((children, rect));
                        }
                    } else if response.hovered() {
                        open_sub = None;
                    }
                    let is_submenu =
                        sub_anchor.as_ref().is_some_and(|(_, r)| *r == rect) || open_sub == Some(i);
                    if enabled && response.clicked() && !is_submenu {
                        match item.request {
                            // W18-A: a canvas row the drawer performs.
                            _ if item.w18.is_some() => {
                                if let Some(row) = item.w18.clone() {
                                    w18::perform(w, ui.ctx(), row);
                                }
                            }
                            // W16-D: a row the application answers through
                            // the Layers panel's request queue.
                            Some(request) => {
                                w.layers.request(request);
                                ui.ctx().request_repaint();
                            }
                            None => w.emit(crate::Intent::Action(item.action)),
                        }
                        w.context_menu = None;
                    }
                    if item.separator_after {
                        let (rule, _) = ui.allocate_exact_size(
                            egui::vec2(
                                ui.available_width(),
                                design::tokens::spacing::Space::XSmall.pt(),
                            ),
                            egui::Sense::hover(),
                        );
                        ui.painter().hline(
                            rule.x_range(),
                            rule.center().y,
                            egui::Stroke::new(
                                tokens.borders.hairline,
                                design::color32(tokens.palette.color(ColorRole::SeparatorHairline)),
                            ),
                        );
                    }
                }
            });
        });
    // W18-G: the open submenu's rows, beside its row: to its right, or, as
    // Photopea flips it, to its left when the right would run off the
    // screen.
    let mut sub_rows = 0usize;
    if let Some((children, anchor)) = sub_anchor {
        sub_rows = children.len();
        let style = ctx.style();
        let font = design::egui_theme::text_style(TypeRole::Body).resolve(&style);
        let widest = children
            .iter()
            .map(|r| {
                ctx.fonts(|f| {
                    f.layout_no_wrap(r.label.clone(), font.clone(), egui::Color32::PLACEHOLDER)
                        .size()
                        .x
                })
            })
            .fold(0.0_f32, f32::max);
        let frame = egui::Frame::popup(&style);
        let outer = (widest + 2.0 * design::tokens::spacing::Space::Small.pt() + row_h).max(150.0)
            + frame.inner_margin.sum().x
            + frame.outer_margin.sum().x
            + 2.0 * frame.stroke.width;
        let at = if anchor.right() + outer <= ctx.screen_rect().right() {
            anchor.right_top()
        } else {
            egui::pos2(anchor.left() - outer, anchor.top())
        };
        egui::Area::new(egui::Id::new("raster-context-submenu"))
            .order(egui::Order::Foreground)
            // W18-A: laid out at its own size from its first frame.
            .default_size(menu_outer_size(ctx, &children, row_h))
            .fixed_pos(at)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    let width = menu_width(ui.ctx(), &children, row_h);
                    ui.set_min_width(width);
                    ui.set_max_width(width);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (k, child) in children.iter().enumerate() {
                        let enabled = child.resolution.is_enabled();
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), row_h),
                            egui::Sense::hover(),
                        );
                        let response =
                            ui.interact(rect, ids::context_subitem(k), egui::Sense::click());
                        if enabled && response.hovered() {
                            ui.painter()
                                .rect_filled(rect, egui::Rounding::ZERO, hover_fill);
                        }
                        let font =
                            design::egui_theme::text_style(TypeRole::Body).resolve(ui.style());
                        let color = design::color32(tokens.palette.text(if enabled {
                            TextRole::Primary
                        } else {
                            TextRole::Tertiary
                        }));
                        ui.painter().text(
                            egui::pos2(
                                rect.left() + design::tokens::spacing::Space::Small.pt(),
                                rect.center().y - font.size * 0.5,
                            ),
                            egui::Align2::LEFT_TOP,
                            child.label.clone(),
                            font,
                            color,
                        );
                        if let Some(reason) = child.resolution.reason() {
                            let _ = response.clone().on_hover_text(reason);
                        }
                        if enabled && response.clicked() {
                            w.emit(crate::Intent::Action(child.action));
                            w.context_menu = None;
                            open_sub = None;
                        }
                    }
                });
            });
    }
    ctx.data_mut(|d| d.insert_temp(ids::open_submenu(), open_sub));
    if fresh {
        return;
    }
    // Any click that is not on one of the menu's rows puts it away.
    if ctx.input(|i| i.pointer.any_click()) {
        let on_menu = all.iter().enumerate().any(|(i, _)| {
            ctx.read_response(ids::context_item(i))
                .is_some_and(|r| r.hovered())
        }) || (0..sub_rows).any(|k| {
            ctx.read_response(ids::context_subitem(k))
                .is_some_and(|r| r.hovered())
        });
        if !on_menu {
            w.context_menu = None;
        }
    }
    if w.context_menu.is_none() {
        ctx.data_mut(|d| d.remove::<Option<usize>>(ids::open_submenu()));
    }
}

/// W18-A: a menu's width: its widest label, with the leading inset and room
/// for a submenu chevron, and never under the old minimum.
fn menu_width(ctx: &egui::Context, rows: &[MenuItem], row_h: f32) -> f32 {
    let font = design::egui_theme::text_style(TypeRole::Body).resolve(&ctx.style());
    let widest = rows
        .iter()
        .map(|r| {
            ctx.fonts(|f| {
                f.layout_no_wrap(r.label.clone(), font.clone(), egui::Color32::PLACEHOLDER)
                    .size()
                    .x
            })
        })
        .fold(0.0_f32, f32::max);
    (widest + 2.0 * design::tokens::spacing::Space::Small.pt() + row_h).max(150.0)
}

/// W18-A: the whole menu's size on screen: [`menu_width`] by its rows and
/// rules, and the popup frame around them.
fn menu_outer_size(ctx: &egui::Context, rows: &[MenuItem], row_h: f32) -> egui::Vec2 {
    let frame = egui::Frame::popup(&ctx.style());
    let rules = rows.iter().filter(|r| r.separator_after).count() as f32;
    let inner = egui::vec2(
        menu_width(ctx, rows, row_h),
        rows.len() as f32 * row_h + rules * design::tokens::spacing::Space::XSmall.pt(),
    );
    inner
        + frame.inner_margin.sum()
        + frame.outer_margin.sum()
        + egui::Vec2::splat(2.0 * frame.stroke.width)
}

/// Stable ids for the menu's item buttons, so tests can click one by name.
pub mod ids {
    pub fn context_item(index: usize) -> egui::Id {
        egui::Id::new(("raster-context-item", index))
    }

    /// W18-G: row `index` of the open submenu (Photopea's Stack Mode).
    pub fn context_subitem(index: usize) -> egui::Id {
        egui::Id::new(("raster-context-subitem", index))
    }

    /// W18-G: where the open submenu row's index is kept between frames.
    pub fn open_submenu() -> egui::Id {
        egui::Id::new("raster-context-open-submenu")
    }
}
