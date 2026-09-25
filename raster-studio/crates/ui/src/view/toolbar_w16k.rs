//! W16-K: the options-bar rows Photopea's bars carry that ours lacked.
//!
//! * Hand and Zoom: Fit on Screen and 100% (the View menu's own rows);
//!   Rotate View: Reset (View > Reset View Rotation).
//! * Every selection tool: Refine Edge; the wand group (Object Selection,
//!   Magic Wand, Quick Selection): Select Subject first.
//! * Type and Vertical Type: Warp (Layer > Text > Warp Text...) and Convert
//!   (Layer > Text > Convert to Shape).
//! * Every brush-driven tool: a brush-preset picker (the Brushes panel's
//!   presets, applied exactly as a click in the panel applies one).
//! * Crop: Photopea's "..." Crop by list - All Layers (Image > Reveal All),
//!   Current Layer, Trim, Selection (Image > Crop to Selection).
//! * Artboard: the + buttons, a new artboard of the active one's size on
//!   each side of it.
//!
//! Every button raises a menu action ([`Intent::Action`]), so the bar and the
//! menu run one route; the captions are the actions' own labels.

use super::*;
use crate::menu::{ArtboardSide, WarpTextItem, ZoomCommand};

/// Pseudo-keys the buttons are marked under
/// (`ids::tool_option(tool, key)`).
pub const FIT_KEY: &str = "w16k_fit";
pub const PIXEL_KEY: &str = "w16k_pixel_to_pixel";
pub const ROTATE_RESET_KEY: &str = "w16k_rotate_reset";
pub const REFINE_EDGE_KEY: &str = "w16k_refine_edge";
pub const SELECT_SUBJECT_KEY: &str = "w16k_select_subject";
pub const TYPE_WARP_KEY: &str = "w16k_type_warp";
pub const TYPE_CONVERT_KEY: &str = "w16k_type_convert";
pub const BRUSH_PICKER_KEY: &str = "w16k_brush_picker";
pub const CROP_BY_KEY: &str = "w16k_crop_by";
pub const ARTBOARD_ADD_KEYS: [(ArtboardSide, &str); 4] = [
    (ArtboardSide::Left, "w16k_artboard_left"),
    (ArtboardSide::Right, "w16k_artboard_right"),
    (ArtboardSide::Above, "w16k_artboard_above"),
    (ArtboardSide::Below, "w16k_artboard_below"),
];

/// Crop by's rows, in Photopea's order (All Layers, Current Layer, Trim,
/// Selection), as the menu actions they run.
pub const CROP_BY: [MenuAction; 4] = [
    MenuAction::RevealAll,
    MenuAction::CropToLayer,
    MenuAction::Trim,
    MenuAction::CropToSelection,
];

/// The selection tools (Refine Edge on each bar).
pub const SELECTION_TOOLS: [ToolId; 10] = [
    ToolId::RectMarquee,
    ToolId::EllipseMarquee,
    ToolId::SingleRowMarquee,
    ToolId::SingleColumnMarquee,
    ToolId::Lasso,
    ToolId::PolygonalLasso,
    ToolId::MagneticLasso,
    ToolId::MagicWand,
    ToolId::QuickSelect,
    ToolId::ObjectSelection,
];

/// The wand group, whose bars lead with Select Subject.
pub const SUBJECT_TOOLS: [ToolId; 4] = [
    ToolId::SingleRowMarquee,
    ToolId::MagicWand,
    ToolId::QuickSelect,
    ToolId::ObjectSelection,
];

/// The id of the brush picker's row `index`.
pub fn brush_preset_id(tool: ToolId, index: usize) -> egui::Id {
    super::super::ids::tool_option(tool, BRUSH_PICKER_KEY).with(index)
}

/// The id of the Crop by list's row for `action`.
pub fn crop_by_id(action: MenuAction) -> egui::Id {
    super::super::ids::tool_option(ToolId::Crop, CROP_BY_KEY).with(action)
}

fn action_button(
    w: &mut Workspace,
    ui: &mut Ui,
    tool: ToolId,
    action: MenuAction,
    key: &'static str,
) {
    let response = super::super::labelled_button(
        ui,
        &action.label(),
        true,
        super::super::ids::tool_option(tool, key),
    );
    if response.clicked() {
        w.emit(Intent::Action(action));
    }
}

