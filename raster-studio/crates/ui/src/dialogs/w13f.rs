//! W13-F: Edit ▸ Convert to Profile…, Image ▸ Reduce Colors… and Image ▸
//! Wavelet Decompose… — Photopea's three questions.
//!
//! Each dialog hands back a spec and nothing else; the pixels are the
//! application's (`app-shell`'s `menu_w13f` converts, quantises or splits
//! as one undoable step). Like Trim, there is no [`super::chrome::Dialog`]
//! impl: the shell parks the confirmed spec for the menu arm of the same
//! name. `show` folds Escape, Enter and the action row into one
//! [`DialogOutcome`], so the keyboard contract is every other dialog's.

use egui::Context;

use color::quantize::{Dither, PaletteKind, MAX_COLORS, MIN_COLORS};

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, combo, integer, readout};
use super::indexed_color::{dither_label, palette_label, IndexedSpec};
use crate::menu::ProfileChoice;
use crate::strings::tr;

/// The scale counts Image ▸ Wavelet Decompose… accepts.
pub const WAVELET_SCALES: std::ops::RangeInclusive<u8> = 2..=7;

/// Fold the keyboard and a drawn action row into one outcome — the shape
/// every dialog here shares. Escape wins over Enter.
fn outcome<S: Copy>(
    keys: DialogKeys,
    drawn: Option<Option<DialogButton>>,
    confirm: Option<S>,
) -> DialogOutcome<S> {
    let mut out = resolve(keys, confirm);
    if let Some(Some(button)) = drawn {
        out = match button {
            DialogButton::Cancel => DialogOutcome::Cancelled,
            DialogButton::Confirm => confirm.map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
            DialogButton::Extra(_) => DialogOutcome::Open,
        };
    }
    out
}

fn resolve<S>(keys: DialogKeys, confirm: Option<S>) -> DialogOutcome<S> {
    if keys.cancel {
        return DialogOutcome::Cancelled;
    }
    match confirm {
        Some(spec) if keys.confirm => DialogOutcome::Confirmed(spec),
        _ => DialogOutcome::Open,
    }
}

// ---------------------------------------------------------------------------
// Convert to Profile
// ---------------------------------------------------------------------------

/// The ICC rendering intents Convert to Profile offers, in Photoshop's order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum RenderingIntent {
    Perceptual,
    Saturation,
    /// Media white maps to media white (the ICC default).
    #[default]
    RelativeColorimetric,
    /// The source white is kept as the colour it is, not adapted to the
    /// destination's white.
    AbsoluteColorimetric,
}

impl RenderingIntent {
    pub const ALL: [RenderingIntent; 4] = [
        RenderingIntent::Perceptual,
        RenderingIntent::Saturation,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::AbsoluteColorimetric,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RenderingIntent::Perceptual => tr("ui.w13f.intent.perceptual"),
            RenderingIntent::Saturation => tr("ui.w13f.intent.saturation"),
            RenderingIntent::RelativeColorimetric => tr("ui.w13f.intent.relative"),
            RenderingIntent::AbsoluteColorimetric => tr("ui.w13f.intent.absolute"),
        }
    }
}

/// The confirmed conversion: destination, intent, black point compensation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ConvertProfileSpec {
    pub target: ProfileChoice,
    pub intent: RenderingIntent,
    pub black_point: bool,
}

impl ConvertProfileSpec {
    /// Photoshop's opening state, aimed at the first listed profile that is
    /// not the document's own (`current`): Relative Colorimetric, with black
    /// point compensation.
    pub fn default_for(current: Option<ProfileChoice>) -> Self {
        let target = ProfileChoice::ALL
            .iter()
            .copied()
            .find(|p| Some(*p) != current)
            .unwrap_or(ProfileChoice::Srgb);
        Self {
            target,
            intent: RenderingIntent::default(),
            black_point: true,
        }
    }
}

/// Edit ▸ Convert to Profile….
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertProfileDialog {
    spec: ConvertProfileSpec,
    /// The built-in profile the document wears, which is no destination.
    current: Option<ProfileChoice>,
    /// The document's profile, by name, for the Source Space readout.
    source: String,
}

impl ConvertProfileDialog {
    pub fn new(current: Option<ProfileChoice>, source: impl Into<String>) -> Self {
        Self {
            spec: ConvertProfileSpec::default_for(current),
            current,
            source: source.into(),
        }
    }

    pub fn spec(&self) -> ConvertProfileSpec {
        self.spec
    }

    /// Set the spec directly — tests and presets.
    pub fn set_spec(&mut self, spec: ConvertProfileSpec) {
        self.spec = spec;
    }

