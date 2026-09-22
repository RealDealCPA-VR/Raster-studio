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
//! # The menu table underneath
//!
//! The menu bar paints a chord beside far more items than [`Action`] names —
//! Merge Down, Select All, Free Transform, the F5–F8 panel toggles — from
//! `ui::menu`'s own shortcut table. Until this layer existed only the table
//! above was ever consulted, so every one of those painted chords was dead,
//! and three of them did something *else* (Ctrl+Shift+I opened File Info
//! where the menu promised Select ▸ Inverse). [`Keymap::resolve_any`] closes
//! the gap by construction: a chord the application's own table does not
//! claim is looked up in the menu table, and the caller dispatches the
//! [`ui::MenuAction`] through the same door a menu click takes.
//! `every_painted_menu_chord_resolves_to_its_own_menu_action` walks the whole
//! menu vocabulary to prove the two tables agree.
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
use ui::menu::{MenuAction, ZoomCommand};

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

    pub const fn ctrl_alt(key: Key) -> Chord {
        Chord {
            ctrl_or_cmd: true,
            alt: true,
            shift: false,
            key,
        }
    }

    pub const fn ctrl_alt_shift(key: Key) -> Chord {
        Chord {
            ctrl_or_cmd: true,
            alt: true,
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
    /// The menu bar's own shortcut table, reversed: chord → the
    /// [`MenuAction`] whose painted shortcut it is. Consulted by
    /// [`Keymap::resolve_any`] for a chord `effective` does not claim. Built
    /// once from [`MenuAction::all`] rather than walked per key press.
    menu: BTreeMap<Chord, MenuAction>,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut map = Keymap {
            effective: BTreeMap::new(),
            overrides: Vec::new(),
            menu: menu_table(),
        };
        map.rebuild();
        map
    }
}

/// What a chord means once both tables have been consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// The application's own table claims the chord: perform the [`Action`].
    App(Action),
    /// Only the menu bar paints this chord: dispatch the [`MenuAction`] the
    /// way a click on its item would be.
    Menu(MenuAction),
}

/// The key a menu shortcut names, in this keymap's vocabulary.
///
/// `None` for a key the platform layer never reports as a chord here (Home,
/// End, Page Up/Down): no menu item paints one, and a table entry that could
/// never be pressed would be a promise this module cannot keep.
fn key_of_menu_key(key: ui::Key) -> Option<Key> {
    Some(match key {
        ui::Key::Char(c) => Key::character(c),
        ui::Key::F(n) => Key::Function(n),
        ui::Key::Delete => Key::Delete,
        ui::Key::Backspace => Key::Backspace,
        ui::Key::Enter => Key::Enter,
        ui::Key::Escape => Key::Escape,
        ui::Key::Tab => Key::Tab,
        ui::Key::Space => Key::Space,
        ui::Key::Left => Key::ArrowLeft,
        ui::Key::Right => Key::ArrowRight,
        ui::Key::Up => Key::ArrowUp,
        ui::Key::Down => Key::ArrowDown,
        ui::Key::Home | ui::Key::End | ui::Key::PageUp | ui::Key::PageDown => return None,
        // The `+`/`=` key: the platform reports the unshifted character.
        ui::Key::Plus => Key::Char('='),
        ui::Key::Minus => Key::Char('-'),
        ui::Key::LeftBracket => Key::Char('['),
        ui::Key::RightBracket => Key::Char(']'),
        ui::Key::Comma => Key::Char(','),
        ui::Key::Period => Key::Char('.'),
        ui::Key::Semicolon => Key::Char(';'),
        ui::Key::Quote => Key::Char('\''),
        ui::Key::Slash => Key::Char('/'),
        ui::Key::Backslash => Key::Char('\\'),
        ui::Key::Grave => Key::Char('`'),
    })
}

