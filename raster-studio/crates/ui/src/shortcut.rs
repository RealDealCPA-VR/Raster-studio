//! Keyboard shortcuts as data.
//!
//! A shortcut is a value, never a string typed into a menu label. Two things
//! fall out of that and both are checked by tests rather than by review:
//!
//! * the hint painted beside a menu item is *derived* from the same value the
//!   keymap would match, so a rebind cannot leave a stale hint behind, and
//! * the whole menu bar can be walked to prove no two enabled items claim the
//!   same chord — see `no_two_menu_items_claim_the_same_chord` in [`crate::menu`].
//!
//! The platform difference is in the rendering only. macOS spells the primary
//! modifier `⌘` and orders modifiers control-option-shift-command; everywhere
//! else it is `Ctrl+Alt+Shift+Key`. [`Shortcut::to_string`] picks per platform;
//! the value itself is identical, so a document of shortcuts is portable.

use std::fmt;

/// A non-modifier key a shortcut can name.
///
/// Deliberately not egui's `Key`: this module is the vocabulary the *menu
/// model* speaks, and the menu model has to be testable with no egui context
/// alive. [`Key::from_egui`] does the one-way translation at the edge.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Key {
    /// A printable character. Stored lowercase; [`Key::character`] normalises.
    Char(char),
    /// Function key `F1`..=`F12`. Clamped into range by [`Key::function`].
    F(u8),
    Delete,
    Backspace,
    Enter,
    Escape,
    Tab,
    Space,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// The `+`/`=` key, which every editor binds "zoom in" to.
    Plus,
    /// The `-`/`_` key.
    Minus,
    LeftBracket,
    RightBracket,
    Comma,
    Period,
    Semicolon,
    Quote,
    Slash,
    Backslash,
    Grave,
}

impl Key {
    /// A character key, normalised to lowercase so `Char('A')` and `Char('a')`
    /// are the same chord.
    pub fn character(c: char) -> Self {
        Key::Char(c.to_ascii_lowercase())
    }

    /// A function key, clamped to `F1..=F12` rather than admitting an `F0` or
    /// an `F99` that no keyboard can produce.
    pub fn function(n: u8) -> Self {
        Key::F(n.clamp(1, 12))
    }

    /// How the key is written in a menu hint.
    pub fn label(self) -> String {
        match self {
            Key::Char(c) => c.to_ascii_uppercase().to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Delete => "Del".into(),
            Key::Backspace => "Backspace".into(),
            Key::Enter => "Enter".into(),
            Key::Escape => "Esc".into(),
            Key::Tab => "Tab".into(),
            Key::Space => "Space".into(),
            // Spelled, not drawn and not a symbol. A hint is *text* — it sits
            // inside "Ctrl+…" in a menu row's shortcut column, where there is
            // nothing to paint an icon into — and the four arrow symbols are
            // absent from the font egui loads, so "Ctrl+←" was "Ctrl+" and a
            // tofu box.
            Key::Left => "Left".into(),
            Key::Right => "Right".into(),
            Key::Up => "Up".into(),
            Key::Down => "Down".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::PageUp => "PgUp".into(),
            Key::PageDown => "PgDn".into(),
            Key::Plus => "+".into(),
            Key::Minus => "-".into(),
            Key::LeftBracket => "[".into(),
            Key::RightBracket => "]".into(),
            Key::Comma => ",".into(),
            Key::Period => ".".into(),
            Key::Semicolon => ";".into(),
            Key::Quote => "'".into(),
            Key::Slash => "/".into(),
            Key::Backslash => "\\".into(),
            Key::Grave => "`".into(),
        }
    }

