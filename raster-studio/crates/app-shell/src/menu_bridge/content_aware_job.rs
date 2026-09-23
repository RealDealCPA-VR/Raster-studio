//! W7-I: Edit ▸ Fill ▸ Contents: Content-Aware and Edit ▸ Content-Aware Scale
//! run their synthesis on a worker.
//!
//! PatchMatch hole filling ([`filters::content_aware_fill`]) and seam carving
//! ([`filters::content_aware_scale`]) are pure data work — a
//! [`filters::FilterBuffer`] in, a buffer out — and take from tens of
//! milliseconds to seconds, far past a frame. So the interaction thread only
//! snapshots the inputs, hands the synthesis to the editor's
//! [`crate::jobs::Spawner`] (worker threads in the desktop binary, inline in
//! the deterministic unit tests), and [`poll`] — called from
//! [`Editor::poll_jobs`] once a frame — applies the result through the same
//! painters the synchronous routes use, as ONE undo step.
//!
//! # A stale result never lands
//!
//! The job remembers which document, which layer, which selection and which
//! pixels it was computed from. When it finishes, the result applies only if
//! all four are still what the user is looking at; otherwise it is dropped
//! and the status line says why. One content-aware job runs at a time.

use std::sync::mpsc::{channel, Receiver, TryRecvError};

use layer_model::LayerId;

use crate::doc::DocumentId;
use crate::editor::Editor;

/// What a content-aware job computes, with the choices it applies with.
pub(crate) enum Kind {
    /// Edit ▸ Fill with Contents: Content-Aware, painted with this spec's
    /// blend mode, opacity and Preserve Transparency.
    Fill(Box<ui::dialogs::FillSpec>),
    /// Edit ▸ Content-Aware Scale ▸ this step.
    Scale(ui::menu::ContentAwareScaleStep),
}

impl Kind {
    fn name(&self) -> &'static str {
        match self {
            Kind::Fill(_) => "Content-Aware Fill",
            Kind::Scale(_) => "Content-Aware Scale",
        }
    }
}

/// A content-aware job in flight: its receiver and the snapshot it must
/// still match to apply.
pub(crate) struct Pending {
    rx: Receiver<Result<filters::FilterBuffer, String>>,
    kind: Kind,
    doc: DocumentId,
    layer: LayerId,
    selection: editor_core::Selection,
    source: filters::FilterBuffer,
}

/// The pixels a job of `kind` reads: 8-bit for the fill (the painter blends
/// the synthesised colour in at the layer's own depth), the layer's own depth
/// for the scale (the same read [`super::edit_active_pixels`] makes).
fn read_source(
    editor: &Editor,
    kind: &Kind,
    layer: LayerId,
) -> Result<filters::FilterBuffer, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let (w, h) = (doc.document.width(), doc.document.height());
    let buffer = match kind {
        Kind::Scale(_) if doc.is_sixteen_bit() => {
            filters::FilterBuffer::from_rgba16(w, h, &doc.layer_rgba16(layer))
        }
        _ => filters::FilterBuffer::from_rgba8(w, h, &super::pixels::read_layer(doc, layer)),
    };
    buffer.map_err(|e| e.to_string())
}

