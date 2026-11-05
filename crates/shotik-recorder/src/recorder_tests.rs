//! The session and device-error cases of `test/unit/recorder.test.js` and
//! `test/unit/recorder-session.test.js`, against fake sources: no device is touched.

use std::any::Any;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use shotik_core::ports::{RecAudioInfo, RecLevels, RecordOptions, RecordTarget, RecorderControl, RecorderEvent, RecordingDone};
use shotik_core::{Error, Rect, Result};

use crate::backend::{
    AudioInput, AudioOpenError, AudioOpenKind, AudioPacket, Backend, MediaSink, MicDevice, SinkRequest, Snapshot,
    ThreadRole, VideoFrame, VideoRequest, VideoSource,
};
use crate::names::LocalTime;
use crate::recorder::{CountdownHook, Recorder};
use crate::trace::Trace;

/// A gate a fake waits at until the test opens it.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

#[derive(Default)]
struct Log {
    video_opened: AtomicUsize,
    video_released: AtomicUsize,
    system_opened: AtomicUsize,
    system_released: AtomicUsize,
    mic_requests: Mutex<Vec<MicDevice>>,
    mic_released: AtomicUsize,
    sinks: Mutex<Vec<SinkRequest>>,
    finalized: AtomicUsize,
    video_written: AtomicUsize,
    audio_written: AtomicUsize,
}

#[derive(Default)]
struct Script {
    video_error: Option<String>,
    first_frame_gate: Option<Arc<Gate>>,
    first_frame_reached: Arc<AtomicBool>,
    lost_after: Option<u64>,
    system_error: Option<AudioOpenKind>,
    mic_error: Option<AudioOpenKind>,
    /// Signalled when the microphone open has begun, then waits at the gate.
    mic_gate: Option<(Arc<Barrier>, Arc<Gate>)>,
    sink_error: Option<String>,
    finalize_gate: Option<Arc<Gate>>,
    finalize_problem: Option<String>,
    write_error_after: Option<usize>,
    mic_lost_after: Option<u32>,
    panic_in_video_open: bool,
}

struct FakeBackend {
    script: Script,
    log: Arc<Log>,
    started: Instant,
}

impl FakeBackend {
    fn new(script: Script) -> Arc<Self> {
        Arc::new(Self { script, log: Arc::default(), started: Instant::now() })
    }
}

struct FakeVideo {
    log: Arc<Log>,
    sink: FakeSinkScript,
    gate: Option<Arc<Gate>>,
    reached: Arc<AtomicBool>,
    frames: u64,
    lost_after: Option<u64>,
}

impl Drop for FakeVideo {
    fn drop(&mut self) {
        self.log.video_released.fetch_add(1, Ordering::SeqCst);
    }
}

impl VideoSource for FakeVideo {
    fn open_sink(&mut self, request: &SinkRequest, _trace: &Trace) -> Result<Box<dyn MediaSink>> {
        self.sink.open(request, &self.log)
    }

    fn wait_first_frame(&mut self, aborted: &dyn Fn() -> bool) -> Result<Option<Snapshot>> {
        self.reached.store(true, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.wait();
        }
        if aborted() {
            return Ok(None);
        }
        let bgra = vec![0x40; 4 * 4 * 2];
        Ok(Some(Snapshot { bgra, stride: 16, width: 4, height: 2 }))
    }

    fn next_frame(&mut self) -> Result<VideoFrame> {
        self.frames += 1;
        Ok(VideoFrame { payload: Box::new(self.frames), fresh: self.frames % 2 == 1 })
    }

    fn lost(&self) -> Option<String> {
        self.lost_after.filter(|after| self.frames >= *after).map(|_| "the display was disconnected".to_string())
    }
}

#[derive(Clone, Default)]
struct FakeSinkScript {
    error: Option<String>,
    finalize_gate: Option<Arc<Gate>>,
    write_error_after: Option<usize>,
    finalize_problem: Option<String>,
}

impl FakeSinkScript {
    fn open(&self, request: &SinkRequest, log: &Arc<Log>) -> Result<Box<dyn MediaSink>> {
        log.sinks.lock().unwrap().push(request.clone());
        if let Some(message) = &self.error {
            return Err(Error::msg(message.clone()));
        }
        fs::write(&request.path, b"part").map_err(|e| Error::io("write", &request.path, e))?;
        Ok(Box::new(FakeSink { log: Arc::clone(log), script: self.clone(), writes: 0 }))
    }
}

struct FakeSink {
    log: Arc<Log>,
    script: FakeSinkScript,
    writes: usize,
}

impl FakeSink {
    fn count_write(&mut self) -> Result<()> {
        self.writes += 1;
        if self.script.write_error_after.is_some_and(|after| self.writes > after) {
            return Err(Error::msg("the encoder failed"));
        }
        Ok(())
    }
}

