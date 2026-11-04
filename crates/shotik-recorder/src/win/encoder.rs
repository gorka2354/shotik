//! The hardware H.264 encoder, driven directly and kept for the life of the process.
//!
//! The Sink Writer can run an encoder by itself, and the first version did, but it activates a new
//! one for every file. On this machine's driver each activation of the NVIDIA encoder leaves 1 Section,
//! 1 Mutant and 4 Events behind, and shutting Media Foundation down after one leaves two thread pools,
//! about 40 handles per recording in all. One activated encoder that only starts and stops streaming
//! stays flat, also when the frame size changes (measured with a census by handle type). So the encoder
//! is activated once per adapter, returned to idle after each recording, and the Sink Writer only muxes
//! what it produces. The price, measured: the NVIDIA encoder keeps one GPU encoding session open until
//! the process ends, with the device about 25 MiB of GPU memory.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use shotik_core::{Error, Result};
use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_PeakConstrainedVBR, ICodecAPI, IMFActivate, IMFAttributes, IMFMediaEventGenerator, IMFMediaType,
    IMFSample, IMFTransform, MEError, METransformDrainComplete, METransformHaveOutput, METransformNeedInput, MFCreateAttributes,
    MFMediaType_Video, MFStartup, MFTEnum2, MFVideoFormat_H264, MFVideoFormat_NV12, CODECAPI_AVEncCommonMaxBitRate,
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID,
    MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT, MF_E_NOTACCEPTING, MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VARIANT, VT_UI4};

use super::com::{hr_error, Context};
use super::d3d::D3d;
use super::sink::{attr_string, h264_type, nv12_type, VideoFormat, KEYFRAME_INTERVAL_SEC};
use crate::trace::Trace;

/// What the encoder calls back with for each compressed frame.
pub(crate) type OnOutput<'a> = &'a mut dyn FnMut(&IMFSample) -> Result<()>;

/// Media Foundation stays up once a hardware encoder exists: shutting it down under one leaves two
/// thread pools behind per cycle, and the encoder must outlive any session anyway. Costs nothing in a
/// process that never records video.
fn keep_media_foundation() -> Result<()> {
    static KEPT: OnceLock<windows::core::Result<()>> = OnceLock::new();
    // SAFETY: an extra reference that is deliberately never given back.
    let started = KEPT.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) });
    started.clone().ctx("Media Foundation is not available on this system")
}

/// One activated encoder on one adapter. See the module comment.
pub(crate) struct HardwareEncoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    _activate: IMFActivate,
    pub name: String,
    /// A session that timed out may still be using it.
    in_use: AtomicBool,
    /// It failed in a way that it cannot be reused after.
    broken: AtomicBool,
}

// SAFETY: see the module comment of `win`. Asynchronous hardware transforms are free threaded by
// contract, and `in_use` gives one session at a time.
unsafe impl Send for HardwareEncoder {}
// SAFETY: as above.
unsafe impl Sync for HardwareEncoder {}

impl HardwareEncoder {
    pub fn is_usable(&self) -> bool {
        !self.broken.load(Ordering::SeqCst)
    }
}

fn u32_variant(value: u32) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: fills the discriminant and the matching member of a zeroed union. VT_UI4 owns nothing,
    // so no VariantClear is due.
    unsafe {
        (*variant.Anonymous.Anonymous).vt = VT_UI4;
        (*variant.Anonymous.Anonymous).Anonymous.ulVal = value;
    }
    variant
}

/// Rate and structure of the stream, on any encoder that has `ICodecAPI`. An encoder may refuse a
/// setting: that is not fatal, the file is still valid, but it has to be visible in the trace.
pub(crate) fn tune_encoder(api: &ICodecAPI, format: &VideoFormat, trace: &Trace) {
    let settings = [
        ("rate control", CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32),
        ("mean bitrate", CODECAPI_AVEncCommonMeanBitRate, format.bitrate),
        ("max bitrate", CODECAPI_AVEncCommonMaxBitRate, format.bitrate.saturating_mul(2)),
        ("gop", CODECAPI_AVEncMPVGOPSize, format.fps * KEYFRAME_INTERVAL_SEC),
        ("b-frames", CODECAPI_AVEncMPVDefaultBPictureCount, 0),
    ];
    for (name, key, value) in settings {
        let variant = u32_variant(value);
        // SAFETY: the key is a constant and the variant a local, valid during the call.
        if let Err(e) = unsafe { api.SetValue(&key, &variant) } {
            trace.push(format!("encoder refused {name}={value}: {:#010X}", e.code().0 as u32));
        }
    }
}

