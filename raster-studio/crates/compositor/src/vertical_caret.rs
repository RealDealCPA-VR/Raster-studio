//! W8-C: caret, hit-test and selection geometry for vertical type.
//!
//! `text_engine` lays a vertical run out by re-positioning its horizontally
//! shaped glyphs into upright em cells stacked down a column per paragraph
//! (`verticalize`), but its caret maths (`ShapedText::caret_rect`,
//! `hit_test`, `selection_rects`) still walks the glyphs along x. This module
//! is the column-wise counterpart the text wrappers in [`crate::text`] use
//! when the run is vertical: caret stops go **down** a column (one at the top
//! of each cluster's cell, subdivided evenly for a multi-character cluster,
//! and one at the bottom of the last cell), the caret is a horizontal bar
//! across the column, a click picks the nearest column and then the nearest
//! stop down it, and a selection is one rect per column.
//!
//! The cells are rebuilt exactly as `verticalize` builds them: the first at
//! the column's top, each next one the previous cell's glyph size further
//! down, marks sharing their base's cell.

use text_engine::{Rect, ShapedLine, ShapedText};

/// One caret stop down a column: byte index and y.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stop {
    index: usize,
    y: f32,
}

/// Each cluster's cell on `line`: `(cluster_start, cluster_end, top, size)`,
/// top to bottom.
fn cells(shaped: &ShapedText, line: &ShapedLine) -> Vec<(usize, usize, f32, f32)> {
    let mut glyphs: Vec<_> = shaped.glyphs[line.glyph_range()].iter().collect();
    glyphs.sort_by_key(|g| g.cluster_start);
    let mut out: Vec<(usize, usize, f32, f32)> = Vec::new();
    let mut cursor = line.top;
    for g in glyphs {
        if out.last().is_some_and(|c| c.0 == g.cluster_start) {
            continue;
        }
        let size = if g.size_px.is_finite() && g.size_px > 0.0 {
            g.size_px
        } else {
            shaped.base_size_px
        };
        out.push((g.cluster_start, g.cluster_end, cursor, size));
        cursor += size;
    }
    out
}

/// The caret stops down column `line`, top to bottom.
fn stops(shaped: &ShapedText, line: &ShapedLine) -> Vec<Stop> {
    let mut out = Vec::new();
    let cells = cells(shaped, line);
    for &(start, end, top, size) in &cells {
        let cluster = shaped.text.get(start..end).unwrap_or("");
        let count = cluster.chars().count().max(1) as f32;
        for (ordinal, (offset, _)) in cluster.char_indices().enumerate() {
            out.push(Stop {
                index: start + offset,
                y: top + size * ordinal as f32 / count,
            });
        }
    }
    match cells.last() {
        Some(&(_, end, top, size)) => out.push(Stop {
            index: end.max(line.byte_start).min(line.byte_end),
            y: top + size,
        }),
        None => out.push(Stop {
            index: line.byte_start,
            y: line.top,
        }),
    }
    out
}

/// The column (line) that owns byte `index`.
fn column_of(shaped: &ShapedText, index: usize) -> Option<&ShapedLine> {
    shaped
        .lines
        .iter()
        .find(|l| index >= l.byte_start && index <= l.byte_end)
        .or_else(|| shaped.lines.last())
}

/// The caret for byte `index`: a zero-height bar across its column, at the
/// top of the cell the index starts (or the bottom of the last cell).
pub(crate) fn caret_rect(shaped: &ShapedText, index: usize) -> Rect {
    let Some(line) = column_of(shaped, index) else {
        return Rect::default();
    };
    let stops = stops(shaped, line);
    let y = stops
        .iter()
        .find(|s| s.index == index)
        .or_else(|| {
            stops
                .iter()
                .filter(|s| s.index <= index)
                .max_by_key(|s| s.index)
        })
        .map_or(line.top, |s| s.y);
    Rect {
        x: line.x_min,
        y,
        width: line.x_max - line.x_min,
        height: 0.0,
    }
}

