//! Preferences.
//!
//! Seven sections down the left, the chosen one on the right. The settings
//! model here is the UI's own — plain data with a [`UiPreferences::sanitized`]
//! pass — so the dialog can be exercised without a shell, a disk or a window.
//! The shell maps it onto whatever it persists.
//!
//! The keymap editor is the part worth being careful about: a shortcut that is
//! silently taken from another command is a bug the user only discovers later,
//! so [`Keymap::assign`] reports the collision instead of resolving it, and the
//! dialog makes the user decide.

use std::collections::BTreeMap;

use design::{tokens::Space, Theme};
use egui::{Context, Key, Modifiers};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, warning, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::controls::{checkbox_row, combo, integer, numeric, sidebar_list};
use super::sizes;
use super::units::format_bytes;

/// Which appearance the app uses.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ThemeChoice {
    /// Follow the operating system.
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    /// All three, in menu order.
    pub const ALL: &'static [ThemeChoice] = &[Self::System, Self::Light, Self::Dark];

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// The appearance to install, given what the system reports.
    pub const fn resolve(self, system: Theme) -> Theme {
        match self {
            Self::System => system,
            Self::Light => Theme::Light,
            Self::Dark => Theme::Dark,
        }
    }
}

/// What the brush cursor looks like.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum BrushCursor {
    /// An outline the size of the brush.
    #[default]
    BrushSize,
    /// A fixed crosshair.
    Crosshair,
    /// Both.
    SizeAndCrosshair,
}

impl BrushCursor {
    /// All, in menu order.
    pub const ALL: &'static [BrushCursor] =
        &[Self::BrushSize, Self::Crosshair, Self::SizeAndCrosshair];

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::BrushSize => "Brush size",
            Self::Crosshair => "Crosshair",
            Self::SizeAndCrosshair => "Brush size and crosshair",
        }
    }
}

/// General behaviour.
#[derive(Clone, PartialEq, Debug)]
pub struct GeneralPrefs {
    /// Minutes between autosaves; `0` turns autosave off.
    pub autosave_minutes: u32,
    pub restore_session: bool,
    pub confirm_before_discarding: bool,
    /// How many documents the Open Recent list keeps.
    pub recent_documents: u32,
}

impl Default for GeneralPrefs {
    fn default() -> Self {
        Self {
            autosave_minutes: 10,
            restore_session: true,
            confirm_before_discarding: true,
            recent_documents: 10,
        }
    }
}

/// Appearance and chrome.
#[derive(Clone, PartialEq, Debug)]
pub struct InterfacePrefs {
    pub theme: ThemeChoice,
    /// Multiplier on every point in the design system.
    pub ui_scale: f32,
    pub show_tooltips: bool,
    pub show_status_bar: bool,
}

impl Default for InterfacePrefs {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            ui_scale: 1.0,
            show_tooltips: true,
            show_status_bar: true,
        }
    }
}

/// Tool behaviour.
#[derive(Clone, PartialEq, Debug)]
pub struct ToolPrefs {
    pub brush_cursor: BrushCursor,
    pub scroll_wheel_zooms: bool,
    pub snap_to_guides: bool,
    /// Stabilisation applied to freehand strokes, `0..=0.99`.
    pub stroke_smoothing: f32,
}

impl Default for ToolPrefs {
    fn default() -> Self {
        Self {
            brush_cursor: BrushCursor::BrushSize,
            scroll_wheel_zooms: false,
            snap_to_guides: true,
            stroke_smoothing: 0.0,
        }
    }
}

/// Undo history.
#[derive(Clone, PartialEq, Debug)]
pub struct HistoryPrefs {
    /// How many undo steps are kept.
    pub states: u32,
    /// Every Nth state is stored as a full snapshot rather than a delta.
    pub snapshot_interval: u32,
    pub log_history_to_metadata: bool,
}

impl Default for HistoryPrefs {
    fn default() -> Self {
        Self {
            states: 100,
            snapshot_interval: 20,
            log_history_to_metadata: false,
        }
    }
}

/// Memory and threading.
#[derive(Clone, PartialEq, Debug)]
pub struct PerformancePrefs {
    /// Tile cache budget, in mebibytes.
    pub memory_budget_mb: u32,
    /// Worker threads; `0` means "one per core".
    pub worker_threads: u32,
    pub gpu_acceleration: bool,
}

impl Default for PerformancePrefs {
    fn default() -> Self {
        Self {
            memory_budget_mb: 2048,
            worker_threads: 0,
            gpu_acceleration: true,
        }
    }
}

/// One scratch location.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScratchDisk {
    pub path: String,
    pub enabled: bool,
}

/// Where the tile store spills to disk.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ScratchPrefs {
    /// In priority order; the first enabled one is used first.
    pub disks: Vec<ScratchDisk>,
}

impl ScratchPrefs {
    /// The disks that will actually be used, in order.
    pub fn active(&self) -> Vec<&ScratchDisk> {
        self.disks.iter().filter(|d| d.enabled).collect()
    }

    /// Move disk `index` one place up the priority list.
    pub fn promote(&mut self, index: usize) -> bool {
        if index == 0 || index >= self.disks.len() {
            return false;
        }
        self.disks.swap(index - 1, index);
        true
    }

    /// Drop disk `index`, leaving the rest in the same order.
    ///
    /// A list the user can grow but not shrink is half a control. Blanking a
    /// path and letting [`UiPreferences::sanitized`] drop it on confirm is not
    /// a substitute: nothing on screen says that is what happens.
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.disks.len() {
            return false;
        }
        self.disks.remove(index);
        true
    }
}

/// Every preference the UI owns.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct UiPreferences {
    pub general: GeneralPrefs,
    pub interface: InterfacePrefs,
    pub tools: ToolPrefs,
    pub history: HistoryPrefs,
    pub performance: PerformancePrefs,
    pub scratch: ScratchPrefs,
    pub keymap: Keymap,
}

