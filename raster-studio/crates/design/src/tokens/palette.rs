//! Semantic color roles and the light/dark palettes that fill them.
//!
//! Raw hex appears in exactly two places in this crate — [`LIGHT_ROLES`] and
//! [`DARK_ROLES`]. Everything else addresses color by [`ColorRole`], so a
//! palette can be swapped wholesale and the UI keeps its meaning.

use std::collections::{BTreeMap, BTreeSet};

use super::color::{contrast_ratio_over, Srgba, TextSize};

/// Every color slot the application is allowed to reference.
///
/// The variant list is the contract: a palette that does not define all of
/// [`ColorRole::ALL`] is incomplete, and [`Palette::missing_roles`] will say so.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum ColorRole {
    // ---- background layers, back to front -------------------------------
    /// Wells and insets that read as cut *into* the surface.
    SurfaceSunken,
    /// The pasteboard: the flat backdrop the open document floats on. The
    /// darkest chrome-adjacent layer in dark mode so the image is what glows.
    BackgroundCanvas,
    /// Bands that frame a panel body: the menu bar, the options bar, the
    /// document-tab strip, panel title rows and the status bar. Sits between
    /// the pasteboard and the panel body so each band reads as its own step.
    SurfaceHeader,
    /// Tool panels, inspectors, sidebars — the body colour of the chrome.
    SurfacePanel,
    /// Cards and floating bars sitting on a panel.
    SurfaceElevated,
    /// Menus, popovers, dialogs.
    SurfaceOverlay,

    // ---- separators ------------------------------------------------------
    /// Default 1px divider. Deliberately translucent so it sits on any layer.
    SeparatorHairline,
    /// Divider that must survive over busy content.
    SeparatorStrong,

    // ---- text ------------------------------------------------------------
    TextPrimary,
    TextSecondary,
    TextTertiary,
    TextDisabled,
    /// Text and glyphs drawn on top of [`ColorRole::Accent`].
    TextOnAccent,
    /// Hyperlink text.
    TextLink,

    // ---- accent ----------------------------------------------------------
    Accent,
    AccentHovered,
    AccentPressed,
    /// Low-alpha accent wash for selected backgrounds.
    AccentSubtle,
    /// Accent drained of energy, for disabled emphasis controls.
    AccentMuted,

    // ---- semantic --------------------------------------------------------
    Success,
    SuccessSubtle,
    Warning,
    WarningSubtle,
    Danger,
    DangerSubtle,

    // ---- control fills ---------------------------------------------------
    ControlFill,
    ControlFillHovered,
    ControlFillActive,
    ControlFillDisabled,
    ControlStroke,
    ControlStrokeStrong,

    // ---- selection & focus ----------------------------------------------
    SelectionFill,
    SelectionStroke,
    FocusRing,

    /// Color of drop shadows at full elevation opacity.
    ShadowColor,
}

impl ColorRole {
    /// Every role a complete palette must define.
    pub const ALL: &'static [ColorRole] = &[
        Self::SurfaceSunken,
        Self::BackgroundCanvas,
        Self::SurfaceHeader,
        Self::SurfacePanel,
        Self::SurfaceElevated,
        Self::SurfaceOverlay,
        Self::SeparatorHairline,
        Self::SeparatorStrong,
        Self::TextPrimary,
        Self::TextSecondary,
        Self::TextTertiary,
        Self::TextDisabled,
        Self::TextOnAccent,
        Self::TextLink,
        Self::Accent,
        Self::AccentHovered,
        Self::AccentPressed,
        Self::AccentSubtle,
        Self::AccentMuted,
        Self::Success,
        Self::SuccessSubtle,
        Self::Warning,
        Self::WarningSubtle,
        Self::Danger,
        Self::DangerSubtle,
        Self::ControlFill,
        Self::ControlFillHovered,
        Self::ControlFillActive,
        Self::ControlFillDisabled,
        Self::ControlStroke,
        Self::ControlStrokeStrong,
        Self::SelectionFill,
        Self::SelectionStroke,
        Self::FocusRing,
        Self::ShadowColor,
    ];
}

/// A background layer that text may be drawn on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum SurfaceRole {
    Sunken,
    Canvas,
    /// Menu / options / tab / status bands and panel title rows.
    Header,
    Panel,
    Elevated,
    Overlay,
}

