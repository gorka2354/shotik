//! Global virtual-desktop DIP mapping, ported from `src/main/region-map.js`.
use serde_json::Value;
use shotik_core::{Display, DisplayId, Error, Rect, Result};

use crate::js;

/// JavaScript numbers are retained until clipping, including finite coordinates
/// outside i32. The mapped rectangle lies within a display's integer bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl From<Rect> for Region {
    fn from(r: Rect) -> Self {
        Self {
            x: r.x.into(),
            y: r.y.into(),
            width: r.width.into(),
            height: r.height.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappedRegion {
    pub display_id: DisplayId,
    pub rel_rect: Rect,
    pub global_rect: Rect,
    pub clipped: bool,
}

fn fmt(r: Region) -> String {
    format!(
        "{}x{} at ({}, {})",
        js::number_text(r.width),
        js::number_text(r.height),
        js::number_text(r.x),
        js::number_text(r.y)
    )
}

pub fn describe_displays(displays: &[Display]) -> String {
    displays
        .iter()
        .map(|d| format!("id {}: {}", d.id, fmt(d.bounds.into())))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn validate_region(args: &Value) -> Result<Region> {
    let mut values = [0.0; 4];
    for (index, key) in ["x", "y", "width", "height"].into_iter().enumerate() {
        let value = args.get(key);
        let n = js::number(value);
        if !n.is_finite() {
            return Err(Error::msg(format!(
                "Invalid region: \"{key}\" must be a finite number, got {}. Pass x, y, width, height in global virtual-desktop DIP coordinates (same space as list_displays bounds).",
                js::json(value))));
        }
        values[index] = js::round(n);
    }
    let [x, y, width, height] = values;
    if width < 1.0 || height < 1.0 {
        return Err(Error::msg(format!(
            "Invalid region: width and height must be >= 1, got {}x{}",
            js::number_text(width),
            js::number_text(height)
        )));
    }
    Ok(Region {
        x,
        y,
        width,
        height,
    })
}

pub fn map_global_region(
    displays: &[Display],
    rect: Region,
    display_id: Option<f64>,
) -> Result<MappedRegion> {
    let pool: Vec<_> = displays
        .iter()
        .filter(|d| display_id.is_none_or(|id| id == d.id as f64))
        .cloned()
        .collect();
    if let Some(id) = display_id.filter(|_| pool.is_empty()) {
        return Err(Error::msg(format!(
            "Unknown display_id {}. Displays: {}",
            js::number_text(id),
            describe_displays(displays)
        )));
    }
    let mut best: Option<(&Display, Region, f64)> = None;
    for d in &pool {
        let b = Region::from(d.bounds);
        let x = rect.x.max(b.x);
        let y = rect.y.max(b.y);
        let width = (rect.x + rect.width).min(b.x + b.width) - x;
        let height = (rect.y + rect.height).min(b.y + b.height) - y;
        if width < 1.0 || height < 1.0 {
            continue;
        }
        let area = width * height;
        if best.as_ref().is_none_or(|(_, _, a)| area > *a) {
            best = Some((
                d,
                Region {
                    x,
                    y,
                    width,
                    height,
                },
                area,
            ));
        }
    }
    let Some((d, inter, _)) = best else {
        let target = display_id.map_or_else(
            || "any display".into(),
            |id| format!("display {}", js::number_text(id)),
        );
        return Err(Error::msg(format!(
            "Region {} does not intersect {target}. Displays: {}",
            fmt(rect),
            describe_displays(if pool.is_empty() { displays } else { &pool })
        )));
    };
    Ok(MappedRegion {
        display_id: d.id,
        rel_rect: Rect::new(
            (inter.x - f64::from(d.bounds.x)) as i32,
            (inter.y - f64::from(d.bounds.y)) as i32,
            inter.width as i32,
            inter.height as i32,
        ),
        global_rect: Rect::new(
            inter.x as i32,
            inter.y as i32,
            inter.width as i32,
            inter.height as i32,
        ),
        clipped: inter.width != rect.width || inter.height != rect.height,
    })
}
