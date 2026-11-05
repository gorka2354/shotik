//! Capture triggers and what happens with an overlay result: `handleOverlayResult` and the `trigger*`
//! functions in `src/main/main.js`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use shotik_core::ports::{
    MainEvent, OverlayAction, OverlayActionKind, OverlayMode, PinRequest, RecordTarget, Toast,
};
use shotik_core::{ocr_select, translate, Error, Frame, HistoryEntry, Rect, Result};

use crate::overlay::OverlayResult;
use crate::App;

/// The main window hides before a capture so it is not in the picture.
const HIDE_MAIN_DELAY: Duration = Duration::from_millis(180);

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

pub(crate) fn text_toast(title: String, body: String) -> Toast {
    Toast { kind: "text".into(), title, body, thumb: None, file: None, color: None }
}

impl App {
    /// Plain copy puts ONLY the image on the clipboard. Adding the file path as text made apps that
    /// prefer text (messengers, chat inputs) paste the path instead of the picture, and made this
    /// indistinguishable from "copy for Claude", which is image and path on purpose.
    fn copy_image(&self, image: &Frame) -> Result<()> {
        self.ports.clipboard.write_image(image)
    }

    fn hide_main_first(&self, hide: bool) {
        if hide && self.ports.ui.main_visible() {
            self.ports.ui.hide_main();
            std::thread::sleep(HIDE_MAIN_DELAY);
        }
    }

    fn entry_toast(&self, kind: &str, title: String, entry: Option<&HistoryEntry>, no_entry_body: String) -> Toast {
        Toast {
            kind: kind.into(),
            title,
            body: entry.map_or(no_entry_body, |e| file_name(&e.file)),
            thumb: entry.and_then(|e| e.thumb.clone()),
            file: entry.map(|e| e.file.clone()),
            color: None,
        }
    }

    /// Runs one overlay action to its end. An error reaches the user as a toast.
    pub(crate) fn handle_overlay_result(self: &Arc<Self>, action: OverlayAction) -> Value {
        if let (Some(display_id), Some(rect)) = (action.display_id, action.rect_phys) {
            self.set_last_region(display_id, rect);
        }
        let kind = action.action;
        let label = action_label(kind);
        match self.apply_action(action) {
            Ok(file) => {
                if file.is_some() {
                    self.ports.ui.send_to_main(MainEvent::HistoryChanged);
                }
                self.set_last_processed(label, file.as_deref())
            }
            Err(e) => {
                self.diag.log(&format!("action {label} failed: {e}"));
                self.ports.ui.show_toast(text_toast(self.t("ocrFailTitle"), clip(&e.to_string(), 120)));
                self.set_last_processed(label, None)
            }
        }
    }

