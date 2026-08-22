//! Pixel content, referenced by content hash.
//!
//! The document never owns pixel bytes. A layer's (or mask's) raster content is
//! a sparse map from [`TileCoord`] to [`TileHash`]; the bytes behind those
//! hashes live in the tile store (`asset-store`). That is what keeps a
//! [`crate::Document`] cheap to clone for history and cheap to serialize, and
//! it is what makes a pixel edit expressible as a small, invertible command:
//! every pixel edit in this crate is a **tile delta** — for each touched
//! coordinate, the hash the tile carries afterwards — and its inverse is the
//! same shape holding the hashes the tiles carried before.
//!
//! A brush stroke that crosses N tiles is therefore *one* [`TileDelta`], hence
//! one command and one undo step, no matter how many tiles it touched.
//!
//! # What a tile holds, and what an absent tile means
//!
//! Two storage shapes, one addressing scheme. Both are hashed with
//! [`raster::TileHash::of`] over their raw bytes, which is exactly what
//! [`raster::Tile::hash`] does, so a hash computed here is the hash the tile
//! store computes for the same bytes.
//!
//! | target | one tile is | absent reads as |
//! |---|---|---|
//! | layer ([`PixelKey::Layer`]) | `TILE_SIZE²` pixels of [`PixelFormat::Rgba8`] | fully transparent |
//! | mask ([`PixelKey::Mask`]) | [`MASK_TILE_BYTES`] 8-bit coverage samples ([`layer_model::MaskKind::Raster`]'s "8-bit grayscale tile set") | **zero coverage** |
//!
//! Both "absent" rows are the same rule — an absent tile is the all-zero tile —
//! and that rule is set by the layers underneath: `raster`'s grid documents an
//! absent tile as fully transparent, and
//! [`layer_model::LayerMask::coverage`] maps a 0.0 sample to 0.0, which hides
//! the layer completely.
//!
//! The consequence for commands is not symmetric, and this is deliberate:
//! removing a *layer* tile erases it back to nothing, while removing a *mask*
//! tile means "hide the layer here". So
//! [`crate::Command::clear_region`] is layer-only, and revealing through a mask
//! is a fill with [`MaskCoverage::REVEALED`] rather than a clear. See
//! [`FillValue`].
//!
//! # Mip levels
//! Tile deltas address whatever [`TileCoord::level`] the caller names, so a
//! paint command may carry refreshed mip tiles alongside the level-0 edit. The
//! *region* helpers ([`tiles_covering`], [`tile_intersects_region`]) are
//! level-0 only: a pixel rect has no meaning at a level whose pixels are a
//! different size.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use layer_model::{LayerId, LayerTree, MaskId};
use raster::{PixelFormat, PixelRect, Tile, TileCoord, TileHash, TILE_SIZE};

/// Upper bound on how many tiles one region command may resolve to.
///
/// [`PixelRect`] dimensions are `u32`, so a caller-supplied rect can name more
/// tiles than exist atoms to store them. A command that would allocate an
/// entry per tile is refused rather than attempted.
pub const MAX_REGION_TILES: u64 = 1 << 20;

/// Rejections raised while building or storing tile references.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PixelError {
    #[error("tile {coord:?} appears more than once in one tile delta")]
    DuplicateTile { coord: TileCoord },
    #[error("pixel store names the same target twice: {target:?}")]
    DuplicateTarget { target: PixelKey },
    #[error("region resolves to {tiles} tiles, more than the {max} a command may carry")]
    RegionTooLarge { tiles: u64, max: u64 },
    #[error("region extends past the addressable tile grid")]
    RegionOutOfRange,
    #[error("tile {coord:?} lies outside the region being edited")]
    TileOutsideRegion { coord: TileCoord },
}

/// What a pixel edit addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PixelTarget {
    /// The layer's own pixels.
    ///
    /// Only a layer *kind* that owns pixels is addressable:
    /// [`layer_model::LayerKind::Raster`] and
    /// [`layer_model::LayerKind::Generator`] (an AI result is rasterized output
    /// the user retouches in place). Every other kind derives its appearance
    /// from something else — a group from its children, an adjustment from what
    /// is beneath it, text and shape from their own parametric description, a
    /// smart object from the document it renders — so a tile stored under one
    /// would never be composited and would never be swept up
    /// ([`PixelStore::retain_referenced`] asks only whether the layer still
    /// exists). Applying it to such a layer is
    /// [`crate::CommandError::NotPaintable`].
    ///
    /// A [`PixelTarget::Mask`] has no such restriction: any kind may carry a
    /// mask, and masking an adjustment layer is its most common use.
    Layer(LayerId),
    /// The coverage data of the mask currently attached to this layer.
    ///
    /// Resolved to the mask's [`MaskId`] at apply time, so the command names a
    /// layer (stable, user-visible) while the store is keyed by the mask
    /// (stable across the layer being renamed, moved, or re-parented).
    /// Applying it to a layer with no mask is
    /// [`crate::CommandError::NoMask`].
    Mask(LayerId),
}

