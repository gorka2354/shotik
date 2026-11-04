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

pub fn hwnd_of(window: &slint::Window) -> Result<HWND> {
    let handle = window.window_handle();
    let handle = handle.window_handle().map_err(|e| Error::msg(format!("no native window yet: {e}")))?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        _ => Err(Error::msg("not a Win32 window")),
    }
}

const NO_ACTIVATE_SUBCLASS: usize = 0x5348_4f54;

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
