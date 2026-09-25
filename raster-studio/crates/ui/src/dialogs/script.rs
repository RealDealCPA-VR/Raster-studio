//! W13-K: File ▸ Script… — a code box, a Run button and an output log.
//!
//! The window is the `ui` half of Photopea's script runner: it holds the text
//! being edited and the lines a run printed, and reports what the user
//! pressed. It runs nothing itself. The shell (`app-shell::script`) owns the
//! JavaScript engine, performs the run through the editor's own commands,
//! and hands the lines back with [`ScriptDialog::push_log`].
//!
//! Enter is a newline here, not a confirm: the box is a code editor, so the
//! dialog grammar's Enter-confirms rule would make a multi-line script
//! impossible to type. Run is the button (or Ctrl+Enter); Escape closes.
//!
//! Every prose sentence the window shows (the safety note, the log) arrives
//! as data from the shell, so the labels drawn here are single words.

use design::{
    color32, current_tokens, primary_button, secondary_button,
    tokens::palette::ColorRole,
    tokens::{Space, TextRole, TypeRole},
};
use egui::{Align, Context, Key, Layout};

use super::chrome::{caption, hairline, modal, DialogWidth};
use super::sizes;

/// What kind of line a run printed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScriptLogKind {
    /// `console.log`, `$.writeln`, `app.echoToOE`.
    Output,
    /// `alert(...)`.
    Alert,
    /// A syntax error, an uncaught exception, a budget stop.
    Error,
    /// What the runner itself says: "Ran in one step", "Loaded x.jsx".
    Info,
}

/// One line of the log.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScriptLogLine {
    pub kind: ScriptLogKind,
    pub text: String,
}

/// What one frame of the window did.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ScriptOutcome {
    /// Still open; nothing pressed.
    Open,
    /// Run was pressed (or Ctrl+Enter) over this source.
    Run(String),
    /// Close (or Escape).
    Closed,
    /// W16-K: Save pressed: keep `source` under `name` in the saved list.
    Save { name: String, source: String },
    /// W16-K: a saved script's delete button.
    Delete(String),
}

/// File ▸ Script….
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ScriptDialog {
    source: String,
    log: Vec<ScriptLogLine>,
    /// A sentence under the title — the shell's statement of what a script
    /// may touch.
    note: String,
    /// Where the Run button was last drawn, so a headless test can press it.
    run_rect: Option<egui::Rect>,
    /// W16-K: the demos listed at the top, `(name, source)`; a click loads
    /// one into the code box (Photopea: "Several demos are available in the
    /// top of the Script window").
    demos: Vec<(String, String)>,
    /// W16-K: the saved scripts listed at the bottom, `(name, source)`; a
    /// click loads one, its delete button asks the shell to remove it.
    saved: Vec<(String, String)>,
    /// W16-K: the name Save keeps the source under.
    save_name: String,
}

impl ScriptDialog {
    /// Most log lines kept; older lines drop off the top.
    pub const MAX_LOG_LINES: usize = 500;

    /// An empty window carrying `note` under its title.
    pub fn new(note: impl Into<String>) -> Self {
        Self {
            note: note.into(),
            ..Self::default()
        }
    }

