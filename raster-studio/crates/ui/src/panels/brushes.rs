//! The Brushes panel: named presets over the same [`BrushSettings`] the tool
//! options bar edits.
//!
//! A preset is not a separate kind of thing from "the brush you have right
//! now" — it is a saved copy of it. Applying one writes every field into
//! [`crate::tool_options::ToolOptions`] through the registry, so a preset can
//! never set a value the schema would refuse, and a tool whose schema lacks a
//! field simply ignores that part of the preset.

use tools::{BrushSettings, ToolId};

use crate::tool_options::{OptionValue, ToolOptions};

/// A named brush.
#[derive(Clone, PartialEq, Debug)]
pub struct BrushPreset {
    pub name: String,
    pub settings: BrushSettings,
}

/// The Brushes panel.
#[derive(Clone, PartialEq, Debug)]
pub struct BrushesState {
    presets: Vec<BrushPreset>,
    /// Index of the preset last applied, cleared as soon as the live brush
    /// stops matching it.
    active: Option<usize>,
}

impl Default for BrushesState {
    fn default() -> Self {
        Self {
            presets: default_presets(),
            active: None,
        }
    }
}

impl BrushesState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn presets(&self) -> &[BrushPreset] {
        &self.presets
    }

    pub fn get(&self, index: usize) -> Option<&BrushPreset> {
        self.presets.get(index)
    }

    pub fn len(&self) -> usize {
        self.presets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    /// Which preset is showing as selected, if the live brush still matches it.
    pub fn active(&self) -> Option<usize> {
        self.active
    }

    /// Save the tool's current brush as a new preset. An empty name is refused
    /// — an unnamed row in a list of names cannot be told apart.
    pub fn capture(&mut self, name: &str, options: &ToolOptions, tool: ToolId) -> Option<usize> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        self.presets.push(BrushPreset {
            name: name.to_string(),
            settings: options.brush_settings(tool),
        });
        let index = self.presets.len() - 1;
        self.active = Some(index);
        Some(index)
    }

    pub fn remove(&mut self, index: usize) -> Option<BrushPreset> {
        if index >= self.presets.len() {
            return None;
        }
        if self.active == Some(index) {
            self.active = None;
        } else if let Some(a) = self.active {
            if a > index {
                self.active = Some(a - 1);
            }
        }
        Some(self.presets.remove(index))
    }

    /// Apply a preset to a tool, returning the option writes that actually
    /// changed something — which is exactly what the panel emits as
    /// [`crate::Intent::SetToolOption`]s.
    ///
    /// A field the tool's schema does not declare is skipped rather than
    /// forced: the pencil has no `roundness`, and a preset should not invent
    /// one for it.
    pub fn apply(
        &mut self,
        index: usize,
        options: &mut ToolOptions,
        tool: ToolId,
    ) -> Vec<(&'static str, OptionValue)> {
        let Some(preset) = self.presets.get(index) else {
            return Vec::new();
        };
        let s = preset.settings;
        let writes: [(&'static str, OptionValue); 10] = [
            ("size", OptionValue::Float(s.size)),
            ("hardness", OptionValue::Float(s.hardness)),
            ("spacing", OptionValue::Float(s.spacing)),
            ("angle", OptionValue::Float(s.angle)),
            ("roundness", OptionValue::Float(s.roundness)),
            ("opacity", OptionValue::Float(s.opacity)),
            ("flow", OptionValue::Float(s.flow)),
            ("smoothing", OptionValue::Float(s.smoothing)),
            ("size_pressure", OptionValue::Bool(s.size_pressure)),
            ("flow_pressure", OptionValue::Bool(s.flow_pressure)),
        ];
        self.active = Some(index);
        writes
            .into_iter()
            .filter(|(key, value)| options.set(tool, key, *value))
            .collect()
    }

    /// Drop the selection highlight once the live brush no longer matches the
    /// preset it came from.
    pub fn sync(&mut self, options: &ToolOptions, tool: ToolId) {
        let Some(index) = self.active else { return };
        let matches = self.presets.get(index).is_some_and(|p| {
            let live = options.brush_settings(tool);
            let saved = p.settings;
            live.size == saved.size
                && live.hardness == saved.hardness
                && live.spacing == saved.spacing
                && live.opacity == saved.opacity
                && live.flow == saved.flow
        });
        if !matches {
            self.active = None;
        }
    }
}

