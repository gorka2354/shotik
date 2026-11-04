//! Direct3D 11 for capture and encoding: the device on the adapter that drives the monitor, a texture
//! helper and the GPU conversion from BGRA to NV12.

use std::mem::ManuallyDrop;
use std::sync::Arc;

use shotik_core::{Error, Result};
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, ID3D11VideoContext,
    ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView, D3D11_BIND_FLAG, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEX2D_VPIV,
    D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE, D3D11_USAGE_STAGING,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};

use super::com::Context;
use crate::backend::Snapshot;
use crate::trace::Trace;

/// The highest GPU thread priority a process can ask for without privileges: a game keeps the GPU
/// busy and DWM is already ahead of it, so a recording at normal priority can fall behind.
const GPU_PRIORITY: i32 = 7;

pub(crate) struct D3d {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
}

// SAFETY: the device is created multithread protected below, which serializes the immediate context.
unsafe impl Send for D3d {}
// SAFETY: as above.
unsafe impl Sync for D3d {}

fn adapter_for(monitor: Option<HMONITOR>) -> windows::core::Result<Option<IDXGIAdapter1>> {
    // SAFETY: plain factory creation; the interfaces own their references.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut first = None;
    let mut index = 0;
    // SAFETY: enumeration ends with an error at the first index out of range.
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        let Some(monitor) = monitor else { return Ok(Some(adapter)) };
        let mut output_index = 0;
        // SAFETY: as above.
        while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
            output_index += 1;
            // SAFETY: the description is returned by value.
            if unsafe { output.GetDesc() }.is_ok_and(|desc| desc.Monitor == monitor) {
                return Ok(Some(adapter));
            }
        }
        first.get_or_insert(adapter);
    }
    Ok(first)
}

impl D3d {
    /// A device on the adapter that drives `monitor`, or on the first adapter without one.
    pub fn create(monitor: Option<HMONITOR>, trace: &Trace) -> Result<Arc<Self>> {
        let adapter = adapter_for(monitor).ctx("Cannot list the graphics adapters")?;
        let Some(adapter) = adapter else {
            return Err(Error::msg("No graphics adapter found"));
        };
        // SAFETY: the description is returned by value.
        let name = unsafe { adapter.GetDesc1() }
            .map(|desc| String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string())
            .unwrap_or_default();
        let adapter: IDXGIAdapter = adapter.cast().ctx("IDXGIAdapter")?;

        let mut device = None;
        let mut context = None;
        // SAFETY: the out pointers are locals, the adapter outlives the call.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .ctx("Cannot create the Direct3D device")?;
        let (Some(device), Some(context)) = (device, context) else {
            return Err(Error::msg("Direct3D returned no device"));
        };

        let protect: ID3D11Multithread = context.cast().ctx("ID3D11Multithread")?;
        // SAFETY: a plain flag on a live object, set before any other thread sees the device.
        let _ = unsafe { protect.SetMultithreadProtected(true) };
        let dxgi: IDXGIDevice = device.cast().ctx("IDXGIDevice")?;
        // SAFETY: a plain value on a live object.
        if let Err(e) = unsafe { dxgi.SetGPUThreadPriority(GPU_PRIORITY) } {
            trace.push(format!("gpu priority not set: {e}"));
        }
        trace.push(format!("adapter \"{name}\""));
        Ok(Arc::new(Self { device, context }))
    }

    /// What Media Foundation needs to use this device.
    pub fn device_manager(&self) -> Result<IMFDXGIDeviceManager> {
        let mut token = 0;
        let mut manager = None;
        // SAFETY: the out pointers are locals.
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }.ctx("MFCreateDXGIDeviceManager")?;
        let Some(manager) = manager else {
            return Err(Error::msg("MFCreateDXGIDeviceManager returned nothing"));
        };
        // SAFETY: the token belongs to this manager, the device is live.
        unsafe { manager.ResetDevice(&self.device, token) }.ctx("IMFDXGIDeviceManager::ResetDevice")?;
        Ok(manager)
    }

    pub fn texture(
        &self,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        usage: D3D11_USAGE,
        bind: D3D11_BIND_FLAG,
        cpu_access: u32,
    ) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: usage,
            BindFlags: bind.0 as u32,
            CPUAccessFlags: cpu_access,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: the description and the out pointer are locals.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }.ctx("CreateTexture2D")?;
        texture.ok_or_else(|| Error::msg("CreateTexture2D returned nothing"))
    }

    pub fn staging_bgra(&self, width: u32, height: u32) -> Result<ID3D11Texture2D> {
        self.texture(
            width,
            height,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_USAGE_STAGING,
            D3D11_BIND_FLAG(0),
            D3D11_CPU_ACCESS_READ.0 as u32,
        )
    }

    /// Copies a mapped staging texture into `out`, rows top-down with `stride` bytes each.
    pub fn read_staging(&self, staging: &ID3D11Texture2D, width: u32, height: u32, out: &mut [u8], stride: usize) -> Result<()> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: a staging texture created with CPU read access, the out value is a local.
        unsafe { self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.ctx("Map")?;
        let row_bytes = width as usize * 4;
        for row in 0..height as usize {
            // SAFETY: Map gave a pointer to `height` rows of `RowPitch` bytes, each at least `row_bytes`
            // long, valid until Unmap below, and `out` holds `height` rows of `stride` by the callers.
            unsafe {
                let src = mapped.pData.cast::<u8>().add(row * mapped.RowPitch as usize);
                std::ptr::copy_nonoverlapping(src, out[row * stride..row * stride + row_bytes].as_mut_ptr(), row_bytes);
            }
        }
        // SAFETY: mapped above.
        unsafe { self.context.Unmap(staging, 0) };
        Ok(())
    }

    /// Reads a BGRA texture back: a copy that waits for the GPU, so not for the frame path.
    pub fn snapshot(&self, texture: &ID3D11Texture2D, width: u32, height: u32) -> Result<Snapshot> {
        let staging = self.staging_bgra(width, height)?;
        // SAFETY: same size and format, both live.
        unsafe { self.context.CopyResource(&staging, texture) };
        let stride = width as usize * 4;
        let mut bgra = vec![0; stride * height as usize];
        self.read_staging(&staging, width, height, &mut bgra, stride)?;
        Ok(Snapshot { bgra, stride, width, height })
    }

    /// Says why the GPU went away, when it did: a driver reset during a game is a real case.
    pub fn removed_reason(&self) -> Option<String> {
        // SAFETY: a query on a live object.
        unsafe { self.device.GetDeviceRemovedReason() }.err().map(|e| format!("{:#010X} {}", e.code().0 as u32, e.message()))
    }
}