/// Start a content-aware job over the active pixel layer.
///
/// Refused on this thread — no job — for everything that can be refused
/// before the synthesis: no document, not a pixel layer, an empty canvas, no
/// selection to fill, a job already running. With the inline spawner the job
/// has finished and applied by the time this returns, and the answer is the
/// apply's; with worker threads the answer says it is running.
pub(crate) fn start(editor: &mut Editor, kind: Kind) -> Result<String, String> {
    if !editor.content_aware_jobs_mut().is_empty() {
        return Err(format!(
            "{} cannot start: a Content-Aware job is still running",
            kind.name()
        ));
    }
    let layer = super::pixel_layer(editor)?;
    let (w, h) = super::canvas_of(editor)?;
    if w == 0 || h == 0 {
        return Err("The canvas has no pixels".to_string());
    }
    let (doc_id, selection) = {
        let doc = editor.active().ok_or("No document is open")?;
        (doc.id(), doc.document.selection.clone())
    };
    let source = read_source(editor, &kind, layer)?;
    let work: Box<dyn FnOnce() -> Result<filters::FilterBuffer, String> + Send> = match &kind {
        Kind::Fill(_) => {
            if matches!(selection, editor_core::Selection::None) {
                return Err("Content-Aware fill needs a selection to fill".to_string());
            }
            let mut hole = vec![false; w as usize * h as usize];
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    if selection.coverage_at(glam::IVec2::new(x, y)) > 0.0 {
                        hole[y as usize * w as usize + x as usize] = true;
                    }
                }
            }
            let input = source.clone();
            Box::new(move || {
                filters::content_aware_fill(&input, &hole, filters::FillOptions::default())
                    .map_err(|e| format!("Content-Aware fill refused: {e}"))
            })
        }
        Kind::Scale(step) => {
            let (px, py) = step.percent();
            let nw = ((u64::from(w) * u64::from(px) + 50) / 100).max(1) as u32;
            let nh = ((u64::from(h) * u64::from(py) + 50) / 100).max(1) as u32;
            let input = source.clone();
            Box::new(move || {
                let carved = filters::content_aware_scale(&input, nw, nh)
                    .map_err(|e| format!("Content-Aware Scale refused: {e}"))?;
                // Placed centred on the canvas: a widening's overflow is
                // cropped, a narrowing leaves transparent bands.
                let mut out =
                    filters::FilterBuffer::transparent(w, h).map_err(|e| e.to_string())?;
                let ox = (i64::from(w) - i64::from(nw)) / 2;
                let oy = (i64::from(h) - i64::from(nh)) / 2;
                for y in 0..nh {
                    for x in 0..nw {
                        let (dx, dy) = (i64::from(x) + ox, i64::from(y) + oy);
                        if dx >= 0 && dy >= 0 && dx < i64::from(w) && dy < i64::from(h) {
                            out.set(dx as u32, dy as u32, carved.get(x, y));
                        }
                    }
                }
                Ok(out)
            })
        }
    };
    let (tx, rx) = channel();
    let spawn = editor.spawner();
    spawn(
        format!("{} job", kind.name()),
        Box::new(move || {
            // The receiver may be gone (the editor closed): nothing to tell.
            let _ = tx.send(work());
        }),
    )
    .map_err(|e| format!("{} could not start a worker: {e}", kind.name()))?;
    let pending = Pending {
        rx,
        kind,
        doc: doc_id,
        layer,
        selection,
        source,
    };
    match pending.rx.try_recv() {
        // Inline spawner: already done — apply it now, as one step.
        Ok(result) => finish(editor, pending, result),
        Err(TryRecvError::Empty) => {
            let running = format!("{} is running…", pending.kind.name());
            editor.content_aware_jobs_mut().push(pending);
            Ok(running)
        }
        Err(TryRecvError::Disconnected) => Err(format!(
            "{} failed: the worker stopped without reporting",
            pending.kind.name()
        )),
    }
}

/// Apply every content-aware job that has finished; its outcome goes to the
/// status line. Once a frame, from [`Editor::poll_jobs`].
pub(crate) fn poll(editor: &mut Editor) {
    if editor.content_aware_jobs_mut().is_empty() {
        return;
    }
    let jobs = std::mem::take(editor.content_aware_jobs_mut());
    for pending in jobs {
        match pending.rx.try_recv() {
            Ok(result) => {
                let message = match finish(editor, pending, result) {
                    Ok(message) | Err(message) => message,
                };
                editor.set_status(message);
            }
            Err(TryRecvError::Empty) => editor.content_aware_jobs_mut().push(pending),
            Err(TryRecvError::Disconnected) => editor.set_status(format!(
                "{} failed: the worker stopped without reporting",
                pending.kind.name()
            )),
        }
    }
}

/// Apply a finished job's result — if what it was computed from is still
/// what the user is looking at.
fn finish(
    editor: &mut Editor,
    pending: Pending,
    result: Result<filters::FilterBuffer, String>,
) -> Result<String, String> {
    let result = result?;
    let name = pending.kind.name();
    let stale = || format!("{name} was discarded: the layer or selection changed while it ran");
    {
        let doc = editor.active().ok_or_else(stale)?;
        if doc.id() != pending.doc
            || doc.document.active_layer() != Some(pending.layer)
            || doc.document.selection != pending.selection
        {
            return Err(stale());
        }
    }
    if read_source(editor, &pending.kind, pending.layer)? != pending.source {
        return Err(stale());
    }
    match pending.kind {
        Kind::Fill(spec) => {
            let (w, _) = super::canvas_of(editor)?;
            let filled = result.to_rgba8();
            let opacity = spec.opacity.clamp(0.0, 1.0);
            let row = w as usize;
            super::fill_selection_painting(editor, &spec, &move |x, y| {
                let i = (y as usize * row + x as usize) * 4;
                [
                    f32::from(filled[i]) / 255.0,
                    f32::from(filled[i + 1]) / 255.0,
                    f32::from(filled[i + 2]) / 255.0,
                    f32::from(filled[i + 3]) / 255.0 * opacity,
                ]
            })?;
            Ok(format!(
                "Filled the selection with Content-Aware at {}% opacity, {} mode",
                (spec.opacity * 100.0).round() as u32,
                spec.blend.label()
            ))
        }
        Kind::Scale(step) => {
            super::edit_active_pixels(editor, name, move |buffer, _| {
                *buffer = result;
                Ok(())
            })?;
            Ok(format!("Content-Aware Scale: {}", step.label()))
        }
    }
}
