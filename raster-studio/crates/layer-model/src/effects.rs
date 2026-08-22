//! Layer effects ("layer styles"): parametric state only.
//!
//! Nothing here renders. The types describe what the effect *is* so that the
//! document model, undo/redo, the properties panel and the file format all
//! agree on a single representation; the compositor grows the matching passes
//! later.
//!
//! # Serde stability
//!
//! Every struct in this module carries `#[serde(default)]` **on the container**,
//! which supplies a `Default` for any field the payload omits; every `Option`
//! effect slot and the `enabled` flag are skipped on write when they hold their
//! default. Consequences the file format depends on:
//!
//! * A document written before an effect parameter existed still loads — the
//!   missing field takes its `Default`.
//! * Adding a parameter is backward compatible. Renaming one is not.
//! * `LayerEffects::default()` serializes to `{}` (asserted by
//!   `default_effects_are_empty_and_cost_nothing_on_disk`), and
//!   [`crate::Layer`] skips the whole `effects` key when it is default, so
//!   layers with no styles cost nothing on disk.

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::ids::AssetId;

/// Straight-alpha RGBA in document color space.
///
/// Each component is *expected* in `0.0..=1.0`. Nothing in this crate enforces
/// that — see the "Numeric ranges" section of the [crate docs](crate) — so a
/// renderer must clamp (or deliberately allow out-of-gamut values) itself.
pub type Rgba = [f32; 4];

const OPAQUE_BLACK: Rgba = [0.0, 0.0, 0.0, 1.0];
const OPAQUE_WHITE: Rgba = [1.0, 1.0, 1.0, 1.0];

/// Photoshop's default global-light direction, in degrees counter-clockwise
/// from +x. Effects that opt into global light start here.
pub const DEFAULT_GLOBAL_LIGHT_DEG: f32 = 120.0;

/// The full set of layer styles attachable to one layer.
///
/// `enabled == false` switches every effect off at once without discarding the
/// parameters, matching the master "Effects" toggle in the layers panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerEffects {
    /// Skipped on write while it holds its default (`true`) so an untouched
    /// effect block serializes to `{}` rather than `{"enabled":true}`.
    #[serde(skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drop_shadow: Option<ShadowEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_shadow: Option<ShadowEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outer_glow: Option<GlowEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_glow: Option<GlowEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bevel_emboss: Option<BevelEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub satin: Option<SatinEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_overlay: Option<ColorOverlayEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gradient_overlay: Option<GradientOverlayEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_overlay: Option<PatternOverlayEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stroke: Option<StrokeEffect>,
}

impl Default for LayerEffects {
    fn default() -> Self {
        Self {
            enabled: true,
            drop_shadow: None,
            inner_shadow: None,
            outer_glow: None,
            inner_glow: None,
            bevel_emboss: None,
            satin: None,
            color_overlay: None,
            gradient_overlay: None,
            pattern_overlay: None,
            stroke: None,
        }
    }
}

/// `skip_serializing_if` predicate for a `bool` whose default is `true`.
fn is_true(b: &bool) -> bool {
    *b
}

impl LayerEffects {
    /// `true` when this block is exactly [`LayerEffects::default()`].
    ///
    /// Used by [`crate::Layer`]'s `skip_serializing_if` so a layer that was
    /// never styled carries no `effects` key at all.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// `true` when no effect slot is filled — the compositor can skip the whole
    /// style pipeline for this layer regardless of `enabled`.
    pub fn is_empty(&self) -> bool {
        self.drop_shadow.is_none()
            && self.inner_shadow.is_none()
            && self.outer_glow.is_none()
            && self.inner_glow.is_none()
            && self.bevel_emboss.is_none()
            && self.satin.is_none()
            && self.color_overlay.is_none()
            && self.gradient_overlay.is_none()
            && self.pattern_overlay.is_none()
            && self.stroke.is_none()
    }

    /// `true` when at least one effect will actually be drawn.
    pub fn affects_composite(&self) -> bool {
        self.enabled && !self.is_empty()
    }

