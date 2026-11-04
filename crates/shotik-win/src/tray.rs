//! Native tray adapter. The application supplies the localized `buildTrayMenu`
//! items; sublabels are displayed in the native menu's right-hand accelerator column.
//! No window is ever shown by construction. Only an actual context-menu request
//! opens a popup. Callbacks run on the tray thread and should post to the app loop.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc, Mutex, OnceLock,
};
use std::thread::{self, JoinHandle};

use shotik_core::error::{Error, Result};
use shotik_core::ports::{Callback, Tray, TrayItem};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WAKE: u32 = WM_APP + 1;
const ICON_EVENT: u32 = WM_APP + 2;
const ICON: &[u8] = include_bytes!("../../../assets/tray.png");

#[derive(Debug, PartialEq)]
struct MenuRow {
    command: u32,
    id: Option<String>,
    text: String,
}

#[derive(Debug, Default, PartialEq)]
struct MenuModel(Vec<MenuRow>);

impl MenuModel {
    fn new(items: Vec<TrayItem>) -> Result<Self> {
        let mut rows = Vec::with_capacity(items.len());
        let mut command = 1;
        for item in items {
            match item {
                TrayItem::Separator => rows.push(MenuRow {
                    command: 0,
                    id: None,
                    text: String::new(),
                }),
                TrayItem::Action {
                    id,
                    label,
                    sublabel,
                } => {
                    if command > u16::MAX as u32 {
                        return Err(Error::msg("Too many tray menu actions"));
                    }
                    if label.contains('\0') || sublabel.as_ref().is_some_and(|s| s.contains('\0')) {
                        return Err(Error::msg("Tray menu text contains a NUL character"));
                    }
                    // Ampersands in product text are literal, not Win32 mnemonics.
                    let mut text = label.replace('&', "&&");
                    if let Some(sublabel) = sublabel.filter(|s| !s.is_empty()) {
                        text.push('\t');
                        text.push_str(&sublabel.replace('&', "&&"));
                    }
                    rows.push(MenuRow {
                        command,
                        id: Some(id),
                        text,
                    });
                    command += 1;
                }
            }
        }
        Ok(Self(rows))
    }

    fn chosen(&self, command: u32) -> Option<&str> {
        self.0
            .iter()
            .find(|row| row.command == command)
            .and_then(|row| row.id.as_deref())
    }
}

