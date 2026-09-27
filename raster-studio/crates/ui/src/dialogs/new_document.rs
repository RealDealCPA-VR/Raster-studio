//! New Document.
//!
//! A preset list on the left, the resolved specification on the right. Picking
//! a preset writes into the same fields the user can type into, so there is
//! never a hidden "preset mode" — editing a field simply drops the preset
//! highlight and keeps the numbers.

use color::ColorSpace;
use design::tokens::Space;
use egui::Context;
use raster::BitDepth;

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, Dialog, DialogButton, DialogOutcome, DialogWidth,
};
use super::color_edit::ColorEdit;
use super::color_picker::ScreenSampler;
use super::controls::{checkbox_row, combo, numeric, readout, swatch};
use super::units::{format_bytes, ResolutionUnit, Unit, DEFAULT_PPI, MAX_PPI};
use super::{ids, sizes};

/// Largest side a new document may have, in pixels.
///
/// **Defined as the engine's own limit, not a number of its own.** The dialog
/// used to carry independent constants, so it could describe a document the
/// loader would refuse — a dialog that offers what the application cannot build
/// is a dialog that lies. `the_dialog_cannot_describe_a_document_the_engine_refuses`
/// is the executable version of that.
pub const MAX_DIMENSION: u32 = editor_core::MAX_CANVAS_DIMENSION;
/// Largest area a new document may have, in pixels. A 300000 x 300000 document
/// is 90 gigapixels; the area cap is what actually keeps the request sane.
pub const MAX_PIXELS: u64 = editor_core::MAX_CANVAS_PIXELS;

/// How a new document's channels are interpreted.
///
/// Only [`ColorMode::Rgb`] is backed by the document model today. The other two
/// are listed and rendered **disabled with a reason** rather than hidden,
/// because silently omitting them tells the user they were never planned; and
/// an enabled item that produced an RGB document anyway would be a lie.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ColorMode {
    #[default]
    Rgb,
    Grayscale,
    Cmyk,
}

impl ColorMode {
    /// Every mode, in menu order.
    pub const ALL: &'static [ColorMode] = &[Self::Rgb, Self::Grayscale, Self::Cmyk];

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rgb => "RGB Color",
            Self::Grayscale => "Grayscale",
            Self::Cmyk => "CMYK Color",
        }
    }

    /// Why this mode cannot be chosen, or `None` when it can.
    pub const fn unavailable(self) -> Option<&'static str> {
        match self {
            Self::Rgb => None,
            Self::Grayscale => Some("Grayscale documents are not supported yet — create an RGB document and use a Black & White adjustment"),
            Self::Cmyk => Some("CMYK documents are not supported yet — the compositor works in RGB"),
        }
    }

    /// Channels stored per pixel, used for the memory estimate.
    pub const fn channels(self) -> u32 {
        match self {
            Self::Rgb => 4,
            Self::Grayscale => 2,
            Self::Cmyk => 5,
        }
    }
}

/// What the background layer of a new document is filled with.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum BackgroundContents {
    #[default]
    White,
    Black,
    /// No background layer at all.
    Transparent,
    /// A straight-alpha RGBA colour in 0..=1.
    Custom([f32; 4]),
}

impl BackgroundContents {
    /// The choices offered, with [`BackgroundContents::Custom`] carrying
    /// whatever colour the dialog currently holds.
    pub const MENU: &'static [BackgroundContents] = &[
        Self::White,
        Self::Black,
        Self::Transparent,
        Self::Custom([1.0, 1.0, 1.0, 1.0]),
    ];

    /// Menu label. `Custom` does not print its colour — the swatch beside the
    /// menu shows it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::White => "White",
            Self::Black => "Black",
            Self::Transparent => "Transparent",
            Self::Custom(_) => "Custom",
        }
    }

    /// Whether two choices are the same *menu entry*, ignoring a custom
    /// colour's value. Needed because the menu compares by identity and a
    /// custom colour changes while it stays selected.
    pub fn same_entry(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::White, Self::White)
                | (Self::Black, Self::Black)
                | (Self::Transparent, Self::Transparent)
                | (Self::Custom(_), Self::Custom(_))
        )
    }

    /// The straight-alpha fill, or `None` for a document with no background.
    pub fn fill(self) -> Option<[f32; 4]> {
        match self {
            Self::White => Some([1.0, 1.0, 1.0, 1.0]),
            Self::Black => Some([0.0, 0.0, 0.0, 1.0]),
            Self::Transparent => None,
            Self::Custom(rgba) => Some(rgba),
        }
    }
}

/// Which shelf of the preset list a preset sits on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PresetGroup {
    Screen,
    Print,
    Social,
    // W18-G: Photopea's other New-dialog shelves.
    Photo,
    Mobile,
    Ads,
    /// Photopea's "2ᴺ": power-of-two squares (textures, icons).
    PowersOfTwo,
}

impl PresetGroup {
    /// Every group, in list order.
    pub const ALL: &'static [PresetGroup] = &[
        Self::Screen,
        Self::Print,
        Self::Social,
        // W18-G
        Self::Photo,
        Self::Mobile,
        Self::Ads,
        Self::PowersOfTwo,
    ];

    /// Section header text.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Screen => "Screen",
            Self::Print => "Print",
            Self::Social => "Social",
            Self::Photo => "Photo",
            Self::Mobile => "Mobile",
            Self::Ads => "Ads",
            // Photopea writes 2ᴺ; the superscript N is not in the UI font.
            Self::PowersOfTwo => "2^N",
        }
    }
}

