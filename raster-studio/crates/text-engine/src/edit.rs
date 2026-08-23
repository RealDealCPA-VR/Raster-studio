//! Editing geometry: point → character index, index → caret, range → selection.
//!
//! Everything here works on a [`ShapedText`] alone, in the same layer-space
//! coordinates the layout reported, and everything is expressed in **byte**
//! indices into [`ShapedText::text`] so the caller can slice the string
//! directly.
//!
//! Caret positions are derived from the shaped glyphs rather than from the
//! string, so a ligature — one glyph covering several characters — still
//! offers a caret between those characters, subdivided across the glyph's
//! advance. That is what keeps [`ShapedText::caret_rect`] and
//! [`ShapedText::hit_test`] exact inverses of each other.
//!
//! Bidirectional text makes that non-trivial: at a direction boundary two
//! different glyphs legitimately offer a caret for the same byte index from
//! opposite sides of the line. [`ShapedText::caret_stops`] resolves that by
//! ownership — the caret for an index belongs to the glyph whose *cluster*
//! covers that index, taken at that glyph's leading edge — rather than to
//! whichever glyph happened to come first in visual order. So the caret before
//! the first character of an embedded Latin run sits at the start of that run,
//! not at the far end of it.

use std::collections::BTreeMap;

use crate::layout::{Rect, ShapedText};

/// A caret position on a line: a byte index and the x it sits at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaretStop {
    /// Byte index into [`ShapedText::text`].
    pub index: usize,
    /// Caret x in layer space.
    pub x: f32,
}

