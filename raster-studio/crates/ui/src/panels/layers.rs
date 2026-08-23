//! The Layers panel: the one users live in.
//!
//! # A flattened tree, not a recursive draw
//!
//! [`LayersModel::build`] walks the document once and produces a flat list of
//! [`LayerRow`]s carrying their own depth, so drawing is a `for` loop and every
//! interesting question — what is at row 7, where would this drop land, is that
//! drop legal — is answered against a `Vec` in a unit test with no window.
//!
//! # Expand state does not live in the document
//!
//! `layer_model::GroupLayer::collapsed` is saved with the file, and it is what
//! a group *starts* folded or unfolded as. But there is no `LayerPatch` field
//! for it, so toggling a twirl-down cannot be a [`Command`] — and it should not
//! be one: folding a group is not an edit and has no business in undo. The
//! panel therefore keeps an override map, seeded from the document and
//! consulted first. See [`LayersState::expanded`].
//!
//! # Drops are validated before they become commands
//!
//! `LayerTree::move_layer` already refuses a cycle, but a refusal that happens
//! *inside* `History::apply` is a failed command the user sees as nothing
//! happening. [`LayersModel::resolve_drop`] answers the same question first, as
//! a [`DropRejection`] the panel can show as a "no drop" cursor, and only
//! produces a [`Command::MoveLayer`] once the move is known to be legal.

use std::collections::{HashMap, HashSet};

use editor_core::{Command, Document, LayerPatch, Patch};
use layer_model::{
    BlendMode, ClippingMode, Layer, LayerId, LayerMask, LayerTree, LockState, MaskId,
};

use crate::menu::LayerClass;

/// One drawable row.
#[derive(Clone, PartialEq, Debug)]
pub struct LayerRow {
    pub id: LayerId,
    /// Nesting depth; `0` for a root layer. Drives the row's indent.
    pub depth: usize,
    pub name: String,
    pub class: LayerClass,
    pub visible: bool,
    /// Effective (clamped) opacity, `0.0..=1.0`.
    pub opacity: f32,
    pub fill_opacity: f32,
    pub blend_mode: BlendMode,
    pub locked: LockState,
    pub has_mask: bool,
    pub mask_enabled: bool,
    pub mask_linked: bool,
    /// How many layer-style slots are filled.
    pub effect_count: usize,
    pub effects_enabled: bool,
    /// This layer clips to the one beneath it.
    pub is_clipping: bool,
    /// Something above this layer clips to it.
    pub is_clip_base: bool,
    pub is_group: bool,
    /// Only meaningful for a group.
    pub expanded: bool,
    pub child_count: usize,
    pub parent: Option<LayerId>,
    pub index_in_parent: usize,
    /// `true` when this row is in the panel's selection.
    pub selected: bool,
    /// `true` when this row is the document's active layer.
    pub active: bool,
}

impl LayerRow {
    /// `true` when the row should show a mask badge.
    pub const fn shows_mask_badge(&self) -> bool {
        self.has_mask
    }

    /// `true` when the row should show an effects badge.
    pub const fn shows_effects_badge(&self) -> bool {
        self.effect_count > 0
    }

    /// `true` when any lock at all is on.
    pub fn shows_lock_badge(&self) -> bool {
        self.locked.any()
    }
}

/// Which panel-owned state the layers panel keeps between frames.
#[derive(Clone, Default, Debug)]
pub struct LayersState {
    /// Rows the user has selected. Order is click order, which is what
    /// "group these" and "delete these" follow.
    selection: Vec<LayerId>,
    /// Overrides of the document's saved collapse state, by layer.
    expanded: HashMap<LayerId, bool>,
    /// The row an in-flight drag started on.
    dragging: Option<LayerId>,
}

