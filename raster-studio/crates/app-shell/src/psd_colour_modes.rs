//! W16-B: a `.psd` / `.psb` in any Photoshop colour mode opens into the
//! matching document mode, and Save as PSD writes CMYK, Lab, Indexed and
//! Greyscale documents in their own mode.
//!
//! The `psd` crate knows where each mode keeps its samples
//! (`psd::colour_modes`); the colour science is this build's own, the same
//! the Image > Mode conversions, Info readout and proofing use:
//!
//! * **CMYK** — `color::cmyk` (the documented ink model, no ICC press
//!   profile) both ways, so a CMYK document saved and reopened keeps its
//!   pixels, and a separation this model produced comes back as the same ink
//!   numbers. An ink split the model would not choose (rich black, say) opens
//!   as its colour and saves as this model's split of that colour.
//! * **Lab** — `color::model` (CIELAB, D65), 8 or 16 bits.
//! * **Indexed** — the palette and transparent index in the file; saving
//!   writes the document's own colours as the palette (median cut past 256).
//! * **Bitmap** — 1-bit black and white; saved as Greyscale (the `psd`
//!   writer does not pack bits).
//! * **Duotone** — the greyscale base printed through the file's inks
//!   (`color::duotone`), when the ink record can be read; the greyscale base
//!   otherwise. Saved as RGB with the inks applied: the ink record is not
//!   written back.
//! * **Multichannel** — no document mode here: its first three inks show as
//!   C, M, Y in an RGB document (one ink as Greyscale); further channels are
//!   left out and named in the report.
//!
//! The editor's tiles are 8-bit RGBA in every mode (16-bit only for RGB and
//! grey sources), so a 16-bit CMYK / Lab file converts at 8-bit precision; the
//! report says so.

use std::collections::HashMap;

use editor_core::color_mode::mode;
use editor_core::Document;

use super::{ImportError, PsdNotes};

/// What opening a file in its own mode decided.
pub(super) struct OpenedMode {
    /// The document mode (`editor_core::color_mode::mode`).
    pub mode: u8,
    /// A 16-bit source: the document keeps working at 16 bits.
    pub deep: bool,
}

fn unit8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn cmyk_to_rgb(ink: [f32; 4]) -> [u8; 3] {
    color::cmyk::cmyk_to_rgb8(color::cmyk::Cmyk {
        c: ink[0],
        m: ink[1],
        y: ink[2],
        k: ink[3],
    })
}

fn lab_to_rgb(lab: [f32; 3]) -> [u8; 3] {
    color::model::lab_to_rgb(lab).map(unit8)
}

fn rgb_to_lab(rgb: [u8; 3]) -> [f32; 3] {
    color::model::rgb_to_lab(rgb.map(|v| f32::from(v) / 255.0))
}

/// One duotone ink's colour as sRGB, or `None` for a colour-book ink whose
/// components are a book id rather than a colour.
fn ink_rgb(color: psd::colour_modes::InkColor) -> Option<[u8; 3]> {
    use psd::colour_modes::InkColor;
    Some(match color {
        InkColor::Rgb(c) => c.map(|v| (v >> 8) as u8),
        InkColor::Cmyk(c) => {
            let ink = c.map(|v| 1.0 - f32::from(v) / 65535.0);
            cmyk_to_rgb(ink)
        }
        InkColor::Lab { l, a, b } => lab_to_rgb([
            f32::from(l) / 100.0,
            f32::from(a) / 100.0,
            f32::from(b) / 100.0,
        ]),
        InkColor::Gray(g) => [unit8(1.0 - f32::from(g) / 10000.0); 3],
        InkColor::Other { .. } => return None,
    })
}

/// The Duotone print table for a file's ink record, or the reason there is
/// none (the document then opens as its greyscale base).
fn duotone_lut(file: &psd::PsdFile, notes: &mut PsdNotes) -> Option<[[u8; 3]; 256]> {
    let record = match psd::colour_modes::DuotoneRecord::parse(&file.color_mode_data) {
        Ok(record) => record,
        Err(why) => {
            notes.push(format!(
                "the Duotone ink record could not be read ({why}); the document opened as its \
                 Grayscale base"
            ));
            return None;
        }
    };
    let mut inks = Vec::with_capacity(record.inks.len());
    for (i, ink) in record.inks.iter().enumerate() {
        let colour = ink_rgb(ink.color).unwrap_or_else(|| {
            notes.push(format!(
                "Duotone ink {} ({}) is a colour-book ink this build cannot look up; it is \
                 shown with a stand-in colour",
                i + 1,
                ink.name
            ));
            color::duotone::DEFAULT_INKS[i.min(3)]
        });
        inks.push(color::duotone::DuotoneInk {
            color: colour,
            curve: ink.curve.clone(),
        });
    }
    Some(color::duotone::DuotoneSpec { inks }.lut())
}

