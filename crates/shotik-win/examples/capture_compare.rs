//! Compares ways to capture a monitor on this machine, the evidence behind `capture.rs`.
//!
//! Pixels stay in memory and are dropped after a few statistics: nothing is written and only numbers
//! are printed. No window is created and no input is sent. Build it in release:
//!
//! ```text
//! cargo build --release -p shotik-win --example capture_compare --features tools
//! capture_compare info                       monitors, adapters, HDR, rotation, what is in front
//! capture_compare latency [warm] [cold]      latency and correctness per method and monitor
//! capture_compare stability <method> [grabs] [gap_ms]   handles and private bytes across many grabs
//! capture_compare footprint                  what a held device costs
//! capture_compare hold <method> <monitor> <secs>   a session kept open: the idle cost of that idea
//! capture_compare otherdesktop               from a thread on a hidden desktop: the secure-desktop stand-in
//! capture_compare stability-otherdesktop <method> [grabs]   the failure path of a method, for leaks
//! capture_compare window-stability <printwindow|wgc> [grabs] [gap_ms]   a window in another process
//! capture_compare leak device <default|adapter|flush|wait|nobgra|warp> [times] [gap_ms]   what a device leaks
//! capture_compare leak wgc <item|pool|session|start|frame> [times] [gap_ms]   which step of a session leaks
//! capture_compare leak dxgi <factory|adapter|output|desc|device-on-adapter|duplicate> [times] [gap_ms]
//! capture_compare leak <wgc-cached|wgc-window> <item|pool|session|start|frame> [times] [gap_ms]
//! capture_compare leak wgc-warp <monitor|window> [times] [gap_ms]
//! capture_compare leak wgc-thread <sta|mta> [times] [gap_ms]
//! capture_compare list-stability [times] [gap_ms]   the window list
//! ```
//! Methods: port (what `WinCapture` does), gdi, gdi-nocaptureblt, gdi-ddb, gdi-strips, dxgi, dxgi-fresh, wgc,
//! wgc-fresh.
//! `-fresh` creates the device per grab.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use shotik_core::geom::Rect;
use shotik_core::ports::Displays;
use shotik_core::ports::{WindowCaptureMethod, WindowSource};
use shotik_win::capture::WinCapture;
use shotik_win::displays::{set_dpi_awareness, WinDisplays};
use shotik_win::window_source::test_support::{Options, TestWindow};
use shotik_win::window_source::WinWindowSource;
use windows::core::{factory, Interface};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutput6,
    IDXGIResource, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC,
    GetDIBits, MonitorFromPoint, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HMONITOR, MONITOR_DEFAULTTONULL,
    ROP_CODE, SRCCOPY,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::StationsAndDesktops::{CloseDesktop, CreateDesktopW, SetThreadDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetGuiResources, GetProcessHandleCount, GR_GDIOBJECTS, GR_USEROBJECTS,
};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
};

type Res<T> = std::result::Result<T, String>;

