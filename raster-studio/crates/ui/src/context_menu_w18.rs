//! W18-A: the canvas right-click menu, built per tool as Photopea builds it.
//!
//! Photopea has no single canvas menu: each tool's right-click raises its own
//! list (its bundle's `YY` handlers). The rows here follow those lists, in
//! their order and with their separators:
//!
//! * **Selection tools** (marquees, lassos, Magic Wand, Quick Selection —
//!   Photopea's `dy.aJP(!0)`): Deselect, Inverse, Modify (the submenu of
//!   Border, Smooth, Expand, Contract, Feather), Refine Edge | Save Selection, Make Work Path |
//!   Layer Via Copy, Layer Via Cut, New Layer | Free Transform, Transform
//!   Selection | Fill, Stroke.
//! * **Move** (`X.pj`'s `YY`): the layers under the pointer, top first — a
//!   click selects that layer — then Share… and Remove BG. Both of those rows
//!   are drawn greyed with the reason: Share… is Photopea's online publishing
//!   (its `publishOnline` dialog, PSD format) and Remove BG its cloud
//!   background-removal service (`bgrm`), and this build has neither. Photopea
//!   lists no Cut, Copy or Paste here. Two Move cases are not built: Ctrl+right-
//!   click (Photopea selects the top layer without a list) and a right-click
//!   on the rulers (Photopea's ruler list).
//! * **During Free Transform** (`jD.ab$`, whichever tool holds the session):
//!   Again | Scale, Rotate, Skew, Distort, Perspective | Warp | Rotate 90° CW,
//!   Rotate 90° CCW, Rotate 180°, Flip Horizontal, Flip Vertical.
//! * **Pen and path tools** (`X.S$`, `lV.mE`): Remove Anchor Point, Remove
//!   Path | Make Selection, Fill, Stroke — on the path under the pointer. Fill
//!   applies at once with the foreground colour, as Photopea's does. Make
//!   Selection and Stroke differ: Photopea opens its Make Selection dialog
//!   (`makesel`: feather, anti-alias, operation) and its Stroke Path dialog
//!   (`strokepath`); this build has neither dialog, so Make Selection loads
//!   the path as a new selection and Stroke strokes it with the foreground
//!   colour at the Brush size, both at once.
//! * **Type** (`al` "showpan"): Edit (the text layer under the pointer) |
//!   Warp Text. Photopea's other Type list, shown while text is being edited
//!   (`B.aqQ`: Select All and the text-editing rows), is not built.
//! * **Zoom** (`X.aX`): Zoom In, Zoom Out | Fit on Screen, 100%.
//! * **Slice tools** (`X.oP`): Delete, Slice Options…, Divide… — the
//!   right-click picks the slice under it first, as Photopea's does.
//!
//! Every other tool keeps the general list ([`super::canvas_items`]):
//! Photopea shows its brush popover or nothing there.
//!
//! The application measures what the rows need at the right-click — the
//! layers under the pointer, the path, the slice — into a [`CanvasMenu`] and
//! opens it with [`open_canvas`]. Rows that are menu actions resolve against
//! the frame's [`MenuContext`] like every other menu row; the rest carry a
//! [`CanvasRow`] the drawer performs ([`perform`]).

use layer_model::LayerId;
use tools::ToolId;

use super::divide_slice::{self, DivideSliceDialog, SliceMenuRequest};
use super::{items, ContextTarget, MenuItem};
use crate::menu::{
    MenuAction, MenuContext, ModifySelection, Resolution, TransformOp, WarpTextItem, ZoomCommand,
};
use crate::strings::tr;
use crate::{Intent, Workspace};

/// What a row that is not a menu action does when clicked.
#[derive(Clone, Debug)]
pub enum CanvasRow {
    /// Emit these intents, in order.
    Emit(Vec<Intent>),
    /// The Paths panel's Work Path becomes this path (`None` drops it), as
    /// the Paths footer's buttons set it.
    WorkPath(Option<vector::Path>),
    /// Open Divide Slices.
    Divide(DivideSliceDialog),
    /// Park this request and emit Edit > Clear, whose arm in the
    /// application takes it (see [`divide_slice`]).
    Slice(SliceMenuRequest),
}

