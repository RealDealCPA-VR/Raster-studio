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
    ///
    /// An SVG shows the raster it embeds; an ICO shows its largest entry,
    /// which is the one an icon viewer opens.
    ///
    /// W10-F: a format this build writes but cannot read back (AVIF) has no
    /// preview: the encode still runs, so [`PreviewSource::encoded_len`]
    /// can size it, and this returns `Unsupported`, which the dialog shows as
    /// "could not be previewed" rather than passing the source off as the
    /// compressed result.
    pub fn render(&self, format: ExportFormat) -> Result<RenderedPreview, CodecError> {
        if !format.reads_back() {
            return Err(CodecError::Unsupported(format!(
                "{} cannot be read back to preview",
                format.extension()
            )));
        }
        self.encodes.set(self.encodes.get() + 1);
        let bytes = encode(format, self.width, self.height, &self.rgba)?;
        let decoded = match format {
            // No payload decodes as an empty buffer, which the codec refuses.
            ExportFormat::Svg => raster::decode_bytes(
                &raster::codec::svg_raster_payload(&bytes).unwrap_or_default(),
            )?,
            // W9-N: TGA has no magic number; read it back as what it is.
            ExportFormat::Tga => raster::codec::decode_surface_bytes_as(
                &bytes,
                raster::ImportLimits::default(),
                raster::ImportFormat::Tga,
            )?
            .into_decoded_image(),
            _ => raster::decode_bytes(&bytes)?,
        };
        Ok(RenderedPreview {
            width: decoded.width,
            height: decoded.height,
            rgba: decoded.rgba8,
            bytes: bytes.len() as u64,
        })
    }
}

impl PreviewSource {
    /// W10-F: the proxy's encoded length in `format`, without decoding it:
    /// the size estimate for a format [`PreviewSource::render`] cannot
    /// preview.
    pub fn encoded_len(&self, format: ExportFormat) -> Result<u64, CodecError> {
        self.encodes.set(self.encodes.get() + 1);
        Ok(encode(format, self.width, self.height, &self.rgba)?.len() as u64)
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
    /// W9-J: write an animation — one frame per `_a_` layer — when the
    /// document has frame layers and the format can hold one (GIF, PNG as
    /// APNG, WebP). On by default, as Photopea exports an animated document;
    /// a document without frame layers exports a still either way.
    pub animated: bool,
}

impl ExportEntry {
    /// A row writing `format` at `scale`.
    pub fn new(suffix: impl Into<String>, format: ExportFormat, scale: f32) -> Self {
        Self {
            enabled: true,
            suffix: suffix.into(),
            preset: ExportPreset::new("export", format).with_scale(scale),
            animated: true,
        }
    }

