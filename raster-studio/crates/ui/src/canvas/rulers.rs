//! Rulers, measurement units, and guides.
//!
//! Rulers are generated, not drawn ad hoc: [`ruler_ticks`] returns the ticks and
//! their labels for one screen edge, and the painter only strokes what it is
//! given. That keeps the arithmetic — which unit, which step, how many
//! subdivisions still read at this zoom — testable without a window.
//!
//! # Rulers under a rotated view
//!
//! A ruler runs along a screen edge and reads a document coordinate. That only
//! makes sense while a document axis is parallel to that edge, i.e. at
//! multiples of a quarter turn (flips included, which reverse the direction but
//! keep the axis). At any other angle a single number cannot describe a
//! position along the edge, so [`ruler_mapping`] reports
//! [`RulerMapping::Oblique`] and [`ruler_ticks`] returns nothing. The gutter is
//! then filled with the disabled token instead of the ordinary one — see
//! [`crate::canvas::paint::gutter_fills`] — and hovering it explains why, with
//! the sentence [`oblique_hint`] returns. Nothing is invented and nothing is
//! silently missing.
//!
//! # Guides
//!
//! Guides are dragged out of the gutters and along the canvas. The gesture is a
//! three-step state machine — [`GuideGesture::begin`], [`GuideGesture::drag`],
//! [`GuideGesture::finish`] — with no `egui` in any of its signatures, so every
//! rule it enforces (a locked guide refuses, a guide dropped back in the gutter
//! is deleted, a click in the ruler that goes nowhere creates nothing) is
//! testable without a window.

use glam::Vec2;

use super::camera::CanvasCamera;
use super::geom::Axis;
use super::viewport::Viewport;

/// A measurement unit the rulers and readouts can display.
///
/// The application's measurement unit is [`crate::dialogs::units::Unit`] — the
/// one the size dialogs offer. This is the same vocabulary in the canvas's own
/// arithmetic type (`f32`, because every coordinate that reaches a ruler tick
/// has already been through the camera in `f32`), and the two convert into each
/// other losslessly through the [`From`] impls below. There is one list of
/// units in the product; `units_are_one_vocabulary_in_two_arithmetics` pins the
/// two enums to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Unit {
    #[default]
    Pixels,
    Percent,
    Inches,
    Centimeters,
    Millimeters,
    Points,
    Picas,
}

impl Unit {
    /// Every unit, in the order the menu lists them.
    pub const ALL: &'static [Unit] = &[
        Unit::Pixels,
        Unit::Percent,
        Unit::Inches,
        Unit::Centimeters,
        Unit::Millimeters,
        Unit::Points,
        Unit::Picas,
    ];

    /// Full name, for the unit menu.
    pub const fn name(self) -> &'static str {
        match self {
            Unit::Pixels => "Pixels",
            Unit::Percent => "Percent",
            Unit::Inches => "Inches",
            Unit::Centimeters => "Centimeters",
            Unit::Millimeters => "Millimeters",
            Unit::Points => "Points",
            Unit::Picas => "Picas",
        }
    }

    /// Suffix for a readout.
    pub const fn abbreviation(self) -> &'static str {
        match self {
            Unit::Pixels => "px",
            Unit::Percent => "%",
            Unit::Inches => "in",
            Unit::Centimeters => "cm",
            Unit::Millimeters => "mm",
            Unit::Points => "pt",
            Unit::Picas => "pc",
        }
    }

    /// How many document pixels one unit spans.
    ///
    /// `dpi` is the document's resolution; `axis_extent` is the document's size
    /// along the axis being measured, which only [`Unit::Percent`] needs. Both
    /// are clamped to something positive, because a document with a zero
    /// resolution or a zero width must still produce a usable ruler rather than
    /// an infinity.
    pub fn doc_pixels_per_unit(self, dpi: f32, axis_extent: f32) -> f32 {
        let dpi = if dpi.is_finite() && dpi > 0.0 {
            dpi
        } else {
            72.0
        };
        let extent = if axis_extent.is_finite() && axis_extent > 0.0 {
            axis_extent
        } else {
            1.0
        };
        match self {
            Unit::Pixels => 1.0,
            Unit::Percent => extent / 100.0,
            Unit::Inches => dpi,
            Unit::Centimeters => dpi / 2.54,
            Unit::Millimeters => dpi / 25.4,
            Unit::Points => dpi / 72.0,
            Unit::Picas => dpi / 6.0,
        }
    }

    /// Document pixels to this unit.
    pub fn from_doc_pixels(self, px: f32, dpi: f32, axis_extent: f32) -> f32 {
        px / self.doc_pixels_per_unit(dpi, axis_extent)
    }

    /// This unit to document pixels.
    pub fn to_doc_pixels(self, value: f32, dpi: f32, axis_extent: f32) -> f32 {
        value * self.doc_pixels_per_unit(dpi, axis_extent)
    }

    /// How many decimals a readout in this unit should show. Pixels are whole;
    /// a millimetre needs one place to be useful, an inch two.
    pub const fn decimals(self) -> usize {
        match self {
            Unit::Pixels => 0,
            Unit::Percent | Unit::Millimeters | Unit::Points | Unit::Picas => 1,
            Unit::Inches | Unit::Centimeters => 2,
        }
    }
}

impl From<crate::dialogs::units::Unit> for Unit {
    fn from(unit: crate::dialogs::units::Unit) -> Self {
        use crate::dialogs::units::Unit as Dialog;
        match unit {
            Dialog::Pixels => Unit::Pixels,
            Dialog::Percent => Unit::Percent,
            Dialog::Inches => Unit::Inches,
            Dialog::Centimeters => Unit::Centimeters,
            Dialog::Millimeters => Unit::Millimeters,
            Dialog::Points => Unit::Points,
            Dialog::Picas => Unit::Picas,
        }
    }
}

impl From<Unit> for crate::dialogs::units::Unit {
    fn from(unit: Unit) -> Self {
        use crate::dialogs::units::Unit as Dialog;
        match unit {
            Unit::Pixels => Dialog::Pixels,
            Unit::Percent => Dialog::Percent,
            Unit::Inches => Dialog::Inches,
            Unit::Centimeters => Dialog::Centimeters,
            Unit::Millimeters => Dialog::Millimeters,
            Unit::Points => Dialog::Points,
            Unit::Picas => Dialog::Picas,
        }
    }
}

/// What a ruler along one screen edge is able to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulerMapping {
    /// The edge reads a document coordinate on this axis. `reversed` is true
    /// when moving right (or down) on screen *decreases* it, which is what a
    /// flip or a 180-degree rotation does.
    Reads { axis: Axis, reversed: bool },
    /// The view is rotated off-axis; no single coordinate describes the edge.
    Oblique,
}

/// The largest deviation from a right angle still treated as axis-aligned.
/// A tenth of a degree: far below anything a user can produce by dragging, and
/// far above the float error in a quarter-turn matrix.
const AXIS_EPS: f32 = 0.0017;

