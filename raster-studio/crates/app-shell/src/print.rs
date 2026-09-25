//! W13-M: File ▸ Print, through the operating system's own print dialog.
//!
//! On Windows, Print opens the system Print dialog (comdlg32 `PrintDlgW`) —
//! printer, copies, preferences, and "Microsoft Print to PDF" among the
//! printers — and sends the flattened image to the chosen printer through
//! GDI: one page, the image centred on it at the document's size at
//! [`PRINT_PPI`] and scaled down only when that would not fit the printable
//! area ([`place_on_page`]). Alpha is composited onto white paper
//! ([`bgr_rows`]), as the PDF path does.
//!
//! Everywhere else — and on Windows when no printer can be opened at all —
//! Print keeps the S1.8 route: a print-ready single-page PDF written where the
//! user chooses, which the platform's own viewer prints. [`route_for`] is that
//! dispatch, decided by the OS name so it is testable on any host.
//!
//! No dependency was added: the Win32 calls come from the `windows-sys` crate
//! the shell already links (MIT OR Apache-2.0), with three more of its
//! feature modules enabled.

/// The resolution a document is printed at. Documents carry no print
/// resolution yet, so a pixel is a PDF point — the same 72 ppi the PDF route
/// has always used for its media box.
pub const PRINT_PPI: f32 = 72.0;

/// How File ▸ Print reaches paper on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintRoute {
    /// The system Print dialog, then the chosen printer (Windows).
    SystemDialog,
    /// A print-ready PDF written where the user picks.
    PdfFile,
}

/// The route for an operating system named as [`std::env::consts::OS`] names
/// it. Only Windows has a system dialog wired; macOS and Linux keep the PDF
/// route (their print dialogs belong to toolkits this build does not link).
pub fn route_for(os: &str) -> PrintRoute {
    if os == "windows" {
        PrintRoute::SystemDialog
    } else {
        PrintRoute::PdfFile
    }
}

/// The route this build takes. The crate's own unit tests, and any run with
/// `RASTER_STUDIO_PRINT_TO_PDF` set (CI, headless scripting), take the PDF
/// route so no modal system dialog ever blocks an unattended process.
pub fn current_route() -> PrintRoute {
    if cfg!(test) || std::env::var_os("RASTER_STUDIO_PRINT_TO_PDF").is_some() {
        return PrintRoute::PdfFile;
    }
    route_for(std::env::consts::OS)
}

/// Where the image lands on the page, in the printer's device pixels,
/// relative to the printable area's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Place an `image_w` × `image_h` pixel image on a printable area of
/// `printable_w` × `printable_h` device pixels at `dpi_x` × `dpi_y`.
///
/// The image prints at its size at `ppi` pixels per inch, scaled down
/// uniformly (never up) when that is larger than the printable area, and is
/// centred. `None` when any size or resolution is not positive.
pub fn place_on_page(
    image_w: u32,
    image_h: u32,
    ppi: f32,
    printable_w: i32,
    printable_h: i32,
    dpi_x: i32,
    dpi_y: i32,
) -> Option<Placement> {
    if image_w == 0
        || image_h == 0
        || !(ppi.is_finite() && ppi > 0.0)
        || printable_w <= 0
        || printable_h <= 0
        || dpi_x <= 0
        || dpi_y <= 0
    {
        return None;
    }
    let natural_w = image_w as f64 / f64::from(ppi) * f64::from(dpi_x);
    let natural_h = image_h as f64 / f64::from(ppi) * f64::from(dpi_y);
    let scale = (f64::from(printable_w) / natural_w)
        .min(f64::from(printable_h) / natural_h)
        .min(1.0);
    let width = ((natural_w * scale).round() as i32).clamp(1, printable_w);
    let height = ((natural_h * scale).round() as i32).clamp(1, printable_h);
    Some(Placement {
        x: (printable_w - width) / 2,
        y: (printable_h - height) / 2,
        width,
        height,
    })
}

/// The bytes of a top-down 24-bit DIB for `rgba`: blue, green, red per pixel,
/// alpha composited onto white paper, each row padded to four bytes. `None`
/// when `rgba` is not `width * height * 4` bytes.
pub fn bgr_rows(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if rgba.len() != w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    let stride = (w * 3).div_ceil(4) * 4;
    let mut out = vec![0u8; stride * h];
    for y in 0..h {
        for x in 0..w {
            let px = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
            let a = u32::from(px[3]);
            let over_white = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
            let o = y * stride + x * 3;
            out[o] = over_white(px[2]);
            out[o + 1] = over_white(px[1]);
            out[o + 2] = over_white(px[0]);
        }
    }
    Some(out)
}

