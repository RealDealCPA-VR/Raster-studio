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

pub mod brush;
pub mod bucket;
pub mod edit;
pub mod error;
pub mod gradient;
pub mod patch;
pub mod path_select;
pub mod pen;
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
pub use patch::{ColorPatch, CoveragePatch, TileBox};
pub use pen::PenTool;
pub use registry::{Cursor, OptionKind, OptionSpec, ToolGroup, ToolInfo};
pub use stroke::{StrokeBuffer, StrokeOp, StrokeTool};
pub use text::{TextSession, TypeTool};
pub use tiles::{MemoryTiles, TileAccess};
pub use tool::{
    CropRequest, Modifiers, PaintTarget, Pattern, PointerEvent, SelectionEdit, Slice, TextEdit,
    Tool, ToolContext, ToolId, ToolRequest, ViewState,
};
