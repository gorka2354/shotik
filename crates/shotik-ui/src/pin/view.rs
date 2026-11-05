use super::state::State;
use crate::{runtime, Pin};
use serde_json::{json, Value};
use shotik_core::{
    ports::{PinRequest, Theme},
    Display, Error, PointF, Rect, Result,
};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HWND, POINT},
        UI::WindowsAndMessaging::*,
    },
};

struct View {
    id: u64,
    state: State,
    file: PathBuf,
    display: Display,
    window: Pin,
}
#[derive(Default)]
struct Pins {
    sequence: u64,
    windows: Vec<View>,
}
thread_local! { static PINS: RefCell<Pins> = RefCell::default(); }

fn with_pin<T>(id: u64, work: impl FnOnce(&mut View) -> Result<T>) -> Result<T> {
    PINS.with_borrow_mut(|pins| {
        work(
            pins.windows
                .iter_mut()
                .find(|pin| pin.id == id)
                .ok_or_else(|| Error::msg("pin is closed"))?,
        )
    })
}
fn close_ui(id: u64) {
    PINS.with_borrow_mut(|pins| {
        if let Some(index) = pins.windows.iter().position(|pin| pin.id == id) {
            let pin = pins.windows.remove(index);
            pin.window.set_picture(slint::Image::default());
            runtime::report(
                pin.window
                    .hide()
                    .map_err(|e| Error::msg(format!("cannot hide pin: {e}"))),
            );
        }
    });
}
fn opacity(pin: &View) -> Result<()> {
    runtime::small_layered(
        pin.window.window(),
        (f64::from(pin.state.opacity) * 2.55).round() as u8,
        false,
    )
}
fn geometry(id: u64, zoom: Option<i32>) -> Result<()> {
    with_pin(id, |pin| {
        let (bounds, display) = runtime::small_geometry(pin.window.window(), &pin.display)?;
        pin.state.bounds = bounds;
        pin.display = display;
        if let Some(direction) = zoom {
            pin.state.zoom(direction);
        } else {
            pin.state.reset();
        }
        pin.window.set_percent((pin.state.zoom * 100.0).round() as i32);
        runtime::show_small(pin.window.window(), pin.state.bounds, &pin.display, true, false, false)?;
        opacity(pin)
    })
}

// A native Win32 menu, with no Slint imitation and no menu window in ghost tests.
fn menu(hwnd: HWND, percent: u8) -> Result<u32> {
    if runtime::env().ghost {
        return Err(Error::msg("native pin menus are disabled in ghost mode"));
    }
    // SAFETY: menu is local, owned by this function, and destroyed on every return path.
    let menu = unsafe { CreatePopupMenu() }.map_err(|e| Error::msg(format!("cannot create pin menu: {e}")))?;
    let result = (|| {
        let entries = [(1, "pinCopy"), (2, "pinSaveAs"), (3, "pinReveal")];
        for (id, key) in entries {
            let wide: Vec<u16> = runtime::tr(key).encode_utf16().chain(Some(0)).collect();
            // SAFETY: string is NUL-terminated and lives through AppendMenuW, which copies it.
            unsafe { AppendMenuW(menu, MF_STRING, id, PCWSTR(wide.as_ptr())) }
                .map_err(|e| Error::msg(format!("cannot add pin menu item: {e}")))?;
        }
        unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, None) }.map_err(|e| Error::msg(e.to_string()))?;
        for (index, op) in [100, 85, 70, 50].into_iter().enumerate() {
            let label = runtime::tr("pinOpacity").replace("{0}", &op.to_string());
            let wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            let flags = MF_STRING | if percent == op { MF_CHECKED } else { MF_UNCHECKED };
            unsafe { AppendMenuW(menu, flags, 10 + index, PCWSTR(wide.as_ptr())) }
                .map_err(|e| Error::msg(e.to_string()))?;
        }
        let selected = [100, 85, 70, 50]
            .iter()
            .position(|value| *value == percent)
            .unwrap_or(0) as u32
            + 10;
        unsafe { CheckMenuRadioItem(menu, 10, 13, selected, MF_BYCOMMAND.0) }.map_err(|e| Error::msg(e.to_string()))?;
        unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, None) }.map_err(|e| Error::msg(e.to_string()))?;
        for (id, key) in [(20, "pinReset"), (21, "pinClose")] {
            let wide: Vec<u16> = runtime::tr(key).encode_utf16().chain(Some(0)).collect();
            unsafe { AppendMenuW(menu, MF_STRING, id, PCWSTR(wide.as_ptr())) }
                .map_err(|e| Error::msg(e.to_string()))?;
        }
        let mut point = POINT::default();
        unsafe { GetCursorPos(&mut point) }.map_err(|e| Error::msg(format!("cannot position pin menu: {e}")))?;
        Ok(unsafe {
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                hwnd,
                None,
            )
        }
        .0 as u32)
    })();
    let destroyed = unsafe { DestroyMenu(menu) }.map_err(|e| Error::msg(format!("cannot destroy pin menu: {e}")));
    let selected = result?;
    destroyed?;
    Ok(selected)
}
fn context_menu(id: u64) -> Result<()> {
    let (hwnd, percent, file) = with_pin(id, |pin| {
        Ok((
            runtime::hwnd_of(pin.window.window())?,
            pin.state.opacity,
            pin.file.clone(),
        ))
    })?;
    match menu(hwnd, percent)? {
        0 => Ok(()),
        1 => runtime::host().pin_copy(&file),
        2 => runtime::host().pin_save_as(&file),
        3 => runtime::host().reveal_file(&file),
        item @ 10..=13 => with_pin(id, |pin| {
            pin.state.set_opacity([100, 85, 70, 50][(item - 10) as usize]);
            opacity(pin)
        }),
        20 => geometry(id, None),
        21 => {
            close_ui(id);
            Ok(())
        }
        _ => Err(Error::msg("unknown native pin command")),
    }
}

