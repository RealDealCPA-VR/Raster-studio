//! W13-N: the Guide Guy panel (Photopea's Window ▸ Guide Guy).
//!
//! Margins, columns and rows with the gutters between them, and a guide
//! through the canvas centre on either axis — previewed on a thumbnail of
//! the canvas in the panel and applied as the document's guides with one
//! click, as ONE [`Command::SetGuides`] (one undo step). The layout
//! arithmetic is the New Guide Layout dialog's own
//! ([`GuideLayoutSpec::guides`]), so the panel and the dialog can never
//! disagree about where a column edge falls.
//!
//! The settings live in the panel (egui memory under [`ids::spec`]), not in
//! the document: they are a tool, and the guides they make are what is
//! saved.

use design::{current_tokens, ColorRole, Space};
use editor_core::{Command, Document, Guide, GuideAxis, Guides};
use egui::Ui;

use crate::dialogs::controls::{checkbox_row, integer, numeric};
use crate::dialogs::new_guide_layout::{Divisions, GuideLayoutSpec, Margins, MAX_DIVISIONS};
use crate::intent::Intent;
use crate::strings::tr;
use crate::view::{empty_state, hairline, hint, labelled_button};
use crate::Workspace;

/// Stable ids for a headless test.
pub mod ids {
    /// Where the panel keeps its [`super::GuideGuySpec`].
    pub fn spec() -> egui::Id {
        egui::Id::new("raster-guide-guy-spec")
    }
    /// The preview thumbnail.
    pub fn preview() -> egui::Id {
        egui::Id::new("raster-guide-guy-preview")
    }
    /// The Apply button.
    pub fn apply() -> egui::Id {
        egui::Id::new("raster-guide-guy-apply")
    }
}

/// Everything the panel asks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GuideGuySpec {
    /// Columns, rows and margins, exactly as New Guide Layout reads them.
    pub layout: GuideLayoutSpec,
    /// A vertical guide through the canvas centre.
    pub center_vertical: bool,
    /// A horizontal guide through the canvas centre.
    pub center_horizontal: bool,
}

impl Default for GuideGuySpec {
    /// Photopea's opening state: 20 px margins, three columns and no rows
    /// with 20 px gutters, both centre guides, existing guides replaced.
    fn default() -> Self {
        Self {
            layout: GuideLayoutSpec {
                columns: Divisions {
                    enabled: true,
                    count: 3,
                    gutter: 20.0,
                },
                rows: Divisions {
                    enabled: false,
                    count: 3,
                    gutter: 20.0,
                },
                margins: Margins {
                    enabled: true,
                    top: 20.0,
                    left: 20.0,
                    bottom: 20.0,
                    right: 20.0,
                },
                clear_existing: true,
            },
            center_vertical: true,
            center_horizontal: true,
        }
    }
}

impl GuideGuySpec {
    /// The guides this spec lays out on a `canvas`, centre guides included,
    /// each placed once. `None` when the margins or gutters leave no room.
    pub fn new_guides(&self, canvas: (u32, u32)) -> Option<Vec<Guide>> {
        let mut out = self.layout.guides(canvas)?;
        let centres = [
            (self.center_vertical, GuideAxis::Vertical, canvas.0),
            (self.center_horizontal, GuideAxis::Horizontal, canvas.1),
        ];
        for (on, axis, extent) in centres {
            let doc = extent as f32 / 2.0;
            if on
                && !out
                    .iter()
                    .any(|g| g.axis == axis && (g.doc - doc).abs() < 1e-3)
            {
                out.push(Guide {
                    axis,
                    doc,
                    locked: false,
                });
            }
        }
        Some(out)
    }

    /// The document's guide set once this spec is applied over `current`:
    /// in place of it when [`GuideLayoutSpec::clear_existing`] is set,
    /// otherwise added to it without repeating a guide already there.
    pub fn applied(&self, current: &Guides, canvas: (u32, u32)) -> Option<Guides> {
        let fresh = self.new_guides(canvas)?;
        let mut guides = current.clone();
        if self.layout.clear_existing {
            guides.list.clear();
        }
        for g in fresh {
            if !guides
                .list
                .iter()
                .any(|o| o.axis == g.axis && (o.doc - g.doc).abs() < 1e-3)
            {
                guides.list.push(g);
            }
        }
        Some(guides)
    }

