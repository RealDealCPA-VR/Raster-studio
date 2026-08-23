//! Export As.
//!
//! The preview and the size readout are not simulations: the dialog holds a
//! small RGBA8 proxy of the document, **actually encodes it** with the chosen
//! codec and settings, and shows the decoded result and the byte count that
//! came back. That is why the quality slider shows real JPEG blocking rather
//! than a blur, and why the estimate tracks the encoder instead of a formula
//! that drifts away from it.
//!
//! The full-size estimate scales the measured proxy size by the area ratio. It
//! is labelled an estimate because that scaling is an approximation — the
//! measurement itself is exact for the proxy.
//!
//! # What the frame costs
//!
//! An encode is expensive: a 512x512 round trip at [`MAX_PROXY_SIDE`] is about
//! a millisecond, and the body asks for a size from three places — once per row
//! in the list, once per enabled row in the total, and once more in the
//! settings readout. Measuring on demand therefore costs five to seven encodes
//! a frame on a three-row list, which is most of a 60fps budget spent
//! re-deriving numbers that did not change. So the measurement is memoised per
//! [`ExportFormat`] and invalidated exactly where the preview cache is, and
//! [`PreviewSource::encode_count`] exists so a test can assert that a steady
//! frame performs **zero** encodes rather than assert on a stopwatch.

use std::cell::{Cell, RefCell};

use design::tokens::Space;
use egui::{Context, TextureHandle};
use raster::{encode, BitDepth, CodecError, ExportFormat, ExportPreset};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, warning, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::controls::{checkbox_row, combo, numeric};
use super::image_size::{filter_label, FILTERS};
use super::sizes;
use super::units::format_bytes;

/// Largest proxy side the dialog will encode per frame. A live preview has to
/// stay interactive, and a full-resolution JPEG encode per keystroke does not.
pub const MAX_PROXY_SIDE: u32 = 512;

/// A downscaled RGBA8 copy of the document, used for the live preview.
///
/// Counts its own encodes. That counter is observability, not state: encoding
/// is the expensive thing this dialog does — a 512x512 round trip is about a
/// millisecond, and the dialog used to run one *per row per frame* from three
/// separate call sites, with the live preview switched off. A count is the only
/// way to assert that a steady frame does no work, because a test cannot see a
/// millisecond and cannot assert on wall-clock time without measuring the
/// machine it runs on. It lives here rather than in a global so that concurrent
/// tests cannot see each other's encodes.
#[derive(Clone, Debug)]
pub struct PreviewSource {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    encodes: Cell<u64>,
}

/// Two proxies are equal when they hold the same pixels. The encode counter is
/// a tally of what has been *done* to a proxy, not part of what it is.
impl PartialEq for PreviewSource {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width && self.height == other.height && self.rgba == other.rgba
    }
}

impl PreviewSource {
    /// Wrap a straight-alpha RGBA8 buffer.
    ///
    /// Rejects a buffer whose length does not match its dimensions, and a side
    /// above [`MAX_PROXY_SIDE`] — the caller downsamples before handing it over,
    /// because only the caller knows how to composite the document.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        let expected = u64::from(width) * u64::from(height) * 4;
        if width == 0
            || height == 0
            || width > MAX_PROXY_SIDE
            || height > MAX_PROXY_SIDE
            || rgba.len() as u64 != expected
        {
            return None;
        }
        Some(Self {
            width,
            height,
            rgba,
            encodes: Cell::new(0),
        })
    }

    /// A stand-in proxy for a document the caller has not rendered yet: a
    /// diagonal ramp with a hard-edged checker, which is enough structure for
    /// the quality slider to show something real.
    pub fn placeholder(width: u32, height: u32) -> Self {
        let width = width.clamp(1, MAX_PROXY_SIDE);
        let height = height.clamp(1, MAX_PROXY_SIDE);
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let ramp = ((x * 255) / width.max(1)) as u8;
                let cross = (((x / 8) + (y / 8)) % 2) as u8 * 255;
                rgba.extend_from_slice(&[ramp, cross, 255u8.saturating_sub(ramp), 255]);
            }
        }
        Self {
            width,
            height,
            rgba,
            encodes: Cell::new(0),
        }
    }

    /// Proxy width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Proxy height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The proxy's pixels.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// How many times this proxy has been encoded since it was made.
    pub fn encode_count(&self) -> u64 {
        self.encodes.get()
    }

    /// Encode the proxy exactly as the preset would, and decode it back.
    ///
    /// The returned pixels are what the file will look like; `bytes` is the
    /// real encoded length of the *proxy*.
    pub fn render(&self, format: ExportFormat) -> Result<RenderedPreview, CodecError> {
        self.encodes.set(self.encodes.get() + 1);
        let bytes = encode(format, self.width, self.height, &self.rgba)?;
        let decoded = raster::decode_bytes(&bytes)?;
        Ok(RenderedPreview {
            width: decoded.width,
            height: decoded.height,
            rgba: decoded.rgba8,
            bytes: bytes.len() as u64,
        })
    }
}

