#![allow(dead_code)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use serde_json::{json, Value};
use shotik_core::ports::*;
use shotik_core::{Display, Error, Frame, HistoryEntry, Point, PointF, Rect, Result, Settings};
use shotik_mcp::{McpServer, WindowCaptureOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use uuid::Uuid;

pub struct TestDir(pub PathBuf);
impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("shotik-r2-mcp-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

pub fn patterned(width: u32, height: u32) -> Frame {
    let mut pixels = vec![0; width as usize * height as usize * 4];
    for (i, px) in pixels.chunks_exact_mut(4).enumerate() {
        px.copy_from_slice(&[(i % 251) as u8, ((i / 7) % 239) as u8, 100, 255]);
    }
    Frame::new(width, height, pixels).unwrap()
}

pub fn flat(width: u32, height: u32) -> Frame {
    Frame::new(
        width,
        height,
        vec![100; width as usize * height as usize * 4],
    )
    .unwrap()
}

pub fn display(id: u64, bounds: Rect, scale: f64, physical_bounds: Rect, primary: bool) -> Display {
    Display {
        id,
        bounds,
        work_area: bounds,
        scale_factor: scale,
        physical_bounds,
        primary,
    }
}

pub struct FakeSettings(pub Mutex<Settings>);
impl SettingsStore for FakeSettings {
    fn get(&self) -> Settings {
        self.0.lock().unwrap().clone()
    }
    fn set(&self, _: Value) -> Result<Settings> {
        Err(Error::msg("unused settings mutation"))
    }
}

pub struct FakeHistory {
    pub dir: Arc<TestDir>,
    pub items: Mutex<Vec<HistoryEntry>>,
    pub queries: Mutex<Vec<(usize, HistoryFilter)>>,
    pub save_error: Mutex<Option<String>>,
}
impl FakeHistory {
    pub fn entry(
        &self,
        name: &str,
        kind: Option<&str>,
        ts: i64,
        duration: Option<f64>,
    ) -> HistoryEntry {
        let file = self.dir.0.join(name);
        std::fs::write(&file, b"fake recording, no real audio").unwrap();
        let entry = HistoryEntry {
            id: name.into(),
            file,
            thumb: None,
            width: 320,
            height: 180,
            ts,
            source: "record".into(),
            kind: kind.map(str::to_owned),
            duration,
            extra: Default::default(),
        };
        self.items.lock().unwrap().insert(0, entry.clone());
        entry
    }
}
impl HistoryStore for FakeHistory {
    fn save_png(
        &self,
        png: &[u8],
        image: &Frame,
        source: &str,
        _: Option<&Path>,
    ) -> Result<HistoryEntry> {
        if let Some(error) = self.save_error.lock().unwrap().clone() {
            return Err(Error::msg(error));
        }
        let name = format!("{}.png", Uuid::new_v4());
        let entry = HistoryEntry {
            id: name.clone(),
            file: self.dir.0.join(name),
            thumb: None,
            width: image.width(),
            height: image.height(),
            ts: 1_700_000_000_123,
            source: source.into(),
            kind: Some("image".into()),
            duration: None,
            extra: Default::default(),
        };
        std::fs::write(&entry.file, png).unwrap();
        self.items.lock().unwrap().insert(0, entry.clone());
        Ok(entry)
    }
    fn add_recording(&self, _: &RecordingDone) -> Result<HistoryEntry> {
        Err(Error::msg("unused recording save"))
    }
    fn list(&self, limit: usize, filter: HistoryFilter) -> Vec<HistoryEntry> {
        self.queries.lock().unwrap().push((limit, filter));
        let mut items = self.items.lock().unwrap();
        items.retain(|it| it.file.exists());
        items
            .iter()
            .filter(|it| match filter {
                HistoryFilter::All => true,
                HistoryFilter::Images => !it.is_recording(),
                HistoryFilter::Recordings => it.is_recording(),
                HistoryFilter::Audio => it.kind.as_deref() == Some("audio"),
                HistoryFilter::Video => it.kind.as_deref() == Some("video"),
            })
            .take(limit)
            .cloned()
            .collect()
    }
    fn get(&self, id: &str) -> Option<HistoryEntry> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .find(|it| it.id == id)
            .cloned()
    }
    fn remove(&self, _: &str) -> Result<()> {
        Ok(())
    }
    fn clear(&self, _: bool) -> Result<()> {
        self.items.lock().unwrap().clear();
        Ok(())
    }
}

pub struct FakeDisplays {
    pub items: Mutex<Vec<Display>>,
    pub cursor: Mutex<PointF>,
}
impl Displays for FakeDisplays {
    fn all(&self) -> Vec<Display> {
        self.items.lock().unwrap().clone()
    }
    fn primary(&self) -> Display {
        self.all().into_iter().find(|d| d.primary).unwrap()
    }
    fn cursor(&self) -> PointF {
        *self.cursor.lock().unwrap()
    }
    fn nearest(&self, p: PointF) -> Display {
        self.all()
            .into_iter()
            .find(|d| d.bounds.contains(p))
            .unwrap_or_else(|| self.primary())
    }
    fn phys_to_dip(&self, p: Point) -> PointF {
        let d = self
            .all()
            .into_iter()
            .find(|d| {
                d.physical_bounds.contains(PointF {
                    x: p.x.into(),
                    y: p.y.into(),
                })
            })
            .unwrap_or_else(|| self.primary());
        PointF {
            x: f64::from(d.bounds.x) + f64::from(p.x - d.physical_bounds.x) / d.scale_factor,
            y: f64::from(d.bounds.y) + f64::from(p.y - d.physical_bounds.y) / d.scale_factor,
        }
    }
}

pub struct Blocker {
    pub entered: Notify,
    release: Mutex<bool>,
    cv: Condvar,
}
impl Blocker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Mutex::new(false),
            cv: Condvar::new(),
        })
    }
    fn wait(&self) {
        self.entered.notify_one();
        let mut release = self.release.lock().unwrap();
        while !*release {
            release = self.cv.wait(release).unwrap();
        }
    }
    pub fn release(&self) {
        *self.release.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

pub struct FakeCapture {
    pub error: Mutex<Option<String>>,
    pub blocker: Mutex<Option<Arc<Blocker>>>,
    pub override_frame: Mutex<Option<Frame>>,
    pub grabbed: Mutex<Vec<u64>>,
}
impl ScreenCapture for FakeCapture {
    fn grab(&self, display: &Display) -> Result<Frame> {
        self.grabbed.lock().unwrap().push(display.id);
        let blocker = self.blocker.lock().unwrap().clone();
        if let Some(blocker) = blocker {
            blocker.wait();
        }
        if let Some(error) = self.error.lock().unwrap().clone() {
            return Err(Error::msg(error));
        }
        if let Some(frame) = self.override_frame.lock().unwrap().clone() {
            return Ok(frame);
        }
        Ok(patterned(
            display.physical_bounds.width as u32,
            display.physical_bounds.height as u32,
        ))
    }
}

pub struct FakeWindows {
    pub items: Mutex<Vec<NativeWindow>>,
    pub list_error: Mutex<Option<String>>,
    pub primary: Mutex<std::result::Result<Frame, String>>,
    pub fallback: Mutex<std::result::Result<Frame, String>>,
    pub calls: Mutex<Vec<(u64, WindowCaptureMethod)>>,
    pub excluded: Mutex<Vec<Option<u32>>>,
}
impl WindowSource for FakeWindows {
    fn list(&self, exclude: Option<u32>) -> Result<Vec<NativeWindow>> {
        self.excluded.lock().unwrap().push(exclude);
        if let Some(error) = self.list_error.lock().unwrap().clone() {
            return Err(Error::msg(error));
        }
        Ok(self.items.lock().unwrap().clone())
    }
    fn capture(&self, id: u64, method: WindowCaptureMethod) -> Result<Frame> {
        self.calls.lock().unwrap().push((id, method));
        match method {
            WindowCaptureMethod::PrintWindow => self.primary.lock().unwrap().clone(),
            WindowCaptureMethod::GraphicsCapture => self.fallback.lock().unwrap().clone(),
        }
        .map_err(Error::msg)
    }
}

#[derive(Default)]
pub struct FakeRecorder(pub Mutex<RecStatus>);
impl RecorderControl for FakeRecorder {
    fn start(&self, _: RecordTarget, _: RecordOptions) -> Result<Option<RecStatus>> {
        Ok(Some(self.status()))
    }
    fn stop(&self) {}
    fn pause(&self) {}
    fn resume(&self) {}
    fn cancel(&self) {}
    fn is_recording(&self) -> bool {
        self.0.lock().unwrap().recording
    }
    fn status(&self) -> RecStatus {
        self.0.lock().unwrap().clone()
    }
    fn set_listener(&self, _: Callback<RecorderEvent>) {}
}

#[derive(Default)]
pub struct FakeUi {
    pub overlay: AtomicBool,
    pub events: Mutex<Vec<MainEvent>>,
}
impl UiPort for FakeUi {
    fn overlay_begin(&self, _: OverlayMode, _: MarkSink, _: Once<OverlayOutcome>) -> Result<()> {
        Ok(())
    }
    fn overlay_add_display(&self, _: Display, _: Frame, _: bool) -> Result<()> {
        Ok(())
    }
    fn overlay_set_window_rects(&self, _: Vec<Rect>) {}
    fn overlay_cancel(&self) {}
    fn overlay_is_open(&self) -> bool {
        self.overlay.load(Ordering::SeqCst)
    }
    fn overlay_bounds(&self) -> Vec<Rect> {
        vec![]
    }
    fn show_main(&self) {}
    fn hide_main(&self) {}
    fn main_visible(&self) -> bool {
        false
    }
    fn send_to_main(&self, event: MainEvent) {
        self.events.lock().unwrap().push(event);
    }
    fn show_toast(&self, _: Toast) {}
    fn toast_visible(&self) -> bool {
        false
    }
    fn last_toast(&self) -> Option<Toast> {
        None
    }
    fn show_pin(&self, _: PinRequest) {}
    fn pin_bounds(&self) -> Vec<Rect> {
        vec![]
    }
    fn show_translate_popup(&self, _: TranslatePopup) {}
    fn update_translate_popup(&self, _: TranslatePopup) {}
    fn hide_translate_popup(&self) {}
    fn translate_popup_visible(&self) -> bool {
        false
    }
    fn translate_popup_bounds(&self) -> Option<Rect> {
        None
    }
    fn show_selection_bubble(&self, _: PointF) {}
    fn hide_selection_bubble(&self) {}
    fn selection_bubble_visible(&self) -> bool {
        false
    }
    fn selection_bubble_bounds(&self) -> Option<Rect> {
        None
    }
    fn show_countdown(&self, _: Display, _: Rect, _: Once<()>) {}
    fn show_rec_windows(&self, _: RecWindows) {}
    fn update_rec_windows(&self, _: bool, _: Option<RecLevels>) {}
    fn close_rec_windows(&self) {}
    fn apply_theme(&self, _: Theme) {}
    fn rebuild_language(&self) {}
    fn ui_state(&self, _: &str, _: usize) -> Result<Value> {
        Ok(Value::Null)
    }
    fn ui_action(&self, _: &str, _: usize, _: Value) -> Result<Value> {
        Ok(Value::Null)
    }
    fn capture_page(&self, _: &str, _: usize, _: &Path) -> Result<(u32, u32)> {
        Ok((0, 0))
    }
}

#[derive(Clone)]
pub enum Selection {
    Png(Vec<u8>),
    Cancel,
    Timeout,
    Error(String),
    Deferred,
}

pub struct Fixture {
    pub dir: Arc<TestDir>,
    pub settings: Arc<FakeSettings>,
    pub history: Arc<FakeHistory>,
    pub displays: Arc<FakeDisplays>,
    pub capture: Arc<FakeCapture>,
    pub windows: Arc<FakeWindows>,
    pub recorder: Arc<FakeRecorder>,
    pub ui: Arc<FakeUi>,
    pub selection: Arc<Mutex<Selection>>,
    pub timeouts: Arc<Mutex<Vec<u64>>>,
    pub pending_selection: Arc<Mutex<Option<Once<Result<RegionSelection>>>>>,
    pub selection_entered: Arc<Notify>,
}
impl Fixture {
    pub fn new() -> Self {
        let dir = Arc::new(TestDir::new());
        let settings: Settings = serde_json::from_value(json!({
            "saveDir": dir.0, "fileNamePattern": "test", "autoSave": false, "showToast": false,
            "hotkeys": {"region":"", "fullscreen":"", "repeatLast":"", "textGrab":"", "record":"", "recordAudio":"", "translateSelection":""},
            "translate": {"target":"en", "provider":"free", "deeplKey":"", "autoBubble":false, "ocrFallback":false},
            "recording": {"fps":30, "format":"mp4", "systemAudio":false, "microphone":false, "microphoneDeviceId":"", "countdown":false},
            "audioRecording": {"systemAudio":false, "microphone":false},
            "mcp":{"enabled":true,"port":7464}, "language":"en", "launchAtStartup":false,
            "downscaleForAI":true, "lastRegion":null,
        })).unwrap();
        Self {
            settings: Arc::new(FakeSettings(Mutex::new(settings))),
            history: Arc::new(FakeHistory {
                dir: dir.clone(),
                items: Mutex::new(vec![]),
                queries: Mutex::new(vec![]),
                save_error: Mutex::new(None),
            }),
            dir,
            displays: Arc::new(FakeDisplays {
                items: Mutex::new(vec![
                    display(
                        1,
                        Rect::new(0, 0, 160, 120),
                        1.5,
                        Rect::new(0, 0, 240, 180),
                        true,
                    ),
                    display(
                        2,
                        Rect::new(160, 20, 200, 100),
                        1.0,
                        Rect::new(240, 30, 200, 100),
                        false,
                    ),
                    display(
                        3,
                        Rect::new(-100, -20, 100, 100),
                        1.0,
                        Rect::new(-100, -30, 100, 100),
                        false,
                    ),
                ]),
                cursor: Mutex::new(PointF { x: 180.0, y: 40.0 }),
            }),
            capture: Arc::new(FakeCapture {
                error: Mutex::new(None),
                blocker: Mutex::new(None),
                override_frame: Mutex::new(None),
                grabbed: Mutex::new(vec![]),
            }),
            windows: Arc::new(FakeWindows {
                items: Mutex::new(vec![
                    NativeWindow {
                        id: 42,
                        title: "Editor".into(),
                        app: Some("edit.exe".into()),
                        bounds: Some(Rect::new(30, 45, 120, 90)),
                        minimized: false,
                    },
                    NativeWindow {
                        id: 43,
                        title: "Editor notes".into(),
                        app: None,
                        bounds: Some(Rect::new(250, 40, 80, 60)),
                        minimized: false,
                    },
                    NativeWindow {
                        id: 44,
                        title: "Minimized".into(),
                        app: Some("".into()),
                        bounds: None,
                        minimized: true,
                    },
                ]),
                list_error: Mutex::new(None),
                primary: Mutex::new(Ok(patterned(120, 90))),
                fallback: Mutex::new(Err("fallback failed".into())),
                calls: Mutex::new(vec![]),
                excluded: Mutex::new(vec![]),
            }),
            recorder: Arc::new(FakeRecorder::default()),
            ui: Arc::new(FakeUi::default()),
            selection: Arc::new(Mutex::new(Selection::Png(
                patterned(12, 10).to_png().unwrap(),
            ))),
            timeouts: Arc::new(Mutex::new(vec![])),
            pending_selection: Arc::new(Mutex::new(None)),
            selection_entered: Arc::new(Notify::new()),
        }
    }

    pub fn services(&self) -> McpServices {
        let choice = self.selection.clone();
        let timeouts = self.timeouts.clone();
        let pending = self.pending_selection.clone();
        let entered = self.selection_entered.clone();
        McpServices {
            settings: self.settings.clone(),
            history: self.history.clone(),
            displays: self.displays.clone(),
            capture: self.capture.clone(),
            windows: self.windows.clone(),
            recorder: self.recorder.clone(),
            ui: self.ui.clone(),
            select_region: Arc::new(move |timeout, done| {
                timeouts.lock().unwrap().push(timeout);
                let choice = choice.lock().unwrap().clone();
                let outcome = match choice {
                    Selection::Png(png) => Ok(RegionSelection::Selected { png }),
                    Selection::Cancel => Ok(RegionSelection::Cancelled),
                    Selection::Timeout => Ok(RegionSelection::TimedOut),
                    Selection::Error(message) => Err(Error::msg(message)),
                    Selection::Deferred => {
                        *pending.lock().unwrap() = Some(done);
                        entered.notify_one();
                        return;
                    }
                };
                done(outcome);
            }),
        }
    }

    pub fn server(&self) -> McpServer {
        McpServer::new("test-server-version", self.services()).with_window_capture_options(
            WindowCaptureOptions {
                exclude_pid: None,
                force_print_window_failure: false,
            },
        )
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
impl HttpResponse {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

pub async fn request(port: u16, method: &str, path: &str, body: &[u8]) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let headers = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", body.len());
    stream.write_all(headers.as_bytes()).await.unwrap();
    // An oversized request is allowed to close early, just like Node's
    // req.destroy(). The server's parse-error response remains readable.
    let _ = stream.write_all(body).await;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let end = response
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .expect("HTTP headers");
    let headers = std::str::from_utf8(&response[..end]).unwrap();
    let mut lines = headers.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|line| {
            let (key, value) = line.split_once(':').unwrap();
            (key.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    HttpResponse {
        status,
        headers,
        body: response[end + 4..].to_vec(),
    }
}

pub async fn rpc(port: u16, message: Value) -> HttpResponse {
    request(port, "POST", "/mcp", &serde_json::to_vec(&message).unwrap()).await
}

pub async fn tool(port: u16, name: &str, arguments: Value) -> Value {
    let response = rpc(port, json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":name, "arguments":arguments}})).await;
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let value = response.json();
    assert!(value.get("error").is_none(), "{value}");
    value["result"].clone()
}

pub fn text(result: &Value) -> &str {
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "text")
        .unwrap()["text"]
        .as_str()
        .unwrap()
}

pub fn parsed_text(result: &Value) -> Value {
    serde_json::from_str(text(result)).unwrap()
}

pub fn image(result: &Value) -> Frame {
    use base64::Engine;
    let block = result["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "image")
        .unwrap();
    assert_eq!(block["mimeType"], "image/png");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(block["data"].as_str().unwrap())
        .unwrap();
    Frame::from_png(&bytes).unwrap()
}

pub fn assert_error(result: &Value, expected: &str) {
    assert_eq!(
        result,
        &json!({"content":[{"type":"text","text":format!("Error: {expected}")}],"isError":true})
    );
}
