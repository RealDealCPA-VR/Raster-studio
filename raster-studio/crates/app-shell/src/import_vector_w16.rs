//! W16-I: File > Open of an SVG, an EPS or a one-page PDF / PDF-compatible
//! AI opens its **layers**, as Photopea does: groups as groups, vector
//! shapes as shape layers, text as text layers and images as raster layers.
//!
//! A child module of `editor_open_pages` (declared there with `#[path]`);
//! [`Editor::open_w13d_document`] asks [`Editor::open_vector_document`]
//! first, so File > Open (the picker), a drop, File > Open Recent and the
//! command line all reach it through [`Editor::open_resource_file`]. File >
//! Revert rebuilds the layers through [`Editor::open_pages_document`], which
//! asks [`Editor::open_vector_layered`].
//!
//! The readers are in the `raster` crate: `svg_import::layers` (SVG, and the
//! display lists below), `formats::postscript::layers` (the EPS
//! interpreter's display list) and `formats::pdf::layers` (a page's paths
//! and text). They produce the same format-neutral tree the Sketch / XD /
//! Figma reader does, and [`document_from_design_on`] maps it with the same
//! shape and text mapping, on the canvas the file names (the SVG's size, the
//! EPS bounding box, the PDF page).
//!
//! # Flattening, reported
//!
//! An element that cannot be a live layer opens as a raster layer and the
//! "<format> import report" says how many and why. When the layers cannot
//! be read at all (a PDF page with an image, a clip, a shading; an EPS the
//! interpreter cannot draw; a PDF of two or more pages, which the PDF
//! import dialog opens as page artboards), the file opens as one picture,
//! as before, and the status line says why its layers were not read.

use std::io::Read;
use std::path::Path;

use raster::codec::formats::{pdf, postscript};
use raster::codec::svg_import::{self, layers::VectorLayers};
use raster::{ImportFormat, ImportLimits};

use super::super::super::{Action, ActionError, DocumentId, Editor, Effect, OpenDocument};
use super::super::import_design::{document_from_design_on, report, DesignImport};
use super::{read_limited, w13d_format};
use crate::import::{DecodedImage, PsdImport, PsdNotes};

