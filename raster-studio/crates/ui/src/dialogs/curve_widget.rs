//! The Curves editor: a square graph the curve is drawn and edited on.
//!
//! Before this widget the Curves dialog was five output sliders at fixed
//! inputs — no graph, no channel choice, nothing to read the curve against.
//! This is Photopea's editor in the shape the rest of the dialogs use:
//!
//! * a channel dropdown (RGB, Red, Green, Blue — or, W8-B, Lightness, a, b
//!   on a Lab document, Photoshop's Lab list, no composite) choosing which of
//!   [`layer_model::AdjustmentKind::CurvesFull`]'s four curves is edited;
//! * the histogram of that channel behind the graph (luma for RGB), so a point
//!   is placed against the tones it moves;
//! * the identity diagonal, and the curve itself sampled through the same
//!   [`adjustments::Curve`] the renderer evaluates;
//! * a press on the graph away from every point adds one there, a press on a
//!   point grabs it, a drag moves it (never past its neighbours, so the curve
//!   stays a function), and dragging an inner point off the graph removes it.
//!
//! The widget edits the point lists in place and reports whether anything
//! moved; the dialog turns that into a new kind, and its existing live
//! preview re-runs the real adjustment.

use adjustments::{Curve, HISTOGRAM_BINS};
use design::tokens::palette::ColorRole;
use design::tokens::{grid, Radius};
use design::{color32, current_tokens, egui_theme::rounding};
use egui::{pos2, Rect, Sense};

use crate::dialogs::controls::combo;
use crate::dialogs::sizes;
use crate::strings::tr;

/// The four curves a `CurvesFull` carries, in its field order.
pub const CHANNELS: [usize; 4] = [0, 1, 2, 3];

/// The graph's stable id, so a test can find and drive it.
pub fn curve_graph_id() -> egui::Id {
    egui::Id::new(("raster-dialogs", "curve-graph"))
}

/// The smallest distance, in input units, two neighbouring points keep. A
/// point dragged onto its neighbour's x would make the curve a relation.
const MIN_GAP: f32 = 1.0 / 255.0;

/// How finely the drawn curve is sampled.
const SAMPLES: usize = 96;

/// Editor state that outlives a frame: which channel is shown and which
/// point, if any, the pointer is holding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CurveEditor {
    channel: usize,
    dragging: Option<usize>,
    /// W8-B: the document is in Lab mode, so the channels listed are
    /// Lightness, a and b (the red, green and blue slots).
    lab: bool,
}

impl CurveEditor {
    /// The channel shown: `0` RGB, then red, green, blue.
    pub fn channel(&self) -> usize {
        self.channel
    }

    /// Show `channel` (clamped to the four there are; on a Lab document,
    /// which has no composite row, to Lightness, a or b).
    pub fn set_channel(&mut self, channel: usize) {
        let lowest = usize::from(self.lab);
        self.channel = channel.clamp(lowest, 3);
        self.dragging = None;
    }

    /// Whether a point is held.
    pub fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// W8-B: list the channels as a Lab document's (Lightness, a, b), and
    /// show the first of them: Lightness on a Lab document, the composite
    /// otherwise.
    pub fn set_lab(&mut self, lab: bool) {
        self.lab = lab;
        self.set_channel(0);
    }

    /// Whether the channels are a Lab document's.
    pub fn is_lab(&self) -> bool {
        self.lab
    }
}

/// The histograms drawn behind each channel: luma, red, green, blue.
pub type ChannelHistograms = [[u32; HISTOGRAM_BINS]; 4];

/// The label a channel is listed under.
pub fn channel_label(channel: usize) -> String {
    tr(match channel {
        1 => "ui.adjustment.red",
        2 => "ui.adjustment.green",
        3 => "ui.adjustment.blue",
        _ => "ui.adjustment.curve.rgb",
    })
    .to_string()
}

/// W8-B: the channels a Lab document lists — Lightness, a and b in the
/// red, green and blue slots. Photoshop's Lab Levels and Curves have no
/// composite row: one run on a and b too would cast every neutral.
pub const LAB_CHANNELS: [usize; 3] = [1, 2, 3];

/// W8-B: the label a channel is listed under on a Lab document: Lightness,
/// a and b (Photoshop's channel names; a and b are the CIELAB axis names and
/// are not translated).
pub fn lab_channel_label(channel: usize) -> String {
    match channel {
        2 => "a".to_string(),
        3 => "b".to_string(),
        _ => tr("ui.adjustment.lightness").to_string(),
    }
}

