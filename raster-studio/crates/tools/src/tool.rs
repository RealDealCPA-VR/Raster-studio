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
use layer_model::{LayerId, MaskId};
use raster::PixelRect;
use selection::{BooleanOp, Rect};

use crate::error::ToolError;
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
/// `editor-core` has no selection command yet — [`Selection`] is a plain field
/// on the document — so this rides its own outbox rather than being smuggled
/// through a fabricated [`Command`]. The application applies it with
/// [`SelectionEdit::apply`] and records whatever undo entry it uses for
/// selection state.
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

/// Something a tool wants that is not a [`Command`] and not a selection.
///
/// The two outboxes are separate because a selection edit is by far the most
/// common non-command result and deserves a typed accessor; everything else
/// shares this one.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolRequest {
    Crop(CropRequest),
    Slices(Vec<Slice>),
}

/// Everything a tool may read, plus the outboxes for everything it wants
/// changed.
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
    /// The active pattern, for the pattern-driven tools.
    pub pattern: Option<Pattern>,
    /// Where a "sample all layers" read comes from — typically the cached
    /// flattened composite. `None` means sample the active layer.
    pub sample_from: Option<PixelKey>,
    /// The canvas view, mutated in place by the navigation tools.
    pub view: ViewState,
    /// The layers under the pointer, topmost first — what auto-select walks.
    pub layer_stack: Vec<LayerId>,
    /// Pixel bytes.
    pub tiles: &'a mut dyn TileAccess,

    commands: Vec<Command>,
    selection_edits: Vec<SelectionEdit>,
    requests: Vec<ToolRequest>,
    picked: Option<[f32; 4]>,
}

impl<'a> ToolContext<'a> {
    /// A context over `tiles` with nothing selected and no active layer.
    pub fn new(tiles: &'a mut dyn TileAccess, canvas: PixelRect) -> Self {
        Self {
            active_layer: None,
            active_mask: None,
            paint_target: PaintTarget::Layer,
            canvas,
            selection: Selection::None,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 1.0],
            pattern: None,
            sample_from: None,
            view: ViewState::default(),
            layer_stack: Vec::new(),
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

    /// How much of one pixel the current selection lets an edit through.
    pub fn clip_at(&self, p: glam::IVec2) -> f32 {
        self.selection.coverage_at(p)
    }
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

    /// Whether a gesture is currently in progress.
    fn is_active(&self) -> bool;
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
