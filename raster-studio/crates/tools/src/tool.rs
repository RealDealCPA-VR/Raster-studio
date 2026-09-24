//! The [`Tool`] trait and the context a gesture runs against.
//!
//! A tool is a small state machine over pointer events. It owns no pixels and
//! no document: it reads through [`ToolContext`], and everything it wants
//! *changed* leaves through one of two outboxes —
//! [`ToolContext::emit`] for [`Command`]s the application runs through history,
//! and [`ToolContext::emit_selection`] for selection changes, which
//! `editor-core` does not yet model as a command.
//!
//! The rule that makes undo behave: **a gesture emits its command when the
//! gesture ends**, not while it is running. A brush stroke of four hundred dabs
//! is one `PaintTiles`, one history entry, one ctrl+Z.

use editor_core::{Command, PixelKey, PixelTarget, Selection};
use glam::{Mat3, Vec2};
use layer_model::{LayerId, LayerKind, MaskId, TextLayer};
use raster::PixelRect;
use selection::{BooleanOp, Rect};

use crate::brush::BrushSettings;
use crate::error::ToolError;
use crate::gradient::GradientRamp;
use crate::tiles::TileAccess;

/// Stable identifier for a tool — the key the UI binds shortcuts and icons to,
/// and what a saved workspace persists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ToolId {
    Move,
    RectMarquee,
    EllipseMarquee,
    SingleRowMarquee,
    SingleColumnMarquee,
    Lasso,
    PolygonalLasso,
    MagneticLasso,
    MagicWand,
    QuickSelect,
    Crop,
    Slice,
    Eyedropper,
    SpotHealing,
    HealingBrush,
    Patch,
    RedEye,
    Brush,
    Pencil,
    ColorReplacement,
    CloneStamp,
    PatternStamp,
    Eraser,
    BackgroundEraser,
    MagicEraser,
    Gradient,
    PaintBucket,
    PatternFill,
    Blur,
    Sharpen,
    Smudge,
    Dodge,
    Burn,
    Sponge,
    /// Card 061: regrade the mask coverage inside a user-marked boundary
    /// band (local smooth + re-grade, bounded to the painted region) —
    /// manual refinement, not automatic matting. See
    /// [`crate::stroke::StrokeOp::RefineBoundary`].
    RefineBoundary,
    /// Author a path one click at a time. See [`crate::pen::PenTool`].
    Pen,
    /// Click a path to select the shape layer that owns it. See
    /// [`crate::path_select::PathSelectTool`].
    PathSelect,
    /// Drag a path's anchors. See [`crate::path_select::DirectSelectionTool`].
    DirectSelection,
    /// Click to place a text layer and type into it. See
    /// [`crate::text::TypeTool`].
    Type,
    Rectangle,
    RoundedRectangle,
    Ellipse,
    Polygon,
    Star,
    Line,
    CustomShape,
    Hand,
    Zoom,
    RotateView,
    FreeTransform,
    /// W4-G: drag to measure; Enter straightens the active layer. See
    /// [`crate::measure::RulerTool`].
    Ruler,
    /// W4-G: up to four persistent sample points. See
    /// [`crate::measure::ColorSamplerTool`].
    ColorSampler,
    /// W4-G: paint from an earlier history state. See
    /// [`crate::history_brush::HistoryBrushTool`].
    HistoryBrush,
    /// W4-G: the Pen slot's path-editing tools. See
    /// [`crate::path_select::AnchorTool`].
    AddAnchor,
    DeleteAnchor,
    ConvertAnchor,
    /// W7-F: drag a quad, adjust its corners, Enter rectifies the image into
    /// the crop rect. See [`crate::perspective_crop::PerspectiveCropTool`].
    PerspectiveCrop,
    /// W7-F: text laid out in top-to-bottom columns. See
    /// [`crate::text::TypeTool`] ([`crate::text::TypeMode::Vertical`]).
    VerticalType,
    /// W7-F: typing makes a selection from the glyph outlines, horizontal.
    HorizontalTypeMask,
    /// W7-F: the vertical Type Mask.
    VerticalTypeMask,
    /// W7-F: wet paint that picks up the colour under it. See
    /// [`crate::mixer_brush::MixerBrushTool`].
    MixerBrush,
    /// W7-F: drag out an artboard. See [`crate::artboard::ArtboardTool`].
    Artboard,
    /// W7-F: clicks place points a smooth curve passes through. See
    /// [`crate::curvature_pen::CurvaturePenTool`].
    CurvaturePen,
    /// W7-F: a freehand drag fitted into a path. See
    /// [`crate::pen::FreeformPenTool`].
    FreeformPen,
}

impl ToolId {
    /// Every tool, in palette order.
    ///
    /// The registry is checked against this list, so a new variant that is
    /// added here and nowhere else fails a test rather than silently
    /// disappearing from the UI.
    pub const ALL: &'static [ToolId] = &[
        ToolId::Move,
        ToolId::RectMarquee,
        ToolId::EllipseMarquee,
        ToolId::SingleRowMarquee,
        ToolId::SingleColumnMarquee,
        ToolId::Lasso,
        ToolId::PolygonalLasso,
        ToolId::MagneticLasso,
        ToolId::MagicWand,
        ToolId::QuickSelect,
        ToolId::Crop,
        ToolId::Slice,
        ToolId::Eyedropper,
        ToolId::SpotHealing,
        ToolId::HealingBrush,
        ToolId::Patch,
        ToolId::RedEye,
        ToolId::Brush,
        ToolId::Pencil,
        ToolId::ColorReplacement,
        ToolId::CloneStamp,
        ToolId::PatternStamp,
        ToolId::Eraser,
        ToolId::BackgroundEraser,
        ToolId::MagicEraser,
        ToolId::Gradient,
        ToolId::PaintBucket,
        ToolId::PatternFill,
        ToolId::Blur,
        ToolId::Sharpen,
        ToolId::Smudge,
        ToolId::Dodge,
        ToolId::Burn,
        ToolId::Sponge,
        ToolId::RefineBoundary,
        ToolId::Pen,
        ToolId::PathSelect,
        ToolId::DirectSelection,
        ToolId::Type,
        ToolId::Rectangle,
        ToolId::RoundedRectangle,
        ToolId::Ellipse,
        ToolId::Polygon,
        ToolId::Star,
        ToolId::Line,
        ToolId::CustomShape,
        ToolId::Hand,
        ToolId::Zoom,
        ToolId::RotateView,
        ToolId::FreeTransform,
        ToolId::Ruler,
        ToolId::ColorSampler,
        ToolId::HistoryBrush,
        ToolId::AddAnchor,
        ToolId::DeleteAnchor,
        ToolId::ConvertAnchor,
        ToolId::PerspectiveCrop,
        ToolId::VerticalType,
        ToolId::HorizontalTypeMask,
        ToolId::VerticalTypeMask,
        ToolId::MixerBrush,
        ToolId::Artboard,
        ToolId::CurvaturePen,
        ToolId::FreeformPen,
    ];
}

/// Modifier keys held during a pointer event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        shift: false,
        alt: false,
        ctrl: false,
    };

    pub fn shift() -> Self {
        Modifiers {
            shift: true,
            ..Modifiers::NONE
        }
    }

    pub fn alt() -> Self {
        Modifiers {
            alt: true,
            ..Modifiers::NONE
        }
    }

    /// How a selection gesture combines with what is already selected.
    ///
    /// The convention every raster editor shares: plain replaces, shift adds,
    /// alt subtracts, both intersect. `ctrl` is left to the tool.
    pub fn selection_op(self) -> BooleanOp {
        match (self.shift, self.alt) {
            (true, true) => BooleanOp::Intersect,
            (true, false) => BooleanOp::Add,
            (false, true) => BooleanOp::Subtract,
            (false, false) => BooleanOp::Replace,
        }
    }
}

