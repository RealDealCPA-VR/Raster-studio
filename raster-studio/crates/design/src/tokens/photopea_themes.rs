//! W13X-4 / W15-D: all seven of Photopea's themes (More > Theme), as palettes.
//!
//! Photopea ships seven themes (`iV.ml` in its `pp.js`): Light Grey, Dark
//! Grey, Blue, Dark Blue, Purple, Black and White. W13X-4 added five of them
//! (Light Grey, Blue, Dark Blue, Purple, Black); W15-D added the other two,
//! [`DARK_GREY_ROLES`] and [`WHITE_ROLES`]. The app's own
//! [`super::palette::LIGHT_ROLES`] and [`super::palette::DARK_ROLES`] were
//! modelled on White and Dark Grey but are not copies of them (their docs
//! list every number that differs); they stay, so a saved "light" or "dark"
//! preference still draws what it drew.
//!
//! Six of Photopea's per-theme numbers are carried over, one per role:
//! `--base` -> `SurfacePanel`, `--bg-canvas` -> `BackgroundCanvas`,
//! `--bg-bbtn` -> `ControlFill`, `--bg-bbtnOver` -> `ControlFillHovered`,
//! `--text-color` -> `TextPrimary`, and `--accent` (#3482F6 in every theme)
//! -> `Accent`. Photopea has no number for the other roles (wells, header
//! bands, raised cards, secondary text, links, semantic and data colours,
//! accent hover/press shades), so those are derived around the six. Photopea's
//! `--bg-panel`, `--bg-input` and border numbers are not mapped.
//!
//! Every palette here is held to the same gates as the first two —
//! `crates/design/tests/token_gates.rs` iterates [`crate::Theme::ALL`] — and
//! where one of the six numbers fails a gate, the palette's doc names the
//! number, the value used instead, and the measured ratio. The full list of
//! departures (six numbers in all) is pinned by
//! `tests::the_photopea_numbers_are_carried_over_except_the_listed_departures`,
//! so this prose and the tables cannot drift apart.

use super::color::Srgba;
use super::palette::ColorRole;

/// Photopea's **Light Grey**: `--base` #E0E0E0 panel over a #B0B0B0
/// `--bg-canvas` pasteboard, #F2F2F2 buttons with a #FFFFFF hover — all as
/// Photopea has them. Two departures:
/// - text #222221, not Photopea's #393837: #393837 clears the #A2A2A2 well at
///   only 4.58:1, so no lighter (dimmer) shade is left for secondary and
///   tertiary text, which the gates hold to 4.5:1 on every surface AND
///   require to differ from primary; the primary goes darker to make room.
/// - accent #0B5CC4, not #3482F6: #3482F6 stands 2.81:1 off the #E0E0E0
///   panel, under the 3:1 accent-border gate.
pub const LIGHT_GREY_ROLES: &[(ColorRole, Srgba)] = &[
    (ColorRole::SurfaceSunken, Srgba::hex(0xA2A2A2)),
    (ColorRole::BackgroundCanvas, Srgba::hex(0xB0B0B0)),
    (ColorRole::SurfaceHeader, Srgba::hex(0xC8C8C8)),
    (ColorRole::SurfacePanel, Srgba::hex(0xE0E0E0)),
    (ColorRole::SurfaceElevated, Srgba::hex(0xEEEEEE)),
    (ColorRole::SurfaceOverlay, Srgba::hex(0xEBEBEB)),
    (ColorRole::SeparatorHairline, Srgba::hexa(0x00000022)),
    (ColorRole::SeparatorStrong, Srgba::hex(0x9B9B9B)),
    (ColorRole::TextPrimary, Srgba::hex(0x222221)),
    (ColorRole::TextSecondary, Srgba::hex(0x2B2A29)),
    (ColorRole::TextTertiary, Srgba::hex(0x323130)),
    (ColorRole::TextDisabled, Srgba::hex(0x848484)),
    (ColorRole::TextOnAccent, Srgba::hex(0xFFFFFF)),
    (ColorRole::TextLink, Srgba::hex(0x06337A)),
    (ColorRole::Accent, Srgba::hex(0x0B5CC4)),
    (ColorRole::AccentHovered, Srgba::hex(0x0A52B0)),
    (ColorRole::AccentPressed, Srgba::hex(0x09489C)),
    (ColorRole::AccentSubtle, Srgba::hexa(0x0B62CE1F)),
    (ColorRole::AccentMuted, Srgba::hex(0x8FAFD8)),
    (ColorRole::Success, Srgba::hex(0x1A6E34)),
    (ColorRole::SuccessSubtle, Srgba::hexa(0x1E7F3C1F)),
    (ColorRole::Warning, Srgba::hex(0x7E5200)),
    (ColorRole::WarningSubtle, Srgba::hexa(0x9A64001F)),
    (ColorRole::Danger, Srgba::hex(0xA8201A)),
    (ColorRole::DangerSubtle, Srgba::hexa(0xC0261F1F)),
    (ColorRole::ControlFill, Srgba::hex(0xF2F2F2)),
    (ColorRole::ControlFillHovered, Srgba::hex(0xFFFFFF)),
    (ColorRole::ControlFillActive, Srgba::hex(0xD7D7D7)),
    (ColorRole::ControlFillDisabled, Srgba::hex(0xE9E9E9)),
    (ColorRole::ControlStroke, Srgba::hexa(0x0000002E)),
    (ColorRole::ControlStrokeStrong, Srgba::hexa(0x00000047)),
    (ColorRole::SelectionFill, Srgba::hexa(0x0B62CE3D)),
    (ColorRole::SelectionStroke, Srgba::hex(0x0B5CC4)),
    (ColorRole::FocusRing, Srgba::hexa(0x0B62CE99)),
    (ColorRole::ShadowColor, Srgba::hexa(0x0000002E)),
    (ColorRole::ChannelRed, Srgba::hex(0x901212)),
    (ColorRole::ChannelGreen, Srgba::hex(0x0F4E22)),
    (ColorRole::ChannelBlue, Srgba::hex(0x123F95)),
    (ColorRole::Luminance, Srgba::hex(0x3C3C3C)),
];

