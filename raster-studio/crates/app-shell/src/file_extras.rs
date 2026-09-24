//! W10-E: the File-menu gaps and their two Image-menu companions, hosted.
//!
//! * File ▸ Automate ▸ Batch… / Convert Formats… ([`crate::automate`]),
//! * File ▸ Export ▸ Color Lookup Tables… (this module: [`stack_lut`],
//!   [`cube_text`]),
//! * File ▸ Export ▸ PDF… (this module, over [`raster::pdf`]),
//! * File ▸ File Info… as an XMP editor (this module's per-document store;
//!   Export As writes it through [`raster::metadata`]),
//! * Image ▸ Variables ▸ Define… / Data Sets… ([`crate::variables`]),
//! * Image ▸ Vectorize Bitmap… ([`crate::vectorize`]).
//!
//! # The road a confirmation travels
//!
//! Every one of these dialogs takes the road Trim's does
//! (`crate::dialog_host`): the dialog host holds one
//! [`FileExtrasDialog`], [`drive`] draws it, a confirmation is parked here
//! and the menu pick rides `ChromeOutput::menu` to
//! `menu_bridge::perform`, whose arm calls [`perform`] with `&mut Editor`.
//! A pick with nothing parked (a chord, the digest gate) runs at the
//! dialog's defaults where that is meaningful, and refuses loudly where it
//! is not (Batch and Variables need their folders and definitions).
//!
//! # File Info is kept per document, for the session
//!
//! `editor_core::DocumentMeta` has no XMP fields and this wave does not own
//! it, so the fields File Info confirms live in a per-document store here,
//! keyed by [`DocumentId`]: they survive for as long as the document is open
//! and ride every Export As of it, but a `.rstudio` save does not carry them.
//! A document opened from a PNG / JPEG / TIFF that carried an XMP packet
//! starts from that packet's fields.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use raster::metadata::{EmbeddedMetadata, XmpFields};
use ui::dialogs::DialogOutcome;
use ui::menu::MenuAction;

use crate::chrome::ChromeOutput;
use crate::doc::{DocumentId, OpenDocument};
use crate::editor::Editor;

/// Largest source file read back for its XMP / EXIF: 64 MiB.
const MAX_SOURCE_METADATA_BYTES: u64 = 64 << 20;
/// Largest CSV the Data Sets page imports: 16 MiB.
const MAX_CSV_BYTES: u64 = 16 << 20;

/// One of the W10-E dialogs, open in the dialog host.
pub enum FileExtrasDialog {
    Batch(Box<ui::dialogs::BatchDialog>),
    Variables(Box<ui::dialogs::VariablesDialog>),
    ExportLut(Box<ui::dialogs::ExportLutDialog>),
    ExportPdf(Box<ui::dialogs::ExportPdfDialog>),
    Vectorize(Box<ui::dialogs::VectorizeDialog>),
    FileInfo(Box<ui::dialogs::FileInfoDialog>),
}

impl std::fmt::Debug for FileExtrasDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Batch(_) => "Batch",
            Self::Variables(_) => "Variables",
            Self::ExportLut(_) => "ExportLut",
            Self::ExportPdf(_) => "ExportPdf",
            Self::Vectorize(_) => "Vectorize",
            Self::FileInfo(_) => "FileInfo",
        };
        write!(f, "FileExtrasDialog::{name}")
    }
}

/// A confirmation waiting for its menu pick.
enum Parked {
    Batch(ui::dialogs::BatchSpec),
    Variables(ui::dialogs::VariablesSpec),
    Lut(ui::dialogs::ExportLutSpec),
    Pdf(ui::dialogs::PdfExportSpec),
    Vectorize(ui::dialogs::VectorizeSpec),
    FileInfo(XmpFields),
}

thread_local! {
    static PARKED: RefCell<Option<Parked>> = const { RefCell::new(None) };
    /// The File Info fields confirmed per open document (see the module
    /// header).
    static FILE_INFO: RefCell<HashMap<DocumentId, XmpFields>> = RefCell::new(HashMap::new());
}

