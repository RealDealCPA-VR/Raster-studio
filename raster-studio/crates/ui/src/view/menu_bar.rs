//! Drawing the menu bar.
//!
//! One rule, applied at exactly one place: [`item`] resolves the action against
//! the frame's [`MenuContext`] and either enables the row and wires its click
//! to the resulting intent, or disables it and attaches the reason to its
//! hover text. There is no path through this file that draws a row which does
//! nothing.

use design::{color32, current_tokens, egui_theme::font_id, ColorRole, Space, TextRole, TypeRole};
use egui::{Response, Ui};

use crate::menu::{menu_bar as build_menus, Entry, MenuAction, MenuContext, Resolution};
use crate::Workspace;

use super::text;

/// Draw the bar and post whatever the user picked.
pub fn menu_bar(w: &mut Workspace, ctx: &egui::Context, context: &MenuContext) {
    let menus = build_menus(w.recent.len());
    egui::TopBottomPanel::top("raster-menu-bar")
        .frame(bar_frame(ctx))
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.spacing_mut().item_spacing.x = Space::Hair.pt();
                for menu in &menus {
                    let title = text(ui, menu.title, TextRole::Primary, TypeRole::Body);
                    let opener = ui.menu_button(title, |ui| {
                        ui.set_min_width(menu_min_width(ui));
                        entries(w, ui, &menu.entries, context);
                    });
                    super::mark(ui, opener.response.rect, super::ids::menu_title(menu.title));
                }
            });
        });
}

/// The menu bar's own surface: the panel colour with a hairline beneath it, so
/// the bar reads as part of the chrome rather than as a box on top of it.
fn bar_frame(ctx: &egui::Context) -> egui::Frame {
    let t = design::current_theme(ctx).tokens();
    egui::Frame::none()
        .fill(color32(t.palette.color(ColorRole::SurfacePanel)))
        .inner_margin(egui::Margin::symmetric(Space::Small.pt(), Space::Hair.pt()))
        .stroke(egui::Stroke::NONE)
}

/// Wide enough that a shortcut hint never collides with its label.
fn menu_min_width(ui: &Ui) -> f32 {
    current_tokens(ui).metrics.inspector_label_width * 2.5
}

fn entries(w: &mut Workspace, ui: &mut Ui, entries: &[Entry], context: &MenuContext) {
    for entry in entries {
        match entry {
            Entry::Item(action) => {
                item(w, ui, *action, context);
            }
            Entry::Separator => {
                ui.add_space(Space::Hair.pt());
                super::hairline(ui);
                ui.add_space(Space::Hair.pt());
            }
            Entry::Submenu {
                label,
                entries: children,
            } => {
                let enabled = children
                    .iter()
                    .flat_map(Entry::actions)
                    .any(|a| a.resolve(context).is_enabled());
                let title = text(
                    ui,
                    *label,
                    if enabled {
                        TextRole::Primary
                    } else {
                        TextRole::Disabled
                    },
                    TypeRole::Body,
                );
                // A submenu whose every child is unavailable is itself
                // unavailable, and says so on hover rather than opening onto a
                // list of dead rows.
                let opener = if enabled {
                    ui.menu_button(title, |ui| {
                        ui.set_min_width(menu_min_width(ui));
                        self::entries(w, ui, children, context);
                    })
                    .response
                } else {
                    ui.add_enabled(false, egui::Button::new(title))
                        .on_disabled_hover_text("Nothing in this submenu is available right now")
                };
                super::mark(ui, opener.rect, super::ids::menu_submenu(label));
            }
        }
    }
}

/// One row. The whole contract of the menu lives here.
fn item(w: &mut Workspace, ui: &mut Ui, action: MenuAction, context: &MenuContext) -> Response {
    let resolution = action.resolve(context);
    let enabled = resolution.is_enabled();
    let t = current_tokens(ui);

    // Undo and Redo name the step they would move and an Open Recent slot
    // names its file, which is the difference between a menu that reports and
    // one that merely lists. The decision lives in `MenuAction::label_in`, so
    // it is testable without a window.
    let label = action.label_in(context);

    // The checkable rows reserve the gutter with spaces and the tick is *drawn*
    // into it below. It used to be a "✓" in the label, and U+2713 is not in the
    // font egui loads, so every checked menu row showed a tofu box.
    let checked = action.checked(context);
    let check = if checked.is_some() { "     " } else { "" };
    let role = if enabled {
        TextRole::Primary
    } else {
        TextRole::Disabled
    };
    let rich = egui::RichText::new(format!("{check}{label}"))
        .color(color32(t.palette.text(role)))
        .font(font_id(t, TypeRole::Body));

    let mut button = egui::Button::new(rich);
    if let Some(chord) = action.shortcut() {
        button = button.shortcut_text(
            egui::RichText::new(chord.to_string())
                .color(color32(t.palette.text(TextRole::Tertiary)))
                .font(font_id(t, TypeRole::Footnote)),
        );
    }

    let response = ui.add_enabled(enabled, button);
    if checked == Some(true) {
        let side = response.rect.height().min(t.metrics.min_hit_target);
        let gutter = egui::Rect::from_center_size(
            egui::pos2(response.rect.left() + side * 0.5, response.rect.center().y),
            egui::Vec2::splat(side),
        );
        super::paint_icon(
            ui,
            gutter,
            "check",
            if enabled {
                TextRole::Primary
            } else {
                TextRole::Disabled
            },
        );
    }
    // A menu row's own id comes from call order, which no test can name. This
    // one is keyed by the action, so `tests/wired_controls.rs` can click the
    // exact row and assert what it posts — the gate this file's opening claim
    // ("there is no path through this file that draws a row which does
    // nothing") needs in order to mean anything.
    super::mark(ui, response.rect, super::ids::menu_item(action));
    match resolution {
        Resolution::Enabled(intent) => {
            if response.clicked() {
                w.emit(intent);
                ui.close_menu();
            }
            response
        }
        Resolution::Disabled(reason) => response.on_disabled_hover_text(reason),
    }
}
