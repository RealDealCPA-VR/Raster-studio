//! The layer tree container: a flat id->Layer map plus an ordered root list.
//!
//! A flat map (rather than nested ownership) keeps ids stable, makes command
//! apply/undo cheap, and avoids borrow-checker fights when mutating one layer
//! while reading another. Group membership is expressed via
//! [`GroupLayer::children`] and the root order list.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ids::LayerId;
use crate::layer::{Layer, LayerKind};

/// Owns all layers in a document and their z-order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayerTree {
    layers: HashMap<LayerId, Layer>,
    /// Top-level layer ids, top-most first.
    root: Vec<LayerId>,
}

#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("layer {0} not found")]
    NotFound(LayerId),
    #[error("parent {0} is not a group")]
    NotAGroup(LayerId),
}

impl LayerTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn get(&self, id: LayerId) -> Option<&Layer> {
        self.layers.get(&id)
    }

    pub fn get_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        self.layers.get_mut(&id)
    }

    /// Top-level ids in z-order (top-most first).
    pub fn root(&self) -> &[LayerId] {
        &self.root
    }

    /// Insert a layer at the top of the root list. Returns its id.
    pub fn push_root(&mut self, layer: Layer) -> LayerId {
        let id = layer.id;
        self.layers.insert(id, layer);
        self.root.insert(0, id);
        id
    }

    /// Remove a layer and detach it from root or any parent group.
    /// Returns the removed layer.
    pub fn remove(&mut self, id: LayerId) -> Result<Layer, TreeError> {
        let layer = self.layers.remove(&id).ok_or(TreeError::NotFound(id))?;
        self.root.retain(|&r| r != id);
        for l in self.layers.values_mut() {
            if let LayerKind::Group(g) = &mut l.kind {
                g.children.retain(|&c| c != id);
            }
        }
        Ok(layer)
    }

    /// Re-parent `id` into `parent` group at `index` (or root if `parent` is
    /// `None`). Detaches from its current location first.
    pub fn move_layer(
        &mut self,
        id: LayerId,
        parent: Option<LayerId>,
        index: usize,
    ) -> Result<(), TreeError> {
        if !self.layers.contains_key(&id) {
            return Err(TreeError::NotFound(id));
        }
        // Detach from wherever it currently lives.
        self.root.retain(|&r| r != id);
        for l in self.layers.values_mut() {
            if let LayerKind::Group(g) = &mut l.kind {
                g.children.retain(|&c| c != id);
            }
        }
        // Attach to the new location.
        match parent {
            None => {
                let idx = index.min(self.root.len());
                self.root.insert(idx, id);
            }
            Some(pid) => {
                let p = self.layers.get_mut(&pid).ok_or(TreeError::NotFound(pid))?;
                match &mut p.kind {
                    LayerKind::Group(g) => {
                        let idx = index.min(g.children.len());
                        g.children.insert(idx, id);
                    }
                    _ => return Err(TreeError::NotAGroup(pid)),
                }
            }
        }
        Ok(())
    }

    /// Depth-first iteration of ids in composite order (root order, recursing
    /// into groups). Useful for the render graph walk.
    pub fn iter_depth_first(&self) -> Vec<LayerId> {
        let mut out = Vec::with_capacity(self.layers.len());
        for &id in &self.root {
            self.push_recursive(id, &mut out);
        }
        out
    }

    fn push_recursive(&self, id: LayerId, out: &mut Vec<LayerId>) {
        out.push(id);
        if let Some(Layer {
            kind: LayerKind::Group(g),
            ..
        }) = self.layers.get(&id)
        {
            for &c in &g.children {
                self.push_recursive(c, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::Layer;

    #[test]
    fn push_and_get() {
        let mut t = LayerTree::new();
        let id = t.push_root(Layer::raster("L1"));
        assert_eq!(t.len(), 1);
        assert_eq!(t.get(id).unwrap().name, "L1");
        assert_eq!(t.root(), &[id]);
    }

    #[test]
    fn move_into_group_and_back() {
        let mut t = LayerTree::new();
        let g = t.push_root(Layer::group("G"));
        let l = t.push_root(Layer::raster("L"));
        t.move_layer(l, Some(g), 0).unwrap();

        let order = t.iter_depth_first();
        // Root has [l? no—moved], so depth-first from root [g? or l?]
        assert!(order.contains(&g) && order.contains(&l));
        // l should appear immediately after g (its only child).
        let gi = order.iter().position(|&x| x == g).unwrap();
        assert_eq!(order[gi + 1], l);

        // Move back to root.
        t.move_layer(l, None, 0).unwrap();
        assert!(t.root().contains(&l));
    }

    #[test]
    fn remove_detaches_from_group() {
        let mut t = LayerTree::new();
        let g = t.push_root(Layer::group("G"));
        let l = t.push_root(Layer::raster("L"));
        t.move_layer(l, Some(g), 0).unwrap();
        t.remove(l).unwrap();
        assert!(t.get(l).is_none());
        assert_eq!(t.len(), 1);
    }
}