#[cfg(test)]
thread_local! {
    /// Folders the Batch dialog's Choose buttons answer, in order, in place
    /// of the platform picker.
    pub(crate) static PICKED_FOLDERS_FOR_TEST: RefCell<Vec<PathBuf>> =
        const { RefCell::new(Vec::new()) };
    /// The CSV the Data Sets page's Import answers, in place of the picker.
    pub(crate) static PICKED_CSV_FOR_TEST: RefCell<Option<PathBuf>> =
        const { RefCell::new(None) };
}

fn park(parked: Parked) {
    PARKED.with(|slot| *slot.borrow_mut() = Some(parked));
}

fn take_parked(accept: impl Fn(&Parked) -> bool) -> Option<Parked> {
    PARKED.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(accept) {
            slot.take()
        } else {
            None
        }
    })
}

/// The folder the Batch dialog's Choose button asks for.
fn pick_folder() -> Option<PathBuf> {
    #[cfg(test)]
    {
        let next = PICKED_FOLDERS_FOR_TEST.with(|q| {
            let mut q = q.borrow_mut();
            (!q.is_empty()).then(|| q.remove(0))
        });
        if next.is_some() {
            return next;
        }
    }
    rfd::FileDialog::new()
        .set_title("Choose a folder")
        .pick_folder()
}

/// The CSV the Data Sets page's Import asks for.
fn pick_csv() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(path) = PICKED_CSV_FOR_TEST.with(|p| p.borrow_mut().take()) {
        return Some(path);
    }
    rfd::FileDialog::new()
        .add_filter("CSV", &["csv", "CSV", "txt"])
        .set_title("Import data sets")
        .pick_file()
}

/// Read a file of at most `max` bytes, the size checked before a byte is
/// read (this runs on the interaction thread).
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if len > max {
        return Err(format!(
            "{} is {len} bytes; at most {max} are read",
            path.display()
        ));
    }
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether the dialog host opens one of this module's dialogs for `action`.
pub(crate) fn opens_dialog(action: &MenuAction) -> bool {
    matches!(
        action,
        MenuAction::AutomateBatch
            | MenuAction::ConvertFormats
            | MenuAction::ExportColorLookup
            | MenuAction::ExportPdf
            | MenuAction::DefineVariables
            | MenuAction::DataSets
            | MenuAction::VectorizeBitmap
            | MenuAction::FileInfo
    )
}

/// The dialog `action` opens over `editor`, or `None` when there is nothing
/// for it to work on (the bridge's arm then says why).
pub(crate) fn dialog_for(action: &MenuAction, editor: &Editor) -> Option<FileExtrasDialog> {
    use ui::dialogs::{BatchDialog, BatchMode};
    Some(match action {
        MenuAction::AutomateBatch => FileExtrasDialog::Batch(Box::new(BatchDialog::new(
            BatchMode::Batch,
            editor.actions().iter().map(|a| a.name.clone()).collect(),
        ))),
        MenuAction::ConvertFormats => FileExtrasDialog::Batch(Box::new(BatchDialog::new(
            BatchMode::ConvertFormats,
            Vec::new(),
        ))),
        MenuAction::ExportColorLookup => {
            let open = editor.active()?;
            FileExtrasDialog::ExportLut(Box::new(ui::dialogs::ExportLutDialog::new(
                file_stem(open),
                visible_adjustments(&open.document).len(),
            )))
        }
        MenuAction::ExportPdf => {
            let open = editor.active()?;
            FileExtrasDialog::ExportPdf(Box::new(ui::dialogs::ExportPdfDialog::new(
                open.document.width(),
                open.document.height(),
            )))
        }
        MenuAction::DefineVariables => FileExtrasDialog::Variables(Box::new(
            crate::variables::dialog(editor, ui::dialogs::VariablesPage::Define)?,
        )),
        MenuAction::DataSets => FileExtrasDialog::Variables(Box::new(crate::variables::dialog(
            editor,
            ui::dialogs::VariablesPage::DataSets,
        )?)),
        MenuAction::VectorizeBitmap => {
            crate::vectorize::source_layer(editor).ok()?;
            FileExtrasDialog::Vectorize(Box::default())
        }
        MenuAction::FileInfo => {
            let open = editor.active()?;
            FileExtrasDialog::FileInfo(Box::new(ui::dialogs::FileInfoDialog::new(
                file_info_of(open),
                facts(open),
            )))
        }
        _ => return None,
    })
}

