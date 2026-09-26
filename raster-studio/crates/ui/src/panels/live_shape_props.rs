//! W16-G: the Properties shape page's Live Shape section.
//!
//! A shape drawn by the Rectangle, Rounded Rectangle, Ellipse, Star or Line
//! tool keeps (the Parametric Shape tool's shapes are plain paths, as in
//! Photopea, so a polygon record comes only from a `ShapeLayer::live` built
//! directly) its parameters ([`layer_model::LiveShape`]) while its
//! path is untouched (`tools::shape::live_shape_of`). This section shows
//! them the way Photopea's Live Shape block does — the box's W, H, X and Y,
//! a rectangle's four corner radii with a Same Radii link, a polygon's
//! sides, a star's points and inner radius, a line's weight — and each edit
//! is one [`Intent::EditLayerKind`] carrying the new parameters and the path
//! they regenerate (`tools::shape::apply_live`), so it undoes in one step.
//! A shape that is not live (a custom shape, a path edited by hand) has no
//! section: [`LiveShapeProperties::show`] answers `None`.

use editor_core::Document;
use layer_model::{LayerId, LayerKind, LiveShape};

use crate::intent::Intent;

/// Stable ids for the Live Shape controls, so a headless test can find them.
pub mod ids {
    use layer_model::LayerId;

    /// A frame field: 0 = W, 1 = H, 2 = X, 3 = Y.
    pub fn live_frame(layer: LayerId, index: usize) -> egui::Id {
        egui::Id::new(("raster-properties-live-frame", layer, index))
    }
    /// A corner radius field, clockwise from the top left.
    pub fn live_radius(layer: LayerId, corner: usize) -> egui::Id {
        egui::Id::new(("raster-properties-live-radius", layer, corner))
    }
    /// The Same Radii link.
    pub fn live_same_radii(layer: LayerId) -> egui::Id {
        egui::Id::new(("raster-properties-live-same-radii", layer))
    }
    /// A shape parameter field (sides, points, inner radius, weight).
    pub fn live_param(layer: LayerId, key: &'static str) -> egui::Id {
        egui::Id::new(("raster-properties-live-param", layer, key))
    }
}

/// The Live Shape section's reads and edits.
pub struct LiveShapeProperties;

/// The catalogue keys of the frame fields, in [`ids::live_frame`] order.
const FRAME_KEYS: [&str; 4] = [
    "ui.docks.shape.live.w",
    "ui.docks.shape.live.h",
    "ui.docks.shape.live.x",
    "ui.docks.shape.live.y",
];
/// The catalogue keys of the corner radius fields, clockwise from top left.
const CORNER_KEYS: [&str; 4] = [
    "ui.docks.shape.live.radius.tl",
    "ui.docks.shape.live.radius.tr",
    "ui.docks.shape.live.radius.br",
    "ui.docks.shape.live.radius.bl",
];

impl LiveShapeProperties {
    /// The layer's live parameters, `None` when it is not a live shape.
    pub fn live(doc: &Document, layer: LayerId) -> Option<LiveShape> {
        match &doc.layers.get(layer)?.kind {
            LayerKind::Shape(s) => tools::shape::live_shape_of(s).cloned(),
            _ => None,
        }
    }

    /// Replace the layer's live parameters (and so its path). `None` when
    /// the layer is not live, the parameters describe nothing (a zero or
    /// negative size, a non-finite value) or nothing would change.
    pub fn set(doc: &Document, layer: LayerId, live: LiveShape) -> Option<Intent> {
        let LayerKind::Shape(current) = &doc.layers.get(layer)?.kind else {
            return None;
        };
        let before = tools::shape::live_shape_of(current)?;
        if *before == live {
            return None;
        }
        let next = tools::shape::apply_live(current, live).ok()?;
        (next != *current).then(|| Intent::EditLayerKind {
            layer,
            kind: Box::new(LayerKind::Shape(next)),
        })
    }

