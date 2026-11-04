//! The real overlay off-screen, on `test/fake-screen.png`, driven through the same routes the contract tests
//! use, plus real Slint pointer events and real key messages to its own window. Writes PNGs to a temp
//! directory for comparing with `docs/rust/reference-ui/`, measures latency and memory, checks behaviour.
//!
//! Touches nothing on the desktop: ghost mode, windows at x+20000, no focus, no input to anything else.
//!
//! ```sh
//! cargo run --release -p shotik-ui --example overlay_preview -- [output-dir] [--bench-only | --real-path-offscreen]
//! ```
//!
//! `--real-path-offscreen` runs the non-ghost code path with a display that is itself far off-screen and
//! without the keyboard focus request: motion, cloak and styles as on the real desktop, still out of sight.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shotik_core::ports::{
    AppHost, AudioDevice, HotkeyError, MarkSink, OcrBoxes, OcrLine, OcrWord, Once, OverlayActionKind, OverlayMode, OverlayOutcome,
    RecStatus, Theme, UiPort,
};
use shotik_core::{Display, Frame, Rect, Result};
use shotik_ui::testing::StubHost;
use shotik_ui::{overlay, Ui, UiEnv};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::LogicalPosition;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetGuiResources, GR_GDIOBJECTS, GR_USEROBJECTS};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsWindowVisible, PostMessageW, SendMessageW, SetWindowPos,
    GWL_EXSTYLE, SWP_NOACTIVATE, SWP_NOZORDER, WM_CLOSE, WM_KEYDOWN, WM_KEYUP, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST,
};

/// `StubHost` with word boxes for Live Text and a record of what reached the application.
#[derive(Default)]
struct PreviewHost {
    inner: StubHost,
    colors: Mutex<Vec<String>>,
    copied: Mutex<Vec<String>>,
    ocr_crops: AtomicU32,
    ocr_words: Mutex<OcrBoxes>,
}

macro_rules! delegate {
    ($($name:ident ( $($arg:ident : $ty:ty),* ) $(-> $ret:ty)?;)*) => {
        $(fn $name(&self, $($arg: $ty),*) $(-> $ret)? { self.inner.$name($($arg),*) })*
    };
}

impl AppHost for PreviewHost {
    delegate! {
        app_state() -> Value;
        settings_set(patch: Value) -> Result<Value>;
        settings_choose_dir() -> Result<Option<PathBuf>>;
        i18n() -> Value;
        theme() -> Theme;
        history_list(limit: usize) -> Vec<Value>;
        history_remove(id: &str) -> Result<()>;
        history_clear(delete_files: bool) -> Result<()>;
        history_copy(id: &str) -> Result<bool>;
        history_copy_path(id: &str) -> Result<bool>;
        history_pin(id: &str) -> Result<bool>;
        history_reveal(id: &str) -> Result<()>;
        history_open(id: &str) -> Result<()>;
        history_ocr(id: &str, done: Once<Result<Option<String>>>);
        history_export_gif(id: &str, done: Once<Option<PathBuf>>);
        hotkeys_suspend();
        hotkeys_resume() -> Vec<HotkeyError>;
        capture_region();
        capture_text();
        capture_full();
        capture_repeat();
        capture_record();
        capture_record_audio();
        rec_stop();
        rec_pause_toggle();
        rec_cancel();
        rec_status() -> RecStatus;
        microphones() -> Result<Vec<AudioDevice>>;
        open_external(url: &str) -> Result<()>;
        open_save_dir() -> Result<()>;
        reveal_file(path: &Path) -> Result<()>;
        translate_text(text: &str, target: Option<&str>, done: Once<Result<Value>>);
        overlay_translate(text: &str, done: Once<Result<Value>>);
        bubble_activated();
        pin_copy(file: &Path) -> Result<()>;
        pin_save_as(file: &Path) -> Result<()>;
    }

    fn copy_text(&self, text: &str) -> Result<()> {
        self.copied.lock().unwrap().push(text.to_string());
        Ok(())
    }

    fn overlay_ocr_boxes(&self, png: Vec<u8>, done: Once<OcrBoxes>) {
        assert!(Frame::from_png(&png).is_ok(), "the crop must be a PNG");
        self.ocr_crops.fetch_add(1, Ordering::SeqCst);
        done(self.ocr_words.lock().unwrap().clone());
    }

    fn color_copied(&self, hex: &str) {
        self.colors.lock().unwrap().push(hex.to_string());
    }
}

fn private_mb() -> f64 {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    // SAFETY: the out pointer and its size match, and the pseudo handle needs no closing.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::from_mut(&mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
        .expect("process memory info");
    }
    counters.PrivateUsage as f64 / 1_048_576.0
}

/// GDI objects and USER objects of this process: every window a session makes has to be gone after it.
fn gui_objects() -> (u32, u32) {
    // SAFETY: plain queries with the pseudo handle.
    unsafe { (GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS), GetGuiResources(GetCurrentProcess(), GR_USEROBJECTS)) }
}

fn median_worst(samples: &mut [f64]) -> (f64, f64) {
    samples.sort_by(f64::total_cmp);
    (samples[samples.len() / 2], *samples.last().unwrap_or(&0.0))
}