/// The result of encoding and decoding the proxy.
#[derive(Clone, PartialEq, Debug)]
pub struct RenderedPreview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Real encoded size of the *proxy*, in bytes.
    pub bytes: u64,
}

/// One row of the export list.
#[derive(Clone, PartialEq, Debug)]
pub struct ExportEntry {
    /// Whether this row is included in the export.
    pub enabled: bool,
    /// Appended to the base file name, before the extension. May be empty.
    pub suffix: String,
    pub preset: ExportPreset,
}

impl ExportEntry {
    /// A row writing `format` at `scale`.
    pub fn new(suffix: impl Into<String>, format: ExportFormat, scale: f32) -> Self {
        Self {
            enabled: true,
            suffix: suffix.into(),
            preset: ExportPreset::new("export", format).with_scale(scale),
        }
    }

    /// The file name this row writes, given the document's base name.
    pub fn file_name(&self, base: &str) -> String {
        let stem = raster::sanitize_file_stem(base);
        format!("{}{}.{}", stem, self.suffix, self.preset.format.extension())
    }
}

/// What the dialog commits to.
#[derive(Clone, PartialEq, Debug)]
pub struct ExportJob {
    pub base_name: String,
    /// Only the enabled rows, in list order.
    pub entries: Vec<ExportEntry>,
}

impl ExportJob {
    /// Whether the job can run: at least one row, every preset valid, and no
    /// two rows writing the same file.
    pub fn is_valid(&self) -> bool {
        self.validation_error().is_none()
    }

    /// Why the job cannot run, in words.
    pub fn validation_error(&self) -> Option<String> {
        if self.base_name.trim().is_empty() {
            return Some("Give the export a file name".to_string());
        }
        if self.entries.is_empty() {
            return Some("Enable at least one export".to_string());
        }
        for entry in &self.entries {
            if let Err(error) = entry.preset.validate() {
                return Some(error.to_string());
            }
        }
        let mut names: Vec<String> = self
            .entries
            .iter()
            .map(|e| e.file_name(&self.base_name))
            .collect();
        names.sort();
        for pair in names.windows(2) {
            if pair[0] == pair[1] {
                return Some(format!(
                    "Two exports would both write {} — give them different suffixes",
                    pair[0]
                ));
            }
        }
        None
    }
}

/// Export As.
pub struct ExportAsDialog {
    document: (u32, u32),
    base_name: String,
    entries: Vec<ExportEntry>,
    selected: usize,
    proxy: PreviewSource,
    show_preview: bool,
    /// Cached preview texture, rebuilt when the settings that affect it change.
    texture: Option<TextureHandle>,
    cached_for: Option<ExportFormat>,
    cached: Option<RenderedPreview>,
    /// Proxy size per format, so a format is encoded at most once per settings
    /// change rather than once per row per frame. Invalidated wherever
    /// `cached_for` is.
    measured: RefCell<Vec<(ExportFormat, Option<u64>)>>,
}

impl std::fmt::Debug for ExportAsDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportAsDialog")
            .field("document", &self.document)
            .field("base_name", &self.base_name)
            .field("entries", &self.entries)
            .field("selected", &self.selected)
            .field("show_preview", &self.show_preview)
            .finish_non_exhaustive()
    }
}

impl ExportAsDialog {
    /// Open on a `width` x `height` document called `base_name`.
    pub fn new(
        width: u32,
        height: u32,
        base_name: impl Into<String>,
        proxy: PreviewSource,
    ) -> Self {
        Self {
            document: (width.max(1), height.max(1)),
            base_name: base_name.into(),
            entries: vec![ExportEntry::new("", ExportFormat::Png, 1.0)],
            selected: 0,
            proxy,
            show_preview: true,
            texture: None,
            cached_for: None,
            cached: None,
            measured: RefCell::new(Vec::new()),
        }
    }

    /// The export rows, including the disabled ones.
    pub fn entries(&self) -> &[ExportEntry] {
        &self.entries
    }

    /// The row the settings panel is editing.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Select a row; out-of-range indices clamp to the last one.
    pub fn select(&mut self, index: usize) {
        self.selected = index.min(self.entries.len().saturating_sub(1));
    }

