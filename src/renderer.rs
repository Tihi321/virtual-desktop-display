//! D3D11 device, borderless topmost window on the physical monitor, flip-model
//! swapchain and the compose loop (each virtual display's capture is drawn
//! over its piece of the physical display).
use crate::capture::{FrameEvent, MonitorCapture, SendBox};
use crate::displays::Rect;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use windows::core::{s, w, Interface, PCSTR};
use windows::Win32::Foundation::{HANDLE, HMODULE, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11PixelShader,
    ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11VertexShader, D3D11_BOX, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_SAMPLER_DESC, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, IDXGISwapChain2, DXGI_MWA_NO_ALT_ENTER,
    DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SetWindowPos, ShowWindow, TranslateMessage, HWND_TOPMOST, MA_NOACTIVATE, MSG,
    SWP_NOACTIVATE, SW_SHOWNOACTIVATE, WM_CLOSE, WM_DESTROY, WM_MOUSEACTIVATE, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// Device + immediate context on the default hardware adapter.
#[derive(Clone)]
pub struct D3d {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
}

pub fn create_device() -> Result<D3d> {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device = device.ok_or_else(|| anyhow!("no D3D11 device"))?;
    let context = context.ok_or_else(|| anyhow!("no D3D11 context"))?;
    let mt: ID3D11Multithread = context.cast()?;
    unsafe {
        let _ = mt.SetMultithreadProtected(true);
    }
    Ok(D3d { device, context })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Create the window and pump its messages on a dedicated thread.
fn spawn_window_thread(rect: Rect) -> Result<(isize, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel::<Result<isize, String>>();
    let th = std::thread::Builder::new()
        .name("render-window".into())
        .spawn(move || unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let class = w!("VirtualDesktopDisplayRenderer");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc); // 0 if already registered; harmless.
            let hwnd = match CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("virtual-desktop-display"),
                WS_POPUP,
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                None,
                None,
                Some(hinst.into()),
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    let _ = tx.send(Err(format!("CreateWindowExW: {e}")));
                    return;
                }
            };
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = tx.send(Ok(hwnd.0 as isize));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })?;
    match rx.recv() {
        Ok(Ok(h)) => Ok((h, th)),
        Ok(Err(e)) => bail!(e),
        Err(_) => bail!("window thread died"),
    }
}

pub struct Renderer {
    hwnd: isize,
    stop: Arc<AtomicBool>,
    render_thread: Option<JoinHandle<()>>,
    window_thread: Option<JoinHandle<()>>,
}

fn copy_region(
    ctx: &ID3D11DeviceContext,
    dst: &ID3D11Texture2D,
    dst_w: u32,
    dst_h: u32,
    dx: u32,
    dy: u32,
    src: &ID3D11Texture2D,
) {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    unsafe { src.GetDesc(&mut d) };
    // Clip to the destination.
    let w = d.Width.min(dst_w.saturating_sub(dx));
    let h = d.Height.min(dst_h.saturating_sub(dy));
    if w == 0 || h == 0 {
        return;
    }
    let b = D3D11_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: w,
        bottom: h,
        back: 1,
    };
    unsafe { ctx.CopySubresourceRegion(dst, 0, dx, dy, 0, src, 0, Some(&b)) };
}

impl Renderer {
    /// Start rendering onto a window covering `physical`. Each piece is a
    /// capture plus its destination rect RELATIVE to the physical display;
    /// everything not covered by a piece stays black.
    pub fn start(
        d3d: &D3d,
        physical: Rect,
        pieces: Vec<(Arc<MonitorCapture>, Rect)>,
        frame_event: Arc<FrameEvent>,
    ) -> Result<Self> {
        let rect = physical;
        let (hwnd_raw, window_thread) = spawn_window_thread(rect)?;
        let hwnd = HWND(hwnd_raw as *mut _);

        let setup = || -> Result<(IDXGISwapChain2, HANDLE)> {
            let dxgi_dev: IDXGIDevice = d3d.device.cast()?;
            let factory: IDXGIFactory2 = unsafe { dxgi_dev.GetAdapter()?.GetParent()? };
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: rect.w as u32,
                Height: rect.h as u32,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
            };
            let sc1: IDXGISwapChain1 =
                unsafe { factory.CreateSwapChainForHwnd(&d3d.device, hwnd, &desc, None, None)? };
            unsafe {
                let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            }
            let sc: IDXGISwapChain2 = sc1.cast()?;
            unsafe { sc.SetMaximumFrameLatency(1)? };
            let waitable = unsafe { sc.GetFrameLatencyWaitableObject() };
            if waitable.is_invalid() {
                bail!("no frame latency waitable object");
            }
            Ok((sc, waitable))
        };
        let (sc, waitable) = match setup() {
            Ok(v) => v,
            Err(e) => {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                let _ = window_thread.join();
                return Err(e);
            }
        };

