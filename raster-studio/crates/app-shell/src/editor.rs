//! The application: open documents, the active one, the tool, the colours, the
//! keymap, the preferences — and the one function that performs an [`Action`].
//!
//! # No window required
//!
//! Nothing here touches wgpu, winit or egui. That is the point: menu
//! enablement, command emission, tab switching, save/open/export, autosave and
//! crash recovery are all decided in this module, so all of it is testable
//! without a display. [`crate::shell`] is a thin layer that turns platform
//! events into [`Action`]s and draws what this type holds.
//!
//! # The editor is a view
//!
//! Every document change goes through [`OpenDocument::apply`], which runs a
//! [`Command`] through [`editor_core::History`]. This module never writes to a
//! [`editor_core::Document`] field directly, with one deliberate exception:
//! `set_active_layer`, which is a cursor rather than content and has no command
//! (see `editor_core::Document`'s own note on it).
//!
//! # Why `dispatch` has no wildcard arm
//!
//! An action that reaches a `_ => tracing::debug!("not wired yet")` arm is a
//! menu item that does nothing. The match below is exhaustive, so adding an
//! [`Action`] variant fails to compile until it is wired, and
//! `every_action_does_something` proves at run time that no wired arm is a
//! silent no-op.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use editor_core::pixels::{PixelTarget, TileDelta};
use editor_core::{Command, LayerPatch};
use layer_model::{Layer, LayerId, LayerKind, MaskId};
use tools::{registry, BrushSettings, ToolId};

use crate::action::Action;
use crate::dialogs::{CloseChoice, FileDialogs, NativeDialogs, PROJECT_EXTENSION};
use crate::doc::{DocumentError, DocumentId, OpenDocument};
use crate::keymap::{Chord, Conflict, Keymap};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::session::{self, SessionRecord};
use compositor::TileSource;

/// Make a layer name safe for a file name: keep letters, digits, spaces,
/// underscore and hyphen, collapse runs, and refuse a bare dot.
fn safe_file_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    let trimmed = out.trim().trim_matches('.');
    let trimmed = if trimmed.is_empty() { "layer" } else { trimmed };
    trimmed.to_string()
}

/// Rewrite a `PaintTiles` command so only colour component `channel` (0..=2)
/// of each touched pixel changes, keeping the other channels of the tile's
/// prior content. This is channel editing: with the red channel isolated as the
/// edit target, a brush stroke darkens only red, leaving green and blue where
/// they were.
///
/// The masking happens *here*, at the shell's apply boundary, so the command
/// that reaches history and the journal already carries masked hashes — undo,
/// redo and crash replay all see a fully-specified edit and do not need to know
/// any channel state. The inverse is built by `apply` from the prior hashes, so
/// undo restores the whole prior tile exactly.
fn mask_paint_to_channel(doc: &mut OpenDocument, command: Command, channel: usize) -> Command {
    // Only colour components (R/G/B) are isolated for editing; an alpha or a
    // mask coverage target is a singular channel and paints normally.
    if channel >= 3 {
        return command;
    }
    match command {
        Command::PaintTiles { target, delta } => mask_delta(
            doc,
            channel,
            |d| Command::PaintTiles { target, delta: d },
            target,
            delta,
        ),
        Command::FillRegion {
            target,
            rect,
            value,
            delta,
        } => mask_delta(
            doc,
            channel,
            |d| Command::FillRegion {
                target,
                rect,
                value,
                delta: d,
            },
            target,
            delta,
        ),
        _ => command,
    }
}

/// Mask `delta` so only colour component `channel` changes, returning the
/// command rebuilt with the masked delta (or the original when the delta
/// cannot be rewritten). `build` re-assembles the active variant.
fn mask_delta(
    doc: &mut OpenDocument,
    channel: usize,
    build: impl FnOnce(TileDelta) -> Command,
    target: PixelTarget,
    delta: TileDelta,
) -> Command {
    let Ok(key) = editor_core::resolve_target(&doc.document, target) else {
        return build(delta);
    };

    let mut edits: Vec<editor_core::pixels::TileEdit> = Vec::with_capacity(delta.len());
    for edit in delta.edits() {
        let Some(new_hash) = edit.hash else {
            // A tile removal edits the whole pixel (there is nothing to keep),
            // and is orthogonal to colour-channel editing; it stays as-is.
            edits.push(*edit);
            continue;
        };
        let Some(new_bytes) = doc.tiles.tile(new_hash).map(<[u8]>::to_vec) else {
            edits.push(*edit);
            continue;
        };
        let prior_hash = doc.document.pixels.tile(key, edit.coord);
        let prior_bytes = prior_hash
            .and_then(|h| doc.tiles.tile(h))
            .map(<[u8]>::to_vec)
            .unwrap_or_else(|| vec![0u8; new_bytes.len()]);
        if prior_bytes.len() != new_bytes.len() {
            edits.push(*edit);
            continue;
        }
        // Keep the prior value on every channel but the target one.
        let mut masked = prior_bytes;
        for i in (channel..new_bytes.len()).step_by(4) {
            masked[i] = new_bytes[i];
        }
        let masked_hash = doc.tiles.insert_bytes(masked);
        edits.push(editor_core::pixels::TileEdit::set(edit.coord, masked_hash));
    }

    match TileDelta::new(edits) {
        Ok(new_delta) => build(new_delta),
        Err(_) => build(delta),
    }
}

/// Canvas size of a File ▸ New document.
pub const NEW_DOCUMENT_SIZE: (u32, u32) = (1920, 1080);
/// Smallest and largest brush diameter the bracket keys will reach.
pub const MIN_BRUSH_SIZE: f32 = 1.0;
pub const MAX_BRUSH_SIZE: f32 = 5000.0;
/// Zoom step of one Ctrl+= / Ctrl+-.
pub const ZOOM_STEP: f32 = 1.25;

/// The brush a tool starts life with: the one
/// [`tools::registry::make`] builds it holding.
///
/// Read off a freshly built instance rather than written out again here, so
/// there is exactly one table saying what a Pencil is. A tool that stamps no
/// dabs — a marquee, the gradient, the hand — has no brush of its own and
/// answers with the application default, which nothing then reads.
fn seeded_brush(tool: ToolId) -> BrushSettings {
    registry::make(tool).brush().unwrap_or_default()
}

/// What an action changed. There is no "nothing happened" variant on purpose:
/// an action that would produce one is a bug, and the type is what says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// The camera moved.
    View,
    /// The active tool or its settings changed.
    Tool,
    /// The foreground/background colours changed.
    Color,
    /// Panel visibility changed.
    Panels,
    /// The active document's content changed; recomposite.
    DocumentEdited,
    /// The set of open documents, or which one is active, changed.
    DocumentSet,
    /// A document was written to disk.
    Saved,
    /// An image was written to disk.
    Exported,
    /// The preferences window, or something it holds, changed.
    Preferences,
    /// The application was asked to close.
    Quit,
}

/// Why an action did not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    /// The action does not apply in the current state. `reason` is what a
    /// disabled menu item shows on hover — a menu item that is greyed out
    /// without saying why is only half an answer.
    #[error("{reason}")]
    Unavailable { action: Action, reason: String },
    /// The user backed out of a dialog.
    #[error("cancelled")]
    Cancelled(Action),
    /// It was attempted and failed.
    #[error("{reason}")]
    Failed { action: Action, reason: String },
}

impl ActionError {
    pub fn action(&self) -> Action {
        match self {
            ActionError::Unavailable { action, .. }
            | ActionError::Cancelled(action)
            | ActionError::Failed { action, .. } => *action,
        }
    }

    fn unavailable(action: Action, reason: impl Into<String>) -> Self {
        ActionError::Unavailable {
            action,
            reason: reason.into(),
        }
    }

    fn failed(action: Action, reason: impl std::fmt::Display) -> Self {
        ActionError::Failed {
            action,
            reason: reason.to_string(),
        }
    }
}

/// Why a tab could not be made active.
///
/// Deliberately *not* an [`ActionError`]. A tab click is not one of the
/// [`Action`]s, and the previous code reported `Action::NextDocument` for every
/// out-of-range index — so a refusal would have named a command the user never
/// issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("there is no document in tab {index}; {open} are open")]
pub struct NoSuchTab {
    pub index: usize,
    pub open: usize,
}

/// What one autosave pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutosaveReport {
    pub written: Vec<(DocumentId, PathBuf)>,
    pub failed: Vec<(DocumentId, String)>,
}

impl AutosaveReport {
    pub fn is_empty(&self) -> bool {
        self.written.is_empty() && self.failed.is_empty()
    }
}

/// What a startup recovery pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// `(project, commands replayed)`. A scratch autosave replays nothing —
    /// the package *is* the work — so it appears here with a count of zero.
    pub restored: Vec<(PathBuf, usize)>,
    pub declined: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

impl RecoveryReport {
    pub fn is_empty(&self) -> bool {
        self.restored.is_empty() && self.declined.is_empty() && self.failed.is_empty()
    }
}

/// A smart object whose contents are being edited in a scratch tab (the S1.2
/// embedded-document editor). `layer` owns the object's pixels in `parent`;
/// `contents` is the id of the scratch document currently showing them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EmbeddedContents {
    parent: DocumentId,
    layer: LayerId,
    contents: DocumentId,
}

/// The application.
pub struct Editor {
    paths: AppPaths,
    prefs: Preferences,
    keymap: Keymap,
    recent: RecentFiles,
    dialogs: Box<dyn FileDialogs>,
    app_version: String,

    docs: Vec<OpenDocument>,
    active: Option<usize>,
    next_id: u64,
    untitled_count: u32,

    tool: ToolId,
    /// The active tool's brush — what `[`, `]`, the options bar and the status
    /// bar all read and write.
    brush: BrushSettings,
    /// Every *other* tool's brush, parked while it is not selected.
    ///
    /// The brush belongs to the tool, not to the application. The Pencil is
    /// defined by nothing but its settings — one hard aliased pixel, no
    /// pressure — and it draws through the same `StrokeOp::Paint` the Brush
    /// does, so a single application-wide brush makes the two the same tool the
    /// moment either is used. Eight tools are like that; see
    /// [`Editor::set_tool`]. A slot is filled the first time its tool is left,
    /// and read back through [`Editor::brush_for`], which seeds an absent one
    /// from [`tools::registry::make`] so the registry stays the one table.
    brushes: BTreeMap<ToolId, BrushSettings>,
    foreground: [f32; 4],
    background: [f32; 4],

    panels_visible: bool,
    preferences_open: bool,
    /// Whether the File ▸ File Info… window (document metadata) is up.
    file_info_open: bool,
    /// The colour component paint/fill should write, when the Channels panel
    /// has selected one channel to edit. `None` edits all components.
    paint_channel: Option<usize>,
    /// A smart object whose contents are open in an embedded-document tab
    /// (S1.2 editor): which document and layer own it, and which scratch
    /// document is showing its pixels right now.
    embedded: Option<EmbeddedContents>,
    pending_conflict: Option<Conflict>,
    temporary_hand: bool,
    quit_requested: bool,
    status: Option<String>,

