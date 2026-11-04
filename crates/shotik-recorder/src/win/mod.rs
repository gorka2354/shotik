//! The Windows side of recording. Everything runs in the multithreaded apartment: Media Foundation,
//! Direct3D 11 with multithread protection, WASAPI and free-threaded Windows Graphics Capture are all
//! usable from any MTA thread, which is what makes the `unsafe impl Send` in these modules sound.

pub(crate) mod backend;
mod com;
mod d3d;
pub(crate) mod gif_decode;
mod sink;
mod source;
mod synth_audio;
mod wasapi;
mod wgc;
