//! One overlay window: draws `State` through Slint properties and turns input into `Action`s. All of it lives
//! on the UI thread. As little logic as it can hold; what is not here is in `state.rs`, `raster.rs`, `layer.rs`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;
use shotik_core::ports::{OverlayMode, Theme};
use shotik_core::{Display, Error, Frame, Rect, Result};
use slint::{Color, ComponentHandle, Image, LogicalSize, ModelRc, Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel, Model as _};

use super::layer::TileGrid;
use super::native::{self, Hook, KeyEvent};
use super::pixels::{self, MagnifierPainter, RgbImage};
use super::raster::{self, Painter, TextCache};
use super::state::{reduce, Action, Button, Cursor, Effect, IRect, LayerOp, RectF, Rgb, SizeKey, State, Tool, COLORS, WordState};
use crate::runtime::{self, Placement};
use crate::{Overlay, OverlayHint, OverlayMotion, OverlayTile, OverlayTips, OverlayWord};

/// The frame ready to show: converted on the caller's thread, so the UI thread only has to draw it.
pub struct PreparedFrame {
    pub width: u32,
    pub height: u32,
    pub bright: SharedPixelBuffer<Rgb8Pixel>,
    pub dim: SharedPixelBuffer<Rgb8Pixel>,
}

impl PreparedFrame {
    /// Takes the frame by value so its pixels are gone once this returns: the session keeps RGB only.
    pub fn new(frame: Frame) -> Result<Self> {
        let (width, height) = (frame.width(), frame.height());
        if width == 0 || height == 0 {
            return Err(Error::msg(format!("cannot show a {width}x{height} frame")));
        }
        // A fresh buffer costs more than the conversion: every page is touched for the first time. Below a
        // megapixel a thread would not pay for itself.
        let big = u64::from(width) * u64::from(height) >= 1_000_000;
        let new_buffer = || SharedPixelBuffer::<Rgb8Pixel>::new(width, height);
        let (mut bright, mut dim) = if big {
            std::thread::scope(|scope| match std::thread::Builder::new().spawn_scoped(scope, new_buffer) {
                Ok(other) => (new_buffer(), other.join().unwrap_or_else(|_| new_buffer())),
                Err(_) => (new_buffer(), new_buffer()),
            })
        } else {
            (new_buffer(), new_buffer())
        };
        let threads = if big { std::thread::available_parallelism().map_or(1, |n| n.get().min(4)) } else { 1 };
        pixels::bgra_to_rgb_and_veil_parallel(frame.bgra(), bright.make_mut_bytes(), dim.make_mut_bytes(), threads);
        Ok(Self { width, height, bright, dim })
    }
}

/// What a window needs from its session.
pub struct Wiring {
    pub index: usize,
    pub mode: OverlayMode,
    pub ghost: bool,
    /// Runs an action later on the UI thread, in this window.
    pub dispatch: Rc<dyn Fn(usize, Action)>,
}

pub struct OverlayWindow {
    pub comp: Overlay,
    pub state: State,
    pub display: Display,
    pub source: SharedPixelBuffer<Rgb8Pixel>,
    pub text: Arc<TextCache>,
    scale: f64,
    hook: Rc<Hook>,
    painter: Painter,
    grid: Option<TileGrid>,
    bright_tiles: Rc<VecModel<OverlayTile>>,
    dim_tiles: Rc<VecModel<OverlayTile>>,
    tile_rows: HashMap<usize, usize>,
    stale_dims: BTreeSet<usize>,
    words: Rc<VecModel<OverlayWord>>,
    shown_words: Vec<i32>,
    magnifier: MagnifierPainter,
    magnifier_at: Option<(i32, i32)>,
    accent: [u8; 3],
    shadows: Shadows,
    card_shadow_for: f64,
}

#[derive(Default)]
struct Shadows {
    magnifier: bool,
    toolbar: bool,
    options: bool,
    text_toolbar: bool,
}

/// CSS `box-shadow` of a floating panel.
struct ShadowSpec {
    width: f64,
    height: f64,
    radius: f64,
    spread: f64,
    blur: f64,
    alpha: f64,
    offset_y: f64,
}

