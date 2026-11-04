//! The UI thread and native window control. Slint windows live on the main thread; everything else
//! posts to it through these helpers.

use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::thread::ThreadId;
use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::{Map, Value};
use shotik_core::ports::{AppHost, Theme};
use shotik_core::{Error, Result};
use slint::{PhysicalPosition, PhysicalSize};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOP, HWND_TOPMOST, MA_NOACTIVATE,
    STYLESTRUCT, SWP_FRAMECHANGED, SWP_NOACTIVATE, WM_MOUSEACTIVATE, WM_NCDESTROY, WM_STYLECHANGING, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

/// Ghost mode keeps every window this far to the right, in pixels and in DIP.
pub const GHOST_OFFSET: i32 = 20000;

/// Facts about this run the windows need.
#[derive(Clone, Debug)]
pub struct UiEnv {
    pub ghost: bool,
    /// `SHOTIK_POPUP_REAL`: the translate popup uses the focusable show path even in ghost mode.
    pub popup_real: bool,
    /// `SHOTIK_TP_TTL`: auto-hide of the translate popup, default 9000.
    pub translate_popup_ttl_ms: u64,
}

struct Context {
    env: UiEnv,
    ui_thread: ThreadId,
    host: OnceLock<Arc<dyn AppHost>>,
}

static CONTEXT: OnceLock<Context> = OnceLock::new();

fn context() -> &'static Context {
    CONTEXT.get().expect("shotik_ui::Ui::new runs before any window")
}

pub(crate) fn init(env: UiEnv) -> Result<()> {
    let context = Context { env, ui_thread: std::thread::current().id(), host: OnceLock::new() };
    CONTEXT.set(context).map_err(|_| Error::msg("the UI was initialized twice"))
}

pub(crate) fn set_host(host: Arc<dyn AppHost>) {
    let _ = context().host.set(host);
}

pub fn env() -> &'static UiEnv {
    &context().env
}

/// What windows ask the application for.
pub fn host() -> &'static Arc<dyn AppHost> {
    context().host.get().expect("Ui::set_host runs before the event loop")
}

/// A translated string from the application's dictionary. An unknown key shows the key, as the JS does.
pub fn tr(key: &str) -> slint::SharedString {
    dictionary().get(key).and_then(Value::as_str).unwrap_or(key).into()
}

/// The dictionary for the current language.
pub fn dictionary() -> Map<String, Value> {
    match host().i18n().get("dict") {
        Some(Value::Object(dict)) => dict.clone(),
        _ => Map::new(),
    }
}

/// Runs on the UI thread, later. Returns at once.
pub fn on_ui(work: impl FnOnce() + Send + 'static) {
    if let Err(e) = slint::invoke_from_event_loop(work) {
        eprintln!("the UI event loop is gone: {e}");
    }
}

/// Runs on the UI thread and returns the result. From the UI thread itself it runs directly. Other
/// threads block until it is done, so never hold a lock the UI thread may want while calling this.
pub fn on_ui_wait<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    if std::thread::current().id() == context().ui_thread {
        return Ok(work());
    }
    let (done, result) = mpsc::channel();
    slint::invoke_from_event_loop(move || {
        let _ = done.send(work());
    })
    .map_err(|e| Error::msg(format!("the UI event loop is gone: {e}")))?;
    result
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| Error::msg("the UI thread did not answer in 30 s"))
}

/// `win.focus()`: the keyboard goes to this window. Not winit's `focus_window`, which presses and releases a
/// fake Alt key to get the foreground right: that input lands in whatever application has the keyboard, where
/// Alt opens a menu. Sharing the foreground thread's input state for a moment gets the same right with no input.
pub fn take_focus(hwnd: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };
    // SAFETY: plain calls on a live window; the thread attachment is undone before returning.
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        let foreground = GetForegroundWindow();
        let their_thread = GetWindowThreadProcessId(foreground, None);
        let my_thread = GetCurrentThreadId();
        let attached = their_thread != 0 && their_thread != my_thread && AttachThreadInput(my_thread, their_thread, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let taken = SetForegroundWindow(hwnd).as_bool();
        if attached {
            let _ = AttachThreadInput(my_thread, their_thread, false);
        }
        if !taken {
            eprintln!("Windows refused the keyboard focus; a click on the window takes it");
        }
    }
}

