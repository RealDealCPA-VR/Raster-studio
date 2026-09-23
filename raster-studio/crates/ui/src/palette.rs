//! The tool palette: one column of slots, driven entirely by the registry.
//!
//! # Slots, not tools
//!
//! The palette does not show fifty buttons. It shows one button per *slot* —
//! the tools that declare the same `ToolInfo::slot` id, which is Photopea's
//! grouping (Blur/Sharpen/Smudge share a slot with no letter between them;
//! Rotate View sits under Hand on its own `R`) — with the rest reachable from a
//! fly-out. [`PaletteModel::build`] derives the whole thing from
//! `tools::registry::all()`, so a new tool joins the palette by declaring a
//! slot, and `every_tool_is_reachable_from_exactly_one_slot` fails if one that
//! declares a slot ever is not. A tool with no slot (Free Transform) is a menu
//! item and a chord, not a button.
//!
//! # The footer
//!
//! Under the colour wells sit Photopea's `Q` and `F`: the quick-mask toggle,
//! whose engaged state is [`PaletteState::quick_mask`], and the screen-mode
//! cycle, whose state is [`PaletteState::screen_mode`]. Both are *mirrors*:
//! the application owns the mode (the editor's quick mask and screen mode),
//! writes them into the palette before every frame, and the footer only
//! draws them. A click on `Q` raises the Select menu's action; a click on `F`
//! raises a cycle request the chrome turns into its screen-mode action
//! ([`PaletteState::take_screen_mode_cycle`]). Neither flips its own flag, so
//! a refused toggle never leaves a control lit over an unchanged screen.
//!
//! # The slot remembers
//!
//! A slot shows the variant last used from it, the way every editor does: pick
//! the elliptical marquee once and the `M` button keeps showing an ellipse. The
//! memory lives in [`PaletteState`], not in the model, because the model is
//! rebuilt from the registry every frame.

use std::collections::HashMap;

use tools::{registry, ToolGroup, ToolId, ToolInfo};

/// One button of the palette, plus its fly-out variants.
#[derive(Clone, PartialEq, Debug)]
pub struct PaletteSlot {
    /// The registry's slot id (`ToolInfo::slot`), shared by every tool here.
    pub id: &'static str,
    pub group: ToolGroup,
    /// The key of the slot's first tool, when it declares one. A slot's tools
    /// may answer to different letters (Hand `H`, Rotate View `R`) or to none
    /// (the blur brushes); this is the key the button's tooltip shows.
    pub shortcut: Option<char>,
    /// The tools in the slot, in registry order. Never empty.
    pub tools: Vec<ToolId>,
}

impl PaletteSlot {
    /// The first tool, which is what an untouched slot shows.
    pub fn primary(&self) -> ToolId {
        self.tools[0]
    }

    /// `true` when the slot has variants worth a fly-out.
    pub fn has_variants(&self) -> bool {
        self.tools.len() > 1
    }
}

/// The palette, derived from the registry.
#[derive(Clone, PartialEq, Debug)]
pub struct PaletteModel {
    slots: Vec<PaletteSlot>,
}

impl Default for PaletteModel {
    fn default() -> Self {
        Self::build()
    }
}

impl PaletteModel {
    /// Group the registry into slots, preserving registry order.
    ///
    /// Tools declaring the same `ToolInfo::slot` land in one slot, whatever
    /// their shortcuts; a tool declaring no slot is left out, because it is
    /// reached from a menu and a chord instead (Free Transform, `Ctrl+T`).
    /// Grouping by slot rather than by letter is what lets the four keyless
    /// blur brushes share one button instead of taking four.
    pub fn build() -> Self {
        let mut slots: Vec<PaletteSlot> = Vec::new();
        for info in registry::all() {
            let Some(id) = info.slot else {
                continue;
            };
            match slots.iter_mut().find(|s| s.id == id) {
                Some(slot) => slot.tools.push(info.id),
                None => slots.push(PaletteSlot {
                    id,
                    group: info.group,
                    shortcut: info.shortcut,
                    tools: vec![info.id],
                }),
            }
        }
        Self { slots }
    }