struct Marks {
    log: Arc<Mutex<Vec<(&'static str, Instant)>>>,
    sink: MarkSink,
}

impl Marks {
    fn new() -> Self {
        let log: Arc<Mutex<Vec<(&'static str, Instant)>>> = Arc::default();
        let copy = log.clone();
        Self { log, sink: Arc::new(move |label| copy.lock().unwrap().push((label, Instant::now()))) }
    }

    fn at(&self, label: &str) -> Option<Instant> {
        self.log.lock().unwrap().iter().find(|(name, _)| *name == label).map(|(_, at)| *at)
    }

    fn wait(&self, label: &str) -> Instant {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(at) = self.at(label) {
                return at;
            }
            assert!(Instant::now() < deadline, "no {label} mark in 15 s");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

struct Checks {
    failed: Vec<String>,
    passed: u32,
}

impl Checks {
    fn ok(&mut self, condition: bool, what: impl Into<String>) {
        if condition {
            self.passed += 1;
        } else {
            let what = what.into();
            println!("FAILED: {what}");
            self.failed.push(what);
        }
    }
}

fn primary() -> Display {
    let bounds = Rect::new(0, 0, 2560, 1440);
    Display { id: 1, bounds, work_area: bounds, scale_factor: 1.5, physical_bounds: Rect::new(0, 0, 3840, 2160), primary: true }
}

fn second() -> Display {
    let bounds = Rect::new(2560, 462, 1920, 1080);
    Display { id: 2, bounds, work_area: bounds, scale_factor: 1.0, physical_bounds: Rect::new(3840, 694, 1920, 1080), primary: false }
}

fn third() -> Display {
    let bounds = Rect::new(-2560, 279, 2560, 1440);
    Display { id: 3, bounds, work_area: bounds, scale_factor: 1.0, physical_bounds: Rect::new(-2560, 419, 2560, 1440), primary: false }
}

/// Word boxes of the fixture's text, image pixels on the 3840 wide frame, relative to the crop of the
/// reference selection. Measured from `overlay-live-text.png` (which ran real OCR) at 1.92 per display pixel.
fn fixture_words() -> OcrBoxes {
    type WordBox = (&'static str, f64, f64, f64, f64);
    let lines: [&[WordBox]; 4] = [
        &[
            ("Test", 270.0, 162.0, 40.0, 18.0),
            ("Window", 315.0, 162.0, 78.0, 18.0),
            ("-", 399.0, 165.0, 8.0, 14.0),
            ("Shotik", 413.0, 162.0, 62.0, 18.0),
            ("QA", 478.0, 162.0, 29.0, 18.0),
        ],
        &[("Hello", 297.0, 257.0, 58.0, 24.0), ("Shotik", 363.0, 257.0, 71.0, 24.0), ("123", 442.0, 257.0, 39.0, 24.0)],
        &[
            ("Привет", 297.0, 310.0, 85.0, 25.0),
            ("мир", 390.0, 312.0, 47.0, 23.0),
            ("тестовая", 444.0, 312.0, 100.0, 23.0),
            ("строк", 553.0, 312.0, 65.0, 23.0),
        ],
        &[
            ("const", 297.0, 381.0, 60.0, 18.0),
            ("answer", 371.0, 381.0, 75.0, 18.0),
            ("=", 459.0, 383.0, 12.0, 14.0),
            ("42;", 484.0, 381.0, 35.0, 18.0),
            ("//", 535.0, 381.0, 23.0, 18.0),
            ("code", 573.0, 381.0, 49.0, 18.0),
            ("sample", 636.0, 381.0, 74.0, 18.0),
        ],
    ];
    let k = 1.92;
    let (crop_x, crop_y) = (480.0, 280.0);
    OcrBoxes {
        lines: lines
            .iter()
            .map(|line| OcrLine {
                text: line.iter().map(|w| w.0).collect::<Vec<_>>().join(" "),
                words: line
                    .iter()
                    .map(|&(text, x, y, w, h)| OcrWord { text: text.into(), x: x * k - crop_x, y: y * k - crop_y, w: w * k, h: h * k })
                    .collect(),
            })
            .collect(),
    }
}

fn pointer_move(index: usize, x: f32, y: f32) {
    overlay::dispatch_event_for_test(index, WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) }).expect("pointer move");
}

fn pointer_button(index: usize, x: f32, y: f32, pressed: bool) {
    let position = LogicalPosition::new(x, y);
    let button = PointerEventButton::Left;
    let event = if pressed { WindowEvent::PointerPressed { position, button } } else { WindowEvent::PointerReleased { position, button } };
    overlay::dispatch_event_for_test(index, event).expect("pointer button");
}

fn click(index: usize, x: f32, y: f32) {
    pointer_move(index, x, y);
    pointer_button(index, x, y, true);
    pointer_button(index, x, y, false);
}

fn type_text(index: usize, text: &str) {
    for ch in text.chars() {
        let s: slint::SharedString = ch.to_string().into();
        overlay::dispatch_event_for_test(index, WindowEvent::KeyPressed { text: s.clone() }).expect("key press");
        overlay::dispatch_event_for_test(index, WindowEvent::KeyReleased { text: s }).expect("key release");
    }
}

/// A key message to the overlay's own window, carrying only what a Russian layout would: the scan code.
fn send_key(index: usize, virtual_key: u16, scan: u32) {
    let hwnd = overlay::hwnd_for_test(index).expect("hwnd");
    let lparam = 1 | ((scan as isize) << 16);
    // SAFETY: the window belongs to this process; the message carries no pointers.
    unsafe {
        SendMessageW(HWND(hwnd as *mut _), WM_KEYDOWN, Some(WPARAM(virtual_key as usize)), Some(LPARAM(lparam)));
        SendMessageW(HWND(hwnd as *mut _), WM_KEYUP, Some(WPARAM(virtual_key as usize)), Some(LPARAM(lparam | (1 << 30) | (1 << 31))));
    }
}

fn state(ui: &Ui, index: usize) -> Value {
    ui.ui_state("overlay", index).expect("ui state")
}

fn action(ui: &Ui, index: usize, value: Value) -> Value {
    ui.ui_action("overlay", index, value).expect("ui action")
}

fn wait_closed(ui: &Ui) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while ui.overlay_is_open() {
        assert!(Instant::now() < deadline, "the session did not end");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Lets the UI thread finish what it has queued, so a key message's deferred action has run.
fn settle_ui(ui: &Ui) {
    let _ = ui.ui_state("overlay", 0);
}

/// Waits until the window has painted once more.
fn wait_paint(since: u64) -> (u64, f64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (count, last_ms) = overlay::paint_stats_for_test();
        if count > since {
            return (count, last_ms);
        }
        assert!(Instant::now() < deadline, "no paint in 5 s");
        std::thread::yield_now();
    }
}

fn open(ui: &Ui, mode: OverlayMode, frame: &Frame) -> (Marks, mpsc::Receiver<OverlayOutcome>) {
    let marks = Marks::new();
    let (tx, rx) = mpsc::channel();
    ui.overlay_begin(
        mode,
        marks.sink.clone(),
        Box::new(move |outcome| {
            tx.send(outcome).expect("the preview waits for the outcome");
        }),
    )
    .expect("begin");
    ui.overlay_add_display(primary(), frame.clone(), true).expect("add display");
    marks.wait("show");
    (marks, rx)
}

fn cancel_and_wait(ui: &Ui, rx: mpsc::Receiver<OverlayOutcome>, checks: &mut Checks) {
    ui.overlay_cancel();
    wait_closed(ui);
    checks.ok(rx.recv_timeout(Duration::from_secs(5)) == Ok(OverlayOutcome::Cancelled), "cancel fires done with Cancelled");
    checks.ok(rx.recv_timeout(Duration::from_millis(50)).is_err(), "done fires exactly once");
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test/fake-screen.png")
}

fn reference_shots(ui: &Ui, host: &PreviewHost, frame: &Frame, out: &Path, checks: &mut Checks) {
    *host.ocr_words.lock().unwrap() = fixture_words();
    let (marks, rx) = open(ui, OverlayMode::Normal, frame);
    let timing = ["winCreate", "painted", "show"].map(|label| marks.at(label));
    checks.ok(timing.iter().all(Option::is_some), "winCreate, painted and show are reported");
    checks.ok(timing[1] <= timing[2], "painted comes before show");
    checks.ok(ui.overlay_is_open(), "is_open during a session");
    checks.ok(ui.overlay_bounds() == [Rect::new(20000, 0, 2560, 1440)], "bounds in global DIP with the ghost offset");

    let snap = state(ui, 0);
    checks.ok(snap["ready"] == true, "ready after show");
    checks.ok(snap["mode"] == "idle" && snap["hintsVisible"] == true, "idle with hints");
    checks.ok(snap["viewport"]["width"] == 2560.0 && snap["image"]["width"] == 3840.0, "viewport and image sizes");
    let (w, h) = ui.capture_page("overlay", 0, &out.join("overlay-idle.png")).expect("capture idle");
    checks.ok((w, h) == (3840, 2160), format!("capture is physical size, got {w}x{h}"));

    let (sx, sy) = (2560.0 / 1920.0, 1440.0 / 1080.0);
    let rect = json!({ "x": 240.0 * sx, "y": 140.0 * sy, "w": 1100.0 * sx, "h": 640.0 * sy });
    let snap = action(ui, 0, json!({ "type": "select", "rect": rect }));
    checks.ok(snap["toolbarVisible"] == true && snap["mode"] == "selected", "select shows the toolbar");
    ui.capture_page("overlay", 0, &out.join("overlay-selection.png")).expect("capture selection");

    let snap = action(
        ui,
        0,
        json!({ "type": "annotate", "annotations": [
            {"type":"rect","x1":270.0*sx,"y1":225.0*sy,"x2":940.0*sx,"y2":330.0*sy,"color":"#FF4D5E","stroke":4},
            {"type":"arrow","x1":1100.0*sx,"y1":460.0*sy,"x2":940.0*sx,"y2":305.0*sy,"color":"#3E9EFF","stroke":4},
            {"type":"counter","x":270.0*sx,"y":210.0*sy,"n":1,"color":"#FF9F2E"},
            {"type":"text","x":340.0*sx,"y":450.0*sy,"lines":["Review this area"],"fontPx":23,"color":"#FFFFFF"},
        ] }),
    );
    checks.ok(snap["annotationCount"] == 4, "annotate replaces the annotations");
    ui.capture_page("overlay", 0, &out.join("overlay-annotations.png")).expect("capture annotations");

    action(ui, 0, json!({ "type": "annotate", "annotations": [] }));
    let snap = action(ui, 0, json!({ "type": "live-text" }));
    checks.ok(snap["textMode"] == true, "Live Text entered");
    let deadline = Instant::now() + Duration::from_secs(5);
    while state(ui, 0)["wordCount"] == 0 {
        assert!(Instant::now() < deadline, "no word boxes");
        std::thread::sleep(Duration::from_millis(2));
    }
    checks.ok(host.ocr_crops.load(Ordering::SeqCst) == 1, "one crop went to recognition");
    action(ui, 0, json!({ "type": "text-select-all" }));
    let snap = state(ui, 0);
    checks.ok(snap["selectedWords"] == snap["wordCount"], "Select All selects every word");
    // Same condition as the reference: the flash has gone away by itself.
    std::thread::sleep(Duration::from_millis(1500));
    checks.ok(state(ui, 0)["flash"].is_null(), "the flash message expires");
    ui.capture_page("overlay", 0, &out.join("overlay-live-text.png")).expect("capture Live Text");

    let snap = action(ui, 0, json!({ "type": "copy-color", "hex": "#3E9EFF" }));
    checks.ok(snap["ready"] == true, "copy-color keeps the session");
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.colors.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "color_copied was not called");
        std::thread::sleep(Duration::from_millis(2));
    }
    checks.ok(host.colors.lock().unwrap().as_slice() == ["#3E9EFF"], "copy-color reports the colour");

