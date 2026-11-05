//! `POST /test/<route>`: `setupTestHandler` in `src/main/main.js`, plus the routes that replace running
//! JavaScript inside windows (docs/rust/CONTRACT.md). Only installed in test mode.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use shotik_core::highlight::{find_highlight_rect, HighlightOptions};
use shotik_core::ports::{
    HistoryFilter, OverlayAction, OverlayActionKind, RecordTarget, Theme, Toast, TranslatePopup,
};
use shotik_core::{ocr_select, Error, Frame, Point, PointF, Rect, Result};

use crate::host::history_json;
use crate::selection::crop_with_padding;
use crate::App;

fn wait(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn str_of<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

fn i32_of(body: &Value, key: &str, fallback: i32) -> i32 {
    body.get(key).and_then(Value::as_f64).map_or(fallback, |v| v.round() as i32)
}

fn required_path(body: &Value) -> Result<&Path> {
    str_of(body, "path").map(Path::new).ok_or_else(|| Error::msg("test route needs \"path\""))
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| Error::io("read", path, e))
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| Error::msg(format!("cannot serialize a test reply: {e}")))
}

impl App {
    pub(crate) fn test_route(self: &Arc<Self>, route: &str, body: Value) -> Result<Value> {
        let ui = self.ports.ui.clone();
        let target = str_of(&body, "target").unwrap_or("overlay").to_string();
        let index = body.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        match route {
            "ping" => Ok(json!("pong")),
            "update-status" => Ok(self.update_status()),
            "update-check" => {
                self.check_for_update(true);
                Ok(self.update_status())
            }
            "trigger" => self.route_trigger(&body),
            "record-stop" => {
                self.stop_recording();
                wait(800);
                to_json(&self.ports.recorder.status())
            }
            "record-status" => to_json(&self.ports.recorder.status()),
            "record-start" => self.route_record_start(&body),
            "overlay-timing" => Ok(json!(self.state().overlay_timing)),
            "overlay-cancel" => {
                ui.overlay_cancel();
                Ok(json!(true))
            }
            "translate-selection" => {
                self.trigger_translate_selection();
                wait(500);
                Ok(json!({ "popup": ui.translate_popup_visible() }))
            }
            "selection-bubble" => {
                self.on_selection_detected(
                    str_of(&body, "text").unwrap_or(""),
                    i32_of(&body, "x", 900),
                    i32_of(&body, "y", 500),
                );
                wait(250);
                Ok(json!({ "bubble": ui.selection_bubble_visible(), "bounds": ui.selection_bubble_bounds() }))
            }
            "bubble-activate" => {
                self.bubble_activate();
                wait(body.get("wait").and_then(Value::as_u64).unwrap_or(500));
                Ok(json!({ "popup": ui.translate_popup_visible() }))
            }
            "bubble-clear" => {
                ui.hide_selection_bubble();
                Ok(json!({ "bubble": ui.selection_bubble_visible() }))
            }
            "inject-clear" => {
                self.clear_selection_bubble();
                Ok(json!({ "bubble": ui.selection_bubble_visible() }))
            }
            "show-tp-loading" => {
                ui.show_translate_popup(TranslatePopup {
                    original: str_of(&body, "text").unwrap_or("x").to_string(),
                    loading: true,
                    loading_text: Some("…".to_string()),
                    ..TranslatePopup::default()
                });
                wait(150);
                Ok(json!({ "popup": ui.translate_popup_visible() }))
            }
            "resolve-tp" => {
                ui.update_translate_popup(TranslatePopup {
                    original: str_of(&body, "text").unwrap_or("x").to_string(),
                    translated: Some(str_of(&body, "translated").unwrap_or("ok").to_string()),
                    source: Some("en".to_string()),
                    target: Some("ru".to_string()),
                    ..TranslatePopup::default()
                });
                wait(60);
                Ok(json!({ "popup": ui.translate_popup_visible() }))
            }
            "tp-visible" => {
                if let Some(ms) = body.get("wait").and_then(Value::as_u64) {
                    wait(ms);
                }
                Ok(json!({ "popupVisible": ui.translate_popup_visible() }))
            }
            "tp-hover" => {
                let over = body.get("over").and_then(Value::as_bool).unwrap_or(false);
                ui.ui_action("tp", 0, json!({ "type": "hover", "over": over }))?;
                Ok(json!("ok"))
            }
            "ocr-selection" => {
                let text = self.recognize_for_selection(&read(required_path(&body)?)?)?;
                Ok(json!({ "text": text }))
            }
            "word-at" => {
                let png = read(required_path(&body)?)?;
                let x = body.get("x").and_then(Value::as_f64).unwrap_or(0.0);
                let y = body.get("y").and_then(Value::as_f64).unwrap_or(0.0);
                let text = ocr_select::recognize_word_at(self.ports.ocr.as_ref(), &png, x, y, self.env.platform)?;
                Ok(json!({ "text": text }))
            }
            "ocr-highlight" => {
                let png = read(required_path(&body)?)?;
                let image = Frame::from_png(&png)?;
                let found = find_highlight_rect(
                    image.bgra(),
                    image.width(),
                    image.height(),
                    i32_of(&body, "sx", 0),
                    i32_of(&body, "sy", 0),
                    HighlightOptions::default(),
                );
                let ocr_png = match found {
                    Some(rect) => crop_with_padding(&image, rect)?.to_png()?,
                    None => png,
                };
                let text = self.recognize_for_selection(&ocr_png)?.trim().to_string();
                Ok(json!({ "text": text, "cropped": found.is_some(), "rect": found }))
            }
            "sim-down" => {
                self.on_mouse_down(Point { x: i32_of(&body, "x", 0), y: i32_of(&body, "y", 0) });
                wait(40);
                Ok(json!({ "bubble": ui.selection_bubble_visible(), "popup": ui.translate_popup_visible() }))
            }
            "gesture-sim" => {
                // The tail of a gesture, post-OCR, with a known fixture: no real display geometry.
                let text = self.recognize_for_selection(&read(required_path(&body)?)?)?.trim().to_string();
                let last = self.state().last_translated.clone();
                if !text.is_empty() && last.as_deref() != Some(text.as_str()) {
                    self.state().pending_selection = Some(text.clone());
                    ui.show_selection_bubble(PointF {
                        x: body.get("x").and_then(Value::as_f64).unwrap_or(900.0),
                        y: body.get("y").and_then(Value::as_f64).unwrap_or(500.0),
                    });
                }
                wait(250);
                Ok(json!({ "text": text, "bubble": ui.selection_bubble_visible() }))
            }
            "state" => Ok(self.route_state()),
            "show-toast" => {
                ui.show_toast(Toast {
                    kind: str_of(&body, "kind").unwrap_or("text").to_string(),
                    title: str_of(&body, "title").unwrap_or("T").to_string(),
                    body: str_of(&body, "body").unwrap_or("B").to_string(),
                    thumb: None,
                    file: None,
                    color: None,
                    action: None,
                });
                wait(250);
                Ok(json!({ "visible": ui.toast_visible() }))
            }
            "toast-visible" => Ok(json!({ "visible": ui.toast_visible() })),
            "toast-hover" => {
                let over = body.get("over").and_then(Value::as_bool).unwrap_or(false);
                ui.ui_action("toast", 0, json!({ "type": "hover", "over": over }))?;
                Ok(json!("ok"))
            }
            "hotkeys" => {
                match str_of(&body, "op") {
                    Some("suspend") => self.suspend_hotkeys(),
                    Some("resume") => self.resume_hotkeys(),
                    _ => {}
                }
                Ok(json!({ "suspended": self.state().hotkeys_suspended }))
            }
            "clipboard" => self.route_clipboard(),
            "sim-action" => self.route_sim_action(&body),
            "screenshot" => {
                let displays = &self.ports.displays;
                let display = match body.get("displayId").and_then(Value::as_u64) {
                    Some(id) => displays.by_id(id),
                    None => displays.under_cursor(),
                };
                let png = self.ports.capture.grab(&display)?.to_png()?;
                let out = str_of(&body, "path")
                    .map_or_else(|| self.env.temp_dir.join("shotik-test-shot.png"), Into::into);
                fs::write(&out, png).map_err(|e| Error::io("write screenshot", &out, e))?;
                Ok(json!(out))
            }
            "last-toast" => to_json(&ui.last_toast()),
            "pins" => {
                let pins: Vec<Value> = ui.pin_bounds().into_iter().map(|bounds| json!({ "bounds": bounds })).collect();
                Ok(json!(pins))
            }
            "set-theme" => {
                let current = self.ports.theme.current();
                let forced = match str_of(&body, "mode") {
                    Some("dark") => Some(Theme { dark: true, ..current }),
                    Some("light") => Some(Theme { dark: false, ..current }),
                    _ => None,
                };
                self.state().forced_theme = forced;
                let theme = self.theme();
                ui.apply_theme(theme.clone());
                to_json(&theme)
            }
            "show-main" => {
                ui.show_main();
                Ok(json!("ok"))
            }
            "quit" => {
                let quit = self.ports.quit.clone();
                self.spawn(move |_| {
                    wait(100);
                    quit();
                });
                Ok(json!("bye"))
            }
            "settings-set" => self.apply_settings(body.get("patch").cloned().unwrap_or(json!({}))),
            "history-list" => {
                let limit = body.get("limit").and_then(Value::as_u64).unwrap_or(200) as usize;
                Ok(history_json(&self.ports.history.list(limit, HistoryFilter::All)))
            }
            "export-gif" => {
                let id = str_of(&body, "id").ok_or_else(|| Error::msg("export-gif needs \"id\""))?;
                Ok(json!(self.export_gif_blocking(id)))
            }
            "ui-state" if target == "rec-controls" => {
                // Readable after a recording ends and when ghost mode built no controls (TEST-ROUTES.md).
                let mut state = ui.ui_state(&target, index).unwrap_or_else(|_| json!({ "meters": [] }));
                state["diagnostics"] = rec_diagnostics(&self.ports.recorder.status().trace);
                Ok(state)
            }
            "ui-state" => ui.ui_state(&target, index),
            "ui-action" => {
                let action = body.get("action").cloned().unwrap_or(Value::Null);
                if target == "bubble" && action.get("type").and_then(Value::as_str) == Some("pointer-down") {
                    let point: PointF = action
                        .get("point")
                        .cloned()
                        .and_then(|p| serde_json::from_value(p).ok())
                        .ok_or_else(|| Error::msg("pointer-down needs \"point\" with x and y"))?;
                    self.on_pointer_down_dip(point);
                    wait(40);
                    return Ok(json!({ "bubble": ui.selection_bubble_visible(), "popup": ui.translate_popup_visible() }));
                }
                ui.ui_action(&target, index, action)
            }
            "capture-page" => {
                let out = str_of(&body, "path")
                    .map_or_else(|| self.env.temp_dir.join("shotik-page.png"), Into::into);
                let (width, height) = ui.capture_page(&target, index, &out)?;
                Ok(json!({ "path": out, "size": { "width": width, "height": height } }))
            }
            other => Err(Error::msg(format!("unknown test route: {other}"))),
        }
    }