    pub fn title(&self) -> &'static str {
        tr("ui.w13f.convert.title")
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        (Some(self.spec.target) == self.current).then(|| tr("ui.w13f.convert.same").to_string())
    }

    pub fn confirm(&self) -> Option<ConvertProfileSpec> {
        self.blocked_reason().is_none().then_some(self.spec)
    }

    /// Escape and Enter, without drawing.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<ConvertProfileSpec> {
        resolve(keys, self.confirm())
    }

    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<ConvertProfileSpec> {
        let keys = DialogKeys::read(ctx);
        let title = self.title();
        let drawn = modal(
            ctx,
            "w13f-convert-profile",
            title,
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        outcome(keys, drawn, self.confirm())
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(ui, tr("ui.w13f.convert.subtitle"));
        design::section_header(ui, tr("ui.w13f.convert.source"));
        readout(ui, self.source.clone());
        design::section_header(ui, tr("ui.w13f.convert.destination"));
        let current = self.current;
        combo(
            ui,
            egui::Id::new(("dialogs", "w13f-convert-target")),
            &mut self.spec.target,
            ProfileChoice::ALL,
            |p| p.label().to_string(),
            |p| (Some(p) == current).then(|| tr("ui.w13f.convert.same")),
        );
        design::section_header(ui, tr("ui.w13f.convert.options"));
        ui.label(tr("ui.w13f.convert.intent"));
        combo(
            ui,
            egui::Id::new(("dialogs", "w13f-convert-intent")),
            &mut self.spec.intent,
            &RenderingIntent::ALL,
            |i| i.label().to_string(),
            |_| None,
        );
        checkbox_row(ui, tr("ui.w13f.convert.bpc"), &mut self.spec.black_point);
        caption(ui, tr("ui.w13f.convert.intent_note"));
        action_row(ui, tr("ui.w13f.ok"), self.blocked_reason().as_deref(), &[])
    }
}

// ---------------------------------------------------------------------------
// Reduce Colors
// ---------------------------------------------------------------------------

/// Image ▸ Reduce Colors…: the Indexed Color question (palette source,
/// count, dither) asked of the active layer, which stays RGB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReduceColorsDialog {
    spec: IndexedSpec,
}

impl Default for ReduceColorsDialog {
    fn default() -> Self {
        Self {
            spec: Self::DEFAULT,
        }
    }
}

impl ReduceColorsDialog {
    /// The opening state: an adaptive palette of 16 colours, diffused.
    pub const DEFAULT: IndexedSpec = IndexedSpec {
        palette: PaletteKind::Adaptive,
        colors: 16,
        dither: Dither::Diffusion,
    };

    pub fn spec(&self) -> IndexedSpec {
        self.spec
    }

    /// Set the spec directly — tests and presets.
    pub fn set_spec(&mut self, spec: IndexedSpec) {
        self.spec = spec;
    }

    pub fn title(&self) -> &'static str {
        tr("ui.w13f.reduce.title")
    }

    pub fn blocked_reason(&self) -> Option<String> {
        (!self.spec.is_valid()).then(|| tr("ui.w13f.reduce.bad_count").to_string())
    }

    pub fn confirm(&self) -> Option<IndexedSpec> {
        self.spec.is_valid().then_some(self.spec)
    }

    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<IndexedSpec> {
        resolve(keys, self.confirm())
    }

    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<IndexedSpec> {
        let keys = DialogKeys::read(ctx);
        let title = self.title();
        let drawn = modal(
            ctx,
            "w13f-reduce-colors",
            title,
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        outcome(keys, drawn, self.confirm())
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(ui, tr("ui.w13f.reduce.subtitle"));
        design::section_header(ui, tr("ui.w13f.reduce.palette"));
        combo(
            ui,
            egui::Id::new(("dialogs", "w13f-reduce-palette")),
            &mut self.spec.palette,
            &PaletteKind::ALL,
            |k| palette_label(k).to_string(),
            |_| None,
        );
        design::section_header(ui, tr("ui.w13f.reduce.colors"));
        let mut n = i64::from(self.spec.colors);
        let web = self.spec.palette == PaletteKind::Web;
        ui.add_enabled_ui(!web, |ui| {
            integer(ui, &mut n, i64::from(MIN_COLORS)..=i64::from(MAX_COLORS))
        });
        self.spec.colors = n.clamp(i64::from(MIN_COLORS), i64::from(MAX_COLORS)) as u16;
        design::section_header(ui, tr("ui.w13f.reduce.dither"));
        combo(
            ui,
            egui::Id::new(("dialogs", "w13f-reduce-dither")),
            &mut self.spec.dither,
            &Dither::ALL,
            |d| dither_label(d).to_string(),
            |_| None,
        );
        action_row(ui, tr("ui.w13f.ok"), self.blocked_reason().as_deref(), &[])
    }
}

// ---------------------------------------------------------------------------
// Wavelet Decompose
// ---------------------------------------------------------------------------

/// The confirmed split: how many detail layers over the residual.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WaveletSpec {
    pub scales: u8,
}

impl Default for WaveletSpec {
    fn default() -> Self {
        Self { scales: 5 }
    }
}

impl WaveletSpec {
    pub fn is_valid(&self) -> bool {
        WAVELET_SCALES.contains(&self.scales)
    }
}

