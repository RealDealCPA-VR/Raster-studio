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

/// W9-E: one sampled brush tip — the pixels a sampled brush's settings name
/// by hash. Kept here, beside the brushes, because the settings JSON only
/// carries the hash; without the pixels a restarted application would have
/// a name and nothing to stamp.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct BrushTipPreset {
    /// [`tip_hash`] of the pixels, lowercase hex.
    pub hash: String,
    pub width: u32,
    pub height: u32,
    /// `width * height` coverage bytes, `255` = full paint.
    ///
    /// Never written into the JSON: [`PresetStore::save`] puts the pixels in
    /// a binary file of their own (one byte per pixel, named by the hash, in
    /// [`PresetStore::tips_dir`]) and [`PresetStore::load`] reads them back.
    /// A file written before that (pixels inline as a number array) still
    /// deserializes here, and its next save moves the pixels out.
    #[serde(default, skip_serializing)]
    pub alpha8: Vec<u8>,
}

/// W9-E: the content hash that names a sampled tip: BLAKE3 over its width,
/// height (little-endian) and coverage bytes.
pub fn tip_hash(width: u32, height: u32, alpha8: &[u8]) -> crate::BlobHash {
    let mut bytes = Vec::with_capacity(8 + alpha8.len());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(alpha8);
    crate::BlobHash::of(&bytes)
}

/// The store: ordered, named, persisted as one JSON document.
///
/// Order is creation order and is part of the interface — "the pattern I just
/// defined" is the last one, which is what a menu item with no name dialog
/// offers back.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct PresetStore {
    #[serde(default)]
    patterns: Vec<PatternPreset>,
    /// Named layer-style presets: `(name, serialized LayerEffects JSON)`.
    /// The store crate deliberately does not depend on `layer-model`; the
    /// application serializes at its own edge (the same contract as the
    /// brushes' JSON).
    #[serde(default)]
    styles: Vec<(String, String)>,
    /// `(name, serialized settings)` — the application owns the schema.
    brushes: Vec<(String, String)>,
    /// W9-E: the pixels of every sampled tip a brush above names.
    #[serde(default)]
    tips: Vec<BrushTipPreset>,
    /// W9-N: gradients a `.grd` file brought in (File > Open).
    #[serde(default)]
    gradients: Vec<crate::resources::GradientResource>,
    /// W9-N: custom shapes a `.csh` file brought in (File > Open).
    #[serde(default)]
    shapes: Vec<crate::resources::ShapeResource>,
}

impl PresetStore {
    /// W9-E: store a sampled tip, returning its hash. The same pixels stored
    /// twice are stored once.
    pub fn define_tip(&mut self, width: u32, height: u32, alpha8: Vec<u8>) -> crate::BlobHash {
        let hash = tip_hash(width, height, &alpha8);
        let hex = hash.to_hex();
        if !self.tips.iter().any(|t| t.hash == hex) {
            self.tips.push(BrushTipPreset {
                hash: hex,
                width,
                height,
                alpha8,
            });
        }
        hash
    }

    /// W9-E: every stored tip, oldest first.
    pub fn tips(&self) -> &[BrushTipPreset] {
        &self.tips
    }

    /// W9-E: the stored tip with this hash.
    pub fn tip(&self, hash: crate::BlobHash) -> Option<&BrushTipPreset> {
        let hex = hash.to_hex();
        self.tips.iter().find(|t| t.hash == hex)
    }

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

    /// Store a layer-style preset, replacing any of the same name.
    pub fn define_style(&mut self, name: &str, effects_json: String) {
        let slot = (name.to_string(), effects_json);
        if let Some(existing) = self.styles.iter_mut().find(|(n, _)| n == name) {
            *existing = slot;
        } else {
            self.styles.push(slot);
        }
    }

    /// Every style preset, oldest first.
    pub fn styles(&self) -> &[(String, String)] {
        &self.styles
    }

    /// The most recently defined style preset — what an unnamed menu item
    /// offers back (the same rule as [`Self::latest_pattern`]).
    pub fn latest_style(&self) -> Option<&(String, String)> {
        self.styles.last()
    }

