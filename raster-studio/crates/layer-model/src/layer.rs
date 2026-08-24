//! Layer types. Mirrors the design doc's `LayerKind`/`Layer` shape.

use glam::Affine2;
use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::effects::{LayerEffects, Rgba};
use crate::ids::{AssetId, LayerId, MaskId};
use crate::mask::LayerMask;

/// Straight-alpha opaque black in the document's colour space.
const OPAQUE_BLACK: Rgba = [0.0, 0.0, 0.0, 1.0];

/// The variant-specific payload of a layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayerKind {
    Raster(RasterLayer),
    Group(GroupLayer),
    Adjustment(AdjustmentLayer),
    Text(TextLayer),
    Shape(ShapeLayer),
    SmartObject(SmartObjectLayer),
    Generator(GeneratorLayer),
}

fn one() -> f32 {
    1.0
}

/// Common properties shared by every layer regardless of kind.
///
/// `PartialEq` is derived so undo/redo tests can assert whole-document
/// equality. It compares `f32` fields with IEEE semantics, so a layer holding a
/// NaN (nothing in this crate produces one, but `opacity` and `transform` are
/// public) does not compare equal even to itself.
///
/// The numeric ranges documented on `opacity` and `fill_opacity` are
/// *expectations*, not enforced invariants — see the "Numeric ranges" section
/// of the [crate docs](crate). Read them through
/// [`Layer::effective_opacity`] / [`Layer::effective_fill_opacity`] to get a
/// value that is guaranteed finite and in `0.0..=1.0`.
///
/// Layer ownership of children is *not* stored here — see [`crate::LayerTree`],
/// which enforces that an id appears under at most one parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub locked: LockState,
    /// Overall opacity. Scales the layer *and* its effects. Expected in
    /// `0.0..=1.0`; nothing enforces that, so read it through
    /// [`Layer::effective_opacity`] before compositing.
    pub opacity: f32,
    /// Opacity of the layer's own pixels only; layer effects are unaffected.
    /// Photoshop's "Fill". Defaults to 1.0 for documents written before this
    /// field existed. Expected in `0.0..=1.0`; nothing enforces that, so read
    /// it through [`Layer::effective_fill_opacity`] before compositing.
    #[serde(default = "one")]
    pub fill_opacity: f32,
    pub blend_mode: BlendMode,
    /// Layer-to-document affine transform (non-destructive).
    #[serde(with = "affine2_serde")]
    pub transform: Affine2,
    /// Attached mask, if any. Carries everything the compositor needs to turn
    /// the referenced coverage data into an alpha multiplier.
    ///
    /// **Wire format.** This field used to serialize as a bare [`MaskId`]
    /// string; it is now the whole [`LayerMask`] object, because a mask id
    /// alone cannot tell the compositor whether the mask is enabled, inverted,
    /// feathered or at what density. A `null` mask is unaffected, but a
    /// pre-0.1.0 document that actually *had* a mask no longer loads. That is
    /// deliberate rather than papered over with a legacy string form: nothing
    /// has shipped at this version, and a permanently accepted alternate shape
    /// would cost more than the migration it saves. Documents from 0.1.0
    /// onward round-trip.
    #[serde(default)]
    pub mask: Option<LayerMask>,
    pub clipping: ClippingMode,
    /// Layer styles. Empty by default, and the key is omitted entirely from
    /// serialized output while the block is untouched — see
    /// [`LayerEffects::is_default`].
    #[serde(default, skip_serializing_if = "LayerEffects::is_default")]
    pub effects: LayerEffects,
    pub kind: LayerKind,
}

impl Layer {
    /// Construct a raster layer with sensible defaults.
    pub fn raster(name: impl Into<String>) -> Self {
        Self::with_kind(name, LayerKind::Raster(RasterLayer::default()))
    }

    /// Construct an empty group.
    pub fn group(name: impl Into<String>) -> Self {
        Self::with_kind(name, LayerKind::Group(GroupLayer::default()))
    }

    /// Construct a layer of any kind with default common properties.
    ///
    /// A [`LayerKind::Group`] must be handed to the tree **empty**. Every
    /// insertion path ([`crate::LayerTree::push_root`],
    /// [`crate::LayerTree::insert_at`]) rejects a group that already names
    /// children: an id the tree does not know is [`crate::TreeError::NotFound`],
    /// and an id it does know already has exactly one parent (invariant 2), so
    /// it is [`crate::TreeError::AlreadyParented`]. There is no input for which
    /// a pre-populated group is accepted.
    ///
    /// To create a group *around* existing layers in one step — Photoshop's
    /// "Group Selected Layers" — use [`crate::LayerTree::group_layers`], which
    /// inserts the empty group and re-parents the children atomically. To fill
    /// a group afterwards, use [`crate::LayerTree::move_layer`] or
    /// [`crate::LayerTree::insert_at`].
    pub fn with_kind(name: impl Into<String>, kind: LayerKind) -> Self {
        Self {
            id: LayerId::new(),
            name: name.into(),
            visible: true,
            locked: LockState::default(),
            opacity: 1.0,
            fill_opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: Affine2::IDENTITY,
            mask: None,
            clipping: ClippingMode::None,
            effects: LayerEffects::default(),
            kind,
        }
    }