    /// The layer's transform as a pure translation `(dx, dy)` — what the
    /// Move tool leaves — or `None` when it scales, rotates or skews (or the
    /// layer is gone). The live record is in the shape's own (untransformed)
    /// space; the canvas box is the record's moved by this.
    pub fn translation(doc: &Document, layer: LayerId) -> Option<(f64, f64)> {
        let t = doc.layers.get(layer)?.transform;
        (t.matrix2.abs_diff_eq(glam::Mat2::IDENTITY, 1e-6) && t.translation.is_finite())
            .then(|| (f64::from(t.translation.x), f64::from(t.translation.y)))
    }

    /// The box Properties shows as `[x, y, w, h]`, in canvas pixels: the live
    /// record's box moved by the layer's translation, so a shape dragged by
    /// the Move tool shows where it is. `None` when the layer is not live or
    /// its transform is not a pure translation (the record's box is then not
    /// the canvas box, and the W, H, X, Y fields are not drawn).
    pub fn canvas_frame(doc: &Document, layer: LayerId) -> Option<[f64; 4]> {
        let [x, y, w, h] = Self::live(doc, layer)?.frame();
        let (dx, dy) = Self::translation(doc, layer)?;
        Some([x + dx, y + dy, w, h])
    }

    /// Set one of W (0), H (1), X (2), Y (3), in canvas pixels (X and Y are
    /// taken back through the layer's translation into the record). A size
    /// must stay positive; `None` under a non-translation transform.
    pub fn set_frame(doc: &Document, layer: LayerId, index: usize, value: f64) -> Option<Intent> {
        let live = Self::live(doc, layer)?;
        let (dx, dy) = Self::translation(doc, layer)?;
        let mut frame = live.frame();
        frame[0] += dx;
        frame[1] += dy;
        let slot = match index {
            0 => 2,
            1 => 3,
            2 => 0,
            3 => 1,
            _ => return None,
        };
        if !value.is_finite() || (slot >= 2 && value <= 0.0) {
            return None;
        }
        frame[slot] = value;
        frame[0] -= dx;
        frame[1] -= dy;
        Self::set(doc, layer, live.with_frame(frame))
    }

    /// Set a rectangle's corner radius `corner` (clockwise from the top
    /// left) — or, with `same`, all four to it (Photopea's Same Radii).
    pub fn set_radius(
        doc: &Document,
        layer: LayerId,
        corner: usize,
        radius: f64,
        same: bool,
    ) -> Option<Intent> {
        let LiveShape::Rectangle { x, y, w, h, radii } = Self::live(doc, layer)? else {
            return None;
        };
        if corner > 3 || !radius.is_finite() {
            return None;
        }
        let r = radius.max(0.0);
        let mut next = radii;
        if same {
            next = [r; 4];
        } else {
            next[corner] = r;
        }
        Self::set(
            doc,
            layer,
            LiveShape::Rectangle {
                x,
                y,
                w,
                h,
                radii: next,
            },
        )
    }

    /// The shape's own parameter `key` (`sides`, `points`, `inner_ratio`,
    /// `weight`) set to `value`.
    pub fn set_param(doc: &Document, layer: LayerId, key: &str, value: f64) -> Option<Intent> {
        if !value.is_finite() {
            return None;
        }
        let mut live = Self::live(doc, layer)?;
        let count = |v: f64| v.round().clamp(3.0, 100.0) as u32;
        match (&mut live, key) {
            (LiveShape::Polygon { sides, .. }, "sides") => *sides = count(value),
            (LiveShape::Star { points, .. }, "points") => *points = count(value),
            (LiveShape::Star { inner_ratio, .. }, "inner_ratio") => {
                *inner_ratio = value.clamp(0.01, 1.0)
            }
            (LiveShape::Line { weight, .. }, "weight") => *weight = value.max(0.1),
            _ => return None,
        }
        Self::set(doc, layer, live)
    }

