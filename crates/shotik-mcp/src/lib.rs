//! Stateless Streamable HTTP MCP, compatible with `src/main/mcp.js`.
//!
//! Construct [`McpServer`] with the application's [`McpServices`], its version,
//! and optional test/window options; call `start(port)` on a Tokio runtime.
//! Only 127.0.0.1 is ever bound. Port zero selects an ephemeral port for tests.
//! This crate has no native adapters, desktop interaction or environment reads.

mod js;
pub mod region_map;
mod server;
pub mod tools;
pub mod window_capture;

pub use server::{is_local_request, CallLogEntry, McpServer, ServerStatus, TestHandler, BODY_LIMIT};
pub use shotik_core::ports::McpServices;
pub use tools::{tool_definitions, McpTools, ToolDefinition};
pub use window_capture::WindowCaptureOptions;
