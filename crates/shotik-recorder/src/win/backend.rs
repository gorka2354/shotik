//! Picks the sources by capture mode.

use std::any::Any;

use shotik_core::{Error, Result};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL};
use windows::Win32::System::SystemInformation::GetLocalTime;

use super::com::{qpc_ticks, MediaFoundation, ThreadSetup};
use super::d3d::D3d;
use super::sink::MfSink;
use super::source::{PipelineSource, SynthGrabber};
use super::synth_audio::SynthInput;
use super::wasapi::{Endpoint, WasapiInput};
use super::wgc::WgcGrabber;
use crate::backend::{AudioInput, AudioOpenError, Backend, MediaSink, MicDevice, SinkRequest, ThreadRole, VideoRequest, VideoSource};
use crate::names::LocalTime;
use crate::recorder::CaptureMode;
use crate::region::crop_box;
use crate::synth::{Scene, Tone, MICROPHONE_TONE_HZ, SYSTEM_TONE_HZ};
use crate::trace::Trace;

/// The synthetic source has no monitor to clip to.
const SYNTH_CANVAS: u32 = 16384;

fn device_line(what: &str, input: &WasapiInput) -> String {
    match input.device_format() {
        Some(format) => format!("{what}, {format}, resampled to 48000 Hz stereo"),
        None => what.to_string(),
    }
}

/// Outlives everything a session opens.
struct SessionGuard {
    _media: MediaFoundation,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        D3d::trim_all();
    }
}

pub(crate) struct WinBackend {
    capture: CaptureMode,
}

impl WinBackend {
    pub fn new(capture: CaptureMode) -> Self {
        Self { capture }
    }
}

impl Backend for WinBackend {
    fn session_guard(&self) -> Result<Box<dyn Any>> {
        Ok(Box::new(SessionGuard { _media: MediaFoundation::start()? }))
    }

    fn enter_thread(&self, role: ThreadRole) -> Box<dyn Any> {
        Box::new(ThreadSetup::enter(role))
    }

    fn now(&self) -> i64 {
        qpc_ticks()
    }

    fn local_time(&self) -> LocalTime {
        // SAFETY: returns the time by value.
        let t = unsafe { GetLocalTime() };
        LocalTime { year: t.wYear, month: t.wMonth, day: t.wDay, hour: t.wHour, minute: t.wMinute, second: t.wSecond }
    }

    fn open_video(&self, request: &VideoRequest, trace: &Trace) -> Result<Box<dyn VideoSource>> {
        match &self.capture {
            CaptureMode::Real { displays } => {
                let bounds = displays.by_id(request.display_id).physical_bounds;
                let center = POINT { x: bounds.x + bounds.width / 2, y: bounds.y + bounds.height / 2 };
                // SAFETY: takes the point by value and returns a handle or null.
                let monitor = unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONULL) };
                if monitor.is_invalid() {
                    return Err(Error::msg(format!("No display at {},{}", center.x, center.y)));
                }
                let d3d = D3d::shared(Some(monitor), trace)?;
                let grabber = WgcGrabber::start(&d3d, monitor, request.rect_phys, request.fps, trace)?;
                let (width, height) = grabber.size();
                Ok(Box::new(PipelineSource::new(Box::new(grabber), Some(d3d), width, height, request.fps)))
            }
            CaptureMode::Synthetic(options) => {
                let crop = crop_box(request.rect_phys, SYNTH_CANVAS, SYNTH_CANVAS)?;
                let d3d = match D3d::shared(None, trace) {
                    Ok(d3d) => Some(d3d),
                    Err(e) => {
                        trace.push(format!("no GPU for the synthetic source: {e}"));
                        None
                    }
                };
                let scene = Scene::new(crop.x, crop.y, crop.width, crop.height, options.marker);
                let grabber = SynthGrabber::new(scene, crop.width, crop.height);
                Ok(Box::new(PipelineSource::new(Box::new(grabber), d3d, crop.width, crop.height, request.fps)))
            }
        }
    }

    fn open_system_audio(&self, trace: &Trace) -> std::result::Result<Box<dyn AudioInput>, AudioOpenError> {
        match &self.capture {
            CaptureMode::Real { .. } => {
                let input = WasapiInput::open(Endpoint::Loopback)?;
                trace.push(device_line("system audio: loopback of the default output", &input));
                Ok(Box::new(input))
            }
            CaptureMode::Synthetic(options) => {
                let tone = Tone { hz: SYSTEM_TONE_HZ, marker: options.marker };
                Ok(Box::new(SynthInput::new(tone, options.system_silence)))
            }
        }
    }

    fn open_microphone(
        &self,
        device: &MicDevice,
        trace: &Trace,
    ) -> std::result::Result<Box<dyn AudioInput>, AudioOpenError> {
        match &self.capture {
            CaptureMode::Real { .. } => {
                let input = WasapiInput::open(Endpoint::Microphone(device.clone()))?;
                trace.push(device_line("microphone: capture opened", &input));
                Ok(Box::new(input))
            }
            CaptureMode::Synthetic(options) => {
                let tone = Tone { hz: MICROPHONE_TONE_HZ, marker: options.marker };
                Ok(Box::new(SynthInput::new(tone, None)))
            }
        }
    }

    fn open_audio_sink(&self, request: &SinkRequest, trace: &Trace) -> Result<Box<dyn MediaSink>> {
        Ok(Box::new(MfSink::audio_only(request, trace)?))
    }
}
