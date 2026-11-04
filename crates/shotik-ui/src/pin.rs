//! Pinned screenshot.

use std::path::Path;

use serde_json::Value;
use shotik_core::ports::{PinRequest, Theme};
use shotik_core::{Error, Rect, Result};

pub fn show(_request: PinRequest) {}
pub fn bounds() -> Vec<Rect> {
    Vec::new()
}
pub fn apply_theme(_theme: &Theme) {}
pub fn ui_state(_index: usize) -> Result<Value> {
    Err(Error::msg("not implemented yet"))
}
pub fn capture_page(_index: usize, _path: &Path) -> Result<(u32, u32)> {
    Err(Error::msg("not implemented yet"))
}
