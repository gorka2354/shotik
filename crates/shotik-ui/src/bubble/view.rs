use super::state::State;
use crate::{runtime, Bubble};
use serde_json::{json, Value};
use shotik_core::{ports::Theme, Error, PointF, Rect, Result};
use slint::ComponentHandle;
use std::{cell::RefCell, path::Path, time::Duration};

#[derive(Default)]
struct View {
    state: State,
    window: Option<Bubble>,
    timer: slint::Timer,
}
thread_local! { static VIEW: RefCell<View> = RefCell::default(); }
impl View {
    fn retire(&mut self) {
        self.timer.stop();
        if let Some(window) = self.window.take() {
            runtime::report(
                window
                    .hide()
                    .map_err(|e| Error::msg(format!("cannot hide bubble: {e}"))),
            );
        }
    }
    fn arm(&self) {
        self.timer.stop();
        if let Some(deadline) = self.state.deadline {
            self.timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(deadline.saturating_sub(runtime::now_ms())),
                || {
                    VIEW.with_borrow_mut(|view| {
                        view.state.tick(runtime::now_ms());
                        if view.state.visible {
                            view.arm();
                        } else {
                            view.retire();
                        }
                    });
                },
            );
        }
    }
    fn show(&mut self, at: PointF) -> Result<()> {
        let (display, _) = runtime::small_screen(Some(at))?;
        let window = Bubble::new().map_err(|e| Error::msg(format!("cannot create bubble: {e}")))?;
        crate::apply_theme!(window, runtime::current_theme());
        window.on_keepalive(|| {
            runtime::on_ui(|| {
                VIEW.with_borrow_mut(|view| {
                    if view.state.visible {
                        view.state.keepalive(runtime::now_ms());
                        view.arm();
                    }
                })
            })
        });
        window.on_activate(|| runtime::on_ui(|| runtime::host().bubble_activated()));
        window.window().on_close_requested(|| {
            runtime::on_ui(hide_ui);
            slint::CloseRequestResponse::KeepWindowShown
        });
        self.state.show(at, display.work_area, runtime::now_ms());
        self.retire();
        self.window = Some(window.clone_strong());
        self.arm();
        runtime::after_native(window, move |window| {
            VIEW.with_borrow(|view| {
                if view.state.visible && view.window.as_ref().is_some_and(|w| runtime::same_window(w, &window)) {
                    runtime::prepare_small(window.window(), view.state.bounds, &display, false, false, false)?;
                    runtime::small_region(window.window(), runtime::SmallRegion::Bubble, display.scale_factor)?;
                    window
                        .show()
                        .map_err(|e| Error::msg(format!("cannot show bubble: {e}")))?;
                }
                Ok(())
            })
        });
        Ok(())
    }
}
fn hide_ui() {
    VIEW.with_borrow_mut(|view| {
        view.state.hide();
        view.retire();
    });
}
pub fn show(at: PointF) {
    runtime::on_ui(move || runtime::report(VIEW.with_borrow_mut(|view| view.show(at))));
}
pub fn hide() {
    runtime::on_ui(hide_ui);
}
pub fn visible() -> bool {
    runtime::on_ui_wait(|| VIEW.with_borrow(|view| view.window.as_ref().is_some_and(|w| w.window().is_visible())))
        .unwrap_or_else(|error| {
            eprintln!("cannot query bubble visibility: {error}");
            false
        })
}
pub fn bounds() -> Option<Rect> {
    runtime::on_ui_wait(|| {
        VIEW.with_borrow(|view| {
            view.window
                .as_ref()
                .filter(|w| w.window().is_visible())
                .map(|_| runtime::ghost_bounds(view.state.bounds))
        })
    })
    .unwrap_or_else(|error| {
        eprintln!("cannot query bubble bounds: {error}");
        None
    })
}
pub fn apply_theme(theme: &Theme) {
    let theme = theme.clone();
    runtime::on_ui(move || {
        VIEW.with_borrow(|view| {
            if let Some(window) = &view.window {
                crate::apply_theme!(window, theme);
            }
        })
    });
}
pub fn ui_state() -> Result<Value> {
    runtime::on_ui_wait(|| VIEW.with_borrow(|view| json!({"bubble":view.state.visible})))
}
pub fn capture_page(path: &Path) -> Result<(u32, u32)> {
    let path = path.to_owned();
    runtime::on_ui_wait(move || {
        VIEW.with_borrow(|view| {
            runtime::snapshot_png(
                view.window
                    .as_ref()
                    .ok_or_else(|| Error::msg("bubble is not open"))?
                    .window(),
                &path,
            )
        })
    })?
}

/// Local input/render probe for the ghost preview; never generates OS input.
#[doc(hidden)]
pub fn with_preview_window<T: Send + 'static>(work: impl FnOnce(&Bubble) -> Result<T> + Send + 'static) -> Result<T> {
    if !runtime::env().ghost {
        return Err(Error::msg("preview requires ghost mode"));
    }
    runtime::on_ui_wait(move || {
        VIEW.with_borrow(|view| work(view.window.as_ref().ok_or_else(|| Error::msg("bubble is not open"))?))
    })?
}
