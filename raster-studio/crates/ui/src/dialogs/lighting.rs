//! Filter ▸ Render ▸ Lighting Effects: the light's on-preview handle (W10-C).
//!
//! Lighting Effects is one of the generated [`super::filter_dialog`] dialogs
//! (its schema, apply function and confirm path are there, so it previews,
//! lands as one undo step and becomes a smart filter on a smart object like
//! every other filter). What the generated form cannot do on its own is let
//! the light be *placed*: this module draws the light as a handle over the
//! preview image and turns a click or a drag on the preview into writes of
//! the light's position parameters ([`LIGHT_POSITION_KEYS`]), which the
//! numeric fields then show.

use design::tokens::palette::ColorRole;
use design::tokens::Space;
use design::{color32, current_tokens};
use egui::Sense;

use super::filter_dialog::{FilterId, FilterParams, ParamValue, LIGHT_POSITION_KEYS};

/// The id the preview's click-and-drag surface is registered under.
pub fn preview_id() -> egui::Id {
    egui::Id::new(("raster-studio-lighting", "preview"))
}

/// Where on the preview `params` puts the light.
pub fn handle_position(image: egui::Rect, params: &FilterParams) -> egui::Pos2 {
    let (kx, ky) = LIGHT_POSITION_KEYS;
    image.min
        + egui::vec2(
            params.float(kx).clamp(0.0, 1.0) * image.width(),
            params.float(ky).clamp(0.0, 1.0) * image.height(),
        )
}

/// Draw the light handle for `id`'s dialog over the preview image drawn at
/// `image`, and return the parameter writes a click or drag asked for.
///
/// Returns `None` for every filter but Lighting Effects, and when the pointer
/// did nothing this frame.
pub fn position_handle(
    ui: &mut egui::Ui,
    id: FilterId,
    image: egui::Rect,
    params: &FilterParams,
) -> Option<[(&'static str, ParamValue); 2]> {
    if id != FilterId::LightingEffects {
        return None;
    }
    let (kx, ky) = LIGHT_POSITION_KEYS;
    let response = ui.interact(image, preview_id(), Sense::click_and_drag());
    let mut writes = None;
    let mut centre = handle_position(image, params);
    if response.clicked() || response.drag_started() || response.dragged() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let local = pointer - image.min;
            let x = (local.x / image.width().max(1.0)).clamp(0.0, 1.0);
            let y = (local.y / image.height().max(1.0)).clamp(0.0, 1.0);
            centre = image.min + egui::vec2(x * image.width(), y * image.height());
            writes = Some([(kx, ParamValue::Float(x)), (ky, ParamValue::Float(y))]);
            ui.ctx().request_repaint();
        }
    }
    let t = current_tokens(ui);
    let painter = ui.painter_at(image);
    let radius = Space::Small.pt();
    painter.circle_filled(centre, radius, color32(t.palette.color(ColorRole::Accent)));
    painter.circle_stroke(
        centre,
        radius,
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::TextOnAccent)),
        ),
    );
    writes
}

#[cfg(test)]
mod tests {
    use super::super::chrome::test_support::Harness;
    use super::super::filter_dialog::{filter_by_id, FilterDialog};
    use super::*;

    fn lit_dialog() -> FilterDialog {
        let spec = filter_by_id(FilterId::LightingEffects).expect("Lighting Effects has a dialog");
        FilterDialog::new(
            spec,
            filters::FilterBuffer::filled(64, 64, [0.4, 0.4, 0.4, 1.0]).unwrap(),
        )
    }

    fn drag_events(from: egui::Pos2, to: egui::Pos2) -> Vec<Vec<egui::Event>> {
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let mut frames = vec![vec![egui::Event::PointerMoved(from), button(from, true)]];
        for step in 1..=6 {
            frames.push(vec![egui::Event::PointerMoved(
                from + (to - from) * (step as f32 / 6.0),
            )]);
        }
        frames.push(vec![button(to, false)]);
        frames.push(Vec::new());
        frames
    }

    /// The real route: the generated dialog, drawn headless, has a live
    /// surface over its preview; dragging on it moves the light, and the
    /// preview it then renders is brightest where the light went.
    #[test]
    fn dragging_on_the_lighting_preview_moves_the_light() {
        let mut dialog = lit_dialog();
        let harness = Harness::new();
        let rect = harness.settle(preview_id(), |ctx| {
            let _ = dialog.show(ctx, None);
        });
        // The handle starts at the schema's centre.
        let start = handle_position(rect, dialog.params());
        assert!(
            (start - rect.center()).length() < 1.0,
            "{start:?} vs {rect:?}"
        );

        let to = rect.min + egui::vec2(rect.width() * 0.2, rect.height() * 0.8);
        for events in drag_events(rect.center(), to) {
            harness.frame(events, |ctx| {
                let _ = dialog.show(ctx, None);
            });
        }
        let (kx, ky) = LIGHT_POSITION_KEYS;
        let x = dialog.params().float(kx);
        let y = dialog.params().float(ky);
        assert!(
            (x - 0.2).abs() < 0.03 && (y - 0.8).abs() < 0.03,
            "the light followed the drag: ({x}, {y})"
        );
        let lit = dialog.preview_buffer();
        assert!(
            lit.get(12, 51)[0] > lit.get(51, 12)[0],
            "the preview is brightest under the moved light"
        );
    }

    #[test]
    fn other_filters_get_no_light_handle() {
        let spec = filter_by_id(FilterId::GaussianBlur).unwrap();
        let mut dialog = FilterDialog::new(
            spec,
            filters::FilterBuffer::filled(32, 32, [0.4, 0.4, 0.4, 1.0]).unwrap(),
        );
        let harness = Harness::new();
        for _ in 0..4 {
            harness.frame(Vec::new(), |ctx| {
                let _ = dialog.show(ctx, None);
            });
        }
        assert!(!harness.was_drawn(preview_id()));
    }
}