        let pw = rect.w as u32;
        let ph = rect.h as u32;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let device = SendBox(d3d.device.clone());
        let ctx = SendBox(d3d.context.clone());
        let sc = SendBox(sc);
        let waitable = SendBox(waitable);
        let pieces = SendBox(pieces);
        let render_thread = std::thread::Builder::new()
            .name("render".into())
            .spawn(move || {
                let (device, ctx, sc, waitable, pieces) = (
                    device.get(),
                    ctx.get(),
                    sc.get(),
                    *waitable.get(),
                    pieces.get(),
                );
                let mut gpu = Gpu::default();
                let mut first = true;
                while !stop_t.load(Ordering::Relaxed) {
                    // Wake on a new frame (WGC only delivers on change).
                    let woke = unsafe { WaitForSingleObject(frame_event.0, 100) } == WAIT_OBJECT_0;
                    if !first && !woke {
                        continue;
                    }
                    first = false;
                    if unsafe { WaitForSingleObject(waitable, 1000) } != WAIT_OBJECT_0 {
                        continue;
                    }
                    let buf: ID3D11Texture2D = match unsafe { sc.GetBuffer(0) } {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::error!("GetBuffer: {e}");
                            break;
                        }
                    };
                    match gpu.rtv(device, &buf) {
                        Ok(rtv) => unsafe {
                            ctx.ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 1.0]);
                        },
                        Err(e) => tracing::error!("backbuffer RTV: {e}"),
                    }
                    for (cap, dest) in pieces.iter() {
                        let Some(t) = cap.latest() else { continue };
                        let (dx, dy) = (dest.x.max(0) as u32, dest.y.max(0) as u32);
                        let mut d = D3D11_TEXTURE2D_DESC::default();
                        unsafe { t.GetDesc(&mut d) };
                        if d.Width == dest.w as u32 && d.Height == dest.h as u32 {
                            copy_region(ctx, &buf, pw, ph, dx, dy, &t);
                            continue;
                        }
                        if let Err(e) = gpu.stretch(device, ctx, &buf, &t, *dest) {
                            tracing::error!("stretch draw failed: {e}");
                            copy_region(ctx, &buf, pw, ph, dx, dy, &t);
                        }
                    }
                    let hr =
                        unsafe { sc.Present(1, windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0)) };
                    if hr.is_err() {
                        tracing::error!("Present failed: {hr:?}");
                        break;
                    }
                }
            })?;

        Ok(Self {
            hwnd: hwnd_raw,
            stop,
            render_thread: Some(render_thread),
            window_thread: Some(window_thread),
        })
    }

    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.render_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.window_thread.take() {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(self.hwnd as *mut _)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            let _ = t.join();
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.stop();
    }
}

const HLSL: &str = r#"
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VSOut vs_main(uint id : SV_VertexID) {
    VSOut o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    o.uv = uv;
    return o;
}
Texture2D tex : register(t0);
SamplerState smp : register(s0);
float4 ps_main(VSOut i) : SV_Target { return tex.Sample(smp, i.uv); }
"#;

fn compile_shader(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut blob: Option<ID3DBlob> = None;
    let mut err: Option<ID3DBlob> = None;
    let r = unsafe {
        D3DCompile(
            HLSL.as_ptr() as *const _,
            HLSL.len(),
            PCSTR::null(),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut blob,
            Some(&mut err),
        )
    };
    if let Err(e) = r {
        let msg = err
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer() as *const u8,
                    b.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        bail!("D3DCompile failed: {e} {msg}");
    }
    blob.ok_or_else(|| anyhow!("D3DCompile returned no blob"))
}