    pub fn is_group(&self) -> bool {
        matches!(self.kind, LayerKind::Group(_))
    }

    /// The ids of this layer's direct children, or an empty slice for a
    /// non-group.
    pub fn children(&self) -> &[LayerId] {
        match &self.kind {
            LayerKind::Group(g) => &g.children,
            _ => &[],
        }
    }

    /// The mask id, regardless of whether the mask is enabled.
    pub fn mask_id(&self) -> Option<MaskId> {
        self.mask.as_ref().map(|m| m.id)
    }

    /// The mask only when it can change the composite — a disabled or
    /// zero-density mask resolves to `None` so the compositor can skip it.
    pub fn effective_mask(&self) -> Option<&LayerMask> {
        self.mask.as_ref().filter(|m| m.affects_composite())
    }

    /// Attach a mask, replacing any existing one. Returns the previous mask.
    pub fn set_mask(&mut self, mask: LayerMask) -> Option<LayerMask> {
        self.mask.replace(mask)
    }

    /// `true` when this layer is clipped to the layer beneath it.
    pub fn is_clipping(&self) -> bool {
        self.clipping == ClippingMode::ClipToBelow
    }

    /// [`Layer::opacity`] made safe to multiply with: always finite and within
    /// `0.0..=1.0`.
    ///
    /// The field itself is public and unvalidated, so a hand-edited document can
    /// hold `5.0` or (through a binary format) a NaN. This is the accessor the
    /// compositor uses; a non-finite value resolves to `0.0` rather than
    /// poisoning the accumulator (see [`crate::blend::unit`]).
    pub fn effective_opacity(&self) -> f32 {
        crate::blend::unit(self.opacity)
    }

    /// [`Layer::fill_opacity`] made safe to multiply with, on the same terms as
    /// [`Layer::effective_opacity`].
    pub fn effective_fill_opacity(&self) -> f32 {
        crate::blend::unit(self.fill_opacity)
    }

    /// `true` when the layer contributes nothing to the composite and the
    /// render graph may drop it entirely.
    ///
    /// Uses [`Layer::effective_opacity`], so a NaN opacity counts as a no-op
    /// rather than slipping through the comparison (`NaN <= 0.0` is `false`).
    pub fn is_noop(&self) -> bool {
        !self.visible || self.effective_opacity() <= 0.0
    }
}

/// Lock flags. Any subset can be engaged independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LockState {
    pub pixels: bool,
    pub position: bool,
    pub transparency: bool,
    /// Locks everything, including renaming and deletion.
    pub all: bool,
}

impl LockState {
    /// `true` when any lock at all is engaged.
    pub fn any(self) -> bool {
        self.pixels || self.position || self.transparency || self.all
    }

    /// `true` when pixel edits must be refused.
    pub fn blocks_pixel_edit(self) -> bool {
        self.all || self.pixels
    }

    /// `true` when transform / move must be refused.
    pub fn blocks_transform(self) -> bool {
        self.all || self.position
    }
}

/// Clipping-mask behavior relative to the layer directly below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ClippingMode {
    #[default]
    None,
    /// Clip to the alpha of the layer beneath.
    ClipToBelow,
}

/// Pixel content lives as tiles owned by the asset/tile store; a raster layer
/// references them indirectly. Kept minimal in the scaffold.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RasterLayer {
    /// Optional origin asset this raster was imported from (for provenance).
    pub source_asset: Option<AssetId>,
}

/// How a group composites its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum GroupBlending {
    /// Children are composited into their own buffer, then that buffer is
    /// blended into the parent using the group's own blend mode and opacity.
    /// Required whenever the group's blend mode is not `Normal`.
    #[default]
    Isolated,
    /// Children blend directly against whatever is beneath the group, as if the
    /// group did not exist (Photoshop's "Pass Through").
    PassThrough,
}

/// A group owns an ordered list of child layer ids (top-most first).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupLayer {
    pub children: Vec<LayerId>,
    /// Collapsed state in the layers panel. Purely presentational.
    pub collapsed: bool,
    pub blending: GroupBlending,
}

/// A non-destructive adjustment applied to everything beneath it (or clipped).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdjustmentLayer {
    pub kind: AdjustmentKind,
}

/// Which of the three automatic tone / colour commands an
/// [`AdjustmentKind::Auto`] layer stands for.
///
/// An auto command is an *analysis*, not a pixel function: it reads the
/// backdrop's histogram and emits a concrete Levels. Storing the command rather
/// than the Levels it happened to produce is what keeps it non-destructive —
/// re-open the document against different pixels and it re-decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AutoAdjustment {
    /// Auto Contrast: one black/white point from the composite gray, applied to
    /// every channel, so contrast changes and colour balance does not.
    Contrast,
    /// Auto Tone: an independent black/white point per channel.
    Tone,
    /// Auto Color: per-channel black/white points and a per-channel gamma that
    /// lands each channel's median on mid-gray.
    Color,
}