/// One named starting point.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DocumentPreset {
    pub group: PresetGroup,
    pub name: &'static str,
    pub width: f64,
    pub height: f64,
    pub unit: Unit,
    pub ppi: f64,
    pub background: BackgroundContents,
}

/// The built-in presets.
pub const PRESETS: &[DocumentPreset] = &[
    px("Web 1280 x 720", PresetGroup::Screen, 1280.0, 720.0),
    px("Web 1920 x 1080", PresetGroup::Screen, 1920.0, 1080.0),
    px("Web 2560 x 1440", PresetGroup::Screen, 2560.0, 1440.0),
    px("UHD 3840 x 2160", PresetGroup::Screen, 3840.0, 2160.0),
    px("iPhone 15 Pro", PresetGroup::Screen, 1179.0, 2556.0),
    px("iPad Pro 11\"", PresetGroup::Screen, 1668.0, 2388.0),
    print("Letter", 8.5, 11.0),
    print("Legal", 8.5, 14.0),
    print("Tabloid", 11.0, 17.0),
    print_mm("A4", 210.0, 297.0),
    print_mm("A3", 297.0, 420.0),
    print_mm("A5", 148.0, 210.0),
    print("Photo 4 x 6", 4.0, 6.0),
    print("Photo 8 x 10", 8.0, 10.0),
    px("Instagram Post", PresetGroup::Social, 1080.0, 1080.0),
    px("Instagram Story", PresetGroup::Social, 1080.0, 1920.0),
    px("YouTube Thumbnail", PresetGroup::Social, 1280.0, 720.0),
    px("Facebook Cover", PresetGroup::Social, 1640.0, 856.0),
    px("X Header", PresetGroup::Social, 1500.0, 500.0),
    px("LinkedIn Banner", PresetGroup::Social, 1584.0, 396.0),
    // ---- W18-G: Photopea's Photo, Mobile, Ads and 2ᴺ shelves, its values.
    photo("Wallet", 2.0, 3.0),
    photo("Enprint", 3.5, 5.0),
    photo("5 x 7 in", 5.0, 7.0),
    photo("8 x 12 in", 8.0, 12.0),
    photo("12 x 18 in", 12.0, 18.0),
    photo("16 x 24 in", 16.0, 24.0),
    photo("20 x 30 in", 20.0, 30.0),
    photo("24 x 36 in", 24.0, 36.0),
    mobile("iPhone 4", 640.0, 900.0, 326.0),
    mobile("iPhone 5", 640.0, 1136.0, 326.0),
    mobile("iPhone 6 7 8", 750.0, 1334.0, 326.0),
    mobile("iPhone 6+ 7+ 8+", 1080.0, 1920.0, 401.0),
    mobile("iPhone X", 1125.0, 2436.0, 458.0),
    mobile("Samsung A5", 720.0, 1280.0, 72.0),
    mobile("Samsung S5", 1080.0, 1920.0, 72.0),
    mobile("Samsung S6", 1440.0, 2560.0, 72.0),
    mobile("Samsung S8+", 1440.0, 2560.0, 72.0),
    mobile("Google Pixel", 1080.0, 1920.0, 72.0),
    mobile("iPad Mini 4", 1536.0, 2048.0, 72.0),
    mobile("iPad Air 2", 1536.0, 2048.0, 72.0),
    mobile("iPad Pro 9.7\"", 1536.0, 2048.0, 72.0),
    mobile("iPad Pro 10.5\"", 1668.0, 2224.0, 72.0),
    mobile("iPad Pro 12.9\"", 2048.0, 2732.0, 72.0),
    px("Small Square", PresetGroup::Ads, 200.0, 200.0),
    px("Square", PresetGroup::Ads, 250.0, 250.0),
    px("Medium Rect", PresetGroup::Ads, 300.0, 250.0),
    px("Large Rect", PresetGroup::Ads, 336.0, 280.0),
    px("Mobile Leaderboard", PresetGroup::Ads, 320.0, 50.0),
    px("Large Mobile", PresetGroup::Ads, 320.0, 100.0),
    px("Banner", PresetGroup::Ads, 468.0, 60.0),
    px("Leaderboard", PresetGroup::Ads, 728.0, 90.0),
    px("Large Leaderboard", PresetGroup::Ads, 970.0, 90.0),
    px("Billboard", PresetGroup::Ads, 970.0, 250.0),
    px("Vertical Rect", PresetGroup::Ads, 240.0, 400.0),
    px("Skyscraper", PresetGroup::Ads, 120.0, 600.0),
    px("Wide Skyscraper", PresetGroup::Ads, 160.0, 600.0),
    px("Half Page", PresetGroup::Ads, 300.0, 600.0),
    px("Portrait", PresetGroup::Ads, 300.0, 1050.0),
    px("16 x 16", PresetGroup::PowersOfTwo, 16.0, 16.0),
    px("32 x 32", PresetGroup::PowersOfTwo, 32.0, 32.0),
    px("64 x 64", PresetGroup::PowersOfTwo, 64.0, 64.0),
    px("128 x 128", PresetGroup::PowersOfTwo, 128.0, 128.0),
    px("256 x 256", PresetGroup::PowersOfTwo, 256.0, 256.0),
    px("512 x 512", PresetGroup::PowersOfTwo, 512.0, 512.0),
    px("1024 x 1024", PresetGroup::PowersOfTwo, 1024.0, 1024.0),
    px("2048 x 2048", PresetGroup::PowersOfTwo, 2048.0, 2048.0),
    px("4096 x 4096", PresetGroup::PowersOfTwo, 4096.0, 4096.0),
];

