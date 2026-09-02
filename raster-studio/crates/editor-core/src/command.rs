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
//! journal stays cheap. See [`crate::pixels`] for the tile-delta shape every
//! pixel edit takes.
//!
//! # Atomicity
//! **Every** variant is all-or-nothing: on `Err` the document is byte-identical
//! to what it was before the call. Single commands achieve that by validating
//! before mutating; [`Command::Transaction`] achieves it by rolling back the
//! members that already succeeded. [`crate::History`] depends on this — it
//! records an entry only on success, so a command that half-applied would leave
//! a mutation nothing can undo.
//!
//! There is exactly one exception, and it announces itself:
//! [`CommandError::RollbackFailed`] is returned when a transaction member failed
//! *and* undoing the members that had already applied failed too. Then the
//! document is left wherever the rollback stopped — the error says so, and the
//! caller must reload instead of continuing to edit. Every other `Err` from
//! [`Command::apply`] leaves the document untouched.
//!
//! Keeping that exception unreachable in practice is why an *inverse* may never
//! be refusable: a recorded entry whose undo cannot apply is the same
//! "mutation with no way back" in slow motion. Two rules follow from it — no
//! command may insert an already fully-locked layer (see
//! [`CommandError::CannotInsertLocked`]), and an inverse captured from a
//! corrupt document normalizes what it captures (see
//! [`Command::SetLayerProperties`]).

use glam::Affine2;
use serde::{Deserialize, Serialize};

use layer_model::{
    BlendMode, ClippingMode, DetachedSubtree, Layer, LayerEffects, LayerId, LayerKind, LayerMask,
    LockState,
};
use raster::PixelRect;

use crate::document::{Document, Guides};
use crate::pixels::{
    pixel_rect_serde, tile_intersects_region, tiles_covering, Coverage, FillValue, PixelError,
    PixelKey, PixelTarget, TileDelta, TileEdit,
};

/// A patch field for a value that can be *absent*.
///
/// [`LayerPatch`] uses `Option<T>` for fields that always have a value, where
/// `None` can safely mean "unchanged". That encoding cannot express clearing a
/// nullable field: `mask: None` would be indistinguishable from "leave the mask
/// alone". A dedicated three-state enum is the fix, and it keeps the inverse
/// exact — the inverse of `Set` on a layer that had no mask is `Clear`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum Patch<T> {
    /// Leave the field as it is.
    #[default]
    Keep,
    /// Replace the field with this value.
    Set(T),
    /// Remove the field's value.
    Clear,
}

impl<T> Patch<T> {
    pub fn is_keep(&self) -> bool {
        matches!(self, Patch::Keep)
    }

    /// The patch that restores `current`.
    fn restoring(current: Option<T>) -> Self {
        match current {
            Some(v) => Patch::Set(v),
            None => Patch::Clear,
        }
    }
}

/// A patch of optional layer properties. `None` fields are left unchanged;
/// nullable fields use [`Patch`] because `None` is already spoken for.
///
/// Covers every field of [`layer_model::Layer`] except `id` (identity is not
/// editable) and `kind` (changing a layer's kind is a different operation than
/// changing its properties — it would have to move pixel and child ownership).
/// That coverage claim is checked, not asserted: the field list is destructured
/// exhaustively in `a_patch_covers_every_editable_layer_field` and in
/// `touches_more_than_lock_state`, so adding a field to `Layer` or to this
/// struct fails to compile until it is handled here.
///
/// Every field defaults, so a patch written by an older build still
/// deserializes from a version-1 or -2 journal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerPatch {
    pub name: Option<String>,
    pub visible: Option<bool>,
    /// Must be finite and within `0.0..=1.0`; see [`CommandError::InvalidOpacity`].
    pub opacity: Option<f32>,
    /// Same range rule as `opacity`.
    pub fill_opacity: Option<f32>,
    pub blend_mode: Option<BlendMode>,
    pub locked: Option<LockState>,
    pub clipping: Option<ClippingMode>,
    /// Photopea's link chain. Absolute per layer, like `visible`.
    pub linked: Option<bool>,
    /// The layer's whole layer-to-document transform, as the 6 affine
    /// components. Absolute, not a delta — [`Command::TransformLayer`] is the
    /// relative operation. Must be finite.
    ///
    /// Because it is absolute, this field is a way to *move* a layer, and it is
    /// refused on a position-locked layer exactly as
    /// [`Command::TransformLayer`] is. Without that check the lock would be
    /// bypassable by composing the matrix caller-side.
    pub transform: Option<[f32; 6]>,
    /// Attach, replace, or detach the layer's mask.
    pub mask: Patch<LayerMask>,
    /// The whole layer-style block, replaced wholesale — this is how drop
    /// shadow, stroke and the overlays are edited and undone.
    ///
    /// Its numeric fields are **not** range-checked by this command; they are
    /// `layer_model`'s clamp-at-read contract. See [`LayerPatch::validate`].
    ///
    /// Boxed for the same reason [`Command::CreateLayer`] boxes its layer: a
    /// [`LayerEffects`] is by far the largest thing a layer owns, and inlining
    /// it here would set the size of every `Command`, including the ones a
    /// brush stroke emits by the hundred. `Box<T>` serializes exactly like `T`.
    pub effects: Option<Box<LayerEffects>>,
}

impl LayerPatch {
    /// Reject an out-of-range `opacity` or `fill_opacity` and a non-finite
    /// `transform`. Runs before any mutation, so a patch carrying one bad field
    /// changes nothing at all.
    ///
    /// **Those three fields and no others.** `effects` is replaced wholesale
    /// with no numeric check, deliberately: `layer_model` classifies every
    /// effect parameter as *expected*-range rather than enforced and defines the
    /// contract as clamp-at-read (`layer_model::blend::unit` is the shared
    /// clamp; see the "Numeric ranges" section of that crate's docs). Duplicating
    /// the ranges here would put the same rule in two places and let them drift.
    /// So a patch *can* write a NaN shadow `spread` into the document, and the
    /// compositor is the layer that neutralizes it — pinned by
    /// `an_effect_numeric_is_layer_models_contract_not_this_commands`.
    ///
    /// The three that are checked here are checked because they are the ones
    /// this command writes as raw numbers a consumer multiplies directly, and
    /// because a non-finite `transform` also has to be refused *before* it can
    /// reach the undo entry.
    fn validate(&self) -> Result<(), CommandError> {
        for v in [self.opacity, self.fill_opacity].into_iter().flatten() {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(CommandError::InvalidOpacity(v));
            }
        }
        if let Some(m) = self.transform {
            if !m.iter().all(|v| v.is_finite()) {
                return Err(CommandError::InvalidTransform(m));
            }
        }
        Ok(())
    }

    /// `true` when this patch changes anything other than the lock flags.
    ///
    /// A layer that is fully locked (`LockState::all`) both before and after
    /// the patch refuses every such patch. Two carve-outs, both about keeping
    /// the lock escapable and the history sound: a patch that touches `locked`
    /// alone always applies (or a layer locked by mistake could never be
    /// unlocked through the command system, which is where undo lives), and a
    /// patch that releases the lock may carry other fields with it (that is the
    /// shape of the inverse of "edit, then lock").
    ///
    /// Destructured exhaustively on purpose — a new [`LayerPatch`] field will
    /// not compile until it is classified here, so the lock cannot be bypassed
    /// by a field somebody forgot to list.
    fn touches_more_than_lock_state(&self) -> bool {
        let LayerPatch {
            name,
            visible,
            opacity,
            fill_opacity,
            blend_mode,
            locked: _,
            clipping,
            linked,
            transform,
            mask,
            effects,
        } = self;
        name.is_some()
            || visible.is_some()
            || opacity.is_some()
            || fill_opacity.is_some()
            || blend_mode.is_some()
            || clipping.is_some()
            || linked.is_some()
            || transform.is_some()
            || !mask.is_keep()
            || effects.is_some()
    }
}

/// The complete, versioned set of editing operations.
///
/// Adding a variant is a format change — bump `DOCUMENT_FORMAT_VERSION` and add
/// a migration if older journals must still replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Add a layer at the document root.
    ///
    /// Refuses a payload that is already fully locked
    /// ([`CommandError::CannotInsertLocked`]): this command's inverse is a
    /// [`Command::DeleteLayer`], which the blanket lock refuses, so such a
    /// create would be an edit with no way back. Paste, duplicate and import of
    /// a locked layer therefore create it unlocked and lock it in the same
    /// [`Command::Transaction`].
    CreateLayer {
        /// Boxed deliberately, as is [`LayerPatch::effects`]. A [`Layer`]
        /// carries its whole effect block and is several times larger than this
        /// entire enum, so inlining it would set the size of *every* `Command`,
        /// including the tiny ones a brush stroke emits by the hundred.
        /// `Box<T>` serializes exactly like `T`, so the journal's wire format is
        /// unaffected. The size *relationship* is pinned by
        /// `boxing_keeps_a_command_far_smaller_than_a_layer` rather than quoted
        /// here as a byte count, which is the form that goes stale. Build one
        /// with [`Command::create_layer`].
        layer: Box<Layer>,
    },
    /// Remove a layer and, if it is a group, its whole subtree. Refused when
    /// the layer or any descendant is fully locked ([`LockState::all`], which
    /// `layer_model` documents as covering deletion).
    DeleteLayer { layer_id: LayerId },
    /// The inverse of [`Command::DeleteLayer`]: put a detached subtree back
    /// exactly where it came from.
    ///
    /// Deleting a group takes its whole subtree with it, so undo cannot be
    /// "re-create one layer, then move it" — that would restore the group and
    /// silently lose its children. The payload carries every removed layer plus
    /// the original parent and index, and [`layer_model::LayerTree::reinsert`]
    /// restores the position, which is why this variant needs no follow-up
    /// move.
    ///
    /// Like [`Command::CreateLayer`] it refuses to insert a fully locked layer
    /// ([`CommandError::CannotInsertLocked`]), anywhere in the subtree. A
    /// payload built by [`Command::DeleteLayer`] never contains one, since the
    /// delete refused the lock in the first place; only an untrusted journal
    /// can carry one here.
    RestoreLayers { subtree: DetachedSubtree },
    /// Re-parent or re-order a layer within the tree.
    ///
    /// Deliberately **not** gated on [`LockState`]: the locks guard a layer's
    /// pixels and its position *on the canvas*, not its position in the layer
    /// list. Restacking a locked layer moves no pixel, and forbidding it would
    /// make a locked layer impossible to organize.
    MoveLayer {
        layer_id: LayerId,
        parent: Option<LayerId>,
        index: usize,
    },
    /// Change layer properties through a [`LayerPatch`].
    ///
    /// Lock-aware, and it has to be: `patch.transform` is an absolute matrix, so
    /// an unguarded patch would move a position-locked layer. A fully locked
    /// layer ([`LockState::all`]) refuses every patch except one that touches
    /// nothing but `locked` itself, which is how a lock is released.
    ///
    /// # Undo of a corrupt prior value
    /// `opacity`, `fill_opacity` and `transform` are range-checked on the way in
    /// ([`LayerPatch::validate`]) but are public, unvalidated fields on
    /// [`Layer`], so a hand-edited or corrupt document can hold `2.0`, or a NaN
    /// matrix. The inverse this command captures is applied through that same
    /// validation, so a raw capture would fail its own `validate()` and the edit
    /// would be permanently un-undoable. The capture is therefore sanitized —
    /// and what that costs differs between the two halves:
    ///
    /// * `opacity` and `fill_opacity` are captured through
    ///   [`Layer::effective_opacity`] / [`Layer::effective_fill_opacity`].
    ///   `layer_model` puts both fields in its clamp-at-read group and those
    ///   methods *are* that read, so undo restores exactly the value the
    ///   compositor was already using.
    /// * `transform` is in no such group: `layer_model` defines no
    ///   `effective_transform`, and `blend::unit` cannot clamp a matrix. A
    ///   non-finite prior matrix is therefore **repaired, not restored** — it
    ///   comes back as the identity (see `restorable_transform`). A layer whose
    ///   matrix was NaN drew nothing before the edit and is visible, at the
    ///   origin, after the undo. That is the deliberate price of keeping the
    ///   edit undoable at all, since a non-finite matrix cannot go into an
    ///   inverse patch that `validate` will accept.
    ///
    /// Pinned by `an_edit_to_a_corrupt_layer_is_still_undoable`.
    SetLayerProperties {
        layer_id: LayerId,
        patch: LayerPatch,
    },
    /// Replace the document's pixel selection wholesale. The inverse carries
    /// the selection that was there, so scaling or moving a selection — which
    /// is a direct field write everywhere else — becomes one undoable step
    /// when a gizmo commits it.
    SetSelection {
        selection: crate::selection::Selection,
    },
    /// Flip the document's colour mode (Image ▸ Mode ▸ …). The inverse
    /// carries the mode that was there, so the mode rides the same undo step
    /// as the pixel rewrite it accompanies instead of drifting after an undo.
    SetMetaColorMode { from: u8, to: u8 },
    TransformLayer {
        layer_id: LayerId,
        /// **Pre**-multiplied onto the layer's current transform:
        /// `new = delta * current`.
        ///
        /// The layer's own transform maps layer space to document space, so a
        /// delta on the left acts in *document* space — which is what a canvas
        /// gizmo produces, and what makes "drag 10px right" mean 10 document
        /// pixels regardless of how the layer is already rotated or scaled.
        matrix: [f32; 6],
    },
    /// Replace a set of tiles on a layer or mask.
    ///
    /// This is the pixel edit: brush strokes, erases, clone/heal results,
    /// pasted content, an AI generation's output. The whole stroke is one
    /// command with one [`TileDelta`], however many tiles it crossed, so it is
    /// one undo step. The inverse carries the hashes those tiles held before,
    /// so undo restores them exactly rather than re-rasterizing anything.
    PaintTiles {
        target: PixelTarget,
        delta: TileDelta,
    },
    /// Flood a rectangle with one solid value: a color on a layer, a coverage
    /// sample on a mask.
    ///
    /// `delta` is authoritative — it is what actually gets applied, and its
    /// inverse is a [`Command::PaintTiles`] holding the previous hashes.
    /// `rect` and `value` are recorded so the journal describes the *intent*, so
    /// a replay whose tile blobs are missing can re-rasterize, and so apply can
    /// check that a fill only ever touches tiles inside its own region. Build
    /// one with [`Command::fill_region`], which derives the interior tiles.
    ///
    /// The value's kind must match the target's storage format; see
    /// [`FillValue`].
    FillRegion {
        target: PixelTarget,
        #[serde(with = "pixel_rect_serde")]
        rect: PixelRect,
        value: FillValue,
        delta: TileDelta,
    },
    /// Erase a rectangle of a **layer** back to nothing. Same shape as
    /// [`Command::FillRegion`] with no value: fully-covered tiles are *removed*
    /// rather than filled with transparent pixels, so a cleared area costs no
    /// storage.
    ///
    /// Layer targets only. Removing a mask tile does not erase anything — it
    /// reads as zero coverage, i.e. the layer fully hidden (see
    /// [`crate::pixels`]) — so a mask target is
    /// [`CommandError::CannotClearMask`]. Reveal through a mask with a
    /// `FillRegion` of [`crate::pixels::MaskCoverage::REVEALED`] instead.
    ClearRegion {
        target: PixelTarget,
        #[serde(with = "pixel_rect_serde")]
        rect: PixelRect,
        delta: TileDelta,
    },
    /// Replace a layer's **kind payload** in place: an adjustment's parameters,
    /// a text layer's content and styling, a shape's path, a group's blending.
    ///
    /// This is the edit every slider in the Properties panel makes, and until
    /// this variant existed it made none: [`LayerPatch`] deliberately covers
    /// every field of a [`Layer`] *except* `kind`, so an adjustment layer's
    /// parameters had no command behind them at all. The panel re-reads the
    /// value from the document each frame, so the knob visibly sprang back the
    /// instant the pointer was released, and an adjustment created at identity
    /// — Curves, Levels — could never be made to change anything.
    ///
    /// # It cannot change a layer's *class*
    ///
    /// `kind` must be the same variant the layer already holds, or this is
    /// [`CommandError::CannotChangeLayerClass`]. That is the reason
    /// `LayerPatch` left `kind` out in the first place: turning a group into a
    /// raster layer would orphan its children and turning a raster layer into a
    /// group would invent them. Editing the payload *within* a class moves no
    /// ownership, which is what makes it safe to do here.
    ///
    /// # A group keeps the children the tree says it has
    ///
    /// [`layer_model::LayerTree`] is the authority on which ids a group owns,
    /// and [`layer_model::GroupLayer::children`] is that record. A payload
    /// arriving here therefore has its `children` **ignored** in favour of the
    /// list already in the document, so editing a group's blending mode cannot
    /// duplicate or strand a subtree. Pinned by
    /// `editing_a_groups_blending_cannot_rewrite_its_children`.
    ///
    /// # Wire format
    ///
    /// Purely additive. Every entry an earlier build wrote is one of the older
    /// variants and still deserializes byte for byte, so no journal replay
    /// breaks and no document migration is involved — which is why
    /// [`crate::DOCUMENT_FORMAT_VERSION`] is unchanged.
    SetLayerKind {
        layer_id: LayerId,
        /// Boxed for the reason [`Command::CreateLayer`]'s layer is: a
        /// [`LayerKind`] carrying a full Curves or text run dwarfs the small
        /// commands a stroke emits by the hundred, and `Box<T>` serializes
        /// exactly like `T`.
        kind: Box<LayerKind>,
    },
    /// Resize the canvas — the rectangle every export, composite and selection
    /// is measured against ([`crate::DocumentMeta::size`]).
    ///
    /// This is the command a **crop** is built from, and it exists because
    /// there was no way to express one: `tools::CropRequest` documented "not a
    /// [`Command`], because `editor-core` has no canvas-resize command yet", so
    /// a crop could not go through [`crate::History`] and therefore could not
    /// be undone. It carries only the size; moving the pixels under the new
    /// origin is a [`Command::TransformLayer`] per layer, and a crop is the
    /// [`Command::Transaction`] of the two — which is what makes the whole crop
    /// one undo step.
    ///
    /// The inverse is this same command holding the size the document had, so
    /// undo restores it exactly. A size this build cannot serve is refused
    /// ([`CommandError::CanvasTooLarge`]) *before* anything is written, on the
    /// same terms [`crate::Document`] refuses one at load.
    ///
    /// # Wire format
    ///
    /// Purely additive, like [`Command::SetLayerKind`]: every entry an earlier
    /// build wrote is one of the older variants and still deserializes, so
    /// [`crate::DOCUMENT_FORMAT_VERSION`] is unchanged.
    SetCanvasSize { size: glam::UVec2 },
    /// Image ▸ Image Size: resample every pixel target and resize the canvas
    /// as **one** undoable step.
    ///
    /// The pixel work cannot live in `editor-core` — the tile *bytes* live in
    /// the application's store, and this crate only ever sees hashes — so the
    /// caller rasterizes and supplies, per target, the **complete** new tile
    /// map: a [`TileDelta`] with a [`TileEdit`] for every coordinate the new
    /// canvas covers (a `None` hash removes a tile, which is how a shrink
    /// drops the tiles past the new edge).
    ///
    /// Apply replaces each target's map, moves [`crate::DocumentMeta::size`],
    /// and returns the inverse: a [`Command::Transaction`] that puts every old
    /// map back and restores the old size — byte-exact, because the inverse
    /// carries the old hashes and the store still holds their bytes. Locked
    /// layers are resampled like any other: a pixel lock guards a layer
    /// against *edits*, and a document-level resize is not an edit.
    ///
    /// A size this build cannot serve is refused
    /// ([`CommandError::CanvasTooLarge`]) before anything is written, and a
    /// target that no longer exists is refused rather than skipped, so a
    /// stale caller cannot half-resample a document.
    ///
    /// # Wire format
    ///
    /// Purely additive, like [`Command::SetCanvasSize`].
    ResampleImage {
        size: glam::UVec2,
        changes: Vec<(PixelTarget, TileDelta)>,
    },
    /// Replace the whole document guide set (add, move, remove, or flip the
    /// group visibility/lock all land here as one undoable step per gesture).
    /// The inverse captures the previous set, so undo restores it exactly.
    SetGuides { guides: Guides },
    /// A batch of commands applied atomically (import, AI result, flatten...).
    /// Its inverse is the reversed inverses of its members.
    Transaction {
        label: String,
        commands: Vec<Command>,
    },
}

