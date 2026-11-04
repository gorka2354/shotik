//! Window selection and surface fallback; all native work stays behind ports.
use serde::Serialize;
use serde_json::Value;
use shotik_core::ports::{Displays, WindowCaptureMethod, WindowSource};
use shotik_core::{Display, DisplayId, Error, Frame, Point, Rect, Result};

use crate::js;

/// The composition root supplies test-mode PID exclusion and SHOTIK_WINCAP_FAIL.
/// This module never reads environment variables or touches native windows itself.
#[derive(Clone, Copy, Debug)]
pub struct WindowCaptureOptions {
    pub exclude_pid: Option<u32>,
    pub force_print_window_failure: bool,
}

impl Default for WindowCaptureOptions {
    fn default() -> Self {
        Self {
            exclude_pid: Some(std::process::id()),
            force_print_window_failure: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ListedWindow {
    pub id: u64,
    pub title: String,
    pub app: Option<String>,
    pub bounds: Option<Rect>,
    pub display_id: Option<DisplayId>,
    pub minimized: bool,
}

pub struct CapturedWindow {
    pub frame: Frame,
    pub title: String,
    pub app: Option<String>,
    pub id: u64,
    pub method: WindowCaptureMethod,
    pub other_matches: Vec<String>,
    pub uniform: bool,
}

pub fn score_title(win_title: &str, query: &str) -> u8 {
    let t = win_title.to_lowercase();
    let q = query.to_lowercase();
    if t == q {
        3
    } else if t.starts_with(&q) {
        2
    } else if t.contains(&q) {
        1
    } else {
        0
    }
}

pub fn looks_blank(frame: &Frame) -> bool {
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    if width < 2 || height < 2 {
        return true;
    }
    let bmp = frame.bgra();
    for y in (0..height).step_by((height / 16).max(1)) {
        for x in (0..width).step_by((width / 16).max(1)) {
            let i = (y * width + x) * 4;
            if (0..3).any(|c| bmp[i + c].abs_diff(bmp[c]) > 3) {
                return false;
            }
        }
    }
    true
}

fn overlap(a: Rect, b: Rect) -> i64 {
    let width = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width))
        - i64::from(a.x.max(b.x));
    let height = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height))
        - i64::from(a.y.max(b.y));
    width.max(0) * height.max(0)
}

fn matching_display(displays: &[Display], rect: Rect, physical: bool) -> Option<&Display> {
    // Largest intersecting area; nearest rectangle when completely off screen.
    displays.iter().rev().max_by(|a, b| {
        let bounds = |d: &Display| {
            if physical {
                d.physical_bounds
            } else {
                d.bounds
            }
        };
        let distance = |d: &Display| {
            let b = bounds(d);
            let dx = (f64::from(b.x) - f64::from(rect.x) - f64::from(rect.width))
                .max(f64::from(rect.x) - f64::from(b.x) - f64::from(b.width))
                .max(0.0);
            let dy = (f64::from(b.y) - f64::from(rect.y) - f64::from(rect.height))
                .max(f64::from(rect.y) - f64::from(b.y) - f64::from(b.height))
                .max(0.0);
            dx * dx + dy * dy
        };
        overlap(rect, bounds(a))
            .cmp(&overlap(rect, bounds(b)))
            .then_with(|| distance(b).total_cmp(&distance(a)))
    })
}

pub fn list_windows(
    source: &dyn WindowSource,
    displays: &dyn Displays,
    options: WindowCaptureOptions,
) -> Result<Vec<ListedWindow>> {
    let ds = displays.all();
    source
        .list(options.exclude_pid)?
        .into_iter()
        .map(|w| {
            if w.minimized {
                return Ok(ListedWindow {
                    id: w.id,
                    title: w.title,
                    app: w.app.filter(|s| !s.is_empty()),
                    bounds: None,
                    display_id: None,
                    minimized: true,
                });
            }
            let rect = w
                .bounds
                .ok_or_else(|| Error::msg("window enumeration returned malformed data"))?;
            let Some(d) = matching_display(&ds, rect, true) else {
                // screenToDipRect's reference fallback when no matching screen can
                // be obtained: preserve physical bounds and report no display id.
                return Ok(ListedWindow {
                    id: w.id,
                    title: w.title,
                    app: w.app.filter(|s| !s.is_empty()),
                    bounds: Some(rect),
                    display_id: None,
                    minimized: false,
                });
            };
            // Anchor on an interior physical point, so a rectangle spanning two
            // scales uses the scale of its matching display, just like screenToDipRect.
            let p = Point {
                x: d.physical_bounds.x + d.physical_bounds.width / 2,
                y: d.physical_bounds.y + d.physical_bounds.height / 2,
            };
            let anchor = displays.phys_to_dip(p);
            let dip = Rect::new(
                js::round(anchor.x + (f64::from(rect.x) - f64::from(p.x)) / d.scale_factor) as i32,
                js::round(anchor.y + (f64::from(rect.y) - f64::from(p.y)) / d.scale_factor) as i32,
                js::round(f64::from(rect.width) / d.scale_factor) as i32,
                js::round(f64::from(rect.height) / d.scale_factor) as i32,
            );
            let display_id = matching_display(&ds, dip, false).map(|d| d.id);
            Ok(ListedWindow {
                id: w.id,
                title: w.title,
                app: w.app.filter(|s| !s.is_empty()),
                bounds: Some(dip),
                display_id,
                minimized: false,
            })
        })
        .collect()
}

