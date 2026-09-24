//! W10-J: the View menu's guide rows and the keyboard chords Photoshop and
//! Photopea have that no menu row lists.
//!
//! * **View > New Guides from Shape** — guides at the active shape layer's
//!   document bounds: both vertical edges and the vertical centre, both
//!   horizontal edges and the horizontal centre, appended to the document's
//!   guide set as one [`Command::SetGuides`] (one Ctrl+Z).
//! * **Alt+Ctrl+T** — duplicate, then Free Transform the copy: with a
//!   selection the selected pixels are floated to a new layer (Layer via
//!   Copy), without one the whole layer is duplicated; either way the copy is
//!   the active layer when the transform session begins, so the original
//!   stays where it was.
//! * **Shift+[ / Shift+]** — the painting tool's hardness steps down / up by
//!   25% (0, 25, 50, 75, 100), Photoshop's steps.
//! * **The number keys** — the painting tool's opacity: `1`..`9` are
//!   10%..90% and `0` is 100%; a second digit typed within
//!   [`SECOND_DIGIT_WINDOW`] of the first makes the two an exact percent
//!   (`5` `5` is 55%, `0` `5` is 5%).

use std::cell::Cell;
use std::time::{Duration, Instant};

use editor_core::{Command, Guide, GuideAxis};

use crate::editor::Editor;

/// How soon a second number key has to follow the first to be read as the
/// second digit of one percent rather than a new single-digit opacity.
pub const SECOND_DIGIT_WINDOW: Duration = Duration::from_millis(900);

thread_local! {
    /// The first digit of a possible two-digit opacity, and when it arrived.
    static PENDING_DIGIT: Cell<Option<(u8, Instant)>> = const { Cell::new(None) };
}

/// What one number key means, given the digit still waiting for a partner
/// (if any, with its arrival time) and the time now: the opacity percent to
/// set, and the digit that is now waiting. A pair consumes the waiting digit,
/// so a third key starts afresh.
pub fn opacity_for_key(
    pending: Option<(u8, Instant)>,
    digit: u8,
    now: Instant,
) -> (u8, Option<(u8, Instant)>) {
    match pending {
        Some((first, at)) if now.saturating_duration_since(at) <= SECOND_DIGIT_WINDOW => {
            (ui::menu::opacity_of_two_digits(first, digit), None)
        }
        _ => (ui::menu::opacity_of_digit(digit), Some((digit, now))),
    }
}

/// Whether `tool`'s options bar has the brush `key` (`opacity`, `hardness`):
/// the painting tools do, the selection and navigation tools do not.
fn tool_has_brush_option(tool: tools::ToolId, key: &str) -> bool {
    tools::registry::info(tool).is_some_and(|info| info.options.iter().any(|o| o.key == key))
}

/// A number key: set the painting tool's opacity (see the module docs).
pub fn tool_opacity(editor: &mut Editor, digit: u8) -> Result<String, String> {
    tool_opacity_at(editor, digit, Instant::now())
}

/// [`tool_opacity`] at an explicit time, for tests.
pub fn tool_opacity_at(editor: &mut Editor, digit: u8, now: Instant) -> Result<String, String> {
    if digit > 9 {
        return Err("Opacity keys are the digits 0 to 9".to_string());
    }
    let tool = editor.tool();
    if !tool_has_brush_option(tool, "opacity") {
        PENDING_DIGIT.with(|p| p.set(None));
        return Err(format!(
            "The {} tool has no opacity to set",
            tools::registry::info(tool).map_or("active", |i| i.name)
        ));
    }
    let (percent, pending) = opacity_for_key(PENDING_DIGIT.with(Cell::get), digit, now);
    PENDING_DIGIT.with(|p| p.set(pending));
    let mut brush = *editor.brush();
    brush.opacity = f32::from(percent) / 100.0;
    editor.set_brush(brush);
    Ok(format!("Opacity {percent}%"))
}

/// Hardness one Shift+bracket step from `hardness`: the nearest quarter,
/// moved one quarter `harder` or softer, within 0..=1.
pub fn stepped_hardness(hardness: f32, harder: bool) -> f32 {
    let quarters = if hardness.is_finite() {
        (hardness.clamp(0.0, 1.0) * 4.0).round() as i32
    } else {
        4
    };
    let next = if harder { quarters + 1 } else { quarters - 1 };
    next.clamp(0, 4) as f32 / 4.0
}

/// Shift+[ / Shift+]: step the painting tool's hardness by 25%.
pub fn brush_hardness(editor: &mut Editor, harder: bool) -> Result<String, String> {
    let tool = editor.tool();
    if !tool_has_brush_option(tool, "hardness") {
        return Err(format!(
            "The {} tool has no hardness to set",
            tools::registry::info(tool).map_or("active", |i| i.name)
        ));
    }
    let mut brush = *editor.brush();
    let next = stepped_hardness(brush.hardness, harder);
    if (next - brush.hardness).abs() < f32::EPSILON {
        return Err(if harder {
            "The brush is already at full hardness".to_string()
        } else {
            "The brush is already at zero hardness".to_string()
        });
    }
    brush.hardness = next;
    editor.set_brush(brush);
    Ok(format!("Hardness {}%", (next * 100.0).round() as i32))
}

