//! Main window: pure state and a UI-thread-only Slint adapter.
//!
//! R7 acceptance: the reducer tests cover the main test protocol; `main_preview` exercises
//! the real view/host boundary and captures all four pages in both themes in ghost mode.
//! Native dragging, resizing and Snap Layouts require the separate live desktop check.

#[doc(hidden)]
pub mod fixtures;
pub mod state;
mod view;

pub use view::{
    apply_theme, capture_page, hide, rebuild_language, send, show, ui_action, ui_state, visible,
};