    /// Returns the history file the action produced.
    fn apply_action(self: &Arc<Self>, action: OverlayAction) -> Result<Option<PathBuf>> {
        let st = self.settings();
        let needs_png = || action.png.clone().ok_or_else(|| Error::msg("the overlay returned no image"));
        match action.action {
            OverlayActionKind::Copy => {
                let png = needs_png()?;
                let image = Frame::from_png(&png)?;
                let entry = if st.auto_save {
                    Some(self.ports.history.save_png(&png, &image, "region", None)?)
                } else {
                    None
                };
                self.copy_image(&image)?;
                if st.show_toast {
                    self.ports.ui.show_toast(self.entry_toast(
                        "image",
                        self.t("copiedTitle"),
                        entry.as_ref(),
                        self.t("noSave"),
                    ));
                }
                Ok(entry.map(|e| e.file))
            }
            OverlayActionKind::Claude => {
                let png = needs_png()?;
                let image = Frame::from_png(&png)?;
                let entry = self.ports.history.save_png(&png, &image, "region", None)?;
                self.ports.clipboard.write_image_and_text(&image, &entry.file.to_string_lossy())?;
                if st.show_toast {
                    self.ports.ui.show_toast(Toast {
                        kind: "claude".into(),
                        title: self.t("claudeTitle"),
                        body: self.t("claudeBody"),
                        thumb: entry.thumb.clone(),
                        file: Some(entry.file.clone()),
                        color: None,
                    });
                }
                Ok(Some(entry.file))
            }
            OverlayActionKind::Save => {
                let png = needs_png()?;
                let image = Frame::from_png(&png)?;
                let Some(path) = self.ports.dialogs.save_png(&st.save_dir.join("Shot.png"))? else {
                    return Ok(None);
                };
                fs::write(&path, &png).map_err(|e| Error::io("write screenshot", &path, e))?;
                self.copy_image(&image)?;
                if st.show_toast {
                    self.ports.ui.show_toast(Toast {
                        kind: "image".into(),
                        title: self.t("savedTitle"),
                        body: path.to_string_lossy().into_owned(),
                        thumb: None,
                        file: Some(path.clone()),
                        color: None,
                    });
                }
                Ok(Some(path))
            }
            OverlayActionKind::Pin => {
                let png = needs_png()?;
                let entry = if st.auto_save {
                    let image = Frame::from_png(&png)?;
                    Some(self.ports.history.save_png(&png, &image, "pin", None)?)
                } else {
                    None
                };
                let file = match &entry {
                    Some(e) => e.file.clone(),
                    None => {
                        let stamp = chrono::Utc::now().timestamp_millis();
                        let temp = self.env.temp_dir.join(format!("shotik-pin-{stamp}.png"));
                        fs::write(&temp, &png).map_err(|e| Error::io("write pin image", &temp, e))?;
                        temp
                    }
                };
                if let (Some(rect), Some(display_id)) = (action.rect_dip, action.display_id) {
                    let d = self.ports.displays.by_id(display_id);
                    self.ports.ui.show_pin(PinRequest {
                        file,
                        bounds: Rect::new(d.bounds.x + rect.x, d.bounds.y + rect.y, rect.width, rect.height),
                    });
                }
                Ok(entry.map(|e| e.file))
            }
            OverlayActionKind::Ocr => {
                let png = needs_png()?;
                let toast = match self.ports.ocr.recognize(&png, None) {
                    Ok(text) if !text.is_empty() => {
                        self.ports.clipboard.write_text(&text)?;
                        text_toast(self.t("ocrDoneTitle"), clip(&text, 120))
                    }
                    Ok(_) => text_toast(self.t("ocrTitle"), self.t("ocrNoText")),
                    Err(e) => text_toast(self.t("ocrFailTitle"), clip(&e.to_string(), 120)),
                };
                self.ports.ui.show_toast(toast);
                Ok(None)
            }
            OverlayActionKind::Copytext => {
                if let Some(text) = action.text.filter(|t| !t.is_empty()) {
                    self.ports.clipboard.write_text(&text)?;
                    if st.show_toast {
                        self.ports.ui.show_toast(text_toast(self.t("ocrDoneTitle"), clip(&text, 120)));
                    }
                }
                Ok(None)
            }
            OverlayActionKind::Record => {
                let (Some(display_id), Some(rect_phys), Some(rect_dip)) =
                    (action.display_id, action.rect_phys, action.rect_dip)
                else {
                    return Err(Error::msg("the overlay returned no region to record"));
                };
                self.start_recording(RecordTarget::Region { display_id, rect_phys, rect_dip })?;
                Ok(None)
            }
        }
    }

    pub(crate) fn set_last_region(&self, display_id: u64, rect_phys: Rect) {
        let patch = json!({ "lastRegion": { "displayId": display_id, "rectPhys": rect_phys } });
        if let Err(e) = self.ports.settings.set(patch) {
            self.diag.log(&format!("cannot remember the last region: {e}"));
        }
    }

    fn run_overlay_and_handle(self: &Arc<Self>, mode: OverlayMode) {
        match self.run_overlay(mode) {
            OverlayResult::Action(action) => {
                self.handle_overlay_result(action);
            }
            OverlayResult::Busy | OverlayResult::Cancelled => {}
        }
    }

    pub(crate) fn trigger_region(self: &Arc<Self>, hide_main: bool) {
        self.hide_main_first(hide_main);
        self.run_overlay_and_handle(OverlayMode::Normal);
    }

    /// Live Text: select an area, then extract or translate the text inside it.
    pub(crate) fn trigger_text(self: &Arc<Self>, hide_main: bool) {
        self.hide_main_first(hide_main);
        self.run_overlay_and_handle(OverlayMode::Text);
    }

    /// If already recording, stop; otherwise select an area and start.
    pub(crate) fn trigger_record(self: &Arc<Self>) {
        if self.is_recording() {
            self.stop_recording();
            return;
        }
        self.hide_main_first(true);
        self.run_overlay_and_handle(OverlayMode::Record);
    }