    /// Draw the Live Shape section for `layer`; `None` (and nothing drawn)
    /// when the layer is not a live shape. The edits the user made this
    /// frame come back as intents.
    pub fn show(ui: &mut egui::Ui, doc: &Document, layer: LayerId) -> Option<Vec<Intent>> {
        use crate::strings::tr;
        let live = Self::live(doc, layer)?;
        let mut intents: Vec<Option<Intent>> = Vec::new();
        design::section_header(ui, tr("ui.docks.shape.live"));
        let frame = Self::canvas_frame(doc, layer);
        for (index, value) in frame
            .map(|[x, y, w, h]| [w, h, x, y])
            .into_iter()
            .flatten()
            .enumerate()
        {
            let mut v = value;
            let field = design::inspector_field(ui, tr(FRAME_KEYS[index]), |ui| {
                ui.add(egui::DragValue::new(&mut v).max_decimals(2))
            })
            .inner;
            ui.interact(
                field.rect,
                ids::live_frame(layer, index),
                egui::Sense::hover(),
            );
            if field.changed() {
                intents.push(Self::set_frame(doc, layer, index, v));
            }
        }
        match live {
            LiveShape::Rectangle { radii, .. } => {
                let key = ids::live_same_radii(layer);
                let equal = radii.iter().all(|r| *r == radii[0]);
                let mut same = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(equal);
                let toggle =
                    design::inspector_field(ui, tr("ui.docks.shape.live.same.radii"), |ui| {
                        ui.checkbox(&mut same, "")
                    })
                    .inner;
                ui.interact(toggle.rect, key, egui::Sense::hover());
                if toggle.changed() {
                    ui.data_mut(|d| d.insert_temp(key, same));
                }
                for (corner, radius) in radii.into_iter().enumerate() {
                    let mut r = radius;
                    let field = design::inspector_field(ui, tr(CORNER_KEYS[corner]), |ui| {
                        ui.add(
                            egui::DragValue::new(&mut r)
                                .range(0.0..=f64::from(u16::MAX))
                                .max_decimals(2),
                        )
                    })
                    .inner;
                    ui.interact(
                        field.rect,
                        ids::live_radius(layer, corner),
                        egui::Sense::hover(),
                    );
                    if field.changed() {
                        intents.push(Self::set_radius(doc, layer, corner, r, same));
                    }
                }
            }
            LiveShape::Polygon { sides, .. } => {
                intents.push(Self::param_row(ui, doc, layer, "sides", f64::from(sides)));
            }
            LiveShape::Star {
                points,
                inner_ratio,
                ..
            } => {
                intents.push(Self::param_row(ui, doc, layer, "points", f64::from(points)));
                intents.push(Self::param_row(ui, doc, layer, "inner_ratio", inner_ratio));
            }
            LiveShape::Line { weight, .. } => {
                intents.push(Self::param_row(ui, doc, layer, "weight", weight));
            }
            LiveShape::Ellipse { .. } => {}
        }
        Some(intents.into_iter().flatten().collect())
    }

