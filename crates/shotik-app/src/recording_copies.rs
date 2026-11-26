//! The recorder registers the finished call locally. One worker owns all target folder operations.

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, Weak};
use std::time::{Duration, Instant};

use shotik_core::ports::Toast;
use shotik_core::recording_copy::{
    deliver_queued, load_receipt, save_receipt, Content, CopyQueue, Delivery, Recording, RETRY_DELAY,
};
use shotik_core::{Error, Result};

use crate::actions::{file_name, text_toast};
use crate::{App, Diagnostics};

// An enqueue that exists only in memory must not wait for the five-minute delivery timer.
const ENQUEUE_SAVE_RETRY: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Pending {
    queue: CopyQueue,
    loaded: bool,
    dirty: bool,
    retry_enqueue_at: Option<Instant>,
    /// Only the first attempt adds to the ordinary saved toast. A restart is always a retry.
    first: HashMap<Recording, Option<Toast>>,
    has_receipt: bool,
}

impl Pending {
    fn sync(&mut self, path: &Path, diag: &Diagnostics) -> Result<()> {
        let receipt_path = path.with_extension("receipt");
        if !self.loaded {
            let mut stored = CopyQueue::load_with_log(path, |message| diag.log(message))?;
            if let Some(sent) = load_receipt(&receipt_path)? {
                stored.acknowledge(&sent);
                self.has_receipt = true;
                self.dirty = true;
            }
            for entry in self.queue.pending() {
                stored.enqueue(entry.recording.clone());
            }
            self.queue = stored;
            self.loaded = true;
        }
        if self.dirty {
            self.queue.save(path)?;
            self.dirty = false;
            self.retry_enqueue_at = None;
        }
        if self.has_receipt {
            match std::fs::remove_file(&receipt_path) {
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io("remove recording copy receipt", receipt_path, e)),
                Ok(()) => {}
            }
            self.has_receipt = false;
        }
        Ok(())
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
    pub(super) fn flush_unsaved_enqueue(&self) {
        let Ok(mut pending) = self.copies.pending.lock() else {
            self.diag
                .log("cannot flush recording copy queue: its mutex was poisoned");
            return;
        };
        if pending.retry_enqueue_at.is_some() {
            if let Err(e) = pending.sync(&self.copies.path, &self.diag) {
                self.diag.log(&e.to_string());
            }
        }
    }

    pub(super) fn queue_audio_copy(&self, file: &Path, content: Option<Content>, toast: Option<Toast>) {
        let recording = Recording::new(file, content);
        let mut pending = self.copies.pending.lock().unwrap();
        pending.queue.enqueue(recording.clone());
        pending.first.insert(recording, toast);
        pending.dirty = true;
        if let Err(e) = pending.sync(&self.copies.path, &self.diag) {
            self.diag.log(&e.to_string());
            if pending.dirty {
                pending
                    .retry_enqueue_at
                    .get_or_insert_with(|| Instant::now() + ENQUEUE_SAVE_RETRY);
            }
        }
        drop(pending);
        self.copies.wake();
    }

