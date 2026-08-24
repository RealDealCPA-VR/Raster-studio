//! The keymap: key chords in, [`Action`]s out.
//!
//! One table serves the key handler, the menu bar (which renders the chord next
//! to the item) and the shortcut editor, so the three cannot disagree about
//! what `Ctrl+S` does.
//!
//! # Layers
//!
//! A keymap is a *default table* plus a *user layer*. Only the user layer is
//! persisted ([`Keymap::overrides`]), so a build that changes a default gives
//! every user the new default without wiping their customisations. An override
//! with no action unbinds the chord outright, which is how a user removes a
//! default they keep hitting by accident.
//!
//! # Conflicts
//!
//! A conflict is one chord naming two different actions. The effective map is
//! conflict-free by construction — an override replaces the chord's whole
//! entry — so conflicts are detected at the two places they can actually
//! arise:
//!
//! * [`conflicts`] over a raw binding list, which is how the default table is
//!   validated (there is a test) and how an *imported* keymap file is checked;
//! * [`Keymap::bind`], which refuses to give a chord a second meaning and
//!   reports what it would have stolen, so the UI can ask before
//!   [`Keymap::force_bind`] takes it.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::action::{Action, ToolKey};

/// A key, independent of layout modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// A printable character, normalised to lower case for letters.
    Char(char),
    Tab,
    Space,
    Enter,
    Escape,
    Backspace,
    Delete,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    /// `F1`..=`F24`.
    Function(u8),
}

impl Key {
    /// A printable character key, normalising letter case.
    pub fn character(c: char) -> Key {
        Key::Char(c.to_ascii_lowercase())
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Char(c) => write!(f, "{}", c.to_ascii_uppercase()),
            Key::Tab => f.write_str("Tab"),
            Key::Space => f.write_str("Space"),
            Key::Enter => f.write_str("Enter"),
            Key::Escape => f.write_str("Escape"),
            Key::Backspace => f.write_str("Backspace"),
            Key::Delete => f.write_str("Delete"),
            Key::ArrowLeft => f.write_str("Left"),
            Key::ArrowRight => f.write_str("Right"),
            Key::ArrowUp => f.write_str("Up"),
            Key::ArrowDown => f.write_str("Down"),
            Key::Function(n) => write!(f, "F{n}"),
        }
    }
}

/// A chord failed to parse. Carries the text so the message can name it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{text}` is not a key chord: {reason}")]
pub struct ChordParseError {
    pub text: String,
    pub reason: &'static str,
}

impl FromStr for Key {
    type Err = ChordParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = |reason| ChordParseError {
            text: s.to_string(),
            reason,
        };
        Ok(match s {
            "Tab" => Key::Tab,
            "Space" => Key::Space,
            "Enter" => Key::Enter,
            "Escape" => Key::Escape,
            "Backspace" => Key::Backspace,
            "Delete" => Key::Delete,
            "Left" => Key::ArrowLeft,
            "Right" => Key::ArrowRight,
            "Up" => Key::ArrowUp,
            "Down" => Key::ArrowDown,
            other => {
                if let Some(n) = other.strip_prefix('F') {
                    if let Ok(n) = n.parse::<u8>() {
                        if (1..=24).contains(&n) {
                            return Ok(Key::Function(n));
                        }
                        return Err(bad("function keys run from F1 to F24"));
                    }
                }
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Key::character(c),
                    _ => return Err(bad("expected a single character or a named key")),
                }
            }
        })
    }
}

/// Modifiers + a key. `ctrl_or_cmd` is one flag on purpose: the same binding is
/// Ctrl on Windows/Linux and Command on macOS, and splitting them would double
/// every table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub ctrl_or_cmd: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl Chord {
    /// A chord with no modifiers.
    pub const fn plain(key: Key) -> Chord {
        Chord {
            ctrl_or_cmd: false,
            alt: false,
            shift: false,
            key,
        }
    }

    pub const fn ctrl(key: Key) -> Chord {
        Chord {
            ctrl_or_cmd: true,
            alt: false,
            shift: false,
            key,
        }
    }

    pub const fn ctrl_shift(key: Key) -> Chord {
        Chord {
            ctrl_or_cmd: true,
            alt: false,
            shift: true,
            key,
        }
    }

    /// A letter chord, from the raw character the platform reported.
    pub fn letter(c: char) -> Chord {
        Chord::plain(Key::character(c))
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl_or_cmd {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        write!(f, "{}", self.key)
    }
}

