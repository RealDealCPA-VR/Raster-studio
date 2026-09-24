//! File ▸ Export ▸ Slices…: one file per Slice-tool region.
//!
//! The Slice tool publishes its regions when the gesture is committed (Enter);
//! [`crate::tool_input`] hands them to [`remember_committed`], which keeps them
//! per document in the editor's [`SliceStore`] — the latest committed set
//! replaces the previous one, as a new slice set does in Photopea. File ▸
//! Export ▸ Slices… ([`export_slices`]) then asks for a folder once and writes
//! every region of the active document's set, cut from the full composite, as
//! `<document>_01.<ext>`, `<document>_02.<ext>`, … in slice order.
//!
//! The format and its settings (quality, scale, resampling, depth) are the
//! first row of the last Export As job the shell handed to the writer (the
//! folder picker answered) —
//! [`ui::dialogs::export_as::last_confirmed_entry`] — and a plain PNG at 100%
//! before any Export As has been written. Each file goes through the same
//! colour-managed exporter as Export As ([`raster::export::export_batch_to_dir`]),
//! so a slice is byte-for-byte what exporting that region alone would write.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use raster::PixelRect;

use tools::slice_select::{default_slice_name, SliceOptions};
use ui::dialogs::slice_options::{slice_file_name, slice_file_stem};

use crate::doc::{DocumentId, OpenDocument};
use crate::editor::Editor;

/// The committed slice sets, per open document.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SliceStore {
    by_document: HashMap<DocumentId, Vec<PixelRect>>,
    /// W10-A: each slice's options (name, URL, alt), in the same order as
    /// its rectangle in `by_document`.
    options: HashMap<DocumentId, Vec<SliceOptions>>,
    /// W10-A: the slice the Slice Select tool's last press picked, which is
    /// what Delete removes.
    picked: HashMap<DocumentId, usize>,
}

impl SliceStore {
    /// Replace `document`'s slice set. An empty set forgets it. Each slice is
    /// named by its position ([`default_slice_name`]).
    pub fn remember(&mut self, document: DocumentId, rects: Vec<PixelRect>) {
        let options = (1..=rects.len())
            .map(|n| SliceOptions::named(default_slice_name(n)))
            .collect();
        self.remember_with(document, rects, options);
    }

    /// Replace `document`'s slice set with `rects` and their `options`, in
    /// the same order. The picked slice is forgotten.
    fn remember_with(
        &mut self,
        document: DocumentId,
        rects: Vec<PixelRect>,
        options: Vec<SliceOptions>,
    ) {
        debug_assert_eq!(rects.len(), options.len());
        self.picked.remove(&document);
        if rects.is_empty() {
            self.by_document.remove(&document);
            self.options.remove(&document);
        } else {
            self.by_document.insert(document, rects);
            self.options.insert(document, options);
        }
    }

    /// `document`'s committed slice set, in slice order (empty when none).
    pub fn get(&self, document: DocumentId) -> &[PixelRect] {
        self.by_document
            .get(&document)
            .map_or(&[][..], Vec::as_slice)
    }

    /// W10-A: each slice's options, in slice order.
    pub fn options(&self, document: DocumentId) -> &[SliceOptions] {
        self.options.get(&document).map_or(&[][..], Vec::as_slice)
    }

    /// W10-A: the set as the tools see it: each rectangle with its name.
    pub fn slices(&self, document: DocumentId) -> Vec<tools::Slice> {
        self.get(document)
            .iter()
            .zip(self.options(document))
            .map(|(rect, o)| tools::Slice {
                rect: *rect,
                name: o.name.clone(),
            })
            .collect()
    }

    /// W10-A: the slice Slice Select last picked, while it still exists.
    pub fn picked(&self, document: DocumentId) -> Option<usize> {
        self.picked
            .get(&document)
            .copied()
            .filter(|i| *i < self.get(document).len())
    }

    /// W10-A: record (or clear) the slice Slice Select picked.
    pub fn set_picked(&mut self, document: DocumentId, index: Option<usize>) {
        match index.filter(|i| *i < self.get(document).len()) {
            Some(i) => {
                self.picked.insert(document, i);
            }
            None => {
                self.picked.remove(&document);
            }
        }
    }