/// A resolved pixel-store key: what a [`PixelTarget`] points at once the
/// document has been consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PixelKey {
    Layer(LayerId),
    Mask(MaskId),
}

impl PixelKey {
    /// Total order over keys, used only to make serialization deterministic.
    /// Uuids have no meaningful order, so this is arbitrary but stable.
    fn order(&self) -> (u8, [u8; 16]) {
        match self {
            PixelKey::Layer(id) => (0, *id.0.as_bytes()),
            PixelKey::Mask(id) => (1, *id.0.as_bytes()),
        }
    }
}

/// One tile's content after an edit.
///
/// `hash: None` removes the tile from the map, leaving the store's zero value
/// there: fully transparent for a layer, **zero coverage** (the layer fully
/// hidden) for a mask. See the module's table — for a mask that is a
/// meaningful edit, not an erase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileEdit {
    pub coord: TileCoord,
    pub hash: Option<TileHash>,
}

impl TileEdit {
    /// The tile at `coord` becomes `hash`.
    pub fn set(coord: TileCoord, hash: TileHash) -> Self {
        Self {
            coord,
            hash: Some(hash),
        }
    }

    /// The tile at `coord` is removed from the map.
    pub fn clear(coord: TileCoord) -> Self {
        Self { coord, hash: None }
    }
}

/// A set of tile edits applied as one unit.
///
/// # Invariants
/// At most one edit per [`TileCoord`], and the edits are held in ascending
/// coordinate order. Both hold after deserialization too — the wire form is a
/// plain list and is re-checked on the way in — so a delta and its inverse
/// always name the same coordinate set, which is what makes undo exact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "Vec<TileEdit>", try_from = "Vec<TileEdit>")]
pub struct TileDelta {
    edits: Vec<TileEdit>,
}

impl TryFrom<Vec<TileEdit>> for TileDelta {
    type Error = PixelError;

    fn try_from(edits: Vec<TileEdit>) -> Result<Self, Self::Error> {
        TileDelta::new(edits)
    }
}

impl From<TileDelta> for Vec<TileEdit> {
    fn from(d: TileDelta) -> Self {
        d.edits
    }
}

impl TileDelta {
    /// Build a delta, sorting the edits and refusing a repeated coordinate.
    ///
    /// A repeat is refused rather than last-write-wins because the inverse can
    /// only capture one prior value per coordinate: silently collapsing two
    /// edits would produce an inverse that does not undo the delta.
    pub fn new(edits: impl IntoIterator<Item = TileEdit>) -> Result<Self, PixelError> {
        let mut edits: Vec<TileEdit> = edits.into_iter().collect();
        edits.sort_by_key(|e| e.coord);
        if let Some(w) = edits.windows(2).find(|w| w[0].coord == w[1].coord) {
            return Err(PixelError::DuplicateTile { coord: w[0].coord });
        }
        Ok(Self { edits })
    }

    /// A delta touching exactly one tile.
    pub fn single(edit: TileEdit) -> Self {
        Self { edits: vec![edit] }
    }

    pub fn edits(&self) -> &[TileEdit] {
        &self.edits
    }

    pub fn len(&self) -> usize {
        self.edits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// The post-edit content of `coord`, or `None` if this delta does not touch
    /// it. The inner `Option` is the tile's new hash (`None` = removed).
    pub fn get(&self, coord: TileCoord) -> Option<Option<TileHash>> {
        self.edits
            .binary_search_by_key(&coord, |e| e.coord)
            .ok()
            .map(|i| self.edits[i].hash)
    }

    pub fn iter(&self) -> impl Iterator<Item = &TileEdit> {
        self.edits.iter()
    }
}

/// Serialized form of one live tile reference.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct TileEntry {
    coord: TileCoord,
    hash: TileHash,
}

