//! Orchestration against fake ports: what the user gets from each action, without a window.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use shotik_core::ports::*;
use shotik_core::settings::{defaults, FileSettings};
use shotik_core::history::FileHistory;
use shotik_core::{Display, Error, Frame, Point, PointF, Rect, Result};

use crate::{cli_capture_mode, App, Environment, Ports};

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

struct FakeDisplays;
impl Displays for FakeDisplays {
    fn all(&self) -> Vec<Display> {
        vec![display()]
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

struct FakeCapture;
impl ScreenCapture for FakeCapture {
    fn grab(&self, _display: &Display) -> Result<Frame> {
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

struct NoHttp;
impl Http for NoHttp {
    fn send(&self, _request: HttpRequest<'_>) -> Result<HttpResponse> {
        Err(Error::msg("no network in tests"))
    }
}

struct NoShell;
impl Shell for NoShell {
    fn reveal_in_folder(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
    fn open_path(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
    fn open_url(&self, _url: &str) -> Result<()> {
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
}
impl RecorderControl for FakeRecorder {
    fn start(&self, _target: RecordTarget, options: RecordOptions) -> Result<Option<RecStatus>> {
        let mut recording = self.recording.lock().unwrap();
        if *recording {
            return Ok(None);
        }
        *recording = true;
        self.starts.lock().unwrap().push(options);
        Ok(Some(RecStatus { recording: true, ..RecStatus::default() }))
    }
    fn stop(&self) {
        *self.stops.lock().unwrap() += 1;
        *self.recording.lock().unwrap() = false;
    }
    fn pause(&self) {}
    fn resume(&self) {}
    fn cancel(&self) {}
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
    main_shown: Mutex<u32>,
    overlay_open: Mutex<bool>,
    overlay_answer: Mutex<Option<OverlayOutcome>>,
    overlay_displays: Mutex<Vec<(u64, bool)>>,
    pending_done: Mutex<Option<Once<OverlayOutcome>>>,
    marks: Mutex<Option<MarkSink>>,
}
impl UiPort for FakeUi {
    fn overlay_begin(&self, _mode: OverlayMode, marks: MarkSink, done: Once<OverlayOutcome>) -> Result<()> {
        *self.overlay_open.lock().unwrap() = true;
        *self.marks.lock().unwrap() = Some(marks);
        *self.pending_done.lock().unwrap() = Some(done);
        Ok(())
    }
    fn overlay_add_display(&self, display: Display, _frame: Frame, focused: bool) -> Result<()> {
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
        false
    }
    fn send_to_main(&self, _event: MainEvent) {}
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
    fn close_rec_windows(&self) {}
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
    hotkeys: Arc<FakeHotkeys>,
    tray: Arc<FakeTray>,
    autostart: Arc<FakeAutostart>,
    selection: Arc<FakeSelection>,
    dir: PathBuf,
    _rt: tokio::runtime::Runtime,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rig_with(name: &str, ghost: bool, test_mode: bool) -> Rig {
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
    let hotkeys = Arc::new(FakeHotkeys::default());
    let tray = Arc::new(FakeTray::default());
    let autostart = Arc::new(FakeAutostart::default());
    let selection = Arc::new(FakeSelection::default());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let ports = Ports {
        settings,
        history,
        displays: Arc::new(FakeDisplays),
        capture: Arc::new(FakeCapture),
        windows: Arc::new(NoWindows),
        clipboard: clipboard.clone(),
        ocr: Arc::new(FakeOcr("распознанный текст")),
        http: Arc::new(NoHttp),
        shell: Arc::new(NoShell),
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
        quit: Arc::new(|| {}),
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
        version: "2.0.0-test".into(),
        platform: "win32",
        locale: "ru-RU".into(),
        temp_dir: dir.clone(),
    };
    let app = App::new(ports, env, rt.handle().clone());
    Rig { app, ui, clipboard, recorder, hotkeys, tray, autostart, selection, dir, _rt: rt }
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