    native_window_checks(0, (20000, 0, 3840, 2160), true, checks);

    checks.ok(ui.ui_state("overlay", 5).is_err(), "an unknown window index is an error");
    checks.ok(ui.ui_action("overlay", 0, json!({ "type": "nonsense" })).is_err(), "an unknown action is an error");
    cancel_and_wait(ui, rx, checks);
    checks.ok(ui.overlay_bounds().is_empty(), "no bounds after the session");
    checks.ok(ui.ui_state("overlay", 0).is_err(), "no state after the session");
}

/// The window is exactly where it should be, and nothing can change that.
fn native_window_checks(index: usize, bounds: (i32, i32, i32, i32), ghost: bool, checks: &mut Checks) {
    let hwnd = HWND(overlay::hwnd_for_test(index).expect("hwnd") as *mut _);
    let read = || {
        let (mut outer, mut inner) = (Default::default(), Default::default());
        // SAFETY: the window belongs to this process and the out pointers are valid.
        unsafe {
            GetWindowRect(hwnd, &mut outer).expect("window rect");
            GetClientRect(hwnd, &mut inner).expect("client rect");
        }
        (outer, inner)
    };
    let check = |checks: &mut Checks, what: &str| {
        let (outer, inner) = read();
        let at_exact_bounds = (outer.left, outer.top, outer.right - outer.left, outer.bottom - outer.top) == bounds;
        checks.ok(at_exact_bounds, format!("{what}: outer {outer:?}"));
        checks.ok((inner.right, inner.bottom) == (bounds.2, bounds.3), format!("{what}: client {inner:?}"));
    };
    check(checks, "placed");
    // SAFETY: moving this process's own off-screen window; the subclass is expected to refuse.
    unsafe {
        SetWindowPos(hwnd, None, 25000, 100, 640, 480, SWP_NOZORDER | SWP_NOACTIVATE).expect("set window pos");
    }
    check(checks, "after an attempt to move and resize");
    // SAFETY: plain queries.
    let (ex, foreground, visible) =
        unsafe { (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32, GetForegroundWindow(), IsWindowVisible(hwnd).as_bool()) };
    checks.ok(visible, "the window is shown");
    checks.ok(ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0, "out of the taskbar");
    checks.ok(ex & WS_EX_TOPMOST.0 != 0, "topmost");
    checks.ok((ex & WS_EX_NOACTIVATE.0 != 0) == ghost, "only ghost windows refuse activation");
    checks.ok(foreground != hwnd, "the overlay did not take the foreground");
}

fn interaction_shots(ui: &Ui, host: &PreviewHost, frame: &Frame, out: &Path, checks: &mut Checks) {
    let (_marks, rx) = open(ui, OverlayMode::Normal, frame);
    ui.overlay_set_window_rects(vec![Rect::new(480, 280, 2200, 1280), Rect::new(2940, 340, 360, 120)]);

    // Hover-snap with real pointer events.
    pointer_move(0, 2000.0, 260.0);
    let snap = state(ui, 0);
    checks.ok(snap["hoverRect"]["x"] == 1960.0, format!("hover finds the small window first: {}", snap["hoverRect"]));
    checks.ok(snap["magnifierVisible"] == true, "the magnifier follows the pointer");
    ui.capture_page("overlay", 0, &out.join("overlay-hover.png")).expect("capture hover");

    // Key messages to the overlay's own window, by scan code only. `Alt` is named, `F` is by row.
    send_key(0, 0x12, 0x38);
    settle_ui(ui);
    // Pressing alone keeps the pointer move's modifiers until the key comes up; the message pair released it.
    pointer_move(0, 2100.0, 260.0);
    checks.ok(state(ui, 0)["hoverRect"]["x"] == 1960.0, "Alt released: snapping is back");

    // A click on the hovered window.
    click(0, 2000.0, 260.0);
    let snap = state(ui, 0);
    checks.ok(snap["mode"] == "selected" && snap["selection"]["w"] == 360.0, format!("click snaps to the window: {}", snap["selection"]));

    // Press Esc-free: a new selection by dragging outside.
    let (x0, y0, x1, y1) = (320.0, 186.5, 1786.5, 1040.0);
    pointer_move(0, x0, y0);
    pointer_button(0, x0, y0, true);
    pointer_move(0, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let snap = state(ui, 0);
    checks.ok(snap["mode"] == "selecting" && snap["magnifierVisible"] == true && snap["toolbarVisible"] == false, "dragging out a selection");
    pointer_move(0, x1, y1);
    ui.capture_page("overlay", 0, &out.join("overlay-selecting.png")).expect("capture selecting");
    pointer_button(0, x1, y1, false);
    let snap = state(ui, 0);
    checks.ok(snap["mode"] == "selected" && snap["toolbarVisible"] == true, "selected after the drag");
    checks.ok(snap["dims"] == "2200 × 1280", format!("size label: {}", snap["dims"]));

    // Tool buttons through real pointer events. The toolbar is centred under the selection.
    let toolbar_x = (x0 + x1) / 2.0 - 360.0;
    let toolbar_y = y1 + 10.0;
    let button_x = |i: f32| toolbar_x + 6.0 + i * 36.0 + 17.0;
    pointer_move(0, button_x(1.0), toolbar_y + 23.0);
    std::thread::sleep(Duration::from_millis(600));
    ui.capture_page("overlay", 0, &out.join("overlay-tooltip.png")).expect("capture tooltip");
    click(0, button_x(1.0), toolbar_y + 23.0);
    let snap = state(ui, 0);
    checks.ok(snap["tool"] == "pen", format!("clicking the pen button: {}", snap["tool"]));
    checks.ok(snap["optionsVisible"] == true, "the options bar opens for a drawing tool");
    checks.ok(snap["mode"] == "selected", "the toolbar click did not reach the canvas");

    // Colour: third dot of the options bar, below the toolbar. Then a pen stroke.
    let options_x = toolbar_x;
    let options_y = toolbar_y + 46.0 + 8.0;
    click(0, options_x + 11.0 + 2.0 * 27.0 + 10.0, options_y + 22.0);
    let snap = state(ui, 0);
    checks.ok(snap["color"] == "#FFE03A", format!("clicking a colour: {}", snap["color"]));
    click(0, options_x + 11.0 + 237.0 + 6.0 + 11.0 + 6.0 + 2.0 * 31.0 + 14.0, options_y + 22.0);
    checks.ok(state(ui, 0)["size"] == "L", "clicking a size");
    ui.capture_page("overlay", 0, &out.join("overlay-options.png")).expect("capture options");

    pointer_move(0, 600.0, 500.0);
    pointer_button(0, 600.0, 500.0, true);
    for i in 1..30 {
        let t = i as f32;
        pointer_move(0, 600.0 + t * 12.0, 500.0 + (t * 0.5).sin() * 60.0);
    }
    pointer_button(0, 950.0, 500.0, false);
    checks.ok(state(ui, 0)["annotationCount"] == 1, "a pen stroke became an annotation");

    // Each tool once, by the real key messages for its row: digits, scan 0x02 + n.
    for (scan, name, from, to) in [
        (0x04_u32, "arrow", (1100.0, 500.0), (1400.0, 620.0)),
        (0x05, "line", (1100.0, 700.0), (1400.0, 760.0)),
        (0x06, "rect", (500.0, 650.0), (800.0, 800.0)),
        (0x07, "ellipse", (850.0, 650.0), (1050.0, 800.0)),
        (0x08, "highlight", (500.0, 850.0), (900.0, 860.0)),
        (0x09, "blur", (950.0, 830.0), (1300.0, 960.0)),
    ] {
        send_key(0, 0, scan);
        settle_ui(ui);
        let snap = state(ui, 0);
        checks.ok(snap["tool"] == name, format!("scan code {scan:#x} picks {name}, got {}", snap["tool"]));
        pointer_move(0, from.0, from.1);
        pointer_button(0, from.0, from.1, true);
        pointer_move(0, (from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0);
        pointer_move(0, to.0, to.1);
        pointer_button(0, to.0, to.1, false);
    }
    checks.ok(state(ui, 0)["annotationCount"] == 7, format!("every tool drew: {}", state(ui, 0)["annotationCount"]));

    // Counter: scan 0x0B is the 0 key. Twice, and the second number follows.
    send_key(0, 0, 0x0B);
    settle_ui(ui);
    click(0, 1500.0, 500.0);
    click(0, 1560.0, 500.0);
    checks.ok(state(ui, 0)["counterNext"] == 3, "two counters numbered");

    // Text tool with Cyrillic typed into the Slint editor; the digit row goes to the editor, not the tools.
    send_key(0, 0, 0x0A);
    settle_ui(ui);
    checks.ok(state(ui, 0)["tool"] == "text", "9 picks the text tool");
    click(0, 1450.0, 620.0);
    checks.ok(state(ui, 0)["editorOpen"] == true, "the editor opens where clicked");
    type_text(0, "Привет 1");
    send_key(0, 0, 0x02);
    settle_ui(ui);
    checks.ok(state(ui, 0)["tool"] == "text", "keys are the editor's while it is open");
    ui.capture_page("overlay", 0, &out.join("overlay-editor.png")).expect("capture editor");
    overlay::dispatch_event_for_test(0, WindowEvent::KeyPressed { text: Key::Return.into() }).expect("enter");
    overlay::dispatch_event_for_test(0, WindowEvent::KeyReleased { text: Key::Return.into() }).expect("enter");
    let snap = state(ui, 0);
    checks.ok(snap["editorOpen"] == false && snap["annotationCount"] == 10, format!("Enter commits the text: {}", snap["annotationCount"]));
    ui.capture_page("overlay", 0, &out.join("overlay-all-tools.png")).expect("capture all tools");

    // Undo and redo.
    let snap = action(ui, 0, json!({ "type": "undo" }));
    checks.ok(snap["annotationCount"] == 9 && snap["redoCount"] == 1, "undo");
    let snap = action(ui, 0, json!({ "type": "redo" }));
    checks.ok(snap["annotationCount"] == 10 && snap["redoCount"] == 0, "redo");

    // Moving the selection away and back keeps annotations right under and outside the veil.
    action(ui, 0, json!({ "type": "set-tool", "tool": "move" }));
    pointer_move(0, 1000.0, 500.0);
    pointer_button(0, 1000.0, 500.0, true);
    pointer_move(0, 1200.0, 600.0);
    pointer_button(0, 1200.0, 600.0, false);
    ui.capture_page("overlay", 0, &out.join("overlay-moved.png")).expect("capture moved");
    pointer_move(0, 1200.0, 600.0);
    pointer_button(0, 1200.0, 600.0, true);
    pointer_move(0, 1000.0, 500.0);
    pointer_button(0, 1000.0, 500.0, false);

    // Enter by its real message: the output image, with everything burned in.
    send_key(0, 0x0D, 0x1C);
    let outcome = rx.recv_timeout(Duration::from_secs(15)).expect("an outcome");
    wait_closed(ui);
    match outcome {
        OverlayOutcome::Done(done) => {
            checks.ok(done.action == OverlayActionKind::Copy, "Enter copies");
            checks.ok(done.display_id == Some(1), "the display is reported");
            checks.ok(done.rect_phys == Some(Rect::new(480, 280, 2200, 1280)), format!("rect_phys {:?}", done.rect_phys));
            checks.ok(done.rect_dip == Some(Rect::new(320, 187, 1467, 853)), format!("rect_dip {:?}", done.rect_dip));
            let png = done.png.expect("the image");
            std::fs::write(out.join("overlay-output.png"), &png).expect("write output");
            let image = Frame::from_png(&png).expect("decode");
            checks.ok((image.width(), image.height()) == (2200, 1280), format!("output size {}x{}", image.width(), image.height()));
        }
        other => checks.ok(false, format!("expected an action, got {other:?}")),
    }
    let _ = host;
}

fn modes(ui: &Ui, host: &PreviewHost, frame: &Frame, out: &Path, checks: &mut Checks) {
    // Claude mode: badge.
    let (_m, rx) = open(ui, OverlayMode::Claude, frame);
    checks.ok(state(ui, 0)["badgeVisible"] == true, "Claude mode shows its badge");
    ui.capture_page("overlay", 0, &out.join("overlay-claude.png")).expect("capture Claude");
    action(ui, 0, json!({ "type": "select", "rect": { "x": 100, "y": 100, "w": 300, "h": 200 } }));
    action(ui, 0, json!({ "type": "button", "button": "claude" }));
    match rx.recv_timeout(Duration::from_secs(15)).expect("an outcome") {
        OverlayOutcome::Done(done) => {
            checks.ok(done.action == OverlayActionKind::Claude && done.png.is_some(), "the Claude action carries the image");
        }
        other => checks.ok(false, format!("{other:?}")),
    }
    wait_closed(ui);

    // Record mode: select then finish at once, no image.
    let (_m, rx) = open(ui, OverlayMode::Record, frame);
    pointer_move(0, 100.0, 100.0);
    pointer_button(0, 100.0, 100.0, true);
    pointer_move(0, 400.0, 300.0);
    pointer_button(0, 400.0, 300.0, false);
    match rx.recv_timeout(Duration::from_secs(15)).expect("an outcome") {
        OverlayOutcome::Done(done) => {
            checks.ok(done.action == OverlayActionKind::Record && done.png.is_none(), "record mode finishes on mouse-up without an image");
            checks.ok(done.rect_phys == Some(Rect::new(150, 150, 450, 300)), format!("record rect {:?}", done.rect_phys));
            checks.ok(done.rect_dip == Some(Rect::new(100, 100, 300, 200)), format!("record rect dip {:?}", done.rect_dip));
        }
        other => checks.ok(false, format!("{other:?}")),
    }
    wait_closed(ui);

    // Text mode: mouse-up goes straight into Live Text; Enter finishes with the text. Translation card too.
    *host.ocr_words.lock().unwrap() = fixture_words();
    let (_m, rx) = open(ui, OverlayMode::Text, frame);
    pointer_move(0, 320.0, 186.5);
    pointer_button(0, 320.0, 186.5, true);
    pointer_move(0, 1786.5, 1040.0);
    pointer_button(0, 1786.5, 1040.0, false);
    let deadline = Instant::now() + Duration::from_secs(5);
    while state(ui, 0)["wordCount"] == 0 {
        assert!(Instant::now() < deadline, "no word boxes in text mode");
        std::thread::sleep(Duration::from_millis(2));
    }
    // Rubber band over the first line, with real pointer events.
    pointer_move(0, 340.0, 200.0);
    pointer_button(0, 340.0, 200.0, true);
    pointer_move(0, 700.0, 240.0);
    ui.capture_page("overlay", 0, &out.join("overlay-rubber.png")).expect("capture rubber band");
    pointer_button(0, 700.0, 240.0, false);
    let snap = state(ui, 0);
    checks.ok(snap["selectedText"] == "Test Window - Shotik QA", format!("rubber band: {}", snap["selectedText"]));

    // R: translation card. StubHost answers "[stub] ...".
    let snap = action(ui, 0, json!({ "type": "key", "code": "KeyR" }));
    checks.ok(!snap["translate"].is_null(), "R opens the card");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snap = state(ui, 0);
        if snap["translate"]["loading"] == false {
            checks.ok(snap["translate"]["body"].as_str().is_some_and(|b| b.starts_with("[stub] Test")), format!("{}", snap["translate"]));
            break;
        }
        assert!(Instant::now() < deadline, "no translation");
        std::thread::sleep(Duration::from_millis(2));
    }
    ui.capture_page("overlay", 0, &out.join("overlay-translate.png")).expect("capture translation");
    let snap = action(ui, 0, json!({ "type": "translate-copy" }));
    checks.ok(!snap["flash"].is_null(), "copying the translation flashes");
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.copied.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "copy_text was not called");
        std::thread::sleep(Duration::from_millis(2));
    }
    action(ui, 0, json!({ "type": "key", "code": "Escape" }));
    action(ui, 0, json!({ "type": "key", "code": "Enter" }));
    match rx.recv_timeout(Duration::from_secs(15)).expect("an outcome") {
        OverlayOutcome::Done(done) => {
            checks.ok(done.action == OverlayActionKind::Copytext, "Enter in Live Text is copytext");
            checks.ok(done.text.as_deref() == Some("Test Window - Shotik QA"), format!("{:?}", done.text));
        }
        other => checks.ok(false, format!("{other:?}")),
    }
    wait_closed(ui);