pub fn hwnd_of(window: &slint::Window) -> Result<HWND> {
    let handle = window.window_handle();
    let handle = handle.window_handle().map_err(|e| Error::msg(format!("no native window yet: {e}")))?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        _ => Err(Error::msg("not a Win32 window")),
    }
}

const NO_ACTIVATE_SUBCLASS: usize = 0x5348_4f54;
const DPI_SUBCLASS: usize = 0x5348_4f44;

thread_local! {
    /// The rectangle Windows suggested, while its `WM_DPICHANGED` is being handled further down the chain.
    static DPI_RECT: std::cell::Cell<Option<windows::Win32::Foundation::RECT>> = const { std::cell::Cell::new(None) };
}

/// Winit answers `WM_DPICHANGED` with a rectangle of its own, moved so the cursor keeps its place in the title
/// bar. That can put the window's centre back on the monitor it just left: Windows changes the DPI again, and
/// its move loop scales the size each time, so the window grows while it is dragged. Windows' suggested
/// rectangle is computed not to do that, and it is what the documentation and Chromium use. Winit still
/// handles the message, because it reports the new scale; only the rectangle it applies is replaced.
unsafe extern "system" fn dpi_subclass(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_DPICHANGED, WM_WINDOWPOSCHANGING};
    // SAFETY: Win32 owns the message parameters for this call: for these messages lparam points to a RECT
    // and a WINDOWPOS owned by the sender, and the rest is forwarded as received.
    unsafe {
        match message {
            WM_DPICHANGED => {
                DPI_RECT.set(Some(*(lparam.0 as *const windows::Win32::Foundation::RECT)));
                let result = DefSubclassProc(hwnd, message, wparam, lparam);
                DPI_RECT.set(None);
                return result;
            }
            WM_WINDOWPOSCHANGING => {
                if let Some(rect) = DPI_RECT.get() {
                    let pos = &mut *(lparam.0 as *mut WINDOWPOS);
                    if !pos.flags.contains(SWP_NOMOVE) {
                        pos.x = rect.left;
                        pos.y = rect.top;
                    }
                    if !pos.flags.contains(SWP_NOSIZE) {
                        pos.cx = rect.right - rect.left;
                        pos.cy = rect.bottom - rect.top;
                    }
                }
            }
            WM_NCDESTROY => {
                let _ = RemoveWindowSubclass(hwnd, Some(dpi_subclass), DPI_SUBCLASS);
            }
            _ => {}
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

/// For a window the user can move between monitors.
pub fn follow_suggested_dpi_rect(hwnd: HWND) -> Result<()> {
    // SAFETY: `hwnd` is a live window of this thread and the subclass proc outlives it.
    unsafe { SetWindowSubclass(hwnd, Some(dpi_subclass), DPI_SUBCLASS, 0) }
        .ok()
        .map_err(|e| Error::msg(format!("cannot subclass a window for DPI changes: {e}")))
}

/// Winit recomputes the extended style when decorations or visibility change, which would drop
/// no-activate and show the window in the taskbar. Keep them in the style transition itself.
unsafe extern "system" fn no_activate_subclass(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_STYLECHANGING && wparam.0 as i32 == GWL_EXSTYLE.0 {
        // SAFETY: for WM_STYLECHANGING, lparam points to a STYLESTRUCT owned by the sender.
        let styles = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
        styles.styleNew = (styles.styleNew | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) & !WS_EX_APPWINDOW.0;
    }
    if message == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    if message == WM_NCDESTROY {
        // SAFETY: removing our own subclass from the window being destroyed.
        let _ = unsafe { RemoveWindowSubclass(window, Some(no_activate_subclass), NO_ACTIVATE_SUBCLASS) };
    }
    // SAFETY: forwarding the message this subclass received, with its own arguments.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

/// How a window sits on the desktop. Coordinates are physical pixels, before the ghost offset.
#[derive(Clone, Copy, Debug, Default)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub topmost: bool,
    /// Never takes focus and stays out of the taskbar. Always on in ghost mode, so nothing a test does
    /// can reach the user's desktop or keyboard focus.
    pub no_activate: bool,
    /// Mouse goes through to what is underneath.
    pub click_through: bool,
    /// Windows 11 rounded corners with the native shadow, which software rendering does not draw itself.
    pub rounded: bool,
    /// The display's scale. Off-screen windows all report the nearest monitor's DPI, so ghost mode has
    /// to tell Slint the real one.
    pub scale: f32,
}

/// Applies a placement. The HWND must exist: call after `show()`.
pub fn place(window: &slint::Window, placement: Placement) -> Result<()> {
    let ghost = env().ghost;
    let x = placement.x + if ghost { GHOST_OFFSET } else { 0 };
    let no_activate = placement.no_activate || ghost;
    let hwnd = hwnd_of(window)?;
    window.set_position(PhysicalPosition::new(x, placement.y));
    window.set_size(PhysicalSize::new(placement.width, placement.height));
    // SAFETY: `hwnd` is a live window owned by this thread, and the subclass proc outlives it.
    unsafe {
        let mut ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if no_activate {
            SetWindowSubclass(hwnd, Some(no_activate_subclass), NO_ACTIVATE_SUBCLASS, 0)
                .ok()
                .map_err(|e| Error::msg(format!("cannot make a window non-activating: {e}")))?;
            ex |= WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
        }
        if placement.click_through {
            ex |= WS_EX_TRANSPARENT.0 as isize;
        }
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
        if placement.rounded {
            let corner = DWMWCP_ROUND;
            // A failure only means square corners, for example on Windows 10.
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                std::ptr::from_ref(&corner).cast(),
                std::mem::size_of_val(&corner) as u32,
            );
        }
        let after = if placement.topmost { HWND_TOPMOST } else { HWND_TOP };
        SetWindowPos(
            hwnd,
            Some(after),
            x,
            placement.y,
            placement.width as i32,
            placement.height as i32,
            SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )
        .map_err(|e| Error::msg(format!("cannot place a window: {e}")))?;
    }
    if ghost && placement.scale > 0.0 {
        window
            .dispatch_event_with_result(slint::platform::WindowEvent::ScaleFactorChanged {
                scale_factor: placement.scale,
            })
            .map_err(|e| Error::msg(format!("cannot set the scale: {e}")))?;
    }
    Ok(())
}

/// Renders a window to a PNG in-process, off-screen, without the desktop: the `capture-page` route.
pub fn snapshot_png(window: &slint::Window, path: &std::path::Path) -> Result<(u32, u32)> {
    let shot = window.take_snapshot().map_err(|e| Error::msg(format!("cannot render a window: {e}")))?;
    let (width, height) = (shot.width(), shot.height());
    let mut bgra = Vec::with_capacity(width as usize * height as usize * 4);
    for px in shot.as_slice() {
        bgra.extend_from_slice(&[px.b, px.g, px.r, px.a]);
    }
    let png = shotik_core::Frame::new(width, height, bgra)?.to_png()?;
    std::fs::write(path, png).map_err(|e| Error::io("write window image", path, e))?;
    Ok((width, height))
}

/// Parses "#RRGGBB".
pub fn color_of(hex: &str) -> slint::Color {
    let value = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x0067C0);
    slint::Color::from_rgb_u8((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

/// Dark text on a light accent, as `overlay.js` and `app.js` compute it.
pub fn accent_text(hex: &str) -> slint::Color {
    let c = color_of(hex);
    let luminance = 0.299 * f32::from(c.red()) + 0.587 * f32::from(c.green()) + 0.114 * f32::from(c.blue());
    if luminance > 160.0 {
        slint::Color::from_rgb_u8(0x1b, 0x1b, 0x1b)
    } else {
        slint::Color::from_rgb_u8(0xff, 0xff, 0xff)
    }
}

/// Sets a generated component's `Theme` global. Every window calls it on creation and on theme change.
#[macro_export]
macro_rules! apply_theme {
    ($component:expr, $theme:expr) => {{
        use slint::ComponentHandle as _;
        let global = $component.global::<$crate::Theme>();
        global.set_dark($theme.dark);
        global.set_accent($crate::runtime::color_of(&$theme.accent));
        global.set_accent_text($crate::runtime::accent_text(&$theme.accent));
    }};
}

/// The theme the application reports right now.
pub fn current_theme() -> Theme {
    host().theme()
}

// Small-window support is additive: no renderer/window state is shared with the overlay/main tasks.

/// Monotonic milliseconds for the pure small-window states.
pub fn now_ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

#[cfg(test)]
pub(crate) mod test_clock {
    #[derive(Default)]
    pub struct FakeClock(std::cell::Cell<u64>);
    impl FakeClock {
        pub fn now(&self) -> u64 {
            self.0.get()
        }
        pub fn advance(&self, ms: u64) -> u64 {
            self.0.set(self.now() + ms);
            self.now()
        }
    }
}

/// Void UiPort methods cannot return an error. Keep failures explicit in stderr.
pub fn report(result: Result<()>) {
    if let Err(error) = result {
        eprintln!("small window: {error}");
    }
}

/// Decode without Slint's path cache: a closed/hidden window must release all its image pixels.
pub fn uncached_image(path: &std::path::Path) -> Result<slint::Image> {
    let image = image::ImageReader::open(path)
        .map_err(|e| Error::io("read window image", path, e))?
        .decode()
        .map_err(|e| Error::msg(format!("cannot decode window image {}: {e}", path.display())))?
        .into_rgba8();
    if image.width() == 0 || image.height() == 0 {
        return Err(Error::msg("window image has no pixels"));
    }
    Ok(slint::Image::from_rgba8(
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(image.as_raw(), image.width(), image.height()),
    ))
}

/// Slint 1.18 creates HWNDs at the next event-loop boundary. Await them while they are still hidden,
/// then install styles/regions before show. The temporary winit Arc is dropped before the callback.
pub fn after_native<C: slint::ComponentHandle + 'static>(component: C, work: impl FnOnce(C) -> Result<()> + 'static) {
    use slint::winit_030::WinitWindowAccessor;
    if let Err(error) = slint::spawn_local(async move {
        let result = async {
            component
                .window()
                .winit_window()
                .await
                .map_err(|e| Error::msg(format!("cannot create a native small window: {e}")))?;
            work(component)
        }
        .await;
        report(result);
    }) {
        eprintln!("cannot schedule a small window: {error}");
    }
}

pub fn same_window<C: slint::ComponentHandle>(a: &C, b: &C) -> bool {
    std::ptr::eq(a.window(), b.window())
}

/// Bounds returned to test routes are global DIP, including exactly one ghost offset.
pub fn ghost_bounds(mut bounds: shotik_core::Rect) -> shotik_core::Rect {
    if env().ghost {
        bounds.x += GHOST_OFFSET;
    }
    bounds
}

/// Show only after native placement and no-activate styles are installed (also on the first frame).
pub fn show_small(
    window: &slint::Window,
    bounds: shotik_core::Rect,
    display: &shotik_core::Display,
    rounded: bool,
    no_activate: bool,
    click_through: bool,
) -> Result<()> {
    prepare_small(window, bounds, display, rounded, no_activate, click_through)?;
    window
        .show()
        .map_err(|e| Error::msg(format!("cannot show a small window: {e}")))
}

/// Configure a hidden window before applying its region and showing the first frame.
pub fn prepare_small(
    window: &slint::Window,
    bounds: shotik_core::Rect,
    display: &shotik_core::Display,
    rounded: bool,
    no_activate: bool,
    click_through: bool,
) -> Result<()> {
    let scale = display.scale_factor;
    let physical = dip_to_physical(bounds, display);
    // window_handle() lazily creates the hidden HWND using Ui's attributes hook.
    place(
        window,
        Placement {
            x: physical.x,
            y: physical.y,
            width: physical.width.max(1) as u32,
            height: physical.height.max(1) as u32,
            topmost: true,
            rounded,
            no_activate,
            click_through,
            scale: scale as f32,
        },
    )?;
    keep_small_styles(window, 0)?;
    if click_through {
        small_layered(window, 255, true)?;
    }
    // Windows created off-screen report the nearest monitor DPI. Keep layout in the source DIP.
    if env().ghost {
        window
            .dispatch_event_with_result(slint::platform::WindowEvent::Resized {
                size: slint::LogicalSize::new(bounds.width as f32, bounds.height as f32),
            })
            .map_err(|e| Error::msg(format!("cannot size a ghost window: {e}")))?;
    }
    Ok(())
}

const SMALL_STYLE_SUBCLASS: usize = 0x5348_534d;
fn small_ex_style(style: u32, flags: u32) -> u32 {
    (style | flags | WS_EX_TOOLWINDOW.0) & !WS_EX_APPWINDOW.0
}
unsafe extern "system" fn small_style_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    flags: usize,
) -> LRESULT {
    if message == WM_STYLECHANGING && wparam.0 as i32 == GWL_EXSTYLE.0 {
        // SAFETY: WM_STYLECHANGING carries the sender's live STYLESTRUCT.
        let styles = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
        styles.styleNew = small_ex_style(styles.styleNew, flags as u32);
    }
    if message == WM_NCDESTROY {
        // SAFETY: remove only our own subclass from our own dying window.
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(small_style_subclass), SMALL_STYLE_SUBCLASS) };
    }
    // SAFETY: forward this window's message unchanged.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

