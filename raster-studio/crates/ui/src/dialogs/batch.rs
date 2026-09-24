//! W10-E: File ▸ Automate ▸ Batch… and File ▸ Automate ▸ Convert Formats….
//!
//! One dialog, two modes. **Batch** runs a recorded Action from the Actions
//! panel's library over every image in a source folder and saves each result
//! into a destination folder in the chosen format; **Convert Formats** does
//! the same with no Action — decode, optionally rescale, re-encode.
//!
//! The dialog owns only the question: a [`BatchSpec`] (mode, Action, the two
//! folders, format, JPEG quality and scale). The folders are typed or chosen
//! with the platform folder picker, which the *host* runs — the dialog raises
//! a request ([`BatchDialog::take_folder_request`]) and the host answers with
//! [`BatchDialog::set_folder`], the way Color Lookup's "Load" asks for a
//! `.cube`. The files are read and written by the application's job worker,
//! never here.

use std::path::{Path, PathBuf};

use egui::Context;
use raster::ExportFormat;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{combo, integer};
use super::sizes;
use crate::strings::tr;

/// Which of the two File ▸ Automate commands the dialog asks for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BatchMode {
    /// Play a recorded Action over each file, then save it.
    Batch,
    /// Re-encode each file (format, quality, scale) with no Action.
    ConvertFormats,
}

/// One of the dialog's two folder fields.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BatchFolder {
    Source,
    Destination,
}

/// The formats a batch can write. A closed list rather than
/// [`ExportFormat`] itself, because JPEG's quality is a separate field here
/// and must not make two JPEG choices compare unequal.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BatchFormat {
    Png,
    Jpeg,
    WebP,
    Tiff,
    Bmp,
    Gif,
    Tga,
}

impl BatchFormat {
    pub const ALL: [BatchFormat; 7] = [
        BatchFormat::Png,
        BatchFormat::Jpeg,
        BatchFormat::WebP,
        BatchFormat::Tiff,
        BatchFormat::Bmp,
        BatchFormat::Gif,
        BatchFormat::Tga,
    ];

    /// The codec format, with `quality` applied to JPEG.
    pub fn export_format(self, quality: u8) -> ExportFormat {
        match self {
            BatchFormat::Png => ExportFormat::Png,
            BatchFormat::Jpeg => ExportFormat::Jpeg(quality.clamp(1, 100)),
            BatchFormat::WebP => ExportFormat::WebP,
            BatchFormat::Tiff => ExportFormat::Tiff,
            BatchFormat::Bmp => ExportFormat::Bmp,
            BatchFormat::Gif => ExportFormat::Gif,
            BatchFormat::Tga => ExportFormat::Tga,
        }
    }

    /// The name the format list shows (the file extension, upper case).
    pub fn label(self) -> String {
        self.export_format(90).extension().to_uppercase()
    }
}

/// What a confirmed Batch / Convert Formats asks the application to do.
#[derive(Clone, PartialEq, Debug)]
pub struct BatchSpec {
    pub mode: BatchMode,
    /// The Action to play, by its index in the Actions library. Required in
    /// [`BatchMode::Batch`], ignored by Convert Formats.
    pub action: Option<usize>,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub format: BatchFormat,
    /// JPEG quality, `1..=100`.
    pub quality: u8,
    /// Output size as a percentage of each input, `1..=1000`.
    pub scale_percent: u32,
}

impl BatchSpec {
    /// A spec for `mode` with no folders chosen yet: PNG, quality 90, 100%.
    pub fn new(mode: BatchMode) -> Self {
        Self {
            mode,
            action: None,
            source: PathBuf::new(),
            destination: PathBuf::new(),
            format: BatchFormat::Png,
            quality: 90,
            scale_percent: 100,
        }
    }

    /// The codec format each output is written in.
    pub fn export_format(&self) -> ExportFormat {
        self.format.export_format(self.quality)
    }

    /// The scale as a multiplier.
    pub fn scale(&self) -> f32 {
        self.scale_percent.clamp(1, 1000) as f32 / 100.0
    }

    /// Why the spec cannot run with an Actions library of `actions` entries,
    /// or `None` when it can.
    pub fn blocked_reason(&self, actions: usize) -> Option<&'static str> {
        if self.source.as_os_str().is_empty() {
            return Some(tr("ui.batch.choose.source"));
        }
        if self.destination.as_os_str().is_empty() {
            return Some(tr("ui.batch.choose.destination"));
        }
        if same_folder(&self.source, &self.destination) {
            return Some(tr("ui.batch.same.folder"));
        }
        if !(1..=1000).contains(&self.scale_percent) {
            return Some(tr("ui.batch.scale.range"));
        }
        if self.mode == BatchMode::Batch {
            match self.action {
                None if actions == 0 => return Some(tr("ui.batch.no.actions")),
                None => return Some(tr("ui.batch.choose.action")),
                Some(i) if i >= actions => return Some(tr("ui.batch.choose.action")),
                Some(_) => {}
            }
        }
        None
    }
}

