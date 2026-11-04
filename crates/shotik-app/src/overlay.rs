//! An overlay session: the half of `startOverlaySession` in `src/main/capture.js` that is not a window.

use std::sync::{mpsc, Arc};
use std::time::Instant;

use shotik_core::ports::{MarkSink, OverlayAction, OverlayMode, OverlayOutcome, Toast};
use shotik_core::Rect;

use crate::App;

pub(crate) enum OverlayResult {
    /// Another session is open; the caller does nothing.
    Busy,
    Cancelled,
    Action(OverlayAction),
}

impl App {
    /// Opens the overlay and waits for the user. Blocks: run on a blocking thread.
    pub(crate) fn run_overlay(self: &Arc<Self>, mode: OverlayMode) -> OverlayResult {
        let ui = &self.ports.ui;
        if ui.overlay_is_open() {
            return OverlayResult::Busy;
        }
        let started = Instant::now();
        let (done_tx, done_rx) = mpsc::channel();

        let app = self.clone();
        let marks: MarkSink = Arc::new(move |label| app.overlay_mark(label, started));
        let session = {
            let mut state = self.state();
            state.overlay_timing.clear();
            state.overlay_session += 1;
            state.overlay_session
        };
        if let Err(e) = ui.overlay_begin(
            mode,
            marks.clone(),
            Box::new(move |outcome| {
                let _ = done_tx.send(outcome);
            }),
        ) {
            self.capture_failed(&e.to_string());
            return OverlayResult::Cancelled;
        }

        // Hover-snap rectangles arrive when they arrive: the overlay must not wait for them.
        self.spawn(|app| app.ports.ui.overlay_set_window_rects(app.snap_rects()));

        // The display under the cursor first, shown at once; the others follow in the background.
        let cursor = self.ports.displays.under_cursor();
        match self.ports.capture.grab(&cursor) {
            Ok(frame) => {
                marks("grab");
                if let Err(e) = ui.overlay_add_display(cursor.clone(), frame, true) {
                    ui.overlay_cancel();
                    self.capture_failed(&e.to_string());
                }
            }
            Err(e) => {
                ui.overlay_cancel();
                self.capture_failed(&e.to_string());
            }
        }
        for display in self.ports.displays.all().into_iter().filter(|d| d.id != cursor.id) {
            self.spawn(move |app| match app.ports.capture.grab(&display) {
                // The session this frame was taken for was cancelled and another one is open.
                Ok(_) if app.state().overlay_session != session => {}
                Ok(frame) => {
                    if let Err(e) = app.ports.ui.overlay_add_display(display, frame, false) {
                        app.diag.log(&format!("overlay display failed: {e}"));
                    }
                }
                // The session goes on with the monitors that worked, but this must be visible somewhere.
                Err(e) => app.diag.log(&format!("capture of display {} failed: {e}", display.id)),
            });
        }

        match done_rx.recv() {
            Ok(OverlayOutcome::Done(action)) => OverlayResult::Action(action),
            Ok(OverlayOutcome::Cancelled) | Err(_) => OverlayResult::Cancelled,
        }
    }

    fn overlay_mark(&self, label: &'static str, started: Instant) {
        let mark = format!("{label}={}", started.elapsed().as_millis());
        let mut state = self.state();
        state.overlay_timing.push(mark);
        if label == "show" {
            self.diag.write_timing(&state.overlay_timing);
        }
    }

    fn capture_failed(&self, message: &str) {
        self.diag.log(&format!("capture failed: {message}"));
        self.ports.ui.show_toast(Toast {
            kind: "text".into(),
            title: self.t("recFailTitle"),
            body: message.chars().take(120).collect(),
            thumb: None,
            file: None,
            color: None,
        });
    }

    /// Visible top-level windows in physical pixels, topmost first, without Shotik's own.
    pub(crate) fn snap_rects(&self) -> Vec<Rect> {
        if self.env.fake_screen {
            return shotik_fake::fixture_snap_rects(&self.ports.displays.primary());
        }
        match self.ports.windows.list(Some(std::process::id())) {
            Ok(windows) => windows.into_iter().filter(|w| !w.minimized).filter_map(|w| w.bounds).collect(),
            Err(e) => {
                // Snapping is a convenience: selecting by hand still works.
                self.diag.log(&format!("window list for hover-snap failed: {e}"));
                Vec::new()
            }
        }
    }
}
