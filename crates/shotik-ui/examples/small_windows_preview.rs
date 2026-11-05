//! Ghost-only visual/contract probe. Output and its experiment log live in a fresh temp directory.
//! Run: cargo run -p shotik-ui --example small_windows_preview
#![windows_subsystem = "windows"]

use serde_json::{json, Value};
use shotik_core::{
    ports::{PinRequest, RecLevels, RecWindows, Toast, TranslatePopup, UiPort},
    Display, Error, Frame, PointF, Rect, Result,
};
use shotik_ui::{bubble, countdown, pin, rec_controls, runtime, testing::StubHost, toast, translate_popup, Ui, UiEnv};
use slint::platform::WindowEvent;
use slint::ComponentHandle;
use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};
use windows::core::BOOL;
use windows::Win32::{
    Foundation::{HWND, LPARAM, RECT},
    Graphics::Gdi::{CreateRectRgn, DeleteObject, GetWindowRgn, PtInRegion},
    System::{
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
        Threading::{GetCurrentProcess, GetCurrentProcessId},
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowDisplayAffinity, GetWindowLongPtrW,
        GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE, SM_CXVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    },
};

fn memory() -> Result<u64> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    counters.cb = std::mem::size_of_val(&counters) as u32;
    // SAFETY: reading counters of this process into a correctly sized output structure.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            counters.cb,
        )
    }
    .map_err(|e| Error::msg(format!("cannot measure private bytes: {e}")))?;
    Ok(counters.PrivateUsage as u64)
}

fn owned_visible_windows() -> Result<Vec<Value>> {
    struct Probe {
        windows: Vec<Value>,
        failed: bool,
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: EnumWindows calls synchronously with our live Probe pointer.
        let probe = unsafe { &mut *(data.0 as *mut Probe) };
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if pid != unsafe { GetCurrentProcessId() } || !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return BOOL(1);
        }
        let mut bounds = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut bounds) }.is_err() {
            probe.failed = true;
            return BOOL(0);
        }
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let mut class = [0u16; 256];
        let class_len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        let class = String::from_utf16_lossy(&class[..class_len]);
        // winit/event_loop.rs creates this message target with WS_VISIBLE so it can receive WM_PAINT.
        // It has no layered bitmap and is never displayed; it is not a Slint product surface.
        if class == "Winit Thread Event Target"
            && style & (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) == (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0)
        {
            return BOOL(1);
        }
        let region = unsafe { CreateRectRgn(0, 0, 0, 0) };
        if region.is_invalid() {
            probe.failed = true;
            return BOOL(0);
        }
        let region_kind = unsafe { GetWindowRgn(hwnd, region) }.0;
        let center = unsafe {
            PtInRegion(
                region,
                (bounds.right - bounds.left) / 2,
                (bounds.bottom - bounds.top) / 2,
            )
        }
        .as_bool();
        let corner = unsafe { PtInRegion(region, 1, 1) }.as_bool();
        unsafe {
            let _ = DeleteObject(region.into());
        }
        let mut affinity = 0;
        if unsafe { GetWindowDisplayAffinity(hwnd, &mut affinity) }.is_err() {
            probe.failed = true;
            return BOOL(0);
        }
        probe.windows.push(json!({"class":class,"x":bounds.left,"y":bounds.top,"width":bounds.right-bounds.left,"height":bounds.bottom-bounds.top,
            "noactivate":style & WS_EX_NOACTIVATE.0 != 0,"toolwindow":style & WS_EX_TOOLWINDOW.0 != 0,
            "layered":style & WS_EX_LAYERED.0 != 0,"clickThrough":style & WS_EX_TRANSPARENT.0 != 0,
            "region":{"kind":region_kind,"center":center,"corner":corner},"affinity":affinity,
            "foreground":unsafe { GetForegroundWindow() } == hwnd}));
        BOOL(1)
    }
    let mut probe = Probe {
        windows: Vec::new(),
        failed: false,
    };
    // SAFETY: local callback data stays alive until EnumWindows returns. Other windows are never changed.
    unsafe { EnumWindows(Some(visit), LPARAM((&mut probe as *mut Probe) as isize)) }
        .map_err(|e| Error::msg(format!("cannot enumerate preview windows: {e}")))?;
    if probe.failed {
        return Err(Error::msg("cannot inspect a preview window"));
    }
    // A monitor left of the primary has a negative origin: x+20000 need not itself exceed 20000.
    let desktop_right = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    for window in &probe.windows {
        if window["x"].as_i64().unwrap_or(0) <= i64::from(desktop_right)
            || window["noactivate"] != true
            || window["toolwindow"] != true
            || window["foreground"] != false
        {
            return Err(Error::msg(format!("ghost safety check failed: {window}")));
        }
    }
    Ok(probe.windows)
}

