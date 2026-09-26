//! W16-N: the panels speak the interface language, read back from the drawn
//! chrome — panel tab titles, a new layer's default name, its History step,
//! the Layers panel's blend modes, the Info rows, the Adjustments buttons,
//! the Character panel's choices and the shipped brush presets.
//!
//! Its own test binary for the reason `w16n_language.rs` is: the interface
//! language is process-wide (`ui::strings::set_locale`, installed by
//! `Editor::set_preferences`), so one `#[test]` walks the whole sequence.
//! `w16n_language.rs` proves the Window > Language click stores and applies
//! the choice; here the choice is applied the way that click's output is
//! (`Editor::set_preferences`) and every assertion reads painted text.

use app_shell::chrome::install_theme;
use app_shell::{
    AppPaths, Chrome, ChromeOutput, Editor, Preferences, RecentFiles, ScriptedDialogs,
};

fn raw_input(time: f64, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        time: Some(time),
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    }
}

fn collect_texts(
    shapes: &[egui::Shape],
    out: &mut Vec<(std::sync::Arc<egui::Galley>, egui::Pos2)>,
) {
    for shape in shapes {
        match shape {
            egui::Shape::Text(t) => out.push((t.galley.clone(), t.pos)),
            egui::Shape::Vec(inner) => collect_texts(inner, out),
            _ => {}
        }
    }
}

struct Window {
    ctx: egui::Context,
    chrome: Chrome,
    /// The frames' clock: half a second a frame unless a step says
    /// otherwise, so a click is never a double click and a pointer that
    /// rests a frame is past egui's tooltip delay.
    time: f64,
}

impl Window {
    fn new(editor: &mut Editor) -> Self {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        ctx.style_mut(|s| {
            s.animation_time = 0.0;
        });
        let mut window = Self {
            ctx,
            chrome: Chrome::new(),
            time: 0.0,
        };
        for _ in 0..3 {
            window.frame(editor, Vec::new());
        }
        window
    }

    fn frame(
        &mut self,
        editor: &mut Editor,
        events: Vec<egui::Event>,
    ) -> (
        ChromeOutput,
        Vec<(std::sync::Arc<egui::Galley>, egui::Pos2)>,
    ) {
        self.frame_after(0.5, editor, events)
    }

    /// One frame, `dt` seconds after the last.
    fn frame_after(
        &mut self,
        dt: f64,
        editor: &mut Editor,
        events: Vec<egui::Event>,
    ) -> (
        ChromeOutput,
        Vec<(std::sync::Arc<egui::Galley>, egui::Pos2)>,
    ) {
        let mut out = ChromeOutput::default();
        let chrome = &mut self.chrome;
        self.time += dt;
        let full = self.ctx.run(raw_input(self.time, events), |ctx| {
            out = chrome.ui(ctx, editor);
        });
        let shapes: Vec<egui::Shape> = full.shapes.into_iter().map(|c| c.shape).collect();
        let mut texts = Vec::new();
        collect_texts(&shapes, &mut texts);
        (out, texts)
    }

    fn painted(&mut self, editor: &mut Editor) -> Vec<String> {
        self.frame(editor, Vec::new())
            .1
            .into_iter()
            .map(|(g, _)| g.text().trim().to_string())
            .collect()
    }

    fn click_at(&mut self, editor: &mut Editor, pos: egui::Pos2) -> ChromeOutput {
        self.frame(editor, vec![egui::Event::PointerMoved(pos)]);
        let (out, _) = self.frame(
            editor,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );
        out
    }

    /// Click the first galley painted as `label`.
    fn click_text(&mut self, editor: &mut Editor, label: &str) -> ChromeOutput {
        let (_, texts) = self.frame(editor, Vec::new());
        let pos = texts
            .into_iter()
            .find(|(g, _)| g.text().trim() == label)
            .map(|(g, pos)| egui::Rect::from_min_size(pos, g.size()).center())
            .unwrap_or_else(|| panic!("{label:?} was never painted"));
        self.click_at(editor, pos)
    }

    fn click_id(&mut self, editor: &mut Editor, id: egui::Id) -> ChromeOutput {
        let pos = self
            .ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was never drawn"))
            .rect
            .center();
        self.click_at(editor, pos)
    }

    /// Rest the pointer on widget `id` and read what the frame painted — its
    /// tooltip included.
    fn hover_id(&mut self, editor: &mut Editor, id: egui::Id) -> Vec<String> {
        self.frame(editor, Vec::new());
        let pos = self
            .ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was never drawn"))
            .rect
            .center();
        // Glide in (egui counts a move by the pointer's velocity: three
        // positions inside a tenth of a second), then rest past the tooltip
        // delay.
        self.frame(
            editor,
            vec![egui::Event::PointerMoved(pos - egui::vec2(4.0, 0.0))],
        );
        for step in [2.0, 0.0] {
            let at = pos - egui::vec2(step, 0.0);
            self.frame_after(0.016, editor, vec![egui::Event::PointerMoved(at)]);
        }
        let mut last = Vec::new();
        for _ in 0..3 {
            last = self
                .frame(editor, Vec::new())
                .1
                .into_iter()
                .map(|(g, _)| g.text().trim().to_string())
                .collect();
        }
        self.frame(editor, vec![egui::Event::PointerGone]);
        last
    }
}

