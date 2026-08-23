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
//! * **There is one chrome.** [`chrome::Chrome`] owns a [`ui::Workspace`] and
//!   draws it: the nine menus, the tool palette and its fly-outs, the options
//!   bar and all thirteen docked panels are the `ui` crate's, reached from the
//!   binary. What this crate still draws itself is what that crate has no model
//!   for — the document tab strip, preferences, and the transient status
//!   message. [`menu_bridge::pick`] is the single translation from
//!   [`ui::Intent`] to something the shell performs.
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
//! * **A view rotation cannot be shown.** [`tool_input`] drives the Rotate View
//!   tool like any other, but [`render::Camera`] is axis-aligned by
//!   construction, so the angle has nowhere to be written back to. Hand and
//!   Zoom reach the screen in full.
//! * **A selection gesture is not undoable.** `editor-core` models the
//!   selection as a field rather than a command, so a marquee changes the
//!   document directly and marks it dirty. Named in [`tools::SelectionEdit`]'s
//!   own documentation, not invented here.
//! * **A stroke is invisible until the button is released.**
//!   `tools::StrokeTool::commit` emits the stroke's single `PaintTiles` command
//!   from `on_pointer_up` alone, and the document's pixel references are
//!   rewritten by that command and nothing else — so the canvas is unchanged
//!   for the whole drag and the stroke appears at the release. There is no live
//!   preview layer to draw one into. See [`tool_input`].
//! * **A shape gesture draws nothing.** The seven shape tools emit one undoable
//!   `CreateLayer` holding a visible `layer_model::LayerKind::Shape` with the
//!   dragged path, so the layer appears in the Layers panel — and nowhere else,
//!   because `compositor::composite` answers `LayerKind::Text | Shape |
//!   SmartObject` with an empty arm ("No rasterizer for these yet; they
//!   contribute nothing"). The composited pixels are byte-identical after the
//!   gesture. See [`tool_input`].
//! * **The right button does nothing on the canvas.** There is no context menu
//!   to give it, and [`ui::canvas::InputRouter`] would hand a `Secondary` press
//!   to the active tool exactly as it hands it a `Primary` one — so a right-drag
//!   would paint. [`shell::pointer_button`] refuses it rather than leaving the
//!   user a stroke they did not ask for.
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
pub mod tool_input;

pub use action::{Action, Category, ToolKey};
pub use chrome::{Chrome, ChromeOutput, Rebind, ShortcutRow};
pub use dialogs::{CloseChoice, FileDialogs, NativeDialogs, ScriptedDialogs};
pub use dirty::DirtyTiles;
pub use doc::{DocumentError, DocumentId, OpenDocument};
pub use editor::{
    color_hex, ActionError, AutosaveReport, Editor, Effect, NoSuchTab, RecoveryReport,
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
pub use tool_input::{PointerOutcome, Refusal, ToolPointer};

use std::path::PathBuf;

/// Start the application: real dialogs, the user's configuration directory, and
/// whatever files were named on the command line.
pub fn launch(files: Vec<PathBuf>) -> Result<(), ShellError> {
    Shell::new(Editor::native(), files).run()
}
