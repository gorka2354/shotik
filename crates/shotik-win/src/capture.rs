//! Screen capture of one display. Implements `ScreenCapture`.
//!
//! The method is GDI `BitBlt` with `CAPTUREBLT`, chosen by measurement (`examples/capture_compare.rs`, three real
//! monitors, both on a quiet machine and with a game holding the GPU at 99%). The 3840x2160 primary takes about
//! 45 ms warm and 60 ms cold, where Electron takes 350 ms, and the numbers hardly move under GPU load. Handle count
//! and private bytes stay flat across thousands of grabs. Nothing is held: no device, no session, no frames. So
//! `warm_up` has nothing to do, and the first grab after a week in the tray costs the same as the hundredth. No
//! code is needed for rotated monitors either.
//!
//! The other two lost on evidence, not taste:
//! - Desktop Duplication opened per grab returns a blank frame on a still screen, with `AccumulatedFrames` 0, in
//!   89 of 90 grabs on a quiet machine, and its 3 to 12 ms are the time to fetch that nothing. Seeing a still
//!   screen takes a duplication left running and a held frame, which a tray app must not carry.
//! - Windows.Graphics.Capture is correct, but setting up a session takes 40 ms before the first frame, about 50 to
//!   90 ms warm with tails near 300, and over 200 ms cold.
//! - Either one on a device created per grab leaks a Section and a Mutant each time, see `capture/d3d.rs`.
//!
//! One explicit fallback. An exclusive fullscreen application bypasses the compositor, and GDI may read a flat
//! colour there. Only that combination, a flat frame while the shell reports exclusive fullscreen, asks Desktop
//! Duplication for a second opinion, which works there because such a game presents all the time. A solid
//! wallpaper never gets that far, because the first such request creates a Direct3D device that stays for the life
//! of the process. If the second opinion sees content, that frame wins; otherwise the flat one stands. Every other
//! failure is reported.

use std::time::Duration;

use shotik_core::error::{Error, Result};
use shotik_core::frame::Frame;
use shotik_core::geom::Display;
use shotik_core::ports::ScreenCapture;
use windows::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN};

use crate::displays::{monitor_handle, PerMonitorDpiScope};

pub(crate) mod d3d;
pub(crate) mod dxgi;
pub(crate) mod gdi;
pub(crate) mod wgc;

/// The second opinion on a flat frame. One composition normally; a display asleep produces nothing at all,
/// and the flat frame must not cost the user more than this.
const SECOND_OPINION_TIMEOUT: Duration = Duration::from_millis(500);

/// How a frame was produced, for tests and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMethod {
    Gdi,
    DesktopDuplication,
}

/// One flat colour, sampled on a 16x16 grid with a small tolerance: `looksBlank` in `window-capture.js`.
pub fn is_flat(frame: &Frame) -> bool {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    if w < 2 || h < 2 {
        return true;
    }
    let bgra = frame.bgra();
    let (b0, g0, r0) = (bgra[0], bgra[1], bgra[2]);
    let (step_x, step_y) = ((w / 16).max(1), (h / 16).max(1));
    for y in (0..h).step_by(step_y) {
        for x in (0..w).step_by(step_x) {
            let i = (y * w + x) * 4;
            if bgra[i].abs_diff(b0) > 3 || bgra[i + 1].abs_diff(g0) > 3 || bgra[i + 2].abs_diff(r0) > 3 {
                return false;
            }
        }
    }
    true
}

/// While a UAC prompt or the lock screen has the input, nothing an ordinary process can do reaches it: GDI
/// would hand back a black frame as if all were well. `OpenInputDesktop` is denied exactly then.
fn ensure_input_desktop() -> Result<()> {
    const DESKTOP_READOBJECTS: u32 = 0x0001;
    // SAFETY: plain flags; the handle, if any, is closed right below.
    match unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(DESKTOP_READOBJECTS)) } {
        Ok(desktop) => {
            // SAFETY: opened above and closed once.
            let _ = unsafe { CloseDesktop(desktop) };
            Ok(())
        }
        Err(e) => Err(Error::msg(format!(
            "the screen cannot be captured while the secure desktop is active (a UAC prompt or the lock screen): {e}"
        ))),
    }
}

