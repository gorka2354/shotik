//! Timer, level meters, pause, stop, cancel.

use std::path::Path;

use serde_json::Value;
use shotik_core::ports::{RecLevels, RecWindows, Theme};
use shotik_core::{Error, Result};

pub fn show(_windows: &RecWindows) {}
pub fn update(_paused: bool, _levels: Option<RecLevels>) {}
pub fn close() {}
pub fn apply_theme(_theme: &Theme) {}
pub fn ui_state() -> Result<Value> {
    Err(Error::msg("not implemented yet"))
}
pub fn capture_page(_path: &Path) -> Result<(u32, u32)> {
    Err(Error::msg("not implemented yet"))
}
