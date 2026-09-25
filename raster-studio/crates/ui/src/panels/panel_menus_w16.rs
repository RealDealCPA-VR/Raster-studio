//! W16-E: the panel menus Photopea keeps on its preset panels, the History
//! panel and the Channels panel, and the requests only the application can
//! carry out.
//!
//! # The preset menus (Swatches, Brushes, Styles)
//!
//! Photopea's preset picker menu is, in its order: **Open .ACO/.ABR/.ASL**,
//! **Export as .ACO/.ABR/.ASL**, **Name Change**, **Delete**, **Tiles/List**,
//! **Define New** (and **New Folder**, which this build does not have: the
//! three preset lists here are flat). The rows are drawn under the move
//! controls the panel header's overflow button reveals, the way the Channels
//! panel's own menu is ([`panel_menu`]).
//!
//! Name Change, Delete and Export act on the item last clicked in the panel
//! ([`selected`]); with nothing selected Export writes the whole list. The
//! Swatches and Brushes lists live in the workspace, so their renames and
//! deletes happen here; the style presets live in the application's preset
//! store, so those are [`PanelRequest`]s.
//!
//! # Requests
//!
//! A file picker, the preset store, the history and the document camera are
//! the application's. A panel posts a [`PanelRequest`] with [`post`]; the
//! application takes the queue once a frame ([`take_requests`], called while
//! it builds the menu context) and says what happened on the status line.
//! The queue is per thread: the UI thread is the one that draws the panels
//! and builds the menu context.

use std::cell::RefCell;

use design::{current_tokens, Space};
use editor_core::{Command, Document, History};
use egui::Ui;

use crate::dock::PanelId;
use crate::intent::Intent;
use crate::menu::MenuAction;
use crate::panels::channels::ChannelKind;
use crate::strings::tr;
use crate::view::{hairline, labelled_button, text_field_sized};
use crate::Workspace;

/// A preset list with a Photopea panel menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Library {
    Swatches,
    Brushes,
    Styles,
}

impl Library {
    pub const ALL: [Library; 3] = [Library::Swatches, Library::Brushes, Library::Styles];

    /// The library a panel shows, if it is one of the three.
    pub fn of(panel: PanelId) -> Option<Self> {
        match panel {
            PanelId::Swatches => Some(Self::Swatches),
            PanelId::Brushes => Some(Self::Brushes),
            PanelId::Styles => Some(Self::Styles),
            _ => None,
        }
    }

    /// The panel that shows this library.
    pub fn panel(self) -> PanelId {
        match self {
            Self::Swatches => PanelId::Swatches,
            Self::Brushes => PanelId::Brushes,
            Self::Styles => PanelId::Styles,
        }
    }

    /// The file extension Photopea opens and exports this library as.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Swatches => "aco",
            Self::Brushes => "abr",
            Self::Styles => "asl",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Swatches => "swatches",
            Self::Brushes => "brushes",
            Self::Styles => "styles",
        }
    }

    fn open_label(self) -> &'static str {
        tr(match self {
            Self::Swatches => "ui.w16.menu.open.aco",
            Self::Brushes => "ui.w16.menu.open.abr",
            Self::Styles => "ui.w16.menu.open.asl",
        })
    }

    fn export_label(self) -> &'static str {
        tr(match self {
            Self::Swatches => "ui.w16.menu.export.aco",
            Self::Brushes => "ui.w16.menu.export.abr",
            Self::Styles => "ui.w16.menu.export.asl",
        })
    }
}

