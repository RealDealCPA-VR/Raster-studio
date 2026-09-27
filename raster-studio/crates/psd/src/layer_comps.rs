//! W18-B: layer comps — image resource 1065 and each layer's `cmls` settings.
//!
//! Photoshop keeps a document's layer comps in two places:
//!
//! * **Resource 1065** lists the comps: `u32 16` then a descriptor whose
//!   `list` is one `Comp` descriptor per comp — `Nm  ` (name), `compID`,
//!   `capturedInfo` (bit 0 visibility, bit 1 position, bit 2 appearance),
//!   and an optional `comment` — plus `lastAppliedComp`, the id of the comp
//!   last applied.
//! * **Each layer's `shmd` metadata** holds a `cmls` item: `u32 16` then a
//!   descriptor with the layer's `LyrI` and `layerSettings`, one entry per
//!   comp the layer is recorded in: `compList` (the comp ids the entry is
//!   for), `enab` (visible), `Ofst` (`Hrzn`/`Vrtc`: the layer's top-left in
//!   that comp), `blendOptions` (`Md  ` blend mode, `Opct` and
//!   `fillOpacity` percentages) and `Lefx` (the layer style). An entry that
//!   omits a key inherits it from the entries before it. Comp id 0 is
//!   Photoshop's "Last Document State".
//!
//! Every count here is bounded ([`MAX_COMPS`], the descriptor reader's own
//! ceilings) and nothing indexes on a file-supplied number.

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::error::{PsdError, PsdResult};
use crate::limits::ReadOptions;
use crate::model::{ImageResource, PsdLayer, TaggedBlock};
use layer_model::BlendMode;

/// The layer comps image resource.
pub const ID_LAYER_COMPS: u16 = 1065;
/// `capturedInfo` bits.
pub const CAPTURED_VISIBILITY: u32 = 1;
pub const CAPTURED_POSITION: u32 = 2;
pub const CAPTURED_APPEARANCE: u32 = 4;
/// Most comps (and per-layer settings) read from one file; the rest are
/// dropped, never allocated.
pub const MAX_COMPS: usize = 4_096;
/// The comp id Photoshop uses for the Last Document State.
pub const LAST_DOCUMENT_STATE_ID: i32 = 0;

/// One comp in resource 1065.
#[derive(Debug, Clone, PartialEq)]
pub struct PsdLayerComp {
    pub id: i32,
    pub name: String,
    /// `capturedInfo`: the `CAPTURED_*` bits.
    pub captured: u32,
    pub comment: Option<String>,
}

/// Resource 1065.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PsdLayerComps {
    pub comps: Vec<PsdLayerComp>,
    /// `lastAppliedComp`: a comp id.
    pub last_applied: Option<i32>,
}

/// One layer's recorded state in one comp (a `layerSettings` entry, with
/// what it inherits filled in).
#[derive(Debug, Clone, PartialEq)]
pub struct PsdCompLayerState {
    pub comp_id: i32,
    pub visible: Option<bool>,
    /// `Ofst`: the layer's top-left, document pixels.
    pub offset: Option<(i32, i32)>,
    pub blend_mode: Option<BlendMode>,
    /// `0.0..=1.0`.
    pub opacity: Option<f64>,
    /// `0.0..=1.0`.
    pub fill_opacity: Option<f64>,
    /// `Lefx`: the layer style descriptor (the `lfx2` descriptor's shape).
    pub effects: Option<Descriptor>,
}

impl PsdCompLayerState {
    /// A state with nothing recorded but its comp.
    pub fn new(comp_id: i32) -> Self {
        Self {
            comp_id,
            visible: None,
            offset: None,
            blend_mode: None,
            opacity: None,
            fill_opacity: None,
            effects: None,
        }
    }
}

fn invalid(what: String) -> PsdError {
    PsdError::InvalidDocument(format!("layer comps: {what}"))
}

fn versioned_descriptor(data: &[u8], opts: &ReadOptions) -> PsdResult<Descriptor> {
    let mut cur = Cursor::new(data);
    let mut version = cur.u32()?;
    if version != 16 {
        // Some writers put a word before the descriptor version (Photopea
        // reads both layouts).
        version = cur.u32()?;
    }
    if version != 16 {
        return Err(invalid(format!("descriptor version {version}, not 16")));
    }
    Descriptor::read(&mut cur, opts)
}

