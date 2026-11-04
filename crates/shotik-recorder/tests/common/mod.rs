//! What the file tests share: an independent oracle (ffprobe, ffmpeg and the Windows player engine,
//! never this crate) and a recorder with its own temporary directories.
#![allow(dead_code)]

mod handles;

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use shotik_core::ports::{RecordOptions, RecordTarget, RecorderControl, RecorderEvent, RecordingDone};
use shotik_core::Rect;
use shotik_recorder::{CaptureMode, Recorder, RecorderConfig, SynthOptions};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount, GetProcessTimes};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Timing assertions do not hold when two encoders run at once on a machine doing other work.
pub fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn find_named(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_named(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|f| f == name) {
            return Some(path);
        }
    }
    None
}

fn tool(name: &str) -> PathBuf {
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>());
    if let Some(found) = on_path.map(|dir| dir.join(name)).find(|file| file.is_file()) {
        return found;
    }
    let packages = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap()).join("Microsoft/WinGet/Packages");
    let mut dirs: Vec<PathBuf> = fs::read_dir(&packages)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.file_name().is_some_and(|f| f.to_string_lossy().starts_with("Gyan.FFmpeg")))
        .collect();
    dirs.sort();
    dirs.iter().find_map(|dir| find_named(dir, name)).unwrap_or_else(|| panic!("{name} is neither on PATH nor under {}", packages.display()))
}

fn ffprobe_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| tool("ffprobe.exe"))
}

fn ffmpeg_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| tool("ffmpeg.exe"))
}

fn run(mut command: Command) -> Vec<u8> {
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command.output().unwrap_or_else(|e| panic!("{command:?}: {e}"));
    assert!(output.status.success(), "{command:?}: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

/// `ffprobe -show_streams -show_format` with frames really counted.
pub fn probe(file: &Path) -> Value {
    let mut c = Command::new(ffprobe_path());
    c.args(["-v", "error", "-count_frames", "-count_packets", "-show_streams", "-show_format", "-of", "json"]).arg(file);
    serde_json::from_slice(&run(c)).unwrap()
}

pub fn stream<'a>(info: &'a Value, kind: &str) -> Option<&'a Value> {
    info["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == kind)
}

pub fn seconds(v: &Value) -> f64 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("not a number: {v}"))
}

pub fn count(v: &Value) -> u64 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("not a count: {v}"))
}

/// The decoded audio, interleaved stereo at 48 kHz.
pub fn decode_pcm(file: &Path) -> Vec<f32> {
    let mut c = Command::new(ffmpeg_path());
    c.args(["-v", "error", "-i"]).arg(file).args(["-vn", "-f", "f32le", "-acodec", "pcm_f32le", "-ac", "2", "-ar", "48000", "-"]);
    run(c).chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// The mean luma of each decoded frame, with its time.
pub fn frame_luma(file: &Path) -> Vec<(f64, f64)> {
    let mut c = Command::new(ffmpeg_path());
    c.args(["-v", "error", "-i"]).arg(file).args([
        "-an", "-vf", "scale=8:8,format=gray", "-fps_mode", "passthrough", "-f", "rawvideo", "-",
    ]);
    let bytes = run(c);
    let mut p = Command::new(ffprobe_path());
    p.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "frame=pts_time", "-of", "csv=p=0"]).arg(file);
    let times: Vec<f64> = String::from_utf8(run(p))
        .unwrap()
        .lines()
        .filter_map(|l| l.trim().trim_end_matches(',').parse().ok())
        .collect();
    let lumas: Vec<f64> = bytes.chunks_exact(64).map(|f| f.iter().map(|&b| f64::from(b)).sum::<f64>() / 64.0).collect();
    assert_eq!(times.len(), lumas.len(), "ffprobe and ffmpeg disagree on the frames");
    times.into_iter().zip(lumas).collect()
}

/// One frame as RGB24.
pub fn frame_rgb(file: &Path, at_sec: f64, width: u32, height: u32) -> Vec<u8> {
    let mut c = Command::new(ffmpeg_path());
    c.args(["-v", "error", "-ss", &at_sec.to_string(), "-i"]).arg(file).args([
        "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-",
    ]);
    let out = run(c);
    assert_eq!(out.len(), (width * height * 3) as usize);
    out
}

/// Amplitude of a tone in one channel of a stretch of the audio, as `decodeAudio` in `test/ghost/media.js`.
pub fn amplitude(stereo: &[f32], start_sec: f64, len_sec: f64, hz: f64) -> f64 {
    let first = (start_sec * 48_000.0) as usize;
    let last = ((start_sec + len_sec) * 48_000.0) as usize;
    let (mut re, mut im, mut n) = (0.0, 0.0, 0_usize);
    for i in first..last.min(stereo.len() / 2) {
        let angle = std::f64::consts::TAU * hz * i as f64 / 48_000.0;
        let s = f64::from(stereo[i * 2]);
        re += s * angle.cos();
        im += s * angle.sin();
        n += 1;
    }
    assert!(n > 0, "no samples at {start_sec}");
    2.0 * f64::hypot(re, im) / n as f64
}

