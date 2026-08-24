//! The seam between the editor's logic and the platform's file dialogs.
//!
//! Every question the application has to ask the *operating system* goes
//! through [`FileDialogs`]. That is not indirection for its own sake: it is
//! what lets "Ctrl+O opens a file and it lands in a tab" be a unit test rather
//! than a claim. [`NativeDialogs`] is the real implementation over `rfd`;
//! [`ScriptedDialogs`] answers from a queue and records what it was asked.

use std::path::{Path, PathBuf};

/// What the user chose when asked about unsaved work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    Save,
    Discard,
    Cancel,
}

/// File filters, shared by the native dialog and the tests' assertions.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "tif", "tiff", "gif", "bmp", "ico", "tga",
];
/// Extension of a Raster Studio project package (a directory).
pub const PROJECT_EXTENSION: &str = "rstudio";

/// Everything the shell needs to ask the platform.
pub trait FileDialogs {
    /// "Open…". `None` means the user cancelled.
    fn pick_open_file(&mut self) -> Option<PathBuf>;
    /// "Open Project…". A separate question because a `.rstudio` package is a
    /// **directory**, and no file picker can return one — which is why File ▸
    /// Open could not open the application's own save format at all.
    fn pick_open_project(&mut self) -> Option<PathBuf>;
    /// "Save As…", starting at `suggested`.
    fn pick_save_path(&mut self, suggested: &Path) -> Option<PathBuf>;
    /// "Export…", starting at `suggested`.
    fn pick_export_path(&mut self, suggested: &Path) -> Option<PathBuf>;
    /// "Export Layers…" — where to write the per-layer files.
    fn pick_export_folder(&mut self) -> Option<PathBuf>;
    /// Closing a document with unsaved changes.
    fn confirm_close(&mut self, document: &str) -> CloseChoice;
    /// A previous run crashed with unsaved work in `document`.
    fn confirm_recover(&mut self, document: &str) -> bool;
    /// Something failed and the user has to be told. This is the path that
    /// exists so a GPU failure is a dialog rather than a silent abort.
    fn report_error(&mut self, title: &str, message: &str);
}

/// The real dialogs.
#[derive(Debug, Default)]
pub struct NativeDialogs;

impl FileDialogs for NativeDialogs {
    fn pick_open_file(&mut self) -> Option<PathBuf> {
        // No project filter here: a `.rstudio` package is a directory, so a
        // file picker can never return one and the filter only advertised a
        // capability this dialog does not have. Projects come through
        // `pick_open_project`.
        rfd::FileDialog::new()
            .add_filter("Images", IMAGE_EXTENSIONS)
            .add_filter("All files", &["*"])
            .set_title("Open")
            .pick_file()
    }

    fn pick_open_project(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Open Project")
            .pick_folder()
    }

    fn pick_save_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Raster Studio project", &[PROJECT_EXTENSION])
            .set_title("Save As");
        if let Some(dir) = suggested.parent() {
            if dir.is_dir() {
                dialog = dialog.set_directory(dir);
            }
        }
        if let Some(name) = suggested.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        dialog.save_file()
    }

    fn pick_export_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PNG", &["png"])
            .add_filter("JPEG", &["jpg", "jpeg"])
            .add_filter("WebP", &["webp"])
            .add_filter("TIFF", &["tif", "tiff"])
            .add_filter("GIF", &["gif"])
            .add_filter("BMP", &["bmp"])
            .set_title("Export");
        if let Some(dir) = suggested.parent() {
            if dir.is_dir() {
                dialog = dialog.set_directory(dir);
            }
        }
        if let Some(name) = suggested.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        dialog.save_file()
    }

    fn pick_export_folder(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Export Layers")
            .pick_folder()
    }

    fn confirm_close(&mut self, document: &str) -> CloseChoice {
        // rfd's three-button set is Yes/No/Cancel; the labels below say which
        // is which so "No" cannot be read as "do not close".
        match rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Unsaved changes")
            .set_description(format!(
                "“{document}” has changes that have not been saved.\n\n\
                 Yes — save and close.  No — discard them.  Cancel — keep editing."
            ))
            .set_buttons(rfd::MessageButtons::YesNoCancel)
            .show()
        {
            rfd::MessageDialogResult::Yes => CloseChoice::Save,
            rfd::MessageDialogResult::No => CloseChoice::Discard,
            _ => CloseChoice::Cancel,
        }
    }

    fn confirm_recover(&mut self, document: &str) -> bool {
        matches!(
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Warning)
                .set_title("Recover unsaved work?")
                .set_description(format!(
                    "Raster Studio closed unexpectedly with unsaved changes to \
                     “{document}”.\n\nRestore them?"
                ))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show(),
            rfd::MessageDialogResult::Yes
        )
    }

    fn report_error(&mut self, title: &str, message: &str) {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(title)
            .set_description(message)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    }
}

/// Pre-programmed answers, for tests and for headless runs.
///
/// Every queue drains from the front; an exhausted queue answers "cancel",
/// which is the safe default for a prompt nobody is there to answer.
#[derive(Debug, Default)]
pub struct ScriptedDialogs {
    pub open_files: Vec<PathBuf>,
    /// Answers for the folder picker behind "Open Project…".
    pub open_projects: Vec<PathBuf>,
    pub save_paths: Vec<PathBuf>,
    pub export_paths: Vec<PathBuf>,
    /// Where to write exported per-layer files (Export Layers…).
    pub export_folders: Vec<PathBuf>,
    pub close_choices: Vec<CloseChoice>,
    pub recover_answers: Vec<bool>,
    /// Every error the editor reported, in order: `(title, message)`.
    pub errors: Vec<(String, String)>,
    /// Every `suggested` path a save/export dialog was opened at.
    pub suggested: Vec<PathBuf>,
}