impl SurfaceRole {
    /// Every surface text is allowed to land on.
    pub const ALL: &'static [SurfaceRole] = &[
        Self::Sunken,
        Self::Canvas,
        Self::Header,
        Self::Panel,
        Self::Elevated,
        Self::Overlay,
    ];

    /// The palette slot backing this surface.
    pub const fn color_role(self) -> ColorRole {
        match self {
            Self::Sunken => ColorRole::SurfaceSunken,
            Self::Canvas => ColorRole::BackgroundCanvas,
            Self::Header => ColorRole::SurfaceHeader,
            Self::Panel => ColorRole::SurfacePanel,
            Self::Elevated => ColorRole::SurfaceElevated,
            Self::Overlay => ColorRole::SurfaceOverlay,
        }
    }
}

/// A foreground text role, paired with the WCAG floor it is held to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum TextRole {
    Primary,
    Secondary,
    /// De-emphasised text — section headers, hints, unit suffixes.
    ///
    /// It is held to the same 4.5:1 floor as body text, because the crate's own
    /// primitives render it at [`TypeRole::Footnote`] (11pt), far below the
    /// >= 18pt that WCAG 2.1 SC 1.4.3 needs before the 3:1 floor applies.
    ///
    /// [`TypeRole::Footnote`]: super::typography::TypeRole::Footnote
    Tertiary,
    /// Disabled text is exempt from WCAG AA (SC 1.4.3 excludes inactive
    /// controls) and is therefore not contrast-checked.
    Disabled,
    Link,
}

impl TextRole {
    /// Every text role, including the exempt one.
    pub const ALL: &'static [TextRole] = &[
        Self::Primary,
        Self::Secondary,
        Self::Tertiary,
        Self::Disabled,
        Self::Link,
    ];

    /// The palette slot backing this text role.
    pub const fn color_role(self) -> ColorRole {
        match self {
            Self::Primary => ColorRole::TextPrimary,
            Self::Secondary => ColorRole::TextSecondary,
            Self::Tertiary => ColorRole::TextTertiary,
            Self::Disabled => ColorRole::TextDisabled,
            Self::Link => ColorRole::TextLink,
        }
    }

    /// The size class this role is contrast-checked at, or `None` when the
    /// role is exempt from the check.
    pub const fn checked_size(self) -> Option<TextSize> {
        match self {
            Self::Primary | Self::Secondary | Self::Tertiary | Self::Link => Some(TextSize::Normal),
            Self::Disabled => None,
        }
    }
}

/// Shown in place of a role a palette forgot to define.
///
/// Loud on purpose: a missing token must be visible, not silently black.
pub const MISSING_ROLE_COLOR: Srgba = Srgba::hex(0xFF00FF);

/// A complete mapping from [`ColorRole`] to color.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Palette {
    roles: BTreeMap<ColorRole, Srgba>,
    is_dark: bool,
}

impl Palette {
    /// Build a palette from role/color pairs. Later pairs win.
    ///
    /// Completeness is *not* enforced here — call [`Palette::missing_roles`].
    pub fn from_pairs(is_dark: bool, pairs: &[(ColorRole, Srgba)]) -> Self {
        Self {
            roles: pairs.iter().copied().collect(),
            is_dark,
        }
    }

    /// The light palette (the opt-in system-follow alternative).
    pub fn light() -> Self {
        Self::from_pairs(false, LIGHT_ROLES)
    }

    /// The dark palette — the shipped default, tuned to Photopea’s neutral greys:
    /// mid-grey chrome standing off a darker pasteboard.
    pub fn dark() -> Self {
        Self::from_pairs(true, DARK_ROLES)
    }

    /// `true` if this palette is intended for a dark appearance.
    pub const fn is_dark(&self) -> bool {
        self.is_dark
    }

    /// Color for `role`, or [`MISSING_ROLE_COLOR`] if the palette omits it.
    pub fn color(&self, role: ColorRole) -> Srgba {
        self.roles.get(&role).copied().unwrap_or(MISSING_ROLE_COLOR)
    }

    /// Color for `role`, or `None` if the palette omits it.
    pub fn get(&self, role: ColorRole) -> Option<Srgba> {
        self.roles.get(&role).copied()
    }

    /// The exact set of roles this palette defines.
    pub fn defined_roles(&self) -> BTreeSet<ColorRole> {
        self.roles.keys().copied().collect()
    }

    /// Roles from [`ColorRole::ALL`] that this palette fails to define.
    pub fn missing_roles(&self) -> Vec<ColorRole> {
        ColorRole::ALL
            .iter()
            .copied()
            .filter(|r| !self.roles.contains_key(r))
            .collect()
    }

    /// Convenience accessor for a background layer.
    pub fn surface(&self, surface: SurfaceRole) -> Srgba {
        self.color(surface.color_role())
    }

