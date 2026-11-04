//! One recording session from opening the inputs to the finished file. Each stage runs on its own
//! thread: frames and audio go through a bounded queue to the writer, so a slow encoder delays the
//! stamps of nothing, and every thread reads the same timeline.
//!
//! The order of opening and the errors follow `__shotikStart` in `src/recorder/recorder.js`.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use shotik_core::ports::{RecAudioInfo, RecLevels, RecordOptions, RecordTarget, RecordingDone};

use crate::backend::{
    AudioInput, AudioOpenError, AudioPacket, Backend, MediaSink, MicDevice, SinkRequest, Snapshot, ThreadRole,
    VideoRequest, VideoSource,
};
use crate::clock::{FramePacer, Piece, Timeline, TICKS_PER_SEC};
use crate::convert::bgra_to_jpeg;
use crate::mix::{frames_to_ticks, ticks_to_frames, to_pcm16, InputTrack, PeakMeter, TrackStats, CHANNELS, SAMPLE_RATE};
use crate::names::move_into_place;
use crate::trace::Trace;

/// The mixer stays this far behind real time, so a late packet still finds its place.
const MIX_DELAY_FRAMES: i64 = SAMPLE_RATE * 120 / 1000;
/// After a stop: how long to wait for the last packets before the rest of the tail is silence.
const TAIL_WAIT_TICKS: i64 = TICKS_PER_SEC * 60 / 1000;
const MIX_PERIOD: Duration = Duration::from_millis(10);
/// 20 ms of audio per sample written.
const AUDIO_CHUNK_FRAMES: i64 = SAMPLE_RATE / 50;
const LEVEL_PERIOD_TICKS: i64 = TICKS_PER_SEC / 10;
/// The shortest audio-only file: a stop right after the start still gives a valid one.
const MIN_AUDIO_ONLY_TICKS: i64 = TICKS_PER_SEC / 10;
const QUEUE_DEPTH: usize = 16;

pub(crate) struct Job {
    pub target: RecordTarget,
    pub options: RecordOptions,
    /// The temporary file being written.
    pub part: PathBuf,
    /// `save_dir/Rec_<date>_<time>`, without the extension.
    pub save_base: PathBuf,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    Done(RecordingDone),
    Canceled,
    /// For the user, with one of the known prefixes when it applies.
    Failed(String),
}

/// What a running session reports on the way.
pub(crate) trait SessionEvents: Send + Sync {
    fn ready(&self, audio: RecAudioInfo);
    fn levels(&self, levels: RecLevels);
    /// Something the user must know about while the session goes on.
    fn problem(&self, message: String);
}

#[derive(Debug, Default)]
struct TimeState {
    timeline: Timeline,
    paused: bool,
}

/// Shared by all threads of a session and by whoever controls it.
#[derive(Debug, Default)]
pub(crate) struct Control {
    time: Mutex<TimeState>,
    stop: AtomicBool,
    cancel: AtomicBool,
    /// Media length of the file: known once the video thread has written its last frame, or in
    /// audio-only sessions once a stop arrived.
    end: Mutex<Option<i64>>,
    mix_done: AtomicBool,
    failure: Mutex<Option<String>>,
    pub bytes: AtomicU64,
}

impl Control {
    pub fn request_stop(&self, now: i64) {
        let mut time = self.time.lock().unwrap();
        self.stop.store(true, Ordering::SeqCst);
        if time.timeline.is_started() {
            time.timeline.stop(now);
        }
    }

    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn pause(&self, now: i64) {
        let mut time = self.time.lock().unwrap();
        time.paused = true;
        if time.timeline.is_started() {
            time.timeline.pause(now);
        }
    }

    pub fn resume(&self, now: i64) {
        let mut time = self.time.lock().unwrap();
        time.paused = false;
        if time.timeline.is_started() {
            time.timeline.resume(now);
        }
    }

    /// The timeline starts here. What was asked for while the session was starting applies now.
    fn begin_timeline(&self, now: i64) {
        let mut time = self.time.lock().unwrap();
        time.timeline.start(now);
        if time.paused {
            time.timeline.pause(now);
        }
        if self.stop.load(Ordering::SeqCst) {
            time.timeline.stop(now);
        }
    }

    #[cfg(test)]
    fn position(&self, now: i64) -> Option<i64> {
        self.time.lock().unwrap().timeline.position(now)
    }

