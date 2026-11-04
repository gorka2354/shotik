//! Where a finished recording goes. Reference: `tsName` and `finalize` in `src/main/recorder.js`.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use shotik_core::{Error, Result};

/// Local wall-clock time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

pub fn ts_name(t: &LocalTime) -> String {
    format!(
        "Rec_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

fn with_suffix(base: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(base);
    name.push(suffix);
    PathBuf::from(name)
}

/// `base.ext`, or `base-2.ext` and so on when taken.
pub fn free_path(base: &Path, ext: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let first = with_suffix(base, &format!(".{ext}"));
    if !exists(&first) {
        return first;
    }
    (2_u32..)
        .map(|i| with_suffix(base, &format!("-{i}.{ext}")))
        .find(|candidate| !exists(candidate))
        .expect("the file name counter ran out")
}

/// Moves the finished part file to its final name. Temp and the save directory may be on different
/// volumes, where only a copy works.
pub fn move_into_place(part: &Path, base: &Path, ext: &str) -> Result<PathBuf> {
    let out = free_path(base, ext, Path::exists);
    if fs::rename(part, &out).is_ok() {
        return Ok(out);
    }
    fs::copy(part, &out).map_err(|e| Error::io("copy the recording to", &out, e))?;
    fs::remove_file(part).map_err(|e| Error::io("remove the temporary recording", part, e))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_has_the_format_of_the_js_version() {
        let t = LocalTime { year: 2026, month: 3, day: 7, hour: 9, minute: 5, second: 1 };
        assert_eq!(ts_name(&t), "Rec_2026-03-07_09-05-01");
    }

    #[test]
    fn a_taken_name_gets_a_counter() {
        let base = Path::new("dir/Rec_x");
        assert_eq!(free_path(base, "mp4", |_| false), Path::new("dir/Rec_x.mp4"));
        let taken = |p: &Path| p == Path::new("dir/Rec_x.mp4") || p == Path::new("dir/Rec_x-2.mp4");
        assert_eq!(free_path(base, "mp4", taken), Path::new("dir/Rec_x-3.mp4"));
    }

    #[test]
    fn dots_in_the_name_are_kept() {
        assert_eq!(free_path(Path::new("a.b/Rec_1.2"), "m4a", |_| false), Path::new("a.b/Rec_1.2.m4a"));
    }

    #[test]
    fn the_part_file_moves_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("shotik-names-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let base = dir.join("Rec_t");
        let existing = dir.join("Rec_t.mp4");
        fs::write(&existing, b"old").unwrap();
        let part = dir.join("rec.part");
        fs::write(&part, b"new").unwrap();
        let out = move_into_place(&part, &base, "mp4").unwrap();
        assert_eq!(out, dir.join("Rec_t-2.mp4"));
        assert_eq!(fs::read(&out).unwrap(), b"new");
        assert_eq!(fs::read(&existing).unwrap(), b"old");
        assert!(!part.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