impl ShapedText {
    /// Every caret position on `line`, one per byte index, ordered along the
    /// line from left to right.
    ///
    /// Where several glyphs claim the same index — which happens at every
    /// direction boundary in bidirectional text — the stop is taken from the
    /// glyph that *owns* the index (the one whose cluster covers it), at that
    /// glyph's leading edge. A glyph's trailing edge only supplies a caret for
    /// an index no glyph owns, such as the end of a line, and the line's own
    /// edges only supply one when no glyph offers anything at all.
    ///
    /// Returns an empty vector if `line` is out of range.
    #[must_use]
    pub fn caret_stops(&self, line: usize) -> Vec<CaretStop> {
        /// Leading edge of the glyph whose cluster covers the index.
        const OWNED: u8 = 0;
        /// Trailing edge of a cluster.
        const TRAILING: u8 = 1;
        /// The line box's own edge.
        const LINE_EDGE: u8 = 2;

        let Some(shaped_line) = self.lines.get(line) else {
            return Vec::new();
        };
        let mut candidates: Vec<(usize, f32, u8)> = Vec::new();

        for glyph in &self.glyphs[shaped_line.glyph_range()] {
            let cluster = self
                .text
                .get(glyph.cluster_start..glyph.cluster_end)
                .unwrap_or("");
            let count = cluster.chars().count().max(1) as f32;
            let step = glyph.advance / count;
            for (ordinal, (offset, _)) in cluster.char_indices().enumerate() {
                let along = step * ordinal as f32;
                let x = if glyph.rtl {
                    glyph.x + glyph.advance - along
                } else {
                    glyph.x + along
                };
                candidates.push((glyph.cluster_start + offset, x, OWNED));
            }
            let end_x = if glyph.rtl {
                glyph.x
            } else {
                glyph.x + glyph.advance
            };
            candidates.push((glyph.cluster_end, end_x, TRAILING));
        }

        let edge_x = if shaped_line.rtl {
            shaped_line.x_min
        } else {
            shaped_line.x_max
        };
        candidates.push((shaped_line.byte_end, edge_x, LINE_EDGE));
        candidates.push((shaped_line.byte_start, shaped_line.x_min, LINE_EDGE));

        let mut best: BTreeMap<usize, (f32, u8)> = BTreeMap::new();
        for (index, x, priority) in candidates {
            let slot = best.entry(index).or_insert((x, priority));
            if priority < slot.1 {
                *slot = (x, priority);
            }
        }
        let mut stops: Vec<CaretStop> = best
            .into_iter()
            .map(|(index, (x, _))| CaretStop { index, x })
            .collect();
        stops.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.index.cmp(&b.index)));
        stops
    }

    /// Index of the visual line that owns `y`, clamped to the block.
    #[must_use]
    pub fn line_at_y(&self, y: f32) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        for (index, line) in self.lines.iter().enumerate() {
            if y < line.bottom {
                return index;
            }
        }
        self.lines.len() - 1
    }

    /// Index of the first visual line that contains `index`.
    #[must_use]
    pub fn line_of_index(&self, index: usize) -> usize {
        for (line_index, line) in self.lines.iter().enumerate() {
            if index >= line.byte_start && index <= line.byte_end {
                return line_index;
            }
        }
        self.lines.len().saturating_sub(1)
    }

    /// Map a point in layer space to the byte index of the closest caret
    /// position. Always returns a valid index; out-of-range points clamp.
    #[must_use]
    pub fn hit_test(&self, x: f32, y: f32) -> usize {
        let line_index = self.line_at_y(y);
        let stops = self.caret_stops(line_index);
        let fallback = self.lines.get(line_index).map_or(0, |line| line.byte_start);
        let mut best = fallback;
        let mut best_distance = f32::INFINITY;
        for stop in stops {
            let distance = (stop.x - x).abs();
            if distance < best_distance - 1e-6
                || ((distance - best_distance).abs() <= 1e-6 && stop.index < best)
            {
                best_distance = distance;
                best = stop.index;
            }
        }
        best
    }

    /// The caret rectangle for a byte index: zero width, spanning the line box.
    ///
    /// The UI picks its own caret thickness; the engine only says where the
    /// caret's leading edge is and how tall the line is.
    #[must_use]
    pub fn caret_rect(&self, index: usize) -> Rect {
        let line_index = self.line_of_index(index);
        let Some(line) = self.lines.get(line_index) else {
            return Rect::default();
        };
        let stops = self.caret_stops(line_index);
        let exact = stops.iter().find(|stop| stop.index == index).map(|s| s.x);
        let x = exact.unwrap_or_else(|| {
            // Inside a cluster we did not subdivide (or past the end): take the
            // nearest stop at or before the index.
            stops
                .iter()
                .filter(|stop| stop.index <= index)
                .max_by_key(|stop| stop.index)
                .map_or(line.x_min, |stop| stop.x)
        });
        Rect {
            x,
            y: line.top,
            width: 0.0,
            height: line.bottom - line.top,
        }
    }

    /// Rectangles covering the byte range `start..end`, one or more per line.
    ///
    /// Bidirectional text produces several rectangles on a line, one per
    /// directional stretch; an empty or inverted range produces none.
    #[must_use]
    pub fn selection_rects(&self, start: usize, end: usize) -> Vec<Rect> {
        if start >= end {
            return Vec::new();
        }
        let mut out = Vec::new();
        for line in &self.lines {
            let mut spans: Vec<(f32, f32)> = Vec::new();
            for glyph in &self.glyphs[line.glyph_range()] {
                let lo = glyph.cluster_start.max(start);
                let hi = glyph.cluster_end.min(end);
                if lo >= hi {
                    continue;
                }
                let cluster = self
                    .text
                    .get(glyph.cluster_start..glyph.cluster_end)
                    .unwrap_or("");
                let count = cluster.chars().count().max(1) as f32;
                let step = glyph.advance / count;
                let before = |index: usize| -> f32 {
                    cluster
                        .char_indices()
                        .take_while(|(offset, _)| glyph.cluster_start + offset < index)
                        .count() as f32
                };
                let (from, to) = (before(lo), before(hi).max(before(lo) + 1.0));
                let (a, b) = if glyph.rtl {
                    (
                        glyph.x + glyph.advance - step * to,
                        glyph.x + glyph.advance - step * from,
                    )
                } else {
                    (glyph.x + step * from, glyph.x + step * to)
                };
                spans.push((a.min(b), a.max(b)));
            }
            if spans.is_empty() {
                continue;
            }
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut merged: Vec<(f32, f32)> = Vec::new();
            for (lo, hi) in spans {
                match merged.last_mut() {
                    Some(last) if lo <= last.1 + 1e-3 => last.1 = last.1.max(hi),
                    _ => merged.push((lo, hi)),
                }
            }
            for (lo, hi) in merged {
                out.push(Rect {
                    x: lo,
                    y: line.top,
                    width: hi - lo,
                    height: line.bottom - line.top,
                });
            }
        }
        out
    }
}
