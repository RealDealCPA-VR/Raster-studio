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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use editor_core::pixels::{PixelTarget, TileDelta};
use editor_core::{Command, LayerPatch};
use layer_model::{Layer, LayerId, LayerKind, MaskId};
use raster::TILE_SIZE;
use tools::{registry, BrushSettings, ToolId};

use crate::action::Action;
use crate::dialogs::{
    BrowserUrls, CloseChoice, FileDialogs, NativeDialogs, UrlLauncher, PROJECT_EXTENSION,
};
use crate::doc::{DocumentError, DocumentId, OpenDocument};
use crate::keymap::{Chord, Conflict, Keymap};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::session::{self, SessionRecord};
use compositor::TileSource;

/// Make a layer name safe for a file name: keep letters, digits, spaces,
/// underscore and hyphen, collapse runs, and refuse a bare dot.
pub(crate) fn safe_file_name(name: &str) -> String {
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
        _ => {
            // A transaction wraps whole-document edits (the filter path's
            // write_layer); its members are exactly the commands isolation
            // masks, so recurse — the label and ordering survive.
            if let Command::Transaction { label, commands } = command {
                return Command::Transaction {
                    label,
                    commands: commands
                        .into_iter()
                        .map(|c| mask_paint_to_channel(doc, c, channel))
                        .collect(),
                };
            }
            command
        }
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
///
/// W2-G: an autosave is a job. `started` names every document the pass
/// handed to a worker; `written` and `failed` name the ones whose job had
/// *completed* by the time the pass returned — all of them under the inline
/// spawner the tests run with, none of them under worker threads, where the
/// completions arrive through [`Editor::poll_saves`] on later frames.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutosaveReport {
    pub started: Vec<(DocumentId, PathBuf)>,
    pub written: Vec<(DocumentId, PathBuf)>,
    pub failed: Vec<(DocumentId, String)>,
}

impl AutosaveReport {
    pub fn is_empty(&self) -> bool {
        self.started.is_empty() && self.written.is_empty() && self.failed.is_empty()
    }
}

/// A save (Ctrl+S, Save As, autosave) running on a worker.
struct PendingSave {
    id: DocumentId,
    kind: crate::jobs::SaveKind,
    target: PathBuf,
    /// The document's title at spawn time, for the status line.
    title: String,
    /// Shared with the worker; read once a frame for the status bar.
    progress: std::sync::Arc<project_format::SaveProgress>,
    rx: std::sync::mpsc::Receiver<crate::jobs::SaveOutcome>,
    /// The status text last shown for this save, so the poll rewrites the
    /// status line only when the numbers move — and only while the line
    /// still shows this save's own text (a newer message is not overwritten).
    shown: Option<String>,
    /// W2-G: a Ctrl+S / Save As the user pressed while this *autosave* was
    /// running, queued to start the moment it lands. Two writers to one
    /// package at once would race the swap, and dropping the user's save
    /// behind the timer's is not an answer either.
    follow_up: Option<PathBuf>,
}

/// W2-G: the side file a document's commands are journaled to while a save of
/// its snapshot runs on a worker — a sibling of the package, so the swap that
/// replaces the package never touches it.
pub const JOURNAL_HOLD_SUFFIX: &str = "journal-hold";

/// W2-G: where the journal hold of the package at `project` lives:
/// `P.rstudio` → `P.rstudio.journal-hold`, next to it.
pub fn journal_hold_path(project: &Path) -> PathBuf {
    let mut name = project.as_os_str().to_os_string();
    name.push(format!(".{JOURNAL_HOLD_SUFFIX}"));
    PathBuf::from(name)
}

