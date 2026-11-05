//! Tray menu and global hotkeys: `buildTrayMenu` and `registerHotkeys` in `src/main/main.js`.

use std::sync::Arc;

use shotik_core::ports::{HotkeyAction, HotkeyBinding, TrayItem};

use crate::App;

fn item(id: &str, label: String, sublabel: Option<&str>) -> TrayItem {
    TrayItem::Action {
        id: id.to_string(),
        label,
        sublabel: sublabel.filter(|s| !s.is_empty()).map(str::to_string),
    }
}

impl App {
    pub(crate) fn init_tray(self: &Arc<Self>) {
        let on_item = self.clone();
        let on_icon = self.clone();
        self.ports.tray.set_handlers(
            Arc::new(move |id| on_item.tray_item(&id)),
            // While recording, a click stops it: no need to remember the hotkey.
            Arc::new(move |()| {
                if on_icon.is_recording() {
                    on_icon.stop_recording();
                } else {
                    on_icon.ports.ui.show_main();
                }
            }),
        );
        self.refresh_tray();
    }

    pub(crate) fn refresh_tray(&self) {
        let recording = self.is_recording();
        let tooltip = self.t(if recording { "trayRecTooltip" } else { "trayTooltip" });
        if let Err(e) = self.ports.tray.set_tooltip(&tooltip) {
            self.diag.log(&format!("tray tooltip failed: {e}"));
        }
        if let Err(e) = self.ports.tray.set_menu(self.tray_menu(recording)) {
            self.diag.log(&format!("tray menu failed: {e}"));
        }
    }

    fn tray_menu(&self, recording: bool) -> Vec<TrayItem> {
        let hk = self.settings().hotkeys;
        let mut items = vec![
            item("region", self.t("trayRegion"), Some(&hk.region)),
            item("full", self.t("trayFull"), Some(&hk.fullscreen)),
            item("repeat", self.t("trayRepeat"), Some(&hk.repeat_last)),
            item("text", self.t("trayText"), Some(&hk.text_grab)),
            item("translate", self.t("trayTranslate"), Some(&hk.translate_selection)),
        ];
        if recording {
            items.push(item("stop-rec", self.t("trayStopRec"), None));
        } else {
            items.push(item("record", self.t("trayRecord"), Some(&hk.record)));
            items.push(item("record-audio", self.t("trayRecordAudio"), Some(&hk.record_audio)));
        }
        items.push(TrayItem::Separator);
        items.push(item("open", self.t("trayOpen"), None));
        items.push(TrayItem::Separator);
        items.push(item("quit", self.t("trayQuit"), None));
        items
    }

    fn tray_item(self: &Arc<Self>, id: &str) {
        match id {
            "region" => self.spawn(|app| app.trigger_region(false)),
            "full" => self.spawn(|app| app.trigger_full()),
            "repeat" => self.spawn(|app| app.trigger_repeat()),
            "text" => self.spawn(|app| app.trigger_text(false)),
            "translate" => self.spawn(|app| app.trigger_translate_selection()),
            "stop-rec" => self.stop_recording(),
            "record" => self.spawn(|app| app.trigger_record()),
            "record-audio" => self.spawn(|app| app.trigger_record_audio()),
            "open" => self.ports.ui.show_main(),
            "quit" => self.shutdown(),
            other => self.diag.log(&format!("unknown tray item: {other}")),
        }
    }

    pub(crate) fn shutdown(&self) {
        self.ports.hotkeys.unregister_all();
        self.stop_selection_watcher();
        (self.ports.quit)();
    }

    /// The settings page suspends global shortcuts while it records a new combo: otherwise the OS
    /// delivers the pressed combo to an existing binding instead of the recorder field.
    pub(crate) fn suspend_hotkeys(&self) {
        self.state().hotkeys_suspended = true;
        self.ports.hotkeys.unregister_all();
    }

    pub(crate) fn resume_hotkeys(self: &Arc<Self>) {
        self.state().hotkeys_suspended = false;
        self.register_hotkeys();
    }

    pub(crate) fn register_hotkeys(self: &Arc<Self>) {
        self.ports.hotkeys.unregister_all();
        self.state().hotkey_errors.clear();
        if self.env.ghost || self.state().hotkeys_suspended {
            return; // never grab the user's keys during invisible testing
        }
        let hk = self.settings().hotkeys;
        let bindings: Vec<HotkeyBinding> = [
            (HotkeyAction::Region, hk.region),
            (HotkeyAction::Fullscreen, hk.fullscreen),
            (HotkeyAction::RepeatLast, hk.repeat_last),
            (HotkeyAction::TextGrab, hk.text_grab),
            (HotkeyAction::Record, hk.record),
            (HotkeyAction::RecordAudio, hk.record_audio),
            (HotkeyAction::TranslateSelection, hk.translate_selection),
        ]
        .into_iter()
        .filter(|(_, accelerator)| !accelerator.is_empty())
        .map(|(action, accelerator)| HotkeyBinding { action, accelerator })
        .collect();
        let app = self.clone();
        let errors = self.ports.hotkeys.register(&bindings, Arc::new(move |action| app.fire_hotkey(action)));
        self.state().hotkey_errors = errors;
    }
}
