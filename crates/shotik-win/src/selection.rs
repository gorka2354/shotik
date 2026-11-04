//! Read-only UI Automation watcher. Only the explicitly called `send_copy` sends input.
pub mod state;

use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use shotik_core::error::{Error, Result};
use shotik_core::ports::{Callback, SelectionEvent, SelectionWatcher};
use windows::Win32::Foundation::{E_NOINTERFACE, POINT};
use windows::Win32::Globalization::{
    LCMapStringEx, LCMAP_SORTKEY, LOCALE_NAME_INVARIANT, NORM_IGNORECASE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId, UIA_E_NOTSUPPORTED,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use state::{Position, SelectionRead, WatchState, POLL_MS};

#[derive(Default)]
pub struct WindowsSelectionWatcher {
    worker: Mutex<Option<Worker>>,
    error: Arc<Mutex<Option<Error>>>,
}

struct Worker {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn stop(self) {
        let _ = self.stop.send(());
        if self.thread.thread().id() != thread::current().id() {
            let _ = self.thread.join();
        }
    }
}

struct ComApartment;
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn selection_error(e: windows::core::Error) -> Error {
    Error::msg(format!("Selection watcher: {e}"))
}

fn cursor() -> windows::core::Result<Position> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point)? };
    Ok(Position {
        x: point.x,
        y: point.y,
    })
}

fn read_selection(automation: &IUIAutomation) -> windows::core::Result<SelectionRead> {
    unsafe {
        let focused = match automation.GetFocusedElement() {
            Err(e) if e.code().0 == 0 => return Ok(SelectionRead::default()),
            result => result?,
        };
        let pattern: IUIAutomationTextPattern = match focused.GetCurrentPatternAs(UIA_TextPatternId)
        {
            Err(e)
                if e.code().0 == 0
                    || e.code() == E_NOINTERFACE
                    || e.code().0 as u32 == UIA_E_NOTSUPPORTED =>
            {
                return Ok(SelectionRead::default())
            }
            result => result?,
        };
        let ranges = match pattern.GetSelection() {
            Err(e) if e.code().0 == 0 => {
                return Ok(SelectionRead {
                    has_pattern: true,
                    ..SelectionRead::default()
                })
            }
            result => result?,
        };
        let mut text = String::new();
        for i in 0..ranges.Length()? {
            text.push_str(&ranges.GetElement(i)?.GetText(5000)?.to_string());
        }
        let comparison_key = selection_key(text.trim())?;
        Ok(SelectionRead {
            text,
            has_pattern: true,
            comparison_key,
        })
    }
}

fn selection_key(text: &str) -> windows::core::Result<Vec<u8>> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let source: Vec<u16> = text.encode_utf16().collect();
    let flags = LCMAP_SORTKEY | NORM_IGNORECASE.0;
    let required = unsafe {
        LCMapStringEx(
            LOCALE_NAME_INVARIANT,
            flags,
            &source,
            None,
            None,
            None,
            windows::Win32::Foundation::LPARAM(0),
        )
    };
    if required == 0 {
        return Err(windows::core::Error::from_thread());
    }
    // With LCMAP_SORTKEY the Win32 destination size and result are bytes, even
    // though the generated windows-rs binding uses a u16 slice. Allocating this
    // many u16s supplies at least the requested bytes and the correct byte count.
    let mut destination = vec![0u16; required as usize];
    let written = unsafe {
        LCMapStringEx(
            LOCALE_NAME_INVARIANT,
            flags,
            &source,
            Some(&mut destination),
            None,
            None,
            windows::Win32::Foundation::LPARAM(0),
        )
    };
    if written == 0 {
        return Err(windows::core::Error::from_thread());
    }
    Ok(
        unsafe { std::slice::from_raw_parts(destination.as_ptr().cast::<u8>(), written as usize) }
            .to_vec(),
    )
}