/// Draw the open dialog for one frame. Answers whether it closed; a
/// confirmation is parked and its menu pick pushed onto `out.menu`.
pub(crate) fn drive(
    dialog: &mut FileExtrasDialog,
    ctx: &egui::Context,
    out: &mut ChromeOutput,
) -> bool {
    fn settle<T>(
        outcome: DialogOutcome<T>,
        out: &mut ChromeOutput,
        land: impl FnOnce(T) -> (Parked, MenuAction),
    ) -> bool {
        match outcome {
            DialogOutcome::Open => false,
            DialogOutcome::Cancelled => true,
            DialogOutcome::Confirmed(value) => {
                let (parked, action) = land(value);
                park(parked);
                out.menu.push(action);
                true
            }
        }
    }
    match dialog {
        FileExtrasDialog::Batch(d) => {
            let outcome = d.show(ctx);
            if let Some(field) = d.take_folder_request() {
                if let Some(path) = pick_folder() {
                    d.set_folder(field, path);
                }
            }
            settle(outcome, out, |spec| {
                let action = match spec.mode {
                    ui::dialogs::BatchMode::Batch => MenuAction::AutomateBatch,
                    ui::dialogs::BatchMode::ConvertFormats => MenuAction::ConvertFormats,
                };
                (Parked::Batch(spec), action)
            })
        }
        FileExtrasDialog::Variables(d) => {
            let outcome = d.show(ctx);
            if d.take_csv_request() {
                if let Some(path) = pick_csv() {
                    match read_bounded(&path, MAX_CSV_BYTES)
                        .and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
                    {
                        Ok(text) => {
                            let _ = d.load_csv(&text);
                        }
                        Err(e) => d.set_csv_error(e),
                    }
                }
            }
            let page = d.page();
            settle(outcome, out, |spec| {
                let action = match page {
                    ui::dialogs::VariablesPage::Define => MenuAction::DefineVariables,
                    ui::dialogs::VariablesPage::DataSets => MenuAction::DataSets,
                };
                (Parked::Variables(spec), action)
            })
        }
        FileExtrasDialog::ExportLut(d) => settle(d.show(ctx), out, |spec| {
            (Parked::Lut(spec), MenuAction::ExportColorLookup)
        }),
        FileExtrasDialog::ExportPdf(d) => settle(d.show(ctx), out, |spec| {
            (Parked::Pdf(spec), MenuAction::ExportPdf)
        }),
        FileExtrasDialog::Vectorize(d) => settle(d.show(ctx), out, |spec| {
            (Parked::Vectorize(spec), MenuAction::VectorizeBitmap)
        }),
        FileExtrasDialog::FileInfo(d) => settle(d.show(ctx), out, |fields| {
            (Parked::FileInfo(fields), MenuAction::FileInfo)
        }),
    }
}

/// Whether [`perform`] answers `action` (File Info is answered by
/// [`take_confirmed_file_info`] inside the bridge's own File Info arm).
pub(crate) fn performs(action: MenuAction) -> bool {
    opens_dialog(&action) && action != MenuAction::FileInfo
}

