//! The boundaries of the application. Implementations: `shotik-win` (real), `shotik-fake` (tests,
//! ghost mode), `shotik-ui` ([`UiPort`]), `shotik-recorder` ([`RecorderControl`]), `shotik-app`
//! ([`AppHost`]).
//!
//! Ports are synchronous and thread-safe; callers run slow ones on a blocking thread. Something that
//! reports later takes a callback, so this crate needs no async runtime.
//!
//! This file is shared by all crates: additive changes only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::frame::Frame;
use crate::geom::{Display, DisplayId, Point, PointF, Rect};
use crate::history::HistoryEntry;
use crate::settings::Settings;

pub type Callback<T> = Arc<dyn Fn(T) + Send + Sync>;
pub type Once<T> = Box<dyn FnOnce(T) + Send>;

// ---------- persistent state ----------

pub trait SettingsStore: Send + Sync {
    fn get(&self) -> Settings;
    /// Deep-merges the patch, saves, returns the result. Rules: `deepMerge` in `src/main/settings.js`.
    fn set(&self, patch: Value) -> Result<Settings>;
}

/// Which entries a history query returns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryFilter {
    #[default]
    All,
    Images,
    Recordings,
    /// Recordings of one kind.
    Audio,
    Video,
}

/// Screenshot and recording history. Reference: `src/main/history.js`.
pub trait HistoryStore: Send + Sync {
    /// Saves to `dir` (default: `settings.saveDir`), writes a thumbnail, adds an entry.
    fn save_png(&self, png: &[u8], image: &Frame, source: &str, dir: Option<&Path>) -> Result<HistoryEntry>;
    /// Registers a finished recording file.
    fn add_recording(&self, done: &RecordingDone) -> Result<HistoryEntry>;
    /// Newest first. Entries whose file is gone are dropped first.
    fn list(&self, limit: usize, filter: HistoryFilter) -> Vec<HistoryEntry>;
    fn get(&self, id: &str) -> Option<HistoryEntry>;
    fn remove(&self, id: &str) -> Result<()>;
    fn clear(&self, delete_files: bool) -> Result<()>;
}

// ---------- displays and capture ----------

pub trait Displays: Send + Sync {
    fn all(&self) -> Vec<Display>;
    fn primary(&self) -> Display;
    /// Cursor position in global DIP.
    fn cursor(&self) -> PointF;
    /// The display containing or nearest to a global DIP point.
    fn nearest(&self, point: PointF) -> Display;
    /// Physical pixels to global DIP.
    fn phys_to_dip(&self, point: Point) -> PointF;
    /// A physical rectangle to global DIP, on the display holding most of it: floored origin, enclosing
    /// size. `screen.screenToDipRect(null, rect)` in the JS version, which `list_windows` reports.
    fn phys_rect_to_dip(&self, rect: Rect) -> Rect {
        let origin = self.phys_to_dip(Point { x: rect.x, y: rect.y });
        let scale = self.nearest(origin).scale_factor;
        Rect::new(
            origin.x.floor() as i32,
            origin.y.floor() as i32,
            (f64::from(rect.width) / scale).ceil() as i32,
            (f64::from(rect.height) / scale).ceil() as i32,
        )
    }

    fn under_cursor(&self) -> Display {
        self.nearest(self.cursor())
    }

    /// An unknown id means the primary display: a remembered monitor may have been unplugged.
    fn by_id(&self, id: DisplayId) -> Display {
        self.all().into_iter().find(|d| d.id == id).unwrap_or_else(|| self.primary())
    }
}

pub trait ScreenCapture: Send + Sync {
    /// One display at full physical resolution.
    fn grab(&self, display: &Display) -> Result<Frame>;
    /// Keeps the first real capture fast. Optional.
    fn warm_up(&self) {}
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NativeWindow {
    /// HWND on Windows. Ephemeral: the OS recycles them.
    pub id: u64,
    pub title: String,
    pub app: Option<String>,
    /// Physical pixels without the DWM shadow. None while minimized: there are no true bounds then.
    pub bounds: Option<Rect>,
    pub minimized: bool,
}

/// Which window capture method produced an image. Reported to MCP clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowCaptureMethod {
    /// Reads the window's own surface even when covered. Can come back blank on GPU-only surfaces.
    PrintWindow,
    /// Second attempt for such surfaces.
    GraphicsCapture,
}

