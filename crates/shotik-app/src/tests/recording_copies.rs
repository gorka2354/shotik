//! Exercise the recorder, worker and on-disk recovery together with synthetic files.

use super::*;
use shotik_core::recording_copy::{part_path, Content, CopyQueue};
use std::{fs, io};

type FingerprintHook = Box<dyn FnOnce() -> io::Result<()> + Send>;

#[derive(Default)]
struct Files {
    before_fingerprint: Mutex<Option<FingerprintHook>>,
    after_publish: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    after_stage: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    after_absence: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl RecordingCopy for Files {
    fn file_size(&self, path: &Path) -> io::Result<u64> {
        fs::metadata(path).map(|m| m.len())
    }

    fn publish(&self, source: &Path, destination: &Path) -> io::Result<()> {
        let content = self.fingerprint(source)?;
        self.stage(source, &part_path(destination), content)?
            .publish(destination)
    }

    fn fingerprint(&self, path: &Path) -> io::Result<Content> {
        if let Some(hook) = self.before_fingerprint.lock().unwrap().take() {
            hook()?;
        }
        Content::read(fs::File::open(path)?)
    }

    fn source_absent(&self, path: &Path) -> io::Result<bool> {
        let absent = shotik_core::recording_copy::absent_from_parent(path)?;
        if absent {
            if let Some(hook) = self.after_absence.lock().unwrap().take() {
                hook();
            }
        }
        Ok(absent)
    }

    fn stage(&self, source: &Path, path: &Path, content: Content) -> io::Result<Box<dyn RecordingPart + '_>> {
        let bytes = fs::read(source)?;
        Content::read(bytes.as_slice())?.verify(content)?;
        fs::write(path, bytes)?;
        if let Some(hook) = self.after_stage.lock().unwrap().take() {
            hook();
        }
        self.reopen_part(path, content)
    }

    fn reopen_part(&self, path: &Path, content: Content) -> io::Result<Box<dyn RecordingPart + '_>> {
        self.fingerprint(path)?.verify(content)?;
        Ok(Box::new(Part {
            files: self,
            path: path.to_owned(),
        }))
    }
}

struct Part<'a> {
    files: &'a Files,
    path: PathBuf,
}

impl RecordingPart for Part<'_> {
    fn publish(&self, destination: &Path) -> io::Result<()> {
        // No replacing an existing file, just as for the Windows rename.
        fs::hard_link(&self.path, destination)?;
        self.discard()?;
        if let Some(hook) = self.files.after_publish.lock().unwrap().take() {
            hook();
        }
        Ok(())
    }

    fn discard(&self) -> io::Result<()> {
        fs::remove_file(&self.path)
    }
}

struct Calls {
    rig: Rig,
    files: Arc<Files>,
    target: PathBuf,
    queue: PathBuf,
}

impl Calls {
    fn new(name: &str) -> Self {
        let mut rig = rig(name);
        let files = Arc::new(Files::default());
        let mut ports = rig.app.ports.clone();
        ports.recording_copy = files.clone();
        rig.app = App::new(ports, rig.app.env.clone(), rig.app.rt.clone()).unwrap();
        let target = rig.dir.join("calls");
        let queue = rig.dir.join("recording-copies.json");
        rig.app
            .ports
            .settings
            .set(json!({
                "audioRecording": {"copyDir": target}, "language": "en", "mcp": {"enabled": false}
            }))
            .unwrap();
        fs::create_dir(rig.dir.join("local")).unwrap();
        Self {
            rig,
            files,
            target,
            queue,
        }
    }

    fn finish(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let file = self.rig.dir.join("local").join(name);
        fs::write(&file, bytes).unwrap();
        self.rig.app.on_recorder_event(RecorderEvent::Done(RecordingDone {
            audio_content: None,
            file: file.clone(),
            audio_only: name.ends_with(".m4a"),
            first_frame_jpeg: None,
            width: 0,
            height: 0,
            duration_sec: 1.0,
        }));
        file
    }

    fn wait_failures(&self, count: usize) {
        assert!(eventually(|| self
            .rig
            .ui
            .toasts
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.body.contains("Will retry."))
            .count()
            == count));
    }

    fn wait_toast(&self, text: &str) {
        assert!(
            eventually(|| self
                .rig
                .ui
                .toasts
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.title.contains(text) || t.body.contains(text))),
            "missing toast {text}"
        );
    }

    fn waiting(&self) -> usize {
        CopyQueue::load(&self.queue).unwrap().pending().len()
    }

    fn restart(&mut self) {
        let previous = Arc::downgrade(&self.rig.app);
        self.rig.app = App::new(
            self.rig.app.ports.clone(),
            self.rig.app.env.clone(),
            self.rig.app.rt.clone(),
        )
        .unwrap();
        assert!(eventually(|| previous.upgrade().is_none()));
        self.rig.app.start(&["--hidden".into()]);
    }
}

