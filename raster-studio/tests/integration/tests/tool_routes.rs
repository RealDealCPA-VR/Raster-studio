//! W3-I: every palette tool, driven through the real pointer route.
//!
//! Each test here selects a tool the way the palette does
//! (`ui::Intent::SelectTool` → `menu_bridge::pick` → `menu_bridge::record` →
//! `ChromeOutput::select_tool` → `Editor::set_tool`, the exact steps
//! `shell.rs` performs), performs a realistic gesture through
//! `app_shell::tool_input::ToolPointer::handle` — the route `shell.rs` feeds
//! winit pointer events into — on a fixture document with known pixels, and
//! asserts an observable specific to that tool: which pixels changed and in
//! which direction, or which pixels the selection now covers. Every editing
//! gesture must land as exactly ONE history entry, and one Undo through the
//! editor's own action route must restore the document byte for byte.
//!
//! A tool that fails its route test here is a real bug in the product, not in
//! the test: the history of this project says an untested route hides a dead
//! tool.

use app_shell::action::Action;
use app_shell::edit_target::EditTargetKind;
use app_shell::editor::Editor;
use app_shell::tool_input::{PointerOutcome, ToolPointer};
use app_shell::{menu_bridge, Chrome, ChromeOutput};
use editor_core::Selection;
use glam::{IVec2, Vec2};
use integration_tests::app::{self, DocExt};
use tools::{Modifiers, ToolId};
use ui::canvas::Route;

// --------------------------------------------------------------- fixtures --

/// The fixture canvas. Large enough for the 60 px dodge/burn/sponge brushes
/// to leave untouched pixels around a stroke, small enough that a 400 × 300
/// viewport shows all of it at 100%.
const W: u32 = 128;
const H: u32 = 128;

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const GREY: [u8; 4] = [160, 160, 160, 255];
const MID_GREY: [u8; 4] = [128, 128, 128, 255];
const DARK: [u8; 4] = [40, 40, 40, 255];
const DARK_RED: [u8; 4] = [128, 0, 0, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
/// The two tones of the 4 px checker the blur/sharpen tests read variance off.
const CHECKER_LO: [u8; 4] = [96, 96, 96, 255];
const CHECKER_HI: [u8; 4] = [160, 160, 160, 255];
/// A saturated red the sponge desaturates.
const SATURATED: [u8; 4] = [200, 60, 60, 255];
/// Flash-red pupil colour for the red-eye fixture.
const PUPIL: [u8; 4] = [220, 30, 30, 255];

/// Left half red, right half blue, split at x = 64.
fn halves(x: u32, _y: u32) -> [u8; 4] {
    if x < 64 {
        RED
    } else {
        BLUE
    }
}

fn mid_grey(_x: u32, _y: u32) -> [u8; 4] {
    MID_GREY
}

fn white(_x: u32, _y: u32) -> [u8; 4] {
    WHITE
}

fn saturated(_x: u32, _y: u32) -> [u8; 4] {
    SATURATED
}

/// A 4 px checker of two greys.
fn checker4(x: u32, y: u32) -> [u8; 4] {
    if (x / 4 + y / 4).is_multiple_of(2) {
        CHECKER_LO
    } else {
        CHECKER_HI
    }
}

fn disc(x: u32, y: u32, cx: u32, cy: u32, r: u32) -> bool {
    let dx = x as i64 - cx as i64;
    let dy = y as i64 - cy as i64;
    dx * dx + dy * dy <= (r * r) as i64
}

/// Light grey with a dark blemish of radius 4 at `(cx, cy)`.
fn spot_at(cx: u32, cy: u32) -> impl Fn(u32, u32) -> [u8; 4] {
    move |x, y| if disc(x, y, cx, cy, 4) { DARK } else { GREY }
}

/// Light grey with a flash-red pupil of radius 8 at the centre.
fn red_eye(x: u32, y: u32) -> [u8; 4] {
    if disc(x, y, 64, 64, 8) {
        PUPIL
    } else {
        GREY
    }
}

/// White with a blue wall at x in 60..68 splitting it into two white regions.
fn wall(x: u32, _y: u32) -> [u8; 4] {
    if (60..68).contains(&x) {
        BLUE
    } else {
        WHITE
    }
}

/// White with a black square over 40..88 on both axes — a strong edge for
/// the magnetic lasso to snap onto.
fn black_square(x: u32, y: u32) -> [u8; 4] {
    if (40..88).contains(&x) && (40..88).contains(&y) {
        BLACK
    } else {
        WHITE
    }
}

/// Left half dark red, right half white.
fn dark_red_left(x: u32, _y: u32) -> [u8; 4] {
    if x < 64 {
        DARK_RED
    } else {
        WHITE
    }
}

// ---------------------------------------------------------------- harness --

/// A shell editor over one `W × H` document whose pixels are `fixture`,
/// baked through the real command route. The tempdir must outlive the editor.
fn open(fixture: &dyn Fn(u32, u32) -> [u8; 4]) -> (tempfile::TempDir, Editor) {
    let dir = tempfile::tempdir().expect("a tempdir");
    let mut ed = app::shell_editor(dir.path(), W, H);
    let layer = app::the_opened_layer(&ed);
    ed.active_mut()
        .expect("one document")
        .paint_canvas(layer, fixture);
    (dir, ed)
}

/// Select a tool the way a palette click does: the intent resolves through
/// `menu_bridge::pick`, is recorded into the frame's `ChromeOutput`, and the
/// shell applies `select_tool` to the editor (`shell.rs` apply_chrome).
fn select_tool(ed: &mut Editor, id: ToolId) {
    let pick = menu_bridge::pick(&ui::Intent::SelectTool(id), ed)
        .unwrap_or_else(|| panic!("{id:?}: Intent::SelectTool did not route to a Pick"));
    let mut out = ChromeOutput::default();
    menu_bridge::record(pick, &mut out);
    let tool = out
        .select_tool
        .unwrap_or_else(|| panic!("{id:?}: the pick did not record select_tool: {out:?}"));
    ed.set_tool(tool);
    assert_eq!(ed.tool(), id, "the palette intent did not select {id:?}");
}

fn composite(ed: &mut Editor) -> Vec<u8> {
    ed.active_mut().expect("one document").composite_all()
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

fn depth(ed: &Editor) -> usize {
    ed.active().expect("one document").history_depth()
}

fn selection(ed: &Editor) -> Selection {
    ed.active()
        .expect("one document")
        .document
        .selection
        .clone()
}

fn cov(ed: &Editor, x: i32, y: i32) -> f32 {
    selection(ed).coverage_at(IVec2::new(x, y))
}

/// Edit ▸ Undo through the editor's own action route.
fn undo(ed: &mut Editor) {
    ed.dispatch(Action::Undo).expect("undo is available");
}

fn v(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

fn drag(pointer: &mut ToolPointer, ed: &mut Editor, pts: &[Vec2]) -> Vec<PointerOutcome> {
    app::shell_stroke(pointer, ed, pts)
}

fn click(pointer: &mut ToolPointer, ed: &mut Editor, at: Vec2) -> Vec<PointerOutcome> {
    app::shell_click(pointer, ed, at)
}

fn alt_click(pointer: &mut ToolPointer, ed: &mut Editor, at: Vec2) -> Vec<PointerOutcome> {
    app::shell_click_with(pointer, ed, at, Modifiers::alt())
}

/// Every sample of a gesture reached the tool and none was refused.
fn all_reached(id: ToolId, outcomes: &[PointerOutcome]) {
    for o in outcomes {
        assert!(
            o.reached_tool,
            "{id:?}: a sample did not reach the tool: {o:?}"
        );
        assert_eq!(o.failed, None, "{id:?}: the tool refused a sample: {o:?}");
    }
}

/// The document pixels a gesture changed, as `(x, y)`.
fn changed(before: &[u8], after: &[u8]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            if px(before, x, y) != px(after, x, y) {
                out.push((x, y));
            }
        }
    }
    out
}

/// Distance from a pixel centre to the segment `a`–`b`.
fn dist_to_segment(p: (u32, u32), a: Vec2, b: Vec2) -> f32 {
    let p = Vec2::new(p.0 as f32 + 0.5, p.1 as f32 + 0.5);
    let ab = b - a;
    let t = if ab.length_squared() < 1e-6 {
        0.0
    } else {
        ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    };
    (p - (a + ab * t)).length()
}

/// Every changed pixel lies within `radius` of the stroke's segment — the
/// "changes pixels only inside the stroke" half of a retouch tool's contract.
fn changed_only_within(id: ToolId, before: &[u8], after: &[u8], a: Vec2, b: Vec2, radius: f32) {
    let moved = changed(before, after);
    assert!(!moved.is_empty(), "{id:?}: the gesture changed no pixel");
    let stray: Vec<_> = moved
        .iter()
        .copied()
        .filter(|p| dist_to_segment(*p, a, b) > radius)
        .collect();
    assert!(
        stray.is_empty(),
        "{id:?}: {} pixel(s) changed outside the stroke's {radius} px reach, e.g. {:?}",
        stray.len(),
        &stray[..stray.len().min(5)]
    );
}

/// Population variance of the red channel over the half-open window.
fn variance(buf: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> f64 {
    let mut n = 0.0;
    let mut sum = 0.0;
    let mut sq = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            let r = px(buf, x, y)[0] as f64;
            n += 1.0;
            sum += r;
            sq += r * r;
        }
    }
    let mean = sum / n;
    sq / n - mean * mean
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_luminance(p: [u8; 4]) -> f64 {
    0.2126 * srgb_to_linear(p[0]) + 0.7152 * srgb_to_linear(p[1]) + 0.0722 * srgb_to_linear(p[2])
}

fn saturation(p: [u8; 4]) -> i32 {
    let hi = p[0].max(p[1]).max(p[2]) as i32;
    let lo = p[0].min(p[1]).min(p[2]) as i32;
    hi - lo
}

/// Two colours within `tol` on every channel.
fn near(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| x.abs_diff(*y) <= tol)
}

/// A pixel-editing gesture: the composite before, the composite after, one
/// history entry, and the gesture's outcomes for the caller to inspect.
struct PixelRun {
    before: Vec<u8>,
    after: Vec<u8>,
}

/// Select `id`, run `gesture`, and check the route-level contract every pixel
/// tool shares: every sample reached the tool, exactly one history entry
/// landed, and the composite changed. The tool-specific assertions follow in
/// the caller; `undo_restores` closes the loop.
fn run_pixel_route(
    ed: &mut Editor,
    pointer: &mut ToolPointer,
    id: ToolId,
    gesture: impl FnOnce(&mut ToolPointer, &mut Editor) -> Vec<PointerOutcome>,
) -> PixelRun {
    select_tool(ed, id);
    let before = composite(ed);
    let d0 = depth(ed);
    let outcomes = gesture(pointer, ed);
    all_reached(id, &outcomes);
    let steps: usize = outcomes.iter().map(|o| o.steps).sum();
    assert_eq!(
        steps, 1,
        "{id:?}: the gesture reported {steps} history steps, not one"
    );
    assert_eq!(
        depth(ed),
        d0 + 1,
        "{id:?}: one gesture must land as exactly one history entry"
    );
    let after = composite(ed);
    assert_ne!(after, before, "{id:?}: the gesture changed no pixel");
    PixelRun { before, after }
}

/// One Undo restores the composite byte for byte and takes the entry back.
fn undo_restores(ed: &mut Editor, id: ToolId, run: &PixelRun) {
    let d = depth(ed);
    undo(ed);
    assert_eq!(depth(ed), d - 1, "{id:?}: undo did not take one entry back");
    assert_eq!(
        composite(ed),
        run.before,
        "{id:?}: undo did not restore the pixels byte for byte"
    );
}

/// Select `id`, run `gesture`, and check the contract every selection tool
/// shares: every sample reached the tool, exactly one history entry landed
/// (`Command::SetSelection`), the selection changed, and no pixel moved.
fn run_selection_route(
    ed: &mut Editor,
    pointer: &mut ToolPointer,
    id: ToolId,
    gesture: impl FnOnce(&mut ToolPointer, &mut Editor) -> Vec<PointerOutcome>,
) -> Vec<u8> {
    select_tool(ed, id);
    assert_eq!(
        selection(ed),
        Selection::None,
        "{id:?}: the fixture starts unselected"
    );
    let before = composite(ed);
    let d0 = depth(ed);
    let outcomes = gesture(pointer, ed);
    all_reached(id, &outcomes);
    assert!(
        outcomes.iter().any(|o| o.selection_changed),
        "{id:?}: no sample reported a selection change: {outcomes:?}"
    );
    assert_eq!(
        depth(ed),
        d0 + 1,
        "{id:?}: one selection gesture must land as exactly one history entry"
    );
    assert_ne!(
        selection(ed),
        Selection::None,
        "{id:?}: nothing was selected"
    );
    assert_eq!(
        composite(ed),
        before,
        "{id:?}: a selection gesture moved pixels"
    );
    before
}

/// One Undo clears the selection again and leaves the pixels alone.
fn undo_restores_selection(ed: &mut Editor, id: ToolId, before: &[u8]) {
    let d = depth(ed);
    undo(ed);
    assert_eq!(depth(ed), d - 1, "{id:?}: undo did not take one entry back");
    assert_eq!(
        selection(ed),
        Selection::None,
        "{id:?}: undo did not restore the empty selection"
    );
    assert_eq!(composite(ed), before, "{id:?}: undo moved pixels");
}

// ------------------------------------------------------- selection tools --

#[test]
fn rect_marquee_selects_exactly_the_dragged_box() {
    let id = ToolId::RectMarquee;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[v(32.0, 32.0), v(64.0, 64.0), v(96.0, 96.0)])
    });
    assert_eq!(cov(&ed, 32, 32), 1.0, "the box's first pixel");
    assert_eq!(cov(&ed, 95, 95), 1.0, "the box's last pixel");
    assert_eq!(cov(&ed, 31, 64), 0.0, "one column left of the box");
    assert_eq!(
        cov(&ed, 96, 64),
        0.0,
        "the column the drag ended on is outside"
    );
    assert_eq!(cov(&ed, 64, 31), 0.0, "one row above");
    assert_eq!(
        cov(&ed, 64, 96),
        0.0,
        "the row the drag ended on is outside"
    );
    undo_restores_selection(&mut ed, id, &before);
}

#[test]
fn ellipse_marquee_selects_the_inscribed_ellipse_of_the_drag() {
    let id = ToolId::EllipseMarquee;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[v(32.0, 32.0), v(64.0, 64.0), v(96.0, 96.0)])
    });
    assert_eq!(
        cov(&ed, 64, 64),
        1.0,
        "the centre of the ellipse is selected"
    );
    assert_eq!(
        cov(&ed, 64, 36),
        1.0,
        "a point inside the ellipse near its top rim"
    );
    assert_eq!(
        cov(&ed, 33, 33),
        0.0,
        "the drag box's corner lies outside the ellipse and must not be selected"
    );
    assert_eq!(cov(&ed, 10, 10), 0.0, "outside the drag box");
    undo_restores_selection(&mut ed, id, &before);
}

#[test]
fn single_row_marquee_selects_exactly_the_clicked_row() {
    let id = ToolId::SingleRowMarquee;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| {
        click(p, ed, v(10.5, 40.5))
    });
    assert_eq!(cov(&ed, 0, 40), 1.0, "row 40 is selected at the left edge");
    assert_eq!(
        cov(&ed, 127, 40),
        1.0,
        "row 40 is selected at the right edge"
    );
    assert_eq!(cov(&ed, 64, 39), 0.0, "the row above is not");
    assert_eq!(cov(&ed, 64, 41), 0.0, "the row below is not");
    undo_restores_selection(&mut ed, id, &before);
}

#[test]
fn single_column_marquee_selects_exactly_the_clicked_column() {
    let id = ToolId::SingleColumnMarquee;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| {
        click(p, ed, v(40.5, 10.5))
    });
    assert_eq!(cov(&ed, 40, 0), 1.0, "column 40 is selected at the top");
    assert_eq!(
        cov(&ed, 40, 127),
        1.0,
        "column 40 is selected at the bottom"
    );
    assert_eq!(cov(&ed, 39, 64), 0.0, "the column to the left is not");
    assert_eq!(cov(&ed, 41, 64), 0.0, "the column to the right is not");
    undo_restores_selection(&mut ed, id, &before);
}

/// The corners of a 20..100 square, closed, as a drag path.
fn square_path() -> Vec<Vec2> {
    vec![
        v(20.0, 20.0),
        v(60.0, 20.0),
        v(100.0, 20.0),
        v(100.0, 60.0),
        v(100.0, 100.0),
        v(60.0, 100.0),
        v(20.0, 100.0),
        v(20.0, 60.0),
        v(20.0, 20.0),
    ]
}

fn assert_square_selected(id: ToolId, ed: &Editor) {
    assert_eq!(cov(ed, 60, 60), 1.0, "{id:?}: inside the outline");
    assert_eq!(cov(ed, 25, 95), 1.0, "{id:?}: inside, near a corner");
    assert_eq!(cov(ed, 10, 10), 0.0, "{id:?}: outside, top-left");
    assert_eq!(cov(ed, 110, 60), 0.0, "{id:?}: outside, right");
    assert_eq!(cov(ed, 60, 110), 0.0, "{id:?}: outside, below");
}

#[test]
fn lasso_selects_the_region_the_drag_outlined() {
    let id = ToolId::Lasso;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &square_path())
    });
    assert_square_selected(id, &ed);
    undo_restores_selection(&mut ed, id, &before);
}

#[test]
fn polygonal_lasso_closes_on_the_first_vertex_and_selects_the_polygon() {
    let id = ToolId::PolygonalLasso;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| {
        let mut out = Vec::new();
        for corner in [
            v(20.0, 20.0),
            v(100.0, 20.0),
            v(100.0, 100.0),
            v(20.0, 100.0),
        ] {
            let d = depth(ed);
            out.extend(click(p, ed, corner));
            assert_eq!(
                depth(ed),
                d,
                "a corner click emitted a step before the close"
            );
        }
        // Clicking back within POLYGON_CLOSE_PX of the first vertex closes.
        out.extend(click(p, ed, v(21.0, 21.0)));
        out
    });
    assert_square_selected(id, &ed);
    undo_restores_selection(&mut ed, id, &before);
}

#[test]
fn magnetic_lasso_snaps_the_guide_path_onto_the_image_edge() {
    let id = ToolId::MagneticLasso;
    let (_dir, mut ed) = open(&black_square);
    let mut pointer = ToolPointer::new();
    // A loose guide path 6 px outside the black square's edges (which sit at
    // 40 and 88), sampled every 10 px so the tool keeps sparse anchors.
    let mut path = Vec::new();
    for x in (34..=94).step_by(10) {
        path.push(v(x as f32, 34.0));
    }
    for y in (34..=94).step_by(10) {
        path.push(v(94.0, y as f32));
    }
    for x in (34..=94).rev().step_by(10) {
        path.push(v(x as f32, 94.0));
    }
    for y in (34..=94).rev().step_by(10) {
        path.push(v(34.0, y as f32));
    }
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &path));
    assert_eq!(cov(&ed, 64, 64), 1.0, "the square's interior is selected");
    assert_eq!(
        cov(&ed, 44, 64),
        1.0,
        "inside the square, just past its left edge"
    );
    assert_eq!(cov(&ed, 10, 10), 0.0, "far outside");
    // The magnetic claim: the guide ran at x = 34, but the selection edge is
    // the image edge at x = 40 — a pixel between the two is NOT selected. A
    // freehand fill of the same path would select it.
    assert_eq!(
        cov(&ed, 36, 64),
        0.0,
        "the path did not snap from the guide (x = 34) onto the edge (x = 40)"
    );
    undo_restores_selection(&mut ed, id, &before);
}

#[test]
fn magic_wand_selects_the_contiguous_colour_under_the_click() {
    let id = ToolId::MagicWand;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| {
        click(p, ed, v(32.0, 64.0))
    });
    assert_eq!(cov(&ed, 32, 64), 1.0, "the clicked pixel");
    assert_eq!(
        cov(&ed, 5, 120),
        1.0,
        "the far corner of the same red region"
    );
    assert_eq!(cov(&ed, 63, 64), 1.0, "the last red column");
    assert_eq!(cov(&ed, 64, 64), 0.0, "the first blue column");
    assert_eq!(cov(&ed, 96, 64), 0.0, "the blue half");
    undo_restores_selection(&mut ed, id, &before);
}

#[test]
fn quick_selection_grows_from_the_scrubbed_region_to_its_colour() {
    let id = ToolId::QuickSelect;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[v(20.0, 64.0), v(35.0, 64.0), v(50.0, 64.0)])
    });
    assert_eq!(cov(&ed, 35, 64), 1.0, "under the scrub");
    assert_eq!(
        cov(&ed, 63, 64),
        1.0,
        "the scrub's colour grows to the region's edge"
    );
    assert_eq!(cov(&ed, 64, 64), 0.0, "and stops at the blue");
    assert_eq!(cov(&ed, 96, 64), 0.0, "the blue half is untouched");
    undo_restores_selection(&mut ed, id, &before);
}

