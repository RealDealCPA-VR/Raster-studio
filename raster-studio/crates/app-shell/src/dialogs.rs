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

/// W15-A: AVIF and HEIC are decoded in a child process (the editor's own
/// binary, run with `--decode-worker`), so a decoder panic cannot close the
/// editor. Declared here, beside the open filters that offer those files.
#[path = "decode_worker.rs"]
pub mod decode_worker;

/// What the user chose when asked about unsaved work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    Save,
    Discard,
    Cancel,
}

/// File filters, shared by the native dialog and the tests' assertions.
///
/// W10-F: Netpbm (`ppm`/`pgm`/`pbm`/`pnm`), DDS, GIMP XCF (opened as a
/// layered document, `import::document_from_xcf`) and JPEG XL join the list.
///
/// W15-A: AVIF joins it, read in the decode worker process
/// ([`decode_worker`]); `.heic` / `.heif`, read the same way, are offered
/// through [`HEIF_EXTENSIONS`].
///
/// W11-H: OpenEXR and Radiance HDR (opened as 32 Bits/Channel documents), Apple ICNS,
/// Amiga IFF ILBM/PBM and Krita KRA (its merged image) join the list.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "tif", "tiff", "gif", "bmp", "ico", "tga", "svg", "ppm", "pgm",
    "pbm", "pnm", "dds", "xcf", "jxl", "exr", "hdr", "icns", "iff", "ilbm", "lbm", "kra",
    // W13-D: gzip-compressed SVG; PDF / AI (one artboard per page); EMF /
    // WMF; EPS, PDN, Sketch, XD and FIG through their embedded previews.
    "svgz", "pdf", "ai", "eps", "wmf", "emf", "pdn", "sketch", "xd", "fig",
    // W13-C: Adobe DNG, developed into a 16 Bits/Channel document. The
    // vendor RAWs (CR2, CR3, NEF, ARW, RAF, ORF, RW2) are not offered: they
    // are recognised and refused by name (`raster::codec::formats::raw`).
    // W15-A: AVIF, decoded in the decode worker process.
    "dng", "avif",
    // W16-L: JPEG 2000, VTF, FITS, DICOM, DXF (drawn); Clip Studio, zipped
    // Pixelmator Pro, CorelDRAW and InDesign through their embedded
    // previews. Affinity Photo and PaintTool SAI are recognised and
    // refused by name, so they are not offered.
    "jp2", "j2k", "jpf", "vtf", "fits", "fit", "fts", "dcm", "dxf", "clip", "pxd", "cdr", "indd",
];
/// W15-A: HEIC / HEIF, which File > Open reads in the decode worker process
/// ([`decode_worker`]). Kept apart from [`IMAGE_EXTENSIONS`] because
/// `raster::ImportFormat` has no `.heic` spelling: the codec finds a HEIC by
/// its `ftyp` brand, whatever its name.
pub const HEIF_EXTENSIONS: &[&str] = &["heic", "heif"];
/// W10-F: extension of Photoshop's large-document format, opened through the
/// same layered road as a `.psd` (both start `8BPS`; the `psd` crate reads
/// version 2).
pub const PSB_EXTENSION: &str = "psb";
/// W9-N: Photoshop / Photopea resource files File > Open feeds into their
/// libraries (patterns, gradients, custom shapes, swatches, a profile) — see
/// `asset_store::resources`. `.atn` (W13-E, W16-H) adds its action set to
/// the Actions panel, where its steps play.
pub const RESOURCE_EXTENSIONS: &[&str] = asset_store::resources::ResourceKind::EXTENSIONS;
/// Extension of a Raster Studio project package (a directory).
pub const PROJECT_EXTENSION: &str = "rstudio";
/// W9-E: extension of a Photoshop brush file, which File > Open reads into
/// the Brushes panel ([`crate::editor::Editor::import_abr`]).
pub const ABR_EXTENSION: &str = "abr";
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
    // W15-A: HEIC / HEIF, through the decode worker.
    everything.extend_from_slice(HEIF_EXTENSIONS);
    everything.push(PSD_EXTENSION);
    everything.push(PSB_EXTENSION);
    // W9-E: a Photoshop brush file opens into the Brushes panel.
    everything.push(ABR_EXTENSION);
    // W9-N: resource files feed their libraries.
    everything.extend_from_slice(RESOURCE_EXTENSIONS);
    // W9-K: a font file loads its faces for the session.
    everything.extend_from_slice(FONT_EXTENSIONS);
    // W9-H: a Photoshop style library adds its styles to the style presets.
    everything.push(ASL_EXTENSION);
    // W13-D: a 3D LUT becomes a Color Lookup layer (`editor_open_any`).
    everything.push(CUBE_EXTENSION);
    // W16-L: a curves preset or a .3dl / .look LUT becomes an adjustment layer.
    everything.extend_from_slice(W16_ADJUSTMENT_EXTENSIONS);
    // W13-K: a script opens in the File > Script window.
    everything.extend_from_slice(crate::script::SCRIPT_EXTENSIONS);
    let mut images = IMAGE_EXTENSIONS.to_vec();
    images.extend_from_slice(HEIF_EXTENSIONS);
    images.push(PSD_EXTENSION);
    images.push(PSB_EXTENSION);
    vec![
        ("Raster Studio projects and images", everything),
        ("Raster Studio project", project),
        ("Images", images),
        ("Fonts", FONT_EXTENSIONS.to_vec()),
        ("Photoshop brushes", vec![ABR_EXTENSION]),
        ("Photoshop resources", RESOURCE_EXTENSIONS.to_vec()),
        ("Photoshop styles", vec![ASL_EXTENSION]),
        ("Color lookup tables", vec![CUBE_EXTENSION]),
        // W16-L.
        (
            "Curves presets and 3D LUTs",
            W16_ADJUSTMENT_EXTENSIONS.to_vec(),
        ),
        ("Scripts", crate::script::SCRIPT_EXTENSIONS.to_vec()),
        ("All files", vec!["*"]),
    ]
}