fn hr(stage: &str, e: windows::core::Error) -> String {
    format!("{stage}:{:#010x}", e.code().0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Method {
    Gdi,
    GdiNoCaptureBlt,
    Dxgi,
    DxgiFresh,
    Wgc,
    WgcFresh,
    GdiDdb,
    GdiStrips,
    Port,
}

impl Method {
    const ALL: [Method; 9] = [
        Method::Port,
        Method::Gdi,
        Method::GdiNoCaptureBlt,
        Method::GdiDdb,
        Method::GdiStrips,
        Method::Dxgi,
        Method::DxgiFresh,
        Method::Wgc,
        Method::WgcFresh,
    ];

    fn name(self) -> &'static str {
        match self {
            Method::Gdi => "gdi",
            Method::GdiNoCaptureBlt => "gdi-nocaptureblt",
            Method::Dxgi => "dxgi",
            Method::DxgiFresh => "dxgi-fresh",
            Method::Wgc => "wgc",
            Method::WgcFresh => "wgc-fresh",
            Method::GdiDdb => "gdi-ddb",
            Method::GdiStrips => "gdi-strips",
            Method::Port => "port",
        }
    }

    fn parse(s: &str) -> Method {
        Method::ALL.into_iter().find(|m| m.name() == s).unwrap_or_else(|| panic!("unknown method {s}"))
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct Stages {
    device: f64,
    setup: f64,
    frame: f64,
    gpu_copy: f64,
    cpu_copy: f64,
    teardown: f64,
}

#[derive(Clone, Copy, Default, Debug)]
struct DupInfo {
    accumulated: u32,
    present_time_zero: bool,
}

struct Grabbed {
    bgra: Vec<u8>,
    width: u32,
    height: u32,
    stages: Stages,
    dup: Option<DupInfo>,
}

impl Grabbed {
    /// Up to the moment the pixels are usable. Teardown is reported on its own.
    fn latency(&self) -> f64 {
        let s = self.stages;
        s.device + s.setup + s.frame + s.gpu_copy + s.cpu_copy
    }
}

#[derive(Clone)]
struct D3d {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
}

fn create_device(adapter: &IDXGIAdapter1) -> Res<D3d> {
    let (mut device, mut context) = (None, None);
    // SAFETY: the out-parameters are valid for the call and the adapter is alive.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| hr("D3D11CreateDevice", e))?;
    Ok(D3d { device: device.ok_or("no device")?, context: context.ok_or("no context")? })
}

#[derive(Clone)]
struct Monitor {
    index: usize,
    rect: Rect,
    hmon: HMONITOR,
}

struct Output {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput1,
}

fn find_output(hmon: HMONITOR) -> Res<Output> {
    // SAFETY: creates a COM object with no input.
    let dxgi: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| hr("CreateDXGIFactory1", e))?;
    let mut a = 0;
    // SAFETY: EnumAdapters1 and EnumOutputs return errors past the last index, which ends the loops.
    while let Ok(adapter) = unsafe { dxgi.EnumAdapters1(a) } {
        let mut o = 0;
        while let Ok(output) = unsafe { adapter.EnumOutputs(o) } {
            let desc = unsafe { output.GetDesc() }.map_err(|e| hr("GetDesc", e))?;
            if desc.Monitor == hmon {
                let output: IDXGIOutput1 = output.cast().map_err(|e| hr("IDXGIOutput1", e))?;
                return Ok(Output { adapter, output });
            }
            o += 1;
        }
        a += 1;
    }
    Err("no DXGI output for this monitor".into())
}

/// Copies a texture to a new top-down BGRA buffer through a staging texture.
fn read_texture(d3d: &D3d, texture: &ID3D11Texture2D, stages: &mut Stages) -> Res<(Vec<u8>, u32, u32)> {
    let t = Instant::now();
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `desc` is a writable out-parameter on a live texture.
    unsafe { texture.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut staging: Option<ID3D11Texture2D> = None;
    // SAFETY: `desc` describes a valid staging texture; the out-parameter is valid.
    unsafe { d3d.device.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(|e| hr("CreateTexture2D", e))?;
    let staging = staging.ok_or("no staging texture")?;
    // SAFETY: same size and format, from the same device.
    unsafe { d3d.context.CopyResource(&staging, texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: the staging texture has CPU read access; Map waits for the copy to finish.
    unsafe { d3d.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(|e| hr("Map", e))?;
    stages.gpu_copy = ms(t.elapsed());

    let t = Instant::now();
    let (w, h) = (desc.Width as usize, desc.Height as usize);
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        // SAFETY: mapped.pData points at `RowPitch * h` readable bytes, and each row has at least w * 4.
        let row = unsafe { std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), w * 4) };
        out[y * w * 4..(y + 1) * w * 4].copy_from_slice(row);
    }
    // SAFETY: mapped above and not used after this.
    unsafe { d3d.context.Unmap(&staging, 0) };
    force_opaque(&mut out);
    stages.cpu_copy = ms(t.elapsed());
    Ok((out, desc.Width, desc.Height))
}

fn force_opaque(bgra: &mut [u8]) {
    for px in bgra.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

fn grab_gdi(rect: Rect, rop: ROP_CODE) -> Res<Grabbed> {
    let mut stages = Stages::default();
    let t = Instant::now();
    let (w, h) = (rect.width, rect.height);
    // SAFETY: every handle is released below on all paths; the bits pointer lives until the bitmap is deleted.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("GetDC".into());
        }
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = match CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(dib) => dib,
            Err(e) => {
                let _ = DeleteDC(mem);
                ReleaseDC(None, screen);
                return Err(hr("CreateDIBSection", e));
            }
        };
        let old = SelectObject(mem, dib.into());
        stages.setup = ms(t.elapsed());
        let t = Instant::now();
        let blit = BitBlt(mem, 0, 0, w, h, Some(screen), rect.x, rect.y, rop);
        let _ = GdiFlush();
        stages.frame = ms(t.elapsed());
        let result = match blit {
            Ok(()) => {
                let t = Instant::now();
                let mut out = vec![0u8; w as usize * h as usize * 4];
                std::ptr::copy_nonoverlapping(bits as *const u8, out.as_mut_ptr(), out.len());
                force_opaque(&mut out);
                stages.cpu_copy = ms(t.elapsed());
                Ok(out)
            }
            Err(e) => Err(hr("BitBlt", e)),
        };
        let t = Instant::now();
        SelectObject(mem, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        stages.teardown = ms(t.elapsed());
        result.map(|bgra| Grabbed { bgra, width: w as u32, height: h as u32, stages, dup: None })
    }
}

/// Through a device-dependent bitmap: the blit may land in memory the driver reads faster, then
/// `GetDIBits` converts.
fn grab_gdi_ddb(rect: Rect) -> Res<Grabbed> {
    let mut stages = Stages::default();
    let t = Instant::now();
    let (w, h) = (rect.width, rect.height);
    // SAFETY: every handle is released below on all paths; the destination buffer has the size `info` says.
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bitmap.into());
        stages.setup = ms(t.elapsed());
        let t = Instant::now();
        let blit = BitBlt(mem, 0, 0, w, h, Some(screen), rect.x, rect.y, SRCCOPY | CAPTUREBLT);
        stages.frame = ms(t.elapsed());
        let t = Instant::now();
        SelectObject(mem, old);
        let mut out = vec![0u8; w as usize * h as usize * 4];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = GetDIBits(mem, bitmap, 0, h as u32, Some(out.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        force_opaque(&mut out);
        stages.cpu_copy = ms(t.elapsed());
        let t = Instant::now();
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        stages.teardown = ms(t.elapsed());
        blit.map_err(|e| hr("BitBlt", e))?;
        if lines != h {
            return Err(format!("GetDIBits returned {lines} lines"));
        }
        Ok(Grabbed { bgra: out, width: w as u32, height: h as u32, stages, dup: None })
    }
}

/// The monitor in horizontal strips, one thread each: does anything below GDI blit in parallel?
fn grab_gdi_strips(rect: Rect, strips: i32) -> Res<Grabbed> {
    let mut stages = Stages::default();
    let t = Instant::now();
    let (w, h) = (rect.width as usize, rect.height as usize);
    let mut out = vec![0u8; w * h * 4];
    let row_bytes = w * 4;
    let rows_each = h.div_ceil(strips as usize);
    let result: Res<()> = std::thread::scope(|scope| {
        let mut joins = Vec::new();
        for (i, chunk) in out.chunks_mut(rows_each * row_bytes).enumerate() {
            let y = rect.y + (i * rows_each) as i32;
            let part = Rect::new(rect.x, y, rect.width, (chunk.len() / row_bytes) as i32);
            joins.push(scope.spawn(move || -> Res<()> {
                let _dpi = PerThreadDpi::enter();
                let g = grab_gdi(part, SRCCOPY | CAPTUREBLT)?;
                chunk.copy_from_slice(&g.bgra);
                Ok(())
            }));
        }
        for j in joins {
            j.join().map_err(|_| "strip panicked".to_string())??;
        }
        Ok(())
    });
    result?;
    stages.frame = ms(t.elapsed());
    Ok(Grabbed { bgra: out, width: rect.width as u32, height: rect.height as u32, stages, dup: None })
}

/// Threads inherit the process awareness here, which `main` set, so this is only documentation of intent.
struct PerThreadDpi;

impl PerThreadDpi {
    fn enter() -> Self {
        Self
    }
}

fn grab_dxgi(d3d: &D3d, output: &IDXGIOutput1, mut stages: Stages) -> Res<Grabbed> {
    let t = Instant::now();
    // SAFETY: both COM objects are alive; the duplication is released when dropped.
    let dup = unsafe { output.DuplicateOutput(&d3d.device) }.map_err(|e| hr("DuplicateOutput", e))?;
    stages.setup = ms(t.elapsed());

    let t = Instant::now();
    let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
    let mut resource: Option<IDXGIResource> = None;
    // SAFETY: the out-parameters are valid for the call.
    unsafe { dup.AcquireNextFrame(1000, &mut info, &mut resource) }.map_err(|e| hr("AcquireNextFrame", e))?;
    stages.frame = ms(t.elapsed());
    let dup_info = DupInfo { accumulated: info.AccumulatedFrames, present_time_zero: info.LastPresentTime == 0 };

    let read = (|| {
        let texture: ID3D11Texture2D =
            resource.as_ref().ok_or("no frame resource")?.cast().map_err(|e| hr("ID3D11Texture2D", e))?;
        read_texture(d3d, &texture, &mut stages)
    })();
    let t = Instant::now();
    drop(resource);
    // SAFETY: a frame was acquired above and is released once, whether or not reading it worked.
    let _ = unsafe { dup.ReleaseFrame() };
    drop(dup);
    stages.teardown = ms(t.elapsed());
    let (bgra, width, height) = read?;
    Ok(Grabbed { bgra, width, height, stages, dup: Some(dup_info) })
}

fn winrt_device(d3d: &D3d) -> Res<IDirect3DDevice> {
    let dxgi: IDXGIDevice = d3d.device.cast().map_err(|e| hr("IDXGIDevice", e))?;
    // SAFETY: takes a live DXGI device.
    let inspectable =
        unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }.map_err(|e| hr("CreateDirect3D11Device", e))?;
    inspectable.cast().map_err(|e| hr("IDirect3DDevice", e))
}

fn grab_wgc(d3d: &D3d, device: &IDirect3DDevice, hmon: HMONITOR, mut stages: Stages) -> Res<Grabbed> {
    let t = Instant::now();
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(|e| hr("interop", e))?;
    // SAFETY: the handle comes from MonitorFromPoint for a monitor that exists.
    let item: GraphicsCaptureItem =
        unsafe { interop.CreateForMonitor(hmon) }.map_err(|e| hr("CreateForMonitor", e))?;
    let size = item.Size().map_err(|e| hr("Size", e))?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 1, size)
        .map_err(|e| hr("CreateFreeThreaded", e))?;
    let session = pool.CreateCaptureSession(&item).map_err(|e| hr("CreateCaptureSession", e))?;
    session.SetIsCursorCaptureEnabled(false).map_err(|e| hr("SetIsCursorCaptureEnabled", e))?;
    session.SetIsBorderRequired(false).map_err(|e| hr("SetIsBorderRequired", e))?;
    if session.IsBorderRequired().map_err(|e| hr("IsBorderRequired", e))? {
        return Err("the capture border cannot be turned off, not starting".into());
    }
    let (tx, rx) = mpsc::channel::<()>();
    let token = pool
        .FrameArrived(&TypedEventHandler::new(move |_, _| {
            let _ = tx.send(());
            Ok(())
        }))
        .map_err(|e| hr("FrameArrived", e))?;
    session.StartCapture().map_err(|e| hr("StartCapture", e))?;
    stages.setup = ms(t.elapsed());

    let t = Instant::now();
    let arrived = rx.recv_timeout(Duration::from_millis(1000));
    stages.frame = ms(t.elapsed());
    let read = (|| {
        arrived.map_err(|_| "no frame within 1000 ms".to_string())?;
        let frame = pool.TryGetNextFrame().map_err(|e| hr("TryGetNextFrame", e))?;
        let surface = frame.Surface().map_err(|e| hr("Surface", e))?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| hr("access", e))?;
        // SAFETY: the surface wraps a D3D11 texture of the same device.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(|e| hr("GetInterface", e))?;
        let read = read_texture(d3d, &texture, &mut stages);
        drop(texture);
        let _ = frame.Close();
        read
    })();
    let t = Instant::now();
    let _ = pool.RemoveFrameArrived(token);
    let _ = session.Close();
    let _ = pool.Close();
    stages.teardown = ms(t.elapsed());
    let (bgra, width, height) = read?;
    Ok(Grabbed { bgra, width, height, stages, dup: None })
}

