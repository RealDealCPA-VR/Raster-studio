//! The layer tree: layers, groups, masks, blend state, and identity types.
//!
//! This crate is deliberately free of rendering and persistence concerns. It
//! describes *what* a document contains; `render` decides how to composite it
//! and `project-format` decides how to serialize it.

pub mod blend;
pub mod ids;
pub mod layer;
pub mod tree;

pub use blend::BlendMode;
pub use ids::{AssetId, LayerId, MaskId};
pub use layer::{
    AdjustmentKind, AdjustmentLayer, ClippingMode, GeneratorLayer, GroupLayer, Layer, LayerKind,
    LockState, RasterLayer, ShapeLayer, SmartObjectLayer, TextLayer,
};
pub use tree::LayerTree;
