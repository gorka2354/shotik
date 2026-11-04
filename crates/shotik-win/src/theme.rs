//! System theme and accent colour. Implements `ThemeSource`. Reference: `src/main/theme.js`.
//!
//! `current` reads the same sources Electron does: `AppsUseLightTheme` for dark mode (Chromium's
//! `NativeThemeWin`) and `DwmGetColorizationColor` for the accent (`SystemPreferences::GetAccentColor`).
//! `watch` blocks a dedicated thread in a message loop on a window that is never shown, because
//! `WM_SETTINGCHANGE` and `WM_DWMCOLORIZATIONCOLORCHANGED` are only broadcast to top-level windows. There
//! is no polling: the thread wakes only when the system says something changed.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use shotik_core::ports::{Callback, Theme, ThemeSource};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowLongPtrW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetWindowLongPtrW, TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA,
    MSG, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_DWMCOLORIZATIONCOLORCHANGED, WM_NCCREATE, WM_SETTINGCHANGE,
    WM_SYSCOLORCHANGE, WM_THEMECHANGED, WNDCLASSEXW, WS_POPUP,
};

use crate::displays::win_err;

/// Windows default blue, as in `src/main/theme.js`.
const FALLBACK_ACCENT: &str = "#0067C0";

/// `0xAARRGGBB` to "#RRGGBB": what `'#' + getAccentColor().slice(0, 6).toUpperCase()` yields.
fn accent_from_argb(argb: u32) -> String {
    format!("#{:06X}", argb & 0x00FF_FFFF)
}

/// The accent, or the same fallback `theme.js` uses. The port's signature has no error, and a toolbar in
/// the default blue is the behaviour the reference chose for this failure.
fn read_accent() -> String {
    let (mut color, mut opaque) = (0u32, windows::core::BOOL::default());
    // SAFETY: plain out-parameters.
    match unsafe { DwmGetColorizationColor(&mut color, &mut opaque) } {
        Ok(()) => accent_from_argb(color),
        Err(_) => FALLBACK_ACCENT.to_owned(),
    }
}

