//! The ten MCP tools from `registerMcpTools` in `src/main/main.js`.
use std::fs;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use shotik_core::ports::{
    HistoryFilter, MainEvent, McpServices, RegionSelection, WindowCaptureMethod,
};
use shotik_core::{Error, Frame, Rect, Result};
use tokio::sync::oneshot;

use crate::js;
use crate::region_map::{map_global_region, validate_region};
use crate::window_capture::{self, WindowCaptureOptions};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Copied verbatim from the JS registration. A contract test evaluates that
/// registration independently and compares every name, description and schema.
pub fn tool_definitions() -> Vec<ToolDefinition> {
    serde_json::from_str(include_str!("tool-definitions.json"))
        .expect("checked-in tool definitions")
}

#[derive(Clone)]
pub struct McpTools {
    services: McpServices,
    window_options: WindowCaptureOptions,
}

impl McpTools {
    pub fn new(services: McpServices, window_options: WindowCaptureOptions) -> Self {
        Self {
            services,
            window_options,
        }
    }

    pub(crate) fn set_window_options(&mut self, options: WindowCaptureOptions) {
        self.window_options = options;
    }

    /// The HTTP layer checks the registry first, so unknown names are RPC
    /// -32602 errors rather than a tool result with isError.
    pub async fn call(&self, name: &str, args: Value) -> Result<Vec<Value>> {
        if name == "ask_user_to_select_region" {
            return self.select_region().await;
        }
        let this = self.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || this.call_blocking(&name, &args))
            .await
            .map_err(|e| Error::msg(format!("MCP tool task failed: {e}")))?
    }

    async fn select_region(&self) -> Result<Vec<Value>> {
        let services = self.services.clone();
        let waiting = async move {
            let receiver = tokio::task::spawn_blocking(move || {
                if services.ui.overlay_is_open() {
                    return Err(Error::msg("Selection overlay is already open"));
                }
                let (sender, receiver) = oneshot::channel();
                (services.select_region)(
                    120_000,
                    Box::new(move |out| {
                        let _ = sender.send(out);
                    }),
                );
                Ok(receiver)
            })
            .await
            .map_err(|e| Error::msg(format!("MCP selection task failed: {e}")))??;
            receiver
                .await
                .map_err(|_| Error::msg("Selection callback was dropped"))?
        };
        let selection = match tokio::time::timeout(Duration::from_secs(120), waiting).await {
            Ok(result) => result?,
            Err(_) => RegionSelection::TimedOut,
        };
        match selection {
            RegionSelection::Cancelled => Ok(vec![text("The user cancelled the selection.")]),
            RegionSelection::TimedOut => Ok(vec![text("Timed out waiting for the user (2 min).")]),
            RegionSelection::Selected { png } => {
                let settings = self.services.settings.clone();
                tokio::task::spawn_blocking(move || {
                    Ok(vec![image_content(
                        &png,
                        settings.get().downscale_for_ai,
                        false,
                    )?])
                })
                .await
                .map_err(|e| Error::msg(format!("MCP image task failed: {e}")))?
            }
        }
    }

    fn call_blocking(&self, name: &str, args: &Value) -> Result<Vec<Value>> {
        let s = &self.services;
        let full_res = js::truthy(args.get("full_resolution"));
        match name {
            "take_screenshot" => {
                // Fullscreen's reference deliberately does not coerce display_id:
                // a numeric string is unknown and falls back to primary.
                let display = if js::truthy(args.get("display_id")) {
                    s.displays
                        .all()
                        .into_iter()
                        .find(|d| {
                            args.get("display_id").and_then(Value::as_f64) == Some(d.id as f64)
                        })
                        .unwrap_or_else(|| s.displays.primary())
                } else {
                    s.displays.under_cursor()
                };
                let frame = s.capture.grab(&display)?;
                let png = encode_capture(&frame)?;
                let entry = s.history.save_png(&png, &frame, "mcp", None)?;
                s.ui.send_to_main(MainEvent::HistoryChanged);
                let b = display.bounds;
                let primary = if display.id == s.displays.primary().id {
                    "primary"
                } else {
                    "secondary"
                };
                Ok(vec![image_content(&png, s.settings.get().downscale_for_ai, full_res)?,
                    text(format!("Captured display {} ({primary}, bounds {}x{} at {},{}). Saved to {} ({}x{}px)",
                        display.id, b.width, b.height, b.x, b.y, entry.file.display(), entry.width, entry.height))])
            }
            "take_screenshot_region" => {
                let rect = validate_region(args)?;
                let id = args
                    .get("display_id")
                    .filter(|v| !v.is_null())
                    .map(|v| js::number(Some(v)));
                let mapped = map_global_region(&s.displays.all(), rect, id)?;
                let display = s.displays.by_id(mapped.display_id);
                let frame = s.capture.grab(&display)?;
                let r = mapped.rel_rect;
                let scale = display.scale_factor;
                let x = js::round(f64::from(r.x) * scale).max(0.0) as i32;
                let y = js::round(f64::from(r.y) * scale).max(0.0) as i32;
                let width = js::round(f64::from(r.width) * scale)
                    .min(f64::from(frame.width()) - f64::from(x))
                    as i32;
                let height = js::round(f64::from(r.height) * scale)
                    .min(f64::from(frame.height()) - f64::from(y))
                    as i32;
                if width < 1 || height < 1 {
                    return Err(Error::msg("Region outside display bounds"));
                }
                let png = frame.crop(Rect::new(x, y, width, height))?.to_png()?;
                let g = mapped.global_rect;
                let note = if mapped.clipped {
                    format!(
                        " (clipped to display {}: requested region crossed its edge)",
                        mapped.display_id
                    )
                } else {
                    format!(" (display {})", mapped.display_id)
                };
                Ok(vec![
                    image_content(&png, s.settings.get().downscale_for_ai, full_res)?,
                    text(format!(
                        "Captured {}x{} DIP at global ({}, {}){note}",
                        g.width, g.height, g.x, g.y
                    )),
                ])
            }
            "list_windows" => Ok(vec![json_text(&window_capture::list_windows(
                s.windows.as_ref(),
                s.displays.as_ref(),
                self.window_options,
            )?)?]),
            "capture_window" => {
                let captured = window_capture::capture_window(
                    s.windows.as_ref(),
                    args.get("window_id"),
                    args.get("title"),
                    self.window_options,
                )?;
                let png = encode_capture(&captured.frame)?;
                let entry = s.history.save_png(&png, &captured.frame, "mcp", None)?;
                s.ui.send_to_main(MainEvent::HistoryChanged);
                let mut extra = String::new();
                if !captured.other_matches.is_empty() {
                    let titles = captured
                        .other_matches
                        .iter()
                        .map(|s| format!("\"{s}\""))
                        .collect::<Vec<_>>()
                        .join(", ");
                    extra.push_str(&format!(" Other windows matching the title: {titles} — pass window_id to disambiguate."));
                }
                if captured.uniform {
                    extra.push_str(" Note: the image is one flat color — the window may be genuinely blank, or it renders on a GPU-only surface this method cannot read.");
                }
                let app = captured.app.map_or_else(String::new, |s| format!(" ({s})"));
                let method = match captured.method {
                    WindowCaptureMethod::PrintWindow => "printwindow",
                    WindowCaptureMethod::GraphicsCapture => "graphicscapture",
                };
                Ok(vec![
                    image_content(&png, s.settings.get().downscale_for_ai, full_res)?,
                    text(format!(
                        "Captured window \"{}\"{app} via {method}. Saved to {} ({}x{}px).{extra}",
                        captured.title,
                        entry.file.display(),
                        entry.width,
                        entry.height
                    )),
                ])
            }
            "get_last_screenshot" => {
                let Some(last) = s.history.list(1, HistoryFilter::Images).into_iter().next() else {
                    return Ok(vec![text("No screenshots in history yet.")]);
                };
                let png = fs::read(&last.file).map_err(|e| Error::io("read", &last.file, e))?;
                Ok(vec![
                    image_content(&png, s.settings.get().downscale_for_ai, full_res)?,
                    text(format!(
                        "{} ({}x{}px, taken {})",
                        last.file.display(),
                        last.width,
                        last.height,
                        iso(last.ts)?
                    )),
                ])
            }
            "list_screenshots" => {
                // JS Array.slice(0, limit): negative values count from the end.
                let value = args.get("limit").filter(|v| js::truthy(Some(v)));
                let n = if value.is_none() {
                    10.0
                } else {
                    js::number(value)
                };
                let n = if n.is_nan() { 0.0 } else { n.min(50.0).trunc() };
                let items = if n < 0.0 {
                    let mut items = s.history.list(usize::MAX, HistoryFilter::Images);
                    items.truncate((items.len() as f64 + n).max(0.0) as usize);
                    items
                } else {
                    s.history.list(n as usize, HistoryFilter::Images)
                };
                let items: Result<Vec<_>> = items
                    .into_iter()
                    .map(|it| {
                        Ok(json!({
                            "file": it.file, "width": it.width, "height": it.height,
                            "taken": iso(it.ts)?, "source": it.source,
                        }))
                    })
                    .collect();
                Ok(vec![json_text(&items?)?])
            }
            "list_recordings" => {
                self.with_recording_note(vec![json_text(&self.find_recordings(args, 10)?)?])
            }
            "get_last_recording" => {
                let mut query = json!({"limit": 1});
                if let Some(kind) = args.get("kind") {
                    query["kind"] = kind.clone();
                }
                let last = self.find_recordings(&query, 1)?.into_iter().next();
                let out = if let Some(last) = last {
                    json_text(&last)?
                } else {
                    let kind = args
                        .get("kind")
                        .and_then(Value::as_str)
                        .map_or_else(String::new, |s| format!("{s} "));
                    text(format!("No {kind}recordings in history yet."))
                };
                self.with_recording_note(vec![out])
            }
            "list_displays" => {
                let primary = s.displays.primary().id;
                let items: Vec<_> = s.displays.all().into_iter().map(|d| json!({
                    "id": d.id, "bounds": d.bounds, "scaleFactor": js::number_value(d.scale_factor), "primary": d.id == primary,
                })).collect();
                Ok(vec![json_text(&items)?])
            }
            _ => Err(Error::msg(format!("Unknown tool: {name}"))),
        }
    }

    fn find_recordings(&self, args: &Value, default_limit: usize) -> Result<Vec<Value>> {
        let filter = match args.get("kind") {
            None => HistoryFilter::Recordings,
            Some(Value::String(kind)) if kind == "audio" => HistoryFilter::Audio,
            Some(Value::String(kind)) if kind == "video" => HistoryFilter::Video,
            kind => {
                return Err(Error::msg(format!(
                    "\"kind\" must be \"audio\" or \"video\", got {}",
                    js::json(kind)
                )))
            }
        };
        let n = js::number(args.get("limit")).floor();
        let limit = if n > 0.0 {
            n.min(50.0) as usize
        } else {
            default_limit
        };
        self.services
            .history
            .list(limit, filter)
            .into_iter()
            .map(|it| {
                // Reference intentionally reports 0 if stat fails after history.list.
                let size = fs::metadata(&it.file).map_or(0, |m| m.len());
                let mut out = json!({ "file": it.file, "kind": it.kind,
                    "duration_sec": js::number_value(it.duration.unwrap_or(0.0)),
                    "size_mb": js::number_value(js::round(size as f64 / 1_048_576.0 * 10.0) / 10.0),
                    "recorded": iso(it.ts)?,
                });
                if it.kind.as_deref() == Some("video") {
                    out["width"] = it.width.into();
                    out["height"] = it.height.into();
                }
                Ok(out)
            })
            .collect()
    }

    fn with_recording_note(&self, mut content: Vec<Value>) -> Result<Vec<Value>> {
        if self.services.recorder.is_recording() {
            let what = if self.services.recorder.status().audio_only == Some(true) {
                "An audio recording"
            } else {
                "A screen recording"
            };
            content.push(text(format!(
                "{what} is in progress right now; it appears here once stopped."
            )));
        }
        Ok(content)
    }
}

