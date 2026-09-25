//! W16-L: File > Open of a curves preset (`.acv`) and of the 3D LUT formats
//! `.3dl` and `.look`, beside the `.cube` route (`editor_open_any`).
//!
//! A child of [`super`] (the resource router), which asks
//! [`is_w16_resource_path`] and hands the file to
//! [`Editor::open_w16_resource`].
//!
//! | File | Lands as |
//! |---|---|
//! | `.acv` (Photoshop Curves preset) | a **Curves adjustment layer** on the active document with the file's composite, red, green and blue curves (one undo step). This build's Curves dialog keeps no list of saved presets for a file to join, so the curve arrives as a layer, the way a `.cube` arrives as a Color Lookup layer; extra curves a CMYK preset carries are ignored |
//! | `.3dl` (Autodesk / Lustre 3D LUT) | a **Color Lookup** adjustment layer: the input mesh line gives the edge (or a `Mesh a b` header, `2^a + 1`), the entries are integers scaled by the output depth (`Mesh`'s, else the smallest of 10, 12 or 16 bits that holds the largest entry), blue varying fastest |
//! | `.look` (SpeedGrade look, XML) | a **Color Lookup** layer from its `<LUT>`: `<size>` and `<data>`, hexadecimal little-endian `f32` RGB triples. The order is taken as `.cube`'s (red fastest); no SpeedGrade-written file was available to confirm it |
//!
//! Every file is read through a size cap before it is parsed, and every
//! table is validated by [`adjustments::Lut3d::new`] (edge 2..=65, finite).

use std::path::Path;

use adjustments::{AdjustmentError, Lut3d, MAX_LUT_SIZE};
use editor_core::Command;
use layer_model::{AdjustmentKind, AdjustmentLayer, Layer, LayerKind};

use super::super::{Action, ActionError, Editor, Effect};

/// The extensions this module routes.
pub const W16_RESOURCE_EXTENSIONS: &[&str] = &["acv", "3dl", "look"];

/// Largest file read: a 65-point `.3dl` is ~275k short lines.
const MAX_BYTES: u64 = 16 << 20;

/// Whether File > Open routes `path` here.
pub fn is_w16_resource_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        W16_RESOURCE_EXTENSIONS
            .iter()
            .any(|x| x.eq_ignore_ascii_case(e))
    })
}

fn read_capped(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("the file is larger than {MAX_BYTES} bytes"));
    }
    Ok(bytes)
}

/// The curves of a Photoshop `.acv`: composite, red, green, blue, each as
/// `[input, output]` points in `0..=1`.
pub fn parse_acv(bytes: &[u8]) -> Result<[Vec<[f32; 2]>; 4], String> {
    let u16_at = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|s| u16::from_be_bytes([s[0], s[1]]))
            .ok_or_else(|| "the file ends inside a curve".to_string())
    };
    let version = u16_at(0)?;
    if version != 1 && version != 4 {
        return Err(format!("this is not a Curves preset (version {version})"));
    }
    let count = usize::from(u16_at(2)?);
    if count == 0 {
        return Err("the preset holds no curves".into());
    }
    let identity = || vec![[0.0, 0.0], [1.0, 1.0]];
    let mut curves = [identity(), identity(), identity(), identity()];
    let mut at = 4;
    for index in 0..count.min(64) {
        let points = usize::from(u16_at(at)?);
        at += 2;
        if !(2..=19).contains(&points) {
            return Err(format!(
                "curve {index} has {points} points (2-19 are allowed)"
            ));
        }
        let mut curve = Vec::with_capacity(points);
        for _ in 0..points {
            let output = f32::from(u16_at(at)?.min(255)) / 255.0;
            let input = f32::from(u16_at(at + 2)?.min(255)) / 255.0;
            at += 4;
            curve.push([input, output]);
        }
        curve.sort_by(|a, b| a[0].total_cmp(&b[0]));
        curve.dedup_by(|a, b| a[0] == b[0]);
        if curve.len() < 2 {
            return Err(format!("curve {index} has fewer than two distinct points"));
        }
        if let Some(slot) = curves.get_mut(index) {
            *slot = curve;
        }
    }
    Ok(curves)
}

