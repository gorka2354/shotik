//! Shotik binary: the composition root. Picks real or fake ports, starts everything, nothing else.
#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shotik_app::{App, Environment, Ports};
use shotik_core::history::FileHistory;
use shotik_core::ports::{Callback, ScreenCapture, Tray, TrayItem, Hotkeys, HotkeyAction, HotkeyBinding, HotkeyError, SelectionEvent, SelectionWatcher};
use shotik_core::settings::{defaults, FileSettings};
use shotik_core::{Error, Result};
use shotik_recorder::{CaptureMode, Recorder, RecorderConfig, WinAudioDevices};
use shotik_ui::{Ui, UiEnv};
use shotik_win::single_instance::{acquire, Launch};

fn env_flag(name: &str) -> bool {
    std::env::var(name).as_deref() == Ok("1")
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Logged where a tray app can leave it: there is no console.
fn fatal(temp: &std::path::Path, message: &str) {
    let _ = std::fs::write(temp.join("shotik-fatal.log"), message);
    eprintln!("{message}");
}

fn main() {
    let temp = std::env::temp_dir();
    if let Err(e) = run(&temp) {
        fatal(&temp, &e.to_string());
        std::process::exit(1);
    }
}

fn run(temp: &std::path::Path) -> Result<()> {
    shotik_win::displays::set_dpi_awareness()?;
    let argv: Vec<OsString> = std::env::args_os().collect();
    let args: Vec<String> = argv.iter().map(|a| a.to_string_lossy().into_owned()).collect();

    let test_mode = env_flag("SHOTIK_TEST") || args.iter().any(|a| a == "--test");
    let ghost = env_flag("SHOTIK_GHOST");
    let real_capture = env_flag("SHOTIK_REAL_CAPTURE");
    let fake_screen = env_path("SHOTIK_FAKE_SCREEN");
    let port_override = std::env::var("SHOTIK_PORT").ok().and_then(|p| p.parse().ok()).filter(|p| *p != 0);

    // A second launch hands its arguments to the first and leaves.
    let forwarded: Arc<Mutex<Option<Arc<App>>>> = Arc::default();
    let receiver = forwarded.clone();
    let on_forwarded: Callback<Vec<OsString>> = Arc::new(move |argv| {
        let args: Vec<String> = argv.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        if let Some(app) = receiver.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            app.second_instance(&args);
        }
    });
    let _instance = match acquire(test_mode, &argv, on_forwarded)? {
        Launch::Primary(instance) => instance,
        Launch::Forwarded => return Ok(()),
    };

    let data_dir = shotik_win::paths::app_data()?.join(if test_mode { "Shotik-Test" } else { "Shotik" });
    std::fs::create_dir_all(&data_dir).map_err(|e| Error::io("create profile folder", &data_dir, e))?;
    let locale = shotik_win::paths::system_locale();

    let settings = Arc::new(FileSettings::new(
        &data_dir,
        defaults(&shotik_win::paths::pictures()?, &locale, "win32", env_path("SHOTIK_SAVE_DIR").as_deref()),
    )?);
    if !settings.ignored_fields().is_empty() {
        eprintln!("settings fields ignored for having the wrong type: {:?}", settings.ignored_fields());
    }
    let history = Arc::new(FileHistory::new(&data_dir, settings.clone())?);
    if history.skipped_entries() > 0 {
        eprintln!("history entries that could not be read: {}", history.skipped_entries());
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| Error::msg(format!("cannot start the runtime: {e}")))?;

    let ui = Ui::new(UiEnv {
        ghost,
        popup_real: env_flag("SHOTIK_POPUP_REAL"),
        translate_popup_ttl_ms: std::env::var("SHOTIK_TP_TTL").ok().and_then(|v| v.parse().ok()).unwrap_or(9000),
    })?;

    let capture: Arc<dyn ScreenCapture> = match &fake_screen {
        Some(path) => Arc::new(shotik_fake::FakeScreen::new(path)?),
        None => Arc::new(shotik_win::capture::WinCapture::new()),
    };
    // Test instances must not touch the keyboard or the tray: nothing a test does reaches the desktop.
    let hotkeys: Arc<dyn Hotkeys> = if ghost { Arc::new(NoHotkeys) } else { Arc::new(shotik_win::hotkeys::WindowsHotkeys::new()?) };
    let tray: Arc<dyn Tray> = if ghost { Arc::new(NoTray) } else { Arc::new(shotik_win::tray::WindowsTray::new("Shotik")?) };
    let selection: Arc<dyn SelectionWatcher> = if ghost || test_mode {
        Arc::new(NoSelection)
    } else {
        Arc::new(shotik_win::selection::WindowsSelectionWatcher::default())
    };
    // The clipboard of a test run stays out of the user's clipboard history.
    let clipboard = if test_mode { shotik_win::clipboard::WinClipboard::excluded_from_history() } else { shotik_win::clipboard::WinClipboard::new() };

    let displays = Arc::new(shotik_win::displays::WinDisplays::new());
    let mut recorder_config = RecorderConfig::new(CaptureMode::from_flags(test_mode, real_capture, displays.clone()));
    recorder_config.temp_dir = temp.to_path_buf();
    let recorder = Arc::new(Recorder::new(recorder_config)?);

    let ui_port: Arc<Ui> = Arc::new(ui);
    let ports = Ports {
        settings,
        history,
        displays,
        capture,
        windows: Arc::new(shotik_win::window_source::WinWindowSource::new()),
        clipboard: Arc::new(clipboard),
        ocr: Arc::new(shotik_win::ocr::WindowsOcr),
        http: Arc::new(shotik_win::http::WinHttp),
        shell: Arc::new(shotik_win::shell::WinShell::new()),
        dialogs: Arc::new(shotik_win::dialogs::WinDialogs::new()),
        hotkeys,
        tray,
        theme: Arc::new(shotik_win::theme::WinTheme::new()),
        autostart: Arc::new(shotik_win::autostart::WinAutostart::new()),
        selection,
        audio_devices: Arc::new(WinAudioDevices),
        recorder,
        ui: ui_port.clone(),
        export_gif: Arc::new(|video| shotik_recorder::export_gif(video, 12, 640)),
        quit: Arc::new(Ui::quit),
    };

    let env = Environment {
        test_mode,
        ghost,
        fake_screen: fake_screen.is_some(),
        force_controls: env_flag("SHOTIK_FORCE_CONTROLS"),
        real_capture,
        translate_stub: env_flag("SHOTIK_TRANSLATE_STUB"),
        wincap_fail: env_flag("SHOTIK_WINCAP_FAIL"),
        port_override,
        version: env!("CARGO_PKG_VERSION").to_string(),
        platform: "win32",
        locale,
        temp_dir: temp.to_path_buf(),
    };

    let app = App::new(ports, env, rt.handle().clone());
    ui_port.set_host(app.host());
    *forwarded.lock().unwrap_or_else(|e| e.into_inner()) = Some(app.clone());

    let start_app = app.clone();
    shotik_ui::runtime::on_ui(move || start_app.start(&args));
    ui_port.run()
}

struct NoHotkeys;
impl Hotkeys for NoHotkeys {
    fn register(&self, _bindings: &[HotkeyBinding], _fired: Callback<HotkeyAction>) -> Vec<HotkeyError> {
        Vec::new()
    }
    fn unregister_all(&self) {}
}

struct NoTray;
impl Tray for NoTray {
    fn set_menu(&self, _items: Vec<TrayItem>) -> Result<()> {
        Ok(())
    }
    fn set_tooltip(&self, _tooltip: &str) -> Result<()> {
        Ok(())
    }
    fn set_handlers(&self, _item: Callback<String>, _icon_clicked: Callback<()>) {}
}

struct NoSelection;
impl SelectionWatcher for NoSelection {
    fn start(&self, _events: Callback<SelectionEvent>) -> Result<()> {
        Ok(())
    }
    fn stop(&self) {}
    fn send_copy(&self) -> Result<()> {
        Ok(())
    }
}