/// A pointer sample in document (image-pixel) space.
#[derive(Debug, Clone, Copy)]
pub struct PointerEvent {
    /// Position in image pixels; fractional, because a stylus reports subpixel
    /// positions and a brush needs them.
    pub pos: Vec2,
    /// Stylus pressure in `0..=1`. A mouse reports `1.0`.
    pub pressure: f32,
    pub modifiers: Modifiers,
}

impl PointerEvent {
    /// A full-pressure event with no modifiers — the mouse case.
    pub fn at(x: f32, y: f32) -> Self {
        Self {
            pos: Vec2::new(x, y),
            pressure: 1.0,
            modifiers: Modifiers::NONE,
        }
    }

    pub fn with_pressure(mut self, p: f32) -> Self {
        self.pressure = p;
        self
    }

    pub fn with_modifiers(mut self, m: Modifiers) -> Self {
        self.modifiers = m;
        self
    }
}

/// Which surface of the active layer a pixel tool writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaintTarget {
    /// The layer's own pixels.
    #[default]
    Layer,
    /// The layer's mask coverage.
    Mask,
}

/// The canvas view: pan, zoom and rotation, owned by the app and mutated by the
/// navigation tools.
///
/// Kept here rather than in a UI crate because [`ToolId::Hand`],
/// [`ToolId::Zoom`] and [`ToolId::RotateView`] are tools like any other and
/// have to be able to change it. A view change is *not* a [`Command`]: it is
/// not part of the document and does not belong in undo history.
/// Card 042: one snap candidate in document space (axis + coordinate). The
/// UI's richer candidate (with the reason) converts into this at the shell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapCandidate {
    /// The axis the coordinate lives on.
    pub axis: SnapAxis,
    /// The document coordinate to land on.
    pub doc: f32,
}

/// Card 042: snap a moving rect (base + delta) against candidates — per
/// axis, the nearest candidate within `threshold_doc` for any of the rect's
/// min/max/center carries the shift. The geometry snaps, not a cursor dot.
/// Card 043: the move participants with the LINK chain pulled in — a
/// linked participant carries every OTHER linked layer with it (the current
/// model's single chain). Dedup preserves first-seen order; the flat set
/// traversal cannot cycle.
pub fn with_link_chain(participants: &[LayerId], linked: &[LayerId]) -> Vec<LayerId> {
    let mut out = participants.to_vec();
    if participants.iter().any(|p| linked.contains(p)) {
        for id in linked {
            if !out.contains(id) {
                out.push(*id);
            }
        }
    }
    out
}

pub fn snap_delta(
    base: raster::PixelRect,
    delta: Vec2,
    candidates: &[SnapCandidate],
    threshold_doc: f32,
) -> Vec2 {
    let mut out = delta;
    let min = Vec2::new(base.x as f32 + delta.x, base.y as f32 + delta.y);
    let max = Vec2::new(
        (base.x + base.width as i64) as f32 + delta.x,
        (base.y + base.height as i64) as f32 + delta.y,
    );
    let center = (min + max) * 0.5;
    for (axis, features) in [
        (SnapAxis::X, [min.x, max.x, center.x]),
        (SnapAxis::Y, [min.y, max.y, center.y]),
    ] {
        let mut best: Option<(f32, f32)> = None;
        for candidate in candidates {
            if candidate.axis != axis {
                continue;
            }
            for feature in features {
                let d = (candidate.doc - feature).abs();
                if d <= threshold_doc && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, candidate.doc));
                }
            }
        }
        if let Some((_, doc_value)) = best {
            let nearest = features
                .iter()
                .copied()
                .min_by(|a, b| (doc_value - a).abs().total_cmp(&(doc_value - b).abs()))
                .expect("three features");
            let shift = doc_value - nearest;
            if axis == SnapAxis::X {
                out.x += shift;
            } else {
                out.y += shift;
            }
        }
    }
    out
}

/// Card 042: which axis a snap candidate constrains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapAxis {
    /// The horizontal axis.
    X,
    /// The vertical axis.
    Y,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewState {
    /// Document point currently at the centre of the viewport.
    pub center: Vec2,
    /// Screen pixels per document pixel.
    pub zoom: f32,
    /// Clockwise view rotation, radians.
    pub rotation: f32,
    /// Viewport size in screen pixels.
    pub viewport: Vec2,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            center: Vec2::ZERO,
            zoom: 1.0,
            rotation: 0.0,
            viewport: Vec2::new(1280.0, 720.0),
        }
    }
}

impl ViewState {
    /// Smallest and largest zoom the navigation tools will settle on.
    pub const MIN_ZOOM: f32 = 1.0 / 256.0;
    pub const MAX_ZOOM: f32 = 256.0;

    /// Document space -> screen space.
    pub fn to_screen(&self) -> Mat3 {
        Mat3::from_translation(self.viewport * 0.5)
            * Mat3::from_scale(Vec2::splat(self.zoom))
            * Mat3::from_angle(self.rotation)
            * Mat3::from_translation(-self.center)
    }

    /// Screen space -> document space.
    ///
    /// Singular exactly when `zoom` is zero. Nothing in this crate produces
    /// that: [`ViewState::zoom_about`] and [`ViewState::set_zoom`] both clamp
    /// into `MIN_ZOOM..=MAX_ZOOM`, and the navigation tools go through them.
    /// `zoom` is a public field, though, so a caller that assigns it directly
    /// owns the invariant — assign `0.0` and this matrix is degenerate and
    /// [`ViewState::document_at`] returns non-finite points. Use
    /// [`ViewState::set_zoom`] and that cannot happen.
    pub fn to_document(&self) -> Mat3 {
        self.to_screen().inverse()
    }

    /// Set the zoom, clamped into `MIN_ZOOM..=MAX_ZOOM`; a non-finite value is
    /// ignored. The safe way to write the field.
    pub fn set_zoom(&mut self, zoom: f32) {
        if !zoom.is_finite() {
            return;
        }
        self.zoom = zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
    }

    pub fn document_at(&self, screen: Vec2) -> Vec2 {
        self.to_document().transform_point2(screen)
    }

    pub fn screen_at(&self, doc: Vec2) -> Vec2 {
        self.to_screen().transform_point2(doc)
    }

    /// Zoom about a fixed document point, keeping that point under the cursor.
    pub fn zoom_about(&mut self, doc_anchor: Vec2, factor: f32) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let before = self.screen_at(doc_anchor);
        self.zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        let after = self.screen_at(doc_anchor);
        // Move the centre so the anchor lands where it was.
        let drift = after - before;
        self.center += self.to_document().transform_vector2(drift);
    }
}

