//! Private-profile, fake-screen host for the R7 preview and view-boundary tests.
//! No system settings, clipboard, shell, microphone, capture or hotkey adapter is used.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use shotik_core::ports::{
    AppHost, AudioDevice, HotkeyError, MainEvent, OcrBoxes, Once, RecStatus, Theme, UiPort,
};
use shotik_core::{Error, Result};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::testing::StubHost;
use crate::Ui;

pub struct PreviewHost {
    pub stub: StubHost,
    pub settings: Mutex<Value>,
    pub history: Mutex<Vec<Value>>,
    pub reference_translation: AtomicBool,
    pub deny_settings: AtomicBool,
    pub mcp_running: AtomicBool,
    pub root: PathBuf,
}

impl PreviewHost {
    pub fn new(root: &Path) -> Result<Arc<Self>> {
        std::fs::create_dir_all(root)
            .map_err(|e| Error::io("create preview directory", root, e))?;
        let source =
            image::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fake-screen.png"))
                .map_err(|e| Error::msg(format!("cannot read fake screen: {e}")))?;
        let mut history = Vec::new();
        for (i, (x, y, w, h)) in [
            (240, 140, 1100, 640),
            (270, 225, 700, 190),
            (1440, 150, 240, 100),
        ]
        .into_iter()
        .enumerate()
        {
            let path = root.join(format!("Reference-{}.png", i + 1));
            source
                .crop_imm(x, y, w, h)
                .save(&path)
                .map_err(|e| Error::msg(format!("cannot write fake crop: {e}")))?;
            history.push(json!({"id":format!("fixture-{}",i+1),"file":path,"thumb":path,"width":w,"height":h,
                "source":(["region","fullscreen","repeat"][i]),"kind":"image","ts":chrono::Local::now().timestamp_millis()-(i as i64*12+2)*60000}));
        }
        let settings = shotik_core::settings::defaults(root, "en", "win32", Some(root));
        let settings = shotik_core::settings::deep_merge(
            &serde_json::to_value(settings).map_err(|e| Error::msg(e.to_string()))?,
            &json!({"language":"en","translate":{"target":"ru"},"mcp":{"port":51429},"recording":{"countdown":false,"systemAudio":true,"microphone":true}}),
        );
        Ok(Arc::new(Self {
            stub: StubHost::default(),
            settings: Mutex::new(settings),
            history: Mutex::new(history),
            reference_translation: AtomicBool::new(false),
            deny_settings: AtomicBool::new(false),
            mcp_running: AtomicBool::new(true),
            root: root.into(),
        }))
    }
    fn called(&self, name: &str) {
        self.stub.calls.lock().unwrap().push(name.into());
    }
    fn contains(&self, id: &str) -> bool {
        self.history
            .lock()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == id)
    }
    fn response(&self) -> Value {
        let settings = self.settings.lock().unwrap().clone();
        json!({"settings":settings,"version":env!("CARGO_PKG_VERSION"),"mcp":{"running":settings["mcp"]["enabled"] == true && self.mcp_running.load(Ordering::SeqCst),"port":settings["mcp"]["port"]},"mcpLog":[],"hotkeyErrors":[],"hasLastRegion":true})
    }
}

