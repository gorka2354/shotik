//! The Media Foundation Sink Writer: H.264 and AAC into MP4. The sink writer encodes by itself; what
//! is chosen here is whether it may use hardware, and with which GPU device.

use std::any::Any;
use std::path::Path;

use shotik_core::{Error, Result};
use windows::core::{Interface, GUID, HSTRING};

use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_PeakConstrainedVBR, eAVEncH264VProfile_High, ICodecAPI, IMFActivate, IMFAttributes, IMFByteStream,
    IMFDXGIDeviceManager, IMFMediaType, IMFSample, IMFSinkWriter, IMFSinkWriterEx, IMFTransform, MFAudioFormat_AAC,
    MFAudioFormat_PCM, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL,
    MFMediaType_Audio, MFMediaType_Video, MFNominalRange_16_235, MFTEnumEx, MFTranscodeContainerType_MPEG4, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709, MFVideoTransFunc_709,
    MFVideoTransferMatrix_BT709, CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    MFT_CATEGORY_AUDIO_ENCODER, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_ENUM_HARDWARE_URL_Attribute, MFT_FRIENDLY_NAME_Attribute,
    MFT_REGISTER_TYPE_INFO, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES,
    MF_MT_YUV_MATRIX, MF_READWRITE_DISABLE_CONVERTERS, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SINK_WRITER_ALL_STREAMS,
    MF_SINK_WRITER_D3D_MANAGER, MF_SINK_WRITER_STATISTICS, MF_TRANSCODE_CONTAINERTYPE,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VARIANT, VT_UI4};

use super::com::{hr_error, Context};
use crate::backend::{MediaSink, SinkRequest};
use crate::mix::{CHANNELS, SAMPLE_RATE};
use crate::trace::Trace;

/// 128 kbit/s, as `audioBitsPerSecond` in `src/recorder/recorder.js`.
const AAC_BYTES_PER_SEC: u32 = 16_000;
const PCM_BYTES_PER_FRAME: u32 = CHANNELS as u32 * 2;
/// A keyframe every two seconds: seeking lands near the requested position.
const KEYFRAME_INTERVAL_SEC: u32 = 2;

/// An encoder input. MF objects are free threaded.
pub(crate) struct SendSample(pub IMFSample);

// SAFETY: see the module comment of `win`.
unsafe impl Send for SendSample {}

#[derive(Clone, Copy, Debug)]
pub(crate) struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

fn video_type(subtype: &GUID, format: &VideoFormat) -> windows::core::Result<IMFMediaType> {
    // SAFETY: only setters on a media type this function owns, with pointers to constants.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(format.width, format.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(format.fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        // what the conversion produces, so a player does not guess
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        Ok(t)
    }
}

pub(crate) fn nv12_type(format: &VideoFormat) -> windows::core::Result<IMFMediaType> {
    let t = video_type(&MFVideoFormat_NV12, format)?;
    // SAFETY: setters on a media type this function owns.
    unsafe {
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, format.width)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    }
    Ok(t)
}

fn h264_type(format: &VideoFormat) -> windows::core::Result<IMFMediaType> {
    let t = video_type(&MFVideoFormat_H264, format)?;
    // SAFETY: setters on a media type this function owns.
    unsafe {
        t.SetUINT32(&MF_MT_AVG_BITRATE, format.bitrate)?;
        t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
    }
    Ok(t)
}

fn audio_type(subtype: &GUID) -> windows::core::Result<IMFMediaType> {
    // SAFETY: setters on a media type this function owns, with pointers to constants.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE as u32)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS as u32)?;
        Ok(t)
    }
}

fn aac_type() -> windows::core::Result<IMFMediaType> {
    let t = audio_type(&MFAudioFormat_AAC)?;
    // SAFETY: a setter on a media type this function owns.
    unsafe { t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SEC)? };
    Ok(t)
}

fn pcm_type() -> windows::core::Result<IMFMediaType> {
    let t = audio_type(&MFAudioFormat_PCM)?;
    // SAFETY: setters on a media type this function owns.
    unsafe {
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, PCM_BYTES_PER_FRAME)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, SAMPLE_RATE as u32 * PCM_BYTES_PER_FRAME)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    }
    Ok(t)
}

fn attr_string(attributes: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut text = windows::core::PWSTR::null();
    let mut len = 0;
    // SAFETY: on success the string is allocated for us, copied here and freed right after.
    unsafe {
        attributes.GetAllocatedString(key, &mut text, &mut len).ok()?;
        let value = text.to_string().ok();
        CoTaskMemFree(Some(text.0.cast()));
        value
    }
}

