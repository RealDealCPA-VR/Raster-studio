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
    /// W9-H: contours, Blend If and the extra instances of the effects
    /// Photoshop lets a style repeat. Appended with its own default so a
    /// document written before it loads unchanged, and skipped on write while
    /// it is default so an old style still serializes byte-for-byte as before.
    #[serde(default, skip_serializing_if = "StyleExtras::is_default")]
    pub extras: StyleExtras,
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
            extras: StyleExtras::default(),
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
            && self.extras.instance_count() == 0
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
            + self.extras.instance_count()
    }

    /// W9-H: every drop shadow, bottom-most first: the primary slot, then
    /// the extra instances, each with the contour it is drawn through.
    pub fn drop_shadows(&self) -> Vec<(&ShadowEffect, &Contour)> {
        self.drop_shadow
            .iter()
            .map(|e| (e, &self.extras.contours.drop_shadow))
            .chain(
                self.extras
                    .drop_shadows
                    .iter()
                    .map(|i| (&i.effect, &i.contour)),
            )
            .collect()
    }

    /// W9-H: every inner shadow, bottom-most first.
    pub fn inner_shadows(&self) -> Vec<(&ShadowEffect, &Contour)> {
        self.inner_shadow
            .iter()
            .map(|e| (e, &self.extras.contours.inner_shadow))
            .chain(
                self.extras
                    .inner_shadows
                    .iter()
                    .map(|i| (&i.effect, &i.contour)),
            )
            .collect()
    }

    /// W9-H: every stroke, bottom-most first.
    pub fn strokes(&self) -> Vec<&StrokeEffect> {
        self.stroke.iter().chain(&self.extras.strokes).collect()
    }

    /// W9-H: every colour overlay, bottom-most first.
    pub fn color_overlays(&self) -> Vec<&ColorOverlayEffect> {
        self.color_overlay
            .iter()
            .chain(&self.extras.color_overlays)
            .collect()
    }

    /// W9-H: every gradient overlay, bottom-most first.
    pub fn gradient_overlays(&self) -> Vec<&GradientOverlayEffect> {
        self.gradient_overlay
            .iter()
            .chain(&self.extras.gradient_overlays)
            .collect()
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
///
/// W7-B: the pixels travel **with the fill** in [`PatternFill::tile`]. A
/// document therefore saves, reopens, undoes, journals and copies a style
/// with the pattern it draws, and the compositor needs no asset table to
/// resolve it. `asset` is kept for a reference that names pixels this model
/// does not carry; on its own it draws nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatternFill {
    /// Asset holding the pattern tile. `None` means "unset"; the compositor
    /// must skip the effect rather than guess.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetId>,
    /// W7-B: the pattern's own pixels. `None` draws nothing. A stored tile
    /// that fails [`PatternTile::new`]'s checks loads as `None` rather than
    /// refusing the whole document.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_tile"
    )]
    pub tile: Option<PatternTile>,
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
            tile: None,
            scale: 1.0,
            offset_px: [0.0, 0.0],
            angle_deg: 0.0,
            link_with_layer: true,
        }
    }
}

impl PatternFill {
    /// W7-B: `true` when the fill carries pixels to draw.
    pub fn is_drawable(&self) -> bool {
        self.tile.is_some()
    }
}

/// W7-B: the largest edge a [`PatternTile`] may have, in pixels.
pub const MAX_PATTERN_EDGE: u32 = 16_384;
/// W7-B: the most pixels a [`PatternTile`] may hold (256 MiB of RGBA8).
pub const MAX_PATTERN_PIXELS: u64 = 1 << 26;

/// Why a [`PatternTile`] could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternTileError {
    #[error("a pattern needs at least one pixel")]
    Empty,
    #[error("a {width}x{height} pattern is over the size limit")]
    TooLarge { width: u32, height: u32 },
    #[error("a {width}x{height} pattern needs {expected} bytes, got {found}")]
    WrongLength {
        width: u32,
        height: u32,
        expected: u64,
        found: u64,
    },
}