fn keep_small_styles(window: &slint::Window, extra: u32) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED;
    let hwnd = hwnd_of(window)?;
    // SAFETY: this process owns the HWND; only the taskbar/layering bits of its style are changed.
    unsafe {
        let previous = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let flags = (previous & (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0)) | extra;
        SetWindowSubclass(hwnd, Some(small_style_subclass), SMALL_STYLE_SUBCLASS, flags as usize)
            .ok()
            .map_err(|e| Error::msg(format!("cannot retain small-window styles: {e}")))?;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, small_ex_style(previous, flags) as isize);
        let taskbar_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & (WS_EX_TOOLWINDOW.0 | WS_EX_APPWINDOW.0);
        if taskbar_style != WS_EX_TOOLWINDOW.0 {
            return Err(Error::msg("cannot hide a small window from the taskbar"));
        }
    }
    Ok(())
}

/// WS_EX_TRANSPARENT alone changes painting order; layered+transparent also passes hit tests through.
pub fn small_layered(window: &slint::Window, alpha: u8, click_through: bool) -> Result<()> {
    use windows::Win32::{
        Foundation::COLORREF,
        UI::WindowsAndMessaging::{SetLayeredWindowAttributes, LWA_ALPHA, WS_EX_LAYERED},
    };
    let flags = WS_EX_LAYERED.0 | if click_through { WS_EX_TRANSPARENT.0 } else { 0 };
    keep_small_styles(window, flags)?;
    let hwnd = hwnd_of(window)?;
    // SAFETY: live HWND owned by this process; the subclass retains these flags across winit updates.
    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA) }
        .map_err(|e| Error::msg(format!("cannot set window alpha: {e}")))
}

