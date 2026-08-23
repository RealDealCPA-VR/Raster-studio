//! The Navigator and Info panels — the two that report rather than edit.
//!
//! # Navigator
//!
//! A thumbnail of the whole document with a rectangle showing what the viewport
//! is looking at. All of it is arithmetic on two rectangles and a scale factor,
//! which is exactly the kind of thing that is wrong by a factor of the zoom
//! level until somebody writes the test — so it is [`ViewBox`], and it is
//! tested.
//!
//! # Info
//!
//! The pointer's document coordinates, the colour under it, and the selection's
//! bounds. Every readout formats through this module so the status bar and the
//! panel cannot disagree about how a number is written.

use editor_core::{Document, Selection};

/// The zoom levels the zoom control steps through, ascending.
///
/// Photoshop's ladder: a step is always a visible change and always lands on a
/// value that can be typed back in.
pub const ZOOM_STEPS: &[f32] = &[
    0.0025, 0.005, 0.01, 0.0167, 0.025, 0.0333, 0.05, 0.0667, 0.10, 0.125, 0.1667, 0.25, 0.3333,
    0.50, 0.6667, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0,
];

/// Smallest and largest zoom the viewport allows.
pub const MIN_ZOOM: f32 = 0.0025;
pub const MAX_ZOOM: f32 = 32.0;

/// The next zoom step above `zoom`, or the ceiling.
pub fn zoom_in(zoom: f32) -> f32 {
    if !zoom.is_finite() {
        return 1.0;
    }
    ZOOM_STEPS
        .iter()
        .copied()
        // A hair of tolerance, so being *at* a step advances off it rather
        // than finding itself.
        .find(|s| *s > zoom * 1.0001)
        .unwrap_or(MAX_ZOOM)
}

/// The next zoom step below `zoom`, or the floor.
pub fn zoom_out(zoom: f32) -> f32 {
    if !zoom.is_finite() {
        return 1.0;
    }
    ZOOM_STEPS
        .iter()
        .rev()
        .copied()
        .find(|s| *s < zoom * 0.9999)
        .unwrap_or(MIN_ZOOM)
}