    fn param_row(
        ui: &mut egui::Ui,
        doc: &Document,
        layer: LayerId,
        key: &'static str,
        value: f64,
    ) -> Option<Intent> {
        let label = match key {
            "sides" => "ui.docks.shape.live.sides",
            "points" => "ui.docks.shape.live.points",
            "inner_ratio" => "ui.docks.shape.live.inner",
            _ => "ui.docks.shape.live.weight",
        };
        let mut v = value;
        let field = design::inspector_field(ui, crate::strings::tr(label), |ui| {
            ui.add(egui::DragValue::new(&mut v).max_decimals(2))
        })
        .inner;
        ui.interact(
            field.rect,
            ids::live_param(layer, key),
            egui::Sense::hover(),
        );
        if field.changed() {
            Self::set_param(doc, layer, key, v)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::{Layer, ShapeLayer};
    use tools::shape::live_shape_of;

    fn live_rect_doc() -> (Document, LayerId) {
        let live = LiveShape::Rectangle {
            x: 10.0,
            y: 20.0,
            w: 80.0,
            h: 40.0,
            radii: [0.0; 4],
        };
        let shape = tools::shape::apply_live(&ShapeLayer::default(), live).unwrap();
        let mut doc = Document::new(128, 128, "Live");
        let id = doc
            .layers
            .push_root(Layer::with_kind("Rectangle", LayerKind::Shape(shape)))
            .unwrap();
        doc.set_active_layer(Some(id)).unwrap();
        (doc, id)
    }

    fn edited(intent: Option<Intent>) -> ShapeLayer {
        let Some(Intent::EditLayerKind { kind, .. }) = intent else {
            panic!("no edit: {intent:?}");
        };
        let LayerKind::Shape(s) = *kind else { panic!() };
        s
    }

    #[test]
    fn a_radius_edit_rewrites_the_path_and_same_radii_sets_all_four() {
        let (doc, id) = live_rect_doc();
        let before = match &doc.layers.get(id).unwrap().kind {
            LayerKind::Shape(s) => s.path_svg.clone(),
            _ => unreachable!(),
        };
        let one = edited(LiveShapeProperties::set_radius(&doc, id, 1, 10.0, false));
        assert_ne!(one.path_svg, before, "the path follows the radius");
        let Some(LiveShape::Rectangle { radii, .. }) = live_shape_of(&one).cloned() else {
            panic!("still live");
        };
        assert_eq!(radii, [0.0, 10.0, 0.0, 0.0]);
        let all = edited(LiveShapeProperties::set_radius(&doc, id, 1, 10.0, true));
        let Some(LiveShape::Rectangle { radii, .. }) = live_shape_of(&all).cloned() else {
            panic!("still live");
        };
        assert_eq!(radii, [10.0; 4]);
        // W, H, X, Y move and resize the box.
        let wider = edited(LiveShapeProperties::set_frame(&doc, id, 0, 100.0));
        assert_eq!(
            live_shape_of(&wider).unwrap().frame(),
            [10.0, 20.0, 100.0, 40.0]
        );
        assert!(LiveShapeProperties::set_frame(&doc, id, 1, 0.0).is_none());
        assert!(LiveShapeProperties::set_radius(&doc, id, 0, 0.0, false).is_none());
    }

    /// A layer moved by a translation (the Move tool) shows and edits its
    /// canvas box; a scaled layer's box is not the record's, so X/Y refuse.
    #[test]
    fn the_frame_is_in_canvas_pixels_through_the_layers_translation() {
        let (mut doc, id) = live_rect_doc();
        doc.layers.get_mut(id).unwrap().transform =
            glam::Affine2::from_translation(glam::Vec2::new(24.0, -8.0));
        assert_eq!(
            LiveShapeProperties::canvas_frame(&doc, id),
            Some([34.0, 12.0, 80.0, 40.0])
        );
        let moved = edited(LiveShapeProperties::set_frame(&doc, id, 2, 50.0));
        assert_eq!(
            live_shape_of(&moved).unwrap().frame(),
            [26.0, 20.0, 80.0, 40.0]
        );
        let down = edited(LiveShapeProperties::set_frame(&doc, id, 3, 0.0));
        assert_eq!(
            live_shape_of(&down).unwrap().frame(),
            [10.0, 8.0, 80.0, 40.0]
        );
        doc.layers.get_mut(id).unwrap().transform =
            glam::Affine2::from_scale(glam::Vec2::new(2.0, 1.0));
        assert_eq!(LiveShapeProperties::canvas_frame(&doc, id), None);
        assert!(LiveShapeProperties::set_frame(&doc, id, 2, 50.0).is_none());
    }

    /// The route: a real headless frame of the workspace with Properties
    /// open on a drawn rectangle draws the Live Shape section, and dragging
    /// its Top Right radius field emits the one layer edit whose path is
    /// rounded — all four corners while Same Radii is on, only that corner
    /// once the link is clicked off.
    #[test]
    fn the_properties_panel_edits_a_live_rectangles_radius_in_a_real_frame() {
        use crate::dock::{LayoutId, PanelId};
        let (doc, id) = live_rect_doc();
        let history = editor_core::History::new();
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut ws = crate::Workspace::new();
        ws.dock.apply_layout(LayoutId::Minimal);
        ws.dock.set_open(PanelId::Properties, true);
        let mut time = 0.0;
        let mut frame = |ws: &mut crate::Workspace, events: Vec<egui::Event>| {
            time += 1.0;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 1600.0),
                )),
                events,
                time: Some(time),
                ..Default::default()
            };
            let out = ctx.run(input, |c| ws.ui(c, &doc, &history));
            (ws.drain_intents(), out)
        };
        let mut drawn = Vec::new();
        for _ in 0..3 {
            let (_, out) = frame(&mut ws, Vec::new());
            drawn.clear();
            fn texts(shape: &egui::Shape, out: &mut Vec<String>) {
                match shape {
                    egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
                    _ => {}
                }
            }
            for clipped in &out.shapes {
                texts(&clipped.shape, &mut drawn);
            }
        }
        for key in [
            "ui.docks.shape.live",
            "ui.docks.shape.live.same.radii",
            "ui.docks.shape.live.radius.tr",
        ] {
            let text = crate::strings::tr(key);
            assert!(
                drawn.iter().any(|d| d == text),
                "{key} not drawn: {drawn:?}"
            );
        }
        let button = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let field = ctx
            .read_response(ids::live_radius(id, 1))
            .expect("the Top Right radius field is drawn")
            .rect;
        // Drag the field 20 points to the right.
        macro_rules! drag {
            ($ws:expr) => {{
                let at = field.center();
                let to = at + egui::vec2(20.0, 0.0);
                let mut intents = Vec::new();
                intents.extend(frame($ws, vec![egui::Event::PointerMoved(at), button(at, true)]).0);
                let mid = at + egui::vec2(10.0, 0.0);
                intents.extend(frame($ws, vec![egui::Event::PointerMoved(mid)]).0);
                intents.extend(frame($ws, vec![egui::Event::PointerMoved(to)]).0);
                intents.extend(frame($ws, vec![button(to, false)]).0);
                frame($ws, Vec::new());
                let last = intents
                    .into_iter()
                    .filter(|i| matches!(i, Intent::EditLayerKind { layer, .. } if *layer == id))
                    .last();
                edited(last)
            }};
        }
        let before = match &doc.layers.get(id).unwrap().kind {
            LayerKind::Shape(s) => s.path_svg.clone(),
            _ => unreachable!(),
        };
        let linked = drag!(&mut ws);
        assert_ne!(linked.path_svg, before, "the path was rewritten");
        let Some(LiveShape::Rectangle { radii, .. }) = live_shape_of(&linked).cloned() else {
            panic!("still live: {linked:?}");
        };
        assert!(radii[1] > 0.0, "{radii:?}");
        assert!(
            radii.iter().all(|r| *r == radii[1]),
            "Same Radii: {radii:?}"
        );
        // Click Same Radii off, then drag the same field: one corner only.
        let at = ctx
            .read_response(ids::live_same_radii(id))
            .expect("the Same Radii link is drawn")
            .rect
            .center();
        frame(
            &mut ws,
            vec![
                egui::Event::PointerMoved(at),
                button(at, true),
                button(at, false),
            ],
        );
        frame(&mut ws, Vec::new());
        let single = drag!(&mut ws);
        let Some(LiveShape::Rectangle { radii, .. }) = live_shape_of(&single).cloned() else {
            panic!("still live");
        };
        assert!(radii[1] > 0.0, "{radii:?}");
        assert_eq!(
            [radii[0], radii[2], radii[3]],
            [0.0; 3],
            "one corner: {radii:?}"
        );
    }
}
