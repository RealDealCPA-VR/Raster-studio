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
    /// Aim the shell's edits at the active layer's content or its mask
    /// coverage (card 007). The Properties panel's Layer/Mask control raises
    /// this alongside its own display state; the shell owns the validated
    /// target (`app_shell::edit_target`) and answers tools from it.
    /// Card 026: double-clicking a text layer's row in the Layers panel
    /// enters that layer for canvas text editing — the shell opens a live
    /// session on the existing layer (never a new one).
    EnterTextLayer {
        layer: layer_model::LayerId,
    },
    SetEditTarget {
        mask: bool,
    },
    /// Move the history cursor by whole steps.
    HistoryJump(HistoryJump),
    /// Set the view zoom, as a scale factor (`1.0` is 100%).
    SetZoom(f32),
    /// Set the foreground / background colour, straight-alpha sRGB.
    SetForeground([f32; 4]),
    SetBackground([f32; 4]),
    /// Open the colour picker dialog for one of the colour wells — the
    /// swatches' double-click.
    OpenColorPicker(crate::panels::color::ColorWell),
    /// Open the gradient editor dialog for the effective tool's ramp.
    OpenGradientEditor,
    /// Open the brush editor dialog over the effective tool's brush.
    OpenBrushEditor,
    /// W4-G: confirm the gesture the live tool is holding, as Enter does —
    /// the Ruler options bar's Straighten Layer button.
    ConfirmTool,
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
    /// The Actions panel: start recording every applied command.
    StartRecording,
    /// The Actions panel: stop recording, keeping what was captured.
    StopRecording,
    /// The Actions panel: replay the last stopped recording on the active
    /// document.
    ReplayRecording,
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
    /// W10-B: the Glyphs panel picked a character for text layer `layer`.
    /// The application inserts it at the caret of the live typing session
    /// when one is open (the draft, not the committed text), and otherwise
    /// appends it to the layer's text as one undo step.
    InsertGlyph {
        layer: LayerId,
        text: String,
    },
    /// W13-L: move the Animation timeline's playhead to `t_ms` — a ruler
    /// scrub, each playback frame, Stop. The application puts the tracked
    /// layers' values at that time on the document so the canvas shows the
    /// frame, with no history step and no dirty flag
    /// (`editor_core::timeline::seek`): the playhead is not an edit. An
    /// absolute time, so absorbing it twice lands in one place.
    SeekTimeline {
        t_ms: u32,
    },
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
    // W10-J: appended (bit positions follow `ALL`, so older flags keep theirs).
    /// View > Extras (Ctrl+H): the master switch over every overlay
    /// [`ViewFlag::is_extra`] names. Off hides them all at once while each
    /// keeps its own tick, so turning Extras back on restores exactly the set
    /// that was showing.
    Extras,
    /// View > Show > Slices: the committed Slice-tool regions on the canvas.
    Slices,
    /// View > Snap To > Guides.
    SnapToGuides,
    /// View > Snap To > Grid (only while the grid is showing).
    SnapToGrid,
    /// View > Snap To > Layers: other layers' edges and centres.
    SnapToLayers,
    /// View > Snap To > Document Bounds: the canvas edges and centre.
    SnapToBounds,
    /// View > Snap To > Slices (only while slices are showing).
    SnapToSlices,
    // W13-F: appended (bit positions follow `ALL`).
    /// View > Pattern Preview: the canvas repeated around itself, for
    /// seamless-pattern work. A view setting; the document is untouched.
    PatternPreview,
    // W16-K: appended (bit positions follow `ALL`).
    /// View > Show > Paths: the path being edited, its anchors and handles.
    Paths,
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
        ViewFlag::Extras,
        ViewFlag::Slices,
        ViewFlag::SnapToGuides,
        ViewFlag::SnapToGrid,
        ViewFlag::SnapToLayers,
        ViewFlag::SnapToBounds,
        ViewFlag::SnapToSlices,
        ViewFlag::PatternPreview,
        // W16-K.
        ViewFlag::Paths,
    ];

    /// W10-J: the View > Snap To submenu's targets, in Photoshop's order.
    pub const SNAP_TO: &'static [ViewFlag] = &[
        ViewFlag::SnapToGuides,
        ViewFlag::SnapToGrid,
        ViewFlag::SnapToLayers,
        ViewFlag::SnapToSlices,
        ViewFlag::SnapToBounds,
    ];

    /// W10-J: the overlays View > Extras hides at once.
    pub const fn is_extra(self) -> bool {
        matches!(
            self,
            ViewFlag::Guides
                | ViewFlag::SmartGuides
                | ViewFlag::Grid
                | ViewFlag::PixelGrid
                | ViewFlag::SelectionEdges
                | ViewFlag::LayerEdges
                | ViewFlag::Slices
                // W16-K.
                | ViewFlag::Paths
        )
    }

    /// W10-J: whether the flag's row lives in a View submenu (Show, Snap To)
    /// rather than in the flat list of toggles.
    pub const fn in_submenu(self) -> bool {
        matches!(
            self,
            ViewFlag::Slices
                // W16-K: under View > Show.
                | ViewFlag::Paths
                | ViewFlag::SnapToGuides
                | ViewFlag::SnapToGrid
                | ViewFlag::SnapToLayers
                | ViewFlag::SnapToBounds
                | ViewFlag::SnapToSlices
        )
    }

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
            ViewFlag::Extras => "Extras",
            ViewFlag::Slices => "Slices",
            ViewFlag::SnapToGuides => "Guides",
            ViewFlag::SnapToGrid => "Grid",
            ViewFlag::SnapToLayers => "Layers",
            ViewFlag::SnapToBounds => "Document Bounds",
            ViewFlag::SnapToSlices => "Slices",
            // W13-F: the menu row reads `ui.w13f.menu.pattern_preview`
            // (`MenuAction::label`); a const fn cannot call `tr`.
            ViewFlag::PatternPreview => "Pattern Preview",
            ViewFlag::Paths => "Paths",
        }
    }
}

