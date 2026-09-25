//! W13-N: the application half of the Styles panel, Select ▸ Magic Cut,
//! Image ▸ Merge Channels, File ▸ Automate ▸ PDF Presentation / Resize
//! Images / Crop and Straighten Photos / Generate Mockups, and Layer ▸ Text
//! ▸ Convert to Point / Paragraph Text.
//!
//! A child of [`crate::menu_bridge`] (declared there with `#[path]`), so it
//! reuses the bridge's own selection, mask and pixel helpers rather than
//! copying them.
//!
//! # Where the windows live
//!
//! Magic Cut and the Resize Images / Generate Mockups folder dialog are
//! hosted here, in a thread-local slot drawn by [`frame`] — which the menu
//! bar's [`super::draw`] calls every frame it draws, with the context, the
//! editor and the menu bar's own click handler. A click on the row lands in
//! [`perform`] with nothing parked and opens the window; the window's OK
//! parks its answer and clicks the same row again through that handler, so
//! the confirmed run travels the road every menu pick travels (the chrome's
//! router, [`super::pick`], [`super::perform`]) and lands as one undo step.
//!
//! These windows are not in the chrome's dialog host, so
//! `Chrome::dialog_open` does not see them. Two things keep them modal all
//! the same. The dialog chrome's scrim takes the pointer. And while one is
//! up, [`frame`] parks egui's keyboard focus on [`keyboard_sink`] whenever
//! no field in the window holds it — so `egui::Context::wants_keyboard_input`
//! is true, and the shell's `route_key` (which asks exactly that, through
//! `Shell::keyboard_owner`) hands no chord to the keymap: Ctrl+Z, a document
//! switch or a tool key cannot act on the document behind the window. Enter
//! and Escape still reach the window, which reads them from egui's input.
//! [`land_cut`] also refuses a cut unless the document and layer it was
//! painted over are still the active ones.
//!
//! All of that holds only where [`frame`] runs, and the chrome runs the menu
//! bar's draw only while the menu bar shows. In Full Screen Mode (no menu
//! bar) the rows are still reachable through Help ▸ Search Commands, so
//! [`perform`] refuses to OPEN any of these windows there, with an error
//! that says to leave Full Screen Mode — rather than open one that nothing
//! draws and nothing holds the keyboard for.
//!
//! The same frame publishes the style presets to the Styles panel
//! ([`ui::panels::styles::StylesView`]).

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use editor_core::{Command, Selection};
use layer_model::{LayerEffects, LayerId};
use ui::dialogs::magic_cut::{MagicCutDialog, MagicCutOutput, MagicCutSpec};
use ui::dialogs::merge_channels::MergeChannelsDialog;
use ui::dialogs::resize_images::{fitted_size, FolderJob, FolderJobDialog, FolderJobSpec};
use ui::dialogs::DialogOutcome;
use ui::menu::MenuAction;
use ui::Intent;

use crate::doc::DocumentId;
use crate::editor::Editor;

/// The window this module is showing, if any.
enum Window {
    MagicCut(Box<MagicCutDialog>),
    FolderJob(Box<FolderJobDialog>),
    MergeChannels(Box<MergeChannelsDialog>),
}

#[derive(Default)]
struct State {
    window: Option<Window>,
    parked_cut: Option<MagicCutSpec>,
    /// The document and layer the open (or parked) Magic Cut was painted
    /// over.
    cut_target: Option<(DocumentId, LayerId)>,
    /// The documents the open Merge Channels window lists, in its order.
    merge_candidates: Vec<DocumentId>,
    /// The confirmed red, green and blue sources.
    parked_merge: Option<[DocumentId; 3]>,
    parked_job: Option<FolderJobSpec>,
    /// The parsed style presets and the fingerprint of the store they were
    /// parsed from.
    styles: Option<(u64, Vec<(String, LayerEffects)>)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    /// Folders the folder dialog's Choose buttons answer, in order, in place
    /// of the platform picker.
    pub(crate) static PICKED_FOLDERS_FOR_TEST: RefCell<Vec<PathBuf>> =
        const { RefCell::new(Vec::new()) };
}

/// The rows this module performs.
pub(crate) fn performs(action: MenuAction) -> bool {
    matches!(
        action,
        MenuAction::ApplyStyleAt(_)
            | MenuAction::MagicCut
            | MenuAction::MergeChannels
            | MenuAction::PdfPresentation
            | MenuAction::ResizeImages
            | MenuAction::CropAndStraightenPhotos
            | MenuAction::GenerateMockups
            | MenuAction::ConvertToPointText
            | MenuAction::ConvertToParagraphText
    )
}

/// The rows whose click, with nothing confirmed, opens a window (or asks a
/// folder) instead of changing the document — the gates that demand a
/// changed digest hold them to a loud answer instead.
#[cfg(test)]
pub(crate) fn is_loud_without_a_dialog(action: MenuAction) -> bool {
    matches!(
        action,
        MenuAction::MagicCut
            | MenuAction::MergeChannels
            | MenuAction::PdfPresentation
            | MenuAction::ResizeImages
            | MenuAction::CropAndStraightenPhotos
            | MenuAction::GenerateMockups
    )
}

/// Which window is open, for a test.
#[cfg(test)]
pub(crate) fn open_window() -> Option<&'static str> {
    STATE.with(|s| match &s.borrow().window {
        Some(Window::MagicCut(_)) => Some("Magic Cut"),
        Some(Window::FolderJob(d)) => Some(match d.spec().job {
            FolderJob::ResizeImages => "Resize Images",
            FolderJob::GenerateMockups => "Generate Mockups",
        }),
        Some(Window::MergeChannels(_)) => Some("Merge Channels"),
        None => None,
    })
}