    /// Number of filled effect slots. Used by the layers panel badge and by
    /// render-cost estimation.
    pub fn count(&self) -> usize {
        [
            self.drop_shadow.is_some(),
            self.inner_shadow.is_some(),
            self.outer_glow.is_some(),
            self.inner_glow.is_some(),
            self.bevel_emboss.is_some(),
            self.satin.is_some(),
            self.color_overlay.is_some(),
            self.gradient_overlay.is_some(),
            self.pattern_overlay.is_some(),
            self.stroke.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count()
    }
}

/// Drop shadow and inner shadow share a parameter set; only the direction the
/// offset silhouette is composited differs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShadowEffect {
    pub blend_mode: BlendMode,
    pub color: Rgba,
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub opacity: f32,
    /// Light direction in degrees, counter-clockwise from +x.
    pub angle_deg: f32,
    /// When `true`, `angle_deg` is overridden by the document's global light.
    pub use_global_light: bool,
    /// Offset distance along `angle_deg`, in document pixels.
    pub distance_px: f32,
    /// Fraction of `size_px` spent growing the silhouette before blurring
    /// (Photoshop's "Spread" for drop shadows, "Choke" for inner shadows).
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub spread: f32,
    /// Blur radius in document pixels. Expected `>= 0.0`; the compositor
    /// clamps.
    pub size_px: f32,
    /// Monochromatic noise added to the shadow. Expected in `0.0..=1.0`; the
    /// compositor clamps.
    pub noise: f32,
    /// Drop shadow only: when `true` the shadow is not drawn under the layer's
    /// own opaque pixels.
    pub knockout: bool,
}

impl Default for ShadowEffect {
    fn default() -> Self {
        Self {
            blend_mode: BlendMode::Multiply,
            color: OPAQUE_BLACK,
            opacity: 0.75,
            angle_deg: DEFAULT_GLOBAL_LIGHT_DEG,
            use_global_light: true,
            distance_px: 5.0,
            spread: 0.0,
            size_px: 5.0,
            noise: 0.0,
            knockout: true,
        }
    }
}

/// How a glow's blur is shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum GlowTechnique {
    /// Gaussian blur — soft, no hard corners.
    #[default]
    Softer,
    /// Distance-transform based — preserves sharp detail.
    Precise,
}

/// Where an inner glow originates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum GlowSource {
    /// Glow grows inward from the layer's edge.
    #[default]
    Edge,
    /// Glow radiates outward from the layer's center.
    Center,
}

/// What fills a glow, overlay or stroke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FillStyle {
    Solid(Rgba),
    Gradient(Gradient),
    Pattern(PatternFill),
}

impl Default for FillStyle {
    fn default() -> Self {
        FillStyle::Solid(OPAQUE_WHITE)
    }
}

/// Outer and inner glow. `source` is ignored by outer glow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlowEffect {
    pub blend_mode: BlendMode,
    pub fill: FillStyle,
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub opacity: f32,
    /// Monochromatic noise. Expected in `0.0..=1.0`; the compositor clamps.
    pub noise: f32,
    pub technique: GlowTechnique,
    /// Fraction of `size_px` spent choking the silhouette. Expected in
    /// `0.0..=1.0`; the compositor clamps.
    pub spread: f32,
    /// Blur radius in document pixels. Expected `>= 0.0`; the compositor
    /// clamps.
    pub size_px: f32,
    /// Inner glow only.
    pub source: GlowSource,
    /// Portion of the glow's falloff targeted by the contour. Expected in
    /// `0.0..=1.0`; the compositor clamps.
    pub range: f32,
    /// Randomizes gradient glow colors. Expected in `0.0..=1.0`; the
    /// compositor clamps.
    pub jitter: f32,
}

impl Default for GlowEffect {
    fn default() -> Self {
        Self {
            blend_mode: BlendMode::Screen,
            fill: FillStyle::Solid([1.0, 0.95, 0.7, 1.0]),
            opacity: 0.75,
            noise: 0.0,
            technique: GlowTechnique::Softer,
            spread: 0.0,
            size_px: 5.0,
            source: GlowSource::Edge,
            range: 0.5,
            jitter: 0.0,
        }
    }
}

