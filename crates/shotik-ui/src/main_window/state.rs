//! Toolkit-free main-window state. Effects are interpreted by `view`, never by the reducer.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use shotik_core::accelerator::{to_accelerator, KeyEvent};
use shotik_core::ports::{AudioDevice, RecStatus};
use shotik_core::{Error, Result};

pub const HOTKEYS: [(&str, &str); 7] = [
    ("region", "hkRegion"),
    ("fullscreen", "hkFull"),
    ("repeatLast", "hkRepeat"),
    ("textGrab", "hkText"),
    ("record", "hkRecord"),
    ("recordAudio", "hkRecordAudio"),
    ("translateSelection", "hkTranslate"),
];
pub const LANGUAGES: [(&str, &str); 13] = [
    ("en", "English"),
    ("ru", "Русский"),
    ("uk", "Українська"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("fr", "Français"),
    ("it", "Italiano"),
    ("pt", "Português"),
    ("pl", "Polski"),
    ("tr", "Türkçe"),
    ("zh", "中文"),
    ("ja", "日本語"),
    ("ko", "한국어"),
];

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Page {
    History,
    Text,
    Claude,
    Settings,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextState {
    pub input: String,
    pub result: String,
    pub result_visible: bool,
    pub loading: bool,
    #[serde(skip)]
    pub target: String,
    #[serde(skip)]
    pub target_touched: bool,
    #[serde(skip)]
    pub meta: String,
    #[serde(skip)]
    pub last_result: String,
    #[serde(skip)]
    pub request: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneState {
    pub enabled: bool,
    pub saved_enabled: bool,
    pub device_row_visible: bool,
    pub device_id: String,
    pub device_label: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct State {
    pub ready: bool,
    pub page: Option<Page>,
    pub title: String,
    pub text: TextState,
    pub microphone: MicrophoneState,
    #[serde(skip)]
    pub settings: Value,
    #[serde(skip)]
    pub version: String,
    #[serde(skip)]
    pub mcp: Value,
    #[serde(skip)]
    pub logs: Vec<Value>,
    #[serde(skip)]
    pub history: Vec<Value>,
    #[serde(skip)]
    pub devices: Vec<AudioDevice>,
    #[serde(skip)]
    pub dictionary: Map<String, Value>,
    #[serde(skip)]
    pub lang: String,
    #[serde(skip)]
    pub hotkey_errors: Vec<Value>,
    #[serde(skip)]
    pub hotkey_focus: Option<String>,
    #[serde(skip)]
    pub recording: RecStatus,
    #[serde(skip)]
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
// `Loaded` is rare and short-lived; boxing it would only add noise at the one place that builds it.
#[allow(clippy::large_enum_variant)]
pub enum Action {
    Navigate {
        page: Page,
    },
    QuickTranslate {
        text: String,
    },
    SetTitle {
        title: String,
    },
    Reload,
    MicrophoneToggle,
    MicrophoneSelect {
        #[serde(rename = "deviceId")]
        device_id: String,
    },
    HotkeyFocus {
        field: String,
    },
    HotkeyBlur {
        field: String,
    },
    HistoryClear,
    HistoryPin {
        id: String,
    },
    #[serde(skip)]
    Loaded {
        app: Value,
        dictionary: Map<String, Value>,
        lang: String,
        devices: Vec<AudioDevice>,
        history: Vec<Value>,
        recording: RecStatus,
    },
    #[serde(skip)]
    SettingsSaved(Value),
    #[serde(skip)]
    SettingsPatch(Value),
    #[serde(skip)]
    Input(String),
    #[serde(skip)]
    Target(String),
    #[serde(skip)]
    Translate,
    #[serde(skip)]
    Translated {
        request: u64,
        result: std::result::Result<Value, String>,
    },
    #[serde(skip)]
    CopyTranslation,
    #[serde(skip)]
    CopyCommand,
    #[serde(skip)]
    Capture(String),
    #[serde(skip)]
    History {
        id: String,
        operation: String,
    },
    #[serde(skip)]
    HistoryLoaded(Vec<Value>),
    #[serde(skip)]
    Devices(Vec<AudioDevice>),
    #[serde(skip)]
    McpLog(Value),
    #[serde(skip)]
    McpStatus(Value),
    #[serde(skip)]
    RecState(RecStatus),
    #[serde(skip)]
    HotkeyRecorded {
        accelerator: Option<String>,
    },
    #[serde(skip)]
    HotkeysResumed(Vec<Value>),
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    Reload,
    Save(Value),
    Title(String),
    Translate {
        text: String,
        target: String,
        request: u64,
    },
    CopyText {
        text: String,
        command: bool,
    },
    SuspendHotkeys,
    ResumeHotkeys,
    Capture(String),
    History {
        id: String,
        operation: String,
    },
    ClearHistory,
}

impl State {
    pub fn tr(&self, key: &str) -> String {
        self.dictionary
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(key)
            .into()
    }
    pub fn format(&self, key: &str, args: &[&str]) -> String {
        let mut s = self.tr(key);
        for (i, arg) in args.iter().enumerate() {
            s = s.replace(&format!("{{{i}}}"), arg);
        }
        s
    }
    pub fn setting(&self, pointer: &str) -> &Value {
        self.settings.pointer(pointer).unwrap_or(&Value::Null)
    }
    pub fn string(&self, pointer: &str) -> String {
        self.setting(pointer).as_str().unwrap_or("").into()
    }
    pub fn boolean(&self, pointer: &str) -> bool {
        self.setting(pointer).as_bool().unwrap_or(false)
    }
    pub fn command(&self) -> String {
        format!(
            "claude mcp add shotik --transport http http://127.0.0.1:{}/mcp",
            self.setting("/mcp/port").as_u64().unwrap_or(7464)
        )
    }
    pub fn microphone_options(&self) -> Vec<(String, String)> {
        let mut options = vec![("default".into(), self.tr("recMicrophoneDefault"))];
        for (i, device) in self
            .devices
            .iter()
            .filter(|d| !d.id.is_empty() && d.id != "default")
            .enumerate()
        {
            let label = if device.name.is_empty() {
                self.format("recMicrophoneNumber", &[&(i + 1).to_string()])
            } else {
                device.name.clone()
            };
            options.push((device.id.clone(), label));
        }
        let id = self.string("/recording/microphoneDeviceId");
        if !id.is_empty() && !options.iter().any(|(value, _)| value == &id) {
            options.push((id, self.tr("recMicrophoneUnavailable")));
        }
        options
    }
    fn refresh_settings(&mut self) {
        let enabled = self.boolean("/recording/microphone");
        let id = self.string("/recording/microphoneDeviceId");
        let id = if id.is_empty() { "default".into() } else { id };
        let label = self
            .microphone_options()
            .into_iter()
            .find(|(value, _)| value == &id)
            .map(|(_, label)| label)
            .unwrap_or_default();
        self.microphone = MicrophoneState {
            enabled,
            saved_enabled: enabled,
            device_row_visible: enabled || self.boolean("/audioRecording/microphone"),
            device_id: id,
            device_label: label,
        };
        if !self.text.target_touched {
            self.text.target = self
                .setting("/translate/target")
                .as_str()
                .unwrap_or("en")
                .into();
        }
    }
    fn apply_app(&mut self, app: Value) {
        if let Some(settings) = app.get("settings") {
            self.settings = settings.clone();
        }
        if let Some(mcp) = app.get("mcp") {
            self.mcp = mcp.clone();
        }
        if let Some(errors) = app.get("hotkeyErrors").and_then(Value::as_array) {
            self.hotkey_errors = errors.clone();
        }
        self.refresh_settings();
    }
    fn blur_hotkey(&mut self, effects: &mut Vec<Effect>) {
        if self.hotkey_focus.take().is_some() {
            effects.push(Effect::ResumeHotkeys);
        }
    }
}

pub fn reduce(state: &mut State, action: Action) -> Result<Vec<Effect>> {
    let mut effects = Vec::new();
    match action {
        Action::Navigate { page } => {
            if page != Page::Settings {
                state.blur_hotkey(&mut effects);
            }
            state.page = Some(page);
        }
        Action::SetTitle { title } => {
            if title.contains('\0') {
                return Err(Error::msg("a window title cannot contain NUL"));
            }
            state.title = title.clone();
            effects.push(Effect::Title(title));
        }
        Action::Reload => {
            state.blur_hotkey(&mut effects);
            state.ready = false;
            state.revision += 1;
            // Invalidate translations still in flight before reconstructing the view.
            state.text.request += 1;
            effects.push(Effect::Reload);
        }
        Action::Loaded {
            app,
            dictionary,
            lang,
            devices,
            history,
            recording,
        } => {
            state.dictionary = dictionary;
            state.lang = lang;
            state.devices = devices;
            state.version = app
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
            state.logs = app
                .get("mcpLog")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            state.logs.truncate(60);
            state.history = history;
            state.recording = recording;
            state.apply_app(app);
            state.page = Some(Page::History);
            state.title = "Shotik".into();
            let request = state.text.request;
            let target = state
                .setting("/translate/target")
                .as_str()
                .unwrap_or("en")
                .to_owned();
            state.text = TextState {
                request,
                target,
                ..Default::default()
            };
            state.ready = true;
            effects.push(Effect::Title(state.title.clone()));
        }
        Action::SettingsSaved(app) => state.apply_app(app),
        Action::SettingsPatch(patch) => effects.push(Effect::Save(patch)),
        Action::MicrophoneToggle => {
            state.microphone.enabled = !state.microphone.enabled;
            state.microphone.device_row_visible =
                state.microphone.enabled || state.boolean("/audioRecording/microphone");
            effects.push(Effect::Save(
                json!({"recording":{"microphone":state.microphone.enabled}}),
            ));
        }
        Action::MicrophoneSelect { device_id } => effects.push(Effect::Save(
            json!({"recording":{"microphoneDeviceId":device_id}}),
        )),
        Action::HotkeyFocus { field } => {
            if !HOTKEYS.iter().any(|(name, _)| name == &field) {
                return Err(Error::msg(format!("unknown hotkey field: {field}")));
            }
            state.page = Some(Page::Settings);
            if state.hotkey_focus.as_ref() != Some(&field) {
                state.blur_hotkey(&mut effects);
                state.hotkey_focus = Some(field);
                effects.push(Effect::SuspendHotkeys);
            }
        }
        Action::HotkeyBlur { field } => {
            if !HOTKEYS.iter().any(|(name, _)| name == &field) {
                return Err(Error::msg(format!("unknown hotkey field: {field}")));
            }
            if state.hotkey_focus.as_ref() == Some(&field) {
                state.blur_hotkey(&mut effects);
            }
        }
        Action::HotkeyRecorded { accelerator } => {
            if let (Some(field), Some(acc)) = (state.hotkey_focus.clone(), accelerator) {
                effects.push(Effect::Save(json!({"hotkeys":{field:acc}})));
            }
            state.blur_hotkey(&mut effects);
        }
        Action::HotkeysResumed(errors) => state.hotkey_errors = errors,
        Action::Input(input) => state.text.input = input,
        Action::Target(target) => {
            state.text.target = target;
            state.text.target_touched = true;
        }
        Action::QuickTranslate { text } => {
            state.blur_hotkey(&mut effects);
            state.page = Some(Page::Text);
            state.text.input = text;
            effects.extend(reduce(state, Action::Translate)?);
        }
        Action::Translate => {
            state.text.request += 1;
            let text = state.text.input.trim().to_owned();
            state.text.result_visible = !text.is_empty();
            state.text.loading = !text.is_empty();
            state.text.meta.clear();
            if !text.is_empty() {
                state.text.result = state.tr("txtTranslating");
                effects.push(Effect::Translate {
                    text,
                    target: state.text.target.clone(),
                    request: state.text.request,
                });
            }
        }
        Action::Translated { request, result } if request == state.text.request => {
            state.text.loading = false;
            match result {
                Ok(value) => {
                    state.text.last_result = value
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into();
                    state.text.result = if state.text.last_result.is_empty() {
                        "—".into()
                    } else {
                        state.text.last_result.clone()
                    };
                    let source = value.get("source").and_then(Value::as_str).unwrap_or("");
                    if !source.is_empty() && source != "auto" {
                        state.text.meta = format!(
                            "{source} → {}",
                            value.get("target").and_then(Value::as_str).unwrap_or("")
                        );
                    }
                }
                Err(_) => state.text.result = state.tr("txtTranslateFail"),
            }
        }
        Action::Translated { .. } => {}
        Action::CopyTranslation => {
            if !state.text.last_result.is_empty() {
                effects.push(Effect::CopyText {
                    text: state.text.last_result.clone(),
                    command: false,
                });
            }
        }
        Action::CopyCommand => effects.push(Effect::CopyText {
            text: state.command(),
            command: true,
        }),
        Action::Capture(mode) => {
            let mode = if state.recording.recording && matches!(mode.as_str(), "record" | "audio") {
                "stop".into()
            } else {
                mode
            };
            effects.push(Effect::Capture(mode));
        }
        Action::HistoryClear => effects.push(Effect::ClearHistory),
        Action::HistoryPin { id } => {
            if !state
                .history
                .iter()
                .any(|item| item.get("id").and_then(Value::as_str) == Some(&id))
            {
                return Err(Error::msg(format!("history entry not found: {id}")));
            }
            effects.push(Effect::History {
                id,
                operation: "pin".into(),
            });
        }
        Action::History { id, operation } => effects.push(Effect::History { id, operation }),
        Action::HistoryLoaded(history) => state.history = history,
        Action::Devices(devices) => {
            state.devices = devices;
            state.refresh_settings();
        }
        Action::McpLog(entry) => {
            state.logs.insert(0, entry);
            state.logs.truncate(60);
        }
        Action::McpStatus(status) => state.mcp = status,
        Action::RecState(recording) => state.recording = recording,
    }
    Ok(effects)
}

/// Windows/winit supplies the physical code. The dictionary/keyboard layout never supplies it.
pub fn recorded_accelerator(event: &KeyEvent<'_>, platform: &str) -> Option<String> {
    to_accelerator(event, platform)
}

/// Remove the trusted dictionary's HTML markup for Slint's plain text surfaces.
pub fn plain_text(html: &str) -> String {
    let html = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    let mut tag = false;
    html.chars()
        .filter(|c| match c {
            '<' => {
                tag = true;
                false
            }
            '>' => {
                tag = false;
                false
            }
            _ => !tag,
        })
        .collect()
}

pub fn short(acc: &str) -> String {
    if acc.is_empty() {
        "—".into()
    } else {
        acc.replace("PrintScreen", "PrtSc")
    }
}

/// The number field's `Number(value) || 7464`, then the JS 1024..65535 clamp.
/// The core's MCP port is an integer, so a fractional port remains an explicit input error.
pub fn port_from_input(value: &str) -> Result<u16> {
    let value = value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n != 0.0)
        .unwrap_or(7464.0)
        .clamp(1024.0, 65535.0);
    if value.fract() != 0.0 {
        return Err(Error::msg("the MCP port must be an integer"));
    }
    Ok(value as u16)
}

pub fn relative_time(state: &State, timestamp: i64, now: i64) -> Option<String> {
    let seconds = (now - timestamp).div_euclid(1000);
    if seconds < 60 {
        Some(state.tr("justNow"))
    } else if seconds < 3600 {
        Some(state.format("minAgo", &[&(seconds / 60).to_string()]))
    } else if seconds < 86400 {
        Some(state.format("hourAgo", &[&(seconds / 3600).to_string()]))
    } else {
        None
    } // Local calendar formatting belongs to the view.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded() -> State {
        let settings =
            shotik_core::settings::defaults(std::path::Path::new("preview"), "en", "win32", None);
        let mut state = State::default();
        reduce(&mut state, Action::Loaded { app: json!({"settings":settings,"version":"test","mcp":{"running":true,"port":7464}}),
            dictionary: shotik_core::i18n::dict("en"), lang:"en".into(), devices:vec![], history:vec![json!({"id":"fixture-1"})], recording:RecStatus::default() }).unwrap();
        state
    }
    #[test]
    fn protocol_state_has_exact_public_fields_and_ready_barrier() {
        assert!(!State::default().ready);
        let state = loaded();
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(
            value,
            json!({"ready":true,"page":"history","title":"Shotik","text":{"input":"","result":"","resultVisible":false,"loading":false},
            "microphone":{"enabled":false,"savedEnabled":false,"deviceRowVisible":true,"deviceId":"default","deviceLabel":"System default"}})
        );
    }
    #[test]
    fn all_navigation_and_native_title_contract() {
        let mut state = loaded();
        for page in ["history", "text", "claude", "settings"] {
            let action = serde_json::from_value(json!({"type":"navigate","page":page})).unwrap();
            assert!(reduce(&mut state, action).unwrap().is_empty());
            assert_eq!(serde_json::to_value(state.page).unwrap(), page);
        }
        assert_eq!(
            reduce(
                &mut state,
                Action::SetTitle {
                    title: "GhostTarget".into()
                }
            )
            .unwrap(),
            vec![Effect::Title("GhostTarget".into())]
        );
        assert!(
            serde_json::from_value::<Action>(json!({"type":"navigate","page":"bogus"})).is_err()
        );
        assert!(serde_json::from_value::<Action>(json!({"type":"bogus"})).is_err());
    }
    #[test]
    fn translation_displays_loading_errors_and_ignores_stale_results() {
        let mut state = loaded();
        let effects = reduce(
            &mut state,
            Action::QuickTranslate {
                text: " hello world ".into(),
            },
        )
        .unwrap();
        let request = state.text.request;
        assert_eq!(
            effects,
            vec![Effect::Translate {
                text: "hello world".into(),
                target: "en".into(),
                request
            }]
        );
        assert!(state.text.result_visible && state.text.loading);
        assert_eq!(state.text.result, state.tr("txtTranslating"));
        reduce(
            &mut state,
            Action::Translated {
                request,
                result: Ok(json!({"text":"[stub] hello world","source":"stub","target":"en"})),
            },
        )
        .unwrap();
        assert_eq!(state.text.result, "[stub] hello world");
        assert!(!state.text.loading);
        reduce(
            &mut state,
            Action::Navigate {
                page: Page::Settings,
            },
        )
        .unwrap();
        assert!(state.text.result_visible);
        reduce(&mut state, Action::QuickTranslate { text: "new".into() }).unwrap();
        reduce(
            &mut state,
            Action::Translated {
                request,
                result: Ok(json!({"text":"old"})),
            },
        )
        .unwrap();
        assert!(state.text.loading);
        let request = state.text.request;
        reduce(
            &mut state,
            Action::Translated {
                request,
                result: Err("offline".into()),
            },
        )
        .unwrap();
        assert_eq!(state.text.result, state.tr("txtTranslateFail"));
        assert!(!state.text.loading);
        reduce(&mut state, Action::QuickTranslate { text: "   ".into() }).unwrap();
        assert!(!state.text.result_visible && !state.text.loading);
    }
    #[test]
    fn microphone_persists_and_retains_unavailable_localized_device() {
        let mut state = loaded();
        assert_eq!(
            reduce(&mut state, Action::MicrophoneToggle).unwrap(),
            vec![Effect::Save(json!({"recording":{"microphone":true}}))]
        );
        assert!(state.microphone.enabled);
        assert!(!state.microphone.saved_enabled);
        assert_eq!(
            reduce(
                &mut state,
                Action::MicrophoneSelect {
                    device_id: "unplugged-device".into()
                }
            )
            .unwrap(),
            vec![Effect::Save(
                json!({"recording":{"microphoneDeviceId":"unplugged-device"}})
            )]
        );
        state.settings["recording"]["microphone"] = json!(true);
        state.settings["recording"]["microphoneDeviceId"] = json!("unplugged-device");
        let saved = json!({"settings":state.settings.clone()});
        reduce(&mut state, Action::SettingsSaved(saved)).unwrap();
        assert!(state.microphone.saved_enabled && state.microphone.device_row_visible);
        assert_eq!(
            state.microphone.device_label,
            "Selected microphone is unavailable"
        );
        state.dictionary = shotik_core::i18n::dict("ru");
        state.refresh_settings();
        assert_eq!(
            state.microphone.device_label,
            "Выбранный микрофон недоступен"
        );
        reduce(&mut state, Action::Reload).unwrap();
        assert!(!state.ready);
    }
    #[test]
    fn hotkey_focus_resume_cancel_and_physical_russian_layout() {
        let mut state = loaded();
        assert_eq!(
            reduce(
                &mut state,
                Action::HotkeyFocus {
                    field: "region".into()
                }
            )
            .unwrap(),
            vec![Effect::SuspendHotkeys]
        );
        assert!(reduce(
            &mut state,
            Action::HotkeyFocus {
                field: "region".into()
            }
        )
        .unwrap()
        .is_empty());
        assert_eq!(state.page, Some(Page::Settings));
        let acc = recorded_accelerator(
            &KeyEvent {
                key: "ф",
                code: "KeyA",
                ctrl_key: true,
                ..Default::default()
            },
            "win32",
        );
        assert_eq!(acc.as_deref(), Some("Ctrl+A"));
        assert_eq!(
            reduce(&mut state, Action::HotkeyRecorded { accelerator: acc }).unwrap(),
            vec![
                Effect::Save(json!({"hotkeys":{"region":"Ctrl+A"}})),
                Effect::ResumeHotkeys
            ]
        );
        reduce(
            &mut state,
            Action::HotkeyFocus {
                field: "region".into(),
            },
        )
        .unwrap();
        assert_eq!(
            reduce(
                &mut state,
                Action::HotkeyBlur {
                    field: "region".into()
                }
            )
            .unwrap(),
            vec![Effect::ResumeHotkeys]
        );
        reduce(
            &mut state,
            Action::HotkeyFocus {
                field: "region".into(),
            },
        )
        .unwrap();
        assert_eq!(
            reduce(&mut state, Action::Navigate { page: Page::Text }).unwrap(),
            vec![Effect::ResumeHotkeys]
        );
        assert!(reduce(
            &mut state,
            Action::HotkeyFocus {
                field: "missing".into()
            }
        )
        .is_err());
    }
    #[test]
    fn history_actions_and_events_match_the_contract() {
        let mut state = loaded();
        assert_eq!(
            reduce(&mut state, Action::HistoryClear).unwrap(),
            vec![Effect::ClearHistory]
        );
        assert_eq!(
            reduce(
                &mut state,
                Action::HistoryPin {
                    id: "fixture-1".into()
                }
            )
            .unwrap(),
            vec![Effect::History {
                id: "fixture-1".into(),
                operation: "pin".into()
            }]
        );
        assert!(reduce(
            &mut state,
            Action::HistoryPin {
                id: "missing".into()
            }
        )
        .is_err());
        reduce(&mut state, Action::HistoryLoaded(vec![])).unwrap();
        assert!(state.history.is_empty());
        for i in 0..75 {
            reduce(&mut state, Action::McpLog(json!({"tool":i}))).unwrap();
        }
        assert_eq!(state.logs.len(), 60);
        assert_eq!(state.logs[0]["tool"], 74);
        reduce(
            &mut state,
            Action::RecState(RecStatus {
                recording: true,
                audio_only: Some(true),
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(
            reduce(&mut state, Action::Capture("record".into())).unwrap(),
            vec![Effect::Capture("stop".into())]
        );
    }
    #[test]
    fn relative_time_uses_the_current_dictionary() {
        let state = loaded();
        assert_eq!(relative_time(&state, 0, 2000), Some(state.tr("justNow")));
        assert_eq!(relative_time(&state, 0, 120000), Some("2 min ago".into()));
        assert_eq!(relative_time(&state, 0, 7200000), Some("2 h ago".into()));
        assert_eq!(relative_time(&state, 0, 86400000), None);
    }
    #[test]
    fn port_field_keeps_js_default_and_bounds() {
        for (input, expected) in [
            ("", 7464),
            ("0", 7464),
            ("invalid", 7464),
            ("-12", 1024),
            ("80", 1024),
            ("1e4", 10000),
            (" 9000 ", 9000),
            ("999999", 65535),
        ] {
            assert_eq!(port_from_input(input).unwrap(), expected);
        }
        assert!(port_from_input("1234.5").is_err());
    }
    #[test]
    fn dictionary_markup_keeps_the_empty_state_line_break() {
        let text = plain_text(&loaded().format("emptyBody", &["PrtSc"]));
        assert!(text.contains("lands\nin your clipboard"));
        assert_eq!(plain_text("<b>value</b><br/>next"), "value\nnext");
    }
    #[test]
    fn reload_resets_manual_target_and_invalidates_in_flight_translation() {
        let mut state = loaded();
        reduce(&mut state, Action::Target("de".into())).unwrap();
        reduce(
            &mut state,
            Action::QuickTranslate {
                text: "pending".into(),
            },
        )
        .unwrap();
        let request = state.text.request;
        reduce(&mut state, Action::Reload).unwrap();
        let app = json!({"settings":state.settings});
        reduce(
            &mut state,
            Action::Loaded {
                app,
                dictionary: shotik_core::i18n::dict("en"),
                lang: "en".into(),
                devices: vec![],
                history: vec![],
                recording: RecStatus::default(),
            },
        )
        .unwrap();
        assert_eq!(state.text.target, "en");
        assert!(!state.text.target_touched);
        reduce(
            &mut state,
            Action::Translated {
                request,
                result: Ok(json!({"text":"stale"})),
            },
        )
        .unwrap();
        assert!(!state.text.result_visible);
        assert!(state.text.result.is_empty());
    }
}