/// Photopea's **Blue**: `--base` #404550 panel over a #252A35 pasteboard,
/// #60606A buttons with a #6A6A7A hover, #F0F0FA text — all as Photopea has
/// them. One departure: accent #5B9BF0 (the shipped dark accent, dark text on
/// it), not #3482F6, which stands 2.59:1 off the #404550 panel, under the
/// 3:1 accent-border gate.
pub const BLUE_ROLES: &[(ColorRole, Srgba)] = &[
    (ColorRole::SurfaceSunken, Srgba::hex(0x1E222B)),
    (ColorRole::BackgroundCanvas, Srgba::hex(0x252A35)),
    (ColorRole::SurfaceHeader, Srgba::hex(0x343944)),
    (ColorRole::SurfacePanel, Srgba::hex(0x404550)),
    (ColorRole::SurfaceElevated, Srgba::hex(0x4B505A)),
    (ColorRole::SurfaceOverlay, Srgba::hex(0x444954)),
    (ColorRole::SeparatorHairline, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::SeparatorStrong, Srgba::hex(0x575B65)),
    (ColorRole::TextPrimary, Srgba::hex(0xF0F0FA)),
    (ColorRole::TextSecondary, Srgba::hex(0xE2E2EC)),
    (ColorRole::TextTertiary, Srgba::hex(0xD0D1DB)),
    (ColorRole::TextDisabled, Srgba::hex(0x8F929C)),
    (ColorRole::TextOnAccent, Srgba::hex(0x0B1526)),
    (ColorRole::TextLink, Srgba::hex(0x9CCBFF)),
    (ColorRole::Accent, Srgba::hex(0x5B9BF0)),
    (ColorRole::AccentHovered, Srgba::hex(0x4A8CE4)),
    (ColorRole::AccentPressed, Srgba::hex(0x4385DC)),
    (ColorRole::AccentSubtle, Srgba::hexa(0x5B9BF033)),
    (ColorRole::AccentMuted, Srgba::hex(0x4E70A0)),
    (ColorRole::Success, Srgba::hex(0x4DBE70)),
    (ColorRole::SuccessSubtle, Srgba::hexa(0x4DBE7033)),
    (ColorRole::Warning, Srgba::hex(0xE0A020)),
    (ColorRole::WarningSubtle, Srgba::hexa(0xE0A02033)),
    (ColorRole::Danger, Srgba::hex(0xF27068)),
    (ColorRole::DangerSubtle, Srgba::hexa(0xF2706833)),
    (ColorRole::ControlFill, Srgba::hex(0x60606A)),
    (ColorRole::ControlFillHovered, Srgba::hex(0x6A6A7A)),
    (ColorRole::ControlFillActive, Srgba::hex(0x737382)),
    (ColorRole::ControlFillDisabled, Srgba::hex(0x4B4E59)),
    (ColorRole::ControlStroke, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::ControlStrokeStrong, Srgba::hexa(0xFFFFFF3D)),
    (ColorRole::SelectionFill, Srgba::hexa(0x5B9BF066)),
    (ColorRole::SelectionStroke, Srgba::hex(0x7FB3F5)),
    (ColorRole::FocusRing, Srgba::hexa(0x7FB3F5B3)),
    (ColorRole::ShadowColor, Srgba::hexa(0x00000080)),
    (ColorRole::ChannelRed, Srgba::hex(0xFF6B6B)),
    (ColorRole::ChannelGreen, Srgba::hex(0x5FD068)),
    (ColorRole::ChannelBlue, Srgba::hex(0x6FA8FF)),
    (ColorRole::Luminance, Srgba::hex(0xF5F5F5)),
];

