//! `%TEMP%\shotik-bubble.log`: the real focus and clipboard behaviour of the bubble-to-translate path
//! cannot be reproduced off-screen, so it is logged and the tail read after a live repro. Truncated on
//! startup, so it never grows without bound. Lengths only, never the text itself: people select
//! passwords and private messages.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Diagnostics {
    file: PathBuf,
    timing_file: PathBuf,
}

impl Diagnostics {
    pub fn new(temp_dir: &Path) -> Self {
        let file = temp_dir.join("shotik-bubble.log");
        let _ = fs::write(&file, "");
        Self { file, timing_file: temp_dir.join("shotik-overlay-timing.log") }
    }

    /// A diagnostics write that fails must not disturb the feature it describes.
    pub fn log(&self, message: &str) {
        let stamp = chrono::Utc::now().format("%H:%M:%S%.3f");
        if let Ok(mut f) = OpenOptions::new().append(true).create(true).open(&self.file) {
            let _ = writeln!(f, "{stamp} {message}");
        }
    }

    /// The last overlay's marks, overwritten each time, so real latency can be read without a debug build.
    pub fn write_timing(&self, marks: &[String]) {
        let _ = fs::write(&self.timing_file, marks.join(" "));
    }
}