/// The parametric adjustments an adjustment layer can carry.
///
/// This is the **persisted** vocabulary: every variant here survives a
/// save/reload, which is what "adjustments remain editable after save/reload"
/// means. The `adjustments` crate owns the evaluation of each one and its
/// parameter validation; the shapes here are deliberately plain data so the
/// project format has nothing to interpret.
///
/// Ranges are stated per variant, but nothing here enforces them — a value out
/// of range is a document that opens with a slider clamped back, never a
/// refusal to open. See `adjustments::Adjustment`'s lenient `From` impl.
///
/// # Narrow and full spellings
///
/// Five adjustments have two variants each: a narrow one that predates the
/// `adjustments` crate (`Levels`, `Curves`, `Exposure`, `HueSaturation`,
/// `ColorBalance`) and a `*Full` one that carries the settings the narrow
/// shape has no room for — per-channel Levels and Curves, a Levels output
/// range, an exposure offset and gamma, a colorizing Hue/Saturation, a
/// luminosity-preserving Colour Balance.
///
/// The narrow variants were **not** widened, because every existing document
/// and every existing `match` in the workspace spells them the old way. A
/// writer should emit the narrow variant whenever the settings fit — that is
/// what `adjustments::Adjustment::to_layer_kind` does — and reach for the full
/// one only when they do not, so the common document stays byte-compatible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AdjustmentKind {
    /// Composite-only Levels over the full `0..=1` output range.
    Levels { black: f32, white: f32, gamma: f32 },
    /// Levels with per-channel mappings and an output range.
    ///
    /// Each `[f32; 5]` is
    /// `[input_black, input_white, gamma, output_black, output_white]`. The
    /// per-channel mappings run first, then `composite`. The identity mapping
    /// is `[0.0, 1.0, 1.0, 0.0, 1.0]`.
    LevelsFull {
        composite: [f32; 5],
        red: [f32; 5],
        green: [f32; 5],
        blue: [f32; 5],
    },
    /// Composite-only Curves.
    Curves { points: Vec<[f32; 2]> },
    /// Curves with a curve per channel. The per-channel curves run first, then
    /// `composite`. The identity curve is `[[0.0, 0.0], [1.0, 1.0]]`, which is
    /// how a channel that is not being curved is spelled.
    CurvesFull {
        composite: Vec<[f32; 2]>,
        red: Vec<[f32; 2]>,
        green: Vec<[f32; 2]>,
        blue: Vec<[f32; 2]>,
    },
    /// Exposure in stops alone, on linear light.
    Exposure { stops: f32 },
    /// Exposure with the offset and gamma the control also carries:
    /// `((v · 2^stops) + offset) ^ (1/gamma)`. `offset` is in `-1.0..=1.0` and
    /// `gamma` in `0.01..=100.0`.
    ExposureFull { stops: f32, offset: f32, gamma: f32 },
    /// Hue rotation, saturation and lightness.
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    /// Hue/Saturation including colorize mode. When `colorize` is `Some`, it is
    /// `[hue_degrees, saturation, lightness]` and it *replaces* the hue and
    /// saturation of every pixel rather than shifting them; `hue`, `saturation`
    /// and `lightness` are then unused.
    HueSaturationFull {
        hue: f32,
        saturation: f32,
        lightness: f32,
        colorize: Option<[f32; 3]>,
    },
    /// Shadow / midtone / highlight colour shifts.
    ColorBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
    },
    /// Colour Balance with the "preserve luminosity" switch, which renormalises
    /// each pixel back to the luminance it had before the shift.
    ColorBalanceFull {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        preserve_luminosity: bool,
    },
    /// Brightness and contrast, both in `-1.0..=1.0`, on encoded values.
    BrightnessContrast { brightness: f32, contrast: f32 },
    /// Vibrance (a saturation boost weighted toward the *dull* colours) and a
    /// flat saturation, both in `-1.0..=1.0`.
    Vibrance { vibrance: f32, saturation: f32 },
    /// Black & White. `weights` is
    /// `[red, yellow, green, cyan, blue, magenta]`, each in `-3.0..=3.0`;
    /// `tint` is an optional `[hue_degrees, saturation]` pair.
    BlackAndWhite {
        weights: [f32; 6],
        tint: Option<[f32; 2]>,
    },
    /// A photographic filter: an **sRGB-encoded** filter colour, a `density` in
    /// `0.0..=1.0`, and whether the filter is allowed to darken the image.
    PhotoFilter {
        color_srgb: [f32; 3],
        density: f32,
        preserve_luminosity: bool,
    },
    /// Channel mixer. `rows[out]` is `[red, green, blue, constant]`; in
    /// `monochrome` mode `rows[0]` alone drives all three outputs.
    ChannelMixer {
        rows: [[f32; 4]; 3],
        monochrome: bool,
    },
    /// Invert. Carries no parameters, which is why it is a unit variant.
    Invert,
    /// Posterize to `levels` steps per channel, `2..=256`.
    Posterize { levels: u32 },
    /// Threshold at an encoded gray `level` in `0.0..=1.0`.
    Threshold { level: f32 },
    /// Gradient map. `stops` are `(position, sRGB-encoded colour)` pairs; at
    /// least two distinct positions are needed for the map to mean anything.
    GradientMap {
        stops: Vec<(f32, [f32; 3])>,
        reverse: bool,
    },
    /// Selective colour: nine ranges of `[cyan, magenta, yellow, black]` deltas
    /// in `-1.0..=1.0`, ordered
    /// `[reds, yellows, greens, cyans, blues, magentas, whites, neutrals,
    /// blacks]`. `relative` scales the ink already present; otherwise the delta
    /// is added outright.
    SelectiveColor {
        ranges: [[f32; 4]; 9],
        relative: bool,
    },
    /// Auto Tone / Auto Contrast / Auto Color, with the fraction of pixels
    /// clipped at *each* end of the histogram (`0.0..=0.1`).
    Auto { mode: AutoAdjustment, clip: f32 },
}