    fn send_pending_recordings(&self) -> Option<Duration> {
        let folder = self.settings().audio_recording.copy_dir;
        let paused = folder.as_os_str().is_empty();
        let mut pending = self.copies.pending.lock().unwrap();
        if let Some(delay) = pending
            .retry_enqueue_at
            .and_then(|at| at.checked_duration_since(Instant::now()))
        {
            return Some(delay);
        }
        let storage_error = if !paused || pending.retry_enqueue_at.is_some() {
            pending.sync(&self.copies.path, &self.diag).err()
        } else {
            None
        };
        if let Some(e) = &storage_error {
            if pending.retry_enqueue_at.is_some() {
                // Keep the whole queue in memory and try its local save again, even when copying is paused.
                pending.retry_enqueue_at = Some(Instant::now() + ENQUEUE_SAVE_RETRY);
                let first = std::mem::take(&mut pending.first);
                drop(pending);
                if !first.is_empty() {
                    self.diag.log(&e.to_string());
                    let status = if paused {
                        self.t("audioCopyPaused")
                    } else {
                        self.t_args("audioCopyFailed", &[&e.to_string()])
                    };
                    let warning = self.t("audioCopyQueueUnsaved");
                    for mut toast in first.into_values().flatten() {
                        toast.body.push_str(&format!("\n{status}\n{warning}"));
                        self.ports.ui.show_toast(toast);
                    }
                }
                return Some(ENQUEUE_SAVE_RETRY);
            }
            self.diag.log(&e.to_string());
        }
        if paused {
            let first = std::mem::take(&mut pending.first);
            drop(pending);
            for mut toast in first.into_values().flatten() {
                toast.body.push_str(&format!("\n{}", self.t("audioCopyPaused")));
                self.ports.ui.show_toast(toast);
            }
            return None;
        }
        let files: Vec<_> = pending.queue.pending().iter().map(|e| e.recording.clone()).collect();
        drop(pending);

        let mut recovered = Vec::new();
        let mut sent_folders = Vec::new();
        for file in files.iter().rev() {
            if self.settings().audio_recording.copy_dir != folder {
                // A settings change wakes the worker for a fresh pass.
                break;
            }
            if let Some(sent_to) = self.send_recording(file, &folder) {
                recovered.push(&file.source);
                if !sent_folders.contains(&sent_to) {
                    sent_folders.push(sent_to);
                }
            }
        }
        if !recovered.is_empty() && self.settings().show_toast {
            let title = self.t_args("audioCopyRetried", &[&recovered.len().to_string()]);
            let sent = sent_folders
                .iter()
                .map(|dir| self.t_args("audioCopySent", &[&dir.to_string_lossy()]))
                .collect::<Vec<_>>()
                .join("\n");
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
        pending
            .queue
            .retry_after()
            .or_else(|| (pending.dirty || pending.has_receipt || storage_error.is_some()).then_some(RETRY_DELAY))
    }

    fn send_recording(&self, recording: &Recording, folder: &Path) -> Option<PathBuf> {
        let port = self.ports.recording_copy.as_ref();
        let path = &self.copies.path;
        let file = &recording.source;
        let mut key = recording.clone();
        let mut queue_saved = true;
        let mut sync = |pending: &mut Pending| {
            pending.sync(path, &self.diag).map_err(|e| {
                queue_saved = false;
                self.diag.log(&e.to_string());
                io::Error::other(e)
            })
        };
        // A previous delivery may still need its local acknowledgement flushed. Do not
        // start a second publication until that succeeds.
        let mut pending = self.copies.pending.lock().unwrap();
        let ready = sync(&mut pending);
        let mut entry = pending
            .queue
            .pending()
            .iter()
            .find(|e| &e.recording == recording)?
            .clone();
        let reserved = pending.queue.reserved_by_others(recording);
        drop(pending);
        let result = ready.and_then(|()| {
            deliver_queued(port, &mut entry, folder, &reserved, |next| {
                let mut pending = self.copies.pending.lock().unwrap();
                pending.queue.update(&key, next)?;
                key = next.recording.clone();
                pending.dirty = true;
                sync(&mut pending)
            })
        });

        let mut pending = self.copies.pending.lock().unwrap();
        let first = pending.first.remove(recording);
        if result.is_ok() {
            // The receiver may move the final file immediately. Write its receipt before
            // attempting queue replacement, and retire it in memory even if storage fails.
            if matches!(result, Ok(Delivery::Sent)) {
                match save_receipt(&path.with_extension("receipt"), &entry) {
                    Ok(()) => pending.has_receipt = true,
                    Err(e) => self.diag.log(&e.to_string()),
                }
            }
            pending.queue.finish(&key);
            pending.dirty = true;
            if let Err(e) = pending.sync(path, &self.diag) {
                self.diag.log(&e.to_string());
            }
        }
        drop(pending);
        if matches!(result, Ok(Delivery::SourceGone)) {
            self.diag.log(&format!(
                "recording copy: local file missing, removed {}",
                file.display()
            ));
        }
        if let Err(e) = &result {
            self.diag
                .log(&format!("recording copy pending {}: {e}", file.display()));
        }
        let sent_to = entry.target().and_then(Path::parent).unwrap_or(folder);
        match first {
            Some(Some(mut toast)) => {
                let status = match &result {
                    Ok(Delivery::Sent) => self.t_args("audioCopySent", &[&sent_to.to_string_lossy()]),
                    Ok(Delivery::SourceGone) => self.t("audioCopyGone"),
                    Err(e) => self.t_args("audioCopyFailed", &[&self.copy_error_text(e)]),
                };
                toast.body.push_str(&format!("\n{status}"));
                if result.is_err() && !queue_saved {
                    toast.body.push_str(&format!("\n{}", self.t("audioCopyQueueUnsaved")));
                }
                self.ports.ui.show_toast(toast);
                None
            }
            Some(None) => None,
            None => matches!(result, Ok(Delivery::Sent)).then(|| sent_to.to_owned()),
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
            ErrorKind::ResourceBusy | ErrorKind::InvalidData => "audioCopyBusy",
            _ => return error.to_string(),
        };
        self.t(key)
    }
}