/// Smallest and largest UI scale the interface stays usable at.
pub const UI_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.75..=2.0;
/// Largest number of undo states the dialog will accept.
pub const MAX_HISTORY_STATES: u32 = 1000;
/// Largest memory budget the dialog will accept, in mebibytes.
pub const MAX_MEMORY_MB: u32 = 1 << 20;

impl UiPreferences {
    /// Force every field into a range the app can actually run with.
    ///
    /// A preferences file is user-editable, so this is the boundary that stops
    /// a hand-typed `ui_scale: 0` from making the app unusable with no way back.
    pub fn sanitized(mut self) -> Self {
        self.general.recent_documents = self.general.recent_documents.clamp(0, 100);
        self.general.autosave_minutes = self.general.autosave_minutes.min(24 * 60);
        self.interface.ui_scale = if self.interface.ui_scale.is_finite() {
            self.interface
                .ui_scale
                .clamp(*UI_SCALE_RANGE.start(), *UI_SCALE_RANGE.end())
        } else {
            1.0
        };
        self.tools.stroke_smoothing = if self.tools.stroke_smoothing.is_finite() {
            self.tools.stroke_smoothing.clamp(0.0, 0.99)
        } else {
            0.0
        };
        self.history.states = self.history.states.clamp(1, MAX_HISTORY_STATES);
        self.history.snapshot_interval = self
            .history
            .snapshot_interval
            .clamp(1, self.history.states.max(1));
        self.performance.memory_budget_mb =
            self.performance.memory_budget_mb.clamp(64, MAX_MEMORY_MB);
        self.performance.worker_threads = self.performance.worker_threads.min(256);
        self.scratch.disks.retain(|d| !d.path.trim().is_empty());
        self
    }

    /// Whether every field is already inside its range.
    pub fn is_sane(&self) -> bool {
        *self == self.clone().sanitized()
    }

    /// Autosave interval, or `None` when autosave is off.
    pub fn autosave_interval(&self) -> Option<std::time::Duration> {
        (self.general.autosave_minutes > 0)
            .then(|| std::time::Duration::from_secs(u64::from(self.general.autosave_minutes) * 60))
    }
}

// ------------------------------------------------------------------ keymap

/// A key plus its modifiers.
///
/// Ordered so it can key a `BTreeMap` in the conflict scan; the ordering is
/// arbitrary but total, which is all that is asked of it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Shortcut {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Shortcut {
    /// A shortcut with no modifiers.
    pub const fn plain(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    /// Ctrl (Command on macOS) plus `key`.
    pub const fn ctrl(key: Key) -> Self {
        Self {
            key,
            ctrl: true,
            shift: false,
            alt: false,
        }
    }

    /// Ctrl+Shift plus `key`.
    pub const fn ctrl_shift(key: Key) -> Self {
        Self {
            key,
            ctrl: true,
            shift: true,
            alt: false,
        }
    }

    /// The egui modifier set this shortcut needs.
    pub fn modifiers(self) -> Modifiers {
        Modifiers {
            alt: self.alt,
            ctrl: self.ctrl,
            shift: self.shift,
            mac_cmd: false,
            command: self.ctrl,
        }
    }

    /// How the shortcut is written in a menu.
    pub fn display(self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        out.push_str(self.key.name());
        out
    }
}

/// One bindable command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyCommand {
    /// Stable identifier.
    pub id: &'static str,
    /// Menu the command lives in, for grouping the editor.
    pub category: &'static str,
    pub label: &'static str,
    pub default: Option<Shortcut>,
}

const fn command(
    id: &'static str,
    category: &'static str,
    label: &'static str,
    default: Shortcut,
) -> KeyCommand {
    KeyCommand {
        id,
        category,
        label,
        default: Some(default),
    }
}

/// Every command the keymap editor can rebind.
pub const KEY_COMMANDS: &[KeyCommand] = &[
    command("file.new", "File", "New", Shortcut::ctrl(Key::N)),
    command("file.open", "File", "Open", Shortcut::ctrl(Key::O)),
    command("file.save", "File", "Save", Shortcut::ctrl(Key::S)),
    command(
        "file.save_as",
        "File",
        "Save As",
        Shortcut::ctrl_shift(Key::S),
    ),
    command(
        "file.export",
        "File",
        "Export As",
        Shortcut::ctrl_shift(Key::E),
    ),
    command("edit.undo", "Edit", "Undo", Shortcut::ctrl(Key::Z)),
    command("edit.redo", "Edit", "Redo", Shortcut::ctrl_shift(Key::Z)),
    command("edit.cut", "Edit", "Cut", Shortcut::ctrl(Key::X)),
    command("edit.copy", "Edit", "Copy", Shortcut::ctrl(Key::C)),
    command("edit.paste", "Edit", "Paste", Shortcut::ctrl(Key::V)),
    command("image.size", "Image", "Image Size", Shortcut::ctrl(Key::I)),
    command(
        "image.canvas_size",
        "Image",
        "Canvas Size",
        Shortcut::ctrl_shift(Key::C),
    ),
    command(
        "layer.new",
        "Layer",
        "New Layer",
        Shortcut::ctrl_shift(Key::N),
    ),
    command(
        "layer.group",
        "Layer",
        "Group Layers",
        Shortcut::ctrl(Key::G),
    ),
    command("select.all", "Select", "Select All", Shortcut::ctrl(Key::A)),
    command(
        "select.deselect",
        "Select",
        "Deselect",
        Shortcut::ctrl(Key::D),
    ),
    command("view.zoom_in", "View", "Zoom In", Shortcut::ctrl(Key::Plus)),
    command(
        "view.zoom_out",
        "View",
        "Zoom Out",
        Shortcut::ctrl(Key::Minus),
    ),
    command(
        "view.fit",
        "View",
        "Fit on Screen",
        Shortcut::ctrl(Key::Num0),
    ),
    command("tool.brush", "Tools", "Brush", Shortcut::plain(Key::B)),
    command("tool.eraser", "Tools", "Eraser", Shortcut::plain(Key::E)),
    command("tool.move", "Tools", "Move", Shortcut::plain(Key::V)),
    command("tool.marquee", "Tools", "Marquee", Shortcut::plain(Key::M)),
    command("tool.lasso", "Tools", "Lasso", Shortcut::plain(Key::L)),
    command(
        "tool.eyedropper",
        "Tools",
        "Eyedropper",
        Shortcut::plain(Key::I),
    ),
];