/// What the methods with a retained device keep between grabs.
#[derive(Default)]
struct Held {
    d3d: Option<D3d>,
    winrt: Option<IDirect3DDevice>,
    outputs: BTreeMap<usize, IDXGIOutput1>,
    adapter: Option<IDXGIAdapter1>,
}

fn grab(method: Method, mon: &Monitor, held: &mut Held) -> Res<Grabbed> {
    match method {
        Method::Gdi => grab_gdi(mon.rect, SRCCOPY | CAPTUREBLT),
        Method::GdiNoCaptureBlt => grab_gdi(mon.rect, SRCCOPY),
        Method::GdiDdb => grab_gdi_ddb(mon.rect),
        Method::GdiStrips => grab_gdi_strips(mon.rect, 4),
        Method::Port => {
            let display = WinDisplays::new().all().into_iter().nth(mon.index).ok_or("monitor gone")?;
            let mut stages = Stages::default();
            let t = Instant::now();
            let (frame, _) = WinCapture::new().grab_with_method(&display).map_err(|e| e.to_string())?;
            stages.frame = ms(t.elapsed());
            let (width, height) = (frame.width(), frame.height());
            Ok(Grabbed { bgra: frame.bgra().to_vec(), width, height, stages, dup: None })
        }
        Method::Dxgi => {
            let mut stages = Stages::default();
            let t = Instant::now();
            if !held.outputs.contains_key(&mon.index) {
                let found = find_output(mon.hmon)?;
                if held.d3d.is_none() {
                    held.d3d = Some(create_device(&found.adapter)?);
                    held.adapter = Some(found.adapter);
                }
                held.outputs.insert(mon.index, found.output);
            }
            stages.device = ms(t.elapsed());
            grab_dxgi(held.d3d.as_ref().unwrap(), &held.outputs[&mon.index], stages)
        }
        Method::DxgiFresh => {
            let mut stages = Stages::default();
            let t = Instant::now();
            let found = find_output(mon.hmon)?;
            let d3d = create_device(&found.adapter)?;
            stages.device = ms(t.elapsed());
            grab_dxgi(&d3d, &found.output, stages)
        }
        Method::Wgc => {
            let mut stages = Stages::default();
            let t = Instant::now();
            if held.d3d.is_none() {
                let found = find_output(mon.hmon)?;
                let d3d = create_device(&found.adapter)?;
                held.winrt = Some(winrt_device(&d3d)?);
                held.d3d = Some(d3d);
            }
            stages.device = ms(t.elapsed());
            grab_wgc(held.d3d.as_ref().unwrap(), held.winrt.as_ref().unwrap(), mon.hmon, stages)
        }
        Method::WgcFresh => {
            let mut stages = Stages::default();
            let t = Instant::now();
            let found = find_output(mon.hmon)?;
            let d3d = create_device(&found.adapter)?;
            let device = winrt_device(&d3d)?;
            stages.device = ms(t.elapsed());
            grab_wgc(&d3d, &device, mon.hmon, stages)
        }
    }
}

fn monitors() -> Vec<Monitor> {
    WinDisplays::new()
        .all()
        .into_iter()
        .enumerate()
        .map(|(index, d)| {
            let r = d.physical_bounds;
            let center = POINT { x: r.x + r.width / 2, y: r.y + r.height / 2 };
            // SAFETY: takes the point by value.
            let hmon = unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONULL) };
            Monitor { index, rect: r, hmon }
        })
        .collect()
}

// ---------- statistics ----------

fn stats(mut v: Vec<f64>) -> (f64, f64, f64) {
    if v.is_empty() {
        return (f64::NAN, f64::NAN, f64::NAN);
    }
    v.sort_by(f64::total_cmp);
    let p95 = v[((v.len() as f64 * 0.95) as usize).min(v.len() - 1)];
    (v[v.len() / 2], p95, v[v.len() - 1])
}

/// Least squares slope of `y` over its index.
fn slope(y: &[f64]) -> f64 {
    let n = y.len() as f64;
    if y.len() < 2 {
        return 0.0;
    }
    let mean_x = (n - 1.0) / 2.0;
    let mean_y = y.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (i, v) in y.iter().enumerate() {
        let dx = i as f64 - mean_x;
        num += dx * (v - mean_y);
        den += dx * dx;
    }
    num / den
}

const GRID_X: usize = 64;
const GRID_Y: usize = 36;

/// A coarse sample of the frame, enough to compare and to see that it is not one flat colour.
fn grid(g: &Grabbed) -> Vec<[u8; 4]> {
    let (w, h) = (g.width as usize, g.height as usize);
    let mut out = Vec::with_capacity(GRID_X * GRID_Y);
    for gy in 0..GRID_Y {
        for gx in 0..GRID_X {
            let x = (gx * w + w / 2) / GRID_X;
            let y = (gy * h + h / 2) / GRID_Y;
            let i = (y * w + x) * 4;
            out.push([g.bgra[i], g.bgra[i + 1], g.bgra[i + 2], g.bgra[i + 3]]);
        }
    }
    out
}

fn is_uniform(grid: &[[u8; 4]]) -> bool {
    let first = grid[0];
    grid.iter().all(|p| (0..3).all(|c| p[c].abs_diff(first[c]) <= 3))
}

/// Percent of samples equal within a small tolerance, and the same with red and blue swapped, which
/// tells a wrong channel order from a screen that changed.
fn matches(a: &[[u8; 4]], b: &[[u8; 4]]) -> (f64, f64) {
    let close = |x: u8, y: u8| x.abs_diff(y) <= 2;
    let (mut same, mut swapped) = (0, 0);
    for (p, q) in a.iter().zip(b) {
        if close(p[0], q[0]) && close(p[1], q[1]) && close(p[2], q[2]) {
            same += 1;
        }
        if close(p[0], q[2]) && close(p[1], q[1]) && close(p[2], q[0]) {
            swapped += 1;
        }
    }
    let n = a.len() as f64;
    (100.0 * f64::from(same) / n, 100.0 * f64::from(swapped) / n)
}

// ---------- process counters ----------

#[derive(Clone, Copy, Debug)]
struct Counters {
    handles: u32,
    private_bytes: u64,
    working_set: u64,
    gdi: u32,
    user: u32,
}

