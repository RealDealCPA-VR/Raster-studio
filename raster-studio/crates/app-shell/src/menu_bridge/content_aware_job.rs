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
//! and the status line says why. One content-aware job runs at a time: a
//! Fill or Scale is refused while any job is running or queued, while a spot
//! heal released meanwhile waits in the queue (W8-D round 2).
//!
//! # W8-D: the Spot Healing Brush's Content-Aware release
//!
//! The same worker runs the brush's heavy finish. With
//! [`tools::ToolContext::defer_heavy_commits`] set (the pointer route sets
//! it), the release snapshots the stroke's context and hands it over as a
//! [`tools::stroke::DeferredStroke`]; [`start_deferred_stroke`] runs its
//! [`tools::stroke::SpotSynthesis`] here, the status line counts the seconds
//! while it runs, Escape ([`cancel_deferred_strokes`]) drops it, and the
//! finish lands [`tools::stroke::DeferredStroke::finish`]'s command as the
//! stroke's ONE history entry, unless the pixels under the stroke changed
//! while it ran, in which case nothing lands and the status line says so.

use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::Instant;

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
    /// W8-D: a Spot Healing Brush stroke of the Content-Aware type, released
    /// with its synthesis handed over.
    SpotHeal(Box<tools::stroke::DeferredStroke>),
}

impl Kind {
    fn name(&self) -> &'static str {
        match self {
            Kind::Fill(_) => "Content-Aware Fill",
            Kind::Scale(_) => "Content-Aware Scale",
            Kind::SpotHeal(_) => "Content-Aware Spot Healing",
        }
    }
}

