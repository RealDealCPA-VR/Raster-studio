//! Image Size — resample the document.
//!
//! The whole dialog is one small state machine over four coupled numbers
//! (pixel width, pixel height, resolution, printed size) and two switches
//! (constrain proportions, resample). Which of the four move when one is typed
//! into depends on those switches, and getting that wrong silently destroys
//! pixels — so the coupling lives here, as pure arithmetic, and is tested
//! exhaustively.
//!
//! # Constrain proportions is exact, not approximate
//!
//! The aspect ratio is stored as the *reduced integer* ratio of the original
//! size, not as a float. `1920 x 1080` is therefore `16 : 9`, and every width
//! that is a multiple of 16 produces a height that satisfies
//! `new_w * old_h == new_h * old_w` exactly. A float ratio drifts, and after a
//! few edits a "locked" 16:9 document is 1.0002:1 out.
//!
//! Between the multiples the ratio cannot be hit exactly — no integer height
//! is 16:9 with a width of 1921 — so the guarantee there is that the answer is
//! the **nearest** one, computed in integers and rounded half up. That is what
//! `constrain_rounds_the_other_side_instead_of_truncating_it` and
//! `the_locked_side_is_always_the_nearest_integer` assert, and they exist
//! because the multiples-only tests that came first could not tell this
//! arithmetic apart from the truncating float the paragraph above rejects.

use design::tokens::Space;
use egui::Context;
use raster::ResampleFilter;

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, warning, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::controls::{checkbox_row, combo, numeric, readout};
use super::new_document::{MAX_DIMENSION, MAX_PIXELS};
use super::units::{format_bytes, ResolutionUnit, Unit, MAX_PPI};
use crate::strings::tr;

/// Bytes one pixel of a flattened 8-bit RGBA document occupies.
pub const BYTES_PER_PIXEL: u64 = 4;

/// What the dialog commits to.
#[derive(Clone, PartialEq, Debug)]
pub struct ImageSizeSpec {
    pub width: u32,
    pub height: u32,
    pub resolution_ppi: f64,
    /// `Some(filter)` resamples the pixels; `None` changes only the print
    /// metadata, leaving every pixel exactly as it was.
    pub resample: Option<ResampleFilter>,
}

impl ImageSizeSpec {
    /// Whether this specification can be applied.
    pub fn is_valid(&self) -> bool {
        self.width >= 1
            && self.height >= 1
            && self.width <= MAX_DIMENSION
            && self.height <= MAX_DIMENSION
            && u64::from(self.width) * u64::from(self.height) <= MAX_PIXELS
            && self.resolution_ppi.is_finite()
            && self.resolution_ppi > 0.0
            && self.resolution_ppi <= MAX_PPI
    }

    /// Bytes the flattened result occupies.
    pub fn flat_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * BYTES_PER_PIXEL
    }
}

/// Image Size.
#[derive(Clone, Debug)]
pub struct ImageSizeDialog {
    original_width: u32,
    original_height: u32,
    original_ppi: f64,
    /// The reduced integer aspect ratio of the original size.
    ratio_w: u32,
    ratio_h: u32,
    width: u32,
    height: u32,
    ppi: f64,
    constrain: bool,
    resample: bool,
    filter: ResampleFilter,
    pixel_unit: Unit,
    print_unit: Unit,
    resolution_unit: ResolutionUnit,
}

