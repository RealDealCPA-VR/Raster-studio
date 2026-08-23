//! Canvas Size — change the frame without resampling the pixels.
//!
//! The whole dialog turns on one piece of arithmetic: given the old size, the
//! new size and a nine-way anchor, where does the existing image land? That
//! offset can be negative (the canvas is being cropped), and getting its sign
//! or its rounding wrong quietly shifts every layer, so it is a pure function
//! with a case per anchor.

use design::{
    color32, current_tokens,
    egui_theme::rounding,
    tokens::palette::ColorRole,
    tokens::{Radius, Space},
};
use egui::{vec2, Context, Sense};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, warning, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::color_edit::ColorEdit;
use super::color_picker::ScreenSampler;
use super::controls::{checkbox_row, combo, numeric, swatch};
use super::new_document::{BackgroundContents, MAX_DIMENSION, MAX_PIXELS};
use super::units::{format_bytes, Unit};
use super::{ids, sizes};

/// Where the existing image sits inside the new canvas.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// All nine, in reading order — which is also the order the 3x3 grid is
    /// drawn in, so the widget and the tests cannot disagree about which cell
    /// is which.
    pub const ALL: [Anchor; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Center,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    /// Column, 0 = left, 1 = centre, 2 = right.
    pub const fn column(self) -> u8 {
        match self {
            Self::TopLeft | Self::Left | Self::BottomLeft => 0,
            Self::Top | Self::Center | Self::Bottom => 1,
            Self::TopRight | Self::Right | Self::BottomRight => 2,
        }
    }

    /// Row, 0 = top, 1 = middle, 2 = bottom.
    pub const fn row(self) -> u8 {
        match self {
            Self::TopLeft | Self::Top | Self::TopRight => 0,
            Self::Left | Self::Center | Self::Right => 1,
            Self::BottomLeft | Self::Bottom | Self::BottomRight => 2,
        }
    }

    /// The anchor at `(row, column)`, or `None` outside the 3x3 grid.
    pub fn at(row: u8, column: u8) -> Option<Anchor> {
        (row < 3 && column < 3).then(|| Self::ALL[usize::from(row) * 3 + usize::from(column)])
    }

    /// Human name, for the accessible label and the tooltip.
    pub const fn label(self) -> &'static str {
        match self {
            Self::TopLeft => "Top left",
            Self::Top => "Top",
            Self::TopRight => "Top right",
            Self::Left => "Left",
            Self::Center => "Center",
            Self::Right => "Right",
            Self::BottomLeft => "Bottom left",
            Self::Bottom => "Bottom",
            Self::BottomRight => "Bottom right",
        }
    }

    /// Where the old content's origin lands in the new canvas, in pixels.
    ///
    /// Negative means the canvas is smaller on that side and the content is
    /// being cropped.
    ///
    /// A centred odd difference divides toward zero, which is what makes the
    /// odd pixel always belong to the trailing edge: growing a 100 px canvas by
    /// one adds the column on the right, and shrinking it by one takes that
    /// same column away again. Flooring instead would add on the right but
    /// remove from the left, so nudging the size up and back down would move
    /// the image.
    pub fn offset(self, old: (u32, u32), new: (u32, u32)) -> (i64, i64) {
        let dx = i64::from(new.0) - i64::from(old.0);
        let dy = i64::from(new.1) - i64::from(old.1);
        let x = match self.column() {
            0 => 0,
            1 => dx / 2,
            _ => dx,
        };
        let y = match self.row() {
            0 => 0,
            1 => dy / 2,
            _ => dy,
        };
        (x, y)
    }
}

/// What one edge of the canvas does when the size changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Change {
    /// The edge does not move.
    #[default]
    None,
    /// Room is added on this side.
    Grow,
    /// This side is cropped away.
    Shrink,
}

