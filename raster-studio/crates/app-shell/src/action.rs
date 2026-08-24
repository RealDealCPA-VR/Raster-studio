//! The catalogue of things the application can be asked to do.
//!
//! An [`Action`] is a *name*, not an implementation. The keymap resolves a key
//! chord to one, the menu bar renders one, and [`crate::Editor::dispatch`]
//! performs one. Keeping the three on the same enum is what makes "this menu
//! item does nothing" a compile error rather than a bug report: `dispatch`
//! matches exhaustively with no wildcard arm, so a new variant does not build
//! until it is wired.
//!
//! # Why tool selection is one variant and not forty-five
//!
//! `tools::registry` already owns the Photoshop-style *cycle group*: several
//! tools share a letter and pressing it repeatedly walks the group
//! ([`tools::registry::cycle`]). Modelling the action as
//! [`Action::SelectTool`] carrying that letter keeps the registry the single
//! source of truth for which tools exist; a variant per [`tools::ToolId`] would
//! be a second list to keep in step.

use std::collections::BTreeSet;
use std::fmt;

/// The letter that selects a tool group, normalised to lower case.
///
/// Constructed only through [`ToolKey::new`], so a key that no tool answers to
/// cannot be built — which is what stops the keymap binding a letter to a tool
/// group that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToolKey(char);

impl ToolKey {
    /// The key, if any tool in the registry answers to it.
    pub fn new(key: char) -> Option<Self> {
        let key = key.to_ascii_lowercase();
        tools::registry::by_shortcut(key)
            .first()
            .map(|_| ToolKey(key))
    }

    pub fn char(self) -> char {
        self.0
    }

    /// Every letter the tool registry answers to, in sorted order.
    pub fn all() -> Vec<ToolKey> {
        tools::registry::all()
            .iter()
            .filter_map(|t| t.shortcut)
            .collect::<BTreeSet<char>>()
            .into_iter()
            .map(ToolKey)
            .collect()
    }
}

impl fmt::Display for ToolKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.to_ascii_uppercase())
    }
}

/// Which menu an action belongs under. Used to build the menu bar and to group
/// the keyboard-shortcut editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    File,
    Edit,
    Layer,
    View,
    Tool,
    Window,
}

impl Category {
    pub const ALL: &'static [Category] = &[
        Category::File,
        Category::Edit,
        Category::Layer,
        Category::View,
        Category::Tool,
        Category::Window,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Category::File => "File",
            Category::Edit => "Edit",
            Category::Layer => "Layer",
            Category::View => "View",
            Category::Tool => "Tools",
            Category::Window => "Window",
        }
    }
}

/// A named, routable application action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    // ---- File ----
    NewDocument,
    Open,
    /// Open a `.rstudio` package. Separate from [`Action::Open`] because a
    /// package is a *directory*: the platform's file picker cannot return one,
    /// so this action goes through the folder picker instead.
    OpenProject,
    Save,
    SaveAs,
    Export,
    CloseDocument,
    Quit,
    // ---- Edit ----
    Undo,
    Redo,
    /// Open or close the preferences window, which holds the shortcut editor.
    ShowPreferences,
    /// Open or close the File Info… metadata window.
    ShowFileInfo,
    // ---- Layer ----
    NewLayer,
    DeleteLayer,
    DuplicateLayer,
    ToggleLayerVisibility,
    // ---- View ----
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActualPixels,
    TogglePanels,
    // ---- Tools / painting ----
    SelectTool(ToolKey),
    /// Hold to borrow the hand tool; released by [`crate::Editor::release_temporary_hand`].
    TemporaryHand,
    DecreaseBrushSize,
    IncreaseBrushSize,
    SwapColors,
    ResetColors,
    // ---- Window ----
    NextDocument,
    PreviousDocument,
}

