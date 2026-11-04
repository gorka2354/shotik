//! The Media Foundation Sink Writer: MP4 with AAC, and H.264 from either side. It encodes with the
//! software encoder by itself, and only muxes what the retained hardware encoder produces (see the
//! `encoder` module for why it does not run that one too).

use std::any::Any;
use std::path::Path;

use shotik_core::{Error, Result};
use windows::core::{Interface, GUID, HSTRING};
use windows::Win32::Media::MediaFoundation::{
    eAVEncH264VProfile_High, ICodecAPI, IMFActivate, IMFAttributes, IMFByteStream, IMFMediaType, IMFSample, IMFSinkWriter,
    IMFSinkWriterEx, IMFTransform, MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Audio, MFMediaType_Video, MFNominalRange_16_235, MFTEnumEx,
    MFTranscodeContainerType_MPEG4, MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709,
    MFVideoTransFunc_709, MFVideoTransferMatrix_BT709, MFT_CATEGORY_AUDIO_ENCODER, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG,
    MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_ENUM_HARDWARE_URL_Attribute,
    MFT_FRIENDLY_NAME_Attribute, MFT_REGISTER_TYPE_INFO, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE,
    MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX, MF_READWRITE_DISABLE_CONVERTERS, MF_SINK_WRITER_ALL_STREAMS, MF_SINK_WRITER_STATISTICS,
    MF_TRANSCODE_CONTAINERTYPE,
};
use windows::Win32::System::Com::CoTaskMemFree;

use super::com::{hr_error, Context};
use super::encoder::{tune_encoder, EncoderStream};
use crate::backend::{MediaSink, SinkRequest};
use crate::mix::{CHANNELS, SAMPLE_RATE};
use crate::trace::Trace;

/// 128 kbit/s, as `audioBitsPerSecond` in `src/recorder/recorder.js`.
const AAC_BYTES_PER_SEC: u32 = 16_000;
const PCM_BYTES_PER_FRAME: u32 = CHANNELS as u32 * 2;
/// A keyframe every two seconds: seeking lands near the requested position.
pub(crate) const KEYFRAME_INTERVAL_SEC: u32 = 2;

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

pub(crate) fn h264_type(format: &VideoFormat) -> windows::core::Result<IMFMediaType> {
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

pub(crate) fn attr_string(attributes: &IMFAttributes, key: &GUID) -> Option<String> {
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
fn enumerate_h264_encoders(flags: MFT_ENUM_FLAG) -> Vec<String> {
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

/// Rate and structure of the stream when the sink writer runs the encoder itself.
fn tune_internal_encoder(writer: &IMFSinkWriter, stream: u32, format: &VideoFormat, trace: &Trace) {
    let mut raw = std::ptr::null_mut();
    // SAFETY: asks the stream's encoder for an interface, the out pointer is a local.
    if let Err(e) = unsafe { writer.GetServiceForStream(stream, &GUID::zeroed(), &ICodecAPI::IID, &mut raw) } {
        trace.push(format!("encoder has no ICodecAPI: {e}"));
        return;
    }
    // SAFETY: GetServiceForStream succeeded and returned a reference of the requested interface.
    let api = unsafe { ICodecAPI::from_raw(raw) };
    tune_encoder(&api, format, trace);
}

pub(crate) struct MfSink {
    writer: IMFSinkWriter,
    video_stream: Option<u32>,
    audio_stream: Option<u32>,
    /// With a retained hardware encoder the sink writer only muxes.
    encoder: Option<EncoderStream>,
}

// SAFETY: see the module comment of `win`; the sink writer is free threaded.
unsafe impl Send for MfSink {}

fn create_writer(path: &Path) -> windows::core::Result<IMFSinkWriter> {
    // SAFETY: setters on attributes this function owns; the path string lives during the call.
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 2)?;
        let attributes = attributes.ok_or_else(windows::core::Error::empty)?;
        attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        // what goes in is what the encoders take: a converter on the way would be a hidden CPU cost
        attributes.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)?;
        MFCreateSinkWriterFromURL(&HSTRING::from(path), None::<&IMFByteStream>, &attributes)
    }
}

fn add_audio(writer: &IMFSinkWriter) -> Result<u32> {
    let output = aac_type().ctx("AAC media type")?;
    // SAFETY: the writer and the media types are live objects.
    let stream = unsafe { writer.AddStream(&output) }.ctx("Cannot add the audio stream")?;
    let input = pcm_type().ctx("PCM media type")?;
    // SAFETY: as above.
    unsafe { writer.SetInputMediaType(stream, &input, None) }
        .map_err(|e| hr_error("No AAC encoder on this system (a Windows N edition needs the Media Feature Pack)", &e))?;
    Ok(stream)
}

