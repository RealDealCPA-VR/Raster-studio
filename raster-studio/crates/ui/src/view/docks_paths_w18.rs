//! W18-E: the Paths panel rows' thumbnail, Ctrl+click to load a path as the
//! selection and double-click to rename (Photopea learn/vector-graphics:
//! "You can hold Ctrl and click on the thumbnail of the path, to turn it into
//! a selction"; "select paths, delete paths, create new, rename them").
//!
//! A child of `docks.rs`, drawn from its `paths_body`.

use design::{color32, egui_theme::rounding, ColorRole, Radius, Space};
use editor_core::{Command, Document};
use egui::{Sense, Ui, Vec2};
use layer_model::{LayerId, LayerKind};

use super::current_tokens;

/// W18-E: the egui id of a path row's thumbnail well.
pub(crate) fn path_thumb_id(layer: LayerId) -> egui::Id {
    egui::Id::new(("raster-paths-thumb", layer))
}

/// W18-E: the egui id of a path row's rename field.
pub(crate) fn path_rename_id(layer: LayerId) -> egui::Id {
    egui::Id::new(("raster-paths-rename", layer))
}

/// The egui-memory key the row being renamed (and its draft) lives under.
fn rename_key() -> egui::Id {
    egui::Id::new("raster-paths-renaming")
}

/// W18-E: the row being renamed and its draft name, if any.
pub(crate) fn renaming(ui: &Ui) -> Option<(LayerId, String)> {
    ui.ctx()
        .data(|d| d.get_temp::<Option<(LayerId, String)>>(rename_key()))
        .flatten()
}

/// W18-E: start renaming `layer`, its field seeded with `name`.
pub(crate) fn begin_rename(ui: &Ui, layer: LayerId, name: &str) {
    ui.ctx().data_mut(|d| {
        d.insert_temp(rename_key(), Some((layer, name.to_owned())));
    });
    ui.ctx()
        .memory_mut(|m| m.request_focus(path_rename_id(layer)));
}

fn end_rename(ui: &Ui) {
    ui.ctx()
        .data_mut(|d| d.insert_temp::<Option<(LayerId, String)>>(rename_key(), None));
}

/// W18-E: the rename field of the row being renamed, on the chrome's shared
/// [`crate::view::text_field_sized`] (seeded with `name`, the draft kept in egui
/// memory). Enter (or clicking away) answers the rename command, Escape
/// cancels; an all-whitespace name is refused (the row keeps its name).
/// `None` while the field is open.
pub(crate) fn rename_field(ui: &mut Ui, layer: LayerId, name: String) -> Option<Command> {
    let width = ui.available_width() - Space::Large.pt();
    let edit = crate::view::text_field_sized(ui, path_rename_id(layer), &name, width);
    let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if cancel || edit.response.lost_focus() {
        end_rename(ui);
    }
    edit.committed
        .filter(|_| !cancel)
        .and_then(|draft| crate::panels::layers::LayersModel::rename(layer, &draft))
}

/// W18-E: the path of shape layer `layer`, in document space.
pub(crate) fn layer_path(doc: &Document, layer: LayerId) -> Option<vector::Path> {
    let l = doc.layers.get(layer)?;
    let LayerKind::Shape(shape) = &l.kind else {
        return None;
    };
    let path = vector::parse_svg(&shape.path_svg).ok()?;
    Some(path.transform(&crate::panels::paths::affine_of(l.transform)))
}

/// W18-E: Ctrl+click on a path row: the path loaded as the selection.
pub(crate) fn load_command(doc: &Document, layer: LayerId) -> Option<Command> {
    let path = layer_path(doc, layer)?;
    crate::panels::paths::load_as_selection(doc, &path)
}

/// How finely a thumbnail outline is flattened, in document pixels.
const THUMB_TOLERANCE: f64 = 0.5;

