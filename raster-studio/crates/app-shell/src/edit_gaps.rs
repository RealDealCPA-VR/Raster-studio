//! W10-G: the Edit-menu gaps the shell hosts — Preset Manager, Fade,
//! Auto-Align Layers, Auto-Blend Layers and Perspective Warp.
//!
//! Each opens a dialog from `ui::dialogs` through the dialog host
//! ([`GapDialog`], one [`crate::dialog_host::ActiveDialog`] variant for all
//! five), parks the confirmed answer here, and pushes its own menu action so
//! [`crate::menu_bridge::perform`] reaches [`perform`], which applies it to
//! the live editor as one undoable step (the Preset Manager's edit is to the
//! application's preset store, which has no history: Cancel is its undo).
//!
//! Two of the Preset Manager's kinds, swatches and tool presets, are not in
//! the preset store: they are the Swatches and Tool Presets panels' lists on
//! the chrome's [`ui::Workspace`], which the dialog host never sees. The
//! per-frame [`sync_workspace_presets`] (called from
//! `Editor::sync_panel_presets`, the hook that already keeps those panels and
//! the preferences in step) publishes a copy of both lists for the dialog to
//! open over, and writes a confirmed edit of them back into the panels on
//! the next frame.

use std::cell::RefCell;

use asset_store::presets::{PatternPreset, PresetStore};
use asset_store::resources::{GradientResource, ShapeResource};
use editor_core::Command;
use filters::FilterBuffer;
use layer_model::LayerId;
use ui::dialogs::{
    AutoAlignDialog, AutoAlignSpec, AutoBlendDialog, AutoBlendSpec, DialogOutcome, FadeDialog,
    FadeSpec, PerspectiveWarpDialog, PerspectiveWarpSpec, PresetEntry, PresetFileRequest,
    PresetLibrary, PresetManagerDialog,
};
use ui::menu::MenuAction;
use ui::panels::tool_presets::SavedToolPreset;

use crate::chrome::ChromeOutput;
use crate::editor::Editor;
use crate::menu_bridge::pixels;

thread_local! {
    static CONFIRMED_PRESETS: RefCell<Option<PresetLibrary>> = const { RefCell::new(None) };
    static CONFIRMED_FADE: RefCell<Option<FadeSpec>> = const { RefCell::new(None) };
    static CONFIRMED_ALIGN: RefCell<Option<AutoAlignSpec>> = const { RefCell::new(None) };
    static CONFIRMED_BLEND: RefCell<Option<AutoBlendSpec>> = const { RefCell::new(None) };
    static CONFIRMED_WARP: RefCell<Option<PerspectiveWarpSpec>> = const { RefCell::new(None) };
    /// The workspace's swatches and tool presets as the last frame left them.
    static WORKSPACE_LISTS: RefCell<WorkspaceLists> = RefCell::new(WorkspaceLists::default());
    /// A confirmed Preset Manager edit of those lists, for the next frame.
    static PENDING_WORKSPACE: RefCell<Option<WorkspaceLists>> = const { RefCell::new(None) };
}

/// The Preset Manager kinds that live on the chrome's workspace.
#[derive(Clone, Debug, Default, PartialEq)]
struct WorkspaceLists {
    swatches: Vec<(String, [f32; 4])>,
    tool_presets: Vec<SavedToolPreset>,
}

impl WorkspaceLists {
    fn of(w: &ui::Workspace) -> Self {
        Self {
            swatches: w
                .swatches
                .swatches()
                .iter()
                .map(|s| (s.name.clone(), s.rgba))
                .collect(),
            tool_presets: w.tool_presets.saved(),
        }
    }

    /// The two lists out of an edited library; a payload that does not
    /// parse is refused with the entry's name.
    fn from_library(library: &PresetLibrary) -> Result<Self, String> {
        let mut swatches = Vec::with_capacity(library.swatches.len());
        for e in &library.swatches {
            let rgba: [f32; 4] = serde_json::from_str(&e.data)
                .map_err(|err| format!("Swatch \"{}\": {err}", e.name))?;
            if !rgba
                .iter()
                .all(|c| c.is_finite() && (0.0..=1.0).contains(c))
            {
                return Err(format!("Swatch \"{}\": its colour is out of range", e.name));
            }
            swatches.push((e.name.clone(), rgba));
        }
        let mut tool_presets = Vec::with_capacity(library.tool_presets.len());
        for e in &library.tool_presets {
            let mut preset: SavedToolPreset = serde_json::from_str(&e.data)
                .map_err(|err| format!("Tool preset \"{}\": {err}", e.name))?;
            if preset.to_preset().is_none() {
                return Err(format!(
                    "Tool preset \"{}\": this build has no tool \"{}\"",
                    e.name, preset.tool
                ));
            }
            preset.name = e.name.clone();
            tool_presets.push(preset);
        }
        Ok(Self {
            swatches,
            tool_presets,
        })
    }

