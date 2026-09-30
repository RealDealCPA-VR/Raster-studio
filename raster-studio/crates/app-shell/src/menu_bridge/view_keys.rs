//! W10-J: the View menu's guide rows and the keyboard chords Photoshop and
//! Photopea have that no menu row lists.
//!
//! * **View > Guides from Layer** (W18-G: Photopea's row, any layer) —
//!   four guides on each selected layer's document bounds (left, top, right,
//!   bottom), appended to the document's guide set as one
//!   [`Command::SetGuides`] (one Ctrl+Z).
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

/// W18-G: View > Guides from Layer, Photopea's `gidsFromLayer`: for every
/// selected layer (of any kind) with content, four guides on its document
/// bounds — left, top, right, bottom — appended to the guide set as one
/// step. A layer with nothing drawn is skipped, as Photopea skips it.
pub fn guides_from_shape(editor: &mut Editor) -> Result<String, String> {
    let (command, layers) = {
        let doc = editor.active().ok_or("No document is open")?;
        let mut chosen = doc.document.layer_selection();
        if let Some(active) = doc.document.active_layer() {
            if !chosen.contains(&active) {
                chosen.push(active);
            }
        }
        if chosen.is_empty() {
            return Err("Select a layer first".to_string());
        }
        let mut guides = doc.document.guides.clone();
        let mut layers = 0usize;
        for id in chosen {
            // The layer's own rect, as Photopea's: its tight ink, not the
            // stored tiles that hold it.
            let bounds = crate::tool_input::tight_document_bounds(&doc.document, &doc.tiles, id);
            if let Some(bounds) = bounds.filter(|b| b.width > 0 && b.height > 0) {
                guides.list.extend(shape_guides(bounds));
                layers += 1;
            }
        }
        if layers == 0 {
            return Err("The selected layers have nothing drawn to put guides on".to_string());
        }
        (
            Command::Transaction {
                label: "Guides from Layer".to_string(),
                commands: vec![Command::SetGuides { guides }],
            },
            layers,
        )
    };
    editor.apply_command(command);
    Ok(format!(
        "Placed {} guides on the bounds of {layers} layer{}",
        layers * 4,
        if layers == 1 { "" } else { "s" }
    ))
}

