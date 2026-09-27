//! W18-A: Photopea's Divide Slices dialog, raised by the Divide… row of the
//! slice tools' canvas menu.
//!
//! Photopea's dialog (`jh` in its bundle) has two blocks, Horizontally and
//! Vertically, each a checkbox, a choice of "N equal parts" or "N pixels per
//! part", and the number N (default 4, 0 to 1000). Its answer is the `divide`
//! action: the target — the slice the right-click picked, or the whole canvas
//! when there was none — is cut into a grid and the cells take its place.
//! Horizontally cuts across the width (Photopea's `slicesAcross` /
//! `pixelsAcross`), Vertically down the height.
//!
//! * **N equal parts**: `t = floor(size / N)`, cuts at `start + j·t` for
//!   `j = 1 … N−1`; the last part takes the remainder.
//! * **N pixels per part**: cuts at `start + j·N` while `j·N < size`.
//!
//! The dialog lives in the `ui` crate and knows nothing of the slice store:
//! it is opened with the slice set it was raised over ([`DivideRequest`]),
//! and its confirmation is a [`SliceMenuRequest`] the canvas menu parks
//! ([`park`]) for the application, which takes it ([`take`]) in the arm of
//! the Clear row it emits and refuses it when the set has changed since.
//!
//! Photopea applies each change live and steps back on Cancel; this dialog
//! applies once, on OK.

use std::cell::RefCell;

use raster::PixelRect;

use crate::dialogs::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use crate::dialogs::controls::{checkbox_row, combo, integer};
use crate::strings::tr;

/// How one axis is divided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DivideMode {
    /// N parts of equal size (the last takes the remainder).
    EqualParts,
    /// Parts N pixels wide (the last takes what is left).
    PixelsPerPart,
}

impl DivideMode {
    /// Photopea's order.
    pub const ALL: [DivideMode; 2] = [DivideMode::EqualParts, DivideMode::PixelsPerPart];

    /// Photopea's wording.
    pub fn label(self) -> String {
        match self {
            DivideMode::EqualParts => tr("ui.divide_slice.equal").to_string(),
            DivideMode::PixelsPerPart => tr("ui.divide_slice.pixels").to_string(),
        }
    }
}

/// One block of the dialog.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DivideAxis {
    pub on: bool,
    pub mode: DivideMode,
    pub n: u32,
}

impl Default for DivideAxis {
    /// Photopea's: unticked, equal parts, 4.
    fn default() -> Self {
        Self {
            on: false,
            mode: DivideMode::EqualParts,
            n: 4,
        }
    }
}

impl DivideAxis {
    /// The cut positions inside `start .. start + size`, in order.
    fn cuts(&self, start: i64, size: i64) -> Vec<i64> {
        if !self.on || self.n == 0 || size <= 0 {
            return Vec::new();
        }
        let n = i64::from(self.n);
        match self.mode {
            DivideMode::EqualParts => {
                let t = size / n;
                if t == 0 {
                    return Vec::new();
                }
                (1..n).map(|j| start + j * t).collect()
            }
            DivideMode::PixelsPerPart => (1..)
                .map(|j| j * n)
                .take_while(|o| *o < size)
                .map(|o| start + o)
                .collect(),
        }
    }
}

/// Both blocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DivideSpec {
    /// Cuts across the width (columns).
    pub horizontally: DivideAxis,
    /// Cuts down the height (rows).
    pub vertically: DivideAxis,
}

impl DivideSpec {
    /// The grid `rect` is cut into, row by row.
    pub fn grid(&self, rect: PixelRect) -> Vec<PixelRect> {
        let (x0, y0) = (rect.x, rect.y);
        let (x1, y1) = (x0 + i64::from(rect.width), y0 + i64::from(rect.height));
        let mut xs = vec![x0];
        xs.extend(self.horizontally.cuts(x0, x1 - x0));
        xs.push(x1);
        let mut ys = vec![y0];
        ys.extend(self.vertically.cuts(y0, y1 - y0));
        ys.push(y1);
        xs.dedup();
        ys.dedup();
        let mut out = Vec::new();
        for r in ys.windows(2) {
            for c in xs.windows(2) {
                if c[1] > c[0] && r[1] > r[0] {
                    out.push(PixelRect::new(
                        c[0],
                        r[0],
                        (c[1] - c[0]) as u32,
                        (r[1] - r[0]) as u32,
                    ));
                }
            }
        }
        out
    }
}

