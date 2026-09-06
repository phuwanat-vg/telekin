//! DXGI Desktop Duplication capture backend.

use anyhow::Context;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTPUT_DESC,
};

use super::{Frame, ScreenCapture};
use telekin_proto::MonitorInfo;

/// How long to block waiting for a desktop change before returning `None`.
const ACQUIRE_TIMEOUT_MS: u32 = 100;

/// Walk every adapter's attached outputs in a stable order; index == monitor id.
fn enumerate_outputs() -> anyhow::Result<Vec<(IDXGIAdapter1, IDXGIOutput, DXGI_OUTPUT_DESC)>> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut out = Vec::new();
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                let desc = output.GetDesc()?;
                if desc.AttachedToDesktop.as_bool() {
                    out.push((adapter.clone(), output, desc));
                }
                oi += 1;
            }
            ai += 1;
        }
        Ok(out)
    }
}

fn desc_name(desc: &DXGI_OUTPUT_DESC) -> String {
    let raw = &desc.DeviceName;
    let len = raw.iter().position(|c| *c == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..len])
}

pub fn list_monitors() -> anyhow::Result<Vec<MonitorInfo>> {
    let outputs = enumerate_outputs()?;
    Ok(outputs
        .iter()
        .enumerate()
        .map(|(i, (_, _, desc))| {
            let r = desc.DesktopCoordinates;
            MonitorInfo {
                id: i as u32,
                width: (r.right - r.left) as u32,
                height: (r.bottom - r.top) as u32,
                name: desc_name(desc),
            }
        })
        .collect())
}

pub fn open(monitor: u32) -> anyhow::Result<Box<dyn ScreenCapture>> {
    Ok(Box::new(DxgiCapture::new(monitor)?))
}

struct DxgiCapture {
    monitor: u32,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// CPU-readable copy target, reallocated when dimensions change.
    staging: Option<ID3D11Texture2D>,
    width: u32,
    height: u32,
    /// Tightly packed BGRA; DXGI rows are padded, so we repack on copy.
    buffer: Vec<u8>,
    have_frame: bool,
}

impl DxgiCapture {
    fn new(monitor: u32) -> anyhow::Result<Self> {
        let outputs = enumerate_outputs()?;
        let (adapter, output, desc) = outputs
            .into_iter()
            .nth(monitor as usize)
            .with_context(|| format!("monitor {monitor} not found"))?;

        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        unsafe {
            // A device bound to a specific adapter must pass driver type UNKNOWN.
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("D3D11CreateDevice failed")?;
        }
        let device = device.context("no D3D11 device")?;
        let context = context.context("no D3D11 context")?;

        let output1: IDXGIOutput1 = output.cast()?;
        let duplication = unsafe { output1.DuplicateOutput(&device) }
            .context("DuplicateOutput failed (is another capture app already running?)")?;

        let r = desc.DesktopCoordinates;
        Ok(Self {
            monitor,
            device,
            context,
            duplication,
            staging: None,
            width: (r.right - r.left) as u32,
            height: (r.bottom - r.top) as u32,
            buffer: Vec::new(),
            have_frame: false,
        })
    }

    /// Re-create the duplication object after DXGI_ERROR_ACCESS_LOST
    /// (mode change, UAC prompt, session switch, GPU driver reset).
    fn reinit(&mut self) -> anyhow::Result<()> {
        *self = Self::new(self.monitor)?;
        Ok(())
    }

    fn ensure_staging(&mut self, width: u32, height: u32) -> anyhow::Result<ID3D11Texture2D> {
        if let Some(tex) = &self.staging {
            if self.width == width && self.height == height {
                return Ok(tex.clone());
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex))? };
        let tex = tex.context("CreateTexture2D returned null")?;
        self.staging = Some(tex.clone());
        self.width = width;
        self.height = height;
        self.have_frame = false;
        Ok(tex)
    }

    /// Copy the acquired frame into `self.buffer`. Split out so the caller
    /// can release the DXGI frame on every path.
    fn copy_acquired(&mut self, resource: IDXGIResource) -> anyhow::Result<()> {
        let src: ID3D11Texture2D = resource.cast()?;
        let mut src_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut src_desc) };

        let staging = self.ensure_staging(src_desc.Width, src_desc.Height)?;
        unsafe { self.context.CopyResource(&staging, &src) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("Map failed")?;
        }

        let (w, h) = (self.width as usize, self.height as usize);
        self.buffer.resize(w * h * 4, 0);
        let src_pitch = mapped.RowPitch as usize;
        let row_bytes = w * 4;
        unsafe {
            let base = mapped.pData as *const u8;
            for y in 0..h {
                std::ptr::copy_nonoverlapping(
                    base.add(y * src_pitch),
                    self.buffer.as_mut_ptr().add(y * row_bytes),
                    row_bytes,
                );
            }
            self.context.Unmap(&staging, 0);
        }
        self.have_frame = true;
        Ok(())
    }
}

impl ScreenCapture for DxgiCapture {
    fn next_frame(&mut self) -> anyhow::Result<Option<Frame<'_>>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;

        let acquired = unsafe {
            self.duplication
                .AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut info, &mut resource)
        };
        match acquired {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                tracing::warn!("DXGI access lost; re-initializing duplication");
                self.reinit()?;
                return Ok(None);
            }
            Err(e) => return Err(e).context("AcquireNextFrame failed"),
        }

        // LastPresentTime == 0 means only the cursor moved: no new pixels.
        let result = if info.LastPresentTime == 0 {
            Ok(false)
        } else {
            match resource {
                Some(r) => self.copy_acquired(r).map(|()| true),
                None => Ok(false),
            }
        };

        unsafe {
            let _ = self.duplication.ReleaseFrame();
        }

        if result? {
            Ok(Some(Frame {
                width: self.width,
                height: self.height,
                bgra: &self.buffer,
            }))
        } else {
            Ok(None)
        }
    }

    fn last_frame(&self) -> Option<Frame<'_>> {
        self.have_frame.then(|| Frame {
            width: self.width,
            height: self.height,
            bgra: &self.buffer,
        })
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}