/// Why a shortcut could not be assigned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeymapError {
    /// The command id is not in [`KEY_COMMANDS`].
    UnknownCommand,
    /// Another command already owns that shortcut.
    Conflict { held_by: &'static str },
}

impl std::fmt::Display for KeymapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCommand => write!(f, "no such command"),
            Self::Conflict { held_by } => {
                let label = KEY_COMMANDS
                    .iter()
                    .find(|c| c.id == *held_by)
                    .map_or(*held_by, |c| c.label);
                write!(f, "already used by {label}")
            }
        }
    }
}

/// The whole keyboard map: one optional shortcut per command.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Keymap {
    bindings: BTreeMap<&'static str, Option<Shortcut>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: KEY_COMMANDS.iter().map(|c| (c.id, c.default)).collect(),
        }
    }
}

impl Keymap {
    /// The shortcut bound to `id`, if any.
    pub fn shortcut(&self, id: &str) -> Option<Shortcut> {
        self.bindings.get(id).copied().flatten()
    }

    /// The command a shortcut currently triggers.
    pub fn command_for(&self, shortcut: Shortcut) -> Option<&'static str> {
        self.bindings
            .iter()
            .find(|(_, bound)| **bound == Some(shortcut))
            .map(|(id, _)| *id)
    }

    /// Bind `shortcut` to `id`.
    ///
    /// Refuses rather than stealing: a shortcut already owned by another
    /// command comes back as [`KeymapError::Conflict`] naming the owner, and
    /// nothing changes. Re-assigning a command its own shortcut succeeds and is
    /// a no-op.
    pub fn assign(&mut self, id: &str, shortcut: Shortcut) -> Result<(), KeymapError> {
        let Some(key) = KEY_COMMANDS.iter().find(|c| c.id == id).map(|c| c.id) else {
            return Err(KeymapError::UnknownCommand);
        };
        if let Some(owner) = self.command_for(shortcut) {
            if owner != key {
                return Err(KeymapError::Conflict { held_by: owner });
            }
            return Ok(());
        }
        self.bindings.insert(key, Some(shortcut));
        Ok(())
    }

    /// Bind `shortcut` to `id`, taking it away from whoever had it.
    ///
    /// Returns the command that lost it. The dialog only calls this after the
    /// user has been told what they are about to displace.
    pub fn force_assign(&mut self, id: &str, shortcut: Shortcut) -> Option<&'static str> {
        let key = KEY_COMMANDS.iter().find(|c| c.id == id).map(|c| c.id)?;
        let displaced = self.command_for(shortcut).filter(|owner| *owner != key);
        if let Some(owner) = displaced {
            self.bindings.insert(owner, None);
        }
        self.bindings.insert(key, Some(shortcut));
        displaced
    }

    /// Unbind a command.
    pub fn clear(&mut self, id: &str) -> bool {
        match KEY_COMMANDS.iter().find(|c| c.id == id) {
            Some(command) => {
                self.bindings.insert(command.id, None);
                true
            }
            None => false,
        }
    }

    /// Put every command back on its default shortcut.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether `id` differs from its default.
    pub fn is_customized(&self, id: &str) -> bool {
        KEY_COMMANDS
            .iter()
            .find(|c| c.id == id)
            .is_some_and(|c| self.shortcut(c.id) != c.default)
    }

    /// Every pair of commands sharing a shortcut.
    ///
    /// Should always be empty — [`Keymap::assign`] refuses to create one — but
    /// a keymap can also arrive from a file, and a duplicate there must be
    /// visible rather than silently first-wins.
    pub fn conflicts(&self) -> Vec<(&'static str, &'static str)> {
        let mut seen: BTreeMap<Shortcut, &'static str> = BTreeMap::new();
        let mut out = Vec::new();
        for command in KEY_COMMANDS {
            let Some(shortcut) = self.shortcut(command.id) else {
                continue;
            };
            match seen.get(&shortcut) {
                Some(first) => out.push((*first, command.id)),
                None => {
                    seen.insert(shortcut, command.id);
                }
            }
        }
        out
    }
}

// ------------------------------------------------------------------ dialog

/// The sections of the Preferences dialog, in sidebar order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PrefsSection {
    #[default]
    General,
    Interface,
    Tools,
    History,
    Performance,
    ScratchDisks,
    Keymap,
}

impl PrefsSection {
    /// All seven, in sidebar order.
    pub const ALL: [PrefsSection; 7] = [
        Self::General,
        Self::Interface,
        Self::Tools,
        Self::History,
        Self::Performance,
        Self::ScratchDisks,
        Self::Keymap,
    ];

    /// Sidebar label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Interface => "Interface",
            Self::Tools => "Tools",
            Self::History => "History",
            Self::Performance => "Performance",
            Self::ScratchDisks => "Scratch Disks",
            Self::Keymap => "Keyboard Shortcuts",
        }
    }
}