/// Which view overlays are currently on.
///
/// A bit set rather than ten bools so the menu can be driven by
/// [`ViewFlag::ALL`] and a new flag cannot be forgotten in the menu builder.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ViewFlags {
    bits: u32,
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
            // W10-J: Extras on, slices shown, every Snap To target on —
            // Photoshop's fresh-install View menu.
            ViewFlag::Extras,
            ViewFlag::Slices,
            ViewFlag::SnapToGuides,
            ViewFlag::SnapToGrid,
            ViewFlag::SnapToLayers,
            ViewFlag::SnapToBounds,
            ViewFlag::SnapToSlices,
            // W16-K: paths show, as in Photopea.
            ViewFlag::Paths,
        ] {
            f.set(flag, true);
        }
        f
    }

    /// W10-J: whether `flag`'s overlay is actually drawn: its own tick, and
    /// for an [extra](ViewFlag::is_extra) View > Extras as well. The menu's
    /// checkmark reads [`ViewFlags::get`]; every painter reads this.
    pub fn shows(self, flag: ViewFlag) -> bool {
        self.get(flag) && (!flag.is_extra() || self.get(ViewFlag::Extras))
    }

    fn mask(flag: ViewFlag) -> u32 {
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
    /// Pixels are available to paste from the application's own store.
    pub pixels: bool,
    /// Pixels are available from the OS image clipboard (a screenshot, another
    /// application's copy). Card 052: these enable plain Paste but NOT Paste
    /// Into — the external payload has no in-document origin yet, and masking
    /// it by the selection is card 053's job.
    pub external_pixels: bool,
    /// Whole layers are available to paste.
    pub layers: bool,
}

impl ClipboardState {
    /// Nothing has been copied yet.
    pub const EMPTY: ClipboardState = ClipboardState {
        pixels: false,
        external_pixels: false,
        layers: false,
    };

    /// `true` when Paste would produce something.
    pub const fn is_empty(self) -> bool {
        !self.pixels && !self.external_pixels && !self.layers
    }

    /// `true` when the application's own store holds something — the only
    /// source Paste Into can honor before card 053.
    pub const fn has_internal_pixels(self) -> bool {
        self.pixels || self.layers
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
        // W10-J: the set is 32 bits wide now; a thirty-third flag would
        // silently alias.
        assert!(ViewFlag::ALL.len() <= 32);
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
            external_pixels: true,
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
