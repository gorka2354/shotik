//! Slint and Win32 adapter. Only owned UI-thread objects live here; worker jobs carry plain data.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use chrono::{Local, TimeZone};
use serde_json::{json, Map, Value};
use shotik_core::accelerator::KeyEvent;
use shotik_core::ports::{MainEvent, Theme};
use shotik_core::{Error, Result};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, Image, ModelRc, SharedPixelBuffer, SharedString, VecModel};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::state::{self, Action, Effect, Page, State, HOTKEYS, LANGUAGES};
use crate::runtime::{self, Placement};
use crate::{MainCaptureButton, MainLogEntry, MainSection, MainSetting, MainShot, MainWindow};

struct View {
    ui: MainWindow,
    state: State,
    shown: bool,
    requested_visible: bool,
    thumbnails: HashMap<String, Image>,
    busy: HashSet<String>,
    dictionary: Rc<RefCell<Map<String, Value>>>,
    timer: slint::Timer,
    error: Option<String>,
    placement: Option<Placement>,
    ticks: u32,
}

thread_local! { static VIEW: RefCell<Option<View>> = const { RefCell::new(None) }; }
type Job = Box<dyn FnOnce() + Send>;
static WORKER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();

/// Persistence and shell operations stay ordered, particularly hotkey save -> real resume.
fn work(job: impl FnOnce() + Send + 'static) -> Result<()> {
    let sender = WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("shotik-main-host".into())
            .spawn(move || {
                for job in receiver {
                    job();
                }
            })
            .expect("start main-window host worker");
        sender
    });
    sender
        .send(Box::new(job))
        .map_err(|_| Error::msg("the main-window host worker stopped"))
}

fn report(result: Result<()>) {
    if let Err(error) = result {
        fail(error);
    }
}

fn fail(error: Error) {
    let message = error.to_string();
    eprintln!("main window: {message}");
    VIEW.with_borrow_mut(|view| {
        if let Some(view) = view {
            view.error = Some(message);
        }
    });
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    Rc::new(VecModel::from(rows)).into()
}
fn strings(rows: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    model(rows.into_iter().map(SharedString::from).collect())
}

