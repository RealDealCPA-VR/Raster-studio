//! The dock: which panels exist, where they sit, and the saved layouts that
//! move them all at once.
//!
//! Docking is state, not drawing. [`DockState`] is a plain, serializable value
//! with no egui in it, so "switching to the Painting workspace opens Brushes
//! and closes History" is a unit test rather than something you check by
//! looking. The drawing side reads this state and nothing else decides where a
//! panel goes.
//!
//! Panels really are movable: every panel header carries an overflow disclosure
//! that opens Move to Left / Right / Bottom and a pair of reorder chevrons, which
//! post [`crate::Intent::DockPanel`] and [`crate::Intent::ReorderPanel`] and
//! come back here through [`crate::Workspace::absorb`]. `moving_a_panel_across_
//! sides_through_the_header` in `tests/clicking_the_real_thing.rs` drives that
//! path with real clicks, so [`DockState::dock`] cannot quietly lose its only
//! caller again.
//!
//! Sizes are clamped on the way in rather than on the way out. A width that
//! arrives out of range — from a drag that overshot, or from a settings file
//! written by a build with different minimums — is corrected once, at the
//! setter, so every later reader can trust it.

use serde::{Deserialize, Serialize};

use design::Space;

/// Every dockable panel.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PanelId {
    Layers,
    History,
    Adjustments,
    Properties,
    Color,
    Swatches,
    Brushes,
    Character,
    Paragraph,
    Navigator,
    Info,
    Channels,
    Paths,
    /// The Actions recorder: record, stop and replay command sequences.
    Actions,
}

impl PanelId {
    /// Every panel, in Window-menu order.
    pub const ALL: &'static [PanelId] = &[
        PanelId::Layers,
        PanelId::History,
        PanelId::Adjustments,
        PanelId::Properties,
        PanelId::Color,
        PanelId::Swatches,
        PanelId::Brushes,
        PanelId::Character,
        PanelId::Paragraph,
        PanelId::Navigator,
        PanelId::Info,
        PanelId::Channels,
        PanelId::Paths,
        PanelId::Actions,
    ];

    /// Panel title, as shown on its header and in the Window menu.
    pub const fn title(self) -> &'static str {
        match self {
            PanelId::Layers => "Layers",
            PanelId::History => "History",
            PanelId::Adjustments => "Adjustments",
            PanelId::Properties => "Properties",
            PanelId::Color => "Color",
            PanelId::Swatches => "Swatches",
            PanelId::Brushes => "Brushes",
            PanelId::Character => "Character",
            PanelId::Paragraph => "Paragraph",
            PanelId::Navigator => "Navigator",
            PanelId::Info => "Info",
            PanelId::Channels => "Channels",
            PanelId::Paths => "Paths",
            PanelId::Actions => "Actions",
        }
    }

    /// Stable id string, used for the egui widget id and for settings keys.
    pub const fn key(self) -> &'static str {
        match self {
            PanelId::Layers => "layers",
            PanelId::History => "history",
            PanelId::Adjustments => "adjustments",
            PanelId::Properties => "properties",
            PanelId::Color => "color",
            PanelId::Swatches => "swatches",
            PanelId::Brushes => "brushes",
            PanelId::Character => "character",
            PanelId::Paragraph => "paragraph",
            PanelId::Navigator => "navigator",
            PanelId::Info => "info",
            PanelId::Channels => "channels",
            PanelId::Paths => "paths",
            PanelId::Actions => "actions",
        }
    }
}

/// Which edge of the window a panel is docked to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DockSide {
    Left,
    Right,
    /// Below the canvas, above the status bar.
    Bottom,
}

impl DockSide {
    pub const ALL: &'static [DockSide] = &[DockSide::Left, DockSide::Right, DockSide::Bottom];
}

/// Where one panel sits and how it is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PanelPlacement {
    pub side: DockSide,
    /// Position within its side, smaller first. Ties break on [`PanelId::ALL`]
    /// order, so the arrangement is total even after a sloppy drag.
    pub order: u8,
    /// Whether the panel is in the dock at all.
    pub open: bool,
    /// Whether its body is folded away, leaving only the header. Tabbed
    /// groups no longer fold; the field survives for session compatibility.
    pub collapsed: bool,
    /// Which tabbed group of its side the panel belongs to. Panels sharing
    /// `(side, group)` stack as tabs; only the group's active member draws.
    #[serde(default)]
    pub group: u8,
    /// Whether this panel is the tab its group is showing. Exactly one open
    /// member of a group is active; [`DockState::normalize`] repairs older
    /// sessions where no member is.
    #[serde(default)]
    pub active: bool,
}

/// A named arrangement the user can switch to in one click.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LayoutId {
    /// The default: layers, history, properties, colour.
    Essentials,
    /// Brushes and colour forward, history out of the way.
    Painting,
    /// Adjustments, histogram-adjacent panels, navigator.
    Photography,
    /// Everything closed but the canvas.
    Minimal,
}