/// Run `f` on the open Magic Cut window, for a test.
#[cfg(test)]
pub(crate) fn with_magic_cut<R>(f: impl FnOnce(&mut MagicCutDialog) -> R) -> Option<R> {
    STATE.with(|s| match &mut s.borrow_mut().window {
        Some(Window::MagicCut(d)) => Some(f(d)),
        _ => None,
    })
}

/// Run `f` on the open Merge Channels window, for a test.
#[cfg(test)]
pub(crate) fn with_merge_channels<R>(f: impl FnOnce(&mut MergeChannelsDialog) -> R) -> Option<R> {
    STATE.with(|s| match &mut s.borrow_mut().window {
        Some(Window::MergeChannels(d)) => Some(f(d)),
        _ => None,
    })
}

/// Run `f` on the open folder dialog, for a test.
#[cfg(test)]
pub(crate) fn with_folder_job<R>(f: impl FnOnce(&mut FolderJobDialog) -> R) -> Option<R> {
    STATE.with(|s| match &mut s.borrow_mut().window {
        Some(Window::FolderJob(d)) => Some(f(d)),
        _ => None,
    })
}

/// The folder the folder dialog's Choose button asks for.
fn pick_folder() -> Option<PathBuf> {
    #[cfg(test)]
    {
        PICKED_FOLDERS_FOR_TEST.with(|q| {
            let mut q = q.borrow_mut();
            (!q.is_empty()).then(|| q.remove(0))
        })
    }
    #[cfg(not(test))]
    {
        rfd::FileDialog::new()
            .set_title("Choose a folder")
            .pick_folder()
    }
}

/// The egui id that holds the keyboard while one of this module's windows
/// is up (see the module docs).
pub(crate) fn keyboard_sink() -> egui::Id {
    egui::Id::new("raster-w13n-modal-keyboard")
}

/// Hold the keyboard for the open window: register [`keyboard_sink`] this
/// frame (egui drops focus from an id no frame registers) and give it focus
/// unless a field of the window already has it.
fn hold_keyboard(ctx: &egui::Context) {
    let sink = keyboard_sink();
    egui::Area::new(sink.with("area"))
        .fixed_pos(egui::Pos2::ZERO)
        .interactable(false)
        .show(ctx, |ui| {
            let at = egui::Rect::from_min_size(ui.max_rect().min, egui::Vec2::ZERO);
            let _ = ui.interact(at, sink, egui::Sense::hover());
        });
    if ctx.memory(|m| m.focused().is_none()) {
        ctx.memory_mut(|m| m.request_focus(sink));
    }
}

/// One frame: publish the styles to the Styles panel, and draw the open
/// window. A confirmed window parks its answer and clicks its row again
/// through `on_click` — the menu bar's own handler.
pub(crate) fn frame(ctx: &egui::Context, editor: &Editor, on_click: &mut dyn FnMut(Intent)) {
    publish_styles(ctx, editor);
    let Some(mut window) = STATE.with(|s| s.borrow_mut().window.take()) else {
        return;
    };
    let (closed, again) = match &mut window {
        Window::MagicCut(d) => match d.show(ctx) {
            DialogOutcome::Open => (false, None),
            DialogOutcome::Cancelled => (true, None),
            DialogOutcome::Confirmed(spec) => {
                STATE.with(|s| s.borrow_mut().parked_cut = Some(spec));
                (true, Some(MenuAction::MagicCut))
            }
        },
        Window::FolderJob(d) => {
            let outcome = d.show(ctx);
            if let Some(field) = d.take_folder_request() {
                if let Some(path) = pick_folder() {
                    d.set_folder(field, path);
                }
            }
            match outcome {
                DialogOutcome::Open => (false, None),
                DialogOutcome::Cancelled => (true, None),
                DialogOutcome::Confirmed(spec) => {
                    let row = match spec.job {
                        FolderJob::ResizeImages => MenuAction::ResizeImages,
                        FolderJob::GenerateMockups => MenuAction::GenerateMockups,
                    };
                    STATE.with(|s| s.borrow_mut().parked_job = Some(spec));
                    (true, Some(row))
                }
            }
        }
        Window::MergeChannels(d) => match d.show(ctx) {
            DialogOutcome::Open => (false, None),
            DialogOutcome::Cancelled => (true, None),
            DialogOutcome::Confirmed(spec) => {
                let parked = STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    let ids = spec.sources.map(|i| s.merge_candidates.get(i).copied());
                    let ids = ids[0].zip(ids[1]).zip(ids[2]).map(|((r, g), b)| [r, g, b]);
                    s.parked_merge = ids;
                    ids.is_some()
                });
                (true, parked.then_some(MenuAction::MergeChannels))
            }
        },
    };
    if closed {
        // The keyboard goes back to the application with the window.
        ctx.memory_mut(|m| m.surrender_focus(keyboard_sink()));
    } else {
        hold_keyboard(ctx);
        STATE.with(|s| s.borrow_mut().window = Some(window));
    }
    if let Some(row) = again {
        on_click(Intent::Action(row));
    }
}

/// Hand the Styles panel every style preset, parsed once per change.
fn publish_styles(ctx: &egui::Context, editor: &Editor) {
    use std::hash::{Hash, Hasher};
    let stored = editor.presets().styles();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    stored.hash(&mut h);
    let print = h.finish();
    let styles = STATE.with(|s| {
        let mut s = s.borrow_mut();
        match &s.styles {
            Some((p, parsed)) if *p == print => parsed.clone(),
            _ => {
                let parsed = listed_styles(stored);
                s.styles = Some((print, parsed.clone()));
                parsed
            }
        }
    });
    ui::panels::styles::StylesView { styles }.publish(ctx);
}