impl MediaSink for FakeSink {
    fn write_video(&mut self, _frame: Box<dyn Any + Send>, _pts: i64, _duration: i64) -> Result<()> {
        self.count_write()?;
        self.log.video_written.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn write_audio(&mut self, _pcm: &[i16], _pts: i64, _duration: i64) -> Result<()> {
        self.count_write()?;
        self.log.audio_written.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn bytes_written(&self) -> u64 {
        self.writes as u64 * 10
    }

    fn finalize(self: Box<Self>) -> Result<Option<String>> {
        if let Some(gate) = &self.script.finalize_gate {
            gate.wait();
        }
        self.log.finalized.fetch_add(1, Ordering::SeqCst);
        Ok(self.script.finalize_problem.clone())
    }
}

struct FakeAudio {
    log: Arc<Log>,
    microphone: bool,
    started: Instant,
    next_ms: u64,
    reads: u32,
    lost_after: Option<u32>,
}

impl Drop for FakeAudio {
    fn drop(&mut self) {
        let counter = if self.microphone { &self.log.mic_released } else { &self.log.system_released };
        counter.fetch_add(1, Ordering::SeqCst);
    }
}

impl AudioInput for FakeAudio {
    fn read(&mut self, deliver: &mut dyn FnMut(AudioPacket<'_>)) -> Result<()> {
        self.reads += 1;
        if self.lost_after.is_some_and(|after| self.reads > after) {
            return Err(Error::msg("unplugged"));
        }
        thread::sleep(Duration::from_millis(5));
        let now_ms = self.started.elapsed().as_millis() as u64;
        while self.next_ms + 10 <= now_ms {
            let samples = vec![0.25_f32; 480 * 2];
            deliver(AudioPacket { at: self.next_ms as i64 * 10_000, samples: &samples });
            self.next_ms += 10;
        }
        Ok(())
    }
}

impl Backend for FakeBackend {
    fn session_guard(&self) -> Result<Box<dyn Any>> {
        Ok(Box::new(()))
    }

    fn enter_thread(&self, _role: ThreadRole) -> Box<dyn Any> {
        Box::new(())
    }

    fn now(&self) -> i64 {
        self.started.elapsed().as_nanos() as i64 / 100
    }

    fn local_time(&self) -> LocalTime {
        LocalTime { year: 2026, month: 11, day: 4, hour: 12, minute: 0, second: 0 }
    }

    fn open_video(&self, _request: &VideoRequest, _trace: &Trace) -> Result<Box<dyn VideoSource>> {
        self.log.video_opened.fetch_add(1, Ordering::SeqCst);
        assert!(!self.script.panic_in_video_open, "scripted panic");
        if let Some(message) = &self.script.video_error {
            return Err(Error::msg(message.clone()));
        }
        Ok(Box::new(FakeVideo {
            log: Arc::clone(&self.log),
            sink: self.sink_script(),
            gate: self.script.first_frame_gate.clone(),
            reached: Arc::clone(&self.script.first_frame_reached),
            frames: 0,
            lost_after: self.script.lost_after,
        }))
    }

    fn open_system_audio(&self, _trace: &Trace) -> std::result::Result<Box<dyn AudioInput>, AudioOpenError> {
        self.log.system_opened.fetch_add(1, Ordering::SeqCst);
        if let Some(kind) = self.script.system_error {
            return Err(AudioOpenError { kind, detail: "scripted".into() });
        }
        Ok(Box::new(self.audio(false, None)))
    }

    fn open_microphone(
        &self,
        device: &MicDevice,
        _trace: &Trace,
    ) -> std::result::Result<Box<dyn AudioInput>, AudioOpenError> {
        self.log.mic_requests.lock().unwrap().push(device.clone());
        if let Some((opening, gate)) = &self.script.mic_gate {
            opening.wait();
            gate.wait();
        }
        if let Some(kind) = self.script.mic_error {
            return Err(AudioOpenError { kind, detail: "scripted".into() });
        }
        Ok(Box::new(self.audio(true, self.script.mic_lost_after)))
    }

    fn open_audio_sink(&self, request: &SinkRequest, _trace: &Trace) -> Result<Box<dyn MediaSink>> {
        self.sink_script().open(request, &self.log)
    }
}

impl FakeBackend {
    fn sink_script(&self) -> FakeSinkScript {
        FakeSinkScript {
            error: self.script.sink_error.clone(),
            finalize_gate: self.script.finalize_gate.clone(),
            write_error_after: self.script.write_error_after,
            finalize_problem: self.script.finalize_problem.clone(),
        }
    }

    fn audio(&self, microphone: bool, lost_after: Option<u32>) -> FakeAudio {
        let next_ms = self.started.elapsed().as_millis() as u64;
        FakeAudio { log: Arc::clone(&self.log), microphone, started: self.started, next_ms, reads: 0, lost_after }
    }
}

/// A recorder over the fake backend with its own directories and an event channel.
struct Fixture {
    recorder: Arc<Recorder>,
    log: Arc<Log>,
    events: Receiver<RecorderEvent>,
    dir: PathBuf,
    save_dir: PathBuf,
    temp_dir: PathBuf,
}

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

impl Fixture {
    fn new(script: Script) -> Self {
        Self::build(script, None, Duration::from_secs(10))
    }

    fn build(script: Script, countdown: Option<CountdownHook>, finalize_timeout: Duration) -> Self {
        let backend = FakeBackend::new(script);
        let log = Arc::clone(&backend.log);
        let dir = std::env::temp_dir().join(format!(
            "shotik-recorder-test-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::SeqCst)
        ));
        let (save_dir, temp_dir) = (dir.join("saved"), dir.join("temp"));
        fs::create_dir_all(&temp_dir).unwrap();
        let recorder = Arc::new(Recorder::with_backend(
            backend,
            temp_dir.clone(),
            countdown,
            finalize_timeout,
            Duration::from_millis(500),
        ));
        let (tx, events) = channel::<RecorderEvent>();
        let tx: Mutex<Sender<RecorderEvent>> = Mutex::new(tx);
        recorder.set_listener(Arc::new(move |event| {
            let _ = tx.lock().unwrap().send(event);
        }));
        Self { recorder, log, events, dir, save_dir, temp_dir }
    }

    fn options(&self, system_audio: bool, microphone: bool, device: &str) -> RecordOptions {
        RecordOptions {
            fps: 30,
            format: "auto".into(),
            system_audio,
            microphone,
            microphone_device_id: device.into(),
            save_dir: self.save_dir.clone(),
        }
    }

    fn start_video(&self, system_audio: bool, microphone: bool) -> Option<shotik_core::ports::RecStatus> {
        self.recorder.start(video_target(), self.options(system_audio, microphone, "default")).unwrap()
    }

    fn start_audio(&self, system_audio: bool, microphone: bool) -> Option<shotik_core::ports::RecStatus> {
        self.recorder.start(RecordTarget::AudioOnly, self.options(system_audio, microphone, "default")).unwrap()
    }

    /// Events until one matches, or a panic that shows what came.
    fn wait_for(&self, what: &str, matches: impl Fn(&RecorderEvent) -> bool) -> RecorderEvent {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(event) => {
                    if matches(&event) {
                        return event;
                    }
                    seen.push(event);
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    panic!("no {what}, saw {seen:?}, status {:?}", self.recorder.status())
                }
            }
        }
    }

    fn wait_running(&self) {
        self.wait_for("running state", |e| matches!(e, RecorderEvent::State(s) if s.audio.is_some()));
    }

    fn wait_idle(&self) {
        self.wait_for("idle state", |e| matches!(e, RecorderEvent::State(s) if !s.recording));
        assert!(!self.recorder.is_recording());
    }

    fn wait_error(&self) -> String {
        let event = self.wait_for("error", |e| matches!(e, RecorderEvent::Error(_)));
        let RecorderEvent::Error(message) = event else { unreachable!() };
        message
    }

    fn wait_done(&self) -> RecordingDone {
        let event = self.wait_for("done", |e| matches!(e, RecorderEvent::Done(_)));
        let RecorderEvent::Done(done) = event else { unreachable!() };
        done
    }

    fn trace(&self) -> Vec<String> {
        self.recorder.status().trace
    }

    fn temp_files(&self) -> usize {
        fs::read_dir(&self.temp_dir).unwrap().count()
    }

    fn saved_files(&self) -> usize {
        fs::read_dir(&self.save_dir).map_or(0, Iterator::count)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.recorder.cancel();
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.recorder.is_recording() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn video_target() -> RecordTarget {
    RecordTarget::Region { display_id: 1, rect_phys: Rect::new(0, 0, 320, 180), rect_dip: Rect::new(0, 0, 214, 120) }
}

// ---- test/unit/recorder.test.js ----

#[test]
fn selected_microphone_uses_the_exact_device_never_a_silent_fallback() {
    let f = Fixture::new(Script::default());
    f.recorder.start(video_target(), f.options(false, true, "usb-mic")).unwrap();
    f.wait_running();
    assert_eq!(*f.log.mic_requests.lock().unwrap(), vec![MicDevice::Exact("usb-mic".into())]);
}

#[test]
fn system_default_microphone_does_not_pin_a_stale_device_id() {
    let f = Fixture::new(Script::default());
    f.recorder.start(video_target(), f.options(false, true, "default")).unwrap();
    f.wait_running();
    assert_eq!(*f.log.mic_requests.lock().unwrap(), vec![MicDevice::Default]);
}

#[test]
fn a_microphone_failure_is_reported_by_name_and_releases_the_screen() {
    for kind in [
        AudioOpenKind::NotAllowed,
        AudioOpenKind::NotFound,
        AudioOpenKind::NotReadable,
        AudioOpenKind::Overconstrained,
    ] {
        let f = Fixture::new(Script { mic_error: Some(kind), ..Script::default() });
        f.start_video(false, true).unwrap();
        assert_eq!(f.wait_error(), format!("microphone:{}", kind.name()));
        f.wait_idle();
        assert_eq!(f.log.video_released.load(Ordering::SeqCst), 1, "screen capture released");
        assert_eq!(f.log.finalized.load(Ordering::SeqCst), 0);
        assert_eq!(f.saved_files(), 0);
    }
}

#[test]
fn an_empty_microphone_stream_is_an_error_not_a_silent_video() {
    let f = Fixture::new(Script { mic_error: Some(AudioOpenKind::Empty), ..Script::default() });
    f.start_video(false, true).unwrap();
    assert_eq!(f.wait_error(), "microphone:Error");
    f.wait_idle();
    assert_eq!(f.log.video_released.load(Ordering::SeqCst), 1);
}

#[test]
fn stopping_during_microphone_startup_releases_a_late_stream() {
    let opening = Arc::new(Barrier::new(2));
    let gate = Arc::new(Gate::default());
    let f = Fixture::new(Script { mic_gate: Some((Arc::clone(&opening), Arc::clone(&gate))), ..Script::default() });
    f.start_video(false, true).unwrap();
    opening.wait();
    f.recorder.stop();
    gate.open();
    f.wait_idle();
    assert_eq!(f.log.mic_released.load(Ordering::SeqCst), 1, "the late stream is released");
    assert_eq!(f.log.video_released.load(Ordering::SeqCst), 1);
    assert_eq!(f.log.finalized.load(Ordering::SeqCst), 0, "nothing to keep");
    assert_eq!((f.saved_files(), f.temp_files()), (0, 0));
    assert!(f.log.sinks.lock().unwrap().is_empty());
}

#[test]
fn audio_only_never_touches_the_screen_and_records_aac_m4a() {
    let f = Fixture::new(Script::default());
    let started = f.start_audio(true, true).unwrap();
    assert_eq!(started.audio_only, Some(true));
    f.wait_running();
    assert_eq!(f.recorder.status().audio, Some(RecAudioInfo { system: true, microphone: true, tracks: 1 }));
    thread::sleep(Duration::from_millis(50));
    f.recorder.stop();
    let done = f.wait_done();
    assert_eq!(done.file.extension().unwrap(), "m4a");
    assert!(done.audio_only);
    assert_eq!((done.width, done.height), (0, 0));
    assert_eq!(done.first_frame_jpeg, None);
    assert_eq!(f.log.video_opened.load(Ordering::SeqCst), 0);
    assert!(f.log.sinks.lock().unwrap()[0].audio);
    f.wait_idle();
    assert!(f.trace().iter().any(|l| l == "recorder.start audio/mp4;codecs=mp4a.40.2"), "{:?}", f.trace());
    assert!(f.trace().iter().any(|l| l.starts_with("index ok")), "{:?}", f.trace());
}

#[test]
fn microphone_only_audio_never_asks_for_the_screen_or_system_audio() {
    let f = Fixture::new(Script::default());
    f.start_audio(false, true).unwrap();
    f.wait_running();
    assert_eq!(f.recorder.status().audio, Some(RecAudioInfo { system: false, microphone: true, tracks: 1 }));
    assert_eq!(f.log.video_opened.load(Ordering::SeqCst), 0);
    assert_eq!(f.log.system_opened.load(Ordering::SeqCst), 0);
    assert!(f.trace().iter().any(|l| l.contains("vtracks=0 atracks=0")), "{:?}", f.trace());
}

#[test]
fn audio_mode_without_system_audio_is_an_explicit_error_not_a_silent_file() {
    let f = Fixture::new(Script { system_error: Some(AudioOpenKind::NotReadable), ..Script::default() });
    f.start_audio(true, false).unwrap();
    assert_eq!(f.wait_error(), "audio:system-unavailable");
    f.wait_idle();
    assert_eq!(f.log.finalized.load(Ordering::SeqCst), 0);
    assert_eq!(f.recorder.status().audio, None);
}

#[test]
fn audio_mode_without_system_audio_fails_even_when_the_microphone_is_on() {
    let f = Fixture::new(Script { system_error: Some(AudioOpenKind::NotReadable), ..Script::default() });
    f.start_audio(true, true).unwrap();
    assert_eq!(f.wait_error(), "audio:system-unavailable");
    f.wait_idle();
    assert!(f.log.mic_requests.lock().unwrap().is_empty());
}

#[test]
fn a_video_session_goes_on_without_system_audio_and_says_so() {
    let f = Fixture::new(Script { system_error: Some(AudioOpenKind::NotFound), ..Script::default() });
    f.start_video(true, false).unwrap();
    assert_eq!(f.wait_error(), "audio:system-unavailable");
    f.wait_running();
    assert_eq!(f.recorder.status().audio, Some(RecAudioInfo { system: false, microphone: false, tracks: 0 }));
    f.recorder.stop();
    assert_eq!(f.wait_done().file.extension().unwrap(), "mp4");
}

#[test]
fn audio_mode_with_both_inputs_off_explains_instead_of_recording_silence() {
    let f = Fixture::new(Script::default());
    f.start_audio(false, false).unwrap();
    assert_eq!(f.wait_error(), "audio:none");
    f.wait_idle();
    assert!(f.log.sinks.lock().unwrap().is_empty());
}

/// Replaces the Opus fallback of the JS version: MP4 and AAC are the only output here, and the
/// missing encoder has to be visible instead of a different file.
#[test]
fn without_an_encoder_the_session_fails_with_the_reason() {
    let f = Fixture::new(Script { sink_error: Some("no AAC encoder on this system".into()), ..Script::default() });
    f.start_audio(false, true).unwrap();
    assert_eq!(f.wait_error(), "no AAC encoder on this system");
    f.wait_idle();
    assert_eq!(f.log.mic_released.load(Ordering::SeqCst), 1);
}

#[test]
fn a_video_source_that_cannot_open_is_reported() {
    let f = Fixture::new(Script { video_error: Some("no such display".into()), ..Script::default() });
    f.start_video(false, false).unwrap();
    assert_eq!(f.wait_error(), "no such display");
    f.wait_idle();
}

// ---- test/unit/recorder-session.test.js ----

/// A countdown that lasts until the test says so.
fn manual_countdown() -> (CountdownHook, Arc<Gate>, Arc<AtomicUsize>) {
    let gate = Arc::new(Gate::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let hook: CountdownHook = {
        let gate = Arc::clone(&gate);
        let calls = Arc::clone(&calls);
        Arc::new(move |_target, done| {
            calls.fetch_add(1, Ordering::SeqCst);
            let gate = Arc::clone(&gate);
            thread::spawn(move || {
                gate.wait();
                done(());
            });
        })
    };
    (hook, gate, calls)
}

fn wait_until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn audio_start_during_the_video_countdown_is_refused_one_session_wins() {
    let (hook, gate, calls) = manual_countdown();
    let f = Fixture::build(Script::default(), Some(hook), Duration::from_secs(10));
    let first = {
        let recorder = Arc::clone(&f.recorder);
        let options = f.options(false, false, "default");
        thread::spawn(move || recorder.start(video_target(), options).unwrap())
    };
    wait_until("the countdown", || calls.load(Ordering::SeqCst) == 1);
    assert!(f.recorder.is_recording(), "counts as recording while the countdown runs");
    assert_eq!(f.start_audio(true, true), None, "second start refused");
    assert_eq!(f.log.video_opened.load(Ordering::SeqCst), 0, "nothing opens during the countdown");
    gate.open();
    let status = first.join().unwrap().unwrap();
    assert_eq!((status.recording, status.audio_only), (true, Some(false)), "the video session is the one running");
    f.wait_running();
    assert_eq!(f.log.video_opened.load(Ordering::SeqCst), 1, "exactly one session");
    f.recorder.cancel();
    f.wait_idle();
}

#[test]
fn stop_during_the_countdown_calls_the_start_off() {
    let (hook, gate, calls) = manual_countdown();
    let f = Fixture::build(Script::default(), Some(hook), Duration::from_secs(10));
    let first = {
        let recorder = Arc::clone(&f.recorder);
        let options = f.options(false, false, "default");
        thread::spawn(move || recorder.start(video_target(), options).unwrap())
    };
    wait_until("the countdown", || calls.load(Ordering::SeqCst) == 1);
    f.recorder.stop();
    assert_eq!(first.join().unwrap(), None);
    assert!(!f.recorder.is_recording());
    assert_eq!(f.log.video_opened.load(Ordering::SeqCst), 0, "no capture ever started");
    gate.open();
}

#[test]
fn cancel_during_the_countdown_calls_the_start_off() {
    let (hook, gate, calls) = manual_countdown();
    let f = Fixture::build(Script::default(), Some(hook), Duration::from_secs(10));
    let first = {
        let recorder = Arc::clone(&f.recorder);
        let options = f.options(false, false, "default");
        thread::spawn(move || recorder.start(video_target(), options).unwrap())
    };
    wait_until("the countdown", || calls.load(Ordering::SeqCst) == 1);
    f.recorder.cancel();
    assert_eq!(first.join().unwrap(), None);
    assert!(!f.recorder.is_recording());
    assert_eq!(f.log.video_opened.load(Ordering::SeqCst), 0);
    gate.open();
}

#[test]
fn audio_mode_skips_the_countdown() {
    let (hook, _gate, calls) = manual_countdown();
    let f = Fixture::build(Script::default(), Some(hook), Duration::from_secs(10));
    let status = f.start_audio(true, true).unwrap();
    assert_eq!((status.recording, status.audio_only), (true, Some(true)));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no countdown");
    f.wait_running();
    f.recorder.stop();
    f.wait_idle();
}

#[test]
fn a_session_can_follow_a_session() {
    let f = Fixture::new(Script::default());
    f.start_audio(true, false).unwrap();
    f.wait_running();
    assert_eq!(f.start_video(false, false), None, "one at a time");
    f.recorder.stop();
    f.wait_idle();
    assert!(f.start_video(false, false).is_some());
    f.wait_running();
    f.recorder.stop();
    f.wait_idle();
    assert_eq!(f.log.finalized.load(Ordering::SeqCst), 2);
}

// ---- the session contract ----

#[test]
fn audio_stays_absent_until_capture_really_runs() {
    let gate = Arc::new(Gate::default());
    let f = Fixture::new(Script { first_frame_gate: Some(Arc::clone(&gate)), ..Script::default() });
    let started = f.start_video(false, true).unwrap();
    assert_eq!(started.audio, None, "from the first moment");
    assert_eq!(started.paused, Some(false));
    assert_eq!(f.recorder.status().audio, None, "while the first frame is awaited");
    assert!(serde_json::to_value(f.recorder.status()).unwrap().get("audio").is_none());
    gate.open();
    f.wait_running();
    let status = f.recorder.status();
    assert_eq!(status.audio, Some(RecAudioInfo { system: false, microphone: true, tracks: 1 }));
    assert!(status.start_ts.is_some_and(|ts| ts > 0));
    f.recorder.cancel();
    f.wait_idle();
}

#[test]
fn a_video_session_with_no_inputs_reports_zero_tracks() {
    let f = Fixture::new(Script::default());
    f.start_video(false, false).unwrap();
    f.wait_running();
    assert_eq!(f.recorder.status().audio, Some(RecAudioInfo { system: false, microphone: false, tracks: 0 }));
    assert_eq!(f.recorder.status().levels, None, "no meters without inputs");
}

#[test]
fn stop_delivers_the_file_with_the_dip_size_of_the_region() {
    let f = Fixture::new(Script::default());
    f.start_video(true, false).unwrap();
    f.wait_running();
    thread::sleep(Duration::from_millis(120));
    f.recorder.stop();
    let done = f.wait_done();
    assert_eq!((done.width, done.height), (214, 120), "DIP, as recorder.js gives history");
    assert!(!done.audio_only);
    assert!(done.duration_sec > 0.1 && done.duration_sec < 2.0, "{}", done.duration_sec);
    assert_eq!(done.file.parent().unwrap(), f.save_dir);
    assert_eq!(done.file.file_name().unwrap(), "Rec_2026-11-04_12-00-00.mp4");
    assert_eq!(fs::read(&done.file).unwrap(), b"part");
    let jpeg = done.first_frame_jpeg.expect("a thumbnail");
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    assert_eq!(f.temp_files(), 0, "the part file moved");
    f.wait_idle();
    assert!(f.log.video_written.load(Ordering::SeqCst) >= 3);
    assert!(f.log.audio_written.load(Ordering::SeqCst) >= 1);
}

#[test]
fn the_recorder_reports_idle_before_the_listener_hears_done() {
    let f = Fixture::new(Script::default());
    let seen = Arc::new(Mutex::new(None));
    {
        let recorder = Arc::downgrade(&f.recorder);
        let seen = Arc::clone(&seen);
        f.recorder.set_listener(Arc::new(move |event| {
            if matches!(event, RecorderEvent::Done(_)) {
                *seen.lock().unwrap() = recorder.upgrade().map(|r| r.status().recording);
            }
        }));
    }
    f.start_audio(true, false).unwrap();
    wait_until("capture", || f.recorder.status().audio.is_some());
    f.recorder.stop();
    wait_until("done", || seen.lock().unwrap().is_some());
    assert_eq!(*seen.lock().unwrap(), Some(false), "ghost tests read the status as soon as they see the file");
}

#[test]
fn a_taken_file_name_gets_a_counter() {
    let f = Fixture::new(Script::default());
    fs::create_dir_all(&f.save_dir).unwrap();
    fs::write(f.save_dir.join("Rec_2026-11-04_12-00-00.mp4"), b"old").unwrap();
    f.start_video(false, false).unwrap();
    f.wait_running();
    f.recorder.stop();
    assert_eq!(f.wait_done().file.file_name().unwrap(), "Rec_2026-11-04_12-00-00-2.mp4");
}

#[test]
fn an_unusable_save_directory_is_a_start_error_and_leaves_the_recorder_free() {
    let f = Fixture::new(Script::default());
    fs::create_dir_all(&f.dir).unwrap();
    let blocked = f.dir.join("a-file");
    fs::write(&blocked, b"x").unwrap();
    let mut options = f.options(false, false, "default");
    options.save_dir = blocked.join("sub");
    assert!(f.recorder.start(video_target(), options).is_err());
    assert!(!f.recorder.is_recording());
    assert!(f.start_video(false, false).is_some());
}

#[test]
fn cancel_deletes_everything() {
    let f = Fixture::new(Script::default());
    f.start_video(true, true).unwrap();
    f.wait_running();
    thread::sleep(Duration::from_millis(50));
    f.recorder.cancel();
    f.wait_idle();
    assert_eq!(f.log.finalized.load(Ordering::SeqCst), 0);
    assert_eq!((f.saved_files(), f.temp_files()), (0, 0));
    assert_eq!(f.log.video_released.load(Ordering::SeqCst), 1);
    assert_eq!(f.log.mic_released.load(Ordering::SeqCst), 1);
    assert_eq!(f.log.system_released.load(Ordering::SeqCst), 1);
}

#[test]
fn stop_right_after_the_start_keeps_nothing_when_capture_had_not_begun() {
    let gate = Arc::new(Gate::default());
    let f = Fixture::new(Script { first_frame_gate: Some(Arc::clone(&gate)), ..Script::default() });
    f.start_video(false, false).unwrap();
    f.recorder.stop();
    gate.open();
    f.wait_idle();
    assert_eq!((f.saved_files(), f.temp_files()), (0, 0));
    assert_eq!(f.log.finalized.load(Ordering::SeqCst), 0);
}

#[test]
fn stop_right_after_capture_begins_gives_a_short_valid_file() {
    let f = Fixture::new(Script::default());
    f.start_video(true, false).unwrap();
    f.wait_running();
    f.recorder.stop();
    let done = f.wait_done();
    assert!(done.file.exists());
    assert!(f.log.video_written.load(Ordering::SeqCst) >= 1, "at least one frame");
    assert!(f.log.audio_written.load(Ordering::SeqCst) >= 1, "audio reaches the end of the video");
}

#[test]
fn a_session_writes_frames_in_real_time_and_pause_stops_them() {
    let f = Fixture::new(Script::default());
    f.start_video(false, false).unwrap();
    f.wait_running();
    thread::sleep(Duration::from_millis(300));
    f.recorder.pause();
    let status = f.recorder.status();
    assert_eq!(status.paused, Some(true));
    assert!(matches!(
        f.wait_for("paused state", |e| matches!(e, RecorderEvent::State(s) if s.paused == Some(true))),
        RecorderEvent::State(_)
    ));
    thread::sleep(Duration::from_millis(50));
    let during = f.log.video_written.load(Ordering::SeqCst);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(f.log.video_written.load(Ordering::SeqCst), during, "no frames while paused");
    f.recorder.resume();
    assert_eq!(f.recorder.status().paused, Some(false));
    thread::sleep(Duration::from_millis(300));
    f.recorder.stop();
    let done = f.wait_done();
    assert!(done.duration_sec > 0.55 && done.duration_sec < 0.9, "pause is not in the length: {}", done.duration_sec);
    let frames = f.log.video_written.load(Ordering::SeqCst) as f64;
    assert!((frames - done.duration_sec * 30.0).abs() <= 1.0, "constant rate: {frames} in {}", done.duration_sec);
}

#[test]
fn levels_are_emitted_about_ten_times_a_second_and_land_in_the_status() {
    let f = Fixture::new(Script::default());
    f.start_audio(true, true).unwrap();
    f.wait_running();
    let first = Instant::now();
    let mut count = 0;
    let mut last = RecLevels::default();
    while first.elapsed() < Duration::from_millis(1000) {
        if let Ok(RecorderEvent::Levels(levels)) = f.events.recv_timeout(Duration::from_millis(50)) {
            count += 1;
            last = levels;
        }
    }
    assert!((7..=13).contains(&count), "{count} per second");
    assert_eq!(last, RecLevels { system: 0.25, microphone: 0.25 });
    assert_eq!(f.recorder.status().levels, Some(last));
}

#[test]
fn an_input_lost_midway_is_reported_and_the_session_goes_on() {
    let f = Fixture::new(Script { mic_lost_after: Some(3), ..Script::default() });
    f.start_video(false, true).unwrap();
    assert_eq!(f.wait_error(), "microphone:NotFoundError");
    assert!(f.recorder.is_recording());
    f.recorder.stop();
    assert!(f.wait_done().file.exists());
}

#[test]
fn a_display_lost_midway_finishes_the_file_and_says_why() {
    let f = Fixture::new(Script { lost_after: Some(5), ..Script::default() });
    f.start_video(false, false).unwrap();
    let error = f.wait_error();
    assert_eq!(error, "the display was disconnected");
    assert!(f.wait_done().file.exists());
    f.wait_idle();
}

#[test]
fn an_encoder_failure_midway_keeps_what_was_written_and_says_why() {
    let f = Fixture::new(Script { write_error_after: Some(5), ..Script::default() });
    f.start_video(false, false).unwrap();
    assert_eq!(f.wait_error(), "the encoder failed");
    assert!(f.wait_done().file.exists());
    f.wait_idle();
}

#[test]
fn an_encoder_failing_on_the_last_frames_keeps_the_file_and_says_why() {
    let f = Fixture::new(Script { finalize_problem: Some("the last frames were lost".into()), ..Script::default() });
    f.start_video(false, false).unwrap();
    f.wait_running();
    f.recorder.stop();
    assert_eq!(f.wait_error(), "the last frames were lost");
    assert!(f.wait_done().file.exists());
}

#[test]
fn a_save_folder_lost_during_the_session_keeps_the_recording_and_says_where() {
    let f = Fixture::new(Script::default());
    f.start_video(false, false).unwrap();
    f.wait_running();
    thread::sleep(Duration::from_millis(80));
    fs::remove_dir_all(&f.save_dir).unwrap();
    fs::write(&f.save_dir, b"a file where the folder was").unwrap();
    f.recorder.stop();
    let error = f.wait_error();
    let done = f.wait_done();
    assert_eq!(fs::read(&done.file).unwrap(), b"part", "the only copy of the recording");
    assert_eq!(done.file.file_name().unwrap(), "Rec_2026-11-04_12-00-00.mp4", "under its real name");
    assert!(error.contains(&done.file.display().to_string()), "{error}");
    fs::remove_file(&f.save_dir).unwrap();
    f.wait_idle();
}

#[test]
fn a_cancel_during_finalize_still_deletes_everything() {
    let gate = Arc::new(Gate::default());
    let f = Fixture::new(Script { finalize_gate: Some(Arc::clone(&gate)), ..Script::default() });
    f.start_video(false, false).unwrap();
    f.wait_running();
    f.recorder.stop();
    thread::sleep(Duration::from_millis(60));
    f.recorder.cancel();
    gate.open();
    f.wait_idle();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(f.temp_files(), 0, "no part file");
    assert_eq!(fs::read_dir(&f.save_dir).unwrap().count(), 0, "no recording");
}

#[test]
fn the_finalize_timeout_frees_the_recorder_and_a_late_file_still_arrives() {
    let gate = Arc::new(Gate::default());
    let f = Fixture::build(
        Script { finalize_gate: Some(Arc::clone(&gate)), ..Script::default() },
        None,
        Duration::from_millis(100),
    );
    f.start_video(false, false).unwrap();
    f.wait_running();
    assert!(f.recorder.has_unfinished_file());
    f.recorder.stop();
    let error = f.wait_error();
    assert!(error.contains("did not finish"), "{error}");
    f.wait_idle();
    assert!(!f.recorder.is_recording());
    assert!(f.recorder.has_unfinished_file(), "the worker is still writing: ending the process would lose the file");
    assert!(f.trace().iter().any(|l| l.starts_with("finalize timed out")), "{:?}", f.trace());

    assert!(f.start_audio(true, false).is_some(), "the recorder is free again");
    f.wait_running();
    gate.open();
    assert!(f.wait_done().file.exists(), "the file of the first session is not lost");
    assert!(f.recorder.is_recording(), "the second session goes on");
    f.recorder.stop();
    f.wait_idle();
    for _ in 0..500 {
        if !f.recorder.has_unfinished_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!f.recorder.has_unfinished_file(), "every file written, nothing left to lose");
}

#[test]
fn a_stuck_cancel_times_out_quietly() {
    let gate = Arc::new(Gate::default());
    let reached = Arc::new(AtomicBool::new(false));
    let f = Fixture::new(Script {
        first_frame_gate: Some(Arc::clone(&gate)),
        first_frame_reached: Arc::clone(&reached),
        ..Script::default()
    });
    f.start_video(false, false).unwrap();
    wait_until("the worker to be stuck", || reached.load(Ordering::SeqCst));
    f.recorder.cancel();
    f.wait_idle();
    assert!(f.trace().iter().any(|l| l == "finalize timed out canceled=true"), "{:?}", f.trace());
    gate.open();
    wait_until("the part file to go", || f.temp_files() == 0);
}

#[test]
fn a_panic_in_a_session_is_reported_and_leaves_the_recorder_free() {
    let f = Fixture::new(Script { panic_in_video_open: true, ..Script::default() });
    f.start_video(false, false).unwrap();
    assert!(f.wait_error().contains("internal error"));
    f.wait_idle();
    assert!(f.start_audio(true, false).is_some());
}

#[test]
fn trace_starts_over_with_each_session() {
    let f = Fixture::new(Script::default());
    f.start_audio(true, false).unwrap();
    f.wait_running();
    f.recorder.stop();
    f.wait_idle();
    assert!(f.trace().iter().any(|l| l.contains("audioOnly=true")));
    f.start_video(false, false).unwrap();
    f.wait_running();
    assert!(!f.trace().iter().any(|l| l.contains("audioOnly=true")), "{:?}", f.trace());
}