/// What a ruler running along `screen_axis` can read, given the camera.
pub fn ruler_mapping(
    camera: &CanvasCamera,
    viewport: &Viewport,
    screen_axis: Axis,
) -> RulerMapping {
    let Some(to_doc) = camera.pt_to_doc(viewport) else {
        return RulerMapping::Oblique;
    };
    let along = to_doc.transform_vector2(match screen_axis {
        Axis::X => Vec2::X,
        Axis::Y => Vec2::Y,
    });
    let len = along.length();
    if len <= 0.0 || !along.is_finite() {
        return RulerMapping::Oblique;
    }
    let n = along / len;
    if n.y.abs() <= AXIS_EPS {
        RulerMapping::Reads {
            axis: Axis::X,
            reversed: n.x < 0.0,
        }
    } else if n.x.abs() <= AXIS_EPS {
        RulerMapping::Reads {
            axis: Axis::Y,
            reversed: n.y < 0.0,
        }
    } else {
        RulerMapping::Oblique
    }
}

/// How prominent a tick is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickKind {
    /// Labelled, full height.
    Major,
    /// Half height, no label.
    Minor,
}

/// One tick on a ruler.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    /// Position along the ruler, in screen points.
    pub screen_pt: f32,
    /// The document coordinate it marks.
    pub doc: f32,
    /// The value in the ruler's unit — what the label says.
    pub value: f32,
    pub kind: TickKind,
    /// `Some` only on [`TickKind::Major`].
    pub label: Option<String>,
}

/// Everything the tick generator needs that is not the camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RulerSpec {
    pub unit: Unit,
    /// Document resolution, pixels per inch.
    pub dpi: f32,
    /// The document's size along the axis the ruler ends up reading; only
    /// [`Unit::Percent`] uses it.
    pub doc_extent: Vec2,
    /// Smallest gap between two labelled ticks, in screen points.
    pub min_label_gap_pt: f32,
    /// Smallest gap between two minor ticks, in screen points.
    pub min_minor_gap_pt: f32,
}

impl Default for RulerSpec {
    fn default() -> Self {
        Self {
            unit: Unit::Pixels,
            dpi: 72.0,
            doc_extent: Vec2::new(1.0, 1.0),
            // Sixteen grid units between labels, and a unit and a half between
            // minor ticks: the two densities the 1-2-5 ladder is chosen against.
            min_label_gap_pt: design::tokens::grid(16.0),
            min_minor_gap_pt: design::tokens::grid(1.5),
        }
    }
}

/// A hard ceiling on how many ticks one ruler may produce, so a pathological
/// zoom cannot turn a frame into a million line segments.
const MAX_TICKS: usize = 4096;

/// The 1-2-5 step at or above `minimum`.
///
/// Returns `1.0` for a non-positive or non-finite input rather than looping —
/// a caller that has divided by a zoom is the likely source of one.
pub fn nice_step(minimum: f32) -> f32 {
    if !minimum.is_finite() || minimum <= 0.0 {
        return 1.0;
    }
    let decade = 10f32.powf(minimum.log10().floor());
    for m in [1.0, 2.0, 5.0] {
        let candidate = m * decade;
        if candidate >= minimum * (1.0 - 1e-4) {
            return candidate;
        }
    }
    10.0 * decade
}

/// How many parts a major division is split into, given how much room a single
/// part would get.
fn subdivisions(major_gap_pt: f32, min_minor_gap_pt: f32) -> u32 {
    for n in [10u32, 5, 4, 2] {
        if major_gap_pt / n as f32 >= min_minor_gap_pt {
            return n;
        }
    }
    1
}

/// Format a ruler label without trailing noise: whole numbers print whole.
fn format_value(value: f32, unit: Unit) -> String {
    let rounded = if value.abs() < 1e-6 { 0.0 } else { value };
    if (rounded - rounded.round()).abs() < 1e-4 {
        format!("{:.0}", rounded.round())
    } else {
        format!("{:.*}", unit.decimals(), rounded)
    }
}