/// A repeating image a pattern-driven tool paints with, as straight-alpha sRGB8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Pattern {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, ToolError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(ToolError::RegionTooLarge {
                tiles: u64::MAX,
                max: crate::patch::MAX_PATCH_TILES,
            })?;
        if width == 0 || height == 0 || pixels.len() != expected {
            return Err(ToolError::Degenerate);
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// A solid one-pixel pattern — the simplest useful fixture.
    pub fn solid(rgba: [u8; 4]) -> Self {
        Self {
            width: 1,
            height: 1,
            pixels: rgba.to_vec(),
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Sample the pattern at a document position, tiling infinitely in both
    /// directions with the origin at `(0, 0)`.
    pub fn sample(&self, x: i64, y: i64) -> [u8; 4] {
        let px = x.rem_euclid(self.width as i64) as usize;
        let py = y.rem_euclid(self.height as i64) as usize;
        let i = (py * self.width as usize + px) * 4;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }
}

/// A selection change a tool wants applied.
///
/// The application folds this edit and turns the result into
/// `editor_core::Command::SetSelection` (card 056), so a selection gesture is
/// one undoable history step like any other edit. The edit rides its own
/// outbox because the FOLD needs the base selection and the boolean op — the
/// command carries only the result.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionEdit {
    /// The shape the gesture produced.
    pub incoming: Selection,
    /// How it combines with what is already selected.
    pub op: BooleanOp,
}

impl SelectionEdit {
    pub fn new(incoming: Selection, op: BooleanOp) -> Self {
        Self { incoming, op }
    }

    /// Fold this edit into `base`.
    pub fn apply(&self, canvas: Rect, base: &Selection) -> Result<Selection, ToolError> {
        Ok(selection::combine_selection(
            canvas,
            base,
            &self.incoming,
            self.op,
        )?)
    }
}

/// A crop, as the tool describes it.
///
/// Not a [`Command`], because `editor-core` has no canvas-resize command yet:
/// a crop changes [`editor_core::DocumentMeta::size`] and every layer's
/// position, and nothing in the command set expresses that. The tool therefore
/// reports what the user asked for and the application performs it. **This is a
/// real gap, not a design choice** — until a resize command exists, a crop is
/// not undoable through [`editor_core::History`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropRequest {
    /// The kept region, in current document pixels.
    pub rect: PixelRect,
    /// Rotation applied before cropping, radians clockwise.
    pub straighten: f32,
    /// Throw the cropped-away pixels away rather than keeping them off-canvas.
    pub delete_cropped: bool,
    /// W4-D: the exact pixel size the crop produces (the W x H x Resolution
    /// preset), or `None` to keep the kept region's own size. The region is
    /// scaled onto this canvas.
    pub output_size: Option<(u32, u32)>,
}

impl CropRequest {
    /// The four document-space corners the crop keeps, once `straighten` has
    /// been applied — clockwise from the top-left, matching
    /// [`crate::transform::TransformState`]'s corner order.
    ///
    /// This is the *whole* of what straightening means geometrically, and it is
    /// as far as this crate can take it: the resample that turns this quad back
    /// into an axis-aligned document needs the canvas-resize command that
    /// `editor-core` does not have yet (see the type's own docs). Handing the
    /// application the quad rather than only the angle at least means it does
    /// not have to re-derive the rotation convention, and pins that convention
    /// under test.
    ///
    /// Rotation is clockwise in a y-down document, about the rect's centre —
    /// the same direction the straighten slider moves. A `straighten` of zero
    /// gives back the rect's own corners exactly.
    pub fn straightened_corners(&self) -> [glam::Vec2; 4] {
        let cx = (self.rect.x as f32 + self.rect.right() as f32) * 0.5;
        let cy = (self.rect.y as f32 + self.rect.bottom() as f32) * 0.5;
        let center = glam::Vec2::new(cx, cy);
        let corners = [
            glam::Vec2::new(self.rect.x as f32, self.rect.y as f32),
            glam::Vec2::new(self.rect.right() as f32, self.rect.y as f32),
            glam::Vec2::new(self.rect.right() as f32, self.rect.bottom() as f32),
            glam::Vec2::new(self.rect.x as f32, self.rect.bottom() as f32),
        ];
        if !self.straighten.is_finite() || self.straighten == 0.0 {
            return corners;
        }
        let (s, c) = self.straighten.sin_cos();
        corners.map(|p| {
            let d = p - center;
            center + glam::Vec2::new(d.x * c - d.y * s, d.x * s + d.y * c)
        })
    }
}

/// One export slice.
#[derive(Debug, Clone, PartialEq)]
pub struct Slice {
    pub rect: PixelRect,
    pub name: String,
}

/// One keystroke aimed at a tool that is editing text.
///
/// Deliberately tiny. A full caret model — arrow keys, selection, home/end —
/// needs the glyph boxes `ui::canvas::text_overlay` computes and a click that
/// lands inside an existing run, and neither is wired; growing this enum ahead
/// of that would be inventing a text editor nothing drives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextEdit<'a> {
    /// Insert this text at the caret, replacing any selected range.
    Insert(&'a str),
    /// Remove the character before the caret, or the selected range.
    Backspace,
    /// End the session, committing the draft as one history entry (card 025).
    Confirm,
    /// End the session without committing: an existing layer returns to its
    /// original payload with no history entry; a layer this session created
    /// is deleted.
    Cancel,
    /// Move the caret one character (`back` = towards the start); `extend`
    /// keeps the anchor (Shift-selection). Card 027.
    CaretStep { back: bool, extend: bool },
    /// Move to the current paragraph's start or end (Home/End).
    ParagraphEdge { end: bool, extend: bool },
    /// Move one word (card 027's word movement).
    WordStep { forward: bool, extend: bool },
    /// Select the whole draft (Ctrl+A inside a session — card 028 routes it).
    SelectAll,
    /// Delete the character after the caret, or the selected range (the
    /// forward Delete key). Card 028.
    DeleteForward,
    /// Replace the selection (or insert at the caret) with clipboard text.
    /// The clipboard I/O happens in the shell; the session sees only text.
    /// Card 028.
    PasteText(&'a str),
    /// Begin/replace the live IME preedit at the caret (card 029: the shell
    /// routes winit's `Ime::Preedit` here).
    SetComposition(&'a str),
    /// Commit the IME composition: the preedit becomes draft text and the
    /// committed string is inserted in its place (winit's `Ime::Commit`).
    CommitIme(&'a str),
    /// Withdraw the composition without committing (an empty `Ime::Preedit`
    /// or the platform cancelling it). Card 029.
    ClearComposition,
    /// Resize the paragraph box to `width` (a `None` height keeps it auto).
    /// Reflows the draft without touching the layer's affine scale (card
    /// 032: the box is the paragraph's geometry, nothing else's). A point
    /// run converts to a box.
    ResizeBox {
        /// The wrapping width, in layer pixels.
        width: f32,
        /// `None` keeps the auto height.
        height: Option<f32>,
    },
}

/// Something a tool wants that is not a [`Command`] and not a selection.
///
/// The two outboxes are separate because a selection edit is by far the most
/// common non-command result and deserves a typed accessor; everything else
/// shares this one.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolRequest {
    Crop(CropRequest),
    Slices(Vec<Slice>),
    /// Make this layer the document's selection: Path Select clicked a shape
    /// layer's path. Consumed by the shell, not by the tool's own command
    /// stream — selection is a field write, not history.
    SelectLayer(LayerId),
    /// Render this layer with this payload *outside history*: the live text
    /// draft (card 025). Keystrokes must be visible but must not be undoable
    /// one by one, so the draft lands on the document directly and the
    /// session's confirm/cancel reconciles — confirm emits the one
    /// `SetLayerKind` history entry, cancel restores the original payload the
    /// same direct way.
    TextDraft {
        layer: LayerId,
        kind: Box<LayerKind>,
    },
    /// Commit a finished text session atomically (card 025): the draft has
    /// been living on the document outside history, so the shell first
    /// restores the original payload (direct, no history) and THEN applies
    /// `Command::SetLayerKind(draft)` — the command's inverse is captured
    /// from the restored original, so one Ctrl+Z takes the whole session
    /// back, and cancel-style restores stay history-free.
    TextConfirm {
        layer: LayerId,
        original: Box<LayerKind>,
        draft: Box<LayerKind>,
    },
    /// Card 036: commit one whole-layer affine for EVERY selected participant
    /// as ONE undoable transaction. The tool supplies the document-space
    /// delta and the (already lock-checked, ancestor-normalized) set; the
    /// shell conjugates per participant through its own parent chain and
    /// wraps the batch in a `Command::Transaction`.
    TransformLayers {
        /// The normalized participant set.
        layers: Vec<LayerId>,
        /// The gizmo's corner delta in document space (column-major).
        delta: [f32; 6],
    },
}

/// Everything a tool may read, plus the outboxes for everything it wants
/// changed.
/// Card 026: an existing text layer hit by a Type click, resolved by the
/// shell — the layer to enter, its payload as the session's original, and the
/// caret byte index the shaped hit test chose.
#[derive(Debug, Clone)]
pub struct TextHitCaret {
    pub layer: LayerId,
    pub original: TextLayer,
    pub caret: usize,
}

/// W9-D: the options-bar key of the retouching tools' Sample choice.
pub const SAMPLE_LAYERS_KEY: &str = "sample";

/// W9-D: which layers a retouching stroke reads its source pixels from — the
/// Sample choice of the Clone Stamp, the Healing Brush, the Spot Healing
/// Brush, Blur, Sharpen and Smudge. [`SampleLayers::Current`] reads the layer
/// being painted, as every stroke did before; the other two read the
/// composite the shell hands over through [`ToolContext::composite_sampler`]
/// and still write onto the active layer, which may be empty (non-destructive
/// retouching on a layer of its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SampleLayers {
    /// The layer being painted.
    #[default]
    Current,
    /// The composite of the visible layers at and below the active one.
    CurrentAndBelow,
    /// The composite of every visible layer.
    All,
}

impl SampleLayers {
    /// The options bar's labels, in [`SampleLayers::from_choice`] order.
    pub const CHOICES: &'static [&'static str] =
        &["Current Layer", "Current & Below", "All Layers"];

    /// The variant a Choice index names; an index past the end clamps to the
    /// last entry, the options bar's own rule.
    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => SampleLayers::Current,
            1 => SampleLayers::CurrentAndBelow,
            _ => SampleLayers::All,
        }
    }
}