fn ensure() -> Result<MainWindow> {
    if let Some(ui) = VIEW.with_borrow(|view| view.as_ref().map(|view| view.ui.clone_strong())) {
        return Ok(ui);
    }
    let ui =
        MainWindow::new().map_err(|e| Error::msg(format!("cannot create the main window: {e}")))?;
    crate::apply_theme!(&ui, &runtime::current_theme());
    let dictionary = Rc::new(RefCell::new(runtime::dictionary()));
    let lookup = dictionary.clone();
    ui.on_lookup(move |key| {
        state::plain_text(
            lookup
                .borrow()
                .get(key.as_str())
                .and_then(Value::as_str)
                .unwrap_or(key.as_str()),
        )
        .into()
    });
    ui.on_navigate(|page| {
        report(page_of(page.as_str()).and_then(|page| dispatch(Action::Navigate { page })));
    });
    ui.on_input_edited(|value| report(dispatch(Action::Input(value.into()))));
    ui.on_target_selected(|index| {
        if let Some((target, _)) = usize::try_from(index).ok().and_then(|i| LANGUAGES.get(i)) {
            report(dispatch(Action::Target((*target).into())));
        }
    });
    ui.on_translate(|| report(dispatch(Action::Translate)));
    ui.on_copy_text(|| report(dispatch(Action::CopyTranslation)));
    ui.on_copy_command(|| report(dispatch(Action::CopyCommand)));
    ui.on_capture(|mode| report(dispatch(Action::Capture(mode.into()))));
    ui.on_history_action(|id, operation| {
        report(dispatch(Action::History {
            id: id.into(),
            operation: operation.into(),
        }))
    });
    ui.on_setting(|key, value| report(setting(key.as_str(), value.as_str())));
    ui.on_hotkey_focus(|key, focused| {
        let field = key.strip_prefix("hotkeys.").unwrap_or(&key).to_owned();
        report(dispatch(if focused {
            Action::HotkeyFocus { field }
        } else {
            Action::HotkeyBlur { field }
        }));
    });
    ui.on_caption(|action| report(caption(action.as_str())));
    ui.window().on_close_requested(|| {
        runtime::on_ui(|| report(hide_now()));
        slint::CloseRequestResponse::KeepWindowShown
    });
    let mut modifiers = winit::keyboard::ModifiersState::empty();
    ui.window().on_winit_window_event(move |_, event| {
        match event {
            winit::event::WindowEvent::ModifiersChanged(value) => modifiers = value.state(),
            winit::event::WindowEvent::Focused(false) => {
                let field = VIEW.with_borrow(|view| {
                    view.as_ref()
                        .and_then(|view| view.state.hotkey_focus.clone())
                });
                if let Some(field) = field {
                    report(dispatch(Action::HotkeyBlur { field }));
                    blur_editor();
                }
            }
            winit::event::WindowEvent::KeyboardInput { event, .. } => {
                let recording = VIEW.with_borrow(|view| {
                    view.as_ref()
                        .is_some_and(|view| view.state.hotkey_focus.is_some())
                });
                if recording {
                    if event.state == winit::event::ElementState::Pressed
                        || event.physical_key
                            == winit::keyboard::PhysicalKey::Code(
                                winit::keyboard::KeyCode::PrintScreen,
                            )
                    {
                        if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
                            let code = format!("{code:?}");
                            let action = match code.as_str() {
                                "Escape" => Some(Action::HotkeyRecorded { accelerator: None }),
                                "Delete" | "Backspace" => Some(Action::HotkeyRecorded {
                                    accelerator: Some(String::new()),
                                }),
                                _ => state::recorded_accelerator(
                                    &KeyEvent {
                                        key: &code,
                                        code: &code,
                                        ctrl_key: modifiers.control_key(),
                                        alt_key: modifiers.alt_key(),
                                        shift_key: modifiers.shift_key(),
                                        meta_key: modifiers.super_key(),
                                    },
                                    "win32",
                                )
                                .map(|accelerator| {
                                    Action::HotkeyRecorded {
                                        accelerator: Some(accelerator),
                                    }
                                }),
                            };
                            if let Some(action) = action {
                                report(dispatch(action));
                                blur_editor();
                            }
                        }
                    }
                    return EventResult::PreventDefault;
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });
    VIEW.with_borrow_mut(|view| {
        *view = Some(View {
            ui: ui.clone_strong(),
            state: State {
                title: "Shotik".into(),
                dictionary: dictionary.borrow().clone(),
                ..Default::default()
            },
            shown: false,
            requested_visible: false,
            thumbnails: HashMap::new(),
            busy: HashSet::new(),
            dictionary: dictionary.clone(),
            timer: slint::Timer::default(),
            error: None,
            placement: None,
            ticks: 0,
        })
    });
    dispatch(Action::Reload)?;
    Ok(ui)
}

fn copied(command: bool) {
    VIEW.with_borrow(|view| {
        if let Some(view) = view {
            if command {
                view.ui.set_command_copied(true);
            } else {
                view.ui.set_text_copied(true);
            }
        }
    });
    slint::Timer::single_shot(
        Duration::from_millis(if command { 1500 } else { 1200 }),
        move || {
            VIEW.with_borrow(|view| {
                if let Some(view) = view {
                    if command {
                        view.ui.set_command_copied(false);
                    } else {
                        view.ui.set_text_copied(false);
                    }
                }
            });
        },
    );
}

/// Winit creates the initially hidden HWND on the next event-loop turn. Waiting locally keeps
/// the cold show path safe too: placement and no-activate styles precede the first show.
fn after_native(ui: MainWindow, next: impl FnOnce() + 'static) -> Result<()> {
    slint::spawn_local(async move {
        match ui.window().winit_window().await {
            Ok(_) => next(),
            Err(error) => fail(Error::msg(format!(
                "cannot create the native main window: {error}"
            ))),
        }
    })
    .map(|_| ())
    .map_err(|e| Error::msg(format!("cannot schedule main window creation: {e}")))
}

fn page_of(page: &str) -> Result<Page> {
    match page {
        "history" => Ok(Page::History),
        "text" => Ok(Page::Text),
        "claude" => Ok(Page::Claude),
        "settings" => Ok(Page::Settings),
        _ => Err(Error::msg(format!("unknown main page: {page}"))),
    }
}

fn dispatch(action: Action) -> Result<()> {
    let reset_scroll = matches!(action, Action::Loaded { .. });
    let (effects, repaint) = VIEW.with_borrow_mut(|view| {
        let view = view
            .as_mut()
            .ok_or_else(|| Error::msg("main window is absent"))?;
        if matches!(action, Action::Reload) {
            view.error = None;
        }
        if matches!(&action,Action::McpStatus(status) if &view.state.mcp == status)
            || matches!(&action,Action::Devices(devices) if &view.state.devices == devices)
        {
            return Ok((Vec::new(), false));
        }
        let repaint = !matches!(
            action,
            Action::SettingsPatch(_)
                | Action::MicrophoneSelect { .. }
                | Action::Capture(_)
                | Action::History { .. }
                | Action::HistoryPin { .. }
                | Action::HistoryClear
                | Action::CopyTranslation
                | Action::CopyCommand
        );
        state::reduce(&mut view.state, action).map(|effects| (effects, repaint))
    })?;
    if repaint {
        render();
    }
    if reset_scroll {
        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
        ui.invoke_reset_scroll();
    }
    for effect in effects {
        execute(effect)?;
    }
    Ok(())
}

fn execute(effect: Effect) -> Result<()> {
    let host = runtime::host().clone();
    match effect {
        Effect::Reload => {
            let revision = VIEW.with_borrow(|view| view.as_ref().unwrap().state.revision);
            work(move || {
                let app = host.app_state();
                let i18n = host.i18n();
                let devices = host.microphones();
                let history = host.history_list(200);
                let recording = host.rec_status();
                runtime::on_ui(move || {
                    let current = VIEW.with_borrow(|view| {
                        view.as_ref()
                            .is_some_and(|view| view.state.revision == revision)
                    });
                    if !current {
                        return;
                    }
                    match devices {
                        Ok(devices) => {
                            let ui =
                                VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                            report(after_native(ui, move || {
                                let current = VIEW.with_borrow(|view| {
                                    view.as_ref()
                                        .is_some_and(|view| view.state.revision == revision)
                                });
                                if current {
                                    report(dispatch(Action::Loaded {
                                        app,
                                        dictionary: i18n
                                            .get("dict")
                                            .and_then(Value::as_object)
                                            .cloned()
                                            .unwrap_or_default(),
                                        lang: i18n
                                            .get("lang")
                                            .and_then(Value::as_str)
                                            .unwrap_or("en")
                                            .into(),
                                        devices,
                                        history,
                                        recording,
                                    }));
                                    let show = VIEW.with_borrow(|view| {
                                        view.as_ref().is_some_and(|view| {
                                            view.requested_visible && !view.shown
                                        })
                                    });
                                    if show {
                                        report(show_requested_now());
                                    }
                                }
                            }));
                        }
                        Err(error) => fail(error),
                    }
                });
            })?;
        }
        Effect::Save(patch) => {
            let language = patch.get("language").is_some();
            work(move || {
                let result = host.settings_set(patch);
                runtime::on_ui(move || match result {
                    Ok(app) => {
                        report(dispatch(Action::SettingsSaved(app)));
                        if language {
                            report(dispatch(Action::Reload));
                        }
                    }
                    Err(error) => {
                        fail(error);
                        render();
                    }
                });
            })?;
        }
        Effect::Title(title) => {
            VIEW.with_borrow(|view| {
                if let Some(view) = view {
                    view.ui.set_native_title(title.clone().into());
                }
            });
            if let Some(hwnd) = VIEW.with_borrow(|view| {
                view.as_ref()
                    .and_then(|view| runtime::hwnd_of(view.ui.window()).ok())
            }) {
                let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
                // SAFETY: owned live HWND; UTF-16 buffer remains valid for the call.
                unsafe { SetWindowTextW(hwnd, PCWSTR(wide.as_ptr())) }
                    .map_err(|e| Error::msg(format!("cannot set the main title: {e}")))?;
            }
        }
        Effect::Translate {
            text,
            target,
            request,
        } => {
            work(move || {
                host.translate_text(
                    &text,
                    Some(&target),
                    Box::new(move |result| {
                        let result = result.map_err(|error| {
                            eprintln!("manual translation failed: {error}");
                            error.to_string()
                        });
                        runtime::on_ui(move || {
                            report(dispatch(Action::Translated { request, result }))
                        });
                    }),
                )
            })?;
        }
        Effect::CopyText { text, command } => work(move || {
            let result = host.copy_text(&text);
            runtime::on_ui(move || match result {
                Ok(()) => copied(command),
                Err(error) => fail(error),
            });
        })?,
        Effect::SuspendHotkeys => host.hotkeys_suspend(),
        Effect::ResumeHotkeys => work(move || {
            let errors = host
                .hotkeys_resume()
                .into_iter()
                .map(|error| json!({"name":error.name,"acc":error.acc}))
                .collect();
            runtime::on_ui(move || report(dispatch(Action::HotkeysResumed(errors))));
        })?,
        Effect::Capture(mode) => work(move || match mode.as_str() {
            "region" => host.capture_region(),
            "full" => host.capture_full(),
            "repeat" => host.capture_repeat(),
            "text" => host.capture_text(),
            "record" => host.capture_record(),
            "audio" => host.capture_record_audio(),
            "stop" => host.rec_stop(),
            "folder" => {
                if let Err(error) = host.open_save_dir() {
                    runtime::on_ui(move || fail(error));
                }
            }
            _ => {
                runtime::on_ui(move || fail(Error::msg(format!("unknown capture action: {mode}"))))
            }
        })?,
        Effect::ClearHistory => work(move || {
            let result = host.history_clear(false);
            let history = host.history_list(200);
            runtime::on_ui(move || match result {
                Ok(()) => report(dispatch(Action::HistoryLoaded(history))),
                Err(error) => fail(error),
            });
        })?,
        Effect::History { id, operation } => {
            if matches!(operation.as_str(), "ocr" | "gif") {
                VIEW.with_borrow_mut(|view| {
                    if let Some(view) = view {
                        view.busy.insert(id.clone());
                    }
                });
                render();
            }
            work(move || {
                let result = match operation.as_str() {
                    "copy" => require_item(host.history_copy(&id)),
                    "path" => require_item(host.history_copy_path(&id)),
                    "pin" => require_item(host.history_pin(&id)),
                    "folder" => host.history_reveal(&id),
                    "open" => host.history_open(&id),
                    "remove" => host.history_remove(&id),
                    "ocr" => {
                        let finished_id = id.clone();
                        host.history_ocr(
                            &id,
                            Box::new(move |result| {
                                runtime::on_ui(move || match result {
                                    Ok(_) => finish_busy(&finished_id),
                                    Err(error) => {
                                        finish_busy(&finished_id);
                                        fail(error);
                                    }
                                })
                            }),
                        );
                        return;
                    }
                    "gif" => {
                        let finished_id = id.clone();
                        host.history_export_gif(
                            &id,
                            Box::new(move |_| runtime::on_ui(move || finish_busy(&finished_id))),
                        );
                        return;
                    }
                    _ => Err(Error::msg(format!("unknown history action: {operation}"))),
                };
                let history = if operation == "remove" && result.is_ok() {
                    Some(host.history_list(200))
                } else {
                    None
                };
                runtime::on_ui(move || {
                    if let Some(history) = history {
                        report(dispatch(Action::HistoryLoaded(history)));
                    }
                    report(result);
                });
            })?;
        }
    }
    Ok(())
}

fn require_item(result: Result<bool>) -> Result<()> {
    if result? {
        Ok(())
    } else {
        Err(Error::msg("the history item is no longer available"))
    }
}
fn finish_busy(id: &str) {
    VIEW.with_borrow_mut(|view| {
        if let Some(view) = view {
            view.busy.remove(id);
        }
    });
    render();
}

fn setting(key: &str, value: &str) -> Result<()> {
    if key == "recording.microphone" {
        let enabled = VIEW.with_borrow(|view| {
            view.as_ref()
                .is_some_and(|view| view.state.microphone.enabled)
        });
        if !matches!(value, "true" | "false") {
            return Err(Error::msg("invalid microphone switch value"));
        }
        if (value == "true") != enabled {
            return dispatch(Action::MicrophoneToggle);
        }
        return Ok(());
    }
    if key == "recording.microphoneDeviceId" {
        return dispatch(Action::MicrophoneSelect {
            device_id: value.into(),
        });
    }
    if key == "saveDir" {
        let host = runtime::host().clone();
        return work(move || {
            let result = host
                .settings_choose_dir()
                .map(|path| path.map(|_| host.app_state()));
            runtime::on_ui(move || match result {
                Ok(Some(app)) => report(dispatch(Action::SettingsSaved(app))),
                Ok(None) => {}
                Err(error) => fail(error),
            });
        });
    }
    let value = if key == "mcp.port" {
        json!(state::port_from_input(value)?)
    } else if key == "recording.fps" {
        let fps = value
            .parse::<u32>()
            .map_err(|_| Error::msg("invalid recording frame rate"))?;
        if ![15, 24, 30, 60].contains(&fps) {
            return Err(Error::msg("invalid recording frame rate"));
        }
        json!(fps)
    } else if matches!(
        key,
        "autoSave"
            | "showToast"
            | "downscaleForAI"
            | "launchAtStartup"
            | "recording.systemAudio"
            | "recording.microphone"
            | "recording.countdown"
            | "audioRecording.systemAudio"
            | "audioRecording.microphone"
            | "translate.autoBubble"
            | "translate.ocrFallback"
            | "mcp.enabled"
    ) {
        if !matches!(value, "true" | "false") {
            return Err(Error::msg("invalid switch value"));
        }
        json!(value == "true")
    } else {
        json!(if key == "translate.deeplKey" {
            value.trim()
        } else {
            value
        })
    };
    let mut patch = value;
    for part in key.split('.').rev() {
        patch = json!({part:patch});
    }
    dispatch(Action::SettingsPatch(patch))
}

fn render() {
    // Setters may synchronously deliver local focus changes. Never hold VIEW across a setter.
    let data = VIEW.with_borrow(|view| {
        view.as_ref().map(|view| {
            (
                view.ui.clone_strong(),
                view.state.clone(),
                view.shown,
                view.dictionary.clone(),
                view.thumbnails.clone(),
                view.busy.clone(),
            )
        })
    });
    let Some((ui, state, shown, dictionary, mut thumbnails, busy)) = data else {
        return;
    };
    let state = &state;
    *dictionary.borrow_mut() = state.dictionary.clone();
    ui.set_language_revision(state.revision as i32 * 2 + i32::from(state.ready));
    ui.set_page(
        match state.page.unwrap_or(Page::History) {
            Page::History => "history",
            Page::Text => "text",
            Page::Claude => "claude",
            Page::Settings => "settings",
        }
        .into(),
    );
    ui.set_version(state.version.clone().into());
    ui.set_native_title(state.title.clone().into());
    ui.set_text_input(state.text.input.clone().into());
    ui.set_text_result(state.text.result.clone().into());
    ui.set_text_meta(state.text.meta.clone().into());
    ui.set_text_loading(state.text.loading);
    ui.set_text_visible(state.text.result_visible);
    ui.set_languages(strings(LANGUAGES.iter().map(|(_, label)| (*label).into())));
    ui.set_text_target(
        LANGUAGES
            .iter()
            .position(|(id, _)| id == &state.text.target)
            .unwrap_or(0) as i32,
    );
    ui.set_translate_hotkey(state::short(&state.string("/hotkeys/translateSelection")).into());
    ui.set_text_hotkey(state::short(&state.string("/hotkeys/textGrab")).into());
    ui.set_focused_hotkey(
        state
            .hotkey_focus
            .as_ref()
            .map(|field| format!("hotkeys.{field}"))
            .unwrap_or_default()
            .into(),
    );
    ui.set_sections(model(settings_sections(state)));
    let warning = state
        .hotkey_errors
        .iter()
        .filter_map(|error| error.get("acc").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(", ");
    ui.set_hotkey_warning(
        if warning.is_empty() {
            String::new()
        } else {
            state.format("hkWarn", &[&warning])
        }
        .into(),
    );
    let running = state
        .mcp
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ui.set_mcp_running(running);
    ui.set_mcp_enabled(state.boolean("/mcp/enabled"));
    ui.set_mcp_status(
        if running {
            state::plain_text(
                &state.format(
                    "mcpRunning",
                    &[&state
                        .mcp
                        .get("port")
                        .and_then(Value::as_u64)
                        .unwrap_or(7464)
                        .to_string()],
                ),
            )
        } else {
            state.tr(if state.boolean("/mcp/enabled") {
                "mcpNotRunning"
            } else {
                "mcpDisabled"
            })
        }
        .into(),
    );
    ui.set_mcp_command(state.command().into());
    ui.set_logs(model(
        state
            .logs
            .iter()
            .map(|entry| MainLogEntry {
                time: local_time(
                    entry.get("ts").and_then(Value::as_i64).unwrap_or(0),
                    true,
                    &state.lang,
                )
                .into(),
                tool: entry
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                elapsed: state
                    .format(
                        "logMs",
                        &[&entry
                            .get("ms")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0)
                            .to_string()],
                    )
                    .into(),
                ok: entry.get("ok").and_then(Value::as_bool).unwrap_or(false),
            })
            .collect(),
    ));
    ui.set_captures(model(capture_buttons(state)));
    ui.set_empty_body(
        state::plain_text(&state.format(
            "emptyBody",
            &[&state::short(&state.string("/hotkeys/region"))],
        ))
        .into(),
    );
    if shown {
        let now = Local::now().timestamp_millis();
        let ids: HashSet<_> = state
            .history
            .iter()
            .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect();
        thumbnails.retain(|id, _| ids.contains(id));
        let shots = state
            .history
            .iter()
            .map(|entry| {
                let id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let kind = entry.get("kind").and_then(Value::as_str).unwrap_or("image");
                let thumbnail = if kind == "audio" {
                    Image::default()
                } else {
                    thumbnails
                        .entry(id.clone())
                        .or_insert_with(|| {
                            let path = entry
                                .get("thumb")
                                .and_then(Value::as_str)
                                .filter(|path| !path.is_empty())
                                .or_else(|| entry.get("file").and_then(Value::as_str))
                                .unwrap_or("");
                            match decode_thumbnail(Path::new(path)) {
                                Ok(image) => image,
                                Err(error) => {
                                    eprintln!("history thumbnail: {error}");
                                    Image::default()
                                }
                            }
                        })
                        .clone()
                };
                let source = entry.get("source").and_then(Value::as_str).unwrap_or("");
                let badge = if kind == "audio" {
                    state.tr("srcAudio")
                } else {
                    state.tr(match source {
                        "region" => "srcRegion",
                        "fullscreen" => "srcFullscreen",
                        "repeat" => "srcRepeat",
                        "mcp" => "srcMcp",
                        "pin" => "srcPin",
                        "record" => "srcRecord",
                        _ => source,
                    })
                };
                let ts = entry.get("ts").and_then(Value::as_i64).unwrap_or(now);
                let time = state::relative_time(state, ts, now)
                    .unwrap_or_else(|| local_time(ts, false, &state.lang));
                let size = if kind == "audio" {
                    Path::new(entry.get("file").and_then(Value::as_str).unwrap_or(""))
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or("")
                        .to_owned()
                } else {
                    format!(
                        "{}×{}",
                        entry.get("width").and_then(Value::as_u64).unwrap_or(0),
                        entry.get("height").and_then(Value::as_u64).unwrap_or(0)
                    )
                };
                let duration = entry
                    .get("duration")
                    .and_then(Value::as_f64)
                    .filter(|s| *s > 0.0 && s.is_finite())
                    .map(|s| {
                        format!(
                            "{}:{:02}",
                            (s / 60.0).floor() as u64,
                            (s % 60.0).round() as u64
                        )
                    })
                    .unwrap_or_default();
                let mut tips = if kind == "audio" {
                    ["actPlay", "actPath", "actFolder", "actDelete", "", ""]
                } else if kind == "video" {
                    ["actPlay", "actGif", "actPath", "actFolder", "actDelete", ""]
                } else {
                    [
                        "actCopy",
                        "actPath",
                        "actPin",
                        "actOcr",
                        "actFolder",
                        "actDelete",
                    ]
                };
                if kind == "video" && busy.contains(&id) {
                    tips[1] = "actGifBusy";
                }
                MainShot {
                    id: id.clone().into(),
                    thumbnail,
                    badge: badge.into(),
                    time: time.into(),
                    size: size.into(),
                    duration: duration.into(),
                    kind: kind.into(),
                    busy: busy.contains(&id),
                    tips: strings(tips.into_iter().map(|key| state.tr(key))),
                }
            })
            .collect();
        ui.set_shots(model(shots));
    }
    VIEW.with_borrow_mut(|view| {
        if let Some(view) = view {
            if view.shown {
                view.thumbnails = thumbnails;
            }
        }
    });
}

/// Decode without Slint's global path-image cache, so hiding really releases decoded thumbnails.
fn decode_thumbnail(path: &Path) -> Result<Image> {
    let mut reader =
        image::ImageReader::open(path).map_err(|e| Error::io("open thumbnail", path, e))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|e| Error::msg(format!("cannot decode thumbnail: {e}")))?;
    let decoded = decoded.thumbnail(560, 350).into_rgba8();
    let pixels = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        decoded.as_raw(),
        decoded.width(),
        decoded.height(),
    );
    Ok(Image::from_rgba8(pixels))
}