    /// Put these lists into the panels. Each list is rebuilt through the
    /// panel's own add/remove, so it counts as the user's edit and the
    /// preferences hook writes it; a list that is already equal is left
    /// alone.
    fn apply_to(&self, w: &mut ui::Workspace) {
        let live = Self::of(w);
        if live.swatches != self.swatches {
            while w.swatches.remove(0).is_some() {}
            for (name, rgba) in &self.swatches {
                w.swatches.add(name.clone(), *rgba);
            }
        }
        if live.tool_presets != self.tool_presets {
            while w.tool_presets.remove(0).is_some() {}
            for saved in &self.tool_presets {
                if let Some(preset) = saved.to_preset() {
                    w.tool_presets.add(preset);
                }
            }
        }
    }
}

/// Once a frame, with the chrome's workspace: a confirmed Preset Manager
/// edit of the swatches and tool presets lands in their panels, then the
/// panels' lists are published for the next time the dialog opens.
pub fn sync_workspace_presets(w: &mut ui::Workspace) {
    if let Some(lists) = PENDING_WORKSPACE.with(|p| p.borrow_mut().take()) {
        lists.apply_to(w);
    }
    let lists = WorkspaceLists::of(w);
    WORKSPACE_LISTS.with(|l| *l.borrow_mut() = lists);
}

#[cfg(test)]
thread_local! {
    /// The file the Preset Manager's Import / Export picker answers once, in
    /// place of the native dialog.
    pub(crate) static PICKED_PRESET_FILE_FOR_TEST: RefCell<Option<std::path::PathBuf>> =
        const { RefCell::new(None) };
}

fn pick_preset_file(save: bool) -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(path) = PICKED_PRESET_FILE_FOR_TEST.with(|p| p.borrow_mut().take()) {
        return Some(path);
    }
    let dialog = rfd::FileDialog::new().add_filter("JSON", &["json"]);
    if save {
        dialog.set_file_name("presets.json").save_file()
    } else {
        dialog.pick_file()
    }
}

/// The five dialogs, as the host holds them.
#[derive(Debug)]
pub enum GapDialog {
    PresetManager(Box<PresetManagerDialog>),
    Fade(FadeDialog),
    AutoAlign(AutoAlignDialog),
    AutoBlend(AutoBlendDialog),
    PerspectiveWarp(Box<PerspectiveWarpDialog>),
}

/// Whether `action` is one of this module's.
pub fn owns(action: MenuAction) -> bool {
    matches!(
        action,
        MenuAction::PresetManager
            | MenuAction::Fade
            | MenuAction::AutoAlignLayers
            | MenuAction::AutoBlendLayers
            | MenuAction::PerspectiveWarp
    )
}

/// The dialog `action` opens over the editor's current state, or `None`
/// when it cannot open (the menu's gate says why; [`perform`] repeats it).
pub fn open(action: MenuAction, editor: &Editor) -> Option<GapDialog> {
    match action {
        MenuAction::PresetManager => Some(GapDialog::PresetManager(Box::new(
            PresetManagerDialog::new(with_workspace_lists(library_of(editor.presets()))),
        ))),
        MenuAction::Fade => {
            crate::fade::fadeable(editor).map(|l| GapDialog::Fade(FadeDialog::new(l)))
        }
        MenuAction::AutoAlignLayers => (selected_layers(editor).len() >= 2)
            .then(|| GapDialog::AutoAlign(AutoAlignDialog::new())),
        MenuAction::AutoBlendLayers => (selected_layers(editor).len() >= 2)
            .then(|| GapDialog::AutoBlend(AutoBlendDialog::new())),
        MenuAction::PerspectiveWarp => crate::menu_bridge::warp_source(editor).map(|source| {
            GapDialog::PerspectiveWarp(Box::new(PerspectiveWarpDialog::new(&source)))
        }),
        _ => None,
    }
}

