//! The OS image clipboard, behind an injectable seam (card 051).
//!
//! Copy/paste of *pixels* must be testable without owning a real clipboard,
//! so the service is a trait with two implementations: [`OsClipboard`] (the
//! shipped one, over `arboard`) and [`FakeClipboard`] (an in-memory slot the
//! tests inject — the same pattern `dialogs::ScriptedDialogs` established for
//! file dialogs). [`crate::Editor`] stores the service; the desktop binary
//! gets the OS implementation by construction, and a test swaps in the fake
//! through [`crate::Editor::set_clipboard`].
//!
//! # Why `arboard`
//!
//! The crate is already a direct dependency — the text-editing route
//! ([`crate::shell::Shell::route_text_clipboard_key`]) writes selection text
//! with it — and its default feature set includes `image-data`, so image
//! get/set needs no manifest change at all. Adding a second clipboard stack
//! for pixels would split the lifecycle for no capability; the choice follows
//! the [arboard clipboard API](https://docs.rs/arboard/3.6.1/arboard/struct.Clipboard.html),
//! which exposes image get/set over one `ImageData` shape and documents the
//! platform-specific error behavior this module maps below.
//!
//! # Normalization
//!
//! The application's one image shape is [`ClipboardImage`]: **straight**
//! (non-premultiplied) RGBA, row-major, four bytes per pixel — exactly what
//! `arboard::ImageData` promises, so no channel reordering happens in either
//! direction. Everything that crosses the seam is validated first: the byte
//! count must match the dimensions, and both dimensions are bounded by
//! [`MAX_CLIPBOARD_DIMENSION`], so a hostile or corrupt clipboard payload
//! cannot turn into a multi-gigabyte allocation downstream.
//!
//! # Errors and lifecycle
//!
//! Every failure is an ordinary [`ClipboardError`] the caller can report —
//! a clipboard held by another process is [`ClipboardError::Busy`], a
//! platform without image support is [`ClipboardError::Unavailable`], a
//! malformed payload is [`ClipboardError::InvalidImage`], and a clipboard
//! that simply holds no image is `Ok(None)`, which is a *normal* answer (the
//! paste router falls back to internal content). [`OsClipboard`] opens the
//! native clipboard only for the duration of one transfer and drops it
//! immediately afterwards — `arboard` documents that the underlying
//! implementation opens/closes the OS clipboard per operation — so there is
//! no long-lived handle to release at shutdown: the service's `Drop` (with
//! the [`crate::Editor`]) is the whole lifecycle.

/// Straight RGBA pixels: `rgba` holds `width * height * 4` bytes, row-major,
/// non-premultiplied — the shape every image enters and leaves the
/// application through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The largest side a clipboard image may have. Nothing real fits past this,
/// and bounding the dimensions keeps a corrupt payload from allocating
/// absurdly downstream.
pub const MAX_CLIPBOARD_DIMENSION: u32 = 16384;

/// Why a clipboard operation failed. Every kind is recoverable: the caller
/// reports it and carries on — none of these is a bug or a panic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    /// The native clipboard is held by another party right now (another
    /// process, or another thread of this one). Retrying later can succeed.
    #[error("the clipboard is busy; try again")]
    Busy,
    /// The platform (or this build of it) cannot serve image content at all.
    #[error("the clipboard cannot hold images on this platform")]
    Unavailable,
    /// The payload is unusable: dimensions and byte count disagree, a
    /// dimension is zero, or a dimension exceeds [`MAX_CLIPBOARD_DIMENSION`].
    #[error("{0}")]
    InvalidImage(String),
}

