//! Pure function: physical monitor rects to the DIP layout Chromium computes. See CONTRACT.md.
//!
//! A port of `DisplayInfosToScreenWinDisplays` (`ui/display/win/screen_win.cc`),
//! `CalculateDisplayPlacement` (`scaling_util.cc`) and `ApplyDisplayPlacement` (`display_layout.cc`)
//! at Chromium 130, the version Electron 33 ships. Float arithmetic stays `f32` on purpose: Chromium
//! computes in `float`, and `f64` lands on the other side of a floor or ceil. `3840 * (1.0 / 1.25)`
//! is exactly 3072 in `f32` and 3072.00005 in `f64`, whose ceil is one pixel too wide.

use shotik_core::geom::{Point, PointF, Rect};

/// What the system reports about one monitor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorInfo {
    /// Global physical pixels.
    pub physical: Rect,
    /// Physical pixels minus the taskbar.
    pub work: Rect,
    /// DPI / 96, times the accessibility text scale: Chromium's device scale factor.
    pub scale: f32,
}

/// One monitor in global DIP. `bounds` and `work_area` use the same index as the input slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DipRects {
    pub bounds: Rect,
    pub work_area: Rect,
}

/// Lays out the monitors in DIP, in input order.
///
/// The primary, the monitor at physical (0,0), keeps its origin. Everything reachable from it through
/// touching edges or corners attaches to its parent: they abut in DIP, and the offset along the
/// shared edge scales by the parent's factor (see [`placement`]). A monitor no chain reaches stays at
/// its scaled physical position, because Chromium "scales relative to (0, 0)" there.
///
/// Returns nothing when no monitor sits at the origin. Chromium treats that as a race with a display
/// change and also answers with an empty list, so the caller should query again.
pub fn layout(monitors: &[MonitorInfo]) -> Vec<DipRects> {
    let Some(primary) = monitors.iter().position(|m| m.physical.x == 0 && m.physical.y == 0) else {
        return Vec::new();
    };

    let mut rects: Vec<DipRects> = monitors
        .iter()
        .map(|m| DipRects {
            bounds: scale_to_enclosing_rect(m.physical, 1.0 / m.scale),
            work_area: scale_to_enclosing_rect(m.work, 1.0 / m.scale),
        })
        .collect();

    // Build the tree depth-first with an explicit stack, exactly as Chromium does: which parent wins
    // when a monitor touches several depends on this order.
    let mut parent_of: Vec<Option<usize>> = vec![None; monitors.len()];
    let mut remaining: Vec<usize> = (0..monitors.len()).filter(|&i| i != primary).collect();
    let mut parents = vec![primary];
    while let Some(parent) = parents.pop() {
        for child in take_touching(&monitors[parent], monitors, &mut remaining) {
            parent_of[child] = Some(parent);
            // A parent was popped before its children were found, so its position is final here.
            let parent_bounds = rects[parent].bounds;
            apply_placement(placement(&monitors[parent], &monitors[child]), parent_bounds, &mut rects[child]);
            parents.push(child);
        }
    }

    // `ApplyToDisplayList` returns early with an empty placement list, so monitors nothing reaches may
    // overlap the primary: they are only pushed apart once at least one monitor was placed.
    if parent_of.iter().any(Option::is_some) {
        deintersect(primary, &parent_of, &mut rects);
    }
    rects
}

/// `ScreenWin::ScreenToDIPPoint`: relative to the display, scaled by `1.0 / scale`, then its DIP origin.
pub fn phys_to_dip(point: PointF, physical: Rect, scale: f32, dip_bounds: Rect) -> PointF {
    let factor = 1.0 / scale;
    let x = (point.x as f32 - physical.x as f32) * factor + dip_bounds.x as f32;
    let y = (point.y as f32 - physical.y as f32) * factor + dip_bounds.y as f32;
    PointF { x: f64::from(x), y: f64::from(y) }
}

/// `display::win::DIPToScreenPoint`: a floored physical point.
pub fn dip_to_phys(point: Point, physical: Rect, scale: f32, dip_bounds: Rect) -> Point {
    let x = (point.x as f32 - dip_bounds.x as f32) * scale + physical.x as f32;
    let y = (point.y as f32 - dip_bounds.y as f32) * scale + physical.y as f32;
    Point { x: x.floor() as i32, y: y.floor() as i32 }
}