    /// Add a row, copying the selected one so a second size is one click away.
    pub fn add_entry(&mut self) -> usize {
        let mut entry = self
            .entries
            .get(self.selected)
            .cloned()
            .unwrap_or_else(|| ExportEntry::new("", ExportFormat::Png, 1.0));
        entry.suffix = self.unique_suffix(&entry);
        self.entries.push(entry);
        self.selected = self.entries.len() - 1;
        self.selected
    }

    /// Remove a row. The last row cannot be removed — an export list with
    /// nothing in it has nothing to confirm.
    pub fn remove_entry(&mut self, index: usize) -> bool {
        if self.entries.len() <= 1 || index >= self.entries.len() {
            return false;
        }
        self.entries.remove(index);
        self.selected = self.selected.min(self.entries.len() - 1);
        true
    }

    /// Why a row cannot be removed, if it cannot.
    pub fn removal_blocked(&self) -> Option<&'static str> {
        (self.entries.len() <= 1).then_some("An export needs at least one output")
    }

    /// Mutable access to a row, invalidating the preview and size caches.
    ///
    /// Both caches are keyed by settings that a caller holding a `&mut` could
    /// change, so both are dropped here rather than at each individual setter —
    /// one invalidation point is one thing to keep right.
    pub fn entry_mut(&mut self, index: usize) -> Option<&mut ExportEntry> {
        self.cached_for = None;
        self.measured.get_mut().clear();
        self.entries.get_mut(index)
    }

    /// The document's base file name.
    pub fn base_name(&self) -> &str {
        &self.base_name
    }

    /// Rename the export.
    pub fn set_base_name(&mut self, name: impl Into<String>) {
        self.base_name = name.into();
    }

    /// Whether the live preview is on.
    pub fn show_preview(&self) -> bool {
        self.show_preview
    }

    /// Turn the live preview on or off.
    ///
    /// Off drops the preview encode. It does **not** drop the size estimate,
    /// which the export list shows per row whether or not the image is on
    /// screen — but that estimate is memoised per format by
    /// [`ExportAsDialog::measure_proxy`], so a steady frame with unchanged
    /// settings encodes nothing either way. What the toggle saves is the
    /// decode and the texture upload the preview needs on top of the encode.
    pub fn set_show_preview(&mut self, on: bool) {
        self.show_preview = on;
        if !on {
            self.texture = None;
            self.cached = None;
            self.cached_for = None;
        }
    }

    /// The format of the selected row.
    pub fn format(&self) -> ExportFormat {
        self.entries
            .get(self.selected)
            .map_or(ExportFormat::Png, |e| e.preset.format)
    }

    /// Set the selected row's format, keeping the quality where the new format
    /// has one.
    pub fn set_format(&mut self, format: ExportFormat) {
        let quality = self.quality();
        if let Some(entry) = self.entry_mut(self.selected) {
            entry.preset.format = match format {
                ExportFormat::Jpeg(_) => ExportFormat::Jpeg(quality.unwrap_or(90)),
                other => other,
            };
            if !entry.preset.format.supports_16_bit() {
                entry.preset.bit_depth = BitDepth::Eight;
            }
        }
    }

    /// The selected row's JPEG quality, or `None` for a format without one.
    pub fn quality(&self) -> Option<u8> {
        match self.format() {
            ExportFormat::Jpeg(q) => Some(q),
            _ => None,
        }
    }

    /// Set the selected row's quality. Ignored unless the format has one; the
    /// value is clamped into [`ExportFormat::JPEG_QUALITY_RANGE`], because the
    /// codec rejects anything else rather than clamping silently.
    pub fn set_quality(&mut self, quality: u8) -> bool {
        let quality = quality.clamp(
            *ExportFormat::JPEG_QUALITY_RANGE.start(),
            *ExportFormat::JPEG_QUALITY_RANGE.end(),
        );
        let index = self.selected;
        match self.entry_mut(index) {
            Some(entry) if matches!(entry.preset.format, ExportFormat::Jpeg(_)) => {
                entry.preset.format = ExportFormat::Jpeg(quality);
                true
            }
            _ => false,
        }
    }

    /// The selected row's scale factor.
    pub fn scale(&self) -> f32 {
        self.entries
            .get(self.selected)
            .map_or(1.0, |e| e.preset.scale)
    }

    /// Set the selected row's scale factor.
    pub fn set_scale(&mut self, scale: f32) {
        let index = self.selected;
        if let Some(entry) = self.entry_mut(index) {
            entry.preset.scale = scale;
        }
    }

    /// The pixel size row `index` writes.
    pub fn target_size(&self, index: usize) -> Option<(u32, u32)> {
        self.entries
            .get(index)?
            .preset
            .target_size(self.document.0, self.document.1)
            .ok()
    }

    /// The real encoded size of the *proxy* for row `index`.
    ///
    /// This is a measurement, not a model: the bytes come back from the codec.
    /// It is also memoised per format, because the dialog asks for it from
    /// three places — the row list, the total, and the settings readout — and
    /// asks again every frame. Two rows exporting PNG at different scales share
    /// one encode; the scale is applied afterwards, by
    /// [`ExportAsDialog::estimated_bytes`].
    pub fn measure_proxy(&self, index: usize) -> Option<u64> {
        let format = self.entries.get(index)?.preset.format;
        if let Some((_, bytes)) = self
            .measured
            .borrow()
            .iter()
            .find(|(cached, _)| *cached == format)
        {
            return *bytes;
        }
        // A format the codec refuses is cached as a failure too. Retrying a
        // failing encode every frame is the same waste as repeating a
        // successful one, and it is the case a slow machine can least afford.
        let bytes = self.proxy.render(format).ok().map(|p| p.bytes);
        self.measured.borrow_mut().push((format, bytes));
        bytes
    }

    /// The estimated size of the file row `index` writes.
    ///
    /// The proxy measurement scaled by the area ratio between the output and
    /// the proxy. Monotone in quality because the measurement is.
    pub fn estimated_bytes(&self, index: usize) -> Option<u64> {
        let measured = self.measure_proxy(index)?;
        let (out_w, out_h) = self.target_size(index)?;
        let proxy_area = u64::from(self.proxy.width()) * u64::from(self.proxy.height());
        if proxy_area == 0 {
            return None;
        }
        let out_area = u64::from(out_w) * u64::from(out_h);
        Some(
            (measured as u128 * out_area as u128 / proxy_area as u128).min(u128::from(u64::MAX))
                as u64,
        )
    }

    /// The total estimated size of every enabled row.
    pub fn total_estimated_bytes(&self) -> u64 {
        (0..self.entries.len())
            .filter(|i| self.entries[*i].enabled)
            .filter_map(|i| self.estimated_bytes(i))
            .sum()
    }

    /// How many times the proxy has been encoded since the dialog opened.
    ///
    /// The measurement of the claim the module header makes: a steady frame
    /// with unchanged settings must not move this number.
    pub fn encode_count(&self) -> u64 {
        self.proxy.encode_count()
    }

    /// The job the dialog currently describes.
    pub fn job(&self) -> ExportJob {
        ExportJob {
            base_name: self.base_name.clone(),
            entries: self.entries.iter().filter(|e| e.enabled).cloned().collect(),
        }
    }

    fn unique_suffix(&self, like: &ExportEntry) -> String {
        let mut candidate = if like.suffix.is_empty() {
            "@2x".to_string()
        } else {
            format!("{}-copy", like.suffix)
        };
        let mut counter = 2;
        while self
            .entries
            .iter()
            .any(|e| e.suffix == candidate && e.preset.format == like.preset.format)
        {
            candidate = format!("{}-{counter}", like.suffix);
            counter += 1;
        }
        candidate
    }

    /// Draw the dialog for one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = super::chrome::resolve(self, keys);
        self.refresh_preview(ctx);
        let drawn = modal(
            ctx,
            "export-as",
            self.title(),
            Some("Every enabled row is written when you export."),
            DialogWidth::Split,
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

    /// Re-encode the proxy if the settings that affect it changed.
    fn refresh_preview(&mut self, ctx: &Context) {
        if !self.show_preview {
            return;
        }
        let format = self.format();
        if self.cached_for == Some(format) && self.texture.is_some() {
            return;
        }
        self.cached_for = Some(format);
        match self.proxy.render(format) {
            Ok(preview) => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [preview.width as usize, preview.height as usize],
                    &preview.rgba,
                );
                self.texture =
                    Some(ctx.load_texture("export-preview", image, egui::TextureOptions::LINEAR));
                self.cached = Some(preview);
            }
            Err(_) => {
                self.texture = None;
                self.cached = None;
            }
        }
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(sizes::preview_column_width());
                self.preview_panel(ui);
            });
            ui.add_space(Space::Large.pt());
            ui.vertical(|ui| {
                self.entry_list(ui);
                hairline(ui);
                self.settings(ui);
            });
        });
        hairline(ui);
        design::inspector_field(ui, "File name", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.base_name)
                    .desired_width(sizes::text_field_wide()),
            );
        });
        caption(
            ui,
            format!("Total: {}", format_bytes(self.total_estimated_bytes())),
        );
        if let Some(reason) = self.blocked_reason() {
            warning(ui, reason);
        }
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        let mut show = self.show_preview;
        if checkbox_row(ui, "Live preview", &mut show).changed() {
            self.set_show_preview(show);
        }
        match (&self.texture, self.show_preview) {
            (Some(texture), true) => {
                let size = texture.size_vec2();
                let scale = (sizes::export_preview_width() / size.x.max(1.0)).min(1.0);
                ui.image((texture.id(), size * scale));
                if let Some(preview) = &self.cached {
                    caption(
                        ui,
                        format!(
                            "Proxy encodes to {} at {} x {}",
                            format_bytes(preview.bytes),
                            self.proxy.width(),
                            self.proxy.height()
                        ),
                    );
                }
            }
            (_, true) => {
                caption(ui, "This format could not be previewed.");
            }
            (_, false) => {
                caption(ui, "Live preview is off.");
            }
        }
    }

    fn entry_list(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Exports");
        let base = self.base_name.clone();
        let mut toggled: Option<usize> = None;
        let mut selected: Option<usize> = None;
        for (index, entry) in self.entries.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut enabled = entry.enabled;
                if checkbox_row(ui, "", &mut enabled).changed() {
                    toggled = Some(index);
                }
                let label = format!(
                    "{}  ·  {}",
                    entry.file_name(&base),
                    self.estimated_bytes(index)
                        .map_or_else(|| "—".to_string(), format_bytes)
                );
                if design::list_row(ui, &label, index == self.selected).clicked() {
                    selected = Some(index);
                }
            });
        }
        if let Some(index) = toggled {
            if let Some(entry) = self.entries.get_mut(index) {
                entry.enabled = !entry.enabled;
            }
        }
        if let Some(index) = selected {
            self.select(index);
        }
        ui.horizontal(|ui| {
            if design::ghost_button(ui, "Add export").clicked() {
                self.add_entry();
            }
            let blocked = self.removal_blocked();
            let response = ui
                .add_enabled_ui(blocked.is_none(), |ui| {
                    design::ghost_button(ui, "Remove export")
                })
                .inner;
            match blocked {
                Some(reason) => {
                    response.on_disabled_hover_text(reason);
                }
                None => {
                    if response.clicked() {
                        self.remove_entry(self.selected);
                    }
                }
            }
        });
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Settings");
        let index = self.selected;
        let Some(entry) = self.entries.get(index).cloned() else {
            caption(ui, "No export selected");
            return;
        };
        design::inspector_field(ui, "Format", |ui| {
            let mut format = entry.preset.format;
            if combo(
                ui,
                "ex-format",
                &mut format,
                &ExportFormat::ALL[..],
                format_name,
                |_| None,
            ) {
                self.set_format(format);
            }
        });
        design::inspector_field(ui, "Suffix", |ui| {
            let mut suffix = entry.suffix.clone();
            if ui
                .add(
                    egui::TextEdit::singleline(&mut suffix)
                        .desired_width(sizes::text_field_short()),
                )
                .changed()
            {
                if let Some(entry) = self.entry_mut(index) {
                    entry.suffix = suffix;
                }
            }
        });
        match self.quality() {
            Some(quality) => {
                design::inspector_field(ui, "Quality", |ui| {
                    let mut value = f64::from(quality);
                    if numeric(ui, &mut value, 1.0..=100.0, 0, "").changed() {
                        self.set_quality(value.round() as u8);
                    }
                });
            }
            None => {
                design::inspector_field(ui, "Quality", |ui| {
                    ui.add_enabled_ui(false, |ui| {
                        let mut value = 100.0;
                        numeric(ui, &mut value, 1.0..=100.0, 0, "")
                    })
                    .inner
                    .on_disabled_hover_text(format!(
                        "{} is lossless — it has no quality setting",
                        format_name(entry.preset.format)
                    ));
                });
            }
        }
        design::inspector_field(ui, "Scale", |ui| {
            let mut scale = f64::from(entry.preset.scale) * 100.0;
            if numeric(ui, &mut scale, 1.0..=1000.0, 1, "%").changed() {
                self.set_scale((scale / 100.0) as f32);
            }
        });
        design::inspector_field(ui, "Resample", |ui| {
            let mut filter = entry.preset.filter;
            if combo(
                ui,
                "ex-filter",
                &mut filter,
                FILTERS,
                |f| filter_label(f).to_string(),
                |_| None,
            ) {
                if let Some(entry) = self.entry_mut(index) {
                    entry.preset.filter = filter;
                }
            }
        });
        let supports_16 = entry.preset.format.supports_16_bit();
        design::inspector_field(ui, "Depth", |ui| {
            let mut depth = entry.preset.bit_depth;
            if combo(
                ui,
                "ex-depth",
                &mut depth,
                &[BitDepth::Eight, BitDepth::Sixteen],
                |d| match d {
                    BitDepth::Eight => "8 bit".to_string(),
                    BitDepth::Sixteen => "16 bit".to_string(),
                },
                |d| {
                    (d == BitDepth::Sixteen && !supports_16)
                        .then_some("This format stores 8 bits per channel")
                },
            ) {
                if let Some(entry) = self.entry_mut(index) {
                    entry.preset.bit_depth = depth;
                }
            }
        });

        design::section_header(ui, "Metadata");
        let mut include = entry.preset.include_metadata;
        if checkbox_row(ui, "Embed colour profile", &mut include).changed() {
            if let Some(entry) = self.entry_mut(index) {
                entry.preset.include_metadata = include;
            }
        }
        if !entry.preset.format.supports_icc() {
            caption(
                ui,
                format!(
                    "{} cannot carry a colour profile.",
                    format_name(entry.preset.format)
                ),
            );
        }
        ui.add_enabled_ui(false, |ui| {
            let mut exif = false;
            checkbox_row(ui, "Embed EXIF and XMP", &mut exif)
        })
        .inner
        .on_disabled_hover_text("EXIF and XMP writing is not implemented — only ICC is embedded");

        if let Some((w, h)) = self.target_size(index) {
            caption(
                ui,
                format!(
                    "{w} x {h} px  ·  about {}",
                    self.estimated_bytes(index)
                        .map_or_else(|| "unknown".to_string(), format_bytes)
                ),
            );
        }
    }
}