#[test]
fn two_same_size_calls_with_overlapping_names_both_arrive() {
    let c = Calls::new("mirror-suffix-collision");
    c.finish("Rec.m4a", b"AAA");
    c.wait_failures(1);
    c.finish("Rec-2.m4a", b"BBB");
    c.wait_failures(2);
    fs::create_dir(&c.target).unwrap();
    fs::write(c.target.join("Rec.m4a"), b"existing call").unwrap();
    c.finish("wake.mp4", b"video");
    c.wait_toast("Sent after retry: 2");
    assert_eq!(c.waiting(), 0);
    assert_eq!(fs::read(c.target.join("Rec.m4a")).unwrap(), b"existing call");
    assert_eq!(fs::read(c.target.join("Rec-2.m4a")).unwrap(), b"BBB");
    assert_eq!(fs::read(c.target.join("Rec-3.m4a")).unwrap(), b"AAA");
}

#[test]
fn a_missing_source_folder_recovers_and_only_an_absent_file_is_logged() {
    let mut c = Calls::new("mirror-source-volume");
    let volume = c.rig.dir.join("local/volume");
    let detached = c.rig.dir.join("detached");
    fs::create_dir(&volume).unwrap();
    let gone = c.finish("gone.m4a", b"gone");
    c.wait_failures(1);
    c.finish("volume/kept.m4a", b"kept");
    c.wait_failures(2);
    fs::rename(&volume, &detached).unwrap();
    fs::remove_file(gone).unwrap();
    fs::create_dir(&c.target).unwrap();
    c.restart();
    let log = c.rig.dir.join("shotik-bubble.log");
    assert!(eventually(|| fs::read_to_string(&log)
        .unwrap()
        .lines()
        .any(|s| s.contains("local file missing") && s.contains("gone.m4a"))));
    assert_eq!(
        c.waiting(),
        1,
        "only the missing file in the readable folder was removed"
    );
    fs::rename(&detached, &volume).unwrap();
    c.finish("wake.mp4", b"video");
    c.wait_toast("Sent after retry: 1");
    assert_eq!(c.waiting(), 0);
    assert_eq!(fs::read(c.target.join("kept.m4a")).unwrap(), b"kept");
    assert!(!fs::read_to_string(log)
        .unwrap()
        .lines()
        .any(|s| s.contains("local file missing") && s.contains("kept.m4a")));
}

#[test]
fn two_calls_saved_at_the_same_path_keep_their_own_queue_entries() {
    let c = Calls::new("mirror-path-reuse");
    let original = c.finish("Rec.m4a", b"old");
    c.wait_failures(1);
    c.finish("Rec.m4a", b"new");
    c.wait_failures(2);
    assert_eq!(c.waiting(), 2);
    fs::create_dir(&c.target).unwrap();
    c.finish("wake.mp4", b"video");
    c.wait_toast("Sent after retry: 1");
    assert_eq!(c.waiting(), 1);
    assert_eq!(fs::read(c.target.join("Rec.m4a")).unwrap(), b"new");
    fs::write(original, b"old").unwrap();
    c.finish("again.mp4", b"video");
    assert!(eventually(|| c.waiting() == 0));
    assert_eq!(fs::read(c.target.join("Rec-2.m4a")).unwrap(), b"old");
}

#[test]
fn a_replacement_is_never_published_for_an_older_recording() {
    let mut c = Calls::new("mirror-replaced-source");
    c.finish("witness.m4a", b"wake");
    c.wait_failures(1);
    let original = c.finish("Rec.m4a", b"old");
    c.wait_failures(2);
    fs::write(original, b"new").unwrap();
    fs::create_dir(&c.target).unwrap();
    c.restart();
    c.wait_toast("Sent after retry");
    assert!(!c.target.join("Rec.m4a").exists());
    assert_eq!(c.waiting(), 1);
}

#[test]
fn an_in_flight_absence_cannot_remove_the_next_call_at_that_path() {
    use std::sync::mpsc::sync_channel;

    let c = Calls::new("mirror-in-flight-reuse");
    let old = c.finish("Rec.m4a", b"old");
    c.wait_failures(1);
    fs::rename(old, c.rig.dir.join("archived.m4a")).unwrap();
    fs::create_dir(&c.target).unwrap();
    let (checked, wait_checked) = sync_channel(1);
    let (resume, wait_resume) = sync_channel(1);
    *c.files.after_absence.lock().unwrap() = Some(Box::new(move || {
        checked.send(()).unwrap();
        wait_resume.recv_timeout(Duration::from_secs(10)).unwrap();
    }));
    c.finish("wake.mp4", b"video");
    wait_checked.recv_timeout(Duration::from_secs(10)).unwrap();
    c.finish("Rec.m4a", b"new");
    resume.send(()).unwrap();
    c.wait_toast("Sent to ");
    assert_eq!(fs::read(c.target.join("Rec.m4a")).unwrap(), b"new");
    assert_eq!(c.waiting(), 0);
}

