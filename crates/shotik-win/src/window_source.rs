//! Enumerates and captures top-level windows. Implements `WindowSource`.
//!
//! Direct Win32 ports of `src/main/windows-enum.ps1 -Detailed` and `src/main/capture-window.ps1`, without
//! the PowerShell. Coordinates are physical pixels through a per-monitor-aware scope on this thread.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

use shotik_core::error::{Error, Result};
use shotik_core::frame::Frame;
use shotik_core::geom::Rect;
use shotik_core::ports::{NativeWindow, WindowCaptureMethod, WindowSource};
use windows::core::BOOL;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowPlacement, GetWindowRect, GetWindowThreadProcessId, InternalGetWindowText, IsIconic, IsWindow,
    IsWindowVisible, WINDOWPLACEMENT,
};

use crate::capture::gdi::Canvas;
use crate::capture::wgc;
use crate::displays::{from_wide_buf, rect_from, win_err, PerMonitorDpiScope};

/// `capture-window.ps1` ran under an 8 s process timeout. `PrintWindow` can wait on the target's thread,
/// and a hung application must become an error, not a stuck MCP call.
const PRINT_WINDOW_TIMEOUT: Duration = Duration::from_secs(8);
/// The first frame of a window normally takes one composition.
const GRAPHICS_CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);
/// `PW_RENDERFULLCONTENT`: reads the DWM-composited content, so a covered or off-screen window works.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);
/// Windows smaller than this are not worth listing, as in `windows-enum.ps1`.
const MIN_SIDE: i32 = 48;

fn hwnd_from_id(id: u64) -> HWND {
    HWND(id as isize as *mut c_void)
}

/// `Process.ProcessName`: the executable's name without `.exe`.
fn strip_exe(name: &str) -> &str {
    match name.len().checked_sub(4) {
        Some(cut) if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".exe") => &name[..cut],
        _ => name,
    }
}

/// Process names by pid, from one snapshot. The snapshot also names elevated processes, which opening each
/// process would not, and `Process.GetProcessById` did not need to either.
fn process_names() -> HashMap<u32, String> {
    let mut names = HashMap::new();
    // SAFETY: the snapshot handle is closed below; `entry` has `dwSize` set as the API requires.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return names;
        };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = from_wide_buf(&entry.szExeFile);
                names.insert(entry.th32ProcessID, strip_exe(&name).to_owned());
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    names
}

/// The name of one process, for an error message.
pub(crate) fn process_name(pid: u32) -> Option<String> {
    process_names().remove(&pid)
}

/// The window's title as the system stores it. `InternalGetWindowText` never sends a message, so a hung
/// window cannot block the list: exactly what `GetWindowText` did from the separate PowerShell process.
fn window_title(hwnd: HWND) -> String {
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: the buffer and its length are valid, and an invalid window just yields 0.
        let len = unsafe { InternalGetWindowText(hwnd, &mut buf) };
        let len = usize::try_from(len).unwrap_or(0);
        if len + 1 < buf.len() || buf.len() >= 1 << 16 {
            return String::from_utf16_lossy(&buf[..len]);
        }
        let bigger = buf.len() * 2;
        buf.resize(bigger, 0);
    }
}

fn dwm_rect(hwnd: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    // SAFETY: `r` is exactly the size passed, and this attribute writes a RECT.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            std::ptr::from_mut(&mut r).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .ok()?;
    Some(rect_from(r))
}