/// Perform a W10-E menu pick with whatever its dialog confirmed.
pub(crate) fn perform(action: MenuAction, editor: &mut Editor) -> Result<String, String> {
    match action {
        MenuAction::AutomateBatch | MenuAction::ConvertFormats => {
            let mode = if action == MenuAction::AutomateBatch {
                ui::dialogs::BatchMode::Batch
            } else {
                ui::dialogs::BatchMode::ConvertFormats
            };
            match take_parked(|p| matches!(p, Parked::Batch(s) if s.mode == mode)) {
                Some(Parked::Batch(spec)) => crate::automate::start(editor, spec),
                _ => Err(format!(
                    "{}: choose the source and destination folders in its dialog",
                    action.label().trim_end_matches('…')
                )),
            }
        }
        MenuAction::DefineVariables | MenuAction::DataSets => {
            match take_parked(|p| matches!(p, Parked::Variables(_))) {
                Some(Parked::Variables(spec)) => crate::variables::perform(editor, spec),
                _ => Err("Variables: define them in Image > Variables > Define...".to_string()),
            }
        }
        MenuAction::ExportColorLookup => {
            let spec = match take_parked(|p| matches!(p, Parked::Lut(_))) {
                Some(Parked::Lut(spec)) => spec,
                _ => ui::dialogs::ExportLutSpec {
                    grid: ui::dialogs::LutGrid::Medium,
                    title: editor.active().map(file_stem).unwrap_or_default(),
                },
            };
            export_color_lookup(editor, &spec)
        }
        MenuAction::ExportPdf => {
            let spec = match take_parked(|p| matches!(p, Parked::Pdf(_))) {
                Some(Parked::Pdf(spec)) => spec,
                _ => ui::dialogs::PdfExportSpec::default(),
            };
            export_pdf(editor, &spec)
        }
        MenuAction::VectorizeBitmap => {
            let spec = match take_parked(|p| matches!(p, Parked::Vectorize(_))) {
                Some(Parked::Vectorize(spec)) => spec,
                _ => ui::dialogs::VectorizeSpec::default(),
            };
            crate::vectorize::vectorize(editor, &spec)
        }
        other => Err(format!("{} is not a W10-E action", other.label())),
    }
}

/// Whether `action` is one of this module's picks whose perform, with no
/// dialog confirmation parked, refuses or writes files rather than editing
/// the document — the digest gate holds those to "loud", not to a changed
/// document.
#[cfg(test)]
pub(crate) fn is_loud_without_a_dialog(action: MenuAction) -> bool {
    matches!(
        action,
        MenuAction::AutomateBatch
            | MenuAction::ConvertFormats
            | MenuAction::ExportColorLookup
            | MenuAction::ExportPdf
            | MenuAction::DefineVariables
            | MenuAction::DataSets
    )
}

// ---------------------------------------------------------------------------
// File Info
// ---------------------------------------------------------------------------

/// The File Info fields a File Info confirmation parked, if one did.
pub(crate) fn take_confirmed_file_info() -> Option<XmpFields> {
    match take_parked(|p| matches!(p, Parked::FileInfo(_))) {
        Some(Parked::FileInfo(fields)) => Some(fields),
        _ => None,
    }
}

/// Keep `fields` as the active document's File Info.
pub(crate) fn set_file_info(editor: &mut Editor, fields: XmpFields) -> Result<String, String> {
    let open = editor.active().ok_or("No document is open")?;
    let id = open.id();
    let title = fields.title.clone();
    FILE_INFO.with(|store| store.borrow_mut().insert(id, fields));
    editor.set_status(if title.is_empty() {
        "File Info updated: Export As writes it into PNG, JPEG and TIFF".to_string()
    } else {
        format!("File Info updated ({title}): Export As writes it into PNG, JPEG and TIFF")
    });
    Ok("File Info".to_string())
}

/// The File Info fields of `open`: what File Info confirmed, else the XMP
/// the source file carried, else blank.
pub fn file_info_of(open: &OpenDocument) -> XmpFields {
    if let Some(fields) = FILE_INFO.with(|store| store.borrow().get(&open.id()).cloned()) {
        return fields;
    }
    source_bytes(open)
        .and_then(|bytes| raster::metadata::read_xmp(&bytes))
        .map(|packet| XmpFields::from_packet(&packet))
        .unwrap_or_default()
}

