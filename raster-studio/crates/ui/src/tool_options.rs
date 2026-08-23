//! The options bar: the active tool's settings, driven by the registry schema.
//!
//! There is no `match` over [`ToolId`] in this module and there must never be
//! one. `tools::registry` publishes an [`OptionSpec`] list per tool precisely so
//! that a new tool appears in the options bar without anybody editing the UI;
//! a match here would re-open the hole the registry exists to close.
//!
//! # Two things the registry does not describe yet
//!
//! Both are added by *capability*, never by tool identity, so a new tool
//! inherits them or does not on the same rule as every existing one:
//!
//! * **Paint blend mode.** Every stamping tool composites its stroke through a
//!   blend mode, and the registry schema has no slot for it. It is offered to
//!   the [`ToolGroup::Paint`] and [`ToolGroup::Retouch`] groups.
//! * **Gradient stops.** A ramp is not expressible as a `Float`/`Choice`, so
//!   the stop editor is offered to any tool whose schema declares a `shape`
//!   choice — see [`wants_gradient_stops`].
//!
//! The blend mode is a `Choice` like any other, so it lives in the same
//! [`ToolOptions`] map and travels on the same [`crate::Intent::SetToolOption`].
//! A ramp cannot: it is a list of stops rather than a scalar, so it is stored
//! in its own map here and travels as [`crate::Intent::SetToolGradient`]. The
//! options bar's Reset is a third: it clears both maps for one tool at once and
//! says so as [`crate::Intent::ResetToolOptions`]. Three intents, and every
//! control in the bar posts one of them — nothing in the bar writes state the
//! application cannot see.

use std::collections::HashMap;

use layer_model::{BlendMode, Gradient, GradientStop};
use selection::BooleanOp;
use tools::{BrushSettings, OptionKind, OptionSpec, ToolGroup, ToolId, ToolInfo};

/// The value behind one option key.
///
/// The variant must match its [`OptionKind`]; [`OptionValue::conform`] is the
/// one place that is enforced, and every write goes through it. A settings file
/// that names `size` as a bool therefore loses its bool rather than poisoning a
/// brush.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum OptionValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    /// Index into the spec's `choices`.
    Choice(usize),
    /// Straight-alpha sRGB.
    Color([f32; 4]),
}

impl OptionValue {
    /// The value a spec starts at.
    pub fn default_for(kind: &OptionKind) -> Self {
        match *kind {
            OptionKind::Float { default, .. } => OptionValue::Float(default),
            OptionKind::Int { default, .. } => OptionValue::Int(default),
            OptionKind::Bool { default } => OptionValue::Bool(default),
            OptionKind::Choice { default, .. } => OptionValue::Choice(default),
            OptionKind::Color { default } => OptionValue::Color(default),
        }
    }

    /// This value forced into `kind`'s shape and range, or `None` when the two
    /// are different kinds entirely.
    ///
    /// Clamping rather than rejecting is deliberate for the *in-range* case: a
    /// drag that overshoots a slider is a normal thing for a pointer to do, and
    /// the tool must not see a size of `-4`. A non-finite float, which no
    /// slider produces but a preset file can carry, falls back to the spec
    /// default — there is no sensible clamp for a NaN.
    pub fn conform(self, kind: &OptionKind) -> Option<Self> {
        Some(match (self, *kind) {
            (OptionValue::Float(v), OptionKind::Float { min, max, default }) => {
                OptionValue::Float(if v.is_finite() {
                    v.clamp(min.min(max), max.max(min))
                } else {
                    default
                })
            }
            (OptionValue::Int(v), OptionKind::Int { min, max, .. }) => {
                OptionValue::Int(v.clamp(min.min(max), max.max(min)))
            }
            (OptionValue::Bool(v), OptionKind::Bool { .. }) => OptionValue::Bool(v),
            (OptionValue::Choice(v), OptionKind::Choice { choices, default }) => {
                OptionValue::Choice(if choices.is_empty() {
                    default
                } else {
                    v.min(choices.len() - 1)
                })
            }
            (OptionValue::Color(c), OptionKind::Color { default }) => {
                OptionValue::Color(if c.iter().all(|v| v.is_finite()) {
                    [
                        c[0].clamp(0.0, 1.0),
                        c[1].clamp(0.0, 1.0),
                        c[2].clamp(0.0, 1.0),
                        c[3].clamp(0.0, 1.0),
                    ]
                } else {
                    default
                })
            }
            _ => return None,
        })
    }