fn with_version(d: &Descriptor) -> PsdResult<Vec<u8>> {
    let mut sink = Sink::new();
    sink.u32(16);
    d.write(&mut sink)?;
    Ok(sink.into_inner())
}

fn int(d: &Descriptor, key: &str) -> Option<i32> {
    d.number(key)
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32)
}

/// Parse a 1065 payload.
pub fn parse(data: &[u8], opts: &ReadOptions) -> PsdResult<PsdLayerComps> {
    let d = versioned_descriptor(data, opts)?;
    let mut out = PsdLayerComps {
        comps: Vec::new(),
        last_applied: int(&d, "lastAppliedComp"),
    };
    if let Some(Value::List(list)) = d.get("list") {
        for item in list.iter().take(MAX_COMPS) {
            let Value::Descriptor(c) = item else {
                continue;
            };
            let Some(id) = int(c, "compID") else {
                continue;
            };
            out.comps.push(PsdLayerComp {
                id,
                name: c
                    .text("Nm  ")
                    .or_else(|| c.text("Nm"))
                    .unwrap_or_default()
                    .to_string(),
                captured: int(c, "capturedInfo").map_or(7, |v| v as u32 & 7),
                comment: c.text("comment").map(str::to_string),
            });
        }
    }
    Ok(out)
}

/// The comps in `resources`' 1065, if there is one.
pub fn layer_comps(
    resources: &[ImageResource],
    opts: &ReadOptions,
) -> Option<PsdResult<PsdLayerComps>> {
    resources
        .iter()
        .find(|r| r.id == ID_LAYER_COMPS)
        .map(|r| parse(&r.data, opts))
}

/// The 1065 resource for `comps`.
pub fn resource(comps: &PsdLayerComps) -> PsdResult<ImageResource> {
    let mut list = Vec::with_capacity(comps.comps.len().min(MAX_COMPS));
    for c in comps.comps.iter().take(MAX_COMPS) {
        let mut d = Descriptor::new("Comp");
        d.push("Nm  ", Value::Text(c.name.clone()))?;
        d.push("compID", Value::Integer(c.id))?;
        d.push("capturedInfo", Value::Integer((c.captured & 7) as i32))?;
        if let Some(comment) = c.comment.as_ref().filter(|s| !s.is_empty()) {
            d.push("comment", Value::Text(comment.clone()))?;
        }
        list.push(Value::Descriptor(d));
    }
    let mut d = Descriptor::new("null");
    d.push("list", Value::List(list))?;
    if let Some(id) = comps.last_applied {
        d.push("lastAppliedComp", Value::Integer(id))?;
    }
    Ok(ImageResource {
        id: ID_LAYER_COMPS,
        name: String::new(),
        data: with_version(&d)?,
    })
}

// ------------------------------------------------------------ shmd / cmls

/// One `shmd` metadata item.
struct MetaItem {
    signature: [u8; 4],
    key: [u8; 4],
    copy_on_duplicate: u8,
    data: Vec<u8>,
}

fn read_shmd(data: &[u8]) -> PsdResult<Vec<MetaItem>> {
    let mut cur = Cursor::new(data);
    let count = cur.u32()? as usize;
    // Twelve bytes of header per item, at least: the count is a claim.
    let count = count.min(cur.remaining() / 12).min(MAX_COMPS);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let signature = cur.tag()?;
        let key = cur.tag()?;
        let copy_on_duplicate = cur.u8()?;
        cur.skip(3)?;
        let len = cur.u32()? as usize;
        let data = cur.take(len)?.to_vec();
        out.push(MetaItem {
            signature,
            key,
            copy_on_duplicate,
            data,
        });
    }
    Ok(out)
}

fn write_shmd(items: &[MetaItem]) -> Vec<u8> {
    let mut sink = Sink::new();
    sink.u32(items.len() as u32);
    for item in items {
        sink.tag(&item.signature);
        sink.tag(&item.key);
        sink.u8(item.copy_on_duplicate);
        sink.zeros(3);
        sink.u32(item.data.len() as u32);
        sink.bytes(&item.data);
    }
    sink.into_inner()
}