/// Hand, Zoom and Rotate View have no settings, only these buttons; `true`
/// when `tool` is one of them (the bar then draws nothing else).
pub(super) fn view_row(w: &mut Workspace, ui: &mut Ui, tool: ToolId) -> bool {
    match tool {
        ToolId::Hand | ToolId::Zoom => {
            action_button(
                w,
                ui,
                tool,
                MenuAction::Zoom(ZoomCommand::FitOnScreen),
                FIT_KEY,
            );
            action_button(
                w,
                ui,
                tool,
                MenuAction::Zoom(ZoomCommand::ActualPixels),
                PIXEL_KEY,
            );
            true
        }
        ToolId::RotateView => {
            action_button(w, ui, tool, MenuAction::ResetViewRotation, ROTATE_RESET_KEY);
            true
        }
        _ => false,
    }
}

/// Whether `tool` paints with a brush (so its bar offers the presets).
fn brush_driven(tool: ToolId) -> bool {
    thread_local! {
        static SEEN: std::cell::RefCell<Vec<(ToolId, bool)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    SEEN.with(|seen| {
        if let Some((_, b)) = seen.borrow().iter().find(|(t, _)| *t == tool) {
            return *b;
        }
        let b = tools::registry::make(tool).brush().is_some();
        seen.borrow_mut().push((tool, b));
        b
    })
}

/// The rows after the tool's own options.
pub(super) fn trailing_row(w: &mut Workspace, ui: &mut Ui, tool: ToolId) {
    if SELECTION_TOOLS.contains(&tool) {
        separator(ui);
        if SUBJECT_TOOLS.contains(&tool) {
            action_button(w, ui, tool, MenuAction::SelectSubject, SELECT_SUBJECT_KEY);
        }
        action_button(w, ui, tool, MenuAction::RefineEdge, REFINE_EDGE_KEY);
    }
    if matches!(tool, ToolId::Type | ToolId::VerticalType) {
        separator(ui);
        action_button(
            w,
            ui,
            tool,
            MenuAction::WarpText(WarpTextItem::Dialog),
            TYPE_WARP_KEY,
        );
        action_button(
            w,
            ui,
            tool,
            MenuAction::ConvertTextToShape,
            TYPE_CONVERT_KEY,
        );
    }
    if brush_driven(tool) && !w.brushes.is_empty() {
        separator(ui);
        brush_picker(w, ui, tool);
    }
    if tool == ToolId::Crop {
        separator(ui);
        crop_by(w, ui);
    }
    if tool == ToolId::Artboard {
        separator(ui);
        for (side, key) in ARTBOARD_ADD_KEYS {
            action_button(w, ui, tool, MenuAction::ArtboardNeighbour(side), key);
        }
    }
}

/// The brush icon opens the presets; a row applies its preset the way the
/// Brushes panel's list does.
fn brush_picker(w: &mut Workspace, ui: &mut Ui, tool: ToolId) {
    let opener_id = super::super::ids::tool_option(tool, BRUSH_PICKER_KEY);
    let opener = super::super::icon_button_id(ui, "brush", true, opener_id);
    let popup = opener_id.with("popup");
    if opener.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    let names: Vec<String> = w.brushes.presets().iter().map(|p| p.name.clone()).collect();
    let mut picked = None;
    egui::popup_below_widget(
        ui,
        popup,
        &opener,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            egui::ScrollArea::vertical()
                .max_height(12.0 * ui.spacing().interact_size.y)
                .show(ui, |ui| {
                    for (i, name) in names.iter().enumerate() {
                        let row =
                            super::super::labelled_button(ui, name, true, brush_preset_id(tool, i));
                        if row.clicked() {
                            picked = Some(i);
                        }
                    }
                });
        },
    );
    if let Some(i) = picked {
        ui.memory_mut(|m| m.close_popup());
        let writes = w.brushes.apply(i, &mut w.options, tool);
        for (key, value) in writes {
            w.emit(Intent::SetToolOption { tool, key, value });
        }
    }
}

/// Photopea's "..." Crop by list.
fn crop_by(w: &mut Workspace, ui: &mut Ui) {
    let opener_id = super::super::ids::tool_option(ToolId::Crop, CROP_BY_KEY);
    let opener = super::super::icon_button_id(ui, "overflow", true, opener_id);
    let popup = opener_id.with("popup");
    if opener.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    let mut picked = None;
    egui::popup_below_widget(
        ui,
        popup,
        &opener,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            for action in CROP_BY {
                let row =
                    super::super::labelled_button(ui, &action.label(), true, crop_by_id(action));
                if row.clicked() {
                    picked = Some(action);
                }
            }
        },
    );
    if let Some(action) = picked {
        ui.memory_mut(|m| m.close_popup());
        w.emit(Intent::Action(action));
    }
}
