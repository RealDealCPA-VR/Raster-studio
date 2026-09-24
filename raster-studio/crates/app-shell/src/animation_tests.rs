//! W9-J: animated GIF / APNG / WebP open as `_a_` frame layers and Export As
//! writes them back as animations — driven through the editor's real routes:
//! File > Open (the dispatched action, its picker and the import job), the
//! synchronous open that recent files and drag-and-drop use, and
//! `Editor::request_export`, which the shell calls with the Export As job
//! once the folder picker answered.

use super::*;
use crate::dialogs::ScriptedDialogs;
use crate::recent::RecentFiles;
use raster::animation::{self, AnimationFrame};
use raster::ExportFormat;

const W: u32 = 4;
const H: u32 = 3;

fn solid(rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat((W * H) as usize)
}

/// Three distinct frames. The third is not uniform, so a frame that lost its
/// place or its pixels cannot pass by accident.
fn source_frames() -> Vec<AnimationFrame> {
    let mut third = solid([0, 0, 255, 255]);
    third[..4].copy_from_slice(&[255, 255, 0, 255]);
    vec![
        AnimationFrame {
            rgba8: solid([255, 0, 0, 255]),
            delay_ms: 100,
        },
        AnimationFrame {
            rgba8: solid([0, 255, 0, 255]),
            delay_ms: 200,
        },
        AnimationFrame {
            rgba8: third,
            delay_ms: 300,
        },
    ]
}

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

fn root_names(editor: &Editor) -> Vec<String> {
    let doc = &editor.active().expect("a document is open").document;
    doc.layers
        .root()
        .iter()
        .rev()
        .map(|id| doc.layers.get(*id).unwrap().name.clone())
        .collect()
}

fn assert_frames_match(got: &[AnimationFrame], want: &[AnimationFrame], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: frame count");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(g.rgba8, w.rgba8, "{what}: frame {} pixels", i + 1);
        assert_eq!(g.delay_ms, w.delay_ms, "{what}: frame {} delay", i + 1);
    }
}

#[test]
fn an_animated_gif_opens_as_three_frame_layers_and_exports_as_three_frames() {
    let dir = tempfile::tempdir().unwrap();
    let gif = dir.path().join("walk.gif");
    std::fs::write(
        &gif,
        animation::encode_animation(ExportFormat::Gif, W, H, &source_frames()).unwrap(),
    )
    .unwrap();

    // File > Open: the action asks the picker, the import job decodes.
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(&gif));
    ed.dispatch(crate::action::Action::Open).unwrap();
    ed.poll_imports();
    assert_eq!(
        root_names(&ed),
        ["_a_Frame 1,100", "_a_Frame 2,200", "_a_Frame 3,300"],
        "one `_a_<name>,<delay>` layer per frame, frame 1 at the bottom"
    );
    let open = ed.active().unwrap();
    assert_eq!((open.document.width(), open.document.height()), (W, H));
    let visible: Vec<bool> = open
        .document
        .layers
        .root()
        .iter()
        .rev()
        .map(|id| open.document.layers.get(*id).unwrap().visible)
        .collect();
    assert_eq!(visible, [true, false, false], "the canvas opens on frame 1");
    // Each layer holds its own frame's pixels: compositing frame by frame
    // (that layer shown alone) gives back exactly the source frames.
    let layered = crate::import::composite_animation_frames(&open.document, &open.tiles).unwrap();
    assert_frames_match(&layered, &source_frames(), "layers");

    // Export As: GIF, PNG (APNG) and WebP rows, all Animated (the default).
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let job = ui::dialogs::ExportJob {
        base_name: "walk".to_string(),
        entries: vec![
            ui::dialogs::ExportEntry::new("", ExportFormat::Gif, 1.0),
            ui::dialogs::ExportEntry::new("", ExportFormat::Png, 1.0),
            ui::dialogs::ExportEntry::new("", ExportFormat::WebP, 1.0),
        ],
    };
    ed.request_export(job, out.clone());
    ed.poll_exports();
    for ext in ["gif", "png", "webp"] {
        let path = out.join(format!("walk.{ext}"));
        let back = animation::decode_animation_path(&path, raster::ImportLimits::default())
            .unwrap()
            .unwrap_or_else(|| panic!("{ext} was written as a still"));
        assert_frames_match(&back.frames, &source_frames(), ext);
    }

    // The synchronous open (recent files, drag-and-drop) agrees.
    let mut sync = editor(&dir.path().join("sync"), ScriptedDialogs::new());
    sync.open_path(&out.join("walk.webp")).unwrap();
    assert_eq!(
        root_names(&sync),
        ["_a_Frame 1,100", "_a_Frame 2,200", "_a_Frame 3,300"]
    );
}

