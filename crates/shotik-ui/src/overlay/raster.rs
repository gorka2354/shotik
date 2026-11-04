//! Burns annotations into pixels: `drawAnno` from `overlay.js`. The same code fills the view's layer and the
//! output image, so what the user sees is what leaves the overlay, including what Slint cannot draw itself:
//! canvas shadows and the highlighter's multiply blend.
//!
//! Everything draws into a `Canvas`, an opaque RGB window on the frame, in image pixels. Drawing into a small
//! window costs little however long the stroke is, because paths are cut down to what can reach it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use parley::{FontContext, FontFamily, FontWeight, Layout, LayoutContext, PositionedLayoutItem, StyleProperty};
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{self, Cap, Command, Fill, Format, Join, Mask, Point, Scratch, Stroke, Vector};
use swash::FontRef;

use super::pixels::RgbImage;
use super::state::{js_round, Annotation, IRect, Pt, RectF, Rgb, Shape};

/// An opaque RGB window on the frame. `data` holds the pixels of `area`, rows `stride` bytes apart.
pub struct Canvas<'a> {
    pub data: &'a mut [u8],
    pub stride: usize,
    /// Which image pixels this is.
    pub area: IRect,
}

impl Canvas<'_> {
    fn row_mut(&mut self, y: i32) -> &mut [u8] {
        let start = (y - self.area.y) as usize * self.stride;
        &mut self.data[start..start + self.area.w as usize * 3]
    }
}

/// Coverage for a rectangle of image pixels.
#[derive(Default)]
struct Coverage {
    area: IRect,
    data: Vec<u8>,
}

impl Coverage {
    fn reset(&mut self, area: IRect) {
        self.area = area;
        self.data.clear();
        self.data.resize(area.w as usize * area.h as usize, 0);
    }

    fn is_empty(&self) -> bool {
        self.data.iter().all(|&c| c == 0)
    }

    fn row(&self, y: i32) -> Option<&[u8]> {
        if y < self.area.y || y >= self.area.y + self.area.h {
            return None;
        }
        let w = self.area.w as usize;
        let start = (y - self.area.y) as usize * w;
        Some(&self.data[start..start + w])
    }
}

/// `(a * b) / 255`, exactly.
#[inline]
fn mul255(a: u32, b: u32) -> u32 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

fn intersect(a: IRect, b: IRect) -> Option<IRect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.w).min(b.x + b.w);
    let bottom = (a.y + a.h).min(b.y + b.h);
    (right > x && bottom > y).then_some(IRect { x, y, w: right - x, h: bottom - y })
}

fn inflate(r: IRect, by: i32) -> IRect {
    IRect { x: r.x - by, y: r.y - by, w: r.w + 2 * by, h: r.h + 2 * by }
}

fn point(p: Pt) -> Point {
    Point::new(p.x as f32, p.y as f32)
}

fn mid(a: Pt, b: Pt) -> Pt {
    Pt { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 }
}

/// Whether a box around these points, grown by `margin`, can touch `area`.
fn near(points: &[Pt], margin: f64, area: IRect) -> bool {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in points {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    x1 + margin >= f64::from(area.x)
        && x0 - margin <= f64::from(area.x + area.w)
        && y1 + margin >= f64::from(area.y)
        && y0 - margin <= f64::from(area.y + area.h)
}

/// Canvas shadow of `shadow(c)`: `blur` is the CSS `shadowBlur` in image pixels.
#[derive(Clone, Copy, Debug)]
struct ShadowSpec {
    /// Sizes of the three box blurs that approximate the Gaussian: (left, right) reach of each.
    boxes: [(usize, usize); 3],
    /// Shadow reach beyond the shape.
    reach: i32,
    offset_y: i32,
}

impl ShadowSpec {
    fn new(kx: f64) -> Self {
        // A canvas shadow is a Gaussian with sigma = shadowBlur / 2. Three box blurs, as the SVG spec says.
        let sigma = 5.0 * kx / 2.0;
        let d = (sigma * 3.0 * (2.0 * std::f64::consts::PI).sqrt() / 4.0 + 0.5).floor().max(1.0) as usize;
        let boxes = if d % 2 == 1 {
            [(d / 2, d / 2), (d / 2, d / 2), (d / 2, d / 2)]
        } else {
            [(d / 2, d / 2 - 1), (d / 2 - 1, d / 2), (d / 2, d / 2)]
        };
        let reach = boxes.iter().map(|b| b.0.max(b.1)).sum::<usize>() as i32 + 1;
        Self { boxes, reach, offset_y: js_round(1.5 * kx) as i32 }
    }
}

const SHADOW_ALPHA: u32 = 97; // 0.38 of 255
const HIGHLIGHT_ALPHA: u32 = 107; // 0.42 of 255

/// A line of text rasterized once; both views of it share this.
pub struct TextRun {
    /// Coverage, placed relative to the pen origin on the baseline.
    left: i32,
    top: i32,
    width: usize,
    height: usize,
    mask: Vec<u8>,
    /// Colour glyphs (emoji): RGBA premultiplied, same placement rules.
    colored: Vec<ColorGlyph>,
    pub advance: f64,
    pub ascent: f64,
    pub descent: f64,
}

struct ColorGlyph {
    left: i32,
    top: i32,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

/// Text laid out so far, shared between the view and the thread that renders the output image.
#[derive(Default)]
pub struct TextCache {
    runs: Mutex<HashMap<(String, u32), Arc<TextRun>>>,
}

impl TextCache {
    fn run(&self, text: &str, px: f64) -> Arc<TextRun> {
        let key = (text.to_string(), (px as f32).to_bits());
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        runs.entry(key).or_insert_with(|| Arc::new(ENGINE.with(|e| e.borrow_mut().get_or_insert_with(TextEngine::new).render(text, px as f32)))).clone()
    }
}

thread_local! {
    /// Font database and shaper. Heavy, so only threads that really draw text get one.
    static ENGINE: std::cell::RefCell<Option<TextEngine>> = const { std::cell::RefCell::new(None) };
}

/// Frees this thread's text engine. The overlay calls it at session end.
pub fn free_text_engine() {
    ENGINE.with(|e| e.borrow_mut().take());
}

struct TextEngine {
    fonts: FontContext,
    layout: LayoutContext<[u8; 4]>,
    scale: ScaleContext,
}

impl TextEngine {
    fn new() -> Self {
        Self { fonts: FontContext::new(), layout: LayoutContext::new(), scale: ScaleContext::new() }
    }

    /// `700 ${px}px 'Segoe UI', system-ui, sans-serif`.
    fn render(&mut self, text: &str, px: f32) -> TextRun {
        let mut builder = self.layout.ranged_builder(&mut self.fonts, text, 1.0, false);
        builder.push_default(StyleProperty::FontFamily(FontFamily::from("'Segoe UI', system-ui, sans-serif")));
        builder.push_default(StyleProperty::FontSize(px));
        builder.push_default(StyleProperty::FontWeight(FontWeight::BOLD));
        let mut layout: Layout<[u8; 4]> = builder.build(text);
        layout.break_all_lines(None);

        struct Placed {
            left: i32,
            top: i32,
            width: usize,
            height: usize,
            data: Vec<u8>,
            color: bool,
        }
        let mut placed: Vec<Placed> = Vec::new();
        let (mut advance, mut ascent, mut descent) = (0.0_f64, 0.0_f64, 0.0_f64);
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else { continue };
                let run = glyph_run.run();
                advance = advance.max(f64::from(glyph_run.offset() + glyph_run.advance()));
                ascent = ascent.max(f64::from(run.metrics().ascent));
                descent = descent.max(f64::from(run.metrics().descent));
                let font = run.font();
                let Some(font_ref) = FontRef::from_index(font.data.as_ref(), font.index as usize) else { continue };
                let synthesis = run.synthesis();
                let mut scaler = self
                    .scale
                    .builder(font_ref)
                    .size(run.font_size())
                    .hint(false)
                    .normalized_coords(run.normalized_coords())
                    .build();
                let baseline = glyph_run.baseline();
                for glyph in glyph_run.positioned_glyphs() {
                    let x = f64::from(glyph.x);
                    let origin = x.floor();
                    let mut render = Render::new(&[Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline]);
                    render.format(Format::Alpha).offset(Vector::new((x - origin) as f32, 0.0));
                    if synthesis.embolden() {
                        render.embolden(px / 24.0);
                    }
                    let Some(image) = render.render(&mut scaler, glyph.id as u16) else { continue };
                    if image.placement.width == 0 || image.placement.height == 0 {
                        continue;
                    }
                    placed.push(Placed {
                        left: origin as i32 + image.placement.left,
                        top: image.placement.top + (baseline - glyph.y).round() as i32,
                        width: image.placement.width as usize,
                        height: image.placement.height as usize,
                        color: matches!(image.content, Content::Color),
                        data: image.data,
                    });
                }
            }
        }

        let left = placed.iter().map(|g| g.left).min().unwrap_or(0);
        let top = placed.iter().map(|g| g.top).max().unwrap_or(0);
        let right = placed.iter().map(|g| g.left + g.width as i32).max().unwrap_or(0);
        let bottom = placed.iter().map(|g| g.top - g.height as i32).min().unwrap_or(0);
        let width = (right - left).max(0) as usize;
        let height = (top - bottom).max(0) as usize;
        let mut mask = vec![0_u8; width * height];
        let mut colored = Vec::new();
        for glyph in placed {
            if glyph.color {
                colored.push(ColorGlyph {
                    left: glyph.left,
                    top: glyph.top,
                    width: glyph.width,
                    height: glyph.height,
                    rgba: glyph.data,
                });
                continue;
            }
            let dx = (glyph.left - left) as usize;
            let dy = (top - glyph.top) as usize;
            for row in 0..glyph.height {
                let src = &glyph.data[row * glyph.width..(row + 1) * glyph.width];
                let dst = &mut mask[(dy + row) * width + dx..(dy + row) * width + dx + glyph.width];
                for (d, &s) in dst.iter_mut().zip(src) {
                    *d = (*d).max(s);
                }
            }
        }
        TextRun { left, top, width, height, mask, colored, advance, ascent, descent }
    }
}