fn local_time(timestamp: i64, seconds_only: bool, lang: &str) -> String {
    let Some(time) = Local.timestamp_millis_opt(timestamp).single() else {
        return String::new();
    };
    if seconds_only {
        return time.format("%H:%M:%S").to_string();
    }
    if lang == "ru" {
        use chrono::Datelike;
        let months = [
            "янв.",
            "февр.",
            "мар.",
            "апр.",
            "мая",
            "июн.",
            "июл.",
            "авг.",
            "сент.",
            "окт.",
            "нояб.",
            "дек.",
        ];
        format!(
            "{} {} {}",
            time.day(),
            months[time.month0() as usize],
            time.format("%H:%M")
        )
    } else {
        time.format("%b %-d %I:%M %p").to_string()
    }
}

fn capture_buttons(state: &State) -> Vec<MainCaptureButton> {
    let rows = [
        ("region", "btnRegion", "region", "region", "", 126.0),
        ("full", "btnFull", "full", "fullscreen", "", 166.0),
        (
            "repeat",
            "btnRepeat",
            "repeat",
            "repeatLast",
            "tipRepeatBtn",
            172.0,
        ),
        ("text", "trayText", "ocr", "textGrab", "hkTextSub", 226.0),
        ("record", "btnRecord", "record", "", "tipRecordBtn", 90.0),
        (
            "audio",
            "btnRecordAudio",
            "audio",
            "",
            "tipRecordAudioBtn",
            84.0,
        ),
        ("folder", "", "folder", "", "tipOpenDir", 35.0),
    ];
    rows.into_iter()
        .map(|(mode, label, icon, hotkey, tip, base)| {
            let recording = state.recording.recording
                && ((mode == "audio") == state.recording.audio_only.unwrap_or(false))
                && matches!(mode, "record" | "audio");
            let label = if label.is_empty() {
                String::new()
            } else {
                state.tr(if recording { "btnStopRec" } else { label })
            };
            let shortcut = if hotkey.is_empty() {
                String::new()
            } else {
                state::short(&state.string(&format!("/hotkeys/{hotkey}")))
            };
            MainCaptureButton {
                mode: mode.into(),
                label: label.into(),
                shortcut: shortcut.into(),
                icon: icon.into(),
                tip: if tip.is_empty() {
                    "".into()
                } else {
                    state.tr(tip).into()
                },
                primary: mode == "region",
                recording,
                extent: base,
            }
        })
        .collect()
}

