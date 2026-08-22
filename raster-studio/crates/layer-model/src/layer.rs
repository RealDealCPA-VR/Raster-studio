//! Layer types. Mirrors the design doc's `LayerKind`/`Layer` shape.

use glam::Affine2;
use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::ids::{AssetId, LayerId, MaskId};

/// The variant-specific payload of a layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayerKind {
    Raster(RasterLayer),
    Group(GroupLayer),
    Adjustment(AdjustmentLayer),
    Text(TextLayer),
    Shape(ShapeLayer),
    SmartObject(SmartObjectLayer),
    Generator(GeneratorLayer),
}

/// Common properties shared by every layer regardless of kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub locked: LockState,
    /// 0.0..=1.0
    pub opacity: f32,
    pub blend_mode: BlendMode,
    /// Layer-to-document affine transform (non-destructive).
    #[serde(with = "affine2_serde")]
    pub transform: Affine2,
    pub mask: Option<MaskId>,
    pub clipping: ClippingMode,
    pub kind: LayerKind,
}

impl Layer {
    /// Construct a raster layer with sensible defaults.
    pub fn raster(name: impl Into<String>) -> Self {
        Self {
            id: LayerId::new(),
            name: name.into(),
            visible: true,
            locked: LockState::default(),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: Affine2::IDENTITY,
            mask: None,
            clipping: ClippingMode::None,
            kind: LayerKind::Raster(RasterLayer::default()),
        }
    }

    /// Construct an empty group.
    pub fn group(name: impl Into<String>) -> Self {
        Self {
            id: LayerId::new(),
            name: name.into(),
            visible: true,
            locked: LockState::default(),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: Affine2::IDENTITY,
            mask: None,
            clipping: ClippingMode::None,
            kind: LayerKind::Group(GroupLayer {
                children: Vec::new(),
            }),
        }
    }

    pub fn is_group(&self) -> bool {
        matches!(self.kind, LayerKind::Group(_))
    }
}

/// Lock flags. Any subset can be engaged independently.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct LockState {
    pub pixels: bool,
    pub position: bool,
    pub transparency: bool,
}

/// Clipping-mask behavior relative to the layer directly below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ClippingMode {
    #[default]
    None,
    /// Clip to the alpha of the layer beneath.
    ClipToBelow,
}

/// Pixel content lives as tiles owned by the asset/tile store; a raster layer
/// references them indirectly. Kept minimal in the scaffold.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RasterLayer {
    /// Optional origin asset this raster was imported from (for provenance).
    pub source_asset: Option<AssetId>,
}

/// A group owns an ordered list of child layer ids (top-most first).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupLayer {
    pub children: Vec<LayerId>,
}

/// A non-destructive adjustment applied to everything beneath it (or clipped).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdjustmentLayer {
    pub kind: AdjustmentKind,
}

/// The parametric adjustments available in Phase 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AdjustmentKind {
    Levels {
        black: f32,
        white: f32,
        gamma: f32,
    },
    Curves {
        points: Vec<[f32; 2]>,
    },
    Exposure {
        stops: f32,
    },
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    ColorBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
    },
}

/// Editable text layer. Postponed (Phase 3); shape reserved so the enum and
/// serialization are forward-compatible.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TextLayer {
    pub text: String,
    pub font_family: String,
    pub size_px: f32,
}

/// Vector shape layer (Phase 3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShapeLayer {
    /// Placeholder path representation; a real path model lands with the pen tool.
    pub path_svg: String,
}

/// Embedded or linked document rendered non-destructively (Phase 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartObjectLayer {
    pub asset: AssetId,
    pub linked: bool,
}

/// A generator layer whose pixels are produced by an AI operation. Carries a
/// reference to recorded provenance so the result stays reproducible.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    #[test]
    fn raster_layer_defaults() {
        let l = Layer::raster("Background");
        assert_eq!(l.opacity, 1.0);
        assert!(l.visible);
        assert!(matches!(l.kind, LayerKind::Raster(_)));
    }

    #[test]
    fn layer_serde_roundtrip() {
        let l = Layer::group("Group 1");
        let json = serde_json::to_string(&l).unwrap();
        let back: Layer = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, l.id);
        assert!(back.is_group());
    }
}
