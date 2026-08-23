//! Everything that can go wrong reading or writing a `.psd`.
//!
//! A `.psd` is **untrusted input**: it arrives by email, from a shared drive,
//! from a download. Every length, count and offset inside one is chosen by
//! whoever wrote the file. Each variant below that mentions a length, a count
//! or an offset exists because the answer to a value we do not like is a named
//! refusal rather than an allocation, a panic, or a loop.

/// Result alias for every fallible operation in this crate.
pub type PsdResult<T> = Result<T, PsdError>;

/// Renders a four-byte tag as something readable in an error message.
///
/// Non-printable bytes become `\xNN` so a hostile tag can neither forge a
/// convincing message nor emit control characters into a terminal.
pub fn tag_name(tag: [u8; 4]) -> String {
    let mut s = String::with_capacity(4);
    for b in tag {
        if b.is_ascii_graphic() || b == b' ' {
            s.push(b as char);
        } else {
            s.push_str(&format!("\\x{b:02x}"));
        }
    }
    s
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PsdError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The file ended in the middle of a field.
    ///
    /// `at` is an absolute offset into the whole file, even when the read was
    /// happening inside a bounded sub-section.
    #[error("truncated: {needed} bytes needed at offset {at}, but only {available} remain")]
    Truncated {
        needed: usize,
        available: usize,
        at: usize,
    },

    #[error("expected signature {expected:?} at offset {at}, found {found:?}")]
    BadSignature {
        expected: &'static str,
        found: String,
        at: usize,
    },

    /// Version 2 is PSB, the large-document variant, whose section lengths are
    /// 64-bit. This build reads and writes version 1 only.
    #[error("unsupported .psd version {0} (this build handles version 1; version 2 is PSB)")]
    UnsupportedVersion(u16),

    #[error("unsupported colour mode {name} ({code}); this build handles Greyscale and RGB")]
    UnsupportedColorMode { code: u16, name: &'static str },

    #[error("unsupported bit depth {0}; this build handles 8, 16 and 32")]
    UnsupportedDepth(u16),

    #[error("unsupported compression code {0}; 0=raw 1=RLE 2=ZIP 3=ZIP+prediction")]
    UnsupportedCompression(u16),

    #[error("unknown blend mode key {0:?}")]
    UnknownBlendMode(String),

    /// A dimension, count or declared length is outside what this reader will
    /// act on. This is the "absurd declared length" refusal: it fires *before*
    /// anything is allocated.
    #[error("{what} is {value}, more than the {max} this reader accepts")]
    LimitExceeded {
        what: &'static str,
        value: u64,
        max: u64,
    },

    /// The running budget for decoded pixel bytes ran out.
    #[error("decoding this file would need more than the {max} byte budget")]
    BudgetExhausted { max: u64 },

    /// Arithmetic on file-supplied numbers overflowed.
    #[error("arithmetic overflow computing {what} from values in the file")]
    Overflow { what: &'static str },

    #[error("header declares {declared} channels but the file's colour mode needs at least {min}")]
    ChannelCountTooSmall { declared: u16, min: u16 },

    /// The header declares a canvas with no area.
    ///
    /// Photoshop cannot produce one, and the writer refuses to emit one, so a
    /// file that contains one is damaged. It is refused on the way *in* rather
    /// than tolerated and then rejected on the way out: a document the reader
    /// hands back has to be one the writer accepts, and an
    /// [`PsdError::InvalidDocument`] from `write` would blame the caller for a
    /// defect in someone else's file.
    #[error("the canvas is {width}x{height}; a .psd must be at least 1x1")]
    EmptyCanvas { width: u32, height: u32 },

    /// The merged image's single RLE byte-count table does not hold one whole
    /// row-count per row of every channel.
    ///
    /// Kept apart from [`PsdError::ChannelSizeMismatch`] because the numbers are
    /// table entries, not bytes, and nothing has been decoded yet: reporting
    /// "expected 3 bytes of pixel data, decoded 2" for a three-channel table two
    /// entries long would name neither the quantity nor the stage correctly.
    #[error(
        "the merged image's row-count table holds {actual} entries, not the {expected} that \
         {channels} channels of {height} rows need"
    )]
    RowCountTableMismatch {
        expected: usize,
        actual: usize,
        channels: usize,
        height: usize,
    },

    /// A row-count table was handed to the splitter for a shape with no rows.
    ///
    /// Separate from [`PsdError::RowCountTableMismatch`] because there is no
    /// entry count that *would* have been right: with zero rows per channel the
    /// only honest report is that the request itself has no answer.
    #[error(
        "the merged image declares no rows, so its {actual}-entry row-count table cannot be \
         split into {channels} channels"
    )]
    RowCountTableWithoutRows { actual: usize, channels: usize },

    #[error("{what}: expected {expected} bytes of pixel data, decoded {actual}")]
    ChannelSizeMismatch {
        what: &'static str,
        expected: usize,
        actual: usize,
    },

    #[error("RLE row {row} decoded to {actual} bytes, expected {expected}")]
    BadRle {
        row: usize,
        expected: usize,
        actual: usize,
    },

    #[error("zlib stream is malformed: {0}")]
    BadZip(String),

    #[error("layer rectangle {top},{left},{bottom},{right} is inside out or out of range")]
    BadRect {
        top: i32,
        left: i32,
        bottom: i32,
        right: i32,
    },

    /// A section declared a length that does not agree with what is inside it.
    #[error("section {what} declares {declared} bytes but {consumed} were consumed")]
    SectionLengthMismatch {
        what: &'static str,
        declared: u64,
        consumed: u64,
    },

    #[error("descriptor nesting is deeper than the {max} levels this reader accepts")]
    DescriptorTooDeep { max: usize },

    /// Group nesting, expressed by `lsct` section dividers, went deeper than
    /// the reader will build a tree for.
    ///
    /// This exists for the same reason [`PsdError::DescriptorTooDeep`] does: a
    /// tree assembled from a file is walked, written, flattened and eventually
    /// dropped, and an unbounded depth turns any one of those into a stack
    /// overflow — an abort that cannot be caught and takes the host application
    /// down with it. Photoshop's own nesting limit is ten.
    #[error("group nesting is deeper than the {max} levels this reader accepts")]
    GroupTooDeep { max: usize },

    #[error("unknown descriptor value type {0:?}")]
    UnknownDescriptorType(String),

    /// A caller-side mistake rather than a file-side one: the data handed to
    /// the writer is not self-consistent.
    #[error("cannot write this document: {0}")]
    InvalidDocument(String),
}

impl PsdError {
    /// `true` when the error says "this file is damaged or hostile" rather than
    /// "this program was misused".
    pub fn is_file_fault(&self) -> bool {
        !matches!(self, PsdError::InvalidDocument(_) | PsdError::Io(_))
    }
}