/// Photopea's **Dark Blue**: a #171921 `--bg-canvas` pasteboard, #363B50
/// buttons with a #303045 hover, #BBBBBB text and the #3482F6 accent — all
/// as Photopea has them. One departure: the panel is #303445, not Photopea's
/// `--base` #222531, which stands only 1.15:1 off the #171921 pasteboard,
/// under the 1.4:1 chrome-over-pasteboard gate for a dark theme; #303445 is
/// the same blue lifted to 1.42:1.
pub const DARK_BLUE_ROLES: &[(ColorRole, Srgba)] = &[
    (ColorRole::SurfaceSunken, Srgba::hex(0x08090C)),
    (ColorRole::BackgroundCanvas, Srgba::hex(0x171921)),
    (ColorRole::SurfaceHeader, Srgba::hex(0x1E212C)),
    (ColorRole::SurfacePanel, Srgba::hex(0x303445)),
    (ColorRole::SurfaceElevated, Srgba::hex(0x393C4B)),
    (ColorRole::SurfaceOverlay, Srgba::hex(0x343849)),
    (ColorRole::SeparatorHairline, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::SeparatorStrong, Srgba::hex(0x454957)),
    (ColorRole::TextPrimary, Srgba::hex(0xBBBBBB)),
    (ColorRole::TextSecondary, Srgba::hex(0xB2B2B2)),
    (ColorRole::TextTertiary, Srgba::hex(0xAAAAAA)),
    (ColorRole::TextDisabled, Srgba::hex(0x6C6F77)),
    (ColorRole::TextOnAccent, Srgba::hex(0x0B1526)),
    (ColorRole::TextLink, Srgba::hex(0x9CCBFF)),
    (ColorRole::Accent, Srgba::hex(0x3482F6)),
    (ColorRole::AccentHovered, Srgba::hex(0x4A90F7)),
    (ColorRole::AccentPressed, Srgba::hex(0x3180F3)),
    (ColorRole::AccentSubtle, Srgba::hexa(0x3482F633)),
    (ColorRole::AccentMuted, Srgba::hex(0x446698)),
    (ColorRole::Success, Srgba::hex(0x4DBE70)),
    (ColorRole::SuccessSubtle, Srgba::hexa(0x4DBE7033)),
    (ColorRole::Warning, Srgba::hex(0xE0A020)),
    (ColorRole::WarningSubtle, Srgba::hexa(0xE0A02033)),
    (ColorRole::Danger, Srgba::hex(0xF27068)),
    (ColorRole::DangerSubtle, Srgba::hexa(0xF2706833)),
    (ColorRole::ControlFill, Srgba::hex(0x363B50)),
    (ColorRole::ControlFillHovered, Srgba::hex(0x303045)),
    (ColorRole::ControlFillActive, Srgba::hex(0x3C3C50)),
    (ColorRole::ControlFillDisabled, Srgba::hex(0x303446)),
    (ColorRole::ControlStroke, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::ControlStrokeStrong, Srgba::hexa(0xFFFFFF3D)),
    (ColorRole::SelectionFill, Srgba::hexa(0x3482F666)),
    (ColorRole::SelectionStroke, Srgba::hex(0x7FB3F5)),
    (ColorRole::FocusRing, Srgba::hexa(0x7FB3F5B3)),
    (ColorRole::ShadowColor, Srgba::hexa(0x00000080)),
    (ColorRole::ChannelRed, Srgba::hex(0xFF6B6B)),
    (ColorRole::ChannelGreen, Srgba::hex(0x5FD068)),
    (ColorRole::ChannelBlue, Srgba::hex(0x6FA8FF)),
    (ColorRole::Luminance, Srgba::hex(0xC2C2C2)),
];

