//! The shared layout grammar every dialog in this module obeys.
//!
//! One title, the content, then a right-aligned action row with the primary
//! action **last**. Escape cancels, Enter confirms, and nothing here paints a
//! literal colour, radius or gap — it all comes from `design`.
//!
//! The interesting part is [`resolve`]: the decision "what does this keystroke
//! do to this dialog" is a pure function of the dialog's state and one frame's
//! keys, so the confirm/cancel contract of all thirteen dialogs is asserted in
//! a loop rather than clicked through by hand.

use std::hash::Hash;

use design::{
    color32, current_theme, current_tokens,
    egui_theme::rounding,
    primary_button, secondary_button,
    tokens::palette::ColorRole,
    tokens::{Elevation, Radius, Space, TextRole, TypeRole},
};
use egui::{Align, Context, Key, Layout, Ui};

use super::action::DialogAction;

/// The keys the dialog grammar reserves, as seen in one frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DialogKeys {
    /// Enter (or the numeric keypad's Enter) went down this frame.
    pub confirm: bool,
    /// Escape went down this frame.
    pub cancel: bool,
}

impl DialogKeys {
    /// Nothing pressed.
    pub const NONE: Self = Self {
        confirm: false,
        cancel: false,
    };
    /// Enter.
    pub const CONFIRM: Self = Self {
        confirm: true,
        cancel: false,
    };
    /// Escape.
    pub const CANCEL: Self = Self {
        confirm: false,
        cancel: true,
    };

    /// Read Enter/Escape out of one frame of egui input.
    pub fn read(ctx: &Context) -> Self {
        ctx.input(|i| Self {
            confirm: i.key_pressed(Key::Enter),
            cancel: i.key_pressed(Key::Escape),
        })
    }
}

/// What one frame did to a dialog.
#[derive(Clone, PartialEq, Debug)]
pub enum DialogOutcome<T> {
    /// Still open; nothing was decided.
    Open,
    /// The user backed out. **Never** carries a payload — that is the whole
    /// point of the type.
    Cancelled,
    /// The user committed, and this is what they committed to.
    Confirmed(T),
}

impl<T> DialogOutcome<T> {
    /// The payload, if the dialog was confirmed.
    pub fn confirmed(self) -> Option<T> {
        match self {
            Self::Confirmed(value) => Some(value),
            _ => None,
        }
    }

    /// Whether the dialog should stay on screen.
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }
}

/// The contract every dialog in this module satisfies.
///
/// Implementing it is what puts a dialog into the registry that
/// `every_dialog_confirms_to_an_action_and_cancels_to_nothing` iterates, so a
/// new dialog cannot quietly skip the confirm/cancel guarantee.
pub trait Dialog {
    /// The dialog's title, as shown in its header.
    fn title(&self) -> &'static str;

    /// Label of the primary action. "OK" unless the dialog can say something
    /// more specific, which is nearly always.
    fn confirm_label(&self) -> &'static str {
        "OK"
    }

    /// The action confirming right now would produce, or `None` when the
    /// current state is not valid to commit.
    ///
    /// Pure: calling it must not change the dialog.
    fn confirm(&self) -> Option<DialogAction>;

    /// Why the primary action is unavailable, in words a user can act on.
    ///
    /// `Some` exactly when [`Dialog::confirm`] is `None` — a disabled button
    /// with no reason is the bug this pairing exists to prevent.
    fn blocked_reason(&self) -> Option<String> {
        None
    }
}

/// Apply one frame's keys to a dialog.
///
/// Escape wins over Enter: if both arrive in the same frame the user is backing
/// out, and committing an edit they were trying to abandon is the worse
/// failure. A confirm that the dialog refuses (invalid state) leaves it
/// [`DialogOutcome::Open`] rather than closing silently.
pub fn resolve(dialog: &dyn Dialog, keys: DialogKeys) -> DialogOutcome<DialogAction> {
    if keys.cancel {
        return DialogOutcome::Cancelled;
    }
    if keys.confirm {
        if let Some(action) = dialog.confirm() {
            return DialogOutcome::Confirmed(action);
        }
    }
    DialogOutcome::Open
}

/// Which button in an action row was pressed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogButton {
    Cancel,
    Confirm,
    /// An extra secondary action, by its index in the row.
    Extra(usize),
}