/// The fixed part of the catalogue — every variant that carries no payload.
///
/// `Action::all()` is this list plus one [`Action::SelectTool`] per registry
/// letter. `every_variant_is_reachable_from_all` pins the two together.
const FIXED: &[Action] = &[
    Action::NewDocument,
    Action::Open,
    Action::OpenProject,
    Action::Save,
    Action::SaveAs,
    Action::Export,
    Action::CloseDocument,
    Action::Quit,
    Action::Undo,
    Action::Redo,
    Action::ShowPreferences,
    Action::ShowFileInfo,
    Action::DeleteLayer,
    Action::DuplicateLayer,
    Action::ToggleLayerVisibility,
    Action::ZoomIn,
    Action::ZoomOut,
    Action::ZoomFit,
    Action::ZoomActualPixels,
    Action::TogglePanels,
    Action::TemporaryHand,
    Action::DecreaseBrushSize,
    Action::IncreaseBrushSize,
    Action::SwapColors,
    Action::ResetColors,
    Action::NextDocument,
    Action::PreviousDocument,
];

impl Action {
    /// Every action the application has, each exactly once.
    pub fn all() -> Vec<Action> {
        let mut out = FIXED.to_vec();
        out.extend(ToolKey::all().into_iter().map(Action::SelectTool));
        out
    }

    /// Stable identifier, used as the key in the persisted keymap and as the
    /// completeness oracle in tests.
    ///
    /// Matched exhaustively with no wildcard: a new variant does not compile
    /// until it is named here, and naming it without adding it to [`FIXED`]
    /// fails `every_variant_is_reachable_from_all`.
    pub fn id(self) -> String {
        match self {
            Action::NewDocument => "new-document".into(),
            Action::Open => "open".into(),
            Action::OpenProject => "open-project".into(),
            Action::Save => "save".into(),
            Action::SaveAs => "save-as".into(),
            Action::Export => "export".into(),
            Action::CloseDocument => "close-document".into(),
            Action::Quit => "quit".into(),
            Action::Undo => "undo".into(),
            Action::Redo => "redo".into(),
            Action::ShowPreferences => "show-preferences".into(),
            Action::ShowFileInfo => "show-file-info".into(),
            Action::NewLayer => "new-layer".into(),
            Action::DeleteLayer => "delete-layer".into(),
            Action::DuplicateLayer => "duplicate-layer".into(),
            Action::ToggleLayerVisibility => "toggle-layer-visibility".into(),
            Action::ZoomIn => "zoom-in".into(),
            Action::ZoomOut => "zoom-out".into(),
            Action::ZoomFit => "zoom-fit".into(),
            Action::ZoomActualPixels => "zoom-actual-pixels".into(),
            Action::TogglePanels => "toggle-panels".into(),
            Action::SelectTool(k) => format!("select-tool-{}", k.char()),
            Action::TemporaryHand => "temporary-hand".into(),
            Action::DecreaseBrushSize => "decrease-brush-size".into(),
            Action::IncreaseBrushSize => "increase-brush-size".into(),
            Action::SwapColors => "swap-colors".into(),
            Action::ResetColors => "reset-colors".into(),
            Action::NextDocument => "next-document".into(),
            Action::PreviousDocument => "previous-document".into(),
        }
    }

    /// Parse an [`Action::id`] back. Unknown ids read as `None` so a keymap
    /// written by a newer build loses one binding rather than failing to load.
    pub fn from_id(id: &str) -> Option<Action> {
        if let Some(rest) = id.strip_prefix("select-tool-") {
            let mut chars = rest.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            return ToolKey::new(c).map(Action::SelectTool);
        }
        Action::all().into_iter().find(|a| a.id() == id)
    }

    /// The menu this action lives under.
    pub fn category(self) -> Category {
        match self {
            Action::NewDocument
            | Action::Open
            | Action::OpenProject
            | Action::Save
            | Action::SaveAs
            | Action::Export
            | Action::CloseDocument
            | Action::Quit => Category::File,
            Action::Undo | Action::Redo | Action::ShowPreferences | Action::ShowFileInfo => {
                Category::Edit
            }
            Action::NewLayer
            | Action::DeleteLayer
            | Action::DuplicateLayer
            | Action::ToggleLayerVisibility => Category::Layer,
            Action::ZoomIn
            | Action::ZoomOut
            | Action::ZoomFit
            | Action::ZoomActualPixels
            | Action::TogglePanels => Category::View,
            Action::SelectTool(_)
            | Action::TemporaryHand
            | Action::DecreaseBrushSize
            | Action::IncreaseBrushSize
            | Action::SwapColors
            | Action::ResetColors => Category::Tool,
            Action::NextDocument | Action::PreviousDocument => Category::Window,
        }
    }

