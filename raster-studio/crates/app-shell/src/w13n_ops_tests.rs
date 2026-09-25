//! W13-N: every row of this wave driven through the application's real
//! routes — the menu bar's click handler (`Chrome::menu_click`), the chrome's
//! own frame (which draws the docks, the menu bar and this module's windows),
//! and `menu_bridge::perform`, the call the shell makes for every
//! `ChromeOutput::menu` pick.

use std::path::{Path, PathBuf};

use editor_core::{Command, Selection};
use glam::IVec2;
use ui::menu::MenuAction;
use ui::PanelId;

use super::*;
use crate::chrome::{install_theme, Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor(dir: &Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new().exporting_folder(dir.join("out"))),
    )
}

fn png(dir: &Path, name: &str, w: u32, h: u32, px: impl Fn(u32, u32) -> [u8; 4]) -> PathBuf {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend(px(x, y));
        }
    }
    let path = dir.join(name);
    std::fs::write(
        &path,
        raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap(),
    )
    .unwrap();
    path
}

const W: u32 = 120;
const H: u32 = 90;

fn inside_disc(x: u32, y: u32) -> bool {
    (x as f32 + 0.5 - 60.0).powi(2) + (y as f32 + 0.5 - 45.0).powi(2) < 25.0 * 25.0
}

/// A textured orange disc on a textured teal ground.
fn disc(x: u32, y: u32) -> [u8; 4] {
    let t = ((x * 7 + y * 13) % 11) as i32 * 3 - 15;
    let c: [i32; 3] = if inside_disc(x, y) {
        [220 + t / 2, 130 + t, 40]
    } else {
        [40, 120 + t, 150 - t]
    };
    let c = c.map(|v| v.clamp(0, 255) as u8);
    [c[0], c[1], c[2], 255]
}

/// The headless window: the chrome drawn over the editor, frame by frame.
struct Window {
    ctx: egui::Context,
    chrome: Chrome,
}

impl Window {
    fn new() -> Self {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        Self {
            ctx,
            chrome: Chrome::new(),
        }
    }

    fn frame(&mut self, ed: &mut Editor, events: Vec<egui::Event>) -> ChromeOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        let mut out = ChromeOutput::default();
        let chrome = &mut self.chrome;
        let _ = self.ctx.run(input, |ctx| out = chrome.ui(ctx, ed));
        out
    }

    /// A frame's menu picks performed the way the shell performs them.
    fn perform(out: ChromeOutput, ed: &mut Editor) -> Vec<(MenuAction, Result<String, String>)> {
        out.menu
            .into_iter()
            .map(|a| (a, crate::menu_bridge::perform(a, ed)))
            .collect()
    }

    fn settle(&mut self, ed: &mut Editor) {
        for _ in 0..3 {
            let out = self.frame(ed, Vec::new());
            Self::perform(out, ed);
        }
    }

    /// Click `action`'s menu row: the menu bar's own click handler, then the
    /// shell's perform of what it recorded.
    fn menu_click(
        &mut self,
        ed: &mut Editor,
        action: MenuAction,
    ) -> Vec<(MenuAction, Result<String, String>)> {
        let context = crate::menu_bridge::context(ed, self.chrome.workspace());
        let intent = crate::menu_bridge::resolve_intent(action, &context, ed)
            .unwrap_or_else(|r| panic!("{action:?} is disabled: {r}"));
        let mut out = ChromeOutput::default();
        self.chrome.menu_click(intent, ed, &mut out);
        Self::perform(out, ed)
    }

    fn rect(&self, id: egui::Id) -> Option<egui::Rect> {
        self.ctx.read_response(id).map(|r| r.rect)
    }

    /// Press at `from`, drag through `path`, release.
    fn drag(&mut self, ed: &mut Editor, from: egui::Pos2, path: &[egui::Pos2]) {
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        self.frame(
            ed,
            vec![egui::Event::PointerMoved(from), button(from, true)],
        );
        for p in path {
            self.frame(ed, vec![egui::Event::PointerMoved(*p)]);
        }
        let end = *path.last().unwrap_or(&from);
        self.frame(ed, vec![button(end, false)]);
    }

    fn click(&mut self, ed: &mut Editor, at: egui::Pos2) -> ChromeOutput {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        self.frame(
            ed,
            vec![egui::Event::PointerMoved(at), button(true), button(false)],
        )
    }

    fn press_enter(&mut self, ed: &mut Editor) -> ChromeOutput {
        self.frame(
            ed,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        )
    }
}