impl Drop for D3d {
    fn drop(&mut self) {
        // SAFETY: called on the last reference. Unbinding and flushing release what Direct3D otherwise
        // keeps for deferred destruction.
        unsafe {
            self.context.ClearState();
            self.context.Flush();
        }
    }
}

/// Bits of `D3D11_VIDEO_PROCESSOR_COLOR_SPACE`: YCbCr_Matrix is bit 2, Nominal_Range bits 4 and 5.
const COLOR_SPACE_RGB_FULL: u32 = 0;
const COLOR_SPACE_BT709_16_235: u32 = (1 << 2) | (1 << 4);
const FORMAT_SUPPORT_INPUT: u32 = 1;
const FORMAT_SUPPORT_OUTPUT: u32 = 2;

/// BGRA to BT.709 limited range NV12 on the GPU.
pub(crate) struct VideoConverter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    input_view: ID3D11VideoProcessorInputView,
    /// Views of the encoder's surfaces, by texture. The sample allocator recycles a fixed set.
    outputs: Vec<(usize, ID3D11VideoProcessorOutputView)>,
}

// SAFETY: see the module comment of `win`; only one thread uses it at a time.
unsafe impl Send for VideoConverter {}

impl VideoConverter {
    pub fn new(d3d: &D3d, source: &ID3D11Texture2D, width: u32, height: u32) -> Result<Self> {
        let video_device: ID3D11VideoDevice = d3d.device.cast().ctx("ID3D11VideoDevice")?;
        let video_context: ID3D11VideoContext = d3d.context.cast().ctx("ID3D11VideoContext")?;
        let rate = DXGI_RATIONAL { Numerator: 1, Denominator: 1 };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: width,
            InputHeight: height,
            OutputFrameRate: rate,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        // SAFETY: the description is a local, the rest are queries on live objects.
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content) }.ctx("CreateVideoProcessorEnumerator")?;
        // SAFETY: as above.
        let input = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM) }.ctx("CheckVideoProcessorFormat")?;
        // SAFETY: as above.
        let output = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }.ctx("CheckVideoProcessorFormat")?;
        if input & FORMAT_SUPPORT_INPUT == 0 || output & FORMAT_SUPPORT_OUTPUT == 0 {
            return Err(Error::msg("the video processor cannot convert BGRA to NV12"));
        }
        // SAFETY: the enumerator is live.
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }.ctx("CreateVideoProcessor")?;

        let rect = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
        // SAFETY: plain state changes on live objects, the pointers are to locals used during the call.
        unsafe {
            video_context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&rect));
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&rect));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&rect));
            let input_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: COLOR_SPACE_RGB_FULL };
            video_context.VideoProcessorSetStreamColorSpace(&processor, 0, &input_space);
            let output_space = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: COLOR_SPACE_BT709_16_235 };
            video_context.VideoProcessorSetOutputColorSpace(&processor, &output_space);
        }

        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let mut input_view = None;
        // SAFETY: the description and out pointer are locals, the resources are live.
        unsafe { video_device.CreateVideoProcessorInputView(source, &enumerator, &desc, Some(&mut input_view)) }
            .ctx("CreateVideoProcessorInputView")?;
        let Some(input_view) = input_view else {
            return Err(Error::msg("CreateVideoProcessorInputView returned nothing"));
        };
        Ok(Self { video_device, video_context, enumerator, processor, input_view, outputs: Vec::new() })
    }

    fn output_view(&mut self, target: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorOutputView> {
        let key = target.as_raw() as usize;
        if let Some((_, view)) = self.outputs.iter().find(|(k, _)| *k == key) {
            return Ok(view.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut view = None;
        // SAFETY: the description and out pointer are locals, the resources are live. A view holds a
        // reference to its texture, so the key stays valid while the entry exists.
        unsafe { self.video_device.CreateVideoProcessorOutputView(target, &self.enumerator, &desc, Some(&mut view)) }
            .ctx("CreateVideoProcessorOutputView")?;
        let Some(view) = view else {
            return Err(Error::msg("CreateVideoProcessorOutputView returned nothing"));
        };
        self.outputs.push((key, view.clone()));
        Ok(view)
    }

    /// Converts the source texture into `target`, which must be NV12 with render target binding.
    pub fn blit(&mut self, target: &ID3D11Texture2D) -> Result<()> {
        let output = self.output_view(target)?;
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(self.input_view.clone())),
            ..D3D11_VIDEO_PROCESSOR_STREAM::default()
        };
        // SAFETY: the views and the processor are live, the stream array lives during the call.
        let result = unsafe {
            self.video_context.VideoProcessorBlt(&self.processor, &output, 0, std::slice::from_ref(&stream))
        };
        // SAFETY: the struct holds a reference of its own that nothing else drops, and it is not used again.
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.ctx("VideoProcessorBlt")
    }
}