/// A content-aware job in flight: its receiver and the snapshot it must
/// still match to apply.
pub(crate) struct Pending {
    /// The worker's answer; `None` for a spot heal still WAITING in the
    /// queue behind the running job (W8-D round 2), not yet handed to a
    /// worker.
    rx: Option<Receiver<Result<filters::FilterBuffer, String>>>,
    kind: Kind,
    doc: DocumentId,
    layer: LayerId,
    selection: editor_core::Selection,
    /// The layer as the job read it; `None` for a spot heal, whose
    /// [`tools::stroke::DeferredStroke`] carries its own snapshot of just
    /// the stroke's context.
    source: Option<filters::FilterBuffer>,
    started: Instant,
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
        Kind::SpotHeal(_) => return Err("a spot heal carries its own snapshot".to_string()),
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
        Kind::SpotHeal(_) => {
            return Err("a spot heal starts through start_deferred_stroke".to_string())
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
    run(editor, kind, doc_id, layer, selection, Some(source), work)
}

/// W8-D: start the synthesis of a Spot Healing Brush release the tool
/// handed over ([`tools::Tool::take_deferred_commit`]) on the worker. Same
/// contract as [`start`]: with the inline spawner it has landed (as one
/// history entry) by the time this returns; with worker threads the answer
/// says it is running, and [`poll`] lands it.
///
/// W8-D round 2: a release while another content-aware job (an earlier
/// heal, a Content-Aware Fill or Scale) is still running is not refused: it
/// WAITS in the queue, in release order, and [`poll`] starts it once nothing
/// runs, re-snapshotted from the pixels as they are then
/// ([`tools::stroke::DeferredStroke::refresh`]). So click-click healing
/// lands every stroke, each as its own history entry, with the bytes the
/// same strokes released one after another synchronously make.
pub(crate) fn start_deferred_stroke(
    editor: &mut Editor,
    deferred: tools::stroke::DeferredStroke,
) -> Result<String, String> {
    let layer = target_layer(&deferred);
    let (doc_id, selection) = {
        let doc = editor.active().ok_or("No document is open")?;
        (doc.id(), doc.document.selection.clone())
    };
    let kind = Kind::SpotHeal(Box::new(deferred));
    if !editor.content_aware_jobs_mut().is_empty() {
        editor.content_aware_jobs_mut().push(Pending {
            rx: None,
            kind,
            doc: doc_id,
            layer,
            selection,
            source: None,
            started: Instant::now(),
        });
        return Ok(queued_status(editor));
    }
    launch_spot_heal(editor, kind, doc_id, layer, selection)
}

/// The layer a deferred stroke writes (its pixels or its mask).
fn target_layer(deferred: &tools::stroke::DeferredStroke) -> LayerId {
    match deferred.target() {
        editor_core::PixelTarget::Layer(id) | editor_core::PixelTarget::Mask(id) => id,
    }
}

/// How many spot heals wait in the queue.
fn queued_heals(editor: &mut Editor) -> usize {
    editor
        .content_aware_jobs_mut()
        .iter()
        .filter(|pending| pending.rx.is_none())
        .count()
}

/// The status line for a heal that joined the queue.
fn queued_status(editor: &mut Editor) -> String {
    format!(
        "Content-Aware Spot Healing is queued ({} waiting) behind the running Content-Aware job - Esc cancels",
        queued_heals(editor)
    )
}

/// Hand a spot heal's synthesis to the worker.
fn launch_spot_heal(
    editor: &mut Editor,
    kind: Kind,
    doc_id: DocumentId,
    layer: LayerId,
    selection: editor_core::Selection,
) -> Result<String, String> {
    let Kind::SpotHeal(deferred) = &kind else {
        return Err("only a spot heal launches here".to_string());
    };
    let snapshot = deferred.synthesis().clone();
    let work: Box<dyn FnOnce() -> Result<filters::FilterBuffer, String> + Send> =
        Box::new(move || {
            snapshot
                .run()
                .map_err(|e| format!("Content-Aware Spot Healing refused: {e}"))
        });
    run(editor, kind, doc_id, layer, selection, None, work)
}

/// Hand `work` to the editor's spawner and either apply its result now (the
/// inline spawner) or queue it for [`poll`].
fn run(
    editor: &mut Editor,
    kind: Kind,
    doc: DocumentId,
    layer: LayerId,
    selection: editor_core::Selection,
    source: Option<filters::FilterBuffer>,
    work: Box<dyn FnOnce() -> Result<filters::FilterBuffer, String> + Send>,
) -> Result<String, String> {
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
        rx: Some(rx),
        kind,
        doc,
        layer,
        selection,
        source,
        started: Instant::now(),
    };
    let polled = pending.rx.as_ref().map(Receiver::try_recv);
    match polled.unwrap_or(Err(TryRecvError::Disconnected)) {
        // Inline spawner: already done — apply it now, as one step.
        Ok(result) => finish(editor, pending, result),
        Err(TryRecvError::Empty) => {
            let running = running_status(&pending, 0);
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
    let waiting = jobs.iter().filter(|pending| pending.rx.is_none()).count();
    for pending in jobs {
        let Some(rx) = pending.rx.as_ref() else {
            // W8-D round 2: a queued heal keeps its place.
            editor.content_aware_jobs_mut().push(pending);
            continue;
        };
        match rx.try_recv() {
            Ok(result) => {
                let message = match finish(editor, pending, result) {
                    Ok(message) | Err(message) => message,
                };
                editor.set_status(message);
            }
            Err(TryRecvError::Empty) => {
                // W8-D: a spot heal counts its seconds on the status line
                // while the user waits on it.
                if matches!(pending.kind, Kind::SpotHeal(_)) {
                    editor.set_status(running_status(&pending, waiting));
                }
                editor.content_aware_jobs_mut().push(pending)
            }
            Err(TryRecvError::Disconnected) => editor.set_status(format!(
                "{} failed: the worker stopped without reporting",
                pending.kind.name()
            )),
        }
    }
    start_queued_heals(editor);
}

/// W8-D round 2: once nothing runs, start the queued heals in release order.
/// Each is re-snapshotted from the pixels as they are now (the heal before
/// it may have landed); one whose document is no longer the active one is
/// dropped with a status line. With the inline spawner each lands before the
/// next starts; with worker threads the first starts and the rest wait.
fn start_queued_heals(editor: &mut Editor) {
    let name = "Content-Aware Spot Healing";
    loop {
        let jobs = editor.content_aware_jobs_mut();
        if jobs.is_empty() || jobs.iter().any(|pending| pending.rx.is_some()) {
            return;
        }
        let pending = jobs.remove(0);
        let Kind::SpotHeal(mut deferred) = pending.kind else {
            continue;
        };
        let refreshed = match editor.active_mut() {
            Some(doc) if doc.id() == pending.doc => {
                let depth = doc.document.meta.bit_depth;
                let access =
                    crate::tool_input::DocumentTiles::new(&doc.document.pixels, &mut doc.tiles)
                        .at_document_depth(depth);
                deferred
                    .refresh(&access)
                    .map_err(|e| format!("{name} refused: {e}"))
            }
            _ => Err(format!(
                "{name} was discarded: its document is no longer the active one"
            )),
        };
        let message = refreshed.and_then(|()| {
            launch_spot_heal(
                editor,
                Kind::SpotHeal(deferred),
                pending.doc,
                pending.layer,
                pending.selection,
            )
        });
        editor.set_status(match message {
            Ok(message) | Err(message) => message,
        });
    }
}

/// What the status line says while `pending` runs, with `waiting` heals
/// queued behind it.
fn running_status(pending: &Pending, waiting: usize) -> String {
    match pending.kind {
        Kind::SpotHeal(_) if waiting > 0 => format!(
            "{} is running ({:.1} s, {waiting} more queued) - Esc cancels",
            pending.kind.name(),
            pending.started.elapsed().as_secs_f32()
        ),
        Kind::SpotHeal(_) => format!(
            "{} is running ({:.1} s) - Esc cancels",
            pending.kind.name(),
            pending.started.elapsed().as_secs_f32()
        ),
        _ => format!("{} is running…", pending.kind.name()),
    }
}

/// W8-D: Escape drops every Spot Healing synthesis still running or
/// queued: the worker's answer, when it comes, has no receiver and nothing
/// lands. Reports whether there was one.
pub(crate) fn cancel_deferred_strokes(editor: &mut Editor) -> bool {
    let jobs = std::mem::take(editor.content_aware_jobs_mut());
    let (heals, others): (Vec<_>, Vec<_>) = jobs
        .into_iter()
        .partition(|pending| matches!(pending.kind, Kind::SpotHeal(_)));
    *editor.content_aware_jobs_mut() = others;
    if heals.is_empty() {
        return false;
    }
    editor.set_status("Content-Aware Spot Healing was cancelled");
    true
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
    if let Kind::SpotHeal(deferred) = &pending.kind {
        // W8-D: a heal writes the layer it was stroked on with the selection
        // it was stroked under, so only the document and the pixels under
        // the stroke (checked by the finish itself) must still be the same.
        if editor.active().map(|doc| doc.id()) != Some(pending.doc) {
            return Err(format!(
                "{name} was discarded: its document is no longer the active one"
            ));
        }
        return finish_spot_heal(editor, deferred, &result);
    }
    {
        let doc = editor.active().ok_or_else(stale)?;
        if doc.id() != pending.doc
            || doc.document.active_layer() != Some(pending.layer)
            || doc.document.selection != pending.selection
        {
            return Err(stale());
        }
    }
    if Some(read_source(editor, &pending.kind, pending.layer)?) != pending.source {
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
        Kind::SpotHeal(_) => unreachable!("finished above"),
    }
}

/// W8-D: land a spot heal's synthesis as the stroke's one history entry,
/// through the same tile seam the pointer route paints through.
fn finish_spot_heal(
    editor: &mut Editor,
    deferred: &tools::stroke::DeferredStroke,
    synthesized: &filters::FilterBuffer,
) -> Result<String, String> {
    let name = "Content-Aware Spot Healing";
    let outcome = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let depth = doc.document.meta.bit_depth;
        let mut access =
            crate::tool_input::DocumentTiles::new(&doc.document.pixels, &mut doc.tiles)
                .at_document_depth(depth);
        deferred
            .finish(&mut access, synthesized)
            .map_err(|e| format!("{name} refused: {e}"))?
    };
    match outcome {
        tools::stroke::DeferredFinish::Landed(command) => {
            editor.apply_command(command);
            Ok(format!("{name}: the stroke was healed"))
        }
        tools::stroke::DeferredFinish::Unchanged => Ok(format!("{name} changed no pixel")),
        tools::stroke::DeferredFinish::Stale => Err(format!(
            "{name} was discarded: the pixels under the stroke changed while it ran"
        )),
    }
}

/// W8-D: the Spot Healing Brush's Content-Aware release through the real
/// pointer route ([`crate::tool_input::ToolPointer::handle`], the options
/// bar's Type riding the same settings seed the shell passes) and the job
/// seam ([`Editor::poll_jobs`], what the frame loop's `pump_jobs` calls).
#[cfg(test)]
mod spot_heal_tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use crate::tool_input::ToolPointer;
    use glam::Vec2;
    use tools::{ToolContext, ToolId, ToolSetting};
    use ui::canvas::{PointerInput, PointerPhase};

