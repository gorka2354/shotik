//! The settings model: the same JSON as `settings.json` of the Electron version, so an upgrade keeps
//! everything. Unknown keys survive a load and save (`extra`). Reference: `src/main/settings.js`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::geom::{DisplayId, Rect};
use crate::ports::SettingsStore;
use crate::{storage, Error, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotkeys {
    pub region: String,
    pub fullscreen: String,
    pub repeat_last: String,
    pub text_grab: String,
    pub record: String,
    pub record_audio: String,
    pub translate_selection: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateSettings {
    pub target: String,
    /// "free" or "deepl".
    pub provider: String,
    pub deepl_key: String,
    pub auto_bubble: bool,
    pub ocr_fallback: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSettings {
    pub fps: u32,
    pub format: String,
    pub system_audio: bool,
    pub microphone: bool,
    pub microphone_device_id: String,
    pub countdown: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioRecordingSettings {
    pub system_audio: bool,
    pub microphone: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpSettings {
    pub enabled: bool,
    pub port: u16,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastRegion {
    pub display_id: DisplayId,
    pub rect_phys: Rect,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub save_dir: PathBuf,
    pub file_name_pattern: String,
    pub auto_save: bool,
    pub show_toast: bool,
    pub hotkeys: Hotkeys,
    pub translate: TranslateSettings,
    pub recording: RecordingSettings,
    pub audio_recording: AudioRecordingSettings,
    pub mcp: McpSettings,
    /// "system", "en" or "ru".
    pub language: String,
    pub launch_at_startup: bool,
    #[serde(rename = "downscaleForAI")]
    pub downscale_for_ai: bool,
    pub last_region: Option<LastRegion>,
    /// Keys this version does not know.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// OS-dependent values are supplied by the composition root, including SHOTIK_SAVE_DIR.
pub fn defaults(
    pictures: &Path,
    locale: &str,
    platform: &str,
    save_dir: Option<&Path>,
) -> Settings {
    let hotkeys = if platform == "darwin" {
        [
            "Cmd+Shift+2",
            "Cmd+Shift+1",
            "Cmd+Shift+7",
            "Cmd+Shift+4",
            "Cmd+Shift+6",
            "Cmd+Shift+8",
            "Cmd+Alt+T",
        ]
    } else {
        [
            "PrintScreen",
            "Ctrl+PrintScreen",
            "Shift+PrintScreen",
            "Ctrl+Shift+PrintScreen",
            "Ctrl+Alt+PrintScreen",
            "Ctrl+Alt+Shift+R",
            "Ctrl+Alt+T",
        ]
    };
    Settings {
        save_dir: save_dir
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| pictures.join("Shotik")),
        file_name_pattern: "Shot_{date}_{time}".into(),
        auto_save: true,
        show_toast: true,
        hotkeys: Hotkeys {
            region: hotkeys[0].into(),
            fullscreen: hotkeys[1].into(),
            repeat_last: hotkeys[2].into(),
            text_grab: hotkeys[3].into(),
            record: hotkeys[4].into(),
            record_audio: hotkeys[5].into(),
            translate_selection: hotkeys[6].into(),
            extra: Map::new(),
        },
        translate: TranslateSettings {
            target: if locale.to_lowercase().starts_with("ru") {
                "ru"
            } else {
                "en"
            }
            .into(),
            provider: "free".into(),
            deepl_key: String::new(),
            auto_bubble: true,
            ocr_fallback: true,
            extra: Map::new(),
        },
        recording: RecordingSettings {
            fps: 30,
            format: "auto".into(),
            system_audio: false,
            microphone: false,
            microphone_device_id: "default".into(),
            countdown: true,
            extra: Map::new(),
        },
        audio_recording: AudioRecordingSettings {
            system_audio: true,
            microphone: true,
            extra: Map::new(),
        },
        mcp: McpSettings {
            enabled: true,
            port: 7464,
            extra: Map::new(),
        },
        language: "system".into(),
        launch_at_startup: false,
        downscale_for_ai: true,
        last_region: None,
        extra: Map::new(),
    }
}

/// `deepMerge`: recurse only when both values are objects; arrays and null replace a field.
/// At the root JS spreads the base and enumerates the override (including array indices).
pub fn deep_merge(base: &Value, over: &Value) -> Value {
    fn entries(value: &Value) -> Vec<(String, Value)> {
        match value {
            Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            Value::Array(values) => values
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone()))
                .collect(),
            Value::String(s) => s
                .chars()
                .enumerate()
                .map(|(i, c)| (i.to_string(), Value::String(c.to_string())))
                .collect(),
            _ => Vec::new(),
        }
    }
    let mut out = if base.is_array() {
        base.clone()
    } else {
        Value::Object(entries(base).into_iter().collect())
    };
    for (key, value) in entries(over) {
        let previous = match base {
            Value::Array(a) => key.parse::<usize>().ok().and_then(|i| a.get(i)),
            _ => base.get(&key),
        };
        let merged = match previous {
            Some(old) if old.is_object() && value.is_object() => deep_merge(old, &value),
            _ => value,
        };
        match &mut out {
            Value::Object(map) => {
                map.insert(key, merged);
            }
            Value::Array(array) => {
                // Non-index properties of a JS array do not survive JSON.stringify.
                if let Ok(i) = key.parse::<usize>() {
                    if i < u32::MAX as usize && i.to_string() == key {
                        if i >= array.len() {
                            array.resize(i + 1, Value::Null);
                        }
                        array[i] = merged;
                    }
                }
            }
            _ => unreachable!(),
        }
    }
    out
}

struct State {
    settings: Settings,
    // Keep unknown JSON at every depth, including fields inside lastRegion.rectPhys.
    json: Value,
}

pub struct FileSettings {
    file: PathBuf,
    state: Mutex<State>,
}

impl FileSettings {
    pub fn new(user_data: &Path, defaults: Settings) -> Result<Self> {
        let file = user_data.join("settings.json");
        let default_json = serde_json::to_value(&defaults)
            .map_err(|e| Error::msg(format!("cannot serialize settings: {e}")))?;
        let json = deep_merge(
            &default_json,
            &storage::read_json(&file)?.unwrap_or(Value::Null),
        );
        // Malformed files (including invalid known field types) behave as missing files.
        let (settings, json) = match serde_json::from_value(json.clone()) {
            Ok(settings) => (settings, json),
            Err(_) => (defaults, default_json),
        };
        Ok(Self {
            file,
            state: Mutex::new(State { settings, json }),
        })
    }
}

impl SettingsStore for FileSettings {
    fn get(&self) -> Settings {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .settings
            .clone()
    }

    fn set(&self, patch: Value) -> Result<Settings> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let json = deep_merge(&state.json, &patch);
        let settings: Settings = serde_json::from_value(json.clone())
            // Do not include field values: settings may contain credentials.
            .map_err(|_| Error::msg("invalid settings field type"))?;
        storage::write_json(&self.file, &json)?;
        *state = State {
            settings: settings.clone(),
            json,
        };
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::test_support::TestDir;

    fn cfg() -> Settings {
        defaults(Path::new("pictures"), "en-US", "win32", None)
    }

    #[test]
    fn electron_settings_load_and_save_keep_unknown_keys_at_every_level() {
        // Generated by src/main/settings.js get(), with mocked Electron paths and synthetic data.
        let original: Value =
            serde_json::from_str(include_str!("../tests/fixtures/electron-settings.json")).unwrap();
        let dir = TestDir::new();
        let file = dir.path().join("settings.json");
        fs::write(&file, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
        let store = FileSettings::new(dir.path(), cfg()).unwrap();
        assert_eq!(store.get().hotkeys.region, "Alt+F8");
        assert_eq!(store.get().translate.target, "de");
        assert_eq!(store.get().recording.fps, 60);
        assert_eq!(
            store.get().last_region.unwrap().rect_phys,
            Rect::new(-10, 3, 100, 80)
        );
        assert_eq!(store.get().hotkeys.extra["futureHotkey"], "Ctrl+F9");
        assert_eq!(
            store.get().translate.extra["futureTranslate"],
            json!({"mode": 2})
        );
        assert_eq!(
            store.get().recording.extra["futureRecorder"],
            json!(["x", 1])
        );
        assert_eq!(store.get().audio_recording.extra["futureAudio"], true);
        assert_eq!(store.get().mcp.extra["futureServer"], "kept");
        assert_eq!(store.get().extra["futureTop"], original["futureTop"]);
        let patch = json!({"showToast": false, "translate": {"target": "ru"}, "futureTop": {"added": "value"}});
        let expected = deep_merge(&original, &patch);
        store.set(patch).unwrap();
        let back: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        // Includes the original rectPhys keys and unknown fields inside that rectangle.
        assert_eq!(back, expected);
        assert_eq!(back["lastRegion"]["rectPhys"]["futureRect"], 7);
        assert_eq!(
            FileSettings::new(dir.path(), cfg()).unwrap().get(),
            store.get()
        );
        // A second replacement exercises rename over an existing settings.json on Windows.
        store.set(json!({"autoSave": true})).unwrap();
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary files remain"
        );
    }

    #[test]
    fn missing_corrupt_and_invalid_files_use_defaults_without_writing_on_read() {
        let dir = TestDir::new();
        let file = dir.path().join("settings.json");
        assert_eq!(FileSettings::new(dir.path(), cfg()).unwrap().get(), cfg());
        assert!(!file.exists());
        for bytes in [
            b"{".as_slice(),
            b"\xff",
            br#"{"hotkeys":null}"#,
            br#"{"mcp":{"port":"invalid"}}"#,
        ] {
            fs::write(&file, bytes).unwrap();
            assert_eq!(FileSettings::new(dir.path(), cfg()).unwrap().get(), cfg());
            assert_eq!(fs::read(&file).unwrap(), bytes);
        }
    }

    #[test]
    fn default_paths_locales_platform_hotkeys_and_flags_match_js() {
        let win = cfg();
        assert_eq!(win.save_dir, Path::new("pictures").join("Shotik"));
        assert_eq!(win.file_name_pattern, "Shot_{date}_{time}");
        assert_eq!(
            serde_json::to_value(win.hotkeys).unwrap(),
            json!({
                "region": "PrintScreen", "fullscreen": "Ctrl+PrintScreen", "repeatLast": "Shift+PrintScreen",
                "textGrab": "Ctrl+Shift+PrintScreen", "record": "Ctrl+Alt+PrintScreen", "recordAudio": "Ctrl+Alt+Shift+R", "translateSelection": "Ctrl+Alt+T"
            })
        );
        let mac = defaults(
            Path::new("pictures"),
            "RU-ru",
            "darwin",
            Some(Path::new("override")),
        );
        assert_eq!(mac.save_dir, Path::new("override"));
        assert_eq!(mac.translate.target, "ru");
        assert_eq!(
            serde_json::to_value(mac.hotkeys).unwrap(),
            json!({
                "region": "Cmd+Shift+2", "fullscreen": "Cmd+Shift+1", "repeatLast": "Cmd+Shift+7",
                "textGrab": "Cmd+Shift+4", "record": "Cmd+Shift+6", "recordAudio": "Cmd+Shift+8", "translateSelection": "Cmd+Alt+T"
            })
        );
        let empty_override = defaults(Path::new("pics"), "", "linux", Some(Path::new("")));
        assert_eq!(empty_override.save_dir, Path::new("pics").join("Shotik"));
        assert_eq!(empty_override.translate.target, "en");
        let json = serde_json::to_value(cfg()).unwrap();
        assert_eq!(
            json["translate"],
            json!({"target": "en", "provider": "free", "deeplKey": "", "autoBubble": true, "ocrFallback": true})
        );
        assert_eq!(
            json["recording"],
            json!({"fps": 30, "format": "auto", "systemAudio": false, "microphone": false, "microphoneDeviceId": "default", "countdown": true})
        );
        assert_eq!(
            json["audioRecording"],
            json!({"systemAudio": true, "microphone": true})
        );
        assert_eq!(json["mcp"], json!({"enabled": true, "port": 7464}));
        assert_eq!(json["lastRegion"], Value::Null);
        for key in ["autoSave", "showToast", "downscaleForAI"] {
            assert_eq!(json[key], true);
        }
        assert_eq!(json["launchAtStartup"], false);
        assert_eq!(json["language"], "system");
    }

    #[test]
    fn deep_merge_recurses_objects_but_replaces_arrays_null_and_scalars() {
        let base = json!({"a": {"x": 1, "keep": 2}, "b": [1, 2], "c": null, "d": 4});
        let over = json!({"a": {"x": null, "new": false}, "b": [3], "c": {"x": 1}, "d": {"x": 2}, "e": "new"});
        assert_eq!(
            deep_merge(&base, &over),
            json!({"a": {"x": null, "keep": 2, "new": false}, "b": [3], "c": {"x": 1}, "d": {"x": 2}, "e": "new"})
        );
        assert_eq!(deep_merge(&base, &Value::Null), base);
        assert_eq!(
            deep_merge(&json!({"a": [1, 2]}), &json!({"a": {"z": 1}})),
            json!({"a": {"z": 1}})
        );
        assert_eq!(
            deep_merge(&json!({"a": {"x": 1}}), &json!({"a": 0})),
            json!({"a": 0})
        );
        assert_eq!(deep_merge(&json!([1, 2]), &json!([3])), json!([3, 2]));
        assert_eq!(
            deep_merge(&json!({}), &json!([1, 2])),
            json!({"0": 1, "1": 2})
        );
        assert_eq!(
            deep_merge(&json!("ab"), &json!("x")),
            json!({"0": "x", "1": "b"})
        );
        assert_eq!(base["a"]["x"], 1, "does not mutate base");
    }

    #[test]
    fn failed_save_or_invalid_patch_keeps_previous_cache_and_cleans_temp() {
        let dir = TestDir::new();
        let store = FileSettings::new(dir.path(), cfg()).unwrap();
        store.set(json!({"language": "ru"})).unwrap();
        let before = store.get();
        let file = dir.path().join("settings.json");
        let bytes = fs::read(&file).unwrap();
        assert!(store.set(json!({"mcp": null})).is_err());
        assert_eq!(fs::read(&file).unwrap(), bytes);
        assert_eq!(store.get(), before);
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap(); // force rename failure without platform-specific permissions
        assert!(matches!(
            store.set(json!({"language": "en"})),
            Err(Error::Io { .. })
        ));
        assert_eq!(store.get(), before);
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temporary file removed after failure"
        );
    }

    #[test]
    fn concurrent_patches_do_not_lose_unknown_fields() {
        let dir = TestDir::new();
        let store = Arc::new(FileSettings::new(dir.path(), cfg()).unwrap());
        let workers: Vec<_> = (0..4)
            .map(|i| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || store.set(json!({format!("future{i}"): i})).unwrap())
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let back = FileSettings::new(dir.path(), cfg()).unwrap().get();
        for i in 0..4 {
            assert_eq!(back.extra[&format!("future{i}")], i);
        }
    }
}
