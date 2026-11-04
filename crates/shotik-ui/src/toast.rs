//! Notification with thumbnail.

use std::path::Path;

use serde_json::Value;
use shotik_core::ports::{Theme, Toast};
use shotik_core::{Error, Result};

pub fn show(_toast: Toast) {}
pub fn visible() -> bool {
    false
}
pub fn last() -> Option<Toast> {
    None
}
pub fn apply_theme(_theme: &Theme) {}
pub fn ui_state() -> Result<Value> {
    Err(Error::msg("not implemented yet"))
}
pub fn ui_action(_action: Value) -> Result<Value> {
    Err(Error::msg("not implemented yet"))
}
pub fn capture_page(_path: &Path) -> Result<(u32, u32)> {
    Err(Error::msg("not implemented yet"))
}
