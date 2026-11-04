//! One frame of a window through Windows.Graphics.Capture: a session opened for a single frame on the shared
//! device, and closed right after.

use std::sync::mpsc;
use std::time::Duration;

use shotik_core::error::{Error, Result};
use shotik_core::frame::Frame;
use shotik_core::geom::Rect;
use windows::core::{factory, Interface};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use super::d3d::shared_device;
use crate::displays::win_err;

/// Closes everything on every path, so an early return cannot leave a session running.
struct Session {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: i64,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// The window's current content, without the cursor. On Windows too old to turn off the capture border, the
/// window shows a yellow frame for this one moment, which beats failing: this is a second attempt already.
pub(crate) fn capture_window(hwnd: HWND, timeout: Duration) -> Result<Frame> {
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .map_err(|e| win_err("Windows.Graphics.Capture is not available", e))?;
    // SAFETY: a stale or invalid handle makes the call fail, it does not crash.
    let item: GraphicsCaptureItem =
        unsafe { interop.CreateForWindow(hwnd) }.map_err(|e| win_err("cannot capture this window", e))?;
    let size = item.Size().map_err(|e| win_err("cannot read the size of the window", e))?;
    if size.Width <= 0 || size.Height <= 0 {
        return Err(Error::msg(format!("window has empty bounds ({} x {})", size.Width, size.Height)));
    }
    let device = shared_device(None)?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        device.winrt()?,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        1,
        size,
    )
    .map_err(|e| win_err("cannot create a capture frame pool", e))?;
    let session = pool.CreateCaptureSession(&item).map_err(|e| win_err("cannot create a capture session", e))?;
    session.SetIsCursorCaptureEnabled(false).map_err(|e| win_err("cannot exclude the cursor from the capture", e))?;
    let _ = session.SetIsBorderRequired(false);
    let (arrived, wait) = mpsc::channel::<()>();
    let token = pool
        .FrameArrived(&TypedEventHandler::new(move |_, _| {
            // The receiver is gone once the frame was taken or the wait timed out.
            let _ = arrived.send(());
            Ok(())
        }))
        .map_err(|e| win_err("cannot listen for capture frames", e))?;
    let session = Session { pool, session, token };
    session.session.StartCapture().map_err(|e| win_err("cannot start the capture", e))?;
    wait.recv_timeout(timeout)
        .map_err(|_| Error::msg(format!("no frame of the window within {} ms", timeout.as_millis())))?;
    let frame = session.pool.TryGetNextFrame().map_err(|e| win_err("cannot take the captured frame", e))?;
    let content = frame.ContentSize().map_err(|e| win_err("cannot read the size of the captured frame", e))?;
    let surface = frame.Surface().map_err(|e| win_err("cannot read the captured surface", e))?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| win_err("IDirect3DDxgiInterfaceAccess", e))?;
    // SAFETY: the surface wraps a texture of the shared device, alive as long as `frame`.
    let texture: ID3D11Texture2D =
        unsafe { access.GetInterface() }.map_err(|e| win_err("cannot reach the captured texture", e))?;
    let pixels = device.read_texture(&texture);
    drop(texture);
    drop(access);
    let _ = surface.Close();
    let _ = frame.Close();
    let pixels = pixels?;
    // The texture keeps the size the pool was made with. A window that shrank at this very moment fills only
    // part of it, and the rest is undefined.
    if content.Width < pixels.width() as i32 || content.Height < pixels.height() as i32 {
        return pixels.crop(Rect::new(0, 0, content.Width, content.Height));
    }
    Ok(pixels)
}