impl ClipboardImage {
    /// Validates dimensions and byte count, returning the image or naming
    /// what is wrong with it. The one gate every clipboard image passes.
    pub fn validate(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, ClipboardError> {
        check_dims(width, height, rgba.len())?;
        Ok(ClipboardImage {
            width,
            height,
            rgba,
        })
    }
}

/// The dimension/byte-count gate, borrowable: [`ClipboardImage::validate`]
/// owns its payload (the OS path hands over the bytes), but a caller that
/// only wants the check — [`OsClipboard::set_image`], which must not clone
/// the buffer just to throw the clone away — uses this.
fn check_dims(width: u32, height: u32, byte_count: usize) -> Result<(), ClipboardError> {
    if width == 0 || height == 0 {
        return Err(ClipboardError::InvalidImage(format!(
            "image dimensions must be positive, got {width}x{height}"
        )));
    }
    if width > MAX_CLIPBOARD_DIMENSION || height > MAX_CLIPBOARD_DIMENSION {
        return Err(ClipboardError::InvalidImage(format!(
            "image dimensions {width}x{height} exceed the {MAX_CLIPBOARD_DIMENSION} limit"
        )));
    }
    let expected = width as usize * height as usize * 4;
    if byte_count != expected {
        return Err(ClipboardError::InvalidImage(format!(
            "image byte count {byte_count} does not match {width}x{height} (expected {expected})"
        )));
    }
    Ok(())
}

/// The clipboard service. Implemented by [`OsClipboard`] (shipped) and
/// [`FakeClipboard`] (tests).
pub trait ImageClipboard {
    /// The clipboard's image, if it currently holds one. `Ok(None)` means the
    /// clipboard holds no image (empty, or text only) — not an error.
    fn get_image(&mut self) -> Result<Option<ClipboardImage>, ClipboardError>;
    /// Puts an image on the clipboard, replacing whatever was there.
    fn set_image(&mut self, image: &ClipboardImage) -> Result<(), ClipboardError>;
    /// Whether an image paste would find anything — the menu-enablement
    /// probe (card 052). The OS implementation answers by reading the image
    /// (arboard exposes no cheaper availability check); callers use it once
    /// per menu resolution, not per frame.
    fn has_image(&mut self) -> bool {
        matches!(self.get_image(), Ok(Some(_)))
    }
}

/// The shipped implementation over `arboard`. Holds no OS state between
/// operations: each call opens the native clipboard for the transfer and
/// drops it, so shutdown needs no explicit release (see the module docs).
///
/// Every call serializes behind a process-wide lock: the OS clipboard is a
/// global resource, arboard maps a same-process collision to
/// [`ClipboardError::Busy`] (the native open fails while another thread of
/// this process holds it), and concurrent access from parallel test threads
/// has proven able to corrupt the heap outright. Serializing removes that
/// failure class for everything behind the lock — which must therefore be
/// EVERY clipboard access in the process, the text route included (see
/// [`with_os_clipboard_lock`]).
pub struct OsClipboard;

static OS_CLIPBOARD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs `f` while holding the process-wide OS clipboard lock. Callers that
/// touch `arboard` directly — today only the text route in
/// [`crate::shell::Shell`] — wrap their use in this, so image and text
/// clipboard traffic serializes behind the same lock instead of racing on
/// the native clipboard.
pub(crate) fn with_os_clipboard_lock<T>(f: impl FnOnce() -> T) -> T {
    let _guard = OS_CLIPBOARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

impl ImageClipboard for OsClipboard {
    fn get_image(&mut self) -> Result<Option<ClipboardImage>, ClipboardError> {
        let _guard = OS_CLIPBOARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut clipboard = arboard::Clipboard::new().map_err(map_arboard)?;
        match clipboard.get_image() {
            Ok(data) => {
                // `arboard::ImageData` guarantees `width * height * 4` bytes of
                // straight RGBA; validation is still applied, because a
                // malformed payload would otherwise be trusted implicitly.
                let image = ClipboardImage::validate(
                    u32::try_from(data.width).unwrap_or(u32::MAX),
                    u32::try_from(data.height).unwrap_or(u32::MAX),
                    data.bytes.into_owned(),
                )?;
                Ok(Some(image))
            }
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(map_arboard(e)),
        }
    }

    fn set_image(&mut self, image: &ClipboardImage) -> Result<(), ClipboardError> {
        // Re-validate on the way out — without cloning the pixels twice: the
        // dimension/byte-count gate borrows, and only the transfer itself
        // copies.
        check_dims(image.width, image.height, image.rgba.len())?;
        let _guard = OS_CLIPBOARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut clipboard = arboard::Clipboard::new().map_err(map_arboard)?;
        clipboard
            .set_image(arboard::ImageData {
                width: image.width as usize,
                height: image.height as usize,
                bytes: image.rgba.clone().into(),
            })
            .map_err(map_arboard)
    }
}

fn map_arboard(error: arboard::Error) -> ClipboardError {
    // `arboard::Error` is `#[non_exhaustive]`: future kinds fold into the
    // catch-all instead of breaking this build.
    match error {
        arboard::Error::ClipboardOccupied => ClipboardError::Busy,
        // "Not supported" (Wayland primary, headless environments) and a
        // conversion failure both mean: this platform cannot serve images
        // through the clipboard right now.
        arboard::Error::ClipboardNotSupported
        | arboard::Error::ConversionFailure
        | arboard::Error::Unknown { .. }
        | _ => ClipboardError::Unavailable,
    }
}

/// The deterministic fake: an in-memory slot with optional injected failure.
/// It applies the same validation as the real implementation, so a test that
/// stores and fetches exercises the same gate the OS path does.
#[derive(Default)]
pub struct FakeClipboard {
    slot: Option<ClipboardImage>,
    failure: Option<ClipboardError>,
}

impl FakeClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every subsequent operation fails with this error — the stand-in for a
    /// busy or image-less platform.
    pub fn with_failure(mut self, failure: ClipboardError) -> Self {
        self.failure = Some(failure);
        self
    }

    /// Seeds the slot directly, bypassing the clipboard round trip (a test
    /// wants to control what a paste *finds*).
    pub fn seed(&mut self, image: ClipboardImage) {
        self.slot = Some(image);
    }
}

impl ImageClipboard for FakeClipboard {
    fn get_image(&mut self) -> Result<Option<ClipboardImage>, ClipboardError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        Ok(self.slot.clone())
    }