impl WindowsSelectionWatcher {
    /// UIA providers may disappear during a read. Like the reference script this
    /// becomes an empty/no-pattern observation; the actual error remains inspectable.
    /// Cursor/worker failures stop the watcher and are reported here as well.
    pub fn take_error(&self) -> Option<Error> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

impl SelectionWatcher for WindowsSelectionWatcher {
    fn start(&self, events: Callback<SelectionEvent>) -> Result<()> {
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| Error::msg("Selection watcher lock poisoned"))?;
        if worker.is_some() {
            return Ok(());
        }
        // Same existing modes as main.js: neither reads the user's UIA selection.
        if std::env::var_os("SHOTIK_GHOST").is_some_and(|v| v == "1")
            || std::env::var_os("SHOTIK_TEST").is_some_and(|v| v == "1")
        {
            return Ok(());
        }
        let (stop, stopped) = mpsc::channel();
        let (ready, initialized) = mpsc::sync_channel(1);
        let error = Arc::clone(&self.error);
        let handle = thread::Builder::new()
            .name("shotik-selection".into())
            .spawn(move || {
                let init = || -> windows::core::Result<(ComApartment, IUIAutomation)> {
                    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
                    let apartment = ComApartment;
                    let automation =
                        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
                    Ok((apartment, automation))
                };
                let (_apartment, automation) = match init() {
                    Ok(value) => value,
                    Err(e) => {
                        let _ = ready.send(Err(selection_error(e)));
                        return;
                    }
                };
                // Prewarm before reporting readiness. Never log the private selected text.
                let _ = read_selection(&automation);
                if ready.send(Ok(())).is_err() {
                    return;
                }
                let mut state = WatchState::default();
                let started = Instant::now();
                loop {
                    match stopped.recv_timeout(Duration::from_millis(POLL_MS)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    let down = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0 };
                    let position = match cursor() {
                        Ok(value) => value,
                        Err(e) => {
                            *error.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(selection_error(e));
                            break;
                        }
                    };
                    let tick = state.tick(started.elapsed().as_millis() as u64, down, position);
                    for event in &tick.events {
                        events(event.clone());
                    }
                    if tick.read_selection {
                        let info = match read_selection(&automation) {
                            Ok(value) => value,
                            Err(e) => {
                                *error.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Some(selection_error(e));
                                SelectionRead::default()
                            }
                        };
                        let position = match cursor() {
                            Ok(value) => value,
                            Err(e) => {
                                *error.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Some(selection_error(e));
                                break;
                            }
                        };
                        for event in state.selection(&tick, info, position) {
                            events(event);
                        }
                    }
                }
            })
            .map_err(|e| Error::msg(format!("Cannot start selection watcher: {e}")))?;
        match initialized.recv() {
            Ok(Ok(())) => {
                *worker = Some(Worker {
                    stop,
                    thread: handle,
                });
                Ok(())
            }
            result => {
                let _ = handle.join();
                result
                    .unwrap_or_else(|_| Err(Error::msg("Selection watcher exited during startup")))
            }
        }
    }

    fn stop(&self) {
        let worker = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(worker) = worker {
            worker.stop();
        }
    }

    fn send_copy(&self) -> Result<()> {
        let inputs = copy_inputs();
        let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
        if sent != inputs.len() as u32 {
            // A partial insertion must not leave our synthetic Ctrl/C held.
            // Send only the key-up events for keys left down by that prefix.
            let mut released = 0;
            let mut needed = 0;
            if (1..=3).contains(&sent) {
                let cleanup = &inputs[if sent == 2 { 2 } else { 3 }..];
                needed = cleanup.len();
                released = unsafe { SendInput(cleanup, size_of::<INPUT>() as i32) };
            }
            return Err(Error::msg(format!(
                "SendInput Ctrl+C sent {sent}/4 events; cleanup released {released}/{needed} keys (input may be blocked by UIPI)"
            )));
        }
        thread::sleep(Duration::from_millis(90));
        Ok(())
    }
}

fn copy_inputs() -> [INPUT; 4] {
    [
        (VK_CONTROL, false),
        (VK_C, false),
        (VK_C, true),
        (VK_CONTROL, true),
    ]
    .map(|(key, up)| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                ..KEYBDINPUT::default()
            },
        },
    })
}

impl Drop for WindowsSelectionWatcher {
    fn drop(&mut self) {
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            worker.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_sequence_is_ctrl_c_and_releases_both_keys_without_sending_it() {
        let inputs = copy_inputs();
        let expected = [
            (VK_CONTROL, false),
            (VK_C, false),
            (VK_C, true),
            (VK_CONTROL, true),
        ];
        for (input, (key, up)) in inputs.iter().zip(expected) {
            assert_eq!(input.r#type, INPUT_KEYBOARD);
            let keyboard = unsafe { input.Anonymous.ki };
            assert_eq!(keyboard.wVk, key);
            assert_eq!(
                keyboard.dwFlags,
                if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                }
            );
        }
    }

    #[test]
    fn selection_comparison_matches_powershell_invariant_collation_without_reading_uia() {
        for (a, b) in [
            ("hello", "HELLO"),
            ("ПРИВЕТ", "привет"),
            ("Σ", "ς"),
            ("ß", "ss"),
            ("é", "e\u{301}"),
        ] {
            assert_eq!(
                selection_key(a).unwrap(),
                selection_key(b).unwrap(),
                "{a} vs {b}"
            );
        }
        assert_ne!(selection_key("İ").unwrap(), selection_key("i").unwrap());
        assert_ne!(
            selection_key("hello").unwrap(),
            selection_key("world").unwrap()
        );
    }
}
