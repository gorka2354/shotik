use super::state::State;
use crate::{runtime, Countdown};
use shotik_core::{ports::Once, Display, Error, Rect, Result};
use slint::ComponentHandle;
use std::{cell::RefCell, path::Path, time::Duration};

struct View {
    state: State,
    window: Countdown,
    done: Option<Once<()>>,
    timer: slint::Timer,
}
thread_local! { static VIEW: RefCell<Option<View>> = const { RefCell::new(None) }; }

fn advance() {
    let done = VIEW.with_borrow_mut(|slot| {
        let view = slot.as_mut()?;
        let now = runtime::now_ms();
        view.window.set_number(i32::from(view.state.number(now)));
        if view.state.tick(now) {
            let mut view = slot.take().expect("countdown exists");
            view.timer.stop();
            runtime::report(
                view.window
                    .hide()
                    .map_err(|e| Error::msg(format!("cannot hide countdown: {e}"))),
            );
            view.done.take()
        } else {
            let next = view
                .state
                .next_deadline(now)
                .expect("unfinished countdown has a wake deadline");
            view.timer
                .start(slint::TimerMode::SingleShot, Duration::from_millis(next - now), advance);
            None
        }
    });
    // Callback can start other windows; no RefCell borrow may cross it.
    if let Some(done) = done {
        done(());
    }
}

pub fn show(display: Display, rect_dip: Rect, done: Once<()>) {
    runtime::on_ui(move || {
        runtime::report((|| {
            if VIEW.with_borrow(|view| view.is_some()) {
                return Err(Error::msg("countdown is already open"));
            }
            let window = Countdown::new().map_err(|e| Error::msg(format!("cannot create countdown: {e}")))?;
            let timer = slint::Timer::default();
            VIEW.with_borrow_mut(|view| {
                *view = Some(View {
                    state: State::new(runtime::now_ms()),
                    window: window.clone_strong(),
                    done: Some(done),
                    timer,
                })
            });
            runtime::after_native(window, move |window| {
                runtime::show_small(
                    window.window(),
                    State::placement(display.bounds, rect_dip),
                    &display,
                    true,
                    true,
                    true,
                )?;
                VIEW.with_borrow_mut(|view| {
                    if let Some(view) = view {
                        let now = runtime::now_ms();
                        view.state = State::new(now);
                        let delay = view.state.next_deadline(now).expect("new countdown deadline") - now;
                        view.timer
                            .start(slint::TimerMode::SingleShot, Duration::from_millis(delay), advance);
                    }
                });
                Ok(())
            });
            Ok(())
        })())
    });
}

/// Off-screen reference/preview support, through the same renderer as the other windows.
pub fn capture_page(path: &Path) -> Result<(u32, u32)> {
    let path = path.to_owned();
    runtime::on_ui_wait(move || {
        VIEW.with_borrow(|view| {
            runtime::snapshot_png(
                view.as_ref()
                    .ok_or_else(|| Error::msg("countdown is not open"))?
                    .window
                    .window(),
                &path,
            )
        })
    })?
}
