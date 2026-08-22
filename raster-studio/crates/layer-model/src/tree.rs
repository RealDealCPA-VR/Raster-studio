//! The layer tree container: a flat id->Layer map plus an ordered root list.
//!
//! A flat map (rather than nested ownership) keeps ids stable, makes command
//! apply/undo cheap, and avoids borrow-checker fights when mutating one layer
//! while reading another. Group membership is expressed via
//! [`crate::layer::GroupLayer::children`] and the root order list.
//!
//! # Structural invariants
//!
//! Every mutating method upholds all four, and [`LayerTree::validate`] checks
//! them. Deserialization runs `validate` too, so a corrupt document fails to
//! load instead of crashing the editor later. The undo path is held to the same
//! standard: [`DetachedSubtree`] has private fields and no public constructor,
//! so outside this crate it can only arrive from [`LayerTree::remove`] or from
//! deserializing a journal — and that deserialization is routed through the
//! same structural `check` that [`LayerTree::reinsert`] runs before touching
//! the tree. That check ends in a reachability walk from the subtree's own
//! root, so an undo cannot put back an island or a cycle that the tree's own
//! traversal would then fail to reach. `reinsert` also asserts `validate()`
//! afterwards in debug builds, but that is a canary, not the guard: it is
//! compiled out in release and fires only after the mutation.
//!
//! 1. Ids are unique — a layer appears in `layers` at most once.
//! 2. **A layer id appears under at most one parent**: exactly one reference
//!    exists to it across `root` and every group's `children`.
//! 3. Every referenced id exists in `layers`, and every layer in `layers` is
//!    reachable from `root`.
//! 4. There are no cycles — a group can never contain itself or an ancestor.
//!
//! Together these make [`LayerTree::iter_depth_first`] terminating and make
//! `len()` agree with the traversal.
//!
//! One documented hole: [`LayerTree::get_mut`] hands out `&mut Layer`, and a
//! caller that edits a [`crate::layer::GroupLayer::children`] list through it
//! bypasses every check here. Structure is changed with `insert_at`,
//! `move_layer`, `remove` and `reinsert`; `get_mut` is for a layer's own
//! properties. [`LayerTree::validate`] is public so a caller that does reach
//! into `children` can check its work.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ids::LayerId;
use crate::layer::{ClippingMode, Layer, LayerKind};

/// Owns all layers in a document and their z-order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(try_from = "LayerTreeRepr")]
pub struct LayerTree {
    layers: HashMap<LayerId, Layer>,
    /// Top-level layer ids, top-most first.
    root: Vec<LayerId>,
}

/// Deserialization shadow of [`LayerTree`]. Exists only so `TryFrom` can run
/// [`LayerTree::validate`] before the value escapes into the document.
#[derive(Deserialize)]
struct LayerTreeRepr {
    #[serde(default)]
    layers: HashMap<LayerId, Layer>,
    #[serde(default)]
    root: Vec<LayerId>,
}

impl TryFrom<LayerTreeRepr> for LayerTree {
    type Error = TreeError;

    fn try_from(r: LayerTreeRepr) -> Result<Self, Self::Error> {
        let t = LayerTree {
            layers: r.layers,
            root: r.root,
        };
        t.validate()?;
        Ok(t)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    #[error("layer {0} not found")]
    NotFound(LayerId),
    #[error("parent {0} is not a group")]
    NotAGroup(LayerId),
    #[error("layer {0} is already in the tree")]
    DuplicateId(LayerId),
    #[error("layer {0} already has a parent")]
    AlreadyParented(LayerId),
    #[error("moving {moving} into {parent} would create a cycle")]
    WouldCycle { moving: LayerId, parent: LayerId },
    #[error("group {0} must be inserted empty")]
    NotEmpty(LayerId),
    #[error("layers {a} and {b} are not siblings")]
    NotSiblings { a: LayerId, b: LayerId },
    #[error("layer tree is corrupt: {0}")]
    Corrupt(String),
}

/// A layer and its whole subtree, detached from the tree.
///
/// Returned by [`LayerTree::remove`] so the command layer can put the exact
/// structure back with [`LayerTree::reinsert`] on undo. Removing a group and
/// keeping only the group layer would strand its children.
///
/// The fields are private and there is no public constructor, so outside this
/// crate the only ways to obtain one are [`LayerTree::remove`] and
/// deserializing a previously serialized value — and both routes run the same
/// structural check. That is what lets `reinsert` be an invariant-preserving
/// operation: a value naming ids it does not carry would push a dangling id
/// into the tree, and one whose layers are not all reachable from its own root
/// would push in an orphaned island, either of which reintroduces on the undo
/// path exactly the reachable-vs-stored divergence `remove` exists to prevent.
///
/// It is serializable because the command journal has to carry it: a delete's
/// inverse *is* the detached subtree. Deserialization runs the same structural
/// check that [`LayerTree::reinsert`] runs, so a hand-edited journal cannot
/// smuggle a malformed subtree in either.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "DetachedSubtreeRepr")]
pub struct DetachedSubtree {
    /// The id that was removed; `layers[0]` is its layer.
    root: LayerId,
    /// The subtree in depth-first order, `root` first. Group `children` lists
    /// are intact, so reinsertion restores the exact structure.
    layers: Vec<Layer>,
    /// Where `root` sat: parent group id, or `None` for the document root.
    parent: Option<LayerId>,
    /// Index `root` occupied within `parent`'s child list (or within `root`).
    index: usize,
}

/// Deserialization shadow of [`DetachedSubtree`]. Exists only so `TryFrom` can
/// run the structural `check` before the value escapes into a command.
/// Field names mirror [`DetachedSubtree`] exactly, so the wire format is just
/// the struct.
#[derive(Deserialize)]
struct DetachedSubtreeRepr {
    root: LayerId,
    layers: Vec<Layer>,
    #[serde(default)]
    parent: Option<LayerId>,
    #[serde(default)]
    index: usize,
}

impl TryFrom<DetachedSubtreeRepr> for DetachedSubtree {
    type Error = TreeError;

    fn try_from(r: DetachedSubtreeRepr) -> Result<Self, Self::Error> {
        let s = DetachedSubtree {
            root: r.root,
            layers: r.layers,
            parent: r.parent,
            index: r.index,
        };
        s.check()?;
        Ok(s)
    }
}

impl DetachedSubtree {
    /// The id that was removed.
    pub fn root(&self) -> LayerId {
        self.root
    }

    /// The parent group `root` sat in, or `None` for the document root.
    pub fn parent(&self) -> Option<LayerId> {
        self.parent
    }

    /// Index `root` occupied within its sibling list.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Every detached layer, depth-first, `root` first.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// The removed layer itself.
    pub fn root_layer(&self) -> &Layer {
        self.layers
            .first()
            .expect("a detached subtree always holds its root layer")
    }