pub fn dip_to_physical(bounds: shotik_core::Rect, display: &shotik_core::Display) -> shotik_core::Rect {
    let scale = display.scale_factor;
    shotik_core::Rect::new(
        display.physical_bounds.x + (f64::from(bounds.x - display.bounds.x) * scale).round() as i32,
        display.physical_bounds.y + (f64::from(bounds.y - display.bounds.y) * scale).round() as i32,
        (f64::from(bounds.width) * scale).round() as i32,
        (f64::from(bounds.height) * scale).round() as i32,
    )
}

/// Native placement may change while dragging. Ghost windows retain their supplied source monitor.
pub fn small_bounds(window: &slint::Window, display: &shotik_core::Display) -> Result<shotik_core::Rect> {
    small_geometry(window, display).map(|(bounds, _)| bounds)
}

pub fn small_geometry(
    window: &slint::Window,
    source: &shotik_core::Display,
) -> Result<(shotik_core::Rect, shotik_core::Display)> {
    use windows::Win32::{Foundation::RECT, UI::WindowsAndMessaging::GetWindowRect};
    let mut rect = RECT::default();
    // SAFETY: live HWND, writable RECT.
    unsafe { GetWindowRect(hwnd_of(window)?, &mut rect) }
        .map_err(|e| Error::msg(format!("cannot read window bounds: {e}")))?;
    let display = if env().ghost {
        rect.left -= GHOST_OFFSET;
        rect.right -= GHOST_OFFSET;
        source.clone()
    } else {
        let center = shotik_core::PointF {
            x: f64::from(rect.left) + f64::from(rect.right - rect.left) / 2.0,
            y: f64::from(rect.top) + f64::from(rect.bottom - rect.top) / 2.0,
        };
        nearest_physical(small_displays()?, center)?
    };
    let scale = display.scale_factor;
    let bounds = shotik_core::Rect::new(
        display.bounds.x + (f64::from(rect.left - display.physical_bounds.x) / scale).round() as i32,
        display.bounds.y + (f64::from(rect.top - display.physical_bounds.y) / scale).round() as i32,
        (f64::from(rect.right - rect.left) / scale).round() as i32,
        (f64::from(rect.bottom - rect.top) / scale).round() as i32,
    );
    Ok((bounds, display))
}

