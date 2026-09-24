//! W10-B: the document-record panels and the Channels panel's alpha rows,
//! through the application — the real [`Chrome`] draws the panel, a real
//! click lands on it, and the frame's output is performed against a real
//! [`Editor`] the way the shell performs it: document commands through
//! history, menu actions through [`super::perform`].

use crate::chrome::{Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::presenter::CanvasPresenter;
use crate::recent::RecentFiles;
use raster::PixelRect;

fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    }
}

fn editor(dir: &std::path::Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    )
}

struct Window {
    ctx: egui::Context,
    chrome: Chrome,
}

impl Window {
    /// A chrome with `panel` open and raised.
    fn with_panel(editor: &mut Editor, panel: ui::PanelId) -> Self {
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let mut w = Self {
            ctx,
            chrome: Chrome::new(),
        };
        w.chrome
            .emit(ui::Intent::SetPanelOpen { panel, open: true });
        w.frame(editor, Vec::new());
        w.chrome
            .emit(ui::Intent::ApplyLayout(ui::LayoutId::Minimal));
        w.chrome
            .emit(ui::Intent::SetPanelOpen { panel, open: true });
        for _ in 0..4 {
            w.frame(editor, Vec::new());
        }
        w
    }

    fn frame(&mut self, editor: &mut Editor, events: Vec<egui::Event>) -> ChromeOutput {
        let mut out = ChromeOutput::default();
        let chrome = &mut self.chrome;
        let _ = self.ctx.run(raw_input(events), |ctx| {
            out = chrome.ui(ctx, editor);
        });
        out
    }

    /// Click the widget `id` and perform what the frame asked, as the shell
    /// does: commands through history, then menu actions.
    fn click(&mut self, editor: &mut Editor, id: egui::Id) -> ChromeOutput {
        for _ in 0..2 {
            self.frame(editor, Vec::new());
        }
        let at = self
            .ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was never drawn"))
            .rect
            .center();
        let out = self.frame(
            editor,
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
        for command in out.commands.clone() {
            editor.apply_command(command);
        }
        for action in out.menu.clone() {
            super::perform(action, editor).unwrap_or_else(|e| panic!("{action:?}: {e}"));
        }
        self.frame(editor, Vec::new());
        out
    }
}

/// A 16x16 transparent document with one saved selection covering the
/// top-left 8x8 quarter.
fn document_with_an_alpha_channel(dir: &std::path::Path) -> Editor {
    let mut ed = editor(dir);
    ed.new_document_with(16, 16, "Alpha", crate::import::BlankBackground::Transparent)
        .unwrap();
    ed.active_mut().unwrap().document.saved_selections.push((
        "Alpha 1".into(),
        editor_core::Selection::Rect {
            min: glam::IVec2::new(0, 0),
            max: glam::IVec2::new(8, 8),
        },
    ));
    ed
}

#[test]
fn viewing_an_alpha_channel_shows_its_grayscale_on_the_canvas() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = document_with_an_alpha_channel(dir.path());
    let whole = PixelRect::new(0, 0, 16, 16);
    let exported_before = ed.active_mut().unwrap().composite(whole).unwrap();
    let layers_before = ed.active().unwrap().document.layers.len();

    let mut window = Window::with_panel(&mut ed, ui::PanelId::Channels);
    let out = window.click(&mut ed, ui::panels::channels::alpha_eye_id(0));
    assert!(
        out.menu.contains(&ui::MenuAction::EditAlphaChannel(0)),
        "the eye routed to the application: {out:?}"
    );

    // The channel is open: a hidden scratch layer carries it, is active, and
    // painting aims at its mask.
    let open = ed.active().unwrap();
    let edit = open
        .document
        .extras
        .alpha_edit
        .expect("the channel is open");
    assert_eq!(edit.index, 0);
    assert_eq!(open.document.active_layer(), Some(edit.layer));
    assert!(!open.document.layers.get(edit.layer).unwrap().visible);
    assert!(ed.edit_target_is_mask(), "brushes paint into the channel");

