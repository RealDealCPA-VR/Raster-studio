//! W9-N: gradients a `.grd` import registers are chips in the gradient
//! editor's preset strip, and clicking one loads that ramp.

use layer_model::{Gradient, GradientStop};
use ui::dialogs::gradient_editor::{
    imported_chip_id, imported_gradients, register_imported_gradients,
};
use ui::dialogs::{Dialog, GradientEditorDialog};

fn two_stop(a: [f32; 4], b: [f32; 4]) -> Gradient {
    Gradient {
        stops: vec![
            GradientStop {
                position: 0.0,
                color: a,
                midpoint: 0.5,
            },
            GradientStop {
                position: 1.0,
                color: b,
                midpoint: 0.5,
            },
        ],
        alpha_stops: Vec::new(),
        smoothness: 1.0,
    }
}

fn frame(ctx: &egui::Context, dialog: &mut GradientEditorDialog, events: Vec<egui::Event>) {
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 900.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            let _ = dialog.show(ctx, None);
        },
    );
}

#[test]
fn registering_appends_new_names_and_replaces_known_ones_and_skips_empty_ramps() {
    let red = two_stop([1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]);
    let blue = two_stop([0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]);
    register_imported_gradients([("W9N ui a", red.clone()), ("W9N ui b", red)]);
    register_imported_gradients([
        ("W9N ui a", blue),
        (
            "W9N ui empty",
            Gradient {
                stops: Vec::new(),
                alpha_stops: Vec::new(),
                smoothness: 1.0,
            },
        ),
    ]);
    let list = imported_gradients();
    let a: Vec<_> = list.iter().filter(|(n, _)| n == "W9N ui a").collect();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].1.stops[0].color, [0.0, 0.0, 1.0, 1.0]);
    let pa = list.iter().position(|(n, _)| n == "W9N ui a").unwrap();
    let pb = list.iter().position(|(n, _)| n == "W9N ui b").unwrap();
    assert!(pa < pb, "a known name must keep its place");
    assert!(!list.iter().any(|(n, _)| n == "W9N ui empty"));
}

#[test]
fn an_imported_chip_is_drawn_and_clicking_it_loads_that_ramp() {
    let green = two_stop([0.0, 1.0, 0.0, 1.0], [1.0, 0.0, 1.0, 1.0]);
    register_imported_gradients([("W9N ui green", green)]);
    let index = imported_gradients()
        .iter()
        .position(|(n, _)| n == "W9N ui green")
        .unwrap();

    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let mut dialog = GradientEditorDialog::default();
    for _ in 0..6 {
        frame(&ctx, &mut dialog, Vec::new());
    }
    let at = ctx
        .read_response(imported_chip_id(index))
        .expect("the imported chip was not drawn")
        .rect
        .center();
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    frame(
        &ctx,
        &mut dialog,
        vec![egui::Event::PointerMoved(at), button(true), button(false)],
    );
    let g = dialog.gradient();
    assert_eq!(g.stops[0].color, [0.0, 1.0, 0.0, 1.0], "{g:?}");
    assert_eq!(g.stops[1].color, [1.0, 0.0, 1.0, 1.0], "{g:?}");
    // Repaired the way any ramp the editor opens on is: an opacity ramp.
    assert!(g.alpha_stops.len() >= 2);
    assert!(dialog.confirm().is_some());
    // An index with no imported gradient is refused and changes nothing.
    assert!(!dialog.apply_imported(usize::MAX));
    assert_eq!(dialog.gradient().stops[0].color, [0.0, 1.0, 0.0, 1.0]);
}