    fn route_trigger(self: &Arc<Self>, body: &Value) -> Result<Value> {
        match str_of(body, "mode") {
            Some("full") => {
                self.trigger_full();
                return Ok(self.state().last_processed.clone().unwrap_or(Value::Null));
            }
            Some("repeat") => {
                self.trigger_repeat();
                return Ok(self.state().last_processed.clone().unwrap_or(Value::Null));
            }
            Some("audio") => {
                self.trigger_record_audio();
                return to_json(&self.ports.recorder.status());
            }
            // Overlay modes stay open: do not wait for the user.
            Some("text") => self.spawn(|app| app.trigger_text(false)),
            Some("record") => self.spawn(|app| app.trigger_record()),
            _ => self.spawn(|app| app.trigger_region(false)),
        }
        wait(1200);
        Ok(json!({ "overlayOpen": self.ports.ui.overlay_is_open() }))
    }

    fn route_record_start(self: &Arc<Self>, body: &Value) -> Result<Value> {
        let display = match body.get("displayId").and_then(Value::as_u64) {
            Some(id) => self.ports.displays.by_id(id),
            None => self.ports.displays.primary(),
        };
        let rect_of = |key: &str| -> Option<Rect> { body.get(key).and_then(|v| serde_json::from_value(v.clone()).ok()) };
        let rect_dip = rect_of("rectDip").unwrap_or(Rect::new(20, 20, 320, 180));
        let scale = display.scale_factor;
        let rect_phys = rect_of("rectPhys").unwrap_or(Rect::new(
            (f64::from(rect_dip.x) * scale).round() as i32,
            (f64::from(rect_dip.y) * scale).round() as i32,
            (f64::from(rect_dip.width) * scale).round() as i32,
            (f64::from(rect_dip.height) * scale).round() as i32,
        ));
        let status = self.start_recording(RecordTarget::Region { display_id: display.id, rect_phys, rect_dip })?;
        to_json(&status)
    }