#[test]
fn a_non_frame_layer_shows_in_every_exported_frame_and_a_scaled_row_scales_every_frame() {
    let dir = tempfile::tempdir().unwrap();
    let gif = dir.path().join("a.gif");
    std::fs::write(
        &gif,
        animation::encode_animation(ExportFormat::Gif, W, H, &source_frames()).unwrap(),
    )
    .unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&gif).unwrap();
    // A plain layer on top, opaque white in its top-left pixel only.
    let doc = ed.active_mut().unwrap();
    let mut overlay = vec![0u8; (W * H * 4) as usize];
    overlay[..4].copy_from_slice(&[255, 255, 255, 255]);
    let image = crate::import::DecodedImage {
        width: W,
        height: H,
        rgba8: overlay,
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
    };
    let (command, _) = crate::import::import_command(&image, "Overlay", &mut doc.tiles).unwrap();
    doc.apply(command).unwrap();

    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut double = ui::dialogs::ExportEntry::new("@2x", ExportFormat::Png, 2.0);
    double.preset.filter = raster::ResampleFilter::Nearest;
    ed.request_export(
        ui::dialogs::ExportJob {
            base_name: "a".to_string(),
            entries: vec![
                ui::dialogs::ExportEntry::new("", ExportFormat::Gif, 1.0),
                double,
            ],
        },
        out.clone(),
    );
    ed.poll_exports();
    let back =
        animation::decode_animation_path(&out.join("a.gif"), raster::ImportLimits::default())
            .unwrap()
            .unwrap();
    assert_eq!(back.frames.len(), 3);
    for (i, (got, src)) in back.frames.iter().zip(source_frames()).enumerate() {
        assert_eq!(&got.rgba8[..4], &[255, 255, 255, 255], "frame {}", i + 1);
        assert_eq!(&got.rgba8[4..], &src.rgba8[4..], "frame {}", i + 1);
    }
    let big =
        animation::decode_animation_path(&out.join("a@2x.png"), raster::ImportLimits::default())
            .unwrap()
            .unwrap();
    assert_eq!((big.width, big.height, big.frames.len()), (W * 2, H * 2, 3));
}

/// Set the visibility of the root layer named `name` directly on the document.
fn set_root_visible(ed: &mut Editor, name: &str, visible: bool) {
    let doc = &mut ed.active_mut().unwrap().document;
    let id = *doc
        .layers
        .root()
        .iter()
        .find(|id| doc.layers.get(**id).unwrap().name == name)
        .unwrap_or_else(|| panic!("no root layer named {name:?}"));
    doc.layers.get_mut(id).unwrap().visible = visible;
}

#[test]
fn a_hidden_non_frame_layer_shows_in_no_exported_frame() {
    let dir = tempfile::tempdir().unwrap();
    let gif = dir.path().join("h.gif");
    std::fs::write(
        &gif,
        animation::encode_animation(ExportFormat::Gif, W, H, &source_frames()).unwrap(),
    )
    .unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&gif).unwrap();
    let doc = ed.active_mut().unwrap();
    let image = crate::import::DecodedImage {
        width: W,
        height: H,
        rgba8: solid([255, 255, 255, 255]),
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
    };
    let (command, _) = crate::import::import_command(&image, "Hidden", &mut doc.tiles).unwrap();
    doc.apply(command).unwrap();
    set_root_visible(&mut ed, "Hidden", false);

    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    ed.request_export(
        ui::dialogs::ExportJob {
            base_name: "h".to_string(),
            entries: vec![ui::dialogs::ExportEntry::new("", ExportFormat::Gif, 1.0)],
        },
        out.clone(),
    );
    ed.poll_exports();
    let back =
        animation::decode_animation_path(&out.join("h.gif"), raster::ImportLimits::default())
            .unwrap()
            .unwrap();
    // The opaque white layer would cover every frame if it were shown.
    assert_frames_match(&back.frames, &source_frames(), "hidden non-frame layer");
}