impl GapDialog {
    /// Draw one frame. A confirmation is parked and its action pushed onto
    /// `out.menu`; answers `true` when the dialog closed.
    pub fn drive(&mut self, ctx: &egui::Context, out: &mut ChromeOutput) -> bool {
        fn settle<T>(
            outcome: DialogOutcome<T>,
            park: impl FnOnce(T),
            action: MenuAction,
            out: &mut ChromeOutput,
        ) -> bool {
            match outcome {
                DialogOutcome::Open => false,
                DialogOutcome::Cancelled => true,
                DialogOutcome::Confirmed(spec) => {
                    park(spec);
                    out.menu.push(action);
                    true
                }
            }
        }
        match self {
            GapDialog::PresetManager(dialog) => {
                let outcome = dialog.show(ctx);
                if let Some(request) = dialog.take_file_request() {
                    serve_file_request(dialog, request);
                }
                settle(
                    outcome,
                    |lib| CONFIRMED_PRESETS.with(|s| *s.borrow_mut() = Some(lib)),
                    MenuAction::PresetManager,
                    out,
                )
            }
            GapDialog::Fade(dialog) => settle(
                dialog.show(ctx),
                |spec| CONFIRMED_FADE.with(|s| *s.borrow_mut() = Some(spec)),
                MenuAction::Fade,
                out,
            ),
            GapDialog::AutoAlign(dialog) => settle(
                dialog.show(ctx),
                |spec| CONFIRMED_ALIGN.with(|s| *s.borrow_mut() = Some(spec)),
                MenuAction::AutoAlignLayers,
                out,
            ),
            GapDialog::AutoBlend(dialog) => settle(
                dialog.show(ctx),
                |spec| CONFIRMED_BLEND.with(|s| *s.borrow_mut() = Some(spec)),
                MenuAction::AutoBlendLayers,
                out,
            ),
            GapDialog::PerspectiveWarp(dialog) => settle(
                dialog.show(ctx),
                |spec| CONFIRMED_WARP.with(|s| *s.borrow_mut() = Some(spec)),
                MenuAction::PerspectiveWarp,
                out,
            ),
        }
    }
}

/// Import or Export pressed in the Preset Manager: ask for the file, then
/// read it into the dialog or write the dialog's library to it.
fn serve_file_request(dialog: &mut PresetManagerDialog, request: PresetFileRequest) {
    match request {
        PresetFileRequest::Export => {
            let Some(path) = pick_preset_file(true) else {
                return;
            };
            let written = serde_json::to_string_pretty(dialog.library())
                .map_err(|e| e.to_string())
                .and_then(|json| std::fs::write(&path, json).map_err(|e| e.to_string()));
            dialog.set_note(match written {
                Ok(()) => format!("{} -> {}", dialog.library().len(), path.display()),
                Err(e) => e,
            });
        }
        PresetFileRequest::Import => {
            let Some(path) = pick_preset_file(false) else {
                return;
            };
            match read_library(&path) {
                Ok(library) => {
                    dialog.import(library);
                }
                Err(e) => dialog.set_note(e),
            }
        }
    }
}

/// A preset library file, checked entry by entry: a payload the store could
/// not rebuild is refused here, at import, not at confirm.
pub fn read_library(path: &std::path::Path) -> Result<PresetLibrary, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let library: PresetLibrary =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    store_from(&library, &PresetStore::new())?;
    WorkspaceLists::from_library(&library)?;
    Ok(library)
}

/// `library` with the workspace's swatches and tool presets (as the last
/// frame published them) filled in.
fn with_workspace_lists(mut library: PresetLibrary) -> PresetLibrary {
    let lists = WORKSPACE_LISTS.with(|l| l.borrow().clone());
    library.swatches = lists
        .swatches
        .iter()
        .map(|(name, rgba)| PresetEntry {
            name: name.clone(),
            data: serde_json::to_string(rgba).unwrap_or_default(),
        })
        .collect();
    library.tool_presets = lists
        .tool_presets
        .iter()
        .map(|p| PresetEntry {
            name: p.name.clone(),
            data: serde_json::to_string(p).unwrap_or_default(),
        })
        .collect();
    library
}

