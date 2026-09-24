//! W10-E: File ▸ Export ▸ Color Lookup Tables….
//!
//! Photoshop's Export Color Lookup writes the document's adjustment-layer
//! stack as a 3D LUT: an identity lattice of colours run through every
//! visible adjustment layer, bottom to top, and the results written as an
//! Adobe/Resolve `.cube`. This dialog asks the two questions the file needs —
//! the lattice size (17, 33 or 65 points per edge, Photoshop's Small /
//! Medium / Large grid) and the table's title — and hands back an
//! [`ExportLutSpec`]; the lattice is sampled and the file written by the
//! application (`app_shell::file_extras`).

use egui::Context;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::combo;
use super::sizes;
use crate::strings::tr;

/// The lattice edge of an exported cube.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LutGrid {
    /// 17 points per edge (Photoshop's "Small").
    Small,
    /// 33 points per edge ("Medium", the default).
    Medium,
    /// 65 points per edge ("Large").
    Large,
}

impl LutGrid {
    pub const ALL: [LutGrid; 3] = [LutGrid::Small, LutGrid::Medium, LutGrid::Large];

    /// Points per edge.
    pub const fn points(self) -> usize {
        match self {
            LutGrid::Small => 17,
            LutGrid::Medium => 33,
            LutGrid::Large => 65,
        }
    }

    pub fn label(self) -> String {
        format!("{} ({})", self.points(), tr(self.name_key()))
    }

    const fn name_key(self) -> &'static str {
        match self {
            LutGrid::Small => "ui.export_lut.small",
            LutGrid::Medium => "ui.export_lut.medium",
            LutGrid::Large => "ui.export_lut.large",
        }
    }
}

/// A confirmed Color Lookup export.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExportLutSpec {
    pub grid: LutGrid,
    /// Written as the file's `TITLE`; also its file name.
    pub title: String,
}

/// File ▸ Export ▸ Color Lookup Tables….
#[derive(Clone, Debug)]
pub struct ExportLutDialog {
    spec: ExportLutSpec,
    /// How many visible adjustment layers the document has, for the caption.
    adjustments: usize,
}

impl ExportLutDialog {
    /// Over a document titled `title` with `adjustments` visible adjustment
    /// layers.
    pub fn new(title: impl Into<String>, adjustments: usize) -> Self {
        Self {
            spec: ExportLutSpec {
                grid: LutGrid::Medium,
                title: title.into(),
            },
            adjustments,
        }
    }

    pub fn spec(&self) -> &ExportLutSpec {
        &self.spec
    }

    pub fn set_spec(&mut self, spec: ExportLutSpec) {
        self.spec = spec;
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<&'static str> {
        self.spec
            .title
            .trim()
            .is_empty()
            .then(|| tr("ui.export_lut.title.empty"))
    }

    pub fn confirm(&self) -> Option<ExportLutSpec> {
        self.blocked_reason().is_none().then(|| ExportLutSpec {
            grid: self.spec.grid,
            title: self.spec.title.trim().to_string(),
        })
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<ExportLutSpec> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(spec) = self.confirm() {
                return DialogOutcome::Confirmed(spec);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<ExportLutSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "w10e-export-lut",
            tr("ui.export_lut.title"),
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
        caption(ui, tr("ui.export_lut.subtitle"));
        caption(
            ui,
            format!("{}: {}", tr("ui.export_lut.adjustments"), self.adjustments),
        );
        design::inspector_field(ui, tr("ui.export_lut.name"), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.spec.title)
                    .id(egui::Id::new("w10e-export-lut-title"))
                    .desired_width(sizes::text_field_name()),
            );
        });
        design::inspector_field(ui, tr("ui.export_lut.grid"), |ui| {
            combo(
                ui,
                "w10e-export-lut-grid",
                &mut self.spec.grid,
                &LutGrid::ALL,
                LutGrid::label,
                |_| None,
            );
        });
        action_row(ui, tr("ui.export_lut.export"), self.blocked_reason(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_sizes_are_photoshops_and_a_blank_title_blocks() {
        assert_eq!(
            LutGrid::ALL.map(LutGrid::points),
            [17, 33, 65],
            "Small / Medium / Large"
        );
        let mut dialog = ExportLutDialog::new("Look", 2);
        assert_eq!(dialog.spec().grid, LutGrid::Medium);
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(ExportLutSpec {
                grid: LutGrid::Medium,
                title: "Look".into()
            })
        );
        dialog.set_spec(ExportLutSpec {
            grid: LutGrid::Large,
            title: "  ".into(),
        });
        assert!(dialog.blocked_reason().is_some());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = ExportLutDialog::new("Look", 0);
            assert!(dialog.show(ctx).is_open());
        });
    }
}