/// `ctx.font`'s ascent the way Blink uses it for `textBaseline`.
fn rounded(metric: f64) -> f64 {
    js_round(metric)
}

/// Draws annotations. Reusable between calls: it keeps its scratch buffers.
pub struct Painter {
    kx: f64,
    shadow: ShadowSpec,
    text: Arc<TextCache>,
    scratch: Scratch,
    commands: Vec<Command>,
    coverage: Coverage,
    blurred: Coverage,
    temp: Vec<u8>,
}

impl Painter {
    pub fn new(kx: f64, text: Arc<TextCache>) -> Self {
        Self {
            kx,
            shadow: ShadowSpec::new(kx),
            text,
            scratch: Scratch::new(),
            commands: Vec::new(),
            coverage: Coverage::default(),
            blurred: Coverage::default(),
            temp: Vec::new(),
        }
    }

    /// How far paint can reach beyond an annotation's geometry.
    pub fn paint_reach(&self, annotation: &Annotation) -> f64 {
        let shadow = f64::from(self.shadow.reach + self.shadow.offset_y);
        match annotation {
            Annotation::Pen { stroke, .. } | Annotation::Line(Shape { stroke, .. }) | Annotation::Ellipse(Shape { stroke, .. }) => {
                stroke * self.kx / 2.0 + shadow + 1.0
            }
            Annotation::Rect(Shape { stroke, .. }) => stroke * self.kx + shadow + 1.0,
            Annotation::Arrow(Shape { stroke, .. }) => stroke * self.kx / 2.0 + shadow + 1.0,
            Annotation::Highlight { stroke, .. } => stroke * self.kx * 3.2 / 2.0 + 1.0,
            Annotation::Blur(_) => 1.0,
            Annotation::Text { .. } | Annotation::Counter { .. } => shadow + 1.0,
        }
    }

    /// The image pixels this annotation can touch, before clipping to the frame.
    pub fn bounds(&self, annotation: &Annotation) -> Option<IRect> {
        let reach = self.paint_reach(annotation);
        let around = |x0: f64, y0: f64, x1: f64, y1: f64| -> IRect {
            let (x, y) = ((x0 - reach).floor() as i32, (y0 - reach).floor() as i32);
            IRect { x, y, w: (x1 + reach).ceil() as i32 - x, h: (y1 + reach).ceil() as i32 - y }
        };
        match annotation {
            Annotation::Pen { points, .. } | Annotation::Highlight { points, .. } => {
                if points.is_empty() {
                    return None;
                }
                let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for p in points {
                    x0 = x0.min(p.x);
                    y0 = y0.min(p.y);
                    x1 = x1.max(p.x);
                    y1 = y1.max(p.y);
                }
                Some(around(x0, y0, x1, y1))
            }
            Annotation::Line(s) | Annotation::Rect(s) | Annotation::Ellipse(s) | Annotation::Blur(s) => {
                Some(around(s.x1.min(s.x2), s.y1.min(s.y2), s.x1.max(s.x2), s.y1.max(s.y2)))
            }
            Annotation::Arrow(s) => {
                let lw = s.stroke * self.kx;
                let head = (10.0 * self.kx).max(lw * 3.2);
                Some(around(s.x1.min(s.x2 - head), s.y1.min(s.y2 - head), s.x1.max(s.x2 + head), s.y1.max(s.y2 + head)))
            }
            Annotation::Text { x, y, lines, font_px, .. } => {
                let fp = font_px * self.kx;
                let (mut left, mut right) = (*x, *x);
                let (mut top, mut bottom) = (*y, *y);
                for (i, line) in lines.iter().enumerate() {
                    if line.is_empty() {
                        continue;
                    }
                    let run = self.text.run(line, fp);
                    let baseline = y + i as f64 * fp * 1.25 + rounded(run.ascent);
                    left = left.min(x + f64::from(run.left));
                    right = right.max(x + f64::from(run.left) + run.width as f64 + 2.0);
                    top = top.min(baseline - f64::from(run.top) - 2.0);
                    bottom = bottom.max(baseline - f64::from(run.top) + run.height as f64 + 2.0);
                }
                Some(around(left, top, right, bottom))
            }
            Annotation::Counter { x, y, .. } => {
                let r = 15.0 * self.kx;
                Some(around(x - r, y - r, x + r, y + r))
            }
        }
    }

