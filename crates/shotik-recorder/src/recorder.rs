//! [`Recorder`] implements `RecorderControl`. The session machine decides, the engine works on its
//! own threads, and this module connects the two and reports to the listener. It has no UI: the
//! application shows the border and controls when it hears `RecorderEvent::State` and registers the
//! file in history when it hears `RecorderEvent::Done`.

use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use shotik_core::ports::{
    Callback, Displays, Once, RecAudioInfo, RecLevels, RecStatus, RecordOptions, RecordTarget, RecorderControl,
    RecorderEvent,
};
use shotik_core::{Error, Result};

use crate::backend::Backend;
use crate::engine::{self, Control, Job, Outcome, SessionEvents};
use crate::names::ts_name;
use crate::session::{Request, SessionId, SessionMachine, Timeouts};
use crate::synth::SynthOptions;
use crate::trace::Trace;

/// Where pictures and sound come from.
#[derive(Clone)]
pub enum CaptureMode {
    /// Windows Graphics Capture, WASAPI. `displays` finds the monitor of a recording region.
    Real { displays: Arc<dyn Displays> },
    /// An animated picture and tones, nothing from the machine.
    Synthetic(SynthOptions),
}

impl CaptureMode {
    /// The rule of `src/main/recorder.js`, with the environment read by the caller:
    /// `test` is SHOTIK_TEST=1, `real_capture` is SHOTIK_REAL_CAPTURE=1.
    pub fn from_flags(test: bool, real_capture: bool, displays: Arc<dyn Displays>) -> Self {
        if test && !real_capture {
            Self::Synthetic(SynthOptions::default())
        } else {
            Self::Real { displays }
        }
    }
}

/// Runs before a video session captures anything: shows the countdown and calls `done` when it has
/// finished. The recorder counts as recording meanwhile and a stop or cancel calls the start off.
/// The hook decides whether a countdown is wanted at all, by settings and test mode.
pub type CountdownHook = Arc<dyn Fn(&RecordTarget, Once<()>) + Send + Sync>;

#[derive(Clone)]
pub struct RecorderConfig {
    pub capture: CaptureMode,
    /// Where the part file lives until it is complete.
    pub temp_dir: PathBuf,
    pub countdown: Option<CountdownHook>,
    /// How long a stop may take to finalize before the recorder reports failure and frees itself.
    /// Longer than the 4 s of the JS version, whose file was already on disk at that point: this one
    /// waits for the encoder to drain and the index to be written. A worker that finishes later
    /// still delivers its file.
    pub finalize_timeout: Duration,
    pub cancel_timeout: Duration,
}

impl RecorderConfig {
    pub fn new(capture: CaptureMode) -> Self {
        Self {
            capture,
            temp_dir: std::env::temp_dir(),
            countdown: None,
            finalize_timeout: Duration::from_secs(10),
            cancel_timeout: Duration::from_secs(2),
        }
    }
}

struct Inner {
    backend: Arc<dyn Backend>,
    temp_dir: PathBuf,
    countdown: Option<CountdownHook>,
    machine: Mutex<SessionMachine>,
    /// Wakes the countdown wait and the watchdog.
    wake: Condvar,
    listener: Mutex<Option<Callback<RecorderEvent>>>,
    trace: Trace,
    control: Mutex<Option<Arc<Control>>>,
    started: Instant,
    /// Session workers still running.
    unfinished: AtomicUsize,
}

pub struct Recorder {
    inner: Arc<Inner>,
}

impl Recorder {
    #[cfg(windows)]
    pub fn new(config: RecorderConfig) -> Result<Self> {
        let RecorderConfig { capture, temp_dir, countdown, finalize_timeout, cancel_timeout } = config;
        let backend = Arc::new(crate::win::backend::WinBackend::new(capture));
        Ok(Self::with_backend(backend, temp_dir, countdown, finalize_timeout, cancel_timeout))
    }

    #[cfg(not(windows))]
    pub fn new(_config: RecorderConfig) -> Result<Self> {
        Err(Error::Unsupported("Recording"))
    }