    /// The file name this row writes, given the document's base name.
    pub fn file_name(&self, base: &str) -> String {
        let stem = raster::sanitize_file_stem(base);
        format!("{}{}.{}", stem, self.suffix, self.preset.format.extension())
    }
}

thread_local! {
    /// The first row of the last Export As that was actually sent to be
    /// written (the folder picker answered). Set by [`remember_exported_job`].
    static LAST_CONFIRMED: std::cell::RefCell<Option<ExportEntry>> =
        const { std::cell::RefCell::new(None) };
}

/// The settings File > Export > Slices writes with: the first row of the last
/// Export As that was sent to be written (format, quality, scale, resampling,
/// depth), or a plain PNG at 100% before any has been.
///
/// Remembered per UI thread, which is the thread every dialog and every menu
/// action runs on.
pub fn last_confirmed_entry() -> ExportEntry {
    LAST_CONFIRMED
        .with(|last| last.borrow().clone())
        .unwrap_or_else(|| ExportEntry::new("", ExportFormat::Png, 1.0))
}

/// Remember `job`'s first row as the settings File > Export > Slices writes
/// with. The shell calls this where an Export As job is handed to the writer —
/// after the folder picker answered — so a dialog confirmed and then cancelled
/// at the folder picker changes nothing. [`Dialog::confirm`] stays pure.
pub fn remember_exported_job(job: &ExportJob) {
    if let Some(first) = job.entries.first() {
        LAST_CONFIRMED.with(|last| *last.borrow_mut() = Some(first.clone()));
    }
}

/// Forget the remembered Export As settings (tests start from the default).
pub fn forget_last_confirmed_entry() {
    LAST_CONFIRMED.with(|last| *last.borrow_mut() = None);
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
            return Some(
                crate::strings::tr("ui.export_as.give.the.export.a.file.name").to_string(),
            );
        }
        if self.entries.is_empty() {
            return Some(crate::strings::tr("ui.export_as.enable.at.least.one.export").to_string());
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
    /// W7-D: the exported document's colour mode
    /// (`editor_core::DocumentMeta::color_mode`), for [`Self::ink_note`].
    color_mode: u8,
    /// W9-J: how many `_a_` frame layers the document has, as the host said
    /// through [`Self::set_animation_frames`]. `None` (the host has not said)
    /// and `Some(0)` both mean the Animated option is not offered on any row.
    animation_frames: Option<usize>,
    /// W13-L: `Some((fps, length_ms))` when the animation is the document's
    /// timeline (Timeline mode) rather than its `_a_` frame layers, as the
    /// host said through [`Self::set_timeline_frames`]; the caption says so.
    timeline_frames: Option<(u32, u32)>,
    /// W10-E: the File Info XMP packet and the source file's EXIF the host
    /// offered ([`Self::set_metadata`]), and whether the job embeds them.
    metadata: raster::metadata::EmbeddedMetadata,
    embed_metadata: bool,
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
    /// Replace the live-preview proxy.
    ///
    /// A host that cannot composite while opening the dialog (it holds only a
    /// shared borrow) starts from the placeholder and swaps the real one in on
    /// a later frame; the cached render is dropped so the next frame re-encodes
    /// against the new pixels.
    pub fn set_proxy(&mut self, proxy: PreviewSource) {
        self.proxy = proxy;
        self.texture = None;
        self.cached_for = None;
        self.cached = None;
    }

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
            color_mode: 0,
            animation_frames: None,
            timeline_frames: None,
            metadata: raster::metadata::EmbeddedMetadata::default(),
            embed_metadata: true,
        }
    }

    /// W10-E: the metadata this document can carry into its files: File
    /// Info's XMP packet and, when the document was opened from a file that
    /// had one, that file's EXIF block. Embedded while
    /// [`Self::embeds_metadata`] (on by default).
    pub fn set_metadata(&mut self, metadata: raster::metadata::EmbeddedMetadata) {
        self.metadata = metadata;
    }

    /// W10-E: whether the job writes [`Self::set_metadata`]'s metadata.
    pub fn embeds_metadata(&self) -> bool {
        self.embed_metadata
    }

    /// W10-E: turn metadata embedding on or off (the Metadata checkbox).
    pub fn set_embed_metadata(&mut self, on: bool) {
        self.embed_metadata = on;
    }

    /// W9-J: the number of `_a_` frame layers in the document. With `0` the
    /// Animated option is not offered at all.
    pub fn set_animation_frames(&mut self, frames: usize) {
        self.animation_frames = Some(frames);
        self.timeline_frames = None;
    }

    /// W13-L: the document is in Timeline mode, so an animated export writes
    /// `frames` frames of its timeline (`fps` frames a second over
    /// `length_ms`), not its `_a_` frame layers.
    pub fn set_timeline_frames(&mut self, frames: usize, fps: u32, length_ms: u32) {
        self.animation_frames = Some(frames);
        self.timeline_frames = Some((fps, length_ms));
    }

    /// W9-J: whether the selected row offers the Animated option: its format
    /// can hold an animation and the host said the document has at least one
    /// `_a_` frame layer. Until the host says so ([`Self::set_animation_frames`])
    /// nothing is offered, so a still document never shows the option.
    pub fn offers_animation(&self) -> bool {
        raster::animation::can_animate(self.format())
            && self.animation_frames.is_some_and(|n| n > 0)
    }

    /// W7-D: the colour mode of the document being exported, so the dialog
    /// can say how the selected format writes it ([`Self::ink_note`]).
    pub fn set_color_mode(&mut self, mode: u8) {
        self.color_mode = mode;
    }

    /// W7-D: how the selected row's format writes a non-RGB document, or
    /// `None` for an RGB (or Grayscale) one. A CMYK document goes out as a
    /// CMYK JPEG/TIFF and an Indexed one as a palette PNG/GIF; every other
    /// pairing — and every Lab document, since no encoder here writes Lab —
    /// is converted back to RGB, and the dialog says so before the click.
    pub fn ink_note(&self) -> Option<&'static str> {
        use editor_core::color_mode::mode;
        let ink = raster::export::ExportInk::for_color_mode(self.color_mode);
        let carried = ink.carried_by(self.format());
        let key = match self.color_mode {
            mode::LAB => "ui.export_as.lab.as.rgb",
            mode::CMYK if carried => "ui.export_as.cmyk.written",
            mode::CMYK => "ui.export_as.cmyk.as.rgb",
            mode::INDEXED if carried => "ui.export_as.indexed.written",
            mode::INDEXED => "ui.export_as.indexed.as.rgb",
            _ => return None,
        };
        Some(crate::strings::tr(key))
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
        (self.entries.len() <= 1).then_some(crate::strings::tr(
            "ui.export_as.an.export.needs.at.least.one",
        ))
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
                // W10-F: AVIF has a quality too; it carries over likewise.
                ExportFormat::Avif(_) => ExportFormat::Avif(quality.unwrap_or(80)),
                // W11-H: and lossy WebP.
                ExportFormat::WebPLossy(_) => ExportFormat::WebPLossy(quality.unwrap_or(80)),
                // W13-L: and MP4 video (W15-B: H.264, or the AV1 option).
                ExportFormat::Mp4(_) => ExportFormat::Mp4(quality.unwrap_or(80)),
                ExportFormat::Mp4Av1(_) => ExportFormat::Mp4Av1(quality.unwrap_or(80)),
                other => other,
            };
            if !entry.preset.format.supports_16_bit() {
                entry.preset.bit_depth = BitDepth::Eight;
            }
        }
    }

    /// The selected row's JPEG (W10-F: or AVIF; W11-H: or lossy WebP)
    /// quality, or `None` for a format without one. Lossless WebP has none
    /// (`raster::ExportFormat::WebP`); "WebP (lossy)" is its own row.
    pub fn quality(&self) -> Option<u8> {
        match self.format() {
            ExportFormat::Jpeg(q) | ExportFormat::Avif(q) | ExportFormat::WebPLossy(q) => Some(q),
            // W13-L: MP4 video (W15-B: either codec).
            ExportFormat::Mp4(q) | ExportFormat::Mp4Av1(q) => Some(q),
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
            Some(entry) if matches!(entry.preset.format, ExportFormat::Avif(_)) => {
                entry.preset.format = ExportFormat::Avif(quality);
                true
            }
            Some(entry) if matches!(entry.preset.format, ExportFormat::WebPLossy(_)) => {
                entry.preset.format = ExportFormat::WebPLossy(quality);
                true
            }
            // W13-L: MP4 video.
            Some(entry) if matches!(entry.preset.format, ExportFormat::Mp4(_)) => {
                entry.preset.format = ExportFormat::Mp4(quality);
                true
            }
            // W15-B: MP4 with the AV1 codec.
            Some(entry) if matches!(entry.preset.format, ExportFormat::Mp4Av1(_)) => {
                entry.preset.format = ExportFormat::Mp4Av1(quality);
                true
            }
            _ => false,
        }
    }

    /// W15-B: the selected MP4 row's video codec, or `None` when the row is
    /// not an MP4.
    pub fn mp4_codec(&self) -> Option<raster::codec::formats::mp4::Mp4Codec> {
        use raster::codec::formats::mp4::Mp4Codec;
        match self.format() {
            ExportFormat::Mp4(_) => Some(Mp4Codec::H264),
            ExportFormat::Mp4Av1(_) => Some(Mp4Codec::Av1),
            _ => None,
        }
    }

    /// W15-B: choose the selected MP4 row's codec, keeping its quality.
    /// Ignored (returns `false`) unless the row is an MP4.
    pub fn set_mp4_codec(&mut self, codec: raster::codec::formats::mp4::Mp4Codec) -> bool {
        use raster::codec::formats::mp4::Mp4Codec;
        let Some(quality) = self.mp4_codec().and(self.quality()) else {
            return false;
        };
        let index = self.selected;
        match self.entry_mut(index) {
            Some(entry) => {
                entry.preset.format = match codec {
                    Mp4Codec::H264 => ExportFormat::Mp4(quality),
                    Mp4Codec::Av1 => ExportFormat::Mp4Av1(quality),
                };
                true
            }
            None => false,
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
        let bytes = if format.reads_back() {
            self.proxy.render(format).ok().map(|p| p.bytes)
        } else {
            self.proxy.encoded_len(format).ok()
        };
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
        // W10-E: every row carries the metadata when embedding is on; the
        // writer puts in what each container holds (`raster::metadata`).
        let embedded = if self.embed_metadata {
            self.metadata.clone()
        } else {
            raster::metadata::EmbeddedMetadata::default()
        };
        ExportJob {
            base_name: self.base_name.clone(),
            entries: self
                .entries
                .iter()
                .filter(|e| e.enabled)
                .cloned()
                .map(|mut e| {
                    e.preset.embedded = embedded.clone();
                    e
                })
                .collect(),
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
            Some(crate::strings::tr(
                "ui.export_as.every.enabled.row.is.written.when",
            )),
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
        design::inspector_field(ui, crate::strings::tr("ui.export_as.file.name"), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.base_name)
                    .desired_width(sizes::text_field_wide()),
            );
        });
        caption(
            ui,
            format!("Total: {}", format_bytes(self.total_estimated_bytes())),
        );
        if let Some(note) = self.ink_note() {
            caption(ui, note);
        }
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
        if checkbox_row(
            ui,
            crate::strings::tr("ui.export_as.live.preview"),
            &mut show,
        )
        .changed()
        {
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
                caption(
                    ui,
                    crate::strings::tr("ui.export_as.this.format.could.not.be.previewed"),
                );
            }
            (_, false) => {
                caption(ui, crate::strings::tr("ui.export_as.live.preview.is.off"));
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
            if design::ghost_button(ui, crate::strings::tr("ui.export_as.add.export")).clicked() {
                self.add_entry();
            }
            let blocked = self.removal_blocked();
            let response = ui
                .add_enabled_ui(blocked.is_none(), |ui| {
                    design::ghost_button(ui, crate::strings::tr("ui.export_as.remove.export"))
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
            caption(ui, crate::strings::tr("ui.export_as.no.export.selected"));
            return;
        };
        design::inspector_field(ui, "Format", |ui| {
            let mut format = entry.preset.format;
            // W10-F: every writable format, AVIF (write-only) included.
            let formats = ExportFormat::writable();
            if combo(ui, "ex-format", &mut format, &formats, format_name, |_| {
                None
            }) {
                self.set_format(format);
            }
        });
        // W15-B: an MP4 row chooses its codec: H.264 (the default) or AV1.
        if let Some(codec) = self.mp4_codec() {
            use raster::codec::formats::mp4::Mp4Codec;
            let codec_name = |c: Mp4Codec| {
                crate::strings::tr(match c {
                    Mp4Codec::H264 => "ui.export_as.codec.h264",
                    Mp4Codec::Av1 => "ui.export_as.codec.av1",
                })
                .to_string()
            };
            design::inspector_field(ui, crate::strings::tr("ui.export_as.codec"), |ui| {
                let mut chosen = codec;
                if combo(
                    ui,
                    "ex-mp4-codec",
                    &mut chosen,
                    &[Mp4Codec::H264, Mp4Codec::Av1],
                    codec_name,
                    |_| None,
                ) {
                    self.set_mp4_codec(chosen);
                }
            });
        }
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
                    .on_disabled_hover_text(
                        if lossless(entry.preset.format) {
                            format!(
                                "{} is lossless — it has no quality setting",
                                format_name(entry.preset.format)
                            )
                        } else {
                            format!(
                                "{} has no quality setting",
                                format_name(entry.preset.format)
                            )
                        },
                    );
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
                    BitDepth::Eight => crate::strings::tr("ui.export_as.8.bit").to_string(),
                    BitDepth::Sixteen => crate::strings::tr("ui.export_as.16.bit").to_string(),
                },
                |d| {
                    (d == BitDepth::Sixteen && !supports_16).then_some(crate::strings::tr(
                        "ui.export_as.this.format.stores.8.bits.per",
                    ))
                },
            ) {
                if let Some(entry) = self.entry_mut(index) {
                    entry.preset.bit_depth = depth;
                }
            }
        });

        if self.offers_animation() {
            let mut animated = entry.animated;
            if checkbox_row(ui, "Animated", &mut animated).changed() {
                if let Some(entry) = self.entry_mut(index) {
                    entry.animated = animated;
                }
            }
            let text = match (self.timeline_frames, self.animation_frames) {
                // W13-L: Timeline mode writes the timeline's frames.
                (Some((fps, length)), Some(n)) => {
                    crate::strings::tr("ui.export_as.timeline.frames")
                        .replace("{format}", &format_name(entry.preset.format))
                        .replace("{frames}", &n.to_string())
                        .replace("{fps}", &fps.to_string())
                        .replace("{length}", &length.to_string())
                }
                _ => {
                    let frames = self
                        .animation_frames
                        .map_or_else(String::new, |n| format!(" ({n})"));
                    format!(
                        "{}: one frame per _a_ layer{frames}, bottom first; other layers show as the document has them",
                        format_name(entry.preset.format)
                    )
                }
            };
            caption(ui, text);
        }

        design::section_header(ui, "Metadata");
        let mut include = entry.preset.include_metadata;
        if checkbox_row(
            ui,
            crate::strings::tr("ui.export_as.embed.colour.profile"),
            &mut include,
        )
        .changed()
        {
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
        // W10-E: File Info's XMP (PNG, JPEG, TIFF) and the source's EXIF
        // (JPEG), written by `raster::metadata` into the finished file.
        let mut embed = self.embed_metadata;
        let has_metadata = !self.metadata.is_empty();
        let response = ui
            .add_enabled_ui(has_metadata, |ui| {
                checkbox_row(
                    ui,
                    crate::strings::tr("ui.export_as.embed.exif.and.xmp"),
                    &mut embed,
                )
            })
            .inner;
        if has_metadata {
            if response.changed() {
                self.embed_metadata = embed;
            }
            let format = entry.preset.format;
            let key = if !raster::metadata::carries_xmp(format) {
                "ui.export_as.metadata.none"
            } else if matches!(format, ExportFormat::Jpeg(_)) {
                "ui.export_as.metadata.xmp.exif"
            } else {
                "ui.export_as.metadata.xmp"
            };
            caption(ui, crate::strings::tr(key));
        } else {
            response.on_disabled_hover_text(crate::strings::tr("ui.export_as.metadata.empty"));
        }

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
        ExportFormat::WebP => crate::strings::tr("ui.export_as.webp.lossless").to_string(),
        ExportFormat::Tiff => "TIFF".to_string(),
        ExportFormat::Gif => "GIF".to_string(),
        ExportFormat::Bmp => "BMP".to_string(),
        ExportFormat::Tga => "TGA".to_string(),
        ExportFormat::Ico => "ICO".to_string(),
        ExportFormat::Svg => "SVG".to_string(),
        // W10-F.
        ExportFormat::Ppm => "PPM".to_string(),
        ExportFormat::Pgm => "PGM".to_string(),
        ExportFormat::Pbm => "PBM".to_string(),
        ExportFormat::Dds => "DDS".to_string(),
        ExportFormat::DdsBc3 => "DDS/BC3".to_string(),
        ExportFormat::Avif(_) => "AVIF".to_string(),
        // W11-H: OpenEXR (32-bit float) and lossless JPEG XL.
        ExportFormat::Exr => "EXR".to_string(),
        ExportFormat::Jxl => "JXL".to_string(),
        ExportFormat::WebPLossy(_) => crate::strings::tr("ui.export_as.webp.lossy").to_string(),
        // A format the codec gains later reads as its extension until it is
        // given a name here.
        #[allow(unreachable_patterns)]
        other => other.extension().to_uppercase(),
    }
}

