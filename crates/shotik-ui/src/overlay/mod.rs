//! Freeze frame, selection, annotations, toolbar, Live Text. The window half of `startOverlaySession` in
//! `src/main/capture.js`: one session at a time, one window per display, all destroyed when it ends.
//!
//! Everything Slint lives on the UI thread in a thread local. Only `OPEN` and `BOUNDS` are shared, so
//! `is_open` and `bounds` answer from any thread without waiting for it.

mod keys;
mod layer;
mod native;
mod pixels;
mod raster;
mod state;
mod view;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use shotik_core::ports::{MarkSink, Once, OverlayAction, OverlayMode, OverlayOutcome, Theme, Toast};
use shotik_core::{Display, Error, Frame, Rect, Result};
use slint::{ComponentHandle, Rgb8Pixel, SharedPixelBuffer};

use self::pixels::RgbImage;
use self::raster::{Painter, TextCache};
use self::state::{Action, Annotation, Effect, Finish, IRect, RectF, Translated};
use self::view::{OverlayWindow, PreparedFrame, Wiring};
use crate::runtime::{self, on_ui, on_ui_wait};
use crate::Overlay;

/// A session exists from `begin` until `done` has fired.
static OPEN: AtomicBool = AtomicBool::new(false);
/// `overlay_bounds`, in window order.
static BOUNDS: Mutex<Vec<Rect>> = Mutex::new(Vec::new());
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// A component waiting for its native window, which winit creates on the next turn of the event loop.
struct Slot {
    comp: Overlay,
    ready: Rc<std::cell::Cell<bool>>,
}

struct Pending {
    slot: Slot,
    display: Display,
    frame: PreparedFrame,
    focused: bool,
}

struct Session {
    id: u64,
    mode: OverlayMode,
    marks: MarkSink,
    done: Option<Once<OverlayOutcome>>,
    /// In creation order: the `index` of the test routes.
    windows: Vec<OverlayWindow>,
    /// Made ahead while the first capture runs, for the display under the cursor.
    spare: Option<Slot>,
    pending: Vec<Pending>,
    /// Hover-snap rectangles once they arrive, physical pixels, topmost first.
    rects: Option<Vec<Rect>>,
    /// The session is over and the window teardown has been scheduled.
    ending: bool,
}

fn set_bounds(update: impl FnOnce(&mut Vec<Rect>)) {
    update(&mut BOUNDS.lock().unwrap_or_else(|e| e.into_inner()));
}

fn with_session<T>(work: impl FnOnce(&mut Session) -> T) -> Option<T> {
    SESSION.with(|cell| cell.borrow_mut().as_mut().map(work))
}

/// A component and the future that notes when its native window exists.
fn new_slot(session: u64) -> Result<Slot> {
    let comp = Overlay::new().map_err(|e| Error::msg(format!("cannot create the overlay: {e}")))?;
    let ready = Rc::new(std::cell::Cell::new(false));
    let weak = comp.as_weak();
    let flag = ready.clone();
    slint::spawn_local(async move {
        use slint::winit_030::WinitWindowAccessor as _;
        if let Some(comp) = weak.upgrade() {
            if let Err(e) = comp.window().winit_window().await {
                report_failure(session, &format!("no native window for the overlay: {e}"));
                return;
            }
        }
        flag.set(true);
        open_pending(session);
    })
    .map_err(|e| Error::msg(format!("the UI event loop is not running: {e}")))?;
    Ok(Slot { comp, ready })
}

/// Opens a session. On error `done` is dropped unfired and the caller has the error.
pub fn begin(mode: OverlayMode, marks: MarkSink, done: Once<OverlayOutcome>) -> Result<()> {
    if OPEN.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err(Error::msg("Selection overlay is already open"));
    }
    set_bounds(Vec::clear);
    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let posted = slint::invoke_from_event_loop(move || {
        let spare = match new_slot(id) {
            Ok(slot) => Some(slot),
            Err(e) => {
                // `add_display` will try again and report what it finds.
                eprintln!("overlay: {e}");
                None
            }
        };
        let session = Session {
            id,
            mode,
            marks,
            done: Some(done),
            windows: Vec::new(),
            spare,
            pending: Vec::new(),
            rects: None,
            ending: false,
        };
        SESSION.with(|cell| *cell.borrow_mut() = Some(session));
    });
    if let Err(e) = posted {
        OPEN.store(false, Ordering::SeqCst);
        return Err(Error::msg(format!("the UI event loop is gone: {e}")));
    }
    Ok(())
}

