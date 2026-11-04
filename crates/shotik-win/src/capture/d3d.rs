//! A Direct3D 11 device and the one thing capture needs it for: reading a texture back to memory.
//!
//! **A device is created once per adapter and never released.** Releasing a device that has captured
//! anything leaks one kernel Section, one Mutant and about 120 KB of private memory, on the machine this was
//! measured on (`capture_compare leak dxgi duplicate`; a capture session does the same). On a device that stays
//! alive, a duplication or a session per grab leaks neither. That is what took the Electron app to 139,628
//! handles: Chromium's duplication path recreates its device after a failure. So nothing here creates a device
//! until a capture path actually needs one, which the main path never does, and from then on it stays. It
//! costs about 110 MB of private memory for the rest of the process, only if that happens.

use std::sync::{Arc, Mutex, OnceLock};

use shotik_core::error::{Error, Result};
use shotik_core::frame::Frame;
use windows::core::Interface;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1};
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;

use crate::displays::win_err;

/// Device and immediate context, with the context made safe to call from any thread.
pub(crate) struct D3d {
    pub(crate) device: ID3D11Device,
    pub(crate) context: ID3D11DeviceContext,
}

fn create_on(adapter: &IDXGIAdapter1) -> Result<D3d> {
    let (mut device, mut context) = (None, None);
    // SAFETY: the out-parameters are valid for the call and the adapter is alive. BGRA support is what
    // Windows.Graphics.Capture needs from a device.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| win_err("cannot create a Direct3D 11 device", e))?;
    let (Some(device), Some(context)) = (device, context) else {
        return Err(Error::msg("Direct3D 11 returned no device"));
    };
    // Callers on several threads share one immediate context, and the free-threaded frame pool reaches it too.
    let protect: ID3D11Multithread = context.cast().map_err(|e| win_err("ID3D11Multithread", e))?;
    // SAFETY: a plain setter on a live object.
    unsafe {
        let _ = protect.SetMultithreadProtected(true);
    }
    Ok(D3d { device, context })
}

/// The device for one adapter, for the life of the process. See the top of this file for why.
pub(crate) struct SharedDevice {
    d3d: D3d,
    winrt: OnceLock<IDirect3DDevice>,
}

// SAFETY: Direct3D 11 devices are free-threaded by design, the immediate context is multithread-protected
// above, and the WinRT wrapper is an agile object around the same device.
unsafe impl Send for SharedDevice {}
// SAFETY: as above.
unsafe impl Sync for SharedDevice {}

/// The key of an adapter: its LUID, stable for the life of the process.
type AdapterKey = (u32, i32);

fn adapter_key(adapter: &IDXGIAdapter1) -> Result<AdapterKey> {
    // SAFETY: a plain getter on a live object.
    let desc = unsafe { adapter.GetDesc1() }.map_err(|e| win_err("cannot read the adapter", e))?;
    Ok((desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart))
}

static DEVICES: Mutex<Vec<(AdapterKey, Arc<SharedDevice>)>> = Mutex::new(Vec::new());

/// The shared device for an adapter, or for the default one. Created the first time it is asked for.
pub(crate) fn shared_device(adapter: Option<&IDXGIAdapter1>) -> Result<Arc<SharedDevice>> {
    let default;
    let adapter = match adapter {
        Some(adapter) => adapter,
        None => {
            // SAFETY: creates a COM object with no input; index 0 is the adapter the system prefers.
            let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| win_err("cannot reach DXGI", e))?;
            default = unsafe { factory.EnumAdapters1(0) }.map_err(|e| win_err("no graphics adapter", e))?;
            &default
        }
    };
    let key = adapter_key(adapter)?;
    let mut devices = DEVICES.lock().map_err(|_| Error::msg("the device list is poisoned"))?;
    if let Some((_, device)) = devices.iter().find(|(k, _)| *k == key) {
        return Ok(Arc::clone(device));
    }
    let device = Arc::new(SharedDevice { d3d: create_on(adapter)?, winrt: OnceLock::new() });
    devices.push((key, Arc::clone(&device)));
    Ok(device)
}

impl SharedDevice {
    pub(crate) fn d3d(&self) -> &D3d {
        &self.d3d
    }

    /// The device as Windows.Graphics.Capture wants it.
    pub(crate) fn winrt(&self) -> Result<&IDirect3DDevice> {
        if let Some(winrt) = self.winrt.get() {
            return Ok(winrt);
        }
        let dxgi: IDXGIDevice = self.d3d.device.cast().map_err(|e| win_err("IDXGIDevice", e))?;
        // SAFETY: takes a live DXGI device.
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .map_err(|e| win_err("cannot wrap the device for capture", e))?;
        let winrt: IDirect3DDevice = inspectable.cast().map_err(|e| win_err("IDirect3DDevice", e))?;
        // Losing a race just drops the second wrapper, which holds no device of its own.
        Ok(self.winrt.get_or_init(|| winrt))
    }

    /// Copies a BGRA texture to a new frame, through a staging texture that lives only for this call: nothing
    /// holds a frame between grabs. The alpha channel of a screen surface is undefined, so it is forced
    /// opaque. A PNG with alpha 0 would be an invisible screenshot.
    pub(crate) fn read_texture(&self, texture: &ID3D11Texture2D) -> Result<Frame> {
        let (device, context) = (&self.d3d.device, &self.d3d.context);
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` is a writable out-parameter on a live texture.
        unsafe { texture.GetDesc(&mut desc) };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        desc.ArraySize = 1;
        desc.MipLevels = 1;
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` describes a valid staging texture and the out-parameter is valid.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }
            .map_err(|e| win_err("cannot allocate a staging texture", e))?;
        let staging = staging.ok_or_else(|| Error::msg("Direct3D 11 returned no staging texture"))?;
        // SAFETY: same size and format, created from the same device.
        unsafe { context.CopyResource(&staging, texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture has CPU read access. Map blocks until the copy has finished.
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
            .map_err(|e| win_err("cannot read the captured surface", e))?;
        let (w, h) = (desc.Width as usize, desc.Height as usize);
        let mut out = vec![0u8; w * h * 4];
        for (y, row) in out.chunks_exact_mut(w * 4).enumerate() {
            // SAFETY: Map gives `RowPitch` bytes per row for `h` rows, each at least `w * 4` long.
            let src = unsafe {
                std::slice::from_raw_parts((mapped.pData as *const u8).add(y * mapped.RowPitch as usize), w * 4)
            };
            row.copy_from_slice(src);
        }
        // SAFETY: mapped above; the slices borrowed from it are gone.
        unsafe { context.Unmap(&staging, 0) };
        force_opaque(&mut out);
        Frame::new(desc.Width, desc.Height, out)
    }
}

pub(crate) fn force_opaque(bgra: &mut [u8]) {
    for px in bgra.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_adapter_gets_the_same_device() {
        let first = shared_device(None).unwrap();
        let second = shared_device(None).unwrap();
        assert!(Arc::ptr_eq(&first, &second), "a second device would leak on release if it were ever dropped");
        let wrapper = first.winrt().unwrap().clone();
        assert_eq!(&wrapper, second.winrt().unwrap(), "one wrapper");
    }

    #[test]
    fn force_opaque_touches_only_alpha() {
        let mut px = [1, 2, 3, 0, 4, 5, 6, 77];
        force_opaque(&mut px);
        assert_eq!(px, [1, 2, 3, 255, 4, 5, 6, 255]);
    }
}
