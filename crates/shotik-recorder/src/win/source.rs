//! From a picture source to encoder input. With a hardware encoder the region stays on the GPU:
//! copied out of the capture texture, converted to NV12 by the video processor. The software encoder
//! gets the same picture through system memory.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use shotik_core::{Error, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_FLAG, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSample, IMFVideoSampleAllocatorEx, MFCreateAttributes,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateVideoSampleAllocatorEx, MF_E_SAMPLEALLOCATOR_EMPTY, MF_SA_D3D11_BINDFLAGS,
    MF_SA_D3D11_USAGE,
};

use super::com::{qpc_ticks, Context};
use super::d3d::{VideoConverter, D3d};
use super::sink::{hardware_h264_encoders, nv12_type, MfSink, SendSample, VideoFormat};
use crate::backend::{MediaSink, SinkRequest, Snapshot, VideoFrame, VideoSource};
use crate::clock::TICKS_PER_SEC;
use crate::convert::{bgra_to_nv12, nv12_len};
use crate::region::video_bitrate;
use crate::synth::Scene;
use crate::trace::Trace;

/// A monitor that delivers no first frame this long means something is wrong, not slow.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// The encoder holds frames while the queue to it is full: the pool must outlast both.
const SURFACES_INITIAL: u32 = 4;
const SURFACES_MAX: u32 = 24;
const ENCODER_STALL_TIMEOUT: Duration = Duration::from_secs(5);
const TRIAL_FRAMES: i64 = 3;

/// Where the pictures come from.
pub(crate) trait Grabber: Send {
    /// A first picture exists.
    fn ready(&self) -> bool;
    /// Puts the newest picture into `latest`. False when there was nothing new.
    fn refresh_gpu(&mut self, d3d: &D3d, latest: &ID3D11Texture2D) -> Result<bool>;
    /// Puts the newest picture into `bgra`, rows of `stride` bytes. False when there was nothing new.
    fn refresh_cpu(&mut self, d3d: Option<&D3d>, bgra: &mut [u8], stride: usize) -> Result<bool>;
    fn lost(&self) -> Option<String>;
}

struct GpuPath {
    latest: ID3D11Texture2D,
    converter: VideoConverter,
    allocator: IMFVideoSampleAllocatorEx,
    _manager: IMFDXGIDeviceManager,
}

struct CpuPath {
    bgra: Vec<u8>,
    nv12: Vec<u8>,
}

enum Path {
    Unbound,
    Gpu(GpuPath),
    Cpu(CpuPath),
}

pub(crate) struct PipelineSource {
    grabber: Box<dyn Grabber>,
    d3d: Option<Arc<D3d>>,
    format: VideoFormat,
    path: Path,
}

// SAFETY: see the module comment of `win`; the session hands it from thread to thread, never shares it.
unsafe impl Send for PipelineSource {}

fn surface_allocator(manager: &IMFDXGIDeviceManager, format: &VideoFormat) -> Result<IMFVideoSampleAllocatorEx> {
    let mut raw = std::ptr::null_mut();
    // SAFETY: the out pointer is a local; on success it holds a reference of the requested interface.
    let allocator = unsafe {
        MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw).ctx("MFCreateVideoSampleAllocatorEx")?;
        IMFVideoSampleAllocatorEx::from_raw(raw)
    };
    let mut attributes: Option<IMFAttributes> = None;
    // SAFETY: the out pointer is a local, the rest are setters on live objects.
    unsafe {
        MFCreateAttributes(&mut attributes, 2).ctx("MFCreateAttributes")?;
        let attributes = attributes.ok_or_else(|| Error::msg("MFCreateAttributes returned nothing"))?;
        attributes.SetUINT32(&MF_SA_D3D11_BINDFLAGS, D3D11_BIND_RENDER_TARGET.0 as u32).ctx("MF_SA_D3D11_BINDFLAGS")?;
        attributes.SetUINT32(&MF_SA_D3D11_USAGE, D3D11_USAGE_DEFAULT.0 as u32).ctx("MF_SA_D3D11_USAGE")?;
        allocator.SetDirectXManager(manager).ctx("IMFVideoSampleAllocator::SetDirectXManager")?;
        let surface_type = nv12_type(format).ctx("NV12 media type")?;
        allocator
            .InitializeSampleAllocatorEx(SURFACES_INITIAL, SURFACES_MAX, &attributes, &surface_type)
            .ctx("InitializeSampleAllocatorEx")?;
    }
    Ok(allocator)
}

impl PipelineSource {
    pub fn new(grabber: Box<dyn Grabber>, d3d: Option<Arc<D3d>>, width: u32, height: u32, fps: u32) -> Self {
        let format = VideoFormat { width, height, fps, bitrate: video_bitrate(width, height, fps) };
        Self { grabber, d3d, format, path: Path::Unbound }
    }

