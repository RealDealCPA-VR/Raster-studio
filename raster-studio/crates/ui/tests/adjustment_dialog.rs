//! The Image ▸ Adjustments dialog, driven headlessly from outside the crate.
//!
//! The unit tests in `dialogs::adjustment_dialog` prove the state; these prove
//! the *drawn* dialog: every one of the fifteen lays out in both appearances
//! without panicking, the preview and the Levels histogram occupy real
//! rectangles, and Enter/Escape reach the dialog through the same `show` the
//! shell calls — so a dialog that computes correctly but cannot be drawn or
//! cannot be confirmed fails here.

use editor_core::Command;
use layer_model::{AdjustmentKind, AdjustmentLayer, LayerKind};
use ui::dialogs::ids;
use ui::dialogs::{AdjustmentDialog, DialogAction, DialogOutcome};
use ui::menu::AdjustmentId;

const SCREEN: egui::Vec2 = egui::vec2(1600.0, 1000.0);

fn context(theme: design::Theme) -> egui::Context {
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, theme);
    ctx
}

fn frame(ctx: &egui::Context, events: Vec<egui::Event>, f: impl FnOnce(&egui::Context)) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
        events,
        ..Default::default()
    };
    let mut f = Some(f);
    let _ = ctx.run(input, |ctx| {
        if let Some(f) = f.take() {
            f(ctx);
        }
    });
}

fn key(key: egui::Key) -> Vec<egui::Event> {
    vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }]
}

/// Lay the dialog out until the widget under `id` holds still, and return
/// where it landed.
fn settle(ctx: &egui::Context, dialog: &mut AdjustmentDialog, id: egui::Id) -> egui::Rect {
    let mut previous = None;
    let mut repeats = 0;
    for _ in 0..24 {
        frame(ctx, Vec::new(), |ctx| {
            assert!(
                dialog.show(ctx, None).is_open(),
                "{:?} closed itself",
                dialog.id()
            );
        });
        let rect = ctx.read_response(id).map(|r| r.rect);
        match rect {
            Some(rect) if Some(rect) == previous => {
                repeats += 1;
                if repeats >= 4 {
                    return rect;
                }
            }
            _ => repeats = 0,
        }
        previous = rect;
    }
    panic!("{id:?} never settled for {:?}", dialog.id());
}

#[test]
fn every_adjustment_dialog_draws_a_preview_in_both_themes() {
    for theme in design::Theme::ALL {
        for id in AdjustmentId::ALL {
            let ctx = context(*theme);
            let mut dialog = AdjustmentDialog::with_placeholder(*id);
            let preview = settle(&ctx, &mut dialog, ids::adjustment_preview());
            assert!(
                preview.width() > 0.0 && preview.height() > 0.0,
                "{id:?} in {theme:?}: the preview has no area"
            );
            // Preview off: the well is still there, so the layout does not
            // jump, and the adjustment is no longer run per frame.
            dialog.set_preview_enabled(false);
            let well = settle(&ctx, &mut dialog, ids::adjustment_preview());
            assert!(well.width() > 0.0 && well.height() > 0.0);
        }
    }
}

#[test]
fn only_levels_draws_a_histogram() {
    let ctx = context(design::Theme::Dark);
    let mut levels = AdjustmentDialog::with_placeholder(AdjustmentId::Levels);
    let histogram = settle(&ctx, &mut levels, ids::adjustment_histogram());
    let preview = ctx.read_response(ids::adjustment_preview()).unwrap().rect;
    assert!(histogram.width() > 0.0 && histogram.height() > 0.0);
    assert!(
        histogram.top() >= preview.bottom(),
        "the histogram ({histogram:?}) is not below the preview ({preview:?})"
    );
    assert!(
        histogram.width() >= preview.width(),
        "the histogram ({histogram:?}) is narrower than the preview it describes ({preview:?})"
    );
    for id in AdjustmentId::ALL
        .iter()
        .filter(|id| **id != AdjustmentId::Levels)
    {
        let ctx = context(design::Theme::Dark);
        let mut dialog = AdjustmentDialog::with_placeholder(*id);
        settle(&ctx, &mut dialog, ids::adjustment_preview());
        assert!(
            ctx.read_response(ids::adjustment_histogram()).is_none(),
            "{id:?} drew a histogram"
        );
    }
}