fn percent(d: &Descriptor, key: &str) -> Option<f64> {
    d.number(key)
        .filter(|v| v.is_finite())
        .map(|v| (v / 100.0).clamp(0.0, 1.0))
}

/// The `BlnM` codes Photoshop writes, for the modes the model has.
const BLNM: &[&str] = &[
    "Nrml",
    "Dslv",
    "Drkn",
    "Mltp",
    "CBrn",
    "Lmbs",
    "dkCl",
    "Lghn",
    "Scrn",
    "CDdg",
    "lddg",
    "lgCl",
    "Ovrl",
    "SftL",
    "HrdL",
    "vLit",
    "lLit",
    "pLit",
    "HrdM",
    "Dfrn",
    "Xclu",
    "Sbtr",
    "blendDivide",
    "H   ",
    "Strt",
    "Clr ",
    "Lmns",
];

fn blnm_of(mode: BlendMode) -> &'static str {
    BLNM.iter()
        .copied()
        .find(|code| crate::effects::blend_from_blnm(code) == Some(mode))
        .unwrap_or("Nrml")
}

/// The comp states `layer`'s `shmd` / `cmls` records, in file order, each
/// with what it inherits from the entries before it. Empty when there is no
/// `cmls`; an unreadable one is an error.
pub fn layer_states(layer: &PsdLayer, opts: &ReadOptions) -> PsdResult<Vec<PsdCompLayerState>> {
    let Some(block) = layer.extra.iter().find(|b| &b.key == b"shmd") else {
        return Ok(Vec::new());
    };
    let items = read_shmd(&block.data)?;
    let Some(item) = items.iter().find(|i| &i.key == b"cmls") else {
        return Ok(Vec::new());
    };
    let d = versioned_descriptor(&item.data, opts)?;
    let Some(Value::List(entries)) = d.get("layerSettings") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut acc = PsdCompLayerState::new(0);
    for entry in entries.iter().take(MAX_COMPS) {
        let Value::Descriptor(e) = entry else {
            continue;
        };
        if let Some(Value::Bool(v)) = e.get("enab") {
            acc.visible = Some(*v);
        }
        if let Some(o) = e.descriptor("Ofst") {
            if let (Some(x), Some(y)) = (int(o, "Hrzn"), int(o, "Vrtc")) {
                acc.offset = Some((x, y));
            }
        }
        if let Some(b) = e.descriptor("blendOptions") {
            if let Some(Value::Enumerated { value, .. }) = b.get("Md  ").or_else(|| b.get("Md")) {
                if let Some(mode) = crate::effects::blend_from_blnm(value) {
                    acc.blend_mode = Some(mode);
                }
            }
            if let Some(v) = percent(b, "Opct") {
                acc.opacity = Some(v);
            }
            if let Some(v) = percent(b, "fillOpacity") {
                acc.fill_opacity = Some(v);
            }
        }
        if let Some(fx) = e.descriptor("Lefx") {
            acc.effects = Some(fx.clone());
        }
        if let Some(Value::List(ids)) = e.get("compList") {
            for id in ids.iter().take(MAX_COMPS) {
                if out.len() >= MAX_COMPS {
                    break;
                }
                let id = match id {
                    Value::Integer(v) => *v,
                    Value::LargeInteger(v) => {
                        (*v).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
                    }
                    _ => continue,
                };
                let mut state = acc.clone();
                state.comp_id = id;
                out.push(state);
            }
        }
    }
    Ok(out)
}

fn entry(state: &PsdCompLayerState) -> PsdResult<Descriptor> {
    let mut e = Descriptor::new("null");
    e.push("compList", Value::List(vec![Value::Integer(state.comp_id)]))?;
    if let Some(v) = state.visible {
        e.push("enab", Value::Bool(v))?;
    }
    if let Some((x, y)) = state.offset {
        let mut o = Descriptor::new("null");
        o.push("Hrzn", Value::Integer(x))?;
        o.push("Vrtc", Value::Integer(y))?;
        e.push("Ofst", Value::Descriptor(o.clone()))?;
        e.push("FXRefPoint", Value::Descriptor(o))?;
    }
    let pct = |v: f64| Value::UnitFloat {
        unit: *b"#Prc",
        value: if v.is_finite() {
            v.clamp(0.0, 1.0) * 100.0
        } else {
            100.0
        },
    };
    if state.blend_mode.is_some() || state.opacity.is_some() || state.fill_opacity.is_some() {
        let mut b = Descriptor::new("null");
        if let Some(mode) = state.blend_mode {
            b.push(
                "Md  ",
                Value::Enumerated {
                    type_id: "BlnM".into(),
                    value: blnm_of(mode).into(),
                },
            )?;
        }
        if let Some(v) = state.opacity {
            b.push("Opct", pct(v))?;
        }
        if let Some(v) = state.fill_opacity {
            b.push("fillOpacity", pct(v))?;
        }
        e.push("blendOptions", Value::Descriptor(b))?;
    }
    if let Some(fx) = &state.effects {
        e.push("Lefx", Value::Descriptor(fx.clone()))?;
    }
    Ok(e)
}