/// W9-D: the composite a retouching stroke with a Sample other than Current
/// Layer reads, computed by the shell on demand — only over the rects the
/// stroke asks for, so a stroke composites the tiles it reaches and nothing
/// else. The shell builds one at the press from the document as committed
/// then; the stroke keeps it until its release, so what it reads cannot move
/// under it.
pub trait CompositeSampler: Send + Sync {
    /// The composite of `layers` (never [`SampleLayers::Current`]) over
    /// `rect`, which is in the PAINT TARGET's pixel space (the active layer's
    /// pixels): linear, premultiplied RGBA, `rect.width × rect.height`,
    /// row-major.
    fn composite(
        &self,
        layers: SampleLayers,
        rect: PixelRect,
    ) -> Result<filters::FilterBuffer, ToolError>;
}

pub struct ToolContext<'a> {
    /// The layer the tool edits.
    pub active_layer: Option<LayerId>,
    /// The mask attached to that layer, if the app wants mask edits routed.
    pub active_mask: Option<MaskId>,
    /// Whether pixel tools write to the layer or to its mask.
    pub paint_target: PaintTarget,
    /// The document's pixel bounds — what a crop, a fill or an invert is
    /// relative to.
    pub canvas: PixelRect,
    /// The current selection. Painting is multiplied by its coverage, so
    /// [`Selection::None`] (coverage 1.0 everywhere) is "paint anywhere".
    pub selection: Selection,
    /// Foreground colour, straight-alpha **linear** RGBA.
    pub foreground: [f32; 4],
    /// Background colour, straight-alpha **linear** RGBA.
    pub background: [f32; 4],
    /// The ramp the gradient tools paint with, converted from the UI's
    /// gradient model. The options bar and the dialog edit the workspace's
    /// copy; the application threads it here per gesture, the same way the
    /// foreground and background travel.
    pub ramp: GradientRamp,
    /// The active pattern, for the pattern-driven tools.
    pub pattern: Option<Pattern>,
    /// Where a "sample all layers" read comes from — typically the cached
    /// flattened composite. `None` means sample the active layer.
    pub sample_from: Option<PixelKey>,
    /// The canvas view, mutated in place by the navigation tools.
    pub view: ViewState,
    /// The layers under the pointer, topmost first — what auto-select walks.
    pub layer_stack: Vec<LayerId>,
    /// Card 026: the existing text layer under this sample's click, resolved
    /// by the shell (which owns the shaping stack) — `None` unless the tool
    /// is the Type tool and a text layer was hit. `on_pointer_up` enters that
    /// layer instead of creating one.
    pub text_hit: Option<TextHitCaret>,
    /// Every shape layer, as `(layer, its whole shape definition)` pairs,
    /// top-most first — the same order [`Self::layer_stack`] walks. Path
    /// Select hit-tests the paths; Direct Selection edits the active layer's
    /// anchors and rebuilds its kind from these. Filled by the shell; empty
    /// by default.
    pub shape_paths: Vec<(LayerId, layer_model::ShapeLayer)>,
    /// W9-K: the outlines a Type click may start Type on a Path along, as SVG
    /// path data in DOCUMENT space: the Paths panel's current path (the
    /// selected one, else the Work Path) first, then every visible shape
    /// layer's outline through its layer transform, top-most first. Filled by
    /// the shell for the Type tools only; empty by default.
    pub type_path_outlines: Vec<String>,
    /// Card 035: the ACTIVE layer's parent chain as one transform (parent
    /// space → document space), filled by the shell from the compositor's
    /// parent convention. A whole-layer transform delta computed in document
    /// space conjugates through this before landing on the layer's own
    /// transform: `delta_parent = P^-1 * delta_doc * P`. `None` = identity.
    pub active_layer_parent_transform: Option<glam::Affine2>,
    /// Card 042: document-space snap candidates for the moving geometry
    /// (canvas edges/center + every layer NOT in the selected set), filled
    /// by the shell. The Move tool snaps its drag target against them.
    pub snap_candidates: Vec<SnapCandidate>,
    /// Card 042: the snap threshold in DOCUMENT pixels (the screen threshold
    /// converted through the current zoom), filled by the shell.
    pub snap_threshold_doc: f32,
    /// Card 042: the ACTIVE layer's TIGHT ink extent in document space
    /// (alpha scan for raster kinds, exact geometry otherwise). The Move
    /// tool snaps against this — tile-level bounds are too coarse to
    /// describe the visible edges.
    pub active_layer_ink_bounds: Option<PixelRect>,
    /// Card 043: every layer carrying the link flag (the current model's
    /// ONE chain). The move commits pull the whole chain in when any
    /// participant is linked.
    pub linked_layers: Vec<LayerId>,
    /// Card 044: the ACTIVE layer keeps editable geometry (Text, Shape,
    /// SmartObject). The transform tool refuses the pixel-patch modes on it
    /// rather than silently rasterizing.
    pub active_layer_parametric: bool,
    /// Card 040: the ACTIVE layer's document→layer-pixel mapping, filled by
    /// the shell for paintable kinds. Paint tools route their samples
    /// through it, so a moved/scaled layer is painted where the pointer
    /// displays, not at raw document coordinates. `None` = identity.
    /// Card 058: for a MASK paint target this carries the mask POSE's
    /// inverse (layer transform ∘ mask.transform), so coverage lands where
    /// the compositor samples it.
    pub sample_to_layer: Option<glam::Affine2>,
    /// Card 058: the canvas rectangle expressed in the PAINT TARGET's space
    /// — the identity answer for content, the bounding box of the canvas
    /// pre-imaged through the mask pose for mask painting. Stroke dabs live
    /// in target space, so this (not `canvas`, which is document space) is
    /// the rect that clipping and rasterization must be measured against.
    /// `None` = same as `canvas`.
    pub paint_space_canvas: Option<PixelRect>,
    /// Card 037: the rendered-content pick under this sample's pointer,
    /// computed by the shell through the bounded visible-content test. The
    /// outer `None` means "no shell ran" (direct-begin sessions keep their
    /// raw-sampler fallback); the inner `None` means the shell's bounded
    /// test found NO visible content — a real answer, not an absence of one.
    /// Other tools ignore it.
    pub content_pick: Option<Option<LayerId>>,
    /// Card 036: the document's selected layer set (the panel's Shift
    /// selection), as recorded by the shell. The tool normalizes and
    /// lock-checks it into its session target.
    pub selected_layers: Vec<LayerId>,
    /// Card 036: every layer's parent (child → parent or `None` for roots),
    /// filled by the shell — the ancestry the tool normalizes against.
    pub layer_parents: Vec<(LayerId, Option<LayerId>)>,
    /// Card 036: every layer's whole-lock flag, filled by the shell — the
    /// all-or-nothing refusal reads it.
    pub layer_locks: Vec<(LayerId, bool)>,
    /// Card 036: every layer's parent-chain transform (parent space →
    /// document space), filled by the shell — the session's representative
    /// layer conjugates its delta through this.
    pub layer_parent_transforms: Vec<(LayerId, glam::Affine2)>,
    /// Pixel bytes.
    pub tiles: &'a mut dyn TileAccess,
    /// Card 034: the ACTIVE layer's content bounds (the real stored extent
    /// through the compositor's bounds query), filled by the shell. A
    /// transform with no pixel selection surrounds this — the logo, the
    /// text — instead of the whole canvas. `None` when the layer has no ink
    /// or the shell has nothing to say; tools fall back to the canvas.
    pub active_layer_content_bounds: Option<PixelRect>,
    /// W4-G: the History Brush's source — an earlier state of this document
    /// (the opened state unless the user picked another), filled by the shell
    /// at the press that begins a History Brush stroke. `None` when the shell
    /// has none to offer.
    pub history_source: Option<std::sync::Arc<crate::history_brush::HistorySource>>,
    /// W4-G: the document's Colour Sampler points (pixel centres, document
    /// pixels, placement order). They belong to the document, not to a tool
    /// instance, so they survive a tool switch; the shell lends them here and
    /// the Colour Sampler edits them in place. `None` when the shell has no
    /// document to lend them from.
    pub samplers: Option<&'a mut Vec<Vec2>>,
    /// W8-D: the shell can run a heavy stroke finish off the interaction
    /// thread. When `true`, a tool whose release would synthesise for longer
    /// than a frame (the Spot Healing Brush's Content-Aware type) emits
    /// nothing at release and hands the work over through
    /// [`Tool::take_deferred_commit`] instead; `false` (the default) keeps
    /// the release synchronous, emitting its command as every other tool does.
    pub defer_heavy_commits: bool,
    /// W8-C: the layers a Perspective Crop commit rectifies, each with its
    /// document→layer-pixel map, in depth-first order. The shell fills it
    /// only for the Perspective Crop tool, with every Raster and Generator
    /// layer in the document (hidden ones included) whose document transform
    /// inverts; each listed layer is resampled and commits one
    /// [`Command::PaintTiles`]. Groups, adjustment, text, shape and smart
    /// object layers are not listed: they are not rectified and keep their
    /// own geometry. Empty by default (a bare harness), in which case the
    /// active layer is the one there is.
    pub rectify_layers: Vec<(LayerId, glam::Affine2)>,
    /// W9-D: the composite a retouching stroke's Sample choice reads when it
    /// is not Current Layer, filled by the shell at the press that begins
    /// such a stroke (see [`CompositeSampler`]). `None` in a bare harness and
    /// for every other tool; a stroke asking for it then is refused
    /// ([`ToolError::Degenerate`], the refusal a clone stamp with no source
    /// gives) rather than silently reading the active layer instead.
    pub composite_sampler: Option<std::sync::Arc<dyn CompositeSampler>>,

    commands: Vec<Command>,
    selection_edits: Vec<SelectionEdit>,
    requests: Vec<ToolRequest>,
    picked: Option<[f32; 4]>,
}