    fn route_state(self: &Arc<Self>) -> Value {
        let ui = &self.ports.ui;
        let displays: Vec<Value> = self
            .ports
            .displays
            .all()
            .iter()
            .map(|d| json!({ "id": d.id, "bounds": d.bounds, "workArea": d.work_area }))
            .collect();
        // Ask the UI thread first: it takes the state lock to record the overlay's marks, so waiting for it
        // while holding that lock stalls both for the 30 s of `on_ui_wait`.
        let (overlay_open, overlay_bounds, main_visible) = (ui.overlay_is_open(), ui.overlay_bounds(), ui.main_visible());
        let mcp = self.mcp_status();
        let state = self.state();
        json!({
            "overlayOpen": overlay_open,
            "overlayCount": overlay_bounds.len(),
            "overlayBounds": overlay_bounds,
            "displays": displays,
            "mainVisible": main_visible,
            "lastProcessed": state.last_processed,
            "hotkeyErrors": state.hotkey_errors,
            "hotkeysSuspended": state.hotkeys_suspended,
            "mcp": mcp,
        })
    }

    fn route_clipboard(&self) -> Result<Value> {
        let clipboard = &self.ports.clipboard;
        let image = clipboard.read_image()?;
        Ok(json!({
            "text": clipboard.read_text()?,
            "hasImage": image.is_some(),
            "imageSize": image.as_ref().map(|i| json!({ "width": i.width(), "height": i.height() })),
            "formats": clipboard.formats()?,
        }))
    }