fn begin(writer: &IMFSinkWriter, audio_stream: Option<u32>, trace: &Trace) -> Result<()> {
    // SAFETY: all streams are configured.
    unsafe { writer.BeginWriting() }.ctx("The encoder did not start")?;
    if let Some(stream) = audio_stream {
        let name = encoder_of(writer, stream, &MFT_CATEGORY_AUDIO_ENCODER).map_or_else(|| "unnamed".into(), |(name, _)| name);
        trace.push(format!("encoder aac \"{name}\" {} bit/s", AAC_BYTES_PER_SEC * 8));
    }
    Ok(())
}

impl MfSink {
    /// H.264 from a retained hardware encoder, which hands over its stream to this file.
    pub fn with_encoder(request: &SinkRequest, format: &VideoFormat, stream: EncoderStream, trace: &Trace) -> Result<Self> {
        let writer = create_writer(&request.path).ctx("Cannot create the recording file")?;
        let compressed = stream.output_type()?;
        // SAFETY: the writer and the media type are live objects. The same type in and out: no encoder.
        let video_stream = unsafe {
            let video = writer.AddStream(&compressed).ctx("Cannot add the video stream")?;
            writer.SetInputMediaType(video, &compressed, None).ctx("The muxer does not take this stream")?;
            video
        };
        let audio_stream = if request.audio { Some(add_audio(&writer)?) } else { None };
        begin(&writer, audio_stream, trace)?;
        trace.push(format!(
            "encoder h264 hw \"{}\" {}x{} {} fps {} bit/s",
            stream.name(),
            format.width,
            format.height,
            format.fps,
            format.bitrate
        ));
        Ok(Self { writer, video_stream: Some(video_stream), audio_stream, encoder: Some(stream) })
    }

    /// H.264 from the first software encoder Media Foundation offers.
    pub fn software(request: &SinkRequest, format: &VideoFormat, trace: &Trace) -> Result<Self> {
        let encoders = software_h264_encoders();
        if encoders.is_empty() {
            return Err(Error::msg(
                "No H.264 encoder on this system (a Windows N edition needs the Media Feature Pack)",
            ));
        }
        let writer = create_writer(&request.path).ctx("Cannot create the recording file")?;
        let output = h264_type(format).ctx("H.264 media type")?;
        // SAFETY: the writer and the media types are live objects.
        let video_stream = unsafe { writer.AddStream(&output) }.ctx("Cannot add the video stream")?;
        let input = nv12_type(format).ctx("NV12 media type")?;
        // SAFETY: as above.
        unsafe { writer.SetInputMediaType(video_stream, &input, None) }.ctx("No H.264 encoder takes this format")?;
        tune_internal_encoder(&writer, video_stream, format, trace);
        let audio_stream = if request.audio { Some(add_audio(&writer)?) } else { None };
        begin(&writer, audio_stream, trace)?;
        // Microsoft's encoder has no attributes to ask for its name, the enumeration has it
        let name = match encoder_of(&writer, video_stream, &MFT_CATEGORY_VIDEO_ENCODER) {
            Some((name, _)) if name != "unnamed" => name,
            _ => encoders.join(", "),
        };
        trace.push(format!(
            "encoder h264 sw \"{name}\" {}x{} {} fps {} bit/s",
            format.width, format.height, format.fps, format.bitrate
        ));
        Ok(Self { writer, video_stream: Some(video_stream), audio_stream, encoder: None })
    }

    pub fn audio_only(request: &SinkRequest, trace: &Trace) -> Result<Self> {
        let writer = create_writer(&request.path).ctx("Cannot create the recording file")?;
        let audio_stream = Some(add_audio(&writer)?);
        begin(&writer, audio_stream, trace)?;
        Ok(Self { writer, video_stream: None, audio_stream, encoder: None })
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
        // SAFETY: the sample is live.
        unsafe {
            sample.0.SetSampleTime(pts).ctx("SetSampleTime")?;
            sample.0.SetSampleDuration(duration).ctx("SetSampleDuration")?;
        }
        let writer = &self.writer;
        match &mut self.encoder {
            Some(encoder) => encoder.encode(&sample.0, &mut |compressed| {
                // SAFETY: both are live; the writer takes its own reference.
                unsafe { writer.WriteSample(stream, compressed) }.ctx("The video muxer failed")
            }),
            // SAFETY: as above.
            None => unsafe { writer.WriteSample(stream, &sample.0) }.ctx("The video encoder failed"),
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
        let mut this = *self;
        if let (Some(encoder), Some(stream)) = (&mut this.encoder, this.video_stream) {
            let writer = &this.writer;
            encoder.drain(&mut |compressed| {
                // SAFETY: both are live; the writer takes its own reference.
                unsafe { writer.WriteSample(stream, compressed) }.ctx("The video muxer failed")
            })?;
        }
        // SAFETY: no other thread writes any more: the writer thread has ended.
        unsafe { this.writer.Finalize() }.ctx("Cannot finish the recording file")
    }
}
