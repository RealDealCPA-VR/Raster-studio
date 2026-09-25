//! W13-K: File ▸ Script — Photoshop-DOM JavaScript, run in-process.
//!
//! Photopea runs scripts written against Photoshop's DOM (`app`,
//! `app.activeDocument`, `doc.artLayers.add()`, `doc.selection.fill(...)`,
//! `layer.name = ...`, `alert(...)`). This module does the same with an
//! embedded pure-Rust JavaScript engine (`boa_engine`, Unlicense OR MIT):
//!
//! * [`engine`] runs the script on a thread of its own under a step budget
//!   and a time limit, with one native function — a channel back here;
//! * [`host`] answers every DOM call against the live [`Editor`] through the
//!   routes the menus already use, and folds the run into ONE history step
//!   per document it changed;
//! * the window ([`ui::dialogs::ScriptDialog`]: a code box, Run, an output
//!   log) opens from File ▸ Script… and is drawn by `menu_bridge::draw`.
//!   Run parks the source here and clicks the same `MenuAction::Script` row
//!   through the chrome, so the run is a `menu_bridge::perform` like any
//!   other menu action.
//!
//! A `.jsx` / `.js` opened through File ▸ Open, a drag-and-drop or Open
//! Recent does not run: it opens in the window with its source shown, and
//! runs when the user presses Run — the confirm. A script has no filesystem
//! or network access of its own; `app.open` and `saveAs` go through the
//! platform pickers (see [`host`]).
//!
//! # What is not here
//!
//! The DOM is a subset (listed in the parity matrix). There are no action
//! descriptors (`executeAction`), no `File` reads or writes, no
//! `doc.close()`, no adjustment or filter calls, and `doc.resolution` is
//! always 72 because the document stores none. A single builtin call that
//! runs long by itself (`"x".repeat(1e9)`) cannot be interrupted until it
//! returns.

use std::cell::RefCell;
use std::path::Path;

use ui::dialogs::{ScriptDialog, ScriptLogKind, ScriptOutcome};

use crate::editor::{ActionError, Editor, Effect};

mod engine;
mod host;
#[cfg(test)]
mod tests;

pub use engine::ScriptLimits;
pub use host::SCRIPT_LABEL;

/// The file extensions File ▸ Open routes to the script window.
pub const SCRIPT_EXTENSIONS: &[&str] = &["jsx", "js"];

/// The largest script file File ▸ Open reads.
pub const MAX_SCRIPT_BYTES: u64 = 1024 * 1024;

/// What the window says under its title.
pub const SAFETY_NOTE: &str = "Photoshop-DOM JavaScript. A run is one undo step per document; \
     a script can reach files only through app.open and saveAs, which ask you with the file picker.";

/// Whether `path` names a script (by extension, any case).
pub fn is_script_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SCRIPT_EXTENSIONS.iter().any(|s| s.eq_ignore_ascii_case(e)))
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptEnd {
    /// It ran to its end.
    Completed,
    /// It did not parse; nothing ran.
    Syntax(String),
    /// It threw and nothing caught it. Whatever it did before stays, as one
    /// undo step.
    Failed(String),
    /// A limit stopped it (an endless loop). Whatever it did before stays, as
    /// one undo step.
    Stopped(String),
    /// The engine itself failed; nothing is claimed about the document beyond
    /// the steps already folded.
    Crashed,
}

/// What one run did.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptReport {
    pub end: ScriptEnd,
    /// Every line the run printed, in order, then the runner's own summary.
    pub lines: Vec<ui::dialogs::ScriptLogLine>,
    /// How many documents the run changed (each is one history step).
    pub documents_changed: usize,
}

impl ScriptReport {
    /// The status-line sentence.
    pub fn summary(&self) -> String {
        match &self.end {
            ScriptEnd::Completed => {
                format!("Script ran; {} document(s) changed", self.documents_changed)
            }
            ScriptEnd::Syntax(e) => format!("Script not run: {e}"),
            ScriptEnd::Failed(e) => format!("Script failed: {e}"),
            ScriptEnd::Stopped(e) => format!("Script {e}"),
            ScriptEnd::Crashed => "Script: the script engine failed".to_string(),
        }
    }
}

