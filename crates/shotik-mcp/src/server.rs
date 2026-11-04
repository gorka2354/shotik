use std::collections::VecDeque;
use std::io;
use std::net::Ipv4Addr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::Response;
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use shotik_core::ports::{Callback, MainEvent, McpServices};
use shotik_core::Result;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use uuid::Uuid;

use crate::js;
use crate::tools::{tool_definitions, McpTools, ToolDefinition};
use crate::WindowCaptureOptions;

pub const BODY_LIMIT: usize = 64 * 1024 * 1024;
const LOG_MAX: usize = 60;
const PROTOCOL_FALLBACK: &str = "2025-03-26";

/// Supplied only in test mode by shotik-app. Runs on a blocking worker so route
/// logic can use synchronous application ports without stalling HTTP requests.
pub type TestHandler = Arc<dyn Fn(&str, Value) -> Result<Value> + Send + Sync>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CallLogEntry {
    pub ts: u64,
    pub tool: String,
    pub args: Value,
    pub ms: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct ServerStatus {
    pub running: bool,
    pub port: Option<u16>,
}

struct RequestState {
    version: String,
    tools: McpTools,
    definitions: Vec<ToolDefinition>,
    test_handler: Option<TestHandler>,
    log: Mutex<VecDeque<CallLogEntry>>,
    listener: RwLock<Option<Callback<CallLogEntry>>>,
}

struct RunningServer {
    port: u16,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<io::Result<()>>,
}

pub struct McpServer {
    state: Arc<RequestState>,
    running: Mutex<Option<RunningServer>>,
    lifecycle: tokio::sync::Mutex<()>,
}

impl McpServer {
    pub fn new(version: impl Into<String>, services: McpServices) -> Self {
        let ui = services.ui.clone();
        Self {
            state: Arc::new(RequestState {
                version: version.into(),
                tools: McpTools::new(services, WindowCaptureOptions::default()),
                definitions: tool_definitions(),
                test_handler: None,
                log: Mutex::new(VecDeque::new()),
                listener: RwLock::new(Some(Arc::new(move |entry| {
                    ui.send_to_main(MainEvent::McpLog(
                        serde_json::to_value(entry).expect("serializable call log"),
                    ));
                }))),
            }),
            running: Mutex::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
        }
    }

    /// Startup-only configuration; a running server's routes cannot change.
    pub fn with_test_handler(mut self, handler: TestHandler) -> Self {
        Arc::get_mut(&mut self.state)
            .expect("configure before start")
            .test_handler = Some(handler);
        self
    }

    pub fn with_window_capture_options(mut self, options: WindowCaptureOptions) -> Self {
        let state = Arc::get_mut(&mut self.state).expect("configure before start");
        state.tools.set_window_options(options);
        self
    }

    pub fn set_log_listener(&self, listener: Option<Callback<CallLogEntry>>) {
        *self.state.listener.write().expect("log listener lock") = listener;
    }

    /// Newest first. The copy cannot mutate the server's ring buffer.
    pub fn get_log(&self) -> Vec<CallLogEntry> {
        self.state
            .log
            .lock()
            .expect("call log lock")
            .iter()
            .cloned()
            .collect()
    }

    pub fn status(&self) -> ServerStatus {
        let running = self.running.lock().expect("server state lock");
        ServerStatus {
            running: running.is_some(),
            port: running.as_ref().map(|s| s.port),
        }
    }

    /// Idempotent, including concurrent callers. The return value is the bound
    /// port, which may differ from the requested port when zero was requested.
    pub async fn start(&self, port: u16) -> io::Result<u16> {
        let _guard = self.lifecycle.lock().await;
        if let Some(port) = self.status().port {
            return Ok(port);
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let port = listener.local_addr()?.port();
        let app = Router::new()
            .fallback(request_listener)
            .with_state(self.state.clone());
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = receiver.await;
                })
                .await
        });
        *self.running.lock().expect("server state lock") = Some(RunningServer {
            port,
            shutdown,
            task,
        });
        Ok(port)
    }

    /// Stops accepting requests, then waits for active requests to finish.
    pub async fn stop(&self) -> io::Result<()> {
        let _guard = self.lifecycle.lock().await;
        let running = self.running.lock().expect("server state lock").take();
        if let Some(running) = running {
            let _ = running.shutdown.send(());
            running.task.await.map_err(io::Error::other)??;
        }
        Ok(())
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Some(running) = self.running.get_mut().expect("server state lock").take() {
            let _ = running.shutdown.send(());
        }
    }
}

