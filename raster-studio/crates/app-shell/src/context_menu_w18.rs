//! W18-A: what the canvas right-click menu measures, at the right-click.
//!
//! [`ui::context_menu::w18`] builds each tool's list (Photopea's per-tool
//! menus); this module fills in what those rows need from the live document,
//! at the point the pointer was right-clicked:
//!
//! * **Move**: the layers whose visible content is under the point, top first
//!   ([`layers_under`]) — the shell's own canvas pick
//!   ([`crate::hit_testing::visible_content_at`]: visibility, opacity, masks
//!   and the placed extent, exactly what the Move tool's Auto-Select claims),
//!   asked again with each hit hidden until nothing more is under the point.
//! * **Selection tools**: Make Work Path, the Paths footer's work path from
//!   the selection.
//! * **Pen and path tools**: the current path — the Paths panel's Work Path
//!   when it is the selected row, else the active (or Paths-selected) shape
//!   layer's, else the pen's Work Path — and the anchor and the subpath under
//!   the point, for Remove Anchor Point and Remove Path; Make Selection, Fill
//!   and Stroke are the Paths footer's commands over it.
//! * **Type**: the text layer under the point, for Edit.
//! * **Slice tools**: the slice under the point. As in Photopea, the
//!   right-click picks it (so Slice Options edits it); Delete removes it and
//!   Divide… opens Divide Slices over it (the canvas when there is none). The
//!   slice store is the application's, so these ride a parked
//!   [`SliceMenuRequest`] and the Clear row's arm
//!   ([`crate::slices_export::perform_slice_menu_request`]).

use editor_core::Command;
use glam::Vec2;
use layer_model::{LayerId, LayerKind};
use tools::ToolId;
use ui::context_menu::divide_slice::{self, DivideSliceDialog, DivideTarget, SliceMenuRequest};
use ui::context_menu::w18::{family, CanvasMenu, CanvasRow, Family};
use ui::strings::tr;
use ui::Intent;
use vector::anchors;

use crate::doc::OpenDocument;
use crate::editor::Editor;

/// Open the acting tool's canvas menu for a right-click at `pos` (egui
/// points), which is `at` in document pixels; `zoom_pt` is points per
/// document pixel.
pub(crate) fn open(
    ctx: &egui::Context,
    workspace: &mut ui::Workspace,
    editor: &Editor,
    doc: &OpenDocument,
    at: Vec2,
    zoom_pt: f32,
    pos: egui::Pos2,
) {
    let menu = measure(workspace, editor, doc, at, zoom_pt);
    if family(&menu) == Family::Slice {
        // Photopea's right-click picks the slice under it.
        let before = editor.slices.get(doc.id()).to_vec();
        if let Some(index) = slice_at(&before, at) {
            divide_slice::park(SliceMenuRequest::Pick { index, before });
            workspace.emit(Intent::Action(ui::menu::MenuAction::ClearPixels));
        }
    }
    ui::context_menu::w18::open_canvas(workspace, ctx, menu, pos);
}

/// Everything the acting tool's rows need, measured at `at`.
pub(crate) fn measure(
    workspace: &ui::Workspace,
    editor: &Editor,
    doc: &OpenDocument,
    at: Vec2,
    zoom_pt: f32,
) -> CanvasMenu {
    let tool = editor.tool();
    let mut menu = CanvasMenu::bare(tool);
    menu.transforming =
        workspace.canvas.sessions.transform.is_some() || tool == ToolId::FreeTransform;
    match family(&menu) {
        Family::Move => {
            menu.layers_under = layers_under(doc, at)
                .into_iter()
                .map(|id| {
                    let name = doc
                        .document
                        .layers
                        .get(id)
                        .map(|l| l.name.clone())
                        .unwrap_or_default();
                    (id, name)
                })
                .collect();
        }
        Family::Selection => {
            menu.make_work_path = ui::panels::paths::selection_to_path(&doc.document)
                .map(|p| CanvasRow::WorkPath(Some(p)))
                .ok_or(tr("ui.canvas_menu.no_selection"));
        }
        Family::Pen => pen_rows(&mut menu, workspace, doc, at, zoom_pt),
        Family::Type => {
            menu.edit_text = layers_under(doc, at)
                .into_iter()
                .find(|id| {
                    doc.document
                        .layers
                        .get(*id)
                        .is_some_and(|l| matches!(l.kind, LayerKind::Text(_)))
                })
                .map(|layer| CanvasRow::Emit(vec![Intent::EnterTextLayer { layer }]))
                .ok_or(tr("ui.canvas_menu.no_text"));
        }
        Family::Slice => {
            let before = editor.slices.get(doc.id()).to_vec();
            let under = slice_at(&before, at);
            menu.delete_slice = under
                .map(|index| {
                    CanvasRow::Slice(SliceMenuRequest::Delete {
                        index,
                        before: before.clone(),
                    })
                })
                .ok_or(tr("ui.canvas_menu.no_slice"));
            let canvas = raster::PixelRect::new(0, 0, doc.document.width(), doc.document.height());
            let target = under.map_or(DivideTarget::Area(canvas), DivideTarget::Slice);
            menu.divide_slice = Ok(CanvasRow::Divide(DivideSliceDialog::new(before, target)));
        }
        Family::Transform | Family::Zoom | Family::General => {}
    }
    menu
}

