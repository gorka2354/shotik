//! What the overlay knows and decides: selection geometry, hit-testing, handles, hover-snap, annotation
//! history, Live Text selection, toolbar placement, flash timer. No toolkit types: `view.rs` draws this and
//! feeds it input. A port of `src/overlay/overlay.js`; function names follow it so both read side by side.
//!
//! Two coordinate spaces, as in the JS: CSS pixels (`css`, Slint's logical pixels, local to the display)
//! and image pixels (`img`, the physical pixels of the frozen frame).

use std::collections::{BTreeMap, BTreeSet};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};
use shotik_core::ports::{OcrBoxes, OverlayActionKind, OverlayMode};

use super::keys::Code;

// ---------- values ----------

/// An opaque colour, `#RRGGBB` in JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    pub fn parse(hex: &str) -> Option<Self> {
        let digits = hex.strip_prefix('#')?;
        if digits.len() != 6 || !digits.is_ascii() {
            return None;
        }
        let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
        Some(Self(channel(0)?, channel(2)?, channel(4)?))
    }

    /// The digit colour on a counter badge: `lightOrDark`.
    pub fn light_or_dark(self) -> Self {
        let luminance = 0.299 * f64::from(self.0) + 0.587 * f64::from(self.1) + 0.114 * f64::from(self.2);
        if luminance > 150.0 {
            Self(0x1b, 0x1d, 0x24)
        } else {
            Self(0xff, 0xff, 0xff)
        }
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| D::Error::custom(format!("not a #RRGGBB colour: {text}")))
    }
}

/// The palette of the options bar, in order.
pub const COLORS: [Rgb; 9] = [
    Rgb(0xFF, 0x4D, 0x5E),
    Rgb(0xFF, 0x9F, 0x2E),
    Rgb(0xFF, 0xE0, 0x3A),
    Rgb(0x3D, 0xDC, 0x84),
    Rgb(0x3E, 0x9E, 0xFF),
    Rgb(0xB3, 0x6B, 0xFF),
    Rgb(0xFF, 0x6B, 0xD5),
    Rgb(0xFF, 0xFF, 0xFF),
    Rgb(0x16, 0x18, 0x1D),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Move,
    Pen,
    Arrow,
    Line,
    Rect,
    Ellipse,
    Highlight,
    Blur,
    Text,
    Counter,
}

impl Tool {
    /// In the order of the toolbar buttons and of keys 1 to 9, 0.
    pub const ALL: [Tool; 10] = [
        Tool::Move,
        Tool::Pen,
        Tool::Arrow,
        Tool::Line,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Highlight,
        Tool::Blur,
        Tool::Text,
        Tool::Counter,
    ];

    /// `NEED_COLOR`: tools with the options bar.
    fn has_options(self) -> bool {
        !matches!(self, Tool::Move | Tool::Blur)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizeKey {
    S,
    M,
    L,
}

impl SizeKey {
    pub const ALL: [SizeKey; 3] = [SizeKey::S, SizeKey::M, SizeKey::L];

    /// `STROKES`, CSS pixels.
    pub fn stroke(self) -> f64 {
        match self {
            SizeKey::S => 2.2,
            SizeKey::M => 4.0,
            SizeKey::L => 7.0,
        }
    }

    /// `FONTS`, CSS pixels.
    pub fn font(self) -> f64 {
        match self {
            SizeKey::S => 16.0,
            SizeKey::M => 23.0,
            SizeKey::L => 32.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

/// `{x,y,w,h}` in whichever space the field says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// `normRect`.
    pub fn from_corners(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self { x: x1.min(x2), y: y1.min(y2), w: (x2 - x1).abs(), h: (y2 - y1).abs() }
    }

    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

/// Whole pixels of the frozen frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
}

/// `Math.round`: halves go up, unlike `f64::round` on negative numbers.
pub fn js_round(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// `clamp`: when the range is inverted the upper bound wins, which is what keeps oversized chrome on screen.
pub fn clamp(v: f64, low: f64, high: f64) -> f64 {
    high.min(low.max(v))
}

// ---------- annotations ----------

/// Two corners, in image pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    #[serde(default = "default_color")]
    pub color: Rgb,
    /// CSS pixels, as on the toolbar; the renderer scales it.
    #[serde(default = "default_stroke")]
    pub stroke: f64,
}

fn default_color() -> Rgb {
    COLORS[0]
}

fn default_stroke() -> f64 {
    4.0
}

/// A committed or in-progress mark. Geometry in image pixels, stroke and font in CSS pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Annotation {
    Pen {
        points: Vec<Pt>,
        color: Rgb,
        #[serde(default = "default_stroke")]
        stroke: f64,
    },
    Highlight {
        points: Vec<Pt>,
        color: Rgb,
        #[serde(default = "default_stroke")]
        stroke: f64,
    },
    Line(Shape),
    Arrow(Shape),
    Rect(Shape),
    Ellipse(Shape),
    Blur(Shape),
    Text {
        x: f64,
        y: f64,
        lines: Vec<String>,
        color: Rgb,
        #[serde(rename = "fontPx")]
        font_px: f64,
    },
    Counter {
        x: f64,
        y: f64,
        n: u32,
        color: Rgb,
    },
}

impl Annotation {
    /// CSS pixels to image pixels, as the `annotate` test route does. Stroke and font stay in CSS pixels.
    fn scaled(mut self, kx: f64, ky: f64) -> Self {
        match &mut self {
            Annotation::Pen { points, .. } | Annotation::Highlight { points, .. } => {
                for p in points {
                    p.x *= kx;
                    p.y *= ky;
                }
            }
            Annotation::Line(s) | Annotation::Arrow(s) | Annotation::Rect(s) | Annotation::Ellipse(s) | Annotation::Blur(s) => {
                s.x1 *= kx;
                s.x2 *= kx;
                s.y1 *= ky;
                s.y2 *= ky;
            }
            Annotation::Text { x, y, .. } | Annotation::Counter { x, y, .. } => {
                *x *= kx;
                *y *= ky;
            }
        }
        self
    }
}

// ---------- interaction state ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Selecting,
    Selected,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Idle => "idle",
            Mode::Selecting => "selecting",
            Mode::Selected => "selected",
        }
    }
}

/// The eight handles in the order of `HANDLE_NAMES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
}

impl Handle {
    const ALL: [Handle; 8] = [Handle::Nw, Handle::N, Handle::Ne, Handle::E, Handle::Se, Handle::S, Handle::Sw, Handle::W];

    /// Position on a rectangle, as fractions of its size.
    fn fraction(self) -> (f64, f64) {
        match self {
            Handle::Nw => (0.0, 0.0),
            Handle::N => (0.5, 0.0),
            Handle::Ne => (1.0, 0.0),
            Handle::E => (1.0, 0.5),
            Handle::Se => (1.0, 1.0),
            Handle::S => (0.5, 1.0),
            Handle::Sw => (0.0, 1.0),
            Handle::W => (0.0, 0.5),
        }
    }

    fn cursor(self) -> Cursor {
        match self {
            Handle::Nw | Handle::Se => Cursor::NwseResize,
            Handle::Ne | Handle::Sw => Cursor::NeswResize,
            Handle::N | Handle::S => Cursor::NsResize,
            Handle::E | Handle::W => Cursor::EwResize,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cursor {
    Crosshair,
    Move,
    Text,
    Progress,
    NwseResize,
    NeswResize,
    NsResize,
    EwResize,
}

#[derive(Clone, Debug, PartialEq)]
enum Drag {
    Create { sx: f64, sy: f64, s_css_x: f64, s_css_y: f64 },
    Move { off_x: f64, off_y: f64 },
    Resize { ax: Option<f64>, ay: Option<f64>, r0: RectF },
    Draw,
}

/// The text editor while it is open. Its text lives here so committing stays pure.
#[derive(Clone, Debug, PartialEq)]
pub struct Editor {
    pub x: f64,
    pub y: f64,
    img_x: f64,
    img_y: f64,
    pub text: String,
}

/// One recognized word, in CSS pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub line: usize,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct Rubber {
    sx: f64,
    sy: f64,
    base: BTreeSet<usize>,
}

/// The floating message of `flash`.
#[derive(Clone, Debug, PartialEq)]
pub struct Flash {
    pub text: String,
    /// Horizontal centre and top, CSS pixels.
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranslateCard {
    pub loading: bool,
    pub body: String,
    pub meta: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Place {
    pub x: f64,
    pub y: f64,
    /// Tooltips open below the bar: there is no room above.
    pub tips_below: bool,
}

/// What the translation request returned.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Translated {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
}

// Chrome sizes from `overlay.css`, the `offsetWidth` and `offsetHeight` the JS measures. `overlay.slint`
// lays out to the same numbers and `view.rs` checks that at runtime.
pub const TOOLBAR_W: f64 = 720.0;
pub const TOOLBAR_H: f64 = 46.0;
pub const OPTIONS_W: f64 = 372.0;
pub const OPTIONS_H: f64 = 44.0;
pub const TEXT_TOOLBAR_W: f64 = 167.0;
pub const TEXT_TOOLBAR_H: f64 = 46.0;

pub const FLASH_MS: u64 = 1400;

/// The screen-space state of one overlay window.
#[derive(Clone, Debug)]
pub struct State {
    pub for_claude: bool,
    pub for_text: bool,
    pub for_record: bool,
    i18n: BTreeMap<String, String>,

    pub ready: bool,
    pub img_w: f64,
    pub img_h: f64,
    pub view_w: f64,
    pub view_h: f64,
    pub kx: f64,
    pub ky: f64,

    pub mode: Mode,
    /// Image pixels, floats while dragging.
    pub sel: Option<RectF>,
    snap_windows: Vec<RectF>,
    /// CSS pixels.
    pub hover_rect: Option<RectF>,
    pub alt_down: bool,
    pub shift_down: bool,
    drag: Option<Drag>,
    pub tool: Tool,
    pub color: Rgb,
    pub size: SizeKey,
    pub annotations: Vec<Annotation>,
    pub redo_stack: Vec<Annotation>,
    pub counter_n: u32,
    pub temp: Option<Annotation>,
    /// CSS pixels; off-window until the first pointer move.
    pub mouse: Pt,
    pub editor: Option<Editor>,
    pub cursor: Cursor,

    pub toolbar: Option<Place>,
    pub options: Option<Place>,
    pub text_toolbar: Option<Place>,
    pub badge_visible: bool,

    pub text_mode: bool,
    pub words: Vec<Word>,
    pub sel_words: BTreeSet<usize>,
    pub hover_word: Option<usize>,
    pub words_visible: bool,
    rubber: Option<Rubber>,
    pub rubber_rect: Option<RectF>,
    ocr_generation: u64,
    ocr_rect: IRect,

    pub flash: Option<Flash>,
    flash_generation: u64,
    pub card: Option<TranslateCard>,
    translate_generation: u64,

    /// A session-ending action went out: nothing more happens here.
    pub closing: bool,
}

/// What one window asks of the outside after an action.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// The annotation layer changed.
    Layer(LayerOp),
    /// Ends the session with nothing.
    Cancel,
    /// Ends the session with this choice.
    Finish(Finish),
    /// `C`: copy the colour under the cursor, then cancel.
    PickColor { ix: i32, iy: i32 },
    /// The `copy-color` test route: the colour effect alone.
    CopyColor(String),
    /// Recognize this crop of the pristine frame and answer with `Action::OcrDone`.
    Recognize { generation: u64, rect: IRect },
    /// Translate and answer with `Action::TranslateDone`.
    Translate { generation: u64, text: String },
    CopyText(String),
    /// Answer with `Action::FlashExpired` after this long.
    ArmFlash { generation: u64, ms: u64 },
    /// The text editor opened: give it the keyboard.
    FocusEditor,
    /// The translation card's text changed: answer with `Action::CardMeasured`.
    MeasureCard,
}

/// How the annotation layer has to change. The layer keeps pixels, so it is told what happened instead of
/// comparing lists. Everything is already applied to `State` when these go out.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerOp {
    /// `State::temp` got points from this index on (a pen or highlighter drag).
    LiveExtended { from: usize },
    /// `State::temp` changed its shape.
    LiveChanged,
    /// `State::temp` went away without becoming an annotation.
    LiveDropped,
    /// `State::temp` became the last annotation.
    LiveCommitted,
    /// An annotation was added on top, without having been live.
    Added,
    /// This annotation left the list (undo).
    Removed(Annotation),
    /// The whole list was replaced.
    Replaced,
}

/// The user's choice. `render_png`: burn the annotations in and encode the selection.
#[derive(Clone, Debug, PartialEq)]
pub struct Finish {
    pub action: OverlayActionKind,
    pub rect_phys: Option<IRect>,
    pub rect_css: Option<RectF>,
    pub text: Option<String>,
    pub render_png: bool,
}

/// The toolbar buttons that are not tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Ocr,
    Pin,
    Rec,
    Save,
    Claude,
    Copy,
    Cancel,
}

