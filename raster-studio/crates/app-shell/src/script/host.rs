//! W13-K: the editor half of File ▸ Script — every DOM call the prelude
//! makes, answered against the live [`Editor`] through the routes the menus
//! and panels already use (`Editor::dispatch`, `Editor::apply_command`,
//! `menu_bridge::perform`, `menu_bridge::fill_selection_with`), and folded
//! into ONE history step per document when the run ends.
//!
//! # One undo step
//!
//! Every edit a script makes lands in history as it happens, because the
//! script reads the document between its edits. The first time a run touches
//! a document, [`Host::track`] notes its history depth, lifts its history
//! ceiling for the run, and — for a document saved as a `.rstudio` package —
//! diverts its command journal to a side file. [`Host::finish`] then takes the
//! forward commands the run recorded, undoes them, and applies them again as
//! one [`Command::Transaction`] labelled [`SCRIPT_LABEL`]; the side file is
//! deleted, so the package journal holds that one transaction and not its
//! parts twice. One Ctrl+Z takes the whole run back.
//!
//! # What a script cannot reach
//!
//! Nothing here reads or writes a path the script names. `app.open` runs
//! File ▸ Open (the platform picker) and `saveAs` runs File ▸ Export (the
//! platform save picker, opened in the document's own export folder). W13X-6:
//! a name the script passes is suggested to that picker as its file name
//! (`dialogs::suggest_next_file_name`, which keeps only the last path
//! component), so the picker opens prefilled, as Photopea's does; the user
//! still confirms or changes it, and no folder of the script's choosing is
//! ever used.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use editor_core::{Command, LayerPatch, Selection};
use glam::{Affine2, IVec2, Vec2};
use layer_model::{BlendMode, Layer, LayerId, LayerKind};
use serde_json::{json, Value};
use ui::dialogs::{ScriptLogKind, ScriptLogLine};
use ui::menu::MenuAction;

use crate::action::Action;
use crate::doc::DocumentId;
use crate::editor::Editor;

/// The history label a script run's step carries.
pub const SCRIPT_LABEL: &str = "Script";

/// How long `app.open` waits for the picked file to finish importing.
const OPEN_WAIT: Duration = Duration::from_secs(30);

/// The history ceiling a touched document runs under, so no step of the run
/// is compacted away before [`Host::finish`] folds them.
const RUN_HISTORY_LIMIT: usize = 1_000_000;

struct Tracked {
    id: DocumentId,
    depth: usize,
    limit: usize,
    hold: Option<PathBuf>,
}

/// One run's view of the editor.
pub(crate) struct Host<'a> {
    editor: &'a mut Editor,
    tracked: Vec<Tracked>,
    pub(crate) log: Vec<ScriptLogLine>,
}

type Reply = Result<Value, String>;

impl<'a> Host<'a> {
    pub(crate) fn new(editor: &'a mut Editor) -> Self {
        Self {
            editor,
            tracked: Vec::new(),
            log: Vec::new(),
        }
    }

    /// Answer one DOM call with its JSON reply text: `{"v": value}` or
    /// `{"e": "why"}` (which the prelude throws as an `Error`).
    pub(crate) fn answer(&mut self, op: &str, args: &str) -> String {
        let reply = match serde_json::from_str::<Vec<Value>>(args) {
            Ok(args) => self.call(op, &args),
            Err(e) => Err(format!("bad arguments to {op}: {e}")),
        };
        match reply {
            Ok(v) => json!({ "v": v }).to_string(),
            Err(e) => json!({ "e": e }).to_string(),
        }
    }

