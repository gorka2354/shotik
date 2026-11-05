//! Orchestration: what `src/main/main.js` and the session half of `src/main/capture.js` did.
//!
//! Everything here works through ports, so it runs with real implementations in the binary and with
//! `shotik-fake` in tests. Slow work runs on Tokio's blocking pool; nothing here touches a window.

mod actions;
mod diagnostics;
mod host;
mod mcp_wiring;
mod overlay;
mod recording;
mod selection;
mod test_routes;
mod tray;
mod update;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use serde_json::{json, Value};
use shotik_core::ports::{
    AppHost, AudioDevices, Autostart, Clipboard, Dialogs, Displays, HistoryStore, Hotkeys, HotkeyAction,
    HotkeyError, Http, Ocr, RecorderControl, ScreenCapture, SelectionWatcher, SettingsStore, Shell, Theme,
    ThemeSource, Tray, UiPort, WindowSource,
};
use shotik_core::{i18n, Result, Settings};
use tokio::runtime::Handle;

use diagnostics::Diagnostics;
pub use update::UpdateConfig;

/// Facts about this run, resolved by the binary from the environment and arguments.
#[derive(Clone, Debug)]
pub struct Environment {
    pub test_mode: bool,
    pub ghost: bool,
    /// `SHOTIK_FAKE_SCREEN` is in effect.
    pub fake_screen: bool,
    /// `SHOTIK_FORCE_CONTROLS`: build the recording windows even in ghost mode.
    pub force_controls: bool,
    /// `SHOTIK_REAL_CAPTURE`: a test instance captures for real.
    pub real_capture: bool,
    pub translate_stub: bool,
    /// `SHOTIK_WINCAP_FAIL`.
    pub wincap_fail: bool,
    /// `SHOTIK_PORT`.
    pub port_override: Option<u16>,
    pub version: String,
    /// "win32" or "darwin", as the JS version reports it.
    pub platform: &'static str,
    pub locale: String,
    pub temp_dir: PathBuf,
    pub update: UpdateConfig,
}

/// Writes a GIF next to the video: fps 12, at most 640 px wide.
pub type ExportGif = Arc<dyn Fn(&Path) -> Result<PathBuf> + Send + Sync>;

/// All the boundaries of the application.
#[derive(Clone)]
pub struct Ports {
    pub settings: Arc<dyn SettingsStore>,
    pub history: Arc<dyn HistoryStore>,
    pub displays: Arc<dyn Displays>,
    pub capture: Arc<dyn ScreenCapture>,
    pub windows: Arc<dyn WindowSource>,
    pub clipboard: Arc<dyn Clipboard>,
    pub ocr: Arc<dyn Ocr>,
    pub http: Arc<dyn Http>,
    pub shell: Arc<dyn Shell>,
    pub dialogs: Arc<dyn Dialogs>,
    pub hotkeys: Arc<dyn Hotkeys>,
    pub tray: Arc<dyn Tray>,
    pub theme: Arc<dyn ThemeSource>,
    pub autostart: Arc<dyn Autostart>,
    pub selection: Arc<dyn SelectionWatcher>,
    pub audio_devices: Arc<dyn AudioDevices>,
    pub recorder: Arc<dyn RecorderControl>,
    pub ui: Arc<dyn UiPort>,
    pub export_gif: ExportGif,
    /// Ends the process's event loop.
    pub quit: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct State {
    hotkey_errors: Vec<HotkeyError>,
    hotkeys_suspended: bool,
    last_processed: Option<Value>,
    /// Text the shown bubble was raised for.
    pending_selection: Option<String>,
    /// Suppresses the bubble for text just translated.
    last_translated: Option<String>,
    ocr_busy: bool,
    /// Marks of the last overlay, `name=ms`.
    overlay_timing: Vec<String>,
    /// Set from the first moment of a start until the recorder has the session (the countdown takes
    /// 2 s): a second start then must not open a second file, and stop calls the start off.
    rec_starting: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// `set-theme` in tests.
    forced_theme: Option<Theme>,
    update: update::UpdateState,
}

pub struct App {
    pub(crate) ports: Ports,
    pub(crate) env: Environment,
    pub(crate) rt: Handle,
    state: Mutex<State>,
    pub(crate) mcp: mcp_wiring::McpSlot,
    pub(crate) diag: Diagnostics,
    /// `AppHost` methods take `&self` but spawn work that outlives the call.
    pub(crate) me: Weak<App>,
}

impl App {
    pub fn new(ports: Ports, env: Environment, rt: Handle) -> Arc<Self> {
        let diag = Diagnostics::new(&env.temp_dir);
        Arc::new_cyclic(|me| Self {
            ports,
            env,
            rt,
            state: Mutex::default(),
            mcp: mcp_wiring::McpSlot::default(),
            diag,
            me: me.clone(),
        })
    }

