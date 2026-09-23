//! Preferences.
//!
//! Six sections down the left, the chosen one on the right. The settings
//! model here is the UI's own — plain data with a [`UiPreferences::sanitized`]
//! pass — so the dialog can be exercised without a shell, a disk or a window.
//! The shell maps it onto whatever it persists.
//!
//! # Every control has a consumer
//!
//! A preference nothing reads is a lie the dialog tells: the user flips it,
//! nothing changes, and there is no way to tell that from a bug. So the
//! controls are an enumeration, [`PrefControl`], the sections draw *from* it
//! (a control that is not in the list is not drawn), and the shell's tests
//! iterate the same list against a table of what each one changes. Three
//! sections this dialog used to have — Tools' brush cursor, Performance and a
//! scratch-disk priority list — went away when that table was written,
//! because nothing consumed them.
//!
//! # The keymap editor
//!
//! The one part worth being careful about: a shortcut that is silently taken
//! from another command is a bug the user only discovers later, so
//! [`Keymap::assign`] reports the collision instead of resolving it, and the
//! dialog makes the user decide. The model is generic — the host lists the
//! commands and their chords ([`keymap_editor`]) and takes the diff back.

use design::{tokens::Space, Theme};
use egui::{Context, Key};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, warning, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::controls::{checkbox_row, combo, integer, numeric, sidebar_list};
use super::sizes;
use super::units::Unit;
use crate::strings::{tr, Locale};

#[path = "keymap_editor.rs"]
pub mod keymap_editor;
pub use keymap_editor::{KeyChange, KeyCommand, Keymap, KeymapError, Shortcut};

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

/// General behaviour.
#[derive(Clone, PartialEq, Debug)]
pub struct GeneralPrefs {
    /// Minutes between autosaves; `0` turns autosave off.
    pub autosave_minutes: u32,
}

impl Default for GeneralPrefs {
    fn default() -> Self {
        Self {
            autosave_minutes: 10,
        }
    }
}

/// Appearance, language and measurement.
#[derive(Clone, PartialEq, Debug)]
pub struct InterfacePrefs {
    pub theme: ThemeChoice,
    /// Multiplier on every point in the design system.
    pub ui_scale: f32,
    /// The catalogue locale [`crate::strings::tr`] resolves in.
    pub language: Locale,
    /// The measurement unit the rulers and the size readouts use.
    pub units: Unit,
}

impl Default for InterfacePrefs {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            ui_scale: 1.0,
            language: Locale::En,
            units: Unit::Pixels,
        }
    }
}

/// Tool behaviour.
#[derive(Clone, PartialEq, Debug)]
pub struct ToolPrefs {
    /// A plain wheel zooms. Off, the plain wheel pans and Ctrl+wheel zooms.
    pub scroll_wheel_zooms: bool,
}

impl Default for ToolPrefs {
    fn default() -> Self {
        Self {
            scroll_wheel_zooms: true,
        }
    }
}

/// Undo history.
#[derive(Clone, PartialEq, Debug)]
pub struct HistoryPrefs {
    /// How many undo steps are kept.
    pub states: u32,
}

impl Default for HistoryPrefs {
    fn default() -> Self {
        Self { states: 100 }
    }
}

/// Where working files go.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ScratchPrefs {
    /// The directory autosaves of never-saved documents are written to. Empty
    /// means the application's default location.
    pub dir: String,
}

impl ScratchPrefs {
    /// The directory as a path, or `None` for the default.
    pub fn dir(&self) -> Option<&str> {
        let dir = self.dir.trim();
        (!dir.is_empty()).then_some(dir)
    }
}

/// Every preference the UI owns.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct UiPreferences {
    pub general: GeneralPrefs,
    pub interface: InterfacePrefs,
    pub tools: ToolPrefs,
    pub history: HistoryPrefs,
    pub scratch: ScratchPrefs,
    pub keymap: Keymap,
    /// The section the dialog opens on. Not a setting: the host sets it to
    /// [`PrefsSection::Keymap`] for Edit ▸ Keyboard Shortcuts…, and the dialog
    /// never writes it.
    pub page: PrefsSection,
}