fn counters() -> Counters {
    let mut handles = 0u32;
    let mut mem = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo-handle is always valid and the out-parameters are writable.
    unsafe {
        let me = GetCurrentProcess();
        let _ = GetProcessHandleCount(me, &mut handles);
        let _ = GetProcessMemoryInfo(
            me,
            std::ptr::from_mut(&mut mem).cast::<PROCESS_MEMORY_COUNTERS>(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        );
        Counters {
            handles,
            private_bytes: mem.PrivateUsage as u64,
            working_set: mem.WorkingSetSize as u64,
            gdi: GetGuiResources(me, GR_GDIOBJECTS),
            user: GetGuiResources(me, GR_USEROBJECTS),
        }
    }
}

// The handle table by object type. Nothing else tells a leak of Section and Mutant pairs from anything else.
#[repr(C)]
#[derive(Clone, Copy)]
struct HandleEntry {
    object: *mut c_void,
    pid: usize,
    handle: usize,
    access: u32,
    backtrace: u16,
    type_index: u16,
    attributes: u32,
    reserved: u32,
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
    fn NtQueryObject(handle: *mut c_void, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
}

fn handle_census() -> BTreeMap<String, i64> {
    const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;
    const OBJECT_TYPE_INFORMATION: u32 = 2;
    let me = unsafe { GetCurrentProcessId() } as usize;
    let mut buf: Vec<u8> = vec![0; 16 << 20];
    loop {
        let mut ret = 0u32;
        // SAFETY: the buffer and its length are valid; the call fills at most that many bytes.
        let status = unsafe {
            NtQuerySystemInformation(SYSTEM_EXTENDED_HANDLE_INFORMATION, buf.as_mut_ptr().cast(), buf.len() as u32, &mut ret)
        };
        if status == 0 {
            break;
        }
        if status as u32 != 0xC000_0004 || buf.len() > (1 << 30) {
            return BTreeMap::new();
        }
        buf = vec![0; buf.len() * 2];
    }
    // SAFETY: on success the buffer starts with the count and a reserved word, then that many entries.
    let count = unsafe { *(buf.as_ptr() as *const usize) };
    let entries = unsafe {
        std::slice::from_raw_parts(buf.as_ptr().add(2 * size_of::<usize>()).cast::<HandleEntry>(), count)
    };
    let mut names: BTreeMap<u16, String> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for e in entries.iter().filter(|e| e.pid == me) {
        let name = names.entry(e.type_index).or_insert_with(|| {
            let mut info = vec![0u8; 4096];
            let mut ret = 0u32;
            // SAFETY: querying the type of one of our own handles into a large enough buffer; on success
            // the buffer begins with the name as a UNICODE_STRING pointing inside it.
            unsafe {
                let status = NtQueryObject(e.handle as *mut c_void, OBJECT_TYPE_INFORMATION, info.as_mut_ptr().cast(), info.len() as u32, &mut ret);
                if status != 0 {
                    return format!("type{}", e.type_index);
                }
                let s = &*(info.as_ptr() as *const UnicodeString);
                String::from_utf16_lossy(std::slice::from_raw_parts(s.buffer, usize::from(s.length) / 2))
            }
        });
        *out.entry(name.clone()).or_insert(0) += 1;
    }
    out
}

fn exe_name(pid: u32) -> Option<String> {
    // SAFETY: a process snapshot, closed by dropping nothing: this tool is short-lived and calls it rarely.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == pid {
                    found = Some(String::from_utf16_lossy(&entry.szExeFile).trim_end_matches('\0').to_owned());
                    break;
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
        found
    }
}

/// What is in front right now: a fullscreen app means a game may hold the GPU and the monitor.
fn foreground(mons: &[Monitor]) -> String {
    // SAFETY: read-only queries about the current foreground window, by value.
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let rect = Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top);
        let on = mons.iter().find(|m| m.rect == rect).map(|m| m.index);
        format!(
            "foreground exe={} fullscreen_on_monitor={:?}",
            exe_name(pid).unwrap_or_else(|| "?".into()),
            on
        )
    }
}

// ---------- modes ----------

fn info() {
    let mons = monitors();
    println!("{}", foreground(&mons));
    for m in &mons {
        print!("monitor {} rect {}x{} at {},{} ", m.index, m.rect.width, m.rect.height, m.rect.x, m.rect.y);
        match find_output(m.hmon) {
            Ok(found) => {
                // SAFETY: plain getters on live COM objects.
                let (adapter, desc) = unsafe {
                    (found.adapter.GetDesc1().unwrap(), found.output.GetDesc().unwrap())
                };
                let name = String::from_utf16_lossy(&adapter.Description);
                let o6: windows::core::Result<IDXGIOutput6> = found.output.cast::<IDXGIOutput>().and_then(|o| o.cast());
                let hdr = o6
                    .ok()
                    .and_then(|o| unsafe { o.GetDesc1() }.ok())
                    .map(|d| (d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, d.BitsPerColor, d.MaxLuminance));
                println!(
                    "adapter '{}' rotation {} hdr(active,bits,maxnits) {:?}",
                    name.trim_end_matches('\0'),
                    desc.Rotation.0,
                    hdr
                );
            }
            Err(e) => println!("no output: {e}"),
        }
    }
    println!("WGC supported: {:?}", GraphicsCaptureSession::IsSupported());
    // SAFETY: a read-only query with no input.
    println!("notification state: {:?}", unsafe { windows::Win32::UI::Shell::SHQueryUserNotificationState() });
}

fn cold_child(method: Method, monitor: usize) {
    let mons = monitors();
    let mut held = Held::default();
    let t = Instant::now();
    match grab(method, &mons[monitor], &mut held) {
        Ok(g) => {
            let total = ms(t.elapsed());
            let gr = grid(&g);
            println!(
                "ok {total:.2} {:.2} {:.2} {:.2} {:.2} {:.2} {} {}",
                g.stages.device, g.stages.setup, g.stages.frame, g.stages.gpu_copy, g.stages.cpu_copy, g.width, is_uniform(&gr)
            );
        }
        Err(e) => println!("err {e}"),
    }
}

fn run_cold(method: Method, monitor: usize) -> Res<Vec<f64>> {
    let line = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["cold-child", method.name(), &monitor.to_string()])
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&line.stdout).trim().to_owned();
    let mut parts = text.split(' ');
    if parts.next() != Some("ok") {
        return Err(text);
    }
    Ok(parts.take(6).filter_map(|p| p.parse().ok()).collect())
}