/// Bevel placement relative to the layer edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum BevelStyle {
    #[default]
    InnerBevel,
    OuterBevel,
    Emboss,
    PillowEmboss,
    StrokeEmboss,
}

/// Edge treatment for bevel and emboss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum BevelTechnique {
    #[default]
    SmoothBevel,
    ChiselHard,
    ChiselSoft,
}

/// Whether the bevel reads as raised or carved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum BevelDirection {
    #[default]
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BevelEffect {
    pub style: BevelStyle,
    pub technique: BevelTechnique,
    pub direction: BevelDirection,
    /// Height scale of the generated normal (1.0 = 100%). Expected in
    /// `0.0..=10.0`; the compositor clamps.
    pub depth: f32,
    /// Bevel width in document pixels. Expected `>= 0.0`; the compositor
    /// clamps.
    pub size_px: f32,
    /// Blur applied to the bevel shading, in document pixels. Expected
    /// `>= 0.0`; the compositor clamps.
    pub soften_px: f32,
    /// Light direction in degrees, counter-clockwise from +x.
    pub angle_deg: f32,
    /// Light elevation in degrees above the layer plane. Expected in `0..=90`;
    /// the compositor clamps.
    pub altitude_deg: f32,
    pub use_global_light: bool,
    pub highlight_mode: BlendMode,
    pub highlight_color: Rgba,
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub highlight_opacity: f32,
    pub shadow_mode: BlendMode,
    pub shadow_color: Rgba,
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub shadow_opacity: f32,
}

impl Default for BevelEffect {
    fn default() -> Self {
        Self {
            style: BevelStyle::InnerBevel,
            technique: BevelTechnique::SmoothBevel,
            direction: BevelDirection::Up,
            depth: 1.0,
            size_px: 5.0,
            soften_px: 0.0,
            angle_deg: DEFAULT_GLOBAL_LIGHT_DEG,
            altitude_deg: 30.0,
            use_global_light: true,
            highlight_mode: BlendMode::Screen,
            highlight_color: OPAQUE_WHITE,
            highlight_opacity: 0.75,
            shadow_mode: BlendMode::Multiply,
            shadow_color: OPAQUE_BLACK,
            shadow_opacity: 0.75,
        }
    }
}

/// Satin: the layer silhouette offset twice and differenced, producing a
/// draped-cloth interior shading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SatinEffect {
    pub blend_mode: BlendMode,
    pub color: Rgba,
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub opacity: f32,
    pub angle_deg: f32,
    pub distance_px: f32,
    /// Blur radius in document pixels. Expected `>= 0.0`; the compositor
    /// clamps.
    pub size_px: f32,
    pub invert: bool,
}

