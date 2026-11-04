//! The Win32 clipboard. Implements `Clipboard`. Reference: Chromium's `ClipboardWin`, which is what
//! Electron's `clipboard` module runs on.
//!
//! An image goes on as "PNG" and `CF_DIB`. The order matters, as Chromium notes: some programs take the
//! first format they know, and Word's DIB handling is buggy, so PNG comes first. The system synthesizes
//! `CF_BITMAP` and `CF_DIBV5` for whoever asks for those.
//!
//! The clipboard is a global lock, so nothing slow happens while holding it: PNG encoding and memory
//! allocation are done before opening, and the owner window lives only for the write. Destroying it
//! leaves the data in place, and no window stays around for other applications to block on.

use std::time::{Duration, Instant};

use shotik_core::error::{Error, Result};
use shotik_core::frame::Frame;
use shotik_core::ports::Clipboard;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{SetDIBitsToDevice, BITMAPINFO, BITMAPINFOHEADER, BI_BITFIELDS, BI_RGB, DIB_RGB_COLORS};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
    GetOpenClipboardWindow, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_BITMAP, CF_DIB, CF_DIBV5, CF_HDROP, CF_LOCALE, CF_OEMTEXT, CF_TEXT, CF_UNICODETEXT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetWindowThreadProcessId, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};

use crate::capture::d3d::force_opaque;
use crate::capture::gdi::Canvas;
use crate::displays::{from_wide_buf, wide, win_err};

/// How long to wait for another application to let go. It usually holds the lock for microseconds; a
/// clipboard manager or remote desktop can take longer. Long enough for those, short enough to report.
const OPEN_BUDGET: Duration = Duration::from_millis(1000);
/// The largest decoded image we accept: a 16K frame. A larger DIB is either garbage or will not fit.
const MAX_IMAGE_BYTES: u64 = 1 << 30;

fn format_id(name: PCWSTR) -> u32 {
    // SAFETY: `name` is a NUL-terminated literal. The system returns the same id for the same name.
    unsafe { RegisterClipboardFormatW(name) }
}

fn png_format() -> u32 {
    format_id(w!("PNG"))
}

