//! W10-A: independent link groups.
//!
//! Photopea's Layer ▸ Link Layers links the selected layers into a group of
//! their own; linking another selection makes another group, and moving one
//! member moves its own group only. [`Layer::link_group`] is that group's id.
//!
//! Documents written before groups existed carry only the single-chain
//! [`Layer::linked`] flag. [`Layer::link_key`] reads such a layer as a member
//! of the one group [`LEGACY_LINK_GROUP`], so an old document's chain keeps
//! moving together exactly as it did, and fresh ids
//! ([`LayerTree::next_link_group`]) never collide with it.

use crate::ids::LayerId;
use crate::layer::Layer;
use crate::tree::LayerTree;

/// The group an old document's single `linked` chain maps to.
pub const LEGACY_LINK_GROUP: u64 = 0;

impl Layer {
    /// The link group this layer moves with, or `None` when it is not linked.
    ///
    /// The `linked` flag is the authority on *whether* a layer is linked (it
    /// is what the Layers panel's badge shows and what its link button
    /// toggles); [`Layer::link_group`] only says *which* group. So a layer
    /// whose flag is off links nothing even if it still remembers a group,
    /// and a linked layer with no group id (an old document's single chain)
    /// is in [`LEGACY_LINK_GROUP`].
    pub fn link_key(&self) -> Option<u64> {
        self.linked
            .then(|| self.link_group.unwrap_or(LEGACY_LINK_GROUP))
    }
}

impl LayerTree {
    /// A group id no layer of this tree uses (and never
    /// [`LEGACY_LINK_GROUP`]): one past the largest in use.
    pub fn next_link_group(&self) -> u64 {
        self.iter_depth_first()
            .into_iter()
            .filter_map(|id| self.get(id)?.link_group)
            .max()
            .map_or(LEGACY_LINK_GROUP + 1, |m| {
                m.saturating_add(1).max(LEGACY_LINK_GROUP + 1)
            })
    }

    /// Every layer, depth-first, that shares a link group with any of
    /// `participants` — the participants themselves included when they are
    /// linked. A layer in no group links nothing, so an unlinked selection
    /// answers an empty list.
    pub fn link_partners(&self, participants: &[LayerId]) -> Vec<LayerId> {
        let groups: Vec<u64> = participants
            .iter()
            .filter_map(|id| self.get(*id)?.link_key())
            .collect();
        if groups.is_empty() {
            return Vec::new();
        }
        self.iter_depth_first()
            .into_iter()
            .filter(|id| {
                self.get(*id)
                    .and_then(Layer::link_key)
                    .is_some_and(|g| groups.contains(&g))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_of(n: usize) -> (LayerTree, Vec<LayerId>) {
        let mut tree = LayerTree::default();
        let mut ids = Vec::new();
        for i in 0..n {
            let layer = Layer::raster(format!("L{i}"));
            ids.push(layer.id);
            tree.push_root(layer).unwrap();
        }
        (tree, ids)
    }

    #[test]
    fn two_groups_are_independent_and_an_unlinked_layer_links_nothing() {
        let (mut tree, ids) = tree_of(5);
        let g1 = tree.next_link_group();
        assert_ne!(g1, LEGACY_LINK_GROUP);
        for id in &ids[0..2] {
            let layer = tree.get_mut(*id).unwrap();
            layer.linked = true;
            layer.link_group = Some(g1);
        }
        let g2 = tree.next_link_group();
        assert_ne!(g1, g2);
        for id in &ids[2..4] {
            let layer = tree.get_mut(*id).unwrap();
            layer.linked = true;
            layer.link_group = Some(g2);
        }
        let mut a = tree.link_partners(&[ids[0]]);
        a.sort_by_key(|i| i.0);
        let mut want = ids[0..2].to_vec();
        want.sort_by_key(|i| i.0);
        assert_eq!(a, want, "group one only");
        let mut b = tree.link_partners(&[ids[3]]);
        b.sort_by_key(|i| i.0);
        let mut want = ids[2..4].to_vec();
        want.sort_by_key(|i| i.0);
        assert_eq!(b, want, "group two only");
        assert!(tree.link_partners(&[ids[4]]).is_empty());
    }

    #[test]
    fn an_old_documents_linked_flag_is_one_group() {
        let (mut tree, ids) = tree_of(3);
        tree.get_mut(ids[0]).unwrap().linked = true;
        tree.get_mut(ids[2]).unwrap().linked = true;
        assert_eq!(
            tree.get(ids[0]).unwrap().link_key(),
            Some(LEGACY_LINK_GROUP)
        );
        let mut got = tree.link_partners(&[ids[2]]);
        got.sort_by_key(|i| i.0);
        let mut want = vec![ids[0], ids[2]];
        want.sort_by_key(|i| i.0);
        assert_eq!(got, want);
        // A fresh group never reuses the legacy id.
        assert_eq!(tree.next_link_group(), LEGACY_LINK_GROUP + 1);
    }

    #[test]
    fn clearing_the_linked_flag_unlinks_even_when_a_group_id_remains() {
        // The Layers panel's link button unlinks by patching `linked` alone.
        let (mut tree, ids) = tree_of(2);
        let g = tree.next_link_group();
        for id in &ids {
            let layer = tree.get_mut(*id).unwrap();
            layer.linked = true;
            layer.link_group = Some(g);
        }
        assert_eq!(tree.link_partners(&[ids[0]]).len(), 2);
        tree.get_mut(ids[0]).unwrap().linked = false;
        tree.get_mut(ids[1]).unwrap().linked = false;
        assert_eq!(tree.get(ids[0]).unwrap().link_key(), None);
        assert!(
            tree.link_partners(&[ids[0]]).is_empty(),
            "a layer the panel unlinked still moves with its old group"
        );
    }

    #[test]
    fn the_group_id_round_trips_and_old_json_reads_as_none() {
        let mut layer = Layer::raster("a");
        let plain = serde_json::to_string(&layer).unwrap();
        assert!(!plain.contains("link_group"), "skipped while None");
        layer.link_group = Some(7);
        let json = serde_json::to_string(&layer).unwrap();
        let back: Layer = serde_json::from_str(&json).unwrap();
        assert_eq!(back.link_group, Some(7));
        let old: Layer =
            serde_json::from_str(&plain.replace("\"linked\":false", "\"linked\":true")).unwrap();
        assert_eq!(old.link_group, None);
        assert_eq!(old.link_key(), Some(LEGACY_LINK_GROUP));
    }
}