/// An exclusive fullscreen application bypasses the compositor GDI reads from. The shell knows.
fn exclusive_fullscreen_running() -> bool {
    // SAFETY: a read-only query with no input.
    matches!(unsafe { SHQueryUserNotificationState() }, Ok(state) if state == QUNS_RUNNING_D3D_FULL_SCREEN)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WinCapture;

impl WinCapture {
    pub fn new() -> Self {
        Self
    }

    /// `grab`, also saying which method produced the frame.
    pub fn grab_with_method(&self, display: &Display) -> Result<(Frame, CaptureMethod)> {
        let _dpi = PerMonitorDpiScope::enter();
        let monitor = monitor_handle(display)?;
        ensure_input_desktop()?;
        let frame = gdi::grab(display.physical_bounds)?;
        // In this order: the flat check costs microseconds, and most frames stop there.
        if !is_flat(&frame) || !exclusive_fullscreen_running() {
            return Ok((frame, CaptureMethod::Gdi));
        }
        // A failed second opinion says nothing about the flat frame, which may well be real.
        if let Ok(other) = dxgi::grab(monitor, SECOND_OPINION_TIMEOUT) {
            let same_size = (other.width(), other.height()) == (frame.width(), frame.height());
            if same_size && !is_flat(&other) {
                return Ok((other, CaptureMethod::DesktopDuplication));
            }
        }
        Ok((frame, CaptureMethod::Gdi))
    }
}

impl ScreenCapture for WinCapture {
    fn grab(&self, display: &Display) -> Result<Frame> {
        self.grab_with_method(display).map(|(frame, _)| frame)
    }
    // `warm_up` keeps the trait's empty default on purpose, see the top of this file.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::WinDisplays;
    use shotik_core::geom::Rect;
    use shotik_core::ports::Displays;

    fn solid(w: u32, h: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
        std::iter::repeat_n([b, g, r, 255], (w * h) as usize).flatten().collect()
    }

    #[test]
    fn flat_means_one_colour_on_the_grid_within_tolerance() {
        assert!(is_flat(&Frame::new(64, 64, solid(64, 64, 10, 20, 30)).unwrap()));
        assert!(is_flat(&Frame::new(1, 1, solid(1, 1, 10, 20, 30)).unwrap()), "too small to tell");

        let mut near = solid(64, 64, 10, 20, 30);
        near[4] = 13;
        assert!(is_flat(&Frame::new(64, 64, near).unwrap()), "within the tolerance");

        let mut content = solid(64, 64, 10, 20, 30);
        let i = (32 * 64 + 32) * 4;
        content[i] = 200;
        assert!(!is_flat(&Frame::new(64, 64, content).unwrap()), "a sampled pixel differs");

        let mut gradient = Vec::new();
        for y in 0..64u32 {
            for x in 0..64u32 {
                gradient.extend_from_slice(&[x as u8 * 4, y as u8 * 4, 0, 255]);
            }
        }
        assert!(!is_flat(&Frame::new(64, 64, gradient).unwrap()));
    }

    /// The fallback path itself, on a real monitor. A still screen has no new frame to give, which must be an
    /// error and never a blank picture. A changing one gives a full frame.
    #[test]
    fn the_second_opinion_returns_content_or_says_there_is_none() {
        let _dpi = PerMonitorDpiScope::enter();
        for display in WinDisplays::new().all() {
            let monitor = monitor_handle(&display).unwrap();
            match dxgi::grab(monitor, Duration::from_millis(300)) {
                Ok(frame) => {
                    assert_eq!(frame.width() as i32, display.physical_bounds.width);
                    assert_eq!(frame.height() as i32, display.physical_bounds.height);
                    assert!(frame.bgra().chunks_exact(4).all(|px| px[3] == 255));
                    assert!(!is_flat(&frame), "a blank frame must not come back as a success");
                }
                Err(e) => assert!(e.to_string().contains("nothing on the screen changed"), "{e}"),
            }
        }
    }

    #[test]
    fn the_input_desktop_is_reachable_right_now() {
        ensure_input_desktop().unwrap();
    }

    #[test]
    fn every_monitor_comes_back_at_full_physical_size_and_opaque() {
        let capture = WinCapture::new();
        let mut any_content = false;
        for display in WinDisplays::new().all() {
            let (frame, _method) = capture.grab_with_method(&display).unwrap();
            let p = display.physical_bounds;
            assert_eq!((frame.width() as i32, frame.height() as i32), (p.width, p.height), "{display:?}");
            assert!(frame.bgra().chunks_exact(4).all(|px| px[3] == 255), "opaque on {display:?}");
            any_content |= !is_flat(&frame);
        }
        assert!(any_content, "at least one monitor shows more than one colour");
    }

    #[test]
    fn an_unaware_thread_still_gets_full_physical_frames() {
        let display = WinDisplays::new().primary();
        let frame = crate::displays::tests::on_unaware_thread(|| WinCapture::new().grab(&display).unwrap());
        assert_eq!(frame.width() as i32, display.physical_bounds.width);
        assert_eq!(frame.height() as i32, display.physical_bounds.height);
    }

    #[test]
    fn a_display_that_is_gone_is_an_explicit_error() {
        let capture = WinCapture::new();
        let mut gone = WinDisplays::new().primary();
        gone.physical_bounds = Rect::new(gone.physical_bounds.x, gone.physical_bounds.y, 11, 11);
        let err = capture.grab(&gone).unwrap_err().to_string();
        assert!(err.contains("is gone"), "{err}");

        let mut nothing = gone.clone();
        nothing.physical_bounds = Rect::default();
        assert!(capture.grab(&nothing).is_err());
    }

    #[test]
    fn grabbing_works_from_several_threads_at_once() {
        let displays = WinDisplays::new().all();
        std::thread::scope(|scope| {
            for _ in 0..3 {
                scope.spawn(|| {
                    let capture = WinCapture::new();
                    for display in &displays {
                        let frame = capture.grab(display).unwrap();
                        assert_eq!(frame.width() as i32, display.physical_bounds.width);
                    }
                });
            }
        });
    }

    #[test]
    fn a_canvas_refuses_an_empty_size() {
        assert!(gdi::Canvas::new(0, 10).is_err());
        assert!(gdi::Canvas::new(10, -1).is_err());
        let canvas = gdi::Canvas::new(3, 2).unwrap();
        assert_eq!(canvas.pixels().len(), 3 * 2 * 4);
    }
}