/// Photopea's **Purple**: `--base` #4B3E51 panel over a neutral #252525
/// `--bg-canvas` pasteboard, #68606E buttons with a #756A7A hover, #F0F0FA
/// text — all as Photopea has them. One departure: accent #5B9BF0, not
/// #3482F6, which stands 2.69:1 off the #4B3E51 panel, under the 3:1
/// accent-border gate. (Photopea's #322A35 `--bg-panel` / `--bg-input` is not
/// mapped.)
pub const PURPLE_ROLES: &[(ColorRole, Srgba)] = &[
    (ColorRole::SurfaceSunken, Srgba::hex(0x1E1E1E)),
    (ColorRole::BackgroundCanvas, Srgba::hex(0x252525)),
    (ColorRole::SurfaceHeader, Srgba::hex(0x3A333D)),
    (ColorRole::SurfacePanel, Srgba::hex(0x4B3E51)),
    (ColorRole::SurfaceElevated, Srgba::hex(0x564A5B)),
    (ColorRole::SurfaceOverlay, Srgba::hex(0x4F4254)),
    (ColorRole::SeparatorHairline, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::SeparatorStrong, Srgba::hex(0x615566)),
    (ColorRole::TextPrimary, Srgba::hex(0xF0F0FA)),
    (ColorRole::TextSecondary, Srgba::hex(0xE3E2EC)),
    (ColorRole::TextTertiary, Srgba::hex(0xD2D0DC)),
    (ColorRole::TextDisabled, Srgba::hex(0x958E9D)),
    (ColorRole::TextOnAccent, Srgba::hex(0x0B1526)),
    (ColorRole::TextLink, Srgba::hex(0x9CCBFF)),
    (ColorRole::Accent, Srgba::hex(0x5B9BF0)),
    (ColorRole::AccentHovered, Srgba::hex(0x4A8CE4)),
    (ColorRole::AccentPressed, Srgba::hex(0x4385DC)),
    (ColorRole::AccentSubtle, Srgba::hexa(0x5B9BF033)),
    (ColorRole::AccentMuted, Srgba::hex(0x536CA0)),
    (ColorRole::Success, Srgba::hex(0x4DBE70)),
    (ColorRole::SuccessSubtle, Srgba::hexa(0x4DBE7033)),
    (ColorRole::Warning, Srgba::hex(0xE0A020)),
    (ColorRole::WarningSubtle, Srgba::hexa(0xE0A02033)),
    (ColorRole::Danger, Srgba::hex(0xF27068)),
    (ColorRole::DangerSubtle, Srgba::hexa(0xF2706833)),
    (ColorRole::ControlFill, Srgba::hex(0x68606E)),
    (ColorRole::ControlFillHovered, Srgba::hex(0x756A7A)),
    (ColorRole::ControlFillActive, Srgba::hex(0x7D7382)),
    (ColorRole::ControlFillDisabled, Srgba::hex(0x554A5B)),
    (ColorRole::ControlStroke, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::ControlStrokeStrong, Srgba::hexa(0xFFFFFF3D)),
    (ColorRole::SelectionFill, Srgba::hexa(0x5B9BF066)),
    (ColorRole::SelectionStroke, Srgba::hex(0x7FB3F5)),
    (ColorRole::FocusRing, Srgba::hexa(0x7FB3F5B3)),
    (ColorRole::ShadowColor, Srgba::hexa(0x00000080)),
    (ColorRole::ChannelRed, Srgba::hex(0xFF6B6B)),
    (ColorRole::ChannelGreen, Srgba::hex(0x5FD068)),
    (ColorRole::ChannelBlue, Srgba::hex(0x6FA8FF)),
    (ColorRole::Luminance, Srgba::hex(0xF5F5F5)),
];

