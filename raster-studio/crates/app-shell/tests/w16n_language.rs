//! W16-N: Window > Language and Window > Glass Menus, through the real route.
//!
//! The drawn chrome (`Chrome::ui`, the menu bar `menu_bridge::draw` paints)
//! is clicked the way a user clicks it: the Window menu's title, then the
//! Language submenu, then a language's own name. The click's
//! `ChromeOutput::preferences` is applied with `Editor::set_preferences`,
//! exactly as `Shell` applies it, and the next frame is read back.
//!
//! This is its own test binary on purpose: the interface language is
//! process-wide (`ui::strings::set_locale`, installed by
//! `Editor::set_preferences`), and switching it inside the library's test
//! binary would change the strings every other test in that process reads.
//! One `#[test]` walks the whole sequence so nothing here races either.

use app_shell::chrome::install_theme;
use app_shell::{
    AppPaths, Chrome, ChromeOutput, Editor, Preferences, RecentFiles, ScriptedDialogs,
};

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

struct Window {
    ctx: egui::Context,
    chrome: Chrome,
}

impl Window {
    fn new(editor: &mut Editor) -> Self {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut window = Self {
            ctx,
            chrome: Chrome::new(),
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
    ) -> (ChromeOutput, Vec<egui::Shape>) {
        let mut out = ChromeOutput::default();
        let chrome = &mut self.chrome;
        let full = self.ctx.run(raw_input(events), |ctx| {
            out = chrome.ui(ctx, editor);
        });
        (out, full.shapes.into_iter().map(|c| c.shape).collect())
    }

    /// Every galley one frame painted, with where it sits.
    fn texts(&mut self, editor: &mut Editor) -> Vec<(std::sync::Arc<egui::Galley>, egui::Pos2)> {
        let (_, shapes) = self.frame(editor, Vec::new());
        let mut out = Vec::new();
        collect_texts(&shapes, &mut out);
        out
    }

    fn painted(&mut self, editor: &mut Editor) -> Vec<String> {
        self.texts(editor)
            .into_iter()
            .map(|(g, _)| g.text().trim().to_string())
            .collect()
    }

    /// Click the first galley whose text is `label` (a checkable row carries
    /// leading spaces for its tick gutter, so the match is on the trimmed
    /// text) and return what that frame meant.
    fn click_text(&mut self, editor: &mut Editor, label: &str) -> ChromeOutput {
        let rect = self
            .texts(editor)
            .into_iter()
            .find(|(g, _)| g.text().trim() == label)
            .map(|(g, pos)| egui::Rect::from_min_size(pos, g.size()))
            .unwrap_or_else(|| panic!("{label:?} was never painted"));
        let pos = rect.center();
        // Hover first, so a submenu row opens the way a pointer opens it.
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

    /// Open `menu` > `submenu` and click `row`, applying the preferences the
    /// click produced the way `Shell` does; answers whether it produced any.
    fn pick(&mut self, editor: &mut Editor, path: &[&str]) -> bool {
        let mut out = ChromeOutput::default();
        for label in path {
            out = self.click_text(editor, label);
        }
        let changed = out.preferences.is_some();
        if let Some(prefs) = out.preferences {
            editor.set_preferences(prefs);
        }
        // Let the menu close and the next frame draw with what changed.
        self.frame(editor, vec![egui::Event::PointerGone]);
        self.frame(editor, Vec::new());
        changed
    }

    /// The fills of every rectangle one frame painted.
    fn fills(&mut self, editor: &mut Editor) -> Vec<egui::Color32> {
        let (_, shapes) = self.frame(editor, Vec::new());
        let mut out = Vec::new();
        collect_fills(&shapes, &mut out);
        out
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

fn collect_fills(shapes: &[egui::Shape], out: &mut Vec<egui::Color32>) {
    for shape in shapes {
        match shape {
            egui::Shape::Rect(r) => out.push(r.fill),
            egui::Shape::Vec(inner) => collect_fills(inner, out),
            _ => {}
        }
    }
}

/// Every character of every painted galley has a glyph in the font it was
/// laid out in — no empty "tofu" box stands in for one.
fn every_painted_glyph_exists(window: &mut Window, editor: &mut Editor) -> Result<(), String> {
    let texts = window.texts(editor);
    for (galley, _) in &texts {
        for section in &galley.job.sections {
            let text = &galley.job.text[section.byte_range.clone()];
            let font = section.format.font_id.clone();
            if !window.ctx.fonts(|f| f.has_glyphs(&font, text)) {
                return Err(format!("{text:?} has a character its font cannot draw"));
            }
        }
    }
    Ok(())
}

#[test]
fn window_language_switches_the_drawn_menu_bar_and_glass_menus_turn_the_menus_translucent() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = Editor::with_state(
        AppPaths::rooted(dir.path()),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    let mut window = Window::new(&mut editor);

    // English first: the bar speaks the source strings.
    let english = window.painted(&mut editor);
    for title in [
        "File", "Edit", "Image", "Layer", "Select", "Filter", "View", "Window", "Help",
    ] {
        assert!(
            english.iter().any(|t| t == title),
            "{title} is on the English bar: {english:?}"
        );
    }

    // Window > Language > Deutsch: stored, applied the same frame it lands.
    assert!(
        window.pick(&mut editor, &["Window", "Language", "Deutsch"]),
        "the Deutsch row produced no preferences"
    );
    assert_eq!(editor.preferences().language, "de", "the choice is stored");
    let german = window.painted(&mut editor);
    for title in [
        "Datei",
        "Bearbeiten",
        "Bild",
        "Ebene",
        "Auswahl",
        "Filter",
        "Ansicht",
        "Fenster",
        "Hilfe",
    ] {
        assert!(
            german.iter().any(|t| t == title),
            "{title} is on the German bar: {german:?}"
        );
    }
    assert!(
        !german.iter().any(|t| t == "File" || t == "Window"),
        "no English title survives the switch: {german:?}"
    );

    // Glass Menus: off, an open menu sits on the opaque overlay fill.
    let tokens = design::Theme::Dark.tokens();
    let opaque = design::color32(tokens.palette.color(design::ColorRole::SurfaceOverlay));
    let glass = design::egui_theme::glass_menu_fill(tokens);
    window.click_text(&mut editor, "Fenster");
    let fills = window.fills(&mut editor);
    assert!(
        fills.contains(&opaque),
        "an open menu is drawn on the overlay fill"
    );
    assert!(!fills.contains(&glass), "no glass while Glass Menus is off");
    // Window > Glass Menus (Glasmenüs in German) turns it on and stores it.
    assert!(
        window.pick(&mut editor, &["Glasmenüs"]),
        "the Glass Menus row produced no preferences"
    );
    assert!(editor.preferences().glass_menus, "Glass Menus is stored");
    window.click_text(&mut editor, "Fenster");
    let fills = window.fills(&mut editor);
    assert!(
        fills.contains(&glass),
        "with Glass Menus on the open menu is drawn on the translucent fill {glass:?}: {fills:?}"
    );
    assert!(glass.a() < 255, "the glass fill lets the canvas through");
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
    window.pick(&mut editor, &["Fenster", "Glasmenüs"]);
    assert!(
        !editor.preferences().glass_menus,
        "Glass Menus toggles back off"
    );

    // Japanese: the bar is in Japanese and every glyph it paints exists — the
    // bundled CJK face is in the fallback chain.
    assert!(window.pick(&mut editor, &["Fenster", "Sprache", "日本語"]));
    assert_eq!(editor.preferences().language, "ja");
    let japanese = window.painted(&mut editor);
    for title in [
        "ファイル",
        "編集",
        "イメージ",
        "レイヤー",
        "ウィンドウ",
        "ヘルプ",
    ] {
        assert!(
            japanese.iter().any(|t| t == title),
            "{title} is on the Japanese bar: {japanese:?}"
        );
    }
    every_painted_glyph_exists(&mut window, &mut editor).unwrap();

    // Korean and Simplified Chinese draw too.
    assert!(window.pick(&mut editor, &["ウィンドウ", "言語", "한국어"]));
    let korean = window.painted(&mut editor);
    assert!(korean.iter().any(|t| t == "파일"), "{korean:?}");
    every_painted_glyph_exists(&mut window, &mut editor).unwrap();
    assert!(window.pick(&mut editor, &["창", "언어", "简体中文"]));
    let chinese = window.painted(&mut editor);
    assert!(chinese.iter().any(|t| t == "文件"), "{chinese:?}");
    every_painted_glyph_exists(&mut window, &mut editor).unwrap();

    // And back to English, through the same route.
    assert!(window.pick(&mut editor, &["窗口", "语言", "English"]));
    assert_eq!(editor.preferences().language, "en");
    assert!(window.painted(&mut editor).iter().any(|t| t == "File"));
}