/// W10-F: whether `format` stores the pixels it is given exactly (for an
/// opaque 8-bit image). PGM / PBM reduce colour and BC3 compresses by
/// blocks, so neither is "lossless" though neither has a quality.
fn lossless(format: ExportFormat) -> bool {
    !matches!(
        format,
        ExportFormat::Jpeg(_)
            | ExportFormat::Avif(_)
            | ExportFormat::WebPLossy(_)
            | ExportFormat::Pgm
            | ExportFormat::Pbm
            | ExportFormat::DdsBc3
            | ExportFormat::Gif
    )
}

impl Dialog for ExportAsDialog {
    fn title(&self) -> &'static str {
        crate::strings::tr("ui.export_as.export.as")
    }

    fn confirm_label(&self) -> &'static str {
        "Export"
    }

    fn confirm(&self) -> Option<DialogAction> {
        let job = self.job();
        if !job.is_valid() {
            return None;
        }
        Some(DialogAction::Export(Box::new(job)))
    }

    fn blocked_reason(&self) -> Option<String> {
        self.job().validation_error()
    }
}

// ---------------------------------------------------------------------------
// W10-E: File ▸ Export ▸ PDF…
// ---------------------------------------------------------------------------

/// The page a PDF export lays the image on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PdfPageSize {
    /// A page exactly the image's size at the chosen pixels per inch.
    Image,
    /// ISO A4, the image fitted and centred.
    A4,
    /// US Letter, the image fitted and centred.
    Letter,
}