struct NativeMenu(HMENU);
impl NativeMenu {
    fn new(model: &MenuModel) -> Result<Self> {
        let menu = Self(unsafe { CreatePopupMenu() }.map_err(tray_error)?);
        for row in &model.0 {
            let text: Vec<u16> = row.text.encode_utf16().chain(Some(0)).collect();
            unsafe {
                AppendMenuW(
                    menu.0,
                    if row.id.is_some() {
                        MF_STRING
                    } else {
                        MF_SEPARATOR
                    },
                    row.command as usize,
                    PCWSTR(text.as_ptr()),
                )
            }
            .map_err(tray_error)?;
        }
        Ok(menu)
    }
}
impl Drop for NativeMenu {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

struct NativeIcon(HICON);
impl NativeIcon {
    fn new() -> Result<Self> {
        // An icon resource may contain PNG bytes on supported Windows versions.
        unsafe { CreateIconFromResourceEx(ICON, true, 0x0003_0000, 0, 0, LR_DEFAULTCOLOR) }
            .map(Self)
            .map_err(tray_error)
    }
}
impl Drop for NativeIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

fn tray_error(e: windows::core::Error) -> Error {
    Error::msg(format!("Tray: {e}"))
}

fn tooltip(text: &str) -> Result<[u16; 128]> {
    if text.contains('\0') {
        return Err(Error::msg("Tray tooltip contains a NUL character"));
    }
    let mut result = [0; 128];
    let mut len = 0;
    for character in text.chars() {
        if len + character.len_utf16() > 127 {
            break;
        }
        let mut units = [0; 2];
        for unit in character.encode_utf16(&mut units) {
            result[len] = *unit;
            len += 1;
        }
    }
    Ok(result)
}

enum Command {
    Menu(MenuModel, mpsc::Sender<Result<()>>),
    Tooltip(Box<[u16; 128]>, mpsc::Sender<Result<()>>),
    Handlers(Callback<String>, Callback<()>),
    Quit,
}

enum NativeEvent {
    Restart,
    Activate,
    ContextMenu(i32, i32),
}

struct Signals {
    events: mpsc::Sender<NativeEvent>,
    target: AtomicUsize,
    taskbar_created: u32,
}

// This procedure never borrows the mutable tray state and never calls user code.
// It also works while TrackPopupMenuEx runs its nested message loop.
unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Signals;
    if !pointer.is_null() {
        let signals = unsafe { &*pointer };
        let event = if message == signals.taskbar_created {
            Some(NativeEvent::Restart)
        } else if message == ICON_EVENT {
            match lparam.0 as u32 & 0xffff {
                NIN_SELECT | 1025 => Some(NativeEvent::Activate), // NIN_KEYSELECT = NIN_SELECT | NINF_KEY
                WM_CONTEXTMENU => Some(NativeEvent::ContextMenu(
                    wparam.0 as u16 as i16 as i32,
                    (wparam.0 >> 16) as u16 as i16 as i32,
                )),
                _ => None,
            }
        } else {
            None
        };
        if let Some(event) = event {
            let _ = signals.events.send(event);
            let target = signals.target.load(Ordering::Relaxed);
            if target != 0 {
                unsafe {
                    let _ = PostMessageW(Some(HWND(target as *mut _)), WAKE, WPARAM(0), LPARAM(0));
                }
            }
            return LRESULT(0);
        }
    }
    if message == WM_NCDESTROY {
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

struct TrayWindows {
    messages: HWND,
    broadcasts: HWND,
}
impl TrayWindows {
    fn new(signals: &Signals) -> Result<Self> {
        static CLASS: OnceLock<windows::core::Result<()>> = OnceLock::new();
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }.map_err(tray_error)?.0);
        CLASS
            .get_or_init(|| unsafe {
                let class = WNDCLASSW {
                    hInstance: instance,
                    lpfnWndProc: Some(procedure),
                    lpszClassName: w!("Shotik.NativeTray"),
                    ..WNDCLASSW::default()
                };
                if RegisterClassW(&class) == 0 {
                    Err(windows::core::Error::from_thread())
                } else {
                    Ok(())
                }
            })
            .as_ref()
            .map_err(|e| tray_error(e.clone()))?;
        let parameter = Some(signals as *const Signals as *const _);
        let messages = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("Shotik.NativeTray"),
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance),
                parameter,
            )
        }
        .map_err(tray_error)?;
        // Message-only windows don't receive TaskbarCreated broadcasts. This
        // invisible, nonactivating top-level companion only relays that message
        // and owns the context menu. It never gets WS_VISIBLE or ShowWindow.
        let broadcasts = match unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("Shotik.NativeTray"),
                w!(""),
                WS_POPUP,
                20000,
                20000,
                0,
                0,
                None,
                None,
                Some(instance),
                parameter,
            )
        } {
            Ok(hwnd) => hwnd,
            Err(e) => {
                unsafe {
                    let _ = DestroyWindow(messages);
                }
                return Err(tray_error(e));
            }
        };
        signals.target.store(messages.0 as usize, Ordering::Relaxed);
        Ok(Self {
            messages,
            broadcasts,
        })
    }
}
impl Drop for TrayWindows {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.messages);
            let _ = DestroyWindow(self.broadcasts);
        }
    }
}

struct Notification(NOTIFYICONDATAW);

