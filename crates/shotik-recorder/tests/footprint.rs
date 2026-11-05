//! What staying ready costs after the first recording, step by step, in a fresh process.
#![cfg(windows)]

mod common;

use std::thread;
use std::time::Duration;

use common::{handle_count, private_mb, region, Bench};
use shotik_core::ports::RecordTarget;
use shotik_recorder::SynthOptions;

fn nvidia_smi(query: &str) -> String {
    std::process::Command::new("nvidia-smi")
        .args([format!("--query-gpu={query}").as_str(), "--format=csv,noheader"])
        .output()
        .map_or_else(|e| e.to_string(), |out| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn line(what: &str) {
    thread::sleep(Duration::from_millis(500));
    println!(
        "{what}: private {:.1} MB, handles {}, GPU encoding sessions {}, GPU memory {}",
        private_mb(),
        handle_count(),
        nvidia_smi("encoder.stats.sessionCount"),
        nvidia_smi("memory.used")
    );
}

#[test]
#[ignore = "for numbers, run alone"]
fn what_a_process_keeps() {
    line("fresh process");
    let bench = Bench::synthetic(SynthOptions::default());
    line("recorder created");
    bench.record(RecordTarget::AudioOnly, bench.options(30, true, false), 1.0);
    line("after an audio-only session");
    bench.record(region(100, 100, 1280, 720, 1.0), bench.options(30, false, false), 1.0);
    line("after the first video session");
    bench.record(region(100, 100, 1280, 720, 1.0), bench.options(30, false, false), 1.0);
    line("after the second video session");
}
