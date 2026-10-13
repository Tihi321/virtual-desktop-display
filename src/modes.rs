//! Parsec VDD display modes.
//!
//! The driver has a built-in list of modes (0.45) and room for 5 custom ones
//! stored in `HKLM\SOFTWARE\Parsec\vdd\{0..4}` (DWORDs `width`, `height`,
//! `hz`). The driver reads them whenever a display is added, no restart
//! needed. Reading is possible without admin rights; writing is done only by
//! the elevated helper (`--register-modes`), see `request_register`.
//!
//! Everything except the registry / elevation functions is pure and tested.
use crate::config::SplitConfig;
use crate::geometry;
use anyhow::{anyhow, bail, Result};
use std::path::Path;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WAIT_OBJECT_0,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, KEY_WRITE, REG_DWORD,
    REG_OPEN_CREATE_OPTIONS, REG_VALUE_TYPE,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// Number of custom mode slots in the driver.
pub const MAX_CUSTOM: usize = 5;
/// Registry key of the driver settings (under HKLM).
const VDD_KEY: &str = "SOFTWARE\\Parsec\\vdd";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mode {
    pub w: u32,
    pub h: u32,
    pub hz: u32,
}

impl Mode {
    pub const fn new(w: u32, h: u32, hz: u32) -> Self {
        Self { w, h, hz }
    }
    fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}@{}", self.w, self.h, self.hz)
    }
}

/// Built-in sizes of parsec-vdd 0.45 (all offered at 60 Hz), largest first.
pub const BUILTIN: [Mode; 27] = [
    Mode::new(4096, 2160, 60),
    Mode::new(3840, 2160, 60),
    Mode::new(3840, 1600, 60),
    Mode::new(3840, 1080, 60),
    Mode::new(3440, 1440, 60),
    Mode::new(3240, 2160, 60),
    Mode::new(3200, 1800, 60),
    Mode::new(3000, 2000, 60),
    Mode::new(2880, 1800, 60),
    Mode::new(2880, 1620, 60),
    Mode::new(2736, 1824, 60),
    Mode::new(2560, 1600, 60),
    Mode::new(2560, 1440, 60),
    Mode::new(2560, 1080, 60),
    Mode::new(2496, 1664, 60),
    Mode::new(2256, 1504, 60),
    Mode::new(2048, 1152, 60),
    Mode::new(1920, 1200, 60),
    Mode::new(1920, 1080, 60),
    Mode::new(1800, 1200, 60),
    Mode::new(1680, 1050, 60),
    Mode::new(1600, 1200, 60),
    Mode::new(1600, 900, 60),
    Mode::new(1440, 900, 60),
    Mode::new(1366, 768, 60),
    Mode::new(1280, 800, 60),
    Mode::new(1280, 720, 60),
];

/// True if the driver offers `m` out of the box (built-in sizes: 60 Hz only).
pub fn is_builtin(m: Mode) -> bool {
    m.hz == 60 && BUILTIN.iter().any(|b| b.w == m.w && b.h == m.h)
}

fn push_unique(v: &mut Vec<Mode>, m: Mode) {
    if !v.contains(&m) {
        v.push(m);
    }
}

/// Unique virtual display modes (piece size + refresh) needed by the enabled
/// splits. Each entry is a split with the size of the display it applies to.
pub fn required(splits: &[(SplitConfig, (i32, i32))]) -> Vec<Mode> {
    let mut out = Vec::new();
    for (sc, (dw, dh)) in splits {
        if !sc.enabled || *dw <= 0 || *dh <= 0 {
            continue;
        }
        let hz = sc.refresh_hz.max(1);
        for p in geometry::pieces(*dw, *dh, &sc.spec()) {
            if p.w > 0 && p.h > 0 {
                push_unique(&mut out, Mode::new(p.w as u32, p.h as u32, hz));
            }
        }
    }
    out
}