/// A row's payload, or why it cannot be used.
pub type Payload = Result<CanvasRow, &'static str>;

/// What the application measured at the right-click.
#[derive(Clone, Debug)]
pub struct CanvasMenu {
    /// The acting tool.
    pub tool: ToolId,
    /// A free transform is live (or the Free Transform tool is up).
    pub transforming: bool,
    /// The layers whose visible content is under the pointer, top first.
    pub layers_under: Vec<(LayerId, String)>,
    pub make_work_path: Payload,
    pub remove_anchor: Payload,
    pub remove_path: Payload,
    pub make_selection: Payload,
    pub fill_path: Payload,
    pub stroke_path: Payload,
    pub edit_text: Payload,
    pub delete_slice: Payload,
    pub divide_slice: Payload,
}

impl CanvasMenu {
    /// A menu for `tool` with nothing measured: every custom row greyed.
    pub fn bare(tool: ToolId) -> Self {
        let none = || Err(tr("ui.canvas_menu.unavailable"));
        Self {
            tool,
            transforming: false,
            layers_under: Vec::new(),
            make_work_path: none(),
            remove_anchor: none(),
            remove_path: none(),
            make_selection: none(),
            fill_path: none(),
            stroke_path: none(),
            edit_text: none(),
            delete_slice: none(),
            divide_slice: none(),
        }
    }
}

/// The per-tool families.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    Selection,
    Move,
    Transform,
    Pen,
    Type,
    Zoom,
    Slice,
    /// Every other tool: the general list.
    General,
}

/// Which list `menu` shows.
pub fn family(menu: &CanvasMenu) -> Family {
    use ToolId as T;
    if menu.transforming {
        return Family::Transform;
    }
    match menu.tool {
        T::RectMarquee
        | T::EllipseMarquee
        | T::SingleRowMarquee
        | T::SingleColumnMarquee
        | T::Lasso
        | T::PolygonalLasso
        | T::MagneticLasso
        | T::MagicWand
        | T::QuickSelect => Family::Selection,
        T::Move => Family::Move,
        T::FreeTransform => Family::Transform,
        T::Pen
        | T::FreeformPen
        | T::CurvaturePen
        | T::AddAnchor
        | T::DeleteAnchor
        | T::ConvertAnchor
        | T::PathSelect
        | T::DirectSelection => Family::Pen,
        T::Type | T::VerticalType => Family::Type,
        T::Zoom => Family::Zoom,
        T::Slice | T::SliceSelect => Family::Slice,
        _ => Family::General,
    }
}

/// A custom row.
fn custom(label: &str, placeholder: MenuAction, payload: &Payload) -> MenuItem {
    let (resolution, row) = match payload {
        Ok(row) => {
            // The resolution's intent is what a click emits for `Emit`; the
            // other kinds are performed by the drawer, and name the menu
            // action they stand nearest to.
            let intent = match row {
                CanvasRow::Emit(intents) => intents
                    .first()
                    .cloned()
                    .unwrap_or(Intent::Action(placeholder)),
                _ => Intent::Action(placeholder),
            };
            (Resolution::Enabled(intent), Some(row.clone()))
        }
        Err(reason) => (Resolution::Disabled(reason), None),
    };
    MenuItem {
        label: label.to_string(),
        action: placeholder,
        resolution,
        separator_after: false,
        request: None,
        w18: row,
    }
}

/// The Modify submenu row's label (the Select menu's own submenu name).
pub fn modify_label() -> &'static str {
    crate::strings::tr_en("Modify")
}

/// Rule a separator under the last row so far.
fn rule(rows: &mut [MenuItem]) {
    if let Some(last) = rows.last_mut() {
        last.separator_after = true;
    }
}