    fn open_gpu(&self, d3d: &D3d, request: &SinkRequest, trace: &Trace) -> Result<(MfSink, GpuPath)> {
        let encoders = hardware_h264_encoders();
        if encoders.is_empty() {
            return Err(Error::msg("no hardware H.264 encoder is installed"));
        }
        trace.push(format!("hardware encoders: {}", encoders.join(", ")));
        let bind = D3D11_BIND_FLAG(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0);
        let (w, h) = (self.format.width, self.format.height);
        let latest = d3d.texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_USAGE_DEFAULT, bind, 0)?;
        let converter = VideoConverter::new(d3d, &latest, w, h)?;
        let manager = d3d.device_manager()?;
        let allocator = surface_allocator(&manager, &self.format)?;
        let mut gpu = GpuPath { latest, converter, allocator, _manager: manager };
        let trial_started = Instant::now();
        self.trial_encode(d3d, &mut gpu, request)?;
        trace.push(format!("hardware encoder trial ok in {} ms", trial_started.elapsed().as_millis()));
        let sink = MfSink::open_hardware(request, &self.format, &gpu._manager, trace)?;
        Ok((sink, gpu))
    }

    /// Hardware encoders can accept the media type and then fail on the second frame, as the Microsoft
    /// DX12 encoder does at 64x64 where NVENC refuses at once, and a session limit shows up the same
    /// way. The real file must not be the one to find out: once frames are lost there is no way to
    /// switch to the software encoder, so a few blank frames go to a throwaway file first.
    fn trial_encode(&self, d3d: &D3d, gpu: &mut GpuPath, request: &SinkRequest) -> Result<()> {
        let path = request.path.with_extension("trial");
        let probe = SinkRequest { path: path.clone(), audio: false };
        let result = (|| {
            let mut sink = MfSink::open_hardware(&probe, &self.format, &gpu._manager, &Trace::default())?;
            let frame_ticks = TICKS_PER_SEC / i64::from(self.format.fps.max(1));
            for n in 0..TRIAL_FRAMES {
                let sample = Self::gpu_frame(d3d, gpu)?;
                sink.write_video(Box::new(SendSample(sample)), n * frame_ticks, frame_ticks)?;
            }
            Box::new(sink).finalize()
        })();
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        result
    }

    fn cpu_path(&self) -> CpuPath {
        let (w, h) = (self.format.width, self.format.height);
        CpuPath { bgra: vec![0; w as usize * h as usize * 4], nv12: vec![0; nv12_len(w, h)] }
    }

    fn gpu_frame(d3d: &D3d, gpu: &mut GpuPath) -> Result<IMFSample> {
        let deadline = Instant::now() + ENCODER_STALL_TIMEOUT;
        let sample = loop {
            // SAFETY: a call on a live allocator.
            match unsafe { gpu.allocator.AllocateSample() } {
                Ok(sample) => break sample,
                Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(super::com::hr_error("The encoder stopped taking frames", &e)),
            }
        };
        // SAFETY: the allocator was set up with one buffer per sample, a DXGI surface.
        let buffer = unsafe { sample.GetBufferByIndex(0) }.ctx("GetBufferByIndex")?;
        let dxgi: IMFDXGIBuffer = buffer.cast().ctx("IMFDXGIBuffer")?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: on success `raw` holds a reference of the requested interface.
        let surface = unsafe {
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw).ctx("IMFDXGIBuffer::GetResource")?;
            ID3D11Texture2D::from_raw(raw)
        };
        gpu.converter.blit(&surface).map_err(|e| match d3d.removed_reason() {
            Some(reason) => Error::msg(format!("The graphics device was lost: {reason}")),
            None => e,
        })?;
        let two_d: IMF2DBuffer = buffer.cast().ctx("IMF2DBuffer")?;
        // SAFETY: queries and a setter on live objects. A flush submits the conversion at once.
        unsafe {
            let length = two_d.GetContiguousLength().ctx("GetContiguousLength")?;
            buffer.SetCurrentLength(length).ctx("SetCurrentLength")?;
            d3d.context.Flush();
        }
        Ok(sample)
    }
}

fn memory_sample(nv12: &[u8]) -> Result<IMFSample> {
    // SAFETY: the buffer is created with room for the data and locked around the copy.
    unsafe {
        let buffer = MFCreateMemoryBuffer(nv12.len() as u32).ctx("MFCreateMemoryBuffer")?;
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None).ctx("IMFMediaBuffer::Lock")?;
        std::ptr::copy_nonoverlapping(nv12.as_ptr(), data, nv12.len());
        buffer.Unlock().ctx("IMFMediaBuffer::Unlock")?;
        buffer.SetCurrentLength(nv12.len() as u32).ctx("SetCurrentLength")?;
        let sample = MFCreateSample().ctx("MFCreateSample")?;
        sample.AddBuffer(&buffer).ctx("AddBuffer")?;
        Ok(sample)
    }
}