/// Editable text layer. Postponed (Phase 3); shape reserved so the enum and
/// serialization are forward-compatible.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TextLayer {
    pub text: String,
    pub font_family: String,
    pub size_px: f32,
}

/// Which points a shape layer's path encloses.
///
/// The same two rules every vector renderer offers, and the same two
/// `vector::FillRule` spells — kept as a separate enum here because this is the
/// *persisted* vocabulary and `layer-model` sits below `vector`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ShapeFillRule {
    /// Inside when the signed winding number is not zero.
    #[default]
    NonZero,
    /// Inside when a ray crosses the path an odd number of times.
    EvenOdd,
}

/// End treatment for the open ends of a shape layer's stroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ShapeCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// Corner treatment for a shape layer's stroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ShapeJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// The stroke drawn along a shape layer's path.
///
/// This is the shape's *own* stroke — the one the pen and shape tools set — and
/// is a different thing from [`crate::StrokeEffect`], which traces the alpha
/// edge of whatever the layer already drew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShapeStroke {
    /// Straight-alpha RGBA in the document's colour space.
    pub color: Rgba,
    /// Total stroke width in document pixels; half sits either side of the
    /// path. Expected `>= 0.0`; the compositor clamps.
    pub width_px: f32,
    pub cap: ShapeCap,
    pub join: ShapeJoin,
    /// Longest miter as a multiple of half the width before a
    /// [`ShapeJoin::Miter`] degrades to a bevel. SVG's default is 4.
    pub miter_limit: f32,
    /// Alternating on/off dash lengths in document pixels, starting with "on".
    /// Empty means a solid stroke; an odd-length pattern repeats to make it
    /// even, the SVG rule.
    pub dash: Vec<f32>,
    /// How far into the dash pattern the stroke starts.
    pub dash_offset: f32,
}

impl Default for ShapeStroke {
    fn default() -> Self {
        Self {
            color: OPAQUE_BLACK,
            width_px: 1.0,
            cap: ShapeCap::default(),
            join: ShapeJoin::default(),
            miter_limit: 4.0,
            dash: Vec::new(),
            dash_offset: 0.0,
        }
    }
}

/// Vector shape layer: a path plus how it is painted.
///
/// # Why the path is still SVG text
///
/// `path_svg` is not a placeholder any more — it is the serialised form of a
/// real `vector::Path`, written by `vector::to_svg` and read back by
/// `vector::parse_svg` with the geometry intact (`vector`'s own
/// `a_shape_survives_a_full_round_of_editing` pins the round trip). Storing the
/// path as its standard text encoding rather than as a second, bespoke list of
/// segments keeps one representation instead of two, and keeps `layer-model`
/// below `vector` in the dependency order rather than beside it.
///
/// # Fill and stroke
///
/// A shape with no `fill` and no `stroke` draws nothing. The default is an
/// opaque black fill and no stroke, which is what a freshly drawn shape looks
/// like — and, because container-level `#[serde(default)]` fills missing keys
/// from [`ShapeLayer::default`], what a document written before these fields
/// existed loads as. Such a document used to composite to nothing at all, so
/// the change can only make an old shape layer visible, never change one that
/// was already drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShapeLayer {
    /// The path, as SVG path data in the layer's own pixel space.
    pub path_svg: String,
    /// Interior paint. Straight-alpha RGBA in the document's colour space;
    /// `None` leaves the shape unfilled.
    pub fill: Option<Rgba>,
    /// Which points the fill considers inside.
    pub fill_rule: ShapeFillRule,
    /// Outline paint, or `None` for no stroke.
    pub stroke: Option<ShapeStroke>,
}

impl Default for ShapeLayer {
    fn default() -> Self {
        Self {
            path_svg: String::new(),
            fill: Some(OPAQUE_BLACK),
            fill_rule: ShapeFillRule::NonZero,
            stroke: None,
        }
    }
}

impl ShapeLayer {
    /// A black-filled shape from SVG path data.
    pub fn from_svg(path_svg: impl Into<String>) -> Self {
        Self {
            path_svg: path_svg.into(),
            ..Self::default()
        }
    }