fn history_depth(ed: &Editor) -> usize {
    ed.active().unwrap().history_depth()
}

// ---------------------------------------------------------------------------
// Styles panel
// ---------------------------------------------------------------------------

/// A style saved from one layer is listed as a swatch in the Styles panel
/// the chrome draws; a real click on it applies the style to the active
/// layer as one undo step.
#[test]
fn a_click_on_a_styles_swatch_applies_the_style_as_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.open_path(&png(dir.path(), "a.png", 64, 48, |_, _| [90, 90, 90, 255]))
        .unwrap();
    let styled = ed.active().unwrap().document.active_layer().unwrap();
    let effects = layer_model::LayerEffects {
        drop_shadow: Some(layer_model::ShadowEffect::default()),
        ..layer_model::LayerEffects::default()
    };
    ed.apply_command(Command::SetLayerProperties {
        layer_id: styled,
        patch: editor_core::LayerPatch {
            effects: Some(Box::new(effects.clone())),
            ..Default::default()
        },
    });
    ed.define_style_preset().unwrap();
    let plain = layer_model::Layer::raster("Plain");
    let plain_id = plain.id;
    ed.apply_command(Command::create_layer(plain));
    ed.set_active_layer(plain_id);

    let mut win = Window::new();
    win.chrome
        .workspace_for_test()
        .dock
        .set_open(PanelId::Styles, true);
    win.chrome.workspace_for_test().dock.raise(PanelId::Styles);
    win.settle(&mut ed);
    let tile = win
        .rect(ui::panels::styles::ids::tile(0))
        .expect("the saved style is a swatch in the Styles panel");
    let depth = history_depth(&ed);
    let out = win.click(&mut ed, tile.center());
    let done = Window::perform(out, &mut ed);
    assert!(
        done.iter()
            .any(|(a, r)| *a == MenuAction::ApplyStyleAt(0) && r.is_ok()),
        "the click applied style 0: {done:?}"
    );
    let layer = |ed: &Editor| {
        ed.active()
            .unwrap()
            .document
            .layers
            .get(plain_id)
            .unwrap()
            .effects
            .clone()
    };
    assert_eq!(layer(&ed), effects);
    assert_eq!(history_depth(&ed), depth + 1, "one undo step");
    ed.dispatch(crate::action::Action::Undo).unwrap();
    assert!(layer(&ed).is_default());
}

// ---------------------------------------------------------------------------
// Magic Cut
// ---------------------------------------------------------------------------

fn disc_editor(dir: &Path) -> Editor {
    let mut ed = editor(dir);
    ed.open_path(&png(dir, "disc.png", W, H, disc)).unwrap();
    ed
}

fn iou(sel: &Selection) -> f64 {
    let (mut inter, mut union) = (0u32, 0u32);
    for y in 0..H {
        for x in 0..W {
            let a = sel.coverage_at(IVec2::new(x as i32, y as i32)) >= 0.5;
            let b = inside_disc(x, y);
            inter += u32::from(a && b);
            union += u32::from(a || b);
        }
    }
    f64::from(inter) / f64::from(union)
}

/// Select > Magic Cut…: the row opens the window over the active layer, a
/// real drag on its pane paints a foreground stroke over the disc, the brush
/// toggle and a second drag paint the background, and Enter lands the cut as
/// the selection — one undo step — through the menu row's own road.
#[test]
fn magic_cut_strokes_painted_on_its_pane_select_the_disc() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = disc_editor(dir.path());
    let mut win = Window::new();
    win.settle(&mut ed);
    let done = win.menu_click(&mut ed, MenuAction::MagicCut);
    assert!(
        matches!(done.as_slice(), [(MenuAction::MagicCut, Ok(_))]),
        "{done:?}"
    );
    assert_eq!(open_window(), Some("Magic Cut"));
    win.settle(&mut ed);
    let pane = win
        .rect(ui::dialogs::magic_cut::ids::pane())
        .expect("the Magic Cut pane is drawn");
    let scale = pane.width() / W as f32;
    let at = |x: f32, y: f32| pane.min + egui::vec2(x * scale, y * scale);
    win.drag(&mut ed, at(50.0, 45.0), &[at(60.0, 45.0), at(70.0, 45.0)]);
    let toggle = win
        .rect(ui::dialogs::magic_cut::ids::brush_toggle())
        .expect("the brush toggle is drawn");
    win.click(&mut ed, toggle.center());
    win.drag(
        &mut ed,
        at(5.0, 5.0),
        &[at(60.0, 5.0), at(115.0, 5.0), at(115.0, 85.0)],
    );
    let strokes =
        with_magic_cut(|d| d.strokes().iter().map(|s| s.foreground).collect::<Vec<_>>()).unwrap();
    assert_eq!(strokes, vec![true, false], "one stroke of each brush");

    let depth = history_depth(&ed);
    let out = win.press_enter(&mut ed);
    let done = Window::perform(out, &mut ed);
    assert!(
        matches!(done.as_slice(), [(MenuAction::MagicCut, Ok(_))]),
        "Enter clicked the Magic Cut row with the cut parked: {done:?}"
    );
    assert_eq!(open_window(), None, "the window closed");
    let sel = ed.active().unwrap().document.selection.clone();
    assert!(
        iou(&sel) > 0.9,
        "the cut matches the disc only {:.3}",
        iou(&sel)
    );
    assert_eq!(history_depth(&ed), depth + 1, "one undo step");
    ed.dispatch(crate::action::Action::Undo).unwrap();
    assert!(ed.active().unwrap().document.selection.is_none());
}