/// The names of the installed H.264 encoders of one kind.
pub(crate) fn enumerate_h264_encoders(flags: MFT_ENUM_FLAG) -> Vec<String> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0;
    // SAFETY: the out pointers are locals, the type infos live during the call.
    let found = unsafe { MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, Some(&input), Some(&output), &mut list, &mut count) };
    if found.is_err() || list.is_null() {
        return Vec::new();
    }
    // SAFETY: MFTEnumEx returned an array of `count` references. Taking each element drops its
    // reference, and the array itself is CoTaskMem.
    unsafe {
        let names = std::slice::from_raw_parts_mut(list, count as usize)
            .iter_mut()
            .filter_map(|slot| slot.take())
            .map(|activate| attr_string(&activate, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_else(|| "unnamed".into()))
            .collect();
        CoTaskMemFree(Some(list.cast()));
        names
    }
}

pub(crate) fn hardware_h264_encoders() -> Vec<String> {
    enumerate_h264_encoders(MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0))
}

fn software_h264_encoders() -> Vec<String> {
    enumerate_h264_encoders(MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0))
}

fn encoder_of(writer: &IMFSinkWriter, stream: u32, category: &GUID) -> Option<(String, bool)> {
    let ex: IMFSinkWriterEx = writer.cast().ok()?;
    for index in 0..8 {
        let mut found = GUID::zeroed();
        let mut transform: Option<IMFTransform> = None;
        // SAFETY: the out pointers are locals; the call fails past the last transform.
        if unsafe { ex.GetTransformForStream(stream, index, Some(&mut found), &mut transform) }.is_err() {
            return None;
        }
        if found == *category {
            let transform = transform?;
            // SAFETY: a query on a live object. Not every software transform has attributes.
            let Ok(attributes) = (unsafe { transform.GetAttributes() }) else {
                return Some(("unnamed".into(), false));
            };
            let name = attr_string(&attributes, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_else(|| "unnamed".into());
            let hardware = attr_string(&attributes, &MFT_ENUM_HARDWARE_URL_Attribute).is_some();
            return Some((name, hardware));
        }
    }
    None
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

/// Rate and structure of the stream. An encoder may refuse a setting: that is not fatal, the file is
/// still valid, but it has to be visible in the trace.
fn tune_encoder(writer: &IMFSinkWriter, stream: u32, format: &VideoFormat, trace: &Trace) {
    let mut raw = std::ptr::null_mut();
    // SAFETY: asks the stream's encoder for an interface, the out pointer is a local.
    if let Err(e) = unsafe { writer.GetServiceForStream(stream, &GUID::zeroed(), &ICodecAPI::IID, &mut raw) } {
        trace.push(format!("encoder has no ICodecAPI: {e}"));
        return;
    }
    // SAFETY: GetServiceForStream succeeded and returned a reference of the requested interface.
    let api = unsafe { ICodecAPI::from_raw(raw) };
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

pub(crate) struct MfSink {
    writer: IMFSinkWriter,
    video_stream: Option<u32>,
    audio_stream: Option<u32>,
}

// SAFETY: see the module comment of `win`; the sink writer is free threaded.
unsafe impl Send for MfSink {}

fn create_writer(path: &Path, manager: Option<&IMFDXGIDeviceManager>) -> windows::core::Result<IMFSinkWriter> {
    // SAFETY: setters on attributes this function owns; the path string lives during the call.
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 4)?;
        let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
        attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        if let Some(manager) = manager {
            attributes.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attributes.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, manager)?;
        } else {
            // what goes in is what the encoders take: a converter on the way would be a hidden CPU
            // cost. The sink writer refuses this flag together with a device manager.
            attributes.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)?;
        }
        MFCreateSinkWriterFromURL(&HSTRING::from(path), None::<&IMFByteStream>, &attributes)
    }
}

impl MfSink {
    /// The hardware path. `Err` says why not, and the caller goes on to the software path.
    pub fn open_hardware(
        request: &SinkRequest,
        format: &VideoFormat,
        manager: &IMFDXGIDeviceManager,
        trace: &Trace,
    ) -> Result<Self> {
        Self::open(request, Some(format), Some(manager), trace)
    }

    pub fn open_software(request: &SinkRequest, format: &VideoFormat, trace: &Trace) -> Result<Self> {
        if software_h264_encoders().is_empty() {
            return Err(Error::msg(
                "No H.264 encoder on this system (a Windows N edition needs the Media Feature Pack)",
            ));
        }
        Self::open(request, Some(format), None, trace)
    }

    pub fn open_audio_only(request: &SinkRequest, trace: &Trace) -> Result<Self> {
        Self::open(request, None, None, trace)
    }