/// The rows `menu` shows, resolved against `ctx`.
pub fn rows(ctx: &MenuContext, menu: &CanvasMenu) -> Vec<MenuItem> {
    let mut rows: Vec<MenuItem> = Vec::new();
    match family(menu) {
        Family::Selection => {
            rows.extend(items(
                ctx,
                &[MenuAction::Deselect, MenuAction::InverseSelection],
            ));
            // Photopea's Modify submenu: one row wearing its first child's
            // action and gate; [`super::children_of`] opens the five beside it.
            let mut modify = items(ctx, &[MenuAction::Modify(ModifySelection::ALL[0])]);
            for row in &mut modify {
                row.label = modify_label().to_string();
            }
            rows.extend(modify);
            rows.extend(items(ctx, &[MenuAction::RefineEdge]));
            rule(&mut rows);
            rows.extend(items(ctx, &[MenuAction::SaveSelection]));
            rows.push(custom(
                tr("ui.canvas_menu.make_work_path"),
                MenuAction::SaveSelection,
                &menu.make_work_path,
            ));
            rule(&mut rows);
            rows.extend(items(
                ctx,
                &[
                    MenuAction::LayerViaCopy,
                    MenuAction::LayerViaCut,
                    MenuAction::NewLayer,
                ],
            ));
            rule(&mut rows);
            rows.extend(items(
                ctx,
                &[MenuAction::FreeTransform, MenuAction::TransformSelection],
            ));
            rule(&mut rows);
            rows.extend(items(
                ctx,
                &[MenuAction::FillDialog, MenuAction::StrokeDialog],
            ));
        }
        Family::Move => {
            for (id, name) in &menu.layers_under {
                rows.push(custom(
                    name,
                    MenuAction::SelectAllLayers,
                    &Ok(CanvasRow::Emit(vec![Intent::SelectLayers {
                        layers: vec![*id],
                        active: Some(*id),
                    }])),
                ));
            }
            rule(&mut rows);
            // Photopea's two rows after the layers; neither service exists
            // in this build, so both stay greyed with the reason.
            rows.push(custom(
                tr("ui.canvas_menu.share"),
                MenuAction::SaveAsPsd,
                &Err(tr("ui.canvas_menu.no_share")),
            ));
            rows.push(custom(
                tr("ui.canvas_menu.remove_bg"),
                MenuAction::SelectSubject,
                &Err(tr("ui.canvas_menu.no_remove_bg")),
            ));
        }
        Family::Transform => {
            use TransformOp as T;
            rows.extend(items(ctx, &[MenuAction::TransformAgain]));
            rule(&mut rows);
            rows.extend(items(
                ctx,
                &[
                    MenuAction::Transform(T::Scale),
                    MenuAction::Transform(T::Rotate),
                    MenuAction::Transform(T::Skew),
                    MenuAction::Transform(T::Distort),
                    MenuAction::Transform(T::Perspective),
                ],
            ));
            rule(&mut rows);
            rows.extend(items(ctx, &[MenuAction::Transform(T::Warp)]));
            rule(&mut rows);
            rows.extend(items(
                ctx,
                &[
                    MenuAction::Transform(T::Rotate90Cw),
                    MenuAction::Transform(T::Rotate90Ccw),
                    MenuAction::Transform(T::Rotate180),
                    MenuAction::Transform(T::FlipHorizontal),
                    MenuAction::Transform(T::FlipVertical),
                ],
            ));
        }
        Family::Pen => {
            rows.push(custom(
                tr("ui.canvas_menu.remove_anchor"),
                MenuAction::ClearPixels,
                &menu.remove_anchor,
            ));
            rows.push(custom(
                tr("ui.canvas_menu.remove_path"),
                MenuAction::ClearPixels,
                &menu.remove_path,
            ));
            rule(&mut rows);
            rows.push(custom(
                tr("ui.canvas_menu.make_selection"),
                MenuAction::LoadSelection,
                &menu.make_selection,
            ));
            rows.push(custom(
                tr("ui.canvas_menu.fill"),
                MenuAction::FillDialog,
                &menu.fill_path,
            ));
            rows.push(custom(
                tr("ui.canvas_menu.stroke"),
                MenuAction::StrokeDialog,
                &menu.stroke_path,
            ));
        }
        Family::Type => {
            rows.push(custom(
                tr("ui.canvas_menu.edit_text"),
                MenuAction::WarpText(WarpTextItem::Dialog),
                &menu.edit_text,
            ));
            rule(&mut rows);
            rows.extend(items(ctx, &[MenuAction::WarpText(WarpTextItem::Dialog)]));
        }
        Family::Zoom => {
            rows.extend(items(
                ctx,
                &[
                    MenuAction::Zoom(ZoomCommand::In),
                    MenuAction::Zoom(ZoomCommand::Out),
                ],
            ));
            rule(&mut rows);
            rows.extend(items(
                ctx,
                &[
                    MenuAction::Zoom(ZoomCommand::FitOnScreen),
                    MenuAction::Zoom(ZoomCommand::ActualPixels),
                ],
            ));
        }
        Family::Slice => {
            rows.push(custom(
                tr("ui.canvas_menu.delete_slice"),
                MenuAction::ClearPixels,
                &menu.delete_slice,
            ));
            rows.extend(items(ctx, &[MenuAction::SliceOptions]));
            rows.push(custom(
                tr("ui.canvas_menu.divide_slice"),
                MenuAction::SliceOptions,
                &menu.divide_slice,
            ));
        }
        Family::General => return super::canvas_items(ctx),
    }
    // A rule under the last row would draw into nothing.
    if let Some(last) = rows.last_mut() {
        last.separator_after = false;
    }
    rows
}