    // C copies the colour under the cursor and ends the session.
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    pointer_move(0, 100.0, 100.0);
    host.colors.lock().unwrap().clear();
    send_key(0, 0, 0x2E);
    checks.ok(rx.recv_timeout(Duration::from_secs(15)) == Ok(OverlayOutcome::Cancelled), "C cancels after copying");
    wait_closed(ui);
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.colors.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the picked colour never arrived");
        std::thread::sleep(Duration::from_millis(2));
    }
    let picked = host.colors.lock().unwrap()[0].clone();
    checks.ok(picked.len() == 7 && picked.starts_with('#'), format!("picked {picked}"));

    // Shift held: snapping off, and a click takes the whole screen, through Slint's own modifiers.
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    ui.overlay_set_window_rects(vec![Rect::new(480, 280, 2200, 1280)]);
    pointer_move(0, 700.0, 500.0);
    checks.ok(!state(ui, 0)["hoverRect"].is_null(), "hovering a window");
    send_key(0, 0x10, 0x2A);
    settle_ui(ui);
    overlay::dispatch_event_for_test(0, WindowEvent::KeyPressed { text: Key::Shift.into() }).expect("shift");
    pointer_move(0, 701.0, 500.0);
    checks.ok(state(ui, 0)["hoverRect"].is_null(), "Shift turns snapping off");
    pointer_button(0, 701.0, 500.0, true);
    pointer_button(0, 701.0, 500.0, false);
    overlay::dispatch_event_for_test(0, WindowEvent::KeyReleased { text: Key::Shift.into() }).expect("shift");
    let snap = state(ui, 0);
    checks.ok(snap["selection"]["w"] == 3840.0, format!("Shift+click takes the whole screen: {}", snap["selection"]));

    // The editor: Esc drops it, Shift+Enter makes a line, Enter commits.
    action(ui, 0, json!({ "type": "set-tool", "tool": "text" }));
    click(0, 400.0, 400.0);
    type_text(0, "gone");
    overlay::dispatch_event_for_test(0, WindowEvent::KeyPressed { text: Key::Escape.into() }).expect("esc");
    overlay::dispatch_event_for_test(0, WindowEvent::KeyReleased { text: Key::Escape.into() }).expect("esc");
    let snap = state(ui, 0);
    checks.ok(snap["editorOpen"] == false && snap["annotationCount"] == 0, "Esc drops the editor");
    checks.ok(snap["mode"] == "selected", "Esc in the editor keeps the session");
    click(0, 400.0, 400.0);
    checks.ok(state(ui, 0)["editorText"] == "", format!("a new editor starts empty: {}", state(ui, 0)["editorText"]));
    type_text(0, "a");
    overlay::dispatch_event_for_test(0, WindowEvent::KeyPressed { text: Key::Shift.into() }).expect("shift");
    overlay::dispatch_event_for_test(0, WindowEvent::KeyPressed { text: Key::Return.into() }).expect("enter");
    overlay::dispatch_event_for_test(0, WindowEvent::KeyReleased { text: Key::Return.into() }).expect("enter");
    overlay::dispatch_event_for_test(0, WindowEvent::KeyReleased { text: Key::Shift.into() }).expect("shift");
    type_text(0, "b");
    checks.ok(state(ui, 0)["editorText"] == "a\nb", format!("Shift+Enter makes a line: {}", state(ui, 0)["editorText"]));
    overlay::dispatch_event_for_test(0, WindowEvent::KeyPressed { text: Key::Return.into() }).expect("enter");
    overlay::dispatch_event_for_test(0, WindowEvent::KeyReleased { text: Key::Return.into() }).expect("enter");
    let snap = state(ui, 0);
    checks.ok(snap["editorOpen"] == false && snap["annotationCount"] == 1, "Enter commits");

    // A double click inside the selection copies.
    action(ui, 0, json!({ "type": "set-tool", "tool": "move" }));
    click(0, 800.0, 600.0);
    click(0, 800.0, 600.0);
    match rx.recv_timeout(Duration::from_secs(15)).expect("an outcome") {
        OverlayOutcome::Done(done) => checks.ok(done.action == OverlayActionKind::Copy, "a double click copies"),
        other => checks.ok(false, format!("{other:?}")),
    }
    wait_closed(ui);
    checks.ok(rx.recv_timeout(Duration::from_millis(50)).is_err(), "done fires exactly once on a double click");

    // Alt+F4 and anything else that closes the window ends the session with nothing.
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    let hwnd = overlay::hwnd_for_test(0).expect("hwnd");
    // SAFETY: this process's own window.
    unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)).expect("post close") };
    checks.ok(rx.recv_timeout(Duration::from_secs(15)) == Ok(OverlayOutcome::Cancelled), "WM_CLOSE cancels");
    wait_closed(ui);

    // Escape through the real message.
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    send_key(0, 0x1B, 0x01);
    checks.ok(rx.recv_timeout(Duration::from_secs(15)) == Ok(OverlayOutcome::Cancelled), "Esc cancels");
    wait_closed(ui);
    checks.ok(rx.recv_timeout(Duration::from_millis(50)).is_err(), "done fires exactly once on Esc");
}

