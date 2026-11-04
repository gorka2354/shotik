//! The annotation layer the view shows: a sparse grid of opaque tiles over the frozen frame. Slint repaints
//! an image item whole, so changing one big layer would repaint most of a 4K window on every pointer move.
//! With tiles, a stroke costs a few small rectangles, however long it already is.
//!
//! Each tile keeps `base`, the frame with committed annotations, and `display`, that plus the shape being
//! drawn. No toolkit types here.

use std::collections::BTreeSet;

use super::pixels::RgbImage;
use super::raster::{copy_area, Canvas, Painter};
use super::state::{Annotation, IRect, Pt};

/// Tile side in image pixels. Small enough that a pointer move repaints little, big enough that a 4K
/// screen stays around 500 tiles at worst.
pub const TILE: i32 = 128;

pub struct Tile {
    pub rect: IRect,
    base: Vec<u8>,
    pub display: Vec<u8>,
}

pub struct TileGrid {
    width: i32,
    height: i32,
    columns: i32,
    tiles: Vec<Option<Tile>>,
    /// Tiles whose `display` currently differs from `base`.
    live: BTreeSet<usize>,
    /// Tiles changed since the last `take_changed`.
    changed: BTreeSet<usize>,
}

impl TileGrid {
    pub fn new(width: i32, height: i32) -> Self {
        let columns = (width + TILE - 1) / TILE;
        let rows = (height + TILE - 1) / TILE;
        let mut tiles = Vec::new();
        tiles.resize_with((columns * rows) as usize, || None);
        Self { width, height, columns, tiles, live: BTreeSet::new(), changed: BTreeSet::new() }
    }

    /// Tiles that changed, for the view to show; each comes once.
    pub fn take_changed(&mut self) -> Vec<(usize, &Tile)> {
        let changed = std::mem::take(&mut self.changed);
        changed.into_iter().filter_map(|i| self.tiles[i].as_ref().map(|t| (i, t))).collect()
    }

    pub fn tile(&self, index: usize) -> Option<&Tile> {
        self.tiles[index].as_ref()
    }

    fn rect_of(&self, index: usize) -> IRect {
        let column = index as i32 % self.columns;
        let row = index as i32 / self.columns;
        let (x, y) = (column * TILE, row * TILE);
        IRect { x, y, w: TILE.min(self.width - x), h: TILE.min(self.height - y) }
    }

    /// Indices of the grid cells overlapping `area`.
    fn cells_in(&self, area: IRect) -> Vec<usize> {
        let x0 = area.x.max(0);
        let y0 = area.y.max(0);
        let x1 = (area.x + area.w).min(self.width);
        let y1 = (area.y + area.h).min(self.height);
        if x1 <= x0 || y1 <= y0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for row in y0 / TILE..=(y1 - 1) / TILE {
            for column in x0 / TILE..=(x1 - 1) / TILE {
                out.push((row * self.columns + column) as usize);
            }
        }
        out
    }

    fn ensure(&mut self, index: usize, source: RgbImage<'_>) -> &mut Tile {
        let rect = self.rect_of(index);
        self.tiles[index].get_or_insert_with(|| {
            let mut base = Vec::new();
            copy_area(source, rect, &mut base);
            Tile { rect, display: base.clone(), base }
        })
    }

    fn paint_display(tile: &mut Tile, area: IRect, annotation: &Annotation, source: RgbImage<'_>, painter: &mut Painter) {
        let stride = tile.rect.w as usize * 3;
        let start = (area.y - tile.rect.y) as usize * stride + (area.x - tile.rect.x) as usize * 3;
        // Back to the base first, so what was drawn here before does not stay.
        for row in 0..area.h as usize {
            let from = start + row * stride;
            let to = from + area.w as usize * 3;
            tile.display[from..to].copy_from_slice(&tile.base[from..to]);
        }
        let mut canvas = Canvas { data: &mut tile.display[start..], stride, area };
        painter.draw(&mut canvas, annotation, source);
    }