fn settings_sections(state: &State) -> Vec<MainSection> {
    let row = |key: &str, label: &str, sub: &str, kind: &str| MainSetting {
        key: key.into(),
        label: state.tr(label).into(),
        sub: state.tr(sub).into(),
        kind: kind.into(),
        value: state.string(&format!("/{}", key.replace('.', "/"))).into(),
        checked: if key == "recording.microphone" {
            state.microphone.enabled
        } else {
            state.boolean(&format!("/{}", key.replace('.', "/")))
        },
        options: ModelRc::default(),
        values: ModelRc::default(),
        index: 0,
        placeholder: if kind == "hotkey" {
            state.tr("hkPlaceholder")
        } else if kind == "password" {
            state.tr("trKeyPh")
        } else {
            String::new()
        }
        .into(),
    };
    let combo = |key: &str, label: &str, sub: &str, options: Vec<(String, String)>| {
        let mut row = row(key, label, sub, "combo");
        let value = state.setting(&format!("/{}", key.replace('.', "/")));
        let value = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        row.index = options.iter().position(|(id, _)| id == &value).unwrap_or(0) as i32;
        row.options = strings(options.iter().map(|(_, label)| label.clone()));
        row.values = strings(options.into_iter().map(|(id, _)| id));
        row
    };
    let lang_options = || {
        LANGUAGES
            .iter()
            .map(|(value, label)| ((*value).into(), (*label).into()))
            .collect()
    };
    let mut hotkeys: Vec<_> = HOTKEYS
        .iter()
        .map(|(field, label)| {
            row(
                &format!("hotkeys.{field}"),
                label,
                &format!("{label}Sub"),
                "hotkey",
            )
        })
        .collect();
    // The recorder uses localized instructions while focused, the field label stays accessible.
    for hotkey in &mut hotkeys {
        if state
            .hotkey_focus
            .as_ref()
            .is_some_and(|field| hotkey.key.as_str() == format!("hotkeys.{field}"))
        {
            hotkey.placeholder = state.tr("hkRecording").into();
        }
    }
    let mut save = row("saveDir", "saveDir", "", "button");
    save.sub = state.string("/saveDir").into();
    save.value = state.tr("btnChange").into();
    let mut recording = vec![
        combo(
            "recording.fps",
            "recFps",
            "recFpsSub",
            [15, 24, 30, 60]
                .map(|fps| (fps.to_string(), format!("{fps} fps")))
                .to_vec(),
        ),
        combo(
            "recording.format",
            "recFormat",
            "recFormatSub",
            vec![("auto".into(), "WebM".into()), ("mp4".into(), "MP4".into())],
        ),
        row("recording.systemAudio", "recAudio", "recAudioSub", "switch"),
        row(
            "recording.microphone",
            "recMicrophone",
            "recMicrophoneSub",
            "switch",
        ),
    ];
    if state.microphone.device_row_visible {
        recording.push(combo(
            "recording.microphoneDeviceId",
            "recMicrophoneDevice",
            "recMicrophoneDeviceSub",
            state.microphone_options(),
        ));
    }
    recording.push(row(
        "recording.countdown",
        "recCountdown",
        "recCountdownSub",
        "switch",
    ));
    let mut translate = vec![
        row(
            "translate.autoBubble",
            "trAutoBubble",
            "trAutoBubbleSub",
            "switch",
        ),
        row("translate.ocrFallback", "trOcr", "trOcrSub", "switch"),
        combo(
            "translate.target",
            "trTarget",
            "trTargetSub",
            lang_options(),
        ),
        combo(
            "translate.provider",
            "trProvider",
            "trProviderSub",
            vec![
                ("free".into(), state.tr("trProviderFree")),
                ("deepl".into(), state.tr("trProviderDeepl")),
            ],
        ),
    ];
    if state.string("/translate/provider") == "deepl" {
        translate.push(row("translate.deeplKey", "trKey", "trKeySub", "password"));
    }
    let mut port = row("mcp.port", "port", "portSub", "input");
    port.value = state
        .setting("/mcp/port")
        .as_u64()
        .unwrap_or(7464)
        .to_string()
        .into();
    let sections = vec![
        ("hkTitle", hotkeys),
        (
            "saveTitle",
            vec![save, row("autoSave", "autoSave", "autoSaveSub", "switch")],
        ),
        (
            "clipTitle",
            vec![
                row("showToast", "toasts", "toastsSub", "switch"),
                row("downscaleForAI", "downscale", "downscaleSub", "switch"),
                row("launchAtStartup", "autostart", "autostartSub", "switch"),
                combo(
                    "language",
                    "language",
                    "languageSub",
                    vec![
                        ("system".into(), state.tr("langSystem")),
                        ("en".into(), "English".into()),
                        ("ru".into(), "Русский".into()),
                    ],
                ),
            ],
        ),
        ("recSecTitle", recording),
        (
            "audioSecTitle",
            vec![
                row(
                    "audioRecording.systemAudio",
                    "audioSystem",
                    "audioSystemSub",
                    "switch",
                ),
                row(
                    "audioRecording.microphone",
                    "audioMicrophone",
                    "audioMicrophoneSub",
                    "switch",
                ),
            ],
        ),
        ("trTitle", translate),
        ("mcpSettingsTitle", vec![port]),
    ];
    let mut offset = 0.0;
    sections
        .into_iter()
        .enumerate()
        .map(|(i, (title, rows))| {
            let extra = if i == 0 && !state.hotkey_errors.is_empty() {
                32.0
            } else {
                0.0
            };
            let extent = 54.0 + rows.len() as f32 * 57.0 + extra;
            let section = MainSection {
                title: state.tr(title).into(),
                rows: model(rows),
                offset,
                extent,
            };
            offset += extent + 12.0;
            section
        })
        .collect()
}

