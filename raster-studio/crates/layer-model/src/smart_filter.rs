//! Smart filters: the non-destructive filter stack a smart-object layer
//! carries (Photopea's Filter ▸ Convert for Smart Filters).
//!
//! A smart object's stored tiles are its *source*. A filter applied to it
//! does not rewrite those tiles: it is appended here as a [`SmartFilter`] —
//! which filter, with which parameters, whether it is on, and how strongly and
//! in which blend mode it lands — and the compositor renders the source and
//! then runs the stack bottom-up (index 0 first). Turning a filter off,
//! editing its parameters or deleting it is therefore a change to this list
//! and nothing else, so the source is never lost.
//!
//! This crate only describes the stack. The filter *implementations* live in
//! the `filters` crate and the name-to-function table in the UI; a filter is
//! named here by a stable string key (see [`SmartFilter::filter`]) so the
//! model stays free of either dependency.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;

fn yes() -> bool {
    true
}

fn one() -> f32 {
    1.0
}

/// One parameter value of a smart filter, mirroring the kinds a filter
/// dialog edits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SmartParam {
    Float(f32),
    Int(i32),
    Bool(bool),
    /// Index into the parameter's list of choices.
    Choice(u32),
    /// Straight-alpha RGBA.
    Color([f32; 4]),
}

/// One entry of a smart object's filter stack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartFilter {
    /// The filter's stable key — the name of its `ui::menu::FilterId`
    /// variant (for example `"GaussianBlur"`). A key this build does not know
    /// is kept and saved untouched, and renders as a pass-through.
    pub filter: String,
    /// Every parameter the filter's dialog confirmed, keyed by the dialog's
    /// own parameter key.
    #[serde(default)]
    pub params: BTreeMap<String, SmartParam>,
    /// The eye in the Layers panel. An off filter is kept, and skipped.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// How much of the filtered result replaces what the stack produced
    /// below it. Expected in `0.0..=1.0`; read it through
    /// [`SmartFilter::effective_opacity`].
    #[serde(default = "one")]
    pub opacity: f32,
    /// How the filtered result is blended onto what the stack produced below
    /// it (the filter's Blending Options).
    #[serde(default)]
    pub blend_mode: BlendMode,
}

impl SmartFilter {
    /// A filter at full strength, Normal blend, switched on.
    pub fn new(filter: impl Into<String>, params: BTreeMap<String, SmartParam>) -> Self {
        Self {
            filter: filter.into(),
            params,
            enabled: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
        }
    }

    /// [`SmartFilter::opacity`] clamped to `0.0..=1.0` (non-finite reads as
    /// `0.0`), on the same terms as [`crate::Layer::effective_opacity`].
    pub fn effective_opacity(&self) -> f32 {
        crate::blend::unit(self.opacity)
    }

    /// `true` when the filter can change the composite: switched on and not
    /// at zero opacity.
    pub fn is_active(&self) -> bool {
        self.enabled && self.effective_opacity() > 0.0
    }
}

/// `true` when any filter in `stack` can change the composite.
pub fn stack_is_active(stack: &[SmartFilter]) -> bool {
    stack.iter().any(SmartFilter::is_active)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AssetId;
    use crate::layer::{Layer, LayerKind, SmartObjectLayer};

    fn blur(radius: f32) -> SmartFilter {
        let mut params = BTreeMap::new();
        params.insert("radius".to_string(), SmartParam::Float(radius));
        params.insert("edge".to_string(), SmartParam::Choice(0));
        SmartFilter::new("GaussianBlur", params)
    }

    #[test]
    fn a_new_filter_is_on_at_full_strength() {
        let f = blur(4.0);
        assert!(f.enabled);
        assert_eq!(f.opacity, 1.0);
        assert_eq!(f.blend_mode, BlendMode::Normal);
        assert!(f.is_active());
    }

    #[test]
    fn an_off_or_transparent_filter_is_inactive_and_the_stack_says_so() {
        let mut off = blur(4.0);
        off.enabled = false;
        let mut clear = blur(4.0);
        clear.opacity = 0.0;
        let mut nan = blur(4.0);
        nan.opacity = f32::NAN;
        assert!(!off.is_active() && !clear.is_active() && !nan.is_active());
        assert!(!stack_is_active(&[off.clone(), clear, nan]));
        assert!(!stack_is_active(&[]));
        assert!(stack_is_active(&[off, blur(1.0)]));
    }

    #[test]
    fn a_smart_object_round_trips_its_stack_and_an_old_one_loads_with_none() {
        let mut so = SmartObjectLayer {
            asset: AssetId::new(),
            linked: false,
            filters: vec![blur(3.5)],
        };
        so.filters[0].blend_mode = BlendMode::Multiply;
        so.filters[0].opacity = 0.4;
        let layer = Layer::with_kind("S", LayerKind::SmartObject(so.clone()));
        let json = serde_json::to_string(&layer).unwrap();
        let back: Layer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, layer);

        // An empty stack is not written at all, so a document that never used
        // smart filters serializes exactly as it did before they existed.
        let bare = SmartObjectLayer {
            asset: so.asset,
            linked: true,
            filters: Vec::new(),
        };
        let json = serde_json::to_string(&bare).unwrap();
        assert!(!json.contains("filters"), "{json}");
        let old = format!(r#"{{"asset":"{}","linked":true}}"#, so.asset.0);
        let back: SmartObjectLayer = serde_json::from_str(&old).unwrap();
        assert_eq!(back, bare);
    }

    #[test]
    fn missing_fields_of_a_filter_default_to_on_full_normal() {
        let f: SmartFilter = serde_json::from_str(r#"{"filter":"Median"}"#).unwrap();
        assert_eq!(f, SmartFilter::new("Median", BTreeMap::new()));
    }
}
