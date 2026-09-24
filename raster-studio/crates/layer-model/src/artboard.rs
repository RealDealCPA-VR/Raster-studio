//! W7-F: artboards — Photopea's group-like layer with its own canvas rect and
//! background.
//!
//! # The shape on disk
//!
//! An artboard is a [`crate::LayerKind::Group`] whose bottom-most child is a
//! raster **background plate**: a [`crate::RasterLayer`] carrying
//! [`crate::RasterLayer::artboard`]. The plate names the artboard's rect and
//! background colour, and its pixels are that colour over that rect, so the
//! artboard is visible — and composites, exports and round-trips through PSD
//! — through the ordinary group and raster paths with no new layer kind.
//! The field is appended to `RasterLayer` and omitted while `None`, so a
//! document from before artboards opens unchanged.
//!
//! # Clipping and export (W8-C)
//!
//! As in Photopea, an artboard's contents are clipped to its rect: the
//! compositor clips the artboard group's children to the plate's rect (see
//! `compositor::composite`), so nothing drawn inside the group shows outside
//! it. File > Export > Artboards to Files enumerates the artboards through
//! [`artboards`] and writes one image per artboard
//! (`app_shell::artboard_export`).

use serde::{Deserialize, Serialize};

use crate::{LayerId, LayerKind, LayerTree};

/// One artboard's canvas: a document-space rect and the background it is
/// filled with.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Artboard {
    /// Left edge, document pixels.
    pub x: i64,
    /// Top edge, document pixels.
    pub y: i64,
    /// Width in pixels (at least 1).
    pub width: u32,
    /// Height in pixels (at least 1).
    pub height: u32,
    /// Background, straight-alpha linear RGBA; alpha 0 is a transparent
    /// artboard.
    pub background: [f32; 4],
}

impl Artboard {
    /// `true` when the rect has area and every number is finite.
    pub fn is_valid(&self) -> bool {
        self.width > 0 && self.height > 0 && self.background.iter().all(|v| v.is_finite())
    }
}

/// The artboard `group` is, when it is one: its background plate and the
/// artboard that plate names.
pub fn artboard_of(tree: &LayerTree, group: LayerId) -> Option<(LayerId, Artboard)> {
    let LayerKind::Group(g) = &tree.get(group)?.kind else {
        return None;
    };
    // Children are listed top-most first; the plate sits at the bottom.
    g.children.iter().rev().find_map(|child| {
        let LayerKind::Raster(r) = &tree.get(*child)?.kind else {
            return None;
        };
        r.artboard.map(|a| (*child, a))
    })
}

/// Every artboard in the document, as `(group, artboard)`, in depth-first
/// (panel) order.
pub fn artboards(tree: &LayerTree) -> Vec<(LayerId, Artboard)> {
    tree.iter_depth_first()
        .into_iter()
        .filter_map(|id| artboard_of(tree, id).map(|(_, a)| (id, a)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Layer, RasterLayer};

    fn plate(a: Artboard) -> Layer {
        Layer::with_kind(
            "Background",
            LayerKind::Raster(RasterLayer {
                artboard: Some(a),
                ..RasterLayer::default()
            }),
        )
    }

    const BOARD: Artboard = Artboard {
        x: 10,
        y: 20,
        width: 30,
        height: 40,
        background: [1.0, 1.0, 1.0, 1.0],
    };

    #[test]
    fn a_group_with_a_plate_is_an_artboard_and_a_plain_group_is_not() {
        let mut tree = LayerTree::default();
        let group = tree.push_root(Layer::group("Artboard 1")).unwrap();
        let plain = tree.push_root(Layer::group("Group")).unwrap();
        let bg = tree.push_root(plate(BOARD)).unwrap();
        tree.move_layer(bg, Some(group), 0).unwrap();
        assert_eq!(artboard_of(&tree, group), Some((bg, BOARD)));
        assert_eq!(artboard_of(&tree, plain), None);
        assert_eq!(artboards(&tree), vec![(group, BOARD)]);
    }

    #[test]
    fn a_raster_without_the_field_serialises_as_before_and_old_json_loads() {
        let plain = RasterLayer::default();
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("artboard"), "{json}");
        let old: RasterLayer = serde_json::from_str(r#"{"source_asset":null}"#).unwrap();
        assert_eq!(old, RasterLayer::default());
        let with = RasterLayer {
            artboard: Some(BOARD),
            ..RasterLayer::default()
        };
        let back: RasterLayer =
            serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
        assert_eq!(back, with);
        assert!(BOARD.is_valid());
    }
}