/// Converts on the caller's thread, then posts. A frame arriving after the session ended is dropped: the
/// user cancelled while another monitor was still being captured, and that is not an error.
pub fn add_display(display: Display, frame: Frame, focused: bool) -> Result<()> {
    if !is_open() {
        return Ok(());
    }
    let frame = PreparedFrame::new(frame)?;
    on_ui(move || {
        let added = with_session(|session| -> Result<()> {
            if session.ending {
                return Ok(());
            }
            // The spare was made for the display under the cursor.
            let spare = if focused || session.windows.is_empty() { session.spare.take() } else { None };
            let slot = match spare {
                Some(slot) => slot,
                None => new_slot(session.id)?,
            };
            session.pending.push(Pending { slot, display, frame, focused });
            Ok(())
        });
        if let Some(Err(e)) = added {
            if focused {
                fail_session(&e.to_string());
            } else {
                eprintln!("overlay: display failed: {e}");
            }
            return;
        }
        if let Some(id) = with_session(|session| session.id) {
            open_pending(id);
        }
    });
    Ok(())
}

/// Opens every display whose frame and native window are both there.
fn open_pending(session_id: u64) {
    loop {
        let next = with_session(|session| {
            if session.id != session_id || session.ending {
                return None;
            }
            let at = session.pending.iter().position(|p| p.slot.ready.get())?;
            Some(session.pending.remove(at))
        });
        let Some(Some(pending)) = next else { return };
        let focused = pending.focused;
        if let Err(e) = open_window(session_id, pending) {
            if focused {
                fail_session(&e.to_string());
                return;
            }
            eprintln!("overlay: display failed: {e}");
        }
    }
}

fn open_window(session_id: u64, pending: Pending) -> Result<()> {
    let Pending { slot, display, frame, focused } = pending;
    let ghost = runtime::env().ghost;
    let opened = with_session(|session| -> Result<()> {
        let dispatch: Rc<dyn Fn(usize, Action)> = Rc::new(move |index, action| post_action(session_id, index, action));
        let index = session.windows.len();
        let wiring = Wiring { index, mode: session.mode, ghost, dispatch };
        let bounds = view::dip_bounds(&display, ghost);
        let mut window = OverlayWindow::new(slot.comp, display, frame, &wiring)?;
        if focused {
            (session.marks)("winCreate");
        }
        let marks = session.marks.clone();
        window.open(focused, ghost, &move || {
            if focused {
                marks("painted");
            }
        })?;
        if let Some(rects) = &session.rects {
            let snap = view::snap_rects(&window.display, rects);
            window.apply(Action::SetWindows { rects: snap });
        }
        session.windows.push(window);
        set_bounds(|all| all.push(bounds));
        if focused {
            (session.marks)("show");
        }
        Ok(())
    });
    opened.unwrap_or(Ok(()))
}

/// The session cannot go on: tell the user and end it, so it cannot hang unseen.
fn fail_session(message: &str) {
    eprintln!("overlay session failed: {message}");
    crate::toast::show(Toast {
        kind: "text".into(),
        title: runtime::tr("recFailTitle").to_string(),
        body: message.chars().take(120).collect(),
        thumb: None,
        file: None,
        color: None,
    });
    settle(Plan::Cancelled);
}

fn report_failure(session_id: u64, message: &str) {
    if with_session(|session| session.id == session_id && !session.ending) == Some(true) {
        fail_session(message);
    }
}

/// Runs an action on a window, then what it asks for. Callers are inside Slint's dispatch or on the event
/// loop; teardown is always deferred, so a window is never destroyed under its own event.
fn run_action(session_id: u64, index: usize, action: Action) {
    let effects = with_session(|session| {
        if session.id != session_id || session.ending {
            return None;
        }
        session.windows.get_mut(index).map(|w| w.apply(action))
    });
    let Some(Some(effects)) = effects else { return };
    for effect in effects {
        run_effect(session_id, index, effect);
    }
}

