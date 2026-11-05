use super::state::State;
use crate::{runtime, RecBorder};
use shotik_core::{ports::RecWindows, Error, Result};
use slint::ComponentHandle;
use std::{cell::RefCell, path::Path, time::Duration};

struct View {
    window: RecBorder,
    timer: slint::Timer,
}
thread_local! { static VIEW: RefCell<Option<View>> = const { RefCell::new(None) }; }
fn close_ui() {
    VIEW.with_borrow_mut(|view| {
        if let Some(view) = view.take() {
            view.timer.stop();
            runtime::report(
                view.window
                    .hide()
                    .map_err(|e| Error::msg(format!("cannot hide recording border: {e}"))),
            );
        }
    });
}
pub fn show(windows: &RecWindows) {
    let windows = windows.clone();
    runtime::on_ui(move || {
        runtime::report((|| {
            close_ui();
            let Some(region) = windows.rect_dip else {
                return Ok(());
            };
            let state = State::new(windows.display.bounds, region);
            let window = RecBorder::new().map_err(|e| Error::msg(format!("cannot create recording border: {e}")))?;
            let timer = slint::Timer::default();
            let weak = window.as_weak();
            let started = runtime::now_ms();
            timer.start(slint::TimerMode::Repeated, Duration::from_millis(40), move || {
                if let Some(window) = weak.upgrade() {
                    window.set_pulse(State::opacity(runtime::now_ms() - started));
                }
            });
            VIEW.with_borrow_mut(|view| {
                *view = Some(View {
                    window: window.clone_strong(),
                    timer,
                })
            });
            runtime::after_native(window, move |window| {
                VIEW.with_borrow(|view| {
                    if view.as_ref().is_some_and(|v| runtime::same_window(&v.window, &window)) {
                        runtime::prepare_small(window.window(), state.bounds, &windows.display, false, true, true)?;
                        runtime::small_region(
                            window.window(),
                            runtime::SmallRegion::Frame {
                                thickness: state.thickness,
                            },
                            windows.display.scale_factor,
                        )?;
                        runtime::exclude_capture(window.window())?;
                        window
                            .show()
                            .map_err(|e| Error::msg(format!("cannot show recording border: {e}")))?;
                    }
                    Ok(())
                })
            });
            Ok(())
        })())
    });
}
pub fn close() {
    runtime::on_ui(close_ui);
}
pub fn capture_page(path: &Path) -> Result<(u32, u32)> {
    let path = path.to_owned();
    runtime::on_ui_wait(move || {
        VIEW.with_borrow(|view| {
            runtime::snapshot_png(
                view.as_ref()
                    .ok_or_else(|| Error::msg("recording border is not open"))?
                    .window
                    .window(),
                &path,
            )
        })
    })?
}