/// Everything that can happen to the state. JSON through the `ui-action` test route.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Action {
    PointerDown {
        x: f64,
        y: f64,
        #[serde(default)]
        shift: bool,
    },
    PointerMove {
        x: f64,
        y: f64,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        alt: bool,
    },
    PointerUp {
        x: f64,
        y: f64,
        #[serde(default)]
        shift: bool,
    },
    DoubleClick {
        x: f64,
        y: f64,
    },
    Key {
        code: Code,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        shift: bool,
    },
    KeyUp {
        code: Code,
    },
    SetTool {
        tool: Tool,
    },
    SetColor {
        color: Rgb,
    },
    SetSize {
        size: SizeKey,
    },
    Undo,
    Redo,
    Button {
        button: Button,
    },
    TextCopy,
    TextAll,
    TextTranslate,
    TextBack,
    TranslateClose,
    TranslateCopy,
    EditorInput {
        text: String,
    },
    EditorCommit,
    EditorCancel,
    /// Hover-snap rectangles in CSS pixels, topmost first.
    SetWindows {
        rects: Vec<RectF>,
    },
    OcrDone {
        generation: u64,
        boxes: OcrBoxes,
    },
    TranslateDone {
        generation: u64,
        /// None: the request failed.
        result: Option<Translated>,
    },
    CardMeasured {
        height: f64,
    },
    FlashExpired {
        generation: u64,
    },
    /// Test route: local CSS rectangle, enters `selected`.
    Select {
        rect: RectF,
    },
    /// Test route: replaces the annotations; geometry in CSS pixels.
    Annotate {
        annotations: Vec<Annotation>,
    },
    /// Test route: same as the OCR button.
    LiveText,
    /// Test route: same as the Select All button.
    TextSelectAll,
    /// Test route: the colour effect alone.
    CopyColor {
        hex: String,
    },
}

/// Applies an action.
pub fn reduce(state: &mut State, action: Action) -> Vec<Effect> {
    let mut fx = Vec::new();
    if !state.closing {
        state.apply(action, &mut fx);
    }
    fx
}

impl State {
    pub fn new(mode: OverlayMode, image: (u32, u32), viewport: (f64, f64), i18n: BTreeMap<String, String>) -> Self {
        let img_w = f64::from(image.0);
        let img_h = f64::from(image.1);
        Self {
            for_claude: mode == OverlayMode::Claude,
            for_text: mode == OverlayMode::Text,
            for_record: mode == OverlayMode::Record,
            i18n,
            ready: false,
            img_w,
            img_h,
            view_w: viewport.0,
            view_h: viewport.1,
            kx: img_w / viewport.0,
            ky: img_h / viewport.1,
            mode: Mode::Idle,
            sel: None,
            snap_windows: Vec::new(),
            hover_rect: None,
            alt_down: false,
            shift_down: false,
            drag: None,
            tool: Tool::Move,
            color: COLORS[0],
            size: SizeKey::M,
            annotations: Vec::new(),
            redo_stack: Vec::new(),
            counter_n: 1,
            temp: None,
            mouse: Pt { x: -100.0, y: -100.0 },
            editor: None,
            cursor: Cursor::Crosshair,
            toolbar: None,
            options: None,
            text_toolbar: None,
            badge_visible: mode == OverlayMode::Claude,
            text_mode: false,
            words: Vec::new(),
            sel_words: BTreeSet::new(),
            hover_word: None,
            words_visible: false,
            rubber: None,
            rubber_rect: None,
            ocr_generation: 0,
            ocr_rect: IRect::default(),
            flash: None,
            flash_generation: 0,
            card: None,
            translate_generation: 0,
            closing: false,
        }
    }

    fn apply(&mut self, action: Action, fx: &mut Vec<Effect>) {
        match action {
            Action::PointerDown { x, y, shift } => self.pointer_down(x, y, shift, fx),
            Action::PointerMove { x, y, shift, alt } => self.pointer_move(x, y, shift, alt, fx),
            Action::PointerUp { x, y, shift } => self.pointer_up(x, y, shift, fx),
            Action::DoubleClick { x, y } => self.double_click(x, y, fx),
            Action::Key { code, ctrl, shift } => self.key_down(code, ctrl, shift, fx),
            Action::KeyUp { code } => self.key_up(code),
            Action::SetTool { tool } => self.set_tool(tool, fx),
            Action::SetColor { color } => self.color = color,
            Action::SetSize { size } => self.size = size,
            Action::Undo => self.undo(fx),
            Action::Redo => self.redo(fx),
            Action::Button { button } => match button {
                Button::Ocr => self.enter_text_mode(fx),
                Button::Pin => self.do_action(OverlayActionKind::Pin, fx),
                Button::Rec => self.do_action(OverlayActionKind::Record, fx),
                Button::Save => self.do_action(OverlayActionKind::Save, fx),
                Button::Claude => self.do_action(OverlayActionKind::Claude, fx),
                Button::Copy => self.do_action(OverlayActionKind::Copy, fx),
                Button::Cancel => self.cancel(fx),
            },
            Action::TextCopy => self.copy_text_selection(true, fx),
            Action::TextAll | Action::TextSelectAll => self.select_all_words(),
            Action::TextTranslate => self.translate_selection(fx),
            Action::TextBack => self.exit_text_mode(),
            Action::TranslateClose => self.card = None,
            Action::TranslateCopy => {
                if let Some(card) = &self.card {
                    fx.push(Effect::CopyText(card.body.clone()));
                    self.flash("copiedTitle", fx);
                }
            }
            Action::EditorInput { text } => {
                if let Some(editor) = &mut self.editor {
                    editor.text = text;
                }
            }
            Action::EditorCommit => self.commit_text(fx),
            Action::EditorCancel => self.editor = None,
            Action::SetWindows { rects } => {
                self.snap_windows = rects;
                self.update_hover();
            }
            Action::OcrDone { generation, boxes } => self.ocr_done(generation, boxes, fx),
            Action::TranslateDone { generation, result } => self.translate_done(generation, result, fx),
            Action::CardMeasured { height } => {
                if let Some(card) = &mut self.card {
                    card.height = height;
                }
                self.position_translate_card();
            }
            Action::FlashExpired { generation } => {
                if generation == self.flash_generation {
                    self.flash = None;
                }
            }
            Action::Select { rect } => {
                let p = self.to_img(rect.x, rect.y);
                let q = self.to_img(rect.x + rect.w, rect.y + rect.h);
                self.sel = Some(RectF::from_corners(p.x, p.y, q.x, q.y));
                self.mode = Mode::Selected;
                self.position_toolbar();
            }
            Action::Annotate { annotations } => {
                let (kx, ky) = (self.kx, self.ky);
                self.annotations = annotations.into_iter().map(|a| a.scaled(kx, ky)).collect();
                fx.push(Effect::Layer(LayerOp::Replaced));
            }
            Action::LiveText => self.enter_text_mode(fx),
            Action::CopyColor { hex } => fx.push(Effect::CopyColor(hex)),
        }
    }

    /// `I18N[k] || k`.
    fn t(&self, key: &str) -> String {
        self.i18n.get(key).cloned().unwrap_or_else(|| key.to_string())
    }

    // ---------- geometry helpers ----------

    fn to_img(&self, css_x: f64, css_y: f64) -> Pt {
        Pt { x: clamp(css_x * self.kx, 0.0, self.img_w), y: clamp(css_y * self.ky, 0.0, self.img_h) }
    }

    /// `selCss`.
    pub fn sel_css(&self) -> Option<RectF> {
        self.sel.map(|s| RectF::new(s.x / self.kx, s.y / self.ky, s.w / self.kx, s.h / self.ky))
    }

    /// `roundSel`.
    pub fn round_sel(&self) -> Option<IRect> {
        self.sel.map(|s| IRect {
            x: js_round(s.x) as i32,
            y: js_round(s.y) as i32,
            w: js_round(s.w).max(1.0) as i32,
            h: js_round(s.h).max(1.0) as i32,
        })
    }

    fn inside_sel(&self, ix: f64, iy: f64) -> bool {
        self.sel.is_some_and(|s| s.contains(ix, iy))
    }

    /// `handleAt`: the handle under a CSS point, within 7 pixels.
    pub fn handle_at(&self, css_x: f64, css_y: f64) -> Option<Handle> {
        if self.mode != Mode::Selected || self.tool != Tool::Move {
            return None;
        }
        let r = self.sel_css()?;
        Handle::ALL.into_iter().find(|h| {
            let (fx, fy) = h.fraction();
            (css_x - (r.x + r.w * fx)).abs() <= 7.0 && (css_y - (r.y + r.h * fy)).abs() <= 7.0
        })
    }

    fn update_hover(&mut self) {
        self.hover_rect = None;
        if self.mode != Mode::Idle || self.alt_down || self.shift_down || self.mouse.x < 0.0 {
            return;
        }
        let (mx, my) = (self.mouse.x, self.mouse.y);
        if let Some(hit) = self.snap_windows.iter().find(|r| r.contains(mx, my)) {
            let x1 = hit.x.max(0.0);
            let y1 = hit.y.max(0.0);
            let x2 = self.view_w.min(hit.x + hit.w);
            let y2 = self.view_h.min(hit.y + hit.h);
            if x2 - x1 > 8.0 && y2 - y1 > 8.0 {
                self.hover_rect = Some(RectF::new(x1, y1, x2 - x1, y2 - y1));
            }
        }
    }

    // ---------- pointer ----------

    fn pointer_down(&mut self, x: f64, y: f64, shift: bool, fx: &mut Vec<Effect>) {
        if self.text_mode {
            self.text_down(x, y, shift);
            return;
        }
        if self.editor.is_some() {
            self.commit_text(fx);
            return;
        }
        let ip = self.to_img(x, y);
        match self.mode {
            Mode::Idle => self.begin_create(x, y, ip),
            Mode::Selecting => {}
            Mode::Selected => {
                if self.tool == Tool::Move {
                    if let Some(handle) = self.handle_at(x, y) {
                        let r = self.round_sel().unwrap_or_default();
                        let (left, top) = (f64::from(r.x), f64::from(r.y));
                        let (right, bottom) = (left + f64::from(r.w), top + f64::from(r.h));
                        let (ax, ay) = match handle {
                            Handle::Nw => (Some(right), Some(bottom)),
                            Handle::Ne => (Some(left), Some(bottom)),
                            Handle::Se => (Some(left), Some(top)),
                            Handle::Sw => (Some(right), Some(top)),
                            Handle::N => (None, Some(bottom)),
                            Handle::S => (None, Some(top)),
                            Handle::E => (Some(left), None),
                            Handle::W => (Some(right), None),
                        };
                        let r0 = RectF::new(left, top, f64::from(r.w), f64::from(r.h));
                        self.drag = Some(Drag::Resize { ax, ay, r0 });
                        self.hide_toolbars();
                    } else if self.inside_sel(ip.x, ip.y) {
                        let s = self.sel.unwrap_or_default();
                        self.drag = Some(Drag::Move { off_x: ip.x - s.x, off_y: ip.y - s.y });
                        self.hide_toolbars();
                    } else {
                        self.begin_create(x, y, ip);
                    }
                    return;
                }
                match self.tool {
                    Tool::Text => self.open_text_editor(x, y, ip, fx),
                    Tool::Counter => {
                        let n = self.counter_n;
                        self.counter_n += 1;
                        self.push_anno(Annotation::Counter { x: ip.x, y: ip.y, n, color: self.color }, LayerOp::Added, fx);
                    }
                    tool => {
                        self.drag = Some(Drag::Draw);
                        let (color, stroke) = (self.color, self.size.stroke());
                        self.temp = Some(match tool {
                            Tool::Pen => Annotation::Pen { points: vec![ip], color, stroke },
                            Tool::Highlight => Annotation::Highlight { points: vec![ip], color, stroke },
                            other => {
                                let shape = Shape { x1: ip.x, y1: ip.y, x2: ip.x, y2: ip.y, color, stroke };
                                match other {
                                    Tool::Arrow => Annotation::Arrow(shape),
                                    Tool::Line => Annotation::Line(shape),
                                    Tool::Rect => Annotation::Rect(shape),
                                    Tool::Ellipse => Annotation::Ellipse(shape),
                                    _ => Annotation::Blur(shape),
                                }
                            }
                        });
                        fx.push(Effect::Layer(LayerOp::LiveChanged));
                    }
                }
            }
        }
    }

    fn begin_create(&mut self, css_x: f64, css_y: f64, ip: Pt) {
        self.drag = Some(Drag::Create { sx: ip.x, sy: ip.y, s_css_x: css_x, s_css_y: css_y });
        self.sel = Some(RectF::new(ip.x, ip.y, 0.0, 0.0));
        self.mode = Mode::Selecting;
        self.hide_toolbars();
    }

    fn hide_toolbars(&mut self) {
        self.toolbar = None;
        self.options = None;
    }

