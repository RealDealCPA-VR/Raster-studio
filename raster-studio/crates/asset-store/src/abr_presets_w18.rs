//! W18-I: brush presets in an `.abr` — names and dynamics, not just tips.
//!
//! [`write_abr`](super::write_abr) writes the sampled tips (`samp`) only.
//! Photoshop keeps each preset's name and its settings in a second section,
//! `desc`: an action descriptor (version 16, class `null`) whose `Brsh`
//! list holds one `brushPreset` object per brush — its name (`Nm  `), its
//! tip (`Brsh`, a `sampledBrush` naming the `samp` entry by its identifier
//! in `sampledData`, with diameter `Dmtr`, angle `Angl`, roundness `Rndn`,
//! spacing `Spcn` and hardness `Hrdn`) and its dynamics: Shape Dynamics
//! (`useTipDynamics`, `szVr` size jitter, `minimumDiameter`, `angleDynamics`,
//! `roundnessDynamics`, `minimumRoundness`), Scattering (`useScatter`,
//! `scatterDynamics`, `bothAxes`, `Cnt ` and `countDynamics`) and Transfer
//! (`usePaintDynamics`, `opVr` opacity jitter, `prVr` flow jitter), each
//! jitter a `brVr` object's `jitter` percentage.
//!
//! [`write_abr_presets`] writes both sections; [`parse_abr_presets`] reads
//! them back, pairing each tip with its preset through the identifier, so a
//! file this writes names and configures its brushes for any reader of the
//! format — and for this one.

use psd::bytes::{Cursor, Sink};
use psd::{Descriptor, Value};

use super::{parse_abr, AbrBrush, AbrError};

/// A brush's Shape Dynamics, Scattering and Transfer jitters, as fractions
/// (`0..=1`) the way the brush engine holds them; `scatter` is in
/// diameters (`0..=10`) and `count` dabs per step (`1..=16`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AbrDynamics {
    pub size_jitter: f32,
    pub min_diameter: f32,
    pub angle_jitter: f32,
    pub roundness_jitter: f32,
    pub min_roundness: f32,
    pub scatter: f32,
    pub scatter_both_axes: bool,
    pub count: u32,
    pub count_jitter: f32,
    pub opacity_jitter: f32,
    pub flow_jitter: f32,
}

impl Default for AbrDynamics {
    fn default() -> Self {
        Self {
            size_jitter: 0.0,
            min_diameter: 0.0,
            angle_jitter: 0.0,
            roundness_jitter: 0.0,
            min_roundness: 0.0,
            scatter: 0.0,
            scatter_both_axes: false,
            count: 1,
            count_jitter: 0.0,
            opacity_jitter: 0.0,
            flow_jitter: 0.0,
        }
    }
}

/// One brush preset: its name, its sampled tip and its settings.
#[derive(Clone, PartialEq, Debug)]
pub struct AbrPreset {
    pub name: String,
    pub tip: AbrBrush,
    /// The brush diameter in pixels.
    pub diameter: f32,
    /// Tip angle in degrees.
    pub angle_deg: f32,
    /// `0..=1`, 1 round.
    pub roundness: f32,
    /// Dab spacing as a fraction of the diameter.
    pub spacing: f32,
    /// `0..=1`.
    pub hardness: f32,
    pub dynamics: AbrDynamics,
}

impl AbrPreset {
    /// A preset for `tip` named `name`, at the tip's own size, round, 25%
    /// spacing, no dynamics.
    pub fn new(name: impl Into<String>, tip: AbrBrush) -> Self {
        let diameter = tip.width.max(tip.height) as f32;
        Self {
            name: name.into(),
            tip,
            diameter,
            angle_deg: 0.0,
            roundness: 1.0,
            spacing: 0.25,
            hardness: 1.0,
            dynamics: AbrDynamics::default(),
        }
    }
}

/// The identifier the `samp` entry of tip `index` carries — the one
/// [`write_abr`](super::write_abr) writes, which `sampledData` names.
pub(crate) fn tip_id(index: usize) -> String {
    format!("$raster-studio-brush-{index:015}")
}

fn pct(v: f32) -> Value {
    Value::UnitFloat {
        unit: *b"#Prc",
        value: f64::from(if v.is_finite() { v } else { 0.0 }) * 100.0,
    }
}

fn unit(code: &[u8; 4], v: f32) -> Value {
    Value::UnitFloat {
        unit: *code,
        value: f64::from(if v.is_finite() { v } else { 0.0 }),
    }
}

/// A `brVr` object: control off, 25 fade steps, `jitter` as a percentage.
fn variation(jitter: f32) -> Value {
    let mut d = Descriptor::new("brVr");
    let _ = d.push("bVTy", Value::Integer(0));
    let _ = d.push("fStp", Value::Integer(25));
    let _ = d.push("jitter", pct(jitter));
    Value::Descriptor(d)
}