fn notify(message: NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) -> Result<()> {
    unsafe {
        SetLastError(ERROR_SUCCESS);
    }
    if unsafe { Shell_NotifyIconW(message, data) }.as_bool() {
        return Ok(());
    }
    let code = unsafe { GetLastError() };
    if code == ERROR_SUCCESS {
        Err(Error::msg(format!(
            "Shell_NotifyIconW({}) failed",
            message.0
        )))
    } else {
        Err(tray_error(windows::core::Error::from(code)))
    }
}

impl Notification {
    fn add(&self) -> Result<()> {
        notify(NIM_ADD, &self.0)?;
        if let Err(e) = notify(NIM_SETVERSION, &self.0) {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &self.0);
            }
            return Err(e);
        }
        Ok(())
    }
}
impl Drop for Notification {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.0);
        }
    }
}

pub struct WindowsTray {
    commands: mpsc::Sender<Command>,
    hwnd: usize,
    menu_owner: usize,
    thread: Option<JoinHandle<()>>,
    error: Arc<Mutex<Option<Error>>>,
}

impl WindowsTray {
    /// Adds the real icon. Ghost/test composition must use a fake tray instead;
    /// this constructor rejects ghost mode so tests cannot accidentally show it.
    pub fn new(initial_tooltip: &str) -> Result<Self> {
        if std::env::var_os("SHOTIK_GHOST").is_some_and(|value| value == "1") {
            return Err(Error::msg("A native tray cannot be created in ghost mode"));
        }
        let initial_tooltip = tooltip(initial_tooltip)?;
        let (commands, receiver) = mpsc::channel();
        let (ready, initialized) = mpsc::sync_channel(1);
        let error = Arc::new(Mutex::new(None));
        let thread_error = Arc::clone(&error);
        let handle = thread::Builder::new()
            .name("shotik-tray".into())
            .spawn(move || {
                let run = || -> Result<()> {
                    let (events, native_events) = mpsc::channel();
                    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
                    if taskbar_created == 0 {
                        return Err(tray_error(windows::core::Error::from_thread()));
                    }
                    let signals = Signals {
                        events,
                        target: AtomicUsize::new(0),
                        taskbar_created,
                    };
                    let windows = TrayWindows::new(&signals)?;
                    let icon = NativeIcon::new()?;
                    let mut notification = Notification(NOTIFYICONDATAW {
                        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                        hWnd: windows.messages,
                        uID: 1,
                        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
                        uCallbackMessage: ICON_EVENT,
                        hIcon: icon.0,
                        szTip: initial_tooltip,
                        Anonymous: NOTIFYICONDATAW_0 {
                            uVersion: NOTIFYICON_VERSION_4,
                        },
                        ..NOTIFYICONDATAW::default()
                    });
                    notification.add()?;
                    let mut model = MenuModel::default();
                    let mut menu = NativeMenu::new(&model)?;
                    let mut handlers: Option<(Callback<String>, Callback<()>)> = None;
                    if ready
                        .send(Ok((
                            windows.messages.0 as usize,
                            windows.broadcasts.0 as usize,
                        )))
                        .is_err()
                    {
                        return Ok(());
                    }
                    let mut message = MSG::default();
                    loop {
                        let result = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
                        if result == -1 {
                            return Err(tray_error(windows::core::Error::from_thread()));
                        }
                        if result == 0 {
                            return Ok(());
                        }
                        unsafe {
                            let _ = TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                        // Always drain after dispatch, including after the popup's
                        // nested loop: no command or Explorer restart is lost there.
                        while let Ok(command) = receiver.try_recv() {
                            match command {
                                Command::Menu(new_model, reply) => {
                                    let result = NativeMenu::new(&new_model).map(|new_menu| {
                                        model = new_model;
                                        menu = new_menu;
                                    });
                                    let _ = reply.send(result);
                                }
                                Command::Tooltip(text, reply) => {
                                    let mut data = notification.0;
                                    data.szTip = *text;
                                    let result = notify(NIM_MODIFY, &data);
                                    if result.is_ok() {
                                        notification.0 = data;
                                    }
                                    let _ = reply.send(result);
                                }
                                Command::Handlers(item, clicked) => {
                                    handlers = Some((item, clicked))
                                }
                                Command::Quit => return Ok(()),
                            }
                        }
                        while let Ok(event) = native_events.try_recv() {
                            match event {
                                NativeEvent::Restart => {
                                    if let Err(e) = notification.add() {
                                        *thread_error.lock().unwrap_or_else(|e| e.into_inner()) =
                                            Some(e);
                                    }
                                }
                                NativeEvent::Activate => {
                                    if let Some((_, clicked)) = &handlers {
                                        clicked(());
                                    }
                                }
                                NativeEvent::ContextMenu(mut x, mut y) => {
                                    if x == -1 && y == -1 {
                                        let mut position = POINT::default();
                                        unsafe { GetCursorPos(&mut position) }
                                            .map_err(tray_error)?;
                                        x = position.x;
                                        y = position.y;
                                    }
                                    // Invoked only in direct response to the user's
                                    // tray interaction, never by an automated test.
                                    unsafe {
                                        let _ = SetForegroundWindow(windows.broadcasts);
                                        SetLastError(ERROR_SUCCESS);
                                    }
                                    let choice = unsafe {
                                        TrackPopupMenuEx(
                                            menu.0,
                                            (TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON).0,
                                            x,
                                            y,
                                            windows.broadcasts,
                                            None,
                                        )
                                    }
                                    .0 as u32;
                                    if choice == 0 {
                                        let error = unsafe { GetLastError() };
                                        if error != ERROR_SUCCESS {
                                            *thread_error
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner()) =
                                                Some(tray_error(windows::core::Error::from(error)));
                                        }
                                    }
                                    unsafe {
                                        let _ = PostMessageW(
                                            Some(windows.broadcasts),
                                            WM_NULL,
                                            WPARAM(0),
                                            LPARAM(0),
                                        );
                                    }
                                    if let (Some(id), Some((chosen, _))) =
                                        (model.chosen(choice), &handlers)
                                    {
                                        chosen(id.to_owned());
                                    }
                                    // A WAKE may have been dispatched inside the menu loop.
                                    unsafe {
                                        let _ = PostMessageW(
                                            Some(windows.messages),
                                            WAKE,
                                            WPARAM(0),
                                            LPARAM(0),
                                        );
                                    }
                                }
                            }
                        }
                    }
                };
                if let Err(e) = run() {
                    // Before startup completes, return the actual initialization error.
                    // Afterwards retain it for take_error / subsequent port calls.
                    let message = e.to_string();
                    *thread_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                    let _ = ready.send(Err(Error::msg(message)));
                }
            })
            .map_err(|e| Error::msg(format!("Cannot start tray thread: {e}")))?;
        match initialized.recv() {
            Ok(Ok((hwnd, menu_owner))) => Ok(Self {
                commands,
                hwnd,
                menu_owner,
                thread: Some(handle),
                error,
            }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(error) => {
                let _ = handle.join();
                Err(Error::msg(format!("Tray startup: {error}")))
            }
        }
    }

    /// Asynchronous Shell / message-loop failures, including failed re-addition
    /// after Explorer restarts, are available to the application's error policy.
    pub fn take_error(&self) -> Option<Error> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn send(&self, command: Command) -> Result<()> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        if self
            .thread
            .as_ref()
            .is_some_and(|handle| handle.thread().id() == thread::current().id())
        {
            return Err(Error::msg(
                "Tray callbacks must post work to the application loop",
            ));
        }
        self.commands
            .send(command)
            .map_err(|_| Error::msg("Tray thread has exited"))?;
        unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WAKE, WPARAM(0), LPARAM(0)) }
            .map_err(tray_error)
    }
}