/// The ticks for the ruler along `screen_axis`.
///
/// Empty when the view is oblique (see the module docs), when the viewport has
/// collapsed, or when the requested density would exceed [`MAX_TICKS`].
pub fn ruler_ticks(
    camera: &CanvasCamera,
    viewport: &Viewport,
    screen_axis: Axis,
    spec: &RulerSpec,
) -> Vec<Tick> {
    let RulerMapping::Reads { axis: doc_axis, .. } = ruler_mapping(camera, viewport, screen_axis)
    else {
        return Vec::new();
    };
    if viewport.is_degenerate() {
        return Vec::new();
    }
    let bounds = viewport.content_bounds_pt();
    let (start_pt, end_pt) = match screen_axis {
        Axis::X => (bounds.min.x, bounds.max.x),
        Axis::Y => (bounds.min.y, bounds.max.y),
    };
    let probe = |along: f32| -> Vec2 {
        let p = match screen_axis {
            Axis::X => Vec2::new(along, bounds.min.y),
            Axis::Y => Vec2::new(bounds.min.x, along),
        };
        camera.doc_of_screen_pt(viewport, p)
    };
    let doc_start = doc_axis.of(probe(start_pt));
    let doc_end = doc_axis.of(probe(end_pt));
    if !doc_start.is_finite() || !doc_end.is_finite() || doc_start == doc_end {
        return Vec::new();
    }

    let per_unit = spec
        .unit
        .doc_pixels_per_unit(spec.dpi, doc_axis.of(spec.doc_extent));
    // Screen points covered by one unit, always positive.
    let pt_per_unit = ((end_pt - start_pt) / (doc_end - doc_start) * per_unit).abs();
    if !pt_per_unit.is_finite() || pt_per_unit <= 0.0 {
        return Vec::new();
    }

    let major_units = nice_step(spec.min_label_gap_pt.max(1.0) / pt_per_unit);
    let major_gap_pt = major_units * pt_per_unit;
    let parts = subdivisions(major_gap_pt, spec.min_minor_gap_pt.max(0.5));
    let minor_units = major_units / parts as f32;

    let (lo_doc, hi_doc) = if doc_start < doc_end {
        (doc_start, doc_end)
    } else {
        (doc_end, doc_start)
    };
    let lo_units = lo_doc / per_unit;
    let hi_units = hi_doc / per_unit;
    let first = (lo_units / minor_units).floor() * minor_units;
    let count = ((hi_units - first) / minor_units).ceil() as i64 + 1;
    if count <= 0 || count as usize > MAX_TICKS {
        return Vec::new();
    }

    // Screen position of a document coordinate along the ruler.
    let screen_of = |doc: f32| -> f32 {
        start_pt + (doc - doc_start) / (doc_end - doc_start) * (end_pt - start_pt)
    };

    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let value = first + i as f32 * minor_units;
        let doc = value * per_unit;
        // Whether this minor tick coincides with a major one.
        let k = value / major_units;
        let is_major = (k - k.round()).abs() < 1e-3;
        let screen_pt = screen_of(doc);
        if !screen_pt.is_finite() {
            continue;
        }
        out.push(Tick {
            screen_pt,
            doc,
            value,
            kind: if is_major {
                TickKind::Major
            } else {
                TickKind::Minor
            },
            label: is_major.then(|| format_value(value, spec.unit)),
        });
    }
    // Ascending along the ruler, so a caller may reason about gaps between
    // neighbours even when a flip or a half turn reversed the coordinate.
    out.sort_by(|a, b| {
        a.screen_pt
            .partial_cmp(&b.screen_pt)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// A single guide line.
///
/// `axis` names the *coordinate* the guide holds constant, so an `Axis::X`
/// guide is a vertical line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guide {
    pub axis: Axis,
    /// The document coordinate the guide sits at.
    pub doc: f32,
    /// A locked guide is drawn but cannot be dragged or deleted by pointer.
    pub locked: bool,
}

impl Guide {
    pub fn new(axis: Axis, doc: f32) -> Self {
        Self {
            axis,
            doc,
            locked: false,
        }
    }

    pub fn locked(mut self) -> Self {
        self.locked = true;
        self
    }

    /// Where the guide crosses the screen, in points along the perpendicular
    /// screen axis — `None` when the view is oblique, where a guide is still
    /// drawn as a full line but has no single screen coordinate.
    pub fn screen_pt(&self, camera: &CanvasCamera, viewport: &Viewport) -> Option<f32> {
        let screen_axis = match ruler_mapping(camera, viewport, Axis::X) {
            RulerMapping::Reads { axis, .. } if axis == self.axis => Axis::X,
            RulerMapping::Reads { .. } => Axis::Y,
            RulerMapping::Oblique => return None,
        };
        let p = camera.screen_pt_of(viewport, self.axis.compose(self.doc, 0.0));
        Some(screen_axis.of(p))
    }
}

/// The document's guides, plus whether they are shown and locked as a set.
///
/// [`Default`] is [`Guides::new`] and not the derived one. The derive would
/// produce `visible: false`, and a hidden guide is neither drawn, nor
/// hit-testable, nor a snap candidate — so a `#[derive(Default)]` on any struct
/// that happens to contain a `Guides` would silently yield guides that exist
/// and can never be seen or grabbed.
#[derive(Debug, Clone, PartialEq)]
pub struct Guides {
    guides: Vec<Guide>,
    /// Hidden guides still snap nothing and are not hit-testable.
    pub visible: bool,
    /// Locks every guide at once, on top of each guide's own flag.
    pub locked: bool,
}

impl Default for Guides {
    fn default() -> Self {
        Self::new()
    }
}

impl Guides {
    /// A visible, unlocked, empty set.
    pub fn new() -> Self {
        Self {
            guides: Vec::new(),
            visible: true,
            locked: false,
        }
    }

    /// The most guides a document may hold. Dragging one out of a ruler is a
    /// single gesture, so this only bites on a pathological script.
    pub const MAX: usize = 512;

    pub fn len(&self) -> usize {
        self.guides.len()
    }

    pub fn is_empty(&self) -> bool {
        self.guides.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Guide> {
        self.guides.iter()
    }

    pub fn get(&self, index: usize) -> Option<&Guide> {
        self.guides.get(index)
    }

    /// Add a guide, returning its index. A non-finite position is refused, and
    /// so is one past [`Guides::MAX`].
    pub fn add(&mut self, guide: Guide) -> Option<usize> {
        if !guide.doc.is_finite() || self.guides.len() >= Self::MAX {
            return None;
        }
        self.guides.push(guide);
        Some(self.guides.len() - 1)
    }

    /// Move a guide. Refused for a locked guide or a non-finite position.
    pub fn move_to(&mut self, index: usize, doc: f32) -> bool {
        if !doc.is_finite() || self.locked {
            return false;
        }
        match self.guides.get_mut(index) {
            Some(g) if !g.locked => {
                g.doc = doc;
                true
            }
            _ => false,
        }
    }

    /// Remove a guide. Refused for a locked guide.
    pub fn remove(&mut self, index: usize) -> Option<Guide> {
        if self.locked || index >= self.guides.len() || self.guides[index].locked {
            return None;
        }
        Some(self.guides.remove(index))
    }

    pub fn clear(&mut self) {
        if !self.locked {
            self.guides.retain(|g| g.locked);
        }
    }

    /// Rebuild from the document's persisted guide model, replacing every
    /// guide. This is the draw-time half of the guide seam: `observe` seeds the
    /// view from [`editor_core::Document::guides`] each frame, and
    /// [`Guides::to_document`] hands the result back so the shell can persist
    /// and undo an edit. The axis mapping: the model's `Horizontal`/`Vertical`
    /// free the display layer from the camera-aware `Axis`, so X-constant
    /// (vertical) maps one way and Y-constant (horizontal) the other.
    pub fn from_document(src: &editor_core::Guides) -> Self {
        Self {
            guides: src
                .list
                .iter()
                .map(|g| Guide {
                    axis: match g.axis {
                        editor_core::GuideAxis::Vertical => Axis::X,
                        editor_core::GuideAxis::Horizontal => Axis::Y,
                    },
                    doc: g.doc,
                    locked: g.locked,
                })
                .collect(),
            visible: src.visible,
            locked: src.locked,
        }
    }

    /// The persisted model this draw-time set stands for.
    pub fn to_document(&self) -> editor_core::Guides {
        editor_core::Guides {
            list: self
                .guides
                .iter()
                .map(|g| editor_core::Guide {
                    axis: match g.axis {
                        Axis::X => editor_core::GuideAxis::Vertical,
                        Axis::Y => editor_core::GuideAxis::Horizontal,
                    },
                    doc: g.doc,
                    locked: g.locked,
                })
                .collect(),
            visible: self.visible,
            locked: self.locked,
        }
    }

    /// The guide nearest a screen position, within `tolerance_pt`.
    ///
    /// Hidden guides are never hit; locked ones are, so the UI can explain why
    /// they will not move instead of silently doing nothing.
    pub fn hit_test(
        &self,
        camera: &CanvasCamera,
        viewport: &Viewport,
        pos_pt: Vec2,
        tolerance_pt: f32,
    ) -> Option<usize> {
        if !self.visible {
            return None;
        }
        let mut best: Option<(f32, usize)> = None;
        for (i, g) in self.guides.iter().enumerate() {
            let Some(at) = g.screen_pt(camera, viewport) else {
                continue;
            };
            // A guide of axis X is a vertical line, so it is grabbed by the
            // pointer's screen x when the view is upright — which is exactly
            // the axis `screen_pt` measured along.
            let along = match ruler_mapping(camera, viewport, Axis::X) {
                RulerMapping::Reads { axis, .. } if axis == g.axis => pos_pt.x,
                RulerMapping::Reads { .. } => pos_pt.y,
                RulerMapping::Oblique => continue,
            };
            let d = (along - at).abs();
            if d <= tolerance_pt && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, i));
            }
        }
        best.map(|(_, i)| i)
    }
}

/// What the UI says over a ruler gutter that cannot read anything.
pub const OBLIQUE_HINT: &str =
    "Rulers are hidden while the view is rotated off-axis. Reset the view rotation to read them.";

/// The sentence to show over the ruler gutters, or `None` while they work.
///
/// Both edges go oblique together — a rotation that takes one off-axis takes
/// the other with it — so one hint covers both gutters.
pub fn oblique_hint(camera: &CanvasCamera, viewport: &Viewport) -> Option<&'static str> {
    Axis::ALL
        .iter()
        .any(|a| ruler_mapping(camera, viewport, *a) == RulerMapping::Oblique)
        .then_some(OBLIQUE_HINT)
}

/// The two ruler gutters of `outer`, top first, each `thickness_pt` deep.
///
/// The corner square where they meet belongs to the top gutter, so a press
/// there is unambiguous.
pub fn gutters(outer: egui::Rect, thickness_pt: f32) -> [egui::Rect; 2] {
    let t = if thickness_pt.is_finite() {
        thickness_pt.max(0.0)
    } else {
        0.0
    };
    let top = egui::Rect::from_min_max(outer.min, egui::pos2(outer.max.x, outer.min.y + t));
    let left = egui::Rect::from_min_max(
        egui::pos2(outer.min.x, outer.min.y + t),
        egui::pos2(outer.min.x + t, outer.max.y),
    );
    [top, left]
}

