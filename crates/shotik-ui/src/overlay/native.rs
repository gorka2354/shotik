//! What the overlay needs from Win32 that Slint does not give: physical keys, exact bounds that nothing can
//! change, a window that is on screen only once it has its first frame, keyboard focus.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use shotik_core::{Error, Result};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, MapVirtualKeyW, MAPVK_VK_TO_VSC, VK_CONTROL, VK_RMENU, VK_SHIFT};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, SetForegroundWindow, SetWindowLongPtrW, BringWindowToTop,
    GWL_EXSTYLE, HTCLIENT, SC_KEYMENU, STYLESTRUCT, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_KEYDOWN, WM_KEYUP, WM_NCDESTROY,
    WM_NCHITTEST, WM_PAINT, WM_STYLECHANGING, WM_SYSCOMMAND, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_WINDOWPOSCHANGING, WS_EX_APPWINDOW,
    WS_EX_TOOLWINDOW,
};

use super::keys::{code_of, scan_code_of, Code};

const SUBCLASS_ID: usize = 0x5348_4f56;

/// Completed paints of overlay windows, and how long the last one took: Slint draws and presents inside
/// WM_PAINT. Atomics, so a test can watch without posting to the UI thread, which would starve WM_PAINT.
pub static PAINTS: AtomicU64 = AtomicU64::new(0);
pub static LAST_PAINT_MICROS: AtomicU64 = AtomicU64::new(0);

/// A key as the subclass saw it, to run outside the window procedure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: Code,
    pub down: bool,
    pub ctrl: bool,
    pub shift: bool,
}

/// What the subclass knows about its window.
pub struct Hook {
    /// While the text editor is open the keys are the editor's: `textEditor` stops propagation in the JS.
    pub editor_open: Cell<bool>,
    /// Exact outer bounds in physical pixels: x, y, width, height.
    bounds: (i32, i32, i32, i32),
    on_key: Box<dyn Fn(KeyEvent)>,
}

impl Hook {
    pub fn new(bounds: (i32, i32, i32, i32), on_key: impl Fn(KeyEvent) + 'static) -> Rc<Self> {
        Rc::new(Self {
            editor_open: Cell::new(false),
            bounds,
            on_key: Box::new(on_key),
        })
    }
}

/// Keeps the window's bounds and styles, and reports keys by physical key.
pub fn install(hwnd: HWND, hook: Rc<Hook>) -> Result<()> {
    let data = Rc::into_raw(hook) as usize;
    // SAFETY: `hwnd` is a live window of this thread and the proc outlives it. The reference count `data`
    // holds is released in WM_NCDESTROY, or right here when the subclass could not be installed.
    unsafe {
        if !SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, data).as_bool() {
            drop(Rc::from_raw(data as *const Hook));
            return Err(Error::msg("cannot subclass the overlay window"));
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize));
    }
    Ok(())
}

unsafe extern "system" fn subclass(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, data: usize) -> LRESULT {
    // SAFETY: `data` is the `Rc<Hook>` given up in `install`; it stays valid until WM_NCDESTROY below.
    let hook = unsafe { &*(data as *const Hook) };
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
            // The editor keeps its keys, except that Alt and Shift releases always count: the JS listens
            // for `keyup` on the window and the editor only stops `keydown`.
            let virtual_key = wparam.0 as u16;
            let mut scan = scan_code_of(lparam.0);
            if scan == 0 {
                // Synthetic input may leave the scan code out.
                // SAFETY: plain query without pointers.
                scan = unsafe { MapVirtualKeyW(u32::from(virtual_key), MAPVK_VK_TO_VSC) } as u8;
            }
            if let Some(code) = code_of(virtual_key, scan) {
                if !(down && hook.editor_open.get()) {
                    // SAFETY: plain queries of this thread's keyboard state.
                    let (ctrl, shift, alt_gr) = unsafe {
                        (GetKeyState(i32::from(VK_CONTROL.0)) < 0, GetKeyState(i32::from(VK_SHIFT.0)) < 0, GetKeyState(i32::from(VK_RMENU.0)) < 0)
                    };
                    // Windows reports AltGr as Ctrl plus right Alt; the browser does not call that Ctrl.
                    let ctrl = ctrl && !alt_gr;
                    (hook.on_key)(KeyEvent { code, down, ctrl, shift });
                }
            }
        }
        WM_WINDOWPOSCHANGING => {
            // `will-resize` and `setBounds` in capture.js: a frozen frame must never stretch or move,
            // whatever Windows, winit or a DPI change wants.
            // SAFETY: for this message lparam points to a WINDOWPOS owned by the sender.
            let pos = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
            let (x, y, width, height) = hook.bounds;
            if !pos.flags.contains(SWP_NOMOVE) {
                pos.x = x;
                pos.y = y;
            }
            if !pos.flags.contains(SWP_NOSIZE) {
                pos.cx = width;
                pos.cy = height;
            }
        }
        WM_STYLECHANGING if wparam.0 as i32 == GWL_EXSTYLE.0 => {
            // Winit recomputes the extended style on visibility changes, which would put the window in the taskbar.
            // SAFETY: for this message lparam points to a STYLESTRUCT owned by the sender.
            let styles = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
            styles.styleNew = (styles.styleNew | WS_EX_TOOLWINDOW.0) & !WS_EX_APPWINDOW.0;
        }
        // No resize zones at the edges.
        WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
        // `e.preventDefault()` on Alt: no menu mode, or the next key would go to it.
        WM_SYSCOMMAND if (wparam.0 & 0xFFF0) == SC_KEYMENU as usize => return LRESULT(0),
        WM_PAINT => {
            let started = Instant::now();
            // SAFETY: forwarding the message this subclass received, with its own arguments.
            let result = unsafe { DefSubclassProc(window, message, wparam, lparam) };
            LAST_PAINT_MICROS.store(started.elapsed().as_micros() as u64, Ordering::Relaxed);
            PAINTS.fetch_add(1, Ordering::Release);
            return result;
        }
        WM_NCDESTROY => {
            // SAFETY: removing our own subclass and taking back the count given up in `install`.
            unsafe {
                let _ = RemoveWindowSubclass(window, Some(subclass), SUBCLASS_ID);
                let result = DefSubclassProc(window, message, wparam, lparam);
                drop(Rc::from_raw(data as *const Hook));
                return result;
            }
        }
        _ => {}
    }
    // SAFETY: forwarding the message this subclass received, with its own arguments.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

/// A cloaked window is shown as far as Windows and GDI are concerned, so it can paint for real, but it is not
/// composed. That is how the first frame gets there before anything is visible. Failure only risks a
/// flash, and older Windows lacks the call, so it is reported and ignored.
pub fn set_cloaked(hwnd: HWND, cloaked: bool) {
    let value = BOOL::from(cloaked);
    // SAFETY: `hwnd` is live and the attribute value outlives the call.
    let result = unsafe {
        DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, std::ptr::from_ref(&value).cast(), std::mem::size_of_val(&value) as u32)
    };
    if let Err(e) = result {
        eprintln!("overlay: cannot {} the window: {e}", if cloaked { "cloak" } else { "uncloak" });
    }
}

/// `win.focus()`: the keyboard goes to the overlay under the cursor. Not used in ghost mode.
pub fn take_focus(hwnd: HWND) {
    // SAFETY: plain calls on a live window; the thread attachment is undone before returning.
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        // Windows refuses when another process holds the foreground. Sharing its input queue for a
        // moment makes this the same input state; no input is sent.
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
            eprintln!("overlay: Windows refused the keyboard focus; a click on the overlay takes it");
        }
    }
}