impl Tray for WindowsTray {
    fn set_menu(&self, items: Vec<TrayItem>) -> Result<()> {
        let model = MenuModel::new(items)?;
        let (reply, result) = mpsc::channel();
        self.send(Command::Menu(model, reply))?;
        result
            .recv()
            .map_err(|_| Error::msg("Tray thread exited while updating menu"))?
    }

    fn set_tooltip(&self, text: &str) -> Result<()> {
        let text = tooltip(text)?;
        let (reply, result) = mpsc::channel();
        self.send(Command::Tooltip(Box::new(text), reply))?;
        result
            .recv()
            .map_err(|_| Error::msg("Tray thread exited while updating tooltip"))?
    }

    fn set_handlers(&self, item: Callback<String>, icon_clicked: Callback<()>) {
        if let Err(e) = self.send(Command::Handlers(item, icon_clicked)) {
            *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
        }
    }
}

impl Drop for WindowsTray {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        unsafe {
            // Cancel only our own popup's nested loop before waiting for its thread.
            let _ = PostMessageW(
                Some(HWND(self.menu_owner as *mut _)),
                WM_CANCELMODE,
                WPARAM(0),
                LPARAM(0),
            );
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut _)), WAKE, WPARAM(0), LPARAM(0));
        }
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

    #[test]
    fn menu_keeps_reference_order_ids_separators_and_right_aligned_hotkeys() {
        // Same order as buildTrayMenu; labels are supplied by the app's i18n.
        let mut items: Vec<_> = [
            ("region", "Снимок области", Some("PrintScreen")),
            ("full", "Весь экран", Some("Ctrl+PrintScreen")),
            ("repeat", "Переснять область", Some("Shift+PrintScreen")),
            ("text", "Вытащить текст", Some("Ctrl+Shift+PrintScreen")),
            ("translate", "Перевести выделение", Some("Ctrl+Alt+T")),
            ("record", "Записать область…", Some("Ctrl+Alt+PrintScreen")),
            ("audio", "Записать звук…", Some("Ctrl+Alt+Shift+R")),
        ]
        .into_iter()
        .map(|(id, label, sublabel)| TrayItem::Action {
            id: id.into(),
            label: label.into(),
            sublabel: sublabel.map(str::to_owned),
        })
        .collect();
        items.extend([
            TrayItem::Separator,
            TrayItem::Action {
                id: "open".into(),
                label: "Открыть Shotik".into(),
                sublabel: None,
            },
            TrayItem::Separator,
            TrayItem::Action {
                id: "quit".into(),
                label: "Выход".into(),
                sublabel: None,
            },
        ]);
        let model = MenuModel::new(items).unwrap();
        assert_eq!(model.0.len(), 11);
        assert_eq!(model.0[0].text, "Снимок области\tPrintScreen");
        assert_eq!(model.0[6].text, "Записать звук…\tCtrl+Alt+Shift+R");
        assert!(model.0[7].id.is_none());
        assert_eq!(model.chosen(8), Some("open"));
        assert_eq!(model.chosen(9), Some("quit"));
        assert_eq!(model.chosen(0), None);
        assert_eq!(model.chosen(500), None);
    }

    #[test]
    fn stop_recording_has_no_sublabel_and_ampersands_are_literal() {
        let model = MenuModel::new(vec![
            TrayItem::Action {
                id: "stop".into(),
                label: "Stop recording".into(),
                sublabel: None,
            },
            TrayItem::Action {
                id: "other".into(),
                label: "A & B".into(),
                sublabel: Some(String::new()),
            },
        ])
        .unwrap();
        assert_eq!(model.0[0].text, "Stop recording");
        assert_eq!(model.0[1].text, "A && B");
        assert_eq!(model.chosen(1), Some("stop"));
    }

    #[test]
    fn tooltip_fits_win32_buffer_without_splitting_surrogate_pairs() {
        let text = "x".repeat(126) + "🦀";
        let encoded = tooltip(&text).unwrap();
        assert_eq!(encoded[126], 0);
        assert_eq!(encoded[127], 0);
        assert!(tooltip("bad\0text").is_err());
    }

    #[test]
    fn embedded_png_is_a_valid_icon_resource_without_showing_it() {
        let _icon = NativeIcon::new().unwrap();
    }
}