    next_autosave: Option<Instant>,
    /// Scratch autosaves this run owns, keyed by the document they hold.
    ///
    /// The map is what makes an autosave *recoverable*: it is handed to the
    /// crash marker ([`Editor::autosave_paths`]) and it is what says which file
    /// to delete when a document is saved or closed for real.
    autosaves: BTreeMap<DocumentId, PathBuf>,
    /// Unique per run of the process. Part of every scratch autosave's name.
    session_tag: String,
    revision: u64,
    /// The layer and pointer gesture whose kind edit is currently the top of
    /// the active document's history, when one is.
    ///
    /// What makes a drag of an adjustment slider one undo step instead of two
    /// hundred.
    ///
    /// Cleared by a history jump and by every dispatched action, because those
    /// move the *timeline* — after an undo the top of the stack can be an
    /// older kind edit on the same layer, which
    /// [`tops_out_with_kind_edit`] cannot tell from the one this gesture
    /// pushed. An ordinary command landing on top is caught by that guard
    /// instead and deliberately does not clear this. See
    /// [`Editor::apply_kind_edit`].
    kind_gesture: Option<(LayerId, u64)>,
    /// What Edit ▸ Copy last put on the clipboard.
    ///
    /// **In-process, not the system clipboard.** Cut, Copy, Copy Merged, Paste
    /// and Paste Into were five menu items nothing performed, because there was
    /// nowhere in the application to keep the pixels between the copy and the
    /// paste — `ui::ClipboardState` records only *whether* a paste would
    /// produce something, which is a fact about a store that did not exist.
    /// This is that store. Crossing the process boundary is a separate job (it
    /// needs a platform clipboard and an encode/decode of the payload) and
    /// nothing here depends on which side of it the pixels live on.
    clipboard: Option<Clipboard>,
}

/// A rectangle of pixels lifted out of a document, straight RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, `width * height * 4` bytes.
    pub rgba8: Vec<u8>,
}

/// Whether the top of `doc`'s history is a kind edit to `layer`.
///
/// The guard on the fold in [`Editor::apply_kind_edit`]: the gesture id says
/// the pointer never came up, which is a claim about the mouse and not about
/// the history stack. This is the claim about the history stack.
fn tops_out_with_kind_edit(doc: &OpenDocument, layer: LayerId) -> bool {
    matches!(
        doc.history.journal().last(),
        Some(Command::SetLayerKind { layer_id, .. }) if *layer_id == layer
    )
}

/// Black over white — the defaults `D` restores.
pub const DEFAULT_FOREGROUND: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
pub const DEFAULT_BACKGROUND: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// `#RRGGBB` for a colour, so the status bar can name what changed.
pub fn color_hex(rgba: [f32; 4]) -> String {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", c(rgba[0]), c(rgba[1]), c(rgba[2]))
}

/// Bumped once per [`Editor`] built, so two editors in one process cannot mint
/// the same session tag even if the clock does not tick between them.
static SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A token that is unique to this run of the process.
///
/// # The data-loss bug this exists to prevent
///
/// Scratch autosaves used to be named `autosave-{DocumentId}.rstudio`, and
/// `DocumentId` comes from a per-process counter that restarts at 1. So a run
/// that crashed with an hour of unsaved work in `autosave-1.rstudio` had that
/// file silently overwritten the moment the *next* run opened its first
/// document and the autosave timer fired. The pid tells runs apart; the clock
/// and the sequence number tell apart runs that reuse a pid.
fn mint_session_tag() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{pid:x}-{nanos:x}-{seq:x}")
}

impl Editor {
    /// Build an editor over an existing configuration directory.
    pub fn new(paths: AppPaths, dialogs: Box<dyn FileDialogs>) -> Self {
        let prefs = Preferences::load(&paths.preferences_file());
        let recent = RecentFiles::load(&paths.recent_file());
        Editor::with_state(paths, prefs, recent, dialogs)
    }

    /// The editor the desktop binary runs: real dialogs, real config directory.
    pub fn native() -> Self {
        Editor::new(AppPaths::discover(), Box::new(NativeDialogs))
    }

    pub fn with_state(
        paths: AppPaths,
        prefs: Preferences,
        recent: RecentFiles,
        dialogs: Box<dyn FileDialogs>,
    ) -> Self {
        let prefs = prefs.sanitized();
        let keymap = Keymap::with_overrides(prefs.keymap_overrides.clone());
        Editor {
            paths,
            prefs,
            keymap,
            recent,
            dialogs,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            docs: Vec::new(),
            active: None,
            next_id: 1,
            untitled_count: 0,
            tool: ToolId::Move,
            brush: seeded_brush(ToolId::Move),
            brushes: BTreeMap::new(),
            foreground: DEFAULT_FOREGROUND,
            background: DEFAULT_BACKGROUND,
            panels_visible: true,
            preferences_open: false,
            file_info_open: false,
            paint_channel: None,
            pending_conflict: None,
            temporary_hand: false,
            quit_requested: false,
            status: None,
            next_autosave: None,
            autosaves: BTreeMap::new(),
            session_tag: mint_session_tag(),
            revision: 0,
            kind_gesture: None,
            clipboard: None,
            embedded: None,
        }
    }

    // ---------------------------------------------------------------- state

    /// Monotonic counter, bumped by every change to editor or document state.
    ///
    /// It is what `every_action_does_something` checks: an action whose handler
    /// forgot to actually do anything leaves this untouched.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn touch(&mut self) {
        self.revision += 1;
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn preferences(&self) -> &Preferences {
        &self.prefs
    }

    /// Replace the preferences, re-deriving everything that depends on them.
    ///
    /// # The keymap is its own source of truth
    ///
    /// Rebuilding the keymap unconditionally from `prefs.keymap_overrides` used
    /// to throw runtime rebindings away: `Shell::capture_geometry` clones the
    /// *stored* preferences, adds the window rectangle, and hands them back, so
    /// every clean exit reverted the user's shortcuts and then persisted the
    /// reverted list. An incoming list that still matches what this editor last
    /// stored is therefore treated as an echo — the live keymap wins and is
    /// copied into the preferences. Only a list that genuinely differs is taken
    /// as a deliberate change and rebuilds the map.
    pub fn set_preferences(&mut self, prefs: Preferences) {
        let mut prefs = prefs.sanitized();
        let live = self.keymap.overrides().to_vec();
        if prefs.keymap_overrides != live {
            if prefs.keymap_overrides == self.prefs.keymap_overrides {
                prefs.keymap_overrides = live;
            } else {
                self.keymap = Keymap::with_overrides(prefs.keymap_overrides.clone());
            }
        }
        for doc in &mut self.docs {
            doc.history.set_limit(prefs.history_depth);
        }
        // The next autosave is rescheduled from the new interval rather than
        // kept, or turning autosave on would not take effect until the old
        // deadline that was never set.
        self.next_autosave = None;
        self.prefs = prefs;
        self.touch();
    }