    fn call(&mut self, op: &str, a: &[Value]) -> Reply {
        match op {
            "log" => {
                let kind = match str_at(a, 0)?.as_str() {
                    "alert" => ScriptLogKind::Alert,
                    "error" => ScriptLogKind::Error,
                    _ => ScriptLogKind::Output,
                };
                self.log.push(ScriptLogLine {
                    kind,
                    text: str_at(a, 1)?,
                });
                Ok(Value::Null)
            }
            "consts" => Ok(json!({
                "blend": BlendMode::ALL.iter().map(|m| blend_name(*m)).collect::<Vec<_>>(),
                "version": crate::version::about_line(),
            })),
            // ---- app ------------------------------------------------------
            "app.documents" => Ok(json!(self
                .editor
                .documents()
                .iter()
                .map(|d| d.id().0)
                .collect::<Vec<_>>())),
            "app.activeDocument" => Ok(self
                .editor
                .active()
                .map_or(Value::Null, |d| json!(d.id().0))),
            "app.setActiveDocument" => {
                let id = doc_at(a, 0)?;
                self.activate(id)?;
                Ok(Value::Null)
            }
            "app.color" => {
                let rgba = match str_at(a, 0)?.as_str() {
                    "backgroundColor" => self.editor.background(),
                    _ => self.editor.foreground(),
                };
                Ok(json!(bytes_of(rgba)))
            }
            "app.setColor" => {
                let rgba = rgba_of(a, 1)?;
                match str_at(a, 0)?.as_str() {
                    "backgroundColor" => self.editor.set_background(rgba),
                    _ => self.editor.set_foreground(rgba),
                }
                Ok(Value::Null)
            }
            "app.newDocument" => {
                let (w, h) = (dim_at(a, 0)?, dim_at(a, 1)?);
                let name = a
                    .get(2)
                    .and_then(Value::as_str)
                    .unwrap_or("Untitled")
                    .to_string();
                let background = match a.get(3).and_then(Value::as_str) {
                    Some("transparent") => crate::import::BlankBackground::Transparent,
                    Some("background") => crate::import::BlankBackground::Solid {
                        rgba8: crate::menu_bridge::rgba8_of(self.editor.background()),
                        depth: raster::BitDepth::Eight,
                    },
                    _ => crate::import::BlankBackground::Solid {
                        rgba8: [255, 255, 255, 255],
                        depth: raster::BitDepth::Eight,
                    },
                };
                self.editor
                    .new_document_with(w, h, &name, background)
                    .map_err(|e| e.to_string())?;
                self.editor
                    .active()
                    .map(|d| json!(d.id().0))
                    .ok_or_else(|| "the new document did not open".to_string())
            }
            "app.open" => self.open(a.first().and_then(Value::as_str)),
            // ---- document -------------------------------------------------
            "doc.info" => {
                let index = self.index_of(doc_at(a, 0)?)?;
                let open = &self.editor.documents()[index];
                Ok(json!({
                    "name": open.title(),
                    "width": open.document.width(),
                    "height": open.document.height(),
                    // The document stores no resolution (see the parity
                    // matrix); 72 ppi is what a pixel-sized document means.
                    "resolution": 72,
                }))
            }
            "doc.children" => {
                let index = self.index_of(doc_at(a, 0)?)?;
                let parent = opt_layer_at(a, 1)?;
                let layers = &self.editor.documents()[index].document.layers;
                let ids: Vec<LayerId> = match parent {
                    None => layers.root().to_vec(),
                    Some(parent) => match layers.get(parent).map(|l| &l.kind) {
                        Some(LayerKind::Group(g)) => g.children.clone(),
                        Some(_) => Vec::new(),
                        None => return Err("that layer set is gone".to_string()),
                    },
                };
                // The tree lists top-most first, as Photoshop's `layers`
                // does (`layers[0]` is the top of the stack).
                Ok(Value::Array(
                    ids.iter()
                        .filter_map(|id| layers.get(*id))
                        .map(|l| json!({ "id": id_json(l.id), "group": matches!(l.kind, LayerKind::Group(_)) }))
                        .collect(),
                ))
            }
            "doc.activeLayer" => {
                let index = self.index_of(doc_at(a, 0)?)?;
                let open = &self.editor.documents()[index];
                Ok(match open.document.active_layer() {
                    Some(id) => {
                        let group = matches!(
                            open.document.layers.get(id).map(|l| &l.kind),
                            Some(LayerKind::Group(_))
                        );
                        json!({ "id": id_json(id), "group": group })
                    }
                    None => Value::Null,
                })
            }
            "doc.setActiveLayer" => {
                let doc = doc_at(a, 0)?;
                let layer = layer_at(a, 1)?;
                self.focus_layer(doc, layer)?;
                Ok(Value::Null)
            }
            "doc.addLayer" => {
                let doc = doc_at(a, 0)?;
                let parent = opt_layer_at(a, 1)?;
                self.track(doc)?;
                let above = self.editor.active().and_then(|d| d.document.active_layer());
                self.editor
                    .dispatch(Action::NewLayer)
                    .map_err(|e| e.to_string())?;
                let id = self
                    .editor
                    .active()
                    .and_then(|d| d.document.active_layer())
                    .ok_or("the new layer did not appear")?;
                self.place_new(id, parent, above)?;
                Ok(json!({ "id": id_json(id), "group": false }))
            }
            "doc.addGroup" => {
                let doc = doc_at(a, 0)?;
                let parent = opt_layer_at(a, 1)?;
                self.track(doc)?;
                let above = self.editor.active().and_then(|d| d.document.active_layer());
                let name = {
                    let open = self.editor.active().ok_or("No document is open")?;
                    format!("Group {}", count_groups(&open.document) + 1)
                };
                let group = Layer::group(name);
                let id = group.id;
                self.apply(Command::create_layer(group))?;
                self.place_new(id, parent, above)?;
                self.focus_layer(doc, id)?;
                Ok(json!({ "id": id_json(id), "group": true }))
            }
            "doc.resizeImage" => {
                let doc = doc_at(a, 0)?;
                self.track(doc)?;
                let (w0, h0) = self.size()?;
                let (w, h) = match (opt_num(a, 1), opt_num(a, 2)) {
                    (Some(w), Some(h)) => (w, h),
                    (Some(w), None) => (w, w * f64::from(h0) / f64::from(w0)),
                    (None, Some(h)) => (h * f64::from(w0) / f64::from(h0), h),
                    (None, None) => return Err("resizeImage needs a width or a height".into()),
                };
                let filter = match a.get(3).and_then(Value::as_str) {
                    Some("nearest") => raster::ResampleFilter::Nearest,
                    Some("bilinear") => raster::ResampleFilter::Triangle,
                    Some("lanczos") => raster::ResampleFilter::Lanczos3,
                    _ => raster::ResampleFilter::Mitchell,
                };
                let spec = ui::dialogs::ImageSizeSpec {
                    width: to_dim(w)?,
                    height: to_dim(h)?,
                    resolution_ppi: 72.0,
                    resample: Some(filter),
                };
                if !spec.is_valid() {
                    return Err(format!(
                        "{}x{} is not a size this editor allows",
                        spec.width, spec.height
                    ));
                }
                let command = self
                    .editor
                    .active_mut()
                    .ok_or("No document is open")?
                    .resample_command(&spec)
                    .map_err(|e| e.to_string())?;
                self.apply(command)?;
                Ok(Value::Null)
            }
            "doc.resizeCanvas" => {
                let doc = doc_at(a, 0)?;
                self.track(doc)?;
                let (w0, h0) = self.size()?;
                let (w, h) = (dim_at(a, 1)?, dim_at(a, 2)?);
                let (fx, fy) = anchor_fraction(a.get(3).and_then(Value::as_str).unwrap_or(""));
                let min = IVec2::new(
                    ((f64::from(w0) - f64::from(w)) * fx).round() as i32,
                    ((f64::from(h0) - f64::from(h)) * fy).round() as i32,
                );
                self.resize_canvas(w, h, min)?;
                Ok(Value::Null)
            }
            "doc.crop" => {
                let doc = doc_at(a, 0)?;
                self.track(doc)?;
                let b = a
                    .get(1)
                    .and_then(Value::as_array)
                    .ok_or("crop needs [left, top, right, bottom]")?;
                let n = |i: usize| {
                    b.get(i)
                        .and_then(Value::as_f64)
                        .ok_or("crop needs four numbers")
                };
                let (l, t, r, bottom) = (n(0)?, n(1)?, n(2)?, n(3)?);
                if r <= l || bottom <= t {
                    return Err("crop bounds are empty".into());
                }
                self.resize_canvas(
                    to_dim(r - l)?,
                    to_dim(bottom - t)?,
                    IVec2::new(l.round() as i32, t.round() as i32),
                )?;
                Ok(Value::Null)
            }
            "doc.flatten" => self.perform(doc_at(a, 0)?, MenuAction::FlattenImage),
            "doc.mergeVisible" => self.perform(doc_at(a, 0)?, MenuAction::MergeVisible),
            "doc.saveAs" => {
                let doc = doc_at(a, 0)?;
                self.activate(doc)?;
                if let Some(name) = a.get(1).and_then(Value::as_str) {
                    self.log.push(ScriptLogLine {
                        kind: ScriptLogKind::Info,
                        text: format!(
                            "saveAs(\"{name}\"): the save picker opens with that name and chooses where the file goes"
                        ),
                    });
                    crate::dialogs::suggest_next_file_name(name);
                }
                let result = self.editor.dispatch(Action::Export);
                // A refusal before the picker opened must not leave the name
                // for a later, user-started picker.
                let _ = crate::dialogs::take_suggested_file_name();
                match result {
                    Ok(_) => Ok(json!(self.editor.status().unwrap_or("Exported"))),
                    Err(crate::editor::ActionError::Cancelled(_)) => Ok(Value::Null),
                    Err(e) => Err(e.to_string()),
                }
            }
            // ---- layers ---------------------------------------------------
            "layer.info" => {
                let index = self.index_of(doc_at(a, 0)?)?;
                let id = layer_at(a, 1)?;
                let open = &self.editor.documents()[index];
                let layer = open.document.layers.get(id).ok_or("that layer is gone")?;
                let bounds =
                    crate::tool_input::tight_document_bounds(&open.document, &open.tiles, id)
                        .map(|r| {
                            json!([
                                r.x,
                                r.y,
                                r.x + i64::from(r.width),
                                r.y + i64::from(r.height)
                            ])
                        })
                        .unwrap_or_else(|| json!([0, 0, 0, 0]));
                let text = match &layer.kind {
                    LayerKind::Text(t) => json!({
                        "contents": t.text,
                        "size": t.size_px,
                        "font": t.font_family,
                        "color": bytes_of(t.style.fill),
                        "position": [layer.transform.translation.x, layer.transform.translation.y],
                    }),
                    _ => Value::Null,
                };
                Ok(json!({
                    "name": layer.name,
                    "opacity": (layer.opacity * 100.0).round(),
                    "fillOpacity": (layer.fill_opacity * 100.0).round(),
                    "visible": layer.visible,
                    "blend": blend_name(layer.blend_mode),
                    "kind": kind_name(&layer.kind),
                    "bounds": bounds,
                    "text": text,
                }))
            }
            "layer.parent" => {
                let index = self.index_of(doc_at(a, 0)?)?;
                let id = layer_at(a, 1)?;
                Ok(self.editor.documents()[index]
                    .document
                    .layers
                    .parent_of(id)
                    .map_or(Value::Null, id_json))
            }
            "layer.set" => {
                let doc = doc_at(a, 0)?;
                let id = layer_at(a, 1)?;
                self.track(doc)?;
                let p = a
                    .get(2)
                    .and_then(Value::as_object)
                    .ok_or("layer.set needs a patch")?;
                let mut patch = LayerPatch::default();
                if let Some(v) = p.get("name").and_then(Value::as_str) {
                    patch.name = Some(v.to_string());
                }
                if let Some(v) = p.get("opacity").and_then(Value::as_f64) {
                    patch.opacity = Some(percent(v)?);
                }
                if let Some(v) = p.get("fillOpacity").and_then(Value::as_f64) {
                    patch.fill_opacity = Some(percent(v)?);
                }
                if let Some(v) = p.get("visible").and_then(Value::as_bool) {
                    patch.visible = Some(v);
                }
                if let Some(v) = p.get("blend").and_then(Value::as_str) {
                    patch.blend_mode = Some(blend_of(v)?);
                }
                self.apply(Command::SetLayerProperties {
                    layer_id: id,
                    patch,
                })?;
                Ok(Value::Null)
            }
            "layer.translate" => {
                let doc = doc_at(a, 0)?;
                let id = layer_at(a, 1)?;
                self.track(doc)?;
                let delta = Vec2::new(num_at(a, 2)? as f32, num_at(a, 3)? as f32);
                self.transform(id, Affine2::from_translation(delta))
            }
            "layer.scale" => {
                let doc = doc_at(a, 0)?;
                let id = layer_at(a, 1)?;
                self.track(doc)?;
                let (sx, sy) = (num_at(a, 2)? / 100.0, num_at(a, 3)? / 100.0);
                if sx == 0.0 || sy == 0.0 || !sx.is_finite() || !sy.is_finite() {
                    return Err("resize needs non-zero percentages".into());
                }
                let pivot = self.pivot(id, a.get(4).and_then(Value::as_str).unwrap_or(""))?;
                let m = Affine2::from_translation(pivot)
                    * Affine2::from_scale(Vec2::new(sx as f32, sy as f32))
                    * Affine2::from_translation(-pivot);
                self.transform(id, m)
            }
            "layer.rotate" => {
                let doc = doc_at(a, 0)?;
                let id = layer_at(a, 1)?;
                self.track(doc)?;
                let angle = (num_at(a, 2)? as f32).to_radians();
                let pivot = self.pivot(id, a.get(3).and_then(Value::as_str).unwrap_or(""))?;
                let m = Affine2::from_translation(pivot)
                    * Affine2::from_angle(angle)
                    * Affine2::from_translation(-pivot);
                self.transform(id, m)
            }
            "layer.duplicate" => {
                let doc = doc_at(a, 0)?;
                let id = layer_at(a, 1)?;
                self.focus_layer(doc, id)?;
                self.editor
                    .dispatch(Action::DuplicateLayer)
                    .map_err(|e| e.to_string())?;
                self.active_layer_json()
            }
            "layer.remove" => {
                let doc = doc_at(a, 0)?;
                let id = layer_at(a, 1)?;
                self.track(doc)?;
                self.apply(Command::DeleteLayer { layer_id: id })?;
                Ok(Value::Null)
            }
            "layer.mergeDown" => {
                let doc = doc_at(a, 0)?;
                let id = layer_at(a, 1)?;
                self.focus_layer(doc, id)?;
                self.perform(doc, MenuAction::MergeDown)?;
                self.active_layer_json()
            }
            "layer.toText" => {
                let doc = doc_at(a, 0)?;
                let id = layer_at(a, 1)?;
                self.track(doc)?;
                self.convert_to_text(doc, id)
            }
            "layer.setText" => {
                let doc = doc_at(a, 0)?;
                let id = layer_at(a, 1)?;
                self.track(doc)?;
                self.set_text(
                    id,
                    a.get(2)
                        .and_then(Value::as_object)
                        .ok_or("setText needs a patch")?,
                )
            }
            // ---- selection ------------------------------------------------
            "sel.all" => {
                self.track(doc_at(a, 0)?)?;
                let (w, h) = self.size()?;
                self.set_selection(Selection::Rect {
                    min: IVec2::ZERO,
                    max: IVec2::new(w as i32, h as i32),
                })
            }
            "sel.none" => {
                self.track(doc_at(a, 0)?)?;
                self.set_selection(Selection::None)
            }
            "sel.invert" => self.perform(doc_at(a, 0)?, MenuAction::InverseSelection),
            "sel.polygon" => {
                self.track(doc_at(a, 0)?)?;
                let points: Vec<Vec2> = a
                    .get(1)
                    .and_then(Value::as_array)
                    .ok_or("select needs an array of [x, y] points")?
                    .iter()
                    .map(|p| {
                        let x = p.get(0).and_then(Value::as_f64);
                        let y = p.get(1).and_then(Value::as_f64);
                        match (x, y) {
                            (Some(x), Some(y)) => Ok(Vec2::new(x as f32, y as f32)),
                            _ => Err("select needs an array of [x, y] points".to_string()),
                        }
                    })
                    .collect::<Result<_, _>>()?;
                if points.len() < 3 {
                    return Err("select needs at least three points".into());
                }
                if opt_num(a, 3).unwrap_or(0.0) > 0.0 {
                    return Err("select: a feather is not supported by the script runner; \
                                use Select > Modify > Feather"
                        .into());
                }
                let incoming = polygon_selection(&points)?;
                let op = match a.get(2).and_then(Value::as_str) {
                    Some("extend") => selection::BooleanOp::Add,
                    Some("diminish") => selection::BooleanOp::Subtract,
                    Some("intersect") => selection::BooleanOp::Intersect,
                    _ => selection::BooleanOp::Replace,
                };
                let (w, h) = self.size()?;
                let base = self
                    .editor
                    .active()
                    .ok_or("No document is open")?
                    .document
                    .selection
                    .clone();
                let canvas = selection::Rect::new(IVec2::ZERO, IVec2::new(w as i32, h as i32));
                let next = selection::combine_selection(canvas, &base, &incoming, op)
                    .map_err(|e| e.to_string())?;
                self.set_selection(next)
            }
            "sel.fill" => {
                self.track(doc_at(a, 0)?)?;
                let rgb = a
                    .get(1)
                    .and_then(Value::as_array)
                    .ok_or("fill needs a colour")?;
                let c = |i: usize| {
                    rgb.get(i)
                        .and_then(Value::as_f64)
                        .map(|v| (v / 255.0).clamp(0.0, 1.0) as f32)
                        .ok_or("fill needs a colour")
                };
                let spec = ui::dialogs::FillSpec {
                    contents: ui::dialogs::FillContents::Color([c(0)?, c(1)?, c(2)?, 1.0]),
                    blend: blend_of(a.get(2).and_then(Value::as_str).unwrap_or("normal"))?,
                    opacity: percent(opt_num(a, 3).unwrap_or(100.0))?,
                    preserve_transparency: a.get(4).and_then(Value::as_bool).unwrap_or(false),
                };
                let depth = self.depth();
                crate::menu_bridge::fill_selection_with(self.editor, &spec)?;
                if self.depth() == depth {
                    return Err(self.refusal("the fill changed nothing"));
                }
                Ok(Value::Null)
            }
            "sel.clear" => self.perform(doc_at(a, 0)?, MenuAction::ClearPixels),
            "sel.bounds" => {
                let index = self.index_of(doc_at(a, 0)?)?;
                Ok(self.editor.documents()[index]
                    .document
                    .selection
                    .bounds()
                    .map_or(Value::Null, |(min, max)| {
                        json!([min.x, min.y, max.x, max.y])
                    }))
            }
            other => Err(format!("{other} is not something the script runner knows")),
        }
    }

