//! The layer tree: layers, groups, masks, blend state, effects, and identity
//! types.
//!
//! This crate is deliberately free of rendering and persistence concerns. It
//! describes *what* a document contains; `render` decides how to composite it
//! and `project-format` decides how to serialize it.
//!
//! The four things a compositor needs from this crate beyond the raw layer
//! list are [`BlendMode::blend_rgb`] (the reference blend math for all 27
//! modes), [`Layer::effective_mask`] (a mask resolved to something applicable),
//! [`LayerTree::clipping_group`] (which layers clip to which base), and
//! [`Layer::effective_opacity`] (see below).
//!
//! # Numeric ranges
//!
//! Two kinds of numeric field live here, and the doc comments distinguish them
//! deliberately:
//!
//! * **Enforced.** [`LayerMask`]'s `density` and `feather_px` are private,
//!   reachable only through clamping setters, and re-validated on every
//!   deserialize via `#[serde(try_from)]`. Their documented ranges are
//!   invariants a caller may rely on.
//! * **Expected.** Every other range in this crate — [`Layer::opacity`],
//!   [`Layer::fill_opacity`], every effect parameter in [`effects`], every
//!   [`Rgba`] component — is documented as an *expectation*, because the field
//!   is public `f32` and nothing rejects an out-of-range or non-finite write.
//!   A hand-edited document, a binary format, or a future tool can put `5.0`
//!   or a NaN there.
//!
//! Consumers of the "expected" group must clamp on read.
//! [`blend::unit`] is the shared clamp (finite values are clamped, non-finite
//! becomes `0.0`), and [`Layer::effective_opacity`] /
//! [`Layer::effective_fill_opacity`] apply it for the two fields the
//! compositor multiplies on every pixel.

pub mod artboard;
pub mod blend;
// W10-B: layer comps, notes, text styles and the alpha-channel edit.
pub mod doc_extras;
pub mod effects;
pub mod fill;
pub mod ids;
pub mod layer;
pub mod mask;
pub mod smart_filter;
pub mod text;
pub mod tree;

pub use artboard::Artboard;
pub use blend::{dissolve_keeps_source, BlendMode};
pub use effects::{
    BevelDirection, BevelEffect, BevelStyle, BevelTechnique, ColorOverlayEffect, FillStyle,
    GlowEffect, GlowSource, GlowTechnique, Gradient, GradientOverlayEffect, GradientStop,
    GradientStyle, LayerEffects, PatternFill, PatternOverlayEffect, PatternTile, PatternTileError,
    Rgba, SatinEffect, ShadowEffect, StrokeEffect, StrokePosition, MAX_PATTERN_EDGE,
    MAX_PATTERN_PIXELS,
};
// W9-B: live fill layers.
pub use fill::{FillLayer, FillSource, GradientFill};
pub use ids::{AssetId, LayerId, MaskId};
pub use layer::{
    AdjustmentKind, AdjustmentLayer, AssetOrigin, AssetRecord, AutoAdjustment, ClippingMode,
    GeneratorLayer, GroupBlending, GroupLayer, Layer, LayerKind, LockState, RasterLayer, ShapeCap,
    ShapeFillRule, ShapeJoin, ShapeLayer, ShapeStroke, SmartObjectLayer, TextLayer,
};
// W9-F: shape fill paint (gradient / pattern) and stroke alignment.
pub use layer::{ShapeFill, ShapeFillPaint, ShapeGradientFill, ShapeStrokeAlign};
pub use mask::{LayerMask, MaskError, MaskKind, VectorMask};
pub use smart_filter::{stack_is_active, SmartFilter, SmartParam};
pub use tree::{ClippingGroup, DetachedSubtree, LayerTree, TreeError};
// W10-B: the document-level records of the Layer Comps, Notes and style panels.
pub use doc_extras::{
    AlphaEdit, CharacterStyle, CompLayerState, DocumentExtras, LayerComp, Note, ParagraphStyle,
    StyleLink,
};