fn latency(warm: usize, cold: usize, gap: Duration, only: Option<Method>) {
    let mons = monitors();
    println!("{}", foreground(&mons));
    println!(
        "{:<17} {:>3} {:>9} | {:>7} {:>7} | {:>7} {:>7} {:>7} | {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} | {:>4} {:>7} {:>7} {:>6} {:>5} | notes",
        "method", "mon", "size", "coldMed", "coldMax", "warmMed", "warmP95", "warmMax", "dev", "setup", "frame", "gpu", "cpu", "tear",
        "fail", "match%", "minMat%", "swap%", "flat"
    );
    for method in Method::ALL.into_iter().filter(|m| only.is_none_or(|o| o == *m)) {
        for mon in &mons {
            let mut cold_times = Vec::new();
            let mut cold_errors = BTreeMap::<String, u32>::new();
            for _ in 0..cold {
                match run_cold(method, mon.index) {
                    Ok(v) if !v.is_empty() => cold_times.push(v[0]),
                    Ok(_) => {}
                    Err(e) => *cold_errors.entry(e).or_insert(0) += 1,
                }
                std::thread::sleep(gap);
            }

            let mut held = Held::default();
            let mut warm_times = Vec::new();
            let mut stage_values: [Vec<f64>; 6] = Default::default();
            let (mut fail, mut flat, mut wrong_size, mut zero_present, mut acc0) = (0, 0, 0, 0, 0);
            let mut errors = BTreeMap::<String, u32>::new();
            let mut same = Vec::new();
            let mut swapped = Vec::new();
            // The first grab builds what is retained: not part of the warm numbers.
            let _ = grab(method, mon, &mut held);
            std::thread::sleep(gap);
            for _ in 0..warm {
                match grab(method, mon, &mut held) {
                    Ok(g) => {
                        warm_times.push(g.latency());
                        let s = g.stages;
                        for (slot, v) in stage_values.iter_mut().zip([s.device, s.setup, s.frame, s.gpu_copy, s.cpu_copy, s.teardown]) {
                            slot.push(v);
                        }
                        if (g.width, g.height) != (mon.rect.width as u32, mon.rect.height as u32) {
                            wrong_size += 1;
                        }
                        if let Some(d) = g.dup {
                            zero_present += u32::from(d.present_time_zero);
                            acc0 += u32::from(d.accumulated == 0);
                        }
                        let this = grid(&g);
                        flat += u32::from(is_uniform(&this));
                        drop(g);
                        if let Ok(reference) = grab_gdi(mon.rect, SRCCOPY | CAPTUREBLT) {
                            let (a, b) = matches(&this, &grid(&reference));
                            same.push(a);
                            swapped.push(b);
                        }
                    }
                    Err(e) => {
                        fail += 1;
                        *errors.entry(e).or_insert(0) += 1;
                    }
                }
                std::thread::sleep(gap);
            }
            let (cold_med, _, cold_max) = stats(cold_times);
            let (warm_med, warm_p95, warm_max) = stats(warm_times);
            let med = |i: usize, sv: &[Vec<f64>; 6]| stats(sv[i].clone()).0;
            let (match_med, _, _) = stats(same.clone());
            let match_min = same.iter().copied().fold(f64::INFINITY, f64::min);
            let (swap_med, _, _) = stats(swapped);
            println!(
                "{:<17} {:>3} {:>9} | {:>7.1} {:>7.1} | {:>7.1} {:>7.1} {:>7.1} | {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>6.1} | {:>4} {:>7.1} {:>7.1} {:>6.1} {:>5} | wrong_size={} present0={} accumulated0={} errors={:?} cold_errors={:?}",
                method.name(), mon.index, format!("{}x{}", mon.rect.width, mon.rect.height),
                cold_med, cold_max, warm_med, warm_p95, warm_max,
                med(0, &stage_values), med(1, &stage_values), med(2, &stage_values), med(3, &stage_values), med(4, &stage_values), med(5, &stage_values),
                fail, match_med, match_min, swap_med, flat, wrong_size, zero_present, acc0, errors, cold_errors
            );
        }
    }
}

/// One attempt of whatever is being tracked: its latency, and whether the result looked flat or misshapen.
struct Attempt {
    latency: f64,
    flat: bool,
    wrong_size: bool,
}

/// Runs `attempt` many times and reports what happened to the process: the handle table above all, because a
/// leak of 1.5 handles per capture is what took the Electron app to 139,628 handles in 16 days.
fn track(label: &str, grabs: usize, gap: Duration, mons: &[Monitor], mut attempt: impl FnMut(usize) -> Res<Attempt>) {
    println!("{label} | {}", foreground(mons));
    let census_start = handle_census();
    let start = counters();
    let t_start = Instant::now();
    let mut handles = Vec::with_capacity(grabs);
    let mut private = Vec::with_capacity(grabs);
    let mut latencies = Vec::with_capacity(grabs);
    let mut failures = BTreeMap::<String, u32>::new();
    let (mut ok, mut flat, mut wrong_size) = (0u32, 0u32, 0u32);
    for i in 0..grabs {
        match attempt(i) {
            Ok(a) => {
                ok += 1;
                latencies.push(a.latency);
                flat += u32::from(a.flat);
                wrong_size += u32::from(a.wrong_size);
            }
            Err(e) => *failures.entry(e).or_insert(0) += 1,
        }
        let c = counters();
        handles.push(f64::from(c.handles));
        private.push(c.private_bytes as f64);
        std::thread::sleep(gap);
        if (i + 1) % 250 == 0 {
            eprintln!("{label} {} done, handles {} private {:.1} MB", i + 1, c.handles, c.private_bytes as f64 / 1e6);
        }
    }
    let end = counters();
    let census_end = handle_census();
    let skip = handles.len() / 20;
    let half = handles.len() / 2;
    let (lat_med, lat_p95, lat_max) = stats(latencies);
    println!(
        "{label} grabs={grabs} ok={ok} failed={} flat={flat} wrong_size={wrong_size} seconds={:.0} latency med/p95/max {lat_med:.1}/{lat_p95:.1}/{lat_max:.1} ms",
        grabs as u32 - ok, t_start.elapsed().as_secs_f64()
    );
    println!(
        "  handles {} -> {} slope {:+.4}/grab (first half {:+}, second half {:+}); private {:.1} -> {:.1} MB slope {:+.0} B/grab, second half {:+.1} MB; gdi {} -> {}; user {} -> {}; ws {:.0} MB",
        start.handles, end.handles, slope(&handles[skip..]),
        handles[half] - handles[0], handles[handles.len() - 1] - handles[half],
        start.private_bytes as f64 / 1e6, end.private_bytes as f64 / 1e6, slope(&private[skip..]),
        (private[private.len() - 1] - private[half]) / 1e6, start.gdi, end.gdi, start.user, end.user, end.working_set as f64 / 1e6
    );
    let mut delta: Vec<String> = Vec::new();
    for (name, after) in &census_end {
        let before = census_start.get(name).copied().unwrap_or(0);
        if *after != before {
            delta.push(format!("{name} {before}->{after}"));
        }
    }
    println!("  handle types changed: {}", if delta.is_empty() { "none".to_string() } else { delta.join(", ") });
    println!("  failures: {failures:?}");
    println!("  end {}", foreground(mons));
}

fn stability(method: Method, grabs: usize, gap: Duration) {
    let mons = monitors();
    let mut held = Held::default();
    // Everything lazily created (DLLs, driver threads, the retained device) settles first.
    for mon in &mons {
        let _ = grab(method, mon, &mut held);
    }
    let all = mons.clone();
    track(method.name(), grabs, gap, &mons, |i| {
        let mon = &all[i % all.len()];
        grab(method, mon, &mut held)
            .map(|g| Attempt {
                latency: g.latency(),
                flat: is_uniform(&grid(&g)),
                wrong_size: (g.width, g.height) != (mon.rect.width as u32, mon.rect.height as u32),
            })
            .map_err(|e| format!("mon{} {e}", mon.index))
    });
}

/// The failure path: from a thread on a hidden desktop, where capture is refused. If destroying a failed
/// duplication leaks like Electron's does, it shows here without waiting for a game to make it fail.
fn stability_other_desktop(method: Method, grabs: usize) {
    std::thread::spawn(move || {
        // SAFETY: a new desktop no one looks at; never switched to. The name is a literal.
        let desktop = unsafe {
            CreateDesktopW(
                windows::core::w!("shotik-capture-compare-stability"),
                windows::core::PCWSTR::null(),
                None,
                DESKTOP_CONTROL_FLAGS(0),
                DESKTOP_ACCESS_FLAGS(0x10000000).0,
                None,
            )
        }
        .expect("CreateDesktop");
        // SAFETY: this thread has no windows or hooks yet, which is the requirement.
        unsafe { SetThreadDesktop(desktop) }.expect("SetThreadDesktop");
        let mons = monitors();
        let mut held = Held::default();
        for mon in &mons {
            let _ = grab(method, mon, &mut held);
        }
        let all = mons.clone();
        track(&format!("{} on another desktop", method.name()), grabs, Duration::from_millis(5), &mons, |i| {
            let mon = &all[i % all.len()];
            grab(method, mon, &mut held)
                .map(|g| Attempt { latency: g.latency(), flat: is_uniform(&grid(&g)), wrong_size: false })
                .map_err(|e| format!("mon{} {e}", mon.index))
        });
        drop(held);
        // SAFETY: created above; the COM objects using it are gone.
        let _ = unsafe { CloseDesktop(desktop) };
    })
    .join()
    .unwrap();
}