    // ---- documents ---------------------------------------------------------

    fn index_of(&self, id: DocumentId) -> Result<usize, String> {
        self.editor
            .documents()
            .iter()
            .position(|d| d.id() == id)
            .ok_or_else(|| "that document is closed".to_string())
    }

    fn activate(&mut self, id: DocumentId) -> Result<usize, String> {
        let index = self.index_of(id)?;
        self.editor.activate(index).map_err(|e| e.to_string())?;
        Ok(index)
    }

    /// Make `id` the active document and note what [`Host::finish`] needs to
    /// fold this run's edits on it into one step.
    fn track(&mut self, id: DocumentId) -> Result<usize, String> {
        let index = self.activate(id)?;
        if self.tracked.iter().any(|t| t.id == id) {
            return Ok(index);
        }
        let open = &mut self.editor.documents_mut()[index];
        if open.journal_hold().is_some() {
            return Err(format!(
                "{} is still being saved; run the script once the save has finished",
                open.title()
            ));
        }
        let hold = open.project_path().map(|_| {
            std::env::temp_dir().join(format!(
                "raster-studio-script-{}-{}.journal",
                std::process::id(),
                id.0
            ))
        });
        if let Some(side) = &hold {
            let _ = std::fs::remove_file(side);
            open.begin_journal_hold(side.clone());
        }
        let limit = open.history.limit();
        open.history.set_limit(RUN_HISTORY_LIMIT);
        self.tracked.push(Tracked {
            id,
            depth: open.history_depth(),
            limit,
            hold,
        });
        Ok(index)
    }