fn cut_with(win: &mut Window, ed: &mut Editor, output: ui::dialogs::magic_cut::MagicCutOutput) {
    use ui::dialogs::magic_cut::MagicCutStroke;
    win.menu_click(ed, MenuAction::MagicCut);
    with_magic_cut(|d| {
        d.add_stroke(MagicCutStroke {
            foreground: true,
            radius: 4.0,
            points: vec![[50.0, 45.0], [70.0, 45.0]],
        });
        d.add_stroke(MagicCutStroke {
            foreground: false,
            radius: 4.0,
            points: vec![[5.0, 5.0], [115.0, 5.0], [115.0, 85.0]],
        });
        d.set_output(output);
    })
    .expect("the window opened");
    win.settle(ed);
    let out = win.press_enter(ed);
    let done = Window::perform(out, ed);
    assert!(
        matches!(done.as_slice(), [(MenuAction::MagicCut, Ok(_))]),
        "{done:?}"
    );
}

/// The Layer Mask and New Layer outputs: each ONE step, and the selection
/// the user had is left alone.
#[test]
fn magic_cut_masks_the_layer_or_copies_the_cut_to_a_new_layer() {
    use ui::dialogs::magic_cut::MagicCutOutput;
    let dir = tempfile::tempdir().unwrap();
    let mut ed = disc_editor(dir.path());
    let mut win = Window::new();
    win.settle(&mut ed);
    let layer = ed.active().unwrap().document.active_layer().unwrap();

    let depth = history_depth(&ed);
    cut_with(&mut win, &mut ed, MagicCutOutput::LayerMask);
    assert_eq!(history_depth(&ed), depth + 1);
    let doc = &ed.active().unwrap().document;
    assert!(
        doc.layers.get(layer).unwrap().mask.is_some(),
        "the layer has a mask"
    );
    assert!(doc.selection.is_none(), "the selection is untouched");
    ed.dispatch(crate::action::Action::Undo).unwrap();
    assert!(ed
        .active()
        .unwrap()
        .document
        .layers
        .get(layer)
        .unwrap()
        .mask
        .is_none());

    let before = ed.active().unwrap().document.layers.len();
    cut_with(&mut win, &mut ed, MagicCutOutput::NewLayer);
    assert_eq!(
        history_depth(&ed),
        depth + 1,
        "the mask was undone; one step"
    );
    let doc = ed.active().unwrap();
    assert_eq!(doc.document.layers.len(), before + 1, "a new layer");
    let copied = doc.document.active_layer().unwrap();
    assert_ne!(copied, layer);
    let px = super::super::pixels::read_layer(doc, copied);
    let alpha = |x: u32, y: u32| px[((y * W + x) * 4 + 3) as usize];
    assert_eq!(alpha(60, 45), 255, "the disc's centre was copied");
    assert_eq!(alpha(3, 3), 0, "the ground was not");
}

// ---------------------------------------------------------------------------
// Merge Channels
// ---------------------------------------------------------------------------

