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
//! * **Bitmap** — 1-bit black and white; W18-H: saved as a 1-bit Bitmap
//!   file (flat: the layers are merged; mid-grey luma is the threshold).
//! * **Duotone** — the greyscale base printed through the file's inks
//!   (`color::duotone`), when the ink record can be read; the greyscale base
//!   otherwise. W18-H: saved as a Duotone file — the grey base each colour
//!   is printed from, and the ink record — when the document's colours are
//!   the prints of a known set of inks: the inks of a Duotone `.psd` opened
//!   in this session, or the Duotone dialog's default inks of any type. The
//!   document keeps no ink record of its own (the inks are baked into its
//!   tiles), so a document whose colours no known set of inks prints (inks
//!   picked in the dialog, or edits in colours the inks cannot print) is
//!   saved as RGB with its inks applied, and the report says so.
//! * **Multichannel** — no document mode here: its first three inks show as
//!   C, M, Y in an RGB document (one ink as Greyscale); further channels are
//!   left out and named in the report.
//!
//! The editor's tiles are 8-bit RGBA in every mode (16-bit only for RGB and
//! grey sources), so a 16-bit CMYK / Lab file converts at 8-bit precision; the
//! report says so.

use std::cell::RefCell;
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

thread_local! {
    /// W18-H: the ink records of the Duotone files opened on this thread,
    /// newest last, so Save as PSD can write a Duotone document back with
    /// its inks (see the module docs).
    static OPENED_DUOTONES: RefCell<Vec<psd::colour_modes::DuotoneRecord>> =
        const { RefCell::new(Vec::new()) };
}

/// The most ink records [`OPENED_DUOTONES`] keeps.
const MAX_OPENED_DUOTONES: usize = 32;

/// W18-H: the print table of an ink record (a colour-book ink shows as the
/// stand-in colour it opens with).
fn record_spec(record: &psd::colour_modes::DuotoneRecord) -> color::duotone::DuotoneSpec {
    color::duotone::DuotoneSpec {
        inks: record
            .inks
            .iter()
            .enumerate()
            .map(|(i, ink)| color::duotone::DuotoneInk {
                color: ink_rgb(ink.color).unwrap_or(color::duotone::DEFAULT_INKS[i.min(3)]),
                curve: ink.curve.clone(),
            })
            .collect(),
    }
}

/// W18-H: a dialog spec as the ink record a Duotone file carries: its inks
/// as 16-bit RGB, its curves sampled at the record's thirteen tints.
fn spec_record(spec: &color::duotone::DuotoneSpec) -> psd::colour_modes::DuotoneRecord {
    use psd::colour_modes::{DuotoneInkRecord, InkColor, DUOTONE_CURVE_TINTS};
    psd::colour_modes::DuotoneRecord {
        inks: spec
            .inks
            .iter()
            .enumerate()
            .map(|(i, ink)| DuotoneInkRecord {
                color: InkColor::Rgb(ink.color.map(|v| u16::from(v) * 257)),
                name: format!("Ink {}", i + 1),
                curve: DUOTONE_CURVE_TINTS
                    .iter()
                    .map(|t| {
                        let t = f32::from(*t) / 100.0;
                        [t, ink.amount(t).clamp(0.0, 1.0)]
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// W18-H: the ink record whose prints are every colour of `composite_rgba8`
/// (each opaque pixel within one code of a print), and for each colour the
/// grey it is printed from; `None` when no known set of inks prints them.
fn known_inks(
    composite_rgba8: &[u8],
) -> Option<(psd::colour_modes::DuotoneRecord, HashMap<[u8; 3], u8>)> {
    let mut candidates: Vec<psd::colour_modes::DuotoneRecord> =
        OPENED_DUOTONES.with(|o| o.borrow().iter().rev().cloned().collect());
    for kind in color::duotone::DuotoneType::ALL {
        candidates.push(spec_record(&color::duotone::DuotoneSpec::of_type(kind)));
    }
    let colours: std::collections::HashSet<[u8; 3]> = composite_rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 0)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    'candidate: for record in candidates {
        let lut = record_spec(&record).lut();
        let mut grey_of = HashMap::with_capacity(colours.len());
        for rgb in &colours {
            let (g, d) = lut
                .iter()
                .enumerate()
                .map(|(g, p)| {
                    let d = (0..3)
                        .map(|c| i32::from(p[c]).abs_diff(i32::from(rgb[c])))
                        .max()
                        .unwrap_or(0);
                    (g as u8, d)
                })
                .min_by_key(|(_, d)| *d)?;
            if d > 1 {
                continue 'candidate;
            }
            grey_of.insert(*rgb, g);
        }
        return Some((record, grey_of));
    }
    None
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
    // W18-H: remembered, so Save as PSD can write the inks back.
    OPENED_DUOTONES.with(|o| {
        let mut opened = o.borrow_mut();
        opened.retain(|r| *r != record);
        opened.push(record.clone());
        let over = opened.len().saturating_sub(MAX_OPENED_DUOTONES);
        opened.drain(..over);
    });
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
            // W18-H: a 1-bit Bitmap file, flat as Photoshop keeps one.
            if !file.layers.is_empty() {
                notes.push("a Bitmap .psd holds one flat image: the layers were merged into it");
                file.layers.clear();
            }
            from_working_rgb(file, Separation::Bitmap)?;
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
        mode::DUOTONE => match known_inks(composite_rgba8) {
            // W18-H: the grey base and the ink record (see the module docs).
            Some((record, greys)) => {
                let mut grey_of = |rgb: [u8; 3]| match greys.get(&rgb) {
                    Some(g) => *g,
                    None => psd::colour_modes::luma601(rgb),
                };
                from_working_rgb(
                    file,
                    Separation::Duotone {
                        grey_of: &mut grey_of,
                        record: &record,
                    },
                )?;
            }
            None => notes.push(
                "a Duotone document is saved as RGB with its inks applied: its colours are not \
                 the prints of any inks this build knows (the document keeps no ink record)",
            ),
        },
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
