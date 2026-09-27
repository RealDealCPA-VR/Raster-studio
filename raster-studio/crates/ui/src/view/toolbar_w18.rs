//! W18-F: the options-bar controls Photopea's bars carry that ours lacked.
//!
//! * The Cancel cross beside the Commit check (Photopea's `im` pair), on
//!   every bar that shows the check - and the check now also shows for a
//!   Type run, a Perspective Crop quad and a held Show Transform Controls
//!   drag, which the shell publishes ([`tools::registry::bar_w18`]).
//!   Puppet Warp is not here: ours is a modal dialog with its own OK and
//!   Cancel, not an on-canvas session with a bar.
//! * The Parametric Shape bar shows only the picked shape's keys
//!   ([`tools::registry::parametric_option_shown`]).
//! * The Zoom bar's Zoom In / Zoom Out toggle and the Zoom and Hand bars'
//!   All Documents box ([`tools::registry::bar_w18::NavPrefs`], read by the
//!   shell's pointer route).
//! * The Clone Stamp and Healing Brush bars' Alt toggle (Photopea's "Select
//!   Source": the next click picks the source, then the toggle pops out),
//!   and the K key held for the same pick (read here, every frame, from the
//!   keyboard egui sees).
//! * The Paint Bucket's pattern picker, beside its Fill choice while that is
//!   Pattern: the defined patterns the shell publishes; a row makes that
//!   pattern the active one.

use super::*;
use tools::registry::bar_w18 as bar;

/// Pseudo-keys the controls are marked under (`ids::tool_option(tool, key)`).
pub const CANCEL_KEY: &str = "w18_cancel";
pub const ZOOM_IN_KEY: &str = "w18_zoom_in";
pub const ZOOM_OUT_KEY: &str = "w18_zoom_out";
pub const ALL_DOCUMENTS_KEY: &str = "w18_all_documents";
pub const SELECT_SOURCE_KEY: &str = "w18_select_source";
pub const PATTERN_PICKER_KEY: &str = "w18_pattern_picker";

/// The tools whose bar carries the Alt (Select Source) toggle.
pub const SOURCE_TOOLS: [ToolId; 2] = [ToolId::CloneStamp, ToolId::HealingBrush];

/// The id of the pattern picker's row `index`.
pub fn pattern_row_id(index: usize) -> egui::Id {
    super::super::ids::tool_option(ToolId::PaintBucket, PATTERN_PICKER_KEY).with(index)
}

/// Every frame: whether K is held for a clone-source pick. Not while a text
/// field has the keyboard, and not as part of a shortcut chord.
pub(super) fn observe_keys(ctx: &egui::Context) {
    let held = !ctx.wants_keyboard_input()
        && ctx.input(|i| {
            i.key_down(egui::Key::K)
                && !i.modifiers.command
                && !i.modifiers.alt
                && !i.modifiers.ctrl
        });
    bar::set_source_key_held(held);
}

/// The bar's options for `tool` less the Parametric Shape keys the picked
/// shape does not own.
pub(super) fn shown(
    options: &crate::ToolOptions,
    tool: ToolId,
    specs: Vec<OptionSpec>,
) -> Vec<OptionSpec> {
    if tool != ToolId::Polygon {
        return specs;
    }
    let pshape = options
        .get(tool, "pshape")
        .and_then(OptionValue::as_choice)
        .unwrap_or(0);
    specs
        .into_iter()
        .filter(|spec| tools::registry::parametric_option_shown(spec.key, pshape))
        .collect()
}

/// Whether the shell published a held edit for `tool`.
pub(super) fn published_pending(tool: ToolId) -> bool {
    bar::pending_for(tool) == Some(true)
}

/// Photopea's Cancel cross: the held edit is abandoned as Escape abandons
/// it (the shell takes the request on its next frame).
pub(super) fn cancel_button(ui: &mut Ui, tool: ToolId) {
    let response = super::super::icon_button_id(
        ui,
        "close",
        true,
        super::super::ids::tool_option(tool, CANCEL_KEY),
    )
    .on_hover_text(crate::strings::tr("ui.w18f.cancel.hint"));
    if response.clicked() {
        bar::post_cancel();
    }
}