    /// Whether this annotation can paint inside `area`: cheaper than finding out by drawing, and it spares
    /// a long diagonal line from repainting everything in its bounding box.
    pub fn touches(&self, annotation: &Annotation, area: IRect) -> bool {
        let reach = self.paint_reach(annotation);
        let Some(bounds) = self.bounds(annotation) else { return false };
        if intersect(bounds, area).is_none() {
            return false;
        }
        match annotation {
            Annotation::Line(s) | Annotation::Arrow(s) => {
                let head = match annotation {
                    Annotation::Arrow(s) => (10.0 * self.kx).max(s.stroke * self.kx * 3.2),
                    _ => 0.0,
                };
                near(&[Pt { x: s.x2, y: s.y2 }], reach + head, area) || segment_near_rect(s, reach, area)
            }
            Annotation::Rect(s) => {
                let r = RectF::from_corners(s.x1, s.y1, s.x2, s.y2);
                // Nothing to paint strictly inside the box.
                !(f64::from(area.x) > r.x + reach
                    && f64::from(area.y) > r.y + reach
                    && f64::from(area.x + area.w) < r.x + r.w - reach
                    && f64::from(area.y + area.h) < r.y + r.h - reach)
            }
            Annotation::Ellipse(s) => {
                let r = RectF::from_corners(s.x1, s.y1, s.x2, s.y2);
                let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                let (rx, ry) = (r.w / 2.0 - reach, r.h / 2.0 - reach);
                if rx <= 0.0 || ry <= 0.0 {
                    return true;
                }
                let inside = |x: f64, y: f64| ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) < 1.0;
                let (ax, ay) = (f64::from(area.x), f64::from(area.y));
                let (bx, by) = (ax + f64::from(area.w), ay + f64::from(area.h));
                !(inside(ax, ay) && inside(bx, ay) && inside(ax, by) && inside(bx, by))
            }
            Annotation::Pen { points, .. } | Annotation::Highlight { points, .. } => {
                points.windows(2).any(|p| near(p, reach, area))
            }
            _ => true,
        }
    }

    /// `drawAnno`, clipped to the canvas.
    pub fn draw(&mut self, canvas: &mut Canvas<'_>, annotation: &Annotation, source: RgbImage<'_>) {
        if !self.touches(annotation, canvas.area) {
            return;
        }
        let kx = self.kx;
        match annotation {
            Annotation::Pen { points, color, stroke } => {
                let lw = stroke * kx;
                self.pen_path(points, lw / 2.0 + f64::from(self.shadow.reach + self.shadow.offset_y) + 1.0, canvas.area);
                let mut style = Stroke::new(lw as f32);
                style.cap(Cap::Round).join(Join::Round);
                self.stroke_with_shadow(canvas, style, *color);
            }
            Annotation::Highlight { points, color, stroke } => {
                let lw = stroke * kx * 3.2;
                self.polyline_path(points, lw / 2.0 + 1.0, canvas.area);
                let mut style = Stroke::new(lw as f32);
                style.cap(Cap::Butt).join(Join::Round);
                let area = canvas.area;
                self.rasterize(area, StyleKind::Stroke(style));
                if !self.coverage.is_empty() {
                    composite_multiply(canvas, &self.coverage, *color);
                }
            }
            Annotation::Line(s) => self.line(canvas, s),
            Annotation::Arrow(s) => {
                self.line(canvas, s);
                let angle = (s.y2 - s.y1).atan2(s.x2 - s.x1);
                let lw = s.stroke * kx;
                let head = (10.0 * kx).max(lw * 3.2);
                self.commands.clear();
                self.commands.push(Command::MoveTo(point(Pt { x: s.x2, y: s.y2 })));
                for side in [-0.45, 0.45] {
                    let p = Pt { x: s.x2 - head * (angle + side).cos(), y: s.y2 - head * (angle + side).sin() };
                    self.commands.push(Command::LineTo(point(p)));
                }
                self.commands.push(Command::Close);
                self.fill_with_shadow(canvas, s.color);
            }
            Annotation::Rect(s) => {
                let r = RectF::from_corners(s.x1, s.y1, s.x2, s.y2);
                rounded_rect_path(&mut self.commands, r, (6.0 * kx).min(r.w / 4.0).min(r.h / 4.0));
                let mut style = Stroke::new((s.stroke * kx) as f32);
                style.cap(Cap::Butt).join(Join::Miter).miter_limit(10.0);
                self.stroke_with_shadow(canvas, style, s.color);
            }
            Annotation::Ellipse(s) => {
                let r = RectF::from_corners(s.x1, s.y1, s.x2, s.y2);
                ellipse_path(&mut self.commands, r);
                let mut style = Stroke::new((s.stroke * kx) as f32);
                style.cap(Cap::Butt).join(Join::Miter).miter_limit(10.0);
                self.stroke_with_shadow(canvas, style, s.color);
            }
            Annotation::Blur(s) => self.pixelate(canvas, s, source),
            Annotation::Text { x, y, lines, color, font_px } => {
                let fp = font_px * kx;
                for (i, line) in lines.iter().enumerate() {
                    if line.is_empty() {
                        continue;
                    }
                    let run = self.text.run(line, fp);
                    let origin_x = js_round(*x) as i32;
                    let baseline = js_round(y + i as f64 * fp * 1.25 + rounded(run.ascent)) as i32;
                    self.draw_run(canvas, &run, origin_x, baseline, *color, true);
                }
            }
            Annotation::Counter { x, y, n, color } => {
                let r = 15.0 * kx;
                ellipse_path(&mut self.commands, RectF::new(x - r, y - r, 2.0 * r, 2.0 * r));
                self.fill_with_shadow(canvas, *color);
                let run = self.text.run(&n.to_string(), r * 1.15);
                let origin_x = js_round(x - run.advance / 2.0) as i32;
                let baseline = js_round(y + r * 0.05 + (rounded(run.ascent) - rounded(run.descent)) / 2.0) as i32;
                self.draw_run(canvas, &run, origin_x, baseline, color.light_or_dark(), false);
            }
        }
    }

    fn line(&mut self, canvas: &mut Canvas<'_>, s: &Shape) {
        self.commands.clear();
        self.commands.push(Command::MoveTo(point(Pt { x: s.x1, y: s.y1 })));
        self.commands.push(Command::LineTo(point(Pt { x: s.x2, y: s.y2 })));
        let mut style = Stroke::new((s.stroke * self.kx) as f32);
        style.cap(Cap::Round);
        self.stroke_with_shadow(canvas, style, s.color);
    }

    /// The pen's curve through midpoints, keeping only pieces that can reach `area`.
    fn pen_path(&mut self, points: &[Pt], reach: f64, area: IRect) {
        self.commands.clear();
        let n = points.len();
        if n < 2 {
            return;
        }
        let mut connected = false;
        let mut from = points[0];
        for i in 1..n.saturating_sub(1) {
            let to = mid(points[i], points[i + 1]);
            if near(&[from, points[i], to], reach, area) {
                if !connected {
                    self.commands.push(Command::MoveTo(point(from)));
                    connected = true;
                }
                self.commands.push(Command::QuadTo(point(points[i]), point(to)));
            } else {
                connected = false;
            }
            from = to;
        }
        let last = points[n - 1];
        if near(&[from, last], reach, area) {
            if !connected {
                self.commands.push(Command::MoveTo(point(from)));
            }
            self.commands.push(Command::LineTo(point(last)));
        }
    }

    /// The highlighter's straight segments, keeping only those that can reach `area`.
    fn polyline_path(&mut self, points: &[Pt], reach: f64, area: IRect) {
        self.commands.clear();
        let mut connected = false;
        for pair in points.windows(2) {
            if near(pair, reach, area) {
                if !connected {
                    self.commands.push(Command::MoveTo(point(pair[0])));
                    connected = true;
                }
                self.commands.push(Command::LineTo(point(pair[1])));
            } else {
                connected = false;
            }
        }
    }

    fn rasterize(&mut self, area: IRect, style: StyleKind<'_>) {
        self.coverage.reset(area);
        if self.commands.is_empty() {
            return;
        }
        let commands: &[Command] = &self.commands;
        let mut mask = Mask::with_scratch(commands, &mut self.scratch);
        match style {
            StyleKind::Stroke(stroke) => mask.style(stroke),
            StyleKind::Fill => mask.style(Fill::NonZero),
        };
        mask.format(Format::Alpha)
            .size(area.w as u32, area.h as u32)
            .offset(zeno::Vector::new(-area.x as f32, -area.y as f32));
        mask.render_into(&mut self.coverage.data, None);
    }

    fn stroke_with_shadow(&mut self, canvas: &mut Canvas<'_>, style: Stroke<'_>, color: Rgb) {
        let reach = self.shadow.reach + self.shadow.offset_y;
        self.rasterize(inflate(canvas.area, reach), StyleKind::Stroke(style));
        self.paint_coverage(canvas, color, true);
    }

    fn fill_with_shadow(&mut self, canvas: &mut Canvas<'_>, color: Rgb) {
        let reach = self.shadow.reach + self.shadow.offset_y;
        self.rasterize(inflate(canvas.area, reach), StyleKind::Fill);
        self.paint_coverage(canvas, color, true);
    }

    fn paint_coverage(&mut self, canvas: &mut Canvas<'_>, color: Rgb, shadow: bool) {
        if self.coverage.is_empty() {
            return;
        }
        if shadow {
            self.blur_coverage();
            composite_shadow(canvas, &self.blurred, self.shadow.offset_y);
        }
        composite_color(canvas, &self.coverage, color);
    }

    /// Fills `blurred` from `coverage` with three box blurs.
    fn blur_coverage(&mut self) {
        let area = self.coverage.area;
        let (w, h) = (area.w as usize, area.h as usize);
        self.blurred.reset(area);
        self.temp.clear();
        self.temp.resize(w * h, 0);
        let [first, second, third] = self.shadow.boxes;

        box_horizontal(&self.coverage.data, &mut self.temp, w, first);
        box_vertical(&self.temp, &mut self.blurred.data, w, h, first);

        box_horizontal(&self.blurred.data, &mut self.temp, w, second);
        box_vertical(&self.temp, &mut self.blurred.data, w, h, second);

        box_horizontal(&self.blurred.data, &mut self.temp, w, third);
        box_vertical(&self.temp, &mut self.blurred.data, w, h, third);
    }

    /// Copies a run onto the coverage buffer around the canvas, then paints it.
    fn draw_run(&mut self, canvas: &mut Canvas<'_>, run: &TextRun, origin_x: i32, baseline: i32, color: Rgb, shadow: bool) {
        let reach = if shadow { self.shadow.reach + self.shadow.offset_y } else { 0 };
        let area = inflate(canvas.area, reach);
        self.coverage.reset(area);
        let run_rect = IRect {
            x: origin_x + run.left,
            y: baseline - run.top,
            w: run.width as i32,
            h: run.height as i32,
        };
        if let Some(hit) = intersect(run_rect, area) {
            for y in hit.y..hit.y + hit.h {
                let src_row = (y - run_rect.y) as usize * run.width;
                let src = &run.mask[src_row + (hit.x - run_rect.x) as usize..][..hit.w as usize];
                let dst_row = (y - area.y) as usize * area.w as usize;
                self.coverage.data[dst_row + (hit.x - area.x) as usize..][..hit.w as usize].copy_from_slice(src);
            }
        }
        // Colour glyphs have no ink of their own colour: their alpha still casts the shadow.
        if shadow {
            for glyph in &run.colored {
                add_alpha(&mut self.coverage, glyph, origin_x, baseline);
            }
        }
        if !self.coverage.is_empty() {
            if shadow {
                self.blur_coverage();
                composite_shadow(canvas, &self.blurred, self.shadow.offset_y);
            }
            for glyph in &run.colored {
                clear_alpha(&mut self.coverage, glyph, origin_x, baseline);
            }
            composite_color(canvas, &self.coverage, color);
        }
        for glyph in &run.colored {
            composite_glyph(canvas, glyph, origin_x, baseline);
        }
    }

    /// `case 'blur'`: a coarse grid of averages over the pristine frame, never over other annotations.
    fn pixelate(&mut self, canvas: &mut Canvas<'_>, s: &Shape, source: RgbImage<'_>) {
        let r = RectF::from_corners(s.x1, s.y1, s.x2, s.y2);
        if r.w < 2.0 || r.h < 2.0 {
            return;
        }
        let block = (8.0 * self.kx).max(r.w.min(r.h) / 12.0);
        let cols = js_round(r.w / block).max(1.0) as i32;
        let rows = js_round(r.h / block).max(1.0) as i32;
        let (rx, ry) = (js_round(r.x) as i32, js_round(r.y) as i32);
        let (rw, rh) = (js_round(r.w) as i32, js_round(r.h) as i32);
        let Some(hit) = intersect(IRect::new(rx, ry, rw, rh), canvas.area) else { return };
        // The cells are aligned to the patch, so any window on it shows the same cell colours.
        let cell_of = |offset: i32, cells: i32, total: i32| (i64::from(offset) * i64::from(cells) / i64::from(total)) as i32;
        let first_col = cell_of(hit.x - rx, cols, rw);
        let last_col = cell_of(hit.x + hit.w - 1 - rx, cols, rw);
        let first_row = cell_of(hit.y - ry, rows, rh);
        let last_row = cell_of(hit.y + hit.h - 1 - ry, rows, rh);
        let edge = |cell: i32, cells: i32, total: i32| {
            // First pixel whose cell is `cell`.
            ((i64::from(cell) * i64::from(total) + i64::from(cells) - 1) / i64::from(cells)) as i32
        };
        for row in first_row..=last_row {
            let (y0, y1) = (ry + edge(row, rows, rh), ry + edge(row + 1, rows, rh));
            for col in first_col..=last_col {
                let (x0, x1) = (rx + edge(col, cols, rw), rx + edge(col + 1, cols, rw));
                let [cr, cg, cb] = average(source, x0, y0, x1, y1);
                let Some(cell) = intersect(IRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }, hit) else { continue };
                for y in cell.y..cell.y + cell.h {
                    let left = (cell.x - canvas.area.x) as usize * 3;
                    let row_px = &mut canvas.row_mut(y)[left..left + cell.w as usize * 3];
                    for px in row_px.chunks_exact_mut(3) {
                        px[0] = cr;
                        px[1] = cg;
                        px[2] = cb;
                    }
                }
            }
        }
    }
}