/// Keys arrive inside the window procedure and may end the session, so they wait for the next turn. Pointer
/// callbacks are safe to run directly but share this path for the same ordering.
fn post_action(session_id: u64, index: usize, action: Action) {
    if matches!(action, Action::Key { .. } | Action::KeyUp { .. }) {
        on_ui(move || run_action(session_id, index, action));
    } else {
        run_action(session_id, index, action);
    }
}

fn run_effect(session_id: u64, index: usize, effect: Effect) {
    match effect {
        Effect::Layer(_) => {}
        Effect::Cancel => settle(Plan::Cancelled),
        Effect::Finish(finish) => finish_session(index, finish),
        Effect::PickColor { ix, iy } => {
            let hex = with_session(|session| session.windows.get(index).map(|w| w.source_image().hex_at(ix, iy))).flatten();
            if let Some(hex) = hex {
                std::thread::spawn(move || runtime::host().color_copied(&hex));
            }
            settle(Plan::Cancelled);
        }
        Effect::CopyColor(hex) => {
            std::thread::spawn(move || runtime::host().color_copied(&hex));
        }
        Effect::Recognize { generation, rect } => recognize(session_id, index, generation, rect),
        Effect::Translate { generation, text } => translate(session_id, index, generation, &text),
        Effect::CopyText(text) => {
            std::thread::spawn(move || {
                if let Err(e) = runtime::host().copy_text(&text) {
                    eprintln!("overlay: cannot copy the text: {e}");
                }
            });
        }
        Effect::ArmFlash { generation, ms } => {
            slint::Timer::single_shot(Duration::from_millis(ms), move || {
                run_action(session_id, index, Action::FlashExpired { generation });
            });
        }
        Effect::FocusEditor => {
            with_session(|session| {
                if let Some(window) = session.windows.get(index) {
                    window.comp.invoke_focus_editor();
                }
            });
        }
        Effect::MeasureCard => {
            let height = with_session(|session| session.windows.get(index).map(|w| f64::from(w.comp.get_card_height()))).flatten();
            if let Some(height) = height {
                run_action(session_id, index, Action::CardMeasured { height });
            }
        }
    }
}

/// `oc.toBlob` of the pristine crop, then `ocrBoxes`.
fn recognize(session_id: u64, index: usize, generation: u64, rect: IRect) {
    let Some(source) = with_session(|session| session.windows.get(index).map(|w| w.source.clone())).flatten() else { return };
    std::thread::spawn(move || {
        let reply = move |boxes| {
            on_ui(move || run_action(session_id, index, Action::OcrDone { generation, boxes }));
        };
        match crop_png(&source, rect) {
            Ok(png) => runtime::host().overlay_ocr_boxes(png, Box::new(reply)),
            Err(e) => {
                eprintln!("overlay: cannot prepare the crop for recognition: {e}");
                reply(Default::default());
            }
        }
    });
}

fn crop_png(source: &SharedPixelBuffer<Rgb8Pixel>, rect: IRect) -> Result<Vec<u8>> {
    let image = RgbImage { data: source.as_bytes(), width: source.width() as i32, height: source.height() as i32 };
    let mut rgb = Vec::new();
    raster::copy_area(image, clip_rect(rect, image.width, image.height), &mut rgb);
    let clipped = clip_rect(rect, image.width, image.height);
    Frame::new(clipped.w as u32, clipped.h as u32, raster::rgb_to_bgra(&rgb))?.to_png()
}

fn clip_rect(r: IRect, width: i32, height: i32) -> IRect {
    let x = r.x.clamp(0, width - 1);
    let y = r.y.clamp(0, height - 1);
    IRect { x, y, w: r.w.min(width - x).max(1), h: r.h.min(height - y).max(1) }
}

