//! Which part of the canvas an edit invalidated.
//!
//! The presenter holds a document-sized GPU texture. Re-uploading all of it
//! after every brush dab would move a 4K image across the bus sixty times a
//! second, so an edit records the tiles it touched and only those are
//! recomposited and re-uploaded.
//!
//! This is an *optimisation over a correct baseline*, in two senses. The
//! compositor's own cache is keyed by the inputs that produced each tile, so a
//! tile this module fails to mention still recomposites correctly the moment
//! its key changes — a missed coordinate costs a key computation, never a wrong
//! pixel. And every command whose reach is not a tile list (a layer property
//! change, a re-order, a delete) reports [`DirtyTiles::everything`] rather than
//! guessing, so being wrong here is expensive, not incorrect.

use std::collections::BTreeSet;

use editor_core::Command;
use raster::TileCoord;

/// The set of tiles that need recompositing, or "all of them".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirtyTiles {
    all: bool,
    tiles: BTreeSet<TileCoord>,
}

impl DirtyTiles {
    /// Nothing to redraw.
    pub fn none() -> Self {
        Self::default()
    }

    /// Everything to redraw.
    pub fn all() -> Self {
        DirtyTiles {
            all: true,
            tiles: BTreeSet::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.all && self.tiles.is_empty()
    }

    /// `true` when the whole canvas has to be recomposited.
    pub fn is_all(&self) -> bool {
        self.all
    }

    /// The individual tiles, meaningful only while [`DirtyTiles::is_all`] is
    /// false.
    pub fn tiles(&self) -> impl Iterator<Item = TileCoord> + '_ {
        self.tiles.iter().copied()
    }

    pub fn insert(&mut self, coord: TileCoord) {
        if !self.all {
            self.tiles.insert(coord);
        }
    }

    pub fn mark_all(&mut self) {
        self.all = true;
        // The list is meaningless now, and keeping it would let a later
        // `merge` of a tile set silently un-mark the whole canvas.
        self.tiles.clear();
    }

    /// Fold `other` in.
    pub fn merge(&mut self, other: &DirtyTiles) {
        if other.all {
            self.mark_all();
        } else if !self.all {
            self.tiles.extend(other.tiles.iter().copied());
        }
    }

    /// Take the accumulated set, leaving nothing behind.
    pub fn take(&mut self) -> DirtyTiles {
        std::mem::take(self)
    }

    /// Record what `command` invalidated.
    pub fn record(&mut self, command: &Command) {
        self.merge(&touched_by(command));
    }
}