/// Record `states` as `layer`'s `cmls` (one self-contained entry per state),
/// keeping every other `shmd` item the layer has. An empty `states` removes
/// the `cmls` item (and a `shmd` left empty).
pub fn set_layer_states(layer: &mut PsdLayer, states: &[PsdCompLayerState]) -> PsdResult<()> {
    let mut items = match layer.extra.iter().find(|b| &b.key == b"shmd") {
        Some(block) => read_shmd(&block.data).unwrap_or_default(),
        None => Vec::new(),
    };
    items.retain(|i| &i.key != b"cmls");
    if !states.is_empty() {
        let mut d = Descriptor::new("null");
        if let Some(id) = layer.layer_id {
            d.push("LyrI", Value::Integer(id.min(i32::MAX as u32) as i32))?;
        }
        let mut list = Vec::with_capacity(states.len().min(MAX_COMPS));
        for s in states.iter().take(MAX_COMPS) {
            list.push(Value::Descriptor(entry(s)?));
        }
        d.push("layerSettings", Value::List(list))?;
        items.push(MetaItem {
            signature: *b"8BIM",
            key: *b"cmls",
            copy_on_duplicate: 1,
            data: with_version(&d)?,
        });
    }
    layer.extra.retain(|b| &b.key != b"shmd");
    if !items.is_empty() {
        layer
            .extra
            .push(TaggedBlock::new(*b"shmd", write_shmd(&items)));
    }
    Ok(())
}