/// How the required modes map onto the driver's mode slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotPlan {
    /// Required modes the driver already has built in.
    pub builtin: Vec<Mode>,
    /// Required modes already registered as custom modes.
    pub registered: Vec<Mode>,
    /// Required modes that still have to be written (a free or replaceable slot exists).
    pub to_register: Vec<Mode>,
    /// Required modes that fit nowhere (more than 5 custom sizes): use `fallback_for`.
    pub overflow: Vec<Mode>,
    /// The final custom list (at most 5) to write: everything needed first,
    /// then unneeded existing entries only while slots are left over.
    pub new_registry: Vec<Mode>,
}

impl SlotPlan {
    /// True if the registry has to be rewritten.
    pub fn needs_write(&self) -> bool {
        !self.to_register.is_empty()
    }
}

pub fn plan_slots(required: &[Mode], current_custom: &[Mode]) -> SlotPlan {
    let mut current: Vec<Mode> = Vec::new();
    for m in current_custom {
        push_unique(&mut current, *m);
    }
    current.truncate(MAX_CUSTOM);

    let mut builtin = Vec::new();
    let mut registered = Vec::new();
    let mut missing = Vec::new();
    for m in required {
        if is_builtin(*m) {
            push_unique(&mut builtin, *m);
        } else if current.contains(m) {
            push_unique(&mut registered, *m);
        } else {
            push_unique(&mut missing, *m);
        }
    }

    let room = MAX_CUSTOM - registered.len();
    let mut to_register = missing.clone();
    let overflow = if to_register.len() > room {
        to_register.split_off(room)
    } else {
        Vec::new()
    };

    let mut new_registry: Vec<Mode> = registered.clone();
    new_registry.extend(to_register.iter().copied());
    for m in &current {
        if new_registry.len() >= MAX_CUSTOM {
            break;
        }
        push_unique(&mut new_registry, *m);
    }
    SlotPlan {
        builtin,
        registered,
        to_register,
        overflow,
        new_registry,
    }
}

/// The mode to use when `m` is not available: the built-in size with the
/// smallest area that is at least as wide and as high; the largest built-in
/// one if none is. The renderer stretches the piece. Always 60 Hz.
pub fn fallback_for(m: Mode) -> Mode {
    BUILTIN
        .iter()
        .filter(|b| b.w >= m.w && b.h >= m.h)
        .min_by_key(|b| b.area())
        .or_else(|| BUILTIN.iter().max_by_key(|b| b.area()))
        .copied()
        .unwrap_or(BUILTIN[0])
}

/// The mode the virtual display will really get for the wanted `m`, given the
/// custom modes in the registry right now: exact if built-in or registered,
/// else the fallback. The bool is true for an exact match.
pub fn actual_for(m: Mode, registered: &[Mode]) -> (Mode, bool) {
    if is_builtin(m) || registered.contains(&m) {
        (m, true)
    } else {
        (fallback_for(m), false)
    }
}

// ---- CLI encoding ----------------------------------------------------------

