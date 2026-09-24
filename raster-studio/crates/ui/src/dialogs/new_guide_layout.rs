//! W10-J: View > New Guide Layout… — columns, rows, gutters and margins.
//!
//! Photoshop's (and Photopea's) layout dialog: a number of columns and of
//! rows, each with a gutter between neighbours, laid out inside optional
//! margins. Every column and row contributes a guide on each of its edges
//! and each margin a guide on its line; coincident guides are placed once.
//! Confirms to the document's guide set with the layout's guides added — or,
//! with "Clear existing guides" ticked, in place of the old ones — as one
//! [`Command::SetGuides`], so one Ctrl+Z removes the whole layout.

use editor_core::{Command, Guide, GuideAxis, Guides};
use egui::Context;

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::controls::{checkbox_row, integer, numeric};
use crate::strings::tr;

/// The most columns or rows the dialog lays out.
pub const MAX_DIVISIONS: i64 = 100;

/// One direction of the layout: how many divisions, and the gutter between
/// neighbouring divisions, in document pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Divisions {
    pub enabled: bool,
    pub count: i64,
    pub gutter: f64,
}

/// The four margins, in document pixels, and whether they apply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margins {
    pub enabled: bool,
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
}

/// Everything the dialog asks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuideLayoutSpec {
    pub columns: Divisions,
    pub rows: Divisions,
    pub margins: Margins,
    /// Replace the document's guides rather than add to them.
    pub clear_existing: bool,
}

impl Default for GuideLayoutSpec {
    /// Photoshop's opening layout: 8 columns with a 20 px gutter, no rows,
    /// no margins, existing guides kept.
    fn default() -> Self {
        Self {
            columns: Divisions {
                enabled: true,
                count: 8,
                gutter: 20.0,
            },
            rows: Divisions {
                enabled: false,
                count: 4,
                gutter: 20.0,
            },
            margins: Margins {
                enabled: false,
                top: 0.0,
                left: 0.0,
                bottom: 0.0,
                right: 0.0,
            },
            clear_existing: false,
        }
    }
}

/// The edges of `count` divisions with `gutter` between them, filling
/// `lo..hi`. `None` when the gutters leave no room (a division would have no
/// width) or a number is not finite.
fn division_edges(lo: f64, hi: f64, count: i64, gutter: f64) -> Option<Vec<f64>> {
    if !(lo.is_finite() && hi.is_finite() && gutter.is_finite()) || count < 1 || gutter < 0.0 {
        return None;
    }
    let n = count.min(MAX_DIVISIONS) as f64;
    let size = (hi - lo - (n - 1.0) * gutter) / n;
    if size <= 0.0 {
        return None;
    }
    let mut out = Vec::with_capacity(2 * count as usize);
    for i in 0..count.min(MAX_DIVISIONS) {
        let start = lo + i as f64 * (size + gutter);
        out.push(start);
        out.push(start + size);
    }
    Some(out)
}

impl GuideLayoutSpec {
    /// The guides this layout places on a `canvas` (width, height), in the
    /// order columns, rows, margins, each placed once. `None` when the
    /// margins or the gutters leave no room for a column or a row.
    pub fn guides(&self, canvas: (u32, u32)) -> Option<Vec<Guide>> {
        let (w, h) = (f64::from(canvas.0), f64::from(canvas.1));
        let m = self.margins;
        let (left, right, top, bottom) = if m.enabled {
            (m.left, w - m.right, m.top, h - m.bottom)
        } else {
            (0.0, w, 0.0, h)
        };
        if !(left < right && top < bottom) {
            return None;
        }
        let mut vertical = Vec::new();
        let mut horizontal = Vec::new();
        if self.columns.enabled {
            vertical.extend(division_edges(
                left,
                right,
                self.columns.count,
                self.columns.gutter,
            )?);
        }
        if self.rows.enabled {
            horizontal.extend(division_edges(
                top,
                bottom,
                self.rows.count,
                self.rows.gutter,
            )?);
        }
        if m.enabled {
            vertical.extend([left, right]);
            horizontal.extend([top, bottom]);
        }
        let mut out: Vec<Guide> = Vec::new();
        for (axis, values) in [
            (GuideAxis::Vertical, vertical),
            (GuideAxis::Horizontal, horizontal),
        ] {
            for v in values {
                let doc = v as f32;
                if !out
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
        }
        Some(out)
    }
}

/// View > New Guide Layout….
#[derive(Debug, Clone, PartialEq)]
pub struct NewGuideLayoutDialog {
    current: Guides,
    canvas: (u32, u32),
    spec: GuideLayoutSpec,
}

impl NewGuideLayoutDialog {
    /// Over the document's current guide set and its canvas size.
    pub fn new(current: Guides, canvas: (u32, u32)) -> Self {
        Self {
            current,
            canvas,
            spec: GuideLayoutSpec::default(),
        }
    }

    pub fn spec(&self) -> GuideLayoutSpec {
        self.spec
    }

    pub fn set_spec(&mut self, spec: GuideLayoutSpec) {
        self.spec = spec;
    }