/// The byte index nearest layer-local `(x, y)`: the column whose band is
/// nearest `x`, then the stop down it nearest `y`.
pub(crate) fn hit_test(shaped: &ShapedText, x: f32, y: f32) -> usize {
    let Some(line) = shaped.lines.iter().min_by(|a, b| {
        let d = |l: &ShapedLine| {
            if x < l.x_min {
                l.x_min - x
            } else if x > l.x_max {
                x - l.x_max
            } else {
                0.0
            }
        };
        d(a).total_cmp(&d(b))
    }) else {
        return 0;
    };
    let mut best = line.byte_start;
    let mut best_distance = f32::INFINITY;
    for stop in stops(shaped, line) {
        let distance = (stop.y - y).abs();
        if distance < best_distance - 1e-6
            || ((distance - best_distance).abs() <= 1e-6 && stop.index < best)
        {
            best_distance = distance;
            best = stop.index;
        }
    }
    best
}

/// One rect per column covering the byte range `start..end`.
pub(crate) fn selection_rects(shaped: &ShapedText, start: usize, end: usize) -> Vec<Rect> {
    if start >= end {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in &shaped.lines {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for (cs, ce, top, size) in cells(shaped, line) {
            let (a, b) = (cs.max(start), ce.min(end));
            if a >= b {
                continue;
            }
            let cluster = shaped.text.get(cs..ce).unwrap_or("");
            let count = cluster.chars().count().max(1) as f32;
            let before = |index: usize| {
                cluster
                    .char_indices()
                    .take_while(|(offset, _)| cs + offset < index)
                    .count() as f32
            };
            let (from, to) = (before(a), before(b).max(before(a) + 1.0));
            lo = lo.min(top + size * from / count);
            hi = hi.max(top + size * to / count);
        }
        if lo < hi {
            out.push(Rect {
                x: line.x_min,
                y: lo,
                width: line.x_max - line.x_min,
                height: hi - lo,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::testkit::text_fixture_family;

    fn run(text: &str, vertical: bool) -> text_engine::TextRun {
        let layer = layer_model::TextLayer {
            text: text.into(),
            font_family: text_fixture_family().into(),
            size_px: 20.0,
            paragraph: layer_model::text::Paragraph {
                vertical,
                ..Default::default()
            },
            ..Default::default()
        };
        text_engine::TextRun::from(&layer)
    }

    #[test]
    fn a_vertical_caret_is_a_bar_across_the_column_that_walks_down_it() {
        let r = run("ABC", true);
        let c0 = crate::text_caret_rect(&r, 0);
        let c1 = crate::text_caret_rect(&r, 1);
        let c3 = crate::text_caret_rect(&r, 3);
        assert_eq!(c0.height, 0.0, "a bar across the column: {c0:?}");
        assert!(c0.width > 10.0, "{c0:?}");
        assert!(
            c1.y > c0.y + 10.0 && c3.y > c1.y + 10.0,
            "the caret walks DOWN: {c0:?} {c1:?} {c3:?}"
        );
        assert_eq!(c0.x, c1.x, "the same column");
        // Horizontal type is untouched: a vertical bar that walks right.
        let h = run("ABC", false);
        let (h0, h1) = (crate::text_caret_rect(&h, 0), crate::text_caret_rect(&h, 1));
        assert_eq!(h0.width, 0.0);
        assert!(h1.x > h0.x);
    }

    #[test]
    fn a_click_down_a_vertical_column_lands_on_the_stop_under_it() {
        let r = run("ABCD", true);
        let c2 = crate::text_caret_rect(&r, 2);
        let x = c2.x + c2.width / 2.0;
        // Just below the top of the third cell: caret 2.
        assert_eq!(crate::text_hit_index(&r, x, c2.y + 2.0), Some(2));
        // The same point in x but at the column top: caret 0.
        let c0 = crate::text_caret_rect(&r, 0);
        assert_eq!(crate::text_hit_index(&r, x, c0.y + 1.0), Some(0));
        // The selection of "BC" is one rect down the column.
        let sel = crate::text_selection_rects(&r, 1, 3);
        assert_eq!(sel.len(), 1, "{sel:?}");
        let c1 = crate::text_caret_rect(&r, 1);
        let c3 = crate::text_caret_rect(&r, 3);
        assert!((sel[0].y - c1.y).abs() < 1e-3 && (sel[0].y + sel[0].height - c3.y).abs() < 1e-3);
    }
}