    fn view(&self, now: i64) -> (Option<i64>, bool) {
        let time = self.time.lock().unwrap();
        (time.timeline.position(now), time.timeline.is_stopped())
    }

    fn place(&self, at: i64, len: i64, pieces: &mut Vec<Piece>) {
        pieces.clear();
        self.time.lock().unwrap().timeline.place(at, len, |piece| pieces.push(piece));
    }

    fn is_canceled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// A stop or cancel arrived. Before capture runs both mean nothing to keep.
    fn is_ending(&self) -> bool {
        self.is_canceled() || self.stop.load(Ordering::SeqCst)
    }

    fn end(&self) -> Option<i64> {
        *self.end.lock().unwrap()
    }

    fn set_end(&self, ticks: i64) {
        self.end.lock().unwrap().get_or_insert(ticks);
    }

    fn fail(&self, message: String, now: i64) {
        self.failure.lock().unwrap().get_or_insert(message);
        self.request_stop(now);
    }
}

enum Packet {
    Video { frame: Box<dyn Any + Send>, pts: i64, duration: i64 },
    Audio { pcm: Vec<i16>, pts: i64, duration: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Input {
    System,
    Microphone,
}

impl Input {
    fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Microphone => "microphone",
        }
    }

    /// What the user hears about an input that stopped delivering in the middle of a session.
    fn lost_message(self) -> &'static str {
        match self {
            Self::System => "audio:system-unavailable",
            Self::Microphone => "microphone:NotFoundError",
        }
    }
}

struct TrackShared {
    input: Input,
    track: Mutex<InputTrack>,
    meter: PeakMeter,
}

#[derive(Clone, Copy, Debug, Default)]
struct VideoStats {
    frames: u64,
    fresh: u64,
    repeated: u64,
    /// Frames written later than the next one was due: the thread or the encoder fell behind.
    late: u64,
}

fn mime_of(has_video: bool, has_audio: bool) -> &'static str {
    match (has_video, has_audio) {
        (false, _) => "audio/mp4;codecs=mp4a.40.2",
        (true, false) => "video/mp4;codecs=h264",
        (true, true) => "video/mp4;codecs=h264,mp4a.40.2",
    }
}