pub fn capture_window(
    source: &dyn WindowSource,
    window_id: Option<&Value>,
    title: Option<&Value>,
    options: WindowCaptureOptions,
) -> Result<CapturedWindow> {
    let window_id = window_id.filter(|id| !id.is_null());
    let title = title.filter(|v| js::truthy(Some(v)));
    if window_id.is_none() && title.is_none() {
        return Err(Error::msg(
            "Pass window_id (from list_windows) or title (substring of the window title)",
        ));
    }
    let wins = source.list(options.exclude_pid)?;
    if wins.is_empty() {
        return Err(Error::msg("Could not enumerate windows"));
    }
    let mut other_matches = Vec::new();
    let target = if let Some(id) = window_id {
        let target = wins
            .iter()
            .find(|w| w.id as f64 == js::number(Some(id)))
            .ok_or_else(|| {
                Error::msg(format!(
                    "No window with id {}. Use list_windows for current ids.",
                    js::string(Some(id))
                ))
            })?;
        if let Some(title) = title {
            let title = title
                .as_str()
                .ok_or_else(|| Error::msg("query.toLowerCase is not a function"))?;
            if score_title(&target.title, title) == 0 {
                return Err(Error::msg(format!("Window id {} now belongs to \"{}\", which does not match title \"{title}\" — the id is stale (window ids get recycled). Call list_windows again for a fresh id.", js::string(Some(id)), target.title)));
            }
        }
        target
    } else {
        let title = title
            .expect("validated above")
            .as_str()
            .ok_or_else(|| Error::msg("query.toLowerCase is not a function"))?;
        let mut scored: Vec<_> = wins
            .iter()
            .map(|w| (w, score_title(&w.title, title)))
            .filter(|(_, score)| *score > 0)
            .collect();
        scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        let target = scored
            .first()
            .ok_or_else(|| {
                Error::msg(format!(
                    "No window matches title \"{title}\". Use list_windows for exact titles."
                ))
            })?
            .0;
        other_matches = scored
            .iter()
            .skip(1)
            .take(5)
            .map(|(w, _)| w.title.clone())
            .collect();
        target
    };
    if target.minimized {
        return Err(Error::msg(format!("Window \"{}\" is minimized — its surface has no pixels while iconic. Ask the user to restore it, then retry.", target.title)));
    }
    let primary = if options.force_print_window_failure {
        Err(Error::msg("forced failure (test)"))
    } else {
        source
            .capture(target.id, WindowCaptureMethod::PrintWindow)
            .and_then(|frame| {
                if frame.width() == 0 || frame.height() == 0 {
                    Err(Error::msg("PrintWindow produced an empty image"))
                } else {
                    Ok(frame)
                }
            })
    };
    let (mut frame, first_error) = match primary {
        Ok(frame) => (Some(frame), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let mut method = WindowCaptureMethod::PrintWindow;
    if frame.as_ref().is_none_or(looks_blank) {
        // Intentionally keep a valid primary frame if the fallback fails or is
        // also uniform. That is part of the reference's observable contract.
        if let Ok(alt) = source.capture(target.id, WindowCaptureMethod::GraphicsCapture) {
            if alt.width() > 0 && alt.height() > 0 && (!looks_blank(&alt) || frame.is_none()) {
                frame = Some(alt);
                method = WindowCaptureMethod::GraphicsCapture;
            }
        }
    }
    let frame = frame.ok_or_else(|| Error::msg(format!(
        "Could not capture \"{}\"{}. The window may use a GPU-only surface; take_screenshot of its display is the fallback.",
        target.title, first_error.map_or_else(String::new, |e| format!(" ({e})")))))?;
    let uniform = looks_blank(&frame);
    Ok(CapturedWindow {
        frame,
        title: target.title.clone(),
        app: target.app.clone().filter(|s| !s.is_empty()),
        id: target.id,
        method,
        other_matches,
        uniform,
    })
}
