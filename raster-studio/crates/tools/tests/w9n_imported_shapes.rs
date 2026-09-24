//! W9-N: custom shapes a `.csh` import registers join the Custom Shape
//! tool's live Shape list, and the tool draws the one a Shape index names.

use tools::registry::{custom_shape_at, custom_shape_choices, register_custom_shape_outlines};
use tools::{OptionKind, ToolId, ToolSetting};

const BUILTIN: usize = vector::CUSTOM_SHAPE_NAMES.len();

fn index_of(name: &str) -> Option<usize> {
    custom_shape_choices().iter().position(|c| *c == name)
}

#[test]
fn a_registered_outline_is_listed_after_the_builtins_and_keeps_its_index() {
    let list = register_custom_shape_outlines([("W9N tools square", "M0 0 L1 0 L1 1 L0 1 Z")]);
    assert_eq!(&list[..BUILTIN], &vector::CUSTOM_SHAPE_NAMES[..]);
    let at = index_of("W9N tools square").expect("listed");
    assert!(at >= BUILTIN);
    // Re-registering the same name with a new outline keeps the index and
    // takes the new path.
    register_custom_shape_outlines([
        ("W9N tools other", "M0 0 L1 0 L0 1 Z"),
        ("W9N tools square", "M0 0 L2 0 L2 1 Z"),
    ]);
    assert_eq!(index_of("W9N tools square"), Some(at));
    let (name, path) = custom_shape_at(at);
    assert_eq!(name, "W9N tools square");
    assert_eq!(path.bounds().max.x, 2.0);
    // The options bar's spec is the live list.
    let OptionKind::Choice { choices, .. } = tools::registry::custom_shape_spec().kind else {
        panic!("not a choice")
    };
    assert!(choices.contains(&"W9N tools other"));
}

#[test]
fn unusable_outlines_and_builtin_names_are_not_listed() {
    let before = custom_shape_choices().len();
    register_custom_shape_outlines([
        ("W9N tools garbage", "this is not path data"),
        ("W9N tools flat", "M0 0 L1 0 Z"),
        ("W9N tools nan", "M0 0 LNaN 1 Z"),
        ("", "M0 0 L1 0 L1 1 Z"),
        ("Heart", "M0 0 L1 0 L1 1 Z"),
    ]);
    for name in ["W9N tools garbage", "W9N tools flat", "W9N tools nan"] {
        assert_eq!(index_of(name), None, "{name} was listed");
    }
    // Other tests in this binary may register concurrently, so the list may
    // grow - but never by a refused entry, and never shrink.
    assert!(custom_shape_choices().len() >= before);
    assert_eq!(
        custom_shape_choices()
            .iter()
            .filter(|c| **c == "Heart")
            .count(),
        1
    );
}

#[test]
fn a_shape_index_past_the_builtins_reaches_the_tool_and_a_huge_one_clamps() {
    register_custom_shape_outlines([("W9N tools tri", "M0.5 0 L1 1 L0 1 Z")]);
    let at = index_of("W9N tools tri").unwrap();
    let mut tool = tools::registry::make(ToolId::CustomShape);
    tool.set_setting("preset", ToolSetting::Choice(at)).unwrap();
    let (_, want) = custom_shape_at(at);
    assert_eq!(want.segments().len(), 3);
    // An index far past the end clamps to the last entry, never panics.
    let (last, _) = custom_shape_at(usize::MAX);
    assert_eq!(Some(last.as_str()), custom_shape_choices().last().copied());
    tool.set_setting("preset", ToolSetting::Choice(usize::MAX))
        .unwrap();
    // A built-in index still names the built-in.
    assert_eq!(custom_shape_at(0).0, "Heart");
}