fn until(name: &str, mut condition: impl FnMut() -> Result<bool>) -> Result<()> {
    let start = Instant::now();
    loop {
        if condition()? {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(Error::msg(format!("preview timed out: {name}")));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn flush() -> Result<()> {
    runtime::on_ui_wait(|| ())
}
fn tr(key: &'static str) -> Result<String> {
    runtime::on_ui_wait(move || runtime::tr(key).to_string())
}

struct Probe {
    root: PathBuf,
    captures: Vec<Value>,
    peak: u64,
}
impl Probe {
    fn capture(&mut self, name: &str, target: &str) -> Result<()> {
        let path = self.root.join(format!("{name}.png"));
        let size = if target == "countdown" {
            countdown::capture_page(&path)?
        } else {
            Ui.capture_page(target, 0, &path)?
        };
        let decoded = Frame::from_png(&std::fs::read(&path).map_err(|e| Error::io("read preview", &path, e))?)?;
        if size.0 < 10 || size.1 < 10 || !decoded.bgra().chunks_exact(4).any(|pixel| pixel[3] != 0) {
            return Err(Error::msg(format!("empty preview: {name}")));
        }
        self.peak = self.peak.max(memory()?);
        let windows = owned_visible_windows()?;
        if windows.is_empty() {
            return Err(Error::msg(format!("no visible ghost surface: {name}")));
        }
        if target == "bubble" {
            assert!(windows
                .iter()
                .any(|w| w["region"]["kind"] != 0 && w["region"]["center"] == true && w["region"]["corner"] == false));
        }
        if target == "rec-border" {
            assert!(windows.iter().any(|w| w["affinity"] == 0x11
                && w["clickThrough"] == true
                && w["layered"] == true
                && w["region"]["center"] == false
                && w["region"]["corner"] == true));
        }
        self.captures.push(json!({"name":name,"size":size,"windows":windows}));
        Ok(())
    }
}

fn display() -> Display {
    // Explicit fixture monitor: no screen pixels or real audio are ever read.
    Display {
        id: 1,
        bounds: Rect::new(0, 0, 2560, 1440),
        work_area: Rect::new(0, 0, 2560, 1400),
        physical_bounds: Rect::new(0, 0, 3840, 2160),
        scale_factor: 1.5,
        primary: true,
    }
}

fn render(root: &Path, host: Arc<StubHost>) -> Result<Value> {
    let before = memory()?;
    let mut probe = Probe {
        root: root.to_owned(),
        captures: Vec::new(),
        peak: before,
    };
    let fake_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fake-screen.png");
    let fake = Frame::from_png(&std::fs::read(&fake_path).map_err(|e| Error::io("read fake screen", &fake_path, e))?)?;
    let crop = root.join("fixture-pin.png");
    std::fs::write(&crop, fake.crop(Rect::new(240, 140, 1100, 640))?.to_png()?)
        .map_err(|e| Error::io("write fixture", &crop, e))?;
    drop(fake); // The tray measurement must not retain the preview's decoded source frame.
    let missing = root.join("missing.png");
    assert!(runtime::on_ui_wait(move || runtime::uncached_image(&missing).map(drop))?.is_err());
    let corrupt = root.join("corrupt.png");
    std::fs::write(&corrupt, b"not a PNG").map_err(|e| Error::io("write invalid fixture", &corrupt, e))?;
    assert!(runtime::on_ui_wait(move || runtime::uncached_image(&corrupt).map(drop))?.is_err());

    for dark in [true, false] {
        let suffix = if dark { "" } else { "-light" };
        let theme = {
            let mut theme = host.theme.lock().unwrap();
            theme.dark = dark;
            theme.clone()
        };
        Ui.apply_theme(theme);
        flush()?;
        for (kind, title, body) in [
            ("image", tr("copiedTitle")?, "Shot_2026-11-04_20-47-49.png"),
            (
                "claude",
                tr("claudeTitle")?,
                "Ctrl+V in the console — pastes the image or the path",
            ),
            ("repeat", tr("repeatTitle")?, "Shot_2026-11-04_20-47-50.png"),
            ("text", "Text copied".into(), "Hello Shotik 123 — fixture text only."),
            ("color", tr("colorTitle")?, "#3E9EFF"),
            ("video", tr("recSavedTitle")?, "Video · 2s"),
            ("audio", tr("recSavedTitle")?, "Audio · 2s"),
            ("gif", "GIF ready".into(), "Reference-clip.gif"),
        ] {
            Ui.show_toast(Toast {
                kind: kind.into(),
                title,
                body: body.into(),
                thumb: ["image", "claude", "repeat", "video"].contains(&kind).then(|| {
                    if kind == "repeat" {
                        crop.clone()
                    } else {
                        fake_path.clone()
                    }
                }),
                file: None,
                color: (kind == "color").then(|| "#3E9EFF".into()),
                action: None,
            });
            until("toast visible", || Ok(Ui.toast_visible()))?;
            Ui.ui_action("toast", 0, json!({"type":"hover","over":true}))?;
            probe.capture(&format!("toast-{kind}{suffix}"), "toast")?;
        }
        toast::with_preview_window(|window| {
            window.invoke_open_file();
            Ok(())
        })?;
        flush()?;
        assert!(!Ui.toast_visible());
        assert!(Ui.ui_action("toast", 0, json!({"type":"unknown"})).is_err());
        assert!(Ui.ui_action("tp", 0, json!({"type":"hover","over":null})).is_err());

        Ui.show_pin(PinRequest {
            file: crop.clone(),
            bounds: Rect::new(240, 140, 1100, 640),
        });
        until("pin ready", || Ok(Ui.ui_state("pin", 0)?["ready"] == true))?;
        probe.capture(&format!("pin{suffix}"), "pin")?;
        let bounds = Ui.pin_bounds();
        assert_eq!(bounds.len(), 1);
        assert_eq!(bounds[0], Rect::new(20240, 140, 1100, 640));
        pin::with_preview_window(0, |window| {
            window
                .window()
                .dispatch_event_with_result(WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(40.0, 40.0),
                })
                .map_err(|e| Error::msg(e.to_string()))?;
            window
                .window()
                .dispatch_event_with_result(WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(40.0, 40.0),
                    delta_x: 0.0,
                    delta_y: 120.0,
                })
                .map_err(|e| Error::msg(e.to_string()))?;
            Ok(())
        })?;
        flush()?;
        assert_eq!((Ui.pin_bounds()[0].width, Ui.pin_bounds()[0].height), (1210, 704));
        probe.capture(&format!("pin-hover-zoom{suffix}"), "pin")?;
        pin::with_preview_window(0, |window| {
            window
                .window()
                .dispatch_event_with_result(WindowEvent::KeyPressed { text: "0".into() })
                .map_err(|e| Error::msg(e.to_string()))?;
            Ok(())
        })?;
        flush()?;
        assert_eq!((Ui.pin_bounds()[0].width, Ui.pin_bounds()[0].height), (1100, 640));
        pin::with_preview_window(0, |window| {
            window
                .window()
                .dispatch_event_with_result(WindowEvent::KeyPressed {
                    text: slint::platform::Key::Escape.into(),
                })
                .map_err(|e| Error::msg(e.to_string()))?;
            Ok(())
        })?;
        flush()?;
        assert!(Ui.ui_state("pin", 0).is_err());

        Ui.show_pin(PinRequest {
            file: crop.clone(),
            bounds: Rect::new(400, 400, 320, 180),
        });
        until("double-click pin", || Ok(Ui.ui_state("pin", 0)?["ready"] == true))?;
        pin::with_preview_window(0, |window| {
            let position = slint::LogicalPosition::new(50.0, 50.0);
            window
                .window()
                .dispatch_event_with_result(WindowEvent::PointerMoved { position })
                .map_err(|e| Error::msg(e.to_string()))?;
            for _ in 0..2 {
                for event in [
                    WindowEvent::PointerPressed {
                        position,
                        button: slint::platform::PointerEventButton::Left,
                    },
                    WindowEvent::PointerReleased {
                        position,
                        button: slint::platform::PointerEventButton::Left,
                    },
                ] {
                    window
                        .window()
                        .dispatch_event_with_result(event)
                        .map_err(|e| Error::msg(e.to_string()))?;
                }
            }
            Ok(())
        })?;
        flush()?;
        assert!(Ui.pin_bounds().is_empty(), "double-click must close the pin");

        Ui.show_selection_bubble(PointF { x: 900.0, y: 500.0 });
        until("bubble visible", || Ok(Ui.selection_bubble_visible()))?;
        assert!(Ui
            .selection_bubble_bounds()
            .is_some_and(|b| b.x >= 20000 && b.width == 60));
        probe.capture(&format!("selection-bubble{suffix}"), "bubble")?;
        bubble::with_preview_window(|window| {
            window.invoke_activate();
            Ok(())
        })?;
        flush()?;
        assert!(host.calls.lock().unwrap().iter().any(|name| name == "bubble_activated"));
        Ui.hide_selection_bubble();
        flush()?;
        assert!(Ui.selection_bubble_bounds().is_none());

        let original = "Hello Shotik, please review this example.";
        Ui.show_translate_popup(TranslatePopup {
            original: original.into(),
            loading: true,
            loading_text: Some("…".into()),
            ..Default::default()
        });
        until("popup visible", || Ok(Ui.translate_popup_visible()))?;
        assert_eq!(
            Ui.ui_state("tp", 0)?,
            json!({"original":original,"translated":"…","loading":true})
        );
        probe.capture(&format!("translate-loading{suffix}"), "tp")?;
        translate_popup::with_preview_window(|window| {
            window
                .window()
                .dispatch_event_with_result(WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(40.0, 85.0),
                })
                .map_err(|e| Error::msg(e.to_string()))?;
            assert!(
                window.get_pointer_over(),
                "hover must include the read-only translation field"
            );
            Ok(())
        })?;
        Ui.ui_action("tp", 0, json!({"type":"hover","over":true}))?;
        Ui.update_translate_popup(TranslatePopup {
            original: original.into(),
            translated: Some("Привет, Shotik, проверь этот пример.".into()),
            source: Some("en".into()),
            target: Some("ru".into()),
            ..Default::default()
        });
        flush()?;
        assert_eq!(
            Ui.ui_state("tp", 0)?,
            json!({"original":original,"translated":"Привет, Shotik, проверь этот пример.","loading":false})
        );
        assert!(Ui
            .translate_popup_bounds()
            .is_some_and(|b| b.x >= 20000 && (120..=520).contains(&b.height)));
        probe.capture(&format!("translate-result{suffix}"), "tp")?;
        Ui.update_translate_popup(TranslatePopup {
            original: original.into(),
            error: Some(tr("txtTranslateFail")?),
            ..Default::default()
        });
        flush()?;
        probe.capture(&format!("translate-error{suffix}"), "tp")?;
        Ui.update_translate_popup(TranslatePopup {
            original: original.repeat(20),
            translated: Some("A long translated line. ".repeat(90)),
            ..Default::default()
        });
        flush()?;
        probe.capture(&format!("translate-long{suffix}"), "tp")?;
        translate_popup::with_preview_window(|window| {
            window.invoke_copy();
            Ok(())
        })?;
        flush()?;
        assert!(!Ui.translate_popup_visible());

        for (inputs, name) in [
            (vec![], "silent"),
            (vec!["system"], "system"),
            (vec!["microphone"], "microphone"),
            (vec!["system", "microphone"], "both"),
        ] {
            Ui.show_rec_windows(RecWindows {
                display: display(),
                rect_dip: Some(Rect::new(20, 20, 320, 180)),
                meters: inputs.into_iter().map(str::to_owned).collect(),
            });
            until("recording controls", || {
                Ok(Ui.ui_state("rec-controls", 0)?["meters"]
                    .as_array()
                    .is_some_and(|meters| {
                        meters.len()
                            == if name == "both" {
                                2
                            } else if name == "silent" {
                                0
                            } else {
                                1
                            }
                    }))
            })?;
            Ui.update_rec_windows(
                false,
                Some(RecLevels {
                    system: 0.18,
                    microphone: 0.16,
                }),
            );
            flush()?;
            // Native windows are created at a subsequent event-loop boundary.
            until("recorder HWNDs", || Ok(owned_visible_windows()?.len() == 2))?;
            probe.capture(&format!("recording-controls-{name}{suffix}"), "rec-controls")?;
            if name == "both" {
                let state = Ui.ui_state("rec-controls", 0)?;
                assert_eq!(state["meters"][0]["input"], "system");
                assert_eq!(state["meters"][1]["input"], "microphone");
                for meter in state["meters"].as_array().unwrap() {
                    assert!(meter["fill"].as_f64().unwrap() > meter["track"].as_f64().unwrap() * 0.2);
                }
                probe.capture(&format!("recording-border{suffix}"), "rec-border")?;
                Ui.update_rec_windows(
                    true,
                    Some(RecLevels {
                        system: 1.0,
                        microphone: 0.0,
                    }),
                );
                flush()?;
                probe.capture(&format!("recording-controls-paused{suffix}"), "rec-controls")?;
                rec_controls::with_preview_window(|window| {
                    window.invoke_pause();
                    window.invoke_stop();
                    window.invoke_cancel();
                    Ok(())
                })?;
                for expected in ["rec_pause_toggle", "rec_stop", "rec_cancel"] {
                    assert!(host.calls.lock().unwrap().iter().any(|name| name == expected));
                }
            }
            Ui.close_rec_windows();
            flush()?;
        }
        Ui.show_rec_windows(RecWindows {
            display: display(),
            rect_dip: None,
            meters: vec!["microphone".into()],
        });
        until("audio controls", || Ok(owned_visible_windows()?.len() == 1))?;
        probe.capture(&format!("recording-audio-controls{suffix}"), "rec-controls")?;
        Ui.close_rec_windows();
        flush()?;
        assert_eq!(Ui.ui_state("rec-controls", 0)?["meters"], json!([]));
    }

    Ui.show_translate_popup(TranslatePopup {
        original: "selectable text".into(),
        loading: true,
        ..Default::default()
    });
    until("popup Escape probe", || Ok(Ui.translate_popup_visible()))?;
    translate_popup::with_preview_window(|window| {
        let position = slint::LogicalPosition::new(60.0, 85.0);
        for event in [
            WindowEvent::PointerMoved { position },
            WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            },
            WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            },
            WindowEvent::KeyPressed {
                text: slint::platform::Key::Escape.into(),
            },
        ] {
            window
                .window()
                .dispatch_event_with_result(event)
                .map_err(|e| Error::msg(e.to_string()))?;
        }
        Ok(())
    })?;
    flush()?;
    assert!(
        !Ui.translate_popup_visible(),
        "Escape must work while a read-only text field has local focus"
    );
    // Exercise toolkit timers as well as the fake-clock unit tests. All input remains local/semantic.
    Ui.show_translate_popup(TranslatePopup {
        original: "timer probe".into(),
        loading: true,
        ..Default::default()
    });
    until("timer popup", || Ok(Ui.translate_popup_visible()))?;
    thread::sleep(Duration::from_millis(750));
    assert!(Ui.translate_popup_visible(), "loading must not expire");
    Ui.ui_action("tp", 0, json!({"type":"hover","over":true}))?;
    Ui.update_translate_popup(TranslatePopup {
        original: "timer probe".into(),
        translated: Some("result under hover".into()),
        ..Default::default()
    });
    flush()?;
    thread::sleep(Duration::from_millis(750));
    assert!(Ui.translate_popup_visible(), "a result under hover must stay visible");
    Ui.ui_action("tp", 0, json!({"type":"hover","over":false}))?;
    until("popup auto-hide after leave", || Ok(!Ui.translate_popup_visible()))?;
    assert!(Ui.translate_popup_bounds().is_none());
    let timer_toast = Toast {
        kind: "text".into(),
        title: "Timer probe".into(),
        body: String::new(),
        thumb: None,
        file: None,
        color: None,
        action: None,
    };
    Ui.show_toast(timer_toast.clone());
    until("timer toast", || Ok(Ui.toast_visible()))?;
    Ui.ui_action("toast", 0, json!({"type":"hover","over":true}))?;
    Ui.show_toast(timer_toast);
    flush()?;
    thread::sleep(Duration::from_millis(4400));
    assert!(Ui.toast_visible(), "re-trigger under a parked cursor must stay paused");
    Ui.ui_action("toast", 0, json!({"type":"hover","over":false}))?;
    until("toast after leave", || Ok(!Ui.toast_visible()))?;

    // A supplied test fixture proves that diagnostics survive close, without claiming an encoder ran.
    let diagnostic_fixture = rec_controls::Diagnostics {
        audio_codec: Some(rec_controls::AudioCodec::Aac),
        indexed: true,
        screen_video_tracks: Some(7),
        screen_audio_tracks: Some(3),
    };
    rec_controls::set_diagnostics(diagnostic_fixture.clone());
    flush()?;
    assert_eq!(
        Ui.ui_state("rec-controls", 0)?["diagnostics"],
        serde_json::to_value(diagnostic_fixture).unwrap()
    );
    rec_controls::set_diagnostics(Default::default());
    flush()?;
    let (done_tx, done_rx) = mpsc::channel();
    Ui.show_countdown(
        display(),
        Rect::new(300, 300, 800, 600),
        Box::new(move |_| {
            let _ = done_tx.send(());
        }),
    );
    until("countdown native", || Ok(owned_visible_windows()?.len() == 1))?;
    probe.capture("countdown-3", "countdown")?;
    thread::sleep(Duration::from_millis(720));
    probe.capture("countdown-2", "countdown")?;
    thread::sleep(Duration::from_millis(700));
    probe.capture("countdown-1", "countdown")?;
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| Error::msg("countdown did not complete"))?;
    flush()?;
    assert!(owned_visible_windows()?.is_empty());
    thread::sleep(Duration::from_secs(10));
    let after = memory()?;
    Ok(
        json!({"captures":probe.captures,"privateBytes":{"before":before,"peak":probe.peak,"after10s":after},
        "checks":"ghost HWNDs, bounds, hover actions, state schemas, laid-out meter DIP, diagnostic setter persistence, countdown callback",
        "unverified":"native menus, physical input, DWM shadow/corners, actual mixed-DPI transitions, production recorder diagnostics wiring",
        "differences":"opaque toast/popup/controls/pin; bubble uses a 32-DIP elliptical region without soft shadow; countdown is an opaque rounded square"}),
    )
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("shotik-r8-small-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    std::env::set_var("SHOTIK_GHOST", "1");
    let ui = Ui::new(UiEnv {
        ghost: true,
        popup_real: false,
        translate_popup_ttl_ms: 350,
    })?;
    let host = Arc::new(StubHost::default());
    ui.set_host(host.clone());
    let (tx, rx) = mpsc::channel();
    let output = root.clone();
    slint::Timer::single_shot(Duration::ZERO, move || {
        thread::spawn(move || {
            let result = std::panic::catch_unwind(|| render(&output, host))
                .unwrap_or_else(|_| Err(Error::msg("preview assertion failed")));
            let _ = tx.send(result);
            Ui::quit();
        });
    });
    ui.run()?;
    let result = rx.recv()??;
    let report = root.join("report.json");
    std::fs::write(&report, serde_json::to_vec_pretty(&result)?)?;
    println!("{}", report.display());
    Ok(())
}