/// Which gutter a screen point is in, as the axis of the guide a drag out of it
/// creates.
///
/// The top ruler produces horizontal lines, and a horizontal line is the set of
/// points with one *y* — so it yields [`Axis::Y`]. The left ruler is the mirror
/// of that.
pub fn gutter_at(outer: egui::Rect, thickness_pt: f32, pos_pt: Vec2) -> Option<Axis> {
    let p = super::geom::to_pos2(pos_pt);
    if !pos_pt.is_finite() {
        return None;
    }
    let [top, left] = gutters(outer, thickness_pt);
    if top.contains(p) {
        Some(Axis::Y)
    } else if left.contains(p) {
        Some(Axis::X)
    } else {
        None
    }
}

/// A guide being dragged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GuideDrag {
    /// Pulled out of a ruler; it does not exist yet.
    New { axis: Axis },
    /// An existing guide, by index.
    Existing { index: usize },
}

/// What a press means to the guides.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GuideGrab {
    /// Not the guides' business; the press goes on to the tool.
    None,
    /// Begin this drag. The caller claims the gesture.
    Start(GuideDrag),
    /// The pointer is on a guide that is locked. Nothing moves, and the cursor
    /// says so rather than the press silently doing nothing.
    Refused,
}

/// How a guide drag ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideDrop {
    /// The guide stayed where it was let go.
    Kept,
    /// Dropped back into a gutter or off the canvas: removed.
    Deleted,
    /// A press in the ruler that never left it: nothing was ever created.
    NeverCreated,
}

/// Everything a guide gesture needs that is not the pointer position.
///
/// `outer` is the *gutter-inclusive* rectangle the canvas occupies. It has to
/// be, because [`crate::canvas::Viewport`] has the gutters taken off it —
/// which is exactly why a press in a ruler used to be rejected as being over a
/// panel and no guide could ever be created.
/// The camera and viewport are held **by value**, not borrowed: the caller has
/// to hand the guides out mutably at the same time, and a `&self` borrow of the
/// canvas would make that impossible. Both are `Copy`, so this costs nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuideGesture {
    pub camera: CanvasCamera,
    pub viewport: Viewport,
    pub outer: egui::Rect,
    pub ruler_thickness_pt: f32,
    /// Guides can only be pulled out of a ruler that is on screen.
    pub rulers_visible: bool,
    /// How close, in screen points, the pointer has to be to grab a guide.
    pub grab_pt: f32,
}

impl GuideGesture {
    /// The gutter thickness to hit-test against, or zero when the rulers are
    /// hidden and there is nothing to pull a guide out of.
    fn thickness(&self) -> f32 {
        if self.rulers_visible {
            self.ruler_thickness_pt
        } else {
            0.0
        }
    }

    /// Which gutter `pos_pt` is in, honouring the rulers-hidden case.
    pub fn gutter_at(&self, pos_pt: Vec2) -> Option<Axis> {
        gutter_at(self.outer, self.thickness(), pos_pt)
    }

    /// The document coordinate a guide of `axis` dropped at `pos_pt` holds.
    fn doc_of(&self, axis: Axis, pos_pt: Vec2) -> Option<f32> {
        let doc = axis.of(self.camera.doc_of_screen_pt(&self.viewport, pos_pt));
        doc.is_finite().then_some(doc)
    }

    /// What a press at `pos_pt` starts.
    pub fn begin(&self, guides: &Guides, pos_pt: Vec2) -> GuideGrab {
        if !pos_pt.is_finite()
            || self.viewport.is_degenerate()
            || !self.outer.contains(super::geom::to_pos2(pos_pt))
        {
            return GuideGrab::None;
        }
        if let Some(axis) = self.gutter_at(pos_pt) {
            return GuideGrab::Start(GuideDrag::New { axis });
        }
        let Some(index) = guides.hit_test(&self.camera, &self.viewport, pos_pt, self.grab_pt)
        else {
            return GuideGrab::None;
        };
        match guides.get(index) {
            Some(g) if !g.locked && !guides.locked => {
                GuideGrab::Start(GuideDrag::Existing { index })
            }
            Some(_) => GuideGrab::Refused,
            None => GuideGrab::None,
        }
    }

    /// Drive a running drag to `pos_pt`.
    ///
    /// A [`GuideDrag::New`] becomes an [`GuideDrag::Existing`] the moment the
    /// pointer leaves the gutter and the guide is actually created; until then
    /// nothing is added, so a click in the ruler that goes nowhere leaves the
    /// document alone. `None` means the drag is over — the guide it named is
    /// gone.
    pub fn drag(&self, drag: GuideDrag, guides: &mut Guides, pos_pt: Vec2) -> Option<GuideDrag> {
        if !pos_pt.is_finite() {
            return Some(drag);
        }
        match drag {
            GuideDrag::New { axis } => {
                if self.gutter_at(pos_pt).is_some() || !self.viewport.contains_pt(pos_pt) {
                    return Some(drag);
                }
                let doc = self.doc_of(axis, pos_pt)?;
                match guides.add(Guide::new(axis, doc)) {
                    // Full, or refused: stay a New so a later move can retry.
                    None => Some(drag),
                    Some(index) => Some(GuideDrag::Existing { index }),
                }
            }
            GuideDrag::Existing { index } => {
                let axis = guides.get(index)?.axis;
                let doc = self.doc_of(axis, pos_pt)?;
                guides.move_to(index, doc);
                Some(GuideDrag::Existing { index })
            }
        }
    }

