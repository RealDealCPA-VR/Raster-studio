//! What a click means.
//!
//! The whole of this crate is a view. Nothing below `src/` holds a `&mut
//! Document`; every control resolves to an [`Intent`], the workspace collects
//! them, and the application performs them — document edits through
//! [`editor_core::History`] so undo/redo stays uniform, everything else through
//! its own machinery.
//!
//! That indirection is not ceremony. It is what makes "this button emits that
//! command" a value a test can compare, with no window, no GPU and no event
//! loop: [`crate::menu::MenuAction::resolve`] and the panel models return
//! `Intent`s, and the tests assert on them directly.

use editor_core::Command;
use layer_model::LayerId;
use tools::ToolId;

use crate::dock::{DockSide, LayoutId, PanelId};
use crate::menu::MenuAction;
use crate::panels::channels::ChannelKind;
use crate::panels::history::HistoryJump;
use crate::tool_options::OptionValue;

/// One thing the user asked for.
///
/// [`Intent::Document`] is the only variant that carries a document edit, and
/// it carries a whole [`Command`] rather than a description of one — the UI has
/// already decided exactly what the edit is, so the application has nothing
/// left to interpret.
///
/// # Every workspace intent is idempotent
///
/// The variants [`crate::Workspace::absorb`] performs — the panel, dock,
/// layout, view-flag, ruler, channel and tool-option ones, i.e. exactly the set
/// an application routes back into the workspace — **must be safe to apply
/// twice**. The drawing side applies them as it draws: `view::docks` moves a
/// panel the moment its header control is clicked and *then* emits the intent,
/// because a control that rearranges itself under the pointer lands the next
/// click on the wrong thing. An application that drains the outbox and absorbs
/// what it finds is therefore applying an intent that has already landed.
///
/// So every one of them is an **absolute set**, never a relative step: `open`,
/// `side`, `to`, `on`, `visible`, the option's `value`. This is why
/// [`Intent::ReorderPanel`] carries a destination index rather than a
/// direction — as `up: bool` it moved the panel one place for the click and one
/// more for the absorb. `every_workspace_intent_is_idempotent_under_absorb`
/// pins the rule for the whole set.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// A document edit, ready to run through history.
    Document(Command),
    /// A named application action: something needing a dialog, the file system,
    /// or work this crate does not own. Every one is an enumerable value, so
    /// "the menu item does nothing" is a test failure rather than a discovery.
    Action(MenuAction),
    /// Replace a layer's *kind payload* — an adjustment's parameters, a text
    /// layer's content and styling, a shape's path, a group's blending mode.
    ///
    /// This is the one edit in the whole crate with no [`Command`] behind it,
    /// and the reason is structural rather than an oversight: `LayerPatch`
    /// deliberately covers every field of a layer **except** `kind`, because
    /// changing a layer's kind would have to move pixel and child ownership.
    /// Editing the payload *within* a kind is a different and much smaller
    /// operation, and `editor-core` has no command for it yet.
    ///
    /// Until it does, the application applies this itself and records its own
    /// history entry. The UI still never mutates the document — it emits the
    /// new payload and nothing more.
    EditLayerKind {
        layer: LayerId,
        kind: Box<layer_model::LayerKind>,
    },
    /// Make a tool active.
    SelectTool(ToolId),
    /// Write one option of one tool. `key` is the registry's stable option key.
    SetToolOption {
        tool: ToolId,
        key: &'static str,
        value: OptionValue,
    },
    /// Replace a gradient tool's ramp.
    ///
    /// A ramp is a list of stops, not a scalar, so it cannot travel as an
    /// [`OptionValue`] — and boxing it keeps [`Intent`] small enough that the
    /// common variants are not paying for it.
    SetToolGradient {
        tool: ToolId,
        gradient: Box<layer_model::Gradient>,
    },
    /// Return one tool to its registry defaults — the options bar's Reset.
    ///
    /// One intent rather than one per cleared key: the application's job is to
    /// re-read the tool's settings, and "everything went back to default" is a
    /// smaller and more honest thing to say than a list of writes that happens
    /// to be however many keys the user had touched.
    ResetToolOptions(ToolId),
    /// Replace the layers-panel selection. `active` is the layer that gains
    /// focus, and is always a member of `layers` unless `layers` is empty.
    SelectLayers {
        layers: Vec<LayerId>,
        active: Option<LayerId>,
    },
    /// Expand or collapse a group row.
    SetGroupExpanded {
        layer: LayerId,
        expanded: bool,
    },
    /// Move the history cursor by whole steps.
    HistoryJump(HistoryJump),
    /// Set the view zoom, as a scale factor (`1.0` is 100%).
    SetZoom(f32),
    /// Set the foreground / background colour, straight-alpha sRGB.
    SetForeground([f32; 4]),
    SetBackground([f32; 4]),
    /// Set the zoom's centre, in document pixels — the Navigator's pan.
    SetViewCenter((f32, f32)),
    /// Show or hide one colour channel, or the whole composite.
    ///
    /// A view setting rather than a document edit, which is why it is not an
    /// [`Intent::Document`]: hiding the red channel changes what the compositor
    /// is asked to draw, not what the file contains.
    SetChannelVisible {
        channel: ChannelKind,
        visible: bool,
    },
    /// Make one channel the editing target.
    SelectChannel(ChannelKind),
    /// Show or hide a dock panel.
    SetPanelOpen {
        panel: PanelId,
        open: bool,
    },
    /// Move a panel to another side of the window.
    DockPanel {
        panel: PanelId,
        side: DockSide,
    },
    /// Put a panel at index `to` among the panels open on its own side.
    ///
    /// A destination, not a direction, so absorbing it twice leaves the panel
    /// in one place — see the idempotency rule on [`Intent`].
    ReorderPanel {
        panel: PanelId,
        to: u8,
    },
    /// Switch the whole dock to a saved layout.
    ApplyLayout(LayoutId),
    /// Switch appearance.
    SetTheme(design::Theme),
    /// Turn one view overlay on or off.
    SetViewFlag {
        flag: ViewFlag,
        on: bool,
    },
    /// Change the unit the rulers and readouts measure in.
    SetRulerUnit(crate::dialogs::units::Unit),
}