impl LayersState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The selected layers, in click order.
    pub fn selection(&self) -> &[LayerId] {
        &self.selection
    }

    pub fn is_selected(&self, id: LayerId) -> bool {
        self.selection.contains(&id)
    }

    /// Replace the selection with a single layer.
    pub fn select_only(&mut self, id: LayerId) {
        self.selection.clear();
        self.selection.push(id);
    }

    /// Add or remove one layer from the selection (a ctrl-click).
    pub fn toggle_selected(&mut self, id: LayerId) {
        match self.selection.iter().position(|x| *x == id) {
            Some(i) => {
                self.selection.remove(i);
            }
            None => self.selection.push(id),
        }
    }

    /// Select every row between the last-clicked one and `id`, inclusive (a
    /// shift-click). `rows` is the visible order.
    pub fn select_range(&mut self, rows: &[LayerRow], id: LayerId) {
        let anchor = self.selection.last().copied();
        let (Some(anchor), Some(to)) = (
            anchor.and_then(|a| rows.iter().position(|r| r.id == a)),
            rows.iter().position(|r| r.id == id),
        ) else {
            self.select_only(id);
            return;
        };
        let (lo, hi) = if anchor <= to {
            (anchor, to)
        } else {
            (to, anchor)
        };
        self.selection = rows[lo..=hi].iter().map(|r| r.id).collect();
        // Keep the clicked row as the anchor for a following shift-click.
        if let Some(i) = self.selection.iter().position(|x| *x == id) {
            let clicked = self.selection.remove(i);
            self.selection.push(clicked);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Drop selected ids that are no longer in the document — after an undo of
    /// a create, or a delete.
    pub fn prune(&mut self, doc: &Document) {
        self.selection.retain(|id| doc.layers.contains(*id));
        self.expanded.retain(|id, _| doc.layers.contains(*id));
        if let Some(id) = self.dragging {
            if !doc.layers.contains(id) {
                self.dragging = None;
            }
        }
    }

    /// Whether a group shows its children, falling back to the document's own
    /// saved collapse flag.
    pub fn is_expanded(&self, tree: &LayerTree, id: LayerId) -> bool {
        if let Some(over) = self.expanded.get(&id) {
            return *over;
        }
        match tree.get(id).map(|l| &l.kind) {
            Some(layer_model::LayerKind::Group(g)) => !g.collapsed,
            _ => true,
        }
    }

    pub fn set_expanded(&mut self, id: LayerId, expanded: bool) {
        self.expanded.insert(id, expanded);
    }

    pub fn dragging(&self) -> Option<LayerId> {
        self.dragging
    }

    pub fn begin_drag(&mut self, id: LayerId) {
        self.dragging = Some(id);
    }

    pub fn end_drag(&mut self) -> Option<LayerId> {
        self.dragging.take()
    }
}

/// Where a dragged row would land.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropPosition {
    /// Immediately above `0`, as its sibling.
    Above(LayerId),
    /// Immediately below `0`, as its sibling.
    Below(LayerId),
    /// Inside `0`, which must be a group, as its first child.
    Into(LayerId),
}

impl DropPosition {
    /// The row the drop is relative to.
    pub const fn anchor(self) -> LayerId {
        match self {
            DropPosition::Above(id) | DropPosition::Below(id) | DropPosition::Into(id) => id,
        }
    }
}

/// Why a drop cannot happen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum DropRejection {
    /// The drop would put a group inside itself or inside one of its own
    /// descendants — the tree would stop being a tree.
    #[error("a group cannot be moved inside itself")]
    IntoOwnDescendant,
    /// `Into` named a layer that is not a group.
    #[error("only a group can hold layers")]
    NotAGroup,
    /// The move would leave the layer exactly where it is.
    #[error("the layer is already there")]
    NoChange,
    /// The dragged layer or the drop anchor is not in the document.
    #[error("the layer is no longer in the document")]
    Missing,
}

/// The flattened tree.
#[derive(Clone, PartialEq, Debug)]
pub struct LayersModel {
    rows: Vec<LayerRow>,
}

impl LayersModel {
    /// Flatten the document's layer tree, top-most first, skipping the children
    /// of collapsed groups.
    pub fn build(doc: &Document, state: &LayersState) -> Self {
        let active = doc.active_layer();
        let mut rows = Vec::with_capacity(doc.layers.len());
        let mut stack: Vec<(LayerId, usize)> = doc
            .layers
            .root()
            .iter()
            .rev()
            .map(|id| (*id, 0usize))
            .collect();
        while let Some((id, depth)) = stack.pop() {
            let Some(layer) = doc.layers.get(id) else {
                continue;
            };
            let expanded = state.is_expanded(&doc.layers, id);
            let children = layer.children();
            rows.push(LayerRow {
                id,
                depth,
                name: layer.name.clone(),
                class: LayerClass::of(&layer.kind),
                visible: layer.visible,
                opacity: layer.effective_opacity(),
                fill_opacity: layer.effective_fill_opacity(),
                blend_mode: layer.blend_mode,
                locked: layer.locked,
                has_mask: layer.mask.is_some(),
                mask_enabled: layer.mask.as_ref().is_some_and(|m| m.enabled),
                mask_linked: layer.mask.as_ref().is_some_and(|m| m.linked),
                effect_count: layer.effects.count(),
                effects_enabled: layer.effects.enabled,
                is_clipping: layer.is_clipping(),
                is_clip_base: is_clip_base(doc, id),
                is_group: layer.is_group(),
                expanded,
                child_count: children.len(),
                parent: doc.layers.parent_of(id),
                index_in_parent: doc.layers.index_in_parent(id).unwrap_or(0),
                selected: state.is_selected(id),
                active: active == Some(id),
            });
            if layer.is_group() && expanded {
                for child in children.iter().rev() {
                    stack.push((*child, depth + 1));
                }
            }
        }
        Self { rows }
    }