/// Make the software-rendered bubble truly circular and the recording frame truly hollow.
pub enum SmallRegion {
    Bubble,
    Frame { thickness: i32 },
}

pub fn small_region(window: &slint::Window, region: SmallRegion, scale: f64) -> Result<()> {
    use windows::Win32::Graphics::Gdi::{
        CombineRgn, CreateEllipticRgn, CreateRectRgn, DeleteObject, SetWindowRgn, RGN_DIFF,
    };
    let size = window.size();
    let hwnd = hwnd_of(window)?;
    let (width, height) = (size.width as i32, size.height as i32);
    // SAFETY: GDI regions are owned locally until SetWindowRgn succeeds, then the OS owns the outer one.
    unsafe {
        let outer = match region {
            SmallRegion::Bubble => {
                let margin = (14.0 * scale).round() as i32;
                CreateEllipticRgn(margin, margin, width - margin, height - margin)
            }
            SmallRegion::Frame { .. } => CreateRectRgn(0, 0, width, height),
        };
        if outer.is_invalid() {
            return Err(Error::msg("cannot allocate a window region"));
        }
        if let SmallRegion::Frame { thickness } = region {
            let t = (f64::from(thickness) * scale).round() as i32;
            let inner = CreateRectRgn(t, t, width - t, height - t);
            if inner.is_invalid() {
                let _ = DeleteObject(outer.into());
                return Err(Error::msg("cannot allocate the frame hole"));
            }
            let combined = CombineRgn(Some(outer), Some(outer), Some(inner), RGN_DIFF);
            let _ = DeleteObject(inner.into());
            if combined.0 == 0 {
                let _ = DeleteObject(outer.into());
                return Err(Error::msg("cannot cut the frame hole"));
            }
        }
        if SetWindowRgn(hwnd, Some(outer), true) == 0 {
            let _ = DeleteObject(outer.into());
            return Err(Error::msg("cannot apply a window region"));
        }
    }
    Ok(())
}