/// Preferences.
#[derive(Clone, Debug)]
pub struct PreferencesDialog {
    prefs: UiPreferences,
    original: UiPreferences,
    section: PrefsSection,
    /// The command whose shortcut is being captured, if any.
    capturing: Option<&'static str>,
    /// The most recent refused assignment, shown in the editor.
    last_conflict: Option<(&'static str, Shortcut, KeymapError)>,
}

impl Default for PreferencesDialog {
    fn default() -> Self {
        Self::new(UiPreferences::default())
    }
}

impl PreferencesDialog {
    /// Open on `prefs`, sanitised on the way in.
    pub fn new(prefs: UiPreferences) -> Self {
        let prefs = prefs.sanitized();
        Self {
            original: prefs.clone(),
            prefs,
            section: PrefsSection::General,
            capturing: None,
            last_conflict: None,
        }
    }

    /// The preferences as edited.
    pub fn prefs(&self) -> &UiPreferences {
        &self.prefs
    }

    /// Mutable access.
    pub fn prefs_mut(&mut self) -> &mut UiPreferences {
        &mut self.prefs
    }

    /// The section on screen.
    pub fn section(&self) -> PrefsSection {
        self.section
    }

    /// Show a different section.
    pub fn set_section(&mut self, section: PrefsSection) {
        self.section = section;
    }

    /// Whether anything changed since the dialog opened.
    pub fn is_modified(&self) -> bool {
        self.prefs != self.original
    }

    /// Restore every setting in *every* section to its default.
    pub fn restore_defaults(&mut self) {
        self.prefs = UiPreferences::default();
        self.last_conflict = None;
        self.capturing = None;
    }

    /// The command whose next keystroke will be captured.
    pub fn capturing(&self) -> Option<&'static str> {
        self.capturing
    }

    /// Start listening for a shortcut for `id`.
    pub fn begin_capture(&mut self, id: &'static str) {
        self.capturing = Some(id);
        self.last_conflict = None;
    }

    /// Stop listening without binding anything.
    pub fn cancel_capture(&mut self) {
        self.capturing = None;
    }

    /// Try to bind `shortcut` to the command being captured.
    ///
    /// On a conflict the binding is **not** made; the collision is recorded so
    /// the dialog can offer to displace the other command, and capture stays
    /// open so Escape still gets the user out.
    pub fn capture(&mut self, shortcut: Shortcut) -> Result<(), KeymapError> {
        let Some(id) = self.capturing else {
            return Err(KeymapError::UnknownCommand);
        };
        match self.prefs.keymap.assign(id, shortcut) {
            Ok(()) => {
                self.capturing = None;
                self.last_conflict = None;
                Ok(())
            }
            Err(error) => {
                self.last_conflict = Some((id, shortcut, error));
                Err(error)
            }
        }
    }

