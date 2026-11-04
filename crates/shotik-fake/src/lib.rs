//! Fake implementations of the ports, for tests and ghost mode.
//!
//! Ghost mode replaces the screen with a fixture image: `SHOTIK_FAKE_SCREEN` in the JS version
//! (`grabDisplay` and `enumWindows` in `src/main/capture.js`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use shotik_core::ports::{NativeWindow, ScreenCapture, WindowCaptureMethod, WindowSource};
use shotik_core::{Display, Error, Frame, Rect, Result};

/// Serves a fixture image as every display, scaled to the display's physical size.
pub struct FakeScreen {
    path: PathBuf,
    /// Scaled copies by size: resizing to 4K on every grab would dominate what tests measure.
    scaled: Mutex<HashMap<(u32, u32), Frame>>,
}

impl FakeScreen {
    /// Fails at once when the fixture is unreadable, instead of on the first capture.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        load(&path)?;
        Ok(Self { path, scaled: Mutex::new(HashMap::new()) })
    }
}

fn load(path: &Path) -> Result<Frame> {
    let bytes = std::fs::read(path).map_err(|e| Error::io("read fake screen", path, e))?;
    Frame::from_png(&bytes)
}

impl ScreenCapture for FakeScreen {
    fn grab(&self, display: &Display) -> Result<Frame> {
        let size = (display.physical_bounds.width as u32, display.physical_bounds.height as u32);
        let mut scaled = self.scaled.lock().expect("fake screen cache poisoned");
        if let Some(frame) = scaled.get(&size) {
            return Ok(frame.clone());
        }
        let original = load(&self.path)?;
        let frame = if (original.width(), original.height()) == size {
            original
        } else {
            original.resize(size.0, size.1)?
        };
        scaled.insert(size, frame.clone());
        Ok(frame)
    }
}

/// The two windows drawn in `test/fake-screen.png`, in physical pixels of the primary display, for
/// hover-snap in ghost mode. The fixture is 1920x1080 stretched onto that display.
pub fn fixture_snap_rects(primary: &Display) -> Vec<Rect> {
    let k = f64::from(primary.physical_bounds.width) / 1920.0;
    let s = |v: i32| (f64::from(v) * k).round() as i32;
    vec![Rect::new(s(240), s(140), s(1100), s(640)), Rect::new(s(1470), s(170), s(180), s(60))]
}

/// A window source with a fixed list. `capture` serves a frame set per window id.
#[derive(Default)]
pub struct FakeWindows {
    pub windows: Mutex<Vec<NativeWindow>>,
    pub frames: Mutex<HashMap<u64, Frame>>,
}

impl WindowSource for FakeWindows {
    fn list(&self, _exclude_pid: Option<u32>) -> Result<Vec<NativeWindow>> {
        Ok(self.windows.lock().expect("fake windows poisoned").clone())
    }

    fn capture(&self, id: u64, _method: WindowCaptureMethod) -> Result<Frame> {
        self.frames
            .lock()
            .expect("fake frames poisoned")
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::msg(format!("no fake frame for window {id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(w: i32, h: i32) -> Display {
        Display {
            id: 1,
            bounds: Rect::new(0, 0, w, h),
            work_area: Rect::new(0, 0, w, h),
            scale_factor: 1.0,
            physical_bounds: Rect::new(0, 0, w, h),
            primary: true,
        }
    }

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fake-screen.png")
    }

    #[test]
    fn serves_the_fixture_at_the_display_size() {
        let screen = FakeScreen::new(fixture()).unwrap();
        let frame = screen.grab(&display(960, 540)).unwrap();
        assert_eq!((frame.width(), frame.height()), (960, 540));
    }

    #[test]
    fn a_missing_fixture_fails_at_construction() {
        assert!(FakeScreen::new("no-such-file.png").is_err());
    }

    #[test]
    fn snap_rects_follow_the_physical_width() {
        assert_eq!(fixture_snap_rects(&display(1920, 1080))[0], Rect::new(240, 140, 1100, 640));
        assert_eq!(fixture_snap_rects(&display(3840, 2160))[1], Rect::new(2940, 340, 360, 120));
    }
}
