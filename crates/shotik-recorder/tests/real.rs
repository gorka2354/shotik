//! Real screen capture and real system audio, run by hand with `--ignored`: they read the user's
//! screen and whatever plays. No window opens, the microphone is never touched, nobody looks at the
//! pictures, and all output stays in a temporary directory that is deleted before the test ends.
#![cfg(windows)]

mod common;

use std::os::windows::process::CommandExt;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::{amplitude, count, cpu_ms, decode_pcm, handle_count, media_foundation, private_mb, probe, region, seconds, serial, stream, Bench};
use shotik_core::ports::{Displays, RecordTarget, RecorderControl, RecorderEvent};
use shotik_core::{Display, PointF, Point, Rect};
use shotik_recorder::{CaptureMode, Recorder, SynthOptions};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::core::BOOL;

const MONITORINFOF_PRIMARY: u32 = 1;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Enough of `Displays` for the recorder: which monitor to capture.
struct TestDisplays(Vec<Display>);

unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..MONITORINFO::default() };
    // SAFETY: the system passes a live handle; `list` is the vector passed to EnumDisplayMonitors below.
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let r = info.rcMonitor;
            let bounds = Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top);
            (*(list.0 as *mut Vec<Display>)).push(Display {
                id: monitor.0 as u64,
                bounds,
                work_area: bounds,
                scale_factor: 1.0,
                physical_bounds: bounds,
                primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
    }
    true.into()
}

impl TestDisplays {
    fn new() -> Self {
        // SAFETY: a process-wide setting in a test binary; a repeat fails harmlessly.
        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let mut list: Vec<Display> = Vec::new();
        // SAFETY: the callback runs during the call and `list` outlives it.
        let _ = unsafe { EnumDisplayMonitors(None, None, Some(collect), LPARAM(&raw mut list as isize)) };
        assert!(!list.is_empty());
        Self(list)
    }
}

impl Displays for TestDisplays {
    fn all(&self) -> Vec<Display> {
        self.0.clone()
    }
    fn primary(&self) -> Display {
        self.0.iter().find(|d| d.primary).unwrap_or(&self.0[0]).clone()
    }
    fn cursor(&self) -> PointF {
        PointF { x: 0.0, y: 0.0 }
    }
    fn nearest(&self, _point: PointF) -> Display {
        self.primary()
    }
    fn phys_to_dip(&self, point: Point) -> PointF {
        PointF { x: f64::from(point.x), y: f64::from(point.y) }
    }
}