    /// Take the pending shortcut away from whoever holds it and give it to the
    /// command that was refused.
    pub fn resolve_conflict(&mut self) -> Option<&'static str> {
        let (id, shortcut, _) = self.last_conflict.take()?;
        self.capturing = None;
        self.prefs.keymap.force_assign(id, shortcut)
    }

    /// The refused assignment still waiting on the user, if any.
    pub fn pending_conflict(&self) -> Option<(&'static str, Shortcut, KeymapError)> {
        self.last_conflict
    }

    /// Draw the dialog for one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        if keys.cancel && self.capturing.is_some() {
            // Escape leaves the shortcut field before it leaves the dialog.
            self.cancel_capture();
            return DialogOutcome::Open;
        }
        if self.capturing.is_some() {
            if let Some(shortcut) = read_shortcut(ctx) {
                let _ = self.capture(shortcut);
            }
        }
        let mut outcome = super::chrome::resolve(self, keys);
        let drawn = modal(
            ctx,
            "preferences",
            self.title(),
            None,
            DialogWidth::Broad,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => {
                    self.restore_defaults();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(sizes::sidebar_width());
                let labels: Vec<&str> = PrefsSection::ALL.iter().map(|s| s.label()).collect();
                let mut index = PrefsSection::ALL
                    .iter()
                    .position(|s| *s == self.section)
                    .unwrap_or(0);
                if sidebar_list(ui, &mut index, &labels) {
                    self.section = PrefsSection::ALL[index];
                }
            });
            ui.add_space(Space::Large.pt());
            ui.vertical(|ui| {
                ui.set_width(sizes::pane_width());
                egui::ScrollArea::vertical()
                    .max_height(sizes::pane_max_height())
                    .show(ui, |ui| match self.section {
                        PrefsSection::General => self.general(ui),
                        PrefsSection::Interface => self.interface(ui),
                        PrefsSection::Tools => self.tools(ui),
                        PrefsSection::History => self.history(ui),
                        PrefsSection::Performance => self.performance(ui),
                        PrefsSection::ScratchDisks => self.scratch(ui),
                        PrefsSection::Keymap => self.keymap(ui),
                    });
            });
        });
        hairline(ui);
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &["Restore Defaults"],
        )
    }

    fn general(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Documents");
        design::inspector_field(ui, "Autosave", |ui| {
            let mut minutes = i64::from(self.prefs.general.autosave_minutes);
            if integer(ui, &mut minutes, 0..=1440).changed() {
                self.prefs.general.autosave_minutes = minutes.max(0) as u32;
            }
            caption(ui, "minutes (0 is off)");
        });
        design::inspector_field(ui, "Recent files", |ui| {
            let mut count = i64::from(self.prefs.general.recent_documents);
            if integer(ui, &mut count, 0..=100).changed() {
                self.prefs.general.recent_documents = count.max(0) as u32;
            }
        });
        checkbox_row(
            ui,
            "Reopen the last session at startup",
            &mut self.prefs.general.restore_session,
        );
        checkbox_row(
            ui,
            "Ask before discarding unsaved work",
            &mut self.prefs.general.confirm_before_discarding,
        );
        if self.prefs.general.autosave_minutes == 0 {
            caption(
                ui,
                "Autosave is off: a crash loses everything since the last save.",
            );
        }
    }

    fn interface(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Appearance");
        design::inspector_field(ui, "Theme", |ui| {
            combo(
                ui,
                "pref-theme",
                &mut self.prefs.interface.theme,
                ThemeChoice::ALL,
                |t| t.label().to_string(),
                |_| None,
            );
        });
        design::inspector_field(ui, "UI scale", |ui| {
            let mut scale = f64::from(self.prefs.interface.ui_scale) * 100.0;
            let lo = f64::from(*UI_SCALE_RANGE.start()) * 100.0;
            let hi = f64::from(*UI_SCALE_RANGE.end()) * 100.0;
            if numeric(ui, &mut scale, lo..=hi, 0, "%").changed() {
                self.prefs.interface.ui_scale = (scale / 100.0) as f32;
            }
        });
        checkbox_row(ui, "Show tooltips", &mut self.prefs.interface.show_tooltips);
        checkbox_row(
            ui,
            "Show the status bar",
            &mut self.prefs.interface.show_status_bar,
        );
    }

    fn tools(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Pointer");
        design::inspector_field(ui, "Brush cursor", |ui| {
            combo(
                ui,
                "pref-cursor",
                &mut self.prefs.tools.brush_cursor,
                BrushCursor::ALL,
                |c| c.label().to_string(),
                |_| None,
            );
        });
        checkbox_row(
            ui,
            "Scroll wheel zooms instead of scrolling",
            &mut self.prefs.tools.scroll_wheel_zooms,
        );
        checkbox_row(ui, "Snap to guides", &mut self.prefs.tools.snap_to_guides);
        design::slider_row(
            ui,
            "Smoothing",
            &mut self.prefs.tools.stroke_smoothing,
            0.0..=0.99,
        );
    }

    fn history(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Undo");
        design::inspector_field(ui, "States", |ui| {
            let mut states = i64::from(self.prefs.history.states);
            if integer(ui, &mut states, 1..=i64::from(MAX_HISTORY_STATES)).changed() {
                self.prefs.history.states = states.max(1) as u32;
            }
        });
        design::inspector_field(ui, "Snapshot every", |ui| {
            let mut interval = i64::from(self.prefs.history.snapshot_interval);
            let max = i64::from(self.prefs.history.states.max(1));
            if integer(ui, &mut interval, 1..=max).changed() {
                self.prefs.history.snapshot_interval = interval.max(1) as u32;
            }
            caption(ui, "states");
        });
        checkbox_row(
            ui,
            "Record the edit log in saved files",
            &mut self.prefs.history.log_history_to_metadata,
        );
    }

    fn performance(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Memory");
        design::inspector_field(ui, "Tile cache", |ui| {
            let mut mb = i64::from(self.prefs.performance.memory_budget_mb);
            if integer(ui, &mut mb, 64..=i64::from(MAX_MEMORY_MB)).changed() {
                self.prefs.performance.memory_budget_mb = mb.max(64) as u32;
            }
            caption(ui, "MB");
        });
        caption(
            ui,
            format!(
                "About {}",
                format_bytes(u64::from(self.prefs.performance.memory_budget_mb) << 20)
            ),
        );
        design::section_header(ui, "Threads");
        design::inspector_field(ui, "Workers", |ui| {
            let mut threads = i64::from(self.prefs.performance.worker_threads);
            if integer(ui, &mut threads, 0..=256).changed() {
                self.prefs.performance.worker_threads = threads.max(0) as u32;
            }
            caption(ui, "0 = one per core");
        });
        checkbox_row(
            ui,
            "Use the GPU where available",
            &mut self.prefs.performance.gpu_acceleration,
        );
    }

    fn scratch(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Scratch disks");
        let count = self.prefs.scratch.disks.len();
        // Both decided while drawing and applied after it: either one changes
        // the length or the order of the very list being iterated.
        let mut promote: Option<usize> = None;
        let mut remove: Option<usize> = None;
        for index in 0..count {
            ui.horizontal(|ui| {
                let disk = &mut self.prefs.scratch.disks[index];
                checkbox_row(ui, "", &mut disk.enabled);
                ui.add(
                    egui::TextEdit::singleline(&mut disk.path)
                        .desired_width(sizes::text_field_path()),
                );
                if index > 0 && design::ghost_button(ui, "Move up").clicked() {
                    promote = Some(index);
                }
                if design::ghost_button(ui, "Remove")
                    .on_hover_text("Stop using this location for scratch")
                    .clicked()
                {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = promote {
            self.prefs.scratch.promote(index);
        }
        if let Some(index) = remove {
            self.prefs.scratch.remove(index);
        }
        if design::ghost_button(ui, "Add scratch disk").clicked() {
            self.prefs.scratch.disks.push(ScratchDisk {
                path: String::new(),
                enabled: true,
            });
        }
        if self.prefs.scratch.active().is_empty() {
            caption(
                ui,
                "No scratch disk is enabled — the tile store stays entirely in memory.",
            );
        }
    }

    fn keymap(&mut self, ui: &mut egui::Ui) {
        if let Some((id, shortcut, error)) = self.last_conflict {
            let label = KEY_COMMANDS
                .iter()
                .find(|c| c.id == id)
                .map_or(id, |c| c.label);
            warning(ui, format!("{} for {label}: {error}", shortcut.display()));
            ui.horizontal(|ui| {
                if design::primary_button(ui, "Reassign anyway").clicked() {
                    self.resolve_conflict();
                }
                if design::secondary_button(ui, "Keep as it was").clicked() {
                    self.last_conflict = None;
                    self.capturing = None;
                }
            });
            hairline(ui);
        }
        let mut category = "";
        for command in KEY_COMMANDS {
            if command.category != category {
                category = command.category;
                design::section_header(ui, category);
            }
            ui.horizontal(|ui| {
                let capturing = self.capturing == Some(command.id);
                let text = if capturing {
                    "Press a key…".to_string()
                } else {
                    self.prefs
                        .keymap
                        .shortcut(command.id)
                        .map_or_else(|| "—".to_string(), Shortcut::display)
                };
                design::inspector_field(ui, command.label, |ui| {
                    if design::secondary_button(ui, &text).clicked() {
                        self.begin_capture(command.id);
                    }
                    if design::ghost_button(ui, "Clear").clicked() {
                        self.prefs.keymap.clear(command.id);
                    }
                    if self.prefs.keymap.is_customized(command.id) {
                        caption(ui, "changed");
                    }
                });
            });
        }
        ui.add_space(Space::Small.pt());
        if design::secondary_button(ui, "Reset all shortcuts").clicked() {
            self.prefs.keymap.reset();
        }
        for (first, second) in self.prefs.keymap.conflicts() {
            warning(ui, format!("{first} and {second} share a shortcut"));
        }
    }
}

/// The first key pressed this frame, with its modifiers — what the shortcut
/// field captures.
///
/// Escape and Enter are skipped: they belong to the dialog grammar, and a
/// keymap editor that lets the user bind Escape traps them in it.
pub fn read_shortcut(ctx: &Context) -> Option<Shortcut> {
    ctx.input(|input| {
        let modifiers = input.modifiers;
        input.events.iter().find_map(|event| match event {
            egui::Event::Key {
                key, pressed: true, ..
            } if !matches!(key, Key::Escape | Key::Enter | Key::Tab) => Some(Shortcut {
                key: *key,
                ctrl: modifiers.ctrl || modifiers.command,
                shift: modifiers.shift,
                alt: modifiers.alt,
            }),
            _ => None,
        })
    })
}

impl Dialog for PreferencesDialog {
    fn title(&self) -> &'static str {
        "Preferences"
    }

    fn confirm_label(&self) -> &'static str {
        "Save"
    }

    fn confirm(&self) -> Option<DialogAction> {
        let prefs = self.prefs.clone().sanitized();
        prefs
            .keymap
            .conflicts()
            .is_empty()
            .then(|| DialogAction::SetPreferences(Box::new(prefs)))
    }

    fn blocked_reason(&self) -> Option<String> {
        let conflicts = self.prefs.keymap.conflicts();
        (!conflicts.is_empty()).then(|| {
            format!(
                "{} keyboard shortcut{} used twice",
                conflicts.len(),
                if conflicts.len() == 1 { " is" } else { "s are" }
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::frame_both_themes;

    #[test]
    fn the_defaults_are_already_sane() {
        assert!(UiPreferences::default().is_sane());
    }

    #[test]
    fn sanitizing_repairs_every_out_of_range_field() {
        let mut prefs = UiPreferences::default();
        prefs.interface.ui_scale = 0.0;
        prefs.tools.stroke_smoothing = 5.0;
        prefs.history.states = 0;
        prefs.history.snapshot_interval = 0;
        prefs.performance.memory_budget_mb = 1;
        prefs.performance.worker_threads = 10_000;
        prefs.general.recent_documents = 5_000;
        prefs.general.autosave_minutes = 100_000;
        let fixed = prefs.sanitized();
        assert_eq!(fixed.interface.ui_scale, *UI_SCALE_RANGE.start());
        assert_eq!(fixed.tools.stroke_smoothing, 0.99);
        assert_eq!(fixed.history.states, 1);
        assert_eq!(fixed.history.snapshot_interval, 1);
        assert_eq!(fixed.performance.memory_budget_mb, 64);
        assert_eq!(fixed.performance.worker_threads, 256);
        assert_eq!(fixed.general.recent_documents, 100);
        assert_eq!(fixed.general.autosave_minutes, 1440);
        assert!(fixed.is_sane());
    }

    #[test]
    fn a_non_finite_setting_falls_back_to_its_default_not_to_a_boundary() {
        // Clamping a NaN keeps the NaN, and clamping an infinity would silently
        // turn "this field is corrupt" into "the user asked for the maximum".
        // Both go back to the default instead.
        let mut prefs = UiPreferences::default();
        prefs.interface.ui_scale = f32::NAN;
        prefs.tools.stroke_smoothing = f32::INFINITY;
        let fixed = prefs.sanitized();
        assert_eq!(fixed.interface.ui_scale, 1.0);
        assert_eq!(fixed.tools.stroke_smoothing, 0.0);
        assert!(fixed.is_sane());
    }

    #[test]
    fn the_snapshot_interval_never_exceeds_the_history_depth() {
        let mut prefs = UiPreferences::default();
        prefs.history.states = 10;
        prefs.history.snapshot_interval = 500;
        assert_eq!(prefs.sanitized().history.snapshot_interval, 10);
    }

    #[test]
    fn autosave_off_means_no_interval() {
        let mut prefs = UiPreferences::default();
        assert_eq!(
            prefs.autosave_interval(),
            Some(std::time::Duration::from_secs(600))
        );
        prefs.general.autosave_minutes = 0;
        assert_eq!(prefs.autosave_interval(), None);
    }

    #[test]
    fn a_blank_scratch_path_is_dropped_on_the_way_in() {
        let mut prefs = UiPreferences::default();
        prefs.scratch.disks = vec![
            ScratchDisk {
                path: "  ".to_string(),
                enabled: true,
            },
            ScratchDisk {
                path: "D:/scratch".to_string(),
                enabled: true,
            },
        ];
        let fixed = prefs.sanitized();
        assert_eq!(fixed.scratch.disks.len(), 1);
        assert_eq!(fixed.scratch.active().len(), 1);
    }

    #[test]
    fn a_removed_scratch_disk_leaves_the_priority_order_intact() {
        // Four, not three: with three, a `swap_remove` of the middle entry
        // produces the same list as an order-preserving `remove`, and the test
        // would pass over a bug it is meant to catch.
        let mut scratch = ScratchPrefs {
            disks: ["a", "b", "c", "d"]
                .into_iter()
                .map(|path| ScratchDisk {
                    path: path.into(),
                    enabled: true,
                })
                .collect(),
        };
        assert!(!scratch.remove(4), "an index that does not exist");
        assert_eq!(scratch.disks.len(), 4);
        assert!(scratch.remove(1));
        let order: Vec<&str> = scratch.disks.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(order, ["a", "c", "d"], "removing one resorted the rest");
        assert_eq!(scratch.active().len(), 3);
        for _ in 0..3 {
            assert!(scratch.remove(0));
        }
        assert!(scratch.disks.is_empty());
        assert!(!scratch.remove(0), "an empty list has nothing to remove");
    }

    #[test]
    fn scratch_priority_can_be_reordered() {
        let mut scratch = ScratchPrefs {
            disks: vec![
                ScratchDisk {
                    path: "a".into(),
                    enabled: true,
                },
                ScratchDisk {
                    path: "b".into(),
                    enabled: true,
                },
            ],
        };
        assert!(!scratch.promote(0));
        assert!(!scratch.promote(9));
        assert!(scratch.promote(1));
        assert_eq!(scratch.disks[0].path, "b");
    }

    #[test]
    fn the_theme_choice_resolves_the_way_the_menu_promises() {
        assert_eq!(ThemeChoice::Light.resolve(Theme::Dark), Theme::Light);
        assert_eq!(ThemeChoice::Dark.resolve(Theme::Light), Theme::Dark);
        assert_eq!(ThemeChoice::System.resolve(Theme::Light), Theme::Light);
        assert_eq!(ThemeChoice::System.resolve(Theme::Dark), Theme::Dark);
    }

    #[test]
    fn the_default_keymap_has_no_conflicts() {
        let keymap = Keymap::default();
        assert!(
            keymap.conflicts().is_empty(),
            "shipped defaults collide: {:?}",
            keymap.conflicts()
        );
        for command in KEY_COMMANDS {
            assert_eq!(
                keymap.shortcut(command.id),
                command.default,
                "{}",
                command.id
            );
            assert!(!keymap.is_customized(command.id));
        }
    }

    #[test]
    fn every_command_has_a_unique_id() {
        let mut ids: Vec<&str> = KEY_COMMANDS.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn a_shortcut_already_in_use_is_refused_and_names_its_owner() {
        let mut keymap = Keymap::default();
        let save = Shortcut::ctrl(Key::S);
        assert_eq!(keymap.command_for(save), Some("file.save"));
        let error = keymap.assign("tool.brush", save).unwrap_err();
        assert_eq!(
            error,
            KeymapError::Conflict {
                held_by: "file.save"
            }
        );
        assert_eq!(error.to_string(), "already used by Save");
        // And nothing moved.
        assert_eq!(keymap.shortcut("file.save"), Some(save));
        assert_eq!(keymap.shortcut("tool.brush"), Some(Shortcut::plain(Key::B)));
    }

    #[test]
    fn reassigning_a_command_its_own_shortcut_is_a_no_op() {
        let mut keymap = Keymap::default();
        let save = Shortcut::ctrl(Key::S);
        assert!(keymap.assign("file.save", save).is_ok());
        assert_eq!(keymap.shortcut("file.save"), Some(save));
    }

    #[test]
    fn a_free_shortcut_binds() {
        let mut keymap = Keymap::default();
        let free = Shortcut::ctrl_shift(Key::K);
        assert_eq!(keymap.command_for(free), None);
        assert!(keymap.assign("tool.brush", free).is_ok());
        assert_eq!(keymap.shortcut("tool.brush"), Some(free));
        assert!(keymap.is_customized("tool.brush"));
        assert!(keymap.conflicts().is_empty());
    }

    #[test]
    fn an_unknown_command_cannot_be_bound() {
        let mut keymap = Keymap::default();
        assert_eq!(
            keymap.assign("nope", Shortcut::plain(Key::Q)),
            Err(KeymapError::UnknownCommand)
        );
        assert!(!keymap.clear("nope"));
        assert_eq!(keymap.force_assign("nope", Shortcut::plain(Key::Q)), None);
    }

    #[test]
    fn forcing_an_assignment_unbinds_the_previous_owner() {
        let mut keymap = Keymap::default();
        let save = Shortcut::ctrl(Key::S);
        assert_eq!(keymap.force_assign("tool.brush", save), Some("file.save"));
        assert_eq!(keymap.shortcut("tool.brush"), Some(save));
        assert_eq!(keymap.shortcut("file.save"), None);
        assert!(keymap.conflicts().is_empty());
    }

    #[test]
    fn clearing_and_resetting_a_binding() {
        let mut keymap = Keymap::default();
        assert!(keymap.clear("file.save"));
        assert_eq!(keymap.shortcut("file.save"), None);
        assert!(keymap.is_customized("file.save"));
        keymap.reset();
        assert_eq!(keymap.shortcut("file.save"), Some(Shortcut::ctrl(Key::S)));
        assert!(!keymap.is_customized("file.save"));
    }

    #[test]
    fn shortcuts_display_their_modifiers_in_a_stable_order() {
        assert_eq!(Shortcut::ctrl(Key::S).display(), "Ctrl+S");
        assert_eq!(Shortcut::ctrl_shift(Key::S).display(), "Ctrl+Shift+S");
        assert_eq!(Shortcut::plain(Key::B).display(), "B");
        let all = Shortcut {
            key: Key::K,
            ctrl: true,
            shift: true,
            alt: true,
        };
        assert_eq!(all.display(), "Ctrl+Alt+Shift+K");
    }

    #[test]
    fn a_shortcut_maps_onto_egui_modifiers() {
        let m = Shortcut::ctrl_shift(Key::S).modifiers();
        assert!(m.ctrl && m.command && m.shift && !m.alt);
        let plain = Shortcut::plain(Key::B).modifiers();
        assert!(!plain.ctrl && !plain.shift && !plain.alt);
    }

    #[test]
    fn a_keymap_from_a_file_can_still_be_shown_to_conflict() {
        let mut keymap = Keymap::default();
        // force_assign is the only way to create a duplicate, and it does not.
        // A conflict therefore has to be constructed the way a file would: two
        // commands holding the same shortcut.
        keymap
            .bindings
            .insert("tool.brush", Some(Shortcut::ctrl(Key::S)));
        let conflicts = keymap.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].0 == "file.save" || conflicts[0].1 == "file.save");
    }

    #[test]
    fn the_capture_flow_binds_a_free_shortcut_and_closes() {
        let mut dialog = PreferencesDialog::default();
        dialog.begin_capture("tool.brush");
        assert_eq!(dialog.capturing(), Some("tool.brush"));
        assert!(dialog.capture(Shortcut::ctrl_shift(Key::K)).is_ok());
        assert_eq!(dialog.capturing(), None);
        assert_eq!(
            dialog.prefs().keymap.shortcut("tool.brush"),
            Some(Shortcut::ctrl_shift(Key::K))
        );
    }

    #[test]
    fn the_capture_flow_refuses_a_taken_shortcut_and_offers_a_way_out() {
        let mut dialog = PreferencesDialog::default();
        dialog.begin_capture("tool.brush");
        let save = Shortcut::ctrl(Key::S);
        assert!(dialog.capture(save).is_err());
        // Nothing moved yet, and the dialog knows what it refused.
        assert_eq!(dialog.prefs().keymap.shortcut("file.save"), Some(save));
        assert_eq!(
            dialog.prefs().keymap.shortcut("tool.brush"),
            Some(Shortcut::plain(Key::B))
        );
        let (id, shortcut, _) = dialog.pending_conflict().expect("a recorded conflict");
        assert_eq!(id, "tool.brush");
        assert_eq!(shortcut, save);
        // Then the user decides.
        assert_eq!(dialog.resolve_conflict(), Some("file.save"));
        assert_eq!(dialog.prefs().keymap.shortcut("tool.brush"), Some(save));
        assert_eq!(dialog.prefs().keymap.shortcut("file.save"), None);
        assert!(dialog.pending_conflict().is_none());
    }

    #[test]
    fn cancelling_a_capture_leaves_the_keymap_alone() {
        let mut dialog = PreferencesDialog::default();
        let before = dialog.prefs().keymap.clone();
        dialog.begin_capture("tool.brush");
        dialog.cancel_capture();
        assert_eq!(dialog.capturing(), None);
        assert!(dialog.capture(Shortcut::plain(Key::Z)).is_err());
        assert_eq!(&dialog.prefs().keymap, &before);
    }

    #[test]
    fn restore_defaults_resets_every_section() {
        let mut dialog = PreferencesDialog::default();
        dialog.prefs_mut().interface.ui_scale = 1.5;
        dialog.prefs_mut().performance.gpu_acceleration = false;
        dialog.prefs_mut().keymap.clear("file.save");
        assert!(dialog.is_modified());
        dialog.restore_defaults();
        assert_eq!(dialog.prefs(), &UiPreferences::default());
        assert!(!dialog.is_modified());
    }

    #[test]
    fn confirm_hands_back_sanitized_preferences() {
        let mut dialog = PreferencesDialog::default();
        dialog.prefs_mut().interface.ui_scale = 99.0;
        match dialog.confirm() {
            Some(DialogAction::SetPreferences(prefs)) => {
                assert_eq!(prefs.interface.ui_scale, *UI_SCALE_RANGE.end());
                assert!(prefs.is_sane());
            }
            other => panic!("expected preferences, got {other:?}"),
        }
    }

    #[test]
    fn a_conflicting_keymap_blocks_saving_and_says_so() {
        let mut dialog = PreferencesDialog::default();
        dialog
            .prefs_mut()
            .keymap
            .bindings
            .insert("tool.brush", Some(Shortcut::ctrl(Key::S)));
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().unwrap().contains("used twice"));
    }

    #[test]
    fn cancel_produces_nothing() {
        let dialog = PreferencesDialog::default();
        assert_eq!(
            super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn every_section_draws_in_both_appearances() {
        for section in PrefsSection::ALL {
            frame_both_themes(|ctx| {
                let mut dialog = PreferencesDialog::default();
                dialog.set_section(section);
                dialog.prefs_mut().scratch.disks.push(ScratchDisk {
                    path: "D:/scratch".to_string(),
                    enabled: true,
                });
                dialog.prefs_mut().scratch.disks.push(ScratchDisk {
                    path: "E:/scratch".to_string(),
                    enabled: false,
                });
                assert!(dialog.show(ctx).is_open());
            });
        }
    }

    #[test]
    fn the_keymap_editor_draws_its_conflict_banner() {
        frame_both_themes(|ctx| {
            let mut dialog = PreferencesDialog::default();
            dialog.set_section(PrefsSection::Keymap);
            dialog.begin_capture("tool.brush");
            let _ = dialog.capture(Shortcut::ctrl(Key::S));
            assert!(dialog.show(ctx).is_open());
        });
    }
}