/// The style presets the Styles panel lists, in its order: every stored
/// preset whose effects parse. A preset library may carry one that does not
/// (it is stored verbatim); it is left out of the panel, and — because the
/// panel's swatch index and [`apply_style_at`] both count through THIS list —
/// every swatch after it still applies its own style.
fn listed_styles(stored: &[(String, String)]) -> Vec<(String, LayerEffects)> {
    stored
        .iter()
        .filter_map(|(name, json)| serde_json::from_str(json).ok().map(|e| (name.clone(), e)))
        .collect()
}

/// The windows of this module are drawn — and hold the keyboard — only from
/// the menu bar's [`super::draw`], which the chrome runs only while the menu
/// bar shows. In Full Screen Mode (no menu bar) the rows are still reachable
/// through Help ▸ Search Commands, so opening a window there would leave it
/// invisible with the keymap live behind it. Refuse, loudly, instead.
fn window_can_show(editor: &Editor, action: MenuAction) -> Result<(), String> {
    if editor.screen_mode().menu_visible() {
        Ok(())
    } else {
        Err(format!(
            "{}: leave Full Screen Mode first (press F); its window needs the menu bar",
            action.label().trim_end_matches('…')
        ))
    }
}

/// Perform one of this module's rows.
pub(crate) fn perform(action: MenuAction, editor: &mut Editor) -> Result<String, String> {
    match action {
        MenuAction::ApplyStyleAt(index) => apply_style_at(editor, index),
        MenuAction::MagicCut => match STATE.with(|s| s.borrow_mut().parked_cut.take()) {
            Some(spec) => land_cut(editor, spec),
            None => open_magic_cut(editor),
        },
        MenuAction::MergeChannels => match STATE.with(|s| s.borrow_mut().parked_merge.take()) {
            Some(sources) => merge_channels(editor, sources),
            None => open_merge_channels(editor),
        },
        MenuAction::PdfPresentation => pdf_presentation(editor),
        MenuAction::ResizeImages | MenuAction::GenerateMockups => {
            let job = if action == MenuAction::ResizeImages {
                FolderJob::ResizeImages
            } else {
                FolderJob::GenerateMockups
            };
            let parked = STATE.with(|s| {
                let mut s = s.borrow_mut();
                match &s.parked_job {
                    Some(spec) if spec.job == job => s.parked_job.take(),
                    _ => None,
                }
            });
            match parked {
                Some(spec) if job == FolderJob::ResizeImages => resize_images(&spec),
                Some(spec) => generate_mockups(editor, &spec),
                None => {
                    window_can_show(editor, action)?;
                    if job == FolderJob::GenerateMockups {
                        active_smart_object(editor)?;
                    }
                    STATE.with(|s| {
                        s.borrow_mut().window =
                            Some(Window::FolderJob(Box::new(FolderJobDialog::new(job))))
                    });
                    Ok(format!(
                        "{}: choose the folders",
                        action.label().trim_end_matches('…')
                    ))
                }
            }
        }
        MenuAction::CropAndStraightenPhotos => crop_and_straighten(editor),
        MenuAction::ConvertToPointText => convert_text(editor, false),
        MenuAction::ConvertToParagraphText => convert_text(editor, true),
        other => Err(format!("{}: not a W13-N row", other.label())),
    }
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

/// The Styles panel's click: style preset `index` onto the active layer, one
/// undo step.
fn apply_style_at(editor: &mut Editor, index: usize) -> Result<String, String> {
    let (name, effects) = listed_styles(editor.presets().styles())
        .into_iter()
        .nth(index)
        .ok_or_else(|| "That style is no longer in the Styles panel".to_string())?;
    let doc = editor.active().ok_or("No document is open")?;
    let layer = doc.document.active_layer().ok_or("Select a layer first")?;
    let current = doc
        .document
        .layers
        .get(layer)
        .ok_or("The active layer is not in the document")?;
    if current.locked.all {
        return Err("The layer is locked".to_string());
    }
    if current.effects == effects {
        return Err(format!("The layer already wears the style \"{name}\""));
    }
    editor.apply_command(Command::SetLayerProperties {
        layer_id: layer,
        patch: editor_core::LayerPatch {
            effects: Some(Box::new(effects)),
            ..Default::default()
        },
    });
    Ok(format!("Applied style \"{name}\""))
}

// ---------------------------------------------------------------------------
// Magic Cut
// ---------------------------------------------------------------------------

fn open_magic_cut(editor: &mut Editor) -> Result<String, String> {
    window_can_show(editor, MenuAction::MagicCut)?;
    let layer = super::pixel_layer(editor)?;
    let (w, h) = super::canvas_of(editor)?;
    let doc = editor.active().ok_or("No document is open")?;
    let rgba = super::pixels::read_layer(doc, layer);
    let target = (doc.id(), layer);
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.window = Some(Window::MagicCut(Box::new(MagicCutDialog::new(w, h, rgba))));
        s.cut_target = Some(target);
    });
    Ok("Magic Cut: paint over what to keep and what to drop".to_string())
}