    /// Fold every tracked document's steps of this run into one, and put the
    /// journals and history ceilings back. Returns how many documents changed.
    pub(crate) fn finish(&mut self) -> usize {
        let active = self.editor.active().map(|d| d.id());
        let mut changed = 0;
        for t in std::mem::take(&mut self.tracked) {
            let Ok(index) = self.index_of(t.id) else {
                continue;
            };
            let _ = self.editor.activate(index);
            let depth = self.editor.documents()[index].history_depth();
            let forwards: Vec<Command> = if depth > t.depth {
                self.editor.documents()[index]
                    .history
                    .journal()
                    .skip(t.depth)
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
            if !forwards.is_empty() {
                self.editor.jump_history(t.depth);
            }
            let open = &mut self.editor.documents_mut()[index];
            if t.hold.is_some() {
                if let Some(side) = open.end_journal_hold() {
                    let _ = std::fs::remove_file(side);
                }
            }
            if !forwards.is_empty() {
                let count = forwards.len();
                self.editor.apply_command(Command::Transaction {
                    label: SCRIPT_LABEL.to_string(),
                    commands: forwards,
                });
                if self.editor.documents()[index].history_depth() == t.depth + 1 {
                    changed += 1;
                } else {
                    // The fold was refused: put the run's own steps back
                    // rather than lose them.
                    let why = self.refusal("the steps could not be folded");
                    self.editor.jump_history(t.depth + count);
                    self.log.push(ScriptLogLine {
                        kind: ScriptLogKind::Error,
                        text: format!("the run was kept as {count} separate steps: {why}"),
                    });
                    changed += 1;
                }
            }
            self.editor.documents_mut()[index]
                .history
                .set_limit(t.limit);
        }
        if let Some(id) = active {
            if let Ok(index) = self.index_of(id) {
                let _ = self.editor.activate(index);
            }
        }
        changed
    }

    fn open(&mut self, suggested: Option<&str>) -> Reply {
        if let Some(name) = suggested {
            self.log.push(ScriptLogLine {
                kind: ScriptLogKind::Info,
                text: format!(
                    "app.open(\"{name}\"): the file picker opens with that name and chooses what opens"
                ),
            });
            crate::dialogs::suggest_next_file_name(name);
        }
        let before: Vec<DocumentId> = self.editor.documents().iter().map(|d| d.id()).collect();
        let result = self.editor.dispatch(Action::Open);
        // Never left for a later, user-started picker.
        let _ = crate::dialogs::take_suggested_file_name();
        match result {
            Ok(_) => {}
            Err(crate::editor::ActionError::Cancelled(_)) => return Ok(Value::Null),
            Err(e) => return Err(e.to_string()),
        }
        let started = Instant::now();
        while self.editor.imports_pending() {
            self.editor.poll_imports();
            if !self.editor.imports_pending() {
                break;
            }
            if started.elapsed() > OPEN_WAIT {
                return Err("app.open: the file is still loading".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(self
            .editor
            .documents()
            .iter()
            .map(|d| d.id())
            .find(|id| !before.contains(id))
            .map_or(Value::Null, |id| json!(id.0)))
    }

    // ---- edits -------------------------------------------------------------

    fn depth(&self) -> usize {
        self.editor.active().map_or(0, |d| d.history_depth())
    }

    fn size(&self) -> Result<(u32, u32), String> {
        let open = self.editor.active().ok_or("No document is open")?;
        Ok((open.document.width(), open.document.height()))
    }

    fn refusal(&self, fallback: &str) -> String {
        self.editor.status().unwrap_or(fallback).to_string()
    }

    /// Apply one command to the active document through the editor, and
    /// report a refusal as an error rather than a status line.
    fn apply(&mut self, command: Command) -> Result<(), String> {
        let depth = self.depth();
        self.editor.apply_command(command);
        if self.depth() == depth {
            return Err(self.refusal("the edit was refused"));
        }
        Ok(())
    }

    /// A menu row, performed as a click would perform it.
    fn perform(&mut self, doc: DocumentId, action: MenuAction) -> Reply {
        self.track(doc)?;
        crate::menu_bridge::perform(action, self.editor)?;
        Ok(Value::Null)
    }

    /// Image ▸ Canvas Size's engine, with a refusal reported as an error.
    fn resize_canvas(&mut self, w: u32, h: u32, min: IVec2) -> Result<(), String> {
        let command = self
            .editor
            .active_mut()
            .ok_or("No document is open")?
            .resize_canvas(w, h, min)
            .map_err(|e| e.to_string())?;
        self.apply(command)
    }

    fn set_selection(&mut self, next: Selection) -> Reply {
        let current = self
            .editor
            .active()
            .ok_or("No document is open")?
            .document
            .selection
            .clone();
        if next != current {
            self.apply(Command::SetSelection { selection: next })?;
        }
        Ok(Value::Null)
    }

    /// Make `layer` the active (and only selected) layer of `doc`.
    fn focus_layer(&mut self, doc: DocumentId, layer: LayerId) -> Result<(), String> {
        self.track(doc)?;
        let exists = self
            .editor
            .active()
            .is_some_and(|d| d.document.layers.contains(layer));
        if !exists {
            return Err("that layer is gone".into());
        }
        self.editor.set_layer_selection(vec![layer], Some(layer));
        Ok(())
    }

    fn active_layer_json(&self) -> Reply {
        let open = self.editor.active().ok_or("No document is open")?;
        let id = open.document.active_layer().ok_or("no layer is active")?;
        let group = matches!(
            open.document.layers.get(id).map(|l| &l.kind),
            Some(LayerKind::Group(_))
        );
        Ok(json!({ "id": id_json(id), "group": group }))
    }

    /// Put a just-created layer where Photoshop's `add()` puts it: inside
    /// `parent` when one is named, otherwise directly above the layer that
    /// was active.
    fn place_new(
        &mut self,
        id: LayerId,
        parent: Option<LayerId>,
        above: Option<LayerId>,
    ) -> Result<(), String> {
        let layers = &self
            .editor
            .active()
            .ok_or("No document is open")?
            .document
            .layers;
        let siblings = |p: Option<LayerId>| -> Option<Vec<LayerId>> {
            match p {
                None => Some(layers.root().to_vec()),
                Some(g) => match layers.get(g).map(|l| &l.kind) {
                    Some(LayerKind::Group(group)) => Some(group.children.clone()),
                    _ => None,
                },
            }
        };
        // Lists are top-most first, so "directly above `a`" is `a`'s own
        // index once the new layer is out of the list.
        let target = match parent {
            Some(group) => {
                siblings(Some(group)).ok_or("that layer set is gone")?;
                Some((Some(group), 0))
            }
            None => above.and_then(|a| {
                let p = layers.parent_of(a);
                let without: Vec<LayerId> = siblings(p)?.into_iter().filter(|x| *x != id).collect();
                Some((p, without.iter().position(|x| *x == a)?))
            }),
        };
        if let Some((parent, index)) = target {
            let already =
                layers.parent_of(id) == parent && layers.index_in_parent(id) == Some(index);
            if !already {
                self.apply(Command::MoveLayer {
                    layer_id: id,
                    parent,
                    index,
                })?;
            }
        }
        Ok(())
    }

    fn transform(&mut self, id: LayerId, delta: Affine2) -> Reply {
        self.apply(Command::TransformLayer {
            layer_id: id,
            matrix: delta.to_cols_array(),
        })?;
        Ok(Value::Null)
    }

    /// The point of `id`'s document bounds the anchor names (the canvas's,
    /// for a layer with no ink).
    fn pivot(&self, id: LayerId, anchor: &str) -> Result<Vec2, String> {
        let open = self.editor.active().ok_or("No document is open")?;
        let rect = crate::tool_input::tight_document_bounds(&open.document, &open.tiles, id)
            .filter(|r| r.width > 0 && r.height > 0)
            .unwrap_or_else(|| {
                raster::PixelRect::new(0, 0, open.document.width(), open.document.height())
            });
        let (fx, fy) = anchor_fraction(anchor);
        Ok(Vec2::new(
            rect.x as f32 + rect.width as f32 * fx as f32,
            rect.y as f32 + rect.height as f32 * fy as f32,
        ))
    }

    /// `layer.kind = LayerKind.TEXT`: an empty text layer takes the layer's
    /// place in the stack (Photoshop converts an empty layer the same way).
    fn convert_to_text(&mut self, doc: DocumentId, id: LayerId) -> Reply {
        let (name, parent, index) = {
            let layers = &self
                .editor
                .active()
                .ok_or("No document is open")?
                .document
                .layers;
            let layer = layers.get(id).ok_or("that layer is gone")?;
            if matches!(layer.kind, LayerKind::Text(_)) {
                return Ok(id_json(id));
            }
            (
                layer.name.clone(),
                layers.parent_of(id),
                layers.index_in_parent(id).ok_or("that layer is gone")?,
            )
        };
        let mut text = layer_model::TextLayer::default();
        text.style.fill = self.editor.foreground();
        let layer = Layer::with_kind(name, LayerKind::Text(text));
        let new_id = layer.id;
        self.apply(Command::create_layer(layer))?;
        self.apply(Command::MoveLayer {
            layer_id: new_id,
            parent,
            index,
        })?;
        self.apply(Command::DeleteLayer { layer_id: id })?;
        self.focus_layer(doc, new_id)?;
        Ok(id_json(new_id))
    }

    fn set_text(&mut self, id: LayerId, p: &serde_json::Map<String, Value>) -> Reply {
        let (mut text, transform) = {
            let layer = self
                .editor
                .active()
                .ok_or("No document is open")?
                .document
                .layers
                .get(id)
                .ok_or("that layer is gone")?;
            match &layer.kind {
                LayerKind::Text(t) => (t.clone(), layer.transform),
                _ => return Err("the layer is not a text layer".into()),
            }
        };
        let before = text.clone();
        if let Some(v) = p.get("contents").and_then(Value::as_str) {
            text.text = v.to_string();
        }
        if let Some(v) = p.get("size").and_then(Value::as_f64) {
            if !(v.is_finite() && v > 0.0) {
                return Err("a text size must be above zero".into());
            }
            text.size_px = v as f32;
        }
        if let Some(v) = p.get("font").and_then(Value::as_str) {
            text.font_family = v.to_string();
        }
        if let Some(rgb) = p.get("color").and_then(Value::as_array) {
            let c = |i: usize| {
                rgb.get(i)
                    .and_then(Value::as_f64)
                    .map(|v| (v / 255.0).clamp(0.0, 1.0) as f32)
                    .ok_or("a colour needs three numbers")
            };
            text.style.fill = [c(0)?, c(1)?, c(2)?, text.style.fill[3]];
        }
        if text != before {
            self.apply(Command::SetLayerKind {
                layer_id: id,
                kind: Box::new(LayerKind::Text(text)),
            })?;
        }
        if let Some(pos) = p.get("position").and_then(Value::as_array) {
            let x = pos
                .first()
                .and_then(Value::as_f64)
                .ok_or("position needs [x, y]")?;
            let y = pos
                .get(1)
                .and_then(Value::as_f64)
                .ok_or("position needs [x, y]")?;
            let delta = Vec2::new(x as f32, y as f32) - transform.translation;
            if delta != Vec2::ZERO {
                self.transform(id, Affine2::from_translation(delta))?;
            }
        }
        Ok(Value::Null)
    }
}

// ---- argument and value helpers --------------------------------------------

fn str_at(a: &[Value], i: usize) -> Result<String, String> {
    a.get(i)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("argument {} should be text", i + 1))
}

fn num_at(a: &[Value], i: usize) -> Result<f64, String> {
    a.get(i)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("argument {} should be a number", i + 1))
}

fn opt_num(a: &[Value], i: usize) -> Option<f64> {
    a.get(i).and_then(Value::as_f64).filter(|v| v.is_finite())
}

fn to_dim(v: f64) -> Result<u32, String> {
    let r = v.round();
    if r >= 1.0 && r <= f64::from(u32::MAX) {
        Ok(r as u32)
    } else {
        Err(format!("{v} is not a size in pixels"))
    }
}

fn dim_at(a: &[Value], i: usize) -> Result<u32, String> {
    to_dim(num_at(a, i)?)
}

fn doc_at(a: &[Value], i: usize) -> Result<DocumentId, String> {
    a.get(i)
        .and_then(Value::as_u64)
        .map(DocumentId)
        .ok_or_else(|| "expected a document".to_string())
}

fn layer_at(a: &[Value], i: usize) -> Result<LayerId, String> {
    opt_layer_at(a, i)?.ok_or_else(|| "expected a layer".to_string())
}

fn opt_layer_at(a: &[Value], i: usize) -> Result<Option<LayerId>, String> {
    match a.get(i) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|_| "expected a layer".to_string()),
    }
}

