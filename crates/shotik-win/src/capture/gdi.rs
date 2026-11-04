//! The GDI pieces of capture: a memory canvas, and a blit from the screen into one.

use std::ffi::c_void;

use shotik_core::error::{Error, Result};
use shotik_core::frame::Frame;
use shotik_core::geom::Rect;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};

use super::d3d::force_opaque;
use crate::displays::win_err;

/// A memory DC with a top-down 32-bit DIB selected into it. Everything is released on drop, so no early
/// return can leak a GDI object. Neither Send nor Sync: a DC belongs to the thread using it.
pub(crate) struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut c_void,
    width: usize,
    height: usize,
}

impl Canvas {
    pub(crate) fn new(width: i32, height: i32) -> Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(Error::msg(format!("cannot capture an empty area of {width}x{height}")));
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: the DC is ours and deleted on the failure path below or in drop. `bits` receives memory
        // owned by the bitmap, valid until it is deleted.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(Error::msg("cannot create a memory device context"));
            }
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) if !bits.is_null() => bitmap,
                Ok(bitmap) => {
                    let _ = DeleteObject(bitmap.into());
                    let _ = DeleteDC(dc);
                    return Err(Error::msg(format!("no memory for a {width}x{height} bitmap")));
                }
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(win_err(&format!("cannot allocate a {width}x{height} bitmap"), e));
                }
            };
            let previous = SelectObject(dc, bitmap.into());
            Ok(Self { dc, bitmap, previous, bits, width: width as usize, height: height as usize })
        }
    }

    pub(crate) fn dc(&self) -> HDC {
        self.dc
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// The pixels as GDI left them: BGRA, top row first, alpha undefined. Flushes pending drawing first.
    pub(crate) fn pixels(&self) -> &[u8] {
        // SAFETY: flushing has no preconditions. The bitmap has exactly `width * height` 32-bit pixels and
        // lives as long as `self`, and nothing writes to it while this borrow exists on this thread.
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts(self.bits.cast::<u8>(), self.width * self.height * 4)
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: created in `new`, released once, the bitmap deselected before it is deleted.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The screen's device context, for the whole virtual desktop.
struct ScreenDc(HDC);

impl ScreenDc {
    fn get() -> Result<Self> {
        // SAFETY: a null window means the screen, released in drop.
        let dc = unsafe { GetDC(None) };
        if dc.is_invalid() {
            return Err(Error::msg("cannot get the screen's device context"));
        }
        Ok(Self(dc))
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: obtained in `get`, released once.
        unsafe { ReleaseDC(None, self.0) };
    }
}

/// Copies a physical rectangle of the screen to a frame. `CAPTUREBLT` is what brings layered windows
/// into the picture: toasts and similar, which the user sees and expects in a screenshot. The cursor is
/// not part of it, as in Electron's capture.
pub(crate) fn grab(rect: Rect) -> Result<Frame> {
    let canvas = Canvas::new(rect.width, rect.height)?;
    let screen = ScreenDc::get()?;
    // SAFETY: both DCs are valid and the destination is exactly the size of the copied area.
    unsafe {
        BitBlt(canvas.dc(), 0, 0, rect.width, rect.height, Some(screen.0), rect.x, rect.y, SRCCOPY | CAPTUREBLT)
    }
    .map_err(|e| win_err("cannot copy from the screen", e))?;
    drop(screen);
    let mut pixels = canvas.pixels().to_vec();
    drop(canvas);
    force_opaque(&mut pixels);
    Frame::new(rect.width as u32, rect.height as u32, pixels)
}