enum StyleKind<'a> {
    Stroke(Stroke<'a>),
    Fill,
}

fn segment_near_rect(s: &Shape, reach: f64, area: IRect) -> bool {
    // Distance from the rectangle's centre to the segment, against half its diagonal plus the reach.
    let (cx, cy) = (f64::from(area.x) + f64::from(area.w) / 2.0, f64::from(area.y) + f64::from(area.h) / 2.0);
    let radius = f64::from(area.w).hypot(f64::from(area.h)) / 2.0 + reach;
    let (dx, dy) = (s.x2 - s.x1, s.y2 - s.y1);
    let length2 = dx * dx + dy * dy;
    let t = if length2 == 0.0 { 0.0 } else { (((cx - s.x1) * dx + (cy - s.y1) * dy) / length2).clamp(0.0, 1.0) };
    (s.x1 + t * dx - cx).hypot(s.y1 + t * dy - cy) <= radius
}

/// `roundRect` with one radius.
fn rounded_rect_path(commands: &mut Vec<Command>, r: RectF, radius: f64) {
    const KAPPA: f64 = 0.552_284_75;
    commands.clear();
    let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
    let c = radius * (1.0 - KAPPA);
    let p = |x: f64, y: f64| point(Pt { x, y });
    commands.push(Command::MoveTo(p(x0 + radius, y0)));
    commands.push(Command::LineTo(p(x1 - radius, y0)));
    commands.push(Command::CurveTo(p(x1 - c, y0), p(x1, y0 + c), p(x1, y0 + radius)));
    commands.push(Command::LineTo(p(x1, y1 - radius)));
    commands.push(Command::CurveTo(p(x1, y1 - c), p(x1 - c, y1), p(x1 - radius, y1)));
    commands.push(Command::LineTo(p(x0 + radius, y1)));
    commands.push(Command::CurveTo(p(x0 + c, y1), p(x0, y1 - c), p(x0, y1 - radius)));
    commands.push(Command::LineTo(p(x0, y0 + radius)));
    commands.push(Command::CurveTo(p(x0, y0 + c), p(x0 + c, y0), p(x0 + radius, y0)));
    commands.push(Command::Close);
}

