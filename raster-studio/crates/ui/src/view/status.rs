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
///
/// It is a text field at all times rather than a label that swaps itself for
/// one on click, and that is the whole fix for the control this used to be.
/// The old shape inserted an editing buffer into memory on the click frame and
/// only built the `TextEdit` on the *next* frame — where it appeared without
/// focus. The first click therefore did nothing visible, and, because
/// [`egui::Context::wants_keyboard_input`] stayed false, `Workspace::handle_keys`
/// went on routing the keystrokes that
/// followed to [`crate::keys::tool_for_key`]: clicking the zoom readout and
/// typing silently switched tools. A field that is always present takes focus
/// from the click that lands on it, which is what makes the very next
/// keystroke arrive here instead.
///
/// The edit itself is [`super::text_field`]'s, so the seed / commit / cancel
/// rules are the ones every other field in the chrome follows rather than a
/// second hand-rolled memory buffer.
fn zoom_field(w: &mut Workspace, ui: &mut Ui) {
    let width = current_tokens(ui).metrics.numeric_field_width;
    let shown = format_zoom(w.status.zoom);
    let edit = super::text_field_sized(ui, super::ids::status_zoom(), &shown, width);
    edit.response.on_hover_text("Type a zoom level");
    let Some(text) = edit.committed else {
        return;
    };
    // Unreadable text is dropped rather than guessed at: the field re-seeds
    // itself from the live zoom on the next frame, so the readout goes back to
    // the truth instead of keeping whatever was typed.
    if let Some(zoom) = StatusBar::parse_zoom(&text) {
        if zoom != w.status.zoom {
            w.emit(Intent::SetZoom(zoom));
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
