//! Translating selected text: the hotkey, the background watcher, the bubble.
//! Reference: the auto-translate section of `src/main/main.js`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use shotik_core::highlight::{find_highlight_rect, HighlightOptions};
use shotik_core::ports::{SelectionEvent, TranslatePopup};
use shotik_core::{Display, Frame, Point, PointF, Rect, Result};

use crate::actions::text_toast;
use crate::App;

impl App {
    /// Translates the current selection anywhere on screen: copies it with a synthetic Ctrl+C, then
    /// translates and shows the popup. The user's clipboard is restored right away.
    pub(crate) fn trigger_translate_selection(self: &Arc<Self>) {
        let clipboard = &self.ports.clipboard;
        let test = self.env.test_mode;
        let saved_text = clipboard.read_text().unwrap_or_default();
        let saved_image = clipboard.read_image().unwrap_or_default();
        if !test {
            let _ = clipboard.clear();
            if let Err(e) = self.ports.selection.send_copy() {
                self.diag.log(&format!("synthetic copy failed: {e}"));
            }
        }
        // The application answers Ctrl+C in its own time. The clipboard was just cleared, so any text is the
        // answer, and with nothing selected the wait is all there is to it.
        let patience = Duration::from_millis(if test { 60 } else { 800 });
        let waiting = Instant::now();
        let text = loop {
            let text = clipboard.read_text().unwrap_or_default().trim().to_string();
            if !text.is_empty() || waiting.elapsed() >= patience {
                break text;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if !test {
            let restored = match &saved_image {
                Some(image) => clipboard.write_image(image),
                None if !saved_text.is_empty() => clipboard.write_text(&saved_text),
                None => clipboard.clear(),
            };
            if let Err(e) = restored {
                self.diag.log(&format!("cannot restore the clipboard: {e}"));
            }
        }
        if text.is_empty() {
            self.ports.ui.show_toast(text_toast(self.t("trNoSelTitle"), self.t("trNoSelBody")));
            return;
        }
        self.state().last_translated = Some(text.clone()); // the auto-bubble must not re-pop for it
        self.translate_to_popup(&text);
    }

    /// Shared by the hotkey (text via clipboard) and the bubble (text via UI Automation).
    pub(crate) fn translate_to_popup(self: &Arc<Self>, text: &str) {
        let target = self.settings().translate.target;
        self.diag.log(&format!("translate: start target={target}"));
        self.ports.ui.show_translate_popup(TranslatePopup {
            original: text.to_string(),
            loading: true,
            loading_text: Some(self.t("txtTranslating")),
            ..TranslatePopup::default()
        });
        match self.translate_call(text, None) {
            Ok(result) => {
                self.diag.log(&format!("translate: ok source={} provider={}", result.source, result.provider));
                self.ports.ui.update_translate_popup(TranslatePopup {
                    original: text.to_string(),
                    translated: Some(result.text),
                    source: Some(result.source),
                    target: Some(target),
                    ..TranslatePopup::default()
                });
                self.set_last_processed("translate-selection", None);
            }
            Err(e) => {
                self.diag.log(&format!("translate: FAIL {e}"));
                self.ports.ui.update_translate_popup(TranslatePopup {
                    original: text.to_string(),
                    error: Some(self.t("txtTranslateFail")),
                    ..TranslatePopup::default()
                });
            }
        }
    }

    pub(crate) fn start_selection_watcher(self: &Arc<Self>) {
        if self.env.test_mode || self.env.ghost {
            return; // tests drive this path over HTTP
        }
        if !self.settings().translate.auto_bubble {
            return;
        }
        let app = self.clone();
        if let Err(e) = self.ports.selection.start(Arc::new(move |event| app.on_selection_event(event))) {
            self.diag.log(&format!("selection watcher failed to start: {e}"));
        }
    }

    pub(crate) fn stop_selection_watcher(&self) {
        self.ports.ui.hide_selection_bubble();
        self.state().pending_selection = None;
        self.ports.selection.stop();
    }

    fn on_selection_event(self: &Arc<Self>, event: SelectionEvent) {
        match event {
            SelectionEvent::Selected { text, x, y } => self.on_selection_detected(&text, x, y),
            SelectionEvent::Cleared => self.clear_selection_bubble(),
            SelectionEvent::MouseDown { x, y } => self.on_mouse_down(Point { x, y }),
            SelectionEvent::Gesture { x1, y1, x2, y2, double } => {
                self.spawn(move |app| app.on_gesture(Point { x: x1, y: y1 }, Point { x: x2, y: y2 }, double));
            }
        }
    }

    /// Coordinates are physical pixels, as the watcher reports them.
    pub(crate) fn on_selection_detected(self: &Arc<Self>, text: &str, x: i32, y: i32) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.state().last_translated.as_deref() == Some(text) {
            self.diag.log("detect: same as lastTranslated, skip");
            return;
        }
        self.state().pending_selection = Some(text.to_string());
        let at = self.ports.displays.phys_to_dip(Point { x, y });
        self.diag.log(&format!("detect: {} ch -> bubble", text.chars().count()));
        self.ports.ui.show_selection_bubble(at);
    }

    /// Selection cleared: hide the bubble but KEEP the pending text, because a click racing with this
    /// clear must still have text to translate. Re-selecting the same text raises the bubble again.
    pub(crate) fn clear_selection_bubble(&self) {
        self.diag.log("clear: hide bubble (pendingSelection kept)");
        self.ports.ui.hide_selection_bubble();
        self.state().last_translated = None;
    }

    /// Click-away dismiss: a press outside the bubble or popup hides it. A press ON them is left alone,
    /// that window handles its own click. Robust where window blur proved flaky.
    pub(crate) fn on_mouse_down(&self, phys: Point) {
        self.on_pointer_down_dip(self.ports.displays.phys_to_dip(phys));
    }

    /// The same logic from a global DIP point; test routes start here (TEST-ROUTES.md, `bubble`).
    pub(crate) fn on_pointer_down_dip(&self, at: PointF) {
        let ui = &self.ports.ui;
        if ui.selection_bubble_visible() && !inside(ui.selection_bubble_bounds(), at) {
            ui.hide_selection_bubble();
            self.state().pending_selection = None;
        }
        if ui.translate_popup_visible() && !inside(ui.translate_popup_bounds(), at) {
            ui.hide_translate_popup();
        }
    }

    /// Apps that expose no UI Automation text (Telegram posts, canvases, images) still paint the
    /// selection on screen: read those pixels. No clipboard, no synthetic keys.
    pub(crate) fn on_gesture(self: &Arc<Self>, p1: Point, p2: Point, double: bool) {
        if !self.settings().translate.ocr_fallback {
            return;
        }
        {
            let mut state = self.state();
            if state.ocr_busy {
                return; // drags can arrive quickly: one OCR at a time
            }
            state.ocr_busy = true;
        }
        let displays = &self.ports.displays;
        let (a, b) = (displays.phys_to_dip(p1), displays.phys_to_dip(p2));
        let (rect, sample, bubble_at, disp) = if double {
            // A double-click selects a word: its highlight fits a small band around the point.
            let disp = displays.nearest(a);
            let (half_w, half_h) = (160.0, 24.0);
            (
                region(a.x - half_w - f64::from(disp.bounds.x), a.y - half_h - f64::from(disp.bounds.y), half_w * 2.0, half_h * 2.0),
                a,
                a,
                disp,
            )
        } else {
            let (x1, x2) = (a.x.min(b.x), a.x.max(b.x));
            let (y1, y2) = (a.y.min(b.y), a.y.max(b.y));
            let disp = displays.nearest(PointF { x: x1, y: y1 });
            let (pad_x, pad_y) = (30.0, 22.0); // generous, so the whole highlight is inside
            (
                region(x1 - pad_x - f64::from(disp.bounds.x), y1 - pad_y - f64::from(disp.bounds.y), x2 - x1 + pad_x * 2.0, y2 - y1 + pad_y * 2.0),
                PointF { x: (x1 + x2) / 2.0, y: (y1 + y2) / 2.0 },
                b,
                disp,
            )
        };
        let result = self.ocr_selection_region(&disp, rect, sample);
        self.state().ocr_busy = false;
        match result {
            Ok(text) => {
                self.diag.log(&format!("gesture -> {} ch", text.chars().count()));
                if text.is_empty() || self.state().last_translated.as_deref() == Some(text.as_str()) {
                    return;
                }
                self.state().pending_selection = Some(text);
                self.ports.ui.show_selection_bubble(bubble_at);
            }
            Err(e) => self.diag.log(&format!("gesture OCR failed: {e}")),
        }
    }

    /// Captures a generous region, finds the highlight the app painted behind the selected text, crops
    /// to it and reads it. No clear highlight means nothing was selected (a drag in a game, moving the
    /// mouse with the button held): no text. This is what stops false detections.
    fn ocr_selection_region(&self, disp: &Display, rect_dip: Rect, sample: PointF) -> Result<String> {
        if rect_dip.width < 6 || rect_dip.height < 6 {
            return Ok(String::new());
        }
        let scale = disp.scale_factor;
        let phys = Rect::new(
            (f64::from(rect_dip.x) * scale).round() as i32,
            (f64::from(rect_dip.y) * scale).round() as i32,
            (f64::from(rect_dip.width) * scale).round() as i32,
            (f64::from(rect_dip.height) * scale).round() as i32,
        );
        let image = self.ports.capture.grab(disp)?.crop(phys)?;
        let sx = ((sample.x - f64::from(disp.bounds.x) - f64::from(rect_dip.x)) * scale).round() as i32;
        let sy = ((sample.y - f64::from(disp.bounds.y) - f64::from(rect_dip.y)) * scale).round() as i32;
        let Some(highlight) = find_highlight_rect(image.bgra(), image.width(), image.height(), sx, sy, HighlightOptions::default()) else {
            self.diag.log("no highlight -> nothing selected, skip");
            return Ok(String::new());
        };
        let png = crop_with_padding(&image, highlight)?.to_png()?;
        Ok(self.recognize_for_selection(&png)?.trim().to_string())
    }

    /// The bubble was clicked: translate the selection it was raised for.
    pub(crate) fn bubble_activate(self: &Arc<Self>) {
        let pending = self.state().pending_selection.take();
        self.ports.ui.hide_selection_bubble();
        match pending {
            Some(text) => {
                self.diag.log(&format!("activate: translating {} ch", text.chars().count()));
                self.state().last_translated = Some(text.clone());
                self.spawn(move |app| app.translate_to_popup(&text));
            }
            None => self.diag.log("activate: NO pendingSelection -> nothing to translate"),
        }
    }
}

fn region(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect::new(x.max(0.0).round() as i32, y.max(0.0).round() as i32, width.round() as i32, height.round() as i32)
}

fn inside(bounds: Option<Rect>, at: PointF) -> bool {
    bounds.is_some_and(|b| b.contains(at))
}

/// The highlight with 3 px around it, clipped to the image.
pub(crate) fn crop_with_padding(image: &Frame, highlight: Rect) -> Result<Frame> {
    let pad = 3;
    let x = (highlight.x - pad).max(0);
    let y = (highlight.y - pad).max(0);
    image.crop(Rect::new(x, y, highlight.width + pad * 2, highlight.height + pad * 2))
}
