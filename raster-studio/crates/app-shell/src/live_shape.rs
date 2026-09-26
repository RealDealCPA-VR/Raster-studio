//! W16-G: live shapes, the Parametric Shape tool and the Vector Gradient
//! tool, driven through the shell's own routes: the canvas pointer
//! (`ToolPointer::handle`, the route every canvas sample takes), the real
//! chrome frame with the Properties panel open (its `ChromeOutput::layer_kind`
//! applied by `Editor::apply_kind_edit`, what the shell does with it), and
//! the document's composite — and the `.psd` side: a live shape travels as
//! its `vogk` origination ([`live_vogk`] on the way out, [`live_from_psd`]
//! on the way in, both called from `import::psd_live`).

use layer_model::{LiveShape, ShapeLayer};

/// W16-G: `shape`'s live origination for a `.psd` (`vogk`), in canvas
/// pixels. Only under a pure translation: the record is an axis-aligned box,
/// so a scaled, rotated or skewed layer keeps its plain path (and the sharp
/// rectangle block `psd_live` writes for an axis-aligned rectangle path).
/// `None` for a shape that is not live or has no origination (polygon,
/// star).
pub(crate) fn live_vogk(shape: &ShapeLayer, transform: glam::Affine2) -> Option<Vec<u8>> {
    if transform.matrix2 != glam::Mat2::IDENTITY || !transform.translation.is_finite() {
        return None;
    }
    let live = tools::shape::live_shape_of(shape)?;
    let (dx, dy) = (
        f64::from(transform.translation.x),
        f64::from(transform.translation.y),
    );
    let [x, y, w, h] = live.frame();
    psd::live_origin::encode_live_origination(&live.with_frame([x + dx, y + dy, w, h]))
}

/// W16-G: the live shape a `.psd` layer's `vogk` describes, when it still
/// describes the layer's path — the regenerated outline's bounds within half
/// a pixel of the imported path's (`path_svg`, canvas pixels) — with the
/// path it regenerates, which is stored in place of the imported one so the
/// record reads as live (`tools::shape::live_shape_of`). `None` keeps the
/// imported path as a plain path.
pub(crate) fn live_from_psd(source: &psd::PsdLayer, path_svg: &str) -> Option<(LiveShape, String)> {
    let block = source.extra.iter().find(|b| b.key == *b"vogk")?;
    let live =
        psd::live_origin::decode_live_origination(&block.data, &psd::ReadOptions::default())?;
    let regenerated = tools::shape::live_path(&live).ok()?;
    let imported = vector::parse_svg(path_svg).ok()?.bounds();
    let ours = regenerated.bounds();
    let close = [
        (imported.min.x, ours.min.x),
        (imported.min.y, ours.min.y),
        (imported.max.x, ours.max.x),
        (imported.max.y, ours.max.y),
    ]
    .iter()
    .all(|(a, b)| (a - b).abs() <= 0.5);
    close.then(|| (live, vector::to_svg(&regenerated)))
}

#[cfg(test)]
mod tests {
    use glam::Vec2;
    use layer_model::{LayerKind, LiveShape, ShapeFillPaint, ShapeLayer};
    use tools::{Modifiers, ToolId, ToolSetting};
    use ui::canvas::{PointerInput, PointerPhase};

    use crate::chrome::{install_theme, Chrome, ChromeOutput};
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use crate::tool_input::ToolPointer;