impl<'a> ToolContext<'a> {
    /// Card 036: a layer's parent, from the shell-filled map.
    pub fn parent_of(&self, layer: LayerId) -> Option<LayerId> {
        self.layer_parents
            .iter()
            .find(|(id, _)| *id == layer)
            .and_then(|(_, parent)| *parent)
    }

    /// Card 036: whether a layer is whole-locked, from the shell-filled map.
    pub fn layer_lock(&self, layer: LayerId) -> Option<bool> {
        self.layer_locks
            .iter()
            .find(|(id, _)| *id == layer)
            .map(|(_, locked)| *locked)
    }

    /// Card 036: a layer's parent-chain transform, from the shell-filled map.
    pub fn parent_transform_of(&self, layer: LayerId) -> Option<glam::Affine2> {
        self.layer_parent_transforms
            .iter()
            .find(|(id, _)| *id == layer)
            .map(|(_, transform)| *transform)
    }

    /// A context over `tiles` with nothing selected and no active layer.
    pub fn new(tiles: &'a mut dyn TileAccess, canvas: PixelRect) -> Self {
        Self {
            active_layer_content_bounds: None,
            history_source: None,
            samplers: None,
            defer_heavy_commits: false,
            rectify_layers: Vec::new(),
            composite_sampler: None,
            active_layer_parent_transform: None,
            snap_candidates: Vec::new(),
            snap_threshold_doc: 8.0,
            active_layer_ink_bounds: None,
            linked_layers: Vec::new(),
            active_layer_parametric: false,
            sample_to_layer: None,
            paint_space_canvas: None,
            content_pick: None,
            selected_layers: Vec::new(),
            layer_parents: Vec::new(),
            layer_locks: Vec::new(),
            layer_parent_transforms: Vec::new(),
            active_layer: None,
            active_mask: None,
            paint_target: PaintTarget::Layer,
            canvas,
            selection: Selection::None,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 1.0],
            ramp: GradientRamp::black_to_white(),
            pattern: None,
            sample_from: None,
            view: ViewState::default(),
            layer_stack: Vec::new(),
            text_hit: None,
            shape_paths: Vec::new(),
            type_path_outlines: Vec::new(),
            tiles,
            commands: Vec::new(),
            selection_edits: Vec::new(),
            requests: Vec::new(),
            picked: None,
        }
    }

    pub fn with_layer(mut self, id: LayerId) -> Self {
        self.active_layer = Some(id);
        self
    }

    pub fn with_foreground(mut self, rgba: [f32; 4]) -> Self {
        self.foreground = rgba;
        self
    }

    /// Queue a command for history.
    pub fn emit(&mut self, cmd: Command) {
        self.commands.push(cmd);
    }

    /// Queue a selection change.
    pub fn emit_selection(&mut self, edit: SelectionEdit) {
        self.selection_edits.push(edit);
    }

    /// Queue a crop or a slice set.
    pub fn emit_request(&mut self, req: ToolRequest) {
        self.requests.push(req);
    }

    /// Requests queued so far, without draining.
    pub fn requests(&self) -> &[ToolRequest] {
        &self.requests
    }

    pub fn drain_requests(&mut self) -> Vec<ToolRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Record a colour the eyedropper picked.
    pub fn set_picked(&mut self, rgba: [f32; 4]) {
        self.picked = Some(rgba);
        self.foreground = rgba;
    }

    /// The last colour an eyedropper picked, if any.
    pub fn picked(&self) -> Option<[f32; 4]> {
        self.picked
    }

    /// Commands queued so far, without draining.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Selection edits queued so far, without draining.
    pub fn selection_edits(&self) -> &[SelectionEdit] {
        &self.selection_edits
    }

    /// Take everything queued.
    pub fn drain(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    pub fn drain_selection(&mut self) -> Vec<SelectionEdit> {
        std::mem::take(&mut self.selection_edits)
    }

    /// The canvas as a `selection::Rect`, which is what the boolean ops and
    /// `invert` measure "everything" against.
    pub fn canvas_rect(&self) -> Rect {
        Rect::from_xywh(
            self.canvas.x.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            self.canvas.y.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            self.canvas.width,
            self.canvas.height,
        )
    }

    /// What a pixel command should name.
    pub fn pixel_target(&self) -> Result<PixelTarget, ToolError> {
        let id = self.active_layer.ok_or(ToolError::NoActiveLayer)?;
        Ok(match self.paint_target {
            PaintTarget::Layer => PixelTarget::Layer(id),
            PaintTarget::Mask => PixelTarget::Mask(id),
        })
    }

    /// What the tile store is keyed by for the current paint target.
    pub fn pixel_key(&self) -> Result<PixelKey, ToolError> {
        match self.paint_target {
            PaintTarget::Layer => Ok(PixelKey::Layer(
                self.active_layer.ok_or(ToolError::NoActiveLayer)?,
            )),
            PaintTarget::Mask => Ok(PixelKey::Mask(
                self.active_mask.ok_or(ToolError::NoActiveLayer)?,
            )),
        }
    }

    /// Refuse a gesture that has no meaning on an 8-bit coverage mask.
    ///
    /// The tools that *do* mean something there — the brush, the fills, the
    /// gradient, the shape rasteriser, the free transform — branch on
    /// [`ToolContext::paint_target`] and load a
    /// [`crate::patch::CoveragePatch`] instead. Everything whose whole job is
    /// to read or write colour (red-eye, patch, the magic eraser) calls this
    /// first, because [`editor_core::Command::PaintTiles`] would otherwise
    /// happily store a four-byte-per-pixel tile in a one-byte-per-pixel mask
    /// slot; nothing downstream checks.
    pub fn require_layer_target(&self) -> Result<(), ToolError> {
        match self.paint_target {
            PaintTarget::Layer => Ok(()),
            PaintTarget::Mask => Err(ToolError::UnsupportedOnMask),
        }
    }

    /// Where a colour-reading tool should sample from.
    pub fn sample_key(&self) -> Result<PixelKey, ToolError> {
        match self.sample_from {
            Some(k) => Ok(k),
            None => Ok(PixelKey::Layer(
                self.active_layer.ok_or(ToolError::NoActiveLayer)?,
            )),
        }
    }

    /// W9-D: the sampled composite of `layers` over `rect` (paint-target
    /// pixels), or `None` for [`SampleLayers::Current`] — the caller reads
    /// the layer itself then. Refused when the shell lent no sampler.
    pub fn sampled_composite(
        &self,
        layers: SampleLayers,
        rect: PixelRect,
    ) -> Option<Result<filters::FilterBuffer, ToolError>> {
        if layers == SampleLayers::Current {
            return None;
        }
        Some(match &self.composite_sampler {
            Some(sampler) => sampler.composite(layers, rect),
            None => Err(ToolError::Degenerate),
        })
    }

    /// How much of one pixel the current selection lets an edit through.
    pub fn clip_at(&self, p: glam::IVec2) -> f32 {
        self.selection.coverage_at(p)
    }
}