/// Image > Merge Channels…: the row opens a window asking which open
/// grayscale document each channel comes from; Enter builds the new RGB
/// document from those picks. The row is greyed with fewer than three
/// documents open.
#[test]
fn merge_channels_builds_an_rgb_document_from_three_grayscale_ones() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let mut win = Window::new();
    for (i, v) in [200u8, 100, 30].into_iter().enumerate() {
        ed.open_path(&png(dir.path(), &format!("g{i}.png"), 8, 6, |_, _| {
            [v, v, v, 255]
        }))
        .unwrap();
        ed.active_mut().unwrap().document.meta.color_mode = ui::menu::ColorMode::Grayscale as u8;
        if i < 2 {
            let context = crate::menu_bridge::context(&mut ed, win.chrome.workspace());
            assert!(
                crate::menu_bridge::resolve_intent(MenuAction::MergeChannels, &context, &ed)
                    .is_err(),
                "greyed with {} document(s)",
                i + 1
            );
        }
    }
    let done = win.menu_click(&mut ed, MenuAction::MergeChannels);
    assert!(matches!(done.as_slice(), [(_, Ok(_))]), "{done:?}");
    assert_eq!(open_window(), Some("Merge Channels"));
    assert_eq!(ed.documents().len(), 3, "the row only asked");
    // Red from g2, green from g0, blue from g1.
    with_merge_channels(|d| {
        assert_eq!(d.candidates().len(), 3);
        d.set_source(0, 2);
        d.set_source(1, 0);
        d.set_source(2, 1);
    })
    .unwrap();
    win.settle(&mut ed);
    let out = win.press_enter(&mut ed);
    let done = Window::perform(out, &mut ed);
    assert!(
        matches!(done.as_slice(), [(MenuAction::MergeChannels, Ok(_))]),
        "{done:?}"
    );
    assert_eq!(open_window(), None);
    assert_eq!(ed.documents().len(), 4);
    let index = ed.documents().len() - 1;
    let (w, h, rgba) = composite_of(&mut ed, index).unwrap();
    assert_eq!((w, h), (8, 6));
    assert_eq!(
        &rgba[..4],
        &[30, 200, 100, 255],
        "each channel from its pick"
    );
}

// ---------------------------------------------------------------------------
// PDF Presentation
// ---------------------------------------------------------------------------

/// File > Automate > PDF Presentation…: every open document is one page of
/// a well-formed PDF in the chosen folder.
#[test]
fn pdf_presentation_writes_one_page_per_open_document() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("out")).unwrap();
    let mut ed = editor(dir.path());
    ed.open_path(&png(dir.path(), "a.png", 30, 20, |_, _| [255, 0, 0, 255]))
        .unwrap();
    ed.open_path(&png(dir.path(), "b.png", 10, 40, |_, _| [0, 0, 255, 255]))
        .unwrap();
    let mut win = Window::new();
    let done = win.menu_click(&mut ed, MenuAction::PdfPresentation);
    assert!(matches!(done.as_slice(), [(_, Ok(_))]), "{done:?}");
    let pdf = std::fs::read(dir.path().join("out").join("Presentation.pdf")).unwrap();
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.starts_with("%PDF-1.4"));
    assert!(text.contains("/Count 2"));
    assert_eq!(text.matches("/Type /Page ").count(), 2);
    assert!(text.contains("/MediaBox [0 0 30 20]"));
    assert!(text.contains("/MediaBox [0 0 10 40]"));
    // Every cross-reference entry points at its object.
    let xref = text.rfind("\nxref\n").unwrap() + 1;
    let entries: Vec<usize> = text[xref..]
        .lines()
        .skip(3)
        .take_while(|l| l.ends_with(" n "))
        .map(|l| l[..10].parse().unwrap())
        .collect();
    assert_eq!(
        entries.len(),
        8,
        "catalog, pages and three objects per page"
    );
    for (n, off) in entries.iter().enumerate() {
        assert!(
            pdf[*off..].starts_with(format!("{} 0 obj", n + 1).as_bytes()),
            "object {} is not at {off}",
            n + 1
        );
    }
}

// ---------------------------------------------------------------------------
// Resize Images / Generate Mockups
// ---------------------------------------------------------------------------