/// Land a confirmed cut as ONE undo step: the selection, the active layer's
/// mask, or a new layer holding the cut pixels.
fn land_cut(editor: &mut Editor, spec: MagicCutSpec) -> Result<String, String> {
    let target = STATE.with(|s| s.borrow_mut().cut_target.take());
    let doc = editor.active().ok_or("No document is open")?;
    let here = (doc.id(), doc.document.active_layer());
    if target.map(|(d, l)| (d, Some(l))) != Some(here) {
        return Err(
            "Magic Cut: the document or layer the cut was painted over is no longer the active one"
                .to_string(),
        );
    }
    let (w, h) = super::canvas_of(editor)?;
    if (spec.mask.width(), spec.mask.height()) != (w, h) {
        return Err("Magic Cut: the canvas changed size while the window was open".to_string());
    }
    let cut = Selection::Mask(spec.mask);
    match spec.output {
        MagicCutOutput::Selection => {
            super::set_selection(editor, |_, _, _| Ok(cut))?;
            Ok("Magic Cut: selected the cut".to_string())
        }
        MagicCutOutput::LayerMask | MagicCutOutput::NewLayer => {
            // The mask and the layer are made FROM a selection by the
            // bridge's own Layer Mask and Layer via Copy; the cut stands in
            // for the selection while they run (a field write, not a step),
            // and the selection the user had comes back after — so the one
            // step recorded is the mask or the layer.
            let (before, layers) = {
                let doc = editor.active_mut().ok_or("No document is open")?;
                let layers = doc.document.layers.iter_depth_first();
                (std::mem::replace(&mut doc.document.selection, cut), layers)
            };
            let result = if spec.output == MagicCutOutput::LayerMask {
                super::create_mask(editor, ui::menu::MaskOp::RevealSelection)
            } else {
                super::layer_via(editor, false)
            };
            if let Some(doc) = editor.active_mut() {
                doc.document.selection = before;
            }
            // Photopea's new layer is the one the user works on next.
            let made = editor.active().and_then(|doc| {
                doc.document
                    .layers
                    .iter_depth_first()
                    .into_iter()
                    .find(|id| !layers.contains(id))
            });
            if let (Ok(_), Some(id)) = (&result, made) {
                editor.set_active_layer(id);
            }
            result.map(|_| {
                if spec.output == MagicCutOutput::LayerMask {
                    "Magic Cut: masked the layer to the cut".to_string()
                } else {
                    "Magic Cut: copied the cut to a new layer".to_string()
                }
            })
        }
    }
}

// ---------------------------------------------------------------------------
// New documents from pixels
// ---------------------------------------------------------------------------

/// Open `rgba` (`width * height`, straight RGBA8, sRGB) as a new document
/// titled `title`, and make it the active one.
fn open_pixels(
    editor: &mut Editor,
    title: &str,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<(), String> {
    let depth = editor.preferences().history_depth;
    let image = crate::import::DecodedImage {
        width,
        height,
        rgba8: rgba,
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
    };
    let imported =
        crate::import::document_from_image(&image, title, depth).map_err(|e| e.to_string())?;
    editor
        .new_document_with(
            width,
            height,
            title,
            crate::import::BlankBackground::Transparent,
        )
        .map_err(|e| e.to_string())?;
    let id = editor.active().ok_or("No document is open")?.id();
    let slot = editor
        .documents_mut()
        .iter_mut()
        .find(|d| d.id() == id)
        .ok_or("The new document is not open")?;
    *slot = crate::doc::OpenDocument::from_import(id, imported);
    Ok(())
}

/// The flattened composite of the open document at `index`.
fn composite_of(editor: &mut Editor, index: usize) -> Result<(u32, u32, Vec<u8>), String> {
    let open = editor
        .documents_mut()
        .get_mut(index)
        .ok_or("That document is no longer open")?;
    let (w, h) = (open.document.width(), open.document.height());
    let rgba = open
        .composite(open.canvas_rect())
        .map_err(|e| e.to_string())?;
    Ok((w, h, rgba))
}

// ---------------------------------------------------------------------------
// Merge Channels
// ---------------------------------------------------------------------------

/// Image ▸ Merge Channels…: the first three open grayscale documents of the
/// first one's size become the red, green and blue channels of a new RGB
/// document, in tab order.
/// The open documents Merge Channels can take a channel from: the grayscale
/// ones of the first grayscale document's size — at least three of them.
fn merge_candidates(editor: &Editor) -> Result<Vec<usize>, String> {
    let grey: Vec<usize> = editor
        .documents()
        .iter()
        .enumerate()
        .filter(|(_, d)| d.document.meta.color_mode == ui::menu::ColorMode::Grayscale as u8)
        .map(|(i, _)| i)
        .collect();
    let Some(&first) = grey.first() else {
        return Err(
            "Merge Channels needs three open grayscale documents of one size; none is grayscale"
                .to_string(),
        );
    };
    let size = {
        let d = &editor.documents()[first].document;
        (d.width(), d.height())
    };
    let same: Vec<usize> = grey
        .into_iter()
        .filter(|&i| {
            let d = &editor.documents()[i].document;
            (d.width(), d.height()) == size
        })
        .collect();
    if same.len() < 3 {
        return Err(format!(
            "Merge Channels needs three open grayscale documents of one size; {} of {} x {} are open",
            same.len(),
            size.0,
            size.1
        ));
    }
    Ok(same)
}

/// Image ▸ Merge Channels… with nothing confirmed: ask which document each
/// channel comes from.
fn open_merge_channels(editor: &mut Editor) -> Result<String, String> {
    window_can_show(editor, MenuAction::MergeChannels)?;
    let same = merge_candidates(editor)?;
    let ids: Vec<DocumentId> = same.iter().map(|&i| editor.documents()[i].id()).collect();
    let titles: Vec<String> = same
        .iter()
        .map(|&i| editor.documents()[i].title().to_string())
        .collect();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.merge_candidates = ids;
        s.window = Some(Window::MergeChannels(Box::new(MergeChannelsDialog::new(
            titles,
        ))));
    });
    Ok("Merge Channels: choose the document for each channel".to_string())
}

