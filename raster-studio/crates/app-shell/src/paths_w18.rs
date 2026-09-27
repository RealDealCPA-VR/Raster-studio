//! W18-E: the path tools' gaps from the post-wave-16 parity audit.
//!
//! * **Delete** with Path Select deletes the selected path components, and
//!   with Direct Selection the selected knots, as ONE `SetLayerKind` step —
//!   Edit > Clear's path-tool branch ([`delete_selected_path_parts`]) —
//!   instead of clearing pixels (Photopea learn/vg-manipulation: "delete
//!   them by pressing Delete").
//! * The active shape layer's path outline is drawn on the canvas, with the
//!   Path Select components' knots filled and Direct Selection's knots
//!   (selected ones filled, with their handles) — [`paint_paths`], gated by
//!   the chrome on View > Show > Paths.
//! * The Pen options bar's Make Selection / Shape presses
//!   ([`tools::path_select::request_pen_make`]) are answered here with the
//!   Paths panel's current path ([`answer_pen_make`]).

use design::Space;
use editor_core::Command;
use layer_model::{Layer, LayerKind, ShapeLayer};
use tools::path_select::{self, ComponentOp, PenMake};
use tools::ToolId;
use ui::canvas::paths::{committed_overlay, PathShown};

use crate::chrome::ChromeOutput;
use crate::editor::Editor;

/// W18-E: Edit > Clear (the Delete key) with Path Select or Direct Selection
/// and a selection of path parts on the active shape layer: delete them as
/// ONE `SetLayerKind` step. `None` hands the key back to the pixel Clear
/// (another tool, or nothing selected); `Some(Err)` is a refusal (the shape
/// would be left empty — Layer > Delete removes a layer).
pub(crate) fn delete_selected_path_parts(editor: &mut Editor) -> Option<Result<String, String>> {
    let tool = editor.tool();
    if !matches!(tool, ToolId::PathSelect | ToolId::DirectSelection) {
        return None;
    }
    let parts = path_select::selected_path_parts();
    let doc = editor.active()?;
    let active = doc.document.active_layer()?;
    let layer = doc.document.layers.get(active)?;
    let LayerKind::Shape(shape) = &layer.kind else {
        return None;
    };
    let path = vector::svg::parse(&shape.path_svg).ok()?;
    let mut shape = shape.clone();
    let (next, message) = if tool == ToolId::PathSelect {
        let (_, picked) = parts.components.filter(|(l, _)| *l == active)?;
        let Some((next, _)) = path_select::apply_component_op(&path, &picked, ComponentOp::Delete)
        else {
            return Some(Err(
                "A shape keeps at least one path component; Layer > Delete removes the layer"
                    .into(),
            ));
        };
        (next, format!("Deleted {} path component(s)", picked.len()))
    } else {
        let (_, knots) = parts.knots.filter(|(l, _)| *l == active)?;
        let Some(next) = path_select::delete_knots(&path, &knots) else {
            return Some(Err(
                "A shape keeps at least one path component; Layer > Delete removes the layer"
                    .into(),
            ));
        };
        (next, format!("Deleted {} knot(s)", knots.len()))
    };
    shape.path_svg = vector::svg::to_svg(&next);
    editor.apply_command(Command::SetLayerKind {
        layer_id: active,
        kind: Box::new(LayerKind::Shape(shape)),
    });
    if tool == ToolId::PathSelect {
        path_select::clear_selected_components();
    } else {
        path_select::clear_selected_knots();
    }
    Some(Ok(message))
}

/// W18-E: a path on the canvas, Path Select's selected components (when it
/// is the tool) and Direct Selection's selected knots as (subpath, knot)
/// (when it is).
type ShownPath = (
    vector::Path,
    Option<Vec<usize>>,
    Option<Vec<(usize, usize)>>,
);