const MAGNIFIER_SHADOW: ShadowSpec =
    ShadowSpec { width: 161.0, height: 184.0, radius: 14.0, spread: 48.0, blur: 32.0, alpha: 0.5, offset_y: 12.0 };
const TOOLBAR_SHADOW: ShadowSpec =
    ShadowSpec { width: super::state::TOOLBAR_W, height: super::state::TOOLBAR_H, radius: 13.0, spread: 56.0, blur: 40.0, alpha: 0.55, offset_y: 14.0 };
const OPTIONS_SHADOW: ShadowSpec =
    ShadowSpec { width: super::state::OPTIONS_W, height: super::state::OPTIONS_H, radius: 13.0, spread: 56.0, blur: 40.0, alpha: 0.55, offset_y: 14.0 };
const TEXT_TOOLBAR_SHADOW: ShadowSpec = ShadowSpec {
    width: super::state::TEXT_TOOLBAR_W,
    height: super::state::TEXT_TOOLBAR_H,
    radius: 13.0,
    spread: 56.0,
    blur: 40.0,
    alpha: 0.55,
    offset_y: 14.0,
};

fn shadow_image(spec: &ShadowSpec, scale: f64) -> Image {
    let shadow = raster::panel_shadow(raster::PanelShadowSpec {
        width: spec.width * scale,
        height: spec.height * scale,
        radius: spec.radius * scale,
        spread: spec.spread * scale,
        blur: spec.blur * scale,
        alpha: spec.alpha,
        offset_y: spec.offset_y * scale,
    });
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(shadow.width, shadow.height);
    buffer.make_mut_bytes().copy_from_slice(&shadow.rgba);
    Image::from_rgba8_premultiplied(buffer)
}

fn color_of(c: Rgb) -> Color {
    Color::from_rgb_u8(c.0, c.1, c.2)
}

fn accent_of(theme: &Theme) -> [u8; 3] {
    let c = runtime::color_of(&theme.accent);
    [c.red(), c.green(), c.blue()]
}

fn string_of(dict: &serde_json::Map<String, Value>, key: &str) -> SharedString {
    dict.get(key).and_then(Value::as_str).unwrap_or(key).into()
}

/// The strings the state itself shows, from the dictionary.
fn state_strings(dict: &serde_json::Map<String, Value>) -> BTreeMap<String, String> {
    ["copiedTitle", "ocrNoText", "txtRecognizing", "txtTranslating", "txtTranslateFail"]
        .into_iter()
        .map(|key| (key.to_string(), string_of(dict, key).to_string()))
        .collect()
}

fn tips_of(dict: &serde_json::Map<String, Value>) -> OverlayTips {
    let d = |key: &str| string_of(dict, key);
    let many = |keys: &[&str]| -> ModelRc<SharedString> {
        ModelRc::new(VecModel::from(keys.iter().map(|k| d(k)).collect::<Vec<_>>()))
    };
    OverlayTips {
        tools: many(&[
            "toolMove",
            "toolPen",
            "toolArrow",
            "toolLine",
            "toolRect",
            "toolEllipse",
            "toolHighlight",
            "toolBlur",
            "toolText",
            "toolCounter",
        ]),
        undo: d("tipUndo"),
        redo: d("tipRedo"),
        ocr: d("tipOcr"),
        pin: d("tipPin"),
        rec: d("tipRec"),
        save: d("tipSave"),
        claude: d("tipClaude"),
        copy: d("tipCopy"),
        cancel: d("tipCancel"),
        sizes: many(&["tipThin", "tipMedium", "tipThick"]),
        text_copy: d("txtCopy"),
        text_all: d("txtAll"),
        text_translate: d("txtTranslate"),
        text_back: d("txtBack"),
        translation_title: d("txtTranslation"),
        translation_copy: d("txtCopyTranslation"),
    }
}

fn hints_of(dict: &serde_json::Map<String, Value>) -> ModelRc<OverlayHint> {
    let pairs = [
        ("hintDrag", "hintDragDesc"),
        ("hintClick", "hintClickDesc"),
        ("hintAlt", "hintAltDesc"),
        ("hintColor", "hintColorDesc"),
        ("hintEsc", "hintEscDesc"),
    ];
    let hints: Vec<OverlayHint> =
        pairs.iter().map(|(bold, desc)| OverlayHint { bold: string_of(dict, bold), desc: string_of(dict, desc) }).collect();
    ModelRc::new(VecModel::from(hints))
}