/// "1720x1440@60,1280x1440@60"
pub fn encode(modes: &[Mode]) -> String {
    modes
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_one(s: &str) -> Result<Mode> {
    let (size, hz) = s
        .trim()
        .split_once('@')
        .ok_or_else(|| anyhow!("mode {s:?}: expected WxH@HZ"))?;
    let (w, h) = size
        .split_once('x')
        .ok_or_else(|| anyhow!("mode {s:?}: expected WxH@HZ"))?;
    let num = |t: &str| -> Result<u32> {
        t.trim()
            .parse::<u32>()
            .map_err(|_| anyhow!("mode {s:?}: bad number {t:?}"))
    };
    let m = Mode::new(num(w)?, num(h)?, num(hz)?);
    if !(320..=16384).contains(&m.w) || !(200..=16384).contains(&m.h) || !(1..=500).contains(&m.hz)
    {
        bail!("mode {s:?}: out of range");
    }
    Ok(m)
}

/// Parse and validate a list (at most 5 modes, no duplicates). The elevated
/// helper writes whatever this accepts, so it is deliberately strict.
pub fn decode(s: &str) -> Result<Vec<Mode>> {
    let mut out = Vec::new();
    for part in s.split(',').filter(|p| !p.trim().is_empty()) {
        let m = parse_one(part)?;
        if out.contains(&m) {
            bail!("duplicate mode {m}");
        }
        out.push(m);
    }
    if out.len() > MAX_CUSTOM {
        bail!("at most {MAX_CUSTOM} custom modes, got {}", out.len());
    }
    Ok(out)
}

// ---- registry --------------------------------------------------------------

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn slot_path(i: usize) -> Vec<u16> {
    wide(&format!("{VDD_KEY}\\{i}"))
}

fn read_dword(key: HKEY, name: PCWSTR) -> Option<u32> {
    let mut data = [0u8; 4];
    let mut len = 4u32;
    let mut ty = REG_VALUE_TYPE::default();
    let r = unsafe {
        RegQueryValueExW(
            key,
            name,
            None,
            Some(&mut ty),
            Some(data.as_mut_ptr()),
            Some(&mut len),
        )
    };
    (r == ERROR_SUCCESS && ty == REG_DWORD && len == 4).then(|| u32::from_le_bytes(data))
}

/// The custom modes currently registered (read-only; no admin rights needed).
/// Missing keys are not an error.
pub fn read_custom_modes() -> Result<Vec<Mode>> {
    let mut out = Vec::new();
    for i in 0..MAX_CUSTOM {
        let path = slot_path(i);
        let mut key = HKEY::default();
        let r = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(path.as_ptr()),
                None,
                KEY_READ | KEY_WOW64_64KEY,
                &mut key,
            )
        };
        if r == ERROR_FILE_NOT_FOUND {
            continue;
        }
        if r != ERROR_SUCCESS {
            bail!("RegOpenKeyExW(HKLM\\{VDD_KEY}\\{i}) failed: {}", r.0);
        }
        let m = (|| {
            Some(Mode::new(
                read_dword(key, w!("width"))?,
                read_dword(key, w!("height"))?,
                read_dword(key, w!("hz"))?,
            ))
        })();
        unsafe {
            let _ = RegCloseKey(key);
        }
        if let Some(m) = m.filter(|m| m.w > 0 && m.h > 0) {
            out.push(m);
        }
    }
    Ok(out)
}

fn set_dword(key: HKEY, name: PCWSTR, v: u32) -> Result<()> {
    let r = unsafe { RegSetValueExW(key, name, None, REG_DWORD, Some(&v.to_le_bytes())) };
    if r != ERROR_SUCCESS {
        bail!("RegSetValueExW failed: {}", r.0);
    }
    Ok(())
}

/// Write `modes` to slots `0..n` and delete the remaining slots of `0..4`.
/// Needs admin rights: only call this from the elevated `--register-modes`
/// helper.
pub fn write_custom_modes(modes: &[Mode]) -> Result<()> {
    if modes.len() > MAX_CUSTOM {
        bail!("at most {MAX_CUSTOM} custom modes");
    }
    for (i, m) in modes.iter().enumerate() {
        let path = slot_path(i);
        let mut key = HKEY::default();
        let r = unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(path.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPEN_CREATE_OPTIONS(0),
                KEY_WRITE | KEY_WOW64_64KEY,
                None,
                &mut key,
                None,
            )
        };
        if r != ERROR_SUCCESS {
            bail!("RegCreateKeyExW(HKLM\\{VDD_KEY}\\{i}) failed: {}", r.0);
        }
        let res = set_dword(key, w!("width"), m.w)
            .and_then(|_| set_dword(key, w!("height"), m.h))
            .and_then(|_| set_dword(key, w!("hz"), m.hz));
        unsafe {
            let _ = RegCloseKey(key);
        }
        res?;
    }
    // Remove the slots that are no longer used.
    let mut parent = HKEY::default();
    let ppath = wide(VDD_KEY);
    let r = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(ppath.as_ptr()),
            None,
            KEY_WRITE | KEY_WOW64_64KEY,
            &mut parent,
        )
    };
    if r != ERROR_SUCCESS {
        bail!("RegOpenKeyExW(HKLM\\{VDD_KEY}) failed: {}", r.0);
    }
    let mut result = Ok(());
    for i in modes.len()..MAX_CUSTOM {
        let name = wide(&i.to_string());
        let r = unsafe { RegDeleteTreeW(parent, PCWSTR(name.as_ptr())) };
        if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
            result = Err(anyhow!("deleting slot {i} failed: {}", r.0));
        }
    }
    unsafe {
        let _ = RegCloseKey(parent);
    }
    result
}

