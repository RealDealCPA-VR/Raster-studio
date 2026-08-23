//! Cursor management.
//!
//! The canvas speaks a richer cursor vocabulary than egui does — a brush ring
//! sized to the brush, a rotate arrow, crop marks — so [`CanvasCursor`] is the
//! vocabulary and [`CanvasCursor::to_egui`] is the one lossy step, with the
//! substitutions written down rather than left as a surprise. Anything egui
//! cannot express is *drawn*: [`CanvasCursor::draws_its_own`] says so, and the
//! painter puts the glyph on the canvas while the system pointer is hidden.
//!
//! # The precise toggle
//!
//! Every editor has one: a key that swaps the pictorial cursors — the brush
//! ring, the bucket, the eyedropper — for a plain crosshair, because a picture
//! of a bucket cannot be aimed. [`cursor_for_tool`] takes it as a parameter, so
//! the toggle is one boolean and not a special case scattered through the
//! tools.

use tools::registry::Cursor as ToolCursor;
use tools::ToolId;

/// A cursor the canvas can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CanvasCursor {
    /// The default arrow.
    #[default]
    Arrow,
    /// Four-way move.
    Move,
    /// A plain crosshair.
    Crosshair,
    /// The precise crosshair the toggle swaps in — same egui icon as
    /// [`CanvasCursor::Crosshair`], kept distinct so the state is visible to
    /// the UI and to tests.
    PreciseCross,
    /// A ring at the true brush size, drawn by the canvas.
    BrushOutline,
    Eyedropper,
    Bucket,
    /// The hand tool, ready to grab.
    OpenHand,
    /// The hand tool, mid-drag.
    ClosedHand,
    ZoomIn,
    ZoomOut,
    /// Rotate the view or the transform box.
    Rotate,
    /// The crop tool's corner marks.
    CropMarks,
    /// The slice tool's knife.
    Slice,
    /// Text insertion.
    Text,
    ResizeHorizontal,
    ResizeVertical,
    /// The `\` diagonal.
    ResizeNwSe,
    /// The `/` diagonal.
    ResizeNeSw,
    /// The gesture is not allowed here.
    NotAllowed,
}

impl CanvasCursor {
    /// Every cursor, so a test can assert the whole mapping is total.
    pub const ALL: &'static [CanvasCursor] = &[
        CanvasCursor::Arrow,
        CanvasCursor::Move,
        CanvasCursor::Crosshair,
        CanvasCursor::PreciseCross,
        CanvasCursor::BrushOutline,
        CanvasCursor::Eyedropper,
        CanvasCursor::Bucket,
        CanvasCursor::OpenHand,
        CanvasCursor::ClosedHand,
        CanvasCursor::ZoomIn,
        CanvasCursor::ZoomOut,
        CanvasCursor::Rotate,
        CanvasCursor::CropMarks,
        CanvasCursor::Slice,
        CanvasCursor::Text,
        CanvasCursor::ResizeHorizontal,
        CanvasCursor::ResizeVertical,
        CanvasCursor::ResizeNwSe,
        CanvasCursor::ResizeNeSw,
        CanvasCursor::NotAllowed,
    ];

    /// `true` when the canvas paints this cursor itself and the system pointer
    /// is hidden underneath it.
    pub const fn draws_its_own(self) -> bool {
        matches!(self, CanvasCursor::BrushOutline)
    }

    /// The egui cursor to install.
    ///
    /// egui 0.29 has no rotate, eyedropper, bucket, crop or slice cursor. Each
    /// falls back to the icon that reads closest, and the substitution is
    /// listed here so nobody has to guess why a bucket looks like a crosshair:
    ///
    /// | canvas cursor | egui icon | why |
    /// |---|---|---|
    /// | `Eyedropper`, `Bucket`, `CropMarks`, `Slice` | `Crosshair` | all four aim at a point |
    /// | `Rotate` | `AllScroll` | the only icon that reads as "turn", not "grab" |
    /// | `BrushOutline` | `None` | the ring is drawn instead |
    pub const fn to_egui(self) -> egui::CursorIcon {
        use egui::CursorIcon as I;
        match self {
            CanvasCursor::Arrow => I::Default,
            CanvasCursor::Move => I::Move,
            CanvasCursor::Crosshair | CanvasCursor::PreciseCross => I::Crosshair,
            CanvasCursor::BrushOutline => I::None,
            CanvasCursor::Eyedropper => I::Crosshair,
            CanvasCursor::Bucket => I::Crosshair,
            CanvasCursor::OpenHand => I::Grab,
            CanvasCursor::ClosedHand => I::Grabbing,
            CanvasCursor::ZoomIn => I::ZoomIn,
            CanvasCursor::ZoomOut => I::ZoomOut,
            CanvasCursor::Rotate => I::AllScroll,
            CanvasCursor::CropMarks => I::Crosshair,
            CanvasCursor::Slice => I::Crosshair,
            CanvasCursor::Text => I::Text,
            CanvasCursor::ResizeHorizontal => I::ResizeHorizontal,
            CanvasCursor::ResizeVertical => I::ResizeVertical,
            CanvasCursor::ResizeNwSe => I::ResizeNwSe,
            CanvasCursor::ResizeNeSw => I::ResizeNeSw,
            CanvasCursor::NotAllowed => I::NotAllowed,
        }
    }

    /// The precise-cursor substitution: pictorial cursors become a crosshair,
    /// everything else is already aimable and stays as it is.
    pub const fn precise(self) -> Self {
        match self {
            CanvasCursor::BrushOutline
            | CanvasCursor::Eyedropper
            | CanvasCursor::Bucket
            | CanvasCursor::CropMarks
            | CanvasCursor::Slice
            | CanvasCursor::Crosshair => CanvasCursor::PreciseCross,
            other => other,
        }
    }
}