/// The right-aligned action row: extras, then Cancel, then the primary.
///
/// Drawn right-to-left so the primary lands hard against the trailing edge and
/// every dialog puts it in the same place. A blocked primary is rendered
/// disabled and carries `blocked` as its tooltip, because a button that looks
/// live and does nothing is worse than one that says why it cannot.
pub fn action_row(
    ui: &mut Ui,
    confirm_label: &str,
    blocked: Option<&str>,
    extras: &[&str],
) -> Option<DialogButton> {
    let extras: Vec<(&str, Option<&str>)> = extras.iter().map(|label| (*label, None)).collect();
    action_row_with_extras(ui, confirm_label, blocked, &extras)
}

/// The action row, with a reason an *extra* is unavailable.
///
/// The same rule the primary has always had, extended to the secondary
/// actions: the colour picker's Eyedropper cannot work without a screen
/// sampler, and a button that arms a mode nothing can complete is the exact
/// failure this pairing exists to prevent. A blocked extra is drawn disabled,
/// explains itself on hover, and never reports a press.
pub fn action_row_with_extras(
    ui: &mut Ui,
    confirm_label: &str,
    blocked: Option<&str>,
    extras: &[(&str, Option<&str>)],
) -> Option<DialogButton> {
    let mut pressed = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = Space::Small.pt();
        let enabled = blocked.is_none();
        let response = ui
            .add_enabled_ui(enabled, |ui| primary_button(ui, confirm_label))
            .inner;
        if let Some(reason) = blocked {
            response.on_disabled_hover_text(reason);
        } else if response.clicked() {
            pressed = Some(DialogButton::Confirm);
        }
        if secondary_button(ui, "Cancel").clicked() {
            pressed = Some(DialogButton::Cancel);
        }
        for (index, (label, extra_blocked)) in extras.iter().enumerate() {
            let response = ui
                .add_enabled_ui(extra_blocked.is_none(), |ui| secondary_button(ui, label))
                .inner;
            match extra_blocked {
                Some(reason) => {
                    response.on_disabled_hover_text(*reason);
                }
                None => {
                    if response.clicked() {
                        pressed = Some(DialogButton::Extra(index));
                    }
                }
            }
        }
    });
    pressed
}

/// The dialog header: the title, and an optional one-line description under it.
pub fn title_block(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    let t = current_tokens(ui);
    ui.label(
        egui::RichText::new(title)
            .color(color32(t.palette.text(TextRole::Primary)))
            .font(design::egui_theme::font_id(t, TypeRole::Title)),
    );
    if let Some(subtitle) = subtitle {
        ui.add_space(Space::Hair.pt());
        ui.label(
            egui::RichText::new(subtitle)
                .color(color32(t.palette.text(TextRole::Secondary)))
                .font(design::egui_theme::font_id(t, TypeRole::Footnote)),
        );
    }
    ui.add_space(Space::Medium.pt());
}

/// A full-width hairline separator. Sections are divided by a rule and space,
/// never by a box.
pub fn hairline(ui: &mut Ui) {
    let t = current_tokens(ui);
    ui.add_space(Space::Medium.pt());
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), t.borders.hairline),
        egui::Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        ui.painter().hline(
            rect.x_range(),
            rect.center().y,
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::SeparatorHairline)),
            ),
        );
    }
    ui.add_space(Space::Medium.pt());
}

/// Quiet secondary text — a computed size, a hint, a unit suffix.
pub fn caption(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    let t = current_tokens(ui);
    ui.label(
        egui::RichText::new(text.into())
            .color(color32(t.palette.text(TextRole::Secondary)))
            .font(design::egui_theme::font_id(t, TypeRole::Footnote)),
    )
}

/// Text that has to be *noticed* — a validation failure.
pub fn warning(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    let t = current_tokens(ui);
    ui.label(
        egui::RichText::new(text.into())
            .color(color32(t.palette.color(ColorRole::Warning)))
            .font(design::egui_theme::font_id(t, TypeRole::Footnote)),
    )
}

