//! User presets — named patterns and brushes the menus define and reuse.
//!
//! Patterns are self-contained snapshots (width, height, RGBA8), so they need
//! nothing from the tool crates. Brushes are stored as JSON because
//! [`crate`] deliberately does not depend on the tools crate to know
//! `BrushSettings`' schema; the application serializes and deserializes at its
//! own edge, where both sides of the conversion live.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One named pattern.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct PatternPreset {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes of RGBA8, row-major.
    pub rgba8: Vec<u8>,
}

impl PatternPreset {
    /// The colour of the pattern pixel at `(x, y)`, tiled over an infinite
    /// plane — the coordinate is taken modulo the tile, which is what makes a
    /// pattern a pattern.
    pub fn pixel(&self, x: i64, y: i64) -> [u8; 4] {
        let tx = x.rem_euclid(i64::from(self.width)) as usize;
        let ty = y.rem_euclid(i64::from(self.height)) as usize;
        let i = (ty * self.width as usize + tx) * 4;
        [
            self.rgba8[i],
            self.rgba8[i + 1],
            self.rgba8[i + 2],
            self.rgba8[i + 3],
        ]
    }
}

/// The store: ordered, named, persisted as one JSON document.
///
/// Order is creation order and is part of the interface — "the pattern I just
/// defined" is the last one, which is what a menu item with no name dialog
/// offers back.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct PresetStore {
    patterns: Vec<PatternPreset>,
    /// `(name, serialized settings)` — the application owns the schema.
    brushes: Vec<(String, String)>,
}

impl PresetStore {
    /// Empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a pattern, replacing any of the same name in place so re-defining
    /// is an update rather than a duplicate.
    pub fn define_pattern(&mut self, preset: PatternPreset) {
        if let Some(slot) = self.patterns.iter_mut().find(|p| p.name == preset.name) {
            *slot = preset;
        } else {
            self.patterns.push(preset);
        }
    }

    /// Every pattern, oldest first.
    pub fn patterns(&self) -> &[PatternPreset] {
        &self.patterns
    }

    /// The names, in menu order.
    pub fn pattern_names(&self) -> Vec<String> {
        self.patterns.iter().map(|p| p.name.clone()).collect()
    }

    /// The named pattern, if defined.
    pub fn pattern(&self, name: &str) -> Option<&PatternPreset> {
        self.patterns.iter().find(|p| p.name == name)
    }

    /// The most recently defined pattern — what an unnamed menu item offers.
    pub fn latest_pattern(&self) -> Option<&PatternPreset> {
        self.patterns.last()
    }

    /// Store a brush preset, replacing any of the same name.
    pub fn define_brush(&mut self, name: &str, settings_json: String) {
        let slot = (name.to_string(), settings_json);
        if let Some(existing) = self.brushes.iter_mut().find(|(n, _)| n == name) {
            *existing = slot;
        } else {
            self.brushes.push(slot);
        }
    }

    /// Every brush preset, oldest first.
    pub fn brushes(&self) -> &[(String, String)] {
        &self.brushes
    }

    /// Whether anything at all is stored.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty() && self.brushes.is_empty()
    }

    /// Write the store as one pretty JSON document.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Read the store; a missing file is an empty store, because a user who
    /// has never defined a preset has not done anything wrong.
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// The path the application persists this store at.
    pub fn file_in(root: &Path) -> PathBuf {
        root.join("presets.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_tile_by_modulo() {
        let mut store = PresetStore::new();
        store.define_pattern(PatternPreset {
            name: "Checker".to_string(),
            width: 2,
            height: 2,
            rgba8: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 255, 0, 0, 255,
            ],
        });
        let p = store.pattern("Checker").unwrap();
        assert_eq!(p.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(p.pixel(2, 0), [255, 0, 0, 255], "x tiles");
        assert_eq!(p.pixel(0, 4), [255, 0, 0, 255], "y tiles");
        assert_eq!(p.pixel(-1, 0), [0, 255, 0, 255], "negative x wraps");
        assert_eq!(p.pixel(1, -3), [255, 0, 0, 255], "negative y wraps");
    }

    #[test]
    fn redefining_a_name_updates_in_place() {
        let mut store = PresetStore::new();
        store.define_pattern(PatternPreset {
            name: "P".to_string(),
            width: 1,
            height: 1,
            rgba8: vec![0, 0, 0, 255],
        });
        store.define_pattern(PatternPreset {
            name: "P".to_string(),
            width: 1,
            height: 1,
            rgba8: vec![255, 255, 255, 255],
        });
        assert_eq!(store.patterns().len(), 1);
        assert_eq!(store.pattern("P").unwrap().rgba8, vec![255, 255, 255, 255]);
    }

    #[test]
    fn the_store_round_trips_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        let mut store = PresetStore::new();
        store.define_pattern(PatternPreset {
            name: "Dot".to_string(),
            width: 1,
            height: 1,
            rgba8: vec![1, 2, 3, 4],
        });
        store.define_brush("Fat", r#"{"size":24.0}"#.to_string());
        store.save(&path).unwrap();

        let loaded = PresetStore::load(&path);
        assert_eq!(loaded.pattern("Dot").unwrap().rgba8, vec![1, 2, 3, 4]);
        assert_eq!(
            loaded.brushes(),
            &[("Fat".to_string(), r#"{"size":24.0}"#.to_string())]
        );
    }

    #[test]
    fn a_missing_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = PresetStore::load(&dir.path().join("nope.json"));
        assert!(store.is_empty());
    }
}