/// A clean, sorted point list: the identity when `points` cannot make a
/// curve, otherwise the merged knots [`Curve`] itself keeps.
pub fn normalized(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    Curve::new(points)
        .unwrap_or_else(|_| Curve::identity())
        .points()
        .into_iter()
        .map(|[x, y]| [x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)])
        .collect()
}

/// Draw the channel dropdown and the graph for one frame, editing `curves`
/// (composite, red, green, blue) in place. Returns whether a point moved, was
/// added or was removed.
pub fn show(
    ui: &mut egui::Ui,
    editor: &mut CurveEditor,
    curves: [&mut Vec<[f32; 2]>; 4],
    histograms: Option<&ChannelHistograms>,
) -> bool {
    design::inspector_field(ui, tr("ui.adjustment.curve.channel"), |ui| {
        let mut channel = editor.channel;
        let lab = editor.lab;
        let label = |c: usize| {
            if lab {
                lab_channel_label(c)
            } else {
                channel_label(c)
            }
        };
        let options: &[usize] = if lab { &LAB_CHANNELS } else { &CHANNELS };
        if combo(
            ui,
            ("adjustment", "curve-channel"),
            &mut channel,
            options,
            label,
            |_| None,
        ) {
            editor.set_channel(channel);
        }
    });
    let [composite, red, green, blue] = curves;
    let points: &mut Vec<[f32; 2]> = match editor.channel {
        1 => red,
        2 => green,
        3 => blue,
        _ => composite,
    };
    let side = sizes::filter_preview_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), Sense::hover());
    let response = ui.interact(rect, curve_graph_id(), Sense::click_and_drag());
    let changed = interact(ui, &response, rect, editor, points);
    paint(
        ui,
        rect,
        editor,
        points,
        histograms.map(|h| &h[editor.channel.min(3)]),
    );
    crate::dialogs::chrome::caption(ui, tr("ui.adjustment.curve.hint"));
    changed
}

/// Graph space (input right, output up, both `0..=1`) of a screen point.
fn to_graph(rect: Rect, pos: egui::Pos2) -> [f32; 2] {
    [
        ((pos.x - rect.left()) / rect.width().max(f32::EPSILON)).clamp(0.0, 1.0),
        ((rect.bottom() - pos.y) / rect.height().max(f32::EPSILON)).clamp(0.0, 1.0),
    ]
}

/// Screen position of a graph point.
fn to_screen(rect: Rect, p: [f32; 2]) -> egui::Pos2 {
    pos2(
        rect.left() + rect.width() * p[0],
        rect.bottom() - rect.height() * p[1],
    )
}

/// The press / drag / release logic. Pure over the pointer state egui
/// reports, so a press and release in one frame (a click) still adds a point.
fn interact(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: Rect,
    editor: &mut CurveEditor,
    points: &mut Vec<[f32; 2]>,
) -> bool {
    let (pressed, down, released, pos) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.pointer.interact_pos(),
        )
    });
    let Some(pos) = pos else {
        if released {
            editor.dragging = None;
        }
        return false;
    };
    let reach = current_tokens(ui).metrics.min_hit_target;
    let mut changed = false;
    if pressed && response.contains_pointer() && rect.contains(pos) {
        *points = normalized(points);
        let nearest = points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, to_screen(rect, *p).distance(pos)))
            .filter(|(_, d)| *d <= reach * 0.5)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        editor.dragging = match nearest {
            Some(i) => Some(i),
            None => {
                let [x, y] = to_graph(rect, pos);
                let at = points.iter().position(|p| p[0] > x).unwrap_or(points.len());
                let clear_left = at == 0 || x - points[at - 1][0] >= MIN_GAP;
                let clear_right = at == points.len() || points[at][0] - x >= MIN_GAP;
                if clear_left && clear_right {
                    points.insert(at, [x, y]);
                    changed = true;
                    Some(at)
                } else {
                    None
                }
            }
        };
    }
    if let Some(index) = editor.dragging.filter(|i| *i < points.len()) {
        if down || pressed || released {
            let n = points.len();
            let inner = index > 0 && index + 1 < n;
            if inner && !rect.expand(reach).contains(pos) {
                // Dragged off the graph: the point goes, as in Photopea.
                points.remove(index);
                editor.dragging = None;
                return true;
            }
            let [x, y] = to_graph(rect, pos);
            let lo = if index == 0 {
                0.0
            } else {
                points[index - 1][0] + MIN_GAP
            };
            let hi = if index + 1 == n {
                1.0
            } else {
                points[index + 1][0] - MIN_GAP
            };
            let next = [x.clamp(lo.min(hi), hi.max(lo)), y];
            if points[index] != next {
                points[index] = next;
                changed = true;
            }
        }
    }
    if released || (!down && !pressed) {
        editor.dragging = None;
    }
    changed
}

