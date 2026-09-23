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

#[test]
fn background_eraser_clears_only_the_colour_first_touched() {
    let id = ToolId::BackgroundEraser;
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
    assert_eq!(commit.steps, 0, "a slice set is not an edit: {commit:?}");
    assert_eq!(depth(&ed), d0);
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
