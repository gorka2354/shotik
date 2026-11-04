//! Shared, portable atomic JSON persistence. The temporary file is on the same volume.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::{Error, Result};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        // On success rename already removed this path. On error remove the partial write.
        let _ = fs::remove_file(&self.0);
    }
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::msg(format!("cannot serialize JSON: {e}")))?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| Error::io("create directory", parent, e))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::msg("JSON path has no file name"))?;
    let (temp, mut file) = loop {
        let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = name.to_os_string();
        temp_name.push(format!(".{}.{n}.tmp", std::process::id()));
        let temp = parent.join(temp_name);
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (TempFile(temp), file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::io("create temporary JSON", temp, e)),
        }
    };
    file.write_all(&bytes)
        .map_err(|e| Error::io("write JSON", &temp.0, e))?;
    file.sync_all()
        .map_err(|e| Error::io("flush JSON", &temp.0, e))?;
    drop(file);
    fs::rename(&temp.0, path).map_err(|e| Error::io("replace JSON", path, e))
}

pub(crate) fn read_json(path: &Path) -> Result<Option<serde_json::Value>> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io("read JSON", path, e)),
    }
}

pub(crate) fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io("delete file", path, e)),
    }
}
