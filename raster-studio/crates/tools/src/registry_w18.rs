//! W18-F: what the options bar and the application shell tell each other
//! beyond the option values themselves.
//!
//! The bar (`ui`) and the shell (`app-shell`) both depend on this crate and
//! run on the one UI thread, so the few facts that are not a tool option
//! travel through thread-local slots here, the way the panels' requests
//! travel through `ui::panels::panel_menus_w16`:
//!
//! * the shell publishes, every frame, whether the live tool holds an edit
//!   Enter would confirm ([`publish_pending`]) - a Type run, a Perspective
//!   Crop quad, a Show Transform Controls drag - so the bar draws Photopea's
//!   Cancel cross and Commit check for it;
//! * the bar posts the requests the shell performs with the document in
//!   hand: the Cancel cross ([`post_cancel`]), a Crop by row
//!   ([`post_crop_by`]) and a Paint Bucket pattern pick
//!   ([`post_pattern_pick`]);
//! * the bar's view toggles (the Zoom bar's Zoom In / Zoom Out, the Zoom and
//!   Hand bars' All Documents, [`NavPrefs`]) and the Clone Stamp / Healing
//!   Brush source picks (the Alt toggle and the K key) are read by the
//!   shell's pointer route.

use std::cell::{Cell, RefCell};

use crate::tool::ToolId;

/// Photopea's Crop by rows, in its order: the box the Crop tool is given
/// (and then waits for the commit on) is the bounds of all layers, of the
/// current layer, of the non-transparent composite, or of the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CropBy {
    AllLayers,
    CurrentLayer,
    Trim,
    Selection,
}

impl CropBy {
    /// Every row, in the list's order.
    pub const ALL: [CropBy; 4] = [
        CropBy::AllLayers,
        CropBy::CurrentLayer,
        CropBy::Trim,
        CropBy::Selection,
    ];
}

/// The Zoom and Hand bars' toggles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NavPrefs {
    /// The Zoom bar's Zoom Out: a click steps out (Alt then steps in).
    pub zoom_out: bool,
    /// The Zoom bar's All Documents: a zoom is applied to every open
    /// document.
    pub zoom_all_documents: bool,
    /// The Hand bar's All Documents: a pan is applied to every open
    /// document.
    pub hand_all_documents: bool,
}

thread_local! {
    static PENDING: Cell<Option<(ToolId, bool)>> = const { Cell::new(None) };
    static CANCEL: Cell<bool> = const { Cell::new(false) };
    static CROP_BY: Cell<Option<CropBy>> = const { Cell::new(None) };
    static NAV: Cell<NavPrefs> = const {
        Cell::new(NavPrefs {
            zoom_out: false,
            zoom_all_documents: false,
            hand_all_documents: false,
        })
    };
    static SELECT_SOURCE: Cell<bool> = const { Cell::new(false) };
    static SOURCE_KEY: Cell<bool> = const { Cell::new(false) };
    static PATTERNS: RefCell<(Vec<String>, Option<String>)> =
        const { RefCell::new((Vec::new(), None)) };
    static PATTERN_PICK: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The shell: whether `tool`, the live tool, holds an edit Enter would
/// confirm. `None` forgets it (a new pointer, no live tool).
pub fn publish_pending(state: Option<(ToolId, bool)>) {
    PENDING.with(|p| p.set(state));
}

/// The bar: what the shell last published for `tool`, `None` when it
/// published nothing for that tool.
pub fn pending_for(tool: ToolId) -> Option<bool> {
    PENDING
        .with(Cell::get)
        .and_then(|(t, pending)| (t == tool).then_some(pending))
}

/// The bar's Cancel cross: discard the held edit (a Type run included,
/// which Escape would commit instead).
pub fn post_cancel() {
    CANCEL.with(|c| c.set(true));
}

/// The shell: take a posted Cancel.
pub fn take_cancel() -> bool {
    CANCEL.with(|c| c.replace(false))
}

/// The bar's Crop by row: set the crop box to `by`.
pub fn post_crop_by(by: CropBy) {
    CROP_BY.with(|c| c.set(Some(by)));
}

/// The shell: take a posted Crop by row.
pub fn take_crop_by() -> Option<CropBy> {
    CROP_BY.with(|c| c.take())
}

/// The Zoom and Hand bars' toggles as they stand.
pub fn nav() -> NavPrefs {
    NAV.with(Cell::get)
}

/// Set the Zoom and Hand bars' toggles.
pub fn set_nav(prefs: NavPrefs) {
    NAV.with(|n| n.set(prefs));
}

/// The Clone Stamp / Healing Brush bar's Alt toggle (Photopea's "Select
/// Source"): the next press picks the source, then the toggle pops out.
pub fn arm_select_source(on: bool) {
    SELECT_SOURCE.with(|s| s.set(on));
}

/// Whether the Alt toggle is in.
pub fn select_source_armed() -> bool {
    SELECT_SOURCE.with(Cell::get)
}

/// The shell, at a press: take the Alt toggle (it pops out).
pub fn take_select_source() -> bool {
    SELECT_SOURCE.with(|s| s.replace(false))
}

/// The bar, every frame: whether K is held (Photopea: "Select clone source
/// by holding Alt (or K) and clicking on the image").
pub fn set_source_key_held(held: bool) {
    SOURCE_KEY.with(|s| s.set(held));
}

/// Whether K is held.
pub fn source_key_held() -> bool {
    SOURCE_KEY.with(Cell::get)
}

/// The shell: the defined patterns' names and the active one.
pub fn publish_patterns(names: Vec<String>, active: Option<String>) {
    PATTERNS.with(|p| *p.borrow_mut() = (names, active));
}

/// The bar: the pattern names the shell published, and the active one.
pub fn patterns() -> (Vec<String>, Option<String>) {
    PATTERNS.with(|p| p.borrow().clone())
}

/// The bar's pattern picker: make `name` the active pattern.
pub fn post_pattern_pick(name: String) {
    PATTERN_PICK.with(|p| *p.borrow_mut() = Some(name));
}

/// The shell: take a posted pattern pick.
pub fn take_pattern_pick() -> Option<String> {
    PATTERN_PICK.with(|p| p.borrow_mut().take())
}

/// Every slot back to its start, for a test that must not inherit another
/// test's state on a reused thread.
pub fn reset() {
    publish_pending(None);
    CANCEL.with(|c| c.set(false));
    CROP_BY.with(|c| c.set(None));
    set_nav(NavPrefs::default());
    arm_select_source(false);
    set_source_key_held(false);
    publish_patterns(Vec::new(), None);
    PATTERN_PICK.with(|p| *p.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_taken_once_and_pending_answers_only_its_tool() {
        reset();
        publish_pending(Some((ToolId::Type, true)));
        assert_eq!(pending_for(ToolId::Type), Some(true));
        assert_eq!(pending_for(ToolId::Crop), None);
        post_cancel();
        assert!(take_cancel());
        assert!(!take_cancel());
        post_crop_by(CropBy::Trim);
        assert_eq!(take_crop_by(), Some(CropBy::Trim));
        assert_eq!(take_crop_by(), None);
        arm_select_source(true);
        assert!(select_source_armed());
        assert!(take_select_source());
        assert!(!select_source_armed(), "the toggle pops out once used");
        post_pattern_pick("Dots".into());
        assert_eq!(take_pattern_pick().as_deref(), Some("Dots"));
        assert_eq!(take_pattern_pick(), None);
        reset();
        assert_eq!(pending_for(ToolId::Type), None);
    }
}
