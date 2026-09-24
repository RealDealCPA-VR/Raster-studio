//! W11-G: Help > Search Commands (Ctrl+Shift+P), the command palette.
//!
//! Every menu-bar row the host found enabled when it opened the palette is a
//! candidate, named by its menu path (`Filter > Blur > Gaussian Blur…`).
//! Typing filters them: every whitespace-separated term has to occur in the
//! path, ignoring case, and a command whose own label starts with the query
//! ranks first. Up / Down move the highlight, Enter (or a click) runs the
//! highlighted command, Escape closes. Running hands the [`MenuAction`] back
//! to the host, which routes it exactly as a menu click is routed, so a row
//! with a dialog opens its dialog.

use egui::Context;

use super::chrome::{hairline, modal, DialogKeys, DialogWidth};
use super::shortcut_sheet::matches_terms;
use super::sizes;
use crate::menu::{Entry, Menu, MenuAction};

/// One command the palette can run.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandEntry {
    /// What running it performs.
    pub action: MenuAction,
    /// Its menu path, `Filter > Blur > Gaussian Blur…`.
    pub path: String,
    /// Its own label, the last step of the path.
    pub label: String,
}

/// What one frame of the palette did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommandSearchOutcome {
    /// Still open.
    Open,
    /// Closed without running anything.
    Closed,
    /// Closed by running this command.
    Run(MenuAction),
}

/// Help > Search Commands.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommandSearchDialog {
    entries: Vec<CommandEntry>,
    query: String,
    selected: usize,
    focused: bool,
}

fn walk(
    entries: &[Entry],
    path: &str,
    enabled: &dyn Fn(MenuAction) -> bool,
    out: &mut Vec<CommandEntry>,
) {
    for entry in entries {
        match entry {
            Entry::Item(action) => {
                if !enabled(*action) || out.iter().any(|e| e.action == *action) {
                    continue;
                }
                let label = action.label();
                out.push(CommandEntry {
                    action: *action,
                    path: format!("{path} > {label}"),
                    label,
                });
            }
            Entry::Separator => {}
            Entry::Submenu { label, entries } => {
                walk(entries, &format!("{path} > {label}"), enabled, out);
            }
        }
    }
}

impl CommandSearchDialog {
    /// A palette over every row of `menus` that `enabled` accepts, in
    /// menu-bar order (a row reachable twice is listed once).
    pub fn from_menus(menus: &[Menu], enabled: impl Fn(MenuAction) -> bool) -> Self {
        let mut entries = Vec::new();
        for menu in menus {
            walk(&menu.entries, menu.title, &enabled, &mut entries);
        }
        Self {
            entries,
            ..Self::default()
        }
    }

    /// Every candidate, filter or not.
    pub fn entries(&self) -> &[CommandEntry] {
        &self.entries
    }

    /// The search text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replace the search text; the highlight goes back to the first match.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.selected = 0;
    }

    /// The candidates the search text keeps, best first: a command whose own
    /// label starts with the query, then the rest in menu-bar order.
    pub fn matches(&self) -> Vec<&CommandEntry> {
        let head = self.query.trim().to_lowercase();
        let mut out: Vec<&CommandEntry> = self
            .entries
            .iter()
            .filter(|e| matches_terms(&self.query, &[&e.path]))
            .collect();
        out.sort_by_key(|e| !e.label.to_lowercase().starts_with(&head));
        out
    }

    /// Move the highlight by `delta` rows, clamped to the matches.
    pub fn step(&mut self, delta: i32) {
        let n = self.matches().len();
        if n == 0 {
            self.selected = 0;
            return;
        }
        let next = self.selected as i64 + i64::from(delta);
        self.selected = next.clamp(0, n as i64 - 1) as usize;
    }

    /// The highlighted command, which Enter runs.
    pub fn selected(&self) -> Option<MenuAction> {
        self.matches().get(self.selected).map(|e| e.action)
    }

    /// The window's title: the Help row's own label.
    pub fn title(&self) -> String {
        MenuAction::CommandSearch.label()
    }

    /// Draw one frame.
    pub fn show(&mut self, ctx: &Context) -> CommandSearchOutcome {
        let keys = DialogKeys::read(ctx);
        if keys.cancel {
            return CommandSearchOutcome::Closed;
        }
        let (up, down) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
            )
        });
        if up {
            self.step(-1);
        }
        if down {
            self.step(1);
        }
        if keys.confirm {
            return match self.selected() {
                Some(action) => CommandSearchOutcome::Run(action),
                None => CommandSearchOutcome::Open,
            };
        }
        let title = self.title();
        let first = !self.focused;
        self.focused = true;
        let drawn = modal(
            ctx,
            "command-search",
            &title,
            None,
            DialogWidth::Wide,
            |ui| {
                let before = self.query.clone();
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .id_salt("command-search-query")
                        .hint_text("Search…")
                        .desired_width(f32::INFINITY),
                );
                if first {
                    field.request_focus();
                }
                if self.query != before {
                    self.selected = 0;
                }
                hairline(ui);
                let rows: Vec<(MenuAction, String)> = self
                    .matches()
                    .into_iter()
                    .map(|e| (e.action, e.path.clone()))
                    .collect();
                let mut clicked = None;
                egui::ScrollArea::vertical()
                    .id_salt("command-search-rows")
                    .max_height(sizes::list_max_height())
                    .show(ui, |ui| {
                        for (i, (action, path)) in rows.iter().enumerate() {
                            if ui
                                .selectable_label(i == self.selected, path.as_str())
                                .clicked()
                            {
                                clicked = Some(*action);
                            }
                        }
                    });
                clicked
            },
        );
        match drawn.flatten() {
            Some(action) => CommandSearchOutcome::Run(action),
            None => CommandSearchOutcome::Open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::{menu_bar, FilterId};

    fn palette() -> CommandSearchDialog {
        CommandSearchDialog::from_menus(&menu_bar(0), |_| true)
    }

    #[test]
    fn gaussian_finds_the_gaussian_blur_filter_first() {
        let mut p = palette();
        assert!(p.entries().len() > 100, "{}", p.entries().len());
        p.set_query("gaussian");
        let top = p.matches()[0];
        assert_eq!(top.action, MenuAction::Filter(FilterId::GaussianBlur));
        assert!(top.path.starts_with("Filter > "), "{}", top.path);
        assert_eq!(
            p.selected(),
            Some(MenuAction::Filter(FilterId::GaussianBlur))
        );
    }

    #[test]
    fn a_disabled_row_is_not_offered_and_the_highlight_is_clamped() {
        let mut p = CommandSearchDialog::from_menus(&menu_bar(0), |a| {
            a != MenuAction::Filter(FilterId::GaussianBlur)
        });
        p.set_query("gaussian blur");
        assert!(p
            .matches()
            .iter()
            .all(|e| e.action != MenuAction::Filter(FilterId::GaussianBlur)));
        p.set_query("zzzz no such command");
        assert_eq!(p.selected(), None);
        p.step(3);
        assert_eq!(p.selected(), None);
    }

    #[test]
    fn it_draws_in_both_appearances_and_enter_runs_the_highlight() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut p = palette();
            assert_eq!(p.show(ctx), CommandSearchOutcome::Open);
        });
        let ctx = Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut p = palette();
        p.set_query("gaussian");
        let enter = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
            ..Default::default()
        };
        let mut outcome = CommandSearchOutcome::Open;
        let _ = ctx.run(enter, |ctx| outcome = p.show(ctx));
        assert_eq!(
            outcome,
            CommandSearchOutcome::Run(MenuAction::Filter(FilterId::GaussianBlur))
        );
    }
}