/// One edge of the canvas.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// All four, in the order the anchor cell paints them.
    pub const ALL: [Side; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];

    /// The [`crate::icons::ui_icon`] key this side carries for `change`, or
    /// `None` when it does not move.
    ///
    /// A growing edge points **outward** — that is the direction the canvas is
    /// travelling — and a cropped one points inward.
    ///
    /// These were four typed triangles, written as `"\u{25B2}"` rather than as
    /// the character so the tofu gate's source scan did not see them. Two of
    /// the four — U+25B2 and U+25BC — are absent from egui 0.29's font stack,
    /// so the top and bottom edges of the selected anchor cell were drawn as
    /// empty squares.
    pub const fn icon(self, change: Change) -> Option<&'static str> {
        let (outward, inward) = match self {
            Self::Left => ("chevron-left", "chevron-right"),
            Self::Right => ("chevron-right", "chevron-left"),
            Self::Top => ("chevron-up", "chevron-down"),
            Self::Bottom => ("chevron-down", "chevron-up"),
        };
        match change {
            Change::None => None,
            Change::Grow => Some(outward),
            Change::Shrink => Some(inward),
        }
    }
}

/// What each edge of the canvas does, for one anchor and one size change.
///
/// Derived from [`Anchor::offset`] rather than from the anchor's row and column
/// directly, so the arrows and the offset the dialog commits cannot disagree:
/// the old content occupies `offset .. offset + old`, and each edge's movement
/// is the gap between that span and the new canvas.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EdgeChange {
    pub left: Change,
    pub right: Change,
    pub top: Change,
    pub bottom: Change,
}

impl EdgeChange {
    /// The four edges, for painting and for iterating in a test.
    pub const fn get(&self, side: Side) -> Change {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
            Side::Top => self.top,
            Side::Bottom => self.bottom,
        }
    }

    /// Whether nothing moves at all.
    pub fn is_static(&self) -> bool {
        Side::ALL.iter().all(|s| self.get(*s) == Change::None)
    }
}

fn change_of(amount: i64) -> Change {
    match amount {
        0 => Change::None,
        n if n > 0 => Change::Grow,
        _ => Change::Shrink,
    }
}

/// How each edge moves when `old` becomes `new` around `anchor`.
pub fn edge_changes(anchor: Anchor, old: (u32, u32), new: (u32, u32)) -> EdgeChange {
    let (ox, oy) = anchor.offset(old, new);
    let dx = i64::from(new.0) - i64::from(old.0);
    let dy = i64::from(new.1) - i64::from(old.1);
    EdgeChange {
        left: change_of(ox),
        right: change_of(dx - ox),
        top: change_of(oy),
        bottom: change_of(dy - oy),
    }
}

/// What the dialog commits to.
#[derive(Clone, PartialEq, Debug)]
pub struct CanvasSizeSpec {
    pub width: u32,
    pub height: u32,
    /// Where the existing content lands, already resolved from the anchor.
    pub offset: (i64, i64),
    pub anchor: Anchor,
    /// What fills any newly exposed area.
    pub background: BackgroundContents,
}

impl CanvasSizeSpec {
    /// Whether the new canvas is a legal size.
    pub fn is_valid(&self) -> bool {
        self.width >= 1
            && self.height >= 1
            && self.width <= MAX_DIMENSION
            && self.height <= MAX_DIMENSION
            && u64::from(self.width) * u64::from(self.height) <= MAX_PIXELS
    }

    /// Whether any existing pixel falls outside the new canvas.
    pub fn crops(&self, old: (u32, u32)) -> bool {
        let (x, y) = self.offset;
        x < 0
            || y < 0
            || x + i64::from(old.0) > i64::from(self.width)
            || y + i64::from(old.1) > i64::from(self.height)
    }
}

/// Canvas Size.
#[derive(Clone, Debug)]
pub struct CanvasSizeDialog {
    old_width: u32,
    old_height: u32,
    /// The new size as typed, in `unit`.
    width: f64,
    height: f64,
    unit: Unit,
    /// In relative mode the fields are *deltas* added to the current size.
    relative: bool,
    anchor: Anchor,
    background: BackgroundContents,
    ppi: f64,
    /// The nested colour picker, when the Custom fill swatch is clicked.
    color_edit: ColorEdit<()>,
}

impl CanvasSizeDialog {
    /// Open on a document of `width` x `height` at `ppi`.
    pub fn new(width: u32, height: u32, ppi: f64) -> Self {
        Self {
            old_width: width.max(1),
            old_height: height.max(1),
            width: f64::from(width.max(1)),
            height: f64::from(height.max(1)),
            unit: Unit::Pixels,
            relative: false,
            anchor: Anchor::Center,
            background: BackgroundContents::Transparent,
            ppi,
            color_edit: ColorEdit::new(),
        }
    }