impl LayoutId {
    pub const ALL: &'static [LayoutId] = &[
        LayoutId::Essentials,
        LayoutId::Painting,
        LayoutId::Photography,
        LayoutId::Minimal,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            LayoutId::Essentials => "Essentials",
            LayoutId::Painting => "Painting",
            LayoutId::Photography => "Photography",
            LayoutId::Minimal => "Minimal",
        }
    }

    /// The panels this layout opens, per side, with their tabbed group.
    /// Groups follow Photopea's pairings where the panels exist: channels with
    /// layers, adjustments with properties, info with navigator.
    fn panels(self) -> &'static [(PanelId, DockSide, u8)] {
        match self {
            // Photopea has no left dock: every panel lives on the right of
            // the canvas, tabbed in Photopea's pairings.
            LayoutId::Essentials => &[
                (PanelId::Properties, DockSide::Right, 0),
                (PanelId::Adjustments, DockSide::Right, 0),
                (PanelId::Layers, DockSide::Right, 1),
                (PanelId::History, DockSide::Right, 2),
                (PanelId::Color, DockSide::Right, 2),
            ],
            LayoutId::Painting => &[
                (PanelId::Color, DockSide::Left, 0),
                (PanelId::Swatches, DockSide::Left, 0),
                (PanelId::Brushes, DockSide::Left, 1),
                (PanelId::Layers, DockSide::Right, 0),
                (PanelId::Properties, DockSide::Right, 1),
            ],
            LayoutId::Photography => &[
                (PanelId::Navigator, DockSide::Left, 0),
                (PanelId::Info, DockSide::Left, 0),
                (PanelId::History, DockSide::Left, 1),
                (PanelId::Properties, DockSide::Right, 0),
                (PanelId::Adjustments, DockSide::Right, 0),
                (PanelId::Layers, DockSide::Right, 1),
                (PanelId::Channels, DockSide::Right, 1),
            ],
            LayoutId::Minimal => &[],
        }
    }
}

/// Narrowest a side dock may be dragged. Below this an inspector row's label
/// column no longer fits beside its field, so the panel stops being usable
/// rather than merely being small.
pub const MIN_DOCK_WIDTH: f32 = 220.0;

/// The width of a collapsed side dock: one column of panel icons. Photopea's
/// icon rail is a single hit target wide, and nothing wider would be a rail.
pub const RAIL_WIDTH_PT: f32 = 40.0;
/// Widest a side dock may be dragged, so the canvas cannot be squeezed away.
pub const MAX_DOCK_WIDTH: f32 = 520.0;
/// Shortest the bottom dock may be.
pub const MIN_DOCK_HEIGHT: f32 = 96.0;
/// Tallest the bottom dock may be.
pub const MAX_DOCK_HEIGHT: f32 = 400.0;

/// The full arrangement of the dock.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct DockState {
    /// One entry per [`PanelId::ALL`] member, in that order.
    placements: Vec<PanelPlacement>,
    left_width: f32,
    right_width: f32,
    bottom_height: f32,
    /// Photopea's collapse-to-icons: a whole side dock folded to a rail of
    /// panel icons at [`DockState::RAIL_WIDTH_PT`]. Indexed by [`side_slot`].
    #[serde(default)]
    collapsed: [bool; 2],
    /// The layout this state was last set from, or `None` once the user has
    /// moved something and it no longer matches any preset.
    layout: Option<LayoutId>,
}

impl Default for DockState {
    fn default() -> Self {
        Self::from_layout(LayoutId::Essentials)
    }
}

impl DockState {
    /// The arrangement a named layout describes.
    pub fn from_layout(layout: LayoutId) -> Self {
        let mut state = Self {
            placements: PanelId::ALL
                .iter()
                .map(|_| PanelPlacement {
                    side: DockSide::Right,
                    order: u8::MAX,
                    open: false,
                    collapsed: false,
                    group: 0,
                    active: false,
                })
                .collect(),
            left_width: 260.0,
            right_width: 300.0,
            bottom_height: 160.0,
            collapsed: [false; 2],
            layout: Some(layout),
        };
        // `order` is per (side, group): tabs within a group count separately
        // from the groups stacked above and below.
        let mut next = [[0u8; 4]; 3];
        for (panel, side, group) in layout.panels() {
            let slot = DockSide::ALL.iter().position(|s| s == side).unwrap_or(0);
            let index = Self::index_of(*panel);
            let first_in_group = next[slot][*group as usize] == 0;
            state.placements[index] = PanelPlacement {
                side: *side,
                order: next[slot][*group as usize],
                open: true,
                collapsed: false,
                group: *group,
                active: first_in_group,
            };
            next[slot][*group as usize] = next[slot][*group as usize].saturating_add(1);
        }
        state
    }

    fn index_of(panel: PanelId) -> usize {
        PanelId::ALL
            .iter()
            .position(|p| *p == panel)
            .expect("PanelId::ALL is exhaustive")
    }

    /// The preset this arrangement came from, if the *user* has not changed it
    /// since.
    ///
    /// "Changed" means a decision: a panel opened, closed, collapsed, moved,
    /// reordered, or a splitter dragged. It deliberately does not mean a rail
    /// being measured at a different number of points, which happens on the
    /// first drawn frame and whenever the window is resized — see
    /// [`DockState::sync_side_width`].
    pub fn layout(&self) -> Option<LayoutId> {
        self.layout
    }

    /// Replace the whole arrangement with a preset.
    pub fn apply_layout(&mut self, layout: LayoutId) {
        let widths = (self.left_width, self.right_width, self.bottom_height);
        *self = Self::from_layout(layout);
        // Sizes are the user's, not the layout's: switching workspaces should
        // not undo a dock the user has widened.
        self.left_width = widths.0;
        self.right_width = widths.1;
        self.bottom_height = widths.2;
    }