/// W7-B: the pixels a pattern fill tiles: straight-alpha RGBA8 in the
/// document's colour space, row-major, `width * height * 4` bytes.
///
/// Immutable once built, and cheap to clone (the bytes are shared), because
/// every style edit, undo inverse and dialog copy clones the effect block.
/// [`PatternTile::content_hash`] is computed once, here, and is what a tile
/// cache keys on: two tiles with the same pixels share a key, and a change to
/// any pixel changes it.
#[derive(Clone)]
pub struct PatternTile {
    name: String,
    width: u32,
    height: u32,
    rgba8: std::sync::Arc<[u8]>,
    hash: u64,
}

impl PatternTile {
    /// Build a tile, checking the size and the byte count.
    pub fn new(
        name: impl Into<String>,
        width: u32,
        height: u32,
        rgba8: Vec<u8>,
    ) -> Result<Self, PatternTileError> {
        if width == 0 || height == 0 {
            return Err(PatternTileError::Empty);
        }
        let pixels = u64::from(width) * u64::from(height);
        if width > MAX_PATTERN_EDGE || height > MAX_PATTERN_EDGE || pixels > MAX_PATTERN_PIXELS {
            return Err(PatternTileError::TooLarge { width, height });
        }
        let expected = pixels * 4;
        if rgba8.len() as u64 != expected {
            return Err(PatternTileError::WrongLength {
                width,
                height,
                expected,
                found: rgba8.len() as u64,
            });
        }
        let hash = pattern_content_hash(width, height, &rgba8);
        Ok(Self {
            name: name.into(),
            width,
            height,
            rgba8: rgba8.into(),
            hash,
        })
    }

    /// The name the pattern was defined under, for display only.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The raw straight-alpha RGBA8 bytes.
    pub fn rgba8(&self) -> &[u8] {
        &self.rgba8
    }

    /// A hash of the dimensions and pixels (not the name), stable across
    /// runs. What a cache keys a pattern by.
    pub fn content_hash(&self) -> u64 {
        self.hash
    }

    /// The pixel at `(x, y)` of the infinite tiling.
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

/// FNV-style mixing over the dimensions, then eight bytes at a time over the
/// pixels (the tail folded in byte by byte), so a whole-canvas pattern hashes
/// in one pass.
fn pattern_content_hash(width: u32, height: u32, bytes: &[u8]) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(PRIME);
        h ^= h >> 29;
    };
    mix(u64::from(width));
    mix(u64::from(height));
    let (words, tail) = bytes.as_chunks::<8>();
    for w in words {
        mix(u64::from_le_bytes(*w));
    }
    for b in tail {
        mix(u64::from(*b));
    }
    mix(bytes.len() as u64);
    h
}

impl std::fmt::Debug for PatternTile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PatternTile")
            .field("name", &self.name)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("hash", &format_args!("{:016x}", self.hash))
            .finish()
    }
}

impl PartialEq for PatternTile {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
            && self.width == other.width
            && self.height == other.height
            && self.name == other.name
            && (std::sync::Arc::ptr_eq(&self.rgba8, &other.rgba8) || self.rgba8 == other.rgba8)
    }
}

/// Bytes written as one binary blob (MessagePack `bin`) rather than one
/// integer per byte.
struct TileBytesOut<'a>(&'a [u8]);

impl Serialize for TileBytesOut<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(self.0)
    }
}

/// Reads what [`TileBytesOut`] wrote: a blob, or (from a format with no blob
/// type, such as JSON) a sequence of integers.
struct TileBytesIn(Vec<u8>);

