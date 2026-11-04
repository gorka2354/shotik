//! Domain types, port traits and pure logic of Shotik.
//!
//! Everything external (screen, clipboard, windows, OCR, the UI itself) sits behind a trait in
//! [`ports`]. This crate uses only the standard library's portable APIs and has no UI or async
//! runtime dependency, so it builds and tests on any platform.

pub mod error;
pub mod frame;
pub mod geom;
pub mod history;
pub mod ports;
pub mod settings;

pub use error::{Error, Result};
pub use frame::Frame;
pub use geom::{Display, DisplayId, Point, PointF, Rect};
pub use history::HistoryEntry;
pub use settings::Settings;
