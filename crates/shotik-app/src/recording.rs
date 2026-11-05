//! A recording session seen from the application: the guard around a countdown, the windows, the
//! finished file. Capture and encoding live in `shotik-recorder`. Reference: `src/main/recorder.js` and
//! the recorder events at the end of `src/main/main.js`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use shotik_core::format::format_dur;
use shotik_core::ports::{MainEvent, RecStatus, RecWindows, RecordOptions, RecordTarget, RecorderEvent, RecordingDone, Toast};
use shotik_core::Result;

use crate::actions::{file_name, text_toast};
use crate::App;

impl App {
    pub(crate) fn init_recorder(self: &Arc<Self>) {
        let app = self.clone();
        self.ports.recorder.set_listener(Arc::new(move |event| app.on_recorder_event(event)));
    }

    /// True from the first line of a start until the file is finalized.
    pub(crate) fn is_recording(&self) -> bool {
        self.state().rec_starting.is_some() || self.ports.recorder.is_recording()
    }

    /// While a start is on its way (countdown), stopping calls it off.
    pub(crate) fn stop_recording(&self) {
        if let Some(pending) = &self.state().rec_starting {
            pending.store(true, Ordering::SeqCst);
            return;
        }
        self.ports.recorder.stop();
    }

    pub(crate) fn cancel_recording(&self) {
        if let Some(pending) = &self.state().rec_starting {
            pending.store(true, Ordering::SeqCst);
            return;
        }
        self.ports.recorder.cancel();
    }

    /// Ok(None) when a session already runs, or the start was called off during the countdown.
    pub(crate) fn start_recording(self: &Arc<Self>, target: RecordTarget) -> Result<Option<RecStatus>> {
        let pending = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.state();
            if state.rec_starting.is_some() || state.update.handing_over || self.ports.recorder.is_recording() {
                return Ok(None);
            }
            state.rec_starting = Some(pending.clone());
        }
        let result = self.begin_recording(target, &pending);
        self.state().rec_starting = None;
        // Stop or cancel can arrive while the recorder is starting, after the last look at `pending`.
        if pending.load(Ordering::SeqCst) && matches!(result, Ok(Some(_))) {
            self.ports.recorder.cancel();
            return Ok(None);
        }
        result
    }

    fn begin_recording(self: &Arc<Self>, target: RecordTarget, pending: &AtomicBool) -> Result<Option<RecStatus>> {
        let st = self.settings();
        let audio_only = matches!(target, RecordTarget::AudioOnly);
        // Inputs: video uses recording.*, audio mode its own toggles; the microphone device is shared.
        let (system_audio, microphone) = if audio_only {
            (st.audio_recording.system_audio, st.audio_recording.microphone)
        } else {
            (st.recording.system_audio, st.recording.microphone)
        };

        let synthetic = self.env.test_mode && !self.env.real_capture;
        if let RecordTarget::Region { display_id, rect_dip, .. } = &target {
            if st.recording.countdown && !self.env.ghost && !synthetic {
                let (done, finished) = mpsc::channel();
                self.ports.ui.show_countdown(
                    self.ports.displays.by_id(*display_id),
                    *rect_dip,
                    Box::new(move |()| {
                        let _ = done.send(());
                    }),
                );
                let _ = finished.recv_timeout(Duration::from_secs(5));
            }
        }
        if pending.load(Ordering::SeqCst) {
            return Ok(None);
        }

        let status = self.ports.recorder.start(
            target.clone(),
            RecordOptions {
                fps: st.recording.fps,
                format: st.recording.format.clone(),
                system_audio,
                microphone,
                microphone_device_id: st.recording.microphone_device_id.clone(),
                save_dir: st.save_dir.clone(),
            },
        )?;
        if status.is_some() && (!self.env.ghost || self.env.force_controls) {
            let mut meters = Vec::new();
            if system_audio {
                meters.push("system".to_string());
            }
            if microphone {
                meters.push("microphone".to_string());
            }
            let (display, rect_dip) = match target {
                RecordTarget::Region { display_id, rect_dip, .. } => (self.ports.displays.by_id(display_id), Some(rect_dip)),
                RecordTarget::AudioOnly => (self.ports.displays.primary(), None),
            };
            self.ports.ui.show_rec_windows(RecWindows { display, rect_dip, meters });
        }
        Ok(status)
    }

    pub(crate) fn on_recorder_event(self: &Arc<Self>, event: RecorderEvent) {
        match event {
            RecorderEvent::State(status) => {
                self.refresh_tray();
                if !status.recording {
                    self.ports.ui.close_rec_windows();
                } else {
                    self.ports.ui.update_rec_windows(status.paused.unwrap_or(false), status.levels.clone());
                }
                self.ports.ui.send_to_main(MainEvent::RecState(status));
            }
            RecorderEvent::Levels(levels) => {
                let paused = self.ports.recorder.status().paused.unwrap_or(false);
                self.ports.ui.update_rec_windows(paused, Some(levels));
            }
            RecorderEvent::Done(done) => self.on_recording_done(&done),
            RecorderEvent::Error(message) => {
                // A lost audio input is an error too, and the recording goes on: its controls must stay.
                if !self.ports.recorder.is_recording() {
                    self.ports.ui.close_rec_windows();
                }
                let body = self.recording_error_text(&message);
                self.ports.ui.show_toast(text_toast(self.t("recFailTitle"), body));
            }
        }
    }

    fn on_recording_done(self: &Arc<Self>, done: &RecordingDone) {
        let entry = match self.ports.history.add_recording(done) {
            Ok(entry) => entry,
            Err(e) => {
                // The file exists; history could not take it. The user must hear both.
                let body = format!("{}: {e}", done.file.display());
                self.ports.ui.show_toast(text_toast(self.t("recFailTitle"), body.chars().take(120).collect()));
                self.set_last_processed("record-done", Some(&done.file));
                return;
            }
        };
        self.ports.ui.send_to_main(MainEvent::HistoryChanged);
        if self.settings().show_toast {
            let audio = done.audio_only;
            let body = if done.duration_sec > 0.0 {
                let key = if audio { "recSavedAudioBody" } else { "recSavedBody" };
                self.t_args(key, &[&format_dur(done.duration_sec)])
            } else {
                file_name(&entry.file)
            };
            self.ports.ui.show_toast(Toast {
                kind: if audio { "audio" } else { "video" }.into(),
                title: self.t("recSavedTitle"),
                body,
                thumb: entry.thumb.clone(),
                file: Some(entry.file.clone()),
                color: None,
                action: None,
            });
        }
        self.set_last_processed("record-done", Some(&entry.file));
    }

    fn recording_error_text(&self, detail: &str) -> String {
        let known = if detail.starts_with("microphone:") {
            if detail.contains("NotAllowedError") || detail.contains("SecurityError") {
                Some("recMicrophoneDenied")
            } else if detail.contains("NotFoundError") || detail.contains("OverconstrainedError") {
                Some("recMicrophoneMissing")
            } else {
                Some("recMicrophoneFailed")
            }
        } else {
            match detail {
                "audio:none" => Some("recAudioNone"),
                "audio:system-unavailable" => Some("recSystemAudioUnavailable"),
                _ => None,
            }
        };
        known.map_or_else(|| detail.chars().take(120).collect(), |key| self.t(key))
    }
}
