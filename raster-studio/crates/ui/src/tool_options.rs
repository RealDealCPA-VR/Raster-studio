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
//! * **Paint blend mode.** A tool that lays a source colour over the layer
//!   composites it through a blend mode, and the registry schema has no slot
//!   for it. It is offered to exactly the tools that answer it
//!   (`tools::composites_strokes`: Brush, Pencil, Clone Stamp and Pattern
//!   Stamp). The retouching strokes mix toward a computed target or erase
//!   and have no source colour to blend; they, the fills, the gradient, the
//!   magic eraser, Patch and Red Eye refuse the key and get no combo.
//! * **Gradient stops.** A ramp is not expressible as a `Float`/`Choice`, so
//!   the stop editor is offered to any tool whose schema declares a `shape`
//!   choice — see [`wants_gradient_stops`].
//!
//! The blend mode is a `Choice` like any other, so it lives in the same
//! [`ToolOptions`] map, travels on the same [`crate::Intent::SetToolOption`],
//! and forwards to the tool with the rest of the touched set ([`ToolOptions::held`])
//! under [`BLEND_MODE_KEY`] — the key the tools crate answers it by
//! (`tools::BLEND_MODE_KEY`; six tools composite through it: the four
//! source-over stroke tools Brush, Pencil, Clone Stamp and Pattern Stamp
//! their dabs, and since W9-L the Gradient its ramp and the Paint Bucket its
//! fill — exactly `tools::composites_strokes`). It used to be filtered out of the forward set as
//! a key "no tool could answer", which is what made the Mode combo a dead
//! control.
//! A ramp cannot: it is a list of stops rather than a scalar, so it is stored
//! in its own map here and travels as [`crate::Intent::SetToolGradient`]. The
//! options bar's Reset is a third: it clears both maps for one tool at once and
//! says so as [`crate::Intent::ResetToolOptions`]. Three intents, and every
//! control in the bar posts one of them — nothing in the bar writes state the
//! application cannot see.

use std::collections::HashMap;

use layer_model::{BlendMode, Gradient, GradientStop};
use selection::BooleanOp;
use tools::{BrushSettings, OptionKind, OptionSpec, ToolId, ToolInfo};

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
/// module note — but the tools crate's own constant, so the options bar and
/// `StrokeTool::set_setting` agree on it by construction.
pub const BLEND_MODE_KEY: &str = tools::BLEND_MODE_KEY;

/// The blend-mode option, offered to the source-over stroke tools.
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

/// `true` when a tool composites a source colour over the layer and
/// therefore wants a blend mode.
///
/// The tools crate owns the answer ([`tools::composites_strokes`]): Brush,
/// Pencil, Clone Stamp and Pattern Stamp, whose dabs are composited through
/// the mode. The retouching strokes, the fills, the gradient, the magic
/// eraser, Patch and Red Eye refuse the key, so the combo is offered to
/// exactly the tools whose `set_setting` answers it: a touched Mode is never
/// a refusal at the press and never a control that does nothing.
pub fn wants_blend_mode(info: &ToolInfo) -> bool {
    tools::composites_strokes(info.id)
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
    // W9-K: the Type tools' Font combo lists the installed families, not the
    // static table's three generics.
    if specs.iter().any(|s| s.key == FONT_FAMILY_KEY) {
        refresh_font_choices();
    }
    // W9-K / W9-N: the live Font and Custom Shape lists.
    for spec in &mut specs {
        *spec = live_spec(*spec);
    }
    specs
}

/// W9-K: the Type tools' font option key.
pub const FONT_FAMILY_KEY: &str = "font_family";

/// W9-K: register every family the compositor can shape with as a Type-tool
/// Font choice and return the live list - the three generics, then the
/// installed families (the same list the Character panel offers), then any
/// loaded later (File > Open of a font file), each keeping its index.
pub fn refresh_font_choices() -> &'static [&'static str] {
    tools::registry::register_font_families(compositor::font_families())
}