    /// The command Apply emits over `doc`.
    pub fn command(&self, doc: &Document) -> Option<Command> {
        let guides = self.applied(&doc.guides, (doc.width(), doc.height()))?;
        (guides != doc.guides).then_some(Command::SetGuides { guides })
    }
}

/// The spec the panel is showing on `ctx`.
pub fn spec(ctx: &egui::Context) -> GuideGuySpec {
    ctx.data(|d| d.get_temp::<GuideGuySpec>(ids::spec()))
        .unwrap_or_default()
}

/// Put `spec` in the panel on `ctx`.
pub fn set_spec(ctx: &egui::Context, spec: GuideGuySpec) {
    ctx.data_mut(|d| d.insert_temp(ids::spec(), spec));
}

const NO_DOCUMENT: &str = "ui.guide_guy.no_document";
const MARGINS: &str = "ui.guide_guy.margins";
const TOP: &str = "ui.guide_guy.top";
const LEFT: &str = "ui.guide_guy.left";
const BOTTOM: &str = "ui.guide_guy.bottom";
const RIGHT: &str = "ui.guide_guy.right";
const COLUMNS: &str = "ui.guide_guy.columns";
const ROWS: &str = "ui.guide_guy.rows";
const GUTTER: &str = "ui.guide_guy.gutter";
const CENTER_V: &str = "ui.guide_guy.center_vertical";
const CENTER_H: &str = "ui.guide_guy.center_horizontal";
const REPLACE: &str = "ui.guide_guy.replace";
const APPLY: &str = "ui.guide_guy.apply";
const NO_ROOM: &str = "ui.guide_guy.no_room";

/// Draw the panel.
pub(crate) fn guide_guy_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let mut s = spec(ui.ctx());
    let px = tr("ui.selection_modify.px");
    let big = f64::from(doc.width().max(doc.height()));

    checkbox_row(ui, tr(MARGINS), &mut s.layout.margins.enabled);
    ui.add_enabled_ui(s.layout.margins.enabled, |ui| {
        egui::Grid::new("raster-guide-guy-margins")
            .num_columns(4)
            .show(ui, |ui| {
                let m = &mut s.layout.margins;
                ui.label(hint(ui, tr(TOP)));
                numeric(ui, &mut m.top, 0.0..=big, 1, px);
                ui.label(hint(ui, tr(LEFT)));
                numeric(ui, &mut m.left, 0.0..=big, 1, px);
                ui.end_row();
                ui.label(hint(ui, tr(BOTTOM)));
                numeric(ui, &mut m.bottom, 0.0..=big, 1, px);
                ui.label(hint(ui, tr(RIGHT)));
                numeric(ui, &mut m.right, 0.0..=big, 1, px);
                ui.end_row();
            });
    });
    for (label, div) in [(COLUMNS, &mut s.layout.columns), (ROWS, &mut s.layout.rows)] {
        ui.horizontal(|ui| {
            checkbox_row(ui, tr(label), &mut div.enabled);
            ui.add_enabled_ui(div.enabled, |ui| {
                integer(ui, &mut div.count, 1..=MAX_DIVISIONS);
                ui.label(hint(ui, tr(GUTTER)));
                numeric(ui, &mut div.gutter, 0.0..=big, 1, px);
            });
        });
    }
    checkbox_row(ui, tr(CENTER_V), &mut s.center_vertical);
    checkbox_row(ui, tr(CENTER_H), &mut s.center_horizontal);
    checkbox_row(ui, tr(REPLACE), &mut s.layout.clear_existing);
    set_spec(ui.ctx(), s);

    ui.add_space(Space::XSmall.pt());
    let canvas = (doc.width(), doc.height());
    let proposed = s.new_guides(canvas);
    preview(ui, doc, proposed.as_deref());
    if proposed.is_none() {
        ui.label(hint(ui, tr(NO_ROOM)));
    }
    hairline(ui);
    let command = s.command(doc);
    if labelled_button(ui, tr(APPLY), command.is_some(), ids::apply()).clicked() {
        if let Some(command) = command {
            w.emit(Intent::Document(command));
        }
    }
}

