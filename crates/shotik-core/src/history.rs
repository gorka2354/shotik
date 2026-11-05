//! The history model: the same JSON as `history.json` of the Electron version.
//! Reference: `src/main/history.js`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, FixedOffset, Local, NaiveDateTime};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ports::{HistoryFilter, HistoryStore, RecordingDone, SettingsStore};
use crate::{storage, Error, Frame, Result};

const MAX_ITEMS: usize = 500;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
type Clock = Arc<dyn Fn() -> DateTime<FixedOffset> + Send + Sync>;

/// Old entries have no `kind`; they are images.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub file: PathBuf,
    pub thumb: Option<PathBuf>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// Milliseconds since the Unix epoch.
    pub ts: i64,
    #[serde(default)]
    pub source: String,
    /// "image", "video" or "audio".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Seconds. Recordings only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl HistoryEntry {
    pub fn is_recording(&self) -> bool {
        matches!(self.kind.as_deref(), Some("video" | "audio"))
    }
}

/// Filename tokens use local calendar fields and replace only their first occurrence, as JS does.
pub fn ts_name(pattern: &str, local: NaiveDateTime) -> String {
    pattern
        .replacen("{date}", &local.format("%Y-%m-%d").to_string(), 1)
        .replacen("{time}", &local.format("%H-%M-%S").to_string(), 1)
}

pub struct FileHistory {
    index: PathBuf,
    thumbs: PathBuf,
    settings: Arc<dyn SettingsStore>,
    skipped: usize,
    items: Mutex<Vec<HistoryEntry>>,
    clock: Clock,
}

impl FileHistory {
    pub fn new(user_data: &Path, settings: Arc<dyn SettingsStore>) -> Result<Self> {
        Self::with_clock(
            user_data,
            settings,
            Arc::new(|| Local::now().fixed_offset()),
        )
    }

    fn with_clock(
        user_data: &Path,
        settings: Arc<dyn SettingsStore>,
        clock: Clock,
    ) -> Result<Self> {
        let index = user_data.join("history.json");
        // One unreadable entry must cost that entry alone: an empty list here would erase the whole
        // history on the next save.
        let stored = match storage::read_json(&index)? {
            Some(Value::Array(stored)) => stored,
            _ => Vec::new(),
        };
        let total = stored.len();
        let items: Vec<HistoryEntry> = stored
            .into_iter()
            .filter_map(|entry| serde_json::from_value(entry).ok())
            .collect();
        Ok(Self {
            index,
            thumbs: user_data.join("thumbs"),
            settings,
            skipped: total - items.len(),
            items: Mutex::new(items),
            clock,
        })
    }

    /// How many stored entries could not be read. The composition root logs this.
    pub fn skipped_entries(&self) -> usize {
        self.skipped
    }

    pub fn last(&self) -> Option<HistoryEntry> {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first()
            .cloned()
    }

    /// `history.update`: shallowly assign the patch, preserving unknown fields.
    pub fn update(&self, id: &str, patch: Value) -> Result<Option<HistoryEntry>> {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        let Some(index) = items.iter().position(|entry| entry.id == id) else {
            return Ok(None);
        };
        let mut value = serde_json::to_value(&items[index])
            .map_err(|_| Error::msg("cannot serialize history entry"))?;
        if let Some(patch) = patch.as_object() {
            value
                .as_object_mut()
                .expect("history entry is an object")
                .extend(patch.clone());
        }
        let entry = serde_json::from_value(value)
            .map_err(|_| Error::msg("invalid history entry field type"))?;
        let mut next = items.clone();
        next[index] = entry;
        storage::write_json(&self.index, &next)?;
        *items = next;
        Ok(Some(items[index].clone()))
    }