impl Intent {
    /// The document edit this intent carries, if any.
    pub fn as_command(&self) -> Option<&Command> {
        match self {
            Intent::Document(c) => Some(c),
            _ => None,
        }
    }

    /// The named action this intent carries, if any.
    pub fn as_action(&self) -> Option<MenuAction> {
        match self {
            Intent::Action(a) => Some(*a),
            _ => None,
        }
    }
}

/// A toggle in the View menu.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum ViewFlag {
    Rulers,
    Guides,
    SmartGuides,
    Grid,
    PixelGrid,
    Snap,
    SelectionEdges,
    LayerEdges,
    ProofColors,
    GamutWarning,
    /// Mirror the *view* left to right. A view setting, not an edit: the
    /// document is untouched, which is what makes it a flag and not a command.
    FlipHorizontal,
    /// Mirror the view top to bottom.
    FlipVertical,
    /// Swap the pictorial cursors — the brush ring, the bucket — for a
    /// crosshair, for work that needs the exact pixel.
    PreciseCursor,
}

impl ViewFlag {
    /// Every flag, in menu order.
    pub const ALL: &'static [ViewFlag] = &[
        ViewFlag::Rulers,
        ViewFlag::Guides,
        ViewFlag::SmartGuides,
        ViewFlag::Grid,
        ViewFlag::PixelGrid,
        ViewFlag::Snap,
        ViewFlag::SelectionEdges,
        ViewFlag::LayerEdges,
        ViewFlag::ProofColors,
        ViewFlag::GamutWarning,
        ViewFlag::FlipHorizontal,
        ViewFlag::FlipVertical,
        ViewFlag::PreciseCursor,
    ];

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            ViewFlag::Rulers => "Rulers",
            ViewFlag::Guides => "Guides",
            ViewFlag::SmartGuides => "Smart Guides",
            ViewFlag::Grid => "Grid",
            ViewFlag::PixelGrid => "Pixel Grid",
            ViewFlag::Snap => "Snap",
            ViewFlag::SelectionEdges => "Selection Edges",
            ViewFlag::LayerEdges => "Layer Edges",
            ViewFlag::ProofColors => "Proof Colors",
            ViewFlag::GamutWarning => "Gamut Warning",
            ViewFlag::FlipHorizontal => "Flip View Horizontal",
            ViewFlag::FlipVertical => "Flip View Vertical",
            ViewFlag::PreciseCursor => "Precise Cursor",
        }
    }
}

/// Which view overlays are currently on.
///
/// A bit set rather than ten bools so the menu can be driven by
/// [`ViewFlag::ALL`] and a new flag cannot be forgotten in the menu builder.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ViewFlags {
    bits: u16,
}