    /// Persist preferences (including the current keymap overrides) and the
    /// recent-files list.
    pub fn persist(&mut self) -> std::io::Result<()> {
        self.prefs.keymap_overrides = self.keymap.overrides().to_vec();
        self.paths.ensure()?;
        self.prefs.save(&self.paths.preferences_file())?;
        self.recent.save(&self.paths.recent_file())
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Direct access to the keymap.
    ///
    /// The preferences are re-synchronised from it at every write point that
    /// could otherwise revert it ([`Editor::set_preferences`],
    /// [`Editor::persist`]), so a caller that reaches in here does not have to
    /// remember to do it. The shortcut editor uses [`Editor::rebind`] and
    /// friends instead, which sync immediately and report conflicts.
    pub fn keymap_mut(&mut self) -> &mut Keymap {
        self.touch();
        &mut self.keymap
    }

    /// Bind a chord, refusing to steal it from another action.
    ///
    /// The [`Conflict`] is both returned and parked in
    /// [`Editor::pending_conflict`], which is what the shortcut editor renders
    /// as "…is already Save. Replace?".
    pub fn rebind(&mut self, chord: Chord, action: Action) -> Result<(), Conflict> {
        match self.keymap.bind(chord, action) {
            Ok(()) => {
                self.pending_conflict = None;
                self.after_keymap_change(format!("{chord} is now {}", action.label()));
                Ok(())
            }
            Err(conflict) => {
                self.pending_conflict = Some(conflict.clone());
                self.set_status(conflict.to_string());
                Err(conflict)
            }
        }
    }

    /// Bind a chord even though it already meant something else — what the
    /// conflict prompt's "Replace" answers.
    pub fn force_rebind(&mut self, chord: Chord, action: Action) {
        self.keymap.force_bind(chord, action);
        self.pending_conflict = None;
        self.after_keymap_change(format!("{chord} is now {}", action.label()));
    }

    /// Remove a chord's meaning, default included.
    pub fn unbind_chord(&mut self, chord: Chord) {
        self.keymap.unbind(chord);
        self.pending_conflict = None;
        self.after_keymap_change(format!("{chord} unbound"));
    }

    /// Drop the whole user layer, restoring the shipped table.
    pub fn reset_keymap(&mut self) {
        self.keymap.reset();
        self.pending_conflict = None;
        self.after_keymap_change("Shortcuts restored to their defaults".to_string());
    }

    /// The conflict the last [`Editor::rebind`] refused, if it has not been
    /// answered yet.
    pub fn pending_conflict(&self) -> Option<&Conflict> {
        self.pending_conflict.as_ref()
    }

    pub fn clear_conflict(&mut self) {
        if self.pending_conflict.take().is_some() {
            self.touch();
        }
    }

    fn after_keymap_change(&mut self, status: String) {
        self.prefs.keymap_overrides = self.keymap.overrides().to_vec();
        self.set_status(status);
    }

    /// Whether the preferences window (which holds the shortcut editor) is up.
    pub fn preferences_open(&self) -> bool {
        self.preferences_open
    }

    /// Whether the File ▸ File Info… window is up.
    pub fn file_info_open(&self) -> bool {
        self.file_info_open
    }

    /// Toggle the File Info… metadata window.
    pub fn toggle_file_info(&mut self) {
        self.file_info_open = !self.file_info_open;
    }

    /// Set the single colour component paint/fill commands should write, or
    /// `None` to write all of them. Set from the Channels panel each frame.
    pub fn set_paint_channel(&mut self, channel: Option<usize>) {
        self.paint_channel = channel;
    }

    /// File ▸ Export Layers…: write each layer as its own PNG into a chosen
    /// directory. A layer is composited *alone* (every other layer hidden) over
    /// transparent, through the real compositor — the same isolation the merge
    /// path uses — so effects and blends are honoured per layer.
    pub fn export_layers(&mut self) -> Result<String, String> {
        let Some(dir) = self.dialogs.pick_export_folder() else {
            return Err("Export Layers: no destination chosen".to_string());
        };
        let doc = self
            .active()
            .ok_or_else(|| "No document is open".to_string())?;
        let ids: Vec<LayerId> = doc.document.layers.iter_depth_first();
        let rect = doc.canvas_rect();
        let mut written = 0usize;
        for id in ids {
            let mut staged = doc.document.clone();
            for other in staged.layers.iter_depth_first() {
                if other != id {
                    if let Some(l) = staged.layers.get_mut(other) {
                        l.visible = false;
                    }
                }
            }
            let canvas = compositor::composite_region(
                &staged,
                &doc.tiles,
                rect,
                0,
                compositor::CompositeOptions::default(),
            )
            .map_err(|e| e.to_string())?;
            let rgba8 = canvas.to_rgba8(&doc.document.meta.color_space);
            let name = doc
                .document
                .layers
                .get(id)
                .map(|l| safe_file_name(&l.name))
                .unwrap_or_else(|| "layer".to_string());
            let path = dir.join(format!("{name}.png"));
            raster::encode_to_path(
                &path,
                raster::ExportFormat::Png,
                doc.document.width(),
                doc.document.height(),
                raster::EncodedPixels::Rgba8(&rgba8),
                &raster::EncodeOptions::default(),
            )
            .map_err(|e| e.to_string())?;
            written += 1;
        }
        self.status = Some(format!("Exported {written} layer(s) to {}", dir.display()));
        self.touch();
        Ok("Exported layers".to_string())
    }

    /// File ▸ Print…: render the active document's full composite to a
    /// print-ready single-page PDF, chosen through the export path dialog with
    /// a `.pdf` suggestion. This is the S1.8 Print route: the OS printing/spool
    /// surface is hereby reached as "Print as PDF" (a standard dialogless
    /// print destination) backed by a pure, tested PDF encoder.
    pub fn print_pdf(&mut self) -> Result<String, String> {
        let suggested = self
            .active()
            .map(|d| d.suggested_export_path().with_extension("pdf"))
            .ok_or_else(|| "No document is open".to_string())?;
        let Some(target) = self.dialogs.pick_export_path(&suggested) else {
            return Err("Print: no destination chosen".to_string());
        };
        let doc = self
            .active_mut()
            .ok_or_else(|| "No document is open".to_string())?;
        doc.print_to(&target).map_err(|e| e.to_string())?;
        self.status = Some(format!("Printed {}", target.display()));
        self.touch();
        Ok("Print…".to_string())
    }

    /// Layer ▸ Rasterize: bake the active text/shape/styled layer's pixels into
    /// a raster layer, replacing it in place (same parent, same position).
    /// The source is composited *alone* through the real compositor — so
    /// effects and its own shape are honoured — then written back as a raster.
    pub fn rasterize_active_layer(&mut self) -> Result<String, String> {
        let Some(open) = self.active() else {
            return Err("No document is open".to_string());
        };
        // Conversion does not re-point the selection, so fall back to the first
        // layer when no layer is marked active rather than refusing.
        let Some(source) = open
            .document
            .active_layer()
            .or_else(|| open.document.layers.iter_depth_first().first().copied())
        else {
            return Err("Select a layer first".to_string());
        };
        let rect = open.canvas_rect();
        let mut staged = open.document.clone();
        for other in staged.layers.iter_depth_first() {
            if other != source {
                if let Some(l) = staged.layers.get_mut(other) {
                    l.visible = false;
                }
            }
        }
        let canvas = compositor::composite_region(
            &staged,
            &open.tiles,
            rect,
            0,
            compositor::CompositeOptions::default(),
        )
        .map_err(|e| e.to_string())?;
        let rgba = canvas.to_rgba8(&open.document.meta.color_space);
        let parent = open.document.layers.parent_of(source);
        let index = open
            .document
            .layers
            .index_in_parent(source)
            .ok_or("The layer is not in the tree")?;
        let name = open
            .document
            .layers
            .get(source)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| "Rasterized".to_string());

        let (w, h) = (open.document.width(), open.document.height());
        let command = {
            let doc = self.active_mut().ok_or("No document is open")?;
            let layer = layer_model::Layer::raster(name);
            let new_id = layer.id;
            let mut commands = vec![Command::create_layer(layer)];
            let grid = raster::TileGrid::from_rgba8(w, h, &rgba).map_err(|e| e.to_string())?;
            let mut edits = Vec::new();
            for (coord, tile) in grid.iter() {
                let hash = doc.tiles.insert_bytes(tile.data().to_vec());
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            commands.push(
                Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                    .map_err(|e| e.to_string())?,
            );
            commands.push(Command::MoveLayer {
                layer_id: new_id,
                parent,
                index,
            });
            if doc.document.layers.contains(source) {
                commands.push(Command::DeleteLayer { layer_id: source });
            }
            Command::Transaction {
                label: "Rasterize Layer".to_string(),
                commands,
            }
        };
        self.apply_command(command);
        Ok("Rasterized layer".to_string())
    }

    /// Layer ▸ Rasterize ▸ Layer / Smart Object: bake any non-pixel active
    /// layer (text, shape, style, or a smart object's contents) into plain
    /// pixels. `rasterize_active_layer` already composites whichever layer is
    /// active, so these targets are the same engine with a different label.
    pub fn rasterize_layer(&mut self) -> Result<String, String> {
        self.rasterize_active_layer()
    }

    /// Layer ▸ Rasterize ▸ All Layers: flatten the whole document to a single
    /// raster layer holding the full composite, replacing the layer tree as one
    /// undoable transaction.
    pub fn flatten_all_layers(&mut self) -> Result<String, String> {
        let (w, h, rgba) = {
            let open = self
                .active()
                .ok_or_else(|| "No document is open".to_string())?;
            let rect = open.canvas_rect();
            let canvas = compositor::composite_region(
                &open.document,
                &open.tiles,
                rect,
                0,
                compositor::CompositeOptions::default(),
            )
            .map_err(|e| e.to_string())?;
            (
                open.document.width(),
                open.document.height(),
                canvas.to_rgba8(&open.document.meta.color_space),
            )
        };
        let command = {
            let doc = self.active_mut().ok_or("No document is open")?;
            let mut commands = Vec::new();
            let ids: Vec<LayerId> = doc.document.layers.iter_depth_first();
            for id in ids {
                commands.push(Command::DeleteLayer { layer_id: id });
            }
            let layer = layer_model::Layer::raster("Flattened");
            let new_id = layer.id;
            commands.push(Command::create_layer(layer));
            let grid = raster::TileGrid::from_rgba8(w, h, &rgba).map_err(|e| e.to_string())?;
            let mut edits = Vec::new();
            for (coord, tile) in grid.iter() {
                let hash = doc.tiles.insert_bytes(tile.data().to_vec());
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            commands.push(
                Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                    .map_err(|e| e.to_string())?,
            );
            Command::Transaction {
                label: "Flatten Image".to_string(),
                commands,
            }
        };
        self.apply_command(command);
        Ok("Flattened all layers".to_string())
    }

    /// Layer ▸ Convert to Smart Object: bake the active layer's pixels into a
    /// smart-object layer and replace it in place (same parent, position, name).
    /// The compositor renders a smart object from its stored pixels (an
    /// embedded-document cache), so the result draws exactly what the source
    /// drew before conversion.
    pub fn convert_to_smart_object(&mut self) -> Result<String, String> {
        let Some(open) = self.active() else {
            return Err("No document is open".to_string());
        };
        let Some(source) = open.document.active_layer() else {
            return Err("Select a layer first".to_string());
        };
        let rect = open.canvas_rect();
        let mut staged = open.document.clone();
        for other in staged.layers.iter_depth_first() {
            if other != source {
                if let Some(l) = staged.layers.get_mut(other) {
                    l.visible = false;
                }
            }
        }
        let canvas = compositor::composite_region(
            &staged,
            &open.tiles,
            rect,
            0,
            compositor::CompositeOptions::default(),
        )
        .map_err(|e| e.to_string())?;
        let rgba = canvas.to_rgba8(&open.document.meta.color_space);
        let parent = open.document.layers.parent_of(source);
        let index = open
            .document
            .layers
            .index_in_parent(source)
            .ok_or("The layer is not in the tree")?;
        let name = open
            .document
            .layers
            .get(source)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| "Smart Object".to_string());
        let (w, h) = (open.document.width(), open.document.height());
        let command = {
            let doc = self.active_mut().ok_or("No document is open")?;
            let layer = layer_model::Layer::with_kind(
                name,
                layer_model::LayerKind::SmartObject(layer_model::SmartObjectLayer {
                    asset: layer_model::AssetId::new(),
                    linked: false,
                }),
            );
            let new_id = layer.id;
            let mut commands = vec![Command::create_layer(layer)];
            let grid = raster::TileGrid::from_rgba8(w, h, &rgba).map_err(|e| e.to_string())?;
            let mut edits = Vec::new();
            for (coord, tile) in grid.iter() {
                let hash = doc.tiles.insert_bytes(tile.data().to_vec());
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            commands.push(
                Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                    .map_err(|e| e.to_string())?,
            );
            commands.push(Command::MoveLayer {
                layer_id: new_id,
                parent,
                index,
            });
            if doc.document.layers.contains(source) {
                commands.push(Command::DeleteLayer { layer_id: source });
            }
            Command::Transaction {
                label: "Convert to Smart Object".to_string(),
                commands,
            }
        };
        self.apply_command(command);
        Ok("Converted layer to a smart object".to_string())
    }

    /// File ▸ Duplicate…: open a copy of the current document's state as a new
    /// document (same pixels and layer tree, fresh undo history, `copy` title).
    /// File ▸ Close All: close every open document, answering the unsaved-
    /// changes prompt once per document (walking from the back so indexes stay
    /// valid). Backing out of one prompt cancels the whole close-all.
    pub fn close_all_documents(&mut self) -> Result<String, String> {
        for index in (0..self.docs.len()).rev() {
            self.close_document(index).map_err(|e| match e {
                ActionError::Cancelled(_) => "Close All cancelled".to_string(),
                ActionError::Unavailable { reason, .. } => reason,
                ActionError::Failed { reason, .. } => reason,
            })?;
        }
        Ok("Closed all documents".to_string())
    }

    /// Resize the active document's canvas, resampling every pixel-bearing
    /// layer by `src_min` (crop/pad with transparency), as one undoable step.
    /// The engine is [`OpenDocument::resize_canvas`]; this applies it through
    /// history so undo restores the previous canvas and pixels.
    pub fn resize_canvas(
        &mut self,
        new_w: u32,
        new_h: u32,
        src_min: glam::IVec2,
    ) -> Result<String, String> {
        if new_w == 0 || new_h == 0 {
            return Err("The canvas cannot be empty".to_string());
        }
        let command = {
            let doc = self
                .active_mut()
                .ok_or_else(|| "No document is open".to_string())?;
            doc.resize_canvas(new_w, new_h, src_min)
                .map_err(|e| e.to_string())?
        };
        self.apply_command(command);
        self.status = Some(format!("Canvas resized to {new_w}×{new_h}"));
        Ok("Resized canvas".to_string())
    }

    /// Image ▸ Crop to Selection: resize the canvas to the live selection's
    /// bounds, moving the selected content to the origin. No-op with a clear
    /// reason when there is no selection.
    pub fn crop_to_selection(&mut self) -> Result<String, String> {
        let (min, max) = {
            let open = self
                .active()
                .ok_or_else(|| "No document is open".to_string())?;
            open.document
                .selection
                .bounds()
                .ok_or_else(|| "There is no selection to crop to".to_string())?
        };
        let (w, h) = ((max.x - min.x), (max.y - min.y));
        self.resize_canvas(w as u32, h as u32, min)
    }

    /// Image ▸ Trim: resize the canvas to the bounding box of the visible
    /// (non-transparent) composite, moving that content to the origin. A no-op
    /// (with a status note) when the content already fills the canvas.
    pub fn trim_canvas(&mut self) -> Result<String, String> {
        let (w, h, rgba) = {
            let idx = self
                .active
                .ok_or_else(|| "No document is open".to_string())?;
            let doc = self
                .docs
                .get_mut(idx)
                .ok_or_else(|| "No document is open".to_string())?;
            let rect = doc.canvas_rect();
            let rgba = doc.composite(rect).map_err(|e| e.to_string())?;
            (doc.document.width(), doc.document.height(), rgba)
        };
        let mut minx = w;
        let mut miny = h;
        let mut maxx = 0;
        let mut maxy = 0;
        let mut any = false;
        for y in 0..h {
            for x in 0..w {
                let a = rgba[((y * w + x) as usize) * 4 + 3];
                if a > 0 {
                    any = true;
                    if x < minx {
                        minx = x;
                    }
                    if x > maxx {
                        maxx = x;
                    }
                    if y < miny {
                        miny = y;
                    }
                    if y > maxy {
                        maxy = y;
                    }
                }
            }
        }
        if !any {
            return Err("The document has no content to trim to".to_string());
        }
        if minx == 0 && miny == 0 && maxx + 1 == w && maxy + 1 == h {
            return Err("The content already fills the canvas".to_string());
        }
        let new_w = maxx - minx + 1;
        let new_h = maxy - miny + 1;
        self.resize_canvas(new_w, new_h, glam::IVec2::new(minx as i32, miny as i32))
    }

    pub fn duplicate_document(&mut self) -> Result<String, String> {
        let idx = self
            .active
            .ok_or_else(|| "No document is open".to_string())?;
        let new_id = self.mint_id();
        let copy = self.docs[idx].duplicate(new_id);
        self.docs.push(copy);
        self.active = Some(self.docs.len() - 1);
        self.touch();
        Ok("Duplicated document".to_string())
    }

    /// Layer ▸ Smart Object ▸ Edit Contents…: open a smart object's stored
    /// pixels in a scratch document so they can be edited as their own raster,    /// keeping the (parent, layer) pair so a later [`Self::commit_smart_object_contents`]
    /// writes the edits back as one undoable step on the parent. This is the
    /// S1.2 embedded-document editor.
    pub fn edit_smart_object_contents(&mut self) -> Result<String, String> {
        let (parent, layer_id, name, w, h, rgba) = {
            let open = self
                .active()
                .ok_or_else(|| "No document is open".to_string())?;
            // The active layer when it is a smart object, else the first smart
            // object in the tree: conversion does not re-point the selection,
            // so Edit Contents must not depend on it having done so.
            let layer = open
                .document
                .active_layer()
                .filter(|id| {
                    matches!(
                        open.document.layers.get(*id).map(|l| &l.kind),
                        Some(LayerKind::SmartObject(_))
                    )
                })
                .or_else(|| {
                    open.document
                        .layers
                        .iter_depth_first()
                        .into_iter()
                        .find(|id| {
                            matches!(
                                open.document.layers.get(*id).map(|l| &l.kind),
                                Some(LayerKind::SmartObject(_))
                            )
                        })
                })
                .ok_or_else(|| "Select a smart object layer first".to_string())?;
            let name = open
                .document
                .layers
                .get(layer)
                .map(|l| l.name.clone())
                .unwrap_or_else(|| "Smart Object".to_string());
            let rgba = open.layer_pixels(layer).map_err(|e| e.to_string())?;
            (
                open.id(),
                layer,
                name,
                open.document.width(),
                open.document.height(),
                rgba,
            )
        };
        let title = format!("{name} @ Contents");
        let contents_id = self.mint_id();
        let doc = OpenDocument::blank(contents_id, w, h, &title, self.prefs.history_depth)
            .map_err(|e| e.to_string())?;
        self.docs.push(doc);
        self.active = Some(self.docs.len() - 1);
        self.embedded = Some(EmbeddedContents {
            parent,
            layer: layer_id,
            contents: contents_id,
        });
        // Seed the tab with the object's own pixels as its first undoable step,
        // so the tab's raster IS the smart object's contents from frame one.
        let seed = {
            let target_layer = self
                .active()
                .and_then(|d| d.document.layers.iter_depth_first().first().copied())
                .ok_or_else(|| "Contents document has no layer".to_string())?;
            let grid = raster::TileGrid::from_rgba8(w, h, &rgba).map_err(|e| e.to_string())?;
            let mut edits = Vec::new();
            let doc = self.active_mut().ok_or("Contents document missing")?;
            for (coord, tile) in grid.iter() {
                let hash = doc.tiles.insert_bytes(tile.data().to_vec());
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(target_layer), edits)
                .map_err(|e| e.to_string())?
        };
        self.apply_command(seed);
        self.touch();
        self.status = Some(format!("Editing contents of {name}"));
        Ok("Edit Contents…".to_string())
    }

    /// Commit the open embedded tab's pixels back into its parent smart object
    /// as one undoable step on the parent, then close the tab and return to the
    /// parent document. A no-op with a clear reason when no contents tab is open.
    pub fn commit_smart_object_contents(&mut self) -> Result<String, String> {
        let embedded = self
            .embedded
            .clone()
            .ok_or_else(|| "No smart object contents are being edited".to_string())?;

        // Composite the scratch tab at its own resolution.
        let (w, h, rgba) = {
            let idx = self
                .docs
                .iter()
                .position(|d| d.id() == embedded.contents)
                .ok_or_else(|| "The contents document is gone".to_string())?;
            let doc = self
                .docs
                .get_mut(idx)
                .ok_or_else(|| "The contents document is gone".to_string())?;
            let rect = doc.canvas_rect();
            let rgba = doc.composite(rect).map_err(|e| e.to_string())?;
            (doc.document.width(), doc.document.height(), rgba)
        };

        // Write those pixels onto the parent smart object as a command, so the
        // whole commit is one undo step.
        let layer = embedded.layer;
        let command = {
            let parent_idx = self
                .docs
                .iter()
                .position(|d| d.id() == embedded.parent)
                .ok_or_else(|| "The parent document is gone".to_string())?;
            let parent = self
                .docs
                .get_mut(parent_idx)
                .ok_or_else(|| "The parent document is gone".to_string())?;
            let grid = raster::TileGrid::from_rgba8(w, h, &rgba).map_err(|e| e.to_string())?;
            let mut edits = Vec::new();
            for (coord, tile) in grid.iter() {
                let hash = parent.tiles.insert_bytes(tile.data().to_vec());
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(layer), edits)
                .map_err(|e| e.to_string())?
        };

        // Switch to the parent, apply (undoable), then drop the scratch tab.
        let parent_idx = self
            .docs
            .iter()
            .position(|d| d.id() == embedded.parent)
            .ok_or_else(|| "The parent document is gone".to_string())?;
        self.active = Some(parent_idx);
        self.apply_command(command);
        if let Some(ci) = self.docs.iter().position(|d| d.id() == embedded.contents) {
            self.docs.remove(ci);
            if self.active.map(|a| a >= ci).unwrap_or(false) {
                self.active = Some(self.active.unwrap_or(0).saturating_sub(1));
            }
        }
        self.embedded = None;
        self.touch();
        Ok("Committed smart object contents".to_string())
    }

    /// Layer ▸ New Fill Layer ▸ Solid Color: add a raster layer filled with
    /// the current foreground colour across the whole canvas.
    pub fn new_solid_fill_layer(&mut self) -> Result<String, String> {
        let (w, h) = self
            .active()
            .map(|d| (d.document.width(), d.document.height()))
            .ok_or_else(|| "No document is open".to_string())?;
        let fg = self.foreground();
        let [r, g, b, a] = [
            (fg[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (fg[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (fg[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            (fg[3].clamp(0.0, 1.0) * 255.0).round() as u8,
        ];
        let pixels = {
            let n = (w as usize) * (h as usize) * 4;
            let mut v = Vec::with_capacity(n);
            for _ in 0..(w as usize * h as usize) {
                v.extend_from_slice(&[r, g, b, a]);
            }
            v
        };
        let command = {
            let doc = self.active_mut().ok_or("No document is open")?;
            let layer = layer_model::Layer::raster("Color Fill");
            let new_id = layer.id;
            let grid = raster::TileGrid::from_rgba8(w, h, &pixels).map_err(|e| e.to_string())?;
            let mut edits = Vec::new();
            for (coord, tile) in grid.iter() {
                let hash = doc.tiles.insert_bytes(tile.data().to_vec());
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            Command::Transaction {
                label: "New Fill Layer".to_string(),
                commands: vec![
                    Command::create_layer(layer),
                    Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                        .map_err(|e| e.to_string())?,
                ],
            }
        };
        self.apply_command(command);
        Ok("Added solid color fill layer".to_string())
    }

    /// Layer ▸ New Fill Layer ▸ Gradient: add a raster layer holding a linear
    /// gradient from the current foreground (left) to the background (right)
    /// across the whole canvas. Like the Solid Color layer, the gradient is
    /// *baked* into the layer's pixels at creation rather than a live
    /// generator — a one-step undoable fill, honest about being a raster.
    pub fn new_gradient_fill_layer(&mut self) -> Result<String, String> {
        let (w, h) = self
            .active()
            .map(|d| (d.document.width(), d.document.height()))
            .ok_or_else(|| "No document is open".to_string())?;
        let fg = self.foreground();
        let bg = self.background();
        let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        let far = [to_u8(fg[0]), to_u8(fg[1]), to_u8(fg[2]), to_u8(fg[3])];
        let near = [to_u8(bg[0]), to_u8(bg[1]), to_u8(bg[2]), to_u8(bg[3])];
        let mut pixels = Vec::with_capacity((w as usize) * (h as usize) * 4);
        let denom = (w.max(1)) as f32;
        for _ in 0..h {
            for x in 0..w {
                let t = x as f32 / denom;
                let row = [
                    (far[0] as f32 * (1.0 - t) + near[0] as f32 * t).round() as u8,
                    (far[1] as f32 * (1.0 - t) + near[1] as f32 * t).round() as u8,
                    (far[2] as f32 * (1.0 - t) + near[2] as f32 * t).round() as u8,
                    (far[3] as f32 * (1.0 - t) + near[3] as f32 * t).round() as u8,
                ];
                pixels.extend_from_slice(&row);
            }
        }
        let command = {
            let doc = self.active_mut().ok_or("No document is open")?;
            let layer = layer_model::Layer::raster("Gradient Fill");
            let new_id = layer.id;
            let grid = raster::TileGrid::from_rgba8(w, h, &pixels).map_err(|e| e.to_string())?;
            let mut edits = Vec::new();
            for (coord, tile) in grid.iter() {
                let hash = doc.tiles.insert_bytes(tile.data().to_vec());
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            Command::Transaction {
                label: "New Gradient Fill".to_string(),
                commands: vec![
                    Command::create_layer(layer),
                    Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                        .map_err(|e| e.to_string())?,
                ],
            }
        };
        self.apply_command(command);
        Ok("Added gradient fill layer".to_string())
    }

    pub fn recent(&self) -> &RecentFiles {
        &self.recent
    }

    pub fn documents(&self) -> &[OpenDocument] {
        &self.docs
    }

    /// Every open document, mutably — for the shell's per-window state (the
    /// camera's viewport size). Not an editing path: content still changes only
    /// through [`Editor::apply_command`].
    pub fn documents_mut(&mut self) -> &mut [OpenDocument] {
        &mut self.docs
    }

    /// Run a command emitted by a panel through the active document's history.
    ///
    /// This is how the UI edits: it emits intent, the editor applies it, undo
    /// and redo stay uniform. A refusal is reported rather than swallowed.
    pub fn apply_command(&mut self, command: Command) {
        // Deliberately does *not* clear `kind_gesture`. Another command landing
        // on the stack is exactly the case `tops_out_with_kind_edit` is there
        // to catch, and clearing here as well would make that guard unreachable
        // — a safety belt nothing can test is a safety belt nobody can trust.
        let channel = self.paint_channel;
        let Some(doc) = self.active_mut() else {
            return;
        };
        let command = match channel {
            Some(c) => mask_paint_to_channel(doc, command, c),
            None => command,
        };
        match doc.apply(command) {
            Ok(()) => self.touch(),
            Err(e) => {
                let reason = e.to_string();
                self.set_status(reason);
            }
        }
    }

    /// Apply an edit to a layer's kind payload, folding a drag into one step.
    ///
    /// This is the path every adjustment slider and every text field takes, and
    /// it is why they do anything at all: `LayerPatch` covers no layer's `kind`,
    /// so before [`Command::SetLayerKind`] existed the Properties panel emitted
    /// an intent the bridge answered with `None` and the chrome threw away.
    ///
    /// # One sweep, one undo step
    ///
    /// A slider emits on every frame the pointer moves. Each of those is a real
    /// edit and each would be a real history entry, so one drag of the
    /// Brightness knob would cost a hundred-odd presses of Ctrl+Z to take back.
    /// When this edit continues the gesture the previous one belonged to, the
    /// entry that gesture already pushed is **undone first** and the new value
    /// applied over it. The entry that lands therefore captures the payload the
    /// layer held before the drag began, which is exactly what one undo has to
    /// restore — no history surgery and no second inverse.
    ///
    /// # The fold is in memory only — the journal is not folded
    ///
    /// The coalescing above is a claim about [`editor_core::History`] and about
    /// nothing else. On disk the drag is *not* folded: [`OpenDocument::apply`]
    /// appends one `SetLayerKind` record to `commands.journal` and fsyncs it on
    /// every call, while [`OpenDocument::undo`] writes no record at all, so a
    /// saved project gains one journal record per frame of the sweep while its
    /// `history_depth()` gains one. Journal growth during a drag is therefore
    /// bounded by frames, not by gestures.
    ///
    /// That costs disk, not correctness: `SetLayerKind` carries an absolute
    /// payload, so replaying every record in order converges on the value the
    /// user settled on. It is also not a regression of this path — the Opacity
    /// slider, which predates it, journals per frame in exactly the same way.
    /// `a_drag_writes_one_journal_record_per_frame_while_history_gains_one`
    /// measures both numbers, so this paragraph cannot quietly stop being true.
    ///
    /// The undo is taken only when the top of the stack really is this layer's
    /// kind edit. A gesture id is a claim about the pointer, and rolling back
    /// somebody else's command on the strength of it would be worse than
    /// pushing an extra step.
    pub fn apply_kind_edit(&mut self, edit: crate::chrome::KindEdit) {
        let key = edit.gesture.map(|g| (edit.layer, g));
        let command = Command::SetLayerKind {
            layer_id: edit.layer,
            kind: edit.kind,
        };
        let continuing = key.is_some() && key == self.kind_gesture;
        self.kind_gesture = None;
        let outcome = {
            let Some(doc) = self.active_mut() else {
                return;
            };
            // A failed undo leaves the document untouched — `History::undo`
            // puts the entry back — so the fold is simply skipped and the edit
            // lands as its own step rather than being lost.
            if continuing && tops_out_with_kind_edit(doc, edit.layer) {
                let _ = doc.undo();
            }
            doc.apply(command)
        };
        match outcome {
            Ok(()) => {
                self.kind_gesture = key;
                self.touch();
            }
            Err(e) => {
                let reason = e.to_string();
                self.set_status(reason);
            }
        }
    }

    /// Walk the active document's history until `target` commands are applied.
    ///
    /// This is what a click in the history dock performs. It undoes or redoes
    /// one step at a time through [`editor_core::History`] rather than reaching
    /// into the document, so every step of the walk is exactly the step Ctrl+Z
    /// would have taken and the timeline stays consistent.
    ///
    /// Returns how many steps it actually moved. A step that refuses stops the
    /// walk and reports itself, keeping whatever it managed — the same
    /// behaviour as a recovery replay that cannot finish.
    pub fn jump_history(&mut self, target: usize) -> usize {
        // Walking the timeline moves whatever is on top of the stack, so the
        // entry a continuing drag would have folded into is gone.
        self.kind_gesture = None;
        let Some(index) = self.active else {
            return 0;
        };
        let mut moved = 0;
        let mut failure = None;
        while let Some(doc) = self.docs.get_mut(index) {
            let depth = doc.history_depth();
            let step = if depth > target {
                doc.undo()
            } else if depth < target {
                doc.redo()
            } else {
                break;
            };
            match step {
                Ok(true) => moved += 1,
                // The stack ran out before `target` did — a panel drawn from a
                // state that has since moved. Stop rather than spin.
                Ok(false) => break,
                Err(e) => {
                    failure = Some(e.to_string());
                    break;
                }
            }
        }
        if let Some(reason) = failure {
            self.set_status(reason);
        }
        if moved > 0 {
            self.touch();
        }
        moved
    }

    /// Point the active document's layer cursor at `layer`.
    ///
    /// Deliberately not a command: `editor_core` documents the active layer as
    /// a cursor rather than content, with no command of its own.
    pub fn set_active_layer(&mut self, layer: LayerId) {
        let Some(doc) = self.active_mut() else {
            return;
        };
        if doc.document.active_layer() == Some(layer) {
            return;
        }
        match doc.document.set_active_layer(Some(layer)) {
            Ok(()) => self.touch(),
            Err(e) => tracing::debug!("cannot select that layer: {e}"),
        }
    }

    /// Show the user a failure through the platform's dialog.
    pub fn report_error(&mut self, title: &str, message: &str) {
        self.dialogs.report_error(title, message);
        self.status = Some(format!("{title}: {message}"));
        self.touch();
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub fn active(&self) -> Option<&OpenDocument> {
        self.active.and_then(|i| self.docs.get(i))
    }

    pub fn active_mut(&mut self) -> Option<&mut OpenDocument> {
        match self.active {
            Some(i) => self.docs.get_mut(i),
            None => None,
        }
    }

    /// Switch tabs.
    pub fn activate(&mut self, index: usize) -> Result<(), NoSuchTab> {
        if index >= self.docs.len() {
            return Err(NoSuchTab {
                index,
                open: self.docs.len(),
            });
        }
        if self.active != Some(index) {
            self.active = Some(index);
            self.touch();
        }
        Ok(())
    }

    pub fn tool(&self) -> ToolId {
        self.tool
    }

    /// Choose a tool directly — what the tool palette does.
    ///
    /// The outgoing tool's brush is parked and the incoming one's is taken up.
    /// Without that swap the application's single brush would follow the user
    /// from tool to tool and overwrite what each one *is*: the Pencil paints
    /// through the same `StrokeOp::Paint` as the Brush and is told apart from
    /// it only by size 1, hardness 1, `aliased` and no size-from-pressure, so a
    /// shared brush makes them one tool. Blur, Sharpen and Smudge lose their
    /// soft continuous sweep (hardness 0, spacing 0.05) the same way, Dodge,
    /// Burn and Sponge their 60px reach, and the Clone and Healing brushes
    /// their 40px at 0.05.
    pub fn set_tool(&mut self, tool: ToolId) {
        if self.tool != tool {
            self.brushes.insert(self.tool, self.brush);
            self.brush = self.brush_for(tool);
            self.tool = tool;
            self.touch();
        }
    }

    /// The brush `tool` paints with: whatever the options bar and the bracket
    /// keys have made of it, or the tuning [`tools::registry::make`] gives that
    /// tool if it has never been selected.
    pub fn brush_for(&self, tool: ToolId) -> BrushSettings {
        if tool == self.tool {
            return self.brush;
        }
        self.brushes
            .get(&tool)
            .copied()
            .unwrap_or_else(|| seeded_brush(tool))
    }

    /// The tool that is actually acting, which is the hand while Space is held.
    pub fn effective_tool(&self) -> ToolId {
        if self.temporary_hand {
            ToolId::Hand
        } else {
            self.tool
        }
    }

    /// The active tool's brush. [`Editor::brush_for`] answers for any other.
    pub fn brush(&self) -> &BrushSettings {
        &self.brush
    }

    /// Replace the active tool's brush — the options bar and `[` / `]`.
    pub fn set_brush(&mut self, brush: BrushSettings) {
        self.brush = brush;
        self.touch();
    }

    pub fn foreground(&self) -> [f32; 4] {
        self.foreground
    }

    pub fn background(&self) -> [f32; 4] {
        self.background
    }

    pub fn set_foreground(&mut self, rgba: [f32; 4]) {
        self.foreground = rgba;
        self.touch();
    }

    pub fn set_background(&mut self, rgba: [f32; 4]) {
        self.background = rgba;
        self.touch();
    }

    pub fn panels_visible(&self) -> bool {
        self.panels_visible
    }

    pub fn temporary_hand(&self) -> bool {
        self.temporary_hand
    }

    /// Space was released: give the previous tool back.
    pub fn release_temporary_hand(&mut self) {
        if self.temporary_hand {
            self.temporary_hand = false;
            self.touch();
        }
    }

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    /// The last thing worth telling the user, for the status bar.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// What Edit ▸ Copy last lifted, if anything. See [`Clipboard`].
    pub fn clipboard(&self) -> Option<&Clipboard> {
        self.clipboard.as_ref()
    }

    pub fn set_clipboard(&mut self, clipboard: Clipboard) {
        self.clipboard = Some(clipboard);
        self.touch();
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = Some(message.into());
        self.touch();
    }

    /// The window title: the document name, a bullet while it has unsaved
    /// changes, and the application name.
    pub fn window_title(&self) -> String {
        match self.active() {
            Some(doc) if doc.is_dirty() => format!("• {} — Raster Studio", doc.title()),
            Some(doc) => format!("{} — Raster Studio", doc.title()),
            None => "Raster Studio".to_string(),
        }
    }

    /// Packages currently open, for the crash marker.
    pub fn open_project_paths(&self) -> Vec<PathBuf> {
        self.docs
            .iter()
            .filter_map(|d| d.project_path().map(Path::to_path_buf))
            .collect()
    }

    /// Scratch autosaves this run has written, for the crash marker.
    ///
    /// [`Editor::open_project_paths`] covers documents that have a package of
    /// their own; this covers the ones that do not, and without it their
    /// autosaves would be work that nothing could ever read back.
    pub fn autosave_paths(&self) -> Vec<PathBuf> {
        self.autosaves.values().cloned().collect()
    }

    /// Where the scratch autosave of `id` lives, if one has been written.
    pub fn autosave_path_of(&self, id: DocumentId) -> Option<&Path> {
        self.autosaves.get(&id).map(PathBuf::as_path)
    }

    /// Delete a document's scratch autosave, if it has one.
    ///
    /// Called when the document is saved somewhere the user chose, and when it
    /// is closed cleanly: the safety net has served its purpose and the scratch
    /// directory must not grow without bound.
    fn discard_autosave(&mut self, id: DocumentId) {
        let Some(path) = self.autosaves.remove(&id) else {
            return;
        };
        // A package is a directory. `NotFound` is the normal case when the user
        // cleaned the scratch folder themselves.
        if let Err(e) = std::fs::remove_dir_all(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("cannot clear the autosave {}: {e}", path.display());
            }
        }
    }

    /// `true` when closing the window would lose work.
    pub fn has_unsaved_work(&self) -> bool {
        self.docs.iter().any(|d| d.is_dirty())
    }

    fn mint_id(&mut self) -> DocumentId {
        let id = DocumentId(self.next_id);
        self.next_id += 1;
        id
    }

    // ------------------------------------------------------------- opening

    /// `true` when `path` looks like a `.rstudio` package rather than an image.
    pub fn is_project_path(path: &Path) -> bool {
        let named = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case(PROJECT_EXTENSION))
            .unwrap_or(false);
        named || (path.is_dir() && path.join(project_format::MANIFEST_FILE).is_file())
    }

    /// Open a file (image or project) into a new tab.
    pub fn open_path(&mut self, path: &Path) -> Result<DocumentId, DocumentError> {
        let depth = self.prefs.history_depth;
        let id = self.mint_id();
        let doc = if Self::is_project_path(path) {
            OpenDocument::open_project(id, path, depth)?
        } else {
            OpenDocument::open_image(id, path, depth)?
        };
        self.docs.push(doc);
        self.active = Some(self.docs.len() - 1);
        self.recent.record(path);
        let _ = self.recent.save(&self.paths.recent_file());
        self.status = Some(format!("Opened {}", path.display()));
        self.touch();
        Ok(id)
    }

    /// Open several files, as a drag-and-drop delivers them. Returns the ids
    /// that opened; failures are reported to the user and the path is dropped
    /// from the recent list rather than left pointing at something broken.
    pub fn open_paths(&mut self, paths: &[PathBuf]) -> Vec<DocumentId> {
        let mut opened = Vec::new();
        for path in paths {
            match self.open_path(path) {
                Ok(id) => opened.push(id),
                Err(e) => {
                    self.recent.forget(path);
                    let message = format!("{}\n\n{e}", path.display());
                    self.dialogs.report_error("Cannot open this file", &message);
                    self.status = Some(format!("Could not open {}", path.display()));
                    self.touch();
                }
            }
        }
        opened
    }

    /// Close the document at `index`, asking about unsaved work first.
    pub fn close_document(&mut self, index: usize) -> Result<(), ActionError> {
        let action = Action::CloseDocument;
        let Some((dirty, title)) = self
            .docs
            .get(index)
            .map(|d| (d.is_dirty(), d.title().to_string()))
        else {
            return Err(ActionError::unavailable(action, "no document is open"));
        };
        if dirty {
            match self.dialogs.confirm_close(&title) {
                CloseChoice::Cancel => return Err(ActionError::Cancelled(action)),
                CloseChoice::Save => self.save_document(index, false)?,
                CloseChoice::Discard => {}
            }
        }
        if let Some(doc) = self.docs.get(index) {
            let id = doc.id();
            self.discard_autosave(id);
        }
        self.docs.remove(index);
        self.active = if self.docs.is_empty() {
            None
        } else {
            Some(index.min(self.docs.len() - 1))
        };
        self.touch();
        Ok(())
    }

    // -------------------------------------------------------------- saving

    /// Save the document at `index`. `force_dialog` is Save As.
    fn save_document(&mut self, index: usize, force_dialog: bool) -> Result<(), ActionError> {
        let action = if force_dialog {
            Action::SaveAs
        } else {
            Action::Save
        };
        let Some(doc) = self.docs.get(index) else {
            return Err(ActionError::unavailable(action, "no document is open"));
        };
        let existing = doc.project_path().map(Path::to_path_buf);
        let target = match (force_dialog, existing) {
            (false, Some(path)) => path,
            _ => {
                let suggested = doc.suggested_save_path();
                match self.dialogs.pick_save_path(&suggested) {
                    Some(p) => p,
                    None => return Err(ActionError::Cancelled(action)),
                }
            }
        };
        let version = self.app_version.clone();
        let doc = self
            .docs
            .get_mut(index)
            .expect("index checked immediately above");
        doc.save_to(&target, &version)
            .map_err(|e| ActionError::failed(action, e))?;
        let id = doc.id();
        // The work now lives somewhere the user chose, so the safety net goes.
        self.discard_autosave(id);
        self.recent.record(&target);
        let _ = self.recent.save(&self.paths.recent_file());
        self.status = Some(format!("Saved {}", target.display()));
        self.touch();
        Ok(())
    }

    // ------------------------------------------------------------ autosave

    /// When the next autosave is due, if autosave is on.
    pub fn next_autosave(&self) -> Option<Instant> {
        self.next_autosave
    }

    /// Run the autosave timer. Call it once a frame with the current time.
    ///
    /// `now` is a parameter rather than an `Instant::now()` inside so the
    /// schedule is testable without sleeping.
    pub fn autosave_tick(&mut self, now: Instant) -> Option<AutosaveReport> {
        let interval = self.prefs.autosave_interval()?;
        match self.next_autosave {
            None => {
                // First tick after start (or after a preferences change) only
                // arms the timer; it must not autosave immediately.
                self.next_autosave = now.checked_add(interval);
                None
            }
            Some(due) if now < due => None,
            Some(_) => {
                self.next_autosave = now.checked_add(interval);
                let report = self.autosave_now();
                (!report.is_empty()).then_some(report)
            }
        }
    }

    /// Write every dirty document out, regardless of the timer.
    ///
    /// A document that has a project keeps being written to it. One that has
    /// never been saved goes to the scratch directory under a name that is
    /// unique to this run *and* this document ([`mint_session_tag`]), is
    /// recorded in [`Editor::autosave_paths`] so the crash marker can point the
    /// next start at it, and **stays dirty** — the user has still not saved it
    /// anywhere they chose.
    pub fn autosave_now(&mut self) -> AutosaveReport {
        let mut report = AutosaveReport::default();
        let scratch = self.prefs.scratch_dir(&self.paths);
        let version = self.app_version.clone();
        let tag = self.session_tag.clone();
        // Read out before the mutable walk over `self.docs`; a document that
        // was recovered from a previous run's autosave keeps writing to that
        // same package rather than starting a second one.
        let existing = self.autosaves.clone();
        let mut fresh: Vec<(DocumentId, PathBuf)> = Vec::new();
        let mut adopted: Vec<DocumentId> = Vec::new();

        for doc in self.docs.iter_mut().filter(|d| d.is_dirty()) {
            let id = doc.id();
            let (target, is_scratch) = match doc.project_path() {
                Some(p) => (p.to_path_buf(), false),
                None => {
                    let path = existing.get(&id).cloned().unwrap_or_else(|| {
                        scratch.join(format!("autosave-{tag}-{}.{PROJECT_EXTENSION}", id.0))
                    });
                    (path, true)
                }
            };
            if let Some(parent) = target.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    report.failed.push((id, e.to_string()));
                    continue;
                }
            }
            match doc.write_snapshot(&target, &version) {
                Ok(()) => {
                    if is_scratch {
                        fresh.push((id, target.clone()));
                    } else {
                        adopted.push(id);
                    }
                    report.written.push((id, target));
                }
                Err(e) => report.failed.push((id, e.to_string())),
            }
        }

        for (id, path) in fresh {
            self.autosaves.insert(id, path);
        }
        // A document that gained a package since the last pass has just been
        // written there; its scratch copy is now a stale duplicate.
        for id in adopted {
            self.discard_autosave(id);
        }
        if !report.is_empty() {
            self.touch();
        }
        report
    }

    // ------------------------------------------------------------ recovery

    /// Offer to restore whatever a crashed run left behind.
    ///
    /// Two kinds of work, recovered two ways:
    ///
    /// * a document that **had** a package — its journal holds the commands
    ///   accepted after the last save, and they are replayed onto the package;
    /// * a document that had **none** — there is no journal, so the whole of it
    ///   is in a scratch autosave, which is opened and then detached from disk
    ///   ([`OpenDocument::detach_from_disk`]) because the scratch directory is
    ///   not a location the user chose.
    pub fn recover(&mut self, previous: &SessionRecord) -> RecoveryReport {
        let mut report = RecoveryReport::default();
        self.recover_projects(previous, &mut report);
        self.recover_autosaves(previous, &mut report);
        if !report.is_empty() {
            self.touch();
        }
        report
    }

    /// `true` when `path` is a scratch autosave this application wrote, and so
    /// is safe to delete once its offer has been declined.
    ///
    /// A recovered-from autosave that is already this run's own (it is in
    /// [`Editor::autosaves`]) is excluded: deleting it would take away the
    /// safety net of a document that is open right now.
    fn owns_scratch_autosave(&self, path: &Path) -> bool {
        if self.autosaves.values().any(|p| p == path) {
            return false;
        }
        let scratch = self.prefs.scratch_dir(&self.paths);
        path.parent() == Some(scratch.as_path())
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(PROJECT_EXTENSION))
    }

    fn recover_autosaves(&mut self, previous: &SessionRecord, report: &mut RecoveryReport) {
        for autosave in &previous.autosaves {
            if !autosave.exists() {
                continue;
            }
            let id = self.mint_id();
            let depth = self.prefs.history_depth;
            let mut doc = match OpenDocument::open_project(id, autosave, depth) {
                Ok(d) => d,
                Err(e) => {
                    report.failed.push((autosave.clone(), e.to_string()));
                    continue;
                }
            };
            // Ask by the document's own name, not by the scratch file's — the
            // user has never seen `autosave-3f2a-1.rstudio`.
            let title = doc.title().to_string();
            if !self.dialogs.confirm_recover(&title) {
                report.declined.push(autosave.clone());
                // Declined once is declined for good; leaving it would offer
                // the same file at every future start. But only *scratch*
                // autosaves are this application's to delete: a marker naming
                // anything else is either corrupt or not describing an autosave
                // at all, and `remove_dir_all` on a guess destroys real work.
                if self.owns_scratch_autosave(autosave) {
                    let _ = std::fs::remove_dir_all(autosave);
                } else {
                    tracing::warn!(
                        "leaving {} alone: it is not in this run's scratch directory",
                        autosave.display()
                    );
                }
                continue;
            }
            doc.detach_from_disk();
            doc.invalidate_all();
            self.docs.push(doc);
            self.active = Some(self.docs.len() - 1);
            // Keep writing to the file it came from, so this run's autosaves do
            // not leave the previous run's copy behind for ever.
            self.autosaves.insert(id, autosave.clone());
            report.restored.push((autosave.clone(), 0));
        }
    }

    fn recover_projects(&mut self, previous: &SessionRecord, report: &mut RecoveryReport) {
        for project in &previous.open_projects {
            let found = match session::recoverable(project) {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(e) => {
                    report.failed.push((project.clone(), e.to_string()));
                    continue;
                }
            };
            let name = project
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| project.display().to_string());
            if !self.dialogs.confirm_recover(&name) {
                report.declined.push(project.clone());
                continue;
            }
            if let Err(e) = self.open_path(project) {
                report.failed.push((project.clone(), e.to_string()));
                continue;
            }
            let doc = self.docs.last_mut().expect("just opened");
            let (applied, error) =
                session::replay(&mut doc.document, &mut doc.history, &found.commands);
            doc.invalidate_all();
            match error {
                Some(e) => report.failed.push((project.clone(), e)),
                None => report.restored.push((project.clone(), applied)),
            }
        }
    }

    // ---------------------------------------------------------- enablement

    /// Whether `action` applies right now, and if not, what to tell the user.
    ///
    /// The menu bar renders from this, so a greyed-out item always has a
    /// reason attached.
    pub fn can(&self, action: Action) -> Result<(), ActionError> {
        let no_doc = || ActionError::unavailable(action, "no document is open");
        let doc = self.active();
        match action {
            Action::NewDocument
            | Action::Open
            | Action::OpenProject
            | Action::Quit
            | Action::TogglePanels
            | Action::ShowPreferences
            | Action::ShowFileInfo
            | Action::SelectTool(_)
            | Action::DecreaseBrushSize
            | Action::IncreaseBrushSize
            | Action::SwapColors
            | Action::ResetColors => Ok(()),

            Action::Save => match doc {
                None => Err(no_doc()),
                Some(d) if !d.is_dirty() && d.project_path().is_some() => Err(
                    ActionError::unavailable(action, "there are no unsaved changes"),
                ),
                Some(_) => Ok(()),
            },
            Action::SaveAs
            | Action::Export
            | Action::CloseDocument
            | Action::ZoomIn
            | Action::ZoomOut
            | Action::ZoomFit
            | Action::ZoomActualPixels
            | Action::TemporaryHand
            | Action::NewLayer => doc.map(|_| ()).ok_or_else(no_doc),

            Action::Undo => match doc {
                None => Err(no_doc()),
                Some(d) if !d.history.can_undo() => {
                    Err(ActionError::unavailable(action, "there is nothing to undo"))
                }
                Some(_) => Ok(()),
            },
            Action::Redo => match doc {
                None => Err(no_doc()),
                Some(d) if !d.history.can_redo() => {
                    Err(ActionError::unavailable(action, "there is nothing to redo"))
                }
                Some(_) => Ok(()),
            },

            Action::DeleteLayer | Action::ToggleLayerVisibility => match doc {
                None => Err(no_doc()),
                Some(d) if d.document.active_layer().is_none() => Err(ActionError::unavailable(
                    action,
                    "select a layer in the Layers panel first",
                )),
                Some(_) => Ok(()),
            },
            Action::DuplicateLayer => match doc {
                None => Err(no_doc()),
                Some(d) => match d.document.active_layer() {
                    None => Err(ActionError::unavailable(
                        action,
                        "select a layer in the Layers panel first",
                    )),
                    Some(id) if d.document.layers.get(id).is_some_and(Layer::is_group) => {
                        Err(ActionError::unavailable(
                            action,
                            "duplicating a group is not implemented yet",
                        ))
                    }
                    Some(_) => Ok(()),
                },
            },

            Action::NextDocument | Action::PreviousDocument => {
                if self.docs.len() < 2 {
                    Err(ActionError::unavailable(
                        action,
                        "only one document is open",
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    // -------------------------------------------------------------- action

    /// Perform `action`.
    ///
    /// Exhaustive with no wildcard arm: a new [`Action`] variant does not
    /// compile until it is handled here.
    pub fn dispatch(&mut self, action: Action) -> Result<Effect, ActionError> {
        self.can(action)?;
        // Undo, Redo, New Layer — anything named enough to be an action puts
        // something else on top of the history, so a drag that was coalescing
        // must not fold its next frame into whatever is there now.
        self.kind_gesture = None;
        match action {
            Action::NewDocument => self.act_new_document(),
            Action::Open => self.act_open(),
            Action::OpenProject => self.act_open_project(),
            Action::Save => self.act_save(false),
            Action::SaveAs => self.act_save(true),
            Action::Export => self.act_export(),
            Action::CloseDocument => {
                let index = self.active.expect("`can` required a document");
                self.close_document(index)?;
                Ok(Effect::DocumentSet)
            }
            Action::Quit => self.act_quit(),
            Action::Undo => self.act_undo(),
            Action::Redo => self.act_redo(),
            Action::NewLayer => self.act_new_layer(),
            Action::DeleteLayer => self.act_delete_layer(),
            Action::DuplicateLayer => self.act_duplicate_layer(),
            Action::ToggleLayerVisibility => self.act_toggle_layer_visibility(),
            Action::ZoomIn => self.act_zoom(ZOOM_STEP),
            Action::ZoomOut => self.act_zoom(1.0 / ZOOM_STEP),
            Action::ZoomFit => {
                let doc = self.active_mut().expect("`can` required a document");
                doc.camera.fit();
                self.touch();
                Ok(Effect::View)
            }
            Action::ZoomActualPixels => {
                let doc = self.active_mut().expect("`can` required a document");
                doc.camera.zoom = 1.0;
                self.touch();
                Ok(Effect::View)
            }
            Action::TogglePanels => {
                self.panels_visible = !self.panels_visible;
                self.touch();
                Ok(Effect::Panels)
            }
            Action::ShowPreferences => {
                self.preferences_open = !self.preferences_open;
                if !self.preferences_open {
                    // A conflict prompt belongs to the window that raised it.
                    self.pending_conflict = None;
                }
                self.touch();
                Ok(Effect::Preferences)
            }
            Action::ShowFileInfo => {
                self.toggle_file_info();
                self.touch();
                Ok(Effect::Preferences)
            }
            Action::SelectTool(key) => {
                let next = registry::cycle(key.char(), Some(self.tool)).ok_or_else(|| {
                    ActionError::unavailable(action, "no tool answers to that key")
                })?;
                // Through `set_tool`, so a tool reached by its keyboard letter
                // takes up its own brush exactly as one clicked in the palette
                // does.
                self.set_tool(next);
                self.status = Some(
                    registry::info(next)
                        .map(|i| i.name.to_string())
                        .unwrap_or_else(|| format!("{next:?}")),
                );
                self.touch();
                Ok(Effect::Tool)
            }
            Action::TemporaryHand => {
                // Idempotent by design: a held Space repeats, and every repeat
                // must leave the hand engaged rather than toggling it off.
                self.temporary_hand = true;
                self.touch();
                Ok(Effect::Tool)
            }
            Action::DecreaseBrushSize => self.act_scale_brush(1.0 / 1.25),
            Action::IncreaseBrushSize => self.act_scale_brush(1.25),
            // Both colour arms report the new foreground the way the brush
            // arms report the new size. The colour wells in the tool strip are
            // the visible half of this; the status line is what tells a user
            // who invoked it from the menu that anything happened at all.
            Action::SwapColors => {
                std::mem::swap(&mut self.foreground, &mut self.background);
                self.set_status(format!("Foreground {}", color_hex(self.foreground)));
                Ok(Effect::Color)
            }
            Action::ResetColors => {
                self.foreground = DEFAULT_FOREGROUND;
                self.background = DEFAULT_BACKGROUND;
                self.set_status(format!("Foreground {}", color_hex(self.foreground)));
                Ok(Effect::Color)
            }
            Action::NextDocument => self.act_step_document(1),
            Action::PreviousDocument => self.act_step_document(-1),
        }
    }

    fn act_new_document(&mut self) -> Result<Effect, ActionError> {
        self.untitled_count += 1;
        let title = if self.untitled_count == 1 {
            "Untitled".to_string()
        } else {
            format!("Untitled {}", self.untitled_count)
        };
        let id = self.mint_id();
        let (w, h) = NEW_DOCUMENT_SIZE;
        let doc = OpenDocument::blank(id, w, h, &title, self.prefs.history_depth)
            .map_err(|e| ActionError::failed(Action::NewDocument, e))?;
        self.docs.push(doc);
        self.active = Some(self.docs.len() - 1);
        self.touch();
        Ok(Effect::DocumentSet)
    }

    fn act_open(&mut self) -> Result<Effect, ActionError> {
        let Some(path) = self.dialogs.pick_open_file() else {
            return Err(ActionError::Cancelled(Action::Open));
        };
        self.open_path(&path)
            .map_err(|e| ActionError::failed(Action::Open, e))?;
        Ok(Effect::DocumentSet)
    }

    /// File ▸ Open Project…, through the platform's *folder* picker.
    ///
    /// A package is a directory, so this is the only route by which the
    /// application's own save format can be reopened from a dialog. A folder
    /// that is not a package is refused with a message that says so rather than
    /// opened as something unreadable.
    fn act_open_project(&mut self) -> Result<Effect, ActionError> {
        let action = Action::OpenProject;
        let Some(path) = self.dialogs.pick_open_project() else {
            return Err(ActionError::Cancelled(action));
        };
        if !path.join(project_format::MANIFEST_FILE).is_file() {
            return Err(ActionError::Failed {
                action,
                reason: format!(
                    "{} is not a Raster Studio project — it has no {}",
                    path.display(),
                    project_format::MANIFEST_FILE
                ),
            });
        }
        self.open_path(&path)
            .map_err(|e| ActionError::failed(action, e))?;
        Ok(Effect::DocumentSet)
    }

    fn act_save(&mut self, force_dialog: bool) -> Result<Effect, ActionError> {
        let index = self.active.expect("`can` required a document");
        self.save_document(index, force_dialog)?;
        Ok(Effect::Saved)
    }

    fn act_export(&mut self) -> Result<Effect, ActionError> {
        let index = self.active.expect("`can` required a document");
        let suggested = self.docs[index].suggested_export_path();
        let Some(target) = self.dialogs.pick_export_path(&suggested) else {
            return Err(ActionError::Cancelled(Action::Export));
        };
        self.docs[index]
            .export_to(&target)
            .map_err(|e| ActionError::failed(Action::Export, e))?;
        self.status = Some(format!("Exported {}", target.display()));
        self.touch();
        Ok(Effect::Exported)
    }

    fn act_quit(&mut self) -> Result<Effect, ActionError> {
        // Walk from the end so removing one does not renumber the rest.
        for index in (0..self.docs.len()).rev() {
            if self.docs[index].is_dirty() {
                let title = self.docs[index].title().to_string();
                match self.dialogs.confirm_close(&title) {
                    CloseChoice::Cancel => return Err(ActionError::Cancelled(Action::Quit)),
                    CloseChoice::Save => self.save_document(index, false)?,
                    CloseChoice::Discard => {}
                }
            }
        }
        self.quit_requested = true;
        self.touch();
        Ok(Effect::Quit)
    }

    fn act_undo(&mut self) -> Result<Effect, ActionError> {
        let doc = self.active_mut().expect("`can` required a document");
        let undone = doc
            .undo()
            .map_err(|e| ActionError::failed(Action::Undo, e))?;
        if !undone {
            // `can` said there was something; if that is no longer true the
            // state changed under us and saying so beats a silent no-op.
            return Err(ActionError::unavailable(
                Action::Undo,
                "there is nothing to undo",
            ));
        }
        self.touch();
        Ok(Effect::DocumentEdited)
    }

    fn act_redo(&mut self) -> Result<Effect, ActionError> {
        let doc = self.active_mut().expect("`can` required a document");
        let redone = doc
            .redo()
            .map_err(|e| ActionError::failed(Action::Redo, e))?;
        if !redone {
            return Err(ActionError::unavailable(
                Action::Redo,
                "there is nothing to redo",
            ));
        }
        self.touch();
        Ok(Effect::DocumentEdited)
    }

    fn act_new_layer(&mut self) -> Result<Effect, ActionError> {
        let doc = self.active_mut().expect("`can` required a document");
        // One naming rule for both routes to this intent. `Layer {len + 1}`
        // alone repeats a name as soon as a layer has been deleted, and three
        // rows nobody can tell apart is what the layers dock used to show.
        let layer = Layer::raster(crate::doc::next_layer_name(&doc.document));
        let id = layer.id;
        doc.apply(Command::create_layer(layer))
            .map_err(|e| ActionError::failed(Action::NewLayer, e))?;
        // The cursor, not content: a new layer is the one you want to paint on.
        doc.document
            .set_active_layer(Some(id))
            .expect("the layer was just created");
        self.touch();
        Ok(Effect::DocumentEdited)
    }

    fn act_delete_layer(&mut self) -> Result<Effect, ActionError> {
        let doc = self.active_mut().expect("`can` required a document");
        let layer_id = doc
            .document
            .active_layer()
            .expect("`can` required an active layer");
        doc.apply(Command::DeleteLayer { layer_id })
            .map_err(|e| ActionError::failed(Action::DeleteLayer, e))?;
        self.touch();
        Ok(Effect::DocumentEdited)
    }

    fn act_duplicate_layer(&mut self) -> Result<Effect, ActionError> {
        let action = Action::DuplicateLayer;
        let doc = self.active_mut().expect("`can` required a document");
        let source_id = doc
            .document
            .active_layer()
            .expect("`can` required an active layer");
        let source = doc
            .document
            .layers
            .get(source_id)
            .expect("the active layer is in the tree")
            .clone();

        let mut copy = source.clone();
        copy.id = LayerId::new();
        copy.name = format!("{} copy", source.name);
        // A duplicated mask needs its own identity, or both layers would edit
        // one set of coverage tiles.
        let mask_copy = copy.mask.as_mut().map(|m| {
            let old = m.id;
            m.id = MaskId::new();
            (old, m.id)
        });
        let new_id = copy.id;

        let mut commands = vec![Command::create_layer(copy)];
        // Pixels are content-addressed, so "copying" them is copying hashes:
        // the bytes are shared and the duplicate costs nothing on disk.
        if let Some(map) = doc.document.layer_tiles(source_id) {
            let edits: Vec<_> = map
                .iter()
                .map(|(coord, hash)| editor_core::TileEdit::set(coord, hash))
                .collect();
            if !edits.is_empty() {
                commands.push(
                    Command::paint_tiles(editor_core::PixelTarget::Layer(new_id), edits)
                        .map_err(|e| ActionError::failed(action, e))?,
                );
            }
        }
        if let Some((old_mask, _)) = mask_copy {
            if let Some(map) = doc
                .document
                .pixels
                .tiles(editor_core::PixelKey::Mask(old_mask))
            {
                let edits: Vec<_> = map
                    .iter()
                    .map(|(coord, hash)| editor_core::TileEdit::set(coord, hash))
                    .collect();
                if !edits.is_empty() {
                    commands.push(
                        Command::paint_tiles(editor_core::PixelTarget::Mask(new_id), edits)
                            .map_err(|e| ActionError::failed(action, e))?,
                    );
                }
            }
        }

        doc.apply(Command::Transaction {
            label: format!("Duplicate {}", source.name),
            commands,
        })
        .map_err(|e| ActionError::failed(action, e))?;
        doc.document
            .set_active_layer(Some(new_id))
            .expect("the duplicate was just created");
        self.touch();
        Ok(Effect::DocumentEdited)
    }

    fn act_toggle_layer_visibility(&mut self) -> Result<Effect, ActionError> {
        let action = Action::ToggleLayerVisibility;
        let doc = self.active_mut().expect("`can` required a document");
        let layer_id = doc
            .document
            .active_layer()
            .expect("`can` required an active layer");
        let visible = doc
            .document
            .layers
            .get(layer_id)
            .expect("the active layer is in the tree")
            .visible;
        doc.apply(Command::SetLayerProperties {
            layer_id,
            patch: LayerPatch {
                visible: Some(!visible),
                ..Default::default()
            },
        })
        .map_err(|e| ActionError::failed(action, e))?;
        self.touch();
        Ok(Effect::DocumentEdited)
    }

    fn act_zoom(&mut self, factor: f32) -> Result<Effect, ActionError> {
        let doc = self.active_mut().expect("`can` required a document");
        let anchor = doc.camera.viewport_size * 0.5;
        doc.camera.zoom_at(anchor, factor);
        self.touch();
        Ok(Effect::View)
    }

    fn act_scale_brush(&mut self, factor: f32) -> Result<Effect, ActionError> {
        let scaled = (self.brush.size * factor).clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE);
        // Below ~5px a 25% step rounds back onto the value it started from, so
        // the bracket key would do nothing at exactly the sizes where one pixel
        // matters most. Step by a whole pixel there instead.
        let next = if (scaled - self.brush.size).abs() < 1.0 {
            let delta = if factor > 1.0 { 1.0 } else { -1.0 };
            (self.brush.size + delta).clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE)
        } else {
            scaled.round()
        };
        if (next - self.brush.size).abs() < f32::EPSILON {
            let action = if factor > 1.0 {
                Action::IncreaseBrushSize
            } else {
                Action::DecreaseBrushSize
            };
            return Err(ActionError::unavailable(
                action,
                if factor > 1.0 {
                    "the brush is already at its largest"
                } else {
                    "the brush is already at its smallest"
                },
            ));
        }
        self.brush.size = next;
        self.status = Some(format!("Brush {} px", next as i32));
        self.touch();
        Ok(Effect::Tool)
    }

    fn act_step_document(&mut self, delta: isize) -> Result<Effect, ActionError> {
        let len = self.docs.len() as isize;
        let current = self.active.unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.active = Some(next);
        self.touch();
        Ok(Effect::DocumentSet)
    }

    // ------------------------------------------------------------ keyboard

    /// Resolve a chord and perform whatever it names.
    ///
    /// `Ok(None)` means the chord is not bound, which is not an error.
    pub fn handle_chord(
        &mut self,
        chord: &crate::keymap::Chord,
    ) -> Result<Option<Effect>, ActionError> {
        match self.keymap.resolve(chord) {
            Some(action) => self.dispatch(action).map(Some),
            None => Ok(None),
        }
    }
}

/// Free helper so the shell can name a layer kind in a message without
/// depending on `layer_model`'s internals.
pub fn layer_kind_name(kind: &LayerKind) -> &'static str {
    match kind {
        LayerKind::Raster(_) => "raster",
        LayerKind::Group(_) => "group",
        LayerKind::Adjustment(_) => "adjustment",
        LayerKind::Text(_) => "text",
        LayerKind::Shape(_) => "shape",
        LayerKind::SmartObject(_) => "smart object",
        LayerKind::Generator(_) => "generator",
    }
}

/// Duration between autosaves, exposed for the shell's frame scheduler.
pub fn autosave_period(prefs: &Preferences) -> Option<Duration> {
    prefs.autosave_interval()
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