/// File > Automate > Resize Images…: the row opens the folder window, Enter
/// runs it through the row, and every image of the source folder is written
/// fitted into the box, in its own format.
#[test]
fn resize_images_fits_every_image_of_the_folder_into_the_box() {
    let dir = tempfile::tempdir().unwrap();
    let (src, dst) = (dir.path().join("in"), dir.path().join("resized"));
    std::fs::create_dir_all(&src).unwrap();
    png(&src, "wide.png", 400, 100, |x, _| [x as u8, 0, 0, 255]);
    png(&src, "small.png", 20, 30, |_, _| [0, 255, 0, 255]);
    std::fs::write(src.join("notes.txt"), "not an image").unwrap();
    let mut ed = editor(dir.path());
    let mut win = Window::new();
    let done = win.menu_click(&mut ed, MenuAction::ResizeImages);
    assert!(matches!(done.as_slice(), [(_, Ok(_))]), "{done:?}");
    assert_eq!(open_window(), Some("Resize Images"));
    with_folder_job(|d| {
        d.set_box(100, 100);
        d.set_folder(ui::dialogs::BatchFolder::Source, src.clone());
        d.set_folder(ui::dialogs::BatchFolder::Destination, dst.clone());
    })
    .unwrap();
    win.settle(&mut ed);
    let out = win.press_enter(&mut ed);
    let done = Window::perform(out, &mut ed);
    assert!(
        matches!(done.as_slice(), [(MenuAction::ResizeImages, Ok(_))]),
        "{done:?}"
    );
    let wide = raster::decode_path(&dst.join("wide.png")).unwrap();
    assert_eq!((wide.width, wide.height), (100, 25));
    let small = raster::decode_path(&dst.join("small.png")).unwrap();
    assert_eq!((small.width, small.height), (20, 30), "never enlarged");
    assert!(!dst.join("notes.txt").exists());
}

/// File > Automate > Generate Mockups…: the smart object shows each image
/// of the folder in turn and a PNG is written per image; the document ends
/// as it began.
#[test]
fn generate_mockups_exports_the_smart_object_showing_each_image() {
    let dir = tempfile::tempdir().unwrap();
    let (src, dst) = (dir.path().join("designs"), dir.path().join("mockups"));
    std::fs::create_dir_all(&src).unwrap();
    png(&src, "red.png", 16, 16, |_, _| [255, 0, 0, 255]);
    png(&src, "blue.png", 16, 16, |_, _| [0, 0, 255, 255]);
    let mut ed = editor(dir.path());
    ed.open_path(&png(dir.path(), "base.png", 16, 16, |_, _| {
        [0, 255, 0, 255]
    }))
    .unwrap();
    ed.convert_to_smart_object().unwrap();
    let object = ed.active().unwrap().document.layers.root()[0];
    ed.set_active_layer(object);
    let before = ed.active().unwrap().document.layers.clone();
    let mut win = Window::new();
    let done = win.menu_click(&mut ed, MenuAction::GenerateMockups);
    assert!(matches!(done.as_slice(), [(_, Ok(_))]), "{done:?}");
    assert_eq!(open_window(), Some("Generate Mockups"));
    with_folder_job(|d| {
        d.set_folder(ui::dialogs::BatchFolder::Source, src.clone());
        d.set_folder(ui::dialogs::BatchFolder::Destination, dst.clone());
    })
    .unwrap();
    win.settle(&mut ed);
    let out = win.press_enter(&mut ed);
    let done = Window::perform(out, &mut ed);
    assert!(
        matches!(done.as_slice(), [(MenuAction::GenerateMockups, Ok(_))]),
        "{done:?}"
    );
    let red = raster::decode_path(&dst.join("red.png")).unwrap();
    let blue = raster::decode_path(&dst.join("blue.png")).unwrap();
    assert_eq!(&red.rgba8[..3], &[255, 0, 0]);
    assert_eq!(&blue.rgba8[..3], &[0, 0, 255]);
    let after = &ed.active().unwrap().document.layers;
    assert_eq!(
        format!("{after:?}"),
        format!("{before:?}"),
        "the document ends as it began"
    );
}

// ---------------------------------------------------------------------------
// Crop and Straighten Photos
// ---------------------------------------------------------------------------

/// Is `(x, y)`'s centre inside the `w x h` rectangle centred at `c` and
/// turned by `deg` degrees?
fn in_rect(x: u32, y: u32, c: (f32, f32), w: f32, h: f32, deg: f32) -> bool {
    let (s, co) = deg.to_radians().sin_cos();
    let (dx, dy) = (x as f32 + 0.5 - c.0, y as f32 + 0.5 - c.1);
    let (u, v) = (dx * co + dy * s, -dx * s + dy * co);
    u.abs() <= w / 2.0 && v.abs() <= h / 2.0
}