impl AppHost for PreviewHost {
    fn app_state(&self) -> Value {
        self.called("app_state");
        self.response()
    }
    fn settings_set(&self, patch: Value) -> Result<Value> {
        self.called("settings_set");
        if self.deny_settings.load(Ordering::SeqCst) {
            return Err(Error::msg("fixture settings write failed"));
        }
        {
            let mut settings = self.settings.lock().unwrap();
            *settings = shotik_core::settings::deep_merge(&settings, &patch);
            *self.stub.lang.lock().unwrap() = if settings["language"] == "ru" {
                "ru"
            } else {
                "en"
            };
        }
        Ok(self.response())
    }
    fn settings_choose_dir(&self) -> Result<Option<PathBuf>> {
        self.called("settings_choose_dir");
        let path = self.root.join("chosen");
        self.settings.lock().unwrap()["saveDir"] = json!(path);
        Ok(Some(path))
    }
    fn i18n(&self) -> Value {
        self.stub.i18n()
    }
    fn theme(&self) -> Theme {
        self.stub.theme()
    }
    fn history_list(&self, _limit: usize) -> Vec<Value> {
        self.called("history_list");
        self.history.lock().unwrap().clone()
    }
    fn history_remove(&self, id: &str) -> Result<()> {
        self.called("history_remove");
        self.history
            .lock()
            .unwrap()
            .retain(|entry| entry["id"] != id);
        Ok(())
    }
    fn history_clear(&self, delete_files: bool) -> Result<()> {
        if delete_files {
            return Err(Error::msg("preview refuses deleting files"));
        }
        self.called("history_clear:false");
        self.history.lock().unwrap().clear();
        Ok(())
    }
    fn history_copy(&self, id: &str) -> Result<bool> {
        self.called("history_copy");
        Ok(self.contains(id))
    }
    fn history_copy_path(&self, id: &str) -> Result<bool> {
        self.called("history_copy_path");
        Ok(self.contains(id))
    }
    fn history_pin(&self, id: &str) -> Result<bool> {
        self.called("history_pin");
        Ok(self.contains(id))
    }
    fn history_reveal(&self, _id: &str) -> Result<()> {
        self.called("history_reveal");
        Ok(())
    }
    fn history_open(&self, _id: &str) -> Result<()> {
        self.called("history_open");
        Ok(())
    }
    fn history_ocr(&self, _id: &str, done: Once<Result<Option<String>>>) {
        self.called("history_ocr");
        self.stub.history_ocr("fixture", done);
    }
    fn history_export_gif(&self, _id: &str, done: Once<Option<PathBuf>>) {
        self.called("history_export_gif");
        done(Some(self.root.join("fixture.gif")));
    }
    fn hotkeys_suspend(&self) {
        self.stub.hotkeys_suspend();
    }
    fn hotkeys_resume(&self) -> Vec<HotkeyError> {
        self.stub.hotkeys_resume()
    }
    fn capture_region(&self) {
        self.stub.capture_region();
    }
    fn capture_full(&self) {
        self.stub.capture_full();
    }
    fn capture_repeat(&self) {
        self.stub.capture_repeat();
    }
    fn capture_text(&self) {
        self.stub.capture_text();
    }
    fn capture_record(&self) {
        self.stub.capture_record();
    }
    fn capture_record_audio(&self) {
        self.stub.capture_record_audio();
    }
    fn rec_stop(&self) {
        self.stub.rec_stop();
    }
    fn rec_pause_toggle(&self) {
        self.stub.rec_pause_toggle();
    }
    fn rec_cancel(&self) {
        self.stub.rec_cancel();
    }
    fn rec_status(&self) -> RecStatus {
        self.stub.rec_status()
    }
    fn microphones(&self) -> Result<Vec<AudioDevice>> {
        Ok(vec![AudioDevice {
            id: "fixture-mic".into(),
            name: "Preview microphone".into(),
        }])
    }
    fn open_external(&self, url: &str) -> Result<()> {
        self.stub.open_external(url)
    }
    fn open_save_dir(&self) -> Result<()> {
        self.called("open_save_dir");
        Ok(())
    }
    fn reveal_file(&self, path: &Path) -> Result<()> {
        self.stub.reveal_file(path)
    }
    fn translate_text(&self, text: &str, target: Option<&str>, done: Once<Result<Value>>) {
        self.called("translate_text");
        if self.reference_translation.load(Ordering::SeqCst) {
            done(Ok(
                json!({"text":format!("<{}> {text}",target.unwrap_or("en")),"source":"stub","target":target.unwrap_or("en")}),
            ));
        } else {
            self.stub.translate_text(text, target, done);
        }
    }
    fn copy_text(&self, _text: &str) -> Result<()> {
        self.called("copy_text");
        Ok(())
    }
    fn overlay_ocr_boxes(&self, png: Vec<u8>, done: Once<OcrBoxes>) {
        self.stub.overlay_ocr_boxes(png, done);
    }
    fn overlay_translate(&self, text: &str, done: Once<Result<Value>>) {
        self.stub.overlay_translate(text, done);
    }
    fn color_copied(&self, hex: &str) {
        self.stub.color_copied(hex);
    }
    fn bubble_activated(&self) {
        self.stub.bubble_activated();
    }
    fn pin_copy(&self, file: &Path) -> Result<()> {
        self.stub.pin_copy(file)
    }
    fn pin_save_as(&self, file: &Path) -> Result<()> {
        self.stub.pin_save_as(file)
    }
}

pub fn private_bytes() -> Result<u64> {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    counters.cb = std::mem::size_of_val(&counters) as u32;
    // SAFETY: reads only the current owned process's counters into an initialized buffer.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            counters.cb,
        )
    }
    .map_err(|e| Error::msg(format!("cannot read preview private bytes: {e}")))?;
    Ok(counters.PrivateUsage as u64)
}