/// Work a panel hands to the application.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelRequest {
    /// Open .ACO / .ABR / .ASL: the application's open picker, whose file
    /// lands in the library by its extension (File > Open's own route).
    OpenLibrary(Library),
    /// Export as .ACO: these swatches, in order.
    ExportSwatches(Vec<(String, [f32; 4])>),
    /// Export as .ABR: these brushes, in order.
    ExportBrushes(Vec<(String, tools::BrushSettings)>),
    /// Export as .ASL: style preset `Some(index)`, or every one.
    ExportStyles(Option<usize>),
    /// The Styles panel's Name Change.
    RenameStyle { index: usize, name: String },
    /// The Styles panel's Delete.
    DeleteStyle(usize),
    /// Channels > New: an empty (all black) alpha channel.
    NewAlphaChannel,
    /// Channels > Delete on alpha channel `index` (a saved selection).
    DeleteAlphaChannel(usize),
    /// Load a colour channel as the selection: the composite's luminosity,
    /// or one component's values. Needs the pixels, which are the
    /// application's.
    LoadChannelSelection(ChannelKind),
    /// History > Clear History.
    ClearHistory,
    /// Navigator > Angle, in degrees.
    SetViewAngle(f32),
}

thread_local! {
    static QUEUE: RefCell<Vec<PanelRequest>> = const { RefCell::new(Vec::new()) };
}

/// Queue `request` for the application.
pub fn post(request: PanelRequest) {
    QUEUE.with(|q| q.borrow_mut().push(request));
}

/// Take every queued request, oldest first.
pub fn take_requests() -> Vec<PanelRequest> {
    QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Tiles or List, per preset panel (Photopea's Tiles/List row).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Tiles,
    List,
}

fn mode_key(library: Library) -> egui::Id {
    egui::Id::new(("raster-w16-view-mode", library.key()))
}

fn selected_key(library: Library) -> egui::Id {
    egui::Id::new(("raster-w16-selected", library.key()))
}

fn rename_key(library: Library) -> egui::Id {
    egui::Id::new(("raster-w16-renaming", library.key()))
}

fn angle_key() -> egui::Id {
    egui::Id::new("raster-w16-view-angle")
}

/// How `library`'s panel lays its items out.
pub fn view_mode(ctx: &egui::Context, library: Library) -> ViewMode {
    ctx.data(|d| d.get_temp::<ViewMode>(mode_key(library)))
        .unwrap_or_default()
}

/// Set how `library`'s panel lays its items out.
pub fn set_view_mode(ctx: &egui::Context, library: Library, mode: ViewMode) {
    ctx.data_mut(|d| d.insert_temp(mode_key(library), mode));
}

/// The item of `library` last clicked, if any.
pub fn selected(ctx: &egui::Context, library: Library) -> Option<usize> {
    ctx.data(|d| d.get_temp::<Option<usize>>(selected_key(library)))
        .flatten()
}

/// Mark item `index` of `library` as the one the menu acts on.
pub fn set_selected(ctx: &egui::Context, library: Library, index: Option<usize>) {
    ctx.data_mut(|d| d.insert_temp(selected_key(library), index));
}

/// Publish the document camera's angle in degrees for the Navigator.
pub fn publish_view_angle(ctx: &egui::Context, degrees: f32) {
    ctx.data_mut(|d| d.insert_temp(angle_key(), degrees));
}

/// What [`publish_view_angle`] last published (upright before it has).
pub fn published_view_angle(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(angle_key())).unwrap_or(0.0)
}

/// Stable ids for a headless test.
pub mod ids {
    use super::Library;
    use crate::dock::PanelId;