fn translate(session_id: u64, index: usize, generation: u64, text: &str) {
    runtime::host().overlay_translate(
        text,
        Box::new(move |result| {
            let result = match result {
                Ok(value) => match serde_json::from_value::<Translated>(value) {
                    Ok(translated) => Some(translated),
                    Err(e) => {
                        eprintln!("overlay: unexpected translation reply: {e}");
                        None
                    }
                },
                Err(e) => {
                    eprintln!("overlay: translation failed: {e}");
                    None
                }
            };
            on_ui(move || run_action(session_id, index, Action::TranslateDone { generation, result }));
        }),
    );
}

/// What ends a session.
enum Plan {
    Cancelled,
    Action(Box<OverlayAction>),
    /// Burn the annotations in and encode first, off the UI thread.
    Render(Box<RenderJob>),
}

struct RenderJob {
    action: OverlayAction,
    source: SharedPixelBuffer<Rgb8Pixel>,
    annotations: Vec<Annotation>,
    area: IRect,
    kx: f64,
    text: Arc<TextCache>,
}

fn rect_of(r: IRect) -> Rect {
    Rect::new(r.x, r.y, r.w, r.h)
}

/// CSS rectangles are fractions; the port carries whole DIP.
fn rect_dip_of(r: RectF) -> Rect {
    let round = |v: f64| state::js_round(v) as i32;
    Rect::new(round(r.x), round(r.y), round(r.w), round(r.h))
}

fn finish_session(index: usize, finish: Finish) {
    let plan = with_session(|session| {
        let window = session.windows.get(index)?;
        let action = OverlayAction {
            action: finish.action,
            png: None,
            text: finish.text,
            display_id: Some(window.display.id),
            rect_phys: finish.rect_phys.map(rect_of),
            rect_dip: finish.rect_css.map(rect_dip_of),
        };
        Some(match (finish.render_png, finish.rect_phys) {
            (true, Some(area)) => Plan::Render(Box::new(RenderJob {
                action,
                source: window.source.clone(),
                annotations: window.state.annotations.clone(),
                area,
                kx: window.state.kx,
                text: window.text.clone(),
            })),
            _ => Plan::Action(Box::new(action)),
        })
    });
    if let Some(Some(plan)) = plan {
        settle(plan);
    }
}

/// `settle`: exactly once per session, tears down on the next turn of the event loop so no window is
/// destroyed under its own event.
fn settle(plan: Plan) {
    let first = with_session(|session| !std::mem::replace(&mut session.ending, true));
    if first != Some(true) {
        return;
    }
    on_ui(move || teardown(plan));
}

fn teardown(plan: Plan) {
    let Some(mut session) = SESSION.with(|cell| cell.borrow_mut().take()) else { return };
    set_bounds(Vec::clear);
    for window in &session.windows {
        if let Err(e) = window.comp.hide() {
            eprintln!("overlay: cannot hide a window: {e}");
        }
    }
    let done = session.done.take();
    drop(session);
    raster::free_text_engine();

    let finish = move |outcome: OverlayOutcome| {
        OPEN.store(false, Ordering::SeqCst);
        if let Some(done) = done {
            done(outcome);
        }
    };
    match plan {
        Plan::Cancelled => finish(OverlayOutcome::Cancelled),
        Plan::Action(action) => finish(OverlayOutcome::Done(*action)),
        Plan::Render(job) => {
            std::thread::spawn(move || {
                let RenderJob { mut action, source, annotations, area, kx, text } = *job;
                match render_png(&source, area, &annotations, kx, text) {
                    Ok(png) => action.png = Some(png),
                    // The application shows "returned no image" as a toast.
                    Err(e) => eprintln!("overlay: cannot render the output image: {e}"),
                }
                drop(source);
                raster::free_text_engine();
                finish(OverlayOutcome::Done(action));
            });
        }
    }
}

/// The selection with annotations burned in, as `doAction` does.
fn render_png(
    source: &SharedPixelBuffer<Rgb8Pixel>,
    area: IRect,
    annotations: &[Annotation],
    kx: f64,
    text: Arc<TextCache>,
) -> Result<Vec<u8>> {
    let image = RgbImage { data: source.as_bytes(), width: source.width() as i32, height: source.height() as i32 };
    let area = clip_rect(area, image.width, image.height);
    let mut painter = Painter::new(kx, text);
    let rgb = raster::bake(image, area, annotations, &mut painter);
    Frame::new(area.w as u32, area.h as u32, raster::rgb_to_bgra(&rgb))?.to_png()
}