fn scan(x: u32, y: u32) -> [u8; 4] {
    if in_rect(x, y, (130.0, 110.0), 120.0, 80.0, 10.0) {
        [200, 40, 40, 255]
    } else if in_rect(x, y, (310.0, 190.0), 70.0, 100.0, 0.0) {
        [40, 90, 200, 255]
    } else {
        [250, 250, 250, 255]
    }
}

#[test]
fn find_photos_straightens_and_crops_each_photo() {
    let (w, h) = (420u32, 320u32);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            rgba.extend(scan(x, y));
        }
    }
    let photos = find_photos(&rgba, w, h);
    assert_eq!(photos.len(), 2, "two photos on the scan");
    let near = |a: u32, b: f32| (a as f32 - b).abs() <= 3.0;
    let (a, b) = (&photos[0], &photos[1]);
    assert!(
        near(a.width, 118.0) && near(a.height, 78.0),
        "{}x{}",
        a.width,
        a.height
    );
    assert!(
        (a.angle_deg.abs() - 10.0).abs() < 1.5,
        "turned {}",
        a.angle_deg
    );
    assert!(
        near(b.width, 68.0) && near(b.height, 98.0),
        "{}x{}",
        b.width,
        b.height
    );
    // Straightened and cropped inside the edge: no background left in.
    for (photo, colour) in [(a, [200u8, 40, 40]), (b, [40, 90, 200])] {
        for y in 2..photo.height - 2 {
            for x in 2..photo.width - 2 {
                let i = ((y * photo.width + x) * 4) as usize;
                for (c, want) in colour.iter().enumerate() {
                    let d = (i32::from(photo.rgba[i + c]) - i32::from(*want)).abs();
                    assert!(d <= 12, "({x},{y}) is {:?}", &photo.rgba[i..i + 4]);
                }
            }
        }
    }
}

/// File > Automate > Crop and Straighten Photos opens each photo as its own
/// document.
#[test]
fn crop_and_straighten_opens_one_document_per_photo() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.open_path(&png(dir.path(), "scan.png", 420, 320, scan))
        .unwrap();
    let mut win = Window::new();
    let done = win.menu_click(&mut ed, MenuAction::CropAndStraightenPhotos);
    assert!(matches!(done.as_slice(), [(_, Ok(_))]), "{done:?}");
    assert_eq!(ed.documents().len(), 3);
    let titles: Vec<String> = ed.documents()[1..]
        .iter()
        .map(|d| d.title().to_string())
        .collect();
    assert!(
        titles[0].ends_with("Photo 1") && titles[1].ends_with("Photo 2"),
        "{titles:?}"
    );
}

// ---------------------------------------------------------------------------
// Convert to Point / Paragraph Text
// ---------------------------------------------------------------------------

/// Layer > Text > Convert to Point Text: a wrapping box becomes point text
/// with a line break where each line wrapped (one undo step), and Convert to
/// Paragraph Text boxes it again without re-wrapping.
#[test]
fn converting_text_between_point_and_paragraph_keeps_its_lines() {
    use layer_model::text::Frame;
    compositor::load_font(dejavu::sans::regular().to_vec());
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.open_path(&png(dir.path(), "t.png", 300, 200, |_, _| {
        [255, 255, 255, 255]
    }))
    .unwrap();
    let mut text = layer_model::TextLayer::legacy(
        "The quick brown fox jumps over the lazy dog again",
        "DejaVu Sans",
        20.0,
    );
    text.frame = Frame::Box {
        width: 120.0,
        height: None,
    };
    let layer = layer_model::Layer::with_kind("Copy", layer_model::LayerKind::Text(text));
    let id = layer.id;
    ed.apply_command(Command::create_layer(layer));
    ed.set_active_layer(id);
    let text_of = |ed: &Editor| match &ed.active().unwrap().document.layers.get(id).unwrap().kind {
        layer_model::LayerKind::Text(t) => t.clone(),
        _ => unreachable!(),
    };
    let lines = |t: &layer_model::TextLayer| {
        compositor::text_selection_rects(&text_engine::TextRun::from(t), 0, t.text.len()).len()
    };
    let wrapped = lines(&text_of(&ed));
    assert!(wrapped >= 3, "the box wraps into {wrapped} lines");

    let mut win = Window::new();
    let depth = history_depth(&ed);
    let done = win.menu_click(&mut ed, MenuAction::ConvertToPointText);
    assert!(matches!(done.as_slice(), [(_, Ok(_))]), "{done:?}");
    let point = text_of(&ed);
    assert_eq!(point.frame, Frame::Point);
    assert_eq!(point.text.matches('\n').count(), wrapped - 1);
    assert_eq!(lines(&point), wrapped, "the same lines");
    assert_eq!(history_depth(&ed), depth + 1);
    let again = win.menu_click(&mut ed, MenuAction::ConvertToPointText);
    assert!(
        matches!(again.as_slice(), [(_, Err(_))]),
        "already point text"
    );

    let done = win.menu_click(&mut ed, MenuAction::ConvertToParagraphText);
    assert!(matches!(done.as_slice(), [(_, Ok(_))]), "{done:?}");
    let para = text_of(&ed);
    assert!(matches!(para.frame, Frame::Box { .. }));
    assert_eq!(lines(&para), wrapped, "boxed without re-wrapping");
    ed.dispatch(crate::action::Action::Undo).unwrap();
    ed.dispatch(crate::action::Action::Undo).unwrap();
    assert_eq!(
        text_of(&ed).frame,
        Frame::Box {
            width: 120.0,
            height: None
        }
    );
}

