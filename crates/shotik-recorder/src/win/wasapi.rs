//! WASAPI capture: loopback of the default output, or a microphone, in whatever format the device
//! mixes at. Shared mode accepts that format by definition, and `resample` turns it into the session's.

use std::thread;
use std::time::Duration;

use shotik_core::{Error, Result};
use windows::core::HSTRING;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

use super::com::{hr_error, qpc_ticks};
use crate::backend::{AudioInput, AudioOpenError, AudioOpenKind, AudioPacket, MicDevice};
use crate::clock::TICKS_PER_SEC;
use crate::resample::{Converter, DeviceFormat};

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
    format: DeviceFormat,
    converter: Converter,
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: a call on a live client. It also ends the microphone-in-use indicator.
        let _ = unsafe { self.client.Stop() };
    }
}

/// The format the device mixes at, as Windows reports it now: it can change between sessions.
fn mix_format(client: &IAudioClient) -> windows::core::Result<(*mut WAVEFORMATEX, std::result::Result<DeviceFormat, String>)> {
    // SAFETY: a query on a live client. The pointer is CoTaskMem and points to the 18 bytes of the
    // structure plus `cbSize` more, which is what is read here. The caller frees it.
    unsafe {
        let raw = client.GetMixFormat()?;
        let size = 18 + usize::from((*raw).cbSize);
        let bytes = std::slice::from_raw_parts(raw.cast::<u8>(), size);
        Ok((raw, DeviceFormat::parse(bytes)))
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
    let flags = if matches!(endpoint, Endpoint::Loopback) { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
    let (raw_format, format) = mix_format(&client).map_err(|e| fail("IAudioClient::GetMixFormat", e))?;
    let format = format.map_err(|detail| {
        // SAFETY: the pointer came from GetMixFormat and nothing uses it after this.
        unsafe { CoTaskMemFree(Some(raw_format.cast())) };
        AudioOpenError { kind: AudioOpenKind::NotReadable, detail: format!("the device mix format: {detail}") }
    })?;
    // SAFETY: the format came from this client and the call copies it; periodicity 0 is required for
    // shared mode. It is freed right after, whatever the outcome.
    let initialized = unsafe {
        let result = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_TICKS, 0, raw_format, None);
        CoTaskMemFree(Some(raw_format.cast()));
        result
    };
    initialized.map_err(|e| fail(&format!("IAudioClient::Initialize at {format}"), e))?;
    // SAFETY: the client is initialized.
    if unsafe { client.GetBufferSize() }.map_err(|e| fail("IAudioClient::GetBufferSize", e))? == 0 {
        return Err(AudioOpenError { kind: AudioOpenKind::Empty, detail: "the device offers no audio buffer".into() });
    }
    // SAFETY: the client is initialized.
    let capture: IAudioCaptureClient = unsafe { client.GetService() }.map_err(|e| fail("IAudioCaptureClient", e))?;
    // SAFETY: as above.
    unsafe { client.Start() }.map_err(|e| fail("IAudioClient::Start", e))?;
    Ok(Stream { client, capture, format, converter: Converter::new(format) })
}

pub(crate) struct WasapiInput {
    endpoint: Endpoint,
    stream: Option<Stream>,
    lost_since: i64,
    next_retry: i64,
}

// SAFETY: see the module comment of `win`; WASAPI objects are used from MTA threads, one at a time.
unsafe impl Send for WasapiInput {}

impl WasapiInput {
    pub fn open(endpoint: Endpoint) -> std::result::Result<Self, AudioOpenError> {
        let stream = open_stream(&endpoint)?;
        Ok(Self { endpoint, stream: Some(stream), lost_since: 0, next_retry: 0 })
    }

    /// The format the device delivers, for the trace.
    pub fn device_format(&self) -> Option<DeviceFormat> {
        self.stream.as_ref().map(|stream| stream.format)
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
        let Some(stream) = &mut self.stream else {
            return self.try_reopen();
        };
        let capture = stream.capture.clone();
        let block = usize::from(stream.format.block_align);
        let rate = i64::from(stream.format.rate);
        let mut delivered = false;
        let mut failure = None;
        loop {
            // SAFETY: a query on a live object.
            let pending = match unsafe { capture.GetNextPacketSize() } {
                Ok(pending) => pending,
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            };
            if pending == 0 {
                break;
            }
            let (mut data, mut frames, mut flags, mut stamp) = (std::ptr::null_mut(), 0_u32, 0_u32, 0_u64);
            // SAFETY: the out pointers are locals; the buffer stays valid until ReleaseBuffer.
            if let Err(e) = unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut stamp)) } {
                failure = Some(e);
                break;
            }
            let now = qpc_ticks();
            let stamped = stamp as i64;
            let duration = i64::from(frames) * TICKS_PER_SEC / rate;
            let valid = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 && (stamped - now).abs() < PLAUSIBLE_STAMP_TICKS;
            let at = if valid { stamped } else { now - duration };
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            // SAFETY: GetBuffer gave `frames` frames of `block` bytes, readable until ReleaseBuffer below.
            let bytes = (!silent).then(|| unsafe { std::slice::from_raw_parts(data.cast_const(), frames as usize * block) });
            stream.converter.push(bytes, frames as usize, at, &mut |at, samples| deliver(AudioPacket { at, samples }));
            delivered = true;
            // SAFETY: returns exactly what GetBuffer handed out.
            if let Err(e) = unsafe { capture.ReleaseBuffer(frames) } {
                failure = Some(e);
                break;
            }
        }
        if let Some(e) = failure {
            return self.stream_failed(&e);
        }
        if !delivered {
            thread::sleep(IDLE_WAIT);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MicDevice;
    use crate::resample::Encoding;
    use std::f64::consts::TAU;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;
    use windows::Win32::Media::Audio::IAudioRenderClient;

    use super::super::com::Apartment;

    /// -90 dBFS: under the threshold of hearing at any volume a person can stand.
    const INAUDIBLE: f64 = 3.16e-5;

    fn amplitude(left: &[f64], hz: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, s) in left.iter().enumerate() {
            let angle = TAU * hz * i as f64 / 48_000.0;
            re += s * angle.cos();
            im += s * angle.sin();
        }
        2.0 * f64::hypot(re, im) / left.len() as f64
    }

    /// Plays a tone in the device's own mix format, on a thread of its own.
    fn render_tone(hz: f64, level: f64, stop: &AtomicBool) {
        let _apartment = Apartment::enter();
        let device = device_for(&Endpoint::Loopback).expect("the default output");
        // SAFETY: calls on live objects; the buffer from GetBuffer is written within its size and
        // handed back at once. The format pointer is freed after Initialize copied it.
        unsafe {
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).unwrap();
            let (raw, format) = mix_format(&client).unwrap();
            let format = format.unwrap();
            assert_eq!(format.encoding, Encoding::F32, "this check is written for float output: {format}");
            let initialized = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, BUFFER_TICKS, 0, raw, None);
            CoTaskMemFree(Some(raw.cast()));
            initialized.unwrap();
            let size = client.GetBufferSize().unwrap();
            let render: IAudioRenderClient = client.GetService().unwrap();
            client.Start().unwrap();
            let mut index = 0_u64;
            let channels = usize::from(format.channels);
            while !stop.load(Ordering::SeqCst) {
                let free = size - client.GetCurrentPadding().unwrap();
                if free > 0 {
                    let data = render.GetBuffer(free).unwrap().cast::<f32>();
                    for frame in 0..free as usize {
                        let value = (level * (TAU * hz * index as f64 / f64::from(format.rate)).sin()) as f32;
                        index += 1;
                        for channel in 0..channels {
                            data.add(frame * channels + channel).write_unaligned(value);
                        }
                    }
                    render.ReleaseBuffer(free, 0).unwrap();
                }
                thread::sleep(Duration::from_millis(5));
            }
            client.Stop().unwrap();
        }
    }

    /// The real default output, in the format it has right now, with a real signal on it: nothing
    /// else can show that the format was read correctly. The tone is far under what anyone can hear
    /// and nothing goes to disk.
    #[test]
    #[ignore = "plays a 440 Hz tone at -90 dBFS on the default output for three seconds"]
    fn a_tone_on_the_real_output_keeps_its_pitch_and_level() {
        let _apartment = Apartment::enter();
        let mut input = WasapiInput::open(Endpoint::Loopback).expect("the default output opens for loopback");
        let format = input.device_format().unwrap();
        println!("default output: {format}");
        let stop = AtomicBool::new(false);
        let mut left: Vec<f64> = Vec::new();
        let mut stamps: Vec<(i64, usize)> = Vec::new();
        thread::scope(|scope| {
            scope.spawn(|| render_tone(440.0, INAUDIBLE, &stop));
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(3) {
                input
                    .read(&mut |packet| {
                        stamps.push((packet.at, packet.samples.len() / 2));
                        left.extend(packet.samples.iter().step_by(2).map(|s| f64::from(*s)));
                    })
                    .unwrap();
            }
            stop.store(true, Ordering::SeqCst);
        });
        assert!(left.len() > 48_000, "{} frames came through", left.len());
        // the start and end of the tone are not part of the measurement
        let middle = &left[left.len() / 4..left.len() * 3 / 4];
        let level = amplitude(middle, 440.0);
        let pitch = (-20..=20)
            .map(|i| 440.0 + f64::from(i) * 0.05)
            .max_by(|a, b| amplitude(middle, *a).total_cmp(&amplitude(middle, *b)))
            .unwrap();
        // the frame count against the clock of the stamps: a wrong rate shows here
        let (first, last) = (stamps[0], stamps[stamps.len() - 1]);
        let frames: usize = stamps[..stamps.len() - 1].iter().map(|(_, n)| n).sum();
        let rate = frames as f64 / ((last.0 - first.0) as f64 / TICKS_PER_SEC as f64);
        println!(
            "{} frames, measured rate {rate:.1} Hz, 440 Hz tone: pitch {pitch:.2} Hz, level {level:.3e} for {INAUDIBLE:.3e} ({:+.2} dB)",
            left.len(),
            20.0 * (level / INAUDIBLE).log10()
        );
        assert!((rate - 48_000.0).abs() < 20.0, "{rate}");
        assert!((pitch - 440.0).abs() < 0.3, "pitch {pitch}");
        assert!((level / INAUDIBLE - 1.0).abs() < 0.2, "level {level}");
    }

    #[test]
    fn a_device_that_does_not_exist_is_said_to_be_gone() {
        let _apartment = Apartment::enter();
        let error = WasapiInput::open(Endpoint::Microphone(MicDevice::Exact("no-such-device".into()))).err().expect("an error");
        assert_eq!(error.kind, AudioOpenKind::Overconstrained, "{error:?}");
        assert!(!error.detail.is_empty());
    }
}