    /// Number of layers detached, including `root`.
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// `true` when no layers are held. Never true for a value produced by
    /// [`LayerTree::remove`], which always carries at least the removed layer.
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    /// Structural self-check run by [`LayerTree::reinsert`] before it mutates
    /// anything: `layers[0]` really is `root`, ids are unique, every child
    /// named inside the subtree is part of the subtree, each non-root layer is
    /// claimed exactly once, and **every carried layer is reachable from
    /// `root`**.
    ///
    /// The reachability walk is not redundant with the reference counts. A
    /// subtree `{R, P -> [Q], Q -> [P]}` satisfies every counting rule — `P`
    /// and `Q` are each named exactly once, `R` never — yet `P` and `Q` hang
    /// off nothing. Reinserting it would put two layers into `layers` that no
    /// traversal from the document root can reach, so `len()` over-counts,
    /// `iter_depth_first` omits them, and they form a live cycle: precisely the
    /// corruption `remove` returning the *whole* subtree exists to prevent,
    /// arriving instead through undo. Reachability plus the single-reference
    /// rule also rules out every cycle inside the subtree, since a cycle's
    /// members are either double-referenced or unreachable.
    fn check(&self) -> Result<(), TreeError> {
        if self.layers.first().map(|l| l.id) != Some(self.root) {
            return Err(TreeError::Corrupt(format!(
                "detached subtree's first layer is not its root {}",
                self.root
            )));
        }
        let mut ids = HashSet::with_capacity(self.layers.len());
        for l in &self.layers {
            if !ids.insert(l.id) {
                return Err(TreeError::DuplicateId(l.id));
            }
        }
        // Every non-root layer must be claimed by exactly one group inside the
        // subtree, and the root by none: anything else re-enters the tree as a
        // dangling id, an orphan, or a second parent for one layer.
        let mut refs: HashMap<LayerId, usize> = HashMap::new();
        for l in &self.layers {
            for &c in l.children() {
                if !ids.contains(&c) {
                    return Err(TreeError::Corrupt(format!(
                        "detached subtree names child {c} that it does not contain"
                    )));
                }
                *refs.entry(c).or_insert(0) += 1;
            }
        }
        for l in &self.layers {
            let want = usize::from(l.id != self.root);
            let got = refs.get(&l.id).copied().unwrap_or(0);
            if got != want {
                return Err(TreeError::Corrupt(format!(
                    "detached layer {} is referenced {got} times inside the subtree, expected {want}",
                    l.id
                )));
            }
        }
        // Reachability, walked iteratively so a deep subtree cannot overflow
        // the stack on the undo path. Every named child is already known to be
        // one of `self.layers`, so `seen` can only ever be a subset of them and
        // the count comparison below is exact.
        let by_id: HashMap<LayerId, &Layer> = self.layers.iter().map(|l| (l.id, l)).collect();
        let mut seen = HashSet::with_capacity(self.layers.len());
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(l) = by_id.get(&id) {
                stack.extend_from_slice(l.children());
            }
        }
        if seen.len() != self.layers.len() {
            return Err(TreeError::Corrupt(format!(
                "detached subtree carries {} layers but only {} are reachable from its root {}",
                self.layers.len(),
                seen.len(),
                self.root
            )));
        }
        Ok(())
    }
}