/// The store as the Preset Manager lists it: each entry's payload is the
/// store's own JSON for it.
pub fn library_of(store: &PresetStore) -> PresetLibrary {
    fn json<T: serde::Serialize>(value: &T) -> String {
        serde_json::to_string(value).unwrap_or_default()
    }
    let named = |list: &[(String, String)]| -> Vec<PresetEntry> {
        list.iter()
            .map(|(name, data)| PresetEntry {
                name: name.clone(),
                data: data.clone(),
            })
            .collect()
    };
    PresetLibrary {
        brushes: named(store.brushes()),
        gradients: store
            .gradients()
            .iter()
            .map(|g| PresetEntry {
                name: g.name.clone(),
                data: json(g),
            })
            .collect(),
        patterns: store
            .patterns()
            .iter()
            .map(|p| PresetEntry {
                name: p.name.clone(),
                data: json(p),
            })
            .collect(),
        styles: named(store.styles()),
        shapes: store
            .shapes()
            .iter()
            .map(|s| PresetEntry {
                name: s.name.clone(),
                data: json(s),
            })
            .collect(),
        // The workspace's kinds: see `with_workspace_lists`.
        swatches: Vec::new(),
        tool_presets: Vec::new(),
    }
}

/// Rebuild a store from an edited library, in the library's order and under
/// its names. The sampled brush tips are carried over from `old` (a brush
/// names its tip by hash; the pixels are not in the library).
pub fn store_from(library: &PresetLibrary, old: &PresetStore) -> Result<PresetStore, String> {
    let bad = |kind: &str, name: &str, e: serde_json::Error| format!("{kind} \"{name}\": {e}");
    let mut store = PresetStore::new();
    for e in &library.patterns {
        let mut p: PatternPreset =
            serde_json::from_str(&e.data).map_err(|err| bad("Pattern", &e.name, err))?;
        if p.rgba8.len() != p.width as usize * p.height as usize * 4 {
            return Err(format!(
                "Pattern \"{}\": its pixels do not match its size",
                e.name
            ));
        }
        p.name = e.name.clone();
        store.define_pattern(p);
    }
    for e in &library.styles {
        store.define_style(&e.name, e.data.clone());
    }
    for e in &library.brushes {
        store.define_brush(&e.name, e.data.clone());
    }
    for e in &library.gradients {
        let mut g: GradientResource =
            serde_json::from_str(&e.data).map_err(|err| bad("Gradient", &e.name, err))?;
        g.name = e.name.clone();
        store.define_gradient(g);
    }
    for e in &library.shapes {
        let mut s: ShapeResource =
            serde_json::from_str(&e.data).map_err(|err| bad("Shape", &e.name, err))?;
        s.name = e.name.clone();
        store.define_shape(s);
    }
    for tip in old.tips() {
        store.define_tip(tip.width, tip.height, tip.alpha8.clone());
    }
    Ok(store)
}

/// The document's selected layers, bottom of the stack first
/// (`iter_depth_first` walks the root list from its top, index 0).
fn selected_layers(editor: &Editor) -> Vec<LayerId> {
    let Some(open) = editor.active() else {
        return Vec::new();
    };
    let selected = open.document.layer_selection();
    let mut layers: Vec<LayerId> = open
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .filter(|id| selected.contains(id))
        .collect();
    layers.reverse();
    layers
}

