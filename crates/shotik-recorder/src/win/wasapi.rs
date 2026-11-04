//! WASAPI capture: loopback of the default output, or a microphone. The audio engine converts both to
//! the session's format (`AUTOCONVERTPCM`), with its own sample rate converter and channel matrix, so
//! any device works and nothing here depends on its native format.

use std::thread;
use std::time::Duration;

use shotik_core::{Error, Result};
use windows::core::HSTRING;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

use super::com::{hr_error, qpc_ticks};
use crate::backend::{AudioInput, AudioOpenError, AudioOpenKind, AudioPacket, MicDevice};
use crate::clock::TICKS_PER_SEC;
use crate::mix::{frames_to_ticks, CHANNELS, SAMPLE_RATE};

/// The engine holds this much for a thread that was held up.
const BUFFER_TICKS: i64 = TICKS_PER_SEC / 5;
/// A packet stamp further from now than this cannot be right.
const PLAUSIBLE_STAMP_TICKS: i64 = 5 * TICKS_PER_SEC;
const IDLE_WAIT: Duration = Duration::from_millis(5);
/// After the output device goes away Windows needs a moment to switch to the next one.
const REOPEN_INTERVAL_TICKS: i64 = TICKS_PER_SEC / 2;
const REOPEN_GIVE_UP_TICKS: i64 = 3 * TICKS_PER_SEC;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Endpoint {
    Loopback,
    Microphone(MicDevice),
}

struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: a call on a live client. It also ends the microphone-in-use indicator.
        let _ = unsafe { self.client.Stop() };
    }
}

fn session_format() -> WAVEFORMATEX {
    let block = CHANNELS as u16 * 4;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE as u32,
        nAvgBytesPerSec: SAMPLE_RATE as u32 * u32::from(block),
        nBlockAlign: block,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

fn open_error(kind: AudioOpenKind, what: &str, e: &windows::core::Error) -> AudioOpenError {
    AudioOpenError { kind, detail: hr_error(what, e).to_string() }
}

fn device_for(endpoint: &Endpoint) -> std::result::Result<IMMDevice, AudioOpenError> {
    let exact = matches!(endpoint, Endpoint::Microphone(MicDevice::Exact(_)));
    let fail = |what: &str, e: windows::core::Error| open_error(AudioOpenKind::from_hresult(e.code().0, exact), what, &e);
    // SAFETY: plain object creation; this thread is in the MTA.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| fail("MMDeviceEnumerator", e))?;
    match endpoint {
        // SAFETY: a call on a live object.
        Endpoint::Loopback => unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.map_err(|e| fail("default output", e)),
        // SAFETY: as above.
        Endpoint::Microphone(MicDevice::Default) => {
            unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) }.map_err(|e| fail("default microphone", e))
        }
        Endpoint::Microphone(MicDevice::Exact(id)) => {
            // SAFETY: the id string lives during the call.
            let device = unsafe { enumerator.GetDevice(&HSTRING::from(id.as_str())) }.map_err(|e| fail("selected microphone", e))?;
            // SAFETY: a query on a live object.
            let state = unsafe { device.GetState() }.map_err(|e| fail("selected microphone state", e))?;
            if state != DEVICE_STATE_ACTIVE {
                return Err(AudioOpenError {
                    kind: AudioOpenKind::Overconstrained,
                    detail: format!("the selected microphone is in state {}", state.0),
                });
            }
            Ok(device)
        }
    }
}

fn open_stream(endpoint: &Endpoint) -> std::result::Result<Stream, AudioOpenError> {
    let exact = matches!(endpoint, Endpoint::Microphone(MicDevice::Exact(_)));
    let fail = |what: &str, e: windows::core::Error| open_error(AudioOpenKind::from_hresult(e.code().0, exact), what, &e);
    let device = device_for(endpoint)?;
    // SAFETY: a call on a live object; it returns its own reference.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| fail("Activate", e))?;
    let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if matches!(endpoint, Endpoint::Loopback) {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    let format = session_format();
    // SAFETY: the format is a local the call copies; periodicity 0 is required for shared mode.
    unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_TICKS, 0, &format, None) }
        .map_err(|e| fail("IAudioClient::Initialize", e))?;
    // SAFETY: the client is initialized.
    if unsafe { client.GetBufferSize() }.map_err(|e| fail("IAudioClient::GetBufferSize", e))? == 0 {
        return Err(AudioOpenError { kind: AudioOpenKind::Empty, detail: "the device offers no audio buffer".into() });
    }
    // SAFETY: the client is initialized.
    let capture: IAudioCaptureClient = unsafe { client.GetService() }.map_err(|e| fail("IAudioCaptureClient", e))?;
    // SAFETY: as above.
    unsafe { client.Start() }.map_err(|e| fail("IAudioClient::Start", e))?;
    Ok(Stream { client, capture })
}

