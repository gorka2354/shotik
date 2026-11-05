//! Updating to a newer release: when to look, what to tell the user, when a restart is safe. What may be trusted is
//! decided in `shotik_core::update`. See `docs/rust/UPDATES.md`.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use shotik_core::ports::{HttpRequest, MainEvent, Toast};
use shotik_core::update::{
    check_installer, check_is_due, installer_name, verify_manifest, Manifest, UpdateSource, Version, UPDATE_PUBLIC_KEY,
};
use shotik_core::{Error, Result};

use crate::App;

/// How often the schedule looks at whether a check is due: one long sleep would not survive the computer sleeping.
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);
/// Long enough for an installer that just started this copy to have exited: the first thing due is removing it.
const TEST_FIRST_CHECK_AFTER: Duration = Duration::from_secs(2);
const MANIFEST_TIMEOUT_MS: u64 = 20_000;
const INSTALLER_TIMEOUT_MS: u64 = 60_000;
const DOWNLOAD_PREFIX: &str = "shotik-update-";

/// How this copy updates itself.
#[derive(Clone, Debug)]
pub struct UpdateConfig {
    pub source: UpdateSource,
    /// Checks on a schedule. A test instance has no business reaching GitHub, so it only does with a test server.
    pub auto_check: bool,
    pub first_check_after: Duration,
    /// Where the installer put this copy. None for a portable one: an installer would add a second copy elsewhere.
    pub install_dir: Option<PathBuf>,
}