    /// W10-A: replace slice `index`'s options on a document whose file stem
    /// is `doc_stem`. The name must be non-empty, and the file it exports as
    /// ([`slice_file_stem`], compared ignoring case) must not be another
    /// slice's: it names the exported file, and it is what an edited set is
    /// matched back by. Two names that the exporter sanitises to one file
    /// (`hero?` and `hero!` are both `hero_`) would overwrite each other.
    pub fn set_options(
        &mut self,
        document: DocumentId,
        doc_stem: &str,
        index: usize,
        options: SliceOptions,
    ) -> Result<(), String> {
        let name = options.name.trim().to_string();
        if name.is_empty() {
            return Err("A slice needs a name".to_string());
        }
        let all = self
            .options
            .get_mut(&document)
            .filter(|all| index < all.len())
            .ok_or_else(|| format!("There is no slice {}", index + 1))?;
        let stem = slice_file_stem(doc_stem, &name, index + 1);
        if let Some((i, o)) = all.iter().enumerate().find(|(i, o)| {
            *i != index && slice_file_stem(doc_stem, &o.name, i + 1).eq_ignore_ascii_case(&stem)
        }) {
            return Err(if o.name == name {
                format!("Another slice is already named {name}")
            } else {
                format!(
                    "{name} would be exported as {stem}, the same file as slice {} ({})",
                    i + 1,
                    o.name
                )
            });
        }
        all[index] = SliceOptions { name, ..options };
        Ok(())
    }

    /// W10-A: take in the set Slice Select just edited. The slices keep
    /// their options, matched by name (the tool carries the stored names
    /// through the edit). The pick survives a move or a resize and is
    /// forgotten when the set shrank.
    pub fn remember_edited(&mut self, document: DocumentId, slices: &[tools::Slice]) {
        let before = self.options(document).to_vec();
        let picked = self.picked(document);
        let same_count = slices.len() == before.len();
        let options = slices
            .iter()
            .enumerate()
            .map(|(i, s)| {
                before
                    .iter()
                    .find(|o| o.name == s.name)
                    .cloned()
                    .unwrap_or_else(|| SliceOptions::named(name_or_default(&s.name, i)))
            })
            .collect();
        self.remember_with(document, slices.iter().map(|s| s.rect).collect(), options);
        if same_count {
            self.set_picked(document, picked);
        }
    }

    /// W10-A: delete slice `index`; the others keep their names and options.
    pub fn delete(&mut self, document: DocumentId, index: usize) -> bool {
        let mut rects = self.get(document).to_vec();
        let mut options = self.options(document).to_vec();
        if index >= rects.len() {
            return false;
        }
        rects.remove(index);
        options.remove(index);
        self.remember_with(document, rects, options);
        true
    }
}

// ---------------------------------------------------------------------------
// W11-I: persistence. The slice set is saved in the `.rstudio` document
// (`editor_core::Document::slices`) and restored from it, as Photopea keeps
// slices in the file.
// ---------------------------------------------------------------------------

impl SliceStore {
    /// W11-I: `document`'s set as the document file records it: each
    /// rectangle with its name, URL and alt text, in slice order.
    pub fn document_slices(&self, document: DocumentId) -> Vec<editor_core::slices::DocumentSlice> {
        self.get(document)
            .iter()
            .zip(self.options(document))
            .map(|(r, o)| editor_core::slices::DocumentSlice {
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
                name: o.name.clone(),
                url: o.url.clone(),
                alt: o.alt.clone(),
            })
            .collect()
    }

    /// W11-I: take in the set a document file carried. Degenerate
    /// rectangles are dropped; a blank name takes the Slice tool's name for
    /// its position.
    pub fn restore(&mut self, document: DocumentId, saved: &[editor_core::slices::DocumentSlice]) {
        let kept: Vec<_> = saved
            .iter()
            .filter(|s| s.width > 0 && s.height > 0)
            .collect();
        let rects = kept
            .iter()
            .map(|s| PixelRect::new(s.x, s.y, s.width, s.height))
            .collect();
        let options = kept
            .iter()
            .enumerate()
            .map(|(i, s)| SliceOptions {
                name: name_or_default(&s.name, i),
                url: s.url.clone(),
                alt: s.alt.clone(),
            })
            .collect();
        self.remember_with(document, rects, options);
    }
}

/// W11-I: write the ACTIVE document's slice set into the document itself,
/// so the next save carries it. Every slice action works on the active
/// document, so only its record can have changed; another open document's
/// record is never rewritten from the store (a document whose saved set the
/// store had not loaded would otherwise lose it). A record that changes
/// marks the document dirty (a new or edited slice set is an unsaved
/// change, as in Photopea).
pub fn persist_slices(editor: &mut Editor) {
    let Some(id) = editor.active().map(OpenDocument::id) else {
        return;
    };
    let record = editor.slices.document_slices(id);
    let changed = editor
        .active()
        .is_some_and(|doc| doc.document.slices != record);
    if changed {
        // W11-E: through history, so every slice action is one undo step and
        // the document is marked dirty the way any edit marks it.
        editor.apply_command(editor_core::Command::SetSlices { slices: record });
    }
}

/// W11-E: after an undo, a redo or a history jump the document's slice set
/// may have moved under the editor's store; make the store show the
/// document's set again (an empty set clears it).
pub fn resync_active_slices(editor: &mut Editor) {
    let Some((id, slices)) = editor.active().map(|d| (d.id(), d.document.slices.clone())) else {
        return;
    };
    if editor.slices.document_slices(id) != slices {
        editor.slices.restore(id, &slices);
    }
}

