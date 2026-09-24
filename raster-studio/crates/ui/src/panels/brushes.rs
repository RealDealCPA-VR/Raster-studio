//! The Brushes panel: named presets over the same [`BrushSettings`] the tool
//! options bar edits.
//!
//! A preset is not a separate kind of thing from "the brush you have right
//! now" — it is a saved copy of it. Applying one writes every field into
//! [`crate::tool_options::ToolOptions`] through the registry, so a preset can
//! never set a value the schema would refuse, and a tool whose schema lacks a
//! field simply ignores that part of the preset.

use design::{contrast_ratio_over, Srgba, TextSize};
use tools::brush::{BrushDynamics, BrushTip};
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
    /// W4-I: user edits (capture, remove) this session; `0` lets a preset
    /// list restored from the preferences file replace the defaults.
    edits: u64,
    /// W9-E: how many of the brush library's entries
    /// ([`tools::brush::library_since`]) this panel has taken in.
    library_seen: usize,
    /// W9-E: the tip and dynamics of the preset last applied, waiting for the
    /// chrome to lay them over the tool's brush
    /// ([`Self::take_extras_over`]) — they have no options-bar keys.
    applied_extras: Option<(ToolId, BrushTip, BrushDynamics)>,
    /// W9-E: the preset last applied carried a non-round tip or dynamics, so
    /// the next apply must reach the brush even if no option key changes.
    extras_live: bool,
}

