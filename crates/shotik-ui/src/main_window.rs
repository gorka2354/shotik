//! Sidebar, shots gallery, text, Claude and settings pages.

use std::path::Path;

use serde_json::Value;
use shotik_core::ports::{MainEvent, Theme};
use shotik_core::{Error, Result};

pub fn show() {}
pub fn hide() {}
pub fn visible() -> bool {
    false
}
pub fn send(_event: MainEvent) {}
pub fn apply_theme(_theme: &Theme) {}
pub fn rebuild_language() {}
pub fn ui_state() -> Result<Value> {
    Err(Error::msg("not implemented yet"))
}
pub fn ui_action(_action: Value) -> Result<Value> {
    Err(Error::msg("not implemented yet"))
}
pub fn capture_page(_path: &Path) -> Result<(u32, u32)> {
    Err(Error::msg("not implemented yet"))
}