    fn add(&self, mut entry: HistoryEntry) -> Result<HistoryEntry> {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let suffix = NEXT_ID.fetch_add(1, Ordering::Relaxed) % 36u64.pow(4);
            entry.id = format!("{}{:0>4}", base36(entry.ts as u64), base36(suffix));
            if !items.iter().any(|old| old.id == entry.id) {
                break;
            }
        }
        let mut next = Vec::with_capacity((items.len() + 1).min(MAX_ITEMS));
        next.push(entry.clone());
        next.extend(items.iter().take(MAX_ITEMS - 1).cloned());
        storage::write_json(&self.index, &next)?;
        *items = next;
        Ok(entry)
    }

    fn write_thumb(&self, file: &Path, jpeg: &[u8]) -> Result<PathBuf> {
        fs::create_dir_all(&self.thumbs)
            .map_err(|e| Error::io("create thumbnail directory", &self.thumbs, e))?;
        let stem = file
            .file_stem()
            .ok_or_else(|| Error::msg("history file has no name"))?;
        let mut name = stem.to_os_string();
        name.push(".jpg");
        let thumb = self.thumbs.join(name);
        fs::write(&thumb, jpeg).map_err(|e| Error::io("write thumbnail", &thumb, e))?;
        Ok(thumb)
    }
}

fn base36(mut value: u64) -> String {
    if value == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while value != 0 {
        digits.push(char::from_digit((value % 36) as u32, 36).unwrap());
        value /= 36;
    }
    digits.into_iter().rev().collect()
}

fn filtered(items: &[HistoryEntry], limit: usize, filter: HistoryFilter) -> Vec<HistoryEntry> {
    items
        .iter()
        .filter(|entry| match filter {
            HistoryFilter::All => true,
            HistoryFilter::Images => !entry.is_recording(),
            HistoryFilter::Recordings => entry.is_recording(),
            HistoryFilter::Audio => entry.kind.as_deref() == Some("audio"),
            HistoryFilter::Video => entry.kind.as_deref() == Some("video"),
        })
        .take(limit)
        .cloned()
        .collect()
}