impl ImageSizeDialog {
    /// Open the dialog on a document of `width` x `height` at `ppi`.
    pub fn new(width: u32, height: u32, ppi: f64) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let divisor = gcd(width, height).max(1);
        Self {
            original_width: width,
            original_height: height,
            original_ppi: ppi,
            ratio_w: width / divisor,
            ratio_h: height / divisor,
            width,
            height,
            ppi: sane_ppi(ppi),
            constrain: true,
            resample: true,
            filter: ResampleFilter::Lanczos3,
            pixel_unit: Unit::Pixels,
            print_unit: Unit::Inches,
            resolution_unit: ResolutionUnit::PerInch,
        }
    }

    /// The size the document had when the dialog opened.
    pub fn original(&self) -> (u32, u32) {
        (self.original_width, self.original_height)
    }

    /// The reduced aspect ratio the constrain switch locks to.
    pub fn aspect_ratio(&self) -> (u32, u32) {
        (self.ratio_w, self.ratio_h)
    }

    /// Current pixel width.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Current pixel height.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Current resolution in pixels per inch.
    pub fn resolution_ppi(&self) -> f64 {
        self.ppi
    }

    /// Whether the aspect ratio is locked.
    pub fn constrain(&self) -> bool {
        self.constrain
    }

    /// Lock or unlock the aspect ratio.
    ///
    /// Re-locking re-derives the ratio from the size *now on screen*, so the
    /// user's freehand size becomes the new lock rather than snapping back.
    pub fn set_constrain(&mut self, constrain: bool) {
        if constrain && !self.constrain {
            let divisor = gcd(self.width, self.height).max(1);
            self.ratio_w = self.width / divisor;
            self.ratio_h = self.height / divisor;
        }
        self.constrain = constrain;
    }

    /// Whether pixels will be resampled.
    pub fn resample(&self) -> bool {
        self.resample
    }

    /// Turn resampling on or off.
    ///
    /// Switching it **off** restores the original pixel dimensions, because
    /// with resampling off the pixel count is by definition not editable — and
    /// leaving a half-typed pixel size on screen that the dialog would then
    /// ignore is exactly the trap this switch exists to avoid. The printed size
    /// the user was aiming at is preserved by moving the resolution instead.
    pub fn set_resample(&mut self, resample: bool) {
        if self.resample && !resample {
            let print_w = self.print_width_in();
            self.width = self.original_width;
            self.height = self.original_height;
            self.ppi = sane_ppi(f64::from(self.width) / print_w.max(f64::EPSILON));
        }
        self.resample = resample;
    }

    /// The resampling method used when [`ImageSizeDialog::resample`] is on.
    pub fn filter(&self) -> ResampleFilter {
        self.filter
    }

    /// Choose the resampling method.
    pub fn set_filter(&mut self, filter: ResampleFilter) {
        self.filter = filter;
    }

    /// Whether the pixel-dimension fields accept input right now.
    pub fn pixels_editable(&self) -> bool {
        self.resample
    }

    /// Set the pixel width. Returns `false` when resampling is off, because
    /// then the pixel count is not the user's to change.
    pub fn set_width(&mut self, width: u32) -> bool {
        if !self.resample {
            return false;
        }
        self.width = width.clamp(1, MAX_DIMENSION);
        if self.constrain {
            self.height = other_side(self.width, self.ratio_w, self.ratio_h);
        }
        true
    }

    /// Set the pixel height. Returns `false` when resampling is off.
    pub fn set_height(&mut self, height: u32) -> bool {
        if !self.resample {
            return false;
        }
        self.height = height.clamp(1, MAX_DIMENSION);
        if self.constrain {
            self.width = other_side(self.height, self.ratio_h, self.ratio_w);
        }
        true
    }

    /// Printed width in inches.
    pub fn print_width_in(&self) -> f64 {
        f64::from(self.width) / self.ppi
    }

    /// Printed height in inches.
    pub fn print_height_in(&self) -> f64 {
        f64::from(self.height) / self.ppi
    }

    /// Set the printed width, in inches.
    ///
    /// With resampling **on** the resolution is fixed and the pixel count
    /// follows. With it **off** the pixel count is fixed and the resolution
    /// follows — the same physical picture printed larger or smaller.
    pub fn set_print_width_in(&mut self, inches: f64) {
        if !(inches.is_finite() && inches > 0.0) {
            return;
        }
        if self.resample {
            self.set_width((inches * self.ppi).round().max(1.0) as u32);
        } else {
            self.ppi = sane_ppi(f64::from(self.width) / inches);
        }
    }

    /// Set the printed height, in inches. See [`ImageSizeDialog::set_print_width_in`].
    pub fn set_print_height_in(&mut self, inches: f64) {
        if !(inches.is_finite() && inches > 0.0) {
            return;
        }
        if self.resample {
            self.set_height((inches * self.ppi).round().max(1.0) as u32);
        } else {
            self.ppi = sane_ppi(f64::from(self.height) / inches);
        }
    }

    /// Set the resolution.
    ///
    /// With resampling **on** the printed size is what the user is holding
    /// fixed, so the pixel count moves with the resolution. With it **off** the
    /// pixels cannot move, so the printed size does.
    pub fn set_resolution_ppi(&mut self, ppi: f64) {
        let ppi = sane_ppi(ppi);
        if self.resample {
            let (print_w, print_h) = (self.print_width_in(), self.print_height_in());
            self.ppi = ppi;
            self.width = (print_w * ppi).round().clamp(1.0, f64::from(MAX_DIMENSION)) as u32;
            self.height = (print_h * ppi).round().clamp(1.0, f64::from(MAX_DIMENSION)) as u32;
        } else {
            self.ppi = ppi;
        }
    }

    /// Put every field back the way the document actually is.
    pub fn reset(&mut self) {
        *self = Self {
            constrain: self.constrain,
            resample: self.resample,
            filter: self.filter,
            pixel_unit: self.pixel_unit,
            print_unit: self.print_unit,
            resolution_unit: self.resolution_unit,
            ..Self::new(self.original_width, self.original_height, self.original_ppi)
        };
    }

    /// Bytes the document occupied when the dialog opened.
    pub fn original_bytes(&self) -> u64 {
        u64::from(self.original_width) * u64::from(self.original_height) * BYTES_PER_PIXEL
    }

    /// The specification the dialog currently describes.
    pub fn spec(&self) -> ImageSizeSpec {
        ImageSizeSpec {
            width: self.width,
            height: self.height,
            resolution_ppi: self.ppi,
            resample: (self.resample
                && (self.width != self.original_width || self.height != self.original_height))
                .then_some(self.filter),
        }
    }

    /// Draw the dialog for one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = super::chrome::resolve(self, keys);
        let drawn = modal(
            ctx,
            "image-size",
            self.title(),
            Some(crate::strings::tr(
                "ui.image_size.change.how.many.pixels.the.document",
            )),
            DialogWidth::Standard,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => {
                    self.reset();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        let spec = self.spec();
        readout(
            ui,
            format!(
                "{}  ->  {}",
                format_bytes(self.original_bytes()),
                format_bytes(spec.flat_bytes())
            ),
        );
        hairline(ui);

        design::section_header(ui, crate::strings::tr("ui.image_size.pixel.dimensions"));
        let editable = self.pixels_editable();
        ui.add_enabled_ui(editable, |ui| {
            design::inspector_field(ui, "Width", |ui| {
                let mut value = self.pixel_field(self.width, self.original_width);
                if numeric(
                    ui,
                    &mut value,
                    0.0..=1.0e7,
                    self.pixel_unit.decimals(),
                    self.pixel_unit.short(),
                )
                .changed()
                {
                    let px =
                        self.pixel_unit
                            .to_pixels(value, self.ppi, f64::from(self.original_width));
                    self.set_width(px.round().max(1.0) as u32);
                }
            });
            design::inspector_field(ui, "Height", |ui| {
                let mut value = self.pixel_field(self.height, self.original_height);
                if numeric(
                    ui,
                    &mut value,
                    0.0..=1.0e7,
                    self.pixel_unit.decimals(),
                    self.pixel_unit.short(),
                )
                .changed()
                {
                    let px =
                        self.pixel_unit
                            .to_pixels(value, self.ppi, f64::from(self.original_height));
                    self.set_height(px.round().max(1.0) as u32);
                }
            });
            design::inspector_field(ui, "Units", |ui| {
                combo(
                    ui,
                    "is-pixel-unit",
                    &mut self.pixel_unit,
                    &[Unit::Pixels, Unit::Percent],
                    |u| u.label().to_string(),
                    |_| None,
                );
            });
        });
        if !editable {
            caption(
                ui,
                crate::strings::tr("ui.image_size.turn.on.resample.to.change.the"),
            );
        }

        design::section_header(ui, crate::strings::tr("ui.image_size.document.size"));
        design::inspector_field(ui, "Width", |ui| {
            let mut value = self.print_field(self.print_width_in());
            if numeric(
                ui,
                &mut value,
                0.0..=1.0e6,
                self.print_unit.decimals(),
                self.print_unit.short(),
            )
            .changed()
            {
                self.set_print_width_in(self.print_inches(value));
            }
        });
        design::inspector_field(ui, "Height", |ui| {
            let mut value = self.print_field(self.print_height_in());
            if numeric(
                ui,
                &mut value,
                0.0..=1.0e6,
                self.print_unit.decimals(),
                self.print_unit.short(),
            )
            .changed()
            {
                self.set_print_height_in(self.print_inches(value));
            }
        });
        design::inspector_field(ui, "Units", |ui| {
            combo(
                ui,
                "is-print-unit",
                &mut self.print_unit,
                Unit::PHYSICAL,
                |u| u.label().to_string(),
                |_| None,
            );
        });
        design::inspector_field(ui, "Resolution", |ui| {
            let mut value = self.resolution_unit.from_ppi(self.ppi);
            if numeric(ui, &mut value, 1.0..=MAX_PPI, 2, "").changed() {
                self.set_resolution_ppi(self.resolution_unit.to_ppi(value));
            }
            let mut unit = self.resolution_unit;
            if combo(
                ui,
                "is-res-unit",
                &mut unit,
                ResolutionUnit::ALL,
                |u| u.label().to_string(),
                |_| None,
            ) {
                self.resolution_unit = unit;
            }
        });

        design::section_header(ui, "Options");
        let mut constrain = self.constrain;
        if checkbox_row(
            ui,
            crate::strings::tr("ui.image_size.constrain.proportions"),
            &mut constrain,
        )
        .changed()
        {
            self.set_constrain(constrain);
        }
        let mut resample = self.resample;
        if checkbox_row(ui, "Resample", &mut resample).changed() {
            self.set_resample(resample);
        }
        ui.add_enabled_ui(self.resample, |ui| {
            design::inspector_field(ui, "Method", |ui| {
                combo(
                    ui,
                    "is-filter",
                    &mut self.filter,
                    FILTERS,
                    |f| filter_label(f).to_string(),
                    |_| None,
                );
            });
            caption(ui, filter_hint(self.filter));
        });

        if let Some(reason) = self.blocked_reason() {
            ui.add_space(Space::Small.pt());
            warning(ui, reason);
        }
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &["Reset"],
        )
    }

    /// A pixel count as the number the field shows, in the chosen pixel unit.
    /// `reference` is what 100% means for that side.
    fn pixel_field(&self, pixels: u32, reference: u32) -> f64 {
        self.pixel_unit
            .from_pixels(f64::from(pixels), self.ppi, f64::from(reference))
    }

    fn print_field(&self, inches: f64) -> f64 {
        inches / self.print_unit.inches_per().unwrap_or(1.0)
    }

    fn print_inches(&self, value: f64) -> f64 {
        value * self.print_unit.inches_per().unwrap_or(1.0)
    }
}