/// The class of a layer kind, as a word an error message can use.
///
/// A discriminant comparison answers "is this the same class?"; this answers
/// "which classes were they?", which is what makes the refusal readable.
pub fn layer_class_name(kind: &LayerKind) -> &'static str {
    match kind {
        LayerKind::Raster(_) => "raster",
        LayerKind::Group(_) => "group",
        LayerKind::Adjustment(_) => "adjustment",
        LayerKind::Text(_) => "text",
        LayerKind::Shape(_) => "shape",
        LayerKind::SmartObject(_) => "smart object",
        LayerKind::Generator(_) => "generator",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("layer {0} not found")]
    LayerNotFound(LayerId),
    #[error("layer tree error: {0}")]
    Tree(#[from] layer_model::tree::TreeError),
    #[error("command is not invertible without pre-apply capture")]
    NotInvertible,
    #[error("opacity {0} is not a finite value within 0.0..=1.0")]
    InvalidOpacity(f32),
    #[error("transform {0:?} is not finite")]
    InvalidTransform([f32; 6]),
    #[error("layer {0} has no mask to edit")]
    NoMask(LayerId),
    #[error("layer {0} is locked against this edit")]
    LayerLocked(LayerId),
    /// An insertion carried a layer that is already fully locked
    /// ([`LockState::all`]).
    ///
    /// The inverse of an insertion is a deletion, and the blanket lock refuses
    /// deletion — so the insertion would be recorded in [`crate::History`] with
    /// an undo that can never apply, and inside a [`Command::Transaction`] it
    /// would turn a rollback into [`CommandError::RollbackFailed`]. Refusing at
    /// apply time keeps the failure loud and keeps the lock meaningful: nothing
    /// is recorded, so there is nothing to undo.
    ///
    /// The supported shape is a [`Command::Transaction`] of "create it
    /// unlocked, then lock it": that undoes as one step, because a transaction
    /// applies its inverses newest-first and the unlock therefore runs before
    /// the delete.
    #[error(
        "layer {0} cannot be inserted already fully locked: the lock refuses the deletion \
         that is this command's own inverse, so the edit could never be undone; insert it \
         unlocked and lock it in the same transaction instead"
    )]
    CannotInsertLocked(LayerId),
    /// A pixel edit named a layer whose kind owns no pixels of its own. See
    /// [`PixelTarget::Layer`] for which kinds are addressable and why.
    #[error("layer {layer} is a {kind} layer and owns no pixels of its own")]
    NotPaintable { layer: LayerId, kind: &'static str },
    #[error(
        "layer {0}'s mask cannot be cleared: an absent mask tile reads as zero coverage \
         (the layer hidden), not as 'nothing'; fill an explicit coverage instead"
    )]
    CannotClearMask(LayerId),
    #[error("fill value does not match its target's storage format")]
    FillValueMismatch,
    /// A [`Command::SetLayerKind`] carried a payload of a different class from
    /// the one the layer already is.
    ///
    /// Not a near-miss to be papered over: a class change moves pixel and child
    /// ownership, and this command moves neither. Refusing keeps the tree and
    /// the tile store consistent, and keeps the command's inverse — the
    /// previous payload, of the same class — exactly applicable.
    #[error(
        "layer {layer} is a {from} layer; a payload of class {to} cannot replace its own, \
         because changing a layer's class would move pixel and child ownership"
    )]
    CannotChangeLayerClass {
        layer: LayerId,
        from: &'static str,
        to: &'static str,
    },
    /// A [`Command::SetCanvasSize`] named a canvas this build cannot serve.
    ///
    /// The same predicate the loader applies ([`crate::canvas_size_is_supported`]),
    /// so a resize can never leave a document that would be refused when it is
    /// reopened.
    #[error(
        "a {width} x {height} canvas is larger than this build serves \
         ({max_dimension} px per side, {max_pixels} px total)",
        max_dimension = crate::MAX_CANVAS_DIMENSION,
        max_pixels = crate::MAX_CANVAS_PIXELS
    )]
    CanvasTooLarge { width: u32, height: u32 },
    #[error(transparent)]
    Pixel(#[from] PixelError),
    /// A transaction member failed *and* restoring the members that had already
    /// applied failed too. The document is left in whatever state the rollback
    /// reached; the caller must reload rather than continue editing.
    #[error("`{label}` failed ({cause}) and could not be rolled back: {rollback}")]
    RollbackFailed {
        label: String,
        cause: Box<CommandError>,
        rollback: Box<CommandError>,
    },
}

impl Command {
    /// Build a [`Command::CreateLayer`] without the caller having to know the
    /// payload is boxed.
    pub fn create_layer(layer: Layer) -> Self {
        Command::CreateLayer {
            layer: Box::new(layer),
        }
    }

    /// Build a [`Command::PaintTiles`] from loose edits, sorting them and
    /// refusing a repeated coordinate.
    pub fn paint_tiles(
        target: PixelTarget,
        edits: impl IntoIterator<Item = TileEdit>,
    ) -> Result<Self, CommandError> {
        Ok(Command::PaintTiles {
            target,
            delta: TileDelta::new(edits)?,
        })
    }

    /// Build a [`Command::FillRegion`].
    ///
    /// Tiles the rect covers *entirely* are derived here: a solid tile is
    /// content-addressable without reading anything, so the caller does not
    /// have to rasterize the interior of a fill. Tiles the rect only *clips*
    /// depend on the pixels already there, so the caller — which owns the bytes
    /// — supplies their results in `edges`; an entry there overrides a derived
    /// one, which is what a fill constrained by a feathered selection needs. An
    /// entry that does not touch `rect` is refused.
    ///
    /// # A rect with no interior fills nothing
    /// A tile the rect only clips and that `edges` does not name is **left
    /// alone** — deriving a solid tile for it would destroy the pixels outside
    /// the rect. A rect smaller than one tile has no interior at all, so a fill
    /// over it with empty `edges` is a successful command with an empty delta
    /// that changes nothing. That is deliberate (the alternative is corrupting
    /// pixels the user did not select) but it is a real trap, so
    /// [`crate::pixels::edge_tiles`] returns exactly the coordinates a caller
    /// has to rasterize and pass in to make a fill reach the edges of its rect.
    /// Pinned by `a_fill_smaller_than_one_tile_resolves_to_nothing_without_edges`.
    ///
    /// `value` must match the target's storage format
    /// ([`CommandError::FillValueMismatch`]): a mask stores 8-bit coverage, not
    /// RGBA pixels.
    pub fn fill_region(
        target: PixelTarget,
        rect: PixelRect,
        value: impl Into<FillValue>,
        edges: impl IntoIterator<Item = TileEdit>,
    ) -> Result<Self, CommandError> {
        let value = value.into();
        if !value.matches(target) {
            return Err(CommandError::FillValueMismatch);
        }
        let solid = value.solid_tile_hash();
        let delta = region_delta(rect, edges, |_| Some(solid))?;
        Ok(Command::FillRegion {
            target,
            rect,
            value,
            delta,
        })
    }

    /// Build a [`Command::ClearRegion`], on the same terms as
    /// [`Command::fill_region`] — except that a fully covered tile is removed
    /// rather than replaced, and only a [`PixelTarget::Layer`] may be cleared
    /// (see the variant's documentation).
    pub fn clear_region(
        target: PixelTarget,
        rect: PixelRect,
        edges: impl IntoIterator<Item = TileEdit>,
    ) -> Result<Self, CommandError> {
        if let PixelTarget::Mask(id) = target {
            return Err(CommandError::CannotClearMask(id));
        }
        let delta = region_delta(rect, edges, |_| None)?;
        Ok(Command::ClearRegion {
            target,
            rect,
            delta,
        })
    }

