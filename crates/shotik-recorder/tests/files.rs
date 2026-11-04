//! Real encoding with the synthetic sources, checked from the outside with ffprobe, ffmpeg and the
//! Windows player engine. No screen and no device is touched: `CaptureMode::Synthetic`.
#![cfg(windows)]

mod common;

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use common::{
    amplitude, count, decode_pcm, frame_luma, frame_rgb, media_foundation, probe, region, rms_windows, seconds, serial, stream, Bench,
};
use shotik_core::ports::{RecAudioInfo, RecordTarget, RecorderControl, RecorderEvent, RecordingDone};
use shotik_recorder::{export_gif, Marker, SilentStretches, SynthOptions};

const BENCH_REGION: (i32, i32, i32, i32) = (100, 100, 1280, 720);

fn bench_target() -> RecordTarget {
    region(BENCH_REGION.0, BENCH_REGION.1, BENCH_REGION.2, BENCH_REGION.3, 1.5)
}

fn assert_tones(file: &Path, system: bool, microphone: bool) -> Vec<f32> {
    let pcm = decode_pcm(file);
    let (a440, a880) = (amplitude(&pcm, 0.2, 1.0, 440.0), amplitude(&pcm, 0.2, 1.0, 880.0));
    println!("{}: 440 Hz {a440:.3}, 880 Hz {a880:.3}", file.display());
    // the thresholds of test/ghost/recorder.test.js
    if system {
        assert!(a440 > 0.035, "system tone in the file: {a440}");
    } else {
        assert!(a440 < 0.015, "no system tone in the file: {a440}");
    }
    if microphone {
        assert!(a880 > 0.035, "microphone tone in the file: {a880}");
    } else {
        assert!(a880 < 0.015, "no microphone tone in the file: {a880}");
    }
    pcm
}

fn check_video_file(done: &RecordingDone, fps: u64, width: u64, height: u64, audio: bool) {
    let info = probe(&done.file);
    assert_eq!(done.file.extension().unwrap(), "mp4");
    let video = stream(&info, "video").expect("a video stream");
    assert_eq!(video["codec_name"], "h264");
    assert_eq!((video["width"].as_u64().unwrap(), video["height"].as_u64().unwrap()), (width, height));
    assert_eq!(video["pix_fmt"], "yuv420p");
    assert_eq!(video["avg_frame_rate"], format!("{fps}/1"), "constant frame rate: {video}");
    assert_eq!(video["r_frame_rate"], format!("{fps}/1"), "{video}");
    let frames = count(&video["nb_read_frames"]);
    let duration = seconds(&video["duration"]);
    assert!((frames as f64 - duration * fps as f64).abs() <= 1.0, "{frames} frames in {duration} s: {video}");
    assert!((duration - done.duration_sec).abs() < 0.1, "file {duration} s, reported {} s", done.duration_sec);
    assert_eq!(stream(&info, "audio").is_some(), audio, "{info}");
    println!(
        "{}: {frames} frames, {duration:.3} s, {} bytes",
        done.file.display(),
        fs::metadata(&done.file).unwrap().len()
    );
    if audio {
        let sound = stream(&info, "audio").unwrap();
        assert_eq!(sound["codec_name"], "aac");
        assert_eq!(sound["sample_rate"], "48000");
        assert_eq!(sound["channels"], 2);
        let end_gap = (seconds(&sound["duration"]) - duration).abs();
        println!("audio and video end {:.1} ms apart", end_gap * 1000.0);
        assert!(end_gap < 0.05, "audio and video end together: {sound} {video}");
    }
}

fn check_seek(file: &Path, duration: f64) {
    let seek_to = (duration * 0.75 * 10.0).round() / 10.0;
    let mf = media_foundation(file, seek_to);
    println!("Media Foundation: {mf}");
    assert_eq!(mf["canSeek"], true, "{mf}");
    assert!((mf["durationSec"].as_f64().unwrap() - duration).abs() < 0.2, "{mf} vs {duration}");
    assert!((mf["positionAfterSeek"].as_f64().unwrap() - seek_to).abs() < 0.3, "{mf} vs {seek_to}");
}