    /// A pen or highlighter drag got points from `from` on. Only the end of the path changes: the tail of
    /// the curve moved, so the two points before count too.
    pub fn live_extended(&mut self, live: &Annotation, from: usize, source: RgbImage<'_>, painter: &mut Painter) {
        let points = match live {
            Annotation::Pen { points, .. } | Annotation::Highlight { points, .. } => points,
            _ => return,
        };
        let recent: &[Pt] = &points[from.saturating_sub(2)..];
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in recent {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        let reach = painter.paint_reach(live);
        let (x, y) = ((x0 - reach).floor() as i32, (y0 - reach).floor() as i32);
        let region = IRect { x, y, w: (x1 + reach).ceil() as i32 - x, h: (y1 + reach).ceil() as i32 - y };
        for index in self.cells_in(region) {
            let tile = self.ensure(index, source);
            if let Some(area) = overlap(tile.rect, region) {
                Self::paint_display(tile, area, live, source, painter);
            }
            self.live.insert(index);
            self.changed.insert(index);
        }
    }

    /// The live shape changed entirely: every tile it touches is redrawn, the ones it left go back to base.
    pub fn live_changed(&mut self, live: &Annotation, source: RgbImage<'_>, painter: &mut Painter) {
        let mut now = BTreeSet::new();
        if let Some(bounds) = painter.bounds(live) {
            for index in self.cells_in(bounds) {
                let rect = self.rect_of(index);
                if painter.touches(live, rect) {
                    now.insert(index);
                }
            }
        }
        for &index in &now {
            let tile = self.ensure(index, source);
            let rect = tile.rect;
            Self::paint_display(tile, rect, live, source, painter);
            self.changed.insert(index);
        }
        let old = std::mem::take(&mut self.live);
        for index in old.difference(&now) {
            if let Some(tile) = &mut self.tiles[*index] {
                tile.display.copy_from_slice(&tile.base);
                self.changed.insert(*index);
            }
        }
        self.live = now;
    }

    /// The live shape became a committed annotation: what is on display is now the base.
    pub fn live_committed(&mut self) {
        for index in std::mem::take(&mut self.live) {
            if let Some(tile) = &mut self.tiles[index] {
                tile.base.copy_from_slice(&tile.display);
            }
        }
    }

    /// The live shape went away.
    pub fn live_dropped(&mut self) {
        for index in std::mem::take(&mut self.live) {
            if let Some(tile) = &mut self.tiles[index] {
                tile.display.copy_from_slice(&tile.base);
                self.changed.insert(index);
            }
        }
    }

    /// An annotation joins on top of the others.
    pub fn added(&mut self, annotation: &Annotation, source: RgbImage<'_>, painter: &mut Painter) {
        let Some(bounds) = painter.bounds(annotation) else { return };
        for index in self.cells_in(bounds) {
            let rect = self.rect_of(index);
            if !painter.touches(annotation, rect) {
                continue;
            }
            let tile = self.ensure(index, source);
            let stride = tile.rect.w as usize * 3;
            let mut canvas = Canvas { data: &mut tile.base, stride, area: rect };
            painter.draw(&mut canvas, annotation, source);
            tile.display.copy_from_slice(&tile.base);
            self.changed.insert(index);
        }
    }

    /// Redraws everything that exists inside `area` from the frame and this list.
    pub fn rebuild(&mut self, area: IRect, annotations: &[Annotation], source: RgbImage<'_>, painter: &mut Painter) {
        for index in self.cells_in(area) {
            let Some(tile) = &mut self.tiles[index] else { continue };
            copy_area(source, tile.rect, &mut tile.base);
            let stride = tile.rect.w as usize * 3;
            let rect = tile.rect;
            let mut canvas = Canvas { data: &mut tile.base, stride, area: rect };
            for annotation in annotations {
                painter.draw(&mut canvas, annotation, source);
            }
            tile.display.copy_from_slice(&tile.base);
            self.changed.insert(index);
        }
    }

    /// The whole list was replaced.
    pub fn replaced(&mut self, annotations: &[Annotation], source: RgbImage<'_>, painter: &mut Painter) {
        for annotation in annotations {
            let Some(bounds) = painter.bounds(annotation) else { continue };
            for index in self.cells_in(bounds) {
                self.ensure(index, source);
            }
        }
        let whole = IRect::new(0, 0, self.width, self.height);
        self.rebuild(whole, annotations, source, painter);
        self.live.clear();
    }

    /// True while a live shape is on display.
    pub fn has_live(&self) -> bool {
        !self.live.is_empty()
    }
}

fn overlap(a: IRect, b: IRect) -> Option<IRect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.w).min(b.x + b.w);
    let bottom = (a.y + a.h).min(b.y + b.h);
    (right > x && bottom > y).then_some(IRect { x, y, w: right - x, h: bottom - y })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::overlay::raster::{bake, TextCache};
    use crate::overlay::state::{Rgb, Shape};

    const W: i32 = 700;
    const H: i32 = 500;

    fn frame() -> Vec<u8> {
        let mut data = Vec::with_capacity((W * H * 3) as usize);
        for y in 0..H {
            for x in 0..W {
                data.extend_from_slice(&[(x / 3 % 256) as u8, (y / 2 % 256) as u8, 77]);
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

    /// The picture the grid shows, with the frame where no tile exists.
    fn shown(grid: &TileGrid, data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        for tile in grid.tiles.iter().flatten() {
            for row in 0..tile.rect.h {
                let stride = tile.rect.w as usize * 3;
                let src = &tile.display[row as usize * stride..(row as usize + 1) * stride];
                let at = ((tile.rect.y + row) * W + tile.rect.x) as usize * 3;
                out[at..at + stride].copy_from_slice(src);
            }
        }
        out
    }

    fn assert_close(got: &[u8], want: &[u8], what: &str) {
        let mut off = 0;
        for (i, (a, b)) in got.iter().zip(want).enumerate() {
            let difference = (i32::from(*a) - i32::from(*b)).abs();
            assert!(difference <= 3, "{what}: byte {i}: {a} vs {b}");
            if difference > 0 {
                off += 1;
            }
        }
        assert!(off * 20 < got.len(), "{what}: {off} of {} bytes differ", got.len());
    }

    fn zigzag(n: usize) -> Vec<Pt> {
        (0..n).map(|i| Pt { x: 40.0 + (i as f64 * 2.3) % 620.0, y: 250.0 + (i as f64 / 7.0).sin() * 200.0 }).collect()
    }

    #[test]
    fn a_stroke_grown_point_by_point_equals_the_finished_stroke() {
        let data = frame();
        let source = image(&data);
        let mut p = painter();
        let mut grid = TileGrid::new(W, H);
        let points = zigzag(300);
        for tool_points in 2..=points.len() {
            let live = Annotation::Pen { points: points[..tool_points].to_vec(), color: Rgb(255, 0, 0), stroke: 4.0 };
            grid.live_extended(&live, tool_points - 1, source, &mut p);
        }
        let finished = Annotation::Pen { points, color: Rgb(255, 0, 0), stroke: 4.0 };
        let whole = bake(source, IRect::new(0, 0, W, H), std::slice::from_ref(&finished), &mut p);
        assert_close(&shown(&grid, &data), &whole, "incremental pen");
    }

    #[test]
    fn a_highlighter_grown_point_by_point_equals_the_finished_one() {
        let data = frame();
        let source = image(&data);
        let mut p = painter();
        let mut grid = TileGrid::new(W, H);
        let points = zigzag(200);
        for tool_points in 2..=points.len() {
            let live = Annotation::Highlight { points: points[..tool_points].to_vec(), color: Rgb(255, 224, 58), stroke: 4.0 };
            grid.live_extended(&live, tool_points - 1, source, &mut p);
        }
        let finished = Annotation::Highlight { points, color: Rgb(255, 224, 58), stroke: 4.0 };
        let whole = bake(source, IRect::new(0, 0, W, H), std::slice::from_ref(&finished), &mut p);
        assert_close(&shown(&grid, &data), &whole, "incremental highlighter");
    }

    #[test]
    fn a_shape_dragged_around_leaves_no_trail() {
        let data = frame();
        let source = image(&data);
        let mut p = painter();
        let mut grid = TileGrid::new(W, H);
        for (x, y) in [(500.0, 400.0), (100.0, 450.0), (650.0, 60.0), (300.0, 300.0)] {
            let live = Annotation::Arrow(Shape { x1: 50.0, y1: 50.0, x2: x, y2: y, color: Rgb(0, 200, 255), stroke: 4.0 });
            grid.live_changed(&live, source, &mut p);
        }
        let last = Annotation::Arrow(Shape { x1: 50.0, y1: 50.0, x2: 300.0, y2: 300.0, color: Rgb(0, 200, 255), stroke: 4.0 });
        let whole = bake(source, IRect::new(0, 0, W, H), std::slice::from_ref(&last), &mut p);
        assert_close(&shown(&grid, &data), &whole, "after dragging");
        grid.live_dropped();
        assert_eq!(shown(&grid, &data), data, "dropped: back to the frame");
    }

    #[test]
    fn committing_keeps_the_picture_and_later_shapes_draw_over_it() {
        let data = frame();
        let source = image(&data);
        let mut p = painter();
        let mut grid = TileGrid::new(W, H);
        let first = Annotation::Rect(Shape { x1: 100.0, y1: 100.0, x2: 400.0, y2: 300.0, color: Rgb(255, 0, 0), stroke: 4.0 });
        grid.live_changed(&first, source, &mut p);
        grid.live_committed();
        let second = Annotation::Line(Shape { x1: 50.0, y1: 200.0, x2: 500.0, y2: 200.0, color: Rgb(0, 255, 0), stroke: 7.0 });
        grid.live_changed(&second, source, &mut p);
        let whole = bake(source, IRect::new(0, 0, W, H), &[first.clone(), second], &mut p);
        assert_close(&shown(&grid, &data), &whole, "two shapes");
        grid.live_dropped();
        let only_first = bake(source, IRect::new(0, 0, W, H), std::slice::from_ref(&first), &mut p);
        assert_close(&shown(&grid, &data), &only_first, "second dropped");
    }

    #[test]
    fn undo_and_redo_rebuild_from_the_list() {
        let data = frame();
        let source = image(&data);
        let mut p = painter();
        let mut grid = TileGrid::new(W, H);
        let a = Annotation::Rect(Shape { x1: 100.0, y1: 100.0, x2: 400.0, y2: 300.0, color: Rgb(255, 0, 0), stroke: 4.0 });
        let b = Annotation::Ellipse(Shape { x1: 200.0, y1: 150.0, x2: 600.0, y2: 450.0, color: Rgb(0, 0, 255), stroke: 7.0 });
        grid.added(&a, source, &mut p);
        grid.added(&b, source, &mut p);
        let both = bake(source, IRect::new(0, 0, W, H), &[a.clone(), b.clone()], &mut p);
        assert_close(&shown(&grid, &data), &both, "both");

        let bounds = p.bounds(&b).unwrap();
        grid.rebuild(bounds, std::slice::from_ref(&a), source, &mut p);
        let one = bake(source, IRect::new(0, 0, W, H), std::slice::from_ref(&a), &mut p);
        assert_close(&shown(&grid, &data), &one, "undone");

        grid.added(&b, source, &mut p);
        assert_close(&shown(&grid, &data), &both, "redone");
    }

    #[test]
    fn replacing_the_list_shows_exactly_it() {
        let data = frame();
        let source = image(&data);
        let mut p = painter();
        let mut grid = TileGrid::new(W, H);
        let old = Annotation::Line(Shape { x1: 0.0, y1: 0.0, x2: 700.0, y2: 500.0, color: Rgb(255, 255, 0), stroke: 7.0 });
        grid.added(&old, source, &mut p);
        let new = [Annotation::Counter { x: 300.0, y: 200.0, n: 1, color: Rgb(255, 159, 46) }];
        grid.replaced(&new, source, &mut p);
        let want = bake(source, IRect::new(0, 0, W, H), &new, &mut p);
        assert_close(&shown(&grid, &data), &want, "replaced");
        grid.replaced(&[], source, &mut p);
        assert_eq!(shown(&grid, &data), data, "empty list: the frame");
    }

    #[test]
    fn a_pointer_move_touches_only_tiles_near_the_stroke_end() {
        let data = frame();
        let source = image(&data);
        let mut p = painter();
        let mut grid = TileGrid::new(W, H);
        let points = zigzag(1000);
        let mut live = Annotation::Pen { points: points[..999].to_vec(), color: Rgb(255, 0, 0), stroke: 4.0 };
        grid.live_extended(&live, 1, source, &mut p);
        grid.take_changed();
        if let Annotation::Pen { points: pts, .. } = &mut live {
            pts.push(points[999]);
        }
        grid.live_extended(&live, 999, source, &mut p);
        let changed = grid.take_changed().len();
        assert!(changed <= 4, "{changed} tiles for one segment");
    }

    #[test]
    fn take_changed_reports_each_tile_once() {
        let data = frame();
        let source = image(&data);
        let mut p = painter();
        let mut grid = TileGrid::new(W, H);
        assert!(grid.take_changed().is_empty());
        let a = Annotation::Counter { x: 300.0, y: 200.0, n: 1, color: Rgb(255, 159, 46) };
        grid.added(&a, source, &mut p);
        assert!(!grid.take_changed().is_empty());
        assert!(grid.take_changed().is_empty());
    }

    #[test]
    fn tiles_fit_the_edges_of_the_frame() {
        let grid = TileGrid::new(300, 130);
        assert_eq!(grid.rect_of(0), IRect::new(0, 0, 128, 128));
        assert_eq!(grid.rect_of(2), IRect::new(256, 0, 44, 128));
        assert_eq!(grid.rect_of(5), IRect::new(256, 128, 44, 2));
        assert_eq!(grid.cells_in(IRect::new(-50, -50, 60, 60)), [0]);
        assert_eq!(grid.cells_in(IRect::new(127, 127, 2, 2)), [0, 1, 3, 4]);
        assert!(grid.cells_in(IRect::new(400, 0, 10, 10)).is_empty());
    }
}