/// W18-G: a Photopea Photo preset, in inches at 300 ppi.
const fn photo(name: &'static str, w: f64, h: f64) -> DocumentPreset {
    DocumentPreset {
        group: PresetGroup::Photo,
        ..print(name, w, h)
    }
}

/// W18-G: a Photopea Mobile preset, in pixels at the device's own ppi.
const fn mobile(name: &'static str, w: f64, h: f64, ppi: f64) -> DocumentPreset {
    DocumentPreset {
        ppi,
        ..px(name, PresetGroup::Mobile, w, h)
    }
}

const fn px(name: &'static str, group: PresetGroup, w: f64, h: f64) -> DocumentPreset {
    DocumentPreset {
        group,
        name,
        width: w,
        height: h,
        unit: Unit::Pixels,
        ppi: 72.0,
        background: BackgroundContents::White,
    }
}

const fn print(name: &'static str, w: f64, h: f64) -> DocumentPreset {
    DocumentPreset {
        group: PresetGroup::Print,
        name,
        width: w,
        height: h,
        unit: Unit::Inches,
        ppi: 300.0,
        background: BackgroundContents::White,
    }
}

const fn print_mm(name: &'static str, w: f64, h: f64) -> DocumentPreset {
    DocumentPreset {
        group: PresetGroup::Print,
        name,
        width: w,
        height: h,
        unit: Unit::Millimeters,
        ppi: 300.0,
        background: BackgroundContents::White,
    }
}

/// Everything needed to create a document, already resolved to pixels.
#[derive(Clone, PartialEq, Debug)]
pub struct NewDocumentSpec {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub resolution_ppi: f64,
    pub color_mode: ColorMode,
    pub color_space: ColorSpace,
    pub bit_depth: BitDepth,
    pub background: BackgroundContents,
    /// W10-J: the Artboard box — the canvas becomes an artboard ("Artboard
    /// 1") whose background is [`NewDocumentSpec::background`].
    pub artboard: bool,
}

impl NewDocumentSpec {
    /// Whether this specification can actually be built.
    pub fn is_valid(&self) -> bool {
        self.width >= 1
            && self.height >= 1
            && self.width <= MAX_DIMENSION
            && self.height <= MAX_DIMENSION
            && u64::from(self.width) * u64::from(self.height) <= MAX_PIXELS
            && self.resolution_ppi.is_finite()
            && self.resolution_ppi > 0.0
            && self.resolution_ppi <= MAX_PPI
            && self.color_mode.unavailable().is_none()
            && !self.title.trim().is_empty()
    }

    /// Bytes one flat layer of this document occupies in memory.
    pub fn flat_bytes(&self) -> u64 {
        let depth = match self.bit_depth {
            BitDepth::Eight => 1,
            BitDepth::Sixteen => 2,
        };
        u64::from(self.width)
            * u64::from(self.height)
            * u64::from(self.color_mode.channels())
            * depth
    }
}

/// The New Document dialog.
#[derive(Clone, Debug)]
pub struct NewDocumentDialog {
    title: String,
    /// The width and height as typed, in `unit`.
    width: f64,
    height: f64,
    unit: Unit,
    resolution: f64,
    resolution_unit: ResolutionUnit,
    color_mode: ColorMode,
    color_space: ColorSpace,
    bit_depth: BitDepth,
    background: BackgroundContents,
    custom_background: [f32; 4],
    /// W10-J: the Artboard box.
    artboard: bool,
    /// Index into [`PRESETS`], dropped as soon as a field is edited by hand.
    preset: Option<usize>,
    /// The nested colour picker, when the Custom swatch is clicked.
    color_edit: ColorEdit<()>,
}

impl Default for NewDocumentDialog {
    fn default() -> Self {
        let mut dialog = Self {
            title: "Untitled".to_string(),
            width: 1920.0,
            height: 1080.0,
            unit: Unit::Pixels,
            resolution: DEFAULT_PPI,
            resolution_unit: ResolutionUnit::PerInch,
            color_mode: ColorMode::Rgb,
            color_space: ColorSpace::Srgb,
            bit_depth: BitDepth::Eight,
            background: BackgroundContents::White,
            custom_background: [1.0, 1.0, 1.0, 1.0],
            artboard: false,
            preset: None,
            color_edit: ColorEdit::new(),
        };
        dialog.apply_preset(1);
        dialog
    }
}

impl NewDocumentDialog {
    /// The units this dialog offers. Percent is excluded: there is no existing
    /// document for it to be a percentage *of*.
    pub fn units() -> Vec<Unit> {
        Unit::ALL
            .iter()
            .copied()
            .filter(|u| *u != Unit::Percent)
            .collect()
    }

    /// Load preset `index`, leaving the dialog untouched if it is out of range.
    pub fn apply_preset(&mut self, index: usize) {
        let Some(preset) = PRESETS.get(index) else {
            return;
        };
        self.width = preset.width;
        self.height = preset.height;
        self.unit = preset.unit;
        self.resolution = self.resolution_unit.from_ppi(preset.ppi);
        self.background = preset.background;
        self.preset = Some(index);
    }