/// Build the RGB document from the confirmed red, green and blue sources.
fn merge_channels(editor: &mut Editor, sources: [DocumentId; 3]) -> Result<String, String> {
    let mut same = Vec::new();
    for id in sources {
        let index = editor
            .documents()
            .iter()
            .position(|d| d.id() == id)
            .ok_or("Merge Channels: a chosen document was closed")?;
        same.push(index);
    }
    let size = {
        let d = &editor.documents()[same[0]].document;
        (d.width(), d.height())
    };
    if same.iter().any(|&i| {
        let d = &editor.documents()[i].document;
        (d.width(), d.height()) != size
    }) {
        return Err("Merge Channels: the chosen documents are no longer one size".to_string());
    }
    let names: Vec<String> = same
        .iter()
        .map(|&i| editor.documents()[i].title().to_string())
        .collect();
    let mut planes = Vec::new();
    for &i in &same {
        planes.push(composite_of(editor, i)?.2);
    }
    let (w, h) = size;
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    for (c, plane) in planes.iter().enumerate() {
        for (px, src) in rgba
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(plane.as_chunks::<4>().0)
        {
            // A grayscale composite stores R = G = B; alpha flattens onto
            // black, as a channel has no transparency.
            px[c] = ((u32::from(src[0]) * u32::from(src[3]) + 127) / 255) as u8;
        }
    }
    open_pixels(editor, "Merged Channels", w, h, rgba)?;
    Ok(format!(
        "Merged Channels: red from {}, green from {}, blue from {}",
        names[0], names[1], names[2]
    ))
}

// ---------------------------------------------------------------------------
// PDF Presentation
// ---------------------------------------------------------------------------

/// One page of a presentation: its size and its image's compressed stream.
struct PdfSlide {
    page: raster::pdf::PdfPage,
    width: u32,
    height: u32,
    /// The image XObject's `FlateDecode` bytes, as the single-page encoder
    /// wrote them.
    stream: Vec<u8>,
}

/// The image stream of a single-page PDF [`raster::pdf::encode_pdf_on_page`]
/// wrote: object 5's bytes, `/Length` long.
fn image_stream(pdf: &[u8]) -> Option<Vec<u8>> {
    let find = |hay: &[u8], needle: &[u8], from: usize| {
        hay.get(from..)?
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    };
    let obj = find(pdf, b"5 0 obj\n", 0)?;
    let len_at = find(pdf, b"/Length ", obj)? + b"/Length ".len();
    let digits: String = pdf[len_at..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .map(|&b| b as char)
        .collect();
    let len: usize = digits.parse().ok()?;
    let start = find(pdf, b"stream\n", len_at)? + b"stream\n".len();
    pdf.get(start..start + len).map(<[u8]>::to_vec)
}

/// A multi-page PDF of `slides`, one image per page, each fitted to its page.
fn presentation_pdf(slides: &[PdfSlide]) -> Vec<u8> {
    fn num(v: f64) -> String {
        if v.fract() == 0.0 && v.abs() < 1e15 {
            format!("{}", v as i64)
        } else {
            format!("{v:.3}")
        }
    }
    let objects = 3 + 3 * slides.len();
    let mut out: Vec<u8> = b"%PDF-1.4\n% raster-studio presentation\n".to_vec();
    let mut offsets = vec![0usize; objects];
    let kids: Vec<String> = (0..slides.len())
        .map(|i| format!("{} 0 R", 3 + 3 * i))
        .collect();
    let mut obj = |out: &mut Vec<u8>, n: usize, head: String, stream: Option<&[u8]>| {
        offsets[n] = out.len();
        out.extend_from_slice(format!("{n} 0 obj\n{head}").as_bytes());
        if let Some(bytes) = stream {
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(bytes);
            out.extend_from_slice(b"\nendstream");
        }
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(
        &mut out,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        None,
    );
    obj(
        &mut out,
        2,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            slides.len()
        ),
        None,
    );
    for (i, s) in slides.iter().enumerate() {
        let (page, content, image) = (3 + 3 * i, 4 + 3 * i, 5 + 3 * i);
        obj(
            &mut out,
            page,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {content} 0 R \
                 /Resources << /XObject << /Im0 {image} 0 R >> >> >>",
                num(s.page.width_pt),
                num(s.page.height_pt)
            ),
            None,
        );
        let (x, y, w, h) = s.page.placement(s.width, s.height);
        let draw = format!(
            "q\n{} 0 0 {} {} {} cm\n/Im0 Do\nQ\n",
            num(w),
            num(h),
            num(x),
            num(y)
        );
        obj(
            &mut out,
            content,
            format!("<< /Length {} >>", draw.len()),
            Some(draw.as_bytes()),
        );
        obj(
            &mut out,
            image,
            format!(
                "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB \
                 /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>",
                s.width,
                s.height,
                s.stream.len()
            ),
            Some(&s.stream),
        );
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {objects}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets.iter().skip(1) {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {objects} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// A file name in `dir` starting `stem` that is not taken yet.
fn free_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem} {n}.{ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// File ▸ Automate ▸ PDF Presentation…: every open document, in tab order,
/// as one page of `Presentation.pdf` in a chosen folder, each page the
/// document's size at 72 ppi.
fn pdf_presentation(editor: &mut Editor) -> Result<String, String> {
    let count = editor.documents().len();
    if count == 0 {
        return Err("PDF Presentation: no document is open".to_string());
    }
    let Some(dir) = editor.pick_export_folder() else {
        return Err("PDF Presentation: no destination chosen".to_string());
    };
    let mut slides = Vec::with_capacity(count);
    for i in 0..count {
        let (w, h, rgba) = composite_of(editor, i)?;
        let page = raster::pdf::PdfPage::at_ppi(w, h, 72.0);
        let single = raster::pdf::encode_pdf_on_page(w, h, &rgba, page, None);
        let stream = image_stream(&single).ok_or("PDF Presentation: a page did not encode")?;
        slides.push(PdfSlide {
            page,
            width: w,
            height: h,
            stream,
        });
    }
    let path = free_path(&dir, "Presentation", "pdf");
    crate::doc::write_atomically(&path, &presentation_pdf(&slides))
        .map_err(|e| format!("PDF Presentation: {e}"))?;
    Ok(format!(
        "PDF Presentation: {count} page(s) written to {}",
        path.display()
    ))
}

// ---------------------------------------------------------------------------
// Resize Images / Generate Mockups
// ---------------------------------------------------------------------------

/// The active layer, when it is a smart object.
fn active_smart_object(editor: &Editor) -> Result<layer_model::LayerId, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let id = doc.document.active_layer().ok_or("Select a layer first")?;
    match doc.document.layers.get(id).map(|l| &l.kind) {
        Some(layer_model::LayerKind::SmartObject(_)) => Ok(id),
        _ => Err("Generate Mockups replaces a smart object: select one first".to_string()),
    }
}

/// The images in `dir` (not its sub-folders), by name.
fn image_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .and_then(raster::ImportFormat::from_extension)
                .is_some()
        })
        .collect();
    files.sort();
    Ok(files)
}