/// The first slice of `slices` that contains `at` (Photopea's pick order).
pub(crate) fn slice_at(slices: &[raster::PixelRect], at: Vec2) -> Option<usize> {
    let (x, y) = (at.x.floor() as i64, at.y.floor() as i64);
    slices.iter().position(|r| {
        x >= r.x && y >= r.y && x < r.x + i64::from(r.width) && y < r.y + i64::from(r.height)
    })
}

/// The leaf layers whose visible content is under `at`, top first: the
/// shell's canvas pick, asked again with each hit hidden.
pub(crate) fn layers_under(doc: &OpenDocument, at: Vec2) -> Vec<LayerId> {
    let mut probe = doc.document.clone();
    let mut out = Vec::new();
    while let Some(hit) =
        crate::hit_testing::visible_content_at(&probe, &probe.pixels, &doc.tiles, at, 0.5)
    {
        if out.contains(&hit.layer) {
            break;
        }
        out.push(hit.layer);
        match probe.layers.get_mut(hit.layer) {
            Some(layer) => layer.visible = false,
            None => break,
        }
    }
    out
}

/// The path the pen rows act on.
enum PathTarget {
    /// A shape layer's path, in the layer's own space.
    Shape {
        layer: LayerId,
        shape: Box<layer_model::ShapeLayer>,
        transform: glam::Affine2,
    },
    /// The Paths panel's Work Path, in document space.
    Work(vector::Path),
}

fn current_path(workspace: &ui::Workspace, doc: &OpenDocument) -> Option<PathTarget> {
    let paths = &workspace.paths;
    if paths.work_selected {
        if let Some(path) = paths.work_path.clone() {
            return Some(PathTarget::Work(path));
        }
    }
    let shape_of = |id: LayerId| {
        let layer = doc.document.layers.get(id)?;
        match &layer.kind {
            LayerKind::Shape(shape) if !shape.path_svg.trim().is_empty() => {
                Some(PathTarget::Shape {
                    layer: id,
                    shape: Box::new(shape.clone()),
                    transform: layer.transform,
                })
            }
            _ => None,
        }
    };
    doc.document
        .active_layer()
        .and_then(shape_of)
        .or_else(|| paths.selected.and_then(shape_of))
        .or_else(|| paths.work_path.clone().map(PathTarget::Work))
}

