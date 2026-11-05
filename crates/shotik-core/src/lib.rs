//! Domain types, port traits and pure logic of Shotik.
//!
//! Everything external (screen, clipboard, windows, OCR, the UI itself) sits behind a trait in
//! [`ports`]. This crate uses only the standard library's portable APIs and has no UI or async
//! runtime dependency, so it builds and tests on any platform.

pub mod accelerator;
pub mod error;
pub mod format;
pub mod frame;
pub mod geom;
pub mod highlight;
pub mod history;
pub mod i18n;
pub mod ocr_select;
pub mod ports;
pub mod settings;
pub mod translate;
pub mod update;

mod storage;
mod text;

#[cfg(test)]
mod test_support;

pub use error::{Error, Result};
pub use frame::Frame;
pub use geom::{Display, DisplayId, Point, PointF, Rect};
pub use history::HistoryEntry;
pub use settings::Settings;