/// The four guides Guides from Layer places on `bounds`, in Photopea's
/// order: left (vertical), top (horizontal), right, bottom.
pub fn shape_guides(bounds: raster::PixelRect) -> Vec<Guide> {
    let left = bounds.x as f32;
    let top = bounds.y as f32;
    let guide = |axis, doc| Guide {
        axis,
        doc,
        locked: false,
    };
    vec![
        guide(GuideAxis::Vertical, left),
        guide(GuideAxis::Horizontal, top),
        guide(GuideAxis::Vertical, left + bounds.width as f32),
        guide(GuideAxis::Horizontal, top + bounds.height as f32),
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
        assert_eq!(
            opacity_for_key(pending, 7, slow).0,
            70,
            "a slow key stands alone"
        );
    }

    #[test]
    fn hardness_steps_by_quarters_and_stops_at_the_ends() {
        assert_eq!(stepped_hardness(1.0, false), 0.75);
        assert_eq!(stepped_hardness(0.75, true), 1.0);
        assert_eq!(stepped_hardness(1.0, true), 1.0);
        assert_eq!(stepped_hardness(0.0, false), 0.0);
        assert_eq!(
            stepped_hardness(0.3, true),
            0.5,
            "0.3 snaps to 0.25 then steps"
        );
    }

    #[test]
    fn shape_guides_sit_on_the_four_edges_in_photopeas_order() {
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
        // W18-G: Photopea's gidsFromLayer puts no centre guides.
        assert_eq!(v, vec![10.0, 50.0]);
        assert_eq!(h, vec![20.0, 80.0]);
    }

    // ---- the real routes: keymap -> menu action -> `perform` -------------

    fn opened(dir: &std::path::Path) -> Editor {
        let p = dir.join("a.png");
        std::fs::write(
            &p,
            raster::encode(raster::ExportFormat::Png, 96, 64, &[200u8; 96 * 64 * 4]).unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            crate::prefs::AppPaths::rooted(dir.join("config")),
            crate::prefs::Preferences::default(),
            crate::recent::RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new()),
        );
        editor.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        editor.open_path(&p).unwrap();
        editor
    }

    /// The menu action a chord reaches through the default keymap.
    fn chord_action(chord: crate::keymap::Chord) -> ui::menu::MenuAction {
        match crate::keymap::Keymap::default().resolve_any(&chord) {
            Some(crate::keymap::Resolved::Menu(action)) => action,
            other => panic!("{chord} resolves to {other:?}"),
        }
    }

    fn digit(c: char) -> crate::keymap::Chord {
        crate::keymap::Chord::plain(crate::keymap::Key::character(c))
    }

    #[test]
    fn number_keys_set_the_painting_tools_opacity_and_two_quick_ones_are_exact() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.set_tool(tools::ToolId::Brush);
        let five = chord_action(digit('5'));
        assert_eq!(five, ui::menu::MenuAction::ToolOpacity(5));
        assert_eq!(super::super::perform(five, &mut ed).unwrap(), "Opacity 50%");
        assert!((ed.brush().opacity - 0.5).abs() < 1e-6);
        // A second 5 at once: 55%, exactly.
        super::super::perform(chord_action(digit('5')), &mut ed).unwrap();
        assert!(
            (ed.brush().opacity - 0.55).abs() < 1e-6,
            "{}",
            ed.brush().opacity
        );
        // A slow key stands alone; 0 is 100%.
        let later = Instant::now() + SECOND_DIGIT_WINDOW * 2;
        tool_opacity_at(&mut ed, 0, later).unwrap();
        assert!((ed.brush().opacity - 1.0).abs() < 1e-6);
        // The Move tool has no opacity: refused with the reason.
        ed.set_tool(tools::ToolId::Move);
        let refused = super::super::perform(chord_action(digit('3')), &mut ed).unwrap_err();
        assert!(refused.contains("no opacity"), "{refused}");
    }

    #[test]
    fn shift_brackets_step_the_brush_hardness_by_a_quarter() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.set_tool(tools::ToolId::Brush);
        let mut brush = *ed.brush();
        brush.hardness = 1.0;
        ed.set_brush(brush);
        let size = brush.size;
        let shift = |c: char| crate::keymap::Chord {
            ctrl_or_cmd: false,
            alt: false,
            shift: true,
            key: crate::keymap::Key::character(c),
        };
        let softer = chord_action(shift('['));
        assert_eq!(softer, ui::menu::MenuAction::BrushHardness(false));
        super::super::perform(softer, &mut ed).unwrap();
        assert!((ed.brush().hardness - 0.75).abs() < 1e-6);
        super::super::perform(softer, &mut ed).unwrap();
        assert!((ed.brush().hardness - 0.5).abs() < 1e-6);
        super::super::perform(chord_action(shift(']')), &mut ed).unwrap();
        assert!((ed.brush().hardness - 0.75).abs() < 1e-6);
        assert_eq!(ed.brush().size, size, "the size is untouched");
    }

    /// W18-G: View > Guides from Layer is Photopea's row: live over a pixel
    /// layer (not only a shape), four guides on each selected layer's
    /// bounds — left, top, right, bottom — as one undo step.
    #[test]
    fn guides_from_layer_puts_four_guides_on_every_selected_layers_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let pixels = ed.active().unwrap().document.active_layer().unwrap();
        let row = ui::menu::MenuAction::NewGuidesFromShape;
        assert_eq!(row.label(), "Guides from Layer");
        let chrome = crate::chrome::Chrome::new();
        let ctx = super::super::context(&mut ed, chrome.workspace());
        assert!(
            row.resolve(&ctx).is_enabled(),
            "a pixel layer is enough: {:?}",
            row.resolve(&ctx)
        );
        let before = ed.active().unwrap().document.guides.list.len();
        let depth = ed.active().unwrap().history.undo_depth();
        super::super::perform(row, &mut ed).unwrap();
        let doc = ed.active().unwrap();
        assert_eq!(doc.history.undo_depth(), depth + 1, "one undo step");
        let placed: Vec<(GuideAxis, f32)> = doc.document.guides.list[before..]
            .iter()
            .map(|g| (g.axis, g.doc))
            .collect();
        assert_eq!(
            placed,
            vec![
                (GuideAxis::Vertical, 0.0),
                (GuideAxis::Horizontal, 0.0),
                (GuideAxis::Vertical, 96.0),
                (GuideAxis::Horizontal, 64.0),
            ],
            "the pixel layer's bounds, Photopea's order"
        );
        assert!(ed.active_mut().unwrap().undo().unwrap());

        // A shape selected with the pixel layer: four more on its bounds.
        let shape = layer_model::Layer::with_kind(
            "Box",
            layer_model::LayerKind::Shape(layer_model::ShapeLayer::from_svg(
                "M10 20 H50 V60 H10 Z",
            )),
        );
        let id = shape.id;
        ed.apply_command(Command::create_layer(shape));
        {
            let doc = &mut ed.active_mut().unwrap().document;
            doc.set_active_layer(Some(id)).unwrap();
            doc.set_layer_selection(vec![pixels, id]).unwrap();
        }
        super::super::perform(row, &mut ed).unwrap();
        let guides = &ed.active().unwrap().document.guides.list[before..];
        assert_eq!(guides.len(), 8, "{guides:?}");
        let near = |axis, v: f32| {
            guides
                .iter()
                .any(|g| g.axis == axis && (g.doc - v).abs() <= 1.0)
        };
        for v in [10.0, 50.0] {
            assert!(near(GuideAxis::Vertical, v), "vertical {v}: {guides:?}");
        }
        for v in [20.0, 60.0] {
            assert!(near(GuideAxis::Horizontal, v), "horizontal {v}: {guides:?}");
        }
    }

    #[test]
    fn alt_ctrl_t_duplicates_the_layer_and_opens_free_transform_on_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.set_tool(tools::ToolId::Brush);
        let original = ed.active().unwrap().document.active_layer().unwrap();
        let count = ed
            .active()
            .unwrap()
            .document
            .layers
            .iter_depth_first()
            .len();
        let chord = crate::keymap::Chord::ctrl_alt(crate::keymap::Key::character('t'));
        let action = chord_action(chord);
        assert_eq!(action, ui::menu::MenuAction::DuplicateFreeTransform);
        super::super::perform(action, &mut ed).unwrap();
        let doc = ed.active().unwrap();
        assert_eq!(doc.document.layers.iter_depth_first().len(), count + 1);
        let active = doc.document.active_layer().unwrap();
        assert_ne!(active, original, "the copy is the layer being transformed");
        assert_eq!(ed.tool(), tools::ToolId::FreeTransform);
    }

    #[test]
    fn new_guide_layout_opens_from_the_view_menu_and_lands_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::NewGuideLayout, &ed));
        let crate::dialog_host::ActiveDialog::NewGuideLayout(dialog) = host.active_for_test()
        else {
            panic!("View > New Guide Layout did not open its dialog");
        };
        let mut spec = dialog.spec();
        spec.columns.count = 2;
        spec.columns.gutter = 16.0;
        dialog.set_spec(spec);
        let confirmed = ui::dialogs::Dialog::confirm(&**dialog);
        let Some(ui::dialogs::DialogAction::Command(command)) = confirmed else {
            panic!("the layout confirms to a command");
        };
        let depth = ed.active().unwrap().history.undo_depth();
        ed.apply_command(*command);
        let doc = ed.active().unwrap();
        assert_eq!(doc.history.undo_depth(), depth + 1);
        let xs: Vec<f32> = doc.document.guides.list.iter().map(|g| g.doc).collect();
        assert_eq!(
            xs,
            vec![0.0, 40.0, 56.0, 96.0],
            "two 40 px columns, a 16 px gutter"
        );
    }
}