fn ellipse_path(commands: &mut Vec<Command>, r: RectF) {
    const KAPPA: f64 = 0.552_284_75;
    commands.clear();
    let (rx, ry) = (r.w / 2.0, r.h / 2.0);
    let (cx, cy) = (r.x + rx, r.y + ry);
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    let p = |x: f64, y: f64| point(Pt { x, y });
    commands.push(Command::MoveTo(p(cx + rx, cy)));
    commands.push(Command::CurveTo(p(cx + rx, cy + ky), p(cx + kx, cy + ry), p(cx, cy + ry)));
    commands.push(Command::CurveTo(p(cx - kx, cy + ry), p(cx - rx, cy + ky), p(cx - rx, cy)));
    commands.push(Command::CurveTo(p(cx - rx, cy - ky), p(cx - kx, cy - ry), p(cx, cy - ry)));
    commands.push(Command::CurveTo(p(cx + kx, cy - ry), p(cx + rx, cy - ky), p(cx + rx, cy)));
    commands.push(Command::Close);
}

/// One box pass along rows. Pixels past the edges count as empty.
fn box_horizontal(src: &[u8], dst: &mut [u8], width: usize, (left, right): (usize, usize)) {
    let size = (left + right + 1) as u32;
    let multiplier = (1_u32 << 16) / size;
    for (src_row, dst_row) in src.chunks_exact(width).zip(dst.chunks_exact_mut(width)) {
        let mut sum: u32 = src_row[..=right.min(width - 1)].iter().map(|&v| u32::from(v)).sum();
        for x in 0..width {
            dst_row[x] = ((sum * multiplier + (1 << 15)) >> 16) as u8;
            if x + right + 1 < width {
                sum += u32::from(src_row[x + right + 1]);
            }
            if x >= left {
                sum -= u32::from(src_row[x - left]);
            }
        }
    }
}

/// One box pass along columns, a row at a time so memory is read in order.
fn box_vertical(src: &[u8], dst: &mut [u8], width: usize, height: usize, (up, down): (usize, usize)) {
    let size = (up + down + 1) as u32;
    let multiplier = (1_u32 << 16) / size;
    let mut sums = vec![0_u32; width];
    for row in src.chunks_exact(width).take(down + 1) {
        for (sum, &v) in sums.iter_mut().zip(row) {
            *sum += u32::from(v);
        }
    }
    for y in 0..height {
        let out = &mut dst[y * width..(y + 1) * width];
        for (d, &sum) in out.iter_mut().zip(&sums) {
            *d = ((sum * multiplier + (1 << 15)) >> 16) as u8;
        }
        if y + down + 1 < height {
            let row = &src[(y + down + 1) * width..(y + down + 2) * width];
            for (sum, &v) in sums.iter_mut().zip(row) {
                *sum += u32::from(v);
            }
        }
        if y >= up {
            let row = &src[(y - up) * width..(y - up + 1) * width];
            for (sum, &v) in sums.iter_mut().zip(row) {
                *sum -= u32::from(v);
            }
        }
    }
}

