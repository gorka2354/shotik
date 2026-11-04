//! The history model: the same JSON as `history.json` of the Electron version.
//! Reference: `src/main/history.js`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