/// The filter of `windows-enum.ps1`, field by field. Tool windows are not excluded: that was a Chromium
/// limit of the fallback path (see `test/ghost/testwin.ps1`), not a rule of this list.
fn describe(hwnd: HWND, exclude_pid: Option<u32>, names: &HashMap<u32, String>) -> Option<NativeWindow> {
    // SAFETY: all of these take the handle by value and fail or return false when it is stale, which can
    // happen at any moment because the windows belong to other processes.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let minimized = IsIconic(hwnd).as_bool();
        let mut cloaked = 0u32;
        // Its failure leaves 0, as in the script: not cloaked.
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            std::ptr::from_mut(&mut cloaked).cast(),
            size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if exclude_pid == Some(pid) {
            return None;
        }
        let title = window_title(hwnd);
        if title.is_empty() {
            return None;
        }
        let measured = if minimized {
            // DWM bounds of an iconic window point at -32000. The restored rectangle is only good for the
            // size check: it is in workspace coordinates, so it is not reported as bounds.
            let mut placement = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            GetWindowPlacement(hwnd, &mut placement).ok()?;
            rect_from(placement.rcNormalPosition)
        } else {
            dwm_rect(hwnd)?
        };
        if measured.width < MIN_SIDE || measured.height < MIN_SIDE {
            return None;
        }
        Some(NativeWindow {
            id: hwnd.0 as usize as u64,
            title,
            app: names.get(&pid).filter(|n| !n.is_empty()).cloned(),
            bounds: (!minimized).then_some(measured),
            minimized,
        })
    }
}

/// The DWM frame inside the window rectangle, relative to it, when that actually crops something.
/// The same conditions as `capture-window.ps1`.
fn crop_to_frame(window: Rect, frame: Rect) -> Option<Rect> {
    let (cx, cy) = (frame.x - window.x, frame.y - window.y);
    let fits = cx >= 0
        && cy >= 0
        && frame.width >= 1
        && frame.height >= 1
        && cx + frame.width <= window.width
        && cy + frame.height <= window.height;
    let smaller = frame.width < window.width || frame.height < window.height;
    (fits && smaller).then_some(Rect::new(cx, cy, frame.width, frame.height))
}

/// The part of a top-down 32-bit bitmap inside `area`, with alpha forced opaque. GDI and DWM leave alpha
/// at 0 or undefined, and the script's conversion to a 24-bit image dropped it in the same way. Without
/// this the PNG would be fully transparent.
fn crop_opaque(bgra: &[u8], width: usize, area: Rect) -> Vec<u8> {
    let (w, h) = (area.width as usize, area.height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let start = (y + area.y as usize) * width * 4 + area.x as usize * 4;
        out.extend_from_slice(&bgra[start..start + w * 4]);
    }
    crate::capture::d3d::force_opaque(&mut out);
    out
}

/// Refuses what a capture cannot read, with the reference's wording. Shared by both methods.
fn check_capturable(hwnd: HWND, id: u64) -> Result<Rect> {
    // SAFETY: both take the handle by value and only inspect it.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return Err(Error::msg(format!("hwnd {id} is not a window")));
        }
        if IsIconic(hwnd).as_bool() {
            return Err(Error::msg("window is minimized"));
        }
    }
    let mut r = RECT::default();
    // SAFETY: `r` is a valid out-parameter.
    unsafe { GetWindowRect(hwnd, &mut r) }.map_err(|e| win_err("cannot read the window's rectangle", e))?;
    let rect = rect_from(r);
    if rect.width < 1 || rect.height < 1 {
        return Err(Error::msg(format!("window has empty bounds ({} x {})", rect.width, rect.height)));
    }
    Ok(rect)
}

fn print_window(id: u64) -> Result<Frame> {
    let _dpi = PerMonitorDpiScope::enter();
    let hwnd = hwnd_from_id(id);
    let window = check_capturable(hwnd, id)?;
    // 32 bits, not the 24 the script names: `PW_RENDERFULLCONTENT` into a real 24-bit DIB comes back all
    // black (measured on this machine, for every window tried). The script's 24-bit `Bitmap` goes through
    // GDI+, which evidently gives PrintWindow a different target. `prints_like_the_script` holds them equal.
    let canvas = Canvas::new(window.width, window.height)?;
    // SAFETY: a valid window and a valid memory DC with a bitmap the size of the window.
    if !unsafe { PrintWindow(hwnd, canvas.dc(), PW_RENDERFULLCONTENT) }.as_bool() {
        return Err(Error::msg("PrintWindow failed"));
    }
    let area = dwm_rect(hwnd)
        .and_then(|frame| crop_to_frame(window, frame))
        .unwrap_or(Rect::new(0, 0, window.width, window.height));
    let bgra = crop_opaque(canvas.pixels(), canvas.width(), area);
    Frame::new(area.width as u32, area.height as u32, bgra)
}

