//! Windows Graphics Capture of one monitor. The newest frame waits in a slot and older ones are
//! returned to the pool at once, so the pool never runs dry and a frame is at most one composition
//! old when the video thread takes it.
//!
//! The calls that reach a system broker leave ALPC ports behind, irregularly, about 0.2 to 0.45 per
//! call (`IsSupported`, `RequestAccessAsync`, `CreateForMonitor`), which came to two ports per recording.
//! Capturing again on a kept item and device leaves nothing, so what can be kept is: the answers once
//! per process, and the item for as long as the monitor exists.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use shotik_core::{Error, Rect, Result};
use windows::core::{factory, h, Interface, Ref};
use windows::Foundation::Metadata::ApiInformation;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureAccessKind, GraphicsCaptureItem,
    GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_BOX};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use super::com::Context;
use super::d3d::D3d;
use super::source::Grabber;
use crate::clock::TICKS_PER_SEC;
use crate::region::{crop_box, CropBox};
use crate::trace::Trace;

const POOL_BUFFERS: i32 = 3;
const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const SESSION_CLASS: &windows::core::HSTRING = h!("Windows.Graphics.Capture.GraphicsCaptureSession");

/// The frame waiting for the video thread, shared with the capture callback.
type Slot = Arc<Mutex<Option<Direct3D11CaptureFrame>>>;

/// What this Windows can do, asked once.
#[derive(Clone, Debug)]
struct Capabilities {
    supported: bool,
    border_setting: bool,
    update_interval_setting: bool,
    /// Why borderless capture was not granted, when it was not.
    access_problem: Option<String>,
}

fn capabilities() -> &'static Capabilities {
    static ANSWER: OnceLock<Capabilities> = OnceLock::new();
    ANSWER.get_or_init(|| {
        let supported = GraphicsCaptureSession::IsSupported().unwrap_or(false);
        let border_setting = ApiInformation::IsPropertyPresent(SESSION_CLASS, h!("IsBorderRequired")).unwrap_or(false);
        let update_interval_setting = ApiInformation::IsPropertyPresent(SESSION_CLASS, h!("MinUpdateInterval")).unwrap_or(false);
        // Unpackaged applications are allowed without a prompt.
        let access_problem = border_setting
            .then(|| GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless).and_then(|op| op.join()).err())
            .flatten()
            .map(|e| e.to_string());
        Capabilities { supported, border_setting, update_interval_setting, access_problem }
    })
}

/// A capture item stays usable until its monitor goes away, which `closed` says.
struct KeptItem {
    monitor: isize,
    item: GraphicsCaptureItem,
    closed: Arc<AtomicBool>,
}

static ITEMS: Mutex<Vec<KeptItem>> = Mutex::new(Vec::new());

fn item_for(monitor: HMONITOR) -> Result<(GraphicsCaptureItem, Arc<AtomicBool>)> {
    let key = monitor.0 as isize;
    let mut items = ITEMS.lock().unwrap();
    items.retain(|kept| !kept.closed.load(Ordering::SeqCst));
    if let Some(kept) = items.iter().find(|kept| kept.monitor == key) {
        return Ok((kept.item.clone(), Arc::clone(&kept.closed)));
    }
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().ctx("IGraphicsCaptureItemInterop")?;
    // SAFETY: the monitor handle is checked by the call; it returns an error for a stale one.
    let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }.ctx("Cannot capture this display")?;
    let closed = Arc::new(AtomicBool::new(false));
    // the handler stays for as long as the item: it is what tells the item is gone
    item.Closed(&TypedEventHandler::new({
        let closed = Arc::clone(&closed);
        move |_, _| {
            closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }))
    .ctx("GraphicsCaptureItem::Closed")?;
    items.push(KeptItem { monitor: key, item: item.clone(), closed: Arc::clone(&closed) });
    Ok((item, closed))
}

pub(crate) struct WgcGrabber {
    device: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    slot: Slot,
    arrived: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    frame_token: i64,
    crop: CropBox,
    pool_size: SizeInt32,
    staging: Option<ID3D11Texture2D>,
}

// SAFETY: see the module comment of `win`; the WinRT classes here are agile and the session hands the
// grabber from thread to thread, never shares it.
unsafe impl Send for WgcGrabber {}

/// The optional parts of the session, by what this Windows version has. Any failure is reported: a
/// yellow border on the user's screen must have a visible reason.
fn configure_session(session: &GraphicsCaptureSession, fps: u32, trace: &Trace) -> windows::core::Result<()> {
    let can = capabilities();
    session.SetIsCursorCaptureEnabled(true)?;
    if let Some(problem) = &can.access_problem {
        trace.push(format!("borderless access: {problem}"));
    }
    if can.border_setting {
        if let Err(e) = session.SetIsBorderRequired(false) {
            trace.push(format!("capture border stays on: {e}"));
        }
    } else {
        trace.push("capture border stays on: this Windows has no IsBorderRequired");
    }
    if can.update_interval_setting {
        // Twice the output rate is enough to always have a fresh frame; it spares a 240 Hz display.
        let interval = TimeSpan { Duration: TICKS_PER_SEC / i64::from(fps.max(1) * 2) };
        if let Err(e) = session.SetMinUpdateInterval(interval) {
            trace.push(format!("min update interval not set: {e}"));
        }
    }
    Ok(())
}