    /// The preset currently loaded, if the user has not edited since.
    pub fn preset(&self) -> Option<usize> {
        self.preset
    }

    /// Resolution in pixels per inch, whatever unit it was typed in.
    pub fn resolution_ppi(&self) -> f64 {
        self.resolution_unit.to_ppi(self.resolution)
    }

    /// The width in pixels, rounded and clamped to a legal document size.
    pub fn pixel_width(&self) -> u32 {
        to_pixels(self.width, self.unit, self.resolution_ppi())
    }

    /// The height in pixels, rounded and clamped to a legal document size.
    pub fn pixel_height(&self) -> u32 {
        to_pixels(self.height, self.unit, self.resolution_ppi())
    }

    /// Set the width from a pixel count, converting into the current unit.
    pub fn set_pixel_width(&mut self, px: f64) {
        self.width = self.unit.from_pixels(px, self.resolution_ppi(), 0.0);
        self.preset = None;
    }

    /// Set the height from a pixel count, converting into the current unit.
    pub fn set_pixel_height(&mut self, px: f64) {
        self.height = self.unit.from_pixels(px, self.resolution_ppi(), 0.0);
        self.preset = None;
    }

    /// Switch the unit the size fields are typed in, keeping the *pixel* size.
    ///
    /// Switching from inches to pixels must not resize the document; only the
    /// number in the box changes.
    pub fn set_unit(&mut self, unit: Unit) {
        if unit == self.unit {
            return;
        }
        let ppi = self.resolution_ppi();
        let (w_px, h_px) = (
            self.unit.to_pixels(self.width, ppi, 0.0),
            self.unit.to_pixels(self.height, ppi, 0.0),
        );
        self.unit = unit;
        self.width = unit.from_pixels(w_px, ppi, 0.0);
        self.height = unit.from_pixels(h_px, ppi, 0.0);
    }

    /// Switch the resolution unit, keeping the resolution itself.
    pub fn set_resolution_unit(&mut self, unit: ResolutionUnit) {
        if unit == self.resolution_unit {
            return;
        }
        let ppi = self.resolution_ppi();
        self.resolution_unit = unit;
        self.resolution = unit.from_ppi(ppi);
    }

    /// Swap width and height.
    pub fn swap_orientation(&mut self) {
        std::mem::swap(&mut self.width, &mut self.height);
        self.preset = None;
    }

    /// What the background layer is filled with.
    ///
    /// [`BackgroundContents::Custom`] always reports the colour the dialog
    /// holds, whatever colour the menu entry was constructed with.
    pub fn background(&self) -> BackgroundContents {
        match self.background {
            BackgroundContents::Custom(_) => BackgroundContents::Custom(self.custom_background),
            other => other,
        }
    }

    /// Choose the background.
    ///
    /// The one writer of this field. A second control that also wrote it — the
    /// "Transparent background" checkbox this dialog used to carry — could not
    /// know what the menu had chosen, so unticking it reset a Black or Custom
    /// background to White. One field, one control.
    pub fn set_background(&mut self, background: BackgroundContents) {
        if let BackgroundContents::Custom(rgba) = background {
            self.custom_background = rgba;
        }
        self.background = background;
    }

    /// The custom background colour, whether or not it is selected.
    pub fn custom_background(&self) -> [f32; 4] {
        self.custom_background
    }

    /// The nested colour picker, when the Custom swatch has been clicked.
    pub fn color_edit(&self) -> &ColorEdit<()> {
        &self.color_edit
    }

    /// Mutable access to it.
    pub fn color_edit_mut(&mut self) -> &mut ColorEdit<()> {
        &mut self.color_edit
    }

    /// The colour mode; refuses a mode the document model cannot represent.
    pub fn set_color_mode(&mut self, mode: ColorMode) -> bool {
        if mode.unavailable().is_some() {
            return false;
        }
        self.color_mode = mode;
        true
    }

    /// The document specification this dialog currently describes.
    pub fn spec(&self) -> NewDocumentSpec {
        NewDocumentSpec {
            title: self.title.clone(),
            width: self.pixel_width(),
            height: self.pixel_height(),
            resolution_ppi: self.resolution_ppi(),
            color_mode: self.color_mode,
            color_space: self.color_space.clone(),
            bit_depth: self.bit_depth,
            background: self.background(),
            artboard: self.artboard,
        }
    }

    /// W10-J: tick or untick the Artboard box, as a click on it does.
    pub fn set_artboard(&mut self, on: bool) {
        self.artboard = on;
    }