impl Default for SatinEffect {
    fn default() -> Self {
        Self {
            blend_mode: BlendMode::Multiply,
            color: OPAQUE_BLACK,
            opacity: 0.5,
            angle_deg: 19.0,
            distance_px: 11.0,
            size_px: 14.0,
            invert: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorOverlayEffect {
    pub blend_mode: BlendMode,
    pub color: Rgba,
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub opacity: f32,
}

impl Default for ColorOverlayEffect {
    fn default() -> Self {
        Self {
            blend_mode: BlendMode::Normal,
            color: [1.0, 0.0, 0.0, 1.0],
            opacity: 1.0,
        }
    }
}

/// One color stop on a [`Gradient`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradientStop {
    /// Position along the gradient. Expected in `0.0..=1.0`; the renderer
    /// clamps.
    pub position: f32,
    pub color: Rgba,
    /// Midpoint bias toward the next stop (0.5 = linear). Expected in
    /// `0.0..=1.0`; the renderer clamps.
    pub midpoint: f32,
}

impl Default for GradientStop {
    fn default() -> Self {
        Self {
            position: 0.0,
            color: OPAQUE_BLACK,
            midpoint: 0.5,
        }
    }
}

/// A gradient ramp. Stops are expected to be sorted by `position`; a renderer
/// may sort defensively but the editor keeps them ordered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gradient {
    pub stops: Vec<GradientStop>,
    /// Independent alpha ramp; when empty the stops' own alpha is used.
    pub alpha_stops: Vec<GradientStop>,
    /// Adds noise-dithered banding suppression. Expected in `0.0..=1.0`; the
    /// renderer clamps.
    pub smoothness: f32,
}

impl Default for Gradient {
    fn default() -> Self {
        Self {
            stops: vec![
                GradientStop {
                    position: 0.0,
                    color: OPAQUE_BLACK,
                    midpoint: 0.5,
                },
                GradientStop {
                    position: 1.0,
                    color: OPAQUE_WHITE,
                    midpoint: 0.5,
                },
            ],
            alpha_stops: Vec::new(),
            smoothness: 1.0,
        }
    }
}

/// Geometry of a gradient fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum GradientStyle {
    #[default]
    Linear,
    Radial,
    Angle,
    Reflected,
    Diamond,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradientOverlayEffect {
    pub blend_mode: BlendMode,
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub opacity: f32,
    pub gradient: Gradient,
    pub style: GradientStyle,
    pub reverse: bool,
    /// Ramp is fit to the layer bounds when `true`, to the document otherwise.
    pub align_with_layer: bool,
    pub angle_deg: f32,
    /// Ramp length as a fraction of the fitted extent; 1.0 = 100%. Expected
    /// `> 0.0`; the renderer clamps.
    pub scale: f32,
    /// Ramp origin offset from the fitted center, in document pixels.
    pub offset_px: [f32; 2],
    /// Dither the ramp to suppress banding.
    pub dither: bool,
}

impl Default for GradientOverlayEffect {
    fn default() -> Self {
        Self {
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            gradient: Gradient::default(),
            style: GradientStyle::Linear,
            reverse: false,
            align_with_layer: true,
            angle_deg: 90.0,
            scale: 1.0,
            offset_px: [0.0, 0.0],
            dither: false,
        }
    }
}

/// A tiled pattern reference plus its placement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatternFill {
    /// Asset holding the pattern tile. `None` means "unset"; the compositor
    /// must skip the effect rather than guess.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetId>,
    /// Tile scale; 1.0 = the asset's native size. Expected `> 0.0`; the
    /// renderer clamps.
    pub scale: f32,
    /// Tile origin offset in document pixels.
    pub offset_px: [f32; 2],
    pub angle_deg: f32,
    /// When `true` the pattern origin follows the layer as it moves.
    pub link_with_layer: bool,
}