fn encode_capture(frame: &Frame) -> Result<Vec<u8>> {
    if frame.width() == 0 || frame.height() == 0 {
        return Err(Error::msg("Capture produced an empty image"));
    }
    frame.to_png()
}

/// Validates, optionally downsizes to 1600px, and encodes the MCP image block.
pub fn image_content(png: &[u8], downscale_for_ai: bool, full_res: bool) -> Result<Value> {
    if png.is_empty() {
        return Err(Error::msg("Capture produced an empty image"));
    }
    let mut frame =
        Frame::from_png(png).map_err(|_| Error::msg("Capture produced an unreadable image"))?;
    if downscale_for_ai && !full_res {
        frame = frame.fit_within(1600)?;
    }
    Ok(
        json!({ "type": "image", "data": STANDARD.encode(frame.to_png()?), "mimeType": "image/png" }),
    )
}

fn text(value: impl Into<String>) -> Value {
    json!({"type": "text", "text": value.into()})
}

fn json_text(value: &impl Serialize) -> Result<Value> {
    serde_json::to_string_pretty(value)
        .map(text)
        .map_err(|e| Error::msg(e.to_string()))
}

fn iso(ts: i64) -> Result<String> {
    DateTime::<Utc>::from_timestamp_millis(ts)
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| Error::msg("Invalid time value"))
}