/// The tile references of one layer or mask: a sparse `coord -> hash` map.
///
/// Sparse on purpose — an absent coordinate is not "transparent black stored
/// cheaply", it is *nothing stored*, which is what lets a layer sit on a huge
/// canvas while owning only the tiles that were painted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "Vec<TileEntry>", try_from = "Vec<TileEntry>")]
pub struct TileMap {
    tiles: BTreeMap<TileCoord, TileHash>,
}

impl TryFrom<Vec<TileEntry>> for TileMap {
    type Error = PixelError;

    fn try_from(v: Vec<TileEntry>) -> Result<Self, Self::Error> {
        let mut tiles = BTreeMap::new();
        for e in v {
            if tiles.insert(e.coord, e.hash).is_some() {
                return Err(PixelError::DuplicateTile { coord: e.coord });
            }
        }
        Ok(Self { tiles })
    }
}

impl From<TileMap> for Vec<TileEntry> {
    fn from(m: TileMap) -> Self {
        m.tiles
            .into_iter()
            .map(|(coord, hash)| TileEntry { coord, hash })
            .collect()
    }
}

impl TileMap {
    pub fn get(&self, coord: TileCoord) -> Option<TileHash> {
        self.tiles.get(&coord).copied()
    }

    pub fn contains(&self, coord: TileCoord) -> bool {
        self.tiles.contains_key(&coord)
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Tile references in ascending coordinate order.
    pub fn iter(&self) -> impl Iterator<Item = (TileCoord, TileHash)> + '_ {
        self.tiles.iter().map(|(c, h)| (*c, *h))
    }

    /// Apply a delta, returning the delta that restores the previous state.
    ///
    /// The returned delta names exactly the coordinates `delta` named, so
    /// applying it afterwards is an exact reversal — including coordinates that
    /// held nothing before, which come back as [`TileEdit::clear`] rather than
    /// being left behind.
    pub fn apply_delta(&mut self, delta: &TileDelta) -> TileDelta {
        let mut prev = Vec::with_capacity(delta.len());
        for edit in delta.iter() {
            let before = self.tiles.get(&edit.coord).copied();
            prev.push(TileEdit {
                coord: edit.coord,
                hash: before,
            });
            match edit.hash {
                Some(h) => {
                    self.tiles.insert(edit.coord, h);
                }
                None => {
                    self.tiles.remove(&edit.coord);
                }
            }
        }
        // `delta` has unique, ascending coords, so `prev` does too.
        TileDelta { edits: prev }
    }
}

/// Serialized form of the store: a list rather than a map, so no format has to
/// support structured map keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PixelStoreRepr {
    maps: Vec<PixelMapEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PixelMapEntry {
    key: PixelKey,
    tiles: TileMap,
}

/// Every tile reference in a document, keyed by what owns it.
///
/// # Retention
/// A map is dropped the moment it becomes empty, so "paint, then undo" returns
/// the store to exactly its prior value (see the `PartialEq` on
/// [`crate::Document`], which the atomicity tests rely on).
///
/// A map whose owner has been deleted is *not* dropped automatically: the
/// deletion is undoable, and the pixels have to survive for the undo to be
/// exact. [`PixelStore::retain_referenced`] is the explicit sweep, and it must
/// only run when the relevant history has been discarded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "PixelStoreRepr", try_from = "PixelStoreRepr")]
pub struct PixelStore {
    maps: HashMap<PixelKey, TileMap>,
}

impl TryFrom<PixelStoreRepr> for PixelStore {
    type Error = PixelError;

    fn try_from(r: PixelStoreRepr) -> Result<Self, Self::Error> {
        let mut maps = HashMap::with_capacity(r.maps.len());
        for entry in r.maps {
            if maps.insert(entry.key, entry.tiles).is_some() {
                return Err(PixelError::DuplicateTarget { target: entry.key });
            }
        }
        Ok(Self { maps })
    }
}

impl From<PixelStore> for PixelStoreRepr {
    fn from(s: PixelStore) -> Self {
        let mut maps: Vec<PixelMapEntry> = s
            .maps
            .into_iter()
            .map(|(key, tiles)| PixelMapEntry { key, tiles })
            .collect();
        // Deterministic output: two stores with equal contents must serialize
        // to identical bytes, or a content-addressed save is not reproducible.
        maps.sort_by_key(|e| e.key.order());
        Self { maps }
    }
}

impl PixelStore {
    /// The tile references of one target, or `None` if it owns no pixels.
    pub fn tiles(&self, key: PixelKey) -> Option<&TileMap> {
        self.maps.get(&key)
    }