/// Black at `rgba(0,0,0,0.38)` through blurred coverage, shifted down.
fn composite_shadow(canvas: &mut Canvas<'_>, blurred: &Coverage, offset_y: i32) {
    let area = canvas.area;
    let skip = (area.x - blurred.area.x) as usize;
    for y in area.y..area.y + area.h {
        let Some(src) = blurred.row(y - offset_y) else { continue };
        let src = &src[skip..skip + area.w as usize];
        for (px, &cover) in canvas.row_mut(y).chunks_exact_mut(3).zip(src) {
            if cover == 0 {
                continue;
            }
            let keep = 255 - mul255(u32::from(cover), SHADOW_ALPHA);
            px[0] = mul255(u32::from(px[0]), keep) as u8;
            px[1] = mul255(u32::from(px[1]), keep) as u8;
            px[2] = mul255(u32::from(px[2]), keep) as u8;
        }
    }
}

fn composite_color(canvas: &mut Canvas<'_>, coverage: &Coverage, color: Rgb) {
    let area = canvas.area;
    let skip = (area.x - coverage.area.x) as usize;
    let (r, g, b) = (u32::from(color.0), u32::from(color.1), u32::from(color.2));
    for y in area.y..area.y + area.h {
        let Some(src) = coverage.row(y) else { continue };
        let src = &src[skip..skip + area.w as usize];
        for (px, &cover) in canvas.row_mut(y).chunks_exact_mut(3).zip(src) {
            match cover {
                0 => {}
                255 => {
                    px[0] = color.0;
                    px[1] = color.1;
                    px[2] = color.2;
                }
                _ => {
                    let a = u32::from(cover);
                    let keep = 255 - a;
                    px[0] = (mul255(u32::from(px[0]), keep) + mul255(r, a)) as u8;
                    px[1] = (mul255(u32::from(px[1]), keep) + mul255(g, a)) as u8;
                    px[2] = (mul255(u32::from(px[2]), keep) + mul255(b, a)) as u8;
                }
            }
        }
    }
}

/// `globalAlpha = 0.42`, `globalCompositeOperation = 'multiply'`.
fn composite_multiply(canvas: &mut Canvas<'_>, coverage: &Coverage, color: Rgb) {
    let area = canvas.area;
    let skip = (area.x - coverage.area.x) as usize;
    let (r, g, b) = (u32::from(color.0), u32::from(color.1), u32::from(color.2));
    for y in area.y..area.y + area.h {
        let Some(src) = coverage.row(y) else { continue };
        let src = &src[skip..skip + area.w as usize];
        for (px, &cover) in canvas.row_mut(y).chunks_exact_mut(3).zip(src) {
            if cover == 0 {
                continue;
            }
            let a = mul255(u32::from(cover), HIGHLIGHT_ALPHA);
            let keep = 255 - a;
            for (channel, c) in px.iter_mut().zip([r, g, b]) {
                let d = u32::from(*channel);
                *channel = (mul255(d, keep) + mul255(mul255(d, c), a)) as u8;
            }
        }
    }
}

fn add_alpha(coverage: &mut Coverage, glyph: &ColorGlyph, origin_x: i32, baseline: i32) {
    for_glyph_pixels(coverage.area, glyph, origin_x, baseline, |x, y, rgba| {
        let at = (y - coverage.area.y) as usize * coverage.area.w as usize + (x - coverage.area.x) as usize;
        coverage.data[at] = coverage.data[at].max(rgba[3]);
    });
}

fn clear_alpha(coverage: &mut Coverage, glyph: &ColorGlyph, origin_x: i32, baseline: i32) {
    for_glyph_pixels(coverage.area, glyph, origin_x, baseline, |x, y, _| {
        let at = (y - coverage.area.y) as usize * coverage.area.w as usize + (x - coverage.area.x) as usize;
        coverage.data[at] = 0;
    });
}

fn composite_glyph(canvas: &mut Canvas<'_>, glyph: &ColorGlyph, origin_x: i32, baseline: i32) {
    let area = canvas.area;
    let stride = canvas.stride;
    for_glyph_pixels(area, glyph, origin_x, baseline, |x, y, rgba| {
        let alpha = u32::from(rgba[3]);
        if alpha == 0 {
            return;
        }
        let at = (y - area.y) as usize * stride + (x - area.x) as usize * 3;
        for (channel, &color) in canvas.data[at..at + 3].iter_mut().zip(rgba) {
            *channel = (mul255(u32::from(*channel), 255 - alpha) + mul255(u32::from(color), alpha)) as u8;
        }
    });
}

fn for_glyph_pixels(area: IRect, glyph: &ColorGlyph, origin_x: i32, baseline: i32, mut visit: impl FnMut(i32, i32, &[u8])) {
    let rect = IRect {
        x: origin_x + glyph.left,
        y: baseline - glyph.top,
        w: glyph.width as i32,
        h: glyph.height as i32,
    };
    let Some(hit) = intersect(rect, area) else { return };
    for y in hit.y..hit.y + hit.h {
        for x in hit.x..hit.x + hit.w {
            let at = ((y - rect.y) as usize * glyph.width + (x - rect.x) as usize) * 4;
            visit(x, y, &glyph.rgba[at..at + 4]);
        }
    }
}

/// Mean colour of a block of the source, sampling at most 16 by 16 points: a block never needs more, and a
/// huge pixelation drag stays cheap.
fn average(source: RgbImage<'_>, x0: i32, y0: i32, x1: i32, y1: i32) -> [u8; 3] {
    let x0 = x0.clamp(0, source.width - 1);
    let y0 = y0.clamp(0, source.height - 1);
    let x1 = x1.clamp(x0 + 1, source.width);
    let y1 = y1.clamp(y0 + 1, source.height);
    let step_x = ((x1 - x0) / 16).max(1);
    let step_y = ((y1 - y0) / 16).max(1);
    let (mut r, mut g, mut b, mut n) = (0_u32, 0_u32, 0_u32, 0_u32);
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let [pr, pg, pb] = source.pixel(x, y);
            r += u32::from(pr);
            g += u32::from(pg);
            b += u32::from(pb);
            n += 1;
            x += step_x;
        }
        y += step_y;
    }
    [(r / n) as u8, (g / n) as u8, (b / n) as u8]
}

/// Copies a rectangle of the frame into a tightly packed RGB buffer.
pub fn copy_area(source: RgbImage<'_>, area: IRect, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(area.w as usize * area.h as usize * 3);
    let stride = source.width as usize * 3;
    for y in area.y..area.y + area.h {
        let start = y as usize * stride + area.x as usize * 3;
        out.extend_from_slice(&source.data[start..start + area.w as usize * 3]);
    }
}

