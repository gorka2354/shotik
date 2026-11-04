#![windows_subsystem = "windows"]
//! `cargo run -p shotik-ui --example main_preview`: ghost-only contract checks and eight PNGs.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use shotik_core::ports::{MainEvent, RecStatus, Theme, UiPort};
use shotik_core::{Error, Result};
use shotik_ui::main_window::fixtures::{check_protocol, private_bytes, until, PreviewHost};
use shotik_ui::{Ui, UiEnv};

fn run(ui: &Ui, host: &PreviewHost) -> Result<()> {
    let history = host.history.lock().unwrap().clone();
    ui.show_main();
    check_protocol(ui, host)?;
    *host.history.lock().unwrap() = history;
    ui.send_to_main(MainEvent::HistoryChanged);
    ui.send_to_main(MainEvent::RecState(RecStatus::default()));
    ui.ui_action("main", 0, json!({"type":"reload"}))?;
    until(ui, |s| s["ready"] == true)?;
    host.reference_translation.store(true, Ordering::SeqCst);
    ui.show_main();
    until(ui, |s| s["ready"] == true)?;
    let mut images = Vec::new();
    for dark in [false, true] {
        let theme = Theme {
            dark,
            accent: "#006FC7".into(),
            platform: "win32".into(),
        };
        *host.stub.theme.lock().unwrap() = theme.clone();
        ui.apply_theme(theme);
        for (page, name) in [
            ("history", "shots"),
            ("text", "text"),
            ("claude", "claude"),
            ("settings", "settings"),
        ] {
            if page == "text" {
                ui.ui_action("main",0,json!({"type":"quick-translate","text":"Hello Shotik, this is a reference translation."}))?;
                until(ui, |s| {
                    s["text"]["loading"] == false && s["text"]["resultVisible"] == true
                })?;
            } else {
                ui.ui_action("main", 0, json!({"type":"navigate","page":page}))?;
            }
            let path = host.root.join(format!(
                "main-{name}-{}.png",
                if dark { "dark" } else { "light" }
            ));
            let size = ui.capture_page("main", 0, &path)?;
            assert_eq!(size, (1120, 740));
            images.push(path);
        }
    }
    ui.ui_action("main", 0, json!({"type":"navigate","page":"history"}))?;
    std::thread::sleep(Duration::from_millis(300));
    let open = private_bytes()?;
    ui.hide_main();
    assert!(!ui.main_visible());
    std::thread::sleep(Duration::from_secs(10));
    let hidden = private_bytes()?;
    let report = json!({"images":images,"privateBytesOpen":open,"privateBytes10sAfterHide":hidden,"protocol":"passed","ghost":true});
    std::fs::write(
        host.root.join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| Error::msg(e.to_string()))?,
    )
    .map_err(|e| Error::io("write preview report", &host.root, e))?;
    println!("{}", report);
    Ok(())
}

fn main() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "shotik-main-preview-{}-{}",
        std::process::id(),
        chrono::Local::now().timestamp_millis()
    ));
    let host = PreviewHost::new(&root)?;
    let ui = Arc::new(Ui::new(UiEnv {
        ghost: true,
        popup_real: false,
        translate_popup_ttl_ms: 9000,
    })?);
    ui.set_host(host.clone());
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker_ui = ui.clone();
    let worker = std::thread::spawn(move || {
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&worker_ui, &host)))
                .unwrap_or_else(|_| Err(Error::msg("main preview assertion failed")));
        let _ = sender.send(outcome);
        Ui::quit();
    });
    ui.run()?;
    worker
        .join()
        .map_err(|_| Error::msg("main preview worker panicked"))?;
    receiver.recv().map_err(|e| Error::msg(e.to_string()))?
}