    /// End a drag at `pos_pt`.
    ///
    /// Dropping a guide back into a gutter — or anywhere off the image area —
    /// removes it, which is how every editor deletes one.
    pub fn finish(&self, drag: GuideDrag, guides: &mut Guides, pos_pt: Vec2) -> GuideDrop {
        match drag {
            GuideDrag::New { .. } => GuideDrop::NeverCreated,
            GuideDrag::Existing { index } => {
                let dropped_away = !pos_pt.is_finite() || !self.viewport.contains_pt(pos_pt);
                if dropped_away && guides.remove(index).is_some() {
                    GuideDrop::Deleted
                } else {
                    GuideDrop::Kept
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_guide_set_round_trips_through_the_view() {
        // `from_document` (seed for drawing) and `to_document` (persistence) are
        // exact inverses, so a guide dragged on the canvas is captured unchanged.
        let doc = editor_core::Guides {
            visible: true,
            locked: false,
            list: vec![
                editor_core::Guide {
                    axis: editor_core::GuideAxis::Vertical,
                    doc: 24.0,
                    locked: false,
                },
                editor_core::Guide {
                    axis: editor_core::GuideAxis::Horizontal,
                    doc: 120.5,
                    locked: true,
                },
            ],
        };
        let view = Guides::from_document(&doc);
        // Axis mapping: X-constant (vertical) stays a vertical guide.
        assert_eq!(view.iter().next().unwrap().axis, Axis::X);
        assert_eq!(view.iter().nth(1).unwrap().axis, Axis::Y);
        assert_eq!(view.to_document(), doc);
    }
    use crate::canvas::viewport::PanelInsets;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    fn vp() -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            2.0,
        )
    }

    #[test]
    fn unit_conversions_round_trip_and_match_the_definitions() {
        let dpi = 300.0;
        for u in Unit::ALL {
            let px = u.to_doc_pixels(3.0, dpi, 1200.0);
            assert!(
                (u.from_doc_pixels(px, dpi, 1200.0) - 3.0).abs() < 1e-3,
                "{u:?}"
            );
        }
        assert_eq!(Unit::Pixels.doc_pixels_per_unit(dpi, 1200.0), 1.0);
        assert_eq!(Unit::Inches.doc_pixels_per_unit(dpi, 1200.0), 300.0);
        assert!((Unit::Centimeters.doc_pixels_per_unit(dpi, 1200.0) - 118.11).abs() < 0.01);
        assert!((Unit::Millimeters.doc_pixels_per_unit(dpi, 1200.0) - 11.811).abs() < 0.01);
        assert!((Unit::Points.doc_pixels_per_unit(dpi, 1200.0) - 4.1667).abs() < 0.001);
        assert!((Unit::Picas.doc_pixels_per_unit(dpi, 1200.0) - 50.0).abs() < 1e-4);
        // Percent is the only unit that depends on the document's own size.
        assert_eq!(Unit::Percent.doc_pixels_per_unit(dpi, 1200.0), 12.0);
        assert_eq!(Unit::Percent.doc_pixels_per_unit(dpi, 400.0), 4.0);
    }

    #[test]
    fn a_nonsense_resolution_falls_back_instead_of_producing_infinity() {
        for bad in [0.0, -300.0, f32::NAN, f32::INFINITY] {
            let v = Unit::Inches.doc_pixels_per_unit(bad, 100.0);
            assert!(v.is_finite() && v > 0.0, "{bad} gave {v}");
        }
        assert!(Unit::Percent.doc_pixels_per_unit(72.0, 0.0).is_finite());
    }

    #[test]
    fn the_one_two_five_ladder_never_goes_below_what_was_asked() {
        for (asked, want) in [
            (0.3_f32, 0.5_f32),
            (1.0, 1.0),
            (1.1, 2.0),
            (2.0, 2.0),
            (2.5, 5.0),
            (6.0, 10.0),
            (11.0, 20.0),
            (300.0, 500.0),
            (0.011, 0.02),
        ] {
            let got = nice_step(asked);
            assert!(
                (got - want).abs() < want * 1e-4,
                "nice_step({asked}) = {got}, wanted {want}"
            );
            assert!(got >= asked * (1.0 - 1e-3));
        }
        assert_eq!(nice_step(0.0), 1.0);
        assert_eq!(nice_step(f32::NAN), 1.0);
    }

    #[test]
    fn an_upright_view_reads_the_matching_axis_on_each_edge() {
        let cam = CanvasCamera::default();
        assert_eq!(
            ruler_mapping(&cam, &vp(), Axis::X),
            RulerMapping::Reads {
                axis: Axis::X,
                reversed: false
            }
        );
        assert_eq!(
            ruler_mapping(&cam, &vp(), Axis::Y),
            RulerMapping::Reads {
                axis: Axis::Y,
                reversed: false
            }
        );
    }

    #[test]
    fn a_quarter_turn_swaps_which_axis_each_ruler_reads() {
        let cam = CanvasCamera {
            rotation: FRAC_PI_2,
            ..CanvasCamera::default()
        };
        assert_eq!(
            ruler_mapping(&cam, &vp(), Axis::X),
            RulerMapping::Reads {
                axis: Axis::Y,
                reversed: true
            }
        );
        assert_eq!(
            ruler_mapping(&cam, &vp(), Axis::Y),
            RulerMapping::Reads {
                axis: Axis::X,
                reversed: false
            }
        );
    }

    #[test]
    fn a_flip_reverses_the_ruler_without_changing_its_axis() {
        let cam = CanvasCamera {
            flip_x: true,
            ..CanvasCamera::default()
        };
        assert_eq!(
            ruler_mapping(&cam, &vp(), Axis::X),
            RulerMapping::Reads {
                axis: Axis::X,
                reversed: true
            }
        );
        assert_eq!(
            ruler_mapping(&cam, &vp(), Axis::Y),
            RulerMapping::Reads {
                axis: Axis::Y,
                reversed: false
            }
        );
    }

    #[test]
    fn an_oblique_view_has_no_ruler_rather_than_a_wrong_one() {
        let cam = CanvasCamera {
            rotation: FRAC_PI_4,
            ..CanvasCamera::default()
        };
        assert_eq!(ruler_mapping(&cam, &vp(), Axis::X), RulerMapping::Oblique);
        assert!(ruler_ticks(&cam, &vp(), Axis::X, &RulerSpec::default()).is_empty());
    }

    #[test]
    fn ticks_land_where_their_document_coordinate_lands_on_screen() {
        let v = vp();
        let cam = CanvasCamera {
            center: Vec2::new(300.0, 200.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        };
        let ticks = ruler_ticks(&cam, &v, Axis::X, &RulerSpec::default());
        assert!(!ticks.is_empty());
        for t in &ticks {
            let want = cam.screen_pt_of(&v, Vec2::new(t.doc, 0.0)).x;
            assert!(
                (t.screen_pt - want).abs() < 0.01,
                "tick at doc {} drawn at {} but the camera puts it at {want}",
                t.doc,
                t.screen_pt
            );
        }
    }

    #[test]
    fn ticks_cover_the_whole_ruler_and_stay_labelled_far_enough_apart() {
        let v = vp();
        for zoom in [0.05_f32, 0.25, 1.0, 4.0, 32.0] {
            let cam = CanvasCamera {
                center: Vec2::new(500.0, 500.0),
                zoom,
                ..CanvasCamera::default()
            };
            let spec = RulerSpec::default();
            let ticks = ruler_ticks(&cam, &v, Axis::X, &spec);
            assert!(!ticks.is_empty(), "no ticks at zoom {zoom}");
            let bounds = v.content_bounds_pt();
            assert!(
                ticks.first().unwrap().screen_pt <= bounds.min.x + 1e-3,
                "the ruler does not start before its left edge at zoom {zoom}"
            );
            assert!(
                ticks.last().unwrap().screen_pt >= bounds.max.x - 1e-3,
                "the ruler does not reach its right edge at zoom {zoom}"
            );
            let labelled: Vec<f32> = ticks
                .iter()
                .filter(|t| t.kind == TickKind::Major)
                .map(|t| t.screen_pt)
                .collect();
            assert!(labelled.len() >= 2, "too few labels at zoom {zoom}");
            for pair in labelled.windows(2) {
                assert!(
                    (pair[1] - pair[0]).abs() >= spec.min_label_gap_pt * 0.99,
                    "labels {pair:?} crowd at zoom {zoom}"
                );
            }
            for t in &ticks {
                assert_eq!(t.label.is_some(), t.kind == TickKind::Major);
            }
        }
    }

    #[test]
    fn minor_ticks_never_crowd_below_their_minimum_gap() {
        let v = vp();
        let spec = RulerSpec {
            min_minor_gap_pt: 8.0,
            ..RulerSpec::default()
        };
        for zoom in [0.1_f32, 1.0, 7.0] {
            let cam = CanvasCamera {
                zoom,
                ..CanvasCamera::default()
            };
            let ticks = ruler_ticks(&cam, &v, Axis::X, &spec);
            for pair in ticks.windows(2) {
                assert!(
                    (pair[1].screen_pt - pair[0].screen_pt).abs() >= spec.min_minor_gap_pt * 0.99,
                    "minor ticks crowd at zoom {zoom}"
                );
            }
        }
    }

    #[test]
    fn the_label_reads_in_the_chosen_unit_not_in_pixels() {
        let v = vp();
        let cam = CanvasCamera {
            center: Vec2::new(300.0, 200.0),
            zoom: 1.0,
            ..CanvasCamera::default()
        };
        let spec = RulerSpec {
            unit: Unit::Inches,
            dpi: 300.0,
            ..RulerSpec::default()
        };
        let ticks = ruler_ticks(&cam, &v, Axis::X, &spec);
        let major_step = |ts: &[Tick]| -> f32 {
            let majors: Vec<f32> = ts
                .iter()
                .filter(|t| t.kind == TickKind::Major)
                .map(|t| t.doc)
                .collect();
            assert!(majors.len() >= 2);
            majors[1] - majors[0]
        };
        let major = ticks.iter().find(|t| t.kind == TickKind::Major).unwrap();
        // The value is the document coordinate expressed in inches at 300 dpi.
        assert!((major.doc / 300.0 - major.value).abs() < 1e-3);
        let expected = format_value(major.value, Unit::Inches);
        assert_eq!(major.label.as_deref(), Some(expected.as_str()));
        // Half an inch at 300dpi is 150 document pixels; the pixel ruler picks
        // its own 1-2-5 step and lands somewhere else entirely.
        assert!(
            (major_step(&ticks) - 150.0).abs() < 1e-2,
            "{}",
            major_step(&ticks)
        );
        let px_ticks = ruler_ticks(&cam, &v, Axis::X, &RulerSpec::default());
        assert!(
            (major_step(&px_ticks) - 200.0).abs() < 1e-2,
            "{}",
            major_step(&px_ticks)
        );
    }

    #[test]
    fn a_collapsed_viewport_or_a_dead_camera_produces_no_ticks() {
        let collapsed = Viewport::new(Vec2::splat(50.0), PanelInsets::uniform(50.0), 1.0);
        let cam = CanvasCamera::default();
        assert!(ruler_ticks(&cam, &collapsed, Axis::X, &RulerSpec::default()).is_empty());
        let dead = CanvasCamera {
            zoom: 0.0,
            ..CanvasCamera::default()
        };
        assert!(ruler_ticks(&dead, &vp(), Axis::X, &RulerSpec::default()).is_empty());
    }

    #[test]
    fn a_guide_sits_where_its_coordinate_is_on_screen() {
        let v = vp();
        let cam = CanvasCamera {
            center: Vec2::new(100.0, 100.0),
            zoom: 3.0,
            ..CanvasCamera::default()
        };
        let g = Guide::new(Axis::X, 150.0);
        let at = g.screen_pt(&cam, &v).unwrap();
        assert!((at - cam.screen_pt_of(&v, Vec2::new(150.0, 0.0)).x).abs() < 1e-3);

        let h = Guide::new(Axis::Y, 20.0);
        let at_y = h.screen_pt(&cam, &v).unwrap();
        assert!((at_y - cam.screen_pt_of(&v, Vec2::new(0.0, 20.0)).y).abs() < 1e-3);
    }

    #[test]
    fn a_quarter_turned_guide_is_measured_along_the_other_screen_axis() {
        let v = vp();
        let cam = CanvasCamera {
            center: Vec2::new(100.0, 100.0),
            zoom: 1.0,
            rotation: FRAC_PI_2,
            ..CanvasCamera::default()
        };
        let g = Guide::new(Axis::X, 150.0);
        // Under a quarter turn a vertical document line is horizontal on
        // screen, so its screen coordinate is a y.
        let at = g.screen_pt(&cam, &v).unwrap();
        assert!((at - cam.screen_pt_of(&v, Vec2::new(150.0, 0.0)).y).abs() < 1e-3);
    }

    #[test]
    fn guide_hit_testing_takes_the_nearest_inside_the_tolerance_only() {
        let v = vp();
        // Zoom 2 on a 2x display: one document pixel is one screen point, so
        // the distances below read directly as the tolerance's own unit.
        let cam = CanvasCamera {
            center: Vec2::new(100.0, 100.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        };
        assert_eq!(cam.scale_pt(&v), 1.0);
        let mut guides = Guides::new();
        guides.add(Guide::new(Axis::X, 100.0)).unwrap();
        guides.add(Guide::new(Axis::X, 110.0)).unwrap();

        let near_first = cam.screen_pt_of(&v, Vec2::new(101.0, 0.0));
        assert_eq!(guides.hit_test(&cam, &v, near_first, 6.0), Some(0));
        let near_second = cam.screen_pt_of(&v, Vec2::new(108.0, 0.0));
        assert_eq!(guides.hit_test(&cam, &v, near_second, 6.0), Some(1));
        // Half way between them, both are 5pt away; the nearest rule still
        // resolves it deterministically to the first.
        let midway = cam.screen_pt_of(&v, Vec2::new(105.0, 0.0));
        assert_eq!(guides.hit_test(&cam, &v, midway, 6.0), Some(0));
        // Outside the tolerance, nothing is grabbed.
        assert_eq!(guides.hit_test(&cam, &v, midway, 4.0), None);
        let far = cam.screen_pt_of(&v, Vec2::new(400.0, 0.0));
        assert_eq!(guides.hit_test(&cam, &v, far, 6.0), None);
    }

    #[test]
    fn hidden_guides_are_not_grabbable() {
        let v = vp();
        let cam = CanvasCamera::default();
        let mut guides = Guides::new();
        guides.add(Guide::new(Axis::X, 0.0)).unwrap();
        let at = cam.screen_pt_of(&v, Vec2::ZERO);
        assert_eq!(guides.hit_test(&cam, &v, at, 8.0), Some(0));
        guides.visible = false;
        assert_eq!(guides.hit_test(&cam, &v, at, 8.0), None);
    }

    #[test]
    fn locked_guides_refuse_to_move_or_be_deleted() {
        let mut guides = Guides::new();
        let free = guides.add(Guide::new(Axis::X, 10.0)).unwrap();
        let pinned = guides.add(Guide::new(Axis::Y, 20.0).locked()).unwrap();
        assert!(guides.move_to(free, 15.0));
        assert_eq!(guides.get(free).unwrap().doc, 15.0);
        assert!(!guides.move_to(pinned, 25.0));
        assert_eq!(guides.get(pinned).unwrap().doc, 20.0);
        assert!(guides.remove(pinned).is_none());
        assert!(!guides.move_to(free, f32::NAN));

        guides.clear();
        assert_eq!(guides.len(), 1, "the locked guide survives Clear Guides");

        guides.locked = true;
        assert!(!guides.move_to(0, 99.0));
    }

    #[test]
    fn the_guide_list_is_bounded_and_refuses_nonsense_positions() {
        let mut guides = Guides::new();
        assert!(guides.add(Guide::new(Axis::X, f32::NAN)).is_none());
        for i in 0..Guides::MAX {
            assert!(guides.add(Guide::new(Axis::X, i as f32)).is_some());
        }
        assert!(guides.add(Guide::new(Axis::X, 1.0)).is_none());
        assert_eq!(guides.len(), Guides::MAX);
    }

    #[test]
    fn labels_print_whole_numbers_without_a_decimal_tail() {
        assert_eq!(format_value(12.0, Unit::Inches), "12");
        assert_eq!(format_value(-0.0, Unit::Pixels), "0");
        assert_eq!(format_value(0.5, Unit::Inches), "0.50");
        assert_eq!(format_value(0.5, Unit::Millimeters), "0.5");
    }

    const GUTTER_PT: f32 = 16.0;

    /// A viewport with the ruler gutters taken off, and the gutter-inclusive
    /// rectangle they came off — exactly the pair `CanvasView` holds.
    fn ruled() -> (Viewport, egui::Rect) {
        let outer_vp = vp();
        let outer = outer_vp.content_rect();
        let inner = outer_vp.inset_by(PanelInsets::new(GUTTER_PT, 0.0, GUTTER_PT, 0.0));
        (inner, outer)
    }

    fn gesture(camera: CanvasCamera, viewport: Viewport, outer: egui::Rect) -> GuideGesture {
        GuideGesture {
            camera,
            viewport,
            outer,
            ruler_thickness_pt: GUTTER_PT,
            rulers_visible: true,
            grab_pt: 6.0,
        }
    }

    #[test]
    fn each_gutter_names_the_axis_of_the_guide_it_makes() {
        let (_, outer) = ruled();
        let [top, left] = gutters(outer, GUTTER_PT);
        assert_eq!(top.height(), GUTTER_PT);
        assert_eq!(left.width(), GUTTER_PT);
        // The corner square belongs to the top gutter, so it is unambiguous.
        assert!(top.contains(outer.min));
        assert!(!left.contains(outer.min));

        let in_top = Vec2::new(outer.min.x + 100.0, outer.min.y + 4.0);
        let in_left = Vec2::new(outer.min.x + 4.0, outer.min.y + 100.0);
        let in_canvas = Vec2::new(outer.min.x + 100.0, outer.min.y + 100.0);
        // A horizontal line holds one y, so the top ruler makes Axis::Y guides.
        assert_eq!(gutter_at(outer, GUTTER_PT, in_top), Some(Axis::Y));
        assert_eq!(gutter_at(outer, GUTTER_PT, in_left), Some(Axis::X));
        assert_eq!(gutter_at(outer, GUTTER_PT, in_canvas), None);
        assert_eq!(gutter_at(outer, GUTTER_PT, Vec2::new(f32::NAN, 0.0)), None);
        // Hidden rulers have no gutter to pull anything out of.
        assert_eq!(gutter_at(outer, 0.0, in_top), None);
    }

    /// The headline: dragging out of the top ruler creates a horizontal guide
    /// at the document coordinate the pointer was let go over.
    #[test]
    fn dragging_out_of_the_top_ruler_creates_a_guide_where_it_is_dropped() {
        let (v, outer) = ruled();
        let cam = CanvasCamera {
            center: Vec2::new(300.0, 200.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        };
        let g = gesture(cam, v, outer);
        let mut guides = Guides::new();

        let press = Vec2::new(outer.min.x + 120.0, outer.min.y + 4.0);
        let GuideGrab::Start(drag) = g.begin(&guides, press) else {
            panic!("a press in the top ruler started nothing");
        };
        assert_eq!(drag, GuideDrag::New { axis: Axis::Y });

        // Still in the gutter: nothing is created yet.
        let drag = g
            .drag(drag, &mut guides, press + Vec2::new(10.0, 2.0))
            .unwrap();
        assert_eq!(drag, GuideDrag::New { axis: Axis::Y });
        assert!(guides.is_empty());

        let drop_at = v.center_pt();
        let drag = g.drag(drag, &mut guides, drop_at).unwrap();
        let GuideDrag::Existing { index } = drag else {
            panic!("leaving the gutter did not create the guide");
        };
        assert_eq!(guides.len(), 1);
        let guide = *guides.get(index).unwrap();
        assert_eq!(guide.axis, Axis::Y);
        let want = cam.doc_of_screen_pt(&v, drop_at).y;
        assert!(
            (guide.doc - want).abs() < 1e-3,
            "the guide landed at {} but the pointer was over {want}",
            guide.doc
        );
        assert_eq!(g.finish(drag, &mut guides, drop_at), GuideDrop::Kept);
        assert_eq!(guides.len(), 1);
    }

    #[test]
    fn the_left_ruler_makes_vertical_guides() {
        let (v, outer) = ruled();
        let cam = CanvasCamera::default();
        let g = gesture(cam, v, outer);
        let mut guides = Guides::new();
        let press = Vec2::new(outer.min.x + 4.0, outer.min.y + 200.0);
        let GuideGrab::Start(drag) = g.begin(&guides, press) else {
            panic!("a press in the left ruler started nothing");
        };
        assert_eq!(drag, GuideDrag::New { axis: Axis::X });
        let at = v.center_pt();
        let drag = g.drag(drag, &mut guides, at).unwrap();
        assert!(matches!(drag, GuideDrag::Existing { .. }));
        assert_eq!(guides.get(0).unwrap().axis, Axis::X);
        assert!((guides.get(0).unwrap().doc - cam.doc_of_screen_pt(&v, at).x).abs() < 1e-3);
    }

    #[test]
    fn a_click_in_the_ruler_that_goes_nowhere_creates_nothing() {
        let (v, outer) = ruled();
        let cam = CanvasCamera::default();
        let g = gesture(cam, v, outer);
        let mut guides = Guides::new();
        let press = Vec2::new(outer.min.x + 120.0, outer.min.y + 4.0);
        let GuideGrab::Start(drag) = g.begin(&guides, press) else {
            panic!("nothing started");
        };
        assert_eq!(g.finish(drag, &mut guides, press), GuideDrop::NeverCreated);
        assert!(guides.is_empty());
    }

    #[test]
    fn an_existing_guide_is_grabbed_and_moved_and_dropping_it_back_deletes_it() {
        let (v, outer) = ruled();
        let cam = CanvasCamera {
            center: Vec2::new(100.0, 100.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        };
        let g = gesture(cam, v, outer);
        let mut guides = Guides::new();
        let index = guides.add(Guide::new(Axis::X, 100.0)).unwrap();

        let on_it = cam.screen_pt_of(&v, Vec2::new(100.0, 0.0));
        let on_it = Vec2::new(on_it.x, v.center_pt().y);
        assert_eq!(
            g.begin(&guides, on_it),
            GuideGrab::Start(GuideDrag::Existing { index })
        );

        let to = v.center_pt() + Vec2::new(50.0, 0.0);
        let drag = g
            .drag(GuideDrag::Existing { index }, &mut guides, to)
            .unwrap();
        let moved_to = guides.get(index).unwrap().doc;
        assert!((moved_to - cam.doc_of_screen_pt(&v, to).x).abs() < 1e-3);
        assert_ne!(moved_to, 100.0);

        // Dropped back into the gutter, it is deleted.
        let back = Vec2::new(outer.min.x + 4.0, outer.min.y + 200.0);
        assert_eq!(g.finish(drag, &mut guides, back), GuideDrop::Deleted);
        assert!(guides.is_empty());
    }

    #[test]
    fn a_locked_guide_refuses_the_drag_rather_than_ignoring_the_press() {
        let (v, outer) = ruled();
        let cam = CanvasCamera {
            center: Vec2::new(100.0, 100.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        };
        let g = gesture(cam, v, outer);
        let mut guides = Guides::new();
        guides.add(Guide::new(Axis::X, 100.0).locked()).unwrap();
        let x = cam.screen_pt_of(&v, Vec2::new(100.0, 0.0)).x;
        let on_it = Vec2::new(x, v.center_pt().y);
        assert_eq!(g.begin(&guides, on_it), GuideGrab::Refused);

        // The whole set locked refuses too, even for an unlocked guide.
        let mut all_locked = Guides::new();
        all_locked.add(Guide::new(Axis::X, 100.0)).unwrap();
        all_locked.locked = true;
        assert_eq!(g.begin(&all_locked, on_it), GuideGrab::Refused);
    }

    #[test]
    fn a_press_that_is_not_the_guides_business_starts_nothing() {
        let (v, outer) = ruled();
        let cam = CanvasCamera::default();
        let g = gesture(cam, v, outer);
        let guides = Guides::new();
        assert_eq!(g.begin(&guides, v.center_pt()), GuideGrab::None);
        // Outside the canvas region entirely — over a dock.
        assert_eq!(g.begin(&guides, Vec2::new(4.0, 4.0)), GuideGrab::None);
        assert_eq!(g.begin(&guides, Vec2::new(f32::NAN, 1.0)), GuideGrab::None);
        // With the rulers hidden the gutters are not there to be grabbed.
        let hidden = GuideGesture {
            rulers_visible: false,
            ..g
        };
        let in_top = Vec2::new(outer.min.x + 120.0, outer.min.y + 4.0);
        assert_eq!(hidden.begin(&guides, in_top), GuideGrab::None);
    }

    #[test]
    fn dragging_a_guide_that_was_removed_underneath_ends_the_drag() {
        let (v, outer) = ruled();
        let cam = CanvasCamera::default();
        let g = gesture(cam, v, outer);
        let mut guides = Guides::new();
        assert!(g
            .drag(GuideDrag::Existing { index: 7 }, &mut guides, v.center_pt())
            .is_none());
        assert_eq!(
            g.finish(GuideDrag::Existing { index: 7 }, &mut guides, Vec2::ZERO),
            GuideDrop::Kept,
            "removing a guide that is not there is not a deletion"
        );
    }

    #[test]
    fn the_oblique_hint_appears_only_when_the_rulers_cannot_read() {
        let (v, _) = ruled();
        assert_eq!(oblique_hint(&CanvasCamera::default(), &v), None);
        let turned = CanvasCamera {
            rotation: FRAC_PI_4,
            ..CanvasCamera::default()
        };
        assert_eq!(oblique_hint(&turned, &v), Some(OBLIQUE_HINT));
        // A quarter turn still reads, just on the other axis.
        let quarter = CanvasCamera {
            rotation: FRAC_PI_2,
            ..CanvasCamera::default()
        };
        assert_eq!(oblique_hint(&quarter, &v), None);
        assert!(!OBLIQUE_HINT.is_empty());
    }

    /// The derived `Default` would have produced hidden guides — invisible,
    /// un-grabbable, and not snapped to — which is the opposite of what
    /// [`Guides::new`] builds.
    #[test]
    fn a_defaulted_guide_set_is_the_same_thing_as_a_new_one() {
        assert_eq!(Guides::default(), Guides::new());
        assert!(
            Guides::default().visible,
            "a defaulted guide set is invisible, so its guides can never be seen or grabbed"
        );
        assert!(!Guides::default().locked);
        // …and it really is hit-testable, which is what `visible` buys.
        let mut guides = Guides::default();
        guides.add(Guide::new(Axis::X, 100.0)).unwrap();
        let cam = CanvasCamera {
            center: Vec2::new(100.0, 100.0),
            ..CanvasCamera::default()
        };
        let at = cam.screen_pt_of(&vp(), Vec2::new(100.0, 100.0));
        assert_eq!(guides.hit_test(&cam, &vp(), at, 4.0), Some(0));
    }

    /// One list of units in the product, in two arithmetics: the dialogs work
    /// in `f64`, the canvas in `f32`, and neither may grow a unit the other
    /// does not have.
    #[test]
    fn units_are_one_vocabulary_in_two_arithmetics() {
        use crate::dialogs::units::Unit as Dialog;
        assert_eq!(Unit::ALL.len(), Dialog::ALL.len());
        for unit in Unit::ALL {
            let there: Dialog = (*unit).into();
            let back: Unit = there.into();
            assert_eq!(back, *unit, "{unit:?} did not survive the round trip");
            assert_eq!(there.short(), unit.abbreviation(), "{unit:?}");
            assert_eq!(there.label(), unit.name(), "{unit:?}");
        }
        for unit in Dialog::ALL {
            let here: Unit = (*unit).into();
            let back: Dialog = here.into();
            assert_eq!(back, *unit, "{unit:?} did not survive the round trip");
        }
    }

    /// Percent divides by the document's own width. With the placeholder
    /// extent a `RulerSpec` starts life with, that is a step of one hundredth
    /// of a pixel — far past [`MAX_TICKS`], so the ruler renders empty. It only
    /// works once somebody tells the spec how big the document is.
    #[test]
    fn percent_ticks_track_the_documents_real_width() {
        let v = vp();
        let cam = CanvasCamera {
            center: Vec2::new(1000.0, 1000.0),
            zoom: 1.0,
            ..CanvasCamera::default()
        };
        let real = RulerSpec {
            unit: Unit::Percent,
            doc_extent: Vec2::splat(2000.0),
            ..RulerSpec::default()
        };
        let ticks = ruler_ticks(&cam, &v, Axis::X, &real);
        assert!(!ticks.is_empty());
        for t in &ticks {
            // One percent of a 2000px document is 20 document pixels.
            assert!((t.doc - t.value * 20.0).abs() < 1e-2, "{t:?}");
        }
        // …and a document half the size halves how many pixels a percent is.
        let half = RulerSpec {
            doc_extent: Vec2::splat(1000.0),
            ..real
        };
        let ticks = ruler_ticks(&cam, &v, Axis::X, &half);
        for t in &ticks {
            assert!((t.doc - t.value * 10.0).abs() < 1e-2, "{t:?}");
        }

        // And against the placeholder extent a `RulerSpec` starts life with,
        // every label is wrong: one "percent" is a hundredth of a pixel, so a
        // ruler nobody told the document's size to reads in five-figure
        // percentages of nothing.
        let placeholder = RulerSpec {
            unit: Unit::Percent,
            ..RulerSpec::default()
        };
        let bogus = ruler_ticks(&cam, &v, Axis::X, &placeholder);
        let worst = bogus
            .iter()
            .filter(|t| t.kind == TickKind::Major)
            .map(|t| t.value)
            .fold(0.0_f32, |a, b| a.max(b.abs()));
        assert!(
            worst > 1000.0,
            "the placeholder extent happened to produce sane percentages, so this proves nothing"
        );
    }

    #[test]
    fn a_half_turn_reverses_both_rulers() {
        let cam = CanvasCamera {
            rotation: PI,
            ..CanvasCamera::default()
        };
        assert_eq!(
            ruler_mapping(&cam, &vp(), Axis::X),
            RulerMapping::Reads {
                axis: Axis::X,
                reversed: true
            }
        );
        assert_eq!(
            ruler_mapping(&cam, &vp(), Axis::Y),
            RulerMapping::Reads {
                axis: Axis::Y,
                reversed: true
            }
        );
    }
}