/// W11-G: a textured orange disc (radius 26 at 60, 66) on a textured teal
/// ground: an object with a real edge and no flat colour to flood.
fn object_disc(x: u32, y: u32) -> [u8; 4] {
    let t = ((x * 7 + y * 13) % 11) as i32 * 4 - 20;
    let c: [i32; 3] = if in_object_disc(x, y) {
        [220 + t / 2, 130 + t, 40]
    } else {
        [40, 120 + t, 150 - t]
    };
    [
        c[0].clamp(0, 255) as u8,
        c[1].clamp(0, 255) as u8,
        c[2].clamp(0, 255) as u8,
        255,
    ]
}

fn in_object_disc(x: u32, y: u32) -> bool {
    (x as f32 + 0.5 - 60.0).powi(2) + (y as f32 + 0.5 - 66.0).powi(2) < 26.0 * 26.0
}

/// W11-G: the Object Selection tool — a rectangle dragged loosely round the
/// disc runs `select_object` (GrabCut from the rectangle) on the job worker
/// (inline here) and lands the DISC, not the rectangle, as one undoable
/// `SetSelection` step.
#[test]
fn object_selection_selects_the_object_inside_the_dragged_rectangle() {
    let id = ToolId::ObjectSelection;
    let (_dir, mut ed) = open(&object_disc);
    let mut pointer = ToolPointer::new();
    let before = run_selection_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[v(26.0, 32.0), v(60.0, 66.0), v(96.0, 102.0)])
    });
    let (mut inter, mut union) = (0u32, 0u32);
    for y in 0..H {
        for x in 0..W {
            let a = cov(&ed, x as i32, y as i32) >= 0.5;
            let b = in_object_disc(x, y);
            inter += u32::from(a && b);
            union += u32::from(a || b);
        }
    }
    let iou = f64::from(inter) / f64::from(union.max(1));
    assert!(iou >= 0.85, "IoU {iou} against the disc");
    assert_eq!(
        cov(&ed, 30, 36),
        0.0,
        "a corner of the rectangle outside the disc is not selected"
    );
    assert_eq!(ed.status(), Some("Selected the object"));
    undo_restores_selection(&mut ed, id, &before);
}

// --------------------------------------------------------- retouch tools --