impl<'de> Deserialize<'de> for TileBytesIn {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = TileBytesIn;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("pattern bytes")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                Ok(TileBytesIn(v.to_vec()))
            }
            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
                Ok(TileBytesIn(v))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                // A size hint comes from the file, so it reserves at most a
                // megabyte; the loop refuses anything past a valid tile.
                let cap = seq.size_hint().unwrap_or(0).min(1 << 20);
                let mut out = Vec::with_capacity(cap);
                while let Some(b) = seq.next_element::<u8>()? {
                    if out.len() as u64 >= MAX_PATTERN_PIXELS * 4 {
                        return Err(serde::de::Error::custom("pattern bytes over the limit"));
                    }
                    out.push(b);
                }
                Ok(TileBytesIn(out))
            }
        }
        d.deserialize_bytes(V)
    }
}

impl Serialize for PatternTile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("PatternTile", 4)?;
        st.serialize_field("name", &self.name)?;
        st.serialize_field("width", &self.width)?;
        st.serialize_field("height", &self.height)?;
        st.serialize_field("rgba8", &TileBytesOut(&self.rgba8))?;
        st.end()
    }
}

#[derive(Deserialize)]
struct PatternTileRepr {
    #[serde(default)]
    name: String,
    width: u32,
    height: u32,
    rgba8: TileBytesIn,
}

impl<'de> Deserialize<'de> for PatternTile {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = PatternTileRepr::deserialize(d)?;
        PatternTile::new(r.name, r.width, r.height, r.rgba8.0).map_err(serde::de::Error::custom)
    }
}

/// `PatternFill::tile`'s reader: a stored tile whose size and bytes disagree
/// loads as no tile, so one bad pattern costs its overlay, not the file.
fn lenient_tile<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<PatternTile>, D::Error> {
    let r = Option::<PatternTileRepr>::deserialize(d)?;
    Ok(r.and_then(|r| PatternTile::new(r.name, r.width, r.height, r.rgba8.0).ok()))
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

// ---------------------------------------------------------------------------
// W9-H: contours, Blend If, extra effect instances
// ---------------------------------------------------------------------------

/// The named contour shapes of Photoshop's default contour picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ContourPreset {
    /// The identity: the falloff as the blur made it.
    #[default]
    Linear,
    /// Rises to the middle of the falloff and back down.
    Cone,
    /// A smooth S-shaped response.
    Gaussian,
    /// A band in the middle of the falloff (Photoshop's "Ring").
    Ring,
    /// Four rounded terraces (Photoshop's "Rounded Steps").
    RoundedSteps,
    /// The user's own curve, [`Contour::points`].
    Custom,
}

impl ContourPreset {
    /// Every preset, in the order the picker lists them.
    pub const ALL: [ContourPreset; 6] = [
        Self::Linear,
        Self::Cone,
        Self::Gaussian,
        Self::Ring,
        Self::RoundedSteps,
        Self::Custom,
    ];
}

/// W9-H: a contour, the response curve an effect's falloff is passed
/// through (input 0 = the far end of the falloff, 1 = full coverage).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Contour {
    pub preset: ContourPreset,
    /// The curve's knots, `[input, output]` in `0..=1`. Read only when
    /// `preset` is [`ContourPreset::Custom`]; fewer than two usable knots
    /// evaluate as linear.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<[f32; 2]>,
}

impl Contour {
    /// A named preset.
    pub fn preset(preset: ContourPreset) -> Self {
        Self {
            preset,
            points: Vec::new(),
        }
    }

    /// `true` for the identity, which a renderer may skip.
    pub fn is_linear(&self) -> bool {
        match self.preset {
            ContourPreset::Linear => true,
            ContourPreset::Custom => self.custom_knots().len() < 2,
            _ => false,
        }
    }

    fn is_default_linear(&self) -> bool {
        *self == Contour::default()
    }