/// The egui memory slot the open canvas menu's measurements live in.
fn slot() -> egui::Id {
    egui::Id::new("raster-canvas-menu-w18")
}

/// Open the canvas menu `menu` at `pos`.
pub fn open_canvas(w: &mut Workspace, ctx: &egui::Context, menu: CanvasMenu, pos: egui::Pos2) {
    ctx.data_mut(|d| d.insert_temp(slot(), Some((pos, menu))));
    super::open(w, ContextTarget::Canvas, pos);
}

/// The measurements of the canvas menu open at `pos`, if it was opened by
/// [`open_canvas`] (the `ui` host's own canvas opens the general list).
pub fn open_menu(ctx: &egui::Context, pos: egui::Pos2) -> Option<CanvasMenu> {
    ctx.data(|d| d.get_temp::<Option<(egui::Pos2, CanvasMenu)>>(slot()))
        .flatten()
        .filter(|(at, _)| *at == pos)
        .map(|(_, menu)| menu)
}

/// The rows of the canvas menu open at `pos`.
pub fn canvas_rows(ctx: &egui::Context, menu_ctx: &MenuContext, pos: egui::Pos2) -> Vec<MenuItem> {
    match open_menu(ctx, pos) {
        Some(menu) => rows(menu_ctx, &menu),
        None => super::canvas_items(menu_ctx),
    }
}

/// Perform a clicked custom row.
pub fn perform(w: &mut Workspace, ctx: &egui::Context, row: CanvasRow) {
    match row {
        CanvasRow::Emit(intents) => {
            for intent in intents {
                w.emit(intent);
            }
        }
        CanvasRow::WorkPath(path) => {
            w.paths.work_selected = path.is_some();
            w.paths.work_path = path;
            w.paths.work_from_pen = false;
            if w.paths.work_selected {
                w.paths.selected = None;
            }
        }
        CanvasRow::Divide(dialog) => divide_slice::open(ctx, dialog),
        CanvasRow::Slice(request) => {
            divide_slice::park(request);
            w.emit(Intent::Action(MenuAction::ClearPixels));
        }
    }
    ctx.request_repaint();
}