    const W: u32 = 64;
    const H: u32 = 64;
    const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);
    /// The stroke: straight down the red blotch.
    const PATH: [(f32, f32); 3] = [(30.0, 18.0), (30.0, 32.0), (30.0, 45.0)];

    thread_local! {
        static QUEUED: std::cell::RefCell<Vec<Box<dyn FnOnce() + Send>>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// A spawner that holds every job until the test runs it.
    fn queue_job(_name: String, body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
        QUEUED.with(|q| q.borrow_mut().push(body));
        Ok(())
    }

    /// Run every held job body on a real worker thread, joined.
    fn run_queued_on_a_worker() -> usize {
        let bodies: Vec<_> = QUEUED.with(|q| q.borrow_mut().drain(..).collect());
        let n = bodies.len();
        for body in bodies {
            std::thread::spawn(body).join().expect("the job body ran");
        }
        n
    }

    fn stripe(x: u32) -> [u8; 4] {
        if (x / 4).is_multiple_of(2) {
            [0, 0, 0, 255]
        } else {
            [255, 255, 255, 255]
        }
    }

    fn in_blotch(x: u32, y: u32) -> bool {
        (28..33).contains(&x) && (20..44).contains(&y)
    }

    /// Vertical stripes with a red blotch, opened, the camera at 100% with
    /// the image centred, the Spot Healing Brush selected.
    fn editor(dir: &std::path::Path, spawner: crate::jobs::Spawner) -> Editor {
        std::fs::create_dir_all(dir).unwrap();
        let mut rgba = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                rgba.extend_from_slice(&if in_blotch(x, y) {
                    [255, 0, 0, 255]
                } else if (44..48).contains(&x) && (20..32).contains(&y) {
                    // A second, green blotch for a second stroke.
                    [0, 255, 0, 255]
                } else {
                    stripe(x)
                });
            }
        }
        let png = dir.join("blotch.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, W, H, &rgba).unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        let doc = editor.active_mut().unwrap();
        doc.set_viewport(VIEWPORT);
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::new(W as f32 / 2.0, H as f32 / 2.0);
        editor.set_tool(ToolId::SpotHealing);
        editor.set_spawner(spawner);
        editor
    }

    fn screen(x: f32, y: f32) -> Vec2 {
        VIEWPORT * 0.5 + Vec2::new(x - W as f32 / 2.0, y - H as f32 / 2.0)
    }

    fn layer_pixels(editor: &Editor) -> Vec<u8> {
        let doc = editor.active().unwrap();
        let layer = doc.document.active_layer().unwrap();
        crate::menu_bridge::pixels::read_layer(doc, layer)
    }

    fn content_aware() -> Vec<(String, ToolSetting)> {
        vec![(
            tools::stroke::SPOT_HEAL_TYPE_KEY.to_string(),
            ToolSetting::Choice(1),
        )]
    }

    /// Press, drag down the blotch and release through the pointer route;
    /// the release's outcome.
    fn heal(pointer: &mut ToolPointer, editor: &mut Editor) -> crate::PointerOutcome {
        heal_along(pointer, editor, &PATH)
    }

    /// [`heal`] along `path`.
    fn heal_along(
        pointer: &mut ToolPointer,
        editor: &mut Editor,
        path: &[(f32, f32)],
    ) -> crate::PointerOutcome {
        let settings = content_aware();
        for (i, (x, y)) in path.iter().enumerate() {
            let phase = if i == 0 {
                PointerPhase::Down
            } else {
                PointerPhase::Move
            };
            pointer.handle(
                editor,
                PointerInput::at(phase, screen(*x, *y)),
                false,
                &settings,
            );
        }
        let (x, y) = path[path.len() - 1];
        pointer.handle(
            editor,
            PointerInput::at(PointerPhase::Up, screen(x, y)),
            false,
            &settings,
        )
    }

    /// The same stroke through the tool's own SYNCHRONOUS release (no
    /// deferral), applied through the editor: the reference bytes.
    fn synchronous_heal(editor: &mut Editor) -> Vec<u8> {
        synchronous_heal_along(editor, &PATH)
    }

    /// [`synchronous_heal`] along `path`.
    fn synchronous_heal_along(editor: &mut Editor, path: &[(f32, f32)]) -> Vec<u8> {
        let mut tool = tools::registry::make(ToolId::SpotHealing);
        tool.set_brush(editor.brush_for(ToolId::SpotHealing));
        tool.set_setting(tools::stroke::SPOT_HEAL_TYPE_KEY, ToolSetting::Choice(1))
            .unwrap();
        let commands = {
            let doc = editor.active_mut().unwrap();
            let canvas = doc.canvas_rect();
            let layer = doc.document.active_layer();
            let depth = doc.document.meta.bit_depth;
            let mut access =
                crate::tool_input::DocumentTiles::new(&doc.document.pixels, &mut doc.tiles)
                    .at_document_depth(depth);
            let mut ctx = ToolContext::new(&mut access, canvas);
            ctx.active_layer = layer;
            assert!(!ctx.defer_heavy_commits, "the reference must not defer");
            let at = |(x, y): (f32, f32)| tools::PointerEvent::at(x, y);
            tool.on_pointer_down(&mut ctx, at(path[0])).unwrap();
            for p in &path[1..] {
                tool.on_pointer_move(&mut ctx, at(*p)).unwrap();
            }
            tool.on_pointer_up(&mut ctx, at(path[path.len() - 1]))
                .unwrap();
            ctx.drain()
        };
        assert_eq!(commands.len(), 1, "the synchronous release emits one paint");
        for command in commands {
            editor.apply_command(command);
        }
        layer_pixels(editor)
    }

    /// The synthesis runs on the worker: the release lands nothing and
    /// returns, the frames keep going (polls, hover samples) with the
    /// pixels and history untouched and the status line counting; once the
    /// worker has run, the next poll lands exactly the synchronous release's
    /// bytes as ONE history entry, which one Undo takes back.
    #[test]
    fn a_content_aware_spot_heal_runs_on_the_worker_and_lands_the_synchronous_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut reference = editor(&dir.path().join("sync"), crate::jobs::run_inline);
        let expected = synchronous_heal(&mut reference);

        let mut editor = editor(&dir.path().join("job"), queue_job);
        let before = layer_pixels(&editor);
        assert_ne!(expected, before, "the reference heal changed nothing");
        let depth = editor.active().unwrap().history_depth();
        let mut pointer = ToolPointer::new();

        let up = heal(&mut pointer, &mut editor);
        assert_eq!(up.failed, None);
        assert_eq!(up.steps, 0, "the release itself lands no step");
        assert!(editor.jobs_pending(), "the synthesis is a job in flight");
        assert!(
            editor.status().is_some_and(|s| s.contains("running")),
            "{:?}",
            editor.status()
        );
        for _ in 0..3 {
            editor.poll_jobs();
            // A hover between frames reaches the pointer route as usual.
            pointer.handle(
                &mut editor,
                PointerInput::at(PointerPhase::Move, screen(5.0, 5.0)),
                false,
                &content_aware(),
            );
            assert!(editor.jobs_pending(), "still in flight");
            assert!(
                editor
                    .status()
                    .is_some_and(|s| s.contains("running") && s.contains("Esc cancels")),
                "{:?}",
                editor.status()
            );
        }
        assert_eq!(
            layer_pixels(&editor),
            before,
            "nothing lands before the worker ran"
        );
        assert_eq!(editor.active().unwrap().history_depth(), depth);

        assert_eq!(run_queued_on_a_worker(), 1);
        editor.poll_jobs();
        assert!(!editor.jobs_pending(), "nothing left in flight");
        let after = layer_pixels(&editor);
        assert!(
            after == expected,
            "the worker's heal differs from the synchronous release"
        );
        let red = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let i = ((y * W + x) * 4) as usize;
                after[i..i + 4] == [255, 0, 0, 255]
            })
            .count();
        assert_eq!(red, 0, "{red} blotch pixels are still red");
        assert_eq!(editor.active().unwrap().history_depth(), depth + 1);
        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(
            layer_pixels(&editor),
            before,
            "one Undo takes the heal back"
        );
    }

    /// Escape (the route the shell's Escape and focus loss call) drops the
    /// running synthesis: its answer arrives to no one and nothing lands.
    #[test]
    fn escape_cancels_a_running_content_aware_spot_heal() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path(), queue_job);
        let before = layer_pixels(&editor);
        let depth = editor.active().unwrap().history_depth();
        let mut pointer = ToolPointer::new();
        heal(&mut pointer, &mut editor);
        assert!(editor.jobs_pending());

        assert!(pointer.cancel(&mut editor), "there was a job to cancel");
        assert!(!editor.jobs_pending());
        assert!(
            editor.status().is_some_and(|s| s.contains("cancelled")),
            "{:?}",
            editor.status()
        );
        assert_eq!(run_queued_on_a_worker(), 1);
        editor.poll_jobs();
        assert_eq!(layer_pixels(&editor), before);
        assert_eq!(editor.active().unwrap().history_depth(), depth);
    }

    /// W8-D round 2: the second stroke of a click-click heal, released while
    /// the first is still synthesising, is not thrown away: it waits in the
    /// queue (the release reports no failure), starts when the first has
    /// landed, re-reads the pixels the first heal left, and lands as its own
    /// history entry. The bytes equal the two strokes released one after
    /// the other synchronously.
    #[test]
    fn a_spot_heal_released_while_another_runs_is_queued_and_lands_after_it() {
        // Overlapping the first stroke's context, so the second heal's
        // answer depends on the first one having landed.
        const SECOND: [(f32, f32); 2] = [(46.0, 20.0), (46.0, 31.0)];
        let dir = tempfile::tempdir().unwrap();
        let mut reference = editor(&dir.path().join("sync"), crate::jobs::run_inline);
        let after_first = synchronous_heal(&mut reference);
        let expected = synchronous_heal_along(&mut reference, &SECOND);
        assert_ne!(
            after_first, expected,
            "the second reference heal changed nothing"
        );

        let mut editor = editor(&dir.path().join("job"), queue_job);
        let depth = editor.active().unwrap().history_depth();
        let mut pointer = ToolPointer::new();
        assert_eq!(heal(&mut pointer, &mut editor).failed, None);
        let second = heal_along(&mut pointer, &mut editor, &SECOND);
        assert_eq!(second.failed, None, "the second release was refused");
        assert!(
            editor.status().is_some_and(|s| s.contains("queued")),
            "{:?}",
            editor.status()
        );
        editor.poll_jobs();
        assert!(
            editor
                .status()
                .is_some_and(|s| s.contains("1 more queued") && s.contains("Esc cancels")),
            "{:?}",
            editor.status()
        );

        // The first heal's worker runs; the poll lands it and starts the
        // second on the worker.
        assert_eq!(run_queued_on_a_worker(), 1, "only the first ran");
        editor.poll_jobs();
        assert_eq!(layer_pixels(&editor), after_first, "the first heal landed");
        assert_eq!(editor.active().unwrap().history_depth(), depth + 1);
        assert!(editor.jobs_pending(), "the second heal is now running");

        assert_eq!(run_queued_on_a_worker(), 1, "the second heal was started");
        editor.poll_jobs();
        assert!(!editor.jobs_pending());
        assert!(
            layer_pixels(&editor) == expected,
            "the queued heal differs from two synchronous releases"
        );
        assert_eq!(editor.active().unwrap().history_depth(), depth + 2);
        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(layer_pixels(&editor), after_first, "each heal is one step");
    }

    /// W8-D round 2: a heal released while an Edit > Content-Aware Scale job
    /// runs waits for it and heals the scaled pixels; Escape while it is
    /// queued drops it without touching the running Scale.
    #[test]
    fn a_spot_heal_released_during_a_content_aware_scale_waits_for_it() {
        let step = ui::menu::ContentAwareScaleStep::Height90;
        let dir = tempfile::tempdir().unwrap();
        let mut reference = editor(&dir.path().join("sync"), crate::jobs::run_inline);
        start(&mut reference, Kind::Scale(step)).unwrap();
        let scaled = layer_pixels(&reference);
        let expected = synchronous_heal(&mut reference);
        assert_ne!(scaled, expected, "the reference heal changed nothing");

        let mut editor = editor(&dir.path().join("job"), queue_job);
        let depth = editor.active().unwrap().history_depth();
        start(&mut editor, Kind::Scale(step)).unwrap();
        let mut pointer = ToolPointer::new();
        let up = heal(&mut pointer, &mut editor);
        assert_eq!(up.failed, None, "the heal was refused behind the Scale");
        assert_eq!(run_queued_on_a_worker(), 1, "only the Scale ran");
        editor.poll_jobs();
        assert_eq!(layer_pixels(&editor), scaled);
        assert_eq!(run_queued_on_a_worker(), 1, "the heal started after it");
        editor.poll_jobs();
        assert!(!editor.jobs_pending());
        assert!(
            layer_pixels(&editor) == expected,
            "the heal read stale pixels"
        );
        assert_eq!(editor.active().unwrap().history_depth(), depth + 2);

        // Escape drops a queued heal; the running Scale still lands.
        let mut editor = editor_again(&dir.path().join("esc"));
        let depth = editor.active().unwrap().history_depth();
        start(&mut editor, Kind::Scale(step)).unwrap();
        let mut pointer = ToolPointer::new();
        heal(&mut pointer, &mut editor);
        assert!(pointer.cancel(&mut editor), "the queued heal was cancelled");
        assert!(editor.jobs_pending(), "the Scale still runs");
        assert_eq!(run_queued_on_a_worker(), 1);
        editor.poll_jobs();
        assert!(!editor.jobs_pending());
        assert_eq!(QUEUED.with(|q| q.borrow().len()), 0, "no heal started");
        assert_eq!(layer_pixels(&editor), scaled);
        assert_eq!(editor.active().unwrap().history_depth(), depth + 1);
    }

    fn editor_again(dir: &std::path::Path) -> Editor {
        editor(dir, queue_job)
    }

    /// A heal computed from pixels that changed while it ran (an edit under
    /// the stroke) is discarded, not painted over the new pixels.
    #[test]
    fn a_spot_heal_whose_pixels_changed_while_it_ran_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path(), queue_job);
        let mut pointer = ToolPointer::new();
        heal(&mut pointer, &mut editor);
        assert!(editor.jobs_pending());
        // A brush dab across the stroke's context, through the same route.
        editor.set_tool(ToolId::Brush);
        editor.set_foreground([0.0, 0.0, 1.0, 1.0]);
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            pointer.handle(
                &mut editor,
                PointerInput::at(phase, screen(30.0, 30.0)),
                false,
                &[],
            );
        }
        let edited = layer_pixels(&editor);
        let depth = editor.active().unwrap().history_depth();

        assert_eq!(run_queued_on_a_worker(), 1);
        editor.poll_jobs();
        assert_eq!(layer_pixels(&editor), edited, "a stale heal landed");
        assert_eq!(editor.active().unwrap().history_depth(), depth);
        assert!(
            editor.status().is_some_and(|s| s.contains("discarded")),
            "{:?}",
            editor.status()
        );
    }
}