    /// Row `key` of `panel`'s menu.
    pub fn menu_row(panel: PanelId, key: &str) -> egui::Id {
        egui::Id::new(("raster-w16-menu", panel.key(), key))
    }
    /// The Name Change field of `library`'s menu.
    pub fn rename_field(library: Library) -> egui::Id {
        egui::Id::new(("raster-w16-rename", library.key()))
    }
    /// Item `index` of `library` drawn as a List row.
    pub fn list_row(library: Library, index: usize) -> egui::Id {
        egui::Id::new(("raster-w16-list-row", library.key(), index))
    }
    /// Swatch `index` drawn as a tile.
    pub fn swatch_tile(index: usize) -> egui::Id {
        egui::Id::new(("raster-w16-swatch-tile", index))
    }
    /// Comp `index`'s flag toggle: 0 visibility, 1 position, 2 appearance.
    pub fn comp_flag(index: usize, flag: usize) -> egui::Id {
        egui::Id::new(("raster-w16-comp-flag", index, flag))
    }
    /// The Layer Comps panel's Last Document State row.
    pub fn last_state_row() -> egui::Id {
        egui::Id::new("raster-w16-last-document-state")
    }
    /// The author field of the note with document id `note`.
    pub fn note_author(note: u64) -> egui::Id {
        egui::Id::new(("raster-w16-note-author", note))
    }
    /// The Navigator's angle field.
    pub fn navigator_angle() -> egui::Id {
        egui::Id::new("raster-w16-navigator-angle")
    }
    /// Spot channel `index`'s row.
    pub fn spot_row(index: usize) -> egui::Id {
        egui::Id::new(("raster-w16-spot-row", index))
    }
    /// Spot channel `index`'s options button.
    pub fn spot_edit(index: usize) -> egui::Id {
        egui::Id::new(("raster-w16-spot-edit", index))
    }
    /// Spot channel `index`'s delete button.
    pub fn spot_delete(index: usize) -> egui::Id {
        egui::Id::new(("raster-w16-spot-delete", index))
    }
    /// Row `key` of layer `layer`'s vector-mask popup.
    pub fn vector_mask_item(layer: layer_model::LayerId, key: &str) -> egui::Id {
        egui::Id::new(("raster-w16-vector-mask-item", layer, key))
    }
    /// Row `key` of layer `layer`'s raster-mask popup (Delete / Apply).
    pub fn mask_item(layer: layer_model::LayerId, key: &str) -> egui::Id {
        egui::Id::new(("raster-w16-mask-item", layer, key))
    }
}

/// The names of `library`'s items, in panel order.
fn names(w: &Workspace, ctx: &egui::Context, library: Library) -> Vec<String> {
    match library {
        Library::Swatches => w
            .swatches
            .swatches()
            .iter()
            .map(|s| s.name.clone())
            .collect(),
        Library::Brushes => w.brushes.presets().iter().map(|p| p.name.clone()).collect(),
        Library::Styles => crate::panels::styles::StylesView::published(ctx)
            .styles
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
    }
}

/// Draw `panel`'s W16-E menu rows, if it has any. Called under the move
/// controls while the panel's overflow menu is open.
pub(crate) fn panel_menu(
    w: &mut Workspace,
    ui: &mut Ui,
    doc: &Document,
    history: &History,
    panel: PanelId,
) {
    if let Some(library) = Library::of(panel) {
        preset_menu(w, ui, doc, library);
    } else if panel == PanelId::History {
        history_menu(w, ui, doc, history);
    }
}

/// One menu row: a labelled button under a stable id, with the reason on
/// hover when it is off.
fn row(ui: &mut Ui, panel: PanelId, key: &str, label: &str, reason: Option<&str>) -> bool {
    let response = labelled_button(ui, label, reason.is_none(), ids::menu_row(panel, key));
    match reason {
        Some(why) => {
            response.on_hover_text(why);
            false
        }
        None => response.clicked(),
    }
}

/// Rename swatch `index` in place: the palette has no rename of its own, so
/// the swatch is taken out and put back under the new name where it was.
pub fn rename_swatch(w: &mut Workspace, index: usize, name: &str) -> bool {
    let name = name.trim();
    let Some(old) = w.swatches.get(index).cloned() else {
        return false;
    };
    if name.is_empty() || old.name == name {
        return false;
    }
    w.swatches.remove(index);
    if !w.swatches.add(name, old.rgba) {
        // Cannot happen (the colour was just taken out), but never lose it.
        w.swatches.add(old.name, old.rgba);
        return false;
    }
    let last = w.swatches.len() - 1;
    w.swatches.reorder(last, index);
    true
}

