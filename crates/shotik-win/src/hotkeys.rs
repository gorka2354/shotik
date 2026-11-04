//! RegisterHotKey on an owning thread with a message-only window.
//! Callbacks run on that thread; hand work off to the application's event loop.

use std::collections::HashMap;
use std::sync::{mpsc, OnceLock};
use std::thread::{self, JoinHandle};

use shotik_core::error::{Error, Result};
use shotik_core::ports::{Callback, HotkeyAction, HotkeyBinding, HotkeyError, Hotkeys};
use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const COMMAND: u32 = WM_APP + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Accelerator {
    modifiers: HOT_KEY_MODIFIERS,
    key: u32,
}

fn parse(accelerator: &str) -> Option<Accelerator> {
    let mut modifiers = MOD_NOREPEAT;
    let mut parts = accelerator.split('+').peekable();
    while let Some(part) = parts.next() {
        let part = part.to_ascii_lowercase();
        if parts.peek().is_some() {
            let modifier = match part.as_str() {
                "ctrl" | "control" | "commandorcontrol" | "cmdorctrl" => MOD_CONTROL,
                "alt" | "option" => MOD_ALT,
                "shift" => MOD_SHIFT,
                "super" | "meta" => MOD_WIN,
                _ => return None,
            };
            if modifiers.0 & modifier.0 != 0 {
                return None;
            }
            modifiers |= modifier;
            continue;
        }
        let key = match part.as_str() {
            "printscreen" => u32::from(VK_SNAPSHOT.0),
            "space" => u32::from(VK_SPACE.0),
            "home" => u32::from(VK_HOME.0),
            "end" => u32::from(VK_END.0),
            "pageup" => u32::from(VK_PRIOR.0),
            "pagedown" => u32::from(VK_NEXT.0),
            "insert" => u32::from(VK_INSERT.0),
            "up" => u32::from(VK_UP.0),
            "down" => u32::from(VK_DOWN.0),
            "left" => u32::from(VK_LEFT.0),
            "right" => u32::from(VK_RIGHT.0),
            "`" => u32::from(VK_OEM_3.0),
            "-" => u32::from(VK_OEM_MINUS.0),
            "=" => u32::from(VK_OEM_PLUS.0),
            _ if part.len() == 1 && part.as_bytes()[0].is_ascii_alphanumeric() => {
                u32::from(part.as_bytes()[0].to_ascii_uppercase())
            }
            _ => {
                let number = part.strip_prefix('f')?.parse::<u32>().ok()?;
                if !(1..=24).contains(&number) {
                    return None;
                }
                u32::from(VK_F1.0) + number - 1
            }
        };
        return Some(Accelerator { modifiers, key });
    }
    None
}

enum Command {
    Register(
        Vec<HotkeyBinding>,
        Callback<HotkeyAction>,
        mpsc::Sender<Vec<HotkeyError>>,
    ),
    Clear(mpsc::Sender<()>),
    Quit,
}

pub struct WindowsHotkeys {
    commands: mpsc::Sender<Command>,
    hwnd: usize,
    thread: Option<JoinHandle<()>>,
}

fn failed(bindings: &[HotkeyBinding]) -> Vec<HotkeyError> {
    bindings
        .iter()
        .filter(|binding| !binding.accelerator.is_empty())
        .map(|binding| HotkeyError {
            name: binding.action,
            acc: binding.accelerator.clone(),
        })
        .collect()
}

fn clear_registrations(hwnd: HWND, bindings: &mut HashMap<i32, HotkeyAction>) {
    for &id in bindings.keys() {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), id);
        }
    }
    bindings.clear();
    // UnregisterHotKey doesn't remove already queued WM_HOTKEY messages. Drain
    // only our own window before reusing IDs for the replacement bindings.
    let mut message = MSG::default();
    while unsafe { PeekMessageW(&mut message, Some(hwnd), WM_HOTKEY, WM_HOTKEY, PM_REMOVE) }
        .as_bool()
    {}
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn window() -> windows::core::Result<HWND> {
    static CLASS: OnceLock<windows::core::Result<()>> = OnceLock::new();
    let instance = HINSTANCE(unsafe { GetModuleHandleW(None)? }.0);
    CLASS
        .get_or_init(|| unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                lpszClassName: w!("Shotik.NativeHotkeys"),
                ..WNDCLASSW::default()
            };
            if RegisterClassW(&class) == 0 {
                Err(windows::core::Error::from_thread())
            } else {
                Ok(())
            }
        })
        .as_ref()
        .map_err(Clone::clone)?;
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("Shotik.NativeHotkeys"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )
    }
}

