//! Orchestration against fake ports: what the user gets from each action, without a window.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use shotik_core::ports::*;
use shotik_core::settings::{defaults, FileSettings};
use shotik_core::history::FileHistory;
use shotik_core::{Display, Error, Frame, Point, PointF, Rect, Result};

use crate::{cli_capture_mode, App, Environment, Ports, UpdateConfig};

fn display() -> Display {
    Display {
        id: 7,
        bounds: Rect::new(0, 0, 1920, 1080),
        work_area: Rect::new(0, 0, 1920, 1040),
        scale_factor: 1.0,
        physical_bounds: Rect::new(0, 0, 1920, 1080),
        primary: true,
    }
}

fn sample_frame() -> Frame {
    Frame::new(4, 2, vec![10; 4 * 2 * 4]).unwrap()
}

/// A second monitor, for the tests that need one.
fn second_display() -> Display {
    Display { id: 8, primary: false, physical_bounds: Rect::new(1920, 0, 1920, 1080), ..display() }
}

#[derive(Default)]
struct FakeDisplays {
    two: std::sync::atomic::AtomicBool,
}
impl Displays for FakeDisplays {
    fn all(&self) -> Vec<Display> {
        if self.two.load(std::sync::atomic::Ordering::SeqCst) {
            vec![display(), second_display()]
        } else {
            vec![display()]
        }
    }
    fn primary(&self) -> Display {
        display()
    }
    fn cursor(&self) -> PointF {
        PointF { x: 10.0, y: 10.0 }
    }
    fn nearest(&self, _point: PointF) -> Display {
        display()
    }
    fn phys_to_dip(&self, p: Point) -> PointF {
        PointF { x: f64::from(p.x), y: f64::from(p.y) }
    }
}

/// Captures of the second monitor wait at the gate while it is closed.
#[derive(Default)]
struct FakeCapture {
    closed: Mutex<bool>,
    opened: std::sync::Condvar,
    waiting: std::sync::atomic::AtomicUsize,
}
impl ScreenCapture for FakeCapture {
    fn grab(&self, display: &Display) -> Result<Frame> {
        if display.id == second_display().id {
            self.waiting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut closed = self.closed.lock().unwrap();
            while *closed {
                closed = self.opened.wait(closed).unwrap();
            }
        }
        Ok(sample_frame())
    }
}

struct NoWindows;
impl WindowSource for NoWindows {
    fn list(&self, _exclude_pid: Option<u32>) -> Result<Vec<NativeWindow>> {
        Ok(Vec::new())
    }
    fn capture(&self, _id: u64, _method: WindowCaptureMethod) -> Result<Frame> {
        Err(Error::msg("no windows"))
    }
}