impl Default for PatternFill {
    fn default() -> Self {
        Self {
            asset: None,
            scale: 1.0,
            offset_px: [0.0, 0.0],
            angle_deg: 0.0,
            link_with_layer: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatternOverlayEffect {
    pub blend_mode: BlendMode,
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub opacity: f32,
    pub pattern: PatternFill,
}

impl Default for PatternOverlayEffect {
    fn default() -> Self {
        Self {
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            pattern: PatternFill::default(),
        }
    }
}

/// Where a stroke sits relative to the layer's alpha edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum StrokePosition {
    #[default]
    Outside,
    Inside,
    Center,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StrokeEffect {
    /// Stroke width in document pixels. Expected `>= 0.0`; the compositor
    /// clamps.
    pub size_px: f32,
    pub position: StrokePosition,
    pub blend_mode: BlendMode,
    /// Expected in `0.0..=1.0`; the compositor clamps.
    pub opacity: f32,
    pub fill: FillStyle,
    /// Stroke is drawn but the layer's own pixels are knocked out of it.
    pub overprint: bool,
}

impl Default for StrokeEffect {
    fn default() -> Self {
        Self {
            size_px: 3.0,
            position: StrokePosition::Outside,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            fill: FillStyle::Solid(OPAQUE_BLACK),
            overprint: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_effects_are_empty_and_cost_nothing_on_disk() {
        let e = LayerEffects::default();
        assert!(e.is_empty());
        assert!(e.is_default());
        assert!(!e.affects_composite());
        assert_eq!(e.count(), 0);
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            "{}",
            "an untouched effect block must occupy no bytes beyond the braces"
        );
    }

    #[test]
    fn a_disabled_but_empty_block_still_records_the_toggle() {
        // `enabled` is only skipped at its default; switching it off is real
        // state and must survive the round trip.
        let e = LayerEffects {
            enabled: false,
            ..Default::default()
        };
        assert!(!e.is_default());
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"enabled":false}"#);
        assert_eq!(serde_json::from_str::<LayerEffects>(&json).unwrap(), e);
    }

    #[test]
    fn master_toggle_suppresses_without_discarding() {
        let mut e = LayerEffects {
            drop_shadow: Some(ShadowEffect::default()),
            ..Default::default()
        };
        assert!(e.affects_composite());
        e.enabled = false;
        assert!(!e.affects_composite());
        assert!(!e.is_empty(), "parameters must survive the toggle");
        assert_eq!(e.count(), 1);
    }

    #[test]
    fn all_ten_slots_are_countable() {
        let e = LayerEffects {
            enabled: true,
            drop_shadow: Some(ShadowEffect::default()),
            inner_shadow: Some(ShadowEffect::default()),
            outer_glow: Some(GlowEffect::default()),
            inner_glow: Some(GlowEffect::default()),
            bevel_emboss: Some(BevelEffect::default()),
            satin: Some(SatinEffect::default()),
            color_overlay: Some(ColorOverlayEffect::default()),
            gradient_overlay: Some(GradientOverlayEffect::default()),
            pattern_overlay: Some(PatternOverlayEffect::default()),
            stroke: Some(StrokeEffect::default()),
        };
        assert_eq!(e.count(), 10);
        assert!(!e.is_empty());
    }

    #[test]
    fn full_effect_stack_serde_roundtrips_exactly() {
        let e = LayerEffects {
            enabled: true,
            drop_shadow: Some(ShadowEffect {
                angle_deg: 45.0,
                size_px: 12.5,
                ..Default::default()
            }),
            inner_shadow: Some(ShadowEffect::default()),
            outer_glow: Some(GlowEffect {
                fill: FillStyle::Gradient(Gradient::default()),
                technique: GlowTechnique::Precise,
                ..Default::default()
            }),
            inner_glow: Some(GlowEffect {
                source: GlowSource::Center,
                ..Default::default()
            }),
            bevel_emboss: Some(BevelEffect {
                style: BevelStyle::PillowEmboss,
                technique: BevelTechnique::ChiselHard,
                direction: BevelDirection::Down,
                ..Default::default()
            }),
            satin: Some(SatinEffect::default()),
            color_overlay: Some(ColorOverlayEffect::default()),
            gradient_overlay: Some(GradientOverlayEffect {
                style: GradientStyle::Diamond,
                ..Default::default()
            }),
            pattern_overlay: Some(PatternOverlayEffect {
                pattern: PatternFill {
                    asset: Some(AssetId::new()),
                    scale: 2.0,
                    ..Default::default()
                },
                ..Default::default()
            }),
            stroke: Some(StrokeEffect {
                position: StrokePosition::Inside,
                fill: FillStyle::Pattern(PatternFill::default()),
                ..Default::default()
            }),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: LayerEffects = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // Simulates a document written before these parameters existed.
        let shadow: ShadowEffect = serde_json::from_str(r#"{"distance_px":9.0}"#).unwrap();
        assert_eq!(shadow.distance_px, 9.0);
        assert_eq!(shadow.blend_mode, BlendMode::Multiply);
        assert_eq!(shadow.opacity, 0.75);

        let effects: LayerEffects = serde_json::from_str("{}").unwrap();
        assert_eq!(effects, LayerEffects::default());

        // An unknown-to-old-code effect body still loads with defaults filled in.
        let stroke: StrokeEffect = serde_json::from_str(r#"{"position":"Center"}"#).unwrap();
        assert_eq!(stroke.position, StrokePosition::Center);
        assert_eq!(stroke.size_px, 3.0);
    }

    #[test]
    fn global_light_default_is_shared_by_shadow_and_bevel() {
        assert_eq!(ShadowEffect::default().angle_deg, DEFAULT_GLOBAL_LIGHT_DEG);
        assert_eq!(BevelEffect::default().angle_deg, DEFAULT_GLOBAL_LIGHT_DEG);
        assert!(ShadowEffect::default().use_global_light);
        assert!(BevelEffect::default().use_global_light);
    }
}