    /// One tile's content hash.
    pub fn tile(&self, key: PixelKey, coord: TileCoord) -> Option<TileHash> {
        self.maps.get(&key).and_then(|m| m.get(coord))
    }

    /// Number of targets that own at least one tile.
    pub fn len(&self) -> usize {
        self.maps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = PixelKey> + '_ {
        self.maps.keys().copied()
    }

    /// Total tiles referenced across every target.
    pub fn tile_count(&self) -> usize {
        self.maps.values().map(|m| m.len()).sum()
    }

    /// Apply a tile delta to one target, returning the delta that restores it.
    ///
    /// Creates the target's map on demand and drops it again if the edit leaves
    /// it empty, so an edit and its inverse round-trip the whole store to an
    /// equal value.
    pub fn apply(&mut self, key: PixelKey, delta: &TileDelta) -> TileDelta {
        let map = self.maps.entry(key).or_default();
        let inverse = map.apply_delta(delta);
        if map.is_empty() {
            self.maps.remove(&key);
        }
        inverse
    }

    /// Drop the tile references of layers and masks that `tree` no longer
    /// contains, returning how many targets were dropped.
    ///
    /// **Destroys undo exactness** for any still-undoable command that touched
    /// a dropped target — a deleted layer's pixels are what its undo restores.
    /// Call it only after the corresponding history has been cleared.
    pub fn retain_referenced(&mut self, tree: &LayerTree) -> usize {
        let live_masks: Vec<MaskId> = tree
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| tree.get(id).and_then(|l| l.mask_id()))
            .collect();
        let before = self.maps.len();
        self.maps.retain(|key, _| match key {
            PixelKey::Layer(id) => tree.contains(*id),
            PixelKey::Mask(id) => live_masks.contains(id),
        });
        before - self.maps.len()
    }
}

/// serde adapter for [`raster::PixelRect`], which is not itself serializable.
/// Stored as its four components, so the wire form is just the struct.
pub mod pixel_rect_serde {
    use raster::PixelRect;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct Repr {
        x: i64,
        y: i64,
        width: u32,
        height: u32,
    }

    pub fn serialize<S: Serializer>(r: &PixelRect, s: S) -> Result<S::Ok, S::Error> {
        Repr {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        }
        .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PixelRect, D::Error> {
        let r = Repr::deserialize(d)?;
        Ok(PixelRect::new(r.x, r.y, r.width, r.height))
    }
}

/// A solid fill color: straight (non-premultiplied) 8-bit RGBA, matching
/// [`PixelFormat::Rgba8`] — the only storage format v1 writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FillColor(pub [u8; 4]);

impl FillColor {
    /// Fully transparent — the value [`crate::Command::clear_region`] fills
    /// with conceptually, though a clear removes tiles instead of storing them.
    pub const TRANSPARENT: FillColor = FillColor([0, 0, 0, 0]);

    /// Content hash of a full `TILE_SIZE` square of this color.
    ///
    /// Derived through [`raster::Tile`] so it is byte-for-byte the hash the
    /// tile store computes for the same pixels. This is what lets a fill name
    /// its interior tiles without reading, writing, or hashing anything in the
    /// store first.
    pub fn solid_tile_hash(self) -> TileHash {
        let pixels = TILE_SIZE as usize * TILE_SIZE as usize;
        let mut data = Vec::with_capacity(pixels * 4);
        for _ in 0..pixels {
            data.extend_from_slice(&self.0);
        }
        Tile::from_bytes(PixelFormat::Rgba8, data)
            .expect("buffer length is computed from the format itself")
            .hash()
    }
}

/// Bytes in one mask tile: `TILE_SIZE²` single-byte coverage samples.
///
/// A mask tile is *not* an RGBA tile. [`layer_model::MaskKind::Raster`] is an
/// 8-bit grayscale tile set on the same grid, so one mask tile is a quarter the
/// size of one [`PixelFormat::Rgba8`] tile.
pub const MASK_TILE_BYTES: usize = TILE_SIZE as usize * TILE_SIZE as usize;

/// One 8-bit mask coverage sample: 0 hides the layer, 255 reveals it.
///
/// This is a raw stored sample, before [`layer_model::LayerMask`]'s `inverted`,
/// `density` and `feather_px` are applied — those live on the mask, not in the
/// tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MaskCoverage(pub u8);

impl MaskCoverage {
    /// The layer is fully hidden here. Identical in meaning to an *absent*
    /// mask tile (see the module's table), but stored explicitly.
    pub const HIDDEN: MaskCoverage = MaskCoverage(0);

