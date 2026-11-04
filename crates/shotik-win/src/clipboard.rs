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
//!
//! What is on the clipboard was written by any application, so it is untrusted input. A DIB is decoded here in
//! safe Rust, every size is checked with checked arithmetic before anything is allocated or read, and whatever
//! this code does not decode itself, such as RLE or embedded JPEG, is refused with an error.

use std::time::{Duration, Instant};

use shotik_core::error::{Error, Result};
use shotik_core::frame::Frame;
use shotik_core::ports::Clipboard;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{BI_BITFIELDS, BI_RGB};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
    GetOpenClipboardWindow, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_BITMAP, CF_DIB, CF_DIBV5, CF_HDROP, CF_LOCALE, CF_OEMTEXT, CF_TEXT, CF_UNICODETEXT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetWindowThreadProcessId, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};

use crate::displays::{from_wide_buf, wide, win_err};

/// How long to wait for another application to let go. It usually holds the lock for microseconds; a
/// clipboard manager or remote desktop can take longer. Long enough for those, short enough to report.
const OPEN_BUDGET: Duration = Duration::from_millis(1000);
/// The largest decoded image we accept, the same limit the PNG decoder applies. A small DIB can claim a huge
/// image: at one bit per pixel, 16 MB of input decodes to 512 MB.
const MAX_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

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
    // until it closes, and it is locked only for the copy. `GlobalSize` is the real size of the block whatever
    // the data claims inside it, and for a format that is not memory-based it returns 0, which ends here.
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
    let size = w * h * 4;
    let mut out = Vec::with_capacity(BITMAPINFOHEADER_SIZE + size);
    out.extend_from_slice(&(BITMAPINFOHEADER_SIZE as u32).to_le_bytes()); // biSize
    out.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(h as i32).to_le_bytes()); // biHeight: positive means bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&BI_RGB.0.to_le_bytes()); // biCompression
    out.extend_from_slice(&u32::try_from(size).unwrap_or(0).to_le_bytes()); // biSizeImage: 0 is valid for BI_RGB
    out.extend_from_slice(&[0; 16]); // resolution and colour counts
    for row in frame.bgra().chunks_exact(w * 4).rev() {
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

const BITMAPINFOHEADER_SIZE: usize = 40;
/// The header sizes Windows defines beyond `BITMAPCOREHEADER`, which the clipboard does not carry:
/// `BITMAPINFOHEADER`, V2, V3, V4 and V5.
const DIB_HEADER_SIZES: [usize; 5] = [BITMAPINFOHEADER_SIZE, 52, 56, 108, 124];

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    let bytes = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    let bytes = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn damaged(what: &str) -> Error {
    Error::msg(format!("the clipboard image is damaged: {what}"))
}

/// Colour masks of a `BI_BITFIELDS` DIB, or the implicit ones of a 16-bit `BI_RGB` one.
#[derive(Clone, Copy)]
struct Masks {
    red: u32,
    green: u32,
    blue: u32,
    /// 0 when the image has no alpha mask.
    alpha: u32,
}

impl Masks {
    /// The layout of plain 32-bit data, where a pixel's bytes already are BGRA.
    fn are_bgra(&self) -> bool {
        (self.red, self.green, self.blue) == (0x00FF_0000, 0x0000_FF00, 0x0000_00FF)
    }

    fn check(&self) -> Result<()> {
        if self.red | self.green | self.blue == 0 {
            return Err(damaged("the colour masks are empty"));
        }
        for mask in [self.red, self.green, self.blue, self.alpha] {
            let run = mask.checked_shr(mask.trailing_zeros()).unwrap_or(0);
            if run & run.wrapping_add(1) != 0 {
                return Err(damaged("a colour mask is not one run of bits"));
            }
        }
        Ok(())
    }
}

/// One channel of a pixel, scaled to 8 bits. A zero mask means the channel is absent.
fn channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = u64::from(mask >> shift);
    let value = u64::from((pixel & mask) >> shift);
    ((value * 255 + max / 2) / max) as u8
}

/// A checked description of a DIB: everything in it has been validated against the length of the buffer.
struct DibLayout {
    width: usize,
    height: usize,
    top_down: bool,
    bits: u16,
    stride: usize,
    /// Where the pixels start in the buffer. `stride * height` bytes from here are in bounds.
    data: usize,
    /// The colour table: a range of the buffer, in bounds, with whole 4-byte entries.
    palette: std::ops::Range<usize>,
    masks: Option<Masks>,
}