/// The source file's bytes, when the document came from one small enough to
/// read back.
fn source_bytes(open: &OpenDocument) -> Option<Vec<u8>> {
    read_bounded(open.source_path()?, MAX_SOURCE_METADATA_BYTES).ok()
}

/// What Export As embeds for `open`: File Info's packet (when any field is
/// filled) and the source file's EXIF block (when it had one).
pub fn export_metadata(open: &OpenDocument) -> EmbeddedMetadata {
    let exif = source_bytes(open).and_then(|bytes| raster::metadata::read_exif(&bytes));
    EmbeddedMetadata::from_fields(&file_info_of(open), exif)
}

/// The read-only rows File Info lists under its fields.
fn facts(open: &OpenDocument) -> Vec<(String, String)> {
    let origin = open
        .source_path()
        .or(open.project_path())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "Not saved yet".to_string());
    vec![
        ("Name".to_string(), open.title().to_string()),
        (
            "Size".to_string(),
            format!("{} x {} px", open.document.width(), open.document.height()),
        ),
        (
            "Colour space".to_string(),
            open.document.meta.color_space.name().to_string(),
        ),
        ("Source".to_string(), origin),
    ]
}

/// A file-name stem for `open`: its title without an extension.
fn file_stem(open: &OpenDocument) -> String {
    Path::new(open.title())
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| open.title().to_string())
}

// ---------------------------------------------------------------------------
// Export Color Lookup
// ---------------------------------------------------------------------------

/// Every adjustment layer that shows — its own eye and every enclosing
/// group's on — bottom to top, with its opacity (opacity x fill).
pub fn visible_adjustments(
    document: &editor_core::Document,
) -> Vec<(layer_model::AdjustmentKind, f32)> {
    let tree = &document.layers;
    let shows = |id| {
        let mut at = Some(id);
        while let Some(i) = at {
            match tree.get(i) {
                Some(l) if l.visible => at = tree.parent_of(i),
                _ => return false,
            }
        }
        true
    };
    // The depth-first order is top-most first; the stack applies bottom up.
    tree.iter_depth_first()
        .into_iter()
        .rev()
        .filter(|id| shows(*id))
        .filter_map(|id| {
            let layer = tree.get(id)?;
            match &layer.kind {
                layer_model::LayerKind::Adjustment(a) => Some((
                    a.kind.clone(),
                    (layer.opacity * layer.fill_opacity).clamp(0.0, 1.0),
                )),
                _ => None,
            }
        })
        .collect()
}

/// The adjustment stack sampled on an identity lattice of `size` points per
/// edge, red fastest (the `.cube` order): each lattice colour is decoded
/// from `space` to linear light, run through every adjustment bottom to top
/// (`compositor::apply_adjustment`, the function the compositor itself
/// applies; each one mixed with what it received by its opacity, in linear
/// light), re-encoded and clamped to `0..=1`.
///
/// What a LUT cannot hold is left out and said here: layer masks, clipping,
/// blend modes other than Normal and spatial adjustments (whose output
/// depends on neighbouring pixels) are not per-colour functions.
pub fn stack_lut(
    stack: &[(layer_model::AdjustmentKind, f32)],
    space: &color::ColorSpace,
    size: usize,
) -> Vec<[f32; 3]> {
    let prepared: Vec<(compositor::PreparedAdjustment, f32)> = stack
        .iter()
        .map(|(kind, opacity)| (compositor::PreparedAdjustment::new(kind), *opacity))
        .collect();
    let step = (size.max(2) - 1) as f32;
    let mut table = Vec::with_capacity(size * size * size);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let enc = [r as f32 / step, g as f32 / step, b as f32 / step];
                let mut lin = color::to_linear(space, enc);
                for (adj, opacity) in &prepared {
                    let out = adj.apply(lin, space);
                    for c in 0..3 {
                        lin[c] += (out[c] - lin[c]) * opacity;
                    }
                }
                let out = color::from_linear(space, lin);
                table.push(out.map(|v| {
                    if v.is_finite() {
                        v.clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
                }));
            }
        }
    }
    table
}

