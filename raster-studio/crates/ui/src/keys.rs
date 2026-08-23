//! Turning a keypress into the same thing the menu would do.
//!
//! A shortcut hint painted beside a menu item is a promise. This module is what
//! keeps it: [`resolve_key`] looks the chord up in the *same* menu bar the UI
//! draws and resolves it against the *same* [`MenuContext`], so a shortcut can
//! never do something its menu item does not, and can never work while its menu
//! item is disabled.
//!
//! Tool letters are handled after menu chords and only when no modifier is
//! held, so `Ctrl+V` is Paste and a bare `V` is the Move tool. Both halves are
//! pure functions of a key, the modifiers and the context — no egui context is
//! needed to test either.

use crate::menu::{action_for_shortcut, MenuContext, Resolution};
use crate::panels::channels::{ChannelKind, ChannelsState};
use crate::shortcut::{Key, Shortcut};
use tools::ToolId;

/// What a chord does, given the state of the world.
///
/// `None` when no menu item claims the chord — the caller then tries the tool
/// letters. `Some(Resolution::Disabled)` when an item claims it but cannot run:
/// the key is *consumed* in that case, deliberately, so `Ctrl+Z` with an empty
/// history does nothing rather than falling through to something else.
pub fn resolve_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
    context: &MenuContext,
    recent_files: usize,
) -> Option<Resolution> {
    let named = Key::from_egui(key)?;
    let chord = Shortcut {
        ctrl: modifiers.command,
        alt: modifiers.alt,
        shift: modifiers.shift,
        key: named,
    };
    let action = action_for_shortcut(chord, recent_files)?;
    Some(action.resolve(context))
}

/// The channel `Ctrl+<digit>` isolates, given the document the Channels panel
/// is listing.
///
/// Looked up in [`ChannelsState::kind_for_digit`], which reads the same rows
/// the panel draws — so the `Ctrl+3` hint painted beside the red channel and
/// the chord that acts on it are one decision, not two. `None` for any chord
/// carrying another modifier, and for a digit no row wears.
pub fn channel_for_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
    doc: &editor_core::Document,
    channels: &ChannelsState,
) -> Option<ChannelKind> {
    if !modifiers.command || modifiers.alt || modifiers.shift {
        return None;
    }
    let Key::Char(c) = Key::from_egui(key)? else {
        return None;
    };
    let digit = u8::try_from(c.to_digit(10)?).ok()?;
    channels.kind_for_digit(doc, digit)
}