    /// Startup after the UI thread runs. `argv` decides the initial capture.
    pub fn start(self: &Arc<Self>, argv: &[String]) {
        self.ports.settings.get();
        self.init_tray();
        self.register_hotkeys();
        self.start_mcp();
        let app = self.clone();
        self.ports.theme.watch(Arc::new(move |theme| app.ports.ui.apply_theme(theme)));
        self.init_recorder();
        let capture = self.ports.capture.clone();
        self.spawn(move |_| capture.warm_up());
        self.start_selection_watcher();
        self.start_update_schedule();

        if !argv.iter().any(|a| a == "--hidden") {
            self.ports.ui.show_main();
        }
        self.announce_update_result(argv);
        if let Some(mode) = cli_capture_mode(argv) {
            self.spawn(move |app| {
                std::thread::sleep(std::time::Duration::from_millis(400));
                app.run_capture_mode(&mode);
            });
        }
    }

    /// A second launch forwarded its arguments.
    pub fn second_instance(self: &Arc<Self>, argv: &[String]) {
        match cli_capture_mode(argv) {
            Some(mode) => self.spawn(move |app| app.run_capture_mode(&mode)),
            None => self.ports.ui.show_main(),
        }
    }

    pub fn host(self: &Arc<Self>) -> Arc<dyn AppHost> {
        self.clone()
    }

    pub(crate) fn run_capture_mode(self: &Arc<Self>, mode: &str) {
        match mode {
            "region" => self.trigger_region(false),
            "full" => self.trigger_full(),
            "repeat" => self.trigger_repeat(),
            "text" => self.trigger_text(false),
            "record" => self.trigger_record(),
            "audio" => self.trigger_record_audio(),
            "translate" => self.trigger_translate_selection(),
            _ => self.ports.ui.show_main(),
        }
    }

    pub(crate) fn fire_hotkey(self: &Arc<Self>, action: HotkeyAction) {
        self.spawn(move |app| match action {
            HotkeyAction::Region => app.trigger_region(false),
            HotkeyAction::Fullscreen => app.trigger_full(),
            HotkeyAction::RepeatLast => app.trigger_repeat(),
            HotkeyAction::TextGrab => app.trigger_text(false),
            HotkeyAction::Record => app.trigger_record(),
            HotkeyAction::RecordAudio => app.trigger_record_audio(),
            HotkeyAction::TranslateSelection => app.trigger_translate_selection(),
        });
    }

    /// Runs slow work off the caller's thread.
    pub(crate) fn spawn(self: &Arc<Self>, work: impl FnOnce(Arc<App>) + Send + 'static) {
        let app = self.clone();
        self.rt.spawn_blocking(move || work(app));
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn settings(&self) -> Settings {
        self.ports.settings.get()
    }

    pub(crate) fn t(&self, key: &str) -> String {
        self.t_args(key, &[])
    }

    pub(crate) fn t_args(&self, key: &str, args: &[&str]) -> String {
        i18n::t(self.lang(), key, args)
    }

    pub(crate) fn lang(&self) -> &'static str {
        i18n::resolve_lang(&self.settings().language, &self.env.locale)
    }

    pub(crate) fn set_last_processed(&self, action: &str, file: Option<&Path>) -> Value {
        let value = json!({
            "action": action,
            "file": file.map(|f| f.to_string_lossy().into_owned()),
            "ts": chrono::Utc::now().timestamp_millis(),
        });
        self.state().last_processed = Some(value.clone());
        value
    }

    pub(crate) fn theme(&self) -> Theme {
        self.state().forced_theme.clone().unwrap_or_else(|| self.ports.theme.current())
    }
}

/// `--capture <mode>` or `--capture=<mode>`; bare `--capture` means region.
pub fn cli_capture_mode(argv: &[String]) -> Option<String> {
    let at = argv.iter().position(|a| a == "--capture" || a.starts_with("--capture="))?;
    let arg = &argv[at];
    Some(match arg.split_once('=') {
        Some((_, mode)) => mode.to_string(),
        None => argv.get(at + 1).cloned().unwrap_or_else(|| "region".to_string()),
    })
}

#[cfg(test)]
mod tests;
