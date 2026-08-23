//! Drawing the status bar.

use design::{color32, current_tokens, ColorRole, Radius, Space, TextRole, TypeRole};
use editor_core::Document;
use egui::{Align, Layout, Sense, Ui, Vec2};

use crate::intent::Intent;
use crate::panels::navigator::format_zoom;
use crate::status::StatusBar;
use crate::Workspace;

use super::{body, hint, text};

/// The strip along the bottom of the window.
pub fn status_bar(w: &mut Workspace, ctx: &egui::Context, doc: &Document) {
    let t = design::current_theme(ctx).tokens();
    egui::TopBottomPanel::bottom("raster-status")
        .frame(
            egui::Frame::none()
                .fill(color32(t.palette.color(ColorRole::SurfacePanel)))
                .inner_margin(egui::Margin::symmetric(
                    t.metrics.panel_padding,
                    Space::Hair.pt(),
                )),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Space::Small.pt();
                zoom_field(w, ui);
                separator(ui);
                let fields = w.status.fields(doc);
                // The zoom field is drawn above, so skip its readout here.
                for field in fields.iter().skip(1) {
                    if field.label == "Memory" && w.status.is_busy() {
                        continue;
                    }
                    ui.label(hint(ui, format!("{}: ", field.label)));
                    ui.label(body(ui, field.value.clone()));
                    separator(ui);
                }
                if w.status.is_busy() {
                    progress(w, ui);
                    separator(ui);
                }
                ui.label(hint(ui, w.status.tool_hint()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if doc.is_dirty() {
                        ui.label(text(
                            ui,
                            "Unsaved changes",
                            TextRole::Tertiary,
                            TypeRole::Footnote,
                        ));
                    }
                });
            });
        });
}

/// The zoom readout, editable in place.
fn zoom_field(w: &mut Workspace, ui: &mut Ui) {
    let t = current_tokens(ui);
    let id = egui::Id::new("raster-status-zoom-buffer");
    let editing = ui.memory(|m| m.data.get_temp::<String>(id));
    match editing {
        Some(mut buffer) => {
            let response = ui.add_sized(
                Vec2::new(t.metrics.numeric_field_width, t.metrics.control_height),
                egui::TextEdit::singleline(&mut buffer),
            );
            let commit = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if commit {
                if let Some(zoom) = StatusBar::parse_zoom(&buffer) {
                    w.emit(Intent::SetZoom(zoom));
                }
            }
            if commit || cancel || response.clicked_elsewhere() {
                ui.memory_mut(|m| m.data.remove::<String>(id));
            } else {
                ui.memory_mut(|m| m.data.insert_temp(id, buffer));
            }
        }
        None => {
            let label = format_zoom(w.status.zoom);
            let response = ui
                .add(egui::Button::new(body(ui, label.clone())).frame(false))
                .on_hover_text("Click to type a zoom level");
            if response.clicked() {
                ui.memory_mut(|m| m.data.insert_temp(id, label));
            }
        }
    }
}

fn progress(w: &mut Workspace, ui: &mut Ui) {
    let Some(progress) = w.status.progress.clone() else {
        return;
    };
    let t = current_tokens(ui);
    ui.label(hint(ui, progress.label.clone()));
    let width = t.metrics.inspector_label_width;
    let height = Space::Small.pt();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    if ui.is_rect_visible(rect) {
        let radius = Radius::Continuous.resolve(&t.radii, height);
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            design::egui_theme::rounding(radius),
            color32(t.palette.color(ColorRole::SurfaceSunken)),
        );
        // An indeterminate operation gets a travelling bar rather than a full
        // one, so "unknown length" never reads as "finished".
        let (start, end) = match progress.fraction {
            Some(f) => (0.0, f.clamp(0.0, 1.0)),
            None => {
                let phase = ui.input(|i| i.time as f32 * 0.6).fract();
                let head = phase * 1.4 - 0.2;
                ((head - 0.2).clamp(0.0, 1.0), head.clamp(0.0, 1.0))
            }
        };
        if end > start {
            let filled = egui::Rect::from_min_max(
                egui::pos2(rect.left() + start * rect.width(), rect.top()),
                egui::pos2(rect.left() + end * rect.width(), rect.bottom()),
            );
            painter.rect_filled(
                filled,
                design::egui_theme::rounding(radius),
                color32(t.palette.color(ColorRole::Accent)),
            );
        }
        if progress.fraction.is_none() {
            ui.ctx().request_repaint();
        }
    }
}

fn separator(ui: &mut Ui) {
    let t = current_tokens(ui);
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(t.borders.hairline, t.metrics.control_height * 0.6),
        Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        ui.painter().vline(
            rect.center().x,
            rect.y_range(),
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::SeparatorHairline)),
            ),
        );
    }
}
