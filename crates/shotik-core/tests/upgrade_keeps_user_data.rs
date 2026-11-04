//! A user upgrading from Electron loses nothing (CONTRACT.md, Data). Damaged parts of a stored file
//! cost those parts alone, and a real profile can be checked by hand:
//!
//! ```text
//! cargo test -p shotik-core real_profile -- --ignored --nocapture
//! ```
//!
//! The real check works on a copy and prints names and counts only: settings can hold a DeepL key.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use shotik_core::history::FileHistory;
use shotik_core::ports::{HistoryFilter, HistoryStore, SettingsStore};
use shotik_core::settings::{defaults, FileSettings};

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("shotik-core-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_defaults() -> shotik_core::Settings {
    defaults(Path::new("C:/Pictures"), "ru-RU", "win32", None)
}

#[test]
fn a_field_of_the_wrong_type_costs_only_that_field() {
    let dir = temp_dir("bad-field");
    fs::write(
        dir.join("settings.json"),
        json!({
            "saveDir": "D:/Shots",
            "showToast": "yes",
            "hotkeys": { "region": "F13", "record": null },
            "translate": { "deeplKey": "secret-value", "provider": "deepl" },
            "recording": { "fps": "sixty", "countdown": false },
            "mcp": { "port": 70000 },
            "somethingNew": { "kept": true }
        })
        .to_string(),
    )
    .unwrap();

    let store = FileSettings::new(&dir, test_defaults()).unwrap();
    let got = store.get();

    assert_eq!(got.save_dir, Path::new("D:/Shots"));
    assert_eq!(got.hotkeys.region, "F13");
    assert_eq!(got.translate.deepl_key, "secret-value");
    assert_eq!(got.translate.provider, "deepl");
    assert!(!got.recording.countdown);

    let defaults = test_defaults();
    assert_eq!(got.show_toast, defaults.show_toast);
    assert_eq!(got.hotkeys.record, defaults.hotkeys.record);
    assert_eq!(got.recording.fps, defaults.recording.fps);
    assert_eq!(got.mcp.port, defaults.mcp.port);

    let mut ignored = store.ignored_fields().to_vec();
    ignored.sort();
    assert_eq!(ignored, ["hotkeys.record", "mcp.port", "recording.fps", "showToast"]);

    // The next save keeps everything that was valid, unknown keys included.
    store.set(json!({ "autoSave": false })).unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(saved["saveDir"], "D:/Shots");
    assert_eq!(saved["translate"]["deeplKey"], "secret-value");
    assert_eq!(saved["somethingNew"]["kept"], true);
    assert_eq!(saved["autoSave"], false);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unreadable_settings_file_gives_defaults() {
    let dir = temp_dir("corrupt");
    fs::write(dir.join("settings.json"), "{ not json").unwrap();
    let store = FileSettings::new(&dir, test_defaults()).unwrap();
    assert_eq!(store.get().mcp.port, 7464);
    assert!(store.ignored_fields().is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unreadable_history_entry_costs_only_that_entry() {
    let dir = temp_dir("bad-entry");
    let shot = dir.join("shot.png");
    fs::write(&shot, b"png").unwrap();
    let shot = shot.to_str().unwrap();
    fs::write(
        dir.join("history.json"),
        json!([
            { "id": "a", "file": shot, "thumb": null, "width": 10, "height": 20, "ts": 3, "source": "region" },
            { "id": "broken", "file": 12345 },
            { "id": "c", "file": shot, "thumb": null, "width": 10, "height": 20, "ts": 1, "source": "mcp", "kind": "image" }
        ])
        .to_string(),
    )
    .unwrap();

    let settings = Arc::new(FileSettings::new(&dir, test_defaults()).unwrap());
    let history = FileHistory::new(&dir, settings).unwrap();
    assert_eq!(history.skipped_entries(), 1);
    let ids: Vec<String> = history.list(10, HistoryFilter::All).into_iter().map(|e| e.id).collect();
    assert_eq!(ids, ["a", "c"]);
    fs::remove_dir_all(&dir).unwrap();
}

/// By hand, against the real profile in `%APPDATA%\Shotik`.
#[test]
#[ignore = "needs a real Electron profile; see the module docs"]
fn real_profile() {
    let appdata = std::env::var("APPDATA").expect("APPDATA");
    let profile = Path::new(&appdata).join("Shotik");
    let dir = temp_dir("real");
    for name in ["settings.json", "history.json"] {
        fs::copy(profile.join(name), dir.join(name)).expect(name);
    }
    let original: Value = serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap();
    let stored_entries = serde_json::from_slice::<Value>(&fs::read(dir.join("history.json")).unwrap())
        .unwrap()
        .as_array()
        .map_or(0, Vec::len);

    let store = Arc::new(FileSettings::new(&dir, test_defaults()).unwrap());
    let history = FileHistory::new(&dir, store.clone()).unwrap();
    println!("settings keys stored: {}", original.as_object().map_or(0, |o| o.len()));
    println!("settings fields ignored: {:?}", store.ignored_fields());
    println!("history stored {stored_entries}, skipped {}", history.skipped_entries());

    // Every stored value survives a load and save, compared without printing any of them.
    store.set(json!({})).unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap();
    let mut changed = Vec::new();
    diff(&original, &saved, "", &mut changed);
    println!("settings fields changed by a round trip: {changed:?}");
    fs::remove_dir_all(&dir).unwrap();

    assert!(store.ignored_fields().is_empty(), "the real profile must load in full");
    assert_eq!(history.skipped_entries(), 0, "the real history must load in full");
    assert!(changed.is_empty(), "a round trip must keep every stored value");
}

fn diff(old: &Value, new: &Value, path: &str, out: &mut Vec<String>) {
    match old {
        Value::Object(map) => {
            for (key, value) in map {
                let field = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
                match new.get(key) {
                    Some(other) => diff(value, other, &field, out),
                    None => out.push(field),
                }
            }
        }
        _ if old != new => out.push(path.to_string()),
        _ => {}
    }
}
