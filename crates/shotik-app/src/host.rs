//! What the windows ask of the application: the IPC handlers of `setupAppIpc` in `src/main/main.js`
//! and the window handlers in `src/main/windows.js`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use shotik_core::ports::{AppHost, AudioDevice, HistoryFilter, HotkeyError, OcrBoxes, Once, PinRequest, RecStatus, Theme, Toast};
use shotik_core::{i18n, Error, Frame, HistoryEntry, Rect, Result};

use crate::actions::{file_name, text_toast};
use crate::App;

/// App state and settings:set are read by a window; the window lives in another crate and has no
/// business knowing that `Arc<App>` holds the implementation.
impl AppHost for App {
    fn app_state(&self) -> Value {
        json!({
            "settings": self.settings(),
            "version": self.env.version,
            "mcp": self.mcp_status(),
            "update": self.update_status(),
            "mcpLog": self.mcp.get().map_or_else(Vec::new, |server| server.get_log()),
            "hotkeyErrors": self.state().hotkey_errors,
            "hasLastRegion": self.settings().last_region.is_some(),
        })
    }

    fn settings_set(&self, patch: Value) -> Result<Value> {
        self.arc().apply_settings(patch)
    }

    fn settings_choose_dir(&self) -> Result<Option<PathBuf>> {
        let current = self.settings().save_dir;
        let Some(dir) = self.ports.dialogs.pick_directory(&current)? else {
            return Ok(None);
        };
        self.ports.settings.set(json!({ "saveDir": dir }))?;
        Ok(Some(dir))
    }

    fn i18n(&self) -> Value {
        let lang = self.lang();
        json!({ "lang": lang, "dict": Value::Object(i18n::dict(lang)) })
    }

    fn theme(&self) -> Theme {
        App::theme(self)
    }

    fn history_list(&self, limit: usize) -> Vec<Value> {
        let limit = if limit == 0 { 200 } else { limit };
        self.ports
            .history
            .list(limit, HistoryFilter::All)
            .iter()
            .filter_map(|entry| serde_json::to_value(entry).ok())
            .collect()
    }

    fn history_remove(&self, id: &str) -> Result<()> {
        self.ports.history.remove(id)
    }

    fn history_clear(&self, delete_files: bool) -> Result<()> {
        self.ports.history.clear(delete_files)
    }

    fn history_copy(&self, id: &str) -> Result<bool> {
        let Some(entry) = self.ports.history.get(id) else { return Ok(false) };
        self.ports.clipboard.write_image(&read_image(&entry.file)?)?;
        self.ports.ui.show_toast(Toast {
            kind: "image".into(),
            title: self.t("copiedTitle"),
            body: file_name(&entry.file),
            thumb: entry.thumb.clone(),
            file: Some(entry.file.clone()),
            color: None,
            action: None,
        });
        Ok(true)
    }

    fn history_copy_path(&self, id: &str) -> Result<bool> {
        let Some(entry) = self.ports.history.get(id) else { return Ok(false) };
        self.ports.clipboard.write_text(&entry.file.to_string_lossy())?;
        Ok(true)
    }

    fn history_pin(&self, id: &str) -> Result<bool> {
        let Some(entry) = self.ports.history.get(id) else { return Ok(false) };
        let d = self.ports.displays.primary();
        let natural = (f64::from(entry.width) / d.scale_factor).round() as i32;
        let w = natural.min((f64::from(d.work_area.width) * 0.6).round() as i32);
        let h = if entry.width == 0 { 0 } else { (f64::from(w) * f64::from(entry.height) / f64::from(entry.width)).round() as i32 };
        self.ports.ui.show_pin(PinRequest {
            file: entry.file,
            bounds: Rect::new(
                d.work_area.x + (d.work_area.width - w) / 2,
                d.work_area.y + (d.work_area.height - h) / 2,
                w,
                h,
            ),
        });
        Ok(true)
    }

    fn history_reveal(&self, id: &str) -> Result<()> {
        match self.ports.history.get(id) {
            Some(entry) => self.ports.shell.reveal_in_folder(&entry.file),
            None => Ok(()),
        }
    }

    fn history_open(&self, id: &str) -> Result<()> {
        match self.ports.history.get(id) {
            Some(entry) => self.ports.shell.open_path(&entry.file),
            None => Ok(()),
        }
    }