    // What the canvas shows: the presenter reads the chrome's view settings
    // exactly as the shell does each frame, and the channel comes out alone,
    // opaque grayscale — white inside the saved selection, black outside.
    let mut presenter = CanvasPresenter::new();
    assert!(presenter.read_view_settings(&window.chrome));
    let shown = presenter
        .composite_masked(ed.active_mut().unwrap(), whole)
        .unwrap();
    let px = |x: usize, y: usize| &shown[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
    assert_eq!(px(2, 2), &[255, 255, 255, 255], "inside the channel");
    assert_eq!(px(12, 12), &[0, 0, 0, 255], "outside the channel");
    assert_eq!(px(7, 7), &[255, 255, 255, 255]);
    assert_eq!(px(8, 8), &[0, 0, 0, 255]);

    // The export is untouched: the scratch layer is hidden.
    assert_eq!(
        ed.active_mut().unwrap().composite(whole).unwrap(),
        exported_before,
        "the open channel never reaches the composite an export writes"
    );

    // Paint into the channel (a mask tile edit, as a brush commits one):
    // reveal the whole top-right quarter too.
    {
        let doc = ed.active_mut().unwrap();
        let ts = raster::TILE_SIZE as usize;
        let mut coverage = vec![0u8; ts * ts];
        for y in 0..8 {
            for x in 0..16 {
                coverage[y * ts + x] = 255;
            }
        }
        let hash = doc.tiles.insert_bytes(coverage);
        let paint = editor_core::Command::paint_tiles(
            editor_core::PixelTarget::Mask(edit.layer),
            [editor_core::TileEdit::set(
                raster::TileCoord::new(0, 0, 0),
                hash,
            )],
        )
        .unwrap();
        ed.apply_command(paint);
    }

    // The eye again stores the channel and takes the scratch layer away.
    let out = window.click(&mut ed, ui::panels::channels::alpha_eye_id(0));
    assert!(out.menu.contains(&ui::MenuAction::CloseAlphaChannel));
    let open = ed.active().unwrap();
    assert!(open.document.extras.alpha_edit.is_none());
    assert_eq!(open.document.layers.len(), layers_before);
    let (_, stored) = &open.document.saved_selections[0];
    let mask = selection::to_mask(stored, selection::Rect::from_xywh(0, 0, 16, 16)).unwrap();
    assert_eq!(
        mask.coverage_at(glam::IVec2::new(12, 2)),
        255,
        "the paint was kept"
    );
    assert_eq!(mask.coverage_at(glam::IVec2::new(2, 12)), 0);
    assert!(!ed.edit_target_is_mask());
    // And the canvas is back to the composite.
    let mut presenter = CanvasPresenter::new();
    presenter.read_view_settings(&window.chrome);
    let shown = presenter
        .composite_masked(ed.active_mut().unwrap(), whole)
        .unwrap();
    assert_eq!(shown, exported_before);

    // Painting into an alpha channel is undoable: the store rode the close's
    // transaction, so one undo takes the close back (the channel is open
    // again, the stored coverage is the original), the next the paint, the
    // last the open — and the saved selection is the original throughout.
    let stored_at = |ed: &Editor, x: i32, y: i32| {
        let (_, s) = &ed.active().unwrap().document.saved_selections[0];
        selection::to_mask(s, selection::Rect::from_xywh(0, 0, 16, 16))
            .unwrap()
            .coverage_at(glam::IVec2::new(x, y))
    };
    ed.dispatch(crate::Action::Undo).unwrap();
    assert!(
        ed.active().unwrap().document.extras.alpha_edit.is_some(),
        "the close was undone"
    );
    assert_eq!(stored_at(&ed, 12, 2), 0, "the store was undone with it");
    assert_eq!(stored_at(&ed, 2, 2), 255);
    ed.dispatch(crate::Action::Undo).unwrap();
    ed.dispatch(crate::Action::Undo).unwrap();
    assert!(ed.active().unwrap().document.extras.alpha_edit.is_none());
    assert_eq!(ed.active().unwrap().document.layers.len(), layers_before);
    assert_eq!(stored_at(&ed, 12, 2), 0, "undo all: the original channel");
    // And redo brings the painted channel back.
    for _ in 0..3 {
        ed.dispatch(crate::Action::Redo).unwrap();
    }
    assert_eq!(
        stored_at(&ed, 12, 2),
        255,
        "redo: the paint is stored again"
    );
    assert!(ed.active().unwrap().document.extras.alpha_edit.is_none());
}

/// Close Alpha Channel is greyed out, with its reason, while no channel is
/// open, and enabled while one is: never an enabled row that does nothing.
#[test]
fn close_alpha_channel_is_disabled_until_a_channel_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = document_with_an_alpha_channel(dir.path());
    let resolve = |ed: &Editor| {
        let open = ed.active().unwrap();
        ui::MenuAction::CloseAlphaChannel.resolve(&ui::MenuContext::from_document(
            &open.document,
            &open.history,
        ))
    };
    assert_eq!(
        resolve(&ed),
        ui::menu::Resolution::Disabled(ui::menu::NO_ALPHA_CHANNEL_OPEN)
    );
    assert!(super::perform(ui::MenuAction::CloseAlphaChannel, &mut ed).is_err());
    super::perform(ui::MenuAction::EditAlphaChannel(0), &mut ed).unwrap();
    assert!(resolve(&ed).is_enabled());
    super::perform(ui::MenuAction::CloseAlphaChannel, &mut ed).unwrap();
    assert!(!resolve(&ed).is_enabled());
}