// ---------------------------------------------------------------------------
// Round 2: the windows are modal to the keyboard, a cut lands only where it
// was painted, a broken preset does not shift the Styles panel
// ---------------------------------------------------------------------------

/// What the shell's `route_key` does with Ctrl+Z, asked with the keyboard
/// owner `Shell::keyboard_owner` builds from this egui context.
fn ctrl_z_outcome(ctx: &egui::Context) -> crate::shell::KeyOutcome {
    let owner = crate::shell::KeyboardOwner {
        egui_text_focus: ctx.wants_keyboard_input(),
        recording_shortcut: false,
    };
    crate::shell::route_key(
        owner,
        &winit::keyboard::Key::Character("z".into()),
        winit::event::ElementState::Pressed,
        false,
        winit::keyboard::ModifiersState::CONTROL,
    )
}

fn press_escape(win: &mut Window, ed: &mut Editor) -> ChromeOutput {
    win.frame(
        ed,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }],
    )
}

/// While Magic Cut or the Resize Images window is up, no keymap chord
/// reaches the document behind it: the chrome drawn frame by frame leaves
/// egui holding the keyboard, so `route_key` ignores Ctrl+Z. Escape closes
/// the window and gives the keyboard back.
#[test]
fn a_w13n_window_keeps_keymap_chords_off_the_document_behind_it() {
    use crate::shell::KeyOutcome;
    let dir = tempfile::tempdir().unwrap();
    let mut ed = disc_editor(dir.path());
    let mut win = Window::new();
    win.settle(&mut ed);
    assert!(
        matches!(ctrl_z_outcome(&win.ctx), KeyOutcome::Dispatch(_)),
        "with no window up, Ctrl+Z is the keymap's"
    );
    for row in [MenuAction::MagicCut, MenuAction::ResizeImages] {
        let done = win.menu_click(&mut ed, row);
        assert!(matches!(done.as_slice(), [(_, Ok(_))]), "{done:?}");
        win.settle(&mut ed);
        assert!(open_window().is_some(), "{row:?} opened its window");
        assert_eq!(
            ctrl_z_outcome(&win.ctx),
            KeyOutcome::Ignore,
            "{row:?}: a chord under the window must not reach the keymap"
        );
        let out = press_escape(&mut win, &mut ed);
        Window::perform(out, &mut ed);
        assert_eq!(open_window(), None, "Escape closed {row:?}");
        win.settle(&mut ed);
        assert!(
            matches!(ctrl_z_outcome(&win.ctx), KeyOutcome::Dispatch(_)),
            "{row:?}: the keyboard is the application's again"
        );
    }
    // Full Screen Mode draws no menu bar, so nothing would draw these
    // windows or hold the keyboard for them; the rows are still reachable
    // (Help > Search Commands). Opening must be refused, loudly.
    ed.dispatch(crate::action::Action::CycleScreenMode).unwrap();
    ed.dispatch(crate::action::Action::CycleScreenMode).unwrap();
    assert_eq!(ed.screen_mode(), ui::palette::ScreenMode::FullScreen);
    win.settle(&mut ed);
    for row in [
        MenuAction::MagicCut,
        MenuAction::ResizeImages,
        MenuAction::GenerateMockups,
        MenuAction::MergeChannels,
    ] {
        let done = perform(row, &mut ed);
        assert!(
            matches!(&done, Err(e) if e.contains("leave Full Screen Mode")),
            "{row:?} in Full Screen: {done:?}"
        );
        win.settle(&mut ed);
        assert_eq!(open_window(), None, "{row:?} opened no window");
        assert!(
            matches!(ctrl_z_outcome(&win.ctx), KeyOutcome::Dispatch(_)),
            "{row:?}: nothing is left half-open over the document"
        );
    }
    let done = win.menu_click(&mut ed, MenuAction::MagicCut);
    assert!(
        matches!(done.as_slice(), [(MenuAction::MagicCut, Err(e))] if e.contains("Full Screen")),
        "the menu route refuses too: {done:?}"
    );
    assert_eq!(open_window(), None);
    // Back in Standard mode the row opens its window as before.
    ed.dispatch(crate::action::Action::CycleScreenMode).unwrap();
    assert!(ed.screen_mode().menu_visible());
    let done = win.menu_click(&mut ed, MenuAction::MagicCut);
    assert!(matches!(done.as_slice(), [(_, Ok(_))]), "{done:?}");
    win.settle(&mut ed);
    assert_eq!(open_window(), Some("Magic Cut"));
    let out = press_escape(&mut win, &mut ed);
    Window::perform(out, &mut ed);
    assert_eq!(open_window(), None);
}