// ---- elevation -------------------------------------------------------------

/// Run the current exe elevated (UAC prompt) as `--register-modes <list>` and
/// wait for it. `Ok(true)`: the helper succeeded; `Ok(false)`: the user
/// cancelled UAC or the helper failed.
pub fn request_register(modes: &[Mode]) -> Result<bool> {
    let exe = std::env::current_exe()?;
    run_elevated_register(&exe, &encode(modes))
}

fn run_elevated_register(exe: &Path, list: &str) -> Result<bool> {
    let file = wide(&exe.to_string_lossy());
    let params = wide(&format!("--register-modes \"{list}\""));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        if e.code() == ERROR_CANCELLED.to_hresult() {
            tracing::info!("UAC prompt cancelled");
            return Ok(false);
        }
        return Err(anyhow!("elevation failed: {e}"));
    }
    let h = info.hProcess;
    if h.is_invalid() {
        bail!("no process handle from ShellExecuteEx");
    }
    let wait = unsafe { WaitForSingleObject(h, INFINITE) };
    let mut code = 1u32;
    let ok = wait == WAIT_OBJECT_0 && unsafe { GetExitCodeProcess(h, &mut code) }.is_ok();
    unsafe {
        let _ = CloseHandle(h);
    }
    Ok(ok && code == 0)
}