impl Default for BrushesState {
    fn default() -> Self {
        Self {
            presets: default_presets(),
            active: None,
            edits: 0,
            // Only what is published from now on: brushes made before this
            // panel existed belong to whatever panel was there then.
            library_seen: tools::brush::library_len(),
            applied_extras: None,
            extras_live: false,
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

    /// W4-I: user edits this session; see the field.
    pub fn edits(&self) -> u64 {
        self.edits
    }

    /// W4-I: replace the presets with a list read back from the preferences
    /// file. Unnamed entries are dropped, as [`Self::capture`] refuses them.
    /// Not a user edit.
    pub fn restore(&mut self, presets: impl IntoIterator<Item = BrushPreset>) {
        self.presets = presets
            .into_iter()
            .filter(|p| !p.name.trim().is_empty())
            .collect();
        self.active = None;
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
        self.edits += 1;
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
        self.edits += 1;
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
        // W9-E: a sampled tip or dynamics travel beside the option writes;
        // when either is (or was) in play the size write is sent even if
        // unchanged, so the chrome re-reads the brush and lays them over it.
        let extras = s.tip != BrushTip::Round || s.dynamics != BrushDynamics::default();
        let force = extras || self.extras_live;
        self.extras_live = extras;
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
        let sent: Vec<(&'static str, OptionValue)> = writes
            .into_iter()
            .filter(|(key, value)| {
                let changed = options.set(tool, key, *value);
                changed || (force && *key == "size" && options.get(tool, key).is_some())
            })
            .collect();
        // Pending only when something is sent: a pending set nobody reads
        // would otherwise land on a later, unrelated options-bar edit.
        self.applied_extras = (!sent.is_empty()).then_some((tool, s.tip, s.dynamics));
        sent
    }

    /// W9-E: `base` with the tip and dynamics of the preset last applied to
    /// `tool` laid over it (once — the pending extras are consumed), or
    /// `base` unchanged when none is waiting.
    pub fn take_extras_over(&mut self, tool: ToolId, base: BrushSettings) -> BrushSettings {
        match self.applied_extras {
            Some((for_tool, tip, dynamics)) if for_tool == tool => {
                self.applied_extras = None;
                BrushSettings {
                    tip,
                    dynamics,
                    ..base
                }
            }
            _ => base,
        }
    }

    /// W9-E: take in the brushes made outside the panel since the last
    /// call — Edit > Define Brush Preset, a `.abr` from File > Open — as
    /// new presets at the end of the list. Returns how many arrived.
    pub fn absorb_library(&mut self) -> usize {
        let (fresh, seen) = tools::brush::library_since(self.library_seen);
        self.library_seen = seen;
        let n = fresh.len();
        for (name, settings) in fresh {
            if name.trim().is_empty() {
                continue;
            }
            self.presets.push(BrushPreset { name, settings });
            // A user edit like a capture: the preferences keep the list, and a
            // saved list is not restored over it.
            self.edits += 1;
        }
        n
    }

    /// Drop the selection highlight once the live brush no longer matches the
    /// preset it came from.
    pub fn sync(&mut self, options: &ToolOptions, tool: ToolId) {
        self.absorb_library();
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

/// How much of the brush colour a soft tip's rim shows in its preview, from
/// the design tokens: the least coverage at which `ink` over `tile` clears
/// the WCAG floor for a non-text graphic (3:1, the tokens'
/// [`TextSize::Large`] floor). The fall-off of a soft brush is the whole
/// point of the preset, so its edge has to read against the tile, not fade
/// into it — and a theme whose ink cannot clear the floor at all gets a solid
/// rim rather than a faint one.
pub fn soft_tip_rim_coverage(ink: Srgba, tile: Srgba) -> f32 {
    let floor = TextSize::Large.min_contrast_aa();
    let clears = |a: f32| {
        let byte = (a * f32::from(ink.a)).round().clamp(0.0, 255.0) as u8;
        contrast_ratio_over(ink.with_alpha(byte), tile) >= floor
    };
    if !clears(1.0) {
        return 1.0;
    }
    // Contrast climbs monotonically with coverage: bisect for the least.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if clears(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// The alpha each of `rings` nested tip rings is painted at, rim first, so
/// that the rings *stacked* over one another cover the tile from `rim` (see
/// [`soft_tip_rim_coverage`]) at the rim up to fully solid at the core, in
/// even steps.
///
/// Painting every ring at `1 / rings` (what the preview did) stacks to only
/// an eighth at the rim and two thirds at the centre: the soft presets were
/// nearly invisible.
pub fn tip_ring_alphas(rings: usize, rim: f32) -> Vec<f32> {
    if rings <= 1 {
        return vec![1.0; rings];
    }
    let rim = rim.clamp(0.0, 1.0);
    let coverage = |k: usize| rim + (1.0 - rim) * k as f32 / (rings - 1) as f32;
    (0..rings)
        .map(|k| {
            if k == 0 {
                return coverage(0);
            }
            let below = coverage(k - 1);
            ((coverage(k) - below) / (1.0 - below).max(f32::EPSILON)).clamp(0.0, 1.0)
        })
        .collect()
}

/// The coverage `alphas` stack to, painted one over another.
pub fn stacked_coverage(alphas: &[f32]) -> f32 {
    alphas.iter().fold(0.0, |c, a| c + (1.0 - c) * a)
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
    fn a_soft_tips_rings_stack_from_a_legible_rim_to_a_solid_core() {
        let rim = 0.3;
        let alphas = tip_ring_alphas(8, rim);
        assert_eq!(alphas.len(), 8);
        assert!(alphas[0] >= rim - 1e-6, "{alphas:?}");
        for k in 1..=8 {
            let expected = rim + (1.0 - rim) * (k - 1) as f32 / 7.0;
            assert!(
                (stacked_coverage(&alphas[..k]) - expected).abs() < 1e-4,
                "ring {k}: {alphas:?}"
            );
        }
        assert!((stacked_coverage(&alphas) - 1.0).abs() < 1e-4);
        assert_eq!(tip_ring_alphas(1, rim), vec![1.0]);
    }

    /// The rim coverage comes from the theme's own colours: the least that
    /// clears the 3:1 non-text floor, in both themes, and solid when the ink
    /// cannot clear it at all.
    #[test]
    fn the_soft_rim_coverage_is_the_least_that_clears_the_token_floor() {
        for theme in [design::Theme::Dark, design::Theme::Light] {
            let t = theme.tokens();
            let ink = t.palette.text(design::TextRole::Primary);
            let tile = t.palette.color(design::ColorRole::SurfacePanel);
            let rim = soft_tip_rim_coverage(ink, tile);
            let at = |a: f32| contrast_ratio_over(ink.with_alpha((a * 255.0).round() as u8), tile);
            assert!(at(rim) >= 3.0, "{theme:?}: rim {rim} reads {}", at(rim));
            assert!(
                at(rim - 0.02) < 3.0,
                "{theme:?}: rim {rim} is not the least"
            );
            assert!(
                rim > 1.0 / 8.0,
                "{theme:?}: rim {rim} is the old faint eighth"
            );
        }
        let grey = Srgba::hex(0x808080);
        assert_eq!(soft_tip_rim_coverage(grey, grey), 1.0);
    }

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

    #[test]
    fn a_published_brush_is_listed_and_applying_it_carries_its_tip_and_dynamics() {
        let mut s = BrushesState::new();
        let before = s.len();
        let id = tools::brush::TipId([0xA1; 32]);
        let sampled = BrushSettings {
            size: 37.0,
            tip: BrushTip::Sampled(id),
            dynamics: BrushDynamics {
                scatter: 1.5,
                ..Default::default()
            },
            ..BrushSettings::default()
        };
        tools::brush::publish_library_brush("W9E Panel Probe", sampled);
        let mut options = ToolOptions::new();
        // The panel's per-frame sync takes it in.
        s.sync(&options, ToolId::Brush);
        let index = s
            .presets()
            .iter()
            .position(|p| p.name == "W9E Panel Probe")
            .expect("the published brush is listed");
        assert!(s.len() > before);
        assert!(s.edits() > 0, "a listed brush is kept by the preferences");
        assert_eq!(s.absorb_library(), 0, "taken in once");

        let writes = s.apply(index, &mut options, ToolId::Brush);
        assert!(writes.iter().any(|(k, _)| *k == "size"));
        // The chrome lays the extras over the tool's brush, once.
        let base = BrushSettings::default();
        let applied = s.take_extras_over(ToolId::Brush, base);
        assert_eq!(applied.tip, BrushTip::Sampled(id));
        assert_eq!(applied.dynamics.scatter, 1.5);
        assert_eq!(s.take_extras_over(ToolId::Brush, base), base);
        // Re-applying the same preset still reaches the brush (the size write
        // is sent even though the value did not change).
        let again = s.apply(index, &mut options, ToolId::Brush);
        assert_eq!(again.len(), 1, "{again:?}");
        assert_eq!(
            s.take_extras_over(ToolId::Brush, base).tip,
            BrushTip::Sampled(id)
        );
        // And applying a plain round preset afterwards brings the tip back
        // to round rather than leaving the sampled one in force.
        let round = s
            .presets()
            .iter()
            .position(|p| p.name == "Hard Round 24")
            .unwrap();
        s.apply(round, &mut options, ToolId::Brush);
        let back = s.take_extras_over(ToolId::Brush, applied);
        assert_eq!(back.tip, BrushTip::Round);
        assert_eq!(back.dynamics, BrushDynamics::default());
    }
}