/// The status-bar line for a save in flight. Pure, so the wording — and the
/// fact that it carries the tile count once the worker has one — is pinned
/// by a test rather than read off a screenshot.
pub fn save_status_text(title: &str, kind: crate::jobs::SaveKind, done: u64, total: u64) -> String {
    let verb = match kind {
        crate::jobs::SaveKind::Save => "Saving",
        crate::jobs::SaveKind::Autosave { .. } => "Autosaving",
    };
    if total == 0 {
        format!("{verb} {title}…")
    } else {
        format!("{verb} {title}… {done}/{total} tiles")
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
/// One captured step of an Actions recording: the command as applied, the
/// stack position of the layer it targeted (`None` when the command targets
/// no layer), and the tile BYTES the command's delta references — hashes are
/// keys into the recording document's own blob store, so a replay needs the
/// bytes themselves to re-insert into the replay document's store.
#[derive(Debug, Clone)]
pub struct RecordedEdit {
    pub command: Command,
    pub layer: Option<usize>,
    pub tiles: Vec<(raster::TileHash, Vec<u8>)>,
}

/// Every tile byte a command's pixel deltas reference, read from `doc`'s
/// store. Transactions are walked; a hash appears once.
fn captured_tiles(doc: &OpenDocument, command: &Command) -> Vec<(raster::TileHash, Vec<u8>)> {
    fn hashes_of(command: &Command, out: &mut Vec<raster::TileHash>) {
        match command {
            Command::PaintTiles { delta, .. } | Command::FillRegion { delta, .. } => {
                for edit in delta.edits() {
                    if let Some(hash) = edit.hash {
                        out.push(hash);
                    }
                }
            }
            Command::Transaction { commands, .. } => {
                for c in commands {
                    hashes_of(c, out);
                }
            }
            _ => {}
        }
    }
    let mut hashes = Vec::new();
    hashes_of(command, &mut hashes);
    hashes.sort_by_key(|h| format!("{h:?}"));
    hashes.dedup();
    hashes
        .into_iter()
        .filter_map(|h| doc.tiles.tile(h).map(|b| (h, b.to_vec())))
        .collect()
}

/// Rebuild a delta with its hashes re-keyed through `map`. A hash the map
/// lacks is left alone — it can only be one the replay store already had.
fn rekey_delta(
    delta: &editor_core::TileDelta,
    map: &std::collections::BTreeMap<String, raster::TileHash>,
) -> Option<editor_core::TileDelta> {
    let edits: Vec<editor_core::pixels::TileEdit> = delta
        .edits()
        .iter()
        .map(|edit| editor_core::pixels::TileEdit {
            coord: edit.coord,
            hash: edit.hash.map(|h| *map.get(&format!("{h:?}")).unwrap_or(&h)),
        })
        .collect();
    editor_core::pixels::TileDelta::new(edits)
        .map_err(|e| tracing::debug!("replay delta refused: {e}"))
        .ok()
}

/// The stack position of the layer a command's pixel edits target, if any.
///
/// Transactions are walked (their members carry the real targets); the first
/// layer target found wins, which is right for the commands a recording
/// captures — an edit touches one layer.
fn layer_position(tree: &layer_model::LayerTree, command: &Command) -> Option<usize> {
    fn target_of(command: &Command) -> Option<LayerId> {
        match command {
            Command::PaintTiles { target, .. } | Command::FillRegion { target, .. } => match target
            {
                editor_core::pixels::PixelTarget::Layer(id) => Some(*id),
                _ => None,
            },
            Command::Transaction { commands, .. } => commands.iter().find_map(target_of),
            _ => None,
        }
    }
    let id = target_of(command)?;
    tree.iter_depth_first().iter().position(|x| *x == id)
}

/// Rewrite a command's layer targets to `target`: the replay retargeting that
/// lets one recording serve every document with a deep enough stack. The
/// pixel payload (delta, rect, value) is untouched — only who receives it
/// changes.
fn remap_layer(
    command: Command,
    target: LayerId,
    map: &std::collections::BTreeMap<String, raster::TileHash>,
) -> Command {
    match command {
        Command::PaintTiles { target: old, delta } => {
            let delta = rekey_delta(&delta, map).unwrap_or(delta);
            Command::PaintTiles {
                target: retarget(old, target),
                delta,
            }
        }
        Command::FillRegion {
            target: old,
            rect,
            value,
            delta,
        } => {
            let delta = rekey_delta(&delta, map).unwrap_or(delta);
            Command::FillRegion {
                target: retarget(old, target),
                rect,
                value,
                delta,
            }
        }
        Command::Transaction { label, commands } => Command::Transaction {
            label,
            commands: commands
                .into_iter()
                .map(|c| remap_layer(c, target, map))
                .collect(),
        },
        other => other,
    }
}

fn retarget(
    old: editor_core::pixels::PixelTarget,
    target: LayerId,
) -> editor_core::pixels::PixelTarget {
    match old {
        editor_core::pixels::PixelTarget::Layer(_) => {
            editor_core::pixels::PixelTarget::Layer(target)
        }
        other => other,
    }
}

pub struct Editor {
    paths: AppPaths,
    prefs: Preferences,
    keymap: Keymap,
    recent: RecentFiles,
    /// Named patterns and brushes the Edit and Layer menus define and reuse.
    presets: asset_store::presets::PresetStore,
    dialogs: Box<dyn FileDialogs>,
    /// Help-menu browser launches (see [`dialogs::UrlLauncher`]); the shipped
    /// default opens the platform browser, tests inject a recorder.
    urls: Box<dyn UrlLauncher>,
    /// The OS image clipboard service (card 051) — the "separate job" the
    /// internal [`Clipboard`] store's doc comment names: crossing the process
    /// boundary. The OS clipboard by construction, a
    /// [`crate::clipboard::FakeClipboard`] in tests.
    image_clipboard: Box<dyn crate::clipboard::ImageClipboard>,
    /// Card 087: off-thread import jobs in flight, each with its receiver.
    import_jobs: Vec<std::sync::mpsc::Receiver<crate::jobs::ImportOutcome>>,
    /// The import generation: bumped when pending imports are cancelled, so
    /// a stale completion can never apply.
    import_generation: u64,
    /// W2-G: how jobs (imports, saves, exports) are started. Worker threads
    /// in the desktop binary ([`Editor::new`]), inline under
    /// [`Editor::with_state`], whatever a test injects through
    /// [`Editor::set_spawner`].
    spawner: crate::jobs::Spawner,
    /// W2-G: saves in flight — Ctrl+S, Save As and autosave alike — each with
    /// the receiver its completion arrives on.
    save_jobs: Vec<PendingSave>,
    /// W2-G: Export As batches in flight.
    export_jobs: Vec<std::sync::mpsc::Receiver<crate::jobs::ExportOutcome>>,
    /// Fingerprint of what Edit ▸ Copy last wrote to the OS image clipboard
    /// (card 052's ownership policy): paste compares the OS payload against
    /// it, so the editor's OWN copy pastes through the internal route (same
    /// pixels, in-place semantics) while anything ELSE on the OS clipboard —
    /// a screenshot, another app's image — wins as the fresher payload.
    os_copy_fingerprint: Option<u64>,
    /// Cached "does the OS clipboard hold an image?" answer:
    /// `(probed_at, has_image)`. The menu context is built EVERY frame, and
    /// reading the OS clipboard per frame (a 4K screenshot is ~33 MB) would
    /// be absurd — so the probe is throttled to one read per second and
    /// re-probed immediately when the window regains focus (the moment a
    /// foreign app's copy can arrive) and after our own copy/paste.
    os_image_probe: Option<(std::time::Instant, bool)>,
    app_version: String,
    /// The GPU adapter the window actually got, for the diagnostics bundle.
    gpu_adapter_name: Option<String>,

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
    /// Card 058: the content colour wells stashed while a document's edit
    /// target is its mask (mask editing swaps to white/black without losing
    /// these). Per-DOCUMENT: tabbing from a mask-targeted document A to a
    /// fresh document B and aiming B at a mask must not overwrite A's stash.
    content_color_backups: std::collections::HashMap<DocumentId, ([f32; 4], [f32; 4])>,
    /// Card 058 (review round 2): the colour wells are per-document state —
    /// a document aimed at its mask carries the mask pair, a content
    /// document carries the user's colours, and tabbing between them shows
    /// the right wells without one document's state leaking into another's.
    doc_colors: std::collections::HashMap<DocumentId, ([f32; 4], [f32; 4])>,
    /// The ramp the gradient tools paint with. The options bar and the
    /// gradient dialog edit the workspace's copy; the read-back lands here so
    /// [`crate::tool_input::ToolPointer`] can thread it into the tool's
    /// context, the same road the foreground colour travels.
    gradient_ramp: layer_model::Gradient,
    /// W1-C: the pattern the pattern-driven tools (Pattern Stamp, Pattern
    /// Fill) paint with, by preset name. `None` means "the most recently
    /// defined preset", which is what Edit ▸ Define Pattern leaves active;
    /// [`Self::set_active_pattern`] picks another. Threaded into the tool
    /// context per gesture like the ramp and the colours.
    active_pattern: Option<String>,

    panels_visible: bool,
    /// W2-X: Photopea's `F` cycle. The chrome mirrors it into
    /// `PaletteState::screen_mode` and gates its bands on it; the shell sets
    /// the window full screen from it. The editor is the one source of truth.
    screen_mode: ui::palette::ScreenMode,
    preferences_open: bool,
    /// Whether the File ▸ File Info… window (document metadata) is up.
    file_info_open: bool,
    /// The colour component paint/fill should write, when the Channels panel
    /// has selected one channel to edit. `None` edits all components.
    paint_channel: Option<usize>,
    /// Quick-mask mode (Photoshop's "Edit in Quick Mask Mode", `Q`): while on,
    /// pixel edits land in a scratch layer's mask instead of the document, and
    /// leaving converts that painted coverage into the selection.
    quick_mask: bool,
    /// The scratch layer carrying the quick-mask coverage, created on enter
    /// and deleted on leave. `None` outside quick-mask mode.
    quick_mask_layer: Option<LayerId>,
    /// The Actions panel's recording: when set, every command that lands
    /// through [`Self::apply_command`] is captured with the stack position of
    /// the layer it targets, in order.
    recording: Option<Vec<RecordedEdit>>,
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
    /// Card 067: the style block Copy Layer Style captured — style fields
    /// ONLY (never position, masks, text, or asset identity), pasted by
    /// `paste_layer_style` as one wholesale `LayerPatch::effects` replace.
    copied_style: Option<layer_model::LayerEffects>,
    /// The sticky content-vs-mask target per document. Validity is computed at
    /// read time — see [`crate::edit_target`]. Card 007.
    pub(crate) edit_targets: crate::edit_target::EditTargets,
    /// The live pending edit, if a gesture is holding one (card 011).
    pub(crate) edit_sessions: crate::edit_session::EditSessions,
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

/// Card 058: the colour wells a MASK edit target shows — white foreground
/// reveals coverage, black conceals (the mask semantics, not the content
/// colours, which are stashed until the target switches back).
pub const MASK_EDIT_FOREGROUND: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
pub const MASK_EDIT_BACKGROUND: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// `#RRGGBB` for a colour, so the status bar can name what changed.
pub fn color_hex(rgba: [f32; 4]) -> String {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", c(rgba[0]), c(rgba[1]), c(rgba[2]))
}

/// Bumped once per [`Editor`] built, so two editors in one process cannot mint
/// the same session tag even if the clock does not tick between them.
static SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The card 052 ownership fingerprint: dimensions plus every pixel byte, so
/// two images agree only when they ARE the same image. Any standard hasher
/// works — this only ever compares an editor against itself.
fn fingerprint_image(image: &crate::clipboard::ClipboardImage) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    image.width.hash(&mut hasher);
    image.height.hash(&mut hasher);
    hasher.write(&image.rgba);
    hasher.finish()
}

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
    ///
    /// This is the editor the application runs (see [`Editor::native`]), so
    /// its jobs — imports, saves, autosaves, exports — run on worker threads:
    /// the interaction thread never blocks on a disk. [`Editor::with_state`]
    /// is the deterministic constructor the tests build on.
    pub fn new(paths: AppPaths, dialogs: Box<dyn FileDialogs>) -> Self {
        let prefs = Preferences::load(&paths.preferences_file());
        let recent = RecentFiles::load(&paths.recent_file());
        let mut editor = Editor::with_state(paths, prefs, recent, dialogs);
        editor.spawner = crate::jobs::spawn_thread;
        editor
    }

    /// The editor the desktop binary runs: real dialogs, real config directory.
    pub fn native() -> Self {
        Editor::new(AppPaths::discover(), Box::new(NativeDialogs))
    }

    /// An editor over explicit state, with every job run **inline**: a save,
    /// an export or an import completes before the call that started it
    /// returns. That is the deterministic mode the unit tests run in;
    /// [`Editor::new`] switches to worker threads, and a test that wants to
    /// watch a job *in flight* injects a queueing spawner through
    /// [`Editor::set_spawner`].
    pub fn with_state(
        paths: AppPaths,
        prefs: Preferences,
        recent: RecentFiles,
        dialogs: Box<dyn FileDialogs>,
    ) -> Self {
        let prefs = prefs.sanitized();
        // The catalogue is process-wide; the stored language is installed
        // before anything draws a string.
        ui::strings::set_locale(prefs.locale());
        let keymap = Keymap::with_overrides(prefs.keymap_overrides.clone());
        let presets = asset_store::presets::PresetStore::load(&AppPaths::presets_file(&paths));
        Editor {
            paths,
            prefs,
            keymap,
            recent,
            presets,
            dialogs,
            urls: Box::new(BrowserUrls),
            image_clipboard: Box::new(crate::clipboard::OsClipboard),
            import_jobs: Vec::new(),
            import_generation: 0,
            spawner: crate::jobs::run_inline,
            save_jobs: Vec::new(),
            export_jobs: Vec::new(),
            os_copy_fingerprint: None,
            os_image_probe: None,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            gpu_adapter_name: None,
            docs: Vec::new(),
            active: None,
            next_id: 1,
            untitled_count: 0,
            tool: ToolId::Move,
            brush: seeded_brush(ToolId::Move),
            brushes: BTreeMap::new(),
            foreground: DEFAULT_FOREGROUND,
            background: DEFAULT_BACKGROUND,
            content_color_backups: std::collections::HashMap::new(),
            doc_colors: std::collections::HashMap::new(),
            gradient_ramp: layer_model::Gradient::default(),
            active_pattern: None,
            panels_visible: true,
            screen_mode: ui::palette::ScreenMode::Standard,
            preferences_open: false,
            file_info_open: false,
            paint_channel: None,
            quick_mask: false,
            quick_mask_layer: None,
            recording: None,
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
            copied_style: None,
            edit_targets: crate::edit_target::EditTargets::default(),
            edit_sessions: crate::edit_session::EditSessions::default(),
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

    /// Swap the Help-menu browser launcher.
    ///
    /// The shipped editor never calls this; tests inject a recorder so the
    /// digest gate can reach the Help arms without opening browser tabs (C5).
    pub fn set_url_launcher(&mut self, urls: Box<dyn UrlLauncher>) {
        self.urls = urls;
    }

    /// The Help-menu browser launcher, for `menu_bridge::perform`.
    pub fn url_launcher_mut(&mut self) -> &mut dyn UrlLauncher {
        self.urls.as_mut()
    }

    /// Swap the OS image clipboard service (card 051). The shipped editor
    /// never calls this — it runs the OS clipboard — and tests inject
    /// [`crate::clipboard::FakeClipboard`] so paste behavior is deterministic.
    pub fn set_image_clipboard(&mut self, clipboard: Box<dyn crate::clipboard::ImageClipboard>) {
        self.image_clipboard = clipboard;
    }

    /// The OS image clipboard service, for the Copy/Paste routing (card 052).
    pub fn image_clipboard_mut(&mut self) -> &mut dyn crate::clipboard::ImageClipboard {
        self.image_clipboard.as_mut()
    }

    /// Whether an image paste would find anything on the OS clipboard — the
    /// menu-enablement probe (card 052). Throttled: see
    /// [`Editor::os_image_probe`] for why this must not read per frame.
    pub(crate) fn os_clipboard_has_image(&mut self) -> bool {
        const PROBE_TTL: std::time::Duration = std::time::Duration::from_millis(1000);
        if let Some((at, has)) = self.os_image_probe {
            if at.elapsed() < PROBE_TTL {
                return has;
            }
        }
        let has = self.image_clipboard.has_image();
        self.os_image_probe = Some((std::time::Instant::now(), has));
        has
    }

    /// Drops the cached OS-clipboard probe so the next menu context re-reads
    /// it. Called on window focus changes — the moment a payload copied in
    /// another application can have arrived.
    pub(crate) fn invalidate_os_image_probe(&mut self) {
        self.os_image_probe = None;
    }

    /// Records the fingerprint of an image Edit ▸ Copy just put on the OS
    /// clipboard (card 052's ownership policy).
    pub(crate) fn remember_os_copy(&mut self, image: &crate::clipboard::ClipboardImage) {
        self.os_copy_fingerprint = Some(fingerprint_image(image));
        // We JUST put an image there: the next menu context need not re-read.
        self.os_image_probe = Some((std::time::Instant::now(), true));
    }

    /// Whether an image read back from the OS clipboard is the editor's OWN
    /// last copy (same pixels we wrote) — the comparison that routes our copy
    /// through the internal store and anything else through placement.
    pub(crate) fn os_copy_is_ours(&self, image: &crate::clipboard::ClipboardImage) -> bool {
        self.os_copy_fingerprint == Some(fingerprint_image(image))
    }

    /// Paste an image that came from OUTSIDE the application (card 052): the
    /// OS clipboard's payload, routed through the full-source placement
    /// builder exactly like a placed file — nothing is resampled away, the
    /// image lands as a smart object over an embedded asset, ABOVE the active
    /// layer, and becomes the selection.
    ///
    /// The semantics are explicit: external pixels carry no in-document
    /// origin, so they paste **centered** through the same fit the Place
    /// menu item uses (`place_source_fit`, no upscale). The clipboard's
    /// pixels are untagged, so they are read as sRGB — the honest assumption
    /// for everything the OS clipboard carries — and the embedded asset's
    /// bytes are a PNG encoding of the same pixels, so Edit ▸ Contents can
    /// re-decode the source later.
    pub fn paste_external_image(
        &mut self,
        image: crate::clipboard::ClipboardImage,
    ) -> Result<String, String> {
        let name = "Pasted image".to_string();
        // Embedded bytes for the asset record: a PNG of the same pixels, so
        // the stored source is a re-decodable image rather than a bare buffer.
        let png = raster::encode(
            raster::ExportFormat::Png,
            image.width,
            image.height,
            &image.rgba,
        )
        .map_err(|e| e.to_string())?;
        let decoded = crate::import::DecodedImage {
            width: image.width,
            height: image.height,
            rgba8: image.rgba,
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
        };
        let asset = layer_model::AssetId::new();
        let (command, layer_id) = {
            let doc = self.active_mut().ok_or("No document is open")?;
            let canvas = raster::PixelRect::new(0, 0, doc.document.width(), doc.document.height());
            let working = doc.document.meta.color_space.clone();
            let mut placement = crate::placement::place_source_fit(
                &decoded,
                &name,
                canvas,
                &working,
                false,
                &mut doc.tiles,
            )
            .map_err(|e| e.to_string())?;
            placement = placement.with_layer_kind(layer_model::LayerKind::SmartObject(
                layer_model::SmartObjectLayer {
                    asset,
                    linked: false,
                },
            ));
            if let Some(active) = doc.document.active_layer() {
                let parent = doc.document.layers.parent_of(active);
                let index = doc.document.layers.index_in_parent(active);
                if let Some(index) = index {
                    placement.command = match placement.command {
                        Command::Transaction { label, commands } => Command::Transaction {
                            label,
                            commands: {
                                let mut c = commands;
                                c.push(Command::MoveLayer {
                                    layer_id: placement.layer,
                                    parent,
                                    index,
                                });
                                c
                            },
                        },
                        other => other,
                    };
                }
            }
            (placement.command, placement.layer)
        };
        // Same append-only registration policy as placement (card 048).
        if let Some(doc) = self.active_mut() {
            doc.document.set_asset_origin(layer_model::AssetRecord {
                id: asset,
                origin: layer_model::AssetOrigin::Embedded { name, bytes: png },
                source_size: Some((image.width, image.height)),
            });
        }
        self.apply_command(command);
        self.set_layer_selection(vec![layer_id], Some(layer_id));
        self.touch();
        self.status = Some(format!(
            "Pasted {}×{} from the clipboard",
            image.width, image.height
        ));
        Ok(format!(
            "Pasted {}×{} from the clipboard",
            image.width, image.height
        ))
    }

    /// The dialog-facing view of the application preferences.
    ///
    /// Every control the dialog draws maps onto a field this app persists and
    /// reads (`every_preference_control_has_a_consumer` holds that line):
    /// minutes instead of seconds for autosave, undo states for history
    /// depth, the scratch directory as text (empty for the default), and the
    /// live keymap as the keymap page's model. The page is General: Edit ▸
    /// Keyboard Shortcuts… opens on the Keymap page by overriding it where
    /// the menu action is answered (`DialogHost::open_for_menu_action`).
    pub fn ui_preferences(&self) -> ui::dialogs::UiPreferences {
        let prefs = self.preferences();
        ui::dialogs::UiPreferences {
            general: ui::dialogs::GeneralPrefs {
                autosave_minutes: (prefs.autosave_interval_secs / 60).min(u32::MAX as u64) as u32,
            },
            interface: ui::dialogs::InterfacePrefs {
                theme: prefs.theme.into(),
                ui_scale: prefs.ui_scale,
                language: prefs.locale(),
                units: prefs.units.into(),
            },
            tools: ui::dialogs::preferences::ToolPrefs {
                scroll_wheel_zooms: prefs.scroll_wheel_zooms,
            },
            history: ui::dialogs::HistoryPrefs {
                states: prefs.history_depth.min(u32::MAX as usize) as u32,
            },
            scratch: ui::dialogs::preferences::ScratchPrefs {
                dir: prefs
                    .scratch_dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            },
            keymap: self.keymap.editor_model(),
            page: ui::dialogs::PrefsSection::General,
        }
    }

    /// Apply the Preferences dialog's confirmed [`ui::dialogs::UiPreferences`].
    ///
    /// The keymap page's model is diffed against the shipped table and that
    /// diff becomes the live keymap's user layer ([`Keymap::apply_editor_model`]),
    /// so a chord bound on the page resolves the moment the dialog closes and
    /// is persisted with the rest. A model with no commands (a caller that did
    /// not build it from [`Self::ui_preferences`]) leaves the keymap alone
    /// rather than reading as "remove every customisation".
    pub fn apply_ui_preferences(&mut self, ui_prefs: &ui::dialogs::UiPreferences) {
        let mut prefs = self.preferences().clone();
        prefs.theme = ui_prefs.interface.theme.into();
        prefs.ui_scale = ui_prefs.interface.ui_scale;
        prefs.language = ui_prefs.interface.language.code().to_string();
        prefs.units = ui_prefs.interface.units.into();
        prefs.scroll_wheel_zooms = ui_prefs.tools.scroll_wheel_zooms;
        prefs.autosave_interval_secs = ui_prefs.general.autosave_minutes as u64 * 60;
        prefs.history_depth = ui_prefs.history.states.max(1) as usize;
        prefs.scratch_dir = ui_prefs.scratch.dir().map(std::path::PathBuf::from);
        if !ui_prefs.keymap.commands().is_empty() {
            self.keymap.apply_editor_model(&ui_prefs.keymap);
            self.pending_conflict = None;
        }
        prefs.keymap_overrides = self.keymap.overrides().to_vec();
        self.set_preferences(prefs);
    }

    /// The measurement unit the size readouts are written in.
    pub fn display_unit(&self) -> ui::dialogs::Unit {
        self.prefs.units.into()
    }

    /// Adopt a unit chosen outside the Preferences dialog — View ▸ Rulers ▸
    /// <unit> — as the Units preference, saved with the rest. The rulers, the
    /// status bar, the Info panel and the size dialogs all read this one
    /// setting, so the ruler menu cannot leave them disagreeing.
    pub fn set_display_unit(&mut self, unit: ui::dialogs::Unit) {
        let unit: crate::prefs::UnitChoice = unit.into();
        if self.prefs.units == unit {
            return;
        }
        let mut prefs = self.preferences().clone();
        prefs.units = unit;
        self.set_preferences(prefs);
    }

    /// A width × height in pixels, spelled in the Units preference —
    /// `2.540 × 2.540 cm`. Documents carry no resolution of their own, so the
    /// conversion is at the same 72 ppi the rulers fall back to.
    pub fn size_readout(&self, width_px: u32, height_px: u32) -> String {
        ui::dialogs::units::format_size(
            f64::from(width_px),
            f64::from(height_px),
            self.display_unit(),
            ui::dialogs::units::DEFAULT_PPI,
        )
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
        ui::strings::set_locale(prefs.locale());
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
        self.presets.save(&self.paths.presets_file())?;
        self.recent.save(&self.paths.recent_file())
    }

    /// The user preset store: named patterns and brushes.
    pub fn presets(&self) -> &asset_store::presets::PresetStore {
        &self.presets
    }

    /// Mutable access, for the menu items that define presets.
    pub fn presets_mut(&mut self) -> &mut asset_store::presets::PresetStore {
        &mut self.presets
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

    /// Start recording every applied command for the Actions panel.
    ///
    /// A recording already in progress is restarted: one recording at a time.
    pub fn start_recording(&mut self) {
        self.recording = Some(Vec::new());
    }

    /// Stop recording and hand back what was captured, in apply order.
    /// `None` when nothing was being recorded.
    pub fn stop_recording(&mut self) -> Option<Vec<RecordedEdit>> {
        self.recording.take()
    }

    /// Whether a recording is in progress (the panel's record button state).
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Replay a recording onto the ACTIVE document.
    ///
    /// Each captured command's layer target is remapped by stack position:
    /// the layer that sat at index `i` of the recording document maps to the
    /// layer at index `i` here — Actions semantics, where a replay means "do
    /// the same things to my layers", not "do them to a layer with the same
    /// random id". Commands with no layer target replay as-is; commands that
    /// name a position the replay document does not have are skipped and
    /// counted, so the caller can say what did not replay.
    pub fn replay(&mut self, edits: &[RecordedEdit]) -> usize {
        let mut applied = 0;
        for edit in edits {
            let Some(doc) = self.active_mut() else {
                return applied;
            };
            let stack = doc.document.layers.iter_depth_first();
            // Re-insert the captured bytes into THIS document's store: the
            // recording's hashes are keys into another document's blob store.
            let mut map = std::collections::BTreeMap::new();
            for (hash, bytes) in &edit.tiles {
                let fresh = doc.tiles.insert_bytes(bytes.clone());
                map.insert(format!("{hash:?}"), fresh);
            }
            let command = match edit.layer {
                Some(index) => match stack.get(index) {
                    Some(&target) => remap_layer(edit.command.clone(), target, &map),
                    None => continue, // the replay doc is shallower; skip
                },
                None => edit.command.clone(),
            };
            let before = doc.history_depth();
            let _ = doc.apply(command);
            if doc.history_depth() != before {
                applied += 1;
            }
        }
        self.touch();
        applied
    }

    /// The quick-mask session state: while on, pixel tools write to the
    /// scratch mask and the canvas may show the coverage as a red overlay.
    pub fn quick_mask(&self) -> bool {
        self.quick_mask
    }

    /// The scratch layer carrying the quick-mask coverage, if any.
    pub fn quick_mask_layer(&self) -> Option<LayerId> {
        self.quick_mask_layer
    }

    /// Toggle quick-mask mode (`MenuAction::ToggleQuickMask`, the `Q` chord).
    ///
    /// Entering mints a hidden scratch raster layer with an attached mask —
    /// hidden because its only job is to carry the painted coverage, and
    /// attaching the mask there means the tools' existing
    /// [`tools::PaintTarget::Mask`] route works unchanged. Leaving reads the
    /// painted coverage back as a [`editor_core::SelectionMask`] (mode
    /// machinery, not a user selection edit — the scratch coverage is not a
    /// marquee gesture, so it bypasses history the way card 056's exception
    /// describes) and deletes the scratch layer.
    /// Convert the document to a colour mode (Image ▸ Mode ▸ …).
    ///
    /// Tiles are always stored RGBA, so a conversion rewrites every layer's
    /// pixels — RGB→Grayscale collapses each pixel to its Rec.601 luma
    /// (r = g = b), Grayscale→RGB replicates the shared channel. The whole
    /// rewrite is ONE [`Command::Transaction`] (one [`Command::PaintTiles`]
    /// per pixel-bearing layer), so a single undo returns the colours; the
    /// mode itself rides on the metadata next to the rewrite (a field write,
    /// like the bit depth).
    pub fn set_color_mode(&mut self, mode: ui::menu::ColorMode) -> Result<String, String> {
        let target = mode as u8;
        let Some(doc) = self.active_mut() else {
            return Err("No document is open".into());
        };
        if doc.document.meta.color_mode == target {
            return Err("The document is already in that colour mode".into());
        }
        let from_rgb = doc.document.meta.color_mode == 0 && target == 1;
        let from_gray = doc.document.meta.color_mode == 1 && target == 0;
        if !(from_rgb || from_gray) {
            return Err("This build cannot convert between those colour modes".into());
        }
        let ts = TILE_SIZE as usize;
        let mut commands = vec![Command::SetMetaColorMode {
            from: doc.document.meta.color_mode,
            to: target,
        }];
        let layer_ids: Vec<LayerId> = doc.document.layers.iter_depth_first().to_vec();
        for layer_id in layer_ids {
            let Some(map) = doc.document.layer_tiles(layer_id) else {
                continue;
            };
            let mut edits = Vec::new();
            for (coord, hash) in map.iter() {
                if coord.level != 0 {
                    continue;
                }
                let Some(bytes) = doc.tiles.tile(hash) else {
                    continue;
                };
                if bytes.len() != ts * ts * 4 {
                    continue;
                }
                let converted: Vec<u8> = bytes
                    .chunks(4)
                    .flat_map(|px| {
                        if from_rgb {
                            let luma = (0.299 * px[0] as f32
                                + 0.587 * px[1] as f32
                                + 0.114 * px[2] as f32)
                                .round()
                                .clamp(0.0, 255.0) as u8;
                            [luma, luma, luma, px[3]]
                        } else {
                            // Grayscale tiles keep r = g = b; replicate any
                            // of them (the alpha survives untouched).
                            let g = px[0];
                            [g, g, g, px[3]]
                        }
                    })
                    .collect();
                let new_hash = doc.tiles.insert_bytes(converted);
                edits.push(editor_core::pixels::TileEdit::set(coord, new_hash));
            }
            if edits.is_empty() {
                continue;
            }
            let paint = Command::paint_tiles(PixelTarget::Layer(layer_id), edits)
                .map_err(|e: editor_core::CommandError| e.to_string())?;
            commands.push(paint);
        }
        doc.apply(Command::Transaction {
            label: format!("Change Colour Mode to {}", mode.label()),
            commands,
        })
        .map_err(|e| e.to_string())?;
        self.touch();
        Ok(format!("Changed colour mode to {}", mode.label()))
    }

    pub fn toggle_quick_mask(&mut self) -> Result<String, String> {
        if !self.quick_mask {
            let mask_id = MaskId::new();
            let mut layer = Layer::raster("Quick Mask");
            layer.visible = false;
            layer.set_mask(layer_model::LayerMask::new(mask_id));
            let layer_id = layer.id;
            self.apply_command(Command::create_layer(layer));
            self.quick_mask = true;
            self.quick_mask_layer = Some(layer_id);
            return Ok("Entered quick mask".into());
        }
        self.quick_mask = false;
        let Some(layer_id) = self.quick_mask_layer.take() else {
            return Ok("Left quick mask".into());
        };
        let doc = self.active_mut().ok_or("No document is open")?;
        // Read the painted coverage back: mask tiles are 8-bit samples, one
        // byte per pixel, keyed by the scratch mask.
        let (w, h) = (doc.document.width(), doc.document.height());
        let mut coverage = vec![0u8; (w as usize) * (h as usize)];
        let ts = TILE_SIZE as usize;
        if let Some(mask_id) = doc.document.layers.get(layer_id).and_then(|l| l.mask_id()) {
            if let Some(map) = doc
                .document
                .pixels
                .tiles(editor_core::PixelKey::Mask(mask_id))
            {
                for (coord, hash) in map.iter() {
                    if coord.level != 0 {
                        continue;
                    }
                    let Some(bytes) = doc.tiles.tile(hash) else {
                        continue;
                    };
                    let (ox, oy) = coord.pixel_origin();
                    for row in 0..ts {
                        let y = oy + row as i64;
                        if y < 0 || y >= h as i64 {
                            continue;
                        }
                        for col in 0..ts {
                            let x = ox + col as i64;
                            if x < 0 || x >= w as i64 {
                                continue;
                            }
                            let byte = bytes[row * ts + col];
                            if byte > 0 {
                                coverage[y as usize * w as usize + x as usize] = byte;
                            }
                        }
                    }
                }
            }
        }
        let selection = editor_core::SelectionMask::new(glam::IVec2::ZERO, w, h, coverage)
            .map(editor_core::Selection::Mask)
            .unwrap_or(editor_core::Selection::None);
        doc.document.selection = selection;
        let delete = Command::DeleteLayer { layer_id };
        let _ = doc.apply(delete);
        self.touch();
        Ok("Left quick mask".into())
    }

    /// The folder picker behind every export that writes more than one file
    /// (Export As, Export Layers). `None` means the user cancelled.
    pub fn pick_export_folder(&mut self) -> Option<PathBuf> {
        self.dialogs.pick_export_folder()
    }

    /// File ▸ Export Layers…: write each layer as its own PNG into a chosen
    /// directory. A layer is composited *alone* (every other layer hidden) over
    /// transparent, through the real compositor — the same isolation the merge
    /// path uses — so effects and blends are honoured per layer.
    ///
    /// W2-G: the per-layer composites and encodes run on a worker against a
    /// snapshot ([`crate::jobs::spawn_layer_export_with`]); the completion —
    /// the count written, or why not — reaches the status line through
    /// [`Editor::poll_exports`].
    pub fn export_layers(&mut self) -> Result<String, String> {
        let Some(dir) = self.pick_export_folder() else {
            return Err("Export Layers: no destination chosen".to_string());
        };
        let doc = self
            .active()
            .ok_or_else(|| "No document is open".to_string())?;
        let job = crate::jobs::LayerExportJob {
            id: doc.id(),
            document: doc.document.clone(),
            tiles: doc.tiles.clone(),
            dir: dir.clone(),
        };
        let rx = crate::jobs::spawn_layer_export_with(job, self.spawner);
        self.export_jobs.push(rx);
        self.status = Some(format!("Exporting layers to {}…", dir.display()));
        self.touch();
        // Inline spawner: already done. Threads: the frame loop polls.
        self.poll_exports();
        Ok("Exporting layers".to_string())
    }

    /// File ▸ Print…: render the active document's full composite to a
    /// print-ready single-page PDF, chosen through the export path dialog with
    /// a `.pdf` suggestion. This is the S1.8 Print route: the OS printing/spool
    /// surface is hereby reached as "Print as PDF" (a standard dialogless
    /// print destination) backed by a pure, tested PDF encoder.
    /// Help ▸ Export Diagnostics…: write a [`DiagnosticBundle`] — app version,
    /// OS, the GPU adapter the window actually got, and the panic/log lines —
    /// wherever the user picks. `upload_consented` is always `false`: an
    /// export is a file the user sends if and only if they choose to.
    pub fn export_diagnostics(&mut self) -> Result<String, String> {
        let suggested = self.paths.root().join("raster-studio-diagnostics.json");
        let Some(target) = self.dialogs.pick_save_path(&suggested) else {
            return Err("Export Diagnostics: no destination chosen".into());
        };
        let mut bundle = telemetry::DiagnosticBundle::new(&self.app_version);
        bundle.gpu_adapter = self
            .gpu_adapter_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let file = telemetry::panic_bundle(
            &self.paths.root().join("scratch"),
            &self.app_version,
            "exported diagnostics",
        );
        // The scratch bundle carries whatever a previous panic recorded; fold
        // its log lines into the export so a crash report travels with the
        // diagnostics the user chose to send.
        if let Ok(prior) = std::fs::read_to_string(&file) {
            if let Ok(parsed) = serde_json::from_str::<telemetry::DiagnosticBundle>(&prior) {
                bundle.recent_log_lines.extend(parsed.recent_log_lines);
            }
        }
        std::fs::write(&target, bundle.to_json()).map_err(|e| e.to_string())?;
        self.status = Some(format!("Diagnostics written to {}", target.display()));
        Ok("Diagnostics exported".into())
    }

    /// The GPU adapter the window actually got — set once at window creation,
    /// straight from the live wgpu adapter.
    pub fn set_gpu_adapter_name(&mut self, name: String) {
        self.gpu_adapter_name = Some(name);
    }

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
    /// File ▸ Place Embedded… / Place Linked…: ask for an image, then place it.
    /// The dialog answer is `None` when the user cancelled, which is a
    /// cancelled action, not a failure.
    pub fn place_from_dialog(&mut self, linked: bool) -> Result<String, String> {
        let Some(path) = self.dialogs.pick_place_file() else {
            return Err("Place was cancelled".to_string());
        };
        self.place_path(&path, linked)
    }

    /// Place an image file into the active document as a smart object whose
    /// tiles hold the FULL decoded source (fit as a layer transform,
    /// color-converted into the working space) — nothing is clipped away.
    ///
    /// An embedded place carries the file's bytes in the document's asset
    /// table; a linked one carries the path, and [`Editor::refresh_linked_sources`]
    /// re-reads it when the file changes. The pixels land as one undoable
    /// Transaction, so undo removes the placed object entirely.
    pub fn place_path(&mut self, path: &Path, linked: bool) -> Result<String, String> {
        // Cards 046-048: decode and read the source bytes BEFORE anything
        // mutates - a failed decode or read changes neither the layer stack
        // nor dirty/history state.
        let image = crate::import::DecodedImage::decode_path(path).map_err(|e| e.to_string())?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Placed".to_string());
        let asset = layer_model::AssetId::new();
        let origin = if linked {
            layer_model::AssetOrigin::Linked {
                path: path.to_path_buf(),
            }
        } else {
            layer_model::AssetOrigin::Embedded {
                name: name.clone(),
                bytes: std::fs::read(path).map_err(|e| e.to_string())?,
            }
        };

        // The builder places the FULL source - fit as a layer transform,
        // color-converted into the document's working space - instead of
        // clipping to the canvas. Nothing decoded is lost: off-canvas and
        // transparent-margin content stays in the stored tiles.
        let (command, layer_id) = {
            let doc = self.active_mut().ok_or("No document is open")?;
            let canvas = raster::PixelRect::new(0, 0, doc.document.width(), doc.document.height());
            let working = doc.document.meta.color_space.clone();
            let mut placement = crate::placement::place_source_fit(
                &image,
                &name,
                canvas,
                &working,
                false,
                &mut doc.tiles,
            )
            .map_err(|e| e.to_string())?;
            // The placed layer is a smart object over the registered asset,
            // keeping the builder's layer id.
            placement = placement.with_layer_kind(layer_model::LayerKind::SmartObject(
                layer_model::SmartObjectLayer { asset, linked },
            ));
            // Insert ABOVE the active layer: same parent, its index. A
            // create lands at the root top, so the move makes the placement
            // a sibling exactly where the user is looking.
            if let Some(active) = doc.document.active_layer() {
                let parent = doc.document.layers.parent_of(active);
                let index = doc.document.layers.index_in_parent(active);
                if let Some(index) = index {
                    placement.command = match placement.command {
                        Command::Transaction { label, commands } => Command::Transaction {
                            label,
                            commands: {
                                let mut c = commands;
                                c.push(Command::MoveLayer {
                                    layer_id: placement.layer,
                                    parent,
                                    index,
                                });
                                c
                            },
                        },
                        other => other,
                    };
                }
            }
            (placement.command, placement.layer)
        };
        // The asset table is a field, like the selection: registration is
        // not an undoable edit, and the pixels that use it are. STORAGE
        // POLICY (card 048): the table is append-only for the session - an
        // undone placement leaves its record and embedded blob in place
        // (unreachable, immutable, cheap to keep) rather than risking a live
        // reference to a removed asset.
        if let Some(doc) = self.active_mut() {
            doc.document.set_asset_origin(layer_model::AssetRecord {
                id: asset,
                origin,
                source_size: Some((image.width, image.height)),
            });
            if let Ok(stamp) = std::fs::metadata(path).and_then(|m| m.modified()) {
                doc.asset_stamps.insert(asset, stamp);
            }
        }
        self.apply_command(command);
        // The placed layer becomes the selection: the next gesture aims at
        // what the user just placed.
        self.set_layer_selection(vec![layer_id], Some(layer_id));
        self.touch();
        self.status = Some(format!(
            "Placed {}{}",
            name,
            if linked { " (linked)" } else { "" }
        ));
        Ok(format!("Placed {name}"))
    }

    /// Re-read every linked asset source whose file changed on disk, replacing
    /// the smart object layers' pixels as one undoable step per changed
    /// source. Sibling layers sharing an asset (duplicate, copy-paste) move
    /// together in that one step - the stamp is written only after every
    /// sibling has been refreshed. Embedded sources are the user's own bytes
    /// and never re-read.
    pub fn refresh_linked_sources(&mut self) -> Result<String, String> {
        let mut changed = 0usize;
        let mut failures: Vec<String> = Vec::new();
        // Pass 1 (read-only): every smart-object layer, in depth-first order.
        let ids: Vec<(DocumentId, LayerId, layer_model::AssetId)> = {
            let mut out = Vec::new();
            for open in &self.docs {
                for id in open.document.layers.iter_depth_first() {
                    if let Some(layer_model::LayerKind::SmartObject(so)) =
                        open.document.layers.get(id).map(|l| &l.kind)
                    {
                        out.push((open.id(), id, so.asset));
                    }
                }
            }
            out
        };
        // Pass 2: group siblings per (document, asset). Siblings share one
        // linked source and one asset record, so the decode, the stamp and
        // the recorded source size are per group, not per layer.
        let mut order: Vec<(DocumentId, layer_model::AssetId)> = Vec::new();
        let mut grouped: HashMap<(DocumentId, layer_model::AssetId), Vec<LayerId>> = HashMap::new();
        for (doc_id, layer_id, asset) in ids {
            if !grouped.contains_key(&(doc_id, asset)) {
                order.push((doc_id, asset));
            }
            grouped.entry((doc_id, asset)).or_default().push(layer_id);
        }
        for (doc_id, asset) in order {
            let Some(open) = self.docs.iter_mut().find(|d| d.id() == doc_id) else {
                continue;
            };
            let name_for = |open: &OpenDocument, id: LayerId| {
                open.document
                    .layers
                    .get(id)
                    .map(|l| l.name.clone())
                    .unwrap_or_default()
            };
            let layers = grouped.remove(&(doc_id, asset)).unwrap_or_default();
            let Some(layer_model::AssetOrigin::Linked { path }) =
                open.document.asset_origin(asset).cloned()
            else {
                continue;
            };
            let current = match std::fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(stamp) => stamp,
                Err(_) => {
                    // A missing source: keep the last good cached appearance
                    // and say so - never replace it with empty pixels. Every
                    // sibling holding the asset reports.
                    for layer_id in &layers {
                        failures.push(format!("{}: source is missing", name_for(open, *layer_id)));
                    }
                    continue;
                }
            };
            if open.asset_stamps.get(&asset) == Some(&current) {
                continue;
            }
            // Card 050: the replacement IS the builder - the same shared
            // conversion into the working space (an unsupported profile is
            // an error, not a silent recolor) and the same slicing, so the
            // refresh cannot drift from placement. The layers' masks,
            // effects and identities are untouched: only tiles + transforms
            // move, as one undoable transaction. The decode happens once per
            // source; the sibling layers reuse it.
            let image = match crate::import::DecodedImage::decode_path(&path) {
                Ok(image) => image,
                Err(e) => {
                    for layer_id in &layers {
                        failures.push(format!("{}: {e}", name_for(open, *layer_id)));
                    }
                    continue;
                }
            };
            let converted = match crate::placement::working_space_pixels(
                &image,
                &open.document.meta.color_space,
            ) {
                Ok(converted) => converted,
                Err(e) => {
                    for layer_id in &layers {
                        failures.push(format!("{}: {e}", name_for(open, *layer_id)));
                    }
                    continue;
                }
            };
            let new_tiles: Vec<(raster::TileCoord, Vec<u8>)> =
                crate::placement::slice_source_tiles(&converted, glam::IVec2::ZERO);
            // The renormalization: the ratio is OLD source dims / NEW source
            // dims (both known - the record stores what was placed),
            // conjugated through each layer's own transform so the on-canvas
            // footprint stays identical across a resolution change. Without a
            // recorded size (a pre-048 document) the transform is left alone
            // - no jump, ever.
            let old_size = open.document.asset_source_size(asset);
            let resolution_changed = old_size
                .map(|(w, h)| w != image.width || h != image.height)
                .unwrap_or(false);
            let mut commands: Vec<Command> = Vec::new();
            let mut refreshed_any = false;
            for layer_id in &layers {
                let mut edits = Vec::new();
                let mut identical = !resolution_changed;
                for (coord, bytes) in &new_tiles {
                    let hash = open.tiles.insert_bytes(bytes.clone());
                    // A no-op check: the stored map must hold the very same
                    // tile at the very same coordinate. Only then can a
                    // refresh whose file content did not actually change skip
                    // its undo entry (the stamps are session-only, so the
                    // first refresh after a reopen re-decodes and lands here).
                    match open
                        .document
                        .layer_tiles(*layer_id)
                        .and_then(|m| m.get(*coord))
                    {
                        Some(old) if old == hash => {}
                        _ => identical = false,
                    }
                    edits.push(editor_core::pixels::TileEdit::set(*coord, hash));
                }
                // Ghost cleanup: the old coords the new source does not
                // actually store (a tile transparent in the new source is
                // absent from it - the old ink there must go). A leftover
                // ghost also breaks the no-op check.
                if let Some(old_map) = open.document.layer_tiles(*layer_id) {
                    for (c, _) in old_map.iter() {
                        if !new_tiles.iter().any(|(coord, _)| *coord == c) {
                            edits.push(editor_core::pixels::TileEdit::clear(c));
                            identical = false;
                        }
                    }
                }
                if identical {
                    continue;
                }
                refreshed_any = true;
                commands.push(
                    Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(*layer_id), edits)
                        .map_err(|e| e.to_string())?,
                );
                if let Some((old_w, old_h)) = old_size {
                    let own = open
                        .document
                        .layers
                        .get(*layer_id)
                        .map(|l| l.transform)
                        .unwrap_or(glam::Affine2::IDENTITY);
                    if image.width > 0 && image.height > 0 {
                        let ratio = glam::Affine2::from_scale(glam::Vec2::new(
                            old_w as f32 / image.width as f32,
                            old_h as f32 / image.height as f32,
                        ));
                        let matrix = (own * ratio * own.inverse()).to_cols_array();
                        commands.push(Command::TransformLayer {
                            layer_id: *layer_id,
                            matrix,
                        });
                    }
                }
            }
            if !refreshed_any {
                // The file's stamp moved but the decoded content (and the
                // resolution) is what is already stored: nothing to undo,
                // nothing to report - just learn the new stamp.
                open.asset_stamps.insert(asset, current);
                continue;
            }
            // The recorded size becomes the new source's INSIDE the same
            // transaction (card 050 review): undo then reverts the appearance
            // and the anchor together, so the next refresh renormalizes
            // against a size the reverted tiles actually match.
            commands.push(Command::SetAssetSourceSize {
                asset,
                size: Some((image.width, image.height)),
            });
            let first = name_for(open, layers[0]);
            let label = if layers.len() == 1 {
                format!("Refresh {first}")
            } else {
                format!("Refresh {first} +{} sibling(s)", layers.len() - 1)
            };
            open.apply(Command::Transaction { label, commands })
                .map_err(|e| e.to_string())?;
            open.asset_stamps.insert(asset, current);
            changed += 1;
        }
        if changed == 0 && failures.is_empty() {
            return Ok("No linked source has changed".to_string());
        }
        if !failures.is_empty() {
            self.status = Some(format!(
                "Refresh problems ({}): {}",
                failures.len(),
                failures.join("; ")
            ));
        }
        self.touch();
        if failures.is_empty() {
            Ok(format!("Updated {changed} linked source(s)"))
        } else {
            Ok(format!(
                "Updated {changed}; {} skipped (see status)",
                failures.len()
            ))
        }
    }

    /// Layer ▸ Smart Object ▸ Replace Contents… (card 069): swap the active
    /// smart object's source file out from under it without redoing the
    /// layout — the layer keeps its id, name, transform base, mask, effects
    /// and group position; only the stored tiles and the asset row move, in
    /// one undoable [`Command::Transaction`].
    ///
    /// # Shared-assets policy (the explicit card-069 decision)
    ///
    /// Replacement updates the SHARED asset: every smart object layer in the
    /// document that references the same [`layer_model::AssetId`] — including
    /// duplicates and copy-pasted layers, which share the id — updates
    /// together in the same transaction. That is the smart-object semantic
    /// (a placed file is a reference to one source, not to one layer), and it
    /// means no sibling can be left half-updated. There is no per-instance
    /// variant: card 069 ships the shared policy only. Layers that do NOT
    /// reference this asset are untouched — identity is never inferred from
    /// layer names, only from the asset id.
    ///
    /// # Origin-kind policy
    ///
    /// An EMBEDDED source stays embedded (the new origin carries the new
    /// file's name and bytes); a LINKED source stays linked (the new origin
    /// carries the new path). The kind never flips behind the user's back.
    ///
    /// # Sizing (the card-050 renormalization)
    ///
    /// The new source is fit to the EXISTING source frame with aspect
    /// preserved: each sibling's transform is conjugated through its own
    /// matrix (`own · old/new · own⁻¹`), so the on-canvas footprint is
    /// identical across a resolution change. When the asset row has no
    /// recorded `source_size` (a pre-069 record), the transform is left
    /// alone — no jump, ever. The file bytes are read ONCE, up front; the
    /// decode converts into the document's working space and REFUSES an
    /// unsupported profile loudly (card 050's contract — no silent sRGB
    /// fallback), before anything mutates.
    pub fn replace_smart_object_contents(&mut self, path: &Path) -> Result<String, String> {
        // Read the bytes once, before anything mutates: a failed read or
        // decode changes neither the layer stack nor history.
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let open = self
            .active_mut()
            .ok_or_else(|| "No document is open".to_string())?;
        // The ACTIVE layer must be a smart object: unlike Edit Contents
        // (which falls back to the first smart object in the tree), Replace
        // targets exactly what the user is looking at.
        let (layer_id, asset) = {
            let active = open
                .document
                .active_layer()
                .ok_or_else(|| "Select a smart object layer first".to_string())?;
            match open.document.layers.get(active).map(|l| &l.kind) {
                Some(LayerKind::SmartObject(so)) => (active, so.asset),
                Some(other) => {
                    let class = editor_core::command::layer_class_name(other);
                    return Err(format!("The active layer is a {class}, not a smart object"));
                }
                None => return Err("Select a smart object layer first".to_string()),
            }
        };
        let _ = layer_id;
        let old_origin =
            open.document.asset_origin(asset).cloned().ok_or_else(|| {
                "The smart object's asset is missing from the document".to_string()
            })?;
        // Origin-kind policy: embedded stays embedded, linked stays linked.
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Replaced".to_string());
        let (origin, linked) = match &old_origin {
            layer_model::AssetOrigin::Embedded { .. } => (
                layer_model::AssetOrigin::Embedded {
                    name: file_name.clone(),
                    bytes: bytes.clone(),
                },
                false,
            ),
            layer_model::AssetOrigin::Linked { .. } => (
                layer_model::AssetOrigin::Linked {
                    path: path.to_path_buf(),
                },
                true,
            ),
        };
        // Decode: from the captured bytes for an embedded source, from the
        // file for a linked one — both through the raster codec.
        let image = if linked {
            let decoded = raster::decode_surface_path(path, raster::ImportLimits::default())
                .map_err(|e| e.to_string())?
                .into_decoded_image();
            crate::import::DecodedImage {
                width: decoded.width,
                height: decoded.height,
                rgba8: decoded.rgba8,
                color_space: decoded.color_space,
                icc_profile: decoded.icc_profile,
            }
        } else {
            let decoded = raster::decode_surface_bytes(&bytes, raster::ImportLimits::default())
                .map_err(|e| e.to_string())?
                .into_decoded_image();
            crate::import::DecodedImage {
                width: decoded.width,
                height: decoded.height,
                rgba8: decoded.rgba8,
                color_space: decoded.color_space,
                icc_profile: decoded.icc_profile,
            }
        };
        let converted =
            crate::placement::working_space_pixels(&image, &open.document.meta.color_space)
                .map_err(|e| e.to_string())?;
        let new_tiles: Vec<(raster::TileCoord, Vec<u8>)> =
            crate::placement::slice_source_tiles(&converted, glam::IVec2::ZERO);
        let new_size = (image.width, image.height);
        // Every sibling layer referencing the asset, depth-first like the
        // refresh's pass 2 grouping.
        let layers: Vec<layer_model::LayerId> = open
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| {
                matches!(
                    open.document.layers.get(*id).map(|l| &l.kind),
                    Some(LayerKind::SmartObject(so)) if so.asset == asset
                )
            })
            .collect();
        let old_size = open.document.asset_source_size(asset);
        let mut commands: Vec<Command> = Vec::new();
        for layer_id in &layers {
            let mut edits = Vec::new();
            for (coord, bytes) in &new_tiles {
                let hash = open.tiles.insert_bytes(bytes.clone());
                edits.push(editor_core::pixels::TileEdit::set(*coord, hash));
            }
            // Ghost cleanup: the old coords the new source does not actually
            // store (transparent in it) must lose their old ink.
            if let Some(old_map) = open.document.layer_tiles(*layer_id) {
                for (c, _) in old_map.iter() {
                    if !new_tiles.iter().any(|(coord, _)| *coord == c) {
                        edits.push(editor_core::pixels::TileEdit::clear(c));
                    }
                }
            }
            commands.push(
                Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(*layer_id), edits)
                    .map_err(|e| e.to_string())?,
            );
            // The renormalization: ONLY when the row records a source size
            // (a pre-069 record keeps its transform — no jump ever).
            if let Some((old_w, old_h)) = old_size {
                let own = open
                    .document
                    .layers
                    .get(*layer_id)
                    .map(|l| l.transform)
                    .unwrap_or(glam::Affine2::IDENTITY);
                if new_size.0 > 0 && new_size.1 > 0 {
                    let ratio = glam::Affine2::from_scale(glam::Vec2::new(
                        old_w as f32 / new_size.0 as f32,
                        old_h as f32 / new_size.1 as f32,
                    ));
                    let matrix = (own * ratio * own.inverse()).to_cols_array();
                    commands.push(Command::TransformLayer {
                        layer_id: *layer_id,
                        matrix,
                    });
                }
            }
        }
        // The asset row swaps INSIDE the same transaction (card 050's rule
        // for the recorded size): undo then reverts the appearance AND the
        // metadata together.
        commands.push(Command::ReplaceAssetSource {
            asset,
            origin,
            source_size: Some(new_size),
        });
        let label = if layers.len() == 1 {
            format!("Replace Contents of {file_name}")
        } else {
            format!(
                "Replace Contents of {file_name} (+{} sibling(s))",
                layers.len() - 1
            )
        };
        open.apply(Command::Transaction { label, commands })
            .map_err(|e| e.to_string())?;
        // Stamp handling, mirroring the refresh: a linked source learns the
        // new file's stamp so a later refresh does not immediately re-decode
        // the file we just stored; an embedded source has no file to watch.
        if linked {
            if let Ok(stamp) = std::fs::metadata(path).and_then(|m| m.modified()) {
                open.asset_stamps.insert(asset, stamp);
            }
        } else {
            open.asset_stamps.remove(&asset);
        }
        self.touch();
        self.status = Some(format!("Replaced contents with {file_name}"));
        Ok(format!("Replaced contents with {file_name}"))
    }

    /// Layer ▸ Smart Object ▸ Replace Contents…: ask for an image, then swap
    /// it in. The dialog answer is `None` when the user cancelled, which is a
    /// cancelled action, not a failure.
    pub fn replace_from_dialog(&mut self) -> Result<String, String> {
        let Some(path) = self.dialogs.pick_replace_file() else {
            return Err("Replace was cancelled".to_string());
        };
        self.replace_smart_object_contents(&path)
    }

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
        let rgba_png_bytes =
            raster::encode(raster::ExportFormat::Png, w, h, &rgba).map_err(|e| e.to_string())?;
        let style_name = name.clone();
        let new_id_asset = layer_model::AssetId::new();
        let command = {
            let doc = self.active_mut().ok_or("No document is open")?;
            let layer = layer_model::Layer::with_kind(
                name,
                layer_model::LayerKind::SmartObject(layer_model::SmartObjectLayer {
                    asset: new_id_asset,
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
        // Card 071 repair: a CONVERTED smart object needs an asset row —
        // without one, card 069's Replace Contents (and any asset-table
        // consumer) cannot reach it. Same STORAGE POLICY as placement
        // (card 048): append-only for the session, registered outside the
        // command stream. The row's source size is the canvas the
        // conversion rasterized; the embedded bytes are the same PNG the
        // staged composite encoded, so Edit-contents round trips.
        if let Some(doc) = self.active_mut() {
            doc.document.set_asset_origin(layer_model::AssetRecord {
                id: new_id_asset,
                origin: layer_model::AssetOrigin::Embedded {
                    name: style_name,
                    bytes: rgba_png_bytes,
                },
                source_size: Some((w, h)),
            });
        }
        Ok("Converted layer to a smart object".to_string())
    }

    /// File ▸ Duplicate…: open a copy of the current document's state as a new
    /// document (same pixels and layer tree, fresh undo history, `copy` title).
    /// File ▸ Close All: close every open document, answering the unsaved-
    /// changes prompt once per document (walking from the back so indexes stay
    /// valid). Backing out of one prompt cancels the whole close-all.
    /// Close every document except the active one, asking about unsaved work
    /// first. Stops at the first cancellation, leaving the rest open.
    pub fn close_other_documents(&mut self) -> Result<String, String> {
        let Some(active) = self.active else {
            return Err("No document is open".to_string());
        };
        let keep = self.docs[active].id();
        for index in (0..self.docs.len()).rev() {
            if self.docs[index].id() == keep {
                continue;
            }
            self.close_document(index).map_err(|e| match e {
                ActionError::Cancelled(_) => "Close Others cancelled".to_string(),
                ActionError::Unavailable { reason, .. } => reason,
                ActionError::Failed { reason, .. } => reason,
            })?;
        }
        Ok("Closed the other documents".to_string())
    }

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
        self.status = Some(format!(
            "Canvas resized to {}",
            self.size_readout(new_w, new_h)
        ));
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

    /// Image ▸ Image Rotation ▸ 90°: rotate the whole canvas and every layer's
    /// pixels by a quarter turn, as one undoable step.
    pub fn rotate_canvas_90(&mut self, clockwise: bool) -> Result<String, String> {
        let command = {
            let doc = self
                .active_mut()
                .ok_or_else(|| "No document is open".to_string())?;
            doc.rotate_canvas_90(clockwise).map_err(|e| e.to_string())?
        };
        self.apply_command(command);
        Ok("Rotated canvas 90°".to_string())
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
        let (parent, layer_id, name, w, h) = {
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
            // Card 050: the tab is the SOURCE frame - the stored tiles'
            // extent - never the parent canvas, so editing cannot reduce the
            // source to the canvas dimensions.
            let (mut mx, mut my) = (0i32, 0i32);
            let map = open
                .document
                .layer_tiles(layer)
                .ok_or_else(|| "The smart object has no stored pixels".to_string())?;
            for (c, _) in map.iter() {
                mx = mx.max(c.x);
                my = my.max(c.y);
            }
            (
                open.id(),
                layer,
                name,
                (mx + 1) as u32 * raster::TILE_SIZE,
                (my + 1) as u32 * raster::TILE_SIZE,
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
        // Seed the tab with the object's FULL stored tiles (content-addressed
        // bytes re-inserted into the tab's own store), so the tab's raster IS
        // the smart object's source from frame one - at source resolution.
        let seed = {
            // Gather (coord, bytes) from the parent first; the tab's store
            // is separate, so the bytes re-insert there.
            let pairs: Vec<(raster::TileCoord, Vec<u8>)> = {
                let parent = self
                    .docs
                    .iter()
                    .find(|d| d.id() == parent)
                    .ok_or("Parent document missing")?;
                let map = parent
                    .document
                    .layer_tiles(layer_id)
                    .ok_or("The smart object has no stored pixels")?;
                map.iter()
                    .map(|(coord, hash)| {
                        parent
                            .tiles
                            .tile(hash)
                            .map(|b| b.to_vec())
                            .map(|bytes| (coord, bytes))
                            .ok_or_else(|| {
                                "A smart object tile is missing from the store".to_string()
                            })
                    })
                    .collect::<Result<Vec<_>, String>>()?
            };
            let doc = self.active_mut().ok_or("Contents document missing")?;
            let target_layer = doc
                .document
                .layers
                .iter_depth_first()
                .first()
                .copied()
                .ok_or_else(|| "Contents document has no layer".to_string())?;
            let mut edits = Vec::new();
            for (coord, bytes) in pairs {
                let h = doc.tiles.insert_bytes(bytes);
                edits.push(editor_core::pixels::TileEdit::set(coord, h));
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

        // Card 050: write the tab's pixels back at SOURCE resolution - the
        // tab IS the source frame - replacing the object's stored tiles
        // wholesale (a coordinate the tab cleared is cleared on the parent
        // too, so erasures propagate). One undo step.
        let layer = embedded.layer;
        let command = {
            // Card 050: the tab must still exist — a silent empty read here
            // would erase the object under a success message. A missing tab
            // or layer is refused loudly; a legitimately EMPTY map (the user
            // erased everything) commits as a wholesale clear.
            let tab = self
                .docs
                .iter()
                .find(|d| d.id() == embedded.contents)
                .ok_or_else(|| "The contents document is gone".to_string())?;
            let contents_layer = tab
                .document
                .layers
                .iter_depth_first()
                .first()
                .copied()
                .ok_or_else(|| "The contents document has no layer".to_string())?;
            // Gather (coord, bytes) from the tab first: its store is
            // separate from the parent's, and the borrow must end before the
            // parent is taken mutably.
            let pairs: Vec<(raster::TileCoord, Vec<u8>)> = tab
                .document
                .layer_tiles(contents_layer)
                .map(|m| {
                    m.iter()
                        .map(|(coord, hash)| {
                            tab.tiles
                                .tile(hash)
                                .map(|b| b.to_vec())
                                .map(|bytes| (coord, bytes))
                                .ok_or_else(|| {
                                    "A contents tile is missing from the store".to_string()
                                })
                        })
                        .collect::<Result<Vec<_>, String>>()
                })
                .unwrap_or_else(|| Ok(Vec::new()))?;
            let parent_idx = self
                .docs
                .iter()
                .position(|d| d.id() == embedded.parent)
                .ok_or_else(|| "The parent document is gone".to_string())?;
            let parent = self
                .docs
                .get_mut(parent_idx)
                .ok_or_else(|| "The parent document is gone".to_string())?;
            let mut edits = Vec::new();
            let mut new_coords = std::collections::BTreeSet::new();
            for (coord, bytes) in pairs {
                new_coords.insert(coord);
                let hash = parent.tiles.insert_bytes(bytes);
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            // Ghost cleanup: the object's old coords the tab no longer holds
            // (an erasure in the tab propagates as a clear).
            if let Some(old_map) = parent.document.layer_tiles(layer) {
                for (c, _) in old_map.iter() {
                    if !new_coords.contains(&c) {
                        edits.push(editor_core::pixels::TileEdit::clear(c));
                    }
                }
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

    /// Layer ▸ New Fill Layer ▸ Pattern: add a raster layer tiled with the
    /// most recently defined user pattern. Like the Solid Color layer, the
    /// pattern is *baked* into the layer's pixels at creation rather than a
    /// live generator — a one-step undoable fill, honest about being a raster.
    pub fn new_pattern_fill_layer(&mut self) -> Result<String, String> {
        let (w, h) = self
            .active()
            .map(|d| (d.document.width(), d.document.height()))
            .ok_or_else(|| "No document is open".to_string())?;
        let pattern = self
            .presets
            .latest_pattern()
            .ok_or_else(|| "No pattern is defined yet; use Define Pattern first".to_string())?
            .clone();
        let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..i64::from(h) {
            for x in 0..i64::from(w) {
                pixels.extend_from_slice(&pattern.pixel(x, y));
            }
        }
        let command = {
            let doc = self.active_mut().ok_or("No document is open")?;
            let layer = layer_model::Layer::raster("Pattern Fill");
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
        Ok(format!(
            "Added pattern fill layer tiled with “{}”",
            pattern.name
        ))
    }

    /// Edit ▸ Define Pattern: snapshot the active layer's pixels inside the
    /// selection (the whole layer with no selection) as a named pattern.
    ///
    /// Photoshop defines from the merged composite; this build's compositing
    /// is tiled and streaming, so the definition reads the *active layer* and
    /// the status line says so. The name is counter-suffixed because the menu
    /// item opens no dialog to ask; re-defining the same name updates it.
    pub fn define_pattern_from_selection(&mut self) -> Result<String, String> {
        let layer = self
            .active()
            .and_then(|d| d.document.active_layer())
            .ok_or_else(|| "No document is open".to_string())?;
        let (w, h) = self
            .active()
            .map(|d| (d.document.width(), d.document.height()))
            .ok_or_else(|| "No document is open".to_string())?;
        let (rect, has_selection) = {
            let doc = self.active().ok_or("No document is open")?;
            let sel = &doc.document.selection;
            match sel.bounds() {
                Some((lo, hi)) => (
                    (
                        lo.x.max(0) as u32,
                        lo.y.max(0) as u32,
                        (hi.x as u32).min(w),
                        (hi.y as u32).min(h),
                    ),
                    true,
                ),
                None => ((0, 0, w, h), false),
            }
        };
        let (x0, y0, x1, y1) = rect;
        if x1 <= x0 || y1 <= y0 {
            return Err("The selection is empty".to_string());
        }
        let before = {
            let doc = self.active().ok_or("No document is open")?;
            crate::menu_bridge::pixels::read_layer(doc, layer)
        };
        let pw = (x1 - x0) as usize;
        let ph = (y1 - y0) as usize;
        let mut rgba8 = Vec::with_capacity(pw * ph * 4);
        for y in y0 as usize..y1 as usize {
            for x in x0 as usize..x1 as usize {
                let i = (y * w as usize + x) * 4;
                rgba8.extend_from_slice(&before[i..i + 4]);
            }
        }
        let n = self.presets.patterns().len() + 1;
        let name = format!("Pattern {n}");
        self.presets
            .define_pattern(asset_store::presets::PatternPreset {
                name: name.clone(),
                width: pw as u32,
                height: ph as u32,
                rgba8,
            });
        // The pattern just defined is the one the pattern tools paint with,
        // as in Photoshop: defining selects.
        self.active_pattern = Some(name.clone());
        Ok(format!(
            "Defined pattern “{name}” from {} {} — the active layer's pixels{}",
            pw,
            ph,
            if has_selection {
                " inside the selection"
            } else {
                ""
            }
        ))
    }

    /// Edit ▸ Define Brush Preset: store the active tool's brush settings
    /// under a counter-suffixed name (the menu item opens no dialog).
    /// Card 067: capture the ACTIVE layer's style block (style fields only —
    /// the `LayerEffects` struct cannot hold position, masks, text, or asset
    /// identity by construction).
    pub fn copy_layer_style(&mut self) -> Result<String, String> {
        let layer = self
            .active()
            .and_then(|d| d.document.active_layer())
            .ok_or_else(|| "No document is open".to_string())?;
        let effects = {
            let doc = self.active().ok_or("No document is open")?;
            doc.document.layers.get(layer).unwrap().effects.clone()
        };
        self.copied_style = Some(effects);
        Ok("Copied the layer style".to_string())
    }

    /// Card 067: paste the copied style onto the ACTIVE layer — ONE
    /// undoable step (the wholesale `LayerPatch::effects` replace; its
    /// inverse restores the previous style block). Only style fields move.
    pub fn paste_layer_style(&mut self) -> Result<String, String> {
        let effects = self
            .copied_style
            .clone()
            .ok_or_else(|| "No layer style has been copied".to_string())?;
        let layer = self
            .active()
            .and_then(|d| d.document.active_layer())
            .ok_or_else(|| "No document is open".to_string())?;
        self.active_mut()
            .ok_or_else(|| "No document is open".to_string())?
            .apply(Command::SetLayerProperties {
                layer_id: layer,
                patch: editor_core::LayerPatch {
                    effects: Some(Box::new(effects)),
                    ..Default::default()
                },
            })
            .map_err(|e| e.to_string())?;
        Ok("Pasted the layer style".to_string())
    }

    /// Card 067: store the ACTIVE layer's style as the next named preset —
    /// a full snapshot of the style block, persisted with the preset store
    /// (survives restart via `persist`).
    pub fn define_style_preset(&mut self) -> Result<String, String> {
        let layer = self
            .active()
            .and_then(|d| d.document.active_layer())
            .ok_or_else(|| "No document is open".to_string())?;
        let effects = {
            let doc = self.active().ok_or("No document is open")?;
            doc.document.layers.get(layer).unwrap().effects.clone()
        };
        if effects.is_default() {
            return Err("The layer has no style to define".to_string());
        }
        let name = format!("Style {}", self.presets.styles().len() + 1);
        let json = serde_json::to_string(&effects).map_err(|e| e.to_string())?;
        self.presets.define_style(&name, json);
        Ok(format!("Defined style preset \"{name}\""))
    }

    /// Card 067: apply the most recently defined style preset to the ACTIVE
    /// layer — ONE undoable step (the same wholesale replace as a paste).
    pub fn apply_latest_style_preset(&mut self) -> Result<String, String> {
        let (name, json) = self
            .presets
            .latest_style()
            .cloned()
            .ok_or_else(|| "No style preset has been defined".to_string())?;
        let effects: layer_model::LayerEffects =
            serde_json::from_str(&json).map_err(|e| e.to_string())?;
        let layer = self
            .active()
            .and_then(|d| d.document.active_layer())
            .ok_or_else(|| "No document is open".to_string())?;
        self.active_mut()
            .ok_or_else(|| "No document is open".to_string())?
            .apply(Command::SetLayerProperties {
                layer_id: layer,
                patch: editor_core::LayerPatch {
                    effects: Some(Box::new(effects)),
                    ..Default::default()
                },
            })
            .map_err(|e| e.to_string())?;
        Ok(format!("Applied style preset \"{name}\""))
    }

    /// Card 067: the captured style block, if any (the Paste gate reads it).
    pub fn copied_style(&self) -> Option<&layer_model::LayerEffects> {
        self.copied_style.as_ref()
    }

    pub fn define_brush_preset(&mut self) -> Result<String, String> {
        let tool = self.effective_tool();
        let settings = self.brush_for(tool);
        let json = serde_json::to_string(&settings).map_err(|e| e.to_string())?;
        let n = self.presets.brushes().len() + 1;
        let name = format!("Brush {n}");
        self.presets.define_brush(&name, json);
        Ok(format!("Defined brush preset “{name}”"))
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
    /// Card 039: drop every preview lens on non-active documents — a tab
    /// switch must not leave a stale transform preview behind on the tab
    /// the user left.
    pub fn clear_stale_previews(&mut self) {
        let active = self.active().map(|d| d.id());
        for doc in self.docs.iter_mut() {
            if Some(doc.id()) != active && doc.has_preview() {
                doc.clear_preview();
            }
        }
    }

    pub fn documents_mut(&mut self) -> &mut [OpenDocument] {
        &mut self.docs
    }

    /// Run a command emitted by a panel through the active document's history.
    ///
    /// This is how the UI edits: it emits intent, the editor applies it, undo
    /// and redo stay uniform. A refusal is reported rather than swallowed.
    pub fn apply_command(&mut self, command: Command) {
        // The Actions panel records here: `apply_command` is the single choke
        // point every edit flows through, so a recording captures exactly
        // what the user did, in order — with the stack position of the layer
        // each command targets, so a replay can retarget it.
        if self.recording.is_some() {
            let captured = self
                .active()
                .map(|d| {
                    (
                        layer_position(&d.document.layers, &command),
                        captured_tiles(d, &command),
                    )
                })
                .unwrap_or((None, Vec::new()));
            if let Some(recording) = &mut self.recording {
                recording.push(RecordedEdit {
                    command: command.clone(),
                    layer: captured.0,
                    tiles: captured.1,
                });
            }
        }
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

    /// Photopea's multi-selection: the whole set lands in the document, the
    /// active layer moves only when the click named one. Not a command and not
    /// history: selecting layers is not an undoable edit — the DELETE that
    /// consumes the set is the undo step.
    pub fn set_layer_selection(&mut self, layers: Vec<LayerId>, active: Option<LayerId>) {
        let Some(doc) = self.active_mut() else {
            return;
        };
        if let Err(e) = doc.document.set_layer_selection(layers) {
            tracing::debug!("cannot select those layers: {e}");
            return;
        }
        if let Some(layer) = active {
            if let Err(e) = doc.document.set_active_layer(Some(layer)) {
                tracing::debug!("cannot select that layer: {e}");
                return;
            }
        }
        self.touch();
    }

    /// The validated edit target for the active document: which half of the
    /// active layer — content or mask coverage — edits aim at (card 007).
    ///
    /// The stored kind is sticky per document; validity is computed here, so a
    /// mask removed while selected (undo, delete), a tab switch, or an
    /// active-layer change all fall back to content without any repair pass.
    pub fn edit_target(&self) -> Option<crate::edit_target::EditTarget> {
        let doc = self.active()?;
        let kind = self.edit_targets.kind_of(doc.id());
        crate::edit_target::resolve_active(doc, kind)
    }

    /// The sticky kind stored for one document, validated or not.
    pub fn edit_target_kind(&self, id: DocumentId) -> crate::edit_target::EditTargetKind {
        self.edit_targets.kind_of(id)
    }

    /// Whether pixel edits currently aim at the active layer's MASK (card
    /// 055): the validated read-time answer both tool routes share. A target
    /// whose mask has since been removed resolves to content here, exactly
    /// as it does everywhere else the target is read.
    pub fn edit_target_is_mask(&self) -> bool {
        matches!(
            self.edit_target(),
            Some(crate::edit_target::EditTarget {
                kind: crate::edit_target::EditTargetKind::Mask,
                ..
            })
        )
    }

    /// Aim the active document's edits at its content or mask coverage. A
    /// preference, not a pixel edit: nothing here (and nothing in selection)
    /// may mark the document dirty.
    ///
    /// Card 058: switching to the mask swaps the colour wells to the mask
    /// editing pair (white foreground reveals, black conceals) WITHOUT
    /// losing the user's content colours — they are stashed and restored
    /// when the target switches back. A no-op switch (same kind) touches
    /// nothing, so re-aiming at the current target never resets the wells.
    pub fn set_edit_target_kind(&mut self, kind: crate::edit_target::EditTargetKind) {
        let Some(doc) = self.active() else {
            return;
        };
        if self.edit_targets.kind_of(doc.id()) == kind {
            return;
        }
        let doc_id = doc.id();
        self.edit_targets.set_kind(doc_id, kind);
        // The CURRENT wells for this document (per-doc entry, or the global
        // wells a fresh document inherits).
        let current = self
            .doc_colors
            .get(&doc_id)
            .copied()
            .unwrap_or((self.foreground, self.background));
        match kind {
            crate::edit_target::EditTargetKind::Mask => {
                self.content_color_backups.insert(doc_id, current);
                self.doc_colors
                    .insert(doc_id, (MASK_EDIT_FOREGROUND, MASK_EDIT_BACKGROUND));
            }
            crate::edit_target::EditTargetKind::Content => {
                if let Some((fg, bg)) = self.content_color_backups.remove(&doc_id) {
                    self.doc_colors.insert(doc_id, (fg, bg));
                }
            }
        }
        self.touch();
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

    /// Reorder the document tabs: move the tab at `from` to `to`, shifting
    /// the ones between. The active tab follows its document.
    ///
    /// Returns `false` for a no-op (same index, or either end out of range)
    /// rather than panicking: a stale drag's destination is user data.
    pub fn move_document(&mut self, from: usize, to: usize) -> bool {
        if from >= self.docs.len() || to >= self.docs.len() || from == to {
            return false;
        }
        let doc = self.docs.remove(from);
        self.docs.insert(to, doc);
        // The active tab follows its document; the tabs between shift by one.
        self.active = match self.active {
            Some(a) if a == from => Some(to),
            Some(a) if from < a && a <= to => Some(a - 1),
            Some(a) if to <= a && a < from => Some(a + 1),
            other => other,
        };
        self.touch();
        true
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

    /// The colour wells. Card 058: per-DOCUMENT while a document is active
    /// (each document's target switch manages its own entry), falling back
    /// to these fields when no document is open — so tabbing between a
    /// mask-targeted document and a content document never mixes their
    /// colour state.
    pub fn foreground(&self) -> [f32; 4] {
        if let Some(doc) = self.active() {
            if let Some((fg, _)) = self.doc_colors.get(&doc.id()) {
                return *fg;
            }
        }
        self.foreground
    }

    pub fn background(&self) -> [f32; 4] {
        if let Some(doc) = self.active() {
            if let Some((_, bg)) = self.doc_colors.get(&doc.id()) {
                return *bg;
            }
        }
        self.background
    }

    pub fn set_foreground(&mut self, rgba: [f32; 4]) {
        match self.active() {
            Some(doc) => {
                let id = doc.id();
                let bg = self
                    .doc_colors
                    .get(&id)
                    .map(|(_, bg)| *bg)
                    .unwrap_or(self.background);
                self.doc_colors.insert(id, (rgba, bg));
            }
            None => self.foreground = rgba,
        }
        self.touch();
    }

    /// Replace the ramp the gradient tools paint with — the read-back of the
    /// options bar's and the gradient dialog's edits.
    pub fn set_gradient_ramp(&mut self, gradient: layer_model::Gradient) {
        self.gradient_ramp = gradient;
        self.touch();
    }

    /// The ramp the gradient tools paint with.
    pub fn gradient_ramp(&self) -> &layer_model::Gradient {
        &self.gradient_ramp
    }

    /// W1-C: the pattern preset the pattern-driven tools paint with — the
    /// one [`Self::set_active_pattern`] chose if it still exists, otherwise
    /// the most recently defined one. `None` until a pattern is defined.
    pub fn active_pattern(&self) -> Option<&asset_store::presets::PatternPreset> {
        self.active_pattern
            .as_deref()
            .and_then(|name| self.presets.pattern(name))
            .or_else(|| self.presets.latest_pattern())
    }

    /// W1-C: choose the pattern the pattern-driven tools paint with, by
    /// preset name. Refused (and left unchanged) when no preset has that
    /// name, so a stale picker entry cannot silently fall back.
    pub fn set_active_pattern(&mut self, name: &str) -> Result<(), String> {
        if self.presets.pattern(name).is_none() {
            return Err(format!("No pattern named “{name}” is defined"));
        }
        self.active_pattern = Some(name.to_string());
        self.touch();
        Ok(())
    }

    /// W1-C: the active pattern as the tools crate's [`tools::Pattern`]
    /// (straight-alpha sRGB8, the same encoding the preset stores), for
    /// [`crate::tool_input::ToolPointer`] to hand to every tool context.
    /// `None` when no pattern is defined or the stored preset is malformed
    /// (a zero side or a byte count that does not match its size).
    pub fn active_tool_pattern(&self) -> Option<tools::Pattern> {
        let preset = self.active_pattern()?;
        tools::Pattern::new(preset.width, preset.height, preset.rgba8.clone()).ok()
    }

    pub fn set_background(&mut self, rgba: [f32; 4]) {
        match self.active() {
            Some(doc) => {
                let id = doc.id();
                let fg = self
                    .doc_colors
                    .get(&id)
                    .map(|(fg, _)| *fg)
                    .unwrap_or(self.foreground);
                self.doc_colors.insert(id, (fg, rgba));
            }
            None => self.background = rgba,
        }
        self.touch();
    }

    pub fn panels_visible(&self) -> bool {
        self.panels_visible
    }

    /// W2-X: which of Photopea's three screen modes the window is in.
    pub fn screen_mode(&self) -> ui::palette::ScreenMode {
        self.screen_mode
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
            Self::absorb_journal_hold(path);
            OpenDocument::open_project(id, path, depth)?
        } else {
            OpenDocument::open_image(id, path, depth)?
        };
        self.docs.push(doc);
        self.active = Some(self.docs.len() - 1);
        self.recent.record(path);
        let _ = self.recent.save(&self.paths.recent_file());
        self.status = Some(format!("Opened {}", path.display()));
        // Card 077's fidelity report is shown on the DIALOG-initiated route
        // (see `apply_import`): a programmatic/CLI open must never block on a
        // modal notice. A fully supported file has nothing to say either way.
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
                // Waited for, not merely started: the tab is about to go, and
                // a save that fails after it has gone would take the work
                // with it. Closing is the one moment blocking is the right
                // answer.
                CloseChoice::Save => self.save_document_and_wait(index, false)?,
                CloseChoice::Discard => {}
            }
        }
        if let Some(doc) = self.docs.get_mut(index) {
            let id = doc.id();
            // Discarded while a save of it still runs: the commands held
            // aside are the ones the user just chose to lose.
            Self::settle_journal_hold(doc.end_journal_hold(), None);
            self.discard_autosave(id);
            // Card 050: closing either half of an embedded-contents session
            // ends the session - a dangling one would let a later commit
            // erase the object (the commit refuses loudly when the tab is
            // gone; this removes the reason it ever gets there).
            if let Some(embedded) = &self.embedded {
                if embedded.contents == id || embedded.parent == id {
                    self.embedded = None;
                }
            }
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
    ///
    /// W2-G: the write runs on a job thread against a snapshot of the
    /// document (see [`crate::jobs`]). This picks the target (the picker is a
    /// native dialog and has to stay on this thread), starts the job, and
    /// returns; [`Editor::poll_saves`] applies the completion — path
    /// adoption, the dirty flag, the recent list, the status line — back on
    /// this thread. A second save of a document whose save is still in flight
    /// is refused with a reason, which the shell puts on the status line.
    fn save_document(&mut self, index: usize, force_dialog: bool) -> Result<(), ActionError> {
        let action = if force_dialog {
            Action::SaveAs
        } else {
            Action::Save
        };
        let Some(doc) = self.docs.get(index) else {
            return Err(ActionError::unavailable(action, "no document is open"));
        };
        let id = doc.id();
        let title = doc.title().to_string();
        let running = self.save_jobs.iter().find(|p| p.id == id).map(|p| p.kind);
        match running {
            // A save the user asked for is already writing this document:
            // there is nothing a second one would add.
            Some(crate::jobs::SaveKind::Save) => {
                return Err(ActionError::unavailable(
                    action,
                    format!("{title} is already being saved"),
                ));
            }
            // The timer's autosave is writing it: the user's save is not
            // dropped behind it, it is queued to run the moment it lands
            // (with whatever the document has become by then).
            Some(crate::jobs::SaveKind::Autosave { .. }) => {
                let target = self.choose_save_target(index, force_dialog, action)?;
                if let Some(pending) = self.save_jobs.iter_mut().find(|p| p.id == id) {
                    pending.follow_up = Some(target.clone());
                }
                self.status = Some(format!(
                    "Saving {title} to {} once the autosave finishes…",
                    target.display()
                ));
                self.touch();
                return Ok(());
            }
            None => {}
        }
        let target = self.choose_save_target(index, force_dialog, action)?;
        self.start_save(index, crate::jobs::SaveKind::Save, target);
        // Under the inline spawner the job has already completed; apply it
        // now so the caller sees the saved state. Under worker threads this
        // finds nothing yet and the frame loop picks it up.
        self.poll_saves();
        Ok(())
    }

    /// Where a save of the document at `index` goes: its own package, or —
    /// for Save As and a document that has none — wherever the picker says.
    fn choose_save_target(
        &mut self,
        index: usize,
        force_dialog: bool,
        action: Action,
    ) -> Result<PathBuf, ActionError> {
        let doc = &self.docs[index];
        let existing = doc.project_path().map(Path::to_path_buf);
        match (force_dialog, existing) {
            (false, Some(path)) => Ok(path),
            _ => {
                let suggested = doc.suggested_save_path();
                match self.dialogs.pick_save_path(&suggested) {
                    Some(p) => Ok(p),
                    None => Err(ActionError::Cancelled(action)),
                }
            }
        }
    }

    /// [`Editor::save_document`], then block until that save has landed.
    ///
    /// For the two moments a save must be *done* rather than started: closing
    /// the tab and quitting. A save already in flight for the document is
    /// waited for first; if the document is still dirty afterwards (edited
    /// while it ran) a fresh one is started and waited for too.
    fn save_document_and_wait(
        &mut self,
        index: usize,
        force_dialog: bool,
    ) -> Result<(), ActionError> {
        let action = if force_dialog {
            Action::SaveAs
        } else {
            Action::Save
        };
        let Some(doc) = self.docs.get(index) else {
            return Err(ActionError::unavailable(action, "no document is open"));
        };
        let id = doc.id();
        // A loop, not one wait: an autosave that lands may start the save the
        // user queued behind it, and that one has to land too.
        while self.save_in_flight(id) {
            self.wait_for_save(id)
                .map_err(|e| ActionError::failed(action, e))?;
        }
        let doc = &self.docs[index];
        if !force_dialog && !doc.is_dirty() && doc.project_path().is_some() {
            return Ok(());
        }
        self.save_document(index, force_dialog)?;
        while self.save_in_flight(id) {
            self.wait_for_save(id)
                .map_err(|e| ActionError::failed(action, e))?;
        }
        Ok(())
    }

    /// Snapshot the document at `index` and hand it to a worker.
    ///
    /// From this moment until the completion is applied, the document's
    /// commands are journaled to a side file ([`OpenDocument::begin_journal_hold`])
    /// rather than to the package journal the worker is about to copy and
    /// swap out from under them; [`Editor::finish_save`] absorbs the side
    /// file into whichever journal the document has once the save has landed.
    /// A document with a package holds next to it, where the next open after
    /// a crash finds it ([`journal_hold_path`]); one without holds in the
    /// scratch directory.
    fn start_save(&mut self, index: usize, kind: crate::jobs::SaveKind, target: PathBuf) {
        let side = match self.docs[index].project_path() {
            Some(project) => journal_hold_path(project),
            None => {
                let scratch = self.prefs.scratch_dir(&self.paths);
                if let Err(e) = std::fs::create_dir_all(&scratch) {
                    tracing::warn!("cannot create the scratch directory: {e}");
                }
                scratch.join(format!(
                    "hold-{}-{}.journal",
                    self.session_tag,
                    self.docs[index].id().0
                ))
            }
        };
        self.docs[index].begin_journal_hold(side);
        let doc = &self.docs[index];
        let progress = project_format::SaveProgress::new();
        let job = crate::jobs::SaveJob {
            id: doc.id(),
            kind,
            target: target.clone(),
            // The snapshot: a content-addressed document plus its tile map.
            // The worker owns these; the user may keep editing the live ones.
            document: doc.document.clone(),
            tiles: doc.tiles.clone(),
            app_version: self.app_version.clone(),
            progress: progress.clone(),
        };
        let title = doc.title().to_string();
        let id = doc.id();
        let rx = crate::jobs::spawn_save_with(job, self.spawner);
        let shown = save_status_text(&title, kind, 0, 0);
        self.status = Some(shown.clone());
        self.save_jobs.push(PendingSave {
            id,
            kind,
            target,
            title,
            progress,
            rx,
            shown: Some(shown),
            follow_up: None,
        });
        self.touch();
    }

    /// W2-G: the progress record of the save of `id` in flight, if any — what
    /// the status line reads once a frame.
    pub fn save_progress_of(
        &self,
        id: DocumentId,
    ) -> Option<std::sync::Arc<project_format::SaveProgress>> {
        self.save_jobs
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.progress.clone())
    }

    /// Start the save the user queued behind an autosave, now that it has
    /// landed. The document may have been closed meanwhile; then there is
    /// nothing to save.
    fn start_follow_up(&mut self, id: DocumentId, target: Option<PathBuf>) {
        let Some(target) = target else { return };
        if let Some(index) = self.docs.iter().position(|d| d.id() == id) {
            self.start_save(index, crate::jobs::SaveKind::Save, target);
        }
    }

    /// `true` while a save of the document `id` is running.
    pub fn save_in_flight(&self, id: DocumentId) -> bool {
        self.save_jobs.iter().any(|p| p.id == id)
    }

    /// `true` while any save (Ctrl+S, Save As, autosave) is running.
    pub fn saves_pending(&self) -> bool {
        !self.save_jobs.is_empty()
    }

    /// `true` while any job — import, save or export — is in flight, so the
    /// frame loop knows to keep polling.
    pub fn jobs_pending(&self) -> bool {
        self.imports_pending() || self.saves_pending() || !self.export_jobs.is_empty()
    }

    /// Apply every finished job of every kind. Once a frame.
    pub fn poll_jobs(&mut self) {
        self.poll_imports();
        self.poll_saves();
        self.poll_exports();
    }

    /// W2-G: apply every save that has finished, and refresh the status line
    /// for the ones still running.
    pub fn poll_saves(&mut self) {
        let mut report = AutosaveReport::default();
        self.poll_saves_into(&mut report);
    }

    fn poll_saves_into(&mut self, report: &mut AutosaveReport) {
        if self.save_jobs.is_empty() {
            return;
        }
        let jobs = std::mem::take(&mut self.save_jobs);
        for mut pending in jobs {
            match pending.rx.try_recv() {
                Ok(outcome) => {
                    self.finish_save(outcome, report);
                    self.start_follow_up(pending.id, pending.follow_up.take());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still running: show how far it has got, when that moved
                    // — but only while the status line still shows this
                    // save's own text. A message something else put there
                    // since ("already being saved", a tool's) stays; the
                    // completion says its piece when the save lands.
                    let text = save_status_text(
                        &pending.title,
                        pending.kind,
                        pending.progress.tiles_done(),
                        pending.progress.tiles_total(),
                    );
                    if pending.shown.as_deref() != Some(text.as_str()) {
                        if self.status == pending.shown {
                            self.status = Some(text.clone());
                            self.touch();
                        }
                        pending.shown = Some(text);
                    }
                    self.save_jobs.push(pending);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // The worker died without reporting — a panic in the
                    // writer. The previous package is intact (nothing is
                    // swapped in until the whole package is built), so the
                    // only loss is this attempt, and it is said.
                    let outcome = crate::jobs::SaveOutcome {
                        id: pending.id,
                        kind: pending.kind,
                        target: pending.target,
                        result: Err("the save worker stopped without reporting".to_string()),
                    };
                    self.finish_save(outcome, report);
                    self.start_follow_up(pending.id, pending.follow_up.take());
                }
            }
        }
    }

    /// Block until the save of `id` has landed, then apply it (and start the
    /// save queued behind it, if it was an autosave with one).
    ///
    /// Only a failed *Save* is an error here: a failed autosave costs the
    /// safety net, not the save the caller is about to wait for.
    fn wait_for_save(&mut self, id: DocumentId) -> Result<(), String> {
        let Some(at) = self.save_jobs.iter().position(|p| p.id == id) else {
            return Ok(());
        };
        let mut pending = self.save_jobs.remove(at);
        let kind = pending.kind;
        let follow_up = pending.follow_up.take();
        let outcome = pending.rx.recv().unwrap_or(crate::jobs::SaveOutcome {
            id: pending.id,
            kind: pending.kind,
            target: pending.target,
            result: Err("the save worker stopped without reporting".to_string()),
        });
        let mut report = AutosaveReport::default();
        self.finish_save(outcome, &mut report);
        self.start_follow_up(id, follow_up);
        match (kind, report.failed.into_iter().next()) {
            (crate::jobs::SaveKind::Save, Some((_, reason))) => Err(reason),
            _ => Ok(()),
        }
    }

    /// W2-G: the journal hold of a save that has finished. Its records are
    /// commands accepted after the snapshot, so they go after the marker of
    /// whichever package the document now has: the one just written, or —
    /// when the save failed — the one it still had. A document with no
    /// package journals nothing, and the side file simply goes.
    fn settle_journal_hold(side: Option<PathBuf>, package: Option<&Path>) {
        let Some(side) = side else { return };
        match package {
            Some(package) => {
                if let Err(e) = project_format::CommandJournal::absorb(
                    &side,
                    &package.join(project_format::JOURNAL_FILE),
                ) {
                    tracing::warn!(
                        "cannot move the commands journaled during the save into {}: {e}",
                        package.display()
                    );
                }
            }
            None => {
                if let Err(e) = std::fs::remove_file(&side) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!("cannot clear the journal hold {}: {e}", side.display());
                    }
                }
            }
        }
    }

    /// W2-G: a crash while a save of `project` ran on a worker leaves the
    /// commands accepted meanwhile in the hold next to it; move them into the
    /// package's journal — after its marker, whichever save won — before the
    /// journal is read. Once per open, before [`session::recoverable`] or
    /// [`OpenDocument::open_project`], and only for a real package (nothing
    /// to absorb into otherwise).
    fn absorb_journal_hold(project: &Path) {
        if !project.join(project_format::MANIFEST_FILE).is_file() {
            return;
        }
        Self::settle_journal_hold(Some(journal_hold_path(project)), Some(project));
    }

    /// Block until every save in flight has landed. For shutdown: a process
    /// must not exit with a package half-built on a worker.
    pub fn wait_for_saves(&mut self) {
        while let Some(pending) = self.save_jobs.first() {
            let id = pending.id;
            let _ = self.wait_for_save(id);
        }
    }

    /// What a landed save does to the live state — on this thread, the only
    /// one that may touch it.
    fn finish_save(&mut self, outcome: crate::jobs::SaveOutcome, report: &mut AutosaveReport) {
        let crate::jobs::SaveOutcome {
            id,
            kind,
            target,
            result,
        } = outcome;
        // The hold is over either way; where its records go depends on how
        // the save ended (see below).
        let held = self
            .docs
            .iter_mut()
            .find(|d| d.id() == id)
            .and_then(OpenDocument::end_journal_hold);
        match result {
            Ok(saved) => {
                match kind {
                    crate::jobs::SaveKind::Save => {
                        let mut edited_meanwhile = false;
                        if let Some(doc) = self.docs.iter_mut().find(|d| d.id() == id) {
                            // Clean only if the live document is still the
                            // snapshot that was written. An edit made while
                            // the worker ran leaves it dirty: the package
                            // does not hold that edit yet.
                            let unchanged = doc.document_digest() == Some(saved.document);
                            edited_meanwhile = !unchanged;
                            doc.adopt_saved(&target, unchanged);
                        }
                        // The work now lives somewhere the user chose, so the
                        // safety net goes.
                        self.discard_autosave(id);
                        self.recent.record(&target);
                        let _ = self.recent.save(&self.paths.recent_file());
                        self.status = Some(if edited_meanwhile {
                            format!(
                                "Saved {} (edits made during the save are not in it yet)",
                                target.display()
                            )
                        } else {
                            format!("Saved {}", target.display())
                        });
                    }
                    crate::jobs::SaveKind::Autosave { scratch } => {
                        if scratch {
                            self.autosaves.insert(id, target.clone());
                        } else {
                            // A document that gained a package since the last
                            // pass has just been written there; its scratch
                            // copy is now a stale duplicate.
                            self.discard_autosave(id);
                        }
                        tracing::info!("autosaved {}", target.display());
                        // Autosave says nothing on success unless it was
                        // showing its progress: then the line is put back to
                        // something that is not a stale "Autosaving…".
                        if self
                            .status
                            .as_deref()
                            .is_some_and(|s| s.starts_with("Autosaving"))
                        {
                            self.status = Some(format!("Autosaved {}", target.display()));
                        }
                        report.written.push((id, target.clone()));
                    }
                }
                // The package at `target` ends with the marker the worker
                // wrote for this snapshot; the commands accepted while it
                // ran belong after that marker, and this is what makes
                // crash recovery replay exactly them — not the ones the
                // snapshot already holds, and not fewer.
                Self::settle_journal_hold(held, Some(&target));
                self.touch();
            }
            Err(reason) => {
                // The previous package is exactly as it was, and so is its
                // marker: the held commands go after it.
                let package = self
                    .docs
                    .iter()
                    .find(|d| d.id() == id)
                    .and_then(|d| d.project_path().map(Path::to_path_buf));
                Self::settle_journal_hold(held, package.as_deref());
                let what = match kind {
                    crate::jobs::SaveKind::Save => "Save",
                    crate::jobs::SaveKind::Autosave { .. } => "Autosave",
                };
                tracing::warn!("{what} of {} failed: {reason}", target.display());
                self.status = Some(format!("{what} failed: {reason}"));
                report.failed.push((id, reason));
                self.touch();
            }
        }
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
    ///
    /// W2-G: each write is a job on a worker, the same route Ctrl+S takes
    /// ([`Editor::start_save`]); this pass *starts* one per dirty document
    /// and the completions are applied by [`Editor::poll_saves`]. A document
    /// whose save is already in flight is skipped — the next pass catches
    /// whatever it was edited into.
    pub fn autosave_now(&mut self) -> AutosaveReport {
        let mut report = AutosaveReport::default();
        let scratch = self.prefs.scratch_dir(&self.paths);
        let tag = self.session_tag.clone();
        // A document that was recovered from a previous run's autosave keeps
        // writing to that same package rather than starting a second one.
        let existing = self.autosaves.clone();

        let mut to_start: Vec<(usize, crate::jobs::SaveKind, PathBuf)> = Vec::new();
        for (index, doc) in self.docs.iter().enumerate() {
            if !doc.is_dirty() || self.save_in_flight(doc.id()) {
                continue;
            }
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
            to_start.push((
                index,
                crate::jobs::SaveKind::Autosave {
                    scratch: is_scratch,
                },
                target,
            ));
        }
        for (index, kind, target) in to_start {
            report.started.push((self.docs[index].id(), target.clone()));
            self.start_save(index, kind, target);
        }
        // Under the inline spawner every job above has already completed;
        // collect them so the report says what was written. Under worker
        // threads this finds nothing yet.
        self.poll_saves_into(&mut report);
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
            // A crash during a save leaves the commands accepted meanwhile
            // in the hold next to the package; they are part of what is
            // recoverable, so they go into the journal before it is read.
            Self::absorb_journal_hold(project);
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
            | Action::CycleScreenMode
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
            | Action::CloseOthers
            | Action::ZoomIn
            | Action::ZoomOut
            | Action::ZoomFit
            | Action::ZoomActualPixels
            | Action::TemporaryHand
            // Copy reads the canvas; Cut and Paste edit it. All three need a
            // document — there is no canvas to copy from without one.
            | Action::Copy
            | Action::Cut
            | Action::Paste
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
            Action::CloseOthers => {
                self.close_other_documents()
                    .map_err(|reason| ActionError::Failed {
                        action: Action::CloseOthers,
                        reason,
                    })?;
                Ok(Effect::DocumentSet)
            }
            Action::Quit => self.act_quit(),
            // Card 052: the keyboard route reaches the same menu bridge the
            // Edit menu drives, so the freshness policy (external payload
            // first, internal fallback) is one policy no matter how the user
            // asked. A refusal is a status line, not an error dialog.
            Action::Copy => match crate::menu_bridge::perform(ui::menu::MenuAction::Copy, self) {
                Ok(_) => Ok(Effect::View),
                Err(e) => {
                    self.status = Some(e);
                    Ok(Effect::View)
                }
            },
            Action::Cut => match crate::menu_bridge::perform(ui::menu::MenuAction::Cut, self) {
                Ok(_) => Ok(Effect::DocumentEdited),
                Err(e) => {
                    self.status = Some(e);
                    Ok(Effect::View)
                }
            },
            Action::Paste => match crate::menu_bridge::perform(ui::menu::MenuAction::Paste, self) {
                Ok(_) => Ok(Effect::DocumentEdited),
                Err(e) => {
                    self.status = Some(e);
                    Ok(Effect::View)
                }
            },
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
            Action::CycleScreenMode => {
                self.screen_mode = self.screen_mode.next();
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
        let (w, h) = NEW_DOCUMENT_SIZE;
        self.new_document_with(w, h, &title, crate::import::BlankBackground::Transparent)
    }

    /// Create a document at the given size with the given background — the
    /// New Document dialog's confirmed answer.
    ///
    /// The title is used as-is (the dialog refuses an empty one); the
    /// background is the document's *initial* state, not an undo step.
    pub fn new_document_with(
        &mut self,
        width: u32,
        height: u32,
        title: &str,
        background: crate::import::BlankBackground,
    ) -> Result<Effect, ActionError> {
        let id = self.mint_id();
        let doc = OpenDocument::blank_with_background(
            id,
            width,
            height,
            title,
            self.prefs.history_depth,
            background,
        )
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
        // Card 087: the read and decode run off the interaction thread; the
        // document appears (or the failure is reported) when the shell polls
        // the finished job. The picker itself stays on this thread — native
        // file dialogs must be.
        self.request_open(&path);
        Ok(Effect::DocumentSet)
    }

    /// Card 087: start reading and decoding `path` on a worker thread.
    ///
    /// The interaction thread stays responsive while the disk and the codecs
    /// work; [`Self::poll_imports`] applies the finished job.
    pub fn request_open(&mut self, path: &Path) {
        let rx = crate::jobs::spawn_import_with(
            path.to_path_buf(),
            self.import_generation,
            self.prefs.history_depth,
            self.spawner,
        );
        self.import_jobs.push(rx);
        self.status = Some(format!("Opening {}…", path.display()));
        self.touch();
    }

    /// `true` while at least one import job is in flight.
    pub fn imports_pending(&self) -> bool {
        !self.import_jobs.is_empty()
    }

    /// Card 087: cancel every pending import. Completions from the cancelled
    /// generation are dropped unread at poll time — a finished job can never
    /// apply after the user took the cancellation.
    pub fn cancel_pending_imports(&mut self) {
        self.import_generation += 1;
        self.import_jobs.clear();
        self.status = Some("Cancelled pending imports".to_string());
        self.touch();
    }

    /// Card 087: apply whatever import jobs finished, in completion order.
    ///
    /// A stale completion (cancelled generation) is dropped unread; a failure
    /// is reported through the same dialog route [`Self::open_paths`] uses.
    /// The document a completed job becomes is minted fresh here, on this
    /// thread — a completed import can never mutate the wrong document.
    pub fn poll_imports(&mut self) {
        if self.import_jobs.is_empty() {
            return;
        }
        let generation = self.import_generation;
        let jobs = std::mem::take(&mut self.import_jobs);
        self.import_jobs = Vec::new();
        for rx in jobs {
            loop {
                match rx.try_recv() {
                    Ok(outcome) => {
                        if outcome.is_stale(generation) {
                            continue;
                        }
                        self.apply_import(outcome);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        self.import_jobs.push(rx);
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
    }

    /// Build and open the document a finished import job delivered.
    fn apply_import(&mut self, outcome: crate::jobs::ImportOutcome) {
        let depth = self.prefs.history_depth;
        let path = outcome.path().clone();
        match outcome {
            crate::jobs::ImportOutcome::Image { decoded, .. } => match decoded {
                Ok(image) => {
                    let id = self.mint_id();
                    match OpenDocument::open_image_decoded(id, &path, image, depth) {
                        Ok(doc) => self.install_opened(doc, &path),
                        Err(e) => self.report_failed_open(&path, e),
                    }
                }
                Err(e) => {
                    self.dialogs.report_error(
                        "Cannot open this file",
                        &format!(
                            "{}

{e}",
                            path.display()
                        ),
                    );
                    self.recent.forget(&path);
                    self.status = Some(format!("Could not open {}", path.display()));
                    self.touch();
                }
            },
            crate::jobs::ImportOutcome::Psd { parsed, .. } => match parsed {
                Ok(import) => {
                    // W2-G: the layered parse already happened on the worker;
                    // this thread only wraps the result.
                    let id = self.mint_id();
                    let doc = OpenDocument::open_psd_import(id, &path, *import);
                    self.install_opened(doc, &path);
                }
                Err(e) => {
                    self.dialogs.report_error(
                        "Cannot open this file",
                        &format!(
                            "{}

{e}",
                            path.display()
                        ),
                    );
                    self.recent.forget(&path);
                    self.status = Some(format!("Could not open {}", path.display()));
                    self.touch();
                }
            },
        }
    }

    fn install_opened(&mut self, doc: OpenDocument, path: &Path) {
        let notes = doc.psd_notes().clone();
        self.docs.push(doc);
        self.active = Some(self.docs.len() - 1);
        self.recent.record(path);
        let _ = self.recent.save(&self.paths.recent_file());
        self.status = Some(format!("Opened {}", path.display()));
        // Card 077: a PSD that did not map exactly shows its fidelity report
        // right away on this user-initiated route — visible, not buried in a
        // status bar. A fully supported file has nothing to say and shows
        // nothing.
        if let Some(report) = notes.report(Some(path)) {
            self.dialogs.report_notice("PSD import report", &report);
        }
        self.touch();
    }

    fn report_failed_open(&mut self, path: &Path, e: DocumentError) {
        let message = format!(
            "{}

{e}",
            path.display()
        );
        self.dialogs.report_error("Cannot open this file", &message);
        self.recent.forget(path);
        self.status = Some(format!("Could not open {}", path.display()));
        self.touch();
    }

    /// W2-G: how this editor starts its jobs. See [`crate::jobs::Spawner`].
    pub fn spawner(&self) -> crate::jobs::Spawner {
        self.spawner
    }

    /// W2-G: replace how jobs are started — worker threads, inline, or a
    /// test's own queue. Jobs already in flight are unaffected.
    pub fn set_spawner(&mut self, spawner: crate::jobs::Spawner) {
        self.spawner = spawner;
    }

    /// W2-G: run an Export As batch (the dialog's job, into the folder the
    /// picker chose) on a worker against a snapshot of the active document.
    /// The completion — the files written, or why not — reaches the status
    /// line through [`Editor::poll_exports`].
    pub fn request_export(&mut self, job: ui::dialogs::ExportJob, dir: PathBuf) {
        let Some(doc) = self.active() else {
            self.set_status("Export needs an open document");
            return;
        };
        let export = crate::jobs::ExportJob {
            id: doc.id(),
            document: doc.document.clone(),
            tiles: doc.tiles.clone(),
            job,
            dir: dir.clone(),
        };
        let rx = crate::jobs::spawn_export_with(export, self.spawner);
        self.export_jobs.push(rx);
        self.status = Some(format!("Exporting to {}…", dir.display()));
        self.touch();
        // Inline spawner: already done. Threads: the frame loop polls.
        self.poll_exports();
    }

    /// W2-G: apply every export that has finished.
    pub fn poll_exports(&mut self) {
        if self.export_jobs.is_empty() {
            return;
        }
        let jobs = std::mem::take(&mut self.export_jobs);
        for rx in jobs {
            match rx.try_recv() {
                Ok(outcome) => self.finish_export(outcome),
                Err(std::sync::mpsc::TryRecvError::Empty) => self.export_jobs.push(rx),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.status = Some(
                        "Export failed: the export worker stopped without reporting".to_string(),
                    );
                    self.touch();
                }
            }
        }
    }

    fn finish_export(&mut self, outcome: crate::jobs::ExportOutcome) {
        use crate::jobs::ExportRoute;
        match outcome.result {
            Ok(paths) => {
                // A `.psd` export's fidelity notes belong on the document,
                // where File Info reads them.
                if let Some(notes) = outcome.psd_notes {
                    if let Some(doc) = self.docs.iter_mut().find(|d| d.id() == outcome.id) {
                        doc.set_psd_notes(notes);
                    }
                }
                self.status = Some(match outcome.route {
                    ExportRoute::File => format!("Exported {}", outcome.dir.display()),
                    ExportRoute::Layers => format!(
                        "Exported {} layer(s) to {}",
                        paths.len(),
                        outcome.dir.display()
                    ),
                    ExportRoute::Batch => {
                        let last = paths.last().map(|p| p.display().to_string());
                        format!(
                            "Exported {} file(s) to {}",
                            paths.len(),
                            last.unwrap_or_default()
                        )
                    }
                });
            }
            Err(e) => {
                tracing::warn!("export to {} failed: {e}", outcome.dir.display());
                self.status = Some(format!("Export failed: {e}"));
                // The user asked for this one file by name: a dialog, as the
                // synchronous route gave, not only a status line.
                if outcome.route == ExportRoute::File {
                    self.dialogs.report_error(
                        "Export failed",
                        &format!("{}\n\n{e}", outcome.dir.display()),
                    );
                }
            }
        }
        self.touch();
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

    /// File ▸ Export…: one flattened file (or a layered `.psd`).
    ///
    /// W2-G: the composite and the encode run on a worker against a snapshot
    /// ([`crate::jobs::spawn_file_export_with`]); this thread picks the file,
    /// runs the two checks that need the live document, and starts the job.
    /// The completion reaches the status line through
    /// [`Editor::poll_exports`], and a failure the error dialog as well.
    fn act_export(&mut self) -> Result<Effect, ActionError> {
        let action = Action::Export;
        let index = self.active.expect("`can` required a document");
        let suggested = self.docs[index].suggested_export_path();
        let Some(target) = self.dialogs.pick_export_path(&suggested) else {
            return Err(ActionError::Cancelled(action));
        };
        let doc = &self.docs[index];
        if crate::doc::exports_as_psd(&target) {
            // Card 077: the original `.psd` this document was opened from is
            // never silently overwritten with this build's reduced export.
            if let Some(source) = doc.source_path() {
                if crate::doc::exports_as_psd(source) && crate::doc::same_path(source, &target) {
                    return Err(ActionError::failed(
                        action,
                        DocumentError::OriginalOverwrite(source.to_path_buf()),
                    ));
                }
            }
        } else if crate::doc::export_format_for(&target).is_none() {
            return Err(ActionError::failed(
                action,
                DocumentError::UnknownExportFormat(
                    target
                        .extension()
                        .map(|e| e.to_string_lossy().into_owned())
                        .unwrap_or_else(|| target.display().to_string()),
                ),
            ));
        }
        let job = crate::jobs::FileExportJob {
            id: doc.id(),
            target: target.clone(),
            document: doc.document.clone(),
            tiles: doc.tiles.clone(),
            sixteen_bit: doc.is_sixteen_bit(),
        };
        let rx = crate::jobs::spawn_file_export_with(job, self.spawner);
        self.export_jobs.push(rx);
        self.status = Some(format!("Exporting {}…", target.display()));
        self.touch();
        // Inline spawner: already done. Threads: the frame loop polls.
        self.poll_exports();
        Ok(Effect::Exported)
    }

    fn act_quit(&mut self) -> Result<Effect, ActionError> {
        // Walk from the end so removing one does not renumber the rest.
        for index in (0..self.docs.len()).rev() {
            if self.docs[index].is_dirty() {
                let title = self.docs[index].title().to_string();
                match self.dialogs.confirm_close(&title) {
                    CloseChoice::Cancel => return Err(ActionError::Cancelled(Action::Quit)),
                    // Waited for: the process is about to exit.
                    CloseChoice::Save => self.save_document_and_wait(index, false)?,
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
mod save_status_tests {
    use super::save_status_text;
    use crate::jobs::SaveKind;

    /// W2-G: the status line names the save and, once the worker has counted
    /// its tiles, how far along it is.
    #[test]
    fn the_status_line_carries_the_tile_count_once_there_is_one() {
        assert_eq!(
            save_status_text("a.png", SaveKind::Save, 0, 0),
            "Saving a.png…"
        );
        assert_eq!(
            save_status_text("a.png", SaveKind::Save, 340, 1900),
            "Saving a.png… 340/1900 tiles"
        );
        assert_eq!(
            save_status_text("a.png", SaveKind::Autosave { scratch: true }, 2, 16),
            "Autosaving a.png… 2/16 tiles"
        );
    }
}

#[cfg(test)]
mod preview_sweep_tests {
    use super::*;

    #[test]
    fn a_tab_switch_leaves_no_stale_preview_on_the_tab_the_user_left() {
        // Card 039: two open documents; a preview lens lives on the inactive
        // one; the sweep drops it without touching the active document's.
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 16, 16, &[255u8; 16 * 16 * 4]).unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        let first = editor.active().unwrap().id();
        editor.open_path(&png).unwrap();
        let second = editor.active().unwrap().id();
        // A preview on the FIRST document (now inactive).
        let layer = editor.documents()[0].document.active_layer().unwrap();
        editor.documents_mut()[0]
            .set_preview(layer, glam::Affine2::from_translation(glam::vec2(4.0, 0.0)));
        assert!(editor.documents()[0].has_preview());

        editor.clear_stale_previews();
        assert!(
            !editor.documents()[0].has_preview(),
            "the inactive tab's lens is swept"
        );
        // The active document's lens (none here) is untouched; setting one
        // on the active document survives the sweep.
        let layer = editor.active().unwrap().document.active_layer().unwrap();
        editor
            .active_mut()
            .unwrap()
            .set_preview(layer, glam::Affine2::IDENTITY);
        editor.clear_stale_previews();
        assert!(
            editor
                .documents()
                .iter()
                .find(|d| d.id() == second)
                .unwrap()
                .has_preview(),
            "the active document's lens survives"
        );
        let _ = first;
    }
}

#[cfg(test)]
mod preference_tests {
    use super::*;

    fn editor_with_image(dir: &Path) -> Editor {
        let png = dir.join("a.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 16, 16, &[255u8; 16 * 16 * 4]).unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        editor
    }

    #[test]
    fn units_cm_makes_the_status_readout_say_cm() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_image(dir.path());
        ed.resize_canvas(72, 36, glam::IVec2::ZERO).unwrap();
        assert_eq!(ed.status(), Some("Canvas resized to 72 × 36 px"));

        let mut ui_prefs = ed.ui_preferences();
        ui_prefs.interface.units = ui::dialogs::Unit::Centimeters;
        ed.apply_ui_preferences(&ui_prefs);
        assert_eq!(ed.preferences().units, crate::prefs::UnitChoice::Cm);
        // 72 px at the rulers' 72 ppi is one inch: 2.54 cm.
        ed.resize_canvas(72, 36, glam::IVec2::ZERO).unwrap();
        assert_eq!(ed.status(), Some("Canvas resized to 2.540 × 1.270 cm"));
        // And the dialog re-opens showing what was applied.
        assert_eq!(
            ed.ui_preferences().interface.units,
            ui::dialogs::Unit::Centimeters
        );
    }

    #[test]
    fn a_binding_made_on_the_keymap_page_resolves_after_ok_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_image(dir.path());
        let mut dialog = ui::dialogs::PreferencesDialog::new(ed.ui_preferences());
        let free = ui::dialogs::Shortcut {
            key: egui::Key::F9,
            ctrl: true,
            shift: false,
            alt: true,
        };
        dialog.begin_capture(&Action::Export.id());
        dialog.capture(free).unwrap();
        let chord = crate::keymap::chord_of_editor_shortcut(free).unwrap();
        assert_eq!(ed.keymap().resolve(&chord), None, "nothing before OK");
        let Some(ui::dialogs::DialogAction::SetPreferences(confirmed)) =
            ui::dialogs::Dialog::confirm(&dialog)
        else {
            panic!("the dialog confirms its preferences");
        };
        ed.apply_ui_preferences(&confirmed);
        assert_eq!(ed.keymap().resolve(&chord), Some(Action::Export));
        assert!(ed
            .preferences()
            .keymap_overrides
            .iter()
            .any(|o| o.chord == chord && o.action == Some(Action::Export)));
        ed.persist().unwrap();
        let back = Preferences::load(&ed.paths().preferences_file());
        assert_eq!(
            Keymap::with_overrides(back.keymap_overrides).resolve(&chord),
            Some(Action::Export)
        );
    }

    #[test]
    fn a_conflict_on_the_keymap_page_is_reported_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_image(dir.path());
        let mut dialog = ui::dialogs::PreferencesDialog::new(ed.ui_preferences());
        dialog.begin_capture(&Action::Export.id());
        let ctrl_s = ui::dialogs::Shortcut::ctrl(egui::Key::S);
        assert_eq!(
            dialog.capture(ctrl_s),
            Err(ui::dialogs::KeymapError::Conflict {
                held_by: Action::Save.id()
            })
        );
        let (id, shortcut, _) = dialog.pending_conflict().expect("reported inline");
        assert_eq!(
            (id.as_str(), *shortcut),
            (Action::Export.id().as_str(), ctrl_s)
        );
        ed.apply_ui_preferences(dialog.prefs());
        assert_eq!(
            ed.keymap()
                .resolve(&crate::keymap::Chord::ctrl(crate::keymap::Key::character(
                    's'
                ))),
            Some(Action::Save)
        );
        assert!(ed.keymap().overrides().is_empty());
    }

    #[test]
    fn a_dialog_model_with_no_commands_leaves_the_live_keymap_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_image(dir.path());
        let chord = crate::keymap::Chord::ctrl(crate::keymap::Key::Function(9));
        ed.rebind(chord, Action::Export).unwrap();
        ed.apply_ui_preferences(&ui::dialogs::UiPreferences::default());
        assert_eq!(ed.keymap().resolve(&chord), Some(Action::Export));
    }

    #[test]
    fn the_scratch_directory_set_in_the_dialog_is_where_autosaves_go() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_image(dir.path());
        let elsewhere = dir.path().join("elsewhere");
        let mut ui_prefs = ed.ui_preferences();
        assert_eq!(ui_prefs.scratch.dir, "", "empty means the default");
        ui_prefs.scratch.dir = elsewhere.display().to_string();
        ed.apply_ui_preferences(&ui_prefs);
        assert_eq!(ed.preferences().scratch_dir(ed.paths()), elsewhere);
        // Emptied again, the default comes back.
        let mut ui_prefs = ed.ui_preferences();
        ui_prefs.scratch.dir = "  ".to_string();
        ed.apply_ui_preferences(&ui_prefs);
        assert_eq!(
            ed.preferences().scratch_dir(ed.paths()),
            ed.paths().default_scratch_dir()
        );
    }
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