fn assert_nothing_left(bench: &Bench) {
    assert_eq!(bench.temp_files(), Vec::<std::path::PathBuf>::new(), "no part file stays behind");
}

#[test]
fn video_only() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    let options = bench.options(30, false, false);
    let started = Instant::now();
    bench.recorder.start(bench_target(), options).unwrap().unwrap();
    bench.wait_running();
    let start_ms = started.elapsed().as_millis();
    let status = bench.recorder.status();
    assert_eq!(status.audio, Some(RecAudioInfo { system: false, microphone: false, tracks: 0 }));
    thread::sleep(Duration::from_secs(3));
    let stop = Instant::now();
    bench.recorder.stop();
    let done = bench.wait_done();
    println!("start {start_ms} ms, stop {} ms", stop.elapsed().as_millis());
    for line in bench.trace() {
        println!("  {line}");
    }
    check_video_file(&done, 30, 1280, 720, false);
    check_seek(&done.file, done.duration_sec);
    assert_eq!((done.width, done.height), (853, 480), "DIP size for history");
    assert!((done.duration_sec - 3.0).abs() < 0.2, "{}", done.duration_sec);
    let thumb = done.first_frame_jpeg.as_ref().expect("a thumbnail");
    assert_eq!(&thumb[..2], &[0xFF, 0xD8]);
    assert_nothing_left(&bench);
    let name = done.file.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("Rec_") && name.ends_with(".mp4"), "{name}");
    assert!(bench.trace().iter().any(|l| l.starts_with("encoder h264 ")), "{:?}", bench.trace());
    assert!(bench.trace().iter().any(|l| l.starts_with("index ok")), "{:?}", bench.trace());
}

/// The picture of `synthStream`: the conversion to NV12 and the colour tags must survive the round
/// trip through the encoder and a foreign decoder.
#[test]
fn colours_survive_encoding() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    let done = bench.record(region(0, 0, 640, 480, 1.0), bench.options(30, false, false), 1.0);
    let rgb = frame_rgb(&done.file, 0.5, 640, 480);
    let pixel = |x: usize, y: usize| {
        let i = (y * 640 + x) * 3;
        (rgb[i], rgb[i + 1], rgb[i + 2])
    };
    let close = |got: (u8, u8, u8), want: (u8, u8, u8)| {
        got.0.abs_diff(want.0) <= 6 && got.1.abs_diff(want.1) <= 6 && got.2.abs_diff(want.2) <= 6
    };
    let corner = pixel(4, 4);
    assert!(close(corner, (0x12, 0x14, 0x1c)), "background {corner:?}");
    // the rectangle moves 9 px per frame from x = 9: at 0.5 s it is around x = 135..355
    let inside = pixel(220, 350);
    assert!(close(inside, (0x7c, 0x5c, 0xff)), "rectangle {inside:?}");
    println!("background {corner:?}, rectangle {inside:?}");
}

#[test]
fn video_with_system_audio() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    let done = bench.record(bench_target(), bench.options(30, true, false), 2.5);
    check_video_file(&done, 30, 1280, 720, true);
    assert_tones(&done.file, true, false);
    assert!(bench.trace().iter().any(|l| l.starts_with("encoder aac ")), "{:?}", bench.trace());
}

#[test]
fn video_with_microphone() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    let done = bench.record(bench_target(), bench.options(30, false, true), 2.5);
    check_video_file(&done, 30, 1280, 720, true);
    assert_tones(&done.file, false, true);
}

#[test]
fn video_with_both_inputs_mixed() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    bench.recorder.start(bench_target(), bench.options(30, true, true)).unwrap().unwrap();
    bench.wait_running();
    assert_eq!(bench.recorder.status().audio, Some(RecAudioInfo { system: true, microphone: true, tracks: 1 }));
    let levels = bench.wait_for("levels", Duration::from_secs(5), |e| matches!(e, RecorderEvent::Levels(_)));
    let RecorderEvent::Levels(levels) = levels else { unreachable!() };
    println!("levels {levels:?}");
    assert!(levels.system > 0.1 && levels.microphone > 0.1, "{levels:?}");
    thread::sleep(Duration::from_millis(2500));
    bench.recorder.stop();
    let done = bench.wait_done();
    check_video_file(&done, 30, 1280, 720, true);
    assert_tones(&done.file, true, true);
}