thread_local! {
    static WINDOW: RefCell<Option<ScriptDialog>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<String>> = const { RefCell::new(None) };
    static RUNNING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the script window is open.
pub fn window_open() -> bool {
    WINDOW.with(|w| w.borrow().is_some())
}

/// Read (and optionally change) the open window; `None` when it is closed.
pub fn with_window<R>(f: impl FnOnce(&mut ScriptDialog) -> R) -> Option<R> {
    WINDOW.with(|w| w.borrow_mut().as_mut().map(f))
}

fn open_window() {
    WINDOW.with(|w| {
        let mut w = w.borrow_mut();
        if w.is_none() {
            *w = Some(ScriptDialog::new(SAFETY_NOTE));
        }
    });
}

/// `menu_bridge::perform`'s arm for File ▸ Script: run the source Run parked
/// this frame, or open the window when nothing is parked.
pub fn perform(editor: &mut Editor) -> Result<String, String> {
    match PENDING.with(|p| p.borrow_mut().take()) {
        Some(source) => {
            let report = run(editor, &source, ScriptLimits::default());
            let summary = report.summary();
            open_window();
            with_window(|w| {
                for line in &report.lines {
                    w.push_log(line.kind, line.text.clone());
                }
            });
            match report.end {
                ScriptEnd::Completed => Ok(summary),
                _ => Err(summary),
            }
        }
        None => {
            open_window();
            Ok("Script…: type or open a script, then press Run".to_string())
        }
    }
}

/// Draw the window when it is open. Run parks the source and clicks the
/// File ▸ Script row through `on_click`, which the chrome routes to
/// [`perform`].
pub fn draw_window(ctx: &egui::Context, on_click: &mut dyn FnMut(ui::Intent)) {
    let outcome = WINDOW.with(|w| w.borrow_mut().as_mut().map(|d| d.show(ctx)));
    match outcome {
        Some(ScriptOutcome::Run(source)) => {
            PENDING.with(|p| *p.borrow_mut() = Some(source));
            on_click(ui::Intent::Action(ui::menu::MenuAction::Script));
        }
        Some(ScriptOutcome::Closed) => WINDOW.with(|w| *w.borrow_mut() = None),
        Some(ScriptOutcome::Open) | None => {}
    }
}

/// File ▸ Open (or a drop) of a `.jsx` / `.js`: show it in the window. It
/// runs when the user presses Run.
pub fn open_file(path: &Path) -> Result<Effect, ActionError> {
    let failed = |reason: String| ActionError::Failed {
        action: crate::action::Action::Open,
        reason,
    };
    if RUNNING.with(|r| r.get()) {
        return Err(failed("a script cannot open another script".to_string()));
    }
    let size = std::fs::metadata(path)
        .map_err(|e| failed(format!("{}: {e}", path.display())))?
        .len();
    if size > MAX_SCRIPT_BYTES {
        return Err(failed(format!(
            "{} is {size} bytes; a script file may be at most {MAX_SCRIPT_BYTES}",
            path.display()
        )));
    }
    let bytes = std::fs::read(path).map_err(|e| failed(format!("{}: {e}", path.display())))?;
    let source = String::from_utf8(bytes)
        .map_err(|_| failed(format!("{} is not UTF-8 text", path.display())))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    open_window();
    with_window(|w| {
        w.set_source(source);
        w.push_log(
            ScriptLogKind::Info,
            format!("Opened {name}. Read it, then press Run to run it."),
        );
    });
    Ok(Effect::Tool)
}

/// Run `source` against `editor` now.
pub fn run(editor: &mut Editor, source: &str, limits: ScriptLimits) -> ScriptReport {
    use std::sync::mpsc::channel;

    RUNNING.with(|r| r.set(true));
    let (to_host, from_engine) = channel();
    let (reply, replies) = channel::<String>();
    let owned = source.to_string();
    let spawned = std::thread::Builder::new()
        .name("raster-script".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || engine::run_on_thread(owned, limits, to_host, replies));
    let mut host = host::Host::new(editor);
    let mut end = None;
    match spawned {
        Ok(handle) => {
            for message in from_engine.iter() {
                match message {
                    engine::ToHost::Call { op, args } => {
                        if reply.send(host.answer(&op, &args)).is_err() {
                            break;
                        }
                    }
                    engine::ToHost::Finished(done) => end = Some(done),
                }
            }
            if handle.join().is_err() {
                end = None;
            }
        }
        Err(_) => end = None,
    }
    let documents_changed = host.finish();
    let mut lines = std::mem::take(&mut host.log);
    RUNNING.with(|r| r.set(false));
    let end = match end {
        Some(engine::EngineEnd::Completed) => ScriptEnd::Completed,
        Some(engine::EngineEnd::Syntax(e)) => ScriptEnd::Syntax(e),
        Some(engine::EngineEnd::Uncaught(e)) => ScriptEnd::Failed(e),
        Some(engine::EngineEnd::Stopped(e)) => ScriptEnd::Stopped(e),
        None => ScriptEnd::Crashed,
    };
    let report = ScriptReport {
        end,
        lines: Vec::new(),
        documents_changed,
    };
    let kind = match report.end {
        ScriptEnd::Completed => ScriptLogKind::Info,
        _ => ScriptLogKind::Error,
    };
    lines.push(ui::dialogs::ScriptLogLine {
        kind,
        text: report.summary(),
    });
    ScriptReport { lines, ..report }
}