/// Body of the elevated `--register-modes <list>` helper; returns the exit code.
pub fn register_helper(list: &str) -> i32 {
    let modes = match decode(list) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("--register-modes: {e:#}");
            return 2;
        }
    };
    match write_custom_modes(&modes) {
        Ok(()) => {
            tracing::info!("registered custom modes: {}", encode(&modes));
            0
        }
        Err(e) => {
            tracing::error!("--register-modes failed: {e:#}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LayoutKind;

    fn split(layout: LayoutKind, cols: u32, rows: u32, hz: u32) -> SplitConfig {
        SplitConfig {
            layout,
            columns: cols,
            rows,
            refresh_hz: hz,
            order: Vec::new(),
            ..Default::default()
        }
    }

    fn m(w: u32, h: u32) -> Mode {
        Mode::new(w, h, 60)
    }

    #[test]
    fn builtin_list() {
        assert_eq!(BUILTIN.len(), 27);
        assert!(is_builtin(m(1920, 1080)));
        assert!(is_builtin(m(3440, 1440)));
        assert!(!is_builtin(m(1720, 1440)));
        assert!(!is_builtin(Mode::new(1920, 1080, 144)));
    }

    #[test]
    fn required_unique_and_skips_disabled() {
        let a = split(LayoutKind::Rows, 1, 2, 60);
        let mut b = split(LayoutKind::Columns, 2, 1, 60);
        let mut off = split(LayoutKind::Columns, 4, 1, 60);
        off.enabled = false;
        // 1920x2160 rows -> 2x 1920x1080; 3440x1440 columns -> 2x 1720x1440
        let req = required(&[(a.clone(), (1920, 2160)), (b.clone(), (3440, 1440))]);
        assert_eq!(req, vec![m(1920, 1080), m(1720, 1440)]);
        b.enabled = true;
        let req = required(&[(a, (1920, 2160)), (off, (3440, 1440))]);
        assert_eq!(req, vec![m(1920, 1080)]);
    }

    #[test]
    fn required_keeps_hz() {
        let a = split(LayoutKind::Columns, 2, 1, 120);
        assert_eq!(
            required(&[(a, (3840, 1080))]),
            vec![Mode::new(1920, 1080, 120)]
        );
    }

    #[test]
    fn slots_builtin_registered_missing() {
        let req = [m(1920, 1080), m(1720, 1440), m(1280, 1440)];
        let p = plan_slots(&req, &[m(1720, 1440), m(999, 999)]);
        assert_eq!(p.builtin, vec![m(1920, 1080)]);
        assert_eq!(p.registered, vec![m(1720, 1440)]);
        assert_eq!(p.to_register, vec![m(1280, 1440)]);
        assert!(p.overflow.is_empty());
        // needed first, the unneeded entry kept because there is room
        assert_eq!(
            p.new_registry,
            vec![m(1720, 1440), m(1280, 1440), m(999, 999)]
        );
        assert!(p.needs_write());
    }

    #[test]
    fn slots_nothing_to_do() {
        let p = plan_slots(&[m(1920, 1080), m(1720, 1440)], &[m(1720, 1440)]);
        assert!(!p.needs_write());
        assert!(p.overflow.is_empty());
    }

    #[test]
    fn slots_replace_unneeded_when_full() {
        let cur = [
            m(1001, 1001),
            m(1002, 1002),
            m(1003, 1003),
            m(1004, 1004),
            m(1720, 1440),
        ];
        let req = [m(1720, 1440), m(1281, 1440), m(1282, 1440)];
        let p = plan_slots(&req, &cur);
        assert_eq!(p.to_register, vec![m(1281, 1440), m(1282, 1440)]);
        assert_eq!(p.new_registry.len(), MAX_CUSTOM);
        assert_eq!(
            &p.new_registry[..3],
            &[m(1720, 1440), m(1281, 1440), m(1282, 1440)]
        );
        assert_eq!(&p.new_registry[3..], &[m(1001, 1001), m(1002, 1002)]);
    }

    #[test]
    fn slots_overflow_beyond_five() {
        let req: Vec<Mode> = (0..7).map(|i| m(1301 + i, 1000)).collect();
        let p = plan_slots(&req, &[]);
        assert_eq!(p.to_register.len(), 5);
        assert_eq!(p.overflow, vec![m(1306, 1000), m(1307, 1000)]);
        assert_eq!(p.new_registry, p.to_register);
    }

    #[test]
    fn slots_registered_have_priority_on_overflow() {
        let mut req: Vec<Mode> = (0..6).map(|i| m(1301 + i, 1000)).collect();
        req.push(m(1500, 1000));
        let p = plan_slots(&req, &[m(1500, 1000)]);
        assert_eq!(p.registered, vec![m(1500, 1000)]);
        assert_eq!(p.to_register.len(), 4);
        assert_eq!(p.overflow.len(), 2);
        assert_eq!(p.new_registry[0], m(1500, 1000));
    }

    #[test]
    fn fallback_nearest_larger() {
        assert_eq!(fallback_for(m(1720, 1440)), m(2256, 1504));
        assert_eq!(fallback_for(m(1280, 1440)), m(2256, 1504));
        assert_eq!(fallback_for(m(1000, 700)), m(1280, 720));
        // taller than anything: largest built-in
        assert_eq!(fallback_for(m(5000, 5000)), m(4096, 2160));
        // hz is always 60
        assert_eq!(fallback_for(Mode::new(1720, 1440, 144)).hz, 60);
    }

    #[test]
    fn actual_exact_or_fallback() {
        let reg = [m(1720, 1440)];
        assert_eq!(actual_for(m(1920, 1080), &reg), (m(1920, 1080), true));
        assert_eq!(actual_for(m(1720, 1440), &reg), (m(1720, 1440), true));
        assert_eq!(actual_for(m(1280, 1440), &reg), (m(2256, 1504), false));
    }

    #[test]
    fn encode_decode_roundtrip() {
        let v = vec![m(1720, 1440), Mode::new(1280, 1440, 120)];
        let s = encode(&v);
        assert_eq!(s, "1720x1440@60,1280x1440@120");
        assert_eq!(decode(&s).unwrap(), v);
        assert!(decode("").unwrap().is_empty());
    }

    #[test]
    fn decode_rejects_bad_input() {
        assert!(decode("1720x1440").is_err());
        assert!(decode("axb@60").is_err());
        assert!(decode("1720x1440@0").is_err());
        assert!(decode("99999x1440@60").is_err());
        assert!(decode("1720x1440@60,1720x1440@60").is_err());
        let six: Vec<String> = (0..6).map(|i| format!("{}x1000@60", 1301 + i)).collect();
        assert!(decode(&six.join(",")).is_err());
    }
}