fn show_now() -> Result<()> {
    let ui = ensure()?;
    let needs_surface = !ui.window().has_winit_window()
        || !VIEW.with_borrow(|view| view.as_ref().is_some_and(|view| view.shown));
    VIEW.with_borrow_mut(|view| {
        if let Some(view) = view {
            view.shown = true;
            view.requested_visible = true;
        }
    });
    if needs_surface {
        render();
        let scale = if runtime::env().ghost {
            1.0
        } else {
            ui.window().scale_factor()
        };
        let placement = VIEW
            .with_borrow(|view| view.as_ref().and_then(|view| view.placement))
            .unwrap_or(Placement {
                x: 0,
                y: 60,
                width: (1120.0 * scale) as u32,
                height: (740.0 * scale) as u32,
                rounded: true,
                scale,
                ..Default::default()
            });
        if runtime::env().ghost {
            ui.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::ScaleFactorChanged {
                    scale_factor: 1.0,
                })
                .map_err(|e| Error::msg(format!("cannot set the main preview scale: {e}")))?;
            // The off-screen HWND initially inherits the nearest real monitor's 150% DPI.
            // Winit has already converted the minimum to physical pixels; update that too.
            ui.window().with_winit_window(|window| {
                window.set_min_inner_size(Some(winit::dpi::PhysicalSize::new(880, 600)))
            });
        }
        runtime::place(ui.window(), placement)?;
        install_chrome(ui.window())?;
        if runtime::env().ghost {
            ui.window()
                .dispatch_event_with_result(slint::platform::WindowEvent::Resized {
                    size: slint::LogicalSize::new(placement.width as f32, placement.height as f32),
                })
                .map_err(|e| Error::msg(format!("cannot size the main preview: {e}")))?;
        }
    }
    ui.window().set_minimized(false);
    ui.show()
        .map_err(|e| Error::msg(format!("cannot show the main window: {e}")))?;
    VIEW.with_borrow(|view| {
        if let Some(view) = view {
            view.timer
                .start(slint::TimerMode::Repeated, Duration::from_secs(1), tick);
        }
    });
    Ok(())
}

fn show_requested_now() -> Result<()> {
    show_now()?;
    if !runtime::env().ghost {
        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
        ui.window()
            .with_winit_window(|window| window.focus_window());
    }
    Ok(())
}

fn hide_now() -> Result<()> {
    let field = VIEW.with_borrow(|view| {
        view.as_ref()
            .and_then(|view| view.state.hotkey_focus.clone())
    });
    if let Some(field) = field {
        dispatch(Action::HotkeyBlur { field })?;
        blur_editor();
    }
    let ui = VIEW.with_borrow_mut(|view| {
        if let Some(view) = view {
            if view.shown && !view.ui.window().is_minimized() {
                let position = view.ui.window().position();
                let size = view.ui.window().size();
                view.placement = Some(Placement {
                    x: position.x
                        - if runtime::env().ghost {
                            runtime::GHOST_OFFSET
                        } else {
                            0
                        },
                    y: position.y,
                    width: size.width,
                    height: size.height,
                    scale: view.ui.window().scale_factor(),
                    rounded: true,
                    ..Default::default()
                });
            }
            view.shown = false;
            view.requested_visible = false;
            view.timer.stop();
            view.thumbnails.clear();
            Some(view.ui.clone_strong())
        } else {
            None
        }
    });
    if let Some(ui) = ui {
        ui.set_shots(ModelRc::default());
        ui.hide()
            .map_err(|e| Error::msg(format!("cannot hide the main window: {e}")))?;
    }
    Ok(())
}

fn blur_editor() {
    let ui = VIEW.with_borrow(|view| view.as_ref().map(|view| view.ui.clone_strong()));
    if let Some(ui) = ui {
        ui.invoke_blur_hotkeys();
    }
}

fn tick() {
    let refresh = VIEW.with_borrow_mut(|view| {
        let Some(view) = view else {
            return false;
        };
        let refresh = view.shown
            && view.state.page == Some(Page::Settings)
            && view.state.microphone.device_row_visible
            && view.ticks == 0;
        view.ticks = (view.ticks + 1) % 5;
        refresh
    });
    let host = runtime::host().clone();
    report(work(move || {
        // MCP restarts asynchronously in AppHost::settings_set. Read its actual status after
        // that operation rather than leaving the old running/port display on the Claude page.
        let mcp = host.app_state().get("mcp").cloned();
        let devices = if refresh {
            Some(host.microphones())
        } else {
            None
        };
        runtime::on_ui(move || {
            if let Some(mcp) = mcp {
                report(dispatch(Action::McpStatus(mcp)));
            } else {
                fail(Error::msg("application state omitted MCP status"));
            }
            if let Some(devices) = devices {
                match devices {
                    Ok(devices) => report(dispatch(Action::Devices(devices))),
                    Err(error) => fail(error),
                }
            }
        });
    }));
}

pub fn show() {
    runtime::on_ui(|| {
        report(ensure().and_then(|ui| {
            VIEW.with_borrow_mut(|view| {
                if let Some(view) = view {
                    view.requested_visible = true;
                }
            });
            after_native(ui, || {
                let show = VIEW.with_borrow(|view| {
                    view.as_ref()
                        .is_some_and(|view| view.requested_visible && view.state.ready)
                });
                if show {
                    report(show_requested_now());
                }
            })
        }));
    });
}
pub fn hide() {
    runtime::on_ui(|| report(hide_now()));
}
pub fn visible() -> bool {
    match runtime::on_ui_wait(|| {
        VIEW.with_borrow(|view| {
            view.as_ref().is_some_and(|view| {
                view.shown
                    && runtime::hwnd_of(view.ui.window()).is_ok_and(|hwnd| {
                        // SAFETY: query only this view's owned window.
                        unsafe { IsWindowVisible(hwnd).as_bool() }
                    })
            })
        })
    }) {
        Ok(visible) => visible,
        Err(error) => {
            eprintln!("cannot query main visibility: {error}");
            false
        }
    }
}
pub fn send(event: MainEvent) {
    runtime::on_ui(move || {
        if !VIEW.with_borrow(|view| view.is_some()) {
            return;
        }
        match event {
            MainEvent::HistoryChanged => {
                let host = runtime::host().clone();
                report(work(move || {
                    let history = host.history_list(200);
                    runtime::on_ui(move || report(dispatch(Action::HistoryLoaded(history))));
                }));
            }
            MainEvent::McpLog(entry) => report(dispatch(Action::McpLog(entry))),
            MainEvent::RecState(state) => report(dispatch(Action::RecState(state))),
            MainEvent::ThemeChanged(theme) => apply_theme_now(&theme),
        }
    });
}
fn apply_theme_now(theme: &Theme) {
    VIEW.with_borrow(|view| {
        if let Some(view) = view {
            crate::apply_theme!(&view.ui, theme);
        }
    });
}
pub fn apply_theme(theme: &Theme) {
    let theme = theme.clone();
    runtime::on_ui(move || apply_theme_now(&theme));
}
pub fn rebuild_language() {
    runtime::on_ui(|| {
        if VIEW.with_borrow(|view| view.is_some()) {
            report(dispatch(Action::Reload));
        }
    });
}

