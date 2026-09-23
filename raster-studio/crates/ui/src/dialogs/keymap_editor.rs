//! The keymap editor's model: a command list the host supplies, a default
//! chord table, and the effective chord table the user edits.
//!
//! The dialog crate does not know the application's actions — it cannot, the
//! dependency runs the other way — so the editor is *generic*: the host hands
//! in every bindable command (a stable id, a category, a label), the shipped
//! chord for each, and the chords in force, and takes back the edited table.
//! [`Keymap::changes`] is the diff against the defaults, which is exactly the
//! shape of the host's persisted user layer: a chord bound to something other
//! than its default, or a default chord unbound.
//!
//! The table is a map from chord to command, so it is conflict-free by
//! construction: a chord means one thing. A conflict is therefore an *event*,
//! not a state — [`Keymap::assign`] refuses to give a chord a second meaning
//! and names the owner, and the dialog makes the user decide.

use std::collections::BTreeMap;

use egui::{Key, Modifiers};

/// A key plus its modifiers.
///
/// Ordered so it can key a `BTreeMap`; the ordering is arbitrary but total,
/// which is all that is asked of it.
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

/// One bindable command, as the host names it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeyCommand {
    /// Stable identifier; what the host's persisted layer keys on.
    pub id: String,
    /// Menu the command lives in, for grouping the editor.
    pub category: String,
    pub label: String,
}

impl KeyCommand {
    pub fn new(
        id: impl Into<String>,
        category: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category: category.into(),
            label: label.into(),
        }
    }
}

/// Why a shortcut could not be assigned.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeymapError {
    /// The command id is not one the host listed.
    UnknownCommand,
    /// Another command already owns that shortcut.
    Conflict { held_by: String },
}

impl std::fmt::Display for KeymapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCommand => f.write_str(crate::strings::tr("ui.keymap.no.such.command")),
            Self::Conflict { held_by } => {
                write!(
                    f,
                    "{} {held_by}",
                    crate::strings::tr("ui.keymap.already.used.by")
                )
            }
        }
    }
}

/// One difference between the edited table and the defaults — the host's
/// persisted user layer, one entry at a time.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyChange {
    /// `shortcut` now triggers `command`, which is not what it did by default.
    Bound { shortcut: Shortcut, command: String },
    /// `shortcut` had a default meaning and now has none.
    Unbound { shortcut: Shortcut },
}

/// The whole keyboard map: every chord that means something, and what.
///
/// Equality is over the table itself, not over [`Keymap::was_reset`]: a table
/// put back to the defaults *is* the default table, which is what the
/// dialog's "modified" state asks.
#[derive(Clone, Eq, Debug, Default)]
pub struct Keymap {
    /// Display order.
    commands: Vec<KeyCommand>,
    /// The shipped table.
    defaults: BTreeMap<Shortcut, String>,
    /// The table in force, as edited.
    bindings: BTreeMap<Shortcut, String>,
    /// Chords something *outside* this table answers — the host's menu items
    /// that are not bindable commands here (Ctrl+A Select All, Ctrl+E Merge
    /// Down). Chord → the label of what holds it. [`Keymap::assign`] treats
    /// them as taken, so binding one is a conflict the user decides, not a
    /// silent steal of the shortcut the menu paints.
    reserved: BTreeMap<Shortcut, String>,
    /// Whether [`Keymap::reset`] ran on this table. The host's user layer can
    /// hold entries [`Keymap::changes`] never reports (an unbound menu-only
    /// chord); it keeps them across an OK unless the user asked for the
    /// shipped table back.
    was_reset: bool,
}

impl PartialEq for Keymap {
    fn eq(&self, other: &Self) -> bool {
        self.commands == other.commands
            && self.defaults == other.defaults
            && self.bindings == other.bindings
            && self.reserved == other.reserved
    }
}

impl Keymap {
    /// A keymap over `commands`, with the shipped `defaults` and the
    /// `bindings` currently in force. A chord naming a command that is not
    /// listed is dropped rather than shown as a row nothing can explain.
    pub fn new(
        commands: Vec<KeyCommand>,
        defaults: impl IntoIterator<Item = (Shortcut, String)>,
        bindings: impl IntoIterator<Item = (Shortcut, String)>,
    ) -> Self {
        let known = |id: &String| commands.iter().any(|c| c.id == *id);
        let defaults: BTreeMap<Shortcut, String> =
            defaults.into_iter().filter(|(_, id)| known(id)).collect();
        let bindings: BTreeMap<Shortcut, String> =
            bindings.into_iter().filter(|(_, id)| known(id)).collect();
        Self {
            commands,
            defaults,
            bindings,
            reserved: BTreeMap::new(),
            was_reset: false,
        }
    }