/// W16-L: a Photoshop Curves preset (`.acv`) and the `.3dl` / `.look` 3D
/// LUTs, which File > Open turns into a Curves / Color Lookup adjustment
/// layer (`resource_import::w16`).
pub const W16_ADJUSTMENT_EXTENSIONS: &[&str] = &["acv", "3dl", "look"];

/// W13-D: extension of a 3D LUT, which File > Open turns into a Color Lookup
/// adjustment layer ([`crate::editor::Editor::import_cube_lut`]).
pub const CUBE_EXTENSION: &str = "cube";

/// W9-H: extension of a Photoshop style library, whose styles
/// [`crate::editor::Editor::import_style_library`] adds to the style presets.
pub const ASL_EXTENSION: &str = "asl";

/// W9-H: whether `path` names a style library (by extension, any case).
pub fn is_style_library_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ASL_EXTENSION))
}

/// W9-K: the font files File > Open loads for the session.
pub const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "otc", "woff", "woff2"];

/// W9-K: whether `path` names a font file (by extension, any case).
pub fn is_font_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| FONT_EXTENSIONS.iter().any(|f| f.eq_ignore_ascii_case(e)))
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
    /// W11-H: set with the PSD arming when the canvas is past what a `.psd`
    /// can describe, so that picker offers `.psb` first; consumed with it.
    static PSB_PREFERRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// W13X-6: a file name a script passed to `app.open` or `saveAs`, for
    /// the *next* Open or Export picker on this thread; consumed by it.
    static SUGGESTED_NAME: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// W13X-6: suggest `name` to the next Open or Export picker — the name a
/// script passed to `app.open(file)` or `doc.saveAs(file)`. Only its last
/// path component is kept, so a script can prefill the picker's file-name
/// box but never choose a folder: the user still confirms where the file
/// comes from or goes. An empty name suggests nothing.
pub fn suggest_next_file_name(name: &str) {
    let name = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.trim().is_empty());
    SUGGESTED_NAME.with(|slot| *slot.borrow_mut() = name);
}