/// File ▸ Automate ▸ Resize Images…: every image fitted into the box and
/// written under its own name into the destination — in its own format when
/// this build writes it, as PNG otherwise.
fn resize_images(spec: &FolderJobSpec) -> Result<String, String> {
    let files = image_files(&spec.source)?;
    if files.is_empty() {
        return Err(format!(
            "Resize Images: {} holds no image this build reads",
            spec.source.display()
        ));
    }
    std::fs::create_dir_all(&spec.destination)
        .map_err(|e| format!("{}: {e}", spec.destination.display()))?;
    let (mut done, mut failed) = (0usize, Vec::new());
    for file in &files {
        let one = || -> Result<(), String> {
            let image = raster::decode_path(file).map_err(|e| e.to_string())?;
            let (w, h) = fitted_size(
                image.width,
                image.height,
                spec.max_width,
                spec.max_height,
                spec.enlarge,
            );
            let rgba = if (w, h) == (image.width, image.height) {
                image.rgba8
            } else {
                let space = color::ColorSpace::Srgb;
                let linear =
                    raster::linear_from_rgba8(image.width, image.height, &image.rgba8, &space)
                        .map_err(|e| e.to_string())?;
                let scaled = raster::resample(&linear, w, h, raster::ResampleFilter::Lanczos3)
                    .map_err(|e| e.to_string())?;
                raster::rgba8_from_linear(&scaled, &space).map_err(|e| e.to_string())?
            };
            let name = file.file_name().ok_or("no file name")?;
            let (format, out) = match crate::doc::export_format_for(file) {
                Some(format) => (format, spec.destination.join(name)),
                None => (
                    raster::ExportFormat::Png,
                    spec.destination.join(name).with_extension("png"),
                ),
            };
            let bytes = raster::encode(format, w, h, &rgba).map_err(|e| e.to_string())?;
            crate::doc::write_atomically(&out, &bytes).map_err(|e| e.to_string())
        };
        match one() {
            Ok(()) => done += 1,
            Err(e) => failed.push(format!("{}: {e}", file.display())),
        }
    }
    if done == 0 {
        return Err(format!(
            "Resize Images: nothing written ({})",
            failed.join("; ")
        ));
    }
    Ok(format!(
        "Resize Images: {done} image(s) written to {}{}",
        spec.destination.display(),
        if failed.is_empty() {
            String::new()
        } else {
            format!(", {} skipped", failed.len())
        }
    ))
}

/// File ▸ Automate ▸ Generate Mockups…: the active smart object shows each
/// image of the source folder in turn (Replace Contents), and the document
/// is exported as `<image name>.png` into the destination each time; the
/// replacement is undone after every export, so the document ends as it
/// began.
fn generate_mockups(editor: &mut Editor, spec: &FolderJobSpec) -> Result<String, String> {
    active_smart_object(editor)?;
    let files = image_files(&spec.source)?;
    if files.is_empty() {
        return Err(format!(
            "Generate Mockups: {} holds no image this build reads",
            spec.source.display()
        ));
    }
    std::fs::create_dir_all(&spec.destination)
        .map_err(|e| format!("{}: {e}", spec.destination.display()))?;
    let index = editor
        .documents()
        .iter()
        .position(|d| Some(d.id()) == editor.active().map(|a| a.id()))
        .ok_or("No document is open")?;
    let (mut done, mut failed) = (0usize, Vec::new());
    for file in &files {
        if let Err(e) = editor.replace_smart_object_contents(file) {
            failed.push(format!("{}: {e}", file.display()));
            continue;
        }
        let written = composite_of(editor, index).and_then(|(w, h, rgba)| {
            let bytes = raster::encode(raster::ExportFormat::Png, w, h, &rgba)
                .map_err(|e| e.to_string())?;
            let stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("mockup");
            let out = spec
                .destination
                .join(format!("{}.png", raster::sanitize_file_stem(stem)));
            crate::doc::write_atomically(&out, &bytes).map_err(|e| e.to_string())
        });
        editor
            .dispatch(crate::action::Action::Undo)
            .map_err(|e| format!("Generate Mockups: could not restore the smart object: {e}"))?;
        match written {
            Ok(()) => done += 1,
            Err(e) => failed.push(format!("{}: {e}", file.display())),
        }
    }
    if done == 0 {
        return Err(format!(
            "Generate Mockups: nothing written ({})",
            failed.join("; ")
        ));
    }
    Ok(format!(
        "Generate Mockups: {done} mockup(s) written to {}",
        spec.destination.display()
    ))
}