/// Fullscreen-triangle pass with linear sampling, used when a source does not
/// match its piece of the physical display.
struct Stretch {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
}

impl Stretch {
    fn new(device: &ID3D11Device) -> Result<Self> {
        let vs_blob = compile_shader(s!("vs_main"), s!("vs_5_0"))?;
        let ps_blob = compile_shader(s!("ps_main"), s!("ps_5_0"))?;
        let bytes = |b: &ID3DBlob| unsafe {
            std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())
        };
        let mut vs = None;
        let mut ps = None;
        let mut sampler = None;
        unsafe {
            device.CreateVertexShader(bytes(&vs_blob), None, Some(&mut vs))?;
            device.CreatePixelShader(bytes(&ps_blob), None, Some(&mut ps))?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            device.CreateSamplerState(&sd, Some(&mut sampler))?;
        }
        Ok(Self {
            vs: vs.ok_or_else(|| anyhow!("no vertex shader"))?,
            ps: ps.ok_or_else(|| anyhow!("no pixel shader"))?,
            sampler: sampler.ok_or_else(|| anyhow!("no sampler"))?,
        })
    }

    /// Draw `srv` stretched over `vp` = (x, y, w, h) of the target `rtv`.
    fn draw(
        &self,
        ctx: &ID3D11DeviceContext,
        rtv: &ID3D11RenderTargetView,
        srv: &ID3D11ShaderResourceView,
        vp: (f32, f32, f32, f32),
    ) {
        unsafe {
            let vp = D3D11_VIEWPORT {
                TopLeftX: vp.0,
                TopLeftY: vp.1,
                Width: vp.2,
                Height: vp.3,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[vp]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.Draw(3, 0);
            // Unbind so the next copy / Present has no outstanding bindings.
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
        }
    }
}

/// Per-renderer GPU state: the stretch pass plus views cached per texture
/// (capture textures are persistent; back buffers rotate through a few).
#[derive(Default)]
struct Gpu {
    stretch: Option<Stretch>,
    rtvs: HashMap<usize, (ID3D11Texture2D, ID3D11RenderTargetView)>,
    srvs: HashMap<usize, (ID3D11Texture2D, ID3D11ShaderResourceView)>,
}

impl Gpu {
    fn rtv(
        &mut self,
        device: &ID3D11Device,
        tex: &ID3D11Texture2D,
    ) -> Result<ID3D11RenderTargetView> {
        let key = tex.as_raw() as usize;
        if let Some((_, v)) = self.rtvs.get(&key) {
            return Ok(v.clone());
        }
        if self.rtvs.len() > 16 {
            self.rtvs.clear();
        }
        let mut v = None;
        unsafe { device.CreateRenderTargetView(tex, None, Some(&mut v))? };
        let v = v.ok_or_else(|| anyhow!("no RTV"))?;
        // Keep the texture alive so its address cannot be reused by another one.
        self.rtvs.insert(key, (tex.clone(), v.clone()));
        Ok(v)
    }

    fn srv(
        &mut self,
        device: &ID3D11Device,
        tex: &ID3D11Texture2D,
    ) -> Result<ID3D11ShaderResourceView> {
        let key = tex.as_raw() as usize;
        if let Some((_, v)) = self.srvs.get(&key) {
            return Ok(v.clone());
        }
        if self.srvs.len() > 32 {
            self.srvs.clear();
        }
        let mut v = None;
        unsafe { device.CreateShaderResourceView(tex, None, Some(&mut v))? };
        let v = v.ok_or_else(|| anyhow!("no SRV"))?;
        self.srvs.insert(key, (tex.clone(), v.clone()));
        Ok(v)
    }

    /// Stretch `src` over `dest` of `target`.
    fn stretch(
        &mut self,
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        target: &ID3D11Texture2D,
        src: &ID3D11Texture2D,
        dest: Rect,
    ) -> Result<()> {
        if self.stretch.is_none() {
            self.stretch = Some(Stretch::new(device)?);
        }
        let rtv = self.rtv(device, target)?;
        let srv = self.srv(device, src)?;
        if let Some(s) = &self.stretch {
            s.draw(
                ctx,
                &rtv,
                &srv,
                (dest.x as f32, dest.y as f32, dest.w as f32, dest.h as f32),
            );
        }
        Ok(())
    }
}