    /// Translate an egui key press into this vocabulary, or `None` for a key
    /// the menu model has no name for (modifiers, media keys, the numpad).
    pub fn from_egui(key: egui::Key) -> Option<Self> {
        use egui::Key as K;
        Some(match key {
            K::Delete => Key::Delete,
            K::Backspace => Key::Backspace,
            K::Enter => Key::Enter,
            K::Escape => Key::Escape,
            K::Tab => Key::Tab,
            K::Space => Key::Space,
            K::ArrowLeft => Key::Left,
            K::ArrowRight => Key::Right,
            K::ArrowUp => Key::Up,
            K::ArrowDown => Key::Down,
            K::Home => Key::Home,
            K::End => Key::End,
            K::PageUp => Key::PageUp,
            K::PageDown => Key::PageDown,
            K::Plus | K::Equals => Key::Plus,
            K::Minus => Key::Minus,
            K::OpenBracket => Key::LeftBracket,
            K::CloseBracket => Key::RightBracket,
            K::Comma => Key::Comma,
            K::Period => Key::Period,
            K::Semicolon => Key::Semicolon,
            K::Quote => Key::Quote,
            K::Slash => Key::Slash,
            K::Backslash => Key::Backslash,
            K::Backtick => Key::Grave,
            K::F1 => Key::F(1),
            K::F2 => Key::F(2),
            K::F3 => Key::F(3),
            K::F4 => Key::F(4),
            K::F5 => Key::F(5),
            K::F6 => Key::F(6),
            K::F7 => Key::F(7),
            K::F8 => Key::F(8),
            K::F9 => Key::F(9),
            K::F10 => Key::F(10),
            K::F11 => Key::F(11),
            K::F12 => Key::F(12),
            other => {
                let name = other.name();
                let mut chars = name.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_alphanumeric() => Key::character(c),
                    _ => return None,
                }
            }
        })
    }
}