#[test]
fn spot_healing_fills_the_blemish_from_its_surround() {
    let id = ToolId::SpotHealing;
    let (_dir, mut ed) = open(&spot_at(64, 64));
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(60.0, 64.0), v(68.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    let centre_before = px(&run.before, 64, 64);
    let centre_after = px(&run.after, 64, 64);
    assert_eq!(centre_before, DARK);
    assert!(
        centre_after[0] > centre_before[0] + 60,
        "the blemish did not blend toward its surround: {centre_before:?} -> {centre_after:?}"
    );
    assert_eq!(
        px(&run.after, 64, 20),
        GREY,
        "outside the stroke is untouched"
    );
    // Brush 30 px: nothing beyond its 15 px radius (plus antialias) moves.
    changed_only_within(id, &run.before, &run.after, a, b, 16.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn healing_brush_takes_texture_from_the_alt_set_source_and_colour_from_the_surround() {
    let id = ToolId::HealingBrush;
    let (_dir, mut ed) = open(&spot_at(64, 64));
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    // Alt-click sets the source on clean grey and commits nothing.
    let d0 = depth(&ed);
    let src = alt_click(&mut pointer, &mut ed, v(24.0, 64.0));
    all_reached(id, &src);
    assert_eq!(depth(&ed), d0, "setting the source is not an edit");
    let (a, b) = (v(60.0, 64.0), v(68.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    let centre_after = px(&run.after, 64, 64);
    assert!(
        centre_after[0] > DARK[0] + 60,
        "the blemish did not heal toward the grey: {centre_after:?}"
    );
    assert_eq!(px(&run.after, 24, 64), GREY, "the source is only read");
    changed_only_within(id, &run.before, &run.after, a, b, 21.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn patch_lassoes_a_region_then_heals_it_from_where_it_is_dragged_to() {
    let id = ToolId::Patch;
    let (_dir, mut ed) = open(&spot_at(40, 64));
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    // Gesture one: outline the blemish. Nothing is committed yet.
    let d0 = depth(&ed);
    let outline = drag(
        &mut pointer,
        &mut ed,
        &[
            v(28.0, 52.0),
            v(52.0, 52.0),
            v(52.0, 76.0),
            v(28.0, 76.0),
            v(28.0, 52.0),
        ],
    );
    all_reached(id, &outline);
    assert_eq!(depth(&ed), d0, "drawing the outline is not an edit");
    // Gesture two: drag the region onto clean pixels 50 px to the right.
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[v(40.0, 64.0), v(65.0, 64.0), v(90.0, 64.0)])
    });
    let centre_after = px(&run.after, 40, 64);
    assert!(
        centre_after[0] > DARK[0] + 60,
        "the patched region did not heal: {centre_after:?}"
    );
    // Only the outlined region moves — nothing under the drag path itself.
    let stray: Vec<_> = changed(&run.before, &run.after)
        .into_iter()
        .filter(|&(x, y)| !(28..=53).contains(&x) || !(52..=77).contains(&y))
        .collect();
    assert!(
        stray.is_empty(),
        "pixels outside the outline changed: {stray:?}"
    );
    assert_eq!(px(&run.after, 90, 64), GREY, "the source is only read");
    undo_restores(&mut ed, id, &run);
}

#[test]
fn red_eye_desaturates_the_flash_red_inside_the_box_and_nothing_else() {
    let id = ToolId::RedEye;
    let (_dir, mut ed) = open(&red_eye);
    let mut pointer = ToolPointer::new();
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[v(50.0, 50.0), v(78.0, 78.0)])
    });
    let pupil = px(&run.after, 64, 64);
    assert!(pupil[0] < 120, "the red did not drop: {pupil:?}");
    assert!(
        pupil[0].abs_diff(pupil[1]) <= 12 && pupil[0].abs_diff(pupil[2]) <= 12,
        "the pupil is not grey: {pupil:?}"
    );
    assert_eq!(pupil[3], 255);
    assert_eq!(
        px(&run.after, 52, 52),
        GREY,
        "grey inside the box is not red, so untouched"
    );
    assert_eq!(px(&run.after, 20, 20), GREY, "outside the box");
    // Every changed pixel is a pupil pixel.
    for (x, y) in changed(&run.before, &run.after) {
        assert!(
            disc(x, y, 64, 64, 8),
            "a non-pupil pixel changed at ({x}, {y})"
        );
    }
    undo_restores(&mut ed, id, &run);
}

#[test]
fn colour_replacement_recolours_to_the_foreground_and_keeps_luminance() {
    let id = ToolId::ColorReplacement;
    let (_dir, mut ed) = open(&dark_red_left);
    ed.set_foreground([0.0, 0.0, 1.0, 1.0]);
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(30.0, 64.0), v(40.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    let before = px(&run.before, 35, 64);
    let after = px(&run.after, 35, 64);
    assert_eq!(before, DARK_RED);
    assert!(
        after[2] > 150 && after[0] < 20,
        "the hue did not move to the foreground blue: {after:?}"
    );
    let (lb, la) = (linear_luminance(before), linear_luminance(after));
    assert!(
        (la - lb).abs() < lb * 0.05,
        "luminance was not kept: {lb:.4} -> {la:.4}"
    );
    assert_eq!(px(&run.after, 35, 20), DARK_RED, "outside the stroke");
    changed_only_within(id, &run.before, &run.after, a, b, 16.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn clone_stamp_copies_from_the_alt_set_source_under_the_stroke() {
    let id = ToolId::CloneStamp;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    let src = alt_click(&mut pointer, &mut ed, v(32.0, 64.0));
    all_reached(id, &src);
    assert_eq!(depth(&ed), d0, "setting the source is not an edit");
    let (a, b) = (v(96.0, 60.0), v(96.0, 68.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    let cloned = px(&run.after, 96, 64);
    assert!(
        cloned[0] > 200 && cloned[2] < 50,
        "the blue under the stroke was not replaced by the red source: {cloned:?}"
    );
    assert_eq!(px(&run.after, 32, 64), RED, "the source is only read");
    assert_eq!(px(&run.after, 96, 10), BLUE, "outside the stroke");
    changed_only_within(id, &run.before, &run.after, a, b, 21.0);
    undo_restores(&mut ed, id, &run);
}

/// Edit ▸ Define Pattern over a 2 × 2 red/blue checker baked into the
/// top-left corner — the real route the menu item takes. Leaves no selection.
fn define_checker_pattern(ed: &mut Editor) {
    let layer = app::the_opened_layer(ed);
    ed.active_mut().unwrap().paint_canvas(layer, &|x, y| {
        if x < 2 && y < 2 {
            if (x + y).is_multiple_of(2) {
                RED
            } else {
                BLUE
            }
        } else {
            WHITE
        }
    });
    app::set_selection(
        ed,
        Selection::Rect {
            min: IVec2::new(0, 0),
            max: IVec2::new(2, 2),
        },
    );
    let status = ed
        .define_pattern_from_selection()
        .expect("Define Pattern accepts the 2 x 2 selection");
    assert!(status.contains("2 2"), "{status}");
    app::set_selection(ed, Selection::None);
    assert!(
        ed.active_tool_pattern().is_some(),
        "the preset reaches the tools"
    );
}

/// The checker colour the defined pattern puts at a document pixel.
fn checker_at(x: u32, y: u32) -> [u8; 4] {
    if (x % 2 + y % 2).is_multiple_of(2) {
        RED
    } else {
        BLUE
    }
}

#[test]
fn pattern_stamp_paints_the_defined_pattern_under_the_stroke() {
    let id = ToolId::PatternStamp;
    let (_dir, mut ed) = open(&white);
    define_checker_pattern(&mut ed);
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(64.0, 60.0), v(64.0, 68.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    for (x, y) in [(64, 64), (65, 64), (64, 65), (65, 65), (60, 62), (67, 66)] {
        let got = px(&run.after, x, y);
        assert!(
            near(got, checker_at(x, y), 2),
            "({x}, {y}) is {got:?}, the pattern says {:?}",
            checker_at(x, y)
        );
    }
    assert_eq!(px(&run.after, 64, 10), WHITE, "outside the stroke");
    changed_only_within(id, &run.before, &run.after, a, b, 21.0);
    undo_restores(&mut ed, id, &run);
}

/// W13X-6: Photopea's Background Eraser starts on Sampling: Continuous, so
/// every dab samples the colour under its own centre and a stroke started on
/// red that crosses into blue erases both. The options bar shows that default
/// and the tool the palette hands over paints with it.
#[test]
fn background_eraser_by_default_erases_every_colour_it_crosses() {
    let id = ToolId::BackgroundEraser;
    let chrome = Chrome::new();
    assert_eq!(
        chrome
            .workspace()
            .options
            .get(id, tools::stroke_options::SAMPLING_KEY),
        Some(ui::OptionValue::Choice(0)),
        "the options bar does not start Sampling on Continuous"
    );
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    // Start on red, cross into blue.
    let (a, b) = (v(56.0, 64.0), v(72.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    assert_eq!(
        px(&run.after, 50, 64)[3],
        0,
        "red under the stroke is erased"
    );
    assert_eq!(
        px(&run.after, 80, 64)[3],
        0,
        "blue under the stroke is erased too"
    );
    assert_eq!(
        px(&run.after, 10, 64),
        RED,
        "red outside the stroke is kept"
    );
    assert_eq!(
        px(&run.after, 120, 64),
        BLUE,
        "blue outside the stroke is kept"
    );
    changed_only_within(id, &run.before, &run.after, a, b, 21.0);
    undo_restores(&mut ed, id, &run);
}

/// W13X-6: Sampling: Once, chosen in the options bar, samples the colour the
/// stroke first touches and keeps every other colour it crosses.
#[test]
fn background_eraser_on_once_clears_only_the_colour_first_touched() {
    let id = ToolId::BackgroundEraser;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let mut chrome = Chrome::new();
    chrome.set_tool_choice(id, tools::stroke_options::SAMPLING_KEY, 1);
    let seed: Vec<(String, tools::ToolSetting)> = chrome
        .tool_options(id)
        .into_iter()
        .map(|(key, value)| {
            let setting = match value {
                ui::OptionValue::Float(v) => tools::ToolSetting::Float(v),
                ui::OptionValue::Int(v) => tools::ToolSetting::Int(v),
                ui::OptionValue::Bool(v) => tools::ToolSetting::Bool(v),
                ui::OptionValue::Choice(v) => tools::ToolSetting::Choice(v),
                ui::OptionValue::Color(v) => tools::ToolSetting::Color(v),
            };
            (key, setting)
        })
        .collect();
    assert!(
        seed.iter()
            .any(|(k, s)| k == tools::stroke_options::SAMPLING_KEY
                && *s == tools::ToolSetting::Choice(1)),
        "the options bar does not forward Sampling: Once: {seed:?}"
    );
    // Start on red, cross into blue.
    let (a, b) = (v(56.0, 64.0), v(72.0, 64.0));
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let outcomes = seeded_stroke(&mut pointer, &mut ed, &[a, b], &seed);
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one stroke, one history entry");
    let run = PixelRun {
        before,
        after: composite(&mut ed),
    };
    assert_eq!(
        px(&run.after, 50, 64)[3],
        0,
        "red under the stroke is erased"
    );
    assert_eq!(
        px(&run.after, 80, 64),
        BLUE,
        "blue under the stroke is kept"
    );
    assert_eq!(
        px(&run.after, 10, 64),
        RED,
        "red outside the stroke is kept"
    );
    for (x, _) in changed(&run.before, &run.after) {
        assert!(x < 64, "a blue pixel changed at x = {x}");
    }
    changed_only_within(id, &run.before, &run.after, a, b, 21.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn magic_eraser_clears_the_contiguous_colour_under_the_click() {
    let id = ToolId::MagicEraser;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        click(p, ed, v(32.0, 64.0))
    });
    assert_eq!(px(&run.after, 32, 64)[3], 0, "the clicked red is gone");
    assert_eq!(
        px(&run.after, 5, 120)[3],
        0,
        "the whole contiguous red region is gone"
    );
    assert_eq!(
        px(&run.after, 64, 64),
        BLUE,
        "the first blue column is kept"
    );
    assert_eq!(px(&run.after, 96, 64), BLUE, "the blue half is kept");
    undo_restores(&mut ed, id, &run);
}

// ------------------------------------------------------------ fill tools --

#[test]
fn gradient_paints_the_ramp_from_the_drag_start_to_its_end() {
    let id = ToolId::Gradient;
    let (_dir, mut ed) = open(&white);
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(0.0, 64.0), v(127.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[a, v(64.0, 64.0), b])
    });
    // The default ramp is foreground black to background white, opaque,
    // interpolated in LINEAR light (`GradientRamp::sample`) and dithered by
    // under half an encoded level. So every pixel of every row, not just the
    // drag's own, is the sRGB encoding of `t` within one level, and it is
    // neutral and opaque. The white the fixture held must not show through
    // anywhere: an opaque ramp covers it completely.
    for y in [0, 5, 64, 127] {
        for x in 0..W {
            let t = ((x as f32 + 0.5 - a.x) / (b.x - a.x)).clamp(0.0, 1.0);
            let ideal = linear_to_srgb8(t);
            let got = px(&run.after, x, y);
            assert!(
                (got[0] as f32 - ideal).abs() <= 1.0,
                "({x}, {y}) is {got:?}; the linear-light ramp puts {ideal:.2} there"
            );
            assert_eq!(got[0], got[1], "({x}, {y}) is not neutral: {got:?}");
            assert_eq!(got[1], got[2], "({x}, {y}) is not neutral: {got:?}");
            assert_eq!(got[3], 255, "({x}, {y}) is not opaque: {got:?}");
        }
    }
    undo_restores(&mut ed, id, &run);
}

/// A linear-light value in `0..=1`, sRGB-encoded onto the 0..=255 scale.
fn linear_to_srgb8(l: f32) -> f32 {
    let e = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    e * 255.0
}

#[test]
fn paint_bucket_fills_only_the_contiguous_region_under_the_click() {
    let id = ToolId::PaintBucket;
    let (_dir, mut ed) = open(&wall);
    ed.set_foreground([0.0, 1.0, 0.0, 1.0]);
    let mut pointer = ToolPointer::new();
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        click(p, ed, v(20.0, 64.0))
    });
    let filled = px(&run.after, 20, 64);
    assert!(
        filled[1] >= 250 && filled[0] <= 5 && filled[2] <= 5,
        "the clicked region is not the foreground green: {filled:?}"
    );
    assert_eq!(
        px(&run.after, 5, 5),
        filled,
        "the whole left region is filled"
    );
    assert_eq!(px(&run.after, 59, 127), filled, "up to the wall");
    assert_eq!(px(&run.after, 64, 64), BLUE, "the wall is kept");
    assert_eq!(
        px(&run.after, 100, 64),
        WHITE,
        "the disconnected white region is kept"
    );
    for (x, _) in changed(&run.before, &run.after) {
        assert!(x < 60, "a pixel at or past the wall changed at x = {x}");
    }
    undo_restores(&mut ed, id, &run);
}

#[test]
fn pattern_fill_tiles_the_defined_pattern_over_the_canvas() {
    let id = ToolId::PatternFill;
    let (_dir, mut ed) = open(&white);
    define_checker_pattern(&mut ed);
    let mut pointer = ToolPointer::new();
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        click(p, ed, v(64.0, 64.0))
    });
    for (x, y) in [
        (0, 0),
        (1, 0),
        (0, 1),
        (64, 64),
        (65, 64),
        (127, 127),
        (126, 127),
    ] {
        let got = px(&run.after, x, y);
        assert!(
            near(got, checker_at(x, y), 2),
            "({x}, {y}) is {got:?}, the tiled pattern says {:?}",
            checker_at(x, y)
        );
    }
    undo_restores(&mut ed, id, &run);
}

// --------------------------------------------------------- tone/focus tools --

#[test]
fn blur_lowers_the_local_variance_under_the_stroke() {
    let id = ToolId::Blur;
    let (_dir, mut ed) = open(&checker4);
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(56.0, 64.0), v(72.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    let before = variance(&run.before, 56, 56, 72, 72);
    let after = variance(&run.after, 56, 56, 72, 72);
    assert!(
        after < before * 0.7,
        "blur did not lower the local variance: {before:.1} -> {after:.1}"
    );
    assert_eq!(
        px(&run.after, 10, 10),
        checker4(10, 10),
        "outside the stroke"
    );
    changed_only_within(id, &run.before, &run.after, a, b, 21.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn sharpen_raises_the_local_variance_under_the_stroke() {
    let id = ToolId::Sharpen;
    let (_dir, mut ed) = open(&checker4);
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(56.0, 64.0), v(72.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    let before = variance(&run.before, 56, 56, 72, 72);
    let after = variance(&run.after, 56, 56, 72, 72);
    assert!(
        after > before * 1.05,
        "sharpen did not raise the local variance: {before:.1} -> {after:.1}"
    );
    assert_eq!(
        px(&run.after, 10, 10),
        checker4(10, 10),
        "outside the stroke"
    );
    changed_only_within(id, &run.before, &run.after, a, b, 21.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn smudge_drags_the_colour_it_started_on_along_the_stroke() {
    let id = ToolId::Smudge;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    // Start on red and pull rightwards into the blue.
    let (a, b) = (v(56.0, 64.0), v(80.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[a, v(64.0, 64.0), v(72.0, 64.0), b])
    });
    let smeared = px(&run.after, 72, 64);
    assert!(
        smeared[0] > 40,
        "no red was dragged into the blue at (72, 64): {smeared:?}"
    );
    assert!(smeared[0] > px(&run.before, 72, 64)[0]);
    assert_eq!(px(&run.after, 72, 10), BLUE, "outside the stroke");
    assert_eq!(px(&run.after, 20, 64), RED, "behind the stroke start");
    changed_only_within(id, &run.before, &run.after, a, b, 21.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn dodge_lightens_the_midtones_under_the_stroke() {
    let id = ToolId::Dodge;
    let (_dir, mut ed) = open(&mid_grey);
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(54.0, 64.0), v(74.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    let lit = px(&run.after, 64, 64);
    assert!(lit[0] > MID_GREY[0] + 15, "dodge did not lighten: {lit:?}");
    assert_eq!(lit[0], lit[1]);
    assert_eq!(lit[1], lit[2]);
    assert_eq!(lit[3], 255);
    for (x, y) in changed(&run.before, &run.after) {
        assert!(
            px(&run.after, x, y)[0] >= MID_GREY[0],
            "dodge darkened ({x}, {y})"
        );
    }
    assert_eq!(px(&run.after, 10, 10), MID_GREY, "outside the stroke");
    changed_only_within(id, &run.before, &run.after, a, b, 31.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn burn_darkens_the_midtones_under_the_stroke() {
    let id = ToolId::Burn;
    let (_dir, mut ed) = open(&mid_grey);
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(54.0, 64.0), v(74.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    let burnt = px(&run.after, 64, 64);
    assert!(
        burnt[0] < MID_GREY[0] - 15,
        "burn did not darken: {burnt:?}"
    );
    assert_eq!(burnt[0], burnt[1]);
    assert_eq!(burnt[1], burnt[2]);
    assert_eq!(burnt[3], 255);
    for (x, y) in changed(&run.before, &run.after) {
        assert!(
            px(&run.after, x, y)[0] <= MID_GREY[0],
            "burn lightened ({x}, {y})"
        );
    }
    assert_eq!(px(&run.after, 10, 10), MID_GREY, "outside the stroke");
    changed_only_within(id, &run.before, &run.after, a, b, 31.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn sponge_desaturates_under_the_stroke() {
    let id = ToolId::Sponge;
    let (_dir, mut ed) = open(&saturated);
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(54.0, 64.0), v(74.0, 64.0));
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &[a, b]));
    let before = px(&run.before, 64, 64);
    let after = px(&run.after, 64, 64);
    assert!(
        saturation(after) < saturation(before) - 10,
        "the sponge did not change saturation: {before:?} -> {after:?}"
    );
    assert_eq!(after[3], 255);
    for (x, y) in changed(&run.before, &run.after) {
        assert!(
            saturation(px(&run.after, x, y)) <= saturation(SATURATED),
            "the sponge saturated ({x}, {y}) in Desaturate mode"
        );
    }
    assert_eq!(px(&run.after, 10, 10), SATURATED, "outside the stroke");
    changed_only_within(id, &run.before, &run.after, a, b, 31.0);
    undo_restores(&mut ed, id, &run);
}

// ------------------------------------------------------------ smoke rows --
//
// The tools below either have real-route tests elsewhere in this crate
// (Move, Brush, Eraser, FreeTransform) or are covered here by one row each
// that pins the route reaches them and the gesture's headline observable.

#[test]
fn hand_pans_the_view_and_edits_nothing() {
    let id = ToolId::Hand;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let centre = ed.active().unwrap().camera.center;
    let d0 = depth(&ed);
    let outcomes = drag(&mut pointer, &mut ed, &[v(64.0, 64.0), v(84.0, 74.0)]);
    for o in &outcomes {
        assert_eq!(o.route, Some(Route::Pan), "{o:?}");
        assert_eq!(o.failed, None);
    }
    assert!(outcomes.iter().any(|o| o.view_changed), "{outcomes:?}");
    assert_ne!(
        ed.active().unwrap().camera.center,
        centre,
        "the view did not move"
    );
    assert_eq!(depth(&ed), d0, "a pan is not an edit");
}

#[test]
fn zoom_click_zooms_in_and_edits_nothing() {
    let id = ToolId::Zoom;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let zoom = ed.active().unwrap().camera.zoom;
    let d0 = depth(&ed);
    let outcomes = click(&mut pointer, &mut ed, v(64.0, 64.0));
    for o in &outcomes {
        assert_eq!(o.route, Some(Route::Zoom), "{o:?}");
    }
    assert!(outcomes.iter().any(|o| o.view_changed), "{outcomes:?}");
    assert!(
        ed.active().unwrap().camera.zoom > zoom,
        "the view did not zoom in"
    );
    assert_eq!(depth(&ed), d0, "a zoom is not an edit");
}

#[test]
fn rotate_view_routes_to_the_camera_and_edits_nothing() {
    let id = ToolId::RotateView;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    let before = composite(&mut ed);
    let outcomes = drag(&mut pointer, &mut ed, &[v(64.0, 64.0), v(84.0, 74.0)]);
    for o in &outcomes {
        assert_eq!(o.route, Some(Route::RotateView), "{o:?}");
        assert_eq!(o.failed, None);
    }
    assert_eq!(depth(&ed), d0, "rotating the view is not an edit");
    assert_eq!(composite(&mut ed), before);
}

#[test]
fn eyedropper_picks_the_colour_under_the_click_into_the_foreground() {
    let id = ToolId::Eyedropper;
    let (_dir, mut ed) = open(&halves);
    ed.set_foreground([0.0, 0.0, 0.0, 1.0]);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    let outcomes = click(&mut pointer, &mut ed, v(96.0, 64.0));
    all_reached(id, &outcomes);
    assert!(outcomes.iter().any(|o| o.picked.is_some()), "{outcomes:?}");
    let fg = ed.foreground();
    assert!(
        fg[2] > 0.9 && fg[0] < 0.1 && fg[1] < 0.1,
        "picked {fg:?}, not the blue"
    );
    assert_eq!(depth(&ed), d0, "picking a colour is not an edit");
}

#[test]
fn crop_drag_then_enter_resizes_the_canvas_as_one_step_and_undo_restores_it() {
    let id = ToolId::Crop;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let outcomes = drag(&mut pointer, &mut ed, &[v(40.0, 20.0), v(80.0, 60.0)]);
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0, "the drag alone commits nothing");
    assert!(
        pointer.has_pending_commit(),
        "the crop box is not held for Enter"
    );
    let commit = pointer.commit(&mut ed);
    assert!(commit.had_pending);
    assert_eq!(commit.failed, None);
    assert_eq!(
        commit.cropped_to.map(|r| (r.x, r.y, r.width, r.height)),
        Some((40, 20, 40, 40))
    );
    assert_eq!(commit.steps, 1, "a crop is one undoable step: {commit:?}");
    let doc = ed.active().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (40, 40));
    assert_eq!(depth(&ed), d0 + 1);
    // The red/blue split at x = 64 is at x = 24 now.
    let cropped = ed
        .active_mut()
        .unwrap()
        .composite(raster::PixelRect::new(0, 0, 40, 40))
        .unwrap();
    let at = |x: u32, y: u32| {
        let i = ((y * 40 + x) * 4) as usize;
        [cropped[i], cropped[i + 1], cropped[i + 2], cropped[i + 3]]
    };
    assert_eq!(at(23, 20), RED);
    assert_eq!(at(24, 20), BLUE);
    undo(&mut ed);
    let doc = ed.active().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (W, H));
    assert_eq!(composite(&mut ed), before, "undo did not restore the crop");
}

fn layers_of_kind(ed: &Editor, f: impl Fn(&layer_model::LayerKind) -> bool) -> usize {
    let doc = &ed.active().unwrap().document;
    doc.layers
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| doc.layers.get(id))
        .filter(|l| f(&l.kind))
        .count()
}

#[test]
fn type_click_creates_one_text_layer_and_opens_it_for_typing() {
    let id = ToolId::Type;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    let texts = |ed: &Editor| layers_of_kind(ed, |k| matches!(k, layer_model::LayerKind::Text(_)));
    assert_eq!(texts(&ed), 0);
    let outcomes = click(&mut pointer, &mut ed, v(20.0, 30.0));
    all_reached(id, &outcomes);
    assert_eq!(texts(&ed), 1, "a click makes exactly one text layer");
    assert_eq!(depth(&ed), d0 + 1, "the layer is one undoable step");
    assert!(pointer.is_text_editing(), "the run is held open for typing");
    // Escape: a layer this session created is deleted again.
    pointer.text_edit(&mut ed, tools::TextEdit::Cancel);
    assert!(!pointer.is_text_editing());
    assert_eq!(
        texts(&ed),
        0,
        "cancelling removes the layer the click created"
    );
}

#[test]
fn rectangle_shape_drag_creates_one_shape_layer_that_undo_removes() {
    let id = ToolId::Rectangle;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    let shapes =
        |ed: &Editor| layers_of_kind(ed, |k| matches!(k, layer_model::LayerKind::Shape(_)));
    assert_eq!(shapes(&ed), 0);
    let outcomes = drag(
        &mut pointer,
        &mut ed,
        &[v(20.0, 20.0), v(40.0, 40.0), v(60.0, 60.0)],
    );
    all_reached(id, &outcomes);
    assert_eq!(outcomes.iter().map(|o| o.steps).sum::<usize>(), 1);
    assert_eq!(depth(&ed), d0 + 1, "one shape drag is one history entry");
    assert_eq!(shapes(&ed), 1, "the drag made exactly one shape layer");
    undo(&mut ed);
    assert_eq!(shapes(&ed), 0, "undo did not remove the shape layer");
    assert_eq!(depth(&ed), d0);
}

#[test]
fn pen_clicks_then_enter_create_one_shape_layer() {
    let id = ToolId::Pen;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    for at in [v(10.0, 10.0), v(50.0, 10.0), v(50.0, 40.0)] {
        let outcomes = click(&mut pointer, &mut ed, at);
        all_reached(id, &outcomes);
        assert_eq!(depth(&ed), d0, "nothing is emitted while the path is drawn");
    }
    assert!(pointer.has_pending_commit());
    let commit = pointer.commit(&mut ed);
    assert_eq!(commit.failed, None);
    assert_eq!(commit.steps, 1, "{commit:?}");
    assert_eq!(depth(&ed), d0 + 1);
    let shapes =
        |ed: &Editor| layers_of_kind(ed, |k| matches!(k, layer_model::LayerKind::Shape(_)));
    assert_eq!(shapes(&ed), 1, "Enter did not make exactly one shape layer");
    undo(&mut ed);
    assert_eq!(shapes(&ed), 0);
}

#[test]
fn pencil_click_paints_one_hard_black_pixel_as_one_step() {
    let id = ToolId::Pencil;
    let (_dir, mut ed) = open(&white);
    ed.set_foreground([0.0, 0.0, 0.0, 1.0]);
    let mut pointer = ToolPointer::new();
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        click(p, ed, v(64.5, 64.5))
    });
    assert_eq!(
        px(&run.after, 64, 64),
        BLACK,
        "the clicked pixel is not black"
    );
    let moved = changed(&run.before, &run.after);
    assert_eq!(
        moved,
        vec![(64, 64)],
        "a 1 px pencil click touched {moved:?}"
    );
    undo_restores(&mut ed, id, &run);
}

#[test]
fn slice_drag_then_enter_reports_one_slice_and_edits_nothing() {
    let id = ToolId::Slice;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    let before = composite(&mut ed);
    let outcomes = drag(&mut pointer, &mut ed, &[v(10.0, 10.0), v(50.0, 40.0)]);
    all_reached(id, &outcomes);
    let commit = pointer.commit(&mut ed);
    assert!(commit.had_pending, "{commit:?}");
    assert_eq!(commit.slices.len(), 1, "one drag is one slice: {commit:?}");
    // W11-E: slices are one History step each, as in Photoshop, and touch
    // no pixels.
    assert_eq!(commit.steps, 1, "a slice set is one step: {commit:?}");
    assert_eq!(depth(&ed), d0 + 1);
    assert_eq!(composite(&mut ed), before);
}

#[test]
fn refine_boundary_regrades_the_mask_band_and_leaves_the_layer_pixels_alone() {
    let id = ToolId::RefineBoundary;
    let (_dir, mut ed) = open(&halves);
    let layer = app::the_opened_layer(&ed);
    // A mask with a jagged left/right boundary near x = 64, attached and
    // painted through the real command route, then targeted the way the
    // Properties panel's Mask control does.
    ed.active_mut().unwrap().attach_mask(layer);
    ed.active_mut().unwrap().paint_canvas_mask(layer, &|x, y| {
        let edge = 60 + 8 * ((y / 2) % 2);
        if x < edge {
            255
        } else {
            0
        }
    });
    ed.set_edit_target_kind(EditTargetKind::from_focus(true));
    assert!(ed.edit_target_is_mask());
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let layer_before = app::layer_tile_map(&ed, layer);
    let mask_before = app::mask_tile_map(&ed, layer);
    let d0 = depth(&ed);
    let outcomes = drag(
        &mut pointer,
        &mut ed,
        &[v(64.0, 40.0), v(64.0, 64.0), v(64.0, 88.0)],
    );
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one refine stroke is one history entry");
    assert_ne!(
        app::mask_tile_map(&ed, layer),
        mask_before,
        "the stroke did not regrade the mask's coverage"
    );
    assert_eq!(
        app::layer_tile_map(&ed, layer),
        layer_before,
        "the stroke touched the layer's pixels"
    );
    undo(&mut ed);
    assert_eq!(
        app::mask_tile_map(&ed, layer),
        mask_before,
        "undo did not restore the mask"
    );
}

// ------------------------------------------------------------- W4-G tools --

/// The opened layer's own transform, as the document holds it.
fn layer_transform(ed: &Editor, layer: layer_model::LayerId) -> glam::Affine2 {
    ed.active()
        .unwrap()
        .document
        .layers
        .get(layer)
        .expect("the layer exists")
        .transform
}

#[test]
fn ruler_drag_measures_without_editing_then_enter_straightens_the_layer_as_one_step() {
    use ui::canvas::PointerPhase;
    let id = ToolId::Ruler;
    let (_dir, mut ed) = open(&halves);
    let layer = app::the_opened_layer(&ed);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let (a, b) = (v(20.0, 70.0), v(100.0, 54.0));

    // The drag: the shell's readout route shows the line's extent while the
    // button is down, and takes it down on release.
    let down = app::shell_pointer(&mut pointer, &mut ed, PointerPhase::Down, a);
    let moved = app::shell_pointer(&mut pointer, &mut ed, PointerPhase::Move, b);
    let (_, readout) = pointer.live_readout().expect("the ruler shows its extent");
    assert_eq!((readout.width_px, readout.height_px), (80.0, 16.0));
    let up = app::shell_pointer(&mut pointer, &mut ed, PointerPhase::Up, b);
    all_reached(id, &[down, moved, up]);
    assert!(
        pointer.live_readout().is_none(),
        "the label outlived the drag"
    );
    assert_eq!(depth(&ed), d0, "measuring is not an edit");
    assert_eq!(composite(&mut ed), before, "measuring moved pixels");
    assert!(
        pointer.has_pending_commit(),
        "the measurement is not held for Straighten Layer"
    );

    // The held measurement, through the chrome the shell publishes it to: the
    // Info panel's Distance and Angle rows, and the line over the canvas.
    let mut chrome = info_chrome();
    let (frame, painted) = chrome_frame(&mut chrome, &mut pointer, &mut ed);
    let rows = info_tool_rows(&chrome);
    assert_eq!(
        rows,
        vec![
            ("Distance", "81.6 px".to_owned()),
            ("Angle", "11.3°".to_owned())
        ],
        "the Info panel does not show the measurement"
    );
    assert!(
        drawn_info_row(&frame, &painted, "Distance", "81.6 px")
            && drawn_info_row(&frame, &painted, "Angle", "11.3°"),
        "the Info panel did not draw the Distance and Angle rows"
    );
    let zoom = ed.active().unwrap().camera.zoom;
    assert!(
        has_segment(&painted, (b - a) * zoom),
        "the ruler line was not drawn over the canvas"
    );

    // Enter: Straighten Layer — one undoable rotation that lays the measured
    // line level.
    let commit = pointer.commit(&mut ed);
    assert!(commit.had_pending);
    assert_eq!(commit.failed, None, "{commit:?}");
    assert_eq!(commit.steps, 1, "straightening is one step: {commit:?}");
    assert_eq!(depth(&ed), d0 + 1);
    let t = layer_transform(&ed, layer);
    let (a2, b2) = (t.transform_point2(a), t.transform_point2(b));
    assert!(
        (a2.y - b2.y).abs() < 1e-3,
        "the measured line is not level after straightening: {a2:?} {b2:?}"
    );
    assert_ne!(composite(&mut ed), before, "the layer did not turn");
    assert!(
        !pointer.has_pending_commit(),
        "the measurement was consumed"
    );
    chrome_frame(&mut chrome, &mut pointer, &mut ed);
    assert!(
        info_tool_rows(&chrome).is_empty(),
        "the Info rows outlived the measurement"
    );

    undo(&mut ed);
    assert_eq!(depth(&ed), d0);
    assert_eq!(layer_transform(&ed, layer), glam::Affine2::IDENTITY);
    assert_eq!(composite(&mut ed), before, "undo did not restore the layer");
}

/// Publish the pointer's live geometry through the production publisher —
/// the call `shell.rs` makes after every pointer sample — then draw three
/// chrome frames (the first is where egui learns sizes and the posted layout
/// lands) and return every shape the last one painted, `Shape::Vec`s flattened.
fn chrome_frame(
    chrome: &mut Chrome,
    pointer: &mut ToolPointer,
    ed: &mut Editor,
) -> (egui::Context, Vec<egui::Shape>) {
    fn flat(shapes: Vec<egui::Shape>, out: &mut Vec<egui::Shape>) {
        for s in shapes {
            match s {
                egui::Shape::Vec(inner) => flat(inner, out),
                other => out.push(other),
            }
        }
    }
    let geometry = pointer.live_geometry();
    let active = ed.active().map(|d| d.id());
    chrome.publish_tool_geometry(geometry, active);
    let ctx = egui::Context::default();
    app_shell::chrome::install_theme(&ctx, design::Theme::Dark);
    let mut painted = Vec::new();
    for _ in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            chrome.ui(ctx, ed);
        });
        painted.clear();
        flat(
            output.shapes.into_iter().map(|c| c.shape).collect(),
            &mut painted,
        );
    }
    (ctx, painted)
}

/// A chrome with the Info panel on screen, alone in the Minimal layout, the
/// way a user shows it from the Window menu (in the default layout it is a
/// background tab behind the Navigator).
fn info_chrome() -> Chrome {
    let mut chrome = Chrome::new();
    chrome.emit(ui::Intent::ApplyLayout(ui::LayoutId::Minimal));
    chrome.emit(ui::Intent::SetPanelOpen {
        panel: ui::dock::PanelId::Info,
        open: true,
    });
    chrome
}

/// The Info panel's tool rows (Ruler, Colour Sampler) as the chrome holds
/// them — the rows `ui::view::docks` draws after the fixed five.
fn info_tool_rows(chrome: &Chrome) -> Vec<(&'static str, String)> {
    chrome
        .workspace()
        .info
        .tool_readouts()
        .into_iter()
        .map(|r| (r.label, r.value))
        .collect()
}

/// Whether the frame drew the Info panel row `label` reading `value`: the
/// row's value label (`dock::ids::info_value`) was laid out, and a painted
/// text with exactly that value sits inside it.
fn drawn_info_row(
    frame: &egui::Context,
    painted: &[egui::Shape],
    label: &'static str,
    value: &str,
) -> bool {
    let Some(row) = frame.read_response(ui::dock::ids::info_value(label)) else {
        return false;
    };
    painted.iter().any(|s| match s {
        egui::Shape::Text(t) => t.galley.text() == value && row.rect.expand(1.0).contains(t.pos),
        _ => false,
    })
}

/// A painted straight segment whose screen extent is `extent`.
fn has_segment(painted: &[egui::Shape], extent: Vec2) -> bool {
    painted.iter().any(|s| match s {
        egui::Shape::LineSegment { points, .. } => {
            let d = points[1] - points[0];
            (d.x - extent.x).abs() < 0.5 && (d.y - extent.y).abs() < 0.5
        }
        _ => false,
    })
}

/// The centres of every painted circle outline.
fn circle_centres(painted: &[egui::Shape]) -> Vec<egui::Pos2> {
    painted
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Circle(c) => Some(c.center),
            _ => None,
        })
        .collect()
}

/// Whether some two painted circles sit `offset` apart on screen.
fn circles_apart(painted: &[egui::Shape], offset: Vec2) -> bool {
    let centres = circle_centres(painted);
    centres.iter().any(|p| {
        centres.iter().any(|q| {
            let d = *q - *p;
            (d.x - offset.x).abs() < 0.5 && (d.y - offset.y).abs() < 0.5
        })
    })
}

#[test]
fn color_sampler_clicks_place_points_the_info_panel_reads_and_the_canvas_marks() {
    let id = ToolId::ColorSampler;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let points = [v(10.0, 10.0), v(100.0, 10.0), v(64.0, 100.0)];
    for at in points {
        let outcomes = click(&mut pointer, &mut ed, at);
        all_reached(id, &outcomes);
        assert!(
            outcomes.iter().all(|o| o.steps == 0 && o.picked.is_none()),
            "{outcomes:?}"
        );
    }
    assert_eq!(pointer.live_tool(), Some(id));
    assert_eq!(depth(&ed), d0, "placing samplers is not an edit");
    assert_eq!(composite(&mut ed), before);

    // The three points, their colours read off the composite, in the Info
    // panel's #1..#3 rows; a numbered marker for each over the canvas.
    let mut chrome = info_chrome();
    let (frame, painted) = chrome_frame(&mut chrome, &mut pointer, &mut ed);
    assert_eq!(
        info_tool_rows(&chrome),
        vec![
            ("#1", "255, 0, 0 at 10, 10".to_owned()),
            ("#2", "0, 0, 255 at 100, 10".to_owned()),
            ("#3", "0, 0, 255 at 64, 100".to_owned()),
        ],
        "the Info panel does not show the sample points"
    );
    assert!(
        drawn_info_row(&frame, &painted, "#1", "255, 0, 0 at 10, 10")
            && drawn_info_row(&frame, &painted, "#3", "0, 0, 255 at 64, 100"),
        "the Info panel did not draw the sampler rows"
    );
    let zoom = ed.active().unwrap().camera.zoom;
    assert!(
        circles_apart(&painted, (points[1] - points[0]) * zoom)
            && circles_apart(&painted, (points[2] - points[1]) * zoom),
        "the sample points are not marked on the canvas: {:?}",
        circle_centres(&painted)
    );

    // Alt-click takes the first one away; the rows renumber.
    let outcomes = alt_click(&mut pointer, &mut ed, points[0]);
    all_reached(id, &outcomes);
    chrome_frame(&mut chrome, &mut pointer, &mut ed);
    assert_eq!(
        info_tool_rows(&chrome),
        vec![
            ("#1", "0, 0, 255 at 100, 10".to_owned()),
            ("#2", "0, 0, 255 at 64, 100".to_owned()),
        ]
    );
    assert_eq!(depth(&ed), d0);

    // The points are the document's: switch to another tool and use it (the
    // pointer rebuilds its live tool), and the rows and the markers stay.
    select_tool(&mut ed, ToolId::Ruler);
    let outcomes = drag(&mut pointer, &mut ed, &[v(5.0, 120.0), v(40.0, 120.0)]);
    all_reached(ToolId::Ruler, &outcomes);
    assert_eq!(pointer.live_tool(), Some(ToolId::Ruler));
    let (_, painted) = chrome_frame(&mut chrome, &mut pointer, &mut ed);
    let rows = info_tool_rows(&chrome);
    assert!(
        rows.contains(&("#1", "0, 0, 255 at 100, 10".to_owned()))
            && rows.contains(&("#2", "0, 0, 255 at 64, 100".to_owned())),
        "a tool switch lost the sample points: {rows:?}"
    );
    assert!(
        circles_apart(&painted, (points[2] - points[1]) * zoom),
        "a tool switch took the sample markers off the canvas: {:?}",
        circle_centres(&painted)
    );
    // And back: the same points, grabbed rather than re-placed.
    select_tool(&mut ed, id);
    let outcomes = drag(&mut pointer, &mut ed, &[points[2], v(20.0, 100.0)]);
    all_reached(id, &outcomes);
    assert_eq!(
        ed.active().unwrap().samplers(),
        &[v(100.5, 10.5), v(20.5, 100.5)],
        "the second tool instance did not see the first one's points"
    );
}

/// Publish the pointer's geometry, settle three chrome frames on one context
/// (the dock widths land on the second; the rects are read after the third),
/// then press and release the primary button over the centre of the widget
/// marked `id` — the frames a real click produces — and return every frame's
/// output.
fn chrome_click(
    chrome: &mut Chrome,
    pointer: &mut ToolPointer,
    ed: &mut Editor,
    id: egui::Id,
) -> Vec<ChromeOutput> {
    let geometry = pointer.live_geometry();
    let active = ed.active().map(|d| d.id());
    chrome.publish_tool_geometry(geometry, active);
    let ctx = egui::Context::default();
    app_shell::chrome::install_theme(&ctx, design::Theme::Dark);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0));
    let mut frame = |events: Vec<egui::Event>| {
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..Default::default()
        };
        let mut out = ChromeOutput::default();
        let _ = ctx.run(input, |ctx| {
            out = chrome.ui(ctx, ed);
        });
        out
    };
    let mut outs = vec![frame(Vec::new()), frame(Vec::new()), frame(Vec::new())];
    let rect = ctx
        .read_response(id)
        .unwrap_or_else(|| panic!("{id:?} was never drawn"))
        .rect;
    let pos = rect.center();
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    outs.push(frame(vec![egui::Event::PointerMoved(pos), button(true)]));
    outs.push(frame(vec![button(false)]));
    outs.push(frame(Vec::new()));
    outs
}

