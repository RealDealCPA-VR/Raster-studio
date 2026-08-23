//! The tool palette: one column of slots, driven entirely by the registry.
//!
//! # Slots, not tools
//!
//! The palette does not show forty-four buttons. It shows one button per
//! *cycle group* — the tools that share a shortcut letter, which is exactly the
//! set `tools::registry` already calls a group — with the rest reachable from a
//! fly-out. [`PaletteModel::build`] derives the whole thing from
//! `tools::registry::all()`, so a new tool joins the palette by existing, and
//! `every_tool_is_reachable_from_exactly_one_slot` fails if one ever is not.
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
    pub group: ToolGroup,
    /// The key that cycles this slot, when its tools declare one.
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
    /// Tools sharing a shortcut land in one slot; a tool with no shortcut gets
    /// a slot of its own, because there is no key to cycle it with and hiding
    /// it behind another tool would make it unreachable.
    pub fn build() -> Self {
        let mut slots: Vec<PaletteSlot> = Vec::new();
        for info in registry::all() {
            let existing = info.shortcut.and_then(|key| {
                slots
                    .iter_mut()
                    .find(|s| s.shortcut == Some(key) && s.group == info.group)
            });
            match existing {
                Some(slot) => slot.tools.push(info.id),
                None => slots.push(PaletteSlot {
                    group: info.group,
                    shortcut: info.shortcut,
                    tools: vec![info.id],
                }),
            }
        }
        Self { slots }
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

/// What the palette remembers between frames.
#[derive(Clone, PartialEq, Debug)]
pub struct PaletteState {
    active: ToolId,
    /// The variant last chosen from each slot, by slot index.
    last_used: HashMap<usize, ToolId>,
    /// The slot whose fly-out is open, if any.
    pub open_flyout: Option<usize>,
}

impl Default for PaletteState {
    fn default() -> Self {
        Self {
            active: ToolId::Brush,
            last_used: HashMap::new(),
            open_flyout: None,
        }
    }
}

impl PaletteState {
    pub fn new() -> Self {
        Self::default()
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
        Some(key) => format!("{}  ({})", info.name, key.to_ascii_uppercase()),
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
        let mut seen: Vec<ToolId> = m.slots().iter().flat_map(|s| s.tools.clone()).collect();
        let unique: HashSet<ToolId> = seen.iter().copied().collect();
        assert_eq!(unique.len(), seen.len(), "a tool is in two slots");
        seen.sort_by_key(|t| ToolId::ALL.iter().position(|x| x == t));
        assert_eq!(
            seen,
            ToolId::ALL.to_vec(),
            "a tool is missing from the palette"
        );
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
    fn a_tool_with_no_shortcut_gets_a_slot_to_itself() {
        let m = PaletteModel::build();
        for info in registry::all().iter().filter(|i| i.shortcut.is_none()) {
            let slot = m.slot_of(info.id).expect("in the palette");
            assert_eq!(
                m.slots()[slot].tools,
                vec![info.id],
                "{:?} was hidden behind another tool with no key to reach it",
                info.id
            );
        }
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