    /// Whether anything at all is stored.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
            && self.brushes.is_empty()
            && self.styles.is_empty()
            && self.gradients.is_empty()
            && self.shapes.is_empty()
    }

    /// W9-N: add an imported gradient, replacing any of the same name.
    pub fn define_gradient(&mut self, gradient: crate::resources::GradientResource) {
        match self.gradients.iter_mut().find(|g| g.name == gradient.name) {
            Some(slot) => *slot = gradient,
            None => self.gradients.push(gradient),
        }
    }

    /// W9-N: every imported gradient, oldest first.
    pub fn gradients(&self) -> &[crate::resources::GradientResource] {
        &self.gradients
    }

    /// W9-N: add an imported custom shape, replacing any of the same name.
    pub fn define_shape(&mut self, shape: crate::resources::ShapeResource) {
        match self.shapes.iter_mut().find(|s| s.name == shape.name) {
            Some(slot) => *slot = shape,
            None => self.shapes.push(shape),
        }
    }

    /// W9-N: every imported custom shape, oldest first.
    pub fn shapes(&self) -> &[crate::resources::ShapeResource] {
        &self.shapes
    }

    /// Write the store as one pretty JSON document.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        self.save_tips(path)?;
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
        let mut store: Self = serde_json::from_slice(&bytes).unwrap_or_default();
        store.load_tips(path);
        store
    }

    /// W9-E: the directory beside the store file `path` that holds the
    /// sampled tips' pixels (`presets.json` -> `presets.tips/`).
    pub fn tips_dir(path: &Path) -> PathBuf {
        path.with_extension("tips")
    }

    /// W9-E: the binary file one tip's pixels live in: `width * height` raw
    /// coverage bytes, named by the tip's hash.
    fn tip_file(path: &Path, hex: &str) -> PathBuf {
        Self::tips_dir(path).join(format!("{hex}.a8"))
    }

    /// W9-E: write every tip's pixels that is not on disk yet. The files are
    /// content-addressed, so one already there with the right length is the
    /// same pixels and is not rewritten: saving the store after adding a
    /// brush writes that brush's tip and a small JSON, not every tip again.
    fn save_tips(&self, path: &Path) -> std::io::Result<()> {
        if self.tips.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(Self::tips_dir(path))?;
        for tip in &self.tips {
            let file = Self::tip_file(path, &tip.hash);
            let present = std::fs::metadata(&file)
                .map(|m| m.len() == tip.alpha8.len() as u64)
                .unwrap_or(false);
            if present {
                continue;
            }
            let partial = file.with_extension("a8.partial");
            std::fs::write(&partial, &tip.alpha8)?;
            std::fs::rename(&partial, &file)?;
        }
        Ok(())
    }

    /// W9-E: fill in each tip's pixels from its binary file. A tip whose
    /// file is missing, the wrong length, or does not hash to its name is
    /// dropped (the brush that names it then paints round), never trusted.
    fn load_tips(&mut self, path: &Path) {
        self.tips.retain_mut(|tip| {
            let Some(len) = (tip.width as u64).checked_mul(u64::from(tip.height)) else {
                return false;
            };
            if tip.alpha8.is_empty() {
                let file = Self::tip_file(path, &tip.hash);
                let on_disk = std::fs::metadata(&file).map(|m| m.len()).ok();
                if on_disk != Some(len) {
                    return false;
                }
                match std::fs::read(&file) {
                    Ok(bytes) => tip.alpha8 = bytes,
                    Err(_) => return false,
                }
            }
            tip.alpha8.len() as u64 == len
                && tip_hash(tip.width, tip.height, &tip.alpha8).to_hex() == tip.hash
        });
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

    /// W9-N: a presets file written before gradients and shapes existed
    /// still opens, and imported ones survive a save and load.
    #[test]
    fn imported_gradients_and_shapes_persist_and_old_files_still_open() {
        let old: PresetStore =
            serde_json::from_str(r#"{"patterns":[],"brushes":[["B","{}"]]}"#).unwrap();
        assert_eq!(old.brushes().len(), 1);
        assert!(old.gradients().is_empty() && old.shapes().is_empty());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        let mut store = PresetStore::new();
        store.define_gradient(crate::resources::GradientResource {
            name: "G".into(),
            smoothness: 1.0,
            stops: vec![crate::resources::ColorStopResource {
                position: 0.0,
                midpoint: 0.5,
                rgb: [1.0, 0.0, 0.0],
            }],
            opacity_stops: vec![],
        });
        store.define_shape(crate::resources::ShapeResource {
            name: "S".into(),
            id: String::new(),
            subpaths: vec![],
        });
        assert!(!store.is_empty());
        store.save(&path).unwrap();
        let loaded = PresetStore::load(&path);
        assert_eq!(loaded.gradients(), store.gradients());
        assert_eq!(loaded.shapes(), store.shapes());
    }

    #[test]
    fn a_missing_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = PresetStore::load(&dir.path().join("nope.json"));
        assert!(store.is_empty());
    }

    #[test]
    fn sampled_tips_are_stored_once_by_hash_and_survive_a_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        let mut store = PresetStore::new();
        let a = store.define_tip(2, 1, vec![0, 255]);
        assert_eq!(store.define_tip(2, 1, vec![0, 255]), a, "same pixels");
        assert_eq!(store.tips().len(), 1);
        let b = store.define_tip(1, 2, vec![0, 255]);
        assert_ne!(a, b, "the shape is part of the identity");
        store.save(&path).unwrap();
        let loaded = PresetStore::load(&path);
        assert_eq!(loaded.tip(a).unwrap().alpha8, vec![0, 255]);
        assert_eq!(loaded.tip(b).unwrap().height, 2);
        // A store written before tips existed still loads.
        let old: PresetStore =
            serde_json::from_str(r#"{"patterns":[],"brushes":[["x","{}"]]}"#).unwrap();
        assert!(old.tips().is_empty());
        assert_eq!(old.brushes().len(), 1);
    }

    /// W9-E: a tip's pixels go to a binary file (one byte a pixel), not
    /// into the JSON as a number array; a second save does not rewrite them;
    /// a file from before (pixels inline) still loads and migrates; a
    /// damaged pixel file drops its tip instead of trusting it.
    #[test]
    fn tip_pixels_live_in_a_binary_file_not_in_the_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        let (w, h) = (1000u32, 1000u32);
        let pixels: Vec<u8> = (0..w * h).map(|i| (i % 251) as u8).collect();
        let mut store = PresetStore::new();
        let hash = store.define_tip(w, h, pixels.clone());
        store.define_brush("Big", "{}".to_string());
        store.save(&path).unwrap();

        let json_len = std::fs::metadata(&path).unwrap().len();
        assert!(
            json_len < 4096,
            "presets.json is {json_len} bytes: the pixels went into the JSON"
        );
        let blob = PresetStore::tips_dir(&path).join(format!("{}.a8", hash.to_hex()));
        assert_eq!(std::fs::read(&blob).unwrap(), pixels, "one byte a pixel");

        let loaded = PresetStore::load(&path);
        assert_eq!(loaded.tip(hash).unwrap().alpha8, pixels);

        // Unchanged content is not rewritten by the next save.
        let stamp = std::fs::metadata(&blob).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        loaded.save(&path).unwrap();
        assert_eq!(std::fs::metadata(&blob).unwrap().modified().unwrap(), stamp);

        // The pre-binary layout (pixels inline) still opens, and moves out.
        let old_dir = tempfile::tempdir().unwrap();
        let old_path = old_dir.path().join("presets.json");
        let small = tip_hash(2, 1, &[0, 255]).to_hex();
        std::fs::write(
            &old_path,
            format!(
                r#"{{"brushes":[],"tips":[{{"hash":"{small}","width":2,"height":1,"alpha8":[0,255]}}]}}"#
            ),
        )
        .unwrap();
        let old = PresetStore::load(&old_path);
        assert_eq!(old.tips()[0].alpha8, vec![0, 255]);
        old.save(&old_path).unwrap();
        assert!(!std::fs::read_to_string(&old_path)
            .unwrap()
            .contains("alpha8"));
        assert_eq!(PresetStore::load(&old_path).tips()[0].alpha8, vec![0, 255]);

        // A pixel file of the wrong length, or wrong content, is not trusted.
        std::fs::write(&blob, [1, 2, 3]).unwrap();
        assert!(PresetStore::load(&path).tips().is_empty());
        let mut tampered = pixels.clone();
        tampered[0] ^= 1;
        std::fs::write(&blob, &tampered).unwrap();
        assert!(PresetStore::load(&path).tips().is_empty());
    }
}
