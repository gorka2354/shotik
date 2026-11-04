//! Starting the MCP server with the application's services.

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use shotik_core::ports::{MainEvent, McpServices, Once, OverlayMode, RegionSelection};
use shotik_core::{Error, Result};
use shotik_mcp::{McpServer, WindowCaptureOptions};

use crate::actions::text_toast;
use crate::overlay::OverlayResult;
use crate::App;

#[derive(Default)]
pub(crate) struct McpSlot(Mutex<Option<Arc<McpServer>>>);

impl McpSlot {
    pub(crate) fn get(&self) -> Option<Arc<McpServer>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, server: Arc<McpServer>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(server);
    }
}

impl App {
    fn mcp_server(self: &Arc<Self>) -> Arc<McpServer> {
        if let Some(server) = self.mcp.get() {
            return server;
        }
        let select_app = self.clone();
        let services = McpServices {
            settings: self.ports.settings.clone(),
            history: self.ports.history.clone(),
            displays: self.ports.displays.clone(),
            capture: self.ports.capture.clone(),
            windows: self.ports.windows.clone(),
            recorder: self.ports.recorder.clone(),
            ui: self.ports.ui.clone(),
            select_region: Arc::new(move |timeout_ms, done| select_app.select_region_for_mcp(timeout_ms, done)),
        };
        let mut server = McpServer::new(self.env.version.clone(), services).with_window_capture_options(WindowCaptureOptions {
            // Test instances include their own windows, so ghost tests can capture themselves.
            exclude_pid: if self.env.test_mode { None } else { Some(std::process::id()) },
            force_print_window_failure: self.env.wincap_fail,
        });
        if self.env.test_mode {
            let app = self.clone();
            server = server.with_test_handler(Arc::new(move |route, body| app.test_route(route, body)));
        }
        let app = self.clone();
        server.set_log_listener(Some(Arc::new(move |entry| {
            let value = serde_json::to_value(&entry).unwrap_or(Value::Null);
            app.ports.ui.send_to_main(MainEvent::McpLog(value));
        })));
        let server = Arc::new(server);
        self.mcp.set(server.clone());
        server
    }

    pub(crate) fn mcp_status(&self) -> Value {
        match self.mcp.get() {
            Some(server) => {
                let status = server.status();
                json!({ "running": status.running, "port": status.port })
            }
            None => json!({ "running": false, "port": null }),
        }
    }

    /// A port that is taken must be visible to the user, not just a log line.
    pub(crate) fn start_mcp(self: &Arc<Self>) {
        let st = self.settings();
        if !st.mcp.enabled {
            return;
        }
        let port = self.env.port_override.unwrap_or(st.mcp.port);
        let server = self.mcp_server();
        let app = self.clone();
        self.rt.spawn(async move {
            if let Err(e) = server.start(port).await {
                let port_text = port.to_string();
                let error = e.to_string();
                let body = app.t_args("mcpFailBody", &[&port_text, &error]);
                app.ports.ui.show_toast(text_toast(app.t("mcpFailTitle"), body));
            }
        });
    }

    /// `ask_user_to_select_region`. The JS version left the overlay open after the timeout with nobody
    /// waiting for the result; here it closes.
    fn select_region_for_mcp(self: &Arc<Self>, timeout_ms: u64, done: Once<Result<RegionSelection>>) {
        if self.ports.ui.overlay_is_open() {
            done(Err(Error::msg("Selection overlay is already open")));
            return;
        }
        self.spawn(move |app| {
            let (result_tx, result_rx) = mpsc::channel();
            let session = app.clone();
            app.spawn(move |_| {
                let _ = result_tx.send(session.run_overlay(OverlayMode::Claude));
            });
            let selection = match result_rx.recv_timeout(Duration::from_millis(timeout_ms)) {
                Ok(OverlayResult::Action(action)) => {
                    let png = action.png.clone();
                    app.handle_overlay_result(action);
                    match png {
                        Some(png) => Ok(RegionSelection::Selected { png }),
                        None => Err(Error::msg("The overlay returned no image")),
                    }
                }
                Ok(OverlayResult::Busy) => Err(Error::msg("Selection overlay is already open")),
                Ok(OverlayResult::Cancelled) => Ok(RegionSelection::Cancelled),
                Err(_) => {
                    app.ports.ui.overlay_cancel();
                    Ok(RegionSelection::TimedOut)
                }
            };
            done(selection);
        });
    }
}
