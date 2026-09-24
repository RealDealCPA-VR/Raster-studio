//! W11-D: Paste with no document open (Photopea) through the everyday
//! gesture — the Ctrl+V chord on the shell's own key route (`on_key` ->
//! `Keymap::resolve_any` -> `Editor::dispatch`), not the menu bridge.

use super::*;
use winit::keyboard::Key as WKey;

use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn bare_shell(dir: &std::path::Path, clipboard: crate::clipboard::FakeClipboard) -> Shell {
    let mut editor = crate::editor::Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    editor.set_image_clipboard(Box::new(clipboard));
    Shell::new(editor, Vec::new())
}

fn press_ctrl_v(shell: &mut Shell) {
    shell.modifiers = ModifiersState::CONTROL;
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Character("v".into()),
        ElementState::Pressed,
        false,
    );
}

/// Ctrl+V with nothing open and a 5 x 3 image on the clipboard opens a
/// 5 x 3 document showing that image.
#[test]
fn ctrl_v_with_no_document_opens_the_clipboard_image() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (5u32, 3u32);
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i * 10) as u8, 100, 200, 255])
        .collect();
    let mut os = crate::clipboard::FakeClipboard::new();
    os.seed(crate::clipboard::ClipboardImage::validate(w, h, rgba.clone()).unwrap());
    let mut shell = bare_shell(dir.path(), os);
    assert!(shell.editor.active().is_none());

    press_ctrl_v(&mut shell);

    assert_eq!(
        shell.editor.documents().len(),
        1,
        "Ctrl+V opened a document"
    );
    let doc = shell.editor.active_mut().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (w, h));
    let rect = doc.canvas_rect();
    assert_eq!(
        doc.composite(rect).unwrap(),
        rgba,
        "the document shows the clipboard's image"
    );
}

/// Ctrl+V with nothing open and nothing on the clipboard creates nothing
/// and says why on the status line.
#[test]
fn ctrl_v_with_no_document_and_an_empty_clipboard_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = bare_shell(dir.path(), crate::clipboard::FakeClipboard::new());

    press_ctrl_v(&mut shell);

    assert!(shell.editor.documents().is_empty());
    let status = shell
        .editor
        .status()
        .map(|s| s.to_string())
        .unwrap_or_default();
    assert!(
        status.contains("clipboard is empty"),
        "the status line says why: {status:?}"
    );
}