/// Russian strings come from the dictionary and the light theme applies to an open window.
fn language_and_theme(ui: &Ui, host: &PreviewHost, frame: &Frame, out: &Path, checks: &mut Checks) {
    *host.inner.lang.lock().unwrap() = "ru";
    let (_m, rx) = open(ui, OverlayMode::Claude, frame);
    ui.capture_page("overlay", 0, &out.join("overlay-ru.png")).expect("capture Russian");
    action(ui, 0, json!({ "type": "select", "rect": { "x": 320, "y": 186.5, "w": 1466.5, "h": 853.5 } }));
    action(ui, 0, json!({ "type": "set-tool", "tool": "pen" }));
    ui.apply_theme(Theme { dark: false, accent: "#C42B1C".into(), platform: "win32".into() });
    let snap = state(ui, 0);
    checks.ok(snap["ready"] == true, "the theme applies to an open window");
    ui.capture_page("overlay", 0, &out.join("overlay-light.png")).expect("capture light");
    *host.inner.lang.lock().unwrap() = "en";
    cancel_and_wait(ui, rx, checks);
}

/// A second begin while open is refused; a frame after the end is ignored; several displays at once.
fn sessions(ui: &Ui, frame: &Frame, second_frame: &Frame, third_frame: &Frame, checks: &mut Checks) {
    let marks = Marks::new();
    let (tx, rx) = mpsc::channel();
    ui.overlay_begin(OverlayMode::Normal, marks.sink.clone(), Box::new(move |o| tx.send(o).expect("receiver"))).expect("begin");
    checks.ok(
        ui.overlay_begin(OverlayMode::Normal, marks.sink.clone(), Box::new(|_| panic!("a refused session must not fire"))).is_err(),
        "only one session at a time",
    );
    ui.overlay_add_display(primary(), frame.clone(), true).expect("add");
    ui.overlay_add_display(second(), second_frame.clone(), false).expect("add");
    ui.overlay_add_display(third(), third_frame.clone(), false).expect("add");
    marks.wait("show");
    let deadline = Instant::now() + Duration::from_secs(5);
    while ui.overlay_bounds().len() < 3 {
        assert!(Instant::now() < deadline, "not every display opened");
        std::thread::sleep(Duration::from_millis(2));
    }
    // Window order follows who opened first. The routes find a window by its bounds, as the reference
    // generator does.
    let bounds = ui.overlay_bounds();
    let index_of = |rect: Rect| bounds.iter().position(|b| *b == rect);
    checks.ok(index_of(Rect::new(20000, 0, 2560, 1440)).is_some(), format!("primary in {bounds:?}"));
    checks.ok(index_of(Rect::new(17440, 279, 2560, 1440)).is_some(), format!("left display in {bounds:?}"));
    let on_second = index_of(Rect::new(22560, 462, 1920, 1080)).expect("the second display's window");
    checks.ok(state(ui, on_second)["viewport"]["width"] == 1920.0 && state(ui, on_second)["image"]["width"] == 1920.0, "the second display at scale 1");
    ui.overlay_set_window_rects(vec![Rect::new(3900, 700, 400, 300)]);
    pointer_move(on_second, 100.0, 100.0);
    checks.ok(state(ui, on_second)["hoverRect"]["x"] == 60.0, format!("rects convert per display: {}", state(ui, on_second)["hoverRect"]));
    println!("private with three overlays open: {:.1} MB", private_mb());

    // SAFETY: plain query.
    let foreground = unsafe { GetForegroundWindow() };
    for index in 0..3 {
        let hwnd = overlay::hwnd_for_test(index).expect("hwnd");
        checks.ok(foreground.0 as isize != hwnd, format!("window {index} did not take the foreground"));
    }

    ui.overlay_cancel();
    wait_closed(ui);
    checks.ok(rx.recv_timeout(Duration::from_secs(5)) == Ok(OverlayOutcome::Cancelled), "cancel with three windows");

    // Ends before its frame arrives.
    let marks = Marks::new();
    let (tx, rx) = mpsc::channel();
    ui.overlay_begin(OverlayMode::Normal, marks.sink.clone(), Box::new(move |o| tx.send(o).expect("receiver"))).expect("begin");
    ui.overlay_cancel();
    ui.overlay_add_display(primary(), frame.clone(), true).expect("a late frame is not an error");
    wait_closed(ui);
    checks.ok(rx.recv_timeout(Duration::from_secs(5)) == Ok(OverlayOutcome::Cancelled), "cancel before the first frame");
    ui.overlay_add_display(primary(), frame.clone(), true).expect("a frame after the end is not an error");
    checks.ok(!ui.overlay_is_open(), "no session after a stray frame");
}