fn has(painted: &[String], text: &str) -> bool {
    painted.iter().any(|t| t == text)
}

#[test]
fn the_panels_draw_their_titles_names_modes_rows_and_presets_in_the_interface_language() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = Editor::with_state(
        AppPaths::rooted(dir.path()),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    // German, applied as Window > Language > Deutsch applies it.
    let prefs = Preferences {
        language: "de".to_string(),
        ..editor.preferences().clone()
    };
    editor.set_preferences(prefs);
    editor
        .new_document_with(
            200,
            100,
            "t",
            app_shell::import::BlankBackground::Transparent,
        )
        .unwrap();
    let mut window = Window::new(&mut editor);

    // Every open panel's tab title is German, and no English one is left.
    let painted = window.painted(&mut editor);
    for title in [
        "Eigenschaften",
        "Korrekturen",
        "Protokoll",
        "Ebenen",
        "Kan\u{00E4}le",
        "Pfade",
        "Histogramm",
        "Farbe",
        "Farbfelder",
        "Pinsel",
        "Zeichen",
        "Absatz",
    ] {
        assert!(
            has(&painted, title),
            "tab {title:?} is painted: {painted:?}"
        );
    }
    for english in [
        "Properties",
        "Adjustments",
        "History",
        "Layers",
        "Channels",
        "Paths",
        "Swatches",
        "Brushes",
        "Character",
        "Paragraph",
    ] {
        assert!(
            !has(&painted, english),
            "the English tab {english:?} survives: {painted:?}"
        );
    }

    // The Layers panel's + names the new layer "Ebene 2", and History lists
    // the step as "Ebene erstellen".
    let out = window.click_id(&mut editor, ui::view::ids::new_layer());
    assert_eq!(out.commands.len(), 1, "{out:?}");
    for command in out.commands {
        editor.apply_command(command);
    }
    let painted = window.painted(&mut editor);
    assert!(
        has(&painted, "Ebene 2"),
        "the new layer's name: {painted:?}"
    );
    assert!(
        has(&painted, "Ebene erstellen"),
        "the History step: {painted:?}"
    );
    assert!(!has(&painted, "Layer 2") && !has(&painted, "Create Layer"));

    // The Layers panel's New Group button names the group "Gruppe".
    assert!(!has(&painted, "Gruppe"), "no group yet: {painted:?}");
    let out = window.click_id(&mut editor, ui::view::ids::new_group());
    assert_eq!(out.commands.len(), 1, "{out:?}");
    for command in out.commands {
        editor.apply_command(command);
    }
    let painted = window.painted(&mut editor);
    assert!(has(&painted, "Gruppe"), "the new group's name: {painted:?}");
    assert!(!has(&painted, "Group"), "{painted:?}");
    // The Properties panel's heading and the Color panel's Gray notation.
    assert!(
        has(&painted, "Ebeneneigenschaften") && !has(&painted, "Layer Properties"),
        "the Properties heading is German: {painted:?}"
    );
    assert!(
        has(&painted, "Grau") && !has(&painted, "Gray"),
        "the Color panel's Gray is German: {painted:?}"
    );

    // The blend-mode list: open the Layers panel's mode box ("Normal" in
    // German too), and Multiply reads "Multiplizieren".
    window.click_text(&mut editor, "Normal");
    let painted = window.painted(&mut editor);
    assert!(
        has(&painted, "Multiplizieren") && has(&painted, "Negativ multiplizieren"),
        "the blend modes are German: {painted:?}"
    );
    assert!(!has(&painted, "Multiply"), "{painted:?}");
    window.frame(
        &mut editor,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }],
    );

    // The Info panel's rows.
    window.click_text(&mut editor, "Info");
    let painted = window.painted(&mut editor);
    assert!(
        has(&painted, "Dokument") && has(&painted, "Zeiger"),
        "the Info rows are German: {painted:?}"
    );
    assert!(!has(&painted, "Document") && !has(&painted, "Pointer"));

    // The Adjustments panel's buttons: their names are their tooltips.
    window.click_text(&mut editor, "Korrekturen");
    let tip = window.hover_id(
        &mut editor,
        ui::view::ids::adjustment_tile(ui::menu::AdjustmentId::BrightnessContrast),
    );
    assert!(
        has(&tip, "Helligkeit/Kontrast"),
        "the Brightness/Contrast button's name is German: {tip:?}"
    );

    // The Brushes panel's first shipped preset, "Soft Round 24", by its
    // tile's tooltip (the tile id `view::docks::brush_tile_id(0)` builds).
    let tip = window.hover_id(&mut editor, egui::Id::new(("raster-brush-tile", 0usize)));
    assert!(
        has(&tip, "Weich rund 24"),
        "the brush preset's name is German: {tip:?}"
    );

    // The Character panel with no text layer: its reason and the Type
    // tool's default face ("Regular" is "Normal" in German).
    window.click_text(&mut editor, "Zeichen");
    let painted = window.painted(&mut editor);
    assert!(
        has(
            &painted,
            "W\u{00E4}hlen Sie eine Textebene aus, um ihren Text zu bearbeiten"
        ),
        "the Character panel's reason is German: {painted:?}"
    );
    assert!(
        has(&painted, "Schriftschnitt") && !has(&painted, "Regular"),
        "the default face is named in German: {painted:?}"
    );
}