    /// What fills the newly exposed area.
    pub fn background(&self) -> BackgroundContents {
        self.background
    }

    /// Choose the fill.
    pub fn set_background(&mut self, background: BackgroundContents) {
        self.background = background;
    }

    /// The nested colour picker, when the Custom swatch has been clicked.
    pub fn color_edit(&self) -> &ColorEdit<()> {
        &self.color_edit
    }

    /// Mutable access to it.
    pub fn color_edit_mut(&mut self) -> &mut ColorEdit<()> {
        &mut self.color_edit
    }

    /// The size the canvas has now.
    pub fn current(&self) -> (u32, u32) {
        (self.old_width, self.old_height)
    }

    /// The selected anchor.
    pub fn anchor(&self) -> Anchor {
        self.anchor
    }

    /// Choose an anchor.
    pub fn set_anchor(&mut self, anchor: Anchor) {
        self.anchor = anchor;
    }

    /// Whether the fields hold deltas rather than absolute sizes.
    pub fn relative(&self) -> bool {
        self.relative
    }

    /// Switch between absolute and relative entry, rewriting the fields so the
    /// resulting canvas does not change under the user.
    pub fn set_relative(&mut self, relative: bool) {
        if relative == self.relative {
            return;
        }
        let (w, h) = self.new_size();
        self.relative = relative;
        if relative {
            self.set_field_pixels(
                f64::from(w) - f64::from(self.old_width),
                f64::from(h) - f64::from(self.old_height),
            );
        } else {
            self.set_field_pixels(f64::from(w), f64::from(h));
        }
    }

    /// Set the width field from a pixel amount (an absolute size, or a delta in
    /// relative mode).
    pub fn set_width_pixels(&mut self, px: f64) {
        self.width = self
            .unit
            .from_pixels(px, self.ppi, f64::from(self.old_width));
    }

    /// Set the height field from a pixel amount.
    pub fn set_height_pixels(&mut self, px: f64) {
        self.height = self
            .unit
            .from_pixels(px, self.ppi, f64::from(self.old_height));
    }

    fn set_field_pixels(&mut self, w: f64, h: f64) {
        self.set_width_pixels(w);
        self.set_height_pixels(h);
    }

    /// Change the unit the fields are typed in, keeping the resulting canvas.
    pub fn set_unit(&mut self, unit: Unit) {
        if unit == self.unit {
            return;
        }
        let w_px = self
            .unit
            .to_pixels(self.width, self.ppi, f64::from(self.old_width));
        let h_px = self
            .unit
            .to_pixels(self.height, self.ppi, f64::from(self.old_height));
        self.unit = unit;
        self.set_field_pixels(w_px, h_px);
    }

    /// The canvas the fields currently describe.
    pub fn new_size(&self) -> (u32, u32) {
        let w = self
            .unit
            .to_pixels(self.width, self.ppi, f64::from(self.old_width));
        let h = self
            .unit
            .to_pixels(self.height, self.ppi, f64::from(self.old_height));
        let (w, h) = if self.relative {
            (
                f64::from(self.old_width) + w,
                f64::from(self.old_height) + h,
            )
        } else {
            (w, h)
        };
        (round_side(w), round_side(h))
    }

    /// The specification the dialog currently describes.
    pub fn spec(&self) -> CanvasSizeSpec {
        let new = self.new_size();
        CanvasSizeSpec {
            width: new.0,
            height: new.1,
            offset: self.anchor.offset(self.current(), new),
            anchor: self.anchor,
            background: self.background,
        }
    }

