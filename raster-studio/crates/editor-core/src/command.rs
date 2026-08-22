//! Commands: the single, deterministic unit of change.
//!
//! Every user-visible edit is a [`Command`]. A command must be able to:
//! - **apply** itself to a [`Document`],
//! - produce its **inverse** (for undo),
//! - **serialize** (for the on-disk journal and replay),
//! - be **replayed** deterministically.
//!
//! Pixel-heavy payloads (brush strokes, mask tiles, imported assets) are
//! referenced by hash/id rather than embedded, so commands stay small and the
//! journal stays cheap.

use glam::Affine2;
use serde::{Deserialize, Serialize};

use layer_model::{BlendMode, Layer, LayerId};

use crate::document::Document;

/// A patch of optional layer properties. `None` fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayerPatch {
    pub name: Option<String>,
    pub visible: Option<bool>,
    pub opacity: Option<f32>,
    pub blend_mode: Option<BlendMode>,
}

/// The complete, versioned set of editing operations.
///
/// Adding a variant is a format change — bump `DOCUMENT_FORMAT_VERSION` and add
/// a migration if older journals must still replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    CreateLayer {
        layer: Layer,
    },
    DeleteLayer {
        layer_id: LayerId,
    },
    MoveLayer {
        layer_id: LayerId,
        parent: Option<LayerId>,
        index: usize,
    },
    SetLayerProperties {
        layer_id: LayerId,
        patch: LayerPatch,
    },
    TransformLayer {
        layer_id: LayerId,
        /// Post-multiplied onto the layer's current transform.
        matrix: [f32; 6],
    },
    /// A batch of commands applied atomically (import, AI result, flatten...).
    /// Its inverse is the reversed inverses of its members.
    Transaction {
        label: String,
        commands: Vec<Command>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("layer {0} not found")]
    LayerNotFound(LayerId),
    #[error("layer tree error: {0}")]
    Tree(#[from] layer_model::tree::TreeError),
    #[error("command is not invertible without pre-apply capture")]
    NotInvertible,
}

impl Command {
    /// Apply this command to the document, returning the command that would
    /// undo it. Capturing the inverse *during* apply (when we can read the
    /// pre-state) is what makes undo exact.
    pub fn apply(&self, doc: &mut Document) -> Result<Command, CommandError> {
        match self {
            Command::CreateLayer { layer } => {
                let id = layer.id;
                doc.layers.push_root(layer.clone());
                Ok(Command::DeleteLayer { layer_id: id })
            }

            Command::DeleteLayer { layer_id } => {
                // Capture the exact prior position so undo restores it precisely
                // (not just back to the root, as before).
                let (parent, index) = current_location(doc, *layer_id)
                    .ok_or(CommandError::LayerNotFound(*layer_id))?;
                let removed = doc.layers.remove(*layer_id)?;
                // Inverse recreates the layer, then returns it to its prior
                // parent + index via the well-tested MoveLayer command.
                Ok(Command::Transaction {
                    label: "Restore Layer".into(),
                    commands: vec![
                        Command::CreateLayer { layer: removed },
                        Command::MoveLayer {
                            layer_id: *layer_id,
                            parent,
                            index,
                        },
                    ],
                })
            }

            Command::MoveLayer {
                layer_id,
                parent,
                index,
            } => {
                // Capture current location for the inverse.
                let (prev_parent, prev_index) = current_location(doc, *layer_id)
                    .ok_or(CommandError::LayerNotFound(*layer_id))?;
                doc.layers.move_layer(*layer_id, *parent, *index)?;
                Ok(Command::MoveLayer {
                    layer_id: *layer_id,
                    parent: prev_parent,
                    index: prev_index,
                })
            }

            Command::SetLayerProperties { layer_id, patch } => {
                let layer = doc
                    .layers
                    .get_mut(*layer_id)
                    .ok_or(CommandError::LayerNotFound(*layer_id))?;
                // Build the inverse patch from current values before mutating.
                let mut inverse = LayerPatch::default();
                if let Some(v) = &patch.name {
                    inverse.name = Some(layer.name.clone());
                    layer.name = v.clone();
                }
                if let Some(v) = patch.visible {
                    inverse.visible = Some(layer.visible);
                    layer.visible = v;
                }
                if let Some(v) = patch.opacity {
                    inverse.opacity = Some(layer.opacity);
                    layer.opacity = v;
                }
                if let Some(v) = patch.blend_mode {
                    inverse.blend_mode = Some(layer.blend_mode);
                    layer.blend_mode = v;
                }
                Ok(Command::SetLayerProperties {
                    layer_id: *layer_id,
                    patch: inverse,
                })
            }

            Command::TransformLayer { layer_id, matrix } => {
                let layer = doc
                    .layers
                    .get_mut(*layer_id)
                    .ok_or(CommandError::LayerNotFound(*layer_id))?;
                let delta = Affine2::from_cols_array(matrix);
                layer.transform = delta * layer.transform;
                // Inverse applies the inverse affine.
                let inv = delta.inverse();
                Ok(Command::TransformLayer {
                    layer_id: *layer_id,
                    matrix: inv.to_cols_array(),
                })
            }

            Command::Transaction { label, commands } => {
                let mut inverses = Vec::with_capacity(commands.len());
                for c in commands {
                    inverses.push(c.apply(doc)?);
                }
                // Undo in reverse order.
                inverses.reverse();
                Ok(Command::Transaction {
                    label: label.clone(),
                    commands: inverses,
                })
            }
        }
    }