#[test]
fn a_note_survives_save_and_reopen_and_is_absent_from_the_export() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.new_document_with(
        24,
        24,
        "Notes",
        crate::import::BlankBackground::Solid {
            rgba8: [10, 200, 30, 255],
            depth: raster::BitDepth::Eight,
        },
    )
    .unwrap();
    let plain_png = dir.path().join("plain.png");
    ed.active_mut().unwrap().export_to(&plain_png).unwrap();

    // New Note through the drawn panel.
    let mut window = Window::with_panel(&mut ed, ui::PanelId::Notes);
    let out = window.click(&mut ed, ui::panels::notes::ids::new());
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, editor_core::Command::SetDocumentExtras { .. })),
        "{out:?}"
    );
    let note = ed.active().unwrap().document.extras.notes[0].clone();
    assert_eq!(note.text, ui::panels::notes::NEW_NOTE_TEXT);

    // Save, reopen: the note is there.
    let project = dir.path().join("notes.rstudio");
    ed.active_mut().unwrap().save_to(&project, "test").unwrap();
    let mut reopened = editor(dir.path());
    reopened.open_path(&project).unwrap();
    assert_eq!(
        reopened.active().unwrap().document.extras.notes,
        vec![note],
        "the note came back with the file"
    );

    // Export: byte-for-byte the pixels of the document without the note.
    let noted_png = dir.path().join("noted.png");
    reopened
        .active_mut()
        .unwrap()
        .export_to(&noted_png)
        .unwrap();
    let decode = |p: &std::path::Path| {
        raster::decode_surface_path(p, raster::ImportLimits::default())
            .unwrap()
            .pixels
            .into_rgba8()
    };
    assert_eq!(
        decode(&noted_png),
        decode(&plain_png),
        "a note is never rendered into an export"
    );
}