/// Smallest and largest UI scale the interface stays usable at.
pub const UI_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.75..=2.0;
/// Largest number of undo states the dialog will accept.
pub const MAX_HISTORY_STATES: u32 = 1000;
/// Longest autosave period the dialog will accept, in minutes.
pub const MAX_AUTOSAVE_MINUTES: u32 = 24 * 60;

impl UiPreferences {
    /// Force every field into a range the app can actually run with.
    ///
    /// A preferences file is user-editable, so this is the boundary that stops
    /// a hand-typed `ui_scale: 0` from making the app unusable with no way back.
    pub fn sanitized(mut self) -> Self {
        self.general.autosave_minutes = self.general.autosave_minutes.min(MAX_AUTOSAVE_MINUTES);
        self.interface.ui_scale = if self.interface.ui_scale.is_finite() {
            self.interface
                .ui_scale
                .clamp(*UI_SCALE_RANGE.start(), *UI_SCALE_RANGE.end())
        } else {
            1.0
        };
        if !Unit::PREFERENCE_CHOICES.contains(&self.interface.units) {
            self.interface.units = Unit::Pixels;
        }
        self.history.states = self.history.states.clamp(1, MAX_HISTORY_STATES);
        self.scratch.dir = self.scratch.dir.trim().to_string();
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

// ------------------------------------------------------------------ controls

/// Every control the dialog draws, each exactly once.
///
/// The sections draw from this list, and the shell's
/// `every_preference_control_has_a_consumer` iterates it against a table of
/// what each one changes — so a control cannot be added here without a
/// consumer, and cannot be drawn without being here.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PrefControl {
    Autosave,
    Theme,
    UiScale,
    Language,
    Units,
    ScrollWheelZooms,
    HistoryStates,
    ScratchDir,
    Keymap,
}

impl PrefControl {
    /// All, in drawing order.
    pub const ALL: [PrefControl; 9] = [
        Self::Autosave,
        Self::Theme,
        Self::UiScale,
        Self::Language,
        Self::Units,
        Self::ScrollWheelZooms,
        Self::HistoryStates,
        Self::ScratchDir,
        Self::Keymap,
    ];

    /// Stable identifier, for the consumer table and the egui ids.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Autosave => "autosave",
            Self::Theme => "theme",
            Self::UiScale => "ui-scale",
            Self::Language => "language",
            Self::Units => "units",
            Self::ScrollWheelZooms => "scroll-wheel-zooms",
            Self::HistoryStates => "history-states",
            Self::ScratchDir => "scratch-dir",
            Self::Keymap => "keymap",
        }
    }

    /// The section the control is drawn in.
    pub const fn section(self) -> PrefsSection {
        match self {
            Self::Autosave => PrefsSection::General,
            Self::Theme | Self::UiScale | Self::Language | Self::Units => PrefsSection::Interface,
            Self::ScrollWheelZooms => PrefsSection::Tools,
            Self::HistoryStates => PrefsSection::History,
            Self::ScratchDir => PrefsSection::Scratch,
            Self::Keymap => PrefsSection::Keymap,
        }
    }

    /// The controls of one section, in drawing order.
    pub fn in_section(section: PrefsSection) -> impl Iterator<Item = PrefControl> {
        Self::ALL
            .into_iter()
            .filter(move |c| c.section() == section)
    }

    /// Change the setting this control edits to something it is not now — the
    /// edit a consumer test applies and expects to see land.
    ///
    /// `false` when no different value exists: a language list of one, or a
    /// keymap with nothing to bind. The caller decides whether that is
    /// acceptable for the control in question.
    pub fn mutate(self, prefs: &mut UiPreferences) -> bool {
        match self {
            Self::Autosave => {
                let now = prefs.general.autosave_minutes;
                prefs.general.autosave_minutes = if now == 7 { 8 } else { 7 };
            }
            Self::Theme => {
                prefs.interface.theme = if prefs.interface.theme == ThemeChoice::Light {
                    ThemeChoice::Dark
                } else {
                    ThemeChoice::Light
                };
            }
            Self::UiScale => {
                prefs.interface.ui_scale = if prefs.interface.ui_scale == 1.5 {
                    1.25
                } else {
                    1.5
                };
            }
            Self::Language => {
                let Some(other) = Locale::ALL
                    .iter()
                    .copied()
                    .find(|l| *l != prefs.interface.language)
                else {
                    return false;
                };
                prefs.interface.language = other;
            }
            Self::Units => {
                prefs.interface.units = if prefs.interface.units == Unit::Centimeters {
                    Unit::Inches
                } else {
                    Unit::Centimeters
                };
            }
            Self::ScrollWheelZooms => {
                prefs.tools.scroll_wheel_zooms = !prefs.tools.scroll_wheel_zooms;
            }
            Self::HistoryStates => {
                let now = prefs.history.states;
                prefs.history.states = if now == 250 { 300 } else { 250 };
            }
            Self::ScratchDir => {
                prefs.scratch.dir = if prefs.scratch.dir.is_empty() {
                    "scratch-elsewhere".to_string()
                } else {
                    String::new()
                };
            }
            Self::Keymap => {
                let Some(first) = prefs.keymap.commands().first().map(|c| c.id.clone()) else {
                    return false;
                };
                // A chord no default table uses.
                let free = Shortcut {
                    key: Key::F12,
                    ctrl: true,
                    shift: true,
                    alt: true,
                };
                if prefs.keymap.command_for(free) == Some(first.as_str()) {
                    return prefs.keymap.unbind(free);
                }
                prefs.keymap.force_assign(&first, free);
            }
        }
        true
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
    Scratch,
    Keymap,
}

