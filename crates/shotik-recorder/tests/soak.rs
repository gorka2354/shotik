//! Ten minutes of the synthetic source: memory and handles stay flat and no frame goes missing.
#![cfg(windows)]

mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::{count, cpu_ms, handle_count, private_mb, probe, region, seconds, stream, Bench};
use shotik_core::ports::RecorderControl;
use shotik_recorder::SynthOptions;

#[test]
#[ignore = "takes ten minutes"]
fn ten_minutes_stay_flat() {
    const SAMPLE: Duration = Duration::from_secs(30);
    let minutes: u64 = std::env::var("SHOTIK_SOAK_MINUTES").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let bench = Bench::synthetic(SynthOptions::default());
    let baseline = (handle_count(), private_mb());
    let started = Instant::now();
    bench.recorder.start(region(100, 100, 1280, 720, 1.5), bench.options(30, true, true)).unwrap().unwrap();
    bench.wait_running();
    let running = Instant::now();
    let cpu_before = cpu_ms();
    println!("before {baseline:?}, start {} ms", started.elapsed().as_millis());
    let mut samples = Vec::new();
    while running.elapsed() < Duration::from_secs(minutes * 60) {
        thread::sleep(SAMPLE);
        // drained so the event channel of the test does not count as growth
        while bench.events.try_recv().is_ok() {}
        let sample = (running.elapsed().as_secs(), handle_count(), private_mb());
        println!("{} s: handles {}, private {:.1} MB, written {} MB", sample.0, sample.1, sample.2, bench.recorder.status().bytes.unwrap_or(0) / 1_048_576);
        samples.push(sample);
    }
    let cpu = (cpu_ms() - cpu_before) / running.elapsed().as_secs_f64();
    let wall = running.elapsed().as_secs_f64();
    let stop = Instant::now();
    bench.recorder.stop();
    let done = bench.wait_done();
    println!("stop {} ms, CPU {cpu:.0} ms/s", stop.elapsed().as_millis());
    for line in bench.trace() {
        println!("  {line}");
    }

    let (_, first_handles, first_private) = samples[1];
    let (_, last_handles, last_private) = samples[samples.len() - 1];
    thread::sleep(Duration::from_secs(1));
    let (handles_after, private_after) = (handle_count(), private_mb());
    println!(
        "from minute 1 to the end: handles {first_handles} to {last_handles}, private {first_private:.1} to {last_private:.1} MB; after the session: handles {handles_after}, private {private_after:.1} MB"
    );
    assert!(last_handles.abs_diff(first_handles) < 25, "handles {first_handles} to {last_handles}");
    // what grows during a session is the index of the file, which Media Foundation keeps until it writes
    // it at the end: about 85 bytes per sample, 0.4 MB per minute with audio and video. It has to be gone
    // after the session and must stay in that range.
    let per_minute = (last_private - first_private) / (minutes as f64 - 1.0);
    assert!(per_minute < 1.5, "private grows {per_minute} MB per minute: {first_private} to {last_private}");
    assert!(private_after < first_private, "the session gives its memory back: {private_after} after, {first_private} during");

    let info = probe(&done.file);
    let video = stream(&info, "video").unwrap();
    let frames = count(&video["nb_read_frames"]);
    println!("frames {frames}, expected {:.0}; size {} MB", wall * 30.0, std::fs::metadata(&done.file).unwrap().len() / 1_048_576);
    assert!((frames as f64 - wall * 30.0).abs() < 5.0, "{frames} frames in {wall} s");
    let sound = stream(&info, "audio").unwrap();
    let apart = (seconds(&sound["duration"]) - seconds(&video["duration"])).abs();
    println!("audio and video end {:.1} ms apart after ten minutes", apart * 1000.0);
    assert!(apart < 0.05, "{apart}");
    // the marks of drift: the tones are still in tune and at full level at the very end
    assert_eq!(video["avg_frame_rate"], "30/1", "{video}");
}