/// Which of this route's formats `path` holds: an SVG by content (or a
/// gzip-compressed one named `.svg` / `.svgz`), an EPS or a PDF / AI as the
/// W13-D sniff says. `None` for anything else.
pub fn vector_format(path: &Path) -> Option<ImportFormat> {
    let mut head = Vec::with_capacity(4096);
    std::fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_end(&mut head)
        .ok()?;
    if svg_import::looks_like_svg(&head) {
        return Some(ImportFormat::Svg);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if head.starts_with(&[0x1f, 0x8b]) && matches!(ext.as_deref(), Some("svg" | "svgz")) {
        return Some(ImportFormat::Svg);
    }
    w13d_format(path).filter(|f| matches!(f, ImportFormat::Eps | ImportFormat::Pdf))
}

/// `bytes` (a file of `format`) read as layers, with the notes the reader
/// adds to the import report.
pub fn read_vector(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<VectorLayers, String> {
    match format {
        ImportFormat::Svg => {
            svg_import::layers::read_layers(bytes, limits, ImportFormat::Svg, "the drawing")
                .map_err(|e| e.to_string())
        }
        ImportFormat::Eps => {
            let (mut layers, note) =
                postscript::layers(bytes, limits).map_err(|e| e.to_string())?;
            // The interpreter's own note, when it says more than that it
            // drew the artwork.
            if note.contains(';') {
                layers.design.notes.push(note);
            }
            Ok(layers)
        }
        ImportFormat::Pdf => {
            let pages = pdf::page_count(bytes).map_err(|e| e.to_string())?;
            if pages != 1 {
                return Err(format!(
                    "it has {pages} pages; a file of more than one page opens its pages as artboards"
                ));
            }
            pdf::layers::page_layers(bytes, 0, limits).map_err(|e| e.to_string())
        }
        other => Err(format!("{} does not open as vector layers", other.name())),
    }
}

/// `layers` as a document titled `title`, on the canvas the file names.
pub fn document_from_vector(
    layers: &VectorLayers,
    title: &str,
    history_depth: usize,
) -> Result<DesignImport, String> {
    document_from_design_on(
        &layers.design,
        Some((layers.width, layers.height)),
        title,
        history_depth,
    )
}

/// What [`Editor::open_vector_document`] did.
pub(crate) enum VectorOpen {
    /// Not one of this route's files.
    NotOurs,
    /// Opened (as layers, or as a picture after its layers failed), or the
    /// open failed.
    Done(Result<Effect, ActionError>),
    /// An EPS / PDF whose layers could not be read, and why: the W13-D
    /// route opens it as one picture and says so.
    NoLayers(String),
}

fn layered(
    format: ImportFormat,
    path: &Path,
    history_depth: usize,
) -> Result<DesignImport, String> {
    let limits = ImportLimits::default();
    let bytes = read_limited(path, limits)?;
    let layers = read_vector(format, &bytes, limits)?;
    document_from_vector(&layers, &DecodedImage::title_for(path), history_depth)
}

fn as_open_document(id: DocumentId, path: &Path, import: DesignImport) -> OpenDocument {
    OpenDocument::open_psd_import(
        id,
        path,
        PsdImport {
            imported: import.imported,
            notes: PsdNotes::default(),
            merged_preview: None,
        },
    )
}

impl Editor {
    /// W16-I: File > Revert of an SVG, EPS or one-page PDF rebuilds its
    /// layers; `None` for any other file, or one whose layers cannot be
    /// read (it reverts to a picture, as it opened).
    pub(crate) fn open_vector_layered(
        id: DocumentId,
        path: &Path,
        history_depth: usize,
    ) -> Option<OpenDocument> {
        let format = vector_format(path)?;
        layered(format, path, history_depth)
            .ok()
            .map(|import| as_open_document(id, path, import))
    }

    /// W16-I: open `path` as layers when it is an SVG, EPS or one-page PDF
    /// (see the module docs).
    pub(crate) fn open_vector_document(&mut self, path: &Path) -> VectorOpen {
        let Some(format) = vector_format(path) else {
            return VectorOpen::NotOurs;
        };
        let depth = self.prefs.history_depth;
        let why = match layered(format, path, depth) {
            Ok(import) => {
                let id = self.mint_id();
                let summary = import.summary.clone();
                let notes = import.notes.clone();
                self.install_opened(as_open_document(id, path, import), path);
                let mut status = format!("Opened {}: {summary}", path.display());
                if let Some(text) = report(format, &notes, path) {
                    status.push_str(&format!(
                        " ({} not mapped exactly; see the import report)",
                        if notes.len() == 1 {
                            "1 thing".to_string()
                        } else {
                            format!("{} things", notes.len())
                        }
                    ));
                    self.dialogs
                        .report_notice(&format!("{} import report", format.name()), &text);
                }
                self.status = Some(status);
                self.touch();
                return VectorOpen::Done(Ok(Effect::DocumentSet));
            }
            Err(why) => why,
        };
        if format != ImportFormat::Svg {
            return VectorOpen::NoLayers(why);
        }
        // An SVG whose layers cannot be read opens as one picture, as
        // before, and says why.
        let failed =
            |e: String| ActionError::failed(Action::Open, format!("{}: {e}", path.display()));
        let limits = ImportLimits::default();
        let opened = read_limited(path, limits).and_then(|bytes| {
            let surface = svg_import::rasterize(&bytes, limits).map_err(|e| e.to_string())?;
            let image = DecodedImage {
                width: surface.width,
                height: surface.height,
                color_space: surface.color_space,
                icc_profile: surface.icc_profile,
                rgba8: surface.pixels.into_rgba8(),
            };
            let id = self.mint_id();
            OpenDocument::open_image_decoded(id, path, image, depth).map_err(|e| e.to_string())
        });
        VectorOpen::Done(match opened {
            Ok(doc) => {
                self.install_opened(doc, path);
                self.status = Some(format!(
                    "Opened {}: the drawing as one picture; its layers could not be read ({why})",
                    path.display()
                ));
                self.touch();
                Ok(Effect::DocumentSet)
            }
            Err(e) => Err(failed(format!(
                "its layers could not be read ({why}), and {e}"
            ))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use crate::shell::Shell;
    use layer_model::LayerKind;

    fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(dialogs),
        )
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// Every layer of the active document as `name:kind`, depth first.
    fn layer_kinds(editor: &Editor) -> Vec<String> {
        let doc = editor.active().expect("a document opened");
        doc.document
            .layers
            .iter_depth_first()
            .into_iter()
            .map(|id| {
                let layer = doc.document.layers.get(id).unwrap();
                let kind = match &layer.kind {
                    LayerKind::Shape(_) => "shape",
                    LayerKind::Text(_) => "text",
                    LayerKind::Group(_) => "group",
                    LayerKind::Raster(_) => "raster",
                    _ => "other",
                };
                format!("{}:{kind}", layer.name)
            })
            .collect()
    }

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80">
  <rect id="box" x="10" y="10" width="30" height="20" fill="#ff0000"/>
  <circle id="dot" cx="80" cy="30" r="15" fill="#00ff00"/>
  <text id="label" x="10" y="70" font-family="Arial" font-size="14" fill="#000000">Hello</text>
  <g id="pair"><rect x="60" y="60" width="5" height="5"/><rect x="70" y="60" width="5" height="5"/></g>
</svg>"##;

    #[test]
    fn file_open_of_an_svg_opens_its_shapes_text_and_group_as_layers() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "four.svg", SVG.as_bytes());
        // File > Open, through the picker.
        let mut editor = editor(dir.path(), ScriptedDialogs::new().opening(&path));
        editor.dispatch(Action::Open).unwrap();
        editor.poll_imports();
        assert_eq!(editor.documents().len(), 1);
        let doc = editor.active().unwrap();
        assert_eq!(
            (doc.document.width(), doc.document.height()),
            (120, 80),
            "the SVG's own size"
        );
        let mut kinds = layer_kinds(&editor);
        kinds.sort();
        assert_eq!(
            kinds,
            vec![
                "Shape:shape",
                "Shape:shape",
                "box:shape",
                "dot:shape",
                "label:text",
                "pair:group"
            ]
        );
        let status = editor.status().unwrap_or_default().to_string();
        assert!(status.contains("its layers"), "{status}");
    }

    #[test]
    fn file_open_of_an_eps_with_two_fills_opens_two_shape_layers() {
        let eps = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 50\n%%EndComments\n1 0 0 setrgbcolor 10 10 30 20 rectfill\n0 0 1 setrgbcolor newpath 60 10 moveto 90 10 lineto 75 40 lineto closepath fill\n%%EOF\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "two.eps", eps.as_bytes());
        // A drop onto the window.
        let mut shell = Shell::new(editor(dir.path(), ScriptedDialogs::new()), Vec::new());
        shell.on_dropped_files(&[path]);
        let editor = shell.editor();
        assert_eq!(layer_kinds(editor), vec!["Shape:shape", "Shape:shape"]);
        let doc = editor.active().unwrap();
        assert_eq!((doc.document.width(), doc.document.height()), (100, 50));
    }

    /// A one-page PDF whose resources name Helvetica as `/F1`.
    fn one_page_pdf(w: u32, h: u32, content: &str) -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
            format!("<< /Length {} >>
stream
{content}
endstream", content.len() + 1),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut pdf = b"%PDF-1.4
"
        .to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(
                format!(
                    "{} 0 obj
{body}
endobj
",
                    i + 1
                )
                .as_bytes(),
            );
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref
0 {}
0000000000 65535 f 
",
                objects.len() + 1
            )
            .as_bytes(),
        );
        for o in offsets {
            pdf.extend_from_slice(
                format!(
                    "{o:010} 00000 n 
"
                )
                .as_bytes(),
            );
        }
        pdf.extend_from_slice(
            format!(
                "trailer
<< /Size {} /Root 1 0 R >>
startxref
{xref}
%%EOF
",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn file_open_of_a_one_page_pdf_opens_its_paths_and_text_as_layers() {
        let pdf = one_page_pdf(
            100,
            80,
            "1 0 0 rg 10 10 30 20 re f
BT /F1 12 Tf 20 60 Td (Hi) Tj ET",
        );
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "page.ai", &pdf);
        let mut editor = editor(dir.path(), ScriptedDialogs::new().opening(&path));
        editor.dispatch(Action::Open).unwrap();
        editor.poll_imports();
        assert_eq!(layer_kinds(&editor), vec!["Hi:text", "Shape:shape"]);
        // File > Revert reads the layers back.
        let reverted = editor.revert_active();
        assert!(reverted.is_ok(), "{reverted:?}");
        assert_eq!(layer_kinds(&editor), vec!["Hi:text", "Shape:shape"]);
    }

    #[test]
    fn a_pdf_page_with_an_image_opens_as_a_picture_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "image.ai",
            &one_page_pdf(40, 30, "q 40 0 0 30 0 0 cm /Im1 Do Q 1 0 0 rg 0 0 5 5 re f"),
        );
        let mut editor = editor(dir.path(), ScriptedDialogs::new().opening(&path));
        editor.dispatch(Action::Open).unwrap();
        editor.poll_imports();
        assert_eq!(layer_kinds(&editor).len(), 1, "one picture");
        let status = editor.status().unwrap_or_default().to_string();
        assert!(
            status.contains("its layers could not be read") && status.contains("XObject"),
            "{status}"
        );
    }
}
