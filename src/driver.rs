//! Parsec VDD driver: status, official installer download + Authenticode
//! verification, launching the installer, opening links.
//!
//! The driver cannot be bundled (no redistribution grant), so the app only
//! helps the user get the official installer. The download uses WinHTTP (part
//! of Windows, already covered by the `windows` crate: no extra dependency,
//! system TLS stack and certificate store, system proxy settings).
//!
//! The downloaded file is run only after `verify_signature`: a valid
//! Authenticode signature (WinVerifyTrust) whose signer is "Parsec Cloud, Inc.".
use crate::vdd::parsec::ParsecVdd;
use anyhow::{anyhow, bail, Context, Result};
use std::ffi::c_void;
use std::io::Write;
use std::path::{Path, PathBuf};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::Security::Cryptography::{CertGetNameStringW, CERT_NAME_ATTR_TYPE};
use windows::Win32::Security::WinTrust::{
    WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CHOICE_FILE, WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
    WTD_UI_NONE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub const INSTALLER_URL: &str = "https://builds.parsec.app/vdd/parsec-vdd-0.45.0.0.exe";
pub const PROJECT_URL: &str = "https://github.com/nomi-san/parsec-vdd";
pub const SUPPORT_URL: &str =
    "https://support.parsec.app/hc/en-us/articles/32361359271444-VDD-Advanced-Configuration";
/// Required subject common name of the installer's signer.
pub const EXPECTED_SIGNER: &str = "Parsec Cloud, Inc.";

/// Refuse to download more than this (the real installer is about 0.5 MB).
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverStatus {
    /// The driver's device interface is present and can be opened.
    pub installed: bool,
    /// Minor version from the version IOCTL (when it could be read).
    pub version: Option<u32>,
    /// The "Parsec Virtual Display Adapter" device is present (same probe as `installed`).
    pub adapter_present: bool,
}

/// Probe the driver: open its device interface and ask for the version.
pub fn status() -> DriverStatus {
    match ParsecVdd::open() {
        Ok(v) => DriverStatus {
            installed: true,
            version: v.version().ok(),
            adapter_present: true,
        },
        Err(_) => DriverStatus {
            installed: false,
            version: None,
            adapter_present: false,
        },
    }
}

/// `%TEMP%\virtual-desktop-display`
pub fn default_download_dir() -> PathBuf {
    std::env::temp_dir().join("virtual-desktop-display")
}

// ---- URL handling (pure) ---------------------------------------------------

/// Split an https URL into (host, port, path-with-query).
pub fn split_https_url(url: &str) -> Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!("only https URLs are allowed: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>().map_err(|_| anyhow!("bad port in {url}"))?,
        ),
        None => (authority, INTERNET_DEFAULT_HTTPS_PORT),
    };
    if host.is_empty() || host.contains('@') {
        bail!("bad host in {url}");
    }
    Ok((host.to_string(), port, path.to_string()))
}

/// File name (last path segment) of a URL, sanitised.
pub fn file_name_of(url: &str) -> Result<String> {
    let (_, _, path) = split_https_url(url)?;
    let path = path.split(['?', '#']).next().unwrap_or("");
    let name = path.rsplit('/').next().unwrap_or("");
    if name.is_empty() || name.contains(['\\', ':']) || name.starts_with('.') {
        bail!("cannot derive a file name from {url}");
    }
    Ok(name.to_string())
}

