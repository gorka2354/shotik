//! Coordinates. Two spaces exist and must not be mixed:
//! physical pixels, and global virtual-desktop DIP (what `list_displays` reports, see CONTRACT.md).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PointF {
    pub x: f64,
    pub y: f64,
}

/// Serializes as `{x,y,width,height}`. Also reads `{x,y,w,h}`, the overlay's shape in the JS version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    #[serde(alias = "w")]
    pub width: i32,
    #[serde(alias = "h")]
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self { x, y, width, height }
    }

    pub fn contains(&self, p: PointF) -> bool {
        p.x >= f64::from(self.x)
            && p.x <= f64::from(self.x + self.width)
            && p.y >= f64::from(self.y)
            && p.y <= f64::from(self.y + self.height)
    }
}

pub type DisplayId = u64;

/// One monitor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Display {
    /// Stable across restarts while the monitor stays connected.
    pub id: DisplayId,
    /// Global virtual-desktop DIP.
    pub bounds: Rect,
    /// Bounds minus the taskbar, global DIP.
    pub work_area: Rect,
    pub scale_factor: f64,
    /// Global physical pixels. The size of a captured frame.
    pub physical_bounds: Rect,
    pub primary: bool,
}