fn preset_menu(w: &mut Workspace, ui: &mut Ui, doc: &Document, library: Library) {
    let ctx = ui.ctx().clone();
    let panel = library.panel();
    let names = names(w, &ctx, library);
    let chosen = selected(&ctx, library).filter(|i| *i < names.len());
    let none_selected = tr("ui.w16.menu.nothing.selected");
    let empty = tr("ui.w16.menu.library.empty");
    let can_define: Option<&str> = match library {
        Library::Styles => {
            let styled = doc
                .active_layer()
                .and_then(|id| doc.layers.get(id))
                .is_some_and(|l| !l.effects.is_default());
            (!styled).then(|| tr("ui.w16.menu.styles.no.style"))
        }
        _ => None,
    };
    let mut close = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = Space::Hair.pt();
        if row(ui, panel, "open", library.open_label(), None) {
            post(PanelRequest::OpenLibrary(library));
            close = true;
        }
        let export_reason = names.is_empty().then_some(empty);
        if row(ui, panel, "export", library.export_label(), export_reason) {
            post(match library {
                Library::Swatches => PanelRequest::ExportSwatches(
                    w.swatches
                        .swatches()
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| chosen.is_none_or(|c| c == *i))
                        .map(|(_, s)| (s.name.clone(), s.rgba))
                        .collect(),
                ),
                Library::Brushes => PanelRequest::ExportBrushes(
                    w.brushes
                        .presets()
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| chosen.is_none_or(|c| c == *i))
                        .map(|(_, p)| (p.name.clone(), p.settings))
                        .collect(),
                ),
                Library::Styles => PanelRequest::ExportStyles(chosen),
            });
            close = true;
        }
        let on_item = chosen.is_none().then_some(none_selected);
        if row(ui, panel, "rename", tr("ui.w16.menu.rename"), on_item) {
            ctx.data_mut(|d| d.insert_temp(rename_key(library), chosen));
        }
        if row(ui, panel, "delete", tr("ui.w16.menu.delete"), on_item) {
            if let Some(index) = chosen {
                match library {
                    Library::Swatches => {
                        w.swatches.remove(index);
                    }
                    Library::Brushes => {
                        w.brushes.remove(index);
                    }
                    Library::Styles => post(PanelRequest::DeleteStyle(index)),
                }
                set_selected(&ctx, library, None);
            }
            close = true;
        }
        if row(ui, panel, "tiles-list", tr("ui.w16.menu.tiles.list"), None) {
            let next = match view_mode(&ctx, library) {
                ViewMode::Tiles => ViewMode::List,
                ViewMode::List => ViewMode::Tiles,
            };
            set_view_mode(&ctx, library, next);
        }
        if row(
            ui,
            panel,
            "define-new",
            tr("ui.w16.menu.define.new"),
            can_define,
        ) {
            match library {
                Library::Swatches => {
                    let rgba = w.color.current();
                    let name = crate::panels::color::format_hex(rgba);
                    w.swatches.add(name, rgba);
                }
                Library::Brushes => {
                    let tool = w.palette.active();
                    let name = format!("Brush {}", w.brushes.len() + 1);
                    w.brushes.capture(&name, &w.options, tool);
                }
                Library::Styles => w.emit(Intent::Action(MenuAction::DefineStylePreset)),
            }
            close = true;
        }
    });
    // Name Change: a field under the rows, on the item it was opened for.
    let renaming = ctx
        .data(|d| d.get_temp::<Option<usize>>(rename_key(library)))
        .flatten()
        .filter(|i| *i < names.len());
    if let Some(index) = renaming {
        let t = current_tokens(ui);
        let width = ui.available_width().max(t.metrics.min_hit_target);
        let field = text_field_sized(ui, ids::rename_field(library), &names[index], width);
        if let Some(name) = field.committed {
            match library {
                Library::Swatches => {
                    rename_swatch(w, index, &name);
                }
                Library::Brushes => {
                    w.brushes.rename(index, &name);
                }
                Library::Styles => {
                    let name = name.trim().to_string();
                    if !name.is_empty() && name != names[index] {
                        post(PanelRequest::RenameStyle { index, name });
                    }
                }
            }
            close = true;
        }
    }
    hairline(ui);
    if close {
        ctx.data_mut(|d| d.remove::<Option<usize>>(rename_key(library)));
        w.panel_menu = None;
    }
}