pub fn until(ui: &Ui, condition: impl Fn(&Value) -> bool) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state = ui.ui_state("main", 0)?;
        if condition(&state) {
            return Ok(state);
        }
        if Instant::now() >= deadline {
            return Err(Error::msg("main preview did not settle in 15 seconds"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Runs from a worker, exercising UiPort's cross-thread contract on the real product view.
pub fn check_protocol(ui: &Ui, host: &PreviewHost) -> Result<()> {
    until(ui, |s| s["ready"] == true)?;
    assert!(ui.main_visible());
    for page in ["history", "text", "claude", "settings"] {
        let state = ui.ui_action("main", 0, json!({"type":"navigate","page":page}))?;
        assert_eq!(state["page"], page);
    }
    let loading = ui.ui_action(
        "main",
        0,
        json!({"type":"quick-translate","text":"hello world"}),
    )?;
    assert_eq!(loading["text"]["loading"], true);
    assert_eq!(loading["text"]["result"], "Translating…");
    let state = until(ui, |s| {
        s["text"]["loading"] == false && s["text"]["resultVisible"] == true
    })?;
    assert_eq!(state["text"]["input"], "hello world");
    assert_eq!(state["text"]["result"], "[stub] hello world");
    assert_eq!(state["page"], "text");
    ui.ui_action(
        "main",
        0,
        json!({"type":"set-title","title":"ShotikGhostTarget-R7"}),
    )?;
    assert_eq!(ui.ui_state("main", 0)?["title"], "ShotikGhostTarget-R7");
    // Capture verifies both native creation off-screen and the real in-process software renderer.
    let png = host.root.join("contract.png");
    let (width, height) = ui.capture_page("main", 0, &png)?;
    assert_eq!((width, height), (1120, 740));
    ui.ui_action("main", 0, json!({"type":"microphone-toggle"}))?;
    let state = until(ui, |s| s["microphone"]["savedEnabled"] == false)?;
    assert_eq!(state["microphone"]["enabled"], false);
    assert_eq!(state["microphone"]["deviceRowVisible"], true);
    assert_eq!(
        host.settings.lock().unwrap()["recording"]["microphone"],
        false
    );
    ui.ui_action(
        "main",
        0,
        json!({"type":"microphone-select","deviceId":"unplugged-device"}),
    )?;
    let state = until(ui, |s| s["microphone"]["deviceId"] == "unplugged-device")?;
    assert_eq!(
        state["microphone"]["deviceLabel"],
        "Selected microphone is unavailable"
    );
    assert_eq!(
        ui.ui_action("main", 0, json!({"type":"reload"}))?["ready"],
        false
    );
    let state = until(ui, |s| s["ready"] == true)?;
    assert_eq!(state["microphone"]["savedEnabled"], false);
    assert_eq!(state["microphone"]["deviceId"], "unplugged-device");
    ui.ui_action("main", 0, json!({"type":"hotkey-focus","field":"region"}))?;
    assert_eq!(ui.ui_state("main", 0)?["page"], "settings");
    assert!(host
        .stub
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call == "hotkeys_suspend"));
    ui.ui_action("main", 0, json!({"type":"hotkey-blur","field":"region"}))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !host
        .stub
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call == "hotkeys_resume")
    {
        if Instant::now() > deadline {
            return Err(Error::msg("hotkeys were not resumed"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    ui.ui_action("main", 0, json!({"type":"history-pin","id":"fixture-1"}))?;
    ui.ui_action("main", 0, json!({"type":"history-clear"}))?;
    // Queue a reload after the side effects to synchronize their completion.
    ui.ui_action("main", 0, json!({"type":"reload"}))?;
    until(ui, |s| s["ready"] == true)?;
    assert!(host.history.lock().unwrap().is_empty());
    let calls = host.stub.calls.lock().unwrap();
    assert!(calls.iter().any(|call| call == "history_pin"));
    assert!(calls.iter().any(|call| call == "history_clear:false"));
    drop(calls);
    assert!(ui.ui_action("main", 0, json!({"type":"unknown"})).is_err());
    assert!(ui
        .ui_action("main", 0, json!({"type":"navigate","page":"unknown"}))
        .is_err());
    assert!(ui
        .ui_action("main", 0, json!({"type":"hotkey-focus","field":"unknown"}))
        .is_err());
    host.settings_set(json!({"language":"ru"}))?;
    ui.rebuild_language();
    until(ui, |s| {
        s["ready"] == true && s["microphone"]["deviceLabel"] == "Выбранный микрофон недоступен"
    })?;
    host.settings_set(
        json!({"language":"en","recording":{"microphone":true,"microphoneDeviceId":"default"}}),
    )?;
    ui.ui_action("main", 0, json!({"type":"reload"}))?;
    until(ui, |s| s["ready"] == true)?;
    ui.send_to_main(MainEvent::McpLog(json!({"ts":chrono::Local::now().timestamp_millis(),"tool":"take_screenshot","ms":42,"ok":true})));
    ui.send_to_main(MainEvent::RecState(RecStatus {
        recording: true,
        audio_only: Some(true),
        ..Default::default()
    }));
    ui.send_to_main(MainEvent::HistoryChanged);
    ui.hide_main();
    // The query is serialized after hide, so this measures the actual owned view visibility.
    assert!(!ui.main_visible());
    Ok(())
}