    pub fn placement(&self, panel: PanelId) -> PanelPlacement {
        self.placements[Self::index_of(panel)]
    }

    pub fn is_open(&self, panel: PanelId) -> bool {
        self.placement(panel).open
    }

    /// Whether a whole side dock is folded to its icon rail. Only the two
    /// vertical sides collapse; the bottom dock has no rail to become.
    pub fn side_is_collapsed(&self, side: DockSide) -> bool {
        match side {
            DockSide::Left => self.collapsed[0],
            DockSide::Right => self.collapsed[1],
            DockSide::Bottom => false,
        }
    }

    /// Fold a side to its icon rail, or unfold it back to the panels. The
    /// panels' own placements are untouched — unfolding restores exactly what
    /// was folded, which is the round trip.
    pub fn set_side_collapsed(&mut self, side: DockSide, collapsed: bool) {
        match side {
            DockSide::Left => self.collapsed[0] = collapsed,
            DockSide::Right => self.collapsed[1] = collapsed,
            DockSide::Bottom => {}
        }
    }

    pub fn toggle_side_collapsed(&mut self, side: DockSide) {
        let collapsed = self.side_is_collapsed(side);
        self.set_side_collapsed(side, !collapsed);
    }

    pub fn is_collapsed(&self, panel: PanelId) -> bool {
        self.placement(panel).collapsed
    }

    /// Open or close a panel. Opening one that has never been placed drops it
    /// at the end of the right-hand dock rather than nowhere.
    pub fn set_open(&mut self, panel: PanelId, open: bool) {
        let index = Self::index_of(panel);
        if self.placements[index].open == open {
            return;
        }
        self.placements[index].open = open;
        if open && self.placements[index].order == u8::MAX {
            let side = self.placements[index].side;
            self.placements[index].order = self.next_order(side);
        }
        self.layout = None;
    }

    pub fn toggle_open(&mut self, panel: PanelId) {
        self.set_open(panel, !self.is_open(panel));
    }

    pub fn set_collapsed(&mut self, panel: PanelId, collapsed: bool) {
        let index = Self::index_of(panel);
        if self.placements[index].collapsed != collapsed {
            self.placements[index].collapsed = collapsed;
            self.layout = None;
        }
    }

    pub fn toggle_collapsed(&mut self, panel: PanelId) {
        self.set_collapsed(panel, !self.is_collapsed(panel));
    }

    /// Move a panel to a side, at the end of that side's stack.
    ///
    /// Returns `true` when something moved. Docking a panel to the side it is
    /// already open on is a no-op and answers `false`, so the header control
    /// that drives this does not emit an intent per click on the side the
    /// panel is already on.
    pub fn dock(&mut self, panel: PanelId, side: DockSide) -> bool {
        let index = Self::index_of(panel);
        if self.placements[index].side == side && self.placements[index].open {
            return false;
        }
        let order = self.next_order(side);
        self.placements[index].side = side;
        self.placements[index].order = order;
        self.placements[index].open = true;
        self.layout = None;
        true
    }

    fn next_order(&self, side: DockSide) -> u8 {
        self.placements
            .iter()
            .filter(|p| p.side == side && p.open && p.order != u8::MAX)
            .map(|p| p.order.saturating_add(1))
            .max()
            .unwrap_or(0)
    }

    /// The open panels on one side, in the order they should be drawn:
    /// group-major (groups stacked by number), tab order within a group.
    pub fn panels_on(&self, side: DockSide) -> Vec<PanelId> {
        let mut open: Vec<(u8, u8, usize, PanelId)> = PanelId::ALL
            .iter()
            .enumerate()
            .filter_map(|(i, panel)| {
                let p = self.placements[i];
                (p.open && p.side == side).then_some((p.group, p.order, i, *panel))
            })
            .collect();
        // Groups stack by number; tabs within a group by explicit order, then
        // declaration order so ties are total.
        open.sort_by_key(|(group, order, i, _)| (*group, *order, *i));
        open.into_iter().map(|(_, _, _, panel)| panel).collect()
    }

    /// The tabbed groups of one side, stacked in group-number order, each
    /// holding its open members in tab order. Groups are *derived* from the
    /// placements: when the last member of a group closes, the group is gone.
    pub fn groups_on(&self, side: DockSide) -> Vec<(u8, Vec<PanelId>)> {
        let mut groups: Vec<(u8, Vec<PanelId>)> = Vec::new();
        for panel in self.panels_on(side) {
            let group = self.placement(panel).group;
            match groups.last_mut() {
                Some((number, members)) if *number == group => members.push(panel),
                _ => groups.push((group, vec![panel])),
            }
        }
        groups
    }

    /// The panel a group is showing, if any of its members is open.
    pub fn active_panel(&self, side: DockSide, group: u8) -> Option<PanelId> {
        self.groups_on(side)
            .into_iter()
            .find(|(number, _)| *number == group)
            .and_then(|(_, members)| {
                members
                    .iter()
                    .copied()
                    .find(|p| self.placement(*p).active)
                    .or(members.first().copied())
            })
    }

    /// Whether this panel is the tab its group is showing.
    pub fn is_active(&self, panel: PanelId) -> bool {
        let p = self.placement(panel);
        p.open && p.active
    }

