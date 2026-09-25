//! W13X-6: a tool letter enters its group at the member last used from it,
//! Photopea's rule, on the application's own routes — the palette pick
//! (`ui::Intent::SelectTool` → `menu_bridge::pick` → `menu_bridge::record` →
//! `ChromeOutput::select_tool` → `Editor::set_tool`, the steps `shell.rs`
//! performs for a palette or fly-out click) and the key route (the bare
//! letter's chord resolved by the editor's keymap into `Action::SelectTool`,
//! which `Shell::on_key` hands to `Editor::dispatch`).

use app_shell::action::Action;
use app_shell::editor::Editor;
use app_shell::keymap::Resolved;
use app_shell::{menu_bridge, AppPaths, Chord, ChromeOutput, Key, Preferences, RecentFiles};
use app_shell::{ScriptedDialogs, ToolKey};
use tools::ToolId;

fn editor(dir: &std::path::Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    )
}

/// A palette (or fly-out) click on `tool`, performed as the shell does.
fn palette_pick(ed: &mut Editor, tool: ToolId) {
    let pick = menu_bridge::pick(&ui::Intent::SelectTool(tool), ed)
        .unwrap_or_else(|| panic!("{tool:?}: Intent::SelectTool did not route to a Pick"));
    let mut out = ChromeOutput::default();
    menu_bridge::record(pick, &mut out);
    ed.set_tool(out.select_tool.expect("the pick selects a tool"));
    assert_eq!(ed.tool(), tool);
}

/// The bare letter `c` pressed: its chord through the editor's keymap, the
/// resolved action through `Editor::dispatch`.
fn press_letter(ed: &mut Editor, c: char) {
    let chord = Chord {
        ctrl_or_cmd: false,
        alt: false,
        shift: false,
        key: Key::Char(c),
    };
    let action = match ed.keymap().resolve_any(&chord) {
        Some(Resolved::App(action @ Action::SelectTool(_))) => action,
        other => panic!("{c}: the bare letter resolved to {other:?}"),
    };
    ed.dispatch(action).unwrap();
}

#[test]
fn a_tool_letter_enters_its_group_at_the_member_last_used_this_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let group = tools::registry::by_shortcut('m');
    assert!(group.len() > 1, "the marquee group has several tools");
    let last = group[group.len() - 1];

    // Nothing used from the group yet: M lands on its first member.
    palette_pick(&mut ed, ToolId::Brush);
    press_letter(&mut ed, 'm');
    assert_eq!(ed.tool(), group[0], "an unused group starts at its first");

    // Pick the group's last member from the fly-out, leave for the Brush by
    // the palette, press M: back on the member last used, not the first.
    palette_pick(&mut ed, last);
    palette_pick(&mut ed, ToolId::Brush);
    press_letter(&mut ed, 'm');
    assert_eq!(ed.tool(), last, "M went to {:?}, not {last:?}", ed.tool());

    // Stepped to with Shift+M, left by another letter (B), entered again.
    assert_eq!(
        ed.select_tool_letter(
            ToolKey::all()
                .into_iter()
                .find(|k| k.char() == 'm')
                .unwrap(),
            true
        ),
        Some(group[0])
    );
    press_letter(&mut ed, 'b');
    assert!(!group.contains(&ed.tool()));
    press_letter(&mut ed, 'm');
    assert_eq!(ed.tool(), group[0], "the step's member is the last used");

    // A fresh session remembers nothing.
    let mut fresh = editor(dir.path());
    fresh.set_tool(ToolId::Brush);
    press_letter(&mut fresh, 'm');
    assert_eq!(fresh.tool(), group[0]);
}