/// W18-E: the active shape layer's path, in document space, and which of it
/// the live path tool holds selected. `None` when the active layer carries
/// no path.
fn shown_path(editor: &Editor) -> Option<ShownPath> {
    let doc = editor.active()?;
    let active = doc.document.active_layer()?;
    let layer = doc.document.layers.get(active)?;
    let LayerKind::Shape(shape) = &layer.kind else {
        return None;
    };
    let path = vector::svg::parse(&shape.path_svg).ok()?;
    if path.is_empty() {
        return None;
    }
    let path = path.transform(&ui::panels::paths::affine_of(layer.transform));
    let parts = path_select::selected_path_parts();
    match editor.effective_tool() {
        ToolId::PathSelect => {
            let picked = parts
                .components
                .filter(|(l, _)| *l == active)
                .map(|(_, p)| p)
                .unwrap_or_default();
            Some((path, Some(picked), None))
        }
        ToolId::DirectSelection => {
            let knots = parts
                .knots
                .filter(|(l, _)| *l == active)
                .map(|(_, k)| k.iter().map(|r| (r.subpath, r.index)).collect())
                .unwrap_or_default();
            Some((path, None, Some(knots)))
        }
        _ => Some((path, None, None)),
    }
}

/// W18-E: paint the active shape layer's path outline over the canvas, and
/// the path tools' selection furniture: Path Select's selected components
/// with every knot filled, Direct Selection's knots (hollow, the selected
/// ones filled with their handles). The chrome calls this only while View >
/// Show > Paths shows.
pub(crate) fn paint_paths(ctx: &egui::Context, editor: &Editor) {
    use ui::canvas::{paint, style::CanvasStyle};
    let Some((path, components, knots)) = shown_path(editor) else {
        return;
    };
    let Some(doc) = editor.active() else {
        return;
    };
    let shown = match (&components, &knots) {
        (Some(picked), _) => PathShown::Components(picked),
        (_, Some(knots)) => PathShown::Knots(knots),
        _ => PathShown::Outline,
    };
    let camera = crate::tool_input::canvas_camera_of(&doc.camera);
    let viewport = crate::tool_input::canvas_viewport(&doc.camera);
    let overlay = committed_overlay(&path, shown, &camera, &viewport);
    let style = CanvasStyle::from_context(ctx);
    let mut painter = ctx.layer_painter(crate::canvas_extras::overlay_layer());
    painter.set_clip_rect(ctx.available_rect());
    for line in &overlay.outline {
        let points: Vec<egui::Pos2> = line.iter().map(|p| egui::pos2(p.x, p.y)).collect();
        painter.add(egui::Shape::line(points, style.hairline(style.path_stroke)));
    }
    let control = Space::XSmall.pt();
    paint::path(&painter, &overlay.furniture, control * 1.5, control, &style);
}

/// W18-E: answer a parked Pen bar Make press with the Paths panel's current
/// path (the selected path, else the Work Path — the path Layer > Vector
/// Mask > Current Path uses): Selection loads it as the selection, Shape
/// makes a shape layer of it filled with the foreground colour. The bar
/// greys both buttons while there is no such path.
pub(crate) fn answer_pen_make(editor: &Editor, workspace: &ui::Workspace, out: &mut ChromeOutput) {
    let Some(make) = path_select::take_pen_make() else {
        return;
    };
    let Some(doc) = editor.active() else {
        return;
    };
    let Some(path) = workspace
        .paths
        .selected_path(&doc.document)
        .or_else(|| workspace.paths.work_path.clone())
        .filter(|p| !p.is_empty())
    else {
        return;
    };
    match make {
        PenMake::Selection => {
            if let Some(command) = ui::panels::paths::load_as_selection(&doc.document, &path) {
                out.commands.push(command);
            }
        }
        PenMake::Shape => {
            let shape = ShapeLayer {
                path_svg: vector::svg::to_svg(&path),
                fill: Some(editor.foreground()),
                stroke: None,
                ..ShapeLayer::default()
            };
            out.commands.push(Command::create_layer(Layer::with_kind(
                "Shape",
                LayerKind::Shape(shape),
            )));
        }
    }
}

#[cfg(test)]
#[path = "paths_w18_tests.rs"]
mod tests;