/// The resampling methods offered, sharpest last.
pub const FILTERS: &[ResampleFilter] = &[
    ResampleFilter::Nearest,
    ResampleFilter::Triangle,
    ResampleFilter::Mitchell,
    ResampleFilter::Lanczos3,
];

/// Menu label for a resampling method.
pub fn filter_label(filter: ResampleFilter) -> &'static str {
    match filter {
        ResampleFilter::Nearest => crate::strings::tr("ui.image_size.nearest.neighbour"),
        ResampleFilter::Triangle => "Bilinear",
        ResampleFilter::Mitchell => "Bicubic",
        ResampleFilter::Lanczos3 => "Lanczos",
    }
}

/// One line saying what a resampling method is *for*.
pub fn filter_hint(filter: ResampleFilter) -> &'static str {
    match filter {
        ResampleFilter::Nearest => {
            "Hard edges, no blending. Pixel art only — it aliases on downscale."
        }
        ResampleFilter::Triangle => crate::strings::tr("ui.image_size.soft.and.cheap.good.for.a"),
        ResampleFilter::Mitchell => {
            crate::strings::tr("ui.image_size.the.balanced.default.for.photographs")
        }
        ResampleFilter::Lanczos3 => {
            crate::strings::tr("ui.image_size.sharpest.with.a.little.ringing.on")
        }
    }
}

