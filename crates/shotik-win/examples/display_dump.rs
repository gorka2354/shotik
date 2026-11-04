//! Prints the display layout and theme as JSON, to compare with what Electron reports on the same
//! machine: `screen.getAllDisplays()`, `systemPreferences.getAccentColor()`. Geometry only.

use shotik_core::geom::{Point, PointF, Rect};
use shotik_core::ports::{Displays, ThemeSource};
use shotik_win::displays::{set_dpi_awareness, WinDisplays};
use shotik_win::theme::WinTheme;

fn rect(r: Rect) -> String {
    format!(r#"{{"x":{},"y":{},"width":{},"height":{}}}"#, r.x, r.y, r.width, r.height)
}

fn main() {
    set_dpi_awareness().expect("per-monitor DPI awareness");
    let d = WinDisplays::new();
    let displays: Vec<String> = d
        .all()
        .iter()
        .map(|m| {
            format!(
                r#"{{"id":{},"bounds":{},"workArea":{},"scaleFactor":{},"physical":{},"primary":{}}}"#,
                m.id,
                rect(m.bounds),
                rect(m.work_area),
                m.scale_factor,
                rect(m.physical_bounds),
                m.primary
            )
        })
        .collect();
    let points: Vec<String> = std::env::args()
        .skip(1)
        .map(|a| {
            let mut it = a.split(',').map(|v| v.parse::<i32>().expect("x,y"));
            let (x, y) = (it.next().unwrap(), it.next().unwrap());
            let p = d.phys_to_dip(Point { x, y });
            let near = d.nearest(PointF { x: f64::from(x), y: f64::from(y) });
            format!(r#"{{"x":{x},"y":{y},"dipX":{},"dipY":{},"nearestIfDip":{}}}"#, p.x, p.y, near.id)
        })
        .collect();
    let theme = WinTheme::new().current();
    println!(
        r#"{{"displays":[{}],"points":[{}],"dark":{},"accent":"{}","platform":"{}"}}"#,
        displays.join(","),
        points.join(","),
        theme.dark,
        theme.accent,
        theme.platform
    );
}
