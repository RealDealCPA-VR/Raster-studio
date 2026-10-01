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
//! The window is 1366 x 700 logical points, a 1366 x 768 laptop screen less
//! its title bar and taskbar: Window > Language holds 39 rows, taller than
//! that, so its list scrolls. A row is clicked only once it is drawn wholly
//! inside the window; one that is not is scrolled to with the mouse wheel, as
//! a user reaches it. The last row of the list is reached and clicked too.
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
    Locale::Tl,
    Locale::Kk,
];

const TITLES: [&str; 9] = [
    "File", "Edit", "Image", "Layer", "Select", "Filter", "View", "Window", "Help",
];

/// A 1366 x 768 laptop screen less its title bar and taskbar.
fn screen() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1366.0, 700.0))
}

fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(screen()),
        events,
        ..Default::default()
    }
}

struct Window {
    ctx: egui::Context,
    chrome: Chrome,
}

/// Every galley one frame drew: the galley, its whole rect, and the part of
/// that rect its clip lets show. A galley its clip hides entirely (a row a
/// scrolled list has moved out of view) is not in the list: it is not seen.
type Painted = Vec<(std::sync::Arc<egui::Galley>, egui::Rect, egui::Rect)>;

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
        let mut texts = Vec::new();
        for clipped in &full.shapes {
            collect_texts(&clipped.shape, clipped.clip_rect, &mut texts);
        }
        (out, texts)
    }

    fn painted(&mut self, editor: &mut Editor) -> Vec<String> {
        self.frame(editor, Vec::new())
            .1
            .into_iter()
            .map(|(g, _, _)| g.text().trim().to_string())
            .collect()
    }

    /// The shown rects of the Language rows one frame drew, top to bottom.
    fn language_rows(&mut self, editor: &mut Editor) -> Vec<(String, egui::Rect, egui::Rect)> {
        let mut rows: Vec<(String, egui::Rect, egui::Rect)> = self
            .frame(editor, Vec::new())
            .1
            .into_iter()
            .filter(|(g, _, _)| {
                Locale::ALL
                    .iter()
                    .any(|l| l.display_name() == g.text().trim())
            })
            .map(|(g, rect, shown)| (g.text().trim().to_string(), rect, shown))
            .collect();
        rows.sort_by(|a, b| a.2.top().total_cmp(&b.2.top()));
        rows
    }

    /// Scroll the open Language list with the mouse wheel until `name` is
    /// drawn wholly inside the window and is not a row the list's edge cuts
    /// through (the click then asserts it is wholly shown inside the window).
    /// On the way, the shown part of every row must lie inside the window.
    fn reveal_language(&mut self, editor: &mut Editor, name: &str) {
        let target = Locale::ALL
            .iter()
            .position(|l| l.display_name() == name)
            .expect("a listed language");
        for _ in 0..80 {
            let rows = self.language_rows(editor);
            assert!(!rows.is_empty(), "the Language list is not open");
            for (row, _, rect) in &rows {
                assert!(
                    screen().contains_rect(*rect),
                    "{row:?} is drawn at {rect:?}, outside the {:?} window",
                    screen()
                );
            }
            let index = |n: &str| Locale::ALL.iter().position(|l| l.display_name() == n);
            let first = index(&rows[0].0).unwrap();
            let last = index(&rows[rows.len() - 1].0).unwrap();
            let shown = (first < target || target == 0)
                && (target < last || target == Locale::ALL.len() - 1)
                && rows
                    .iter()
                    .any(|(row, whole, shown)| row == name && whole == shown);
            if shown {
                return;
            }
            // Positive y moves the content down, revealing rows above.
            let down = target >= last;
            let pos = rows[rows.len() / 2].2.center();
            self.frame(
                editor,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::vec2(0.0, if down { -1.0 } else { 1.0 }),
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
        }
        panic!("{name:?} never scrolled into the window");
    }

    /// Scroll the open menu under `over` with the mouse wheel to its end, as
    /// a user reaches a row near the bottom of a menu taller than the window.
    fn scroll_menu_to_end(&mut self, editor: &mut Editor, over: egui::Pos2, label: &str) {
        for _ in 0..60 {
            self.frame(
                editor,
                vec![
                    egui::Event::PointerMoved(over),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::vec2(0.0, -1.0),
                        modifiers: egui::Modifiers::default(),
                    },
                ],
            );
        }
        assert!(
            self.frame(editor, Vec::new())
                .1
                .iter()
                .any(|(g, _, _)| g.text().trim() == label),
            "{label:?} is not drawn at the end of the menu"
        );
    }

    /// Click the first galley whose trimmed text is `label`, hovering first
    /// so a submenu row opens the way a pointer opens it. The row must lie
    /// inside the window: a row drawn past its edge cannot be clicked.
    fn click_text(&mut self, editor: &mut Editor, label: &str) -> ChromeOutput {
        let rect = self
            .frame(editor, Vec::new())
            .1
            .into_iter()
            .find(|(g, _, _)| g.text().trim() == label)
            .map(|(_, rect, shown)| {
                assert_eq!(rect, shown, "{label:?} is cut by the edge of its list");
                rect
            })
            .unwrap_or_else(|| panic!("{label:?} was never painted"));
        assert!(
            screen().contains_rect(rect),
            "{label:?} is drawn at {rect:?}, outside the {:?} window",
            screen()
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
        // The Window menu is taller than this window too: Language sits near
        // its end, below where the list is cut, so the menu is scrolled there
        // with the pointer over its rows (below the title, mid-window).
        let title = self
            .frame(editor, Vec::new())
            .1
            .into_iter()
            .find(|(g, _, _)| g.text().trim() == window)
            .map(|(_, rect, _)| rect)
            .expect("the Window title is drawn");
        let over = egui::pos2(title.left() + title.width(), screen().center().y);
        self.scroll_menu_to_end(editor, over, &language);
        self.click_text(editor, &language);
        self.reveal_language(editor, name);
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

fn collect_texts(shape: &egui::Shape, clip: egui::Rect, out: &mut Painted) {
    match shape {
        egui::Shape::Text(t) => {
            let rect = egui::Rect::from_min_size(t.pos, t.galley.size());
            let shown = rect.intersect(clip);
            if shown.is_positive() {
                out.push((t.galley.clone(), rect, shown));
            }
        }
        egui::Shape::Vec(inner) => {
            for shape in inner {
                collect_texts(shape, clip, out);
            }
        }
        _ => {}
    }
}

/// Every character of every painted galley has a glyph in its font.
fn every_painted_glyph_exists(window: &mut Window, editor: &mut Editor) -> Result<(), String> {
    let (_, texts) = window.frame(editor, Vec::new());
    for (galley, _, _) in &texts {
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

    // The last row of the list (and the first) is reached and clicked the
    // same way: none is drawn past the window's bottom edge.
    let last = Locale::ALL[Locale::ALL.len() - 1];
    assert!(
        window.pick_language(&mut editor, current, last.display_name()),
        "the last Language row, {:?}, produced no preferences",
        last.display_name()
    );
    assert_eq!(editor.preferences().language, last.code());
    current = last;

    // And back to English through the same route.
    assert!(window.pick_language(&mut editor, current, "English"));
    assert_eq!(editor.preferences().language, "en");
    assert!(window.painted(&mut editor).iter().any(|t| t == "File"));
}