impl RequestState {
    fn push_log(&self, entry: CallLogEntry) {
        {
            let mut log = self.log.lock().expect("call log lock");
            log.push_front(entry.clone());
            log.truncate(LOG_MAX);
        }
        let listener = self.listener.read().expect("log listener lock").clone();
        if let Some(listener) = listener {
            // As in JS, a broken UI listener cannot break a successful tool.
            let _ = catch_unwind(AssertUnwindSafe(|| listener(entry)));
        }
    }

    async fn handle_rpc(&self, msg: Value) -> std::result::Result<Option<Value>, String> {
        if msg.is_null() {
            return Err("Cannot destructure property 'method' of 'msg' as it is null.".into());
        }
        let method = msg.get("method");
        let id = msg.get("id");
        let params = msg.get("params");
        match method.and_then(Value::as_str) {
            Some("initialize") => {
                let protocol = params
                    .and_then(|p| p.get("protocolVersion"))
                    .filter(|v| js::truthy(Some(v)))
                    .cloned()
                    .unwrap_or_else(|| PROTOCOL_FALLBACK.into());
                Ok(Some(rpc_result(
                    id,
                    json!({
                        "protocolVersion": protocol,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "shotik", "title": "Shotik Screenshots", "version": self.version},
                        "instructions": "Screenshot tools for the user's screen. Use take_screenshot to see the screen, ask_user_to_select_region to let the user mark a specific area.",
                    }),
                )))
            }
            Some("ping") => Ok(Some(rpc_result(id, json!({})))),
            Some("tools/list") => Ok(Some(rpc_result(id, json!({"tools": self.definitions})))),
            Some("tools/call") => {
                let name = js::string(params.and_then(|p| p.get("name")));
                if !self.definitions.iter().any(|t| t.name == name) {
                    return Ok(Some(rpc_error(id, -32602, format!("Unknown tool: {name}"))));
                }
                let args = params
                    .and_then(|p| p.get("arguments"))
                    .filter(|v| js::truthy(Some(v)))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let started = Instant::now();
                let result = self.tools.call(&name, args.clone()).await;
                self.push_log(CallLogEntry {
                    ts: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    tool: name,
                    args,
                    ms: started.elapsed().as_millis() as u64,
                    ok: result.is_ok(),
                    error: result.as_ref().err().map(ToString::to_string),
                });
                let result = match result {
                    Ok(content) => json!({"content": content, "isError": false}),
                    Err(error) => {
                        json!({"content": [{"type": "text", "text": format!("Error: {error}")}], "isError": true})
                    }
                };
                Ok(Some(rpc_result(id, result)))
            }
            _ if id.is_none_or(Value::is_null) => Ok(None),
            _ => Ok(Some(rpc_error(
                id,
                -32601,
                format!("Method not found: {}", js::string(method)),
            ))),
        }
    }
}

/// Binding to 127.0.0.1 keeps other machines out, not web pages in the user's own browser: a page can
/// POST here, and through DNS rebinding read the reply, which would hand any site a screenshot. Only
/// loopback hosts and origins pass. MCP clients outside a browser send no Origin at all.
pub fn is_local_request(host: Option<&str>, origin: Option<&str>) -> bool {
    let host_ok = host.is_some_and(|host| is_loopback_name(strip_port(host)));
    let origin_ok = origin.is_none_or(|origin| {
        ["http://", "https://"]
            .iter()
            .find_map(|scheme| origin.strip_prefix(scheme))
            .is_some_and(|authority| is_loopback_name(strip_port(authority)))
    });
    host_ok && origin_ok
}

fn strip_port(authority: &str) -> &str {
    match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    }
}

fn is_loopback_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "::1"
}