fn check_audio_only(system: bool, microphone: bool) {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    let started = bench.recorder.start(RecordTarget::AudioOnly, bench.options(30, system, microphone)).unwrap().unwrap();
    assert_eq!((started.recording, started.audio_only, started.audio.clone()), (true, Some(true), None));
    bench.wait_running();
    assert_eq!(
        bench.recorder.status().audio,
        Some(RecAudioInfo { system, microphone, tracks: 1 })
    );
    thread::sleep(Duration::from_secs(4));
    bench.recorder.stop();
    let done = bench.wait_done();
    assert_eq!(done.file.extension().unwrap(), "m4a");
    assert!(done.audio_only);
    assert_eq!((done.width, done.height, done.first_frame_jpeg.is_none()), (0, 0, true));
    let trace = bench.trace();
    assert!(trace.iter().any(|l| l.contains("recorder.start audio/mp4;codecs=mp4a.40.2")), "{trace:?}");
    assert!(trace.iter().any(|l| l.starts_with("index ok")), "{trace:?}");
    if !system {
        assert!(trace.iter().any(|l| l.contains("vtracks=0 atracks=0")), "{trace:?}");
    }
    let size = fs::metadata(&done.file).unwrap().len();
    assert!(size < 200 * 1024, "a few seconds of audio stay small: {size}");
    let info = probe(&done.file);
    assert!(stream(&info, "video").is_none(), "{info}");
    let sound = stream(&info, "audio").expect("audio");
    assert_eq!(sound["codec_name"], "aac");
    assert_eq!(sound["channels"], 2);
    assert!((seconds(&sound["duration"]) - done.duration_sec).abs() < 0.1, "{sound} vs {}", done.duration_sec);
    assert_tones(&done.file, system, microphone);
    check_seek(&done.file, done.duration_sec);
    assert_nothing_left(&bench);
}

#[test]
fn audio_only_system() {
    check_audio_only(true, false);
}

#[test]
fn audio_only_microphone() {
    check_audio_only(false, true);
}

#[test]
fn audio_only_both() {
    check_audio_only(true, true);
}

#[test]
fn audio_only_with_no_inputs_is_an_explicit_error() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    bench.recorder.start(RecordTarget::AudioOnly, bench.options(30, false, false)).unwrap().unwrap();
    let event = bench.events.recv_timeout(Duration::from_secs(5));
    let mut errors = Vec::new();
    let mut last = event;
    while let Ok(event) = last {
        if let RecorderEvent::Error(message) = &event {
            errors.push(message.clone());
        }
        if matches!(&event, RecorderEvent::State(s) if !s.recording) {
            break;
        }
        last = bench.events.recv_timeout(Duration::from_secs(5));
    }
    assert_eq!(errors, vec!["audio:none".to_string()]);
    assert!(bench.saved_files().is_empty());
    assert_nothing_left(&bench);
}

#[test]
fn pause_and_resume_leave_no_gap_in_the_file() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    bench.recorder.start(bench_target(), bench.options(30, true, false)).unwrap().unwrap();
    bench.wait_running();
    thread::sleep(Duration::from_secs(2));
    bench.recorder.pause();
    assert_eq!(bench.recorder.status().paused, Some(true));
    thread::sleep(Duration::from_millis(1500));
    bench.recorder.resume();
    thread::sleep(Duration::from_secs(2));
    bench.recorder.stop();
    let done = bench.wait_done();
    check_video_file(&done, 30, 1280, 720, true);
    assert!((done.duration_sec - 4.0).abs() < 0.25, "the pause is not in the file: {}", done.duration_sec);
    check_seek(&done.file, done.duration_sec);

    // a steady tone stays steady over the join: no hole and no doubled stretch
    let pcm = decode_pcm(&done.file);
    let windows = rms_windows(&pcm, 20);
    let ignored_at_each_end = 6;
    let inner = &windows[ignored_at_each_end..windows.len() - ignored_at_each_end];
    let (low, high) = inner.iter().fold((f64::MAX, 0.0_f64), |(l, h), &v| (l.min(v), h.max(v)));
    println!("RMS per 20 ms over the whole file: {low:.4} .. {high:.4}");
    assert!(low > 0.08 && high < 0.13, "{low} .. {high}");
}