    /// Raise one panel as its group's active tab. Clicking a tab.
    pub fn raise(&mut self, panel: PanelId) {
        let index = Self::index_of(panel);
        if !self.placements[index].open {
            return;
        }
        let side = self.placements[index].side;
        let group = self.placements[index].group;
        for (i, p) in self.placements.iter_mut().enumerate() {
            if p.side == side && p.group == group {
                p.active = i == index;
            }
        }
        self.layout = None;
    }

    /// Move a panel into a group of `side`, as its last tab. Joining the
    /// group it already lives in keeps its tab order and answers `false`.
    pub fn move_panel(&mut self, panel: PanelId, side: DockSide, group: u8) -> bool {
        let index = Self::index_of(panel);
        if self.placements[index].open
            && self.placements[index].side == side
            && self.placements[index].group == group
        {
            return false;
        }
        let was_open = self.placements[index].open;
        let old_side = self.placements[index].side;
        let old_group = self.placements[index].group;
        let order = self
            .groups_on(side)
            .iter()
            .find(|(number, _)| *number == group)
            .map(|(_, members)| members.len())
            .unwrap_or(0) as u8;
        self.placements[index].side = side;
        self.placements[index].group = group;
        self.placements[index].order = order;
        self.placements[index].open = true;
        // Take the tab: the group it joined shows what the drag asked for.
        for (i, p) in self.placements.iter_mut().enumerate() {
            let old_group_member = was_open && p.side == old_side && p.group == old_group;
            let joined_group = p.side == side && p.group == group;
            if i == index {
                p.active = true;
            } else if old_group_member || joined_group {
                p.active = false;
            }
        }
        self.normalize();
        self.layout = None;
        true
    }

    /// Repair placements after a load: every (side, group) with open panels
    /// has exactly one active member (first open member in tab order wins);
    /// closed or absent panels are never active.
    pub fn normalize(&mut self) {
        for side in DockSide::ALL {
            let groups: Vec<u8> = {
                let mut seen: Vec<u8> = Vec::new();
                for (i, p) in self.placements.iter().enumerate() {
                    if p.side == *side && p.open && !seen.contains(&p.group) {
                        seen.push(p.group);
                    }
                    let _ = i;
                }
                seen
            };
            for group in groups {
                let members: Vec<usize> = self
                    .panels_on(*side)
                    .into_iter()
                    .filter(|p| {
                        let p = self.placement(*p);
                        p.side == *side && p.group == group
                    })
                    .map(Self::index_of)
                    .collect();
                let any_active = members.iter().any(|i| self.placements[*i].active);
                for (position, i) in members.iter().enumerate() {
                    self.placements[*i].active = if any_active {
                        self.placements[*i].active
                    } else {
                        position == 0
                    };
                }
            }
        }
    }

    pub fn left_width(&self) -> f32 {
        self.left_width
    }

    pub fn right_width(&self) -> f32 {
        self.right_width
    }

    pub fn bottom_height(&self) -> f32 {
        self.bottom_height
    }

    /// Set a side dock's width, clamped into the usable range. A non-finite
    /// value — which a drag against a collapsed window can produce — leaves the
    /// width alone rather than poisoning it.
    ///
    /// # Why the equality check is load-bearing
    ///
    /// The drawing side hands the rail's measured width back here *every
    /// frame*, whether or not the user dragged the splitter. Without the
    /// early return, merely drawing a frame would clear [`DockState::layout`]
    /// — so Window ▸ Workspace would never show a checkmark against the
    /// arrangement actually in use, and re-applying the current layout would
    /// report "changed". A write that changes nothing must therefore change
    /// nothing, including the layout identity.
    pub fn set_side_width(&mut self, side: DockSide, width: f32) {
        if self.sync_side_width(side, width) {
            self.layout = None;
        }
    }

    /// Record a side's extent **without** claiming the user rearranged
    /// anything.
    ///
    /// The drawing layer measures each rail every frame, and the layout engine
    /// owns the number: it can disagree with the width a saved layout asked
    /// for, and it changes when the window is resized. Both are facts about
    /// pixels, not decisions by the user, so recording one must not cost the
    /// dock its saved-layout identity — see [`is_resize`], which is how the
    /// drawing layer tells a splitter drag from a measurement.
    ///
    /// Returns `true` when the stored extent actually moved.
    pub fn sync_side_width(&mut self, side: DockSide, width: f32) -> bool {
        if !width.is_finite() {
            return false;
        }
        let clamped = match side {
            DockSide::Bottom => width.clamp(MIN_DOCK_HEIGHT, MAX_DOCK_HEIGHT),
            _ => width.clamp(MIN_DOCK_WIDTH, MAX_DOCK_WIDTH),
        };
        if self.side_extent(side) == clamped {
            return false;
        }
        match side {
            DockSide::Left => self.left_width = clamped,
            DockSide::Right => self.right_width = clamped,
            DockSide::Bottom => self.bottom_height = clamped,
        }
        true
    }

    /// Move a panel's tab group one slot up or down the side's stack.
    ///
    /// Groups travel whole: a reorder can never split a group's tabs. Returns
    /// the group's new stack index.
    pub fn reorder(&mut self, panel: PanelId, up: bool) -> Option<u8> {
        let placement = self.placement(panel);
        if !placement.open {
            return None;
        }
        let groups = self.groups_on(placement.side);
        let at = groups.iter().position(|(g, _)| *g == placement.group)?;
        let to = if up {
            at.checked_sub(1)?
        } else {
            let next = at + 1;
            if next >= groups.len() {
                return None;
            }
            next
        };
        let to = u8::try_from(to).ok()?;
        self.reorder_group(placement.side, placement.group, to)
            .then_some(to)
    }

