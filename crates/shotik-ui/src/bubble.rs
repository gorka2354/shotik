//! Button next to a text selection.

use std::path::Path;

use serde_json::Value;
use shotik_core::ports::Theme;
use shotik_core::{Error, PointF, Rect, Result};

pub fn show(_at: PointF) {}
pub fn hide() {}
pub fn visible() -> bool {
    false
}
pub fn bounds() -> Option<Rect> {
    None
}
pub fn apply_theme(_theme: &Theme) {}
pub fn ui_state() -> Result<Value> {
    Err(Error::msg("not implemented yet"))
}
pub fn capture_page(_path: &Path) -> Result<(u32, u32)> {
    Err(Error::msg("not implemented yet"))
}