/// W11-I: load the slices a document was saved with into the slice store,
/// for every open document the store holds no set for yet. Returns how many
/// documents got their slices back.
pub fn restore_saved_slices(editor: &mut Editor) -> usize {
    let mut restored = 0;
    let saved: Vec<_> = editor
        .documents()
        .iter()
        .filter(|d| !d.document.slices.is_empty())
        .map(|d| (d.id(), d.document.slices.clone()))
        .collect();
    for (id, slices) in saved {
        if editor.slices.get(id).is_empty() {
            editor.slices.restore(id, &slices);
            restored += 1;
        }
    }
    restored
}

/// `name`, or the Slice tool's name for slice `index` (0-based) when blank.
fn name_or_default(name: &str, index: usize) -> String {
    if name.trim().is_empty() {
        default_slice_name(index + 1)
    } else {
        name.to_string()
    }
}

/// W10-A: Delete (Edit ▸ Clear's key) while the Slice Select tool holds a
/// picked slice deletes that slice instead of clearing pixels. `None` when
/// the tool is not Slice Select or nothing is picked, so the key clears as
/// usual.
pub fn delete_picked_slice(editor: &mut Editor) -> Option<Result<String, String>> {
    if editor.tool() != tools::ToolId::SliceSelect {
        return None;
    }
    let id = editor.active().map(OpenDocument::id)?;
    let index = editor.slices.picked(id)?;
    let name = editor.slices.options(id).get(index)?.name.clone();
    editor.slices.delete(id, index);
    persist_slices(editor);
    let left = editor.slices.get(id).len();
    Some(Ok(format!("Deleted slice {name}; {left} slice(s) left")))
}

/// W10-A: Slice Options: replace slice `index`'s name, URL and alt text on
/// the active document. The name becomes the exported file's name.
pub fn set_slice_options(
    editor: &mut Editor,
    index: usize,
    options: SliceOptions,
) -> Result<String, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let (id, doc_stem) = (doc.id(), document_stem(doc.title()));
    restore_saved_slices(editor);
    editor.slices.set_options(id, &doc_stem, index, options)?;
    persist_slices(editor);
    let name = editor.slices.options(id)[index].name.clone();
    Ok(format!("Slice {} is now named {name}", index + 1))
}

thread_local! {
    /// W10-A: the Slice Options dialog's confirmed answer, parked by the
    /// dialog host for the `SliceOptions` menu arm (the road every dialog
    /// whose answer is not a `DialogAction` takes).
    static CONFIRMED_SLICE_OPTIONS: std::cell::RefCell<Option<ui::dialogs::SliceOptionsSpec>> =
        const { std::cell::RefCell::new(None) };
}

/// W10-A: park a confirmed Slice Options answer for [`perform_slice_options`].
pub(crate) fn park_slice_options(spec: ui::dialogs::SliceOptionsSpec) {
    CONFIRMED_SLICE_OPTIONS.with(|slot| *slot.borrow_mut() = Some(spec));
}

/// Why Slice Options has nothing to open over.
const NO_PICKED_SLICE: &str =
    "Slice Options needs a slice: pick one with the Slice Select tool (click inside it)";

/// W10-A: File > Export > Slice Options... over the active document's picked
/// slice, holding its stored name, URL and alt text. `None` without a
/// document or a picked slice; the menu arm then says why.
pub fn slice_options_dialog(editor: &Editor) -> Option<ui::dialogs::SliceOptionsDialog> {
    let doc = editor.active()?;
    let (id, doc_stem) = (doc.id(), document_stem(doc.title()));
    let index = editor.slices.picked(id)?;
    let all = editor.slices.options(id);
    let own = all.get(index)?;
    // The files the other slices export as, which this one may not take.
    let others = all
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .map(|(i, o)| slice_file_stem(&doc_stem, &o.name, i + 1))
        .collect();
    Some(ui::dialogs::SliceOptionsDialog::new(
        index,
        own.name.clone(),
        own.url.clone(),
        own.alt.clone(),
        doc_stem,
        others,
    ))
}

/// W10-A: the `SliceOptions` menu arm: apply the parked dialog answer to its
/// slice ([`set_slice_options`]). With nothing parked — the dialog could not
/// open — it says why.
pub fn perform_slice_options(editor: &mut Editor) -> Result<String, String> {
    let Some(spec) = CONFIRMED_SLICE_OPTIONS.with(|slot| slot.borrow_mut().take()) else {
        return Err(match editor.active() {
            None => "No document is open".to_string(),
            Some(_) => NO_PICKED_SLICE.to_string(),
        });
    };
    set_slice_options(
        editor,
        spec.index,
        SliceOptions {
            name: spec.name,
            url: spec.url,
            alt: spec.alt,
        },
    )
}