fn snapshot() -> Result<Value> {
    VIEW.with_borrow(|view| {
        let view = view
            .as_ref()
            .ok_or_else(|| Error::msg("main window is absent"))?;
        if let Some(error) = &view.error {
            return Err(Error::msg(error.clone()));
        }
        let mut state = serde_json::to_value(&view.state)
            .map_err(|e| Error::msg(format!("cannot serialize main state: {e}")))?;
        if let Ok(hwnd) = runtime::hwnd_of(view.ui.window()) {
            // SAFETY: own live HWND and caller-owned UTF-16 buffer.
            let length = unsafe { GetWindowTextLengthW(hwnd) };
            let mut title = vec![0_u16; length as usize + 1];
            let length = unsafe { GetWindowTextW(hwnd, &mut title) };
            state["title"] = json!(String::from_utf16_lossy(&title[..length as usize]));
        }
        Ok(state)
    })
}
pub fn ui_state() -> Result<Value> {
    runtime::on_ui_wait(|| {
        ensure()?;
        snapshot()
    })?
}
pub fn ui_action(action: Value) -> Result<Value> {
    let action: Action = serde_json::from_value(action)
        .map_err(|e| Error::msg(format!("invalid main action: {e}")))?;
    runtime::on_ui_wait(move || {
        let ui = ensure()?;
        match action {
            Action::MicrophoneToggle => {
                let enabled =
                    VIEW.with_borrow(|view| view.as_ref().unwrap().state.microphone.enabled);
                ui.invoke_setting("recording.microphone".into(), (!enabled).to_string().into());
            }
            Action::MicrophoneSelect { device_id } => {
                ui.invoke_setting("recording.microphoneDeviceId".into(), device_id.into())
            }
            Action::HotkeyFocus { ref field } if field == "region" => {
                show_now()?;
                dispatch(Action::Navigate {
                    page: Page::Settings,
                })?;
                ui.invoke_focus_region();
                if !ui.get_region_focused() {
                    return Err(Error::msg(
                        "the region hotkey editor did not receive local focus",
                    ));
                }
                // Slint's changed handler is deferred; dispatch the real editor callback after
                // verifying local focus, just as the Electron ghost adapter does.
                ui.invoke_hotkey_focus("hotkeys.region".into(), true);
            }
            Action::HotkeyBlur { ref field } if field == "region" => {
                ui.invoke_blur_region();
                if ui.get_region_focused() {
                    return Err(Error::msg("the region hotkey editor did not blur"));
                }
                ui.invoke_hotkey_focus("hotkeys.region".into(), false);
            }
            Action::HotkeyFocus { .. } | Action::HotkeyBlur { .. } => {
                return Err(Error::msg(
                    "the main test route supports only the region hotkey editor",
                ))
            }
            Action::SetTitle { .. } => {
                show_now()?;
                dispatch(action)?;
            }
            _ => dispatch(action)?,
        }
        snapshot()
    })?
}
pub fn capture_page(path: &Path) -> Result<(u32, u32)> {
    let path = path.to_owned();
    runtime::on_ui_wait(move || {
        let was_visible = VIEW.with_borrow(|view| view.as_ref().is_some_and(|view| view.shown));
        show_now()?;
        let result = VIEW
            .with_borrow(|view| runtime::snapshot_png(view.as_ref().unwrap().ui.window(), &path));
        if !was_visible {
            hide_now()?;
        }
        result
    })?
}

fn caption(action: &str) -> Result<()> {
    if action == "close" {
        return hide_now();
    }
    VIEW.with_borrow(|view| {
        if let Some(view) = view {
            match action {
                "minimize" => view.ui.window().set_minimized(true),
                "maximize" => {
                    // A ghost maximize must never move the owned window onto a real monitor.
                    let maximized = !view.ui.get_chrome_maximized();
                    if !runtime::env().ghost {
                        view.ui.window().set_maximized(maximized);
                    }
                    view.ui.set_chrome_maximized(maximized);
                }
                _ => return Err(Error::msg(format!("unknown caption action: {action}"))),
            }
        }
        Ok(())
    })
}

const CHROME_SUBCLASS: usize = 0x5348_4d41;
const CHROME_STYLE: WINDOW_STYLE =
    WINDOW_STYLE(WS_THICKFRAME.0 | WS_MAXIMIZEBOX.0 | WS_MINIMIZEBOX.0 | WS_SYSMENU.0);

fn install_chrome(window: &slint::Window) -> Result<()> {
    let hwnd = runtime::hwnd_of(window)?;
    // SAFETY: subclass and style apply only to this process's window on its UI thread.
    unsafe {
        SetWindowSubclass(hwnd, Some(chrome_subclass), CHROME_SUBCLASS, 0)
            .ok()
            .map_err(|e| Error::msg(format!("cannot subclass the main window: {e}")))?;
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            GetWindowLongPtrW(hwnd, GWL_STYLE) | CHROME_STYLE.0 as isize,
        );
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        )
        .map_err(|e| Error::msg(format!("cannot apply the main window frame: {e}")))?;
    }
    Ok(())
}

