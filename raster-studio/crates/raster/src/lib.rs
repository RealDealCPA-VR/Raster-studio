//! Tiles, mipmaps, codecs, and pixel formats — the substrate every raster
//! layer and composite is built from.
//!
//! # Tile-first strategy
//! We never model an image as a permanently resident full-canvas texture.
//! Pixels live in fixed-size [`Tile`]s (default [`TILE_SIZE`]), addressed by
//! [`TileCoord`], and content-addressed by [`TileHash`] so identical tiles are
//! stored once. A [`TileGrid`] binds those tiles to one mip level of one
//! image, tracks which part of each edge tile is real image data, and answers
//! "which tiles does this viewport touch?". Higher-level crates (`render`,
//! `asset-store`) build GPU/CPU caches on top of these primitives.
//!
//! # Correctness rules this crate enforces
//! * A tile's [`TileHash`] can never go stale: pixel bytes are only reachable
//!   through accessors that drop the cached hash.
//! * Images whose dimensions are not a multiple of [`TILE_SIZE`] round-trip
//!   exactly; padding in edge tiles is never mistaken for image content.
//! * Mip levels are filtered in linear, premultiplied space, so they neither
//!   darken nor bleed color out of transparent pixels.

pub mod codec;
pub mod export;
pub mod format;
pub mod grid;
pub mod mipmap;
pub mod pdf;
pub mod tile;

pub use codec::{
    decode_bytes, decode_path, decode_surface_bytes, decode_surface_bytes_as, decode_surface_path,
    decode_surface_reader, encode, encode_into, encode_to_path, encode_with, probe_bytes,
    probe_bytes_as, probe_path, probe_reader, AlphaSupport, CodecError, DecodedImage,
    DecodedSurface, EncodeOptions, EncodedPixels, ExportFormat, ImageInfo, ImportFormat,
    ImportLimits, SurfacePixels,
};
pub use export::{
    export, export_batch, export_batch_to_dir, flatten_onto, linear_from_rgba16,
    linear_from_rgba16_pass_through, linear_from_rgba8, linear_from_rgba8_pass_through, resample,
    rgba16_from_linear, rgba16_from_linear_pass_through, rgba8_from_linear,
    rgba8_from_linear_pass_through, sanitize_file_stem, BitDepth, ColorHandling, ExportError,
    ExportMetadata, ExportPreset, ExportedFile, FlattenMode, LinearImage, ResampleFilter,
};
pub use format::PixelFormat;
pub use grid::{GridError, PixelRect, TileGrid};
pub use mipmap::{MipChain, MipError, MipLevel};
pub use tile::{Tile, TileCoord, TileError, TileHash, TILE_SIZE};