fn is_available(format: u32) -> bool {
    // SAFETY: takes a plain number, and does not need the clipboard open.
    unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

// ---------- locking ----------

/// The clipboard is open on this thread until dropped.
struct OpenGuard;

impl Drop for OpenGuard {
    fn drop(&mut self) {
        // SAFETY: only created after OpenClipboard succeeded, and closed once.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Names whoever holds the lock, because "the clipboard is busy" alone does not help anybody.
fn locked_error(waited: Duration) -> Error {
    // SAFETY: returns a handle or an error, by value.
    let who = match unsafe { GetOpenClipboardWindow() } {
        Ok(holder) if !holder.is_invalid() => {
            let mut pid = 0u32;
            // SAFETY: a stale handle just yields 0.
            unsafe { GetWindowThreadProcessId(holder, Some(&mut pid)) };
            match crate::window_source::process_name(pid) {
                Some(name) => format!("{name} (pid {pid})"),
                None => format!("process {pid}"),
            }
        }
        _ => "another application".to_owned(),
    };
    Error::msg(format!("the clipboard stayed locked by {who} for {} ms", waited.as_millis()))
}

fn open_clipboard(owner: Option<HWND>, budget: Duration) -> Result<OpenGuard> {
    let start = Instant::now();
    let mut wait = Duration::from_millis(2);
    loop {
        // SAFETY: a valid window of this thread, or none. Closed by the guard.
        if unsafe { OpenClipboard(owner) }.is_ok() {
            return Ok(OpenGuard);
        }
        let elapsed = start.elapsed();
        if elapsed >= budget {
            return Err(locked_error(elapsed));
        }
        std::thread::sleep(wait.min(budget - elapsed));
        wait = (wait * 2).min(Duration::from_millis(100));
    }
}

/// A message-only window to own the clipboard for one write. `SetClipboardData` requires an owner
/// (with none, `EmptyClipboard` leaves it NULL and the documentation says the call fails).
struct OwnerWindow(HWND);

impl OwnerWindow {
    fn create() -> Result<Self> {
        // SAFETY: a system class with no create parameter. HWND_MESSAGE makes it invisible by nature.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                PCWSTR::null(),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .map_err(|e| win_err("cannot create the clipboard owner window", e))?;
        Ok(Self(hwnd))
    }
}

impl Drop for OwnerWindow {
    fn drop(&mut self) {
        // SAFETY: created on this thread by `create` and destroyed once. The guard is not moved across
        // threads: it lives in the scope of one write.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

// ---------- global memory ----------

/// A movable global block, freed on drop unless the clipboard took it.
struct GlobalMem(HGLOBAL);

impl GlobalMem {
    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        // SAFETY: the size is what we copy below; a zero-size request is valid and we never get one here.
        let block = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
            .map_err(|e| win_err(&format!("cannot allocate {} bytes for the clipboard", bytes.len()), e))?;
        let this = Self(block);
        // SAFETY: the block is at least `bytes.len()` long, freshly allocated and locked for the copy.
        unsafe {
            let dst = GlobalLock(this.0);
            if dst.is_null() {
                return Err(Error::msg("cannot lock clipboard memory"));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst.cast::<u8>(), bytes.len());
            // Its "error" result only says the lock count reached zero.
            let _ = GlobalUnlock(this.0);
        }
        Ok(this)
    }

    /// On success the system owns the block: `SetClipboardData` frees it on the next `EmptyClipboard`.
    fn hand_to_clipboard(self, format: u32) -> Result<()> {
        // SAFETY: the clipboard is open by us and emptied, and this block is unlocked as the API requires.
        unsafe { SetClipboardData(format, Some(HANDLE(self.0 .0))) }
            .map_err(|e| win_err(&format!("cannot place format {format} on the clipboard"), e))?;
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for GlobalMem {
    fn drop(&mut self) {
        // SAFETY: still owned by us, because `hand_to_clipboard` forgets on success.
        let _ = unsafe { GlobalFree(Some(self.0)) };
    }
}

/// The bytes of a clipboard format. None when it is absent or not memory-based.
fn read_format(format: u32) -> Result<Option<Vec<u8>>> {
    // SAFETY: the clipboard is open by the caller. The handle is owned by the clipboard and stays valid
    // until it closes, and it is locked only for the copy.
    unsafe {
        let Ok(handle) = GetClipboardData(format) else {
            return Ok(None);
        };
        let block = HGLOBAL(handle.0);
        let size = GlobalSize(block);
        if size == 0 {
            return Ok(None);
        }
        let ptr = GlobalLock(block);
        if ptr.is_null() {
            return Err(Error::msg(format!("cannot lock clipboard format {format}")));
        }
        let bytes = std::slice::from_raw_parts(ptr.cast::<u8>(), size).to_vec();
        let _ = GlobalUnlock(block);
        Ok(Some(bytes))
    }
}

// ---------- DIB ----------

/// `BITMAPINFOHEADER` plus bottom-up 32-bit rows, the form every consumer reads. Top-down DIBs are valid
/// but some classic applications draw them upside down.
fn dib_from_frame(frame: &Frame) -> Vec<u8> {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        biHeight: h as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: (w * h * 4) as u32,
        ..Default::default()
    };
    let mut out = Vec::with_capacity(size_of::<BITMAPINFOHEADER>() + w * h * 4);
    // SAFETY: BITMAPINFOHEADER is plain data with no padding, viewed as bytes for the copy.
    out.extend_from_slice(unsafe {
        std::slice::from_raw_parts(std::ptr::from_ref(&header).cast::<u8>(), size_of::<BITMAPINFOHEADER>())
    });
    let stride = w * 4;
    for row in frame.bgra().chunks_exact(stride).rev() {
        out.extend_from_slice(row);
    }
    out
}

/// What a 32-bit source's alpha channel means, by Chromium's rule (`ClipboardWin::ReadBitmapInternal`):
/// Windows uses premultiplied alpha, and a colour above its alpha means the channel is garbage, so the
/// image is opaque. All-zero alpha, which plain `BI_RGB` writers leave behind, is opaque too. Otherwise
/// the pixels are unpremultiplied, because `Frame` stores straight alpha.
fn fix_alpha(bgra: &mut [u8]) {
    let mut any_alpha = false;
    let mut garbage = false;
    for px in bgra.chunks_exact(4) {
        any_alpha |= px[3] != 0;
        garbage |= px[0] > px[3] || px[1] > px[3] || px[2] > px[3];
    }
    if garbage || !any_alpha {
        for px in bgra.chunks_exact_mut(4) {
            px[3] = 255;
        }
        return;
    }
    for px in bgra.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

fn read_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Any DIB to a frame. Plain 32-bit data is read directly, keeping its alpha. Everything else (palettes,
/// 16 and 24 bits, RLE, unusual masks) goes through GDI, as in Chromium, and comes out opaque.
fn frame_from_dib(dib: &[u8]) -> Result<Frame> {
    const BITMAPINFOHEADER_SIZE: usize = 40;
    let bad = |what: &str| Error::msg(format!("the clipboard image is damaged: {what}"));
    if dib.len() < BITMAPINFOHEADER_SIZE {
        return Err(bad("too short for a header"));
    }
    let header_size = read_u32(dib, 0) as usize;
    if header_size < BITMAPINFOHEADER_SIZE || header_size > dib.len() {
        return Err(bad("wrong header size"));
    }
    let width = read_u32(dib, 4) as i32;
    let height = read_u32(dib, 8) as i32;
    let bits = u16::from_le_bytes([dib[14], dib[15]]);
    let compression = read_u32(dib, 16);
    let colors_used = read_u32(dib, 32) as usize;
    if width <= 0 || height == 0 {
        return Err(bad("empty dimensions"));
    }
    let (w, h) = (width as u64, u64::from(height.unsigned_abs()));
    if w * h * 4 > MAX_IMAGE_BYTES {
        return Err(Error::msg(format!("the clipboard image is too large: {w}x{h}")));
    }
    let palette = match bits {
        1 | 4 | 8 => {
            let entries = if colors_used != 0 { colors_used } else { 1 << bits };
            entries * 4
        }
        16 | 24 | 32 => colors_used * 4,
        _ => return Err(bad(&format!("{bits} bits per pixel"))),
    };
    // A 40-byte header with bit fields has the three masks right after it; larger headers contain them.
    let masks = if compression == BI_BITFIELDS.0 && header_size == BITMAPINFOHEADER_SIZE { 12 } else { 0 };
    let data = header_size + masks + palette;
    if data > dib.len() {
        return Err(bad("the color table runs past the end"));
    }
    let mask_at = |i: usize| read_u32(dib, BITMAPINFOHEADER_SIZE + i * 4);
    let standard_masks = compression == BI_RGB.0
        || (compression == BI_BITFIELDS.0
            && dib.len() >= BITMAPINFOHEADER_SIZE + 12
            && (mask_at(0), mask_at(1), mask_at(2)) == (0x00FF_0000, 0x0000_FF00, 0x0000_00FF));
    if bits == 32 && standard_masks {
        let stride = w as usize * 4;
        let total = stride * h as usize;
        let pixels = dib.get(data..data + total).ok_or_else(|| bad("the pixel data is truncated"))?;
        let mut out = Vec::with_capacity(total);
        if height > 0 {
            for row in pixels.chunks_exact(stride).rev() {
                out.extend_from_slice(row);
            }
        } else {
            out.extend_from_slice(pixels);
        }
        fix_alpha(&mut out);
        return Frame::new(w as u32, h as u32, out);
    }
    const BI_JPEG: u32 = 4;
    const BI_PNG: u32 = 5;
    if compression == BI_JPEG || compression == BI_PNG {
        return Err(bad("embedded JPEG or PNG is not supported"));
    }
    if compression == BI_RGB.0 {
        let stride = (w as usize * usize::from(bits)).div_ceil(32) * 4;
        if data + stride * h as usize > dib.len() {
            return Err(bad("the pixel data is truncated"));
        }
    }
    frame_from_dib_via_gdi(dib, data, w as i32, h as i32)
}

fn frame_from_dib_via_gdi(dib: &[u8], data: usize, w: i32, h: i32) -> Result<Frame> {
    let canvas = Canvas::new(w, h)?;
    // SAFETY: the source pointers stay inside `dib`, whose layout the caller checked, and the canvas is the
    // size being copied.
    let lines = unsafe {
        SetDIBitsToDevice(
            canvas.dc(),
            0,
            0,
            w as u32,
            h as u32,
            0,
            0,
            0,
            h as u32,
            dib.as_ptr().add(data).cast(),
            dib.as_ptr().cast::<BITMAPINFO>(),
            DIB_RGB_COLORS,
        )
    };
    if lines == 0 {
        return Err(Error::msg("GDI could not read the clipboard image"));
    }
    let mut out = canvas.pixels().to_vec();
    // GDI leaves alpha at 0 for sources without alpha.
    force_opaque(&mut out);
    Frame::new(w as u32, h as u32, out)
}

fn wide_bytes(text: &str) -> Vec<u8> {
    wide(text).iter().flat_map(|c| c.to_le_bytes()).collect()
}

fn text_from_wide_bytes(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    from_wide_buf(&units)
}

// ---------- the port ----------

pub struct WinClipboard {
    open_budget: Duration,
    /// Keep our writes out of the Win+V history and the cloud clipboard, for test mode: test images must
    /// not pile up in the user's history. Also marks the entry, so a test can tell it is still ours.
    exclude_from_history: bool,
}

impl Default for WinClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl WinClipboard {
    pub fn new() -> Self {
        Self { open_budget: OPEN_BUDGET, exclude_from_history: false }
    }

    /// Writes stay out of the clipboard history. For `--test` and ghost instances.
    pub fn excluded_from_history() -> Self {
        Self { open_budget: OPEN_BUDGET, exclude_from_history: true }
    }

    /// How long to wait for another application to release the clipboard.
    pub fn with_open_budget(mut self, budget: Duration) -> Self {
        self.open_budget = budget;
        self
    }

    /// The real formats on the clipboard right now, by name, for diagnostics. `formats` reports what
    /// Electron's `availableFormats` reports, which is a different and shorter list.
    pub fn native_formats(&self) -> Result<Vec<String>> {
        let _open = open_clipboard(None, self.open_budget)?;
        let mut names = Vec::new();
        let mut format = 0;
        loop {
            // SAFETY: the clipboard is open; 0 starts the enumeration and ends it.
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            names.push(format_name(format));
        }
        Ok(names)
    }

    fn write(&self, image: Option<&Frame>, text: Option<&str>) -> Result<()> {
        // All the slow work comes first, so the global lock is held only for a few handle swaps.
        let mut items: Vec<(u32, GlobalMem)> = Vec::new();
        if let Some(image) = image {
            items.push((png_format(), GlobalMem::from_bytes(&image.to_png()?)?));
            items.push((u32::from(CF_DIB.0), GlobalMem::from_bytes(&dib_from_frame(image))?));
        }
        if let Some(text) = text {
            items.push((u32::from(CF_UNICODETEXT.0), GlobalMem::from_bytes(&wide_bytes(text))?));
        }
        if self.exclude_from_history {
            let zero = 0u32.to_le_bytes();
            items.push((format_id(w!("CanIncludeInClipboardHistory")), GlobalMem::from_bytes(&zero)?));
            items.push((format_id(w!("CanUploadToCloudClipboard")), GlobalMem::from_bytes(&zero)?));
            items.push((format_id(w!("ExcludeClipboardContentFromMonitorProcessing")), GlobalMem::from_bytes(&zero)?));
            items.push((owned_marker(), GlobalMem::from_bytes(&zero)?));
        }
        let owner = OwnerWindow::create()?;
        let open = open_clipboard(Some(owner.0), self.open_budget)?;
        // SAFETY: the clipboard is open with our owner window.
        unsafe { EmptyClipboard() }.map_err(|e| win_err("cannot empty the clipboard", e))?;
        for (format, memory) in items {
            memory.hand_to_clipboard(format)?;
        }
        drop(open);
        drop(owner);
        Ok(())
    }
}

/// A format only `excluded_from_history` writes: lets a test see that the entry is still its own.
fn owned_marker() -> u32 {
    format_id(w!("ShotikTestOwned"))
}

fn format_name(format: u32) -> String {
    let standard = [
        (CF_TEXT.0, "CF_TEXT"),
        (CF_BITMAP.0, "CF_BITMAP"),
        (CF_OEMTEXT.0, "CF_OEMTEXT"),
        (CF_DIB.0, "CF_DIB"),
        (CF_UNICODETEXT.0, "CF_UNICODETEXT"),
        (CF_HDROP.0, "CF_HDROP"),
        (CF_LOCALE.0, "CF_LOCALE"),
        (CF_DIBV5.0, "CF_DIBV5"),
    ];
    if let Some((_, name)) = standard.iter().find(|(id, _)| u32::from(*id) == format) {
        return (*name).to_owned();
    }
    let mut buf = [0u16; 256];
    // SAFETY: the buffer and its length are valid; a standard id without a name returns 0.
    let len = unsafe { GetClipboardFormatNameW(format, &mut buf) };
    if len > 0 {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        format!("format {format}")
    }
}

impl Clipboard for WinClipboard {
    fn write_image(&self, image: &Frame) -> Result<()> {
        self.write(Some(image), None)
    }

    fn write_image_and_text(&self, image: &Frame, text: &str) -> Result<()> {
        self.write(Some(image), Some(text))
    }

    fn write_text(&self, text: &str) -> Result<()> {
        self.write(None, Some(text))
    }

    /// Empty when there is no text, like `clipboard.readText()`.
    fn read_text(&self) -> Result<String> {
        let _open = open_clipboard(None, self.open_budget)?;
        Ok(read_format(u32::from(CF_UNICODETEXT.0))?.map_or_else(String::new, |b| text_from_wide_bytes(&b)))
    }

    fn read_image(&self) -> Result<Option<Frame>> {
        let (png, dib) = {
            let _open = open_clipboard(None, self.open_budget)?;
            let png = read_format(png_format())?;
            // A PNG keeps the alpha channel, so only read the DIB when there is no PNG.
            let dib = if png.is_none() { read_format(u32::from(CF_DIB.0))? } else { None };
            (png, dib)
        };
        // Decoding happens after the lock is released.
        match (png, dib) {
            (Some(png), _) => Frame::from_png(&png).map(Some),
            (None, Some(dib)) => frame_from_dib(&dib).map(Some),
            (None, None) => Ok(None),
        }
    }

    fn clear(&self) -> Result<()> {
        let owner = OwnerWindow::create()?;
        let _open = open_clipboard(Some(owner.0), self.open_budget)?;
        // SAFETY: the clipboard is open with our owner window.
        unsafe { EmptyClipboard() }.map_err(|e| win_err("cannot empty the clipboard", e))
    }

    /// The same names and order as Electron's `clipboard.availableFormats()` (`GetStandardFormats` in
    /// `ClipboardWin`). See `native_formats` for what is really there.
    fn formats(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut add = |available: bool, name: &str| {
            if available {
                out.push(name.to_owned());
            }
        };
        add(is_available(u32::from(CF_TEXT.0)), "text/plain");
        add(is_available(format_id(w!("HTML Format"))), "text/html");
        add(is_available(format_id(w!("image/svg+xml"))), "image/svg+xml");
        add(is_available(format_id(w!("Rich Text Format"))), "text/rtf");
        add(is_available(u32::from(CF_DIB.0)), "image/png");
        add(is_available(u32::from(CF_HDROP.0)), "text/uri-list");
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
    use std::sync::Mutex;

    /// Clipboard tests share one system-wide resource with the user: one at a time in this process.
    static EXCLUSIVE: Mutex<()> = Mutex::new(());

    /// Everything memory-based on the clipboard, including images. Handle-based formats are left out: the
    /// system re-creates `CF_BITMAP` from the DIB, as it does the text variants from `CF_UNICODETEXT`.
    struct Saved {
        formats: Vec<(u32, Vec<u8>)>,
    }

    impl Saved {
        fn take() -> Self {
            let _open = open_clipboard(None, OPEN_BUDGET).expect("the clipboard is free enough to read");
            let synthesized = [CF_TEXT.0, CF_OEMTEXT.0, CF_LOCALE.0, CF_DIBV5.0, CF_BITMAP.0].map(u32::from);
            let mut formats = Vec::new();
            let mut format = 0;
            loop {
                // SAFETY: the clipboard is open; 0 starts and ends the enumeration.
                format = unsafe { EnumClipboardFormats(format) };
                if format == 0 {
                    break;
                }
                if synthesized.contains(&format) {
                    continue;
                }
                if let Ok(Some(bytes)) = read_format(format) {
                    formats.push((format, bytes));
                }
            }
            Self { formats }
        }

        fn restore(self) {
            let owner = OwnerWindow::create().expect("owner window");
            let _open = open_clipboard(Some(owner.0), OPEN_BUDGET).expect("the clipboard is free enough to restore");
            // SAFETY: the clipboard is open with our owner window.
            unsafe { EmptyClipboard() }.expect("empty");
            for (format, bytes) in self.formats {
                let _ = GlobalMem::from_bytes(&bytes).and_then(|m| m.hand_to_clipboard(format));
            }
            // The user's own entry coming back is not news for the Win+V history.
            for format in [format_id(w!("CanIncludeInClipboardHistory")), format_id(w!("CanUploadToCloudClipboard"))] {
                let _ = GlobalMem::from_bytes(&0u32.to_le_bytes()).and_then(|m| m.hand_to_clipboard(format));
            }
        }
    }

    /// Saves the user's clipboard, runs the test, puts it back. The window of change is the test's own
    /// few writes. If the user or another app copied something meanwhile, their newer entry stays.
    fn with_clipboard<T>(test: impl FnOnce(&WinClipboard) -> T) -> T {
        let _one_at_a_time = EXCLUSIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = Saved::take();
        let clip = WinClipboard::excluded_from_history();
        let outcome = catch_unwind(AssertUnwindSafe(|| test(&clip)));
        let ours = is_available(owned_marker()) || clip.native_formats().is_ok_and(|f| f.is_empty());
        if ours {
            saved.restore();
        }
        match outcome {
            Ok(value) => value,
            Err(panic) => resume_unwind(panic),
        }
    }

    /// 3x2 with distinct, opaque colours: blue green red / white black gray.
    fn sample() -> Frame {
        Frame::new(
            3,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, //
                255, 255, 255, 255, 0, 0, 0, 255, 128, 128, 128, 255,
            ],
        )
        .unwrap()
    }

    // ----- pure -----

    #[test]
    fn a_dib_is_bottom_up_with_a_plain_header() {
        let dib = dib_from_frame(&sample());
        assert_eq!(read_u32(&dib, 0), 40);
        assert_eq!(read_u32(&dib, 4), 3);
        assert_eq!(read_u32(&dib, 8), 2, "positive height: bottom-up");
        assert_eq!(u16::from_le_bytes([dib[14], dib[15]]), 32);
        assert_eq!(read_u32(&dib, 16), BI_RGB.0);
        assert_eq!(dib.len(), 40 + 3 * 2 * 4);
        assert_eq!(&dib[40..44], &[255, 255, 255, 255], "the bottom row comes first");
        assert_eq!(&dib[52..56], &[255, 0, 0, 255], "then the top row");
    }

    #[test]
    fn a_frame_survives_the_trip_through_a_dib() {
        let back = frame_from_dib(&dib_from_frame(&sample())).unwrap();
        assert_eq!((back.width(), back.height()), (3, 2));
        assert_eq!(back.bgra(), sample().bgra());
    }

    fn header(w: i32, h: i32, bits: u16, compression: u32, colors: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(&compression.to_le_bytes());
        v.extend_from_slice(&[0; 12]);
        v.extend_from_slice(&colors.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v
    }

    #[test]
    fn a_top_down_dib_reads_in_order() {
        let mut dib = header(1, -2, 32, BI_RGB.0, 0);
        dib.extend_from_slice(&[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn zero_alpha_from_a_plain_writer_means_opaque() {
        let mut dib = header(1, 1, 32, BI_RGB.0, 0);
        dib.extend_from_slice(&[10, 20, 30, 0]);
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[10, 20, 30, 255]);
    }

    #[test]
    fn colours_above_alpha_mean_garbage_alpha_and_valid_alpha_is_unpremultiplied() {
        let mut garbage = header(1, 1, 32, BI_RGB.0, 0);
        garbage.extend_from_slice(&[200, 20, 30, 100]);
        assert_eq!(frame_from_dib(&garbage).unwrap().bgra(), &[200, 20, 30, 255]);

        let mut premultiplied = header(1, 1, 32, BI_RGB.0, 0);
        premultiplied.extend_from_slice(&[50, 25, 0, 100]);
        assert_eq!(frame_from_dib(&premultiplied).unwrap().bgra(), &[128, 64, 0, 100]);
    }

    #[test]
    fn a_24_bit_dib_goes_through_gdi_and_comes_out_opaque() {
        let mut dib = header(2, 1, 24, BI_RGB.0, 0);
        dib.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]); // Two pixels and two bytes of row padding.
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn an_8_bit_palette_dib_goes_through_gdi() {
        let mut dib = header(2, 1, 8, BI_RGB.0, 2);
        dib.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]); // Palette.
        dib.extend_from_slice(&[1, 0, 0, 0]); // Indices with row padding.
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[40, 50, 60, 255, 10, 20, 30, 255]);
    }

    #[test]
    fn standard_bit_fields_keep_their_data_and_others_go_through_gdi() {
        let mut dib = header(1, 1, 32, BI_BITFIELDS.0, 0);
        for mask in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF] {
            dib.extend_from_slice(&mask.to_le_bytes());
        }
        dib.extend_from_slice(&[10, 20, 30, 255]);
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[10, 20, 30, 255]);

        let mut swapped = header(1, 1, 32, BI_BITFIELDS.0, 0);
        for mask in [0x0000_00FFu32, 0x0000_FF00, 0x00FF_0000] {
            swapped.extend_from_slice(&mask.to_le_bytes());
        }
        swapped.extend_from_slice(&[10, 20, 30, 255]);
        assert_eq!(frame_from_dib(&swapped).unwrap().bgra(), &[30, 20, 10, 255], "red and blue trade places");
    }

    #[test]
    fn damaged_dibs_are_errors_not_panics() {
        assert!(frame_from_dib(&[]).is_err());
        assert!(frame_from_dib(&[1, 2, 3]).is_err());
        assert!(frame_from_dib(&header(0, 1, 32, BI_RGB.0, 0)).is_err());
        assert!(frame_from_dib(&header(1, 0, 32, BI_RGB.0, 0)).is_err());
        assert!(frame_from_dib(&header(100, 100, 32, BI_RGB.0, 0)).is_err(), "no pixels");
        assert!(frame_from_dib(&header(100, 100, 24, BI_RGB.0, 0)).is_err(), "no pixels");
        assert!(frame_from_dib(&header(1, 1, 7, BI_RGB.0, 0)).is_err(), "impossible depth");
        assert!(frame_from_dib(&header(1, 1, 8, BI_RGB.0, 1 << 20)).is_err(), "palette past the end");
        assert!(frame_from_dib(&header(1, 1, 24, 4, 0)).is_err(), "embedded JPEG");
        let huge = frame_from_dib(&header(1 << 20, 1 << 20, 32, BI_RGB.0, 0)).unwrap_err().to_string();
        assert!(huge.contains("too large"), "{huge}");
    }

    #[test]
    fn text_round_trips_through_bytes() {
        assert_eq!(text_from_wide_bytes(&wide_bytes("C:\\путь\\файл 🙂.png")), "C:\\путь\\файл 🙂.png");
        assert_eq!(text_from_wide_bytes(&wide_bytes("")), "");
    }

    // ----- the real clipboard, saved and restored around each test -----

    #[test]
    fn plain_copy_puts_only_the_image_in_both_formats() {
        let (formats, native, image, text) = with_clipboard(|c| {
            c.write_image(&sample()).unwrap();
            (c.formats().unwrap(), c.native_formats().unwrap(), c.read_image().unwrap(), c.read_text().unwrap())
        });
        assert_eq!(formats, vec!["image/png"], "no text: apps that prefer text must paste the picture");
        assert_eq!(text, "");
        assert_eq!(image.unwrap().bgra(), sample().bgra());
        let png = native.iter().position(|f| f == "PNG").expect("PNG present");
        let dib = native.iter().position(|f| f == "CF_DIB").expect("DIB present");
        assert!(png < dib, "Word takes the first format it knows: PNG first, {native:?}");
        assert!(native.iter().any(|f| f == "CF_BITMAP"), "synthesized for classic apps, {native:?}");
    }

    #[test]
    fn image_and_text_share_one_entry() {
        let (formats, text, image) = with_clipboard(|c| {
            c.write_image_and_text(&sample(), r"C:\Shots\Шот 1.png").unwrap();
            (c.formats().unwrap(), c.read_text().unwrap(), c.read_image().unwrap())
        });
        assert_eq!(formats, vec!["text/plain", "image/png"], "same names as availableFormats");
        assert_eq!(text, r"C:\Shots\Шот 1.png");
        assert_eq!(image.unwrap().bgra(), sample().bgra());
    }

    #[test]
    fn text_only_has_no_image() {
        let (text, image, formats) = with_clipboard(|c| {
            c.write_text("привет 🙂\nline two").unwrap();
            (c.read_text().unwrap(), c.read_image().unwrap(), c.formats().unwrap())
        });
        assert_eq!(text, "привет 🙂\nline two");
        assert!(image.is_none());
        assert_eq!(formats, vec!["text/plain"]);
    }

    #[test]
    fn a_dib_only_entry_from_another_app_reads_as_an_image() {
        let image = with_clipboard(|c| {
            let owner = OwnerWindow::create().unwrap();
            {
                let _open = open_clipboard(Some(owner.0), OPEN_BUDGET).unwrap();
                // SAFETY: the clipboard is open with our owner window.
                unsafe { EmptyClipboard() }.unwrap();
                GlobalMem::from_bytes(&dib_from_frame(&sample()))
                    .unwrap()
                    .hand_to_clipboard(u32::from(CF_DIB.0))
                    .unwrap();
                GlobalMem::from_bytes(&0u32.to_le_bytes()).unwrap().hand_to_clipboard(owned_marker()).unwrap();
            }
            c.read_image().unwrap()
        });
        assert_eq!(image.unwrap().bgra(), sample().bgra());
    }

    #[test]
    fn clear_leaves_nothing() {
        let (formats, native, text) = with_clipboard(|c| {
            c.write_text("x").unwrap();
            c.clear().unwrap();
            (c.formats().unwrap(), c.native_formats().unwrap(), c.read_text().unwrap())
        });
        assert!(formats.is_empty(), "{formats:?}");
        assert!(native.is_empty(), "{native:?}");
        assert_eq!(text, "");
    }

    /// Holds the clipboard from another thread, like an app in the middle of a copy.
    fn hold_clipboard_for(duration: Duration) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<()>) {
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            let owner = OwnerWindow::create().unwrap();
            let open = open_clipboard(Some(owner.0), OPEN_BUDGET).unwrap();
            held_tx.send(()).unwrap();
            std::thread::sleep(duration);
            drop(open);
            drop(owner);
        });
        (join, held_rx)
    }

    #[test]
    fn a_write_waits_out_a_brief_lock() {
        let ok = with_clipboard(|c| {
            let (join, held) = hold_clipboard_for(Duration::from_millis(40));
            held.recv().unwrap();
            let t = Instant::now();
            c.write_text("after the lock").unwrap();
            let waited = t.elapsed();
            join.join().unwrap();
            assert!(waited >= Duration::from_millis(20), "it had to wait, waited {waited:?}");
            c.read_text().unwrap()
        });
        assert_eq!(ok, "after the lock");
    }

    #[test]
    fn a_lock_that_stays_is_an_explicit_error_and_says_who() {
        let message = with_clipboard(|_| {
            let (join, held) = hold_clipboard_for(Duration::from_millis(90));
            held.recv().unwrap();
            let impatient = WinClipboard::excluded_from_history().with_open_budget(Duration::from_millis(20));
            let err = impatient.write_text("never").unwrap_err().to_string();
            let read_err = impatient.read_text().unwrap_err().to_string();
            join.join().unwrap();
            assert!(read_err.contains("locked"), "{read_err}");
            err
        });
        assert!(message.contains("stayed locked"), "{message}");
        // The holder is this test process, which the error must name.
        let me = std::env::current_exe().unwrap();
        let stem = me.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(message.contains(&stem) || message.contains("pid"), "{message}");
    }

    #[test]
    fn saved_contents_come_back_including_images() {
        with_clipboard(|c| {
            c.write_image_and_text(&sample(), "keep me").unwrap();
            let saved = Saved::take();
            c.write_text("something else").unwrap();
            saved.restore();
            assert_eq!(c.read_text().unwrap(), "keep me");
            assert_eq!(c.read_image().unwrap().unwrap().bgra(), sample().bgra());
            // Leaves an entry marked as ours, so the outer restore knows it may put the user's back.
            c.write_text("x").unwrap();
        });
    }
}