    fn custom_knots(&self) -> Vec<[f32; 2]> {
        let mut k: Vec<[f32; 2]> = self
            .points
            .iter()
            .filter(|p| p[0].is_finite() && p[1].is_finite())
            .map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)])
            .collect();
        k.sort_by(|a, b| a[0].total_cmp(&b[0]));
        k
    }

    /// The contour at `t` (clamped to `0..=1`), in `0..=1`.
    pub fn eval(&self, t: f32) -> f32 {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let v = match self.preset {
            ContourPreset::Linear => t,
            ContourPreset::Cone => 1.0 - (2.0 * t - 1.0).abs(),
            // Smoothstep: a soft shoulder at both ends.
            ContourPreset::Gaussian => t * t * (3.0 - 2.0 * t),
            ContourPreset::Ring => {
                let d = (t - 0.5) / 0.2;
                (-d * d).exp()
            }
            ContourPreset::RoundedSteps => {
                const STEPS: f32 = 4.0;
                let x = t * STEPS;
                let base = x.floor().min(STEPS - 1.0);
                let f = (x - base).clamp(0.0, 1.0);
                let s = f * f * (3.0 - 2.0 * f);
                ((base + s) / STEPS).min(1.0)
            }
            ContourPreset::Custom => {
                let k = self.custom_knots();
                if k.len() < 2 {
                    t
                } else if t <= k[0][0] {
                    k[0][1]
                } else if t >= k[k.len() - 1][0] {
                    k[k.len() - 1][1]
                } else {
                    let i = k.iter().position(|p| p[0] > t).unwrap_or(k.len() - 1);
                    let (a, b) = (k[i - 1], k[i]);
                    let span = (b[0] - a[0]).max(f32::EPSILON);
                    a[1] + (b[1] - a[1]) * ((t - a[0]) / span)
                }
            }
        };
        v.clamp(0.0, 1.0)
    }
}

/// W9-H: one Blend If slider: its black and white ends, each split into
/// two handles (`[lo, hi]`, in `0..=1` of the encoded channel). Between a
/// pair's handles the layer fades in or out; below the black end or past
/// the white end it is hidden.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlendIfRange {
    pub black: [f32; 2],
    pub white: [f32; 2],
}

impl Default for BlendIfRange {
    fn default() -> Self {
        Self {
            black: [0.0, 0.0],
            white: [1.0, 1.0],
        }
    }
}

impl BlendIfRange {
    /// `true` when the range lets every value through.
    pub fn is_full(&self) -> bool {
        self.black[1] <= 0.0 && self.white[0] >= 1.0
    }

    /// How much of the layer survives at channel value `v` (`0..=1`).
    pub fn weight(&self, v: f32) -> f32 {
        if self.is_full() {
            return 1.0;
        }
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        // In at the black end: hidden below `lo`, fully in from `hi`.
        let [b0, b1] = self.black;
        let rise = if v >= b1 {
            1.0
        } else if v < b0 {
            0.0
        } else {
            ((v - b0) / (b1 - b0).max(1.0e-6)).clamp(0.0, 1.0)
        };
        // Out at the white end: fully in up to `lo`, hidden past `hi`.
        let [w0, w1] = self.white;
        let fall = if v <= w0 {
            1.0
        } else if v > w1 {
            0.0
        } else {
            (1.0 - (v - w0) / (w1 - w0).max(1.0e-6)).clamp(0.0, 1.0)
        };
        rise * fall
    }
}

/// W9-H: This Layer and Underlying Layer ranges for one channel.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BlendIfChannel {
    pub this_layer: BlendIfRange,
    pub underlying: BlendIfRange,
}

/// W9-H: the channel a Blend If slider pair reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum BlendIfSource {
    #[default]
    Gray,
    Red,
    Green,
    Blue,
}

impl BlendIfSource {
    pub const ALL: [BlendIfSource; 4] = [Self::Gray, Self::Red, Self::Green, Self::Blue];

    /// The channel's value for an encoded (document-space) straight colour.
    pub fn value(self, rgb: [f32; 3]) -> f32 {
        match self {
            Self::Gray => 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2],
            Self::Red => rgb[0],
            Self::Green => rgb[1],
            Self::Blue => rgb[2],
        }
    }
}

