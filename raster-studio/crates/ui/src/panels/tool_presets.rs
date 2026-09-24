//! W10-B: the Tool Presets panel.
//!
//! A tool preset is a tool together with the option values the user set on
//! it — "Brush, 40 px, 20% hardness, Multiply" — saved under a name and
//! brought back with one click. Presets are the user's, not the document's,
//! so they live on the workspace ([`crate::Workspace::tool_presets`]) beside
//! the brush presets, and [`ToolPresetsState::saved`] /
//! [`ToolPresetsState::restore_saved`] give the application a serializable
//! form ([`SavedToolPreset`]) that it keeps in its preferences file (the
//! shell's `Preferences::sync_panel_presets`, once a frame, like the brush
//! presets: the saved list loads into an untouched panel on start, and every
//! edit is written back).
//!
//! Only the options the user actually *touched* are recorded
//! ([`crate::ToolOptions::held`]), and applying a preset first returns the
//! tool to its defaults: a preset therefore restores the tool exactly as it
//! was when saved, not "as it was, plus whatever was changed since". A ramp
//! tool's gradient rides along.
//!
//! Applying goes through the same intents the options bar and the palette
//! raise ([`Intent::ResetToolOptions`], [`Intent::SetToolOption`],
//! [`Intent::SetToolGradient`], [`Intent::SelectTool`]), absorbed here as they
//! are emitted — every one is an absolute set, so the application's second
//! absorb is a no-op.
//!
//! # Strings
//!
//! Through the strings catalogue ([`crate::strings::tr`]): each constant
//! below is a catalogue key.

use design::{current_tokens, Space, TextRole};
use egui::{Align, Layout, Ui, Vec2};
use layer_model::Gradient;
use serde::{Deserialize, Serialize};
use tools::ToolId;

use crate::intent::Intent;
use crate::strings::tr;
use crate::tool_options::{schema_for, wants_gradient_stops, OptionValue};
use crate::view::{body, hairline, hint, icon_action_id, list_row_layout, text, ActionState};
use crate::Workspace;

const NO_PRESETS: &str = "ui.tool_presets.none";
const NEW: &str = "ui.tool_presets.new";
const DELETE: &str = "ui.tool_presets.delete";

/// One saved tool with its options.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolPreset {
    pub name: String,
    pub tool: ToolId,
    /// The touched options, as `(key, value)`.
    pub values: Vec<(String, OptionValue)>,
    /// The ramp, for a tool that draws one.
    pub gradient: Option<Gradient>,
}

impl ToolPreset {
    /// Record `tool` as `options` hold it now.
    pub fn capture(name: impl Into<String>, tool: ToolId, options: &crate::ToolOptions) -> Self {
        let mut values = options.held(tool);
        // `held` walks a hash map; a stable order keeps saved files and
        // comparisons deterministic.
        values.sort_by(|a, b| a.0.cmp(&b.0));
        let gradient = tools::registry::info(tool)
            .filter(|info| wants_gradient_stops(info))
            .map(|_| options.gradient(tool));
        Self {
            name: name.into(),
            tool,
            values,
            gradient,
        }
    }

    /// The intents that put this preset back: the tool's options to their
    /// defaults, then each recorded value, the ramp, and the tool itself.
    /// A key the tool no longer has is skipped.
    pub fn intents(&self) -> Vec<Intent> {
        let mut out = vec![Intent::ResetToolOptions(self.tool)];
        let schema = tools::registry::info(self.tool)
            .map(schema_for)
            .unwrap_or_default();
        for (key, value) in &self.values {
            if let Some(spec) = schema.iter().find(|s| s.key == key.as_str()) {
                out.push(Intent::SetToolOption {
                    tool: self.tool,
                    key: spec.key,
                    value: *value,
                });
            }
        }
        if let Some(gradient) = &self.gradient {
            out.push(Intent::SetToolGradient {
                tool: self.tool,
                gradient: Box::new(gradient.clone()),
            });
        }
        out.push(Intent::SelectTool(self.tool));
        out
    }
}

/// The panel's list, owned by the workspace.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolPresetsState {
    presets: Vec<ToolPreset>,
    /// How many times the user changed the list this session — 0 means the
    /// application may still load the saved list over it (the brush presets'
    /// rule).
    edits: u32,
}