// ---------------------------------------------------------------------------
// Crop and Straighten Photos
// ---------------------------------------------------------------------------

/// One photo found on the scan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Photo {
    pub width: u32,
    pub height: u32,
    /// How far the photo was turned on the scan, degrees counter-clockwise
    /// in image space (y down), in `-45..=45`.
    pub angle_deg: f32,
    pub rgba: Vec<u8>,
}

/// Sample `rgba` (`w * h`) bilinearly at `(x, y)` (pixel centres at `.5`).
fn sample(rgba: &[u8], w: usize, h: usize, x: f32, y: f32) -> [u8; 4] {
    let fx = (x - 0.5).clamp(0.0, (w - 1) as f32);
    let fy = (y - 0.5).clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let px = |x: usize, y: usize, c: usize| f32::from(rgba[(y * w + x) * 4 + c]);
    let mut out = [0u8; 4];
    for (c, o) in out.iter_mut().enumerate() {
        let top = px(x0, y0, c) * (1.0 - tx) + px(x1, y0, c) * tx;
        let bottom = px(x0, y1, c) * (1.0 - tx) + px(x1, y1, c) * tx;
        *o = (top * (1.0 - ty) + bottom * ty).round() as u8;
    }
    out
}

/// The convex hull of `points` (Andrew's monotone chain), counter-clockwise.
fn hull(mut points: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut lower: Vec<[f32; 2]> = Vec::new();
    for &p in &points {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<[f32; 2]> = Vec::new();
    for &p in points.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// The smallest rectangle around `hull`: centre, size along its own axes and
/// the angle of its first axis, radians.
fn min_area_rect(hull: &[[f32; 2]]) -> ([f32; 2], [f32; 2], f32) {
    let mut best = (f32::MAX, [0.0, 0.0], [0.0, 0.0], 0.0f32);
    for i in 0..hull.len() {
        let a = hull[i];
        let b = hull[(i + 1) % hull.len()];
        let theta = (b[1] - a[1]).atan2(b[0] - a[0]);
        let (s, c) = theta.sin_cos();
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in hull {
            // Into the rectangle's frame: rotate by -theta.
            let u = p[0] * c + p[1] * s;
            let v = -p[0] * s + p[1] * c;
            lo = [lo[0].min(u), lo[1].min(v)];
            hi = [hi[0].max(u), hi[1].max(v)];
        }
        let area = (hi[0] - lo[0]) * (hi[1] - lo[1]);
        if area < best.0 {
            let (cu, cv) = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
            let centre = [cu * c - cv * s, cu * s + cv * c];
            best = (area, centre, [hi[0] - lo[0], hi[1] - lo[1]], theta);
        }
    }
    (best.1, best.2, best.3)
}

/// Every photo scanned onto a flat background in `rgba` (`w * h`): the
/// background is the median of the border pixels; each connected region
/// that differs from it (and is at least a little of the scan) is one
/// photo, straightened by the smallest rectangle around it and cropped a
/// pixel inside that rectangle's edge. Top to bottom, then left to right.
pub(crate) fn find_photos(rgba: &[u8], w: u32, h: u32) -> Vec<Photo> {
    let (w, h) = (w as usize, h as usize);
    if w < 4 || h < 4 || rgba.len() != w * h * 4 {
        return Vec::new();
    }
    // The background colour.
    let mut border: Vec<[u8; 4]> = Vec::new();
    for x in 0..w {
        for y in [0, h - 1] {
            let i = (y * w + x) * 4;
            border.push([rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]);
        }
    }
    for y in 0..h {
        for x in [0, w - 1] {
            let i = (y * w + x) * 4;
            border.push([rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]);
        }
    }
    let mut bg = [0u8; 4];
    for (c, v) in bg.iter_mut().enumerate() {
        let mut values: Vec<u8> = border.iter().map(|p| p[c]).collect();
        values.sort_unstable();
        *v = values[values.len() / 2];
    }
    let fg: Vec<bool> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let d: i32 = (0..4)
                .map(|c| (i32::from(p[c]) - i32::from(bg[c])).abs())
                .sum();
            d > 48
        })
        .collect();
    // Components on a reduced grid.
    let f = w.max(h).div_ceil(512).max(1);
    let (gw, gh) = (w.div_ceil(f), h.div_ceil(f));
    let mut cell = vec![false; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let (mut n, mut on) = (0, 0);
            for y in gy * f..((gy + 1) * f).min(h) {
                for x in gx * f..((gx + 1) * f).min(w) {
                    n += 1;
                    on += usize::from(fg[y * w + x]);
                }
            }
            cell[gy * gw + gx] = on * 4 >= n;
        }
    }
    let mut label = vec![0u32; gw * gh];
    let mut next = 0u32;
    let mut areas = vec![0usize];
    for start in 0..gw * gh {
        if !cell[start] || label[start] != 0 {
            continue;
        }
        next += 1;
        areas.push(0);
        let mut stack = vec![start];
        label[start] = next;
        while let Some(i) = stack.pop() {
            areas[next as usize] += 1;
            let (x, y) = ((i % gw) as isize, (i / gw) as isize);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= gw as isize || ny >= gh as isize {
                        continue;
                    }
                    let j = ny as usize * gw + nx as usize;
                    if cell[j] && label[j] == 0 {
                        label[j] = next;
                        stack.push(j);
                    }
                }
            }
        }
    }
    let min_area = (gw * gh / 500).max(16);
    let mut photos: Vec<([f32; 2], Photo)> = Vec::new();
    for k in 1..=next {
        if areas[k as usize] < min_area {
            continue;
        }
        // The component's edge pixels at full resolution.
        let mut points = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if !fg[y * w + x] || label[(y / f) * gw + x / f] != k {
                    continue;
                }
                let edge = x == 0
                    || y == 0
                    || x + 1 == w
                    || y + 1 == h
                    || !fg[y * w + x - 1]
                    || !fg[y * w + x + 1]
                    || !fg[(y - 1) * w + x]
                    || !fg[(y + 1) * w + x];
                if edge {
                    let (px, py) = (x as f32, y as f32);
                    points.extend([
                        [px, py],
                        [px + 1.0, py],
                        [px, py + 1.0],
                        [px + 1.0, py + 1.0],
                    ]);
                }
            }
        }
        let outline = hull(points);
        if outline.len() < 3 {
            continue;
        }
        let (centre, mut size, mut theta) = min_area_rect(&outline);
        // The nearest-upright reading of the same rectangle.
        let quarter = std::f32::consts::FRAC_PI_2;
        while theta > quarter / 2.0 {
            theta -= quarter;
            size = [size[1], size[0]];
        }
        while theta < -quarter / 2.0 {
            theta += quarter;
            size = [size[1], size[0]];
        }
        // A pixel inside the edge, so no background fringe survives.
        let (pw, ph) = (
            (size[0] - 2.0).round().max(1.0) as u32,
            (size[1] - 2.0).round().max(1.0) as u32,
        );
        let (s, c) = theta.sin_cos();
        let mut out = Vec::with_capacity((pw * ph * 4) as usize);
        for v in 0..ph {
            for u in 0..pw {
                let lx = u as f32 + 0.5 - pw as f32 / 2.0;
                let ly = v as f32 + 0.5 - ph as f32 / 2.0;
                let x = centre[0] + lx * c - ly * s;
                let y = centre[1] + lx * s + ly * c;
                out.extend(sample(rgba, w, h, x, y));
            }
        }
        photos.push((
            centre,
            Photo {
                width: pw,
                height: ph,
                angle_deg: -theta.to_degrees(),
                rgba: out,
            },
        ));
    }
    photos.sort_by(|a, b| {
        (a.0[1] / 8.0)
            .round()
            .total_cmp(&(b.0[1] / 8.0).round())
            .then(a.0[0].total_cmp(&b.0[0]))
    });
    photos.into_iter().map(|(_, p)| p).collect()
}