#[test]
fn a_failed_save_of_the_chosen_name_keeps_the_final_file_hidden() {
    let mut c = Calls::new("mirror-prepare-failure");
    fs::create_dir(&c.target).unwrap();
    let saved = c.rig.dir.join("before-stage.json");
    let previous = saved.clone();
    let queue = c.queue.clone();
    *c.files.after_stage.lock().unwrap() = Some(Box::new(move || {
        fs::rename(&queue, previous).unwrap();
        fs::create_dir(queue).unwrap();
    }));
    c.finish("Rec.m4a", b"verified bytes");
    c.wait_toast("retry queue could not be saved to disk");
    assert!(!c.target.join("Rec.m4a").exists());
    fs::remove_dir(&c.queue).unwrap();
    fs::rename(saved, &c.queue).unwrap();
    c.restart();
    c.wait_toast("Sent after retry: 1");
    assert_eq!(fs::read(c.target.join("Rec.m4a")).unwrap(), b"verified bytes");
    assert_eq!(fs::read_dir(&c.target).unwrap().count(), 1);
}

fn consumed_after_failed_queue_save(name: &str, restart: bool) {
    let mut c = Calls::new(name);
    fs::create_dir(&c.target).unwrap();
    let saved = c.rig.dir.join("locked-queue.json");
    let queue = c.queue.clone();
    let locked = saved.clone();
    *c.files.after_publish.lock().unwrap() = Some(Box::new(move || {
        fs::rename(&queue, locked).unwrap();
        fs::create_dir(queue).unwrap(); // Refuse the atomic replacement of the queue.
    }));
    c.finish("Rec.m4a", b"processed once");
    c.wait_toast("Sent to ");
    let dest = c.target.join("Rec.m4a");
    fs::rename(&dest, c.rig.dir.join("archived.m4a")).unwrap();
    fs::remove_dir(&c.queue).unwrap();
    fs::rename(saved, &c.queue).unwrap();
    if restart {
        c.restart();
    } else {
        c.finish("wake.mp4", b"video");
    }
    assert!(eventually(|| c.waiting() == 0));
    assert!(!dest.exists(), "the recipient already archived this recording");
}

#[test]
fn a_failed_queue_save_does_not_resend_a_consumed_call() {
    consumed_after_failed_queue_save("mirror-ack-write", false);
}

#[test]
fn a_failed_queue_save_does_not_resend_a_consumed_call_after_restart() {
    consumed_after_failed_queue_save("mirror-ack-restart", true);
}

#[test]
fn restart_remembers_the_published_suffix_when_a_lower_name_is_freed() {
    let mut c = Calls::new("mirror-pinned-name");
    fs::create_dir(&c.target).unwrap();
    fs::write(c.target.join("Rec.m4a"), b"occupied").unwrap();
    let lower = c.target.join("Rec-2.m4a");
    fs::write(&lower, b"other").unwrap();
    let queue = c.queue.clone();
    let snapshot = c.rig.dir.join("at-publication.json");
    let saved = snapshot.clone();
    *c.files.after_publish.lock().unwrap() = Some(Box::new(move || {
        fs::copy(queue, saved).unwrap();
    }));
    c.finish("Rec.m4a", b"call");
    c.wait_toast("Sent to ");
    assert_eq!(fs::read(c.target.join("Rec-3.m4a")).unwrap(), b"call");
    fs::remove_file(&lower).unwrap();
    fs::copy(snapshot, &c.queue).unwrap(); // Restart at the instant of publication.
    c.restart();
    c.wait_toast("Sent after retry: 1");
    assert_eq!(c.waiting(), 0);
    assert!(!lower.exists(), "the same bytes appeared under two names");
}

#[test]
fn a_blocked_fingerprint_does_not_hold_up_the_local_save_event() {
    use std::sync::mpsc::channel;

    let c = Calls::new("mirror-hash-after-save");
    fs::create_dir(&c.target).unwrap();
    let (hashing, started) = channel();
    let (resume, wait) = channel();
    *c.files.before_fingerprint.lock().unwrap() = Some(Box::new(move || {
        hashing.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(10)).unwrap();
        Ok(())
    }));
    std::thread::scope(|scope| {
        let (saved, done) = channel();
        let calls = &c;
        scope.spawn(move || {
            let _ = saved.send(calls.finish("Rec.m4a", b"local call"));
        });
        started.recv_timeout(Duration::from_secs(10)).unwrap();
        let local = done.recv_timeout(Duration::from_secs(2));
        resume.send(()).unwrap();
        assert_eq!(
            fs::read(local.expect("save waited for the hash")).unwrap(),
            b"local call"
        );
    });
    c.wait_toast("Sent to ");
}