pub fn exclude_capture(window: &slint::Window) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE};
    // SAFETY: this process owns the live HWND.
    unsafe { SetWindowDisplayAffinity(hwnd_of(window)?, WDA_EXCLUDEFROMCAPTURE) }
        .map_err(|e| Error::msg(format!("cannot exclude recording chrome from capture: {e}")))
}

/// Start the platform's ordinary drag loop. Ghost tests only use semantic/local input.
pub fn drag_small(window: &slint::Window) -> Result<()> {
    use slint::winit_030::WinitWindowAccessor;
    if env().ghost {
        return Ok(());
    }
    window
        .with_winit_window(|w| w.drag_window())
        .ok_or_else(|| Error::msg("no native window for drag"))?
        .map_err(|e| Error::msg(format!("cannot drag a window: {e}")))
}

/// The reference's focusable show path. Ghost mode stops before every OS focus call.
pub fn activate_small(window: &slint::Window) -> Result<()> {
    if env().ghost {
        return Ok(());
    }
    // SAFETY: read the extended style of our own live HWND.
    let style = unsafe { GetWindowLongPtrW(hwnd_of(window)?, GWL_EXSTYLE) } as u32;
    if style & WS_EX_NOACTIVATE.0 != 0 {
        return Err(Error::msg("cannot activate a non-activating window"));
    }
    take_focus(hwnd_of(window)?);
    Ok(())
}

#[derive(Clone, Debug)]
struct NativeMonitor {
    bounds: shotik_core::Rect,
    work: shotik_core::Rect,
    scale: f64,
    primary: bool,
}