/// The dim wash behind a modal — and the thing that makes it *modal*.
///
/// It is a full-screen interactive `Area`, not a painter. A painter senses
/// nothing, so a wash drawn with one dims the app while leaving every control
/// under it clickable: the user could paint on the canvas or switch tools with
/// a dialog up. That is a correctness problem and not only a polish one,
/// because dialogs snapshot the document when they open —
/// [`ImageSizeDialog::new`](super::ImageSizeDialog::new) stores the old size
/// for its aspect ratio and [`CanvasSizeDialog`](super::CanvasSizeDialog)
/// stores it for the anchor offsets — so an edit made behind the dialog leaves
/// it computing against a document that no longer exists.
///
/// The area sits in [`egui::Order::PanelResizeLine`]: above the background
/// layer every panel draws into, below the [`egui::Order::Middle`] layer the
/// dialog window itself uses, and below the foreground layer combo popups use.
/// It swallows the click rather than treating it as Cancel — an accidental
/// click outside a half-filled dialog should not throw the work away.
/// How far the scrim dims a dark app, out of 255.
///
/// Heavier than the light value on purpose: over a dark UI a thin wash is
/// indistinguishable from the app's own surfaces, and the scrim has to *read*
/// as "everything behind this is out of reach" or it is only a shadow.
///
/// This is an opacity, which is a design decision, and it should be a role in
/// `design`'s palette — a `ColorRole::Scrim` per appearance, so `scrim` reads
/// one token and does no alpha arithmetic at all. It is not one today, and this
/// task's scope is `crates/ui`, so the two values are named, documented and
/// pinned by a test here rather than written inline where nothing can find them.
const SCRIM_ALPHA_DARK: u8 = 150; // design-exempt: no ColorRole::Scrim exists yet; owning it means editing crates/design

/// The same, over a light app, where less wash reads as more.
const SCRIM_ALPHA_LIGHT: u8 = 90; // design-exempt: no ColorRole::Scrim exists yet; owning it means editing crates/design

/// The scrim's opacity for one appearance.
pub const fn scrim_alpha(is_dark: bool) -> u8 {
    if is_dark {
        SCRIM_ALPHA_DARK
    } else {
        SCRIM_ALPHA_LIGHT
    }
}

pub fn scrim(ctx: &Context) -> egui::Response {
    let palette = current_theme(ctx).palette();
    let mut shade = palette.color(ColorRole::ShadowColor);
    shade.a = scrim_alpha(palette.is_dark());
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("raster-studio-dialog-scrim"))
        .order(egui::Order::PanelResizeLine)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ctx, |ui| {
            let response = ui.allocate_response(screen.size(), egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(screen, egui::Rounding::ZERO, color32(shade));
            response
        })
        .inner
}

/// The widths a dialog surface is allowed to have.
///
/// A fixed ladder rather than a number per dialog, for two reasons: a set of
/// dialogs each a few points wider than the last reads as sloppy even when
/// nobody can say why, and every rung here is a whole number of grid units, so
/// no dialog can drift off the 4pt grid by being sized to fit its content.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum DialogWidth {
    /// One column of fields. Canvas Size.
    Narrow,
    /// One column with a preview or a wide field. Image Size, Color Picker.
    Standard,
    /// One column plus a full-width strip. Gradient Editor.
    Medium,
    /// A list beside a column of fields. New Document.
    Wide,
    /// Two working columns. Export As, Layer Style.
    Split,
    /// A sidebar beside a full pane. Preferences.
    Broad,
}

impl DialogWidth {
    /// Every rung, narrowest first.
    pub const ALL: &'static [DialogWidth] = &[
        Self::Narrow,
        Self::Standard,
        Self::Medium,
        Self::Wide,
        Self::Split,
        Self::Broad,
    ];

    /// Grid units.
    pub const fn units(self) -> f32 {
        match self {
            Self::Narrow => 105.0,
            Self::Standard => 115.0,
            Self::Medium => 130.0,
            Self::Wide => 140.0,
            Self::Split => 155.0,
            Self::Broad => 165.0,
        }
    }

    /// Width in points.
    pub fn pt(self) -> f32 {
        design::tokens::grid(self.units())
    }
}

/// How one modal surface differs from the default.
///
/// Two knobs, and both exist for the nested colour picker: a surface opened
/// *from* a dialog must not land exactly on top of the dialog that opened it,
/// and must not paint a second scrim, because the first one is already there
/// and doubling it dims the whole app twice.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ModalStyle {
    /// Width rung of the surface.
    pub width: DialogWidth,
    /// Nudge from the centre of the screen, in points.
    pub offset: egui::Vec2,
    /// Whether to dim and block everything behind.
    pub scrim: bool,
}