/// `overlay_is_open` and `overlay_bounds` from another thread while the UI thread is busy.
fn is_open_never_blocks(ui: &Ui, frame: &Frame, checks: &mut Checks) {
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    let mut worst = Duration::ZERO;
    for _ in 0..1000 {
        let t = Instant::now();
        let _ = ui.overlay_is_open();
        let _ = ui.overlay_bounds();
        worst = worst.max(t.elapsed());
    }
    println!("overlay_is_open + overlay_bounds, worst of 1000: {:.1} us", worst.as_secs_f64() * 1e6);
    checks.ok(worst < Duration::from_millis(50), "is_open and bounds do not wait for the UI thread");
    cancel_and_wait(ui, rx, checks);
}

/// An open overlay left alone must not redraw: this window spends most of its life waiting for a person.
fn idle_does_not_paint(ui: &Ui, frame: &Frame, checks: &mut Checks) {
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    std::thread::sleep(Duration::from_millis(300));
    let (before, _) = overlay::paint_stats_for_test();
    std::thread::sleep(Duration::from_secs(1));
    let (after, _) = overlay::paint_stats_for_test();
    checks.ok(after == before, format!("{} paints in a second of nothing", after - before));
    cancel_and_wait(ui, rx, checks);
}

fn bench_show(ui: &Ui, frame: &Frame, checks: &mut Checks) {
    // First in this process, as the resident app sees it after being idle.
    let mut measure = |wait_for_spare: bool| -> (f64, f64, f64) {
        let marks = Marks::new();
        let (tx, rx) = mpsc::channel();
        let begin = Instant::now();
        ui.overlay_begin(OverlayMode::Normal, marks.sink.clone(), Box::new(move |o| tx.send(o).expect("receiver"))).expect("begin");
        if wait_for_spare {
            // A real grab takes tens of milliseconds, and the window is being made meanwhile.
            std::thread::sleep(Duration::from_millis(30));
        }
        let add = Instant::now();
        ui.overlay_add_display(primary(), frame.clone(), true).expect("add");
        let returned = add.elapsed().as_secs_f64() * 1000.0;
        let show = marks.wait("show");
        let painted = marks.wait("painted");
        let result = (
            show.duration_since(add).as_secs_f64() * 1000.0,
            show.duration_since(begin).as_secs_f64() * 1000.0,
            painted.duration_since(add).as_secs_f64() * 1000.0,
        );
        let _ = returned;
        ui.overlay_cancel();
        wait_closed(ui);
        checks.ok(rx.recv_timeout(Duration::from_secs(5)) == Ok(OverlayOutcome::Cancelled), "bench session ended");
        result
    };

    let (cold_show, cold_total, cold_painted) = measure(true);
    println!("cold, window made during the grab: add_display to show {cold_show:.1} ms, painted {cold_painted:.1} ms, begin to show {cold_total:.1} ms");
    let mut show: Vec<f64> = Vec::new();
    let mut painted: Vec<f64> = Vec::new();
    for _ in 0..20 {
        let (s, _, p) = measure(true);
        show.push(s);
        painted.push(p);
    }
    let (show_median, show_worst) = median_worst(&mut show);
    let (painted_median, _) = median_worst(&mut painted);
    println!("warm, n=20: add_display to show median {show_median:.1} ms, worst {show_worst:.1} ms; to painted median {painted_median:.1} ms");

    let mut immediate: Vec<f64> = Vec::new();
    for _ in 0..20 {
        let (_, total, _) = measure(false);
        immediate.push(total);
    }
    let (median, worst) = median_worst(&mut immediate);
    println!("warm, n=20, no time between begin and add_display: begin to show median {median:.1} ms, worst {worst:.1} ms");
}