impl ScriptedDialogs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn opening(mut self, path: impl Into<PathBuf>) -> Self {
        self.open_files.push(path.into());
        self
    }

    pub fn opening_project(mut self, path: impl Into<PathBuf>) -> Self {
        self.open_projects.push(path.into());
        self
    }

    pub fn saving_to(mut self, path: impl Into<PathBuf>) -> Self {
        self.save_paths.push(path.into());
        self
    }

    pub fn exporting_to(mut self, path: impl Into<PathBuf>) -> Self {
        self.export_paths.push(path.into());
        self
    }

    pub fn exporting_folder(mut self, path: impl Into<PathBuf>) -> Self {
        self.export_folders.push(path.into());
        self
    }

    pub fn answering_close(mut self, choice: CloseChoice) -> Self {
        self.close_choices.push(choice);
        self
    }

    pub fn answering_recover(mut self, yes: bool) -> Self {
        self.recover_answers.push(yes);
        self
    }
}

impl FileDialogs for ScriptedDialogs {
    fn pick_open_file(&mut self) -> Option<PathBuf> {
        (!self.open_files.is_empty()).then(|| self.open_files.remove(0))
    }

    fn pick_open_project(&mut self) -> Option<PathBuf> {
        (!self.open_projects.is_empty()).then(|| self.open_projects.remove(0))
    }

    fn pick_save_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        self.suggested.push(suggested.to_path_buf());
        (!self.save_paths.is_empty()).then(|| self.save_paths.remove(0))
    }

    fn pick_export_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        self.suggested.push(suggested.to_path_buf());
        (!self.export_paths.is_empty()).then(|| self.export_paths.remove(0))
    }

    fn pick_export_folder(&mut self) -> Option<PathBuf> {
        (!self.export_folders.is_empty()).then(|| self.export_folders.remove(0))
    }

    fn confirm_close(&mut self, _document: &str) -> CloseChoice {
        if self.close_choices.is_empty() {
            CloseChoice::Cancel
        } else {
            self.close_choices.remove(0)
        }
    }

    fn confirm_recover(&mut self, _document: &str) -> bool {
        !self.recover_answers.is_empty() && self.recover_answers.remove(0)
    }

    fn report_error(&mut self, title: &str, message: &str) {
        self.errors.push((title.to_string(), message.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scripted_queue_drains_in_order_then_cancels() {
        let mut d = ScriptedDialogs::new()
            .opening("/a.png")
            .opening("/b.png")
            .answering_close(CloseChoice::Discard);
        assert_eq!(d.pick_open_file(), Some(PathBuf::from("/a.png")));
        assert_eq!(d.pick_open_file(), Some(PathBuf::from("/b.png")));
        assert_eq!(d.pick_open_file(), None, "an empty queue cancels");
        assert_eq!(d.confirm_close("x"), CloseChoice::Discard);
        assert_eq!(
            d.confirm_close("x"),
            CloseChoice::Cancel,
            "the safe default keeps the document open"
        );
        assert!(!d.confirm_recover("x"), "and does not restore anything");
    }

    #[test]
    fn a_save_dialog_records_where_it_was_opened() {
        let mut d = ScriptedDialogs::new().saving_to("/out/final.rstudio");
        let chosen = d.pick_save_path(Path::new("/work/photo.rstudio"));
        assert_eq!(chosen, Some(PathBuf::from("/out/final.rstudio")));
        assert_eq!(d.suggested, [PathBuf::from("/work/photo.rstudio")]);
    }

    #[test]
    fn reported_errors_are_recorded_rather_than_shown() {
        let mut d = ScriptedDialogs::new();
        d.report_error("Graphics failure", "no adapter");
        assert_eq!(d.errors.len(), 1);
        assert_eq!(d.errors[0].0, "Graphics failure");
    }

    #[test]
    fn opening_a_project_is_a_question_of_its_own() {
        // The defect: `pick_open_file` carried a `.rstudio` filter, but a
        // package is a *directory* and no file picker can return one — so File
        // ▸ Open could never open the application's own save format, and the
        // filter advertised a capability the dialog did not have.
        let mut d = ScriptedDialogs::new()
            .opening("/photo.png")
            .opening_project("/work/piece.rstudio");
        assert_eq!(d.pick_open_file(), Some(PathBuf::from("/photo.png")));
        assert_eq!(
            d.pick_open_project(),
            Some(PathBuf::from("/work/piece.rstudio")),
            "the folder picker is what answers for a package"
        );
        assert_eq!(d.pick_open_project(), None, "an empty queue cancels");
        // The two queues are separate: a project answer must not be handed out
        // as a file answer, or the file picker would look like it worked.
        let mut d = ScriptedDialogs::new().opening_project("/work/piece.rstudio");
        assert_eq!(d.pick_open_file(), None);
    }

    #[test]
    fn the_import_filter_covers_what_the_codec_reads() {
        // A filter that offers a format the decoder cannot read (or hides one
        // it can) is a dialog that lies about what the app opens.
        for ext in IMAGE_EXTENSIONS {
            assert!(
                raster::ImportFormat::from_extension(ext).is_some(),
                "the open dialog offers .{ext}, which the codec cannot decode"
            );
        }
        // ...and it does not offer the project extension, which it could never
        // return: `pick_open_project` is that question.
        assert!(
            !IMAGE_EXTENSIONS.contains(&PROJECT_EXTENSION),
            "a file picker cannot return a directory"
        );
    }
}
