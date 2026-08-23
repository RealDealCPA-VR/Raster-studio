//! The text-editing caret and the selection highlight.
//!
//! # Why there is a [`TextLayout`] and not just a `ShapedText`
//!
//! [`text_engine::ShapedText`] already knows where every glyph and line box
//! sits, but a `ShapedGlyph` carries a `FontId` whose inner handle is crate
//! private, so one cannot be built outside `text-engine`. Depending on it
//! directly would make every test here need a real font file and a real shaping
//! run — which would test the shaper, not the caret arithmetic. [`TextLayout`]
//! is therefore the two things the caret actually needs — cluster boxes and
//! line boxes — with [`TextLayout::from_shaped`] as the (trivial, field-copy)
//! adapter. The arithmetic is then testable on a layout written by hand.
//!
//! Turning a *byte range* into rectangles is the whole job: one caret rectangle
//! for a collapsed selection, and one highlight rectangle per visual line for
//! an extended one, because a selection spanning three lines is three
//! rectangles and not one tall box around all of them.
//!
//! Everything is produced in **layer space** — the space `text_engine` reports
//! — and projected by [`project`], so a text layer that has been moved, or a
//! view that has been rotated, needs no special case.

use glam::Vec2;
use text_engine::{ShapedText, TextRun};

use super::camera::CanvasCamera;
use super::geom::DocRect;
use super::viewport::Viewport;

/// One cluster's hit box, in layer space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphBox {
    pub cluster_start: usize,
    /// Exclusive.
    pub cluster_end: usize,
    /// Left edge.
    pub x: f32,
    pub advance: f32,
    pub rtl: bool,
}

/// One visual line's box, in layer space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineBox {
    pub byte_start: usize,
    /// Exclusive.
    pub byte_end: usize,
    pub top: f32,
    pub bottom: f32,
    pub x_min: f32,
    pub x_max: f32,
    pub rtl: bool,
    /// First index into [`TextLayout::glyphs`].
    pub first_glyph: usize,
    pub glyph_count: usize,
}

impl LineBox {
    fn glyph_range(&self) -> std::ops::Range<usize> {
        self.first_glyph..self.first_glyph.saturating_add(self.glyph_count)
    }
}

/// Everything the caret and the highlight need from a laid-out text layer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextLayout {
    pub lines: Vec<LineBox>,
    pub glyphs: Vec<GlyphBox>,
    /// The base em size, used to give an empty line a visible sliver.
    pub em_px: f32,
}

impl TextLayout {
    /// Adopt a shaping result. A field copy, nothing more.
    pub fn from_shaped(shaped: &ShapedText) -> Self {
        Self {
            lines: shaped
                .lines
                .iter()
                .map(|l| LineBox {
                    byte_start: l.byte_start,
                    byte_end: l.byte_end,
                    top: l.top,
                    bottom: l.bottom,
                    x_min: l.x_min,
                    x_max: l.x_max,
                    rtl: l.rtl,
                    first_glyph: l.first_glyph,
                    glyph_count: l.glyph_count,
                })
                .collect(),
            glyphs: shaped
                .glyphs
                .iter()
                .map(|g| GlyphBox {
                    cluster_start: g.cluster_start,
                    cluster_end: g.cluster_end,
                    x: g.x,
                    advance: g.advance,
                    rtl: g.rtl,
                })
                .collect(),
            em_px: shaped.base_size_px,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The glyphs on one line, bounds-checked against a layout whose indices
    /// might not agree with its glyph list.
    fn glyphs_of(&self, line: &LineBox) -> &[GlyphBox] {
        let r = line.glyph_range();
        let start = r.start.min(self.glyphs.len());
        let end = r.end.min(self.glyphs.len());
        &self.glyphs[start..end]
    }
}

/// A caret position and the text selected around it, in bytes into the layer's
/// string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextCursor {
    /// The fixed end of the selection.
    pub anchor: usize,
    /// The moving end; the caret is drawn here.
    pub head: usize,
}

impl TextCursor {
    /// A collapsed cursor at one offset.
    pub fn at(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    /// The selected range, ordered.
    pub fn range(&self) -> std::ops::Range<usize> {
        let lo = self.anchor.min(self.head);
        let hi = self.anchor.max(self.head);
        lo..hi
    }

    /// `true` when nothing is selected and only a caret is drawn.
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.head
    }
}

/// The caret and highlight in layer space.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextOverlayGeometry {
    /// The caret, as a thin rectangle. `None` when the layout has no lines.
    pub caret: Option<DocRect>,
    /// One rectangle per visual line of the selection.
    pub highlight: Vec<DocRect>,
    /// The box the whole run occupies — every line box unioned, in layer
    /// space. Not painted: it is what the canvas hit-tests to decide the
    /// pointer is over live text and owes it an I-beam. `None` for a layout
    /// with no lines.
    pub run_bounds: Option<DocRect>,
}