/// W10-A: the file name slice `number` (1-based) of a document titled `stem`
/// is exported under ([`slice_file_name`]): the Slice tool's own names
/// (`slice_NN`) keep the `<document>_NN` pattern, numbered by the slice's
/// name, so a slice keeps its file name after another is deleted. A name the
/// user gave has the characters a file name cannot hold replaced by `_`, and
/// the exporter then sanitises it ([`slice_file_stem`]: only ASCII letters,
/// digits and `- _ ( )` and space are kept, the rest become `_`, cut at 64).
pub fn export_file_name(stem: &str, name: &str, number: usize) -> String {
    slice_file_name(stem, name, number)
}

/// Keep a slice set the Slice tool just committed on the active document, and
/// return the sentence the status bar shows for it.
pub fn remember_committed(editor: &mut Editor, slices: &[tools::Slice]) -> String {
    let Some(id) = editor.active().map(OpenDocument::id) else {
        return format!("{} slice(s) defined, but no document is open", slices.len());
    };
    // A new set: each slice named as the Slice tool named it (by position
    // when it came without a name), with no URL or alt text yet.
    let options = slices
        .iter()
        .enumerate()
        .map(|(i, s)| SliceOptions::named(name_or_default(&s.name, i)))
        .collect();
    editor
        .slices
        .remember_with(id, slices.iter().map(|s| s.rect).collect(), options);
    persist_slices(editor);
    format!(
        "{} slice(s) defined; File > Export > Slices writes one file each",
        slices.len()
    )
}

/// W11-E: Layer ▸ New Layer Based Slice: a slice over the active layer's
/// ink (its tight bounds, clipped to the canvas), added to the active
/// document's set and named after the layer (numbered when another slice
/// already exports under that name). The new slice is picked and the Slice
/// Select tool raised, so it shows and Slice Options edits it next.
///
/// The new set reaches the document through [`persist_slices`], so it is one
/// history step: Undo removes the slice.
pub fn new_layer_based_slice(editor: &mut Editor) -> Result<String, String> {
    let (id, stem, rect, layer_name) = {
        let doc = editor.active().ok_or("No document is open")?;
        let layer = doc.document.active_layer().ok_or("Select a layer first")?;
        let ink = crate::tool_input::tight_document_bounds(&doc.document, &doc.tiles, layer)
            .filter(|r| r.width > 0 && r.height > 0)
            .ok_or("The layer has no pixels to slice")?;
        let (w, h) = (doc.document.width() as i64, doc.document.height() as i64);
        let x0 = ink.x.max(0);
        let y0 = ink.y.max(0);
        let x1 = (ink.x + ink.width as i64).min(w);
        let y1 = (ink.y + ink.height as i64).min(h);
        if x1 <= x0 || y1 <= y0 {
            return Err("The layer's pixels lie outside the canvas".to_string());
        }
        let name = doc
            .document
            .layers
            .get(layer)
            .map(|l| l.name.clone())
            .unwrap_or_default();
        (
            doc.id(),
            document_stem(doc.title()),
            PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32),
            name,
        )
    };
    restore_saved_slices(editor);
    let mut rects = editor.slices.get(id).to_vec();
    let mut options = editor.slices.options(id).to_vec();
    if rects.contains(&rect) {
        return Err("A slice already covers exactly this layer".to_string());
    }
    let number = rects.len() + 1;
    let base = name_or_default(&layer_name, rects.len());
    // The name exports as a file, so it must not collide with another
    // slice's file (compared as `SliceStore::set_options` compares).
    let taken = |name: &str| {
        let stem_of = slice_file_stem(&stem, name, number);
        options
            .iter()
            .enumerate()
            .any(|(i, o)| slice_file_stem(&stem, &o.name, i + 1).eq_ignore_ascii_case(&stem_of))
    };
    let mut name = base.clone();
    let mut n = 2;
    while taken(&name) {
        name = format!("{base} {n}");
        n += 1;
    }
    rects.push(rect);
    options.push(SliceOptions::named(name.clone()));
    editor.slices.remember_with(id, rects, options);
    persist_slices(editor);
    editor.slices.set_picked(id, Some(number - 1));
    editor.set_tool(tools::ToolId::SliceSelect);
    Ok(format!(
        "Slice {name} covers the layer ({} x {} at {}, {}); {number} slice(s)",
        rect.width, rect.height, rect.x, rect.y
    ))
}

/// W10-A: keep the set the Slice Select tool just edited on the active
/// document (see [`SliceStore::remember_edited`]), and return the status
/// sentence.
pub fn remember_edited(editor: &mut Editor, slices: &[tools::Slice]) -> String {
    let Some(id) = editor.active().map(OpenDocument::id) else {
        return format!("{} slice(s), but no document is open", slices.len());
    };
    restore_saved_slices(editor);
    editor.slices.remember_edited(id, slices);
    persist_slices(editor);
    format!(
        "{} slice(s); File > Export > Slices writes one file each",
        slices.len()
    )
}