    /// Move a panel's tab group to stack slot `to` of its side.
    ///
    /// # Why this is the one the intent carries
    ///
    /// Absolute, and therefore idempotent: applying it twice leaves the panel
    /// where the first application put it. `crate::Intent::ReorderPanel` used
    /// to carry a *direction*, and the drawing side applies an intent before it
    /// emits it, so an application that also absorbed what it drained moved the
    /// panel two places for one click. Every workspace intent must survive
    /// being absorbed twice — see the invariant on [`crate::Intent`]. The
    /// destination is the group's stack slot, so "already there" is a no-op by
    /// construction.
    ///
    /// Returns `true` when the arrangement actually changed.
    pub fn reorder_to(&mut self, panel: PanelId, to: u8) -> bool {
        let placement = self.placement(panel);
        if !placement.open {
            return false;
        }
        self.reorder_group(placement.side, placement.group, to)
    }

    /// Place one group at stack index `to`, shifting the others, and renumber
    /// the stack densely. Returns `false` for a no-op or an out-of-range slot.
    fn reorder_group(&mut self, side: DockSide, group: u8, to: u8) -> bool {
        let groups = self.groups_on(side);
        let from = match groups.iter().position(|(g, _)| *g == group) {
            Some(at) => at,
            None => return false,
        };
        let to = usize::from(to);
        if to >= groups.len() || to == from {
            return false;
        }
        let member_ids: Vec<(u8, Vec<PanelId>)> = groups
            .into_iter()
            .enumerate()
            .map(|(i, (number, members))| {
                let _ = number;
                (u8::try_from(i).unwrap_or(0), members)
            })
            .collect();
        let mut stack: Vec<(u8, Vec<PanelId>)> = member_ids;
        let moved = stack.remove(from);
        stack.insert(to.min(stack.len()), moved);
        // Renumber the stack densely and write the group + tab order back.
        for (stack_index, (_, members)) in stack.iter().enumerate() {
            let number = u8::try_from(stack_index).unwrap_or(u8::MAX);
            for (tab, id) in members.iter().enumerate() {
                let index = Self::index_of(*id);
                self.placements[index].group = number;
                self.placements[index].order = u8::try_from(tab).unwrap_or(u8::MAX - 1);
            }
        }
        self.normalize();
        self.layout = None;
        true
    }

    /// Width (or, for the bottom, height) of one side.
    pub fn side_extent(&self, side: DockSide) -> f32 {
        match side {
            DockSide::Left => self.left_width,
            DockSide::Right => self.right_width,
            DockSide::Bottom => self.bottom_height,
        }
    }

    /// `true` when a side has nothing open on it, so the dock should not be
    /// drawn at all — an empty rail of chrome is worse than no rail.
    pub fn side_is_empty(&self, side: DockSide) -> bool {
        self.panels_on(side).is_empty()
    }

    /// Height of a collapsed panel: its header alone.
    pub fn header_height() -> f32 {
        Space::XXLarge.pt()
    }
}