/// Activates the first encoder Media Foundation offers for this adapter.
fn activate_for(d3d: &D3d, trace: &Trace) -> Result<HardwareEncoder> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    let mut attributes: Option<IMFAttributes> = None;
    // SAFETY: the out pointer is a local.
    unsafe { MFCreateAttributes(&mut attributes, 1) }.ctx("MFCreateAttributes")?;
    let attributes = attributes.ok_or_else(|| Error::msg("MFCreateAttributes returned nothing"))?;
    // SAFETY: a setter copying the bytes of a local.
    unsafe { attributes.SetBlob(&MFT_ENUM_ADAPTER_LUID, &d3d.luid_bytes()) }.ctx("MFT_ENUM_ADAPTER_LUID")?;

    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0;
    // SAFETY: the out pointers are locals, the type infos live during the call.
    unsafe { MFTEnum2(MFT_CATEGORY_VIDEO_ENCODER, flags, Some(&input), Some(&output), &attributes, &mut list, &mut count) }
        .ctx("MFTEnum2")?;
    let activates: Vec<IMFActivate> = if list.is_null() {
        Vec::new()
    } else {
        // SAFETY: MFTEnum2 returned an array of `count` references, which the elements taken here now
        // own, and the array itself is CoTaskMem.
        unsafe {
            let taken = std::slice::from_raw_parts_mut(list, count as usize).iter_mut().filter_map(|slot| slot.take()).collect();
            CoTaskMemFree(Some(list.cast()));
            taken
        }
    };
    if activates.is_empty() {
        return Err(Error::msg("no hardware H.264 encoder on this adapter"));
    }
    let names: Vec<String> = activates.iter().map(|a| attr_string(a, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_else(|| "unnamed".into())).collect();
    trace.push(format!("hardware encoders of this adapter: {}", names.join(", ")));

    let mut last_error = None;
    for (activate, name) in activates.into_iter().zip(names) {
        match open_activate(&activate, d3d) {
            Ok((transform, events)) => {
                return Ok(HardwareEncoder {
                    transform,
                    events,
                    _activate: activate,
                    name,
                    in_use: AtomicBool::new(false),
                    broken: AtomicBool::new(false),
                });
            }
            Err(e) => {
                trace.push(format!("encoder \"{name}\" did not open: {e}"));
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| Error::msg("no hardware H.264 encoder could be opened")))
}

fn open_activate(activate: &IMFActivate, d3d: &D3d) -> Result<(IMFTransform, IMFMediaEventGenerator)> {
    // SAFETY: a call on a live object, returning its own reference.
    let transform: IMFTransform = unsafe { activate.ActivateObject() }.ctx("ActivateObject")?;
    // SAFETY: as above.
    let attributes = unsafe { transform.GetAttributes() }.ctx("GetAttributes")?;
    // SAFETY: queries and setters on a live attribute store.
    unsafe {
        if attributes.GetUINT32(&MF_TRANSFORM_ASYNC).ctx("not an asynchronous transform")? == 0 {
            return Err(Error::msg("not an asynchronous transform"));
        }
        attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).ctx("MF_TRANSFORM_ASYNC_UNLOCK")?;
        if attributes.GetUINT32(&MF_SA_D3D11_AWARE).ctx("does not take Direct3D 11 surfaces")? == 0 {
            return Err(Error::msg("does not take Direct3D 11 surfaces"));
        }
    }
    let manager = d3d.device_manager()?;
    // SAFETY: the message takes the manager as a pointer value, and the encoder takes its own reference.
    unsafe { transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize) }.ctx("MFT_MESSAGE_SET_D3D_MANAGER")?;
    let events: IMFMediaEventGenerator = transform.cast().ctx("IMFMediaEventGenerator")?;
    Ok((transform, events))
}