pub fn show(request: PinRequest) {
    runtime::on_ui(move || {
        runtime::report((|| {
            let (display, _) = runtime::small_screen(Some(PointF {
                x: f64::from(request.bounds.x),
                y: f64::from(request.bounds.y),
            }))?;
            let image = runtime::uncached_image(&request.file)?;
            let window = Pin::new().map_err(|e| Error::msg(format!("cannot create pin: {e}")))?;
            crate::apply_theme!(window, runtime::current_theme());
            window.set_picture(image);
            window.set_close_label(runtime::tr("pinClose"));
            let id = PINS.with_borrow_mut(|pins| {
                pins.sequence += 1;
                pins.sequence
            });
            window.on_dismiss(move || runtime::on_ui(move || close_ui(id)));
            window.on_reset(move || runtime::on_ui(move || runtime::report(geometry(id, None))));
            window.on_zoom(move |direction| runtime::on_ui(move || runtime::report(geometry(id, Some(direction)))));
            window.on_context_menu(move || runtime::on_ui(move || runtime::report(context_menu(id))));
            let weak = window.as_weak();
            window.on_drag(move || {
                if let Some(window) = weak.upgrade() {
                    runtime::report(runtime::drag_small(window.window()));
                }
            });
            window.window().on_close_requested(move || {
                runtime::on_ui(move || close_ui(id));
                slint::CloseRequestResponse::KeepWindowShown
            });
            let state = State::new(request.bounds);
            PINS.with_borrow_mut(|pins| {
                pins.windows.push(View {
                    id,
                    state,
                    file: request.file,
                    display,
                    window: window.clone_strong(),
                })
            });
            runtime::after_native(window, move |_| {
                with_pin(id, |pin| {
                    runtime::show_small(pin.window.window(), pin.state.bounds, &pin.display, true, false, false)?;
                    runtime::follow_suggested_dpi_rect(runtime::hwnd_of(pin.window.window())?)?;
                    runtime::activate_small(pin.window.window())?;
                    pin.state.ready = true;
                    Ok(())
                })
            });
            Ok(())
        })())
    });
}
pub fn close_all() {
    runtime::on_ui(|| {
        let ids = PINS.with_borrow(|pins| pins.windows.iter().map(|pin| pin.id).collect::<Vec<_>>());
        for id in ids {
            close_ui(id);
        }
    });
}
pub fn bounds() -> Vec<Rect> {
    runtime::on_ui_wait(|| {
        PINS.with_borrow(|pins| {
            pins.windows
                .iter()
                .map(|pin| match runtime::small_bounds(pin.window.window(), &pin.display) {
                    Ok(bounds) => runtime::ghost_bounds(bounds),
                    Err(error) => {
                        eprintln!("cannot query pin bounds: {error}");
                        runtime::ghost_bounds(pin.state.bounds)
                    }
                })
                .collect()
        })
    })
    .unwrap_or_else(|error| {
        eprintln!("cannot query pin bounds: {error}");
        Vec::new()
    })
}
pub fn apply_theme(theme: &Theme) {
    let theme = theme.clone();
    runtime::on_ui(move || {
        PINS.with_borrow(|pins| {
            for pin in &pins.windows {
                crate::apply_theme!(pin.window, theme);
            }
        })
    });
}
pub fn ui_state(index: usize) -> Result<Value> {
    runtime::on_ui_wait(move || {
        PINS.with_borrow(|pins| {
            let pin = pins
                .windows
                .get(index)
                .ok_or_else(|| Error::msg("pin index is out of range"))?;
            Ok(json!({"ready":pin.state.ready}))
        })
    })?
}
pub fn capture_page(index: usize, path: &Path) -> Result<(u32, u32)> {
    let path = path.to_owned();
    runtime::on_ui_wait(move || {
        PINS.with_borrow(|pins| {
            runtime::snapshot_png(
                pins.windows
                    .get(index)
                    .ok_or_else(|| Error::msg("pin index is out of range"))?
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
    index: usize,
    work: impl FnOnce(&Pin) -> Result<T> + Send + 'static,
) -> Result<T> {
    if !runtime::env().ghost {
        return Err(Error::msg("preview requires ghost mode"));
    }
    runtime::on_ui_wait(move || {
        PINS.with_borrow(|pins| {
            work(
                &pins
                    .windows
                    .get(index)
                    .ok_or_else(|| Error::msg("pin index is out of range"))?
                    .window,
            )
        })
    })?
}