/// Whether a rail's freshly measured extent is a *user resize*.
///
/// # Why this is not just "the number changed"
///
/// The drawing layer measures each rail every frame and there is no
/// "the splitter was dragged" signal to read. Two things follow:
///
/// * The very first measurement is not a resize. The layout engine has its own
///   idea of a panel's starting width, and it need not equal the one the saved
///   layout asked for; treating that disagreement as a drag clears the dock's
///   saved-layout identity on the first drawn frame — which is exactly the bug
///   `drawing_a_frame_does_not_wipe_the_docks_saved_layout` exists to catch.
/// * A width that changes with no pointer down is the *window* being resized,
///   not the dock. The user did not rearrange anything, so the arrangement is
///   still the preset it was.
///
/// So a resize is: a change from a previous measurement, while the pointer is
/// down.
pub fn is_resize(previous: Option<f32>, measured: f32, pointer_down: bool) -> bool {
    pointer_down && measured.is_finite() && previous.is_some_and(|p| p != measured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_layout_is_essentials_and_opens_the_layers_panel() {
        let d = DockState::default();
        assert_eq!(d.layout(), Some(LayoutId::Essentials));
        assert!(d.is_open(PanelId::Layers));
        assert_eq!(d.placement(PanelId::Layers).side, DockSide::Right);
        assert!(!d.is_open(PanelId::Paths));
    }

    #[test]
    fn switching_to_painting_opens_brushes_and_closes_history() {
        let mut d = DockState::default();
        assert!(d.is_open(PanelId::History));
        assert!(!d.is_open(PanelId::Brushes));
        d.apply_layout(LayoutId::Painting);
        assert!(d.is_open(PanelId::Brushes));
        assert!(!d.is_open(PanelId::History));
        assert_eq!(d.layout(), Some(LayoutId::Painting));
    }

    #[test]
    fn the_minimal_layout_leaves_every_side_empty() {
        let d = DockState::from_layout(LayoutId::Minimal);
        for side in DockSide::ALL {
            assert!(d.side_is_empty(*side), "{side:?} is not empty");
        }
        for panel in PanelId::ALL {
            assert!(!d.is_open(*panel), "{panel:?} is still open");
        }
    }

    #[test]
    fn collapsing_a_side_round_trips_through_the_icon_rail() {
        // The Validate for P1.18: collapse folds the side, expand restores
        // exactly the arrangement that was folded, and the rail is the icon
        // width — one hit target wide, not a skinny panel.
        let mut d = DockState::from_layout(LayoutId::Essentials);
        assert!(!d.side_is_collapsed(DockSide::Right));
        let placements = d.placements.clone();
        let width = d.right_width();

        d.set_side_collapsed(DockSide::Right, true);
        assert!(d.side_is_collapsed(DockSide::Right));
        // The placements survive the fold untouched.
        assert_eq!(d.placements, placements);

        d.set_side_collapsed(DockSide::Right, false);
        assert!(!d.side_is_collapsed(DockSide::Right));
        assert_eq!(d.placements, placements, "unfolding restores the fold");
        assert_eq!(d.right_width(), width);

        // The bottom dock has no rail to become.
        d.set_side_collapsed(DockSide::Bottom, true);
        assert!(!d.side_is_collapsed(DockSide::Bottom));

        assert_eq!(
            RAIL_WIDTH_PT, 40.0,
            "the rail is the icon width: one hit-target column with padding"
        );
    }

    #[test]
    fn switching_layouts_keeps_the_users_dock_widths() {
        let mut d = DockState::default();
        d.set_side_width(DockSide::Right, 480.0);
        d.apply_layout(LayoutId::Photography);
        assert_eq!(d.right_width(), 480.0);
    }

    #[test]
    fn dock_widths_are_clamped_and_a_nan_is_ignored() {
        let mut d = DockState::default();
        d.set_side_width(DockSide::Right, 10_000.0);
        assert_eq!(d.right_width(), MAX_DOCK_WIDTH);
        d.set_side_width(DockSide::Right, 0.0);
        assert_eq!(d.right_width(), MIN_DOCK_WIDTH);
        d.set_side_width(DockSide::Right, f32::NAN);
        assert_eq!(d.right_width(), MIN_DOCK_WIDTH);
        d.set_side_width(DockSide::Bottom, 10_000.0);
        assert_eq!(d.bottom_height(), MAX_DOCK_HEIGHT);
    }

    #[test]
    fn opening_a_never_placed_panel_gives_it_a_real_slot() {
        let mut d = DockState::from_layout(LayoutId::Minimal);
        d.set_open(PanelId::Paths, true);
        assert!(d.is_open(PanelId::Paths));
        assert_ne!(d.placement(PanelId::Paths).order, u8::MAX);
        assert_eq!(d.panels_on(DockSide::Right), vec![PanelId::Paths]);
    }

    #[test]
    fn panels_on_a_side_come_back_in_dock_order() {
        let d = DockState::from_layout(LayoutId::Essentials);
        assert_eq!(
            d.panels_on(DockSide::Right),
            vec![
                PanelId::Properties,
                PanelId::Adjustments,
                PanelId::Layers,
                PanelId::History,
                PanelId::Color
            ]
        );
        assert!(
            d.side_is_empty(DockSide::Left),
            "Photopea has no left dock: Essentials opens nothing on the left"
        );
    }

    #[test]
    fn re_setting_a_dock_width_to_the_value_it_already_holds_changes_nothing() {
        // The rail hands its measured width back every frame. If that cleared
        // the layout, one drawn frame would make Window ▸ Workspace stop
        // showing which arrangement is in use.
        let mut d = DockState::default();
        let before = d.clone();
        d.set_side_width(DockSide::Right, d.right_width());
        assert_eq!(d, before);
        assert_eq!(d.layout(), Some(LayoutId::Essentials));

        // A width outside the range clamps to a value already held, so that
        // must not dirty the layout either.
        d.set_side_width(DockSide::Right, MAX_DOCK_WIDTH);
        assert_eq!(d.layout(), None, "a real change still clears the layout");
        let mut d = DockState::default();
        d.set_side_width(DockSide::Left, 0.0);
        assert_eq!(d.left_width(), MIN_DOCK_WIDTH);
        assert_eq!(d.layout(), None);
        let dirtied = d.clone();
        d.set_side_width(DockSide::Left, -1000.0);
        assert_eq!(d, dirtied, "a second clamp to the same value is a no-op");
    }

    #[test]
    fn a_measurement_is_a_resize_only_when_it_changes_under_the_pointer() {
        // The first measurement of a rail is never a resize: the layout engine
        // has its own idea of a starting width, and it need not match ours.
        assert!(!is_resize(None, 308.0, true));
        assert!(!is_resize(None, 308.0, false));
        // Unchanged is not a resize however hard the pointer is pressed.
        assert!(!is_resize(Some(308.0), 308.0, true));
        // A change with no pointer down is the window resizing, not the dock.
        assert!(!is_resize(Some(308.0), 260.0, false));
        // A change under the pointer is the splitter being dragged.
        assert!(is_resize(Some(308.0), 260.0, true));
        // A measurement that is not a number is never a resize.
        assert!(!is_resize(Some(308.0), f32::NAN, true));
    }

    #[test]
    fn a_real_width_change_still_clears_the_layout() {
        let mut d = DockState::default();
        assert_eq!(d.layout(), Some(LayoutId::Essentials));
        d.set_side_width(DockSide::Right, d.right_width() + 40.0);
        assert_eq!(d.layout(), None);
    }

    #[test]
    fn syncing_a_measured_width_moves_the_extent_but_keeps_the_layout() {
        // The canvas camera is placed from the stored extent, so a measurement
        // has to land; the *identity* of the arrangement is a separate thing.
        let mut d = DockState::default();
        assert!(d.sync_side_width(DockSide::Left, 308.0));
        assert_eq!(d.left_width(), 308.0);
        assert_eq!(d.layout(), Some(LayoutId::Essentials));
        assert!(!d.sync_side_width(DockSide::Left, 308.0));
        assert!(!d.sync_side_width(DockSide::Left, f32::NAN));
        assert_eq!(d.left_width(), 308.0);
    }

    #[test]
    fn reordering_moves_a_group_within_its_side_and_refuses_at_the_ends() {
        // Groups travel whole: raising Layers raises its whole tab group.
        let mut d = DockState::default();
        assert_eq!(
            d.panels_on(DockSide::Right),
            vec![
                PanelId::Properties,
                PanelId::Adjustments,
                PanelId::Layers,
                PanelId::History,
                PanelId::Color
            ]
        );
        assert_eq!(d.reorder(PanelId::Layers, true), Some(0));
        assert_eq!(
            d.panels_on(DockSide::Right),
            vec![
                PanelId::Layers,
                PanelId::Properties,
                PanelId::Adjustments,
                PanelId::History,
                PanelId::Color
            ]
        );
        assert_eq!(d.layout(), None);
        // The raised group's tab stays the shown one.
        assert!(d.is_active(PanelId::Layers));

        // The top group cannot go up and the bottom group cannot go down.
        assert_eq!(d.reorder(PanelId::Layers, true), None);
        assert_eq!(d.reorder(PanelId::Color, false), None);
        // A closed panel has no place in the order at all.
        assert_eq!(d.reorder(PanelId::Paths, true), None);
        // ...and the refusals left the arrangement alone.
        assert_eq!(
            d.panels_on(DockSide::Right),
            vec![
                PanelId::Layers,
                PanelId::Properties,
                PanelId::Adjustments,
                PanelId::History,
                PanelId::Color
            ]
        );
    }

    #[test]
    fn reordering_down_is_the_inverse_of_reordering_up() {
        let mut d = DockState::default();
        let before = d.panels_on(DockSide::Right);
        assert!(d.reorder(PanelId::Layers, true).is_some());
        assert!(d.reorder(PanelId::Layers, false).is_some());
        assert_eq!(d.panels_on(DockSide::Right), before);
    }

    #[test]
    fn moving_a_panel_to_the_index_it_already_holds_changes_nothing() {
        // The whole reason the intent carries a destination rather than a
        // direction: absorbing it twice must not move the panel twice.
        let mut d = DockState::default();
        let at = 2;
        assert_eq!(d.panels_on(DockSide::Right)[at], PanelId::Layers);
        assert!(d.reorder_to(PanelId::Layers, 0));
        let once = d.panels_on(DockSide::Right);
        assert_eq!(once[0], PanelId::Layers);
        assert!(!d.reorder_to(PanelId::Layers, 0));
        assert_eq!(d.panels_on(DockSide::Right), once);
        // An index off the end of the side is a refusal, not a clamp.
        assert!(!d.reorder_to(PanelId::Layers, 9));
        assert_eq!(d.panels_on(DockSide::Right), once);
    }

    #[test]
    fn moving_a_panel_across_the_side_carries_the_rest_along() {
        // Groups travel whole: moving the last panel (Color, tabbed with
        // History) to the front brings History with it and pushes the rest
        // down, rather than trading places with the first.
        let mut d = DockState::default();
        let before = d.panels_on(DockSide::Right);
        let last = *before.last().unwrap();
        assert!(d.reorder_to(last, 0));
        let mut expected = before.clone();
        let partner = d.placement(last).group;
        let group_members: Vec<PanelId> = before
            .iter()
            .copied()
            .filter(|p| d.placement(*p).group == partner)
            .collect();
        for id in group_members.iter().rev() {
            expected.remove(expected.iter().position(|p| p == id).unwrap());
            expected.insert(0, *id);
        }
        assert_eq!(d.panels_on(DockSide::Right), expected);
    }

    #[test]
    fn docking_reports_whether_it_moved_anything() {
        let mut d = DockState::default();
        assert!(d.dock(PanelId::Layers, DockSide::Bottom));
        assert_eq!(d.panels_on(DockSide::Bottom), vec![PanelId::Layers]);
        assert!(!d.dock(PanelId::Layers, DockSide::Bottom));
        // A closed panel docks (and opens) even onto the side it is filed under.
        assert!(!d.is_open(PanelId::Paths));
        assert!(d.dock(PanelId::Paths, DockSide::Right));
        assert!(d.is_open(PanelId::Paths));
    }

    #[test]
    fn moving_a_panel_across_sides_takes_it_off_the_old_one() {
        let mut d = DockState::default();
        assert!(d.panels_on(DockSide::Right).contains(&PanelId::Layers));
        d.dock(PanelId::Layers, DockSide::Left);
        assert!(!d.panels_on(DockSide::Right).contains(&PanelId::Layers));
        assert!(d.panels_on(DockSide::Left).contains(&PanelId::Layers));
        // A hand-arranged dock no longer claims to be a preset.
        assert_eq!(d.layout(), None);
    }

    #[test]
    fn clicking_a_tab_raises_that_panel() {
        // Essentials opens Properties and Adjustments tabbed together.
        let mut d = DockState::default();
        assert!(d.is_active(PanelId::Properties));
        assert!(!d.is_active(PanelId::Adjustments));
        d.raise(PanelId::Adjustments);
        assert!(d.is_active(PanelId::Adjustments));
        assert!(!d.is_active(PanelId::Properties));
        // Raising does not change what is docked, and marks the arrangement
        // as the user's.
        assert_eq!(
            d.panels_on(DockSide::Right),
            vec![
                PanelId::Properties,
                PanelId::Adjustments,
                PanelId::Layers,
                PanelId::History,
                PanelId::Color
            ]
        );
        assert_eq!(d.layout(), None);
    }

    #[test]
    fn dragging_a_panel_into_a_group_joins_it() {
        let mut d = DockState::default();
        // Layers lives in its own group; drag it into Properties/Adjustments.
        assert!(d.move_panel(PanelId::Layers, DockSide::Right, 0));
        assert_eq!(
            d.groups_on(DockSide::Right),
            vec![
                (
                    0,
                    vec![PanelId::Properties, PanelId::Adjustments, PanelId::Layers]
                ),
                (2, vec![PanelId::History, PanelId::Color])
            ]
        );
        // The dragged panel is the tab the group shows.
        assert!(d.is_active(PanelId::Layers));
        assert!(!d.is_active(PanelId::Properties));
        // Joining the group it already lives in changes nothing.
        assert!(!d.move_panel(PanelId::Layers, DockSide::Right, 0));
    }

    #[test]
    fn closing_the_last_tab_removes_the_group() {
        let mut d = DockState::from_layout(LayoutId::Essentials);
        // Right dock: Properties/Adjustments in group 0, Layers alone in 1,
        // History/Color in 2.
        assert_eq!(d.groups_on(DockSide::Right).len(), 3);
        d.set_open(PanelId::Layers, false);
        let groups = d.groups_on(DockSide::Right);
        assert_eq!(groups.len(), 2, "the empty group is gone");
        assert_eq!(groups[0].1, vec![PanelId::Properties, PanelId::Adjustments]);
        // Groups derive from placements, so renumbering is not needed to keep
        // the stack total: the surviving group is the drawn one.
        assert!(d.active_panel(DockSide::Right, 0).is_some());
    }

    #[test]
    fn an_old_session_without_groups_normalizes_to_one_active_tab() {
        // A session saved before groups existed loads every placement with
        // group 0 and active false; normalize picks the first open member of
        // each side so some tab is showing rather than none.
        let mut d = DockState::from_layout(LayoutId::Essentials);
        for p in d.placements.iter_mut() {
            p.group = 0;
            p.active = false;
        }
        d.normalize();
        let right = d.panels_on(DockSide::Right);
        let active = right.iter().filter(|p| d.is_active(**p)).count();
        assert_eq!(active, 1, "exactly one tab shows per side");
    }

    #[test]
    fn collapsing_hides_the_body_but_keeps_the_panel_open() {
        let mut d = DockState::default();
        d.toggle_collapsed(PanelId::Layers);
        assert!(d.is_collapsed(PanelId::Layers));
        assert!(d.is_open(PanelId::Layers));
        assert!(d.panels_on(DockSide::Right).contains(&PanelId::Layers));
    }

    #[test]
    fn a_hand_arranged_dock_stops_claiming_to_be_a_preset() {
        let mut d = DockState::default();
        assert_eq!(d.layout(), Some(LayoutId::Essentials));
        d.toggle_open(PanelId::Paths);
        assert_eq!(d.layout(), None);
    }

    #[test]
    fn closing_an_already_closed_panel_does_not_dirty_the_layout() {
        let mut d = DockState::default();
        d.set_open(PanelId::Paths, false);
        assert_eq!(d.layout(), Some(LayoutId::Essentials));
    }

    #[test]
    fn a_dock_survives_a_settings_round_trip() {
        let mut d = DockState::from_layout(LayoutId::Photography);
        d.set_side_width(DockSide::Left, 333.0);
        d.toggle_collapsed(PanelId::Info);
        let json = serde_json::to_string(&d).expect("serialize");
        let back: DockState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, d);
    }

    #[test]
    fn every_panel_has_a_title_and_a_unique_key() {
        let mut keys: Vec<&str> = PanelId::ALL.iter().map(|p| p.key()).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "two panels share a key");
        for panel in PanelId::ALL {
            assert!(!panel.title().is_empty(), "{panel:?}");
            assert!(!panel.key().is_empty(), "{panel:?}");
        }
    }

    #[test]
    fn every_layout_has_a_title_and_places_only_known_panels() {
        for layout in LayoutId::ALL {
            assert!(!layout.title().is_empty(), "{layout:?}");
            let state = DockState::from_layout(*layout);
            let placed: usize = DockSide::ALL
                .iter()
                .map(|s| state.panels_on(*s).len())
                .sum();
            assert_eq!(
                placed,
                layout.panels().len(),
                "{layout:?} lost a panel between its preset and its dock"
            );
        }
    }
}