fn channel_role(channel: usize) -> ColorRole {
    match channel {
        1 => ColorRole::ChannelRed,
        2 => ColorRole::ChannelGreen,
        3 => ColorRole::ChannelBlue,
        _ => ColorRole::TextPrimary,
    }
}

fn paint(
    ui: &egui::Ui,
    rect: Rect,
    editor: &CurveEditor,
    points: &[[f32; 2]],
    histogram: Option<&[u32; HISTOGRAM_BINS]>,
) {
    let t = current_tokens(ui);
    let painter = ui.painter_at(rect.expand(t.borders.thick));
    let radius = Radius::Small.resolve(&t.radii, rect.height());
    painter.rect_filled(
        rect,
        rounding(radius),
        color32(t.palette.color(ColorRole::SurfaceSunken)),
    );
    if let Some(bins) = histogram {
        let peak = bins.iter().copied().max().unwrap_or(0).max(1) as f32;
        let bar = rect.width() / HISTOGRAM_BINS as f32;
        let fill = color32(t.palette.color(ColorRole::TextTertiary));
        let flat = rounding(Radius::None.resolve(&t.radii, bar));
        for (index, count) in bins.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            let height = rect.height() * (*count as f32 / peak);
            let x0 = rect.left() + index as f32 * bar;
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x0, rect.bottom() - height),
                    pos2(x0 + bar, rect.bottom()),
                ),
                flat,
                fill,
            );
        }
    }
    let rule = egui::Stroke::new(
        t.borders.hairline,
        color32(t.palette.color(ColorRole::SeparatorHairline)),
    );
    for quarter in 1..4 {
        let f = quarter as f32 / 4.0;
        painter.vline(rect.left() + rect.width() * f, rect.y_range(), rule);
        painter.hline(rect.x_range(), rect.top() + rect.height() * f, rule);
    }
    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::ControlStroke)),
        ),
    );
    let curve = Curve::new(points).unwrap_or_else(|_| Curve::identity());
    // W8-B: Lab's channels have no hue of their own to draw in.
    let role = if editor.lab {
        ColorRole::TextPrimary
    } else {
        channel_role(editor.channel)
    };
    let colour = color32(t.palette.color(role));
    let line: Vec<egui::Pos2> = (0..=SAMPLES)
        .map(|k| {
            let x = k as f32 / SAMPLES as f32;
            to_screen(rect, [x, curve.eval(x).clamp(0.0, 1.0)])
        })
        .collect();
    painter.add(egui::Shape::line(
        line,
        egui::Stroke::new(t.borders.thick, colour),
    ));
    let knob = grid(1.0);
    for (index, point) in points.iter().enumerate() {
        let at = to_screen(rect, *point);
        let held = editor.dragging == Some(index);
        painter.circle(
            at,
            knob,
            if held {
                colour
            } else {
                color32(t.palette.color(ColorRole::SurfaceSunken))
            },
            egui::Stroke::new(t.borders.thick, colour),
        );
    }
    painter.rect_stroke(
        rect,
        rounding(radius),
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::ControlStroke)),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0))
    }

    #[test]
    fn graph_space_is_input_right_and_output_up() {
        assert_eq!(to_graph(rect(), pos2(0.0, 100.0)), [0.0, 0.0]);
        assert_eq!(to_graph(rect(), pos2(100.0, 0.0)), [1.0, 1.0]);
        assert_eq!(to_screen(rect(), [0.25, 0.75]), pos2(25.0, 25.0));
    }

    #[test]
    fn a_broken_point_list_normalizes_to_the_identity() {
        assert_eq!(normalized(&[]), vec![[0.0, 0.0], [1.0, 1.0]]);
        assert_eq!(
            normalized(&[[1.0, 0.8], [0.0, 0.1]]),
            vec![[0.0, 0.1], [1.0, 0.8]]
        );
    }

    #[test]
    fn the_channel_is_clamped_and_drops_the_held_point() {
        let mut editor = CurveEditor {
            channel: 0,
            lab: false,
            dragging: Some(1),
        };
        editor.set_channel(9);
        assert_eq!(editor.channel(), 3);
        assert!(!editor.is_dragging());
    }
}
