//! Raster Studio desktop entry point.
//!
//! Phase-0 behavior: initialize diagnostics, load an image (from the first CLI
//! argument, or a generated placeholder), and hand it to the native shell,
//! which displays it on a pan/zoom GPU canvas.
//!
//! Usage:
//! ```text
//! studio-desktop [IMAGE_PATH]
//! ```

use std::path::PathBuf;

use anyhow::Result;

use app_shell::StartupImage;

fn main() -> Result<()> {
    telemetry::init_tracing();
    tracing::info!("Raster Studio {}", env!("CARGO_PKG_VERSION"));

    let image = match std::env::args().nth(1) {
        Some(path) => load_image(PathBuf::from(path))?,
        None => {
            tracing::info!("no image argument; showing generated placeholder");
            placeholder_image(1024, 640)
        }
    };

    tracing::info!("displaying {}x{} image", image.width, image.height);
    app_shell::launch(image)
}

/// Decode an image file to RGBA8 via the `raster` codec facade.
fn load_image(path: PathBuf) -> Result<StartupImage> {
    let decoded = raster::codec::decode_path(&path)?;
    Ok(StartupImage {
        width: decoded.width,
        height: decoded.height,
        rgba8: decoded.rgba8,
    })
}

/// A simple gradient + grid placeholder so the app is useful with no arguments
/// and CI can smoke-test the decode/upload path deterministically.
fn placeholder_image(width: u32, height: u32) -> StartupImage {
    let mut rgba8 = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 4) as usize;
            let r = (x * 255 / width.max(1)) as u8;
            let g = (y * 255 / height.max(1)) as u8;
            let grid = if x % 64 == 0 || y % 64 == 0 { 60 } else { 0 };
            rgba8[i] = r.saturating_add(grid);
            rgba8[i + 1] = g.saturating_add(grid);
            rgba8[i + 2] = 128u8.saturating_add(grid);
            rgba8[i + 3] = 255;
        }
    }
    StartupImage {
        width,
        height,
        rgba8,
    }
}