    /// Draw the dialog for one frame.
    ///
    /// `sampler` reaches the nested colour picker's eyedropper; `None` draws
    /// that button disabled with its reason.
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        let nested = self.color_edit.is_open();
        let keys = if nested {
            super::chrome::DialogKeys::NONE
        } else {
            super::chrome::DialogKeys::read(ctx)
        };
        let mut outcome = super::chrome::resolve(self, keys);
        let drawn = modal(
            ctx,
            "new-document",
            self.title(),
            Some("Choose a preset, or type an exact size."),
            DialogWidth::Wide,
            |ui| self.body(ui),
        );
        if let Some(((), rgba)) = self
            .color_edit
            .show(ctx, "new-document-background", sampler)
        {
            self.set_background(BackgroundContents::Custom(rgba));
        }
        if nested {
            return DialogOutcome::Open;
        }
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
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(sizes::sidebar_width());
                egui::ScrollArea::vertical()
                    .max_height(sizes::list_max_height())
                    .show(ui, |ui| self.preset_list(ui));
            });
            ui.add_space(Space::Large.pt());
            ui.vertical(|ui| self.fields(ui));
        });
        hairline(ui);
        let spec = self.spec();
        ui.horizontal(|ui| {
            readout(
                ui,
                format!(
                    "{} x {} px  ·  {}",
                    spec.width,
                    spec.height,
                    format_bytes(spec.flat_bytes())
                ),
            );
        });
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }

    fn preset_list(&mut self, ui: &mut egui::Ui) {
        for group in PresetGroup::ALL {
            design::section_header(ui, group.label());
            for (index, preset) in PRESETS.iter().enumerate() {
                if preset.group != *group {
                    continue;
                }
                if design::list_row(ui, preset.name, self.preset == Some(index)).clicked() {
                    self.apply_preset(index);
                }
            }
        }
    }

    fn fields(&mut self, ui: &mut egui::Ui) {
        design::inspector_field(ui, "Name", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.title).desired_width(sizes::text_field_name()),
            )
        });
        let units = Self::units();
        let mut unit = self.unit;
        design::inspector_field(ui, "Width", |ui| {
            let decimals = self.unit.decimals();
            if numeric(
                ui,
                &mut self.width,
                0.0..=1.0e7,
                decimals,
                self.unit.short(),
            )
            .changed()
            {
                self.preset = None;
            }
        });
        design::inspector_field(ui, "Height", |ui| {
            let decimals = self.unit.decimals();
            if numeric(
                ui,
                &mut self.height,
                0.0..=1.0e7,
                decimals,
                self.unit.short(),
            )
            .changed()
            {
                self.preset = None;
            }
            if design::ghost_button(ui, "Swap").clicked() {
                self.swap_orientation();
            }
        });
        design::inspector_field(ui, "Units", |ui| {
            if combo(
                ui,
                "nd-unit",
                &mut unit,
                &units,
                |u| u.label().to_string(),
                |_| None,
            ) {
                self.set_unit(unit);
            }
        });
        design::inspector_field(ui, "Resolution", |ui| {
            if numeric(ui, &mut self.resolution, 1.0..=MAX_PPI, 2, "").changed() {
                self.preset = None;
            }
            let mut res_unit = self.resolution_unit;
            if combo(
                ui,
                "nd-res-unit",
                &mut res_unit,
                ResolutionUnit::ALL,
                |u| u.label().to_string(),
                |_| None,
            ) {
                self.set_resolution_unit(res_unit);
            }
        });
        design::inspector_field(ui, "Color mode", |ui| {
            let mut mode = self.color_mode;
            if combo(
                ui,
                "nd-mode",
                &mut mode,
                ColorMode::ALL,
                |m| m.label().to_string(),
                ColorMode::unavailable,
            ) {
                self.set_color_mode(mode);
            }
        });
        design::inspector_field(ui, "Bit depth", |ui| {
            combo(
                ui,
                "nd-depth",
                &mut self.bit_depth,
                &[BitDepth::Eight, BitDepth::Sixteen],
                |d| match d {
                    BitDepth::Eight => "8 bit".to_string(),
                    BitDepth::Sixteen => "16 bit".to_string(),
                },
                |_| None,
            );
        });
        design::inspector_field(ui, "Profile", |ui| {
            let spaces = [
                ColorSpace::Srgb,
                ColorSpace::DisplayP3,
                ColorSpace::LinearSrgb,
            ];
            let mut index = spaces
                .iter()
                .position(|s| *s == self.color_space)
                .unwrap_or(0);
            if combo(
                ui,
                "nd-space",
                &mut index,
                &[0, 1, 2],
                |i| spaces[i].name().to_string(),
                |_| None,
            ) {
                self.color_space = spaces[index].clone();
            }
        });
        let mut chosen: Option<BackgroundContents> = None;
        let mut open_picker = false;
        design::inspector_field(ui, "Background", |ui| {
            let entries = BackgroundContents::MENU;
            let mut index = entries
                .iter()
                .position(|e| e.same_entry(self.background))
                .unwrap_or(0);
            if combo(
                ui,
                "nd-bg",
                &mut index,
                &[0, 1, 2, 3],
                |i| entries[i].label().to_string(),
                |_| None,
            ) {
                // Custom keeps the colour the dialog already holds; the menu
                // entry's own placeholder is not a choice the user made.
                chosen = Some(match entries[index] {
                    BackgroundContents::Custom(_) => {
                        BackgroundContents::Custom(self.custom_background)
                    }
                    other => other,
                });
            }
            if matches!(self.background, BackgroundContents::Custom(_)) {
                open_picker = swatch(
                    ui,
                    ids::custom_background("new-document"),
                    self.custom_background,
                    sizes::swatch_square(),
                )
                .clicked();
            }
        });
        if let Some(background) = chosen {
            self.set_background(background);
        }
        if open_picker {
            self.color_edit.open((), self.custom_background);
        }
        // W10-J: Photoshop's Artboards box: the canvas becomes an artboard
        // with the background above as its colour. The box's widget id is
        // parked under [`artboard_box_key`] so a test clicks the real rectangle.
        let artboard = design::inspector_field(ui, "Artboard", |ui| {
            checkbox_row(ui, "", &mut self.artboard)
        });
        ui.ctx()
            .data_mut(|d| d.insert_temp(artboard_box_key(), artboard.inner.id));
        if let Some(reason) = self.color_mode.unavailable() {
            caption(ui, reason);
        }
    }
}

