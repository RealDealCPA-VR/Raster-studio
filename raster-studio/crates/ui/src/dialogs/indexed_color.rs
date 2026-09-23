//! Image ▸ Mode ▸ Indexed Color… — Photopea's question: where the palette
//! comes from, how many colours it holds, and whether to dither.
//!
//! The dialog hands back an [`IndexedSpec`] and nothing else; the pixels are
//! the application's (the shell builds the palette over the whole document
//! with `color::quantize` and converts as one undoable step). Like Trim, there
//! is no [`super::chrome::Dialog`] impl: the shell parks the confirmed spec for
//! the `SetColorMode(Indexed)` menu arm.

use egui::Context;

use color::quantize::{Dither, PaletteKind, MAX_COLORS, MIN_COLORS};

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{combo, integer};
use crate::strings::tr;

/// The confirmed conversion: palette source, colour count and dither.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IndexedSpec {
    pub palette: PaletteKind,
    /// `MIN_COLORS..=MAX_COLORS`; Web always has 216 and ignores it.
    pub colors: u16,
    pub dither: Dither,
}

impl Default for IndexedSpec {
    /// Photopea's opening state: an adaptive 256-colour palette, diffused.
    fn default() -> Self {
        Self {
            palette: PaletteKind::Adaptive,
            colors: MAX_COLORS,
            dither: Dither::Diffusion,
        }
    }
}

impl IndexedSpec {
    /// Whether the colour count is one a palette can hold.
    pub fn is_valid(&self) -> bool {
        (MIN_COLORS..=MAX_COLORS).contains(&self.colors)
    }
}

/// The palette source's name in the dialog.
pub fn palette_label(kind: PaletteKind) -> &'static str {
    match kind {
        PaletteKind::Exact => tr("ui.indexed.exact"),
        PaletteKind::Web => tr("ui.indexed.web"),
        PaletteKind::Uniform => tr("ui.indexed.uniform"),
        PaletteKind::Adaptive => tr("ui.indexed.adaptive"),
    }
}

/// The dither choice's name in the dialog.
pub fn dither_label(dither: Dither) -> &'static str {
    match dither {
        Dither::None => tr("ui.indexed.dither.none"),
        Dither::Diffusion => tr("ui.indexed.dither.diffusion"),
    }
}

/// Image ▸ Mode ▸ Indexed Color….
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexedColorDialog {
    spec: IndexedSpec,
}

impl IndexedColorDialog {
    pub fn new(spec: IndexedSpec) -> Self {
        Self { spec }
    }

    /// The spec as it stands.
    pub fn spec(&self) -> IndexedSpec {
        self.spec
    }

    /// Set the spec directly — tests and presets.
    pub fn set_spec(&mut self, spec: IndexedSpec) {
        self.spec = spec;
    }

    pub fn title(&self) -> &'static str {
        tr("ui.indexed.title")
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        (!self.spec.is_valid()).then(|| tr("ui.indexed.bad.count").to_string())
    }

    /// The spec a confirmation hands over, or `None` when it is invalid.
    pub fn confirm(&self) -> Option<IndexedSpec> {
        self.spec.is_valid().then_some(self.spec)
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<IndexedSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<IndexedSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "indexed-color",
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
        caption(ui, tr("ui.indexed.subtitle"));
        design::section_header(ui, tr("ui.indexed.palette"));
        combo(
            ui,
            egui::Id::new(("dialogs", "indexed-palette")),
            &mut self.spec.palette,
            &PaletteKind::ALL,
            |k| palette_label(k).to_string(),
            |_| None,
        );
        design::section_header(ui, tr("ui.indexed.colors"));
        let mut n = i64::from(self.spec.colors);
        let web = self.spec.palette == PaletteKind::Web;
        ui.add_enabled_ui(!web, |ui| {
            integer(ui, &mut n, i64::from(MIN_COLORS)..=i64::from(MAX_COLORS))
        });
        self.spec.colors = n.clamp(i64::from(MIN_COLORS), i64::from(MAX_COLORS)) as u16;
        design::section_header(ui, tr("ui.indexed.dither"));
        combo(
            ui,
            egui::Id::new(("dialogs", "indexed-dither")),
            &mut self.spec.dither,
            &Dither::ALL,
            |d| dither_label(d).to_string(),
            |_| None,
        );
        action_row(ui, "OK", self.blocked_reason().as_deref(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_adaptive_256_diffused() {
        let dialog = IndexedColorDialog::default();
        assert_eq!(dialog.confirm(), Some(IndexedSpec::default()));
        assert_eq!(dialog.spec().palette, PaletteKind::Adaptive);
        assert_eq!(dialog.spec().colors, 256);
        assert_eq!(dialog.spec().dither, Dither::Diffusion);
    }

    #[test]
    fn a_count_outside_the_palette_range_is_blocked() {
        let mut dialog = IndexedColorDialog::default();
        dialog.set_spec(IndexedSpec {
            colors: 1,
            ..IndexedSpec::default()
        });
        assert_eq!(dialog.confirm(), None);
        assert!(dialog.blocked_reason().is_some());
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn enter_confirms_the_spec_and_escape_wins() {
        let mut dialog = IndexedColorDialog::default();
        let spec = IndexedSpec {
            palette: PaletteKind::Uniform,
            colors: 16,
            dither: Dither::None,
        };
        dialog.set_spec(spec);
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(spec)
        );
        assert_eq!(
            dialog.resolve(DialogKeys {
                confirm: true,
                cancel: true,
            }),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn every_choice_has_a_label() {
        for k in PaletteKind::ALL {
            assert!(!palette_label(k).is_empty());
        }
        for d in Dither::ALL {
            assert!(!dither_label(d).is_empty());
        }
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = IndexedColorDialog::default();
            assert!(dialog.show(ctx).is_open());
        });
    }
}