/// The Zoom bar's Zoom In / Zoom Out pair: which way a click steps.
pub(super) fn zoom_direction(ui: &mut Ui) {
    use crate::strings::tr;
    let mut prefs = bar::nav();
    for (out, key, icon, tip) in [
        (false, ZOOM_IN_KEY, "plus", tr("ui.w18f.zoom_in")),
        (true, ZOOM_OUT_KEY, "minus", tr("ui.w18f.zoom_out")),
    ] {
        let id = super::super::ids::tool_option(ToolId::Zoom, key);
        let response = super::super::icon_toggle_id(ui, icon, prefs.zoom_out == out, tip, Some(id));
        if response.clicked() && prefs.zoom_out != out {
            prefs.zoom_out = out;
            bar::set_nav(prefs);
        }
    }
}

/// The Zoom and Hand bars' All Documents box.
pub(super) fn all_documents(ui: &mut Ui, tool: ToolId) {
    let mut prefs = bar::nav();
    let mut on = match tool {
        ToolId::Hand => prefs.hand_all_documents,
        _ => prefs.zoom_all_documents,
    };
    let response = ui.checkbox(
        &mut on,
        hint(ui, crate::strings::tr("ui.w18f.all_documents")),
    );
    super::super::mark(
        ui,
        response.rect,
        super::super::ids::tool_option(tool, ALL_DOCUMENTS_KEY),
    );
    if response.changed() {
        match tool {
            ToolId::Hand => prefs.hand_all_documents = on,
            _ => prefs.zoom_all_documents = on,
        }
        bar::set_nav(prefs);
    }
}

/// The Clone Stamp / Healing Brush Alt toggle.
pub(super) fn select_source(ui: &mut Ui, tool: ToolId) {
    use crate::strings::tr;
    let armed = bar::select_source_armed();
    let response = ui
        .selectable_label(armed, body(ui, tr("ui.w18f.select_source.alt")))
        .on_hover_text(tr("ui.w18f.select_source"));
    super::super::mark(
        ui,
        response.rect,
        super::super::ids::tool_option(tool, SELECT_SOURCE_KEY),
    );
    if response.clicked() {
        bar::arm_select_source(!armed);
    }
}

/// Whether the Paint Bucket fills with the pattern right now.
pub(super) fn bucket_fills_pattern(w: &Workspace) -> bool {
    w.options
        .get(ToolId::PaintBucket, tools::bucket::FILL_SOURCE_KEY)
        .and_then(OptionValue::as_choice)
        .is_some_and(|i| {
            tools::bucket::FillSource::from_choice(i) == tools::bucket::FillSource::Pattern
        })
}

/// The Paint Bucket's pattern picker: the active pattern's name opens the
/// defined patterns; a row makes that one active.
pub(super) fn pattern_picker(ui: &mut Ui) {
    use crate::strings::tr;
    let (names, active) = bar::patterns();
    ui.label(hint(ui, tr("ui.w18f.pattern")));
    let opener_id = super::super::ids::tool_option(ToolId::PaintBucket, PATTERN_PICKER_KEY);
    let caption = active
        .clone()
        .unwrap_or_else(|| tr("ui.w18f.no_patterns").to_string());
    let opener = super::super::labelled_button(ui, &caption, !names.is_empty(), opener_id);
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
            egui::ScrollArea::vertical()
                .max_height(12.0 * ui.spacing().interact_size.y)
                .show(ui, |ui| {
                    for (i, name) in names.iter().enumerate() {
                        let row = super::super::labelled_button(ui, name, true, pattern_row_id(i));
                        if row.clicked() {
                            picked = Some(name.clone());
                        }
                    }
                });
        },
    );
    if let Some(name) = picked {
        ui.memory_mut(|m| m.close_popup());
        bar::post_pattern_pick(name);
    }
}