    /// The slot ids, top to bottom — the column as a reader would list it.
    pub fn slot_ids(&self) -> Vec<&'static str> {
        self.slots.iter().map(|s| s.id).collect()
    }

    pub fn slots(&self) -> &[PaletteSlot] {
        &self.slots
    }

    /// The slot a tool lives in.
    pub fn slot_of(&self, tool: ToolId) -> Option<usize> {
        self.slots.iter().position(|s| s.tools.contains(&tool))
    }

    /// The slots of one palette group, in order. Used to draw the dividers.
    pub fn groups(&self) -> Vec<(ToolGroup, Vec<usize>)> {
        let mut out: Vec<(ToolGroup, Vec<usize>)> = Vec::new();
        for (index, slot) in self.slots.iter().enumerate() {
            match out.last_mut() {
                Some((group, members)) if *group == slot.group => members.push(index),
                _ => out.push((slot.group, vec![index])),
            }
        }
        out
    }
}

/// Photopea's `F` cycle: how much chrome surrounds the canvas.
///
/// Photopea's three screen modes, meant for the chrome to *read* through the
/// three predicates below alongside the editor's own `Tab` panels flag. It is
/// a view setting like the ruler unit, so it is not an intent and never
/// reaches a document.
///
/// The application owns the value (the editor's `screen_mode`, cycled by its
/// `CycleScreenMode` action on plain `F` and on the footer's control) and
/// mirrors it into [`PaletteState::screen_mode`] before each frame; the chrome
/// gates the options bar, tool column and docks on [`ScreenMode::panels_visible`]
/// and the menu bar on [`ScreenMode::menu_visible`], and the shell sets the
/// window borderless full screen from [`ScreenMode::fullscreen`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ScreenMode {
    /// Everything: menu, options bar, tools, docks, status.
    #[default]
    Standard,
    /// The canvas fills the window under the menu bar; the panels are gone.
    FullScreenWithMenu,
    /// The canvas alone, in a borderless window.
    FullScreen,
}

impl ScreenMode {
    /// Every mode, in `F` order.
    pub const ALL: &'static [ScreenMode] = &[
        ScreenMode::Standard,
        ScreenMode::FullScreenWithMenu,
        ScreenMode::FullScreen,
    ];

    /// The mode one press of `F` leads to; wraps back to Standard.
    pub const fn next(self) -> ScreenMode {
        match self {
            ScreenMode::Standard => ScreenMode::FullScreenWithMenu,
            ScreenMode::FullScreenWithMenu => ScreenMode::FullScreen,
            ScreenMode::FullScreen => ScreenMode::Standard,
        }
    }

    /// Whether the tool rail, the options bar and the docks are drawn.
    pub const fn panels_visible(self) -> bool {
        matches!(self, ScreenMode::Standard)
    }

    /// Whether the menu bar is drawn.
    pub const fn menu_visible(self) -> bool {
        !matches!(self, ScreenMode::FullScreen)
    }

    /// Whether the window should be borderless full screen.
    pub const fn fullscreen(self) -> bool {
        !matches!(self, ScreenMode::Standard)
    }

    /// The name the footer's tooltip and a menu would show. Bare English
    /// here, like `MenuAction::label`, because the string is this type's, not
    /// the view's.
    pub const fn label(self) -> &'static str {
        match self {
            ScreenMode::Standard => "Standard Screen Mode",
            ScreenMode::FullScreenWithMenu => "Full Screen Mode With Menu Bar",
            ScreenMode::FullScreen => "Full Screen Mode",
        }
    }
}

/// The footer's quick-mask (`Q`) control, under a stable id so a headless
/// test can find and click it. Lives here rather than in `view::ids` because
/// the footer is the palette's, and so is its state.
pub fn quick_mask_control() -> egui::Id {
    egui::Id::new("raster-palette-quick-mask")
}

/// The footer's screen-mode (`F`) control.
pub fn screen_mode_control() -> egui::Id {
    egui::Id::new("raster-palette-screen-mode")
}