/// The cursor a tool asks for, with the precise toggle applied.
pub fn cursor_for_tool(cursor: ToolCursor, precise: bool) -> CanvasCursor {
    let base = match cursor {
        ToolCursor::Arrow => CanvasCursor::Arrow,
        ToolCursor::Move => CanvasCursor::Move,
        ToolCursor::Crosshair => CanvasCursor::Crosshair,
        ToolCursor::BrushRing => CanvasCursor::BrushOutline,
        ToolCursor::Eyedropper => CanvasCursor::Eyedropper,
        ToolCursor::Bucket => CanvasCursor::Bucket,
        ToolCursor::OpenHand => CanvasCursor::OpenHand,
        ToolCursor::ZoomIn => CanvasCursor::ZoomIn,
        ToolCursor::Rotate => CanvasCursor::Rotate,
        ToolCursor::CropMarks => CanvasCursor::CropMarks,
        ToolCursor::Slice => CanvasCursor::Slice,
    };
    if precise {
        base.precise()
    } else {
        base
    }
}

/// The cursor for the tool with this id, with the precise toggle applied.
///
/// Falls back to the arrow for an id the registry does not know, which cannot
/// happen for a `ToolId` that came from the registry but keeps the function
/// total.
pub fn cursor_for_tool_id(id: ToolId, precise: bool) -> CanvasCursor {
    match tools::registry::info(id) {
        Some(info) => cursor_for_tool(info.cursor, precise),
        None => CanvasCursor::Arrow,
    }
}

/// Everything that can override the tool's own cursor, in priority order.
///
/// The pointer being over a panel wins over everything: the canvas must not
/// paint a brush ring on top of the layers list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorOverride {
    /// Nothing to override; use the tool's cursor.
    #[default]
    None,
    /// The pointer is over a panel, not the canvas.
    OverPanel,
    /// The hand tool, or the space bar standing in for it. `dragging` closes
    /// the hand, so a pan reads as a grab and not merely as an offer of one.
    Hand { dragging: bool },
    /// Hovering (or dragging) a transform handle.
    Handle(CanvasCursor),
    /// Hovering a guide.
    Guide { horizontal: bool },
    /// The gesture would do nothing here.
    Refused,
}

