//! Video and audio capture, encoding and muxing; GIF export.
//!
//! [`Recorder`] replaces `src/main/recorder.js` and `src/recorder/recorder.js`, and [`export_gif`]
//! replaces `src/main/gifexport.js` and `src/gifexport/`. Windows Graphics Capture for frames, WASAPI
//! for sound, Media Foundation for H.264 and AAC in MP4. Whether the sources are real or synthetic is
//! a constructor parameter, see [`CaptureMode`].

mod backend;
mod clock;
mod convert;
#[cfg(windows)]
mod devices;
mod engine;
mod gif_export;
mod mix;
mod names;
mod recorder;
mod region;
mod resample;
mod session;
mod synth;
mod trace;

#[cfg(test)]
mod recorder_tests;
#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use devices::WinAudioDevices;
pub use gif_export::export_gif;
pub use recorder::{CaptureMode, CountdownHook, Recorder, RecorderConfig};
pub use synth::{Marker, SilentStretches, SynthOptions};
