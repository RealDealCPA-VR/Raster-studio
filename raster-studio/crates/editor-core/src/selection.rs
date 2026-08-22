//! Selections. v1 supports rectangular selections; the enum leaves room for
//! lasso/path/mask-backed selections without changing consumers.

use glam::IVec2;
use serde::{Deserialize, Serialize};

/// The active pixel selection, if any.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Selection {
    /// Nothing selected — operations apply to the whole active layer.
    #[default]
    None,
    /// Axis-aligned rectangle in document pixel space.
    Rect { min: IVec2, max: IVec2 },
    /// A selection backed by a mask asset (lasso/wand/refine-edge results).
    Mask { asset_hash: String },
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        matches!(self, Selection::None)
    }

    /// Bounding box in pixel space, if the selection is bounded.
    pub fn bounds(&self) -> Option<(IVec2, IVec2)> {
        match self {
            Selection::Rect { min, max } => Some((*min, *max)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_bounds() {
        let s = Selection::Rect {
            min: IVec2::new(1, 2),
            max: IVec2::new(10, 20),
        };
        assert_eq!(s.bounds(), Some((IVec2::new(1, 2), IVec2::new(10, 20))));
        assert!(!s.is_empty());
    }
}