    /// The script as typed so far.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn set_source(&mut self, source: impl Into<String>) {
        self.source = source.into();
    }

    /// Every line the log holds, oldest first.
    pub fn log(&self) -> &[ScriptLogLine] {
        &self.log
    }

    /// Append one line.
    pub fn push_log(&mut self, kind: ScriptLogKind, text: impl Into<String>) {
        self.log.push(ScriptLogLine {
            kind,
            text: text.into(),
        });
        if self.log.len() > Self::MAX_LOG_LINES {
            let over = self.log.len() - Self::MAX_LOG_LINES;
            self.log.drain(..over);
        }
    }

    pub fn clear_log(&mut self) {
        self.log.clear();
    }

    /// W16-K: the demos the top row lists, `(name, source)`.
    pub fn set_demos(&mut self, demos: Vec<(String, String)>) {
        self.demos = demos;
    }

    pub fn demos(&self) -> &[(String, String)] {
        &self.demos
    }

    /// W16-K: the saved scripts the bottom list shows, `(name, source)`.
    pub fn set_saved(&mut self, saved: Vec<(String, String)>) {
        self.saved = saved;
    }

    pub fn saved(&self) -> &[(String, String)] {
        &self.saved
    }

    /// W16-K: the name Save will use.
    pub fn set_save_name(&mut self, name: impl Into<String>) {
        self.save_name = name.into();
    }

    /// W16-K: the id of demo `index`'s button.
    pub fn demo_id(index: usize) -> egui::Id {
        egui::Id::new(("raster-script-demo", index))
    }

    /// W16-K: the id of saved script `index`'s load button.
    pub fn saved_id(index: usize) -> egui::Id {
        egui::Id::new(("raster-script-saved", index))
    }

    /// W16-K: the id of saved script `index`'s delete button.
    pub fn delete_id(index: usize) -> egui::Id {
        egui::Id::new(("raster-script-delete", index))
    }

    /// W16-K: the id of the Save button.
    pub fn save_id() -> egui::Id {
        egui::Id::new("raster-script-save")
    }

    /// The egui id of the code box.
    pub fn source_id() -> egui::Id {
        egui::Id::new("raster-script-source")
    }

    /// Where Run was drawn on the last frame.
    pub fn run_rect(&self) -> Option<egui::Rect> {
        self.run_rect
    }

    /// Draw one frame.
    pub fn show(&mut self, ctx: &Context) -> ScriptOutcome {
        let (escape, ctrl_enter) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.modifiers.command && i.key_pressed(Key::Enter),
            )
        });
        let note = self.note.clone();
        let drawn = modal(
            ctx,
            "script",
            "Script",
            (!note.is_empty()).then_some(note.as_str()),
            DialogWidth::Broad,
            |ui| self.body(ui),
        );
        if escape {
            return ScriptOutcome::Closed;
        }
        let pressed = drawn.flatten();
        if ctrl_enter || pressed == Some(Pressed::Run) {
            return ScriptOutcome::Run(self.source.clone());
        }
        match pressed {
            Some(Pressed::Close) => ScriptOutcome::Closed,
            Some(Pressed::Clear) => {
                self.clear_log();
                ScriptOutcome::Open
            }
            Some(Pressed::Save) => ScriptOutcome::Save {
                name: self.save_name.trim().to_string(),
                source: self.source.clone(),
            },
            Some(Pressed::Delete(i)) => match self.saved.get(i) {
                Some((name, _)) => ScriptOutcome::Delete(name.clone()),
                None => ScriptOutcome::Open,
            },
            _ => ScriptOutcome::Open,
        }
    }

    /// W16-K: the demos row: each loads its source into the code box.
    fn demos_row(&mut self, ui: &mut egui::Ui) {
        if self.demos.is_empty() {
            return;
        }
        let mut load = None;
        ui.horizontal_wrapped(|ui| {
            let _ = caption(ui, "Demos");
            for (i, (name, _)) in self.demos.iter().enumerate() {
                if crate::view::labelled_button(ui, name, true, Self::demo_id(i)).clicked() {
                    load = Some(i);
                }
            }
        });
        if let Some(i) = load {
            self.source = self.demos[i].1.clone();
            self.save_name = self.demos[i].0.clone();
        }
        hairline(ui);
    }

    /// W16-K: the saved scripts (load, delete) and the Save row.
    fn saved_rows(&mut self, ui: &mut egui::Ui) -> Option<Pressed> {
        let mut pressed = None;
        let mut load = None;
        hairline(ui);
        let _ = caption(ui, "Saved");
        for (i, (name, _)) in self.saved.iter().enumerate() {
            ui.horizontal(|ui| {
                if crate::view::labelled_button(ui, name, true, Self::saved_id(i)).clicked() {
                    load = Some(i);
                }
                if crate::view::icon_button_id(ui, "trash", true, Self::delete_id(i)).clicked() {
                    pressed = Some(Pressed::Delete(i));
                }
            });
        }
        if let Some(i) = load {
            self.source = self.saved[i].1.clone();
            self.save_name = self.saved[i].0.clone();
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.save_name)
                    .id(egui::Id::new("raster-script-save-name"))
                    .desired_width(sizes::text_field_wide()),
            );
            let can_save = !self.save_name.trim().is_empty() && !self.source.trim().is_empty();
            if crate::view::labelled_button(ui, "Save", can_save, Self::save_id()).clicked() {
                pressed = Some(Pressed::Save);
            }
        });
        pressed
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<Pressed> {
        self.demos_row(ui);
        let editor = egui::TextEdit::multiline(&mut self.source)
            .id(Self::source_id())
            .code_editor()
            .desired_rows(14)
            .desired_width(f32::INFINITY);
        egui::ScrollArea::vertical()
            .id_salt("script-source-scroll")
            .max_height(sizes::list_max_height())
            .show(ui, |ui| {
                let response = ui.add(editor);
                if ui.memory(|m| m.focused()).is_none() {
                    response.request_focus();
                }
            });
        hairline(ui);
        let t = current_tokens(ui);
        let output = color32(t.palette.text(TextRole::Primary));
        let quiet = color32(t.palette.text(TextRole::Secondary));
        let alert = color32(t.palette.color(ColorRole::Accent));
        let error = color32(t.palette.color(ColorRole::Warning));
        let font = design::egui_theme::font_id(t, TypeRole::Footnote);
        if self.log.is_empty() {
            let _ = caption(ui, "Output");
        }
        egui::ScrollArea::vertical()
            .id_salt("script-log-scroll")
            .max_height(sizes::list_max_height())
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.log {
                    let color = match line.kind {
                        ScriptLogKind::Output => output,
                        ScriptLogKind::Alert => alert,
                        ScriptLogKind::Error => error,
                        ScriptLogKind::Info => quiet,
                    };
                    ui.label(
                        egui::RichText::new(line.text.as_str())
                            .color(color)
                            .font(font.clone()),
                    );
                }
            });
        ui.add_space(Space::Medium.pt());
        let mut pressed = None;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = Space::Small.pt();
            let run = primary_button(ui, "Run");
            self.run_rect = Some(run.rect);
            if run.clicked() {
                pressed = Some(Pressed::Run);
            }
            if secondary_button(ui, "Close").clicked() {
                pressed = Some(Pressed::Close);
            }
            if secondary_button(ui, "Clear").clicked() {
                pressed = Some(Pressed::Clear);
            }
        });
        // W16-K: the saved scripts sit at the bottom, as in Photopea.
        if let Some(saved) = self.saved_rows(ui) {
            pressed = Some(saved);
        }
        pressed
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pressed {
    Run,
    Close,
    Clear,
    // W16-K.
    Save,
    Delete(usize),
}