#[test]
fn an_unidentified_call_is_read_from_the_saved_file_on_the_next_pass() {
    let mut c = Calls::new("mirror-identify-later");
    fs::create_dir(&c.target).unwrap();
    *c.files.before_fingerprint.lock().unwrap() = Some(Box::new(|| Err(io::ErrorKind::PermissionDenied.into())));
    let local = c.finish("Rec.m4a", b"original call");
    c.wait_failures(1);
    assert_eq!(c.waiting(), 1);
    assert!(!c.target.join("Rec.m4a").exists());
    c.restart();
    c.wait_toast("Sent after retry: 1");
    assert_eq!(fs::read(local).unwrap(), b"original call");
    assert_eq!(fs::read(c.target.join("Rec.m4a")).unwrap(), b"original call");
    assert_eq!(c.waiting(), 0);
}

#[test]
fn a_blocked_enqueue_is_saved_after_unlock_and_delivered_after_restart() {
    let mut c = Calls::new("mirror-enqueue-write");
    // Load the empty queue before blocking its replacement. No other job will wake the worker.
    fs::create_dir(&c.target).unwrap();
    c.finish("first.m4a", b"first");
    c.wait_toast("Sent to ");
    let previous = c.queue.with_extension("locked");
    fs::rename(&c.queue, &previous).unwrap();
    fs::create_dir(&c.queue).unwrap();
    let transient = c.finish("brief-lock.m4a", b"brief lock");
    fs::remove_dir(&c.queue).unwrap();
    fs::rename(&previous, &c.queue).unwrap();
    assert!(eventually(|| c.rig.ui.last_toast().is_some_and(|t| t.file.as_ref()
        == Some(&transient)
        && t.body.contains("Sent to "))));
    assert!(!c
        .rig
        .ui
        .toasts
        .lock()
        .unwrap()
        .iter()
        .any(|t| t.body.contains("could not be saved")));

    fs::rename(&c.target, c.rig.dir.join("offline")).unwrap();
    fs::rename(&c.queue, &previous).unwrap();
    fs::create_dir(&c.queue).unwrap();
    c.finish("Rec.m4a", b"queued during the lock");
    c.wait_toast("retry queue could not be saved to disk");
    fs::remove_dir(&c.queue).unwrap();
    fs::rename(previous, &c.queue).unwrap();
    assert!(
        eventually(|| c.waiting() == 1),
        "no automatic save after the lock cleared"
    );
    fs::create_dir(&c.target).unwrap();
    c.restart();
    c.wait_toast("Sent after retry: 1");
    assert_eq!(fs::read(c.target.join("Rec.m4a")).unwrap(), b"queued during the lock");
    assert_eq!(c.waiting(), 0);
}

#[test]
fn quitting_before_the_next_save_retries_the_in_memory_enqueue() {
    for (name, quit_from_tray) in [("mirror-enqueue-release", false), ("mirror-enqueue-quit", true)] {
        let mut c = Calls::new(name);
        fs::create_dir(&c.queue).unwrap();
        c.finish("Rec.m4a", b"saved before quit");
        c.wait_toast("retry queue could not be saved to disk");
        fs::remove_dir(&c.queue).unwrap();
        if quit_from_tray {
            c.rig.app.shutdown();
            assert_eq!(c.waiting(), 1);
        }
        fs::create_dir(&c.target).unwrap();
        c.restart();
        c.wait_toast("Sent after retry: 1");
        assert_eq!(fs::read(c.target.join("Rec.m4a")).unwrap(), b"saved before quit");
    }
}

#[test]
fn an_unreleased_path_only_queue_is_ignored_and_logged() {
    let mut c = Calls::new("mirror-old-queue");
    fs::create_dir(&c.target).unwrap();
    let old = c.rig.dir.join("local/old.m4a");
    fs::write(&old, b"old call").unwrap();
    fs::write(&c.queue, serde_json::to_vec(&[old]).unwrap()).unwrap();
    c.restart();
    let log = c.rig.dir.join("shotik-bubble.log");
    assert!(eventually(|| fs::read_to_string(&log)
        .unwrap()
        .contains("ignoring path-only recording copy queue")));
    c.finish("new.m4a", b"new call");
    c.wait_toast("Sent to ");
    assert_eq!(c.waiting(), 0);
    assert!(!c.target.join("old.m4a").exists());
    assert_eq!(fs::read(c.target.join("new.m4a")).unwrap(), b"new call");
}