/// What is divided.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DivideTarget {
    /// Slice `index` of the set, which the grid replaces.
    Slice(usize),
    /// An area (the canvas) the grid is added over; the set is kept.
    Area(PixelRect),
}

/// A divide over one slice set.
#[derive(Clone, PartialEq, Debug)]
pub struct DivideRequest {
    /// The set the dialog was raised over: the application refuses the
    /// request when its set is no longer this one.
    pub before: Vec<PixelRect>,
    pub target: DivideTarget,
    pub spec: DivideSpec,
}

impl DivideRequest {
    /// The set after the divide, and the index of the slice it removed (the
    /// target slice), or `None` when there is nothing to cut.
    pub fn result(&self) -> Option<(Vec<PixelRect>, Option<usize>)> {
        let (rect, removed) = match self.target {
            DivideTarget::Slice(i) => (*self.before.get(i)?, Some(i)),
            DivideTarget::Area(rect) => (rect, None),
        };
        let grid = self.spec.grid(rect);
        if grid.len() < 2 {
            return None;
        }
        let mut out = self.before.clone();
        if let Some(i) = removed {
            out.remove(i);
        }
        out.extend(grid);
        Some((out, removed))
    }
}

/// What the slice tools' canvas menu asks the application to do to the
/// slice set. Each carries the set it was built over.
#[derive(Clone, PartialEq, Debug)]
pub enum SliceMenuRequest {
    /// Photopea's right-click picks the slice under the pointer.
    Pick {
        index: usize,
        before: Vec<PixelRect>,
    },
    /// The menu's Delete: the slice under the pointer goes.
    Delete {
        index: usize,
        before: Vec<PixelRect>,
    },
    /// Divide Slices confirmed.
    Divide(DivideRequest),
}

thread_local! {
    /// The request waiting for the application's Clear arm (see the module
    /// documentation). One at a time: a newer one replaces it.
    static PARKED: RefCell<Option<SliceMenuRequest>> = const { RefCell::new(None) };
}

/// Park `request` for the application.
pub fn park(request: SliceMenuRequest) {
    PARKED.with(|slot| *slot.borrow_mut() = Some(request));
}

/// Take the parked request, if any.
pub fn take() -> Option<SliceMenuRequest> {
    PARKED.with(|slot| slot.borrow_mut().take())
}

/// The dialog: the request it will confirm, with its spec being edited.
#[derive(Clone, PartialEq, Debug)]
pub struct DivideSliceDialog {
    pub request: DivideRequest,
}

impl DivideSliceDialog {
    /// Over `before`, dividing `target`, with Photopea's defaults.
    pub fn new(before: Vec<PixelRect>, target: DivideTarget) -> Self {
        Self {
            request: DivideRequest {
                before,
                target,
                spec: DivideSpec::default(),
            },
        }
    }

    pub fn title(&self) -> &'static str {
        tr("ui.divide_slice.title")
    }

    /// Why OK is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<&'static str> {
        let spec = self.request.spec;
        if !spec.horizontally.on && !spec.vertically.on {
            return Some(tr("ui.divide_slice.none"));
        }
        if self.request.result().is_none() {
            return Some(tr("ui.divide_slice.nothing"));
        }
        None
    }

    /// The request a confirmation hands over.
    pub fn confirm(&self) -> Option<SliceMenuRequest> {
        self.blocked_reason()
            .is_none()
            .then(|| SliceMenuRequest::Divide(self.request.clone()))
    }

    /// Escape and Enter, without drawing.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<SliceMenuRequest> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(request) = self.confirm() {
                return DialogOutcome::Confirmed(request);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &egui::Context) -> DialogOutcome<SliceMenuRequest> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "divide-slice",
            self.title(),
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        let spec = &mut self.request.spec;
        for (key, axis, salt) in [
            (
                "ui.divide_slice.horizontally",
                &mut spec.horizontally,
                "divide-slice-h",
            ),
            (
                "ui.divide_slice.vertically",
                &mut spec.vertically,
                "divide-slice-v",
            ),
        ] {
            checkbox_row(ui, tr(key), &mut axis.on);
            combo(
                ui,
                salt,
                &mut axis.mode,
                &DivideMode::ALL,
                DivideMode::label,
                |_| None,
            );
            design::inspector_field(ui, tr("ui.divide_slice.n"), |ui| {
                let mut n = i64::from(axis.n);
                if integer(ui, &mut n, 0..=1000).changed() {
                    axis.n = n.clamp(0, 1000) as u32;
                }
            });
        }
        caption(ui, tr("ui.divide_slice.caption"));
        action_row(
            ui,
            tr("ui.divide_slice.confirm"),
            self.blocked_reason(),
            &[],
        )
    }
}