    fn set_image(&mut self, image: &ClipboardImage) -> Result<(), ClipboardError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        self.slot = Some(ClipboardImage::validate(
            image.width,
            image.height,
            image.rgba.clone(),
        )?);
        Ok(())
    }

    fn has_image(&mut self) -> bool {
        // A failing clipboard cannot offer an image to paste.
        !self.failure.is_some() && self.slot.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, rgba: Vec<u8>) -> ClipboardImage {
        ClipboardImage::validate(width, height, rgba).unwrap()
    }

    #[test]
    fn the_fake_round_trips_an_opaque_image() {
        // Fully opaque RGB-ish pixels: alpha 255 everywhere.
        let original = image(
            3,
            2,
            vec![
                10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255, 130, 140,
                150, 255, 160, 170, 180, 255,
            ],
        );
        let mut clipboard = FakeClipboard::new();
        clipboard.set_image(&original).unwrap();
        let fetched = clipboard.get_image().unwrap().expect("an image comes back");
        assert_eq!(
            fetched, original,
            "the opaque image survives the round trip"
        );
    }

    #[test]
    fn the_fake_round_trips_transparent_pixels() {
        // Alpha is part of the payload, not discarded: a fully transparent
        // pixel must come back fully transparent.
        let original = image(2, 1, vec![255, 0, 0, 0, 0, 255, 0, 128]);
        let mut clipboard = FakeClipboard::new();
        clipboard.set_image(&original).unwrap();
        let fetched = clipboard.get_image().unwrap().expect("an image comes back");
        assert_eq!(fetched.rgba, original.rgba, "alpha survives the round trip");
        assert_eq!(
            fetched.rgba[3], 0,
            "the first pixel stays fully transparent"
        );
        assert_eq!(
            fetched.rgba[7], 128,
            "the second pixel keeps its half alpha"
        );
    }

    #[test]
    fn an_empty_clipboard_is_none_not_an_error() {
        let mut clipboard = FakeClipboard::new();
        assert_eq!(clipboard.get_image().unwrap(), None);
    }

    #[test]
    fn malformed_images_are_refused_not_panicked_on() {
        // Byte count and dimensions disagree.
        let err = ClipboardImage::validate(2, 2, vec![0; 3 * 4]).unwrap_err();
        assert!(matches!(err, ClipboardError::InvalidImage(_)), "{err:?}");
        // A zero dimension.
        let err = ClipboardImage::validate(0, 4, vec![0; 16]).unwrap_err();
        assert!(matches!(err, ClipboardError::InvalidImage(_)), "{err:?}");
        // Past the bound.
        let err = ClipboardImage::validate(MAX_CLIPBOARD_DIMENSION + 1, 1, vec![0; 4]).unwrap_err();
        assert!(matches!(err, ClipboardError::InvalidImage(_)), "{err:?}");
        // The gate is exactly at the bound (and the byte count must match).
        let at_bound = ClipboardImage::validate(
            MAX_CLIPBOARD_DIMENSION,
            1,
            vec![0; MAX_CLIPBOARD_DIMENSION as usize * 4],
        );
        assert!(at_bound.is_ok(), "the bound itself is accepted");
    }

    #[test]
    fn set_image_validates_too() {
        let mut clipboard = FakeClipboard::new();
        let bad = ClipboardImage {
            width: 2,
            height: 2,
            rgba: vec![0; 4], // 4 bytes for a 2x2 image: wrong
        };
        let err = clipboard.set_image(&bad).unwrap_err();
        assert!(matches!(err, ClipboardError::InvalidImage(_)), "{err:?}");
        // The refused image left no half-state behind.
        assert_eq!(clipboard.get_image().unwrap(), None);
    }

    #[test]
    fn clipboard_failures_are_ordinary_errors() {
        // A busy clipboard reports Busy — and the caller can retry.
        let mut clipboard = FakeClipboard::new().with_failure(ClipboardError::Busy);
        let err = clipboard.get_image().unwrap_err();
        assert_eq!(err, ClipboardError::Busy);
        let err = clipboard.set_image(&image(1, 1, vec![0; 4])).unwrap_err();
        assert_eq!(err, ClipboardError::Busy);
        // An image-less platform reports Unavailable.
        let mut clipboard = FakeClipboard::new().with_failure(ClipboardError::Unavailable);
        assert_eq!(
            clipboard.get_image().unwrap_err(),
            ClipboardError::Unavailable
        );
    }

    /// The REAL OS round trip, runnable on a machine with a working clipboard
    /// (`cargo test -p app-shell --lib os_clipboard -- --ignored --nocapture`).
    /// Ignored by default so CI machines without an interactive session stay
    /// green; on a desktop host it proves the adapter talks to the actual OS
    /// clipboard. NOTE: this REPLACES whatever image the clipboard held.
    #[test]
    #[ignore = "host-bound: drives the real OS clipboard and replaces its content"]
    fn os_clipboard_image_round_trip() {
        let original = image(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 128, 128, 128, 255,
            ],
        );
        let mut clipboard = OsClipboard;
        clipboard
            .set_image(&original)
            .expect("the OS clipboard accepts an image");
        let fetched = clipboard
            .get_image()
            .expect("the OS clipboard is readable")
            .expect("the image we just set is still there");
        assert_eq!(fetched.width, original.width);
        assert_eq!(fetched.height, original.height);
        assert_eq!(
            fetched.rgba, original.rgba,
            "the OS round trip is byte-exact"
        );
    }

    /// Reads whatever image is CURRENTLY on the OS clipboard — the repeatable
    /// form of the card's manual screenshot check: take a screenshot (or seed
    /// the clipboard from any other app), then run
    /// `cargo test -p app-shell --lib os_clipboard_read_current -- --ignored --nocapture`.
    /// Asserts only that a present image is well-formed (dims > 0, byte count
    /// matches) — the CONTENT belongs to whatever put it there. Ignored by
    /// default: on a headless machine there is no meaningful answer.
    #[test]
    #[ignore = "host-bound: reads whatever image the OS clipboard currently holds"]
    fn os_clipboard_read_current() {
        let mut clipboard = OsClipboard;
        match clipboard.get_image().expect("the OS clipboard is readable") {
            None => println!("the clipboard currently holds no image"),
            Some(image) => {
                println!(
                    "read {}x{} from the OS clipboard",
                    image.width, image.height
                );
                assert!(image.width > 0 && image.height > 0);
                assert_eq!(
                    image.rgba.len(),
                    image.width as usize * image.height as usize * 4
                );
            }
        }
    }
}