impl PdfPageSize {
    pub const ALL: [PdfPageSize; 3] = [PdfPageSize::Image, PdfPageSize::A4, PdfPageSize::Letter];

    pub fn label(self) -> String {
        match self {
            PdfPageSize::Image => crate::strings::tr("ui.export_pdf.page.image").to_string(),
            PdfPageSize::A4 => "A4".to_string(),
            PdfPageSize::Letter => crate::strings::tr("ui.export_pdf.page.letter").to_string(),
        }
    }
}

/// A confirmed PDF export: the page, and the resolution the Image page uses.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PdfExportSpec {
    pub page: PdfPageSize,
    /// Pixels per inch for [`PdfPageSize::Image`], `1..=2400`.
    pub ppi: f64,
    /// Turn A4 / Letter sideways.
    pub landscape: bool,
}

impl Default for PdfExportSpec {
    fn default() -> Self {
        Self {
            page: PdfPageSize::Image,
            ppi: 72.0,
            landscape: false,
        }
    }
}

impl PdfExportSpec {
    /// The page, in points, for a `width` x `height` image.
    pub fn page_for(&self, width: u32, height: u32) -> raster::pdf::PdfPage {
        let paper = match self.page {
            PdfPageSize::Image => return raster::pdf::PdfPage::at_ppi(width, height, self.ppi),
            PdfPageSize::A4 => raster::pdf::PdfPage::A4,
            PdfPageSize::Letter => raster::pdf::PdfPage::LETTER,
        };
        if self.landscape {
            raster::pdf::PdfPage {
                width_pt: paper.height_pt,
                height_pt: paper.width_pt,
            }
        } else {
            paper
        }
    }