impl FromStr for Chord {
    type Err = ChordParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = |reason| ChordParseError {
            text: s.to_string(),
            reason,
        };
        if s.is_empty() {
            return Err(bad("a chord cannot be empty"));
        }
        let parts: Vec<&str> = s.split('+').collect();
        // The separator is also a key. `+` splits to ["", ""] and `Ctrl++` to
        // ["Ctrl", "", ""]: *two* trailing empty tokens mean the key is a plus
        // sign. One trailing empty token (`Ctrl+`) is a stray separator, and
        // must stay an error — reading it as the plus key would silently drop
        // the modifier.
        let n = parts.len();
        let plus_key = n >= 2 && parts[n - 1].is_empty() && parts[n - 2].is_empty();
        let (mods, key_text) = if plus_key {
            (&parts[..n - 2], "+")
        } else {
            (&parts[..n - 1], parts[n - 1])
        };
        if key_text.is_empty() {
            return Err(bad("a chord must end with a key"));
        }
        let mut chord = Chord::plain(key_text.parse::<Key>()?);
        for m in mods {
            match *m {
                "Ctrl" | "Cmd" | "Control" | "Command" => chord.ctrl_or_cmd = true,
                "Alt" | "Option" => chord.alt = true,
                "Shift" => chord.shift = true,
                "" => return Err(bad("a chord must not contain an empty modifier")),
                _ => return Err(bad("unknown modifier (expected Ctrl, Alt or Shift)")),
            }
        }
        Ok(chord)
    }
}

impl Serialize for Chord {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// One entry of a keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub chord: Chord,
    pub action: Action,
}

/// One chord that names more than one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub chord: Chord,
    /// Every action the chord was given, in the order they appeared.
    pub actions: Vec<Action>,
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self.actions.iter().map(|a| a.label()).collect();
        write!(f, "{} is bound to {}", self.chord, names.join(" and "))
    }
}

/// Every chord in `bindings` that names more than one distinct action.
pub fn conflicts(bindings: &[Binding]) -> Vec<Conflict> {
    let mut by_chord: BTreeMap<Chord, Vec<Action>> = BTreeMap::new();
    for b in bindings {
        let slot = by_chord.entry(b.chord).or_default();
        if !slot.contains(&b.action) {
            slot.push(b.action);
        }
    }
    by_chord
        .into_iter()
        .filter(|(_, actions)| actions.len() > 1)
        .map(|(chord, actions)| Conflict { chord, actions })
        .collect()
}

/// A user's change to the default table. `action: None` unbinds the chord.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyOverride {
    pub chord: Chord,
    /// Serialized as [`Action::id`]; an id this build does not know is dropped
    /// on load rather than failing the file.
    #[serde(default, with = "action_id")]
    pub action: Option<Action>,
}

mod action_id {
    use super::Action;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(a: &Option<Action>, s: S) -> Result<S::Ok, S::Error> {
        a.map(|a| a.id()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Action>, D::Error> {
        let id = Option::<String>::deserialize(d)?;
        Ok(id.as_deref().and_then(Action::from_id))
    }
}

/// Chord → action, with a persisted user layer over a built-in default table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    /// Effective map. Rebuilt from the defaults plus [`Keymap::overrides`]
    /// whenever the user layer changes, so it can never drift from it.
    effective: BTreeMap<Chord, Action>,
    overrides: Vec<KeyOverride>,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut map = Keymap {
            effective: BTreeMap::new(),
            overrides: Vec::new(),
        };
        map.rebuild();
        map
    }
}