/// True if `cn` is exactly the expected signer.
pub fn is_expected_signer(cn: &str) -> bool {
    cn == EXPECTED_SIGNER
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

// ---- download --------------------------------------------------------------

struct Internet(*mut c_void);
impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn check(h: *mut c_void, what: &str) -> Result<Internet> {
    if h.is_null() {
        bail!("{what} failed: {}", windows::core::Error::from_thread());
    }
    Ok(Internet(h))
}

/// Download `INSTALLER_URL` into `dest_dir` (created if needed) and return
/// the file path. The file is written under a `.part` name and renamed when
/// complete. The caller must still run `verify_signature` on the result.
pub fn download_installer(dest_dir: &Path) -> Result<PathBuf> {
    download_to(INSTALLER_URL, dest_dir)
}

fn download_to(url: &str, dest_dir: &Path) -> Result<PathBuf> {
    let (host, port, path) = split_https_url(url)?;
    let name = file_name_of(url)?;
    std::fs::create_dir_all(dest_dir).with_context(|| format!("create {}", dest_dir.display()))?;
    let dest = dest_dir.join(&name);
    let part = dest_dir.join(format!("{name}.part"));

    let whost = wide(&host);
    let wpath = wide(&path);
    let status = unsafe {
        let session = check(
            WinHttpOpen(
                w!("virtual-desktop-display"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        let conn = check(
            WinHttpConnect(session.0, PCWSTR(whost.as_ptr()), port, 0),
            "WinHttpConnect",
        )?;
        let req = check(
            WinHttpOpenRequest(
                conn.0,
                w!("GET"),
                PCWSTR(wpath.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ),
            "WinHttpOpenRequest",
        )?;
        WinHttpSendRequest(req.0, None, None, 0, 0, 0).context("WinHttpSendRequest")?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).context("WinHttpReceiveResponse")?;

        let mut code: u32 = 0;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut code as *mut u32 as *mut c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .context("WinHttpQueryHeaders")?;
        if code != 200 {
            bail!("download failed: HTTP {code}");
        }

        let mut file =
            std::fs::File::create(&part).with_context(|| format!("create {}", part.display()))?;
        let mut total: u64 = 0;
        let mut buf = vec![0u8; 64 * 1024];
        let res: Result<()> = (|| loop {
            let mut avail = 0u32;
            WinHttpQueryDataAvailable(req.0, &mut avail).context("WinHttpQueryDataAvailable")?;
            if avail == 0 {
                return Ok(());
            }
            let want = (avail as usize).min(buf.len());
            let mut read = 0u32;
            WinHttpReadData(
                req.0,
                buf.as_mut_ptr() as *mut c_void,
                want as u32,
                &mut read,
            )
            .context("WinHttpReadData")?;
            if read == 0 {
                return Ok(());
            }
            total += read as u64;
            if total > MAX_DOWNLOAD {
                bail!("download larger than {MAX_DOWNLOAD} bytes; aborted");
            }
            file.write_all(&buf[..read as usize])?;
        })();
        drop(file);
        if let Err(e) = res {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        total
    };
    if status == 0 {
        let _ = std::fs::remove_file(&part);
        bail!("download returned no data");
    }
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&part, &dest).with_context(|| format!("rename to {}", dest.display()))?;
    tracing::info!("downloaded {} ({status} bytes)", dest.display());
    Ok(dest)
}

// ---- signature -------------------------------------------------------------

/// Verify the Authenticode signature of `path` (WinVerifyTrust,
/// WINTRUST_ACTION_GENERIC_VERIFY_V2, revocation checked on the whole chain)
/// AND that the signer's subject common name is "Parsec Cloud, Inc.".
/// Returns the signer name.
pub fn verify_signature(path: &Path) -> Result<String> {
    let wpath = wide(&path.to_string_lossy());
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wpath.as_ptr()),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let hwnd = HWND(-1isize as *mut c_void); // INVALID_HANDLE_VALUE: no UI
    let r = unsafe { WinVerifyTrust(hwnd, &mut action, &mut data as *mut _ as *mut c_void) };
    let result = if r != 0 {
        Err(anyhow!(
            "signature check failed for {} (WinVerifyTrust {:#010x})",
            path.display(),
            r as u32
        ))
    } else {
        signer_name(&data)
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(hwnd, &mut action, &mut data as *mut _ as *mut c_void);
    }
    let signer = result?;
    if !is_expected_signer(&signer) {
        bail!("signed by {signer:?}, expected {EXPECTED_SIGNER:?}");
    }
    Ok(signer)
}

/// Subject common name of the leaf signer certificate of a verified state.
fn signer_name(data: &WINTRUST_DATA) -> Result<String> {
    unsafe {
        let prov = WTHelperProvDataFromStateData(data.hWVTStateData);
        if prov.is_null() {
            bail!("no provider data after verification");
        }
        let sgnr = WTHelperGetProvSignerFromChain(prov, 0, false, 0);
        if sgnr.is_null() || (*sgnr).csCertChain == 0 || (*sgnr).pasCertChain.is_null() {
            bail!("no signer in the signature");
        }
        let cert = (*(*sgnr).pasCertChain).pCert;
        if cert.is_null() {
            bail!("no signer certificate");
        }
        // Subject (not issuer) common name.
        let oid = windows::core::s!("2.5.4.3"); // szOID_COMMON_NAME
        let n = CertGetNameStringW(
            cert,
            CERT_NAME_ATTR_TYPE,
            0,
            Some(oid.as_ptr() as *const c_void),
            None,
        );
        if n <= 1 {
            bail!("signer certificate has no common name");
        }
        let mut buf = vec![0u16; n as usize];
        CertGetNameStringW(
            cert,
            CERT_NAME_ATTR_TYPE,
            0,
            Some(oid.as_ptr() as *const c_void),
            Some(&mut buf),
        );
        Ok(String::from_utf16_lossy(&buf[..(n as usize - 1)]))
    }
}

// ---- launching -------------------------------------------------------------

/// Start the installer (it requests elevation itself; the UAC prompt appears).
/// Call only after `verify_signature` succeeded.
pub fn launch_installer(path: &Path) -> Result<()> {
    shell_open(&path.to_string_lossy())
}

/// Open a web page in the default browser (https only).
pub fn open_url(url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        bail!("only https URLs can be opened: {url}");
    }
    shell_open(url)
}

fn shell_open(target: &str) -> Result<()> {
    let t = wide(target);
    let r = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(t.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a value > 32 on success.
    if (r.0 as isize) <= 32 {
        bail!("ShellExecute({target}) failed: code {}", r.0 as isize);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_split() {
        let (h, p, path) = split_https_url(INSTALLER_URL).unwrap();
        assert_eq!(h, "builds.parsec.app");
        assert_eq!(p, 443);
        assert_eq!(path, "/vdd/parsec-vdd-0.45.0.0.exe");
        let (h, p, path) = split_https_url("https://example.com:8443").unwrap();
        assert_eq!((h.as_str(), p, path.as_str()), ("example.com", 8443, "/"));
    }

    #[test]
    fn url_rejects_non_https_and_tricks() {
        assert!(split_https_url("http://builds.parsec.app/x.exe").is_err());
        assert!(split_https_url("https://user@evil.com/x.exe").is_err());
        assert!(split_https_url("https:///x.exe").is_err());
        assert!(open_url("file:///c:/windows/system32/calc.exe").is_err());
    }

    #[test]
    fn file_name() {
        assert_eq!(
            file_name_of(INSTALLER_URL).unwrap(),
            "parsec-vdd-0.45.0.0.exe"
        );
        assert_eq!(file_name_of("https://a.b/x/y.exe?z=1").unwrap(), "y.exe");
        assert!(file_name_of("https://a.b/").is_err());
        assert!(file_name_of("https://a.b/..").is_err());
    }

    #[test]
    fn signer_must_match_exactly() {
        assert!(is_expected_signer("Parsec Cloud, Inc."));
        assert!(!is_expected_signer("Parsec Cloud, Inc"));
        assert!(!is_expected_signer("parsec cloud, inc."));
        assert!(!is_expected_signer("Evil Parsec Cloud, Inc."));
    }

    #[test]
    fn unsigned_file_is_rejected() {
        // The test executable of this crate is not Authenticode signed.
        let exe = std::env::current_exe().unwrap();
        assert!(verify_signature(&exe).is_err());
    }
}