async fn request_listener(State(state): State<Arc<RequestState>>, request: Request) -> Response {
    let local = {
        let headers = request.headers();
        let text = |name| headers.get(name).and_then(|v: &HeaderValue| v.to_str().ok());
        is_local_request(text(header::HOST), text(header::ORIGIN))
    };
    if !local {
        return json_response(StatusCode::FORBIDDEN, json!({"error": "forbidden"}));
    }
    let path = request.uri().path().to_owned();
    if let Some(handler) = state
        .test_handler
        .as_ref()
        .filter(|_| path.starts_with("/test/"))
    {
        let body = if request.method() == Method::POST {
            match to_bytes(request.into_body(), BODY_LIMIT).await {
                Ok(bytes) => {
                    let bytes = if bytes.is_empty() { b"{}" } else { &bytes[..] };
                    match serde_json::from_slice(bytes) {
                        Ok(value) => value,
                        Err(error) => {
                            return json_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                json!({"ok": false, "error": error.to_string()}),
                            )
                        }
                    }
                }
                Err(error) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"ok": false, "error": error.to_string()}),
                    )
                }
            }
        } else {
            json!({})
        };
        let handler = handler.clone();
        let route = path[6..].to_owned();
        return match tokio::task::spawn_blocking(move || handler(&route, body)).await {
            Ok(Ok(result)) => json_response(StatusCode::OK, json!({"ok": true, "result": result})),
            Ok(Err(error)) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"ok": false, "error": error.to_string()}),
            ),
            Err(error) => json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"ok": false, "error": error.to_string()}),
            ),
        };
    }
    if path != "/mcp" {
        return json_response(StatusCode::NOT_FOUND, json!({"error": "not found"}));
    }
    if request.method() == Method::DELETE {
        return empty_response(StatusCode::OK);
    }
    if request.method() != Method::POST {
        let mut response = empty_response(StatusCode::METHOD_NOT_ALLOWED);
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST, DELETE"));
        return response;
    }
    let parsed: Value = match to_bytes(request.into_body(), BODY_LIMIT).await {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    rpc_error(Some(&Value::Null), -32700, "Parse error"),
                )
            }
        },
        Err(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                rpc_error(Some(&Value::Null), -32700, "Parse error"),
            )
        }
    };
    if parsed.is_null() {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Cannot read properties of null (reading 'method')"}),
        );
    }
    let is_init = parsed.get("method").and_then(Value::as_str) == Some("initialize");
    let answer = if let Value::Array(messages) = parsed {
        // Promise.all preserves input order but rejects as soon as any member
        // fails. Its other calls continue and still update the tool log.
        let mut tasks = JoinSet::new();
        let mut answers = vec![None; messages.len()];
        for (index, msg) in messages.into_iter().enumerate() {
            let state = state.clone();
            tasks.spawn(async move { (index, state.handle_rpc(msg).await) });
        }
        while let Some(answer) = tasks.join_next().await {
            match answer {
                Ok((index, Ok(answer))) => answers[index] = answer,
                Ok((_, Err(error))) => {
                    tasks.detach_all();
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"error": error}),
                    );
                }
                Err(error) => {
                    tasks.detach_all();
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"error": error.to_string()}),
                    );
                }
            }
        }
        let answers: Vec<_> = answers.into_iter().flatten().collect();
        if answers.is_empty() {
            None
        } else {
            Some(Value::Array(answers))
        }
    } else {
        match state.handle_rpc(parsed).await {
            Ok(answer) => answer,
            Err(error) => {
                return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": error}))
            }
        }
    };
    let mut response = match answer {
        Some(answer) => json_response(StatusCode::OK, answer),
        None => empty_response(StatusCode::ACCEPTED),
    };
    if is_init {
        response.headers_mut().insert(
            "mcp-session-id",
            HeaderValue::from_str(&Uuid::new_v4().to_string()).expect("UUID is a header value"),
        );
    }
    response
}

fn rpc_result(id: Option<&Value>, result: Value) -> Value {
    let mut out = json!({"jsonrpc": "2.0"});
    if let Some(id) = id {
        out["id"] = id.clone();
    }
    out["result"] = result;
    out
}

fn rpc_error(id: Option<&Value>, code: i32, message: impl Into<String>) -> Value {
    let mut out = json!({"jsonrpc": "2.0"});
    if let Some(id) = id {
        out["id"] = id.clone();
    }
    out["error"] = json!({"code": code, "message": message.into()});
    out
}

fn empty_response(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .expect("valid response")
}

fn json_response(status: StatusCode, value: Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(value.to_string()))
        .expect("valid response")
}