    /// Draw the dialog for one frame.
    ///
    /// `sampler` reaches the nested colour picker's eyedropper; `None` draws
    /// that button disabled with its reason.
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        let nested = self.color_edit.is_open();
        let keys = if nested {
            DialogKeys::NONE
        } else {
            DialogKeys::read(ctx)
        };
        let mut outcome = super::chrome::resolve(self, keys);
        let drawn = modal(
            ctx,
            "canvas-size",
            self.title(),
            Some("Add or remove room around the image. Pixels are not resampled."),
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        if let Some(((), rgba)) = self.color_edit.show(ctx, "canvas-size-fill", sampler) {
            self.background = BackgroundContents::Custom(rgba);
        }
        if nested {
            return DialogOutcome::Open;
        }
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
        caption(
            ui,
            format!(
                "Current: {} x {} px  ·  {}",
                self.old_width,
                self.old_height,
                format_bytes(
                    u64::from(self.old_width)
                        * u64::from(self.old_height)
                        * super::image_size::BYTES_PER_PIXEL
                )
            ),
        );
        hairline(ui);

        design::section_header(ui, "New size");
        let mut relative = self.relative;
        if checkbox_row(ui, "Relative", &mut relative).changed() {
            self.set_relative(relative);
        }
        design::inspector_field(ui, "Width", |ui| {
            let range = if self.relative {
                -1.0e7..=1.0e7
            } else {
                0.0..=1.0e7
            };
            numeric(
                ui,
                &mut self.width,
                range,
                self.unit.decimals(),
                self.unit.short(),
            );
        });
        design::inspector_field(ui, "Height", |ui| {
            let range = if self.relative {
                -1.0e7..=1.0e7
            } else {
                0.0..=1.0e7
            };
            numeric(
                ui,
                &mut self.height,
                range,
                self.unit.decimals(),
                self.unit.short(),
            );
        });
        design::inspector_field(ui, "Units", |ui| {
            let mut unit = self.unit;
            let options: Vec<Unit> = Unit::ALL.to_vec();
            if combo(
                ui,
                "cs-unit",
                &mut unit,
                &options,
                |u| u.label().to_string(),
                |_| None,
            ) {
                self.set_unit(unit);
            }
        });

        design::section_header(ui, "Anchor");
        let old = self.current();
        let new = self.new_size();
        anchor_grid(ui, &mut self.anchor, old, new);

        design::section_header(ui, "Canvas extension");
        let mut open_picker = false;
        design::inspector_field(ui, "Fill", |ui| {
            let entries = BackgroundContents::MENU;
            let mut index = entries
                .iter()
                .position(|e| e.same_entry(self.background))
                .unwrap_or(2);
            if combo(
                ui,
                "cs-fill",
                &mut index,
                &[0, 1, 2, 3],
                |i| entries[i].label().to_string(),
                |_| None,
            ) {
                // Keep the custom colour already chosen rather than resetting
                // it to the menu entry's placeholder white.
                self.background = match (entries[index], self.background) {
                    (BackgroundContents::Custom(_), BackgroundContents::Custom(rgba)) => {
                        BackgroundContents::Custom(rgba)
                    }
                    (entry, _) => entry,
                };
            }
            if let BackgroundContents::Custom(rgba) = self.background {
                open_picker = swatch(
                    ui,
                    ids::custom_background("canvas-size"),
                    rgba,
                    sizes::swatch_square(),
                )
                .clicked();
            }
        });
        if open_picker {
            if let BackgroundContents::Custom(rgba) = self.background {
                self.color_edit.open((), rgba);
            }
        }

        let spec = self.spec();
        ui.add_space(Space::Small.pt());
        caption(
            ui,
            format!(
                "New: {} x {} px  ·  offset {}, {}",
                spec.width, spec.height, spec.offset.0, spec.offset.1
            ),
        );
        if spec.is_valid() && spec.crops(self.current()) {
            warning(
                ui,
                "The new canvas is smaller — content outside it will be clipped.",
            );
        }
        if let Some(reason) = self.blocked_reason() {
            warning(ui, reason);
        }
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

/// The 3x3 anchor selector.
///
/// Each cell is a real hit target from `design`'s metrics. The selected cell
/// carries an arrow on every side that moves — outward where the canvas gains
/// room, inward where it is cropped — so the widget says what it *does* rather
/// than only which square is lit. Which arrows those are is
/// [`edge_changes`], a pure function of the anchor and the two sizes, so the
/// picture and the offset the dialog commits are derived from the same
/// arithmetic.
pub fn anchor_grid(
    ui: &mut egui::Ui,
    anchor: &mut Anchor,
    old: (u32, u32),
    new: (u32, u32),
) -> bool {
    let t = current_tokens(ui);
    let cell = t.metrics.toolbar_button;
    let gap = t.borders.hairline * 2.0;
    let mut changed = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(gap, gap);
        for row in 0..3u8 {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(gap, gap);
                for column in 0..3u8 {
                    let Some(candidate) = Anchor::at(row, column) else {
                        continue;
                    };
                    let (rect, response) = ui.allocate_exact_size(vec2(cell, cell), Sense::click());
                    let selected = candidate == *anchor;
                    if ui.is_rect_visible(rect) {
                        let palette = &t.palette;
                        let fill = if selected {
                            palette.color(ColorRole::Accent)
                        } else if response.hovered() {
                            palette.color(ColorRole::ControlFillHovered)
                        } else {
                            palette.color(ColorRole::ControlFill)
                        };
                        let radius = Radius::Small.resolve(&t.radii, cell);
                        ui.painter()
                            .rect_filled(rect, rounding(radius), color32(fill));
                        if selected {
                            // Drawn, not typed: see `Side::icon`. The ink is the
                            // palette's on-accent colour because the selected
                            // cell is filled with the accent.
                            let ink = color32(palette.color(ColorRole::TextOnAccent));
                            let width = t.borders.hairline * 1.5;
                            let changes = edge_changes(candidate, old, new);
                            let mark = cell * 0.5;
                            crate::icons::ui_icon("anchor-block").paint(
                                ui.painter(),
                                egui::Rect::from_center_size(rect.center(), vec2(mark, mark)),
                                ink,
                                width,
                            );
                            for side in Side::ALL {
                                let Some(key) = side.icon(changes.get(side)) else {
                                    continue;
                                };
                                // Half the mark in from the edge, so the icon's
                                // own box sits inside the cell.
                                let inset = mark * 0.5;
                                let at = match side {
                                    Side::Left => rect.left_center() + egui::Vec2::X * inset,
                                    Side::Right => rect.right_center() - egui::Vec2::X * inset,
                                    Side::Top => rect.center_top() + egui::Vec2::Y * inset,
                                    Side::Bottom => rect.center_bottom() - egui::Vec2::Y * inset,
                                };
                                crate::icons::ui_icon(key).paint(
                                    ui.painter(),
                                    egui::Rect::from_center_size(at, vec2(mark, mark)),
                                    ink,
                                    width,
                                );
                            }
                        }
                    }
                    let response = response.on_hover_text(candidate.label());
                    if response.clicked() && !selected {
                        *anchor = candidate;
                        changed = true;
                    }
                }
            });
        }
    });
    changed
}

