use super::state::{self, State};
use crate::{runtime, TranslatePopup};
use serde_json::Value;
use shotik_core::{
    ports::{Theme, TranslatePopup as Content},
    Display, Error, Rect, Result,
};
use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;
use std::{cell::RefCell, path::Path, time::Duration};

struct View {
    state: State,
    window: Option<TranslatePopup>,
    display: Option<Display>,
    timer: slint::Timer,
}
thread_local! { static VIEW: RefCell<Option<View>> = const { RefCell::new(None) }; }

fn with_view<T>(work: impl FnOnce(&mut View) -> T) -> T {
    VIEW.with_borrow_mut(|view| {
        work(view.get_or_insert_with(|| View {
            state: State::new(runtime::env().translate_popup_ttl_ms),
            window: None,
            display: None,
            timer: slint::Timer::default(),
        }))
    })
}
impl View {
    fn retire(&mut self) {
        self.timer.stop();
        if let Some(window) = self.window.take() {
            runtime::report(
                window
                    .hide()
                    .map_err(|e| Error::msg(format!("cannot hide translation popup: {e}"))),
            );
        }
    }
    fn arm(&self) {
        self.timer.stop();
        if let Some(deadline) = self.state.deadline.filter(|_| self.state.visible) {
            self.timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(deadline.saturating_sub(runtime::now_ms())),
                || {
                    with_view(|view| {
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
    fn paint(&mut self, preserve_position: bool) -> Result<()> {
        let Some(window) = &self.window else {
            return Ok(());
        };
        if preserve_position && window.window().is_visible() {
            let (bounds, display) =
                runtime::small_geometry(window.window(), self.display.as_ref().expect("popup display"))?;
            self.state.bounds.x = bounds.x;
            self.state.bounds.y = bounds.y;
            self.display = Some(display);
        }
        window.set_original(self.state.content.original.as_str().into());
        window.set_translated(self.state.displayed.as_str().into());
        window.set_loading(self.state.content.loading);
        window.set_meta(self.state.meta().as_str().into());
        window.set_heading(runtime::tr("txtTranslation"));
        window.set_copy_label(runtime::tr("txtCopyTranslation"));
        self.state.resize(f64::from(window.get_content_height()));
        if let Some(display) = self.display.as_ref().filter(|_| window.window().has_winit_window()) {
            runtime::show_small(window.window(), self.state.bounds, display, true, false, false)?;
        }
        Ok(())
    }
    fn show(&mut self, content: Content) -> Result<()> {
        let (display, at) = runtime::small_screen(None)?;
        self.state.bounds = state::placement(at, display.work_area);
        self.display = Some(display);
        if self.window.is_none() {
            let window =
                TranslatePopup::new().map_err(|e| Error::msg(format!("cannot create translation popup: {e}")))?;
            crate::apply_theme!(window, runtime::current_theme());
            window.on_dismiss(|| runtime::on_ui(hide_ui));
            let weak = window.as_weak();
            window.on_hover(move |over| {
                let weak = weak.clone();
                runtime::on_ui(move || {
                    if let Some(window) = weak.upgrade() {
                        let current =
                            with_view(|view| view.window.as_ref().is_some_and(|w| runtime::same_window(w, &window)));
                        if current {
                            hover_ui(over);
                        }
                    }
                });
            });
            window.on_copy(|| {
                runtime::on_ui(|| {
                    let text = with_view(|view| view.state.copy_text.clone());
                    if !text.is_empty() {
                        if let Err(error) = runtime::host().copy_text(&text) {
                            eprintln!("cannot copy translation: {error}");
                            return;
                        }
                    }
                    hide_ui();
                })
            });
            window.on_resize(|_| {
                runtime::on_ui(move || {
                    runtime::report(with_view(|view| {
                        if !view.window.as_ref().is_some_and(|w| w.window().has_winit_window()) || !view.state.visible {
                            return Ok(());
                        }
                        // Read the latest content, not a queued height from an older payload/window.
                        let height = view.window.as_ref().expect("checked above").get_content_height();
                        let new = (height.round() as i32).clamp(120, 520);
                        if new == view.state.bounds.height {
                            return Ok(());
                        }
                        let window = view.window.as_ref().expect("checked above");
                        if window.window().is_visible() {
                            let (bounds, display) = runtime::small_geometry(
                                window.window(),
                                view.display.as_ref().expect("popup display"),
                            )?;
                            view.state.bounds.x = bounds.x;
                            view.state.bounds.y = bounds.y;
                            view.display = Some(display);
                        }
                        view.state.resize(f64::from(height));
                        runtime::show_small(
                            window.window(),
                            view.state.bounds,
                            view.display.as_ref().expect("popup display"),
                            true,
                            false,
                            false,
                        )
                    }))
                })
            });
            let weak = window.as_weak();
            window.on_drag(move || {
                if let Some(window) = weak.upgrade() {
                    runtime::report(runtime::drag_small(window.window()));
                }
            });
            window.window().on_close_requested(|| {
                runtime::on_ui(hide_ui);
                slint::CloseRequestResponse::KeepWindowShown
            });
            self.window = Some(window.clone_strong());
            runtime::after_native(window, |window| {
                with_view(|view| {
                    if view.state.visible && view.window.as_ref().is_some_and(|w| runtime::same_window(w, &window)) {
                        view.paint(false)?;
                        runtime::activate_small(window.window())?;
                        view.arm();
                    }
                    Ok(())
                })
            });
        }
        self.state
            .show(content, runtime::tr("txtTranslating").as_str(), runtime::now_ms());
        self.paint(false)?;
        if let Some(window) = self.window.as_ref().filter(|window| window.window().is_visible()) {
            runtime::activate_small(window.window())?;
        }
        self.arm();
        Ok(())
    }
}
fn hide_ui() {
    with_view(|view| {
        view.state.hide();
        view.retire();
    });
}
fn hover_ui(over: bool) {
    with_view(|view| {
        view.state.hover(over, runtime::now_ms());
        view.arm();
    });
}
pub fn show(content: Content) {
    runtime::on_ui(move || runtime::report(with_view(|view| view.show(content))));
}
pub fn update(content: Content) {
    runtime::on_ui(move || {
        runtime::report(with_view(|view| {
            view.state
                .update(content, runtime::tr("txtTranslating").as_str(), runtime::now_ms());
            view.paint(true)?;
            view.arm();
            Ok(())
        }))
    });
}
pub fn hide() {
    runtime::on_ui(hide_ui);
}
pub fn visible() -> bool {
    runtime::on_ui_wait(|| with_view(|view| view.window.as_ref().is_some_and(|w| w.window().is_visible())))
        .unwrap_or_else(|error| {
            eprintln!("cannot query popup visibility: {error}");
            false
        })
}
pub fn bounds() -> Option<Rect> {
    runtime::on_ui_wait(|| {
        with_view(|view| {
            view.window
                .as_ref()
                .filter(|w| w.window().is_visible())
                .and_then(|window| {
                    match runtime::small_bounds(window.window(), view.display.as_ref().expect("popup display")) {
                        Ok(bounds) => Some(runtime::ghost_bounds(bounds)),
                        Err(error) => {
                            eprintln!("cannot query popup bounds: {error}");
                            None
                        }
                    }
                })
        })
    })
    .unwrap_or_else(|error| {
        eprintln!("cannot query popup bounds: {error}");
        None
    })
}
pub fn apply_theme(theme: &Theme) {
    let theme = theme.clone();
    runtime::on_ui(move || {
        with_view(|view| {
            if let Some(window) = &view.window {
                crate::apply_theme!(window, theme);
            }
        })
    });
}
pub fn ui_state() -> Result<Value> {
    runtime::on_ui_wait(|| with_view(|view| view.state.snapshot()))
}
pub fn ui_action(action: Value) -> Result<Value> {
    let over = crate::toast::state::hover_action(&action)?;
    runtime::on_ui_wait(move || {
        hover_ui(over);
        with_view(|view| view.state.snapshot())
    })
}
pub fn capture_page(path: &Path) -> Result<(u32, u32)> {
    let path = path.to_owned();
    runtime::on_ui_wait(move || {
        with_view(|view| {
            runtime::snapshot_png(
                view.window
                    .as_ref()
                    .ok_or_else(|| Error::msg("translation popup is not open"))?
                    .window(),
                &path,
            )
        })
    })?
}

/// Local input/render probe for the ghost preview; never generates OS input.
#[doc(hidden)]
pub fn with_preview_window<T: Send + 'static>(
    work: impl FnOnce(&TranslatePopup) -> Result<T> + Send + 'static,
) -> Result<T> {
    if !runtime::env().ghost {
        return Err(Error::msg("preview requires ghost mode"));
    }
    runtime::on_ui_wait(move || {
        with_view(|view| work(view.window.as_ref().ok_or_else(|| Error::msg("popup is not open"))?))
    })?
}