/// Chromium-compatible attached-monitor DIP coordinates, including the mixed-DPI vector in CONTRACT.
fn monitor_layout(monitors: &[NativeMonitor]) -> Result<Vec<shotik_core::Display>> {
    use shotik_core::{Display, Rect};
    if monitors.is_empty() {
        return Err(Error::msg("no monitors"));
    }
    let primary = monitors
        .iter()
        .position(|m| m.primary)
        .ok_or_else(|| Error::msg("no primary monitor"))?;
    let mut placed = vec![None; monitors.len()];
    let size = |m: &NativeMonitor| {
        (
            (f64::from(m.bounds.width) / m.scale).round() as i32,
            (f64::from(m.bounds.height) / m.scale).round() as i32,
        )
    };
    let (w, h) = size(&monitors[primary]);
    placed[primary] = Some(Rect::new(0, 0, w, h));
    for _ in 1..monitors.len() {
        let mut progress = false;
        for (index, child) in monitors.iter().enumerate() {
            if placed[index].is_some() {
                continue;
            }
            for (parent_index, parent) in monitors.iter().enumerate() {
                let Some(p) = placed[parent_index] else {
                    continue;
                };
                let (c, b) = (child.bounds, parent.bounds);
                let (cw, ch) = size(child);
                let offset_x = (f64::from(c.x - b.x) / parent.scale).floor() as i32;
                let offset_y = (f64::from(c.y - b.y) / parent.scale).floor() as i32;
                let vertical_overlap = c.y < b.y + b.height && b.y < c.y + c.height;
                let horizontal_overlap = c.x < b.x + b.width && b.x < c.x + c.width;
                let origin = if c.x == b.x + b.width && vertical_overlap {
                    Some((p.x + p.width, p.y + offset_y))
                } else if c.x + c.width == b.x && vertical_overlap {
                    Some((p.x - cw, p.y + offset_y))
                } else if c.y == b.y + b.height && horizontal_overlap {
                    Some((p.x + offset_x, p.y + p.height))
                } else if c.y + c.height == b.y && horizontal_overlap {
                    Some((p.x + offset_x, p.y - ch))
                } else {
                    None
                };
                if let Some((x, y)) = origin {
                    placed[index] = Some(Rect::new(x, y, cw, ch));
                    progress = true;
                    break;
                }
            }
        }
        if !progress {
            break;
        }
    }
    monitors
        .iter()
        .enumerate()
        .map(|(index, m)| {
            let bounds = placed[index].ok_or_else(|| Error::msg("cannot map a detached monitor into global DIP"))?;
            let work_area = Rect::new(
                bounds.x + (f64::from(m.work.x - m.bounds.x) / m.scale).round() as i32,
                bounds.y + (f64::from(m.work.y - m.bounds.y) / m.scale).round() as i32,
                (f64::from(m.work.width) / m.scale).round() as i32,
                (f64::from(m.work.height) / m.scale).round() as i32,
            );
            Ok(Display {
                id: index as u64,
                bounds,
                work_area,
                scale_factor: m.scale,
                physical_bounds: m.bounds,
                primary: m.primary,
            })
        })
        .collect()
}

fn small_displays() -> Result<Vec<shotik_core::Display>> {
    use windows::core::BOOL;
    use windows::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO},
        UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
    };
    struct Enumeration {
        monitors: Vec<NativeMonitor>,
        error: Option<String>,
    }
    unsafe extern "system" fn visit(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: EnumDisplayMonitors synchronously supplies our Enumeration pointer.
        let data = unsafe { &mut *(data.0 as *mut Enumeration) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let (mut dx, mut dy) = (0, 0);
        // SAFETY: valid monitor from the enumeration and writable output structures.
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            data.error = Some("cannot read monitor geometry".into());
            return BOOL(0);
        }
        if let Err(e) = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } {
            data.error = Some(format!("cannot read monitor DPI: {e}"));
            return BOOL(0);
        }
        let rect = |r: RECT| shotik_core::Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top);
        data.monitors.push(NativeMonitor {
            bounds: rect(info.rcMonitor),
            work: rect(info.rcWork),
            scale: f64::from(dx) / 96.0,
            primary: info.dwFlags & 1 != 0,
        });
        BOOL(1)
    }
    let mut data = Enumeration {
        monitors: Vec::new(),
        error: None,
    };
    // SAFETY: synchronous enumeration with callback data kept alive on this stack.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(visit),
            LPARAM((&mut data as *mut Enumeration) as isize),
        )
    };
    if let Some(error) = data.error {
        return Err(Error::msg(error));
    }
    if !ok.as_bool() {
        return Err(Error::msg("cannot enumerate monitors"));
    }
    monitor_layout(&data.monitors)
}

