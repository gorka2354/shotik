//! One frame through Desktop Duplication: a duplication opened for a single frame on the shared device.
//! 1000 of them on one device grew nothing.
//!
//! A new duplication hands over a blank first frame when the screen is not changing: `AccumulatedFrames` and
//! `LastPresentTime` are 0 and the texture is black, in 87 of 90 grabs on a quiet machine. The next frame only
//! comes when something on the output changes. So this cannot photograph a still screen, and it says so with an
//! error rather than returning black. It works where it is used: a fullscreen game presents all the time.

use std::time::{Duration, Instant};

use shotik_core::error::{Error, Result};
use shotik_core::frame::Frame;
use windows::core::{Interface, HRESULT};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_ACCESS_DENIED, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_UNSUPPORTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Graphics::Gdi::HMONITOR;

use super::d3d::shared_device;
use crate::displays::win_err;

/// The adapter and output driving a monitor. The list is asked for each time: outputs come and go, and
/// an old factory keeps answering with what was true when it was made.
fn output_for(monitor: HMONITOR) -> Result<(IDXGIAdapter1, IDXGIOutput1)> {
    // SAFETY: creates a COM object with no input.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| win_err("cannot reach DXGI", e))?;
    let mut a = 0;
    // SAFETY: both enumerations return an error past the last index, which ends the loops.
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(a) } {
        let mut o = 0;
        while let Ok(output) = unsafe { adapter.EnumOutputs(o) } {
            // SAFETY: a plain getter on a live object.
            let desc = unsafe { output.GetDesc() }.map_err(|e| win_err("cannot read a DXGI output", e))?;
            if desc.Monitor == monitor {
                let output = output.cast().map_err(|e| win_err("IDXGIOutput1", e))?;
                return Ok((adapter, output));
            }
            o += 1;
        }
        a += 1;
    }
    Err(Error::msg("DXGI does not know this monitor"))
}

/// What a failed duplication means, in words someone can act on.
fn explain(code: HRESULT) -> &'static str {
    if code == E_ACCESSDENIED || code == DXGI_ERROR_ACCESS_DENIED {
        "access denied: the secure desktop may be active"
    } else if code == DXGI_ERROR_ACCESS_LOST {
        "the display mode or the desktop just changed"
    } else if code == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
        "too many applications are duplicating this output"
    } else if code == DXGI_ERROR_UNSUPPORTED {
        "this adapter and output do not support it"
    } else if code == DXGI_ERROR_WAIT_TIMEOUT {
        "no new frame arrived: nothing on the screen changed, or the display is asleep"
    } else {
        "unexpected failure"
    }
}

fn duplication_err(what: &str, e: windows::core::Error) -> Error {
    Error::msg(format!("{what}: {} ({e})", explain(e.code())))
}

/// An acquired frame, released on drop. The API allows one at a time, and a duplication with an unreleased
/// frame cannot be used again.
struct AcquiredFrame<'a>(&'a IDXGIOutputDuplication);

impl Drop for AcquiredFrame<'_> {
    fn drop(&mut self) {
        // SAFETY: only constructed after AcquireNextFrame succeeded, released once.
        let _ = unsafe { self.0.ReleaseFrame() };
    }
}

pub(crate) fn grab(monitor: HMONITOR, timeout: Duration) -> Result<Frame> {
    let (adapter, output) = output_for(monitor)?;
    let device = shared_device(Some(&adapter))?;
    // SAFETY: both objects are alive, and the device is on the output's adapter, as the API requires.
    let duplication = unsafe { output.DuplicateOutput(&device.d3d().device) }
        .map_err(|e| duplication_err("cannot duplicate this output", e))?;
    // SAFETY: a plain getter on a live object.
    let rotation = unsafe { duplication.GetDesc() }.Rotation;
    if rotation != DXGI_MODE_ROTATION_IDENTITY && rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
        // Duplication hands out the panel's orientation. This is only ever a second opinion, and a rotated
        // monitor running an exclusive fullscreen game does not justify rotating pixels here.
        return Err(Error::msg("this output is rotated, which this capture path does not handle"));
    }
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: the out-parameters are valid for the call.
        unsafe { duplication.AcquireNextFrame(left.as_millis() as u32, &mut info, &mut resource) }
            .map_err(|e| duplication_err("no frame from the duplication", e))?;
        let frame = AcquiredFrame(&duplication);
        if is_blank(info.AccumulatedFrames, info.LastPresentTime) {
            // Released by the guard before the next acquire, as the API requires.
            continue;
        }
        let texture: ID3D11Texture2D = resource
            .as_ref()
            .ok_or_else(|| Error::msg("the duplication returned no surface"))?
            .cast()
            .map_err(|e| win_err("ID3D11Texture2D", e))?;
        let pixels = device.read_texture(&texture);
        drop(texture);
        drop(resource);
        drop(frame);
        return pixels;
    }
}

/// A frame the system has not presented anything into: no accumulated updates, and no present time.
fn is_blank(accumulated_frames: u32, last_present_time: i64) -> bool {
    accumulated_frames == 0 || last_present_time == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::HRESULT;

    #[test]
    fn a_frame_nothing_was_presented_into_is_blank() {
        assert!(is_blank(0, 0));
        assert!(is_blank(0, 12345));
        assert!(is_blank(1, 0));
        assert!(!is_blank(1, 12345));
    }

    #[test]
    fn every_known_failure_says_what_it_means() {
        assert!(explain(E_ACCESSDENIED).contains("secure desktop"));
        assert!(explain(DXGI_ERROR_ACCESS_DENIED).contains("secure desktop"));
        assert!(explain(DXGI_ERROR_ACCESS_LOST).contains("changed"));
        assert!(explain(DXGI_ERROR_NOT_CURRENTLY_AVAILABLE).contains("too many"));
        assert!(explain(DXGI_ERROR_UNSUPPORTED).contains("support"));
        assert!(explain(DXGI_ERROR_WAIT_TIMEOUT).contains("nothing on the screen changed"));
        assert!(explain(HRESULT(0x8000_4005_u32 as i32)).contains("unexpected"));
    }
}