/// W9-H: Photoshop's Blend If, one slider set per channel. Every set
/// applies at once: their weights multiply.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BlendIf {
    pub gray: BlendIfChannel,
    pub red: BlendIfChannel,
    pub green: BlendIfChannel,
    pub blue: BlendIfChannel,
}

impl BlendIf {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// `true` when no range hides anything.
    pub fn is_identity(&self) -> bool {
        BlendIfSource::ALL.iter().all(|s| {
            let c = self.channel(*s);
            c.this_layer.is_full() && c.underlying.is_full()
        })
    }

    pub fn channel(&self, source: BlendIfSource) -> &BlendIfChannel {
        match source {
            BlendIfSource::Gray => &self.gray,
            BlendIfSource::Red => &self.red,
            BlendIfSource::Green => &self.green,
            BlendIfSource::Blue => &self.blue,
        }
    }

    pub fn channel_mut(&mut self, source: BlendIfSource) -> &mut BlendIfChannel {
        match source {
            BlendIfSource::Gray => &mut self.gray,
            BlendIfSource::Red => &mut self.red,
            BlendIfSource::Green => &mut self.green,
            BlendIfSource::Blue => &mut self.blue,
        }
    }

    /// How much of the layer survives where its own encoded colour is
    /// `this` and the encoded colour beneath it is `under`.
    pub fn weight(&self, this: [f32; 3], under: [f32; 3]) -> f32 {
        let mut w = 1.0;
        for s in BlendIfSource::ALL {
            let c = self.channel(s);
            if !c.this_layer.is_full() {
                w *= c.this_layer.weight(s.value(this));
            }
            if !c.underlying.is_full() {
                w *= c.underlying.weight(s.value(under));
            }
        }
        w
    }
}

/// W9-H: the contours of the primary effect slots.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleContours {
    #[serde(skip_serializing_if = "Contour::is_default_linear")]
    pub drop_shadow: Contour,
    #[serde(skip_serializing_if = "Contour::is_default_linear")]
    pub inner_shadow: Contour,
    #[serde(skip_serializing_if = "Contour::is_default_linear")]
    pub outer_glow: Contour,
    #[serde(skip_serializing_if = "Contour::is_default_linear")]
    pub inner_glow: Contour,
    /// Bevel and Emboss's gloss contour, applied to its shading.
    #[serde(skip_serializing_if = "Contour::is_default_linear")]
    pub bevel: Contour,
}

impl StyleContours {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// W9-H: an extra shadow instance and the contour it is drawn through.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShadowInstance {
    pub effect: ShadowEffect,
    #[serde(skip_serializing_if = "Contour::is_default_linear")]
    pub contour: Contour,
}

/// W9-H: everything a style carries beyond the ten primary slots.
///
/// The extra instances of a repeatable effect are drawn **above** the
/// primary slot of the same kind, in list order (the last one topmost).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleExtras {
    #[serde(skip_serializing_if = "BlendIf::is_default")]
    pub blend_if: BlendIf,
    #[serde(skip_serializing_if = "StyleContours::is_default")]
    pub contours: StyleContours,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drop_shadows: Vec<ShadowInstance>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inner_shadows: Vec<ShadowInstance>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub strokes: Vec<StrokeEffect>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub color_overlays: Vec<ColorOverlayEffect>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gradient_overlays: Vec<GradientOverlayEffect>,
}

