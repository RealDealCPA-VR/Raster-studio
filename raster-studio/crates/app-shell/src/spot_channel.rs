//! W13X-4: spot channels in and out of `.psd`.
//!
//! A `.psd` stores a spot channel as one more channel of the merged image,
//! named like an alpha channel (1006 / 1045), and marks it as *spot* in the
//! DisplayInfo resource (1077): one record per extra channel, in order, each
//! with the ink colour, the solidity (Photoshop's "opacity" field, 0-100)
//! and a kind byte whose value 2 means spot.
//!
//! [`push_psd_spots`] appends the document's spot channels after its saved
//! selections (the named alpha channels `psd_resources::export_resources`
//! writes) and builds that 1077 record; [`adopt_psd_spots`] reads it back on
//! open and moves the channels it marks spot out of the saved selections and
//! into [`editor_core::Document::spot_channels`].
//!
//! The record lives beside the document in `.rstudio` without help from here:
//! it is a field of [`editor_core::Document`]. What the `.psd` does not get:
//! the merged preview image this build writes is its own composite, which
//! already shows the ink; a reader that composites the layers (Photoshop,
//! Photopea) adds the plate itself.

use editor_core::spot::SpotChannel;
use editor_core::{Document, Selection, SelectionMask};
use glam::IVec2;
use psd::resource::{self as res, AlphaChannel};
use psd::ImageResource;

/// DisplayInfo, the floating-point-colour version (Photoshop CS3+).
pub(crate) const ID_DISPLAY_INFO: u16 = 1077;
/// DisplayInfo's kind byte for a spot channel.
const KIND_SPOT: u8 = 2;
/// DisplayInfo's kind byte for an alpha channel shown as masked areas,
/// Photoshop's default.
const KIND_ALPHA: u8 = 1;
/// Photoshop's default alpha-channel overlay: red at 50%.
const ALPHA_OVERLAY: ([u8; 3], u8) = ([255, 0, 0], 50);

/// The coverage of `channel` over a `width` x `height` canvas, row-major.
fn plane(channel: &SpotChannel, width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(width as usize * height as usize);
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            out.push((channel.ink_at(IVec2::new(x, y)) * 255.0).round() as u8);
        }
    }
    out
}

/// One DisplayInfo record: RGB colour space, 16-bit components, opacity,
/// kind, one pad byte.
fn record(out: &mut Vec<u8>, ink: [u8; 3], opacity: u8, kind: u8) {
    out.extend(0u16.to_be_bytes());
    for c in ink {
        out.extend((u16::from(c) * 257).to_be_bytes());
    }
    out.extend(0u16.to_be_bytes());
    out.extend(u16::from(opacity.min(100)).to_be_bytes());
    out.push(kind);
    out.push(0);
}

/// Append `document`'s spot channels to `channels` (the alpha channels about
/// to be written, saved selections first), as far as `room` channels allow,
/// and return the DisplayInfo resource that marks them spot, plus a note per
/// channel that did not fit. `None` when the document has no spot channel.
pub(crate) fn push_psd_spots(
    document: &Document,
    width: u32,
    height: u32,
    room: usize,
    channels: &mut Vec<AlphaChannel>,
) -> (Option<ImageResource>, Vec<String>) {
    if document.spot_channels.is_empty() {
        return (None, Vec::new());
    }
    let alphas = channels.len();
    let fit = room
        .saturating_sub(alphas)
        .min(document.spot_channels.len());
    let (written, left) = document.spot_channels.split_at(fit);
    let notes = left
        .iter()
        .map(|c| {
            format!(
                "the spot channel {:?} is past the channels this .psd can hold and was not written",
                c.name
            )
        })
        .collect();
    if written.is_empty() {
        return (None, notes);
    }
    let mut data = Vec::new();
    data.extend(1u32.to_be_bytes());
    for _ in 0..alphas {
        record(&mut data, ALPHA_OVERLAY.0, ALPHA_OVERLAY.1, KIND_ALPHA);
    }
    for c in written {
        channels.push(AlphaChannel {
            name: c.name.clone(),
            coverage: plane(c, width, height),
        });
        record(&mut data, c.ink, c.solidity, KIND_SPOT);
    }
    (
        Some(ImageResource {
            id: ID_DISPLAY_INFO,
            name: String::new(),
            data,
        }),
        notes,
    )
}

/// One parsed DisplayInfo record: ink, opacity, kind.
fn display_records(resources: &[ImageResource]) -> Vec<([u8; 3], u8, u8)> {
    let Some(r) = resources.iter().find(|r| r.id == ID_DISPLAY_INFO) else {
        return Vec::new();
    };
    let Some(body) = r.data.get(4..) else {
        return Vec::new();
    };
    body.as_chunks::<14>()
        .0
        .iter()
        .map(|c| {
            let comp = |i: usize| (u16::from_be_bytes([c[i], c[i + 1]]) / 257) as u8;
            let opacity = u16::from_be_bytes([c[10], c[11]]).min(100) as u8;
            ([comp(2), comp(4), comp(6)], opacity, c[12])
        })
        .collect()
}