    fn pointer_move(&mut self, x: f64, y: f64, shift: bool, alt: bool, fx: &mut Vec<Effect>) {
        self.mouse = Pt { x, y };
        if self.text_mode {
            self.text_move(x, y);
            return;
        }
        let ip = self.to_img(x, y);
        if let Some(drag) = self.drag.clone() {
            match drag {
                Drag::Create { sx, sy, .. } => self.sel = Some(RectF::from_corners(sx, sy, ip.x, ip.y)),
                Drag::Move { off_x, off_y } => {
                    if let Some(mut s) = self.sel {
                        s.x = clamp(ip.x - off_x, 0.0, self.img_w - s.w);
                        s.y = clamp(ip.y - off_y, 0.0, self.img_h - s.h);
                        self.sel = Some(s);
                    }
                }
                Drag::Resize { ax, ay, r0 } => {
                    let x1 = ax.unwrap_or(r0.x);
                    let y1 = ay.unwrap_or(r0.y);
                    let x2 = if ax.is_none() { r0.x + r0.w } else { ip.x };
                    let y2 = if ay.is_none() { r0.y + r0.h } else { ip.y };
                    let mut r = RectF::from_corners(x1, y1, x2, y2);
                    if ax.is_none() {
                        r.x = r0.x;
                        r.w = r0.w;
                    }
                    if ay.is_none() {
                        r.y = r0.y;
                        r.h = r0.h;
                    }
                    self.sel = Some(r);
                }
                Drag::Draw => self.draw_move(ip, shift, fx),
            }
            return;
        }

        self.cursor = if self.mode == Mode::Selected && self.tool == Tool::Move {
            match self.handle_at(x, y) {
                Some(handle) => handle.cursor(),
                None if self.inside_sel(ip.x, ip.y) => Cursor::Move,
                None => Cursor::Crosshair,
            }
        } else if self.mode == Mode::Selected && self.tool == Tool::Text {
            Cursor::Text
        } else {
            Cursor::Crosshair
        };
        if self.mode == Mode::Idle {
            self.alt_down = alt;
            self.shift_down = shift;
            self.update_hover();
        }
    }

    fn draw_move(&mut self, ip: Pt, shift: bool, fx: &mut Vec<Effect>) {
        let min_step = 2.0 * self.kx;
        match &mut self.temp {
            Some(Annotation::Pen { points, .. } | Annotation::Highlight { points, .. }) => {
                let last = points[points.len() - 1];
                if (ip.x - last.x).hypot(ip.y - last.y) > min_step {
                    points.push(ip);
                    fx.push(Effect::Layer(LayerOp::LiveExtended { from: points.len() - 1 }));
                }
            }
            Some(temp) => {
                let snap_angle = matches!(temp, Annotation::Line(_) | Annotation::Arrow(_));
                let snap_square = matches!(temp, Annotation::Rect(_) | Annotation::Ellipse(_));
                if let Annotation::Line(s) | Annotation::Arrow(s) | Annotation::Rect(s) | Annotation::Ellipse(s) | Annotation::Blur(s) = temp {
                    let (mut x2, mut y2) = (ip.x, ip.y);
                    if shift && snap_angle {
                        let (dx, dy) = (x2 - s.x1, y2 - s.y1);
                        let quarter = std::f64::consts::FRAC_PI_4;
                        let angle = js_round(dy.atan2(dx) / quarter) * quarter;
                        let length = dx.hypot(dy);
                        x2 = s.x1 + length * angle.cos();
                        y2 = s.y1 + length * angle.sin();
                    }
                    if shift && snap_square {
                        let side = (ip.x - s.x1).abs().max((ip.y - s.y1).abs());
                        let sign = |v: f64| if v < 0.0 { -1.0 } else { 1.0 };
                        x2 = s.x1 + sign(ip.x - s.x1) * side;
                        y2 = s.y1 + sign(ip.y - s.y1) * side;
                    }
                    s.x2 = x2;
                    s.y2 = y2;
                    fx.push(Effect::Layer(LayerOp::LiveChanged));
                }
            }
            None => {}
        }
    }

    fn pointer_up(&mut self, x: f64, y: f64, shift: bool, fx: &mut Vec<Effect>) {
        if self.text_mode {
            self.text_up();
            return;
        }
        let Some(drag) = self.drag.take() else { return };
        match drag {
            Drag::Create { s_css_x, s_css_y, .. } => {
                if (x - s_css_x).hypot(y - s_css_y) < 4.0 {
                    // A click: Shift means the whole screen; otherwise the hovered window, else the whole screen.
                    self.sel = match (shift, self.hover_rect) {
                        (false, Some(h)) => Some(RectF::new(h.x * self.kx, h.y * self.ky, h.w * self.kx, h.h * self.ky)),
                        _ => Some(RectF::new(0.0, 0.0, self.img_w, self.img_h)),
                    };
                    self.hover_rect = None;
                } else if self.sel.is_none_or(|s| s.w < 3.0 || s.h < 3.0) {
                    self.sel = None;
                    self.mode = Mode::Idle;
                    return;
                }
                self.mode = Mode::Selected;
                if self.for_record {
                    self.finish_record(fx);
                    return;
                }
                if self.for_text {
                    self.enter_text_mode(fx);
                    return;
                }
                self.position_toolbar();
            }
            Drag::Draw => {
                if let Some(temp) = self.temp.take() {
                    let significant = match &temp {
                        Annotation::Pen { points, .. } | Annotation::Highlight { points, .. } => points.len() > 1,
                        Annotation::Line(s) | Annotation::Arrow(s) | Annotation::Rect(s) | Annotation::Ellipse(s) | Annotation::Blur(s) => {
                            (s.x2 - s.x1).abs() > 3.0 || (s.y2 - s.y1).abs() > 3.0
                        }
                        _ => false,
                    };
                    if significant {
                        self.push_anno(temp, LayerOp::LiveCommitted, fx);
                    } else {
                        fx.push(Effect::Layer(LayerOp::LiveDropped));
                    }
                }
            }
            Drag::Move { .. } | Drag::Resize { .. } => self.position_toolbar(),
        }
    }

    fn double_click(&mut self, x: f64, y: f64, fx: &mut Vec<Effect>) {
        if self.mode != Mode::Selected {
            return;
        }
        let ip = self.to_img(x, y);
        if self.inside_sel(ip.x, ip.y) {
            self.do_action(OverlayActionKind::Copy, fx);
        }
    }

    fn push_anno(&mut self, annotation: Annotation, op: LayerOp, fx: &mut Vec<Effect>) {
        self.annotations.push(annotation);
        self.redo_stack.clear();
        fx.push(Effect::Layer(op));
    }

    fn undo(&mut self, fx: &mut Vec<Effect>) {
        let Some(annotation) = self.annotations.pop() else { return };
        if matches!(annotation, Annotation::Counter { .. }) {
            self.counter_n = self.counter_n.saturating_sub(1).max(1);
        }
        self.redo_stack.push(annotation.clone());
        fx.push(Effect::Layer(LayerOp::Removed(annotation)));
    }

    fn redo(&mut self, fx: &mut Vec<Effect>) {
        let Some(annotation) = self.redo_stack.pop() else { return };
        if let Annotation::Counter { n, .. } = annotation {
            self.counter_n = n + 1;
        }
        self.annotations.push(annotation);
        fx.push(Effect::Layer(LayerOp::Added));
    }

    // ---------- toolbars ----------

    /// `positionToolbar`.
    fn position_toolbar(&mut self) {
        let rc = match self.sel_css() {
            Some(rc) if self.mode == Mode::Selected && !self.text_mode => rc,
            _ => {
                self.hide_toolbars();
                return;
            }
        };
        let (tw, th, m) = (TOOLBAR_W, TOOLBAR_H, 10.0);
        let x = clamp(rc.x + rc.w / 2.0 - tw / 2.0, 8.0, self.view_w - tw - 8.0);
        let mut y = rc.y + rc.h + m;
        if y + th > self.view_h - 8.0 {
            y = rc.y - th - m;
        }
        if y < 8.0 {
            y = 8.0_f64.max((self.view_h - th - 8.0).min(rc.y + rc.h - th - m - 8.0));
        }
        self.toolbar = Some(Place { x, y, tips_below: y < 56.0 });

        self.options = if self.tool.has_options() {
            let mut oy = y + th + 8.0;
            if oy + OPTIONS_H > self.view_h - 8.0 {
                oy = y - OPTIONS_H - 8.0;
            }
            Some(Place { x: clamp(x, 8.0, self.view_w - OPTIONS_W - 8.0), y: oy, tips_below: false })
        } else {
            None
        };
    }

    fn set_tool(&mut self, tool: Tool, fx: &mut Vec<Effect>) {
        if self.editor.is_some() {
            self.commit_text(fx);
        }
        self.tool = tool;
        self.position_toolbar();
    }

    // ---------- text editor ----------

    fn open_text_editor(&mut self, css_x: f64, css_y: f64, ip: Pt, fx: &mut Vec<Effect>) {
        self.editor = Some(Editor { x: css_x, y: css_y, img_x: ip.x, img_y: ip.y, text: String::new() });
        fx.push(Effect::FocusEditor);
    }

    /// `commitText`.
    fn commit_text(&mut self, fx: &mut Vec<Effect>) {
        let Some(editor) = self.editor.take() else { return };
        let raw = editor.text.replace('\u{a0}', " ");
        let parts: Vec<&str> = raw.split('\n').map(|s| s.trim_end()).collect();
        let last = parts.len().saturating_sub(1);
        let lines: Vec<String> = parts
            .iter()
            .enumerate()
            .filter(|(i, s)| !s.is_empty() || *i < last)
            .map(|(_, s)| (*s).to_string())
            .collect();
        if lines.iter().any(|s| !s.trim().is_empty()) {
            let annotation = Annotation::Text {
                x: editor.img_x,
                y: editor.img_y,
                lines,
                color: self.color,
                font_px: self.size.font(),
            };
            self.push_anno(annotation, LayerOp::Added, fx);
        }
    }

    // ---------- Live Text ----------

    /// `enterTextMode`.
    fn enter_text_mode(&mut self, fx: &mut Vec<Effect>) {
        if self.text_mode || self.mode != Mode::Selected {
            return;
        }
        let Some(rect) = self.round_sel() else { return };
        self.text_mode = true;
        self.hide_toolbars();
        self.badge_visible = false;
        self.cursor = Cursor::Progress;
        self.flash("txtRecognizing", fx);
        self.ocr_generation += 1;
        self.ocr_rect = rect;
        fx.push(Effect::Recognize { generation: self.ocr_generation, rect });
    }

    fn ocr_done(&mut self, generation: u64, boxes: OcrBoxes, fx: &mut Vec<Effect>) {
        // The JS keeps going after leaving Live Text and paints word boxes on the normal view. A stale
        // answer means the user went back, so it is dropped.
        if !self.text_mode || generation != self.ocr_generation {
            return;
        }
        self.cursor = Cursor::Text;
        let r = self.ocr_rect;
        self.words.clear();
        for (line, l) in boxes.lines.iter().enumerate() {
            for w in &l.words {
                self.words.push(Word {
                    text: w.text.clone(),
                    line,
                    x: (f64::from(r.x) + w.x) / self.kx,
                    y: (f64::from(r.y) + w.y) / self.ky,
                    w: w.w / self.kx,
                    h: w.h / self.ky,
                });
            }
        }
        self.sel_words.clear();
        self.words_visible = true;
        if self.words.is_empty() {
            self.flash("ocrNoText", fx);
        }
        self.position_text_toolbar();
    }

    /// `exitTextMode`.
    fn exit_text_mode(&mut self) {
        self.text_mode = false;
        self.words_visible = false;
        self.text_toolbar = None;
        self.card = None;
        self.words.clear();
        self.sel_words.clear();
        self.hover_word = None;
        self.rubber = None;
        self.rubber_rect = None;
        self.cursor = Cursor::Crosshair;
        self.position_toolbar();
    }

    fn word_at(&self, x: f64, y: f64) -> Option<usize> {
        self.words.iter().rposition(|w| x >= w.x && x <= w.x + w.w && y >= w.y && y <= w.y + w.h)
    }

    fn text_down(&mut self, x: f64, y: f64, shift: bool) {
        self.card = None;
        match self.word_at(x, y) {
            Some(i) if !shift => self.sel_words = BTreeSet::from([i]),
            None if !shift => self.sel_words.clear(),
            _ => {}
        }
        self.rubber = Some(Rubber { sx: x, sy: y, base: self.sel_words.clone() });
        self.words_visible = true;
    }

    fn text_move(&mut self, x: f64, y: f64) {
        self.hover_word = self.word_at(x, y);
        let Some(rubber) = &self.rubber else { return };
        let band = RectF::new(rubber.sx.min(x), rubber.sy.min(y), (x - rubber.sx).abs(), (y - rubber.sy).abs());
        let mut next = rubber.base.clone();
        for (i, w) in self.words.iter().enumerate() {
            if w.x < band.x + band.w && w.x + w.w > band.x && w.y < band.y + band.h && w.y + w.h > band.y {
                next.insert(i);
            }
        }
        self.rubber_rect = Some(band);
        self.sel_words = next;
        self.words_visible = true;
    }