/// The presets a new install starts with — one per shape of stroke, rather than
/// a hundred textures nobody has authored yet.
fn default_presets() -> Vec<BrushPreset> {
    let soft = |size: f32, hardness: f32| BrushSettings {
        size,
        hardness,
        ..BrushSettings::default()
    };
    vec![
        BrushPreset {
            name: "Soft Round 24".into(),
            settings: soft(24.0, 0.0),
        },
        BrushPreset {
            name: "Hard Round 24".into(),
            settings: soft(24.0, 1.0),
        },
        BrushPreset {
            name: "Soft Round 100".into(),
            settings: soft(100.0, 0.0),
        },
        BrushPreset {
            name: "Hard Round 4".into(),
            settings: soft(4.0, 1.0),
        },
        BrushPreset {
            name: "Pencil 1px".into(),
            settings: BrushSettings::pencil(1.0),
        },
        BrushPreset {
            name: "Flat Angled 40".into(),
            settings: BrushSettings {
                size: 40.0,
                hardness: 0.9,
                roundness: 0.2,
                angle: 0.7,
                ..BrushSettings::default()
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_presets_are_named_and_distinct() {
        let s = BrushesState::new();
        assert!(s.len() >= 5);
        assert!(s.presets().iter().all(|p| !p.name.is_empty()));
        let mut names: Vec<&str> = s.presets().iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two presets share a name");
    }

    #[test]
    fn applying_a_preset_writes_it_into_the_tool_options() {
        let mut s = BrushesState::new();
        let mut options = ToolOptions::new();
        let hard_round_4 = s
            .presets()
            .iter()
            .position(|p| p.name == "Hard Round 4")
            .expect("preset exists");
        let writes = s.apply(hard_round_4, &mut options, ToolId::Brush);
        assert!(!writes.is_empty());
        let brush = options.brush_settings(ToolId::Brush);
        assert_eq!(brush.size, 4.0);
        assert_eq!(brush.hardness, 1.0);
        assert_eq!(s.active(), Some(hard_round_4));
    }

    #[test]
    fn applying_the_preset_already_in_force_writes_nothing() {
        let mut s = BrushesState::new();
        let mut options = ToolOptions::new();
        s.apply(0, &mut options, ToolId::Brush);
        let again = s.apply(0, &mut options, ToolId::Brush);
        assert!(again.is_empty(), "re-applying emitted {again:?}");
    }

    #[test]
    fn a_field_the_tools_schema_does_not_have_is_skipped_not_forced() {
        let mut s = BrushesState::new();
        let mut options = ToolOptions::new();
        // The clone stamp declares size/hardness/spacing/opacity/aligned, and
        // no roundness or flow.
        let writes = s.apply(0, &mut options, ToolId::CloneStamp);
        let keys: Vec<&str> = writes.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"size"));
        assert!(!keys.contains(&"roundness"), "wrote a key the tool lacks");
        assert!(!keys.contains(&"flow"), "wrote a key the tool lacks");
    }

    #[test]
    fn capturing_saves_the_live_brush_under_a_name() {
        let mut s = BrushesState::new();
        let mut options = ToolOptions::new();
        options.set(ToolId::Brush, "size", OptionValue::Float(77.0));
        let index = s
            .capture("  My Brush  ", &options, ToolId::Brush)
            .expect("a real name");
        assert_eq!(s.get(index).unwrap().name, "My Brush");
        assert_eq!(s.get(index).unwrap().settings.size, 77.0);
        assert_eq!(s.active(), Some(index));
    }

    #[test]
    fn capturing_refuses_an_empty_name() {
        let mut s = BrushesState::new();
        let before = s.len();
        assert!(s
            .capture("   ", &ToolOptions::new(), ToolId::Brush)
            .is_none());
        assert_eq!(s.len(), before);
    }

    #[test]
    fn removing_a_preset_keeps_the_highlight_on_the_right_row() {
        let mut s = BrushesState::new();
        let mut options = ToolOptions::new();
        s.apply(3, &mut options, ToolId::Brush);
        assert_eq!(s.active(), Some(3));
        s.remove(1).expect("in range");
        assert_eq!(
            s.active(),
            Some(2),
            "the highlight did not follow the shift"
        );
        s.remove(2).expect("in range");
        assert_eq!(s.active(), None, "the removed preset stayed highlighted");
        assert_eq!(s.remove(999), None);
    }

    #[test]
    fn changing_the_brush_by_hand_drops_the_preset_highlight() {
        let mut s = BrushesState::new();
        let mut options = ToolOptions::new();
        s.apply(0, &mut options, ToolId::Brush);
        assert_eq!(s.active(), Some(0));
        s.sync(&options, ToolId::Brush);
        assert_eq!(s.active(), Some(0), "an untouched brush still matches");
        options.set(ToolId::Brush, "size", OptionValue::Float(999.0));
        s.sync(&options, ToolId::Brush);
        assert_eq!(s.active(), None);
    }
}