    /// Declare the chords something outside this table already answers,
    /// each with the label of what holds it. A reserved chord this table
    /// itself ships a default for is dropped: the table's own meaning wins.
    pub fn with_reserved(mut self, reserved: impl IntoIterator<Item = (Shortcut, String)>) -> Self {
        self.reserved = reserved
            .into_iter()
            .filter(|(shortcut, _)| !self.defaults.contains_key(shortcut))
            .collect();
        self
    }

    /// What outside this table answers `shortcut`, when nothing in the table
    /// does — the label [`Keymap::assign`] reports a conflict with.
    pub fn reserved_by(&self, shortcut: Shortcut) -> Option<&str> {
        if self.bindings.contains_key(&shortcut) {
            return None;
        }
        self.reserved.get(&shortcut).map(String::as_str)
    }

    /// A keymap whose table in force is the shipped one.
    pub fn from_defaults(
        commands: Vec<KeyCommand>,
        defaults: impl IntoIterator<Item = (Shortcut, String)>,
    ) -> Self {
        let defaults: Vec<(Shortcut, String)> = defaults.into_iter().collect();
        Self::new(commands, defaults.clone(), defaults)
    }

    /// Every command, in display order.
    pub fn commands(&self) -> &[KeyCommand] {
        &self.commands
    }

    /// The command with `id`.
    pub fn command(&self, id: &str) -> Option<&KeyCommand> {
        self.commands.iter().find(|c| c.id == id)
    }

    /// The label of `id`, or the id itself for one the host did not list.
    pub fn label_of(&self, id: &str) -> String {
        self.command(id)
            .map_or_else(|| id.to_string(), |c| c.label.clone())
    }

    /// Every chord that triggers `id`, in table order.
    pub fn shortcuts(&self, id: &str) -> Vec<Shortcut> {
        self.bindings
            .iter()
            .filter(|(_, bound)| bound.as_str() == id)
            .map(|(s, _)| *s)
            .collect()
    }

    /// The first chord that triggers `id`, if any.
    pub fn shortcut(&self, id: &str) -> Option<Shortcut> {
        self.shortcuts(id).into_iter().next()
    }

    /// The shipped chords for `id`.
    pub fn default_shortcuts(&self, id: &str) -> Vec<Shortcut> {
        self.defaults
            .iter()
            .filter(|(_, bound)| bound.as_str() == id)
            .map(|(s, _)| *s)
            .collect()
    }

    /// The command a shortcut currently triggers.
    pub fn command_for(&self, shortcut: Shortcut) -> Option<&str> {
        self.bindings.get(&shortcut).map(String::as_str)
    }

    /// The table in force: chord → command id.
    pub fn bindings(&self) -> impl Iterator<Item = (Shortcut, &str)> {
        self.bindings.iter().map(|(s, id)| (*s, id.as_str()))
    }

    /// Bind `shortcut` to `id`.
    ///
    /// Refuses rather than stealing: a shortcut already owned by another
    /// command comes back as [`KeymapError::Conflict`] naming the owner, and
    /// nothing changes. Re-assigning a command a shortcut it already has
    /// succeeds and is a no-op.
    pub fn assign(&mut self, id: &str, shortcut: Shortcut) -> Result<(), KeymapError> {
        if self.command(id).is_none() {
            return Err(KeymapError::UnknownCommand);
        }
        if let Some(owner) = self.command_for(shortcut) {
            if owner != id {
                return Err(KeymapError::Conflict {
                    held_by: owner.to_string(),
                });
            }
            return Ok(());
        }
        if let Some(holder) = self.reserved_by(shortcut) {
            return Err(KeymapError::Conflict {
                held_by: holder.to_string(),
            });
        }
        self.bindings.insert(shortcut, id.to_string());
        Ok(())
    }

    /// Bind `shortcut` to `id`, taking it away from whoever had it.
    ///
    /// Returns the command that lost it. The dialog only calls this after the
    /// user has been told what they are about to displace.
    pub fn force_assign(&mut self, id: &str, shortcut: Shortcut) -> Option<String> {
        self.command(id)?;
        let displaced = self
            .command_for(shortcut)
            .filter(|owner| *owner != id)
            .or_else(|| self.reserved_by(shortcut))
            .map(str::to_string);
        self.bindings.insert(shortcut, id.to_string());
        displaced
    }

    /// Remove one chord's meaning. `false` when it had none.
    pub fn unbind(&mut self, shortcut: Shortcut) -> bool {
        self.bindings.remove(&shortcut).is_some()
    }

    /// Remove every chord of a command. `false` for a command not listed.
    pub fn clear(&mut self, id: &str) -> bool {
        if self.command(id).is_none() {
            return false;
        }
        self.bindings.retain(|_, bound| bound != id);
        true
    }

    /// Put every chord back to the shipped table.
    pub fn reset(&mut self) {
        self.bindings = self.defaults.clone();
        self.was_reset = true;
    }