    /// Convenience accessor for a text role.
    pub fn text(&self, text: TextRole) -> Srgba {
        self.color(text.color_role())
    }

    /// WCAG contrast of `text` drawn on `surface`, alpha resolved.
    pub fn text_contrast(&self, text: TextRole, surface: SurfaceRole) -> f32 {
        contrast_ratio_over(self.text(text), self.surface(surface))
    }
}

/// Light appearance — Photopea's light theme: a #F0F0F0 chrome around a
/// #D8D8D8 pasteboard, so the document sits in a recess and the panels stand
/// off it. Surfaces climb from the sunken wells to near-white cards.
pub const LIGHT_ROLES: &[(ColorRole, Srgba)] = &[
    (ColorRole::SurfaceSunken, Srgba::hex(0xCCCCCC)),
    (ColorRole::BackgroundCanvas, Srgba::hex(0xD8D8D8)),
    (ColorRole::SurfaceHeader, Srgba::hex(0xE2E2E2)),
    (ColorRole::SurfacePanel, Srgba::hex(0xF0F0F0)),
    (ColorRole::SurfaceElevated, Srgba::hex(0xFAFAFA)),
    (ColorRole::SurfaceOverlay, Srgba::hex(0xF6F6F6)),
    (ColorRole::SeparatorHairline, Srgba::hexa(0x00000022)),
    (ColorRole::SeparatorStrong, Srgba::hex(0xB8B8B8)),
    (ColorRole::TextPrimary, Srgba::hex(0x16161A)),
    (ColorRole::TextSecondary, Srgba::hex(0x4A4A50)),
    // Dark enough to clear 4.5:1 on Sunken, the lowest surface it lands on:
    // Tertiary is rendered at 11pt, which WCAG gives no size discount for.
    (ColorRole::TextTertiary, Srgba::hex(0x555555)),
    (ColorRole::TextDisabled, Srgba::hex(0x9A9A9A)),
    (ColorRole::TextOnAccent, Srgba::hex(0xFFFFFF)),
    // Deep enough for 4.5:1 on the #CCCCCC well.
    (ColorRole::TextLink, Srgba::hex(0x0A50A8)),
    (ColorRole::Accent, Srgba::hex(0x0B62CE)),
    (ColorRole::AccentHovered, Srgba::hex(0x0A57B8)),
    (ColorRole::AccentPressed, Srgba::hex(0x094CA1)),
    (ColorRole::AccentSubtle, Srgba::hexa(0x0B62CE1F)),
    (ColorRole::AccentMuted, Srgba::hex(0x9FC0EA)),
    (ColorRole::Success, Srgba::hex(0x1E7F3C)),
    (ColorRole::SuccessSubtle, Srgba::hexa(0x1E7F3C1F)),
    (ColorRole::Warning, Srgba::hex(0x9A6400)),
    (ColorRole::WarningSubtle, Srgba::hexa(0x9A64001F)),
    (ColorRole::Danger, Srgba::hex(0xC0261F)),
    (ColorRole::DangerSubtle, Srgba::hexa(0xC0261F1F)),
    (ColorRole::ControlFill, Srgba::hex(0xFFFFFF)),
    (ColorRole::ControlFillHovered, Srgba::hex(0xE8E8E8)),
    (ColorRole::ControlFillActive, Srgba::hex(0xDCDCDC)),
    (ColorRole::ControlFillDisabled, Srgba::hex(0xEAEAEA)),
    (ColorRole::ControlStroke, Srgba::hexa(0x00000026)),
    (ColorRole::ControlStrokeStrong, Srgba::hexa(0x0000003D)),
    (ColorRole::SelectionFill, Srgba::hexa(0x0B62CE3D)),
    (ColorRole::SelectionStroke, Srgba::hex(0x0B62CE)),
    (ColorRole::FocusRing, Srgba::hexa(0x0B62CE99)),
    (ColorRole::ShadowColor, Srgba::hexa(0x0000002E)),
];