/// Root mean square of successive stretches of one channel.
pub fn rms_windows(stereo: &[f32], window_ms: usize) -> Vec<f64> {
    let frames = 48 * window_ms;
    stereo
        .chunks_exact(frames * 2)
        .map(|w| {
            let sum: f64 = w.iter().step_by(2).map(|&s| f64::from(s) * f64::from(s)).sum();
            (sum / frames as f64).sqrt()
        })
        .collect()
}

/// What a player built on Media Foundation sees: `test/ghost/mf-probe.ps1`.
pub fn media_foundation(file: &Path, seek_to: f64) -> Value {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/ghost/mf-probe.ps1");
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(script).arg("-Path").arg(file);
    c.args(["-SeekTo", &seek_to.to_string()]);
    let out = String::from_utf8(run(c)).unwrap();
    serde_json::from_str(out.trim().lines().last().unwrap()).unwrap()
}

/// CPU time of this process so far, milliseconds.
pub fn cpu_ms() -> f64 {
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: the out values are locals, the handle is the pseudo handle of this process.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.unwrap();
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    (ticks(kernel) + ticks(user)) as f64 / 10_000.0
}

pub fn private_mb() -> f64 {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    // SAFETY: the struct is a local with its size passed; the EX form starts with the plain one.
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::from_mut(&mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    }
    .unwrap();
    counters.PrivateUsage as f64 / 1_048_576.0
}

pub fn handle_count() -> u32 {
    let mut count = 0;
    // SAFETY: the out value is a local, the handle is the pseudo handle of this process.
    unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }.unwrap();
    count
}

#[allow(unused_imports)]
pub use handles::{diff_types, handle_types};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// A recorder over its own directories, deleted on drop, with real media in them or not.
pub struct Bench {
    pub recorder: Recorder,
    pub events: Receiver<RecorderEvent>,
    pub dir: PathBuf,
    pub save_dir: PathBuf,
    pub temp_dir: PathBuf,
}

impl Bench {
    pub fn new(capture: CaptureMode) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "shotik-recorder-files-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::SeqCst)
        ));
        let (save_dir, temp_dir) = (dir.join("saved"), dir.join("temp"));
        fs::create_dir_all(&temp_dir).unwrap();
        let mut config = RecorderConfig::new(capture);
        config.temp_dir = temp_dir.clone();
        let recorder = Recorder::new(config).unwrap();
        let (tx, events) = channel();
        let tx = Mutex::new(tx);
        recorder.set_listener(std::sync::Arc::new(move |event| {
            let _ = tx.lock().unwrap().send(event);
        }));
        Self { recorder, events, dir, save_dir, temp_dir }
    }

    pub fn synthetic(options: SynthOptions) -> Self {
        Self::new(CaptureMode::Synthetic(options))
    }

    pub fn options(&self, fps: u32, system: bool, microphone: bool) -> RecordOptions {
        RecordOptions {
            fps,
            format: "auto".into(),
            system_audio: system,
            microphone,
            microphone_device_id: "default".into(),
            save_dir: self.save_dir.clone(),
        }
    }

    pub fn wait_for(&self, what: &str, timeout: Duration, matches: impl Fn(&RecorderEvent) -> bool) -> RecorderEvent {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(event) if matches(&event) => return event,
                Ok(RecorderEvent::Error(message)) => panic!("error while waiting for {what}: {message}, {:?}", self.recorder.status()),
                Ok(_) => {}
                Err(_) => panic!("no {what} in {timeout:?}: {:?}", self.recorder.status()),
            }
        }
    }

    /// Until capture really runs. Returns how long that took.
    pub fn wait_running(&self) {
        self.wait_for("capture", Duration::from_secs(15), |e| matches!(e, RecorderEvent::State(s) if s.audio.is_some()));
    }

    pub fn wait_done(&self) -> RecordingDone {
        let event = self.wait_for("the finished file", Duration::from_secs(30), |e| matches!(e, RecorderEvent::Done(_)));
        let RecorderEvent::Done(done) = event else { unreachable!() };
        self.wait_for("idle", Duration::from_secs(5), |e| matches!(e, RecorderEvent::State(s) if !s.recording));
        done
    }

    pub fn trace(&self) -> Vec<String> {
        self.recorder.status().trace
    }

    pub fn temp_files(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.temp_dir).unwrap().flatten().map(|e| e.path()).collect()
    }

    pub fn saved_files(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.save_dir).map_or(Vec::new(), |d| d.flatten().map(|e| e.path()).collect())
    }

    /// Records for `secs` and returns the result.
    pub fn record(&self, target: RecordTarget, options: RecordOptions, secs: f64) -> RecordingDone {
        self.recorder.start(target, options).unwrap().expect("no other session");
        self.wait_running();
        std::thread::sleep(Duration::from_secs_f64(secs));
        self.recorder.stop();
        self.wait_done()
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        self.recorder.cancel();
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.recorder.is_recording() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub fn region(x: i32, y: i32, width: i32, height: i32, scale: f64) -> RecordTarget {
    RecordTarget::Region {
        display_id: 0,
        rect_phys: Rect::new(x, y, width, height),
        rect_dip: Rect::new(
            (f64::from(x) / scale).round() as i32,
            (f64::from(y) / scale).round() as i32,
            (f64::from(width) / scale).round() as i32,
            (f64::from(height) / scale).round() as i32,
        ),
    }
}