/// File ▸ Automate ▸ Crop and Straighten Photos: each photo on the active
/// document's scan opens as its own straightened document.
fn crop_and_straighten(editor: &mut Editor) -> Result<String, String> {
    let index = editor
        .documents()
        .iter()
        .position(|d| Some(d.id()) == editor.active().map(|a| a.id()))
        .ok_or("No document is open")?;
    let title = editor.documents()[index].title().to_string();
    let (w, h, rgba) = composite_of(editor, index)?;
    let photos = find_photos(&rgba, w, h);
    if photos.is_empty() {
        return Err(
            "Crop and Straighten Photos: no photo stands out from the scan's background"
                .to_string(),
        );
    }
    let count = photos.len();
    for (n, photo) in photos.into_iter().enumerate() {
        open_pixels(
            editor,
            &format!("{title} Photo {}", n + 1),
            photo.width,
            photo.height,
            photo.rgba,
        )?;
    }
    Ok(format!(
        "Crop and Straighten Photos: {count} photo(s) opened as documents"
    ))
}

// ---------------------------------------------------------------------------
// Convert to Point / Paragraph Text
// ---------------------------------------------------------------------------

/// Layer ▸ Text ▸ Convert to Point Text / Convert to Paragraph Text, one
/// undo step. Where the lines fall is measured with the compositor's own
/// shaping (the library the canvas renders with).
fn convert_text(editor: &mut Editor, to_paragraph: bool) -> Result<String, String> {
    use text_engine::frame_convert::{is_paragraph_text, to_paragraph_text, to_point_text};
    let (id, text) = super::active_text_layer(editor)?;
    if is_paragraph_text(&text) == to_paragraph {
        return Err(if to_paragraph {
            "The text is already paragraph text".to_string()
        } else {
            "The text is already point text".to_string()
        });
    }
    let run = text_engine::TextRun::from(&text);
    let rects = compositor::text_selection_rects(&run, 0, text.text.len());
    let converted = if to_paragraph {
        let right = rects.iter().map(|r| r.x + r.width).fold(1.0f32, f32::max);
        let bottom = rects
            .iter()
            .map(|r| r.y + r.height)
            .fold(text.size_px, f32::max);
        to_paragraph_text(&text, right, bottom)
    } else {
        let mut starts = Vec::new();
        let mut seen_y: Vec<f32> = Vec::new();
        for r in &rects {
            if seen_y.iter().any(|y| (y - r.y).abs() < 0.5) {
                continue;
            }
            seen_y.push(r.y);
            if let Some(i) = compositor::text_hit_index(&run, r.x + 0.01, r.y + r.height / 2.0) {
                starts.push(i);
            }
        }
        to_point_text(&text, &starts)
    };
    editor.apply_command(Command::SetLayerKind {
        layer_id: id,
        kind: Box::new(layer_model::LayerKind::Text(converted)),
    });
    Ok(if to_paragraph {
        "Converted to paragraph text".to_string()
    } else {
        "Converted to point text".to_string()
    })
}

#[cfg(test)]
#[path = "w13n_ops_tests.rs"]
mod tests;