/// The Ruler options bar's Straighten Layer button, as drawn.
fn straighten_button() -> egui::Id {
    ui::view::ids::tool_option(ToolId::Ruler, "straighten")
}

#[test]
fn ruler_straighten_layer_button_in_the_options_bar_straightens_as_one_step() {
    let id = ToolId::Ruler;
    let (_dir, mut ed) = open(&halves);
    let layer = app::the_opened_layer(&ed);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let mut chrome = info_chrome();

    // With nothing measured the button is there but does nothing.
    let outs = chrome_click(&mut chrome, &mut pointer, &mut ed, straighten_button());
    assert!(
        outs.iter().all(|o| !o.confirm_tool),
        "the button confirmed with no measurement"
    );

    let (a, b) = (v(20.0, 70.0), v(100.0, 54.0));
    let outcomes = drag(&mut pointer, &mut ed, &[a, b]);
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0, "measuring is not an edit");

    // The click: the options bar raises the confirm; the shell performs it
    // through `Chrome::confirm_tool` (its chrome-output step).
    let outs = chrome_click(&mut chrome, &mut pointer, &mut ed, straighten_button());
    assert_eq!(
        outs.iter().filter(|o| o.confirm_tool).count(),
        1,
        "one click, one confirm"
    );
    let commit = chrome.confirm_tool(&mut pointer, &mut ed);
    assert!(commit.had_pending, "{commit:?}");
    assert_eq!(commit.failed, None, "{commit:?}");
    assert_eq!(commit.steps, 1, "straightening is one step: {commit:?}");
    assert_eq!(depth(&ed), d0 + 1);
    let t = layer_transform(&ed, layer);
    let (a2, b2) = (t.transform_point2(a), t.transform_point2(b));
    assert!(
        (a2.y - b2.y).abs() < 1e-3,
        "the measured line is not level: {a2:?} {b2:?}"
    );
    assert!(
        chrome.workspace().info.measure.is_none(),
        "the consumed measurement is still published"
    );
    undo(&mut ed);
    assert_eq!(depth(&ed), d0);
    assert_eq!(composite(&mut ed), before, "undo did not restore the layer");
}

#[test]
fn history_brush_paints_the_opened_state_back_under_the_stroke_as_one_step() {
    let id = ToolId::HistoryBrush;
    // The document opened white; `open` then painted it red|blue as an edit.
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let outcomes = drag(&mut pointer, &mut ed, &[v(20.0, 64.0), v(100.0, 64.0)]);
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one stroke, one step");
    let after = composite(&mut ed);
    for x in [30, 60, 70, 90] {
        assert!(
            near(px(&after, x, 64), WHITE, 2),
            "({x}, 64) was not painted back to the opened white: {:?}",
            px(&after, x, 64)
        );
    }
    assert_eq!(px(&after, 30, 10), RED, "outside the stroke untouched");
    assert_eq!(px(&after, 90, 10), BLUE, "outside the stroke untouched");
    undo(&mut ed);
    assert_eq!(depth(&ed), d0);
    assert_eq!(
        composite(&mut ed),
        before,
        "undo did not restore the stroke"
    );
}

/// The History Brush's options as the shell hands them to every sample:
/// `Chrome::tool_options`, converted at the boundary exactly as `shell.rs`
/// converts them.
fn history_brush_seed(chrome: &Chrome) -> Vec<(String, tools::ToolSetting)> {
    chrome
        .tool_options(ToolId::HistoryBrush)
        .into_iter()
        .map(|(key, value)| {
            let setting = match value {
                ui::OptionValue::Float(v) => tools::ToolSetting::Float(v),
                ui::OptionValue::Int(v) => tools::ToolSetting::Int(v),
                ui::OptionValue::Bool(v) => tools::ToolSetting::Bool(v),
                ui::OptionValue::Choice(v) => tools::ToolSetting::Choice(v),
                ui::OptionValue::Color(v) => tools::ToolSetting::Color(v),
            };
            (key, setting)
        })
        .collect()
}

/// One stroke through `ToolPointer::handle` with the options seed `seed`.
fn seeded_stroke(
    pointer: &mut ToolPointer,
    ed: &mut Editor,
    pts: &[Vec2],
    seed: &[(String, tools::ToolSetting)],
) -> Vec<PointerOutcome> {
    use ui::canvas::PointerPhase;
    let mut out = Vec::new();
    for (i, p) in pts.iter().enumerate() {
        let phase = if i == 0 {
            PointerPhase::Down
        } else {
            PointerPhase::Move
        };
        let pos = app::shell_screen_pt(ed.active().unwrap(), p.x, p.y);
        out.push(pointer.handle(ed, ui::canvas::PointerInput::at(phase, pos), false, seed));
    }
    let last = *pts.last().expect("a stroke has points");
    let pos = app::shell_screen_pt(ed.active().unwrap(), last.x, last.y);
    out.push(pointer.handle(
        ed,
        ui::canvas::PointerInput::at(PointerPhase::Up, pos),
        false,
        seed,
    ));
    out
}

#[test]
fn history_brush_paints_from_the_state_picked_in_the_history_panel() {
    let id = ToolId::HistoryBrush;
    // Row 0: opened white. Row 1: `open` painted it red|blue. Row 2: grey.
    let (_dir, mut ed) = open(&halves);
    let layer = app::the_opened_layer(&ed);
    ed.active_mut().unwrap().paint_canvas(layer, &mid_grey);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    assert_eq!(d0, 2, "the fixture's history");

    // The History panel's source column, row 1: the click sets the brush's
    // source and does not step the document through history.
    let mut chrome = Chrome::new();
    chrome.emit(ui::Intent::ApplyLayout(ui::LayoutId::Minimal));
    chrome.emit(ui::Intent::SetPanelOpen {
        panel: ui::dock::PanelId::History,
        open: true,
    });
    let outs = chrome_click(
        &mut chrome,
        &mut pointer,
        &mut ed,
        ui::view::ids::history_source(1),
    );
    assert!(
        outs.iter().all(|o| o.history_jump.is_none()),
        "the source click stepped through history"
    );
    assert_eq!(depth(&ed), d0);
    assert_eq!(
        chrome
            .workspace()
            .options
            .get(id, tools::history_brush::SOURCE_KEY),
        Some(ui::OptionValue::Int(1)),
        "the click did not set the History Brush source"
    );

    let before = composite(&mut ed);
    let seed = history_brush_seed(&chrome);
    let outcomes = seeded_stroke(
        &mut pointer,
        &mut ed,
        &[v(20.0, 64.0), v(100.0, 64.0)],
        &seed,
    );
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one stroke, one step");
    let after = composite(&mut ed);
    for (x, want) in [(30, RED), (50, RED), (80, BLUE), (95, BLUE)] {
        assert!(
            near(px(&after, x, 64), want, 2),
            "({x}, 64) was not painted from row 1: {:?}",
            px(&after, x, 64)
        );
    }
    assert_eq!(px(&after, 30, 10), MID_GREY, "outside the stroke untouched");
    undo(&mut ed);
    assert_eq!(
        composite(&mut ed),
        before,
        "undo did not restore the stroke"
    );
}

#[test]
fn history_brush_refuses_a_layer_the_source_state_does_not_have() {
    let id = ToolId::HistoryBrush;
    let (_dir, mut ed) = open(&halves);
    // A layer added after the document was opened, with pixels on it.
    let added = {
        let doc = ed.active_mut().unwrap();
        let added = doc.add_layer(layer_model::Layer::raster("Added"));
        doc.paint_canvas(added, &mid_grey);
        added
    };
    ed.set_active_layer(added);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let outcomes = drag(&mut pointer, &mut ed, &[v(20.0, 64.0), v(100.0, 64.0)]);
    let refused = outcomes
        .iter()
        .find_map(|o| o.failed.clone())
        .expect("the stroke was not refused");
    assert!(
        refused.contains("does not contain a corresponding layer"),
        "{refused}"
    );
    assert_eq!(depth(&ed), d0, "a refused stroke made a history entry");
    assert_eq!(composite(&mut ed), before, "the added layer was erased");
}

/// A rectangle shape layer drawn through the real Rectangle route, made the
/// active layer the way a Layers-panel click does, and its path data.
fn rectangle_shape(pointer: &mut ToolPointer, ed: &mut Editor) -> layer_model::LayerId {
    select_tool(ed, ToolId::Rectangle);
    let outcomes = drag(pointer, ed, &[v(20.0, 20.0), v(40.0, 40.0), v(60.0, 60.0)]);
    all_reached(ToolId::Rectangle, &outcomes);
    let doc = &ed.active().unwrap().document;
    let shape = doc
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| {
            doc.layers
                .get(*id)
                .is_some_and(|l| matches!(l.kind, layer_model::LayerKind::Shape(_)))
        })
        .expect("the drag made a shape layer");
    ed.set_active_layer(shape);
    shape
}

fn path_svg(ed: &Editor, layer: layer_model::LayerId) -> String {
    match &ed
        .active()
        .unwrap()
        .document
        .layers
        .get(layer)
        .unwrap()
        .kind
    {
        layer_model::LayerKind::Shape(shape) => shape.path_svg.clone(),
        other => panic!("not a shape layer: {other:?}"),
    }
}

/// The anchors a path's SVG data names: one per move/line/curve command.
fn anchor_commands(svg: &str) -> usize {
    svg.chars()
        .filter(|c| matches!(c, 'M' | 'L' | 'Q' | 'C'))
        .count()
}

/// Select an anchor tool, click once, and check the route contract: the click
/// reached the tool, landed as ONE history entry that rewrote the path, and
/// one Undo put the path back. Returns the edited path.
fn anchor_click(ed: &mut Editor, pointer: &mut ToolPointer, id: ToolId, at: Vec2) -> String {
    let layer = rectangle_shape(pointer, ed);
    let original = path_svg(ed, layer);
    select_tool(ed, id);
    let d0 = depth(ed);
    let outcomes = click(pointer, ed, at);
    all_reached(id, &outcomes);
    assert_eq!(depth(ed), d0 + 1, "{id:?}: one click is one history entry");
    let edited = path_svg(ed, layer);
    assert_ne!(edited, original, "{id:?}: the path did not change");
    undo(ed);
    assert_eq!(
        path_svg(ed, layer),
        original,
        "{id:?}: undo did not restore"
    );
    edited
}

#[test]
fn add_anchor_click_on_the_outline_adds_one_anchor_as_one_step() {
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let layer = rectangle_shape(&mut pointer, &mut ed);
    let before = anchor_commands(&path_svg(&ed, layer));
    undo(&mut ed);
    let edited = anchor_click(&mut ed, &mut pointer, ToolId::AddAnchor, v(40.0, 20.5));
    assert_eq!(anchor_commands(&edited), before + 1, "{edited}");
}

#[test]
fn delete_anchor_click_on_a_corner_removes_it_as_one_step() {
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let layer = rectangle_shape(&mut pointer, &mut ed);
    let before = anchor_commands(&path_svg(&ed, layer));
    undo(&mut ed);
    let edited = anchor_click(&mut ed, &mut pointer, ToolId::DeleteAnchor, v(60.0, 60.0));
    assert_eq!(anchor_commands(&edited), before - 1, "{edited}");
}

#[test]
fn convert_point_click_on_a_corner_grows_handles_as_one_step() {
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let edited = anchor_click(&mut ed, &mut pointer, ToolId::ConvertAnchor, v(60.0, 20.0));
    assert!(
        edited.contains('C'),
        "the corner did not become a curve: {edited}"
    );
}

#[test]
fn pencil_auto_erase_paints_the_background_where_the_stroke_starts_on_the_foreground() {
    let id = ToolId::Pencil;
    let (_dir, mut ed) = open(&white);
    ed.set_foreground([0.0, 0.0, 0.0, 1.0]);
    ed.set_background([1.0, 1.0, 1.0, 1.0]);
    let mut pointer = ToolPointer::new();
    // A black pixel, drawn with the Pencil itself.
    let first = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        click(p, ed, v(64.5, 64.5))
    });
    assert_eq!(px(&first.after, 64, 64), BLACK);
    // Auto Erase on, through the options seed the shell hands every sample.
    let auto_erase = [("auto_erase".to_string(), tools::ToolSetting::Bool(true))];
    let at = |ed: &Editor, x: f32, y: f32| app::shell_screen_pt(ed.active().unwrap(), x, y);
    let d0 = depth(&ed);
    let mut outcomes = Vec::new();
    for phase in [ui::canvas::PointerPhase::Down, ui::canvas::PointerPhase::Up] {
        let pos = at(&ed, 64.5, 64.5);
        outcomes.push(pointer.handle(
            &mut ed,
            ui::canvas::PointerInput::at(phase, pos),
            false,
            &auto_erase,
        ));
    }
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one click, one entry");
    let after = composite(&mut ed);
    assert_eq!(
        px(&after, 64, 64),
        WHITE,
        "a stroke that starts on the foreground paints the background"
    );
    // Started on white, the same option still paints the foreground.
    for phase in [ui::canvas::PointerPhase::Down, ui::canvas::PointerPhase::Up] {
        let pos = at(&ed, 20.5, 20.5);
        pointer.handle(
            &mut ed,
            ui::canvas::PointerInput::at(phase, pos),
            false,
            &auto_erase,
        );
    }
    assert_eq!(px(&composite(&mut ed), 20, 20), BLACK);
}

// ---------------------------------------------------------- completeness --

/// Every tool in the palette is accounted for: tested here, tested by another
/// route test in this crate, or explicitly left to the wave that owns it. A
/// new `ToolId` variant fails this until someone says which.
#[test]
fn every_palette_tool_has_a_real_route_test_or_an_owner() {
    use ToolId::*;
    let here = [
        RectMarquee,
        EllipseMarquee,
        SingleRowMarquee,
        SingleColumnMarquee,
        Lasso,
        PolygonalLasso,
        MagneticLasso,
        MagicWand,
        QuickSelect,
        // W11-G
        ObjectSelection,
        SpotHealing,
        HealingBrush,
        Patch,
        RedEye,
        ColorReplacement,
        CloneStamp,
        PatternStamp,
        BackgroundEraser,
        MagicEraser,
        Gradient,
        PaintBucket,
        PatternFill,
        Blur,
        Sharpen,
        Smudge,
        Dodge,
        Burn,
        Sponge,
        // smoke rows
        Hand,
        Zoom,
        RotateView,
        Eyedropper,
        Crop,
        Type,
        Rectangle,
        Pen,
        Pencil,
        Slice,
        RefineBoundary,
        // W4-G
        Ruler,
        ColorSampler,
        HistoryBrush,
        AddAnchor,
        DeleteAnchor,
        ConvertAnchor,
        // W7-F
        PerspectiveCrop,
        VerticalType,
        HorizontalTypeMask,
        VerticalTypeMask,
        MixerBrush,
        Artboard,
        CurvaturePen,
        FreeformPen,
        // W10-A
        ContentAwareMove,
        SliceSelect,
        Spiral,
        // W10-B
        Note,
    ];
    // Real-route tests in `thumbnail_workflow.rs` / `thumbnail_reproducers.rs`.
    let elsewhere = [Move, Brush, Eraser, FreeTransform];
    // W3-B owns `pen.rs`, `shape.rs`, `path_select.rs` and `registry.rs`; the
    // path-editing tools and the remaining shape kinds ride its route tests.
    let delegated = [
        PathSelect,
        DirectSelection,
        RoundedRectangle,
        Ellipse,
        Polygon,
        Star,
        Line,
        CustomShape,
        // W16-G: the Vector Gradient tool's route test drives the real editor
        // in app-shell's `live_shape` tests.
        VectorGradient,
    ];
    let mut accounted: Vec<ToolId> = Vec::new();
    accounted.extend(here);
    accounted.extend(elsewhere);
    accounted.extend(delegated);
    for id in ToolId::ALL {
        let n = accounted.iter().filter(|t| *t == id).count();
        assert_eq!(
            n, 1,
            "{id:?} is listed {n} times; every tool needs exactly one owner"
        );
    }
    assert_eq!(accounted.len(), ToolId::ALL.len());
    assert!(here.len() >= 26 + 12);
}

// ------------------------------------------- W5-C: selection transforms --
//
// Free Transform and Move with a pixel selection move ONLY the selected
// pixels (Photopea's floating selection), Ctrl+T puts its gizmo up before
// any canvas click, and a committed Ctrl+T hands the palette back to the
// tool it was pressed from. Every step here is the shell's own: the marquee
// through the pointer route, Ctrl+T as the menu pick the keymap chord
// resolves to (`menu_bridge::pick` -> `record` -> `ChromeOutput::menu` ->
// `menu_bridge::perform`), the gizmo drag through `ToolPointer::handle`, and
// Enter as `ToolPointer::commit`.

/// Marquee `(x0, y0)..(x1, y1)` through the Rectangular Marquee's real route.
fn marquee(pointer: &mut ToolPointer, ed: &mut Editor, x0: f32, y0: f32, x1: f32, y1: f32) {
    select_tool(ed, ToolId::RectMarquee);
    let outcomes = drag(pointer, ed, &[v(x0, y0), v(x1, y1)]);
    all_reached(ToolId::RectMarquee, &outcomes);
    assert_eq!(
        selection(ed).bounds(),
        Some((
            IVec2::new(x0 as i32, y0 as i32),
            IVec2::new(x1 as i32, y1 as i32)
        )),
        "the marquee selected the dragged box"
    );
}