impl Keymap {
    /// The built-in table: the Photoshop/Photopea set this application ships.
    ///
    /// Returned as a list rather than a map so [`conflicts`] can see a mistake
    /// in it; `default_table_has_no_conflicts` is what keeps it honest.
    pub fn defaults() -> Vec<Binding> {
        use Action::*;
        let mut out = Vec::new();
        let mut add = |chord: Chord, action: Action| out.push(Binding { chord, action });

        // File
        add(Chord::ctrl(Key::character('n')), NewDocument);
        add(Chord::ctrl(Key::character('o')), Open);
        // A package is a directory, so it needs a picker — and therefore a
        // chord — of its own.
        add(
            Chord {
                ctrl_or_cmd: true,
                alt: true,
                shift: false,
                key: Key::character('o'),
            },
            OpenProject,
        );
        add(Chord::ctrl(Key::character('s')), Save);
        add(Chord::ctrl_shift(Key::character('s')), SaveAs);
        add(Chord::ctrl_shift(Key::character('e')), Export);
        add(Chord::ctrl(Key::character('w')), CloseDocument);
        add(Chord::ctrl(Key::character('q')), Quit);
        // Edit
        add(Chord::ctrl(Key::character('z')), Undo);
        add(Chord::ctrl_shift(Key::character('z')), Redo);
        add(Chord::ctrl(Key::character('y')), Redo);
        add(Chord::ctrl(Key::character('k')), ShowPreferences);
        add(Chord::ctrl_shift(Key::character('i')), ShowFileInfo);
        // Layer
        add(Chord::ctrl_shift(Key::character('n')), NewLayer);
        add(Chord::ctrl_shift(Key::Delete), DeleteLayer);
        add(Chord::ctrl(Key::character('j')), DuplicateLayer);
        add(Chord::ctrl(Key::character(',')), ToggleLayerVisibility);
        // View
        add(Chord::ctrl(Key::character('=')), ZoomIn);
        // On most layouts `+` is Shift+`=`, so the platform reports the shifted
        // character *and* the shift flag. Both spellings are bound, or Ctrl+Plus
        // would silently do nothing on the key it is named after.
        add(Chord::ctrl(Key::character('+')), ZoomIn);
        add(Chord::ctrl_shift(Key::character('+')), ZoomIn);
        add(Chord::ctrl_shift(Key::character('=')), ZoomIn);
        add(Chord::ctrl(Key::character('-')), ZoomOut);
        add(Chord::ctrl(Key::character('0')), ZoomFit);
        add(Chord::ctrl(Key::character('1')), ZoomActualPixels);
        add(Chord::plain(Key::Tab), TogglePanels);
        // Painting / colour
        add(Chord::plain(Key::character('[')), DecreaseBrushSize);
        add(Chord::plain(Key::character(']')), IncreaseBrushSize);
        add(Chord::plain(Key::character('x')), SwapColors);
        add(Chord::plain(Key::character('d')), ResetColors);
        add(Chord::plain(Key::Space), TemporaryHand);
        // Window
        add(Chord::ctrl(Key::Tab), NextDocument);
        add(Chord::ctrl_shift(Key::Tab), PreviousDocument);
        // Tools: one letter per registry cycle group.
        for key in ToolKey::all() {
            add(Chord::plain(Key::Char(key.char())), SelectTool(key));
        }
        out
    }

    /// A keymap with the given user layer applied over the defaults.
    pub fn with_overrides(overrides: Vec<KeyOverride>) -> Self {
        let mut map = Keymap {
            effective: BTreeMap::new(),
            overrides,
        };
        map.rebuild();
        map
    }

    fn rebuild(&mut self) {
        self.effective = Self::defaults()
            .into_iter()
            .map(|b| (b.chord, b.action))
            .collect();
        for o in &self.overrides {
            match o.action {
                Some(action) => {
                    self.effective.insert(o.chord, action);
                }
                None => {
                    self.effective.remove(&o.chord);
                }
            }
        }
    }

    /// The action `chord` performs, if any.
    pub fn resolve(&self, chord: &Chord) -> Option<Action> {
        self.effective.get(chord).copied()
    }