/// Physical outer bounds before the ghost offset, and after.
fn window_bounds(display: &Display, ghost: bool) -> (Rect, (i32, i32, i32, i32)) {
    let p = display.physical_bounds;
    let offset = if ghost { runtime::GHOST_OFFSET } else { 0 };
    (p, (p.x + offset, p.y, p.width, p.height))
}

/// Where this window is in global DIP, in ghost mode with the offset: `overlay_bounds`.
pub fn dip_bounds(display: &Display, ghost: bool) -> Rect {
    let b = display.bounds;
    Rect::new(b.x + if ghost { runtime::GHOST_OFFSET } else { 0 }, b.y, b.width, b.height)
}

/// The snap rectangles for one display: physical pixels to its local CSS pixels, topmost first, without
/// what does not reach it. The JS divides global physical by scale and subtracts the DIP origin, which is
/// only right on the primary; this goes through the display's physical origin instead.
pub fn snap_rects(display: &Display, rects: &[Rect]) -> Vec<RectF> {
    let scale = display.scale_factor;
    let origin = display.physical_bounds;
    let (width, height) = (f64::from(display.bounds.width), f64::from(display.bounds.height));
    rects
        .iter()
        .map(|r| {
            RectF::new(
                f64::from(r.x - origin.x) / scale,
                f64::from(r.y - origin.y) / scale,
                f64::from(r.width) / scale,
                f64::from(r.height) / scale,
            )
        })
        .filter(|r| r.x < width && r.y < height && r.x + r.w > 0.0 && r.y + r.h > 0.0)
        .collect()
}

impl OverlayWindow {
    /// Puts a frame in a component whose native window exists, still hidden. `open` makes it visible.
    pub fn new(comp: Overlay, display: Display, frame: PreparedFrame, wiring: &Wiring) -> Result<Self> {
        let scale = display.scale_factor;
        let (phys, bounds) = window_bounds(&display, wiring.ghost);
        let window = comp.window();
        runtime::place(
            window,
            Placement {
                x: phys.x,
                y: phys.y,
                width: phys.width as u32,
                height: phys.height as u32,
                topmost: true,
                no_activate: false,
                click_through: false,
                rounded: false,
                scale: scale as f32,
            },
        )?;
        // Winit tells about DPI changes later, in its own time; the first frame needs the real one now.
        window
            .dispatch_event_with_result(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: scale as f32 })
            .map_err(|e| Error::msg(format!("cannot set the overlay's scale: {e}")))?;
        window
            .dispatch_event_with_result(slint::platform::WindowEvent::Resized {
                size: LogicalSize::new((f64::from(phys.width) / scale) as f32, (f64::from(phys.height) / scale) as f32),
            })
            .map_err(|e| Error::msg(format!("cannot size the overlay: {e}")))?;

        let dispatch = wiring.dispatch.clone();
        let index = wiring.index;
        let hook = Hook::new(bounds, move |key: KeyEvent| {
            let action = if key.down {
                Action::Key { code: key.code, ctrl: key.ctrl, shift: key.shift }
            } else {
                Action::KeyUp { code: key.code }
            };
            dispatch(index, action);
        });
        native::install(runtime::hwnd_of(window)?, hook.clone())?;

        let dict = runtime::dictionary();
        let viewport = (f64::from(display.bounds.width), f64::from(display.bounds.height));
        let state = State::new(wiring.mode, (frame.width, frame.height), viewport, state_strings(&dict));
        let theme = runtime::current_theme();
        crate::apply_theme!(comp, theme);

        comp.global::<OverlayMotion>().set_enabled(!wiring.ghost);
        comp.set_frame_dim(Image::from_rgb8(frame.dim));
        comp.set_frame_bright(Image::from_rgb8(frame.bright.clone()));
        comp.set_tips(tips_of(&dict));
        comp.set_hints(hints_of(&dict));
        comp.set_badge_text(string_of(&dict, "claudeBadge"));
        comp.set_palette(ModelRc::new(VecModel::from(COLORS.iter().map(|c| color_of(*c)).collect::<Vec<_>>())));