/// One layer drawn alone into document space — through its transform, at
/// full opacity, Normal, with no mask, effects or clipping — as linear
/// premultiplied pixels over the canvas.
fn layer_in_document(
    open: &crate::doc::OpenDocument,
    layer: LayerId,
) -> Result<FilterBuffer, String> {
    let mut staged = open.document.clone();
    let mut keep = vec![layer];
    let mut parent = staged.layers.parent_of(layer);
    while let Some(p) = parent {
        keep.push(p);
        parent = staged.layers.parent_of(p);
    }
    for id in staged.layers.iter_depth_first() {
        if let Some(l) = staged.layers.get_mut(id) {
            if keep.contains(&id) {
                l.visible = true;
                l.opacity = 1.0;
                l.fill_opacity = 1.0;
                l.blend_mode = layer_model::BlendMode::Normal;
                l.mask = None;
                l.effects = layer_model::LayerEffects::default();
                l.clipping = layer_model::ClippingMode::None;
            } else {
                l.visible = false;
            }
        }
    }
    let rect = open.canvas_rect();
    let canvas = compositor::composite_region(
        &staged,
        &open.tiles,
        rect,
        0,
        compositor::CompositeOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    FilterBuffer::from_pixels(rect.width, rect.height, canvas.pixels().to_vec())
        .map_err(|e| e.to_string())
}

/// The selected pixel layers, bottom first, or why they cannot be used.
fn selected_pixel_layers(editor: &Editor, what: &str) -> Result<Vec<LayerId>, String> {
    let open = editor.active().ok_or("No document is open")?;
    let layers = selected_layers(editor);
    if layers.len() < 2 {
        return Err(format!(
            "{what} needs two or more layers selected in the Layers panel"
        ));
    }
    for id in &layers {
        let layer = open
            .document
            .layers
            .get(*id)
            .ok_or("A selected layer is gone")?;
        if !matches!(layer.kind, layer_model::LayerKind::Raster(_)) {
            return Err(format!(
                "{what} works on pixel layers; \"{}\" is a {}",
                layer.name,
                editor_core::layer_class_name(&layer.kind)
            ));
        }
    }
    Ok(layers)
}

/// Edit ▸ Auto-Align Layers: every selected layer but the bottom one (the
/// reference, which stays put) gets the transform that lays it over the
/// reference, estimated by `filters::align` on the two layers as drawn in
/// the document. One undo step.
pub fn auto_align_with(editor: &mut Editor, spec: &AutoAlignSpec) -> Result<String, String> {
    let label = "Auto-Align Layers";
    let layers = selected_pixel_layers(editor, label)?;
    let open = editor.active().ok_or("No document is open")?;
    let reference = layer_in_document(open, layers[0])?;
    let mut commands = Vec::new();
    for &id in &layers[1..] {
        let moving = layer_in_document(open, id)?;
        let name = open
            .document
            .layers
            .get(id)
            .map(|l| l.name.clone())
            .unwrap_or_default();
        let report = filters::align::estimate(&reference, &moving, spec.method)
            .map_err(|e| format!("{label}: \"{name}\": {e}"))?;
        let t = report.transform;
        let moved = t.tx.hypot(t.ty) > 0.05 || t.angle.abs() > 1e-4 || (t.scale - 1.0).abs() > 1e-4;
        if moved {
            commands.push(Command::TransformLayer {
                layer_id: id,
                matrix: t.to_cols(),
            });
        }
    }
    if commands.is_empty() {
        return Err(format!("{label}: the layers are already aligned"));
    }
    let n = commands.len();
    editor.apply_command(Command::Transaction {
        label: label.to_string(),
        commands,
    });
    Ok(format!(
        "{label}: {n} layer{} moved onto the bottom selected layer",
        if n == 1 { "" } else { "s" }
    ))
}

/// Edit ▸ Auto-Blend Layers: the selected layers, drawn in the document,
/// blended by `filters::blend_layers` into one new layer at the top of the
/// stack. One undo step; the source layers are left as they are.
pub fn auto_blend_with(editor: &mut Editor, spec: &AutoBlendSpec) -> Result<String, String> {
    let label = "Auto-Blend Layers";
    let layers = selected_pixel_layers(editor, label)?;
    let buffers = {
        let open = editor.active().ok_or("No document is open")?;
        layers
            .iter()
            .map(|id| layer_in_document(open, *id))
            .collect::<Result<Vec<_>, _>>()?
    };
    let blended = filters::blend_layers::auto_blend(&buffers, spec.method)
        .map_err(|e| format!("{label}: {e}"))?;
    let name = format!(
        "{} ({})",
        label,
        ui::dialogs::auto_align::blend_method_label(spec.method)
    );
    let layer = layer_model::Layer::raster(name.clone());
    let new_id = layer.id;
    let command = {
        let open = editor.active_mut().ok_or("No document is open")?;
        let paint = if open.is_sixteen_bit() {
            open.layer_rgba16_command(new_id, &blended.to_rgba16(), label)?
        } else {
            pixels::write_layer(open, new_id, &blended.to_rgba8(), label)?
        };
        Command::Transaction {
            label: label.to_string(),
            commands: vec![Command::create_layer(layer), paint],
        }
    };
    editor.apply_command(command);
    Ok(format!("{label}: \"{name}\" added"))
}

/// Edit ▸ Perspective Warp: the dialog's quads applied to the active layer
/// at full resolution, folded by the selection, as one undo step.
pub fn perspective_warp_with(
    editor: &mut Editor,
    spec: &PerspectiveWarpSpec,
) -> Result<String, String> {
    let name = "Perspective Warp";
    let doc = editor.active().ok_or("No document is open")?;
    if spec.image_size != (doc.document.width(), doc.document.height()) {
        return Err(format!(
            "The document changed size since {name} opened; open it again"
        ));
    }
    if spec.is_identity() {
        return Err(format!(
            "{name} moved nothing: draw a quad, then drag a corner in Warp mode"
        ));
    }
    crate::menu_bridge::edit_active_pixels(editor, name, |buffer, _| {
        *buffer = spec.apply(buffer);
        Ok(())
    })?;
    Ok(format!("{name} applied"))
}

/// Edit ▸ Preset Manager confirmed: the store is rebuilt from the edited
/// library and saved; an edit of the swatches or tool presets is queued for
/// their panels (the next frame's [`sync_workspace_presets`]).
pub fn presets_with(editor: &mut Editor, library: &PresetLibrary) -> Result<String, String> {
    let store = store_from(library, editor.presets())?;
    let lists = WorkspaceLists::from_library(library)?;
    let lists_changed = WORKSPACE_LISTS.with(|l| *l.borrow() != lists);
    let store_changed = &store != editor.presets();
    if !store_changed && !lists_changed {
        return Err("Preset Manager: nothing changed".to_string());
    }
    if lists_changed {
        WORKSPACE_LISTS.with(|l| *l.borrow_mut() = lists.clone());
        PENDING_WORKSPACE.with(|p| *p.borrow_mut() = Some(lists));
    }
    let n = library.len();
    if !store_changed {
        return Ok(format!("Preset Manager: {n} presets kept"));
    }
    *editor.presets_mut() = store;
    let saved = editor.presets().save(&editor.paths().presets_file());
    match saved {
        Ok(()) => Ok(format!("Preset Manager: {n} presets kept")),
        Err(e) => Err(format!(
            "Preset Manager: the presets changed but could not be saved: {e}"
        )),
    }
}

/// The document half of the menu context, with Fade's step, for the
/// refusal sentences: the same gate the menu row drew.
fn gate_context(editor: &Editor) -> ui::MenuContext {
    match editor.active() {
        Some(open) => {
            let mut ctx = ui::MenuContext::from_document(&open.document, &open.history);
            ctx.fade_step = crate::fade::fadeable(editor);
            ctx
        }
        None => ui::MenuContext::default(),
    }
}

/// [`crate::menu_bridge::perform`]'s arm for this module's actions: apply
/// the parked confirmation, or say why there is none.
pub fn perform(action: MenuAction, editor: &mut Editor) -> Result<String, String> {
    let refused = |editor: &Editor| -> String {
        match action.resolve(&gate_context(editor)) {
            ui::Resolution::Disabled(reason) => reason.to_string(),
            ui::Resolution::Enabled(_) => {
                format!("{}: its dialog was not confirmed", action.label())
            }
        }
    };
    match action {
        MenuAction::PresetManager => match CONFIRMED_PRESETS.with(|s| s.borrow_mut().take()) {
            Some(library) => presets_with(editor, &library),
            None => Err(refused(editor)),
        },
        MenuAction::Fade => match CONFIRMED_FADE.with(|s| s.borrow_mut().take()) {
            Some(spec) => crate::fade::fade_with(editor, &spec),
            None => Err(refused(editor)),
        },
        MenuAction::AutoAlignLayers => match CONFIRMED_ALIGN.with(|s| s.borrow_mut().take()) {
            Some(spec) => auto_align_with(editor, &spec),
            None => Err(refused(editor)),
        },
        MenuAction::AutoBlendLayers => match CONFIRMED_BLEND.with(|s| s.borrow_mut().take()) {
            Some(spec) => auto_blend_with(editor, &spec),
            None => Err(refused(editor)),
        },
        MenuAction::PerspectiveWarp => match CONFIRMED_WARP.with(|s| s.borrow_mut().take()) {
            Some(spec) => perspective_warp_with(editor, &spec),
            None => Err(refused(editor)),
        },
        other => Err(format!("{} is not an Edit-gap action", other.label())),
    }
}

#[cfg(test)]
#[path = "edit_gaps_tests.rs"]
mod tests;