    /// Every chord that performs `action`, in display order.
    pub fn chords_for(&self, action: Action) -> Vec<Chord> {
        self.effective
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(c, _)| *c)
            .collect()
    }

    /// The chord to show next to `action` in a menu.
    pub fn shortcut_for(&self, action: Action) -> Option<Chord> {
        self.chords_for(action).into_iter().next()
    }

    /// The effective map as a binding list.
    pub fn bindings(&self) -> Vec<Binding> {
        self.effective
            .iter()
            .map(|(&chord, &action)| Binding { chord, action })
            .collect()
    }

    /// The user layer — the only part that is persisted.
    pub fn overrides(&self) -> &[KeyOverride] {
        &self.overrides
    }

    /// Bind `chord` to `action`, refusing to steal it from another action.
    pub fn bind(&mut self, chord: Chord, action: Action) -> Result<(), Conflict> {
        if let Some(existing) = self.resolve(&chord) {
            if existing != action {
                return Err(Conflict {
                    chord,
                    actions: vec![existing, action],
                });
            }
        }
        self.force_bind(chord, action);
        Ok(())
    }

    /// Bind `chord` to `action` even if it already meant something else.
    pub fn force_bind(&mut self, chord: Chord, action: Action) {
        self.overrides.retain(|o| o.chord != chord);
        self.overrides.push(KeyOverride {
            chord,
            action: Some(action),
        });
        self.rebuild();
    }

    /// Remove `chord`'s meaning, default included.
    pub fn unbind(&mut self, chord: Chord) {
        self.overrides.retain(|o| o.chord != chord);
        self.overrides.push(KeyOverride {
            chord,
            action: None,
        });
        self.rebuild();
    }

    /// Drop the user layer.
    pub fn reset(&mut self) {
        self.overrides.clear();
        self.rebuild();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_round_trip_through_text() {
        let cases = [
            "Ctrl+Z",
            "Ctrl+Shift+Z",
            "Ctrl+Alt+Shift+S",
            "Tab",
            "Ctrl+Tab",
            "Space",
            "[",
            "]",
            "Ctrl+=",
            "Ctrl++",
            "+",
            "F5",
            "Ctrl+Shift+Delete",
            "Left",
        ];
        for text in cases {
            let chord: Chord = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(chord.to_string(), text, "display must re-parse");
            assert_eq!(text.parse::<Chord>().unwrap(), chord);
        }
    }

    #[test]
    fn bad_chords_are_refused_with_a_reason() {
        for text in ["", "Ctrl+", "Meta+Z", "Ctrl++Z", "F0", "F99", "abc"] {
            let err = text.parse::<Chord>().unwrap_err();
            assert!(!err.reason.is_empty(), "{text} had no reason");
            assert!(err.to_string().contains(text) || text.is_empty());
        }
    }

    #[test]
    fn letter_case_does_not_change_the_key() {
        assert_eq!(Chord::letter('B'), Chord::letter('b'));
        assert_eq!("B".parse::<Chord>().unwrap(), Chord::letter('b'));
    }

    #[test]
    fn default_table_has_no_conflicts() {
        let table = Keymap::defaults();
        let found = conflicts(&table);
        assert!(
            found.is_empty(),
            "default keymap conflicts: {}",
            found
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    #[test]
    fn every_action_has_at_least_one_default_binding() {
        // Wave 3's shell had NewLayer and DeleteLayer with no key at all.
        let map = Keymap::default();
        let missing: Vec<String> = Action::all()
            .into_iter()
            .filter(|a| map.chords_for(*a).is_empty())
            .map(|a| a.id())
            .collect();
        assert!(missing.is_empty(), "actions with no binding: {missing:?}");
    }

    #[test]
    fn a_conflicting_binding_is_detected_rather_than_silently_taken() {
        let mut map = Keymap::default();
        let ctrl_s = Chord::ctrl(Key::character('s'));
        assert_eq!(map.resolve(&ctrl_s), Some(Action::Save));

        let err = map.bind(ctrl_s, Action::Export).unwrap_err();
        assert_eq!(err.chord, ctrl_s);
        assert_eq!(err.actions, vec![Action::Save, Action::Export]);
        assert!(err.to_string().contains("Save"));
        assert_eq!(
            map.resolve(&ctrl_s),
            Some(Action::Save),
            "a refused bind must change nothing"
        );

        // Re-binding a chord to the action it already has is not a conflict.
        map.bind(ctrl_s, Action::Save).unwrap();
        assert_eq!(map.resolve(&ctrl_s), Some(Action::Save));

        // And taking it deliberately works.
        map.force_bind(ctrl_s, Action::Export);
        assert_eq!(map.resolve(&ctrl_s), Some(Action::Export));
        assert!(
            conflicts(&map.bindings()).is_empty(),
            "the effective map is always conflict-free"
        );
    }

    #[test]
    fn conflicts_are_reported_for_an_imported_list() {
        let chord = Chord::ctrl(Key::character('k'));
        let list = vec![
            Binding {
                chord,
                action: Action::Save,
            },
            Binding {
                chord,
                action: Action::Export,
            },
            Binding {
                chord,
                action: Action::Save,
            },
            Binding {
                chord: Chord::ctrl(Key::character('l')),
                action: Action::Open,
            },
        ];
        let found = conflicts(&list);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].chord, chord);
        assert_eq!(
            found[0].actions,
            vec![Action::Save, Action::Export],
            "a repeated identical binding is not a conflict"
        );
    }

    #[test]
    fn the_user_layer_overrides_and_unbinds() {
        let mut map = Keymap::default();
        let tab = Chord::plain(Key::Tab);
        assert_eq!(map.resolve(&tab), Some(Action::TogglePanels));

        map.unbind(tab);
        assert_eq!(map.resolve(&tab), None);
        assert_eq!(map.overrides().len(), 1);

        map.force_bind(tab, Action::ZoomFit);
        assert_eq!(map.resolve(&tab), Some(Action::ZoomFit));
        assert_eq!(map.overrides().len(), 1, "one override per chord");

        map.reset();
        assert_eq!(map.resolve(&tab), Some(Action::TogglePanels));
        assert!(map.overrides().is_empty());
    }

    #[test]
    fn the_user_layer_survives_a_json_round_trip() {
        let mut map = Keymap::default();
        map.force_bind(Chord::ctrl(Key::character('k')), Action::Export);
        map.unbind(Chord::plain(Key::Tab));

        let json = serde_json::to_string(map.overrides()).unwrap();
        let back: Vec<KeyOverride> = serde_json::from_str(&json).unwrap();
        let restored = Keymap::with_overrides(back);
        assert_eq!(restored, map);
        assert_eq!(
            restored.resolve(&Chord::ctrl(Key::character('k'))),
            Some(Action::Export)
        );
        assert_eq!(restored.resolve(&Chord::plain(Key::Tab)), None);
    }

    #[test]
    fn an_override_naming_an_unknown_action_is_dropped_not_fatal() {
        let json = r#"[{"chord":"Ctrl+K","action":"action-from-the-future"}]"#;
        let back: Vec<KeyOverride> = serde_json::from_str(json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].action, None, "unknown ids read as 'unbound'");
    }

    #[test]
    fn tool_letters_reach_the_registry() {
        let map = Keymap::default();
        for key in ToolKey::all() {
            assert_eq!(
                map.resolve(&Chord::letter(key.char())),
                Some(Action::SelectTool(key)),
                "{key} does not select a tool"
            );
        }
    }

    /// Letters the brief for this shell names: `V/M/L/W/B/E/G/T/P/Z/H`.
    const BRIEF_TOOL_LETTERS: [char; 11] = ['v', 'm', 'l', 'w', 'b', 'e', 'g', 't', 'p', 'z', 'h'];

    /// Letters of that set `tools::registry` has no tool for.
    ///
    /// **Empty, and it was not.** `P` — the pen/path group — was the one letter
    /// the registry could not answer, so pressing it was a no-op and the
    /// palette had no pen button. `tools::pen::PenTool` now answers to it and
    /// `tools::text::TypeTool` shares `T` with Free Transform, so every letter
    /// the brief names reaches a tool. Kept as a list rather than deleted: it
    /// is the shape the assertion below needs, and the next letter that goes
    /// missing has somewhere to be recorded.
    const KNOWN_MISSING_TOOL_LETTERS: [char; 0] = [];

    #[test]
    fn the_briefs_tool_letters_are_present_except_the_ones_recorded_as_missing() {
        let mut missing: Vec<char> = BRIEF_TOOL_LETTERS
            .into_iter()
            .filter(|c| ToolKey::new(*c).is_none())
            .collect();
        missing.sort_unstable();
        let mut known = KNOWN_MISSING_TOOL_LETTERS.to_vec();
        known.sort_unstable();
        assert_eq!(
            missing, known,
            "the set of brief letters the registry cannot answer changed; \
             update KNOWN_MISSING_TOOL_LETTERS (or add the tool)"
        );

        // Every letter that *is* present must reach the registry through a
        // binding, so the gap above is the only one.
        let map = Keymap::default();
        for c in BRIEF_TOOL_LETTERS {
            let Some(key) = ToolKey::new(c) else { continue };
            assert_eq!(
                map.resolve(&Chord::letter(c)),
                Some(Action::SelectTool(key)),
                "{c} names a tool group but is not bound"
            );
        }
        // And the missing one really is unbound rather than silently meaning
        // something else.
        for c in KNOWN_MISSING_TOOL_LETTERS {
            assert_eq!(
                map.resolve(&Chord::letter(c)),
                None,
                "{c} has no tool, so it must not be bound to anything"
            );
        }
    }
}
