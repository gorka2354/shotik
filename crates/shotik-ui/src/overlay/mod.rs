//! Freeze frame, selection, annotations, toolbar, Live Text.

pub mod keys;
pub mod layer;
pub mod pixels;
pub mod raster;
pub mod state;

use std::path::Path;

use serde_json::Value;
use shotik_core::ports::{MarkSink, Once, OverlayMode, OverlayOutcome, Theme};
use shotik_core::{Display, Error, Frame, Rect, Result};

pub fn begin(_mode: OverlayMode, _marks: MarkSink, _done: Once<OverlayOutcome>) -> Result<()> {
    Err(Error::msg("not implemented yet"))
}
pub fn add_display(_display: Display, _frame: Frame, _focused: bool) -> Result<()> {
    Err(Error::msg("not implemented yet"))
}
pub fn set_window_rects(_rects: Vec<Rect>) {}
pub fn cancel() {}
pub fn is_open() -> bool {
    false
}
pub fn bounds() -> Vec<Rect> {
    Vec::new()
}
pub fn apply_theme(_theme: &Theme) {}
pub fn ui_state(_index: usize) -> Result<Value> {
    Err(Error::msg("not implemented yet"))
}
pub fn ui_action(_index: usize, _action: Value) -> Result<Value> {
    Err(Error::msg("not implemented yet"))
}
pub fn capture_page(_index: usize, _path: &Path) -> Result<(u32, u32)> {
    Err(Error::msg("not implemented yet"))
}