/// The egui memory slot the open dialog lives in.
fn slot() -> egui::Id {
    egui::Id::new("raster-divide-slice-dialog")
}

/// Open `dialog` (replacing one already open).
pub fn open(ctx: &egui::Context, dialog: DivideSliceDialog) {
    ctx.data_mut(|d| d.insert_temp(slot(), Some(dialog)));
}

/// Whether the dialog is open.
pub fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Option<DivideSliceDialog>>(slot()))
        .flatten()
        .is_some()
}

/// Edit the open dialog in place (a test's typing), answering `None` when
/// none is open.
pub fn with_open<R>(ctx: &egui::Context, f: impl FnOnce(&mut DivideSliceDialog) -> R) -> Option<R> {
    let mut dialog = ctx
        .data(|d| d.get_temp::<Option<DivideSliceDialog>>(slot()))
        .flatten()?;
    let out = f(&mut dialog);
    open(ctx, dialog);
    Some(out)
}

/// Draw the open dialog for one frame; its confirmation, when this frame
/// confirmed it. Cancel and OK close it.
pub fn draw(ctx: &egui::Context) -> Option<SliceMenuRequest> {
    let mut dialog = ctx
        .data(|d| d.get_temp::<Option<DivideSliceDialog>>(slot()))
        .flatten()?;
    let outcome = dialog.show(ctx);
    let (keep, confirmed) = match outcome {
        DialogOutcome::Open => (Some(dialog), None),
        DialogOutcome::Cancelled => (None, None),
        DialogOutcome::Confirmed(request) => (None, Some(request)),
    };
    ctx.data_mut(|d| d.insert_temp(slot(), keep));
    confirmed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(mode: DivideMode, n: u32) -> DivideAxis {
        DivideAxis { on: true, mode, n }
    }

    #[test]
    fn equal_parts_cut_at_floor_steps_and_the_last_part_takes_the_rest() {
        let spec = DivideSpec {
            horizontally: axis(DivideMode::EqualParts, 3),
            vertically: DivideAxis::default(),
        };
        let grid = spec.grid(PixelRect::new(10, 20, 10, 6));
        assert_eq!(
            grid,
            vec![
                PixelRect::new(10, 20, 3, 6),
                PixelRect::new(13, 20, 3, 6),
                PixelRect::new(16, 20, 4, 6),
            ]
        );
    }

    #[test]
    fn pixels_per_part_cut_every_n_and_both_axes_make_a_grid() {
        let spec = DivideSpec {
            horizontally: axis(DivideMode::PixelsPerPart, 4),
            vertically: axis(DivideMode::EqualParts, 2),
        };
        let grid = spec.grid(PixelRect::new(0, 0, 10, 8));
        // Columns 0..4, 4..8, 8..10; rows 0..4, 4..8.
        assert_eq!(grid.len(), 6, "{grid:?}");
        assert_eq!(grid[2], PixelRect::new(8, 0, 2, 4));
        assert_eq!(grid[3], PixelRect::new(0, 4, 4, 4));
    }

    #[test]
    fn a_divided_slice_is_replaced_by_its_grid_and_the_rest_are_kept() {
        let before = vec![PixelRect::new(0, 0, 2, 2), PixelRect::new(4, 0, 4, 4)];
        let mut d = DivideSliceDialog::new(before.clone(), DivideTarget::Slice(1));
        assert_eq!(d.blocked_reason(), Some(tr("ui.divide_slice.none")));
        d.request.spec.vertically = axis(DivideMode::EqualParts, 2);
        let Some(SliceMenuRequest::Divide(request)) = d.confirm() else {
            panic!("OK is live once an axis is ticked");
        };
        let (after, removed) = request.result().unwrap();
        assert_eq!(removed, Some(1));
        assert_eq!(
            after,
            vec![
                before[0],
                PixelRect::new(4, 0, 4, 2),
                PixelRect::new(4, 2, 4, 2),
            ]
        );
        // One part is no divide.
        d.request.spec.vertically.n = 1;
        assert_eq!(d.confirm(), None);
    }
}