    fn text_up(&mut self) {
        self.rubber = None;
        self.rubber_rect = None;
        self.position_text_toolbar();
    }

    /// `selectedText`: the selection, or every word when nothing is selected; lines in order, words by x.
    pub fn selected_text(&self) -> String {
        let chosen: Vec<&Word> = if self.sel_words.is_empty() {
            self.words.iter().collect()
        } else {
            self.sel_words.iter().filter_map(|&i| self.words.get(i)).collect()
        };
        let mut lines: BTreeMap<usize, Vec<&Word>> = BTreeMap::new();
        for word in chosen {
            lines.entry(word.line).or_default().push(word);
        }
        let mut out = Vec::with_capacity(lines.len());
        for words in lines.values_mut() {
            words.sort_by(|a, b| a.x.total_cmp(&b.x));
            out.push(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "));
        }
        out.join("\n")
    }

    fn position_text_toolbar(&mut self) {
        if !self.text_mode {
            return;
        }
        let Some(rc) = self.sel_css() else { return };
        let (tw, th, m) = (TEXT_TOOLBAR_W, TEXT_TOOLBAR_H, 10.0);
        let x = clamp(rc.x + rc.w / 2.0 - tw / 2.0, 8.0, self.view_w - tw - 8.0);
        let mut y = rc.y + rc.h + m;
        if y + th > self.view_h - 8.0 {
            y = 8.0_f64.max(rc.y - th - m);
        }
        self.text_toolbar = Some(Place { x, y, tips_below: y < 56.0 });
    }

    /// `flash`: a message that goes away by itself. Always re-arms; older timers find a newer generation.
    fn flash(&mut self, key: &str, fx: &mut Vec<Effect>) {
        let rc = self.sel_css().unwrap_or(RectF::new(self.view_w / 2.0, 40.0, 0.0, 0.0));
        self.flash = Some(Flash {
            text: self.t(key),
            x: clamp(rc.x + rc.w / 2.0, 80.0, self.view_w - 80.0),
            y: 20.0_f64.max(rc.y - 40.0),
        });
        self.flash_generation += 1;
        fx.push(Effect::ArmFlash { generation: self.flash_generation, ms: FLASH_MS });
    }

    fn copy_text_selection(&mut self, close: bool, fx: &mut Vec<Effect>) {
        let text = self.selected_text();
        if text.is_empty() {
            self.flash("ocrNoText", fx);
            return;
        }
        if close {
            self.closing = true;
            fx.push(Effect::Finish(Finish {
                action: OverlayActionKind::Copytext,
                rect_phys: None,
                rect_css: None,
                text: Some(text),
                render_png: false,
            }));
        } else {
            fx.push(Effect::CopyText(text));
            self.flash("copiedTitle", fx);
        }
    }

    fn select_all_words(&mut self) {
        self.sel_words = (0..self.words.len()).collect();
    }

    fn translate_selection(&mut self, fx: &mut Vec<Effect>) {
        let text = self.selected_text();
        if text.is_empty() {
            self.flash("ocrNoText", fx);
            return;
        }
        self.translate_generation += 1;
        let height = self.card.as_ref().map_or(0.0, |c| c.height);
        self.card = Some(TranslateCard {
            loading: true,
            body: self.t("txtTranslating"),
            meta: String::new(),
            x: 0.0,
            y: 0.0,
            width: 440.0_f64.min(self.view_w * 0.46),
            height,
        });
        self.position_translate_card();
        fx.push(Effect::MeasureCard);
        fx.push(Effect::Translate { generation: self.translate_generation, text });
    }

    fn translate_done(&mut self, generation: u64, result: Option<Translated>, fx: &mut Vec<Effect>) {
        if generation != self.translate_generation {
            return;
        }
        let failed = self.t("txtTranslateFail");
        let Some(card) = &mut self.card else { return };
        card.loading = false;
        match result {
            Some(res) => {
                card.body = if res.text.is_empty() { "—".to_string() } else { res.text };
                card.meta = match res.source.as_deref() {
                    Some(source) if !source.is_empty() && source != "auto" => {
                        format!("{source} → {}", res.target.unwrap_or_default())
                    }
                    _ => String::new(),
                };
            }
            None => card.body = failed,
        }
        self.position_translate_card();
        fx.push(Effect::MeasureCard);
    }

    /// `positionTranslateCard`.
    fn position_translate_card(&mut self) {
        let Some(rc) = self.sel_css() else { return };
        let (view_w, view_h) = (self.view_w, self.view_h);
        let Some(card) = &mut self.card else { return };
        let m = 12.0;
        let mut x = clamp(rc.x + rc.w + m, 8.0, view_w - card.width - 8.0);
        if rc.x + rc.w + m + card.width > view_w {
            x = clamp(rc.x - card.width - m, 8.0, view_w - card.width - 8.0);
        }
        card.x = x;
        card.y = clamp(rc.y, 8.0, view_h - card.height - 8.0);
    }

    // ---------- keyboard ----------

    fn key_down(&mut self, code: Code, ctrl: bool, shift: bool, fx: &mut Vec<Effect>) {
        if self.editor.is_some() {
            return;
        }
        if self.text_mode {
            match code {
                Code::Escape => {
                    if self.card.is_some() {
                        self.card = None;
                    } else {
                        self.exit_text_mode();
                    }
                }
                Code::KeyA if ctrl => self.select_all_words(),
                Code::KeyC if ctrl => self.copy_text_selection(false, fx),
                Code::Enter => self.copy_text_selection(true, fx),
                Code::KeyR => self.translate_selection(fx),
                _ => {}
            }
            return;
        }
        match code {
            Code::Alt => {
                if !self.alt_down {
                    self.alt_down = true;
                    self.update_hover();
                }
                return;
            }
            Code::Shift if self.mode == Mode::Idle => {
                if !self.shift_down {
                    self.shift_down = true;
                    self.update_hover();
                }
                return;
            }
            Code::Escape => {
                if self.drag.take().is_some() {
                    if self.temp.take().is_some() {
                        fx.push(Effect::Layer(LayerOp::LiveDropped));
                    }
                    if self.mode == Mode::Selecting {
                        self.sel = None;
                        self.mode = Mode::Idle;
                    }
                    return;
                }
                self.cancel(fx);
                return;
            }
            Code::Enter if self.mode == Mode::Selected => {
                self.do_action(OverlayActionKind::Copy, fx);
                return;
            }
            Code::KeyC if ctrl => {
                self.do_action(OverlayActionKind::Copy, fx);
                return;
            }
            Code::KeyS if ctrl => {
                self.do_action(OverlayActionKind::Save, fx);
                return;
            }
            Code::KeyZ if ctrl => {
                if shift {
                    self.redo(fx);
                } else {
                    self.undo(fx);
                }
                return;
            }
            Code::KeyY if ctrl => {
                self.redo(fx);
                return;
            }
            Code::KeyC if self.mode == Mode::Idle => {
                let ix = clamp(js_round(self.mouse.x * self.kx), 0.0, self.img_w - 1.0) as i32;
                let iy = clamp(js_round(self.mouse.y * self.ky), 0.0, self.img_h - 1.0) as i32;
                self.closing = true;
                fx.push(Effect::PickColor { ix, iy });
                return;
            }
            Code::KeyF => {
                self.sel = Some(RectF::new(0.0, 0.0, self.img_w, self.img_h));
                self.mode = Mode::Selected;
                self.position_toolbar();
                return;
            }
            _ => {}
        }
        if self.mode != Mode::Selected {
            return;
        }
        match code {
            Code::KeyP => self.do_action(OverlayActionKind::Pin, fx),
            Code::KeyV => self.do_action(OverlayActionKind::Record, fx),
            Code::KeyT => self.enter_text_mode(fx),
            Code::KeyA => self.do_action(OverlayActionKind::Claude, fx),
            Code::BracketLeft | Code::BracketRight => {
                let at = SizeKey::ALL.iter().position(|s| *s == self.size).unwrap_or(1) as i32;
                let next = at + if code == Code::BracketRight { 1 } else { -1 };
                self.size = SizeKey::ALL[next.clamp(0, 2) as usize];
                self.position_toolbar();
            }
            Code::Digit1 => self.set_tool(Tool::Move, fx),
            Code::Digit2 => self.set_tool(Tool::Pen, fx),
            Code::Digit3 => self.set_tool(Tool::Arrow, fx),
            Code::Digit4 => self.set_tool(Tool::Line, fx),
            Code::Digit5 => self.set_tool(Tool::Rect, fx),
            Code::Digit6 => self.set_tool(Tool::Ellipse, fx),
            Code::Digit7 => self.set_tool(Tool::Highlight, fx),
            Code::Digit8 => self.set_tool(Tool::Blur, fx),
            Code::Digit9 => self.set_tool(Tool::Text, fx),
            Code::Digit0 => self.set_tool(Tool::Counter, fx),
            Code::ArrowLeft | Code::ArrowRight | Code::ArrowUp | Code::ArrowDown => {
                let step = (if shift { 10.0 } else { 1.0 }) * self.kx;
                if let Some(mut s) = self.sel {
                    match code {
                        Code::ArrowLeft => s.x = clamp(s.x - step, 0.0, self.img_w - s.w),
                        Code::ArrowRight => s.x = clamp(s.x + step, 0.0, self.img_w - s.w),
                        Code::ArrowUp => s.y = clamp(s.y - step, 0.0, self.img_h - s.h),
                        _ => s.y = clamp(s.y + step, 0.0, self.img_h - s.h),
                    }
                    self.sel = Some(s);
                }
                self.position_toolbar();
            }
            _ => {}
        }
    }

    fn key_up(&mut self, code: Code) {
        match code {
            Code::Alt => {
                self.alt_down = false;
                self.update_hover();
            }
            Code::Shift => {
                self.shift_down = false;
                self.update_hover();
            }
            _ => {}
        }
    }

    // ---------- actions ----------

    fn cancel(&mut self, fx: &mut Vec<Effect>) {
        self.closing = true;
        fx.push(Effect::Cancel);
    }

    fn rect_css_of(&self, r: IRect) -> RectF {
        RectF::new(
            f64::from(r.x) / self.kx,
            f64::from(r.y) / self.ky,
            f64::from(r.w) / self.kx,
            f64::from(r.h) / self.ky,
        )
    }

    fn finish_record(&mut self, fx: &mut Vec<Effect>) {
        let Some(r) = self.round_sel() else { return };
        self.closing = true;
        fx.push(Effect::Finish(Finish {
            action: OverlayActionKind::Record,
            rect_phys: Some(r),
            rect_css: Some(self.rect_css_of(r)),
            text: None,
            render_png: false,
        }));
    }

    /// `doAction`.
    fn do_action(&mut self, action: OverlayActionKind, fx: &mut Vec<Effect>) {
        if self.sel.is_none() || self.mode != Mode::Selected {
            return;
        }
        if action == OverlayActionKind::Record {
            self.finish_record(fx);
            return;
        }
        self.commit_text(fx);
        let Some(r) = self.round_sel() else { return };
        self.closing = true;
        fx.push(Effect::Finish(Finish {
            action,
            rect_phys: Some(r),
            rect_css: Some(self.rect_css_of(r)),
            text: None,
            render_png: true,
        }));
    }

    // ---------- what the view draws ----------

    /// The undimmed part of the frame: the selection, or the window under the cursor. Image pixels.
    pub fn hole(&self) -> Option<IRect> {
        match self.mode {
            Mode::Selected | Mode::Selecting => self.round_sel(),
            Mode::Idle => self.hover_rect.map(|h| IRect {
                x: js_round(h.x * self.kx) as i32,
                y: js_round(h.y * self.ky) as i32,
                w: js_round(h.w * self.kx) as i32,
                h: js_round(h.h * self.ky) as i32,
            }),
        }
    }

    /// Selection frame in image pixels: the border sits fully outside the rectangle.
    pub fn selection_chrome(&self) -> Option<SelectionChrome> {
        if !matches!(self.mode, Mode::Selected | Mode::Selecting) {
            return None;
        }
        let r = self.round_sel()?;
        let moving = matches!(self.drag, Some(Drag::Create { .. } | Drag::Resize { .. } | Drag::Move { .. }));
        let creating = matches!(self.drag, Some(Drag::Create { .. }));
        Some(SelectionChrome {
            rect: r,
            border: 1.5_f64.max(1.5 * self.kx),
            thirds: f64::from(r.w) / self.kx > 150.0 && f64::from(r.h) / self.ky > 150.0 && moving,
            handles: self.mode == Mode::Selected && self.tool == Tool::Move && !self.text_mode && !creating,
        })
    }

    /// Outline of the window under the cursor, image pixels: the stroke sits inside the rectangle.
    pub fn hover_outline(&self) -> Option<(IRect, f64)> {
        if self.mode != Mode::Idle {
            return None;
        }
        self.hole().map(|r| (r, 2.0_f64.max(2.0 * self.kx)))
    }

    /// The size label: text and position in CSS pixels.
    pub fn dims(&self) -> Option<(String, f64, f64)> {
        if let (true, false, Some(r), Some(rc)) = (self.mode != Mode::Idle, self.text_mode, self.round_sel(), self.sel_css()) {
            let x = clamp(rc.x, 8.0, self.view_w - 90.0);
            let mut y = rc.y - 30.0;
            if y < 6.0 {
                y = rc.y + 8.0;
            }
            return Some((format!("{} × {}", r.w, r.h), x, y));
        }
        if self.mode != Mode::Idle {
            return None;
        }
        self.hover_rect.map(|h| {
            let text = format!("{} × {}", js_round(h.w * self.kx), js_round(h.h * self.ky));
            (text, clamp(h.x, 8.0, self.view_w - 90.0), 6.0_f64.max(h.y - 30.0))
        })
    }

    /// The magnifier: source pixel under the cursor and the position of the panel in CSS pixels.
    pub fn magnifier(&self) -> Option<Magnifier> {
        let dragging_edges = matches!(self.drag, Some(Drag::Create { .. } | Drag::Resize { .. }));
        if self.mouse.x < 0.0 || !(self.mode == Mode::Idle || dragging_edges) {
            return None;
        }
        let ix = clamp(js_round(self.mouse.x * self.kx), 0.0, self.img_w - 1.0) as i32;
        let iy = clamp(js_round(self.mouse.y * self.ky), 0.0, self.img_h - 1.0) as i32;
        let mut x = self.mouse.x + 26.0;
        let mut y = self.mouse.y + 26.0;
        if x + 175.0 > self.view_w {
            x = self.mouse.x - 26.0 - 159.0;
        }
        if y + 205.0 > self.view_h {
            y = self.mouse.y - 26.0 - 192.0;
        }
        Some(Magnifier { ix, iy, x, y })
    }

    pub fn hints_visible(&self) -> bool {
        self.mode == Mode::Idle
    }

    pub fn can_undo(&self) -> bool {
        !self.annotations.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// True for one word shown as hovered by the pointer.
    pub fn word_state(&self, index: usize) -> WordState {
        if self.sel_words.contains(&index) {
            WordState::Selected
        } else if self.hover_word == Some(index) {
            WordState::Hovered
        } else {
            WordState::Normal
        }
    }

    /// The JSON of the `ui-state` route. The first fields are the contract (docs/rust/TEST-ROUTES.md); the
    /// rest helps tests and the preview see what a real pointer did.
    pub fn snapshot(&self) -> Value {
        json!({
            "ready": self.ready,
            "mode": self.mode.name(),
            "selection": self.sel,
            "viewport": { "width": self.view_w, "height": self.view_h },
            "image": { "width": self.img_w, "height": self.img_h },
            "toolbarVisible": self.toolbar.is_some(),
            "hintsVisible": self.hints_visible(),
            "textMode": self.text_mode,
            "wordCount": self.words.len(),
            "annotationCount": self.annotations.len(),

            "tool": self.tool,
            "color": self.color,
            "size": self.size,
            "redoCount": self.redo_stack.len(),
            "counterNext": self.counter_n,
            "hoverRect": self.hover_rect,
            "cursor": self.cursor,
            "selectedWords": self.sel_words.len(),
            "selectedText": self.selected_text(),
            "flash": self.flash.as_ref().map(|f| f.text.clone()),
            "optionsVisible": self.options.is_some(),
            "textToolbarVisible": self.text_toolbar.is_some(),
            "editorOpen": self.editor.is_some(),
            "badgeVisible": self.badge_visible,
            "magnifierVisible": self.magnifier().is_some(),
            "dims": self.dims().map(|d| d.0),
            "translate": self.card.as_ref().map(|c| json!({ "loading": c.loading, "body": c.body, "meta": c.meta })),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionChrome {
    pub rect: IRect,
    pub border: f64,
    pub thirds: bool,
    pub handles: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Magnifier {
    pub ix: i32,
    pub iy: i32,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordState {
    Normal,
    Hovered,
    Selected,
}

#[cfg(test)]
mod tests {
    use super::*;
    use shotik_core::ports::{OcrLine, OcrWord};

    /// The 4K display at 150% of the reference machine: image 3840x2160, viewport 2560x1440, k = 1.5.
    fn fresh(mode: OverlayMode) -> State {
        State::new(mode, (3840, 2160), (2560.0, 1440.0), BTreeMap::new())
    }

    fn go(s: &mut State, action: Action) -> Vec<Effect> {
        reduce(s, action)
    }

    fn down(s: &mut State, x: f64, y: f64) -> Vec<Effect> {
        go(s, Action::PointerDown { x, y, shift: false })
    }

    fn moved(s: &mut State, x: f64, y: f64) -> Vec<Effect> {
        go(s, Action::PointerMove { x, y, shift: false, alt: false })
    }

    fn up(s: &mut State, x: f64, y: f64) -> Vec<Effect> {
        go(s, Action::PointerUp { x, y, shift: false })
    }

    fn key(s: &mut State, code: Code) -> Vec<Effect> {
        go(s, Action::Key { code, ctrl: false, shift: false })
    }

    fn ctrl(s: &mut State, code: Code) -> Vec<Effect> {
        go(s, Action::Key { code, ctrl: true, shift: false })
    }

    fn drag(s: &mut State, from: (f64, f64), to: (f64, f64)) -> Vec<Effect> {
        let mut fx = down(s, from.0, from.1);
        fx.extend(moved(s, to.0, to.1));
        fx.extend(up(s, to.0, to.1));
        fx
    }

    /// Selected region: DIP (100,100) 400x300, image pixels (150,150) 600x450.
    fn selected() -> State {
        let mut s = fresh(OverlayMode::Normal);
        drag(&mut s, (100.0, 100.0), (500.0, 400.0));
        assert_eq!(s.mode, Mode::Selected);
        s
    }

    fn windows() -> Vec<RectF> {
        // Topmost first: a small one inside a big one.
        vec![RectF::new(300.0, 300.0, 200.0, 100.0), RectF::new(100.0, 100.0, 800.0, 600.0)]
    }

    fn pen_annotation(s: &State) -> Annotation {
        Annotation::Pen { points: vec![Pt { x: 1.0, y: 2.0 }, Pt { x: 50.0, y: 60.0 }], color: s.color, stroke: 4.0 }
    }

    fn ocr() -> OcrBoxes {
        let word = |text: &str, x: f64, w: f64, y: f64| OcrWord { text: text.into(), x, y, w, h: 30.0 };
        OcrBoxes {
            lines: vec![
                OcrLine { text: "Hello big".into(), words: vec![word("Hello", 0.0, 90.0, 0.0), word("big", 120.0, 60.0, 0.0)] },
                OcrLine { text: "world".into(), words: vec![word("world", 0.0, 90.0, 60.0)] },
            ],
        }
    }

    /// In Live Text with the words of `ocr()` over the selection of `selected()`.
    fn live_text() -> State {
        let mut s = selected();
        go(&mut s, Action::LiveText);
        assert!(s.text_mode);
        let generation = s.ocr_generation;
        go(&mut s, Action::OcrDone { generation, boxes: ocr() });
        s
    }

    #[test]
    fn opening_state_matches_the_contract_shape() {
        let s = fresh(OverlayMode::Normal);
        let snap = s.snapshot();
        assert_eq!(snap["mode"], "idle");
        assert_eq!(snap["selection"], Value::Null);
        assert_eq!(snap["viewport"]["width"], 2560.0);
        assert_eq!(snap["image"]["height"], 2160.0);
        assert_eq!(snap["toolbarVisible"], false);
        assert_eq!(snap["hintsVisible"], true);
        assert_eq!(snap["annotationCount"], 0);
        assert_eq!(s.cursor, Cursor::Crosshair);
    }

    #[test]
    fn a_drag_selects_and_keeps_float_image_pixels() {
        let s = selected();
        assert_eq!(s.sel, Some(RectF::new(150.0, 150.0, 600.0, 450.0)));
        assert!(s.toolbar.is_some());
        assert!(!s.hints_visible());
    }

    #[test]
    fn a_tiny_drag_goes_back_to_idle() {
        let mut s = fresh(OverlayMode::Normal);
        drag(&mut s, (100.0, 100.0), (500.0, 101.0));
        assert_eq!(s.mode, Mode::Idle);
        assert_eq!(s.sel, None);
        assert!(s.toolbar.is_none());
    }

    #[test]
    fn a_click_snaps_to_the_hovered_window_topmost_first() {
        let mut s = fresh(OverlayMode::Normal);
        go(&mut s, Action::SetWindows { rects: windows() });
        moved(&mut s, 350.0, 350.0);
        assert_eq!(s.hover_rect, Some(RectF::new(300.0, 300.0, 200.0, 100.0)));
        down(&mut s, 350.0, 350.0);
        up(&mut s, 350.0, 350.0);
        assert_eq!(s.sel, Some(RectF::new(450.0, 450.0, 300.0, 150.0)));
        assert_eq!(s.mode, Mode::Selected);
    }

    #[test]
    fn a_click_with_nothing_hovered_takes_the_whole_screen() {
        let mut s = fresh(OverlayMode::Normal);
        moved(&mut s, 50.0, 50.0);
        down(&mut s, 50.0, 50.0);
        up(&mut s, 52.0, 51.0);
        assert_eq!(s.sel, Some(RectF::new(0.0, 0.0, 3840.0, 2160.0)));
    }

    #[test]
    fn shift_click_takes_the_whole_screen_over_a_window() {
        let mut s = fresh(OverlayMode::Normal);
        go(&mut s, Action::SetWindows { rects: windows() });
        moved(&mut s, 350.0, 350.0);
        down(&mut s, 350.0, 350.0);
        go(&mut s, Action::PointerUp { x: 350.0, y: 350.0, shift: true });
        assert_eq!(s.sel, Some(RectF::new(0.0, 0.0, 3840.0, 2160.0)));
    }

    #[test]
    fn alt_and_shift_turn_snapping_off_until_released() {
        for code in [Code::Alt, Code::Shift] {
            let mut s = fresh(OverlayMode::Normal);
            go(&mut s, Action::SetWindows { rects: windows() });
            moved(&mut s, 350.0, 350.0);
            assert!(s.hover_rect.is_some());
            key(&mut s, code);
            assert_eq!(s.hover_rect, None, "{code:?}");
            go(&mut s, Action::KeyUp { code });
            assert!(s.hover_rect.is_some(), "{code:?}");
        }
    }

    #[test]
    fn modifiers_on_the_pointer_event_count_too() {
        let mut s = fresh(OverlayMode::Normal);
        go(&mut s, Action::SetWindows { rects: windows() });
        go(&mut s, Action::PointerMove { x: 350.0, y: 350.0, shift: false, alt: true });
        assert_eq!(s.hover_rect, None);
        go(&mut s, Action::PointerMove { x: 351.0, y: 350.0, shift: false, alt: false });
        assert!(s.hover_rect.is_some());
    }

    #[test]
    fn window_rectangles_arriving_late_apply_at_once() {
        let mut s = fresh(OverlayMode::Normal);
        moved(&mut s, 350.0, 350.0);
        assert_eq!(s.hover_rect, None);
        go(&mut s, Action::SetWindows { rects: windows() });
        assert!(s.hover_rect.is_some());
    }

    #[test]
    fn hover_clips_to_the_viewport_and_ignores_slivers() {
        let mut s = fresh(OverlayMode::Normal);
        go(
            &mut s,
            Action::SetWindows {
                rects: vec![RectF::new(-100.0, 10.0, 200.0, 50.0), RectF::new(2555.0, 500.0, 300.0, 300.0)],
            },
        );
        moved(&mut s, 50.0, 30.0);
        assert_eq!(s.hover_rect, Some(RectF::new(0.0, 10.0, 100.0, 50.0)));
        moved(&mut s, 2556.0, 600.0);
        assert_eq!(s.hover_rect, None, "only 5 CSS pixels of it are on this display");
    }

    #[test]
    fn the_hole_follows_the_selection_or_the_hover() {
        let mut s = fresh(OverlayMode::Normal);
        assert_eq!(s.hole(), None);
        go(&mut s, Action::SetWindows { rects: windows() });
        moved(&mut s, 350.0, 350.0);
        assert_eq!(s.hole(), Some(IRect::new(450, 450, 300, 150)));
        assert_eq!(s.hover_outline(), Some((IRect::new(450, 450, 300, 150), 3.0)));
        let s = selected();
        assert_eq!(s.hole(), Some(IRect::new(150, 150, 600, 450)));
        assert_eq!(s.hover_outline(), None);
    }

    #[test]
    fn a_handle_is_hit_within_seven_css_pixels_in_handle_order() {
        let s = selected();
        assert_eq!(s.handle_at(100.0, 100.0), Some(Handle::Nw));
        assert_eq!(s.handle_at(106.9, 93.1), Some(Handle::Nw));
        assert_eq!(s.handle_at(108.0, 100.0), None);
        assert_eq!(s.handle_at(300.0, 100.0), Some(Handle::N));
        assert_eq!(s.handle_at(500.0, 100.0), Some(Handle::Ne));
        assert_eq!(s.handle_at(500.0, 250.0), Some(Handle::E));
        assert_eq!(s.handle_at(500.0, 400.0), Some(Handle::Se));
        assert_eq!(s.handle_at(300.0, 400.0), Some(Handle::S));
        assert_eq!(s.handle_at(100.0, 400.0), Some(Handle::Sw));
        assert_eq!(s.handle_at(100.0, 250.0), Some(Handle::W));
    }

    #[test]
    fn handles_need_the_move_tool() {
        let mut s = selected();
        go(&mut s, Action::SetTool { tool: Tool::Pen });
        assert_eq!(s.handle_at(100.0, 100.0), None);
        assert!(!s.selection_chrome().unwrap().handles);
    }

    #[test]
    fn resizing_by_a_corner_keeps_the_opposite_corner() {
        let mut s = selected();
        drag(&mut s, (500.0, 400.0), (600.0, 500.0));
        assert_eq!(s.sel, Some(RectF::new(150.0, 150.0, 750.0, 600.0)));
        assert!(s.toolbar.is_some(), "the toolbar comes back after a resize");
    }

    #[test]
    fn resizing_past_the_anchor_flips_the_rectangle() {
        let mut s = selected();
        drag(&mut s, (500.0, 400.0), (50.0, 60.0));
        assert_eq!(s.sel, Some(RectF::new(75.0, 90.0, 75.0, 60.0)));
    }

    #[test]
    fn an_edge_handle_moves_one_axis_only() {
        let mut s = selected();
        drag(&mut s, (500.0, 250.0), (700.0, 1000.0));
        assert_eq!(s.sel, Some(RectF::new(150.0, 150.0, 900.0, 450.0)));
        let mut s = selected();
        drag(&mut s, (300.0, 100.0), (900.0, 50.0));
        assert_eq!(s.sel, Some(RectF::new(150.0, 75.0, 600.0, 525.0)));
    }

    #[test]
    fn moving_keeps_the_selection_on_the_frame() {
        let mut s = selected();
        drag(&mut s, (300.0, 250.0), (340.0, 280.0));
        assert_eq!(s.sel, Some(RectF::new(210.0, 195.0, 600.0, 450.0)));
        let mut s = selected();
        drag(&mut s, (300.0, 250.0), (9000.0, 9000.0));
        assert_eq!(s.sel, Some(RectF::new(3240.0, 1710.0, 600.0, 450.0)));
    }

    #[test]
    fn pressing_outside_starts_a_new_selection() {
        let mut s = selected();
        drag(&mut s, (1000.0, 1000.0), (1200.0, 1100.0));
        assert_eq!(s.sel, Some(RectF::new(1500.0, 1500.0, 300.0, 150.0)));
    }

    #[test]
    fn cursor_feedback_follows_the_pointer() {
        let mut s = selected();
        moved(&mut s, 100.0, 100.0);
        assert_eq!(s.cursor, Cursor::NwseResize);
        moved(&mut s, 500.0, 100.0);
        assert_eq!(s.cursor, Cursor::NeswResize);
        moved(&mut s, 300.0, 100.0);
        assert_eq!(s.cursor, Cursor::NsResize);
        moved(&mut s, 500.0, 250.0);
        assert_eq!(s.cursor, Cursor::EwResize);
        moved(&mut s, 300.0, 250.0);
        assert_eq!(s.cursor, Cursor::Move);
        moved(&mut s, 1000.0, 1000.0);
        assert_eq!(s.cursor, Cursor::Crosshair);
        go(&mut s, Action::SetTool { tool: Tool::Text });
        moved(&mut s, 300.0, 250.0);
        assert_eq!(s.cursor, Cursor::Text);
    }

    #[test]
    fn the_size_label_sits_above_or_inside_when_there_is_no_room() {
        let s = selected();
        assert_eq!(s.dims(), Some(("600 × 450".to_string(), 100.0, 70.0)));
        let mut s = fresh(OverlayMode::Normal);
        drag(&mut s, (2550.0, 10.0), (2500.0, 200.0));
        let (_, x, y) = s.dims().unwrap();
        assert_eq!(x, 2470.0, "clamped to the right edge");
        assert_eq!(y, 18.0, "no room above, 8 pixels inside the selection");
    }

    #[test]
    fn the_magnifier_shows_while_idle_and_while_drawing_edges_only() {
        let mut s = fresh(OverlayMode::Normal);
        assert_eq!(s.magnifier(), None, "before the first pointer move");
        moved(&mut s, 100.0, 200.0);
        assert_eq!(s.magnifier(), Some(Magnifier { ix: 150, iy: 300, x: 126.0, y: 226.0 }));
        down(&mut s, 100.0, 200.0);
        moved(&mut s, 300.0, 300.0);
        assert!(s.magnifier().is_some(), "while creating");
        up(&mut s, 300.0, 300.0);
        assert_eq!(s.magnifier(), None, "not while selected");
    }

    #[test]
    fn the_magnifier_flips_near_the_edges() {
        let mut s = fresh(OverlayMode::Normal);
        moved(&mut s, 2500.0, 1400.0);
        let m = s.magnifier().unwrap();
        assert_eq!((m.x, m.y), (2500.0 - 26.0 - 159.0, 1400.0 - 26.0 - 192.0));
        assert_eq!((m.ix, m.iy), (3750, 2100));
        moved(&mut s, 2559.9, 1439.9);
        let m = s.magnifier().unwrap();
        assert_eq!((m.ix, m.iy), (3839, 2159), "clamped to the frame");
    }

    #[test]
    fn the_toolbar_goes_below_above_or_inside() {
        let s = selected();
        let t = s.toolbar.unwrap();
        assert_eq!((t.x, t.y, t.tips_below), (8.0_f64.max(300.0 - 360.0), 410.0, false));

        // No room below: above the selection.
        let mut s = fresh(OverlayMode::Normal);
        drag(&mut s, (1000.0, 1000.0), (1500.0, 1420.0));
        assert_eq!(s.toolbar.unwrap().y, 1000.0 - TOOLBAR_H - 10.0);

        // No room above or below: inside at the bottom.
        let mut s = fresh(OverlayMode::Normal);
        key(&mut s, Code::KeyF);
        let t = s.toolbar.unwrap();
        assert_eq!(t.y, 1440.0 - TOOLBAR_H - 10.0 - 8.0);
        assert_eq!(t.x, 920.0, "centred");

        // Tooltips open downward when the bar hugs the top.
        let mut s = fresh(OverlayMode::Normal);
        drag(&mut s, (1000.0, 2.0), (1500.0, 30.0));
        assert!(s.toolbar.unwrap().tips_below, "y = {}", s.toolbar.unwrap().y);
    }

    #[test]
    fn the_toolbar_clamps_to_the_sides() {
        let mut s = fresh(OverlayMode::Normal);
        drag(&mut s, (2400.0, 100.0), (2550.0, 300.0));
        assert_eq!(s.toolbar.unwrap().x, 2560.0 - TOOLBAR_W - 8.0);
    }

    #[test]
    fn the_options_bar_belongs_to_drawing_tools_and_flips_near_the_bottom() {
        let mut s = selected();
        assert_eq!(s.options, None);
        go(&mut s, Action::SetTool { tool: Tool::Blur });
        assert_eq!(s.options, None, "pixelation has no colour or size");
        go(&mut s, Action::SetTool { tool: Tool::Counter });
        let t = s.toolbar.unwrap();
        let o = s.options.unwrap();
        assert_eq!((o.x, o.y), (t.x, t.y + TOOLBAR_H + 8.0));

        let mut s = fresh(OverlayMode::Normal);
        drag(&mut s, (1000.0, 1000.0), (1500.0, 1340.0));
        go(&mut s, Action::SetTool { tool: Tool::Pen });
        let t = s.toolbar.unwrap();
        assert_eq!(t.y, 1350.0);
        assert_eq!(s.options.unwrap().y, t.y - OPTIONS_H - 8.0);
    }

    #[test]
    fn the_toolbar_hides_during_a_drag_and_comes_back() {
        let mut s = selected();
        down(&mut s, 300.0, 250.0);
        assert!(s.toolbar.is_none());
        moved(&mut s, 310.0, 250.0);
        up(&mut s, 310.0, 250.0);
        assert!(s.toolbar.is_some());
    }

    #[test]
    fn thirds_show_only_while_dragging_a_big_selection() {
        let mut s = selected();
        assert!(!s.selection_chrome().unwrap().thirds, "not at rest");
        down(&mut s, 300.0, 250.0);
        assert!(s.selection_chrome().unwrap().thirds, "while moving");
        up(&mut s, 300.0, 250.0);
        let mut small = fresh(OverlayMode::Normal);
        down(&mut small, 100.0, 100.0);
        moved(&mut small, 200.0, 200.0);
        assert!(!small.selection_chrome().unwrap().thirds, "too small");
    }

    #[test]
    fn the_pen_records_points_past_a_minimum_step() {
        let mut s = selected();
        go(&mut s, Action::SetTool { tool: Tool::Pen });
        let fx = down(&mut s, 200.0, 200.0);
        assert!(fx.contains(&Effect::Layer(LayerOp::LiveChanged)));
        assert!(moved(&mut s, 200.5, 200.0).is_empty(), "under 2 CSS pixels: no new point");
        let fx = moved(&mut s, 210.0, 200.0);
        assert_eq!(fx, [Effect::Layer(LayerOp::LiveExtended { from: 1 })]);
        let fx = up(&mut s, 210.0, 200.0);
        assert_eq!(fx, [Effect::Layer(LayerOp::LiveCommitted)]);
        assert_eq!(s.annotations.len(), 1);
        assert_eq!(s.temp, None);
        match &s.annotations[0] {
            Annotation::Pen { points, stroke, color } => {
                assert_eq!(points.len(), 2);
                assert_eq!(*stroke, 4.0);
                assert_eq!(*color, COLORS[0]);
                assert_eq!(points[1], Pt { x: 315.0, y: 300.0 });
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_click_with_a_drawing_tool_adds_nothing() {
        for tool in [Tool::Pen, Tool::Rect, Tool::Arrow, Tool::Highlight, Tool::Blur] {
            let mut s = selected();
            go(&mut s, Action::SetTool { tool });
            down(&mut s, 200.0, 200.0);
            let fx = up(&mut s, 200.0, 200.0);
            assert_eq!(fx, [Effect::Layer(LayerOp::LiveDropped)], "{tool:?}");
            assert!(s.annotations.is_empty());
        }
    }

    #[test]
    fn shift_snaps_lines_to_45_degrees_and_boxes_to_squares() {
        let mut s = selected();
        go(&mut s, Action::SetTool { tool: Tool::Line });
        down(&mut s, 200.0, 200.0);
        go(&mut s, Action::PointerMove { x: 300.0, y: 210.0, shift: true, alt: false });
        match &s.temp {
            Some(Annotation::Line(l)) => {
                assert!((l.y2 - l.y1).abs() < 1e-9, "snapped horizontal");
                assert!((l.x2 - l.x1 - (150.0_f64.hypot(15.0))).abs() < 1e-9, "length kept");
            }
            other => panic!("{other:?}"),
        }
        up(&mut s, 300.0, 210.0);

        go(&mut s, Action::SetTool { tool: Tool::Rect });
        down(&mut s, 200.0, 200.0);
        go(&mut s, Action::PointerMove { x: 180.0, y: 300.0, shift: true, alt: false });
        match &s.temp {
            Some(Annotation::Rect(r)) => {
                assert_eq!(r.x2 - r.x1, -150.0, "the longer side, keeping the direction");
                assert_eq!(r.y2 - r.y1, 150.0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn counters_number_themselves_through_undo_and_redo() {
        let mut s = selected();
        go(&mut s, Action::SetTool { tool: Tool::Counter });
        down(&mut s, 200.0, 200.0);
        down(&mut s, 220.0, 200.0);
        let numbers = |s: &State| -> Vec<u32> {
            s.annotations
                .iter()
                .map(|a| match a {
                    Annotation::Counter { n, .. } => *n,
                    _ => 0,
                })
                .collect()
        };
        assert_eq!(numbers(&s), [1, 2]);
        assert_eq!(s.counter_n, 3);
        go(&mut s, Action::Undo);
        assert_eq!(s.counter_n, 2);
        down(&mut s, 240.0, 200.0);
        assert_eq!(numbers(&s), [1, 2], "the freed number is reused");
        go(&mut s, Action::Undo);
        go(&mut s, Action::Redo);
        assert_eq!(s.counter_n, 3);
    }

    #[test]
    fn undo_and_redo_walk_the_history_and_a_new_mark_clears_redo() {
        let mut s = selected();
        s.annotations.push(pen_annotation(&s));
        s.annotations.push(pen_annotation(&s));
        assert!(s.can_undo() && !s.can_redo());
        ctrl(&mut s, Code::KeyZ);
        assert_eq!((s.annotations.len(), s.redo_stack.len()), (1, 1));
        go(&mut s, Action::Key { code: Code::KeyZ, ctrl: true, shift: true });
        assert_eq!((s.annotations.len(), s.redo_stack.len()), (2, 0));
        ctrl(&mut s, Code::KeyZ);
        ctrl(&mut s, Code::KeyY);
        assert_eq!(s.annotations.len(), 2);
        ctrl(&mut s, Code::KeyZ);
        go(&mut s, Action::SetTool { tool: Tool::Rect });
        drag(&mut s, (200.0, 200.0), (260.0, 260.0));
        assert_eq!((s.annotations.len(), s.redo_stack.len()), (2, 0));
        for _ in 0..5 {
            ctrl(&mut s, Code::KeyZ);
        }
        assert!(s.annotations.is_empty());
        assert_eq!(s.redo_stack.len(), 2);
    }

    #[test]
    fn digits_pick_tools_and_brackets_pick_sizes() {
        let mut s = selected();
        let table = [
            (Code::Digit2, Tool::Pen),
            (Code::Digit3, Tool::Arrow),
            (Code::Digit4, Tool::Line),
            (Code::Digit5, Tool::Rect),
            (Code::Digit6, Tool::Ellipse),
            (Code::Digit7, Tool::Highlight),
            (Code::Digit8, Tool::Blur),
            (Code::Digit9, Tool::Text),
            (Code::Digit0, Tool::Counter),
            (Code::Digit1, Tool::Move),
        ];
        for (code, tool) in table {
            key(&mut s, code);
            assert_eq!(s.tool, tool);
        }
        key(&mut s, Code::BracketRight);
        assert_eq!(s.size, SizeKey::L);
        key(&mut s, Code::BracketRight);
        assert_eq!(s.size, SizeKey::L);
        key(&mut s, Code::BracketLeft);
        key(&mut s, Code::BracketLeft);
        key(&mut s, Code::BracketLeft);
        assert_eq!(s.size, SizeKey::S);
    }

    #[test]
    fn tool_keys_do_nothing_before_a_selection() {
        let mut s = fresh(OverlayMode::Normal);
        key(&mut s, Code::Digit2);
        assert_eq!(s.tool, Tool::Move);
        assert!(key(&mut s, Code::KeyP).is_empty());
        assert!(key(&mut s, Code::Enter).is_empty());
        assert!(ctrl(&mut s, Code::KeyC).is_empty());
        assert!(ctrl(&mut s, Code::KeyS).is_empty());
    }

    #[test]
    fn f_selects_the_whole_screen_from_anywhere() {
        let mut s = fresh(OverlayMode::Normal);
        key(&mut s, Code::KeyF);
        assert_eq!(s.sel, Some(RectF::new(0.0, 0.0, 3840.0, 2160.0)));
        assert_eq!(s.mode, Mode::Selected);
        assert!(s.toolbar.is_some());
    }

    #[test]
    fn arrows_nudge_by_one_or_ten_css_pixels_inside_the_frame() {
        let mut s = selected();
        key(&mut s, Code::ArrowRight);
        assert_eq!(s.sel.unwrap().x, 151.5);
        go(&mut s, Action::Key { code: Code::ArrowDown, ctrl: false, shift: true });
        assert_eq!(s.sel.unwrap().y, 165.0);
        for _ in 0..200 {
            key(&mut s, Code::ArrowLeft);
        }
        assert_eq!(s.sel.unwrap().x, 0.0);
    }

    #[test]
    fn escape_cancels_a_drag_first_then_the_session() {
        let mut s = fresh(OverlayMode::Normal);
        down(&mut s, 100.0, 100.0);
        moved(&mut s, 300.0, 300.0);
        assert!(key(&mut s, Code::Escape).is_empty());
        assert_eq!(s.mode, Mode::Idle);
        assert_eq!(s.sel, None);
        assert_eq!(key(&mut s, Code::Escape), [Effect::Cancel]);
        assert!(s.closing);
    }

    #[test]
    fn escape_drops_a_stroke_in_progress_and_keeps_the_selection() {
        let mut s = selected();
        go(&mut s, Action::SetTool { tool: Tool::Pen });
        down(&mut s, 200.0, 200.0);
        moved(&mut s, 250.0, 250.0);
        assert_eq!(key(&mut s, Code::Escape), [Effect::Layer(LayerOp::LiveDropped)]);
        assert_eq!(s.temp, None);
        assert_eq!(s.mode, Mode::Selected);
    }

    fn finish_of(fx: &[Effect]) -> &Finish {
        fx.iter()
            .find_map(|e| match e {
                Effect::Finish(f) => Some(f),
                _ => None,
            })
            .expect("a finish effect")
    }

    #[test]
    fn action_keys_finish_with_the_rounded_selection() {
        let table = [
            (Code::Enter, false, OverlayActionKind::Copy),
            (Code::KeyC, true, OverlayActionKind::Copy),
            (Code::KeyS, true, OverlayActionKind::Save),
            (Code::KeyP, false, OverlayActionKind::Pin),
            (Code::KeyA, false, OverlayActionKind::Claude),
        ];
        for (code, with_ctrl, action) in table {
            let mut s = selected();
            let fx = go(&mut s, Action::Key { code, ctrl: with_ctrl, shift: false });
            let f = finish_of(&fx);
            assert_eq!(f.action, action);
            assert!(f.render_png);
            assert_eq!(f.rect_phys, Some(IRect::new(150, 150, 600, 450)));
            assert_eq!(f.rect_css, Some(RectF::new(100.0, 100.0, 400.0, 300.0)));
            assert!(s.closing);
            assert!(key(&mut s, Code::Enter).is_empty(), "exactly one finish");
        }
    }

    #[test]
    fn record_needs_no_image() {
        let mut s = selected();
        let fx = key(&mut s, Code::KeyV);
        let f = finish_of(&fx);
        assert_eq!(f.action, OverlayActionKind::Record);
        assert!(!f.render_png);
        assert_eq!(f.rect_phys, Some(IRect::new(150, 150, 600, 450)));
    }

    #[test]
    fn record_mode_finishes_on_mouse_up() {
        let mut s = fresh(OverlayMode::Record);
        let fx = drag(&mut s, (100.0, 100.0), (500.0, 400.0));
        assert_eq!(finish_of(&fx).action, OverlayActionKind::Record);
        assert!(s.toolbar.is_none());
    }

    #[test]
    fn text_mode_goes_straight_to_live_text_on_mouse_up() {
        let mut s = fresh(OverlayMode::Text);
        let fx = drag(&mut s, (100.0, 100.0), (500.0, 400.0));
        assert!(s.text_mode);
        assert!(s.toolbar.is_none());
        assert!(fx.iter().any(|e| matches!(e, Effect::Recognize { rect, .. } if *rect == IRect::new(150, 150, 600, 450))));
    }

    #[test]
    fn claude_mode_shows_the_badge_until_live_text() {
        let mut s = fresh(OverlayMode::Claude);
        assert!(s.badge_visible);
        drag(&mut s, (100.0, 100.0), (500.0, 400.0));
        assert!(s.badge_visible);
        go(&mut s, Action::LiveText);
        assert!(!s.badge_visible);
    }

    #[test]
    fn a_double_click_inside_copies_and_outside_does_nothing() {
        let mut s = selected();
        assert!(go(&mut s, Action::DoubleClick { x: 1000.0, y: 1000.0 }).is_empty());
        let fx = go(&mut s, Action::DoubleClick { x: 300.0, y: 250.0 });
        assert_eq!(finish_of(&fx).action, OverlayActionKind::Copy);
    }

    #[test]
    fn c_picks_the_colour_under_the_cursor_only_when_idle() {
        let mut s = fresh(OverlayMode::Normal);
        moved(&mut s, 100.0, 200.0);
        assert_eq!(key(&mut s, Code::KeyC), [Effect::PickColor { ix: 150, iy: 300 }]);
        assert!(s.closing);
        let mut s = selected();
        assert!(key(&mut s, Code::KeyC).is_empty());
    }

    #[test]
    fn the_editor_commits_lines_as_a_text_annotation() {
        let mut s = selected();
        go(&mut s, Action::SetTool { tool: Tool::Text });
        let fx = down(&mut s, 200.0, 220.0);
        assert_eq!(fx, [Effect::FocusEditor]);
        assert_eq!(s.editor.as_ref().map(|e| (e.x, e.y)), Some((200.0, 220.0)));
        go(&mut s, Action::EditorInput { text: "one  \ntwo\u{a0}x\n\n".into() });
        let fx = go(&mut s, Action::EditorCommit);
        assert_eq!(fx, [Effect::Layer(LayerOp::Added)]);
        assert_eq!(s.editor, None);
        match &s.annotations[0] {
            Annotation::Text { x, y, lines, font_px, .. } => {
                assert_eq!((*x, *y), (300.0, 330.0));
                assert_eq!(lines, &["one".to_string(), "two x".to_string(), String::new()]);
                assert_eq!(*font_px, 23.0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_editor_adds_nothing() {
        for text in ["", "   ", "\n \n"] {
            let mut s = selected();
            go(&mut s, Action::SetTool { tool: Tool::Text });
            down(&mut s, 200.0, 220.0);
            go(&mut s, Action::EditorInput { text: text.into() });
            assert!(go(&mut s, Action::EditorCommit).is_empty(), "{text:?}");
            assert!(s.annotations.is_empty());
        }
    }

    #[test]
    fn keys_belong_to_the_editor_while_it_is_open() {
        let mut s = selected();
        go(&mut s, Action::SetTool { tool: Tool::Text });
        down(&mut s, 200.0, 220.0);
        assert!(key(&mut s, Code::KeyP).is_empty());
        assert!(key(&mut s, Code::Enter).is_empty());
        assert!(key(&mut s, Code::Escape).is_empty());
        assert!(s.editor.is_some());
        go(&mut s, Action::EditorCancel);
        assert_eq!(s.editor, None);
    }

    #[test]
    fn clicking_outside_or_changing_tool_commits_the_editor() {
        let mut s = selected();
        go(&mut s, Action::SetTool { tool: Tool::Text });
        down(&mut s, 200.0, 220.0);
        go(&mut s, Action::EditorInput { text: "a".into() });
        down(&mut s, 400.0, 300.0);
        assert_eq!(s.annotations.len(), 1);
        assert_eq!(s.editor, None, "the same click does not open another editor");
        down(&mut s, 400.0, 300.0);
        go(&mut s, Action::EditorInput { text: "b".into() });
        go(&mut s, Action::SetTool { tool: Tool::Pen });
        assert_eq!(s.annotations.len(), 2);
    }

    #[test]
    fn an_action_commits_the_open_editor_first() {
        let mut s = selected();
        go(&mut s, Action::SetTool { tool: Tool::Text });
        down(&mut s, 200.0, 220.0);
        go(&mut s, Action::EditorInput { text: "note".into() });
        let fx = go(&mut s, Action::Button { button: Button::Copy });
        assert_eq!(s.annotations.len(), 1);
        assert_eq!(fx[0], Effect::Layer(LayerOp::Added));
        assert_eq!(finish_of(&fx).action, OverlayActionKind::Copy);
    }

    #[test]
    fn live_text_maps_word_boxes_from_the_crop_to_css() {
        let s = live_text();
        assert_eq!(s.words.len(), 3);
        // Selection at image (150,150), k = 1.5: crop x 120 -> css (150 + 120) / 1.5.
        assert_eq!(s.words[1].x, 180.0);
        assert_eq!(s.words[1].y, 100.0);
        assert_eq!(s.words[1].w, 40.0);
        assert_eq!(s.words[2].line, 1);
        assert_eq!(s.cursor, Cursor::Text);
        assert!(s.text_toolbar.is_some());
        assert_eq!(s.snapshot()["wordCount"], 3);
    }

    #[test]
    fn an_empty_recognition_says_so() {
        let mut s = selected();
        go(&mut s, Action::LiveText);
        let generation = s.ocr_generation;
        go(&mut s, Action::OcrDone { generation, boxes: OcrBoxes::default() });
        assert_eq!(s.flash.as_ref().map(|f| f.text.as_str()), Some("ocrNoText"));
        assert_eq!(s.selected_text(), "");
    }

    #[test]
    fn a_stale_recognition_is_dropped() {
        let mut s = selected();
        go(&mut s, Action::LiveText);
        let generation = s.ocr_generation;
        key(&mut s, Code::Escape);
        assert!(!s.text_mode);
        go(&mut s, Action::OcrDone { generation, boxes: ocr() });
        assert!(s.words.is_empty());
        assert!(!s.words_visible);
    }

    #[test]
    fn a_word_click_selects_it_and_a_click_elsewhere_clears() {
        let mut s = live_text();
        down(&mut s, 110.0, 110.0);
        up(&mut s, 110.0, 110.0);
        assert_eq!(s.sel_words, BTreeSet::from([0]));
        assert_eq!(s.selected_text(), "Hello");
        down(&mut s, 2000.0, 1000.0);
        up(&mut s, 2000.0, 1000.0);
        assert!(s.sel_words.is_empty());
        assert_eq!(s.selected_text(), "Hello big\nworld", "nothing selected means everything");
    }

    #[test]
    fn the_rubber_band_adds_what_it_touches_and_shift_keeps_the_rest() {
        let mut s = live_text();
        down(&mut s, 100.0, 100.0);
        moved(&mut s, 170.0, 200.0);
        assert!(s.rubber_rect.is_some());
        assert_eq!(s.sel_words, BTreeSet::from([0, 2]));
        up(&mut s, 170.0, 200.0);
        assert_eq!(s.rubber_rect, None);
        assert_eq!(s.selected_text(), "Hello\nworld");

        go(&mut s, Action::PointerDown { x: 185.0, y: 110.0, shift: true });
        assert_eq!(s.sel_words, BTreeSet::from([0, 2]), "Shift+press on a word keeps the selection");
        moved(&mut s, 190.0, 112.0);
        assert_eq!(s.sel_words, BTreeSet::from([0, 1, 2]));
    }

    #[test]
    fn words_sort_by_x_within_a_line() {
        let mut s = live_text();
        s.sel_words = BTreeSet::from([1, 0]);
        assert_eq!(s.selected_text(), "Hello big");
        s.words.swap(0, 1);
        assert_eq!(s.selected_text(), "Hello big");
    }

    #[test]
    fn select_all_keys_and_buttons_agree() {
        let mut s = live_text();
        ctrl(&mut s, Code::KeyA);
        assert_eq!(s.sel_words.len(), 3);
        s.sel_words.clear();
        go(&mut s, Action::TextSelectAll);
        assert_eq!(s.snapshot()["selectedWords"], 3);
    }

    #[test]
    fn enter_in_live_text_finishes_with_the_text() {
        let mut s = live_text();
        let fx = key(&mut s, Code::Enter);
        let f = finish_of(&fx);
        assert_eq!(f.action, OverlayActionKind::Copytext);
        assert_eq!(f.text.as_deref(), Some("Hello big\nworld"));
        assert!(!f.render_png);
        assert_eq!(f.rect_phys, None);
    }

    #[test]
    fn ctrl_c_in_live_text_copies_and_stays() {
        let mut s = live_text();
        let fx = ctrl(&mut s, Code::KeyC);
        assert!(fx.contains(&Effect::CopyText("Hello big\nworld".into())));
        assert!(!s.closing);
        assert_eq!(s.flash.as_ref().map(|f| f.text.as_str()), Some("copiedTitle"));
    }

    #[test]
    fn empty_live_text_never_finishes() {
        let mut s = selected();
        go(&mut s, Action::LiveText);
        let generation = s.ocr_generation;
        go(&mut s, Action::OcrDone { generation, boxes: OcrBoxes::default() });
        assert!(key(&mut s, Code::Enter).iter().all(|e| matches!(e, Effect::ArmFlash { .. })));
        assert!(!s.closing);
    }

    #[test]
    fn escape_in_live_text_closes_the_card_then_the_mode() {
        let mut s = live_text();
        key(&mut s, Code::KeyR);
        assert!(s.card.is_some());
        key(&mut s, Code::Escape);
        assert!(s.card.is_none());
        assert!(s.text_mode);
        key(&mut s, Code::Escape);
        assert!(!s.text_mode);
        assert!(s.toolbar.is_some(), "the normal toolbar returns");
        assert_eq!(s.cursor, Cursor::Crosshair);
        assert!(!s.closing);
    }

    #[test]
    fn translation_flows_through_the_card() {
        let mut s = live_text();
        let fx = key(&mut s, Code::KeyR);
        let generation = s.translate_generation;
        assert!(fx.contains(&Effect::Translate { generation, text: "Hello big\nworld".into() }));
        let card = s.card.as_ref().unwrap();
        assert!(card.loading);
        assert_eq!(card.body, "txtTranslating");

        let result = Translated { text: "Привет".into(), source: Some("en".into()), target: Some("ru".into()) };
        let fx = go(&mut s, Action::TranslateDone { generation, result: Some(result) });
        assert_eq!(fx, [Effect::MeasureCard]);
        let card = s.card.as_ref().unwrap();
        assert!(!card.loading);
        assert_eq!(card.body, "Привет");
        assert_eq!(card.meta, "en → ru");

        let fx = go(&mut s, Action::TranslateCopy);
        assert!(fx.contains(&Effect::CopyText("Привет".into())));
    }

    #[test]
    fn a_failed_or_autodetected_translation_shows_the_right_text() {
        let mut s = live_text();
        key(&mut s, Code::KeyR);
        let generation = s.translate_generation;
        go(&mut s, Action::TranslateDone { generation, result: None });
        assert_eq!(s.card.as_ref().unwrap().body, "txtTranslateFail");

        key(&mut s, Code::KeyR);
        let generation = s.translate_generation;
        let result = Translated { text: String::new(), source: Some("auto".into()), target: None };
        go(&mut s, Action::TranslateDone { generation, result: Some(result) });
        let card = s.card.as_ref().unwrap();
        assert_eq!(card.body, "—");
        assert_eq!(card.meta, "");
    }

    #[test]
    fn the_card_goes_right_of_the_selection_or_left_when_it_does_not_fit() {
        let mut s = live_text();
        key(&mut s, Code::KeyR);
        go(&mut s, Action::CardMeasured { height: 200.0 });
        let card = s.card.as_ref().unwrap();
        assert_eq!((card.x, card.y, card.width), (512.0, 100.0, 440.0));

        let mut s = fresh(OverlayMode::Normal);
        drag(&mut s, (2200.0, 1300.0), (2500.0, 1400.0));
        go(&mut s, Action::LiveText);
        let generation = s.ocr_generation;
        go(&mut s, Action::OcrDone { generation, boxes: ocr() });
        key(&mut s, Code::KeyR);
        go(&mut s, Action::CardMeasured { height: 200.0 });
        let card = s.card.as_ref().unwrap();
        assert_eq!(card.x, 2200.0 - 440.0 - 12.0);
        assert_eq!(card.y, 1440.0 - 200.0 - 8.0, "kept on screen");
    }

    #[test]
    fn a_press_outside_closes_the_card() {
        let mut s = live_text();
        key(&mut s, Code::KeyR);
        down(&mut s, 2000.0, 1000.0);
        assert!(s.card.is_none());
    }

    #[test]
    fn the_flash_centres_on_the_selection_and_expires_by_generation() {
        let mut s = selected();
        go(&mut s, Action::LiveText);
        let f = s.flash.clone().unwrap();
        assert_eq!((f.x, f.y), (300.0, 60.0));
        let old = s.flash_generation;
        let generation = s.ocr_generation;
        go(&mut s, Action::OcrDone { generation, boxes: OcrBoxes::default() });
        go(&mut s, Action::FlashExpired { generation: old });
        assert!(s.flash.is_some(), "an old timer does not hide a newer message");
        let generation = s.flash_generation;
        go(&mut s, Action::FlashExpired { generation });
        assert_eq!(s.flash, None);
    }

    #[test]
    fn flash_without_a_selection_stays_at_the_top_centre_and_on_screen() {
        let mut s = fresh(OverlayMode::Normal);
        let mut fx = Vec::new();
        s.flash("copiedTitle", &mut fx);
        let f = s.flash.clone().unwrap();
        assert_eq!((f.x, f.y), (1280.0, 20.0));
        assert!(matches!(fx[0], Effect::ArmFlash { ms: FLASH_MS, .. }));
        let mut narrow = State::new(OverlayMode::Normal, (100, 100), (100.0, 100.0), BTreeMap::new());
        narrow.flash("x", &mut fx);
        assert_eq!(narrow.flash.unwrap().x, 20.0, "clamp keeps the upper bound when the range inverts");
    }

    #[test]
    fn hovering_a_word_is_tracked() {
        let mut s = live_text();
        moved(&mut s, 110.0, 110.0);
        assert_eq!(s.word_state(0), WordState::Hovered);
        assert_eq!(s.word_state(1), WordState::Normal);
        down(&mut s, 110.0, 110.0);
        assert_eq!(s.word_state(0), WordState::Selected);
    }

    #[test]
    fn the_select_route_converts_css_to_image_pixels_and_lays_out_the_toolbar() {
        let mut s = fresh(OverlayMode::Normal);
        go(&mut s, Action::Select { rect: RectF::new(320.0, 186.5, 1466.5, 853.5) });
        let snap = s.snapshot();
        assert_eq!(snap["mode"], "selected");
        assert_eq!(snap["selection"]["x"], 480.0);
        assert_eq!(snap["selection"]["w"], 2199.75);
        assert_eq!(snap["toolbarVisible"], true);
        assert_eq!(s.round_sel(), Some(IRect::new(480, 280, 2200, 1280)));
    }

    #[test]
    fn the_annotate_route_scales_geometry_and_not_stroke() {
        let mut s = selected();
        let action: Action = serde_json::from_value(json!({
            "type": "annotate",
            "annotations": [
                {"type":"rect","x1":320,"y1":220,"x2":740,"y2":360,"color":"#FF4D5E","stroke":4},
                {"type":"counter","x":300,"y":210,"n":1,"color":"#FF9F2E"},
                {"type":"text","x":340,"y":390,"lines":["Review this area"],"fontPx":23,"color":"#FFFFFF"},
                {"type":"pen","points":[{"x":10,"y":20},{"x":30,"y":40}],"color":"#FFFFFF","stroke":2.2},
            ]
        }))
        .unwrap();
        let fx = go(&mut s, action);
        assert_eq!(fx, [Effect::Layer(LayerOp::Replaced)]);
        assert_eq!(s.snapshot()["annotationCount"], 4);
        match &s.annotations[0] {
            Annotation::Rect(r) => {
                assert_eq!((r.x1, r.y2), (480.0, 540.0));
                assert_eq!(r.stroke, 4.0);
            }
            other => panic!("{other:?}"),
        }
        match &s.annotations[3] {
            Annotation::Pen { points, stroke, .. } => {
                assert_eq!(points[1], Pt { x: 45.0, y: 60.0 });
                assert_eq!(*stroke, 2.2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn actions_parse_from_the_documented_json() {
        let cases = [
            json!({"type":"select","rect":{"x":240,"y":140,"w":1100,"h":640}}),
            json!({"type":"annotate","annotations":[]}),
            json!({"type":"live-text"}),
            json!({"type":"text-select-all"}),
            json!({"type":"copy-color","hex":"#3E9EFF"}),
            json!({"type":"key","code":"KeyC","ctrl":true}),
            json!({"type":"set-tool","tool":"pen"}),
            json!({"type":"button","button":"copy"}),
        ];
        for case in cases {
            serde_json::from_value::<Action>(case.clone()).unwrap_or_else(|e| panic!("{case}: {e}"));
        }
        assert!(serde_json::from_value::<Action>(json!({"type":"nonsense"})).is_err());
    }

    #[test]
    fn the_copy_color_route_does_not_end_the_session() {
        let mut s = fresh(OverlayMode::Normal);
        let fx = go(&mut s, Action::CopyColor { hex: "#3E9EFF".into() });
        assert_eq!(fx, [Effect::CopyColor("#3E9EFF".into())]);
        assert!(!s.closing);
    }

    #[test]
    fn colours_round_trip_and_reject_nonsense() {
        assert_eq!(Rgb::parse("#3E9EFF"), Some(Rgb(0x3E, 0x9E, 0xFF)));
        assert_eq!(Rgb(0x3E, 0x9E, 0xFF).hex(), "#3E9EFF");
        for bad in ["3E9EFF", "#3E9EF", "#GGGGGG", "#ЖЖЖ"] {
            assert_eq!(Rgb::parse(bad), None, "{bad}");
        }
        assert_eq!(Rgb(0xFF, 0xFF, 0xFF).light_or_dark(), Rgb(0x1b, 0x1d, 0x24));
        assert_eq!(Rgb(0x16, 0x18, 0x1D).light_or_dark(), Rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn rounding_matches_math_round() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(2.4), 2.0);
    }

    #[test]
    fn nothing_happens_after_closing() {
        let mut s = selected();
        key(&mut s, Code::Enter);
        assert!(moved(&mut s, 1.0, 1.0).is_empty());
        assert!(go(&mut s, Action::Undo).is_empty());
        assert!(key(&mut s, Code::Escape).is_empty());
    }
}