    fn history_ocr(&self, id: &str, done: Once<Result<Option<String>>>) {
        let Some(entry) = self.ports.history.get(id) else {
            done(Ok(None));
            return;
        };
        self.arc().spawn(move |app| {
            done((|| {
                let png = fs::read(&entry.file).map_err(|e| Error::io("read screenshot", &entry.file, e))?;
                let text = app.ports.ocr.recognize(&png, None)?;
                if !text.is_empty() {
                    app.ports.clipboard.write_text(&text)?;
                }
                Ok(Some(text))
            })());
        });
    }

    fn history_export_gif(&self, id: &str, done: Once<Option<PathBuf>>) {
        let entry = self.ports.history.get(id).filter(|e| e.kind.as_deref() == Some("video"));
        let Some(entry) = entry else {
            done(None);
            return;
        };
        self.arc().spawn(move |app| done(app.export_gif_with_toast(&entry)));
    }

    fn hotkeys_suspend(&self) {
        self.suspend_hotkeys();
    }

    fn hotkeys_resume(&self) -> Vec<HotkeyError> {
        self.arc().resume_hotkeys();
        self.state().hotkey_errors.clone()
    }

    fn capture_region(&self) {
        self.arc().spawn(|app| app.trigger_region(true));
    }

    fn capture_text(&self) {
        self.arc().spawn(|app| app.trigger_text(true));
    }

    fn capture_full(&self) {
        self.arc().spawn(|app| {
            if app.ports.ui.main_visible() {
                app.ports.ui.hide_main();
                std::thread::sleep(std::time::Duration::from_millis(180));
            }
            app.trigger_full();
        });
    }

    fn capture_repeat(&self) {
        self.arc().spawn(|app| app.trigger_repeat());
    }

    fn capture_record(&self) {
        self.arc().spawn(|app| app.trigger_record());
    }

    fn capture_record_audio(&self) {
        self.arc().spawn(|app| app.trigger_record_audio());
    }

    fn rec_stop(&self) {
        self.stop_recording();
    }

    fn rec_pause_toggle(&self) {
        if self.ports.recorder.status().paused.unwrap_or(false) {
            self.ports.recorder.resume();
        } else {
            self.ports.recorder.pause();
        }
    }

    fn rec_cancel(&self) {
        self.cancel_recording();
    }

    fn rec_status(&self) -> RecStatus {
        self.ports.recorder.status()
    }

    fn microphones(&self) -> Result<Vec<AudioDevice>> {
        self.ports.audio_devices.microphones()
    }

    fn update_check(&self) {
        let app = self.arc();
        self.update_thread("shotik-update-check", move || app.check_for_update(true));
    }

    fn update_install(&self) {
        let app = self.arc();
        self.update_thread("shotik-update-install", move || app.install_update());
    }

    fn open_external(&self, url: &str) -> Result<()> {
        if url.starts_with("http://") || url.starts_with("https://") {
            self.ports.shell.open_url(url)
        } else {
            Err(Error::msg(format!("refusing to open a non-web address: {url}")))
        }
    }

    fn open_save_dir(&self) -> Result<()> {
        let dir = self.settings().save_dir;
        fs::create_dir_all(&dir).map_err(|e| Error::io("create folder", &dir, e))?;
        self.ports.shell.open_path(&dir)
    }

    fn reveal_file(&self, path: &Path) -> Result<()> {
        if path.exists() {
            self.ports.shell.reveal_in_folder(path)
        } else {
            Ok(())
        }
    }

    fn translate_text(&self, text: &str, target: Option<&str>, done: Once<Result<Value>>) {
        if text.trim().is_empty() {
            done(Ok(json!({ "text": "", "source": "auto", "provider": "none" })));
            return;
        }
        let text = text.to_string();
        let target = target.filter(|t| !t.is_empty()).map(str::to_string);
        self.arc().spawn(move |app| {
            let chosen = target.unwrap_or_else(|| app.settings().translate.target);
            done(app.translate_call(&text, Some(&chosen)).map(|r| {
                json!({ "text": r.text, "source": r.source, "provider": r.provider, "target": chosen })
            }));
        });
    }

    fn copy_text(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.ports.clipboard.write_text(text)
    }

    /// A failed recognition is an empty Live Text layer, with the reason in the log.
    fn overlay_ocr_boxes(&self, png: Vec<u8>, done: Once<OcrBoxes>) {
        self.arc().spawn(move |app| {
            done(app.ports.ocr.recognize_boxes(&png, None).unwrap_or_else(|e| {
                app.diag.log(&format!("Live Text OCR failed: {e}"));
                OcrBoxes::default()
            }));
        });
    }