impl ViewFlags {
    /// What a fresh window shows: rulers, guides, snapping and the two edge
    /// overlays. The pixel grid and the proofing overlays stay off — they are
    /// answers to questions the user has not asked yet.
    pub fn defaults() -> Self {
        let mut f = Self::default();
        for flag in [
            ViewFlag::Rulers,
            ViewFlag::Guides,
            ViewFlag::SmartGuides,
            ViewFlag::Snap,
            ViewFlag::SelectionEdges,
            ViewFlag::LayerEdges,
        ] {
            f.set(flag, true);
        }
        f
    }

    fn mask(flag: ViewFlag) -> u16 {
        let index = ViewFlag::ALL
            .iter()
            .position(|f| *f == flag)
            .expect("ViewFlag::ALL is exhaustive");
        1 << index
    }

    pub fn get(self, flag: ViewFlag) -> bool {
        self.bits & Self::mask(flag) != 0
    }

    pub fn set(&mut self, flag: ViewFlag, on: bool) {
        if on {
            self.bits |= Self::mask(flag);
        } else {
            self.bits &= !Self::mask(flag);
        }
    }

    pub fn toggle(&mut self, flag: ViewFlag) {
        self.set(flag, !self.get(flag));
    }
}

/// What the clipboard holds, as far as menu enablement is concerned.
///
/// The UI never touches clipboard *bytes* — it only needs to know whether Paste
/// has anything to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ClipboardState {
    /// Pixels are available to paste.
    pub pixels: bool,
    /// Whole layers are available to paste.
    pub layers: bool,
}

impl ClipboardState {
    /// Nothing has been copied yet.
    pub const EMPTY: ClipboardState = ClipboardState {
        pixels: false,
        layers: false,
    };

    /// `true` when Paste would produce something.
    pub const fn is_empty(self) -> bool {
        !self.pixels && !self.layers
    }
}

/// A long operation the status bar reports on.
#[derive(Clone, PartialEq, Debug)]
pub struct Progress {
    /// What is running, e.g. "Applying Gaussian Blur".
    pub label: String,
    /// `0.0..=1.0`, or `None` for an operation whose length is unknown.
    pub fraction: Option<f32>,
}

impl Progress {
    /// A determinate operation. `fraction` is clamped into `0.0..=1.0`, and a
    /// non-finite value is treated as indeterminate rather than painted as a
    /// NaN-wide bar.
    pub fn new(label: impl Into<String>, fraction: f32) -> Self {
        Self {
            label: label.into(),
            fraction: fraction.is_finite().then(|| fraction.clamp(0.0, 1.0)),
        }
    }

    /// An operation with no known length.
    pub fn indeterminate(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            fraction: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_flags_are_independent() {
        let mut f = ViewFlags::default();
        for flag in ViewFlag::ALL {
            assert!(!f.get(*flag));
        }
        f.set(ViewFlag::Grid, true);
        assert!(f.get(ViewFlag::Grid));
        for flag in ViewFlag::ALL.iter().filter(|x| **x != ViewFlag::Grid) {
            assert!(!f.get(*flag), "{flag:?} moved with Grid");
        }
        f.toggle(ViewFlag::Grid);
        assert!(!f.get(ViewFlag::Grid));
    }

    #[test]
    fn the_default_view_shows_rulers_but_not_the_pixel_grid() {
        let f = ViewFlags::defaults();
        assert!(f.get(ViewFlag::Rulers));
        assert!(f.get(ViewFlag::Snap));
        assert!(!f.get(ViewFlag::PixelGrid));
        assert!(!f.get(ViewFlag::GamutWarning));
    }

    #[test]
    fn every_view_flag_fits_in_the_bit_set() {
        // A tenth flag is fine; a seventeenth would silently alias.
        assert!(ViewFlag::ALL.len() <= 16);
        let mut f = ViewFlags::default();
        for flag in ViewFlag::ALL {
            f.set(*flag, true);
        }
        for flag in ViewFlag::ALL {
            assert!(f.get(*flag), "{flag:?} was aliased away");
        }
    }

    #[test]
    fn every_view_flag_has_a_label() {
        for flag in ViewFlag::ALL {
            assert!(!flag.label().is_empty(), "{flag:?}");
        }
    }

    #[test]
    fn an_empty_clipboard_knows_it() {
        assert!(ClipboardState::EMPTY.is_empty());
        assert!(!ClipboardState {
            pixels: true,
            layers: false
        }
        .is_empty());
    }

    #[test]
    fn a_non_finite_progress_fraction_is_treated_as_indeterminate() {
        assert_eq!(Progress::new("x", f32::NAN).fraction, None);
        assert_eq!(Progress::new("x", 2.0).fraction, Some(1.0));
        assert_eq!(Progress::new("x", -1.0).fraction, Some(0.0));
        assert_eq!(Progress::indeterminate("x").fraction, None);
    }
}