fn history_menu(w: &mut Workspace, ui: &mut Ui, doc: &Document, history: &History) {
    let has_document = doc.width() > 0 && doc.height() > 0;
    let steps = history.undo_depth() + history.redo_depth();
    let mut close = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = Space::Hair.pt();
        let clear_reason = if !has_document {
            Some(tr("ui.w16.history.no.document"))
        } else if steps == 0 {
            Some(tr("ui.w16.history.nothing.to.clear"))
        } else {
            None
        };
        if row(
            ui,
            PanelId::History,
            "clear",
            tr("ui.w16.history.clear"),
            clear_reason,
        ) {
            post(PanelRequest::ClearHistory);
            // The snapshots name rows of the history being cleared.
            w.snapshots.clear();
            close = true;
        }
        let snapshot_reason = (!has_document).then(|| tr("ui.w16.history.no.document"));
        if row(
            ui,
            PanelId::History,
            "snapshot",
            tr("ui.w16.history.new.snapshot"),
            snapshot_reason,
        ) {
            let index = crate::panels::history::HistoryModel::new(history).current();
            w.snapshots.push(crate::panels::history::Snapshot {
                name: format!("Snapshot {}", w.snapshots.len() + 1),
                index,
            });
            close = true;
        }
    });
    hairline(ui);
    if close {
        w.panel_menu = None;
    }
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// The channel the Channels panel's Load / Delete act on: Photopea's
/// "current channel".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChannelTarget {
    /// The composite, a colour component, or a layer mask.
    Color(ChannelKind),
    /// Alpha channel `index` (a saved selection), open for editing.
    Alpha(usize),
    /// Spot channel `index`, clicked in the list.
    Spot(usize),
}

fn spot_key() -> egui::Id {
    egui::Id::new("raster-w16-spot-picked")
}

fn spot_edit_key() -> egui::Id {
    egui::Id::new("raster-w16-spot-editing")
}

/// Make spot channel `index` (or none) the current channel.
pub fn pick_spot(ctx: &egui::Context, index: Option<usize>) {
    ctx.data_mut(|d| d.insert_temp(spot_key(), index));
}

/// The spot channel picked in the list, if any.
pub fn picked_spot(ctx: &egui::Context) -> Option<usize> {
    ctx.data(|d| d.get_temp::<Option<usize>>(spot_key()))
        .flatten()
}

/// Remember which spot channel the open Spot Channel dialog edits (`None`:
/// the dialog makes a new one).
pub fn set_spot_editing(ctx: &egui::Context, index: Option<usize>) {
    ctx.data_mut(|d| d.insert_temp(spot_edit_key(), index));
}

/// The spot channel the open Spot Channel dialog edits, if it edits one.
pub fn spot_editing(ctx: &egui::Context) -> Option<usize> {
    ctx.data(|d| d.get_temp::<Option<usize>>(spot_edit_key()))
        .flatten()
}

/// The command that gives spot channel `index` a new name, ink and
/// solidity, keeping its coverage. `None` when there is no such channel.
pub fn edit_spot_command(
    doc: &Document,
    index: usize,
    name: &str,
    ink: [u8; 3],
    solidity: u8,
) -> Option<Command> {
    let mut channels = doc.spot_channels.clone();
    let channel = channels.get_mut(index)?;
    channel.name = name.to_string();
    channel.ink = ink;
    channel.solidity = solidity.min(100);
    Some(Command::SetSpotChannels { channels })
}

/// The current channel: a picked spot channel, else the alpha channel open
/// for editing, else the colour or mask row selected.
pub fn current_channel(ctx: &egui::Context, w: &Workspace, doc: &Document) -> ChannelTarget {
    if let Some(i) = picked_spot(ctx).filter(|i| *i < doc.spot_channels.len()) {
        return ChannelTarget::Spot(i);
    }
    if let Some(edit) = doc.extras.alpha_edit {
        if edit.index < doc.saved_selections.len() {
            return ChannelTarget::Alpha(edit.index);
        }
    }
    ChannelTarget::Color(w.channels.selected)
}

/// What a channel control does: an intent through the usual road, or a
/// request only the application can carry out.
#[derive(Clone, Debug, PartialEq)]
pub enum ChannelRoute {
    Intent(Intent),
    Request(PanelRequest),
}