fn pen_rows(
    menu: &mut CanvasMenu,
    workspace: &ui::Workspace,
    doc: &OpenDocument,
    at: Vec2,
    zoom_pt: f32,
) {
    let no_path = tr("ui.canvas_menu.no_path");
    let Some(target) = current_path(workspace, doc) else {
        for row in [
            &mut menu.remove_anchor,
            &mut menu.remove_path,
            &mut menu.make_selection,
            &mut menu.fill_path,
            &mut menu.stroke_path,
        ] {
            *row = Err(no_path);
        }
        return;
    };
    // The path in its own space, the point in that space, and the path in
    // document space (what the Paths footer's commands take).
    let (own, point, doc_path) = match &target {
        PathTarget::Shape {
            shape, transform, ..
        } => {
            let Ok(own) = vector::parse_svg(&shape.path_svg) else {
                return;
            };
            let local = transform.inverse().transform_point2(at);
            let doc_path = own.transform(&ui::panels::paths::affine_of(*transform));
            (own, local, doc_path)
        }
        PathTarget::Work(path) => (path.clone(), at, path.clone()),
    };
    let foreground = workspace.color.foreground();
    let width = workspace.options.brush_settings(ToolId::Brush).size;
    menu.make_selection = ui::panels::paths::load_as_selection(&doc.document, &doc_path)
        .map(|c| CanvasRow::Emit(vec![Intent::Document(c)]))
        .ok_or(tr("ui.docks.paths.encloses.nothing"));
    menu.fill_path = Ok(CanvasRow::Emit(vec![Intent::Document(
        ui::panels::paths::fill_layer(&doc_path, foreground),
    )]));
    menu.stroke_path = Ok(CanvasRow::Emit(vec![Intent::Document(
        ui::panels::paths::stroke_layer(&doc_path, foreground, width),
    )]));

    let p = vector::Point::new(f64::from(point.x), f64::from(point.y));
    // The handle grab band, in document pixels at this zoom.
    let radius = f64::from(ui::canvas::GUIDE_GRAB_PT / zoom_pt.max(f32::EPSILON));
    let subpaths = anchors::from_path(&own);
    let edited = |subpaths: Vec<anchors::AnchorPath>| -> CanvasRow {
        let path = anchors::to_path(&subpaths);
        match &target {
            PathTarget::Shape { layer, shape, .. } => {
                let mut shape = (**shape).clone();
                shape.path_svg = vector::to_svg(&path);
                CanvasRow::Emit(vec![Intent::Document(Command::SetLayerKind {
                    layer_id: *layer,
                    kind: Box::new(LayerKind::Shape(shape)),
                })])
            }
            PathTarget::Work(_) => CanvasRow::WorkPath((!path.is_empty()).then_some(path)),
        }
    };
    menu.remove_anchor = anchors::anchor_near(&subpaths, p, radius)
        .and_then(|at| {
            let mut next = subpaths.clone();
            anchors::delete_anchor(&mut next, at).then_some(next)
        })
        .map(edited)
        .ok_or(tr("ui.canvas_menu.no_anchor"));
    menu.remove_path = subpaths
        .iter()
        .position(|sp| {
            let one = anchors::to_path(std::slice::from_ref(sp));
            vector::contains(&one, p, vector::FillRule::NonZero)
                || vector::hit_stroke(&one, p, radius)
        })
        .map(|i| {
            let mut next = subpaths.clone();
            next.remove(i);
            edited(next)
        })
        .ok_or(tr("ui.canvas_menu.no_path_here"));
}

#[cfg(test)]
mod tests {
    //! Through the real chrome: `Chrome::ui` on a headless egui frame, a
    //! right-click where a user's would be, the drawn rows read back from the
    //! painted text, and a row clicked by its drawn rectangle — its intents
    //! performed as `Shell` performs them.
    use crate::chrome::{install_theme, Chrome};
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use editor_core::{Command, PixelTarget, TileEdit};
    use raster::TileCoord;
    use tools::ToolId;
    use ui::context_menu::ids::context_item;

    /// An 8x8 document centred at zoom 4 puts document (4, 4) at the
    /// window's centre, clear of the docks.
    const CENTRE: egui::Pos2 = egui::pos2(700.0, 450.0);
    const ZOOM: f32 = 4.0;

    struct Rig {
        _dir: tempfile::TempDir,
        editor: Editor,
        chrome: Chrome,
        ctx: egui::Context,
        last: Option<egui::FullOutput>,
    }

    fn rig(tool: ToolId) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        std::fs::write(
            &p,
            // Opaque, so the background is under every point of the canvas.
            raster::encode(
                raster::ExportFormat::Png,
                8,
                8,
                &[[9u8, 9, 9, 255]; 8 * 8].concat(),
            )
            .unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        editor.open_path(&p).unwrap();
        {
            let doc = editor.active_mut().unwrap();
            doc.set_viewport(glam::Vec2::new(1400.0, 900.0));
            doc.camera.zoom = ZOOM;
            doc.camera.center = glam::Vec2::new(4.0, 4.0);
        }
        editor.set_tool(tool);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut rig = Rig {
            _dir: dir,
            editor,
            chrome: Chrome::new(),
            ctx,
            last: None,
        };
        for _ in 0..3 {
            rig.step(Vec::new());
        }
        rig
    }