/// An Adobe / Resolve `.cube` file: `TITLE`, `LUT_3D_SIZE`, the unit domain
/// and one `r g b` line per entry, red fastest.
pub fn cube_text(title: &str, size: usize, table: &[[f32; 3]]) -> String {
    let mut out = String::with_capacity(table.len() * 28 + 128);
    let title: String = title
        .chars()
        .filter(|c| *c != '"' && !c.is_control())
        .collect();
    out.push_str(&format!("TITLE \"{title}\"\n"));
    out.push_str("# Written by Raster Studio: File > Export > Color Lookup Tables\n");
    out.push_str(&format!("LUT_3D_SIZE {size}\n"));
    out.push_str("DOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0\n");
    for [r, g, b] in table {
        out.push_str(&format!("{r:.6} {g:.6} {b:.6}\n"));
    }
    out
}

/// File ▸ Export ▸ Color Lookup Tables…: ask for a folder and write
/// `<title>.cube` into it.
fn export_color_lookup(
    editor: &mut Editor,
    spec: &ui::dialogs::ExportLutSpec,
) -> Result<String, String> {
    let open = editor.active().ok_or("No document is open")?;
    let stack = visible_adjustments(&open.document);
    let space = open.document.meta.color_space.clone();
    let Some(dir) = editor.pick_export_folder() else {
        return Err("Export Color Lookup: no destination chosen".to_string());
    };
    let size = spec.grid.points();
    let table = stack_lut(&stack, &space, size);
    let text = cube_text(&spec.title, size, &table);
    let path = dir.join(format!("{}.cube", raster::sanitize_file_stem(&spec.title)));
    crate::doc::write_atomically(&path, text.as_bytes())
        .map_err(|e| format!("Export Color Lookup: {e}"))?;
    let message = format!(
        "Exported a {size}-point Color Lookup of {} adjustment layer(s) to {}",
        stack.len(),
        path.display()
    );
    editor.set_status(message.clone());
    Ok(message)
}

// ---------------------------------------------------------------------------
// Export PDF
// ---------------------------------------------------------------------------

/// File ▸ Export ▸ PDF…: ask for a folder and write `<document>.pdf` — the
/// flattened composite on the chosen page, File Info as the PDF's `/Info`.
fn export_pdf(editor: &mut Editor, spec: &ui::dialogs::PdfExportSpec) -> Result<String, String> {
    if editor.active().is_none() {
        return Err("No document is open".to_string());
    }
    let Some(dir) = editor.pick_export_folder() else {
        return Err("Export PDF: no destination chosen".to_string());
    };
    let open = editor.active_mut().ok_or("No document is open")?;
    let path = write_pdf(open, spec, &dir)?;
    let message = format!("Exported PDF to {}", path.display());
    editor.set_status(message.clone());
    Ok(message)
}

/// Write `open` as `<stem>.pdf` in `dir` on `spec`'s page.
pub fn write_pdf(
    open: &mut OpenDocument,
    spec: &ui::dialogs::PdfExportSpec,
    dir: &Path,
) -> Result<PathBuf, String> {
    let (w, h) = (open.document.width(), open.document.height());
    let rgba = open
        .composite(open.canvas_rect())
        .map_err(|e| format!("Export PDF: {e}"))?;
    let fields = file_info_of(open);
    let info = raster::pdf::PdfInfo {
        title: fields.title.clone(),
        author: fields.author.clone(),
        subject: fields.description.clone(),
        keywords: fields.keyword_line(),
    };
    let bytes = raster::pdf::encode_pdf_on_page(w, h, &rgba, spec.page_for(w, h), Some(&info));
    let path = dir.join(format!(
        "{}.pdf",
        raster::sanitize_file_stem(&file_stem(open))
    ));
    crate::doc::write_atomically(&path, &bytes).map_err(|e| format!("Export PDF: {e}"))?;
    Ok(path)
}