/// Validates a DIB before anything else touches it. It accepts `BI_RGB` at 1, 4, 8, 16, 24 and 32 bits, and
/// `BI_BITFIELDS` at 16 and 32, top-down or bottom-up. Everything else is an error, including RLE, whose length
/// cannot be known without decoding it. Arithmetic is on `u64` and checked, so no header can wrap it around.
fn parse_dib(dib: &[u8]) -> Result<DibLayout> {
    let header_size = le_u32(dib, 0).ok_or_else(|| damaged("too short for a header"))? as usize;
    if !DIB_HEADER_SIZES.contains(&header_size) {
        return Err(damaged("unknown header size"));
    }
    if dib.len() < header_size {
        return Err(damaged("too short for its header"));
    }
    // All of these are inside the 40 bytes that were just checked to be there.
    let field = |at| le_u32(dib, at).ok_or_else(|| damaged("too short for a header"));
    let width = field(4)? as i32;
    let height = field(8)? as i32;
    let planes = le_u16(dib, 12).ok_or_else(|| damaged("too short for a header"))?;
    let bits = le_u16(dib, 14).ok_or_else(|| damaged("too short for a header"))?;
    let compression = field(16)?;
    let colors_used = u64::from(field(32)?);

    // A negative height means top-down, and that is the only negative value that means anything.
    if width <= 0 || height == 0 {
        return Err(damaged("empty dimensions"));
    }
    if planes != 1 {
        return Err(damaged("it does not have exactly one plane"));
    }
    let bitfields = compression == BI_BITFIELDS.0;
    if !bitfields && compression != BI_RGB.0 {
        return Err(Error::msg(format!("the clipboard image uses unsupported compression {compression}")));
    }
    let depth_ok = match bits {
        1 | 4 | 8 | 24 => !bitfields,
        16 | 32 => true,
        _ => false,
    };
    if !depth_ok {
        return Err(damaged(&format!("{bits} bits per pixel with compression {compression}")));
    }

    let (w, h) = (u64::from(width as u32), u64::from(height.unsigned_abs()));
    let too_large = || Error::msg(format!("the clipboard image is too large: {w}x{h}"));
    let decoded = w.checked_mul(h).and_then(|pixels| pixels.checked_mul(4)).ok_or_else(too_large)?;
    if decoded > MAX_IMAGE_BYTES {
        return Err(too_large());
    }
    let stride = w.checked_mul(u64::from(bits)).ok_or_else(too_large)?.div_ceil(32) * 4;
    let pixel_bytes = stride.checked_mul(h).ok_or_else(too_large)?;

    let masks = if bitfields {
        // Inside the header from V2 on, right after it for a plain BITMAPINFOHEADER.
        let mask = |at| le_u32(dib, at).ok_or_else(|| damaged("the colour masks run past the end"));
        let alpha = if header_size >= 56 { mask(52)? } else { 0 };
        let masks = Masks { red: mask(40)?, green: mask(44)?, blue: mask(48)?, alpha };
        masks.check()?;
        Some(masks)
    } else if bits == 16 {
        Some(Masks { red: 0x7C00, green: 0x03E0, blue: 0x001F, alpha: 0 })
    } else {
        None
    };
    let masks_after_header: u64 = if bitfields && header_size == BITMAPINFOHEADER_SIZE { 12 } else { 0 };

    let entries = if bits <= 8 {
        let all = 1u64 << bits;
        match colors_used {
            0 => all,
            used if used <= all => used,
            _ => return Err(damaged("more colours than the depth allows")),
        }
    } else {
        // Optional at higher depths, but it still sits in front of the pixels when present.
        colors_used
    };
    let palette_start = header_size as u64 + masks_after_header;
    let palette_end = entries.checked_mul(4).and_then(|bytes| palette_start.checked_add(bytes));
    let palette_end = palette_end.ok_or_else(|| damaged("the colour table does not fit"))?;
    let end = palette_end.checked_add(pixel_bytes).ok_or_else(too_large)?;
    if palette_end > dib.len() as u64 {
        return Err(damaged("the colour table runs past the end"));
    }
    if end > dib.len() as u64 {
        return Err(damaged("the pixel data is truncated"));
    }
    // All below `dib.len()`, so they fit a usize.
    Ok(DibLayout {
        width: w as usize,
        height: h as usize,
        top_down: height < 0,
        bits,
        stride: stride as usize,
        data: palette_end as usize,
        palette: palette_start as usize..palette_end as usize,
        masks,
    })
}

