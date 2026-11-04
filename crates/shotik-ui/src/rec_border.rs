//! Frame around the recorded region.

use std::path::Path;

use shotik_core::ports::RecWindows;
use shotik_core::{Error, Result};

pub fn show(_windows: &RecWindows) {}
pub fn close() {}
pub fn capture_page(_path: &Path) -> Result<(u32, u32)> {
    Err(Error::msg("not implemented yet"))
}