/// Photopea's **Black**: `--base` #353535 panel over a #1A1A1A pasteboard,
/// #505050 buttons with a #5A5A5A hover, #CCCCCC text and the #3482F6 accent —
/// all as Photopea has them, no departures.
pub const BLACK_ROLES: &[(ColorRole, Srgba)] = &[
    (ColorRole::SurfaceSunken, Srgba::hex(0x151515)),
    (ColorRole::BackgroundCanvas, Srgba::hex(0x1A1A1A)),
    (ColorRole::SurfaceHeader, Srgba::hex(0x292929)),
    (ColorRole::SurfacePanel, Srgba::hex(0x353535)),
    (ColorRole::SurfaceElevated, Srgba::hex(0x414141)),
    (ColorRole::SurfaceOverlay, Srgba::hex(0x393939)),
    (ColorRole::SeparatorHairline, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::SeparatorStrong, Srgba::hex(0x4D4D4D)),
    (ColorRole::TextPrimary, Srgba::hex(0xCCCCCC)),
    (ColorRole::TextSecondary, Srgba::hex(0xC0C0C0)),
    (ColorRole::TextTertiary, Srgba::hex(0xB1B1B1)),
    (ColorRole::TextDisabled, Srgba::hex(0x797979)),
    (ColorRole::TextOnAccent, Srgba::hex(0x0B1526)),
    (ColorRole::TextLink, Srgba::hex(0x9CCBFF)),
    (ColorRole::Accent, Srgba::hex(0x3482F6)),
    (ColorRole::AccentHovered, Srgba::hex(0x4A90F7)),
    (ColorRole::AccentPressed, Srgba::hex(0x3180F3)),
    (ColorRole::AccentSubtle, Srgba::hexa(0x3482F633)),
    (ColorRole::AccentMuted, Srgba::hex(0x486892)),
    (ColorRole::Success, Srgba::hex(0x4DBE70)),
    (ColorRole::SuccessSubtle, Srgba::hexa(0x4DBE7033)),
    (ColorRole::Warning, Srgba::hex(0xE0A020)),
    (ColorRole::WarningSubtle, Srgba::hexa(0xE0A02033)),
    (ColorRole::Danger, Srgba::hex(0xF27068)),
    (ColorRole::DangerSubtle, Srgba::hexa(0xF2706833)),
    (ColorRole::ControlFill, Srgba::hex(0x505050)),
    (ColorRole::ControlFillHovered, Srgba::hex(0x5A5A5A)),
    (ColorRole::ControlFillActive, Srgba::hex(0x646464)),
    (ColorRole::ControlFillDisabled, Srgba::hex(0x3E3E3E)),
    (ColorRole::ControlStroke, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::ControlStrokeStrong, Srgba::hexa(0xFFFFFF3D)),
    (ColorRole::SelectionFill, Srgba::hexa(0x3482F666)),
    (ColorRole::SelectionStroke, Srgba::hex(0x7FB3F5)),
    (ColorRole::FocusRing, Srgba::hexa(0x7FB3F5B3)),
    (ColorRole::ShadowColor, Srgba::hexa(0x00000080)),
    (ColorRole::ChannelRed, Srgba::hex(0xFF6B6B)),
    (ColorRole::ChannelGreen, Srgba::hex(0x5FD068)),
    (ColorRole::ChannelBlue, Srgba::hex(0x6FA8FF)),
    (ColorRole::Luminance, Srgba::hex(0xD1D1D1)),
];

/// W15-D: Photopea's **Dark Grey**: `--base` #474747 panel over a #252525
/// `--bg-canvas` pasteboard, #5D5D5D buttons with a #6A6A6A hover, #D5D5D5
/// text — all as Photopea has them. One departure: accent #5B9BF0 (the
/// shipped dark accent, dark text on it), not #3482F6, which stands 2.51:1
/// off the #474747 panel, under the 3:1 accent-border gate.
pub const DARK_GREY_ROLES: &[(ColorRole, Srgba)] = &[
    (ColorRole::SurfaceSunken, Srgba::hex(0x1E1E1E)),
    (ColorRole::BackgroundCanvas, Srgba::hex(0x252525)),
    (ColorRole::SurfaceHeader, Srgba::hex(0x363636)),
    (ColorRole::SurfacePanel, Srgba::hex(0x474747)),
    (ColorRole::SurfaceElevated, Srgba::hex(0x505050)),
    (ColorRole::SurfaceOverlay, Srgba::hex(0x4A4A4A)),
    (ColorRole::SeparatorHairline, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::SeparatorStrong, Srgba::hex(0x5C5C5C)),
    (ColorRole::TextPrimary, Srgba::hex(0xD5D5D5)),
    (ColorRole::TextSecondary, Srgba::hex(0xCDCDCD)),
    (ColorRole::TextTertiary, Srgba::hex(0xC4C4C4)),
    (ColorRole::TextDisabled, Srgba::hex(0x7E7E7E)),
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
    (ColorRole::ControlFill, Srgba::hex(0x5D5D5D)),
    (ColorRole::ControlFillHovered, Srgba::hex(0x6A6A6A)),
    (ColorRole::ControlFillActive, Srgba::hex(0x747474)),
    (ColorRole::ControlFillDisabled, Srgba::hex(0x4C4C4C)),
    (ColorRole::ControlStroke, Srgba::hexa(0xFFFFFF1F)),
    (ColorRole::ControlStrokeStrong, Srgba::hexa(0xFFFFFF3D)),
    (ColorRole::SelectionFill, Srgba::hexa(0x5B9BF066)),
    (ColorRole::SelectionStroke, Srgba::hex(0x7FB3F5)),
    (ColorRole::FocusRing, Srgba::hexa(0x7FB3F5B3)),
    (ColorRole::ShadowColor, Srgba::hexa(0x00000080)),
    (ColorRole::ChannelRed, Srgba::hex(0xFF6B6B)),
    (ColorRole::ChannelGreen, Srgba::hex(0x5FD068)),
    (ColorRole::ChannelBlue, Srgba::hex(0x6FA8FF)),
    (ColorRole::Luminance, Srgba::hex(0xDADADA)),
];