/// Menu label for an export format.
pub fn format_name(format: ExportFormat) -> String {
    match format {
        ExportFormat::Png => "PNG".to_string(),
        ExportFormat::Jpeg(_) => "JPEG".to_string(),
        ExportFormat::WebP => "WebP (lossless)".to_string(),
        ExportFormat::Tiff => "TIFF".to_string(),
        ExportFormat::Gif => "GIF".to_string(),
        ExportFormat::Bmp => "BMP".to_string(),
    }
}

impl Dialog for ExportAsDialog {
    fn title(&self) -> &'static str {
        "Export As"
    }

    fn confirm_label(&self) -> &'static str {
        "Export"
    }

    fn confirm(&self) -> Option<DialogAction> {
        let job = self.job();
        job.is_valid().then(|| DialogAction::Export(Box::new(job)))
    }

    fn blocked_reason(&self) -> Option<String> {
        self.job().validation_error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::{frame_both_themes, Harness};

    fn dialog() -> ExportAsDialog {
        ExportAsDialog::new(2000, 1000, "Sketch", PreviewSource::placeholder(64, 64))
    }

    #[test]
    fn a_proxy_rejects_a_buffer_that_does_not_match_its_size() {
        assert!(PreviewSource::new(2, 2, vec![0; 16]).is_some());
        assert!(PreviewSource::new(2, 2, vec![0; 15]).is_none());
        assert!(PreviewSource::new(0, 2, vec![]).is_none());
        assert!(PreviewSource::new(MAX_PROXY_SIDE + 1, 1, vec![0; 4]).is_none());
    }

    #[test]
    fn the_placeholder_proxy_is_a_well_formed_image() {
        let proxy = PreviewSource::placeholder(40, 30);
        assert_eq!(proxy.width(), 40);
        assert_eq!(proxy.height(), 30);
        assert_eq!(proxy.rgba().len(), 40 * 30 * 4);
    }

    #[test]
    fn every_format_actually_encodes_and_decodes() {
        let proxy = PreviewSource::placeholder(32, 32);
        for format in ExportFormat::ALL {
            let preview = proxy
                .render(format)
                .unwrap_or_else(|e| panic!("{format:?} failed to round-trip: {e}"));
            assert_eq!((preview.width, preview.height), (32, 32), "{format:?}");
            assert_eq!(preview.rgba.len(), 32 * 32 * 4, "{format:?}");
            assert!(preview.bytes > 0, "{format:?} encoded to nothing");
        }
    }

    #[test]
    fn the_size_estimate_is_monotone_in_quality() {
        let proxy = PreviewSource::placeholder(64, 64);
        let mut previous = 0u64;
        for quality in [1u8, 10, 25, 40, 55, 70, 85, 95, 100] {
            let bytes = proxy
                .render(ExportFormat::Jpeg(quality))
                .expect("jpeg encodes")
                .bytes;
            assert!(
                bytes >= previous,
                "quality {quality} produced {bytes} bytes, below the previous {previous}"
            );
            previous = bytes;
        }
    }

    #[test]
    fn the_dialogs_estimate_is_monotone_in_quality_too() {
        let mut dialog = dialog();
        dialog.set_format(ExportFormat::Jpeg(1));
        let mut previous = 0u64;
        for quality in [1u8, 20, 50, 80, 100] {
            assert!(dialog.set_quality(quality));
            let bytes = dialog.estimated_bytes(0).expect("an estimate");
            assert!(bytes >= previous, "quality {quality}: {bytes} < {previous}");
            previous = bytes;
        }
        // And the top of the range really is bigger than the bottom.
        dialog.set_quality(1);
        let low = dialog.estimated_bytes(0).unwrap();
        dialog.set_quality(100);
        assert!(dialog.estimated_bytes(0).unwrap() > low);
    }

    #[test]
    fn the_estimate_scales_with_the_output_area() {
        let mut dialog = dialog();
        let full = dialog.estimated_bytes(0).expect("an estimate");
        dialog.set_scale(0.5);
        let half = dialog.estimated_bytes(0).expect("an estimate");
        // A quarter of the area, so about a quarter of the bytes.
        let ratio = half as f64 / full as f64;
        assert!((ratio - 0.25).abs() < 1e-4, "ratio was {ratio}");
    }

    #[test]
    fn quality_only_exists_on_a_format_that_has_one() {
        let mut dialog = dialog();
        assert_eq!(dialog.quality(), None);
        assert!(!dialog.set_quality(50));
        dialog.set_format(ExportFormat::Jpeg(90));
        assert_eq!(dialog.quality(), Some(90));
        assert!(dialog.set_quality(50));
        assert_eq!(dialog.quality(), Some(50));
    }

    #[test]
    fn quality_is_clamped_into_the_range_the_codec_accepts() {
        let mut dialog = dialog();
        dialog.set_format(ExportFormat::Jpeg(90));
        dialog.set_quality(0);
        assert_eq!(dialog.quality(), Some(1));
        dialog.set_quality(255);
        assert_eq!(dialog.quality(), Some(100));
        assert!(dialog.job().is_valid());
    }

    #[test]
    fn switching_to_a_format_without_16_bit_drops_the_depth() {
        let mut dialog = dialog();
        dialog.entry_mut(0).unwrap().preset.bit_depth = BitDepth::Sixteen;
        dialog.set_format(ExportFormat::Jpeg(90));
        assert_eq!(dialog.entries()[0].preset.bit_depth, BitDepth::Eight);
    }

    #[test]
    fn the_target_size_follows_the_scale_factor() {
        let mut dialog = dialog();
        assert_eq!(dialog.target_size(0), Some((2000, 1000)));
        dialog.set_scale(0.25);
        assert_eq!(dialog.target_size(0), Some((500, 250)));
    }

    #[test]
    fn file_names_carry_the_suffix_and_the_extension() {
        let entry = ExportEntry::new("@2x", ExportFormat::Jpeg(80), 2.0);
        assert_eq!(entry.file_name("Poster"), "Poster@2x.jpg");
        let plain = ExportEntry::new("", ExportFormat::Png, 1.0);
        assert_eq!(plain.file_name("Poster"), "Poster.png");
    }

    #[test]
    fn adding_an_export_gives_it_a_name_of_its_own() {
        let mut dialog = dialog();
        dialog.add_entry();
        assert_eq!(dialog.entries().len(), 2);
        let names: Vec<String> = dialog
            .entries()
            .iter()
            .map(|e| e.file_name("Sketch"))
            .collect();
        assert_ne!(names[0], names[1]);
        assert!(dialog.job().is_valid());
    }

    #[test]
    fn two_exports_writing_the_same_file_block_the_export() {
        let mut dialog = dialog();
        dialog.add_entry();
        dialog.entry_mut(1).unwrap().suffix = String::new();
        assert!(!dialog.job().is_valid());
        assert!(dialog
            .blocked_reason()
            .unwrap()
            .contains("different suffixes"));
        assert!(dialog.confirm().is_none());
    }

    #[test]
    fn the_last_export_cannot_be_removed() {
        let mut dialog = dialog();
        assert!(!dialog.remove_entry(0));
        assert!(dialog.removal_blocked().is_some());
        dialog.add_entry();
        assert!(dialog.removal_blocked().is_none());
        assert!(dialog.remove_entry(1));
        assert!(!dialog.remove_entry(0));
    }

    #[test]
    fn disabling_every_export_blocks_the_export() {
        let mut dialog = dialog();
        dialog.entry_mut(0).unwrap().enabled = false;
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().unwrap().contains("at least one"));
    }

    #[test]
    fn an_empty_file_name_blocks_the_export() {
        let mut dialog = dialog();
        dialog.set_base_name("   ");
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().unwrap().contains("file name"));
    }

    #[test]
    fn only_enabled_rows_reach_the_job() {
        let mut dialog = dialog();
        dialog.add_entry();
        dialog.entry_mut(0).unwrap().enabled = false;
        let job = dialog.job();
        assert_eq!(job.entries.len(), 1);
        assert!(job.is_valid());
    }

    #[test]
    fn the_total_only_counts_enabled_rows() {
        let mut dialog = dialog();
        let one = dialog.total_estimated_bytes();
        dialog.add_entry();
        let two = dialog.total_estimated_bytes();
        assert!(two > one);
        dialog.entry_mut(1).unwrap().enabled = false;
        assert_eq!(dialog.total_estimated_bytes(), one);
    }

    #[test]
    fn turning_the_preview_off_drops_the_cached_encode() {
        let mut dialog = dialog();
        dialog.set_show_preview(false);
        assert!(!dialog.show_preview());
        assert!(dialog.cached.is_none());
    }

    #[test]
    fn confirm_produces_a_valid_job_and_cancel_produces_nothing() {
        let dialog = dialog();
        assert!(dialog.confirm().unwrap().is_valid());
        assert_eq!(
            super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn it_draws_every_format_in_both_appearances() {
        for format in ExportFormat::ALL {
            frame_both_themes(|ctx| {
                let mut dialog = dialog();
                dialog.set_format(format);
                dialog.add_entry();
                assert!(dialog.show(ctx).is_open());
            });
        }
    }

    #[test]
    fn a_repeated_measurement_of_one_format_encodes_once() {
        let dialog = dialog();
        let before = dialog.encode_count();
        let first = dialog.measure_proxy(0).expect("a measurement");
        assert_eq!(
            dialog.encode_count(),
            before + 1,
            "the first call must encode"
        );
        for _ in 0..8 {
            assert_eq!(dialog.measure_proxy(0), Some(first));
        }
        assert_eq!(
            dialog.encode_count(),
            before + 1,
            "the memoised measurement encoded again"
        );
    }

    #[test]
    fn changing_a_setting_re_measures_exactly_once() {
        let mut dialog = dialog();
        dialog.measure_proxy(0);
        dialog.set_format(ExportFormat::Jpeg(90));
        let before = dialog.encode_count();
        let low = dialog.measure_proxy(0).expect("a measurement");
        assert_eq!(dialog.encode_count(), before + 1);
        dialog.set_quality(20);
        let after_change = dialog.encode_count();
        let lower = dialog.measure_proxy(0).expect("a measurement");
        assert_eq!(
            dialog.encode_count(),
            after_change + 1,
            "quality did not invalidate"
        );
        assert!(
            lower < low,
            "the cache survived a quality change: {lower} vs {low}"
        );
    }

    #[test]
    fn a_steady_frame_encodes_nothing_at_all() {
        // The defect this pins: `estimated_bytes` -> `measure_proxy` ->
        // `PreviewSource::render` ran a full encode plus decode with no
        // memoisation, from three places in `body()`, every frame — including
        // with the live preview switched off, which the toggle's own doc
        // comment claimed it prevented. Three rows cost five to seven encodes a
        // frame, most of a 60fps budget.
        for preview in [true, false] {
            let mut dialog =
                ExportAsDialog::new(2000, 1000, "Sketch", PreviewSource::placeholder(64, 64));
            dialog.set_show_preview(preview);
            dialog.add_entry();
            dialog.entry_mut(1).unwrap().preset.format = ExportFormat::Jpeg(80);
            dialog.add_entry();

            let h = Harness::new();
            // Two warm-up frames: the first populates the caches, the second
            // proves they took.
            for _ in 0..2 {
                h.frame(Vec::new(), |ctx| {
                    dialog.show(ctx);
                });
            }
            let before = dialog.encode_count();
            for _ in 0..5 {
                h.frame(Vec::new(), |ctx| {
                    dialog.show(ctx);
                });
            }
            let after = dialog.encode_count();
            assert_eq!(
                after,
                before,
                "five idle frames with preview {preview} performed {} encodes",
                after - before
            );
        }
    }
}