/// `spec` with the live Font choices when it is the Type tools' font option.
fn live_spec(spec: OptionSpec) -> OptionSpec {
    if spec.key == FONT_FAMILY_KEY && matches!(spec.kind, OptionKind::Choice { .. }) {
        tools::registry::type_font_spec()
    } else if is_custom_shape_spec(&spec) {
        // W9-N: the Custom Shape tool's Shape combo lists the built-in
        // library, then every custom shape a `.csh` import registered.
        tools::registry::custom_shape_spec()
    } else {
        spec
    }
}

/// W9-N: whether `spec` is the Custom Shape tool's Shape option - its key,
/// with the built-in library as the static table's choices (another tool's
/// `preset` option lists something else).
fn is_custom_shape_spec(spec: &OptionSpec) -> bool {
    spec.key == tools::registry::CUSTOM_SHAPE_KEY
        && matches!(spec.kind, OptionKind::Choice { choices, .. }
            if choices.get(..vector::CUSTOM_SHAPE_NAMES.len()) == Some(&vector::CUSTOM_SHAPE_NAMES[..]))
}

/// W4-D round 2: whether the options bar shows `key` for `tool` given what
/// the tool's other options hold now. The Crop tool's W, H, Units and
/// Resolution appear only under its W x H x Resolution Ratio preset
/// ([`tools::edit::crop_option_shown`]); every other option always shows.
pub fn is_shown(options: &ToolOptions, tool: ToolId, key: &str) -> bool {
    match tool {
        ToolId::Crop => {
            let ratio = options
                .get(tool, "ratio")
                .and_then(OptionValue::as_choice)
                .unwrap_or(0);
            tools::edit::crop_option_shown(key, ratio)
        }
        // W10-J: Content-Aware Scale's Amount only in the Content-Aware mode.
        ToolId::FreeTransform => {
            let mode = options
                .get(tool, "mode")
                .and_then(OptionValue::as_choice)
                .unwrap_or(0);
            tools::transform::TransformMode::option_shown(key, mode)
        }
        _ => true,
    }
}

/// W4-D round 2: [`schema_for`] less the options [`is_shown`] hides right
/// now — what the options bar draws.
pub fn shown_schema(options: &ToolOptions, info: &ToolInfo) -> Vec<OptionSpec> {
    schema_for(info)
        .into_iter()
        .filter(|spec| is_shown(options, info.id, spec.key))
        .collect()
}

/// The settings a selection tool is currently configured with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SelectionOptions {
    pub mode: BooleanOp,
    /// Feather radius in document pixels.
    pub feather: f32,
    pub antialias: bool,
}