impl StyleExtras {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// How many extra effect instances are stored.
    pub fn instance_count(&self) -> usize {
        self.drop_shadows.len()
            + self.inner_shadows.len()
            + self.strokes.len()
            + self.color_overlays.len()
            + self.gradient_overlays.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- W9-H: contours, Blend If, instances ------------------------------

    #[test]
    fn contour_presets_shape_the_falloff() {
        let lin = Contour::default();
        assert!(lin.is_linear());
        assert_eq!(lin.eval(0.3), 0.3);
        let cone = Contour::preset(ContourPreset::Cone);
        assert!(!cone.is_linear());
        assert_eq!(cone.eval(0.5), 1.0);
        assert_eq!(cone.eval(1.0), 0.0);
        let ring = Contour::preset(ContourPreset::Ring);
        assert!(ring.eval(0.5) > 0.99 && ring.eval(1.0) < 0.01);
        let steps = Contour::preset(ContourPreset::RoundedSteps);
        assert!(
            (steps.eval(0.26) - steps.eval(0.3)).abs() < 0.05,
            "a terrace"
        );
        assert_eq!(steps.eval(1.0), 1.0);
        let gauss = Contour::preset(ContourPreset::Gaussian);
        assert!(gauss.eval(0.2) < 0.2 && gauss.eval(0.8) > 0.8);
        let custom = Contour {
            preset: ContourPreset::Custom,
            points: vec![[0.0, 1.0], [1.0, 0.0]],
        };
        assert!(
            (custom.eval(0.25) - 0.75).abs() < 1.0e-6,
            "an inverted curve"
        );
        let broken = Contour {
            preset: ContourPreset::Custom,
            points: vec![[f32::NAN, 0.0]],
        };
        assert!(broken.is_linear());
        assert_eq!(broken.eval(0.4), 0.4);
    }

    #[test]
    fn blend_if_hides_the_layer_outside_its_ranges() {
        let mut b = BlendIf::default();
        assert!(b.is_identity());
        assert_eq!(b.weight([0.0; 3], [0.0; 3]), 1.0);
        // Underlying 128..255: hidden over dark, shown over light.
        b.gray.underlying.black = [128.0 / 255.0, 128.0 / 255.0];
        assert!(!b.is_identity());
        assert_eq!(b.weight([1.0; 3], [0.1; 3]), 0.0);
        assert_eq!(b.weight([1.0; 3], [0.9; 3]), 1.0);
        // A split handle fades rather than cuts.
        let r = BlendIfRange {
            black: [0.2, 0.6],
            white: [1.0, 1.0],
        };
        assert!((r.weight(0.4) - 0.5).abs() < 1.0e-5);
        assert_eq!(r.weight(1.0), 1.0, "the default white end keeps white");
        // The white end hides the brightest values of this layer.
        let mut c = BlendIf::default();
        c.red.this_layer.white = [0.5, 0.5];
        assert_eq!(c.weight([0.9, 0.0, 0.0], [0.0; 3]), 0.0);
        assert_eq!(c.weight([0.4, 0.0, 0.0], [0.0; 3]), 1.0);
    }

    #[test]
    fn extras_are_invisible_on_disk_until_used_and_round_trip_when_used() {
        assert_eq!(
            serde_json::to_string(&LayerEffects::default()).unwrap(),
            "{}"
        );
        let mut e = LayerEffects {
            drop_shadow: Some(ShadowEffect::default()),
            ..Default::default()
        };
        let before = serde_json::to_string(&e).unwrap();
        assert!(!before.contains("extras"), "{before}");
        e.extras.drop_shadows.push(ShadowInstance {
            effect: ShadowEffect {
                distance_px: 20.0,
                ..Default::default()
            },
            contour: Contour::preset(ContourPreset::Ring),
        });
        e.extras.strokes.push(StrokeEffect::default());
        e.extras.contours.outer_glow = Contour::preset(ContourPreset::Cone);
        e.extras.blend_if.gray.underlying.black = [0.5, 0.5];
        assert_eq!(e.count(), 3);
        assert_eq!(e.drop_shadows().len(), 2);
        assert_eq!(e.strokes().len(), 1);
        let json = serde_json::to_string(&e).unwrap();
        let back: LayerEffects = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
        // A document written before W9-H loads with no extras.
        let old: LayerEffects = serde_json::from_str(&before).unwrap();
        assert!(old.extras.is_default());
        // An extra instance alone still counts as a style.
        let only_extra = LayerEffects {
            extras: StyleExtras {
                strokes: vec![StrokeEffect::default()],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!only_extra.is_empty());
        assert!(only_extra.affects_composite());
        // Blend If alone is not an effect.
        let blend_only = LayerEffects {
            extras: StyleExtras {
                blend_if: e.extras.blend_if,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(blend_only.is_empty());
        assert!(!blend_only.is_default());
    }

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
            extras: StyleExtras::default(),
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
            extras: StyleExtras::default(),
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

    // ---- W7-B: pattern tiles ride the fill -------------------------------

    fn checker() -> PatternTile {
        PatternTile::new(
            "Checker",
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 0, 255, 255, //
                0, 0, 255, 255, 255, 0, 0, 255,
            ],
        )
        .unwrap()
    }

    #[test]
    fn a_pattern_tile_checks_its_size_and_bytes() {
        assert_eq!(
            PatternTile::new("x", 0, 2, Vec::new()).unwrap_err(),
            PatternTileError::Empty
        );
        assert!(matches!(
            PatternTile::new("x", 2, 2, vec![0; 15]).unwrap_err(),
            PatternTileError::WrongLength {
                expected: 16,
                found: 15,
                ..
            }
        ));
        assert!(matches!(
            PatternTile::new("x", MAX_PATTERN_EDGE + 1, 1, Vec::new()).unwrap_err(),
            PatternTileError::TooLarge { .. }
        ));
        let t = checker();
        assert_eq!(t.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(t.pixel(2, 2), [255, 0, 0, 255], "the tiling wraps");
        assert_eq!(t.pixel(-1, 0), [0, 0, 255, 255], "and wraps below zero");
    }

    #[test]
    fn the_content_hash_follows_the_pixels_not_the_name() {
        let a = checker();
        let renamed = PatternTile::new("Other", 2, 2, a.rgba8().to_vec()).unwrap();
        assert_eq!(a.content_hash(), renamed.content_hash());
        let mut bytes = a.rgba8().to_vec();
        bytes[5] = 1;
        let changed = PatternTile::new("Checker", 2, 2, bytes).unwrap();
        assert_ne!(a.content_hash(), changed.content_hash());
        assert_ne!(a, changed);
        // The same bytes at a different shape are a different pattern.
        let wide = PatternTile::new("Checker", 4, 1, a.rgba8().to_vec()).unwrap();
        assert_ne!(a.content_hash(), wide.content_hash());
    }

    #[test]
    fn a_pattern_fill_round_trips_with_its_pixels() {
        let fill = PatternFill {
            tile: Some(checker()),
            scale: 2.0,
            ..PatternFill::default()
        };
        assert!(fill.is_drawable());
        let e = LayerEffects {
            pattern_overlay: Some(PatternOverlayEffect {
                pattern: fill.clone(),
                ..Default::default()
            }),
            stroke: Some(StrokeEffect {
                fill: FillStyle::Pattern(fill.clone()),
                ..Default::default()
            }),
            ..LayerEffects::default()
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: LayerEffects = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
        let tile = back.pattern_overlay.unwrap().pattern.tile.unwrap();
        assert_eq!(tile.rgba8(), checker().rgba8());
        assert_eq!(tile.name(), "Checker");
        // An untiled fill writes no tile key at all.
        let bare = serde_json::to_string(&PatternFill::default()).unwrap();
        assert!(!bare.contains("tile"), "{bare}");
        assert!(!PatternFill::default().is_drawable());
    }

    #[test]
    fn a_malformed_stored_tile_loads_as_no_tile_not_as_a_refusal() {
        let json = r#"{"tile":{"name":"Bad","width":2,"height":2,"rgba8":[1,2,3]},"scale":3.0}"#;
        let fill: PatternFill = serde_json::from_str(json).unwrap();
        assert!(fill.tile.is_none());
        assert_eq!(fill.scale, 3.0, "the rest of the fill still loads");
    }
}
