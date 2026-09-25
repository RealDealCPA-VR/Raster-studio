//! W13-N: File ▸ Automate ▸ Resize Images… and Generate Mockups….
//!
//! Both read every image in a source folder and write one file per image
//! into a destination folder, so they ask the same two questions; Resize
//! Images also asks for the box each image is fitted into. The folders are
//! typed or chosen with the platform picker, which the host runs — the
//! dialog raises [`FolderJobDialog::take_folder_request`] and the host
//! answers with [`FolderJobDialog::set_folder`], exactly as the Batch
//! dialog does. The files themselves are the application's to read and
//! write; this module owns only the question.

use std::path::PathBuf;

use egui::Context;

use super::batch::BatchFolder;
use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, integer};
use super::sizes;
use crate::strings::tr;

/// Which File ▸ Automate command the dialog asks for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FolderJob {
    /// Every image fitted into a box and written again.
    ResizeImages,
    /// The active smart object's contents replaced by each image, the
    /// document exported once per image.
    GenerateMockups,
}

/// What the dialog confirms.
#[derive(Clone, Debug, PartialEq)]
pub struct FolderJobSpec {
    pub job: FolderJob,
    pub source: PathBuf,
    pub destination: PathBuf,
    /// Resize Images: the box each image is fitted into, aspect kept.
    pub max_width: u32,
    pub max_height: u32,
    /// Resize Images: also scale up images smaller than the box.
    pub enlarge: bool,
}

/// The dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct FolderJobDialog {
    spec: FolderJobSpec,
    source_text: String,
    destination_text: String,
    folder_request: Option<BatchFolder>,
}

/// The largest box side the dialog accepts.
pub const MAX_SIDE: i64 = 30_000;

impl FolderJobDialog {
    pub fn new(job: FolderJob) -> Self {
        Self {
            spec: FolderJobSpec {
                job,
                source: PathBuf::new(),
                destination: PathBuf::new(),
                max_width: 1024,
                max_height: 1024,
                enlarge: false,
            },
            source_text: String::new(),
            destination_text: String::new(),
            folder_request: None,
        }
    }

    pub fn spec(&self) -> &FolderJobSpec {
        &self.spec
    }

    /// The fitting box — tests and presets.
    pub fn set_box(&mut self, width: u32, height: u32) {
        self.spec.max_width = width.max(1);
        self.spec.max_height = height.max(1);
    }

    /// Answer a folder request (or fill a field directly).
    pub fn set_folder(&mut self, field: BatchFolder, path: PathBuf) {
        let text = path.display().to_string();
        match field {
            BatchFolder::Source => {
                self.spec.source = path;
                self.source_text = text;
            }
            BatchFolder::Destination => {
                self.spec.destination = path;
                self.destination_text = text;
            }
        }
    }

    /// The folder field whose Choose button was pressed, once.
    pub fn take_folder_request(&mut self) -> Option<BatchFolder> {
        self.folder_request.take()
    }

    fn title(&self) -> &'static str {
        match self.spec.job {
            FolderJob::ResizeImages => tr("ui.folder_job.resize.title"),
            FolderJob::GenerateMockups => tr("ui.folder_job.mockups.title"),
        }
    }

    /// Why OK is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<&'static str> {
        if self.spec.source.as_os_str().is_empty() || self.spec.destination.as_os_str().is_empty() {
            return Some(tr("ui.folder_job.need_folders"));
        }
        if self.spec.source == self.spec.destination {
            return Some(tr("ui.folder_job.same_folder"));
        }
        None
    }

    /// The spec OK hands over, when both folders are set and differ.
    pub fn confirm(&self) -> Option<FolderJobSpec> {
        self.blocked_reason().is_none().then(|| self.spec.clone())
    }

    /// Escape and Enter, without drawing.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<FolderJobSpec> {
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

    /// Draw one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<FolderJobSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(
            ctx,
            "w13n-folder-job",
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
            BatchFolder::Source => (tr("ui.batch.source"), "w13n-folder-job-source"),
            BatchFolder::Destination => (tr("ui.batch.destination"), "w13n-folder-job-destination"),
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
            match self.spec.job {
                FolderJob::ResizeImages => tr("ui.folder_job.resize.subtitle"),
                FolderJob::GenerateMockups => tr("ui.folder_job.mockups.subtitle"),
            },
        );
        self.folder_row(ui, BatchFolder::Source);
        self.folder_row(ui, BatchFolder::Destination);
        if self.spec.job == FolderJob::ResizeImages {
            let mut w = i64::from(self.spec.max_width);
            let mut h = i64::from(self.spec.max_height);
            design::inspector_field(ui, tr("ui.folder_job.width"), |ui| {
                integer(ui, &mut w, 1..=MAX_SIDE);
            });
            design::inspector_field(ui, tr("ui.folder_job.height"), |ui| {
                integer(ui, &mut h, 1..=MAX_SIDE);
            });
            self.set_box(w as u32, h as u32);
            checkbox_row(ui, tr("ui.folder_job.enlarge"), &mut self.spec.enlarge);
        }
        action_row(ui, tr("ui.folder_job.run"), self.blocked_reason(), &[])
    }
}

/// The size an image of `width x height` is written at when fitted into
/// `max_width x max_height` with its aspect kept; smaller images keep their
/// size unless `enlarge`.
pub fn fitted_size(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
    enlarge: bool,
) -> (u32, u32) {
    let (w, h) = (f64::from(width.max(1)), f64::from(height.max(1)));
    let mut s = (f64::from(max_width.max(1)) / w).min(f64::from(max_height.max(1)) / h);
    if !enlarge {
        s = s.min(1.0);
    }
    (
        ((w * s).round() as u32).max(1),
        ((h * s).round() as u32).max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [
            "ui.folder_job.resize.title",
            "ui.folder_job.mockups.title",
            "ui.folder_job.resize.subtitle",
            "ui.folder_job.mockups.subtitle",
            "ui.folder_job.need_folders",
            "ui.folder_job.same_folder",
            "ui.folder_job.width",
            "ui.folder_job.height",
            "ui.folder_job.enlarge",
            "ui.folder_job.run",
        ] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn ok_needs_two_different_folders() {
        let mut d = FolderJobDialog::new(FolderJob::ResizeImages);
        assert!(d.confirm().is_none());
        d.set_folder(BatchFolder::Source, PathBuf::from("a"));
        d.set_folder(BatchFolder::Destination, PathBuf::from("a"));
        assert!(d.confirm().is_none());
        d.set_folder(BatchFolder::Destination, PathBuf::from("b"));
        assert_eq!(d.confirm().unwrap().destination, PathBuf::from("b"));
    }

    #[test]
    fn images_fit_the_box_with_their_aspect_kept() {
        assert_eq!(fitted_size(2000, 1000, 500, 500, false), (500, 250));
        assert_eq!(fitted_size(100, 50, 500, 500, false), (100, 50));
        assert_eq!(fitted_size(100, 50, 500, 500, true), (500, 250));
    }
}
