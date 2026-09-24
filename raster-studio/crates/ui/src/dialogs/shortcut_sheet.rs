//! W11-G: Help > Keyboard Shortcut Sheet (Shift+/, the `?` key).
//!
//! A read-only, searchable list of every chord the application answers. The
//! rows are not typed here: the host builds them from the live keymap (the
//! application table with the user's overrides, and the menu table
//! underneath it) at open time, so a rebind shows on the next open and the
//! sheet can never list a chord that does something else. Typing filters the
//! rows by command name or chord; Escape, Enter and Close dismiss it.

use egui::Context;

use super::chrome::{hairline, modal, DialogKeys, DialogWidth};
use super::sizes;
use crate::menu::MenuAction;

/// One row of the sheet: a chord and the command it performs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutRow {
    /// The chord as the menu bar spells it (`Ctrl+Shift+P`).
    pub chord: String,
    /// The command's name (`Search Commands…`).
    pub command: String,
}

/// Help > Keyboard Shortcut Sheet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShortcutSheet {
    rows: Vec<ShortcutRow>,
    query: String,
    focused: bool,
}

/// Whether every whitespace-separated term of `query` occurs in one of
/// `haystacks`, ignoring case. An empty query matches everything.
pub(crate) fn matches_terms(query: &str, haystacks: &[&str]) -> bool {
    let lowered: Vec<String> = haystacks.iter().map(|h| h.to_lowercase()).collect();
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .all(|term| lowered.iter().any(|h| h.contains(&term)))
}

impl ShortcutSheet {
    /// A sheet over `rows`, sorted by command then chord, duplicates dropped.
    pub fn new(mut rows: Vec<ShortcutRow>) -> Self {
        rows.sort_by(|a, b| a.command.cmp(&b.command).then(a.chord.cmp(&b.chord)));
        rows.dedup();
        Self {
            rows,
            query: String::new(),
            focused: false,
        }
    }

    /// Every row, filter or not.
    pub fn rows(&self) -> &[ShortcutRow] {
        &self.rows
    }

    /// The search text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replace the search text.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
    }

    /// The rows the search text keeps.
    pub fn visible(&self) -> Vec<&ShortcutRow> {
        self.rows
            .iter()
            .filter(|r| matches_terms(&self.query, &[&r.command, &r.chord]))
            .collect()
    }

    /// The window's title: the Help row's own label.
    pub fn title(&self) -> String {
        MenuAction::ShortcutSheet.label()
    }

    /// Draw one frame. Returns `true` when the window was dismissed.
    pub fn show(&mut self, ctx: &Context) -> bool {
        let keys = DialogKeys::read(ctx);
        let mut dismissed = keys.cancel || keys.confirm;
        let title = self.title();
        let first = !self.focused;
        self.focused = true;
        let drawn = modal(
            ctx,
            "shortcut-sheet",
            &title,
            None,
            DialogWidth::Wide,
            |ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .id_salt("shortcut-sheet-search")
                        .hint_text("Search…")
                        .desired_width(f32::INFINITY),
                );
                if first {
                    field.request_focus();
                }
                hairline(ui);
                let visible: Vec<ShortcutRow> = self.visible().into_iter().cloned().collect();
                egui::ScrollArea::vertical()
                    .id_salt("shortcut-sheet-rows")
                    .max_height(sizes::list_max_height())
                    .show(ui, |ui| {
                        egui::Grid::new("shortcut-sheet-grid")
                            .striped(true)
                            .num_columns(2)
                            .show(ui, |ui| {
                                for row in &visible {
                                    ui.label(row.command.as_str());
                                    ui.label(egui::RichText::new(row.chord.as_str()).monospace());
                                    ui.end_row();
                                }
                            });
                    });
                hairline(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    design::primary_button(ui, "Close").clicked()
                })
                .inner
            },
        );
        if drawn == Some(true) {
            dismissed = true;
        }
        dismissed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> ShortcutSheet {
        ShortcutSheet::new(vec![
            ShortcutRow {
                chord: "Ctrl+S".into(),
                command: "Save".into(),
            },
            ShortcutRow {
                chord: "Ctrl+Shift+P".into(),
                command: "Search Commands…".into(),
            },
            ShortcutRow {
                chord: "Ctrl+S".into(),
                command: "Save".into(),
            },
        ])
    }

    #[test]
    fn the_search_filters_by_command_or_chord_and_ignores_case() {
        let mut s = sheet();
        assert_eq!(s.rows().len(), 2, "the duplicate row is dropped");
        s.set_query("SEARCH");
        assert_eq!(s.visible().len(), 1);
        assert_eq!(s.visible()[0].command, "Search Commands…");
        s.set_query("ctrl+s");
        assert_eq!(s.visible().len(), 2, "both chords start Ctrl+S");
        s.set_query("save ctrl");
        assert_eq!(s.visible().len(), 1);
        s.set_query("nothing like this");
        assert!(s.visible().is_empty());
    }

    #[test]
    fn it_draws_in_both_appearances_and_stays_open_until_dismissed() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut s = sheet();
            assert!(!s.show(ctx));
        });
        assert_eq!(sheet().title(), "Keyboard Shortcut Sheet…");
    }
}