impl HistoryStore for FileHistory {
    fn save_png(
        &self,
        png: &[u8],
        image: &Frame,
        source: &str,
        dir: Option<&Path>,
    ) -> Result<HistoryEntry> {
        if image.width() == 0 || image.height() == 0 {
            return Err(Error::msg("cannot save an empty screenshot"));
        }
        let settings = self.settings.get();
        let save_dir = dir
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(&settings.save_dir);
        fs::create_dir_all(save_dir)
            .map_err(|e| Error::io("create screenshot directory", save_dir, e))?;
        let now = (self.clock)();
        let base = ts_name(&settings.file_name_pattern, now.naive_local());
        let mut suffix = 1u64;
        let file = loop {
            let file = save_dir.join(if suffix == 1 {
                format!("{base}.png")
            } else {
                format!("{base}-{suffix}.png")
            });
            // existsSync in Electron also counts directories as filename collisions.
            if file.exists() {
                suffix += 1;
                continue;
            }
            match OpenOptions::new().create_new(true).write(true).open(&file) {
                Ok(mut output) => {
                    output
                        .write_all(png)
                        .map_err(|e| Error::io("write screenshot", &file, e))?;
                    break file;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => suffix += 1,
                Err(e) => return Err(Error::io("create screenshot", &file, e)),
            }
        };
        let width = image.width().min(480);
        let height = ((f64::from(image.height()) * f64::from(width) / f64::from(image.width()))
            .round() as u32)
            .max(1);
        let thumb = self.write_thumb(&file, &image.resize(width, height)?.to_jpeg(82)?)?;
        self.add(HistoryEntry {
            id: String::new(),
            file,
            thumb: Some(thumb),
            width: image.width(),
            height: image.height(),
            ts: now.timestamp_millis(),
            source: source.into(),
            kind: Some("image".into()),
            duration: None,
            extra: Map::new(),
        })
    }

    fn add_recording(&self, done: &RecordingDone) -> Result<HistoryEntry> {
        let thumb = done
            .first_frame_jpeg
            .as_deref()
            .map(|jpeg| self.write_thumb(&done.file, jpeg))
            .transpose()?;
        let duration = if done.duration_sec.is_nan() {
            0.0
        } else {
            done.duration_sec
        };
        self.add(HistoryEntry {
            id: String::new(),
            file: done.file.clone(),
            thumb,
            width: done.width,
            height: done.height,
            ts: (self.clock)().timestamp_millis(),
            source: "record".into(),
            kind: Some(if done.audio_only { "audio" } else { "video" }.into()),
            duration: Some(crate::format::js_round(duration * 10.0) / 10.0),
            extra: Map::new(),
        })
    }

    fn list(&self, limit: usize, filter: HistoryFilter) -> Vec<HistoryEntry> {
        match self.try_list(limit, filter) {
            Ok(items) => items,
            Err(error) => {
                // Compatibility with the pre-existing, infallible port. Keep the error explicit;
                // try_list lets the caller decide how it is shown. Do not return dead files.
                eprintln!("history list failed: {error}");
                let items = self.items.lock().unwrap_or_else(|e| e.into_inner());
                let alive: Vec<_> = items
                    .iter()
                    .filter(|entry| entry.file.exists())
                    .cloned()
                    .collect();
                filtered(&alive, limit, filter)
            }
        }
    }

    fn try_list(&self, limit: usize, filter: HistoryFilter) -> Result<Vec<HistoryEntry>> {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        let alive: Vec<_> = items
            .iter()
            .filter(|entry| entry.file.exists())
            .cloned()
            .collect();
        if alive.len() != items.len() {
            storage::write_json(&self.index, &alive)?;
            *items = alive;
        }
        Ok(filtered(&items, limit, filter))
    }

    fn get(&self, id: &str) -> Option<HistoryEntry> {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
    }

    fn remove(&self, id: &str) -> Result<()> {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = items.iter().find(|entry| entry.id == id) {
            storage::remove_file(&entry.file)?;
            if let Some(thumb) = &entry.thumb {
                storage::remove_file(thumb)?;
            }
            let next: Vec<_> = items
                .iter()
                .filter(|entry| entry.id != id)
                .cloned()
                .collect();
            storage::write_json(&self.index, &next)?;
            *items = next;
        }
        Ok(())
    }

    fn clear(&self, delete_files: bool) -> Result<()> {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        if delete_files {
            for entry in items.iter() {
                storage::remove_file(&entry.file)?;
                if let Some(thumb) = &entry.thumb {
                    storage::remove_file(thumb)?;
                }
            }
        }
        storage::write_json(&self.index, &Vec::<HistoryEntry>::new())?;
        items.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{defaults, FileSettings};
    use crate::test_support::TestDir;
    use serde_json::json;

    fn store(dir: &TestDir) -> (FileHistory, Arc<FileSettings>) {
        let cfg = defaults(
            dir.path(),
            "en-US",
            "win32",
            Some(&dir.path().join("shots")),
        );
        let settings = Arc::new(FileSettings::new(dir.path(), cfg).unwrap());
        let now = DateTime::parse_from_rfc3339("2026-11-05T00:02:03.456+03:00").unwrap();
        let history =
            FileHistory::with_clock(dir.path(), settings.clone(), Arc::new(move || now)).unwrap();
        (history, settings)
    }

    fn image(width: u32, height: u32) -> Frame {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x % 256) as u8,
                    (y % 256) as u8,
                    ((x + y) % 256) as u8,
                    255,
                ]);
            }
        }
        Frame::new(width, height, pixels).unwrap()
    }

    fn fixture(dir: &TestDir) -> Value {
        let mut fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/electron-history.json")).unwrap();
        for entry in fixture.as_array_mut().unwrap() {
            for key in ["file", "thumb"] {
                if let Some(path) = entry[key].as_str() {
                    let file = dir.path().join(Path::new(path).file_name().unwrap());
                    fs::write(&file, b"synthetic fixture").unwrap();
                    entry[key] = serde_json::to_value(file).unwrap();
                }
            }
        }
        fs::write(
            dir.path().join("history.json"),
            serde_json::to_vec(&fixture).unwrap(),
        )
        .unwrap();
        fixture
    }

    #[test]
    fn an_old_entry_without_kind_is_an_image_and_keeps_unknown_keys() {
        let json = r#"{"id":"a","file":"C:\\x.png","thumb":null,"width":10,"height":20,"ts":5,"source":"region","custom":1}"#;
        let entry: HistoryEntry = serde_json::from_str(json).unwrap();
        assert!(!entry.is_recording());
        assert_eq!(entry.extra.get("custom"), Some(&Value::from(1)));
        let back = serde_json::to_value(&entry).unwrap();
        assert_eq!(back.get("custom"), Some(&Value::from(1)));
        assert!(back.get("kind").is_none());
    }

    #[test]
    fn file_history_reads_electron_index_including_entries_without_kind() {
        let dir = TestDir::new();
        let expected = fixture(&dir);
        let (history, settings) = store(&dir);
        let entries = history.try_list(100, HistoryFilter::All).unwrap();
        assert_eq!(serde_json::to_value(&entries).unwrap(), expected);
        assert_eq!(history.list(1, HistoryFilter::Images)[0].id, "legacy");
        assert_eq!(
            history.list(1, HistoryFilter::Recordings)[0].id,
            "video",
            "filter before limit"
        );
        assert_eq!(history.list(10, HistoryFilter::Audio)[0].id, "audio");
        assert_eq!(history.list(10, HistoryFilter::Video)[0].id, "video");
        assert!(history.list(0, HistoryFilter::All).is_empty());
        assert_eq!(history.last().unwrap().id, "legacy");
        assert!(history.get("missing").is_none());
        let update = history
            .update("video", json!({"gif": "synthetic.gif", "duration": 15.5}))
            .unwrap()
            .unwrap();
        assert_eq!(update.extra["future"], 9);
        assert_eq!(update.extra["gif"], "synthetic.gif");
        let reloaded = FileHistory::new(dir.path(), settings).unwrap();
        let old = reloaded.get("legacy").unwrap();
        assert!(old.kind.is_none());
        assert_eq!(old.extra["future"], json!({"note": "kept"}));
        assert!(serde_json::to_value(old).unwrap().get("kind").is_none());
    }

    #[test]
    fn local_filename_tokens_and_numbered_collisions_match_js() {
        let dir = TestDir::new();
        let (history, settings) = store(&dir);
        let frame = image(32, 16);
        let png = frame.to_png().unwrap();
        let entries: Vec<_> = (0..3)
            .map(|_| history.save_png(&png, &frame, "region", None).unwrap())
            .collect();
        let expected = [
            "Shot_2026-11-05_00-02-03.png",
            "Shot_2026-11-05_00-02-03-2.png",
            "Shot_2026-11-05_00-02-03-3.png",
        ];
        for (entry, name) in entries.iter().zip(expected) {
            assert_eq!(entry.file.file_name().unwrap(), name);
            assert_eq!(fs::read(&entry.file).unwrap(), png);
            assert_eq!(
                entry.ts,
                DateTime::parse_from_rfc3339("2026-11-04T21:02:03.456Z")
                    .unwrap()
                    .timestamp_millis()
            );
            assert_eq!((entry.width, entry.height), (32, 16));
            assert_eq!(entry.kind.as_deref(), Some("image"));
            assert_eq!(entry.source, "region");
            assert!(entry.duration.is_none());
        }
        assert_ne!(entries[0].id, entries[1].id);
        assert!(entries.iter().all(|entry| entry
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())));
        let newest = history.try_list(100, HistoryFilter::All).unwrap();
        assert_eq!(
            newest.iter().map(|e| &e.id).collect::<Vec<_>>(),
            entries.iter().rev().map(|e| &e.id).collect::<Vec<_>>()
        );
        settings
            .set(json!({"fileNamePattern": "Custom_{time}_{date}_{date}_{time}"}))
            .unwrap();
        let overridden = dir.path().join("override");
        let entry = history
            .save_png(&png, &frame, "full", Some(&overridden))
            .unwrap();
        assert_eq!(
            entry.file,
            overridden.join("Custom_00-02-03_2026-11-05_{date}_{time}.png")
        );
        assert_eq!(entry.source, "full");
    }

    #[test]
    fn thumbnails_are_jpeg_quality_82_at_most_480_wide_without_upscaling() {
        let dir = TestDir::new();
        let (history, _) = store(&dir);
        for (width, height, expected_width, expected_height) in
            [(800, 200, 480, 120), (32, 16, 32, 16)]
        {
            let frame = image(width, height);
            let entry = history
                .save_png(&frame.to_png().unwrap(), &frame, "region", None)
                .unwrap();
            let thumb = entry.thumb.unwrap();
            assert_eq!(thumb.parent(), Some(dir.path().join("thumbs").as_path()));
            assert_eq!(thumb.file_stem(), entry.file.file_stem());
            let jpeg = fs::read(thumb).unwrap();
            assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
            let decoded = image::load_from_memory(&jpeg).unwrap();
            assert_eq!(
                (decoded.width(), decoded.height()),
                (expected_width, expected_height)
            );
            let quantization = jpeg
                .windows(2)
                .position(|bytes| bytes == [0xff, 0xdb])
                .unwrap();
            // JPEG's luminance DQT in zigzag order for quality 82 (baseline table scaled by 36%).
            assert_eq!(
                &jpeg[quantization + 5..quantization + 13],
                &[6, 4, 4, 5, 4, 4, 6, 5]
            );
        }
    }

    #[test]
    fn filename_collision_with_a_directory_is_skipped() {
        let dir = TestDir::new();
        let (history, settings) = store(&dir);
        let path = settings.get().save_dir.join("Shot_2026-11-05_00-02-03.png");
        fs::create_dir_all(&path).unwrap();
        let frame = image(10, 10);
        let entry = history
            .save_png(&frame.to_png().unwrap(), &frame, "region", None)
            .unwrap();
        assert_eq!(
            entry.file.file_name().unwrap(),
            "Shot_2026-11-05_00-02-03-2.png"
        );
        assert!(path.is_dir());
    }

    #[test]
    fn recording_metadata_thumbnails_and_duration_rounding_match_js() {
        let dir = TestDir::new();
        let (history, _) = store(&dir);
        let video = dir.path().join("rec.part.mp4");
        fs::write(&video, b"synthetic video").unwrap();
        let jpeg = image(16, 9).to_jpeg(82).unwrap();
        let done = RecordingDone {
            file: video,
            audio_only: false,
            first_frame_jpeg: Some(jpeg.clone()),
            width: 1920,
            height: 1080,
            duration_sec: 12.25,
        };
        let entry = history.add_recording(&done).unwrap();
        assert_eq!(entry.kind.as_deref(), Some("video"));
        assert_eq!(entry.source, "record");
        assert_eq!(entry.duration, Some(12.3));
        assert_eq!(
            entry.thumb.as_ref().unwrap().file_name().unwrap(),
            "rec.part.jpg"
        );
        assert_eq!(fs::read(entry.thumb.unwrap()).unwrap(), jpeg);
        let audio = dir.path().join("call.m4a");
        fs::write(&audio, b"synthetic audio").unwrap();
        let done = RecordingDone {
            file: audio,
            audio_only: true,
            first_frame_jpeg: None,
            width: 0,
            height: 0,
            duration_sec: 0.0,
        };
        let entry = history.add_recording(&done).unwrap();
        assert_eq!(entry.kind.as_deref(), Some("audio"));
        assert_eq!(entry.duration, Some(0.0));
        assert!(entry.thumb.is_none());
        assert_eq!(history.list(1, HistoryFilter::Recordings)[0].id, entry.id);
    }

    #[test]
    fn adding_entry_caps_index_at_500_without_deleting_older_files() {
        let dir = TestDir::new();
        let file = dir.path().join("old.png");
        fs::write(&file, b"synthetic image").unwrap();
        let old: Vec<_> = (0..500).map(|i| json!({"id": format!("old{i}"), "file": file, "thumb": null, "width": 1, "height": 1, "ts": i, "source": "region"})).collect();
        fs::write(
            dir.path().join("history.json"),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        let (history, _) = store(&dir);
        let frame = image(2, 2);
        let latest = history
            .save_png(&frame.to_png().unwrap(), &frame, "region", None)
            .unwrap();
        let entries = history.try_list(usize::MAX, HistoryFilter::All).unwrap();
        assert_eq!(entries.len(), 500);
        assert_eq!(entries[0].id, latest.id);
        assert_eq!(entries[499].id, "old498");
        assert!(history.get("old499").is_none());
        assert!(file.exists());
        let persisted: Value =
            serde_json::from_slice(&fs::read(dir.path().join("history.json")).unwrap()).unwrap();
        assert_eq!(persisted.as_array().unwrap().len(), 500);
    }

    #[test]
    fn listing_prunes_missing_files_and_persists_the_index() {
        let dir = TestDir::new();
        fixture(&dir);
        let (history, settings) = store(&dir);
        let missing = history.get("video").unwrap();
        fs::remove_file(&missing.file).unwrap();
        assert_eq!(
            history
                .try_list(100, HistoryFilter::Recordings)
                .unwrap()
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["audio"]
        );
        assert!(history.get("video").is_none());
        assert!(FileHistory::new(dir.path(), settings)
            .unwrap()
            .get("video")
            .is_none());
        assert!(
            missing.thumb.unwrap().exists(),
            "listing does not delete orphaned thumbs in JS"
        );
    }

    #[test]
    fn remove_and_clear_respect_file_deletion_contract() {
        let dir = TestDir::new();
        fixture(&dir);
        let (history, _) = store(&dir);
        let video = history.get("video").unwrap();
        history.remove("video").unwrap();
        assert!(!video.file.exists());
        assert!(!video.thumb.unwrap().exists());
        assert!(history.get("video").is_none());
        history.remove("absent").unwrap();
        let image = history.get("legacy").unwrap().file;
        history.clear(false).unwrap();
        assert!(image.exists());
        assert!(history.last().is_none());
        fixture(&dir);
        let (history, _) = store(&dir);
        history.clear(true).unwrap();
        assert!(!image.exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("history.json")).unwrap(),
            "[]"
        );
    }

    #[test]
    fn corrupt_index_uses_empty_history_and_persistence_errors_are_explicit() {
        let dir = TestDir::new();
        fs::write(dir.path().join("history.json"), b"not JSON").unwrap();
        let (history, _) = store(&dir);
        assert!(history.list(100, HistoryFilter::All).is_empty());
        fixture(&dir);
        let (history, _) = store(&dir);
        fs::remove_file(dir.path().join("legacy.png")).unwrap();
        let index = dir.path().join("history.json");
        fs::remove_file(&index).unwrap();
        fs::create_dir(&index).unwrap();
        let port: &dyn HistoryStore = &history;
        assert!(matches!(
            port.try_list(100, HistoryFilter::All),
            Err(Error::Io { .. })
        ));
        fs::remove_dir(index).unwrap();
        assert_eq!(
            port.try_list(100, HistoryFilter::All).unwrap().len(),
            2,
            "prune retries after write error"
        );
    }

    #[test]
    fn thumbnail_write_failure_is_reported() {
        let dir = TestDir::new();
        let (history, _) = store(&dir);
        fs::write(dir.path().join("thumbs"), b"not a directory").unwrap();
        let frame = image(10, 10);
        assert!(matches!(
            history.save_png(&frame.to_png().unwrap(), &frame, "region", None),
            Err(Error::Io { .. })
        ));
        assert!(history.last().is_none());
    }
}
