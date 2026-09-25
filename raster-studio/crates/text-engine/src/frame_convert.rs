//! W13-N: Layer ▸ Text ▸ Convert to Point Text / Convert to Paragraph Text.
//!
//! Photoshop's (and Photopea's) two conversions, as pure edits of a
//! [`TextLayer`]:
//!
//! * **Paragraph → point** ([`to_point_text`]): the box goes away and every
//!   place a line *wrapped* becomes a hard line break, so the text keeps the
//!   lines it had. A wrap that fell after a space turns that space into the
//!   break (the byte length is unchanged); a wrap inside a word inserts one,
//!   and every style span and kerning index after it moves over by one byte.
//!   Unlike Photoshop, text past the bottom of a fixed-height box is kept,
//!   not deleted — it becomes more lines of point text.
//! * **Point → paragraph** ([`to_paragraph_text`]): the text gets a box the
//!   size of its laid-out lines, so nothing re-wraps.
//!
//! Where the lines wrapped is layout's answer, not this module's:
//! [`soft_wrap_starts`] reads it off a [`ShapedText`], and a caller that
//! shapes elsewhere (the compositor's library) can pass the byte starts it
//! found there.

use layer_model::text::Frame;
use layer_model::TextLayer;

use crate::ShapedText;

/// The byte index each *wrapped* visual line starts at — every line start
/// that is not the start of a paragraph — ascending.
#[must_use]
pub fn soft_wrap_starts(shaped: &ShapedText) -> Vec<usize> {
    wrap_breaks(&shaped.text, shaped.lines.iter().map(|l| l.byte_start))
}

/// `starts` narrowed to the soft wraps of `text`: inside the string, on a
/// character boundary, and not right after a line break; sorted, once each.
#[must_use]
pub fn wrap_breaks(text: &str, starts: impl IntoIterator<Item = usize>) -> Vec<usize> {
    let mut out: Vec<usize> = starts
        .into_iter()
        .filter(|&b| b > 0 && b < text.len() && text.is_char_boundary(b))
        .filter(|&b| !matches!(text.as_bytes()[b - 1], b'\n' | b'\r'))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Paragraph text to point text: a hard break at each of `soft_breaks`
/// (from [`soft_wrap_starts`] or [`wrap_breaks`]), and no box.
#[must_use]
pub fn to_point_text(layer: &TextLayer, soft_breaks: &[usize]) -> TextLayer {
    let mut out = layer.clone();
    out.frame = Frame::Point;
    let breaks = wrap_breaks(&layer.text, soft_breaks.iter().copied());
    // Right to left, so an insertion never moves a break still to do.
    for &b in breaks.iter().rev() {
        if out.text.as_bytes()[b - 1] == b' ' {
            out.text.replace_range(b - 1..b, "\n");
            continue;
        }
        out.text.insert(b, '\n');
        for span in &mut out.spans {
            if span.start >= b {
                span.start += 1;
            }
            if span.end > b {
                span.end += 1;
            }
        }
        for kern in &mut out.kerning {
            if kern.index >= b {
                kern.index += 1;
            }
        }
    }
    out
}

/// Point text to paragraph text: a box `width x height` layer pixels (the
/// laid-out lines' extent), rounded up so the lines do not re-wrap.
#[must_use]
pub fn to_paragraph_text(layer: &TextLayer, width: f32, height: f32) -> TextLayer {
    let mut out = layer.clone();
    let size = |v: f32| {
        if v.is_finite() {
            v.max(1.0).ceil() + 1.0
        } else {
            1.0
        }
    };
    out.frame = Frame::Box {
        width: size(width),
        height: Some(size(height)),
    };
    out
}

/// Whether `layer` is paragraph (box) text.
#[must_use]
pub fn is_paragraph_text(layer: &TextLayer) -> bool {
    matches!(layer.frame, Frame::Box { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{shape, FontLibrary, TextRun};
    use layer_model::text::StyleSpan;

    fn library() -> FontLibrary {
        let mut library = FontLibrary::empty();
        library.load_bytes(dejavu::sans::regular().to_vec());
        library
    }

    fn boxed(text: &str, width: f32) -> TextLayer {
        let mut layer = TextLayer::legacy(text, "DejaVu Sans", 20.0);
        layer.frame = Frame::Box {
            width,
            height: None,
        };
        layer
    }

    /// The text of each visual line, trailing white space dropped.
    fn lines(library: &mut FontLibrary, layer: &TextLayer) -> Vec<String> {
        let shaped = shape(library, &TextRun::from(layer));
        shaped
            .lines
            .iter()
            .map(|l| shaped.text[l.byte_start..l.byte_end].trim_end().to_string())
            .collect()
    }

    #[test]
    fn paragraph_to_point_keeps_every_line_as_it_wrapped() {
        let mut library = library();
        let layer = boxed("The quick brown fox jumps over the lazy dog again", 120.0);
        let before = lines(&mut library, &layer);
        assert!(before.len() >= 3, "the box wraps: {before:?}");
        let shaped = shape(&mut library, &TextRun::from(&layer));
        let point = to_point_text(&layer, &soft_wrap_starts(&shaped));
        assert_eq!(point.frame, Frame::Point);
        assert_eq!(lines(&mut library, &point), before);
        assert_eq!(point.text.matches('\n').count(), before.len() - 1);
    }

    #[test]
    fn a_break_inside_a_word_inserts_one_and_moves_the_spans_after_it() {
        let mut layer = boxed("abcdef", 10.0);
        layer.spans.push(StyleSpan {
            start: 4,
            end: 6,
            style: Default::default(),
        });
        let point = to_point_text(&layer, &[3]);
        assert_eq!(point.text, "abc\ndef");
        assert_eq!((point.spans[0].start, point.spans[0].end), (5, 7));
    }

    #[test]
    fn point_to_paragraph_boxes_the_lines_without_rewrapping() {
        let mut library = library();
        let point = TextLayer::legacy("First line\nA second, longer line", "DejaVu Sans", 20.0);
        let before = lines(&mut library, &point);
        let shaped = shape(&mut library, &TextRun::from(&point));
        let b = shaped.bounds;
        let para = to_paragraph_text(&point, b.x + b.width, b.y + b.height);
        assert!(is_paragraph_text(&para));
        assert_eq!(lines(&mut library, &para), before);
    }

    #[test]
    fn paragraph_starts_are_not_wraps() {
        assert_eq!(wrap_breaks("ab\ncd ef", [0, 3, 6, 99]), vec![6]);
    }
}