pub fn set_window_rects(rects: Vec<Rect>) {
    on_ui(move || {
        let updates = with_session(|session| {
            let updates: Vec<(usize, Vec<RectF>)> =
                session.windows.iter().enumerate().map(|(i, w)| (i, view::snap_rects(&w.display, &rects))).collect();
            session.rects = Some(rects);
            (session.id, updates)
        });
        if let Some((id, updates)) = updates {
            for (index, rects) in updates {
                run_action(id, index, Action::SetWindows { rects });
            }
        }
    });
}

pub fn cancel() {
    on_ui(|| settle(Plan::Cancelled));
}

/// Works from any thread at once: it never waits for the UI thread.
pub fn is_open() -> bool {
    OPEN.load(Ordering::SeqCst)
}

/// Global DIP bounds of the open windows. Works from any thread at once.
pub fn bounds() -> Vec<Rect> {
    BOUNDS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn apply_theme(theme: &Theme) {
    let theme = theme.clone();
    on_ui(move || {
        with_session(|session| {
            for window in &mut session.windows {
                window.set_theme(&theme);
            }
        });
    });
}

fn no_window(index: usize) -> Error {
    Error::msg(format!("no overlay window {index}"))
}

pub fn ui_state(index: usize) -> Result<Value> {
    on_ui_wait(move || {
        with_session(|session| session.windows.get(index).map(|w| w.state.snapshot())).flatten().ok_or_else(|| no_window(index))
    })?
}

pub fn ui_action(index: usize, action: Value) -> Result<Value> {
    let action: Action = serde_json::from_value(action).map_err(|e| Error::msg(format!("unknown overlay action: {e}")))?;
    on_ui_wait(move || {
        let id = with_session(|session| session.id).ok_or_else(|| no_window(index))?;
        run_action(id, index, action);
        with_session(|session| session.windows.get(index).map(|w| w.state.snapshot())).flatten().ok_or_else(|| no_window(index))
    })?
}

pub fn capture_page(index: usize, path: &Path) -> Result<(u32, u32)> {
    let path: PathBuf = path.to_path_buf();
    on_ui_wait(move || {
        with_session(|session| session.windows.get(index).map(|w| runtime::snapshot_png(w.comp.window(), &path)))
            .flatten()
            .unwrap_or_else(|| Err(no_window(index)))
    })?
}

/// Test hook of the preview: the private HWND of a window, for sending it key messages. Only this process
/// can reach it, and nothing else on the desktop sees them.
pub fn hwnd_for_test(index: usize) -> Result<isize> {
    on_ui_wait(move || {
        with_session(|session| session.windows.get(index).map(|w| runtime::hwnd_of(w.comp.window()).map(|h| h.0 as isize)))
            .flatten()
            .unwrap_or_else(|| Err(no_window(index)))
    })?
}

/// Test hook of the preview: input through Slint's own dispatch, as winit would deliver it.
pub fn dispatch_event_for_test(index: usize, event: slint::platform::WindowEvent) -> Result<()> {
    on_ui_wait(move || {
        let comp = with_session(|session| session.windows.get(index).map(|w| w.comp.clone_strong())).flatten().ok_or_else(|| no_window(index))?;
        comp.window().dispatch_event_with_result(event).map(|_| ()).map_err(|e| Error::msg(format!("cannot dispatch an event: {e}")))
    })?
}

/// Test hook of the preview: completed paints of overlay windows and the time of the last, in
/// milliseconds. Answers without touching the UI thread.
pub fn paint_stats_for_test() -> (u64, f64) {
    (native::PAINTS.load(Ordering::Acquire), native::LAST_PAINT_MICROS.load(Ordering::Relaxed) as f64 / 1000.0)
}

/// Test hook of the preview: the conversion `add_display` runs on the caller's thread.
pub fn prepare_for_test(frame: Frame) -> Result<usize> {
    let prepared = PreparedFrame::new(frame)?;
    Ok(prepared.bright.as_bytes().len() + prepared.dim.as_bytes().len())
}