impl VideoSource for PipelineSource {
    fn open_sink(&mut self, request: &SinkRequest, trace: &Trace) -> Result<Box<dyn MediaSink>> {
        if let Some(d3d) = self.d3d.clone() {
            match self.open_gpu(&d3d, request, trace) {
                Ok((sink, gpu)) => {
                    self.path = Path::Gpu(gpu);
                    return Ok(Box::new(sink));
                }
                Err(e) => trace.push(format!("hardware encoding unavailable: {e}")),
            }
        }
        let sink = MfSink::open_software(request, &self.format, trace)?;
        self.path = Path::Cpu(self.cpu_path());
        Ok(Box::new(sink))
    }

    fn wait_first_frame(&mut self, aborted: &dyn Fn() -> bool) -> Result<Option<Snapshot>> {
        let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
        while !self.grabber.ready() {
            if aborted() {
                return Ok(None);
            }
            if let Some(reason) = self.grabber.lost() {
                return Err(Error::msg(reason));
            }
            if Instant::now() >= deadline {
                return Err(Error::msg("Screen capture delivered no picture"));
            }
            thread::sleep(Duration::from_millis(2));
        }
        if aborted() {
            return Ok(None);
        }
        let (w, h) = (self.format.width, self.format.height);
        match &mut self.path {
            Path::Gpu(gpu) => {
                let Some(d3d) = &self.d3d else {
                    return Err(Error::msg("internal error: GPU path without a device"));
                };
                self.grabber.refresh_gpu(d3d, &gpu.latest)?;
                Ok(Some(d3d.snapshot(&gpu.latest, w, h)?))
            }
            Path::Cpu(cpu) => {
                let stride = w as usize * 4;
                self.grabber.refresh_cpu(self.d3d.as_deref(), &mut cpu.bgra, stride)?;
                Ok(Some(Snapshot { bgra: cpu.bgra.clone(), stride, width: w, height: h }))
            }
            Path::Unbound => Err(Error::msg("internal error: frames before an encoder")),
        }
    }

    fn next_frame(&mut self) -> Result<VideoFrame> {
        let (w, h) = (self.format.width, self.format.height);
        match &mut self.path {
            Path::Gpu(gpu) => {
                let Some(d3d) = &self.d3d else {
                    return Err(Error::msg("internal error: GPU path without a device"));
                };
                let fresh = self.grabber.refresh_gpu(d3d, &gpu.latest)?;
                let sample = Self::gpu_frame(d3d, gpu)?;
                Ok(VideoFrame { payload: Box::new(SendSample(sample)), fresh })
            }
            Path::Cpu(cpu) => {
                let stride = w as usize * 4;
                let fresh = self.grabber.refresh_cpu(self.d3d.as_deref(), &mut cpu.bgra, stride)?;
                if fresh {
                    bgra_to_nv12(&cpu.bgra, stride, w, h, &mut cpu.nv12);
                }
                let sample = memory_sample(&cpu.nv12)?;
                Ok(VideoFrame { payload: Box::new(SendSample(sample)), fresh })
            }
            Path::Unbound => Err(Error::msg("internal error: frames before an encoder")),
        }
    }

    fn lost(&self) -> Option<String> {
        self.grabber.lost()
    }
}

/// The animated test picture, drawn on the CPU and uploaded when the encoder works on the GPU.
pub(crate) struct SynthGrabber {
    scene: Scene,
    pixels: Vec<u8>,
    stride: usize,
}

impl SynthGrabber {
    pub fn new(scene: Scene, width: u32, height: u32) -> Self {
        let stride = width as usize * 4;
        Self { scene, pixels: vec![0; stride * height as usize], stride }
    }
}

impl Grabber for SynthGrabber {
    fn ready(&self) -> bool {
        true
    }

    fn refresh_gpu(&mut self, d3d: &D3d, latest: &ID3D11Texture2D) -> Result<bool> {
        self.scene.draw(qpc_ticks(), &mut self.pixels);
        // SAFETY: the texture was created with the size of this buffer and is not mapped.
        unsafe { d3d.context.UpdateSubresource(latest, 0, None, self.pixels.as_ptr().cast(), self.stride as u32, 0) };
        Ok(true)
    }

    fn refresh_cpu(&mut self, _d3d: Option<&D3d>, bgra: &mut [u8], stride: usize) -> Result<bool> {
        debug_assert_eq!(stride, self.stride);
        self.scene.draw(qpc_ticks(), bgra);
        Ok(true)
    }

    fn lost(&self) -> Option<String> {
        None
    }
}