fn preset_descriptor(index: usize, p: &AbrPreset) -> Result<Descriptor, AbrError> {
    let bad = |_| AbrError::Malformed("a preset descriptor key");
    let mut tip = Descriptor::new("sampledBrush");
    tip.push("Dmtr", unit(b"#Pxl", p.diameter)).map_err(bad)?;
    tip.push("Hrdn", pct(p.hardness)).map_err(bad)?;
    tip.push("Angl", unit(b"#Ang", p.angle_deg)).map_err(bad)?;
    tip.push("Rndn", pct(p.roundness)).map_err(bad)?;
    tip.push("Spcn", pct(p.spacing)).map_err(bad)?;
    tip.push("Intr", Value::Bool(true)).map_err(bad)?;
    tip.push("flipX", Value::Bool(false)).map_err(bad)?;
    tip.push("flipY", Value::Bool(false)).map_err(bad)?;
    tip.push("sampledData", Value::Text(tip_id(index)))
        .map_err(bad)?;
    let d = &p.dynamics;
    let shape = d.size_jitter > 0.0 || d.angle_jitter > 0.0 || d.roundness_jitter > 0.0;
    let scatter = d.scatter > 0.0 || d.count > 1 || d.count_jitter > 0.0;
    let transfer = d.opacity_jitter > 0.0 || d.flow_jitter > 0.0;
    let mut preset = Descriptor::new("brushPreset");
    preset
        .push("Nm  ", Value::Text(p.name.clone()))
        .map_err(bad)?;
    preset.push("Brsh", Value::Descriptor(tip)).map_err(bad)?;
    for (key, value) in [
        ("useTipDynamics", Value::Bool(shape)),
        ("szVr", variation(d.size_jitter)),
        ("minimumDiameter", pct(d.min_diameter)),
        ("angleDynamics", variation(d.angle_jitter)),
        ("roundnessDynamics", variation(d.roundness_jitter)),
        ("minimumRoundness", pct(d.min_roundness)),
        ("useScatter", Value::Bool(scatter)),
        ("scatterDynamics", variation(d.scatter)),
        ("bothAxes", Value::Bool(d.scatter_both_axes)),
        ("Cnt ", Value::Integer(d.count.clamp(1, 16) as i32)),
        ("countDynamics", variation(d.count_jitter)),
        ("usePaintDynamics", Value::Bool(transfer)),
        ("opVr", variation(d.opacity_jitter)),
        ("prVr", variation(d.flow_jitter)),
    ] {
        preset.push(key, value).map_err(bad)?;
    }
    Ok(preset)
}

/// W18-I: File ▸ Export as .ABR with names and dynamics: the `samp`
/// section [`write_abr`](super::write_abr) writes, then a `desc` section
/// naming and configuring each tip (see the module docs). Refuses what
/// `write_abr` refuses.
pub fn write_abr_presets(presets: &[AbrPreset]) -> Result<Vec<u8>, AbrError> {
    let tips: Vec<AbrBrush> = presets.iter().map(|p| p.tip.clone()).collect();
    let mut file = super::write_abr(&tips)?;
    let list = presets
        .iter()
        .enumerate()
        .map(|(i, p)| preset_descriptor(i, p).map(Value::Descriptor))
        .collect::<Result<Vec<_>, _>>()?;
    let mut root = Descriptor::new("null");
    root.push("Brsh", Value::List(list))
        .map_err(|_| AbrError::Malformed("a preset descriptor key"))?;
    let mut sink = Sink::new();
    sink.u32(16);
    root.write(&mut sink)
        .map_err(|_| AbrError::Malformed("a preset descriptor key"))?;
    let desc = sink.into_inner();
    file.extend_from_slice(b"8BIMdesc");
    file.extend_from_slice(&(desc.len() as u32).to_be_bytes());
    file.extend_from_slice(&desc);
    if desc.len() % 2 == 1 {
        file.push(0);
    }
    Ok(file)
}

fn fraction(d: &Descriptor, key: &str) -> Option<f32> {
    d.number(key).map(|v| (v / 100.0) as f32)
}

fn jitter(d: &Descriptor, key: &str) -> f32 {
    d.descriptor(key)
        .and_then(|v| fraction(v, "jitter"))
        .unwrap_or(0.0)
}

/// The `desc` section of a v6+ file, if it has one.
fn desc_section(bytes: &[u8]) -> Option<&[u8]> {
    let mut at = 4usize;
    while at + 12 <= bytes.len() {
        let key = &bytes[at + 4..at + 8];
        let len = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().ok()?) as usize;
        let body = bytes.get(at + 12..at + 12 + len)?;
        if key == b"desc" {
            return Some(body);
        }
        at += 12 + len + len % 2;
    }
    None
}