pub(crate) struct WasapiInput {
    endpoint: Endpoint,
    stream: Option<Stream>,
    lost_since: i64,
    next_retry: i64,
    scratch: Vec<f32>,
}

// SAFETY: see the module comment of `win`; WASAPI objects are used from MTA threads, one at a time.
unsafe impl Send for WasapiInput {}

impl WasapiInput {
    pub fn open(endpoint: Endpoint) -> std::result::Result<Self, AudioOpenError> {
        let stream = open_stream(&endpoint)?;
        Ok(Self { endpoint, stream: Some(stream), lost_since: 0, next_retry: 0, scratch: Vec::new() })
    }

    /// The output device can change under a loopback stream: unplugged headphones, a new default.
    /// That is followed. A microphone is never replaced by another, so losing it is final.
    fn stream_failed(&mut self, e: &windows::core::Error) -> Result<()> {
        self.stream = None;
        if !matches!(self.endpoint, Endpoint::Loopback) {
            return Err(hr_error("capture stream", e));
        }
        let now = qpc_ticks();
        self.lost_since = now;
        self.next_retry = now + REOPEN_INTERVAL_TICKS;
        Ok(())
    }

    fn try_reopen(&mut self) -> Result<()> {
        let now = qpc_ticks();
        if now >= self.next_retry {
            self.next_retry = now + REOPEN_INTERVAL_TICKS;
            match open_stream(&self.endpoint) {
                Ok(stream) => self.stream = Some(stream),
                Err(e) if now - self.lost_since > REOPEN_GIVE_UP_TICKS => {
                    return Err(Error::msg(format!("no output device to follow: {}", e.detail)));
                }
                Err(_) => {}
            }
        }
        thread::sleep(IDLE_WAIT);
        Ok(())
    }
}

impl AudioInput for WasapiInput {
    fn read(&mut self, deliver: &mut dyn FnMut(AudioPacket<'_>)) -> Result<()> {
        let Some(stream) = &self.stream else {
            return self.try_reopen();
        };
        let capture = stream.capture.clone();
        let mut delivered = false;
        loop {
            // SAFETY: a query on a live object.
            let pending = match unsafe { capture.GetNextPacketSize() } {
                Ok(pending) => pending,
                Err(e) => return self.stream_failed(&e),
            };
            if pending == 0 {
                break;
            }
            let (mut data, mut frames, mut flags, mut stamp) = (std::ptr::null_mut(), 0_u32, 0_u32, 0_u64);
            // SAFETY: the out pointers are locals; the buffer stays valid until ReleaseBuffer.
            if let Err(e) = unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut stamp)) } {
                return self.stream_failed(&e);
            }
            let samples = frames as usize * CHANNELS;
            self.scratch.clear();
            self.scratch.resize(samples, 0.0);
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 == 0 {
                // SAFETY: GetBuffer gave `frames` frames of the requested format, 32-bit floats. The
                // copy goes byte by byte, so the buffer's alignment does not matter.
                unsafe {
                    std::ptr::copy_nonoverlapping(data.cast_const(), self.scratch.as_mut_ptr().cast::<u8>(), samples * 4);
                }
            }
            let now = qpc_ticks();
            let stamped = stamp as i64;
            let duration = frames_to_ticks(i64::from(frames));
            let valid = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0
                && (stamped - now).abs() < PLAUSIBLE_STAMP_TICKS;
            let at = if valid { stamped } else { now - duration };
            deliver(AudioPacket { at, samples: &self.scratch });
            delivered = true;
            // SAFETY: returns exactly what GetBuffer handed out.
            if let Err(e) = unsafe { capture.ReleaseBuffer(frames) } {
                return self.stream_failed(&e);
            }
        }
        if !delivered {
            thread::sleep(IDLE_WAIT);
        }
        Ok(())
    }
}