// ---------- pinning a leak down ----------

/// One create-and-release of a device, in a chosen way, to find what the leak of one Section and one Mutant
/// per device depends on.
fn device_cycle(kind: &str, adapter: &IDXGIAdapter1) -> Res<()> {
    let (mut device, mut context): (Option<ID3D11Device>, Option<ID3D11DeviceContext>) = (None, None);
    let flags = if kind == "nobgra" { D3D11_CREATE_DEVICE_FLAG(0) } else { D3D11_CREATE_DEVICE_BGRA_SUPPORT };
    // SAFETY: the out-parameters are valid for the call and the adapter is alive.
    unsafe {
        match kind {
            "default" | "flush" | "nobgra" | "wait" => D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            ),
            "adapter" => D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            ),
            "warp" => D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            ),
            other => return Err(format!("unknown device kind {other}")),
        }
    }
    .map_err(|e| hr("D3D11CreateDevice", e))?;
    if kind == "flush" {
        let context = context.as_ref().ok_or("no context")?;
        // SAFETY: plain calls on a live immediate context.
        unsafe {
            context.ClearState();
            context.Flush();
        }
    }
    drop(context);
    drop(device);
    if kind == "wait" {
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

/// Steps of one capture session on a device that stays alive, to find which one grows the ALPC ports.
fn wgc_cycle(
    kind: &str,
    cached: Option<&IGraphicsCaptureItemInterop>,
    window: Option<HWND>,
    d3d: &D3d,
    device: &IDirect3DDevice,
    hmon: HMONITOR,
) -> Res<()> {
    let fresh;
    let interop = match cached {
        Some(interop) => interop,
        None => {
            fresh = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(|e| hr("interop", e))?;
            &fresh
        }
    };
    if kind == "factory" {
        return Ok(());
    }
    // SAFETY: the handles come from the system for a monitor or window that exists.
    let item: GraphicsCaptureItem = match window {
        Some(hwnd) if kind.ends_with("-by-id") => {
            let id = windows::UI::WindowId { Value: hwnd.0 as usize as u64 };
            GraphicsCaptureItem::TryCreateFromWindowId(id).map_err(|e| hr("TryCreateFromWindowId", e))?
        }
        Some(hwnd) => unsafe { interop.CreateForWindow(hwnd) }.map_err(|e| hr("CreateForWindow", e))?,
        None => unsafe { interop.CreateForMonitor(hmon) }.map_err(|e| hr("CreateForMonitor", e))?,
    };
    if kind == "item" || kind == "item-by-id" {
        return Ok(());
    }
    let size = item.Size().map_err(|e| hr("Size", e))?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 1, size)
        .map_err(|e| hr("CreateFreeThreaded", e))?;
    if kind == "pool" {
        let _ = pool.Close();
        return Ok(());
    }
    let session = pool.CreateCaptureSession(&item).map_err(|e| hr("CreateCaptureSession", e))?;
    session.SetIsCursorCaptureEnabled(false).map_err(|e| hr("SetIsCursorCaptureEnabled", e))?;
    session.SetIsBorderRequired(false).map_err(|e| hr("SetIsBorderRequired", e))?;
    if session.IsBorderRequired().map_err(|e| hr("IsBorderRequired", e))? {
        return Err("border".into());
    }
    if kind == "session" {
        let _ = session.Close();
        let _ = pool.Close();
        return Ok(());
    }
    session.StartCapture().map_err(|e| hr("StartCapture", e))?;
    if kind == "start" {
        let _ = session.Close();
        let _ = pool.Close();
        return Ok(());
    }
    // "frame": take one and read it back, closing in the order the documentation shows.
    let deadline = Instant::now() + Duration::from_millis(1000);
    let frame = loop {
        match pool.TryGetNextFrame() {
            Ok(frame) => break frame,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            Err(e) => return Err(hr("TryGetNextFrame", e)),
        }
    };
    let surface = frame.Surface().map_err(|e| hr("Surface", e))?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|e| hr("access", e))?;
    // SAFETY: the surface wraps a D3D11 texture of the same device.
    let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(|e| hr("GetInterface", e))?;
    let mut stages = Stages::default();
    let read = read_texture(d3d, &texture, &mut stages);
    drop(texture);
    drop(access);
    let _ = surface.Close();
    let _ = frame.Close();
    let _ = session.Close();
    let _ = pool.Close();
    read.map(|_| ())
}

/// A Direct3D device and its WinRT wrapper are free-threaded, which the crate's types do not know.
struct ShareAcrossThreads<T>(T);
// SAFETY: only ever holds a D3D11 device and its agile wrapper here, and this tool uses them one thread at a time.
unsafe impl<T> Send for ShareAcrossThreads<T> {}
// SAFETY: as above.
unsafe impl<T> Sync for ShareAcrossThreads<T> {}

impl<T> ShareAcrossThreads<T> {
    fn get(&self) -> &T {
        &self.0
    }
}

/// The steps of finding an output, each freshly, to find which one grows Sections and Mutants.
fn dxgi_enum_cycle(kind: &str, hmon: HMONITOR) -> Res<()> {
    // SAFETY: creates a COM object with no input.
    let dxgi: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| hr("CreateDXGIFactory1", e))?;
    if kind == "factory" {
        return Ok(());
    }
    // SAFETY: index 0 exists on a machine with a graphics card.
    let adapter = unsafe { dxgi.EnumAdapters1(0) }.map_err(|e| hr("EnumAdapters1", e))?;
    if kind == "adapter" {
        return Ok(());
    }
    // SAFETY: index 0 exists because this machine has monitors on that adapter.
    let output = unsafe { adapter.EnumOutputs(0) }.map_err(|e| hr("EnumOutputs", e))?;
    if kind == "output" {
        return Ok(());
    }
    // SAFETY: a plain getter.
    let desc = unsafe { output.GetDesc() }.map_err(|e| hr("GetDesc", e))?;
    if kind == "desc" {
        return Ok(());
    }
    if kind == "device-on-adapter" {
        let d3d = create_device(&adapter)?;
        drop(d3d);
        return Ok(());
    }
    if kind == "duplicate" {
        let _ = desc;
        let found = find_output(hmon)?;
        let d3d = create_device(&found.adapter)?;
        // SAFETY: both alive.
        let dup = unsafe { found.output.DuplicateOutput(&d3d.device) }.map_err(|e| hr("DuplicateOutput", e))?;
        drop(dup);
        return Ok(());
    }
    Err(format!("unknown kind {kind}"))
}

