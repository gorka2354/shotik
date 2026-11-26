//! The recorder only waits for the local queue write. One worker owns all slow share operations.

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, Weak};
use std::time::Duration;

use shotik_core::ports::Toast;
use shotik_core::recording_copy::{deliver, CopyQueue, Delivery, RETRY_DELAY};
use shotik_core::{Error, Result};

use crate::actions::{file_name, text_toast};
use crate::App;

#[derive(Default)]
struct Pending {
    queue: CopyQueue,
    loaded: bool,
    dirty: bool,
    /// Only the first attempt adds to the ordinary saved toast. A restart is always a retry.
    first: HashMap<PathBuf, Option<Toast>>,
}

impl Pending {
    fn sync(&mut self, path: &Path) -> Result<()> {
        if !self.loaded {
            // A previous read may have failed. Keep any calls saved in the meantime.
            let mut stored = CopyQueue::load(path)?;
            for file in self.queue.pending() {
                stored.enqueue(file);
            }
            self.queue = stored;
            self.loaded = true;
        }
        if self.dirty {
            self.queue.save(path)?;
            self.dirty = false;
        }
        Ok(())
    }

    /// Commit removal before announcing a retry. On a local write failure the entry stays queued.
    fn finish(&mut self, path: &Path, file: &Path, result: &io::Result<Delivery>) -> Result<bool> {
        if result.is_err() || !self.loaded {
            return Ok(false);
        }
        let mut next = self.queue.clone();
        next.finish(file, result);
        next.save(path)?;
        self.queue = next;
        self.dirty = false;
        Ok(true)
    }
}

pub(super) struct RecordingCopies {
    path: PathBuf,
    pending: Mutex<Pending>,
    wakeup: mpsc::SyncSender<()>,
}

impl RecordingCopies {
    pub(super) fn new(dir: &Path) -> (Self, mpsc::Receiver<()>) {
        let (wakeup, receiver) = mpsc::sync_channel(1);
        (
            Self {
                path: dir.join("recording-copies.json"),
                pending: Mutex::default(),
                wakeup,
            },
            receiver,
        )
    }

    pub(super) fn wake(&self) {
        // A full channel already promises another pass, including recordings saved during this one.
        if let Err(mpsc::TrySendError::Disconnected(_)) = self.wakeup.try_send(()) {
            eprintln!("recording copy worker has stopped");
        }
    }
}

pub(super) fn start_worker(app: Weak<App>, wakeups: mpsc::Receiver<()>) -> Result<()> {
    std::thread::Builder::new()
        .name("shotik-recording-copy".into())
        .spawn(move || {
            let mut delay = None;
            while match delay {
                Some(d) => !matches!(wakeups.recv_timeout(d), Err(mpsc::RecvTimeoutError::Disconnected)),
                None => wakeups.recv().is_ok(),
            } {
                let Some(app) = app.upgrade() else { break };
                delay = app.send_pending_recordings();
            }
        })
        .map_err(|e| Error::msg(format!("cannot start the recording copy worker: {e}")))?;
    Ok(())
}

impl App {
    pub(super) fn queue_audio_copy(&self, file: &Path, toast: Option<Toast>) {
        let mut pending = self.copies.pending.lock().unwrap();
        pending.queue.enqueue(file);
        pending.first.insert(file.to_owned(), toast);
        pending.dirty = true;
        if let Err(e) = pending.sync(&self.copies.path) {
            self.diag.log(&e.to_string());
        }
        drop(pending);
        self.copies.wake();
    }

    fn send_pending_recordings(&self) -> Option<Duration> {
        let folder = self.settings().audio_recording.copy_dir;
        let mut pending = self.copies.pending.lock().unwrap();
        if folder.as_os_str().is_empty() {
            let first = std::mem::take(&mut pending.first);
            drop(pending);
            for mut toast in first.into_values().flatten() {
                toast.body.push_str(&format!("\n{}", self.t("audioCopyPaused")));
                self.ports.ui.show_toast(toast);
            }
            return None;
        }
        let storage_error = pending.sync(&self.copies.path).err();
        if let Some(e) = &storage_error {
            self.diag.log(&e.to_string());
        }
        let files = pending.queue.pending().to_vec();
        drop(pending);

        let mut recovered = Vec::new();
        for file in files.iter().rev() {
            if self.settings().audio_recording.copy_dir != folder {
                // A settings change wakes the worker for a fresh pass.
                break;
            }
            if self.send_recording(file, &folder, storage_error.is_none()) {
                recovered.push(file);
            }
        }
        if !recovered.is_empty() && self.settings().show_toast {
            let title = self.t_args("audioCopyRetried", &[&recovered.len().to_string()]);
            let sent = self.t_args("audioCopySent", &[&folder.to_string_lossy()]);
            let (body, file) = match recovered.as_slice() {
                [only] => (format!("{}\n{sent}", file_name(only)), Some((*only).clone())),
                _ => (sent, None),
            };
            self.ports.ui.show_toast(Toast {
                kind: "audio".into(),
                file,
                ..text_toast(title, body)
            });
        }
        let pending = self.copies.pending.lock().unwrap();
        pending.queue.retry_after().or(storage_error.map(|_| RETRY_DELAY))
    }

    /// True when an earlier attempt's recording was delivered and durably removed from the queue.
    fn send_recording(&self, file: &Path, folder: &Path, queue_saved: bool) -> bool {
        let result = deliver(self.ports.recording_copy.as_ref(), file, folder);
        let mut pending = self.copies.pending.lock().unwrap();
        let first = pending.first.remove(file);
        let removed = pending.finish(&self.copies.path, file, &result).unwrap_or_else(|e| {
            self.diag.log(&e.to_string());
            false
        });
        drop(pending);
        if matches!(result, Ok(Delivery::SourceGone)) && removed {
            self.diag.log(&format!(
                "recording copy: local file missing, removed {}",
                file.display()
            ));
        }
        match first {
            Some(Some(mut toast)) => {
                let status = match &result {
                    Ok(Delivery::Sent) => self.t_args("audioCopySent", &[&folder.to_string_lossy()]),
                    Ok(Delivery::SourceGone) => self.t("audioCopyGone"),
                    Err(e) => self.t_args("audioCopyFailed", &[&self.copy_error_text(e)]),
                };
                toast.body.push_str(&format!("\n{status}"));
                if result.is_err() && !queue_saved {
                    toast.body.push_str(&format!("\n{}", self.t("audioCopyQueueUnsaved")));
                }
                self.ports.ui.show_toast(toast);
                false
            }
            Some(None) => false,
            None => removed && matches!(result, Ok(Delivery::Sent)),
        }
    }

    fn copy_error_text(&self, error: &io::Error) -> String {
        let key = match error.kind() {
            ErrorKind::NotFound
            | ErrorKind::NotADirectory
            | ErrorKind::NetworkUnreachable
            | ErrorKind::HostUnreachable
            | ErrorKind::NotConnected
            | ErrorKind::TimedOut
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted => "audioCopyUnavailable",
            ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => "audioCopyDenied",
            ErrorKind::StorageFull | ErrorKind::QuotaExceeded => "audioCopyFull",
            ErrorKind::ResourceBusy => "audioCopyBusy",
            _ => return error.to_string(),
        };
        self.t(key)
    }
}