impl ChannelRoute {
    /// Carry the route out.
    pub fn fire(self, w: &mut Workspace) {
        match self {
            Self::Intent(intent) => w.emit(intent),
            Self::Request(request) => post(request),
        }
    }
}

fn no_document(doc: &Document) -> bool {
    doc.width() == 0 || doc.height() == 0
}

/// Load `target` as the selection (the footer's first button, and a
/// Ctrl+click on a channel row), or the reason it cannot be.
pub fn load_route(target: ChannelTarget, doc: &Document) -> Result<ChannelRoute, &'static str> {
    if no_document(doc) {
        return Err(tr("ui.docks.channels.no.document"));
    }
    match target {
        ChannelTarget::Color(ChannelKind::Mask { layer, .. }) => Ok(ChannelRoute::Intent(
            Intent::Action(MenuAction::SelectLayerPixels {
                layer: Some(layer),
                mask: true,
                op: crate::dialogs::LoadOperation::New,
            }),
        )),
        ChannelTarget::Color(kind) => Ok(ChannelRoute::Request(
            PanelRequest::LoadChannelSelection(kind),
        )),
        ChannelTarget::Alpha(index) => doc
            .saved_selections
            .get(index)
            .map(|(_, selection)| {
                ChannelRoute::Intent(Intent::Document(Command::SetSelection {
                    selection: selection.clone(),
                }))
            })
            .ok_or(tr("ui.w16.channels.gone")),
        ChannelTarget::Spot(index) => doc
            .spot_channels
            .get(index)
            .map(|spot| {
                ChannelRoute::Intent(Intent::Document(Command::SetSelection {
                    selection: spot.coverage.clone(),
                }))
            })
            .ok_or(tr("ui.w16.channels.gone")),
    }
}

/// Channels > New: an empty alpha channel.
pub fn new_route(doc: &Document) -> Result<ChannelRoute, &'static str> {
    if no_document(doc) {
        return Err(tr("ui.docks.channels.no.document"));
    }
    Ok(ChannelRoute::Request(PanelRequest::NewAlphaChannel))
}

/// Channels > Delete on `target`, or the reason it cannot be deleted.
pub fn delete_route(target: ChannelTarget, doc: &Document) -> Result<ChannelRoute, &'static str> {
    if no_document(doc) {
        return Err(tr("ui.docks.channels.no.document"));
    }
    match target {
        ChannelTarget::Color(ChannelKind::Mask { .. }) => Ok(ChannelRoute::Intent(Intent::Action(
            MenuAction::Mask(crate::menu::MaskOp::Delete),
        ))),
        ChannelTarget::Color(_) => Err(tr("ui.w16.channels.color.not.deletable")),
        ChannelTarget::Alpha(index) => Ok(ChannelRoute::Request(PanelRequest::DeleteAlphaChannel(
            index,
        ))),
        ChannelTarget::Spot(index) => {
            if index >= doc.spot_channels.len() {
                return Err(tr("ui.w16.channels.gone"));
            }
            let mut channels = doc.spot_channels.clone();
            channels.remove(index);
            Ok(ChannelRoute::Intent(Intent::Document(
                Command::SetSpotChannels { channels },
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_hands_every_request_over_once_in_order() {
        assert!(take_requests().is_empty());
        post(PanelRequest::ClearHistory);
        post(PanelRequest::NewAlphaChannel);
        assert_eq!(
            take_requests(),
            vec![PanelRequest::ClearHistory, PanelRequest::NewAlphaChannel]
        );
        assert!(take_requests().is_empty());
    }

    #[test]
    fn every_library_names_its_photopea_extension() {
        assert_eq!(Library::Swatches.extension(), "aco");
        assert_eq!(Library::Brushes.extension(), "abr");
        assert_eq!(Library::Styles.extension(), "asl");
        for library in Library::ALL {
            assert_eq!(Library::of(library.panel()), Some(library));
            assert!(!library.open_label().is_empty());
            assert!(!library.export_label().is_empty());
        }
    }
}

#[cfg(test)]
#[path = "w16e_panel_tests.rs"]
mod w16e_panel_tests;