    /// The whole set a confirmation writes, or `None` when the layout does
    /// not fit (see [`GuideLayoutSpec::guides`]).
    pub fn guides_after(&self) -> Option<Guides> {
        let placed = self.spec.guides(self.canvas)?;
        let mut next = self.current.clone();
        if self.spec.clear_existing {
            next.list.clear();
        }
        next.list.extend(placed);
        Some(next)
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = resolve(self, keys);
        let drawn = modal(
            ctx,
            "new-guide-layout",
            self.title(),
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(ui, tr("ui.new_guide_layout.subtitle"));
        let px = tr("ui.refine_mask.px.suffix");
        let (w, h) = (f64::from(self.canvas.0), f64::from(self.canvas.1));
        let spec = &mut self.spec;
        for (label, divisions, extent) in [
            ("Columns", &mut spec.columns, w),
            ("Rows", &mut spec.rows, h),
        ] {
            checkbox_row(ui, label, &mut divisions.enabled);
            ui.add_enabled_ui(divisions.enabled, |ui| {
                design::inspector_field(ui, "Number", |ui| {
                    integer(ui, &mut divisions.count, 1..=MAX_DIVISIONS);
                });
                design::inspector_field(ui, "Gutter", |ui| {
                    numeric(ui, &mut divisions.gutter, 0.0..=extent.max(1.0), 0, px);
                });
            });
        }
        let m = &mut spec.margins;
        checkbox_row(ui, "Margins", &mut m.enabled);
        ui.add_enabled_ui(m.enabled, |ui| {
            for (label, value, extent) in [
                ("Top", &mut m.top, h),
                ("Left", &mut m.left, w),
                ("Bottom", &mut m.bottom, h),
                ("Right", &mut m.right, w),
            ] {
                design::inspector_field(ui, label, |ui| {
                    numeric(ui, value, 0.0..=extent.max(1.0), 0, px);
                });
            }
        });
        checkbox_row(
            ui,
            tr("ui.new_guide_layout.clear"),
            &mut spec.clear_existing,
        );
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

impl Dialog for NewGuideLayoutDialog {
    fn title(&self) -> &'static str {
        tr("ui.new_guide_layout.title")
    }

    fn confirm_label(&self) -> &'static str {
        "Place"
    }

    fn confirm(&self) -> Option<DialogAction> {
        self.guides_after()
            .map(|guides| DialogAction::Command(Box::new(Command::SetGuides { guides })))
    }

    fn blocked_reason(&self) -> Option<String> {
        self.guides_after()
            .is_none()
            .then(|| tr("ui.new_guide_layout.no.room").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verticals(guides: &[Guide]) -> Vec<f32> {
        guides
            .iter()
            .filter(|g| g.axis == GuideAxis::Vertical)
            .map(|g| g.doc)
            .collect()
    }

    fn horizontals(guides: &[Guide]) -> Vec<f32> {
        guides
            .iter()
            .filter(|g| g.axis == GuideAxis::Horizontal)
            .map(|g| g.doc)
            .collect()
    }

    #[test]
    fn two_columns_with_a_gutter_put_a_guide_on_every_column_edge() {
        let spec = GuideLayoutSpec {
            columns: Divisions {
                enabled: true,
                count: 2,
                gutter: 20.0,
            },
            ..GuideLayoutSpec::default()
        };
        let g = spec.guides((220, 100)).unwrap();
        assert_eq!(verticals(&g), vec![0.0, 100.0, 120.0, 220.0]);
        assert!(horizontals(&g).is_empty(), "rows are off");
    }

    #[test]
    fn margins_inset_the_layout_and_add_their_own_lines_once() {
        let spec = GuideLayoutSpec {
            columns: Divisions {
                enabled: true,
                count: 1,
                gutter: 0.0,
            },
            rows: Divisions {
                enabled: true,
                count: 2,
                gutter: 0.0,
            },
            margins: Margins {
                enabled: true,
                top: 10.0,
                left: 20.0,
                bottom: 30.0,
                right: 40.0,
            },
            clear_existing: false,
        };
        let g = spec.guides((200, 140)).unwrap();
        // The single column's edges are the margins' lines: placed once.
        assert_eq!(verticals(&g), vec![20.0, 160.0]);
        assert_eq!(horizontals(&g), vec![10.0, 60.0, 110.0]);
    }

    #[test]
    fn gutters_that_leave_no_room_block_the_dialog_with_a_reason() {
        let mut dialog = NewGuideLayoutDialog::new(Guides::default(), (100, 100));
        dialog.set_spec(GuideLayoutSpec {
            columns: Divisions {
                enabled: true,
                count: 10,
                gutter: 20.0,
            },
            ..GuideLayoutSpec::default()
        });
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("The margins and gutters leave no room for a column or a row")
        );
    }

    #[test]
    fn confirming_adds_to_the_existing_set_or_replaces_it() {
        let existing = Guides {
            list: vec![Guide {
                axis: GuideAxis::Horizontal,
                doc: 7.0,
                locked: true,
            }],
            visible: true,
            locked: false,
        };
        let mut dialog = NewGuideLayoutDialog::new(existing, (640, 64));
        let placed = match dialog.confirm() {
            Some(DialogAction::Command(c)) => match *c {
                Command::SetGuides { guides } => guides,
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        assert_eq!(placed.list[0].doc, 7.0, "the existing guide is kept");
        assert_eq!(placed.list.len(), 1 + 16, "8 columns, 2 edges each");
        let mut spec = dialog.spec();
        spec.clear_existing = true;
        dialog.set_spec(spec);
        let replaced = dialog.guides_after().unwrap();
        assert_eq!(replaced.list.len(), 16);
        assert!(replaced.visible, "the set's own flags are kept");
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = NewGuideLayoutDialog::new(Guides::default(), (64, 64));
            assert!(dialog.show(ctx).is_open());
        });
    }
}