fn bench_conversion(frame: &Frame) {
    let mut samples: Vec<f64> = (0..10)
        .map(|_| {
            let copy = Frame::new(frame.width(), frame.height(), frame.bgra().to_vec()).expect("frame");
            let t = Instant::now();
            let _ = overlay::prepare_for_test(copy).expect("prepare");
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    let (median, worst) = median_worst(&mut samples);
    println!("BGRA to RGB plus veiled copy, 3840x2160, including allocation: median {median:.1} ms, worst {worst:.1} ms");
}

/// A long stroke with real pointer events, waiting for each paint. Reports the time from the move to the
/// end of its paint, and the paint alone, early in the stroke and at its end.
fn bench_stroke(ui: &Ui, frame: &Frame, out: &Path, checks: &mut Checks) {
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    action(ui, 0, json!({ "type": "select", "rect": { "x": 100, "y": 100, "w": 2300, "h": 1200 } }));
    action(ui, 0, json!({ "type": "set-tool", "tool": "pen" }));
    let point = |i: usize| -> (f32, f32) {
        let t = i as f32;
        (150.0 + t * 1.05, 700.0 + (t * 0.09).sin() * 540.0 * (0.4 + 0.6 * (t * 0.0021).cos().abs()))
    };
    let (x, y) = point(0);
    pointer_move(0, x, y);
    pointer_button(0, x, y, true);
    let (mut paints, _) = overlay::paint_stats_for_test();
    let mut totals = Vec::new();
    let mut paint_times = Vec::new();
    for i in 1..=2000 {
        let (x, y) = point(i);
        let t = Instant::now();
        pointer_move(0, x, y);
        let (count, last_ms) = wait_paint(paints);
        paints = count;
        totals.push(t.elapsed().as_secs_f64() * 1000.0);
        paint_times.push(last_ms);
    }
    let points = 2000;
    let stats = |values: &[f64]| {
        let mut v = values.to_vec();
        median_worst(&mut v)
    };
    let (first_total, _) = stats(&totals[..200]);
    let (last_total, last_worst) = stats(&totals[points - 200..]);
    let (first_paint, _) = stats(&paint_times[..200]);
    let (last_paint, last_paint_worst) = stats(&paint_times[points - 200..]);
    println!("pen stroke, 2000 points across 3400x1800 px: move to painted, first 200 median {first_total:.2} ms; last 200 median {last_total:.2} ms, worst {last_worst:.2} ms");
    println!("  the paint alone: first 200 median {first_paint:.2} ms; last 200 median {last_paint:.2} ms, worst {last_paint_worst:.2} ms");
    let t = Instant::now();
    pointer_button(0, 2250.0, 700.0, false);
    println!("  committing it: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    checks.ok(state(ui, 0)["annotationCount"] == 1, "the long stroke committed");

    // Selection drag in a fresh session: only the hole repaints, so a small selection costs little on 4K.
    cancel_and_wait(ui, rx, checks);
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    for (name, rect, start) in [
        ("600x450", json!({ "x": 300, "y": 300, "w": 400, "h": 300 }), (500.0, 450.0)),
        ("3000x1500", json!({ "x": 100, "y": 100, "w": 2000, "h": 1000 }), (1100.0, 600.0)),
    ] {
        action(ui, 0, json!({ "type": "select", "rect": rect }));
        pointer_move(0, start.0, start.1);
        pointer_button(0, start.0, start.1, true);
        let (mut paints, _) = overlay::paint_stats_for_test();
        let mut moves = Vec::new();
        let mut paint_times = Vec::new();
        for i in 1..=240 {
            let t = Instant::now();
            pointer_move(0, start.0 + i as f32 * 1.5, start.1 + i as f32 * 0.5);
            let (count, last_ms) = wait_paint(paints);
            paints = count;
            paint_times.push(last_ms);
            moves.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        pointer_button(0, start.0 + 360.0, start.1 + 120.0, false);
        let (median, worst) = stats(&moves);
        let (paint_median, paint_worst) = stats(&paint_times);
        println!("moving a {name} px selection on 4K: paint median {paint_median:.2} ms, worst {paint_worst:.2} ms; move to painted median {median:.2} ms, worst {worst:.2} ms");
    }

    // Idle pointer with the magnifier.
    cancel_and_wait(ui, rx, checks);
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    pointer_move(0, 100.0, 100.0);
    let (mut paints, _) = overlay::paint_stats_for_test();
    let mut moves = Vec::new();
    let mut paint_times = Vec::new();
    for i in 1..=240 {
        let t = Instant::now();
        pointer_move(0, 100.0 + i as f32 * 3.0, 100.0 + i as f32);
        let (count, last_ms) = wait_paint(paints);
        paints = count;
        paint_times.push(last_ms);
        moves.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let (median, worst) = stats(&moves);
    let (paint_median, paint_worst) = stats(&paint_times);
    println!("idle pointer with the magnifier on 4K: paint median {paint_median:.2} ms, worst {paint_worst:.2} ms; move to painted median {median:.2} ms, worst {worst:.2} ms");
    let _ = out;
    cancel_and_wait(ui, rx, checks);
}

/// What the first text or counter of a session costs: the font database is opened on demand.
fn bench_first_text(ui: &Ui, frame: &Frame, checks: &mut Checks) {
    let (_m, rx) = open(ui, OverlayMode::Normal, frame);
    action(ui, 0, json!({ "type": "select", "rect": { "x": 100, "y": 100, "w": 800, "h": 600 } }));
    action(ui, 0, json!({ "type": "set-tool", "tool": "counter" }));
    pointer_move(0, 300.0, 300.0);
    let t = Instant::now();
    pointer_button(0, 300.0, 300.0, true);
    println!("first counter of a session, font database included: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    pointer_button(0, 300.0, 300.0, false);
    let t = Instant::now();
    pointer_move(0, 340.0, 300.0);
    pointer_button(0, 340.0, 300.0, true);
    println!("second counter: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    pointer_button(0, 340.0, 300.0, false);
    cancel_and_wait(ui, rx, checks);
}

fn run(ui: &Ui, host: &PreviewHost, out: &Path, bench_only: bool) -> Vec<String> {
    let mut checks = Checks { failed: Vec::new(), passed: 0 };
    println!("private before any window: {:.1} MB", private_mb());
    let t = Instant::now();
    let png = std::fs::read(fixture_path()).expect("test/fake-screen.png");
    let base = Frame::from_png(&png).expect("decode fixture");
    let frame = base.resize(3840, 2160).expect("resize");
    let second_frame = base.resize(1920, 1080).expect("resize");
    let third_frame = base.resize(2560, 1440).expect("resize");
    drop(base);
    println!("fixture ready in {:.0} ms, private {:.1} MB", t.elapsed().as_secs_f64() * 1000.0, private_mb());

    bench_conversion(&frame);
    {
        // Everything that lazily initializes once is in place after a first session.
        let (_m, rx) = open(ui, OverlayMode::Normal, &frame);
        cancel_and_wait(ui, rx, &mut checks);
    }
    let objects_before = gui_objects();
    bench_show(ui, &frame, &mut checks);
    let objects_after = gui_objects();
    println!("GDI and USER objects before and after 41 sessions: {objects_before:?} {objects_after:?}");
    checks.ok(
        objects_after.0 <= objects_before.0 + 4 && objects_after.1 <= objects_before.1 + 4,
        format!("no window or GDI object stays behind: {objects_before:?} -> {objects_after:?}"),
    );
    idle_does_not_paint(ui, &frame, &mut checks);
    {
        let (_m, rx) = open(ui, OverlayMode::Normal, &frame);
        println!("private with one 4K overlay open (fixture frames loaded): {:.1} MB", private_mb());
        cancel_and_wait(ui, rx, &mut checks);
    }
    bench_stroke(ui, &frame, out, &mut checks);
    if !bench_only {
        reference_shots(ui, host, &frame, out, &mut checks);
        interaction_shots(ui, host, &frame, out, &mut checks);
        modes(ui, host, &frame, out, &mut checks);
        language_and_theme(ui, host, &frame, out, &mut checks);
        sessions(ui, &frame, &second_frame, &third_frame, &mut checks);
        is_open_never_blocks(ui, &frame, &mut checks);
    }
    bench_first_text(ui, &frame, &mut checks);
    drop(second_frame);
    drop(third_frame);

    // Only the overlay holds pixels from here on: the frame moves into it, as the application's does.
    println!("waiting 10 s with the fixtures freed...");
    drop(frame);
    std::thread::sleep(Duration::from_secs(10));
    let before = private_mb();
    println!("private before a session, fixtures freed: {before:.1} MB");
    let base = Frame::from_png(&png).expect("decode fixture");
    let frame = base.resize(3840, 2160).expect("resize");
    drop(base);
    let marks = Marks::new();
    let (tx, rx) = mpsc::channel();
    ui.overlay_begin(OverlayMode::Normal, marks.sink.clone(), Box::new(move |o| tx.send(o).expect("receiver"))).expect("begin");
    ui.overlay_add_display(primary(), frame, true).expect("add");
    marks.wait("show");
    println!("private with one 4K overlay open, nothing else holding pixels: {:.1} MB", private_mb());
    ui.overlay_cancel();
    wait_closed(ui);
    checks.ok(rx.recv_timeout(Duration::from_secs(5)) == Ok(OverlayOutcome::Cancelled), "the measured session ended");
    println!("private right after close: {:.1} MB", private_mb());
    std::thread::sleep(Duration::from_secs(10));
    let after = private_mb();
    println!("private 10 s after close: {after:.1} MB");
    checks.ok(after < 40.0, format!("the process went back down: {after:.1} MB"));

    println!("checks passed: {}, failed: {}", checks.passed, checks.failed.len());
    checks.failed
}

/// The non-ghost path: motion on, no activation refused, no offset added. The display itself is far off
/// every monitor and the session does not ask for focus, so this stays as invisible as ghost mode.
fn real_path_offscreen(ui: &Ui, out: &Path) -> Vec<String> {
    let mut checks = Checks { failed: Vec::new(), passed: 0 };
    let png = std::fs::read(fixture_path()).expect("test/fake-screen.png");
    let frame = Frame::from_png(&png).expect("decode fixture").resize(1920, 1080).expect("resize");
    let bounds = Rect::new(30000, 0, 1920, 1080);
    let display = Display { id: 7, bounds, work_area: bounds, scale_factor: 1.0, physical_bounds: bounds, primary: false };
    let marks = Marks::new();
    let (tx, rx) = mpsc::channel();
    ui.overlay_begin(OverlayMode::Normal, marks.sink.clone(), Box::new(move |o| tx.send(o).expect("receiver"))).expect("begin");
    // Not focused: the one thing this path may not do from here is call SetForegroundWindow.
    ui.overlay_add_display(display, frame, false).expect("add");
    let deadline = Instant::now() + Duration::from_secs(10);
    while ui.overlay_bounds().is_empty() {
        assert!(Instant::now() < deadline, "the window never opened");
        std::thread::sleep(Duration::from_millis(2));
    }
    checks.ok(ui.overlay_bounds() == [bounds], "no ghost offset on this path");
    native_window_checks(0, (30000, 0, 1920, 1080), false, &mut checks);

    action(ui, 0, json!({ "type": "select", "rect": { "x": 180, "y": 105, "w": 825, "h": 480 } }));
    ui.capture_page("overlay", 0, &out.join("real-entering.png")).expect("capture");
    std::thread::sleep(Duration::from_millis(400));
    ui.capture_page("overlay", 0, &out.join("real-entered.png")).expect("capture");
    let (before, _) = overlay::paint_stats_for_test();
    checks.ok(before > 0, "the window paints through WM_PAINT here too");
    std::thread::sleep(Duration::from_secs(1));
    let (after, _) = overlay::paint_stats_for_test();
    checks.ok(after == before, format!("{} paints in a second after the animation ended", after - before));

    ui.overlay_cancel();
    wait_closed(ui);
    checks.ok(rx.recv_timeout(Duration::from_secs(5)) == Ok(OverlayOutcome::Cancelled), "cancel ends the session");
    println!("checks passed: {}, failed: {}", checks.passed, checks.failed.len());
    checks.failed
}

fn main() {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let bench_only = args.iter().any(|a| a == "--bench-only");
    let real_path = args.iter().any(|a| a == "--real-path-offscreen");
    args.retain(|a| !a.starts_with("--"));
    let out = args.into_iter().next().map_or_else(|| std::env::temp_dir().join("shotik-overlay-preview"), PathBuf::from);
    std::fs::create_dir_all(&out).expect("output directory");
    println!("output: {}", out.display());

    let ui = Arc::new(Ui::new(UiEnv { ghost: !real_path, popup_real: false, translate_popup_ttl_ms: 9000 }).expect("ui"));
    let host = Arc::new(PreviewHost::default());
    ui.set_host(host.clone());

    let driver_ui = ui.clone();
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::spawn(move || {
        // The event loop has to be running before the first window.
        std::thread::sleep(Duration::from_millis(100));
        let scenario = || if real_path { real_path_offscreen(&driver_ui, &out) } else { run(&driver_ui, &host, &out, bench_only) };
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(scenario))
            .unwrap_or_else(|_| vec!["the scenario panicked".to_string()]);
        result_tx.send(failed).expect("main waits");
        Ui::quit();
    });
    ui.run().expect("event loop");
    let failed = result_rx.recv().expect("result");
    if !failed.is_empty() {
        println!("{} checks failed", failed.len());
        std::process::exit(1);
    }
    println!("all checks passed");
}
