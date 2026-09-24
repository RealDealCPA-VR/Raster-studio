//! W10-B: a Glyphs panel pick lands in the text being edited.
//!
//! The panel raises [`ui::Intent::InsertGlyph`] for the active text layer;
//! the chrome hands it out as [`crate::chrome::ChromeOutput::insert_glyphs`]
//! and the shell calls [`insert_glyph`] with its [`ToolPointer`]:
//!
//! * **A typing session is open** — the characters go to the session as a
//!   typed insertion ([`tools::TextEdit::Insert`]), so they land at the caret
//!   (replacing a selected range), in the session's draft. The session's
//!   confirm then commits them with the rest of the run as its one undo
//!   step. A command against the committed text would be overwritten by the
//!   draft at confirm, which is why this route exists.
//! * **No session** — the characters are appended to the layer's committed
//!   text as one undo step ([`ui::panels::glyphs::insert_glyph`]).

use layer_model::LayerId;

use crate::editor::Editor;
use crate::tool_input::ToolPointer;

/// Insert `text` for text layer `layer`: at the live session's caret when a
/// typing session is open, else at the end of the layer's text.
pub(crate) fn insert_glyph(
    pointer: &mut ToolPointer,
    editor: &mut Editor,
    layer: LayerId,
    text: &str,
) -> Result<String, String> {
    if text.is_empty() {
        return Err("No glyph was picked".to_string());
    }
    if pointer.is_text_editing() {
        let out = pointer.text_edit(editor, tools::TextEdit::Insert(text));
        if let Some(reason) = out.failed {
            return Err(reason);
        }
        return Ok(format!("Inserted {text} at the caret"));
    }
    let doc = editor.active().ok_or("No document is open")?;
    let command = ui::panels::glyphs::insert_glyph(&doc.document, layer, text)
        .ok_or("The glyph needs an unlocked text layer")?;
    editor.apply_command(command);
    Ok(format!("Inserted {text}"))
}