/// What the palette remembers between frames.
#[derive(Clone, PartialEq, Debug)]
pub struct PaletteState {
    active: ToolId,
    /// The variant last chosen from each slot, by slot index.
    last_used: HashMap<usize, ToolId>,
    /// The slot whose fly-out is open, if any.
    pub open_flyout: Option<usize>,
    /// A press-and-hold in flight: the slot pressed and the egui time it went
    /// down. Holding past [`HOLD_SECONDS`] opens the fly-out without a click.
    pub hold: Option<(usize, f64)>,
    /// Whether quick-mask mode is engaged, as the footer's `Q` control shows
    /// it. The control never sets this itself: a click only emits the menu
    /// action, and the engaged look follows this flag, which is the editor's
    /// to mirror in — the same door [`PaletteState::activate`] is for the
    /// active tool — so a refused toggle (no document) can never leave the
    /// control lit. The chrome's `sync_workspace` writes it from
    /// `Editor::quick_mask()` every frame, whichever route toggled it (the
    /// footer, the `Q` chord, the Select menu).
    pub quick_mask: bool,
    /// The footer's `F` cycle, mirrored from the editor's screen mode by the
    /// chrome every frame; the control lights the two full-screen modes
    /// ([`ScreenMode::fullscreen`]) and never cycles this itself.
    pub screen_mode: ScreenMode,
    /// A click on the footer's `F` since the chrome last asked. The control
    /// cannot raise an `Intent` for the mode (there is no menu item for it),
    /// so this is its outbox: the chrome takes it and performs the
    /// application's own screen-mode action.
    screen_mode_requested: bool,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self {
            active: ToolId::Brush,
            last_used: HashMap::new(),
            open_flyout: None,
            hold: None,
            quick_mask: false,
            screen_mode: ScreenMode::Standard,
            screen_mode_requested: false,
        }
    }
}

/// How long a press-and-hold on a variant tool's slot opens its fly-out.
pub const HOLD_SECONDS: f64 = 0.3;

impl PaletteState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The footer's `F` was clicked: ask the application to cycle the screen
    /// mode. The mode itself is left alone — it is the editor's, and comes
    /// back through [`PaletteState::screen_mode`] once performed.
    pub fn request_screen_mode_cycle(&mut self) {
        self.screen_mode_requested = true;
    }

    /// Whether `F` was clicked since the last call; clears the request.
    pub fn take_screen_mode_cycle(&mut self) -> bool {
        std::mem::take(&mut self.screen_mode_requested)
    }

    pub fn active(&self) -> ToolId {
        self.active
    }

    /// Make a tool active, remembering it as its slot's variant.
    ///
    /// Returns `true` when the active tool changed, which is what decides
    /// whether an [`crate::Intent::SelectTool`] is worth emitting.
    ///
    /// Deliberately leaves [`PaletteState::open_flyout`] alone. It used to
    /// close the fly-out here, which made the fly-out impossible to close by
    /// clicking its own button: the caller asked "did anything change?", got
    /// `false` for the already-active tool, and toggled the flag straight back
    /// on — over a flag `activate` had just cleared. Closing is now the call
    /// site's decision, made with [`PaletteState::close_flyout`].
    pub fn activate(&mut self, model: &PaletteModel, tool: ToolId) -> bool {
        if let Some(slot) = model.slot_of(tool) {
            self.last_used.insert(slot, tool);
        }
        let changed = self.active != tool;
        self.active = tool;
        changed
    }

    /// The tool a slot's button shows: the active one if it is in this slot,
    /// otherwise the variant last used from it, otherwise the first.
    pub fn representative(&self, model: &PaletteModel, slot: usize) -> ToolId {
        let Some(s) = model.slots().get(slot) else {
            return self.active;
        };
        if s.tools.contains(&self.active) {
            return self.active;
        }
        self.last_used
            .get(&slot)
            .copied()
            .filter(|t| s.tools.contains(t))
            .unwrap_or_else(|| s.primary())
    }

    /// `true` when a slot holds the active tool, so its button reads as
    /// selected.
    pub fn slot_is_active(&self, model: &PaletteModel, slot: usize) -> bool {
        model
            .slots()
            .get(slot)
            .is_some_and(|s| s.tools.contains(&self.active))
    }

    /// The tool a keypress selects.
    ///
    /// Delegates to `registry::cycle`, so the palette and the keymap cannot
    /// disagree about what `M` does after `M`.
    pub fn tool_for_key(&self, key: char) -> Option<ToolId> {
        registry::cycle(key, Some(self.active))
    }

    pub fn toggle_flyout(&mut self, slot: usize) {
        self.open_flyout = if self.open_flyout == Some(slot) {
            None
        } else {
            Some(slot)
        };
    }

    /// Shut the fly-out, whichever slot it belongs to. Returns `true` when one
    /// was open.
    pub fn close_flyout(&mut self) -> bool {
        self.open_flyout.take().is_some()
    }

    /// What a left-click on a palette slot means.
    ///
    /// Split out from the drawing so the fly-out's whole state machine is
    /// testable without a window — it is what
    /// `clicking_the_slot_of_the_active_tool_opens_then_shuts_its_flyout`
    /// drives.
    pub fn click_slot(&mut self, model: &PaletteModel, slot: usize) -> SlotClick {
        let Some(entry) = model.slots().get(slot) else {
            return SlotClick::Nothing;
        };
        let has_variants = entry.has_variants();
        let open_here = self.open_flyout == Some(slot);
        let tool = self.representative(model, slot);

        if self.activate(model, tool) {
            self.close_flyout();
            return SlotClick::Selected(tool);
        }
        // The tool was already active, so the click has nothing else to mean
        // than "show me the variants" — or, if they are already showing, "put
        // them away". Right-click takes the same two branches, which is why
        // the two gestures no longer disagree.
        if has_variants && !open_here {
            self.open_flyout = Some(slot);
            return SlotClick::OpenedFlyout(slot);
        }
        if self.close_flyout() {
            SlotClick::ClosedFlyout
        } else {
            SlotClick::Nothing
        }
    }
}