        let bright_tiles = Rc::new(VecModel::default());
        let dim_tiles = Rc::new(VecModel::default());
        let words = Rc::new(VecModel::default());
        comp.set_bright_tiles(ModelRc::from(bright_tiles.clone()));
        comp.set_dim_tiles(ModelRc::from(dim_tiles.clone()));
        comp.set_words(ModelRc::from(words.clone()));

        Self::wire(&comp, wiring);

        let text = Arc::new(TextCache::default());
        let painter = Painter::new(state.kx, text.clone());
        let magnifier = MagnifierPainter::new((143.0 * scale).round() as usize, scale);
        let accent = accent_of(&theme);
        let mut this = Self {
            comp,
            state,
            display,
            source: frame.bright,
            text,
            scale,
            hook,
            painter,
            grid: None,
            bright_tiles,
            dim_tiles,
            tile_rows: HashMap::new(),
            stale_dims: BTreeSet::new(),
            words,
            shown_words: Vec::new(),
            magnifier,
            magnifier_at: None,
            accent,
            shadows: Shadows::default(),
            card_shadow_for: 0.0,
        };
        this.sync();
        Ok(this)
    }

    /// Paints while cloaked, then lets the window be seen: no flash of an empty window. Reports `painted`
    /// once the first frame is really in the window's surface.
    pub fn open(&mut self, focused: bool, ghost: bool, painted: &dyn Fn()) -> Result<()> {
        use slint::winit_030::WinitWindowAccessor as _;
        let window = self.comp.window();
        let hwnd = runtime::hwnd_of(window)?;
        native::set_cloaked(hwnd, true);
        // Visible as far as GDI goes. Slint draws the first frame inside `show`, and Windows throws away
        // what is drawn into a window that is not shown yet.
        window.with_winit_window(|w| w.set_visible(true));
        self.comp.show().map_err(|e| Error::msg(format!("cannot show the overlay: {e}")))?;
        painted();
        self.state.ready = true;
        native::set_cloaked(hwnd, false);
        if focused && !ghost {
            runtime::take_focus(hwnd);
        }
        Ok(())
    }

    fn wire(comp: &Overlay, wiring: &Wiring) {
        let index = wiring.index;

        let d = wiring.dispatch.clone();
        comp.on_pointer_down(move |x, y, shift| d(index, Action::PointerDown { x: f64::from(x), y: f64::from(y), shift }));
        let d = wiring.dispatch.clone();
        comp.on_pointer_moved(move |x, y, shift, alt| {
            d(index, Action::PointerMove { x: f64::from(x), y: f64::from(y), shift, alt });
        });
        let d = wiring.dispatch.clone();
        comp.on_pointer_up(move |x, y, shift| d(index, Action::PointerUp { x: f64::from(x), y: f64::from(y), shift }));
        let d = wiring.dispatch.clone();
        comp.on_double_click(move |x, y| d(index, Action::DoubleClick { x: f64::from(x), y: f64::from(y) }));
        let d = wiring.dispatch.clone();
        comp.on_set_tool(move |i| d(index, Action::SetTool { tool: Tool::ALL[i as usize] }));
        let d = wiring.dispatch.clone();
        comp.on_set_color(move |i| d(index, Action::SetColor { color: COLORS[i as usize] }));
        let d = wiring.dispatch.clone();
        comp.on_set_size(move |i| d(index, Action::SetSize { size: SizeKey::ALL[i as usize] }));
        let d = wiring.dispatch.clone();
        comp.on_undo(move || d(index, Action::Undo));
        let d = wiring.dispatch.clone();
        comp.on_redo(move || d(index, Action::Redo));
        let d = wiring.dispatch.clone();
        comp.on_action(move |name| {
            let button = match name.as_str() {
                "ocr" => Button::Ocr,
                "pin" => Button::Pin,
                "rec" => Button::Rec,
                "save" => Button::Save,
                "claude" => Button::Claude,
                "copy" => Button::Copy,
                _ => Button::Cancel,
            };
            d(index, Action::Button { button });
        });
        let d = wiring.dispatch.clone();
        comp.on_text_action(move |name| {
            let action = match name.as_str() {
                "copy" => Action::TextCopy,
                "all" => Action::TextAll,
                "translate" => Action::TextTranslate,
                _ => Action::TextBack,
            };
            d(index, action);
        });
        let d = wiring.dispatch.clone();
        comp.on_card_action(move |name| {
            let action = if name == "copy" { Action::TranslateCopy } else { Action::TranslateClose };
            d(index, action);
        });
        let d = wiring.dispatch.clone();
        comp.on_editor_edited(move |text| d(index, Action::EditorInput { text: text.to_string() }));
        let d = wiring.dispatch.clone();
        comp.on_editor_commit(move || d(index, Action::EditorCommit));
        let d = wiring.dispatch.clone();
        comp.on_editor_cancel(move || d(index, Action::EditorCancel));
        // Alt+F4 and friends: `win.on('closed')` settles the session with nothing.
        let d = wiring.dispatch.clone();
        comp.window().on_close_requested(move || {
            d(index, Action::Button { button: Button::Cancel });
            slint::CloseRequestResponse::KeepWindowShown
        });
    }

    pub fn source_image(&self) -> RgbImage<'_> {
        RgbImage { data: self.source.as_bytes(), width: self.source.width() as i32, height: self.source.height() as i32 }
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        crate::apply_theme!(self.comp, theme);
        self.accent = accent_of(theme);
        self.magnifier_at = None;
        self.sync();
    }

    /// Applies an action to the state and redraws. Returns what the session has to do about it.
    pub fn apply(&mut self, action: Action) -> Vec<Effect> {
        let effects = reduce(&mut self.state, action);
        for effect in &effects {
            match effect {
                Effect::Layer(op) => self.apply_layer(op),
                Effect::FocusEditor => {
                    self.comp.set_editor_text(SharedString::default());
                }
                _ => {}
            }
        }
        self.publish_tiles();
        self.sync();
        effects
    }

    fn apply_layer(&mut self, op: &LayerOp) {
        let (width, height) = (self.source.width() as i32, self.source.height() as i32);
        let grid = self.grid.get_or_insert_with(|| TileGrid::new(width, height));
        let source = RgbImage { data: self.source.as_bytes(), width, height };
        let painter = &mut self.painter;
        let state = &self.state;
        match op {
            LayerOp::LiveExtended { from } => {
                if let Some(live) = &state.temp {
                    grid.live_extended(live, *from, source, painter);
                }
            }
            LayerOp::LiveChanged => {
                if let Some(live) = &state.temp {
                    grid.live_changed(live, source, painter);
                }
            }
            LayerOp::LiveDropped => grid.live_dropped(),
            LayerOp::LiveCommitted => grid.live_committed(),
            LayerOp::Added => {
                if let Some(last) = state.annotations.last() {
                    grid.added(last, source, painter);
                }
            }
            LayerOp::Removed(removed) => {
                if let Some(bounds) = painter.bounds(removed) {
                    grid.rebuild(bounds, &state.annotations, source, painter);
                }
            }
            LayerOp::Replaced => grid.replaced(&state.annotations, source, painter),
        }
        // Undo during a stroke rebuilds tiles under it: a stroke's tail alone would not bring it back.
        if matches!(op, LayerOp::Added | LayerOp::Removed(_) | LayerOp::Replaced) {
            if let Some(live) = &state.temp {
                grid.live_changed(live, source, painter);
            }
        }
    }

    /// Hands changed tiles to Slint. The bright copy goes up right away; the copy under the veil only
    /// matters for tiles the hole does not cover, so it waits until one is seen.
    fn publish_tiles(&mut self) {
        let Some(grid) = self.grid.as_mut() else { return };
        let (kx, ky) = (self.state.kx, self.state.ky);
        for (index, tile) in grid.take_changed() {
            let r = tile.rect;
            let buffer = SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(&tile.display, r.w as u32, r.h as u32);
            let item = |image: Image| OverlayTile {
                image,
                x: (f64::from(r.x) / kx) as f32,
                y: (f64::from(r.y) / ky) as f32,
                width: (f64::from(r.w) / kx) as f32,
                height: (f64::from(r.h) / ky) as f32,
            };
            let bright = item(Image::from_rgb8(buffer));
            match self.tile_rows.get(&index) {
                Some(&row) => self.bright_tiles.set_row_data(row, bright),
                None => {
                    self.tile_rows.insert(index, self.bright_tiles.row_count());
                    self.bright_tiles.push(bright);
                    self.dim_tiles.push(item(Image::default()));
                }
            }
            self.stale_dims.insert(index);
        }
    }

    fn refresh_dim_tiles(&mut self, hole: Option<IRect>) {
        if self.stale_dims.is_empty() {
            return;
        }
        let Some(grid) = self.grid.as_ref() else { return };
        let (kx, ky) = (self.state.kx, self.state.ky);
        let covered = |r: IRect| hole.is_some_and(|h| r.x >= h.x && r.y >= h.y && r.x + r.w <= h.x + h.w && r.y + r.h <= h.y + h.h);
        let mut still_stale = BTreeSet::new();
        for &index in &self.stale_dims {
            let Some(tile) = grid.tile(index) else { continue };
            let r = tile.rect;
            if covered(r) {
                still_stale.insert(index);
                continue;
            }
            let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(r.w as u32, r.h as u32);
            pixels::veil_rgb(&tile.display, buffer.make_mut_bytes());
            if let Some(&row) = self.tile_rows.get(&index) {
                self.dim_tiles.set_row_data(
                    row,
                    OverlayTile {
                        image: Image::from_rgb8(buffer),
                        x: (f64::from(r.x) / kx) as f32,
                        y: (f64::from(r.y) / ky) as f32,
                        width: (f64::from(r.w) / kx) as f32,
                        height: (f64::from(r.h) / ky) as f32,
                    },
                );
            }
        }
        self.stale_dims = still_stale;
    }

    /// Pushes the state to Slint. Setters ignore equal values, so this is cheap to call after anything.
    pub fn sync(&mut self) {
        let hole = self.state.hole().map(|r| clip_to(r, self.source.width() as i32, self.source.height() as i32));
        self.refresh_dim_tiles(hole);

        let s = &self.state;
        let c = &self.comp;
        let (kx, ky) = (s.kx, s.ky);
        let x_of = |px: i32| (f64::from(px) / kx) as f32;
        let y_of = |px: i32| (f64::from(px) / ky) as f32;

        c.set_hole_visible(hole.is_some());
        if let Some(r) = hole {
            c.set_hole_x(x_of(r.x));
            c.set_hole_y(y_of(r.y));
            c.set_hole_w(x_of(r.w));
            c.set_hole_h(y_of(r.h));
            c.set_hole_px_x(r.x);
            c.set_hole_px_y(r.y);
            c.set_hole_px_w(r.w);
            c.set_hole_px_h(r.h);
        }

        let chrome = s.selection_chrome();
        c.set_selection_visible(chrome.is_some());
        if let Some(chrome) = chrome {
            c.set_selection_x(x_of(chrome.rect.x));
            c.set_selection_y(y_of(chrome.rect.y));
            c.set_selection_w(x_of(chrome.rect.w));
            c.set_selection_h(y_of(chrome.rect.h));
            c.set_selection_border((chrome.border / kx) as f32);
            c.set_thirds_visible(chrome.thirds);
            c.set_handles_visible(chrome.handles);
        }

        let outline = s.hover_outline();
        c.set_outline_visible(outline.is_some());
        if let Some((_, border)) = outline {
            c.set_outline_border((border / kx) as f32);
        }

        let dims = s.dims();
        c.set_dims_visible(dims.is_some());
        if let Some((text, x, y)) = dims {
            c.set_dims_text(text.into());
            c.set_dims_x(x as f32);
            c.set_dims_y(y as f32);
        }

        let magnifier = s.magnifier();
        c.set_magnifier_visible(magnifier.is_some());
        if let Some(m) = magnifier {
            c.set_magnifier_x(m.x as f32);
            c.set_magnifier_y(m.y as f32);
            if self.magnifier_at != Some((m.ix, m.iy)) {
                self.magnifier_at = Some((m.ix, m.iy));
                let image = RgbImage { data: self.source.as_bytes(), width: self.source.width() as i32, height: self.source.height() as i32 };
                let side = self.magnifier.side();
                let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(side as u32, side as u32);
                self.magnifier.paint(image, m.ix, m.iy, self.accent, buffer.make_mut_bytes());
                c.set_magnifier_image(Image::from_rgba8_premultiplied(buffer));
                c.set_magnifier_hex(image.hex_at(m.ix, m.iy).into());
                c.set_magnifier_pos(format!("{}, {}", m.ix, m.iy).into());
            }
            if !self.shadows.magnifier {
                self.shadows.magnifier = true;
                c.set_magnifier_shadow(shadow_image(&MAGNIFIER_SHADOW, self.scale));
            }
        }

        c.set_hints_visible(s.hints_visible());
        c.set_badge_visible(s.badge_visible);

        c.set_toolbar_visible(s.toolbar.is_some());
        if let Some(t) = s.toolbar {
            c.set_toolbar_x(t.x as f32);
            c.set_toolbar_y(t.y as f32);
            c.set_toolbar_tips_below(t.tips_below);
            if !self.shadows.toolbar {
                self.shadows.toolbar = true;
                c.set_toolbar_shadow(shadow_image(&TOOLBAR_SHADOW, self.scale));
            }
        }
        c.set_tool(Tool::ALL.iter().position(|t| *t == s.tool).unwrap_or(0) as i32);
        c.set_can_undo(s.can_undo());
        c.set_can_redo(s.can_redo());

        c.set_options_visible(s.options.is_some());
        if let Some(o) = s.options {
            c.set_options_x(o.x as f32);
            c.set_options_y(o.y as f32);
            if !self.shadows.options {
                self.shadows.options = true;
                c.set_options_shadow(shadow_image(&OPTIONS_SHADOW, self.scale));
            }
        }
        c.set_color_index(COLORS.iter().position(|v| *v == s.color).map_or(-1, |i| i as i32));
        c.set_size_index(SizeKey::ALL.iter().position(|v| *v == s.size).unwrap_or(1) as i32);

        c.set_text_toolbar_visible(s.text_toolbar.is_some());
        if let Some(t) = s.text_toolbar {
            c.set_text_toolbar_x(t.x as f32);
            c.set_text_toolbar_y(t.y as f32);
            c.set_text_toolbar_tips_below(t.tips_below);
            if !self.shadows.text_toolbar {
                self.shadows.text_toolbar = true;
                c.set_text_toolbar_shadow(shadow_image(&TEXT_TOOLBAR_SHADOW, self.scale));
            }
        }

        self.sync_words();

        let s = &self.state;
        let c = &self.comp;
        c.set_rubber_visible(s.rubber_rect.is_some());
        if let Some(r) = s.rubber_rect {
            c.set_rubber_x(r.x as f32);
            c.set_rubber_y(r.y as f32);
            c.set_rubber_w(r.w as f32);
            c.set_rubber_h(r.h as f32);
        }

        c.set_flash_visible(s.flash.is_some());
        if let Some(f) = &s.flash {
            c.set_flash_text(f.text.as_str().into());
            c.set_flash_x(f.x as f32);
            c.set_flash_y(f.y as f32);
        }

        c.set_card_visible(s.card.is_some());
        if let Some(card) = &s.card {
            c.set_card_x(card.x as f32);
            c.set_card_y(card.y as f32);
            c.set_card_w(card.width as f32);
            c.set_card_loading(card.loading);
            c.set_card_body(card.body.as_str().into());
            c.set_card_meta(card.meta.as_str().into());
        }
        if let Some(card) = self.state.card.as_ref().filter(|card| card.height > 0.0 && card.height != self.card_shadow_for) {
            self.card_shadow_for = card.height;
            let spec = ShadowSpec { width: card.width, height: card.height, radius: 13.0, spread: 64.0, blur: 50.0, alpha: 0.6, offset_y: 18.0 };
            self.comp.set_card_shadow(shadow_image(&spec, self.scale));
        }

        let s = &self.state;
        let c = &self.comp;
        let open = s.editor.is_some();
        if self.hook.editor_open.get() && !open {
            c.invoke_blur_editor();
        }
        self.hook.editor_open.set(open);
        c.set_editor_visible(open);
        if let Some(e) = &s.editor {
            c.set_editor_x(e.x as f32);
            c.set_editor_y(e.y as f32);
            c.set_editor_color(color_of(s.color));
            c.set_editor_font(s.size.font() as f32);
        }

        c.set_cursor(match s.cursor {
            Cursor::Crosshair => 0,
            Cursor::Move => 1,
            Cursor::Text => 2,
            Cursor::Progress => 3,
            Cursor::NwseResize => 4,
            Cursor::NeswResize => 5,
            Cursor::NsResize => 6,
            Cursor::EwResize => 7,
        });
    }

    fn sync_words(&mut self) {
        let s = &self.state;
        self.comp.set_words_visible(s.words_visible);
        if s.words.len() != self.shown_words.len() {
            let items: Vec<OverlayWord> = s
                .words
                .iter()
                .map(|w| OverlayWord { x: w.x as f32, y: w.y as f32, width: w.w as f32, height: w.h as f32, state: 0 })
                .collect();
            self.words.set_vec(items);
            self.shown_words = vec![0; s.words.len()];
        }
        for (index, shown) in self.shown_words.iter_mut().enumerate() {
            let now = match s.word_state(index) {
                WordState::Normal => 0,
                WordState::Hovered => 1,
                WordState::Selected => 2,
            };
            if *shown != now {
                *shown = now;
                let mut row = self.words.row_data(index).unwrap_or_default();
                row.state = now;
                self.words.set_row_data(index, row);
            }
        }
    }
}