/// A value for the report, when the machine has an NVIDIA GPU.
fn nvidia_smi(query: &str) -> String {
    std::process::Command::new("nvidia-smi")
        .args([format!("--query-gpu={query}").as_str(), "--format=csv,noheader"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_or_else(|e| e.to_string(), |out| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn real_bench() -> (Bench, Display) {
    let displays = TestDisplays::new();
    let primary = displays.primary();
    println!("primary display {:?}", primary.physical_bounds);
    (Bench::new(CaptureMode::Real { displays: Arc::new(displays) }), primary)
}

fn real_target(primary: &Display) -> RecordTarget {
    let mut target = region(100, 100, 1280, 720, 1.5);
    if let RecordTarget::Region { display_id, .. } = &mut target {
        *display_id = primary.id;
    }
    target
}

/// The numbers the Electron baseline has: start, CPU per second, stop, size per minute.
fn measure(bench: &Bench, target: RecordTarget, seconds_to_run: u64, system: bool) {
    // before the recording: nvidia-smi holds up the driver for long enough to make frames late
    println!("GPU busy before the recording: {}, encoding sessions {}", nvidia_smi("utilization.gpu"), nvidia_smi("encoder.stats.sessionCount"));
    let started = Instant::now();
    bench.recorder.start(target, bench.options(30, system, false)).unwrap().expect("no other session");
    bench.wait_running();
    let start_ms = started.elapsed().as_millis();
    let running = Instant::now();
    let cpu_before = cpu_ms();
    thread::sleep(Duration::from_secs(seconds_to_run));
    let cpu_after = cpu_ms();
    let wall = running.elapsed().as_secs_f64();
    let private_while_running = private_mb();
    let handles_while_running = handle_count();
    let stop = Instant::now();
    bench.recorder.stop();
    let done = bench.wait_done();
    let stop_ms = stop.elapsed().as_millis();

    for line in bench.trace() {
        println!("  {line}");
    }
    let size = std::fs::metadata(&done.file).unwrap().len() as f64;
    let info = probe(&done.file);
    println!(
        "start {start_ms} ms; CPU {:.0} ms/s; stop {stop_ms} ms; {:.1} MB/min; private {private_while_running:.0} MB; handles {handles_while_running}",
        (cpu_after - cpu_before) / wall,
        size / 1_048_576.0 / done.duration_sec * 60.0
    );
    if let Some(video) = stream(&info, "video") {
        // only that it is a picture: nobody looks at it
        let thumb = done.first_frame_jpeg.as_ref().expect("a thumbnail of the first frame");
        assert_eq!(&thumb[..2], &[0xFF, 0xD8]);
        assert_eq!(video["codec_name"], "h264");
        let frames = count(&video["nb_read_frames"]);
        let expected = wall * 30.0;
        println!("frames in the file {frames}, expected from the wall clock {expected:.1}");
        assert!((frames as f64 - expected).abs() < 4.0, "{frames} frames, expected {expected}");
        assert_eq!(video["avg_frame_rate"], "30/1", "{video}");
    }
    if system {
        // the device format as Windows reports it right now, and the proof its rate was handled: packets
        // placed by their stamps need no correction, and a wrong ratio would show as slips and gaps
        let trace = bench.trace();
        let device = trace.iter().find(|l| l.starts_with("system audio:")).cloned();
        println!("device: {device:?}");
        let stats = trace.iter().find(|l| l.starts_with("audio system")).expect("stats of the system input");
        assert!(stats.contains(" late=0 "), "{stats}");
        let slipped: u64 = stats.rsplit("slipped=").next().unwrap().parse().unwrap();
        assert!(slipped < 2000, "the rate is wrong if this many frames need correcting: {stats}");
        let sound = stream(&info, "audio").expect("an audio track, also when nothing plays");
        assert_eq!(sound["codec_name"], "aac");
        let length = seconds(&sound["duration"]);
        assert!((length - done.duration_sec).abs() < 0.1, "audio runs for the whole recording: {length} vs {}", done.duration_sec);
        // only how loud, never what: nobody listens to this
        let pcm = decode_pcm(&done.file);
        let peak = pcm.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
        println!("system audio peak {peak:.3}, 1 kHz band {:.4}", amplitude(&pcm, 0.0, 1.0, 1000.0));
    }
    let mf = media_foundation(&done.file, done.duration_sec * 0.75);
    println!("Media Foundation: {mf}");
    assert_eq!(mf["canSeek"], true, "{mf}");
    std::fs::remove_file(&done.file).unwrap();
}

/// One smoke test of the real path, with a game on the GPU when the machine happens to run one.
#[test]
#[ignore = "reads the real screen and real system audio"]
fn real_capture_and_loopback() {
    let _serial = serial();
    let (bench, primary) = real_bench();
    println!("-- video and system audio");
    measure(&bench, real_target(&primary), 15, true);
    println!("-- video only");
    measure(&bench, real_target(&primary), 5, false);
    println!("-- system audio only");
    measure(&bench, RecordTarget::AudioOnly, 5, true);
}

/// The scenario of `bench/run.js` against the synthetic source, like the Electron baseline.
#[test]
#[ignore = "takes 20 s, for numbers"]
fn bench_scenario_synthetic() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    measure(&bench, region(100, 100, 1280, 720, 1.5), 15, false);
}

fn one_cycle(recorder: &Recorder, rx: &std::sync::mpsc::Receiver<RecorderEvent>, target: RecordTarget, options: shotik_core::ports::RecordOptions) {
    recorder.start(target, options).unwrap().unwrap();
    let mut stopped = false;
    let mut file = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(RecorderEvent::State(s)) if s.audio.is_some() && !stopped => {
                thread::sleep(Duration::from_millis(700));
                recorder.stop();
                stopped = true;
            }
            Ok(RecorderEvent::Done(done)) => file = Some(done.file),
            Ok(RecorderEvent::State(s)) if !s.recording && stopped => break,
            Ok(RecorderEvent::Error(message)) => panic!("{message}"),
            _ => {}
        }
    }
    std::fs::remove_file(file.expect("a file")).unwrap();
}

fn census_line(census: &std::collections::BTreeMap<String, i64>) -> String {
    census.iter().map(|(name, count)| format!("{name}={count}")).collect::<Vec<_>>().join(" ")
}

/// The leak of the Electron build came from its capture path: handles per session must not grow. A
/// census by kernel object type, because a total hides what grows. `SHOTIK_CYCLES` for a longer run.
fn handle_cycles(name: &str, target: RecordTarget, capture: CaptureMode, system: bool, cycles: usize) {
    const WARM_UP: usize = 3;
    let bench = Bench::new(capture);
    let cycle = || one_cycle(&bench.recorder, &bench.events, target.clone(), bench.options(30, system, false));
    println!("-- {name}: {cycles} sessions");
    println!("before anything: handles {}, private {:.1} MB, {}", handle_count(), private_mb(), census_line(&common::handle_types()));
    for _ in 0..WARM_UP {
        cycle();
    }
    thread::sleep(Duration::from_millis(300));
    let first = common::handle_types();
    let (first_handles, first_private) = (handle_count(), private_mb());
    println!("after {WARM_UP} sessions: handles {first_handles}, private {first_private:.1} MB, {}", census_line(&first));
    for n in 0..cycles {
        cycle();
        if (n + 1) % 5 == 0 {
            println!("{} sessions: handles {}, private {:.1} MB", n + 1, handle_count(), private_mb());
        }
    }
    thread::sleep(Duration::from_millis(300));
    let last = common::handle_types();
    let (last_handles, last_private) = (handle_count(), private_mb());
    println!("after {cycles} more: handles {last_handles}, private {last_private:.1} MB, {}", census_line(&last));
    let growth = common::diff_types(&first, &last);
    let per_session = |delta: i64| delta as f64 / cycles as f64;
    println!("encoding sessions on the GPU with this process idle: {}", nvidia_smi("encoder.stats.sessionCount"));
    println!(
        "{name}: growth by type {growth:?}; handles {:+.2} per session, private {:+.3} MB per session",
        per_session(i64::from(last_handles) - i64::from(first_handles)),
        (last_private - first_private) / cycles as f64
    );
    for (kind, delta) in &growth {
        // ALPC is reported with its numbers: what Windows Graphics Capture does on its side of a session
        if kind != "ALPC Port" {
            assert!(per_session(*delta).abs() < 1.0, "{kind} grows {} per session", per_session(*delta));
        }
    }
    assert!((last_private - first_private) / (cycles as f64) < 0.5, "private {first_private} to {last_private} MB");
}

fn cycles_wanted(default: usize) -> usize {
    std::env::var("SHOTIK_CYCLES").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[test]
#[ignore = "reads the real screen and real system audio for about a minute"]
fn handles_stay_flat_across_real_sessions() {
    let _serial = serial();
    let displays = Arc::new(TestDisplays::new());
    let primary = displays.primary();
    let real = CaptureMode::Real { displays };
    handle_cycles("real capture and loopback", real_target(&primary), real, true, cycles_wanted(24));
}

#[test]
#[ignore = "reads real system audio for about half a minute"]
fn handles_stay_flat_across_real_audio_sessions() {
    let _serial = serial();
    let real = CaptureMode::Real { displays: Arc::new(TestDisplays::new()) };
    handle_cycles("real loopback only", RecordTarget::AudioOnly, real, true, cycles_wanted(24));
}

#[test]
#[ignore = "takes about a minute"]
fn handles_stay_flat_across_synthetic_sessions() {
    let _serial = serial();
    let synth = CaptureMode::Synthetic(SynthOptions::default());
    handle_cycles("synthetic video and audio", region(100, 100, 1280, 720, 1.5), synth, true, cycles_wanted(24));
}

/// Loopback delivers nothing while nothing plays, so this waits for something to play by itself, a game
/// or a browser, instead of playing anything on the user's speakers. Then the real format of the device
/// goes through the whole path: with a wrong rate the stamps and the frame count would disagree, and
/// the track would need gaps or slips to follow.
#[test]
#[ignore = "reads real system audio for up to ten minutes"]
fn real_loopback_with_sound_at_the_device_rate() {
    let _serial = serial();
    let displays = Arc::new(TestDisplays::new());
    let bench = Bench::new(CaptureMode::Real { displays });
    let deadline = Instant::now() + Duration::from_secs(9 * 60);
    while Instant::now() < deadline {
        let done = bench.record(RecordTarget::AudioOnly, bench.options(30, true, false), 15.0);
        let trace = bench.trace();
        let stats = trace.iter().find(|l| l.starts_with("audio system")).expect("stats of the system input").clone();
        let device = trace.iter().find(|l| l.starts_with("system audio:")).expect("the device line").clone();
        let packets: u64 = stats.split("packets=").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
        println!("{device}; {stats}");
        if packets > 0 {
            let pcm = decode_pcm(&done.file);
            let peak = pcm.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
            let info = probe(&done.file);
            let sound = stream(&info, "audio").unwrap();
            println!(
                "peak {peak:.3}; file {:.3} s for {:.3} s recorded; {} Hz, {} channels",
                seconds(&sound["duration"]),
                done.duration_sec,
                sound["sample_rate"],
                sound["channels"]
            );
            assert_eq!(sound["sample_rate"], "48000");
            assert!(stats.contains(" late=0 "), "{stats}");
            let slipped: u64 = stats.rsplit("slipped=").next().unwrap().parse().unwrap();
            assert!(slipped < 2000, "the rate would be wrong: {stats}");
            std::fs::remove_file(&done.file).unwrap();
            return;
        }
        std::fs::remove_file(&done.file).unwrap();
        thread::sleep(Duration::from_secs(5));
    }
    println!("nothing played for nine minutes: silence on the track was all that could be checked");
}
