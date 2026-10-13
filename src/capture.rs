//! Windows.Graphics.Capture of one monitor into a persistent D3D11 texture.
use anyhow::{anyhow, Result};
use std::sync::{Arc, Mutex};
use windows::core::{factory, IInspectable, Interface};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Threading::{CreateEventW, SetEvent};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// Initialise WinRT in the multithreaded apartment (idempotent).
pub fn init_winrt() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

/// Auto-reset event signalled whenever any capture stores a new frame.
pub struct FrameEvent(pub HANDLE);
unsafe impl Send for FrameEvent {}
unsafe impl Sync for FrameEvent {}
impl FrameEvent {
    pub fn new() -> Result<Arc<Self>> {
        let h = unsafe { CreateEventW(None, false, false, None)? };
        Ok(Arc::new(Self(h)))
    }
    fn signal(&self) {
        unsafe {
            let _ = SetEvent(self.0);
        }
    }
}
impl Drop for FrameEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Moves COM pointers into callbacks. D3D11 objects are used from several
/// threads only with multithread protection enabled.
pub struct SendBox<T>(pub T);
unsafe impl<T> Send for SendBox<T> {}
unsafe impl<T> Sync for SendBox<T> {}
impl<T> SendBox<T> {
    pub fn get(&self) -> &T {
        &self.0
    }
}

struct Shared {
    /// Persistent copy of the most recent frame (None until the first frame).
    texture: Mutex<Option<ID3D11Texture2D>>,
    event: Arc<FrameEvent>,
}

pub struct MonitorCapture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: i64,
    shared: Arc<Shared>,
}

fn to_winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    let dxgi: IDXGIDevice = device.cast()?;
    let insp = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
    Ok(insp.cast()?)
}

impl MonitorCapture {
    /// Start capturing `hmonitor` (as returned by `displays::enumerate`).
    /// `device` must have multithread protection enabled.
    pub fn start(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        hmonitor: isize,
        event: Arc<FrameEvent>,
    ) -> Result<Self> {
        init_winrt();
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForMonitor(HMONITOR(hmonitor as *mut _))? };
        let size = item.Size()?;
        let winrt_dev = to_winrt_device(device)?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_dev,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let shared = Arc::new(Shared {
            texture: Mutex::new(None),
            event,
        });

        let sh = shared.clone();
        let com = SendBox((device.clone(), context.clone(), winrt_dev.clone()));
        let cur_size = Mutex::new((size.Width, size.Height));
        let handler =
            TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |sender, _| {
                let (dev, ctx, wdev) = com.get();
                let Some(pool) = sender.as_ref() else {
                    return Ok(());
                };
                let frame = pool.TryGetNextFrame()?;
                let content = frame.ContentSize()?;
                let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
                let src: ID3D11Texture2D = unsafe { access.GetInterface()? };
                let mut sdesc = D3D11_TEXTURE2D_DESC::default();
                unsafe { src.GetDesc(&mut sdesc) };

                {
                    let mut slot = sh.texture.lock().unwrap();
                    let need_new = match slot.as_ref() {
                        None => true,
                        Some(t) => {
                            let mut d = D3D11_TEXTURE2D_DESC::default();
                            unsafe { t.GetDesc(&mut d) };
                            d.Width != sdesc.Width || d.Height != sdesc.Height
                        }
                    };
                    if need_new {
                        let desc = D3D11_TEXTURE2D_DESC {
                            Width: sdesc.Width,
                            Height: sdesc.Height,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: sdesc.Format,
                            SampleDesc: sdesc.SampleDesc,
                            Usage: D3D11_USAGE_DEFAULT,
                            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                            CPUAccessFlags: 0,
                            MiscFlags: 0,
                        };
                        let mut tex = None;
                        unsafe { dev.CreateTexture2D(&desc, None, Some(&mut tex))? };
                        *slot = tex;
                    }
                    if let Some(dst) = slot.as_ref() {
                        unsafe { ctx.CopyResource(dst, &src) };
                    }
                }
                sh.event.signal();

                // Frame size changed: recreate the pool at the new size.
                let changed = {
                    let mut cs = cur_size.lock().unwrap();
                    let c = *cs != (content.Width, content.Height);
                    *cs = (content.Width, content.Height);
                    c
                };
                if changed {
                    pool.Recreate(
                        wdev,
                        DirectXPixelFormat::B8G8R8A8UIntNormalized,
                        2,
                        SizeInt32 {
                            Width: content.Width,
                            Height: content.Height,
                        },
                    )?;
                }
                Ok(())
            });
        let token = pool.FrameArrived(&handler)?;
        let session = pool.CreateCaptureSession(&item)?;
        session
            .SetIsCursorCaptureEnabled(true)
            .map_err(|e| anyhow!("IsCursorCaptureEnabled: {e}"))?;
        if let Err(e) = session.SetIsBorderRequired(false) {
            tracing::warn!("IsBorderRequired(false) not honoured: {e}");
        }
        session.StartCapture()?;
        Ok(Self {
            pool,
            session,
            token,
            shared,
        })
    }

    /// Latest captured frame (persistent texture owned by us), if any arrived yet.
    pub fn latest(&self) -> Option<ID3D11Texture2D> {
        self.shared.texture.lock().unwrap().clone()
    }
}

impl Drop for MonitorCapture {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