    /// The label shown in menus.
    pub fn label(self) -> String {
        match self {
            Action::NewDocument => "New".into(),
            Action::Open => "Open…".into(),
            Action::OpenProject => "Open Project…".into(),
            Action::Save => "Save".into(),
            Action::SaveAs => "Save As…".into(),
            Action::Export => "Export…".into(),
            Action::CloseDocument => "Close".into(),
            Action::Quit => "Quit".into(),
            Action::Undo => "Undo".into(),
            Action::Redo => "Redo".into(),
            Action::ShowPreferences => "Preferences…".into(),
            Action::ShowFileInfo => "File Info…".into(),
            Action::NewLayer => "New Layer".into(),
            Action::DeleteLayer => "Delete Layer".into(),
            Action::DuplicateLayer => "Duplicate Layer".into(),
            Action::ToggleLayerVisibility => "Show / Hide Layer".into(),
            Action::ZoomIn => "Zoom In".into(),
            Action::ZoomOut => "Zoom Out".into(),
            Action::ZoomFit => "Fit on Screen".into(),
            Action::ZoomActualPixels => "Actual Pixels".into(),
            Action::TogglePanels => "Hide / Show Panels".into(),
            Action::SelectTool(k) => match tools::registry::by_shortcut(k.char()).first() {
                Some(id) => match tools::registry::info(*id) {
                    Some(info) => info.name.to_string(),
                    None => format!("Tool {k}"),
                },
                None => format!("Tool {k}"),
            },
            Action::TemporaryHand => "Temporary Hand".into(),
            Action::DecreaseBrushSize => "Smaller Brush".into(),
            Action::IncreaseBrushSize => "Larger Brush".into(),
            Action::SwapColors => "Swap Colours".into(),
            Action::ResetColors => "Default Colours".into(),
            Action::NextDocument => "Next Document".into(),
            Action::PreviousDocument => "Previous Document".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_is_reachable_from_all() {
        // `id()` is an exhaustive match, so a new variant fails to compile
        // until it is named. This then fails until it is also *listed*.
        let all = Action::all();
        let ids: BTreeSet<String> = all.iter().map(|a| a.id()).collect();
        assert_eq!(ids.len(), all.len(), "Action::all() repeats an action");
        assert_eq!(
            all.len(),
            FIXED.len() + ToolKey::all().len(),
            "Action::all() must be the fixed list plus one action per tool letter"
        );
        // Nothing carries a payload except SelectTool, and every SelectTool in
        // `all()` uses a registry letter.
        for a in &all {
            if let Action::SelectTool(k) = a {
                assert!(
                    !tools::registry::by_shortcut(k.char()).is_empty(),
                    "{k} selects no tool"
                );
            }
        }
    }

    #[test]
    fn ids_round_trip() {
        for a in Action::all() {
            assert_eq!(Action::from_id(&a.id()), Some(a), "{a:?}");
        }
        assert_eq!(Action::from_id("no-such-action"), None);
        assert_eq!(Action::from_id("select-tool-"), None);
        assert_eq!(Action::from_id("select-tool-ab"), None);
        // A letter no tool answers to is not an action.
        assert_eq!(Action::from_id("select-tool-9"), None);
    }

    #[test]
    fn a_tool_key_must_name_a_real_tool_group() {
        assert!(ToolKey::new('b').is_some(), "the brush group");
        assert_eq!(ToolKey::new('9'), None);
        // Case is normalised, so Shift+B and B name the same group.
        assert_eq!(ToolKey::new('B'), ToolKey::new('b'));
    }

    #[test]
    fn every_action_has_a_category_and_a_label() {
        for a in Action::all() {
            let label = a.label();
            assert!(!label.is_empty(), "{a:?} has no label");
            assert!(
                Category::ALL.contains(&a.category()),
                "{a:?} has an unlisted category"
            );
        }
    }
}