/// W18-I: read an `.abr`'s brushes with their presets: each tip
/// [`parse_abr`] reads, named and configured by the `desc` entry whose
/// `sampledData` names it. A file with no `desc` (or an entry it cannot
/// match) gives that tip [`AbrPreset::new`]'s defaults and an empty name.
pub fn parse_abr_presets(bytes: &[u8]) -> Result<Vec<AbrPreset>, AbrError> {
    let tips = parse_abr(bytes)?;
    let mut presets: Vec<AbrPreset> = tips
        .into_iter()
        .map(|tip| AbrPreset::new(String::new(), tip))
        .collect();
    let Some(desc) = desc_section(bytes) else {
        return Ok(presets);
    };
    let mut cur = Cursor::new(desc);
    if cur.u32().ok() != Some(16) {
        return Ok(presets);
    }
    let Ok(root) = Descriptor::read(&mut cur, &psd::ReadOptions::default()) else {
        return Ok(presets);
    };
    let Some(Value::List(list)) = root.get("Brsh") else {
        return Ok(presets);
    };
    for item in list {
        let Value::Descriptor(p) = item else { continue };
        let Some(tip) = p.descriptor("Brsh") else {
            continue;
        };
        let Some(index) = tip
            .text("sampledData")
            .and_then(|id| (0..presets.len()).find(|i| tip_id(*i) == id))
        else {
            continue;
        };
        let out = &mut presets[index];
        out.name = p.text("Nm  ").unwrap_or_default().to_string();
        out.diameter = tip.number("Dmtr").map_or(out.diameter, |v| v as f32);
        out.hardness = fraction(tip, "Hrdn").unwrap_or(out.hardness);
        out.angle_deg = tip.number("Angl").map_or(out.angle_deg, |v| v as f32);
        out.roundness = fraction(tip, "Rndn").unwrap_or(out.roundness);
        out.spacing = fraction(tip, "Spcn").unwrap_or(out.spacing);
        out.dynamics = AbrDynamics {
            size_jitter: jitter(p, "szVr"),
            min_diameter: fraction(p, "minimumDiameter").unwrap_or(0.0),
            angle_jitter: jitter(p, "angleDynamics"),
            roundness_jitter: jitter(p, "roundnessDynamics"),
            min_roundness: fraction(p, "minimumRoundness").unwrap_or(0.0),
            scatter: jitter(p, "scatterDynamics"),
            scatter_both_axes: matches!(p.get("bothAxes"), Some(Value::Bool(true))),
            count: p
                .number("Cnt ")
                .map_or(1, |v| (v as i64).clamp(1, 16) as u32),
            count_jitter: jitter(p, "countDynamics"),
            opacity_jitter: jitter(p, "opVr"),
            flow_jitter: jitter(p, "prVr"),
        };
    }
    Ok(presets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tip(side: u32, value: u8) -> AbrBrush {
        AbrBrush {
            width: side,
            height: side,
            alpha8: vec![value; (side * side) as usize],
        }
    }

    /// Names and every dynamic survive a write and a read; the tips are
    /// still what the plain reader sees.
    #[test]
    fn presets_round_trip_with_their_names_and_dynamics() {
        let mut soft = AbrPreset::new("Soft Cloud", tip(9, 128));
        soft.diameter = 42.0;
        soft.angle_deg = 30.0;
        soft.roundness = 0.5;
        soft.spacing = 0.1;
        soft.hardness = 0.25;
        soft.dynamics = AbrDynamics {
            size_jitter: 0.4,
            min_diameter: 0.2,
            angle_jitter: 0.3,
            roundness_jitter: 0.1,
            min_roundness: 0.05,
            scatter: 2.5,
            scatter_both_axes: true,
            count: 3,
            count_jitter: 0.6,
            opacity_jitter: 0.7,
            flow_jitter: 0.8,
        };
        let plain = AbrPreset::new("Grain", tip(4, 255));
        let bytes = write_abr_presets(&[soft.clone(), plain.clone()]).unwrap();

        let tips = parse_abr(&bytes).unwrap();
        assert_eq!(tips, vec![soft.tip.clone(), plain.tip.clone()]);

        let read = parse_abr_presets(&bytes).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].name, "Soft Cloud");
        assert_eq!(read[1].name, "Grain");
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(close(read[0].diameter, 42.0));
        assert!(close(read[0].angle_deg, 30.0));
        assert!(close(read[0].roundness, 0.5));
        assert!(close(read[0].spacing, 0.1));
        assert!(close(read[0].hardness, 0.25));
        let (a, b) = (read[0].dynamics, soft.dynamics);
        for (x, y) in [
            (a.size_jitter, b.size_jitter),
            (a.min_diameter, b.min_diameter),
            (a.angle_jitter, b.angle_jitter),
            (a.roundness_jitter, b.roundness_jitter),
            (a.min_roundness, b.min_roundness),
            (a.scatter, b.scatter),
            (a.count_jitter, b.count_jitter),
            (a.opacity_jitter, b.opacity_jitter),
            (a.flow_jitter, b.flow_jitter),
        ] {
            assert!(close(x, y), "{x} != {y}");
        }
        assert!(a.scatter_both_axes);
        assert_eq!(a.count, 3);
        assert_eq!(read[1].dynamics, AbrDynamics::default());
    }

    /// A tips-only file reads with empty names and default settings.
    #[test]
    fn a_file_without_desc_reads_its_tips_unnamed() {
        let bytes = super::super::write_abr(&[tip(3, 9)]).unwrap();
        let read = parse_abr_presets(&bytes).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "");
        assert_eq!(read[0].tip, tip(3, 9));
    }
}
