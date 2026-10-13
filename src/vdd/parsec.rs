//! parsec-vdd backend. Faithful port of `core/parsec-vdd.h`
//! (https://github.com/nomi-san/parsec-vdd, BSD-2-Clause, (c) 2023 Nguyen Duy).
//!
//! Displays exist only while something sends UPDATE at least every ~100 ms,
//! so a keep-alive thread pings every 50 ms. Dropping `ParsecVdd` removes the
//! displays and stops the thread; if the process dies the driver unplugs them.
use super::VirtualDisplays;
use anyhow::{anyhow, bail, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use windows::core::{GUID, PCSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsA,
    SetupDiGetDeviceInterfaceDetailA, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_A,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileA, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_NO_BUFFERING, FILE_FLAG_OVERLAPPED,
    FILE_FLAG_WRITE_THROUGH, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Threading::CreateEventA;
use windows::Win32::System::IO::{
    CancelIoEx, DeviceIoControl, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};

/// Adapter interface GUID {00b41627-04c4-429e-a26e-0265cf50c8fa}.
pub const VDD_ADAPTER_GUID: GUID = GUID::from_u128(0x00b41627_04c4_429e_a26e_0265cf50c8fa);
/// Hardware id of the driver (for status queries).
pub const VDD_HARDWARE_ID: &str = "Root\\Parsec\\VDA";
/// Monitor friendly name shown by Windows.
pub const VDD_DISPLAY_NAME: &str = "ParsecVDA";
/// Display id seen in HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY.
pub const VDD_DISPLAY_ID: &str = "PSCCDD0";
/// Header uses 8 (driver supports 16) to avoid plugging lag.
pub const VDD_MAX_DISPLAYS: usize = 8;

pub const VDD_IOCTL_ADD: u32 = 0x0022e004;
pub const VDD_IOCTL_REMOVE: u32 = 0x0022a008;
pub const VDD_IOCTL_UPDATE: u32 = 0x0022a00c;
pub const VDD_IOCTL_VERSION: u32 = 0x0022e010;

const IOCTL_TIMEOUT_MS: u32 = 5000;
const KEEPALIVE: Duration = Duration::from_millis(50);
const ERROR_IO_PENDING: i32 = 997;

/// Owned device handle, usable from several threads (each IOCTL has its own
/// OVERLAPPED + event).
struct DevHandle(HANDLE);
unsafe impl Send for DevHandle {}
unsafe impl Sync for DevHandle {}
impl Drop for DevHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// `OpenDeviceHandle`: enumerate present interfaces of the adapter GUID and open the first that works.
fn open_device_handle() -> Result<DevHandle> {
    unsafe {
        let devinfo = SetupDiGetClassDevsA(
            Some(&VDD_ADAPTER_GUID),
            PCSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
        .map_err(|e| anyhow!("SetupDiGetClassDevs failed: {e}"))?;

        let mut found: Option<HANDLE> = None;
        let mut i = 0u32;
        loop {
            let mut iface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(devinfo, None, &VDD_ADAPTER_GUID, i, &mut iface).is_err()
            {
                break;
            }
            i += 1;

            let mut detail_size = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailA(
                devinfo,
                &iface,
                None,
                0,
                Some(&mut detail_size),
                None,
            );
            if detail_size == 0 {
                continue;
            }
            // u64 buffer for alignment.
            let mut buf = vec![0u64; (detail_size as usize).div_ceil(8)];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_A;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_A>() as u32;
            if SetupDiGetDeviceInterfaceDetailA(
                devinfo,
                &iface,
                Some(detail),
                detail_size,
                Some(&mut detail_size),
                None,
            )
            .is_err()
            {
                continue;
            }
            let path = PCSTR((*detail).DevicePath.as_ptr() as *const u8);
            if let Ok(h) = CreateFileA(
                path,
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL
                    | FILE_FLAG_NO_BUFFERING
                    | FILE_FLAG_OVERLAPPED
                    | FILE_FLAG_WRITE_THROUGH,
                None,
            ) {
                if !h.is_invalid() {
                    found = Some(h);
                    break;
                }
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(devinfo);
        found
            .map(DevHandle)
            .ok_or_else(|| anyhow!("parsec-vdd device interface not found (driver not installed?)"))
    }
}

/// `VddIoControl`: fixed 32-byte zero-padded input buffer, DWORD output, 5 s timeout.
/// Returns the output DWORD.
fn vdd_ioctl(dev: &DevHandle, code: u32, data: &[u8]) -> Result<u32> {
    let t0 = std::time::Instant::now();
    let r = vdd_ioctl_inner(dev, code, data);
    let ms = t0.elapsed().as_millis();
    // Keep-alive pings are too chatty to log unless slow.
    if code != VDD_IOCTL_UPDATE {
        tracing::info!("parsec-vdd ioctl {code:#x} took {ms} ms -> {r:?}");
    } else if ms > 200 {
        tracing::warn!("parsec-vdd keep-alive UPDATE took {ms} ms");
    }
    r
}

fn vdd_ioctl_inner(dev: &DevHandle, code: u32, data: &[u8]) -> Result<u32> {
    let mut in_buf = [0u8; 32];
    let n = data.len().min(in_buf.len());
    in_buf[..n].copy_from_slice(&data[..n]);
    let mut out: u32 = 0;
    let mut transferred: u32 = 0;
    unsafe {
        let event = CreateEventA(None, true, false, PCSTR::null())?;
        let mut ov = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        let res = DeviceIoControl(
            dev.0,
            code,
            Some(in_buf.as_ptr() as *const _),
            in_buf.len() as u32,
            Some(&mut out as *mut u32 as *mut _),
            size_of::<u32>() as u32,
            None,
            Some(&mut ov),
        );
        if let Err(e) = res {
            if e.code().0 & 0xFFFF != ERROR_IO_PENDING {
                let _ = CloseHandle(event);
                bail!("DeviceIoControl {code:#x} failed: {e}");
            }
        }
        if let Err(e) = GetOverlappedResultEx(dev.0, &ov, &mut transferred, IOCTL_TIMEOUT_MS, false)
        {
            // Make sure the kernel is done with our stack buffers before returning.
            let _ = CancelIoEx(dev.0, Some(&ov));
            let _ = GetOverlappedResult(dev.0, &ov, &mut transferred, true);
            let _ = CloseHandle(event);
            bail!("ioctl {code:#x} timed out or failed: {e}");
        }
        let _ = CloseHandle(event);
    }
    Ok(out)
}

fn vdd_update(dev: &DevHandle) {
    let _ = vdd_ioctl(dev, VDD_IOCTL_UPDATE, &[]);
}

/// Index encoding used by REMOVE: 16-bit big-endian.
pub fn remove_index_bytes(index: u32) -> [u8; 2] {
    let v: u16 = (((index & 0xFF) << 8) | ((index >> 8) & 0xFF)) as u16;
    v.to_le_bytes()
}

pub struct ParsecVdd {
    dev: Arc<DevHandle>,
    added: Vec<u32>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ParsecVdd {
    /// Open the driver and start the keep-alive thread.
    pub fn open() -> Result<Self> {
        let dev = Arc::new(open_device_handle()?);
        let stop = Arc::new(AtomicBool::new(false));
        let (d, s) = (dev.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("vdd-keepalive".into())
            .spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    vdd_update(&d);
                    std::thread::sleep(KEEPALIVE);
                }
            })?;
        Ok(Self {
            dev,
            added: Vec::new(),
            stop,
            thread: Some(thread),
        })
    }

    /// Minor driver version (VDD_IOCTL_VERSION).
    pub fn version(&self) -> Result<u32> {
        vdd_ioctl(&self.dev, VDD_IOCTL_VERSION, &[])
    }
}

impl VirtualDisplays for ParsecVdd {
    fn add(&mut self, n: usize) -> Result<Vec<u32>> {
        if self.added.len() + n > VDD_MAX_DISPLAYS {
            bail!("too many virtual displays (max {VDD_MAX_DISPLAYS})");
        }
        let mut out = Vec::new();
        for _ in 0..n {
            let idx = vdd_ioctl(&self.dev, VDD_IOCTL_ADD, &[])?;
            vdd_update(&self.dev);
            tracing::info!("parsec-vdd: added display index {idx}");
            self.added.push(idx);
            out.push(idx);
        }
        Ok(out)
    }

    fn remove_all(&mut self) {
        for idx in std::mem::take(&mut self.added) {
            if let Err(e) = vdd_ioctl(&self.dev, VDD_IOCTL_REMOVE, &remove_index_bytes(idx)) {
                tracing::warn!("parsec-vdd: remove {idx} failed: {e}");
            }
            vdd_update(&self.dev);
            tracing::info!("parsec-vdd: removed display index {idx}");
        }
    }
}

impl Drop for ParsecVdd {
    fn drop(&mut self) {
        self.remove_all();
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_index_is_big_endian_u16() {
        assert_eq!(remove_index_bytes(0), [0, 0]);
        assert_eq!(remove_index_bytes(1), [0, 1]);
        assert_eq!(remove_index_bytes(0x0102), [0x01, 0x02]);
    }

    #[test]
    fn guid_matches_header() {
        assert_eq!(
            format!("{:?}", VDD_ADAPTER_GUID).to_lowercase(),
            "00b41627-04c4-429e-a26e-0265cf50c8fa"
        );
    }
}
