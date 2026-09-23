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
//!   bar, all fifteen docked panels and the dialogs (Preferences included,
//!   hosted by [`dialog_host`]) are the `ui` crate's, reached from the binary.
//!   What this crate still draws itself is what that crate has no model for —
//!   the document tab strip, the start screen, the canvas extras
//!   ([`canvas_extras`]: rulers, guides, grid, cursors, the brush ring, the
//!   context menu) and the transient status message. [`menu_bridge::pick`] is
//!   the single translation from [`ui::Intent`] to something the shell
//!   performs.
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
//! # How two edits land
//!
//! * **A stroke is previewed, then committed once (W4-B).**
//!   `tools::StrokeTool::commit` still emits the stroke's single `PaintTiles`
//!   command from `on_pointer_up` alone, but each Move sample publishes the
//!   in-flight tiles through `tools::Tool::live_paint` into the document's
//!   preview lens, so the stroke shows while it is dragged; the release
//!   commits exactly those pixels and Escape leaves no trace. See
//!   [`tool_input`].
//! * **A crop is one undoable step (W4-D).** Straighten, W x H x Resolution
//!   and Delete Cropped Pixels included — see `crop_apply::crop`, which
//!   [`tool_input::ToolPointer::commit`] calls ([`tool_input::crop_command`]
//!   is its geometry half alone). A slice set is exported by File > Export >
//!   Slices, one file per region ([`slices_export`]).
//!
//! # Known gaps
//!
//! Stated rather than implied:
//!
//! * **A crop's rotation and scale ride on the layer transforms.** They are
//!   carried by the root layers' transforms rather than baked into their
//!   pixels; Delete Cropped Pixels clears raster layers' own pixels only
//!   (layer masks, text and shape layers keep their content); and the
//!   Resolution field converts to pixels but is not stored on the document.
//! * **Stylus pressure is verified with synthetic events only.** winit's
//!   `Touch` events (a pen's force included) are routed by [`pen_input`]
//!   through [`shell::Shell::set_pen_pressure`] onto the mouse's pointer
//!   route; the shell's tests drive synthetic events, and a physical pen on
//!   each platform is still needed to confirm the OS's event order.
//! * **The right button never reaches a tool.** It opens the canvas context
//!   menu ([`canvas_extras`]); [`shell::pointer_button`] refuses it as a tool
//!   press, because [`ui::canvas::InputRouter`] would hand a `Secondary` press
//!   to the active tool exactly as it hands it a `Primary` one — so a
//!   right-drag would paint.
//! * **The scratch location is typed, not picked.** The preferences window
//!   edits it as a text field; there is no folder picker.
//! * **Tab does not move keyboard focus between widgets.** Tab is a shortcut
//!   here (Hide/Show Panels, and with Ctrl the document tabs), and egui's focus
//!   navigation would both swallow it and then claim every later key press. See
//!   [`shell::withhold_from_egui`]. Text fields are reached with the pointer.

pub mod action;
pub mod canvas_extras;
pub mod chrome;
pub mod clipboard;
mod crop_apply;
pub mod dialog_host;
pub mod dialogs;
pub mod dirty;
pub mod doc;
pub mod edit_session;
pub mod edit_target;
pub mod editor;
pub mod error;
pub mod hit_testing;
pub mod import;
pub mod interaction_geometry;
pub mod jobs;
pub mod keymap;
pub mod layer_ops;
pub mod menu_bridge;
pub mod pen_input;
pub mod placement;
pub mod prefs;
pub mod presenter;
pub mod recent;
pub mod session;
pub mod shell;
pub mod slices_export;
pub mod tool_input;
pub mod version;

pub use action::{Action, Category, ToolKey};
pub use canvas_extras::{CanvasExtras, ExtrasReport};
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
pub use placement::{place_source, place_source_fit, Placement};
pub use prefs::{AppPaths, Preferences, ThemeChoice, WindowGeometry};
pub use presenter::CanvasPresenter;
pub use recent::{RecentFiles, MAX_RECENT_FILES};
pub use session::{SessionMarker, SessionRecord};
pub use shell::Shell;
pub use tool_input::{PointerOutcome, Refusal, SnapPolicy, ToolPointer};
pub use version::{about_line, set_version_stamp, version, Version};

use std::path::PathBuf;

/// Start the application: real dialogs, the user's configuration directory, and
/// whatever files were named on the command line.
pub fn launch(files: Vec<PathBuf>, shot: Option<PathBuf>) -> Result<(), ShellError> {
    if shot.is_some() {
        if let Ok(spec) = std::env::var(SHOT_VIEW_ENV) {
            let _ = SHOT_VIEW.set(parse_view_flags(&spec));
        }
    }
    Shell::with_shot(Editor::native(), files, shot).run()
}

/// W3-A: the environment variable a `--shot` reads for the View toggles to
/// tick before the capture, as a comma-separated list of flag names
/// (`rulers,grid`, case-insensitive, the [`ui::ViewFlag`] variant names). A
/// screenshot fixture can then show an Extra that is off by default without
/// a click. Ignored without `--shot`.
pub const SHOT_VIEW_ENV: &str = "RASTER_SHOT_VIEW";

/// The View toggles a `--shot` run asked for; unset otherwise. Read by
/// [`Chrome::new`].
static SHOT_VIEW: std::sync::OnceLock<Vec<ui::ViewFlag>> = std::sync::OnceLock::new();

/// The View toggles to turn on at start-up (only ever set by a `--shot` run).
pub(crate) fn shot_view_flags() -> &'static [ui::ViewFlag] {
    SHOT_VIEW.get().map(Vec::as_slice).unwrap_or(&[])
}

/// Parse [`SHOT_VIEW_ENV`]'s value; unknown names are skipped.
pub fn parse_view_flags(spec: &str) -> Vec<ui::ViewFlag> {
    spec.split(',')
        .map(|name| name.trim().to_ascii_lowercase())
        .filter_map(|name| {
            ui::ViewFlag::ALL
                .iter()
                .copied()
                .find(|flag| format!("{flag:?}").to_ascii_lowercase() == name)
        })
        .collect()
}