/// The encoder of this device, activated the first time it is needed.
pub(crate) fn retained_encoder(d3d: &D3d, trace: &Trace) -> Result<Arc<HardwareEncoder>> {
    keep_media_foundation()?;
    let mut slot = d3d.encoder.lock().unwrap();
    if let Some(encoder) = slot.as_ref() {
        if encoder.is_usable() {
            return Ok(Arc::clone(encoder));
        }
        trace.push(format!("encoder \"{}\" failed earlier and is replaced", encoder.name));
    }
    let encoder = Arc::new(activate_for(d3d, trace)?);
    *slot = Some(Arc::clone(&encoder));
    Ok(encoder)
}

/// A stream through the retained encoder, from setting its types to returning it to idle.
pub(crate) struct EncoderStream {
    encoder: Arc<HardwareEncoder>,
    need_input: u32,
    streaming: bool,
    /// The next frame must be a keyframe: an encoder that was used before does not start with one by
    /// itself, its picture count just goes on, and everything up to the next keyframe is undecodable.
    at_start: bool,
}

// SAFETY: see HardwareEncoder.
unsafe impl Send for EncoderStream {}

impl EncoderStream {
    /// Sets the format and starts streaming. An encoder that takes no such format, as NVENC with 64x64,
    /// says so here, and the caller goes on to the software encoder.
    pub fn begin(encoder: Arc<HardwareEncoder>, format: &VideoFormat, trace: &Trace) -> Result<Self> {
        if encoder.in_use.swap(true, Ordering::SeqCst) {
            return Err(Error::msg("the hardware encoder is still busy with an earlier recording"));
        }
        // from here on Drop gives it back
        let mut stream = Self { encoder, need_input: 0, streaming: false, at_start: true };
        stream.discard_events();
        if let Ok(api) = stream.encoder.transform.cast::<ICodecAPI>() {
            tune_encoder(&api, format, trace);
        } else {
            trace.push("encoder has no ICodecAPI");
        }
        let output = h264_type(format).ctx("H.264 media type")?;
        let input = nv12_type(format).ctx("NV12 media type")?;
        // SAFETY: calls on a live transform with media types it copies from. Output first, the order
        // encoders require.
        let accepted = unsafe {
            stream.encoder.transform.SetOutputType(0, &output, 0).and_then(|()| stream.encoder.transform.SetInputType(0, &input, 0))
        };
        if let Err(e) = accepted {
            // NVENC refuses everything after it refused once, also sizes it took before
            stream.encoder.broken.store(true, Ordering::SeqCst);
            return Err(hr_error("The encoder refuses this format", &e));
        }
        stream.start_streaming()?;
        Ok(stream)
    }