impl Dialog for ImageSizeDialog {
    fn title(&self) -> &'static str {
        crate::strings::tr("ui.image_size.image.size")
    }

    fn confirm_label(&self) -> &'static str {
        "Resize"
    }

    fn confirm(&self) -> Option<DialogAction> {
        let spec = self.spec();
        spec.is_valid().then_some(DialogAction::ResizeImage(spec))
    }

    fn blocked_reason(&self) -> Option<String> {
        let spec = self.spec();
        if spec.width < 1 || spec.height < 1 {
            return Some(
                crate::strings::tr("ui.image_size.width.and.height.must.be.at").to_string(),
            );
        }
        if spec.width > MAX_DIMENSION || spec.height > MAX_DIMENSION {
            return Some(format!("No side may exceed {MAX_DIMENSION} pixels"));
        }
        if u64::from(spec.width) * u64::from(spec.height) > MAX_PIXELS {
            return Some(format!(
                "{} x {} is over the {} pixel limit",
                spec.width, spec.height, MAX_PIXELS
            ));
        }
        if !(spec.resolution_ppi.is_finite() && spec.resolution_ppi > 0.0) {
            return Some(
                crate::strings::tr("ui.image_size.resolution.must.be.greater.than.zero")
                    .to_string(),
            );
        }
        (spec.resolution_ppi > MAX_PPI).then(|| format!("Resolution may not exceed {MAX_PPI} ppi"))
    }
}