/// View > New Guides from Shape: guides at the active shape layer's bounds.
pub fn guides_from_shape(editor: &mut Editor) -> Result<String, String> {
    let command = {
        let doc = editor.active().ok_or("No document is open")?;
        let id = doc
            .document
            .active_layer()
            .ok_or("Select a shape layer first")?;
        let layer = doc
            .document
            .layers
            .get(id)
            .ok_or("The active layer is not in the tree")?;
        if !matches!(layer.kind, layer_model::LayerKind::Shape(_)) {
            return Err("The active layer is not a shape layer".to_string());
        }
        let bounds = compositor::bounds::document_bounds(
            &doc.document,
            &doc.tiles,
            id,
            0,
            compositor::CompositeOptions::default(),
        )
        .map_err(|e| e.to_string())?
        .ok_or("The shape layer has no outline to put guides on")?;
        let mut guides = doc.document.guides.clone();
        guides.list.extend(shape_guides(bounds));
        Command::SetGuides { guides }
    };
    editor.apply_command(command);
    Ok("Placed 6 guides on the shape's bounds".to_string())
}

/// The six guides New Guides from Shape places on `bounds`: left, centre and
/// right (vertical), then top, centre and bottom (horizontal).
pub fn shape_guides(bounds: raster::PixelRect) -> Vec<Guide> {
    let left = bounds.x as f32;
    let top = bounds.y as f32;
    let right = left + bounds.width as f32;
    let bottom = top + bounds.height as f32;
    let guide = |axis, doc| Guide {
        axis,
        doc,
        locked: false,
    };
    vec![
        guide(GuideAxis::Vertical, left),
        guide(GuideAxis::Vertical, (left + right) * 0.5),
        guide(GuideAxis::Vertical, right),
        guide(GuideAxis::Horizontal, top),
        guide(GuideAxis::Horizontal, (top + bottom) * 0.5),
        guide(GuideAxis::Horizontal, bottom),
    ]
}

/// Alt+Ctrl+T: duplicate (or float the selection to a copy) and begin Free
/// Transform on the copy.
pub fn duplicate_free_transform(editor: &mut Editor) -> Result<String, String> {
    let has_selection = editor
        .active()
        .ok_or("No document is open")?
        .document
        .selection
        .bounds()
        .is_some();
    let copied = if has_selection {
        super::layer_via(editor, false)?
    } else {
        crate::layer_ops::duplicate_layer(editor, None)?
    };
    crate::tool_input::request_free_transform(editor.tool());
    editor.set_tool(tools::ToolId::FreeTransform);
    Ok(format!(
        "{copied}; Free Transform: drag a handle, Enter to commit, Escape to cancel"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_digit_is_tens_and_zero_is_full() {
        let now = Instant::now();
        assert_eq!(opacity_for_key(None, 5, now).0, 50);
        assert_eq!(opacity_for_key(None, 1, now).0, 10);
        assert_eq!(opacity_for_key(None, 0, now).0, 100);
    }

    #[test]
    fn two_quick_digits_are_an_exact_percent_and_a_slow_one_starts_again() {
        let t0 = Instant::now();
        let (first, pending) = opacity_for_key(None, 5, t0);
        assert_eq!(first, 50);
        let quick = t0 + Duration::from_millis(200);
        let (pair, after) = opacity_for_key(pending, 5, quick);
        assert_eq!(pair, 55, "5 then 5 quickly is 55%");
        assert_eq!(after, None, "the pair consumed the waiting digit");
        let (zero_five, _) = opacity_for_key(Some((0, t0)), 5, quick);
        assert_eq!(zero_five, 5, "0 then 5 is 5%");
        let slow = t0 + SECOND_DIGIT_WINDOW + Duration::from_millis(50);
        assert_eq!(opacity_for_key(pending, 7, slow).0, 70, "a slow key stands alone");
    }

    #[test]
    fn hardness_steps_by_quarters_and_stops_at_the_ends() {
        assert_eq!(stepped_hardness(1.0, false), 0.75);
        assert_eq!(stepped_hardness(0.75, true), 1.0);
        assert_eq!(stepped_hardness(1.0, true), 1.0);
        assert_eq!(stepped_hardness(0.0, false), 0.0);
        assert_eq!(stepped_hardness(0.3, true), 0.5, "0.3 snaps to 0.25 then steps");
    }

    #[test]
    fn shape_guides_sit_on_the_edges_and_centres() {
        let g = shape_guides(raster::PixelRect::new(10, 20, 40, 60));
        let v: Vec<f32> = g
            .iter()
            .filter(|g| g.axis == GuideAxis::Vertical)
            .map(|g| g.doc)
            .collect();
        let h: Vec<f32> = g
            .iter()
            .filter(|g| g.axis == GuideAxis::Horizontal)
            .map(|g| g.doc)
            .collect();
        assert_eq!(v, vec![10.0, 30.0, 50.0]);
        assert_eq!(h, vec![20.0, 50.0, 80.0]);
    }
}