fn clip_to(r: IRect, width: i32, height: i32) -> IRect {
    let x = r.x.clamp(0, width - 1);
    let y = r.y.clamp(0, height - 1);
    IRect { x, y, w: r.w.min(width - x).max(1), h: r.h.min(height - y).max(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(physical: Rect, bounds: Rect, scale: f64) -> Display {
        Display { id: 1, bounds, work_area: bounds, scale_factor: scale, physical_bounds: physical, primary: false }
    }

    #[test]
    fn snap_rectangles_convert_per_display_and_drop_what_misses_it() {
        // The reference machine: the second monitor sits at physical 3840,694 and DIP 2560,462.
        let second = display(Rect::new(3840, 694, 1920, 1080), Rect::new(2560, 462, 1920, 1080), 1.0);
        let rects = [Rect::new(3900, 700, 400, 300), Rect::new(0, 0, 500, 500)];
        assert_eq!(snap_rects(&second, &rects), [RectF::new(60.0, 6.0, 400.0, 300.0)]);

        // The 150% primary.
        let primary = display(Rect::new(0, 0, 3840, 2160), Rect::new(0, 0, 2560, 1440), 1.5);
        let got = snap_rects(&primary, &[Rect::new(480, 280, 2200, 1280), Rect::new(3900, 700, 400, 300)]);
        assert_eq!(got, [RectF::new(320.0, 280.0 / 1.5, 2200.0 / 1.5, 1280.0 / 1.5)]);
    }

    #[test]
    fn bounds_follow_the_display_with_the_ghost_offset() {
        let d = display(Rect::new(-2560, 419, 2560, 1440), Rect::new(-2560, 279, 2560, 1440), 1.0);
        assert_eq!(dip_bounds(&d, false), Rect::new(-2560, 279, 2560, 1440));
        assert_eq!(dip_bounds(&d, true), Rect::new(17440, 279, 2560, 1440));
        assert_eq!(window_bounds(&d, true).1, (17440, 419, 2560, 1440));
    }

    #[test]
    fn the_hole_stays_inside_the_frame() {
        assert_eq!(clip_to(IRect::new(3840, 2160, 1, 1), 3840, 2160), IRect::new(3839, 2159, 1, 1));
        assert_eq!(clip_to(IRect::new(100, 100, 5000, 5000), 3840, 2160), IRect::new(100, 100, 3740, 2060));
    }
}