/// Ctrl+T: the chord's menu action, taken through the pick/record/perform
/// steps `shell.rs` performs for a menu pick.
fn ctrl_t(ed: &mut Editor) {
    let action = ui::MenuAction::FreeTransform;
    let pick = menu_bridge::pick(&ui::Intent::Action(action), ed)
        .expect("Free Transform resolves to a pick");
    let mut out = ChromeOutput::default();
    menu_bridge::record(pick, &mut out);
    assert_eq!(out.menu, vec![action], "Ctrl+T is performed as a menu pick");
    for action in out.menu {
        menu_bridge::perform(action, ed).expect("Free Transform performs");
    }
    assert_eq!(ed.tool(), ToolId::FreeTransform);
}

/// The live transform session's source box and quad, if one is published.
fn transform_geometry(pointer: &mut ToolPointer) -> Option<tools::transform::TransformState> {
    match pointer.live_geometry()?.1 {
        tools::SessionGeometry::Transform { state, .. } => Some(state),
        _ => None,
    }
}

fn in_box(x: u32, y: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> bool {
    (x0..x1).contains(&x) && (y0..y1).contains(&y)
}

/// The no-pointer-sample half (Ctrl+T from the keyboard, a Transform menu
/// item, Show Transform Controls) is proven on the shell itself, which
/// begins the session in the frame that performed the pick:
/// `app-shell`'s `shell::w5c_tests`. This is the pointer half: the first
/// sample the pointer sees, a bare hover with no button, begins it.
#[test]
fn ctrl_t_frames_the_selection_or_the_ink_on_the_first_hover() {
    let (_dir, mut ed) = open(&checker4);
    let mut pointer = ToolPointer::new();
    marquee(&mut pointer, &mut ed, 20.0, 20.0, 40.0, 40.0);
    // A fresh pointer: nothing it has seen could have begun a session.
    let mut pointer = ToolPointer::new();
    ctrl_t(&mut ed);
    let doc = ed.active().unwrap();
    let hover = app::shell_screen_pt(doc, 90.0, 90.0);
    pointer.handle(
        &mut ed,
        ui::canvas::PointerInput::at(ui::canvas::PointerPhase::Move, hover),
        false,
        &[],
    );
    let state =
        transform_geometry(&mut pointer).expect("Ctrl+T published its gizmo on a hover, no click");
    assert_eq!(
        state.source,
        raster::PixelRect::new(20, 20, 20, 20),
        "the gizmo frames the selection"
    );
    // Without a selection the gizmo frames the layer's ink, still with no
    // click.
    app::set_selection(&mut ed, Selection::None);
    select_tool(&mut ed, ToolId::Brush);
    let mut pointer = ToolPointer::new();
    ctrl_t(&mut ed);
    // A hover (no button) is enough for the real route.
    let doc = ed.active().unwrap();
    let hover = app::shell_screen_pt(doc, 5.0, 5.0);
    pointer.handle(
        &mut ed,
        ui::canvas::PointerInput::at(ui::canvas::PointerPhase::Move, hover),
        false,
        &[],
    );
    let state = transform_geometry(&mut pointer).expect("a hover shows the Ctrl+T gizmo");
    assert_eq!(state.source, raster::PixelRect::new(0, 0, W, H));
}

#[test]
fn free_transform_with_a_selection_scales_only_the_selected_pixels() {
    let (_dir, mut ed) = open(&checker4);
    let mut pointer = ToolPointer::new();
    marquee(&mut pointer, &mut ed, 20.0, 20.0, 40.0, 40.0);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    ctrl_t(&mut ed);
    // Scale 2x: the bottom-right corner handle from (40, 40) to (60, 60),
    // anchored on the opposite corner.
    let outcomes = drag(
        &mut pointer,
        &mut ed,
        &[v(40.0, 40.0), v(50.0, 50.0), v(60.0, 60.0)],
    );
    all_reached(ToolId::FreeTransform, &outcomes);
    let state = transform_geometry(&mut pointer).expect("the session is live");
    assert_eq!(state.corners[2], v(60.0, 60.0), "the corner was dragged");
    // Enter.
    let out = pointer.commit(&mut ed);
    assert_eq!(out.failed, None, "{out:?}");
    assert_eq!(depth(&ed), d0 + 1, "one undoable step");
    let after = composite(&mut ed);
    // Every pixel outside the scaled patch's destination is untouched: the
    // rest of the layer did not move.
    let stray: Vec<(u32, u32)> = changed(&before, &after)
        .into_iter()
        .filter(|&(x, y)| !in_box(x, y, 20, 20, 60, 60))
        .collect();
    assert!(
        stray.is_empty(),
        "{} pixel(s) outside the destination changed, e.g. {:?}",
        stray.len(),
        &stray[..stray.len().min(5)]
    );
    // The scaled patch is there: each 8 px cell of the destination carries
    // the 4 px source cell it came from.
    for j in 0..5u32 {
        for i in 0..5u32 {
            let (dx, dy) = (24 + 8 * i, 24 + 8 * j);
            let (sx, sy) = (22 + 4 * i, 22 + 4 * j);
            assert!(
                near(px(&after, dx, dy), checker4(sx, sy), 2),
                "({dx}, {dy}) = {:?}, want the source cell at ({sx}, {sy}) = {:?}",
                px(&after, dx, dy),
                checker4(sx, sy)
            );
        }
    }
    // The selection travelled with the pixels.
    assert!(cov(&ed, 21, 21) > 0.5 && cov(&ed, 58, 58) > 0.5);
    assert_eq!(cov(&ed, 62, 62), 0.0);
    assert_eq!(cov(&ed, 18, 18), 0.0);
    // One Undo puts pixels and selection back.
    undo(&mut ed);
    assert_eq!(composite(&mut ed), before, "undo restored the pixels");
    assert_eq!(
        selection(&ed).bounds(),
        Some((IVec2::new(20, 20), IVec2::new(40, 40)))
    );
}

#[test]
fn committing_a_ctrl_t_transform_returns_to_the_previous_tool() {
    let (_dir, mut ed) = open(&checker4);
    let mut pointer = ToolPointer::new();
    marquee(&mut pointer, &mut ed, 20.0, 20.0, 40.0, 40.0);
    assert_eq!(ed.tool(), ToolId::RectMarquee);
    ctrl_t(&mut ed);
    let outcomes = drag(&mut pointer, &mut ed, &[v(40.0, 40.0), v(60.0, 60.0)]);
    all_reached(ToolId::FreeTransform, &outcomes);
    let out = pointer.commit(&mut ed);
    assert_eq!(out.failed, None, "{out:?}");
    assert_eq!(
        ed.tool(),
        ToolId::RectMarquee,
        "Enter hands the palette back to the tool Ctrl+T was pressed from"
    );
}

#[test]
fn escaping_a_ctrl_t_transform_returns_to_the_previous_tool_and_forgets_it() {
    let (_dir, mut ed) = open(&checker4);
    let mut pointer = ToolPointer::new();
    marquee(&mut pointer, &mut ed, 20.0, 20.0, 40.0, 40.0);
    ctrl_t(&mut ed);
    let outcomes = drag(&mut pointer, &mut ed, &[v(40.0, 40.0), v(60.0, 60.0)]);
    all_reached(ToolId::FreeTransform, &outcomes);
    let d0 = depth(&ed);
    // Escape: the route `Shell::abandon_gesture` takes.
    assert!(pointer.cancel(&mut ed), "there was a session to abandon");
    assert_eq!(depth(&ed), d0, "nothing committed");
    assert_eq!(
        ed.tool(),
        ToolId::RectMarquee,
        "Escape hands the palette back to the tool Ctrl+T was pressed from"
    );
    // Free Transform picked from the palette straight after, a session
    // dragged and committed: the abandoned Ctrl+T's hand-back is gone, so the
    // palette stays on Free Transform.
    select_tool(&mut ed, ToolId::FreeTransform);
    let outcomes = drag(&mut pointer, &mut ed, &[v(40.0, 40.0), v(60.0, 60.0)]);
    all_reached(ToolId::FreeTransform, &outcomes);
    let out = pointer.commit(&mut ed);
    assert_eq!(out.failed, None, "{out:?}");
    assert_eq!(ed.tool(), ToolId::FreeTransform, "no stale hand-back");
}

#[test]
fn move_with_a_selection_moves_only_the_selected_pixels() {
    let (_dir, mut ed) = open(&checker4);
    let mut pointer = ToolPointer::new();
    marquee(&mut pointer, &mut ed, 20.0, 20.0, 40.0, 40.0);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    select_tool(&mut ed, ToolId::Move);
    let outcomes = drag(
        &mut pointer,
        &mut ed,
        &[v(30.0, 30.0), v(50.0, 30.0), v(70.0, 30.0)],
    );
    all_reached(ToolId::Move, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one undoable step");
    let after = composite(&mut ed);
    for y in 0..H {
        for x in 0..W {
            let got = px(&after, x, y);
            if in_box(x, y, 60, 20, 80, 40) {
                assert_eq!(got, px(&before, x - 40, y), "moved pixel at ({x}, {y})");
            } else if in_box(x, y, 20, 20, 40, 40) {
                assert_eq!(got[3], 0, "the vacated pixel ({x}, {y}) is empty");
            } else {
                assert_eq!(
                    got,
                    px(&before, x, y),
                    "({x}, {y}) outside the selection moved"
                );
            }
        }
    }
    // The marching ants moved with the pixels.
    assert_eq!(cov(&ed, 61, 21), 1.0);
    assert_eq!(cov(&ed, 21, 21), 0.0);
    undo(&mut ed);
    assert_eq!(composite(&mut ed), before, "undo restored the pixels");
}

#[test]
fn show_transform_controls_draws_its_box_before_any_click() {
    let (_dir, mut ed) = open(&checker4);
    select_tool(&mut ed, ToolId::Move);
    let mut pointer = ToolPointer::new();
    let settings = vec![("show_transform".to_string(), tools::ToolSetting::Bool(true))];
    // The options-bar seed a hover carries, the checkbox just ticked.
    let doc = ed.active().unwrap();
    let hover = app::shell_screen_pt(doc, 5.0, 5.0);
    pointer.handle(
        &mut ed,
        ui::canvas::PointerInput::at(ui::canvas::PointerPhase::Move, hover),
        true,
        &settings,
    );
    let state = transform_geometry(&mut pointer)
        .expect("the ticked option framed the layer before any click");
    assert_eq!(state.source, raster::PixelRect::new(0, 0, W, H));
    // Unticked: the box goes.
    pointer.handle(
        &mut ed,
        ui::canvas::PointerInput::at(ui::canvas::PointerPhase::Move, hover),
        true,
        &[],
    );
    assert!(transform_geometry(&mut pointer).is_none());
}

// ------------------------------------------------------------ W7-F tools --

/// The composite of the whole (possibly resized) canvas, with its size.
fn composite_sized(ed: &mut Editor) -> (u32, u32, Vec<u8>) {
    let doc = ed.active_mut().expect("one document");
    let (w, h) = (doc.document.width(), doc.document.height());
    let buf = doc
        .composite(raster::PixelRect::new(0, 0, w, h))
        .expect("the canvas composites");
    (w, h, buf)
}

fn px_in(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// The DejaVu fixture face, so the type tools shape the same glyphs on
/// every machine.
fn load_fixture_font() {
    compositor::load_font(dejavu::sans::regular().to_vec());
}

/// The inked bounding box of `after` against `before`: `(x0, y0, x1, y1)`.
fn changed_box(before: &[u8], after: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let moved = changed(before, after);
    let x0 = moved.iter().map(|p| p.0).min()?;
    let x1 = moved.iter().map(|p| p.0).max()?;
    let y0 = moved.iter().map(|p| p.1).min()?;
    let y1 = moved.iter().map(|p| p.1).max()?;
    Some((x0, y0, x1, y1))
}

#[test]
fn perspective_crop_rectifies_a_dragged_and_adjusted_quad_as_one_step() {
    let id = ToolId::PerspectiveCrop;
    // A blue wall at x 60..68 on white: a vertical band in the photo.
    let (_dir, mut ed) = open(&wall);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    // Drag the quad, then pull the two top corners in towards the middle —
    // a trapezoid, narrow at the top, as a photographed facade is.
    all_reached(
        id,
        &drag(&mut pointer, &mut ed, &[v(28.0, 20.0), v(100.0, 100.0)]),
    );
    all_reached(
        id,
        &drag(&mut pointer, &mut ed, &[v(28.0, 20.0), v(56.0, 20.0)]),
    );
    all_reached(
        id,
        &drag(&mut pointer, &mut ed, &[v(100.0, 20.0), v(72.0, 20.0)]),
    );
    assert_eq!(depth(&ed), d0, "adjusting the quad commits nothing");
    assert!(pointer.has_pending_commit(), "the quad is held for Enter");
    let commit = pointer.commit(&mut ed);
    assert_eq!(commit.failed, None, "{commit:?}");
    assert_eq!(
        depth(&ed),
        d0 + 1,
        "a perspective crop is ONE history entry"
    );
    let (w, h, out) = composite_sized(&mut ed);
    // Average edges: (16 + 72) / 2 = 44 wide; the slanted sides are 84.76.
    assert_eq!((w, h), (44, 85), "the canvas is the rectified quad");
    // Rectified, the wall's constant width spreads over the narrow top of the
    // quad and shrinks over its wide bottom: a plain crop would keep it the
    // same width on every row.
    let blue_in_row = |y: u32| {
        (0..w)
            .filter(|x| {
                let p = px_in(&out, w, *x, y);
                p[2] > 200 && p[0] < 60
            })
            .count()
    };
    let (top, bottom) = (blue_in_row(1), blue_in_row(h - 2));
    assert!(
        top > bottom * 3 && bottom > 0,
        "the wall was not rectified: {top} blue px on top, {bottom} at the bottom"
    );
    undo(&mut ed);
    let doc = ed.active().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (W, H));
    assert_eq!(
        composite(&mut ed),
        before,
        "undo did not restore the canvas"
    );
}

#[test]
fn vertical_type_lays_the_typed_run_down_a_column() {
    load_fixture_font();
    let id = ToolId::VerticalType;
    let (_dir, mut ed) = open(&white);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    all_reached(id, &click(&mut pointer, &mut ed, v(64.0, 8.0)));
    assert!(pointer.is_text_editing(), "the run is open for typing");
    pointer.text_edit(&mut ed, tools::TextEdit::Insert("W7FV"));
    let out = pointer.text_edit(&mut ed, tools::TextEdit::Confirm);
    assert_eq!(out.failed, None);
    assert!(!pointer.is_text_editing());
    let doc = &ed.active().unwrap().document;
    let texts: Vec<_> = doc
        .layers
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| match &doc.layers.get(id)?.kind {
            layer_model::LayerKind::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 1, "one text layer");
    assert!(texts[0].paragraph.vertical, "the layer is vertical type");
    assert_eq!(texts[0].text, "W7FV");
    assert_eq!(
        depth(&ed),
        d0 + 2,
        "the click's layer and the confirmed run"
    );
    let after = composite(&mut ed);
    let (x0, y0, x1, y1) = changed_box(&before, &after).expect("the run has ink");
    assert!(
        (y1 - y0) > 2 * (x1 - x0),
        "four glyphs down a column are tall and narrow: x {x0}..{x1}, y {y0}..{y1}"
    );
}

#[test]
fn the_type_masks_turn_the_typed_glyphs_into_one_undoable_selection() {
    load_fixture_font();
    // The SAME string and style on purpose: the horizontal mask shapes and
    // caches "WMW" first, so the vertical one only comes out a column when
    // `compositor::text::hash_layer` keys on `Paragraph::vertical`.
    for (id, tall, typed) in [
        (ToolId::HorizontalTypeMask, false, "WMW"),
        (ToolId::VerticalTypeMask, true, "WMW"),
    ] {
        let (_dir, mut ed) = open(&halves);
        let mut pointer = ToolPointer::new();
        select_tool(&mut ed, id);
        let before = composite(&mut ed);
        let d0 = depth(&ed);
        all_reached(id, &click(&mut pointer, &mut ed, v(40.0, 20.0)));
        assert!(pointer.is_text_editing(), "{id:?}: the run is open");
        pointer.text_edit(&mut ed, tools::TextEdit::Insert(typed));
        let out = pointer.text_edit(&mut ed, tools::TextEdit::Confirm);
        assert_eq!(out.failed, None, "{id:?}");
        assert!(!pointer.is_text_editing());
        assert_eq!(
            layers_of_kind(&ed, |k| matches!(k, layer_model::LayerKind::Text(_))),
            0,
            "{id:?}: a type mask leaves no text layer behind"
        );
        assert_eq!(
            depth(&ed),
            d0 + 1,
            "{id:?}: the whole session is ONE history entry"
        );
        assert_eq!(composite(&mut ed), before, "{id:?}: a mask moves no pixel");
        let sel = selection(&ed);
        let Selection::Mask(mask) = &sel else {
            panic!("{id:?}: the glyphs did not become a mask selection: {sel:?}");
        };
        let (lo, hi) = mask.bounds().expect("the selection covers something");
        let (bw, bh) = (hi.x - lo.x, hi.y - lo.y);
        let b = (lo, hi);
        if tall {
            assert!(bh > 2 * bw, "{id:?}: the vertical mask is a column: {b:?}");
        } else {
            assert!(bw > bh, "{id:?}: the horizontal mask is a line: {b:?}");
        }
        undo(&mut ed);
        assert_eq!(selection(&ed), Selection::None, "{id:?}: undo clears it");
        assert_eq!(depth(&ed), d0);
    }
}

#[test]
fn mixer_brush_carries_the_red_it_picked_up_into_the_blue() {
    let id = ToolId::MixerBrush;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let (a, b) = (v(30.0, 64.0), v(100.0, 64.0));
    let pts: Vec<Vec2> = (0..=14).map(|i| a + (b - a) * (i as f32 / 14.0)).collect();
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| drag(p, ed, &pts));
    // Past the border, the wet brush has dragged red into the blue half.
    let p = px(&run.after, 72, 64);
    assert!(
        p[0] > 30 && p[2] > 30,
        "the stroke did not mix the red it picked up with the blue: {p:?}"
    );
    changed_only_within(id, &run.before, &run.after, a, b, 17.0);
    undo_restores(&mut ed, id, &run);
}

#[test]
fn artboard_drag_makes_one_artboard_group_with_a_white_plate() {
    let id = ToolId::Artboard;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[v(20.0, 20.0), v(40.0, 50.0), v(60.0, 70.0)])
    });
    assert_eq!(px(&run.after, 30, 40), WHITE, "the artboard is not drawn");
    assert_eq!(px(&run.after, 10, 10), RED, "outside the artboard changed");
    let doc = &ed.active().unwrap().document;
    let boards = layer_model::artboard::artboards(&doc.layers);
    assert_eq!(boards.len(), 1, "one artboard");
    let board = boards[0].1;
    assert_eq!(
        (board.x, board.y, board.width, board.height),
        (20, 20, 40, 50)
    );
    undo_restores(&mut ed, id, &run);
    let doc = &ed.active().unwrap().document;
    assert!(layer_model::artboard::artboards(&doc.layers).is_empty());
}

