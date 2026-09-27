//! W18-K: every language wave 18 added switches the drawn menu bar, through
//! the real route.
//!
//! The drawn chrome (`Chrome::ui`, the menu bar `menu_bridge::draw` paints) is
//! clicked the way a user clicks it: the Window menu's title (in the language
//! in force), then Language, then the language's own name. The click's
//! `ChromeOutput::preferences` is applied with `Editor::set_preferences`, as
//! `Shell` applies it, and the next frame's painted galleys are read back:
//! the nine menu titles are the language's, and every painted character has
//! a glyph in the font it was laid out in.
//!
//! Its own test binary for the reason `w16n_language` is: the interface
//! language is process-wide, and one `#[test]` walks the whole sequence.

use app_shell::chrome::install_theme;
use app_shell::{
    AppPaths, Chrome, ChromeOutput, Editor, Preferences, RecentFiles, ScriptedDialogs,
};
use ui::strings::{tr_en, with_locale, Locale};

/// The languages wave 18 added, in the order the test visits them.
const W18: &[Locale] = &[
    Locale::Nl,
    Locale::Sv,
    Locale::Da,
    Locale::No,
    Locale::Fi,
    Locale::Cs,
    Locale::Sk,
    Locale::Hu,
    Locale::Ro,
    Locale::Pt,
    Locale::Ca,
    Locale::Hr,
    Locale::Sl,
    Locale::Id,
    Locale::Vi,
    Locale::ZhTw,
    Locale::El,
    Locale::Bg,
    Locale::Sr,
    Locale::Mk,
    Locale::Et,
    Locale::Lt,
    Locale::Eo,
    Locale::Sq,
];

const TITLES: [&str; 9] = [
    "File", "Edit", "Image", "Layer", "Select", "Filter", "View", "Window", "Help",
];

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

type Painted = Vec<(std::sync::Arc<egui::Galley>, egui::Pos2)>;

impl Window {
    fn new(editor: &mut Editor) -> Self {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        ctx.style_mut(|s| s.animation_time = 0.0);
        let mut window = Self {
            ctx,
            chrome: Chrome::new(),
        };
        for _ in 0..3 {
            window.frame(editor, Vec::new());
        }
        window
    }

    fn frame(&mut self, editor: &mut Editor, events: Vec<egui::Event>) -> (ChromeOutput, Painted) {
        let mut out = ChromeOutput::default();
        let chrome = &mut self.chrome;
        let full = self.ctx.run(raw_input(events), |ctx| {
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

    /// Click the first galley whose trimmed text is `label`, hovering first
    /// so a submenu row opens the way a pointer opens it. The row must lie
    /// inside the window: a row drawn past its edge cannot be clicked.
    fn click_text(&mut self, editor: &mut Editor, label: &str) -> ChromeOutput {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0));
        let rect = self
            .frame(editor, Vec::new())
            .1
            .into_iter()
            .find(|(g, _)| g.text().trim() == label)
            .map(|(g, pos)| egui::Rect::from_min_size(pos, g.size()))
            .unwrap_or_else(|| panic!("{label:?} was never painted"));
        assert!(
            screen.contains_rect(rect),
            "{label:?} is drawn at {rect:?}, outside the {screen:?} window"
        );
        let pos = rect.center();
        self.frame(editor, vec![egui::Event::PointerMoved(pos)]);
        let press = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        self.frame(
            editor,
            vec![egui::Event::PointerMoved(pos), press(true), press(false)],
        )
        .0
    }

    /// Window > Language > `name`, with the menu labels in `current`'s words,
    /// applying the preferences the click produced the way `Shell` does.
    fn pick_language(&mut self, editor: &mut Editor, current: Locale, name: &str) -> bool {
        let window = with_locale(current, || tr_en("Window").to_string());
        let language = with_locale(current, || tr_en("Language").to_string());
        self.click_text(editor, &window);
        self.click_text(editor, &language);
        let out = self.click_text(editor, name);
        let changed = out.preferences.is_some();
        if let Some(prefs) = out.preferences {
            editor.set_preferences(prefs);
        }
        self.frame(editor, vec![egui::Event::PointerGone]);
        self.frame(editor, Vec::new());
        changed
    }
}

fn collect_texts(shapes: &[egui::Shape], out: &mut Painted) {
    for shape in shapes {
        match shape {
            egui::Shape::Text(t) => out.push((t.galley.clone(), t.pos)),
            egui::Shape::Vec(inner) => collect_texts(inner, out),
            _ => {}
        }
    }
}

/// Every character of every painted galley has a glyph in its font.
fn every_painted_glyph_exists(window: &mut Window, editor: &mut Editor) -> Result<(), String> {
    let (_, texts) = window.frame(editor, Vec::new());
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
fn every_w18_language_switches_the_drawn_menu_bar_and_draws_without_tofu() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = Editor::with_state(
        AppPaths::rooted(dir.path()),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    let mut window = Window::new(&mut editor);
    let english = window.painted(&mut editor);
    for title in TITLES {
        assert!(english.iter().any(|t| t == title), "{title}: {english:?}");
    }

    let mut current = Locale::En;
    for &locale in W18 {
        let name = locale.display_name();
        assert!(
            window.pick_language(&mut editor, current, name),
            "the {name:?} row produced no preferences"
        );
        assert_eq!(editor.preferences().language, locale.code());
        let painted = window.painted(&mut editor);
        let expected: Vec<String> = TITLES
            .iter()
            .map(|t| with_locale(locale, || tr_en(t).to_string()))
            .collect();
        for title in &expected {
            assert!(
                painted.iter().any(|t| t == title),
                "{locale:?}: {title:?} is not on the bar: {painted:?}"
            );
        }
        let translated = expected.iter().zip(TITLES).filter(|(t, e)| t != e).count();
        // "Filter" and "Help" are the same word in some languages.
        assert!(
            translated >= 7,
            "{locale:?} left the bar in English: {expected:?}"
        );
        assert!(
            !painted.iter().any(|t| t == "Window"),
            "{locale:?}: the English Window title survived: {painted:?}"
        );
        every_painted_glyph_exists(&mut window, &mut editor)
            .unwrap_or_else(|e| panic!("{locale:?}: {e}"));
        current = locale;
    }

    // And back to English through the same route.
    assert!(window.pick_language(&mut editor, current, "English"));
    assert_eq!(editor.preferences().language, "en");
    assert!(window.painted(&mut editor).iter().any(|t| t == "File"));
}
