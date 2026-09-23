//! The seam between the editor's logic and the platform's file dialogs.
//!
//! Every question the application has to ask the *operating system* goes
//! through [`FileDialogs`]. That is not indirection for its own sake: it is
//! what lets "Ctrl+O opens a file and it lands in a tab" be a unit test rather
//! than a claim. [`NativeDialogs`] is the real implementation over `rfd`;
//! [`ScriptedDialogs`] answers from a queue and records what it was asked.
//!
//! [`UrlLauncher`] is the same idea for the Help menu's browser launches: the
//! shipped [`BrowserUrls`] calls `webbrowser::open`, and tests inject
//! [`RecordingUrls`] so a digest gate can reach those arms without opening
//! tabs on a CI runner.

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
/// W5-D: File ▸ Open's filters, in the order the picker lists them. A
/// package is a folder, which a file picker cannot return — but the user can
/// step into it and pick its `manifest.json`, and the editor maps any file
/// inside a `*.rstudio` folder to the package
/// ([`crate::editor::Editor::project_package_for`]). So the first (default)
/// filter shows projects AND images — everything File ▸ Open can open —
/// followed by a projects-only filter, images, and everything.
pub fn open_file_filters() -> Vec<(&'static str, Vec<&'static str>)> {
    let project = vec![PROJECT_EXTENSION, "json"];
    let mut everything = project.clone();
    everything.extend_from_slice(IMAGE_EXTENSIONS);
    everything.push(PSD_EXTENSION);
    let mut images = IMAGE_EXTENSIONS.to_vec();
    images.push(PSD_EXTENSION);
    vec![
        ("Raster Studio projects and images", everything),
        ("Raster Studio project", project),
        ("Images", images),
        ("All files", vec!["*"]),
    ]
}

/// Extension of a layered Photoshop document, which the export path writes
/// through the `psd` crate (`OpenDocument::export_to` picks the writer by
/// this extension).
pub const PSD_EXTENSION: &str = "psd";