/// The menu bar's key for one of this keymap's keys — the inverse of
/// [`key_of_menu_key`].
fn menu_key_of_key(key: Key) -> Option<ui::Key> {
    Some(match key {
        Key::Char('=') | Key::Char('+') => ui::Key::Plus,
        Key::Char('-') => ui::Key::Minus,
        Key::Char('[') => ui::Key::LeftBracket,
        Key::Char(']') => ui::Key::RightBracket,
        Key::Char(',') => ui::Key::Comma,
        Key::Char('.') => ui::Key::Period,
        Key::Char(';') => ui::Key::Semicolon,
        Key::Char('\'') => ui::Key::Quote,
        Key::Char('/') => ui::Key::Slash,
        Key::Char('\\') => ui::Key::Backslash,
        Key::Char('`') => ui::Key::Grave,
        Key::Char(c) => ui::Key::character(c),
        Key::Tab => ui::Key::Tab,
        Key::Space => ui::Key::Space,
        Key::Enter => ui::Key::Enter,
        Key::Escape => ui::Key::Escape,
        Key::Backspace => ui::Key::Backspace,
        Key::Delete => ui::Key::Delete,
        Key::ArrowLeft => ui::Key::Left,
        Key::ArrowRight => ui::Key::Right,
        Key::ArrowUp => ui::Key::Up,
        Key::ArrowDown => ui::Key::Down,
        // The menu vocabulary stops at F12.
        Key::Function(n) if (1..=12).contains(&n) => ui::Key::F(n),
        Key::Function(_) => return None,
    })
}

/// The chord the platform reports for a menu shortcut.
pub fn chord_of_shortcut(shortcut: ui::Shortcut) -> Option<Chord> {
    Some(Chord {
        ctrl_or_cmd: shortcut.ctrl,
        alt: shortcut.alt,
        shift: shortcut.shift,
        key: key_of_menu_key(shortcut.key)?,
    })
}

/// A chord as the menu bar would paint it.
pub fn shortcut_of_chord(chord: &Chord) -> Option<ui::Shortcut> {
    Some(ui::Shortcut {
        ctrl: chord.ctrl_or_cmd,
        alt: chord.alt,
        shift: chord.shift,
        key: menu_key_of_key(chord.key)?,
    })
}

/// Every painted menu shortcut, as (chord, action) pairs in
/// [`MenuAction::all`] order. Kept as a list so a chord two items claim is
/// visible; `no_two_painted_menu_chords_disagree` is the test that reads it.
pub fn menu_bindings() -> Vec<(Chord, MenuAction)> {
    MenuAction::all()
        .into_iter()
        .filter_map(|action| Some((chord_of_shortcut(action.shortcut()?)?, action)))
        .collect()
}

fn menu_table() -> BTreeMap<Chord, MenuAction> {
    let mut out = BTreeMap::new();
    for (chord, action) in menu_bindings() {
        // First one wins, which is the order the menu bar walks in
        // `ui::menu::action_for_shortcut`.
        out.entry(chord).or_insert(action);
    }
    out
}