#[cfg(test)]
mod tests {
    use super::super::chrome::test_support::{frame_both_themes, Harness};
    use super::*;

    #[test]
    fn it_draws_in_both_appearances_with_a_log() {
        frame_both_themes(|ctx| {
            let mut dialog = ScriptDialog::new("A note");
            dialog.push_log(ScriptLogKind::Output, "hello");
            dialog.push_log(ScriptLogKind::Error, "SyntaxError: nope");
            assert_eq!(dialog.show(ctx), ScriptOutcome::Open);
        });
    }

    #[test]
    fn pressing_run_hands_back_the_source() {
        let harness = Harness::new();
        let mut dialog = ScriptDialog::new("");
        dialog.set_source("alert(1)");
        let mut outcome = ScriptOutcome::Open;
        for _ in 0..6 {
            harness.frame(Vec::new(), |ctx| outcome = dialog.show(ctx));
        }
        assert_eq!(outcome, ScriptOutcome::Open);
        let at = dialog.run_rect().expect("Run was drawn").center();
        harness.frame(Harness::click_events(at), |ctx| outcome = dialog.show(ctx));
        assert_eq!(outcome, ScriptOutcome::Run("alert(1)".to_string()));
    }

    #[test]
    fn escape_closes_and_enter_does_not_run() {
        let harness = Harness::new();
        let mut dialog = ScriptDialog::new("");
        let mut outcome = ScriptOutcome::Open;
        harness.frame(Harness::key_events(Key::Enter), |ctx| {
            outcome = dialog.show(ctx)
        });
        assert_eq!(outcome, ScriptOutcome::Open, "Enter is a newline");
        harness.frame(Harness::key_events(Key::Escape), |ctx| {
            outcome = dialog.show(ctx)
        });
        assert_eq!(outcome, ScriptOutcome::Closed);
    }

    #[test]
    fn the_log_is_bounded() {
        let mut dialog = ScriptDialog::new("");
        for i in 0..(ScriptDialog::MAX_LOG_LINES + 10) {
            dialog.push_log(ScriptLogKind::Output, i.to_string());
        }
        assert_eq!(dialog.log().len(), ScriptDialog::MAX_LOG_LINES);
        assert_eq!(dialog.log()[0].text, "10");
    }
}