/// A test-only global allocator that measures allocation on the calling thread.
///
/// It exists for assertions that are otherwise unprovable prose: that encoding
/// does not clone the source pixel buffer, and that a batch export's *peak*
/// footprint does not grow with the number of presets. Counters are
/// thread-local, so the test harness running other tests in parallel does not
/// perturb a measurement.
///
/// Two different questions, two different counters:
/// * [`measure`] returns the **total** bytes handed out, which answers "was
///   this buffer copied at all?".
/// * [`measure_peak`] returns the high-water mark of bytes **live at once**,
///   which answers "how much does this hold simultaneously?". Total allocation
///   cannot answer that: a loop that allocates and frees N buffers moves the
///   same total whether it retains one of them or all N.
#[cfg(test)]
pub(crate) mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        /// Bytes handed out since the last [`measure`] reset.
        static ALLOCATED: Cell<u64> = const { Cell::new(0) };
        /// Bytes currently live, relative to the last [`measure_peak`] reset.
        /// Signed: memory allocated before the reset and freed inside it
        /// legitimately drives this below zero.
        static LIVE: Cell<i64> = const { Cell::new(0) };
        /// The largest value [`LIVE`] has reached since that reset.
        static PEAK: Cell<i64> = const { Cell::new(0) };
    }

    fn record(total_delta: usize, live_delta: i64) {
        // `try_with`, because an allocation can happen while thread-local
        // storage is being torn down, and a panic inside the allocator aborts.
        if total_delta != 0 {
            let _ = ALLOCATED.try_with(|c| c.set(c.get().saturating_add(total_delta as u64)));
        }
        let _ = LIVE.try_with(|live| {
            let now = live.get().saturating_add(live_delta);
            live.set(now);
            let _ = PEAK.try_with(|peak| {
                if now > peak.get() {
                    peak.set(now);
                }
            });
        });
    }

    struct Counting;

    // SAFETY: every method forwards to `System` with the same arguments; the
    // only added work is updating thread-local integers, which allocates
    // nothing and cannot unwind.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record(layout.size(), layout.size() as i64);
            System.alloc(layout)
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record(layout.size(), layout.size() as i64);
            System.alloc_zeroed(layout)
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            record(0, -(layout.size() as i64));
            System.dealloc(ptr, layout)
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            record(
                new_size.saturating_sub(layout.size()),
                new_size as i64 - layout.size() as i64,
            );
            System.realloc(ptr, layout, new_size)
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;

    /// Run `f`, returning its value and the bytes allocated on this thread
    /// while it ran.
    pub(crate) fn measure<T>(f: impl FnOnce() -> T) -> (T, u64) {
        ALLOCATED.with(|c| c.set(0));
        let value = f();
        let used = ALLOCATED.with(Cell::get);
        (value, used)
    }

    /// Run `f`, returning its value and the most bytes that were live at once
    /// on this thread while it ran.
    pub(crate) fn measure_peak<T>(f: impl FnOnce() -> T) -> (T, u64) {
        LIVE.with(|c| c.set(0));
        PEAK.with(|c| c.set(0));
        let value = f();
        let peak = PEAK.with(Cell::get).max(0) as u64;
        (value, peak)
    }

    #[test]
    fn the_probe_actually_counts() {
        let (v, used) = measure(|| vec![0u8; 4 << 20]);
        assert_eq!(v.len(), 4 << 20);
        assert!(used >= (4 << 20), "counted only {used} bytes");
        let (_, idle) = measure(|| std::hint::black_box(1u32 + 1));
        assert!(
            idle < 1024,
            "an arithmetic expression allocated {idle} bytes"
        );
    }

    /// The peak counter measures what is live at once, not what was handed out
    /// in total — the distinction the batch-export bound depends on.
    #[test]
    fn the_peak_probe_measures_live_bytes_not_total_bytes() {
        const CHUNK: usize = 4 << 20;

        // Four buffers, one at a time: total is 16 MiB, peak is 4 MiB.
        let (_, sequential_peak) = measure_peak(|| {
            for _ in 0..4 {
                let v = std::hint::black_box(vec![0u8; CHUNK]);
                drop(v);
            }
        });
        // ...and the same four retained at once: peak is 16 MiB.
        let (held, retained_peak) = measure_peak(|| {
            let mut held = Vec::new();
            for _ in 0..4 {
                held.push(std::hint::black_box(vec![0u8; CHUNK]));
            }
            held
        });
        assert_eq!(held.len(), 4);

        assert!(
            sequential_peak >= CHUNK as u64 && sequential_peak < (CHUNK * 2) as u64,
            "one-at-a-time peaked at {sequential_peak} bytes"
        );
        assert!(
            retained_peak >= (CHUNK * 4) as u64,
            "four retained buffers peaked at only {retained_peak} bytes"
        );

        // The total counter cannot tell those two apart, which is why the peak
        // counter exists.
        let (_, sequential_total) = measure(|| {
            for _ in 0..4 {
                drop(std::hint::black_box(vec![0u8; CHUNK]));
            }
        });
        assert!(sequential_total >= (CHUNK * 4) as u64);
    }
}
