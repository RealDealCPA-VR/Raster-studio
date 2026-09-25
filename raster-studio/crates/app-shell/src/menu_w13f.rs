//! W13-F: Edit ▸ Assign Profile / Convert to Profile…, Image ▸ Reduce
//! Colors… / Wavelet Decompose…, View ▸ Pattern Preview / Clear Slices /
//! Slices from Guides, and the Slice tool's Slices From Guides button.
//!
//! * **Assign Profile ▸ …** re-tags the document with
//!   [`Command::SetMetaColorSpace`], one undo step; no pixel number
//!   changes, so the picture looks different: the canvas is colour-managed
//!   ([`crate::presenter::DisplayTransform`] converts the document's profile
//!   to the sRGB texture on every upload, as Photopea and Photoshop show a
//!   tagged document), and the new tag re-sends the whole canvas.
//! * **Convert to Profile…** asks for the destination, the rendering intent
//!   and black point compensation ([`ui::dialogs::w13f::ConvertProfileDialog`],
//!   parked here by [`W13fDialog::drive`]), then rewrites every pixel layer's
//!   numbers through linear light (`color::icc` for ICC profiles) and re-tags,
//!   all in one [`Command::Transaction`]: one undo restores the numbers and
//!   the tag together. Matrix-shaper profiles carry no perceptual or
//!   saturation tables, so those two intents run as Relative Colorimetric
//!   (the ICC fallback, and what the dialog says); Absolute Colorimetric
//!   scales by the two profiles' `wtpt` media whites
//!   ([`color::icc::absolute_colorimetric`]); black point compensation maps
//!   the source profile's black onto the destination's in linear light, and
//!   is off under Absolute, as in Photoshop. Colours outside the target clip.
//!   Colours the target holds therefore look the same on the canvas after as
//!   before, to the 8-bit rounding of the new numbers (Adobe RGB to sRGB
//!   within 2 codes; to a wider profile, more on saturated colours where
//!   one destination code spans several sRGB codes). A profile the engine
//!   cannot transform (not a matrix-shaper) gets no display conversion.
//! * **Reduce Colors…** asks for a palette source, a count and a dither
//!   ([`ui::dialogs::w13f::ReduceColorsDialog`]) and maps the active layer
//!   onto that palette (`color::quantize`), one undo step.
//! * **Wavelet Decompose…** asks for the scale count
//!   ([`ui::dialogs::w13f::WaveletDialog`]), hides the active layer and puts,
//!   directly above it, a residual (the coarsest blur) and N Linear Light
//!   detail layers, finest on top, as one undo step. The compositor blends
//!   Linear Light in linear light, so each detail pixel is solved against
//!   the compositor's own arithmetic ([`decompose`]) until the stack
//!   recomposites to the source code for code.
//! * **Pattern Preview** is a view flag: [`paint_pattern_preview`] draws the
//!   document's composite repeated around the canvas, under the extras,
//!   through the same display transform (the document's profile to sRGB),
//!   so while no later presenter pass is on, each copy holds the canvas
//!   texture's own bytes and a seam shows only where the pattern has one.
//!   The presenter's later passes (View > Proof Colors, a hidden channel
//!   eye, a mask view) are not applied to the copies, so with any of them
//!   on the canvas differs from its copies and a false seam can show.
//! * **Clear Slices** / **Slices from Guides** replace the slice set through
//!   `slices_export`, which keeps it in history.
//!
//! A row reached without its dialog (a keyboard chord, an Actions replay)
//! runs at the dialog's opening state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use color::ColorSpace;
use editor_core::Command;
use layer_model::{BlendMode, LayerId, LayerKind};
use raster::PixelRect;
use ui::dialogs::w13f::{
    ConvertProfileDialog, ConvertProfileSpec, ReduceColorsDialog, RenderingIntent, WaveletDialog,
    WaveletSpec, WAVELET_SCALES,
};
use ui::dialogs::{DialogOutcome, IndexedSpec};
use ui::menu::{MenuAction, ProfileChoice};
use ui::strings::tr;

use super::pixels;
use crate::chrome::ChromeOutput;
use crate::editor::Editor;