    /// The visible rows, top-most first.
    pub fn rows(&self) -> &[LayerRow] {
        &self.rows
    }

    pub fn row(&self, id: LayerId) -> Option<&LayerRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Turn a drop onto a row into the move that performs it.
    ///
    /// Every rejection is decided here, before a command exists — see the
    /// module note on why that matters.
    pub fn resolve_drop(
        doc: &Document,
        dragged: LayerId,
        position: DropPosition,
    ) -> Result<Command, DropRejection> {
        let anchor = position.anchor();
        if !doc.layers.contains(dragged) || !doc.layers.contains(anchor) {
            return Err(DropRejection::Missing);
        }
        let (parent, mut index) = match position {
            DropPosition::Into(id) => {
                if !doc.layers.get(id).is_some_and(Layer::is_group) {
                    return Err(DropRejection::NotAGroup);
                }
                (Some(id), 0usize)
            }
            DropPosition::Above(id) => (
                doc.layers.parent_of(id),
                doc.layers
                    .index_in_parent(id)
                    .ok_or(DropRejection::Missing)?,
            ),
            DropPosition::Below(id) => (
                doc.layers.parent_of(id),
                doc.layers
                    .index_in_parent(id)
                    .ok_or(DropRejection::Missing)?
                    + 1,
            ),
        };

        // The cycle check: a group may not land in itself or under itself.
        if let Some(pid) = parent {
            if pid == dragged || doc.layers.is_descendant_of(pid, dragged) {
                return Err(DropRejection::IntoOwnDescendant);
            }
        }

        let current_parent = doc.layers.parent_of(dragged);
        let current_index = doc
            .layers
            .index_in_parent(dragged)
            .ok_or(DropRejection::Missing)?;

        // `move_layer` detaches before it attaches, so a move *within* one
        // parent has to have the vacated slot taken out of the target index.
        // Without this, "drop just below where I already am" walks the layer
        // one step further down every time.
        if current_parent == parent && current_index < index {
            index -= 1;
        }
        if current_parent == parent && current_index == index {
            return Err(DropRejection::NoChange);
        }

        Ok(Command::MoveLayer {
            layer_id: dragged,
            parent,
            index,
        })
    }

    // ---- command builders ------------------------------------------------

    /// Show or hide a layer.
    pub fn set_visible(id: LayerId, visible: bool) -> Command {
        patch(
            id,
            LayerPatch {
                visible: Some(visible),
                ..Default::default()
            },
        )
    }

    /// Set opacity. `opacity` is clamped into `0.0..=1.0`, and a non-finite
    /// value — which a drag on a zero-width slider can produce — is refused
    /// rather than sent to a command that would reject it.
    pub fn set_opacity(id: LayerId, opacity: f32) -> Option<Command> {
        Some(patch(
            id,
            LayerPatch {
                opacity: Some(finite_unit(opacity)?),
                ..Default::default()
            },
        ))
    }

    pub fn set_fill_opacity(id: LayerId, opacity: f32) -> Option<Command> {
        Some(patch(
            id,
            LayerPatch {
                fill_opacity: Some(finite_unit(opacity)?),
                ..Default::default()
            },
        ))
    }

    pub fn set_blend_mode(id: LayerId, mode: BlendMode) -> Command {
        patch(
            id,
            LayerPatch {
                blend_mode: Some(mode),
                ..Default::default()
            },
        )
    }

    pub fn set_locks(id: LayerId, locked: LockState) -> Command {
        patch(
            id,
            LayerPatch {
                locked: Some(locked),
                ..Default::default()
            },
        )
    }

    pub fn set_clipping(id: LayerId, clipping: bool) -> Command {
        patch(
            id,
            LayerPatch {
                clipping: Some(if clipping {
                    ClippingMode::ClipToBelow
                } else {
                    ClippingMode::None
                }),
                ..Default::default()
            },
        )
    }

    /// Attach an empty reveal-all mask.
    pub fn add_mask(id: LayerId) -> Command {
        patch(
            id,
            LayerPatch {
                mask: Patch::Set(LayerMask::new(MaskId::new())),
                ..Default::default()
            },
        )
    }

    pub fn delete_mask(id: LayerId) -> Command {
        patch(
            id,
            LayerPatch {
                mask: Patch::Clear,
                ..Default::default()
            },
        )
    }

    /// Enable or disable an existing mask, keeping everything else about it.
    ///
    /// `None` when the layer has no mask; the panel does not draw the toggle in
    /// that case, so this is defence against a stale click, not a normal path.
    pub fn set_mask_enabled(doc: &Document, id: LayerId, enabled: bool) -> Option<Command> {
        let mut mask = doc.layers.get(id)?.mask.clone()?;
        if mask.enabled == enabled {
            return None;
        }
        mask.enabled = enabled;
        Some(patch(
            id,
            LayerPatch {
                mask: Patch::Set(mask),
                ..Default::default()
            },
        ))
    }