    fn open(
        request: &SinkRequest,
        format: Option<&VideoFormat>,
        manager: Option<&IMFDXGIDeviceManager>,
        trace: &Trace,
    ) -> Result<Self> {
        let writer = create_writer(&request.path, manager).ctx("Cannot create the recording file")?;
        let mut video_stream = None;
        if let Some(format) = format {
            let output = h264_type(format).ctx("H.264 media type")?;
            // SAFETY: the writer and the media types are live objects.
            let stream = unsafe { writer.AddStream(&output) }.ctx("Cannot add the video stream")?;
            let input = nv12_type(format).ctx("NV12 media type")?;
            // SAFETY: as above.
            unsafe { writer.SetInputMediaType(stream, &input, None) }.ctx("No H.264 encoder takes this format")?;
            video_stream = Some(stream);
            tune_encoder(&writer, stream, format, trace);
        }
        let mut audio_stream = None;
        if request.audio {
            let output = aac_type().ctx("AAC media type")?;
            // SAFETY: as above.
            let stream = unsafe { writer.AddStream(&output) }.ctx("Cannot add the audio stream")?;
            let input = pcm_type().ctx("PCM media type")?;
            // SAFETY: as above.
            unsafe { writer.SetInputMediaType(stream, &input, None) }
                .map_err(|e| hr_error("No AAC encoder on this system (a Windows N edition needs the Media Feature Pack)", &e))?;
            audio_stream = Some(stream);
        }
        // SAFETY: all streams are configured.
        unsafe { writer.BeginWriting() }.ctx("The encoder did not start")?;

        if let (Some(format), Some(stream)) = (format, video_stream) {
            let (mut name, hardware) = encoder_of(&writer, stream, &MFT_CATEGORY_VIDEO_ENCODER)
                .unwrap_or_else(|| ("unnamed".into(), false));
            if name == "unnamed" && !hardware {
                // Microsoft's encoder has no attributes to ask, the list has it
                name = software_h264_encoders().join(", ");
            }
            if manager.is_some() && !hardware {
                return Err(Error::msg(format!("the sink writer chose software encoder \"{name}\"")));
            }
            let kind = if hardware { "hw" } else { "sw" };
            trace.push(format!(
                "encoder h264 {kind} \"{name}\" {}x{} {} fps {} bit/s",
                format.width, format.height, format.fps, format.bitrate
            ));
        }
        if let Some(stream) = audio_stream {
            let name = encoder_of(&writer, stream, &MFT_CATEGORY_AUDIO_ENCODER).map_or_else(|| "unknown".into(), |(name, _)| name);
            trace.push(format!("encoder aac \"{name}\" {} bit/s", AAC_BYTES_PER_SEC * 8));
        }
        Ok(Self { writer, video_stream, audio_stream })
    }
}

impl MediaSink for MfSink {
    fn write_video(&mut self, frame: Box<dyn Any + Send>, pts: i64, duration: i64) -> Result<()> {
        let Ok(sample) = frame.downcast::<SendSample>() else {
            return Err(Error::msg("internal error: a frame in the wrong form reached the encoder"));
        };
        let Some(stream) = self.video_stream else {
            return Err(Error::msg("internal error: a frame without a video stream"));
        };
        // SAFETY: the sample and the writer are live; the writer takes its own reference.
        unsafe {
            sample.0.SetSampleTime(pts).ctx("SetSampleTime")?;
            sample.0.SetSampleDuration(duration).ctx("SetSampleDuration")?;
            self.writer.WriteSample(stream, &sample.0).ctx("The video encoder failed")
        }
    }

    fn write_audio(&mut self, pcm: &[i16], pts: i64, duration: i64) -> Result<()> {
        let Some(stream) = self.audio_stream else {
            return Err(Error::msg("internal error: audio without an audio stream"));
        };
        let bytes = pcm.len() * 2;
        // SAFETY: the buffer is created with room for `bytes` and locked around the copy; the writer
        // takes its own reference to the sample.
        unsafe {
            let buffer = MFCreateMemoryBuffer(bytes as u32).ctx("MFCreateMemoryBuffer")?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None).ctx("IMFMediaBuffer::Lock")?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr().cast::<u8>(), data, bytes);
            buffer.Unlock().ctx("IMFMediaBuffer::Unlock")?;
            buffer.SetCurrentLength(bytes as u32).ctx("SetCurrentLength")?;
            let sample = MFCreateSample().ctx("MFCreateSample")?;
            sample.AddBuffer(&buffer).ctx("AddBuffer")?;
            sample.SetSampleTime(pts).ctx("SetSampleTime")?;
            sample.SetSampleDuration(duration).ctx("SetSampleDuration")?;
            self.writer.WriteSample(stream, &sample).ctx("The audio encoder failed")
        }
    }

    fn bytes_written(&self) -> u64 {
        let mut stats = MF_SINK_WRITER_STATISTICS {
            cb: std::mem::size_of::<MF_SINK_WRITER_STATISTICS>() as u32,
            ..MF_SINK_WRITER_STATISTICS::default()
        };
        // SAFETY: the statistics struct is a local with its size filled in.
        match unsafe { self.writer.GetStatistics(MF_SINK_WRITER_ALL_STREAMS.0, &mut stats) } {
            Ok(()) => stats.qwByteCountProcessed,
            Err(_) => 0,
        }
    }

    fn finalize(self: Box<Self>) -> Result<()> {
        // SAFETY: no other thread writes any more: the writer thread has ended.
        unsafe { self.writer.Finalize() }.ctx("Cannot finish the recording file")
    }
}