    fn start_streaming(&mut self) -> Result<()> {
        // SAFETY: plain messages on a live transform whose types are set.
        unsafe {
            self.encoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).ctx("MFT_MESSAGE_NOTIFY_BEGIN_STREAMING")?;
            self.streaming = true;
            self.encoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).ctx("MFT_MESSAGE_NOTIFY_START_OF_STREAM")?;
        }
        Ok(())
    }

    /// The stream's format with what the encoder added to it, the parameter sets above all.
    pub fn output_type(&self) -> Result<IMFMediaType> {
        // SAFETY: a call on a live transform whose output type is set.
        unsafe { self.encoder.transform.GetOutputCurrentType(0) }.ctx("GetOutputCurrentType")
    }

    pub fn name(&self) -> &str {
        &self.encoder.name
    }

    fn discard_events(&self) {
        // SAFETY: a call on a live generator; without waiting it fails at once when empty.
        while unsafe { self.encoder.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) }.is_ok() {}
    }

    /// Handles what the encoder has queued. Returns true when it reported the end of a drain.
    fn pump(&mut self, wait: bool, on_output: OnOutput<'_>) -> Result<bool> {
        let mut flags = if wait { MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0) } else { MF_EVENT_FLAG_NO_WAIT };
        loop {
            // SAFETY: a call on a live generator. Waiting is bounded by the finalize timeout one level up.
            let event = match unsafe { self.encoder.events.GetEvent(flags) } {
                Ok(event) => event,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(false),
                Err(e) => return Err(hr_error("The video encoder failed", &e)),
            };
            flags = MF_EVENT_FLAG_NO_WAIT;
            // SAFETY: a query on a live event.
            let kind = unsafe { event.GetType() }.ctx("IMFMediaEvent::GetType")? as i32;
            if kind == METransformNeedInput.0 {
                self.need_input += 1;
            } else if kind == METransformHaveOutput.0 {
                self.take_output(on_output)?;
            } else if kind == METransformDrainComplete.0 {
                return Ok(true);
            } else if kind == MEError.0 {
                // SAFETY: a query on a live event.
                let status = unsafe { event.GetStatus() }.ctx("IMFMediaEvent::GetStatus")?;
                self.encoder.broken.store(true, Ordering::SeqCst);
                return Err(hr_error("The video encoder failed", &status.ok().err().unwrap_or_else(windows::core::Error::empty)));
            }
        }
    }

    fn take_output(&mut self, on_output: OnOutput<'_>) -> Result<()> {
        let mut buffers = [MFT_OUTPUT_DATA_BUFFER::default()];
        let mut status = 0;
        // SAFETY: the encoder provides the sample; the struct and status are locals.
        let result = unsafe { self.encoder.transform.ProcessOutput(0, &mut buffers, &mut status) };
        // SAFETY: ProcessOutput gave these references to us and nothing else drops them.
        let (sample, events) = unsafe {
            (std::mem::ManuallyDrop::take(&mut buffers[0].pSample), std::mem::ManuallyDrop::take(&mut buffers[0].pEvents))
        };
        drop(events);
        match result {
            Ok(()) => match sample {
                Some(sample) => on_output(&sample),
                None => Ok(()),
            },
            // a late notice from the stream before this one
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(()),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // SAFETY: calls on a live transform; the type comes from itself.
                unsafe {
                    let changed = self.encoder.transform.GetOutputAvailableType(0, 0).ctx("GetOutputAvailableType")?;
                    self.encoder.transform.SetOutputType(0, &changed, 0).ctx("SetOutputType")
                }
            }
            Err(e) => Err(hr_error("The video encoder failed", &e)),
        }
    }

    /// Hands a frame over, stamped by the caller. Compressed frames come back through `on_output`,
    /// not necessarily during this call.
    pub fn encode(&mut self, sample: &IMFSample, on_output: OnOutput<'_>) -> Result<()> {
        if std::mem::take(&mut self.at_start) {
            let api: ICodecAPI = self.encoder.transform.cast().ctx("ICodecAPI")?;
            // SAFETY: the key is a constant and the variant a local, valid during the call.
            unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &u32_variant(1)) }.ctx("The encoder cannot start with a keyframe")?;
        }
        self.pump(false, on_output)?;
        loop {
            while self.need_input == 0 {
                self.pump(true, on_output)?;
            }
            // SAFETY: a live sample; the encoder takes its own reference.
            match unsafe { self.encoder.transform.ProcessInput(0, sample, 0) } {
                Ok(()) => {
                    self.need_input -= 1;
                    break;
                }
                // a late notice from the stream before this one counted for nothing
                Err(e) if e.code() == MF_E_NOTACCEPTING => self.need_input = 0,
                Err(e) => return Err(hr_error("The video encoder failed", &e)),
            }
        }
        self.pump(false, on_output)?;
        Ok(())
    }

    /// Gets everything still inside the encoder out.
    pub fn drain(&mut self, on_output: OnOutput<'_>) -> Result<()> {
        // SAFETY: plain messages on a live transform.
        unsafe {
            self.encoder.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0).ctx("MFT_MESSAGE_NOTIFY_END_OF_STREAM")?;
            self.encoder.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0).ctx("MFT_MESSAGE_COMMAND_DRAIN")?;
        }
        while !self.pump(true, on_output)? {}
        Ok(())
    }
}

impl Drop for EncoderStream {
    /// Back to idle, without types. NVENC keeps its session open regardless of what it is told here.
    fn drop(&mut self) {
        let transform = &self.encoder.transform;
        // SAFETY: plain messages and setters on a live transform.
        let idle = unsafe {
            let mut result = Ok(());
            if self.streaming {
                result = transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
                result = result.and(transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0));
            }
            result = result.and(transform.SetInputType(0, None::<&IMFMediaType>, 0));
            result.and(transform.SetOutputType(0, None::<&IMFMediaType>, 0))
        };
        self.discard_events();
        if idle.is_err() {
            self.encoder.broken.store(true, Ordering::SeqCst);
        }
        self.encoder.in_use.store(false, Ordering::SeqCst);
    }
}
