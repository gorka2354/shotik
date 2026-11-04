//! 3-2-1 before recording.

use shotik_core::ports::Once;
use shotik_core::{Display, Rect};

/// Fires `done` when the countdown has finished.
pub fn show(_display: Display, _rect_dip: Rect, done: Once<()>) {
    done(());
}
