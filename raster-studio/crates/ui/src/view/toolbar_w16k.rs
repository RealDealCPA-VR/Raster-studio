//! W16-K: the options-bar rows Photopea's bars carry that ours lacked.
//!
//! * Zoom: Photopea's Pixel to Pixel (View > 100%) and Fit The Area (View >
//!   Fit on Screen); Rotate View: Photopea's Angle field (the document
//!   camera's rotation, -180 to 180 degrees, the Navigator's own route) and
//!   Reset (View > Reset View Rotation). Not built (see the parity matrix):
//!   the Zoom bar's Zoom In / Zoom Out toggle (a zoom click is stepped by the
//!   canvas router, `canvas::input`, which reads only Alt) and the All
//!   Documents box on the Zoom and Hand bars, so the Hand bar has no row.
//! * Every selection tool: Refine Edge; the wand group (Object Selection,
//!   Magic Wand, Quick Selection): Select Subject first.
//! * Type and Vertical Type: Warp (Layer > Text > Warp Text...) and Convert
//!   (Layer > Text > Convert to Shape).
//! * Every brush-driven tool: a brush-preset picker (the Brushes panel's
//!   presets, applied exactly as a click in the panel applies one).
//! * Crop: Photopea's "..." Crop by list - All Layers (Image > Reveal All),
//!   Current Layer, Trim, Selection (Image > Crop to Selection), in
//!   Photopea's words. Each row crops at once; Photopea sets the crop box
//!   and waits for the commit (not built, see the parity matrix).
//! * Artboard: the + buttons, a new artboard of the active one's size on
//!   each side of it.
//!
//! Every button raises a menu action ([`Intent::Action`]), so the bar and the
//! menu run one route; the captions are the actions' own labels in the
//! active language (`tr_owned`, as the menus draw them).

use super::*;
use crate::menu::{ArtboardSide, WarpTextItem, ZoomCommand};

/// Pseudo-keys the buttons are marked under
/// (`ids::tool_option(tool, key)`).
pub const FIT_KEY: &str = "w16k_fit";
pub const PIXEL_KEY: &str = "w16k_pixel_to_pixel";
pub const ROTATE_RESET_KEY: &str = "w16k_rotate_reset";
pub const ROTATE_ANGLE_KEY: &str = "w16k_rotate_angle";
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

/// Crop by's row captions, Photopea's words (`17.0`, `17.1`, `11.12.0`,
/// `17.2` in its string table), as catalogue keys.
pub fn crop_by_caption(action: MenuAction) -> &'static str {
    use crate::strings::tr;
    match action {
        MenuAction::RevealAll => tr("ui.w16k.crop_by.all_layers"),
        MenuAction::CropToLayer => tr("ui.w16k.crop_by.current_layer"),
        MenuAction::Trim => tr("ui.w16k.crop_by.trim"),
        _ => tr("ui.w16k.crop_by.selection"),
    }
}

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
pub const SUBJECT_TOOLS: [ToolId; 3] = [
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
    let caption = crate::strings::tr_owned(action.label());
    captioned_button(w, ui, tool, action, key, &caption);
}

fn captioned_button(
    w: &mut Workspace,
    ui: &mut Ui,
    tool: ToolId,
    action: MenuAction,
    key: &'static str,
    caption: &str,
) {
    let response =
        super::super::labelled_button(ui, caption, true, super::super::ids::tool_option(tool, key));
    if response.clicked() {
        w.emit(Intent::Action(action));
    }
}

/// Photopea's Rotate View Angle: the document camera's rotation as the
/// application publishes it for the Navigator, committed on Enter through
/// the Navigator's own request (`PanelRequest::SetViewAngle`).
fn angle_field(ui: &mut Ui) {
    use crate::panels::panel_menus_w16 as menus;
    let width = design::current_tokens(ui).metrics.numeric_field_width;
    ui.label(hint(ui, crate::strings::tr("ui.w16.navigator.angle")));
    let shown = menus::published_view_angle(ui.ctx());
    let field = super::super::text_field_sized(
        ui,
        super::super::ids::tool_option(ToolId::RotateView, ROTATE_ANGLE_KEY),
        &format!("{shown:.1}"),
        width,
    );
    ui.label(hint(ui, crate::strings::tr("ui.w16.navigator.degrees")));
    if let Some(typed) = field.committed {
        if let Some(angle) = crate::panels::navigator::parse_angle(&typed) {
            if (angle - shown).abs() > f32::EPSILON {
                menus::publish_view_angle(ui.ctx(), angle);
                menus::post(menus::PanelRequest::SetViewAngle(angle));
            }
        }
    }
}

/// Zoom and Rotate View have no settings, only Photopea's controls; `true`
/// when `tool` is one of them (the bar then draws nothing else). The Hand
/// is not: its bar says it has no options.
pub(super) fn view_row(w: &mut Workspace, ui: &mut Ui, tool: ToolId) -> bool {
    use crate::strings::tr;
    match tool {
        ToolId::Zoom => {
            captioned_button(
                w,
                ui,
                tool,
                MenuAction::Zoom(ZoomCommand::ActualPixels),
                PIXEL_KEY,
                tr("ui.w16k.bar.pixel_to_pixel"),
            );
            captioned_button(
                w,
                ui,
                tool,
                MenuAction::Zoom(ZoomCommand::FitOnScreen),
                FIT_KEY,
                tr("ui.w16k.bar.fit_the_area"),
            );
            true
        }
        ToolId::RotateView => {
            angle_field(ui);
            captioned_button(
                w,
                ui,
                tool,
                MenuAction::ResetViewRotation,
                ROTATE_RESET_KEY,
                tr("ui.w16k.bar.reset"),
            );
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
                let row = super::super::labelled_button(
                    ui,
                    crop_by_caption(action),
                    true,
                    crop_by_id(action),
                );
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