fn leak(what: &str, kind: &str, times: usize, gap: Duration) {
    let mons = monitors();
    let mon = mons[0].clone();
    let found = find_output(mon.hmon).unwrap();
    let label = format!("leak {what} {kind}");
    match what {
        "device" => {
            device_cycle(kind, &found.adapter).unwrap();
            track(&label, times, gap, &mons, |_| {
                let t = Instant::now();
                device_cycle(kind, &found.adapter)?;
                Ok(Attempt { latency: ms(t.elapsed()), flat: false, wrong_size: false })
            });
        }
        "wgc" | "wgc-cached" | "wgc-window" => {
            // "wgc-cached" keeps the activation factory, "wgc-window" also targets a window of another process.
            let d3d = create_device(&found.adapter).unwrap();
            let winrt = winrt_device(&d3d).unwrap();
            let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().unwrap();
            let host = (what == "wgc-window").then(host_window);
            let hwnd = host.as_ref().map(|h| HWND(h.id as usize as *mut c_void));
            let cached = (what != "wgc").then_some(&interop);
            wgc_cycle(kind, cached, hwnd, &d3d, &winrt, mon.hmon).unwrap();
            track(&label, times, gap, &mons, |_| {
                let t = Instant::now();
                wgc_cycle(kind, cached, hwnd, &d3d, &winrt, mon.hmon)?;
                Ok(Attempt { latency: ms(t.elapsed()), flat: false, wrong_size: false })
            });
        }
        "wgc-thread" => {
            // Each capture on its own thread with its own COM apartment, torn down afterwards: are the ports
            // tied to the apartment?
            use windows::Win32::System::Com::{
                CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
            };
            let d3d = create_device(&found.adapter).unwrap();
            let winrt = winrt_device(&d3d).unwrap();
            let shared = ShareAcrossThreads((d3d, winrt));
            let host = host_window();
            let id = host.id;
            let model = if kind == "sta" { COINIT_APARTMENTTHREADED } else { COINIT_MULTITHREADED };
            let hmon_value = mon.hmon.0 as isize;
            let cycle = || -> Res<()> {
                std::thread::scope(|scope| {
                    scope
                        .spawn(|| {
                            // SAFETY: first COM call on this new thread, balanced below on the same thread.
                            unsafe { CoInitializeEx(None, model) }.ok().map_err(|e| hr("CoInitializeEx", e))?;
                            let hwnd = HWND(id as usize as *mut c_void);
                            let hmon = HMONITOR(hmon_value as *mut c_void);
                            let (d3d, winrt) = shared.get();
                            let result = wgc_cycle("frame", None, Some(hwnd), d3d, winrt, hmon);
                            // SAFETY: balances the call above; everything COM made in `wgc_cycle` is dropped.
                            unsafe { CoUninitialize() };
                            result
                        })
                        .join()
                        .map_err(|_| "panicked".to_string())?
                })
            };
            cycle().unwrap();
            track(&label, times, gap, &mons, |_| {
                let t = Instant::now();
                cycle()?;
                Ok(Attempt { latency: ms(t.elapsed()), flat: false, wrong_size: false })
            });
            drop(host);
        }
        "wgc-warp" => {
            // A software device per capture: no driver, no resident memory. Does capture work on it, and clean?
            let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().unwrap();
            let host = (kind == "window").then(host_window);
            let hwnd = host.as_ref().map(|h| HWND(h.id as usize as *mut c_void));
            let cycle = || -> Res<()> {
                let (mut device, mut context) = (None, None);
                // SAFETY: the out-parameters are valid for the call.
                unsafe {
                    D3D11CreateDevice(
                        None,
                        D3D_DRIVER_TYPE_WARP,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        None,
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        Some(&mut context),
                    )
                }
                .map_err(|e| hr("D3D11CreateDevice", e))?;
                let d3d = D3d { device: device.ok_or("no device")?, context: context.ok_or("no context")? };
                let winrt = winrt_device(&d3d)?;
                wgc_cycle("frame", Some(&interop), hwnd, &d3d, &winrt, mon.hmon)
            };
            cycle().unwrap();
            track(&label, times, gap, &mons, |_| {
                let t = Instant::now();
                cycle()?;
                Ok(Attempt { latency: ms(t.elapsed()), flat: false, wrong_size: false })
            });
        }
        "dxgi" => {
            dxgi_enum_cycle(kind, mon.hmon).unwrap();
            track(&label, times, gap, &mons, |_| {
                let t = Instant::now();
                dxgi_enum_cycle(kind, mon.hmon)?;
                Ok(Attempt { latency: ms(t.elapsed()), flat: false, wrong_size: false })
            });
        }
        other => panic!("unknown leak target {other}"),
    }
    settle();
}

/// After a run: a real leak stays where it is, a lazily reclaimed resource comes back down by itself.
fn settle() {
    let Some(seconds) = std::env::var("SHOTIK_BENCH_SETTLE").ok().and_then(|v| v.parse::<u64>().ok()) else {
        return;
    };
    let start = Instant::now();
    while start.elapsed().as_secs() < seconds {
        std::thread::sleep(Duration::from_secs(30));
        let census = handle_census();
        let c = counters();
        println!(
            "  settle +{:>3} s: handles {} ALPC {} Section {} Mutant {} private {:.1} MB",
            start.elapsed().as_secs(),
            c.handles,
            census.get("ALPC Port").copied().unwrap_or(0),
            census.get("Section").copied().unwrap_or(0),
            census.get("Mutant").copied().unwrap_or(0),
            c.private_bytes as f64 / 1e6
        );
    }
}

// ---------- window capture ----------

/// Hosts a test window in its own process, so captures cross a process boundary like real ones, until the
/// parent closes our stdin.
fn window_host(title: &str) {
    let _window = TestWindow::open(Options::new(title));
    let mut sink = String::new();
    let _ = std::io::stdin().read_line(&mut sink);
}

struct HostedWindow {
    child: std::process::Child,
    id: u64,
    bounds: Rect,
}

