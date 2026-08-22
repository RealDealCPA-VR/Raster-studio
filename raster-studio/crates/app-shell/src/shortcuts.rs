//! Keyboard shortcut routing. Maps key chords to named application actions so
//! the menu system, command palette, and key handler share one source of truth.

use std::collections::HashMap;

/// A named, routable application action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Undo,
    Redo,
    Save,
    Open,
    Export,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActualPixels,
    NewLayer,
    DeleteLayer,
}

/// A platform-agnostic chord: modifiers + a single key label (e.g. "Z").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub ctrl_or_cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: String,
}

impl Chord {
    pub fn new(ctrl_or_cmd: bool, shift: bool, alt: bool, key: &str) -> Self {
        Self {
            ctrl_or_cmd,
            shift,
            alt,
            key: key.to_ascii_uppercase(),
        }
    }
}

/// Bidirectional-ish shortcut map. Lookups go chord -> action.
pub struct Shortcuts {
    map: HashMap<Chord, Action>,
}

impl Default for Shortcuts {
    fn default() -> Self {
        let mut map = HashMap::new();
        map.insert(Chord::new(true, false, false, "Z"), Action::Undo);
        map.insert(Chord::new(true, true, false, "Z"), Action::Redo);
        map.insert(Chord::new(true, false, false, "S"), Action::Save);
        map.insert(Chord::new(true, false, false, "O"), Action::Open);
        map.insert(Chord::new(true, true, false, "E"), Action::Export);
        map.insert(Chord::new(true, false, false, "="), Action::ZoomIn);
        map.insert(Chord::new(true, false, false, "-"), Action::ZoomOut);
        map.insert(Chord::new(true, false, false, "0"), Action::ZoomFit);
        map.insert(
            Chord::new(true, false, false, "1"),
            Action::ZoomActualPixels,
        );
        Self { map }
    }
}

impl Shortcuts {
    pub fn resolve(&self, chord: &Chord) -> Option<Action> {
        self.map.get(chord).copied()
    }

    /// Rebind (or add) a chord to an action.
    pub fn bind(&mut self, chord: Chord, action: Action) {
        self.map.insert(chord, action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_undo_redo() {
        let s = Shortcuts::default();
        assert_eq!(
            s.resolve(&Chord::new(true, false, false, "z")),
            Some(Action::Undo)
        );
        assert_eq!(
            s.resolve(&Chord::new(true, true, false, "z")),
            Some(Action::Redo)
        );
    }

    #[test]
    fn rebind_overrides() {
        let mut s = Shortcuts::default();
        s.bind(Chord::new(true, false, false, "K"), Action::Export);
        assert_eq!(
            s.resolve(&Chord::new(true, false, false, "k")),
            Some(Action::Export)
        );
    }

    #[test]
    fn unknown_chord_is_none() {
        let s = Shortcuts::default();
        assert_eq!(s.resolve(&Chord::new(false, false, false, "Q")), None);
    }
}