/// W15-D: Photopea's **White**: `--base` #F7F7F7 panel over a #E0E0E0
/// `--bg-canvas` pasteboard, #E0E0E0 buttons with a #D6D6D6 hover, #333333
/// text and the #3482F6 accent — all as Photopea has them, no departures.
/// White text stands only 3.70:1 on #3482F6, so text on the accent is the
/// dark #0B1526 (4.93:1); Photopea has no number for that role.
pub const WHITE_ROLES: &[(ColorRole, Srgba)] = &[
    (ColorRole::SurfaceSunken, Srgba::hex(0xD0D0D0)),
    (ColorRole::BackgroundCanvas, Srgba::hex(0xE0E0E0)),
    (ColorRole::SurfaceHeader, Srgba::hex(0xEBEBEB)),
    (ColorRole::SurfacePanel, Srgba::hex(0xF7F7F7)),
    (ColorRole::SurfaceElevated, Srgba::hex(0xFDFDFD)),
    (ColorRole::SurfaceOverlay, Srgba::hex(0xFBFBFB)),
    (ColorRole::SeparatorHairline, Srgba::hexa(0x00000022)),
    (ColorRole::SeparatorStrong, Srgba::hex(0xB8B8B8)),
    (ColorRole::TextPrimary, Srgba::hex(0x333333)),
    (ColorRole::TextSecondary, Srgba::hex(0x444444)),
    (ColorRole::TextTertiary, Srgba::hex(0x555555)),
    (ColorRole::TextDisabled, Srgba::hex(0x9A9A9A)),
    (ColorRole::TextOnAccent, Srgba::hex(0x0B1526)),
    (ColorRole::TextLink, Srgba::hex(0x0A50A8)),
    (ColorRole::Accent, Srgba::hex(0x3482F6)),
    (ColorRole::AccentHovered, Srgba::hex(0x4A90F7)),
    (ColorRole::AccentPressed, Srgba::hex(0x3180F3)),
    (ColorRole::AccentSubtle, Srgba::hexa(0x3482F61F)),
    (ColorRole::AccentMuted, Srgba::hex(0x9FC0EA)),
    (ColorRole::Success, Srgba::hex(0x1E7F3C)),
    (ColorRole::SuccessSubtle, Srgba::hexa(0x1E7F3C1F)),
    (ColorRole::Warning, Srgba::hex(0x9A6400)),
    (ColorRole::WarningSubtle, Srgba::hexa(0x9A64001F)),
    (ColorRole::Danger, Srgba::hex(0xC0261F)),
    (ColorRole::DangerSubtle, Srgba::hexa(0xC0261F1F)),
    (ColorRole::ControlFill, Srgba::hex(0xE0E0E0)),
    (ColorRole::ControlFillHovered, Srgba::hex(0xD6D6D6)),
    (ColorRole::ControlFillActive, Srgba::hex(0xCCCCCC)),
    (ColorRole::ControlFillDisabled, Srgba::hex(0xEEEEEE)),
    (ColorRole::ControlStroke, Srgba::hexa(0x00000026)),
    (ColorRole::ControlStrokeStrong, Srgba::hexa(0x0000003D)),
    (ColorRole::SelectionFill, Srgba::hexa(0x3482F63D)),
    (ColorRole::SelectionStroke, Srgba::hex(0x3482F6)),
    (ColorRole::FocusRing, Srgba::hexa(0x3482F699)),
    (ColorRole::ShadowColor, Srgba::hexa(0x0000002E)),
    (ColorRole::ChannelRed, Srgba::hex(0xC81E1E)),
    (ColorRole::ChannelGreen, Srgba::hex(0x1A7436)),
    (ColorRole::ChannelBlue, Srgba::hex(0x1F5FCC)),
    (ColorRole::Luminance, Srgba::hex(0x4A4A50)),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Photopea's numbers for the six carried-over roles, read from `pp.js`
    /// (`iV.ml`): `--base`, `--bg-canvas`, `--bg-bbtn`, `--bg-bbtnOver`,
    /// `--text-color`, `--accent`.
    type PhotopeaRow = (&'static str, &'static [(ColorRole, Srgba)], [u32; 6]);
    const PHOTOPEA: &[PhotopeaRow] = &[
        (
            "Light Grey",
            LIGHT_GREY_ROLES,
            [0xE0E0E0, 0xB0B0B0, 0xF2F2F2, 0xFFFFFF, 0x393837, 0x3482F6],
        ),
        (
            "Blue",
            BLUE_ROLES,
            [0x404550, 0x252A35, 0x60606A, 0x6A6A7A, 0xF0F0FA, 0x3482F6],
        ),
        (
            "Dark Blue",
            DARK_BLUE_ROLES,
            [0x222531, 0x171921, 0x363B50, 0x303045, 0xBBBBBB, 0x3482F6],
        ),
        (
            "Purple",
            PURPLE_ROLES,
            [0x4B3E51, 0x252525, 0x68606E, 0x756A7A, 0xF0F0FA, 0x3482F6],
        ),
        (
            "Black",
            BLACK_ROLES,
            [0x353535, 0x1A1A1A, 0x505050, 0x5A5A5A, 0xCCCCCC, 0x3482F6],
        ),
        (
            "Dark Grey",
            DARK_GREY_ROLES,
            [0x474747, 0x252525, 0x5D5D5D, 0x6A6A6A, 0xD5D5D5, 0x3482F6],
        ),
        (
            "White",
            WHITE_ROLES,
            [0xF7F7F7, 0xE0E0E0, 0xE0E0E0, 0xD6D6D6, 0x333333, 0x3482F6],
        ),
    ];

    const MAPPED: [ColorRole; 6] = [
        ColorRole::SurfacePanel,
        ColorRole::BackgroundCanvas,
        ColorRole::ControlFill,
        ColorRole::ControlFillHovered,
        ColorRole::TextPrimary,
        ColorRole::Accent,
    ];

    /// Every place a palette here uses a number other than Photopea's, as
    /// the palette docs list them: (theme, role, value used).
    const DEPARTURES: &[(&str, ColorRole, u32)] = &[
        ("Light Grey", ColorRole::TextPrimary, 0x222221),
        ("Light Grey", ColorRole::Accent, 0x0B5CC4),
        ("Blue", ColorRole::Accent, 0x5B9BF0),
        ("Dark Blue", ColorRole::SurfacePanel, 0x303445),
        ("Purple", ColorRole::Accent, 0x5B9BF0),
        ("Dark Grey", ColorRole::Accent, 0x5B9BF0),
    ];

    fn value(table: &[(ColorRole, Srgba)], role: ColorRole) -> Srgba {
        table
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, c)| *c)
            .unwrap_or_else(|| panic!("{role:?} missing"))
    }

    #[test]
    fn the_photopea_numbers_are_carried_over_except_the_listed_departures() {
        let mut found = Vec::new();
        for (name, table, numbers) in PHOTOPEA {
            for (role, photopea) in MAPPED.iter().zip(numbers) {
                let used = value(table, *role);
                if used != Srgba::hex(*photopea) {
                    found.push((*name, *role, used));
                }
            }
        }
        let listed: Vec<(&str, ColorRole, Srgba)> = DEPARTURES
            .iter()
            .map(|(n, r, v)| (*n, *r, Srgba::hex(*v)))
            .collect();
        assert_eq!(
            found, listed,
            "the palettes depart from pp.js in different places than the docs list"
        );
    }
}