/// Draw Divide Slices while it is open; its confirmation is parked for the
/// application and Edit > Clear emitted, as the slice rows do.
pub fn draw_divide_slice(w: &mut Workspace, ctx: &egui::Context) {
    if let Some(request) = divide_slice::draw(ctx) {
        perform(w, ctx, CanvasRow::Slice(request));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(rows: &[MenuItem]) -> Vec<String> {
        rows.iter().map(|r| r.label.clone()).collect()
    }

    #[test]
    fn each_tool_family_builds_its_own_rows_in_photopeas_order() {
        let ctx = MenuContext::default();
        let selection = rows(&ctx, &CanvasMenu::bare(ToolId::Lasso));
        assert_eq!(selection[0].action, MenuAction::Deselect);
        assert!(labels(&selection).contains(&tr("ui.canvas_menu.make_work_path").to_string()));
        assert!(selection
            .iter()
            .any(|r| r.action == MenuAction::LayerViaCut));
        // Modify is one submenu row whose children are Photopea's five.
        let modify = selection
            .iter()
            .find(|r| r.label == modify_label())
            .expect("a Modify row");
        assert_eq!(
            super::super::children_of(modify, &ctx)
                .iter()
                .map(|r| r.action)
                .collect::<Vec<_>>(),
            ModifySelection::ALL
                .iter()
                .map(|m| MenuAction::Modify(*m))
                .collect::<Vec<_>>()
        );
        assert!(!selection
            .iter()
            .any(|r| r.action == MenuAction::Modify(ModifySelection::Feather)));
        let zoom = rows(&ctx, &CanvasMenu::bare(ToolId::Zoom));
        assert_eq!(
            zoom.iter().map(|r| r.action).collect::<Vec<_>>(),
            vec![
                MenuAction::Zoom(ZoomCommand::In),
                MenuAction::Zoom(ZoomCommand::Out),
                MenuAction::Zoom(ZoomCommand::FitOnScreen),
                MenuAction::Zoom(ZoomCommand::ActualPixels),
            ]
        );
        assert!(zoom[1].separator_after && !zoom[3].separator_after);
        let mut transform = CanvasMenu::bare(ToolId::Brush);
        transform.transforming = true;
        let transform = rows(&ctx, &transform);
        assert_eq!(transform.len(), 12);
        assert_eq!(transform[0].action, MenuAction::TransformAgain);
        assert_eq!(
            transform[11].action,
            MenuAction::Transform(TransformOp::FlipVertical)
        );
        // Move: the layers under the pointer, then Photopea's Share… and
        // Remove BG (greyed here), and no clipboard rows.
        let mut moving = CanvasMenu::bare(ToolId::Move);
        moving.layers_under = vec![
            (LayerId::new(), "Top".into()),
            (LayerId::new(), "Bottom".into()),
        ];
        let moving = rows(&ctx, &moving);
        assert_eq!(
            labels(&moving),
            vec![
                "Top".to_string(),
                "Bottom".to_string(),
                tr("ui.canvas_menu.share").to_string(),
                tr("ui.canvas_menu.remove_bg").to_string(),
            ]
        );
        assert!(moving[1].separator_after);
        assert!(matches!(moving[0].resolution, Resolution::Enabled(_)));
        assert!(matches!(moving[2].resolution, Resolution::Disabled(_)));
        assert!(matches!(moving[3].resolution, Resolution::Disabled(_)));
        assert!(!moving.iter().any(|r| matches!(
            r.action,
            MenuAction::Cut | MenuAction::Copy | MenuAction::Paste
        )));
        // A tool Photopea gives no list keeps the general one.
        let brush = rows(&ctx, &CanvasMenu::bare(ToolId::Brush));
        assert_eq!(labels(&brush), labels(&super::super::canvas_items(&ctx)));
    }
}
