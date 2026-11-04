//! The seams between the session logic and the operating system: real capture, the synthetic test
//! source and the fakes of the unit tests all sit behind these traits.

use std::any::Any;
use std::path::PathBuf;

use shotik_core::{DisplayId, Rect, Result};

use crate::names::LocalTime;
use crate::trace::Trace;

/// What a thread of a session does, which decides its priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadRole {
    Worker,
    Video,
    Encode,
    AudioCapture,
    AudioMix,
}

/// `default` follows the system. A chosen device is never silently replaced by another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MicDevice {
    Default,
    Exact(String),
}

impl MicDevice {
    pub fn from_setting(id: &str) -> Self {
        if id.is_empty() || id == "default" {
            Self::Default
        } else {
            Self::Exact(id.to_string())
        }
    }
}

/// Why an input did not open. The name follows `microphone:` in the error the application shows,
/// with the same values as the DOMException names of the JS version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioOpenKind {
    NotAllowed,
    NotFound,
    NotReadable,
    Overconstrained,
    /// It opened and offers no audio.
    #[cfg_attr(not(windows), allow(dead_code))]
    Empty,
}

impl AudioOpenKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::NotAllowed => "NotAllowedError",
            Self::NotFound => "NotFoundError",
            Self::NotReadable => "NotReadableError",
            Self::Overconstrained => "OverconstrainedError",
            Self::Empty => "Error",
        }
    }

    /// Windows' answer when opening a device. A chosen device that is gone is what Chromium calls
    /// overconstrained: the application shows the same message for both. An id Windows cannot even
    /// parse belongs here too: settings from the Electron build hold Chromium's hashed device ids.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn from_hresult(hr: i32, exact_device: bool) -> Self {
        const E_ACCESSDENIED: u32 = 0x8007_0005;
        const E_INVALIDARG: u32 = 0x8007_0057;
        const E_NOTFOUND: u32 = 0x8007_0490;
        const AUDCLNT_E_DEVICE_INVALIDATED: u32 = 0x8889_0004;
        match hr as u32 {
            E_ACCESSDENIED => Self::NotAllowed,
            E_NOTFOUND | E_INVALIDARG | AUDCLNT_E_DEVICE_INVALIDATED if exact_device => Self::Overconstrained,
            E_NOTFOUND | AUDCLNT_E_DEVICE_INVALIDATED => Self::NotFound,
            _ => Self::NotReadable,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AudioOpenError {
    pub kind: AudioOpenKind,
    pub detail: String,
}

/// Interleaved stereo f32 at 48 kHz, the first frame captured at `at` on the shared clock.
pub(crate) struct AudioPacket<'a> {
    pub at: i64,
    pub samples: &'a [f32],
}

pub(crate) trait AudioInput: Send {
    /// Hands over what is ready, waiting at most about 10 ms when nothing is. An error means the
    /// input is gone for the rest of the session.
    fn read(&mut self, deliver: &mut dyn FnMut(AudioPacket<'_>)) -> Result<()>;
}

/// A picture in whatever form the session's encoder takes.
pub(crate) struct VideoFrame {
    pub payload: Box<dyn Any + Send>,
    /// False when the source had nothing new and this repeats the last picture.
    pub fresh: bool,
}

/// Top-down BGRA.
pub(crate) struct Snapshot {
    pub bgra: Vec<u8>,
    pub stride: usize,
    pub width: u32,
    pub height: u32,
}

pub(crate) trait MediaSink: Send {
    fn write_video(&mut self, frame: Box<dyn Any + Send>, pts: i64, duration: i64) -> Result<()>;
    /// Interleaved stereo at 48 kHz.
    fn write_audio(&mut self, pcm: &[i16], pts: i64, duration: i64) -> Result<()>;
    fn bytes_written(&self) -> u64;
    /// Completes the file. Dropping the sink instead abandons it.
    fn finalize(self: Box<Self>) -> Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VideoRequest {
    pub display_id: DisplayId,
    pub rect_phys: Rect,
    pub fps: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SinkRequest {
    pub path: PathBuf,
    pub audio: bool,
}

pub(crate) trait VideoSource: Send {
    /// The encoder is bound to the source: they share a GPU device, or neither uses one.
    fn open_sink(&mut self, request: &SinkRequest, trace: &Trace) -> Result<Box<dyn MediaSink>>;
    /// Blocks until the first picture exists. None when `aborted` said to give up.
    fn wait_first_frame(&mut self, aborted: &dyn Fn() -> bool) -> Result<Option<Snapshot>>;
    /// The latest picture, ready to encode.
    fn next_frame(&mut self) -> Result<VideoFrame>;
    /// Set when the source can deliver nothing more, for instance the display was unplugged.
    fn lost(&self) -> Option<String>;
}

pub(crate) trait Backend: Send + Sync + 'static {
    /// Held for the length of a session: whatever must live as long as its media objects.
    fn session_guard(&self) -> Result<Box<dyn Any>>;
    /// Held by each thread of a session for its lifetime.
    fn enter_thread(&self, role: ThreadRole) -> Box<dyn Any>;
    /// The shared clock, in 100 ns ticks.
    fn now(&self) -> i64;
    fn local_time(&self) -> LocalTime;
    fn open_video(&self, request: &VideoRequest, trace: &Trace) -> Result<Box<dyn VideoSource>>;
    fn open_system_audio(&self, trace: &Trace) -> std::result::Result<Box<dyn AudioInput>, AudioOpenError>;
    fn open_microphone(
        &self,
        device: &MicDevice,
        trace: &Trace,
    ) -> std::result::Result<Box<dyn AudioInput>, AudioOpenError>;
    fn open_audio_sink(&self, request: &SinkRequest, trace: &Trace) -> Result<Box<dyn MediaSink>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_setting_of_default_pins_nothing() {
        assert_eq!(MicDevice::from_setting("default"), MicDevice::Default);
        assert_eq!(MicDevice::from_setting(""), MicDevice::Default);
        assert_eq!(MicDevice::from_setting("usb-mic"), MicDevice::Exact("usb-mic".into()));
    }

    #[test]
    fn windows_errors_map_to_the_names_the_application_knows() {
        let hr = |v: u32| v as i32;
        assert_eq!(AudioOpenKind::from_hresult(hr(0x8007_0005), false).name(), "NotAllowedError");
        assert_eq!(AudioOpenKind::from_hresult(hr(0x8007_0490), false).name(), "NotFoundError");
        assert_eq!(AudioOpenKind::from_hresult(hr(0x8007_0490), true).name(), "OverconstrainedError");
        assert_eq!(AudioOpenKind::from_hresult(hr(0x8889_0004), true).name(), "OverconstrainedError");
        assert_eq!(AudioOpenKind::from_hresult(hr(0x8007_0057), true).name(), "OverconstrainedError");
        assert_eq!(AudioOpenKind::from_hresult(hr(0x8889_000A), false).name(), "NotReadableError");
        assert_eq!(AudioOpenKind::from_hresult(hr(0x8000_4005), true).name(), "NotReadableError");
        assert_eq!(AudioOpenKind::Empty.name(), "Error");
    }
}
