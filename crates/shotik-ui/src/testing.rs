//! A stand-in application, so a window can run in a preview or test without the rest of Shotik.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};
use shotik_core::ports::{AppHost, AudioDevice, HotkeyError, OcrBoxes, Once, RecStatus, Theme};
use shotik_core::{i18n, Result};

/// Answers with fixed data and records what it was asked, by name.
pub struct StubHost {
    pub theme: Mutex<Theme>,
    pub lang: Mutex<&'static str>,
    pub calls: Mutex<Vec<String>>,
}

impl Default for StubHost {
    fn default() -> Self {
        Self {
            theme: Mutex::new(Theme { dark: true, accent: "#006FC7".into(), platform: "win32".into() }),
            lang: Mutex::new("en"),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl StubHost {
    fn called(&self, name: &str) {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(name.to_string());
    }
}

impl AppHost for StubHost {
    fn app_state(&self) -> Value {
        self.called("app_state");
        json!({ "settings": {}, "version": "0.0.0-preview", "mcp": { "running": true, "port": 7464 }, "mcpLog": [], "hotkeyErrors": [], "hasLastRegion": false })
    }
    fn settings_set(&self, patch: Value) -> Result<Value> {
        self.called("settings_set");
        Ok(json!({ "settings": patch, "mcp": { "running": true, "port": 7464 }, "hotkeyErrors": [] }))
    }
    fn settings_choose_dir(&self) -> Result<Option<PathBuf>> {
        Ok(None)
    }
    fn i18n(&self) -> Value {
        let lang = *self.lang.lock().unwrap_or_else(|e| e.into_inner());
        json!({ "lang": lang, "dict": Value::Object(i18n::dict(lang)) })
    }
    fn theme(&self) -> Theme {
        self.theme.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn history_list(&self, _limit: usize) -> Vec<Value> {
        Vec::new()
    }
    fn history_remove(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    fn history_clear(&self, _delete_files: bool) -> Result<()> {
        Ok(())
    }
    fn history_copy(&self, _id: &str) -> Result<bool> {
        Ok(true)
    }
    fn history_copy_path(&self, _id: &str) -> Result<bool> {
        Ok(true)
    }
    fn history_pin(&self, _id: &str) -> Result<bool> {
        Ok(true)
    }
    fn history_reveal(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    fn history_open(&self, _id: &str) -> Result<()> {
        Ok(())
    }
    fn history_ocr(&self, _id: &str, done: Once<Result<Option<String>>>) {
        done(Ok(Some("recognized text".into())));
    }
    fn history_export_gif(&self, _id: &str, done: Once<Option<PathBuf>>) {
        done(None);
    }
    fn hotkeys_suspend(&self) {
        self.called("hotkeys_suspend");
    }
    fn hotkeys_resume(&self) -> Vec<HotkeyError> {
        self.called("hotkeys_resume");
        Vec::new()
    }
    fn capture_region(&self) {
        self.called("capture_region");
    }
    fn capture_text(&self) {
        self.called("capture_text");
    }
    fn capture_full(&self) {
        self.called("capture_full");
    }
    fn capture_repeat(&self) {
        self.called("capture_repeat");
    }
    fn capture_record(&self) {
        self.called("capture_record");
    }
    fn capture_record_audio(&self) {
        self.called("capture_record_audio");
    }
    fn rec_stop(&self) {
        self.called("rec_stop");
    }
    fn rec_pause_toggle(&self) {
        self.called("rec_pause_toggle");
    }
    fn rec_cancel(&self) {
        self.called("rec_cancel");
    }
    fn rec_status(&self) -> RecStatus {
        RecStatus::default()
    }
    fn microphones(&self) -> Result<Vec<AudioDevice>> {
        Ok(Vec::new())
    }
    fn open_external(&self, _url: &str) -> Result<()> {
        Ok(())
    }
    fn open_save_dir(&self) -> Result<()> {
        Ok(())
    }
    fn reveal_file(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
    fn translate_text(&self, text: &str, target: Option<&str>, done: Once<Result<Value>>) {
        let target = target.unwrap_or("en");
        done(Ok(json!({ "text": format!("[stub] {text}"), "source": "stub", "provider": "stub", "target": target })));
    }
    fn copy_text(&self, _text: &str) -> Result<()> {
        Ok(())
    }
    fn overlay_ocr_boxes(&self, _png: Vec<u8>, done: Once<OcrBoxes>) {
        done(OcrBoxes::default());
    }
    fn overlay_translate(&self, text: &str, done: Once<Result<Value>>) {
        done(Ok(json!({ "text": format!("[stub] {text}"), "source": "stub", "provider": "stub", "target": "en" })));
    }
    fn color_copied(&self, _hex: &str) {
        self.called("color_copied");
    }
    fn bubble_activated(&self) {
        self.called("bubble_activated");
    }
    fn pin_copy(&self, _file: &Path) -> Result<()> {
        Ok(())
    }
    fn pin_save_as(&self, _file: &Path) -> Result<()> {
        Ok(())
    }
}