/// Dark appearance — Photopea's: a mid-grey chrome (#474747 panel bodies,
/// #3A3A3A header bands) around a DARKER pasteboard (#2B2B2B), so the panels
/// stand off the document area instead of sinking below it. The previous ramp
/// had this inverted (#282828 chrome over a #3C pasteboard).
pub const DARK_ROLES: &[(ColorRole, Srgba)] = &[
    // Neutral greys throughout (R = G = B).
    (ColorRole::SurfaceSunken, Srgba::hex(0x232323)),
    (ColorRole::BackgroundCanvas, Srgba::hex(0x2B2B2B)),
    (ColorRole::SurfaceHeader, Srgba::hex(0x3A3A3A)),
    (ColorRole::SurfacePanel, Srgba::hex(0x474747)),
    (ColorRole::SurfaceElevated, Srgba::hex(0x505050)),
    (ColorRole::SurfaceOverlay, Srgba::hex(0x4A4A4A)),
    (ColorRole::SeparatorHairline, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::SeparatorStrong, Srgba::hex(0x5C5C5C)),
    (ColorRole::TextPrimary, Srgba::hex(0xF2F2F2)),
    (ColorRole::TextSecondary, Srgba::hex(0xD6D6D6)),
    // Light enough to clear 4.5:1 on Elevated (#505050), the highest surface
    // it lands on, at the 11pt it is rendered at.
    (ColorRole::TextTertiary, Srgba::hex(0xC4C4C4)),
    (ColorRole::TextDisabled, Srgba::hex(0x808080)),
    // A #474747 panel needs the accent at 3:1 over it (SC 1.4.11), which puts
    // the accent's luminance above what white text can clear 4.5:1 on. The
    // arithmetic leaves one way through: a brighter blue with DARK text on it.
    // The contrast gate outranks the white-on-blue convention.
    (ColorRole::TextOnAccent, Srgba::hex(0x0B1526)),
    (ColorRole::TextLink, Srgba::hex(0x9CCBFF)),
    (ColorRole::Accent, Srgba::hex(0x5B9BF0)),
    (ColorRole::AccentHovered, Srgba::hex(0x4A8CE4)),
    (ColorRole::AccentPressed, Srgba::hex(0x4385DC)),
    (ColorRole::AccentSubtle, Srgba::hexa(0x5B9BF033)),
    (ColorRole::AccentMuted, Srgba::hex(0x4A6A94)),
    (ColorRole::Success, Srgba::hex(0x4DBE70)),
    (ColorRole::SuccessSubtle, Srgba::hexa(0x4DBE7033)),
    (ColorRole::Warning, Srgba::hex(0xE0A020)),
    (ColorRole::WarningSubtle, Srgba::hexa(0xE0A02033)),
    (ColorRole::Danger, Srgba::hex(0xF27068)),
    (ColorRole::DangerSubtle, Srgba::hexa(0xF2706833)),
    // Controls sit a step above the panel body they are drawn on.
    (ColorRole::ControlFill, Srgba::hex(0x555555)),
    (ColorRole::ControlFillHovered, Srgba::hex(0x606060)),
    (ColorRole::ControlFillActive, Srgba::hex(0x6A6A6A)),
    (ColorRole::ControlFillDisabled, Srgba::hex(0x4C4C4C)),
    (ColorRole::ControlStroke, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::ControlStrokeStrong, Srgba::hexa(0xFFFFFF3D)),
    (ColorRole::SelectionFill, Srgba::hexa(0x5B9BF066)),
    (ColorRole::SelectionStroke, Srgba::hex(0x7FB3F5)),
    (ColorRole::FocusRing, Srgba::hexa(0x7FB3F5B3)),
    (ColorRole::ShadowColor, Srgba::hexa(0x00000080)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_role_is_reported_and_painted_loudly() {
        let sparse = Palette::from_pairs(false, &[(ColorRole::Accent, Srgba::hex(0x112233))]);
        assert!(sparse.missing_roles().contains(&ColorRole::TextPrimary));
        assert_eq!(sparse.color(ColorRole::TextPrimary), MISSING_ROLE_COLOR);
        assert_eq!(sparse.get(ColorRole::TextPrimary), None);
        assert_eq!(sparse.color(ColorRole::Accent), Srgba::hex(0x112233));
    }

    #[test]
    fn role_list_has_no_duplicates() {
        let unique: BTreeSet<ColorRole> = ColorRole::ALL.iter().copied().collect();
        assert_eq!(unique.len(), ColorRole::ALL.len());
    }

    #[test]
    fn surface_and_text_roles_map_onto_distinct_color_roles() {
        let surfaces: BTreeSet<ColorRole> =
            SurfaceRole::ALL.iter().map(|s| s.color_role()).collect();
        assert_eq!(surfaces.len(), SurfaceRole::ALL.len());
        let texts: BTreeSet<ColorRole> = TextRole::ALL.iter().map(|t| t.color_role()).collect();
        assert_eq!(texts.len(), TextRole::ALL.len());
        assert!(surfaces.is_disjoint(&texts));
    }

    #[test]
    fn light_and_dark_disagree_about_darkness() {
        assert!(!Palette::light().is_dark());
        assert!(Palette::dark().is_dark());
    }
}