    /// Apply this command to the document, returning the command that would
    /// undo it. Capturing the inverse *during* apply (when we can read the
    /// pre-state) is what makes undo exact.
    ///
    /// On `Err` the document is unchanged — see the module's atomicity note.
    /// The single exception is [`CommandError::RollbackFailed`], which reports
    /// that the guarantee could not be kept.
    pub fn apply(&self, doc: &mut Document) -> Result<Command, CommandError> {
        match self {
            Command::CreateLayer { layer } => {
                // A layer that arrives already fully locked cannot be created:
                // this command's inverse is a `DeleteLayer`, and the blanket
                // lock refuses deletion, so the entry `History` recorded could
                // never be undone. Checked before the insert, so the refusal
                // changes nothing.
                if layer.locked.all {
                    return Err(CommandError::CannotInsertLocked(layer.id));
                }
                // `push_root` refuses a duplicate id rather than pushing a
                // second reference into `root`; that refusal has to reach the
                // caller, or a replayed journal quietly grows a corrupt tree.
                let id = doc.layers.push_root(layer.as_ref().clone())?;
                Ok(Command::DeleteLayer { layer_id: id })
            }

            Command::SetSelection { selection } => {
                let previous = doc.selection.clone();
                // A selection that names no pixel is a state, not an error.
                doc.selection = selection.clone();
                Ok(Command::SetSelection {
                    selection: previous,
                })
            }

            Command::SetMetaColorMode { from, to } => {
                doc.meta.color_mode = *to;
                Ok(Command::SetMetaColorMode {
                    from: *to,
                    to: *from,
                })
            }

            Command::DeleteLayer { layer_id } => {
                // The blanket lock covers deletion. Checked across the whole
                // subtree, before anything is removed: deleting a group takes
                // its children with it, so a locked child would disappear
                // through its parent otherwise.
                for id in doc.layers.subtree_ids(*layer_id) {
                    if doc.layers.get(id).is_some_and(|l| l.locked.all) {
                        return Err(CommandError::LayerLocked(id));
                    }
                }
                // `remove` hands back the whole subtree, including the original
                // parent and index, so the inverse is a single reinsert.
                let subtree = doc.layers.remove(*layer_id)?;
                Ok(Command::RestoreLayers { subtree })
            }

            Command::RestoreLayers { subtree } => {
                let id = subtree.root();
                // Same rule as `CreateLayer`, for the same reason, across the
                // whole subtree — a locked *child* would block the delete that
                // undoes this restore just as surely as a locked root. An
                // inverse produced by `DeleteLayer` can never trip this (the
                // delete already refused a locked subtree); a hand-written or
                // corrupted journal can, and a journal is untrusted input.
                for l in subtree.layers() {
                    if l.locked.all {
                        return Err(CommandError::CannotInsertLocked(l.id));
                    }
                }
                doc.layers.reinsert(subtree.clone())?;
                Ok(Command::DeleteLayer { layer_id: id })
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
                // Validate the whole patch first: a patch with a good name and
                // a NaN opacity must not rename the layer on its way to the
                // error.
                patch.validate()?;
                {
                    // Locks, before any mutation. `transform` here is the
                    // *absolute* matrix, so leaving it unguarded would let any
                    // caller move a position-locked layer by composing the
                    // matrix itself — which would make the identical check in
                    // `TransformLayer` enforce nothing.
                    //
                    // A lock blocks a field only when it holds both *before*
                    // and *after* the patch. A patch that releases the lock may
                    // carry the edit it enables, because that is exactly what
                    // the inverse of "edit, then lock" looks like — under a
                    // before-only rule that inverse would be refused and the
                    // edit would become un-undoable. It grants no new power
                    // either: releasing a lock is always allowed, so the same
                    // result is reachable as two commands.
                    let layer = doc
                        .layers
                        .get(*layer_id)
                        .ok_or(CommandError::LayerNotFound(*layer_id))?;
                    let before = layer.locked;
                    let after = patch.locked.unwrap_or(before);
                    let position_locked = before.blocks_transform() && after.blocks_transform();
                    let fully_locked = before.all && after.all;
                    if (position_locked && patch.transform.is_some())
                        || (fully_locked && patch.touches_more_than_lock_state())
                    {
                        return Err(CommandError::LayerLocked(*layer_id));
                    }
                }
                let layer = doc
                    .layers
                    .get_mut(*layer_id)
                    .ok_or(CommandError::LayerNotFound(*layer_id))?;
                // Build the inverse patch from current values before mutating.
                //
                // The three range-checked fields are captured through their
                // *effective* value, not the raw one. `Layer::opacity`,
                // `fill_opacity` and `transform` are public and unvalidated —
                // `layer_model` defines their contract as clamp-at-read — so a
                // hand-edited or corrupt document can hold `2.0` or a NaN. The
                // inverse is applied through this same function, and
                // `LayerPatch::validate` would refuse such a value: capturing
                // the raw one would make the edit permanently un-undoable. So
                // undo of an edit to a corrupt layer restores the value the
                // compositor was already using. See the variant's "Undo
                // normalizes" note.
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
                    inverse.opacity = Some(layer.effective_opacity());
                    layer.opacity = v;
                }
                if let Some(v) = patch.fill_opacity {
                    inverse.fill_opacity = Some(layer.effective_fill_opacity());
                    layer.fill_opacity = v;
                }
                if let Some(v) = patch.blend_mode {
                    inverse.blend_mode = Some(layer.blend_mode);
                    layer.blend_mode = v;
                }
                if let Some(v) = patch.locked {
                    inverse.locked = Some(layer.locked);
                    layer.locked = v;
                }
                if let Some(v) = patch.clipping {
                    inverse.clipping = Some(layer.clipping);
                    layer.clipping = v;
                }
                if let Some(v) = patch.linked {
                    inverse.linked = Some(layer.linked);
                    layer.linked = v;
                }
                if let Some(v) = patch.transform {
                    inverse.transform = Some(restorable_transform(layer.transform));
                    layer.transform = Affine2::from_cols_array(&v);
                }
                match &patch.mask {
                    Patch::Keep => {}
                    Patch::Set(m) => {
                        inverse.mask = Patch::restoring(layer.set_mask(m.clone()));
                    }
                    Patch::Clear => {
                        inverse.mask = Patch::restoring(layer.mask.take());
                    }
                }
                if let Some(v) = &patch.effects {
                    inverse.effects = Some(Box::new(layer.effects.clone()));
                    layer.effects = v.as_ref().clone();
                }
                Ok(Command::SetLayerProperties {
                    layer_id: *layer_id,
                    patch: inverse,
                })
            }

            Command::SetLayerKind { layer_id, kind } => {
                let layer = doc
                    .layers
                    .get(*layer_id)
                    .ok_or(CommandError::LayerNotFound(*layer_id))?;
                // The blanket lock is the one that bites: an adjustment's
                // parameters are neither pixels nor a position, so the narrower
                // locks have nothing to say about them.
                if layer.locked.all {
                    return Err(CommandError::LayerLocked(*layer_id));
                }
                if std::mem::discriminant(&layer.kind) != std::mem::discriminant(kind.as_ref()) {
                    return Err(CommandError::CannotChangeLayerClass {
                        layer: *layer_id,
                        from: layer_class_name(&layer.kind),
                        to: layer_class_name(kind),
                    });
                }
                // Everything above only read; nothing has been mutated yet, so
                // each refusal leaves the document exactly as it was.
                let layer = doc
                    .layers
                    .get_mut(*layer_id)
                    .ok_or(CommandError::LayerNotFound(*layer_id))?;
                let mut next = (**kind).clone();
                // The tree owns group membership, not the payload travelling
                // through this command. Taking the children from the document
                // rather than from `kind` is what stops a blending-mode edit —
                // or a hand-written journal — from duplicating or losing a
                // subtree.
                if let (LayerKind::Group(before), LayerKind::Group(after)) =
                    (&layer.kind, &mut next)
                {
                    after.children.clone_from(&before.children);
                }
                let previous = std::mem::replace(&mut layer.kind, next);
                Ok(Command::SetLayerKind {
                    layer_id: *layer_id,
                    kind: Box::new(previous),
                })
            }

            Command::TransformLayer { layer_id, matrix } => {
                // Invert *first*. A drag that collapses a layer to zero width
                // is a legitimate gesture, and `Affine2::inverse` answers it
                // with NaNs; storing those in the undo entry would poison the
                // layer the moment the user pressed ctrl+Z.
                let inv = invert_delta(matrix)?;
                let layer = doc
                    .layers
                    .get_mut(*layer_id)
                    .ok_or(CommandError::LayerNotFound(*layer_id))?;
                if layer.locked.blocks_transform() {
                    return Err(CommandError::LayerLocked(*layer_id));
                }
                layer.transform = Affine2::from_cols_array(matrix) * layer.transform;
                Ok(Command::TransformLayer {
                    layer_id: *layer_id,
                    matrix: inv.to_cols_array(),
                })
            }

            Command::PaintTiles { target, delta } => {
                let key = resolve_target(doc, *target)?;
                let prev = doc.pixels.apply(key, delta);
                Ok(Command::PaintTiles {
                    target: *target,
                    delta: prev,
                })
            }

            Command::FillRegion {
                target,
                rect,
                value,
                delta,
            } => {
                // Re-checked here and not only in the constructor: a journal is
                // untrusted input, and a coverage hash stored into a layer (or
                // an RGBA hash into a mask) is a tile the compositor would read
                // in the wrong format.
                if !value.matches(*target) {
                    return Err(CommandError::FillValueMismatch);
                }
                let key = resolve_target(doc, *target)?;
                check_delta_in_region(*rect, delta)?;
                let prev = doc.pixels.apply(key, delta);
                // Undoing a fill is not another fill — it is a restore of the
                // exact tiles the fill replaced.
                Ok(Command::PaintTiles {
                    target: *target,
                    delta: prev,
                })
            }

            Command::ClearRegion {
                target,
                rect,
                delta,
            } => {
                if let PixelTarget::Mask(id) = target {
                    return Err(CommandError::CannotClearMask(*id));
                }
                let key = resolve_target(doc, *target)?;
                check_delta_in_region(*rect, delta)?;
                let prev = doc.pixels.apply(key, delta);
                Ok(Command::PaintTiles {
                    target: *target,
                    delta: prev,
                })
            }

            Command::SetCanvasSize { size } => {
                // Refused before anything is written, so the document is
                // unchanged on `Err` like every other arm. The same predicate
                // the loader uses, so a resize cannot reach a size a reopen
                // would reject.
                if !crate::canvas_size_is_supported(size.x, size.y) {
                    return Err(CommandError::CanvasTooLarge {
                        width: size.x,
                        height: size.y,
                    });
                }
                let previous = doc.meta.size;
                doc.meta.size = *size;
                Ok(Command::SetCanvasSize { size: previous })
            }

            Command::ResampleImage { size, changes } => {
                if !crate::canvas_size_is_supported(size.x, size.y) {
                    return Err(CommandError::CanvasTooLarge {
                        width: size.x,
                        height: size.y,
                    });
                }
                let mut inverses = Vec::with_capacity(changes.len());
                for (target, delta) in changes {
                    let key = resolve_pixel_key(doc, *target)?;
                    let previous = doc.pixels.apply(key, delta);
                    inverses.push((*target, previous));
                }
                let previous_size = doc.meta.size;
                doc.meta.size = *size;
                // Undo restores the pixels first and the size last; the two
                // are independent, but restoring pixels then re-resizing keeps
                // the inverse's own inverse (the redo entry) in the same
                // shape as this command.
                let mut commands: Vec<Command> = inverses
                    .into_iter()
                    .map(|(target, delta)| Command::PaintTiles { target, delta })
                    .collect();
                commands.push(Command::SetCanvasSize {
                    size: previous_size,
                });
                Ok(Command::Transaction {
                    label: "Image Size".to_string(),
                    commands,
                })
            }

            Command::SetGuides { guides } => {
                let previous = std::mem::replace(&mut doc.guides, guides.clone());
                Ok(Command::SetGuides { guides: previous })
            }

            Command::Transaction { label, commands } => {
                let mut inverses: Vec<Command> = Vec::with_capacity(commands.len());
                for c in commands {
                    match c.apply(doc) {
                        Ok(inv) => inverses.push(inv),
                        Err(cause) => {
                            // Put the document back. Newest first, because each
                            // inverse assumes the ones after it are still
                            // applied.
                            for undo in inverses.iter().rev() {
                                if let Err(rollback) = undo.apply(doc) {
                                    return Err(CommandError::RollbackFailed {
                                        label: label.clone(),
                                        cause: Box::new(cause),
                                        rollback: Box::new(rollback),
                                    });
                                }
                            }
                            return Err(cause);
                        }
                    }
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
            Command::RestoreLayers { .. } => "Restore Layer".into(),
            Command::MoveLayer { .. } => "Move Layer".into(),
            Command::SetLayerProperties { .. } => "Change Layer Properties".into(),
            // Named after the class rather than "Change Layer Kind", because
            // the history panel's row is what the user reads back: "Edit
            // Adjustment" is the step they took, and the class is the only
            // thing that distinguishes it from an edit to a text run.
            Command::SetLayerKind { kind, .. } => {
                let class = layer_class_name(kind);
                let mut label = String::from("Edit ");
                let mut chars = class.chars();
                if let Some(first) = chars.next() {
                    label.extend(first.to_uppercase());
                    label.push_str(chars.as_str());
                }
                label
            }
            Command::TransformLayer { .. } => "Transform Layer".into(),
            Command::SetSelection { .. } => "Transform Selection".into(),
            Command::SetMetaColorMode { .. } => "Change Colour Mode".into(),
            Command::PaintTiles { target, .. } => match target {
                PixelTarget::Layer(_) => "Paint".into(),
                PixelTarget::Mask(_) => "Paint Mask".into(),
            },
            Command::FillRegion { target, .. } => match target {
                PixelTarget::Layer(_) => "Fill".into(),
                PixelTarget::Mask(_) => "Fill Mask".into(),
            },
            Command::ClearRegion { .. } => "Clear".into(),
            Command::SetCanvasSize { .. } => "Resize Canvas".into(),
            Command::ResampleImage { .. } => "Image Size".into(),
            Command::SetGuides { .. } => "Edit Guides".into(),
            Command::Transaction { label, .. } => label.clone(),
        }
    }
}

/// The matrix an undo should restore for a layer that currently holds
/// `current`.
///
/// Normally `current` itself. `Layer::transform` is public and unvalidated, so a
/// corrupt document can hold a NaN or an infinity there; that matrix cannot go
/// into an inverse patch, because [`LayerPatch::validate`] refuses it and the
/// edit would become un-undoable. The identity is the substitution — the only
/// matrix that is certainly usable, and no worse than the non-finite one it
/// replaces, which maps every point to NaN and draws nothing.
fn restorable_transform(current: Affine2) -> [f32; 6] {
    let cols = current.to_cols_array();
    if cols.iter().all(|v| v.is_finite()) {
        cols
    } else {
        Affine2::IDENTITY.to_cols_array()
    }
}

/// Invert a transform delta, or refuse it.
///
/// Refuses a non-finite matrix, a singular one (a drag to zero width or
/// height), and one whose inverse overflows to infinity — every case where the
/// captured undo entry would otherwise carry NaN or ±inf into the layer.
fn invert_delta(matrix: &[f32; 6]) -> Result<Affine2, CommandError> {
    if !matrix.iter().all(|v| v.is_finite()) {
        return Err(CommandError::NotInvertible);
    }
    let delta = Affine2::from_cols_array(matrix);
    let det = delta.matrix2.determinant();
    if !det.is_finite() || det == 0.0 {
        return Err(CommandError::NotInvertible);
    }
    let inv = delta.inverse();
    if !inv.to_cols_array().iter().all(|v| v.is_finite()) {
        return Err(CommandError::NotInvertible);
    }
    Ok(inv)
}

/// Whether a layer kind owns a tile map of its own; `Err` carries the name of
/// the kind that does not.
///
/// [`layer_model::LayerKind::Raster`] holds painted pixels and
/// [`layer_model::LayerKind::Generator`] holds an AI operation's rasterized
/// output, which the user retouches in place. Every other kind derives its
/// appearance from something else, so a tile stored under one would never be
/// read by the compositor and would never be swept up either —
/// [`crate::PixelStore::retain_referenced`] asks only whether the layer still
/// exists. Matched exhaustively: a new [`layer_model::LayerKind`] fails to
/// compile until it is classified here.
fn kind_owning_pixels(kind: &LayerKind) -> Result<(), &'static str> {
    match kind {
        LayerKind::Raster(_) | LayerKind::Generator(_) | LayerKind::SmartObject(_) => Ok(()),
        LayerKind::Group(_) => Err("group"),
        LayerKind::Adjustment(_) => Err("adjustment"),
        LayerKind::Text(_) => Err("text"),
        LayerKind::Shape(_) => Err("shape"),
    }
}