impl WindowsHotkeys {
    /// Creates only a message-only window; no key is registered until `register`.
    pub fn new() -> Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let (ready, initialized) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("shotik-hotkeys".into())
            .spawn(move || {
                let hwnd = match window() {
                    Ok(hwnd) => hwnd,
                    Err(e) => {
                        let _ = ready.send(Err(Error::msg(format!("Hotkey window: {e}"))));
                        return;
                    }
                };
                if ready.send(Ok(hwnd.0 as usize)).is_err() {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                    return;
                }
                let mut bindings = HashMap::<i32, HotkeyAction>::new();
                let mut fired: Option<Callback<HotkeyAction>> = None;
                let mut message = MSG::default();
                'messages: loop {
                    let result = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
                    if result <= 0 {
                        break;
                    }
                    if message.message == COMMAND {
                        while let Ok(command) = receiver.try_recv() {
                            match command {
                                Command::Register(requested, callback, reply) => {
                                    clear_registrations(hwnd, &mut bindings);
                                    fired = Some(callback);
                                    let mut errors = Vec::new();
                                    let mut next_id = 1;
                                    for binding in &requested {
                                        if binding.accelerator.is_empty() {
                                            continue;
                                        }
                                        let registered =
                                            parse(&binding.accelerator).is_some_and(|acc| {
                                                if next_id > 0xbfff {
                                                    return false;
                                                }
                                                let id = next_id;
                                                next_id += 1;
                                                // VK_SNAPSHOT (0x2c), not VK_PRINT, is PrintScreen.
                                                if unsafe {
                                                    RegisterHotKey(
                                                        Some(hwnd),
                                                        id,
                                                        acc.modifiers,
                                                        acc.key,
                                                    )
                                                }
                                                .is_ok()
                                                {
                                                    bindings.insert(id, binding.action);
                                                    true
                                                } else {
                                                    false
                                                }
                                            });
                                        if !registered {
                                            errors.push(HotkeyError {
                                                name: binding.action,
                                                acc: binding.accelerator.clone(),
                                            });
                                        }
                                    }
                                    let _ = reply.send(errors);
                                }
                                Command::Clear(reply) => {
                                    clear_registrations(hwnd, &mut bindings);
                                    fired = None;
                                    let _ = reply.send(());
                                }
                                Command::Quit => break 'messages,
                            }
                        }
                    } else if message.message == WM_HOTKEY {
                        if let (Some(action), Some(callback)) =
                            (bindings.get(&(message.wParam.0 as i32)), &fired)
                        {
                            callback(*action);
                        }
                    } else {
                        unsafe {
                            let _ = TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                }
                clear_registrations(hwnd, &mut bindings);
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            })
            .map_err(|e| Error::msg(format!("Cannot start hotkey thread: {e}")))?;
        match initialized.recv() {
            Ok(Ok(hwnd)) => Ok(Self {
                commands,
                hwnd,
                thread: Some(handle),
            }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(error) => {
                let _ = handle.join();
                Err(Error::msg(format!("Hotkey startup: {error}")))
            }
        }
    }

    fn wake(&self) -> windows::core::Result<()> {
        unsafe {
            PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                COMMAND,
                WPARAM(0),
                LPARAM(0),
            )
        }
    }

    fn on_own_thread(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|handle| handle.thread().id() == thread::current().id())
    }
}

impl Hotkeys for WindowsHotkeys {
    fn register(
        &self,
        bindings: &[HotkeyBinding],
        fired: Callback<HotkeyAction>,
    ) -> Vec<HotkeyError> {
        if self.on_own_thread() {
            return failed(bindings);
        }
        let (reply, result) = mpsc::channel();
        if self
            .commands
            .send(Command::Register(bindings.to_vec(), fired, reply))
            .is_err()
            || self.wake().is_err()
        {
            return failed(bindings);
        }
        result.recv().unwrap_or_else(|_| failed(bindings))
    }