#[test]
fn curvature_pen_clicks_then_enter_make_one_smooth_shape_layer() {
    let id = ToolId::CurvaturePen;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    for at in [v(10.0, 60.0), v(50.0, 20.0), v(90.0, 60.0)] {
        all_reached(id, &click(&mut pointer, &mut ed, at));
        assert_eq!(depth(&ed), d0, "nothing is emitted while points are placed");
    }
    let commit = pointer.commit(&mut ed);
    assert_eq!(commit.failed, None);
    assert_eq!(depth(&ed), d0 + 1, "the path is ONE history entry");
    let doc = &ed.active().unwrap().document;
    let svg: Vec<String> = doc
        .layers
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| match &doc.layers.get(id)?.kind {
            layer_model::LayerKind::Shape(s) => Some(s.path_svg.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(svg.len(), 1, "one shape layer");
    assert!(
        svg[0].contains('C'),
        "the path through the clicks is not curved: {}",
        svg[0]
    );
    undo(&mut ed);
    assert_eq!(
        layers_of_kind(&ed, |k| matches!(k, layer_model::LayerKind::Shape(_))),
        0
    );
}

#[test]
fn freeform_pen_drag_is_fitted_into_one_short_curved_shape_layer() {
    let id = ToolId::FreeformPen;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    let pts: Vec<Vec2> = (0..=40)
        .map(|i| {
            let t = std::f32::consts::PI * i as f32 / 40.0;
            v(64.0 - 40.0 * t.cos(), 90.0 - 50.0 * t.sin())
        })
        .collect();
    let outcomes = drag(&mut pointer, &mut ed, &pts);
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one freehand drag is ONE history entry");
    let doc = &ed.active().unwrap().document;
    let svg: Vec<String> = doc
        .layers
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| match &doc.layers.get(id)?.kind {
            layer_model::LayerKind::Shape(s) => Some(s.path_svg.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(svg.len(), 1, "one shape layer");
    let curves = svg[0].matches('C').count();
    assert!(
        (2..20).contains(&curves),
        "the 41 samples were not fitted into a short curve: {}",
        svg[0]
    );
    undo(&mut ed);
    assert_eq!(depth(&ed), d0);
}

// ------------------------------------------------------ W8-C follow-ups --

/// A `W × H` shell editor over `fixture` whose folder picker answers `out`
/// — what File > Export > Artboards to Files writes into.
fn open_exporting_to(
    fixture: &dyn Fn(u32, u32) -> [u8; 4],
    out: &std::path::Path,
) -> (tempfile::TempDir, Editor) {
    use app_shell::{dialogs::ScriptedDialogs, prefs::AppPaths, recent::RecentFiles};
    let dir = tempfile::tempdir().expect("a tempdir");
    let canvas = dir.path().join("canvas.png");
    let white = vec![255u8; (W * H * 4) as usize];
    std::fs::write(
        &canvas,
        raster::encode(raster::ExportFormat::Png, W, H, &white).unwrap(),
    )
    .unwrap();
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.path().join("config")),
        app_shell::prefs::Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new().exporting_folder(out)),
    );
    ed.open_path(&canvas).expect("the canvas opens");
    app::center_camera(ed.active_mut().unwrap());
    let layer = app::the_opened_layer(&ed);
    ed.active_mut().unwrap().paint_canvas(layer, fixture);
    (dir, ed)
}

/// W8-C: an artboard clips what is inside it to its rect in the composite,
/// and File > Export > Artboards to Files writes one image per artboard,
/// each its own rect with its own contents.
#[test]
fn artboards_clip_their_children_and_export_one_file_each() {
    let id = ToolId::Artboard;
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("boards");
    let (_dir, mut ed) = open_exporting_to(&halves, &out);
    let photo = app::the_opened_layer(&ed);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    all_reached(
        id,
        &drag(&mut pointer, &mut ed, &[v(20.0, 20.0), v(60.0, 70.0)]),
    );
    all_reached(
        id,
        &drag(&mut pointer, &mut ed, &[v(70.0, 20.0), v(110.0, 60.0)]),
    );
    let boards = layer_model::artboard::artboards(&ed.active().unwrap().document.layers);
    assert_eq!(boards.len(), 2, "two artboards");
    // Put the red|blue photo inside the first artboard, above its plate.
    let first = boards
        .iter()
        .find(|(_, b)| b.x == 20)
        .map(|(g, _)| *g)
        .unwrap();
    ed.apply_command(editor_core::Command::MoveLayer {
        layer_id: photo,
        parent: Some(first),
        index: 0,
    });
    let after = composite(&mut ed);
    assert_eq!(px(&after, 30, 40), RED, "inside the artboard: its child");
    assert_eq!(
        px(&after, 10, 10)[3],
        0,
        "outside the artboard its child is clipped away"
    );
    assert_eq!(px(&after, 100, 100)[3], 0, "clipped below it too");
    assert_eq!(px(&after, 90, 40), WHITE, "the second artboard's plate");

    let said = menu_bridge::perform(ui::menu::MenuAction::ExportArtboards, &mut ed)
        .expect("the export runs");
    assert!(said.contains("2 artboard"), "{said}");
    let mut files: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 2, "one file per artboard: {files:?}");
    let decoded: Vec<_> = files
        .iter()
        .map(|f| raster::decode_path(f).expect("an artboard file decodes"))
        .collect();
    let sizes: Vec<_> = decoded.iter().map(|d| (d.width, d.height)).collect();
    assert!(
        sizes.contains(&(40, 50)) && sizes.contains(&(40, 40)),
        "{sizes:?}"
    );
    let photo_board = decoded.iter().find(|d| d.height == 50).unwrap();
    assert_eq!(
        &photo_board.rgba8[..4],
        &RED,
        "the first board holds the photo"
    );
    let plain = decoded.iter().find(|d| d.height == 40).unwrap();
    assert_eq!(
        &plain.rgba8[..4],
        &WHITE,
        "the second board is only its plate"
    );
}

/// An artboard exports only its own branch: a visible layer that sits
/// outside every artboard (here the opened photo, left at the root under the
/// board, lifted above it) must not show through the exported file, which is the artboard's
/// plate alone - as Photopea exports artboards.
#[test]
fn an_exported_artboard_leaves_out_layers_outside_every_artboard() {
    let id = ToolId::Artboard;
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("boards");
    let (_dir, mut ed) = open_exporting_to(&halves, &out);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    all_reached(
        id,
        &drag(&mut pointer, &mut ed, &[v(20.0, 20.0), v(60.0, 70.0)]),
    );
    // Lift the photo to the TOP of the root, above the board, so an opaque
    // plate cannot hide it: only the export's isolation keeps it out.
    let photo = app::the_opened_layer(&ed);
    // Index 0 is the top of a layer list in this model.
    ed.apply_command(editor_core::Command::MoveLayer {
        layer_id: photo,
        parent: None,
        index: 0,
    });
    let over = composite(&mut ed);
    assert_eq!(
        px(&over, 30, 40),
        RED,
        "the photo is above the board on the canvas"
    );
    let said = menu_bridge::perform(ui::menu::MenuAction::ExportArtboards, &mut ed)
        .expect("the export runs");
    assert!(said.contains("1 artboard"), "{said}");
    let files: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let decoded = raster::decode_path(&files[0]).expect("the artboard file decodes");
    assert!(
        decoded.rgba8.as_chunks::<4>().0.iter().all(|p| *p == WHITE),
        "a layer outside the artboard leaked into its export"
    );
}

/// W8-C: Enter rectifies EVERY pixel layer, not just the active one — the
/// opaque copy on top is warped too, so the composite shows the wall
/// rectified although the active layer is the one underneath.
#[test]
fn perspective_crop_rectifies_every_layer_not_only_the_active_one() {
    let id = ToolId::PerspectiveCrop;
    let (_dir, mut ed) = open(&wall);
    let bottom = app::the_opened_layer(&ed);
    let copy = layer_model::Layer::raster("Copy");
    let copy_id = copy.id;
    ed.apply_command(editor_core::Command::create_layer(copy));
    ed.active_mut().unwrap().paint_canvas(copy_id, &wall);
    ed.set_active_layer(bottom);
    assert_eq!(
        ed.active().unwrap().document.active_layer(),
        Some(bottom),
        "the active layer is the one underneath"
    );
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    all_reached(
        id,
        &drag(&mut pointer, &mut ed, &[v(28.0, 20.0), v(100.0, 100.0)]),
    );
    all_reached(
        id,
        &drag(&mut pointer, &mut ed, &[v(28.0, 20.0), v(56.0, 20.0)]),
    );
    all_reached(
        id,
        &drag(&mut pointer, &mut ed, &[v(100.0, 20.0), v(72.0, 20.0)]),
    );
    let commit = pointer.commit(&mut ed);
    assert_eq!(commit.failed, None, "{commit:?}");
    assert_eq!(depth(&ed), d0 + 1, "still ONE history entry");
    let (w, h, out) = composite_sized(&mut ed);
    let blue_in_row = |y: u32| {
        (0..w)
            .filter(|x| {
                let p = px_in(&out, w, *x, y);
                p[2] > 200 && p[0] < 60
            })
            .count()
    };
    let (top, bottom_row) = (blue_in_row(1), blue_in_row(h - 2));
    assert!(
        top > bottom_row * 3 && bottom_row > 0,
        "the top layer was not rectified: {top} blue px on top, {bottom_row} at the bottom"
    );
}

/// W8-C: the Mixer Brush previews its wet stroke while it is dragged, through
/// the same preview lens as every stroke tool, and the release commits
/// exactly what the last preview showed.
#[test]
fn mixer_brush_previews_the_wet_stroke_while_it_is_dragged() {
    use ui::canvas::PointerPhase;
    let id = ToolId::MixerBrush;
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let (a, b) = (v(30.0, 64.0), v(100.0, 64.0));
    let pts: Vec<Vec2> = (0..=14).map(|i| a + (b - a) * (i as f32 / 14.0)).collect();
    let down = app::shell_pointer(&mut pointer, &mut ed, PointerPhase::Down, pts[0]);
    all_reached(id, &[down]);
    let mut previewed = 0;
    for p in &pts[1..] {
        let o = app::shell_pointer(&mut pointer, &mut ed, PointerPhase::Move, *p);
        previewed += o.preview_tiles;
        all_reached(id, &[o]);
    }
    assert!(previewed > 0, "no Move sample previewed a tile");
    assert!(ed.active().unwrap().has_paint_preview(), "the lens is up");
    assert_eq!(depth(&ed), d0, "a preview writes no history");
    let mid = composite(&mut ed);
    let p = px(&mid, 72, 64);
    assert!(
        p[0] > 30 && p[2] > 30,
        "mid-drag the canvas already shows red carried into the blue: {p:?}"
    );
    assert_ne!(mid, before, "the stroke is visible before release");
    let last = *pts.last().unwrap();
    let up = app::shell_pointer(&mut pointer, &mut ed, PointerPhase::Up, last);
    all_reached(id, &[up]);
    assert!(
        !ed.active().unwrap().has_paint_preview(),
        "the lens is down"
    );
    assert_eq!(depth(&ed), d0 + 1);
    assert_eq!(
        composite(&mut ed),
        mid,
        "the release committed exactly the previewed pixels"
    );
}

/// W8-C: the Type Mask tools' options-bar Mode reaches the confirm — Add
/// keeps the selection there and adds the glyphs, Subtract cuts the glyphs
/// out of it.
#[test]
fn the_type_mask_mode_adds_to_and_subtracts_from_the_selection() {
    load_fixture_font();
    let id = ToolId::HorizontalTypeMask;
    for (mode, name) in [(1usize, "Add"), (2, "Subtract")] {
        let (_dir, mut ed) = open(&halves);
        // A full-canvas selection to subtract from, or a small square in the
        // far corner to add to.
        let coverage: Vec<u8> = (0..H)
            .flat_map(|y| {
                (0..W).map(move |x| {
                    if mode == 2 || (x >= 110 && y >= 110) {
                        255
                    } else {
                        0
                    }
                })
            })
            .collect();
        app::set_selection(
            &mut ed,
            Selection::Mask(editor_core::SelectionMask::new(IVec2::ZERO, W, H, coverage).unwrap()),
        );
        let mut pointer = ToolPointer::new();
        select_tool(&mut ed, id);
        let seed = [("mode".to_string(), tools::ToolSetting::Choice(mode))];
        all_reached(
            id,
            &seeded_stroke(&mut pointer, &mut ed, &[v(20.0, 20.0)], &seed),
        );
        assert!(pointer.is_text_editing(), "{name}: the run is open");
        pointer.text_edit(&mut ed, tools::TextEdit::Insert("WMW"));
        let out = pointer.text_edit(&mut ed, tools::TextEdit::Confirm);
        assert_eq!(out.failed, None, "{name}");
        // The glyph pixels: somewhere in the run's box the coverage is full
        // (Add) or gone (Subtract).
        let glyph_cov: Vec<f32> = (20..80)
            .flat_map(|y| (20..100).map(move |x| (x, y)))
            .map(|(x, y)| cov(&ed, x, y))
            .collect();
        assert!(
            cov(&ed, 120, 120) > 0.99,
            "{name}: the old selection is kept"
        );
        if mode == 1 {
            assert!(
                glyph_cov.iter().any(|c| *c > 0.99),
                "Add: the glyphs joined the selection"
            );
            assert!(cov(&ed, 5, 120) < 0.01, "Add: nothing else was selected");
        } else {
            assert!(
                glyph_cov.iter().any(|c| *c < 0.01),
                "Subtract: the glyphs were cut out"
            );
            assert!(cov(&ed, 5, 120) > 0.99, "Subtract: the rest stays selected");
        }
    }
}

/// W8-C: vertical type's caret and click hit-test follow the column — a
/// click with the Vertical Type tool on the third cell of an existing
/// vertical run enters it with the caret before that cell, and the caret
/// drawn is a bar ACROSS the column.
#[test]
fn vertical_type_caret_and_click_follow_the_column() {
    load_fixture_font();
    let id = ToolId::VerticalType;
    let (_dir, mut ed) = open(&white);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    all_reached(id, &click(&mut pointer, &mut ed, v(64.0, 8.0)));
    pointer.text_edit(&mut ed, tools::TextEdit::Insert("ABCD"));
    pointer.text_edit(&mut ed, tools::TextEdit::Confirm);
    assert!(!pointer.is_text_editing());
    let texts = |ed: &Editor| {
        let doc = &ed.active().unwrap().document;
        doc.layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| match &doc.layers.get(id)?.kind {
                layer_model::LayerKind::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(texts(&ed), vec!["ABCD".to_string()]);
    // The third cell starts two em cells (the 24 px default) below the top.
    all_reached(
        id,
        &click(&mut pointer, &mut ed, v(64.0, 8.0 + 2.0 * 24.0 + 3.0)),
    );
    assert!(pointer.is_text_editing(), "the click entered the run");
    let caret: Vec<_> = pointer
        .text_overlay_geometry(&ed)
        .into_iter()
        .filter(|s| {
            s.kind == app_shell::tool_input::TextOverlayKind::Caret && (s.b - s.a).length() > 0.5
        })
        .collect();
    assert!(!caret.is_empty(), "a caret is drawn");
    assert!(
        caret.iter().all(|s| (s.a.y - s.b.y).abs() < 0.01),
        "the vertical caret is a bar across the column: {caret:?}"
    );
    assert!(
        caret[0].a.y > 8.0 + 24.0,
        "the caret sits down the column: {caret:?}"
    );
    pointer.text_edit(&mut ed, tools::TextEdit::Insert("X"));
    pointer.text_edit(&mut ed, tools::TextEdit::Confirm);
    assert_eq!(
        texts(&ed),
        vec!["ABXCD".to_string()],
        "the click placed the caret before the third cell"
    );
}

// ------------------------------------------- W9-D: Sample (all layers) --

/// W9-D: `id`'s options as the options bar holds them once its Sample combo
/// reads `choice` (0 Current Layer, 1 Current & Below, 2 All Layers) — the
/// choice written into the workspace's options as the combo writes it, then
/// `Chrome::tool_options` converted at the boundary exactly as `shell.rs`
/// converts it.
fn sample_seed(id: ToolId, choice: usize) -> Vec<(String, tools::ToolSetting)> {
    let mut chrome = Chrome::new();
    chrome.set_tool_choice(id, tools::tool::SAMPLE_LAYERS_KEY, choice);
    assert_eq!(
        chrome
            .workspace()
            .options
            .get(id, tools::tool::SAMPLE_LAYERS_KEY),
        Some(ui::OptionValue::Choice(choice)),
        "{id:?}: the options bar holds no Sample choice"
    );
    chrome
        .tool_options(id)
        .into_iter()
        .map(|(key, value)| {
            let setting = match value {
                ui::OptionValue::Float(v) => tools::ToolSetting::Float(v),
                ui::OptionValue::Int(v) => tools::ToolSetting::Int(v),
                ui::OptionValue::Bool(v) => tools::ToolSetting::Bool(v),
                ui::OptionValue::Choice(v) => tools::ToolSetting::Choice(v),
                ui::OptionValue::Color(v) => tools::ToolSetting::Color(v),
            };
            (key, setting)
        })
        .collect()
}

/// W9-D: a fresh, EMPTY raster layer added above the photo and made active,
/// the way a Layers-panel click does. Returns (photo layer, empty layer).
fn empty_layer_above(ed: &mut Editor) -> (layer_model::LayerId, layer_model::LayerId) {
    let photo = app::the_opened_layer(ed);
    let empty = ed
        .active_mut()
        .unwrap()
        .add_layer(layer_model::Layer::raster("Retouch"));
    ed.set_active_layer(empty);
    (photo, empty)
}

/// The pixels of one layer alone, RGBA8 over the canvas.
fn layer_px(ed: &Editor, layer: layer_model::LayerId) -> Vec<u8> {
    ed.active()
        .unwrap()
        .layer_pixels(layer)
        .expect("layer pixels")
}

#[test]
fn clone_stamp_sampling_all_layers_copies_the_photo_into_an_empty_layer() {
    let id = ToolId::CloneStamp;
    let (_dir, mut ed) = open(&halves);
    let (photo, empty) = empty_layer_above(&mut ed);
    let photo_before = layer_px(&ed, photo);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let src = alt_click(&mut pointer, &mut ed, v(32.0, 64.0));
    all_reached(id, &src);
    let seed = sample_seed(id, 2);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let outcomes = seeded_stroke(
        &mut pointer,
        &mut ed,
        &[v(96.0, 60.0), v(96.0, 68.0)],
        &seed,
    );
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one stroke, one history entry");
    let retouch = layer_px(&ed, empty);
    let cloned = px(&retouch, 96, 64);
    assert!(
        near(cloned, RED, 2),
        "the empty layer did not receive the photo's red from the source: {cloned:?}"
    );
    assert_eq!(px(&retouch, 96, 10), [0, 0, 0, 0], "outside the stroke");
    assert_eq!(
        layer_px(&ed, photo),
        photo_before,
        "the photo layer was written; only the active layer may be"
    );
    let after = composite(&mut ed);
    assert!(near(px(&after, 96, 64), RED, 2), "the composite shows it");
    undo(&mut ed);
    assert_eq!(composite(&mut ed), before, "undo restores byte for byte");
}

#[test]
fn clone_stamp_sampling_the_current_layer_copies_nothing_from_an_empty_layer() {
    let id = ToolId::CloneStamp;
    let (_dir, mut ed) = open(&halves);
    let (_photo, empty) = empty_layer_above(&mut ed);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    all_reached(id, &alt_click(&mut pointer, &mut ed, v(32.0, 64.0)));
    let seed = sample_seed(id, 0);
    let before = composite(&mut ed);
    let outcomes = seeded_stroke(
        &mut pointer,
        &mut ed,
        &[v(96.0, 60.0), v(96.0, 68.0)],
        &seed,
    );
    all_reached(id, &outcomes);
    assert!(
        layer_px(&ed, empty).iter().all(|&b| b == 0),
        "Current Layer on an empty layer cloned something"
    );
    assert_eq!(composite(&mut ed), before, "no pixel moved");
}

#[test]
fn clone_stamp_sampling_current_and_below_skips_the_layers_above() {
    let id = ToolId::CloneStamp;
    let (_dir, mut ed) = open(&halves);
    let (_photo, empty) = empty_layer_above(&mut ed);
    // A white layer ABOVE the retouch layer, opaque only in the 16 x 16
    // square at the top-left corner (x < 16, y < 16) and clear elsewhere.
    // The source is alt-clicked at (8, 8), inside that square: the photo
    // under it is red, the cover over it is white, so Current & Below reads
    // red and All Layers reads white.
    let cover = ed
        .active_mut()
        .unwrap()
        .add_layer(layer_model::Layer::raster("Cover"));
    ed.active_mut().unwrap().paint_canvas(cover, &|x, y| {
        if x < 16 && y < 16 {
            WHITE
        } else {
            [0; 4]
        }
    });
    ed.set_active_layer(empty);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    // Source on the cover's white square.
    all_reached(id, &alt_click(&mut pointer, &mut ed, v(8.0, 8.0)));
    let outcomes = seeded_stroke(
        &mut pointer,
        &mut ed,
        &[v(96.0, 60.0), v(96.0, 68.0)],
        &sample_seed(id, 1),
    );
    all_reached(id, &outcomes);
    let got = px(&layer_px(&ed, empty), 96, 64);
    assert!(
        near(got, RED, 2),
        "Current & Below read the white layer above it: {got:?}"
    );
    // All Layers, same gesture: the white above is sampled this time.
    let outcomes = seeded_stroke(
        &mut pointer,
        &mut ed,
        &[v(96.0, 60.0), v(96.0, 68.0)],
        &sample_seed(id, 2),
    );
    all_reached(id, &outcomes);
    let got = px(&layer_px(&ed, empty), 96, 64);
    assert!(
        near(got, WHITE, 2),
        "All Layers missed the layer above: {got:?}"
    );
}

#[test]
fn spot_healing_sampling_all_layers_heals_the_blemish_onto_an_empty_layer() {
    let id = ToolId::SpotHealing;
    let (_dir, mut ed) = open(&spot_at(64, 64));
    let (photo, empty) = empty_layer_above(&mut ed);
    let photo_before = layer_px(&ed, photo);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let outcomes = seeded_stroke(
        &mut pointer,
        &mut ed,
        &[v(60.0, 64.0), v(68.0, 64.0)],
        &sample_seed(id, 2),
    );
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one stroke, one history entry");
    let healed = px(&layer_px(&ed, empty), 64, 64);
    assert!(
        healed[3] > 200 && healed[0] > DARK[0] + 60,
        "the empty layer did not receive the healed surround: {healed:?}"
    );
    assert_eq!(
        layer_px(&ed, photo),
        photo_before,
        "the blemish on the photo layer was written"
    );
    let after = composite(&mut ed);
    assert!(
        px(&after, 64, 64)[0] > DARK[0] + 60,
        "the composite still shows the blemish: {:?}",
        px(&after, 64, 64)
    );
    assert_eq!(px(&after, 64, 20), GREY, "outside the stroke");
    undo(&mut ed);
    assert_eq!(composite(&mut ed), before, "undo restores byte for byte");
}

#[test]
fn spot_healing_content_aware_sampling_all_layers_heals_onto_an_empty_layer() {
    // Type = Content-Aware + Sample = All Layers: the shell runs heavy
    // finishes on a worker (defer_heavy_commits), but that worker reads the
    // ACTIVE layer, which is empty here, so the sampled synthesis must run
    // at release from the composite and lay into the empty layer.
    let id = ToolId::SpotHealing;
    let (_dir, mut ed) = open(&spot_at(64, 64));
    let (photo, empty) = empty_layer_above(&mut ed);
    let photo_before = layer_px(&ed, photo);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    // `Chrome::tool_options` leaves out options still at their default, so
    // the non-default Type is pushed here: 1 = Content-Aware.
    let mut seed = sample_seed(id, 2);
    seed.retain(|(key, _)| key != "type");
    seed.push(("type".to_string(), tools::ToolSetting::Choice(1)));
    let before = composite(&mut ed);
    let d0 = depth(&ed);
    let outcomes = seeded_stroke(
        &mut pointer,
        &mut ed,
        &[v(60.0, 64.0), v(68.0, 64.0)],
        &seed,
    );
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "one stroke, one history entry");
    let healed = px(&layer_px(&ed, empty), 64, 64);
    assert!(
        near(healed, GREY, 8),
        "the empty layer did not receive the Content-Aware heal: {healed:?}"
    );
    assert_eq!(
        px(&layer_px(&ed, empty), 64, 20),
        [0, 0, 0, 0],
        "outside the stroke"
    );
    assert_eq!(
        layer_px(&ed, photo),
        photo_before,
        "the blemish on the photo layer was written"
    );
    assert!(
        near(px(&composite(&mut ed), 64, 64), GREY, 8),
        "the composite still shows the blemish"
    );
    undo(&mut ed);
    assert_eq!(composite(&mut ed), before, "undo restores byte for byte");
}

#[test]
fn every_sampling_retouch_tool_writes_the_composite_onto_an_empty_layer() {
    // Blur, Sharpen, Smudge and the Healing Brush with Sample = All Layers:
    // an empty layer above the photo receives pixels under the stroke, and
    // the photo stays as it was. With Current Layer each leaves it empty.
    for id in [
        ToolId::Blur,
        ToolId::Sharpen,
        ToolId::Smudge,
        ToolId::HealingBrush,
    ] {
        for (choice, writes) in [(2, true), (0, false)] {
            let (_dir, mut ed) = open(&halves);
            let (photo, empty) = empty_layer_above(&mut ed);
            let photo_before = layer_px(&ed, photo);
            let mut pointer = ToolPointer::new();
            select_tool(&mut ed, id);
            if id == ToolId::HealingBrush {
                all_reached(id, &alt_click(&mut pointer, &mut ed, v(32.0, 64.0)));
            }
            let outcomes = seeded_stroke(
                &mut pointer,
                &mut ed,
                &[v(56.0, 64.0), v(72.0, 64.0)],
                &sample_seed(id, choice),
            );
            all_reached(id, &outcomes);
            let alpha = px(&layer_px(&ed, empty), 64, 64)[3];
            assert_eq!(
                alpha > 0,
                writes,
                "{id:?} with Sample choice {choice}: the empty layer's alpha under the stroke is {alpha}"
            );
            assert_eq!(
                layer_px(&ed, photo),
                photo_before,
                "{id:?}: the photo was written"
            );
        }
    }
}

// ------------------------------------------------------------ W10-A tools --

/// White with a dark 12 x 12 block over 20..32 on both axes.
fn dark_block(x: u32, y: u32) -> [u8; 4] {
    if (20..32).contains(&x) && (20..32).contains(&y) {
        DARK
    } else {
        WHITE
    }
}

/// The pixels of `buf` inside the half-open box that are not white.
fn ink_in(buf: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if px(buf, x, y) != WHITE {
                n += 1;
            }
        }
    }
    n
}