/// Image ▸ Wavelet Decompose….
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WaveletDialog {
    spec: WaveletSpec,
}

impl WaveletDialog {
    pub fn spec(&self) -> WaveletSpec {
        self.spec
    }

    /// Set the spec directly — tests and presets.
    pub fn set_spec(&mut self, spec: WaveletSpec) {
        self.spec = spec;
    }

    pub fn title(&self) -> &'static str {
        tr("ui.w13f.wavelet.title")
    }

    pub fn blocked_reason(&self) -> Option<String> {
        (!self.spec.is_valid()).then(|| tr("ui.w13f.wavelet.bad_count").to_string())
    }

    pub fn confirm(&self) -> Option<WaveletSpec> {
        self.spec.is_valid().then_some(self.spec)
    }

    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<WaveletSpec> {
        resolve(keys, self.confirm())
    }

    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<WaveletSpec> {
        let keys = DialogKeys::read(ctx);
        let title = self.title();
        let drawn = modal(
            ctx,
            "w13f-wavelet",
            title,
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        outcome(keys, drawn, self.confirm())
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(ui, tr("ui.w13f.wavelet.subtitle"));
        design::section_header(ui, tr("ui.w13f.wavelet.scales"));
        let mut n = i64::from(self.spec.scales);
        integer(
            ui,
            &mut n,
            i64::from(*WAVELET_SCALES.start())..=i64::from(*WAVELET_SCALES.end()),
        );
        self.spec.scales = n.clamp(
            i64::from(*WAVELET_SCALES.start()),
            i64::from(*WAVELET_SCALES.end()),
        ) as u8;
        action_row(ui, tr("ui.w13f.ok"), self.blocked_reason().as_deref(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_opens_on_another_profile_relative_with_black_point() {
        let dialog = ConvertProfileDialog::new(Some(ProfileChoice::Srgb), "sRGB");
        let spec = dialog.confirm().expect("live");
        assert_eq!(spec.target, ProfileChoice::AdobeRgb);
        assert_eq!(spec.intent, RenderingIntent::RelativeColorimetric);
        assert!(spec.black_point);
        let other = ConvertProfileDialog::new(Some(ProfileChoice::AdobeRgb), "Adobe");
        assert_eq!(other.spec().target, ProfileChoice::Srgb);
    }

    #[test]
    fn convert_to_the_documents_own_profile_is_blocked_with_a_reason() {
        let mut dialog = ConvertProfileDialog::new(Some(ProfileChoice::DisplayP3), "P3");
        dialog.set_spec(ConvertProfileSpec {
            target: ProfileChoice::DisplayP3,
            ..dialog.spec()
        });
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("The destination is the profile the document already has")
        );
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn enter_confirms_each_spec_that_was_set() {
        let mut convert = ConvertProfileDialog::new(None, "Custom");
        let spec = ConvertProfileSpec {
            target: ProfileChoice::ProPhotoRgb,
            intent: RenderingIntent::AbsoluteColorimetric,
            black_point: false,
        };
        convert.set_spec(spec);
        assert_eq!(
            convert.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(spec)
        );

        let mut reduce = ReduceColorsDialog::default();
        assert_eq!(reduce.confirm(), Some(ReduceColorsDialog::DEFAULT));
        let spec = IndexedSpec {
            palette: PaletteKind::Uniform,
            colors: 9,
            dither: Dither::None,
        };
        reduce.set_spec(spec);
        assert_eq!(
            reduce.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(spec)
        );
        reduce.set_spec(IndexedSpec { colors: 1, ..spec });
        assert!(reduce.blocked_reason().is_some());
        assert!(reduce.resolve(DialogKeys::CONFIRM).is_open());

        let mut wavelet = WaveletDialog::default();
        assert_eq!(wavelet.confirm(), Some(WaveletSpec { scales: 5 }));
        wavelet.set_spec(WaveletSpec { scales: 3 });
        assert_eq!(
            wavelet.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(WaveletSpec { scales: 3 })
        );
        wavelet.set_spec(WaveletSpec { scales: 9 });
        assert_eq!(
            wavelet.blocked_reason().as_deref(),
            Some("Wavelet Decompose splits into 2 to 7 scales")
        );
        assert_eq!(
            wavelet.resolve(DialogKeys {
                confirm: true,
                cancel: true,
            }),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn every_choice_has_a_label() {
        for i in RenderingIntent::ALL {
            assert!(!i.label().is_empty(), "{i:?}");
        }
        for p in ProfileChoice::ALL {
            assert!(!p.label().is_empty(), "{p:?}");
        }
    }

    #[test]
    fn they_draw_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut convert = ConvertProfileDialog::new(Some(ProfileChoice::Srgb), "sRGB");
            assert!(convert.show(ctx).is_open());
            let mut reduce = ReduceColorsDialog::default();
            assert!(reduce.show(ctx).is_open());
            let mut wavelet = WaveletDialog::default();
            assert!(wavelet.show(ctx).is_open());
        });
    }
}