#[test]
fn enter_confirms_a_moved_adjustment_through_the_drawn_dialog() {
    let ctx = context(design::Theme::Dark);
    let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::BrightnessContrast);
    assert!(dialog.set_kind(AdjustmentKind::BrightnessContrast {
        brightness: 0.5,
        contrast: 0.0,
    }));
    settle(&ctx, &mut dialog, ids::adjustment_preview());
    let mut outcome = DialogOutcome::Open;
    frame(&ctx, key(egui::Key::Enter), |ctx| {
        outcome = dialog.show(ctx, None)
    });
    let DialogOutcome::Confirmed(DialogAction::Command(command)) = outcome else {
        panic!("Enter produced {outcome:?}");
    };
    let Command::CreateLayer { layer } = *command else {
        panic!("the dialog's own action is not the adjustment as a layer");
    };
    assert_eq!(
        layer.kind,
        LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::BrightnessContrast {
                brightness: 0.5,
                contrast: 0.0,
            }
        })
    );
    assert_eq!(dialog.invocation().id, AdjustmentId::BrightnessContrast);
}

#[test]
fn escape_cancels_and_an_untouched_adjustment_will_not_confirm() {
    let ctx = context(design::Theme::Dark);
    let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::Threshold);
    settle(&ctx, &mut dialog, ids::adjustment_preview());
    let mut outcome = DialogOutcome::Open;
    frame(&ctx, key(egui::Key::Escape), |ctx| {
        outcome = dialog.show(ctx, None)
    });
    assert_eq!(outcome, DialogOutcome::Cancelled);

    // Ten adjustments open at the identity; Enter leaves them open.
    for id in AdjustmentId::ALL {
        let ctx = context(design::Theme::Dark);
        let mut dialog = AdjustmentDialog::with_placeholder(*id);
        if !dialog.invocation().is_identity() {
            continue;
        }
        settle(&ctx, &mut dialog, ids::adjustment_preview());
        let mut outcome = DialogOutcome::Open;
        frame(&ctx, key(egui::Key::Enter), |ctx| {
            outcome = dialog.show(ctx, None)
        });
        assert!(
            outcome.is_open(),
            "{id:?} confirmed at its identity: {outcome:?}"
        );
    }
}

#[test]
fn threshold_at_a_fifth_previews_differently_from_four_fifths() {
    let mut low = AdjustmentDialog::with_placeholder(AdjustmentId::Threshold);
    assert!(low.set_kind(AdjustmentKind::Threshold { level: 0.2 }));
    let mut high = AdjustmentDialog::with_placeholder(AdjustmentId::Threshold);
    assert!(high.set_kind(AdjustmentKind::Threshold { level: 0.8 }));
    let (low, high) = (
        low.preview_buffer().to_rgba8(),
        high.preview_buffer().to_rgba8(),
    );
    assert_ne!(low, high);
    for px in low.as_chunks::<4>().0.iter().chain(high.as_chunks::<4>().0) {
        assert!(
            px[..3] == [0, 0, 0] || px[..3] == [255, 255, 255],
            "a threshold left {px:?}"
        );
    }
}

#[test]
fn the_photo_filter_swatch_is_drawn_and_opens_the_nested_picker() {
    // A swatch that is painted but reads nobody's Response is the failure
    // mode `ids` exists to catch: click the real rectangle, and the nested
    // picker must be open on the next frame.
    let ctx = context(design::Theme::Dark);
    let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::PhotoFilter);
    let swatch = settle(
        &ctx,
        &mut dialog,
        ids::adjustment_color(ui::dialogs::adjustment_dialog::ColorTarget::PhotoFilter),
    );
    let at = swatch.center();
    frame(
        &ctx,
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
        ],
        |ctx| {
            let _ = dialog.show(ctx, None);
        },
    );
    assert!(
        dialog.color_edit().is_open(),
        "clicking the Photo Filter swatch opened no picker"
    );
    // While the picker is up, Escape belongs to it, not to the dialog.
    let mut outcome = DialogOutcome::Cancelled;
    frame(&ctx, key(egui::Key::Escape), |ctx| {
        outcome = dialog.show(ctx, None)
    });
    assert!(
        outcome.is_open(),
        "Escape closed the host instead of the picker"
    );
}
