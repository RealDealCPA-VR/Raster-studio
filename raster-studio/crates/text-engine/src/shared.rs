//! W9-K: fonts the user loaded this session, and a process-wide library for
//! the one text operation that needs outlines outside the compositor.
//!
//! File > Open of a `.ttf`/`.otf` hands the file's bytes to
//! [`register_session_font`]; [`with_shared_library`] - the library Layer >
//! Text > Convert to Shape reads glyph outlines from - picks up any it has
//! not seen on its next call. (The compositor's own library is fed the same
//! bytes by the caller, through its `load_font`.)

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use cosmic_text::fontdb::{Database, Source};

use crate::font::FontLibrary;

static SESSION_FONTS: Mutex<Vec<Arc<Vec<u8>>>> = Mutex::new(Vec::new());

/// Remember a font file's bytes for the rest of the session.
///
/// Returns how many faces the file holds; a file with none (not a font) is
/// not remembered and answers `0`.
pub fn register_session_font(bytes: Vec<u8>) -> usize {
    // W16-L: a WOFF / WOFF2 is remembered as the sfnt it carries.
    let bytes = Arc::new(crate::webfont::sfnt_from_webfont(bytes));
    let faces = Database::new()
        .load_font_source(Source::Binary(Arc::clone(&bytes) as _))
        .len();
    if faces > 0 {
        SESSION_FONTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(bytes);
    }
    faces
}

/// Every font file registered so far, oldest first.
fn session_fonts() -> Vec<Arc<Vec<u8>>> {
    SESSION_FONTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

struct Shared {
    library: FontLibrary,
    /// How many session fonts `library` already holds.
    loaded: usize,
}

/// Run `f` against the process-wide shared library: the system fonts (or
/// the `RASTER_STUDIO_FONT_DIRS` set), the user font folder, and every
/// session font registered so far.
pub fn with_shared_library<R>(f: impl FnOnce(&mut FontLibrary) -> R) -> R {
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    let lock = SHARED.get_or_init(|| {
        Mutex::new(Shared {
            library: FontLibrary::with_system_fonts(),
            loaded: 0,
        })
    });
    let mut shared = lock.lock().unwrap_or_else(PoisonError::into_inner);
    let fonts = session_fonts();
    for bytes in fonts.iter().skip(shared.loaded) {
        shared.library.load_bytes(bytes.as_ref().clone());
    }
    shared.loaded = fonts.len();
    f(&mut shared.library)
}