/// An abandoned sink writer lets go of its file with a small delay.
fn remove_part(part: &Path, trace: &Trace) {
    for attempt in 0..50 {
        match std::fs::remove_file(part) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if attempt == 49 => trace.push(format!("cannot remove {}: {e}", part.display())),
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Whatever happens, the temporary file does not stay behind unless it became the result.
pub(crate) fn run_session(
    backend: &dyn Backend,
    job: &Job,
    ctl: &Control,
    trace: &Trace,
    events: &dyn SessionEvents,
) -> Outcome {
    let _thread = backend.enter_thread(ThreadRole::Worker);
    let outcome = drive(backend, job, ctl, trace, events);
    if !matches!(outcome, Outcome::Done(_)) {
        remove_part(&job.part, trace);
    }
    trace.push(format!(
        "finalize bytes={} canceled={} partExists={}",
        ctl.bytes.load(Ordering::Relaxed),
        matches!(outcome, Outcome::Canceled),
        job.part.exists()
    ));
    outcome
}

fn open_system(
    backend: &dyn Backend,
    audio_only: bool,
    trace: &Trace,
    events: &dyn SessionEvents,
) -> Result<Option<Box<dyn AudioInput>>, Outcome> {
    match backend.open_system_audio(trace) {
        Ok(input) => Ok(Some(input)),
        Err(AudioOpenError { kind, detail }) => {
            trace.push(format!("system audio: {} {detail}", kind.name()));
            if audio_only {
                return Err(Outcome::Failed("audio:system-unavailable".into()));
            }
            // A video session goes on without it, as the JS version does, and says so.
            events.problem("audio:system-unavailable".into());
            Ok(None)
        }
    }
}

fn drive(backend: &dyn Backend, job: &Job, ctl: &Control, trace: &Trace, events: &dyn SessionEvents) -> Outcome {
    let options = &job.options;
    let audio_only = matches!(job.target, RecordTarget::AudioOnly);
    trace.push(format!(
        "start; audio={} microphone={} audioOnly={} fps={} format={}",
        options.system_audio, options.microphone, audio_only, options.fps, options.format
    ));
    let _session = match backend.session_guard() {
        Ok(guard) => guard,
        Err(e) => return Outcome::Failed(e.to_string()),
    };

    let fps = if options.fps == 0 { 30 } else { options.fps.min(240) };
    let mut video = None;
    if let RecordTarget::Region { display_id, rect_phys, .. } = &job.target {
        let request = VideoRequest { display_id: *display_id, rect_phys: *rect_phys, fps };
        match backend.open_video(&request, trace) {
            Ok(source) => video = Some(source),
            Err(e) => return Outcome::Failed(e.to_string()),
        }
    }
    if ctl.is_ending() {
        return Outcome::Canceled;
    }

    let mut system = None;
    if options.system_audio {
        system = match open_system(backend, audio_only, trace, events) {
            Ok(input) => input,
            Err(outcome) => return outcome,
        };
    }
    if ctl.is_ending() {
        return Outcome::Canceled;
    }

    let mut microphone = None;
    if options.microphone {
        match backend.open_microphone(&MicDevice::from_setting(&options.microphone_device_id), trace) {
            Ok(input) => microphone = Some(input),
            Err(AudioOpenError { kind, detail }) => {
                trace.push(format!("microphone: {} {detail}", kind.name()));
                return Outcome::Failed(format!("microphone:{}", kind.name()));
            }
        }
    }
    if ctl.is_ending() {
        return Outcome::Canceled;
    }
    trace.push(format!(
        "got source stream, vtracks={} atracks={}",
        u32::from(video.is_some()),
        u32::from(system.is_some())
    ));

    let audio_info = RecAudioInfo {
        system: system.is_some(),
        microphone: microphone.is_some(),
        tracks: u32::from(system.is_some() || microphone.is_some()),
    };
    if audio_only && audio_info.tracks == 0 {
        return Outcome::Failed("audio:none".into());
    }

    let has_audio = audio_info.tracks > 0;
    let request = SinkRequest { path: job.part.clone(), audio: has_audio };
    let opened = match &mut video {
        Some(source) => source.open_sink(&request, trace),
        None => backend.open_audio_sink(&request, trace),
    };
    let sink = match opened {
        Ok(sink) => sink,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    if ctl.is_ending() {
        return Outcome::Canceled;
    }

    let mut snapshot = None;
    if let Some(source) = &mut video {
        match source.wait_first_frame(&|| ctl.is_ending()) {
            Ok(Some(first)) => snapshot = Some(first),
            Ok(None) => return Outcome::Canceled,
            Err(e) => return Outcome::Failed(e.to_string()),
        }
    }
    trace.push(format!("recorder.start {}", mime_of(video.is_some(), has_audio)));

    let (thumb, sink) = run_streams(backend, ctl, trace, events, video, system, microphone, sink, snapshot, fps, audio_info);

    if ctl.is_canceled() {
        return Outcome::Canceled;
    }
    let bytes = sink.bytes_written();
    ctl.bytes.store(bytes, Ordering::Relaxed);
    let failure = ctl.failure.lock().unwrap().take();
    let finished = match sink.finalize() {
        Ok(problem) => problem,
        Err(e) => return Outcome::Failed(failure.unwrap_or_else(|| e.to_string())),
    };
    // Cancel can arrive while the index is being written.
    if ctl.is_canceled() {
        return Outcome::Canceled;
    }
    // What was written up to a failure is a valid file: keep it and say what happened.
    if let Some(message) = failure.or(finished) {
        events.problem(message);
    }

    let duration_sec = ctl.end().unwrap_or(0) as f64 / TICKS_PER_SEC as f64;
    trace.push(format!("index ok mp4 duration={duration_sec:.3}"));
    let ext = if audio_only { "m4a" } else { "mp4" };
    let file = match move_into_place(&job.part, &job.save_base, ext) {
        Ok(file) => file,
        // The folder is gone or full, but the recording is complete and this is its only copy. It stays in
        // the temporary folder under its real name, and the user is told where.
        Err(e) => {
            let kept = job.save_base.file_name().map_or_else(|| job.part.clone(), |name| job.part.with_file_name(name));
            match move_into_place(&job.part, &kept, ext) {
                Ok(file) => {
                    events.problem(format!("{e}. The recording is in {}", file.display()));
                    file
                }
                Err(_) => return Outcome::Failed(e.to_string()),
            }
        }
    };
    let (width, height) = match &job.target {
        RecordTarget::Region { rect_dip, .. } => (rect_dip.width.max(0) as u32, rect_dip.height.max(0) as u32),
        RecordTarget::AudioOnly => (0, 0),
    };
    Outcome::Done(RecordingDone { file, audio_only, first_frame_jpeg: thumb, width, height, duration_sec })
}

#[allow(clippy::too_many_arguments)]
fn run_streams(
    backend: &dyn Backend,
    ctl: &Control,
    trace: &Trace,
    events: &dyn SessionEvents,
    video: Option<Box<dyn VideoSource>>,
    system: Option<Box<dyn AudioInput>>,
    microphone: Option<Box<dyn AudioInput>>,
    sink: Box<dyn MediaSink>,
    snapshot: Option<Snapshot>,
    fps: u32,
    audio_info: RecAudioInfo,
) -> (Option<Vec<u8>>, Box<dyn MediaSink>) {
    let has_video = video.is_some();
    let mut inputs = Vec::new();
    if let Some(input) = system {
        inputs.push((Input::System, input));
    }
    if let Some(input) = microphone {
        inputs.push((Input::Microphone, input));
    }
    let tracks: Vec<Arc<TrackShared>> = inputs
        .iter()
        .map(|(input, _)| Arc::new(TrackShared { input: *input, track: Mutex::default(), meter: PeakMeter::default() }))
        .collect();

    thread::scope(|scope| {
        let (tx, rx) = sync_channel::<Packet>(QUEUE_DEPTH);
        let writer = thread::Builder::new()
            .name("shotik-rec-writer".into())
            .spawn_scoped(scope, move || writer_loop(backend, sink, rx, ctl))
            .expect("the system refused to start a thread");
        let thumb = snapshot.map(|shot| {
            thread::Builder::new()
                .name("shotik-rec-thumb".into())
                .spawn_scoped(scope, move || match bgra_to_jpeg(&shot.bgra, shot.stride, shot.width, shot.height) {
                    Ok(jpeg) => Some(jpeg),
                    Err(e) => {
                        trace.push(format!("thumbnail failed: {e}"));
                        None
                    }
                })
                .expect("the system refused to start a thread")
        });

        let mut capture = Vec::new();
        for ((input, source), shared) in inputs.into_iter().zip(&tracks) {
            let shared = Arc::clone(shared);
            let name = format!("shotik-rec-{}", input.name());
            capture.push(
                thread::Builder::new()
                    .name(name)
                    .spawn_scoped(scope, move || input_loop(backend, ctl, source, &shared, trace, events))
                    .expect("the system refused to start a thread"),
            );
        }
        let mixer = (!tracks.is_empty()).then(|| {
            let tx = tx.clone();
            let tracks = tracks.clone();
            thread::Builder::new()
                .name("shotik-rec-mix".into())
                .spawn_scoped(scope, move || mix_loop(backend, ctl, &tx, &tracks, has_video, events))
                .expect("the system refused to start a thread")
        });
        let picture = video.map(|source| {
            let tx = tx.clone();
            thread::Builder::new()
                .name("shotik-rec-video".into())
                .spawn_scoped(scope, move || video_loop(backend, ctl, source, &tx, fps))
                .expect("the system refused to start a thread")
        });
        drop(tx);

        // Everything is open: this is time zero of the file.
        ctl.begin_timeline(backend.now());
        events.ready(audio_info);

        if let Some(picture) = picture {
            let stats = picture.join().expect("the video thread panicked");
            trace.push(format!(
                "video frames={} fresh={} repeated={} late={}",
                stats.frames, stats.fresh, stats.repeated, stats.late
            ));
        }
        if let Some(mixer) = mixer {
            mixer.join().expect("the mixer thread panicked");
        }
        ctl.mix_done.store(true, Ordering::SeqCst);
        for handle in capture {
            handle.join().expect("an audio thread panicked");
        }
        for shared in &tracks {
            let stats: TrackStats = shared.track.lock().unwrap().stats;
            trace.push(format!(
                "audio {} packets={} gap={} late={} slipped={}",
                shared.input.name(),
                stats.packets,
                stats.gap_frames,
                stats.late_frames,
                stats.slipped_frames
            ));
        }
        let sink = writer.join().expect("the writer thread panicked");
        let thumb = thumb.and_then(|handle| handle.join().expect("the thumbnail thread panicked"));
        (thumb, sink)
    })
}

fn video_loop(
    backend: &dyn Backend,
    ctl: &Control,
    source: Box<dyn VideoSource>,
    tx: &SyncSender<Packet>,
    fps: u32,
) -> VideoStats {
    let _thread = backend.enter_thread(ThreadRole::Video);
    let mut source = source;
    let mut pacer = FramePacer::new(fps);
    let mut stats = VideoStats::default();
    loop {
        if ctl.is_canceled() {
            return stats;
        }
        let now = backend.now();
        let (position, stopping) = ctl.view(now);
        let Some(position) = position else {
            thread::sleep(Duration::from_millis(1));
            continue;
        };
        let due = pacer.due_count(position);
        stats.late += due.saturating_sub(pacer.written() + 1);
        while pacer.written() < due {
            match source.next_frame() {
                Ok(frame) => {
                    if frame.fresh {
                        stats.fresh += 1;
                    } else {
                        stats.repeated += 1;
                    }
                    let n = pacer.written();
                    let packet = Packet::Video { frame: frame.payload, pts: pacer.pts(n), duration: pacer.duration(n) };
                    if tx.send(packet).is_err() {
                        ctl.set_end(pacer.pts(pacer.written()));
                        return stats;
                    }
                    pacer.advance();
                    stats.frames += 1;
                }
                Err(e) => {
                    ctl.fail(e.to_string(), backend.now());
                    ctl.set_end(pacer.pts(pacer.written()));
                    return stats;
                }
            }
        }
        if stopping {
            ctl.set_end(pacer.pts(pacer.written()));
            return stats;
        }
        if let Some(reason) = source.lost() {
            ctl.fail(reason, backend.now());
            continue;
        }
        let until_next = pacer.pts(pacer.written()) - position;
        let wait = until_next.clamp(TICKS_PER_SEC / 1000, TICKS_PER_SEC / 100);
        thread::sleep(Duration::from_nanos(wait as u64 * 100));
    }
}

fn input_loop(
    backend: &dyn Backend,
    ctl: &Control,
    source: Box<dyn AudioInput>,
    shared: &TrackShared,
    trace: &Trace,
    events: &dyn SessionEvents,
) {
    let _thread = backend.enter_thread(ThreadRole::AudioCapture);
    let mut source = source;
    let mut pieces = Vec::new();
    let mut lost = false;
    while !ctl.mix_done.load(Ordering::SeqCst) && !ctl.is_canceled() {
        if lost {
            thread::sleep(MIX_PERIOD);
            continue;
        }
        let result = source.read(&mut |packet: AudioPacket<'_>| {
            shared.meter.update(packet.samples);
            let frames = packet.samples.len() / CHANNELS;
            ctl.place(packet.at, frames_to_ticks(frames as i64), &mut pieces);
            if pieces.is_empty() {
                return;
            }
            let mut track = shared.track.lock().unwrap();
            for piece in &pieces {
                let first = ticks_to_frames(piece.offset) as usize;
                let last = (first + ticks_to_frames(piece.len) as usize).min(frames);
                if first < last {
                    track.push(ticks_to_frames(piece.media), &packet.samples[first * CHANNELS..last * CHANNELS]);
                }
            }
        });
        if let Err(e) = result {
            lost = true;
            trace.push(format!("{} input lost: {e}", shared.input.name()));
            events.problem(shared.input.lost_message().into());
        }
    }
}

fn levels_of(tracks: &[Arc<TrackShared>]) -> RecLevels {
    let mut levels = RecLevels::default();
    for shared in tracks {
        match shared.input {
            Input::System => levels.system = shared.meter.take(),
            Input::Microphone => levels.microphone = shared.meter.take(),
        }
    }
    levels
}

fn mix_loop(
    backend: &dyn Backend,
    ctl: &Control,
    tx: &SyncSender<Packet>,
    tracks: &[Arc<TrackShared>],
    has_video: bool,
    events: &dyn SessionEvents,
) {
    let _thread = backend.enter_thread(ThreadRole::AudioMix);
    let mut cursor = 0_i64;
    let mut mixed = Vec::new();
    let mut last_levels = backend.now();
    let mut tail_deadline = None;
    loop {
        if ctl.is_canceled() {
            return;
        }
        let now = backend.now();
        if now - last_levels >= LEVEL_PERIOD_TICKS {
            last_levels = now;
            events.levels(levels_of(tracks));
        }
        let (position, stopped) = ctl.view(now);
        let Some(position) = position else {
            thread::sleep(MIX_PERIOD);
            continue;
        };
        if !has_video && stopped {
            ctl.set_end(position.max(MIN_AUDIO_ONLY_TICKS));
        }
        let (target, done) = match ctl.end() {
            None => (ticks_to_frames(position) - MIX_DELAY_FRAMES, false),
            Some(end) => {
                let end_frames = ticks_to_frames(end);
                let deadline = *tail_deadline.get_or_insert(now + (end - position).max(0) + TAIL_WAIT_TICKS);
                let have = tracks.iter().map(|s| s.track.lock().unwrap().tail().unwrap_or(0)).min().unwrap_or(0);
                if have >= end_frames || now >= deadline {
                    (end_frames, true)
                } else {
                    (have, false)
                }
            }
        };
        while cursor < target {
            let frames = (target - cursor).min(AUDIO_CHUNK_FRAMES);
            mixed.clear();
            mixed.resize(frames as usize * CHANNELS, 0.0_f32);
            for shared in tracks {
                shared.track.lock().unwrap().read_add(cursor, frames as usize, &mut mixed);
            }
            let mut pcm = Vec::new();
            to_pcm16(&mixed, &mut pcm);
            let pts = frames_to_ticks(cursor);
            let duration = frames_to_ticks(cursor + frames) - pts;
            if tx.send(Packet::Audio { pcm, pts, duration }).is_err() {
                return;
            }
            cursor += frames;
        }
        if done {
            return;
        }
        thread::sleep(MIX_PERIOD);
    }
}

fn writer_loop(
    backend: &dyn Backend,
    sink: Box<dyn MediaSink>,
    rx: Receiver<Packet>,
    ctl: &Control,
) -> Box<dyn MediaSink> {
    let _thread = backend.enter_thread(ThreadRole::Encode);
    let mut sink = sink;
    let mut failed = false;
    let mut since_stats = 0_u32;
    while let Ok(packet) = rx.recv() {
        // keep draining after a failure, so the producers never block on a full queue
        if failed || ctl.is_canceled() {
            continue;
        }
        let written = match packet {
            Packet::Video { frame, pts, duration } => sink.write_video(frame, pts, duration),
            Packet::Audio { pcm, pts, duration } => sink.write_audio(&pcm, pts, duration),
        };
        if let Err(e) = written {
            failed = true;
            ctl.fail(e.to_string(), backend.now());
        }
        since_stats += 1;
        if since_stats >= 32 {
            since_stats = 0;
            ctl.bytes.store(sink.bytes_written(), Ordering::Relaxed);
        }
    }
    sink
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_strings_keep_the_aac_form_the_ghost_tests_look_for() {
        assert_eq!(mime_of(false, true), "audio/mp4;codecs=mp4a.40.2");
        assert_eq!(mime_of(true, false), "video/mp4;codecs=h264");
        assert_eq!(mime_of(true, true), "video/mp4;codecs=h264,mp4a.40.2");
    }

    #[test]
    fn what_was_asked_during_the_start_applies_at_time_zero() {
        let ctl = Control::default();
        ctl.pause(5);
        ctl.begin_timeline(100);
        assert_eq!(ctl.position(200), Some(0), "still paused");
        ctl.resume(300);
        assert_eq!(ctl.position(400), Some(100));
        ctl.request_stop(500);
        assert_eq!(ctl.position(900), Some(200));
    }

    #[test]
    fn a_stop_during_the_start_ends_the_timeline_at_zero() {
        let ctl = Control::default();
        ctl.request_stop(5);
        assert!(ctl.is_ending());
        ctl.begin_timeline(100);
        assert_eq!(ctl.position(900), Some(0));
    }

    #[test]
    fn the_first_failure_is_kept_and_the_length_is_set_once() {
        let ctl = Control::default();
        ctl.fail("first".into(), 1);
        ctl.fail("second".into(), 2);
        assert_eq!(ctl.failure.lock().unwrap().as_deref(), Some("first"));
        assert!(ctl.is_ending());
        ctl.set_end(10);
        ctl.set_end(20);
        assert_eq!(ctl.end(), Some(10));
    }
}