/// The menu item that means the same thing as an application [`Action`].
///
/// The application's table and the menu table overlap on the chords every
/// editor has — Ctrl+S, Ctrl+Z, Ctrl+N — and the application's table wins those.
/// This is the equivalence that lets a test ask "does Ctrl+S still do what the
/// menu paints beside Save?" without the two enums having to be one. Matched
/// without a wildcard, so a new action must say whether it has a twin.
pub fn menu_twin(action: Action) -> Option<MenuAction> {
    Some(match action {
        Action::NewDocument => MenuAction::NewDocument,
        Action::Open => MenuAction::Open,
        Action::Save => MenuAction::Save,
        Action::SaveAs => MenuAction::SaveAs,
        Action::CloseDocument => MenuAction::CloseDocument,
        Action::CloseOthers => MenuAction::CloseOthers,
        Action::Quit => MenuAction::Quit,
        Action::Undo => MenuAction::Undo,
        Action::Redo => MenuAction::Redo,
        Action::ShowPreferences => MenuAction::Preferences,
        Action::ShowFileInfo => MenuAction::FileInfo,
        Action::Copy => MenuAction::Copy,
        Action::Cut => MenuAction::Cut,
        Action::Paste => MenuAction::Paste,
        Action::NewLayer => MenuAction::NewLayer,
        Action::DeleteLayer => MenuAction::DeleteLayer,
        Action::DuplicateLayer => MenuAction::DuplicateLayer,
        Action::ToggleLayerVisibility => MenuAction::ToggleLayerVisibility,
        Action::ZoomIn => MenuAction::Zoom(ZoomCommand::In),
        Action::ZoomOut => MenuAction::Zoom(ZoomCommand::Out),
        Action::ZoomFit => MenuAction::Zoom(ZoomCommand::FitOnScreen),
        Action::ZoomActualPixels => MenuAction::Zoom(ZoomCommand::ActualPixels),
        // A picker over every format; the menu lists one item per format.
        Action::Export
        | Action::OpenProject
        | Action::TogglePanels
        | Action::SelectTool(_)
        | Action::TemporaryHand
        | Action::DecreaseBrushSize
        | Action::IncreaseBrushSize
        | Action::SwapColors
        | Action::ResetColors
        | Action::NextDocument
        | Action::PreviousDocument => return None,
    })
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
        // Export As is Ctrl+Alt+Shift+S, as in Photopea. It *was* Ctrl+Shift+E,
        // which the Layer menu paints beside Merge Visible — so the chord the
        // menu promised merged nothing and opened an export picker instead.
        add(Chord::ctrl_alt_shift(Key::character('s')), Export);
        add(Chord::ctrl(Key::character('w')), CloseDocument);
        add(Chord::ctrl(Key::character('q')), Quit);
        // Edit
        add(Chord::ctrl(Key::character('z')), Undo);
        // Step Backward: the Photoshop alias for undo.
        add(Chord::ctrl_alt(Key::character('z')), Undo);
        // Step Forward.
        add(Chord::ctrl_shift(Key::character('z')), Redo);
        add(Chord::ctrl(Key::character('y')), Redo);
        // Card 052: the image clipboard is keyboard-reachable outside a text
        // session (a live text session consumes these chords first, card 028).
        add(Chord::ctrl(Key::character('c')), Copy);
        add(Chord::ctrl(Key::character('x')), Cut);
        add(Chord::ctrl(Key::character('v')), Paste);
        add(Chord::ctrl(Key::character('k')), ShowPreferences);
        // File Info is Ctrl+Alt+Shift+I, as in Photoshop. Ctrl+Shift+I is
        // Select ▸ Inverse in the menu table underneath, and this table used to
        // take it first.
        add(Chord::ctrl_alt_shift(Key::character('i')), ShowFileInfo);
        // Layer
        add(Chord::ctrl_shift(Key::character('n')), NewLayer);
        add(Chord::ctrl_shift(Key::Delete), DeleteLayer);
        // Ctrl+J is Layer via Copy in the menu table (and in Photoshop); this
        // table used to spend it on a plain duplicate. Duplicate Layer keeps a
        // chord of its own so it stays keyboard-reachable.
        add(Chord::ctrl_alt(Key::character('j')), DuplicateLayer);
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
            menu: menu_table(),
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

    /// The action `chord` performs, if any — the application's own table
    /// only. The key handler wants [`Keymap::resolve_any`]; this is for the
    /// places that ask about *this* table (the shortcut editor, the channel
    /// chords' "does the application already claim this?").
    pub fn resolve(&self, chord: &Chord) -> Option<Action> {
        self.effective.get(chord).copied()
    }

    /// What `chord` means, both tables consulted.
    ///
    /// The application's table wins, user layer included — a chord the user
    /// bound means what they bound it to, even if a menu item paints it. A
    /// chord the user *unbound* means nothing: an unbind is "this key does
    /// nothing", not "this key falls through to whatever the menu says".
    /// Otherwise the menu table answers.
    pub fn resolve_any(&self, chord: &Chord) -> Option<Resolved> {
        if let Some(action) = self.resolve(chord) {
            return Some(Resolved::App(action));
        }
        if self
            .overrides
            .iter()
            .any(|o| o.chord == *chord && o.action.is_none())
        {
            return None;
        }
        self.menu.get(chord).copied().map(Resolved::Menu)
    }

    /// The menu item `chord` reaches through [`Keymap::resolve_any`]'s
    /// fallback, ignoring the application's table.
    pub fn menu_action_for(&self, chord: &Chord) -> Option<MenuAction> {
        self.menu.get(chord).copied()
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

    /// The three chords the audit found doing something other than what the
    /// menu paints beside them, and where the displaced actions went.
    #[test]
    fn the_three_conflicting_chords_now_do_what_the_menu_paints() {
        let map = Keymap::default();
        let ctrl_shift = |c| Chord::ctrl_shift(Key::character(c));
        let ctrl = |c| Chord::ctrl(Key::character(c));
        let ctrl_alt = |c| Chord::ctrl_alt(Key::character(c));
        let ctrl_alt_shift = |c| Chord::ctrl_alt_shift(Key::character(c));

        // Was ShowFileInfo / Export / DuplicateLayer.
        assert_eq!(
            map.resolve_any(&ctrl_shift('i')),
            Some(Resolved::Menu(MenuAction::InverseSelection))
        );
        assert_eq!(
            map.resolve_any(&ctrl_shift('e')),
            Some(Resolved::Menu(MenuAction::MergeVisible))
        );
        assert_eq!(
            map.resolve_any(&ctrl('j')),
            Some(Resolved::Menu(MenuAction::LayerViaCopy))
        );
        assert_eq!(
            map.resolve_any(&ctrl_shift('j')),
            Some(Resolved::Menu(MenuAction::LayerViaCut))
        );
        // Where the displaced three live now.
        assert_eq!(
            map.resolve_any(&ctrl_alt_shift('i')),
            Some(Resolved::App(Action::ShowFileInfo))
        );
        assert_eq!(
            map.resolve_any(&ctrl_alt_shift('s')),
            Some(Resolved::App(Action::Export))
        );
        assert_eq!(
            map.resolve_any(&ctrl_alt('j')),
            Some(Resolved::App(Action::DuplicateLayer))
        );
        // Step Backward / Step Forward.
        assert_eq!(
            map.resolve_any(&ctrl_alt('z')),
            Some(Resolved::App(Action::Undo))
        );
        assert_eq!(
            map.resolve_any(&ctrl_shift('z')),
            Some(Resolved::App(Action::Redo))
        );
    }

    /// The table-driven gate: every chord the menu bar paints beside an item
    /// resolves to that item — either straight from the menu table, or through
    /// the application's own table to the action that means the same thing.
    ///
    /// RED before this module consulted the menu table at all: it listed the
    /// dead chords (Ctrl+E, Ctrl+G, Ctrl+T, Ctrl+A, Ctrl+D, F5–F8, …) and the
    /// three that resolved to a *different* action.
    #[test]
    fn every_painted_menu_chord_resolves_to_its_own_menu_action() {
        let map = Keymap::default();
        let mut wrong = Vec::new();
        let mut checked = 0;
        for action in MenuAction::all() {
            let Some(shortcut) = action.shortcut() else {
                continue;
            };
            let chord = chord_of_shortcut(shortcut)
                .unwrap_or_else(|| panic!("{shortcut} ({action:?}) has no chord in this keymap"));
            let got = map.resolve_any(&chord);
            let agrees = match got {
                Some(Resolved::Menu(menu)) => menu == action,
                Some(Resolved::App(app)) => menu_twin(app) == Some(action),
                None => false,
            };
            if !agrees {
                wrong.push(format!("{chord} paints {action:?} but resolves to {got:?}"));
            }
            checked += 1;
        }
        assert!(checked > 40, "the menu bar lost its shortcuts ({checked})");
        assert!(
            wrong.is_empty(),
            "{} painted chord(s) do not do what the menu says:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }

    /// Conflict detection across both tables: no chord names two different
    /// actions, whether two menu items claim it or the application's default
    /// table shadows a menu item with something that is not its twin.
    #[test]
    fn no_two_painted_menu_chords_disagree() {
        // Within the menu table itself.
        let mut by_chord: BTreeMap<Chord, Vec<MenuAction>> = BTreeMap::new();
        for (chord, action) in menu_bindings() {
            let slot = by_chord.entry(chord).or_default();
            if !slot.contains(&action) {
                slot.push(action);
            }
        }
        let doubled: Vec<String> = by_chord
            .iter()
            .filter(|(_, actions)| actions.len() > 1)
            .map(|(chord, actions)| format!("{chord} is painted beside {actions:?}"))
            .collect();
        assert!(doubled.is_empty(), "{}", doubled.join("; "));

        // Across the two tables: an application default on a painted chord
        // must be that item's twin, or the chord means two things.
        let shadowed: Vec<String> = Keymap::defaults()
            .into_iter()
            .filter_map(|b| {
                let menu = by_chord.get(&b.chord)?.first().copied()?;
                (menu_twin(b.action) != Some(menu)).then(|| {
                    format!(
                        "{} is {} in the keymap but {:?} in the menu",
                        b.chord,
                        b.action.label(),
                        menu
                    )
                })
            })
            .collect();
        assert!(shadowed.is_empty(), "{}", shadowed.join("; "));
    }

    #[test]
    fn menu_shortcuts_round_trip_through_chords() {
        // Every painted shortcut has a chord, and the chord paints back as the
        // same shortcut — so the hint beside the item is the key that fires it.
        for action in MenuAction::all() {
            let Some(shortcut) = action.shortcut() else {
                continue;
            };
            let chord = chord_of_shortcut(shortcut).unwrap();
            assert_eq!(
                shortcut_of_chord(&chord),
                Some(shortcut),
                "{action:?}: {shortcut} -> {chord} -> ?"
            );
        }
        // The `+`/`=` key is one key: both spellings paint as Plus.
        assert_eq!(
            shortcut_of_chord(&Chord::ctrl(Key::Char('+'))),
            Some(ui::Shortcut::ctrl_key(ui::Key::Plus))
        );
        // A key the menu vocabulary cannot name has no shortcut.
        assert_eq!(shortcut_of_chord(&Chord::plain(Key::Function(13))), None);
        assert_eq!(chord_of_shortcut(ui::Shortcut::bare(ui::Key::Home)), None);
    }

    #[test]
    fn the_user_layer_wins_over_the_menu_table_and_an_unbind_stops_the_fallthrough() {
        let mut map = Keymap::default();
        let ctrl_e = Chord::ctrl(Key::character('e'));
        assert_eq!(
            map.resolve_any(&ctrl_e),
            Some(Resolved::Menu(MenuAction::MergeDown))
        );

        // Bound by the user: theirs.
        map.force_bind(ctrl_e, Action::ZoomFit);
        assert_eq!(
            map.resolve_any(&ctrl_e),
            Some(Resolved::App(Action::ZoomFit))
        );

        // Unbound by the user: nothing, not the menu item underneath.
        map.unbind(ctrl_e);
        assert_eq!(map.resolve(&ctrl_e), None);
        assert_eq!(map.resolve_any(&ctrl_e), None);
        assert_eq!(
            map.menu_action_for(&ctrl_e),
            Some(MenuAction::MergeDown),
            "the menu table itself is untouched by the user layer"
        );

        // Reset: the menu item is reachable again.
        map.reset();
        assert_eq!(
            map.resolve_any(&ctrl_e),
            Some(Resolved::Menu(MenuAction::MergeDown))
        );

        // And the persisted form still carries only the user layer.
        map.unbind(ctrl_e);
        let json = serde_json::to_string(map.overrides()).unwrap();
        let restored = Keymap::with_overrides(serde_json::from_str(&json).unwrap());
        assert_eq!(restored, map);
        assert_eq!(restored.resolve_any(&ctrl_e), None);
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