/// W18-E: a path row's thumbnail: the canvas, fitted into a well the row's
/// height, with the path's outline drawn where it sits on the canvas.
pub(crate) fn thumbnail(ui: &mut Ui, doc: &Document, layer: LayerId) -> egui::Response {
    let t = current_tokens(ui);
    let height = t.metrics.list_row_height - Space::XSmall.pt();
    let (cw, ch) = (doc.width().max(1) as f32, doc.height().max(1) as f32);
    let size = if cw >= ch {
        Vec2::new(height * 4.0 / 3.0, height)
    } else {
        Vec2::new(height, height)
    };
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let response = ui.interact(rect, path_thumb_id(layer), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let radius = Radius::Small.resolve(&t.radii, size.y);
    ui.painter().rect_filled(
        rect,
        rounding(radius),
        color32(t.palette.color(ColorRole::SurfaceSunken)),
    );
    // The canvas, fitted and centred in the well.
    let scale = (rect.width() / cw).min(rect.height() / ch);
    let origin = rect.center() - Vec2::new(cw, ch) * scale * 0.5;
    let canvas = egui::Rect::from_min_size(origin, Vec2::new(cw, ch) * scale);
    ui.painter().rect_filled(
        canvas,
        egui::Rounding::ZERO,
        color32(t.palette.color(ColorRole::SurfacePanel)),
    );
    if let Some(path) = layer_path(doc, layer) {
        let stroke = egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::TextPrimary)),
        );
        for line in path.flatten(THUMB_TOLERANCE) {
            let mut points: Vec<egui::Pos2> = line
                .points
                .iter()
                .map(|p| origin + Vec2::new(p.x as f32, p.y as f32) * scale)
                .collect();
            if line.closed {
                if let Some(first) = points.first().copied() {
                    points.push(first);
                }
            }
            if points.len() > 1 && points.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
                ui.painter().add(egui::Shape::line(points, stroke));
            }
        }
    }
    ui.painter().rect_stroke(
        rect,
        rounding(radius),
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::ControlStroke)),
        ),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::Intent;
    use crate::Workspace;

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(360.0, 700.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn doc_with_path() -> (Document, LayerId) {
        let mut doc = Document::new(32, 32, "paths");
        let layer = layer_model::Layer::with_kind(
            "Square",
            LayerKind::Shape(layer_model::ShapeLayer::from_svg(
                "M4 4 L12 4 L12 12 L4 12 Z",
            )),
        );
        let id = doc.layers.push_root(layer).unwrap();
        (doc, id)
    }

    fn run(
        ctx: &egui::Context,
        w: &mut Workspace,
        doc: &Document,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        run_held(ctx, w, doc, events, egui::Modifiers::NONE)
    }

    /// A frame with `held` down, as the platform reports the keyboard's
    /// modifiers in the frame's input (`RawInput::modifiers`).
    fn run_held(
        ctx: &egui::Context,
        w: &mut Workspace,
        doc: &Document,
        events: Vec<egui::Event>,
        held: egui::Modifiers,
    ) -> egui::FullOutput {
        let mut raw = input(events);
        raw.modifiers = held;
        ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| super::super::paths_body(w, ui, doc));
        })
    }

    fn press(at: egui::Pos2, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        }
    }

    fn click(
        ctx: &egui::Context,
        w: &mut Workspace,
        doc: &Document,
        at: egui::Pos2,
        modifiers: egui::Modifiers,
    ) {
        let _ = run_held(
            ctx,
            w,
            doc,
            vec![egui::Event::PointerMoved(at), press(at, true, modifiers)],
            modifiers,
        );
        let _ = run_held(ctx, w, doc, vec![press(at, false, modifiers)], modifiers);
    }

    fn thumb_rect(ctx: &egui::Context, layer: LayerId) -> egui::Rect {
        ctx.read_response(path_thumb_id(layer))
            .expect("the thumbnail was drawn")
            .rect
    }

    #[test]
    fn a_path_row_draws_a_thumbnail_of_its_outline() {
        let (doc, id) = doc_with_path();
        let mut w = Workspace::new();
        let ctx = egui::Context::default();
        let _ = run(&ctx, &mut w, &doc, Vec::new());
        let out = run(&ctx, &mut w, &doc, Vec::new());
        let rect = thumb_rect(&ctx, id);
        // The square's corner (4, 4) on the 32x32 canvas, fitted into the
        // well: a quarter of the way along the canvas box.
        let lines: Vec<Vec<egui::Pos2>> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Path(p) if rect.expand(1.0).contains(p.points[0]) => {
                    Some(p.points.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 1, "one outline in the well: {lines:?}");
        assert_eq!(lines[0].len(), 5, "a closed square");
        let (min, max) = lines[0].iter().fold(
            (
                egui::pos2(f32::MAX, f32::MAX),
                egui::pos2(f32::MIN, f32::MIN),
            ),
            |(lo, hi), p| (lo.min(*p), hi.max(*p)),
        );
        let side = rect.height();
        let canvas_left = rect.center().x - side * 0.5;
        assert!(
            (min.x - (canvas_left + side / 8.0)).abs() < 0.5,
            "{min:?} {rect:?}"
        );
        assert!((max.x - (canvas_left + side * 12.0 / 32.0)).abs() < 0.5);
        assert!((max.y - min.y - side / 4.0).abs() < 0.5);
    }

    #[test]
    fn ctrl_click_on_a_path_row_loads_it_as_the_selection() {
        let (mut doc, id) = doc_with_path();
        let mut w = Workspace::new();
        let ctx = egui::Context::default();
        let _ = run(&ctx, &mut w, &doc, Vec::new());
        let _ = run(&ctx, &mut w, &doc, Vec::new());
        let _ = w.drain_intents();
        click(
            &ctx,
            &mut w,
            &doc,
            thumb_rect(&ctx, id).center(),
            egui::Modifiers::COMMAND,
        );
        let intents = w.drain_intents();
        let command = match intents.as_slice() {
            [Intent::Document(c @ Command::SetSelection { .. })] => c.clone(),
            other => panic!("Ctrl+click emitted {other:?}"),
        };
        assert_eq!(w.paths.selected, None, "Ctrl+click does not select the row");
        command.apply(&mut doc).unwrap();
        assert_eq!(
            doc.selection.bounds(),
            Some((glam::IVec2::new(4, 4), glam::IVec2::new(12, 12)))
        );
        // A plain click selects the row and emits no selection.
        click(
            &ctx,
            &mut w,
            &doc,
            thumb_rect(&ctx, id).center(),
            egui::Modifiers::NONE,
        );
        assert_eq!(w.paths.selected, Some(id));
        assert!(!w
            .drain_intents()
            .iter()
            .any(|i| matches!(i, Intent::Document(Command::SetSelection { .. }))));
    }

    #[test]
    fn double_clicking_a_path_row_renames_it() {
        let (mut doc, id) = doc_with_path();
        let mut w = Workspace::new();
        let ctx = egui::Context::default();
        let _ = run(&ctx, &mut w, &doc, Vec::new());
        let _ = run(&ctx, &mut w, &doc, Vec::new());
        let at = thumb_rect(&ctx, id).center() + egui::vec2(60.0, 0.0);
        click(&ctx, &mut w, &doc, at, egui::Modifiers::NONE);
        click(&ctx, &mut w, &doc, at, egui::Modifiers::NONE);
        let _ = run(&ctx, &mut w, &doc, Vec::new());
        let _ = w.drain_intents();
        assert!(
            ctx.read_response(path_rename_id(id)).is_some(),
            "the double-click opened no rename field"
        );
        // Select the draft, type the new name, Enter.
        let _ = run(
            &ctx,
            &mut w,
            &doc,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                },
                egui::Event::Text("Outline".into()),
            ],
        );
        let _ = run(
            &ctx,
            &mut w,
            &doc,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let intents = w.drain_intents();
        let command = intents
            .iter()
            .find_map(|i| match i {
                Intent::Document(c @ Command::SetLayerProperties { .. }) => Some(c.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no rename: {intents:?}"));
        command.apply(&mut doc).unwrap();
        assert_eq!(doc.layers.get(id).unwrap().name, "Outline");
        let _ = run(&ctx, &mut w, &doc, Vec::new());
        assert!(renaming_in(&ctx).is_none(), "the field closed");
    }

    fn renaming_in(ctx: &egui::Context) -> Option<(LayerId, String)> {
        ctx.data(|d| d.get_temp::<Option<(LayerId, String)>>(rename_key()))
            .flatten()
    }
}
