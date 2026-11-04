use super::state::{meter_fraction, Diagnostics, State};
use crate::{runtime, RecControls};
use serde_json::{json, Value};
use shotik_core::{
    ports::{RecLevels, RecWindows, Theme},
    Error, Result,
};
use slint::ComponentHandle;
use std::{cell::RefCell, path::Path, time::Duration};

struct View {
    state: State,
    window: RecControls,
    timer: slint::Timer,
}
thread_local! {
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
    static DIAGNOSTICS: RefCell<Diagnostics> = RefCell::default();
}
impl View {
    fn paint(&self) {
        self.window.set_time(self.state.time().as_str().into());
        self.window.set_paused(self.state.paused);
        self.window.set_dot_opacity(self.state.dot_opacity(runtime::now_ms()));
        self.window
            .set_system_fraction(meter_fraction(self.state.levels.system));
        self.window
            .set_microphone_fraction(meter_fraction(self.state.levels.microphone));
        self.window.set_system_clip(self.state.levels.system >= 0.98);
        self.window.set_microphone_clip(self.state.levels.microphone >= 0.98);
    }
}
fn close_ui() {
    VIEW.with_borrow_mut(|slot| {
        if let Some(view) = slot.take() {
            view.timer.stop();
            runtime::report(
                view.window
                    .hide()
                    .map_err(|e| Error::msg(format!("cannot hide recording controls: {e}"))),
            );
        }
    });
}
pub fn show(windows: &RecWindows) {
    let windows = windows.clone();
    runtime::on_ui(move || {
        runtime::report((|| {
            close_ui();
            let state = State::new(&windows, runtime::now_ms());
            let window =
                RecControls::new().map_err(|e| Error::msg(format!("cannot create recording controls: {e}")))?;
            crate::apply_theme!(window, runtime::current_theme());
            window.set_system_enabled(state.inputs.iter().any(|s| s == "system"));
            window.set_microphone_enabled(state.inputs.iter().any(|s| s == "microphone"));
            window.on_stop(|| runtime::host().rec_stop());
            window.on_pause(|| runtime::host().rec_pause_toggle());
            window.on_cancel(|| runtime::host().rec_cancel());
            let weak = window.as_weak();
            window.on_drag(move || {
                if let Some(window) = weak.upgrade() {
                    runtime::report(runtime::drag_small(window.window()));
                }
            });
            let timer = slint::Timer::default();
            timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), || {
                VIEW.with_borrow_mut(|view| {
                    if let Some(view) = view {
                        view.state.tick(runtime::now_ms());
                        view.paint();
                    }
                })
            });
            let view = View {
                state,
                window: window.clone_strong(),
                timer,
            };
            view.paint();
            VIEW.with_borrow_mut(|slot| *slot = Some(view));
            runtime::after_native(window, move |window| {
                VIEW.with_borrow(|view| {
                    if let Some(view) = view.as_ref().filter(|v| runtime::same_window(&v.window, &window)) {
                        runtime::show_small(window.window(), view.state.bounds, &windows.display, true, false, false)?;
                        runtime::activate_small(window.window())?;
                    }
                    Ok(())
                })
            });
            Ok(())
        })())
    });
}
pub fn update(paused: bool, levels: Option<RecLevels>) {
    runtime::on_ui(move || {
        VIEW.with_borrow_mut(|view| {
            if let Some(view) = view {
                view.state.update(paused, levels, runtime::now_ms());
                view.paint();
            }
        })
    });
}
pub fn close() {
    runtime::on_ui(close_ui);
}
pub fn apply_theme(theme: &Theme) {
    let theme = theme.clone();
    runtime::on_ui(move || {
        VIEW.with_borrow(|view| {
            if let Some(view) = view {
                crate::apply_theme!(view.window, theme);
            }
        })
    });
}
/// Application integration point. Feed facts from encoder/source startup and successful finalization;
/// nothing in the UI infers codec/tracks/index success from a label or synthetic default.
pub fn set_diagnostics(diagnostics: Diagnostics) {
    runtime::on_ui(move || DIAGNOSTICS.with_borrow_mut(|value| *value = diagnostics));
}
pub fn ui_state() -> Result<Value> {
    runtime::on_ui_wait(|| {
        let meters = VIEW.with_borrow(|view| {
            view.as_ref()
                .filter(|view| view.window.window().is_visible())
                .map(|view| {
                    view.state
                        .inputs
                        .iter()
                        .map(|input| {
                            let (track, fill) = if input == "system" {
                                (view.window.get_system_track(), view.window.get_system_fill())
                            } else {
                                (view.window.get_microphone_track(), view.window.get_microphone_fill())
                            };
                            json!({"input":input,"track":track,"fill":fill})
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        });
        DIAGNOSTICS.with_borrow(|diagnostics| json!({"meters":meters,"diagnostics":diagnostics}))
    })
}
pub fn capture_page(path: &Path) -> Result<(u32, u32)> {
    let path = path.to_owned();
    runtime::on_ui_wait(move || {
        VIEW.with_borrow(|view| {
            runtime::snapshot_png(
                view.as_ref()
                    .ok_or_else(|| Error::msg("recording controls are not open"))?
                    .window
                    .window(),
                &path,
            )
        })
    })?
}

/// Local input/render probe for the ghost preview; never generates OS input.
#[doc(hidden)]
pub fn with_preview_window<T: Send + 'static>(
    work: impl FnOnce(&RecControls) -> Result<T> + Send + 'static,
) -> Result<T> {
    if !runtime::env().ghost {
        return Err(Error::msg("preview requires ghost mode"));
    }
    runtime::on_ui_wait(move || {
        VIEW.with_borrow(|view| work(&view.as_ref().ok_or_else(|| Error::msg("controls are not open"))?.window))
    })?
}