impl ToolPresetsState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn presets(&self) -> &[ToolPreset] {
        &self.presets
    }

    pub fn edits(&self) -> u32 {
        self.edits
    }

    /// Append a preset; answers its index.
    pub fn add(&mut self, preset: ToolPreset) -> usize {
        self.presets.push(preset);
        self.edits += 1;
        self.presets.len() - 1
    }

    /// Remove preset `index`, if there is one.
    pub fn remove(&mut self, index: usize) -> Option<ToolPreset> {
        (index < self.presets.len()).then(|| {
            self.edits += 1;
            self.presets.remove(index)
        })
    }

    /// `<Tool name> <n>` for the lowest free `n`.
    pub fn next_name(&self, tool: ToolId) -> String {
        let base = tools::registry::info(tool).map_or("Tool", |i| i.name);
        let mut n = 1;
        loop {
            let name = format!("{base} {n}");
            if !self.presets.iter().any(|p| p.name == name) {
                return name;
            }
            n += 1;
        }
    }

    /// Apply preset `index` to `w`: absorb and emit its intents.
    pub fn apply(w: &mut Workspace, index: usize) -> bool {
        let Some(preset) = w.tool_presets.presets.get(index).cloned() else {
            return false;
        };
        for intent in preset.intents() {
            w.absorb(&intent);
            w.emit(intent);
        }
        true
    }

    /// The list in its serializable form, for the preferences file.
    pub fn saved(&self) -> Vec<SavedToolPreset> {
        self.presets.iter().map(SavedToolPreset::from).collect()
    }

    /// Replace the list with one read back from the preferences file. An
    /// entry naming a tool this build does not have is dropped. Does not
    /// count as a user edit.
    pub fn restore_saved(&mut self, saved: &[SavedToolPreset]) {
        self.presets = saved
            .iter()
            .filter_map(SavedToolPreset::to_preset)
            .collect();
    }
}

/// A tool preset as a preferences file keeps it: the tool by its registry
/// name, each option value tagged with its kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedToolPreset {
    pub name: String,
    pub tool: String,
    #[serde(default)]
    pub values: Vec<(String, SavedValue)>,
    #[serde(default)]
    pub gradient: Option<Gradient>,
}

/// [`OptionValue`] with serde.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SavedValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    Choice(usize),
    Color([f32; 4]),
}

impl From<OptionValue> for SavedValue {
    fn from(v: OptionValue) -> Self {
        match v {
            OptionValue::Float(f) => SavedValue::Float(f),
            OptionValue::Int(i) => SavedValue::Int(i),
            OptionValue::Bool(b) => SavedValue::Bool(b),
            OptionValue::Choice(c) => SavedValue::Choice(c),
            OptionValue::Color(c) => SavedValue::Color(c),
        }
    }
}

impl From<SavedValue> for OptionValue {
    fn from(v: SavedValue) -> Self {
        match v {
            SavedValue::Float(f) => OptionValue::Float(f),
            SavedValue::Int(i) => OptionValue::Int(i),
            SavedValue::Bool(b) => OptionValue::Bool(b),
            SavedValue::Choice(c) => OptionValue::Choice(c),
            SavedValue::Color(c) => OptionValue::Color(c),
        }
    }
}

impl From<&ToolPreset> for SavedToolPreset {
    fn from(p: &ToolPreset) -> Self {
        Self {
            name: p.name.clone(),
            tool: tools::registry::info(p.tool).map_or_else(String::new, |i| i.name.to_string()),
            values: p
                .values
                .iter()
                .map(|(k, v)| (k.clone(), SavedValue::from(*v)))
                .collect(),
            gradient: p.gradient.clone(),
        }
    }
}

impl SavedToolPreset {
    /// Every value is a finite number — a NaN compares unequal to itself, so
    /// a list holding one would be rewritten to the preferences file every
    /// frame.
    pub fn is_finite(&self) -> bool {
        self.values.iter().all(|(_, v)| match v {
            SavedValue::Float(f) => f.is_finite(),
            SavedValue::Color(c) => c.iter().all(|x| x.is_finite()),
            SavedValue::Int(_) | SavedValue::Bool(_) | SavedValue::Choice(_) => true,
        })
    }