fn onsets_of(values: impl Iterator<Item = (f64, bool)>) -> Vec<f64> {
    let mut onsets = Vec::new();
    let mut was_on = true; // a mark already running at the start is not an onset
    for (t, on) in values {
        if on && !was_on {
            onsets.push(t);
        }
        was_on = on;
    }
    onsets
}

/// Marks at the same instants in picture and sound, on the shared clock. The video onset can only be
/// a frame boundary, up to a frame after the audio onset.
fn marker_offsets(file: &Path) -> Vec<f64> {
    let video = onsets_of(frame_luma(file).into_iter().map(|(t, luma)| (t, luma > 128.0)));
    let pcm = decode_pcm(file);
    let audio = onsets_of(rms_windows(&pcm, 1).into_iter().enumerate().map(|(i, rms)| (i as f64 * 0.001, rms > 0.02)));
    assert!(video.len() >= 3, "marks in the picture: {video:?}");
    assert!(audio.len() >= 3, "marks in the sound: {audio:?}");
    println!("video marks {video:?}");
    println!("audio marks {audio:?}");
    video
        .iter()
        .map(|v| {
            let nearest = audio.iter().copied().min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs())).unwrap();
            v - nearest
        })
        .collect()
}

#[test]
fn audio_and_video_marks_coincide() {
    let _serial = serial();
    let marker = Marker { period_ms: 1000, width_ms: 300 };
    let bench = Bench::synthetic(SynthOptions { marker: Some(marker), system_silence: None });
    let done = bench.record(region(0, 0, 320, 180, 1.0), bench.options(30, true, false), 6.5);
    let offsets = marker_offsets(&done.file);
    println!("A/V offset of each mark, video minus audio, ms: {:?}", offsets.iter().map(|o| (o * 1000.0).round()).collect::<Vec<_>>());
    for offset in &offsets {
        assert!(offset.abs() < 0.05, "offset {offset} of {offsets:?}");
    }
}

#[test]
fn marks_coincide_after_a_pause() {
    let _serial = serial();
    let marker = Marker { period_ms: 1000, width_ms: 300 };
    let bench = Bench::synthetic(SynthOptions { marker: Some(marker), system_silence: None });
    bench.recorder.start(region(0, 0, 320, 180, 1.0), bench.options(30, true, false)).unwrap().unwrap();
    bench.wait_running();
    thread::sleep(Duration::from_millis(3300));
    bench.recorder.pause();
    thread::sleep(Duration::from_millis(1700));
    bench.recorder.resume();
    thread::sleep(Duration::from_millis(3300));
    bench.recorder.stop();
    let done = bench.wait_done();
    assert!((done.duration_sec - 6.6).abs() < 0.3, "{}", done.duration_sec);
    let offsets = marker_offsets(&done.file);
    println!("A/V offset across a pause, ms: {:?}", offsets.iter().map(|o| (o * 1000.0).round()).collect::<Vec<_>>());
    for offset in &offsets {
        assert!(offset.abs() < 0.05, "offset {offset} of {offsets:?}");
    }
}

#[test]
fn silence_on_loopback_does_not_stall_the_track() {
    let _serial = serial();
    let silence = SilentStretches { period_ms: 1000, audible_ms: 300 };
    let bench = Bench::synthetic(SynthOptions { marker: None, system_silence: Some(silence) });
    let done = bench.record(bench_target(), bench.options(30, true, false), 3.5);
    check_video_file(&done, 30, 1280, 720, true);
    let pcm = decode_pcm(&done.file);
    let windows = rms_windows(&pcm, 20);
    let loud = windows.iter().filter(|&&v| v > 0.05).count();
    let quiet = windows.iter().filter(|&&v| v < 0.01).count();
    println!("{} windows: {loud} with sound, {quiet} silent", windows.len());
    assert!(loud > 20 && quiet > 60, "{loud} loud, {quiet} quiet of {}", windows.len());
    assert!(bench.trace().iter().any(|l| l.starts_with("audio system") && !l.contains("gap=0 ")), "{:?}", bench.trace());
}