/// The canvas as a thumbnail with the document's guides faint and the
/// proposed ones in the accent.
fn preview(ui: &mut Ui, doc: &Document, proposed: Option<&[Guide]>) {
    let t = current_tokens(ui);
    let side = (ui.available_width() - 2.0 * Space::Small.pt()).max(t.metrics.control_height);
    let (w, h) = (doc.width() as f32, doc.height() as f32);
    let scale = side / w.max(h);
    let size = egui::vec2(w * scale, h * scale);
    let (outer, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), size.y + 2.0 * Space::Small.pt()),
        egui::Sense::hover(),
    );
    let rect = egui::Rect::from_center_size(outer.center(), size);
    let _ = ui.interact(rect, ids::preview(), egui::Sense::hover());
    let p = ui.painter_at(outer);
    let c = |role| design::color32(t.palette.color(role));
    p.rect_filled(rect, 0.0, c(ColorRole::SurfaceSunken));
    p.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(t.borders.hairline, c(ColorRole::SeparatorStrong)),
    );
    let line = |g: &Guide, stroke: egui::Stroke| {
        let v = g.doc * scale;
        match g.axis {
            GuideAxis::Vertical => p.line_segment(
                [
                    egui::pos2(rect.left() + v, rect.top()),
                    egui::pos2(rect.left() + v, rect.bottom()),
                ],
                stroke,
            ),
            GuideAxis::Horizontal => p.line_segment(
                [
                    egui::pos2(rect.left(), rect.top() + v),
                    egui::pos2(rect.right(), rect.top() + v),
                ],
                stroke,
            ),
        }
    };
    let old = egui::Stroke::new(t.borders.hairline, c(ColorRole::TextTertiary));
    for g in &doc.guides.list {
        line(g, old);
    }
    let new = egui::Stroke::new(t.borders.hairline, c(ColorRole::Accent));
    for g in proposed.unwrap_or_default() {
        line(g, new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [
            NO_DOCUMENT,
            MARGINS,
            TOP,
            LEFT,
            BOTTOM,
            RIGHT,
            COLUMNS,
            ROWS,
            GUTTER,
            CENTER_V,
            CENTER_H,
            REPLACE,
            APPLY,
            NO_ROOM,
        ] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn the_default_lays_out_margins_three_columns_and_both_centres() {
        let spec = GuideGuySpec::default();
        let guides = spec.new_guides((1000, 500)).unwrap();
        let at = |axis| {
            let mut v: Vec<f32> = guides
                .iter()
                .filter(|g| g.axis == axis)
                .map(|g| g.doc)
                .collect();
            v.sort_by(f32::total_cmp);
            v
        };
        // Margins 20 / 980; three columns of (960 - 40) / 3 with 20 px
        // gutters; the centre at 500.
        let cols = (960.0 - 40.0) / 3.0;
        let expected = [
            20.0,
            20.0 + cols,
            40.0 + cols,
            500.0,
            40.0 + 2.0 * cols,
            60.0 + 2.0 * cols,
            980.0,
        ];
        let got = at(GuideAxis::Vertical);
        assert_eq!(got.len(), expected.len(), "{got:?}");
        for (g, e) in got.iter().zip(expected) {
            assert!((g - e).abs() < 1e-3, "{got:?}");
        }
        assert_eq!(at(GuideAxis::Horizontal), vec![20.0, 250.0, 480.0]);
    }

    #[test]
    fn apply_replaces_or_adds_to_the_existing_guides_as_one_command() {
        let mut doc = Document::new(100, 100, "g");
        doc.guides.list.push(Guide {
            axis: GuideAxis::Horizontal,
            doc: 7.0,
            locked: false,
        });
        let mut spec = GuideGuySpec::default();
        spec.layout.columns.enabled = false;
        spec.layout.margins.enabled = false;
        spec.center_horizontal = false;
        let Some(Command::SetGuides { guides }) = spec.command(&doc) else {
            panic!("apply emits SetGuides");
        };
        assert_eq!(guides.list.len(), 1, "replaced: only the centre guide");
        spec.layout.clear_existing = false;
        let Some(Command::SetGuides { guides }) = spec.command(&doc) else {
            panic!("apply emits SetGuides");
        };
        assert_eq!(guides.list.len(), 2, "added beside the old one");
    }
}