    pub fn as_float(self) -> Option<f32> {
        match self {
            OptionValue::Float(v) => Some(v),
            OptionValue::Int(v) => Some(v as f32),
            _ => None,
        }
    }

    pub fn as_int(self) -> Option<i32> {
        match self {
            OptionValue::Int(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_bool(self) -> Option<bool> {
        match self {
            OptionValue::Bool(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_choice(self) -> Option<usize> {
        match self {
            OptionValue::Choice(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_color(self) -> Option<[f32; 4]> {
        match self {
            OptionValue::Color(c) => Some(c),
            _ => None,
        }
    }
}

/// Every blend-mode label, in menu order.
///
/// Built from `BlendMode::ALL` in a const block rather than typed out, so a
/// twenty-eighth mode reaches the options bar with no edit here.
pub const BLEND_MODE_LABELS: [&str; BlendMode::ALL.len()] = {
    let mut out = [""; BlendMode::ALL.len()];
    let mut i = 0;
    while i < BlendMode::ALL.len() {
        out[i] = BlendMode::ALL[i].label();
        i += 1;
    }
    out
};

/// Key of the UI-supplied paint blend mode. Not a registry key — see the
/// module note.
pub const BLEND_MODE_KEY: &str = "ui.blend_mode";

/// The blend-mode option, offered to the painting and retouching groups.
pub fn blend_mode_spec() -> OptionSpec {
    OptionSpec {
        key: BLEND_MODE_KEY,
        label: "Mode",
        kind: OptionKind::Choice {
            choices: &BLEND_MODE_LABELS,
            default: 0,
        },
    }
}

/// `true` when a tool composites a stroke and therefore wants a blend mode.
pub fn wants_blend_mode(info: &ToolInfo) -> bool {
    matches!(info.group, ToolGroup::Paint | ToolGroup::Retouch)
}

/// `true` when a tool draws a ramp and therefore wants a stop editor.
///
/// Decided from the *schema*, not from the tool's identity: a tool that offers
/// a `shape` choice is choosing between ramp geometries, and every such tool
/// needs stops. A second gradient tool would inherit the editor for free.
///
/// The key and its kind are the whole contract. This used to also require the
/// option's *label* to read "Style", which is a display string — a second ramp
/// tool calling the same choice "Type", or shipping a translated label, would
/// have been refused the editor by a predicate whose comment promised it was
/// looking at the schema.
/// `a_second_ramp_tool_inherits_the_stop_editor_whatever_its_label_reads` pins
/// that the label no longer decides.
pub fn wants_gradient_stops(info: &ToolInfo) -> bool {
    info.options
        .iter()
        .any(|o| o.key == "shape" && matches!(o.kind, OptionKind::Choice { .. }))
}

/// The full option list for a tool: its registry schema plus any capability
/// extras, in the order the options bar draws them.
pub fn schema_for(info: &ToolInfo) -> Vec<OptionSpec> {
    let mut specs: Vec<OptionSpec> = Vec::with_capacity(info.options.len() + 1);
    if wants_blend_mode(info) {
        specs.push(blend_mode_spec());
    }
    specs.extend_from_slice(info.options);
    specs
}

/// The settings a selection tool is currently configured with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SelectionOptions {
    pub mode: BooleanOp,
    /// Feather radius in document pixels.
    pub feather: f32,
    pub antialias: bool,
}

/// The four boolean modes the selection schema offers, in schema order.
const SELECTION_MODES: [BooleanOp; 4] = [
    BooleanOp::Replace,
    BooleanOp::Add,
    BooleanOp::Subtract,
    BooleanOp::Intersect,
];

/// Per-tool option values, defaulted from the registry.
///
/// Only values the user has actually changed are stored; everything else falls
/// back to the schema default on read. That is what makes "reset this tool"
/// a removal rather than a re-derivation, and it keeps a saved preset small.
#[derive(Clone, Default, Debug)]
pub struct ToolOptions {
    values: HashMap<(ToolId, &'static str), OptionValue>,
    gradients: HashMap<ToolId, Gradient>,
}

impl ToolOptions {
    pub fn new() -> Self {
        Self::default()
    }

    fn spec(tool: ToolId, key: &str) -> Option<OptionSpec> {
        let info = tools::registry::info(tool)?;
        if key == BLEND_MODE_KEY && wants_blend_mode(info) {
            return Some(blend_mode_spec());
        }
        info.options.iter().copied().find(|o| o.key == key)
    }

    /// The current value of one option, or `None` when the tool has no such
    /// option. Unset options answer with their schema default.
    pub fn get(&self, tool: ToolId, key: &str) -> Option<OptionValue> {
        let spec = Self::spec(tool, key)?;
        Some(match self.values.get(&(tool, spec.key)) {
            Some(v) => *v,
            None => OptionValue::default_for(&spec.kind),
        })
    }

    /// Write one option.
    ///
    /// Returns `true` when the stored value actually changed, so a caller can
    /// avoid emitting an intent per frame while a slider is merely hovered. A
    /// key the tool does not have, or a value of the wrong kind, is refused and
    /// answers `false`.
    pub fn set(&mut self, tool: ToolId, key: &str, value: OptionValue) -> bool {
        let Some(spec) = Self::spec(tool, key) else {
            return false;
        };
        let Some(conformed) = value.conform(&spec.kind) else {
            return false;
        };
        let before = self.get(tool, spec.key);
        if before == Some(conformed) {
            return false;
        }
        self.values.insert((tool, spec.key), conformed);
        true
    }

    /// Forget every change made to one tool, returning it to the schema
    /// defaults.
    ///
    /// Returns `true` when something was actually forgotten, so the options
    /// bar's Reset does not post an intent on a tool that was already at its
    /// defaults.
    pub fn reset(&mut self, tool: ToolId) -> bool {
        let changed = !self.is_default(tool);
        self.values.retain(|(t, _), _| *t != tool);
        self.gradients.remove(&tool);
        changed
    }

    /// Forget every change to every tool.
    pub fn reset_all(&mut self) {
        self.values.clear();
        self.gradients.clear();
    }

    /// `true` when a tool is entirely at its defaults.
    pub fn is_default(&self, tool: ToolId) -> bool {
        !self.values.keys().any(|(t, _)| *t == tool) && !self.gradients.contains_key(&tool)
    }

    fn float(&self, tool: ToolId, key: &str) -> Option<f32> {
        self.get(tool, key).and_then(OptionValue::as_float)
    }

    fn flag(&self, tool: ToolId, key: &str) -> Option<bool> {
        self.get(tool, key).and_then(OptionValue::as_bool)
    }

    fn choice(&self, tool: ToolId, key: &str) -> Option<usize> {
        self.get(tool, key).and_then(OptionValue::as_choice)
    }

    /// The brush the tool would stamp with right now.
    ///
    /// Every field falls back to [`BrushSettings::default`] when the tool's
    /// schema has no such key, so a tool that exposes only `size` still gets a
    /// coherent brush rather than a zeroed one.
    pub fn brush_settings(&self, tool: ToolId) -> BrushSettings {
        let d = BrushSettings::default();
        BrushSettings {
            size: self.float(tool, "size").unwrap_or(d.size),
            hardness: self.float(tool, "hardness").unwrap_or(d.hardness),
            spacing: self.float(tool, "spacing").unwrap_or(d.spacing),
            angle: self.float(tool, "angle").unwrap_or(d.angle),
            roundness: self.float(tool, "roundness").unwrap_or(d.roundness),
            opacity: self.float(tool, "opacity").unwrap_or(d.opacity),
            flow: self.float(tool, "flow").unwrap_or(d.flow),
            smoothing: self.float(tool, "smoothing").unwrap_or(d.smoothing),
            size_pressure: self.flag(tool, "size_pressure").unwrap_or(d.size_pressure),
            flow_pressure: self.flag(tool, "flow_pressure").unwrap_or(d.flow_pressure),
            min_size_ratio: d.min_size_ratio,
            aliased: d.aliased,
        }
    }

    /// The boolean mode, feather and anti-aliasing a selection tool would use.
    ///
    /// `None` for a tool whose schema declares no `mode` choice — that is the
    /// registry's own definition of "not a selection tool".
    pub fn selection_options(&self, tool: ToolId) -> Option<SelectionOptions> {
        let index = self.choice(tool, "mode")?;
        Some(SelectionOptions {
            mode: SELECTION_MODES
                .get(index)
                .copied()
                .unwrap_or(BooleanOp::Replace),
            feather: self.float(tool, "feather").unwrap_or(0.0),
            antialias: self.flag(tool, "antialias").unwrap_or(true),
        })
    }

    /// The blend mode a painting tool composites through.
    ///
    /// `None` for a tool that does not paint.
    pub fn blend_mode(&self, tool: ToolId) -> Option<BlendMode> {
        let index = self.choice(tool, BLEND_MODE_KEY)?;
        BlendMode::ALL.get(index).copied()
    }

    /// The ramp a gradient tool draws. Defaults to black-to-white.
    pub fn gradient(&self, tool: ToolId) -> Gradient {
        self.gradients.get(&tool).cloned().unwrap_or_default()
    }

    /// Replace the ramp, normalising it first.
    ///
    /// A ramp is normalised rather than validated: stops arrive from a drag, so
    /// they can be out of order and outside `0..=1`, and refusing the drag is
    /// not an option. Fewer than two stops is not a ramp at all, so it is
    /// padded from the default rather than left degenerate.
    pub fn set_gradient(&mut self, tool: ToolId, gradient: Gradient) -> bool {
        let normalised = normalise_gradient(gradient);
        if self.gradients.get(&tool) == Some(&normalised) {
            return false;
        }
        self.gradients.insert(tool, normalised);
        true
    }
}

/// Sort a ramp's stops by position, clamp them into `0..=1`, and guarantee at
/// least two.
pub fn normalise_gradient(mut gradient: Gradient) -> Gradient {
    for stop in gradient.stops.iter_mut().chain(&mut gradient.alpha_stops) {
        stop.position = if stop.position.is_finite() {
            stop.position.clamp(0.0, 1.0)
        } else {
            0.0
        };
        stop.midpoint = if stop.midpoint.is_finite() {
            stop.midpoint.clamp(0.0, 1.0)
        } else {
            0.5
        };
    }
    gradient
        .stops
        .sort_by(|a, b| a.position.total_cmp(&b.position));
    gradient
        .alpha_stops
        .sort_by(|a, b| a.position.total_cmp(&b.position));
    let fallback = Gradient::default();
    while gradient.stops.len() < 2 {
        let index = gradient.stops.len();
        gradient.stops.push(GradientStop {
            position: fallback.stops[index].position,
            color: fallback.stops[index].color,
            midpoint: 0.5,
        });
    }
    if !gradient.smoothness.is_finite() {
        gradient.smoothness = fallback.smoothness;
    }
    gradient
}

/// The fewest stops a ramp can have and still be a ramp.
pub const MIN_GRADIENT_STOPS: usize = 2;

/// Whether the stop editor may offer to remove a stop.
///
/// The question is how many stops would be *left*, which is what the disabled
/// tooltip says. Gating on the stop's index instead — which is what this
/// replaced — left the first two stops of a four-stop ramp permanently
/// un-removable under a reason that was not true of them.
pub const fn can_remove_gradient_stop(stop_count: usize) -> bool {
    stop_count > MIN_GRADIENT_STOPS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: ToolId) -> &'static ToolInfo {
        tools::registry::info(id).expect("every ToolId is in the registry")
    }

    #[test]
    fn every_tool_starts_at_its_registry_defaults() {
        let opts = ToolOptions::new();
        for tool in ToolId::ALL {
            for spec in info(*tool).options {
                assert_eq!(
                    opts.get(*tool, spec.key),
                    Some(OptionValue::default_for(&spec.kind)),
                    "{tool:?}/{} did not start at its schema default",
                    spec.key
                );
            }
        }
    }

    #[test]
    fn the_brush_defaults_come_from_the_brush_schema() {
        let opts = ToolOptions::new();
        let b = opts.brush_settings(ToolId::Brush);
        assert_eq!(b.size, 24.0);
        assert_eq!(b.hardness, 0.8);
        assert_eq!(b.spacing, 0.25);
        assert_eq!(b.opacity, 1.0);
        assert_eq!(b.flow, 1.0);
        assert!(b.size_pressure);
        assert!(!b.flow_pressure);
    }

    #[test]
    fn a_different_tool_gets_its_own_defaults_not_the_brushs() {
        let opts = ToolOptions::new();
        // The tone tools declare a bigger, softer brush than the paint brush.
        let dodge = opts.brush_settings(ToolId::Dodge);
        assert_eq!(dodge.size, 60.0);
        assert_eq!(dodge.hardness, 0.0);
        // The clone stamp declares its own again.
        let clone = opts.brush_settings(ToolId::CloneStamp);
        assert_eq!(clone.size, 40.0);
        assert_eq!(clone.hardness, 0.5);
        assert_eq!(clone.spacing, 0.05);
    }

    #[test]
    fn setting_one_tools_size_leaves_every_other_tool_alone() {
        let mut opts = ToolOptions::new();
        assert!(opts.set(ToolId::Brush, "size", OptionValue::Float(120.0)));
        assert_eq!(opts.brush_settings(ToolId::Brush).size, 120.0);
        assert_eq!(opts.brush_settings(ToolId::Eraser).size, 24.0);
    }

    #[test]
    fn an_out_of_range_write_is_clamped_into_the_schema_range() {
        let mut opts = ToolOptions::new();
        opts.set(ToolId::Brush, "size", OptionValue::Float(1e9));
        assert_eq!(opts.brush_settings(ToolId::Brush).size, 5000.0);
        opts.set(ToolId::Brush, "hardness", OptionValue::Float(-3.0));
        assert_eq!(opts.brush_settings(ToolId::Brush).hardness, 0.0);
    }

    #[test]
    fn a_nan_write_falls_back_to_the_schema_default() {
        let mut opts = ToolOptions::new();
        opts.set(ToolId::Brush, "size", OptionValue::Float(f32::NAN));
        assert_eq!(opts.brush_settings(ToolId::Brush).size, 24.0);
    }

    #[test]
    fn a_value_of_the_wrong_kind_is_refused() {
        let mut opts = ToolOptions::new();
        assert!(!opts.set(ToolId::Brush, "size", OptionValue::Bool(true)));
        assert_eq!(opts.brush_settings(ToolId::Brush).size, 24.0);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let mut opts = ToolOptions::new();
        assert!(!opts.set(ToolId::Brush, "nonesuch", OptionValue::Float(1.0)));
        assert_eq!(opts.get(ToolId::Brush, "nonesuch"), None);
    }

    #[test]
    fn writing_the_value_it_already_holds_reports_no_change() {
        let mut opts = ToolOptions::new();
        assert!(!opts.set(ToolId::Brush, "size", OptionValue::Float(24.0)));
        assert!(opts.set(ToolId::Brush, "size", OptionValue::Float(25.0)));
        assert!(!opts.set(ToolId::Brush, "size", OptionValue::Float(25.0)));
    }

    #[test]
    fn resetting_a_tool_returns_it_to_the_schema_defaults() {
        let mut opts = ToolOptions::new();
        opts.set(ToolId::Brush, "size", OptionValue::Float(300.0));
        assert!(!opts.is_default(ToolId::Brush));
        assert!(opts.reset(ToolId::Brush));
        assert!(opts.is_default(ToolId::Brush));
        assert_eq!(opts.brush_settings(ToolId::Brush).size, 24.0);
    }

    #[test]
    fn resetting_a_tool_that_is_already_default_reports_no_change() {
        let mut opts = ToolOptions::new();
        assert!(!opts.reset(ToolId::Brush));
        // A ramp counts as a change even though it is not in the value map.
        let mut ramp = Gradient::default();
        ramp.stops[0].color = [1.0, 0.0, 0.0, 1.0];
        assert!(opts.set_gradient(ToolId::Gradient, ramp));
        assert!(opts.reset(ToolId::Gradient));
        assert!(!opts.reset(ToolId::Gradient));
    }

    #[test]
    fn the_selection_mode_choice_maps_onto_the_boolean_ops() {
        let mut opts = ToolOptions::new();
        let sel = opts
            .selection_options(ToolId::RectMarquee)
            .expect("the marquee declares a mode");
        assert_eq!(sel.mode, BooleanOp::Replace);
        assert_eq!(sel.feather, 0.0);
        assert!(sel.antialias);

        opts.set(ToolId::RectMarquee, "mode", OptionValue::Choice(2));
        opts.set(ToolId::RectMarquee, "feather", OptionValue::Float(12.0));
        let sel = opts.selection_options(ToolId::RectMarquee).unwrap();
        assert_eq!(sel.mode, BooleanOp::Subtract);
        assert_eq!(sel.feather, 12.0);
    }

    #[test]
    fn an_out_of_range_mode_index_is_clamped_to_the_last_choice() {
        let mut opts = ToolOptions::new();
        opts.set(ToolId::RectMarquee, "mode", OptionValue::Choice(99));
        let sel = opts.selection_options(ToolId::RectMarquee).unwrap();
        assert_eq!(sel.mode, BooleanOp::Intersect);
    }

    #[test]
    fn a_tool_with_no_mode_choice_has_no_selection_options() {
        let opts = ToolOptions::new();
        assert!(opts.selection_options(ToolId::Brush).is_none());
        assert!(opts.selection_options(ToolId::Hand).is_none());
    }

    #[test]
    fn painting_tools_get_a_blend_mode_and_navigation_tools_do_not() {
        let mut opts = ToolOptions::new();
        assert_eq!(opts.blend_mode(ToolId::Brush), Some(BlendMode::Normal));
        assert_eq!(opts.blend_mode(ToolId::Hand), None);
        assert_eq!(opts.blend_mode(ToolId::RectMarquee), None);
        assert!(opts.set(
            ToolId::Brush,
            BLEND_MODE_KEY,
            OptionValue::Choice(BlendMode::Multiply.shader_index() as usize)
        ));
        // The choice index is the position in BlendMode::ALL, not the shader
        // index, so look it up the way the UI does.
        let multiply = BlendMode::ALL
            .iter()
            .position(|m| *m == BlendMode::Multiply)
            .unwrap();
        opts.set(ToolId::Brush, BLEND_MODE_KEY, OptionValue::Choice(multiply));
        assert_eq!(opts.blend_mode(ToolId::Brush), Some(BlendMode::Multiply));
    }

    #[test]
    fn the_blend_mode_labels_cover_every_mode_in_order() {
        assert_eq!(BLEND_MODE_LABELS.len(), BlendMode::ALL.len());
        for (label, mode) in BLEND_MODE_LABELS.iter().zip(BlendMode::ALL) {
            assert_eq!(*label, mode.label());
        }
    }

    #[test]
    fn only_the_ramp_tool_asks_for_a_stop_editor() {
        assert!(wants_gradient_stops(info(ToolId::Gradient)));
        for tool in ToolId::ALL.iter().filter(|t| **t != ToolId::Gradient) {
            assert!(
                !wants_gradient_stops(info(*tool)),
                "{tool:?} claimed a gradient stop editor"
            );
        }
    }

    /// The claim in `wants_gradient_stops`'s own doc comment, tested.
    ///
    /// There is only one ramp tool in the registry, so the predicate's promise
    /// — "a second gradient tool would inherit the editor for free" — cannot be
    /// checked against it. Declare that second tool here instead. It differs
    /// from `Gradient` in exactly one way: the choice is labelled "Ramp" rather
    /// than "Style". While the predicate also compared the label, this tool got
    /// no stop editor, which is the identity coupling the comment says it
    /// avoids, hidden behind a display string.
    #[test]
    fn a_second_ramp_tool_inherits_the_stop_editor_whatever_its_label_reads() {
        const RAMP: &[OptionSpec] = &[OptionSpec {
            key: "shape",
            label: "Ramp",
            kind: OptionKind::Choice {
                choices: &["Linear", "Radial"],
                default: 0,
            },
        }];
        let second = ToolInfo {
            options: RAMP,
            ..*info(ToolId::Gradient)
        };
        assert!(wants_gradient_stops(&second));

        // …and the key is still doing the work: a choice under another key,
        // however it is labelled, is not a ramp.
        const NOT_A_RAMP: &[OptionSpec] = &[OptionSpec {
            key: "mode",
            label: "Style",
            kind: OptionKind::Choice {
                choices: &["Linear", "Radial"],
                default: 0,
            },
        }];
        let impostor = ToolInfo {
            options: NOT_A_RAMP,
            ..*info(ToolId::Gradient)
        };
        assert!(!wants_gradient_stops(&impostor));
    }

    /// `wants_gradient_stops`'s doc comment cites the test above by name.
    /// Naming it as a function pointer turns a rename into a compile error, so
    /// the citation cannot rot into a backticked identifier that resolves to
    /// nothing — which is exactly how it was wrong before.
    #[test]
    fn the_doc_comment_cites_a_test_that_really_exists() {
        let cited: fn() = a_second_ramp_tool_inherits_the_stop_editor_whatever_its_label_reads;
        cited();
    }

    #[test]
    fn the_schema_puts_blend_mode_first_and_keeps_the_registry_order_after_it() {
        let brush = schema_for(info(ToolId::Brush));
        assert_eq!(brush[0].key, BLEND_MODE_KEY);
        let rest: Vec<&str> = brush[1..].iter().map(|s| s.key).collect();
        let registry: Vec<&str> = info(ToolId::Brush).options.iter().map(|s| s.key).collect();
        assert_eq!(rest, registry);

        // A tool that does not paint gets exactly its registry schema.
        let hand = schema_for(info(ToolId::Hand));
        assert_eq!(hand.len(), info(ToolId::Hand).options.len());
    }

    #[test]
    fn every_tool_in_the_registry_produces_a_drawable_schema() {
        for tool in ToolId::ALL {
            let specs = schema_for(info(*tool));
            for spec in &specs {
                assert!(!spec.key.is_empty(), "{tool:?} has an unkeyed option");
                assert!(!spec.label.is_empty(), "{tool:?}/{} unlabelled", spec.key);
                if let OptionKind::Choice { choices, default } = spec.kind {
                    assert!(!choices.is_empty(), "{tool:?}/{} has no choices", spec.key);
                    assert!(
                        default < choices.len(),
                        "{tool:?}/{} defaults out of range",
                        spec.key
                    );
                }
            }
            let mut keys: Vec<&str> = specs.iter().map(|s| s.key).collect();
            keys.sort_unstable();
            let count = keys.len();
            keys.dedup();
            assert_eq!(keys.len(), count, "{tool:?} declares a key twice");
        }
    }

    #[test]
    fn a_gradient_is_sorted_clamped_and_never_shorter_than_two_stops() {
        let mut opts = ToolOptions::new();
        let messy = Gradient {
            stops: vec![GradientStop {
                position: 9.0,
                color: [1.0, 0.0, 0.0, 1.0],
                midpoint: f32::NAN,
            }],
            alpha_stops: Vec::new(),
            smoothness: f32::INFINITY,
        };
        assert!(opts.set_gradient(ToolId::Gradient, messy));
        let g = opts.gradient(ToolId::Gradient);
        assert!(g.stops.len() >= 2);
        assert!(g.stops.windows(2).all(|w| w[0].position <= w[1].position));
        assert!(g.stops.iter().all(|s| (0.0..=1.0).contains(&s.position)));
        assert!(g.stops.iter().all(|s| s.midpoint.is_finite()));
        assert!(g.smoothness.is_finite());
    }

    #[test]
    fn the_default_ramp_is_black_to_white() {
        let opts = ToolOptions::new();
        let g = opts.gradient(ToolId::Gradient);
        assert_eq!(g.stops.len(), 2);
        assert_eq!(g.stops[0].color, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(g.stops[1].color, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn setting_the_same_ramp_twice_reports_no_change() {
        let mut opts = ToolOptions::new();
        let g = Gradient::default();
        assert!(opts.set_gradient(ToolId::Gradient, g.clone()));
        assert!(!opts.set_gradient(ToolId::Gradient, g));
    }

    #[test]
    fn any_stop_can_go_while_two_would_remain() {
        // The predicate is about the ramp, not about which stop was asked
        // about: with four stops every one of them is removable, and with two
        // none of them is. The index-based gate this replaced said the
        // opposite for the first two stops of any ramp.
        assert!(!can_remove_gradient_stop(0));
        assert!(!can_remove_gradient_stop(1));
        assert!(!can_remove_gradient_stop(MIN_GRADIENT_STOPS));
        assert!(can_remove_gradient_stop(MIN_GRADIENT_STOPS + 1));
        assert!(can_remove_gradient_stop(4));
    }

    #[test]
    fn a_ramp_never_normalises_below_the_minimum_the_editor_defends() {
        let bare = normalise_gradient(Gradient {
            stops: Vec::new(),
            ..Gradient::default()
        });
        assert_eq!(bare.stops.len(), MIN_GRADIENT_STOPS);
        assert!(!can_remove_gradient_stop(bare.stops.len()));
    }
}