    /// Sound only (calls, voice notes): no region, just the controls bar.
    pub(crate) fn trigger_record_audio(self: &Arc<Self>) {
        if self.is_recording() {
            self.stop_recording();
            return;
        }
        let audio = self.settings().audio_recording;
        if !audio.system_audio && !audio.microphone {
            self.ports.ui.show_toast(text_toast(self.t("recFailTitle"), self.t("recAudioNone")));
            return;
        }
        if self.ports.ui.main_visible() {
            self.ports.ui.hide_main();
        }
        match self.start_recording(RecordTarget::AudioOnly) {
            Ok(_) => {
                self.set_last_processed("record-audio", None);
            }
            Err(e) => self.ports.ui.show_toast(text_toast(self.t("recFailTitle"), clip(&e.to_string(), 120))),
        }
    }

    pub(crate) fn trigger_full(self: &Arc<Self>) {
        if let Err(e) = self.full_screen_to_clipboard() {
            self.ports.ui.show_toast(text_toast(self.t("recFailTitle"), clip(&e.to_string(), 120)));
        }
    }

    fn full_screen_to_clipboard(self: &Arc<Self>) -> Result<()> {
        let st = self.settings();
        let display = self.ports.displays.under_cursor();
        let image = self.ports.capture.grab(&display)?;
        let png = image.to_png()?;
        let entry = if st.auto_save {
            Some(self.ports.history.save_png(&png, &image, "fullscreen", None)?)
        } else {
            None
        };
        self.copy_image(&image)?;
        if st.show_toast {
            self.ports.ui.show_toast(self.entry_toast("image", self.t("screenCopied"), entry.as_ref(), self.t("noSave")));
        }
        if entry.is_some() {
            self.ports.ui.send_to_main(MainEvent::HistoryChanged);
        }
        self.set_last_processed("fullscreen", entry.as_ref().map(|e| e.file.as_path()));
        Ok(())
    }

    pub(crate) fn trigger_repeat(self: &Arc<Self>) {
        if let Err(e) = self.repeat_last_region() {
            self.ports.ui.show_toast(text_toast(self.t("recFailTitle"), clip(&e.to_string(), 120)));
        }
    }

    fn repeat_last_region(self: &Arc<Self>) -> Result<()> {
        let st = self.settings();
        let no_region = || {
            let hotkey = if st.hotkeys.region.is_empty() { "PrtSc" } else { st.hotkeys.region.as_str() };
            self.ports.ui.show_toast(text_toast(self.t("noLastTitle"), self.t_args("noLastBody", &[hotkey])));
        };
        let Some(last) = st.last_region.clone() else {
            no_region();
            return Ok(());
        };
        let display = self.ports.displays.by_id(last.display_id);
        let screen = self.ports.capture.grab(&display)?;
        // The remembered region may no longer fit: the monitor changed, or was unplugged.
        let (w, h) = (screen.width() as i32, screen.height() as i32);
        let r = last.rect_phys;
        let rect = Rect::new(r.x.min(w - 1), r.y.min(h - 1), r.width.min(w - r.x), r.height.min(h - r.y));
        if rect.width < 1 || rect.height < 1 {
            no_region();
            return Ok(());
        }
        let image = screen.crop(rect)?;
        let png = image.to_png()?;
        let entry = if st.auto_save {
            Some(self.ports.history.save_png(&png, &image, "repeat", None)?)
        } else {
            None
        };
        self.copy_image(&image)?;
        if st.show_toast {
            self.ports.ui.show_toast(self.entry_toast("repeat", self.t("repeatTitle"), entry.as_ref(), self.t("copiedTitle")));
        }
        if entry.is_some() {
            self.ports.ui.send_to_main(MainEvent::HistoryChanged);
        }
        self.set_last_processed("repeat", entry.as_ref().map(|e| e.file.as_path()));
        Ok(())
    }

    pub(crate) fn translate_call(&self, text: &str, target: Option<&str>) -> Result<translate::Translation> {
        translate::translate(self.ports.http.as_ref(), &self.settings().translate, text, target, self.env.translate_stub)
    }

    pub(crate) fn recognize_for_selection(&self, png: &[u8]) -> Result<String> {
        ocr_select::recognize_for_selection(self.ports.ocr.as_ref(), png, self.env.platform)
    }
}

fn action_label(kind: OverlayActionKind) -> &'static str {
    match kind {
        OverlayActionKind::Copy => "copy",
        OverlayActionKind::Claude => "claude",
        OverlayActionKind::Save => "save",
        OverlayActionKind::Pin => "pin",
        OverlayActionKind::Ocr => "ocr",
        OverlayActionKind::Copytext => "copytext",
        OverlayActionKind::Record => "record",
    }
}