/// File ▸ Export ▸ Slices…: ask for a folder and write every committed slice
/// of the active document into it.
pub fn export_slices(editor: &mut Editor) -> Result<String, String> {
    restore_saved_slices(editor);
    let doc = editor.active().ok_or("No document is open")?;
    let rects = editor.slices.get(doc.id()).to_vec();
    let options = editor.slices.options(doc.id()).to_vec();
    let names: Vec<String> = options.iter().map(|o| o.name.clone()).collect();
    if rects.is_empty() {
        return Err(
            "Export Slices: this document has no slices - draw them with the Slice tool \
             and press Enter"
                .to_string(),
        );
    }
    let Some(dir) = editor.pick_export_folder() else {
        return Err("Export Slices: no destination chosen".to_string());
    };
    let entry = ui::dialogs::export_as::last_confirmed_entry();
    let doc = editor.active_mut().ok_or("No document is open")?;
    let written = write_named_slices(doc, &rects, &names, &entry.preset, &dir)?;
    // W10-A: a slice given a URL or alt text in Slice Options is laid out in
    // an HTML page beside the images, as Photoshop's Save for Web writes it.
    let page = if options
        .iter()
        .any(|o| !o.url.is_empty() || !o.alt.is_empty())
    {
        let size = (doc.document.width(), doc.document.height());
        let stem = document_stem(doc.title());
        Some(write_slice_page(
            &dir, &stem, size, &rects, &options, &written,
        )?)
    } else {
        None
    };
    Ok(format!(
        "Exported {} slice(s) as {} to {}{}",
        written.len(),
        entry.preset.format.extension().to_uppercase(),
        dir.display(),
        page.map_or(String::new(), |p| format!(
            ", with the page {}",
            p.file_name().unwrap_or_default().to_string_lossy()
        ))
    ))
}

/// A document title's file stem (`poster.psd` is `poster`).
fn document_stem(title: &str) -> String {
    Path::new(title)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| title.to_string())
}

/// `text` safe inside a double-quoted HTML attribute or as element text.
fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// W10-A: the HTML page File > Export > Slices writes beside the images when
/// a slice has a URL or alt text: `<document>.html`, each slice's image
/// placed where the slice sits on a canvas-sized box, carrying its alt text
/// and, when it has a URL, wrapped in a link to it. `written` are the image
/// files, in slice order. Returns the page's path.
pub fn write_slice_page(
    dir: &Path,
    stem: &str,
    (width, height): (u32, u32),
    rects: &[PixelRect],
    options: &[SliceOptions],
    written: &[PathBuf],
) -> Result<PathBuf, String> {
    let mut body = String::new();
    for (index, (rect, file)) in rects.iter().zip(written).enumerate() {
        let o = options.get(index).cloned().unwrap_or_default();
        let x = rect.x.clamp(0, i64::from(width));
        let y = rect.y.clamp(0, i64::from(height));
        let w = (rect.x + i64::from(rect.width)).clamp(0, i64::from(width)) - x;
        let h = (rect.y + i64::from(rect.height)).clamp(0, i64::from(height)) - y;
        let src = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let img = format!(
            "<img src=\"{}\" alt=\"{}\" width=\"{w}\" height=\"{h}\" \
             style=\"position:absolute;left:{x}px;top:{y}px\">",
            html_escape(&src),
            html_escape(&o.alt),
        );
        if o.url.is_empty() {
            body.push_str(&format!("  {img}\n"));
        } else {
            body.push_str(&format!(
                "  <a href=\"{}\">{img}</a>\n",
                html_escape(&o.url)
            ));
        }
    }
    let page = format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n</head>\n<body>\n\
         <div style=\"position:relative;width:{width}px;height:{height}px\">\n\
         {body}</div>\n</body>\n</html>\n",
        title = html_escape(stem),
    );
    let path = dir.join(format!("{stem}.html"));
    std::fs::write(&path, page)
        .map_err(|e| format!("Export Slices: the page {}: {e}", path.display()))?;
    Ok(path)
}

/// Write `rects` of `doc`'s composite into `dir`, one file each, with
/// `preset`'s format and settings. Returns the paths written, in slice order.
///
/// A region is clipped to the canvas; one that misses the canvas entirely is
/// an error naming it rather than a file with no pixels.
pub fn write_slices(
    doc: &mut OpenDocument,
    rects: &[PixelRect],
    preset: &raster::ExportPreset,
    dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    write_named_slices(doc, rects, &[], preset, dir)
}