/// Resolve a pixel target the way [`resolve_target`] does, but without the
/// lock check: a document-level rewrite (Image ▸ Image Size) moves every
/// layer's pixels, including the locked ones — a pixel lock guards a layer
/// against *edits*, not against the document changing size under it.
pub fn resolve_pixel_key(doc: &Document, target: PixelTarget) -> Result<PixelKey, CommandError> {
    match target {
        PixelTarget::Layer(id) => {
            let layer = doc.layers.get(id).ok_or(CommandError::LayerNotFound(id))?;
            if let Err(kind) = kind_owning_pixels(&layer.kind) {
                return Err(CommandError::NotPaintable { layer: id, kind });
            }
            Ok(PixelKey::Layer(id))
        }
        PixelTarget::Mask(id) => {
            // Existence still matters (a mask deleted between the caller's
            // rasterization and this apply must not leave an orphan tile map);
            // only the lock check is dropped.
            let layer = doc.layers.get(id).ok_or(CommandError::LayerNotFound(id))?;
            layer
                .mask_id()
                .map(PixelKey::Mask)
                .ok_or(CommandError::NoMask(id))
        }
    }
}

/// Resolve a pixel target against the document, refusing edits the document
/// forbids: a missing layer, a kind that owns no pixels, a missing mask, or a
/// lock. Runs before any mutation, so every refusal here leaves the document
/// untouched.
pub fn resolve_target(doc: &Document, target: PixelTarget) -> Result<PixelKey, CommandError> {
    match target {
        PixelTarget::Layer(id) => {
            let layer = doc.layers.get(id).ok_or(CommandError::LayerNotFound(id))?;
            if let Err(kind) = kind_owning_pixels(&layer.kind) {
                return Err(CommandError::NotPaintable { layer: id, kind });
            }
            if layer.locked.blocks_pixel_edit() {
                return Err(CommandError::LayerLocked(id));
            }
            Ok(PixelKey::Layer(id))
        }
        PixelTarget::Mask(id) => {
            let layer = doc.layers.get(id).ok_or(CommandError::LayerNotFound(id))?;
            // No kind check: any layer may carry a mask, and a masked
            // adjustment layer is the commonest case of all.
            //
            // A pixel lock guards the layer's own pixels; masking is a separate
            // channel and stays editable. Only the blanket lock stops it. Both
            // halves are pinned by
            // `a_pixel_lock_leaves_the_mask_editable_but_the_blanket_lock_does_not`.
            if layer.locked.all {
                return Err(CommandError::LayerLocked(id));
            }
            layer
                .mask_id()
                .map(PixelKey::Mask)
                .ok_or(CommandError::NoMask(id))
        }
    }
}

/// Every tile a region edit resolves to: the derived interior plus the
/// caller-supplied edge tiles.
///
/// The [`Coverage::Full`] filter is the whole safety of this function, and it
/// is load-bearing rather than an optimization. `interior` describes the tile
/// the region *would* produce if the region owned all of it; applying that to a
/// tile the rect merely clips would overwrite (or, for a clear, delete) the
/// pixels outside the requested rect. So a partially covered tile is left out
/// entirely unless `edges` names it. Pinned by
/// `a_region_edit_never_derives_content_for_a_tile_it_only_clips`; the tiles a
/// caller must supply are [`crate::pixels::edge_tiles`].
fn region_delta(
    rect: PixelRect,
    edges: impl IntoIterator<Item = TileEdit>,
    interior: impl Fn(raster::TileCoord) -> Option<raster::TileHash>,
) -> Result<TileDelta, CommandError> {
    let mut map: std::collections::BTreeMap<raster::TileCoord, Option<raster::TileHash>> =
        tiles_covering(rect)?
            .into_iter()
            .filter(|(_, cov)| *cov == Coverage::Full)
            .map(|(coord, _)| (coord, interior(coord)))
            .collect();
    for edit in edges {
        if !tile_intersects_region(rect, edit.coord) {
            return Err(PixelError::TileOutsideRegion { coord: edit.coord }.into());
        }
        map.insert(edit.coord, edit.hash);
    }
    Ok(TileDelta::new(
        map.into_iter()
            .map(|(coord, hash)| TileEdit { coord, hash }),
    )?)
}

