//! Interactive tools: pointer gestures in, undoable commands out.
//!
//! # The one path everything takes
//!
//! A tool is a small state machine fed [`PointerEvent`]s. It reads through a
//! [`ToolContext`] — the active layer, the selection, the colours, the pixel
//! bytes — and when the gesture *ends* it produces exactly one edit:
//!
//! ```text
//!   pointer down / move / up
//!          |
//!          v
//!   tool accumulates a gesture         (dabs, a rubber band, a path)
//!          |
//!          v  on pointer up, or on commit
//!   load the tiles it will touch       crate::patch::ColorPatch  (a layer)
//!                                      crate::patch::CoveragePatch (a mask)
//!   edit them in linear premultiplied light
//!   encode the touched tiles once  ->  editor_core::TileDelta
//!          |
//!          v
//!   ToolContext::emit(Command::PaintTiles { .. })     <- ONE command
//! ```
//!
//! That last line is the whole point. A brush stroke of four hundred dabs
//! across a dozen tiles is one [`editor_core::Command`], one history entry, one
//! ctrl+Z — because the dabs accumulate in a coverage plane
//! ([`stroke::StrokeBuffer`]) and the plane is composited and encoded exactly
//! once. Compositing per dab would give four hundred commands *and* a stroke
//! that darkens wherever it overlaps itself.
//!
//! # Where the pixels come from
//!
//! `editor-core` holds no pixel bytes: a layer is a sparse map from tile
//! coordinate to content hash. Tools need real bytes, so they go through
//! [`tiles::TileAccess`] — three methods: resolve a reference, fetch bytes by
//! hash, store bytes and get a hash back. An application backs it with its tile
//! store; [`tiles::MemoryTiles`] backs it with two hash maps, which is what the
//! tests and any headless run use.
//!
//! Every pixel a tool touches is decoded to **linear, premultiplied** RGBA on
//! the way in and encoded back to straight-alpha sRGB8 once on the way out.
//! Nothing in this crate averages, blurs, resamples or blends gamma-encoded
//! values.
//!
//! # Layer or mask
//!
//! [`tool::PaintTarget`] decides which surface of the active layer a pixel tool
//! writes to, and **every** pixel tool branches on it before it loads anything:
//! a mask tile is one byte per pixel where a layer tile is four, and
//! [`editor_core::Command::PaintTiles`] carries hashes, so nothing downstream
//! would catch the mistake. The tools that mean something on coverage — the
//! brush and eraser, the fills, the gradient, the rasterised shapes, the free
//! transform — go through [`patch::CoveragePatch`], painting the colour's
//! luminance ([`patch::mask_coverage_of`]: white reveals, black conceals). The
//! ones that are definitionally about colour — red-eye, patch, the magic
//! eraser, and the retouching [`StrokeOp`]s — refuse with
//! [`ToolError::UnsupportedOnMask`] rather than edit the layer the user was not
//! looking at.
//!
//! # What the crate contains
//!
//! | module | what lives there |
//! |---|---|
//! | [`tool`] | the [`Tool`] trait, [`ToolId`], [`ToolContext`], the view state |
//! | [`tiles`] | the byte-store seam and an in-memory implementation |
//! | [`patch`] | tile-aligned working planes and the tile-delta commit |
//! | [`brush`] | the brush engine: dab shape, spacing, pressure, stabilisation |
//! | [`stroke`] | the coverage plane, the per-pixel ops, and [`stroke::StrokeTool`] |
//! | [`gradient`] | five gradient shapes over a multi-stop dithered ramp |
//! | [`bucket`] | flood fill and pattern fill |
//! | [`select`] | marquee, lasso, wand and quick-select gestures |
//! | [`shape`] | the shape tools, in vector-layer and rasterise modes |
//! | [`transform`] | free transform: homography, warp mesh, handles, resampling |
//! | [`edit`] | move, crop, slice, eyedropper, red-eye, patch, magic eraser |
//! | [`text`] | the Type tool: click to place a text layer, then type into it |
//! | [`pen`] | the Pen tool: author a path one click at a time |
//! | [`curvature_pen`] | W7-F: the Curvature Pen (and the Freeform Pen lives in [`pen`]) |
//! | [`perspective_crop`] | W7-F: drag a quad, Enter rectifies and crops |
//! | [`mixer_brush`] | W7-F: wet paint that picks up the colour under it |
//! | [`artboard`] | W7-F: drag out an artboard (a group with a background plate) |
//! | [`pencil`] | the Pencil and its Auto Erase |
//! | [`measure`] | the Ruler and the Colour Sampler |
//! | [`history_brush`] | the History Brush: paint from an earlier state |
//! | [`view`] | hand, zoom and rotate-view — the tools that emit nothing |
//! | [`registry`] | metadata and construction for every tool in the palette |
//!
//! # Two things that are *not* commands
//!
//! * **Selection changes.** [`editor_core::Selection`] is a field on the
//!   document, not a command target, so the selection tools emit a
//!   [`tool::SelectionEdit`] on their own outbox and the application folds it
//!   in.
//! * **Crop and slice.** A crop resizes the canvas *and* moves every layer
//!   under the new origin, which is two commands rather than one, so
//!   [`edit::CropTool`] reports a [`tool::CropRequest`] and the application
//!   turns it into the [`editor_core::Command::Transaction`] that performs it —
//!   see `app_shell::tool_input::crop_command`. A crop **is** undoable, as one
//!   step. A slice set is not an edit at all: it is a set of export regions,
//!   and this crate hands it over as [`tool::ToolRequest::Slices`].
//!
//!   Both publish on an explicit [`Tool::commit`], never on pointer-up: the
//!   crop box waits for Enter so the user can nudge its edges, and
//!   [`edit::SliceTool`] collects slices until the application asks for them,
//!   so the outbox never holds several overlapping versions of one set.
//!
//! # Two tools whose gesture *is* the layer
//!
//! [`text::TypeTool`] and [`pen::PenTool`] are the two that create rather than
//! edit: a Type click makes a [`layer_model::LayerKind::Text`] layer and opens
//! it for typing, and a Pen click sequence builds a path that becomes a
//! [`layer_model::LayerKind::Shape`] when it is closed or committed. Before
//! them there was no gesture in this crate that could produce either kind, and
//! `P` was the one letter of the brief the registry could not answer.