    const SIDE: u32 = 64;
    const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);

    /// One opaque white 64x64 image at 100%, centred in the viewport.
    fn editor(dir: &std::path::Path) -> Editor {
        let png = dir.join("white.png");
        let rgba: Vec<u8> = [255u8, 255, 255, 255].repeat((SIDE * SIDE) as usize);
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, SIDE, SIDE, &rgba).unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        let doc = editor.active_mut().unwrap();
        doc.set_viewport(VIEWPORT);
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::splat(SIDE as f32 / 2.0);
        editor
    }

    fn screen(x: f32, y: f32) -> Vec2 {
        VIEWPORT * 0.5 + Vec2::new(x, y) - Vec2::splat(SIDE as f32 / 2.0)
    }

    /// Press at `a`, move to `b`, release there. Answers the steps it landed.
    fn drag(
        pointer: &mut ToolPointer,
        editor: &mut Editor,
        a: (f32, f32),
        b: (f32, f32),
        settings: &[(String, ToolSetting)],
    ) -> usize {
        let mut steps = 0;
        for (phase, (x, y)) in [
            (PointerPhase::Down, a),
            (PointerPhase::Move, b),
            (PointerPhase::Up, b),
        ] {
            let mut input = PointerInput::at(phase, screen(x, y));
            input.modifiers = Modifiers::NONE;
            steps += pointer.handle(editor, input, false, settings).steps;
        }
        steps
    }

    /// The top-most shape layer, made the active layer (what a click on its
    /// row in the Layers panel does), so Properties and the Vector Gradient
    /// tool act on it.
    fn active_shape(editor: &mut Editor) -> (layer_model::LayerId, ShapeLayer) {
        let doc = &mut editor.active_mut().unwrap().document;
        let (id, shape) = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .find_map(|id| match &doc.layers.get(id)?.kind {
                LayerKind::Shape(s) => Some((id, s.clone())),
                _ => None,
            })
            .expect("a shape layer");
        doc.set_active_layer(Some(id)).unwrap();
        (id, shape)
    }

    fn raw_input(events: Vec<egui::Event>, time: f64) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 1600.0),
            )),
            events,
            time: Some(time),
            ..Default::default()
        }
    }

    /// Draw a rectangle with the Rectangle tool on the canvas, then drag the
    /// Top Left radius field of the Properties panel's Live Shape section in a
    /// real chrome frame: the edits the chrome hands back, applied the way the
    /// shell applies them, round all four corners (Same Radii) as ONE undo step,
    /// and the path the compositor draws follows.
    #[test]
    fn a_drawn_rectangles_corner_radius_changes_in_properties_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_tool(ToolId::Rectangle);
        let mut pointer = ToolPointer::new();
        assert_eq!(
            drag(&mut pointer, &mut ed, (8.0, 8.0), (56.0, 40.0), &[]),
            1,
            "the rectangle is one step"
        );
        let (id, drawn) = active_shape(&mut ed);
        let live = tools::shape::live_shape_of(&drawn).expect("a drawn rectangle is live");
        assert_eq!(live.frame(), [8.0, 8.0, 48.0, 32.0]);
        // The corner pixel is painted while the corners are sharp.
        let corner_alpha = |ed: &mut Editor| {
            let rgba = ed
                .active_mut()
                .unwrap()
                .composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
                .unwrap();
            // The red channel: white background, black fill.
            rgba[((8 * SIDE + 8) * 4) as usize]
        };
        assert!(corner_alpha(&mut ed) < 64, "the sharp corner is filled");

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome
            .workspace_for_test()
            .dock
            .set_open(ui::PanelId::Properties, true);
        chrome
            .workspace_for_test()
            .dock
            .raise(ui::PanelId::Properties);
        let mut time = 0.0;
        let mut frame = |chrome: &mut Chrome, ed: &mut Editor, events: Vec<egui::Event>| {
            time += 0.5;
            let mut out = ChromeOutput::default();
            let _ = ctx.run(raw_input(events, time), |c| out = chrome.ui(c, ed));
            // What the shell does with the Properties panel's edits.
            for edit in out.layer_kind {
                ed.apply_kind_edit(edit);
            }
        };
        for _ in 0..3 {
            frame(&mut chrome, &mut ed, Vec::new());
        }
        let field = ctx
            .read_response(ui::panels::properties::live_ids::live_radius(id, 0))
            .expect("the Live Shape section's Top Left radius is drawn")
            .rect;
        let button = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let at = field.center();
        frame(
            &mut chrome,
            &mut ed,
            vec![egui::Event::PointerMoved(at), button(at, true)],
        );
        for dx in [4.0, 8.0, 12.0] {
            frame(
                &mut chrome,
                &mut ed,
                vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))],
            );
        }
        frame(
            &mut chrome,
            &mut ed,
            vec![button(at + egui::vec2(12.0, 0.0), false)],
        );
        frame(&mut chrome, &mut ed, Vec::new());

        let (_, edited) = active_shape(&mut ed);
        assert_ne!(edited.path_svg, drawn.path_svg, "the path was rewritten");
        let Some(LiveShape::Rectangle { radii, .. }) =
            tools::shape::live_shape_of(&edited).cloned()
        else {
            panic!("still a live rectangle: {edited:?}");
        };
        assert!(radii[0] >= 8.0, "{radii:?}");
        assert!(
            radii.iter().all(|r| *r == radii[0]),
            "Same Radii: {radii:?}"
        );
        assert!(
            corner_alpha(&mut ed) > 192,
            "the rounded corner is not filled"
        );
        // The whole drag is one step: one undo is back to the sharp rectangle.
        assert!(ed.active_mut().unwrap().undo().unwrap());
        let (_, undone) = active_shape(&mut ed);
        assert_eq!(undone.path_svg, drawn.path_svg);
    }

    /// Round 3: a rectangle drawn on the canvas and then dragged by the Move
    /// tool (a layer translation, not a path edit, so it stays live) shows
    /// its CANVAS X and Y in Properties' Live Shape section in a real chrome
    /// frame, and an X typed there (`set_frame`) or dragged there lands the
    /// shape at that canvas X, not that X plus the move.
    #[test]
    fn a_moved_live_shape_shows_and_edits_its_canvas_x_and_y_in_properties() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_tool(ToolId::Rectangle);
        let mut pointer = ToolPointer::new();
        assert_eq!(
            drag(&mut pointer, &mut ed, (8.0, 8.0), (40.0, 40.0), &[]),
            1
        );
        let _ = active_shape(&mut ed);
        ed.set_tool(ToolId::Move);
        let mut pointer = ToolPointer::new();
        assert_eq!(
            drag(&mut pointer, &mut ed, (20.0, 20.0), (44.0, 12.0), &[]),
            1
        );
        let (id, moved) = active_shape(&mut ed);
        let live = tools::shape::live_shape_of(&moved).expect("a moved rectangle stays live");
        assert_eq!(
            live.frame(),
            [8.0, 8.0, 32.0, 32.0],
            "the record is untouched"
        );
        let t = ed
            .active_mut()
            .unwrap()
            .document
            .layers
            .get(id)
            .unwrap()
            .transform;
        assert_eq!(
            t.translation,
            Vec2::new(24.0, -8.0),
            "the Move tool translated it"
        );
        let doc = &ed.active_mut().unwrap().document;
        assert_eq!(
            ui::panels::properties::LiveShapeProperties::canvas_frame(doc, id),
            Some([32.0, 0.0, 32.0, 32.0])
        );

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome
            .workspace_for_test()
            .dock
            .set_open(ui::PanelId::Properties, true);
        chrome
            .workspace_for_test()
            .dock
            .raise(ui::PanelId::Properties);
        let mut time = 0.0;
        let mut frame = |chrome: &mut Chrome, ed: &mut Editor, events: Vec<egui::Event>| {
            time += 0.5;
            let mut out = ChromeOutput::default();
            let full = ctx.run(raw_input(events, time), |c| out = chrome.ui(c, ed));
            for edit in out.layer_kind {
                ed.apply_kind_edit(edit);
            }
            full
        };
        // The number drawn inside a Live Shape frame field (0 W, 1 H, 2 X, 3 Y).
        fn field_text(ctx: &egui::Context, out: &egui::FullOutput, id: egui::Id) -> f64 {
            fn texts(shape: &egui::Shape, rect: egui::Rect, out: &mut Vec<String>) {
                match shape {
                    egui::Shape::Text(t) if rect.contains(t.pos) => {
                        out.push(t.galley.text().to_string())
                    }
                    egui::Shape::Vec(v) => v.iter().for_each(|s| texts(s, rect, out)),
                    _ => {}
                }
            }
            let rect = ctx.read_response(id).expect("the field is drawn").rect;
            let mut found = Vec::new();
            for clipped in &out.shapes {
                texts(&clipped.shape, rect.expand(1.0), &mut found);
            }
            found
                .iter()
                .find_map(|t| t.trim().parse::<f64>().ok())
                .unwrap_or_else(|| panic!("no number in the field: {found:?}"))
        }
        let live_frame = ui::panels::properties::live_ids::live_frame;
        let mut out = frame(&mut chrome, &mut ed, Vec::new());
        for _ in 0..3 {
            out = frame(&mut chrome, &mut ed, Vec::new());
        }
        assert_eq!(
            field_text(&ctx, &out, live_frame(id, 2)),
            32.0,
            "X is the canvas X"
        );
        assert_eq!(
            field_text(&ctx, &out, live_frame(id, 3)),
            0.0,
            "Y is the canvas Y"
        );
        assert_eq!(field_text(&ctx, &out, live_frame(id, 0)), 32.0, "W");

        // Drag the X field: the value it then shows is where the shape is.
        let button = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let at = ctx.read_response(live_frame(id, 2)).unwrap().rect.center();
        frame(
            &mut chrome,
            &mut ed,
            vec![egui::Event::PointerMoved(at), button(at, true)],
        );
        for dx in [4.0, 8.0] {
            frame(
                &mut chrome,
                &mut ed,
                vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))],
            );
        }
        frame(
            &mut chrome,
            &mut ed,
            vec![button(at + egui::vec2(8.0, 0.0), false)],
        );
        let mut out = frame(&mut chrome, &mut ed, Vec::new());
        for _ in 0..2 {
            out = frame(&mut chrome, &mut ed, Vec::new());
        }
        let shown = field_text(&ctx, &out, live_frame(id, 2));
        assert!(shown > 32.0, "the drag moved X: {shown}");
        let (_, dragged) = active_shape(&mut ed);
        let t = ed
            .active_mut()
            .unwrap()
            .document
            .layers
            .get(id)
            .unwrap()
            .transform;
        let canvas_x =
            tools::shape::live_shape_of(&dragged).unwrap().frame()[0] + f64::from(t.translation.x);
        assert_eq!(canvas_x, shown, "the shown X is the canvas X");

        // X = 50 set through Properties puts the left edge at canvas 50.
        let doc = &ed.active_mut().unwrap().document;
        let Some(ui::Intent::EditLayerKind { layer, kind }) =
            ui::panels::properties::LiveShapeProperties::set_frame(doc, id, 2, 50.0)
        else {
            panic!("an X edit");
        };
        ed.apply_kind_edit(crate::chrome::KindEdit {
            layer,
            kind,
            gesture: None,
        });
        let rgba = ed
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
            .unwrap();
        let red = |x: u32, y: u32| rgba[((y * SIDE + x) * 4) as usize];
        assert_eq!(red(49, 10), 255, "left of canvas X 50 is background");
        assert_eq!(red(51, 10), 0, "right of canvas X 50 is the fill");
    }

    /// The Parametric Shape tool (the Polygon slot) draws each of Photopea's
    /// shapes through the canvas pointer, its `pshape` option picking which.
    #[test]
    fn each_parametric_shape_draws_through_the_canvas_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_tool(ToolId::Polygon);
        for (index, name) in tools::shape::PARAMETRIC_SHAPE_CHOICES.iter().enumerate() {
            let mut pointer = ToolPointer::new();
            let settings = vec![("pshape".to_string(), ToolSetting::Choice(index))];
            let steps = drag(&mut pointer, &mut ed, (6.0, 10.0), (58.0, 54.0), &settings);
            assert_eq!(steps, 1, "{name}: one step");
            let (_, shape) = active_shape(&mut ed);
            let path = vector::parse_svg(&shape.path_svg).unwrap();
            let b = path.bounds();
            assert!(b.width() > 20.0 && b.height() > 20.0, "{name}: {b:?}");
        }
    }

    /// Photopea's Parametric Spiral (`X.hn.LC` -> `aoF`) through the canvas
    /// pointer: centred on the press, its radius the drag's length and its
    /// outer arm ending on the release, so a flat horizontal drag (a box no
    /// box-fitted spiral can fill) still draws it, and a drag upward turns it.
    #[test]
    fn the_parametric_spiral_is_centred_on_the_press_through_the_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_tool(ToolId::Polygon);
        let settings = vec![("pshape".to_string(), ToolSetting::Choice(4))];
        let anchors = |shape: &ShapeLayer| -> Vec<vector::Point> {
            let path = vector::parse_svg(&shape.path_svg).unwrap();
            path.elements()
                .iter()
                .filter_map(|e| e.end_point())
                .collect()
        };
        let near = |p: &vector::Point, x: f64, y: f64| p.distance(vector::point(x, y)) < 0.01;
        for tip in [(56.0f32, 32.0f32), (32.0, 4.0)] {
            let mut pointer = ToolPointer::new();
            let steps = drag(&mut pointer, &mut ed, (32.0, 32.0), tip, &settings);
            assert_eq!(steps, 1, "one step");
            let (_, shape) = active_shape(&mut ed);
            let pts = anchors(&shape);
            assert!(pts.iter().any(|p| near(p, 32.0, 32.0)), "centre: {pts:?}");
            assert!(
                pts.iter()
                    .any(|p| near(p, f64::from(tip.0), f64::from(tip.1))),
                "release: {pts:?}"
            );
            // Photopea's first node (1, -1) of the 6-unit spiral, turned
            // (not mirrored) onto the drag: which way the arms wind.
            let (dx, dy) = (f64::from(tip.0) - 32.0, f64::from(tip.1) - 32.0);
            let (c, s) = (dx / 6.0, dy / 6.0);
            assert!(
                pts.iter().any(|p| near(p, 32.0 + c + s, 32.0 + s - c)),
                "winding: {pts:?}"
            );
            let b = vector::parse_svg(&shape.path_svg).unwrap().bounds();
            let r = f64::from((tip.0 - 32.0f32).hypot(tip.1 - 32.0));
            assert!(
                b.width() > r && b.height() > r,
                "it winds round the press: {b:?}"
            );
            assert!(
                b.max.x <= 32.0 + r + 1e-6 && b.min.y >= 32.0 - r - 1e-6,
                "{b:?}"
            );
        }
    }

    /// The Vector Gradient tool drags a gradient-filled rectangle's end handle
    /// on the canvas: one step, the fill's angle turns and the composite's
    /// ramp follows.
    #[test]
    fn the_vector_gradient_tool_moves_a_shapes_gradient_by_its_handle() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_tool(ToolId::Rectangle);
        let mut pointer = ToolPointer::new();
        let gradient = vec![("fill_type".to_string(), ToolSetting::Choice(1))];
        drag(&mut pointer, &mut ed, (0.0, 0.0), (64.0, 64.0), &gradient);
        let (_, before) = active_shape(&mut ed);
        let ShapeFillPaint::Gradient(g) = &before.fill_paint else {
            panic!("a gradient fill: {before:?}");
        };
        let fit = tools::shape::vector_gradient::fit_of(&before.path_svg).unwrap();
        let [start, end] = tools::shape::vector_gradient::handles_of(g, fit);
        let pixel = |ed: &mut Editor, x: u32, y: u32| {
            let rgba = ed
                .active_mut()
                .unwrap()
                .composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
                .unwrap();
            rgba[((y * SIDE + x) * 4) as usize]
        };
        // The default ramp runs bottom to top (angle 90): the left and right
        // edges at mid height are the same shade.
        let (l0, r0) = (pixel(&mut ed, 2, 32), pixel(&mut ed, 61, 32));
        assert!(l0.abs_diff(r0) <= 2, "{l0} {r0}");

        ed.set_tool(ToolId::VectorGradient);
        let mut pointer = ToolPointer::new();
        // Grab the end handle and turn the line to run left to right.
        let steps = drag(
            &mut pointer,
            &mut ed,
            (end.x, end.y),
            (start.x + 32.0, start.y),
            &[],
        );
        assert_eq!(steps, 1, "one step");
        let (_, after) = active_shape(&mut ed);
        let ShapeFillPaint::Gradient(moved) = &after.fill_paint else {
            panic!()
        };
        assert_ne!(moved, g, "the fill's geometry changed");
        let (l1, r1) = (pixel(&mut ed, 2, 32), pixel(&mut ed, 61, 32));
        assert!(
            l1.abs_diff(r1) > 64,
            "the ramp now runs across: left {l1}, right {r1}"
        );
        assert!(matches!(
            pointer.live_geometry(),
            Some((_, tools::SessionGeometry::Measure { .. }))
        ));
    }

    /// A line drawn on the canvas with an end arrowhead stays live (its heads
    /// in the record, as Photopea keeps them): Properties' Weight rewrites the
    /// shaft AND the head as one step, and a `.psd` brings the heads back
    /// live (`keyOriginLineArr*`).
    #[test]
    fn an_arrowed_line_stays_live_through_properties_and_a_psd() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_tool(ToolId::Line);
        let mut pointer = ToolPointer::new();
        let settings = vec![
            ("width".to_string(), ToolSetting::Float(2.0)),
            ("arrow_end".to_string(), ToolSetting::Bool(true)),
        ];
        assert_eq!(
            drag(&mut pointer, &mut ed, (4.0, 32.0), (60.0, 32.0), &settings),
            1
        );
        let (id, drawn) = active_shape(&mut ed);
        let Some(LiveShape::Line {
            arrows: Some(heads),
            ..
        }) = tools::shape::live_shape_of(&drawn).cloned()
        else {
            panic!("the arrowed line is live: {drawn:?}");
        };
        assert!(heads.end && !heads.start);
        let height = |s: &ShapeLayer| vector::parse_svg(&s.path_svg).unwrap().bounds().height();
        let before = height(&drawn);
        let doc = &ed.active().unwrap().document;
        let Some(ui::Intent::EditLayerKind { layer, kind }) =
            ui::panels::properties::LiveShapeProperties::set_param(doc, id, "weight", 4.0)
        else {
            panic!("a weight edit");
        };
        ed.apply_kind_edit(crate::chrome::KindEdit {
            layer,
            kind,
            gesture: None,
        });
        let (_, thicker) = active_shape(&mut ed);
        // The head is 500% of the weight wide: 10 px at 2 px, 20 px at 4.
        assert!(
            (height(&thicker) - 2.0 * before).abs() < 0.5,
            "the head grew with the weight: {before} -> {}",
            height(&thicker)
        );
        let want = tools::shape::live_shape_of(&thicker).cloned().unwrap();
        let psd_path = dir.path().join("arrow.psd");
        ed.active_mut().unwrap().export_to(&psd_path).unwrap();
        ed.open_path(&psd_path).unwrap();
        let (_, back) = active_shape(&mut ed);
        assert_eq!(
            tools::shape::live_shape_of(&back),
            Some(&want),
            "the heads came back live: {back:?}"
        );
    }

    /// A rectangle drawn on the canvas and rounded in Properties (per corner)
    /// is saved as a `.psd` with its live origination and opens again as the
    /// same live rectangle; an ellipse and a line travel the same way.
    #[test]
    fn a_live_rectangle_keeps_its_radii_through_a_psd() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_tool(ToolId::Rectangle);
        let mut pointer = ToolPointer::new();
        drag(&mut pointer, &mut ed, (8.0, 8.0), (56.0, 40.0), &[]);
        let (id, _) = active_shape(&mut ed);
        let doc = &ed.active().unwrap().document;
        let Some(ui::Intent::EditLayerKind { layer, kind }) =
            ui::panels::properties::LiveShapeProperties::set_radius(doc, id, 1, 9.0, false)
        else {
            panic!("a radius edit");
        };
        ed.apply_kind_edit(crate::chrome::KindEdit {
            layer,
            kind,
            gesture: None,
        });
        let (_, rounded) = active_shape(&mut ed);
        let want = tools::shape::live_shape_of(&rounded).cloned().unwrap();
        let psd_path = dir.path().join("live.psd");
        ed.active_mut().unwrap().export_to(&psd_path).unwrap();
        ed.open_path(&psd_path).unwrap();
        let (_, back) = active_shape(&mut ed);
        assert_eq!(
            tools::shape::live_shape_of(&back),
            Some(&want),
            "the radii came back live: {back:?}"
        );
    }
}