/// On open: the channels `file`'s DisplayInfo marks spot leave the saved
/// selections (where `psd_resources::import_resources` put every named
/// channel) and become spot channels, in file order.
pub(crate) fn adopt_psd_spots(file: &psd::PsdFile, document: &mut Document) {
    let records = display_records(&file.resources);
    if !records.iter().any(|r| r.2 == KIND_SPOT) {
        return;
    }
    let (width, height) = (file.header.width, file.header.height);
    for (channel, (ink, opacity, kind)) in res::alpha_channels(file).into_iter().zip(records) {
        if kind != KIND_SPOT {
            continue;
        }
        let Some(at) = document
            .saved_selections
            .iter()
            .position(|(name, _)| *name == channel.name)
        else {
            continue;
        };
        document.saved_selections.remove(at);
        let Ok(mask) = SelectionMask::new(IVec2::ZERO, width, height, channel.coverage) else {
            continue;
        };
        document.spot_channels.push(SpotChannel {
            coverage: Selection::Mask(mask),
            ..SpotChannel::empty(channel.name, ink, opacity)
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use editor_core::Command;
    use ui::menu::MenuAction;
    use ui::panels::channels::spot_ids;
    use ui::{Intent, PanelId};

    use super::*;
    use crate::chrome::{install_theme, Chrome, ChromeOutput};
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    const W: u32 = 40;
    const H: u32 = 30;

    fn editor(dir: &Path) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        )
    }

    /// A white image, open, with the left half selected.
    fn white_with_selection(dir: &Path) -> Editor {
        let png = dir.join("white.png");
        std::fs::write(
            &png,
            raster::encode(
                raster::ExportFormat::Png,
                W,
                H,
                &[255u8; (W * H * 4) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        let mut ed = editor(dir);
        ed.open_path(&png).unwrap();
        ed.apply_command(Command::SetSelection {
            selection: Selection::Rect {
                min: IVec2::ZERO,
                max: IVec2::new(W as i32 / 2, H as i32),
            },
        });
        ed
    }

    /// The headless window: the application's chrome drawn over the editor,
    /// and what each frame asks for applied the way the shell applies it
    /// (`ChromeOutput::commands` through `Editor::apply_command`, menu picks
    /// through `menu_bridge::perform`).
    struct Window {
        ctx: egui::Context,
        chrome: Chrome,
    }

    impl Window {
        fn new() -> Self {
            let ctx = egui::Context::default();
            install_theme(&ctx, design::Theme::Dark);
            let mut chrome = Chrome::new();
            let w = chrome.workspace_for_test();
            if !w.dock.is_open(PanelId::Channels) {
                w.absorb(&Intent::SetPanelOpen {
                    panel: PanelId::Channels,
                    open: true,
                });
            }
            w.dock.raise(PanelId::Channels);
            Self { ctx, chrome }
        }

        fn frame(
            &mut self,
            ed: &mut Editor,
            events: Vec<egui::Event>,
        ) -> Vec<(MenuAction, Result<String, String>)> {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let mut out = ChromeOutput::default();
            let chrome = &mut self.chrome;
            let _ = self.ctx.run(input, |ctx| out = chrome.ui(ctx, ed));
            for command in std::mem::take(&mut out.commands) {
                ed.apply_command(command);
            }
            out.menu
                .into_iter()
                .map(|a| (a, crate::menu_bridge::perform(a, ed)))
                .collect()
        }

        fn click(
            &mut self,
            ed: &mut Editor,
            id: egui::Id,
        ) -> Vec<(MenuAction, Result<String, String>)> {
            for _ in 0..3 {
                self.frame(ed, Vec::new());
            }
            let at = self
                .ctx
                .read_response(id)
                .unwrap_or_else(|| panic!("{id:?} was not drawn"))
                .rect
                .center();
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            self.frame(
                ed,
                vec![egui::Event::PointerMoved(at), press(true), press(false)],
            )
        }

        fn channels_menu(
            &mut self,
            ed: &mut Editor,
            row: &'static str,
        ) -> Vec<(MenuAction, Result<String, String>)> {
            let _ = self.click(ed, ui::view::ids::panel_menu(PanelId::Channels));
            self.click(ed, spot_ids::menu_row(row))
        }

        fn enter(&mut self, ed: &mut Editor) {
            self.frame(
                ed,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                }],
            );
        }
    }

    fn pixel(ed: &mut Editor, x: u32, y: u32) -> [u8; 4] {
        let open = ed.active_mut().unwrap();
        let rgba = open.composite(open.canvas_rect()).unwrap();
        let i = ((y * W + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    /// Channels ▸ New Spot Channel, in the application's own chrome: the
    /// dialog confirms a 100%-solid blue ink over the left-half selection as
    /// one undo step, the composite shows the ink there and only there, and
    /// the channel survives a `.rstudio` save and open.
    #[test]
    fn a_spot_channel_from_the_channels_menu_composites_as_ink_and_survives_save_and_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = white_with_selection(dir.path());
        let mut win = Window::new();
        let depth = ed.active().unwrap().history_depth();
        let _ = win.channels_menu(&mut ed, "new-spot");
        let dialog = win
            .chrome
            .workspace_for_test()
            .channels
            .spot_dialog
            .as_mut()
            .expect("New Spot Channel opened its dialog");
        dialog.set_ink([0, 0, 255]);
        dialog.set_solidity(100);
        win.enter(&mut ed);

        let doc = &ed.active().unwrap().document;
        assert_eq!(doc.spot_channels.len(), 1, "OK added the channel");
        assert_eq!(doc.spot_channels[0].name, "Spot Color 1");
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1, "one step");
        assert_eq!(pixel(&mut ed, 2, 2), [0, 0, 255, 255], "inked half");
        assert_eq!(pixel(&mut ed, W - 2, 2), [255, 255, 255, 255], "clear half");

        let project = dir.path().join("spot.rstudio");
        ed.active_mut().unwrap().save_to(&project, "test").unwrap();
        let mut reopened = editor(dir.path());
        reopened.open_path(&project).unwrap();
        assert_eq!(
            reopened.active().unwrap().document.spot_channels,
            ed.active().unwrap().document.spot_channels
        );
        assert_eq!(pixel(&mut reopened, 2, 2), [0, 0, 255, 255]);
        assert_eq!(pixel(&mut reopened, W - 2, 2), [255, 255, 255, 255]);

        // Undo takes the channel, and its ink, away.
        ed.dispatch(crate::action::Action::Undo).unwrap();
        assert!(ed.active().unwrap().document.spot_channels.is_empty());
        assert_eq!(pixel(&mut ed, 2, 2), [255, 255, 255, 255]);
    }

    /// Channels ▸ Merge Channels reaches Image ▸ Merge Channels' own handler:
    /// with one document open it answers with that handler's reason.
    #[test]
    fn merge_channels_from_the_channels_menu_reaches_the_merge_handler() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = white_with_selection(dir.path());
        let mut win = Window::new();
        let done = win.channels_menu(&mut ed, "merge");
        match done.as_slice() {
            [(MenuAction::MergeChannels, Err(reason))] => assert!(
                reason.contains("three open grayscale documents"),
                "{reason}"
            ),
            got => panic!("the Channels menu's Merge row performed {got:?}"),
        }
    }

    /// Save As PSD writes the spot channel as a named extra channel of the
    /// merged image, marked spot in DisplayInfo with its ink and solidity,
    /// and opening that `.psd` gives the spot channel back.
    #[test]
    fn a_spot_channel_is_written_to_psd_as_a_spot_channel_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = white_with_selection(dir.path());
        let command = {
            let doc = &ed.active().unwrap().document;
            editor_core::spot::new_spot_channel(doc, "Gold", [200, 150, 20], 60)
        };
        ed.apply_command(command);
        let saved = dir.path().join("spot.psd");
        let notes = ed.active_mut().unwrap().export_psd_to(&saved).unwrap();
        assert!(notes.summary().is_none(), "{notes:?}");

        let file = psd::read(&std::fs::read(&saved).unwrap()).unwrap();
        let channels = res::alpha_channels(&file);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "Gold");
        assert_eq!(channels[0].coverage[0], 255, "the selected half is inked");
        assert_eq!(channels[0].coverage[(W - 1) as usize], 0);
        assert_eq!(
            display_records(&file.resources),
            vec![([200, 150, 20], 60, KIND_SPOT)]
        );

        let mut reopened = editor(dir.path());
        reopened.open_path(&saved).unwrap();
        let doc = &reopened.active().unwrap().document;
        assert!(doc.saved_selections.is_empty(), "not an alpha channel");
        assert_eq!(doc.spot_channels.len(), 1);
        let c = &doc.spot_channels[0];
        assert_eq!(
            (c.name.as_str(), c.ink, c.solidity),
            ("Gold", [200, 150, 20], 60)
        );
        assert_eq!(c.ink_at(IVec2::new(1, 1)), 1.0);
        assert_eq!(c.ink_at(IVec2::new(W as i32 - 1, 1)), 0.0);
    }
}