/// One typed option value a shell forwards to a tool, keyed by the option
/// spec's key. Tools-owned on purpose: the options bar's own value type lives
/// in the UI crate, and this is the boundary — the shell converts, the tool
/// consumes, and no UI type crosses into tools (plan card 010).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolSetting {
    Float(f32),
    Int(i32),
    Bool(bool),
    /// Index into the spec's `choices`, which the tool's ordering must match.
    Choice(usize),
    /// Straight-alpha sRGB.
    Color([f32; 4]),
}

/// One live tool session's overlay geometry, as the shell publishes it (plan
/// card 012). Tools-owned: the shell converts it into the canvas sessions the
/// overlays are drawn from. A variant exists per tool whose gesture is
/// visible while it runs; a tool with no live session answers `None` from
/// [`Tool::live_geometry`].
#[derive(Debug, Clone, PartialEq)]
pub enum SessionGeometry {
    /// A free-transform session: the live state, the mode it is in, and the
    /// handle being dragged (emphasised).
    Transform {
        state: crate::transform::TransformState,
        mode: crate::transform::TransformMode,
        active: Option<crate::transform::Handle>,
        /// The layer being transformed, captured at pointer-down — what the
        /// preview (card 013) overrides and what the commit will edit.
        layer: Option<LayerId>,
    },
    /// W4-A: a crop box — being dragged, or released and waiting for Enter.
    /// `rect` is `[min, max]` in document pixels; `guide` is the composition
    /// guide drawn inside it. W4-D round 2: `straighten` is the Straighten
    /// line, `[from, to]` in document pixels, while it is dragged and after
    /// release until Enter or Escape; the painter draws it with the angle
    /// ([`crate::edit::straighten_angle`]) it will level.
    Crop {
        rect: [Vec2; 2],
        guide: CropGuide,
        straighten: Option<[Vec2; 2]>,
    },
    /// W4-A: a marquee's rubber band while the button is down. `rect` is
    /// `[min, max]` in document pixels, with the modifier constraints
    /// (square, from-centre) already applied; for the single-row/column
    /// marquees it is the one-pixel line across the canvas.
    Marquee {
        shape: crate::select::MarqueeShape,
        rect: [Vec2; 2],
    },
    /// W4-A: a lasso outline so far, in document pixels. `closed` means the
    /// release closes it (freehand, magnetic), so the loop is drawn shut;
    /// open (polygonal) means the next vertex is still to come, and the
    /// painter draws a rubber segment from the last vertex to the pointer.
    Lasso { points: Vec<Vec2>, closed: bool },
    /// W4-A: a pen path being authored. `anchors` are the anchor positions,
    /// `handles[i]` the absolute `[in, out]` control points of anchor `i`
    /// (equal to the anchor when straight), all in document pixels.
    /// `closing` is true while the press that closes the path on its first
    /// anchor is held.
    Path {
        anchors: Vec<Vec2>,
        handles: Vec<[Vec2; 2]>,
        closing: bool,
    },
    /// W4-A: the slices drawn and not yet committed, plus the one being
    /// dragged, each `[min, max]` in document pixels, numbered in order.
    Slices { rects: Vec<[Vec2; 2]> },
    /// W4-G: the Ruler's line, `start` to `end` in document pixels — drawn
    /// over the canvas and read into the Info panel's Distance and Angle rows.
    /// Held after release, until the next drag, a click, Escape or Straighten.
    Measure { start: Vec2, end: Vec2 },
    /// W8-C: a Perspective Crop quad, clockwise from the top-left, in
    /// document pixels, with the corner being dragged (emphasised). The
    /// painter draws the outline, a perspective grid and a handle on each
    /// corner.
    PerspectiveCrop {
        quad: [Vec2; 4],
        active: Option<usize>,
    },
    /// W8-C: a Type Mask session over the temporary text layer `layer`: the
    /// painter lays the quick-mask red over everything outside its glyphs.
    TypeMask { layer: LayerId },
}

/// W4-A: the composition guide a published [`SessionGeometry::Crop`] asks
/// for inside its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CropGuide {
    /// No guide lines.
    None,
    /// Two lines each way at the thirds — the default.
    #[default]
    Thirds,
    /// W4-D: the options bar's Overlay choice — a dense grid.
    Grid,
    /// W4-D: both diagonals.
    Diagonals,
    /// W4-D: the golden-section lines.
    GoldenRatio,
}