    /// `true` when the layer names geometry to draw *and* something to draw it
    /// with. A shape that is neither filled nor stroked contributes nothing.
    pub fn is_drawable(&self) -> bool {
        !self.path_svg.trim().is_empty() && (self.fill.is_some() || self.stroke.is_some())
    }
}

/// Embedded or linked document rendered non-destructively (Phase 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartObjectLayer {
    pub asset: AssetId,
    pub linked: bool,
}

/// A generator layer whose pixels are produced by an AI operation. Carries a
/// reference to recorded provenance so the result stays reproducible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneratorLayer {
    /// Free-form key into the document's AI provenance records.
    pub provenance_key: String,
}

/// serde adapter for `glam::Affine2` (stored as its 6 matrix components).
mod affine2_serde {
    use glam::Affine2;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(a: &Affine2, s: S) -> Result<S::Ok, S::Error> {
        a.to_cols_array().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Affine2, D::Error> {
        let arr = <[f32; 6]>::deserialize(d)?;
        Ok(Affine2::from_cols_array(&arr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{ShadowEffect, StrokeEffect};
    use crate::mask::LayerMask;

    #[test]
    fn raster_layer_defaults() {
        let l = Layer::raster("Background");
        assert_eq!(l.opacity, 1.0);
        assert_eq!(l.fill_opacity, 1.0);
        assert!(l.visible);
        assert!(matches!(l.kind, LayerKind::Raster(_)));
        assert!(l.effects.is_empty());
        assert!(l.mask.is_none());
        assert!(!l.is_clipping());
        assert!(!l.is_noop());
    }

    #[test]
    fn layer_serde_roundtrip() {
        let l = Layer::group("Group 1");
        let json = serde_json::to_string(&l).unwrap();
        let back: Layer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, l);
        assert!(back.is_group());
    }

    #[test]
    fn layer_partial_eq_sees_every_field() {
        let base = Layer::raster("L");

        let mut a = base.clone();
        a.opacity = 0.5;
        assert_ne!(a, base);

        let mut b = base.clone();
        b.fill_opacity = 0.5;
        assert_ne!(b, base);

        let mut c = base.clone();
        c.locked.pixels = true;
        assert_ne!(c, base, "LockState must participate in equality");

        let mut d = base.clone();
        d.effects.stroke = Some(StrokeEffect::default());
        assert_ne!(d, base, "effects must participate in equality");

        let mut e = base.clone();
        e.mask = Some(LayerMask::new(MaskId::new()));
        assert_ne!(e, base);

        let mut f = base.clone();
        f.transform = Affine2::from_translation(glam::Vec2::new(3.0, 4.0));
        assert_ne!(f, base);

        let mut g = base.clone();
        g.kind = LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Exposure { stops: 1.0 },
        });
        assert_ne!(g, base, "LayerKind must participate in equality");

        assert_eq!(base.clone(), base);
    }

    #[test]
    fn adjustment_kind_partial_eq() {
        let a = AdjustmentKind::Levels {
            black: 0.0,
            white: 1.0,
            gamma: 1.0,
        };
        let b = AdjustmentKind::Levels {
            black: 0.0,
            white: 1.0,
            gamma: 2.2,
        };
        assert_eq!(a, a.clone());
        assert_ne!(a, b);
        assert_ne!(
            AdjustmentKind::Curves { points: vec![] },
            AdjustmentKind::Curves {
                points: vec![[0.0, 0.0]]
            }
        );
    }

    /// Every adjustment the editor offers must survive a save/reload, which is
    /// the whole reason `AdjustmentKind` exists as a separate, plain-data
    /// vocabulary. A variant that cannot be stored cannot be an adjustment
    /// *layer* at all, only a transient computation.
    #[test]
    fn every_adjustment_kind_round_trips_through_serde() {
        let all = vec![
            AdjustmentKind::Levels {
                black: 0.05,
                white: 0.95,
                gamma: 1.2,
            },
            AdjustmentKind::LevelsFull {
                composite: [0.02, 0.98, 1.1, 0.05, 0.9],
                red: [0.0, 0.9, 1.0, 0.0, 1.0],
                green: [0.1, 1.0, 1.3, 0.0, 1.0],
                blue: [0.0, 1.0, 1.0, 0.0, 1.0],
            },
            AdjustmentKind::Curves {
                points: vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
            },
            AdjustmentKind::CurvesFull {
                composite: vec![[0.0, 0.0], [0.5, 0.55], [1.0, 1.0]],
                red: vec![[0.0, 0.1], [1.0, 1.0]],
                green: vec![[0.0, 0.0], [1.0, 1.0]],
                blue: vec![[0.0, 0.0], [1.0, 0.9]],
            },
            AdjustmentKind::Exposure { stops: -1.5 },
            AdjustmentKind::ExposureFull {
                stops: 1.25,
                offset: -0.03,
                gamma: 1.4,
            },
            AdjustmentKind::HueSaturation {
                hue: 30.0,
                saturation: 0.2,
                lightness: -0.1,
            },
            AdjustmentKind::HueSaturationFull {
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
                colorize: Some([210.0, 0.45, -0.05]),
            },
            AdjustmentKind::ColorBalance {
                shadows: [0.1, 0.0, -0.2],
                midtones: [0.0; 3],
                highlights: [0.0, 0.3, 0.0],
            },
            AdjustmentKind::ColorBalanceFull {
                shadows: [0.1, 0.0, -0.2],
                midtones: [0.0; 3],
                highlights: [0.0, 0.3, 0.0],
                preserve_luminosity: true,
            },
            AdjustmentKind::BrightnessContrast {
                brightness: 0.1,
                contrast: -0.25,
            },
            AdjustmentKind::Vibrance {
                vibrance: 0.4,
                saturation: -0.1,
            },
            AdjustmentKind::BlackAndWhite {
                weights: [0.4, 0.6, 0.4, 0.6, 0.2, 0.8],
                tint: Some([30.0, 0.35]),
            },
            AdjustmentKind::BlackAndWhite {
                weights: [1.0; 6],
                tint: None,
            },
            AdjustmentKind::PhotoFilter {
                color_srgb: [0.92, 0.69, 0.07],
                density: 0.25,
                preserve_luminosity: true,
            },
            // Every `bool` and `Option` in the fixture is set away from its
            // default, or a field that failed to serialize would round trip
            // back to the same value and the test would not notice.
            AdjustmentKind::ChannelMixer {
                rows: [
                    [0.9, 0.1, 0.0, 0.02],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.1, 0.0, 0.9, 0.0],
                ],
                monochrome: true,
            },
            AdjustmentKind::Invert,
            AdjustmentKind::Posterize { levels: 6 },
            AdjustmentKind::Threshold { level: 0.45 },
            AdjustmentKind::GradientMap {
                stops: vec![(0.0, [0.1, 0.0, 0.3]), (1.0, [1.0, 0.9, 0.4])],
                reverse: true,
            },
            AdjustmentKind::SelectiveColor {
                ranges: [[0.2, -0.1, 0.05, 0.05]; 9],
                relative: true,
            },
            AdjustmentKind::Auto {
                mode: AutoAdjustment::Tone,
                clip: 0.001,
            },
            AdjustmentKind::Auto {
                mode: AutoAdjustment::Contrast,
                clip: 0.0,
            },
            AdjustmentKind::Auto {
                mode: AutoAdjustment::Color,
                clip: 0.01,
            },
        ];
        for kind in &all {
            let json = serde_json::to_string(kind).unwrap();
            let back: AdjustmentKind = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, kind, "{json}");
            // And inside the layer it actually lives in.
            let layer = Layer::with_kind(
                "Adj",
                LayerKind::Adjustment(AdjustmentLayer { kind: kind.clone() }),
            );
            let json = serde_json::to_string(&layer).unwrap();
            let back: Layer = serde_json::from_str(&json).unwrap();
            assert_eq!(back, layer);
        }
        // No two of the samples collapse onto each other, so the round trip
        // above is really distinguishing variants and not comparing a
        // degenerate default with itself.
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_shape_layer_defaults_to_a_black_fill_and_no_stroke() {
        let s = ShapeLayer::from_svg("M0 0 L10 0 L10 10 Z");
        assert_eq!(s.fill, Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(s.fill_rule, ShapeFillRule::NonZero);
        assert!(s.stroke.is_none());
        assert!(s.is_drawable());

        // Geometry with nothing to paint it with, and paint with no geometry,
        // are both nothing to draw.
        let mut unpainted = s.clone();
        unpainted.fill = None;
        assert!(!unpainted.is_drawable());
        assert!(!ShapeLayer::default().is_drawable(), "no path");
    }

    #[test]
    fn a_shape_layer_written_before_fill_and_stroke_loads_with_a_black_fill() {
        // The historical payload: geometry and nothing else. It used to
        // composite to nothing, so defaulting it to a visible black fill can
        // only reveal it, never restyle a shape that was already drawn.
        let s: ShapeLayer = serde_json::from_str(r#"{"path_svg":"M0 0 L4 4 Z"}"#).unwrap();
        assert_eq!(s.path_svg, "M0 0 L4 4 Z");
        assert_eq!(s.fill, Some([0.0, 0.0, 0.0, 1.0]));
        assert!(s.stroke.is_none());
        assert!(s.is_drawable());
    }

    #[test]
    fn a_shape_layers_fill_and_stroke_round_trip() {
        let s = ShapeLayer {
            path_svg: "M0 0 L10 10 Z".into(),
            fill: Some([0.25, 0.5, 0.75, 0.5]),
            fill_rule: ShapeFillRule::EvenOdd,
            stroke: Some(ShapeStroke {
                color: [1.0, 0.0, 0.0, 1.0],
                width_px: 4.5,
                cap: ShapeCap::Round,
                join: ShapeJoin::Bevel,
                miter_limit: 2.0,
                dash: vec![6.0, 3.0],
                dash_offset: 1.5,
            }),
        };
        let layer = Layer::with_kind("Shape", LayerKind::Shape(s.clone()));
        let back: Layer = serde_json::from_str(&serde_json::to_string(&layer).unwrap()).unwrap();
        assert_eq!(back, layer);
        // And every field really participates in equality, so the round trip
        // above is not comparing two defaults.
        for mutate in [
            (|s: &mut ShapeLayer| s.fill = None) as fn(&mut ShapeLayer),
            |s: &mut ShapeLayer| s.fill_rule = ShapeFillRule::NonZero,
            |s: &mut ShapeLayer| s.stroke.as_mut().unwrap().width_px = 1.0,
            |s: &mut ShapeLayer| s.stroke.as_mut().unwrap().cap = ShapeCap::Butt,
            |s: &mut ShapeLayer| s.stroke.as_mut().unwrap().join = ShapeJoin::Miter,
            |s: &mut ShapeLayer| s.stroke.as_mut().unwrap().dash.clear(),
            |s: &mut ShapeLayer| s.stroke.as_mut().unwrap().dash_offset = 0.0,
            |s: &mut ShapeLayer| s.stroke.as_mut().unwrap().miter_limit = 4.0,
        ] {
            let mut other = s.clone();
            mutate(&mut other);
            assert_ne!(other, s);
        }
    }

    #[test]
    fn lock_state_queries() {
        let mut l = LockState::default();
        assert!(!l.any());
        l.position = true;
        assert!(l.any());
        assert!(l.blocks_transform());
        assert!(!l.blocks_pixel_edit());

        let all = LockState {
            all: true,
            ..Default::default()
        };
        assert!(all.blocks_pixel_edit() && all.blocks_transform());
        assert_ne!(all, LockState::default());
    }

    #[test]
    fn mask_resolution() {
        let mut l = Layer::raster("L");
        assert!(l.effective_mask().is_none());
        assert!(l.mask_id().is_none());

        let id = MaskId::new();
        assert!(l.set_mask(LayerMask::new(id)).is_none());
        assert_eq!(l.mask_id(), Some(id));
        assert!(l.effective_mask().is_some());

        // Disabling resolves to "no mask" for the compositor but keeps the id.
        l.mask.as_mut().unwrap().enabled = false;
        assert!(l.effective_mask().is_none());
        assert_eq!(l.mask_id(), Some(id));

        // Replacing returns the old one so a command can restore it on undo.
        l.mask.as_mut().unwrap().enabled = true;
        let old = l.set_mask(LayerMask::new(MaskId::new())).unwrap();
        assert_eq!(old.id, id);
    }

    #[test]
    fn an_unstyled_layer_carries_no_effects_key() {
        let l = Layer::raster("Plain");
        let json = serde_json::to_string(&l).unwrap();
        assert!(
            !json.contains("\"effects\""),
            "styleless layers must cost nothing on disk, got {json}"
        );
        assert_eq!(serde_json::from_str::<Layer>(&json).unwrap(), l);

        // One style is enough to bring the key back.
        let mut styled = l.clone();
        styled.effects.stroke = Some(StrokeEffect::default());
        let json = serde_json::to_string(&styled).unwrap();
        assert!(json.contains("\"effects\""));
        assert_eq!(serde_json::from_str::<Layer>(&json).unwrap(), styled);
    }

    #[test]
    fn effects_survive_a_layer_roundtrip() {
        let mut l = Layer::raster("Styled");
        l.effects.drop_shadow = Some(ShadowEffect {
            distance_px: 12.0,
            ..Default::default()
        });
        l.effects.stroke = Some(StrokeEffect::default());
        let json = serde_json::to_string(&l).unwrap();
        let back: Layer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, l);
        assert_eq!(back.effects.count(), 2);
    }

    #[test]
    fn layers_written_before_effects_and_fill_still_load() {
        // A minimal historical payload: no `effects`, no `fill_opacity`, and a
        // `mask` of null.
        let json = r#"{
            "id": "5f0d1e2c-0000-4000-8000-000000000001",
            "name": "Old",
            "visible": true,
            "locked": {"pixels": false, "position": false, "transparency": false},
            "opacity": 0.5,
            "blend_mode": "Multiply",
            "transform": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "mask": null,
            "clipping": "None",
            "kind": {"Raster": {"source_asset": null}}
        }"#;
        let l: Layer = serde_json::from_str(json).unwrap();
        assert_eq!(l.name, "Old");
        assert_eq!(l.opacity, 0.5);
        assert_eq!(l.fill_opacity, 1.0, "must default to fully filled");
        assert!(l.effects.is_empty());
        assert!(!l.locked.all, "the new lock flag defaults to false");
    }