/// W13X-6: the name [`suggest_next_file_name`] left for the next picker, if
/// any; consumed on read, so one suggestion reaches exactly one picker.
pub fn take_suggested_file_name() -> Option<String> {
    SUGGESTED_NAME.with(|slot| slot.borrow_mut().take())
}

/// W11-H: File ▸ Save as PSD… for a `width x height` canvas: arms the next
/// export picker as a PSD save ([`arm_psd_save`]) and, past
/// `psd::write::MAX_DIMENSION` (30 000 px) on either edge, makes it lead with
/// Photoshop's large document format and suggest a `.psb` name.
pub fn arm_psd_save_for((width, height): (u32, u32)) {
    arm_psd_save();
    let large = width > psd::write::MAX_DIMENSION || height > psd::write::MAX_DIMENSION;
    PSB_PREFERRED.with(|preferred| preferred.set(large));
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
        // W13X-6: a script's `saveAs(name)` renames the suggestion, keeping
        // the document's folder; without an extension of its own the name
        // keeps the one the document would export as.
        let renamed = take_suggested_file_name().map(|name| {
            let mut path = suggested.with_file_name(&name);
            if Path::new(&name).extension().is_none() {
                if let Some(ext) = suggested.extension() {
                    path.set_extension(ext);
                }
            }
            path
        });
        let suggested = renamed.as_deref().unwrap_or(suggested);
        let psd = take_psd_save();
        // W11-H: consumed on every picker, used only when armed.
        let psb = PSB_PREFERRED.with(|preferred| preferred.replace(false)) && psd;
        let mut filters: Vec<(&'static str, &'static [&'static str])> = Vec::new();
        if psb {
            // W11-H: a canvas no `.psd` can describe leads with `.psb`.
            filters.push(("Photoshop large document", &[PSB_EXTENSION]));
            filters.push(("Photoshop", &[PSD_EXTENSION]));
        } else if psd {
            filters.push(("Photoshop", &[PSD_EXTENSION]));
            // W11-H: the large document format, for a canvas past 30 000 px
            // (a `.psd` save of one is refused with advice to pick this).
            filters.push(("Photoshop large document", &[PSB_EXTENSION]));
        }
        filters.extend([
            ("PNG", &["png"] as &[&str]),
            ("JPEG", &["jpg", "jpeg"]),
            ("WebP", &["webp"]),
            ("TIFF", &["tif", "tiff"]),
            ("GIF", &["gif"]),
            ("BMP", &["bmp"]),
            ("TGA", &["tga"]),
            // W10-F: the formats `export_format_for` maps by extension.
            ("PPM", &["ppm"]),
            ("PGM", &["pgm"]),
            ("PBM", &["pbm"]),
            ("DDS", &["dds"]),
            ("AVIF", &["avif"]),
            // W11-H: `export_format_for` maps these too; a 32 Bits/Channel
            // document writes its float composite to `.exr`.
            ("OpenEXR", &["exr"]),
            ("JPEG XL", &["jxl"]),
            // Shape layers as paths, text as text (`doc::write_vector_svg`).
            ("SVG", &["svg"]),
        ]);
        if !psd {
            filters.push(("Photoshop large document", &[PSB_EXTENSION]));
            filters.push(("Photoshop", &[PSD_EXTENSION]));
        }
        Self {
            title: if psb {
                "Save as PSB"
            } else if psd {
                "Save as PSD"
            } else {
                "Export"
            },
            filters,
            suggested: if psb {
                suggested.with_extension(PSB_EXTENSION)
            } else if psd {
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
        // W13X-6: a script's `app.open(name)` prefills the file-name box.
        if let Some(name) = take_suggested_file_name() {
            dialog = dialog.set_file_name(name);
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
    /// W13X-6: the file name each Open picker was opened with (a script's
    /// `app.open(name)`), `None` for a plain File ▸ Open.
    pub open_suggestions: Vec<Option<String>>,
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
        // Consumes the suggestion exactly as the native picker does.
        self.open_suggestions.push(take_suggested_file_name());
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

    /// W13-C: a `.dng` the picker offers opens as a 16 Bits/Channel document
    /// of the developed size and colours on both open roads:
    ///
    /// - File > Open: `Action::Open` -> `act_open` -> `request_open` ->
    ///   `jobs::spawn_import_with` (its worker calls
    ///   `DecodedImage::decode_bytes`, no extension hint) -> `poll_imports`
    ///   -> `apply_import` -> `OpenDocument::open_image_decoded`;
    /// - the synchronous road Open Recent, startup files and drops take:
    ///   `OpenDocument::open_image`.
    ///
    /// A vendor RAW is refused naming its format on both roads instead of
    /// opening its embedded preview.
    #[test]
    fn a_dng_opens_as_a_sixteen_bit_document_and_a_vendor_raw_is_refused_by_name() {
        use raster::codec::formats::raw::fixture::{dng, srgb16, Spec};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.dng");
        let spec = Spec {
            width: 20,
            height: 12,
            orientation: 6,
            ..Spec::default()
        };
        std::fs::write(&path, dng(&spec, |_, _| [0.4, 0.25, 0.1])).unwrap();
        let developed = |open: &mut crate::doc::OpenDocument, road: &str| {
            assert_eq!(open.document.meta.bit_depth, 16, "{road}");
            // Orientation 6: the 20x12 sensor image stands up as 12x20.
            assert_eq!(
                (open.document.width(), open.document.height()),
                (12, 20),
                "{road}"
            );
            let rect = open.canvas_rect();
            let px = open.composite(rect).unwrap();
            let centre = ((10 * 12 + 6) * 4) as usize;
            for (c, want) in [0.4f64, 0.25, 0.1].into_iter().enumerate() {
                let want = i32::from((srgb16(want) >> 8) as u8);
                let got = i32::from(px[centre + c]);
                assert!(
                    (got - want).abs() <= 3,
                    "{road}: channel {c}: {got} vs {want}"
                );
            }
        };
        // A TIFF-shaped Nikon file, to be refused as a NEF, by name.
        let nef = dir.path().join("shot.nef");
        let mut tiff = b"II*\0\x08\0\0\0\x03\0".to_vec();
        for (tag, kind, value) in [(262u16, 3u16, 32803u32), (256, 4, 8), (271, 2, 50)] {
            tiff.extend(tag.to_le_bytes());
            tiff.extend(kind.to_le_bytes());
            tiff.extend(if tag == 271 { 6u32 } else { 1u32 }.to_le_bytes());
            tiff.extend(value.to_le_bytes());
        }
        tiff.extend(0u32.to_le_bytes());
        tiff.extend(b"NIKON\0");
        std::fs::write(&nef, &tiff).unwrap();

        // File > Open: the picker answers, the job runs (the editor's
        // default spawner is inline), the shell's poll applies it.
        let mut ed = crate::editor::Editor::with_state(
            crate::prefs::AppPaths::rooted(dir.path().join("config")),
            crate::prefs::Preferences::default(),
            crate::recent::RecentFiles::new(),
            Box::new(ScriptedDialogs::new().opening(&path).opening(&nef)),
        );
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        ed.dispatch(crate::action::Action::Open).unwrap();
        ed.poll_imports();
        assert!(!ed.imports_pending(), "the File > Open job finished");
        let open = ed.active_mut().expect("File > Open made a document");
        developed(open, "File > Open");
        // The NEF on File > Open: no document, the failure reported.
        let before = ed.documents().len();
        ed.dispatch(crate::action::Action::Open).unwrap();
        ed.poll_imports();
        assert_eq!(ed.documents().len(), before, "a NEF makes no document");
        let status = ed.status().unwrap_or_default().to_string();
        assert!(status.starts_with("Could not open"), "{status}");
        // The error the File > Open worker reports is its decode's, which
        // names the format.
        let Err(err) = crate::import::DecodedImage::decode_bytes(&tiff) else {
            panic!("a NEF does not decode on the File > Open worker");
        };
        let text = err.to_string();
        assert!(text.contains("Nikon NEF") && text.contains("DNG"), "{text}");

        // The synchronous road (Open Recent, startup files, drops).
        let mut open =
            crate::doc::OpenDocument::open_image(crate::doc::DocumentId(1300), &path, 10).unwrap();
        developed(&mut open, "OpenDocument::open_image");
        let Err(err) = crate::doc::OpenDocument::open_image(crate::doc::DocumentId(1301), &nef, 10)
        else {
            panic!("a NEF does not open");
        };
        let text = err.to_string();
        assert!(text.contains("Nikon NEF") && text.contains("DNG"), "{text}");
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
        // W9-N: every resource extension the default filter offers is one
        // File > Open routes to a library rather than to the image decoder.
        let default_filter = &open_file_filters()[0].1;
        for ext in RESOURCE_EXTENSIONS {
            assert!(default_filter.contains(ext), ".{ext} is not offered");
            let path = PathBuf::from(format!("/lib/thing.{ext}"));
            assert!(crate::editor::Editor::is_resource_path(&path), ".{ext}");
        }
        assert!(default_filter.contains(&"svg"), "SVG is not offered");
        // W10-F: the new readers are offered, and so is `.psb`, which opens
        // through the layered PSD road. W15-A: AVIF, HEIC and HEIF, read in
        // the decode worker process, are offered too.
        for ext in [
            "ppm", "pgm", "pbm", "dds", "xcf", "jxl", "psb", "avif", "heic", "heif",
        ] {
            assert!(default_filter.contains(&ext), ".{ext} is not offered");
            assert!(
                open_file_filters()[2].1.contains(&ext),
                ".{ext} not in Images"
            );
        }
        // W13-C: nor are the vendor RAWs, which are refused by name; DNG is
        // offered.
        for ext in ["cr2", "cr3", "nef", "arw", "raf", "orf", "rw2"] {
            assert!(
                !default_filter.contains(&ext),
                ".{ext} is offered but cannot open"
            );
        }
        assert!(default_filter.contains(&"dng") && open_file_filters()[2].1.contains(&"dng"));
        // W13-D: the new document formats are offered as images, and a
        // `.cube` (which File > Open routes to a Color Lookup layer) is
        // offered by the default filter and a filter of its own.
        for ext in [
            "svgz", "pdf", "ai", "eps", "wmf", "emf", "pdn", "sketch", "xd", "fig",
        ] {
            assert!(default_filter.contains(&ext), ".{ext} is not offered");
            assert!(
                open_file_filters()[2].1.contains(&ext),
                ".{ext} not in Images"
            );
        }
        assert!(default_filter.contains(&"cube"), ".cube is not offered");
        assert!(
            open_file_filters()
                .iter()
                .any(|(_, exts)| exts.as_slice() == [CUBE_EXTENSION]),
            "no Color lookup tables filter"
        );
        assert!(crate::editor::Editor::is_library_file(Path::new(
            "/luts/film.cube"
        )));
        // Every export picker filter names a format the exporter writes by
        // that extension.
        let request = ExportPickerRequest::next(Path::new("/work/photo.png"));
        for (label, extensions) in &request.filters {
            for ext in *extensions {
                let path = PathBuf::from(format!("/out/x.{ext}"));
                assert!(
                    crate::doc::export_format_for(&path).is_some()
                        || crate::doc::exports_as_psd(&path)
                        || crate::doc::exports_as_svg(&path),
                    "{label}: .{ext} has no writer"
                );
            }
        }
        // ...and it does not offer the project extension, which it could never
        // return: `pick_open_project` is that question.
        assert!(
            !IMAGE_EXTENSIONS.contains(&PROJECT_EXTENSION),
            "a file picker cannot return a directory"
        );
    }
}
