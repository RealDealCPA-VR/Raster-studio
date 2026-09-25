//! W16-D, driven through the real `Chrome`: every Layers-panel behaviour is
//! a click, a double-click, an Alt-click or a drag at the rectangle the
//! panel drew, and every outcome is read back from the editor (or the
//! dialog host, or the painted shapes) after the frame's output is applied
//! the way `Shell::apply_chrome` applies it.

use std::path::{Path, PathBuf};

use editor_core::Command;
use glam::Vec2;
use layer_model::{Layer, LayerId, LayerKind, ShadowEffect, StrokeEffect};
use ui::dialogs::layer_style::StylePage;
use ui::dialogs::EffectKind;
use ui::menu::{EffectSlot, MenuAction};
use ui::panels::layers::w16::{ids, OptionsItem};
use ui::panels::layers::ThumbScale;

use crate::action::Action;
use crate::chrome::Chrome;
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

const W: u32 = 64;
const H: u32 = 64;

fn png(dir: &Path, name: &str) -> PathBuf {
    let rgba = vec![200u8; (W * H * 4) as usize];
    let bytes = raster::encode(raster::ExportFormat::Png, W, H, &rgba).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// What one frame asked of the application, recorded before it is applied.
#[derive(Default)]
struct Applied {
    menu: Vec<MenuAction>,
    actions: Vec<Action>,
    full: Option<egui::FullOutput>,
}

/// The editor, the real chrome drawing the Layers panel, and a clock.
struct Rig {
    _dir: tempfile::TempDir,
    ctx: egui::Context,
    chrome: Chrome,
    ed: Editor,
    t: f64,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&png(dir.path(), "base.png")).unwrap();
        let doc = ed.active_mut().unwrap();
        doc.set_viewport(Vec2::new(400.0, 300.0));
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let mut rig = Self {
            _dir: dir,
            ctx,
            chrome: Chrome::new(),
            ed,
            t: 1.0,
        };
        rig.open_layers_panel();
        rig
    }

    fn open_layers_panel(&mut self) {
        self.chrome
            .emit(ui::Intent::ApplyLayout(ui::LayoutId::Minimal));
        self.chrome.emit(ui::Intent::SetPanelOpen {
            panel: ui::PanelId::Layers,
            open: true,
        });
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.frame(Vec::new(), egui::Modifiers::NONE);
        }
    }

    /// One frame of `events`, its output applied as the shell applies it:
    /// the selection, then the actions, the commands and the menu picks.
    fn frame(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) -> Applied {
        // Frames 50 ms apart: two clicks in consecutive frames are a
        // double-click to egui (see `double_click_at`).
        self.t += 0.05;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            time: Some(self.t),
            modifiers,
            events,
            ..Default::default()
        };
        let mut out = crate::chrome::ChromeOutput::default();
        let full = self.ctx.run(input, |ctx| {
            out = self.chrome.ui(ctx, &mut self.ed);
        });
        let mut applied = Applied {
            menu: out.menu.clone(),
            actions: out.actions.clone(),
            full: Some(full),
        };
        if let Some((layers, active)) = out.select_layers.take() {
            self.ed.set_layer_selection(layers, active);
        }
        for action in std::mem::take(&mut out.actions) {
            let _ = self.ed.dispatch(action);
        }
        for command in std::mem::take(&mut out.commands) {
            self.ed.apply_command(command);
        }
        for action in std::mem::take(&mut out.menu) {
            let _ = crate::menu_bridge::perform(action, &mut self.ed);
        }
        applied.full.get_or_insert_with(Default::default);
        applied
    }

    fn rect(&self, id: egui::Id) -> egui::Rect {
        self.ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} is not drawn"))
            .rect
    }

    fn drawn(&self, id: egui::Id) -> bool {
        self.ctx.read_response(id).is_some()
    }

    fn click_at(&mut self, at: egui::Pos2, modifiers: egui::Modifiers) -> Applied {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        };
        let applied = self.frame(
            vec![egui::Event::PointerMoved(at), button(true), button(false)],
            modifiers,
        );
        // The pointer leaves, so no hover state carries into the next step.
        self.frame(Vec::new(), egui::Modifiers::NONE);
        applied
    }

    fn click(&mut self, id: egui::Id) -> Applied {
        let at = self.rect(id).center();
        self.click_at(at, egui::Modifiers::NONE)
    }

    /// Two clicks at `at` inside egui's double-click window; answers what
    /// the second (the double-click) asked for.
    fn double_click_at(&mut self, at: egui::Pos2) -> Applied {
        // A pause first: a click shortly before would make this pair a
        // triple-click, which egui does not report as a double-click.
        self.t += 1.0;
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(
            vec![egui::Event::PointerMoved(at), button(true), button(false)],
            egui::Modifiers::NONE,
        );
        let applied = self.frame(vec![button(true), button(false)], egui::Modifiers::NONE);
        self.frame(Vec::new(), egui::Modifiers::NONE);
        applied
    }

    fn right_click(&mut self, id: egui::Id) {
        let at = self.rect(id).center();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(
            vec![egui::Event::PointerMoved(at), button(true), button(false)],
            egui::Modifiers::NONE,
        );
        self.frame(Vec::new(), egui::Modifiers::NONE);
    }

    /// Press at `from`, move in steps to `to`, release there.
    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let none = egui::Modifiers::NONE;
        self.frame(vec![egui::Event::PointerMoved(from)], none);
        self.frame(vec![button(from, true)], none);
        for k in 1..=6 {
            let f = k as f32 / 6.0;
            let at = from + (to - from) * f + egui::vec2(0.0, if k == 1 { 12.0 } else { 0.0 });
            self.frame(vec![egui::Event::PointerMoved(at)], none);
        }
        self.frame(vec![egui::Event::PointerMoved(to)], none);
        self.frame(vec![button(to, false)], none);
        self.frame(Vec::new(), none);
    }

    fn depth(&self) -> usize {
        self.ed.active().unwrap().history.undo_depth()
    }

    fn layer(&self, id: LayerId) -> &Layer {
        self.ed.active().unwrap().document.layers.get(id).unwrap()
    }

    fn has_layer(&self, id: LayerId) -> bool {
        self.ed.active().unwrap().document.layers.contains(id)
    }

    /// A raster layer called `name` with an opaque block at (x, y, w, h),
    /// created on top and made active.
    fn block(&mut self, name: &str, (x, y, w, h): (u32, u32, u32, u32)) -> LayerId {
        let layer = Layer::raster(name);
        let id = layer.id;
        self.ed.apply_command(Command::create_layer(layer));
        let mut rgba = vec![0u8; (W * H * 4) as usize];
        for yy in y..y + h {
            for xx in x..x + w {
                let i = ((yy * W + xx) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[10, 20, 30, 255]);
            }
        }
        let paint = {
            let doc = self.ed.active_mut().unwrap();
            crate::menu_bridge::pixels::write_layer(doc, id, &rgba, name).unwrap()
        };
        self.ed.apply_command(paint);
        self.ed.set_active_layer(id);
        self.settle();
        id
    }

    /// A layer carrying a drop shadow and a 5 px stroke.
    fn styled(&mut self) -> LayerId {
        let id = self.block("Styled", (4, 4, 8, 8));
        let mut effects = self.layer(id).effects.clone();
        effects.drop_shadow = Some(ShadowEffect::default());
        effects.stroke = Some(StrokeEffect {
            size_px: 5.0,
            ..StrokeEffect::default()
        });
        self.ed.apply_command(Command::SetLayerProperties {
            layer_id: id,
            patch: editor_core::LayerPatch {
                effects: Some(Box::new(effects)),
                ..Default::default()
            },
        });
        self.settle();
        id
    }

    /// A point on a row away from its eye, thumbnail and name.
    fn row_body(&self, id: LayerId) -> egui::Pos2 {
        let row = self.rect(ui::view::ids::layer_row(id));
        egui::pos2(row.left() + row.width() * 0.7, row.center().y)
    }

    fn trash(&self) -> egui::Pos2 {
        self.rect(ui::view::ids::layer_delete()).center()
    }

    fn open_options(&mut self) {
        self.click(ids::options_button());
        assert!(
            self.drawn(ids::options_item(OptionsItem::AddCopy)),
            "the options menu opened"
        );
    }
}