/// `ScreenWin::ScreenToDIPRect` for a known display: floored origin, enclosing size.
pub fn phys_rect_to_dip(rect: Rect, physical: Rect, scale: f32, dip_bounds: Rect) -> Rect {
    let origin = PointF { x: f64::from(rect.x), y: f64::from(rect.y) };
    let origin = phys_to_dip(origin, physical, scale, dip_bounds);
    let size = scale_to_enclosing_rect(rect, 1.0 / scale);
    Rect::new(origin.x.floor() as i32, origin.y.floor() as i32, size.width, size.height)
}

/// Index of the monitor nearest a physical point: what `MonitorFromPoint(MONITOR_DEFAULTTONEAREST)`
/// answers. A point on a monitor is at distance zero, the first in input order wins a tie.
pub fn nearest_physical(physicals: impl IntoIterator<Item = Rect>, point: Point) -> Option<usize> {
    let mut best: Option<(usize, i64)> = None;
    for (i, r) in physicals.into_iter().enumerate() {
        let d = squared_distance_to_rect(r, point);
        if best.is_none_or(|(_, b)| d < b) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Index of the monitor for a physical rectangle: what `MonitorFromRect(MONITOR_DEFAULTTONEAREST)`
/// answers. The largest intersection, else the nearest one.
pub fn nearest_physical_rect(physicals: impl IntoIterator<Item = Rect>, rect: Rect) -> Option<usize> {
    let mut best_overlap: Option<(usize, i64)> = None;
    let mut best_distance: Option<(usize, i64)> = None;
    for (i, r) in physicals.into_iter().enumerate() {
        let w = (r.x + r.width).min(rect.x + rect.width) - r.x.max(rect.x);
        let h = (r.y + r.height).min(rect.y + rect.height) - r.y.max(rect.y);
        if w > 0 && h > 0 {
            let area = i64::from(w) * i64::from(h);
            if best_overlap.is_none_or(|(_, b)| area > b) {
                best_overlap = Some((i, area));
            }
        }
        let dx = i64::from((r.x - (rect.x + rect.width - 1)).max(rect.x - (r.x + r.width - 1)).max(0));
        let dy = i64::from((r.y - (rect.y + rect.height - 1)).max(rect.y - (r.y + r.height - 1)).max(0));
        let d = dx * dx + dy * dy;
        if best_distance.is_none_or(|(_, b)| d < b) {
            best_distance = Some((i, d));
        }
    }
    best_overlap.or(best_distance).map(|(i, _)| i)
}

/// `ScreenWin::GetScreenWinDisplayNearestDIPPoint`: the monitor whose DIP bounds contain the point,
/// else the primary. It is not a nearest search, despite the name: Chromium then maps through the
/// primary's scale and asks for the nearest monitor in physical space, see [`nearest_dip`].
pub fn dip_point_owner(bounds: impl IntoIterator<Item = Rect>, point: Point) -> Option<usize> {
    let mut primary = None;
    for (i, b) in bounds.into_iter().enumerate() {
        if contains_half_open(b, point) {
            return Some(i);
        }
        if b.x == 0 && b.y == 0 {
            primary = Some(i);
        }
    }
    primary
}

/// `ScreenWin::GetDisplayNearestPoint`: DIP to physical through the owner of the point, then the
/// monitor nearest that physical point.
pub fn nearest_dip(monitors: &[MonitorInfo], rects: &[DipRects], point: PointF) -> Option<usize> {
    let floored = Point { x: point.x.floor() as i32, y: point.y.floor() as i32 };
    let owner = dip_point_owner(rects.iter().map(|r| r.bounds), floored)?;
    let phys = dip_to_phys(floored, monitors[owner].physical, monitors[owner].scale, rects[owner].bounds);
    nearest_physical(monitors.iter().map(|m| m.physical), phys)
}

/// `gfx::Rect::Contains`: the right and bottom edges are outside.
fn contains_half_open(r: Rect, p: Point) -> bool {
    p.x >= r.x && p.x < r.x + r.width && p.y >= r.y && p.y < r.y + r.height
}

fn squared_distance_to_rect(r: Rect, p: Point) -> i64 {
    // The last pixel inside is `right - 1`: a point on the right edge belongs to the next monitor.
    let dx = i64::from(p.x) - i64::from(p.x.clamp(r.x, (r.x + r.width - 1).max(r.x)));
    let dy = i64::from(p.y) - i64::from(p.y.clamp(r.y, (r.y + r.height - 1).max(r.y)));
    dx * dx + dy * dy
}

/// `gfx::ScaleToEnclosingRect`. An empty side stays empty.
fn scale_to_enclosing_rect(r: Rect, scale: f32) -> Rect {
    if scale == 1.0 {
        return r;
    }
    let x = (r.x as f32 * scale).floor() as i32;
    let y = (r.y as f32 * scale).floor() as i32;
    let right = if r.width == 0 { x } else { ((r.x + r.width) as f32 * scale).ceil() as i32 };
    let bottom = if r.height == 0 { y } else { ((r.y + r.height) as f32 * scale).ceil() as i32 };
    Rect::new(x, y, right - x, bottom - y)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Placement {
    position: Position,
    offset: i32,
    /// Offset from the right or bottom edge rather than the top or left one.
    from_end: bool,
}

/// `DisplayInfosTouch`: sharing an edge or just a corner counts.
fn touch(a: Rect, b: Rect) -> bool {
    let max_left = a.x.max(b.x);
    let max_top = a.y.max(b.y);
    let min_right = (a.x + a.width).min(b.x + b.width);
    let min_bottom = (a.y + a.height).min(b.y + b.height);
    (max_left == min_right && a.y <= b.y + b.height && b.y <= a.y + a.height)
        || (max_top == min_bottom && a.x <= b.x + b.width && b.x <= a.x + a.width)
}

/// `FindAndRemoveTouchingDisplayInfos`, including the order libc++'s `std::partition` leaves behind:
/// it swaps from both ends and is not stable, and the order decides who becomes a parent first.
fn take_touching(parent: &MonitorInfo, monitors: &[MonitorInfo], remaining: &mut Vec<usize>) -> Vec<usize> {
    let keep = |i: usize| !touch(parent.physical, monitors[i].physical);
    let mut first = 0;
    let mut last = remaining.len();
    'outer: loop {
        loop {
            if first == last {
                break 'outer;
            }
            if !keep(remaining[first]) {
                break;
            }
            first += 1;
        }
        loop {
            last -= 1;
            if first == last {
                break 'outer;
            }
            if keep(remaining[last]) {
                break;
            }
        }
        remaining.swap(first, last);
        first += 1;
    }
    remaining.split_off(first)
}

/// `CalculateDisplayPosition`.
fn position(parent: Rect, current: Rect) -> Position {
    let max_left = parent.x.max(current.x);
    let max_top = parent.y.max(current.y);
    let min_right = (parent.x + parent.width).min(current.x + current.width);
    let min_bottom = (parent.y + parent.height).min(current.y + current.height);
    if max_left == min_right && max_top == min_bottom {
        // Corner touching.
        if parent.y + parent.height == max_top {
            return Position::Bottom;
        }
        if parent.x == max_left {
            return Position::Left;
        }
        return Position::Top;
    }
    if max_left == min_right && parent.y <= current.y + current.height && current.y <= parent.y + parent.height {
        return if parent.x == max_left { Position::Left } else { Position::Right };
    }
    // The only remaining way to touch, which `touch` has already established.
    if parent.y == max_top {
        Position::Top
    } else {
        Position::Bottom
    }
}

/// `ScaleOffset`: the same relative position on a length measured in another unit.
fn scale_offset(unscaled_length: i32, scale: f32, unscaled_offset: i32) -> i32 {
    let scaled_length = unscaled_length as f32 / scale;
    let percent = unscaled_offset as f32 / unscaled_length as f32;
    (scaled_length * percent).floor() as i32
}

/// `CalculateDisplayPlacement`.
fn placement(parent: &MonitorInfo, current: &MonitorInfo) -> Placement {
    let position = position(parent.physical, current.physical);
    let (parent_begin, parent_end, current_begin, current_end) = match position {
        Position::Top | Position::Bottom => (
            parent.physical.x,
            parent.physical.x + parent.physical.width,
            current.physical.x,
            current.physical.x + current.physical.width,
        ),
        Position::Left | Position::Right => (
            parent.physical.y,
            parent.physical.y + parent.physical.height,
            current.physical.y,
            current.physical.y + current.physical.height,
        ),
    };
    // Everything relative to the parent's start.
    let parent_len = parent_end - parent_begin;
    let begin = current_begin - parent_begin;
    let end = current_end - parent_begin;
    let in_parent = |v: i32| 0 <= v && v <= parent_len;

    if parent_len == end {
        Placement { position, offset: 0, from_end: true }
    } else if in_parent(begin) {
        Placement { position, offset: scale_offset(parent_len, parent.scale, begin), from_end: false }
    } else if in_parent(end) {
        Placement { position, offset: scale_offset(parent_len, parent.scale, parent_len - end), from_end: true }
    } else {
        // The child is longer than the parent on both sides, so the child's scale decides.
        Placement { position, offset: scale_offset(end - begin, current.scale, begin), from_end: false }
    }
}

/// `DisplayLayout::ApplyDisplayPlacement` with `minimum_offset_overlap` 0, as `ScreenWin` calls it.
fn apply_placement(p: Placement, parent: Rect, target: &mut DipRects) {
    let size = target.bounds;
    let mut offset = p.offset;
    let (x, y) = match p.position {
        Position::Top | Position::Bottom => {
            if p.from_end {
                offset = parent.width - offset - size.width;
            }
            offset = offset.min(parent.width).max(-size.width);
            let y = if p.position == Position::Top { -size.height } else { parent.height };
            (offset, y)
        }
        Position::Left | Position::Right => {
            if p.from_end {
                offset = parent.height - offset - size.height;
            }
            offset = offset.min(parent.height).max(-size.height);
            let x = if p.position == Position::Left { -size.width } else { parent.width };
            (x, offset)
        }
    };
    move_keeping_work_area_insets(target, parent.x + x, parent.y + y);
}

/// `Display::UpdateWorkAreaFromInsets`: the work area moves with the display and keeps its insets.
fn move_keeping_work_area_insets(target: &mut DipRects, x: i32, y: i32) {
    let dx = x - target.bounds.x;
    let dy = y - target.bounds.y;
    target.bounds.x = x;
    target.bounds.y = y;
    target.work_area.x += dx;
    target.work_area.y += dy;
}

/// `GetDisplayTreeDepth`: detached monitors go last.
fn tree_depth(mut i: usize, primary: usize, parent_of: &[Option<usize>]) -> usize {
    const MAX_DEPTH: usize = 100;
    let mut depth = 0;
    while i != primary && depth < MAX_DEPTH {
        depth += 1;
        match parent_of[i] {
            Some(parent) => i = parent,
            None => return MAX_DEPTH,
        }
    }
    depth
}

/// `DeIntersectDisplays`: a scaled parent can be too small for its children, so monitors may overlap
/// in DIP after placement. They are pushed apart from the primary outward, along the shorter axis.
///
/// Chromium breaks a tie between equally deep and equally distant monitors by display id, a hash of
/// the adapter LUID that changes on reboot. Input order stands in for it. Its `DisplayLayout::Validate`
/// also rejects a layout when two ids share their low 8 bits, a Chrome OS assumption that strikes at
/// random on Windows. That is not ported either: it cannot be reproduced and nobody wants it.
fn deintersect(primary: usize, parent_of: &[Option<usize>], rects: &mut [DipRects]) {
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by_key(|&i| {
        let b = rects[i].bounds;
        let distance = i64::from(b.x) * i64::from(b.x) + i64::from(b.y) * i64::from(b.y);
        (tree_depth(i, primary, parent_of), distance, i)
    });
    for n in 1..order.len() {
        let target = order[n];
        for &source in &order[..n] {
            let s = rects[source].bounds;
            let t = rects[target].bounds;
            let overlaps = s.x < t.x + t.width && t.x < s.x + s.width && s.y < t.y + t.height && t.y < s.y + s.height;
            if !overlaps {
                continue;
            }
            // `CalculateOffsetX` and `CalculateOffsetY`: away from the origin along each axis.
            let dx = if t.x >= 0 { s.x + s.width - t.x } else { -(t.x + t.width - s.x) };
            let dy = if t.y >= 0 { s.y + s.height - t.y } else { -(t.y + t.height - s.y) };
            if dx == 0 && dy == 0 {
                continue;
            }
            let (dx, dy) = if dx.abs() <= dy.abs() { (dx, 0) } else { (0, dy) };
            move_keeping_work_area_insets(&mut rects[target], t.x + dx, t.y + dy);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monitor with no taskbar.
    fn mon(x: i32, y: i32, w: i32, h: i32, scale: f32) -> MonitorInfo {
        MonitorInfo { physical: Rect::new(x, y, w, h), work: Rect::new(x, y, w, h), scale }
    }

    fn bounds(monitors: &[MonitorInfo]) -> Vec<Rect> {
        layout(monitors).iter().map(|r| r.bounds).collect()
    }

    /// CONTRACT.md, section DIP layout: the real vector from this machine.
    fn real_machine() -> Vec<MonitorInfo> {
        vec![mon(0, 0, 3840, 2160, 1.5), mon(3840, 694, 1920, 1080, 1.0), mon(-2560, 419, 2560, 1440, 1.0)]
    }

    #[test]
    fn real_vector_from_the_contract() {
        assert_eq!(
            bounds(&real_machine()),
            vec![Rect::new(0, 0, 2560, 1440), Rect::new(2560, 462, 1920, 1080), Rect::new(-2560, 279, 2560, 1440)]
        );
    }

    #[test]
    fn input_order_does_not_change_the_result() {
        let m = real_machine();
        let reordered = [m[2], m[0], m[1]];
        assert_eq!(
            bounds(&reordered),
            vec![Rect::new(-2560, 279, 2560, 1440), Rect::new(0, 0, 2560, 1440), Rect::new(2560, 462, 1920, 1080)]
        );
    }

    #[test]
    fn a_single_monitor_scales_in_float_like_chromium() {
        // In f64 the width would be 3072.00005 and its ceil 3073.
        assert_eq!(bounds(&[mon(0, 0, 3840, 2160, 1.25)]), vec![Rect::new(0, 0, 3072, 1728)]);
        // 3840 / 1.75 = 2194.3 and 2160 / 1.75 = 1234.3: the size is enclosing, so it rounds up.
        assert_eq!(bounds(&[mon(0, 0, 3840, 2160, 1.75)]), vec![Rect::new(0, 0, 2195, 1235)]);
    }

    #[test]
    fn chain_of_monitors_attaches_to_the_parent_not_to_the_primary() {
        let m = [
            mon(0, 0, 3840, 2160, 2.0),      // 1920x1080
            mon(3840, 200, 1920, 1080, 1.0), // right of the primary, offset 200 / 2
            mon(5760, 300, 2560, 1440, 1.0), // right of that one, offset 100 / 1
            mon(8320, 100, 2880, 1500, 1.5), // right again: begins above its parent, ends inside it
        ];
        let got = bounds(&m);
        assert_eq!(got[0], Rect::new(0, 0, 1920, 1080));
        assert_eq!(got[1], Rect::new(1920, 100, 1920, 1080));
        assert_eq!(got[2], Rect::new(3840, 200, 2560, 1440));
        // The size encloses the rectangle at its physical position: 8320..11200 is 5546..7467 in DIP,
        // 1921 wide rather than 1920, and 100..1600 is 66..1067. The end at 1600 is inside the parent
        // (300..1740), so the offset runs from the parent's bottom: 140 / 1, so y = 200 + 1440 - 140 - 1001.
        assert_eq!(got[3], Rect::new(6400, 499, 1921, 1001));
    }

    #[test]
    fn a_child_sticking_out_on_both_sides_uses_its_own_scale_in_a_chain() {
        let m = [
            mon(0, 0, 1920, 1080, 1.0),
            mon(1920, 300, 1920, 1080, 1.0), // right of the primary
            mon(3840, 100, 2880, 1800, 1.5), // sticks out above and below its parent
        ];
        // floor(1200 * -200 / 1800) = -134, from the parent's 300.
        assert_eq!(bounds(&m)[2], Rect::new(3840, 166, 1920, 1201));
    }

    #[test]
    fn negative_origins_on_all_sides() {
        let m = [
            mon(0, 0, 2560, 1440, 1.25),       // 2048x1152
            mon(-1920, 300, 1920, 1080, 1.0),  // left, offset 300 / 1.25
            mon(400, -1080, 1920, 1080, 1.5),  // top, offset 400 / 1.25
            mon(-3840, 1000, 1920, 1080, 2.0), // left of the left one, offset 700 / 1
        ];
        let got = bounds(&m);
        assert_eq!(got[1], Rect::new(-1920, 240, 1920, 1080));
        // 400..2320 is 266..1547 in DIP, so 1281 wide.
        assert_eq!(got[2], Rect::new(320, -720, 1281, 720));
        // 1080 * (700 / 1080) is 699.99997 in f32, and the floor makes it 699.
        assert_eq!(got[3], Rect::new(-1920 - 960, 240 + 699, 960, 540));
    }

    #[test]
    fn a_monitor_nothing_reaches_stays_at_its_scaled_position() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(5000, 3000, 2560, 1440, 2.0)];
        // Chromium: "When this happens, we scale relative to (0, 0)".
        assert_eq!(bounds(&m), vec![Rect::new(0, 0, 1920, 1080), Rect::new(2500, 1500, 1280, 720)]);
    }

    #[test]
    fn a_gap_cuts_off_everything_behind_it_and_nothing_is_pushed_apart() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(2000, 0, 1920, 1080, 2.0), mon(3920, 0, 1920, 1080, 1.5)];
        // The second and third touch each other but neither touches the primary, so no chain reaches them.
        // With no placement at all, Chromium returns before pushing overlaps apart: the second overlaps
        // the primary in DIP and stays there.
        assert_eq!(
            bounds(&m),
            vec![Rect::new(0, 0, 1920, 1080), Rect::new(1000, 0, 960, 540), Rect::new(2613, 0, 1281, 720)]
        );
    }

    #[test]
    fn a_detached_monitor_is_pushed_out_once_anything_was_placed() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(1920, 0, 1920, 1080, 1.0), mon(4000, 0, 1920, 1080, 2.0)];
        // The detached one scales to (2000, 0), inside the second: 1840 to clear it along x, 1080 along y.
        assert_eq!(
            bounds(&m),
            vec![Rect::new(0, 0, 1920, 1080), Rect::new(1920, 0, 1920, 1080), Rect::new(2000, 1080, 960, 540)]
        );
    }

    #[test]
    fn an_end_aligned_child_is_placed_from_the_far_edge() {
        // The ends meet at 1080: the offset is 0 from the bottom, and 864 - 1080 puts it above the origin.
        let m = [mon(0, 0, 1920, 1080, 1.25), mon(1920, 0, 1920, 1080, 1.0)];
        assert_eq!(bounds(&m)[1], Rect::new(1536, -216, 1920, 1080));
    }

    #[test]
    fn a_child_ending_inside_the_parent_is_placed_from_the_far_edge() {
        // Begins above, ends at 500: 580 physical px from the bottom. 864 * (580 / 1080) is 464 on paper,
        // but 463.99997 in f32, and the floor makes it 463. Chromium lands on the same pixel.
        let m = [mon(0, 0, 1920, 1080, 1.25), mon(1920, -300, 1600, 800, 1.0)];
        assert_eq!(bounds(&m)[1], Rect::new(1536, 864 - 463 - 800, 1600, 800));
    }

    #[test]
    fn a_child_longer_than_the_parent_uses_its_own_scale() {
        let m = [mon(0, 0, 1280, 720, 1.0), mon(1280, -200, 2160, 3840, 2.0)];
        // Portrait, spans the parent on both sides: -200 physical px at the child's scale of 2.
        assert_eq!(bounds(&m)[1], Rect::new(1280, -100, 1080, 1920));
    }

    #[test]
    fn corners_count_as_touching() {
        let m = [mon(0, 0, 1920, 1080, 1.0), mon(1920, 1080, 1920, 1080, 2.0)];
        // Parent's bottom meets the child's top at a corner: it attaches below, offset 1920.
        assert_eq!(bounds(&m)[1], Rect::new(1920, 1080, 960, 540));
    }

    #[test]
    fn monitors_overlapping_after_scaling_are_pushed_apart() {
        // Two children stacked on the right of a 2x primary: their tops map to 0 and 540, but each is
        // 1080 tall. The nearer one stays and the other moves down along the shorter axis.
        let m = [mon(0, 0, 3840, 2160, 2.0), mon(3840, 0, 1920, 1080, 1.0), mon(3840, 1080, 1920, 1080, 1.0)];
        let got = bounds(&m);
        assert_eq!(got[1], Rect::new(1920, 0, 1920, 1080));
        assert_eq!(got[2], Rect::new(1920, 1080, 1920, 1080));
    }

    #[test]
    fn the_work_area_moves_with_the_monitor_and_keeps_its_insets() {
        let m = [
            MonitorInfo { physical: Rect::new(0, 0, 3840, 2160), work: Rect::new(0, 0, 3840, 2100), scale: 1.5 },
            MonitorInfo {
                physical: Rect::new(3840, 694, 1920, 1080),
                work: Rect::new(3840, 694, 1920, 1040),
                scale: 1.0,
            },
        ];
        let got = layout(&m);
        assert_eq!(got[0].work_area, Rect::new(0, 0, 2560, 1400));
        assert_eq!(got[1].bounds, Rect::new(2560, 462, 1920, 1080));
        assert_eq!(got[1].work_area, Rect::new(2560, 462, 1920, 1040));
    }

    #[test]
    fn no_monitor_at_the_origin_means_no_layout() {
        assert!(layout(&[mon(10, 10, 100, 100, 1.0)]).is_empty());
        assert!(layout(&[]).is_empty());
    }

    #[test]
    fn phys_to_dip_is_relative_to_the_display() {
        let (phys, dip) = (Rect::new(3840, 694, 1920, 1080), Rect::new(2560, 462, 1920, 1080));
        assert_eq!(phys_to_dip(PointF { x: 3841.0, y: 695.0 }, phys, 1.0, dip), PointF { x: 2561.0, y: 463.0 });
        let (phys, dip) = (Rect::new(0, 0, 3840, 2160), Rect::new(0, 0, 2560, 1440));
        let p = phys_to_dip(PointF { x: 3.0, y: 3.0 }, phys, 1.5, dip);
        // Computed in f32, as Chromium does: 3 * (1 / 1.5f32).
        assert_eq!(p.x, f64::from(3.0_f32 * (1.0_f32 / 1.5_f32)));
        assert!((p.x - 2.0).abs() < 1e-5);
    }

    /// `screen.screenToDipPoint` in Electron 33 on this machine, as printed by JavaScript.
    #[test]
    fn phys_to_dip_matches_electron_to_the_last_bit() {
        let m = real_machine();
        let rects = layout(&m);
        let cases: [((i32, i32), (f64, f64)); 12] = [
            ((3840, 694), (2560.0, 462.0)),
            ((4800, 1234), (3520.0, 1002.0)),
            ((-1280, 1139), (-1280.0, 999.0)),
            ((-3, 1858), (-3.0, 1718.0)),
            ((1, 1), (0.6666666865348816, 0.6666666865348816)),
            ((7, 13), (4.6666669845581055, 8.666666984558105)),
            ((3836, 2158), (2557.33349609375, 1438.666748046875)),
            ((3839, 2159), (2559.33349609375, 1439.3333740234375)),
            ((3840, 2159), (2560.0, 1439.3333740234375)),
            ((0, -100), (0.0, -66.66667175292969)),
            ((-5000, -5000), (-5000.0, -5140.0)),
            ((22000, 300), (20720.0, 68.0)),
        ];
        for ((x, y), expected) in cases {
            let i = nearest_physical(m.iter().map(|d| d.physical), Point { x, y }).unwrap();
            let p = PointF { x: f64::from(x), y: f64::from(y) };
            let got = phys_to_dip(p, m[i].physical, m[i].scale, rects[i].bounds);
            assert_eq!((got.x, got.y), expected, "at {x},{y}");
        }
    }

    /// `screen.screenToDipRect(null, rect)` in Electron 33 on this machine.
    #[test]
    fn phys_rect_to_dip_matches_electron() {
        let m = real_machine();
        let rects = layout(&m);
        let cases = [
            (Rect::new(22000, 300, 400, 300), Rect::new(20720, 68, 400, 300)),
            (Rect::new(100, 100, 333, 211), Rect::new(66, 66, 223, 142)),
            // Mostly on the right monitor: its scale and origin apply, though it starts on the primary.
            (Rect::new(3700, 700, 400, 300), Rect::new(2420, 468, 400, 300)),
            (Rect::new(-1000, 500, 501, 301), Rect::new(-1000, 360, 501, 301)),
        ];
        for (phys, expected) in cases {
            let i = nearest_physical_rect(m.iter().map(|d| d.physical), phys).unwrap();
            assert_eq!(phys_rect_to_dip(phys, m[i].physical, m[i].scale, rects[i].bounds), expected);
        }
    }

    /// `screen.getDisplayNearestPoint` in Electron 33 on this machine.
    #[test]
    fn nearest_dip_matches_electron() {
        let m = real_machine();
        let rects = layout(&m);
        let cases = [
            ((2560.0, 462.0), 1),
            ((4479.0, 1541.0), 1),
            ((4480.0, 1542.0), 1),
            ((-2560.0, 279.0), 2),
            ((-1.0, 1718.0), 2),
            ((0.0, 1719.0), 0),
            ((0.0, 0.0), 0),
            ((2559.0, 1439.0), 0),
            ((2560.0, 1440.0), 1),
            ((-100_000.0, 0.0), 2),
            ((100_000.0, 100_000.0), 1),
            ((0.0, -50.0), 0),
            ((22000.0, 300.0), 1),
        ];
        for ((x, y), expected) in cases {
            assert_eq!(nearest_dip(&m, &rects, PointF { x, y }), Some(expected), "at {x},{y}");
        }
    }

    #[test]
    fn dip_to_phys_floors() {
        let (phys, dip) = (Rect::new(0, 0, 3840, 2160), Rect::new(0, 0, 2560, 1440));
        assert_eq!(dip_to_phys(Point { x: 1, y: 2 }, phys, 1.5, dip), Point { x: 1, y: 3 });
    }

    #[test]
    fn phys_rect_to_dip_floors_the_origin_and_encloses_the_size() {
        let (phys, dip) = (Rect::new(0, 0, 3840, 2160), Rect::new(0, 0, 2560, 1440));
        assert_eq!(phys_rect_to_dip(Rect::new(100, 100, 333, 211), phys, 1.5, dip), Rect::new(66, 66, 223, 142));
    }

    #[test]
    fn nearest_physical_prefers_the_monitor_containing_the_point() {
        let rects: Vec<Rect> = real_machine().iter().map(|m| m.physical).collect();
        assert_eq!(nearest_physical(rects.iter().copied(), Point { x: 100, y: 100 }), Some(0));
        // Off the primary by one pixel, off the right monitor by 386: Electron answers the primary.
        assert_eq!(nearest_physical(rects.iter().copied(), Point { x: 3840, y: 2159 }), Some(0));
        assert_eq!(nearest_physical(rects.iter().copied(), Point { x: 3840, y: 700 }), Some(1));
        assert_eq!(nearest_physical(rects.iter().copied(), Point { x: -1, y: 500 }), Some(2));
        // Far outside on each side.
        assert_eq!(nearest_physical(rects.iter().copied(), Point { x: 22000, y: 300 }), Some(1));
        assert_eq!(nearest_physical(rects.iter().copied(), Point { x: -5000, y: -5000 }), Some(2));
        assert_eq!(nearest_physical(rects.iter().copied(), Point { x: 0, y: -100 }), Some(0));
        assert_eq!(nearest_physical(std::iter::empty(), Point { x: 0, y: 0 }), None);
    }

    #[test]
    fn dip_point_owner_falls_back_to_the_primary_rather_than_searching() {
        let m = real_machine();
        let rects = layout(&m);
        let owner = |x, y| dip_point_owner(rects.iter().map(|r| r.bounds), Point { x, y });
        assert_eq!(owner(0, 0), Some(0));
        assert_eq!(owner(2559, 1439), Some(0));
        // The right edge is outside, so this belongs to the next monitor.
        assert_eq!(owner(2560, 462), Some(1));
        assert_eq!(owner(-1, 500), Some(2));
        // Outside everything, even when closer to another monitor.
        assert_eq!(owner(100_000, 500), Some(0));
    }

    #[test]
    fn nearest_dip_goes_through_physical_space_like_chromium() {
        let m = real_machine();
        let rects = layout(&m);
        let nearest = |x: f64, y: f64| nearest_dip(&m, &rects, PointF { x, y });
        assert_eq!(nearest(100.0, 100.0), Some(0));
        assert_eq!(nearest(2600.0, 500.0), Some(1));
        assert_eq!(nearest(-100.0, 500.0), Some(2));
        // Outside everything: mapped through the primary's 1.5 to physical, then the nearest there.
        assert_eq!(nearest(100_000.0, 100_000.0), Some(1));
        assert_eq!(nearest(-100_000.0, 0.0), Some(2));
    }
}