/// What `command` invalidated.
///
/// Matched exhaustively with no wildcard: a new [`Command`] variant must state
/// its reach here rather than defaulting into whichever arm happened to be
/// last.
pub fn touched_by(command: &Command) -> DirtyTiles {
    let from_delta = |delta: &editor_core::TileDelta| {
        let mut out = DirtyTiles::none();
        for edit in delta.iter() {
            // A mip-level edit changes what a *zoomed-out* view samples, and
            // the presenter's texture is level 0, so only level 0 maps to a
            // rectangle it can re-upload. Anything else invalidates wholesale.
            if edit.coord.level == 0 {
                out.insert(edit.coord);
            } else {
                out.mark_all();
                break;
            }
        }
        out
    };
    match command {
        Command::PaintTiles { delta, .. }
        | Command::FillRegion { delta, .. }
        | Command::ClearRegion { delta, .. } => from_delta(delta),
        // Guides are alignment state, not pixels: changing them never dirties
        // a tile of the composite.
        Command::SetGuides { .. } => DirtyTiles::none(),
        Command::Transaction { commands, .. } => {
            let mut out = DirtyTiles::none();
            for c in commands {
                out.merge(&touched_by(c));
                if out.is_all() {
                    break;
                }
            }
            out
        }
        // Everything below changes how existing pixels are *composited* rather
        // than which pixels exist, and the reach is the whole layer (or the
        // whole stack beneath it, for a re-ordered adjustment). There is no
        // cheaper honest answer without walking the layer's tile map, which is
        // what `mark_all` stands in for.
        Command::CreateLayer { .. }
        | Command::DeleteLayer { .. }
        | Command::RestoreLayers { .. }
        | Command::MoveLayer { .. }
        | Command::SetLayerProperties { .. }
        // An adjustment layer's parameters reach every pixel under it, so the
        // canvas has to be re-composited whole. Answering `none` here is what
        // would make a Brightness slider move a value nothing repainted.
        | Command::SetLayerKind { .. }
        // A crop changes which document pixels exist at all, so every tile of
        // the new canvas is new. The presenter rebuilds its texture on a size
        // change anyway; saying `all` here is what keeps a resize that happens
        // to leave the size alone (an undo of a no-op crop) honest.
        | Command::SetCanvasSize { .. }
        | Command::TransformLayer { .. } => DirtyTiles::all(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::pixels::{PixelTarget, TileDelta, TileEdit};
    use layer_model::{Layer, LayerId};
    use raster::TileHash;

    fn paint(coords: &[TileCoord]) -> Command {
        Command::PaintTiles {
            target: PixelTarget::Layer(LayerId::new()),
            delta: TileDelta::new(
                coords
                    .iter()
                    .map(|c| TileEdit::set(*c, TileHash([1; 32])))
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        }
    }

    #[test]
    fn a_paint_marks_exactly_the_tiles_it_touched() {
        let a = TileCoord::new(0, 0, 0);
        let b = TileCoord::new(3, 2, 0);
        let d = touched_by(&paint(&[a, b]));
        assert!(!d.is_all());
        assert_eq!(d.tiles().collect::<Vec<_>>(), vec![a, b]);
        assert!(!d.is_empty());
    }

    #[test]
    fn a_mip_level_edit_invalidates_everything() {
        let d = touched_by(&paint(&[TileCoord::new(0, 0, 1)]));
        assert!(
            d.is_all(),
            "level 1 is not a rectangle of the level-0 image"
        );
    }

    #[test]
    fn a_structural_change_invalidates_everything() {
        for cmd in [
            Command::create_layer(Layer::raster("L")),
            Command::DeleteLayer {
                layer_id: LayerId::new(),
            },
            Command::MoveLayer {
                layer_id: LayerId::new(),
                parent: None,
                index: 0,
            },
            Command::SetLayerProperties {
                layer_id: LayerId::new(),
                patch: editor_core::LayerPatch {
                    visible: Some(false),
                    ..Default::default()
                },
            },
            Command::TransformLayer {
                layer_id: LayerId::new(),
                matrix: [1.0, 0.0, 0.0, 1.0, 5.0, 0.0],
            },
            // An adjustment layer's parameters reach every pixel beneath it,
            // so the whole canvas has to be re-uploaded. `none` here would be a
            // Brightness slider that moves and repaints nothing.
            Command::SetLayerKind {
                layer_id: LayerId::new(),
                kind: Box::new(layer_model::LayerKind::Adjustment(
                    layer_model::AdjustmentLayer {
                        kind: layer_model::AdjustmentKind::Invert,
                    },
                )),
            },
        ] {
            assert!(touched_by(&cmd).is_all(), "{cmd:?} must invalidate all");
        }
    }

    #[test]
    fn a_transaction_is_the_union_of_its_members() {
        let a = TileCoord::new(1, 1, 0);
        let b = TileCoord::new(2, 2, 0);
        let tx = Command::Transaction {
            label: "two paints".into(),
            commands: vec![paint(&[a]), paint(&[b])],
        };
        let d = touched_by(&tx);
        assert!(!d.is_all());
        assert_eq!(d.tiles().collect::<Vec<_>>(), vec![a, b]);

        // ...and one member that invalidates everything wins.
        let tx = Command::Transaction {
            label: "import".into(),
            commands: vec![Command::create_layer(Layer::raster("L")), paint(&[a])],
        };
        assert!(touched_by(&tx).is_all());
    }

    #[test]
    fn marking_all_cannot_be_undone_by_a_later_tile() {
        let mut d = DirtyTiles::none();
        d.record(&Command::create_layer(Layer::raster("L")));
        assert!(d.is_all());
        d.record(&paint(&[TileCoord::new(0, 0, 0)]));
        assert!(d.is_all(), "a tile set must not narrow a full invalidation");
        assert_eq!(d.tiles().count(), 0);
    }

    #[test]
    fn taking_the_set_leaves_it_empty() {
        let mut d = DirtyTiles::none();
        d.record(&paint(&[TileCoord::new(0, 0, 0)]));
        let taken = d.take();
        assert_eq!(taken.tiles().count(), 1);
        assert!(d.is_empty(), "the accumulator resets");
        assert!(!d.is_all());
    }
}