/// Every textured mesh a frame painted, as (texture, uv bounds).
fn textured_meshes(full: &egui::FullOutput) -> Vec<(egui::TextureId, egui::Rect)> {
    fn walk(shape: &egui::Shape, out: &mut Vec<(egui::TextureId, egui::Rect)>) {
        match shape {
            egui::Shape::Mesh(mesh) => {
                let mut uv = egui::Rect::NOTHING;
                for v in &mesh.vertices {
                    uv.extend_with(v.uv);
                }
                out.push((mesh.texture_id, uv));
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &full.shapes {
        walk(&clipped.shape, &mut out);
    }
    out
}

fn close_to(a: egui::Rect, b: egui::Rect) -> bool {
    (a.min - b.min).length() < 1e-4 && (a.max - b.max).length() < 1e-4
}

// ---------------------------------------------------------------------------
// Double-clicks
// ---------------------------------------------------------------------------

/// A double-click on a raster layer's row (away from its name, which
/// renames) opens Layer Style on its Blending Options page, as Photopea's
/// does.
#[test]
fn a_double_click_on_a_layer_row_opens_layer_style() {
    let mut rig = Rig::new();
    let id = rig.block("Ink", (10, 6, 20, 12));
    assert!(!rig.chrome.dialogs_for_test().layer_style_is_open_for_test());
    let at = rig.row_body(id);
    rig.double_click_at(at);
    assert!(
        rig.chrome.dialogs_for_test().layer_style_is_open_for_test(),
        "the double-click opened Layer Style"
    );
    let dialog = rig
        .chrome
        .dialogs_for_test()
        .active_layer_style_dialog_for_test();
    assert_eq!(dialog.layer(), id);
    assert_eq!(dialog.page(), StylePage::Blending);
}

/// A double-click on a smart object's thumbnail opens its contents (a new
/// document tab), and on a fill layer's thumbnail its fill dialog.
#[test]
fn a_double_click_on_a_thumbnail_opens_the_layers_own_editor() {
    let mut rig = Rig::new();
    let ink = rig.block("Ink", (10, 6, 20, 12));
    crate::menu_bridge::perform(MenuAction::ConvertToSmartObject, &mut rig.ed).unwrap();
    rig.settle();
    let doc = &rig.ed.active().unwrap().document;
    let so = doc
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|l| {
            matches!(
                doc.layers.get(*l).map(|x| &x.kind),
                Some(LayerKind::SmartObject(_))
            )
        })
        .expect("Ink became a smart object");
    let _ = ink;
    let docs = rig.ed.documents().len();
    let thumb = rig.rect(ui::view::ids::layer_content_thumb(so)).center();
    let applied = rig.double_click_at(thumb);
    assert!(
        applied.menu.contains(&MenuAction::EditSmartObjectContents),
        "the double-click asked for Edit Contents: {:?}",
        applied.menu
    );
    assert_eq!(
        rig.ed.documents().len(),
        docs + 1,
        "the contents opened in their own tab"
    );

    // Back to the first document, and a solid-colour fill layer on it.
    let mut rig = Rig::new();
    let fill = Layer::with_kind(
        "Fill",
        LayerKind::Fill(layer_model::FillLayer::solid([1.0, 0.0, 0.0, 1.0])),
    );
    let fill_id = fill.id;
    rig.ed.apply_command(Command::create_layer(fill));
    rig.ed.set_active_layer(fill_id);
    rig.settle();
    let thumb = rig
        .rect(ui::view::ids::layer_content_thumb(fill_id))
        .center();
    rig.double_click_at(thumb);
    assert!(
        matches!(
            rig.chrome.dialogs_for_test().active_for_test(),
            crate::dialog_host::ActiveDialog::FillLayer(_)
        ),
        "the fill layer's own dialog opened"
    );
}

// ---------------------------------------------------------------------------
// The effects list
// ---------------------------------------------------------------------------

/// A styled layer lists "Effects" and one row per effect under its row, in
/// the Layer Style dialog's order. Each effect's eye switches that effect
/// off and back on with its parameters, one undo step each; the "Effects"
/// eye switches the whole style; a double-click on an effect opens Layer
/// Style on that effect's page; the fx toggle folds the list away.
#[test]
fn the_effects_list_has_an_eye_per_effect_and_opens_layer_style_on_its_page() {
    let mut rig = Rig::new();
    let id = rig.styled();
    let row = rig.rect(ui::view::ids::layer_row(id));
    let header = rig.rect(ids::effects_row(id));
    let stroke = rig.rect(ids::effect_row(id, EffectSlot::Stroke));
    let shadow = rig.rect(ids::effect_row(id, EffectSlot::DropShadow));
    assert!(
        row.bottom() <= header.top() + 0.5,
        "the list hangs under the row"
    );
    assert!(header.bottom() <= stroke.top() + 0.5);
    assert!(
        stroke.bottom() <= shadow.top() + 0.5,
        "Stroke is listed above Drop Shadow, the dialog's order"
    );

    // The Stroke eye: off, one step, the row stays with its eye off.
    let before = rig.depth();
    rig.click(ids::effect_eye(id, EffectSlot::Stroke));
    assert_eq!(rig.depth(), before + 1, "one undo step");
    assert!(rig.layer(id).effects.stroke.is_none(), "the stroke is off");
    assert!(rig.layer(id).effects.drop_shadow.is_some());
    assert!(
        rig.drawn(ids::effect_row(id, EffectSlot::Stroke)),
        "the hidden effect keeps its row"
    );
    // On again: the same 5 px stroke comes back, one more step.
    rig.click(ids::effect_eye(id, EffectSlot::Stroke));
    assert_eq!(rig.depth(), before + 2);
    assert_eq!(
        rig.layer(id).effects.stroke.as_ref().map(|s| s.size_px),
        Some(5.0),
        "the eye brought back the parameters it hid"
    );

    // The "Effects" eye is the whole style.
    rig.click(ids::effects_eye(id));
    assert!(!rig.layer(id).effects.enabled, "the style is switched off");
    rig.click(ids::effects_eye(id));
    assert!(rig.layer(id).effects.enabled);

    // A double-click on the Stroke row opens Layer Style on the Stroke page.
    let stroke = rig.rect(ids::effect_row(id, EffectSlot::Stroke));
    rig.double_click_at(egui::pos2(stroke.right() - 10.0, stroke.center().y));
    assert!(rig.chrome.dialogs_for_test().layer_style_is_open_for_test());
    assert_eq!(
        rig.chrome
            .dialogs_for_test()
            .active_layer_style_dialog_for_test()
            .page(),
        StylePage::Effect(EffectKind::Stroke),
        "the dialog opened on the effect's own page"
    );
    rig.chrome.dialogs_for_test().close();
    rig.settle();

    // The fx toggle folds the list away and back.
    rig.click(ids::fx_toggle(id));
    assert!(!rig.drawn(ids::effects_row(id)), "folded");
    assert!(!rig.drawn(ids::effect_row(id, EffectSlot::Stroke)));
    rig.click(ids::fx_toggle(id));
    assert!(
        rig.drawn(ids::effect_row(id, EffectSlot::Stroke)),
        "unfolded"
    );
}

// ---------------------------------------------------------------------------
// The trash
// ---------------------------------------------------------------------------

/// Dragged onto the footer's trash: an effect's row deletes that effect,
/// the "Effects" row clears the style, and a layer's row deletes the layer
/// — each one undo step.
#[test]
fn rows_dragged_onto_the_trash_are_deleted() {
    let mut rig = Rig::new();
    let id = rig.styled();
    let trash = rig.trash();

    let before = rig.depth();
    let stroke = rig.rect(ids::effect_row(id, EffectSlot::Stroke));
    rig.drag(egui::pos2(stroke.right() - 10.0, stroke.center().y), trash);
    rig.settle();
    assert_eq!(rig.depth(), before + 1, "one undo step");
    assert!(
        rig.layer(id).effects.stroke.is_none(),
        "the stroke is deleted"
    );
    assert!(
        rig.layer(id).effects.drop_shadow.is_some(),
        "only the stroke"
    );
    assert!(
        !rig.drawn(ids::effect_row(id, EffectSlot::Stroke)),
        "a deleted effect has no row (unlike a hidden one)"
    );

    let header = rig.rect(ids::effects_row(id));
    rig.drag(egui::pos2(header.right() - 10.0, header.center().y), trash);
    rig.settle();
    assert_eq!(rig.depth(), before + 2);
    assert_eq!(
        rig.layer(id).effects,
        layer_model::LayerEffects::default(),
        "the style is cleared"
    );
    assert!(!rig.drawn(ids::effects_row(id)));

    let at = rig.row_body(id);
    rig.drag(at, trash);
    assert_eq!(rig.depth(), before + 3);
    assert!(!rig.has_layer(id), "the layer is deleted");
}

// ---------------------------------------------------------------------------
// Alt-click solo
// ---------------------------------------------------------------------------

/// Alt-click on a layer's eye hides every other layer (one undo step);
/// Alt-clicking it again shows them again.
#[test]
fn alt_click_on_an_eye_shows_only_that_layer() {
    let mut rig = Rig::new();
    let a = rig.block("A", (0, 0, 8, 8));
    let b = rig.block("B", (8, 8, 8, 8));
    let background = *rig
        .ed
        .active()
        .unwrap()
        .document
        .layers
        .root()
        .last()
        .unwrap();
    let before = rig.depth();
    let eye = rig.rect(ui::view::ids::layer_eye(a)).center();
    rig.click_at(eye, egui::Modifiers::ALT);
    assert_eq!(rig.depth(), before + 1, "one undo step");
    assert!(rig.layer(a).visible, "the soloed layer shows");
    assert!(!rig.layer(b).visible, "the others are hidden");
    assert!(!rig.layer(background).visible);

    let eye = rig.rect(ui::view::ids::layer_eye(a)).center();
    rig.click_at(eye, egui::Modifiers::ALT);
    assert_eq!(rig.depth(), before + 2);
    assert!(
        rig.layer(b).visible && rig.layer(background).visible,
        "shown again"
    );

    // A plain click still just flips the one eye.
    let eye = rig.rect(ui::view::ids::layer_eye(b)).center();
    rig.click_at(eye, egui::Modifiers::NONE);
    assert!(!rig.layer(b).visible);
    assert!(rig.layer(a).visible && rig.layer(background).visible);
}

// ---------------------------------------------------------------------------
// The row menu
// ---------------------------------------------------------------------------

/// A right-click on a row opens Photopea's row menu — its rows, order and
/// separators. Its Duplicate Layer copies at once (no dialog), named
/// "<name> copy"; with the panel option "Add "copy" to copied layers"
/// switched off the copy keeps its source's name, and the option is stored.
#[test]
fn the_row_menu_is_photopeas_and_duplicate_layer_follows_the_copy_option() {
    let mut rig = Rig::new();
    let id = rig.block("Ink", (10, 6, 20, 12));
    rig.right_click(ui::view::ids::layer_row(id));
    let menu_ctx = crate::menu_bridge::context(&mut rig.ed, rig.chrome.workspace());
    let items = ui::context_menu::layer_items(&menu_ctx);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        &labels[..15],
        &[
            "Blending Options…",
            "Select Pixels",
            "Duplicate Layer",
            "Duplicate Into…",
            "Delete",
            "Convert to Smart Object",
            "Rasterize",
            "Rasterize Layer Style",
            "Convert to Shape",
            "Create Clipping Mask",
            "Copy Layer Style",
            "Paste Layer Style",
            "Clear Layer Style",
            "Merge Down",
            "Flatten Image",
        ]
    );
    assert_eq!(labels[15], "No Color", "the colour labels close the menu");
    let separators: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.separator_after)
        .map(|(n, _)| n)
        .collect();
    assert_eq!(separators, vec![1, 4, 8, 14], "Photopea's separators");

    let count = rig.ed.active().unwrap().document.layers.len();
    let applied = rig.click(ui::context_menu::ids::context_item(2));
    assert!(
        applied.actions.contains(&Action::DuplicateLayer),
        "Duplicate Layer asked for the dialog-free copy: {:?}",
        applied.actions
    );
    assert!(!rig.chrome.dialogs_for_test().is_open(), "no dialog");
    assert_eq!(rig.ed.active().unwrap().document.layers.len(), count + 1);
    let copy = rig.ed.active().unwrap().document.active_layer().unwrap();
    assert_eq!(rig.layer(copy).name, "Ink copy");

    // Duplicate Into… is the dialog with the destination document.
    rig.right_click(ui::view::ids::layer_row(copy));
    rig.click(ui::context_menu::ids::context_item(3));
    assert!(
        matches!(
            rig.chrome.dialogs_for_test().active_for_test(),
            crate::dialog_host::ActiveDialog::DuplicateLayer(_)
        ),
        "Duplicate Into… opened the Duplicate Layer dialog"
    );
    rig.chrome.dialogs_for_test().close();
    rig.settle();

    // The panel option off: the copy keeps its name, and it is stored.
    rig.open_options();
    rig.click(ids::options_item(OptionsItem::AddCopy));
    rig.settle();
    assert!(!rig.ed.preferences().layers_panel.add_copy, "stored");
    rig.right_click(ui::view::ids::layer_row(copy));
    rig.click(ui::context_menu::ids::context_item(2));
    let second = rig.ed.active().unwrap().document.active_layer().unwrap();
    assert_ne!(second, copy);
    assert_eq!(rig.layer(second).name, "Ink copy", "no second \" copy\"");
}