/// What the system Print dialog did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemPrint {
    /// The page went to the printer the user chose.
    Printed,
    /// The user closed the dialog without printing.
    Cancelled,
    /// No print dialog or printer could be opened (no printer installed, a
    /// driver refused the job): the caller falls back to the PDF route.
    Unavailable(String),
}

/// Open the system Print dialog and print `rgba` (the flattened image) as one
/// page named `document`.
#[cfg(windows)]
pub fn print_with_system_dialog(
    document: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> SystemPrint {
    win::print(document, width, height, rgba)
}

/// No system print dialog is wired on this platform.
#[cfg(not(windows))]
pub fn print_with_system_dialog(
    _document: &str,
    _width: u32,
    _height: u32,
    _rgba: &[u8],
) -> SystemPrint {
    SystemPrint::Unavailable("this platform prints through the PDF route".into())
}

/// The app window the system Print dialog belongs to, as a Win32 `HWND`
/// (0 while there is none). Set by the shell when it builds its window, so
/// the dialog opens over the app, modal to it (the window is disabled while
/// the dialog runs), rather than as an unowned top-level window that can open
/// behind it.
static OWNER_WINDOW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Record `window` as the owner of the system Print dialog (Windows; a no-op
/// elsewhere, where there is no dialog).
pub fn set_owner_window(window: &winit::window::Window) {
    #[cfg(windows)]
    {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = window.window_handle() {
            if let RawWindowHandle::Win32(h) = handle.as_raw() {
                set_owner_hwnd(h.hwnd.get());
            }
        }
    }
    #[cfg(not(windows))]
    let _ = window;
}

/// Record the owner window by its raw handle (0 clears it).
pub fn set_owner_hwnd(hwnd: isize) {
    OWNER_WINDOW.store(hwnd, std::sync::atomic::Ordering::Relaxed);
}

/// The owner window the Print dialog is given, 0 when none is set.
pub fn owner_hwnd() -> isize {
    OWNER_WINDOW.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(windows)]
mod win {
    use super::{bgr_rows, owner_hwnd, place_on_page, SystemPrint, PRINT_PPI};
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Graphics::Gdi::{
        DeleteDC, GetDeviceCaps, SetBrushOrgEx, SetStretchBltMode, StretchDIBits, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HALFTONE, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY,
        RGBQUAD, SRCCOPY, VERTRES,
    };
    use windows_sys::Win32::Storage::Xps::{
        AbortDoc, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW,
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, PrintDlgW, PD_NOPAGENUMS, PD_NOSELECTION, PD_RETURNDC,
        PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGW,
    };

    /// Deletes a printer DC however the job ends.
    pub(super) struct Dc(pub(super) HDC);
    impl Drop for Dc {
        fn drop(&mut self) {
            // SAFETY: the DC came from PrintDlgW with PD_RETURNDC (or
            // CreateDCW in the tests) and is deleted exactly once, here.
            unsafe { DeleteDC(self.0) };
        }
    }

    /// The request handed to `PrintDlgW`: a DC back, no page ranges or
    /// selection, one copy, owned by `owner` (the app window's HWND).
    pub(super) fn dialog_request(owner: isize) -> PRINTDLGW {
        // SAFETY: PRINTDLGW is a plain C struct for which all-zero is the
        // documented "no defaults" state; the size and flags are set below.
        let mut pd: PRINTDLGW = unsafe { std::mem::zeroed() };
        pd.lStructSize = std::mem::size_of::<PRINTDLGW>() as u32;
        pd.hwndOwner = owner as _;
        pd.Flags = PD_RETURNDC | PD_NOPAGENUMS | PD_NOSELECTION | PD_USEDEVMODECOPIESANDCOLLATE;
        pd.nCopies = 1;
        pd
    }

    pub(super) fn print(document: &str, width: u32, height: u32, rgba: &[u8]) -> SystemPrint {
        let mut pd = dialog_request(owner_hwnd());
        // SAFETY: `pd` is initialised as PrintDlgW requires and outlives it.
        // With `hwndOwner` set the dialog is modal to the app window.
        let ok = unsafe { PrintDlgW(&mut pd) };
        // SAFETY: the dialog allocated these (or left them null); they are
        // freed once and never read afterwards.
        unsafe {
            if !pd.hDevMode.is_null() {
                GlobalFree(pd.hDevMode);
            }
            if !pd.hDevNames.is_null() {
                GlobalFree(pd.hDevNames);
            }
        }
        if ok == 0 {
            // SAFETY: no arguments; reads the dialog's last error.
            let code = unsafe { CommDlgExtendedError() };
            return if code == 0 {
                SystemPrint::Cancelled
            } else {
                SystemPrint::Unavailable(format!("the print dialog failed (code {code:#x})"))
            };
        }
        if pd.hDC.is_null() {
            return SystemPrint::Unavailable("the printer returned no device context".into());
        }
        print_on_dc(&Dc(pd.hDC), document, None, width, height, rgba)
    }

    /// Print `rgba` as one page on the printer DC `dc`, the image placed by
    /// [`place_on_page`]. `output` (a NUL-terminated wide path) sends the job
    /// to a file instead of the device's port, which is how the tests print
    /// through "Microsoft Print to PDF" without a dialog.
    pub(super) fn print_on_dc(
        dc: &Dc,
        document: &str,
        output: Option<&[u16]>,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> SystemPrint {
        let Some(bits) = bgr_rows(width, height, rgba) else {
            return SystemPrint::Unavailable("the image buffer does not match its size".into());
        };
        let (Ok(iw), Ok(ih)) = (i32::try_from(width), i32::try_from(height)) else {
            return SystemPrint::Unavailable("the image is too large to print".into());
        };
        // SAFETY: `dc.0` is a live printer DC for the rest of this function.
        let caps = |index: u32| unsafe { GetDeviceCaps(dc.0, index as i32) };
        let Some(place) = place_on_page(
            width,
            height,
            PRINT_PPI,
            caps(HORZRES),
            caps(VERTRES),
            caps(LOGPIXELSX),
            caps(LOGPIXELSY),
        ) else {
            return SystemPrint::Unavailable("the printer reported no printable area".into());
        };
        let name: Vec<u16> = document.encode_utf16().chain(std::iter::once(0)).collect();
        let info = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: name.as_ptr(),
            lpszOutput: output.map_or(std::ptr::null(), |o| o.as_ptr()),
            lpszDatatype: std::ptr::null(),
            fwType: 0,
        };
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: iw,
                // Negative: the rows run top-down, as `bgr_rows` writes them.
                biHeight: -ih,
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        // SAFETY: every pointer handed to GDI below (`info`, `name`, `output`,
        // `bits`, `bmi`) outlives the call it is passed to, and `dc.0` is live.
        unsafe {
            if StartDocW(dc.0, &info) <= 0 {
                return SystemPrint::Unavailable("the printer refused the job".into());
            }
            if StartPage(dc.0) <= 0 {
                AbortDoc(dc.0);
                return SystemPrint::Unavailable("the printer refused the page".into());
            }
            SetStretchBltMode(dc.0, HALFTONE);
            SetBrushOrgEx(dc.0, 0, 0, std::ptr::null_mut());
            let lines = StretchDIBits(
                dc.0,
                place.x,
                place.y,
                place.width,
                place.height,
                0,
                0,
                iw,
                ih,
                bits.as_ptr().cast(),
                &bmi,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            if lines <= 0 {
                AbortDoc(dc.0);
                return SystemPrint::Unavailable("the printer driver refused the image".into());
            }
            if EndPage(dc.0) <= 0 {
                AbortDoc(dc.0);
                return SystemPrint::Unavailable("the printer refused the page".into());
            }
            if EndDoc(dc.0) <= 0 {
                return SystemPrint::Unavailable("the printer did not finish the job".into());
            }
        }
        SystemPrint::Printed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_opens_the_system_dialog_and_every_other_platform_keeps_the_pdf() {
        assert_eq!(route_for("windows"), PrintRoute::SystemDialog);
        for os in ["macos", "linux", "freebsd", ""] {
            assert_eq!(route_for(os), PrintRoute::PdfFile, "{os}");
        }
        // The crate's own tests never open a modal dialog.
        assert_eq!(current_route(), PrintRoute::PdfFile);
    }

    #[test]
    fn an_image_that_fits_prints_at_its_size_centred() {
        // 720 x 360 px at 72 ppi is 10 x 5 in; on a 600 dpi device with a
        // 8 x 10.5 in printable area it is too wide, so it scales to 8 in.
        let p = place_on_page(720, 360, 72.0, 4800, 6300, 600, 600).unwrap();
        assert_eq!(p.width, 4800);
        assert_eq!(p.height, 2400);
        assert_eq!((p.x, p.y), (0, (6300 - 2400) / 2));

        // 144 x 72 px is 2 x 1 in: printed at exactly that size, centred.
        let p = place_on_page(144, 72, 72.0, 4800, 6300, 600, 600).unwrap();
        assert_eq!((p.width, p.height), (1200, 600));
        assert_eq!((p.x, p.y), ((4800 - 1200) / 2, (6300 - 600) / 2));
    }

    #[test]
    fn a_tall_image_is_limited_by_the_page_height_and_never_upscaled() {
        let p = place_on_page(100, 1000, 72.0, 1000, 500, 100, 100).unwrap();
        assert_eq!(p.height, 500);
        assert_eq!(p.width, 50);
        assert!(p.x >= 0 && p.x + p.width <= 1000);
        // A tiny image at a coarse device stays at its natural size (>= 1 px).
        let p = place_on_page(1, 1, 72.0, 1000, 1000, 72, 72).unwrap();
        assert_eq!((p.width, p.height), (1, 1));
    }

    #[test]
    fn an_anisotropic_device_keeps_the_images_physical_aspect() {
        // 300 x 600 dpi: a square image is twice as many pixels tall.
        let p = place_on_page(72, 72, 72.0, 3000, 6000, 300, 600).unwrap();
        assert_eq!((p.width, p.height), (300, 600));
    }

    #[test]
    fn degenerate_sizes_place_nothing() {
        assert_eq!(place_on_page(0, 10, 72.0, 100, 100, 72, 72), None);
        assert_eq!(place_on_page(10, 10, 0.0, 100, 100, 72, 72), None);
        assert_eq!(place_on_page(10, 10, 72.0, 0, 100, 72, 72), None);
        assert_eq!(place_on_page(10, 10, 72.0, 100, 100, 72, -1), None);
    }

    #[test]
    fn the_dib_is_bgr_over_white_with_padded_rows() {
        // 2 x 1: opaque red, then fully transparent.
        let rgba = [255, 0, 0, 255, 9, 9, 9, 0];
        let rows = bgr_rows(2, 1, &rgba).unwrap();
        // 6 bytes of pixels padded to 8.
        assert_eq!(rows, vec![0, 0, 255, 255, 255, 255, 0, 0]);
        // Half-transparent black is mid grey on paper.
        let grey = bgr_rows(1, 1, &[0, 0, 0, 128]).unwrap();
        assert_eq!(grey[0], 127);
        assert_eq!(bgr_rows(2, 2, &rgba), None, "a short buffer is refused");
    }

    /// The Print dialog is owned by the app window the shell registered, so
    /// it opens over the app and is modal to it.
    #[cfg(windows)]
    #[test]
    fn the_print_dialog_is_owned_by_the_registered_app_window() {
        let before = owner_hwnd();
        set_owner_hwnd(0x1234);
        let pd = win::dialog_request(owner_hwnd());
        set_owner_hwnd(before);
        assert_eq!(pd.hwndOwner as isize, 0x1234);
        assert_eq!(
            pd.lStructSize as usize,
            std::mem::size_of::<windows_sys::Win32::UI::Controls::Dialogs::PRINTDLGW>()
        );
        use windows_sys::Win32::UI::Controls::Dialogs::PD_RETURNDC;
        assert_ne!(pd.Flags & PD_RETURNDC, 0, "the dialog must hand back a DC");
    }

    /// The printer half of the dialog route, end to end on a real Windows
    /// printer: the flattened image goes through GDI to "Microsoft Print to
    /// PDF" (sent to a file, so no dialog), and a PDF comes out. Skipped,
    /// saying so, on a machine without that printer.
    #[cfg(windows)]
    #[test]
    fn the_gdi_route_prints_a_page_on_the_microsoft_pdf_printer() {
        use windows_sys::Win32::Graphics::Gdi::CreateDCW;
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let printer = wide("Microsoft Print to PDF");
        // SAFETY: a NUL-terminated device name; null driver/port/devmode.
        let hdc = unsafe {
            CreateDCW(
                std::ptr::null(),
                printer.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if hdc.is_null() {
            eprintln!("skipped: no \"Microsoft Print to PDF\" printer on this machine");
            return;
        }
        let dc = win::Dc(hdc);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("page.pdf");
        let out_w = wide(&out.to_string_lossy());
        let (w, h) = (48u32, 32u32);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % 256) as u8, 40, 200, 255])
            .collect();
        let result = win::print_on_dc(&dc, "w13m print test", Some(&out_w), w, h, &rgba);
        assert_eq!(result, SystemPrint::Printed);
        drop(dc);
        // The spooler writes the file after EndDoc returns.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Ok(bytes) = std::fs::read(&out) {
                if bytes.starts_with(b"%PDF") && bytes.windows(5).any(|x| x == b"%%EOF") {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no PDF came out of the printer at {}",
                out.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