impl TextOverlayGeometry {
    /// The run's box, falling back to what the caret and the highlight cover
    /// when a caller built this by hand and left
    /// [`TextOverlayGeometry::run_bounds`] unset. `None` only when the overlay
    /// is empty.
    pub fn bounds(&self) -> Option<DocRect> {
        if let Some(b) = self.run_bounds {
            return Some(b);
        }
        let mut out = self.caret;
        for r in &self.highlight {
            out = Some(match out {
                Some(acc) => acc.union(r),
                None => *r,
            });
        }
        out
    }
}

/// How wide the caret is in layer units.
///
/// A caret is a *screen* affordance and has to stay legible at every zoom, so
/// the painter strokes it at a screen-point width; this value exists so the
/// layer-space geometry is complete and testable without a camera.
pub const CARET_WIDTH: f32 = 1.0;

/// Which line a byte offset falls on, and the x it sits at within that line.
fn caret_position(layout: &TextLayout, offset: usize) -> Option<(usize, f32)> {
    let first = layout.lines.first()?;
    // The line whose byte range contains the offset; the last line owns
    // anything past the end, which is where the caret sits after the final
    // character.
    let line_index = layout
        .lines
        .iter()
        .position(|l| offset >= l.byte_start && offset < l.byte_end)
        .unwrap_or(if offset <= first.byte_start {
            0
        } else {
            layout.lines.len() - 1
        });
    let line = &layout.lines[line_index];
    let mut x = if line.rtl { line.x_max } else { line.x_min };
    for g in layout.glyphs_of(line) {
        if offset < g.cluster_end {
            // Before this cluster, or inside it: the caret goes to the near
            // edge rather than splitting a ligature into parts the user cannot
            // see.
            return Some((line_index, if g.rtl { g.x + g.advance } else { g.x }));
        }
        x = if g.rtl { g.x } else { g.x + g.advance };
    }
    Some((line_index, x))
}

/// Build the caret and highlight for `cursor` over `layout`.
pub fn geometry(layout: &TextLayout, cursor: TextCursor) -> TextOverlayGeometry {
    let mut out = TextOverlayGeometry::default();
    if layout.lines.is_empty() {
        return out;
    }
    // The run's own box, so the pointer can be told it is over text even where
    // nothing is selected and the caret is elsewhere.
    out.run_bounds = layout
        .lines
        .iter()
        .map(|l| DocRect::new(Vec2::new(l.x_min, l.top), Vec2::new(l.x_max, l.bottom)))
        .reduce(|a, b| a.union(&b));
    if let Some((line_index, x)) = caret_position(layout, cursor.head) {
        let line = &layout.lines[line_index];
        out.caret = Some(DocRect::new(
            Vec2::new(x, line.top),
            Vec2::new(x + CARET_WIDTH, line.bottom),
        ));
    }

    let range = cursor.range();
    if range.is_empty() {
        return out;
    }
    for line in &layout.lines {
        // The part of this line that is selected.
        let lo = range.start.max(line.byte_start);
        let hi = range.end.min(line.byte_end);
        if lo >= hi {
            continue;
        }
        let mut x0 = f32::INFINITY;
        let mut x1 = f32::NEG_INFINITY;
        for g in layout.glyphs_of(line) {
            if g.cluster_end <= lo || g.cluster_start >= hi {
                continue;
            }
            x0 = x0.min(g.x);
            x1 = x1.max(g.x + g.advance);
        }
        if !x0.is_finite() || !x1.is_finite() || x1 <= x0 {
            // A selected line with no glyphs — an empty paragraph. Show a
            // narrow sliver, so the user can see the newline is selected.
            x0 = line.x_min;
            x1 = x0 + CARET_WIDTH.max(layout.em_px * 0.25);
        }
        out.highlight.push(DocRect::new(
            Vec2::new(x0, line.top),
            Vec2::new(x1, line.bottom),
        ));
    }
    out
}

