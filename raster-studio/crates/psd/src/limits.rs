//! Ceilings the reader applies to file-supplied numbers.
//!
//! Every field in this module exists because a `.psd` gets to choose a number
//! and this crate refuses to turn that number straight into an allocation. The
//! checks are *pre*-allocation: a declared length is compared against a ceiling
//! before any `Vec` is grown, so an absurd value costs a comparison, not a
//! gigabyte.
//!
//! [`Budget`] is the part that per-field ceilings cannot express. A file with
//! eight thousand small layers passes every individual check while still
//! asking for far more memory than the sum of any one of them suggests, so the
//! reader draws every pixel allocation from one shrinking pool.

use crate::error::{PsdError, PsdResult};

/// Ceilings applied while reading. Tests drive these down to prove a refusal
/// happens before an allocation; callers can raise them for known-good files.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ReadOptions {
    /// Largest canvas or layer edge, in pixels. Photoshop's own PSD ceiling is
    /// 30 000.
    pub max_dimension: u32,
    /// Largest number of layer records the file may declare.
    pub max_layers: usize,
    /// Largest number of channels one layer record may declare.
    pub max_channels_per_layer: usize,
    /// Deepest group nesting the reader will build a tree for.
    ///
    /// A group costs only two layer records, so [`ReadOptions::max_layers`]
    /// alone permits a tree thousands of levels deep — and every consumer of
    /// that tree, including the implicit `Drop`, would then have to be
    /// stack-safe forever. Photoshop's own limit is ten; this one is generous
    /// and still bounded.
    pub max_group_depth: usize,
    /// Total decoded pixel bytes the whole read may produce.
    pub max_decoded_bytes: u64,
    /// Largest image-resources section that will be retained.
    pub max_resource_bytes: usize,
    /// Largest single tagged block that will be retained.
    pub max_tagged_block_bytes: usize,
    /// Largest layer name, in UTF-16 code units.
    pub max_name_units: usize,
    /// Deepest descriptor nesting accepted.
    pub max_descriptor_depth: usize,
    /// Largest item count one descriptor or list may declare.
    pub max_descriptor_items: usize,
}

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            max_dimension: 30_000,
            max_layers: 8_192,
            max_channels_per_layer: 64,
            max_group_depth: 64,
            max_decoded_bytes: 1 << 30, // 1 GiB
            // These two are generous because they are backstops, not the real
            // bound: a resource or tagged block is copied out of a sub-cursor,
            // so its size is already capped by the bytes the file actually
            // contains. The ceiling exists to refuse a four-gigabyte *claim*
            // before the copy is attempted.
            max_resource_bytes: 256 << 20,
            max_tagged_block_bytes: 512 << 20,
            max_name_units: 4_096,
            max_descriptor_depth: 32,
            max_descriptor_items: 8_192,
        }
    }
}

/// How a document is encoded on the way out.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct WriteOptions {
    /// Encoding for layer channel data.
    pub layer_compression: crate::codec::Compression,
    /// Encoding for the merged composite. Photoshop and Photopea both read all
    /// four, but RLE is what Photoshop itself writes.
    pub merged_compression: crate::codec::Compression,
    /// Write a 72 dpi `ResolutionInfo` resource when the document carries none.
    /// Photoshop shows a document with no resolution resource at 1 dpi.
    pub synthesize_resolution: bool,
    /// Peak bytes [`crate::flatten::flatten_with`] may hold while synthesising
    /// a merged composite for a document that has none.
    ///
    /// The canvas the flattener works on costs sixteen bytes per pixel, and an
    /// isolated group needs a second one, so the cost is set by the header —
    /// which a caller can hand over without ever having read a file that large.
    /// A header alone must not be able to ask the allocator for fourteen
    /// gigabytes, so the flattener refuses before it reserves anything.
    ///
    /// The default allows roughly an 11 000 × 11 000 canvas. A caller with a
    /// larger document either raises this or supplies
    /// [`crate::model::PsdFile::merged`] from its own renderer, which skips the
    /// fallback flattener entirely.
    pub max_flatten_bytes: u64,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            layer_compression: crate::codec::Compression::Rle,
            merged_compression: crate::codec::Compression::Rle,
            synthesize_resolution: true,
            max_flatten_bytes: 2 << 30, // 2 GiB
        }
    }
}

/// A shrinking pool of decoded pixel bytes shared by one whole read.
#[derive(Debug, Clone)]
pub struct Budget {
    remaining: u64,
    max: u64,
}

impl Budget {
    pub fn new(max: u64) -> Self {
        Budget {
            remaining: max,
            max,
        }
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Draw `n` bytes, or refuse. Call this *before* allocating.
    pub fn take(&mut self, n: u64) -> PsdResult<()> {
        match self.remaining.checked_sub(n) {
            Some(left) => {
                self.remaining = left;
                Ok(())
            }
            None => Err(PsdError::BudgetExhausted { max: self.max }),
        }
    }

    /// Return `n` bytes that have been freed again.
    ///
    /// Only the flattener uses this, and only for a scratch canvas it has
    /// finished with. Returning a freed allocation turns the pool from "total
    /// bytes ever asked for" into "bytes held at once", which is the quantity
    /// that actually decides whether a machine runs out of memory: a hundred
    /// sibling groups composited one after another are fine, while a hundred
    /// *nested* ones are not, and only a peak measure tells them apart. It can
    /// never exceed the original ceiling.
    pub fn give(&mut self, n: u64) {
        self.remaining = self.remaining.saturating_add(n).min(self.max);
    }
}

/// Refuse a file-supplied count that is larger than a ceiling.
pub fn check_limit(what: &'static str, value: u64, max: u64) -> PsdResult<()> {
    if value > max {
        return Err(PsdError::LimitExceeded { what, value, max });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_refuses_the_draw_that_would_overrun_it() {
        let mut b = Budget::new(100);
        b.take(60).unwrap();
        assert_eq!(b.remaining(), 40);
        let err = b.take(41).unwrap_err();
        assert!(
            matches!(err, PsdError::BudgetExhausted { max: 100 }),
            "{err}"
        );
        // The failed draw did not consume anything.
        assert_eq!(b.remaining(), 40);
        b.take(40).unwrap();
    }

    #[test]
    fn giving_bytes_back_measures_a_peak_and_never_exceeds_the_ceiling() {
        let mut b = Budget::new(100);
        // Two draws that are fine one after the other, but not at once.
        b.take(80).unwrap();
        assert!(b.take(80).is_err());
        b.give(80);
        assert_eq!(b.remaining(), 100);
        b.take(80).unwrap();
        // Handing back more than was ever taken cannot inflate the pool.
        b.give(u64::MAX);
        assert_eq!(b.remaining(), 100);
    }

    #[test]
    fn check_limit_names_the_field_it_refused() {
        let err = check_limit("layer count", 9_000, 8_192).unwrap_err();
        match err {
            PsdError::LimitExceeded { what, value, max } => {
                assert_eq!((what, value, max), ("layer count", 9_000, 8_192));
            }
            other => panic!("wrong error: {other}"),
        }
    }
}