    #[test]
    fn the_mask_field_is_an_object_now_and_a_bare_id_is_refused() {
        // Pins the wire-format note on `Layer::mask`: a mask is the whole
        // `LayerMask` object, so the old bare-id shape is refused loudly rather
        // than silently dropping the mask (which would lose the user's
        // enabled/inverted/density/feather settings on load).
        let legacy = r#"{
            "id": "5f0d1e2c-0000-4000-8000-000000000002",
            "name": "Masked",
            "visible": true,
            "locked": {},
            "opacity": 1.0,
            "blend_mode": "Normal",
            "transform": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "mask": "5f0d1e2c-0000-4000-8000-0000000000aa",
            "clipping": "None",
            "kind": {"Raster": {"source_asset": null}}
        }"#;
        assert!(
            serde_json::from_str::<Layer>(legacy).is_err(),
            "a bare mask id must not load as a mask"
        );

        // The current shape carries the whole mask across a round trip.
        let mut l = Layer::raster("Masked");
        let mut m = LayerMask::new(MaskId::new());
        m.enabled = false;
        m.inverted = true;
        l.set_mask(m);
        let back: Layer = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back, l);
        assert_eq!(back.mask_id(), l.mask_id());
    }

    #[test]
    fn only_a_group_can_ever_hold_children() {
        // `LayerTree::validate` does not check "a non-group must not claim
        // children" at runtime because it cannot happen: the child list lives
        // in `GroupLayer` and nowhere else. That is a property of these types,
        // so this is where it is pinned — every non-group kind, not just
        // raster.
        let kinds = [
            LayerKind::Raster(RasterLayer::default()),
            LayerKind::Adjustment(AdjustmentLayer {
                kind: AdjustmentKind::Exposure { stops: 1.0 },
            }),
            LayerKind::Text(TextLayer::default()),
            LayerKind::Shape(ShapeLayer::default()),
            LayerKind::SmartObject(SmartObjectLayer {
                asset: AssetId::new(),
                linked: false,
            }),
            LayerKind::Generator(GeneratorLayer {
                provenance_key: "prov".into(),
            }),
        ];
        for kind in kinds {
            let l = Layer::with_kind("L", kind);
            assert!(!l.is_group());
            assert!(
                l.children().is_empty(),
                "{:?} exposed children through a non-group kind",
                l.kind
            );
        }

        // The one kind that can hold them still starts empty.
        let mut g = Layer::group("G");
        assert!(g.is_group());
        assert!(g.children().is_empty());
        if let LayerKind::Group(gr) = &mut g.kind {
            gr.children.push(LayerId::new());
        }
        assert_eq!(g.children().len(), 1, "a group must expose its children");
    }

    #[test]
    fn noop_layers_are_detectable() {
        let mut l = Layer::raster("L");
        l.visible = false;
        assert!(l.is_noop());
        l.visible = true;
        l.opacity = 0.0;
        assert!(l.is_noop());
    }

    #[test]
    fn effective_opacity_repairs_what_the_public_field_cannot_refuse() {
        // The documented range is an expectation, not an invariant: the field
        // accepts anything. The accessor is where the guarantee lives.
        let mut l = Layer::raster("L");
        for (raw, want) in [(5.0f32, 1.0f32), (-2.0, 0.0), (0.4, 0.4)] {
            l.opacity = raw;
            l.fill_opacity = raw;
            assert_eq!(l.effective_opacity(), want, "opacity {raw}");
            assert_eq!(l.effective_fill_opacity(), want, "fill_opacity {raw}");
        }
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            l.opacity = bad;
            l.fill_opacity = bad;
            let (o, f) = (l.effective_opacity(), l.effective_fill_opacity());
            assert!(
                o.is_finite() && (0.0..=1.0).contains(&o),
                "opacity {bad} -> {o}"
            );
            assert!(
                f.is_finite() && (0.0..=1.0).contains(&f),
                "fill {bad} -> {f}"
            );
        }
    }

    #[test]
    fn a_nan_opacity_counts_as_a_noop_rather_than_slipping_through() {
        // `NaN <= 0.0` is false, so a bare field comparison would report this
        // layer as drawable and hand NaN to the compositor.
        let mut l = Layer::raster("L");
        l.opacity = f32::NAN;
        assert!(l.visible);
        assert!(
            l.opacity.partial_cmp(&0.0).is_none(),
            "premise: NaN is incomparable, so a raw `opacity <= 0.0` test misses it"
        );
        assert!(l.is_noop());
    }

    #[test]
    fn an_out_of_range_document_still_loads_and_is_clamped_on_read() {
        // The load is deliberately lenient — refusing it would make one bad
        // number cost the user the whole file. The clamp happens at read.
        let json = r#"{
            "id": "5f0d1e2c-0000-4000-8000-000000000009",
            "name": "Hand edited",
            "visible": true,
            "locked": {},
            "opacity": 5.0,
            "fill_opacity": -1.0,
            "blend_mode": "Normal",
            "transform": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            "mask": null,
            "clipping": "None",
            "kind": {"Raster": {"source_asset": null}}
        }"#;
        let l: Layer = serde_json::from_str(json).unwrap();
        assert_eq!(l.opacity, 5.0, "the stored value is preserved verbatim");
        assert_eq!(l.effective_opacity(), 1.0);
        assert_eq!(l.effective_fill_opacity(), 0.0);
    }
}