/// A cut painted over one document does not land on another of the same
/// size that became active while the window was up.
#[test]
fn a_magic_cut_lands_only_on_the_document_it_was_painted_over() {
    use ui::dialogs::magic_cut::MagicCutStroke;
    let dir = tempfile::tempdir().unwrap();
    let mut ed = disc_editor(dir.path());
    ed.open_path(&png(dir.path(), "other.png", W, H, disc))
        .unwrap();
    ed.activate(0).unwrap();
    let mut win = Window::new();
    win.settle(&mut ed);
    win.menu_click(&mut ed, MenuAction::MagicCut);
    with_magic_cut(|d| {
        d.add_stroke(MagicCutStroke {
            foreground: true,
            radius: 4.0,
            points: vec![[50.0, 45.0], [70.0, 45.0]],
        })
    })
    .expect("the window opened");
    win.settle(&mut ed);
    ed.activate(1).unwrap();
    let depth = history_depth(&ed);
    let out = win.press_enter(&mut ed);
    let done = Window::perform(out, &mut ed);
    assert!(
        matches!(done.as_slice(), [(MenuAction::MagicCut, Err(_))]),
        "the cut is refused on the other document: {done:?}"
    );
    assert!(ed.active().unwrap().document.selection.is_none());
    assert_eq!(history_depth(&ed), depth, "nothing recorded");
    ed.activate(0).unwrap();
    assert!(ed.active().unwrap().document.selection.is_none());
}

/// A stored style preset whose effects do not parse is left out of the
/// Styles panel, and the swatch after it still applies ITS style.
#[test]
fn a_broken_style_preset_does_not_shift_the_styles_panel() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.open_path(&png(dir.path(), "a.png", 64, 48, |_, _| [90, 90, 90, 255]))
        .unwrap();
    let layer = ed.active().unwrap().document.active_layer().unwrap();
    let effects = layer_model::LayerEffects {
        drop_shadow: Some(layer_model::ShadowEffect::default()),
        ..layer_model::LayerEffects::default()
    };
    ed.presets_mut()
        .define_style("Broken", "not a style".to_string());
    ed.presets_mut()
        .define_style("Shadow", serde_json::to_string(&effects).unwrap());

    let mut win = Window::new();
    win.chrome
        .workspace_for_test()
        .dock
        .set_open(PanelId::Styles, true);
    win.chrome.workspace_for_test().dock.raise(PanelId::Styles);
    win.settle(&mut ed);
    assert!(
        win.rect(ui::panels::styles::ids::tile(1)).is_none(),
        "only the parsable preset is a swatch"
    );
    let tile = win
        .rect(ui::panels::styles::ids::tile(0))
        .expect("the Shadow preset is a swatch");
    let out = win.click(&mut ed, tile.center());
    let done = Window::perform(out, &mut ed);
    assert!(
        done.iter()
            .any(|(a, r)| *a == MenuAction::ApplyStyleAt(0) && r.is_ok()),
        "{done:?}"
    );
    assert_eq!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(layer)
            .unwrap()
            .effects,
        effects,
        "the swatch applied the Shadow style"
    );
}