    /// The layer shows through completely — the value that "erases" a mask in
    /// the sense a user means when they paint white on it.
    pub const REVEALED: MaskCoverage = MaskCoverage(255);

    /// Content hash of a full tile of this coverage value.
    ///
    /// Hashed over the raw grayscale bytes with the same
    /// [`raster::TileHash::of`] the tile store applies, so a fill can name its
    /// interior tiles without rasterizing them.
    pub fn solid_tile_hash(self) -> TileHash {
        TileHash::of(&vec![self.0; MASK_TILE_BYTES])
    }
}

/// The value a [`crate::Command::FillRegion`] writes into every tile it fully
/// covers.
///
/// The variant has to match the target: a layer stores RGBA pixels and a mask
/// stores 8-bit coverage, so filling a mask with an [`FillColor`] would store a
/// hash of four-byte pixels where the compositor expects one-byte samples.
/// [`crate::Command::apply`] refuses the mismatch
/// ([`crate::CommandError::FillValueMismatch`]) rather than storing an
/// incoherent hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FillValue {
    /// Fills a [`PixelTarget::Layer`].
    Color(FillColor),
    /// Fills a [`PixelTarget::Mask`].
    Coverage(MaskCoverage),
}

impl FillValue {
    /// Content hash of a full tile of this value, in the target's own storage
    /// format.
    pub fn solid_tile_hash(self) -> TileHash {
        match self {
            FillValue::Color(c) => c.solid_tile_hash(),
            FillValue::Coverage(m) => m.solid_tile_hash(),
        }
    }

    /// `true` when this value is storable in `target`.
    pub fn matches(self, target: PixelTarget) -> bool {
        matches!(
            (self, target),
            (FillValue::Color(_), PixelTarget::Layer(_))
                | (FillValue::Coverage(_), PixelTarget::Mask(_))
        )
    }
}

impl From<FillColor> for FillValue {
    fn from(c: FillColor) -> Self {
        FillValue::Color(c)
    }
}

impl From<MaskCoverage> for FillValue {
    fn from(m: MaskCoverage) -> Self {
        FillValue::Coverage(m)
    }
}

/// How much of a tile a region covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// The region covers the whole tile; its content is decided by the region
    /// alone and can be derived without reading the layer.
    Full,
    /// The region covers part of the tile; the result depends on the pixels
    /// already there, so only the caller (which owns the bytes) can supply it.
    Partial,
}

/// Every level-0 tile a pixel rect touches, with how much of each it covers.
///
/// Returns nothing for an empty rect. Fails rather than allocating when the
/// rect names more than [`MAX_REGION_TILES`] tiles or reaches past the `i32`
/// tile grid.
pub fn tiles_covering(rect: PixelRect) -> Result<Vec<(TileCoord, Coverage)>, PixelError> {
    if rect.is_empty() {
        return Ok(Vec::new());
    }
    let t = TILE_SIZE as i64;
    let x0 = rect.x.div_euclid(t);
    let x1 = (rect.right() - 1).div_euclid(t);
    let y0 = rect.y.div_euclid(t);
    let y1 = (rect.bottom() - 1).div_euclid(t);

    for v in [x0, x1, y0, y1] {
        if v < i32::MIN as i64 || v > i32::MAX as i64 {
            return Err(PixelError::RegionOutOfRange);
        }
    }
    let nx = (x1 - x0 + 1) as u64;
    let ny = (y1 - y0 + 1) as u64;
    let total = nx.checked_mul(ny).ok_or(PixelError::RegionTooLarge {
        tiles: u64::MAX,
        max: MAX_REGION_TILES,
    })?;
    if total > MAX_REGION_TILES {
        return Err(PixelError::RegionTooLarge {
            tiles: total,
            max: MAX_REGION_TILES,
        });
    }

    let mut out = Vec::with_capacity(total as usize);
    for ty in y0..=y1 {
        for tx in x0..=x1 {
            let ox = tx * t;
            let oy = ty * t;
            let full = ox >= rect.x
                && ox + t <= rect.right()
                && oy >= rect.y
                && oy + t <= rect.bottom();
            out.push((
                TileCoord::new(tx as i32, ty as i32, 0),
                if full { Coverage::Full } else { Coverage::Partial },
            ));
        }
    }
    Ok(out)
}