    fn unregister_all(&self) {
        let (reply, result) = mpsc::channel();
        if self.commands.send(Command::Clear(reply)).is_ok()
            && self.wake().is_ok()
            && !self.on_own_thread()
        {
            let _ = result.recv();
        }
    }
}

impl Drop for WindowsHotkeys {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        let _ = self.wake();
        if let Some(handle) = self.thread.take() {
            if handle.thread().id() != thread::current().id() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn parses_defaults_and_every_windows_recorder_key_without_registering_them() {
        for (acc, key) in [
            ("PrintScreen", VK_SNAPSHOT),
            ("Ctrl+PrintScreen", VK_SNAPSHOT),
            ("Shift+PrintScreen", VK_SNAPSHOT),
            ("Ctrl+Shift+PrintScreen", VK_SNAPSHOT),
            ("Ctrl+Alt+PrintScreen", VK_SNAPSHOT),
            ("Ctrl+Alt+Shift+R", VK_R),
            ("Ctrl+Alt+T", VK_T),
            ("Ctrl+Space", VK_SPACE),
            ("Ctrl+Home", VK_HOME),
            ("Ctrl+End", VK_END),
            ("Alt+PageUp", VK_PRIOR),
            ("Alt+PageDown", VK_NEXT),
            ("Shift+Insert", VK_INSERT),
            ("Ctrl+Up", VK_UP),
            ("Ctrl+Down", VK_DOWN),
            ("Super+Left", VK_LEFT),
            ("Super+Right", VK_RIGHT),
            ("Ctrl+`", VK_OEM_3),
            ("Ctrl+-", VK_OEM_MINUS),
            ("Ctrl+=", VK_OEM_PLUS),
            ("Ctrl+1", VK_1),
            ("F1", VK_F1),
            ("Ctrl+Alt+Shift+F24", VK_F24),
        ] {
            let parsed = parse(acc).unwrap_or_else(|| panic!("cannot parse {acc}"));
            assert_eq!(parsed.key, u32::from(key.0), "{acc}");
            assert_ne!(parsed.modifiers.0 & MOD_NOREPEAT.0, 0);
        }
        assert_eq!(
            parse("Ctrl+Alt+Shift+R").unwrap().modifiers,
            MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT
        );
        for bad in [
            "",
            "Ctrl",
            "Ctrl+",
            "Ctrl+Ctrl+R",
            "Ctrl+Bogus",
            "F25",
            "Ctrl+A+B",
        ] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn taken_obscure_combo_returns_hotkey_error_and_is_released() {
        // The only real global registration made by any automated test here.
        // Never send it, never register PrintScreen or a user's ordinary shortcut.
        let first = WindowsHotkeys::new().unwrap();
        let second = WindowsHotkeys::new().unwrap();
        let binding = HotkeyBinding {
            action: HotkeyAction::Region,
            accelerator: "Ctrl+Alt+Shift+F24".into(),
        };
        let (sender, received) = mpsc::channel();
        let callback: Callback<HotkeyAction> = Arc::new(move |action| {
            let _ = sender.send(action);
        });
        assert!(first
            .register(std::slice::from_ref(&binding), Arc::clone(&callback))
            .is_empty());
        assert_eq!(
            second.register(std::slice::from_ref(&binding), Arc::clone(&callback)),
            vec![HotkeyError {
                name: binding.action,
                acc: binding.accelerator.clone()
            }]
        );
        // Exercise only our message-only window's dispatch, without keyboard input.
        unsafe {
            PostMessageW(
                Some(HWND(first.hwnd as *mut _)),
                WM_HOTKEY,
                WPARAM(1),
                LPARAM(0),
            )
        }
        .unwrap();
        assert_eq!(
            received
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            binding.action
        );
        first.unregister_all();
        assert!(second
            .register(std::slice::from_ref(&binding), callback)
            .is_empty());
        second.unregister_all();
    }
}