fn distance(p: shotik_core::PointF, r: shotik_core::Rect) -> f64 {
    let x = (p.x - f64::from(r.x)).min(0.0).abs() + (p.x - f64::from(r.x + r.width)).max(0.0);
    let y = (p.y - f64::from(r.y)).min(0.0).abs() + (p.y - f64::from(r.y + r.height)).max(0.0);
    x * x + y * y
}
fn nearest_physical(displays: Vec<shotik_core::Display>, point: shotik_core::PointF) -> Result<shotik_core::Display> {
    displays
        .into_iter()
        .min_by(|a, b| distance(point, a.physical_bounds).total_cmp(&distance(point, b.physical_bounds)))
        .ok_or_else(|| Error::msg("no monitor near the window"))
}

/// Read monitor geometry/cursor only, never desktop pixels. `at` is global DIP, not a device point.
pub fn small_screen(at: Option<shotik_core::PointF>) -> Result<(shotik_core::Display, shotik_core::PointF)> {
    use windows::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};
    let displays = small_displays()?;
    if let Some(at) = at {
        let display = displays
            .into_iter()
            .min_by(|a, b| distance(at, a.bounds).total_cmp(&distance(at, b.bounds)))
            .ok_or_else(|| Error::msg("no monitor near the selection"))?;
        return Ok((display, at));
    }
    let mut cursor = POINT::default();
    // SAFETY: writable POINT; reading pointer position does not send any input.
    unsafe { GetCursorPos(&mut cursor) }.map_err(|e| Error::msg(format!("cannot read cursor position: {e}")))?;
    let physical = shotik_core::PointF {
        x: f64::from(cursor.x),
        y: f64::from(cursor.y),
    };
    let display = nearest_physical(displays, physical)?;
    let point = shotik_core::PointF {
        x: f64::from(display.bounds.x) + (physical.x - f64::from(display.physical_bounds.x)) / display.scale_factor,
        y: f64::from(display.bounds.y) + (physical.y - f64::from(display.physical_bounds.y)) / display.scale_factor,
    };
    Ok((display, point))
}

#[cfg(test)]
mod small_tests {
    use super::*;
    use shotik_core::Rect;
    #[test]
    fn attached_mixed_dpi_layout_matches_the_contract() {
        let monitors = [
            (Rect::new(0, 0, 3840, 2160), 1.5, true),
            (Rect::new(3840, 694, 1920, 1080), 1.0, false),
            (Rect::new(-2560, 419, 2560, 1440), 1.0, false),
        ]
        .map(|(bounds, scale, primary)| NativeMonitor {
            bounds,
            work: bounds,
            scale,
            primary,
        });
        let displays = monitor_layout(&monitors).unwrap();
        assert_eq!(displays[0].bounds, Rect::new(0, 0, 2560, 1440));
        assert_eq!(displays[1].bounds, Rect::new(2560, 462, 1920, 1080));
        assert_eq!(displays[2].bounds, Rect::new(-2560, 279, 2560, 1440));
        let dip = Rect::new(20, 20, 320, 180);
        assert_eq!(dip_to_physical(dip, &displays[0]), Rect::new(30, 30, 480, 270));
    }

    #[test]
    fn focusable_small_windows_skip_taskbar_without_forcing_noactivate() {
        use windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED;
        let style = small_ex_style(WS_EX_APPWINDOW.0, 0);
        assert_ne!(style & WS_EX_TOOLWINDOW.0, 0);
        assert_eq!(style & (WS_EX_APPWINDOW.0 | WS_EX_NOACTIVATE.0), 0);
        let style = small_ex_style(
            WS_EX_APPWINDOW.0 | WS_EX_NOACTIVATE.0,
            WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0,
        );
        assert_ne!(style & WS_EX_NOACTIVATE.0, 0);
        assert_eq!(style & WS_EX_APPWINDOW.0, 0);
        assert_ne!(style & WS_EX_LAYERED.0, 0);
        assert_ne!(style & WS_EX_TRANSPARENT.0, 0);
    }
}