    /// The preset this entry names, or `None` for a tool this build lacks.
    pub fn to_preset(&self) -> Option<ToolPreset> {
        let tool = tools::registry::all()
            .iter()
            .find(|i| i.name == self.tool)?
            .id;
        Some(ToolPreset {
            name: self.name.clone(),
            tool,
            values: self
                .values
                .iter()
                .map(|(k, v)| (k.clone(), OptionValue::from(*v)))
                .collect(),
            gradient: self.gradient.clone(),
        })
    }
}

/// Stable ids for a headless test.
pub mod ids {
    pub fn new() -> egui::Id {
        egui::Id::new("raster-tool-presets-new")
    }
    pub fn delete() -> egui::Id {
        egui::Id::new("raster-tool-presets-delete")
    }
    /// The row of preset `index`; a click applies it.
    pub fn row(index: usize) -> egui::Id {
        egui::Id::new(("raster-tool-presets-row", index))
    }
}

fn selected_key() -> egui::Id {
    egui::Id::new("raster-tool-presets-selected")
}

/// Draw the panel.
pub(crate) fn tool_presets_body(w: &mut Workspace, ui: &mut Ui) {
    let mut selected: Option<usize> = ui
        .data(|d| d.get_temp(selected_key()))
        .filter(|i| *i < w.tool_presets.presets.len());
    if w.tool_presets.presets.is_empty() {
        ui.label(hint(ui, tr(NO_PRESETS)));
    }
    let mut apply: Option<usize> = None;
    for (index, preset) in w.tool_presets.presets.iter().enumerate() {
        let tool_name = tools::registry::info(preset.tool).map_or("", |i| i.name);
        let response = list_row_layout(ui, ids::row(index), selected == Some(index), |ui| {
            ui.add_space(Space::XSmall.pt());
            ui.label(body(ui, preset.name.clone()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(Space::XSmall.pt());
                ui.label(text(
                    ui,
                    tool_name,
                    TextRole::Secondary,
                    design::TypeRole::Caption,
                ));
            });
        })
        .response;
        if response.clicked() {
            apply = Some(index);
        }
    }
    if let Some(index) = apply {
        selected = Some(index);
        ToolPresetsState::apply(w, index);
    }

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    let t = current_tokens(ui);
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), t.metrics.control_height),
        Layout::right_to_left(Align::Center),
        |ui| {
            let can_delete = if selected.is_some() {
                ActionState::Idle
            } else {
                ActionState::Disabled
            };
            if icon_action_id(ui, "trash", tr(DELETE), can_delete, Some(ids::delete())).clicked() {
                if let Some(index) = selected.take() {
                    w.tool_presets.remove(index);
                }
            }
            if icon_action_id(ui, "plus", tr(NEW), ActionState::Idle, Some(ids::new())).clicked() {
                let tool = w.palette.active();
                let name = w.tool_presets.next_name(tool);
                let preset = ToolPreset::capture(name, tool, &w.options);
                selected = Some(w.tool_presets.add(preset));
            }
        },
    );
    ui.data_mut(|d| match selected {
        Some(i) => d.insert_temp(selected_key(), i),
        None => d.remove::<usize>(selected_key()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W10-B: every string the panel shows is a catalogue key that resolves.
    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [NO_PRESETS, NEW, DELETE] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn a_preset_round_trips_through_its_saved_form() {
        let mut options = crate::ToolOptions::new();
        assert!(options.set(ToolId::Brush, "size", OptionValue::Float(42.0)));
        let preset = ToolPreset::capture("Big", ToolId::Brush, &options);
        assert_eq!(
            preset.values,
            vec![("size".to_string(), OptionValue::Float(42.0))]
        );
        let json = serde_json::to_string(&SavedToolPreset::from(&preset)).unwrap();
        let back: SavedToolPreset = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_preset(), Some(preset));
    }

    #[test]
    fn an_unknown_tool_is_dropped_on_restore() {
        let mut state = ToolPresetsState::new();
        state.restore_saved(&[SavedToolPreset {
            name: "x".into(),
            tool: "No Such Tool".into(),
            values: Vec::new(),
            gradient: None,
        }]);
        assert!(state.presets().is_empty());
        assert_eq!(state.edits(), 0);
    }
}