/// `AppsUseLightTheme == 0`. A missing value means light, as in Chromium.
fn read_dark() -> bool {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value` is a writable DWORD and `size` says so; both strings are NUL-terminated literals.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut value).cast()),
            Some(&mut size),
        )
    };
    status == ERROR_SUCCESS && value == 0
}

fn read_theme() -> Theme {
    Theme { dark: read_dark(), accent: read_accent(), platform: "win32".to_owned() }
}

/// Does this message mean the theme may have changed?
fn is_theme_message(msg: u32, lparam: LPARAM) -> bool {
    match msg {
        WM_DWMCOLORIZATIONCOLORCHANGED | WM_THEMECHANGED | WM_SYSCOLORCHANGE => true,
        WM_SETTINGCHANGE => {
            if lparam.0 == 0 {
                return false;
            }
            // SAFETY: for WM_SETTINGCHANGE a non-null lParam is a NUL-terminated wide string that lives
            // for the duration of the message.
            let area = unsafe { PCWSTR(lparam.0 as *const u16).to_string() };
            area.is_ok_and(|a| a == "ImmersiveColorSet")
        }
        _ => false,
    }
}

/// Fires the callbacks when the theme really differs from the last one reported, since a single
/// change arrives as several of these messages.
struct Watchers {
    last: Theme,
    callbacks: Vec<Callback<Theme>>,
}

impl Watchers {
    fn on_message(&mut self, read: impl Fn() -> Theme) {
        let now = read();
        if now != self.last {
            self.last = now.clone();
            for callback in &self.callbacks {
                callback(now.clone());
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE lParam is the CREATESTRUCTW, whose lpCreateParams is the pointer to
        // `Mutex<Watchers>` passed to CreateWindowExW. The thread's Arc keeps it alive until it ends.
        unsafe {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    if msg == WM_CLOSE {
        // SAFETY: the window belongs to this thread.
        let _ = unsafe { DestroyWindow(hwnd) };
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        // SAFETY: called on the thread whose message loop should end.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    if is_theme_message(msg, lparam) {
        // SAFETY: GWLP_USERDATA holds the pointer stored above, or null before WM_NCCREATE. The Mutex
        // outlives the window because the owning Arc is held by the thread running this procedure.
        let watchers = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Mutex<Watchers>;
        if let Some(watchers) = unsafe { watchers.as_ref() } {
            if let Ok(mut watchers) = watchers.lock() {
                watchers.on_message(read_theme);
            }
        }
    }
    // SAFETY: the standard default for everything not handled above.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The window thread, started by the first `watch`.
struct WatchThread {
    /// As an integer: an HWND is a plain value usable from any thread with PostMessage.
    hwnd: isize,
    join: Option<JoinHandle<()>>,
}

impl Drop for WatchThread {
    fn drop(&mut self) {
        // SAFETY: PostMessageW is safe to call on any window handle, even a stale one, and fails then.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub struct WinTheme {
    watchers: Arc<Mutex<Watchers>>,
    thread: Mutex<Option<WatchThread>>,
}

impl Default for WinTheme {
    fn default() -> Self {
        Self::new()
    }
}

impl WinTheme {
    pub fn new() -> Self {
        Self {
            watchers: Arc::new(Mutex::new(Watchers { last: read_theme(), callbacks: Vec::new() })),
            thread: Mutex::new(None),
        }
    }

    fn ensure_thread(&self) -> shotik_core::Result<()> {
        let mut slot = self.thread.lock().map_err(|_| shotik_core::Error::msg("theme watcher poisoned"))?;
        if slot.is_some() {
            return Ok(());
        }
        let watchers = Arc::clone(&self.watchers);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<shotik_core::Result<isize>>();
        let join = std::thread::Builder::new()
            .name("shotik-theme".into())
            .spawn(move || run_window(&watchers, &ready_tx))
            .map_err(|e| shotik_core::Error::msg(format!("cannot start the theme watcher: {e}")))?;
        let hwnd = ready_rx
            .recv()
            .map_err(|_| shotik_core::Error::msg("the theme watcher thread ended early"))??;
        *slot = Some(WatchThread { hwnd, join: Some(join) });
        Ok(())
    }

    /// The watcher's window, for tests to post messages to.
    #[cfg(test)]
    fn window(&self) -> Option<HWND> {
        self.thread.lock().unwrap().as_ref().map(|t| HWND(t.hwnd as *mut _))
    }
}

fn run_window(watchers: &Arc<Mutex<Watchers>>, ready: &std::sync::mpsc::Sender<shotik_core::Result<isize>>) {
    let class = w!("ShotikThemeWatcher");
    let created: shotik_core::Result<HWND> = (|| {
        // SAFETY: a null name returns this module's handle.
        let module = unsafe { GetModuleHandleW(None) }.map_err(|e| win_err("GetModuleHandle", e))?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: module.into(),
            lpszClassName: class,
            ..Default::default()
        };
        // SAFETY: `wc` is fully initialized. Registering twice fails harmlessly with "already exists",
        // and the first registration points at the same procedure.
        unsafe { RegisterClassExW(&wc) };
        // SAFETY: WS_POPUP without WS_VISIBLE: created hidden and never shown, so it has no surface and
        // cannot take focus. The pointer stays valid because `watchers` outlives this thread's loop.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("Shotik theme watcher"),
                WS_POPUP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                None,
                None,
                Some(module.into()),
                Some(Arc::as_ptr(watchers).cast()),
            )
        }
        .map_err(|e| win_err("cannot create the theme watcher window", e))
    })();
    match created {
        Ok(hwnd) => {
            let _ = ready.send(Ok(hwnd.0 as isize));
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    }
    let mut msg = MSG::default();
    // SAFETY: the standard message loop on this thread's queue. It blocks in GetMessage: no polling.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

impl ThemeSource for WinTheme {
    fn current(&self) -> Theme {
        read_theme()
    }

    /// `changed` fires on the watcher thread, only when the theme really differs from the last report.
    /// A watcher that cannot start means the UI keeps today's theme until restart, which the signature
    /// can only report to stderr.
    fn watch(&self, changed: Callback<Theme>) {
        if let Ok(mut w) = self.watchers.lock() {
            w.last = read_theme();
            w.callbacks.push(changed);
        }
        if let Err(e) = self.ensure_thread() {
            eprintln!("theme changes will not be followed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

    #[test]
    fn accent_is_rrggbb_from_argb_ignoring_alpha() {
        // The raw value Electron printed on this machine was "006fc7e3" (RRGGBBAA), so ARGB 0xE3006FC7.
        assert_eq!(accent_from_argb(0xE300_6FC7), "#006FC7");
        assert_eq!(accent_from_argb(0xFF0A_0B0C), "#0A0B0C");
        assert_eq!(accent_from_argb(0), "#000000");
    }

    #[test]
    fn fallback_is_the_one_from_theme_js() {
        assert_eq!(FALLBACK_ACCENT, "#0067C0");
    }

    #[test]
    fn current_returns_a_well_formed_theme() {
        let t = WinTheme::new().current();
        assert_eq!(t.platform, "win32");
        assert_eq!(t.accent.len(), 7, "{}", t.accent);
        assert!(t.accent.starts_with('#'));
        assert!(t.accent[1..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()), "{}", t.accent);
    }

    #[test]
    fn only_a_colour_setting_change_counts() {
        let immersive: Vec<u16> = "ImmersiveColorSet\0".encode_utf16().collect();
        let other: Vec<u16> = "Environment\0".encode_utf16().collect();
        assert!(is_theme_message(WM_SETTINGCHANGE, LPARAM(immersive.as_ptr() as isize)));
        assert!(!is_theme_message(WM_SETTINGCHANGE, LPARAM(other.as_ptr() as isize)));
        assert!(!is_theme_message(WM_SETTINGCHANGE, LPARAM(0)));
        assert!(is_theme_message(WM_DWMCOLORIZATIONCOLORCHANGED, LPARAM(0)));
        assert!(!is_theme_message(WM_CLOSE, LPARAM(0)));
    }

    fn theme(dark: bool, accent: &str) -> Theme {
        Theme { dark, accent: accent.to_owned(), platform: "win32".to_owned() }
    }

    #[test]
    fn watchers_fire_only_when_the_theme_differs() {
        let fired = Arc::new(AtomicUsize::new(0));
        let f = Arc::clone(&fired);
        let mut w = Watchers {
            last: theme(false, "#000000"),
            callbacks: vec![Arc::new(move |_| {
                f.fetch_add(1, Ordering::SeqCst);
            })],
        };
        w.on_message(|| theme(false, "#000000"));
        assert_eq!(fired.load(Ordering::SeqCst), 0, "same theme: silent");
        w.on_message(|| theme(true, "#000000"));
        assert_eq!(fired.load(Ordering::SeqCst), 1, "dark changed");
        w.on_message(|| theme(true, "#123456"));
        assert_eq!(fired.load(Ordering::SeqCst), 2, "accent changed");
        w.on_message(|| theme(true, "#123456"));
        assert_eq!(fired.load(Ordering::SeqCst), 2, "a repeat of the same change is silent");
    }

    /// Exercises the real window and message loop without changing the user's theme: messages go to our
    /// own hidden window, and a stale `last` stands in for a change.
    #[test]
    fn the_watcher_window_reacts_to_its_messages_and_stops_on_drop() {
        let source = WinTheme::new();
        let fired = Arc::new(AtomicUsize::new(0));
        let f = Arc::clone(&fired);
        source.watch(Arc::new(move |_| {
            f.fetch_add(1, Ordering::SeqCst);
        }));
        let hwnd = source.window().expect("the watcher started");

        // Unchanged theme: the message arrives and stays silent.
        // SAFETY: SendMessage waits for the window thread; the window is alive until `source` is dropped.
        unsafe { SendMessageW(hwnd, WM_DWMCOLORIZATIONCOLORCHANGED, None, None) };
        assert_eq!(fired.load(Ordering::SeqCst), 0);

        // Now pretend the last report was different.
        source.watchers.lock().unwrap().last = theme(false, "#010203");
        // SAFETY: as above.
        unsafe { SendMessageW(hwnd, WM_DWMCOLORIZATIONCOLORCHANGED, None, None) };
        let deadline = Instant::now() + Duration::from_secs(2);
        while fired.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fired.load(Ordering::SeqCst), 1);

        // Drop must end the thread rather than hang the process.
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(source);
            done_tx.send(()).unwrap();
        });
        done_rx.recv_timeout(Duration::from_secs(5)).expect("drop stopped the watcher thread");
    }
}