    fn overlay_translate(&self, text: &str, done: Once<Result<Value>>) {
        let text = text.to_string();
        self.arc().spawn(move |app| {
            let target = app.settings().translate.target;
            done(app.translate_call(&text, None).map(|r| {
                json!({ "text": r.text, "source": r.source, "provider": r.provider, "target": target })
            }));
        });
    }

    fn color_copied(&self, hex: &str) {
        match self.ports.clipboard.write_text(hex) {
            Ok(()) => self.ports.ui.show_toast(Toast {
                kind: "color".into(),
                title: self.t("colorTitle"),
                body: hex.to_string(),
                thumb: None,
                file: None,
                color: Some(hex.to_string()),
                action: None,
            }),
            Err(e) => self.ports.ui.show_toast(text_toast(self.t("colorTitle"), e.to_string())),
        }
    }

    fn bubble_activated(&self) {
        self.arc().bubble_activate();
    }

    fn pin_copy(&self, file: &Path) -> Result<()> {
        self.ports.clipboard.write_image(&read_image(file)?)
    }

    fn pin_save_as(&self, file: &Path) -> Result<()> {
        let Some(path) = self.ports.dialogs.save_png(file)? else { return Ok(()) };
        fs::copy(file, &path).map_err(|e| Error::io("save a copy", &path, e))?;
        Ok(())
    }
}

fn read_image(file: &Path) -> Result<Frame> {
    Frame::from_png(&fs::read(file).map_err(|e| Error::io("read image", file, e))?)
}

impl App {
    /// `App` is always behind an `Arc`; trait methods that spawn work need it back.
    fn arc(&self) -> Arc<App> {
        self.me.upgrade().expect("App is alive while its methods run")
    }

    /// `settings:set`.
    pub(crate) fn apply_settings(self: &Arc<Self>, patch: Value) -> Result<Value> {
        let before = self.settings();
        let after = self.ports.settings.set(patch)?;
        if before.hotkeys != after.hotkeys {
            self.register_hotkeys();
            self.refresh_tray();
        }
        if before.mcp.enabled != after.mcp.enabled || before.mcp.port != after.mcp.port {
            self.restart_mcp(after.mcp.enabled);
        }
        if before.launch_at_startup != after.launch_at_startup {
            self.ports.autostart.set(after.launch_at_startup)?;
        }
        if before.language != after.language {
            self.refresh_tray();
            self.ports.ui.rebuild_language();
        }
        if before.translate.auto_bubble != after.translate.auto_bubble {
            if after.translate.auto_bubble {
                self.start_selection_watcher();
            } else {
                self.stop_selection_watcher();
            }
        }
        Ok(json!({ "settings": self.settings(), "mcp": self.mcp_status(), "hotkeyErrors": self.state().hotkey_errors }))
    }

    /// Stops the old server, then starts the new one, in that order: a same-port restart needs it.
    fn restart_mcp(self: &Arc<Self>, enabled: bool) {
        let app = self.clone();
        let server = self.mcp.get();
        self.rt.spawn(async move {
            if let Some(server) = server {
                if let Err(e) = server.stop().await {
                    app.diag.log(&format!("MCP stop failed: {e}"));
                }
            }
            if enabled {
                app.start_mcp();
            }
        });
    }

    fn export_gif_with_toast(self: &Arc<Self>, entry: &HistoryEntry) -> Option<PathBuf> {
        match (self.ports.export_gif)(&entry.file) {
            Ok(gif) => {
                self.ports.ui.show_toast(Toast {
                    kind: "video".into(),
                    title: self.t("gifReadyTitle"),
                    body: file_name(&gif),
                    thumb: None,
                    file: Some(gif.clone()),
                    color: None,
                    action: None,
                });
                if let Err(e) = self.ports.clipboard.write_text(&gif.to_string_lossy()) {
                    self.diag.log(&format!("cannot copy the GIF path: {e}"));
                }
                Some(gif)
            }
            Err(e) => {
                let body: String = e.to_string().chars().take(120).collect();
                self.ports.ui.show_toast(text_toast(self.t("gifFailTitle"), body));
                None
            }
        }
    }

    pub(crate) fn export_gif_blocking(self: &Arc<Self>, id: &str) -> Option<PathBuf> {
        let entry = self.ports.history.get(id).filter(|e| e.kind.as_deref() == Some("video"))?;
        self.export_gif_with_toast(&entry)
    }
}

/// History entries as the JSON the windows and tests read.
pub(crate) fn history_json(entries: &[HistoryEntry]) -> Value {
    Value::Array(entries.iter().filter_map(|e| serde_json::to_value(e).ok()).collect())
}