/// One row through a colour table. A DIB can claim fewer colours than its depth allows and still use the
/// indexes beyond them, which is damage and not black.
fn decode_indexed_row(src: &[u8], bits: u16, palette: &[u8], dst: &mut [u8]) -> Result<()> {
    for (x, px) in dst.chunks_exact_mut(4).enumerate() {
        let index = match bits {
            8 => src[x],
            4 => (src[x / 2] >> if x % 2 == 0 { 4 } else { 0 }) & 0x0F,
            _ => (src[x / 8] >> (7 - x % 8)) & 0x01,
        };
        let entry = usize::from(index) * 4;
        let color = palette.get(entry..entry + 3).ok_or_else(|| damaged("a pixel uses a colour that is not in its table"))?;
        px.copy_from_slice(&[color[0], color[1], color[2], 255]);
    }
    Ok(())
}

/// Any DIB `parse_dib` accepts to a frame, without leaving safe Rust.
fn frame_from_dib(dib: &[u8]) -> Result<Frame> {
    let layout = parse_dib(dib)?;
    let (w, h, stride) = (layout.width, layout.height, layout.stride);
    let pixels = dib.get(layout.data..layout.data + stride * h).ok_or_else(|| damaged("the pixel data is truncated"))?;
    let palette = dib.get(layout.palette.clone()).ok_or_else(|| damaged("the colour table runs past the end"))?;

    // Plain 32-bit data keeps its alpha. Everything else is opaque unless it has a real alpha mask.
    let plain_32 = layout.bits == 32 && layout.masks.is_none_or(|m| m.are_bgra());
    let has_alpha = plain_32 || layout.masks.is_some_and(|m| m.alpha != 0);

    let mut out = vec![0u8; w * h * 4];
    for (y, dst) in out.chunks_exact_mut(w * 4).enumerate() {
        let row = if layout.top_down { y } else { h - 1 - y };
        let src = &pixels[row * stride..(row + 1) * stride];
        match (layout.bits, layout.masks) {
            (32, _) if plain_32 => dst.copy_from_slice(&src[..w * 4]),
            (32, Some(m)) => {
                for (px, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                    let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                    let alpha = if m.alpha == 0 { 255 } else { channel(v, m.alpha) };
                    d.copy_from_slice(&[channel(v, m.blue), channel(v, m.green), channel(v, m.red), alpha]);
                }
            }
            (24, _) => {
                for (px, d) in src.chunks_exact(3).zip(dst.chunks_exact_mut(4)) {
                    d.copy_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
            (16, Some(m)) => {
                for (px, d) in src.chunks_exact(2).zip(dst.chunks_exact_mut(4)) {
                    let v = u32::from(u16::from_le_bytes([px[0], px[1]]));
                    d.copy_from_slice(&[channel(v, m.blue), channel(v, m.green), channel(v, m.red), 255]);
                }
            }
            (1 | 4 | 8, _) => decode_indexed_row(src, layout.bits, palette, dst)?,
            // `parse_dib` admits no other combination.
            _ => return Err(damaged("an unexpected pixel format")),
        }
    }
    if has_alpha {
        fix_alpha(&mut out);
    }
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
        assert_eq!(le_u32(&dib, 0), Some(40));
        assert_eq!(le_u32(&dib, 4), Some(3));
        assert_eq!(le_u32(&dib, 8), Some(2), "positive height: bottom-up");
        assert_eq!(le_u16(&dib, 12), Some(1));
        assert_eq!(le_u16(&dib, 14), Some(32));
        assert_eq!(le_u32(&dib, 16), Some(BI_RGB.0));
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

    fn with_masks(mut dib: Vec<u8>, masks: [u32; 3]) -> Vec<u8> {
        for mask in masks {
            dib.extend_from_slice(&mask.to_le_bytes());
        }
        dib
    }

    /// A valid DIB of each kind the decoder accepts, for the truncation and mutation tests.
    fn valid_dibs() -> Vec<Vec<u8>> {
        let mut plain32 = header(2, 2, 32, BI_RGB.0, 0);
        plain32.extend_from_slice(&[1; 16]);
        let mut top_down = header(2, -2, 32, BI_RGB.0, 0);
        top_down.extend_from_slice(&[2; 16]);
        let mut bgr = header(2, 2, 24, BI_RGB.0, 0);
        bgr.extend_from_slice(&[3; 16]);
        let mut rgb555 = header(2, 2, 16, BI_RGB.0, 0);
        rgb555.extend_from_slice(&[4; 8]);
        let mut fields32 = with_masks(header(2, 2, 32, BI_BITFIELDS.0, 0), [0xFF, 0xFF00, 0xFF_0000]);
        fields32.extend_from_slice(&[5; 16]);
        let mut fields16 = with_masks(header(2, 2, 16, BI_BITFIELDS.0, 0), [0xF800, 0x07E0, 0x001F]);
        fields16.extend_from_slice(&[6; 8]);
        let mut indexed8 = header(2, 2, 8, BI_RGB.0, 2);
        indexed8.extend_from_slice(&[9, 9, 9, 0, 8, 8, 8, 0, 0, 1, 0, 0, 1, 0, 0, 0]);
        let mut indexed4 = header(2, 2, 4, BI_RGB.0, 2);
        indexed4.extend_from_slice(&[9, 9, 9, 0, 8, 8, 8, 0, 0x01, 0, 0, 0, 0x10, 0, 0, 0]);
        let mut indexed1 = header(2, 2, 1, BI_RGB.0, 0);
        indexed1.extend_from_slice(&[9, 9, 9, 0, 8, 8, 8, 0, 0x40, 0, 0, 0, 0x80, 0, 0, 0]);
        vec![plain32, top_down, bgr, rgb555, fields32, fields16, indexed8, indexed4, indexed1]
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
    fn a_24_bit_dib_skips_row_padding_and_comes_out_opaque() {
        let mut dib = header(2, 1, 24, BI_RGB.0, 0);
        dib.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]); // Two pixels and two bytes of row padding.
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn a_16_bit_dib_is_555_by_default_and_follows_its_masks() {
        let mut plain = header(1, 1, 16, BI_RGB.0, 0);
        plain.extend_from_slice(&[0xFF, 0x7F, 0, 0]); // White in 5-5-5.
        assert_eq!(frame_from_dib(&plain).unwrap().bgra(), &[255, 255, 255, 255]);

        let mut plain = header(1, 1, 16, BI_RGB.0, 0);
        plain.extend_from_slice(&[0x00, 0x7C, 0, 0]); // Pure red in 5-5-5.
        assert_eq!(frame_from_dib(&plain).unwrap().bgra(), &[0, 0, 255, 255]);

        let mut fields = with_masks(header(1, 1, 16, BI_BITFIELDS.0, 0), [0xF800, 0x07E0, 0x001F]);
        fields.extend_from_slice(&[0xE0, 0x07, 0, 0]); // Pure green in 5-6-5.
        assert_eq!(frame_from_dib(&fields).unwrap().bgra(), &[0, 255, 0, 255]);
    }

    #[test]
    fn indexed_dibs_read_through_their_table() {
        let mut eight = header(2, 1, 8, BI_RGB.0, 2);
        eight.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]); // The table.
        eight.extend_from_slice(&[1, 0, 0, 0]); // Indexes with row padding.
        assert_eq!(frame_from_dib(&eight).unwrap().bgra(), &[40, 50, 60, 255, 10, 20, 30, 255]);

        let mut four = header(2, 1, 4, BI_RGB.0, 2);
        four.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]);
        four.extend_from_slice(&[0x10, 0, 0, 0]);
        assert_eq!(frame_from_dib(&four).unwrap().bgra(), &[40, 50, 60, 255, 10, 20, 30, 255]);

        let mut one = header(2, 1, 1, BI_RGB.0, 0);
        one.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]);
        one.extend_from_slice(&[0x80, 0, 0, 0]);
        assert_eq!(frame_from_dib(&one).unwrap().bgra(), &[40, 50, 60, 255, 10, 20, 30, 255]);
    }

    #[test]
    fn a_colour_table_in_front_of_true_colour_pixels_is_skipped() {
        let mut dib = header(1, 1, 24, BI_RGB.0, 2);
        dib.extend_from_slice(&[9; 8]); // An optional table the pixels do not use.
        dib.extend_from_slice(&[1, 2, 3, 0]);
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[1, 2, 3, 255]);
    }

    #[test]
    fn bit_fields_follow_their_masks() {
        let standard = with_masks(header(1, 1, 32, BI_BITFIELDS.0, 0), [0x00FF_0000, 0x0000_FF00, 0x0000_00FF]);
        let mut dib = standard;
        dib.extend_from_slice(&[10, 20, 30, 255]);
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[10, 20, 30, 255], "the data is BGRA already");

        let swapped = with_masks(header(1, 1, 32, BI_BITFIELDS.0, 0), [0x0000_00FF, 0x0000_FF00, 0x00FF_0000]);
        let mut dib = swapped;
        dib.extend_from_slice(&[10, 20, 30, 77]);
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[30, 20, 10, 255], "red and blue trade places, no alpha mask");
    }

    #[test]
    fn a_v5_header_keeps_its_masks_inside() {
        let mut dib = vec![0u8; 124];
        dib[0..4].copy_from_slice(&124u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[16..20].copy_from_slice(&BI_BITFIELDS.0.to_le_bytes());
        dib[40..44].copy_from_slice(&0x0000_00FFu32.to_le_bytes());
        dib[44..48].copy_from_slice(&0x0000_FF00u32.to_le_bytes());
        dib[48..52].copy_from_slice(&0x00FF_0000u32.to_le_bytes());
        dib[52..56].copy_from_slice(&0xFF00_0000u32.to_le_bytes());
        dib.extend_from_slice(&[10, 20, 30, 255]);
        assert_eq!(frame_from_dib(&dib).unwrap().bgra(), &[30, 20, 10, 255]);
    }

    fn error_of(dib: &[u8]) -> String {
        frame_from_dib(dib).expect_err("must be refused").to_string()
    }

    #[test]
    fn every_truncation_of_a_valid_dib_is_an_error() {
        for dib in valid_dibs() {
            frame_from_dib(&dib).expect("the complete DIB is valid");
            for len in 0..dib.len() {
                assert!(frame_from_dib(&dib[..len]).is_err(), "{len} of {} bytes", dib.len());
            }
        }
    }

    #[test]
    fn dimensions_that_overflow_or_make_no_sense_are_errors() {
        for (w, h) in [
            (i32::MAX, i32::MAX),
            (i32::MAX, i32::MIN),
            (i32::MAX, 1),
            (1, i32::MAX),
            (1, i32::MIN),
            (1 << 20, 1 << 20),
            (1 << 16, 1 << 16),
        ] {
            for bits in [1u16, 4, 8, 16, 24, 32] {
                assert!(error_of(&header(w, h, bits, BI_RGB.0, 0)).contains("too large"), "{w}x{h} at {bits}");
            }
        }
        for (w, h) in [(0, 1), (1, 0), (0, 0), (-1, 1), (-1, -1), (i32::MIN, 1)] {
            assert!(error_of(&header(w, h, 32, BI_RGB.0, 0)).contains("empty dimensions"), "{w}x{h}");
        }
    }

    #[test]
    fn a_header_claiming_more_pixels_than_are_there_is_an_error() {
        for (bits, compression) in [(1u16, BI_RGB.0), (4, BI_RGB.0), (8, BI_RGB.0), (16, BI_RGB.0), (24, BI_RGB.0), (32, BI_RGB.0)] {
            let dib = header(100, 100, bits, compression, 0);
            assert!(frame_from_dib(&dib).is_err(), "{bits} bits");
        }
        let dib = with_masks(header(100, 100, 32, BI_BITFIELDS.0, 0), [0xFF, 0xFF00, 0xFF_0000]);
        assert!(error_of(&dib).contains("truncated"));
        let dib = with_masks(header(100, 100, 16, BI_BITFIELDS.0, 0), [0xF800, 0x07E0, 0x001F]);
        assert!(error_of(&dib).contains("truncated"));
    }

    #[test]
    fn every_other_compression_is_refused_before_anything_is_read() {
        // RLE8, RLE4, JPEG, PNG, the alpha bit fields of Windows CE, CMYK and nonsense.
        for compression in [1u32, 2, 4, 5, 6, 11, 12, 13, 0x7FFF_FFFF, u32::MAX] {
            for bits in [1u16, 4, 8, 16, 24, 32] {
                // Far more pixels than bytes: the old path handed these to GDI without a size check.
                let mut dib = header(4096, 4096, bits, compression, 0);
                dib.extend_from_slice(&[0xAB; 64]);
                assert!(error_of(&dib).contains("unsupported compression"), "compression {compression} at {bits}");
            }
        }
    }

    #[test]
    fn bit_fields_are_refused_at_depths_they_do_not_exist_for() {
        for bits in [1u16, 4, 8, 24] {
            let mut dib = with_masks(header(1, 1, bits, BI_BITFIELDS.0, 0), [0xFF, 0xFF00, 0xFF_0000]);
            dib.extend_from_slice(&[0; 16]);
            assert!(frame_from_dib(&dib).is_err(), "{bits} bits");
        }
        for bits in [0u16, 2, 7, 15, 31, 33, 64, u16::MAX] {
            let mut dib = header(1, 1, bits, BI_RGB.0, 0);
            dib.extend_from_slice(&[0; 64]);
            assert!(frame_from_dib(&dib).is_err(), "{bits} bits");
        }
    }

    #[test]
    fn colour_tables_that_cannot_be_true_are_errors() {
        // More colours than the depth has.
        assert!(error_of(&header(1, 1, 8, BI_RGB.0, 257)).contains("more colours"));
        assert!(error_of(&header(1, 1, 1, BI_RGB.0, 3)).contains("more colours"));
        // A count large enough to wrap a 32-bit size, at a depth where the table is optional.
        assert!(frame_from_dib(&header(1, 1, 24, BI_RGB.0, u32::MAX)).is_err());
        assert!(frame_from_dib(&header(1, 1, 32, BI_RGB.0, 0x4000_0000)).is_err());
        // A table that runs past the end.
        assert!(error_of(&header(1, 1, 8, BI_RGB.0, 2)).contains("colour table"));
        // Indexes beyond the table, inside the depth.
        let mut dib = header(1, 1, 8, BI_RGB.0, 1);
        dib.extend_from_slice(&[1, 2, 3, 0, 200, 0, 0, 0]);
        assert!(error_of(&dib).contains("not in its table"));
    }

    #[test]
    fn colour_masks_that_cannot_be_true_are_errors() {
        let check = |masks: [u32; 3]| {
            let mut dib = with_masks(header(1, 1, 32, BI_BITFIELDS.0, 0), masks);
            dib.extend_from_slice(&[0; 4]);
            error_of(&dib)
        };
        assert!(check([0, 0, 0]).contains("empty"));
        assert!(check([0b101, 0xFF00, 0xFF_0000]).contains("one run of bits"));
        // Masks that run past the end of the buffer.
        let mut short = header(1, 1, 32, BI_BITFIELDS.0, 0);
        short.extend_from_slice(&[0xFF, 0, 0, 0]);
        assert!(error_of(&short).contains("masks run past the end"));
    }

    #[test]
    fn broken_headers_are_errors() {
        assert!(frame_from_dib(&[]).is_err());
        assert!(frame_from_dib(&[1, 2, 3]).is_err());
        for size in [0u32, 12, 39, 41, 51, 53, 107, 125, 1 << 20, u32::MAX] {
            let mut dib = header(1, 1, 32, BI_RGB.0, 0);
            dib[0..4].copy_from_slice(&size.to_le_bytes());
            dib.extend_from_slice(&[0; 256]);
            assert!(error_of(&dib).contains("unknown header size"), "header of {size} bytes");
        }
        // A real header size, longer than the buffer.
        let mut short = header(1, 1, 32, BI_RGB.0, 0);
        short[0..4].copy_from_slice(&124u32.to_le_bytes());
        assert!(error_of(&short).contains("too short"));
        let mut planes = header(1, 1, 32, BI_RGB.0, 0);
        planes[12..14].copy_from_slice(&2u16.to_le_bytes());
        planes.extend_from_slice(&[0; 4]);
        assert!(error_of(&planes).contains("plane"));
    }

    /// The decoder takes bytes from any application: garbage must come back as an error or a frame of exactly the
    /// size it claims, and never a panic. Deterministic, so a failure can be replayed.
    #[test]
    fn random_and_mutated_input_never_panics() {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let check = |input: &[u8]| {
            if let Ok(frame) = frame_from_dib(input) {
                assert_eq!(frame.bgra().len() as u64, u64::from(frame.width()) * u64::from(frame.height()) * 4);
            }
        };
        for _ in 0..20_000 {
            let len = (next() % 200) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            check(&bytes);
        }
        let seeds = valid_dibs();
        for _ in 0..60_000 {
            let mut dib = seeds[(next() % seeds.len() as u64) as usize].clone();
            for _ in 0..=(next() % 4) {
                // Mostly the header, where the sizes live.
                let at = (next() % dib.len().min(60) as u64) as usize;
                dib[at] = next() as u8;
            }
            if next() % 3 == 0 {
                dib.truncate((next() % (dib.len() as u64 + 1)) as usize);
            }
            check(&dib);
        }
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