/// W10-A: [`write_slices`] with each slice's name ([`SliceOptions::name`])
/// choosing its file name ([`export_file_name`]); a slice `names` does not
/// name is `<document>_NN` by position.
///
/// No slice overwrites another: when a slice's sanitised stem is one an
/// earlier slice of this export already wrote (compared ignoring case, as the
/// disk does) — names the store refuses, but a document renamed or saved
/// under another title after its slices were named can still meet one — it
/// is written as `<stem>_2`, `<stem>_3`, … instead ([`unique_stem`]).
pub fn write_named_slices(
    doc: &mut OpenDocument,
    rects: &[PixelRect],
    names: &[String],
    preset: &raster::ExportPreset,
    dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    let (w, h) = (doc.document.width(), doc.document.height());
    let composite = doc
        .composite(doc.canvas_rect())
        .map_err(|e| e.to_string())?;
    let stem = document_stem(doc.title());
    let space = doc.document.meta.color_space.clone();
    let metadata = raster::export::ExportMetadata {
        icc_profile: None,
        icc_profile_space: None,
    };
    let mut written = Vec::with_capacity(rects.len());
    let mut taken = std::collections::HashSet::new();
    for (index, rect) in rects.iter().enumerate() {
        let number = index + 1;
        let x0 = rect.x.clamp(0, i64::from(w));
        let y0 = rect.y.clamp(0, i64::from(h));
        let x1 = (rect.x + i64::from(rect.width)).clamp(0, i64::from(w));
        let y1 = (rect.y + i64::from(rect.height)).clamp(0, i64::from(h));
        if x1 <= x0 || y1 <= y0 {
            return Err(format!(
                "Export Slices: slice {number} lies outside the canvas"
            ));
        }
        let (cw, ch) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut crop = vec![0u8; cw * ch * 4];
        for row in 0..ch {
            let s = ((y0 as usize + row) * w as usize + x0 as usize) * 4;
            crop[row * cw * 4..(row + 1) * cw * 4].copy_from_slice(&composite[s..s + cw * 4]);
        }
        let image = raster::export::linear_from_rgba8(cw as u32, ch as u32, &crop, &space)
            .map_err(|e| format!("Export Slices: slice {number}: {e}"))?;
        let mut named = preset.clone();
        let wanted = raster::export::sanitize_file_stem(&export_file_name(
            &stem,
            names.get(index).map_or("", String::as_str),
            number,
        ));
        named.name = unique_stem(&wanted, &mut taken);
        let paths = raster::export::export_batch_to_dir(dir, &image, &[named], &metadata)
            .map_err(|e| format!("Export Slices: slice {number}: {e}"))?;
        written.extend(paths);
    }
    Ok(written)
}

/// `stem` (already sanitised), or `stem_2`, `stem_3`, … — the first that no
/// stem in `taken` equals ignoring case — kept within the exporter's
/// 64-character stem so sanitising cannot cut the suffix off again. The
/// chosen stem is added to `taken`.
fn unique_stem(stem: &str, taken: &mut std::collections::HashSet<String>) -> String {
    const MAX: usize = 64;
    let mut candidate = stem.to_string();
    let mut n = 1usize;
    while taken.contains(&candidate.to_ascii_lowercase()) {
        n += 1;
        let suffix = format!("_{n}");
        // The stem is ASCII (sanitised), so byte slicing is on a char edge.
        let keep = stem.len().min(MAX - suffix.len());
        candidate = format!("{}{suffix}", &stem[..keep]);
    }
    taken.insert(candidate.to_ascii_lowercase());
    candidate
}