    impl Rig {
        fn step(&mut self, events: Vec<egui::Event>) {
            let mut out = None;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let editor = &mut self.editor;
            let chrome = &mut self.chrome;
            let full = self.ctx.run(input, |ctx| {
                out = Some(chrome.ui(ctx, editor));
            });
            let out = out.expect("a frame ran");
            // The selection lands first, as `Shell::apply_chrome` lands it.
            if let Some((layers, active)) = out.select_layers {
                self.editor.set_layer_selection(layers, active);
            } else if let Some(id) = out.select_layer {
                self.editor.set_active_layer(id);
            }
            for command in out.commands {
                self.editor.apply_command(command);
            }
            for action in out.actions {
                let _ = self.editor.dispatch(action);
            }
            for action in out.menu {
                let _ = crate::menu_bridge::perform(action, &mut self.editor);
            }
            self.last = Some(full);
        }

        fn click(&mut self, at: egui::Pos2, button: egui::PointerButton) {
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            self.step(vec![egui::Event::PointerMoved(at)]);
            self.step(vec![press(true), press(false)]);
        }

        /// Right-click at `at` and let the drawer lay the menu out.
        fn menu_at(&mut self, at: egui::Pos2) -> Vec<String> {
            self.click(at, egui::PointerButton::Secondary);
            assert!(
                self.chrome.extras_report().context_menu,
                "the canvas menu opened"
            );
            self.step(Vec::new());
            self.drawn_rows()
        }