impl Dialog for CanvasSizeDialog {
    fn title(&self) -> &'static str {
        "Canvas Size"
    }

    fn confirm_label(&self) -> &'static str {
        "Resize Canvas"
    }

    fn confirm(&self) -> Option<DialogAction> {
        let spec = self.spec();
        spec.is_valid().then_some(DialogAction::ResizeCanvas(spec))
    }

    fn blocked_reason(&self) -> Option<String> {
        let spec = self.spec();
        if spec.width < 1 || spec.height < 1 {
            return Some("The canvas must be at least 1 x 1 pixel".to_string());
        }
        if spec.width > MAX_DIMENSION || spec.height > MAX_DIMENSION {
            return Some(format!("No side may exceed {MAX_DIMENSION} pixels"));
        }
        (u64::from(spec.width) * u64::from(spec.height) > MAX_PIXELS)
            .then(|| format!("{} x {} is over the pixel limit", spec.width, spec.height))
    }
}

fn round_side(value: f64) -> u32 {
    if !value.is_finite() || value < 1.0 {
        return 0;
    }
    value.round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::{frame_both_themes, Harness};

    const OLD: (u32, u32) = (100, 100);
    const GROWN: (u32, u32) = (200, 200);

    #[test]
    fn the_grid_is_in_reading_order() {
        for (index, anchor) in Anchor::ALL.iter().enumerate() {
            let row = (index / 3) as u8;
            let column = (index % 3) as u8;
            assert_eq!(Anchor::at(row, column), Some(*anchor));
            assert_eq!(anchor.row(), row);
            assert_eq!(anchor.column(), column);
        }
        assert_eq!(Anchor::at(3, 0), None);
        assert_eq!(Anchor::at(0, 3), None);
    }

    #[test]
    fn all_nine_anchors_place_a_growing_canvas_correctly() {
        let expected = [
            (Anchor::TopLeft, (0, 0)),
            (Anchor::Top, (50, 0)),
            (Anchor::TopRight, (100, 0)),
            (Anchor::Left, (0, 50)),
            (Anchor::Center, (50, 50)),
            (Anchor::Right, (100, 50)),
            (Anchor::BottomLeft, (0, 100)),
            (Anchor::Bottom, (50, 100)),
            (Anchor::BottomRight, (100, 100)),
        ];
        for (anchor, offset) in expected {
            assert_eq!(anchor.offset(OLD, GROWN), offset, "{anchor:?}");
        }
    }

    #[test]
    fn all_nine_anchors_place_a_shrinking_canvas_correctly() {
        let small = (60, 60);
        let expected = [
            (Anchor::TopLeft, (0, 0)),
            (Anchor::Top, (-20, 0)),
            (Anchor::TopRight, (-40, 0)),
            (Anchor::Left, (0, -20)),
            (Anchor::Center, (-20, -20)),
            (Anchor::Right, (-40, -20)),
            (Anchor::BottomLeft, (0, -40)),
            (Anchor::Bottom, (-20, -40)),
            (Anchor::BottomRight, (-40, -40)),
        ];
        for (anchor, offset) in expected {
            assert_eq!(anchor.offset(OLD, small), offset, "{anchor:?}");
        }
    }

    #[test]
    fn the_odd_pixel_always_belongs_to_the_trailing_edge() {
        // Grow by 1: the extra column goes on the right, so the content does
        // not move at all.
        assert_eq!(Anchor::Center.offset((100, 100), (101, 101)), (0, 0));
        // Shrink by 1 again: that same column is what goes.
        assert_eq!(Anchor::Center.offset((100, 100), (99, 99)), (0, 0));
        // Grow by 3: one column left, two right.
        assert_eq!(Anchor::Center.offset((100, 100), (103, 103)), (1, 1));
        assert_eq!(Anchor::Center.offset((100, 100), (97, 97)), (-1, -1));
    }

    #[test]
    fn nudging_the_size_up_and_back_down_leaves_the_image_where_it_was() {
        for delta in 1..=9i64 {
            let grown = (100 + delta) as u32;
            let up = Anchor::Center.offset((100, 100), (grown, grown));
            let down = Anchor::Center.offset((grown, grown), (100, 100));
            assert_eq!(
                (up.0 + down.0, up.1 + down.1),
                (0, 0),
                "growing by {delta} and shrinking back moved the image"
            );
        }
    }

    #[test]
    fn an_unchanged_size_never_moves_the_content() {
        for anchor in Anchor::ALL {
            assert_eq!(anchor.offset(OLD, OLD), (0, 0), "{anchor:?}");
        }
    }

    #[test]
    fn a_grow_on_one_axis_only_moves_that_axis() {
        assert_eq!(Anchor::Center.offset((100, 100), (300, 100)), (100, 0));
        assert_eq!(Anchor::Center.offset((100, 100), (100, 300)), (0, 100));
    }

    #[test]
    fn cropping_is_reported_only_when_something_is_lost() {
        let mut dialog = CanvasSizeDialog::new(100, 100, 72.0);
        dialog.set_width_pixels(200.0);
        dialog.set_height_pixels(200.0);
        assert!(!dialog.spec().crops(dialog.current()));
        dialog.set_width_pixels(50.0);
        assert!(dialog.spec().crops(dialog.current()));
    }

    #[test]
    fn relative_mode_adds_to_the_current_size() {
        let mut dialog = CanvasSizeDialog::new(100, 80, 72.0);
        dialog.set_relative(true);
        dialog.set_width_pixels(20.0);
        dialog.set_height_pixels(-10.0);
        assert_eq!(dialog.new_size(), (120, 70));
    }

    #[test]
    fn switching_into_relative_mode_keeps_the_resulting_canvas() {
        let mut dialog = CanvasSizeDialog::new(100, 80, 72.0);
        dialog.set_width_pixels(300.0);
        dialog.set_height_pixels(40.0);
        let before = dialog.new_size();
        dialog.set_relative(true);
        assert_eq!(dialog.new_size(), before);
        dialog.set_relative(false);
        assert_eq!(dialog.new_size(), before);
    }

    #[test]
    fn switching_units_keeps_the_resulting_canvas() {
        let mut dialog = CanvasSizeDialog::new(600, 300, 300.0);
        let before = dialog.new_size();
        for unit in Unit::ALL {
            dialog.set_unit(*unit);
            assert_eq!(dialog.new_size(), before, "changed switching to {unit:?}");
        }
    }

    #[test]
    fn percent_is_relative_to_the_current_canvas() {
        let mut dialog = CanvasSizeDialog::new(400, 200, 72.0);
        dialog.set_unit(Unit::Percent);
        dialog.set_width_pixels(200.0);
        dialog.set_height_pixels(400.0);
        assert_eq!(dialog.new_size(), (200, 400));
    }

    #[test]
    fn the_spec_carries_the_offset_the_anchor_implies() {
        let mut dialog = CanvasSizeDialog::new(100, 100, 72.0);
        dialog.set_width_pixels(200.0);
        dialog.set_height_pixels(200.0);
        dialog.set_anchor(Anchor::BottomRight);
        let spec = dialog.spec();
        assert_eq!(spec.offset, (100, 100));
        assert_eq!(spec.anchor, Anchor::BottomRight);
    }

    #[test]
    fn a_zero_canvas_blocks_confirm() {
        let mut dialog = CanvasSizeDialog::new(100, 100, 72.0);
        dialog.set_width_pixels(0.0);
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().unwrap().contains("at least 1"));
    }

    #[test]
    fn confirm_produces_a_valid_spec_and_cancel_produces_nothing() {
        let dialog = CanvasSizeDialog::new(100, 100, 72.0);
        assert!(dialog.confirm().unwrap().is_valid());
        assert_eq!(
            super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn it_draws_in_both_appearances() {
        frame_both_themes(|ctx| {
            let mut dialog = CanvasSizeDialog::new(100, 100, 72.0);
            dialog.set_width_pixels(50.0);
            assert!(dialog.show(ctx, None).is_open());
        });
    }

    #[test]
    fn an_unchanged_size_moves_no_edge_whatever_the_anchor() {
        for anchor in Anchor::ALL {
            let changes = edge_changes(anchor, OLD, OLD);
            assert!(changes.is_static(), "{anchor:?} claimed {changes:?}");
            for side in Side::ALL {
                assert_eq!(side.icon(changes.get(side)), None, "{anchor:?} {side:?}");
            }
        }
    }

    #[test]
    fn a_growing_canvas_grows_on_the_sides_away_from_the_anchor() {
        // Pinned left: room appears on the right and nowhere else.
        let expected = [
            (
                Anchor::TopLeft,
                [Change::None, Change::Grow, Change::None, Change::Grow],
            ),
            (
                Anchor::Top,
                [Change::Grow, Change::Grow, Change::None, Change::Grow],
            ),
            (
                Anchor::TopRight,
                [Change::Grow, Change::None, Change::None, Change::Grow],
            ),
            (
                Anchor::Left,
                [Change::None, Change::Grow, Change::Grow, Change::Grow],
            ),
            (
                Anchor::Center,
                [Change::Grow, Change::Grow, Change::Grow, Change::Grow],
            ),
            (
                Anchor::Right,
                [Change::Grow, Change::None, Change::Grow, Change::Grow],
            ),
            (
                Anchor::BottomLeft,
                [Change::None, Change::Grow, Change::Grow, Change::None],
            ),
            (
                Anchor::Bottom,
                [Change::Grow, Change::Grow, Change::Grow, Change::None],
            ),
            (
                Anchor::BottomRight,
                [Change::Grow, Change::None, Change::Grow, Change::None],
            ),
        ];
        for (anchor, want) in expected {
            let changes = edge_changes(anchor, OLD, GROWN);
            let got = Side::ALL.map(|s| changes.get(s));
            assert_eq!(got, want, "{anchor:?}");
        }
    }

    #[test]
    fn a_shrinking_canvas_crops_the_sides_away_from_the_anchor() {
        let small = (60, 60);
        for anchor in Anchor::ALL {
            let changes = edge_changes(anchor, OLD, small);
            let grow = Side::ALL
                .iter()
                .filter(|s| changes.get(**s) == Change::Grow)
                .count();
            assert_eq!(grow, 0, "{anchor:?} grew while shrinking: {changes:?}");
            // Whatever the anchor, something has to be cropped.
            assert!(!changes.is_static(), "{anchor:?} reported no change");
        }
        // And the sign is anchor-specific: pinned top-left, only the right and
        // bottom go.
        let changes = edge_changes(Anchor::TopLeft, OLD, small);
        assert_eq!(
            Side::ALL.map(|s| changes.get(s)),
            [Change::None, Change::Shrink, Change::None, Change::Shrink]
        );
    }

    #[test]
    fn a_growing_edge_points_out_and_a_cropped_one_points_in() {
        for side in Side::ALL {
            let out = side.icon(Change::Grow).expect("a growing edge has a glyph");
            let inward = side.icon(Change::Shrink).expect("a cropped edge too");
            assert_ne!(out, inward, "{side:?} draws the same arrow either way");
            assert_eq!(side.icon(Change::None), None, "{side:?}");
        }
        // Left growing points the same way as Right shrinking: both are "the
        // edge is travelling left".
        assert_eq!(
            Side::Left.icon(Change::Grow),
            Side::Right.icon(Change::Shrink)
        );
    }

    #[test]
    fn the_arrows_agree_with_the_offset_the_dialog_commits() {
        // The arrows are derived from `Anchor::offset`, so a change to the
        // offset arithmetic can never leave the picture behind.
        let mut dialog = CanvasSizeDialog::new(100, 100, 72.0);
        dialog.set_width_pixels(140.0);
        dialog.set_height_pixels(80.0);
        dialog.set_anchor(Anchor::TopLeft);
        let spec = dialog.spec();
        let changes = edge_changes(dialog.anchor(), dialog.current(), (spec.width, spec.height));
        assert_eq!(spec.offset, (0, 0));
        assert_eq!(changes.left, Change::None);
        assert_eq!(changes.right, Change::Grow);
        assert_eq!(changes.top, Change::None);
        assert_eq!(changes.bottom, Change::Shrink);
    }

    #[test]
    fn choosing_a_custom_fill_and_then_a_colour_changes_the_spec() {
        // The defect this pins: Canvas Size offered a "Custom" fill entry, drew
        // no swatch for it and had no picker, so Custom silently produced the
        // same opaque white as White.
        let h = Harness::new();
        let mut dialog = CanvasSizeDialog::new(100, 100, 72.0);
        dialog.set_background(BackgroundContents::Custom([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(
            dialog.spec().background.fill(),
            BackgroundContents::White.fill(),
            "the starting point is indistinguishable from White, as the bug was"
        );

        h.click_widget(ids::custom_background("canvas-size"), |ctx| {
            dialog.show(ctx, None);
        });
        assert!(dialog.color_edit().is_open(), "the swatch opened nothing");

        let chosen = super::super::color_picker::ColorValue::new([0.2, 0.4, 0.9, 1.0]);
        dialog
            .color_edit_mut()
            .picker_mut()
            .expect("the picker is up")
            .set_color(chosen);
        h.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            assert!(dialog.show(ctx, None).is_open());
        });

        let fill = dialog.spec().background.fill().expect("a custom fill");
        assert_eq!(
            super::super::color_picker::ColorValue::new(fill).to_bytes(),
            chosen.to_bytes()
        );
        assert_ne!(fill, [1.0, 1.0, 1.0, 1.0], "Custom is still White");
    }

    #[test]
    fn a_fill_that_is_not_custom_draws_no_swatch() {
        let h = Harness::new();
        let mut dialog = CanvasSizeDialog::new(100, 100, 72.0);
        dialog.set_background(BackgroundContents::Transparent);
        h.frame(Vec::new(), |ctx| {
            dialog.show(ctx, None);
        });
        assert!(!h.was_drawn(ids::custom_background("canvas-size")));
    }
}