    pub(crate) fn with_backend(
        backend: Arc<dyn Backend>,
        temp_dir: PathBuf,
        countdown: Option<CountdownHook>,
        finalize_timeout: Duration,
        cancel_timeout: Duration,
    ) -> Self {
        let timeouts =
            Timeouts { stop_ms: finalize_timeout.as_millis() as u64, cancel_ms: cancel_timeout.as_millis() as u64 };
        Self {
            inner: Arc::new(Inner {
                backend,
                temp_dir,
                countdown,
                machine: Mutex::new(SessionMachine::new(timeouts)),
                wake: Condvar::new(),
                listener: Mutex::new(None),
                trace: Trace::default(),
                control: Mutex::new(None),
                started: Instant::now(),
                unfinished: AtomicUsize::new(0),
            }),
        }
    }
}

impl Inner {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn emit(&self, event: RecorderEvent) {
        let listener = self.listener.lock().unwrap().clone();
        if let Some(listener) = listener {
            listener(event);
        }
    }

    fn status(&self) -> RecStatus {
        let bytes = self.control.lock().unwrap().as_ref().map_or(0, |c| c.bytes.load(Ordering::Relaxed));
        self.machine.lock().unwrap().status(bytes, self.trace.snapshot())
    }

    fn emit_state(&self) {
        self.emit(RecorderEvent::State(self.status()));
    }

    fn control(&self) -> Option<Arc<Control>> {
        self.control.lock().unwrap().clone()
    }

    /// Waits for the countdown to finish or for the start to be called off.
    fn run_countdown(self: &Arc<Self>, hook: &CountdownHook, target: &RecordTarget) {
        let finished = Arc::new(Mutex::new(false));
        let done: Once<()> = {
            let finished = Arc::clone(&finished);
            let inner = Arc::clone(self);
            Box::new(move |()| {
                *finished.lock().unwrap() = true;
                // through the machine's lock, so the waiter below cannot miss it between check and wait
                let _guard = inner.machine.lock().unwrap();
                inner.wake.notify_all();
            })
        };
        hook(target, done);
        let mut machine = self.machine.lock().unwrap();
        while !*finished.lock().unwrap() && !machine.start_called_off() {
            machine = self.wake.wait(machine).unwrap();
        }
    }

    fn finish(&self, id: SessionId, outcome: Outcome) {
        let current = self.machine.lock().unwrap().settle(id);
        self.wake.notify_all();
        match outcome {
            Outcome::Done(done) => self.emit(RecorderEvent::Done(done)),
            Outcome::Canceled => {}
            Outcome::Failed(message) => self.emit(RecorderEvent::Error(message)),
        }
        if current {
            self.emit_state();
        }
    }

    /// The safety timeout. Lives for one session.
    fn watch(&self, id: SessionId) {
        let mut machine = self.machine.lock().unwrap();
        loop {
            if machine.current_id() != Some(id) {
                return;
            }
            let now = self.now_ms();
            if let Some(expired) = machine.expire(now) {
                drop(machine);
                self.wake.notify_all();
                self.trace.push(format!("finalize timed out canceled={}", expired.canceled));
                if !expired.canceled {
                    self.emit(RecorderEvent::Error(
                        "The recording did not finish in time. It will appear when finalizing completes.".into(),
                    ));
                }
                self.emit_state();
                return;
            }
            let wait = machine
                .deadline_ms()
                .map_or(Duration::from_secs(3600), |deadline| Duration::from_millis(deadline.saturating_sub(now)));
            machine = self.wake.wait_timeout(machine, wait).unwrap().0;
        }
    }
}

struct WorkerEvents {
    inner: Arc<Inner>,
    id: SessionId,
}

impl SessionEvents for WorkerEvents {
    fn ready(&self, audio: RecAudioInfo) {
        if self.inner.machine.lock().unwrap().set_ready(self.id, audio) {
            self.inner.emit_state();
        }
    }

    fn levels(&self, levels: RecLevels) {
        if self.inner.machine.lock().unwrap().set_levels(self.id, levels.clone()) {
            self.inner.emit(RecorderEvent::Levels(levels));
        }
    }

    fn problem(&self, message: String) {
        self.inner.emit(RecorderEvent::Error(message));
    }
}

/// Held by a session's worker for as long as it runs, however it ends.
struct UnfinishedFile(Arc<Inner>);

impl Drop for UnfinishedFile {
    fn drop(&mut self) {
        self.0.unfinished.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A start that returns early must not leave the machine in `Starting`.
struct StartGuard<'a>(&'a Inner);

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        self.0.machine.lock().unwrap().abort_start();
    }
}