/// A modifier + key chord.
///
/// `ctrl` is the *primary* modifier: Control on Windows and Linux, Command on
/// macOS. There is deliberately no separate `cmd` field — a shortcut table that
/// carries both ends up with two spellings of Ctrl+S that no test can compare.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl Shortcut {
    /// A chord with no modifiers.
    pub const fn bare(key: Key) -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
            key,
        }
    }

    /// `Ctrl` + a character.
    pub fn ctrl(c: char) -> Self {
        Self {
            ctrl: true,
            ..Self::bare(Key::character(c))
        }
    }

    /// `Ctrl+Shift` + a character.
    pub fn ctrl_shift(c: char) -> Self {
        Self {
            ctrl: true,
            shift: true,
            ..Self::bare(Key::character(c))
        }
    }

    /// `Ctrl+Alt` + a character.
    pub fn ctrl_alt(c: char) -> Self {
        Self {
            ctrl: true,
            alt: true,
            ..Self::bare(Key::character(c))
        }
    }

    /// `Ctrl+Alt+Shift` + a character.
    pub fn ctrl_alt_shift(c: char) -> Self {
        Self {
            ctrl: true,
            alt: true,
            shift: true,
            ..Self::bare(Key::character(c))
        }
    }

    /// `Shift` + a character.
    pub fn shift(c: char) -> Self {
        Self {
            shift: true,
            ..Self::bare(Key::character(c))
        }
    }

    /// `Ctrl` + any key.
    pub const fn ctrl_key(key: Key) -> Self {
        Self {
            ctrl: true,
            ..Self::bare(key)
        }
    }

    /// `Ctrl+Shift` + any key.
    pub const fn ctrl_shift_key(key: Key) -> Self {
        Self {
            ctrl: true,
            shift: true,
            ..Self::bare(key)
        }
    }

    /// `true` when this chord carries no modifier at all.
    pub const fn is_bare(self) -> bool {
        !self.ctrl && !self.alt && !self.shift
    }

    /// Match an egui key press against this chord.
    ///
    /// egui reports the platform's Command key in `Modifiers::mac_cmd` and
    /// mirrors it into `command`; matching on `command` is therefore what makes
    /// one table work on every platform.
    pub fn matches(self, key: egui::Key, mods: egui::Modifiers) -> bool {
        Key::from_egui(key) == Some(self.key)
            && mods.command == self.ctrl
            && mods.alt == self.alt
            && mods.shift == self.shift
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if cfg!(target_os = "macos") {
            // macOS orders its modifiers option, shift, command — kept — but
            // spells them rather than using ⌥ / ⇧ / ⌘. The HIG glyphs are only
            // legible in a face that has them, and egui's default stack has
            // neither ⌥ nor ⇧, so a mac build would have shown two tofu boxes
            // in front of every chord. Words are honest on every platform.
            if self.alt {
                f.write_str("Opt+")?;
            }
            if self.shift {
                f.write_str("Shift+")?;
            }
            if self.ctrl {
                // `ctrl` is the *primary* modifier, which on macOS is Command.
                f.write_str("Cmd+")?;
            }
            f.write_str(&self.key.label())
        } else {
            if self.ctrl {
                f.write_str("Ctrl+")?;
            }
            if self.alt {
                f.write_str("Alt+")?;
            }
            if self.shift {
                f.write_str("Shift+")?;
            }
            f.write_str(&self.key.label())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_keys_are_case_insensitive() {
        assert_eq!(Key::character('A'), Key::character('a'));
        assert_eq!(Shortcut::ctrl('Z'), Shortcut::ctrl('z'));
    }

    #[test]
    fn a_character_key_is_shown_uppercase_but_stored_lowercase() {
        assert_eq!(Key::character('z'), Key::Char('z'));
        assert_eq!(Key::character('z').label(), "Z");
    }

    #[test]
    fn function_keys_are_clamped_into_range() {
        assert_eq!(Key::function(0), Key::F(1));
        assert_eq!(Key::function(99), Key::F(12));
        assert_eq!(Key::function(7), Key::F(7));
    }

    #[test]
    fn modifiers_are_rendered_in_a_fixed_order() {
        let s = Shortcut::ctrl_alt_shift('s').to_string();
        if cfg!(target_os = "macos") {
            assert_eq!(s, "Opt+Shift+Cmd+S");
        } else {
            assert_eq!(s, "Ctrl+Alt+Shift+S");
        }
    }

    #[test]
    fn a_bare_chord_renders_as_the_key_alone() {
        assert_eq!(Shortcut::bare(Key::Delete).to_string(), "Del");
        assert_eq!(Shortcut::bare(Key::F(7)).to_string(), "F7");
        assert!(Shortcut::bare(Key::F(7)).is_bare());
        assert!(!Shortcut::ctrl('s').is_bare());
    }

    #[test]
    fn a_chord_matches_only_its_own_modifier_set() {
        let save = Shortcut::ctrl('s');
        let command = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        assert!(save.matches(egui::Key::S, command));
        // Same key, one extra modifier: a different chord.
        assert!(!save.matches(
            egui::Key::S,
            egui::Modifiers {
                shift: true,
                ..command
            }
        ));
        // Right modifier, wrong key.
        assert!(!save.matches(egui::Key::A, command));
        // Right key, no modifier.
        assert!(!save.matches(egui::Key::S, egui::Modifiers::default()));
    }

    #[test]
    fn shift_chords_match_only_with_shift_held() {
        let feather = Shortcut::shift('f');
        assert!(feather.matches(
            egui::Key::F,
            egui::Modifiers {
                shift: true,
                ..Default::default()
            }
        ));
        assert!(!feather.matches(egui::Key::F, egui::Modifiers::default()));
    }

    #[test]
    fn the_equals_key_is_the_plus_key() {
        // Every editor binds "zoom in" to Ctrl+= as well as Ctrl+Shift+=.
        assert_eq!(Key::from_egui(egui::Key::Equals), Some(Key::Plus));
        assert_eq!(Key::from_egui(egui::Key::Plus), Some(Key::Plus));
    }

    #[test]
    fn letters_and_digits_translate_from_egui_by_name() {
        assert_eq!(Key::from_egui(egui::Key::A), Some(Key::Char('a')));
        assert_eq!(Key::from_egui(egui::Key::Num0), Some(Key::Char('0')));
        assert_eq!(Key::from_egui(egui::Key::F1), Some(Key::F(1)));
    }

    #[test]
    fn every_key_variant_has_a_non_empty_label() {
        let keys = [
            Key::Char('q'),
            Key::F(3),
            Key::Delete,
            Key::Backspace,
            Key::Enter,
            Key::Escape,
            Key::Tab,
            Key::Space,
            Key::Left,
            Key::Right,
            Key::Up,
            Key::Down,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Plus,
            Key::Minus,
            Key::LeftBracket,
            Key::RightBracket,
            Key::Comma,
            Key::Period,
            Key::Semicolon,
            Key::Quote,
            Key::Slash,
            Key::Backslash,
            Key::Grave,
        ];
        for k in keys {
            assert!(!k.label().is_empty(), "{k:?} has no label");
        }
    }
}