/// Resolve the cursor for one frame.
pub fn resolve(base: CanvasCursor, over: CursorOverride) -> CanvasCursor {
    match over {
        CursorOverride::OverPanel => CanvasCursor::Arrow,
        CursorOverride::Refused => CanvasCursor::NotAllowed,
        CursorOverride::Hand { dragging } => {
            if dragging {
                CanvasCursor::ClosedHand
            } else {
                CanvasCursor::OpenHand
            }
        }
        CursorOverride::Handle(c) => c,
        CursorOverride::Guide { horizontal } => {
            if horizontal {
                CanvasCursor::ResizeVertical
            } else {
                CanvasCursor::ResizeHorizontal
            }
        }
        CursorOverride::None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_in_the_registry_has_a_cursor() {
        for info in tools::registry::all() {
            let c = cursor_for_tool(info.cursor, false);
            // Nothing may fall through to "no cursor at all" unless it draws
            // its own glyph.
            if c.to_egui() == egui::CursorIcon::None {
                assert!(c.draws_its_own(), "{:?} has no cursor", info.id);
            }
            assert_eq!(cursor_for_tool_id(info.id, false), c, "{:?}", info.id);
        }
    }

    #[test]
    fn the_brush_draws_its_own_cursor_and_nothing_else_does() {
        for c in CanvasCursor::ALL {
            assert_eq!(c.draws_its_own(), *c == CanvasCursor::BrushOutline, "{c:?}");
            if !c.draws_its_own() {
                assert_ne!(c.to_egui(), egui::CursorIcon::None, "{c:?}");
            }
        }
    }

    #[test]
    fn the_precise_toggle_replaces_the_pictorial_cursors_only() {
        assert_eq!(
            cursor_for_tool(ToolCursor::BrushRing, true),
            CanvasCursor::PreciseCross
        );
        assert_eq!(
            cursor_for_tool(ToolCursor::Eyedropper, true),
            CanvasCursor::PreciseCross
        );
        assert_eq!(
            cursor_for_tool(ToolCursor::Bucket, true),
            CanvasCursor::PreciseCross
        );
        // The navigation and structural cursors are already aimable.
        assert_eq!(cursor_for_tool(ToolCursor::Move, true), CanvasCursor::Move);
        assert_eq!(
            cursor_for_tool(ToolCursor::OpenHand, true),
            CanvasCursor::OpenHand
        );
        assert_eq!(
            cursor_for_tool(ToolCursor::ZoomIn, true),
            CanvasCursor::ZoomIn
        );
        // Idempotent.
        for c in CanvasCursor::ALL {
            assert_eq!(c.precise().precise(), c.precise(), "{c:?}");
        }
    }

    #[test]
    fn the_precise_cursor_is_never_drawn_by_the_canvas() {
        // The whole point of the toggle is to get the system crosshair back.
        assert!(!CanvasCursor::PreciseCross.draws_its_own());
        assert_eq!(
            CanvasCursor::PreciseCross.to_egui(),
            egui::CursorIcon::Crosshair
        );
    }

    #[test]
    fn a_pointer_over_a_panel_gets_the_plain_arrow_whatever_the_tool_is() {
        for c in CanvasCursor::ALL {
            assert_eq!(
                resolve(*c, CursorOverride::OverPanel),
                CanvasCursor::Arrow,
                "{c:?}"
            );
        }
    }

    #[test]
    fn the_space_bar_hand_opens_and_closes() {
        assert_eq!(
            resolve(
                CanvasCursor::BrushOutline,
                CursorOverride::Hand { dragging: false }
            ),
            CanvasCursor::OpenHand
        );
        assert_eq!(
            resolve(
                CanvasCursor::BrushOutline,
                CursorOverride::Hand { dragging: true }
            ),
            CanvasCursor::ClosedHand
        );
    }

    #[test]
    fn guides_and_handles_and_refusals_override_the_tool() {
        assert_eq!(
            resolve(
                CanvasCursor::Move,
                CursorOverride::Handle(CanvasCursor::ResizeNwSe)
            ),
            CanvasCursor::ResizeNwSe
        );
        assert_eq!(
            resolve(
                CanvasCursor::Move,
                CursorOverride::Guide { horizontal: true }
            ),
            CanvasCursor::ResizeVertical
        );
        assert_eq!(
            resolve(
                CanvasCursor::Move,
                CursorOverride::Guide { horizontal: false }
            ),
            CanvasCursor::ResizeHorizontal
        );
        assert_eq!(
            resolve(CanvasCursor::Move, CursorOverride::Refused),
            CanvasCursor::NotAllowed
        );
        assert_eq!(
            resolve(CanvasCursor::Move, CursorOverride::None),
            CanvasCursor::Move
        );
        assert_eq!(CursorOverride::default(), CursorOverride::None);
    }

    #[test]
    fn an_unknown_tool_id_still_yields_a_cursor() {
        // Every id in ALL is in the registry, so this exercises the total-ness
        // of the mapping rather than a missing entry.
        for id in ToolId::ALL {
            assert!(CanvasCursor::ALL.contains(&cursor_for_tool_id(*id, false)));
            assert!(CanvasCursor::ALL.contains(&cursor_for_tool_id(*id, true)));
        }
    }
}