thread_local! {
    /// Set by File ▸ Save as PSD… for the *next* export picker on this
    /// thread, and cleared by that picker. The editor's `Export` action owns
    /// the only road to the export picker and the `.psd` writer behind it;
    /// this flag is how the menu row makes that one call a PSD save (PSD
    /// filter first, a `.psd` suggestion, the right title) without a second
    /// action. Thread-local, like the parked adjustment parameters in
    /// `dialog_host`, so parallel tests cannot arm each other's pickers.
    static PSD_SAVE_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arm the next export picker as a PSD save.
pub fn arm_psd_save() {
    PSD_SAVE_ARMED.with(|armed| armed.set(true));
}

/// Whether the next export picker was armed as a PSD save; consumed on read,
/// so one arming affects exactly one picker.
pub fn take_psd_save() -> bool {
    PSD_SAVE_ARMED.with(|armed| armed.replace(false))
}

/// `suggested` with its extension swapped for `.psd`, for the armed picker.
pub fn psd_save_suggestion(suggested: &Path) -> PathBuf {
    suggested.with_extension(PSD_EXTENSION)
}

/// One export picker as it is about to be shown: its title, its filters in
/// the order the platform dialog lists them (the first is the one the dialog
/// opens on), and the file name it suggests.
///
/// [`NativeDialogs::pick_export_path`] builds its `rfd` dialog from this and
/// nothing else, and [`ScriptedDialogs`] records the same value — so a test
/// that asserts "Save as PSD led with the PSD filter" is reading the request
/// the real picker is built from, not a claim beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPickerRequest {
    pub title: &'static str,
    /// `(label, extensions)`, first entry first.
    pub filters: Vec<(&'static str, &'static [&'static str])>,
    pub suggested: PathBuf,
}

impl ExportPickerRequest {
    /// The request for the next export picker, starting at `suggested`.
    ///
    /// Consumes the PSD arming ([`take_psd_save`]): armed, the PSD filter
    /// leads, the title says "Save as PSD" and the suggestion wears `.psd`;
    /// unarmed, it is the plain Export picker, which offers PSD too, last,
    /// because `export_to` writes a layered `.psd` by extension either way.
    pub fn next(suggested: &Path) -> Self {
        let psd = take_psd_save();
        let mut filters: Vec<(&'static str, &'static [&'static str])> = Vec::new();
        if psd {
            filters.push(("Photoshop", &[PSD_EXTENSION]));
        }
        filters.extend([
            ("PNG", &["png"] as &[&str]),
            ("JPEG", &["jpg", "jpeg"]),
            ("WebP", &["webp"]),
            ("TIFF", &["tif", "tiff"]),
            ("GIF", &["gif"]),
            ("BMP", &["bmp"]),
        ]);
        if !psd {
            filters.push(("Photoshop", &[PSD_EXTENSION]));
        }
        Self {
            title: if psd { "Save as PSD" } else { "Export" },
            filters,
            suggested: if psd {
                psd_save_suggestion(suggested)
            } else {
                suggested.to_path_buf()
            },
        }
    }

    /// Whether the picker opens on the PSD filter — what File ▸ Save as
    /// PSD… differs from File ▸ Export… by.
    pub fn leads_with_psd(&self) -> bool {
        self.filters
            .first()
            .is_some_and(|(_, extensions)| *extensions == [PSD_EXTENSION])
    }

    /// The suggested file name's extension, lower-cased.
    pub fn suggested_extension(&self) -> Option<String> {
        self.suggested
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
    }
}

/// Everything the shell needs to ask the platform.
pub trait FileDialogs {
    /// "Open…". `None` means the user cancelled.
    fn pick_open_file(&mut self) -> Option<PathBuf>;
    /// "Place Embedded…"/"Place Linked…". `None` means the user cancelled.
    fn pick_place_file(&mut self) -> Option<PathBuf>;
    /// Card 069: "Replace Contents…" on a smart object. A separate question
    /// so a scripted test can answer Place and Replace independently.
    /// `None` means the user cancelled.
    fn pick_replace_file(&mut self) -> Option<PathBuf>;
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
    /// Card 077: a non-fatal notice after an operation succeeded — the PSD
    /// import fidelity report. Information, not an error.
    fn report_notice(&mut self, title: &str, message: &str);
}

/// Opens a Help destination in the user's browser.
///
/// The Help menu is the one place the editor reaches *outside* its process on
/// a menu click, which makes it both untestable through the real path (a test
/// that opens three browser tabs is a test that deserves to be closed) and
/// worth testing (C5: the shipped `webbrowser::open` call had zero coverage).
/// [`BrowserUrls`] is what ships; [`RecordingUrls`] is the test double.
pub trait UrlLauncher {
    /// Open `url`. `false` means the open failed, and the caller falls back
    /// to reporting where the page lives instead.
    fn open_url(&mut self, url: &str) -> bool;
}

/// The shipped launcher: the platform browser.
pub struct BrowserUrls;

impl UrlLauncher for BrowserUrls {
    fn open_url(&mut self, url: &str) -> bool {
        webbrowser::open(url).is_ok()
    }
}

/// Test double: records every URL it is asked to open, opens nothing.
#[derive(Default)]
pub struct RecordingUrls {
    /// The URLs in the order they were requested.
    pub opened: Vec<String>,
}

impl UrlLauncher for RecordingUrls {
    fn open_url(&mut self, url: &str) -> bool {
        self.opened.push(url.to_string());
        true
    }
}

/// The real dialogs.
#[derive(Debug, Default)]
pub struct NativeDialogs;

impl FileDialogs for NativeDialogs {
    fn pick_open_file(&mut self) -> Option<PathBuf> {
        // W5-D: a `.rstudio` package is a directory, so the picker cannot
        // return it — but it can return the manifest inside it, which the
        // editor maps back to the package. See `open_file_filters`.
        let mut dialog = rfd::FileDialog::new();
        for (name, extensions) in open_file_filters() {
            dialog = dialog.add_filter(name, &extensions);
        }
        dialog.set_title("Open").pick_file()
    }

    fn pick_open_project(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Open Project")
            .pick_folder()
    }

    fn pick_place_file(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("Images", IMAGE_EXTENSIONS)
            .add_filter("All files", &["*"])
            .set_title("Place")
            .pick_file()
    }

    fn pick_replace_file(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("Images", IMAGE_EXTENSIONS)
            .add_filter("All files", &["*"])
            .set_title("Replace Contents")
            .pick_file()
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
        // File ▸ Save as PSD… arms the picker for one call: the PSD filter
        // leads, the title says so, and the suggested name wears `.psd`. The
        // request is built by `ExportPickerRequest::next`, the same value
        // the scripted double records, so what a test asserts about the
        // filters is what this dialog is built from.
        let request = ExportPickerRequest::next(suggested);
        let suggested = request.suggested;
        let mut dialog = rfd::FileDialog::new().set_title(request.title);
        for (label, extensions) in request.filters {
            dialog = dialog.add_filter(label, extensions);
        }
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

    fn report_notice(&mut self, title: &str, message: &str) {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Info)
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
    /// Card 077: every notice the editor reported, in order: `(title, message)`.
    pub notices: Vec<(String, String)>,
    /// Every `suggested` path a save/export dialog was opened at.
    pub suggested: Vec<PathBuf>,
    /// Every export picker as it would have been shown — title, filter order
    /// and suggestion — so a test can see that Save as PSD asked for a PSD
    /// *first*, not only that it suggested one.
    pub export_requests: Vec<ExportPickerRequest>,
    /// Answers for the picker behind "Place Embedded…"/"Place Linked…".
    pub place_files: Vec<PathBuf>,
    /// Answers for the picker behind "Replace Contents…" (card 069).
    pub replace_files: Vec<PathBuf>,
}

impl ScriptedDialogs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn opening(mut self, path: impl Into<PathBuf>) -> Self {
        self.open_files.push(path.into());
        self
    }

    /// Answer for the "Place Embedded…/Place Linked…" picker.
    pub fn placing(mut self, path: impl Into<PathBuf>) -> Self {
        self.place_files.push(path.into());
        self
    }

    /// Answer for the "Replace Contents…" picker (card 069).
    pub fn replacing_with(mut self, path: impl Into<PathBuf>) -> Self {
        self.replace_files.push(path.into());
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

    fn pick_place_file(&mut self) -> Option<PathBuf> {
        (!self.place_files.is_empty()).then(|| self.place_files.remove(0))
    }

    fn pick_replace_file(&mut self) -> Option<PathBuf> {
        (!self.replace_files.is_empty()).then(|| self.replace_files.remove(0))
    }

    fn pick_save_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        self.suggested.push(suggested.to_path_buf());
        (!self.save_paths.is_empty()).then(|| self.save_paths.remove(0))
    }

    fn pick_export_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        // The scripted picker builds the same request the native one does —
        // consuming the PSD arming, leading with the PSD filter, suggesting
        // `.psd` — and records it, so a test can see that Save as PSD asked
        // for a PSD.
        let request = ExportPickerRequest::next(suggested);
        self.suggested.push(request.suggested.clone());
        self.export_requests.push(request);
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

    fn report_notice(&mut self, title: &str, message: &str) {
        self.notices.push((title.to_string(), message.to_string()));
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
    fn an_armed_export_picker_suggests_a_psd_exactly_once() {
        let mut d = ScriptedDialogs::new()
            .exporting_to("/out/a.psd")
            .exporting_to("/out/b.png");
        arm_psd_save();
        assert_eq!(
            d.pick_export_path(Path::new("/work/photo.png")),
            Some(PathBuf::from("/out/a.psd"))
        );
        // The arming is consumed: the next picker is the plain export one.
        assert_eq!(
            d.pick_export_path(Path::new("/work/photo.png")),
            Some(PathBuf::from("/out/b.png"))
        );
        assert_eq!(
            d.suggested,
            [
                PathBuf::from("/work/photo.psd"),
                PathBuf::from("/work/photo.png")
            ]
        );
        assert!(!take_psd_save(), "nothing is left armed");
    }

    #[test]
    fn the_armed_request_leads_with_psd_and_the_plain_one_ends_with_it() {
        // The request is what the native picker is built from, so its
        // filter order is the picker's filter order.
        arm_psd_save();
        let armed = ExportPickerRequest::next(Path::new("/work/photo.png"));
        assert!(armed.leads_with_psd(), "{armed:?}");
        assert_eq!(armed.title, "Save as PSD");
        assert_eq!(armed.suggested_extension().as_deref(), Some("psd"));
        assert_eq!(armed.filters[0], ("Photoshop", &[PSD_EXTENSION] as &[&str]));
        // Unarmed: the plain Export picker, PSD offered last.
        let plain = ExportPickerRequest::next(Path::new("/work/photo.png"));
        assert!(!plain.leads_with_psd(), "{plain:?}");
        assert_eq!(plain.title, "Export");
        assert_eq!(plain.suggested_extension().as_deref(), Some("png"));
        assert_eq!(plain.filters[0].0, "PNG");
        assert_eq!(
            plain.filters.last().map(|f| f.0),
            Some("Photoshop"),
            "the plain picker still offers PSD, last"
        );
        // The same formats either way, only the order differs.
        let mut a: Vec<_> = armed.filters.iter().map(|f| f.0).collect();
        let mut p: Vec<_> = plain.filters.iter().map(|f| f.0).collect();
        a.sort_unstable();
        p.sort_unstable();
        assert_eq!(a, p);
        // And the scripted double records exactly the request it answered.
        arm_psd_save();
        let mut d = ScriptedDialogs::new().exporting_to("/out/a.psd");
        let _ = d.pick_export_path(Path::new("/work/photo.png"));
        assert_eq!(d.export_requests.len(), 1);
        assert!(d.export_requests[0].leads_with_psd());
        assert_eq!(d.suggested, [d.export_requests[0].suggested.clone()]);
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