/// Whether two folder paths name the same folder (canonically when both
/// exist, textually otherwise).
fn same_folder(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// File ▸ Automate ▸ Batch… / Convert Formats….
#[derive(Clone, Debug)]
pub struct BatchDialog {
    spec: BatchSpec,
    /// The Actions library's names, in library order.
    actions: Vec<String>,
    source_text: String,
    destination_text: String,
    folder_request: Option<BatchFolder>,
}

impl BatchDialog {
    /// A dialog for `mode` over an Actions library named `actions`. Batch
    /// opens on the first Action when there is one.
    pub fn new(mode: BatchMode, actions: Vec<String>) -> Self {
        let mut spec = BatchSpec::new(mode);
        if mode == BatchMode::Batch && !actions.is_empty() {
            spec.action = Some(0);
        }
        Self {
            spec,
            actions,
            source_text: String::new(),
            destination_text: String::new(),
            folder_request: None,
        }
    }

    pub fn mode(&self) -> BatchMode {
        self.spec.mode
    }

    /// The spec as it stands.
    pub fn spec(&self) -> &BatchSpec {
        &self.spec
    }

    /// Replace the spec — tests and a re-opened dialog.
    pub fn set_spec(&mut self, spec: BatchSpec) {
        self.source_text = spec.source.display().to_string();
        self.destination_text = spec.destination.display().to_string();
        self.spec = spec;
    }

    /// Set one folder field, as the host's folder picker answered.
    pub fn set_folder(&mut self, field: BatchFolder, path: PathBuf) {
        let text = path.display().to_string();
        match field {
            BatchFolder::Source => {
                self.source_text = text;
                self.spec.source = path;
            }
            BatchFolder::Destination => {
                self.destination_text = text;
                self.spec.destination = path;
            }
        }
    }

    /// The folder field whose "Choose" button was pressed since the last
    /// take — the host answers it with its folder picker.
    pub fn take_folder_request(&mut self) -> Option<BatchFolder> {
        self.folder_request.take()
    }

    pub fn title(&self) -> &'static str {
        match self.spec.mode {
            BatchMode::Batch => tr("ui.batch.title"),
            BatchMode::ConvertFormats => tr("ui.batch.convert.title"),
        }
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<&'static str> {
        self.spec.blocked_reason(self.actions.len())
    }

    /// The spec a confirmation hands over, or `None` while it is blocked.
    pub fn confirm(&self) -> Option<BatchSpec> {
        self.blocked_reason().is_none().then(|| self.spec.clone())
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<BatchSpec> {
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
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<BatchSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(
            ctx,
            "w10e-batch",
            title,
            None,
            DialogWidth::Standard,
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

    fn folder_row(&mut self, ui: &mut egui::Ui, field: BatchFolder) {
        let (label, id) = match field {
            BatchFolder::Source => (tr("ui.batch.source"), "w10e-batch-source"),
            BatchFolder::Destination => (tr("ui.batch.destination"), "w10e-batch-destination"),
        };
        design::inspector_field(ui, label, |ui| {
            ui.horizontal(|ui| {
                let text = match field {
                    BatchFolder::Source => &mut self.source_text,
                    BatchFolder::Destination => &mut self.destination_text,
                };
                let edit = egui::TextEdit::singleline(text)
                    .id(egui::Id::new(id))
                    .desired_width(sizes::text_field_path());
                if ui.add(edit).changed() {
                    let path = PathBuf::from(text.trim());
                    match field {
                        BatchFolder::Source => self.spec.source = path,
                        BatchFolder::Destination => self.spec.destination = path,
                    }
                }
                if ui.button(tr("ui.batch.choose")).clicked() {
                    self.folder_request = Some(field);
                }
            });
        });
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(
            ui,
            match self.spec.mode {
                BatchMode::Batch => tr("ui.batch.subtitle"),
                BatchMode::ConvertFormats => tr("ui.batch.convert.subtitle"),
            },
        );
        if self.spec.mode == BatchMode::Batch {
            design::section_header(ui, tr("ui.batch.play"));
            if self.actions.is_empty() {
                caption(ui, tr("ui.batch.no.actions"));
            } else {
                let mut index = self.spec.action.unwrap_or(0);
                let options: Vec<usize> = (0..self.actions.len()).collect();
                let names = self.actions.clone();
                if combo(
                    ui,
                    "w10e-batch-action",
                    &mut index,
                    &options,
                    |i| names.get(i).cloned().unwrap_or_default(),
                    |_| None,
                ) || self.spec.action.is_none()
                {
                    self.spec.action = Some(index);
                }
            }
        }
        design::section_header(ui, tr("ui.batch.folders"));
        self.folder_row(ui, BatchFolder::Source);
        self.folder_row(ui, BatchFolder::Destination);
        design::section_header(ui, tr("ui.batch.save.as"));
        design::inspector_field(ui, tr("ui.batch.format"), |ui| {
            combo(
                ui,
                "w10e-batch-format",
                &mut self.spec.format,
                &BatchFormat::ALL,
                BatchFormat::label,
                |_| None,
            );
        });
        if self.spec.format == BatchFormat::Jpeg {
            design::inspector_field(ui, tr("ui.batch.quality"), |ui| {
                let mut q = i64::from(self.spec.quality);
                if integer(ui, &mut q, 1..=100).changed() {
                    self.spec.quality = q.clamp(1, 100) as u8;
                }
            });
        }
        design::inspector_field(ui, tr("ui.batch.scale"), |ui| {
            let mut s = i64::from(self.spec.scale_percent);
            if integer(ui, &mut s, 1..=1000).changed() {
                self.spec.scale_percent = s.clamp(1, 1000) as u32;
            }
        });
        caption(ui, tr("ui.batch.log.note"));
        action_row(ui, tr("ui.batch.run"), self.blocked_reason(), &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(mode: BatchMode) -> BatchDialog {
        let mut dialog = BatchDialog::new(mode, vec!["Action 1".into()]);
        dialog.set_folder(BatchFolder::Source, PathBuf::from("in-folder"));
        dialog.set_folder(BatchFolder::Destination, PathBuf::from("out-folder"));
        dialog
    }

    #[test]
    fn a_batch_needs_both_folders_and_an_action() {
        let mut dialog = BatchDialog::new(BatchMode::Batch, vec!["Action 1".into()]);
        assert_eq!(dialog.spec().action, Some(0), "opens on the first action");
        assert!(dialog.blocked_reason().is_some(), "no folders yet");
        dialog.set_folder(BatchFolder::Source, PathBuf::from("in-folder"));
        assert!(dialog.blocked_reason().is_some(), "no destination yet");
        dialog.set_folder(BatchFolder::Destination, PathBuf::from("in-folder"));
        assert!(dialog.blocked_reason().is_some(), "the same folder twice");
        dialog.set_folder(BatchFolder::Destination, PathBuf::from("out-folder"));
        assert_eq!(dialog.blocked_reason(), None);
        let none = BatchDialog::new(BatchMode::Batch, Vec::new());
        let mut spec = ready(BatchMode::Batch).spec().clone();
        spec.action = None;
        let mut empty = none.clone();
        empty.set_spec(spec);
        assert!(
            empty.blocked_reason().is_some(),
            "no recorded action to play"
        );
    }

    #[test]
    fn convert_formats_needs_no_action_and_carries_quality_and_scale() {
        let mut dialog = ready(BatchMode::ConvertFormats);
        let mut spec = dialog.spec().clone();
        spec.action = None;
        spec.format = BatchFormat::Jpeg;
        spec.quality = 55;
        spec.scale_percent = 50;
        dialog.set_spec(spec);
        let confirmed = match dialog.resolve(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(spec) => spec,
            other => panic!("Enter did not confirm: {other:?}"),
        };
        assert_eq!(confirmed.export_format(), ExportFormat::Jpeg(55));
        assert_eq!(confirmed.scale(), 0.5);
        assert_eq!(
            dialog.resolve(DialogKeys {
                confirm: true,
                cancel: true
            }),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn every_format_has_a_label_and_the_folder_request_is_taken_once() {
        for format in BatchFormat::ALL {
            assert!(!format.label().is_empty());
        }
        let mut dialog = ready(BatchMode::Batch);
        dialog.folder_request = Some(BatchFolder::Source);
        assert_eq!(dialog.take_folder_request(), Some(BatchFolder::Source));
        assert_eq!(dialog.take_folder_request(), None);
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = ready(BatchMode::Batch);
            assert!(dialog.show(ctx).is_open());
            let mut convert = ready(BatchMode::ConvertFormats);
            assert!(convert.show(ctx).is_open());
        });
    }
}