/// A `.3dl` as a Lut3d (red-fastest table).
pub fn parse_3dl(name: &str, text: &str) -> Result<Lut3d, AdjustmentError> {
    let bad = |reason: String| AdjustmentError::InvalidLut { reason };
    let mut size: Option<usize> = None;
    let mut out_bits: Option<u32> = None;
    let mut entries: Vec<[f64; 3]> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        if words[0].eq_ignore_ascii_case("3DMESH") {
            continue;
        }
        if words[0].eq_ignore_ascii_case("Mesh") {
            let a: u32 = words
                .get(1)
                .and_then(|v| v.parse().ok())
                .filter(|a| (1..=6).contains(a))
                .ok_or_else(|| bad(format!("line {}: a bad Mesh line", n + 1)))?;
            size = Some((1usize << a) + 1);
            out_bits = words
                .get(2)
                .and_then(|v| v.parse().ok())
                .filter(|b| (8..=16).contains(b));
            continue;
        }
        let nums: Vec<f64> = words
            .iter()
            .map(|w| w.parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|_| bad(format!("line {}: not a number", n + 1)))?;
        if nums.len() != 3 {
            // The input mesh (shaper) line: one value per lattice point
            // (after a `Mesh` header it must agree with it).
            if entries.is_empty() && (size.is_none() || size == Some(nums.len())) {
                size = Some(nums.len());
                continue;
            }
            return Err(bad(format!("line {}: an entry needs three values", n + 1)));
        }
        if size.is_none() {
            // A three-point mesh line would be ambiguous; a real one is longer.
            return Err(bad(format!("line {}: the mesh line is missing", n + 1)));
        }
        if entries.len() >= MAX_LUT_SIZE.pow(3) {
            return Err(bad("more entries than a 65-point cube holds".into()));
        }
        entries.push([nums[0], nums[1], nums[2]]);
    }
    let size = size.ok_or_else(|| bad("the file has no mesh line".into()))?;
    if !(2..=MAX_LUT_SIZE).contains(&size) || entries.len() != size * size * size {
        return Err(bad(format!(
            "a {size}-point .3dl needs {} entries, got {}",
            size.saturating_pow(3),
            entries.len()
        )));
    }
    let max = match out_bits {
        Some(b) => f64::from((1u32 << b) - 1),
        None => {
            let top = entries.iter().flatten().fold(0.0f64, |a, v| a.max(*v));
            [1023.0, 4095.0, 65535.0]
                .into_iter()
                .find(|m| top <= *m)
                .unwrap_or(top.max(1.0))
        }
    };
    let n = size;
    let mut table = vec![[0.0f32; 3]; n * n * n];
    for r in 0..n {
        for g in 0..n {
            for b in 0..n {
                let e = entries[(r * n + g) * n + b];
                table[r + g * n + b * n * n] = [
                    (e[0] / max) as f32,
                    (e[1] / max) as f32,
                    (e[2] / max) as f32,
                ];
            }
        }
    }
    Lut3d::new(name, size, table)
}

/// A SpeedGrade `.look` as a Lut3d.
pub fn parse_look(name: &str, text: &str) -> Result<Lut3d, AdjustmentError> {
    let bad = |reason: &str| AdjustmentError::InvalidLut {
        reason: reason.to_string(),
    };
    let inner = |tag: &str| -> Option<&str> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let lut = &text[text.find("<LUT>")?..];
        let start = lut.find(&open)? + open.len();
        let end = lut[start..].find(&close)? + start;
        Some(lut[start..end].trim().trim_matches('"').trim())
    };
    let size: usize = inner("size")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad("the look has no <LUT><size>"))?;
    if !(2..=MAX_LUT_SIZE).contains(&size) {
        return Err(bad("the look's LUT size is out of range (2-65)"));
    }
    let data = inner("data").ok_or_else(|| bad("the look has no <LUT><data>"))?;
    let hex: Vec<u8> = data.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let want = size * size * size;
    if hex.len() != want * 3 * 8 {
        return Err(bad(
            "the look's LUT data does not hold size cubed RGB entries",
        ));
    }
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let not_hex = || bad("the look's LUT data is not hexadecimal");
    let mut floats = Vec::with_capacity(want * 3);
    for chunk in hex.as_chunks::<8>().0 {
        let mut b = [0u8; 4];
        for (i, [hi, lo]) in chunk.as_chunks::<2>().0.iter().enumerate() {
            b[i] = (digit(*hi).ok_or_else(not_hex)? << 4) | digit(*lo).ok_or_else(not_hex)?;
        }
        floats.push(f32::from_le_bytes(b));
    }
    let table = floats
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    Lut3d::new(name, size, table)
}

impl Editor {
    /// Route a `.acv` / `.3dl` / `.look` to its layer; `None` when `path` is
    /// none of them.
    pub fn open_w16_resource(&mut self, path: &Path) -> Option<Result<Effect, ActionError>> {
        if !is_w16_resource_path(path) {
            return None;
        }
        Some(self.import_w16_resource(path))
    }

    fn import_w16_resource(&mut self, path: &Path) -> Result<Effect, ActionError> {
        let fail = |e: &dyn std::fmt::Display| {
            ActionError::failed(Action::Open, format!("{}: {e}", path.display()))
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.active().is_none() {
            let what = if ext == "acv" {
                "open a document to apply these curves to"
            } else {
                "open a document to apply this colour lookup table to"
            };
            return Err(ActionError::unavailable(Action::Open, what));
        }
        let bytes = read_capped(path).map_err(|e| fail(&e))?;
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (layer, status) = if ext == "acv" {
            let [composite, red, green, blue] = parse_acv(&bytes).map_err(|e| fail(&e))?;
            (
                Layer::with_kind(
                    "Curves",
                    LayerKind::Adjustment(AdjustmentLayer {
                        kind: AdjustmentKind::CurvesFull {
                            composite,
                            red,
                            green,
                            blue,
                        },
                    }),
                ),
                format!("Added a Curves layer from {file}"),
            )
        } else {
            let text = String::from_utf8(bytes).map_err(|_| fail(&"the file is not text"))?;
            let lut = if ext == "3dl" {
                parse_3dl(&stem, &text)
            } else {
                parse_look(&stem, &text)
            }
            .map_err(|e| fail(&e))?;
            (
                Layer::with_kind(
                    "Color Lookup",
                    LayerKind::Adjustment(AdjustmentLayer {
                        kind: AdjustmentKind::ColorLookup {
                            name: lut.name().to_string(),
                            size: lut.size() as u32,
                            table: lut.table().to_vec(),
                        },
                    }),
                ),
                format!("Added a Color Lookup layer from {file}"),
            )
        };
        let id = layer.id;
        let doc = self
            .active_mut()
            .ok_or_else(|| fail(&"no document is open"))?;
        doc.apply(Command::create_layer(layer))
            .map_err(|e| fail(&e))?;
        let _ = doc.document.set_active_layer(Some(id));
        self.status = Some(status);
        self.touch();
        Ok(Effect::DocumentEdited)
    }
}