// ---------------------------------------------------------------------------
// Panel options: thumbnails
// ---------------------------------------------------------------------------

/// The panel menu's "− Thumbnail Size" walks down to no thumbnails at all
/// and "+ Thumbnail Size" brings them back; Thumbnails by Layer crops a
/// thumbnail to its layer's bounds (the painted image samples exactly that
/// part of the document thumbnail). The options are stored, and a new
/// chrome starts with them.
#[test]
fn the_panel_menu_sizes_thumbnails_down_to_none_and_crops_them_by_layer() {
    let mut rig = Rig::new();
    let id = rig.block("Ink", (10, 6, 20, 12));
    assert!(rig.drawn(ui::view::ids::layer_content_thumb(id)));
    assert_eq!(
        rig.chrome.workspace().layers.thumb_scale,
        ThumbScale::Regular
    );
    for _ in 0..2 {
        rig.open_options();
        rig.click(ids::options_item(OptionsItem::Smaller));
    }
    rig.settle();
    assert_eq!(rig.chrome.workspace().layers.thumb_scale, ThumbScale::None);
    assert!(
        !rig.drawn(ui::view::ids::layer_content_thumb(id)),
        "no thumbnail at the smallest size"
    );
    assert_eq!(rig.ed.preferences().layers_panel.thumbnail_size, "none");
    rig.open_options();
    rig.click(ids::options_item(OptionsItem::Larger));
    rig.settle();
    assert!(rig.drawn(ui::view::ids::layer_content_thumb(id)), "back");

    // Thumbnails by Layer: the crop is the block's rectangle of the canvas.
    let expected = egui::Rect::from_min_max(
        egui::pos2(10.0 / 64.0, 6.0 / 64.0),
        egui::pos2(30.0 / 64.0, 18.0 / 64.0),
    );
    rig.open_options();
    rig.click(ids::options_item(OptionsItem::ByLayer));
    rig.settle();
    assert!(
        rig.ed.preferences().layers_panel.thumbnails_by_layer,
        "stored"
    );
    let crop = rig
        .chrome
        .workspace()
        .layers
        .thumb_crop(id)
        .expect("the application published the layer's crop");
    assert!(close_to(crop, expected), "{crop:?} vs {expected:?}");
    let tex = rig.chrome.workspace().layer_thumbs[&id].id();
    let applied = rig.frame(Vec::new(), egui::Modifiers::NONE);
    let meshes = textured_meshes(applied.full.as_ref().unwrap());
    assert!(
        meshes
            .iter()
            .any(|(t, uv)| *t == tex && close_to(*uv, expected)),
        "the thumbnail is painted from the layer's crop: {meshes:?}"
    );

    // By Document again: the whole document thumbnail.
    rig.open_options();
    rig.click(ids::options_item(OptionsItem::ByDocument));
    let applied = rig.frame(Vec::new(), egui::Modifiers::NONE);
    let whole = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    assert!(textured_meshes(applied.full.as_ref().unwrap())
        .iter()
        .any(|(t, uv)| *t == tex && close_to(*uv, whole)));

    // A new chrome (a restart) takes the stored options.
    rig.open_options();
    rig.click(ids::options_item(OptionsItem::ByLayer));
    rig.settle();
    rig.chrome = Chrome::new();
    rig.open_layers_panel();
    let layers = &rig.chrome.workspace().layers;
    assert!(layers.thumbs_by_layer, "Thumbnails by Layer survived");
    assert_eq!(layers.thumb_scale, ThumbScale::Small);
}