impl ModalStyle {
    /// A top-level dialog: centred, with a scrim.
    pub fn centered(width: DialogWidth) -> Self {
        Self {
            width,
            offset: egui::Vec2::ZERO,
            scrim: true,
        }
    }

    /// A surface opened from another dialog: offset down and to the right so
    /// both are visible, and no second scrim.
    pub fn nested(width: DialogWidth) -> Self {
        Self {
            width,
            offset: egui::vec2(Space::XXLarge.pt(), Space::XXLarge.pt()),
            scrim: false,
        }
    }
}

/// Draw one modal: scrim, centred floating surface, title, content.
///
/// The action row is the caller's job — it belongs to the content because only
/// the dialog knows whether its primary action is available.
pub fn modal<R>(
    ctx: &Context,
    id_salt: impl Hash,
    title: &str,
    subtitle: Option<&str>,
    width: DialogWidth,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    modal_with(
        ctx,
        id_salt,
        title,
        subtitle,
        ModalStyle::centered(width),
        add_contents,
    )
}

/// [`modal`], with control over the offset and the scrim.
pub fn modal_with<R>(
    ctx: &Context,
    id_salt: impl Hash,
    title: &str,
    subtitle: Option<&str>,
    style: ModalStyle,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    if style.scrim {
        let _ = scrim(ctx);
    }
    let width = style.width.pt();
    let tokens = current_theme(ctx).tokens();
    let radius = Radius::Large.resolve(&tokens.radii, width);
    let frame = egui::Frame::none()
        .fill(color32(tokens.palette.color(ColorRole::SurfaceOverlay)))
        .rounding(rounding(radius))
        .shadow(design::egui_theme::shadow(
            &tokens.palette,
            Elevation::Modal,
        ))
        .inner_margin(egui::Margin::same(Space::XLarge.pt()));

    egui::Window::new(title)
        .id(egui::Id::new(("raster-studio-dialog", id_salt)))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, style.offset)
        .frame(frame)
        .fixed_size(egui::vec2(width, 0.0))
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = Space::Small.pt();
            title_block(ui, title, subtitle);
            let value = add_contents(ui);
            ui.add_space(Space::Large.pt());
            value
        })
        .and_then(|r| r.inner)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Run one headless egui frame over `f`.
    ///
    /// Dialogs are drawn, not just computed, in the tests that use this: a
    /// dialog that panics while painting is a shipped crash, and a unit test on
    /// its state struct alone would never see it.
    pub fn frame<R>(f: impl FnOnce(&Context) -> R) -> R {
        let ctx = Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut f = Some(f);
        let mut out = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            if let Some(f) = f.take() {
                out = Some(f(ctx));
            }
        });
        out.expect("the frame closure always runs")
    }

    /// Run one headless frame in both appearances, so a dialog that only reads
    /// correctly in dark is caught here.
    pub fn frame_both_themes(mut f: impl FnMut(&Context)) {
        for theme in design::Theme::ALL {
            let ctx = Context::default();
            design::apply_theme(&ctx, *theme);
            let _ = ctx.run(egui::RawInput::default(), |ctx| f(ctx));
        }
    }

    /// A persistent context a dialog can be driven across several frames.
    ///
    /// The model tests prove what a control *should* do. This proves the drawn
    /// control is wired to it: lay the dialog out once, read the real rectangle
    /// of a widget back by its id, then press and release inside it. A swatch
    /// that is drawn but reads nobody's `Response` fails here and passes every
    /// state test there is, which is exactly how eight dead swatches shipped.
    pub struct Harness {
        pub ctx: Context,
    }

    impl Harness {
        /// Screen size the harness lays out in. Large enough that a Split-width
        /// dialog and a nested picker both fit without clipping.
        pub const SCREEN: egui::Vec2 = egui::vec2(1600.0, 1000.0);

        /// How many consecutive frames a rectangle must repeat before
        /// [`Harness::settle`] believes the layout has converged.
        pub const STABLE_FRAMES: usize = 4;

        /// A dark-themed context sized to [`Harness::SCREEN`].
        pub fn new() -> Self {
            let ctx = Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            Self { ctx }
        }

        /// Run one frame with `events`.
        pub fn frame(&self, events: Vec<egui::Event>, f: impl FnOnce(&Context)) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Self::SCREEN)),
                events,
                ..Default::default()
            };
            let mut f = Some(f);
            let _ = self.ctx.run(input, |ctx| {
                if let Some(f) = f.take() {
                    f(ctx);
                }
            });
        }

        /// Whether a widget was drawn at all on the last frame.
        pub fn was_drawn(&self, id: egui::Id) -> bool {
            self.ctx.read_response(id).is_some()
        }

        /// A move, a press and a release at `at`.
        pub fn click_events(at: egui::Pos2) -> Vec<egui::Event> {
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ]
        }

        /// A press with no release, for the frame an eyedropper samples on.
        pub fn press_events(at: egui::Pos2) -> Vec<egui::Event> {
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
            ]
        }

        /// A key press and release.
        pub fn key_events(key: Key) -> Vec<egui::Event> {
            vec![
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                },
            ]
        }

        /// Lay out until the widget with `id` stops moving, and return where it
        /// ended up.
        ///
        /// One frame is not enough. Every dialog is an `egui::Area`, and an
        /// area does not know its own size until it has been laid out once — a
        /// centred window is still finding its position two frames in, and its
        /// contents move with it. A rectangle read from the first frame points
        /// at empty space by the time the click arrives, which looks exactly
        /// like a control that is wired to nothing.
        ///
        /// Two matching frames are not enough either: a centred window's first
        /// two passes can agree by coincidence and then jump once the content
        /// below has been measured, so the rectangle has to hold still for
        /// [`Harness::STABLE_FRAMES`] frames running before it is believed.
        pub fn settle(&self, id: egui::Id, mut draw: impl FnMut(&Context)) -> egui::Rect {
            let mut previous = None;
            let mut repeats = 0usize;
            for _ in 0..24 {
                self.frame(Vec::new(), &mut draw);
                let rect = self.ctx.read_response(id).map(|r| r.rect);
                match rect {
                    Some(rect) if Some(rect) == previous => {
                        repeats += 1;
                        if repeats >= Self::STABLE_FRAMES {
                            return rect;
                        }
                    }
                    _ => repeats = 0,
                }
                previous = rect;
            }
            panic!("{id:?} never settled to a stable rectangle")
        }

        /// Lay out until it settles, then click the widget with `id`. `draw`
        /// runs on every frame, because the press has to land inside the
        /// rectangle the widget actually occupies.
        pub fn click_widget(&self, id: egui::Id, mut draw: impl FnMut(&Context)) {
            let at = self.settle(id, &mut draw).center();
            self.frame(Self::click_events(at), &mut draw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::new_document::NewDocumentDialog;

    #[test]
    fn escape_cancels_and_carries_nothing() {
        let dialog = NewDocumentDialog::default();
        assert_eq!(
            resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn enter_confirms() {
        let dialog = NewDocumentDialog::default();
        let outcome = resolve(&dialog, DialogKeys::CONFIRM);
        assert!(outcome.confirmed().is_some());
    }

    #[test]
    fn escape_beats_enter_in_the_same_frame() {
        let dialog = NewDocumentDialog::default();
        let both = DialogKeys {
            confirm: true,
            cancel: true,
        };
        assert_eq!(resolve(&dialog, both), DialogOutcome::Cancelled);
    }

    #[test]
    fn no_keys_leaves_the_dialog_open() {
        let dialog = NewDocumentDialog::default();
        assert!(resolve(&dialog, DialogKeys::NONE).is_open());
    }

    #[test]
    fn a_blocked_dialog_ignores_enter_rather_than_closing() {
        let mut dialog = NewDocumentDialog::default();
        dialog.set_pixel_width(0.0);
        assert!(dialog.confirm().is_none());
        assert!(resolve(&dialog, DialogKeys::CONFIRM).is_open());
    }

    #[test]
    fn every_dialog_width_is_a_whole_number_of_grid_units_and_they_ascend() {
        for width in DialogWidth::ALL {
            let pt = width.pt();
            assert_eq!(
                pt % design::tokens::UNIT_PT,
                0.0,
                "{width:?} is {pt}pt, off the grid"
            );
        }
        for pair in DialogWidth::ALL.windows(2) {
            assert!(
                pair[1].pt() > pair[0].pt(),
                "{:?} is not wider than {:?}",
                pair[1],
                pair[0]
            );
        }
    }

    #[test]
    fn the_modal_shell_draws_in_both_appearances() {
        test_support::frame_both_themes(|ctx| {
            let out = modal(
                ctx,
                "test",
                "Title",
                Some("subtitle"),
                DialogWidth::Narrow,
                |ui| {
                    caption(ui, "content");
                    warning(ui, "careful");
                    hairline(ui);
                    action_row(ui, "OK", None, &["Reset"])
                },
            );
            assert_eq!(out, Some(None));
        });
    }

    #[test]
    fn a_modal_blocks_the_app_behind_it() {
        // The defect this pins: a scrim painted with `ctx.layer_painter` dims
        // the app but senses nothing, so every control behind a dialog stayed
        // clickable. The widget below sits at a fixed position well clear of
        // the centred dialog surface, so only the scrim can be swallowing it.
        let h = test_support::Harness::new();
        let button_id = egui::Id::new("behind-the-dialog");
        let clicked = std::cell::Cell::new(false);

        let draw = |ctx: &Context, with_dialog: bool| {
            egui::Area::new(egui::Id::new("panel-behind"))
                .order(egui::Order::Background)
                .fixed_pos(egui::pos2(40.0, 40.0))
                .show(ctx, |ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(120.0, 40.0), egui::Sense::hover());
                    if ui.interact(rect, button_id, egui::Sense::click()).clicked() {
                        clicked.set(true);
                    }
                });
            if with_dialog {
                modal(ctx, "blocker", "Title", None, DialogWidth::Narrow, |ui| {
                    caption(ui, "content");
                });
            }
        };

        // Without the dialog the widget really is clickable, so the assertion
        // below is about the scrim and not about a mislaid rectangle.
        let at = h.settle(button_id, |ctx| draw(ctx, false)).center();
        h.frame(test_support::Harness::click_events(at), |ctx| {
            draw(ctx, false)
        });
        assert!(
            clicked.get(),
            "the widget behind was never clickable at all"
        );

        clicked.set(false);
        let with_dialog = h.settle(button_id, |ctx| draw(ctx, true));
        assert_eq!(
            with_dialog.center(),
            at,
            "the dialog moved the widget behind it"
        );
        h.frame(test_support::Harness::click_events(at), |ctx| {
            draw(ctx, true)
        });
        assert!(
            !clicked.get(),
            "a control behind the modal reported a click through the scrim"
        );
    }

    #[test]
    fn the_scrim_dims_without_blacking_out_and_leans_on_dark() {
        // A scrim at full alpha hides the document the dialog is about; one too
        // thin does not read as modal at all. Both appearances stay between a
        // quarter and three quarters, and dark carries more wash than light.
        for alpha in [scrim_alpha(true), scrim_alpha(false)] {
            assert!((64..=192).contains(&alpha), "{alpha} is not a scrim");
        }
        assert!(scrim_alpha(true) > scrim_alpha(false));
    }

    #[test]
    fn a_nested_surface_is_offset_and_paints_no_second_scrim() {
        let nested = ModalStyle::nested(DialogWidth::Standard);
        assert!(!nested.scrim);
        assert_ne!(nested.offset, egui::Vec2::ZERO);
        let top = ModalStyle::centered(DialogWidth::Standard);
        assert!(top.scrim);
        assert_eq!(top.offset, egui::Vec2::ZERO);
    }

    #[test]
    fn a_blocked_extra_draws_its_reason_without_reporting_a_press() {
        test_support::frame(|ctx| {
            let out = modal(ctx, "extra", "Title", None, DialogWidth::Narrow, |ui| {
                action_row_with_extras(
                    ui,
                    "OK",
                    None,
                    &[("Eyedropper", Some("no screen sampler here"))],
                )
            });
            assert_eq!(out, Some(None));
        });
    }

    #[test]
    fn a_blocked_action_row_draws_its_reason_without_reporting_a_press() {
        test_support::frame(|ctx| {
            let out = modal(ctx, "blocked", "Title", None, DialogWidth::Narrow, |ui| {
                action_row(ui, "OK", Some("Width must be at least 1 pixel"), &[])
            });
            assert_eq!(out, Some(None));
        });
    }
}