    /// Link or unlink the mask from the layer's transform.
    pub fn set_mask_linked(doc: &Document, id: LayerId, linked: bool) -> Option<Command> {
        let mut mask = doc.layers.get(id)?.mask.clone()?;
        if mask.linked == linked {
            return None;
        }
        mask.linked = linked;
        Some(patch(
            id,
            LayerPatch {
                mask: Patch::Set(mask),
                ..Default::default()
            },
        ))
    }

    /// Turn a layer's whole style block on or off.
    pub fn set_effects_enabled(doc: &Document, id: LayerId, enabled: bool) -> Option<Command> {
        let layer = doc.layers.get(id)?;
        if layer.effects.enabled == enabled {
            return None;
        }
        let mut effects = layer.effects.clone();
        effects.enabled = enabled;
        Some(patch(
            id,
            LayerPatch {
                effects: Some(Box::new(effects)),
                ..Default::default()
            },
        ))
    }

    /// Rename a layer. An all-whitespace name is refused — a nameless row is
    /// unclickable in a list of nameless rows.
    pub fn rename(id: LayerId, name: &str) -> Option<Command> {
        let trimmed = name.trim();
        (!trimmed.is_empty()).then(|| {
            patch(
                id,
                LayerPatch {
                    name: Some(trimmed.to_string()),
                    ..Default::default()
                },
            )
        })
    }

    /// Delete every selected layer, as one undo step.
    ///
    /// Deleting a group takes its subtree with it, so a selection holding both
    /// a group and one of its children would delete the child twice. The
    /// descendants are dropped from the batch first.
    pub fn delete_selection(doc: &Document, selection: &[LayerId]) -> Option<Command> {
        let roots = topmost_only(doc, selection);
        match roots.len() {
            0 => None,
            1 => Some(Command::DeleteLayer { layer_id: roots[0] }),
            _ => Some(Command::Transaction {
                label: "Delete Layers".to_string(),
                commands: roots
                    .into_iter()
                    .map(|layer_id| Command::DeleteLayer { layer_id })
                    .collect(),
            }),
        }
    }

    /// Add an empty raster layer above everything.
    pub fn new_layer(doc: &Document) -> Command {
        Command::create_layer(Layer::raster(format!("Layer {}", doc.layers.len() + 1)))
    }

    /// Add an empty group at the root.
    pub fn new_group() -> Command {
        Command::create_layer(Layer::group("Group"))
    }
}

/// A property patch on one layer.
fn patch(layer_id: LayerId, patch: LayerPatch) -> Command {
    Command::SetLayerProperties { layer_id, patch }
}

/// `v` clamped into `0.0..=1.0`, or `None` if it is not a finite number.
fn finite_unit(v: f32) -> Option<f32> {
    v.is_finite().then(|| v.clamp(0.0, 1.0))
}

/// `true` when the layer directly above `id` clips to it.
fn is_clip_base(doc: &Document, id: LayerId) -> bool {
    let Some(index) = doc.layers.index_in_parent(id) else {
        return false;
    };
    let Some(siblings) = doc.layers.siblings_of(id) else {
        return false;
    };
    index
        .checked_sub(1)
        .and_then(|above| siblings.get(above))
        .and_then(|above| doc.layers.get(*above))
        .is_some_and(Layer::is_clipping)
}

