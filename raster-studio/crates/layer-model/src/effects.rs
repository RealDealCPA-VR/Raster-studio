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
