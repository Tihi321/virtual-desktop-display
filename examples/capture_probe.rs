//! Phase 0 probe: capture one frame of a monitor with WGC and save a PNG to
//! .claude/temp/capture_probe.png. Usage: capture_probe [monitor_index]
//! (default: first non-primary monitor).
use std::sync::Arc;
use std::time::{Duration, Instant};
use vdd_app::capture::{FrameEvent, MonitorCapture};
use vdd_app::displays;
use vdd_app::renderer::create_device;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::System::Threading::WaitForSingleObject;

fn main() -> anyhow::Result<()> {
    vdd_app::set_dpi_aware();
    let targets = displays::enumerate()?;
    for (i, t) in targets.iter().enumerate() {
        println!(
            "[{i}] {:?} {} {:?} primary={}",
            t.friendly_name, t.gdi_name, t.rect, t.primary
        );
    }
    let t = match std::env::args().nth(1) {
        Some(a) => targets
            .get(a.parse::<usize>()?)
            .ok_or_else(|| anyhow::anyhow!("bad index"))?,
        None => targets
            .iter()
            .find(|t| !t.primary)
            .ok_or_else(|| anyhow::anyhow!("no non-primary monitor"))?,
    };
    println!("capturing {:?} {}", t.friendly_name, t.gdi_name);
    let d3d = create_device()?;
    let ev = FrameEvent::new()?;
    let cap = MonitorCapture::start(&d3d.device, &d3d.context, t.hmonitor, Arc::clone(&ev))?;
    let start = Instant::now();
    let tex = loop {
        if let Some(tx) = cap.latest() {
            break tx;
        }
        if start.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("no frame within 5 s");
        }
        unsafe { WaitForSingleObject(ev.0, 200) };
    };
    println!("first frame after {:?}", start.elapsed());

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut desc) };
    let sdesc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..desc
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    unsafe {
        d3d.device
            .CreateTexture2D(&sdesc, None, Some(&mut staging))?
    };
    let staging = staging.unwrap();
    unsafe { d3d.context.CopyResource(&staging, &tex) };
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        d3d.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?
    };
    let (w, h) = (desc.Width as usize, desc.Height as usize);
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = unsafe {
            std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), w * 4)
        };
        for x in 0..w {
            let (b, g, r, a) = (row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]);
            rgba[(y * w + x) * 4..][..4].copy_from_slice(&[r, g, b, a]);
        }
    }
    unsafe { d3d.context.Unmap(&staging, 0) };

    std::fs::create_dir_all(".claude/temp")?;
    let file = std::fs::File::create(".claude/temp/capture_probe.png")?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    println!("wrote .claude/temp/capture_probe.png ({w}x{h})");
    Ok(())
}