#[test]
fn cancel_leaves_nothing() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    bench.recorder.start(bench_target(), bench.options(30, true, true)).unwrap().unwrap();
    bench.wait_running();
    thread::sleep(Duration::from_millis(800));
    assert!(!bench.temp_files().is_empty(), "the part file exists while recording");
    bench.recorder.cancel();
    bench.wait_for("idle", Duration::from_secs(10), |e| matches!(e, RecorderEvent::State(s) if !s.recording));
    assert!(bench.saved_files().is_empty());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !bench.temp_files().is_empty() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_nothing_left(&bench);
}

#[test]
fn stop_right_after_start_is_clean() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    bench.recorder.start(bench_target(), bench.options(30, true, true)).unwrap().unwrap();
    bench.recorder.stop();
    let deadline = Instant::now() + Duration::from_secs(10);
    while bench.recorder.is_recording() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!bench.recorder.is_recording());
    assert!(bench.saved_files().is_empty(), "nothing was captured yet: {:?}", bench.saved_files());
    assert_nothing_left(&bench);
    println!("{:?}", bench.trace());
}

#[test]
fn stop_right_after_capture_begins_gives_a_valid_short_file() {
    let _serial = serial();
    for (target, system) in [(bench_target(), true), (RecordTarget::AudioOnly, true), (bench_target(), false)] {
        let bench = Bench::synthetic(SynthOptions::default());
        bench.recorder.start(target, bench.options(30, system, false)).unwrap().unwrap();
        bench.wait_running();
        bench.recorder.stop();
        let done = bench.wait_done();
        let info = probe(&done.file);
        println!("{}: {info}", done.file.display());
        let any = stream(&info, "video").or_else(|| stream(&info, "audio")).expect("a stream");
        assert!(seconds(&any["duration"]) > 0.0, "{info}");
        if let Some(video) = stream(&info, "video") {
            assert!(count(&video["nb_read_frames"]) >= 1, "{info}");
        }
        assert_nothing_left(&bench);
    }
}

#[test]
fn odd_regions_are_evened_without_shifting() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    let done = bench.record(region(31, 33, 481, 271, 1.0), bench.options(30, false, false), 1.0);
    check_video_file(&done, 30, 480, 270, false);
    assert_eq!((done.width, done.height), (481, 271), "history keeps the DIP size of the selection");
    // the background at the corner and the top of the rectangle at y = 240, 207 rows into the crop
    let rgb = frame_rgb(&done.file, 0.1, 480, 270);
    let pixel = |x: usize, y: usize| {
        let i = (y * 480 + x) * 3;
        (rgb[i], rgb[i + 1], rgb[i + 2])
    };
    let (above, below) = (pixel(60, 203), pixel(60, 212));
    println!("above {above:?}, below {below:?}");
    assert!(above.2 < 60 && below.2 > 200, "the rectangle starts where the crop puts it: {above:?} {below:?}");
}

#[test]
fn other_rates_and_sizes() {
    let _serial = serial();
    for (fps, width, height) in [(60, 1920, 1080), (15, 640, 360), (30, 64, 64), (24, 3840, 2160)] {
        let bench = Bench::synthetic(SynthOptions::default());
        let done = bench.record(region(0, 0, width, height, 1.0), bench.options(fps, false, false), 1.5);
        println!("{fps} fps {width}x{height}: {:?}", bench.trace());
        let info = probe(&done.file);
        let video = stream(&info, "video").unwrap();
        assert_eq!(video["avg_frame_rate"], format!("{fps}/1"), "{video}");
        assert_eq!((video["width"].as_i64().unwrap(), video["height"].as_i64().unwrap()), (i64::from(width), i64::from(height)));
    }
}