impl PrefsSection {
    /// All six, in sidebar order.
    pub const ALL: [PrefsSection; 6] = [
        Self::General,
        Self::Interface,
        Self::Tools,
        Self::History,
        Self::Scratch,
        Self::Keymap,
    ];

    /// Sidebar label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Interface => "Interface",
            Self::Tools => "Tools",
            Self::History => "History",
            Self::Scratch => "Scratch",
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
    capturing: Option<String>,
    /// The most recent refused assignment, shown in the editor.
    last_conflict: Option<(String, Shortcut, KeymapError)>,
}

impl Default for PreferencesDialog {
    fn default() -> Self {
        Self::new(UiPreferences::default())
    }
}

impl PreferencesDialog {
    /// Open on `prefs`, sanitised on the way in, showing [`UiPreferences::page`].
    pub fn new(prefs: UiPreferences) -> Self {
        let prefs = prefs.sanitized();
        Self {
            section: prefs.page,
            original: prefs.clone(),
            prefs,
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

    /// Restore every setting in *every* section to its default. The keymap's
    /// command list is the host's, not a setting: it stays, and every chord
    /// goes back to the shipped table.
    pub fn restore_defaults(&mut self) {
        let mut keymap = self.prefs.keymap.clone();
        keymap.reset();
        self.prefs = UiPreferences {
            keymap,
            page: self.prefs.page,
            ..UiPreferences::default()
        };
        self.last_conflict = None;
        self.capturing = None;
    }

    /// The command whose next keystroke will be captured.
    pub fn capturing(&self) -> Option<&str> {
        self.capturing.as_deref()
    }

    /// Start listening for a shortcut for `id`.
    pub fn begin_capture(&mut self, id: &str) {
        self.capturing = Some(id.to_string());
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
        let Some(id) = self.capturing.clone() else {
            return Err(KeymapError::UnknownCommand);
        };
        match self.prefs.keymap.assign(&id, shortcut) {
            Ok(()) => {
                self.capturing = None;
                self.last_conflict = None;
                Ok(())
            }
            Err(error) => {
                self.last_conflict = Some((id, shortcut, error.clone()));
                Err(error)
            }
        }
    }

    /// Take the pending shortcut away from whoever holds it and give it to the
    /// command that was refused.
    pub fn resolve_conflict(&mut self) -> Option<String> {
        let (id, shortcut, _) = self.last_conflict.take()?;
        self.capturing = None;
        self.prefs.keymap.force_assign(&id, shortcut)
    }

    /// The refused assignment still waiting on the user, if any.
    pub fn pending_conflict(&self) -> Option<&(String, Shortcut, KeymapError)> {
        self.last_conflict.as_ref()
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
                    .show(ui, |ui| {
                        let section = self.section;
                        if section != PrefsSection::Keymap {
                            design::section_header(ui, section.label());
                        }
                        for control in PrefControl::in_section(section) {
                            self.control(ui, control);
                        }
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

    /// Draw one control. Every arm edits exactly the field the control names,
    /// which is what makes [`PrefControl::mutate`] a fair stand-in for a click.
    fn control(&mut self, ui: &mut egui::Ui, control: PrefControl) {
        match control {
            PrefControl::Autosave => {
                design::inspector_field(ui, "Autosave", |ui| {
                    let mut minutes = i64::from(self.prefs.general.autosave_minutes);
                    if integer(ui, &mut minutes, 0..=i64::from(MAX_AUTOSAVE_MINUTES)).changed() {
                        self.prefs.general.autosave_minutes = minutes.max(0) as u32;
                    }
                    caption(ui, tr("ui.preferences.minutes.0.is.off"));
                });
                if self.prefs.general.autosave_minutes == 0 {
                    caption(
                        ui,
                        "Autosave is off: a crash loses everything since the last save.",
                    );
                }
            }
            PrefControl::Theme => {
                design::inspector_field(ui, "Theme", |ui| {
                    combo(
                        ui,
                        control.key(),
                        &mut self.prefs.interface.theme,
                        ThemeChoice::ALL,
                        |t| t.label().to_string(),
                        |_| None,
                    );
                });
            }
            PrefControl::UiScale => {
                design::inspector_field(ui, tr("ui.preferences.ui.scale"), |ui| {
                    let mut scale = f64::from(self.prefs.interface.ui_scale) * 100.0;
                    let lo = f64::from(*UI_SCALE_RANGE.start()) * 100.0;
                    let hi = f64::from(*UI_SCALE_RANGE.end()) * 100.0;
                    if numeric(ui, &mut scale, lo..=hi, 0, "%").changed() {
                        self.prefs.interface.ui_scale = (scale / 100.0) as f32;
                    }
                });
            }
            PrefControl::Language => {
                design::inspector_field(ui, tr("ui.preferences.language"), |ui| {
                    combo(
                        ui,
                        control.key(),
                        &mut self.prefs.interface.language,
                        Locale::ALL,
                        |l| l.display_name().to_string(),
                        |_| None,
                    );
                });
                if Locale::ALL.len() == 1 {
                    caption(ui, tr("ui.preferences.only.english"));
                }
            }
            PrefControl::Units => {
                design::inspector_field(ui, tr("ui.preferences.units"), |ui| {
                    combo(
                        ui,
                        control.key(),
                        &mut self.prefs.interface.units,
                        Unit::PREFERENCE_CHOICES,
                        |u| format!("{} ({})", u.label(), u.short()),
                        |_| None,
                    );
                });
                caption(ui, tr("ui.preferences.units.caption"));
            }
            PrefControl::ScrollWheelZooms => {
                checkbox_row(
                    ui,
                    tr("ui.preferences.scroll.wheel.zooms.instead.of.scrolling"),
                    &mut self.prefs.tools.scroll_wheel_zooms,
                );
                caption(ui, tr("ui.preferences.scroll.wheel.caption"));
            }
            PrefControl::HistoryStates => {
                design::inspector_field(ui, "States", |ui| {
                    let mut states = i64::from(self.prefs.history.states);
                    if integer(ui, &mut states, 1..=i64::from(MAX_HISTORY_STATES)).changed() {
                        self.prefs.history.states = states.max(1) as u32;
                    }
                });
            }
            PrefControl::ScratchDir => {
                design::inspector_field(ui, tr("ui.preferences.scratch.directory"), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.prefs.scratch.dir)
                            .id_salt(control.key())
                            .desired_width(sizes::text_field_path()),
                    );
                });
                caption(ui, tr("ui.preferences.scratch.caption"));
            }
            PrefControl::Keymap => self.keymap(ui),
        }
    }

    fn keymap(&mut self, ui: &mut egui::Ui) {
        if let Some((id, shortcut, error)) = self.last_conflict.clone() {
            let label = self.prefs.keymap.label_of(&id);
            let reason = match &error {
                KeymapError::Conflict { held_by } => format!(
                    "{} {}",
                    tr("ui.keymap.already.used.by"),
                    self.prefs.keymap.label_of(held_by)
                ),
                other => other.to_string(),
            };
            warning(ui, format!("{label}: {} — {reason}", shortcut.display()));
            ui.horizontal(|ui| {
                if design::primary_button(ui, tr("ui.preferences.reassign.anyway")).clicked() {
                    self.resolve_conflict();
                }
                if design::secondary_button(ui, tr("ui.preferences.keep.as.it.was")).clicked() {
                    self.last_conflict = None;
                    self.capturing = None;
                }
            });
            hairline(ui);
        }
        let commands = self.prefs.keymap.commands().to_vec();
        if commands.is_empty() {
            caption(ui, tr("ui.preferences.no.commands"));
        }
        let mut category = String::new();
        for command in &commands {
            if command.category != category {
                category = command.category.clone();
                design::section_header(ui, &category);
            }
            let capturing = self.capturing.as_deref() == Some(command.id.as_str());
            let shortcuts = self.prefs.keymap.shortcuts(&command.id);
            let customized = self.prefs.keymap.is_customized(&command.id);
            let mut unbind: Option<Shortcut> = None;
            let mut add = false;
            design::inspector_field(ui, &command.label, |ui| {
                for shortcut in &shortcuts {
                    if design::secondary_button(ui, &shortcut.display())
                        .on_hover_text(tr("ui.preferences.remove.this.shortcut"))
                        .clicked()
                    {
                        unbind = Some(*shortcut);
                    }
                }
                let add_label = if capturing {
                    tr("ui.preferences.press.a.key")
                } else {
                    tr("ui.preferences.add.shortcut")
                };
                if design::ghost_button(ui, add_label).clicked() {
                    add = true;
                }
                if customized {
                    caption(ui, tr("ui.preferences.changed"));
                }
            });
            if let Some(shortcut) = unbind {
                self.prefs.keymap.unbind(shortcut);
            }
            if add {
                if capturing {
                    self.cancel_capture();
                } else {
                    self.begin_capture(&command.id);
                }
            }
        }
        ui.add_space(Space::Small.pt());
        if design::secondary_button(ui, tr("ui.preferences.reset.all.shortcuts")).clicked() {
            self.prefs.keymap.reset();
            self.last_conflict = None;
            self.capturing = None;
        }
    }
}

/// The first key pressed this frame, with its modifiers — what the shortcut
/// field captures.
///
/// Escape and Enter are skipped: they belong to the dialog grammar, and a
/// keymap editor that lets the user bind Escape traps them in it.
pub fn read_shortcut(ctx: &Context) -> Option<Shortcut> {
    // The modifiers the key event itself carries: the frame-level set is the
    // state at the *end* of the frame and can miss a chord pressed and
    // released inside it.
    ctx.input(|input| {
        input.events.iter().find_map(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
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
        Some(DialogAction::SetPreferences(Box::new(
            self.prefs.clone().sanitized(),
        )))
    }

    fn blocked_reason(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::frame_both_themes;

    /// Preferences over a small host keymap, as the shell would open them.
    fn with_keymap() -> UiPreferences {
        UiPreferences {
            keymap: Keymap::from_defaults(
                vec![
                    KeyCommand::new("save", "File", "Save"),
                    KeyCommand::new("undo", "Edit", "Undo"),
                    KeyCommand::new("brush", "Tools", "Brush"),
                ],
                [
                    (Shortcut::ctrl(Key::S), "save".to_string()),
                    (Shortcut::ctrl(Key::Z), "undo".to_string()),
                    (Shortcut::plain(Key::B), "brush".to_string()),
                ],
            ),
            ..UiPreferences::default()
        }
    }

    #[test]
    fn the_defaults_are_already_sane() {
        assert!(UiPreferences::default().is_sane());
        assert!(
            UiPreferences::default().tools.scroll_wheel_zooms,
            "a plain wheel zooms by default"
        );
    }

    #[test]
    fn sanitizing_repairs_every_out_of_range_field() {
        let mut prefs = UiPreferences::default();
        prefs.interface.ui_scale = 0.0;
        prefs.interface.units = Unit::Picas;
        prefs.history.states = 0;
        prefs.general.autosave_minutes = 100_000;
        prefs.scratch.dir = "  D:/scratch  ".to_string();
        let fixed = prefs.sanitized();
        assert_eq!(fixed.interface.ui_scale, *UI_SCALE_RANGE.start());
        assert_eq!(
            fixed.interface.units,
            Unit::Picas,
            "picas are a units choice the rulers offer; sanitizing keeps them"
        );
        assert_eq!(fixed.history.states, 1);
        assert_eq!(fixed.general.autosave_minutes, MAX_AUTOSAVE_MINUTES);
        assert_eq!(fixed.scratch.dir, "D:/scratch");
        assert!(fixed.is_sane());
    }

    #[test]
    fn a_non_finite_setting_falls_back_to_its_default_not_to_a_boundary() {
        // Clamping a NaN keeps the NaN, and clamping an infinity would silently
        // turn "this field is corrupt" into "the user asked for the maximum".
        let mut prefs = UiPreferences::default();
        prefs.interface.ui_scale = f32::NAN;
        assert_eq!(prefs.clone().sanitized().interface.ui_scale, 1.0);
        prefs.interface.ui_scale = f32::INFINITY;
        assert_eq!(prefs.sanitized().interface.ui_scale, 1.0);
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
    fn a_blank_scratch_directory_means_the_default() {
        let mut scratch = ScratchPrefs::default();
        assert_eq!(scratch.dir(), None);
        scratch.dir = "   ".to_string();
        assert_eq!(scratch.dir(), None);
        scratch.dir = " D:/scratch ".to_string();
        assert_eq!(scratch.dir(), Some("D:/scratch"));
    }

    #[test]
    fn the_theme_choice_resolves_the_way_the_menu_promises() {
        assert_eq!(ThemeChoice::Light.resolve(Theme::Dark), Theme::Light);
        assert_eq!(ThemeChoice::Dark.resolve(Theme::Light), Theme::Dark);
        assert_eq!(ThemeChoice::System.resolve(Theme::Light), Theme::Light);
        assert_eq!(ThemeChoice::System.resolve(Theme::Dark), Theme::Dark);
    }

    /// The dialog draws from [`PrefControl::ALL`]; this pins that the list is
    /// coherent — every control sits in a real section, every non-keymap
    /// section has something to draw, and every control's stand-in edit is a
    /// visible modification. The shell's side of the same table is
    /// `every_preference_control_has_a_consumer`.
    #[test]
    fn every_control_sits_in_a_section_and_its_edit_is_visible() {
        let mut keys: Vec<&str> = PrefControl::ALL.iter().map(|c| c.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(
            keys.len(),
            PrefControl::ALL.len(),
            "two controls share a key"
        );
        for section in PrefsSection::ALL {
            assert!(
                PrefControl::in_section(section).next().is_some(),
                "{section:?} draws nothing"
            );
        }
        for control in PrefControl::ALL {
            assert!(PrefsSection::ALL.contains(&control.section()));
            let mut dialog = PreferencesDialog::new(with_keymap());
            let changed = control.mutate(dialog.prefs_mut());
            if control == PrefControl::Language && Locale::ALL.len() == 1 {
                assert!(!changed, "one locale: there is nothing to change to");
                continue;
            }
            assert!(changed, "{control:?} had nothing to change");
            assert!(dialog.is_modified(), "{control:?}'s edit is invisible");
            // And it is a real edit of the *sanitized* model: confirm keeps it.
            match dialog.confirm() {
                Some(DialogAction::SetPreferences(prefs)) => {
                    assert_ne!(*prefs, with_keymap().sanitized(), "{control:?}")
                }
                other => panic!("expected preferences, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_dialog_opens_on_the_requested_page() {
        let dialog = PreferencesDialog::default();
        assert_eq!(dialog.section(), PrefsSection::General);
        let prefs = UiPreferences {
            page: PrefsSection::Keymap,
            ..UiPreferences::default()
        };
        let dialog = PreferencesDialog::new(prefs);
        assert_eq!(dialog.section(), PrefsSection::Keymap);
        assert!(!dialog.is_modified(), "the page is not an edit");
    }

    #[test]
    fn the_capture_flow_binds_a_free_shortcut_and_closes() {
        let mut dialog = PreferencesDialog::new(with_keymap());
        dialog.begin_capture("brush");
        assert_eq!(dialog.capturing(), Some("brush"));
        assert!(dialog.capture(Shortcut::ctrl_shift(Key::K)).is_ok());
        assert_eq!(dialog.capturing(), None);
        assert_eq!(
            dialog.prefs().keymap.shortcuts("brush"),
            vec![Shortcut::plain(Key::B), Shortcut::ctrl_shift(Key::K)]
        );
        assert!(dialog.is_modified());
    }

    #[test]
    fn the_capture_flow_refuses_a_taken_shortcut_and_offers_a_way_out() {
        let mut dialog = PreferencesDialog::new(with_keymap());
        dialog.begin_capture("brush");
        let save = Shortcut::ctrl(Key::S);
        assert_eq!(
            dialog.capture(save),
            Err(KeymapError::Conflict {
                held_by: "save".to_string()
            })
        );
        // Nothing moved yet, and the dialog knows what it refused.
        assert_eq!(dialog.prefs().keymap.command_for(save), Some("save"));
        assert!(!dialog.is_modified());
        let (id, shortcut, _) = dialog.pending_conflict().expect("a recorded conflict");
        assert_eq!(id, "brush");
        assert_eq!(*shortcut, save);
        // Then the user decides.
        assert_eq!(dialog.resolve_conflict(), Some("save".to_string()));
        assert_eq!(dialog.prefs().keymap.command_for(save), Some("brush"));
        assert_eq!(
            dialog.prefs().keymap.shortcuts("save"),
            Vec::<Shortcut>::new()
        );
        assert!(dialog.pending_conflict().is_none());
        assert_eq!(dialog.capturing(), None);
    }

    #[test]
    fn cancelling_a_capture_leaves_the_keymap_alone() {
        let mut dialog = PreferencesDialog::new(with_keymap());
        let before = dialog.prefs().keymap.clone();
        dialog.begin_capture("brush");
        dialog.cancel_capture();
        assert_eq!(dialog.capturing(), None);
        assert!(dialog.capture(Shortcut::plain(Key::Z)).is_err());
        assert_eq!(&dialog.prefs().keymap, &before);
    }

    #[test]
    fn restore_defaults_resets_every_section_but_keeps_the_host_command_list() {
        let mut prefs = with_keymap();
        prefs.page = PrefsSection::Keymap;
        let mut dialog = PreferencesDialog::new(prefs);
        dialog.prefs_mut().interface.ui_scale = 1.5;
        dialog.prefs_mut().interface.units = Unit::Centimeters;
        dialog.prefs_mut().tools.scroll_wheel_zooms = false;
        dialog.prefs_mut().keymap.unbind(Shortcut::ctrl(Key::S));
        assert!(dialog.is_modified());
        dialog.restore_defaults();
        assert!(!dialog.is_modified());
        assert_eq!(dialog.prefs().keymap.commands().len(), 3);
        assert!(dialog.prefs().keymap.is_default());
        assert_eq!(dialog.prefs().page, PrefsSection::Keymap);
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
    fn cancel_produces_nothing() {
        let dialog = PreferencesDialog::default();
        assert_eq!(
            super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    /// W3-X: the Units control on the Interface page lists Picas, and
    /// picking it there, by clicking the drawn combo and then the drawn row,
    /// lands Picas in the preferences. Read off what egui painted, not off
    /// the choices constant.
    #[test]
    fn the_units_control_offers_picas_and_a_click_on_it_sets_them() {
        use crate::dialogs::chrome::test_support::Harness;
        let h = Harness::new();
        let mut dialog = PreferencesDialog::new(UiPreferences::default());
        dialog.set_section(PrefsSection::Interface);
        let text_rect = |h: &Harness, dialog: &mut PreferencesDialog, text: &str| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Harness::SCREEN)),
                ..Default::default()
            };
            let output = h.ctx.run(input, |ctx| {
                let _ = dialog.show(ctx);
            });
            output
                .shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(t) if t.galley.text() == text => {
                        Some(egui::Rect::from_min_size(t.pos, t.galley.size()))
                    }
                    _ => None,
                })
        };
        let pixels = format!("{} ({})", Unit::Pixels.label(), Unit::Pixels.short());
        let picas = format!("{} ({})", Unit::Picas.label(), Unit::Picas.short());
        let mut combo = None;
        for _ in 0..Harness::STABLE_FRAMES {
            combo = text_rect(&h, &mut dialog, &pixels);
        }
        let combo = combo.expect("the Units combo shows the current unit");
        h.frame(Harness::click_events(combo.center()), |ctx| {
            let _ = dialog.show(ctx);
        });
        let mut row = None;
        for _ in 0..Harness::STABLE_FRAMES {
            row = text_rect(&h, &mut dialog, &picas);
        }
        let row = row.expect("the open Units list draws a Picas row");
        h.frame(Harness::click_events(row.center()), |ctx| {
            let _ = dialog.show(ctx);
        });
        assert_eq!(dialog.prefs().interface.units, Unit::Picas);
    }

    #[test]
    fn every_section_draws_in_both_appearances() {
        for section in PrefsSection::ALL {
            frame_both_themes(|ctx| {
                let mut dialog = PreferencesDialog::new(with_keymap());
                dialog.set_section(section);
                dialog.prefs_mut().scratch.dir = "D:/scratch".to_string();
                assert!(dialog.show(ctx).is_open());
            });
        }
    }

    #[test]
    fn the_keymap_editor_draws_its_conflict_banner() {
        frame_both_themes(|ctx| {
            let mut dialog = PreferencesDialog::new(with_keymap());
            dialog.set_section(PrefsSection::Keymap);
            dialog.begin_capture("brush");
            let _ = dialog.capture(Shortcut::ctrl(Key::S));
            assert!(dialog.pending_conflict().is_some());
            assert!(dialog.show(ctx).is_open());
        });
    }

    #[test]
    fn a_frame_while_capturing_binds_the_key_pressed() {
        frame_both_themes(|ctx| {
            // frame_both_themes gives one frame with no events; the capture
            // stays open across it and the model is untouched.
            let mut dialog = PreferencesDialog::new(with_keymap());
            dialog.set_section(PrefsSection::Keymap);
            dialog.begin_capture("brush");
            assert!(dialog.show(ctx).is_open());
            assert_eq!(dialog.capturing(), Some("brush"));
        });
        // With a key event in the frame the capture completes.
        let ctx = Context::default();
        let mut dialog = PreferencesDialog::new(with_keymap());
        dialog.set_section(PrefsSection::Keymap);
        dialog.begin_capture("brush");
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::K,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            }],
            modifiers: egui::Modifiers::CTRL,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            assert!(dialog.show(ctx).is_open());
        });
        assert_eq!(dialog.capturing(), None);
        assert_eq!(
            dialog.prefs().keymap.command_for(Shortcut::ctrl(Key::K)),
            Some("brush")
        );
    }
}