/// A live numeric readout a running gesture wants shown by the pointer —
/// the shape tools' W/H while a box is being dragged (Photopea's cursor
/// label). Published through [`Tool::live_readout`], deliberately separate
/// from [`SessionGeometry`]: a readout is a label, not overlay geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveReadout {
    /// The dragged box's width, in document pixels.
    pub width_px: f32,
    /// The dragged box's height, in document pixels.
    pub height_px: f32,
    /// Where the label belongs, in document space: the pointer's position.
    pub anchor: Vec2,
}

/// The interface every interactive tool implements.
///
/// Object safe on purpose: [`crate::registry`] hands the UI a
/// `Box<dyn Tool>` so the UI never needs a match over [`ToolId`].
pub trait Tool {
    fn id(&self) -> ToolId;

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError>;

    fn on_pointer_move(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError>;

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError>;

    /// Abandon an in-progress gesture (Esc). Must emit nothing and must leave
    /// the tool reusable.
    fn cancel(&mut self, ctx: &mut ToolContext<'_>);

    /// Confirm a gesture the tool is *holding* rather than one it has finished
    /// (Enter, or the options bar's Apply).
    ///
    /// Three tools work this way and none of them could be reached without it.
    /// [`crate::edit::CropTool`] draws its box on release and waits, so the
    /// user can nudge the edges before the cut; [`crate::edit::SliceTool`]
    /// accumulates regions across several drags and publishes the set once;
    /// [`crate::transform::TransformTool`] keeps a live quad the handles move
    /// and resamples exactly once. Each has
    /// an inherent `commit` of its own and this is what makes it reachable
    /// through a `Box<dyn Tool>` — an application holding one had no way to
    /// call it, so a crop drag produced a box and never a pixel.
    ///
    /// Defaulted to "nothing to confirm", which is the truth for every tool
    /// whose gesture ends at pointer-up. Emits through the same outboxes a
    /// pointer sample does, so a committed gesture is one undoable step.
    fn commit(&mut self, _ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        Ok(())
    }

    /// Whether [`Tool::commit`] has something to confirm right now.
    ///
    /// The read half: it is what lets a shell answer Enter with "the crop was
    /// applied" or leave the key to the keymap, instead of guessing.
    fn has_pending_commit(&self) -> bool {
        false
    }

    /// `true` while the tool has a text run open, so the keyboard belongs to
    /// the canvas rather than to the shortcut table.
    ///
    /// Only [`crate::text::TypeTool`] ever answers `true`. It is on the trait
    /// rather than reached by downcasting because a shell holds a
    /// `Box<dyn Tool>` and has no other way to ask.
    fn is_text_editing(&self) -> bool {
        false
    }

    /// The layer a live text session is editing, if any (card 026): the shell
    /// reads it to refuse text operations against a different document than
    /// the one the run lives in.
    fn text_session_layer(&self) -> Option<LayerId> {
        None
    }

    /// The live text session's selected text — the OS clipboard copy source
    /// (card 028). Tools without a live session return `None`.
    fn text_selection_text(&self) -> Option<String> {
        None
    }

    /// The live session's caret and anchor byte indices, in that order —
    /// the canvas overlay's geometry source (card 030). `None` without a
    /// session.
    fn text_caret_anchor(&self) -> Option<(usize, usize)> {
        None
    }

    /// Whether an IME composition is live in this tool's session (card 029):
    /// while it is, the shell must not double-insert plain characters — the
    /// platform delivers the text through `Ime::Preedit`/`Ime::Commit`.
    fn text_composing(&self) -> bool {
        false
    }

    /// Card 026: begin a text session ENTERING an existing layer (never
    /// creating): the shell resolves the payload, caret and origin from the
    /// document. Only the Type tool implements it.
    fn enter_text_session(
        &mut self,
        _layer: LayerId,
        _original: TextLayer,
        _caret: usize,
        _origin: Vec2,
    ) -> bool {
        false
    }

    /// Feed one keystroke to a tool that is editing text.
    ///
    /// Defaulted to a refusal rather than to a no-op: a shell that routes the
    /// keyboard at a tool which is not typing has routed it to the wrong place,
    /// and a swallowed key is exactly how that stays invisible.
    fn text_edit(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _edit: TextEdit<'_>,
    ) -> Result<(), ToolError> {
        Err(ToolError::NotStarted)
    }

    /// Adopt the application's brush.
    ///
    /// The colours reach a tool through [`ToolContext`], but the brush cannot:
    /// dab shape belongs to the tool's own state machine, not to the context it
    /// reads, and [`crate::registry::make`] hands back a `Box<dyn Tool>` with no
    /// way to reach the settings inside. An application whose options bar and
    /// `[`/`]` keys own one brush therefore has no way to tell the tool what
    /// size to paint — which is exactly the seam an application shell needs.
    ///
    /// Defaulted to a no-op: most tools have no brush, and the ones that do are
    /// free to refuse a change that would disagree with a gesture already in
    /// progress.
    fn set_brush(&mut self, _brush: BrushSettings) {}

    /// Adopt the application's choice for a named option — the transform
    /// tool's `mode` (Scale/Rotate/Skew/…) and `target` (Layer/Selection).
    /// `key` is the spec's key; `index` is the choice's position, which the
    /// registry's spec and the tool's own ordering must agree on.
    ///
    /// Defaulted to a no-op: most tools have no choice options.
    fn set_choice(&mut self, _key: &str, _index: usize) {}

    /// Adopt one typed option value by key — the typed forwarding seam (plan
    /// card 010). The shell converts its own option values into this
    /// tools-owned enum at the boundary, so UI types never reach a tool.
    ///
    /// Default behaviour: a `Choice` routes to [`Tool::set_choice`] so the
    /// existing choice tools keep working unchanged; every other kind (and a
    /// Choice on a tool with no such option) is refused with
    /// [`ToolError::UnknownOption`]. A tool that declares options in the
    /// registry implements this and answers for exactly its spec'd keys — an
    /// unknown or mismatched key is an error the shell can surface, never a
    /// silent no-op.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match setting {
            ToolSetting::Choice(index) => {
                self.set_choice(key, index);
                Ok(())
            }
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }

    /// The brush this tool stamps with, when it has one.
    ///
    /// The read half of [`Tool::set_brush`], and the reason it exists is that
    /// the settings [`crate::registry::make`] builds a tool with *are* that
    /// tool: the Pencil is nothing but `BrushSettings::pencil(1.0)` — one hard
    /// aliased pixel with no pressure — and the Clone Stamp nothing but a soft
    /// 40 at 0.05 spacing. A shell that keeps one brush per tool so `[` and `]`
    /// move the tool the user is looking at needs somewhere to seed each slot
    /// from, and duplicating the registry's table in the shell is how the two
    /// would drift.
    ///
    /// `None` for a tool that stamps no dabs at all — the marquees, the
    /// gradient, the view tools.
    fn brush(&self) -> Option<BrushSettings> {
        None
    }

    /// Whether a gesture is currently in progress.
    fn is_active(&self) -> bool;

    /// The overlay geometry a live session wants published, or `None` when
    /// the tool shows nothing (card 012). Read every frame by the shell —
    /// cheap, because it borrows the tool's own state — and published into
    /// the canvas sessions the overlays are drawn from. Cleared by returning
    /// `None`: the same route that publishes also un-publishes, so Escape or
    /// a committed gesture removes the handles with no second mechanism to
    /// forget.
    fn live_geometry(&self) -> Option<SessionGeometry> {
        None
    }

    /// The numeric readout a live gesture wants shown beside the pointer, or
    /// `None` when there is nothing to show. The app shell reads it after
    /// every pointer sample, next to [`Tool::live_geometry`], and again after
    /// it cancels a gesture (Escape, focus loss); a released or cancelled
    /// gesture answers `None`, and that re-read is what takes the label down.
    fn live_readout(&self) -> Option<LiveReadout> {
        None
    }

    /// W4-B: the in-flight pixels of a running stroke, for the shell's live
    /// preview lens — or `None` when nothing changed since the last call or
    /// the tool paints nothing before it commits. Emits nothing and writes
    /// nothing to `ctx.tiles`; the stroke tools answer it
    /// ([`crate::stroke::StrokeTool::live_paint`]), every other tool keeps
    /// this default.
    fn live_paint(
        &mut self,
        _ctx: &mut ToolContext<'_>,
    ) -> Result<Option<crate::stroke::LivePaint>, ToolError> {
        Ok(None)
    }

    /// W8-D: the heavy finish a release handed over instead of emitting,
    /// when the context asked for it ([`ToolContext::defer_heavy_commits`]).
    /// Taken once, right after the pointer-up: the shell runs the synthesis
    /// on a worker and lands [`crate::stroke::DeferredStroke::finish`]'s
    /// command as the stroke's one history entry. Every tool whose release
    /// is cheap keeps this default.
    fn take_deferred_commit(&mut self) -> Option<crate::stroke::DeferredStroke> {
        None
    }

    /// W8-C: for a Type Mask tool, how its confirmed glyphs combine with the
    /// existing selection (the options bar's New / Add / Subtract /
    /// Intersect); `None` for every other tool.
    fn type_mask_op(&self) -> Option<BooleanOp> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;

    #[test]
    fn a_straightened_crop_reports_a_rotated_quad_about_the_rects_centre() {
        let plain = CropRequest {
            rect: PixelRect::new(10, 20, 100, 40),
            straighten: 0.0,
            delete_cropped: false,
            output_size: None,
        };
        // No straighten: the rect's own corners, clockwise from top-left.
        assert_eq!(
            plain.straightened_corners(),
            [
                glam::Vec2::new(10.0, 20.0),
                glam::Vec2::new(110.0, 20.0),
                glam::Vec2::new(110.0, 60.0),
                glam::Vec2::new(10.0, 60.0),
            ]
        );

        // A quarter turn clockwise in a y-down document: the top-left corner
        // swings to where the bottom-left was.
        let turned = CropRequest {
            straighten: std::f32::consts::FRAC_PI_2,
            ..plain
        };
        let q = turned.straightened_corners();
        let center = glam::Vec2::new(60.0, 40.0);
        assert!(
            (q[0] - glam::Vec2::new(80.0, -10.0)).length() < 1e-3,
            "{q:?}"
        );
        // The quad is rigid: same centre, same edge lengths, same diagonals.
        let mid = q.iter().fold(glam::Vec2::ZERO, |a, b| a + *b) / 4.0;
        assert!((mid - center).length() < 1e-3, "centre moved to {mid:?}");
        for i in 0..4 {
            let before = (plain.straightened_corners()[(i + 1) % 4]
                - plain.straightened_corners()[i])
                .length();
            let after = (q[(i + 1) % 4] - q[i]).length();
            assert!(
                (before - after).abs() < 1e-3,
                "edge {i}: {before} -> {after}"
            );
        }

        // A non-finite angle degrades to no rotation rather than to NaN
        // corners, because a NaN quad would be resampled into the document.
        let bad = CropRequest {
            straighten: f32::NAN,
            ..plain
        };
        assert_eq!(bad.straightened_corners(), plain.straightened_corners());
    }

    #[test]
    fn modifiers_map_to_the_conventional_boolean_ops() {
        assert_eq!(Modifiers::NONE.selection_op(), BooleanOp::Replace);
        assert_eq!(Modifiers::shift().selection_op(), BooleanOp::Add);
        assert_eq!(Modifiers::alt().selection_op(), BooleanOp::Subtract);
        assert_eq!(
            Modifiers {
                shift: true,
                alt: true,
                ctrl: false
            }
            .selection_op(),
            BooleanOp::Intersect
        );
    }

    #[test]
    fn zooming_about_a_point_keeps_that_point_under_the_cursor() {
        let mut v = ViewState {
            center: Vec2::new(100.0, 100.0),
            zoom: 1.0,
            rotation: 0.3,
            viewport: Vec2::new(800.0, 600.0),
        };
        let anchor = Vec2::new(160.0, 40.0);
        let before = v.screen_at(anchor);
        v.zoom_about(anchor, 2.0);
        let after = v.screen_at(anchor);
        assert!(
            (after - before).length() < 1e-3,
            "anchor drifted from {before:?} to {after:?}"
        );
        assert!((v.zoom - 2.0).abs() < 1e-6);
    }

    #[test]
    fn zoom_is_clamped_and_a_nonsense_factor_is_ignored() {
        let mut v = ViewState::default();
        v.zoom_about(Vec2::ZERO, 1e9);
        assert_eq!(v.zoom, ViewState::MAX_ZOOM);
        v.zoom_about(Vec2::ZERO, 1e-9);
        assert_eq!(v.zoom, ViewState::MIN_ZOOM);
        let before = v.zoom;
        v.zoom_about(Vec2::ZERO, f32::NAN);
        v.zoom_about(Vec2::ZERO, 0.0);
        assert_eq!(v.zoom, before);
    }

    #[test]
    fn set_zoom_clamps_so_the_inverse_view_matrix_stays_usable() {
        let mut v = ViewState::default();
        v.set_zoom(0.0);
        assert_eq!(v.zoom, ViewState::MIN_ZOOM, "set_zoom let a zero through");
        let p = v.document_at(Vec2::new(10.0, 10.0));
        assert!(p.is_finite(), "document_at went non-finite: {p:?}");

        v.set_zoom(1e9);
        assert_eq!(v.zoom, ViewState::MAX_ZOOM);
        v.set_zoom(-4.0);
        assert_eq!(v.zoom, ViewState::MIN_ZOOM);
        let before = v.zoom;
        v.set_zoom(f32::NAN);
        assert_eq!(v.zoom, before, "a NaN zoom must be ignored, not stored");

        // The field is public and assigning it directly bypasses the clamp —
        // which is what `to_document`'s doc says, so it is pinned here too.
        v.zoom = 0.0;
        assert!(!v.document_at(Vec2::new(10.0, 10.0)).is_finite());
    }

    #[test]
    fn a_pattern_tiles_in_both_directions_from_the_origin() {
        let p = Pattern::new(
            2,
            2,
            vec![1, 1, 1, 255, 2, 2, 2, 255, 3, 3, 3, 255, 4, 4, 4, 255],
        )
        .unwrap();
        assert_eq!(p.sample(0, 0), [1, 1, 1, 255]);
        assert_eq!(p.sample(1, 0), [2, 2, 2, 255]);
        assert_eq!(p.sample(2, 0), [1, 1, 1, 255]);
        assert_eq!(p.sample(-1, -1), [4, 4, 4, 255]);
        assert!(Pattern::new(0, 2, Vec::new()).is_err());
        assert!(Pattern::new(2, 2, vec![0; 3]).is_err());
    }

    #[test]
    fn a_context_with_no_layer_refuses_to_name_a_pixel_target() {
        let mut tiles = MemoryTiles::new();
        let ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
        assert!(matches!(ctx.pixel_target(), Err(ToolError::NoActiveLayer)));
        // ...and no selection means every pixel is paintable.
        assert_eq!(ctx.clip_at(glam::IVec2::new(1000, -1000)), 1.0);
    }
}