/// The other side of a `major : minor` ratio, rounded half-up and never zero.
fn other_side(known: u32, known_ratio: u32, other_ratio: u32) -> u32 {
    if known_ratio == 0 {
        return known.max(1);
    }
    let numerator = u64::from(known) * u64::from(other_ratio);
    let denominator = u64::from(known_ratio);
    let value = (numerator + denominator / 2) / denominator;
    value.clamp(1, u64::from(MAX_DIMENSION)) as u32
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

fn sane_ppi(ppi: f64) -> f64 {
    if ppi.is_finite() && ppi > 0.0 {
        ppi.min(MAX_PPI)
    } else {
        super::units::DEFAULT_PPI
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::frame_both_themes;

    #[test]
    fn the_aspect_ratio_is_reduced_to_integers() {
        assert_eq!(
            ImageSizeDialog::new(1920, 1080, 72.0).aspect_ratio(),
            (16, 9)
        );
        assert_eq!(ImageSizeDialog::new(800, 600, 72.0).aspect_ratio(), (4, 3));
        assert_eq!(
            ImageSizeDialog::new(1000, 1000, 72.0).aspect_ratio(),
            (1, 1)
        );
        assert_eq!(
            ImageSizeDialog::new(1023, 769, 72.0).aspect_ratio(),
            (1023, 769)
        );
    }

    #[test]
    fn constrain_keeps_the_ratio_exactly_under_width_edits() {
        let mut dialog = ImageSizeDialog::new(1920, 1080, 72.0);
        for multiple in 1..=200u64 {
            let width = (multiple * 16) as u32;
            assert!(dialog.set_width(width));
            let (w, h) = (u64::from(dialog.width()), u64::from(dialog.height()));
            assert_eq!(
                w * 1080,
                h * 1920,
                "width {width} gave {w}x{h}, which is not 16:9"
            );
        }
    }

    #[test]
    fn constrain_keeps_the_ratio_exactly_under_height_edits() {
        let mut dialog = ImageSizeDialog::new(1920, 1080, 72.0);
        for multiple in 1..=200u64 {
            let height = (multiple * 9) as u32;
            assert!(dialog.set_height(height));
            let (w, h) = (u64::from(dialog.width()), u64::from(dialog.height()));
            assert_eq!(
                w * 1080,
                h * 1920,
                "height {height} gave {w}x{h}, which is not 16:9"
            );
        }
    }

    /// The test the two above cannot be: a *non-multiple*.
    ///
    /// The defect this pins is a hole in the coverage rather than in the code.
    /// Every constrain test fed widths that were exact multiples of 16 and
    /// heights that were exact multiples of 9, and at an exact multiple every
    /// implementation agrees — truncating float, rounding float and reduced
    /// integer alike. Replacing `other_side` with the truncating float ratio
    /// the module docs call out as wrong left all twenty tests green. The
    /// arithmetic the module exists to defend only shows itself between the
    /// multiples, so that is where it now gets asserted.
    #[test]
    fn constrain_rounds_the_other_side_instead_of_truncating_it() {
        // 1920x1080 reduces to 16:9. Exact answers, with what a truncating
        // implementation would say instead:
        //   1921 -> 1080.5625  (trunc 1080)
        //   1912 -> 1075.5     (trunc 1075; the half-way case)
        //   1927 -> 1083.9375  (trunc 1083)
        for (width, height) in [(1921u32, 1081u32), (1912, 1076), (1913, 1076), (1927, 1084)] {
            let mut dialog = ImageSizeDialog::new(1920, 1080, 72.0);
            assert!(dialog.set_width(width));
            assert_eq!(
                dialog.height(),
                height,
                "width {width} should give height {height}"
            );
        }
        //   1081 -> 1921.7778  (trunc 1921)
        //   1076 -> 1912.8889  (trunc 1912)
        //   1085 -> 1928.8889  (trunc 1928)
        for (height, width) in [(1081u32, 1922u32), (1076, 1913), (1085, 1929)] {
            let mut dialog = ImageSizeDialog::new(1920, 1080, 72.0);
            assert!(dialog.set_height(height));
            assert_eq!(
                dialog.width(),
                width,
                "height {height} should give width {width}"
            );
        }
    }

    /// The property behind those numbers, over every width in a range that is
    /// mostly non-multiples: the answer is the *nearest* integer, so the ratio
    /// error never exceeds half a ratio unit. Truncation's error reaches a
    /// whole one.
    #[test]
    fn the_locked_side_is_always_the_nearest_integer() {
        let (ratio_w, ratio_h) = (16i64, 9i64);
        for width in 1000..1200u32 {
            let mut dialog = ImageSizeDialog::new(1920, 1080, 72.0);
            assert!(dialog.set_width(width));
            let error = i64::from(dialog.height()) * ratio_w - i64::from(width) * ratio_h;
            assert!(
                2 * error.abs() <= ratio_w,
                "width {width} gave height {}, off the exact ratio by {}/{ratio_w}",
                dialog.height(),
                error
            );
        }
    }

    /// And the drift the module doc names: the ratio itself is stored, so a
    /// detour through a non-multiple leaves it untouched and the original size
    /// is reachable again exactly.
    #[test]
    fn a_non_multiple_detour_does_not_move_the_stored_ratio() {
        let mut dialog = ImageSizeDialog::new(1920, 1080, 72.0);
        for width in [1921, 907, 1913, 33, 1912] {
            assert!(dialog.set_width(width));
            assert_eq!(
                dialog.aspect_ratio(),
                (16, 9),
                "editing to {width} re-derived the lock from the size on screen"
            );
        }
        assert!(dialog.set_width(1920));
        assert_eq!((dialog.width(), dialog.height()), (1920, 1080));
    }

    #[test]
    fn constrain_survives_a_hundred_alternating_edits_without_drifting() {
        // A float ratio drifts here; the reduced integer ratio does not.
        let mut dialog = ImageSizeDialog::new(800, 600, 72.0);
        for step in 1..=100u32 {
            dialog.set_width(step * 4);
            dialog.set_height(dialog.height());
            assert_eq!(dialog.width() * 3, dialog.height() * 4);
        }
    }

    #[test]
    fn unconstrained_edits_move_one_side_only() {
        let mut dialog = ImageSizeDialog::new(1920, 1080, 72.0);
        dialog.set_constrain(false);
        dialog.set_width(400);
        assert_eq!((dialog.width(), dialog.height()), (400, 1080));
        dialog.set_height(50);
        assert_eq!((dialog.width(), dialog.height()), (400, 50));
    }

    #[test]
    fn re_locking_adopts_the_size_on_screen_as_the_new_ratio() {
        let mut dialog = ImageSizeDialog::new(1920, 1080, 72.0);
        dialog.set_constrain(false);
        dialog.set_width(1000);
        dialog.set_height(500);
        dialog.set_constrain(true);
        assert_eq!(dialog.aspect_ratio(), (2, 1));
        dialog.set_width(300);
        assert_eq!(dialog.height(), 150);
    }

    #[test]
    fn a_side_never_collapses_to_zero() {
        let mut dialog = ImageSizeDialog::new(4000, 10, 72.0);
        dialog.set_width(1);
        assert!(dialog.height() >= 1);
        assert!(dialog.width() >= 1);
    }

    #[test]
    fn with_resample_off_the_pixel_count_is_not_editable() {
        let mut dialog = ImageSizeDialog::new(1920, 1080, 300.0);
        dialog.set_resample(false);
        assert!(!dialog.pixels_editable());
        assert!(!dialog.set_width(400));
        assert_eq!(dialog.width(), 1920);
        assert!(!dialog.set_height(400));
        assert_eq!(dialog.height(), 1080);
    }

    #[test]
    fn with_resample_off_resolution_moves_the_printed_size_only() {
        let mut dialog = ImageSizeDialog::new(1200, 600, 300.0);
        dialog.set_resample(false);
        assert!((dialog.print_width_in() - 4.0).abs() < 1e-9);
        dialog.set_resolution_ppi(150.0);
        assert_eq!((dialog.width(), dialog.height()), (1200, 600));
        assert!((dialog.print_width_in() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn with_resample_on_resolution_moves_the_pixel_count() {
        let mut dialog = ImageSizeDialog::new(1200, 600, 300.0);
        assert!(dialog.resample());
        dialog.set_resolution_ppi(150.0);
        assert_eq!((dialog.width(), dialog.height()), (600, 300));
        assert!((dialog.print_width_in() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn with_resample_off_a_printed_width_sets_the_resolution() {
        let mut dialog = ImageSizeDialog::new(1200, 600, 300.0);
        dialog.set_resample(false);
        dialog.set_print_width_in(6.0);
        assert_eq!(dialog.width(), 1200);
        assert!((dialog.resolution_ppi() - 200.0).abs() < 1e-9);
    }

    #[test]
    fn with_resample_on_a_printed_width_sets_the_pixel_count() {
        let mut dialog = ImageSizeDialog::new(1200, 600, 300.0);
        dialog.set_print_width_in(2.0);
        assert_eq!(dialog.width(), 600);
        assert_eq!(dialog.height(), 300);
    }

    #[test]
    fn turning_resampling_off_restores_the_original_pixels() {
        let mut dialog = ImageSizeDialog::new(1200, 600, 300.0);
        dialog.set_width(600);
        assert_eq!(dialog.width(), 600);
        dialog.set_resample(false);
        assert_eq!((dialog.width(), dialog.height()), (1200, 600));
        // The printed size the user had reached is preserved.
        assert!((dialog.print_width_in() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn the_file_size_estimate_follows_the_pixel_count() {
        let mut dialog = ImageSizeDialog::new(1000, 1000, 72.0);
        assert_eq!(dialog.original_bytes(), 1000 * 1000 * 4);
        dialog.set_width(500);
        assert_eq!(dialog.spec().flat_bytes(), 500 * 500 * 4);
    }

    #[test]
    fn a_metadata_only_change_does_not_claim_to_resample() {
        let mut dialog = ImageSizeDialog::new(1200, 600, 300.0);
        dialog.set_resample(false);
        dialog.set_resolution_ppi(150.0);
        assert_eq!(dialog.spec().resample, None);
    }

    #[test]
    fn a_pixel_change_names_the_filter_it_will_use() {
        let mut dialog = ImageSizeDialog::new(1200, 600, 300.0);
        dialog.set_filter(ResampleFilter::Mitchell);
        dialog.set_width(600);
        assert_eq!(dialog.spec().resample, Some(ResampleFilter::Mitchell));
    }

    #[test]
    fn reset_puts_the_document_back() {
        let mut dialog = ImageSizeDialog::new(1200, 600, 300.0);
        dialog.set_width(64);
        dialog.set_resolution_ppi(9.0);
        dialog.reset();
        assert_eq!((dialog.width(), dialog.height()), (1200, 600));
        assert_eq!(dialog.resolution_ppi(), 300.0);
    }

    #[test]
    fn confirm_produces_a_valid_spec_and_cancel_produces_nothing() {
        let dialog = ImageSizeDialog::new(1200, 600, 300.0);
        let action = dialog.confirm().expect("a default size is confirmable");
        assert!(action.is_valid());
        assert_eq!(
            super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn every_resampling_method_has_a_label_and_a_hint() {
        for filter in FILTERS {
            assert!(!filter_label(*filter).is_empty());
            assert!(!filter_hint(*filter).is_empty());
        }
    }

    #[test]
    fn it_draws_in_both_appearances() {
        frame_both_themes(|ctx| {
            let mut dialog = ImageSizeDialog::new(1920, 1080, 72.0);
            assert!(dialog.show(ctx).is_open());
            let mut locked = ImageSizeDialog::new(1920, 1080, 72.0);
            locked.set_resample(false);
            assert!(locked.show(ctx).is_open());
        });
    }
}