impl UpdateConfig {
    /// `test_base` and `test_key` are `SHOTIK_UPDATE_BASE` and `SHOTIK_UPDATE_PUBLIC_KEY`. Only a test instance listens
    /// to them: they say which key to trust.
    pub fn resolve(
        test_mode: bool,
        ghost: bool,
        test_base: Option<String>,
        test_key: Option<String>,
        install_dir: Option<PathBuf>,
    ) -> Self {
        let test_source = test_base
            .filter(|_| test_mode)
            .map(|base| UpdateSource::at(&base, test_key.as_deref().unwrap_or(UPDATE_PUBLIC_KEY)));
        Self {
            auto_check: test_source.is_some() || !(test_mode || ghost),
            first_check_after: if test_source.is_some() { TEST_FIRST_CHECK_AFTER } else { FIRST_CHECK_AFTER },
            source: test_source.unwrap_or_else(UpdateSource::github),
            install_dir,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct UpdateState {
    available: Option<Manifest>,
    checking: bool,
    installing: bool,
    /// From the last look before starting the installer until this process leaves. New recordings and overlays are
    /// refused, so what starts before that look is seen by it, and what starts after it cannot be cut off.
    pub(crate) handing_over: bool,
    last_success: Option<SystemTime>,
    /// The last failure, until the next attempt.
    error: Option<String>,
    /// Which version a toast has told the user about. The tray and main window keep saying it; a toast says it once.
    announced: Option<Version>,
}

/// A panic on the way must not leave a flag up for the rest of the run: recordings or further checks would be refused
/// until a restart. The recorder treats its sessions the same way.
fn without_panic<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    panic::catch_unwind(AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err(Error::msg("the update stopped because of an internal error")))
}

fn clip(text: &str) -> String {
    text.chars().take(200).collect()
}

impl App {
    /// The `update` part of `app_state`.
    pub(crate) fn update_status(&self) -> Value {
        let state = self.state();
        let update = &state.update;
        json!({
            "current": self.env.version,
            "available": update.available.as_ref().map(|manifest| manifest.version.to_string()),
            "checking": update.checking,
            "installing": update.installing,
            "checked": update.last_success.is_some(),
            "error": update.error,
        })
    }

    pub(crate) fn available_update(&self) -> Option<Version> {
        self.state().update.available.as_ref().map(|manifest| manifest.version)
    }

    fn update_changed(&self) {
        self.refresh_tray();
        self.ports.ui.send_to_main(MainEvent::UpdateStatus(self.update_status()));
    }

    /// Runs for as long as the process does. A plain thread, because the runtime waits for its blocking tasks on the way
    /// out, and a stalled download must not hold the application open after the user quit.
    pub(crate) fn start_update_schedule(self: &Arc<Self>) {
        if !self.env.update.auto_check {
            return;
        }
        let app = self.clone();
        self.update_thread("shotik-update-schedule", move || {
            std::thread::sleep(app.env.update.first_check_after);
            app.remove_old_downloads();
            loop {
                app.update_tick(SystemTime::now());
                std::thread::sleep(LOOK_EVERY);
            }
        });
    }

    pub(crate) fn update_thread(&self, name: &str, work: impl FnOnce() + Send + 'static) {
        if let Err(e) = std::thread::Builder::new().name(name.to_string()).spawn(work) {
            self.diag.log(&format!("{name} did not start: {e}"));
        }
    }

    pub(crate) fn update_tick(self: &Arc<Self>, now: SystemTime) {
        let (last_success, failed) = {
            let state = self.state();
            (state.update.last_success, state.update.error.is_some())
        };
        if self.settings().check_updates && (failed || check_is_due(last_success, now)) {
            self.check_for_update(false);
        } else if let Some(toast) = self.available_update().and_then(|version| self.announcement(version, false)) {
            self.ports.ui.show_toast(toast);
        }
    }

    /// A toast about this version, if one should appear now. Unasked, it appears once, when it interrupts nothing: until
    /// then the tray and main window carry the offer, and the hourly look tries again. A test instance has no user to
    /// consider, only somebody's real desktop to depend on.
    fn announcement(&self, version: Version, manual: bool) -> Option<Toast> {
        let news = self.state().update.announced != Some(version);
        if !manual && !(news && self.settings().show_toast && (self.env.test_mode || self.ports.shell.notifications_welcome())) {
            return None;
        }
        self.state().update.announced = Some(version);
        Some(self.update_toast(
            "update",
            self.t_args("updateAvailableTitle", &[&version.to_string()]),
            self.t("updateAvailableBody"),
            Some(self.t("updateAction")),
        ))
    }

    /// One look at the latest release. `manual` means the user asked and is waiting to hear the answer.
    pub(crate) fn check_for_update(self: &Arc<Self>, manual: bool) {
        {
            let mut state = self.state();
            if state.update.checking || state.update.installing {
                return;
            }
            state.update.checking = true;
        }
        self.update_changed();
        let found = without_panic(|| self.newer_release());
        let toast = self.finish_check(found, manual);
        self.update_changed();
        if let Some(toast) = toast {
            self.ports.ui.show_toast(toast);
        }
    }

    fn newer_release(&self) -> Result<Option<Manifest>> {
        let source = &self.env.update.source;
        let manifest = self.download(&source.manifest_url(), MANIFEST_TIMEOUT_MS)?;
        let signature = self.download(&source.signature_url(), MANIFEST_TIMEOUT_MS)?;
        let manifest = verify_manifest(&manifest, &signature, &source.public_key)?;
        Ok((manifest.version > Version::parse(&self.env.version)?).then_some(manifest))
    }

    fn download(&self, url: &str, timeout_ms: u64) -> Result<Vec<u8>> {
        let response = self.ports.http.send(HttpRequest { method: "GET", url, headers: &[], body: None, timeout_ms })?;
        if response.status != 200 {
            return Err(Error::msg(format!("the update server answered {}", response.status)));
        }
        Ok(response.body)
    }

    fn finish_check(&self, found: Result<Option<Manifest>>, manual: bool) -> Option<Toast> {
        let mut state = self.state();
        let update = &mut state.update;
        update.checking = false;
        match found {
            Err(e) => {
                let message = clip(&e.to_string());
                update.error = Some(message.clone());
                drop(state);
                self.diag.log(&format!("update check failed: {message}"));
                manual.then(|| self.update_toast("update-failed", self.t("updateFailedTitle"), message, None))
            }
            Ok(None) => {
                update.error = None;
                update.available = None;
                update.last_success = Some(SystemTime::now());
                drop(state);
                manual.then(|| self.update_toast("update-done", self.t("updateLatestTitle"), String::new(), None))
            }
            Ok(Some(manifest)) => {
                let version = manifest.version;
                update.error = None;
                update.available = Some(manifest);
                update.last_success = Some(SystemTime::now());
                drop(state);
                self.announcement(version, manual)
            }
        }
    }

    fn update_toast(&self, kind: &str, title: String, body: String, action: Option<String>) -> Toast {
        Toast { kind: kind.into(), title, body, thumb: None, file: None, color: None, action }
    }

    fn update_failed(&self, error: &Error) {
        let message = clip(&error.to_string());
        self.state().update.error = Some(message.clone());
        self.diag.log(&format!("update failed: {message}"));
        self.ports.ui.show_toast(self.update_toast("update-failed", self.t("updateFailedTitle"), message, None));
    }

    /// Why a restart would cost the user something now: it would cut a recording, lose a file still being written, or
    /// drop a capture in progress.
    fn update_blocked(&self) -> Option<String> {
        if self.is_recording() || self.ports.recorder.has_unfinished_file() {
            Some(self.t("updateBusyRecording"))
        } else if self.state().overlay_sessions > 0 || self.ports.ui.overlay_is_open() {
            Some(self.t("updateBusyOverlay"))
        } else {
            None
        }
    }

    /// The user's click. On success the installer takes over and this process leaves.
    pub(crate) fn install_update(self: &Arc<Self>) {
        let Some((manifest, install_dir)) = self.begin_install() else {
            return;
        };
        let result = without_panic(|| self.download_and_start_installer(&manifest, &install_dir));
        match result {
            // It cannot replace the executable while this process runs.
            Ok(()) => self.shutdown(),
            Err(e) => {
                {
                    let mut state = self.state();
                    state.update.installing = false;
                    state.update.handing_over = false;
                }
                self.update_failed(&e);
                self.update_changed();
            }
        }
    }

    fn begin_install(self: &Arc<Self>) -> Option<(Manifest, PathBuf)> {
        let Some(manifest) = self.state().update.available.clone() else {
            self.diag.log("update requested with none available");
            return None;
        };
        let Some(install_dir) = self.env.update.install_dir.clone() else {
            let page = self.env.update.source.release_page(manifest.version);
            if let Err(e) = self.ports.shell.open_url(&page) {
                self.update_failed(&e);
            }
            return None;
        };
        if let Some(reason) = self.update_blocked() {
            self.ports.ui.show_toast(self.update_toast("update-failed", self.t("updateBusyTitle"), reason, None));
            return None;
        }
        {
            let mut state = self.state();
            if state.update.installing {
                return None;
            }
            state.update.installing = true;
            state.update.error = None;
        }
        self.update_changed();
        self.ports.ui.show_toast(self.update_toast(
            "update",
            self.t("updateDownloadingTitle"),
            self.t("updateDownloadingBody"),
            None,
        ));
        Some((manifest, install_dir))
    }

    fn download_and_start_installer(&self, manifest: &Manifest, install_dir: &Path) -> Result<()> {
        let url = self.env.update.source.installer_url(manifest.version);
        let installer = self.download(&url, INSTALLER_TIMEOUT_MS)?;
        check_installer(&installer, manifest)?;
        // A folder of its own: an installer loads libraries from where it sits, and the temp folder holds anything.
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos());
        let dir = self.env.temp_dir.join(format!("{DOWNLOAD_PREFIX}{}-{unique}", std::process::id()));
        std::fs::create_dir(&dir).map_err(|e| Error::io("create the update folder", &dir, e))?;
        let result = self.start_installer(&dir, &installer, manifest, install_dir);
        if result.is_err() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                self.diag.log(&format!("update folder left behind: {e}"));
            }
        }
        result
    }

    fn start_installer(&self, dir: &Path, installer: &[u8], manifest: &Manifest, install_dir: &Path) -> Result<()> {
        let path = dir.join(installer_name(manifest.version));
        std::fs::write(&path, installer).map_err(|e| Error::io("write the update", &path, e))?;
        // The download took a while, and a recording may have started meanwhile. The flag goes up first: see `handing_over`.
        self.state().update.handing_over = true;
        if let Some(reason) = self.update_blocked() {
            return Err(Error::msg(reason));
        }
        self.ports.shell.run_installer(&path, install_dir, self.ports.ui.main_visible())
    }

    /// An installer cannot delete itself, so the version it installed cleans up after it, once it has surely exited.
    pub(crate) fn remove_old_downloads(&self) {
        let own = format!("{DOWNLOAD_PREFIX}{}-", std::process::id());
        let entries = match std::fs::read_dir(&self.env.temp_dir) {
            Ok(entries) => entries,
            Err(e) => {
                self.diag.log(&format!("cannot look for old updates: {e}"));
                return;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(DOWNLOAD_PREFIX) || name.starts_with(&own) {
                continue;
            }
            if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                self.diag.log(&format!("old update {name} left behind: {e}"));
            }
        }
    }

    /// What the installer said when it started this process.
    pub(crate) fn announce_update_result(&self, argv: &[String]) {
        if argv.iter().any(|a| a == "--updated") {
            self.diag.log(&format!("updated to {}", self.env.version));
            if self.settings().show_toast {
                self.ports.ui.show_toast(self.update_toast(
                    "update-done",
                    self.t_args("updateDoneTitle", &[&self.env.version]),
                    self.t("updateDoneBody"),
                    None,
                ));
            }
        } else if argv.iter().any(|a| a == "--update-failed") {
            self.diag.log("the installer could not replace the executable");
            self.ports.ui.show_toast(self.update_toast(
                "update-failed",
                self.t("updateFailedTitle"),
                self.t("updateFailedInstallBody"),
                None,
            ));
        }
    }
}