    /// Whether the user put the table back to the shipped defaults
    /// ([`Keymap::reset`]) during this edit.
    pub fn was_reset(&self) -> bool {
        self.was_reset
    }

    /// Whether `id` triggers on a different set of chords than it shipped with.
    pub fn is_customized(&self, id: &str) -> bool {
        self.shortcuts(id) != self.default_shortcuts(id)
    }

    /// Whether anything differs from the shipped table.
    pub fn is_default(&self) -> bool {
        self.bindings == self.defaults
    }

    /// Chords that name more than one command.
    ///
    /// Always empty: the table is keyed by chord, so a chord cannot hold two
    /// meanings — [`Keymap::assign`] refuses instead. Kept as a query so a
    /// confirmed keymap is validated the same way any other dialog result is.
    pub fn conflicts(&self) -> Vec<(Shortcut, Vec<String>)> {
        Vec::new()
    }

    /// The edited table as a diff against the defaults, in chord order.
    pub fn changes(&self) -> Vec<KeyChange> {
        let mut out = Vec::new();
        for (shortcut, command) in &self.bindings {
            if self.defaults.get(shortcut) != Some(command) {
                out.push(KeyChange::Bound {
                    shortcut: *shortcut,
                    command: command.clone(),
                });
            }
        }
        for shortcut in self.defaults.keys() {
            if !self.bindings.contains_key(shortcut) {
                out.push(KeyChange::Unbound {
                    shortcut: *shortcut,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small host table: enough shape to exercise every operation.
    pub(crate) fn sample() -> Keymap {
        Keymap::from_defaults(
            vec![
                KeyCommand::new("save", "File", "Save"),
                KeyCommand::new("undo", "Edit", "Undo"),
                KeyCommand::new("brush", "Tools", "Brush"),
                KeyCommand::new("unbound", "Tools", "Has no key"),
            ],
            [
                (Shortcut::ctrl(Key::S), "save".to_string()),
                (Shortcut::ctrl(Key::Z), "undo".to_string()),
                // Two chords for one command, as Undo really has.
                (
                    Shortcut {
                        key: Key::Z,
                        ctrl: true,
                        shift: false,
                        alt: true,
                    },
                    "undo".to_string(),
                ),
                (Shortcut::plain(Key::B), "brush".to_string()),
            ],
        )
    }

    #[test]
    fn a_chord_held_outside_the_table_is_a_conflict_not_a_silent_steal() {
        // Ctrl+A is not a bindable command here, but the host's menu answers
        // it (Select All). Binding it must be refused, naming the holder.
        let mut keymap = sample().with_reserved([
            (Shortcut::ctrl(Key::A), "Select All".to_string()),
            // A reserved chord the table itself ships is the table's.
            (Shortcut::ctrl(Key::S), "Menu Save".to_string()),
        ]);
        assert_eq!(
            keymap.reserved_by(Shortcut::ctrl(Key::A)),
            Some("Select All")
        );
        assert_eq!(keymap.reserved_by(Shortcut::ctrl(Key::S)), None);
        assert_eq!(
            keymap.assign("brush", Shortcut::ctrl(Key::A)),
            Err(KeymapError::Conflict {
                held_by: "Select All".to_string()
            })
        );
        assert!(keymap.is_default(), "a refused assignment changes nothing");
        // The user's explicit decision takes it, and says what it displaced.
        assert_eq!(
            keymap.force_assign("brush", Shortcut::ctrl(Key::A)),
            Some("Select All".to_string())
        );
        assert_eq!(keymap.command_for(Shortcut::ctrl(Key::A)), Some("brush"));
    }

    #[test]
    fn a_fresh_keymap_is_the_shipped_table_and_has_no_changes() {
        let keymap = sample();
        assert!(keymap.is_default());
        assert!(keymap.changes().is_empty());
        assert_eq!(keymap.shortcuts("undo").len(), 2);
        assert_eq!(keymap.shortcut("unbound"), None);
        for command in keymap.commands() {
            assert!(!keymap.is_customized(&command.id), "{}", command.id);
        }
    }

    #[test]
    fn a_chord_naming_an_unlisted_command_is_dropped_on_the_way_in() {
        let keymap = Keymap::new(
            vec![KeyCommand::new("save", "File", "Save")],
            [(Shortcut::ctrl(Key::S), "save".to_string())],
            [
                (Shortcut::ctrl(Key::S), "save".to_string()),
                (Shortcut::ctrl(Key::Q), "from-the-future".to_string()),
            ],
        );
        assert_eq!(keymap.command_for(Shortcut::ctrl(Key::Q)), None);
        assert_eq!(keymap.bindings().count(), 1);
    }

    #[test]
    fn a_shortcut_already_in_use_is_refused_and_names_its_owner() {
        let mut keymap = sample();
        let save = Shortcut::ctrl(Key::S);
        assert_eq!(keymap.command_for(save), Some("save"));
        let error = keymap.assign("brush", save).unwrap_err();
        assert_eq!(
            error,
            KeymapError::Conflict {
                held_by: "save".to_string()
            }
        );
        // And nothing moved.
        assert_eq!(keymap.command_for(save), Some("save"));
        assert_eq!(keymap.shortcuts("brush"), vec![Shortcut::plain(Key::B)]);
        assert!(keymap.changes().is_empty());
    }

    #[test]
    fn reassigning_a_command_its_own_shortcut_is_a_no_op() {
        let mut keymap = sample();
        assert!(keymap.assign("save", Shortcut::ctrl(Key::S)).is_ok());
        assert!(keymap.is_default());
    }

    #[test]
    fn a_free_shortcut_binds_and_shows_up_as_a_change() {
        let mut keymap = sample();
        let free = Shortcut::ctrl_shift(Key::K);
        assert_eq!(keymap.command_for(free), None);
        assert!(keymap.assign("brush", free).is_ok());
        // Brush now has two chords: its default and the new one.
        assert_eq!(
            keymap.shortcuts("brush"),
            vec![Shortcut::plain(Key::B), free]
        );
        assert!(keymap.is_customized("brush"));
        assert_eq!(
            keymap.changes(),
            vec![KeyChange::Bound {
                shortcut: free,
                command: "brush".to_string()
            }]
        );
    }

    #[test]
    fn an_unknown_command_cannot_be_bound() {
        let mut keymap = sample();
        assert_eq!(
            keymap.assign("nope", Shortcut::plain(Key::Q)),
            Err(KeymapError::UnknownCommand)
        );
        assert!(!keymap.clear("nope"));
        assert_eq!(keymap.force_assign("nope", Shortcut::plain(Key::Q)), None);
        assert!(keymap.is_default());
    }

    #[test]
    fn forcing_an_assignment_takes_the_chord_from_the_previous_owner() {
        let mut keymap = sample();
        let save = Shortcut::ctrl(Key::S);
        assert_eq!(keymap.force_assign("brush", save), Some("save".to_string()));
        assert_eq!(keymap.command_for(save), Some("brush"));
        assert_eq!(keymap.shortcuts("save"), Vec::<Shortcut>::new());
        // The diff says exactly that: Ctrl+S is Brush now, and Save's default
        // chord is gone — one Bound entry, no Unbound one, because the chord
        // still means something.
        assert_eq!(
            keymap.changes(),
            vec![KeyChange::Bound {
                shortcut: save,
                command: "brush".to_string()
            }]
        );
    }

    #[test]
    fn unbinding_a_default_chord_is_recorded_as_unbound() {
        let mut keymap = sample();
        let b = Shortcut::plain(Key::B);
        assert!(keymap.unbind(b));
        assert!(!keymap.unbind(b), "already gone");
        assert_eq!(keymap.shortcut("brush"), None);
        assert!(keymap.is_customized("brush"));
        assert_eq!(keymap.changes(), vec![KeyChange::Unbound { shortcut: b }]);
    }

    #[test]
    fn clearing_a_command_removes_every_one_of_its_chords_and_reset_restores_them() {
        let mut keymap = sample();
        assert!(keymap.clear("undo"));
        assert!(keymap.shortcuts("undo").is_empty());
        assert_eq!(keymap.changes().len(), 2, "both Undo chords are unbound");
        keymap.reset();
        assert!(keymap.is_default());
        assert_eq!(keymap.shortcuts("undo").len(), 2);
    }

    #[test]
    fn the_changes_are_the_diff_and_nothing_else() {
        let mut keymap = sample();
        // A chord moved from one command to another, a new chord bound, and a
        // default removed: three facts, three entries.
        let save = Shortcut::ctrl(Key::S);
        let k = Shortcut::ctrl(Key::K);
        keymap.force_assign("brush", save);
        keymap.assign("save", k).unwrap();
        keymap.unbind(Shortcut::plain(Key::B));
        let changes = keymap.changes();
        assert_eq!(changes.len(), 3, "{changes:?}");
        assert!(changes.contains(&KeyChange::Bound {
            shortcut: save,
            command: "brush".into()
        }));
        assert!(changes.contains(&KeyChange::Bound {
            shortcut: k,
            command: "save".into()
        }));
        assert!(changes.contains(&KeyChange::Unbound {
            shortcut: Shortcut::plain(Key::B)
        }));
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
    fn the_conflict_error_names_the_owner() {
        let error = KeymapError::Conflict {
            held_by: "save".to_string(),
        };
        assert!(error.to_string().ends_with("save"));
        assert!(!KeymapError::UnknownCommand.to_string().is_empty());
    }
}