    pub fn is_valid(&self) -> bool {
        self.ppi.is_finite() && (1.0..=2400.0).contains(&self.ppi)
    }
}

/// File ▸ Export ▸ PDF…: a raster PDF of the composite on a chosen page.
#[derive(Clone, Debug)]
pub struct ExportPdfDialog {
    spec: PdfExportSpec,
    document: (u32, u32),
}

impl ExportPdfDialog {
    /// Over a `width` x `height` document.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            spec: PdfExportSpec::default(),
            document: (width.max(1), height.max(1)),
        }
    }

    pub fn spec(&self) -> PdfExportSpec {
        self.spec
    }

    pub fn set_spec(&mut self, spec: PdfExportSpec) {
        self.spec = spec;
    }

    pub fn blocked_reason(&self) -> Option<&'static str> {
        (!self.spec.is_valid()).then(|| crate::strings::tr("ui.export_pdf.ppi.range"))
    }

    pub fn confirm(&self) -> Option<PdfExportSpec> {
        self.spec.is_valid().then_some(self.spec)
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<PdfExportSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<PdfExportSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "w10e-export-pdf",
            crate::strings::tr("ui.export_pdf.title"),
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
        use crate::strings::tr;
        caption(ui, tr("ui.export_pdf.subtitle"));
        design::inspector_field(ui, tr("ui.export_pdf.page"), |ui| {
            combo(
                ui,
                "w10e-export-pdf-page",
                &mut self.spec.page,
                &PdfPageSize::ALL,
                PdfPageSize::label,
                |_| None,
            );
        });
        if self.spec.page == PdfPageSize::Image {
            design::inspector_field(ui, tr("ui.export_pdf.resolution"), |ui| {
                numeric(ui, &mut self.spec.ppi, 1.0..=2400.0, 0, "ppi");
            });
        } else {
            checkbox_row(ui, tr("ui.export_pdf.landscape"), &mut self.spec.landscape);
        }
        let page = self.spec.page_for(self.document.0, self.document.1);
        caption(
            ui,
            format!(
                "{:.1} x {:.1} in",
                page.width_pt / 72.0,
                page.height_pt / 72.0
            ),
        );
        action_row(ui, tr("ui.export_pdf.export"), self.blocked_reason(), &[])
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
            // An icon previews its largest entry.
            let side = if format == ExportFormat::Ico { 256 } else { 32 };
            assert_eq!((preview.width, preview.height), (side, side), "{format:?}");
            assert_eq!(preview.rgba.len(), (side * side * 4) as usize, "{format:?}");
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
    fn an_exported_job_is_what_slices_export_with() {
        forget_last_confirmed_entry();
        assert_eq!(last_confirmed_entry().preset.format, ExportFormat::Png);
        let mut dialog = dialog();
        dialog.set_format(ExportFormat::Jpeg(90));
        dialog.set_quality(40);
        dialog.set_scale(0.5);
        // Choosing settings is not exporting them, and neither is confirming:
        // `confirm` is pure, the shell remembers only a job it hands on.
        assert_eq!(last_confirmed_entry().preset.format, ExportFormat::Png);
        let Some(DialogAction::Export(job)) = dialog.confirm() else {
            panic!("a valid job");
        };
        assert_eq!(last_confirmed_entry().preset.format, ExportFormat::Png);
        remember_exported_job(&job);
        let remembered = last_confirmed_entry();
        assert_eq!(remembered.preset.format, ExportFormat::Jpeg(40));
        assert_eq!(remembered.preset.scale, 0.5);
        forget_last_confirmed_entry();
    }

    #[test]
    fn the_ico_and_svg_rows_preview_what_the_file_holds() {
        let proxy = PreviewSource::placeholder(20, 10);
        let svg = proxy.render(ExportFormat::Svg).unwrap();
        // The SVG's raster is the proxy, pixel for pixel.
        assert_eq!((svg.width, svg.height), (20, 10));
        assert_eq!(svg.rgba, proxy.rgba());
        let ico = proxy.render(ExportFormat::Ico).unwrap();
        assert_eq!((ico.width, ico.height), (256, 256));
        assert_eq!(format_name(ExportFormat::Ico), "ICO");
        assert_eq!(format_name(ExportFormat::Svg), "SVG");
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
    fn the_dialog_says_how_a_non_rgb_document_is_written_and_draws_it() {
        use editor_core::color_mode::mode;
        let mut dialog = dialog();
        assert_eq!(dialog.ink_note(), None, "an RGB document needs no note");
        let cases = [
            (mode::LAB, ExportFormat::Tiff, "ui.export_as.lab.as.rgb"),
            (
                mode::CMYK,
                ExportFormat::Jpeg(90),
                "ui.export_as.cmyk.written",
            ),
            (mode::CMYK, ExportFormat::Tiff, "ui.export_as.cmyk.written"),
            (mode::CMYK, ExportFormat::Png, "ui.export_as.cmyk.as.rgb"),
            (
                mode::INDEXED,
                ExportFormat::Png,
                "ui.export_as.indexed.written",
            ),
            (
                mode::INDEXED,
                ExportFormat::Gif,
                "ui.export_as.indexed.written",
            ),
            (
                mode::INDEXED,
                ExportFormat::Jpeg(90),
                "ui.export_as.indexed.as.rgb",
            ),
        ];
        for (mode, format, key) in cases {
            dialog.set_color_mode(mode);
            dialog.set_format(format);
            let note = crate::strings::tr(key);
            assert!(!note.is_empty(), "{key} has no catalogue row");
            assert_eq!(dialog.ink_note(), Some(note), "{mode} {format:?}");
            // Drawn, not only computed.
            let ctx = Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            let mut drawn = false;
            for _ in 0..3 {
                let out = ctx.run(egui::RawInput::default(), |ctx| {
                    let _ = dialog.show(ctx);
                });
                drawn = out.shapes.iter().any(|c| match &c.shape {
                    egui::Shape::Text(t) => t.galley.text().contains(note),
                    _ => false,
                });
            }
            assert!(drawn, "{key} was never drawn");
        }
    }

    /// W9-J: every text the dialog draws in one frame (a few frames, so the
    /// layout settles), on a screen large enough for the whole settings panel.
    fn drawn_texts(dialog: &mut ExportAsDialog) -> Vec<String> {
        let ctx = Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1400.0),
            )),
            ..Default::default()
        };
        let mut texts = Vec::new();
        for _ in 0..3 {
            let out = ctx.run(input(), |ctx| {
                let _ = dialog.show(ctx);
            });
            texts = out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                    _ => None,
                })
                .collect();
        }
        texts
    }

    #[test]
    fn the_animated_option_is_drawn_for_formats_that_animate_and_reaches_the_job() {
        let mut dialog = dialog();
        // Until the host names the frame count, a GIF row offers nothing.
        dialog.set_format(ExportFormat::Gif);
        assert!(!dialog.offers_animation(), "offered before the host said");
        assert!(!drawn_texts(&mut dialog).iter().any(|t| t == "Animated"));
        dialog.set_animation_frames(3);
        for format in [ExportFormat::Gif, ExportFormat::Png, ExportFormat::WebP] {
            dialog.set_format(format);
            assert!(dialog.offers_animation(), "{format:?}");
            let texts = drawn_texts(&mut dialog);
            assert!(
                texts.iter().any(|t| t == "Animated"),
                "{format:?}: no Animated checkbox in {texts:?}"
            );
            assert!(
                texts
                    .iter()
                    .any(|t| t.contains("one frame per _a_ layer (3)")),
                "{format:?}: no frame caption in {texts:?}"
            );
        }
        for format in [
            ExportFormat::Jpeg(90),
            ExportFormat::Tiff,
            ExportFormat::Bmp,
        ] {
            dialog.set_format(format);
            assert!(!dialog.offers_animation(), "{format:?}");
            let texts = drawn_texts(&mut dialog);
            assert!(
                !texts.iter().any(|t| t == "Animated"),
                "{format:?} cannot animate but offered it"
            );
        }
        // A document known to have no frame layers is not offered it.
        dialog.set_format(ExportFormat::Gif);
        dialog.set_animation_frames(0);
        assert!(!dialog.offers_animation());
        assert!(!drawn_texts(&mut dialog).iter().any(|t| t == "Animated"));
        // The row's choice travels in the job the shell exports.
        assert!(dialog.job().entries[0].animated, "on by default");
        dialog.entry_mut(0).unwrap().animated = false;
        assert!(!dialog.job().entries[0].animated);
    }

    /// W15-B: one headless frame of the dialog on a context that persists
    /// across calls (so an open combo stays open), feeding `events`; the
    /// drawn texts with their screen rects.
    fn frame_texts(
        ctx: &Context,
        dialog: &mut ExportAsDialog,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Rect)> {
        let out = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1400.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                let _ = dialog.show(ctx);
            },
        );
        out.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_string(),
                    egui::Rect::from_min_size(t.pos, t.galley.size()),
                )),
                _ => None,
            })
            .collect()
    }

    /// W15-B: press and release the pointer on the last drawn `text`.
    fn click_text(ctx: &Context, dialog: &mut ExportAsDialog, text: &str) {
        let texts = frame_texts(ctx, dialog, Vec::new());
        let rect = texts
            .iter()
            .rev()
            .find(|(t, _)| t == text)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{text:?} is not drawn: {texts:?}"));
        let pos = rect.center();
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        frame_texts(
            ctx,
            dialog,
            vec![egui::Event::PointerMoved(pos), button(true)],
        );
        frame_texts(ctx, dialog, vec![button(false)]);
    }

    /// W15-B: an MP4 row draws a Codec field showing H.264, the default;
    /// choosing AV1 in it by pointer puts `Mp4Av1` (same quality) in the job
    /// the shell exports, and that format encodes an `av01` file while the
    /// default encodes `avc1`. A row of another format has no Codec field.
    #[test]
    fn an_mp4_row_chooses_its_codec_by_pointer_and_h264_is_the_default() {
        use raster::codec::formats::mp4::{self, Mp4Codec};
        let mut dialog = dialog();
        dialog.set_format(ExportFormat::Png);
        let texts = drawn_texts(&mut dialog);
        assert!(!texts.iter().any(|t| t == "Codec"), "{texts:?}");
        assert_eq!(dialog.mp4_codec(), None);
        assert!(!dialog.set_mp4_codec(Mp4Codec::Av1), "PNG has no codec");

        // File > Export As > MP4 and the format list both land on H.264.
        assert_eq!(ExportFormat::VIDEO, [ExportFormat::Mp4(80)]);
        dialog.set_format(ExportFormat::Mp4(80));
        assert!(dialog.set_quality(55));
        assert_eq!(dialog.mp4_codec(), Some(Mp4Codec::H264));
        let texts = drawn_texts(&mut dialog);
        assert!(texts.iter().any(|t| t == "Codec"), "{texts:?}");
        assert!(
            texts.iter().any(|t| t == "H.264 (plays everywhere)"),
            "{texts:?}"
        );

        let ctx = Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        for _ in 0..2 {
            frame_texts(&ctx, &mut dialog, Vec::new());
        }
        click_text(&ctx, &mut dialog, "H.264 (plays everywhere)");
        click_text(&ctx, &mut dialog, "AV1 (smaller, newer players)");
        assert_eq!(dialog.mp4_codec(), Some(Mp4Codec::Av1));
        let job = dialog.job();
        assert_eq!(job.entries[0].preset.format, ExportFormat::Mp4Av1(55));
        assert_eq!(dialog.quality(), Some(55), "the quality carried over");
        let texts: Vec<String> = frame_texts(&ctx, &mut dialog, Vec::new())
            .into_iter()
            .map(|t| t.0)
            .collect();
        assert!(
            texts.iter().any(|t| t == "AV1 (smaller, newer players)"),
            "{texts:?}"
        );

        // The job's format is what the export encodes.
        let rgba = PreviewSource::placeholder(32, 32).rgba().to_vec();
        let av1 = encode(job.entries[0].preset.format, 32, 32, &rgba).unwrap();
        assert_eq!(&mp4::probe(&av1).unwrap().codec, b"av01");
        let h264 = encode(ExportFormat::Mp4(55), 32, 32, &rgba).unwrap();
        let info = mp4::probe(&h264).unwrap();
        assert_eq!(&info.codec, b"avc1");
        assert!(info.has_avcc);

        // And back to H.264, keeping the quality.
        assert!(dialog.set_mp4_codec(Mp4Codec::H264));
        assert_eq!(dialog.format(), ExportFormat::Mp4(55));
    }

    /// W13-L: in Timeline mode the Animated caption names the timeline's
    /// frames, not `_a_` layers.
    #[test]
    fn in_timeline_mode_the_animated_caption_names_the_timeline() {
        let mut dialog = dialog();
        dialog.set_format(ExportFormat::Mp4(80));
        dialog.set_timeline_frames(90, 30, 3000);
        assert!(dialog.offers_animation());
        let texts = drawn_texts(&mut dialog);
        assert!(
            texts
                .iter()
                .any(|t| t.contains("the timeline, 90 frames at 30 fps over 3000 ms")),
            "no timeline caption in {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("_a_ layer")),
            "the frame-layer caption is gone: {texts:?}"
        );
        // Back to Frames mode: the frame-layer caption again.
        dialog.set_animation_frames(2);
        let texts = drawn_texts(&mut dialog);
        assert!(texts
            .iter()
            .any(|t| t.contains("one frame per _a_ layer (2)")));
    }

    /// W10-F: the format list the dialog draws offers every new format, and
    /// AVIF carries a live quality that reaches the row's preset; its size is
    /// estimated from a real encode while its preview says it cannot be
    /// shown.
    #[test]
    fn the_new_formats_are_offered_and_avif_has_a_quality() {
        let mut d = dialog();
        for (format, name) in [
            (ExportFormat::Ppm, "PPM"),
            (ExportFormat::Pgm, "PGM"),
            (ExportFormat::Pbm, "PBM"),
            (ExportFormat::Dds, "DDS"),
            (ExportFormat::DdsBc3, "DDS/BC3"),
            (ExportFormat::Avif(80), "AVIF"),
        ] {
            assert!(ExportFormat::writable().contains(&format), "{name}");
            d.set_format(format);
            let texts = drawn_texts(&mut d);
            assert!(
                texts.iter().any(|t| t == name),
                "{name} not drawn: {texts:?}"
            );
            assert!(
                d.measure_proxy(0).is_some_and(|b| b > 0),
                "{name} has no size"
            );
        }
        d.set_format(ExportFormat::Avif(80));
        assert_eq!(d.quality(), Some(80));
        assert!(d.set_quality(35));
        assert_eq!(d.job().entries[0].preset.format, ExportFormat::Avif(35));
        assert!(
            d.proxy.render(ExportFormat::Avif(35)).is_err(),
            "no fake preview"
        );
        let texts = drawn_texts(&mut d);
        assert!(
            texts
                .iter()
                .any(|t| t == crate::strings::tr("ui.export_as.this.format.could.not.be.previewed")),
            "{texts:?}"
        );
        // Lower quality, smaller file: the estimate is a real encode.
        let small = d.proxy.encoded_len(ExportFormat::Avif(10)).unwrap();
        let large = d.proxy.encoded_len(ExportFormat::Avif(95)).unwrap();
        assert!(small < large, "{small} vs {large}");
        // WebP keeps no quality: its encoder is lossless only.
        d.set_format(ExportFormat::WebP);
        assert_eq!(d.quality(), None);
    }

    /// W11-H: the format list the dialog draws offers EXR and JPEG XL, each
    /// sized by a real encode and previewed from what it wrote.
    #[test]
    fn exr_and_jpeg_xl_are_offered_sized_and_previewed() {
        let mut d = dialog();
        for (format, name) in [(ExportFormat::Exr, "EXR"), (ExportFormat::Jxl, "JXL")] {
            assert!(ExportFormat::writable().contains(&format), "{name}");
            d.set_format(format);
            assert_eq!(d.job().entries[0].preset.format, format, "{name}");
            assert_eq!(d.quality(), None, "{name} has no quality knob");
            let texts = drawn_texts(&mut d);
            assert!(
                texts.iter().any(|t| t == name),
                "{name} not drawn: {texts:?}"
            );
            assert!(
                d.measure_proxy(0).is_some_and(|b| b > 0),
                "{name} has no size"
            );
            let preview = d.proxy.render(format).expect("previewed");
            assert_eq!(
                (preview.width, preview.height),
                (d.proxy.width(), d.proxy.height()),
                "{name}"
            );
        }
    }

    /// W11-H: "WebP (lossy)" is offered beside the lossless row, carries a
    /// quality that reaches the encoder, and is sized and previewed by a
    /// real encode.
    #[test]
    fn lossy_webp_is_offered_with_a_quality_sized_and_previewed() {
        let mut d = dialog();
        let lossy = ExportFormat::WebPLossy(80);
        assert!(ExportFormat::writable().contains(&lossy));
        d.set_format(lossy);
        let name = crate::strings::tr("ui.export_as.webp.lossy");
        assert_eq!(name, "WebP (lossy)");
        let texts = drawn_texts(&mut d);
        assert!(texts.iter().any(|t| t == name), "{texts:?}");
        assert_eq!(d.quality(), Some(80));
        assert!(d.set_quality(30));
        assert_eq!(
            d.job().entries[0].preset.format,
            ExportFormat::WebPLossy(30)
        );
        let small = d.proxy.encoded_len(ExportFormat::WebPLossy(5)).unwrap();
        let large = d.proxy.encoded_len(ExportFormat::WebPLossy(100)).unwrap();
        assert!(small < large, "{small} vs {large}");
        let preview = d
            .proxy
            .render(ExportFormat::WebPLossy(30))
            .expect("previewed");
        assert_eq!(
            (preview.width, preview.height),
            (d.proxy.width(), d.proxy.height())
        );
        // The lossless row still has no quality knob.
        d.set_format(ExportFormat::WebP);
        assert_eq!(d.quality(), None);
    }

    #[test]
    fn it_draws_every_format_in_both_appearances() {
        for format in ExportFormat::writable() {
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

    /// W10-E: the metadata the host offers rides every enabled row's preset
    /// while embedding is on, and none of them when it is off.
    #[test]
    fn the_job_carries_the_offered_metadata_while_embedding_is_on() {
        let mut dialog = ExportAsDialog::new(20, 10, "Meta", PreviewSource::placeholder(8, 8));
        assert!(dialog.job().entries[0].preset.embedded.is_empty());
        let meta = raster::metadata::EmbeddedMetadata {
            xmp: Some("<x/>".into()),
            exif: Some(vec![1, 2, 3]),
        };
        dialog.set_metadata(meta.clone());
        dialog.add_entry();
        assert!(dialog.embeds_metadata(), "on by default");
        for entry in dialog.job().entries {
            assert_eq!(entry.preset.embedded, meta);
        }
        dialog.set_embed_metadata(false);
        assert!(dialog
            .job()
            .entries
            .iter()
            .all(|e| e.preset.embedded.is_empty()));
    }

    /// W10-E: the PDF page follows the chosen size — the image at its ppi,
    /// or A4 / Letter, sideways when asked — and a silly ppi blocks.
    #[test]
    fn the_pdf_dialog_picks_its_page_and_blocks_a_bad_resolution() {
        let mut dialog = ExportPdfDialog::new(300, 150);
        let spec = match dialog.resolve(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(spec) => spec,
            other => panic!("Enter did not confirm: {other:?}"),
        };
        let page = spec.page_for(300, 150);
        assert_eq!((page.width_pt, page.height_pt), (300.0, 150.0), "72 ppi");
        dialog.set_spec(PdfExportSpec {
            page: PdfPageSize::A4,
            landscape: true,
            ..PdfExportSpec::default()
        });
        let a4 = dialog.spec().page_for(300, 150);
        assert!(a4.width_pt > a4.height_pt, "landscape A4");
        dialog.set_spec(PdfExportSpec {
            ppi: 0.0,
            ..PdfExportSpec::default()
        });
        assert!(dialog.blocked_reason().is_some());
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = ExportPdfDialog::new(300, 150);
            assert!(dialog.show(ctx).is_open());
        });
    }
}