fn epoch_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

impl RecorderControl for Recorder {
    fn start(&self, target: RecordTarget, options: RecordOptions) -> Result<Option<RecStatus>> {
        let inner = &self.inner;
        if !inner.machine.lock().unwrap().begin() {
            return Ok(None);
        }
        let guard = StartGuard(inner);
        inner.trace.reset();
        fs::create_dir_all(&options.save_dir).map_err(|e| Error::io("create", &options.save_dir, e))?;

        let audio_only = matches!(target, RecordTarget::AudioOnly);
        if !audio_only {
            if let Some(hook) = &inner.countdown {
                inner.run_countdown(hook, &target);
            }
        }

        // in place before the session exists: a stop that comes right after must find it
        let control = Arc::new(Control::default());
        *inner.control.lock().unwrap() = Some(Arc::clone(&control));
        let start_ts = epoch_ms();
        let Some(id) = inner.machine.lock().unwrap().activate(audio_only, start_ts) else {
            return Ok(None);
        };
        drop(guard);

        let part = inner.temp_dir.join(format!("shotik-rec-{}-{start_ts}.part", std::process::id()));
        let save_base = options.save_dir.join(ts_name(&inner.backend.local_time()));
        let job = Job { target, options, part, save_base };

        // first in the listener's order, before the worker can report anything
        let status = inner.status();
        inner.emit(RecorderEvent::State(status.clone()));

        inner.unfinished.fetch_add(1, Ordering::SeqCst);
        let unfinished = UnfinishedFile(Arc::clone(inner));
        let worker = {
            let inner = Arc::clone(inner);
            thread::Builder::new().name("shotik-rec-session".into()).spawn(move || {
                let _unfinished = unfinished;
                let events = WorkerEvents { inner: Arc::clone(&inner), id };
                // a panic in a session must not leave the recorder busy for good
                let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                    engine::run_session(inner.backend.as_ref(), &job, &control, &inner.trace, &events)
                }))
                .unwrap_or_else(|_| Outcome::Failed("The recording stopped because of an internal error".into()));
                inner.finish(id, outcome);
            })
        };
        if let Err(e) = worker {
            inner.machine.lock().unwrap().settle(id);
            inner.emit_state();
            return Err(Error::msg(format!("Cannot start the recording thread: {e}")));
        }
        let watchdog = {
            let inner = Arc::clone(inner);
            thread::Builder::new().name("shotik-rec-watchdog".into()).spawn(move || inner.watch(id))
        };
        if let Err(e) = watchdog {
            inner.trace.push(format!("no finalize watchdog: {e}"));
        }
        Ok(Some(status))
    }

    fn stop(&self) {
        let inner = &self.inner;
        let request = inner.machine.lock().unwrap().request_stop(inner.now_ms());
        inner.wake.notify_all();
        if let (Request::Stop(_), Some(control)) = (request, inner.control()) {
            control.request_stop(inner.backend.now());
        }
    }

    fn pause(&self) {
        let inner = &self.inner;
        let changed = inner.machine.lock().unwrap().pause();
        if let (Some(_), Some(control)) = (changed, inner.control()) {
            control.pause(inner.backend.now());
            inner.emit_state();
        }
    }

    fn resume(&self) {
        let inner = &self.inner;
        let changed = inner.machine.lock().unwrap().resume();
        if let (Some(_), Some(control)) = (changed, inner.control()) {
            control.resume(inner.backend.now());
            inner.emit_state();
        }
    }

    fn cancel(&self) {
        let inner = &self.inner;
        let request = inner.machine.lock().unwrap().request_cancel(inner.now_ms());
        inner.wake.notify_all();
        if let (Request::Cancel(_), Some(control)) = (request, inner.control()) {
            control.request_cancel();
        }
    }

    fn is_recording(&self) -> bool {
        self.inner.machine.lock().unwrap().is_recording()
    }

    fn has_unfinished_file(&self) -> bool {
        self.inner.unfinished.load(Ordering::SeqCst) > 0
    }

    fn status(&self) -> RecStatus {
        self.inner.status()
    }

    fn set_listener(&self, events: Callback<RecorderEvent>) {
        *self.inner.listener.lock().unwrap() = Some(events);
    }
}