impl WgcGrabber {
    pub fn start(d3d: &D3d, monitor: HMONITOR, rect_phys: Rect, fps: u32, trace: &Trace) -> Result<Self> {
        if !capabilities().supported {
            return Err(Error::msg("Windows Graphics Capture is not supported on this system"));
        }
        let (item, closed) = item_for(monitor)?;
        let pool_size = item.Size().ctx("GraphicsCaptureItem::Size")?;
        let crop = crop_box(rect_phys, pool_size.Width.max(0) as u32, pool_size.Height.max(0) as u32)?;

        let device = d3d.capture_device()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&device, FORMAT, POOL_BUFFERS, pool_size)
            .ctx("Direct3D11CaptureFramePool")?;
        let session = pool.CreateCaptureSession(&item).ctx("CreateCaptureSession")?;
        configure_session(&session, fps, trace).ctx("Capture session settings")?;

        let slot: Slot = Arc::default();
        let arrived = Arc::new(AtomicBool::new(false));
        let frame_token = pool
            .FrameArrived(&TypedEventHandler::new({
                let slot = Arc::clone(&slot);
                let arrived = Arc::clone(&arrived);
                move |pool: Ref<Direct3D11CaptureFramePool>, _| {
                    let Some(pool) = pool.as_ref() else { return Ok(()) };
                    while let Ok(frame) = pool.TryGetNextFrame() {
                        if let Some(older) = slot.lock().unwrap().replace(frame) {
                            let _ = older.Close();
                        }
                        arrived.store(true, Ordering::SeqCst);
                    }
                    Ok(())
                }
            }))
            .ctx("FrameArrived")?;

        let grabber = Self { device, pool, session, slot, arrived, closed, frame_token, crop, pool_size, staging: None };
        grabber.session.StartCapture().ctx("Cannot start screen capture")?;
        trace.push(format!(
            "capture {}x{} crop {},{} {}x{}",
            pool_size.Width, pool_size.Height, crop.x, crop.y, crop.width, crop.height
        ));
        Ok(grabber)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.crop.width, self.crop.height)
    }

    fn take_frame(&self) -> Option<Direct3D11CaptureFrame> {
        self.slot.lock().unwrap().take()
    }

    /// The source box inside the frame. A display that shrank leaves less of the region.
    fn source_box(&mut self, frame: &Direct3D11CaptureFrame) -> Result<D3D11_BOX> {
        let content = frame.ContentSize().ctx("ContentSize")?;
        if content.Width != self.pool_size.Width || content.Height != self.pool_size.Height {
            self.pool.Recreate(&self.device, FORMAT, POOL_BUFFERS, content).ctx("Recreate")?;
            self.pool_size = content;
        }
        Ok(D3D11_BOX {
            left: self.crop.x,
            top: self.crop.y,
            front: 0,
            right: (self.crop.x + self.crop.width).min(content.Width.max(0) as u32),
            bottom: (self.crop.y + self.crop.height).min(content.Height.max(0) as u32),
            back: 1,
        })
    }
}

fn texture_of(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D> {
    let surface = frame.Surface().ctx("Frame surface")?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast().ctx("IDirect3DDxgiInterfaceAccess")?;
    // SAFETY: a query on a live object; it returns its own reference.
    unsafe { access.GetInterface() }.ctx("Frame texture")
}

impl Grabber for WgcGrabber {
    fn ready(&self) -> bool {
        self.arrived.load(Ordering::SeqCst)
    }

    fn refresh_gpu(&mut self, d3d: &D3d, latest: &ID3D11Texture2D) -> Result<bool> {
        let Some(frame) = self.take_frame() else { return Ok(false) };
        let source = self.source_box(&frame)?;
        let texture = texture_of(&frame)?;
        if source.right > source.left && source.bottom > source.top {
            // SAFETY: both textures are BGRA and live; the box lies inside the frame by source_box and
            // fits `latest`, which has the size of the crop.
            unsafe { d3d.context.CopySubresourceRegion(latest, 0, 0, 0, 0, &texture, 0, Some(&source)) };
        }
        frame.Close().ctx("Frame close")?;
        Ok(true)
    }

    fn refresh_cpu(&mut self, d3d: Option<&D3d>, bgra: &mut [u8], stride: usize) -> Result<bool> {
        let Some(d3d) = d3d else {
            return Err(Error::msg("internal error: capture without a device"));
        };
        let Some(frame) = self.take_frame() else { return Ok(false) };
        let source = self.source_box(&frame)?;
        let texture = texture_of(&frame)?;
        let (w, h) = self.size();
        if self.staging.is_none() {
            self.staging = Some(d3d.staging_bgra(w, h)?);
        }
        let Some(staging) = &self.staging else { return Ok(false) };
        if source.right > source.left && source.bottom > source.top {
            // SAFETY: as in refresh_gpu.
            unsafe { d3d.context.CopySubresourceRegion(staging, 0, 0, 0, 0, &texture, 0, Some(&source)) };
        }
        frame.Close().ctx("Frame close")?;
        d3d.read_staging(staging, w, h, bgra, stride)?;
        Ok(true)
    }

    fn lost(&self) -> Option<String> {
        self.closed.load(Ordering::SeqCst).then(|| "The recorded display is gone".to_string())
    }
}

impl Drop for WgcGrabber {
    /// Everything the session holds is released here on purpose instead of waiting for the last
    /// reference: `session.Close` is what stops capture and frees the shared surfaces.
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.frame_token);
        let _ = self.session.Close();
        if let Some(frame) = self.take_frame() {
            let _ = frame.Close();
        }
        let _ = self.pool.Close();
    }
}