/// Drop any id in `ids` that lives beneath another id in `ids`, keeping
/// document order.
fn topmost_only(doc: &Document, ids: &[LayerId]) -> Vec<LayerId> {
    let set: HashSet<LayerId> = ids.iter().copied().collect();
    let mut out: Vec<LayerId> = doc
        .layers
        .iter_depth_first()
        .into_iter()
        .filter(|id| set.contains(id))
        .filter(|id| {
            !set.iter()
                .any(|other| other != id && doc.layers.is_descendant_of(*id, *other))
        })
        .collect();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::History;
    use layer_model::{GlowEffect, LayerEffects};

    /// ```text
    /// Top          (raster, root 0)
    /// Group        (group,  root 1)
    ///   Child A    (raster, group 0)
    ///   Inner      (group,  group 1)
    ///     Deep     (raster, inner 0)
    /// Bottom       (raster, root 2)
    /// ```
    struct Fixture {
        doc: Document,
        top: LayerId,
        group: LayerId,
        child_a: LayerId,
        inner: LayerId,
        deep: LayerId,
        bottom: LayerId,
    }

    fn fixture() -> Fixture {
        // `push_root` inserts at the *top*, so building a fixture with it
        // reverses the stack. Append explicitly so the z-order in the test
        // reads the way the panel draws it.
        let mut doc = Document::new(64, 64, "Test");
        let top = doc.layers.insert_at(Layer::raster("Top"), None, 0).unwrap();
        let group = doc
            .layers
            .insert_at(Layer::group("Group"), None, 1)
            .unwrap();
        let bottom = doc
            .layers
            .insert_at(Layer::raster("Bottom"), None, 2)
            .unwrap();
        let child_a = doc
            .layers
            .insert_at(Layer::raster("Child A"), Some(group), 0)
            .unwrap();
        let inner = doc
            .layers
            .insert_at(Layer::group("Inner"), Some(group), 1)
            .unwrap();
        let deep = doc
            .layers
            .insert_at(Layer::raster("Deep"), Some(inner), 0)
            .unwrap();
        Fixture {
            doc,
            top,
            group,
            child_a,
            inner,
            deep,
            bottom,
        }
    }

    fn names(model: &LayersModel) -> Vec<&str> {
        model.rows().iter().map(|r| r.name.as_str()).collect()
    }

    // ---- flattening -------------------------------------------------------

    #[test]
    fn the_tree_flattens_top_most_first_with_depths() {
        let f = fixture();
        let state = LayersState::new();
        let m = LayersModel::build(&f.doc, &state);
        assert_eq!(
            names(&m),
            vec!["Top", "Group", "Child A", "Inner", "Deep", "Bottom"]
        );
        let depths: Vec<usize> = m.rows().iter().map(|r| r.depth).collect();
        assert_eq!(depths, vec![0, 0, 1, 1, 2, 0]);
    }

    #[test]
    fn a_collapsed_group_hides_its_whole_subtree() {
        let f = fixture();
        let mut state = LayersState::new();
        state.set_expanded(f.group, false);
        let m = LayersModel::build(&f.doc, &state);
        assert_eq!(names(&m), vec!["Top", "Group", "Bottom"]);
        assert!(!m.row(f.group).unwrap().expanded);

        // Re-expanding brings them back, and collapsing only the inner group
        // hides only its own child.
        state.set_expanded(f.group, true);
        state.set_expanded(f.inner, false);
        let m = LayersModel::build(&f.doc, &state);
        assert_eq!(
            names(&m),
            vec!["Top", "Group", "Child A", "Inner", "Bottom"]
        );
    }

    #[test]
    fn the_documents_saved_collapse_flag_is_the_default() {
        let mut f = fixture();
        if let Some(layer_model::LayerKind::Group(g)) =
            f.doc.layers.get_mut(f.group).map(|l| &mut l.kind)
        {
            g.collapsed = true;
        }
        let state = LayersState::new();
        assert_eq!(
            names(&LayersModel::build(&f.doc, &state)),
            ["Top", "Group", "Bottom"]
        );

        // ...and the panel's own override wins over it.
        let mut state = LayersState::new();
        state.set_expanded(f.group, true);
        assert_eq!(
            names(&LayersModel::build(&f.doc, &state)),
            ["Top", "Group", "Child A", "Inner", "Deep", "Bottom"]
        );
    }

    #[test]
    fn a_row_reports_its_badges() {
        let mut f = fixture();
        {
            let layer = f.doc.layers.get_mut(f.top).unwrap();
            layer.mask = Some(LayerMask::new(MaskId::new()));
            layer.effects = LayerEffects {
                outer_glow: Some(GlowEffect::default()),
                ..LayerEffects::default()
            };
            layer.locked = LockState {
                pixels: true,
                ..LockState::default()
            };
        }
        let m = LayersModel::build(&f.doc, &LayersState::new());
        let row = m.row(f.top).unwrap();
        assert!(row.shows_mask_badge());
        assert!(row.mask_enabled);
        assert!(row.shows_effects_badge());
        assert_eq!(row.effect_count, 1);
        assert!(row.shows_lock_badge());

        let plain = m.row(f.bottom).unwrap();
        assert!(!plain.shows_mask_badge());
        assert!(!plain.shows_effects_badge());
        assert!(!plain.shows_lock_badge());
    }

    #[test]
    fn the_clipping_indicator_names_both_ends_of_the_pair() {
        let mut f = fixture();
        f.doc.layers.get_mut(f.top).unwrap().clipping = ClippingMode::ClipToBelow;
        let m = LayersModel::build(&f.doc, &LayersState::new());
        assert!(m.row(f.top).unwrap().is_clipping);
        // `Group` is directly below `Top`, so it is the clip base.
        assert!(m.row(f.group).unwrap().is_clip_base);
        assert!(!m.row(f.bottom).unwrap().is_clip_base);
        assert!(!m.row(f.top).unwrap().is_clip_base);
    }

    #[test]
    fn an_out_of_range_opacity_in_the_document_is_shown_clamped() {
        let mut f = fixture();
        f.doc.layers.get_mut(f.top).unwrap().opacity = 4.0;
        let m = LayersModel::build(&f.doc, &LayersState::new());
        assert_eq!(m.row(f.top).unwrap().opacity, 1.0);
    }

    // ---- drag and drop ----------------------------------------------------

    #[test]
    fn dragging_a_group_into_its_own_child_is_rejected_before_any_command() {
        let f = fixture();
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.group, DropPosition::Into(f.inner)),
            Err(DropRejection::IntoOwnDescendant)
        );
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.group, DropPosition::Above(f.deep)),
            Err(DropRejection::IntoOwnDescendant)
        );
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.group, DropPosition::Into(f.group)),
            Err(DropRejection::IntoOwnDescendant)
        );
    }

    #[test]
    fn the_rejected_drop_is_one_the_tree_would_also_have_refused() {
        // Belt and braces: prove the panel and the tree agree about what is
        // illegal, so the pre-check is not merely a different opinion.
        let mut f = fixture();
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.group, DropPosition::Into(f.inner)),
            Err(DropRejection::IntoOwnDescendant)
        );
        let err = f
            .doc
            .layers
            .move_layer(f.group, Some(f.inner), 0)
            .expect_err("the tree must refuse this too");
        assert!(matches!(err, layer_model::TreeError::WouldCycle { .. }));
    }

    #[test]
    fn dropping_into_a_non_group_is_rejected() {
        let f = fixture();
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.top, DropPosition::Into(f.bottom)),
            Err(DropRejection::NotAGroup)
        );
    }

    #[test]
    fn dropping_where_the_layer_already_is_changes_nothing() {
        let f = fixture();
        // `Top` is root index 0; dropping it above itself is where it is.
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.top, DropPosition::Above(f.top)),
            Err(DropRejection::NoChange)
        );
        // ...and dropping it just above `Group` is the same slot again.
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.top, DropPosition::Above(f.group)),
            Err(DropRejection::NoChange)
        );
    }

    #[test]
    fn a_drop_into_a_group_becomes_a_move_to_its_first_slot() {
        let f = fixture();
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.top, DropPosition::Into(f.group)),
            Ok(Command::MoveLayer {
                layer_id: f.top,
                parent: Some(f.group),
                index: 0,
            })
        );
    }

    #[test]
    fn a_drop_below_a_row_lands_beneath_it_in_that_rows_parent() {
        let f = fixture();
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.top, DropPosition::Below(f.child_a)),
            Ok(Command::MoveLayer {
                layer_id: f.top,
                parent: Some(f.group),
                index: 1,
            })
        );
    }

    #[test]
    fn moving_down_within_one_parent_accounts_for_the_slot_it_vacates() {
        // `Top` is root 0; dropping it below `Group` (root 1) must land it at
        // root index 1, not 2 — otherwise it walks past `Bottom`.
        let mut f = fixture();
        let command = LayersModel::resolve_drop(&f.doc, f.top, DropPosition::Below(f.group))
            .expect("legal move");
        assert_eq!(
            command,
            Command::MoveLayer {
                layer_id: f.top,
                parent: None,
                index: 1,
            }
        );
        let mut history = History::new();
        history.apply(&mut f.doc, command).expect("apply");
        assert_eq!(f.doc.layers.root(), &[f.group, f.top, f.bottom]);
    }

    #[test]
    fn moving_up_within_one_parent_does_not_shift_the_index() {
        let mut f = fixture();
        let command = LayersModel::resolve_drop(&f.doc, f.bottom, DropPosition::Above(f.group))
            .expect("legal move");
        assert_eq!(
            command,
            Command::MoveLayer {
                layer_id: f.bottom,
                parent: None,
                index: 1,
            }
        );
        let mut history = History::new();
        history.apply(&mut f.doc, command).expect("apply");
        assert_eq!(f.doc.layers.root(), &[f.top, f.bottom, f.group]);
    }

    #[test]
    fn a_re_parenting_drop_actually_applies() {
        let mut f = fixture();
        let command = LayersModel::resolve_drop(&f.doc, f.bottom, DropPosition::Into(f.inner))
            .expect("legal move");
        let mut history = History::new();
        history.apply(&mut f.doc, command).expect("apply");
        assert_eq!(f.doc.layers.parent_of(f.bottom), Some(f.inner));
        assert_eq!(f.doc.layers.index_in_parent(f.bottom), Some(0));
        assert_eq!(f.doc.layers.root(), &[f.top, f.group]);
    }

    #[test]
    fn a_drop_naming_a_layer_that_is_gone_is_rejected() {
        let mut f = fixture();
        let ghost = f.doc.layers.remove(f.bottom).unwrap().root();
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, ghost, DropPosition::Above(f.top)),
            Err(DropRejection::Missing)
        );
        assert_eq!(
            LayersModel::resolve_drop(&f.doc, f.top, DropPosition::Above(ghost)),
            Err(DropRejection::Missing)
        );
    }

    #[test]
    fn every_rejection_says_something() {
        for r in [
            DropRejection::IntoOwnDescendant,
            DropRejection::NotAGroup,
            DropRejection::NoChange,
            DropRejection::Missing,
        ] {
            assert!(!r.to_string().is_empty(), "{r:?}");
        }
    }

    // ---- command emission -------------------------------------------------

    #[test]
    fn the_visibility_toggle_emits_a_patch_of_the_opposite_value() {
        let f = fixture();
        assert_eq!(
            LayersModel::set_visible(f.top, false),
            Command::SetLayerProperties {
                layer_id: f.top,
                patch: LayerPatch {
                    visible: Some(false),
                    ..Default::default()
                }
            }
        );
    }

    #[test]
    fn opacity_and_blend_changes_emit_patches_that_apply() {
        let mut f = fixture();
        let mut history = History::new();
        history
            .apply(
                &mut f.doc,
                LayersModel::set_opacity(f.top, 0.42).expect("finite"),
            )
            .expect("apply");
        history
            .apply(
                &mut f.doc,
                LayersModel::set_blend_mode(f.top, BlendMode::Multiply),
            )
            .expect("apply");
        let layer = f.doc.layers.get(f.top).unwrap();
        assert!((layer.opacity - 0.42).abs() < 1e-6);
        assert_eq!(layer.blend_mode, BlendMode::Multiply);

        // ...and undo puts both back.
        history.undo(&mut f.doc).unwrap();
        history.undo(&mut f.doc).unwrap();
        let layer = f.doc.layers.get(f.top).unwrap();
        assert_eq!(layer.opacity, 1.0);
        assert_eq!(layer.blend_mode, BlendMode::Normal);
    }

    #[test]
    fn an_out_of_range_opacity_is_clamped_and_a_nan_emits_nothing() {
        let f = fixture();
        let Some(Command::SetLayerProperties { patch, .. }) = LayersModel::set_opacity(f.top, 5.0)
        else {
            panic!("expected a patch");
        };
        assert_eq!(patch.opacity, Some(1.0));
        assert!(LayersModel::set_opacity(f.top, f32::NAN).is_none());
        assert!(LayersModel::set_fill_opacity(f.top, f32::INFINITY).is_none());
    }

    #[test]
    fn the_lock_toggles_emit_the_whole_lock_state() {
        let f = fixture();
        let locks = LockState {
            pixels: true,
            position: true,
            ..LockState::default()
        };
        assert_eq!(
            LayersModel::set_locks(f.top, locks),
            Command::SetLayerProperties {
                layer_id: f.top,
                patch: LayerPatch {
                    locked: Some(locks),
                    ..Default::default()
                }
            }
        );
    }

    #[test]
    fn the_mask_toggle_keeps_everything_else_about_the_mask() {
        let mut f = fixture();
        let mut mask = LayerMask::new(MaskId::new());
        mask.set_feather_px(3.5).unwrap();
        mask.inverted = true;
        let mask_id = mask.id;
        f.doc.layers.get_mut(f.top).unwrap().mask = Some(mask);

        let command = LayersModel::set_mask_enabled(&f.doc, f.top, false).expect("has a mask");
        let mut history = History::new();
        history.apply(&mut f.doc, command).expect("apply");
        let after = f.doc.layers.get(f.top).unwrap().mask.as_ref().unwrap();
        assert!(!after.enabled);
        assert_eq!(after.id, mask_id);
        assert_eq!(after.feather_px(), 3.5);
        assert!(after.inverted);

        // Setting it to what it already is emits nothing.
        assert!(LayersModel::set_mask_enabled(&f.doc, f.top, false).is_none());
        // A layer with no mask emits nothing either.
        assert!(LayersModel::set_mask_enabled(&f.doc, f.bottom, false).is_none());
    }

    #[test]
    fn the_effects_toggle_keeps_the_effects_themselves() {
        let mut f = fixture();
        f.doc.layers.get_mut(f.top).unwrap().effects = LayerEffects {
            outer_glow: Some(GlowEffect::default()),
            ..LayerEffects::default()
        };
        let command = LayersModel::set_effects_enabled(&f.doc, f.top, false).expect("has effects");
        let mut history = History::new();
        history.apply(&mut f.doc, command).expect("apply");
        let effects = &f.doc.layers.get(f.top).unwrap().effects;
        assert!(!effects.enabled);
        assert_eq!(effects.count(), 1);
        assert!(!effects.affects_composite());
    }

    #[test]
    fn renaming_refuses_an_empty_name() {
        let f = fixture();
        assert!(LayersModel::rename(f.top, "   ").is_none());
        let Some(Command::SetLayerProperties { patch, .. }) = LayersModel::rename(f.top, "  Sky  ")
        else {
            panic!("expected a patch");
        };
        assert_eq!(patch.name.as_deref(), Some("Sky"));
    }

    #[test]
    fn deleting_a_multi_selection_is_one_transaction() {
        let f = fixture();
        let Some(Command::Transaction { label, commands }) =
            LayersModel::delete_selection(&f.doc, &[f.top, f.bottom])
        else {
            panic!("expected a transaction");
        };
        assert_eq!(label, "Delete Layers");
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn deleting_a_group_and_its_child_deletes_the_group_once() {
        let mut f = fixture();
        // `deep` lives under `inner`, which lives under `group`.
        let command = LayersModel::delete_selection(&f.doc, &[f.deep, f.group, f.inner])
            .expect("something to delete");
        assert_eq!(command, Command::DeleteLayer { layer_id: f.group });
        let mut history = History::new();
        history.apply(&mut f.doc, command).expect("apply");
        assert!(!f.doc.layers.contains(f.deep));
        assert!(!f.doc.layers.contains(f.inner));
        assert_eq!(f.doc.layers.root(), &[f.top, f.bottom]);
    }

    #[test]
    fn deleting_an_empty_selection_emits_nothing() {
        let f = fixture();
        assert!(LayersModel::delete_selection(&f.doc, &[]).is_none());
    }

    #[test]
    fn a_new_layer_is_named_after_the_count() {
        let f = fixture();
        let Command::CreateLayer { layer } = LayersModel::new_layer(&f.doc) else {
            panic!("expected a create");
        };
        assert_eq!(layer.name, format!("Layer {}", f.doc.layers.len() + 1));
    }

    // ---- selection --------------------------------------------------------

    #[test]
    fn clicking_replaces_the_selection_and_ctrl_clicking_extends_it() {
        let f = fixture();
        let mut state = LayersState::new();
        state.select_only(f.top);
        assert_eq!(state.selection(), &[f.top]);
        state.toggle_selected(f.bottom);
        assert_eq!(state.selection(), &[f.top, f.bottom]);
        state.toggle_selected(f.top);
        assert_eq!(state.selection(), &[f.bottom]);
        state.select_only(f.group);
        assert_eq!(state.selection(), &[f.group]);
    }

    #[test]
    fn shift_clicking_selects_the_run_between_the_two_rows() {
        let f = fixture();
        let mut state = LayersState::new();
        let rows = LayersModel::build(&f.doc, &state);
        state.select_only(f.top);
        state.select_range(rows.rows(), f.inner);
        let mut selected = state.selection().to_vec();
        selected.sort_by_key(|id| {
            rows.rows()
                .iter()
                .position(|r| r.id == *id)
                .expect("selected row is visible")
        });
        assert_eq!(selected, vec![f.top, f.group, f.child_a, f.inner]);
        // The clicked row stays the anchor, so a second shift-click extends
        // from there rather than from where the run started.
        assert_eq!(state.selection().last(), Some(&f.inner));
    }

    #[test]
    fn shift_clicking_with_nothing_selected_selects_the_one_row() {
        let f = fixture();
        let mut state = LayersState::new();
        let rows = LayersModel::build(&f.doc, &state);
        state.select_range(rows.rows(), f.deep);
        assert_eq!(state.selection(), &[f.deep]);
    }

    #[test]
    fn the_selection_drops_layers_that_left_the_document() {
        let mut f = fixture();
        let mut state = LayersState::new();
        state.select_only(f.top);
        state.toggle_selected(f.bottom);
        state.set_expanded(f.group, false);
        state.begin_drag(f.bottom);

        f.doc.layers.remove(f.bottom).unwrap();
        state.prune(&f.doc);
        assert_eq!(state.selection(), &[f.top]);
        assert_eq!(state.dragging(), None);
    }

    #[test]
    fn the_model_marks_the_selected_and_the_active_row() {
        let mut f = fixture();
        f.doc.set_active_layer(Some(f.child_a)).unwrap();
        let mut state = LayersState::new();
        state.select_only(f.top);
        state.toggle_selected(f.child_a);
        let m = LayersModel::build(&f.doc, &state);
        assert!(m.row(f.top).unwrap().selected);
        assert!(!m.row(f.top).unwrap().active);
        assert!(m.row(f.child_a).unwrap().selected);
        assert!(m.row(f.child_a).unwrap().active);
        assert!(!m.row(f.bottom).unwrap().selected);
    }

    #[test]
    fn an_empty_document_has_no_rows() {
        let doc = Document::new(8, 8, "Empty");
        let m = LayersModel::build(&doc, &LayersState::new());
        assert!(m.is_empty());
        assert!(m.rows().is_empty());
    }
}