/// The boolean modes the selection schema offers, in schema order.
/// W9-L: the tools crate's own table (Exclude included), so this read-back
/// and the tool cannot disagree about an index.
const SELECTION_MODES: &[BooleanOp] = tools::select::SELECTION_MODES;

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

    /// Cross-crate test seam: the option spec lookup, unconditionally public
    /// (a dependency's `#[cfg(test)]` does not propagate).
    pub fn spec_for_test(tool: ToolId, key: &str) -> Option<OptionSpec> {
        Self::spec(tool, key)
    }

    fn spec(tool: ToolId, key: &str) -> Option<OptionSpec> {
        let info = tools::registry::info(tool)?;
        if key == BLEND_MODE_KEY && wants_blend_mode(info) {
            return Some(blend_mode_spec());
        }
        info.options
            .iter()
            .copied()
            .find(|o| o.key == key)
            .map(live_spec)
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

    /// The options the user has actually TOUCHED for one tool, as
    /// `(key, value)` pairs — the forward-to-tool set. Untouched options are
    /// deliberately absent: they keep their schema defaults, and forwarding
    /// them would demand `set_setting` answers for keys no tool implements.
    ///
    /// The UI-supplied paint blend mode ([`BLEND_MODE_KEY`]) forwards like
    /// any other touched Choice: the source-over stroke tools answer it (a
    /// Multiply brush composites its dabs with Multiply), and a tool that
    /// does not lay colour over the layer is never offered the control, so
    /// it never holds the key.
    pub fn held(&self, tool: ToolId) -> Vec<(String, OptionValue)> {
        self.values
            .iter()
            .filter(|((t, key), _)| *t == tool && Self::spec(*t, key).is_some())
            .map(|((_, key), value)| ((*key).to_string(), *value))
            .collect()
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
            opacity_pressure: self
                .flag(tool, "opacity_pressure")
                .unwrap_or(d.opacity_pressure),
            min_size_ratio: d.min_size_ratio,
            aliased: d.aliased,
            ..d
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

    /// W4-D: the Crop bar is Photopea's — a Ratio preset, W / H / Units /
    /// Resolution, the Overlay, Straighten and Delete Cropped Pixels — not a
    /// bare 0-100 aspect slider, and every one of them forwards to the tool
    /// once touched.
    #[test]
    fn the_crop_bar_offers_presets_size_overlay_and_straighten_and_forwards_them() {
        let keys: Vec<&str> = info(ToolId::Crop).options.iter().map(|o| o.key).collect();
        assert_eq!(
            keys,
            [
                "ratio",
                "width",
                "height",
                "units",
                "resolution",
                "overlay",
                "straighten_line",
                "delete_cropped"
            ]
        );
        let spec = ToolOptions::spec_for_test(ToolId::Crop, "ratio").unwrap();
        let OptionKind::Choice { choices, default } = spec.kind else {
            panic!("the ratio is not a preset choice");
        };
        assert_eq!(choices[default], "Free");
        for preset in [
            "Original",
            "1:1",
            "4:3",
            "16:9",
            "3:2",
            "5:4",
            "W x H x Resolution",
        ] {
            assert!(choices.contains(&preset), "no {preset} preset");
        }

        let mut opts = ToolOptions::new();
        assert!(opts.set(ToolId::Crop, "ratio", OptionValue::Choice(4)));
        assert!(opts.set(ToolId::Crop, "width", OptionValue::Float(3.0)));
        assert!(opts.set(ToolId::Crop, "overlay", OptionValue::Choice(2)));
        assert!(opts.set(ToolId::Crop, "straighten_line", OptionValue::Bool(true)));
        let mut held = opts.held(ToolId::Crop);
        held.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            held,
            vec![
                ("overlay".to_string(), OptionValue::Choice(2)),
                ("ratio".to_string(), OptionValue::Choice(4)),
                ("straighten_line".to_string(), OptionValue::Bool(true)),
                ("width".to_string(), OptionValue::Float(3.0)),
            ]
        );
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
        assert_eq!(sel.mode, BooleanOp::Exclude);
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

#[cfg(test)]
mod held_tests {
    use super::*;
    use tools::{ToolGroup, ToolId};

    /// Card 061 (review round 4), amended by W1-B2: `held` is the
    /// forward-to-tool set — the touched keys for THAT tool, the paint blend
    /// mode included now that the source-over stroke tools answer it. Untouched options
    /// stay absent, and another tool's touches stay absent.
    #[test]
    fn held_returns_only_touched_keys_for_the_tool() {
        // A non-default strength (setting a value equal to the schema
        // default is a deliberate no-op: nothing is "held" to forward).
        let mut options = ToolOptions::default();
        assert!(options.set(ToolId::RefineBoundary, "strength", OptionValue::Float(0.7)));
        options.set(ToolId::Move, "auto_select", OptionValue::Bool(true));

        let held = options.held(ToolId::RefineBoundary);
        assert_eq!(
            held,
            vec![("strength".to_string(), OptionValue::Float(0.7))],
            "only the touched registry key forwards for the tool"
        );
        assert!(
            options.held(ToolId::Brush).is_empty(),
            "another tool's touches do not leak"
        );
    }

    /// W1-B2: a touched Mode combo FORWARDS. It used to be filtered out of
    /// the forward set as a "UI-supplied key no tool could answer", which is
    /// the whole reason the paint blend mode never reached a brush.
    #[test]
    fn a_touched_blend_mode_is_in_the_forward_set_under_the_tools_key() {
        let mut options = ToolOptions::default();
        let multiply = BlendMode::ALL
            .iter()
            .position(|m| *m == BlendMode::Multiply)
            .unwrap();
        assert!(options.set(ToolId::Brush, BLEND_MODE_KEY, OptionValue::Choice(multiply)));

        let held = options.held(ToolId::Brush);
        assert_eq!(
            held,
            vec![(
                tools::BLEND_MODE_KEY.to_string(),
                OptionValue::Choice(multiply)
            )],
            "the touched blend mode forwards under the key the tools crate answers"
        );
        // The key the options bar stores under IS the tools crate's key.
        assert_eq!(BLEND_MODE_KEY, tools::BLEND_MODE_KEY);
        // ...and the index it forwards names the mode the tools crate reads.
        assert_eq!(
            tools::blend_mode_from_choice(multiply),
            Some(BlendMode::Multiply)
        );
    }

    /// W1-B2 (rounds 2-3): the Mode combo is offered to exactly the tools
    /// whose `set_setting` answers it. A forwarded key a tool refuses is a
    /// status-bar error on every press, and a key a tool accepts but never
    /// composites through is a dead control, so the offer set and the
    /// answer set must be the same set: the four source-over stroke tools
    /// (Brush, Pencil, Clone Stamp, Pattern Stamp) and, since W9-L, the
    /// Gradient and the Paint Bucket get the combo and accept the key; the
    /// retouching strokes, Patch, Red Eye, Pattern Fill and the magic eraser
    /// refuse the key and get no combo.
    #[test]
    fn the_mode_combo_is_offered_to_exactly_the_tools_that_answer_it() {
        let multiply = BlendMode::ALL
            .iter()
            .position(|m| *m == BlendMode::Multiply)
            .unwrap();
        let mut mismatched = Vec::new();
        let mut offered = 0usize;
        for info in tools::registry::all() {
            let is_offered = ToolOptions::spec_for_test(info.id, BLEND_MODE_KEY).is_some();
            let answers = tools::registry::make(info.id)
                .set_setting(BLEND_MODE_KEY, tools::ToolSetting::Choice(multiply))
                .is_ok();
            // Outside the two groups the trait default accepts any Choice,
            // so the equivalence is pinned where the combo could be offered.
            let in_groups = matches!(info.group, ToolGroup::Paint | ToolGroup::Retouch);
            if in_groups && is_offered != answers {
                mismatched.push(format!(
                    "{:?}: offered={is_offered} answers={answers}",
                    info.id
                ));
            }
            if !in_groups && is_offered {
                mismatched.push(format!("{:?}: offered outside paint/retouch", info.id));
            }
            offered += usize::from(is_offered);
        }
        assert!(mismatched.is_empty(), "{mismatched:?}");
        assert_eq!(
            offered, 6,
            "the four source-over stroke tools, the gradient and the paint bucket get the combo: {offered}"
        );
        // The retouching tools that do not lay a source colour over the
        // layer are not offered it: neither the two that do not stamp dabs
        // nor the strokes whose dabs mix toward a target or erase.
        assert!(ToolOptions::spec_for_test(ToolId::Patch, BLEND_MODE_KEY).is_none());
        assert!(ToolOptions::spec_for_test(ToolId::RedEye, BLEND_MODE_KEY).is_none());
        assert!(ToolOptions::spec_for_test(ToolId::Sponge, BLEND_MODE_KEY).is_none());
        assert!(ToolOptions::spec_for_test(ToolId::Blur, BLEND_MODE_KEY).is_none());
        assert!(ToolOptions::spec_for_test(ToolId::Eraser, BLEND_MODE_KEY).is_none());
        assert!(ToolOptions::spec_for_test(ToolId::RefineBoundary, BLEND_MODE_KEY).is_none());
        assert_eq!(ToolOptions::new().blend_mode(ToolId::Patch), None);
        assert_eq!(ToolOptions::new().blend_mode(ToolId::Dodge), None);
        // ...and the source-over tools in the same groups are.
        assert!(ToolOptions::spec_for_test(ToolId::CloneStamp, BLEND_MODE_KEY).is_some());
        assert!(ToolOptions::spec_for_test(ToolId::PatternStamp, BLEND_MODE_KEY).is_some());
        assert!(ToolOptions::spec_for_test(ToolId::Pencil, BLEND_MODE_KEY).is_some());
        // A Mode set for a tool that is not offered it is not held, so the
        // press never forwards a key the tool would refuse.
        let mut options = ToolOptions::default();
        assert!(!options.set(
            ToolId::Sponge,
            BLEND_MODE_KEY,
            OptionValue::Choice(multiply)
        ));
        assert!(options.held(ToolId::Sponge).is_empty());
    }
}

/// W1-B2 (round 3): the Type options reach the TEXT ENGINE, not only the
/// `TextLayer` payload. The options bar holds a size and a font family for
/// the Type tool, the held set forwards to the tool through `set_setting`
/// exactly as the shell forwards it, a click creates the layer, and the layer
/// is shaped by the real engine: a 48px layer lays out taller than a 12px
/// one, and the three Font choices resolve to three families the engine knows.
#[cfg(test)]
mod type_reaches_the_engine_tests {
    use super::*;
    use editor_core::Command;
    use layer_model::LayerKind;
    use raster::PixelRect;
    use tools::tool::{PointerEvent, ToolContext};
    use tools::ToolSetting;

    /// The shell's boundary conversion (shell.rs), verbatim.
    fn to_setting(value: OptionValue) -> ToolSetting {
        match value {
            OptionValue::Float(v) => ToolSetting::Float(v),
            OptionValue::Int(v) => ToolSetting::Int(v),
            OptionValue::Bool(v) => ToolSetting::Bool(v),
            OptionValue::Choice(v) => ToolSetting::Choice(v),
            OptionValue::Color(v) => ToolSetting::Color(v),
        }
    }

    /// Options bar -> held set -> `set_setting` -> click -> the created
    /// text layer, carrying `text` as what the user then typed.
    fn text_layer_from_options(options: &ToolOptions, text: &str) -> layer_model::TextLayer {
        let mut tool = tools::registry::make(ToolId::Type);
        for (key, value) in options.held(ToolId::Type) {
            tool.set_setting(&key, to_setting(value))
                .unwrap_or_else(|e| panic!("Type/{key}: {e}"));
        }
        let mut tiles = tools::MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        let cmds = ctx.drain();
        let Some(Command::CreateLayer { layer }) = cmds.first() else {
            panic!("a Type click creates a layer: {cmds:?}");
        };
        let LayerKind::Text(layer) = &layer.kind else {
            panic!("a Type click creates a TEXT layer: {:?}", layer.kind);
        };
        let mut layer = layer.clone();
        layer.text = text.to_owned();
        layer
    }

    fn shaped_height(
        library: &mut text_engine::FontLibrary,
        layer: &layer_model::TextLayer,
    ) -> f32 {
        let shaped = text_engine::shape(library, &text_engine::TextRun::from(layer));
        shaped.bounds.height
    }

    #[test]
    fn a_held_size_of_48_lays_out_a_taller_layer_than_12_in_the_real_engine() {
        let mut big = ToolOptions::default();
        assert!(big.set(ToolId::Type, "size_px", OptionValue::Float(48.0)));
        let mut small = ToolOptions::default();
        assert!(small.set(ToolId::Type, "size_px", OptionValue::Float(12.0)));

        // The fontless engine still lays out one `line_height`-tall line per
        // paragraph, so the assertion holds on a runner with no fonts at all
        // and on a machine with system fonts alike.
        for mut library in [
            text_engine::FontLibrary::empty(),
            text_engine::FontLibrary::with_system_fonts(),
        ] {
            let tall = shaped_height(&mut library, &text_layer_from_options(&big, "Hg"));
            let short = shaped_height(&mut library, &text_layer_from_options(&small, "Hg"));
            assert!(
                tall > short * 3.0,
                "48px must lay out about four times as tall as 12px (fontless={}): {tall} vs {short}",
                library.is_empty()
            );
        }
    }

    #[test]
    fn the_three_font_choices_reach_three_families_the_engine_resolves() {
        let mut library = text_engine::FontLibrary::with_system_fonts();
        if library.is_empty() {
            // A runner with no fonts has nothing to resolve a generic to; the
            // engine's own `generic_family_names_b2` test pins the resolution
            // against embedded faces.
            return;
        }
        let mut families = Vec::new();
        for choice in 0..3 {
            let mut options = ToolOptions::default();
            // Choice 0 is the schema default and is deliberately not "held";
            // the tool's own default is that same family.
            options.set(ToolId::Type, "font_family", OptionValue::Choice(choice));
            let layer = text_layer_from_options(&options, "iiWW");
            // None of the three is a "missing family" to the engine.
            assert_eq!(
                library.substitute_for(&layer.font_family),
                None,
                "{:?} is a generic name the engine resolves, not substitutes",
                layer.font_family
            );
            let shaped = text_engine::shape(&mut library, &text_engine::TextRun::from(&layer));
            assert!(
                !shaped.glyphs.is_empty(),
                "{:?} shaped nothing",
                layer.font_family
            );
            let used: std::collections::BTreeSet<String> = shaped
                .glyphs
                .iter()
                .filter_map(|g| library.face(g.font).map(|f| f.family))
                .collect();
            families.push((layer.font_family.clone(), used, shaped));
        }
        // Monospace is the choice a user can SEE: every glyph advances the
        // same distance, where the sans `i` is narrower than its `W`.
        let mono = &families[2].2;
        let advances: Vec<i32> = mono
            .glyphs
            .iter()
            .map(|g| (g.advance * 100.0).round() as i32)
            .collect();
        assert!(
            advances.iter().all(|a| *a == advances[0]),
            "monospace choice: equal advances, got {advances:?} ({:?})",
            families[2].1
        );
        let sans = &families[0].2;
        assert!(
            sans.glyphs[0].advance < sans.glyphs[2].advance,
            "sans choice: i narrower than W ({:?})",
            families[0].1
        );
        // And serif is a different family from sans on any machine that has
        // a serif installed (the engine pins one when it can).
        assert_ne!(
            families[1].1, families[0].1,
            "serif choice resolves to a different family than sans"
        );
    }

    /// W3-B: one frame of the real options bar for `tool`, returning the
    /// drawn rect of every option control it marked.
    fn options_bar_rects(tool: ToolId) -> Vec<(&'static str, egui::Rect)> {
        let mut w = crate::Workspace::new();
        w.palette.activate(&crate::PaletteModel::build(), tool);
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let input = || egui::RawInput {
            // Wide enough that the bar's horizontal scroll shows every control.
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(6000.0, 400.0),
            )),
            ..Default::default()
        };
        // Two frames: the first lays out, the second draws at settled sizes.
        let _ = ctx.run(input(), |ctx| crate::view::tool_options(&mut w, ctx));
        let _ = ctx.run(input(), |ctx| crate::view::tool_options(&mut w, ctx));
        tools::registry::info(tool)
            .expect("in the registry")
            .options
            .iter()
            .filter_map(|spec| {
                ctx.read_response(crate::view::ids::tool_option(tool, spec.key))
                    .map(|r| (spec.key, r.rect))
            })
            .collect()
    }

    /// W4-D round 2: one frame of the real options bar for the Crop tool
    /// under the Ratio preset `ratio`, returning the keys it drew.
    fn crop_bar_keys(ratio: Option<usize>) -> Vec<&'static str> {
        let mut w = crate::Workspace::new();
        w.palette
            .activate(&crate::PaletteModel::build(), ToolId::Crop);
        if let Some(ratio) = ratio {
            assert!(w
                .options
                .set(ToolId::Crop, "ratio", OptionValue::Choice(ratio)));
        }
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(6000.0, 400.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input(), |ctx| crate::view::tool_options(&mut w, ctx));
        let _ = ctx.run(input(), |ctx| crate::view::tool_options(&mut w, ctx));
        tools::registry::info(ToolId::Crop)
            .expect("in the registry")
            .options
            .iter()
            .filter(|spec| {
                ctx.read_response(crate::view::ids::tool_option(ToolId::Crop, spec.key))
                    .is_some()
            })
            .map(|spec| spec.key)
            .collect()
    }

    /// W4-D round 2: W, H, Units and Resolution are drawn only under the
    /// W x H x Resolution preset, the one that reads them; under Free and
    /// 16:9 the bar is the preset, the overlay and the two switches.
    #[test]
    fn the_crop_size_fields_show_only_under_the_w_x_h_x_resolution_preset() {
        let without = ["ratio", "overlay", "straighten_line", "delete_cropped"];
        assert_eq!(crop_bar_keys(None), without, "Free (default)");
        assert_eq!(crop_bar_keys(Some(4)), without, "16:9");
        assert_eq!(
            crop_bar_keys(Some(tools::edit::CROP_RATIO_SIZE)),
            [
                "ratio",
                "width",
                "height",
                "units",
                "resolution",
                "overlay",
                "straighten_line",
                "delete_cropped"
            ],
            "W x H x Resolution"
        );
    }

    /// W3-B: the Pen's options bar is no longer empty, and every shape tool
    /// draws fill, stroke and stroke-width controls — drawn by the real
    /// `view::tool_options`, not read out of the schema.
    #[test]
    fn the_options_bar_draws_the_pen_and_shape_paint_controls() {
        const PAINT: &[&str] = &[
            "fill",
            "fill_color",
            "stroke",
            "stroke_color",
            "stroke_width",
        ];
        let cases: &[(ToolId, &[&str])] = &[
            (ToolId::Pen, &["mode", "combine"]),
            (ToolId::Rectangle, &["mode", "from_center"]),
            (ToolId::RoundedRectangle, &["radius", "from_center"]),
            (ToolId::Ellipse, &["from_center"]),
            (ToolId::Polygon, &["sides"]),
            (ToolId::Star, &["points", "inner_ratio"]),
            (ToolId::Line, &["width"]),
            (ToolId::CustomShape, &["preset", "from_center"]),
        ];
        for (tool, own) in cases {
            let rects = options_bar_rects(*tool);
            for key in own.iter().chain(PAINT) {
                let rect = rects
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, r)| *r)
                    .unwrap_or_else(|| panic!("{tool:?}: no `{key}` control was drawn"));
                assert!(
                    rect.width() > 0.0 && rect.height() > 0.0 && rect.is_finite(),
                    "{tool:?}/{key}: drawn at {rect:?}"
                );
            }
        }
    }

    /// W3-B: the Custom Shape picker offers the built-in library, and a pick
    /// travels options bar -> held set -> `set_setting` -> drag -> a shape
    /// layer holding that library entry, not a rectangle.
    #[test]
    fn a_custom_shape_pick_reaches_the_drawn_layer() {
        let spec = ToolOptions::spec_for_test(ToolId::CustomShape, "preset").expect("declared");
        let OptionKind::Choice { choices, .. } = spec.kind else {
            panic!("preset is a Choice");
        };
        assert!(choices.len() >= 8, "{choices:?}");
        for want in ["Heart", "Star", "Arrow", "Speech Bubble", "Check", "Cross"] {
            assert!(choices.contains(&want), "{want} missing from {choices:?}");
        }

        let drawn_svg = |options: &ToolOptions| {
            let mut tool = tools::registry::make(ToolId::CustomShape);
            for (key, value) in options.held(ToolId::CustomShape) {
                tool.set_setting(&key, to_setting(value))
                    .unwrap_or_else(|e| panic!("CustomShape/{key}: {e}"));
            }
            let mut tiles = tools::MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
            tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
                .unwrap();
            tool.on_pointer_move(&mut ctx, PointerEvent::at(110.0, 110.0))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(110.0, 110.0))
                .unwrap();
            let cmds = ctx.drain();
            let Some(Command::CreateLayer { layer }) = cmds.first() else {
                panic!("no layer: {cmds:?}");
            };
            let LayerKind::Shape(shape) = &layer.kind else {
                panic!("{:?}", layer.kind);
            };
            (layer.name.clone(), shape.clone())
        };
        let star = choices.iter().position(|c| *c == "Star").unwrap();
        let mut options = ToolOptions::new();
        let (default_name, default_shape) = drawn_svg(&options);
        assert!(options.set(ToolId::CustomShape, "preset", OptionValue::Choice(star)));
        assert!(options.set(ToolId::CustomShape, "stroke", OptionValue::Choice(1)));
        assert!(options.set(ToolId::CustomShape, "stroke_width", OptionValue::Float(4.0)));
        let (name, shape) = drawn_svg(&options);
        assert_eq!(name, "Star");
        assert_ne!(name, default_name);
        assert_ne!(shape.path_svg, default_shape.path_svg);
        let path = vector::parse_svg(&shape.path_svg).unwrap();
        // A five-point star has ten corners; a rectangle has four.
        assert_eq!(vector::anchors::anchor_points(&path).len(), 10);
        let b = path.bounds();
        assert!(b.min.x >= 9.99 && b.max.x <= 110.01 && b.min.y >= 9.99 && b.max.y <= 110.01);
        assert_eq!(shape.stroke.expect("stroke on").width_px, 4.0);
    }

    /// W9-K: the Type tool's Font combo lists the installed families (not
    /// three generic names): a family the compositor gained from a font file
    /// is a row of the real options bar's open combo, holding its index keeps
    /// it (the static three-name table would clamp it to "monospace"), the
    /// combo then shows that family, and the next Type click makes a layer in
    /// it.
    #[test]
    fn the_type_font_combo_lists_the_installed_families_and_a_pick_reaches_the_layer() {
        let bytes = egui::FontDefinitions::default()
            .font_data
            .get("Ubuntu-Light")
            .expect("egui ships Ubuntu-Light")
            .font
            .to_vec();
        let mut probe = text_engine::FontLibrary::empty();
        probe.load_bytes(bytes.clone());
        let family = probe.family_names().first().cloned().expect("a family");
        assert!(compositor::load_font(bytes) > 0);

        let mut w = crate::Workspace::new();
        w.palette
            .activate(&crate::PaletteModel::build(), ToolId::Type);
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(6000.0, 2000.0));
        let frame = |w: &mut crate::Workspace, events: Vec<egui::Event>| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ctx| crate::view::tool_options(w, ctx),
            )
        };
        let _ = frame(&mut w, vec![]);
        let _ = frame(&mut w, vec![]);
        let key = crate::view::ids::tool_option(ToolId::Type, "font_family");
        let combo = ctx
            .read_response(key)
            .expect("the Font combo is drawn")
            .rect;
        let at = combo.center();
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = frame(&mut w, vec![egui::Event::PointerMoved(at), press(true)]);
        let _ = frame(&mut w, vec![press(false)]);
        let _ = frame(&mut w, vec![]);

        let choices = tools::registry::type_font_choices();
        let index = choices
            .iter()
            .position(|f| *f == family)
            .unwrap_or_else(|| panic!("{family} is a Font choice: {choices:?}"));
        assert!(index >= 3, "listed after the generics");
        assert!(
            ctx.read_response(crate::view::ids::tool_option_choice(
                ToolId::Type,
                "font_family",
                index
            ))
            .is_some(),
            "the open combo draws a row for {family}"
        );

        assert!(w
            .options
            .set(ToolId::Type, "font_family", OptionValue::Choice(index)));
        assert_eq!(
            w.options.get(ToolId::Type, "font_family"),
            Some(OptionValue::Choice(index)),
            "the held index is not clamped to the three generics"
        );
        let _ = frame(
            &mut w,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let out = frame(&mut w, vec![]);
        let combo = ctx.read_response(key).expect("drawn").rect;
        let shows = out.shapes.iter().any(|c| match &c.shape {
            egui::Shape::Text(t) => {
                t.galley.text() == family && combo.intersects(t.visual_bounding_rect())
            }
            _ => false,
        });
        assert!(shows, "the combo shows the picked family");
        assert_eq!(
            text_layer_from_options(&w.options, "Hi").font_family,
            family
        );
    }
}