/// The output image: `area` of the pristine frame with the annotations on it, tightly packed RGB. This is
/// what `doAction` does with its offscreen canvas.
pub fn bake(source: RgbImage<'_>, area: IRect, annotations: &[Annotation], painter: &mut Painter) -> Vec<u8> {
    let mut out = Vec::new();
    copy_area(source, area, &mut out);
    let mut canvas = Canvas { data: &mut out, stride: area.w as usize * 3, area };
    for annotation in annotations {
        painter.draw(&mut canvas, annotation, source);
    }
    out
}

/// Tightly packed BGRA of an RGB picture, what `Frame` wants.
pub fn rgb_to_bgra(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.chunks_exact(3) {
        out.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::state::{Pt, Shape};

    const W: i32 = 400;
    const H: i32 = 300;

    fn frame() -> Vec<u8> {
        let mut data = Vec::with_capacity((W * H * 3) as usize);
        for y in 0..H {
            for x in 0..W {
                data.extend_from_slice(&[(x / 2 % 256) as u8, (y / 2 % 256) as u8, 120]);
            }
        }
        data
    }

    fn image(data: &[u8]) -> RgbImage<'_> {
        RgbImage { data, width: W, height: H }
    }

    fn painter() -> Painter {
        Painter::new(1.5, Arc::new(TextCache::default()))
    }

    fn pen(n: usize) -> Annotation {
        let points = (0..n)
            .map(|i| {
                let t = i as f64 / n as f64;
                Pt { x: 30.0 + t * 330.0, y: 150.0 + (t * 25.0).sin() * 100.0 }
            })
            .collect();
        Annotation::Pen { points, color: Rgb(255, 0, 0), stroke: 4.0 }
    }

    fn shape(x1: f64, y1: f64, x2: f64, y2: f64) -> Shape {
        Shape { x1, y1, x2, y2, color: Rgb(0, 255, 0), stroke: 4.0 }
    }

    fn pixel(baked: &[u8], area: IRect, x: i32, y: i32) -> [u8; 3] {
        let at = ((y - area.y) * area.w + (x - area.x)) as usize * 3;
        [baked[at], baked[at + 1], baked[at + 2]]
    }

    const WHOLE: IRect = IRect::new(0, 0, W, H);

    #[test]
    fn nothing_annotated_means_the_pristine_crop() {
        let data = frame();
        let area = IRect::new(10, 20, 30, 40);
        let out = bake(image(&data), area, &[], &mut painter());
        assert_eq!(out.len(), 30 * 40 * 3);
        assert_eq!(pixel(&out, area, 10, 20), image(&data).pixel(10, 20));
        assert_eq!(pixel(&out, area, 39, 59), image(&data).pixel(39, 59));
    }

    #[test]
    fn a_stroke_paints_its_colour_with_a_shadow_below() {
        let data = frame();
        let line = Annotation::Line(shape(50.0, 100.0, 350.0, 100.0));
        let out = bake(image(&data), WHOLE, &[line], &mut painter());
        assert_eq!(pixel(&out, WHOLE, 200, 100), [0, 255, 0], "the line itself");
        let below = pixel(&out, WHOLE, 200, 106);
        let before = image(&data).pixel(200, 106);
        assert!(below[2] < before[2], "shadow darkens below the line: {below:?} vs {before:?}");
        let far = pixel(&out, WHOLE, 200, 150);
        assert_eq!(far, image(&data).pixel(200, 150), "nothing far away");
    }

    #[test]
    fn the_highlighter_multiplies_over_what_is_under_it() {
        let data = vec![200_u8; (W * H * 3) as usize];
        let stroke = Annotation::Highlight {
            points: vec![Pt { x: 50.0, y: 100.0 }, Pt { x: 300.0, y: 100.0 }],
            color: Rgb(255, 224, 58),
            stroke: 4.0,
        };
        let out = bake(image(&data), WHOLE, &[stroke], &mut painter());
        let got = pixel(&out, WHOLE, 200, 100);
        // 0.42 of the way from 200 towards 200 * channel / 255.
        let expect = |c: f64| 200.0 * (1.0 - 0.42) + 0.42 * 200.0 * c / 255.0;
        for (channel, c) in got.iter().zip([255.0, 224.0, 58.0]) {
            assert!((f64::from(*channel) - expect(c)).abs() <= 2.0, "{got:?}");
        }
        assert_eq!(pixel(&out, WHOLE, 200, 130), [200, 200, 200], "width 4 * 1.5 * 3.2 ends well before this");
    }

    #[test]
    fn pixelation_averages_the_pristine_frame_into_blocks() {
        let mut data = vec![0_u8; (W * H * 3) as usize];
        for chunk in data.chunks_exact_mut(3).step_by(2) {
            chunk[0] = 100;
        }
        let red_box = Annotation::Rect(Shape { x1: 0.0, y1: 0.0, x2: 400.0, y2: 300.0, color: Rgb(255, 0, 0), stroke: 200.0 });
        let blur = Annotation::Blur(shape(100.0, 100.0, 220.0, 196.0));
        let out = bake(image(&data), WHOLE, &[red_box, blur], &mut painter());
        // Inside the patch: the mean of the source, not of the red box painted over it.
        let inside = pixel(&out, WHOLE, 130, 130);
        assert!((i32::from(inside[0]) - 50).abs() <= 5, "{inside:?}");
        // Blocks are flat: k = 1.5 gives 12 pixel blocks here.
        assert_eq!(pixel(&out, WHOLE, 101, 101), pixel(&out, WHOLE, 110, 110));
    }

    #[test]
    fn pixelation_below_two_pixels_draws_nothing() {
        let data = frame();
        let blur = Annotation::Blur(shape(100.0, 100.0, 101.0, 200.0));
        let out = bake(image(&data), WHOLE, &[blur], &mut painter());
        assert_eq!(out, data);
    }

    #[test]
    fn a_window_shows_exactly_the_same_pixels_as_the_whole() {
        let data = frame();
        let annotations = [
            pen(400),
            Annotation::Line(shape(20.0, 20.0, 380.0, 280.0)),
            Annotation::Arrow(shape(380.0, 20.0, 20.0, 280.0)),
            Annotation::Rect(shape(60.0, 60.0, 340.0, 240.0)),
            Annotation::Ellipse(shape(80.0, 40.0, 320.0, 260.0)),
            Annotation::Highlight {
                points: vec![Pt { x: 10.0, y: 280.0 }, Pt { x: 200.0, y: 200.0 }, Pt { x: 390.0, y: 280.0 }],
                color: Rgb(255, 224, 58),
                stroke: 7.0,
            },
            Annotation::Blur(shape(150.0, 100.0, 260.0, 170.0)),
            Annotation::Counter { x: 100.0, y: 100.0, n: 7, color: Rgb(255, 159, 46) },
        ];
        let mut p = painter();
        let whole = bake(image(&data), WHOLE, &annotations, &mut p);
        for (x, y, w, h) in [(0, 0, 40, 40), (90, 90, 64, 64), (128, 128, 128, 128), (300, 200, 100, 100), (200, 0, 16, 300)] {
            let area = IRect::new(x, y, w, h);
            let part = bake(image(&data), area, &annotations, &mut p);
            for py in y..y + h {
                for px in x..x + w {
                    let got = pixel(&part, area, px, py);
                    let want = pixel(&whole, WHOLE, px, py);
                    let close = got.iter().zip(want).all(|(a, b)| (i32::from(*a) - i32::from(b)).abs() <= 2);
                    assert!(close, "at {px},{py} in {area:?}: {got:?} vs {want:?}");
                }
            }
        }
    }

    #[test]
    fn bounds_contain_all_the_paint() {
        let data = frame();
        let mut p = painter();
        let cases = [
            pen(100),
            Annotation::Line(shape(100.0, 100.0, 200.0, 200.0)),
            Annotation::Arrow(shape(100.0, 100.0, 200.0, 200.0)),
            Annotation::Rect(shape(100.0, 100.0, 200.0, 200.0)),
            Annotation::Ellipse(shape(100.0, 100.0, 200.0, 200.0)),
            Annotation::Counter { x: 200.0, y: 150.0, n: 3, color: Rgb(255, 0, 0) },
        ];
        for annotation in cases {
            let bounds = p.bounds(&annotation).unwrap();
            let out = bake(image(&data), WHOLE, std::slice::from_ref(&annotation), &mut p);
            for y in 0..H {
                for x in 0..W {
                    let inside = x >= bounds.x && x < bounds.x + bounds.w && y >= bounds.y && y < bounds.y + bounds.h;
                    if !inside {
                        assert_eq!(pixel(&out, WHOLE, x, y), image(&data).pixel(x, y), "{annotation:?} at {x},{y} outside {bounds:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn touches_never_rejects_a_window_with_paint_in_it() {
        let data = frame();
        let mut p = painter();
        let cases = [
            Annotation::Line(shape(20.0, 20.0, 380.0, 280.0)),
            Annotation::Arrow(shape(380.0, 20.0, 20.0, 280.0)),
            Annotation::Rect(shape(60.0, 60.0, 340.0, 240.0)),
            Annotation::Ellipse(shape(80.0, 40.0, 320.0, 260.0)),
            pen(200),
        ];
        for annotation in cases {
            let whole = bake(image(&data), WHOLE, std::slice::from_ref(&annotation), &mut p);
            for ty in (0..H).step_by(32) {
                for tx in (0..W).step_by(32) {
                    let area = IRect::new(tx, ty, 32.min(W - tx), 32.min(H - ty));
                    if p.touches(&annotation, area) {
                        continue;
                    }
                    for y in area.y..area.y + area.h {
                        for x in area.x..area.x + area.w {
                            assert_eq!(pixel(&whole, WHOLE, x, y), image(&data).pixel(x, y), "{annotation:?}, {area:?} rejected");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_long_diagonal_skips_most_of_its_bounding_box() {
        let p = painter();
        let line = Annotation::Line(Shape { x1: 0.0, y1: 0.0, x2: 3840.0, y2: 2160.0, color: Rgb(1, 2, 3), stroke: 4.0 });
        let mut touched = 0;
        for ty in (0..2160).step_by(128) {
            for tx in (0..3840).step_by(128) {
                if p.touches(&line, IRect::new(tx, ty, 128, 128)) {
                    touched += 1;
                }
            }
        }
        assert!(touched < 120, "{touched} of 510 tiles");
    }

    #[test]
    fn a_counter_has_a_centred_digit_in_contrast_colour() {
        let data = vec![40_u8; (W * H * 3) as usize];
        let counter = Annotation::Counter { x: 200.0, y: 150.0, n: 1, color: Rgb(0, 0, 0) };
        let out = bake(image(&data), WHOLE, &[counter], &mut painter());
        assert_eq!(pixel(&out, WHOLE, 200 - 18, 150), [0, 0, 0], "inside the badge, left of the digit");
        let mut white = 0;
        for y in 140..160 {
            for x in 190..210 {
                if pixel(&out, WHOLE, x, y) == [255, 255, 255] {
                    white += 1;
                }
            }
        }
        assert!(white > 10, "the digit is there and white: {white}");
    }

    #[test]
    fn text_paints_inside_its_reported_bounds() {
        let data = vec![40_u8; (W * H * 3) as usize];
        let mut p = painter();
        let text = Annotation::Text { x: 30.0, y: 50.0, lines: vec!["Жабо Ay".into(), String::new(), "two".into()], color: Rgb(255, 255, 255), font_px: 23.0 };
        let bounds = p.bounds(&text).unwrap();
        let out = bake(image(&data), WHOLE, std::slice::from_ref(&text), &mut p);
        let (mut left, mut right, mut top, mut bottom) = (W, 0, H, 0);
        for y in 0..H {
            for x in 0..W {
                if pixel(&out, WHOLE, x, y)[0] > 200 {
                    left = left.min(x);
                    right = right.max(x);
                    top = top.min(y);
                    bottom = bottom.max(y);
                }
            }
        }
        assert!(right > left && bottom > top, "text painted");
        assert!(left >= bounds.x && right < bounds.x + bounds.w, "{left}..{right} in {bounds:?}");
        assert!(top >= bounds.y && bottom < bounds.y + bounds.h, "{top}..{bottom} in {bounds:?}");
        // textBaseline = top: ink starts below the anchor, and three lines reach past 2.5 line heights.
        let fp = 23.0 * 1.5;
        assert!(f64::from(top) > 50.0, "{top}");
        assert!(f64::from(bottom) > 50.0 + 2.0 * fp * 1.25, "{bottom}");
        assert!(left >= 29, "{left}");
    }

    #[test]
    fn output_pixels_convert_to_bgra() {
        assert_eq!(rgb_to_bgra(&[1, 2, 3, 4, 5, 6]), [3, 2, 1, 255, 6, 5, 4, 255]);
    }

    #[test]
    fn box_blurs_keep_total_energy_away_from_edges() {
        let (w, h) = (41, 41);
        let mut src = vec![0_u8; w * h];
        src[20 * w + 20] = 255;
        let mut once = vec![0_u8; w * h];
        let mut twice = vec![0_u8; w * h];
        box_horizontal(&src, &mut once, w, (1, 1));
        box_vertical(&once, &mut twice, w, h, (1, 1));
        assert_eq!(twice[20 * w + 20], 28, "255 / 9");
        assert_eq!(twice[19 * w + 21], 28, "the 3x3 box around the point");
        assert_eq!(twice[18 * w + 20], 0);
    }
}