#[test]
fn a_paragraph_style_redefined_in_the_panel_restyles_two_layers_in_one_undo() {
    use layer_model::text::Alignment;
    use layer_model::{Layer, LayerKind, TextLayer};
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.new_document_with(
        32,
        32,
        "Styles",
        crate::import::BlankBackground::Transparent,
    )
    .unwrap();
    let text = |name: &str| {
        Layer::with_kind(
            name,
            LayerKind::Text(TextLayer {
                text: name.into(),
                ..TextLayer::default()
            }),
        )
    };
    let (a, b) = (text("A"), text("B"));
    let (a_id, b_id) = (a.id, b.id);
    ed.apply_command(editor_core::Command::create_layer(a));
    ed.apply_command(editor_core::Command::create_layer(b));
    let kind = ui::panels::text_styles::StyleKind::Paragraph;
    let mut window = Window::with_panel(&mut ed, ui::PanelId::ParagraphStyles);

    ed.set_active_layer(a_id);
    window.click(&mut ed, ui::panels::text_styles::ids::new(kind));
    let style = ed.active().unwrap().document.extras.paragraph_styles[0].id;
    window.click(&mut ed, ui::panels::text_styles::ids::row(kind, style));
    ed.set_active_layer(b_id);
    window.click(&mut ed, ui::panels::text_styles::ids::row(kind, style));

    let alignment =
        |ed: &Editor, id| match &ed.active().unwrap().document.layers.get(id).unwrap().kind {
            LayerKind::Text(t) => t.paragraph.alignment,
            _ => unreachable!(),
        };
    let mut centred = match &ed.active().unwrap().document.layers.get(a_id).unwrap().kind {
        LayerKind::Text(t) => t.clone(),
        _ => unreachable!(),
    };
    centred.paragraph.alignment = Alignment::Center;
    ed.apply_command(editor_core::Command::SetLayerKind {
        layer_id: a_id,
        kind: Box::new(LayerKind::Text(centred)),
    });
    ed.set_active_layer(a_id);
    window.click(&mut ed, ui::panels::text_styles::ids::redefine(kind));
    assert_eq!(alignment(&ed, a_id), Alignment::Center);
    assert_eq!(
        alignment(&ed, b_id),
        Alignment::Center,
        "B follows the style"
    );

    // The redefine was one step: one undo puts B back.
    ed.dispatch(crate::Action::Undo).unwrap();
    assert_ne!(alignment(&ed, b_id), Alignment::Center);
}

/// W10-B: a glyph clicked in the drawn Glyphs panel while the user is typing
/// lands at the session's caret, inside the draft the session confirms — the
/// panel's pick reaches the shell as `insert_glyphs`, and the shell's
/// performer hands it to the live session.
#[test]
fn a_glyph_clicked_in_the_panel_is_typed_into_the_live_session() {
    use layer_model::{Layer, LayerKind, TextLayer};
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.new_document_with(
        64,
        64,
        "Glyphs",
        crate::import::BlankBackground::Transparent,
    )
    .unwrap();
    let layer = Layer::with_kind(
        "T",
        LayerKind::Text(TextLayer {
            text: "ab".into(),
            font_family: "DejaVu Sans".into(),
            size_px: 24.0,
            ..TextLayer::default()
        }),
    );
    let id = layer.id;
    ed.apply_command(editor_core::Command::create_layer(layer));
    ed.set_active_layer(id);
    let text_of = |ed: &Editor| match &ed.active().unwrap().document.layers.get(id).unwrap().kind {
        LayerKind::Text(t) => t.text.clone(),
        _ => unreachable!(),
    };
    let mut window = Window::with_panel(&mut ed, ui::PanelId::Glyphs);

    let mut pointer = crate::tool_input::ToolPointer::new();
    pointer.enter_text_session(&mut ed, id);
    assert!(pointer.is_text_editing());
    pointer.text_edit(
        &mut ed,
        tools::TextEdit::CaretStep {
            back: true,
            extend: false,
        },
    );
    let depth = ed.active().unwrap().history_depth();

    let out = window.click(&mut ed, ui::panels::glyphs::ids::cell('A'));
    assert_eq!(out.insert_glyphs, vec![(id, "A".to_string())], "{out:?}");
    assert!(
        out.commands.is_empty(),
        "no command against the committed text"
    );
    for (layer, text) in &out.insert_glyphs {
        super::glyph_insert::insert_glyph(&mut pointer, &mut ed, *layer, text).unwrap();
    }
    assert_eq!(text_of(&ed), "aAb", "inserted at the caret");
    assert_eq!(ed.active().unwrap().history_depth(), depth, "in the draft");
    let confirmed = pointer.text_edit(&mut ed, tools::TextEdit::Confirm);
    assert_eq!(confirmed.steps, 1);
    assert_eq!(text_of(&ed), "aAb", "the session confirmed the glyph");
}