/// Convert `file` (as read) into the editor's working form and say which
/// document mode it opens in.
pub(super) fn open_in_mode(
    file: &mut psd::PsdFile,
    notes: &mut PsdNotes,
) -> Result<OpenedMode, ImportError> {
    use psd::ColorMode as M;
    let source = file.header.color_mode;
    let lut = if source == M::Duotone {
        duotone_lut(file, notes)
    } else {
        None
    };
    let science = psd::colour_modes::WorkingScience {
        cmyk_to_rgb: &cmyk_to_rgb,
        lab_to_rgb: &lab_to_rgb,
        duotone: lut.as_ref(),
    };
    let out = psd::colour_modes::to_working_rgb(file, &science)?;
    let converted = !matches!(source, M::Rgb | M::Grayscale);
    if converted && out.source_depth == psd::Depth::Sixteen {
        notes.push(format!(
            "this 16-bit {} document is edited at 8-bit precision per channel",
            psd::header::mode_name(source.code())
        ));
    }
    let mode = match source {
        M::Rgb => mode::RGB,
        M::Grayscale => mode::GRAYSCALE,
        M::Cmyk => mode::CMYK,
        M::Lab => mode::LAB,
        M::Indexed => mode::INDEXED,
        M::Bitmap => mode::BITMAP,
        M::Duotone if lut.is_some() => mode::DUOTONE,
        M::Duotone => mode::GRAYSCALE,
        M::Multichannel => {
            let shown = if file.header.color_mode == M::Rgb {
                "its first three inks show as cyan, magenta and yellow in an RGB document"
            } else {
                "its ink shows as a Grayscale document"
            };
            let mut note = format!("Multichannel has no document mode here: {shown}");
            if out.dropped_channels > 0 {
                note.push_str(&format!(
                    "; {} further channel(s) were left out",
                    out.dropped_channels
                ));
            }
            notes.push(note);
            if file.header.color_mode == M::Rgb {
                mode::RGB
            } else {
                mode::GRAYSCALE
            }
        }
    };
    Ok(OpenedMode {
        mode,
        deep: out.source_depth == psd::Depth::Sixteen,
    })
}

/// Turn the RGB `file` built from `document` into the document's own mode
/// before it is written. `composite_rgba8` is the document's flattened
/// composite (the source of an Indexed palette).
pub(super) fn save_in_mode(
    document: &Document,
    composite_rgba8: &[u8],
    file: &mut psd::PsdFile,
    notes: &mut PsdNotes,
) -> Result<(), ImportError> {
    use psd::colour_modes::{from_working_rgb, Separation};
    match document.meta.color_mode {
        mode::GRAYSCALE => from_working_rgb(file, Separation::Grayscale)?,
        mode::BITMAP => {
            notes.push(
                "a Bitmap document is saved as Grayscale (its black and white pixels kept): this \
                 build does not write 1-bit .psd files",
            );
            from_working_rgb(file, Separation::Grayscale)?;
        }
        mode::CMYK => {
            // One exact separation per distinct colour: the solve costs tens
            // of microseconds, and a document repeats its colours.
            let mut memo: HashMap<[u8; 3], [f32; 4]> = HashMap::new();
            let mut separate = |rgb: [u8; 3]| {
                *memo.entry(rgb).or_insert_with(|| {
                    let c = color::cmyk::rgb8_to_cmyk(rgb);
                    [c.c, c.m, c.y, c.k]
                })
            };
            from_working_rgb(file, Separation::Cmyk(&mut separate))?;
        }
        mode::LAB => from_working_rgb(file, Separation::Lab(&rgb_to_lab))?,
        mode::INDEXED => save_indexed(composite_rgba8, file, notes)?,
        mode::DUOTONE => notes.push(
            "a Duotone document is saved as RGB with its inks applied: the ink record is not \
             written back",
        ),
        _ => {}
    }
    Ok(())
}

/// Indexed is flat: the layers become the one composite, whose colours are
/// the palette (exactly, up to 256 of them; a median cut past that).
fn save_indexed(
    composite_rgba8: &[u8],
    file: &mut psd::PsdFile,
    notes: &mut PsdNotes,
) -> Result<(), ImportError> {
    if !file.layers.is_empty() {
        notes.push("an Indexed .psd holds one flat image: the layers were merged into it");
        file.layers.clear();
    }
    if file.header.depth != psd::Depth::Eight {
        notes.push("an Indexed .psd is 8-bit: the 16-bit samples were narrowed");
    }
    let mut own: Vec<[u8; 3]> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut transparent = false;
    let mut histogram = color::quantize::Histogram::new();
    for px in composite_rgba8.as_chunks::<4>().0 {
        if px[3] < 128 {
            transparent = true;
            continue;
        }
        let rgb = [px[0], px[1], px[2]];
        if seen.insert(rgb) && own.len() <= 256 {
            own.push(rgb);
        }
    }
    let room = if transparent { 255 } else { 256 };
    let colors = if own.len() <= room {
        own.sort_unstable();
        own
    } else {
        histogram.add_rgba8(composite_rgba8);
        notes.push(format!(
            "this Indexed document holds more than {room} colours; the saved palette is a \
             median cut of them"
        ));
        color::quantize::build_palette(
            &histogram,
            color::quantize::PaletteKind::Adaptive,
            room as u16,
        )
        .map_err(|e| ImportError::Psd(psd::PsdError::InvalidDocument(e.to_string())))?
    };
    let mut colors = if colors.is_empty() {
        vec![[0, 0, 0]]
    } else {
        colors
    };
    let transparent = transparent.then(|| {
        colors.push([0, 0, 0]);
        (colors.len() - 1) as u8
    });
    let table = psd::colour_modes::IndexedTable {
        colors,
        transparent,
    };
    let exact: HashMap<[u8; 3], u8> = table
        .colors
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i as u8) != table.transparent)
        .map(|(i, c)| (*c, i as u8))
        .collect();
    let palette: Vec<[u8; 3]> = match table.transparent {
        Some(t) => table.colors[..usize::from(t)].to_vec(),
        None => table.colors.clone(),
    };
    let mut index_of = |rgb: [u8; 3]| match exact.get(&rgb) {
        Some(i) => *i,
        None => color::quantize::nearest(&palette, rgb.map(i32::from)) as u8,
    };
    psd::colour_modes::from_working_rgb(
        file,
        psd::colour_modes::Separation::Indexed(&table, &mut index_of),
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "psd_colour_modes_tests.rs"]
mod tests;
