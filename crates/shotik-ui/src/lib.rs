//! All windows of Shotik, on Slint's software renderer.
//!
//! Each window is a module: pure state with unit tests, plus a view holding as little logic as it can
//! (docs/rust/ARCHITECTURE.md). [`Ui`] implements `UiPort` by forwarding to those modules, and this file
//! rarely changes, so tasks working on different windows do not meet here.

pub mod bubble;
pub mod countdown;
pub mod main_window;
pub mod overlay;
pub mod pin;
pub mod rec_border;
pub mod rec_controls;
pub mod runtime;
pub mod testing;
pub mod toast;
pub mod translate_popup;

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use shotik_core::ports::{
    AppHost, MainEvent, MarkSink, Once, OverlayMode, OverlayOutcome, PinRequest, RecLevels, RecWindows, Theme as ThemeData,
    Toast as ToastData, TranslatePopup as PopupContent, UiPort,
};
use shotik_core::{Display, Error, Frame, PointF, Rect, Result};

pub use runtime::UiEnv;

slint::include_modules!();

/// The UI: create on the main thread, hand to the application, then call [`Ui::run`].
pub struct Ui;

impl Ui {
    /// Selects the backend. Software rendering on purpose: no GPU device, so a resident app holds no
    /// GPU memory, does not compete with a game, and cannot hit the driver's leak on device release.
    pub fn new(env: UiEnv) -> Result<Self> {
        runtime::init(env)?;
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .with_winit_window_attributes_hook(|attributes| {
                use slint::winit_030::winit;
                // Created hidden and without focus; `runtime::place` decides the rest.
                attributes.with_visible(false).with_active(false).with_position(winit::dpi::PhysicalPosition::new(
                    if runtime::env().ghost { runtime::GHOST_OFFSET } else { 0 },
                    0,
                ))
            })
            .select()
            .map_err(|e| Error::msg(format!("cannot start the UI backend: {e}")))?;
        Ok(Self)
    }

    pub fn set_host(&self, host: Arc<dyn AppHost>) {
        runtime::set_host(host);
    }

    /// Runs on the main thread until `quit`, with or without any window open.
    pub fn run(&self) -> Result<()> {
        slint::run_event_loop_until_quit().map_err(|e| Error::msg(format!("the UI event loop failed: {e}")))
    }

    pub fn quit() {
        let _ = slint::quit_event_loop();
    }
}

fn unknown_target(target: &str) -> Error {
    Error::msg(format!("unknown UI target: {target}"))
}

impl UiPort for Ui {
    fn overlay_begin(&self, mode: OverlayMode, marks: MarkSink, done: Once<OverlayOutcome>) -> Result<()> {
        overlay::begin(mode, marks, done)
    }
    fn overlay_add_display(&self, display: Display, frame: Frame, focused: bool) -> Result<()> {
        overlay::add_display(display, frame, focused)
    }
    fn overlay_set_window_rects(&self, rects: Vec<Rect>) {
        overlay::set_window_rects(rects);
    }
    fn overlay_cancel(&self) {
        overlay::cancel();
    }
    fn overlay_is_open(&self) -> bool {
        overlay::is_open()
    }
    fn overlay_bounds(&self) -> Vec<Rect> {
        overlay::bounds()
    }

    fn show_main(&self) {
        main_window::show();
    }
    fn hide_main(&self) {
        main_window::hide();
    }
    fn main_visible(&self) -> bool {
        main_window::visible()
    }
    fn send_to_main(&self, event: MainEvent) {
        main_window::send(event);
    }

    fn show_toast(&self, content: ToastData) {
        toast::show(content);
    }
    fn toast_visible(&self) -> bool {
        toast::visible()
    }
    fn last_toast(&self) -> Option<ToastData> {
        toast::last()
    }

    fn show_pin(&self, request: PinRequest) {
        pin::show(request);
    }
    fn pin_bounds(&self) -> Vec<Rect> {
        pin::bounds()
    }

    fn show_translate_popup(&self, content: PopupContent) {
        translate_popup::show(content);
    }
    fn update_translate_popup(&self, content: PopupContent) {
        translate_popup::update(content);
    }
    fn hide_translate_popup(&self) {
        translate_popup::hide();
    }
    fn translate_popup_visible(&self) -> bool {
        translate_popup::visible()
    }
    fn translate_popup_bounds(&self) -> Option<Rect> {
        translate_popup::bounds()
    }

    fn show_selection_bubble(&self, at: PointF) {
        bubble::show(at);
    }
    fn hide_selection_bubble(&self) {
        bubble::hide();
    }
    fn selection_bubble_visible(&self) -> bool {
        bubble::visible()
    }
    fn selection_bubble_bounds(&self) -> Option<Rect> {
        bubble::bounds()
    }

    fn show_countdown(&self, display: Display, rect_dip: Rect, done: Once<()>) {
        countdown::show(display, rect_dip, done);
    }
    fn show_rec_windows(&self, windows: RecWindows) {
        rec_border::show(&windows);
        rec_controls::show(&windows);
    }
    fn update_rec_windows(&self, paused: bool, levels: Option<RecLevels>) {
        rec_controls::update(paused, levels);
    }
    fn close_rec_windows(&self) {
        rec_border::close();
        rec_controls::close();
    }

    fn apply_theme(&self, theme: ThemeData) {
        main_window::apply_theme(&theme);
        overlay::apply_theme(&theme);
        toast::apply_theme(&theme);
        pin::apply_theme(&theme);
        translate_popup::apply_theme(&theme);
        bubble::apply_theme(&theme);
        rec_controls::apply_theme(&theme);
    }
    fn rebuild_language(&self) {
        main_window::rebuild_language();
    }

    fn ui_state(&self, target: &str, index: usize) -> Result<Value> {
        match target {
            "overlay" => overlay::ui_state(index),
            "main" => main_window::ui_state(),
            "toast" => toast::ui_state(),
            "pin" => pin::ui_state(index),
            "tp" => translate_popup::ui_state(),
            "bubble" => bubble::ui_state(),
            "rec-controls" => rec_controls::ui_state(),
            other => Err(unknown_target(other)),
        }
    }
    fn ui_action(&self, target: &str, index: usize, action: Value) -> Result<Value> {
        match target {
            "overlay" => overlay::ui_action(index, action),
            "main" => main_window::ui_action(action),
            "toast" => toast::ui_action(action),
            "tp" => translate_popup::ui_action(action),
            other => Err(unknown_target(other)),
        }
    }
    fn capture_page(&self, target: &str, index: usize, path: &Path) -> Result<(u32, u32)> {
        match target {
            "overlay" => overlay::capture_page(index, path),
            "main" => main_window::capture_page(path),
            "toast" => toast::capture_page(path),
            "pin" => pin::capture_page(index, path),
            "tp" => translate_popup::capture_page(path),
            "bubble" => bubble::capture_page(path),
            "rec-controls" => rec_controls::capture_page(path),
            "rec-border" => rec_border::capture_page(path),
            other => Err(unknown_target(other)),
        }
    }
}