fn id_json(id: LayerId) -> Value {
    serde_json::to_value(id).unwrap_or(Value::Null)
}

fn percent(v: f64) -> Result<f32, String> {
    if (0.0..=100.0).contains(&v) {
        Ok((v / 100.0) as f32)
    } else {
        Err(format!("{v} is not a percentage between 0 and 100"))
    }
}

fn bytes_of(rgba: [f32; 4]) -> [u8; 3] {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [b(rgba[0]), b(rgba[1]), b(rgba[2])]
}

fn rgba_of(a: &[Value], i: usize) -> Result<[f32; 4], String> {
    let rgb = a
        .get(i)
        .and_then(Value::as_array)
        .ok_or("expected a colour")?;
    let c = |k: usize| {
        rgb.get(k)
            .and_then(Value::as_f64)
            .map(|v| (v / 255.0).clamp(0.0, 1.0) as f32)
            .ok_or("expected a colour")
    };
    Ok([c(0)?, c(1)?, c(2)?, 1.0])
}

/// The name a blend mode goes by in the DOM: its variant, lower-cased
/// (`BlendMode.COLORBURN == "colorburn"`).
fn blend_name(mode: BlendMode) -> String {
    format!("{mode:?}").to_ascii_lowercase()
}

fn blend_of(name: &str) -> Result<BlendMode, String> {
    let key = name.to_ascii_lowercase().replace([' ', '_', '-'], "");
    BlendMode::ALL
        .iter()
        .copied()
        .find(|m| blend_name(*m) == key)
        .ok_or_else(|| format!("{name} is not a blend mode"))
}