/// A region command may only touch tiles its own region covers. Checked on
/// apply, not only on construction, because a journal is an untrusted input.
fn check_delta_in_region(rect: PixelRect, delta: &TileDelta) -> Result<(), CommandError> {
    for edit in delta.iter() {
        if !tile_intersects_region(rect, edit.coord) {
            return Err(PixelError::TileOutsideRegion { coord: edit.coord }.into());
        }
    }
    Ok(())
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
    use crate::document::{Guide, GuideAxis};
    use crate::pixels::{FillColor, MaskCoverage};
    use glam::Vec2;
    use layer_model::{Layer, LayerMask, MaskId, ShadowEffect, StrokeEffect};
    use raster::{TileCoord, TileHash, TILE_SIZE};

    fn coord(x: i32, y: i32) -> TileCoord {
        TileCoord::new(x, y, 0)
    }

    fn hash(seed: u8) -> TileHash {
        TileHash([seed; 32])
    }

    /// A document with one raster layer, returned with that layer's id.
    fn doc_with_layer() -> (Document, LayerId) {
        let mut doc = Document::new(1024, 1024, "t");
        let layer = Layer::raster("L1");
        let id = layer.id;
        Command::create_layer(layer).apply(&mut doc).unwrap();
        (doc, id)
    }

    /// A document with one Brightness/Contrast adjustment layer at identity.
    fn doc_with_adjustment() -> (Document, LayerId) {
        use layer_model::{AdjustmentKind, AdjustmentLayer};
        let mut doc = Document::new(64, 64, "t");
        let layer = Layer::with_kind(
            "Brightness/Contrast",
            LayerKind::Adjustment(AdjustmentLayer {
                kind: AdjustmentKind::BrightnessContrast {
                    brightness: 0.0,
                    contrast: 0.0,
                },
            }),
        );
        let id = layer.id;
        Command::create_layer(layer).apply(&mut doc).unwrap();
        (doc, id)
    }

    fn adjustment_of(doc: &Document, id: LayerId) -> layer_model::AdjustmentKind {
        match &doc.layers.get(id).unwrap().kind {
            LayerKind::Adjustment(a) => a.kind.clone(),
            other => panic!("not an adjustment: {other:?}"),
        }
    }

    #[test]
    fn setting_a_layer_kind_edits_the_payload_and_undoes_exactly() {
        use layer_model::AdjustmentKind;
        let (mut doc, id) = doc_with_adjustment();

        let inverse = Command::SetLayerKind {
            layer_id: id,
            kind: Box::new(LayerKind::Adjustment(layer_model::AdjustmentLayer {
                kind: AdjustmentKind::BrightnessContrast {
                    brightness: 0.4,
                    contrast: -0.2,
                },
            })),
        }
        .apply(&mut doc)
        .expect("an adjustment's own parameters are editable");

        assert_eq!(
            adjustment_of(&doc, id),
            AdjustmentKind::BrightnessContrast {
                brightness: 0.4,
                contrast: -0.2
            }
        );
        // The inverse carries the payload the layer held *before*, so undo is
        // exact rather than a return to some default.
        inverse.apply(&mut doc).unwrap();
        assert_eq!(
            adjustment_of(&doc, id),
            AdjustmentKind::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0
            }
        );
    }

    #[test]
    fn a_kind_payload_of_another_class_is_refused_and_changes_nothing() {
        let (mut doc, id) = doc_with_adjustment();
        let before = doc.clone();
        let err = Command::SetLayerKind {
            layer_id: id,
            kind: Box::new(LayerKind::Raster(Default::default())),
        }
        .apply(&mut doc)
        .unwrap_err();
        assert!(
            matches!(
                err,
                CommandError::CannotChangeLayerClass { layer, from, to }
                    if layer == id && from == "adjustment" && to == "raster"
            ),
            "{err:?}"
        );
        assert_eq!(
            doc, before,
            "a refused class change still mutated the layer"
        );
    }

    #[test]
    fn a_fully_locked_layer_refuses_a_kind_edit() {
        use layer_model::AdjustmentKind;
        let (mut doc, id) = doc_with_adjustment();
        doc.layers.get_mut(id).unwrap().locked = LockState {
            all: true,
            ..Default::default()
        };
        let before = doc.clone();
        let err = Command::SetLayerKind {
            layer_id: id,
            kind: Box::new(LayerKind::Adjustment(layer_model::AdjustmentLayer {
                kind: AdjustmentKind::Invert,
            })),
        }
        .apply(&mut doc)
        .unwrap_err();
        assert!(
            matches!(err, CommandError::LayerLocked(l) if l == id),
            "{err:?}"
        );
        assert_eq!(doc, before);
    }

    #[test]
    fn editing_a_groups_blending_cannot_rewrite_its_children() {
        use layer_model::{GroupBlending, GroupLayer};
        let mut doc = Document::new(64, 64, "t");
        let group = Layer::group("G");
        let group_id = group.id;
        Command::create_layer(group).apply(&mut doc).unwrap();
        let child = Layer::raster("child");
        let child_id = child.id;
        Command::create_layer(child).apply(&mut doc).unwrap();
        Command::MoveLayer {
            layer_id: child_id,
            parent: Some(group_id),
            index: 0,
        }
        .apply(&mut doc)
        .unwrap();

        // A payload that claims the group is empty, which is exactly what a
        // panel that read the group before the child was added would send.
        Command::SetLayerKind {
            layer_id: group_id,
            kind: Box::new(LayerKind::Group(GroupLayer {
                children: Vec::new(),
                collapsed: true,
                blending: GroupBlending::PassThrough,
            })),
        }
        .apply(&mut doc)
        .unwrap();

        let LayerKind::Group(g) = &doc.layers.get(group_id).unwrap().kind else {
            panic!("still a group");
        };
        // The editable half landed...
        assert_eq!(g.blending, GroupBlending::PassThrough);
        assert!(g.collapsed);
        // ...and the ownership record did not move.
        assert_eq!(g.children, vec![child_id]);
        assert!(doc.layers.get(child_id).is_some());
    }

    #[test]
    fn a_kind_edit_names_its_class_in_the_history_label() {
        use layer_model::{AdjustmentKind, AdjustmentLayer, TextLayer};
        assert_eq!(
            Command::SetLayerKind {
                layer_id: LayerId::new(),
                kind: Box::new(LayerKind::Adjustment(AdjustmentLayer {
                    kind: AdjustmentKind::Invert
                })),
            }
            .label(),
            "Edit Adjustment"
        );
        assert_eq!(
            Command::SetLayerKind {
                layer_id: LayerId::new(),
                kind: Box::new(LayerKind::Text(TextLayer::default())),
            }
            .label(),
            "Edit Text"
        );
    }

    #[test]
    fn create_then_undo_removes_layer() {
        let mut doc = Document::new(100, 100, "t");
        let layer = Layer::raster("L1");
        let id = layer.id;
        let inverse = Command::create_layer(layer).apply(&mut doc).unwrap();
        assert!(doc.layers.get(id).is_some());
        inverse.apply(&mut doc).unwrap();
        assert!(doc.layers.get(id).is_none());
    }

    #[test]
    fn set_properties_inverse_restores() {
        let (mut doc, id) = doc_with_layer();

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
    fn a_patch_covers_every_editable_layer_field() {
        // The name is earned by the exhaustive `let Layer { .. }` below: adding
        // a field to `layer_model::Layer` breaks this test's compilation, so
        // the coverage claim on `LayerPatch` cannot silently go stale (it did
        // once — `effects` was missing and nothing noticed).
        let (mut doc, id) = doc_with_layer();
        let kind_before = doc.layers.get(id).unwrap().kind.clone();
        let before = doc.clone();

        let styled = LayerEffects {
            stroke: Some(StrokeEffect::default()),
            ..Default::default()
        };
        let new_mask = LayerMask::new(MaskId::new());
        let patch = LayerPatch {
            name: Some("renamed".into()),
            visible: Some(false),
            opacity: Some(0.5),
            fill_opacity: Some(0.25),
            blend_mode: Some(BlendMode::Multiply),
            locked: Some(LockState {
                transparency: true,
                ..Default::default()
            }),
            clipping: Some(ClippingMode::ClipToBelow),
            linked: Some(true),
            transform: Some([2.0, 0.0, 0.0, 2.0, 5.0, 6.0]),
            mask: Patch::Set(new_mask.clone()),
            effects: Some(Box::new(styled.clone())),
        };
        let inverse = Command::SetLayerProperties {
            layer_id: id,
            patch: patch.clone(),
        }
        .apply(&mut doc)
        .unwrap();

        // Exhaustive: every field of `Layer` is either asserted patched, or
        // named here as one of the two the patch deliberately cannot touch.
        let Layer {
            id: layer_id,
            name,
            visible,
            locked,
            opacity,
            fill_opacity,
            blend_mode,
            transform,
            mask,
            clipping,
            linked,
            effects,
            kind,
        } = doc.layers.get(id).unwrap().clone();

        assert_eq!(name, "renamed");
        assert!(!visible);
        assert_eq!(
            locked,
            LockState {
                transparency: true,
                ..Default::default()
            }
        );
        assert_eq!(opacity, 0.5);
        assert_eq!(fill_opacity, 0.25);
        assert_eq!(blend_mode, BlendMode::Multiply);
        assert_eq!(transform.to_cols_array(), [2.0, 0.0, 0.0, 2.0, 5.0, 6.0]);
        assert_eq!(mask, Some(new_mask));
        assert_eq!(clipping, ClippingMode::ClipToBelow);
        assert!(linked, "the link chain is patchable state");
        assert_eq!(effects, styled, "layer styles must be editable by command");
        // The two out of scope, unchanged:
        assert_eq!(layer_id, id, "identity is not patchable");
        assert_eq!(kind, kind_before, "kind is not patchable");

        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before, "every field must come back");
    }

    #[test]
    fn a_layer_style_can_be_added_changed_and_undone() {
        let (mut doc, id) = doc_with_layer();
        assert!(doc.layers.get(id).unwrap().effects.is_empty());
        let before = doc.clone();

        let shadowed = LayerEffects {
            drop_shadow: Some(ShadowEffect {
                distance_px: 12.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let undo_add = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                effects: Some(Box::new(shadowed.clone())),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.layers.get(id).unwrap().effects, shadowed);
        let with_shadow = doc.clone();

        // Replacing one style block with another inverts to the first.
        let stroked = LayerEffects {
            stroke: Some(StrokeEffect::default()),
            ..Default::default()
        };
        let undo_replace = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                effects: Some(Box::new(stroked.clone())),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.layers.get(id).unwrap().effects, stroked);

        undo_replace.apply(&mut doc).unwrap();
        assert_eq!(doc, with_shadow);
        undo_add.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn boxing_keeps_a_command_far_smaller_than_a_layer() {
        // Both `CreateLayer.layer` and `LayerPatch.effects` are boxed so that
        // the effect block does not set the size of every command — a brush
        // stroke emits these by the hundred.
        use std::mem::size_of;
        assert!(
            size_of::<Command>() * 4 < size_of::<Layer>(),
            "Command is {} bytes against a Layer's {}: something large got inlined",
            size_of::<Command>(),
            size_of::<Layer>()
        );
        assert!(
            size_of::<LayerPatch>() * 4 < size_of::<LayerEffects>(),
            "LayerPatch is {} bytes against LayerEffects' {}",
            size_of::<LayerPatch>(),
            size_of::<LayerEffects>()
        );
    }

    #[test]
    fn a_mask_can_be_detached_and_undo_puts_it_back() {
        // `Option<T>`-means-unchanged cannot express this: `mask: None` is
        // "leave it alone", so without `Patch` there is no way to say "remove".
        let (mut doc, id) = doc_with_layer();
        let mask = LayerMask::new(MaskId::new());
        doc.layers.get_mut(id).unwrap().set_mask(mask.clone());
        let before = doc.clone();

        let inverse = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                mask: Patch::Clear,
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.layers.get(id).unwrap().mask.is_none());
        assert!(matches!(
            &inverse,
            Command::SetLayerProperties { patch, .. } if patch.mask == Patch::Set(mask.clone())
        ));

        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn attaching_a_mask_to_a_bare_layer_inverts_to_clear() {
        let (mut doc, id) = doc_with_layer();
        let before = doc.clone();
        let inverse = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                mask: Patch::Set(LayerMask::new(MaskId::new())),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        assert!(matches!(
            &inverse,
            Command::SetLayerProperties { patch, .. } if patch.mask == Patch::<LayerMask>::Clear
        ));
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn a_guide_edit_is_applied_and_inverted_whole() {
        // Guides are document state now: adding, moving or removing one is one
        // undoable step that restores the exact prior set.
        let (mut doc, _id) = doc_with_layer();
        let before = doc.clone();
        assert!(doc.guides.list.is_empty(), "a fresh document has no guides");

        let guides = Guides {
            visible: true,
            locked: false,
            list: vec![
                Guide {
                    axis: GuideAxis::Vertical,
                    doc: 24.0,
                    locked: false,
                },
                Guide {
                    axis: GuideAxis::Horizontal,
                    doc: 120.5,
                    locked: true,
                },
            ],
        };
        let inverse = Command::SetGuides {
            guides: guides.clone(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.guides, guides, "the new set is in place");
        // The DOM is cheap to serialize; a save/load round trip keeps the set.
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.guides, guides, "guides survive serialization");
        // Undo restores the emptor set.
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc.guides, before.guides, "undo restored the prior set");
        assert_eq!(doc, before);
    }

    #[test]
    fn an_out_of_range_opacity_is_refused_and_changes_nothing() {
        let (mut doc, id) = doc_with_layer();
        let before = doc.clone();

        for bad in [2.5f32, -0.5, f32::NAN, f32::INFINITY] {
            let err = Command::SetLayerProperties {
                layer_id: id,
                // A good field alongside the bad one: the refusal must not
                // half-apply the patch.
                patch: LayerPatch {
                    name: Some("should not stick".into()),
                    opacity: Some(bad),
                    ..Default::default()
                },
            }
            .apply(&mut doc)
            .unwrap_err();
            assert!(
                matches!(err, CommandError::InvalidOpacity(v) if v.to_bits() == bad.to_bits()),
                "opacity {bad} was accepted or misreported: {err:?}"
            );
            assert_eq!(doc, before, "opacity {bad} left a mutation behind");
        }
    }

    #[test]
    fn a_non_finite_absolute_transform_is_refused() {
        let (mut doc, id) = doc_with_layer();
        let before = doc.clone();
        let err = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                transform: Some([1.0, 0.0, 0.0, f32::NAN, 0.0, 0.0]),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidTransform(_)));
        assert_eq!(doc, before);
    }

    #[test]
    fn transform_inverse_returns_to_identity() {
        let (mut doc, id) = doc_with_layer();

        let translate = Affine2::from_translation(Vec2::new(10.0, 5.0));
        let inverse = Command::TransformLayer {
            layer_id: id,
            matrix: translate.to_cols_array(),
        }
        .apply(&mut doc)
        .unwrap();
        inverse.apply(&mut doc).unwrap();

        let t = doc.layers.get(id).unwrap().transform;
        let diff = (t.translation - Vec2::ZERO).length();
        assert!(diff < 1e-4, "transform did not return to identity: {t:?}");
    }

    #[test]
    fn a_transform_delta_acts_in_document_space() {
        // `new = delta * current`: a translation delta moves the layer by that
        // many *document* pixels even though the layer is already scaled. Under
        // the opposite convention the 10px drag would come out as 20px.
        let (mut doc, id) = doc_with_layer();
        doc.layers.get_mut(id).unwrap().transform = Affine2::from_scale(Vec2::new(2.0, 2.0));

        Command::TransformLayer {
            layer_id: id,
            matrix: Affine2::from_translation(Vec2::new(10.0, 0.0)).to_cols_array(),
        }
        .apply(&mut doc)
        .unwrap();

        let t = doc.layers.get(id).unwrap().transform;
        assert_eq!(t.translation, Vec2::new(10.0, 0.0));
        // A point at layer-space (1,0) is at document (2,0) before the drag and
        // (12,0) after it: the geometry moved 10 document pixels.
        assert_eq!(
            t.transform_point2(Vec2::new(1.0, 0.0)),
            Vec2::new(12.0, 0.0)
        );
    }

    #[test]
    fn a_singular_transform_is_refused_instead_of_storing_nans() {
        let (mut doc, id) = doc_with_layer();
        let before = doc.clone();

        // Dragging a handle onto the opposite one: a legitimate gesture whose
        // matrix has no inverse.
        for matrix in [
            Affine2::from_scale(Vec2::new(0.0, 1.0)).to_cols_array(),
            Affine2::from_scale(Vec2::new(1.0, 0.0)).to_cols_array(),
            Affine2::from_scale(Vec2::ZERO).to_cols_array(),
            [1.0, 2.0, 2.0, 4.0, 0.0, 0.0], // rank 1
            [f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 1.0, f32::INFINITY, 0.0],
        ] {
            let err = Command::TransformLayer {
                layer_id: id,
                matrix,
            }
            .apply(&mut doc)
            .unwrap_err();
            assert!(
                matches!(err, CommandError::NotInvertible),
                "matrix {matrix:?} produced {err:?}"
            );
            assert_eq!(doc, before, "matrix {matrix:?} mutated the document");
        }

        // And the layer is still usable: nothing NaN reached it.
        let t = doc.layers.get(id).unwrap().transform;
        assert!(t.to_cols_array().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_locked_layer_refuses_transform_and_paint() {
        let (mut doc, id) = doc_with_layer();
        doc.layers.get_mut(id).unwrap().locked = LockState {
            all: true,
            ..Default::default()
        };
        let before = doc.clone();

        let err = Command::TransformLayer {
            layer_id: id,
            matrix: Affine2::from_translation(Vec2::new(1.0, 0.0)).to_cols_array(),
        }
        .apply(&mut doc)
        .unwrap_err();
        assert!(matches!(err, CommandError::LayerLocked(l) if l == id));

        let err = Command::paint_tiles(
            PixelTarget::Layer(id),
            [TileEdit::set(coord(0, 0), hash(1))],
        )
        .unwrap()
        .apply(&mut doc)
        .unwrap_err();
        assert!(matches!(err, CommandError::LayerLocked(l) if l == id));
        assert_eq!(doc, before);
    }

    #[test]
    fn a_fully_locked_layer_refuses_rename_absolute_transform_and_delete() {
        // Every one of these used to succeed: `SetLayerProperties` performed no
        // lock check at all, so `LayerPatch.transform` — an *absolute* matrix —
        // was a way around the guard `TransformLayer` performs two lines away.
        let (mut doc, id) = doc_with_layer();
        doc.layers.get_mut(id).unwrap().locked = LockState {
            all: true,
            ..Default::default()
        };
        let before = doc.clone();

        for patch in [
            LayerPatch {
                name: Some("renamed through the lock".into()),
                ..Default::default()
            },
            LayerPatch {
                transform: Some([3.0, 0.0, 0.0, 3.0, 99.0, 99.0]),
                ..Default::default()
            },
            LayerPatch {
                visible: Some(false),
                ..Default::default()
            },
            LayerPatch {
                mask: Patch::Set(LayerMask::new(MaskId::new())),
                ..Default::default()
            },
            LayerPatch {
                effects: Some(Box::new(LayerEffects::default())),
                ..Default::default()
            },
            // A lock change that keeps the layer fully locked is no escape.
            LayerPatch {
                locked: Some(LockState {
                    all: true,
                    pixels: true,
                    ..Default::default()
                }),
                opacity: Some(0.5),
                ..Default::default()
            },
        ] {
            let err = Command::SetLayerProperties {
                layer_id: id,
                patch: patch.clone(),
            }
            .apply(&mut doc)
            .unwrap_err();
            assert!(
                matches!(err, CommandError::LayerLocked(l) if l == id),
                "{patch:?} was accepted or misreported: {err:?}"
            );
            assert_eq!(doc, before, "{patch:?} left a mutation behind");
        }

        let err = Command::DeleteLayer { layer_id: id }
            .apply(&mut doc)
            .unwrap_err();
        assert!(matches!(err, CommandError::LayerLocked(l) if l == id));
        assert_eq!(doc, before, "the refused delete removed the layer anyway");
    }

    #[test]
    fn releasing_the_lock_is_itself_an_undoable_command() {
        // The one carve-out in the blanket lock: a patch that touches nothing
        // but `locked`. Without it a layer locked by mistake could never be
        // unlocked through the command system, which is where undo lives.
        let (mut doc, id) = doc_with_layer();
        doc.layers.get_mut(id).unwrap().locked = LockState {
            all: true,
            ..Default::default()
        };
        let locked = doc.clone();

        let inverse = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                locked: Some(LockState::default()),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        assert!(!doc.layers.get(id).unwrap().locked.any());

        // And now the edits it was refusing go through.
        Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                name: Some("now editable".into()),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.layers.get(id).unwrap().name, "now editable");

        // Undo of the *unlock* restores the lock (after undoing the rename).
        doc.layers.get_mut(id).unwrap().name = "L1".into();
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, locked);
    }

    #[test]
    fn an_edit_that_engages_the_lock_is_still_undoable() {
        // The trap in a "refuse whenever the layer is locked *now*" rule: the
        // inverse of "rename and lock" is "rename back and unlock", and it is
        // applied to a layer that is by then fully locked. Refusing it would
        // leave a recorded edit that nothing can take back — the one thing this
        // crate exists to prevent.
        let (mut doc, id) = doc_with_layer();
        let before = doc.clone();

        let inverse = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                name: Some("renamed and locked".into()),
                locked: Some(LockState {
                    all: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.layers.get(id).unwrap().locked.all);
        assert_eq!(doc.layers.get(id).unwrap().name, "renamed and locked");

        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before, "undo must survive the lock the edit engaged");

        // Same for the position lock and the absolute transform.
        let inverse = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                transform: Some([1.0, 0.0, 0.0, 1.0, 30.0, 0.0]),
                locked: Some(LockState {
                    position: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        assert!(doc.layers.get(id).unwrap().locked.blocks_transform());
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn a_position_locked_layer_refuses_an_absolute_transform_but_allows_a_rename() {
        let (mut doc, id) = doc_with_layer();
        doc.layers.get_mut(id).unwrap().locked = LockState {
            position: true,
            ..Default::default()
        };
        let before = doc.clone();

        let err = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                name: Some("fine".into()),
                transform: Some([1.0, 0.0, 0.0, 1.0, 40.0, 0.0]),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap_err();
        assert!(matches!(err, CommandError::LayerLocked(l) if l == id));
        assert_eq!(doc, before, "the good field must not stick either");

        // The position lock guards position only.
        Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                name: Some("fine".into()),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.layers.get(id).unwrap().name, "fine");
    }

    #[test]
    fn a_locked_layer_can_still_be_restacked() {
        // The deliberate limit of the lock check, pinned so it reads as a
        // decision rather than an oversight: locks guard pixels and canvas
        // position, not the layer list.
        let mut doc = Document::new(100, 100, "t");
        let a = Layer::raster("A");
        let (aid, b) = (a.id, Layer::raster("B"));
        let bid = b.id;
        Command::create_layer(a).apply(&mut doc).unwrap();
        Command::create_layer(b).apply(&mut doc).unwrap();
        doc.layers.get_mut(aid).unwrap().locked.all = true;

        Command::MoveLayer {
            layer_id: aid,
            parent: None,
            index: 1,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.layers.root(), &[bid, aid]);
    }

    #[test]
    fn a_locked_child_cannot_be_deleted_through_its_group() {
        let mut doc = Document::new(100, 100, "t");
        let g = Layer::group("G");
        let gid = g.id;
        Command::create_layer(g).apply(&mut doc).unwrap();
        let child = Layer::raster("Locked child");
        let cid = child.id;
        Command::create_layer(child).apply(&mut doc).unwrap();
        Command::MoveLayer {
            layer_id: cid,
            parent: Some(gid),
            index: 0,
        }
        .apply(&mut doc)
        .unwrap();
        doc.layers.get_mut(cid).unwrap().locked.all = true;
        let before = doc.clone();

        let err = Command::DeleteLayer { layer_id: gid }
            .apply(&mut doc)
            .unwrap_err();
        assert!(
            matches!(err, CommandError::LayerLocked(l) if l == cid),
            "deleting the group would take the locked child with it: {err:?}"
        );
        assert_eq!(doc, before);
    }

    #[test]
    fn a_layer_cannot_be_created_already_fully_locked() {
        // The trap this refusal closes: `CreateLayer` did no lock check, but
        // its inverse is a `DeleteLayer`, which the blanket lock refuses. So
        // creating an already-locked layer succeeded, `History` recorded the
        // entry, and every later undo answered `Err(LayerLocked)` — a recorded
        // mutation with no way back, which is exactly what this crate's central
        // invariant says cannot exist. Reachable from paste, duplicate, import
        // and journal replay of a locked layer.
        let mut doc = Document::new(100, 100, "t");
        let before = doc.clone();
        let mut history = crate::History::new();

        let mut locked = Layer::raster("locked on arrival");
        locked.locked.all = true;
        let lid = locked.id;

        let err = history
            .apply(&mut doc, Command::create_layer(locked.clone()))
            .unwrap_err();
        assert!(
            matches!(err, CommandError::CannotInsertLocked(l) if l == lid),
            "got {err:?}"
        );
        assert_eq!(doc, before, "a refused create must change nothing");
        assert!(
            !history.can_undo(),
            "nothing was applied, so nothing may be recorded"
        );

        // Every other lock flag is still fine on a new layer: only the blanket
        // lock blocks deletion, so only the blanket lock can strand an undo.
        let mut pixel_locked = Layer::raster("pixels locked");
        pixel_locked.locked.pixels = true;
        pixel_locked.locked.position = true;
        let pid = pixel_locked.id;
        history
            .apply(&mut doc, Command::create_layer(pixel_locked))
            .unwrap();
        assert!(history.undo(&mut doc).unwrap());
        assert_eq!(doc, before);
        assert!(doc.layers.get(pid).is_none());

        // And the supported way to end up with a locked layer undoes as one
        // step, because a transaction applies its inverses newest-first: the
        // unlock runs before the delete.
        let mut unlocked = locked.clone();
        unlocked.locked = LockState::default();
        history
            .apply(
                &mut doc,
                Command::Transaction {
                    label: "Paste locked layer".into(),
                    commands: vec![
                        Command::create_layer(unlocked),
                        Command::SetLayerProperties {
                            layer_id: lid,
                            patch: LayerPatch {
                                locked: Some(LockState {
                                    all: true,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        },
                    ],
                },
            )
            .unwrap();
        assert!(doc.layers.get(lid).unwrap().locked.all);

        assert!(
            history.undo(&mut doc).unwrap(),
            "the undo of a locked-layer paste must apply, not be refused by the lock it set"
        );
        assert_eq!(doc, before, "and it must restore the document exactly");
    }

    #[test]
    fn a_transaction_holding_a_locked_create_rolls_back_instead_of_stranding_the_document() {
        // The same defect seen through `Transaction`: the failing member used
        // to be the *undo* of the locked create during rollback, so a rollback
        // that should have restored the document returned `RollbackFailed` —
        // the one case where this crate cannot promise atomicity — and left the
        // document half-edited.
        let (mut doc, id) = doc_with_layer();
        let before = doc.clone();

        let mut locked = Layer::raster("locked on arrival");
        locked.locked.all = true;
        let ghost = LayerId::new();

        let err = Command::Transaction {
            label: "Import".into(),
            commands: vec![
                Command::SetLayerProperties {
                    layer_id: id,
                    patch: LayerPatch {
                        name: Some("touched".into()),
                        ..Default::default()
                    },
                },
                Command::create_layer(locked),
                // A member that fails after the create, forcing a rollback.
                Command::DeleteLayer { layer_id: ghost },
            ],
        }
        .apply(&mut doc)
        .unwrap_err();

        assert!(
            !matches!(err, CommandError::RollbackFailed { .. }),
            "the rollback itself must succeed: {err}"
        );
        assert!(
            matches!(err, CommandError::CannotInsertLocked(_)),
            "and the failure reported is the locked create, not the ghost delete: {err:?}"
        );
        assert_eq!(doc, before, "the transaction left the document untouched");
    }

    #[test]
    fn a_restore_cannot_smuggle_a_locked_layer_back_in() {
        // `RestoreLayers` is the other insertion, and it has the same inverse
        // (`DeleteLayer`), so it needs the same guard. A subtree captured by
        // `DeleteLayer` can never carry a lock — the delete refused one — but a
        // journal is untrusted input, and this is how a hand-written one would
        // reach an un-undoable restore.
        let mut doc = Document::new(100, 100, "t");
        let g = Layer::group("G");
        let gid = g.id;
        Command::create_layer(g).apply(&mut doc).unwrap();
        let child = Layer::raster("C");
        let cid = child.id;
        Command::create_layer(child).apply(&mut doc).unwrap();
        Command::MoveLayer {
            layer_id: cid,
            parent: Some(gid),
            index: 0,
        }
        .apply(&mut doc)
        .unwrap();

        // Lock the child through the field, as a corrupt document would, then
        // detach the subtree behind the command system's back.
        doc.layers.get_mut(cid).unwrap().locked.all = true;
        let subtree = doc.layers.remove(gid).unwrap();
        let before = doc.clone();

        let err = Command::RestoreLayers { subtree }
            .apply(&mut doc)
            .unwrap_err();
        assert!(
            matches!(err, CommandError::CannotInsertLocked(l) if l == cid),
            "the locked *child* is what would block the undo: {err:?}"
        );
        assert_eq!(doc, before);
    }

    #[test]
    fn an_edit_to_a_corrupt_layer_is_still_undoable() {
        // `opacity`, `fill_opacity` and `transform` are public, unvalidated
        // fields (`layer_model` clamps at read), so a hand-edited document can
        // hold values this command refuses to *write*. The inverse used to
        // capture them raw, and the inverse is applied through the same
        // `validate()`, so editing such a layer produced an undo entry that
        // could never apply.
        for corrupt in [2.0f32, -1.0, f32::NAN, f32::INFINITY] {
            let (mut doc, id) = doc_with_layer();
            {
                let layer = doc.layers.get_mut(id).unwrap();
                layer.opacity = corrupt;
                layer.fill_opacity = corrupt;
            }
            let inverse = Command::SetLayerProperties {
                layer_id: id,
                patch: LayerPatch {
                    opacity: Some(0.5),
                    fill_opacity: Some(0.5),
                    ..Default::default()
                },
            }
            .apply(&mut doc)
            .unwrap();

            inverse.apply(&mut doc).unwrap_or_else(|e| {
                panic!("undo of an edit to opacity {corrupt} was refused: {e}")
            });
            // Undo restores the *effective* value — the one the compositor was
            // already using for that corrupt field — so the layer comes back
            // looking exactly as it did, and the document is now valid.
            let layer = doc.layers.get(id).unwrap();
            assert_eq!(layer.opacity, layer_model::blend::unit(corrupt));
            assert_eq!(layer.fill_opacity, layer_model::blend::unit(corrupt));
        }

        // Same for a non-finite transform, which `validate` also refuses.
        let (mut doc, id) = doc_with_layer();
        doc.layers.get_mut(id).unwrap().transform =
            Affine2::from_cols_array(&[f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let inverse = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                transform: Some([1.0, 0.0, 0.0, 1.0, 10.0, 0.0]),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        inverse
            .apply(&mut doc)
            .expect("undo of an edit to a NaN transform was refused");
        assert_eq!(
            doc.layers.get(id).unwrap().transform,
            Affine2::IDENTITY,
            "a matrix that cannot be restored is normalized to the identity"
        );

        // And none of that disturbs the ordinary case: a valid prior value is
        // restored bit-for-bit.
        let (mut doc, id) = doc_with_layer();
        doc.layers.get_mut(id).unwrap().opacity = 0.375;
        let before = doc.clone();
        let inverse = Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                opacity: Some(0.5),
                transform: Some([2.0, 0.0, 0.0, 2.0, 1.0, 1.0]),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .unwrap();
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn transaction_undo_in_reverse() {
        let mut doc = Document::new(100, 100, "t");
        let l1 = Layer::raster("A");
        let l2 = Layer::raster("B");
        let (id1, id2) = (l1.id, l2.id);
        let tx = Command::Transaction {
            label: "Import".into(),
            commands: vec![Command::create_layer(l1), Command::create_layer(l2)],
        };
        let inverse = tx.apply(&mut doc).unwrap();
        assert_eq!(doc.layers.len(), 2);
        inverse.apply(&mut doc).unwrap();
        assert!(doc.layers.get(id1).is_none());
        assert!(doc.layers.get(id2).is_none());
    }

    #[test]
    fn a_failing_transaction_leaves_the_document_exactly_as_it_was() {
        // The old loop bailed on the first error with no rollback, and
        // `History::apply` then dropped the entry, so the members that *had*
        // applied were mutations nothing could undo.
        let (mut doc, id) = doc_with_layer();
        doc.pixels.apply(
            PixelKey::Layer(id),
            &TileDelta::single(TileEdit::set(coord(0, 0), hash(1))),
        );
        doc.set_active_layer(Some(id)).unwrap();
        let before = doc.clone();

        let ghost = LayerId::new();
        let tx = Command::Transaction {
            label: "Import".into(),
            commands: vec![
                Command::create_layer(Layer::raster("new")),
                Command::SetLayerProperties {
                    layer_id: id,
                    patch: LayerPatch {
                        opacity: Some(0.1),
                        name: Some("touched".into()),
                        ..Default::default()
                    },
                },
                Command::paint_tiles(
                    PixelTarget::Layer(id),
                    [
                        TileEdit::set(coord(0, 0), hash(2)),
                        TileEdit::set(coord(1, 0), hash(3)),
                    ],
                )
                .unwrap(),
                // ...and then the member that fails.
                Command::DeleteLayer { layer_id: ghost },
            ],
        };

        let err = tx.apply(&mut doc).unwrap_err();
        assert!(matches!(err, CommandError::Tree(_)), "got {err:?}");
        assert_eq!(doc, before, "the partial transaction was not rolled back");
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(
            doc.pixels.tile(PixelKey::Layer(id), coord(0, 0)),
            Some(hash(1))
        );
    }

    #[test]
    fn a_nested_transaction_rolls_the_whole_thing_back() {
        let mut doc = Document::new(100, 100, "t");
        let before = doc.clone();
        let ghost = LayerId::new();
        let tx = Command::Transaction {
            label: "Outer".into(),
            commands: vec![
                Command::create_layer(Layer::raster("A")),
                Command::Transaction {
                    label: "Inner".into(),
                    commands: vec![
                        Command::create_layer(Layer::raster("B")),
                        Command::MoveLayer {
                            layer_id: ghost,
                            parent: None,
                            index: 0,
                        },
                    ],
                },
            ],
        };
        assert!(tx.apply(&mut doc).is_err());
        assert_eq!(doc, before);
    }

    #[test]
    fn delete_undo_restores_exact_position() {
        let mut doc = Document::new(100, 100, "t");
        let g = Layer::group("G");
        let gid = g.id;
        let target = Layer::raster("Target");
        let tid = target.id;

        Command::create_layer(g).apply(&mut doc).unwrap();
        Command::create_layer(Layer::raster("Base"))
            .apply(&mut doc)
            .unwrap();
        Command::create_layer(target).apply(&mut doc).unwrap();

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

    #[test]
    fn deleting_a_group_and_undoing_restores_its_children_too() {
        // The old inverse re-created only the deleted layer, so undoing the
        // deletion of a group brought the group back empty and dropped every
        // child on the floor.
        let mut doc = Document::new(100, 100, "t");
        let g = Layer::group("G");
        let gid = g.id;
        Command::create_layer(g).apply(&mut doc).unwrap();

        let mut kids = Vec::new();
        for name in ["A", "B"] {
            let l = Layer::raster(name);
            let id = l.id;
            Command::create_layer(l).apply(&mut doc).unwrap();
            Command::MoveLayer {
                layer_id: id,
                parent: Some(gid),
                index: kids.len(),
            }
            .apply(&mut doc)
            .unwrap();
            kids.push(id);
        }
        let before = doc.layers.iter_depth_first();
        assert_eq!(doc.layers.len(), 3);

        let inverse = Command::DeleteLayer { layer_id: gid }
            .apply(&mut doc)
            .unwrap();
        assert_eq!(doc.layers.len(), 0, "the subtree goes with the group");
        assert!(matches!(inverse, Command::RestoreLayers { .. }));

        let redo = inverse.apply(&mut doc).unwrap();
        assert_eq!(doc.layers.len(), 3, "every child came back");
        assert_eq!(doc.layers.iter_depth_first(), before);
        for id in &kids {
            assert_eq!(doc.layers.parent_of(*id), Some(gid));
        }

        // And the inverse of the undo deletes it again, so redo works.
        assert!(matches!(redo, Command::DeleteLayer { layer_id } if layer_id == gid));
        redo.apply(&mut doc).unwrap();
        assert_eq!(doc.layers.len(), 0);
    }

    #[test]
    fn recreating_a_live_layer_is_refused_instead_of_duplicating_it() {
        // `push_root` rejects a duplicate id. Swallowing that result left the
        // tree unchanged but reported success, so a replayed journal would
        // diverge silently from the document it claims to rebuild.
        let mut doc = Document::new(100, 100, "t");
        let layer = Layer::raster("L");
        let id = layer.id;
        Command::create_layer(layer.clone())
            .apply(&mut doc)
            .unwrap();

        let err = Command::create_layer(layer).apply(&mut doc).unwrap_err();
        assert!(
            matches!(
                err,
                CommandError::Tree(layer_model::TreeError::DuplicateId(d)) if d == id
            ),
            "expected a DuplicateId tree error, got {err:?}"
        );
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers.root(), &[id]);
    }

    // ---- pixel editing -------------------------------------------------

    #[test]
    fn a_paint_commands_inverse_restores_the_exact_prior_tiles() {
        let (mut doc, id) = doc_with_layer();
        // Two tiles already hold content; a third is untouched canvas.
        Command::paint_tiles(
            PixelTarget::Layer(id),
            [
                TileEdit::set(coord(0, 0), hash(1)),
                TileEdit::set(coord(1, 0), hash(2)),
            ],
        )
        .unwrap()
        .apply(&mut doc)
        .unwrap();
        let before = doc.clone();

        let stroke = Command::paint_tiles(
            PixelTarget::Layer(id),
            [
                TileEdit::set(coord(0, 0), hash(9)),
                TileEdit::clear(coord(1, 0)),
                TileEdit::set(coord(2, 0), hash(9)),
            ],
        )
        .unwrap();
        let inverse = stroke.apply(&mut doc).unwrap();

        let key = PixelKey::Layer(id);
        assert_eq!(doc.pixels.tile(key, coord(0, 0)), Some(hash(9)));
        assert_eq!(doc.pixels.tile(key, coord(1, 0)), None);
        assert_eq!(doc.pixels.tile(key, coord(2, 0)), Some(hash(9)));

        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before, "undo must restore the exact prior tiles");
    }

    #[test]
    fn a_stroke_across_many_tiles_is_one_command_with_one_inverse() {
        let (mut doc, id) = doc_with_layer();
        let edits: Vec<TileEdit> = (0..64)
            .map(|i| TileEdit::set(coord(i % 8, i / 8), hash(i as u8)))
            .collect();
        let stroke = Command::paint_tiles(PixelTarget::Layer(id), edits).unwrap();
        let before = doc.clone();

        let inverse = stroke.apply(&mut doc).unwrap();
        assert_eq!(doc.pixels.tile_count(), 64);
        match &inverse {
            Command::PaintTiles { delta, .. } => assert_eq!(delta.len(), 64),
            other => panic!("expected one PaintTiles inverse, got {other:?}"),
        }
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
        assert_eq!(doc.pixels.tile_count(), 0);
    }

    #[test]
    fn painting_a_mask_targets_the_layers_mask_not_its_pixels() {
        let (mut doc, id) = doc_with_layer();
        let mask_id = MaskId::new();
        doc.layers
            .get_mut(id)
            .unwrap()
            .set_mask(LayerMask::new(mask_id));
        let before = doc.clone();

        let inverse =
            Command::paint_tiles(PixelTarget::Mask(id), [TileEdit::set(coord(0, 0), hash(5))])
                .unwrap()
                .apply(&mut doc)
                .unwrap();

        assert_eq!(
            doc.pixels.tile(PixelKey::Mask(mask_id), coord(0, 0)),
            Some(hash(5))
        );
        assert!(
            doc.pixels.tiles(PixelKey::Layer(id)).is_none(),
            "a mask edit must not touch the layer's own pixels"
        );
        assert_eq!(doc.mask_tiles(id).unwrap().len(), 1);

        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn painting_a_mask_that_does_not_exist_is_refused() {
        let (mut doc, id) = doc_with_layer();
        let before = doc.clone();
        let err =
            Command::paint_tiles(PixelTarget::Mask(id), [TileEdit::set(coord(0, 0), hash(5))])
                .unwrap()
                .apply(&mut doc)
                .unwrap_err();
        assert!(matches!(err, CommandError::NoMask(l) if l == id));
        assert_eq!(doc, before);
    }

    #[test]
    fn painting_a_layer_that_does_not_exist_is_refused() {
        let mut doc = Document::new(64, 64, "t");
        let ghost = LayerId::new();
        let err = Command::paint_tiles(
            PixelTarget::Layer(ghost),
            [TileEdit::set(coord(0, 0), hash(1))],
        )
        .unwrap()
        .apply(&mut doc)
        .unwrap_err();
        assert!(matches!(err, CommandError::LayerNotFound(l) if l == ghost));
        assert!(doc.pixels.is_empty());
    }

    #[test]
    fn a_paint_delta_may_not_name_a_tile_twice() {
        let err = Command::paint_tiles(
            PixelTarget::Layer(LayerId::new()),
            [
                TileEdit::set(coord(0, 0), hash(1)),
                TileEdit::set(coord(0, 0), hash(2)),
            ],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            CommandError::Pixel(PixelError::DuplicateTile { .. })
        ));
    }

    #[test]
    fn a_fill_derives_its_interior_tiles_and_takes_edges_from_the_caller() {
        let (mut doc, id) = doc_with_layer();
        // Two whole tiles wide, plus one pixel spilling into a third.
        let rect = PixelRect::new(0, 0, TILE_SIZE * 2 + 1, TILE_SIZE);
        let red = FillColor([255, 0, 0, 255]);
        let edge = TileEdit::set(coord(2, 0), hash(77));

        let fill = Command::fill_region(PixelTarget::Layer(id), rect, red, [edge]).unwrap();
        let Command::FillRegion { delta, .. } = &fill else {
            panic!("expected a FillRegion");
        };
        assert_eq!(delta.len(), 3);
        assert_eq!(delta.get(coord(0, 0)), Some(Some(red.solid_tile_hash())));
        assert_eq!(delta.get(coord(1, 0)), Some(Some(red.solid_tile_hash())));
        assert_eq!(delta.get(coord(2, 0)), Some(Some(hash(77))));

        let before = doc.clone();
        let inverse = fill.apply(&mut doc).unwrap();
        let key = PixelKey::Layer(id);
        assert_eq!(
            doc.pixels.tile(key, coord(0, 0)),
            Some(red.solid_tile_hash())
        );
        assert_eq!(doc.pixels.tile(key, coord(2, 0)), Some(hash(77)));

        // Undoing a fill is a restore, not another fill.
        assert!(matches!(inverse, Command::PaintTiles { .. }));
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn a_region_edit_never_derives_content_for_a_tile_it_only_clips() {
        // The `Coverage::Full` filter in `region_delta` is what stops a region
        // edit from writing a whole solid tile (or deleting a whole tile) for a
        // tile the rect merely clips. Without it this fill would replace both
        // tiles wholesale and this clear would delete both, destroying every
        // pixel outside the eight-pixel-wide rect. No `edges` here on purpose:
        // an edge entry would mask the bug by overriding the derived value.
        let t = TILE_SIZE as i64;
        // 8 pixels wide, straddling the boundary between tile (0,0) and (1,0),
        // so *neither* tile is fully covered and there is no interior at all.
        let straddle = PixelRect::new(t - 4, 0, 8, TILE_SIZE);
        assert_eq!(
            crate::pixels::edge_tiles(straddle).unwrap(),
            vec![coord(0, 0), coord(1, 0)],
            "both tiles are only clipped"
        );

        for fill in [true, false] {
            let (mut doc, id) = doc_with_layer();
            Command::paint_tiles(
                PixelTarget::Layer(id),
                [
                    TileEdit::set(coord(0, 0), hash(1)),
                    TileEdit::set(coord(1, 0), hash(2)),
                ],
            )
            .unwrap()
            .apply(&mut doc)
            .unwrap();
            let before = doc.clone();

            let cmd = if fill {
                Command::fill_region(
                    PixelTarget::Layer(id),
                    straddle,
                    FillColor([255, 0, 0, 255]),
                    [],
                )
                .unwrap()
            } else {
                Command::clear_region(PixelTarget::Layer(id), straddle, []).unwrap()
            };
            let delta = match &cmd {
                Command::FillRegion { delta, .. } | Command::ClearRegion { delta, .. } => delta,
                other => panic!("expected a region command, got {other:?}"),
            };
            assert!(
                delta.is_empty(),
                "{} derived {:?} for tiles it only clips",
                cmd.label(),
                delta.edits()
            );

            cmd.apply(&mut doc).unwrap();
            let key = PixelKey::Layer(id);
            assert_eq!(doc.pixels.tile(key, coord(0, 0)), Some(hash(1)));
            assert_eq!(doc.pixels.tile(key, coord(1, 0)), Some(hash(2)));
            assert_eq!(
                doc, before,
                "a region edit with no interior and no edges must change nothing"
            );
        }
    }

    #[test]
    fn a_fill_smaller_than_one_tile_resolves_to_nothing_without_edges() {
        // The common case — a small marquee, a bucket fill inside a shape — has
        // no fully covered tile, so it resolves to an empty delta and applies
        // successfully while painting nothing. Pinned so the contract is
        // visible rather than surprising, together with the way out:
        // `pixels::edge_tiles` names exactly the tiles the caller must
        // rasterize and hand back.
        let (mut doc, id) = doc_with_layer();
        let small = PixelRect::new(4, 4, 8, 8);
        let red = FillColor([255, 0, 0, 255]);

        let fill = Command::fill_region(PixelTarget::Layer(id), small, red, []).unwrap();
        let Command::FillRegion { delta, .. } = &fill else {
            panic!("expected a FillRegion");
        };
        assert_eq!(delta.len(), 0, "a sub-tile rect has no interior to derive");
        let before = doc.clone();
        fill.apply(&mut doc).unwrap();
        assert_eq!(doc.pixels.tile_count(), 0, "nothing was painted");
        assert_eq!(doc, before);

        // The recovery path: the caller asks which tiles it owns, rasterizes
        // those, and passes them as `edges`.
        let owed = crate::pixels::edge_tiles(small).unwrap();
        assert_eq!(owed, vec![coord(0, 0)]);
        let edges: Vec<TileEdit> = owed.iter().map(|c| TileEdit::set(*c, hash(42))).collect();
        let fill = Command::fill_region(PixelTarget::Layer(id), small, red, edges).unwrap();
        let Command::FillRegion { delta, .. } = &fill else {
            panic!("expected a FillRegion");
        };
        assert_eq!(delta.len(), 1);
        let inverse = fill.apply(&mut doc).unwrap();
        assert_eq!(
            doc.pixels.tile(PixelKey::Layer(id), coord(0, 0)),
            Some(hash(42))
        );
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn a_pixel_lock_leaves_the_mask_editable_but_the_blanket_lock_does_not() {
        // The two halves of the rule `resolve_target` states for a mask target,
        // neither of which was pinned: a pixel lock guards the layer's own
        // pixels and masking is a separate channel, while the blanket lock
        // stops everything.
        let (mut doc, id) = doc_with_layer();
        let mask_id = MaskId::new();
        doc.layers
            .get_mut(id)
            .unwrap()
            .set_mask(LayerMask::new(mask_id));
        doc.layers.get_mut(id).unwrap().locked = LockState {
            pixels: true,
            ..Default::default()
        };

        // (a) The carve-out: a pixel lock does not reach the mask.
        let paint_mask = || {
            Command::paint_tiles(PixelTarget::Mask(id), [TileEdit::set(coord(0, 0), hash(5))])
                .unwrap()
        };
        let inverse = paint_mask()
            .apply(&mut doc)
            .expect("a pixel lock must leave the mask editable");
        assert_eq!(
            doc.pixels.tile(PixelKey::Mask(mask_id), coord(0, 0)),
            Some(hash(5))
        );
        // ...and it really is only the mask channel that is open: the layer's
        // own pixels are still refused.
        assert!(matches!(
            Command::paint_tiles(PixelTarget::Layer(id), [TileEdit::set(coord(0, 0), hash(5))])
                .unwrap()
                .apply(&mut doc)
                .unwrap_err(),
            CommandError::LayerLocked(l) if l == id
        ));
        inverse.apply(&mut doc).unwrap();

        // (b) The blanket lock does stop it.
        doc.layers.get_mut(id).unwrap().locked = LockState {
            all: true,
            ..Default::default()
        };
        let before = doc.clone();
        let err = paint_mask().apply(&mut doc).unwrap_err();
        assert!(
            matches!(err, CommandError::LayerLocked(l) if l == id),
            "a fully locked layer's mask must be refused: {err:?}"
        );
        assert_eq!(doc, before);

        // Same rule for a region edit on the mask, which resolves its target
        // through the same function.
        let err = Command::fill_region(
            PixelTarget::Mask(id),
            PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE),
            MaskCoverage::REVEALED,
            [],
        )
        .unwrap()
        .apply(&mut doc)
        .unwrap_err();
        assert!(matches!(err, CommandError::LayerLocked(l) if l == id));
        assert_eq!(doc, before);
    }

    #[test]
    fn a_layer_kind_that_owns_no_pixels_refuses_a_pixel_edit() {
        // A group has no pixels of its own — it is its children — and neither
        // does an adjustment, a text, a shape. (A smart object *does* own
        // pixels: the compositor renders its cached composite from tiles
        // stored under its layer id.) Storing tiles under one used to succeed
        // silently: the compositor would never read them and
        // `PixelStore::retain_referenced` would keep them alive for as long as
        // the layer existed, because it only asks whether the layer is still
        // in the tree.
        use layer_model::{AdjustmentKind, AdjustmentLayer, GeneratorLayer, ShapeLayer, TextLayer};

        for (kind, name) in [
            (LayerKind::Group(Default::default()), "group"),
            (
                LayerKind::Adjustment(AdjustmentLayer {
                    kind: AdjustmentKind::Exposure { stops: 1.0 },
                }),
                "adjustment",
            ),
            (LayerKind::Text(TextLayer::default()), "text"),
            (LayerKind::Shape(ShapeLayer::default()), "shape"),
        ] {
            let mut doc = Document::new(1024, 1024, "t");
            let layer = Layer::with_kind(name, kind);
            let id = layer.id;
            Command::create_layer(layer).apply(&mut doc).unwrap();
            let before = doc.clone();

            for cmd in [
                Command::paint_tiles(
                    PixelTarget::Layer(id),
                    [TileEdit::set(coord(0, 0), hash(1))],
                )
                .unwrap(),
                Command::fill_region(
                    PixelTarget::Layer(id),
                    PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE),
                    FillColor([1, 2, 3, 4]),
                    [],
                )
                .unwrap(),
                Command::clear_region(
                    PixelTarget::Layer(id),
                    PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE),
                    [],
                )
                .unwrap(),
            ] {
                let err = cmd.apply(&mut doc).unwrap_err();
                assert!(
                    matches!(err, CommandError::NotPaintable { layer, kind } if layer == id && kind == name),
                    "{} on a {name} layer produced {err:?}",
                    cmd.label()
                );
                assert_eq!(doc, before, "the refusal left a mutation behind");
            }
            assert!(doc.pixels.is_empty());

            // A mask, though, is a separate channel every kind may carry — a
            // masked adjustment layer is the commonest case there is.
            let mask_id = MaskId::new();
            doc.layers
                .get_mut(id)
                .unwrap()
                .set_mask(LayerMask::new(mask_id));
            Command::paint_tiles(PixelTarget::Mask(id), [TileEdit::set(coord(0, 0), hash(7))])
                .unwrap()
                .apply(&mut doc)
                .unwrap_or_else(|e| panic!("a {name} layer's mask must be paintable: {e:?}"));
            assert_eq!(
                doc.pixels.tile(PixelKey::Mask(mask_id), coord(0, 0)),
                Some(hash(7))
            );
        }

        // The two kinds that do own pixels accept the same command.
        for kind in [
            LayerKind::Raster(Default::default()),
            LayerKind::Generator(GeneratorLayer {
                provenance_key: "run-1".into(),
            }),
        ] {
            let mut doc = Document::new(1024, 1024, "t");
            let layer = Layer::with_kind("paintable", kind);
            let id = layer.id;
            Command::create_layer(layer).apply(&mut doc).unwrap();
            Command::paint_tiles(
                PixelTarget::Layer(id),
                [TileEdit::set(coord(0, 0), hash(1))],
            )
            .unwrap()
            .apply(&mut doc)
            .unwrap();
            assert_eq!(
                doc.pixels.tile(PixelKey::Layer(id), coord(0, 0)),
                Some(hash(1))
            );
        }
    }

    #[test]
    fn an_effect_numeric_is_layer_models_contract_not_this_commands() {
        // `LayerPatch::validate` checks `opacity`, `fill_opacity` and
        // `transform` and nothing else. This pins where the boundary actually
        // is: a NaN inside the effect block goes through, because `layer_model`
        // defines effect parameters as clamp-at-read rather than validated on
        // write. The claim that something neutralizes it is checked here too,
        // against the clamp that crate publishes.
        let (mut doc, id) = doc_with_layer();
        let effects = LayerEffects {
            drop_shadow: Some(ShadowEffect {
                spread: f32::NAN,
                opacity: 5.0,
                ..Default::default()
            }),
            ..Default::default()
        };

        Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                effects: Some(Box::new(effects)),
                ..Default::default()
            },
        }
        .apply(&mut doc)
        .expect("effect numerics are not validated here");

        let shadow = doc
            .layers
            .get(id)
            .unwrap()
            .effects
            .drop_shadow
            .clone()
            .unwrap();
        assert!(
            shadow.spread.is_nan(),
            "the value really did reach the layer"
        );
        assert_eq!(layer_model::blend::unit(shadow.spread), 0.0);
        assert_eq!(layer_model::blend::unit(shadow.opacity), 1.0);

        // The three fields that *are* validated still are, so narrowing the
        // doc did not narrow the check.
        assert!(matches!(
            Command::SetLayerProperties {
                layer_id: id,
                patch: LayerPatch {
                    opacity: Some(f32::NAN),
                    ..Default::default()
                },
            }
            .apply(&mut doc)
            .unwrap_err(),
            CommandError::InvalidOpacity(_)
        ));
    }

    #[test]
    fn a_clear_removes_the_tiles_it_fully_covers() {
        let (mut doc, id) = doc_with_layer();
        Command::paint_tiles(
            PixelTarget::Layer(id),
            [
                TileEdit::set(coord(0, 0), hash(1)),
                TileEdit::set(coord(1, 0), hash(2)),
            ],
        )
        .unwrap()
        .apply(&mut doc)
        .unwrap();
        let before = doc.clone();

        let clear = Command::clear_region(
            PixelTarget::Layer(id),
            PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE),
            [],
        )
        .unwrap();
        let inverse = clear.apply(&mut doc).unwrap();

        let key = PixelKey::Layer(id);
        assert_eq!(
            doc.pixels.tile(key, coord(0, 0)),
            None,
            "a cleared tile must cost no storage"
        );
        assert_eq!(doc.pixels.tile(key, coord(1, 0)), Some(hash(2)));

        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn a_region_command_may_not_touch_a_tile_outside_its_region() {
        let (mut doc, id) = doc_with_layer();
        let rect = PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE);

        // Refused at construction...
        let err = Command::fill_region(
            PixelTarget::Layer(id),
            rect,
            FillColor::TRANSPARENT,
            [TileEdit::set(coord(9, 9), hash(1))],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            CommandError::Pixel(PixelError::TileOutsideRegion { .. })
        ));

        // ...and again on apply, because a journal is untrusted input.
        let forged = Command::FillRegion {
            target: PixelTarget::Layer(id),
            rect,
            value: FillValue::Color(FillColor::TRANSPARENT),
            delta: TileDelta::single(TileEdit::set(coord(9, 9), hash(1))),
        };
        let before = doc.clone();
        let err = forged.apply(&mut doc).unwrap_err();
        assert!(matches!(
            err,
            CommandError::Pixel(PixelError::TileOutsideRegion { .. })
        ));
        assert_eq!(doc, before);
    }

    #[test]
    fn a_mask_region_is_filled_with_coverage_and_cannot_be_cleared() {
        // The convention (see `crate::pixels`): a mask tile is 8-bit coverage,
        // and an *absent* mask tile reads as zero coverage — the layer hidden —
        // not as "nothing stored". So "clear it back to nothing" has no
        // meaning on a mask, and revealing is an explicit full-coverage fill.
        let (mut doc, id) = doc_with_layer();
        let mask_id = MaskId::new();
        doc.layers
            .get_mut(id)
            .unwrap()
            .set_mask(LayerMask::new(mask_id));
        let before = doc.clone();
        let rect = PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE);

        // An RGBA color cannot fill a mask...
        assert!(matches!(
            Command::fill_region(PixelTarget::Mask(id), rect, FillColor([1, 2, 3, 4]), [])
                .unwrap_err(),
            CommandError::FillValueMismatch
        ));
        // ...and a coverage sample cannot fill a layer.
        assert!(matches!(
            Command::fill_region(PixelTarget::Layer(id), rect, MaskCoverage::REVEALED, [])
                .unwrap_err(),
            CommandError::FillValueMismatch
        ));
        // Refused on apply too, because a journal is untrusted input.
        let forged = Command::FillRegion {
            target: PixelTarget::Mask(id),
            rect,
            value: FillValue::Color(FillColor::TRANSPARENT),
            delta: TileDelta::single(TileEdit::set(coord(0, 0), hash(1))),
        };
        assert!(matches!(
            forged.apply(&mut doc).unwrap_err(),
            CommandError::FillValueMismatch
        ));
        assert_eq!(doc, before);

        // Clearing a mask is refused, at construction and on apply.
        assert!(matches!(
            Command::clear_region(PixelTarget::Mask(id), rect, []).unwrap_err(),
            CommandError::CannotClearMask(l) if l == id
        ));
        let forged_clear = Command::ClearRegion {
            target: PixelTarget::Mask(id),
            rect,
            delta: TileDelta::single(TileEdit::clear(coord(0, 0))),
        };
        assert!(matches!(
            forged_clear.apply(&mut doc).unwrap_err(),
            CommandError::CannotClearMask(l) if l == id
        ));
        assert_eq!(doc, before);

        // The supported way to reveal through a mask: fill full coverage.
        let reveal =
            Command::fill_region(PixelTarget::Mask(id), rect, MaskCoverage::REVEALED, []).unwrap();
        let inverse = reveal.apply(&mut doc).unwrap();
        assert_eq!(
            doc.pixels.tile(PixelKey::Mask(mask_id), coord(0, 0)),
            Some(MaskCoverage::REVEALED.solid_tile_hash())
        );
        assert!(
            doc.pixels.tiles(PixelKey::Layer(id)).is_none(),
            "a mask fill must not touch the layer's own pixels"
        );
        inverse.apply(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn a_region_larger_than_the_grid_is_refused_at_construction() {
        let err = Command::clear_region(
            PixelTarget::Layer(LayerId::new()),
            PixelRect::new(0, 0, u32::MAX, u32::MAX),
            [],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            CommandError::Pixel(PixelError::RegionTooLarge { .. })
        ));
    }

    /// Every [`Command`] variant must serialize and deserialize losslessly so
    /// the on-disk journal can be replayed after a crash. Verified structurally:
    /// serializing the deserialized value yields the exact same JSON.
    fn json_roundtrip(cmd: &Command) {
        let json = serde_json::to_string(cmd).expect("serialize");
        let back: Command = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, cmd, "command did not round-trip losslessly");
        let json2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(json, json2);
    }

    #[test]
    fn command_variants_serde_roundtrip() {
        let layer = Layer::raster("L");
        let id = layer.id;

        json_roundtrip(&Command::create_layer(layer.clone()));
        json_roundtrip(&Command::DeleteLayer { layer_id: id });

        // `RestoreLayers` carries a `DetachedSubtree`, which is only
        // obtainable from a real removal.
        let mut doc = Document::new(1024, 1024, "t");
        let g = Layer::group("G");
        let gid = g.id;
        Command::create_layer(g).apply(&mut doc).unwrap();
        let child = Layer::raster("Child");
        let cid = child.id;
        Command::create_layer(child).apply(&mut doc).unwrap();
        Command::MoveLayer {
            layer_id: cid,
            parent: Some(gid),
            index: 0,
        }
        .apply(&mut doc)
        .unwrap();
        json_roundtrip(
            &Command::DeleteLayer { layer_id: gid }
                .apply(&mut doc)
                .unwrap(),
        );

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
                locked: Some(LockState::default()),
                clipping: Some(ClippingMode::ClipToBelow),
                transform: Some([1.0, 0.0, 0.0, 1.0, 2.0, 3.0]),
                mask: Patch::Set(LayerMask::new(MaskId::new())),
                ..Default::default()
            },
        });
        json_roundtrip(&Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                mask: Patch::Clear,
                ..Default::default()
            },
        });
        json_roundtrip(&Command::TransformLayer {
            layer_id: id,
            matrix: [1.0, 0.0, 0.0, 1.0, 10.0, -5.0],
        });
        json_roundtrip(
            &Command::paint_tiles(
                PixelTarget::Layer(id),
                [
                    TileEdit::set(coord(0, 0), hash(3)),
                    TileEdit::clear(coord(4, 2)),
                ],
            )
            .unwrap(),
        );
        json_roundtrip(
            &Command::fill_region(
                PixelTarget::Layer(id),
                PixelRect::new(-5, 10, TILE_SIZE, TILE_SIZE),
                FillColor([1, 2, 3, 4]),
                [],
            )
            .unwrap(),
        );
        json_roundtrip(
            &Command::fill_region(
                PixelTarget::Mask(id),
                PixelRect::new(-5, 10, TILE_SIZE, TILE_SIZE),
                MaskCoverage::REVEALED,
                [],
            )
            .unwrap(),
        );
        json_roundtrip(
            &Command::clear_region(
                PixelTarget::Layer(id),
                PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE),
                [],
            )
            .unwrap(),
        );
        json_roundtrip(&Command::Transaction {
            label: "Import".into(),
            commands: vec![
                Command::create_layer(layer.clone()),
                Command::DeleteLayer { layer_id: id },
            ],
        });
    }

    #[test]
    fn a_patch_written_before_the_new_fields_still_deserializes() {
        // A version-2 journal entry: only the four original keys.
        let json = r#"{"name":null,"visible":true,"opacity":0.5,"blend_mode":"Normal"}"#;
        let patch: LayerPatch = serde_json::from_str(json).unwrap();
        assert_eq!(patch.visible, Some(true));
        assert_eq!(patch.opacity, Some(0.5));
        assert!(
            patch.mask.is_keep(),
            "an absent mask key must mean unchanged"
        );
        assert!(patch.transform.is_none());
        assert!(patch.locked.is_none());
    }

    #[test]
    fn every_variant_has_a_label() {
        let id = LayerId::new();
        let labels = [
            Command::create_layer(Layer::raster("L")).label(),
            Command::DeleteLayer { layer_id: id }.label(),
            Command::MoveLayer {
                layer_id: id,
                parent: None,
                index: 0,
            }
            .label(),
            Command::SetLayerProperties {
                layer_id: id,
                patch: LayerPatch::default(),
            }
            .label(),
            Command::TransformLayer {
                layer_id: id,
                matrix: Affine2::IDENTITY.to_cols_array(),
            }
            .label(),
            Command::paint_tiles(PixelTarget::Layer(id), [])
                .unwrap()
                .label(),
            Command::paint_tiles(PixelTarget::Mask(id), [])
                .unwrap()
                .label(),
            Command::fill_region(
                PixelTarget::Layer(id),
                PixelRect::new(0, 0, 1, 1),
                FillColor::TRANSPARENT,
                [],
            )
            .unwrap()
            .label(),
            Command::fill_region(
                PixelTarget::Mask(id),
                PixelRect::new(0, 0, 1, 1),
                MaskCoverage::HIDDEN,
                [],
            )
            .unwrap()
            .label(),
            Command::clear_region(PixelTarget::Layer(id), PixelRect::new(0, 0, 1, 1), [])
                .unwrap()
                .label(),
            Command::Transaction {
                label: "Custom".into(),
                commands: vec![],
            }
            .label(),
        ];
        assert!(labels.iter().all(|l| !l.is_empty()));
        assert_eq!(labels.last().unwrap(), "Custom");
        assert_eq!(labels[5], "Paint");
        assert_eq!(labels[6], "Paint Mask");
        assert_eq!(labels[7], "Fill");
        assert_eq!(labels[8], "Fill Mask");
        assert_eq!(labels[9], "Clear");
    }
}