pub trait WindowSource: Send + Sync {
    /// Visible top-level windows, topmost first.
    fn list(&self, exclude_pid: Option<u32>) -> Result<Vec<NativeWindow>>;
    fn capture(&self, id: u64, method: WindowCaptureMethod) -> Result<Frame>;
}

// ---------- clipboard, OCR, shell ----------

pub trait Clipboard: Send + Sync {
    fn write_image(&self, image: &Frame) -> Result<()>;
    /// One clipboard entry with both formats: terminals paste the path, image apps the picture.
    fn write_image_and_text(&self, image: &Frame, text: &str) -> Result<()>;
    fn write_text(&self, text: &str) -> Result<()>;
    fn read_text(&self) -> Result<String>;
    fn read_image(&self) -> Result<Option<Frame>>;
    fn clear(&self) -> Result<()>;
    /// Names of the available formats, for tests and diagnostics.
    fn formats(&self) -> Result<Vec<String>>;
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OcrWord {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OcrLine {
    pub text: String,
    pub words: Vec<OcrWord>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OcrBoxes {
    pub lines: Vec<OcrLine>,
}

/// Raw engine access. Language choice and upscaling are core logic on top of this.
pub trait Ocr: Send + Sync {
    /// `lang`: a BCP-47 tag such as "ru" or "en-US"; None means the user's profile languages.
    fn recognize(&self, png: &[u8], lang: Option<&str>) -> Result<String>;
    fn recognize_boxes(&self, png: &[u8], lang: Option<&str>) -> Result<OcrBoxes>;
}

pub struct HttpRequest<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: Option<&'a [u8]>,
    pub timeout_ms: u64,
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Transport for translation, so translation logic is testable without a network.
pub trait Http: Send + Sync {
    fn send(&self, request: HttpRequest<'_>) -> Result<HttpResponse>;
}

pub trait Shell: Send + Sync {
    fn reveal_in_folder(&self, path: &Path) -> Result<()>;
    fn open_path(&self, path: &Path) -> Result<()>;
    fn open_url(&self, url: &str) -> Result<()>;
}

pub trait Dialogs: Send + Sync {
    /// None means the user cancelled.
    fn save_png(&self, default_path: &Path) -> Result<Option<PathBuf>>;
    fn pick_directory(&self, default_path: &Path) -> Result<Option<PathBuf>>;
}

// ---------- hotkeys, tray, theme, autostart ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HotkeyAction {
    Region,
    Fullscreen,
    RepeatLast,
    TextGrab,
    Record,
    RecordAudio,
    TranslateSelection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub action: HotkeyAction,
    /// In the settings.json format, e.g. "Ctrl+Shift+PrintScreen".
    pub accelerator: String,
}

/// A combo that could not be registered, shown to the user as a conflict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyError {
    pub name: HotkeyAction,
    pub acc: String,
}

pub trait Hotkeys: Send + Sync {
    /// Replaces all registrations. Returns the combos that failed.
    fn register(&self, bindings: &[HotkeyBinding], fired: Callback<HotkeyAction>) -> Vec<HotkeyError>;
    fn unregister_all(&self);
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrayItem {
    Action { id: String, label: String, sublabel: Option<String> },
    Separator,
}

pub trait Tray: Send + Sync {
    fn set_menu(&self, items: Vec<TrayItem>) -> Result<()>;
    fn set_tooltip(&self, tooltip: &str) -> Result<()>;
    /// Handlers: a menu item chosen (by id), and a click on the icon itself.
    fn set_handlers(&self, item: Callback<String>, icon_clicked: Callback<()>);
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub dark: bool,
    /// "#RRGGBB".
    pub accent: String,
    /// "win32" or "darwin", as the JS version reports it.
    pub platform: String,
}

pub trait ThemeSource: Send + Sync {
    fn current(&self) -> Theme;
    fn watch(&self, changed: Callback<Theme>);
}

pub trait Autostart: Send + Sync {
    /// Starts with `--hidden` at login.
    fn set(&self, enabled: bool) -> Result<()>;
}

// ---------- selection watcher ----------

/// What the background watcher reports. Coordinates are physical pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectionEvent {
    /// A stable text selection read through UI Automation.
    Selected { text: String, x: i32, y: i32 },
    /// The selection went away.
    Cleared,
    /// Left mouse press anywhere, for click-away dismiss.
    MouseDown { x: i32, y: i32 },
    /// A drag or double-click in an app that exposes no text: try OCR on the highlight.
    Gesture { x1: i32, y1: i32, x2: i32, y2: i32, double: bool },
}

pub trait SelectionWatcher: Send + Sync {
    /// Read-only: never moves the mouse, clicks or touches the clipboard.
    fn start(&self, events: Callback<SelectionEvent>) -> Result<()>;
    fn stop(&self);
    /// Synthetic Ctrl+C for the translate hotkey. The only part that sends input.
    fn send_copy(&self) -> Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

pub trait AudioDevices: Send + Sync {
    fn microphones(&self) -> Result<Vec<AudioDevice>>;
}

// ---------- recording ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordTarget {
    Region { display_id: DisplayId, rect_phys: Rect, rect_dip: Rect },
    AudioOnly,
}

/// Which inputs a session really got. Field names are part of the test contract.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecAudioInfo {
    pub system: bool,
    pub microphone: bool,
    pub tracks: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecLevels {
    pub system: f32,
    pub microphone: f32,
}

/// Same fields as `recorder.status()` in the JS version.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecStatus {
    pub recording: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    /// Set once capture really runs; null while the session starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<RecAudioInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub levels: Option<RecLevels>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub trace: Vec<String>,
}

/// A finished recording, ready for history.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordingDone {
    pub file: PathBuf,
    pub audio_only: bool,
    /// JPEG of the first frame.
    pub first_frame_jpeg: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub duration_sec: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecorderEvent {
    /// The session state changed: started, paused, resumed, stopped.
    State(RecStatus),
    Levels(RecLevels),
    Done(RecordingDone),
    /// Message for the user. Known prefixes: "microphone:", "audio:none", "audio:system-unavailable".
    Error(String),
}

/// Inputs for one session, from settings.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordOptions {
    pub fps: u32,
    pub format: String,
    pub system_audio: bool,
    pub microphone: bool,
    pub microphone_device_id: String,
    /// Directory the finished file goes to.
    pub save_dir: PathBuf,
}