// The child is left behind only when this tool panics, and it exits when our end of its stdin closes.
#[allow(clippy::zombie_processes)]
fn host_window() -> HostedWindow {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let title = format!("ShotikR3WindowHost-{}", std::process::id());
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["window-host", &title])
        .stdin(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("the window host starts");
    let source = WinWindowSource::new();
    for _ in 0..200 {
        if let Some(w) = source.list(None).unwrap().into_iter().find(|w| w.title == title) {
            return HostedWindow { child, id: w.id, bounds: w.bounds.unwrap() };
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the hosted window did not appear");
}

impl Drop for HostedWindow {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn window_stability(method: WindowCaptureMethod, grabs: usize, gap: Duration) {
    let mons = monitors();
    let host = host_window();
    let source = WinWindowSource::new();
    let bounds = host.bounds;
    let id = host.id;
    let _ = source.capture(id, method);
    track(&format!("window {method:?} across processes"), grabs, gap, &mons, |_| {
        let t = Instant::now();
        let frame = source.capture(id, method).map_err(|e| e.to_string())?;
        let latency = ms(t.elapsed());
        let (w, h) = (frame.width() as i32, frame.height() as i32);
        let i = ((frame.height() / 2 * frame.width() + frame.width() / 2) * 4) as usize;
        let center = &frame.bgra()[i..i + 3];
        Ok(Attempt {
            latency,
            flat: center.iter().zip([40u8, 30, 200]).any(|(got, want)| got.abs_diff(want) > 3),
            wrong_size: (w, h) != (bounds.width, bounds.height),
        })
    });
    // The shared device stays alive here, as in production.
    settle();
    drop(host);
}

fn list_stability(grabs: usize, gap: Duration) {
    let mons = monitors();
    let source = WinWindowSource::new();
    let _ = source.list(None);
    track("list_windows", grabs, gap, &mons, |_| {
        let t = Instant::now();
        let list = source.list(Some(std::process::id())).map_err(|e| e.to_string())?;
        Ok(Attempt { latency: ms(t.elapsed()), flat: list.is_empty(), wrong_size: false })
    });
}

fn footprint() {
    let mons = monitors();
    let line = |label: &str| {
        let c = counters();
        println!(
            "{label:<44} private {:>7.1} MB  ws {:>7.1} MB  handles {:>5}",
            c.private_bytes as f64 / 1e6, c.working_set as f64 / 1e6, c.handles
        );
    };
    line("start (displays enumerated)");
    let found = find_output(mons[0].hmon).unwrap();
    line("DXGI factory and output, no device");
    let t = Instant::now();
    let d3d = create_device(&found.adapter).unwrap();
    println!("  first D3D11CreateDevice {:.1} ms", ms(t.elapsed()));
    line("device held");
    let t = Instant::now();
    let again = create_device(&found.adapter).unwrap();
    println!("  second D3D11CreateDevice (first still held) {:.1} ms", ms(t.elapsed()));
    drop(again);
    let g = grab_dxgi(&d3d, &found.output, Stages::default()).unwrap();
    drop(g);
    line("device held after one DXGI grab");
    let winrt = winrt_device(&d3d).unwrap();
    let g = grab_wgc(&d3d, &winrt, mons[0].hmon, Stages::default()).unwrap();
    drop(g);
    line("device held after one WGC grab");
    drop(winrt);
    drop(d3d);
    line("device released");
    std::thread::sleep(Duration::from_millis(500));
    line("device released, 500 ms later");
    let t = Instant::now();
    let d3d = create_device(&found.adapter).unwrap();
    println!("  D3D11CreateDevice after release {:.1} ms", ms(t.elapsed()));
    line("device held again");
    drop(d3d);
    std::thread::sleep(Duration::from_millis(200));
    let mut again_ms = Vec::new();
    for _ in 0..10 {
        let t = Instant::now();
        let d3d = create_device(&found.adapter).unwrap();
        again_ms.push(ms(t.elapsed()));
        drop(d3d);
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("  create after release, ten times: {again_ms:.1?}");
}

/// A session kept open and left alone, the way a warm-up that "keeps it ready" would. Prints our own CPU;
/// the DWM and GPU side is sampled from outside by whoever runs this.
fn hold(method: Method, monitor: usize, seconds: u64) {
    let mons = monitors();
    let mon = &mons[monitor];
    let found = find_output(mon.hmon).unwrap();
    let d3d = create_device(&found.adapter).unwrap();
    let cpu = || {
        let (mut a, mut b, mut k, mut u) = Default::default();
        // SAFETY: the pseudo-handle is valid and the out-parameters are writable.
        unsafe {
            let _ = windows::Win32::System::Threading::GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u);
        }
        let v = |f: windows::Win32::Foundation::FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
        (v(k) + v(u)) as f64 / 10_000.0
    };
    let before = counters();
    match method {
        Method::Dxgi => {
            // SAFETY: both COM objects are alive.
            let dup = unsafe { found.output.DuplicateOutput(&d3d.device) }.unwrap();
            let c0 = cpu();
            println!("holding a duplication on monitor {monitor} for {seconds} s, not acquiring");
            std::thread::sleep(Duration::from_secs(seconds));
            println!("cpu {:.2} ms/s", (cpu() - c0) / seconds as f64);
            drop(dup);
        }
        Method::Wgc => {
            let winrt = winrt_device(&d3d).unwrap();
            let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().unwrap();
            // SAFETY: the handle comes from MonitorFromPoint for a monitor that exists.
            let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(mon.hmon) }.unwrap();
            let size = item.Size().unwrap();
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt, DirectXPixelFormat::B8G8R8A8UIntNormalized, 1, size).unwrap();
            let session = pool.CreateCaptureSession(&item).unwrap();
            session.SetIsCursorCaptureEnabled(false).unwrap();
            session.SetIsBorderRequired(false).unwrap();
            assert!(!session.IsBorderRequired().unwrap());
            session.StartCapture().unwrap();
            let c0 = cpu();
            println!("holding a capture session on monitor {monitor} for {seconds} s, frames left in the pool");
            std::thread::sleep(Duration::from_secs(seconds));
            println!("cpu {:.2} ms/s", (cpu() - c0) / seconds as f64);
            let _ = session.Close();
            let _ = pool.Close();
        }
        _ => panic!("hold takes dxgi or wgc"),
    }
    let after = counters();
    println!("private {:.1} -> {:.1} MB", before.private_bytes as f64 / 1e6, after.private_bytes as f64 / 1e6);
}

/// From a thread attached to a new hidden desktop, which is not the input desktop. A stand-in for the
/// secure desktop that shows nothing to the user; the real thing needs a UAC prompt or a lock screen.
fn other_desktop() {
    std::thread::spawn(move || {
        // SAFETY: a new desktop no one looks at; never switched to. The name is a literal.
        let desktop = unsafe {
            CreateDesktopW(
                windows::core::w!("shotik-capture-compare"),
                windows::core::PCWSTR::null(),
                None,
                DESKTOP_CONTROL_FLAGS(0),
                DESKTOP_ACCESS_FLAGS(0x10000000).0, // GENERIC_ALL
                None,
            )
        }
        .expect("CreateDesktop");
        // SAFETY: this thread has no windows or hooks yet, which is the requirement.
        unsafe { SetThreadDesktop(desktop) }.expect("SetThreadDesktop");
        let mons = monitors();
        let mon = &mons[0];
        for method in Method::ALL {
            let mut held = Held::default();
            let mut results = Vec::new();
            for _ in 0..3 {
                results.push(match grab(method, mon, &mut held) {
                    Ok(g) => {
                        let gr = grid(&g);
                        format!("ok {:.1} ms flat={} first={:?}", g.latency(), is_uniform(&gr), &gr[0][..3])
                    }
                    Err(e) => format!("err {e}"),
                });
            }
            println!("{:<17} {results:?}", method.name());
        }
        // SAFETY: created above; the COM objects using it are gone.
        let _ = unsafe { CloseDesktop(desktop) };
    })
    .join()
    .unwrap();
}

fn main() {
    set_dpi_awareness().expect("per-monitor DPI awareness");
    // To see whether a leak depends on the thread having an apartment.
    if std::env::var_os("SHOTIK_BENCH_MTA").is_some() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        // SAFETY: first COM call on this thread, for the life of the process.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().expect("COM");
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0) {
        Some("info") => info(),
        Some("cold-child") => cold_child(Method::parse(arg(1).unwrap()), arg(2).unwrap().parse().unwrap()),
        Some("latency") => latency(
            arg(1).and_then(|v| v.parse().ok()).unwrap_or(30),
            arg(2).and_then(|v| v.parse().ok()).unwrap_or(5),
            Duration::from_millis(arg(3).and_then(|v| v.parse().ok()).unwrap_or(120)),
            arg(4).map(Method::parse),
        ),
        Some("stability") => stability(
            Method::parse(arg(1).unwrap()),
            arg(2).and_then(|v| v.parse().ok()).unwrap_or(1000),
            Duration::from_millis(arg(3).and_then(|v| v.parse().ok()).unwrap_or(60)),
        ),
        Some("footprint") => footprint(),
        Some("hold") => hold(Method::parse(arg(1).unwrap()), arg(2).unwrap().parse().unwrap(), arg(3).unwrap().parse().unwrap()),
        Some("otherdesktop") => other_desktop(),
        Some("stability-otherdesktop") => stability_other_desktop(
            Method::parse(arg(1).unwrap()),
            arg(2).and_then(|v| v.parse().ok()).unwrap_or(1000),
        ),
        Some("leak") => leak(
            arg(1).unwrap(),
            arg(2).unwrap(),
            arg(3).and_then(|v| v.parse().ok()).unwrap_or(200),
            Duration::from_millis(arg(4).and_then(|v| v.parse().ok()).unwrap_or(20)),
        ),
        Some("window-host") => window_host(arg(1).unwrap()),
        Some("window-stability") => window_stability(
            match arg(1).unwrap() {
                "printwindow" => WindowCaptureMethod::PrintWindow,
                "wgc" => WindowCaptureMethod::GraphicsCapture,
                other => panic!("unknown window method {other}"),
            },
            arg(2).and_then(|v| v.parse().ok()).unwrap_or(1000),
            Duration::from_millis(arg(3).and_then(|v| v.parse().ok()).unwrap_or(20)),
        ),
        Some("list-stability") => list_stability(
            arg(1).and_then(|v| v.parse().ok()).unwrap_or(1000),
            Duration::from_millis(arg(2).and_then(|v| v.parse().ok()).unwrap_or(10)),
        ),
        _ => eprintln!("usage: see the top of examples/capture_compare.rs"),
    }
}