    /// Runs an overlay action end to end and reports what landed on the clipboard. The user's clipboard
    /// is saved and restored around it.
    fn route_sim_action(self: &Arc<Self>, body: &Value) -> Result<Value> {
        let clipboard = &self.ports.clipboard;
        let saved_text = clipboard.read_text().unwrap_or_default();
        let saved_image = clipboard.read_image().unwrap_or_default();
        let png = str_of(body, "pngBase64").map(decode_base64).transpose()?;
        let action = match str_of(body, "action").unwrap_or("copy") {
            "claude" => OverlayActionKind::Claude,
            "copy" => OverlayActionKind::Copy,
            other => return Err(Error::msg(format!("sim-action does not simulate \"{other}\""))),
        };
        self.handle_overlay_result(OverlayAction { action, png, text: None, display_id: None, rect_phys: None, rect_dip: None });
        let image = clipboard.read_image()?;
        let reply = json!({ "text": clipboard.read_text()?, "hasImage": image.is_some(), "formats": clipboard.formats()? });
        match &saved_image {
            Some(image) => clipboard.write_image(image)?,
            None if !saved_text.is_empty() => clipboard.write_text(&saved_text)?,
            None => clipboard.clear()?,
        }
        Ok(reply)
    }
}

/// What really happened in the latest recording, read from the recorder's trace: never a default that
/// looks like success.
fn rec_diagnostics(trace: &[String]) -> Value {
    let tracks = |key: &str| {
        trace.iter().rev().find(|line| line.starts_with("got source stream")).and_then(|line| {
            let value = line.split(key).nth(1)?;
            value.split_whitespace().next()?.parse::<u32>().ok()
        })
    };
    json!({
        "audioCodec": trace.iter().any(|l| l.starts_with("recorder.start audio/mp4;codecs=mp4a.40.2")).then_some("aac"),
        "indexed": trace.iter().any(|l| l.starts_with("index ok")),
        "screenVideoTracks": tracks("vtracks="),
        "screenAudioTracks": tracks("atracks="),
    })
}

fn decode_base64(text: &str) -> Result<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in text.bytes().filter(|b| *b != b'=') {
        let value = ALPHABET
            .iter()
            .position(|c| *c == byte)
            .ok_or_else(|| Error::msg("pngBase64 is not base64"))? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(out)
}