/// The tool a bare letter selects, cycling within its group.
///
/// `None` for any chord carrying a modifier — a tool letter is always bare —
/// and for a key no tool claims.
pub fn tool_for_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
    active: Option<ToolId>,
) -> Option<ToolId> {
    if modifiers.any() {
        return None;
    }
    match Key::from_egui(key)? {
        Key::Char(c) => tools::registry::cycle(c, active),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::{ClipboardState, Intent};
    use crate::menu::MenuAction;

    fn command() -> egui::Modifiers {
        egui::Modifiers {
            command: true,
            ..Default::default()
        }
    }

    fn open_document() -> MenuContext {
        MenuContext {
            has_document: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_menu_chord_resolves_to_its_menu_items_action() {
        let ctx = MenuContext {
            can_undo: true,
            ..open_document()
        };
        let resolution = resolve_key(egui::Key::Z, command(), &ctx, 0).expect("Ctrl+Z is claimed");
        assert_eq!(resolution.intent(), Some(&Intent::Action(MenuAction::Undo)));
    }

    #[test]
    fn a_chord_whose_item_is_disabled_reports_the_same_reason_the_menu_would() {
        let ctx = open_document();
        let resolution = resolve_key(egui::Key::Z, command(), &ctx, 0).expect("claimed");
        assert_eq!(resolution.reason(), Some("Nothing to undo"));
        assert_eq!(
            MenuAction::Undo.resolve(&ctx).reason(),
            resolution.reason(),
            "the key and the menu item disagree"
        );
    }

    #[test]
    fn every_channel_shortcut_the_panel_prints_selects_that_channel() {
        // A chord hint painted beside a control is a promise. This walks the
        // rows the panel actually draws and presses each hint it prints.
        let doc = editor_core::Document::new(32, 32, "Test");
        let channels = ChannelsState::new();
        let mut pressed = 0usize;
        for row in channels.rows(&doc) {
            let Some(chord) = row.shortcut() else {
                continue;
            };
            let Key::Char(c) = chord.key else {
                panic!("{} printed a non-character chord", row.name)
            };
            let key = egui::Key::from_name(&c.to_string())
                .unwrap_or_else(|| panic!("egui has no key for {c}"));
            assert!(
                chord.ctrl && !chord.alt && !chord.shift,
                "{} printed {chord}",
                row.name
            );
            assert_eq!(
                channel_for_key(key, command(), &doc, &channels),
                Some(row.kind),
                "{} prints {chord} and it does nothing",
                row.name
            );
            // ...and the same chord must not already belong to a menu item,
            // or the menu would swallow it before the panel ever saw it.
            assert_eq!(
                action_for_shortcut(chord, 10),
                None,
                "{chord} is both a channel chord and a menu chord"
            );
            pressed += 1;
        }
        assert!(pressed >= 4, "the panel printed no chords at all");
    }

    #[test]
    fn a_channel_chord_needs_the_primary_modifier_and_nothing_else() {
        let doc = editor_core::Document::new(32, 32, "Test");
        let channels = ChannelsState::new();
        assert_eq!(
            channel_for_key(egui::Key::Num3, command(), &doc, &channels),
            Some(ChannelKind::Component(0))
        );
        assert_eq!(
            channel_for_key(egui::Key::Num3, egui::Modifiers::default(), &doc, &channels),
            None
        );
        assert_eq!(
            channel_for_key(
                egui::Key::Num3,
                egui::Modifiers {
                    shift: true,
                    ..command()
                },
                &doc,
                &channels
            ),
            None
        );
        // An RGB document has no fourth component, so Ctrl+6 names nothing.
        assert_eq!(
            channel_for_key(egui::Key::Num6, command(), &doc, &channels),
            None
        );
        // A letter is never a channel chord.
        assert_eq!(
            channel_for_key(egui::Key::B, command(), &doc, &channels),
            None
        );
    }

    #[test]
    fn an_unclaimed_chord_resolves_to_nothing() {
        assert!(resolve_key(egui::Key::Num9, command(), &open_document(), 0).is_none());
        // A bare letter is a tool key, not a menu chord.
        assert!(resolve_key(
            egui::Key::B,
            egui::Modifiers::default(),
            &open_document(),
            0
        )
        .is_none());
    }

    #[test]
    fn the_modifiers_are_part_of_the_chord() {
        let ctx = MenuContext {
            can_undo: true,
            can_redo: true,
            ..open_document()
        };
        let undo = resolve_key(egui::Key::Z, command(), &ctx, 0).unwrap();
        let redo = resolve_key(
            egui::Key::Z,
            egui::Modifiers {
                shift: true,
                ..command()
            },
            &ctx,
            0,
        )
        .unwrap();
        assert_eq!(undo.intent(), Some(&Intent::Action(MenuAction::Undo)));
        assert_eq!(redo.intent(), Some(&Intent::Action(MenuAction::Redo)));
    }

    #[test]
    fn paste_needs_the_clipboard_whichever_way_it_is_invoked() {
        let empty = open_document();
        assert_eq!(
            resolve_key(egui::Key::V, command(), &empty, 0)
                .unwrap()
                .reason(),
            Some("The clipboard is empty")
        );
        let full = MenuContext {
            clipboard: ClipboardState {
                pixels: true,
                layers: false,
            },
            ..empty
        };
        assert!(resolve_key(egui::Key::V, command(), &full, 0)
            .unwrap()
            .is_enabled());
    }

    #[test]
    fn a_bare_letter_selects_a_tool_and_a_modified_one_does_not() {
        assert_eq!(
            tool_for_key(egui::Key::B, egui::Modifiers::default(), None),
            Some(ToolId::Brush)
        );
        assert_eq!(tool_for_key(egui::Key::B, command(), None), None);
        assert_eq!(
            tool_for_key(
                egui::Key::B,
                egui::Modifiers {
                    shift: true,
                    ..Default::default()
                },
                None
            ),
            None
        );
    }

    #[test]
    fn pressing_a_tool_letter_again_cycles_within_its_group() {
        let group = tools::registry::by_shortcut('m');
        assert!(group.len() > 1);
        let first = tool_for_key(egui::Key::M, egui::Modifiers::default(), None).unwrap();
        assert_eq!(first, group[0]);
        let second = tool_for_key(egui::Key::M, egui::Modifiers::default(), Some(first)).unwrap();
        assert_eq!(second, group[1]);
    }

    #[test]
    fn a_key_no_tool_claims_selects_nothing() {
        assert_eq!(
            tool_for_key(egui::Key::Num8, egui::Modifiers::default(), None),
            None
        );
        assert_eq!(
            tool_for_key(egui::Key::Escape, egui::Modifiers::default(), None),
            None
        );
    }

    #[test]
    fn no_tool_letter_is_shadowed_by_a_bare_menu_chord() {
        // A menu item bound to a bare letter would swallow a tool key, and the
        // tool would silently stop responding. Nothing in the menu bar may
        // claim a bare letter.
        for info in tools::registry::all() {
            let Some(letter) = info.shortcut else {
                continue;
            };
            let chord = Shortcut::bare(Key::character(letter));
            assert_eq!(
                action_for_shortcut(chord, 10),
                None,
                "{:?}'s key {letter} is also a menu chord",
                info.id
            );
        }
    }

    #[test]
    fn every_bare_menu_chord_is_a_key_no_tool_wants() {
        // The same rule from the other side, so adding a bare-letter menu item
        // fails here even if no tool uses that letter today.
        for menu in crate::menu::menu_bar(10) {
            for action in menu.actions() {
                let Some(chord) = action.shortcut() else {
                    continue;
                };
                if !chord.is_bare() {
                    continue;
                }
                if let Key::Char(c) = chord.key {
                    assert!(
                        tools::registry::by_shortcut(c).is_empty(),
                        "{action:?} claims bare `{c}`, which is a tool key"
                    );
                }
            }
        }
    }
}