/// Runs `print_window` on its own thread so a hung target becomes an error. On a timeout the thread stays
/// blocked inside the system call until the target answers or dies, then exits and frees everything.
fn print_window_with_timeout(id: u64, timeout: Duration) -> Result<Frame> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Each stuck thread holds a device context and a bitmap, so repeated calls on a hung window must not add
    // up. They all come back together once the window answers.
    static STUCK_OR_RUNNING: AtomicUsize = AtomicUsize::new(0);
    const MOST: usize = 8;
    if STUCK_OR_RUNNING.fetch_add(1, Ordering::SeqCst) >= MOST {
        STUCK_OR_RUNNING.fetch_sub(1, Ordering::SeqCst);
        return Err(Error::msg(format!("hwnd {id}: earlier captures are still waiting for hung applications")));
    }
    let (done, result) = mpsc::channel();
    std::thread::Builder::new()
        .name("shotik-printwindow".into())
        .spawn(move || {
            let _ = done.send(print_window(id));
            STUCK_OR_RUNNING.fetch_sub(1, Ordering::SeqCst);
        })
        .map_err(|e| {
            STUCK_OR_RUNNING.fetch_sub(1, Ordering::SeqCst);
            Error::msg(format!("cannot start a capture thread: {e}"))
        })?;
    result.recv_timeout(timeout).map_err(|_| {
        Error::msg(format!("hwnd {id} did not answer within {} s: the application may be hung", timeout.as_secs()))
    })?
}