/// The overlay in screen points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextOverlay {
    /// The caret as a segment from the top of the line box to the bottom, so it
    /// stays a line under view rotation instead of becoming a skewed box.
    pub caret: Option<[Vec2; 2]>,
    /// One quad per selected line, clockwise from its top-left.
    pub highlight: Vec<[Vec2; 4]>,
    /// Whether the caret is in the visible half of its blink cycle.
    pub caret_visible: bool,
}

impl TextOverlay {
    pub fn is_empty(&self) -> bool {
        self.caret.is_none() && self.highlight.is_empty()
    }
}

/// How long one full blink of the caret takes, in seconds.
pub const BLINK_PERIOD_SECS: f64 = 1.06;

/// Whether the caret is showing at this moment.
///
/// A pure function of the clock, like the marching ants: no state to keep in
/// sync, and no way for a dropped frame to leave the caret stuck off.
pub fn caret_visible(time_secs: f64) -> bool {
    if !time_secs.is_finite() {
        return true;
    }
    time_secs.rem_euclid(BLINK_PERIOD_SECS) < BLINK_PERIOD_SECS * 0.5
}

/// Project the layer-space geometry into screen points.
///
/// `origin` is where the text layer's own origin sits in document space.
pub fn project(
    geometry: &TextOverlayGeometry,
    origin: Vec2,
    camera: &CanvasCamera,
    viewport: &Viewport,
    time_secs: f64,
) -> TextOverlay {
    let mut out = TextOverlay {
        caret_visible: caret_visible(time_secs),
        ..TextOverlay::default()
    };
    if viewport.is_degenerate() {
        return out;
    }
    let to_screen = |p: Vec2| camera.screen_pt_of(viewport, origin + p);
    if let Some(c) = geometry.caret {
        let top = to_screen(Vec2::new(c.min.x, c.min.y));
        let bottom = to_screen(Vec2::new(c.min.x, c.max.y));
        if top.is_finite() && bottom.is_finite() {
            out.caret = Some([top, bottom]);
        }
    }
    for r in &geometry.highlight {
        let quad = [
            to_screen(r.min),
            to_screen(Vec2::new(r.max.x, r.min.y)),
            to_screen(r.max),
            to_screen(Vec2::new(r.min.x, r.max.y)),
        ];
        if quad.iter().all(|p| p.is_finite()) {
            out.highlight.push(quad);
        }
    }
    out
}