#[derive(Default)]
struct FakeClipboard {
    image: Mutex<Option<Frame>>,
    text: Mutex<String>,
}
impl Clipboard for FakeClipboard {
    fn write_image(&self, image: &Frame) -> Result<()> {
        *self.image.lock().unwrap() = Some(image.clone());
        self.text.lock().unwrap().clear();
        Ok(())
    }
    fn write_image_and_text(&self, image: &Frame, text: &str) -> Result<()> {
        *self.image.lock().unwrap() = Some(image.clone());
        *self.text.lock().unwrap() = text.to_string();
        Ok(())
    }
    fn write_text(&self, text: &str) -> Result<()> {
        *self.image.lock().unwrap() = None;
        *self.text.lock().unwrap() = text.to_string();
        Ok(())
    }
    fn read_text(&self) -> Result<String> {
        Ok(self.text.lock().unwrap().clone())
    }
    fn read_image(&self) -> Result<Option<Frame>> {
        Ok(self.image.lock().unwrap().clone())
    }
    fn clear(&self) -> Result<()> {
        *self.image.lock().unwrap() = None;
        self.text.lock().unwrap().clear();
        Ok(())
    }
    fn formats(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

struct FakeOcr(&'static str);
impl Ocr for FakeOcr {
    fn recognize(&self, _png: &[u8], _lang: Option<&str>) -> Result<String> {
        Ok(self.0.to_string())
    }
    fn recognize_boxes(&self, _png: &[u8], _lang: Option<&str>) -> Result<OcrBoxes> {
        Ok(OcrBoxes::default())
    }
}

type DuringRequest = Box<dyn Fn(&str) + Send>;

/// Answers from a table. Anything else is a failed connection: tests have no network.
#[derive(Default)]
struct FakeHttp {
    routes: Mutex<std::collections::HashMap<String, (u16, Vec<u8>)>>,
    requests: Mutex<Vec<String>>,
    /// Runs while a request is under way, for what the user does meanwhile.
    during: Mutex<Option<DuringRequest>>,
}
impl Http for FakeHttp {
    fn send(&self, request: HttpRequest<'_>) -> Result<HttpResponse> {
        self.requests.lock().unwrap().push(request.url.to_string());
        if let Some(during) = self.during.lock().unwrap().as_ref() {
            during(request.url);
        }
        match self.routes.lock().unwrap().get(request.url) {
            Some((status, body)) => Ok(HttpResponse { status: *status, body: body.clone() }),
            None => Err(Error::msg("no network in tests")),
        }
    }
}

/// What was handed to an installer, read at the moment it started: it deletes nothing, but a test reads this later.
struct StartedInstaller {
    path: PathBuf,
    contents: Vec<u8>,
    install_dir: PathBuf,
    show_main: bool,
}

#[derive(Default)]
struct FakeShell {
    urls: Mutex<Vec<String>>,
    installers: Mutex<Vec<StartedInstaller>>,
    installer_error: Mutex<Option<String>>,
}
impl Shell for FakeShell {
    fn reveal_in_folder(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
    fn open_path(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
    fn open_url(&self, url: &str) -> Result<()> {
        self.urls.lock().unwrap().push(url.to_string());
        Ok(())
    }
    fn run_installer(&self, installer: &Path, install_dir: &Path, show_main: bool) -> Result<()> {
        if let Some(error) = self.installer_error.lock().unwrap().as_ref() {
            return Err(Error::msg(error.clone()));
        }
        self.installers.lock().unwrap().push(StartedInstaller {
            path: installer.to_path_buf(),
            contents: std::fs::read(installer).unwrap(),
            install_dir: install_dir.to_path_buf(),
            show_main,
        });
        Ok(())
    }
}

struct NoDialogs;
impl Dialogs for NoDialogs {
    fn save_png(&self, _default_path: &Path) -> Result<Option<PathBuf>> {
        Ok(None)
    }
    fn pick_directory(&self, _default_path: &Path) -> Result<Option<PathBuf>> {
        Ok(None)
    }
}

#[derive(Default)]
struct FakeHotkeys {
    registered: Mutex<Vec<HotkeyBinding>>,
    unregistered: Mutex<u32>,
}
impl Hotkeys for FakeHotkeys {
    fn register(&self, bindings: &[HotkeyBinding], _fired: Callback<HotkeyAction>) -> Vec<HotkeyError> {
        *self.registered.lock().unwrap() = bindings.to_vec();
        bindings
            .iter()
            .filter(|b| b.accelerator == "Taken")
            .map(|b| HotkeyError { name: b.action, acc: b.accelerator.clone() })
            .collect()
    }
    fn unregister_all(&self) {
        *self.unregistered.lock().unwrap() += 1;
        self.registered.lock().unwrap().clear();
    }
}

#[derive(Default)]
struct FakeTray {
    menu: Mutex<Vec<TrayItem>>,
    tooltip: Mutex<String>,
}
impl Tray for FakeTray {
    fn set_menu(&self, items: Vec<TrayItem>) -> Result<()> {
        *self.menu.lock().unwrap() = items;
        Ok(())
    }
    fn set_tooltip(&self, tooltip: &str) -> Result<()> {
        *self.tooltip.lock().unwrap() = tooltip.to_string();
        Ok(())
    }
    fn set_handlers(&self, _item: Callback<String>, _icon_clicked: Callback<()>) {}
}

struct FakeTheme;
impl ThemeSource for FakeTheme {
    fn current(&self) -> Theme {
        Theme { dark: false, accent: "#006FC7".into(), platform: "win32".into() }
    }
    fn watch(&self, _changed: Callback<Theme>) {}
}

#[derive(Default)]
struct FakeAutostart(Mutex<Option<bool>>);
impl Autostart for FakeAutostart {
    fn set(&self, enabled: bool) -> Result<()> {
        *self.0.lock().unwrap() = Some(enabled);
        Ok(())
    }
}

#[derive(Default)]
struct FakeSelection {
    started: Mutex<u32>,
    stopped: Mutex<u32>,
    /// An application that answers Ctrl+C after a while.
    copy_answer: Mutex<Option<(Arc<FakeClipboard>, String)>>,
}
impl SelectionWatcher for FakeSelection {
    fn start(&self, _events: Callback<SelectionEvent>) -> Result<()> {
        *self.started.lock().unwrap() += 1;
        Ok(())
    }
    fn stop(&self) {
        *self.stopped.lock().unwrap() += 1;
    }
    fn send_copy(&self) -> Result<()> {
        if let Some((clipboard, text)) = self.copy_answer.lock().unwrap().take() {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                *clipboard.text.lock().unwrap() = text;
            });
        }
        Ok(())
    }
}

struct NoAudio;
impl AudioDevices for NoAudio {
    fn microphones(&self) -> Result<Vec<AudioDevice>> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct FakeRecorder {
    recording: Mutex<bool>,
    starts: Mutex<Vec<RecordOptions>>,
    stops: Mutex<u32>,
    cancels: Mutex<u32>,
    during_start: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl RecorderControl for FakeRecorder {
    fn start(&self, _target: RecordTarget, options: RecordOptions) -> Result<Option<RecStatus>> {
        let mut recording = self.recording.lock().unwrap();
        if *recording {
            return Ok(None);
        }
        *recording = true;
        drop(recording);
        self.starts.lock().unwrap().push(options);
        if let Some(during) = self.during_start.lock().unwrap().take() {
            during();
        }
        Ok(Some(RecStatus { recording: true, ..RecStatus::default() }))
    }
    fn stop(&self) {
        *self.stops.lock().unwrap() += 1;
        *self.recording.lock().unwrap() = false;
    }
    fn pause(&self) {}
    fn resume(&self) {}
    fn cancel(&self) {
        *self.cancels.lock().unwrap() += 1;
        *self.recording.lock().unwrap() = false;
    }
    fn is_recording(&self) -> bool {
        *self.recording.lock().unwrap()
    }
    fn status(&self) -> RecStatus {
        RecStatus { recording: self.is_recording(), ..RecStatus::default() }
    }
    fn set_listener(&self, _events: Callback<RecorderEvent>) {}
}

/// Records what the application asks of the UI; an overlay session answers with `overlay_answer`.
#[derive(Default)]
struct FakeUi {
    toasts: Mutex<Vec<Toast>>,
    popups: Mutex<Vec<TranslatePopup>>,
    pins: Mutex<Vec<PinRequest>>,
    bubble: Mutex<bool>,
    popup_visible: Mutex<bool>,
    rec_windows: Mutex<Vec<RecWindows>>,
    rec_windows_closed: Mutex<u32>,
    main_shown: Mutex<u32>,
    overlay_open: Mutex<bool>,
    overlay_session: Mutex<u64>,
    overlay_answer: Mutex<Option<OverlayOutcome>>,
    overlay_displays: Mutex<Vec<(u64, bool)>>,
    pending_done: Mutex<Option<Once<OverlayOutcome>>>,
    marks: Mutex<Option<MarkSink>>,
    main_open: Mutex<bool>,
    update_pushed: Mutex<Option<Value>>,
}
impl UiPort for FakeUi {
    fn overlay_begin(&self, _mode: OverlayMode, marks: MarkSink, done: Once<OverlayOutcome>) -> Result<u64> {
        *self.overlay_open.lock().unwrap() = true;
        *self.marks.lock().unwrap() = Some(marks);
        *self.pending_done.lock().unwrap() = Some(done);
        let mut session = self.overlay_session.lock().unwrap();
        *session += 1;
        Ok(*session)
    }
    fn overlay_add_display(&self, session: u64, display: Display, _frame: Frame, focused: bool) -> Result<()> {
        if session != *self.overlay_session.lock().unwrap() {
            return Ok(()); // as the real one does: a frame of an earlier session is dropped
        }
        self.overlay_displays.lock().unwrap().push((display.id, focused));
        if focused {
            if let Some(marks) = self.marks.lock().unwrap().as_ref() {
                marks("winCreate");
                marks("painted");
                marks("show");
            }
            if let Some(answer) = self.overlay_answer.lock().unwrap().take() {
                *self.overlay_open.lock().unwrap() = false;
                if let Some(done) = self.pending_done.lock().unwrap().take() {
                    done(answer);
                }
            }
        }
        Ok(())
    }
    fn overlay_set_window_rects(&self, _rects: Vec<Rect>) {}
    fn overlay_cancel(&self) {
        *self.overlay_open.lock().unwrap() = false;
        if let Some(done) = self.pending_done.lock().unwrap().take() {
            done(OverlayOutcome::Cancelled);
        }
    }
    fn overlay_is_open(&self) -> bool {
        *self.overlay_open.lock().unwrap()
    }
    fn overlay_bounds(&self) -> Vec<Rect> {
        Vec::new()
    }
    fn show_main(&self) {
        *self.main_shown.lock().unwrap() += 1;
    }
    fn hide_main(&self) {}
    fn main_visible(&self) -> bool {
        *self.main_open.lock().unwrap()
    }
    fn send_to_main(&self, event: MainEvent) {
        if let MainEvent::UpdateStatus(status) = event {
            *self.update_pushed.lock().unwrap() = Some(status);
        }
    }
    fn show_toast(&self, toast: Toast) {
        self.toasts.lock().unwrap().push(toast);
    }
    fn toast_visible(&self) -> bool {
        false
    }
    fn last_toast(&self) -> Option<Toast> {
        self.toasts.lock().unwrap().last().cloned()
    }
    fn show_pin(&self, pin: PinRequest) {
        self.pins.lock().unwrap().push(pin);
    }
    fn pin_bounds(&self) -> Vec<Rect> {
        self.pins.lock().unwrap().iter().map(|p| p.bounds).collect()
    }
    fn show_translate_popup(&self, content: TranslatePopup) {
        *self.popup_visible.lock().unwrap() = true;
        self.popups.lock().unwrap().push(content);
    }
    fn update_translate_popup(&self, content: TranslatePopup) {
        self.popups.lock().unwrap().push(content);
    }
    fn hide_translate_popup(&self) {
        *self.popup_visible.lock().unwrap() = false;
    }
    fn translate_popup_visible(&self) -> bool {
        *self.popup_visible.lock().unwrap()
    }
    fn translate_popup_bounds(&self) -> Option<Rect> {
        Some(Rect::new(100, 100, 380, 220))
    }
    fn show_selection_bubble(&self, _at: PointF) {
        *self.bubble.lock().unwrap() = true;
    }
    fn hide_selection_bubble(&self) {
        *self.bubble.lock().unwrap() = false;
    }
    fn selection_bubble_visible(&self) -> bool {
        *self.bubble.lock().unwrap()
    }
    fn selection_bubble_bounds(&self) -> Option<Rect> {
        Some(Rect::new(500, 500, 60, 60))
    }
    fn show_countdown(&self, _display: Display, _rect_dip: Rect, done: Once<()>) {
        done(());
    }
    fn show_rec_windows(&self, windows: RecWindows) {
        self.rec_windows.lock().unwrap().push(windows);
    }
    fn update_rec_windows(&self, _paused: bool, _levels: Option<RecLevels>) {}
    fn close_rec_windows(&self) {
        *self.rec_windows_closed.lock().unwrap() += 1;
    }
    fn apply_theme(&self, _theme: Theme) {}
    fn rebuild_language(&self) {}
    fn ui_state(&self, _target: &str, _index: usize) -> Result<Value> {
        Ok(Value::Null)
    }
    fn ui_action(&self, _target: &str, _index: usize, _action: Value) -> Result<Value> {
        Ok(Value::Null)
    }
    fn capture_page(&self, _target: &str, _index: usize, _path: &Path) -> Result<(u32, u32)> {
        Ok((0, 0))
    }
}

struct Rig {
    app: Arc<App>,
    ui: Arc<FakeUi>,
    clipboard: Arc<FakeClipboard>,
    recorder: Arc<FakeRecorder>,
    displays: Arc<FakeDisplays>,
    capture: Arc<FakeCapture>,
    hotkeys: Arc<FakeHotkeys>,
    tray: Arc<FakeTray>,
    autostart: Arc<FakeAutostart>,
    selection: Arc<FakeSelection>,
    http: Arc<FakeHttp>,
    shell: Arc<FakeShell>,
    quits: Arc<std::sync::atomic::AtomicU32>,
    dir: PathBuf,
    _rt: tokio::runtime::Runtime,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rig_with(name: &str, ghost: bool, test_mode: bool) -> Rig {
    rig_installed(name, ghost, test_mode, true)
}

fn rig_installed(name: &str, ghost: bool, test_mode: bool, installed: bool) -> Rig {
    let dir = std::env::temp_dir().join(format!("shotik-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let save_dir = dir.join("shots");
    let settings: Arc<FileSettings> = Arc::new(
        FileSettings::new(&dir, defaults(&dir, "ru-RU", "win32", Some(&save_dir))).unwrap(),
    );
    let history = Arc::new(FileHistory::new(&dir, settings.clone()).unwrap());
    let ui = Arc::new(FakeUi::default());
    let clipboard = Arc::new(FakeClipboard::default());
    let recorder = Arc::new(FakeRecorder::default());
    let displays = Arc::new(FakeDisplays::default());
    let capture = Arc::new(FakeCapture::default());
    let hotkeys = Arc::new(FakeHotkeys::default());
    let tray = Arc::new(FakeTray::default());
    let autostart = Arc::new(FakeAutostart::default());
    let selection = Arc::new(FakeSelection::default());
    let http = Arc::new(FakeHttp::default());
    let shell = Arc::new(FakeShell::default());
    let quits = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let quit_count = quits.clone();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let ports = Ports {
        settings,
        history,
        displays: displays.clone(),
        capture: capture.clone(),
        windows: Arc::new(NoWindows),
        clipboard: clipboard.clone(),
        ocr: Arc::new(FakeOcr("распознанный текст")),
        http: http.clone(),
        shell: shell.clone(),
        dialogs: Arc::new(NoDialogs),
        hotkeys: hotkeys.clone(),
        tray: tray.clone(),
        theme: Arc::new(FakeTheme),
        autostart: autostart.clone(),
        selection: selection.clone(),
        audio_devices: Arc::new(NoAudio),
        recorder: recorder.clone(),
        ui: ui.clone(),
        export_gif: Arc::new(|video| Ok(video.with_extension("gif"))),
        quit: Arc::new(move || {
            quit_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }),
    };
    let env = Environment {
        test_mode,
        ghost,
        fake_screen: true,
        force_controls: false,
        real_capture: false,
        translate_stub: true,
        wincap_fail: false,
        port_override: None,
        version: "2.0.0".into(),
        platform: "win32",
        locale: "ru-RU".into(),
        temp_dir: dir.clone(),
        update: UpdateConfig {
            source: shotik_core::update::UpdateSource::at(UPDATES, &update_public_key()),
            auto_check: false,
            first_check_after: Duration::ZERO,
            install_dir: installed.then(|| dir.join("Programs")),
        },
    };
    let app = App::new(ports, env, rt.handle().clone());
    Rig { app, ui, clipboard, recorder, displays, capture, hotkeys, tray, autostart, selection, http, shell, quits, dir, _rt: rt }
}

fn rig(name: &str) -> Rig {
    rig_with(name, true, true)
}

fn action(kind: OverlayActionKind) -> OverlayAction {
    OverlayAction {
        action: kind,
        png: Some(sample_frame().to_png().unwrap()),
        text: None,
        display_id: Some(7),
        rect_phys: Some(Rect::new(10, 20, 4, 2)),
        rect_dip: Some(Rect::new(10, 20, 4, 2)),
    }
}

fn eventually(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn plain_copy_puts_only_the_image_on_the_clipboard() {
    let r = rig("copy");
    let out = r.app.handle_overlay_result(action(OverlayActionKind::Copy));
    assert!(r.clipboard.image.lock().unwrap().is_some());
    assert_eq!(*r.clipboard.text.lock().unwrap(), "", "a path here makes messengers paste text");
    assert_eq!(out["action"], "copy");
    assert!(out["file"].as_str().unwrap().ends_with(".png"));
    assert_eq!(r.ui.toasts.lock().unwrap()[0].kind, "image");
}

#[test]
fn copy_for_claude_is_image_and_path() {
    let r = rig("claude");
    r.app.handle_overlay_result(action(OverlayActionKind::Claude));
    assert!(r.clipboard.image.lock().unwrap().is_some());
    assert!(r.clipboard.text.lock().unwrap().ends_with(".png"));
    assert_eq!(r.ui.toasts.lock().unwrap()[0].kind, "claude");
}

#[test]
fn copy_without_auto_save_writes_no_file_and_says_so() {
    let r = rig("no-save");
    r.app.ports.settings.set(json!({ "autoSave": false })).unwrap();
    let out = r.app.handle_overlay_result(action(OverlayActionKind::Copy));
    assert!(out["file"].is_null());
    let toasts = r.ui.toasts.lock().unwrap();
    assert_eq!(toasts[0].body, r.app.t("noSave"));
    assert!(toasts[0].file.is_none());
}

#[test]
fn the_overlay_remembers_the_last_region_for_repeat() {
    let r = rig("repeat");
    let mut selected = action(OverlayActionKind::Copy);
    selected.rect_phys = Some(Rect::new(1, 0, 2, 2));
    r.app.handle_overlay_result(selected);
    let saved = r.app.settings().last_region.unwrap();
    assert_eq!(saved.display_id, 7);
    assert_eq!(saved.rect_phys, Rect::new(1, 0, 2, 2));

    r.app.trigger_repeat();
    let toast = r.ui.toasts.lock().unwrap().last().cloned().unwrap();
    assert_eq!(toast.kind, "repeat");
    assert_eq!(r.app.state().last_processed.as_ref().unwrap()["action"], "repeat");
}

#[test]
fn a_remembered_region_that_no_longer_fits_counts_as_no_region() {
    let r = rig("repeat-gone");
    r.app.handle_overlay_result(action(OverlayActionKind::Copy)); // region at 10,20 on a 4x2 screen
    r.app.trigger_repeat();
    let toast = r.ui.toasts.lock().unwrap().last().cloned().unwrap();
    assert_eq!(toast.title, r.app.t("noLastTitle"));
}

#[test]
fn repeat_with_no_region_explains_itself() {
    let r = rig("repeat-none");
    r.app.trigger_repeat();
    let toast = r.ui.toasts.lock().unwrap().last().cloned().unwrap();
    assert_eq!(toast.title, r.app.t("noLastTitle"));
    assert!(toast.body.contains("PrintScreen"), "{}", toast.body);
}

#[test]
fn a_pin_opens_at_the_selection_in_global_coordinates() {
    let r = rig("pin");
    r.app.handle_overlay_result(action(OverlayActionKind::Pin));
    let pins = r.ui.pins.lock().unwrap();
    assert_eq!(pins[0].bounds, Rect::new(10, 20, 4, 2));
    assert!(pins[0].file.exists());
}

#[test]
fn ocr_copies_the_text_and_reports_it() {
    let r = rig("ocr");
    r.app.handle_overlay_result(action(OverlayActionKind::Ocr));
    assert_eq!(*r.clipboard.text.lock().unwrap(), "распознанный текст");
    assert_eq!(r.ui.toasts.lock().unwrap()[0].title, r.app.t("ocrDoneTitle"));
}

#[test]
fn the_overlay_session_shows_the_cursor_display_first_and_records_marks() {
    let r = rig("session");
    *r.ui.overlay_answer.lock().unwrap() = Some(OverlayOutcome::Done(action(OverlayActionKind::Copy)));
    r.app.trigger_region(false);
    assert_eq!(r.ui.overlay_displays.lock().unwrap()[0], (7, true));
    let marks = r.app.state().overlay_timing.clone();
    for label in ["grab", "winCreate", "painted", "show"] {
        assert!(marks.iter().any(|m| m.starts_with(&format!("{label}="))), "{label} in {marks:?}");
    }
    let log = std::fs::read_to_string(r.dir.join("shotik-overlay-timing.log")).unwrap();
    assert!(log.contains("show="), "{log}");
    assert_eq!(r.app.state().last_processed.as_ref().unwrap()["action"], "copy");
}

#[test]
fn a_cancelled_overlay_leaves_no_trace() {
    let r = rig("cancel");
    *r.ui.overlay_answer.lock().unwrap() = Some(OverlayOutcome::Cancelled);
    r.app.trigger_region(false);
    assert!(r.app.state().last_processed.is_none());
    assert!(r.ui.toasts.lock().unwrap().is_empty());
}

#[test]
fn a_second_overlay_is_ignored_while_one_is_open() {
    let r = rig("busy");
    *r.ui.overlay_open.lock().unwrap() = true;
    r.app.trigger_region(false);
    assert!(r.ui.overlay_displays.lock().unwrap().is_empty());
}

#[test]
fn a_selection_raises_the_bubble_and_a_click_translates_it() {
    let r = rig("bubble");
    r.app.on_selection_detected("hello", 900, 500);
    assert!(*r.ui.bubble.lock().unwrap());
    r.app.bubble_activate();
    assert!(!*r.ui.bubble.lock().unwrap());
    assert!(eventually(|| r.ui.popups.lock().unwrap().len() == 2));
    let popups = r.ui.popups.lock().unwrap();
    assert!(popups[0].loading);
    assert_eq!(popups[1].translated.as_deref(), Some("‹ru› hello"));
}

#[test]
fn the_same_selection_does_not_raise_the_bubble_again_until_cleared() {
    let r = rig("again");
    r.app.on_selection_detected("hello", 1, 1);
    r.app.bubble_activate();
    assert!(eventually(|| r.ui.popups.lock().unwrap().len() == 2));
    r.app.on_selection_detected("hello", 1, 1);
    assert!(!*r.ui.bubble.lock().unwrap(), "just translated");
    r.app.clear_selection_bubble();
    r.app.on_selection_detected("hello", 1, 1);
    assert!(*r.ui.bubble.lock().unwrap(), "cleared, so it may rise again");
}

#[test]
fn the_translate_hotkey_waits_for_a_slow_application_and_restores_the_clipboard() {
    let r = rig_with("slow-copy", true, false);
    *r.clipboard.text.lock().unwrap() = "what the user had".into();
    *r.selection.copy_answer.lock().unwrap() = Some((r.clipboard.clone(), "selected words".into()));
    r.app.trigger_translate_selection();
    assert_eq!(r.ui.popups.lock().unwrap()[0].original, "selected words");
    assert_eq!(*r.clipboard.text.lock().unwrap(), "what the user had");
}

#[test]
fn a_clear_racing_with_a_click_still_translates() {
    let r = rig("race");
    r.app.on_selection_detected("hello", 1, 1);
    r.app.clear_selection_bubble();
    r.app.bubble_activate();
    assert!(eventually(|| r.ui.popups.lock().unwrap().len() == 2));
}

#[test]
fn a_click_away_hides_the_bubble_and_popup_but_a_click_on_them_does_not() {
    let r = rig("click-away");
    r.app.on_selection_detected("hello", 1, 1);
    *r.ui.popup_visible.lock().unwrap() = true;
    r.app.on_mouse_down(Point { x: 510, y: 510 }); // on the bubble, outside the popup
    assert!(*r.ui.bubble.lock().unwrap());
    assert!(!*r.ui.popup_visible.lock().unwrap());
    r.app.on_mouse_down(Point { x: 5, y: 5 });
    assert!(!*r.ui.bubble.lock().unwrap());
    assert!(r.app.state().pending_selection.is_none());
}

#[test]
fn audio_recording_with_both_inputs_off_explains_instead_of_recording_silence() {
    let r = rig("audio-off");
    r.app.ports.settings.set(json!({ "audioRecording": { "systemAudio": false, "microphone": false } })).unwrap();
    r.app.trigger_record_audio();
    assert!(r.recorder.starts.lock().unwrap().is_empty());
    assert_eq!(r.ui.toasts.lock().unwrap()[0].body, r.app.t("recAudioNone"));
}

#[test]
fn audio_recording_uses_its_own_toggles_and_the_shared_microphone_device() {
    let r = rig("audio");
    r.app.ports.settings.set(json!({
        "audioRecording": { "systemAudio": true, "microphone": false },
        "recording": { "microphone": true, "microphoneDeviceId": "mic-7" },
    })).unwrap();
    r.app.trigger_record_audio();
    let starts = r.recorder.starts.lock().unwrap();
    assert!(starts[0].system_audio);
    assert!(!starts[0].microphone);
    assert_eq!(starts[0].microphone_device_id, "mic-7");
}

#[test]
fn the_record_trigger_stops_a_running_session() {
    let r = rig("stop");
    r.app.trigger_record_audio();
    assert!(r.app.is_recording());
    r.app.trigger_record();
    assert_eq!(*r.recorder.stops.lock().unwrap(), 1);
    assert!(!r.app.is_recording());
}

#[test]
fn a_stop_that_arrives_while_the_recorder_starts_is_not_lost() {
    let r = rig("start-race");
    let app = r.app.clone();
    *r.recorder.during_start.lock().unwrap() = Some(Box::new(move || app.stop_recording()));
    let started = r.app.start_recording(RecordTarget::AudioOnly).unwrap();
    assert!(started.is_none(), "it was called off");
    assert_eq!(*r.recorder.cancels.lock().unwrap(), 1);
    assert!(!r.app.is_recording());
}

#[test]
fn losing_an_audio_input_keeps_the_controls_of_the_recording_that_goes_on() {
    let r = rig("audio-lost");
    r.app.start_recording(RecordTarget::AudioOnly).unwrap();
    r.app.on_recorder_event(RecorderEvent::Error("audio:system-unavailable".into()));
    assert_eq!(*r.ui.rec_windows_closed.lock().unwrap(), 0, "the recording goes on");
    assert_eq!(r.ui.toasts.lock().unwrap().len(), 1, "and the user is told");

    r.recorder.stop();
    r.app.on_recorder_event(RecorderEvent::Error("the encoder failed".into()));
    assert_eq!(*r.ui.rec_windows_closed.lock().unwrap(), 1, "a failure that ended it closes them");
}

#[test]
fn each_frame_carries_the_session_it_was_captured_for() {
    use std::sync::atomic::Ordering::SeqCst;
    let r = rig("late-frame");
    r.displays.two.store(true, SeqCst);
    *r.capture.closed.lock().unwrap() = true;
    *r.ui.overlay_answer.lock().unwrap() = Some(OverlayOutcome::Cancelled);
    assert!(matches!(r.app.run_overlay(OverlayMode::Normal), crate::overlay::OverlayResult::Cancelled));
    let wait_for = |count: usize| {
        for _ in 0..500 {
            if r.capture.waiting.load(SeqCst) == count {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(r.capture.waiting.load(SeqCst), count, "captures on their way");
    };
    wait_for(1);
    let second = {
        let app = r.app.clone();
        std::thread::spawn(move || app.run_overlay(OverlayMode::Normal))
    };
    wait_for(2);
    *r.capture.closed.lock().unwrap() = false;
    r.capture.opened.notify_all();
    let count = || r.ui.overlay_displays.lock().unwrap().iter().filter(|(id, _)| *id == second_display().id).count();
    for _ in 0..500 {
        if count() > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(count(), 1, "one window for the second monitor, not one more with the old frame");
    r.app.ports.ui.overlay_cancel();
    second.join().unwrap();
}

#[test]
fn stopping_during_a_start_calls_it_off() {
    let r = rig("start-cancel");
    let pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    r.app.state().rec_starting = Some(pending.clone());
    assert!(r.app.is_recording());
    r.app.stop_recording();
    assert!(pending.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(*r.recorder.stops.lock().unwrap(), 0);
}

#[test]
fn recording_windows_appear_outside_ghost_mode_with_a_meter_per_input() {
    let r = rig_with("rec-windows", false, true);
    r.app.ports.settings.set(json!({ "recording": { "systemAudio": true, "microphone": false, "countdown": false } })).unwrap();
    r.app.start_recording(RecordTarget::Region {
        display_id: 7,
        rect_phys: Rect::new(0, 0, 100, 100),
        rect_dip: Rect::new(0, 0, 100, 100),
    }).unwrap();
    let windows = r.ui.rec_windows.lock().unwrap();
    assert_eq!(windows[0].meters, ["system"]);
}

#[test]
fn hotkeys_are_never_registered_in_ghost_mode() {
    let r = rig("ghost-keys");
    r.app.register_hotkeys();
    assert!(r.hotkeys.registered.lock().unwrap().is_empty());
}

#[test]
fn hotkeys_register_outside_ghost_mode_and_conflicts_are_reported() {
    let r = rig_with("keys", false, true);
    r.app.register_hotkeys();
    assert_eq!(r.hotkeys.registered.lock().unwrap().len(), 7);
    r.app.apply_settings(json!({ "hotkeys": { "region": "Taken", "record": "" } })).unwrap();
    assert_eq!(r.hotkeys.registered.lock().unwrap().len(), 6, "an empty combo is not registered");
    assert_eq!(r.app.state().hotkey_errors[0].acc, "Taken");
}

#[test]
fn suspended_hotkeys_stay_off_until_resumed() {
    let r = rig_with("suspend", false, true);
    r.app.register_hotkeys();
    r.app.suspend_hotkeys();
    assert!(r.hotkeys.registered.lock().unwrap().is_empty());
    r.app.register_hotkeys();
    assert!(r.hotkeys.registered.lock().unwrap().is_empty());
    r.app.resume_hotkeys();
    assert_eq!(r.hotkeys.registered.lock().unwrap().len(), 7);
}

#[test]
fn the_tray_menu_offers_stop_only_while_recording() {
    let r = rig("tray");
    r.app.init_tray();
    let has = |id: &str| {
        r.tray.menu.lock().unwrap().iter().any(|i| matches!(i, TrayItem::Action { id: x, .. } if x == id))
    };
    assert!(has("record"));
    assert!(!has("stop-rec"));
    r.app.trigger_record_audio();
    r.app.refresh_tray();
    assert!(has("stop-rec"));
    assert!(!has("record"));
    assert_eq!(*r.tray.tooltip.lock().unwrap(), r.app.t("trayRecTooltip"));
}

#[test]
fn settings_changes_reach_their_side_effects() {
    let r = rig_with("settings", false, false);
    r.app.apply_settings(json!({ "launchAtStartup": true })).unwrap();
    assert_eq!(*r.autostart.0.lock().unwrap(), Some(true));
    r.app.apply_settings(json!({ "translate": { "autoBubble": false } })).unwrap();
    assert_eq!(*r.selection.stopped.lock().unwrap(), 1);
    r.app.apply_settings(json!({ "translate": { "autoBubble": true } })).unwrap();
    assert_eq!(*r.selection.started.lock().unwrap(), 1);
}

#[test]
fn only_web_addresses_open_externally() {
    let r = rig("external");
    assert!(r.app.open_external("https://example.com").is_ok());
    assert!(r.app.open_external("file:///C:/Windows/system32/calc.exe").is_err());
}

#[test]
fn test_routes_answer_like_the_js_version() {
    let r = rig("routes");
    assert_eq!(r.app.test_route("ping", json!({})).unwrap(), "pong");
    assert!(r.app.test_route("nonsense", json!({})).is_err());
    let state = r.app.test_route("state", json!({})).unwrap();
    assert_eq!(state["overlayOpen"], false);
    assert_eq!(state["displays"][0]["id"], 7);
    assert!(state["displays"][0]["workArea"].is_object());
    let merged = r.app.test_route("settings-set", json!({ "patch": { "showToast": false } })).unwrap();
    assert_eq!(merged["settings"]["showToast"], false);
}

#[test]
fn sim_action_restores_the_users_clipboard() {
    let r = rig("sim");
    r.clipboard.write_text("the user's text").unwrap();
    let png = sample_frame().to_png().unwrap();
    let encoded = encode(&png);
    let reply = r.app.test_route("sim-action", json!({ "action": "claude", "pngBase64": encoded })).unwrap();
    assert_eq!(reply["hasImage"], true);
    assert!(reply["text"].as_str().unwrap().ends_with(".png"));
    assert_eq!(*r.clipboard.text.lock().unwrap(), "the user's text");
    assert!(r.clipboard.image.lock().unwrap().is_none());
}

fn encode(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        out.push(A[(b[0] >> 2) as usize] as char);
        out.push(A[(((b[0] & 3) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 { A[(((b[1] & 15) << 2) | (b[2] >> 6)) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(b[2] & 63) as usize] as char } else { '=' });
    }
    out
}

#[test]
fn recording_diagnostics_never_invent_success() {
    let r = rig("diagnostics");
    let empty = r.app.test_route("ui-state", json!({ "target": "rec-controls" })).unwrap();
    assert!(empty["diagnostics"]["audioCodec"].is_null());
    assert_eq!(empty["diagnostics"]["indexed"], false);
    assert!(empty["diagnostics"]["screenVideoTracks"].is_null());
}

#[test]
fn cli_capture_mode_reads_both_spellings() {
    let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(cli_capture_mode(&args(&["x", "--capture", "full"])).as_deref(), Some("full"));
    assert_eq!(cli_capture_mode(&args(&["x", "--capture=text"])).as_deref(), Some("text"));
    assert_eq!(cli_capture_mode(&args(&["x", "--capture"])).as_deref(), Some("region"));
    assert_eq!(cli_capture_mode(&args(&["x", "--hidden"])), None);
}

// ---------- updates ----------

const UPDATES: &str = "https://updates.test";
const INSTALLER: &[u8] = b"the real installer";

fn update_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn update_public_key() -> String {
    hex(update_key().verifying_key().as_bytes())
}

fn signed_with(key: &ed25519_dalek::SigningKey, manifest: &[u8]) -> Vec<u8> {
    use ed25519_dalek::Signer;
    let mut message = b"shotik-update-manifest-v1\n".to_vec();
    message.extend_from_slice(manifest);
    hex(&key.sign(&message).to_bytes()).into_bytes()
}

fn manifest_of(version: &str, installer: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    format!(r#"{{"version":"{version}","sha256":"{}","size":{}}}"#, hex(&sha2::Sha256::digest(installer)), installer.len())
        .into_bytes()
}

impl Rig {
    /// Publishes a correctly signed release on the fake server.
    fn publish(&self, version: &str) {
        self.publish_signed_by(version, &update_key());
        self.http.routes.lock().unwrap().insert(format!("{UPDATES}/Shotik-Setup-{version}.exe"), (200, INSTALLER.to_vec()));
    }

    fn publish_signed_by(&self, version: &str, key: &ed25519_dalek::SigningKey) {
        let manifest = manifest_of(version, INSTALLER);
        let signature = signed_with(key, &manifest);
        let mut routes = self.http.routes.lock().unwrap();
        routes.insert(format!("{UPDATES}/latest.json"), (200, manifest));
        routes.insert(format!("{UPDATES}/latest.json.sig"), (200, signature));
    }

    fn toasts(&self) -> Vec<Toast> {
        self.ui.toasts.lock().unwrap().clone()
    }

    fn quit_count(&self) -> u32 {
        self.quits.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn tray_has_update(&self) -> bool {
        self.tray.menu.lock().unwrap().iter().any(|i| matches!(i, TrayItem::Action { id, .. } if id == "update"))
    }

    fn requests_of(&self, part: &str) -> usize {
        self.http.requests.lock().unwrap().iter().filter(|url| url.contains(part)).count()
    }

    fn download_folders(&self) -> usize {
        std::fs::read_dir(&self.dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("shotik-update-"))
            .count()
    }
}

#[test]
fn a_newer_signed_release_is_offered_and_announced_once() {
    let r = rig("update-offer");
    r.publish("2.1.0");
    r.app.check_for_update(false);
    let status = r.app.update_status();
    assert_eq!(status["available"], "2.1.0");
    assert_eq!(status["current"], "2.0.0");
    assert_eq!(status["checked"], true);
    assert_eq!(status["checking"], false);
    assert_eq!(*r.ui.update_pushed.lock().unwrap(), Some(status), "the main window hears about it");
    assert!(r.tray_has_update());
    let toasts = r.toasts();
    assert_eq!(toasts.len(), 1);
    assert_eq!(toasts[0].kind, "update");
    assert!(toasts[0].title.contains("2.1.0"), "{}", toasts[0].title);
    assert_eq!(toasts[0].action.as_deref(), Some(r.app.t("updateAction").as_str()));

    r.app.check_for_update(false);
    assert_eq!(r.toasts().len(), 1, "a toast says it once; the tray and main window keep saying it");
    r.app.check_for_update(true);
    assert_eq!(r.toasts().len(), 2, "unless the user asks");
    r.publish("2.2.0");
    r.app.check_for_update(false);
    assert_eq!(r.toasts().len(), 3, "a newer version is news again");
    assert_eq!(r.app.update_status()["available"], "2.2.0");
}

#[test]
fn turned_off_toasts_leave_the_offer_in_the_tray() {
    let r = rig("update-quiet");
    r.app.ports.settings.set(json!({ "showToast": false })).unwrap();
    r.publish("2.1.0");
    r.app.check_for_update(false);
    assert!(r.toasts().is_empty());
    assert!(r.tray_has_update());
    r.app.check_for_update(true);
    assert_eq!(r.toasts().len(), 1, "the user asked, so the user hears");
}

#[test]
fn the_same_or_an_older_release_is_not_offered() {
    for version in ["2.0.0", "1.3.1", "1.99.99"] {
        let r = rig(&format!("update-old-{version}"));
        r.publish(version);
        r.app.check_for_update(false);
        assert_eq!(r.app.update_status()["available"], Value::Null, "{version}");
        assert_eq!(r.app.update_status()["checked"], true);
        assert!(r.toasts().is_empty());
        assert!(!r.tray_has_update());
    }
}

#[test]
fn a_release_taken_back_stops_being_offered() {
    let r = rig("update-pulled");
    r.publish("2.1.0");
    r.app.check_for_update(false);
    r.publish("2.0.0");
    r.app.check_for_update(false);
    assert_eq!(r.app.update_status()["available"], Value::Null);
    assert!(!r.tray_has_update());
}

#[test]
fn asking_when_up_to_date_gets_an_answer() {
    let r = rig("update-latest");
    r.publish("2.0.0");
    r.app.check_for_update(true);
    assert_eq!(r.toasts()[0].title, r.app.t("updateLatestTitle"));
}

#[test]
fn a_manifest_signed_by_someone_else_is_refused() {
    let r = rig("update-forged");
    r.publish_signed_by("9.9.9", &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));
    r.app.check_for_update(false);
    let status = r.app.update_status();
    assert_eq!(status["available"], Value::Null);
    assert_eq!(status["checked"], false);
    assert!(status["error"].as_str().unwrap().contains("signature"), "{status}");
    assert!(r.toasts().is_empty(), "nothing to bother the user with in the background");
    assert!(!r.tray_has_update());

    r.app.check_for_update(true);
    assert_eq!(r.toasts().last().unwrap().kind, "update-failed");
}

#[test]
fn a_failed_background_check_is_quiet_and_tried_again_within_the_hour() {
    let r = rig("update-offline");
    let now = std::time::SystemTime::now();
    r.app.update_tick(now);
    assert!(r.toasts().is_empty());
    assert!(r.app.update_status()["error"].as_str().unwrap().contains("no network"));
    let attempts = r.requests_of("latest.json");
    r.app.update_tick(now + Duration::from_secs(3600));
    assert!(r.requests_of("latest.json") > attempts, "a failure does not wait a day");

    r.publish("2.0.0");
    r.app.update_tick(now + Duration::from_secs(7200));
    assert_eq!(r.app.update_status()["error"], Value::Null, "a success clears it");
    let attempts = r.requests_of("latest.json");
    r.app.update_tick(now + Duration::from_secs(7200 + 3600));
    assert_eq!(r.requests_of("latest.json"), attempts, "a success stands for a day");
}

#[test]
fn a_server_error_is_an_error_with_its_status() {
    let r = rig("update-404");
    r.http.routes.lock().unwrap().insert(format!("{UPDATES}/latest.json"), (404, Vec::new()));
    r.app.check_for_update(false);
    assert!(r.app.update_status()["error"].as_str().unwrap().contains("404"));
}

#[test]
fn the_setting_stops_scheduled_checks_but_not_a_manual_one() {
    let r = rig("update-setting");
    r.publish("2.1.0");
    r.app.ports.settings.set(json!({ "checkUpdates": false })).unwrap();
    r.app.update_tick(std::time::SystemTime::now());
    assert!(r.http.requests.lock().unwrap().is_empty(), "with the setting off, nothing leaves the machine");
    r.app.check_for_update(true);
    assert_eq!(r.app.update_status()["available"], "2.1.0");
}

#[test]
fn installing_hands_the_verified_file_to_the_installer_and_leaves() {
    let r = rig("update-install");
    r.publish("2.1.0");
    r.app.check_for_update(false);
    r.app.install_update();
    let started = r.shell.installers.lock().unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].contents, INSTALLER);
    assert_eq!(started[0].install_dir, r.dir.join("Programs"));
    assert!(!started[0].show_main);
    assert_eq!(started[0].path.file_name().unwrap(), "Shotik-Setup-2.1.0.exe");
    let folder = started[0].path.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
    assert!(folder.starts_with("shotik-update-"), "a folder of its own: {folder}");
    assert_eq!(r.quit_count(), 1, "the installer cannot replace a running executable");
}

#[test]
fn an_open_main_window_comes_back_after_the_update() {
    let r = rig("update-main");
    *r.ui.main_open.lock().unwrap() = true;
    r.publish("2.1.0");
    r.app.check_for_update(false);
    r.app.install_update();
    assert!(r.shell.installers.lock().unwrap()[0].show_main);
}

#[test]
fn a_tampered_installer_is_never_started() {
    let r = rig("update-tampered");
    r.publish("2.1.0");
    r.app.check_for_update(false);
    r.http.routes.lock().unwrap().insert(format!("{UPDATES}/Shotik-Setup-2.1.0.exe"), (200, b"the fake installer".to_vec()));
    r.app.install_update();
    assert!(r.shell.installers.lock().unwrap().is_empty());
    assert_eq!(r.quit_count(), 0);
    assert_eq!(r.download_folders(), 0, "not even written to disk");
    let status = r.app.update_status();
    assert!(status["error"].as_str().unwrap().contains("hash"), "{status}");
    assert_eq!(status["installing"], false);
    assert_eq!(status["available"], "2.1.0", "still on offer");
    assert_eq!(r.toasts().last().unwrap().kind, "update-failed");

    r.http.routes.lock().unwrap().insert(format!("{UPDATES}/Shotik-Setup-2.1.0.exe"), (200, INSTALLER.to_vec()));
    r.app.install_update();
    assert_eq!(r.shell.installers.lock().unwrap().len(), 1, "and a clean retry works");
}

#[test]
fn a_failed_download_keeps_the_application_running() {
    let r = rig("update-no-installer");
    r.publish("2.1.0");
    r.app.check_for_update(false);
    r.http.routes.lock().unwrap().remove(&format!("{UPDATES}/Shotik-Setup-2.1.0.exe"));
    r.app.install_update();
    assert_eq!(r.quit_count(), 0);
    assert_eq!(r.app.update_status()["installing"], false);
    assert_eq!(r.toasts().last().unwrap().kind, "update-failed");
}

#[test]
fn an_installer_that_will_not_start_keeps_the_application_running() {
    let r = rig("update-blocked-exe");
    r.publish("2.1.0");
    r.app.check_for_update(false);
    *r.shell.installer_error.lock().unwrap() = Some("blocked by antivirus".into());
    r.app.install_update();
    assert_eq!(r.quit_count(), 0);
    assert_eq!(r.download_folders(), 0, "nothing left behind");
    assert!(r.toasts().last().unwrap().body.contains("blocked by antivirus"));
}

#[test]
fn nothing_is_installed_while_recording() {
    let r = rig("update-recording");
    r.publish("2.1.0");
    r.app.check_for_update(false);
    *r.recorder.recording.lock().unwrap() = true;
    r.app.install_update();
    assert_eq!(r.requests_of("Shotik-Setup"), 0, "not even downloaded");
    assert!(r.shell.installers.lock().unwrap().is_empty());
    assert_eq!(r.quit_count(), 0);
    assert_eq!(r.toasts().last().unwrap().body, r.app.t("updateBusyRecording"));
    assert_eq!(r.app.update_status()["available"], "2.1.0", "still on offer afterwards");
}

#[test]
fn nothing_is_installed_with_the_overlay_open() {
    let r = rig("update-overlay");
    r.publish("2.1.0");
    r.app.check_for_update(false);
    *r.ui.overlay_open.lock().unwrap() = true;
    r.app.install_update();
    assert!(r.shell.installers.lock().unwrap().is_empty());
    assert_eq!(r.quit_count(), 0);
    assert_eq!(r.toasts().last().unwrap().body, r.app.t("updateBusyOverlay"));
}

#[test]
fn a_recording_started_during_the_download_stops_the_install() {
    let r = rig("update-race");
    r.publish("2.1.0");
    r.app.check_for_update(false);
    let recorder = r.recorder.clone();
    *r.http.during.lock().unwrap() = Some(Box::new(move |url| {
        if url.contains("Shotik-Setup") {
            *recorder.recording.lock().unwrap() = true;
        }
    }));
    r.app.install_update();
    assert!(r.shell.installers.lock().unwrap().is_empty());
    assert_eq!(r.quit_count(), 0, "the recording goes on");
    assert_eq!(r.download_folders(), 0);
    assert_eq!(r.app.update_status()["installing"], false);
}

#[test]
fn a_portable_copy_opens_the_release_page() {
    let r = rig_installed("update-portable", true, true, false);
    r.publish("2.1.0");
    r.app.check_for_update(false);
    r.app.install_update();
    assert_eq!(*r.shell.urls.lock().unwrap(), vec![format!("{UPDATES}/tag/v2.1.0")]);
    assert!(r.shell.installers.lock().unwrap().is_empty(), "an installer would add a second copy elsewhere");
    assert_eq!(r.quit_count(), 0);
}

#[test]
fn nothing_to_install_does_nothing() {
    let r = rig("update-nothing");
    r.app.install_update();
    assert!(r.http.requests.lock().unwrap().is_empty());
    assert_eq!(r.quit_count(), 0);
}

#[test]
fn the_installer_is_heard_when_it_restarts_the_application() {
    let r = rig("update-result");
    r.app.announce_update_result(&["x".into(), "--hidden".into()]);
    assert!(r.toasts().is_empty());
    r.app.announce_update_result(&["x".into(), "--updated".into()]);
    assert_eq!(r.toasts()[0].kind, "update-done");
    assert!(r.toasts()[0].title.contains("2.0.0"));
    r.app.announce_update_result(&["x".into(), "--update-failed".into()]);
    assert_eq!(r.toasts()[1].kind, "update-failed");
    assert_eq!(r.toasts()[1].body, r.app.t("updateFailedInstallBody"));
}

#[test]
fn only_finished_updates_are_cleaned_up() {
    let r = rig("update-clean");
    let own = format!("shotik-update-{}-1", std::process::id());
    for folder in ["shotik-update-1-2", &own, "shotik-updates", "other"] {
        std::fs::create_dir_all(r.dir.join(folder).join("inner")).unwrap();
    }
    r.app.remove_old_downloads();
    assert!(!r.dir.join("shotik-update-1-2").exists());
    assert!(r.dir.join(&own).exists(), "this process may be using it");
    assert!(r.dir.join("other").exists());
    assert!(r.dir.join("shotik-updates").exists(), "a prefix match is not enough");
}

#[test]
fn test_overrides_only_work_for_a_test_instance() {
    let base = || Some("http://127.0.0.1:1".to_string());
    let key = || Some("00".repeat(32));
    let real = UpdateConfig::resolve(false, false, base(), key(), None);
    assert_eq!(real.source, shotik_core::update::UpdateSource::github(), "an environment variable must not change whom a user's copy trusts");
    assert!(real.auto_check);
    assert_eq!(real.first_check_after, Duration::from_secs(60));

    let test = UpdateConfig::resolve(true, true, base(), key(), None);
    assert_eq!(test.source.manifest_url(), "http://127.0.0.1:1/latest.json");
    assert_eq!(test.source.public_key, "00".repeat(32));
    assert!(test.auto_check);

    let quiet = UpdateConfig::resolve(true, true, None, None, None);
    assert!(!quiet.auto_check, "a test instance has no business reaching GitHub");
    assert!(!UpdateConfig::resolve(false, true, None, None, None).auto_check);
    assert!(!UpdateConfig::resolve(true, false, None, None, None).auto_check);
}
