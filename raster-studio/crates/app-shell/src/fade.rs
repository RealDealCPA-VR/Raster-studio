//! Edit ▸ Fade (W10-G): lay the last filter, adjustment, fill or stroke over
//! the pixels it replaced, at an opacity and in a blend mode — as one step
//! that replaces the step it fades.
//!
//! # What is fadeable, and how the shell knows
//!
//! The routes that write a whole pixel layer for a filter, an adjustment, a
//! fill or a stroke (`menu_bridge::edit_active_pixels`, `remap_active_layer`,
//! `fill_selection_painting`, `stroke_selection_with`) call [`remember`] with
//! the layer's pixels before and after, just before their command is applied.
//! Nothing else does, so a paint stroke, a transform, a new layer or a
//! selection change leaves the record describing a step that is no longer the
//! last one — and [`fadeable`] then says no.
//!
//! The record is *sealed* the first time it is looked at after the step
//! landed: the document's top undo entry must carry the step's label and the
//! layer's pixels must equal the recorded "after". Sealing keeps a copy of the
//! top entry's inverse command (tile hashes, not pixels); from then on the
//! step is still the last one exactly while the document's top undo entry is
//! that same inverse, which is cheap enough to ask every frame (the menu
//! context does). Undo moves the top entry, so Fade greys out after an undo,
//! as Photoshop's does.
//!
//! # The fade itself
//!
//! [`fade_with`] blends each pixel with [`ui::dialogs::FadeSpec::fade`]
//! (the step's result laid over the original by the blend mode, then mixed
//! back toward the original by `1 - opacity`), undoes the step, and applies
//! the faded pixels labelled "Fade <step>": the history holds one entry where
//! it held one before. A fade is not itself fadeable.

use std::cell::RefCell;

use editor_core::Command;
use layer_model::LayerId;

use crate::doc::DocumentId;
use crate::editor::Editor;
use crate::menu_bridge::pixels;

/// A layer's samples at the document's depth.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Samples {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}

/// A sample type [`remember`] accepts.
pub(crate) trait FadeSample: Copy {
    fn samples(values: &[Self]) -> Samples;
}

impl FadeSample for u8 {
    fn samples(values: &[Self]) -> Samples {
        Samples::Eight(values.to_vec())
    }
}

impl FadeSample for u16 {
    fn samples(values: &[Self]) -> Samples {
        Samples::Sixteen(values.to_vec())
    }
}

/// The last fadeable step.
#[derive(Clone, Debug)]
struct Record {
    doc: DocumentId,
    layer: LayerId,
    label: String,
    before: Samples,
    after: Samples,
    /// The top undo entry's inverse once the step was seen to land.
    sealed: Option<Command>,
}

thread_local! {
    static LAST: RefCell<Option<Record>> = const { RefCell::new(None) };
}

/// Record a fadeable step: `label` is about to replace `before` with `after`
/// on `layer` of document `doc`. Called by the routes named in the module
/// documentation just before their command is applied.
pub(crate) fn remember<S: FadeSample>(
    doc: DocumentId,
    layer: LayerId,
    label: &str,
    before: &[S],
    after: &[S],
) {
    LAST.with(|slot| {
        *slot.borrow_mut() = Some(Record {
            doc,
            layer,
            label: label.to_string(),
            before: S::samples(before),
            after: S::samples(after),
            sealed: None,
        })
    });
}

/// Forget the record (a fade was applied, or a test wants a clean slate).
pub(crate) fn forget() {
    LAST.with(|slot| *slot.borrow_mut() = None);
}

/// The layer's current samples at the document's depth.
fn current(open: &crate::doc::OpenDocument, layer: LayerId) -> Samples {
    if open.is_sixteen_bit() {
        Samples::Sixteen(open.layer_rgba16(layer))
    } else {
        Samples::Eight(pixels::read_layer(open, layer))
    }
}

/// The recorded step, when it is still the active document's last step.
fn live_record(editor: &Editor) -> Option<Record> {
    let open = editor.active()?;
    LAST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let record = slot.as_mut()?;
        if record.doc != open.id() || !open.document.layers.contains(record.layer) {
            return None;
        }
        let top = open.history.peek_undo()?;
        match &record.sealed {
            Some(sealed) => (sealed == top).then(|| record.clone()),
            None => {
                if open.history.undo_label() != Some(record.label.as_str())
                    || current(open, record.layer) != record.after
                {
                    return None;
                }
                record.sealed = Some(top.clone());
                Some(record.clone())
            }
        }
    })
}

/// The label of the step Edit ▸ Fade would fade, or `None` when the last
/// step is not a fadeable one. What the menu context reads each frame.
pub fn fadeable(editor: &Editor) -> Option<String> {
    live_record(editor).map(|r| r.label)
}

/// Apply a confirmed Fade: one step replacing the step it fades.
pub(crate) fn fade_with(
    editor: &mut Editor,
    spec: &ui::dialogs::FadeSpec,
) -> Result<String, String> {
    let record = live_record(editor).ok_or(ui::menu::FADE_NOTHING)?;
    let faded = match (&record.before, &record.after) {
        (Samples::Eight(b), Samples::Eight(a)) => Samples::Eight(spec.fade_rgba8(b, a)),
        (Samples::Sixteen(b), Samples::Sixteen(a)) => Samples::Sixteen(spec.fade_rgba16(b, a)),
        _ => return Err("The recorded step changed depth; it cannot be faded".to_string()),
    };
    if faded == record.after {
        return Err(format!(
            "Fade at {}% {} keeps {} as it is",
            (spec.opacity * 100.0).round(),
            spec.mode.label(),
            record.label
        ));
    }
    let label = format!("Fade {}", record.label);
    {
        let open = editor.active_mut().ok_or("No document is open")?;
        if !open.undo().map_err(|e| e.to_string())? {
            return Err(ui::menu::FADE_NOTHING.to_string());
        }
    }
    forget();
    if faded == record.before {
        // Nothing of the step is kept: the undo alone is the fade.
        return Ok(format!("{label}: none of {} kept", record.label));
    }
    let command = {
        let open = editor.active_mut().ok_or("No document is open")?;
        match &faded {
            Samples::Eight(rgba) => pixels::write_layer(open, record.layer, rgba, &label)?,
            Samples::Sixteen(rgba) => open.layer_rgba16_command(record.layer, rgba, &label)?,
        }
    };
    editor.apply_command(command);
    let landed = editor
        .active()
        .and_then(|open| open.history.undo_label().map(str::to_string));
    if landed.as_deref() != Some(label.as_str()) {
        return Err(format!("{label} was refused by the document"));
    }
    Ok(format!(
        "{label} at {}%, {}",
        (spec.opacity * 100.0).round(),
        spec.mode.label()
    ))
}