fn kind_name(kind: &LayerKind) -> &'static str {
    match kind {
        LayerKind::Raster(_) => "normal",
        LayerKind::Group(_) => "group",
        LayerKind::Adjustment(_) => "adjustment",
        LayerKind::Text(_) => "text",
        LayerKind::Shape(_) => "shape",
        LayerKind::SmartObject(_) => "smartobject",
        LayerKind::Generator(_) => "generator",
        LayerKind::Fill(_) => "fill",
    }
}

fn count_groups(doc: &editor_core::Document) -> usize {
    doc.layers
        .iter_depth_first()
        .iter()
        .filter(|id| {
            matches!(
                doc.layers.get(**id).map(|l| &l.kind),
                Some(LayerKind::Group(_))
            )
        })
        .count()
}

/// `(x, y)` fractions of a box an anchor name picks; the centre otherwise.
fn anchor_fraction(anchor: &str) -> (f64, f64) {
    let a = anchor.to_ascii_lowercase();
    let fx = if a.ends_with("left") {
        0.0
    } else if a.ends_with("right") {
        1.0
    } else {
        0.5
    };
    let fy = if a.starts_with("top") {
        0.0
    } else if a.starts_with("bottom") {
        1.0
    } else {
        0.5
    };
    (fx, fy)
}

/// A polygon's selection: the exact rectangle when the four points are an
/// axis-aligned box on whole pixels, otherwise the lasso's coverage mask.
fn polygon_selection(points: &[Vec2]) -> Result<Selection, String> {
    if points.len() == 4 {
        let xs: Vec<f32> = points.iter().map(|p| p.x).collect();
        let ys: Vec<f32> = points.iter().map(|p| p.y).collect();
        let (x0, x1) = (
            xs.iter().copied().fold(f32::INFINITY, f32::min),
            xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        );
        let (y0, y1) = (
            ys.iter().copied().fold(f32::INFINITY, f32::min),
            ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        );
        let on_box = points
            .iter()
            .all(|p| (p.x == x0 || p.x == x1) && (p.y == y0 || p.y == y1));
        let whole = [x0, x1, y0, y1].iter().all(|v| v.fract() == 0.0);
        if on_box && whole && x1 > x0 && y1 > y0 {
            return Ok(Selection::Rect {
                min: IVec2::new(x0 as i32, y0 as i32),
                max: IVec2::new(x1 as i32, y1 as i32),
            });
        }
    }
    selection::lasso_polygonal(points)
        .map(Selection::Mask)
        .map_err(|e| e.to_string())
}