/// The layer-space origin of a [`TextRun`], so a caller does not have to
/// remember which field it lives in or that it is a bare array.
pub fn run_origin(run: &TextRun) -> Vec2 {
    Vec2::new(run.origin[0], run.origin[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::viewport::PanelInsets;

    fn vp() -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            2.0,
        )
    }

    fn cam() -> CanvasCamera {
        CanvasCamera {
            center: Vec2::new(50.0, 50.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        }
    }

    /// Two lines of four ten-wide clusters each, twenty units tall.
    fn two_lines() -> TextLayout {
        let mut glyphs = Vec::new();
        for line in 0..2usize {
            for i in 0..4usize {
                let byte = line * 4 + i;
                glyphs.push(GlyphBox {
                    cluster_start: byte,
                    cluster_end: byte + 1,
                    x: i as f32 * 10.0,
                    advance: 10.0,
                    rtl: false,
                });
            }
        }
        TextLayout {
            lines: (0..2usize)
                .map(|i| LineBox {
                    byte_start: i * 4,
                    byte_end: i * 4 + 4,
                    top: i as f32 * 20.0,
                    bottom: i as f32 * 20.0 + 20.0,
                    x_min: 0.0,
                    x_max: 40.0,
                    rtl: false,
                    first_glyph: i * 4,
                    glyph_count: 4,
                })
                .collect(),
            glyphs,
            em_px: 20.0,
        }
    }

    #[test]
    fn a_collapsed_cursor_gives_a_caret_and_no_highlight() {
        let t = two_lines();
        let g = geometry(&t, TextCursor::at(2));
        let caret = g.caret.unwrap();
        assert_eq!(caret.min.x, 20.0, "the caret sits before the third cluster");
        assert_eq!(caret.min.y, 0.0);
        assert_eq!(caret.max.y, 20.0);
        assert!(g.highlight.is_empty());
    }

    #[test]
    fn the_caret_reaches_the_start_and_the_end() {
        let t = two_lines();
        assert_eq!(geometry(&t, TextCursor::at(0)).caret.unwrap().min.x, 0.0);
        // Offset 4 begins the second line, so the caret goes there.
        let second = geometry(&t, TextCursor::at(4)).caret.unwrap();
        assert_eq!(second.min.x, 0.0);
        assert_eq!(second.min.y, 20.0);
        // Past the whole string it sits after the last cluster of the last line.
        let past = geometry(&t, TextCursor::at(99)).caret.unwrap();
        assert_eq!(past.min.x, 40.0);
        assert_eq!(past.min.y, 20.0);
    }

    #[test]
    fn a_selection_inside_one_line_is_one_rectangle() {
        let t = two_lines();
        let g = geometry(&t, TextCursor { anchor: 1, head: 3 });
        assert_eq!(g.highlight.len(), 1);
        let r = g.highlight[0];
        assert_eq!(r.min.x, 10.0);
        assert_eq!(r.max.x, 30.0);
        assert_eq!(r.min.y, 0.0);
        assert_eq!(r.max.y, 20.0);
        // The caret is still drawn, at the moving end.
        assert_eq!(g.caret.unwrap().min.x, 30.0);
    }

    #[test]
    fn a_selection_across_lines_is_one_rectangle_per_line() {
        let t = two_lines();
        let g = geometry(&t, TextCursor { anchor: 2, head: 6 });
        assert_eq!(g.highlight.len(), 2);
        assert_eq!(g.highlight[0].min.x, 20.0);
        assert_eq!(g.highlight[0].max.x, 40.0);
        assert_eq!(g.highlight[1].min.x, 0.0);
        assert_eq!(g.highlight[1].max.x, 20.0);
        // The two boxes are on different lines, not stacked into one.
        assert_ne!(g.highlight[0].min.y, g.highlight[1].min.y);
    }

    #[test]
    fn the_selection_is_direction_agnostic_but_the_caret_is_not() {
        let t = two_lines();
        let forward = geometry(&t, TextCursor { anchor: 1, head: 3 });
        let backward = geometry(&t, TextCursor { anchor: 3, head: 1 });
        assert_eq!(forward.highlight, backward.highlight);
        assert_ne!(forward.caret, backward.caret);
        assert_eq!(TextCursor { anchor: 3, head: 1 }.range(), 1..3);
        assert!(TextCursor::at(4).is_collapsed());
        assert!(!TextCursor { anchor: 1, head: 2 }.is_collapsed());
    }

    #[test]
    fn an_empty_line_still_shows_that_its_newline_is_selected() {
        let mut t = two_lines();
        // Insert an empty line between the two.
        t.lines.insert(
            1,
            LineBox {
                byte_start: 4,
                byte_end: 5,
                top: 20.0,
                bottom: 40.0,
                x_min: 0.0,
                x_max: 0.0,
                rtl: false,
                first_glyph: 4,
                glyph_count: 0,
            },
        );
        let g = geometry(&t, TextCursor { anchor: 0, head: 8 });
        let sliver = g
            .highlight
            .iter()
            .find(|r| (r.min.y - 20.0).abs() < 1e-4)
            .unwrap();
        assert!(sliver.width() > 0.0, "an empty line vanished");
    }

    #[test]
    fn an_empty_layout_produces_nothing() {
        let t = TextLayout::default();
        assert!(t.is_empty());
        let g = geometry(&t, TextCursor::at(0));
        assert!(g.caret.is_none());
        assert!(g.highlight.is_empty());
    }

    #[test]
    fn a_layout_whose_glyph_indices_are_out_of_range_does_not_panic() {
        let t = TextLayout {
            lines: vec![LineBox {
                byte_start: 0,
                byte_end: 10,
                top: 0.0,
                bottom: 10.0,
                x_min: 0.0,
                x_max: 10.0,
                rtl: false,
                first_glyph: 900,
                glyph_count: usize::MAX,
            }],
            glyphs: vec![GlyphBox {
                cluster_start: 0,
                cluster_end: 1,
                x: 0.0,
                advance: 5.0,
                rtl: false,
            }],
            em_px: 10.0,
        };
        let g = geometry(&t, TextCursor { anchor: 0, head: 5 });
        assert!(g.caret.is_some());
        assert_eq!(g.highlight.len(), 1);
    }

    #[test]
    fn a_right_to_left_line_puts_the_caret_on_the_other_side_of_the_cluster() {
        let t = TextLayout {
            lines: vec![LineBox {
                byte_start: 0,
                byte_end: 2,
                top: 0.0,
                bottom: 10.0,
                x_min: 0.0,
                x_max: 20.0,
                rtl: true,
                first_glyph: 0,
                glyph_count: 2,
            }],
            glyphs: vec![
                GlyphBox {
                    cluster_start: 0,
                    cluster_end: 1,
                    x: 10.0,
                    advance: 10.0,
                    rtl: true,
                },
                GlyphBox {
                    cluster_start: 1,
                    cluster_end: 2,
                    x: 0.0,
                    advance: 10.0,
                    rtl: true,
                },
            ],
            em_px: 10.0,
        };
        // Before the first (right-most) cluster the caret is at its right edge.
        assert_eq!(geometry(&t, TextCursor::at(0)).caret.unwrap().min.x, 20.0);
        assert_eq!(geometry(&t, TextCursor::at(1)).caret.unwrap().min.x, 10.0);
    }

    #[test]
    fn the_caret_blinks_on_a_fixed_cycle() {
        assert!(caret_visible(0.0));
        assert!(caret_visible(BLINK_PERIOD_SECS * 0.25));
        assert!(!caret_visible(BLINK_PERIOD_SECS * 0.75));
        assert!(caret_visible(BLINK_PERIOD_SECS * 1.25));
        // Never stuck off on a bad clock.
        assert!(caret_visible(f64::NAN));
    }

    #[test]
    fn projection_follows_the_camera_and_the_layer_origin() {
        let v = vp();
        let c = cam();
        let t = two_lines();
        let g = geometry(&t, TextCursor { anchor: 0, head: 2 });
        let origin = Vec2::new(30.0, 40.0);
        let o = project(&g, origin, &c, &v, 0.0);
        let [top, bottom] = o.caret.unwrap();
        assert_eq!(top, c.screen_pt_of(&v, origin + Vec2::new(20.0, 0.0)));
        assert_eq!(bottom, c.screen_pt_of(&v, origin + Vec2::new(20.0, 20.0)));
        assert_eq!(o.highlight.len(), 1);
        assert_eq!(o.highlight[0][0], c.screen_pt_of(&v, origin));
        assert!(o.caret_visible);
        assert!(!o.is_empty());
    }

    /// The highlight is a quad, not a rectangle, so a rotated view does not
    /// leave it axis-aligned while the text is not.
    #[test]
    fn the_highlight_rotates_with_the_view() {
        let v = vp();
        let turned = CanvasCamera {
            rotation: std::f32::consts::FRAC_PI_4,
            ..cam()
        };
        let t = two_lines();
        let g = geometry(&t, TextCursor { anchor: 0, head: 2 });
        let o = project(&g, Vec2::ZERO, &turned, &v, 0.0);
        let quad = o.highlight[0];
        let top_edge = quad[1] - quad[0];
        assert!(
            top_edge.y.abs() > 1.0,
            "the highlight stayed axis-aligned under rotation"
        );
        // The caret leans with it too.
        let [a, b] = o.caret.unwrap();
        assert!((b - a).x.abs() > 1.0);
    }

    #[test]
    fn a_collapsed_viewport_projects_nothing() {
        let collapsed = Viewport::new(Vec2::splat(50.0), PanelInsets::uniform(50.0), 1.0);
        let t = two_lines();
        let g = geometry(&t, TextCursor::at(1));
        let o = project(&g, Vec2::ZERO, &cam(), &collapsed, 0.0);
        assert!(o.is_empty());
    }

    #[test]
    fn a_text_runs_origin_is_read_from_the_run() {
        let mut run = TextRun::point("hi", "Sans", 12.0);
        run.origin = [7.0, 9.0];
        assert_eq!(run_origin(&run), Vec2::new(7.0, 9.0));
    }
}