#[test]
fn a_still_png_is_unaffected_on_open_and_on_an_animated_export_row() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("still.png");
    std::fs::write(
        &png,
        raster::encode(ExportFormat::Png, W, H, &solid([10, 20, 30, 255])).unwrap(),
    )
    .unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(&png));
    ed.dispatch(crate::action::Action::Open).unwrap();
    ed.poll_imports();
    assert_eq!(root_names(&ed), ["still.png"], "one plain layer, as before");

    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let row = ui::dialogs::ExportEntry::new("", ExportFormat::Gif, 1.0);
    assert!(row.animated);
    ed.request_export(
        ui::dialogs::ExportJob {
            base_name: "still".to_string(),
            entries: vec![row],
        },
        out.clone(),
    );
    ed.poll_exports();
    let bytes = std::fs::read(out.join("still.gif")).unwrap();
    assert!(
        animation::decode_animation_bytes(&bytes, raster::ImportLimits::default())
            .unwrap()
            .is_none(),
        "a document without frame layers exports a still"
    );
    assert_eq!(
        raster::decode_bytes(&bytes).unwrap().rgba8,
        solid([10, 20, 30, 255])
    );
}

#[test]
fn an_animated_row_turned_off_exports_the_composite_still() {
    let dir = tempfile::tempdir().unwrap();
    let gif = dir.path().join("b.gif");
    std::fs::write(
        &gif,
        animation::encode_animation(ExportFormat::Gif, W, H, &source_frames()).unwrap(),
    )
    .unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&gif).unwrap();
    // Show frame 2 instead of frame 1, so the still is the canvas as it is
    // and not a first-frame fallback.
    set_root_visible(&mut ed, "_a_Frame 1,100", false);
    set_root_visible(&mut ed, "_a_Frame 2,200", true);
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut row = ui::dialogs::ExportEntry::new("", ExportFormat::Gif, 1.0);
    row.animated = false;
    ed.request_export(
        ui::dialogs::ExportJob {
            base_name: "b".to_string(),
            entries: vec![row],
        },
        out.clone(),
    );
    ed.poll_exports();
    let bytes = std::fs::read(out.join("b.gif")).unwrap();
    assert!(
        animation::decode_animation_bytes(&bytes, raster::ImportLimits::default())
            .unwrap()
            .is_none()
    );
    // Only frame 2 is visible on the canvas, so the still is frame 2.
    assert_eq!(
        raster::decode_bytes(&bytes).unwrap().rgba8,
        source_frames()[1].rgba8
    );
}

/// The texts the Export As dialog draws when the real host opens it from
/// File > Export As > GIF over the active document and draws a few frames.
fn export_as_gif_texts(ed: &Editor) -> Vec<String> {
    let mut host = crate::dialog_host::DialogHost::default();
    assert!(host.open_for_menu_action(&ui::menu::MenuAction::Export(ExportFormat::Gif), ed));
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let mut out = crate::ChromeOutput::default();
    let mut texts = Vec::new();
    for _ in 0..3 {
        let full = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1400.0),
                )),
                ..Default::default()
            },
            |ctx| host.ui(ctx, None, &mut out),
        );
        texts = full
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
    }
    assert!(host.is_open(), "the Export As dialog closed by itself");
    texts
}

#[test]
fn the_export_as_dialog_offers_animated_only_for_a_document_with_frame_layers() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("still.png");
    std::fs::write(
        &png,
        raster::encode(ExportFormat::Png, W, H, &solid([10, 20, 30, 255])).unwrap(),
    )
    .unwrap();
    let gif = dir.path().join("walk.gif");
    std::fs::write(
        &gif,
        animation::encode_animation(ExportFormat::Gif, W, H, &source_frames()).unwrap(),
    )
    .unwrap();

    // A still: the GIF row draws no Animated checkbox and no frame caption.
    let mut still = editor(dir.path(), ScriptedDialogs::new().opening(&png));
    still.dispatch(crate::action::Action::Open).unwrap();
    still.poll_imports();
    let texts = export_as_gif_texts(&still);
    assert!(
        texts.iter().any(|t| t == "Metadata"),
        "the settings panel was not drawn: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t == "Animated"),
        "a still document offered Animated: {texts:?}"
    );
    assert!(!texts.iter().any(|t| t.contains("_a_ layer")), "{texts:?}");

    // A 3-frame GIF: the checkbox is drawn and the caption counts 3 frames.
    let mut animated = editor(dir.path(), ScriptedDialogs::new().opening(&gif));
    animated.dispatch(crate::action::Action::Open).unwrap();
    animated.poll_imports();
    let texts = export_as_gif_texts(&animated);
    assert!(
        texts.iter().any(|t| t == "Animated"),
        "a 3-frame document did not offer Animated: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("one frame per _a_ layer (3)")),
        "no frame count in the caption: {texts:?}"
    );
}
