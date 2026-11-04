//! The settings model: the same JSON as `settings.json` of the Electron version, so an upgrade keeps
//! everything. Unknown keys survive a load and save (`extra`). Reference: `src/main/settings.js`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::geom::{DisplayId, Rect};

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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioRecordingSettings {
    pub system_audio: bool,
    pub microphone: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpSettings {
    pub enabled: bool,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastRegion {
    pub display_id: DisplayId,
    pub rect_phys: Rect,
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