/// The layer ids of the active document, depth-first.
fn layer_ids(ed: &Editor) -> Vec<layer_model::LayerId> {
    ed.active().unwrap().document.layers.iter_depth_first()
}

/// The shape layer a shape gesture just made (the one not in `before`).
fn newest_shape(ed: &Editor, before: &[layer_model::LayerId]) -> layer_model::LayerId {
    let doc = &ed.active().unwrap().document;
    layer_ids(ed)
        .into_iter()
        .find(|id| {
            !before.contains(id)
                && doc
                    .layers
                    .get(*id)
                    .is_some_and(|l| matches!(l.kind, layer_model::LayerKind::Shape(_)))
        })
        .expect("the gesture made a shape layer")
}

#[test]
fn content_aware_move_moves_the_selected_block_and_fills_where_it_was() {
    let id = ToolId::ContentAwareMove;
    let (_dir, mut ed) = open(&dark_block);
    let mut pointer = ToolPointer::new();
    marquee(&mut pointer, &mut ed, 16.0, 16.0, 36.0, 36.0);
    let sel_before = selection(&ed);
    // Move mode (the default): drag from inside the selection by (60, 50).
    let run = run_pixel_route(&mut ed, &mut pointer, id, |p, ed| {
        drag(p, ed, &[v(26.0, 26.0), v(56.0, 50.0), v(86.0, 76.0)])
    });
    assert!(
        near(px(&run.after, 86, 76), DARK, 8),
        "the block did not land at the destination: {:?}",
        px(&run.after, 86, 76)
    );
    let hole = px(&run.after, 26, 26);
    assert!(
        linear_luminance(hole) > 0.8,
        "the source was not filled from the white around it: {hole:?}"
    );
    // Nothing far from the source and the destination changed.
    assert_eq!(px(&run.after, 120, 10), WHITE);
    assert_eq!(px(&run.after, 5, 120), WHITE);
    // The selection followed the patch, in the same history step.
    assert_eq!(cov(&ed, 80, 70), 1.0, "the selection did not move");
    assert_eq!(cov(&ed, 20, 20), 0.0, "the selection stayed behind");
    undo_restores(&mut ed, id, &run);
    assert_eq!(
        selection(&ed),
        sel_before,
        "undo did not restore the selection"
    );

    // Extend mode: the copy lands and the source stays.
    let (_dir, mut ed) = open(&dark_block);
    let mut pointer = ToolPointer::new();
    marquee(&mut pointer, &mut ed, 16.0, 16.0, 36.0, 36.0);
    select_tool(&mut ed, id);
    let d0 = depth(&ed);
    let seed = vec![(
        tools::content_aware_move::MODE_KEY.to_string(),
        tools::ToolSetting::Choice(1),
    )];
    let outcomes = seeded_stroke(
        &mut pointer,
        &mut ed,
        &[v(26.0, 26.0), v(86.0, 76.0)],
        &seed,
    );
    all_reached(id, &outcomes);
    assert_eq!(depth(&ed), d0 + 1, "Extend is one history entry too");
    let after = composite(&mut ed);
    assert!(near(px(&after, 86, 76), DARK, 8), "the copy did not land");
    assert_eq!(px(&after, 26, 26), DARK, "Extend kept the source");
}

#[test]
fn slice_select_moves_then_deletes_the_committed_slice_without_a_history_entry() {
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    // A slice committed through the Slice tool's own route.
    select_tool(&mut ed, ToolId::Slice);
    all_reached(
        ToolId::Slice,
        &drag(&mut pointer, &mut ed, &[v(10.0, 10.0), v(50.0, 40.0)]),
    );
    let commit = pointer.commit(&mut ed);
    assert_eq!(commit.slices.len(), 1, "{commit:?}");
    let first = commit.slices[0].rect;
    let before = composite(&mut ed);
    let d0 = depth(&ed);

    let id = ToolId::SliceSelect;
    select_tool(&mut ed, id);
    // A body drag moves the committed slice by (30, 20).
    let centre = v(
        first.x as f32 + first.width as f32 / 2.0,
        first.y as f32 + first.height as f32 / 2.0,
    );
    let outcomes = drag(
        &mut pointer,
        &mut ed,
        &[centre, centre + v(15.0, 10.0), centre + v(30.0, 20.0)],
    );
    all_reached(id, &outcomes);
    let published: Vec<_> = outcomes.iter().filter_map(|o| o.slices.clone()).collect();
    assert_eq!(published.len(), 1, "one edited set: {outcomes:?}");
    assert_eq!(published[0].len(), 1, "{published:?}");
    let moved = published[0][0].rect;
    assert_eq!(
        moved,
        raster::PixelRect::new(first.x + 30, first.y + 20, first.width, first.height),
        "the slice did not move with the drag"
    );
    // The overlay shows the set as the document now holds it.
    let Some((_, tools::SessionGeometry::SliceSelect { rects, .. })) = pointer.live_geometry()
    else {
        panic!("Slice Select shows no slices");
    };
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0][0], v(moved.x as f32, moved.y as f32));

    // Alt+click on the slice where it now is deletes it: the next press was
    // built over the set the move committed, not the old one.
    let outcomes = alt_click(
        &mut pointer,
        &mut ed,
        v(moved.x as f32 + 5.0, moved.y as f32 + 5.0),
    );
    all_reached(id, &outcomes);
    assert_eq!(
        outcomes.last().unwrap().slices,
        Some(Vec::new()),
        "Alt+click did not publish the emptied set"
    );
    assert!(
        pointer.live_geometry().is_none(),
        "the deleted slice is still drawn"
    );
    // A press where the slice first was finds nothing either.
    click(&mut pointer, &mut ed, centre);
    assert!(
        pointer.live_geometry().is_none(),
        "the old slice came back: the store did not keep the edits"
    );

    assert_eq!(
        depth(&ed),
        d0 + 2,
        "the move and the delete are one History step each (W11-E)"
    );
    assert_eq!(composite(&mut ed), before, "Slice Select touched pixels");
}

/// W10-A: with the Slice Select tool, Delete (the key's real route: the
/// keymap, the bridge's pick, then `menu_bridge::perform`) removes the slice
/// the last press picked instead of clearing pixels; the other slices keep
/// their names through the edit, so File > Export > Slices writes them under
/// their own numbers, and a slice given a name in its options is exported
/// under that name.
#[test]
fn slice_select_delete_key_removes_the_picked_slice_and_names_survive_to_the_export() {
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("slices");
    let (_dir, mut ed) = open_exporting_to(&halves, &out);
    let mut pointer = ToolPointer::new();
    // Three slices through the Slice tool's route, committed with Enter.
    select_tool(&mut ed, ToolId::Slice);
    let boxes = [
        (v(4.0, 4.0), v(24.0, 24.0)),
        (v(40.0, 4.0), v(60.0, 24.0)),
        (v(80.0, 4.0), v(100.0, 24.0)),
    ];
    for (from, to) in boxes {
        all_reached(ToolId::Slice, &drag(&mut pointer, &mut ed, &[from, to]));
    }
    let commit = pointer.commit(&mut ed);
    assert_eq!(commit.slices.len(), 3, "{commit:?}");
    let before = composite(&mut ed);
    let d0 = depth(&ed);

    // Picking the tool shows the committed set at once — the shell's
    // pending-session step, which runs after every menu pick and on every
    // pointer sample — each slice labelled by its name, none picked yet.
    select_tool(&mut ed, ToolId::SliceSelect);
    pointer.begin_pending_session(&mut ed, &[]);
    assert_eq!(
        slice_overlay(&mut pointer),
        (3, vec!["01".to_string(), "02".into(), "03".into()], None)
    );

    // Pick the middle slice with a plain click: the overlay marks it, and
    // the chrome draws it with the thick selected outline.
    all_reached(
        ToolId::SliceSelect,
        &click(&mut pointer, &mut ed, v(50.0, 14.0)),
    );
    assert_eq!(
        slice_overlay(&mut pointer),
        (3, vec!["01".to_string(), "02".into(), "03".into()], Some(1))
    );
    let (texts, picked_outlines) = painted_slice_overlay(&mut pointer, &mut ed);
    for label in ["01", "02", "03"] {
        assert!(
            texts.iter().any(|t| t == label),
            "{label} not drawn: {texts:?}"
        );
    }
    assert_eq!(picked_outlines, 1, "the picked slice is not drawn picked");

    // Press Delete.
    let chord = app_shell::Chord::plain(app_shell::Key::Delete);
    let Some(app_shell::keymap::Resolved::Menu(action)) = ed.keymap().resolve_any(&chord) else {
        panic!("Delete does not resolve to a menu action");
    };
    let pick = menu_bridge::pick(&ui::Intent::Action(action), &ed)
        .unwrap_or_else(|| panic!("{action:?} did not route"));
    let mut frame = ChromeOutput::default();
    menu_bridge::record(pick, &mut frame);
    assert_eq!(frame.menu, vec![action], "{frame:?}");
    let said = menu_bridge::perform(action, &mut ed).expect("Delete performs");
    assert!(said.contains("slice_02"), "{said}");
    assert_eq!(
        depth(&ed),
        d0 + 1,
        "deleting a slice is one History step (W11-E)"
    );
    assert_eq!(composite(&mut ed), before, "Delete cleared pixels");
    // The shell's step after the menu pick (no press in between): the
    // overlay is the store's — the middle slice gone, the survivors keeping
    // their own labels, nothing picked — and that is what the chrome draws.
    pointer.begin_pending_session(&mut ed, &[]);
    let Some((_, tools::SessionGeometry::SliceSelect { rects, .. })) = pointer.live_geometry()
    else {
        panic!("Slice Select shows no slices");
    };
    assert_eq!(
        rects,
        vec![[v(4.0, 4.0), v(24.0, 24.0)], [v(80.0, 4.0), v(100.0, 24.0)]],
        "the deleted slice is still drawn"
    );
    assert_eq!(
        slice_overlay(&mut pointer),
        (2, vec!["01".to_string(), "03".into()], None),
        "the survivor was relabelled by position, or the deleted slice is still picked"
    );
    let (texts, picked_outlines) = painted_slice_overlay(&mut pointer, &mut ed);
    assert!(
        texts.iter().any(|t| t == "03") && !texts.iter().any(|t| t == "02"),
        "the canvas labels the survivors by position: {texts:?}"
    );
    assert_eq!(picked_outlines, 0, "a deleted slice is still drawn picked");
    // A press on nothing picks nothing: Delete falls back to Edit > Clear.
    click(&mut pointer, &mut ed, v(50.0, 14.0));
    let cleared = menu_bridge::perform(action, &mut ed);
    assert!(
        !cleared.as_deref().unwrap_or("").contains("Deleted slice"),
        "a miss still deleted a slice: {cleared:?}"
    );
    // ... and File > Export > Slice Options... has nothing to open over.
    let options = ui::menu::MenuAction::SliceOptions;
    let mut host = app_shell::dialog_host::DialogHost::default();
    assert!(!host.open_for_menu_action(&options, &ed));
    let refused = menu_bridge::perform(options, &mut ed).unwrap_err();
    assert!(refused.contains("Slice Select"), "{refused}");

    // Move the last slice: it keeps its name (slice_03), it is not renumbered.
    let outcomes = drag(
        &mut pointer,
        &mut ed,
        &[v(90.0, 14.0), v(90.0, 34.0), v(90.0, 54.0)],
    );
    all_reached(ToolId::SliceSelect, &outcomes);
    let published: Vec<_> = outcomes.iter().filter_map(|o| o.slices.clone()).collect();
    assert_eq!(published.len(), 1, "one edited set: {outcomes:?}");
    let names: Vec<&str> = published[0].iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["slice_01", "slice_03"],
        "the slices were renumbered"
    );
    assert_eq!(published[0][1].rect, raster::PixelRect::new(80, 44, 20, 20));

    // Slice Options on the first slice through its menu row: pick the slice,
    // click File > Export > Slice Options... (the row resolves, and the
    // chrome's route hands it to the dialog host), type a name, a URL and an
    // alt text into the dialog, press Enter; the confirmation rides the
    // frame's menu picks to the bridge, as every parked dialog's does.
    click(&mut pointer, &mut ed, v(14.0, 14.0));
    let ws = ui::Workspace::new();
    let menu = menu_bridge::context(&mut ed, &ws);
    let intent = menu_bridge::resolve_intent(options, &menu, &ed)
        .unwrap_or_else(|why| panic!("Slice Options is disabled: {why}"));
    assert_eq!(intent, ui::Intent::Action(options));
    assert!(
        host.open_for_menu_action(&options, &ed),
        "Slice Options did not open over the picked slice"
    );
    let typed = type_into_slice_options(
        &mut host,
        &["hero", "https://example.com/hero?a=1&b=2", "The \"hero\""],
    );
    assert!(!host.is_open(), "Enter did not close Slice Options");
    assert_eq!(typed.menu, vec![options], "{typed:?}");
    let said = menu_bridge::perform(options, &mut ed).expect("the options apply");
    assert!(said.contains("hero"), "{said}");
    // The overlay follows the rename in the same frame.
    pointer.begin_pending_session(&mut ed, &[]);
    assert_eq!(
        slice_overlay(&mut pointer),
        (2, vec!["hero".to_string(), "03".into()], Some(0))
    );
    // The store refuses a name another slice has (the dialog blocks it too).
    assert!(
        app_shell::slices_export::set_slice_options(
            &mut ed,
            1,
            tools::slice_select::SliceOptions::named("hero"),
        )
        .is_err(),
        "two slices took one name"
    );

    ui::dialogs::export_as::forget_last_confirmed_entry();
    let said =
        menu_bridge::perform(ui::menu::MenuAction::ExportSlices, &mut ed).expect("the export");
    assert!(said.contains("Exported 2 slice(s)"), "{said}");
    assert!(said.contains("canvas.html"), "{said}");
    let mut names: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["canvas.html", "canvas_03.png", "hero.png"]);
    let moved = raster::decode_path(&out.join("canvas_03.png")).unwrap();
    assert_eq!((moved.width, moved.height), (20, 20));
    // The URL and the alt text are written: the page links the hero slice's
    // image and gives it its alt text, escaped; the other slice has neither.
    let page = std::fs::read_to_string(out.join("canvas.html")).unwrap();
    assert!(
        page.contains(
            "<a href=\"https://example.com/hero?a=1&amp;b=2\"><img src=\"hero.png\" \
             alt=\"The &quot;hero&quot;\" width=\"20\" height=\"20\" \
             style=\"position:absolute;left:4px;top:4px\"></a>"
        ),
        "{page}"
    );
    assert!(
        page.contains("  <img src=\"canvas_03.png\" alt=\"\""),
        "{page}"
    );
}

/// W10-A: the Slice Select overlay as published: how many slices, their
/// labels, and which one (by place in the drawn list) is picked.
fn slice_overlay(pointer: &mut ToolPointer) -> (usize, Vec<String>, Option<usize>) {
    match pointer.live_geometry() {
        Some((
            _,
            tools::SessionGeometry::SliceSelect {
                rects,
                labels,
                picked,
            },
        )) => (rects.len(), labels, picked),
        other => panic!("Slice Select shows no slice set: {other:?}"),
    }
}

/// W10-A: publish the live overlay through the production publisher and draw
/// real chrome frames; the texts painted, and how many rectangles carry the
/// thick selected-handle outline the painter gives the picked slice.
fn painted_slice_overlay(pointer: &mut ToolPointer, ed: &mut Editor) -> (Vec<String>, usize) {
    let ctx = egui::Context::default();
    app_shell::chrome::install_theme(&ctx, design::Theme::Dark);
    let style = ui::canvas::CanvasStyle::from_context(&ctx);
    let picked = style.thick(style.handle_selected);
    let mut chrome = Chrome::new();
    let geometry = pointer.live_geometry();
    chrome.publish_tool_geometry(geometry, ed.active().map(|d| d.id()));
    let mut shapes = Vec::new();
    for _ in 0..2 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            chrome.ui(ctx, ed);
        });
        shapes = output.shapes.into_iter().map(|c| c.shape).collect();
    }
    fn flat(shapes: Vec<egui::Shape>, out: &mut Vec<egui::Shape>) {
        for s in shapes {
            match s {
                egui::Shape::Vec(inner) => flat(inner, out),
                other => out.push(other),
            }
        }
    }
    let mut all = Vec::new();
    flat(shapes, &mut all);
    let texts = all
        .iter()
        .filter_map(|s| match s {
            egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
            _ => None,
        })
        .collect();
    let outlines = all
        .iter()
        .filter(|s| matches!(s, egui::Shape::Rect(r) if r.stroke == picked))
        .count();
    (texts, outlines)
}

