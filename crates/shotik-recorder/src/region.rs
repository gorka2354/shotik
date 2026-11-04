//! What a region of a monitor means for the encoder.

use shotik_core::{Error, Rect, Result};

/// The part of the monitor that is encoded, physical pixels relative to the monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Clips the region to the monitor and makes its size even, as 4:2:0 requires. The top left corner
/// stays where it is: a pixel comes off the right or the bottom, the picture never shifts.
pub fn crop_box(rect: Rect, monitor_width: u32, monitor_height: u32) -> Result<CropBox> {
    let mw = i64::from(monitor_width);
    let mh = i64::from(monitor_height);
    let left = i64::from(rect.x).max(0);
    let top = i64::from(rect.y).max(0);
    let right = (i64::from(rect.x) + i64::from(rect.width)).min(mw);
    let bottom = (i64::from(rect.y) + i64::from(rect.height)).min(mh);
    let width = (right - left).max(0) as u32 & !1;
    let height = (bottom - top).max(0) as u32 & !1;
    if width < 2 || height < 2 {
        return Err(Error::msg(format!(
            "The recording region {}x{} at {},{} is outside the {monitor_width}x{monitor_height} display or too small",
            rect.width, rect.height, rect.x, rect.y
        )));
    }
    Ok(CropBox { x: left as u32, y: top as u32, width, height })
}

/// Bits per second for H.264 of screen content. 4 Mbit/s for 1280x720 at 30 fps stays under the
/// 35 MB per minute of the Electron build even when everything moves, and sublinear growth gives
/// 7.4 for 1080p and 21 for 4K. A still screen takes far less: the rate is a ceiling for VBR.
pub fn video_bitrate(width: u32, height: u32, fps: u32) -> u32 {
    const REFERENCE_PIXELS_PER_SEC: f64 = 1280.0 * 720.0 * 30.0;
    const REFERENCE_BITRATE: f64 = 4_000_000.0;
    let pixels_per_sec = f64::from(width) * f64::from(height) * f64::from(fps);
    let bitrate = REFERENCE_BITRATE * (pixels_per_sec / REFERENCE_PIXELS_PER_SEC).powf(0.75);
    bitrate.clamp(500_000.0, 100_000_000.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_even_region_inside_the_monitor_is_untouched() {
        let got = crop_box(Rect::new(100, 100, 1280, 720), 3840, 2160).unwrap();
        assert_eq!(got, CropBox { x: 100, y: 100, width: 1280, height: 720 });
    }

    #[test]
    fn odd_sizes_lose_the_last_row_and_column_without_shifting() {
        let got = crop_box(Rect::new(31, 33, 481, 271), 3840, 2160).unwrap();
        assert_eq!(got, CropBox { x: 31, y: 33, width: 480, height: 270 });
    }

    #[test]
    fn a_region_past_the_edge_is_clipped() {
        let got = crop_box(Rect::new(3000, 2000, 2000, 500), 3840, 2160).unwrap();
        assert_eq!(got, CropBox { x: 3000, y: 2000, width: 840, height: 160 });
        let got = crop_box(Rect::new(-10, -20, 110, 120), 3840, 2160).unwrap();
        assert_eq!(got, CropBox { x: 0, y: 0, width: 100, height: 100 });
    }

    #[test]
    fn a_region_with_nothing_left_is_an_error() {
        assert!(crop_box(Rect::new(4000, 0, 100, 100), 3840, 2160).is_err());
        assert!(crop_box(Rect::new(0, 0, 1, 100), 3840, 2160).is_err());
        assert!(crop_box(Rect::new(0, 0, 0, 0), 3840, 2160).is_err());
    }

    #[test]
    fn bitrate_grows_with_pixels_and_stays_under_the_electron_rate_for_the_bench_case() {
        let bench = video_bitrate(1280, 720, 30);
        assert_eq!(bench, 4_000_000);
        assert!(f64::from(bench) / 8.0 * 60.0 / 1e6 < 35.0);
        assert!(video_bitrate(1920, 1080, 30) > bench);
        assert!(video_bitrate(3840, 2160, 60) > video_bitrate(3840, 2160, 30));
        assert_eq!(video_bitrate(2, 2, 1), 500_000);
    }
}