/// Give every record in `layers` (depth first) without a layer id a fresh
/// one past the largest in use, and re-stamp the `LyrI` of its `cmls`, so
/// the comp settings name the layer they sit on. Iterative: the tree may be
/// deep.
pub fn number_layers(layers: &mut [PsdLayer], opts: &ReadOptions) -> PsdResult<()> {
    let mut next = {
        let mut max = 0u32;
        let mut stack: Vec<&PsdLayer> = layers.iter().collect();
        while let Some(l) = stack.pop() {
            max = max.max(l.layer_id.unwrap_or(0));
            stack.extend(l.children());
        }
        max.saturating_add(1)
    };
    let mut stack: Vec<&mut PsdLayer> = layers.iter_mut().collect();
    while let Some(l) = stack.pop() {
        if l.layer_id.is_none() {
            l.layer_id = Some(next);
            next = next.saturating_add(1);
            let states = layer_states(l, opts)?;
            if !states.is_empty() {
                set_layer_states(l, &states)?;
            }
        }
        if let Some(g) = l.group_data_mut() {
            stack.extend(g.children.iter_mut());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rect;

    fn comps() -> PsdLayerComps {
        PsdLayerComps {
            comps: vec![
                PsdLayerComp {
                    id: 11,
                    name: "Day".into(),
                    captured: 7,
                    comment: Some("sunny".into()),
                },
                PsdLayerComp {
                    id: 12,
                    name: "Night".into(),
                    captured: CAPTURED_VISIBILITY,
                    comment: None,
                },
            ],
            last_applied: Some(12),
        }
    }

    #[test]
    fn resource_1065_round_trips() {
        let r = resource(&comps()).unwrap();
        assert_eq!(r.id, ID_LAYER_COMPS);
        let back = layer_comps(&[r], &ReadOptions::default()).unwrap().unwrap();
        assert_eq!(back, comps());
    }

    #[test]
    fn layer_states_round_trip_and_keep_other_metadata() {
        let mut layer = PsdLayer::raster("L", Rect::sized(2, 2));
        layer.layer_id = Some(5);
        // Another metadata item (a timeline, say) that must survive.
        let other = write_shmd(&[MetaItem {
            signature: *b"8BIM",
            key: *b"mlst",
            copy_on_duplicate: 0,
            data: vec![9, 9],
        }]);
        layer.extra.push(TaggedBlock::new(*b"shmd", other));
        let mut fx = Descriptor::new("null");
        fx.push("masterFXSwitch", Value::Bool(true)).unwrap();
        let states = vec![
            PsdCompLayerState {
                comp_id: 11,
                visible: Some(false),
                offset: Some((3, -4)),
                blend_mode: Some(BlendMode::Multiply),
                opacity: Some(0.5),
                fill_opacity: Some(0.25),
                effects: Some(fx),
            },
            PsdCompLayerState {
                visible: Some(true),
                ..PsdCompLayerState::new(12)
            },
        ];
        set_layer_states(&mut layer, &states).unwrap();
        let back = layer_states(&layer, &ReadOptions::default()).unwrap();
        assert_eq!(back[0], states[0]);
        // The second entry inherits what it does not state.
        assert_eq!(back[1].visible, Some(true));
        assert_eq!(back[1].offset, Some((3, -4)));
        let items = read_shmd(&layer.extra[0].data).unwrap();
        assert!(items.iter().any(|i| &i.key == b"mlst" && i.data == [9, 9]));
        set_layer_states(&mut layer, &[]).unwrap();
        assert!(layer_states(&layer, &ReadOptions::default())
            .unwrap()
            .is_empty());
        assert!(!layer.extra.is_empty(), "the other metadata item stays");
    }

    /// Resource 1065 and a `cmls` spelled out byte by byte the way
    /// Photoshop lays them out (the `Nm  ` key in its four-character form,
    /// the `cmls` data preceded by an extra word), not produced by
    /// [`resource`] / [`set_layer_states`].
    #[test]
    fn a_hand_built_photoshop_layout_1065_and_cmls_parse() {
        fn key(s: &mut Vec<u8>, k: &str) {
            let n = if k.len() == 4 { 0 } else { k.len() as u32 };
            s.extend_from_slice(&n.to_be_bytes());
            s.extend_from_slice(k.as_bytes());
        }
        fn class(s: &mut Vec<u8>, id: &str, count: u32) {
            s.extend_from_slice(&1u32.to_be_bytes());
            s.extend_from_slice(&0u16.to_be_bytes());
            key(s, id);
            s.extend_from_slice(&count.to_be_bytes());
        }
        fn long(s: &mut Vec<u8>, k: &str, v: i32) {
            key(s, k);
            s.extend_from_slice(b"long");
            s.extend_from_slice(&v.to_be_bytes());
        }
        fn text(s: &mut Vec<u8>, k: &str, v: &str) {
            key(s, k);
            s.extend_from_slice(b"TEXT");
            let u: Vec<u16> = v.encode_utf16().chain([0]).collect();
            s.extend_from_slice(&(u.len() as u32).to_be_bytes());
            for c in u {
                s.extend_from_slice(&c.to_be_bytes());
            }
        }
        let mut r = Vec::new();
        r.extend_from_slice(&16u32.to_be_bytes());
        class(&mut r, "null", 2);
        key(&mut r, "list");
        r.extend_from_slice(b"VlLs");
        r.extend_from_slice(&2u32.to_be_bytes());
        for (id, name, info) in [(1001, "Hero", 7), (1002, "Alt", 3)] {
            r.extend_from_slice(b"Objc");
            class(&mut r, "Comp", 3);
            text(&mut r, "Nm  ", name);
            long(&mut r, "compID", id);
            long(&mut r, "capturedInfo", info);
        }
        long(&mut r, "lastAppliedComp", 1002);
        let parsed = parse(&r, &ReadOptions::default()).unwrap();
        assert_eq!(parsed.comps.len(), 2);
        assert_eq!(parsed.comps[0].name, "Hero");
        assert_eq!(
            parsed.comps[1].captured,
            CAPTURED_VISIBILITY | CAPTURED_POSITION
        );
        assert_eq!(parsed.last_applied, Some(1002));

        let mut c = Vec::new();
        c.extend_from_slice(&0u32.to_be_bytes()); // the extra word
        c.extend_from_slice(&16u32.to_be_bytes());
        class(&mut c, "null", 2);
        long(&mut c, "LyrI", 7);
        key(&mut c, "layerSettings");
        c.extend_from_slice(b"VlLs");
        c.extend_from_slice(&1u32.to_be_bytes());
        c.extend_from_slice(b"Objc");
        class(&mut c, "null", 3);
        key(&mut c, "compList");
        c.extend_from_slice(b"VlLs");
        c.extend_from_slice(&2u32.to_be_bytes());
        for id in [1001i32, 1002] {
            c.extend_from_slice(b"long");
            c.extend_from_slice(&id.to_be_bytes());
        }
        key(&mut c, "enab");
        c.extend_from_slice(b"bool");
        c.push(0);
        key(&mut c, "Ofst");
        c.extend_from_slice(b"Objc");
        class(&mut c, "null", 2);
        long(&mut c, "Hrzn", 40);
        long(&mut c, "Vrtc", 50);
        let mut shmd = Vec::new();
        shmd.extend_from_slice(&1u32.to_be_bytes());
        shmd.extend_from_slice(b"8BIMcmls");
        shmd.extend_from_slice(&[1, 0, 0, 0]);
        shmd.extend_from_slice(&(c.len() as u32).to_be_bytes());
        shmd.extend_from_slice(&c);
        let mut layer = PsdLayer::raster("L", Rect::sized(1, 1));
        layer.extra.push(TaggedBlock::new(*b"shmd", shmd));
        let states = layer_states(&layer, &ReadOptions::default()).unwrap();
        assert_eq!(states.len(), 2, "one entry listing two comps");
        assert_eq!(states[1].comp_id, 1002);
        assert_eq!(states[1].visible, Some(false));
        assert_eq!(states[1].offset, Some((40, 50)));
    }

    #[test]
    fn hostile_payloads_are_errors_not_panics() {
        let opts = ReadOptions::default();
        assert!(parse(&[], &opts).is_err());
        assert!(parse(&[0, 0, 0, 1, 0, 0, 0, 2], &opts).is_err());
        let good = resource(&comps()).unwrap().data;
        for cut in 0..good.len() {
            let _ = parse(&good[..cut], &opts);
        }
        // A shmd claiming four billion items.
        let mut layer = PsdLayer::raster("L", Rect::sized(1, 1));
        layer.extra.push(TaggedBlock::new(
            *b"shmd",
            vec![0xff, 0xff, 0xff, 0xff, 0, 0],
        ));
        assert!(layer_states(&layer, &opts).unwrap().is_empty());
        let mut with = PsdLayer::raster("L", Rect::sized(1, 1));
        set_layer_states(&mut with, &[PsdCompLayerState::new(3)]).unwrap();
        let data = with.extra[0].data.clone();
        for cut in 0..data.len() {
            let mut l = PsdLayer::raster("L", Rect::sized(1, 1));
            l.extra
                .push(TaggedBlock::new(*b"shmd", data[..cut].to_vec()));
            let _ = layer_states(&l, &opts);
        }
    }

    #[test]
    fn number_layers_fills_ids_and_restamps_lyri() {
        let mut g = PsdLayer::group("G");
        let mut child = PsdLayer::raster("C", Rect::sized(1, 1));
        set_layer_states(&mut child, &[PsdCompLayerState::new(1)]).unwrap();
        g.push_child(child).unwrap();
        let mut top = PsdLayer::raster("T", Rect::sized(1, 1));
        top.layer_id = Some(9);
        let mut layers = vec![g, top];
        number_layers(&mut layers, &ReadOptions::default()).unwrap();
        let child = &layers[0].children()[0];
        let id = child.layer_id.unwrap();
        assert!(id > 9 && layers[0].layer_id.unwrap() > 9 && layers[0].layer_id != Some(id));
        let shmd = child.extra.iter().find(|b| &b.key == b"shmd").unwrap();
        let items = read_shmd(&shmd.data).unwrap();
        let d = versioned_descriptor(&items[0].data, &ReadOptions::default()).unwrap();
        assert_eq!(d.number("LyrI"), Some(f64::from(id)));
    }
}