unsafe extern "system" fn chrome_subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    // SAFETY: Win32 owns all message parameters for the duration of this callback.
    unsafe {
        match message {
            WM_SYSCOMMAND if runtime::env().ghost && wparam.0 & 0xfff0 == SC_MAXIMIZE as usize => {
                return LRESULT(0)
            }
            WM_STYLECHANGING if wparam.0 as i32 == GWL_STYLE.0 => {
                (*(lparam.0 as *mut STYLESTRUCT)).styleNew |= CHROME_STYLE.0;
            }
            WM_NCCALCSIZE if wparam.0 != 0 => {
                if IsZoomed(hwnd).as_bool() {
                    let mut monitor = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        ..Default::default()
                    };
                    if GetMonitorInfoW(
                        MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                        &mut monitor,
                    )
                    .as_bool()
                    {
                        (*(lparam.0 as *mut NCCALCSIZE_PARAMS)).rgrc[0] = monitor.rcWork;
                    }
                }
                return LRESULT(0);
            }
            WM_NCHITTEST => {
                let point = POINT {
                    x: (lparam.0 as u16 as i16) as i32,
                    y: ((lparam.0 >> 16) as u16 as i16) as i32,
                };
                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_ok() {
                    let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
                    let x = (point.x - rect.left) as f32 / scale;
                    let y = (point.y - rect.top) as f32 / scale;
                    let width = (rect.right - rect.left) as f32 / scale;
                    let height = (rect.bottom - rect.top) as f32 / scale;
                    let hit = chrome_hit(x, y, width, height, IsZoomed(hwnd).as_bool());
                    if hit != HTCLIENT {
                        return LRESULT(hit as isize);
                    }
                }
            }
            WM_NCLBUTTONDOWN if wparam.0 == HTMAXBUTTON as usize => return LRESULT(0),
            WM_NCLBUTTONUP if wparam.0 == HTMAXBUTTON as usize => {
                runtime::on_ui(|| report(caption("maximize")));
                return LRESULT(0);
            }
            WM_SIZE => {
                let maximized = wparam.0 == SIZE_MAXIMIZED as usize;
                runtime::on_ui(move || {
                    VIEW.with_borrow(|view| {
                        if let Some(view) = view {
                            view.ui.set_chrome_maximized(maximized);
                        }
                    })
                });
            }
            WM_NCDESTROY => {
                let _ = RemoveWindowSubclass(hwnd, Some(chrome_subclass), CHROME_SUBCLASS);
            }
            _ => {}
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

fn chrome_hit(x: f32, y: f32, width: f32, height: f32, maximized: bool) -> u32 {
    if !maximized {
        let left = x < 6.0;
        let right = x >= width - 6.0;
        let top = y < 6.0;
        let bottom = y >= height - 6.0;
        match (left, right, top, bottom) {
            (true, _, true, _) => return HTTOPLEFT,
            (_, true, true, _) => return HTTOPRIGHT,
            (true, _, _, true) => return HTBOTTOMLEFT,
            (_, true, _, true) => return HTBOTTOMRIGHT,
            (true, _, _, _) => return HTLEFT,
            (_, true, _, _) => return HTRIGHT,
            (_, _, true, _) => return HTTOP,
            (_, _, _, true) => return HTBOTTOM,
            _ => {}
        }
    }
    if y < 44.0 {
        if x >= width - 92.0 && x < width - 46.0 {
            HTMAXBUTTON
        } else if x < width - 138.0 {
            HTCAPTION
        } else {
            HTCLIENT
        }
    } else {
        HTCLIENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shotik_core::ports::UiPort;
    use slint::Model;
    use std::sync::Arc;

    fn click_local(x: f32, y: f32) -> Result<()> {
        runtime::on_ui_wait(move || {
            let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
            let position = slint::LogicalPosition::new(x, y);
            for event in [
                slint::platform::WindowEvent::PointerMoved { position },
                slint::platform::WindowEvent::PointerPressed {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                },
                slint::platform::WindowEvent::PointerReleased {
                    position,
                    button: slint::platform::PointerEventButton::Left,
                },
            ] {
                ui.window().dispatch_event_with_result(event).unwrap();
            }
        })
    }
    #[test]
    fn native_chrome_hit_regions_do_not_cover_client_controls() {
        assert_eq!(chrome_hit(400., 20., 1120., 740., false), HTCAPTION);
        assert_eq!(chrome_hit(1050., 20., 1120., 740., false), HTMAXBUTTON);
        assert_eq!(chrome_hit(1100., 20., 1120., 740., false), HTCLIENT);
        assert_eq!(chrome_hit(500., 120., 1120., 740., false), HTCLIENT);
        assert_eq!(chrome_hit(1., 1., 1120., 740., false), HTTOPLEFT);
        assert_eq!(chrome_hit(1., 1., 1120., 740., true), HTCAPTION);
    }

    #[test]
    fn main_protocol_controls_and_eight_renders_on_real_slint() {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        let root = std::env::temp_dir().join(format!(
            "shotik-main-contract-{}-{}",
            std::process::id(),
            Local::now().timestamp_millis()
        ));
        struct RemoveOnDrop(std::path::PathBuf);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = RemoveOnDrop(root.clone());
        let host = super::super::fixtures::PreviewHost::new(&root).unwrap();
        runtime::init(crate::UiEnv {
            ghost: true,
            popup_real: false,
            translate_popup_ttl_ms: 9000,
        })
        .unwrap();
        assert!(VIEW.with_borrow(|view| view.is_none()));
        runtime::set_host(host.clone());
        let mut builder =
            winit::event_loop::EventLoop::<slint::winit_030::SlintEvent>::with_user_event();
        builder.with_any_thread(true);
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .with_winit_event_loop_builder(builder)
            .with_winit_window_attributes_hook(|attributes| {
                attributes
                    .with_visible(false)
                    .with_active(false)
                    .with_position(winit::dpi::PhysicalPosition::new(runtime::GHOST_OFFSET, 60))
            })
            .select()
            .unwrap();
        let ui = Arc::new(crate::Ui);
        let worker_ui = ui.clone();
        let worker = std::thread::spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                    let ui = worker_ui;
                    let history = host.history.lock().unwrap().clone();
                    // Cold tray activation must work before anybody has queried ui-state.
                    ui.show_main();
                    super::super::fixtures::check_protocol(&ui, &host)?;
                    *host.history.lock().unwrap() = history;
                    ui.send_to_main(MainEvent::HistoryChanged);
                    ui.send_to_main(MainEvent::RecState(Default::default()));
                    ui.ui_action("main", 0, json!({"type":"navigate","page":"history"}))?;
                    ui.show_main();
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while !ui.main_visible() {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    ui.capture_page("main", 0, &root.join("gallery-before-hover.png"))?;
                    let copied_before = host
                        .stub
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|call| call.as_str() == "history_copy")
                        .count();
                    let opened_before = host
                        .stub
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|call| call.as_str() == "history_open")
                        .count();
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        ui.window()
                            .dispatch_event_with_result(
                                slint::platform::WindowEvent::PointerMoved {
                                    position: slint::LogicalPosition::new(400., 230.),
                                },
                            )
                            .unwrap();
                    })?;
                    ui.capture_page("main", 0, &root.join("gallery-hover-thumb.png"))?;
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        ui.window()
                            .dispatch_event_with_result(
                                slint::platform::WindowEvent::PointerMoved {
                                    position: slint::LogicalPosition::new(310., 330.),
                                },
                            )
                            .unwrap();
                    })?;
                    ui.capture_page("main", 0, &root.join("gallery-hover.png"))?;
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        let position = slint::LogicalPosition::new(310., 330.);
                        ui.window()
                            .dispatch_event_with_result(
                                slint::platform::WindowEvent::PointerPressed {
                                    position,
                                    button: slint::platform::PointerEventButton::Left,
                                },
                            )
                            .unwrap();
                        ui.window()
                            .dispatch_event_with_result(
                                slint::platform::WindowEvent::PointerReleased {
                                    position,
                                    button: slint::platform::PointerEventButton::Left,
                                },
                            )
                            .unwrap();
                    })?;
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while host
                        .stub
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|call| call.as_str() == "history_copy")
                        .count()
                        == copied_before
                    {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "gallery hover Copy button was not clickable"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    assert_eq!(
                        host.stub
                            .calls
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|call| call.as_str() == "history_open")
                            .count(),
                        opened_before,
                        "a per-item action also opened the file"
                    );
                    let captures_before = host
                        .stub
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|call| call.as_str() == "capture_region")
                        .count();
                    click_local(321., 112.)?;
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while host
                        .stub
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|call| call.as_str() == "capture_region")
                        .count()
                        == captures_before
                    {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "the Area button was not clickable"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    ui.ui_action("main", 0, json!({"type":"navigate","page":"claude"}))?;
                    let enabled_before = host.settings.lock().unwrap()["mcp"]["enabled"]
                        .as_bool()
                        .unwrap();
                    click_local(1054., 137.)?;
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while host.settings.lock().unwrap()["mcp"]["enabled"] == enabled_before {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "the MCP switch was not clickable"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    ui.ui_action("main", 0, json!({"type":"reload"}))?;
                    super::super::fixtures::until(&ui, |state| state["ready"] == true)?;
                    ui.show_main();
                    runtime::on_ui_wait(|| {
                        VIEW.with_borrow(|view| {
                            let view = view.as_ref().unwrap();
                            let hwnd = runtime::hwnd_of(view.ui.window()).unwrap();
                            let mut rect = RECT::default();
                            // SAFETY: read-only checks against this test's owned HWND.
                            unsafe {
                                GetWindowRect(hwnd, &mut rect).unwrap();
                                assert!(rect.left >= runtime::GHOST_OFFSET);
                                assert_ne!(
                                    GetWindowLongPtrW(hwnd, GWL_EXSTYLE)
                                        & WS_EX_NOACTIVATE.0 as isize,
                                    0
                                );
                                assert_ne!(GetForegroundWindow(), hwnd);
                            }
                            let sections = settings_sections(&view.state);
                            let keys: Vec<_> = sections
                                .iter()
                                .flat_map(|section| {
                                    section.rows.iter().map(|row| row.key.to_string())
                                })
                                .collect();
                            for key in [
                                "hotkeys.region",
                                "hotkeys.fullscreen",
                                "hotkeys.repeatLast",
                                "hotkeys.textGrab",
                                "hotkeys.record",
                                "hotkeys.recordAudio",
                                "hotkeys.translateSelection",
                                "saveDir",
                                "autoSave",
                                "showToast",
                                "downscaleForAI",
                                "launchAtStartup",
                                "language",
                                "recording.fps",
                                "recording.format",
                                "recording.systemAudio",
                                "recording.microphone",
                                "recording.microphoneDeviceId",
                                "recording.countdown",
                                "audioRecording.systemAudio",
                                "audioRecording.microphone",
                                "translate.autoBubble",
                                "translate.ocrFallback",
                                "translate.target",
                                "translate.provider",
                                "mcp.port",
                            ] {
                                assert!(
                                    keys.iter().any(|value| value == key),
                                    "missing control {key}"
                                );
                            }
                        });
                    })?;
                    // The controls use the same callback as real local input; all saves hit the fake host.
                    for (key, value) in [
                        ("autoSave", "false"),
                        ("showToast", "false"),
                        ("downscaleForAI", "false"),
                        ("launchAtStartup", "true"),
                        ("recording.fps", "60"),
                        ("recording.format", "mp4"),
                        ("recording.systemAudio", "true"),
                        ("recording.microphone", "true"),
                        ("recording.countdown", "false"),
                        ("audioRecording.systemAudio", "false"),
                        ("audioRecording.microphone", "true"),
                        ("translate.autoBubble", "false"),
                        ("translate.ocrFallback", "false"),
                        ("translate.target", "de"),
                        ("translate.provider", "deepl"),
                        ("translate.deeplKey", ""),
                        ("mcp.port", "9000"),
                        ("mcp.enabled", "false"),
                        ("saveDir", ""),
                    ] {
                        runtime::on_ui_wait(move || {
                            let ui =
                                VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                            ui.invoke_setting(key.into(), value.into());
                        })?;
                    }
                    ui.ui_action("main", 0, json!({"type":"reload"}))?;
                    super::super::fixtures::until(&ui, |state| state["ready"] == true)?;
                    let settings = host.settings.lock().unwrap().clone();
                    assert_eq!(settings["recording"]["fps"], 60);
                    assert_eq!(settings["recording"]["format"], "mp4");
                    assert_eq!(settings["translate"]["target"], "de");
                    assert_eq!(settings["translate"]["provider"], "deepl");
                    assert_eq!(settings["mcp"]["port"], 9000);
                    assert_eq!(settings["mcp"]["enabled"], false);
                    assert_eq!(
                        settings["saveDir"],
                        host.root.join("chosen").to_str().unwrap()
                    );
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        for mode in [
                            "region", "full", "repeat", "text", "record", "audio", "folder",
                        ] {
                            ui.invoke_capture(mode.into());
                        }
                        for operation in ["copy", "path", "pin", "folder", "open", "ocr", "gif"] {
                            ui.invoke_history_action("fixture-1".into(), operation.into());
                        }
                        ui.invoke_copy_command();
                    })?;
                    // A subsequent reload synchronizes the ordered host worker without real input.
                    ui.ui_action("main", 0, json!({"type":"reload"}))?;
                    super::super::fixtures::until(&ui, |s| s["ready"] == true)?;
                    let calls = host.stub.calls.lock().unwrap();
                    for name in [
                        "capture_region",
                        "capture_full",
                        "capture_repeat",
                        "capture_text",
                        "capture_record",
                        "capture_record_audio",
                        "open_save_dir",
                        "history_copy",
                        "history_copy_path",
                        "history_pin",
                        "history_reveal",
                        "history_open",
                        "history_ocr",
                        "history_export_gif",
                        "copy_text",
                    ] {
                        assert!(
                            calls.iter().any(|call| call == name),
                            "control did not reach {name}"
                        );
                    }
                    drop(calls);
                    ui.send_to_main(MainEvent::RecState(shotik_core::ports::RecStatus {
                        recording: true,
                        audio_only: Some(true),
                        ..Default::default()
                    }));
                    ui.send_to_main(MainEvent::McpLog(
                        json!({"ts":1,"tool":"fixture-event","ms":42,"ok":false}),
                    ));
                    runtime::on_ui_wait(|| {
                        VIEW.with_borrow(|view| {
                            let view = view.as_ref().unwrap();
                            assert_eq!(
                                view.ui.get_logs().row_data(0).unwrap().tool,
                                "fixture-event"
                            );
                            let recording = view
                                .ui
                                .get_captures()
                                .iter()
                                .find(|button| button.mode == "audio")
                                .unwrap();
                            assert!(recording.recording);
                            assert_eq!(recording.label, view.state.tr("btnStopRec"));
                        });
                    })?;
                    ui.send_to_main(MainEvent::RecState(Default::default()));
                    for (page, name) in [
                        ("history", "shots"),
                        ("text", "text"),
                        ("claude", "claude"),
                        ("settings", "settings"),
                    ] {
                        for dark in [false, true] {
                            ui.apply_theme(Theme {
                                dark,
                                accent: "#006FC7".into(),
                                platform: "win32".into(),
                            });
                            ui.ui_action("main", 0, json!({"type":"navigate","page":page}))?;
                            let path = root.join(format!("{name}-{dark}.png"));
                            assert_eq!(ui.capture_page("main", 0, &path)?, (1120, 740));
                            let image = image::open(&path)
                                .map_err(|e| Error::msg(e.to_string()))?
                                .to_rgba8();
                            assert_eq!(
                                image.get_pixel(500, 20).0,
                                if dark {
                                    [32, 32, 32, 255]
                                } else {
                                    [243, 243, 243, 255]
                                }
                            );
                        }
                    }
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        ui.invoke_setting("language".into(), "ru".into());
                    })?;
                    super::super::fixtures::until(&ui, |s| {
                        s["ready"] == true && s["microphone"]["deviceLabel"] == "Как в системе"
                    })?;
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        assert_eq!(ui.invoke_tr("navShots".into()), "Снимки");
                        ui.window().set_size(slint::PhysicalSize::new(880, 600));
                    })?;
                    ui.ui_action("main", 0, json!({"type":"navigate","page":"history"}))?;
                    assert_eq!(
                        ui.capture_page("main", 0, &root.join("shots-ru-minimum.png"))?,
                        (880, 600)
                    );
                    ui.ui_action(
                        "main",
                        0,
                        json!({"type":"quick-translate","text":"Проверка русского интерфейса"}),
                    )?;
                    super::super::fixtures::until(&ui, |s| s["text"]["loading"] == false)?;
                    assert_eq!(
                        ui.capture_page("main", 0, &root.join("text-ru-minimum.png"))?,
                        (880, 600)
                    );
                    ui.ui_action("main", 0, json!({"type":"navigate","page":"settings"}))?;
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        // Local Slint input only, no OS input or desktop interaction.
                        ui.window()
                            .dispatch_event_with_result(
                                slint::platform::WindowEvent::PointerScrolled {
                                    position: slint::LogicalPosition::new(600., 450.),
                                    delta_x: 0.,
                                    delta_y: -10000.,
                                },
                            )
                            .unwrap();
                    })?;
                    assert_eq!(
                        ui.capture_page("main", 0, &root.join("settings-ru-bottom.png"))?,
                        (880, 600)
                    );
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        ui.window().set_size(slint::PhysicalSize::new(1120, 740));
                        ui.invoke_setting("language".into(), "en".into());
                    })?;
                    super::super::fixtures::until(&ui, |s| {
                        s["ready"] == true && s["microphone"]["deviceLabel"] == "System default"
                    })?;
                    // AppHost returns before a real MCP restart finishes. The view must later
                    // show the actual listener status, not the stale settings-set response.
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        ui.invoke_setting("mcp.enabled".into(), "false".into());
                    })?;
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while runtime::on_ui_wait(|| {
                        VIEW.with_borrow(|view| view.as_ref().unwrap().ui.get_mcp_enabled())
                    })? {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    host.mcp_running
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        ui.invoke_setting("mcp.enabled".into(), "true".into());
                    })?;
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while !runtime::on_ui_wait(|| {
                        VIEW.with_borrow(|view| {
                            let ui = &view.as_ref().unwrap().ui;
                            ui.get_mcp_enabled() && !ui.get_mcp_running()
                        })
                    })? {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    host.mcp_running
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    while !runtime::on_ui_wait(|| {
                        VIEW.with_borrow(|view| view.as_ref().unwrap().ui.get_mcp_running())
                    })? {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "MCP status did not refresh after restart"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    host.deny_settings
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        ui.invoke_setting("autoSave".into(), "true".into());
                    })?;
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    while ui.ui_state("main", 0).is_ok() {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "settings error was swallowed"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    host.deny_settings
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    ui.ui_action("main", 0, json!({"type":"reload"}))?;
                    super::super::fixtures::until(&ui, |s| s["ready"] == true)?;
                    runtime::on_ui_wait(|| {
                        let ui = VIEW.with_borrow(|view| view.as_ref().unwrap().ui.clone_strong());
                        ui.invoke_caption("close".into());
                    })?;
                    assert!(!ui.main_visible());
                    runtime::on_ui_wait(|| {
                        VIEW.with_borrow(|view| {
                            let view = view.as_ref().unwrap();
                            assert!(view.thumbnails.is_empty());
                            assert_eq!(view.ui.get_shots().row_count(), 0);
                        })
                    })?;
                    ui.show_main();
                    ui.hide_main();
                    std::thread::sleep(Duration::from_millis(50));
                    assert!(!ui.main_visible(), "a pending show overrode a later hide");
                    Ok(())
                }));
            crate::Ui::quit();
            result
        });
        ui.run().unwrap();
        match worker.join().unwrap() {
            Ok(result) => result.unwrap(),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
}
