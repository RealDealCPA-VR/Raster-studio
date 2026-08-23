//! The application: state, behaviour, and the native shell around it.
//!
//! # Shape
//!
//! ```text
//!   apps/studio-desktop            argv, tracing
//!            |
//!   shell    | winit + wgpu + egui   window, surface, key events, frames
//!            v
//!   editor   | Editor                open documents, active tool, colours,
//!            |                       keymap, preferences, autosave, recovery
//!            v
//!   doc      | OpenDocument          Document + History + tiles + camera
//!            v
//!   editor-core / compositor / raster
//! ```
//!
//! Everything below [`shell`] runs without a window, and that is deliberate:
//! menu enablement, command emission, tab switching, open/save/export, the
//! keymap, autosave and crash recovery are all decided in [`editor`], so all of
//! it is under test.
//!
//! # Three rules this crate keeps
//!
//! * **The image is in the document.** An opened file becomes a real raster
//!   layer whose pixels live in the tile store ([`import`]); the canvas draws
//!   the *compositor's* output of that document ([`presenter`]) and nothing
//!   else. There is no second picture beside the document — which is what made
//!   the layers panel say "No layers yet" under a visible photograph.
//! * **The UI emits commands.** [`chrome`] takes `&Editor` and returns a
//!   [`chrome::ChromeOutput`]; every document change goes through
//!   [`editor_core::History`], so undo and redo are uniform. A field of that
//!   output is set only when the user *did* something this frame — it is never
//!   a mirror of current state, because a mirror captured before an action is
//!   applied after it and undoes it.
//! * **Nothing does nothing.** [`Action`] is the whole catalogue,
//!   [`editor::Editor::dispatch`] matches it exhaustively with no wildcard arm,
//!   and an action that cannot apply right now returns the reason a disabled
//!   menu item shows.
//!
//! # Known gaps
//!
//! Stated rather than implied:
//!
//! * **Tools do not receive pointer events.** See [`shell`]: a canvas drag pans
//!   the view whatever tool is selected. The tool palette, the tool letters,
//!   the brush-size keys and the colour wells all work; the gesture that would
//!   paint does not exist yet, and no tool consumes the foreground colour.
//! * **There is no pen/path tool.** The brief names `P` among the tool letters,
//!   but `tools::registry` ships no tool answering to it, so `P` is unbound.
//!   Recorded rather than hidden — see `keymap`'s
//!   `the_briefs_tool_letters_are_present_except_the_ones_recorded_as_missing`.
//! * **The scratch location is shown, not edited.** The preferences window
//!   ([`chrome`]) covers theme, UI scale, autosave interval, history depth and
//!   the whole keymap; the scratch directory is displayed read-only because
//!   changing it needs a folder picker that is not wired.
//! * **File ▸ New has no size dialog.** It makes a
//!   [`editor::NEW_DOCUMENT_SIZE`] canvas.
//! * **Tab does not move keyboard focus between widgets.** Tab is a shortcut
//!   here (Hide/Show Panels, and with Ctrl the document tabs), and egui's focus
//!   navigation would both swallow it and then claim every later key press. See
//!   [`shell::withhold_from_egui`]. Text fields are reached with the pointer.

pub mod action;
pub mod chrome;
pub mod dialogs;
pub mod dirty;
pub mod doc;
pub mod editor;
pub mod error;
pub mod import;
pub mod keymap;
pub mod menu_bridge;
pub mod prefs;
pub mod presenter;
pub mod recent;
pub mod session;
pub mod shell;

pub use action::{Action, Category, ToolKey};
pub use chrome::{Chrome, ChromeOutput, HistoryRow, LayerRow, Rebind, ShortcutRow};
pub use dialogs::{CloseChoice, FileDialogs, NativeDialogs, ScriptedDialogs};
pub use dirty::DirtyTiles;
pub use doc::{DocumentError, DocumentId, OpenDocument};
pub use editor::{
    color_hex, ActionError, AutosaveReport, Editor, Effect, Menu, MenuItem, NoSuchTab,
    RecoveryReport,
};
pub use error::ShellError;
pub use import::{DecodedImage, ImportError, ImportedDocument};
pub use keymap::{Binding, Chord, Conflict, Key, KeyOverride, Keymap};
pub use menu_bridge::Pick;
pub use prefs::{AppPaths, Preferences, ThemeChoice, WindowGeometry};
pub use presenter::CanvasPresenter;
pub use recent::{RecentFiles, MAX_RECENT_FILES};
pub use session::{SessionMarker, SessionRecord};
pub use shell::Shell;

use std::path::PathBuf;

/// Start the application: real dialogs, the user's configuration directory, and
/// whatever files were named on the command line.
pub fn launch(files: Vec<PathBuf>) -> Result<(), ShellError> {
    Shell::new(Editor::native(), files).run()
}