    /// Human-readable label for history UI.
    pub fn label(&self) -> String {
        match self {
            Command::CreateLayer { .. } => "Create Layer".into(),
            Command::DeleteLayer { .. } => "Delete Layer".into(),
            Command::MoveLayer { .. } => "Move Layer".into(),
            Command::SetLayerProperties { .. } => "Change Layer Properties".into(),
            Command::TransformLayer { .. } => "Transform Layer".into(),
            Command::Transaction { label, .. } => label.clone(),
        }
    }
}

/// Find a layer's current parent (None = root) and index within that list.
fn current_location(doc: &Document, id: LayerId) -> Option<(Option<LayerId>, usize)> {
    if let Some(idx) = doc.layers.root().iter().position(|&r| r == id) {
        return Some((None, idx));
    }
    for &pid in &doc.layers.iter_depth_first() {
        if let Some(layer) = doc.layers.get(pid) {
            if let layer_model::LayerKind::Group(g) = &layer.kind {
                if let Some(idx) = g.children.iter().position(|&c| c == id) {
                    return Some((Some(pid), idx));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::Layer;

    #[test]
    fn create_then_undo_removes_layer() {
        let mut doc = Document::new(100, 100, "t");
        let layer = Layer::raster("L1");
        let id = layer.id;
        let inverse = Command::CreateLayer { layer }.apply(&mut doc).unwrap();
        assert!(doc.layers.get(id).is_some());
        inverse.apply(&mut doc).unwrap();
        assert!(doc.layers.get(id).is_none());
    }

    #[test]
    fn set_properties_inverse_restores() {
        let mut doc = Document::new(100, 100, "t");
        let layer = Layer::raster("L1");
        let id = layer.id;
        Command::CreateLayer { layer }.apply(&mut doc).unwrap();

        let patch = LayerPatch {
            opacity: Some(0.25),
            visible: Some(false),
            ..Default::default()
        };
        let inverse = Command::SetLayerProperties {
            layer_id: id,
            patch,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.layers.get(id).unwrap().opacity, 0.25);
        assert!(!doc.layers.get(id).unwrap().visible);

        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc.layers.get(id).unwrap().opacity, 1.0);
        assert!(doc.layers.get(id).unwrap().visible);
    }

    #[test]
    fn transform_inverse_returns_to_identity() {
        let mut doc = Document::new(100, 100, "t");
        let layer = Layer::raster("L1");
        let id = layer.id;
        Command::CreateLayer { layer }.apply(&mut doc).unwrap();

        let translate = Affine2::from_translation(glam::Vec2::new(10.0, 5.0));
        let inverse = Command::TransformLayer {
            layer_id: id,
            matrix: translate.to_cols_array(),
        }
        .apply(&mut doc)
        .unwrap();
        inverse.apply(&mut doc).unwrap();

        let t = doc.layers.get(id).unwrap().transform;
        let diff = (t.translation - glam::Vec2::ZERO).length();
        assert!(diff < 1e-4, "transform did not return to identity: {t:?}");
    }

    #[test]
    fn transaction_undo_in_reverse() {
        let mut doc = Document::new(100, 100, "t");
        let l1 = Layer::raster("A");
        let l2 = Layer::raster("B");
        let (id1, id2) = (l1.id, l2.id);
        let tx = Command::Transaction {
            label: "Import".into(),
            commands: vec![
                Command::CreateLayer { layer: l1 },
                Command::CreateLayer { layer: l2 },
            ],
        };
        let inverse = tx.apply(&mut doc).unwrap();
        assert_eq!(doc.layers.len(), 2);
        inverse.apply(&mut doc).unwrap();
        assert!(doc.layers.get(id1).is_none());
        assert!(doc.layers.get(id2).is_none());
    }

    #[test]
    fn delete_undo_restores_exact_position() {
        let mut doc = Document::new(100, 100, "t");
        let g = Layer::group("G");
        let gid = g.id;
        let target = Layer::raster("Target");
        let tid = target.id;

        Command::CreateLayer { layer: g }.apply(&mut doc).unwrap();
        Command::CreateLayer {
            layer: Layer::raster("Base"),
        }
        .apply(&mut doc)
        .unwrap();
        Command::CreateLayer { layer: target }.apply(&mut doc).unwrap();

        // Park the target inside the group at a known position.
        Command::MoveLayer {
            layer_id: tid,
            parent: Some(gid),
            index: 0,
        }
        .apply(&mut doc)
        .unwrap();

        let before: Vec<LayerId> = match &doc.layers.get(gid).unwrap().kind {
            layer_model::LayerKind::Group(gr) => gr.children.clone(),
            _ => unreachable!(),
        };
        assert!(before.contains(&tid));

        let inverse = Command::DeleteLayer { layer_id: tid }
            .apply(&mut doc)
            .unwrap();
        assert!(doc.layers.get(tid).is_none());

        // Undo must restore the layer to its exact prior parent + index, not
        // merely push it back to the root.
        inverse.apply(&mut doc).unwrap();
        assert!(doc.layers.get(tid).is_some());
        let after: Vec<LayerId> = match &doc.layers.get(gid).unwrap().kind {
            layer_model::LayerKind::Group(gr) => gr.children.clone(),
            _ => unreachable!(),
        };
        assert_eq!(after, before);
        assert!(after.contains(&tid));
    }

    /// Every [`Command`] variant must serialize and deserialize losslessly so
    /// the on-disk journal can be replayed after a crash. Verified structurally:
    /// serializing the deserialized value yields the exact same JSON.
    fn json_roundtrip(cmd: &Command) {
        let json = serde_json::to_string(cmd).expect("serialize");
        let back: Command = serde_json::from_str(&json).expect("deserialize");
        let json2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(json, json2, "command did not round-trip losslessly");
    }

    #[test]
    fn command_variants_serde_roundtrip() {
        let layer = Layer::raster("L");
        let id = layer.id;

        json_roundtrip(&Command::CreateLayer { layer: layer.clone() });
        json_roundtrip(&Command::DeleteLayer { layer_id: id });
        json_roundtrip(&Command::MoveLayer {
            layer_id: id,
            parent: None,
            index: 2,
        });
        json_roundtrip(&Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                opacity: Some(0.5),
                visible: Some(false),
                ..Default::default()
            },
        });
        json_roundtrip(&Command::TransformLayer {
            layer_id: id,
            matrix: [1.0, 0.0, 0.0, 1.0, 10.0, -5.0],
        });
        json_roundtrip(&Command::Transaction {
            label: "Import".into(),
            commands: vec![
                Command::CreateLayer { layer: layer.clone() },
                Command::DeleteLayer { layer_id: id },
            ],
        });
    }
}