fn graphics_capture(id: u64) -> Result<Frame> {
    let _dpi = PerMonitorDpiScope::enter();
    let hwnd = hwnd_from_id(id);
    let window = check_capturable(hwnd, id)?;
    let pixels = wgc::capture_window(hwnd, GRAPHICS_CAPTURE_TIMEOUT)?;
    // Windows.Graphics.Capture returns the window's visual area already (measured: 386x293 for a 400x300
    // window, the DWM frame), but tell from the size rather than assume: a frame the size of the whole window
    // rectangle would still have the invisible resize borders.
    if (pixels.width() as i32, pixels.height() as i32) == (window.width, window.height) {
        if let Some(area) = dwm_rect(hwnd).and_then(|frame| crop_to_frame(window, frame)) {
            return pixels.crop(area);
        }
    }
    Ok(pixels)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WinWindowSource;

impl WinWindowSource {
    pub fn new() -> Self {
        Self
    }
}

impl WindowSource for WinWindowSource {
    /// Visible top-level windows, topmost first. `EnumWindows` walks them in z-order.
    fn list(&self, exclude_pid: Option<u32>) -> Result<Vec<NativeWindow>> {
        let _dpi = PerMonitorDpiScope::enter();

        unsafe extern "system" fn collect(hwnd: HWND, out: LPARAM) -> BOOL {
            // SAFETY: `out` is the Vec below, alive and unaliased for the duration of EnumWindows.
            unsafe { (*(out.0 as *mut Vec<HWND>)).push(hwnd) };
            true.into()
        }

        let mut handles: Vec<HWND> = Vec::with_capacity(256);
        // SAFETY: the callback only pushes to `handles`, which outlives the call.
        unsafe { EnumWindows(Some(collect), LPARAM(std::ptr::from_mut(&mut handles) as isize)) }
            .map_err(|e| win_err("could not enumerate windows", e))?;
        let names = process_names();
        Ok(handles.into_iter().filter_map(|hwnd| describe(hwnd, exclude_pid, &names)).collect())
    }

    fn capture(&self, id: u64, method: WindowCaptureMethod) -> Result<Frame> {
        match method {
            WindowCaptureMethod::PrintWindow => print_window_with_timeout(id, PRINT_WINDOW_TIMEOUT),
            WindowCaptureMethod::GraphicsCapture => graphics_capture(id),
        }
    }
}

/// A window at x=22000 for tests and the stability example: off every monitor, at the bottom of the
/// z-order, never active, with no taskbar button (`WS_EX_NOACTIVATE`). See `test/ghost/testwin.ps1` for
/// why it must be shown explicitly: a process started hidden hands that state to its first window.
#[cfg(any(test, feature = "tools"))]
#[doc(hidden)]
pub mod test_support {
    use super::*;
    use std::sync::Arc;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, HINSTANCE, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, HBRUSH, PAINTSTRUCT};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW, GetWindowLongPtrW,
        PostMessageW, PostQuitMessage, RegisterClassExW, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
        CREATESTRUCTW, GWLP_USERDATA, HWND_BOTTOM, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SW_SHOWMINNOACTIVE, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DESTROY, WM_NCCREATE, WM_PAINT, WNDCLASSEXW,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
    };

    /// Colours as `(blue, green, red)`, the order they come back in a frame.
    pub type Bgr = (u8, u8, u8);

    #[derive(Clone)]
    pub struct Options {
        pub title: String,
        pub x: i32,
        pub y: i32,
        pub width: i32,
        pub height: i32,
        pub background: Bgr,
        /// A rectangle in client coordinates in another colour, so the content is not uniform.
        pub patch: Option<(Rect, Bgr)>,
        pub tool_window: bool,
    }

    impl Options {
        pub fn new(title: impl Into<String>) -> Self {
            Self {
                title: title.into(),
                x: 22000,
                y: 300,
                width: 400,
                height: 300,
                background: (40, 30, 200),
                patch: Some((Rect::new(40, 40, 120, 80), (90, 200, 30))),
                tool_window: false,
            }
        }
    }

    struct Paint {
        background: Bgr,
        patch: Option<(Rect, Bgr)>,
    }

    fn colorref((b, g, r): Bgr) -> COLORREF {
        COLORREF(u32::from(b) << 16 | u32::from(g) << 8 | u32::from(r))
    }

    /// Blocks the window's thread for a while, the way a hung application behaves.
    const WM_APP_STALL: u32 = WM_APP + 1;

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_NCCREATE => {
                // SAFETY: lParam is the CREATESTRUCTW, and lpCreateParams the Arc pointer given below, which
                // the owning thread keeps alive until the message loop ends.
                unsafe {
                    let create = &*(lparam.0 as *const CREATESTRUCTW);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
            WM_PAINT => {
                // SAFETY: the standard paint sequence on our own window; every GDI object is released.
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    let dc = BeginPaint(hwnd, &mut ps);
                    let paint = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Paint;
                    if let Some(paint) = paint.as_ref() {
                        let mut client = RECT::default();
                        let _ = GetClientRect(hwnd, &mut client);
                        let brush: HBRUSH = CreateSolidBrush(colorref(paint.background));
                        FillRect(dc, &client, brush);
                        let _ = DeleteObject(brush.into());
                        if let Some((r, color)) = paint.patch {
                            let brush: HBRUSH = CreateSolidBrush(colorref(color));
                            let rect = RECT { left: r.x, top: r.y, right: r.x + r.width, bottom: r.y + r.height };
                            FillRect(dc, &rect, brush);
                            let _ = DeleteObject(brush.into());
                        }
                    }
                    let _ = EndPaint(hwnd, &ps);
                }
                LRESULT(0)
            }
            WM_APP_STALL => {
                std::thread::sleep(Duration::from_millis(wparam.0 as u64));
                LRESULT(0)
            }
            WM_DESTROY => {
                // SAFETY: called on the thread whose message loop should end.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            // SAFETY: the standard default.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// A window with its own thread and message loop, destroyed on drop.
    pub struct TestWindow {
        hwnd: isize,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl TestWindow {
        pub fn open(options: Options) -> Self {
            let (ready, wait) = mpsc::channel();
            let thread = std::thread::spawn(move || run(options, &ready));
            let hwnd = wait.recv().expect("the test window thread started");
            Self { hwnd, thread: Some(thread) }
        }

        pub fn id(&self) -> u64 {
            self.hwnd as usize as u64
        }

        pub fn hwnd(&self) -> HWND {
            HWND(self.hwnd as *mut c_void)
        }

        /// Puts this window right below `above` in the z-order, staying without activation.
        pub fn place_below(&self, above: &TestWindow) {
            // SAFETY: both windows are alive; the flags keep size, position and activation unchanged.
            unsafe {
                SetWindowPos(self.hwnd(), Some(above.hwnd()), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)
            }
            .expect("SetWindowPos");
        }

        pub fn minimize(&self) {
            // SAFETY: a valid window; the command does not activate anything.
            let _ = unsafe { ShowWindow(self.hwnd(), SW_SHOWMINNOACTIVE) };
        }

        /// Makes the window's thread stop answering for this long.
        pub fn stall(&self, duration: Duration) {
            // SAFETY: a plain post to our own window.
            unsafe { PostMessageW(Some(self.hwnd()), WM_APP_STALL, WPARAM(duration.as_millis() as usize), LPARAM(0)) }
                .expect("PostMessage");
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: a plain post; the window thread destroys its own window.
            let _ = unsafe { PostMessageW(Some(self.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn run(options: Options, ready: &mpsc::Sender<isize>) {
        let _dpi = PerMonitorDpiScope::enter();
        let paint = Arc::new(Paint { background: options.background, patch: options.patch });
        let title = crate::displays::wide(&options.title);
        // SAFETY: a null name returns this module's handle.
        let module = unsafe { GetModuleHandleW(None) }.expect("module handle");
        let instance: HINSTANCE = module.into();
        let class = w!("ShotikTestWindow");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        };
        // SAFETY: fully initialized. Registering twice fails harmlessly, with the same procedure.
        unsafe { RegisterClassExW(&wc) };
        let mut ex_style = WS_EX_NOACTIVATE;
        if options.tool_window {
            ex_style |= WS_EX_TOOLWINDOW;
        }
        // SAFETY: created hidden, off every monitor. The parameter points at `paint`, alive until the loop ends.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(ex_style.0),
                class,
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW,
                options.x,
                options.y,
                options.width,
                options.height,
                None,
                None,
                Some(instance),
                Some(Arc::as_ptr(&paint).cast()),
            )
        }
        .expect("CreateWindowEx");
        // Shown at the bottom of the z-order and without activation: nothing the user could notice.
        // SAFETY: our own window, by value.
        unsafe {
            SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW)
        }
        .expect("SetWindowPos");
        // A window entirely off the desktop gets no WM_PAINT of its own. PrintWindow forces one, the way an
        // application's own painting would have filled the surface before it ever moved off-screen.
        if let Ok(canvas) = Canvas::new(1, 1) {
            // SAFETY: our own window and a valid memory DC. Clipped to one pixel: only the side effect counts.
            let _ = unsafe { PrintWindow(hwnd, canvas.dc(), PW_RENDERFULLCONTENT) };
        }
        ready.send(hwnd.0 as isize).expect("the opener is waiting");
        let mut msg = MSG::default();
        // SAFETY: the standard message loop on this thread.
        unsafe {
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // WM_CLOSE reached DefWindowProc, which destroyed the window. Harmless if something else did.
        // SAFETY: a stale handle just fails.
        let _ = unsafe { DestroyWindow(hwnd) };
        drop(paint);
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{Options, TestWindow};
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};

    fn unique_title(tag: &str) -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        format!("ShotikR3Test-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
    }

    fn find(title: &str) -> Option<NativeWindow> {
        WinWindowSource::new().list(None).unwrap().into_iter().find(|w| w.title == title)
    }

    /// The window system answers these calls asynchronously: give a new window a moment.
    fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        false
    }

    /// Pixel at a position in a frame as (blue, green, red, alpha).
    fn px(f: &Frame, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = (y * f.width() + x) as usize * 4;
        let b = f.bgra();
        (b[i], b[i + 1], b[i + 2], b[i + 3])
    }

    fn close(a: (u8, u8, u8), b: (u8, u8, u8)) -> bool {
        a.0.abs_diff(b.0) <= 3 && a.1.abs_diff(b.1) <= 3 && a.2.abs_diff(b.2) <= 3
    }

    /// Captures until the window has painted, which can take a few moments after it was shown.
    fn capture_painted(id: u64, method: WindowCaptureMethod, expect: (u8, u8, u8)) -> Frame {
        let source = WinWindowSource::new();
        let mut last = None;
        for _ in 0..40 {
            let f = source.capture(id, method).expect("capture");
            let (b, g, r, _) = px(&f, f.width() / 2, f.height() / 2);
            if close((b, g, r), expect) {
                return f;
            }
            last = Some(f);
            std::thread::sleep(Duration::from_millis(50));
        }
        let f = last.unwrap();
        panic!("the center stayed {:?}, expected {expect:?}", px(&f, f.width() / 2, f.height() / 2));
    }

    fn assert_content(f: &Frame, window_bounds: Rect) {
        assert_eq!(
            (f.width() as i32, f.height() as i32),
            (window_bounds.width, window_bounds.height),
            "cropped to the DWM frame, as list reports it"
        );
        let opaque = f.bgra().chunks_exact(4).all(|p| p[3] == 255);
        assert!(opaque, "every pixel is opaque");
        let first = f.bgra()[..3].to_vec();
        let uniform = f.bgra().chunks_exact(4).all(|p| p[..3] == first[..]);
        assert!(!uniform, "the content is not a flat colour");
    }

    // ----- pure -----

    #[test]
    fn process_name_drops_only_a_final_exe() {
        assert_eq!(strip_exe("notepad.exe"), "notepad");
        assert_eq!(strip_exe("NOTEPAD.EXE"), "NOTEPAD");
        assert_eq!(strip_exe("a.exe.txt"), "a.exe.txt");
        assert_eq!(strip_exe("System"), "System");
        assert_eq!(strip_exe(".exe"), "");
        assert_eq!(strip_exe("exe"), "exe");
        assert_eq!(strip_exe("файл.exe"), "файл");
    }

    #[test]
    fn the_crop_follows_the_script() {
        let window = Rect::new(100, 100, 416, 339);
        assert_eq!(crop_to_frame(window, Rect::new(107, 100, 402, 332)), Some(Rect::new(7, 0, 402, 332)));
        assert_eq!(crop_to_frame(window, window), None, "nothing to crop");
        assert_eq!(crop_to_frame(window, Rect::new(90, 100, 100, 100)), None, "starts outside");
        assert_eq!(crop_to_frame(window, Rect::new(100, 100, 500, 100)), None, "ends outside");
        assert_eq!(crop_to_frame(window, Rect::new(100, 100, 0, 100)), None, "empty");
    }

    #[test]
    fn a_crop_keeps_the_right_rows_and_is_opaque() {
        // Two rows of three pixels, alpha left at 0 by GDI.
        let bgra = [
            1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, //
            11, 12, 13, 0, 14, 15, 16, 0, 17, 18, 19, 0,
        ];
        assert_eq!(crop_opaque(&bgra, 3, Rect::new(1, 1, 2, 1)), vec![14, 15, 16, 255, 17, 18, 19, 255]);
        assert_eq!(crop_opaque(&bgra, 3, Rect::new(0, 0, 1, 2)), vec![1, 2, 3, 255, 11, 12, 13, 255]);
    }

    // ----- the list -----

    #[test]
    fn lists_a_window_like_the_script() {
        let title = unique_title("list");
        let w = TestWindow::open(Options::new(&title));
        assert!(wait_for(|| find(&title).is_some()), "the window appears in the list");
        let found = find(&title).unwrap();
        assert_eq!(found.id, w.id());
        assert!(!found.minimized);
        let exe = std::env::current_exe().unwrap();
        assert_eq!(found.app.as_deref(), exe.file_stem().and_then(|s| s.to_str()), "app is the process name");
        let bounds = found.bounds.unwrap();
        assert_eq!(Some(bounds), dwm_rect(w.hwnd()), "DWM frame bounds in physical pixels");
        assert!(bounds.x >= 22000, "off-screen, {bounds:?}");
    }

    #[test]
    fn an_unaware_thread_gets_the_same_window_bounds() {
        let title = unique_title("unaware");
        let _w = TestWindow::open(Options::new(&title));
        assert!(wait_for(|| find(&title).is_some()));
        let aware = find(&title).unwrap();
        let unaware = crate::displays::tests::on_unaware_thread(|| find(&title).unwrap());
        assert_eq!(unaware.bounds, aware.bounds);
    }

    #[test]
    fn exclude_pid_hides_this_process_and_only_it() {
        let title = unique_title("exclude");
        let _w = TestWindow::open(Options::new(&title));
        assert!(wait_for(|| find(&title).is_some()));
        let me = std::process::id();
        let others = WinWindowSource::new().list(Some(me)).unwrap();
        assert!(others.iter().all(|w| w.title != title));
        // Nothing of ours is left, but the desktop still has other windows.
        let source = WinWindowSource::new();
        let all = source.list(None).unwrap();
        assert!(all.len() >= others.len());
    }

    #[test]
    fn the_list_is_in_z_order_topmost_first() {
        let (top_title, under_title) = (unique_title("top"), unique_title("under"));
        let top = TestWindow::open(Options::new(&top_title));
        let under = TestWindow::open(Options::new(&under_title));
        under.place_below(&top);
        let order = |list: &[NativeWindow]| {
            let t = list.iter().position(|w| w.title == top_title);
            let u = list.iter().position(|w| w.title == under_title);
            matches!((t, u), (Some(t), Some(u)) if t < u)
        };
        assert!(wait_for(|| order(&WinWindowSource::new().list(None).unwrap())));
    }

    #[test]
    fn tool_windows_are_listed_like_the_script_does() {
        let title = unique_title("tool");
        let mut options = Options::new(&title);
        options.tool_window = true;
        let _w = TestWindow::open(options);
        assert!(wait_for(|| find(&title).is_some()));
    }

    #[test]
    fn cloaked_windows_are_left_out() {
        let title = unique_title("cloak");
        let w = TestWindow::open(Options::new(&title));
        assert!(wait_for(|| find(&title).is_some()));
        let cloak = BOOL::from(true);
        // SAFETY: our own window, and the attribute takes a BOOL of exactly this size.
        unsafe {
            DwmSetWindowAttribute(
                w.hwnd(),
                DWMWA_CLOAK,
                std::ptr::from_ref(&cloak).cast(),
                size_of::<BOOL>() as u32,
            )
        }
        .unwrap();
        assert!(wait_for(|| find(&title).is_none()), "a cloaked window is not listed");
    }

    #[test]
    fn a_minimized_window_is_flagged_without_bounds_and_cannot_be_captured() {
        let title = unique_title("min");
        let w = TestWindow::open(Options::new(&title));
        w.minimize();
        assert!(wait_for(|| find(&title).is_some_and(|f| f.minimized)), "listed as minimized");
        let found = find(&title).unwrap();
        assert_eq!(found.bounds, None, "no true bounds while minimized");
        let source = WinWindowSource::new();
        for method in [WindowCaptureMethod::PrintWindow, WindowCaptureMethod::GraphicsCapture] {
            let err = source.capture(w.id(), method).unwrap_err().to_string();
            assert_eq!(err, "window is minimized");
        }
    }

    #[test]
    fn small_and_untitled_windows_are_left_out() {
        let small_title = unique_title("small");
        let mut small = Options::new(&small_title);
        small.width = 40;
        small.height = 40;
        let _small = TestWindow::open(small);
        let empty = TestWindow::open(Options::new(""));
        let reference = unique_title("reference");
        let _reference = TestWindow::open(Options::new(&reference));
        assert!(wait_for(|| find(&reference).is_some()));
        let list = WinWindowSource::new().list(None).unwrap();
        assert!(list.iter().all(|w| w.title != small_title), "smaller than 48 px");
        assert!(list.iter().all(|w| w.id != empty.id()), "no title");
    }

    #[test]
    fn long_and_non_ascii_titles_arrive_whole() {
        let mut title = unique_title("Заголовок 🙂");
        title.push_str(&"x".repeat(900));
        let _w = TestWindow::open(Options::new(&title));
        assert!(wait_for(|| find(&title).is_some()), "the whole title, {} chars", title.len());
    }

    // ----- capture -----

    fn check_method(method: WindowCaptureMethod) {
        let title = unique_title("capture");
        let w = TestWindow::open(Options::new(&title));
        assert!(wait_for(|| find(&title).is_some()));
        let bounds = find(&title).unwrap().bounds.unwrap();
        let f = capture_painted(w.id(), method, (40, 30, 200));
        assert_content(&f, bounds);
    }

    #[test]
    fn printwindow_reads_an_off_screen_window() {
        check_method(WindowCaptureMethod::PrintWindow);
    }

    #[test]
    fn graphics_capture_reads_an_off_screen_window() {
        check_method(WindowCaptureMethod::GraphicsCapture);
    }

    fn check_covered(method: WindowCaptureMethod) {
        let (target_title, cover_title) = (unique_title("covered"), unique_title("cover"));
        let target = TestWindow::open(Options::new(&target_title));
        let mut cover_options = Options::new(&cover_title);
        cover_options.background = (10, 180, 240);
        cover_options.patch = None;
        let cover = TestWindow::open(cover_options);
        target.place_below(&cover);
        assert!(wait_for(|| find(&target_title).is_some() && find(&cover_title).is_some()));
        let list = WinWindowSource::new().list(None).unwrap();
        let (t, c) = (
            list.iter().position(|w| w.id == target.id()).unwrap(),
            list.iter().position(|w| w.id == cover.id()).unwrap(),
        );
        assert!(c < t, "the cover really is above the target");
        let target_bounds = list[t].bounds.unwrap();
        let cover_bounds = list[c].bounds.unwrap();
        assert_eq!(
            (target_bounds.x, target_bounds.y, target_bounds.width, target_bounds.height),
            (cover_bounds.x, cover_bounds.y, cover_bounds.width, cover_bounds.height),
            "fully covered"
        );
        // The target's own colour comes back, not the window on top of it.
        let f = capture_painted(target.id(), method, (40, 30, 200));
        assert_content(&f, target_bounds);
    }

    #[test]
    fn printwindow_reads_a_covered_window() {
        check_covered(WindowCaptureMethod::PrintWindow);
    }

    #[test]
    fn graphics_capture_reads_a_covered_window() {
        check_covered(WindowCaptureMethod::GraphicsCapture);
    }

    #[test]
    fn a_tool_window_can_be_captured_with_both_methods() {
        for method in [WindowCaptureMethod::PrintWindow, WindowCaptureMethod::GraphicsCapture] {
            let title = unique_title("toolcap");
            let mut options = Options::new(&title);
            options.tool_window = true;
            let w = TestWindow::open(options);
            assert!(wait_for(|| find(&title).is_some()));
            let f = capture_painted(w.id(), method, (40, 30, 200));
            assert!(f.width() >= 48);
        }
    }

    #[test]
    fn a_window_that_is_gone_is_an_explicit_error() {
        let title = unique_title("gone");
        let w = TestWindow::open(Options::new(&title));
        let id = w.id();
        drop(w);
        let source = WinWindowSource::new();
        for method in [WindowCaptureMethod::PrintWindow, WindowCaptureMethod::GraphicsCapture] {
            let err = source.capture(id, method).unwrap_err().to_string();
            assert_eq!(err, format!("hwnd {id} is not a window"));
        }
    }

    /// The list must not block on a window whose thread is stuck, and a capture must come back either way.
    #[test]
    fn a_hung_window_blocks_neither_the_list_nor_forever_a_capture() {
        let title = unique_title("hung");
        let w = TestWindow::open(Options::new(&title));
        assert!(wait_for(|| find(&title).is_some()));
        w.stall(Duration::from_millis(1500));
        std::thread::sleep(Duration::from_millis(100));

        let t = Instant::now();
        assert!(find(&title).is_some(), "still listed by its stored title");
        assert!(t.elapsed() < Duration::from_millis(500), "the list took {:?}", t.elapsed());

        let t = Instant::now();
        let result = print_window_with_timeout(w.id(), Duration::from_millis(400));
        let waited = t.elapsed();
        assert!(waited < Duration::from_millis(1000), "the timeout holds, waited {waited:?}");
        if let Err(e) = result {
            assert!(e.to_string().contains("did not answer"), "{e}");
        }
    }

    #[test]
    fn process_name_finds_this_process_and_not_a_bogus_one() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(process_name(std::process::id()).as_deref(), exe.file_stem().and_then(|s| s.to_str()));
        assert_eq!(process_name(0xFFFF_FFF0), None);
    }
}