#[test]
fn a_taken_name_gets_a_counter_for_real_files_too() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    fs::create_dir_all(&bench.save_dir).unwrap();
    let first = bench.record(RecordTarget::AudioOnly, bench.options(30, true, false), 0.3);
    let name = first.file.file_stem().unwrap().to_string_lossy().into_owned();
    // the same second is the only way to collide, so the older file is put in place under the next name
    let second = bench.record(RecordTarget::AudioOnly, bench.options(30, true, false), 0.3);
    assert!(first.file.exists() && second.file.exists());
    assert_ne!(first.file, second.file);
    assert!(name.starts_with("Rec_"));
}

#[test]
fn gif_of_a_recording() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    let done = bench.record(bench_target(), bench.options(30, false, false), 3.0);
    let started = Instant::now();
    let gif = export_gif(&done.file, 12, 640).unwrap();
    println!("GIF of {:.1} s took {} ms, {} bytes", done.duration_sec, started.elapsed().as_millis(), fs::metadata(&gif).unwrap().len());
    assert_eq!(gif, done.file.with_extension("gif"), "next to the video");
    let info = probe(&gif);
    let video = stream(&info, "video").unwrap();
    assert_eq!(video["codec_name"], "gif");
    assert_eq!((video["width"].as_u64().unwrap(), video["height"].as_u64().unwrap()), (640, 360));
    let frames = count(&video["nb_read_frames"]);
    let expected = done.duration_sec * 12.0;
    assert!((frames as f64 - expected).abs() <= 2.0, "{frames} frames for {expected}");
    let length = seconds(&info["format"]["duration"]);
    assert!((length - done.duration_sec).abs() < 0.2, "the GIF lasts as long as the video: {length}");
}

#[test]
fn gif_of_something_that_is_no_video_is_an_explicit_error() {
    let dir = std::env::temp_dir().join(format!("shotik-gif-bad-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let not_video = dir.join("x.mp4");
    fs::write(&not_video, b"this is no video").unwrap();
    let error = export_gif(&not_video, 12, 640).unwrap_err().to_string();
    assert!(error.contains("x.mp4"), "{error}");
    assert!(!dir.join("x.gif").exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn gif_exports_leave_no_handles_behind() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    let done = bench.record(region(0, 0, 640, 360, 1.0), bench.options(30, false, false), 1.0);
    for _ in 0..2 {
        export_gif(&done.file, 12, 320).unwrap();
    }
    thread::sleep(Duration::from_millis(200));
    let before = common::handle_types();
    for _ in 0..8 {
        export_gif(&done.file, 12, 320).unwrap();
    }
    thread::sleep(Duration::from_millis(200));
    let growth = common::diff_types(&before, &common::handle_types());
    println!("over 8 exports: {growth:?}");
    for (kind, delta) in &growth {
        assert!(*delta < 8, "{kind} grows by {delta} over 8 exports");
    }
}

/// The number to beat is the 12.7 s of the Electron build for a 15 s clip of this scenario.
#[test]
#[ignore = "takes 20 s, for numbers"]
fn gif_bench_scenario() {
    let _serial = serial();
    let bench = Bench::synthetic(SynthOptions::default());
    let done = bench.record(bench_target(), bench.options(30, false, false), 15.0);
    let started = Instant::now();
    let gif = export_gif(&done.file, 0, 0).unwrap();
    println!("GIF of {:.1} s took {} ms, {:.2} MB", done.duration_sec, started.elapsed().as_millis(), fs::metadata(&gif).unwrap().len() as f64 / 1_048_576.0);
}

/// `SHOTIK_GIF_SOURCE` names a file to export, for a comparison from the outside.
#[test]
#[ignore = "a tool for comparing quality"]
fn gif_of_a_given_file() {
    let source = std::env::var("SHOTIK_GIF_SOURCE").expect("SHOTIK_GIF_SOURCE");
    let started = Instant::now();
    let gif = export_gif(Path::new(&source), 0, 0);
    println!("{source}: {} ms, {gif:?}", started.elapsed().as_millis());
    gif.unwrap();
}