#![forbid(unsafe_code)]

pub mod artboard;
pub mod brush;
pub mod bucket;
pub mod curvature_pen;
pub mod edit;
pub mod error;
pub mod gradient;
pub mod history_brush;
pub mod measure;
pub mod mixer_brush;
pub mod patch;
pub mod path_select;
pub mod pen;
pub mod pencil;
pub mod perspective_crop;
pub mod registry;
pub mod select;
pub mod shape;
pub mod stroke;
pub mod text;
pub mod tiles;
pub mod tool;
pub mod transform;
pub mod view;

pub use brush::{BrushSettings, Dab, DabEmitter};
pub use error::ToolError;
/// The blend mode a paint stroke composites through.
///
/// Re-exported from `layer-model` rather than declared here so the options
/// bar, the layer panel and the stroke compositor speak the same 27 modes —
/// a paint "Multiply" is the layer panel's "Multiply". The options bar offers
/// it under [`BLEND_MODE_KEY`] and it reaches a [`StrokeTool`] as a
/// [`ToolSetting::Choice`] indexing [`BlendMode::ALL`] — see
/// [`blend_mode_from_choice`].
pub use layer_model::BlendMode;

/// The options-bar key the paint blend mode travels under.
///
/// Not a registry key: the registry schema has no slot for a blend mode, so
/// the UI adds the control by capability — to exactly the four source-over
/// stroke tools [`composites_strokes`] names (Brush, Pencil, Clone Stamp,
/// Pattern Stamp), which are the only tools whose `set_setting` accepts it —
/// and forwards it like any other touched option. The `ui.` prefix is
/// history — the key is now answered by [`StrokeTool::set_setting`] and must
/// be forwarded, not filtered.
pub const BLEND_MODE_KEY: &str = "ui.blend_mode";

/// The blend mode a [`ToolSetting::Choice`] index under [`BLEND_MODE_KEY`]
/// names: the position in [`BlendMode::ALL`], which is the order the options
/// bar lists them in. `None` when the index is out of range.
pub fn blend_mode_from_choice(index: usize) -> Option<BlendMode> {
    BlendMode::ALL.get(index).copied()
}

/// `true` when the tool the registry builds for `id` lays a source colour
/// over the layer and composites it through a blend mode, and therefore
/// answers [`BLEND_MODE_KEY`]: the four [`StrokeTool`]s whose op is a
/// source-over one (`StrokeOp::composites_source`) — Brush, Pencil, Clone
/// Stamp and Pattern Stamp.
///
/// This is the capability the options bar offers the Mode combo by. The
/// other twelve stroke tools mix toward a computed target (Blur, Sharpen,
/// Smudge, Dodge, Burn, Sponge, Healing Brush, Spot Healing, Color
/// Replacement) or take coverage away (Eraser, Background Eraser) — there
/// is no source colour of theirs for a mode to act on, and Refine Boundary
/// never composites at all — so their `set_setting` refuses the key as
/// unknown and they are not offered the combo. Patch, Red Eye, the two
/// fills, the gradient and the magic eraser have no dab step either.
/// Offering any of them the combo would make a touched Mode a control that
/// does nothing (or a refusal on every press), which is the defect this
/// predicate exists to prevent.
///
/// Pinned against construction by
/// `composites_strokes_is_exactly_the_set_that_answers_the_mode_key` in
/// `tests/options_reach_the_tools_b2.rs`: inside the paint/retouch groups
/// this predicate must equal "`set_setting(BLEND_MODE_KEY, ..)` is `Ok`".
pub fn composites_strokes(id: ToolId) -> bool {
    matches!(
        id,
        ToolId::Brush | ToolId::Pencil | ToolId::CloneStamp | ToolId::PatternStamp
    )
}
pub use patch::{ColorPatch, CoveragePatch, TileBox};
pub use pen::PenTool;
pub use registry::{Cursor, OptionKind, OptionSpec, ToolGroup, ToolInfo};
pub use stroke::{StrokeBuffer, StrokeOp, StrokeTool};
pub use text::{TextSession, TypeTool};
pub use tiles::{MemoryTiles, TileAccess};
pub use tool::TextHitCaret;
pub use tool::{
    snap_delta, with_link_chain, CropRequest, Modifiers, PaintTarget, Pattern, PointerEvent,
    SelectionEdit, SessionGeometry, Slice, SnapAxis, SnapCandidate, TextEdit, Tool, ToolContext,
    ToolId, ToolRequest, ToolSetting, ViewState,
};
