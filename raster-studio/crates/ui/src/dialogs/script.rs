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
            _ => ScriptOutcome::Open,
        }
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<Pressed> {
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
        pressed
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pressed {
    Run,
    Close,
    Clear,
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