        /// The rows the drawer laid out, read from the frame: each row id's
        /// rectangle, and the text the drawer painted at its leading inset
        /// (the menu floats over the docks, whose text sits under it).
        fn drawn_rows(&self) -> Vec<String> {
            let inset = design::Space::Small.pt();
            let full = self.last.as_ref().unwrap();
            let texts: Vec<(egui::Pos2, String)> = full
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => Some((t.pos, t.galley.text().to_string())),
                    _ => None,
                })
                .collect();
            (0..)
                .map_while(|i| self.ctx.read_response(context_item(i)))
                .map(|r| {
                    texts
                        .iter()
                        .rev()
                        .find(|(p, _)| {
                            (p.x - (r.rect.left() + inset)).abs() < 0.5
                                && p.y >= r.rect.top()
                                && p.y < r.rect.bottom()
                        })
                        .map(|(_, t)| t.clone())
                        .unwrap_or_default()
                })
                .collect()
        }

        /// The open submenu's rows, read like [`Self::drawn_rows`].
        fn drawn_subrows(&self) -> Vec<String> {
            let inset = design::Space::Small.pt();
            let full = self.last.as_ref().unwrap();
            (0..)
                .map_while(|k| {
                    self.ctx
                        .read_response(ui::context_menu::ids::context_subitem(k))
                })
                .map(|r| {
                    full.shapes
                        .iter()
                        .rev()
                        .find_map(|c| match &c.shape {
                            egui::Shape::Text(t)
                                if (t.pos.x - (r.rect.left() + inset)).abs() < 0.5
                                    && t.pos.y >= r.rect.top()
                                    && t.pos.y < r.rect.bottom() =>
                            {
                                Some(t.galley.text().to_string())
                            }
                            _ => None,
                        })
                        .unwrap_or_default()
                })
                .collect()
        }

        fn click_row(&mut self, label: &str) {
            let rows = self.drawn_rows();
            let i = rows
                .iter()
                .position(|r| r == label)
                .unwrap_or_else(|| panic!("no row {label:?} in {rows:?}"));
            let at = self
                .ctx
                .read_response(context_item(i))
                .unwrap()
                .rect
                .center();
            self.click(at, egui::PointerButton::Primary);
            self.step(Vec::new());
        }
    }

    /// A raster layer on top whose ink covers document (2..6, 2..6).
    fn inked_layer(editor: &mut Editor, name: &str) -> layer_model::LayerId {
        let mut bytes = Vec::with_capacity(256 * 256 * 4);
        for y in 0..256u32 {
            for x in 0..256u32 {
                let inked = (2..6).contains(&x) && (2..6).contains(&y);
                bytes.extend_from_slice(&if inked { [200, 10, 10, 255] } else { [0; 4] });
            }
        }
        let doc = editor.active_mut().unwrap();
        let layer = layer_model::Layer::raster(name);
        let id = layer.id;
        doc.apply(Command::create_layer(layer)).unwrap();
        let hash = doc.tiles.insert_bytes(bytes);
        doc.apply(
            Command::paint_tiles(
                PixelTarget::Layer(id),
                vec![TileEdit::set(TileCoord::new(0, 0, 0), hash)],
            )
            .unwrap(),
        )
        .unwrap();
        id
    }

    #[test]
    fn move_lists_the_two_layers_under_the_point_and_a_row_selects_one() {
        let mut rig = rig(ToolId::Move);
        let background = rig
            .editor
            .active()
            .unwrap()
            .document
            .active_layer()
            .unwrap();
        let top = inked_layer(&mut rig.editor, "Red ink");
        {
            let doc = rig.editor.active_mut().unwrap();
            doc.document.set_layer_selection(vec![top]).unwrap();
            doc.document.set_active_layer(Some(top)).unwrap();
        }
        let background_name = rig
            .editor
            .active()
            .unwrap()
            .document
            .layers
            .get(background)
            .unwrap()
            .name
            .clone();
        let doc = rig.editor.active().unwrap();
        assert_eq!(
            super::layers_under(doc, glam::Vec2::new(4.5, 4.5)),
            vec![top, background],
            "both layers are under the ink"
        );
        assert_eq!(
            super::layers_under(doc, glam::Vec2::new(0.5, 0.5)),
            vec![background],
            "only the background is under a bare corner"
        );

        let rows = rig.menu_at(CENTRE);
        assert_eq!(
            rows,
            vec![
                "Red ink".to_string(),
                background_name.clone(),
                "Cut".to_string(),
                "Copy".to_string(),
                "Paste".to_string(),
            ],
            "the Move menu: the layers under the pointer, then the clipboard"
        );
        assert_eq!(
            rig.editor.active().unwrap().document.active_layer(),
            Some(top),
            "the right-click itself selects nothing"
        );
        rig.click_row(&background_name);
        assert_eq!(
            rig.editor.active().unwrap().document.active_layer(),
            Some(background),
            "the row selected the layer it names"
        );
    }

    #[test]
    fn each_tool_right_click_shows_its_own_rows() {
        let cases: [(ToolId, &[&str]); 4] = [
            (
                ToolId::RectMarquee,
                &[
                    "Deselect",
                    "Refine Edge…",
                    "Make Work Path",
                    "Layer via Copy",
                ],
            ),
            (ToolId::Zoom, &["Zoom In", "Zoom Out", "Fit on Screen"]),
            (
                ToolId::Pen,
                &["Remove Anchor Point", "Remove Path", "Make Selection"],
            ),
            (ToolId::Slice, &["Delete", "Slice Options…", "Divide…"]),
        ];
        for (tool, expected) in cases {
            let mut rig = rig(tool);
            let rows = rig.menu_at(CENTRE);
            for label in expected {
                assert!(
                    rows.iter().any(|r| r == label),
                    "{tool:?}: {label:?} missing from {rows:?}"
                );
            }
        }
        // While a free transform is up, whichever tool: its own list.
        let mut rig = rig(ToolId::FreeTransform);
        let rows = rig.menu_at(CENTRE);
        for label in ["Scale", "Skew", "Warp", "Flip Horizontal", "Flip Vertical"] {
            assert!(rows.iter().any(|r| r == label), "{label:?} in {rows:?}");
        }
        // A brush keeps the general list.
        let mut rig = rig_brush();
        let rows = rig.menu_at(CENTRE);
        assert_eq!(
            rows,
            [
                "Fill…",
                "Stroke…",
                "Free Transform",
                "Transform Selection",
                "All",
                "Deselect",
                "Inverse"
            ],
            "the general list"
        );
    }

    fn rig_brush() -> Rig {
        rig(ToolId::Brush)
    }

    #[test]
    fn a_selection_row_makes_the_work_path_through_the_real_route() {
        let mut rig = rig(ToolId::RectMarquee);
        rig.editor.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(1, 1),
            max: glam::IVec2::new(6, 6),
        };
        rig.step(Vec::new());
        rig.menu_at(CENTRE);
        assert!(rig.chrome.workspace().paths.work_path.is_none());
        rig.click_row("Make Work Path");
        let paths = &rig.chrome.workspace().paths;
        assert!(
            paths.work_selected,
            "the Work Path is the Paths panel's row"
        );
        let path = paths
            .work_path
            .as_ref()
            .expect("a work path from the selection");
        let b = path.bounds();
        assert!(
            (b.min.x - 1.0).abs() < 1e-6 && (b.max.x - 6.0).abs() < 1e-6,
            "the path traces the selection: {b:?}"
        );
    }

    #[test]
    fn the_selection_menus_modify_submenu_opens_a_modify_dialog_through_the_real_route() {
        let mut rig = rig(ToolId::RectMarquee);
        rig.editor.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(1, 1),
            max: glam::IVec2::new(6, 6),
        };
        rig.step(Vec::new());
        let rows = rig.menu_at(CENTRE);
        assert!(
            !rows.iter().any(|r| r == "Feather…"),
            "Modify's rows are in its submenu, not the menu: {rows:?}"
        );
        // Hover the Modify row: its five open beside it.
        let i = rows
            .iter()
            .position(|r| r == "Modify")
            .expect("a Modify row");
        let at = rig
            .ctx
            .read_response(context_item(i))
            .unwrap()
            .rect
            .center();
        rig.step(vec![egui::Event::PointerMoved(at)]);
        // The submenu settles beside its row within a few frames.
        for _ in 0..3 {
            rig.step(Vec::new());
        }
        let sub = rig.drawn_subrows();
        assert_eq!(
            sub,
            ["Border…", "Smooth…", "Expand…", "Contract…", "Feather…"],
            "Photopea's Modify submenu"
        );
        let row = rig.ctx.read_response(context_item(i)).unwrap().rect;
        let first = rig
            .ctx
            .read_response(ui::context_menu::ids::context_subitem(0))
            .unwrap()
            .rect;
        assert!(
            first.left() >= row.right() - 1.0,
            "the submenu opens beside its row, not over it: {row:?} {first:?}"
        );
        assert!(!rig.chrome.dialog_open());
        let k = sub.iter().position(|r| r == "Expand…").unwrap();
        let at = rig
            .ctx
            .read_response(ui::context_menu::ids::context_subitem(k))
            .unwrap()
            .rect
            .center();
        rig.click(at, egui::PointerButton::Primary);
        rig.step(Vec::new());
        assert!(
            rig.chrome.dialog_open(),
            "the Expand row opened Select > Modify > Expand's dialog"
        );
    }

    #[test]
    fn a_zoom_row_zooms_through_the_real_route() {
        let mut rig = rig(ToolId::Zoom);
        let before = rig.editor.active().unwrap().camera.zoom;
        rig.menu_at(CENTRE);
        rig.click_row("Zoom In");
        let after = rig.editor.active().unwrap().camera.zoom;
        assert!(after > before, "zoom {before} -> {after}");
    }

    #[test]
    fn the_slice_menu_deletes_and_divides_the_slice_under_the_pointer() {
        let mut rig = rig(ToolId::SliceSelect);
        let id = rig.editor.active().unwrap().id();
        rig.editor.slices.remember(
            id,
            vec![
                raster::PixelRect::new(0, 0, 2, 2),
                raster::PixelRect::new(2, 2, 6, 6),
            ],
        );
        // The right-click picks the slice under it, as Photopea's does.
        rig.menu_at(CENTRE);
        assert_eq!(rig.editor.slices.picked(id), Some(1));

        // Divide… opens Divide Slices over that slice; Vertically into 2.
        rig.click_row("Divide…");
        assert!(ui::context_menu::divide_slice::is_open(&rig.ctx));
        ui::context_menu::divide_slice::with_open(&rig.ctx, |d| {
            d.request.spec.vertically.on = true;
            d.request.spec.vertically.n = 2;
        })
        .unwrap();
        rig.step(vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }]);
        rig.step(Vec::new());
        assert!(!ui::context_menu::divide_slice::is_open(&rig.ctx));
        assert_eq!(
            rig.editor.slices.get(id),
            &[
                raster::PixelRect::new(0, 0, 2, 2),
                raster::PixelRect::new(2, 2, 6, 3),
                raster::PixelRect::new(2, 5, 6, 3),
            ],
            "the slice under the pointer became two"
        );
        assert_eq!(
            rig.editor.active().unwrap().document.slices.len(),
            3,
            "the divide reached the document (one undo step)"
        );

        // Delete removes the slice under the pointer.
        rig.menu_at(egui::pos2(CENTRE.x - 12.0, CENTRE.y - 12.0 - ZOOM));
        rig.click_row("Delete");
        assert_eq!(
            rig.editor.slices.get(id).len(),
            2,
            "{:?}",
            rig.editor.slices.get(id)
        );
    }
}