#[cfg(test)]
#[path = "w11i_slices_tests.rs"]
mod w11i_slices_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    /// A 48x32 document whose left half is red and right half blue, opened in
    /// an editor that answers the folder picker with `out` (when given).
    fn editor_with(dir: &Path, out: Option<&Path>) -> Editor {
        let mut dialogs = ScriptedDialogs::new();
        if let Some(out) = out {
            dialogs = dialogs.exporting_folder(out);
        }
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(dialogs),
        );
        let (w, h) = (48u32, 32u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(if x < w / 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        let path = dir.join("poster.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap(),
        )
        .unwrap();
        ed.open_path(&path).expect("the probe opens");
        ed
    }

    fn slice(x: i64, y: i64, width: u32, height: u32) -> tools::Slice {
        tools::Slice {
            rect: PixelRect::new(x, y, width, height),
            name: String::new(),
        }
    }

    #[test]
    fn with_no_slices_the_export_refuses_before_asking_for_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let mut ed = editor_with(dir.path(), Some(&out));
        let reason = export_slices(&mut ed).unwrap_err();
        assert!(reason.contains("no slices"), "{reason}");
        assert!(!out.exists(), "a folder was written with nothing to export");
    }

    #[test]
    fn a_later_slice_set_replaces_the_earlier_one() {
        let mut store = SliceStore::default();
        let id = DocumentId(7);
        store.remember(id, vec![PixelRect::new(0, 0, 1, 1)]);
        store.remember(
            id,
            vec![PixelRect::new(2, 2, 3, 3), PixelRect::new(0, 0, 1, 1)],
        );
        assert_eq!(store.get(id).len(), 2);
        assert!(store.get(DocumentId(8)).is_empty());
        store.remember(id, Vec::new());
        assert!(store.get(id).is_empty());
    }

    #[test]
    fn a_slice_keeps_its_name_and_options_through_an_edit_and_a_delete() {
        let mut store = SliceStore::default();
        let id = DocumentId(3);
        store.remember(
            id,
            vec![
                PixelRect::new(0, 0, 4, 4),
                PixelRect::new(8, 0, 4, 4),
                PixelRect::new(16, 0, 4, 4),
            ],
        );
        let mut hero = SliceOptions::named("hero");
        hero.url = "https://example.com".into();
        store.set_options(id, "poster", 2, hero.clone()).unwrap();
        assert!(store
            .set_options(id, "poster", 0, SliceOptions::named("hero"))
            .is_err());
        assert!(store
            .set_options(id, "poster", 0, SliceOptions::named("  "))
            .is_err());
        store.set_picked(id, Some(1));
        assert!(store.delete(id, 1));
        assert_eq!(store.picked(id), None, "the deleted slice is still picked");
        let names: Vec<_> = store.options(id).iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["slice_01", "hero"]);
        // An edited set (the hero moved) keeps each slice's options by name.
        let mut edited = store.slices(id);
        edited[1].rect = PixelRect::new(20, 4, 4, 4);
        store.remember_edited(id, &edited);
        assert_eq!(store.options(id)[1], hero);
        assert_eq!(store.get(id)[1], PixelRect::new(20, 4, 4, 4));
        assert_eq!(export_file_name("poster", "slice_03", 1), "poster_03");
        assert_eq!(export_file_name("poster", "", 2), "poster_02");
        assert_eq!(export_file_name("poster", "a/b:c", 2), "a_b_c");
    }

    #[test]
    fn a_slice_outside_the_canvas_is_named_not_written_empty() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let mut ed = editor_with(dir.path(), Some(&out));
        remember_committed(&mut ed, &[slice(100, 100, 8, 8)]);
        let reason = export_slices(&mut ed).unwrap_err();
        assert!(reason.contains("slice 1 lies outside"), "{reason}");
    }

    /// W10-A: the File > Export > Slice Options... row, clicked through the
    /// chrome's own menu route, opens the dialog over the picked slice (it is
    /// not performed without asking); Enter parks the answer and hands the
    /// row to the bridge, which applies it. Without a pick the row says why.
    #[test]
    fn the_slice_options_row_opens_its_dialog_over_the_picked_slice() {
        use ui::menu::MenuAction;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with(dir.path(), None);
        remember_committed(&mut ed, &[slice(0, 0, 8, 8), slice(10, 0, 8, 8)]);
        let id = ed.active().unwrap().id();
        let click = |ed: &mut Editor, chrome: &mut crate::Chrome| {
            let menu = crate::menu_bridge::context(ed, chrome.workspace());
            let intent =
                crate::menu_bridge::resolve_intent(MenuAction::SliceOptions, &menu, ed).unwrap();
            let mut out = crate::ChromeOutput::default();
            chrome.menu_click(intent, ed, &mut out);
            out
        };
        // No pick: no dialog; the row reaches the bridge, which says why.
        let mut chrome = crate::Chrome::new();
        let out = click(&mut ed, &mut chrome);
        assert!(!chrome.dialogs_for_test().is_open());
        assert_eq!(out.menu, vec![MenuAction::SliceOptions]);
        let why = crate::menu_bridge::perform(MenuAction::SliceOptions, &mut ed).unwrap_err();
        assert!(why.contains("Slice Select"), "{why}");
        // Picked: the dialog opens and Enter confirms slice 2's fields.
        ed.slices.set_picked(id, Some(1));
        let out = click(&mut ed, &mut chrome);
        assert!(out.menu.is_empty(), "the row performed without asking");
        let host = chrome.dialogs_for_test();
        assert!(host.is_open(), "Slice Options did not open");
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = crate::ChromeOutput::default();
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        for events in [Vec::new(), vec![enter]] {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| host.ui(ctx, None, &mut out));
        }
        assert!(!host.is_open(), "Enter did not close Slice Options");
        assert_eq!(out.menu, vec![MenuAction::SliceOptions]);
        let said = crate::menu_bridge::perform(MenuAction::SliceOptions, &mut ed).unwrap();
        assert_eq!(said, "Slice 2 is now named slice_02");
    }

    fn named(x: i64, name: &str) -> tools::Slice {
        tools::Slice {
            rect: PixelRect::new(x, 0, 8, 8),
            name: name.into(),
        }
    }

    /// Wave-10 review: Slice Options refuses a name whose exported file is
    /// another slice's, not only an equal name: `hero?` and `hero!` both
    /// export as `hero_`, a name `poster_03` is the Slice tool's `slice_03`
    /// file in `poster`, and `Hero` is `hero` on a case-folding disk.
    #[test]
    fn slice_options_refuses_a_name_that_exports_to_another_slices_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with(dir.path(), None);
        remember_committed(
            &mut ed,
            &[
                named(0, "slice_01"),
                named(10, "slice_02"),
                named(20, "slice_03"),
            ],
        );
        set_slice_options(&mut ed, 0, SliceOptions::named("hero?")).unwrap();
        let why = set_slice_options(&mut ed, 1, SliceOptions::named("hero!")).unwrap_err();
        assert_eq!(
            why,
            "hero! would be exported as hero_, the same file as slice 1 (hero?)"
        );
        let why = set_slice_options(&mut ed, 1, SliceOptions::named("poster_03")).unwrap_err();
        assert!(why.contains("slice 3 (slice_03)"), "{why}");
        set_slice_options(&mut ed, 1, SliceOptions::named("hero")).unwrap();
        let why = set_slice_options(&mut ed, 2, SliceOptions::named("HERO")).unwrap_err();
        assert!(why.contains("slice 2 (hero)"), "{why}");
        // The dialog the shell opens over slice 3 blocks the same names.
        let id = ed.active().unwrap().id();
        ed.slices.set_picked(id, Some(2));
        for (name, blocked) in [("Hero", true), ("HERO_", true), ("poster_01", false)] {
            let mut dialog = slice_options_dialog(&ed).unwrap();
            dialog.set_name(name);
            assert_eq!(dialog.blocked_reason().is_some(), blocked, "{name}");
        }
        // The names the store holds still export to three files.
        let stems: Vec<_> = ed
            .slices
            .options(id)
            .iter()
            .enumerate()
            .map(|(i, o)| slice_file_stem("poster", &o.name, i + 1))
            .collect();
        assert_eq!(stems, ["hero_", "hero", "poster_03"]);
    }

    /// Wave-10 review: however a set came to hold names that sanitise to one
    /// file (here straight from a committed set, which skips Slice Options,
    /// as a document renamed after its slices were named would), Export
    /// Slices writes every slice to its own file, the status counts the
    /// files, and the page points each slice at its own image.
    #[test]
    fn slices_whose_names_sanitise_to_one_file_are_all_written() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let mut ed = editor_with(dir.path(), Some(&out));
        let long = "y".repeat(70);
        remember_committed(
            &mut ed,
            &[
                named(0, "slice_03"),
                named(8, "hero?"),
                named(16, "hero!"),
                named(24, "HERO_"),
                named(32, &long),
                named(40, &format!("{long}z")),
            ],
        );
        let id = ed.active().unwrap().id();
        let mut o = ed.slices.options(id)[0].clone();
        o.url = "https://example.com".into();
        ed.slices.set_options(id, "poster", 0, o).unwrap();
        let said = export_slices(&mut ed).unwrap();
        assert!(said.starts_with("Exported 6 slice(s) as PNG"), "{said}");
        let mut files: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        let y64 = "y".repeat(64);
        let y62 = "y".repeat(62);
        let mut want = vec![
            "HERO__3.png".to_string(),
            "hero_.png".into(),
            "hero__2.png".into(),
            "poster.html".into(),
            "poster_03.png".into(),
            format!("{y64}.png"),
            format!("{y62}_2.png"),
        ];
        want.sort();
        assert_eq!(files, want);
        let html = std::fs::read_to_string(out.join("poster.html")).unwrap();
        for file in &want[..] {
            if file.ends_with(".png") {
                assert_eq!(
                    html.matches(&format!("src=\"{file}\"")).count(),
                    1,
                    "{file} in {html}"
                );
            }
        }
    }

    /// W10-A: the page written beside the images escapes what it quotes.
    #[test]
    fn the_slice_page_links_and_describes_each_slice_escaped() {
        let dir = tempfile::tempdir().unwrap();
        let rects = [PixelRect::new(-4, 2, 10, 10), PixelRect::new(20, 0, 8, 8)];
        let options = [
            SliceOptions {
                name: "a".into(),
                url: "https://x.test/?q=<b>&r='1'".into(),
                alt: "A \"quoted\" slice".into(),
            },
            SliceOptions::named("b"),
        ];
        let written = [dir.path().join("a.png"), dir.path().join("b.png")];
        let page =
            write_slice_page(dir.path(), "doc", (24, 16), &rects, &options, &written).unwrap();
        assert_eq!(page, dir.path().join("doc.html"));
        let html = std::fs::read_to_string(page).unwrap();
        assert!(html.contains("width:24px;height:16px"), "{html}");
        assert!(
            html.contains(
                "<a href=\"https://x.test/?q=&lt;b&gt;&amp;r=&#39;1&#39;\"><img src=\"a.png\" \
                 alt=\"A &quot;quoted&quot; slice\" width=\"6\" height=\"10\" \
                 style=\"position:absolute;left:0px;top:2px\"></a>"
            ),
            "{html}"
        );
        // Clipped to the canvas, and no link without a URL.
        assert!(
            html.contains(
                "  <img src=\"b.png\" alt=\"\" width=\"4\" height=\"8\" \
                 style=\"position:absolute;left:20px;top:0px\">\n"
            ),
            "{html}"
        );
    }
}
