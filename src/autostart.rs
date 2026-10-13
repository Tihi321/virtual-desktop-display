//! "Start with Windows" via HKCU\Software\Microsoft\Windows\CurrentVersion\Run.
use anyhow::{bail, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SAM_FLAGS, REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("VirtualDesktopDisplay");

fn open(access: REG_SAM_FLAGS) -> Result<HKEY> {
    let mut key = HKEY::default();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, access, &mut key) };
    if r != ERROR_SUCCESS {
        bail!("RegOpenKeyExW failed: {}", r.0);
    }
    Ok(key)
}

/// The Run value: the quoted exe path.
pub fn command_line() -> Result<String> {
    Ok(format!("\"{}\"", std::env::current_exe()?.display()))
}

pub fn is_enabled() -> bool {
    let Ok(key) = open(KEY_QUERY_VALUE) else {
        return false;
    };
    let r = unsafe { RegQueryValueExW(key, VALUE, None, None, None, None) };
    unsafe {
        let _ = RegCloseKey(key);
    }
    r == ERROR_SUCCESS
}

pub fn set_enabled(enable: bool) -> Result<()> {
    let key = open(KEY_SET_VALUE)?;
    let r = if enable {
        let wide: Vec<u16> = command_line()?.encode_utf16().chain(Some(0)).collect();
        let bytes =
            unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
        unsafe { RegSetValueExW(key, VALUE, None, REG_SZ, Some(bytes)) }
    } else {
        let r = unsafe { RegDeleteValueW(key, VALUE) };
        // Deleting a value that does not exist is fine.
        if r == ERROR_FILE_NOT_FOUND {
            ERROR_SUCCESS
        } else {
            r
        }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    if r != ERROR_SUCCESS {
        bail!("registry update failed: {}", r.0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_line_is_quoted() {
        let c = super::command_line().unwrap();
        assert!(c.starts_with('"') && c.ends_with('"'));
    }
}