/// A zoom clamped into range, with a non-finite value falling back to 100%.
pub fn clamp_zoom(zoom: f32) -> f32 {
    if zoom.is_finite() {
        zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// The zoom that fits `doc_size` inside `viewport`, leaving a small margin.
///
/// A zero-sized viewport — which happens for one frame while a window is being
/// created, and every frame while it is minimised — answers 100% rather than
/// zero or infinity.
pub fn fit_zoom(doc_size: (u32, u32), viewport: (f32, f32)) -> f32 {
    let (w, h) = (doc_size.0 as f32, doc_size.1 as f32);
    if w <= 0.0 || h <= 0.0 || viewport.0 <= 0.0 || viewport.1 <= 0.0 {
        return 1.0;
    }
    const MARGIN: f32 = 0.94;
    clamp_zoom((viewport.0 / w).min(viewport.1 / h) * MARGIN)
}

/// Format a zoom as the panel and the status bar both write it.
///
/// Three cases, and each earns its place: below 1% two decimals are needed to
/// tell adjacent ladder steps apart at all; a fractional percentage below 100%
/// keeps one decimal, because `12.5%` and `16.7%` are real rungs and rounding
/// them to `13%` and `17%` makes two different zooms print the same; everything
/// else is a whole number, which is what a user types back in.
pub fn format_zoom(zoom: f32) -> String {
    let percent = clamp_zoom(zoom) * 100.0;
    if percent < 1.0 {
        format!("{percent:.2}%")
    } else if percent < 100.0 && (percent - percent.round()).abs() > 0.05 {
        format!("{percent:.1}%")
    } else {
        format!("{}%", percent.round())
    }
}

/// Parse a zoom the user typed. Accepts `50`, `50%`, ` 50 % `, `12.5%`.
pub fn parse_zoom(text: &str) -> Option<f32> {
    let t = text.trim().trim_end_matches('%').trim();
    let percent: f32 = t.parse().ok()?;
    (percent.is_finite() && percent > 0.0).then(|| clamp_zoom(percent / 100.0))
}

/// The Navigator's geometry: where the viewport sits over the document.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ViewBox {
    /// Top-left of the visible area, in document pixels. May be negative when
    /// the document is smaller than the viewport.
    pub origin: (f32, f32),
    /// Size of the visible area, in document pixels.
    pub size: (f32, f32),
}

impl ViewBox {
    /// What the viewport can see, given where it is centred and how far in.
    pub fn from_viewport(center: (f32, f32), viewport_px: (f32, f32), zoom: f32) -> Self {
        let zoom = clamp_zoom(zoom);
        let size = (viewport_px.0 / zoom, viewport_px.1 / zoom);
        Self {
            origin: (center.0 - size.0 * 0.5, center.1 - size.1 * 0.5),
            size,
        }
    }

    /// The box as a fraction of the document, clamped into `0.0..=1.0` on both
    /// axes — which is what the thumbnail actually draws.
    ///
    /// A viewport wider than the document produces a full-width rectangle
    /// rather than one hanging off the side of the thumbnail.
    pub fn normalised(&self, doc_size: (u32, u32)) -> (f32, f32, f32, f32) {
        let (w, h) = (doc_size.0 as f32, doc_size.1 as f32);
        if w <= 0.0 || h <= 0.0 {
            return (0.0, 0.0, 1.0, 1.0);
        }
        let x0 = (self.origin.0 / w).clamp(0.0, 1.0);
        let y0 = (self.origin.1 / h).clamp(0.0, 1.0);
        let x1 = ((self.origin.0 + self.size.0) / w).clamp(0.0, 1.0);
        let y1 = ((self.origin.1 + self.size.1) / h).clamp(0.0, 1.0);
        (x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }

    /// `true` when the whole document is on screen, so the navigator rectangle
    /// covers everything and dragging it does nothing.
    pub fn covers_document(&self, doc_size: (u32, u32)) -> bool {
        let (_, _, w, h) = self.normalised(doc_size);
        w >= 1.0 && h >= 1.0
    }

    /// The document point a click at `fraction` of the thumbnail centres on.
    pub fn center_for_click(fraction: (f32, f32), doc_size: (u32, u32)) -> (f32, f32) {
        (
            fraction.0.clamp(0.0, 1.0) * doc_size.0 as f32,
            fraction.1.clamp(0.0, 1.0) * doc_size.1 as f32,
        )
    }
}

/// One line of the Info panel.
#[derive(Clone, PartialEq, Debug)]
pub struct InfoReadout {
    pub label: &'static str,
    pub value: String,
}

/// Everything the Info panel reports.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct InfoState {
    /// Pointer position in document pixels, when it is over the canvas.
    pub pointer: Option<(f32, f32)>,
    /// Colour under the pointer, straight-alpha sRGB.
    pub sampled: Option<[f32; 4]>,
}

impl InfoState {
    /// The readouts, in panel order. Every one is present in every state, with
    /// an em dash where there is nothing to report — a row that disappears
    /// makes the panel jump about as the pointer moves.
    pub fn readouts(&self, doc: &Document) -> Vec<InfoReadout> {
        const NOTHING: &str = "—";
        let pointer = match self.pointer {
            Some((x, y)) => format!("{}, {}", x.floor() as i64, y.floor() as i64),
            None => NOTHING.to_string(),
        };
        let (rgb, hex) = match self.sampled {
            Some(c) => (
                format!(
                    "{}, {}, {}",
                    (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (c[2].clamp(0.0, 1.0) * 255.0).round() as u8
                ),
                super::color::format_hex(c),
            ),
            None => (NOTHING.to_string(), NOTHING.to_string()),
        };
        vec![
            InfoReadout {
                label: "Pointer",
                value: pointer,
            },
            InfoReadout {
                label: "RGB",
                value: rgb,
            },
            InfoReadout {
                label: "Hex",
                value: hex,
            },
            InfoReadout {
                label: "Document",
                value: format!("{} × {} px", doc.width(), doc.height()),
            },
            InfoReadout {
                label: "Selection",
                value: format_selection(&doc.selection),
            },
        ]
    }
}

/// How a selection's extent is written, everywhere it is written.
pub fn format_selection(selection: &Selection) -> String {
    // `bounds()` is already `None` both for "no selection" and for a selection
    // that happens to select nothing, which is exactly the split this readout
    // wants — see `Selection::is_empty`'s documentation for why testing
    // `is_empty` here instead would be wrong.
    match selection.bounds() {
        Some((min, max)) => {
            format!(
                "{} × {} at {}, {}",
                (max.x - min.x).max(0),
                (max.y - min.y).max(0),
                min.x,
                min.y
            )
        }
        _ => "None".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::SelectionMask;
    use glam::IVec2;

    #[test]
    fn the_zoom_ladder_is_ascending_and_contains_one_hundred_percent() {
        assert!(ZOOM_STEPS.windows(2).all(|w| w[0] < w[1]));
        assert!(ZOOM_STEPS.contains(&1.0));
        assert_eq!(ZOOM_STEPS.first().copied(), Some(MIN_ZOOM));
        assert_eq!(ZOOM_STEPS.last().copied(), Some(MAX_ZOOM));
    }

    #[test]
    fn zooming_in_and_out_walks_the_ladder_one_step_at_a_time() {
        assert_eq!(zoom_in(1.0), 2.0);
        assert_eq!(zoom_out(1.0), 0.6667);
        assert_eq!(zoom_in(0.5), 0.6667);
        // Between two steps, the next step in that direction.
        assert_eq!(zoom_in(1.5), 2.0);
        assert_eq!(zoom_out(1.5), 1.0);
    }

    #[test]
    fn zooming_stops_at_the_ends_rather_than_running_off() {
        assert_eq!(zoom_in(MAX_ZOOM), MAX_ZOOM);
        assert_eq!(zoom_out(MIN_ZOOM), MIN_ZOOM);
        assert_eq!(zoom_in(1e9), MAX_ZOOM);
        assert_eq!(zoom_out(0.0), MIN_ZOOM);
    }

    #[test]
    fn a_non_finite_zoom_lands_on_one_hundred_percent() {
        assert_eq!(zoom_in(f32::NAN), 1.0);
        assert_eq!(zoom_out(f32::NAN), 1.0);
        assert_eq!(clamp_zoom(f32::NAN), 1.0);
        assert_eq!(clamp_zoom(f32::INFINITY), 1.0);
    }

    #[test]
    fn fit_uses_the_tighter_axis_and_leaves_a_margin() {
        // A wide document in a square viewport is limited by width.
        let z = fit_zoom((1000, 500), (500.0, 500.0));
        assert!(z < 0.5 && z > 0.4, "{z}");
        assert!(1000.0 * z <= 500.0);
        assert!(500.0 * z <= 500.0);
        // A tall one is limited by height.
        let z = fit_zoom((500, 1000), (500.0, 500.0));
        assert!(1000.0 * z <= 500.0);
    }

    #[test]
    fn fitting_into_nothing_answers_one_hundred_percent() {
        assert_eq!(fit_zoom((100, 100), (0.0, 0.0)), 1.0);
        assert_eq!(fit_zoom((0, 0), (500.0, 500.0)), 1.0);
    }

    #[test]
    fn zoom_formats_and_parses_back() {
        assert_eq!(format_zoom(1.0), "100%");
        assert_eq!(format_zoom(0.5), "50%");
        assert_eq!(format_zoom(0.125), "12.5%");
        assert_eq!(format_zoom(0.005), "0.50%");
        assert_eq!(format_zoom(16.0), "1600%");
        // Two adjacent ladder rungs must not print the same string.
        assert_ne!(format_zoom(0.125), format_zoom(0.1667));
        assert_eq!(format_zoom(0.1667), "16.7%");
        assert_eq!(format_zoom(0.6667), "66.7%");
    }

    #[test]
    fn zoom_parses_what_a_person_types() {
        assert_eq!(parse_zoom("100"), Some(1.0));
        assert_eq!(parse_zoom("100%"), Some(1.0));
        assert_eq!(parse_zoom("  50 % "), Some(0.5));
        assert_eq!(parse_zoom("12.5%"), Some(0.125));
        // Out of range clamps rather than failing: the user meant "as far as
        // it goes".
        assert_eq!(parse_zoom("999999%"), Some(MAX_ZOOM));
    }

    #[test]
    fn zoom_refuses_what_is_not_a_number() {
        for bad in ["", "%", "abc", "-50%", "0%", "NaN"] {
            assert_eq!(parse_zoom(bad), None, "{bad:?} parsed");
        }
    }

    #[test]
    fn the_view_box_shrinks_as_the_zoom_climbs() {
        let a = ViewBox::from_viewport((500.0, 500.0), (800.0, 600.0), 1.0);
        assert_eq!(a.size, (800.0, 600.0));
        assert_eq!(a.origin, (100.0, 200.0));
        let b = ViewBox::from_viewport((500.0, 500.0), (800.0, 600.0), 2.0);
        assert_eq!(b.size, (400.0, 300.0));
        assert_eq!(b.origin, (300.0, 350.0));
    }

    #[test]
    fn the_navigator_rectangle_is_a_fraction_of_the_document() {
        let v = ViewBox {
            origin: (250.0, 500.0),
            size: (500.0, 250.0),
        };
        let (x, y, w, h) = v.normalised((1000, 1000));
        assert!((x - 0.25).abs() < 1e-6);
        assert!((y - 0.5).abs() < 1e-6);
        assert!((w - 0.5).abs() < 1e-6);
        assert!((h - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_view_wider_than_the_document_clamps_to_the_thumbnail() {
        let v = ViewBox {
            origin: (-500.0, -500.0),
            size: (4000.0, 4000.0),
        };
        let (x, y, w, h) = v.normalised((1000, 1000));
        assert_eq!((x, y, w, h), (0.0, 0.0, 1.0, 1.0));
        assert!(v.covers_document((1000, 1000)));

        let inside = ViewBox {
            origin: (100.0, 100.0),
            size: (200.0, 200.0),
        };
        assert!(!inside.covers_document((1000, 1000)));
    }

    #[test]
    fn a_zero_sized_document_does_not_divide_by_zero() {
        let v = ViewBox {
            origin: (0.0, 0.0),
            size: (100.0, 100.0),
        };
        assert_eq!(v.normalised((0, 0)), (0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn clicking_the_thumbnail_centres_on_that_point() {
        assert_eq!(
            ViewBox::center_for_click((0.5, 0.5), (1000, 800)),
            (500.0, 400.0)
        );
        // Off the edge lands on the edge, not outside the document.
        assert_eq!(
            ViewBox::center_for_click((-1.0, 2.0), (1000, 800)),
            (0.0, 800.0)
        );
    }

    #[test]
    fn info_always_reports_every_row_even_with_nothing_to_report() {
        let doc = Document::new(640, 480, "Test");
        let rows = InfoState::default().readouts(&doc);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].value, "—");
        assert_eq!(rows[3].value, "640 × 480 px");
        assert_eq!(rows[4].value, "None");
        assert!(rows.iter().all(|r| !r.label.is_empty()));
    }

    #[test]
    fn info_reports_the_pointer_and_the_colour_under_it() {
        let doc = Document::new(640, 480, "Test");
        let state = InfoState {
            pointer: Some((12.7, 40.2)),
            sampled: Some([1.0, 0.5, 0.0, 1.0]),
        };
        let rows = state.readouts(&doc);
        assert_eq!(rows[0].value, "12, 40");
        assert_eq!(rows[1].value, "255, 128, 0");
        assert_eq!(rows[2].value, "#FF8000");
    }

    #[test]
    fn info_reports_the_selections_extent() {
        let mut doc = Document::new(640, 480, "Test");
        assert_eq!(format_selection(&doc.selection), "None");
        let mask = SelectionMask::filled(IVec2::new(10, 20), 100, 50).unwrap();
        doc.selection = Selection::Mask(mask);
        let rows = InfoState::default().readouts(&doc);
        assert_eq!(rows[4].value, "100 × 50 at 10, 20");
    }
}