/// `tr(key)` with each `{name}` placeholder filled in.
fn t(key: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = tr(key).to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

fn no_document() -> String {
    tr("ui.w13f.status.no_document").to_string()
}

/// Whether [`perform`] answers `action`.
pub(crate) fn performs(action: MenuAction) -> bool {
    matches!(
        action,
        MenuAction::AssignProfile(_)
            | MenuAction::ConvertToProfile
            | MenuAction::ReduceColors
            | MenuAction::WaveletDecompose
            | MenuAction::ClearSlices
            | MenuAction::SlicesFromGuides
    )
}

/// Perform one W13-F row, with the spec its dialog parked (or the dialog's
/// opening state when it was reached without one).
pub(crate) fn perform(action: MenuAction, editor: &mut Editor) -> Result<String, String> {
    match action {
        MenuAction::AssignProfile(p) => assign_profile(editor, p),
        MenuAction::ConvertToProfile => {
            let spec = match PARKED.with(|p| p.borrow_mut().convert.take()) {
                Some(spec) => spec,
                None => {
                    let doc = editor.active().ok_or_else(no_document)?;
                    ConvertProfileSpec::default_for(ui::menu::W13fFacts::of(&doc.document).profile)
                }
            };
            convert_to_profile(editor, spec)
        }
        MenuAction::ReduceColors => {
            let spec = PARKED
                .with(|p| p.borrow_mut().reduce.take())
                .unwrap_or(ReduceColorsDialog::DEFAULT);
            reduce_colors(editor, spec)
        }
        MenuAction::WaveletDecompose => {
            let spec = PARKED
                .with(|p| p.borrow_mut().wavelet.take())
                .unwrap_or_default();
            wavelet_decompose(editor, spec.scales)
        }
        MenuAction::ClearSlices => clear_slices(editor),
        MenuAction::SlicesFromGuides => slices_from_guides(editor),
        other => Err(t("ui.w13f.status.not_a_row", &[("label", &other.label())])),
    }
}

/// The rows whose perform, in the menu gates' fixture, refuses with the
/// reason rather than editing: a profile from a file cancels at the scripted
/// picker ("No profile file was chosen").
#[cfg(test)]
pub(crate) fn is_loud_without_a_dialog(action: MenuAction) -> bool {
    matches!(action, MenuAction::AssignProfile(ProfileChoice::FromFile))
}

/// The rows whose whole effect is the document's profile tag, which the
/// menu gates' digest does not read (the tag is metadata, not a layer or
/// the selection); `tests::assign_profile_*` pins the effect.
#[cfg(test)]
pub(crate) fn changes_only_the_tag(action: MenuAction) -> bool {
    matches!(action, MenuAction::AssignProfile(_))
}

// ---------------------------------------------------------------------------
// The dialogs
// ---------------------------------------------------------------------------

/// What the three dialogs confirmed, waiting for the menu pick that rides
/// [`ChromeOutput::menu`] out of the same frame (the Trim dialog's shape:
/// a menu action carries no parameters, and only `perform` holds the
/// editor). Thread-local, so two tests cannot hand each other a spec.
#[derive(Default)]
struct Parked {
    convert: Option<ConvertProfileSpec>,
    reduce: Option<IndexedSpec>,
    wavelet: Option<WaveletSpec>,
}

thread_local! {
    static PARKED: RefCell<Parked> = RefCell::new(Parked::default());
}

/// Edit ▸ Convert to Profile…, Image ▸ Reduce Colors… or Image ▸ Wavelet
/// Decompose…, open in the dialog host (`ActiveDialog::W13f`).
#[derive(Debug)]
pub enum W13fDialog {
    Convert(ConvertProfileDialog),
    Reduce(ReduceColorsDialog),
    Wavelet(WaveletDialog),
}

/// Whether `action` opens one of the dialogs here.
pub(crate) fn opens_dialog(action: &MenuAction) -> bool {
    matches!(
        action,
        MenuAction::ConvertToProfile | MenuAction::ReduceColors | MenuAction::WaveletDecompose
    )
}

/// The dialog `action` opens over the active document; `None` with no
/// document, which leaves the arm to say why.
pub(crate) fn dialog_for(action: &MenuAction, editor: &Editor) -> Option<W13fDialog> {
    let doc = editor.active()?;
    Some(match action {
        MenuAction::ConvertToProfile => {
            let current = ui::menu::W13fFacts::of(&doc.document).profile;
            W13fDialog::Convert(ConvertProfileDialog::new(
                current,
                profile_name(&doc.document.meta.color_space),
            ))
        }
        MenuAction::ReduceColors => W13fDialog::Reduce(ReduceColorsDialog::default()),
        MenuAction::WaveletDecompose => W13fDialog::Wavelet(WaveletDialog::default()),
        _ => return None,
    })
}

impl W13fDialog {
    /// Draw one frame. A confirmation parks the spec and pushes the menu
    /// pick that performs it; returns whether the dialog closed.
    pub(crate) fn drive(&mut self, ctx: &egui::Context, out: &mut ChromeOutput) -> bool {
        fn close<S>(
            outcome: DialogOutcome<S>,
            out: &mut ChromeOutput,
            action: MenuAction,
            park: impl FnOnce(&mut Parked, S),
        ) -> bool {
            match outcome {
                DialogOutcome::Open => false,
                DialogOutcome::Cancelled => true,
                DialogOutcome::Confirmed(spec) => {
                    PARKED.with(|p| park(&mut p.borrow_mut(), spec));
                    out.menu.push(action);
                    true
                }
            }
        }
        match self {
            W13fDialog::Convert(d) => {
                close(d.show(ctx), out, MenuAction::ConvertToProfile, |p, s| {
                    p.convert = Some(s)
                })
            }
            W13fDialog::Reduce(d) => close(d.show(ctx), out, MenuAction::ReduceColors, |p, s| {
                p.reduce = Some(s)
            }),
            W13fDialog::Wavelet(d) => {
                close(d.show(ctx), out, MenuAction::WaveletDecompose, |p, s| {
                    p.wavelet = Some(s)
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

#[cfg(test)]
thread_local! {
    /// The file the next "Profile from File…" pick answers, in tests.
    static SCRIPTED_PROFILE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn pick_profile_file() -> Option<PathBuf> {
    SCRIPTED_PROFILE.with(|s| s.borrow_mut().take())
}

#[cfg(not(test))]
fn pick_profile_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter(
            tr("ui.w13f.status.icc_filter"),
            &["icc", "icm", "ICC", "ICM"],
        )
        .set_title(tr("ui.w13f.status.pick_profile"))
        .pick_file()
}

/// The name a colour space goes by in the dialog: the built-in profile's
/// row name, an ICC profile's own description, or the space's name.
fn profile_name(space: &ColorSpace) -> String {
    let facts = |p: ProfileChoice| p.label().to_string();
    match space {
        ColorSpace::Srgb => facts(ProfileChoice::Srgb),
        ColorSpace::DisplayP3 => facts(ProfileChoice::DisplayP3),
        ColorSpace::IccProfile { profile, .. } => {
            if *profile == color::icc::adobe_rgb_1998_profile() {
                facts(ProfileChoice::AdobeRgb)
            } else if *profile == color::icc::prophoto_rgb_profile() {
                facts(ProfileChoice::ProPhotoRgb)
            } else {
                asset_store::resources::icc::parse(profile)
                    .ok()
                    .and_then(|p| p.description)
                    .unwrap_or_else(|| space.name().to_string())
            }
        }
        other => other.name().to_string(),
    }
}

/// The colour space `choice` names, and the name the status line uses.
fn target_space(choice: ProfileChoice) -> Result<(ColorSpace, String), String> {
    let icc = |bytes: Vec<u8>| raster::codec::icc_profile_space(&bytes);
    let file_error = |path: &std::path::Path, error: &dyn std::fmt::Display| {
        t(
            "ui.w13f.status.file_error",
            &[("path", &path.display()), ("error", error)],
        )
    };
    Ok(match choice {
        ProfileChoice::Srgb => (ColorSpace::Srgb, choice.label().to_string()),
        ProfileChoice::DisplayP3 => (ColorSpace::DisplayP3, choice.label().to_string()),
        ProfileChoice::AdobeRgb => (
            icc(color::icc::adobe_rgb_1998_profile()),
            choice.label().to_string(),
        ),
        ProfileChoice::ProPhotoRgb => (
            icc(color::icc::prophoto_rgb_profile()),
            choice.label().to_string(),
        ),
        ProfileChoice::FromFile => {
            let path = pick_profile_file()
                .ok_or_else(|| tr("ui.w13f.status.no_profile_file").to_string())?;
            let len = std::fs::metadata(&path)
                .map_err(|e| file_error(&path, &e))?
                .len();
            if len > asset_store::resources::icc::MAX_ICC_BYTES as u64 {
                return Err(t(
                    "ui.w13f.status.profile_too_big",
                    &[("path", &path.display()), ("len", &len)],
                ));
            }
            let bytes = std::fs::read(&path).map_err(|e| file_error(&path, &e))?;
            let profile =
                asset_store::resources::icc::parse(&bytes).map_err(|e| file_error(&path, &e))?;
            if !profile.is_rgb() {
                let space = String::from_utf8_lossy(&profile.data_space)
                    .trim()
                    .to_string();
                return Err(t(
                    "ui.w13f.status.profile_not_rgb",
                    &[("path", &path.display()), ("space", &space)],
                ));
            }
            let name = profile.description.clone().unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            (icc(profile.bytes), name)
        }
    })
}

/// Encoded RGB of one colour space to and from linear sRGB, with an ICC
/// profile parsed once rather than per pixel.
enum SpaceTransform {
    Builtin(ColorSpace),
    Icc(Box<color::icc::MatrixShaper>),
}

impl SpaceTransform {
    fn new(space: &ColorSpace) -> Result<Self, String> {
        match space {
            ColorSpace::IccProfile { profile, .. } => color::icc::MatrixShaper::parse(profile)
                .map(|m| SpaceTransform::Icc(Box::new(m)))
                .map_err(|e| t("ui.w13f.status.untransformable", &[("error", &e)])),
            other => Ok(SpaceTransform::Builtin(other.clone())),
        }
    }

    fn decode(&self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            SpaceTransform::Builtin(s) => color::to_linear(s, rgb),
            SpaceTransform::Icc(m) => m.to_linear_srgb(rgb),
        }
    }

    fn encode(&self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            SpaceTransform::Builtin(s) => color::from_linear(s, rgb),
            SpaceTransform::Icc(m) => m.from_linear_srgb(rgb),
        }
    }

    /// The media white Absolute Colorimetric scales by: an ICC profile's
    /// `wtpt`, and D65 for the built-in D65 spaces (what their published
    /// version 2 profiles record).
    fn media_white(&self) -> [f32; 3] {
        match self {
            SpaceTransform::Builtin(_) => color::icc::MEDIA_WHITE_D65,
            SpaceTransform::Icc(m) => m.media_white(),
        }
    }

    /// The luminance of this space's black (code 0) in linear light.
    fn black(&self) -> f32 {
        let [r, g, b] = self.decode([0.0; 3]);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// `rgb` encoded in `from` re-encoded in `self`, clipped to the cube.
    #[cfg(test)]
    fn convert(&self, from: &SpaceTransform, rgb: [f32; 3]) -> [f32; 3] {
        clip(self.encode(from.decode(rgb)))
    }
}

fn clip(rgb: [f32; 3]) -> [f32; 3] {
    rgb.map(|c| {
        if c.is_finite() {
            c.clamp(0.0, 1.0)
        } else {
            0.0
        }
    })
}

/// One Convert to Profile transform: source and destination, and what the
/// rendering intent and black point compensation do between them.
struct Conversion {
    from: SpaceTransform,
    to: SpaceTransform,
    /// The (source, destination) media whites, under Absolute Colorimetric.
    absolute: Option<([f32; 3], [f32; 3])>,
    /// The (source, destination) black luminances, under black point
    /// compensation, when they differ.
    black_point: Option<(f32, f32)>,
}

impl Conversion {
    fn new(from: SpaceTransform, to: SpaceTransform, spec: &ConvertProfileSpec) -> Self {
        let absolute = (spec.intent == RenderingIntent::AbsoluteColorimetric)
            .then(|| (from.media_white(), to.media_white()));
        let blacks = (from.black(), to.black());
        let black_point = (spec.black_point
            && absolute.is_none()
            && (blacks.0 - blacks.1).abs() > 1e-6
            && blacks.0 < 1.0)
            .then_some(blacks);
        Self {
            from,
            to,
            absolute,
            black_point,
        }
    }

    fn convert(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mut lin = self.from.decode(rgb);
        if let Some((from, to)) = self.absolute {
            lin = color::icc::absolute_colorimetric(lin, from, to);
        }
        if let Some((source, dest)) = self.black_point {
            lin = lin.map(|c| dest + (c - source) * (1.0 - dest) / (1.0 - source));
        }
        clip(self.to.encode(lin))
    }
}

/// Every pixel of an RGBA buffer through `conversion`; alpha is kept and a
/// fully transparent pixel is left alone.
fn convert_pixels<S: Copy + Eq + std::hash::Hash>(
    pixels: &[S],
    conversion: &Conversion,
    unit: impl Fn(S) -> f32,
    code: impl Fn(f32) -> S,
    transparent: impl Fn(S) -> bool,
) -> Vec<S> {
    let mut out = pixels.to_vec();
    let mut cache: HashMap<[S; 3], [S; 3]> = HashMap::new();
    for px in out.as_chunks_mut::<4>().0.iter_mut() {
        if transparent(px[3]) {
            continue;
        }
        let key = [px[0], px[1], px[2]];
        let new = *cache
            .entry(key)
            .or_insert_with(|| conversion.convert(key.map(&unit)).map(&code));
        px[..3].copy_from_slice(&new);
    }
    out
}

fn rgb_document(editor: &Editor) -> Result<(), String> {
    let doc = editor.active().ok_or_else(no_document)?;
    if doc.document.meta.color_mode != 0 {
        return Err(tr("ui.w13f.status.needs_rgb_document").to_string());
    }
    Ok(())
}

/// Apply `command` and answer whether it reached the history.
fn applied(
    editor: &mut Editor,
    command: Command,
    label: &str,
    fallback: &str,
) -> Result<(), String> {
    let steps = editor.active().map_or(0, |d| d.history_depth());
    editor.apply_command(command);
    if editor.active().map_or(0, |d| d.history_depth()) == steps {
        let reason = editor.status().unwrap_or(fallback).to_string();
        return Err(t(
            "ui.w13f.status.refused",
            &[("label", &label), ("reason", &reason)],
        ));
    }
    Ok(())
}

fn assign_profile(editor: &mut Editor, choice: ProfileChoice) -> Result<String, String> {
    rgb_document(editor)?;
    let (space, name) = target_space(choice)?;
    let doc = editor.active().ok_or_else(no_document)?;
    if doc.document.meta.color_space == space {
        return Err(t("ui.w13f.status.already_tagged", &[("name", &name)]));
    }
    applied(
        editor,
        Command::SetMetaColorSpace { space },
        tr("ui.w13f.menu.assign_profile"),
        tr("ui.w13f.status.layer_not_rewritten"),
    )?;
    Ok(t("ui.w13f.status.assigned", &[("name", &name)]))
}

fn convert_to_profile(editor: &mut Editor, spec: ConvertProfileSpec) -> Result<String, String> {
    rgb_document(editor)?;
    let (current, depth) = {
        let doc = editor.active().ok_or_else(no_document)?;
        (
            doc.document.meta.color_space.clone(),
            doc.document.meta.bit_depth,
        )
    };
    if depth == 32 {
        return Err(tr("ui.w13f.why.convert_depth").to_string());
    }
    let from = SpaceTransform::new(&current)
        .map_err(|e| t("ui.w13f.status.own_profile", &[("error", &e)]))?;
    let (space, name) = target_space(spec.target)?;
    if space == current {
        return Err(t("ui.w13f.status.already_in", &[("name", &name)]));
    }
    let to = SpaceTransform::new(&space).map_err(|e| format!("{name}: {e}"))?;
    let conversion = Conversion::new(from, to, &spec);
    let label = tr("ui.w13f.convert.title");
    let command = {
        let doc = editor.active_mut().ok_or_else(no_document)?;
        let ids: Vec<LayerId> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| doc.document.layer_tiles(*id).is_some())
            .collect();
        let mut commands = Vec::new();
        for id in ids {
            if doc.is_sixteen_bit() {
                let before = doc.layer_rgba16(id);
                let after = convert_pixels(
                    &before,
                    &conversion,
                    |c| f32::from(c) / 65535.0,
                    |v| (v * 65535.0).round() as u16,
                    |a| a == 0,
                );
                if after != before {
                    commands.push(doc.layer_rgba16_command(id, &after, label)?);
                }
            } else {
                let before = pixels::read_layer(doc, id);
                let after = convert_pixels(
                    &before,
                    &conversion,
                    |c| f32::from(c) / 255.0,
                    |v| (v * 255.0).round() as u8,
                    |a| a == 0,
                );
                if after != before {
                    commands.push(pixels::write_layer(doc, id, &after, label)?);
                }
            }
        }
        // The tag rides the same step as the numbers, so one undo puts the
        // document back under the profile its old numbers belong to.
        commands.push(Command::SetMetaColorSpace { space });
        Command::Transaction {
            label: label.to_string(),
            commands,
        }
    };
    applied(
        editor,
        command,
        label,
        tr("ui.w13f.status.layer_not_rewritten"),
    )?;
    Ok(t(
        "ui.w13f.status.converted",
        &[("name", &name), ("intent", &spec.intent.label())],
    ))
}

// ---------------------------------------------------------------------------
// Reduce Colors
// ---------------------------------------------------------------------------

/// The active layer, when it owns pixels this route may rewrite.
fn active_pixel_layer(editor: &Editor) -> Result<LayerId, String> {
    let doc = editor.active().ok_or_else(no_document)?;
    let id = doc
        .document
        .active_layer()
        .ok_or_else(|| tr("ui.w13f.status.select_layer").to_string())?;
    let layer = doc
        .document
        .layers
        .get(id)
        .ok_or_else(|| tr("ui.w13f.status.layer_missing").to_string())?;
    match &layer.kind {
        LayerKind::Raster(_) | LayerKind::Generator(_) => {}
        other => {
            return Err(t(
                "ui.w13f.status.not_pixel_layer",
                &[("kind", &editor_core::layer_class_name(other))],
            ))
        }
    }
    if layer.locked.blocks_pixel_edit() {
        return Err(tr("ui.w13f.status.locked").to_string());
    }
    if doc.document.meta.bit_depth != 8 {
        return Err(tr("ui.w13f.why.needs_8bit").to_string());
    }
    if doc.document.meta.color_mode != 0 {
        return Err(tr("ui.w13f.why.needs_rgb").to_string());
    }
    Ok(id)
}

fn reduce_colors(editor: &mut Editor, spec: IndexedSpec) -> Result<String, String> {
    use color::quantize::{build_palette, remap_rgba8, Histogram, PaletteKind};
    let label = tr("ui.w13f.reduce.title");
    let layer = active_pixel_layer(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or_else(no_document)?;
        let (w, h) = (doc.document.width(), doc.document.height());
        let before = pixels::read_layer(doc, layer);
        let mut histogram = Histogram::new();
        histogram.add_rgba8(&before);
        let palette = build_palette(
            &histogram,
            spec.palette,
            spec.colors
                .clamp(color::quantize::MIN_COLORS, color::quantize::MAX_COLORS),
        )
        .map_err(|e| format!("{label}: {e}"))?;
        let mut after = before.clone();
        remap_rgba8(&mut after, w as usize, &palette, spec.dither);
        let selection = doc.document.selection.clone();
        pixels::mask_by_selection(&before, &mut after, &selection, w, h);
        if after == before {
            return Err(tr("ui.w13f.status.reduce_nothing").to_string());
        }
        crate::fade::remember(doc.id(), layer, label, &before, &after);
        pixels::write_layer(doc, layer, &after, label)?
    };
    editor.apply_command(command);
    let n = if spec.palette == PaletteKind::Web {
        216
    } else {
        spec.colors
    };
    Ok(t("ui.w13f.status.reduced", &[("n", &n)]))
}

// ---------------------------------------------------------------------------
// Wavelet Decompose
// ---------------------------------------------------------------------------

/// How far from the greedy choice the residual (`R`) and the second-finest
/// layer (`P`) are searched, in codes, for a pixel whose stack does not yet
/// land on its source code.
const SEARCH_RESIDUAL: i32 = 24;
const SEARCH_PENULTIMATE: i32 = 8;

/// Linear light of each 8-bit sRGB code, exactly as the compositor decodes
/// a stored tile (`color::to_linear` over `code / 255`).
fn linear_lut() -> [f32; 256] {
    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        *slot = color::to_linear(&ColorSpace::Srgb, [i as f32 / 255.0; 3])[0];
    }
    lut
}

/// The 8-bit code the compositor writes for linear value `v`
/// (`compositor::Canvas::to_rgba8`: encode, then round).
fn output_code(v: f32) -> i32 {
    let e = color::from_linear(&ColorSpace::Srgb, [v; 3])[0];
    let e = if e.is_finite() {
        e.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (e * 255.0).round() as i32
}

/// The code whose linear value is nearest `v`.
fn nearest_code(v: f32) -> i32 {
    output_code(v.clamp(0.0, 1.0))
}

/// One Linear Light layer at full opacity over an opaque backdrop, as the
/// compositor evaluates it: `unit(b + 2s - 1)`.
fn linear_light(backdrop: f32, source: f32) -> f32 {
    BlendMode::LinearLight.blend_channel(backdrop, source)
}

/// Three passes of a clamped-edge box blur of radius `r` over one plane.
fn box_blur3(plane: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    fn pass(
        src: &[f32],
        dst: &mut [f32],
        len: usize,
        stride: usize,
        lines: usize,
        step: usize,
        r: usize,
    ) {
        let norm = 1.0 / (2 * r + 1) as f32;
        for line in 0..lines {
            let base = line * step;
            let at = |i: isize| src[base + (i.clamp(0, len as isize - 1) as usize) * stride];
            let mut sum = 0.0f32;
            for i in -(r as isize)..=(r as isize) {
                sum += at(i);
            }
            for i in 0..len {
                dst[base + i * stride] = sum * norm;
                sum += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
            }
        }
    }
    let mut a = plane.to_vec();
    let mut b = vec![0.0f32; plane.len()];
    for _ in 0..3 {
        pass(&a, &mut b, w, 1, h, w, r);
        pass(&b, &mut a, h, w, w, 1, r);
    }
    a
}

/// Split an 8-bit sRGB RGBA image into a residual and `scales` detail
/// images (finest first) that recomposite to it: the residual as a Normal
/// layer, each detail as a Linear Light layer above it, coarsest lowest.
///
/// Scale `k` is the difference of two box-Gaussian blurs (radii `2^(k-2)`
/// and `2^(k-1)`, scale 1 against the image itself), all in linear light,
/// where the compositor blends. Each layer's codes are chosen greedily
/// against its blur, the finest against the source code; a pixel the greedy
/// pass leaves off its code is searched over nearby residual and
/// second-finest codes until the stack lands on it. Alpha is carried
/// unchanged into every layer, so the recomposition is exact where the
/// source is opaque.
pub(crate) fn decompose(rgba: &[u8], w: usize, h: usize, scales: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
    let lut = linear_lut();
    let n = w * h;
    let mut residual = vec![0u8; n * 4];
    let mut details = vec![vec![0u8; n * 4]; scales];
    for p in 0..n {
        let a = rgba[p * 4 + 3];
        residual[p * 4 + 3] = a;
        for d in details.iter_mut() {
            d[p * 4 + 3] = a;
        }
    }
    for c in 0..3 {
        let source: Vec<f32> = (0..n).map(|p| lut[rgba[p * 4 + c] as usize]).collect();
        let blur = |k: usize| -> Vec<f32> {
            if k == 0 {
                source.clone()
            } else {
                box_blur3(&source, w, h, 1 << (k - 1))
            }
        };
        // The residual: the coarsest blur.
        let coarsest = blur(scales);
        let mut value: Vec<f32> = Vec::with_capacity(n);
        for p in 0..n {
            let code = nearest_code(coarsest[p]);
            residual[p * 4 + c] = code as u8;
            value.push(lut[code as usize]);
        }
        drop(coarsest);
        // Coarsest detail first, each aimed at the next finer blur.
        for k in (1..=scales).rev() {
            let target = blur(k - 1);
            let layer = &mut details[k - 1];
            for p in 0..n {
                let v = value[p];
                let ideal = nearest_code((target[p] - v + 1.0) * 0.5);
                let (code, out) = (ideal - 2..=ideal + 2)
                    .map(|s| s.clamp(0, 255))
                    .map(|s| (s, linear_light(v, lut[s as usize])))
                    .min_by(|x, y| (x.1 - target[p]).abs().total_cmp(&(y.1 - target[p]).abs()))
                    .expect("five candidates");
                layer[p * 4 + c] = code as u8;
                value[p] = out;
            }
        }
        // Land every opaque pixel on its own code.
        for p in 0..n {
            let want = i32::from(rgba[p * 4 + c]);
            if rgba[p * 4 + 3] == 0 || output_code(value[p]) == want {
                continue;
            }
            let mut codes: Vec<i32> = std::iter::once(i32::from(residual[p * 4 + c]))
                .chain(
                    (1..=scales)
                        .rev()
                        .map(|k| i32::from(details[k - 1][p * 4 + c])),
                )
                .collect();
            if let Some(better) = land(&codes, want, &lut) {
                codes = better;
                residual[p * 4 + c] = codes[0] as u8;
                for (i, k) in (1..=scales).rev().enumerate() {
                    details[k - 1][p * 4 + c] = codes[i + 1] as u8;
                }
            }
        }
    }
    (residual, details)
}

/// The stack's output for `codes` (residual first, finest last) with the
/// finest code chosen to land nearest `want`; answers the full code list and
/// how far from `want` it lands.
fn best_finest(codes: &mut [i32], want: i32, lut: &[f32; 256]) -> i32 {
    let last = codes.len() - 1;
    let mut v = lut[codes[0] as usize];
    for s in &codes[1..last] {
        v = linear_light(v, lut[*s as usize]);
    }
    let ideal = nearest_code((lut[want as usize] - v + 1.0) * 0.5);
    let mut best = (i32::MAX, codes[last]);
    for s in (ideal - 3..=ideal + 3).map(|s| s.clamp(0, 255)) {
        let miss = (output_code(linear_light(v, lut[s as usize])) - want).abs();
        if miss < best.0 {
            best = (miss, s);
        }
    }
    codes[last] = best.1;
    best.0
}

/// Search the residual and the second-finest code near the greedy ones for
/// a combination that lands exactly on `want`; `None` when nothing nearby
/// does better than the greedy stack.
fn land(greedy: &[i32], want: i32, lut: &[f32; 256]) -> Option<Vec<i32>> {
    let penultimate = greedy.len() - 2;
    let mut start = greedy.to_vec();
    let mut best_miss = best_finest(&mut start, want, lut);
    let mut best = start;
    if best_miss == 0 {
        return Some(best);
    }
    let p_range = if penultimate == 0 {
        0..=0
    } else {
        -SEARCH_PENULTIMATE..=SEARCH_PENULTIMATE
    };
    for dr in (0..=SEARCH_RESIDUAL).flat_map(|d| [d, -d]) {
        for dp in p_range.clone() {
            let mut codes = greedy.to_vec();
            codes[0] = (codes[0] + dr).clamp(0, 255);
            if penultimate > 0 {
                codes[penultimate] = (codes[penultimate] + dp).clamp(0, 255);
            }
            let miss = best_finest(&mut codes, want, lut);
            if miss < best_miss {
                best_miss = miss;
                best = codes;
                if miss == 0 {
                    return Some(best);
                }
            }
        }
    }
    Some(best)
}

/// A new raster layer holding `rgba`, placed at `index` of `parent`: the
/// commands, and its id.
fn layer_commands(
    doc: &mut crate::doc::OpenDocument,
    mut layer: layer_model::Layer,
    rgba: &[u8],
    parent: Option<LayerId>,
    index: usize,
) -> Result<(Vec<Command>, LayerId), String> {
    let (w, h) = (doc.document.width(), doc.document.height());
    let id = layer.id;
    layer.visible = true;
    let mut commands = vec![Command::create_layer(layer)];
    let grid = raster::TileGrid::from_rgba8(w, h, rgba).map_err(|e| e.to_string())?;
    let mut edits = Vec::new();
    for (coord, tile) in grid.iter() {
        let hash = doc.tiles.insert_bytes(tile.data().to_vec());
        edits.push(editor_core::pixels::TileEdit::set(coord, hash));
    }
    if !edits.is_empty() {
        commands.push(
            Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(id), edits)
                .map_err(|e| e.to_string())?,
        );
    }
    commands.push(Command::MoveLayer {
        layer_id: id,
        parent,
        index,
    });
    Ok((commands, id))
}

fn wavelet_decompose(editor: &mut Editor, scales: u8) -> Result<String, String> {
    let label = tr("ui.w13f.wavelet.title");
    if !WAVELET_SCALES.contains(&scales) {
        return Err(tr("ui.w13f.wavelet.bad_count").to_string());
    }
    let source = active_pixel_layer(editor)?;
    let (command, top) = {
        let doc = editor.active_mut().ok_or_else(no_document)?;
        // The menu greys the row for the same reason
        // (`ui::menu::wavelet_needs_srgb`): the split is solved per channel
        // against the sRGB decode, which any other profile's matrix mixes.
        if doc.document.meta.color_space != ColorSpace::Srgb {
            return Err(t(
                "ui.w13f.status.wavelet_other_space",
                &[
                    ("reason", &ui::menu::wavelet_needs_srgb()),
                    ("space", &profile_name(&doc.document.meta.color_space)),
                ],
            ));
        }
        let (w, h) = (
            doc.document.width() as usize,
            doc.document.height() as usize,
        );
        let rgba = pixels::read_layer(doc, source);
        if rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            return Err(tr("ui.w13f.status.wavelet_empty").to_string());
        }
        let layer = doc
            .document
            .layers
            .get(source)
            .ok_or_else(|| tr("ui.w13f.status.layer_missing").to_string())?;
        let (name, transform) = (layer.name.clone(), layer.transform);
        let parent = doc.document.layers.parent_of(source);
        let index = doc.document.layers.index_in_parent(source).unwrap_or(0);
        let (residual, details) = decompose(&rgba, w, h, usize::from(scales));

        let mut commands = vec![Command::SetLayerProperties {
            layer_id: source,
            patch: editor_core::LayerPatch {
                visible: Some(false),
                ..Default::default()
            },
        }];
        let mut base = layer_model::Layer::raster(t("ui.w13f.status.residual", &[("name", &name)]));
        base.transform = transform;
        let (more, _) = layer_commands(doc, base, &residual, parent, index)?;
        commands.extend(more);
        let mut top = source;
        for k in (1..=usize::from(scales)).rev() {
            let mut detail = layer_model::Layer::raster(t(
                "ui.w13f.status.scale",
                &[("name", &name), ("k", &k)],
            ));
            detail.transform = transform;
            detail.blend_mode = BlendMode::LinearLight;
            let (more, id) = layer_commands(doc, detail, &details[k - 1], parent, index)?;
            commands.extend(more);
            top = id;
        }
        (
            Command::Transaction {
                label: label.to_string(),
                commands,
            },
            top,
        )
    };
    applied(
        editor,
        command,
        label,
        tr("ui.w13f.status.layers_not_added"),
    )?;
    editor.set_layer_selection(vec![top], Some(top));
    Ok(t("ui.w13f.status.decomposed", &[("n", &scales)]))
}

// ---------------------------------------------------------------------------
// Slices
// ---------------------------------------------------------------------------

fn clear_slices(editor: &mut Editor) -> Result<String, String> {
    let id = editor.active().ok_or_else(no_document)?.id();
    crate::slices_export::restore_saved_slices(editor);
    let count = editor.slices.get(id).len();
    if count == 0 {
        return Err(tr("ui.w13f.why.no_slices").to_string());
    }
    editor.slices.remember(id, Vec::new());
    crate::slices_export::persist_slices(editor);
    Ok(t("ui.w13f.status.cleared_slices", &[("n", &count)]))
}

/// The cell edges the guides of one axis cut `0..len` into: 0, every guide
/// strictly inside (rounded to a pixel, once each), `len`.
fn cuts(positions: impl Iterator<Item = f32>, len: u32) -> Vec<i64> {
    let mut out: Vec<i64> = positions
        .filter(|p| p.is_finite())
        .map(|p| p.round() as i64)
        .filter(|p| *p > 0 && *p < i64::from(len))
        .collect();
    out.push(0);
    out.push(i64::from(len));
    out.sort_unstable();
    out.dedup();
    out
}

fn slices_from_guides(editor: &mut Editor) -> Result<String, String> {
    let (xs, ys) = {
        let doc = editor.active().ok_or_else(no_document)?;
        let guides = &doc.document.guides.list;
        if guides.is_empty() {
            return Err(tr("ui.w13f.why.no_guides").to_string());
        }
        let along = |axis: editor_core::GuideAxis| {
            guides.iter().filter(move |g| g.axis == axis).map(|g| g.doc)
        };
        (
            cuts(
                along(editor_core::GuideAxis::Vertical),
                doc.document.width(),
            ),
            cuts(
                along(editor_core::GuideAxis::Horizontal),
                doc.document.height(),
            ),
        )
    };
    if xs.len() == 2 && ys.len() == 2 {
        return Err(tr("ui.w13f.status.no_guide_crosses").to_string());
    }
    let mut slices = Vec::new();
    for row in ys.windows(2) {
        for col in xs.windows(2) {
            slices.push(tools::Slice {
                rect: PixelRect::new(
                    col[0],
                    row[0],
                    (col[1] - col[0]) as u32,
                    (row[1] - row[0]) as u32,
                ),
                name: String::new(),
            });
        }
    }
    let count = slices.len();
    crate::slices_export::remember_committed(editor, &slices);
    Ok(t(
        "ui.w13f.status.sliced",
        &[
            ("n", &count),
            ("cols", &(xs.len() - 1)),
            ("rows", &(ys.len() - 1)),
        ],
    ))
}

// ---------------------------------------------------------------------------
// View > Pattern Preview
// ---------------------------------------------------------------------------

/// The longest edge, in pixels, of the picture the pattern copies are drawn
/// from: the document's composite, sampled down to this when larger.
pub(crate) const PATTERN_PREVIEW_MAX_PX: u32 = 2048;

/// How many copies out from the document, in each direction, at most: the
/// view zoomed far out stops tiling there rather than drawing thousands.
const PATTERN_PREVIEW_REACH: i64 = 12;

/// What the cached picture was built from: the document, its content
/// revision, its size and (a hash of) the profile the display transform
/// decoded.
type PreviewKey = (crate::doc::DocumentId, u64, u32, u32, u64);

/// A hash of `space`: its kind, and an ICC profile's content hash.
fn space_key(space: &ColorSpace) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(space).hash(&mut h);
    if let ColorSpace::IccProfile { asset_hash, .. } = space {
        asset_hash.hash(&mut h);
    }
    h.finish()
}

fn preview_id() -> egui::Id {
    egui::Id::new("raster-w13f-pattern-preview")
}

/// The pattern preview's picture as last built, for tests to read.
#[cfg(test)]
pub(crate) fn pattern_preview_image(
    ctx: &egui::Context,
) -> Option<std::sync::Arc<egui::ColorImage>> {
    ctx.data(|d| d.get_temp(preview_id().with("image")))
}

/// The texture the pattern copies draw with, (re)built when the document's
/// content, size or profile moved: its composite, through the presenter's
/// own display transform ([`crate::presenter::DisplayTransform`], the
/// document's profile to sRGB), so each copy holds the bytes the canvas
/// texture holds while no later presenter pass is on; then sampled to at
/// most [`PATTERN_PREVIEW_MAX_PX`] on its long edge. Only the display step
/// is applied: the presenter's proof, channel-mask and mask-view passes
/// (`CanvasPresenter::composite_masked`, after the display step) are not,
/// so with View > Proof Colors, a hidden channel eye or a mask view on the
/// copies differ from the canvas.
fn pattern_texture(
    ctx: &egui::Context,
    editor: &mut Editor,
) -> Option<(egui::TextureHandle, u32, u32)> {
    let revision = editor.content_revision();
    let doc = editor.active_mut()?;
    let rect = doc.canvas_rect();
    let (w, h) = (rect.width, rect.height);
    if w == 0 || h == 0 {
        return None;
    }
    let key: PreviewKey = (
        doc.id(),
        revision,
        w,
        h,
        space_key(&doc.document.meta.color_space),
    );
    let held: Option<(PreviewKey, egui::TextureHandle)> = ctx.data(|d| d.get_temp(preview_id()));
    if let Some((k, texture)) = held {
        if k == key {
            return Some((texture, w, h));
        }
    }
    let mut rgba = doc.composite(rect).ok()?;
    crate::presenter::DisplayTransform::new(&doc.document.meta.color_space).apply(&mut rgba);
    let step = w.max(h).div_ceil(PATTERN_PREVIEW_MAX_PX).max(1);
    let (ow, oh) = (w.div_ceil(step), h.div_ceil(step));
    let mut pixels = Vec::with_capacity((ow * oh) as usize);
    for oy in 0..oh {
        for ox in 0..ow {
            let (x, y) = ((ox * step).min(w - 1), (oy * step).min(h - 1));
            let at = ((y * w + x) * 4) as usize;
            let px = &rgba[at..at + 4];
            pixels.push(egui::Color32::from_rgba_unmultiplied(
                px[0], px[1], px[2], px[3],
            ));
        }
    }
    let image = egui::ColorImage {
        size: [ow as usize, oh as usize],
        pixels,
    };
    #[cfg(test)]
    ctx.data_mut(|d| {
        d.insert_temp(
            preview_id().with("image"),
            std::sync::Arc::new(image.clone()),
        )
    });
    let texture = ctx.load_texture("w13f-pattern-preview", image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(preview_id(), (key, texture.clone())));
    Some((texture, w, h))
}

/// W13-F: View ▸ Pattern Preview. While the flag is ticked, the document's
/// composite is drawn again in every cell of the grid the canvas tiles
/// (up to [`PATTERN_PREVIEW_REACH`] copies out), through the document's own
/// camera, so a seamless pattern's seams show where the copies meet. Painted
/// on the extras' layer before them, so rulers, guides and handles sit over
/// the copies; clipped to the rectangle the docks left.
pub(crate) fn paint_pattern_preview(
    ctx: &egui::Context,
    flags: ui::ViewFlags,
    editor: &mut Editor,
) {
    if !flags.get(ui::ViewFlag::PatternPreview) {
        ctx.data_mut(|d| d.remove::<(PreviewKey, egui::TextureHandle)>(preview_id()));
        return;
    }
    let Some((texture, w, h)) = pattern_texture(ctx, editor) else {
        return;
    };
    let Some(doc) = editor.active() else {
        return;
    };
    let camera = crate::tool_input::canvas_camera_of(&doc.camera);
    let viewport = crate::tool_input::canvas_viewport(&doc.camera);
    let content = ctx.available_rect();
    if !(content.width() > 0.0 && content.height() > 0.0) {
        return;
    }
    let painter = ctx
        .layer_painter(crate::canvas_extras::overlay_layer())
        .with_clip_rect(content);
    // Which cells the visible area reaches, from its corners in document
    // space (a turned view makes that a bounding box).
    let corners = [
        content.left_top(),
        content.right_top(),
        content.right_bottom(),
        content.left_bottom(),
    ]
    .map(|p| camera.doc_of_screen_pt(&viewport, glam::Vec2::new(p.x, p.y)));
    if !corners.iter().all(|c| c.is_finite()) {
        return;
    }
    let (fw, fh) = (w as f32, h as f32);
    let span = |lo: f32, hi: f32, len: f32| {
        let a = (lo / len).floor() as i64;
        let b = (hi / len).floor() as i64;
        (
            a.clamp(-PATTERN_PREVIEW_REACH, PATTERN_PREVIEW_REACH),
            b.clamp(-PATTERN_PREVIEW_REACH, PATTERN_PREVIEW_REACH),
        )
    };
    let min = corners
        .iter()
        .fold(glam::Vec2::splat(f32::MAX), |a, c| a.min(*c));
    let max = corners
        .iter()
        .fold(glam::Vec2::splat(f32::MIN), |a, c| a.max(*c));
    let (i0, i1) = span(min.x, max.x, fw);
    let (j0, j1) = span(min.y, max.y, fh);
    for j in j0..=j1 {
        for i in i0..=i1 {
            if i == 0 && j == 0 {
                continue;
            }
            let (x0, y0) = (i as f32 * fw, j as f32 * fh);
            let (x1, y1) = (x0 + fw, y0 + fh);
            let mut mesh = egui::Mesh::with_texture(texture.id());
            for (p, uv) in [
                (glam::Vec2::new(x0, y0), egui::pos2(0.0, 0.0)),
                (glam::Vec2::new(x1, y0), egui::pos2(1.0, 0.0)),
                (glam::Vec2::new(x1, y1), egui::pos2(1.0, 1.0)),
                (glam::Vec2::new(x0, y1), egui::pos2(0.0, 1.0)),
            ] {
                let s = camera.screen_pt_of(&viewport, p);
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: egui::pos2(s.x, s.y),
                    uv,
                    color: egui::Color32::WHITE,
                });
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(egui::Shape::mesh(mesh));
        }
    }
}

#[cfg(test)]
#[path = "menu_w13f_tests.rs"]
mod tests;
