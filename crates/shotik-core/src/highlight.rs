//! Connected selection-highlight detection over BGRA8 (`src/main/highlight.js`).

use crate::geom::Rect;

#[derive(Clone, Copy, Debug)]
pub struct HighlightOptions {
    pub tol: f64,
    pub min_frac: f64,
    pub max_frac: f64,
    pub min_color: u8,
}

impl Default for HighlightOptions {
    fn default() -> Self {
        Self {
            tol: 40.0,
            min_frac: 0.004,
            max_frac: 0.80,
            min_color: 22,
        }
    }
}

#[derive(Default)]
struct Count {
    n: u32,
    r: u32,
    g: u32,
    b: u32,
}

pub fn find_highlight_rect(
    bgra: &[u8],
    width: u32,
    height: u32,
    sx: i32,
    sy: i32,
    opts: HighlightOptions,
) -> Option<Rect> {
    let total = (width as usize).checked_mul(height as usize)?;
    if total == 0
        || width > i32::MAX as u32
        || height > i32::MAX as u32
        || bgra.len() < total.checked_mul(4)?
    {
        return None;
    }
    let (width, height, sx, sy) = (
        i64::from(width),
        i64::from(height),
        i64::from(sx),
        i64::from(sy),
    );
    let inside = |x, y| x >= 0 && y >= 0 && x < width && y < height;
    let pixel = |x, y| {
        let i = ((y * width + x) * 4) as usize;
        [bgra[i + 2], bgra[i + 1], bgra[i]]
    };
    // Preserve insertion order: JS Map breaks frequency ties by first colour encountered.
    let mut counts: Vec<(u16, Count)> = Vec::new();
    const R: i64 = 9;
    for dy in -R..=R {
        for dx in -R..=R {
            let (x, y) = (sx + dx, sy + dy);
            if !inside(x, y) {
                continue;
            }
            let [r, g, b] = pixel(x, y);
            if r.max(g).max(b) - r.min(g).min(b) < opts.min_color {
                continue;
            }
            let key = (u16::from(r >> 3) << 10) | (u16::from(g >> 3) << 5) | u16::from(b >> 3);
            let i = if let Some(i) = counts.iter().position(|(k, _)| *k == key) {
                i
            } else {
                counts.push((key, Count::default()));
                counts.len() - 1
            };
            let count = &mut counts[i].1;
            count.n += 1;
            count.r += u32::from(r);
            count.g += u32::from(g);
            count.b += u32::from(b);
        }
    }
    let mut best: Option<&Count> = None;
    for (_, count) in &counts {
        if best.is_none_or(|best| count.n > best.n) {
            best = Some(count);
        }
    }
    let best = best?;
    let average = [best.r, best.g, best.b].map(|v| f64::from(v) / f64::from(best.n));
    let matches = |x, y| {
        pixel(x, y)
            .iter()
            .zip(average)
            .map(|(v, avg)| (f64::from(*v) - avg).abs())
            .sum::<f64>()
            <= opts.tol
    };
    let mut seed = None;
    if inside(sx, sy) && matches(sx, sy) {
        seed = Some((sx, sy));
    } else {
        'search: for radius in 1..=R {
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let (x, y) = (sx + dx, sy + dy);
                    if inside(x, y) && matches(x, y) {
                        seed = Some((x, y));
                        break 'search;
                    }
                }
            }
        }
    }
    let (seed_x, seed_y) = seed?;
    let mut seen = vec![false; total];
    let index = (seed_y * width + seed_x) as usize;
    let mut stack = vec![index];
    seen[index] = true;
    let (mut min_x, mut min_y, mut max_x, mut max_y, mut matched) = (width, height, -1, -1, 0usize);
    while let Some(index) = stack.pop() {
        let (x, y) = (index as i64 % width, index as i64 / width);
        matched += 1;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
        for (nx, ny) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
            if !inside(nx, ny) {
                continue;
            }
            let next = (ny * width + nx) as usize;
            if !seen[next] && matches(nx, ny) {
                seen[next] = true;
                stack.push(next);
            }
        }
    }
    let fraction = matched as f64 / total as f64;
    if fraction < opts.min_frac || fraction > opts.max_frac {
        return None;
    }
    Some(Rect::new(
        min_x as i32,
        min_y as i32,
        (max_x - min_x + 1) as i32,
        (max_y - min_y + 1) as i32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BG: [u8; 3] = [245, 245, 245];
    const HL: [u8; 3] = [90, 150, 240];
    const TEXT: [u8; 3] = [10, 10, 10];

    fn paint(buffer: &mut [u8], width: usize, x: usize, y: usize, rgb: [u8; 3]) {
        let i = (y * width + x) * 4;
        buffer[i..i + 4].copy_from_slice(&[rgb[2], rgb[1], rgb[0], 255]);
    }

    fn make_region(width: usize, height: usize, rect: Option<Rect>, text: bool) -> Vec<u8> {
        let mut buffer = vec![0; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                paint(&mut buffer, width, x, y, BG);
            }
        }
        if let Some(rect) = rect {
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    paint(&mut buffer, width, x as usize, y as usize, HL);
                }
            }
            if text {
                for x in (rect.x + 3..rect.x + rect.width - 2).step_by(5) {
                    for y in rect.y + 2..rect.y + rect.height - 2 {
                        paint(&mut buffer, width, x as usize, y as usize, TEXT);
                    }
                }
            }
        }
        buffer
    }

    // Same five vectors as test/unit/highlight.test.js (including BGRA construction).
    #[test]
    fn finds_highlight_block_among_normal_background() {
        let rect = Rect::new(40, 12, 90, 18);
        let buffer = make_region(200, 40, Some(rect), true);
        let r = find_highlight_rect(&buffer, 200, 40, 70, 20, HighlightOptions::default()).unwrap();
        assert!((r.x - rect.x).abs() <= 2 && (r.y - rect.y).abs() <= 2);
        assert!((r.width - rect.width).abs() <= 4 && (r.height - rect.height).abs() <= 4);
    }

    #[test]
    fn uniform_background_is_not_highlighted() {
        let buffer = make_region(200, 40, None, false);
        assert_eq!(
            find_highlight_rect(&buffer, 200, 40, 100, 20, HighlightOptions::default()),
            None
        );
    }

    #[test]
    fn highlight_filling_page_is_background() {
        let buffer = make_region(100, 30, Some(Rect::new(0, 0, 100, 30)), false);
        assert_eq!(
            find_highlight_rect(&buffer, 100, 30, 50, 15, HighlightOptions::default()),
            None
        );
    }

    #[test]
    fn tiny_highlight_is_noise() {
        let buffer = make_region(200, 40, Some(Rect::new(100, 20, 2, 2)), false);
        assert_eq!(
            find_highlight_rect(&buffer, 200, 40, 101, 21, HighlightOptions::default()),
            None
        );
    }

    #[test]
    fn multi_line_highlight_captures_full_height() {
        let buffer = make_region(220, 70, Some(Rect::new(15, 10, 180, 46)), true);
        assert!(
            find_highlight_rect(&buffer, 220, 70, 100, 33, HighlightOptions::default())
                .unwrap()
                .height
                >= 40
        );
    }

    #[test]
    fn disconnected_colours_do_not_expand_the_selected_block() {
        let rect = Rect::new(40, 12, 90, 18);
        let mut buffer = make_region(200, 40, Some(rect), true);
        for x in 170..190 {
            for y in 2..8 {
                paint(&mut buffer, 200, x, y, HL);
            }
        }
        // Land directly on a text glyph: detector must find a nearby seed.
        assert_eq!(
            find_highlight_rect(&buffer, 200, 40, 43, 20, HighlightOptions::default()),
            Some(rect)
        );
    }

    #[test]
    fn empty_short_and_outside_buffers_are_handled() {
        assert_eq!(
            find_highlight_rect(&[], 0, 2, 0, 0, HighlightOptions::default()),
            None
        );
        assert_eq!(
            find_highlight_rect(&[0; 15], 2, 2, 0, 0, HighlightOptions::default()),
            None
        );
        let buffer = make_region(200, 40, Some(Rect::new(0, 0, 20, 10)), false);
        assert_eq!(
            find_highlight_rect(&buffer, 200, 40, -1, 3, HighlightOptions::default()),
            Some(Rect::new(0, 0, 20, 10))
        );
        assert_eq!(
            find_highlight_rect(
                &buffer,
                200,
                40,
                i32::MAX,
                i32::MAX,
                HighlightOptions::default()
            ),
            None
        );
    }

    #[test]
    fn threshold_options_accept_zero_and_use_inclusive_fraction_edges() {
        let buffer = make_region(100, 10, Some(Rect::new(0, 0, 80, 10)), false);
        assert!(find_highlight_rect(
            &buffer,
            100,
            10,
            20,
            5,
            HighlightOptions {
                tol: 0.0,
                ..HighlightOptions::default()
            }
        )
        .is_some());
        assert!(find_highlight_rect(
            &buffer,
            100,
            10,
            20,
            5,
            HighlightOptions {
                max_frac: 0.799,
                ..HighlightOptions::default()
            }
        )
        .is_none());
        let buffer = make_region(200, 40, Some(Rect::new(100, 20, 2, 2)), false);
        assert!(find_highlight_rect(
            &buffer,
            200,
            40,
            101,
            21,
            HighlightOptions {
                min_frac: 0.0,
                ..HighlightOptions::default()
            }
        )
        .is_some());
    }
}