/// W10-A: drive the open Slice Options dialog with real keyboard input: the
/// name field has focus when it opens, so Ctrl+A and the first text replace
/// the name; Tab moves to the URL and then the alt text, each typed in turn;
/// Enter confirms. Returns what the frames put in the chrome output.
fn type_into_slice_options(
    host: &mut app_shell::dialog_host::DialogHost,
    fields: &[&str; 3],
) -> ChromeOutput {
    let ctx = egui::Context::default();
    app_shell::chrome::install_theme(&ctx, design::Theme::Dark);
    let mut out = ChromeOutput::default();
    let key = |key: egui::Key, modifiers: egui::Modifiers| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    let mut run = |events: Vec<egui::Event>, modifiers: egui::Modifiers| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            modifiers,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| host.ui(ctx, None, &mut out));
    };
    let none = egui::Modifiers::default();
    for _ in 0..2 {
        run(Vec::new(), none);
    }
    run(
        vec![key(egui::Key::A, egui::Modifiers::COMMAND)],
        egui::Modifiers::COMMAND,
    );
    run(vec![egui::Event::Text(fields[0].into())], none);
    for field in &fields[1..] {
        run(vec![key(egui::Key::Tab, none)], none);
        run(vec![egui::Event::Text((*field).into())], none);
    }
    run(vec![key(egui::Key::Enter, none)], none);
    out
}

#[test]
fn spiral_drag_draws_one_shape_layer_whose_arms_wind_the_chosen_turns() {
    let id = ToolId::Spiral;
    let (_dir, mut ed) = open(&white);
    let mut pointer = ToolPointer::new();
    select_tool(&mut ed, id);
    let draw = |pointer: &mut ToolPointer, ed: &mut Editor, clockwise: bool| {
        let seed = vec![
            ("turns".to_string(), tools::ToolSetting::Float(3.0)),
            ("inner_radius".to_string(), tools::ToolSetting::Float(0.1)),
            (
                "direction".to_string(),
                tools::ToolSetting::Choice(if clockwise { 0 } else { 1 }),
            ),
        ];
        let before_layers = layer_ids(ed);
        let d0 = depth(ed);
        let outcomes = seeded_stroke(
            pointer,
            ed,
            &[v(24.0, 24.0), v(64.0, 64.0), v(104.0, 104.0)],
            &seed,
        );
        all_reached(id, &outcomes);
        assert_eq!(depth(ed), d0 + 1, "one spiral drag is one history entry");
        newest_shape(ed, &before_layers)
    };
    let cw_layer = draw(&mut pointer, &mut ed, true);
    let cw = composite(&mut ed);
    // Ink only inside the drag box.
    assert_eq!(ink_in(&cw, 0, 0, W, H), ink_in(&cw, 23, 23, 105, 105));
    // Walk right from the centre along y = 64: three turns cross the ray
    // three times, so the ink/no-ink state flips about six times.
    let mut flips = 0;
    let mut last = false;
    for x in 64..W {
        let inked = px(&cw, x, 64) != WHITE;
        if inked != last {
            flips += 1;
            last = inked;
        }
    }
    assert!(
        (5..=7).contains(&flips),
        "three turns should cross the ray about six times, got {flips}"
    );
    // Undo removes the layer; the other Direction draws the mirror image.
    undo(&mut ed);
    assert!(!layer_ids(&ed).contains(&cw_layer));
    draw(&mut pointer, &mut ed, false);
    let ccw = composite(&mut ed);
    assert!(ccw != cw, "Counter-clockwise drew the same pixels");
    let (a, b) = (
        ink_in(&cw, 0, 0, W, H) as f64,
        ink_in(&ccw, 0, 0, W, H) as f64,
    );
    assert!(
        (a - b).abs() / a < 0.05,
        "mirror images differ in area: {a} vs {b}"
    );
}

/// The number of built-in custom shapes: the live Shape list starts with
/// them (Heart first, Crescent last) and lists defined ones after.
fn builtin_custom_shapes() -> usize {
    let (heart, _) = tools::registry::custom_shape_at(0);
    assert_eq!(heart, "Heart");
    tools::registry::custom_shape_choices()
        .iter()
        .position(|c| *c == "Crescent")
        .expect("the built-in library ends with the Crescent")
        + 1
}

#[test]
fn define_custom_shape_adds_the_active_shape_to_the_custom_shape_picker_and_the_presets() {
    let (_dir, mut ed) = open(&white);
    let mut pointer = ToolPointer::new();
    let workspace = ui::Workspace::new();
    let action = ui::menu::MenuAction::DefineCustomShape;
    // A pixel layer and no path: the menu row is greyed.
    assert!(
        app::menu_intent(&workspace, &ed, action).is_none(),
        "Define Custom Shape was enabled with nothing to define"
    );
    // A spiral shape layer, drawn through its tool and made active.
    select_tool(&mut ed, ToolId::Spiral);
    let before_layers = layer_ids(&ed);
    all_reached(
        ToolId::Spiral,
        &drag(&mut pointer, &mut ed, &[v(10.0, 10.0), v(60.0, 60.0)]),
    );
    let spiral = newest_shape(&ed, &before_layers);
    ed.set_active_layer(spiral);
    let spiral_px = composite(&mut ed);
    let spiral_ink = ink_in(&spiral_px, 0, 0, W, H);
    let spiral_box = ink_box(&spiral_px);
    assert!(spiral_ink > 100, "the spiral drew {spiral_ink} pixels");
    // The Edit menu row is live and performs.
    assert!(app::menu_intent(&workspace, &ed, action).is_some());
    let d0 = depth(&ed);
    let said = menu_bridge::perform(action, &mut ed).expect("Define Custom Shape performs");
    assert_eq!(depth(&ed), d0, "defining a shape is not a document edit");
    let name = ed
        .active()
        .unwrap()
        .document
        .layers
        .get(spiral)
        .unwrap()
        .name
        .clone();
    assert!(said.contains(&name), "{said}");
    // Listed in the Custom Shape tool's Shape picker...
    let index = tools::registry::custom_shape_choices()
        .iter()
        .position(|c| *c == name)
        .expect("the defined shape is in the Shape list");
    assert!(index >= builtin_custom_shapes());
    // ...kept in the presets, and written to the presets file.
    assert!(ed.presets().shapes().iter().any(|s| s.name == name));
    let file = std::fs::read_to_string(ed.paths().presets_file()).expect("presets written");
    assert!(file.contains(&name), "the presets file does not hold it");

    // Drawn with the Custom Shape tool, it is the spiral again, fitted into
    // the new box: the same ink, in the new place only.
    undo(&mut ed);
    assert!(!layer_ids(&ed).contains(&spiral), "undo removed the spiral");
    assert_eq!(ink_in(&composite(&mut ed), 0, 0, W, H), 0);
    select_tool(&mut ed, ToolId::CustomShape);
    let seed = vec![("preset".to_string(), tools::ToolSetting::Choice(index))];
    let outcomes = seeded_stroke(
        &mut pointer,
        &mut ed,
        &[v(70.0, 70.0), v(120.0, 120.0)],
        &seed,
    );
    all_reached(ToolId::CustomShape, &outcomes);
    let drawn = composite(&mut ed);
    let inside = ink_in(&drawn, 69, 69, 121, 121);
    assert_eq!(ink_in(&drawn, 0, 0, W, H), inside, "ink outside the box");
    // A custom shape is fitted by its own bounds, so the spiral's outline
    // (which does not reach every side of its drag box) now fills the 50 x
    // 50 box: the same ink, scaled by the ratio of the two boxes.
    let (x0, y0, x1, y1) = ink_box(&drawn);
    assert!(
        x1 - x0 >= 48 && y1 - y0 >= 48,
        "the shape does not fill its box: {:?}",
        (x0, y0, x1, y1)
    );
    let (sx0, sy0, sx1, sy1) = spiral_box;
    let scale = f64::from((x1 - x0) * (y1 - y0)) / f64::from((sx1 - sx0) * (sy1 - sy0));
    let expected = spiral_ink as f64 * scale;
    assert!(
        (inside as f64 - expected).abs() / expected < 0.1,
        "the custom shape is not the spiral: {inside} vs {expected:.0} pixels"
    );
}

#[test]
fn link_layers_makes_an_independent_group_per_click_and_a_move_takes_its_group_only() {
    let (_dir, mut ed) = open(&white);
    let mut pointer = ToolPointer::new();
    let add = |ed: &mut Editor, name: &str| {
        let doc = ed.active_mut().unwrap();
        let id = doc.add_layer(layer_model::Layer::raster(name));
        doc.fill_layer(id, BLUE);
        id
    };
    let [a, b, c, d, e, f] = ["A", "B", "C", "D", "E", "F"].map(|n| add(&mut ed, n));
    let at = |ed: &Editor, id| {
        ed.active()
            .unwrap()
            .document
            .layers
            .get(id)
            .unwrap()
            .transform
            .translation
    };
    let group = |ed: &Editor, id| {
        ed.active()
            .unwrap()
            .document
            .layers
            .get(id)
            .unwrap()
            .link_group
    };
    let link = |ed: &mut Editor, ids: [layer_model::LayerId; 2]| {
        ed.set_layer_selection(ids.to_vec(), Some(ids[0]));
        let d0 = depth(ed);
        menu_bridge::perform(ui::menu::MenuAction::LinkLayers, ed).expect("Link Layers performs");
        assert_eq!(depth(ed), d0 + 1, "one click is one history entry");
    };
    link(&mut ed, [a, b]);
    link(&mut ed, [c, d]);
    assert!(group(&ed, a).is_some() && group(&ed, a) == group(&ed, b));
    assert!(group(&ed, c).is_some() && group(&ed, c) == group(&ed, d));
    assert_ne!(
        group(&ed, a),
        group(&ed, c),
        "the second click joined the first group"
    );
    // An old document's single chain: E and F carry only the legacy flag.
    for id in [e, f] {
        ed.active_mut().unwrap().set_props(
            id,
            editor_core::LayerPatch {
                linked: Some(true),
                ..editor_core::LayerPatch::default()
            },
        );
    }

    let move_layer = |pointer: &mut ToolPointer, ed: &mut Editor, id| {
        ed.set_layer_selection(vec![id], Some(id));
        select_tool(ed, ToolId::Move);
        let outcomes = drag(pointer, ed, &[v(64.0, 64.0), v(74.0, 64.0), v(84.0, 64.0)]);
        all_reached(ToolId::Move, &outcomes);
    };
    let moved = Vec2::new(20.0, 0.0);
    // Move A: B comes along; C, D, E and F stay.
    move_layer(&mut pointer, &mut ed, a);
    assert_eq!(at(&ed, a), moved);
    assert_eq!(at(&ed, b), moved, "A's group partner did not move");
    for id in [c, d, e, f] {
        assert_eq!(at(&ed, id), Vec2::ZERO, "another group moved");
    }
    // Move D: only its own group.
    move_layer(&mut pointer, &mut ed, d);
    assert_eq!(at(&ed, c), moved);
    assert_eq!(at(&ed, a), moved, "group one moved again");
    assert_eq!(at(&ed, e), Vec2::ZERO);
    // Move E: the legacy chain moves as one group (E and F), nothing else.
    move_layer(&mut pointer, &mut ed, e);
    assert_eq!(at(&ed, f), moved, "the old single chain is one group");
    assert_eq!(at(&ed, a), moved);
    assert_eq!(at(&ed, c), moved);

    // Link Layers on a selection that is exactly one group unlinks it.
    link(&mut ed, [a, b]);
    assert_eq!(group(&ed, a), None);
    assert!(!ed.active().unwrap().document.layers.get(a).unwrap().linked);
    move_layer(&mut pointer, &mut ed, a);
    assert_eq!(at(&ed, a), moved * 2.0);
    assert_eq!(at(&ed, b), moved, "an unlinked layer followed");
    // The unlink undoes in one step.
    undo(&mut ed); // the last move
    undo(&mut ed); // the unlink
    assert!(group(&ed, a).is_some(), "undo did not restore the group");
    assert_eq!(group(&ed, a), group(&ed, b));
}

/// One chrome frame on `ctx` with `events` and `modifiers` held, its output
/// applied to the editor the way `shell.rs` `apply_chrome` applies it: the
/// layer selection first, then the document commands in order.
fn applied_chrome_frame(
    ctx: &egui::Context,
    chrome: &mut Chrome,
    ed: &mut Editor,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 2000.0),
        )),
        events,
        modifiers,
        ..Default::default()
    };
    let mut out = ChromeOutput::default();
    let _ = ctx.run(input, |ctx| {
        out = chrome.ui(ctx, ed);
    });
    if let Some((layers, active)) = out.select_layers {
        ed.set_layer_selection(layers, active);
    } else if let Some(id) = out.select_layer {
        ed.set_active_layer(id);
    }
    for command in out.commands {
        ed.apply_command(command);
    }
}

/// Press and release the primary button over the widget `id` as the last
/// frame drew it, `modifiers` held (Ctrl adds a Layers row to the panel's
/// selection), each frame applied by [`applied_chrome_frame`].
fn applied_chrome_click(
    ctx: &egui::Context,
    chrome: &mut Chrome,
    ed: &mut Editor,
    id: egui::Id,
    modifiers: egui::Modifiers,
) {
    // Settle first: the rows re-lay out after the last click's edit lands.
    for _ in 0..2 {
        applied_chrome_frame(ctx, chrome, ed, Vec::new(), egui::Modifiers::default());
    }
    let pos = ctx
        .read_response(id)
        .unwrap_or_else(|| panic!("{id:?} was never drawn"))
        .rect
        .center();
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    };
    applied_chrome_frame(
        ctx,
        chrome,
        ed,
        vec![egui::Event::PointerMoved(pos), button(true)],
        modifiers,
    );
    applied_chrome_frame(ctx, chrome, ed, vec![button(false)], modifiers);
    applied_chrome_frame(ctx, chrome, ed, Vec::new(), egui::Modifiers::default());
}

/// W10-A, the Layers panel's own link button (the footer's first button),
/// driven through real chrome frames: every click links the rows selected in
/// the panel into a group of their own, a Move drag takes its own group only,
/// and the same button unlinks, also a group Layer > Link Layers made.
#[test]
fn the_layers_panel_link_button_makes_a_group_per_click_and_unlinks_what_it_shows() {
    let (_dir, mut ed) = open(&white);
    let mut pointer = ToolPointer::new();
    let add = |ed: &mut Editor, name: &str| {
        let doc = ed.active_mut().unwrap();
        let id = doc.add_layer(layer_model::Layer::raster(name));
        doc.fill_layer(id, BLUE);
        id
    };
    let [a, b, c, d, e, f] = ["A", "B", "C", "D", "E", "F"].map(|n| add(&mut ed, n));
    let layer = |ed: &Editor, id| {
        ed.active()
            .unwrap()
            .document
            .layers
            .get(id)
            .unwrap()
            .clone()
    };
    let at = |ed: &Editor, id| layer(ed, id).transform.translation;

    let ctx = egui::Context::default();
    app_shell::chrome::install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    for _ in 0..3 {
        applied_chrome_frame(
            &ctx,
            &mut chrome,
            &mut ed,
            Vec::new(),
            egui::Modifiers::default(),
        );
    }
    let plain = egui::Modifiers::default();
    let ctrl = egui::Modifiers::COMMAND;
    let row = ui::view::ids::layer_row;
    let link_button = ui::view::ids::layer_link();
    // Select two rows in the panel (a click, then a Ctrl+click) and press
    // the link button.
    let mut link_in_panel = |ed: &mut Editor, pair: [layer_model::LayerId; 2]| {
        applied_chrome_click(&ctx, &mut chrome, ed, row(pair[0]), plain);
        applied_chrome_click(&ctx, &mut chrome, ed, row(pair[1]), ctrl);
        applied_chrome_click(&ctx, &mut chrome, ed, link_button, plain);
    };

    link_in_panel(&mut ed, [a, b]);
    link_in_panel(&mut ed, [c, d]);
    for id in [a, b, c, d] {
        assert!(layer(&ed, id).linked, "the panel did not link {id:?}");
    }
    let g1 = layer(&ed, a).link_group;
    let g2 = layer(&ed, c).link_group;
    assert!(g1.is_some(), "the panel's link left no group id");
    assert_eq!(layer(&ed, b).link_group, g1, "one click, two groups");
    assert_eq!(layer(&ed, d).link_group, g2, "one click, two groups");
    assert_ne!(g1, g2, "the second panel click joined the first group");

    let move_layer = |pointer: &mut ToolPointer, ed: &mut Editor, id| {
        ed.set_layer_selection(vec![id], Some(id));
        select_tool(ed, ToolId::Move);
        let outcomes = drag(pointer, ed, &[v(64.0, 64.0), v(74.0, 64.0), v(84.0, 64.0)]);
        all_reached(ToolId::Move, &outcomes);
    };
    let moved = Vec2::new(20.0, 0.0);
    move_layer(&mut pointer, &mut ed, a);
    assert_eq!(
        at(&ed, b),
        moved,
        "A's panel-made group did not move with it"
    );
    for id in [c, d, e, f] {
        assert_eq!(at(&ed, id), Vec2::ZERO, "the other panel group moved");
    }
    move_layer(&mut pointer, &mut ed, d);
    assert_eq!(at(&ed, c), moved);
    assert_eq!(at(&ed, a), moved, "group one moved with group two");

    // The panel's button unlinks what it shows linked: A and B.
    link_in_panel(&mut ed, [a, b]);
    assert!(!layer(&ed, a).linked && !layer(&ed, b).linked);
    move_layer(&mut pointer, &mut ed, a);
    assert_eq!(at(&ed, a), moved * 2.0);
    assert_eq!(at(&ed, b), moved, "B (badge unlinked) still moved with A");

    // A group Layer > Link Layers made, unlinked from the panel.
    ed.set_layer_selection(vec![e, f], Some(e));
    menu_bridge::perform(ui::menu::MenuAction::LinkLayers, &mut ed).expect("Link Layers performs");
    assert!(layer(&ed, e).linked && layer(&ed, e).link_group.is_some());
    link_in_panel(&mut ed, [e, f]);
    assert!(!layer(&ed, e).linked, "the panel did not unlink E");
    move_layer(&mut pointer, &mut ed, e);
    assert_eq!(at(&ed, e), moved);
    assert_eq!(
        at(&ed, f),
        Vec2::ZERO,
        "F (badge unlinked) still moved with E: the menu's group outlived the panel's unlink"
    );
}

/// The half-open bounding box of the non-white pixels of `buf`.
fn ink_box(buf: &[u8]) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (W, H, 0, 0);
    for y in 0..H {
        for x in 0..W {
            if px(buf, x, y) != WHITE {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    (x0, y0, x1, y1)
}

#[test]
fn define_custom_shape_takes_the_paths_panels_current_path_over_a_pixel_layer() {
    let (_dir, mut ed) = open(&white);
    let mut ws = ui::Workspace::new();
    let action = ui::menu::MenuAction::DefineCustomShape;
    // A pixel layer and no path: the row is off, with its reason.
    let ctx = menu_bridge::context(&mut ed, &ws);
    let off = menu_bridge::resolve(action, &ctx, &ed).expect_err("the row is off");
    assert!(off.contains("shape layer or a path"), "{off}");
    // The pen's Work Path is current (the Star outline stands in for a
    // drawn path): the row is on and performs.
    let (_, star) = tools::registry::custom_shape_at(1);
    ws.paths.work_path = Some(star);
    ws.paths.work_selected = true;
    let ctx = menu_bridge::context(&mut ed, &ws);
    let picked = menu_bridge::resolve(action, &ctx, &ed).expect("the row is on");
    let menu_bridge::Pick::Menu(a) = picked else {
        panic!("not a menu operation: {picked:?}");
    };
    let before = tools::registry::custom_shape_choices().len();
    let said = menu_bridge::perform(a, &mut ed).expect("Define Custom Shape performs");
    assert!(said.contains("Custom Shape"), "{said}");
    let list = tools::registry::custom_shape_choices();
    assert!(list.len() > before, "nothing joined the Shape list");
    assert!(
        list.iter()
            .skip(builtin_custom_shapes())
            .any(|c| c.starts_with("Custom Shape") && said.contains(c)),
        "the defined shape is not listed: {said} / {list:?}"
    );
}

/// W10-B: the Note tool, from the palette pick through the shell's pointer
/// route: a click on the canvas pins a note there (a document record, one
/// undo step, no pixel touched), a click in the pasteboard pins nothing, and
/// undo takes the note away.
#[test]
fn note_click_pins_a_note_on_the_document_in_one_undo_step() {
    let (_dir, mut ed) = open(&halves);
    let mut pointer = ToolPointer::new();
    let id = ToolId::Note;
    select_tool(&mut ed, id);
    let before = composite(&mut ed);
    let d0 = depth(&ed);

    let outcomes = click(&mut pointer, &mut ed, v(30.5, 20.25));
    all_reached(id, &outcomes);
    let notes = ed.active().unwrap().document.extras.notes.clone();
    assert_eq!(notes.len(), 1, "{outcomes:?}");
    assert_eq!((notes[0].x, notes[0].y), (30.5, 20.25));
    assert_eq!(notes[0].text, ui::panels::notes::NEW_NOTE_TEXT);
    assert_eq!(depth(&ed), d0 + 1, "the pin is one history entry");
    assert_eq!(composite(&mut ed), before, "a note is not pixels");

    click(&mut pointer, &mut ed, v(-40.0, 20.0));
    assert_eq!(ed.active().unwrap().document.extras.notes.len(), 1);

    undo(&mut ed);
    assert!(ed.active().unwrap().document.extras.notes.is_empty());
}