pub trait RecorderControl: Send + Sync {
    /// Ok(None) when a session already runs or is starting: one at a time.
    fn start(&self, target: RecordTarget, options: RecordOptions) -> Result<Option<RecStatus>>;
    fn stop(&self);
    fn pause(&self);
    fn resume(&self);
    fn cancel(&self);
    /// True from the first moment of a start until the file is finalized.
    fn is_recording(&self) -> bool;
    fn status(&self) -> RecStatus;
    fn set_listener(&self, events: Callback<RecorderEvent>);
}

// ---------- UI ----------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OverlayMode {
    #[default]
    Normal,
    /// `ask_user_to_select_region`: shows the badge, returns to the agent.
    Claude,
    /// Starts in Live Text.
    Text,
    /// Selecting a region to record.
    Record,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayActionKind {
    Copy,
    Claude,
    Save,
    Pin,
    Ocr,
    Copytext,
    Record,
}

/// What the user chose in the overlay.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayAction {
    pub action: OverlayActionKind,
    /// The selection with annotations burned in.
    pub png: Option<Vec<u8>>,
    pub text: Option<String>,
    pub display_id: Option<DisplayId>,
    pub rect_phys: Option<Rect>,
    /// Display-relative DIP.
    pub rect_dip: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OverlayOutcome {
    Cancelled,
    Done(OverlayAction),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Toast {
    /// image, claude, text, repeat, video, audio, gif, color.
    pub kind: String,
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb: Option<PathBuf>,
    /// Opened in its folder when the toast is clicked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PinRequest {
    pub file: PathBuf,
    /// Global DIP.
    pub bounds: Rect,
}

/// Content of the translate popup.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TranslatePopup {
    pub original: String,
    pub loading: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loading_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Recording chrome: border around the region (none for audio) and the controls bar.
#[derive(Clone, Debug, PartialEq)]
pub struct RecWindows {
    pub display: Display,
    /// Display-relative DIP. None for audio-only sessions.
    pub rect_dip: Option<Rect>,
    /// Inputs with a level meter: "system", "microphone".
    pub meters: Vec<String>,
}

/// Events pushed to the main window.
#[derive(Clone, Debug, PartialEq)]
pub enum MainEvent {
    HistoryChanged,
    McpLog(Value),
    RecState(RecStatus),
    ThemeChanged(Theme),
}

/// Reports overlay timing marks ("winCreate", "painted", "show") as they happen.
pub type MarkSink = Callback<&'static str>;

/// Everything other threads may ask of the UI. Calls post to the event loop and return at once.
pub trait UiPort: Send + Sync {
    /// Opens a session. `done` fires once, with the user's choice. Displays arrive through
    /// `overlay_add_display`, the one under the cursor first.
    fn overlay_begin(&self, mode: OverlayMode, marks: MarkSink, done: Once<OverlayOutcome>) -> Result<()>;
    fn overlay_add_display(&self, display: Display, frame: Frame, focused: bool) -> Result<()>;
    /// Hover-snap rectangles, physical pixels, topmost first. May arrive after the windows.
    fn overlay_set_window_rects(&self, rects: Vec<Rect>);
    fn overlay_cancel(&self);
    fn overlay_is_open(&self) -> bool;
    /// Global DIP bounds of the open overlay windows, in ghost mode with the offset applied.
    fn overlay_bounds(&self) -> Vec<Rect>;

    fn show_main(&self);
    fn hide_main(&self);
    fn main_visible(&self) -> bool;
    fn send_to_main(&self, event: MainEvent);

    fn show_toast(&self, toast: Toast);
    fn toast_visible(&self) -> bool;
    /// The last toast shown, for tests.
    fn last_toast(&self) -> Option<Toast>;

    fn show_pin(&self, pin: PinRequest);
    /// Global DIP bounds of the open pins.
    fn pin_bounds(&self) -> Vec<Rect>;

    /// Shows near the cursor, or updates in place when already shown.
    fn show_translate_popup(&self, content: TranslatePopup);
    fn update_translate_popup(&self, content: TranslatePopup);
    fn hide_translate_popup(&self);
    fn translate_popup_visible(&self) -> bool;
    fn translate_popup_bounds(&self) -> Option<Rect>;

    /// Global DIP, near the end of the selection.
    fn show_selection_bubble(&self, at: PointF);
    fn hide_selection_bubble(&self);
    fn selection_bubble_visible(&self) -> bool;
    fn selection_bubble_bounds(&self) -> Option<Rect>;

    /// The 3-2-1 before capture starts. `done` fires when it has finished.
    fn show_countdown(&self, display: Display, rect_dip: Rect, done: Once<()>);
    fn show_rec_windows(&self, windows: RecWindows);
    fn update_rec_windows(&self, paused: bool, levels: Option<RecLevels>);
    fn close_rec_windows(&self);

    fn apply_theme(&self, theme: Theme);
    fn rebuild_language(&self);

    /// Test hooks, see CONTRACT.md. `target` as listed there.
    fn ui_state(&self, target: &str, index: usize) -> Result<Value>;
    fn ui_action(&self, target: &str, index: usize, action: Value) -> Result<Value>;
    /// Renders a window to a PNG file in-process. Returns its size.
    fn capture_page(&self, target: &str, index: usize, path: &Path) -> Result<(u32, u32)>;
}

// ---------- what MCP tools ask of the application ----------

/// What `ask_user_to_select_region` got.
#[derive(Clone, Debug, PartialEq)]
pub enum RegionSelection {
    Selected { png: Vec<u8> },
    Cancelled,
    TimedOut,
}

/// The ports and application services MCP tools use. `shotik-app` builds it.
#[derive(Clone)]
pub struct McpServices {
    pub settings: Arc<dyn SettingsStore>,
    pub history: Arc<dyn HistoryStore>,
    pub displays: Arc<dyn Displays>,
    pub capture: Arc<dyn ScreenCapture>,
    pub windows: Arc<dyn WindowSource>,
    pub recorder: Arc<dyn RecorderControl>,
    pub ui: Arc<dyn UiPort>,
    /// Opens the overlay for the user, waits up to the given time, handles the result like a normal
    /// capture and reports the image. Fails when an overlay is already open.
    pub select_region: Arc<dyn Fn(u64, Once<Result<RegionSelection>>) + Send + Sync>,
}

// ---------- what the windows ask of the application ----------

/// What a window can ask the application to do. Mirrors the IPC channels of the JS version
/// (`setupAppIpc` in `src/main/main.js`, handlers in `src/main/windows.js`).
pub trait AppHost: Send + Sync {
    /// `app:state`: settings, version, mcp, mcpLog, hotkeyErrors, hasLastRegion.
    fn app_state(&self) -> Value;
    /// `settings:set`: merges the patch, applies side effects, returns settings, mcp, hotkeyErrors.
    fn settings_set(&self, patch: Value) -> Result<Value>;
    fn settings_choose_dir(&self) -> Result<Option<PathBuf>>;
    fn i18n(&self) -> Value;
    fn theme(&self) -> Theme;

    /// History entries in the `history.json` shape.
    fn history_list(&self, limit: usize) -> Vec<Value>;
    fn history_remove(&self, id: &str) -> Result<()>;
    fn history_clear(&self, delete_files: bool) -> Result<()>;
    fn history_copy(&self, id: &str) -> Result<bool>;
    fn history_copy_path(&self, id: &str) -> Result<bool>;
    fn history_pin(&self, id: &str) -> Result<bool>;
    fn history_reveal(&self, id: &str) -> Result<()>;
    fn history_open(&self, id: &str) -> Result<()>;
    fn history_ocr(&self, id: &str, done: Once<Result<Option<String>>>);
    fn history_export_gif(&self, id: &str, done: Once<Option<PathBuf>>);

    fn hotkeys_suspend(&self);
    /// The real registration of a just-recorded combo. Returns the resulting errors.
    fn hotkeys_resume(&self) -> Vec<HotkeyError>;

    fn capture_region(&self);
    fn capture_text(&self);
    fn capture_full(&self);
    fn capture_repeat(&self);
    fn capture_record(&self);
    fn capture_record_audio(&self);

    fn rec_stop(&self);
    fn rec_pause_toggle(&self);
    fn rec_cancel(&self);
    fn rec_status(&self) -> RecStatus;
    fn microphones(&self) -> Result<Vec<AudioDevice>>;

    fn open_external(&self, url: &str) -> Result<()>;
    fn open_save_dir(&self) -> Result<()>;
    fn reveal_file(&self, path: &Path) -> Result<()>;
    fn translate_text(&self, text: &str, target: Option<&str>, done: Once<Result<Value>>);
    fn copy_text(&self, text: &str) -> Result<()>;

    // overlay
    fn overlay_ocr_boxes(&self, png: Vec<u8>, done: Once<OcrBoxes>);
    fn overlay_translate(&self, text: &str, done: Once<Result<Value>>);
    /// `C` in the overlay.
    fn color_copied(&self, hex: &str);

    // small windows
    fn bubble_activated(&self);
    fn pin_copy(&self, file: &Path) -> Result<()>;
    fn pin_save_as(&self, file: &Path) -> Result<()>;
}