/// The layers that composite as a single clipping group.
///
/// In Photoshop terms: `base` is the layer whose alpha does the clipping, and
/// `clipped` are the layers stacked directly above it that carry
/// [`ClippingMode::ClipToBelow`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClippingGroup {
    /// The clipping base. Its own `clipping` is [`ClippingMode::None`].
    pub base: LayerId,
    /// Layers clipped to `base`, top-most first. Never empty.
    pub clipped: Vec<LayerId>,
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

    pub fn contains(&self, id: LayerId) -> bool {
        self.layers.contains_key(&id)
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

    /// Insert a layer at the top of the root list.
    ///
    /// # Errors
    ///
    /// - [`TreeError::DuplicateId`] if the id is already in the tree. Silently
    ///   re-inserting would push a second reference into `root` and produce
    ///   duplicate ids in every traversal.
    /// - [`TreeError::NotFound`] / [`TreeError::AlreadyParented`] if the layer
    ///   is a group naming children — see [`LayerTree::insert_at`], which
    ///   rejects every such group.
    pub fn push_root(&mut self, layer: Layer) -> Result<LayerId, TreeError> {
        self.insert_at(layer, None, 0)
    }

    /// Insert a layer into `parent` at `index` (clamped), or at the root when
    /// `parent` is `None`. Same error conditions as [`LayerTree::push_root`],
    /// plus [`TreeError::NotAGroup`].
    ///
    /// **A group must arrive empty.** Invariant 2 says every layer already in
    /// `layers` has exactly one parent, so a named child is either unknown
    /// ([`TreeError::NotFound`]) or already parented
    /// ([`TreeError::AlreadyParented`]) — there is no third case, and therefore
    /// no input for which a pre-populated group is accepted. To wrap existing
    /// layers in a new group in one atomic step, use
    /// [`LayerTree::group_layers`].
    pub fn insert_at(
        &mut self,
        layer: Layer,
        parent: Option<LayerId>,
        index: usize,
    ) -> Result<LayerId, TreeError> {
        let id = layer.id;
        if self.layers.contains_key(&id) {
            return Err(TreeError::DuplicateId(id));
        }
        // A group arriving with children must not steal ids that are already
        // owned elsewhere (invariant 2) or name ids that do not exist.
        let mut seen = HashSet::new();
        for &c in layer.children() {
            if !seen.insert(c) {
                return Err(TreeError::DuplicateId(c));
            }
            if !self.layers.contains_key(&c) {
                return Err(TreeError::NotFound(c));
            }
            if self.reference_count(c) > 0 {
                return Err(TreeError::AlreadyParented(c));
            }
        }
        // Validate the destination before mutating anything.
        if let Some(pid) = parent {
            match self.layers.get(&pid) {
                None => return Err(TreeError::NotFound(pid)),
                Some(p) if !p.is_group() => return Err(TreeError::NotAGroup(pid)),
                Some(_) => {}
            }
        }

        self.layers.insert(id, layer);
        self.attach(id, parent, index)?;
        Ok(id)
    }

    /// Remove `id` **and its entire subtree**.
    ///
    /// Removing only the group layer would leave its children in `layers`
    /// unreachable from `root`, over-counting `len()` and dropping them from
    /// `iter_depth_first`. The returned [`DetachedSubtree`] holds everything
    /// needed to undo the removal via [`LayerTree::reinsert`].
    pub fn remove(&mut self, id: LayerId) -> Result<DetachedSubtree, TreeError> {
        if !self.layers.contains_key(&id) {
            return Err(TreeError::NotFound(id));
        }
        let parent = self.parent_of(id);
        let index = self
            .index_in_parent(id)
            .ok_or_else(|| TreeError::Corrupt(format!("layer {id} has no position")))?;

        let ids = self.subtree_ids(id);
        let mut layers = Vec::with_capacity(ids.len());
        for sid in &ids {
            // Every id came from a live traversal, so the removal cannot fail.
            let l = self
                .layers
                .remove(sid)
                .ok_or_else(|| TreeError::Corrupt(format!("subtree id {sid} vanished")))?;
            layers.push(l);
        }
        self.detach(id);

        Ok(DetachedSubtree {
            root: id,
            layers,
            parent,
            index,
        })
    }

    /// Put a previously [`removed`](LayerTree::remove) subtree back.
    ///
    /// Restores the original parent and index when they are still valid; a
    /// missing parent is an error rather than a silent re-parent to the root,
    /// so a broken undo is loud.
    ///
    /// # Errors
    ///
    /// [`TreeError::Corrupt`] when the subtree is not internally consistent —
    /// its first layer is not its root, it names a child it does not carry, a
    /// layer is claimed by two parents, or some carried layer is not reachable
    /// from the subtree's root, which is what stops an undo from smuggling an
    /// orphaned island or a group cycle back into the tree —
    /// [`TreeError::DuplicateId`] when any id is already in the tree,
    /// and [`TreeError::NotFound`] / [`TreeError::NotAGroup`] for a destination
    /// that no longer accepts it. All checks run before any mutation, so a
    /// rejected reinsert leaves the tree unchanged and still valid.
    pub fn reinsert(&mut self, sub: DetachedSubtree) -> Result<(), TreeError> {
        sub.check()?;
        for l in &sub.layers {
            if self.layers.contains_key(&l.id) {
                return Err(TreeError::DuplicateId(l.id));
            }
        }
        if let Some(pid) = sub.parent {
            match self.layers.get(&pid) {
                None => return Err(TreeError::NotFound(pid)),
                Some(p) if !p.is_group() => return Err(TreeError::NotAGroup(pid)),
                Some(_) => {}
            }
        }
        let root = sub.root;
        let (parent, index) = (sub.parent, sub.index);
        for l in sub.layers {
            self.layers.insert(l.id, l);
        }
        let r = self.attach(root, parent, index);
        debug_assert!(
            r.is_err() || self.validate().is_ok(),
            "reinsert left the tree invalid: {:?}",
            self.validate()
        );
        r
    }

    /// Re-parent `id` into `parent` group at `index` (or root if `parent` is
    /// `None`). Detaches from its current location first.
    ///
    /// # Errors
    ///
    /// [`TreeError::WouldCycle`] when `parent` is `id` itself or one of its
    /// descendants. Allowing that would make the tree self-referential and send
    /// [`LayerTree::iter_depth_first`] into unbounded recursion.
    ///
    /// All validation happens before any mutation, so a rejected move leaves
    /// the tree byte-for-byte unchanged.
    pub fn move_layer(
        &mut self,
        id: LayerId,
        parent: Option<LayerId>,
        index: usize,
    ) -> Result<(), TreeError> {
        if !self.layers.contains_key(&id) {
            return Err(TreeError::NotFound(id));
        }
        if let Some(pid) = parent {
            if pid == id || self.is_descendant_of(pid, id) {
                return Err(TreeError::WouldCycle {
                    moving: id,
                    parent: pid,
                });
            }
            match self.layers.get(&pid) {
                None => return Err(TreeError::NotFound(pid)),
                Some(p) if !p.is_group() => return Err(TreeError::NotAGroup(pid)),
                Some(_) => {}
            }
        }
        self.detach(id);
        self.attach(id, parent, index)
    }

    /// Wrap existing sibling layers in a new group, atomically.
    ///
    /// This is Photoshop's "Group Selected Layers" (Ctrl+G). `group` must be an
    /// **empty** [`crate::LayerKind::Group`] layer not already in the tree; it
    /// is inserted at (`parent`, `index`) and then `ids` are re-parented into
    /// it, in the order given (top-most first). Returns the new group's id.
    ///
    /// Doing this by hand — `insert_at` an empty group, then `move_layer` each
    /// child — is not atomic: a failure part-way leaves the document holding a
    /// half-built group that the user never asked for, and every intermediate
    /// state is a state the render graph and the layers panel can observe.
    ///
    /// # Errors
    ///
    /// All of them are raised before anything is mutated, and the work itself
    /// runs on a copy that is only committed once [`LayerTree::validate`]
    /// passes, so **a rejected call leaves the tree byte-for-byte unchanged**.
    ///
    /// - [`TreeError::DuplicateId`] — `group` is already in the tree, or `ids`
    ///   names the same layer twice.
    /// - [`TreeError::NotAGroup`] — `group` is not a group, or `parent` is not.
    /// - [`TreeError::NotEmpty`] — `group` already names children.
    /// - [`TreeError::NotFound`] — an id in `ids`, or `parent`, is not present.
    /// - [`TreeError::NotSiblings`] — the `ids` do not all share one parent.
    ///   Grouping across levels has no well-defined resulting z-order, so it is
    ///   refused rather than guessed at.
    /// - [`TreeError::WouldCycle`] — `parent` is one of `ids` or lives beneath
    ///   one of them; the new group would end up inside a layer it contains.
    pub fn group_layers(
        &mut self,
        ids: &[LayerId],
        group: Layer,
        parent: Option<LayerId>,
        index: usize,
    ) -> Result<LayerId, TreeError> {
        let gid = group.id;
        if !group.is_group() {
            return Err(TreeError::NotAGroup(gid));
        }
        if !group.children().is_empty() {
            return Err(TreeError::NotEmpty(gid));
        }
        if self.layers.contains_key(&gid) {
            return Err(TreeError::DuplicateId(gid));
        }
        if let Some(pid) = parent {
            match self.layers.get(&pid) {
                None => return Err(TreeError::NotFound(pid)),
                Some(p) if !p.is_group() => return Err(TreeError::NotAGroup(pid)),
                Some(_) => {}
            }
        }

        let mut seen = HashSet::with_capacity(ids.len());
        for &id in ids {
            if !seen.insert(id) {
                return Err(TreeError::DuplicateId(id));
            }
            if !self.layers.contains_key(&id) {
                return Err(TreeError::NotFound(id));
            }
            // The group lands inside `parent`, so re-parenting a layer that
            // `parent` descends from would close a loop.
            if let Some(pid) = parent {
                if self.is_descendant_of(pid, id) {
                    return Err(TreeError::WouldCycle {
                        moving: id,
                        parent: pid,
                    });
                }
            }
        }
        if let Some(&first) = ids.first() {
            let home = self.parent_of(first);
            for &id in &ids[1..] {
                if self.parent_of(id) != home {
                    return Err(TreeError::NotSiblings { a: first, b: id });
                }
            }
        }

        // Every failure mode above is already ruled out, but the commit-a-copy
        // shape is what makes atomicity a property of the code rather than of
        // the argument above it.
        let mut next = self.clone();
        next.insert_at(group, parent, index)?;
        for (i, &id) in ids.iter().enumerate() {
            next.move_layer(id, Some(gid), i)?;
        }
        next.validate()?;
        *self = next;
        Ok(gid)
    }

    /// The parent group of `id`, or `None` when it sits at the document root
    /// (or is not in the tree at all — use [`LayerTree::contains`] to tell
    /// those apart).
    pub fn parent_of(&self, id: LayerId) -> Option<LayerId> {
        self.layers
            .iter()
            .find(|(_, l)| l.children().contains(&id))
            .map(|(pid, _)| *pid)
    }

    /// The ordered sibling list containing `id` (top-most first).
    pub fn siblings_of(&self, id: LayerId) -> Option<&[LayerId]> {
        match self.parent_of(id) {
            Some(pid) => self.layers.get(&pid).map(|p| p.children()),
            None if self.root.contains(&id) => Some(&self.root),
            None => None,
        }
    }

    /// Position of `id` within its sibling list.
    pub fn index_in_parent(&self, id: LayerId) -> Option<usize> {
        self.siblings_of(id)?.iter().position(|&s| s == id)
    }

    /// `true` when `candidate` is `ancestor` itself or lives anywhere beneath
    /// it.
    pub fn is_descendant_of(&self, candidate: LayerId, ancestor: LayerId) -> bool {
        if candidate == ancestor {
            return true;
        }
        self.subtree_ids(ancestor).contains(&candidate)
    }

    /// Depth of `id` below the document root; root-level layers are 0.
    pub fn depth_of(&self, id: LayerId) -> Option<usize> {
        if !self.layers.contains_key(&id) {
            return None;
        }
        let mut d = 0;
        let mut cur = id;
        let mut guard = self.layers.len() + 1;
        while let Some(p) = self.parent_of(cur) {
            d += 1;
            cur = p;
            guard -= 1;
            if guard == 0 {
                return None;
            }
        }
        Some(d)
    }

    /// The clipping group `id` participates in, or `None` when it is not part
    /// of one.
    ///
    /// A clipping group is a contiguous run of siblings: one base layer plus
    /// the layers stacked directly above it that carry
    /// [`ClippingMode::ClipToBelow`]. Both a base with nothing clipped to it
    /// and a dangling clipper with no layer beneath it return `None` — in both
    /// cases the compositor draws the layer normally.
    ///
    /// This is the query the compositor needs to render clipped layers into the
    /// base's alpha before blending the result down.
    pub fn clipping_group(&self, id: LayerId) -> Option<ClippingGroup> {
        let sibs = self.siblings_of(id)?;
        let i = sibs.iter().position(|&s| s == id)?;

        // Walk *down* (increasing index) to the first non-clipping layer: the
        // base. A run of clippers that reaches the bottom has no base.
        let mut b = i;
        loop {
            let l = self.layers.get(&sibs[b])?;
            if l.clipping == ClippingMode::None {
                break;
            }
            b += 1;
            if b >= sibs.len() {
                return None;
            }
        }

        // Walk back *up* collecting the clippers stacked on the base.
        let mut clipped = Vec::new();
        let mut k = b;
        while k > 0 {
            k -= 1;
            let l = self.layers.get(&sibs[k])?;
            if l.clipping != ClippingMode::ClipToBelow {
                break;
            }
            clipped.push(sibs[k]);
        }
        if clipped.is_empty() {
            return None;
        }
        // Collected bottom-up; hand back top-most first to match z-order.
        clipped.reverse();
        Some(ClippingGroup {
            base: sibs[b],
            clipped,
        })
    }

    /// `true` when `id` is clipped to a base layer beneath it.
    ///
    /// A layer flagged [`ClippingMode::ClipToBelow`] with nothing beneath it to
    /// clip to is *not* clipped — the flag has no effect.
    pub fn is_clipped(&self, id: LayerId) -> bool {
        self.clipping_group(id)
            .is_some_and(|g| g.clipped.contains(&id))
    }

    /// Depth-first iteration of ids in composite order (root order, descending
    /// into groups). Useful for the render graph walk.
    ///
    /// Guaranteed to terminate and to visit each id at most once even if the
    /// invariants were somehow violated.
    pub fn iter_depth_first(&self) -> Vec<LayerId> {
        let mut out = Vec::with_capacity(self.layers.len());
        let mut seen = HashSet::with_capacity(self.layers.len());
        for &id in &self.root {
            self.walk_subtree(id, &mut out, &mut seen);
        }
        out
    }

    /// All ids in `id`'s subtree, `id` first, in depth-first order.
    /// Empty when `id` is not in the tree.
    pub fn subtree_ids(&self, id: LayerId) -> Vec<LayerId> {
        let mut out = Vec::new();
        if self.layers.contains_key(&id) {
            let mut seen = HashSet::new();
            self.walk_subtree(id, &mut out, &mut seen);
        }
        out
    }

    /// Check every structural invariant. Cheap enough for debug assertions and
    /// for validating freshly deserialized documents.
    pub fn validate(&self) -> Result<(), TreeError> {
        // Invariant 2: exactly one reference to every layer.
        let mut refs: HashMap<LayerId, usize> = HashMap::new();
        for &r in &self.root {
            *refs.entry(r).or_insert(0) += 1;
        }
        for l in self.layers.values() {
            for &c in l.children() {
                *refs.entry(c).or_insert(0) += 1;
            }
        }
        for (id, n) in &refs {
            if !self.layers.contains_key(id) {
                return Err(TreeError::NotFound(*id));
            }
            if *n > 1 {
                return Err(TreeError::AlreadyParented(*id));
            }
        }
        for id in self.layers.keys() {
            if !refs.contains_key(id) {
                return Err(TreeError::Corrupt(format!(
                    "layer {id} is not referenced by root or any group"
                )));
            }
        }
        // Invariants 3 and 4: everything is reachable from root exactly once.
        // Combined with the single-reference check above, reachability rules out
        // cycles: a cycle's members are either double-referenced or unreachable.
        let reached = self.iter_depth_first();
        if reached.len() != self.layers.len() {
            return Err(TreeError::Corrupt(format!(
                "{} layers stored but {} reachable from root",
                self.layers.len(),
                reached.len()
            )));
        }
        // "Only a group holds children" needs no check here: the child list
        // lives in [`crate::layer::GroupLayer`] and nowhere else, so
        // `Layer::children()` returns an empty slice for every other kind and
        // a non-group naming a child is a state this crate cannot represent.
        // `layer::tests::only_a_group_can_ever_hold_children` pins that, which
        // is a real test — the runtime check this replaces was a branch no
        // input could reach.
        Ok(())
    }

    // ---- internals ---------------------------------------------------------

    /// How many parents currently reference `id` (0 or 1 under the invariants).
    fn reference_count(&self, id: LayerId) -> usize {
        let in_root = self.root.iter().filter(|&&r| r == id).count();
        let in_groups: usize = self
            .layers
            .values()
            .map(|l| l.children().iter().filter(|&&c| c == id).count())
            .sum();
        in_root + in_groups
    }

    /// Unlink `id` from wherever it currently sits. The layer itself stays in
    /// `layers`.
    fn detach(&mut self, id: LayerId) {
        self.root.retain(|&r| r != id);
        for l in self.layers.values_mut() {
            if let LayerKind::Group(g) = &mut l.kind {
                g.children.retain(|&c| c != id);
            }
        }
    }

    /// Link `id` into `parent` at `index` (clamped). Callers must have already
    /// validated `parent`.
    fn attach(
        &mut self,
        id: LayerId,
        parent: Option<LayerId>,
        index: usize,
    ) -> Result<(), TreeError> {
        match parent {
            None => {
                let idx = index.min(self.root.len());
                self.root.insert(idx, id);
                Ok(())
            }
            Some(pid) => {
                let p = self.layers.get_mut(&pid).ok_or(TreeError::NotFound(pid))?;
                match &mut p.kind {
                    LayerKind::Group(g) => {
                        let idx = index.min(g.children.len());
                        g.children.insert(idx, id);
                        Ok(())
                    }
                    _ => Err(TreeError::NotAGroup(pid)),
                }
            }
        }
    }

    /// Depth-first walk from `id`, appending each id visited to `out`.
    ///
    /// Iterative rather than recursive: nesting depth is bounded only by the
    /// number of layers, and `validate` runs this walk on every deserialize.
    /// Recursion would turn a deep — but perfectly legal — document into a
    /// stack overflow, and a stack overflow aborts the process instead of
    /// returning the [`TreeError`] the caller is waiting to reject the file
    /// with.
    fn walk_subtree(&self, id: LayerId, out: &mut Vec<LayerId>, seen: &mut HashSet<LayerId>) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                // Defensive: the invariants forbid this, but a truncated
                // traversal beats an endless one if a future edit slips through.
                continue;
            }
            let Some(l) = self.layers.get(&id) else {
                continue;
            };
            out.push(id);
            // Reversed, so the children pop off in their stored z-order.
            stack.extend(l.children().iter().rev().copied());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `G { A, B }` at the root plus a sibling `S`, returning
    /// `(tree, g, a, b, s)`.
    fn nested() -> (LayerTree, LayerId, LayerId, LayerId, LayerId) {
        let mut t = LayerTree::new();
        let s = t.push_root(Layer::raster("S")).unwrap();
        let g = t.push_root(Layer::group("G")).unwrap();
        let a = t.push_root(Layer::raster("A")).unwrap();
        let b = t.push_root(Layer::raster("B")).unwrap();
        t.move_layer(a, Some(g), 0).unwrap();
        t.move_layer(b, Some(g), 1).unwrap();
        t.validate().unwrap();
        (t, g, a, b, s)
    }

    #[test]
    fn push_and_get() {
        let mut t = LayerTree::new();
        let id = t.push_root(Layer::raster("L1")).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t.get(id).unwrap().name, "L1");
        assert_eq!(t.root(), &[id]);
        t.validate().unwrap();
    }

    #[test]
    fn move_into_group_and_back() {
        let mut t = LayerTree::new();
        let g = t.push_root(Layer::group("G")).unwrap();
        let l = t.push_root(Layer::raster("L")).unwrap();
        t.move_layer(l, Some(g), 0).unwrap();

        let order = t.iter_depth_first();
        let gi = order.iter().position(|&x| x == g).unwrap();
        assert_eq!(order[gi + 1], l);
        assert_eq!(t.parent_of(l), Some(g));
        assert_eq!(t.depth_of(l), Some(1));

        t.move_layer(l, None, 0).unwrap();
        assert!(t.root().contains(&l));
        assert_eq!(t.parent_of(l), None);
        assert_eq!(t.depth_of(l), Some(0));
        t.validate().unwrap();
    }

    // ---- bug 1: cycles ------------------------------------------------------

    #[test]
    fn moving_a_group_into_itself_is_rejected() {
        let mut t = LayerTree::new();
        let g = t.push_root(Layer::group("G")).unwrap();
        let err = t.move_layer(g, Some(g), 0).unwrap_err();
        assert_eq!(
            err,
            TreeError::WouldCycle {
                moving: g,
                parent: g
            }
        );
        // And the tree is untouched: without the guard `walk_subtree` would
        // loop forever here.
        assert_eq!(t.root(), &[g]);
        assert_eq!(t.iter_depth_first(), vec![g]);
        t.validate().unwrap();
    }

    #[test]
    fn moving_a_group_into_its_own_descendant_is_rejected() {
        let mut t = LayerTree::new();
        let outer = t.push_root(Layer::group("Outer")).unwrap();
        let mid = t.push_root(Layer::group("Mid")).unwrap();
        let inner = t.push_root(Layer::group("Inner")).unwrap();
        t.move_layer(mid, Some(outer), 0).unwrap();
        t.move_layer(inner, Some(mid), 0).unwrap();

        let err = t.move_layer(outer, Some(inner), 0).unwrap_err();
        assert_eq!(
            err,
            TreeError::WouldCycle {
                moving: outer,
                parent: inner
            }
        );
        assert_eq!(t.iter_depth_first(), vec![outer, mid, inner]);
        t.validate().unwrap();
    }

    #[test]
    fn a_rejected_move_leaves_the_tree_unchanged() {
        let (mut t, g, a, _b, s) = nested();
        let before = t.iter_depth_first();

        // Target is not a group.
        assert_eq!(
            t.move_layer(g, Some(s), 0).unwrap_err(),
            TreeError::NotAGroup(s)
        );
        assert_eq!(
            t.iter_depth_first(),
            before,
            "detach must not have happened"
        );

        // Target does not exist.
        let ghost = LayerId::new();
        assert_eq!(
            t.move_layer(a, Some(ghost), 0).unwrap_err(),
            TreeError::NotFound(ghost)
        );
        assert_eq!(t.iter_depth_first(), before);
        t.validate().unwrap();
    }

    #[test]
    fn moving_a_descendant_up_to_its_ancestors_parent_is_allowed() {
        let (mut t, g, a, _b, _s) = nested();
        // Not a cycle: `a` is below `g`, so `g` is a legal new parent for... it
        // already is. Move `a` out to the root instead, then back under `g`.
        t.move_layer(a, None, 0).unwrap();
        assert_eq!(t.parent_of(a), None);
        t.move_layer(a, Some(g), 0).unwrap();
        assert_eq!(t.parent_of(a), Some(g));
        t.validate().unwrap();
    }

    // ---- bug 2: orphaned subtrees ------------------------------------------

    #[test]
    fn removing_a_group_removes_its_whole_subtree() {
        let (mut t, g, a, b, s) = nested();
        assert_eq!(t.len(), 4);

        let sub = t.remove(g).unwrap();

        assert_eq!(t.len(), 1, "len must not count orphans");
        assert_eq!(t.iter_depth_first(), vec![s]);
        assert!(t.get(a).is_none(), "child A must be gone, not orphaned");
        assert!(t.get(b).is_none(), "child B must be gone, not orphaned");
        assert!(!t.contains(g));
        t.validate().unwrap();

        // The detached subtree carries everything undo needs.
        assert_eq!(sub.root(), g);
        assert_eq!(sub.len(), 3);
        assert!(!sub.is_empty());
        assert_eq!(sub.root_layer().id, g);
        assert_eq!(sub.parent(), None);
        assert_eq!(sub.index(), 0);
        assert_eq!(
            sub.layers().iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![g, a, b]
        );
    }

    #[test]
    fn removing_a_nested_group_removes_grandchildren_too() {
        let mut t = LayerTree::new();
        let outer = t.push_root(Layer::group("Outer")).unwrap();
        let mid = t.push_root(Layer::group("Mid")).unwrap();
        let leaf = t.push_root(Layer::raster("Leaf")).unwrap();
        t.move_layer(mid, Some(outer), 0).unwrap();
        t.move_layer(leaf, Some(mid), 0).unwrap();
        assert_eq!(t.len(), 3);

        let sub = t.remove(outer).unwrap();
        assert_eq!(sub.len(), 3);
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
        assert!(t.iter_depth_first().is_empty());
        t.validate().unwrap();
    }

    #[test]
    fn removed_subtree_reinserts_at_its_original_position() {
        let (mut t, g, a, b, _s) = nested();
        let sub = t.remove(a).unwrap();
        assert_eq!(sub.parent(), Some(g));
        assert_eq!(sub.index(), 0);
        assert_eq!(t.get(g).unwrap().children(), &[b]);

        t.reinsert(sub).unwrap();
        assert_eq!(t.get(g).unwrap().children(), &[a, b], "order restored");
        assert_eq!(t.len(), 4);
        t.validate().unwrap();
    }

    #[test]
    fn reinserting_a_group_restores_the_whole_structure() {
        let (mut t, g, a, b, s) = nested();
        let before = t.iter_depth_first();
        let sub = t.remove(g).unwrap();
        t.reinsert(sub).unwrap();
        assert_eq!(t.iter_depth_first(), before);
        assert_eq!(t.get(g).unwrap().children(), &[a, b]);
        assert!(t.root().contains(&s));
        t.validate().unwrap();
    }

    #[test]
    fn reinserting_twice_is_rejected() {
        let (mut t, g, _a, _b, _s) = nested();
        let sub = t.remove(g).unwrap();
        t.reinsert(sub.clone()).unwrap();
        assert_eq!(t.reinsert(sub).unwrap_err(), TreeError::DuplicateId(g));
        t.validate().unwrap();
    }

    // `DetachedSubtree`'s fields are private, so outside this crate the values
    // below are unconstructible — that is the primary guard. These tests build
    // them from inside the module to prove the secondary guard in `reinsert`
    // also holds, because `remove` is not the only code that could ever
    // construct one.

    #[test]
    fn reinserting_a_subtree_with_a_phantom_root_is_rejected() {
        let (mut t, _g, _a, _b, _s) = nested();
        let before = t.iter_depth_first();
        let phantom = LayerId::new();

        let err = t
            .reinsert(DetachedSubtree {
                root: phantom,
                layers: Vec::new(),
                parent: None,
                index: 0,
            })
            .unwrap_err();

        assert!(
            matches!(err, TreeError::Corrupt(_)),
            "expected Corrupt, got {err:?}"
        );
        assert!(
            !t.root().contains(&phantom),
            "a dangling id must never reach `root`"
        );
        assert_eq!(t.iter_depth_first(), before);
        assert_eq!(t.len(), before.len());
        t.validate()
            .expect("a rejected reinsert leaves a valid tree");
    }

    #[test]
    fn reinserting_a_subtree_missing_one_of_its_children_is_rejected() {
        let (mut t, g, _a, _b, _s) = nested();
        let before = t.iter_depth_first();
        let mut sub = t.remove(g).unwrap();
        // Drop child `a`'s layer while `g` still names it: reinserting this
        // would put an unresolvable id into the group's child list.
        let dropped = sub.layers.remove(1);
        let err = t.reinsert(sub).unwrap_err();
        assert!(
            matches!(err, TreeError::Corrupt(_)),
            "expected Corrupt, got {err:?}"
        );
        assert!(!t.contains(dropped.id));
        assert!(!t.contains(g), "nothing may be inserted by a rejected call");
        t.validate().unwrap();
        assert_eq!(t.iter_depth_first().len(), before.len() - 3);
    }

    #[test]
    fn reinserting_a_subtree_whose_first_layer_is_not_its_root_is_rejected() {
        let (mut t, g, a, _b, _s) = nested();
        let mut sub = t.remove(g).unwrap();
        sub.layers.swap(0, 1);
        assert_eq!(sub.layers[0].id, a);
        let err = t.reinsert(sub).unwrap_err();
        assert!(
            matches!(err, TreeError::Corrupt(_)),
            "expected Corrupt, got {err:?}"
        );
        assert!(!t.contains(g) && !t.contains(a));
        t.validate().unwrap();
    }

    #[test]
    fn reinserting_a_subtree_that_claims_one_child_twice_is_rejected() {
        let (mut t, g, a, _b, _s) = nested();
        let mut sub = t.remove(g).unwrap();
        // Make `g` name `a` twice: two references to one id.
        if let LayerKind::Group(gr) = &mut sub.layers[0].kind {
            gr.children.push(a);
        }
        let err = t.reinsert(sub).unwrap_err();
        assert!(
            matches!(err, TreeError::Corrupt(_)),
            "expected Corrupt, got {err:?}"
        );
        t.validate().unwrap();
    }

    /// `{root, P -> [Q], Q -> [P]}`: a subtree whose root carries nothing and
    /// whose other two layers point at each other. Every *counting* rule holds
    /// — P and Q are each named exactly once and the root never — so only a
    /// reachability walk from `root` can reject it.
    fn subtree_with_a_disconnected_cycle() -> (DetachedSubtree, LayerId, LayerId, LayerId) {
        let root = Layer::group("Root");
        let mut p = Layer::group("P");
        let mut q = Layer::group("Q");
        let (rid, pid, qid) = (root.id, p.id, q.id);
        if let LayerKind::Group(g) = &mut p.kind {
            g.children.push(qid);
        }
        if let LayerKind::Group(g) = &mut q.kind {
            g.children.push(pid);
        }
        let sub = DetachedSubtree {
            root: rid,
            layers: vec![root, p, q],
            parent: None,
            index: 0,
        };
        // Premise: the pre-existing rules really are satisfied, so this test
        // fails for the reachability pass and nothing else.
        let mut refs: HashMap<LayerId, usize> = HashMap::new();
        for l in &sub.layers {
            for &c in l.children() {
                *refs.entry(c).or_insert(0) += 1;
            }
        }
        assert_eq!(refs.get(&rid), None, "the root must be unreferenced");
        assert_eq!(refs.get(&pid), Some(&1));
        assert_eq!(refs.get(&qid), Some(&1));
        (sub, rid, pid, qid)
    }

    #[test]
    fn reinserting_a_subtree_hiding_a_disconnected_cycle_is_rejected() {
        let (mut t, _g, _a, _b, _s) = nested();
        let before = snapshot(&t);
        let len_before = t.len();
        let (sub, rid, pid, qid) = subtree_with_a_disconnected_cycle();

        let err = sub.check().unwrap_err();
        assert!(
            matches!(&err, TreeError::Corrupt(m) if m.contains("reachable")),
            "expected a reachability rejection, got {err:?}"
        );
        assert_eq!(t.reinsert(sub).unwrap_err(), err);

        // Nothing may have landed: without the walk, `reinsert` returns Ok and
        // leaves the tree holding a live P<->Q cycle that no traversal reaches.
        for id in [rid, pid, qid] {
            assert!(!t.contains(id), "a rejected reinsert inserted {id}");
        }
        assert_eq!(t.len(), len_before);
        assert_eq!(snapshot(&t), before);
        t.validate()
            .expect("a rejected reinsert leaves a valid tree");
    }

    #[test]
    fn a_hand_edited_journal_cannot_smuggle_in_a_disconnected_cycle() {
        // Serialization is unchecked by design (the value was already valid),
        // so this is exactly the payload a hand-edited journal could hold.
        let (sub, ..) = subtree_with_a_disconnected_cycle();
        let json = serde_json::to_string(&sub).unwrap();
        let err = serde_json::from_str::<DetachedSubtree>(&json).unwrap_err();
        assert!(
            err.to_string().contains("reachable"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn remove_detaches_from_group() {
        let (mut t, g, a, b, _s) = nested();
        t.remove(a).unwrap();
        assert!(t.get(a).is_none());
        assert_eq!(t.get(g).unwrap().children(), &[b]);
        assert_eq!(t.len(), 3);
        t.validate().unwrap();
    }

    #[test]
    fn removing_a_missing_layer_errors() {
        let mut t = LayerTree::new();
        let ghost = LayerId::new();
        assert_eq!(t.remove(ghost).unwrap_err(), TreeError::NotFound(ghost));
    }

    // ---- bug 3: duplicate push ---------------------------------------------

    #[test]
    fn pushing_the_same_id_twice_is_rejected() {
        let mut t = LayerTree::new();
        let l = Layer::raster("L");
        let id = t.push_root(l.clone()).unwrap();
        assert_eq!(t.push_root(l).unwrap_err(), TreeError::DuplicateId(id));
        assert_eq!(t.root(), &[id], "root must not gain a second reference");
        assert_eq!(t.iter_depth_first(), vec![id]);
        assert_eq!(t.len(), 1);
        t.validate().unwrap();
    }

    #[test]
    fn re_pushing_a_layer_after_removal_is_fine() {
        let mut t = LayerTree::new();
        let l = Layer::raster("L");
        let id = t.push_root(l.clone()).unwrap();
        t.remove(id).unwrap();
        assert_eq!(t.push_root(l).unwrap(), id);
        t.validate().unwrap();
    }

    // ---- bug 4: one parent per id ------------------------------------------

    #[test]
    fn moving_never_leaves_a_second_reference_behind() {
        let (mut t, g, a, _b, s) = nested();
        // Bounce `a` around; every landing must leave exactly one reference.
        for dest in [None, Some(g), None, Some(g)] {
            t.move_layer(a, dest, 0).unwrap();
            assert_eq!(t.reference_count(a), 1);
            t.validate().unwrap();
        }
        assert_eq!(t.reference_count(s), 1);
    }

    #[test]
    fn a_group_carrying_children_cannot_steal_a_parented_id() {
        let (mut t, g, a, _b, _s) = nested();
        // Fabricate a second group that claims `a`, which already lives in `g`.
        let mut thief = Layer::group("Thief");
        if let LayerKind::Group(gr) = &mut thief.kind {
            gr.children.push(a);
        }
        assert_eq!(
            t.push_root(thief).unwrap_err(),
            TreeError::AlreadyParented(a)
        );
        assert_eq!(t.parent_of(a), Some(g));
        assert_eq!(t.len(), 4, "the rejected group must not have been inserted");
        t.validate().unwrap();
    }

    #[test]
    fn a_group_naming_an_unknown_child_is_rejected() {
        let mut t = LayerTree::new();
        let ghost = LayerId::new();
        let mut gr = Layer::group("G");
        if let LayerKind::Group(g) = &mut gr.kind {
            g.children.push(ghost);
        }
        assert_eq!(t.push_root(gr).unwrap_err(), TreeError::NotFound(ghost));
        assert!(t.is_empty());
    }

    #[test]
    fn a_prepopulated_group_is_rejected_on_every_insertion_path() {
        // The doc on `Layer::with_kind` used to claim a caller could hand in a
        // populated group. Invariant 2 makes that unreachable: a named child is
        // either unknown or already parented, never free.
        let (mut t, g, a, _b, _s) = nested();
        let before = snapshot(&t);

        let mut claims_known = Layer::group("Known");
        if let LayerKind::Group(gr) = &mut claims_known.kind {
            gr.children.push(a);
        }
        assert_eq!(
            t.push_root(claims_known).unwrap_err(),
            TreeError::AlreadyParented(a)
        );

        let ghost = LayerId::new();
        let mut claims_unknown = Layer::group("Unknown");
        if let LayerKind::Group(gr) = &mut claims_unknown.kind {
            gr.children.push(ghost);
        }
        assert_eq!(
            t.insert_at(claims_unknown, Some(g), 0).unwrap_err(),
            TreeError::NotFound(ghost)
        );

        assert_eq!(snapshot(&t), before);
        t.validate().unwrap();
    }

    // ---- grouping existing layers ------------------------------------------

    /// Full structural fingerprint: the root order plus every layer's child
    /// list in depth-first order. Two trees with the same snapshot are the same
    /// shape.
    fn snapshot(t: &LayerTree) -> (Vec<LayerId>, Vec<(LayerId, Vec<LayerId>)>) {
        (
            t.root().to_vec(),
            t.iter_depth_first()
                .into_iter()
                .map(|id| (id, t.get(id).unwrap().children().to_vec()))
                .collect(),
        )
    }

    /// Root stack `[a, b, c]`, all siblings at the document root.
    fn flat() -> (LayerTree, LayerId, LayerId, LayerId) {
        let mut t = LayerTree::new();
        let c = t.push_root(Layer::raster("C")).unwrap();
        let b = t.push_root(Layer::raster("B")).unwrap();
        let a = t.push_root(Layer::raster("A")).unwrap();
        assert_eq!(t.root(), &[a, b, c]);
        (t, a, b, c)
    }

    #[test]
    fn group_layers_wraps_siblings_in_one_step() {
        let (mut t, a, b, c) = flat();
        let g = t.group_layers(&[a, b], Layer::group("G"), None, 0).unwrap();

        assert_eq!(t.root(), &[g, c], "the grouped layers left the root");
        assert_eq!(
            t.get(g).unwrap().children(),
            &[a, b],
            "children keep the order they were named in"
        );
        assert_eq!(t.iter_depth_first(), vec![g, a, b, c]);
        assert_eq!(t.len(), 4);
        assert_eq!(t.parent_of(a), Some(g));
        t.validate().unwrap();
    }

    #[test]
    fn group_layers_can_wrap_layers_inside_an_existing_group() {
        let (mut t, outer, a, b, _s) = nested();
        let inner = t
            .group_layers(&[a, b], Layer::group("Inner"), Some(outer), 0)
            .unwrap();
        assert_eq!(t.get(outer).unwrap().children(), &[inner]);
        assert_eq!(t.get(inner).unwrap().children(), &[a, b]);
        assert_eq!(t.depth_of(a), Some(2));
        t.validate().unwrap();
    }

    #[test]
    fn group_layers_with_no_ids_just_inserts_the_empty_group() {
        let (mut t, a, b, c) = flat();
        let g = t.group_layers(&[], Layer::group("G"), None, 1).unwrap();
        assert_eq!(t.root(), &[a, g, b, c]);
        assert!(t.get(g).unwrap().children().is_empty());
        t.validate().unwrap();
    }

    #[test]
    fn every_group_layers_rejection_leaves_the_tree_untouched() {
        let (mut t, a, b, _c) = flat();
        let before = snapshot(&t);
        let ghost = LayerId::new();

        let not_a_group = Layer::raster("Not a group");
        let mut prefilled = Layer::group("Prefilled");
        if let LayerKind::Group(gr) = &mut prefilled.kind {
            gr.children.push(a);
        }

        type Case = (
            TreeError,
            Box<dyn FnOnce(&mut LayerTree) -> Result<LayerId, TreeError>>,
        );
        let cases: Vec<Case> = vec![
            // `group` is not a group at all.
            (
                TreeError::NotAGroup(not_a_group.id),
                Box::new(move |t: &mut LayerTree| t.group_layers(&[a], not_a_group, None, 0)),
            ),
            // `group` already names children.
            (
                TreeError::NotEmpty(prefilled.id),
                Box::new(move |t: &mut LayerTree| t.group_layers(&[a], prefilled, None, 0)),
            ),
            // An id that is not in the tree.
            (
                TreeError::NotFound(ghost),
                Box::new(move |t: &mut LayerTree| {
                    t.group_layers(&[a, ghost], Layer::group("G"), None, 0)
                }),
            ),
            // The same id twice.
            (
                TreeError::DuplicateId(a),
                Box::new(move |t: &mut LayerTree| {
                    t.group_layers(&[a, b, a], Layer::group("G"), None, 0)
                }),
            ),
            // A destination that does not exist.
            (
                TreeError::NotFound(ghost),
                Box::new(move |t: &mut LayerTree| {
                    t.group_layers(&[a], Layer::group("G"), Some(ghost), 0)
                }),
            ),
            // A destination that is not a group.
            (
                TreeError::NotAGroup(b),
                Box::new(move |t: &mut LayerTree| {
                    t.group_layers(&[a], Layer::group("G"), Some(b), 0)
                }),
            ),
        ];

        for (expected, run) in cases {
            assert_eq!(run(&mut t).unwrap_err(), expected);
            assert_eq!(
                snapshot(&t),
                before,
                "rejection for {expected:?} mutated the tree"
            );
            t.validate().unwrap();
        }
    }

    #[test]
    fn group_layers_refuses_ids_that_are_not_siblings() {
        let (mut t, _g, a, _b, s) = nested();
        let before = snapshot(&t);
        // `a` lives in the group; `s` lives at the root.
        assert_eq!(
            t.group_layers(&[a, s], Layer::group("G"), None, 0)
                .unwrap_err(),
            TreeError::NotSiblings { a, b: s }
        );
        assert_eq!(snapshot(&t), before);
        t.validate().unwrap();
    }

    #[test]
    fn group_layers_refuses_to_park_the_group_inside_a_layer_it_swallows() {
        let mut t = LayerTree::new();
        let outer = t.push_root(Layer::group("Outer")).unwrap();
        let mid = t.push_root(Layer::group("Mid")).unwrap();
        t.move_layer(mid, Some(outer), 0).unwrap();
        let before = snapshot(&t);

        // Grouping `outer` into a new group that itself lives inside `mid`
        // would make `outer` its own ancestor.
        assert_eq!(
            t.group_layers(&[outer], Layer::group("G"), Some(mid), 0)
                .unwrap_err(),
            TreeError::WouldCycle {
                moving: outer,
                parent: mid
            }
        );
        assert_eq!(snapshot(&t), before);
        assert_eq!(t.iter_depth_first(), vec![outer, mid]);
        t.validate().unwrap();
    }

    #[test]
    fn insert_at_places_into_a_group_directly() {
        let mut t = LayerTree::new();
        let g = t.push_root(Layer::group("G")).unwrap();
        let a = t.insert_at(Layer::raster("A"), Some(g), 0).unwrap();
        let b = t.insert_at(Layer::raster("B"), Some(g), 99).unwrap();
        assert_eq!(t.get(g).unwrap().children(), &[a, b], "index is clamped");
        assert_eq!(t.iter_depth_first(), vec![g, a, b]);
        t.validate().unwrap();
    }

    #[test]
    fn insert_at_rejects_a_non_group_parent() {
        let mut t = LayerTree::new();
        let r = t.push_root(Layer::raster("R")).unwrap();
        assert_eq!(
            t.insert_at(Layer::raster("X"), Some(r), 0).unwrap_err(),
            TreeError::NotAGroup(r)
        );
        assert_eq!(t.len(), 1);
    }

    // ---- serde --------------------------------------------------------------

    #[test]
    fn nested_tree_serde_roundtrip_preserves_structure_exactly() {
        let mut t = LayerTree::new();
        let bg = t.push_root(Layer::raster("Background")).unwrap();
        let outer = t.push_root(Layer::group("Outer")).unwrap();
        let inner = t.push_root(Layer::group("Inner")).unwrap();
        let leaf1 = t.push_root(Layer::raster("Leaf 1")).unwrap();
        let leaf2 = t.push_root(Layer::raster("Leaf 2")).unwrap();
        let top = t.push_root(Layer::raster("Top")).unwrap();
        t.move_layer(inner, Some(outer), 0).unwrap();
        t.move_layer(leaf1, Some(inner), 0).unwrap();
        t.move_layer(leaf2, Some(inner), 1).unwrap();
        t.get_mut(leaf2).unwrap().clipping = ClippingMode::ClipToBelow;
        t.get_mut(outer).unwrap().opacity = 0.42;

        let expected_order = t.iter_depth_first();
        assert_eq!(
            expected_order,
            vec![top, outer, inner, leaf1, leaf2, bg],
            "sanity: depth-first order before the round trip"
        );

        let json = serde_json::to_string(&t).unwrap();
        let back: LayerTree = serde_json::from_str(&json).unwrap();

        assert_eq!(back.root(), t.root());
        assert_eq!(back.len(), t.len());
        assert_eq!(back.iter_depth_first(), expected_order);
        for id in expected_order {
            assert_eq!(back.get(id), t.get(id), "layer {id} differs after reload");
        }
        assert_eq!(back.parent_of(leaf1), Some(inner));
        assert_eq!(back.parent_of(inner), Some(outer));
        assert_eq!(back.get(outer).unwrap().opacity, 0.42);
        back.validate().unwrap();
    }

    #[test]
    fn a_detached_subtree_survives_the_journal_and_still_reinserts() {
        // A delete's inverse *is* the detached subtree, so it has to make the
        // round trip through the on-disk command journal intact.
        let (mut t, g, a, b, _s) = nested();
        let before = snapshot(&t);
        let sub = t.remove(g).unwrap();

        let json = serde_json::to_string(&sub).unwrap();
        let back: DetachedSubtree = serde_json::from_str(&json).unwrap();
        assert_eq!(back, sub);
        assert_eq!(back.root(), g);
        assert_eq!(back.parent(), sub.parent());
        assert_eq!(back.index(), sub.index());

        t.reinsert(back).unwrap();
        assert_eq!(snapshot(&t), before);
        assert_eq!(t.get(g).unwrap().children(), &[a, b]);
        t.validate().unwrap();
    }

    #[test]
    fn a_hand_edited_journal_cannot_smuggle_in_a_broken_subtree() {
        let (mut t, g, a, _b, _s) = nested();
        let sub = t.remove(g).unwrap();
        let json = serde_json::to_string(&sub).unwrap();

        // Drop child `a`'s layer while `g` still names it. Without the
        // `try_from` shadow this deserializes fine and `reinsert` is handed a
        // subtree naming an id it does not carry.
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut value = value;
        let layers = value["layers"].as_array_mut().unwrap();
        let pos = layers
            .iter()
            .position(|l| l["id"].as_str() == Some(&a.0.to_string()))
            .expect("child A must be in the payload");
        layers.remove(pos);
        let corrupt = serde_json::to_string(&value).unwrap();

        let err = serde_json::from_str::<DetachedSubtree>(&corrupt).unwrap_err();
        assert!(
            err.to_string().contains("does not contain"),
            "unexpected error: {err}"
        );

        // An empty payload is refused too: `layers[0]` has to be the root.
        let empty = format!(
            r#"{{"root":"{}","layers":[],"parent":null,"index":0}}"#,
            g.0
        );
        assert!(serde_json::from_str::<DetachedSubtree>(&empty).is_err());
    }

    #[test]
    fn empty_tree_serde_roundtrip() {
        let t = LayerTree::new();
        let json = serde_json::to_string(&t).unwrap();
        let back: LayerTree = serde_json::from_str(&json).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn a_corrupt_document_fails_to_load_instead_of_crashing_later() {
        // Hand-built payload whose group claims a child that is also at root:
        // two parents for one id.
        let mut t = LayerTree::new();
        t.push_root(Layer::group("G")).unwrap();
        let a = t.push_root(Layer::raster("A")).unwrap();
        let json = serde_json::to_string(&t).unwrap();
        // Splice `a` into G's children while leaving it in `root`.
        let corrupt = json.replace("\"children\":[]", &format!("\"children\":[\"{}\"]", a.0));
        assert_ne!(corrupt, json, "the splice must have applied");
        let err = serde_json::from_str::<LayerTree>(&corrupt).unwrap_err();
        assert!(
            err.to_string().contains("already has a parent"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_document_with_an_unreachable_layer_fails_to_load() {
        let mut t = LayerTree::new();
        let a = t.push_root(Layer::raster("A")).unwrap();
        let json = serde_json::to_string(&t).unwrap();
        let corrupt = json.replace(&format!("\"root\":[\"{}\"]", a.0), "\"root\":[]");
        assert_ne!(corrupt, json);
        let err = serde_json::from_str::<LayerTree>(&corrupt).unwrap_err();
        assert!(err.to_string().contains("not referenced"), "got: {err}");
    }

    // ---- clipping -----------------------------------------------------------

    /// Root stack, top-most first: `[c2, c1, base, other]` where c2 and c1 clip.
    fn clip_stack() -> (LayerTree, LayerId, LayerId, LayerId, LayerId) {
        let mut t = LayerTree::new();
        let other = t.push_root(Layer::raster("Other")).unwrap();
        let base = t.push_root(Layer::raster("Base")).unwrap();
        let c1 = t.push_root(Layer::raster("Clip 1")).unwrap();
        let c2 = t.push_root(Layer::raster("Clip 2")).unwrap();
        t.get_mut(c1).unwrap().clipping = ClippingMode::ClipToBelow;
        t.get_mut(c2).unwrap().clipping = ClippingMode::ClipToBelow;
        assert_eq!(t.root(), &[c2, c1, base, other]);
        (t, c2, c1, base, other)
    }

    #[test]
    fn clipping_group_is_found_from_any_member() {
        let (t, c2, c1, base, _other) = clip_stack();
        let expected = ClippingGroup {
            base,
            clipped: vec![c2, c1],
        };
        assert_eq!(t.clipping_group(base).unwrap(), expected);
        assert_eq!(t.clipping_group(c1).unwrap(), expected);
        assert_eq!(t.clipping_group(c2).unwrap(), expected);
        assert!(t.is_clipped(c1) && t.is_clipped(c2));
        assert!(!t.is_clipped(base), "the base is not itself clipped");
    }

    #[test]
    fn a_layer_outside_the_run_is_not_in_the_group() {
        let (t, _c2, _c1, _base, other) = clip_stack();
        assert!(t.clipping_group(other).is_none());
        assert!(!t.is_clipped(other));
    }

    #[test]
    fn a_clipper_with_nothing_beneath_it_is_not_clipped() {
        let mut t = LayerTree::new();
        let lone = t.push_root(Layer::raster("Lone")).unwrap();
        t.get_mut(lone).unwrap().clipping = ClippingMode::ClipToBelow;
        assert!(t.clipping_group(lone).is_none());
        assert!(!t.is_clipped(lone));
    }

    #[test]
    fn clipping_runs_do_not_cross_group_boundaries() {
        // `inner` sits inside a group; the layer below the *group* is not its
        // sibling, so it cannot be the clipping base.
        let mut t = LayerTree::new();
        let below = t.push_root(Layer::raster("Below")).unwrap();
        let g = t.push_root(Layer::group("G")).unwrap();
        let inner = t.push_root(Layer::raster("Inner")).unwrap();
        t.move_layer(inner, Some(g), 0).unwrap();
        t.get_mut(inner).unwrap().clipping = ClippingMode::ClipToBelow;

        assert!(t.clipping_group(inner).is_none());
        assert!(t.clipping_group(below).is_none());

        // Give `inner` a real sibling base and the group forms.
        let base = t.insert_at(Layer::raster("Base"), Some(g), 1).unwrap();
        assert_eq!(
            t.clipping_group(inner).unwrap(),
            ClippingGroup {
                base,
                clipped: vec![inner]
            }
        );
    }

    #[test]
    fn two_adjacent_clipping_groups_stay_separate() {
        let mut t = LayerTree::new();
        // Bottom-up pushes: root ends up [c2, base2, c1, base1].
        let base1 = t.push_root(Layer::raster("Base 1")).unwrap();
        let c1 = t.push_root(Layer::raster("Clip 1")).unwrap();
        let base2 = t.push_root(Layer::raster("Base 2")).unwrap();
        let c2 = t.push_root(Layer::raster("Clip 2")).unwrap();
        t.get_mut(c1).unwrap().clipping = ClippingMode::ClipToBelow;
        t.get_mut(c2).unwrap().clipping = ClippingMode::ClipToBelow;
        assert_eq!(t.root(), &[c2, base2, c1, base1]);

        assert_eq!(
            t.clipping_group(c2).unwrap(),
            ClippingGroup {
                base: base2,
                clipped: vec![c2]
            }
        );
        assert_eq!(
            t.clipping_group(c1).unwrap(),
            ClippingGroup {
                base: base1,
                clipped: vec![c1]
            }
        );
    }

    #[test]
    fn clipping_group_of_an_unknown_layer_is_none() {
        let t = LayerTree::new();
        assert!(t.clipping_group(LayerId::new()).is_none());
    }

    // ---- misc ---------------------------------------------------------------

    #[test]
    fn a_deeply_nested_document_walks_without_overflowing_the_stack() {
        // Nothing bounds nesting depth but the layer count, and `validate`
        // walks the whole tree on every deserialize — so a legal deep document
        // must be *walkable*, not merely rejectable. A recursive walk aborts
        // the process here (a stack overflow is not a catchable error), which
        // means the file never gets as far as being accepted or refused.
        const DEPTH: usize = 100_000;
        let mut t = LayerTree::new();
        let top = t.push_root(Layer::group("G0")).unwrap();
        let mut parent = top;
        for i in 1..DEPTH {
            parent = t
                .insert_at(Layer::group(format!("G{i}")), Some(parent), 0)
                .unwrap();
        }
        assert_eq!(t.len(), DEPTH);
        // (No `depth_of` here: `parent_of` is a linear scan, so walking one
        // chain of this length with it is quadratic.)
        let order = t.iter_depth_first();
        assert_eq!(order.len(), DEPTH);
        assert_eq!(order[0], top);
        assert_eq!(
            order[DEPTH - 1],
            parent,
            "the walk reaches the deepest leaf"
        );
        assert_eq!(t.subtree_ids(top).len(), DEPTH);
        t.validate().unwrap();
    }

    #[test]
    fn subtree_ids_and_descendant_queries() {
        let (t, g, a, b, s) = nested();
        assert_eq!(t.subtree_ids(g), vec![g, a, b]);
        assert_eq!(t.subtree_ids(a), vec![a]);
        assert!(t.subtree_ids(LayerId::new()).is_empty());
        assert!(t.is_descendant_of(a, g));
        assert!(t.is_descendant_of(g, g));
        assert!(!t.is_descendant_of(g, a));
        assert!(!t.is_descendant_of(s, g));
    }

    #[test]
    fn sibling_and_index_queries() {
        let (t, g, a, b, s) = nested();
        assert_eq!(t.siblings_of(a).unwrap(), &[a, b]);
        assert_eq!(t.index_in_parent(b), Some(1));
        assert_eq!(t.siblings_of(g).unwrap(), t.root());
        assert_eq!(t.index_in_parent(s), Some(1));
        assert!(t.siblings_of(LayerId::new()).is_none());
    }
}