/// The level-0 tiles a region command cannot decide by itself.
///
/// A region edit ([`crate::Command::fill_region`],
/// [`crate::Command::clear_region`]) derives content only for the tiles its rect
/// covers *entirely*, because a solid tile is content-addressable without
/// reading anything. A tile the rect merely *clips* keeps whatever lies outside
/// the rect, so its post-edit content depends on bytes only the caller holds —
/// and a region command that carries no edit for such a tile leaves it
/// untouched rather than guessing.
///
/// This is the list of exactly those coordinates. A caller that wants a fill to
/// reach the edges of its rect rasterizes these and passes the results as
/// `edges`. The list is empty for a tile-aligned rect and holds *every*
/// coordinate for a rect smaller than one tile — which is why a small marquee
/// fill with no `edges` resolves to nothing at all.
pub fn edge_tiles(rect: PixelRect) -> Result<Vec<TileCoord>, PixelError> {
    Ok(tiles_covering(rect)?
        .into_iter()
        .filter(|(_, cov)| *cov == Coverage::Partial)
        .map(|(coord, _)| coord)
        .collect())
}

/// Whether a level-0 tile overlaps a pixel rect at all.
///
/// Tiles at any other mip level report `false`: a pixel rect addresses level-0
/// pixels, so a region command has nothing to say about a coarser tile.
pub fn tile_intersects_region(rect: PixelRect, coord: TileCoord) -> bool {
    if rect.is_empty() || coord.level != 0 {
        return false;
    }
    let t = TILE_SIZE as i64;
    let (ox, oy) = coord.pixel_origin();
    ox < rect.right() && oy < rect.bottom() && ox + t > rect.x && oy + t > rect.y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(seed: u8) -> TileHash {
        TileHash([seed; 32])
    }

    fn c(x: i32, y: i32) -> TileCoord {
        TileCoord::new(x, y, 0)
    }

    #[test]
    fn a_delta_refuses_a_repeated_coordinate() {
        let err = TileDelta::new([TileEdit::set(c(0, 0), h(1)), TileEdit::set(c(0, 0), h(2))])
            .unwrap_err();
        assert_eq!(err, PixelError::DuplicateTile { coord: c(0, 0) });
    }

    #[test]
    fn a_delta_is_held_in_ascending_coordinate_order() {
        let d = TileDelta::new([
            TileEdit::set(c(3, 1), h(1)),
            TileEdit::set(c(0, 9), h(2)),
            TileEdit::clear(c(1, 0)),
        ])
        .unwrap();
        let coords: Vec<TileCoord> = d.iter().map(|e| e.coord).collect();
        assert_eq!(coords, vec![c(0, 9), c(1, 0), c(3, 1)]);
        assert_eq!(d.get(c(1, 0)), Some(None));
        assert_eq!(d.get(c(3, 1)), Some(Some(h(1))));
        assert_eq!(d.get(c(7, 7)), None);
    }

    #[test]
    fn applying_a_delta_yields_the_delta_that_restores_it() {
        let mut map = TileMap::default();
        map.apply_delta(&TileDelta::single(TileEdit::set(c(0, 0), h(1))));

        let forward = TileDelta::new([
            // one tile that already held something...
            TileEdit::set(c(0, 0), h(9)),
            // ...and one that did not.
            TileEdit::set(c(5, 5), h(8)),
        ])
        .unwrap();
        let inverse = map.apply_delta(&forward);
        assert_eq!(map.get(c(0, 0)), Some(h(9)));
        assert_eq!(map.get(c(5, 5)), Some(h(8)));

        // The previously-empty tile must come back as *empty*, not be left
        // behind, or undo would silently keep the painted tile.
        assert_eq!(inverse.get(c(5, 5)), Some(None));
        assert_eq!(inverse.get(c(0, 0)), Some(Some(h(1))));

        map.apply_delta(&inverse);
        assert_eq!(map.get(c(0, 0)), Some(h(1)));
        assert!(!map.contains(c(5, 5)));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn the_store_drops_a_target_whose_last_tile_is_erased() {
        let key = PixelKey::Layer(LayerId::new());
        let mut store = PixelStore::default();
        let inverse = store.apply(key, &TileDelta::single(TileEdit::set(c(0, 0), h(1))));
        assert_eq!(store.len(), 1);

        store.apply(key, &inverse);
        assert!(
            store.is_empty(),
            "an emptied map must be dropped, or undo cannot restore the store to an equal value"
        );
        assert_eq!(store, PixelStore::default());
    }

    #[test]
    fn store_roundtrips_through_json_deterministically() {
        let mut store = PixelStore::default();
        for i in 0..4u8 {
            store.apply(
                PixelKey::Layer(LayerId::new()),
                &TileDelta::single(TileEdit::set(c(i as i32, 0), h(i))),
            );
        }
        store.apply(
            PixelKey::Mask(MaskId::new()),
            &TileDelta::single(TileEdit::set(c(1, 1), h(7))),
        );
        let json = serde_json::to_string(&store).unwrap();
        let back: PixelStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back, store);
        assert_eq!(
            serde_json::to_string(&back).unwrap(),
            json,
            "serialization must not depend on hash-map iteration order"
        );
    }

    #[test]
    fn a_store_naming_a_target_twice_fails_to_load() {
        let key = PixelKey::Layer(LayerId::new());
        let repr = PixelStoreRepr {
            maps: vec![
                PixelMapEntry {
                    key,
                    tiles: TileMap::default(),
                },
                PixelMapEntry {
                    key,
                    tiles: TileMap::default(),
                },
            ],
        };
        assert_eq!(
            PixelStore::try_from(repr).unwrap_err(),
            PixelError::DuplicateTarget { target: key }
        );
    }

    #[test]
    fn retain_referenced_drops_only_dead_targets() {
        use layer_model::{Layer, LayerMask};

        let mut tree = LayerTree::new();
        let mut live = Layer::raster("live");
        let mask_id = MaskId::new();
        live.set_mask(LayerMask::new(mask_id));
        let live_id = live.id;
        tree.push_root(live).unwrap();
        let dead_id = LayerId::new();

        let mut store = PixelStore::default();
        for key in [
            PixelKey::Layer(live_id),
            PixelKey::Mask(mask_id),
            PixelKey::Layer(dead_id),
            PixelKey::Mask(MaskId::new()),
        ] {
            store.apply(key, &TileDelta::single(TileEdit::set(c(0, 0), h(1))));
        }
        assert_eq!(store.len(), 4);
        assert_eq!(store.retain_referenced(&tree), 2);
        assert!(store.tiles(PixelKey::Layer(live_id)).is_some());
        assert!(store.tiles(PixelKey::Mask(mask_id)).is_some());
        assert!(store.tiles(PixelKey::Layer(dead_id)).is_none());
    }

    #[test]
    fn a_solid_tile_hash_matches_what_the_tile_store_computes() {
        assert_eq!(
            FillColor::TRANSPARENT.solid_tile_hash(),
            Tile::transparent(PixelFormat::Rgba8).hash()
        );
        let red = FillColor([255, 0, 0, 255]);
        let mut manual = Tile::transparent(PixelFormat::Rgba8);
        for px in manual.data_mut().chunks_exact_mut(4) {
            px.copy_from_slice(&[255, 0, 0, 255]);
        }
        assert_eq!(red.solid_tile_hash(), manual.hash());
        assert_ne!(red.solid_tile_hash(), FillColor::TRANSPARENT.solid_tile_hash());
    }

    #[test]
    fn a_mask_tile_is_grayscale_and_is_not_an_rgba_tile() {
        // The convention this pins: a mask tile is MASK_TILE_BYTES 8-bit
        // samples hashed by the same rule `raster::Tile` uses, so it can never
        // be confused with the four-byte-per-pixel tile of the same coverage.
        assert_eq!(MASK_TILE_BYTES, TILE_SIZE as usize * TILE_SIZE as usize);
        assert_eq!(
            MaskCoverage::HIDDEN.solid_tile_hash(),
            TileHash::of(&[0u8; TILE_SIZE as usize * TILE_SIZE as usize])
        );
        assert_eq!(
            MaskCoverage::REVEALED.solid_tile_hash(),
            TileHash::of(&[255u8; TILE_SIZE as usize * TILE_SIZE as usize])
        );
        assert_ne!(MaskCoverage::HIDDEN.solid_tile_hash(), MaskCoverage::REVEALED.solid_tile_hash());
        assert_ne!(
            MaskCoverage::HIDDEN.solid_tile_hash(),
            FillColor::TRANSPARENT.solid_tile_hash(),
            "an all-zero mask tile and an all-zero RGBA tile are different bytes"
        );
        assert_eq!(
            Tile::byte_len(PixelFormat::Rgba8),
            MASK_TILE_BYTES * 4,
            "a mask tile is a quarter the size of an RGBA tile"
        );
    }

    #[test]
    fn a_fill_value_only_matches_its_own_kind_of_target() {
        let layer = PixelTarget::Layer(LayerId::new());
        let mask = PixelTarget::Mask(LayerId::new());
        let color = FillValue::from(FillColor([1, 2, 3, 4]));
        let coverage = FillValue::from(MaskCoverage::REVEALED);

        assert!(color.matches(layer) && !color.matches(mask));
        assert!(coverage.matches(mask) && !coverage.matches(layer));
        assert_eq!(color.solid_tile_hash(), FillColor([1, 2, 3, 4]).solid_tile_hash());
        assert_eq!(coverage.solid_tile_hash(), MaskCoverage::REVEALED.solid_tile_hash());
    }

    #[test]
    fn tiles_covering_separates_interior_from_edge() {
        let t = TILE_SIZE as i64;
        // Exactly one whole tile.
        let full = tiles_covering(PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE)).unwrap();
        assert_eq!(full, vec![(c(0, 0), Coverage::Full)]);

        // Offset by one pixel: four tiles, none of them whole.
        let straddle = tiles_covering(PixelRect::new(1, 1, TILE_SIZE, TILE_SIZE)).unwrap();
        assert_eq!(straddle.len(), 4);
        assert!(straddle.iter().all(|(_, cov)| *cov == Coverage::Partial));

        // A 3x1 run starting one pixel before a tile boundary: the middle tile
        // is whole, the two it clips are not.
        let run = tiles_covering(PixelRect::new(t - 1, 0, TILE_SIZE + 2, TILE_SIZE)).unwrap();
        assert_eq!(
            run,
            vec![
                (c(0, 0), Coverage::Partial),
                (c(1, 0), Coverage::Full),
                (c(2, 0), Coverage::Partial),
            ]
        );

        // Negative coordinates address tiles left of the origin.
        let left = tiles_covering(PixelRect::new(-t, 0, TILE_SIZE, TILE_SIZE)).unwrap();
        assert_eq!(left, vec![(c(-1, 0), Coverage::Full)]);

        assert!(tiles_covering(PixelRect::new(0, 0, 0, 10))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn edge_tiles_names_exactly_the_tiles_a_region_edit_cannot_derive() {
        let t = TILE_SIZE as i64;

        // Tile-aligned: nothing for the caller to supply.
        assert!(edge_tiles(PixelRect::new(0, 0, TILE_SIZE * 2, TILE_SIZE))
            .unwrap()
            .is_empty());

        // Straddling a tile boundary: both clipped tiles, and only those.
        assert_eq!(
            edge_tiles(PixelRect::new(t - 4, 0, 8, TILE_SIZE)).unwrap(),
            vec![c(0, 0), c(1, 0)]
        );

        // Smaller than one tile: the single tile it sits in. This is the case
        // that would otherwise silently resolve to an empty fill.
        assert_eq!(
            edge_tiles(PixelRect::new(4, 4, 8, 8)).unwrap(),
            vec![c(0, 0)]
        );

        // A run whose middle tile is whole: the whole one is *not* listed.
        assert_eq!(
            edge_tiles(PixelRect::new(t - 1, 0, TILE_SIZE + 2, TILE_SIZE)).unwrap(),
            vec![c(0, 0), c(2, 0)]
        );

        assert!(edge_tiles(PixelRect::new(0, 0, 0, 0)).unwrap().is_empty());
        assert!(matches!(
            edge_tiles(PixelRect::new(0, 0, u32::MAX, u32::MAX)),
            Err(PixelError::RegionTooLarge { .. })
        ));
    }

    #[test]
    fn tiles_covering_refuses_an_absurd_region_instead_of_allocating_it() {
        let err = tiles_covering(PixelRect::new(0, 0, u32::MAX, u32::MAX)).unwrap_err();
        assert!(matches!(err, PixelError::RegionTooLarge { .. }));
    }

    #[test]
    fn region_intersection_ignores_other_mip_levels() {
        let rect = PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE);
        assert!(tile_intersects_region(rect, c(0, 0)));
        assert!(!tile_intersects_region(rect, c(1, 0)));
        assert!(!tile_intersects_region(rect, TileCoord::new(0, 0, 1)));
        assert!(!tile_intersects_region(PixelRect::new(0, 0, 0, 0), c(0, 0)));
    }
}