/// What a left-click on a palette slot did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotClick {
    /// The active tool changed, and an [`crate::Intent::SelectTool`] is owed.
    Selected(ToolId),
    /// The slot's fly-out opened.
    OpenedFlyout(usize),
    /// The open fly-out closed.
    ClosedFlyout,
    /// Nothing happened: an already-active tool with no variants.
    Nothing,
}

/// The registry entry for a tool, or a panic-free fallback.
///
/// Every [`ToolId`] is in the registry — `tools` has a test for it — so `None`
/// is unreachable in practice; this exists so no drawing path needs an
/// `unwrap`.
pub fn info(tool: ToolId) -> Option<&'static ToolInfo> {
    registry::info(tool)
}

/// The tooltip a palette button shows: the tool's name and its key.
pub fn tooltip(info: &ToolInfo) -> String {
    match info.shortcut {
        Some(key) => format!("{} ({})", info.name, key.to_ascii_uppercase()),
        None => info.name.to_string(),
    }
}

/// Palette-group heading, used for the fly-out and for accessibility labels.
pub const fn group_label(group: ToolGroup) -> &'static str {
    match group {
        ToolGroup::Select => "Selection",
        ToolGroup::Crop => "Crop & Slice",
        ToolGroup::Retouch => "Retouch",
        ToolGroup::Paint => "Paint",
        ToolGroup::Draw => "Draw",
        ToolGroup::Navigate => "Navigate",
        ToolGroup::Transform => "Transform",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_tool_is_reachable_from_exactly_one_slot() {
        let m = PaletteModel::build();
        let seen: Vec<ToolId> = m.slots().iter().flat_map(|s| s.tools.clone()).collect();
        let unique: HashSet<ToolId> = seen.iter().copied().collect();
        assert_eq!(unique.len(), seen.len(), "a tool is in two slots");
        // Every tool that declares a slot, and only those, in registry order
        // (slots are contiguous runs of the registry, so flattening them
        // gives the registry back): a tool with no slot is a menu item, and
        // putting it on the column anyway would be the palette overruling the
        // registry.
        let expected: Vec<ToolId> = registry::all()
            .iter()
            .filter(|i| i.slot.is_some())
            .map(|i| i.id)
            .collect();
        assert_eq!(seen, expected, "the palette and the registry disagree");
        assert_eq!(
            m.slot_of(ToolId::FreeTransform),
            None,
            "Free Transform is Ctrl+T and the Edit menu, not a button"
        );
    }

    /// Photopea's column, top to bottom. The registry is the source of the
    /// order; this pins that the palette reproduces it slot for slot.
    #[test]
    fn the_column_is_photopeas_nineteen_slots_in_photopeas_order() {
        let m = PaletteModel::build();
        assert_eq!(
            m.slot_ids(),
            vec![
                "move",
                "marquee",
                "lasso",
                "wand",
                "crop",
                "eyedropper",
                "heal",
                "brush",
                "clone",
                "eraser",
                "gradient",
                "blur",
                "tone",
                "pen",
                "type",
                "path",
                "shape",
                "hand",
                "zoom",
            ]
        );
        let primaries: Vec<ToolId> = m.slots().iter().map(|s| s.primary()).collect();
        assert_eq!(
            primaries,
            vec![
                ToolId::Move,
                ToolId::RectMarquee,
                ToolId::Lasso,
                ToolId::MagicWand,
                ToolId::Crop,
                ToolId::Eyedropper,
                ToolId::SpotHealing,
                ToolId::Brush,
                ToolId::CloneStamp,
                ToolId::Eraser,
                ToolId::Gradient,
                ToolId::Blur,
                ToolId::Dodge,
                ToolId::Pen,
                ToolId::Type,
                ToolId::PathSelect,
                ToolId::Rectangle,
                ToolId::Hand,
                ToolId::Zoom,
            ]
        );
    }

    #[test]
    fn a_slot_may_hold_tools_with_different_keys_or_none() {
        let m = PaletteModel::build();
        // Blur, Sharpen, Smudge and Refine Boundary have no letter and used to
        // take four buttons; Photopea gives them one.
        let blur = m.slot_of(ToolId::Blur).expect("in the palette");
        assert_eq!(m.slot_of(ToolId::Sharpen), Some(blur));
        assert_eq!(m.slot_of(ToolId::Smudge), Some(blur));
        assert_eq!(m.slot_of(ToolId::RefineBoundary), Some(blur));
        assert_eq!(m.slots()[blur].shortcut, None);
        assert!(m.slots()[blur].has_variants());
        // Rotate View sits under Hand and keeps its own `R`.
        let hand = m.slot_of(ToolId::Hand).expect("in the palette");
        assert_eq!(m.slot_of(ToolId::RotateView), Some(hand));
        assert_eq!(m.slots()[hand].shortcut, Some('h'));
        assert_eq!(registry::cycle('r', None), Some(ToolId::RotateView));
    }

    #[test]
    fn the_screen_mode_cycles_standard_menu_full_and_back() {
        let mut mode = ScreenMode::default();
        assert_eq!(mode, ScreenMode::Standard);
        assert!(mode.panels_visible() && mode.menu_visible() && !mode.fullscreen());
        mode = mode.next();
        assert_eq!(mode, ScreenMode::FullScreenWithMenu);
        assert!(!mode.panels_visible() && mode.menu_visible() && mode.fullscreen());
        mode = mode.next();
        assert_eq!(mode, ScreenMode::FullScreen);
        assert!(!mode.panels_visible() && !mode.menu_visible() && mode.fullscreen());
        assert_eq!(mode.next(), ScreenMode::Standard);
        // `ALL` is the cycle, and every mode has a name.
        for pair in ScreenMode::ALL.windows(2) {
            assert_eq!(pair[0].next(), pair[1]);
        }
        for mode in ScreenMode::ALL {
            assert!(!mode.label().is_empty());
        }
    }

    #[test]
    fn the_footer_state_starts_in_standard_mode_with_quick_mask_off() {
        let state = PaletteState::new();
        assert!(!state.quick_mask);
        assert_eq!(state.screen_mode, ScreenMode::Standard);
        assert_ne!(quick_mask_control(), screen_mode_control());
    }

    /// W2-X: the footer's `F` asks; it never cycles. The request is an
    /// outbox the chrome drains once, and the mode stays the mirrored one.
    #[test]
    fn a_screen_mode_request_is_taken_once_and_leaves_the_mode_alone() {
        let mut state = PaletteState::new();
        assert!(!state.take_screen_mode_cycle());
        state.request_screen_mode_cycle();
        assert_eq!(state.screen_mode, ScreenMode::Standard);
        assert!(state.take_screen_mode_cycle());
        assert!(
            !state.take_screen_mode_cycle(),
            "the request was not cleared"
        );
        assert_eq!(state.screen_mode, ScreenMode::Standard);
    }

    #[test]
    fn no_slot_is_empty() {
        for slot in PaletteModel::build().slots() {
            assert!(!slot.tools.is_empty());
        }
    }

    #[test]
    fn tools_sharing_a_key_share_a_slot() {
        let m = PaletteModel::build();
        // The four marquees share `M`.
        let slot = m.slot_of(ToolId::RectMarquee).expect("in the palette");
        assert_eq!(m.slot_of(ToolId::EllipseMarquee), Some(slot));
        assert_eq!(m.slot_of(ToolId::SingleRowMarquee), Some(slot));
        assert!(m.slots()[slot].has_variants());
        assert_eq!(m.slots()[slot].shortcut, Some('m'));
        assert_eq!(m.slots()[slot].primary(), ToolId::RectMarquee);
    }

    #[test]
    fn slots_keep_the_registrys_order_within_a_slot() {
        let m = PaletteModel::build();
        for slot in m.slots() {
            let positions: Vec<usize> = slot
                .tools
                .iter()
                .map(|t| registry::all().iter().position(|i| i.id == *t).unwrap())
                .collect();
            assert!(
                positions.windows(2).all(|w| w[0] < w[1]),
                "{slot:?} reordered the registry"
            );
        }
    }

    #[test]
    fn the_group_runs_partition_the_slots_in_order() {
        // A run is a *divider position*, not a set: the registry lists Retouch
        // twice (the healing tools, then the tone tools further down), and the
        // palette must draw a divider at each boundary rather than collapsing
        // them. So the assertion is that the runs cover every slot exactly
        // once, in order, and that no two adjacent runs share a group.
        let m = PaletteModel::build();
        let groups = m.groups();
        let mut expected = 0usize;
        for (group, members) in &groups {
            for index in members {
                assert_eq!(*index, expected, "{group:?} broke the slot order");
                expected += 1;
            }
        }
        assert_eq!(expected, m.slots().len(), "a slot is in no run");
        for pair in groups.windows(2) {
            assert_ne!(
                pair[0].0, pair[1].0,
                "two adjacent runs share a group, so a divider would be drawn inside one"
            );
        }
    }

    #[test]
    fn a_slot_shows_the_variant_last_used_from_it() {
        let m = PaletteModel::build();
        let mut state = PaletteState::new();
        let marquee = m.slot_of(ToolId::RectMarquee).unwrap();
        assert_eq!(state.representative(&m, marquee), ToolId::RectMarquee);

        state.activate(&m, ToolId::EllipseMarquee);
        assert_eq!(state.representative(&m, marquee), ToolId::EllipseMarquee);
        assert!(state.slot_is_active(&m, marquee));

        // Move away: the slot keeps showing the ellipse, and stops being
        // selected.
        state.activate(&m, ToolId::Brush);
        assert_eq!(state.representative(&m, marquee), ToolId::EllipseMarquee);
        assert!(!state.slot_is_active(&m, marquee));
        assert!(state.slot_is_active(&m, m.slot_of(ToolId::Brush).unwrap()));
    }

    #[test]
    fn activating_reports_whether_anything_changed() {
        let m = PaletteModel::build();
        let mut state = PaletteState::new();
        assert!(state.activate(&m, ToolId::Eraser));
        assert!(!state.activate(&m, ToolId::Eraser));
        assert_eq!(state.active(), ToolId::Eraser);
    }

    #[test]
    fn pressing_the_key_walks_the_slot_exactly_as_the_registry_says() {
        let m = PaletteModel::build();
        let mut state = PaletteState::new();
        state.activate(&m, ToolId::Brush);
        let group = registry::by_shortcut('m');
        assert!(group.len() > 1);
        // From outside the group, the key lands on its first member.
        let first = state.tool_for_key('m').expect("m selects something");
        assert_eq!(first, group[0]);
        state.activate(&m, first);
        assert_eq!(state.tool_for_key('m'), Some(group[1]));
    }

    #[test]
    fn an_unbound_key_selects_nothing() {
        let state = PaletteState::new();
        assert_eq!(state.tool_for_key('§'), None);
    }

    #[test]
    fn opening_a_flyout_toggles() {
        let mut state = PaletteState::new();
        state.toggle_flyout(2);
        assert_eq!(state.open_flyout, Some(2));
        state.toggle_flyout(2);
        assert_eq!(state.open_flyout, None);
    }

    /// The bug this pins: `activate` used to clear `open_flyout` itself, so the
    /// caller's `else if has_variants { toggle_flyout(slot) }` re-opened the
    /// fly-out it had just closed and the button became a one-way door.
    #[test]
    fn activating_a_tool_leaves_the_flyout_flag_to_the_caller() {
        let m = PaletteModel::build();
        let mut state = PaletteState::new();
        state.toggle_flyout(3);
        state.activate(&m, ToolId::Brush);
        assert_eq!(state.open_flyout, Some(3));
        assert!(state.close_flyout());
        assert!(!state.close_flyout());
    }

    #[test]
    fn clicking_the_slot_of_the_active_tool_opens_then_shuts_its_flyout() {
        let m = PaletteModel::build();
        let mut state = PaletteState::new();
        let slot = m.slot_of(ToolId::RectMarquee).unwrap();
        assert!(m.slots()[slot].has_variants());

        // First click selects the tool; the fly-out stays shut.
        assert_eq!(
            state.click_slot(&m, slot),
            SlotClick::Selected(ToolId::RectMarquee)
        );
        assert_eq!(state.open_flyout, None);
        // Second click reveals the variants...
        assert_eq!(state.click_slot(&m, slot), SlotClick::OpenedFlyout(slot));
        assert_eq!(state.open_flyout, Some(slot));
        // ...and a third puts them away again. This is the one that regressed.
        assert_eq!(state.click_slot(&m, slot), SlotClick::ClosedFlyout);
        assert_eq!(state.open_flyout, None);
    }

    #[test]
    fn clicking_a_different_slot_shuts_the_flyout_that_was_open() {
        let m = PaletteModel::build();
        let mut state = PaletteState::new();
        let marquee = m.slot_of(ToolId::RectMarquee).unwrap();
        let eraser = m.slot_of(ToolId::Eraser).unwrap();
        state.click_slot(&m, marquee);
        state.click_slot(&m, marquee);
        assert_eq!(state.open_flyout, Some(marquee));

        assert_eq!(
            state.click_slot(&m, eraser),
            SlotClick::Selected(ToolId::Eraser)
        );
        assert_eq!(state.open_flyout, None);
    }

    #[test]
    fn clicking_the_slot_of_a_tool_with_no_variants_never_opens_anything() {
        let m = PaletteModel::build();
        let mut state = PaletteState::new();
        let solo = m
            .slots()
            .iter()
            .position(|s| !s.has_variants())
            .expect("some tool stands alone");
        state.click_slot(&m, solo);
        assert_eq!(state.click_slot(&m, solo), SlotClick::Nothing);
        assert_eq!(state.open_flyout, None);
    }

    #[test]
    fn clicking_a_slot_that_is_not_there_does_nothing() {
        let m = PaletteModel::build();
        let mut state = PaletteState::new();
        assert_eq!(state.click_slot(&m, 9_999), SlotClick::Nothing);
        assert_eq!(state.open_flyout, None);
    }

    #[test]
    fn a_slot_index_past_the_end_does_not_panic() {
        let m = PaletteModel::build();
        let state = PaletteState::new();
        assert_eq!(state.representative(&m, 9_999), state.active());
        assert!(!state.slot_is_active(&m, 9_999));
    }

    #[test]
    fn every_tool_has_a_tooltip_naming_it_and_its_key() {
        for tool in ToolId::ALL {
            let i = info(*tool).unwrap_or_else(|| panic!("{tool:?} is not in the registry"));
            let text = tooltip(i);
            assert!(text.contains(i.name), "{tool:?}: {text}");
            if let Some(key) = i.shortcut {
                assert!(
                    text.contains(key.to_ascii_uppercase()),
                    "{tool:?} does not show its key: {text}"
                );
            }
        }
    }

    #[test]
    fn every_palette_group_has_a_heading() {
        for group in [
            ToolGroup::Select,
            ToolGroup::Crop,
            ToolGroup::Retouch,
            ToolGroup::Paint,
            ToolGroup::Draw,
            ToolGroup::Navigate,
            ToolGroup::Transform,
        ] {
            assert!(!group_label(group).is_empty(), "{group:?}");
        }
    }
}
