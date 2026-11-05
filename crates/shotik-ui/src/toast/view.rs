use super::state::{self, State};
use crate::{runtime, Toast};
use serde_json::{json, Value};
use shotik_core::{
    ports::{Theme, Toast as Content},
    Display, Error, Result,
};
use slint::ComponentHandle;
use std::{cell::RefCell, path::Path, time::Duration};

#[derive(Default)]
struct View {
    state: State,
    window: Option<Toast>,
    timer: slint::Timer,
}
thread_local! { static VIEW: RefCell<View> = RefCell::default(); }

impl View {
    fn retire(&mut self) {
        self.timer.stop();
        if let Some(window) = self.window.take() {
            window.set_thumbnail(slint::Image::default());
            runtime::report(window.hide().map_err(|e| Error::msg(format!("cannot hide toast: {e}"))));
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
    fn show(&mut self, content: Content, display: Display) -> Result<()> {
        let window = if let Some(window) = self.window.as_ref() {
            window.clone_strong()
        } else {
            let window = Toast::new().map_err(|e| Error::msg(format!("cannot create toast: {e}")))?;
            window.on_dismiss(|| runtime::on_ui(hide_ui));
            window.on_open_file(|| {
                runtime::on_ui(|| {
                    let file = VIEW.with_borrow(|view| view.state.last.as_ref().and_then(|toast| toast.file.clone()));
                    hide_ui();
                    if let Some(file) = file.filter(|path| path.exists()) {
                        runtime::report(runtime::host().reveal_file(&file));
                    }
                })
            });
            let weak = window.as_weak();
            window.on_hover(move |over| {
                let weak = weak.clone();
                runtime::on_ui(move || {
                    if let Some(window) = weak.upgrade() {
                        let current = VIEW
                            .with_borrow(|view| view.window.as_ref().is_some_and(|w| runtime::same_window(w, &window)));
                        if current {
                            hover_ui(over);
                        }
                    }
                });
            });
            window.window().on_close_requested(|| {
                runtime::on_ui(hide_ui);
                slint::CloseRequestResponse::KeepWindowShown
            });
            window
        };
        crate::apply_theme!(window, runtime::current_theme());
        window.set_heading(content.title.as_str().into());
        window.set_message(content.body.as_str().into());
        let thumbnail = content.thumb.as_deref().map(runtime::uncached_image).transpose()?;
        window.set_has_thumbnail(thumbnail.is_some());
        window.set_thumbnail(thumbnail.unwrap_or_default());
        window.set_swatch(if content.kind == "color" {
            content
                .color
                .as_deref()
                .map(runtime::color_of)
                .unwrap_or(runtime::color_of("#262a36"))
        } else {
            runtime::color_of("#262a36")
        });
        window.set_glyph(
            if content.thumb.is_some() || content.kind == "color" && content.color.is_some() {
                ""
            } else {
                match content.kind.as_str() {
                    "text" => "⎘",
                    "claude" => "✦",
                    "repeat" => "⟳",
                    "video" => "⏺",
                    "audio" => "♪",
                    "color" => "◉",
                    _ => "✓",
                }
            }
            .into(),
        );
        self.window = Some(window.clone_strong());
        self.state.show(content, runtime::now_ms());
        self.arm();
        runtime::after_native(window, move |window| {
            VIEW.with_borrow(|view| {
                if view.state.visible && view.window.as_ref().is_some_and(|w| runtime::same_window(w, &window)) {
                    runtime::show_small(
                        window.window(),
                        state::placement(display.work_area),
                        &display,
                        true,
                        true,
                        false,
                    )?;
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
fn hover_ui(over: bool) {
    VIEW.with_borrow_mut(|view| {
        view.state.hover(over, runtime::now_ms());
        view.arm();
    });
}

pub fn show(content: Content) {
    runtime::on_ui(move || {
        runtime::report((|| {
            let (display, _) = runtime::small_screen(None)?;
            VIEW.with_borrow_mut(|view| view.show(content, display))
        })())
    });
}
pub fn hide() {
    runtime::on_ui(hide_ui);
}
pub fn visible() -> bool {
    runtime::on_ui_wait(|| VIEW.with_borrow(|view| view.window.as_ref().is_some_and(|w| w.window().is_visible())))
        .unwrap_or_else(|error| {
            eprintln!("cannot query toast visibility: {error}");
            false
        })
}
pub fn last() -> Option<Content> {
    runtime::on_ui_wait(|| VIEW.with_borrow(|view| view.state.last.clone())).unwrap_or_else(|error| {
        eprintln!("cannot query last toast: {error}");
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
    runtime::on_ui_wait(|| json!({"visible":VIEW.with_borrow(|view| view.state.visible)}))
}
pub fn ui_action(action: Value) -> Result<Value> {
    let over = super::state::hover_action(&action)?;
    runtime::on_ui_wait(move || {
        hover_ui(over);
        json!({"visible":VIEW.with_borrow(|view| view.state.visible)})
    })
}
pub fn capture_page(path: &Path) -> Result<(u32, u32)> {
    let path = path.to_owned();
    runtime::on_ui_wait(move || {
        VIEW.with_borrow(|view| {
            runtime::snapshot_png(
                view.window
                    .as_ref()
                    .ok_or_else(|| Error::msg("toast is not open"))?
                    .window(),
                &path,
            )
        })
    })?
}

/// Local input/render probe for the ghost preview; never generates OS input.
#[doc(hidden)]
pub fn with_preview_window<T: Send + 'static>(work: impl FnOnce(&Toast) -> Result<T> + Send + 'static) -> Result<T> {
    if !runtime::env().ghost {
        return Err(Error::msg("preview requires ghost mode"));
    }
    runtime::on_ui_wait(move || {
        VIEW.with_borrow(|view| work(view.window.as_ref().ok_or_else(|| Error::msg("toast is not open"))?))
    })?
}