/// W10-J: where [`NewDocumentDialog`] parks its Artboard box's widget id
/// (egui temp data), for a test to find the drawn checkbox.
pub fn artboard_box_key() -> egui::Id {
    egui::Id::new(("new-document", "artboard-box"))
}

/// A typed length resolved to whole pixels.
///
/// `0` stands for "not a usable size" — a negative, non-finite or sub-pixel
/// entry — and is what [`NewDocumentDialog::blocked_reason`] turns into the
/// "at least 1 pixel" message. The float-to-int cast saturates rather than
/// wrapping, so an absurd entry surfaces as an out-of-range size instead of a
/// small one.
fn to_pixels(value: f64, unit: Unit, ppi: f64) -> u32 {
    let px = unit.to_pixels(value, ppi, 0.0);
    if !px.is_finite() || px < 1.0 {
        return 0;
    }
    px.round() as u32
}

impl Dialog for NewDocumentDialog {
    fn title(&self) -> &'static str {
        "New Document"
    }

    fn confirm_label(&self) -> &'static str {
        "Create"
    }

    fn confirm(&self) -> Option<DialogAction> {
        // `blocked_reason` is the one gate — every condition that must refuse
        // the confirm lives there, so `confirm()` and the disabled primary
        // button cannot disagree about what is committable.
        self.blocked_reason()
            .is_none()
            .then(|| DialogAction::NewDocument(Box::new(self.spec())))
    }

    fn blocked_reason(&self) -> Option<String> {
        let spec = self.spec();
        if spec.title.trim().is_empty() {
            return Some("Give the document a name".to_string());
        }
        if spec.width < 1 || spec.height < 1 {
            return Some("Width and height must be at least 1 pixel".to_string());
        }
        if spec.width > MAX_DIMENSION || spec.height > MAX_DIMENSION {
            return Some(format!("No side may exceed {MAX_DIMENSION} pixels"));
        }
        if u64::from(spec.width) * u64::from(spec.height) > MAX_PIXELS {
            return Some(format!(
                "{} x {} is {} pixels, over the {} pixel limit",
                spec.width,
                spec.height,
                u64::from(spec.width) * u64::from(spec.height),
                MAX_PIXELS
            ));
        }
        if !(spec.resolution_ppi.is_finite() && spec.resolution_ppi > 0.0) {
            return Some("Resolution must be greater than zero".to_string());
        }
        if spec.resolution_ppi > MAX_PPI {
            return Some(format!("Resolution may not exceed {MAX_PPI} ppi"));
        }
        // 16 bits per channel is accepted: the compositor reads RGBA16 tiles
        // at their own depth (W3-H), the base layer is created from RGBA16
        // tiles, and paint lands back at 16 bits (W4-F, `doc_depth.rs`).
        spec.color_mode.unavailable().map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::{frame_both_themes, Harness};

    #[test]
    fn the_default_is_a_real_document() {
        let dialog = NewDocumentDialog::default();
        assert!(dialog.spec().is_valid());
        assert_eq!(dialog.pixel_width(), 1920);
        assert_eq!(dialog.pixel_height(), 1080);
    }

    #[test]
    fn every_preset_resolves_to_a_creatable_document() {
        for (index, preset) in PRESETS.iter().enumerate() {
            let mut dialog = NewDocumentDialog::default();
            dialog.apply_preset(index);
            let spec = dialog.spec();
            assert!(
                spec.is_valid(),
                "preset {index} ({}) resolved to {}x{}",
                preset.name,
                spec.width,
                spec.height
            );
        }
    }

    /// W18-G: Photopea's Photo, Mobile, Ads and 2ᴺ shelves are in the list
    /// the dialog draws (every group of `PresetGroup::ALL`), with Photopea's
    /// values: picking one fills the fields with its size and resolution.
    #[test]
    fn photopeas_photo_mobile_ads_and_power_of_two_shelves_are_offered() {
        let pick = |name: &str| {
            let index = PRESETS
                .iter()
                .position(|p| p.name == name)
                .unwrap_or_else(|| panic!("no {name} preset"));
            let mut dialog = NewDocumentDialog::default();
            dialog.apply_preset(index);
            (
                PRESETS[index].group,
                dialog.pixel_width(),
                dialog.pixel_height(),
                dialog.resolution_ppi(),
            )
        };
        for group in [
            PresetGroup::Photo,
            PresetGroup::Mobile,
            PresetGroup::Ads,
            PresetGroup::PowersOfTwo,
        ] {
            assert!(PresetGroup::ALL.contains(&group), "{group:?} is not drawn");
        }
        assert_eq!(pick("Enprint"), (PresetGroup::Photo, 1050, 1500, 300.0));
        assert_eq!(pick("iPhone X"), (PresetGroup::Mobile, 1125, 2436, 458.0));
        assert_eq!(pick("Leaderboard"), (PresetGroup::Ads, 728, 90, 72.0));
        let squares: Vec<u32> = PRESETS
            .iter()
            .filter(|p| p.group == PresetGroup::PowersOfTwo)
            .map(|p| p.width as u32)
            .collect();
        assert_eq!(squares, (4..=12).map(|n| 1u32 << n).collect::<Vec<_>>());
        assert_eq!(PresetGroup::PowersOfTwo.label(), "2^N");
    }

    #[test]
    fn a4_at_300ppi_is_the_size_a_print_shop_expects() {
        let index = PRESETS.iter().position(|p| p.name == "A4").unwrap();
        let mut dialog = NewDocumentDialog::default();
        dialog.apply_preset(index);
        // 210 mm = 8.2677 in -> 2480 px; 297 mm = 11.6929 in -> 3508 px.
        assert_eq!(dialog.pixel_width(), 2480);
        assert_eq!(dialog.pixel_height(), 3508);
        assert_eq!(dialog.resolution_ppi(), 300.0);
    }

    #[test]
    fn switching_units_keeps_the_pixel_size() {
        let mut dialog = NewDocumentDialog::default();
        let (w, h) = (dialog.pixel_width(), dialog.pixel_height());
        for unit in NewDocumentDialog::units() {
            dialog.set_unit(unit);
            assert_eq!(
                dialog.pixel_width(),
                w,
                "width changed switching to {unit:?}"
            );
            assert_eq!(
                dialog.pixel_height(),
                h,
                "height changed switching to {unit:?}"
            );
        }
    }

    #[test]
    fn switching_the_resolution_unit_keeps_the_resolution() {
        let mut dialog = NewDocumentDialog {
            resolution: 300.0,
            ..NewDocumentDialog::default()
        };
        dialog.set_resolution_unit(ResolutionUnit::PerCentimeter);
        assert!((dialog.resolution_ppi() - 300.0).abs() < 1e-9);
        assert!((dialog.resolution - 300.0 / 2.54).abs() < 1e-9);
    }

    #[test]
    fn editing_a_field_drops_the_preset_highlight() {
        let mut dialog = NewDocumentDialog::default();
        assert!(dialog.preset().is_some());
        dialog.set_pixel_width(640.0);
        assert!(dialog.preset().is_none());
        assert_eq!(dialog.pixel_width(), 640);
    }

    #[test]
    fn swapping_orientation_exchanges_the_sides() {
        let mut dialog = NewDocumentDialog::default();
        let (w, h) = (dialog.pixel_width(), dialog.pixel_height());
        dialog.swap_orientation();
        assert_eq!((dialog.pixel_width(), dialog.pixel_height()), (h, w));
    }

    #[test]
    fn an_unsupported_colour_mode_is_refused_and_says_why() {
        let mut dialog = NewDocumentDialog::default();
        assert!(!dialog.set_color_mode(ColorMode::Cmyk));
        assert_eq!(dialog.spec().color_mode, ColorMode::Rgb);
        assert!(ColorMode::Cmyk.unavailable().is_some());
        assert!(ColorMode::Rgb.unavailable().is_none());
    }

    #[test]
    fn a_zero_size_blocks_confirm_and_explains_itself() {
        let mut dialog = NewDocumentDialog::default();
        dialog.set_pixel_width(0.0);
        assert!(dialog.confirm().is_none());
        assert!(dialog
            .blocked_reason()
            .unwrap()
            .contains("at least 1 pixel"));
    }

    #[test]
    fn an_absurd_area_is_refused() {
        let mut dialog = NewDocumentDialog::default();
        dialog.set_pixel_width(200_000.0);
        dialog.set_pixel_height(200_000.0);
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().unwrap().contains("pixel limit"));
    }

    #[test]
    fn the_dialog_cannot_describe_a_document_the_engine_refuses() {
        // The dialog carried its own limits, so "valid here" and "loadable
        // there" were two different questions with two different answers. Every
        // size this dialog will confirm must be a size `editor-core` accepts —
        // otherwise Create builds a document that cannot be reopened.
        assert_eq!(MAX_DIMENSION, editor_core::MAX_CANVAS_DIMENSION);
        assert_eq!(MAX_PIXELS, editor_core::MAX_CANVAS_PIXELS);

        let mut dialog = NewDocumentDialog {
            unit: Unit::Pixels,
            ..Default::default()
        };
        // Every size at, just under, and just over each edge of the dialog's
        // own rules, checked against the engine's.
        for (w, h) in [
            (1.0, 1.0),
            (1920.0, 1080.0),
            (f64::from(MAX_DIMENSION), 3_333.0),
            (f64::from(MAX_DIMENSION) + 1.0, 1.0),
            (31_622.0, 31_622.0),
            (31_624.0, 31_624.0),
            (200_000.0, 200_000.0),
        ] {
            dialog.set_pixel_width(w);
            dialog.set_pixel_height(h);
            let spec = dialog.spec();
            if spec.is_valid() {
                assert!(
                    editor_core::canvas_size_is_supported(spec.width, spec.height),
                    "the dialog would create {}x{}, which the engine refuses",
                    spec.width,
                    spec.height
                );
                assert!(dialog.confirm().is_some());
            } else {
                assert!(
                    dialog.confirm().is_none(),
                    "an invalid spec must not be confirmable"
                );
                assert!(dialog.blocked_reason().is_some(), "and must say why");
            }
        }
    }

    #[test]
    fn backgrounds_resolve_to_the_right_fill() {
        assert_eq!(BackgroundContents::White.fill(), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(BackgroundContents::Black.fill(), Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(BackgroundContents::Transparent.fill(), None);
        assert_eq!(
            BackgroundContents::Custom([0.2, 0.4, 0.6, 0.5]).fill(),
            Some([0.2, 0.4, 0.6, 0.5])
        );
    }

    #[test]
    fn a_sixteen_bit_document_confirms_with_its_depth() {
        // W4-F: the compositor composites RGBA16 tiles and the shell builds
        // the base layer from them, so the dialog no longer refuses 16 bits:
        // confirming hands the shell a spec that says Sixteen.
        let dialog = NewDocumentDialog {
            bit_depth: BitDepth::Sixteen,
            ..Default::default()
        };
        assert_eq!(dialog.blocked_reason(), None);
        let outcome =
            super::super::chrome::resolve(&dialog, super::super::chrome::DialogKeys::CONFIRM);
        match outcome {
            DialogOutcome::Confirmed(DialogAction::NewDocument(spec)) => {
                assert_eq!(spec.bit_depth, BitDepth::Sixteen)
            }
            other => panic!("a 16-bit document must confirm: {other:?}"),
        }
    }

    #[test]
    fn the_memory_estimate_scales_with_area_and_depth() {
        let mut dialog = NewDocumentDialog::default();
        dialog.set_pixel_width(100.0);
        dialog.set_pixel_height(100.0);
        assert_eq!(dialog.spec().flat_bytes(), 100 * 100 * 4);
        dialog.bit_depth = BitDepth::Sixteen;
        assert_eq!(dialog.spec().flat_bytes(), 100 * 100 * 4 * 2);
    }

    #[test]
    fn it_draws_in_both_appearances() {
        frame_both_themes(|ctx| {
            let mut dialog = NewDocumentDialog::default();
            assert!(dialog.show(ctx, None).is_open());
        });
    }

    #[test]
    fn a_background_choice_survives_a_trip_through_transparent_and_back() {
        // The defect this pins: a second "Transparent background" checkbox sat
        // under the Background menu and wrote the same field. Its off-branch
        // hardcoded White, so picking Black or Custom and then toggling
        // transparency on and off silently landed on White. The menu is now
        // the only writer, so the round trip has to come back where it started.
        let custom = BackgroundContents::Custom([0.2, 0.4, 0.6, 1.0]);
        for choice in [BackgroundContents::Black, custom] {
            let mut dialog = NewDocumentDialog::default();
            dialog.set_background(choice);
            let before = dialog.spec().background;
            dialog.set_background(BackgroundContents::Transparent);
            assert_eq!(dialog.spec().background, BackgroundContents::Transparent);
            dialog.set_background(before);
            assert_eq!(
                dialog.spec().background,
                before,
                "{choice:?} did not come back"
            );
            assert_ne!(
                dialog.spec().background,
                BackgroundContents::White,
                "{choice:?} collapsed to White"
            );
        }
    }

    #[test]
    fn choosing_custom_and_then_a_colour_changes_the_spec() {
        // The defect this pins: the Custom swatch was drawn and its Response
        // dropped, so `custom_background` stayed opaque white forever and
        // "Custom" was indistinguishable from "White".
        let h = Harness::new();
        let mut dialog = NewDocumentDialog::default();
        dialog.set_background(BackgroundContents::Custom([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(
            dialog.spec().background.fill(),
            BackgroundContents::White.fill()
        );

        h.click_widget(ids::custom_background("new-document"), |ctx| {
            dialog.show(ctx, None);
        });
        assert!(dialog.color_edit().is_open(), "the swatch opened nothing");

        let chosen = crate::dialogs::ColorValue::new([0.9, 0.3, 0.1, 1.0]);
        dialog
            .color_edit_mut()
            .picker_mut()
            .expect("the picker is up")
            .set_color(chosen);
        h.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            assert!(dialog.show(ctx, None).is_open());
        });

        let fill = dialog.spec().background.fill().expect("a custom fill");
        assert_eq!(
            crate::dialogs::ColorValue::new(fill).to_bytes(),
            chosen.to_bytes()
        );
        assert_ne!(fill, [1.0, 1.0, 1.0, 1.0], "Custom is still White");
    }

    #[test]
    fn a_background_that_is_not_custom_draws_no_swatch() {
        let h = Harness::new();
        let mut dialog = NewDocumentDialog::default();
        dialog.set_background(BackgroundContents::Black);
        h.frame(Vec::new(), |ctx| {
            dialog.show(ctx, None);
        });
        assert!(!h.was_drawn(ids::custom_background("new-document")));
    }

    /// W10-J: the Artboard box is drawn, off by default, and a click on the
    /// drawn checkbox ticks it; the confirmed spec carries it to the shell.
    #[test]
    fn clicking_the_artboard_box_makes_the_confirmed_spec_an_artboard() {
        let h = Harness::new();
        let mut dialog = NewDocumentDialog::default();
        assert!(!dialog.spec().artboard, "off by default");
        h.frame(Vec::new(), |ctx| {
            dialog.show(ctx, None);
        });
        let id = h
            .ctx
            .data(|d| d.get_temp::<egui::Id>(artboard_box_key()))
            .expect("the Artboard box was drawn");
        h.click_widget(id, |ctx| {
            dialog.show(ctx, None);
        });
        assert!(dialog.spec().artboard, "the click ticked the box");
        match super::super::chrome::resolve(&dialog, super::super::chrome::DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(DialogAction::NewDocument(spec)) => {
                assert!(spec.artboard, "the confirmed spec lost the box");
            }
            other => panic!("the dialog did not confirm: {other:?}"),
        }
        h.click_widget(id, |ctx| {
            dialog.show(ctx, None);
        });
        assert!(!dialog.spec().artboard, "a second click unticks it");
    }
}
