//! Display enumeration, mode setting and layout (QueryDisplayConfig / SetDisplayConfig).
use crate::config::SplitConfig;
use crate::vdd::parsec::VDD_DISPLAY_NAME;
use anyhow::{anyhow, bail, Result};
use std::time::{Duration, Instant};
use windows::core::BOOL;
use windows::core::PCWSTR;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, QUERY_DISPLAY_CONFIG_FLAGS, SDC_ALLOW_CHANGES, SDC_APPLY,
    SDC_USE_SUPPLIED_DISPLAY_CONFIG,
};
use windows::Win32::Foundation::{LPARAM, LUID, POINTL, RECT};
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, CDS_TYPE,
    DEVMODEW, DEVMODEW_0, DEVMODEW_0_1, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT,
    DM_PELSWIDTH, DM_POSITION, ENUM_CURRENT_SETTINGS, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
}

/// Stable identity of a display target: (adapter LUID, target id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetKey {
    pub luid_low: u32,
    pub luid_high: i32,
    pub target_id: u32,
}

#[derive(Debug, Clone)]
pub struct TargetInfo {
    pub key: TargetKey,
    pub friendly_name: String,
    pub device_path: String,
    /// `\\.\DISPLAYn`
    pub gdi_name: String,
    pub rect: Rect,
    /// HMONITOR as an integer (0 if not found).
    pub hmonitor: isize,
    pub primary: bool,
}

fn wide_to_string(w: &[u16]) -> String {
    let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..n])
}

fn luid_key(luid: LUID, target_id: u32) -> TargetKey {
    TargetKey {
        luid_low: luid.LowPart,
        luid_high: luid.HighPart,
        target_id,
    }
}

/// DISPLAYCONFIG_PATH_ACTIVE / DISPLAYCONFIG_PATH_MODE_IDX_INVALID (wingdi.h).
const PATH_ACTIVE: u32 = 0x1;
const MODE_IDX_INVALID: u32 = 0xFFFF_FFFF;

/// Active display config arrays.
#[derive(Clone)]
pub struct DisplayConfig {
    pub paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    pub modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

/// Query the currently active topology.
pub fn query_active_config() -> Result<DisplayConfig> {
    query_config(QDC_ONLY_ACTIVE_PATHS)
}

/// QueryDisplayConfig with the given flags (ACTIVE or ALL paths).
pub fn query_config(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> Result<DisplayConfig> {
    // The topology can change between the size query and the query itself.
    for _ in 0..4 {
        let (mut np, mut nm) = (0u32, 0u32);
        unsafe { GetDisplayConfigBufferSizes(flags, &mut np, &mut nm).ok()? };
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        let r = unsafe {
            QueryDisplayConfig(
                flags,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                None,
            )
        };
        if r.0 == 122 {
            continue; // ERROR_INSUFFICIENT_BUFFER
        }
        r.ok()?;
        paths.truncate(np as usize);
        modes.truncate(nm as usize);
        return Ok(DisplayConfig { paths, modes });
    }
    bail!("QueryDisplayConfig: topology kept changing")
}

fn target_names(path: &DISPLAYCONFIG_PATH_INFO) -> (String, String) {
    let mut t = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    let r = unsafe { DisplayConfigGetDeviceInfo(&mut t.header) };
    if r != 0 {
        return (String::new(), String::new());
    }
    (
        wide_to_string(&t.monitorFriendlyDeviceName),
        wide_to_string(&t.monitorDevicePath),
    )
}

fn source_gdi_name(path: &DISPLAYCONFIG_PATH_INFO) -> String {
    let mut s = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        },
        ..Default::default()
    };
    let r = unsafe { DisplayConfigGetDeviceInfo(&mut s.header) };
    if r != 0 {
        return String::new();
    }
    wide_to_string(&s.viewGdiDeviceName)
}

struct MonitorEntry {
    hmonitor: isize,
    device: String,
    rect: Rect,
    primary: bool,
}

fn enum_monitors() -> Vec<MonitorEntry> {
    unsafe extern "system" fn cb(hm: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<MonitorEntry>);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hm, &mut mi as *mut _ as *mut MONITORINFO).as_bool() {
            let r = mi.monitorInfo.rcMonitor;
            out.push(MonitorEntry {
                hmonitor: hm.0 as isize,
                device: wide_to_string(&mi.szDevice),
                rect: Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top),
                primary: mi.monitorInfo.dwFlags & 1 != 0,
            });
        }
        BOOL(1)
    }
    let mut v: Vec<MonitorEntry> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// Enumerate all active display targets with names, rects and HMONITORs.
pub fn enumerate() -> Result<Vec<TargetInfo>> {
    let cfg = query_active_config()?;
    let monitors = enum_monitors();
    let mut out = Vec::new();
    for p in &cfg.paths {
        let (friendly_name, device_path) = target_names(p);
        let gdi_name = source_gdi_name(p);
        let m = monitors
            .iter()
            .find(|m| m.device.eq_ignore_ascii_case(&gdi_name));
        out.push(TargetInfo {
            key: luid_key(p.targetInfo.adapterId, p.targetInfo.id),
            friendly_name,
            device_path,
            gdi_name,
            rect: m.map(|m| m.rect).unwrap_or(Rect::new(0, 0, 0, 0)),
            hmonitor: m.map(|m| m.hmonitor).unwrap_or(0),
            primary: m.map(|m| m.primary).unwrap_or(false),
        });
    }
    Ok(out)
}

/// Find the physical display whose friendly name contains `needle` (case-insensitive).
pub fn find_physical<'a>(targets: &'a [TargetInfo], needle: &str) -> Option<&'a TargetInfo> {
    let n = needle.to_lowercase();
    targets
        .iter()
        .find(|t| t.friendly_name.to_lowercase().contains(&n))
}

impl TargetInfo {
    /// EDID manufacturer + product id from the device path, e.g.
    /// `\\?\DISPLAY#RTK4707#5&..` gives "RTK4707".
    pub fn display_id(&self) -> Option<String> {
        display_id_from_path(&self.device_path)
    }

    /// True for displays created by the virtual display driver (ours).
    pub fn is_virtual(&self) -> bool {
        self.friendly_name.contains(VDD_DISPLAY_NAME)
            || self
                .display_id()
                .is_some_and(|id| id.to_ascii_uppercase().starts_with("PSCCDD"))
    }
}

/// Parse "RTK4707" out of `\\?\DISPLAY#RTK4707#...`.
pub fn display_id_from_path(path: &str) -> Option<String> {
    let mut it = path.split('#');
    let head = it.next()?;
    if !head.to_ascii_uppercase().ends_with("DISPLAY") {
        return None;
    }
    let id = it.next()?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Find the real display a split refers to: `display_id` exactly (case
/// insensitive) first, then `name_match` as a case-insensitive substring of the
/// friendly name. Virtual displays are never returned.
pub fn resolve_split<'a>(targets: &'a [TargetInfo], split: &SplitConfig) -> Option<&'a TargetInfo> {
    let real = || targets.iter().filter(|t| !t.is_virtual());
    let id = split.display_id.trim();
    if !id.is_empty() {
        if let Some(t) = real().find(|t| t.display_id().is_some_and(|d| d.eq_ignore_ascii_case(id)))
        {
            return Some(t);
        }
    }
    let n = split.name_match.trim().to_lowercase();
    if !n.is_empty() {
        return real().find(|t| t.friendly_name.to_lowercase().contains(&n));
    }
    None
}

/// Current refresh rate of a GDI display (`\\.\DISPLAYn`), if known.
pub fn current_refresh(gdi_name: &str) -> Option<u32> {
    let name: Vec<u16> = gdi_name.encode_utf16().chain(Some(0)).collect();
    let mut cur = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    unsafe {
        EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut cur)
            .as_bool()
            .then_some(cur.dmDisplayFrequency)
    }
}

/// Targets in `after` that were not in `before`.
pub fn new_targets(before: &[TargetInfo], after: &[TargetInfo]) -> Vec<TargetInfo> {
    after
        .iter()
        .filter(|a| !before.iter().any(|b| b.key == a.key))
        .cloned()
        .collect()
}

/// Connected (available) targets according to QDC_ALL_PATHS, deduplicated.
/// Unlike the active-path query this also sees targets Windows has attached
/// but left inactive (e.g. because a stale persisted topology says so).
pub fn available_targets() -> Result<Vec<TargetKey>> {
    let cfg = query_config(QDC_ALL_PATHS)?;
    let mut v: Vec<TargetKey> = Vec::new();
    for p in &cfg.paths {
        if p.targetInfo.targetAvailable.as_bool() {
            let k = luid_key(p.targetInfo.adapterId, p.targetInfo.id);
            if !v.contains(&k) {
                v.push(k);
            }
        }
    }
    Ok(v)
}

/// State before adding virtual displays.
pub struct Snapshot {
    pub active: Vec<TargetInfo>,
    pub available: Vec<TargetKey>,
}

pub fn snapshot() -> Result<Snapshot> {
    Ok(Snapshot {
        active: enumerate()?,
        available: available_targets()?,
    })
}

/// Fallback activation: append inactive-but-available paths for `new` to the
/// current active config (mode indices invalid, Windows picks the modes).
fn activate_manually(new: &[TargetKey]) -> Result<()> {
    let active = query_active_config()?;
    let all = query_config(QDC_ALL_PATHS)?;
    let mut paths = active.paths.clone();
    let mut used: Vec<(u32, i32, u32)> = paths
        .iter()
        .map(|p| {
            (
                p.sourceInfo.adapterId.LowPart,
                p.sourceInfo.adapterId.HighPart,
                p.sourceInfo.id,
            )
        })
        .collect();
    for key in new {
        if paths
            .iter()
            .any(|p| luid_key(p.targetInfo.adapterId, p.targetInfo.id) == *key)
        {
            continue;
        }
        let cand = all.paths.iter().find(|p| {
            luid_key(p.targetInfo.adapterId, p.targetInfo.id) == *key
                && !used.contains(&(
                    p.sourceInfo.adapterId.LowPart,
                    p.sourceInfo.adapterId.HighPart,
                    p.sourceInfo.id,
                ))
        });
        let Some(cand) = cand else {
            bail!("no free source path for target {key:?}");
        };
        let mut p = *cand;
        p.flags |= PATH_ACTIVE;
        p.sourceInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
        p.targetInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
        used.push((
            p.sourceInfo.adapterId.LowPart,
            p.sourceInfo.adapterId.HighPart,
            p.sourceInfo.id,
        ));
        paths.push(p);
    }
    let r = unsafe {
        SetDisplayConfig(
            Some(&paths),
            Some(&active.modes),
            SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES,
        )
    };
    if r != 0 {
        bail!("SetDisplayConfig(manual activation) failed: {r}");
    }
    Ok(())
}

/// Poll until every key is an ACTIVE target; returns their info (sorted by target id).
fn wait_active(keys: &[TargetKey], timeout: Duration) -> Result<Option<Vec<TargetInfo>>> {
    let start = Instant::now();
    loop {
        let now = enumerate()?;
        if keys.iter().all(|k| now.iter().any(|t| t.key == *k)) {
            let mut v: Vec<TargetInfo> =
                now.into_iter().filter(|t| keys.contains(&t.key)).collect();
            v.sort_by_key(|t| t.key.target_id);
            return Ok(Some(v));
        }
        if start.elapsed() > timeout {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// After ADD: wait for `count` new connected targets (ALL_PATHS), make sure
/// they are ACTIVE (Windows may leave them inactive if a persisted topology
/// for this monitor set says so): first wait briefly for auto-activation, then
/// a manual path injection. Returns the active targets sorted by target id.
pub fn bring_up_new_targets(
    before: &Snapshot,
    count: usize,
    timeout: Duration,
) -> Result<Vec<TargetInfo>> {
    let start = Instant::now();
    let mut new_keys = loop {
        let now = available_targets()?;
        let new: Vec<TargetKey> = now
            .into_iter()
            .filter(|k| !before.available.contains(k))
            .collect();
        if new.len() >= count {
            break new;
        }
        if start.elapsed() > timeout {
            bail!(
                "timed out waiting for {count} new connected display targets (saw {})",
                new.len()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    new_keys.sort_by_key(|k| k.target_id);
    new_keys.truncate(count);
    tracing::info!("new connected targets: {new_keys:?}");

    let mut result = wait_active(&new_keys, Duration::from_millis(1500))?;
    if result.is_none() {
        // No global SDC_TOPOLOGY_EXTEND: it costs seconds and would re-enable
        // displays the user had turned off. Inject only our paths.
        tracing::info!("new targets not auto-activated; injecting paths manually");
        activate_manually(&new_keys)?;
        result = wait_active(&new_keys, Duration::from_secs(5))?;
    }
    let result = result.ok_or_else(|| anyhow!("new display targets could not be activated"))?;

    // Sanity: only the expected targets should be active now.
    let now = enumerate()?;
    for t in &now {
        if !before.active.iter().any(|b| b.key == t.key) && !new_keys.contains(&t.key) {
            tracing::warn!(
                "unexpected newly active target: {} {:?}",
                t.friendly_name,
                t.key
            );
        }
    }
    for b in &before.active {
        if !now.iter().any(|t| t.key == b.key) {
            tracing::warn!(
                "previously active target deactivated: {} {:?}",
                b.friendly_name,
                b.key
            );
        }
    }
    Ok(result)
}

/// Poll until none of `keys` is an active target any more.
pub fn wait_targets_gone(keys: &[TargetKey], timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        match enumerate() {
            Ok(now) if !keys.iter().any(|k| now.iter().any(|t| t.key == *k)) => return true,
            Err(_) => return false,
            _ => {}
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Set `gdi_name` to width x height @ hz if not already.
pub fn set_mode(gdi_name: &str, width: u32, height: u32, hz: u32, pos: (i32, i32)) -> Result<()> {
    let name: Vec<u16> = gdi_name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut cur = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut cur).as_bool()
            && cur.dmPelsWidth == width
            && cur.dmPelsHeight == height
            && cur.dmDisplayFrequency == hz
        {
            return Ok(());
        }
        let dm = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            dmFields: DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY,
            Anonymous1: DEVMODEW_0 {
                Anonymous2: DEVMODEW_0_1 {
                    dmPosition: POINTL { x: pos.0, y: pos.1 },
                    ..Default::default()
                },
            },
            dmPelsWidth: width,
            dmPelsHeight: height,
            dmDisplayFrequency: hz,
            ..Default::default()
        };
        let r = ChangeDisplaySettingsExW(
            PCWSTR(name.as_ptr()),
            Some(&dm),
            None,
            // Dynamic change (no CDS_UPDATEREGISTRY); DM_POSITION attaches a
            // source that was injected without a desktop position.
            CDS_TYPE(0),
            None,
        );
        tracing::info!(
            "ChangeDisplaySettingsExW({gdi_name}, {width}x{height}@{hz} at {pos:?}) -> {}",
            r.0
        );
        if r != DISP_CHANGE_SUCCESSFUL {
            bail!(
                "ChangeDisplaySettingsExW({gdi_name}, {width}x{height}@{hz}) failed: {}",
                r.0
            );
        }
    }
    Ok(())
}

/// True if every key is currently an active target.
pub fn all_present(keys: &[TargetKey]) -> Result<bool> {
    let now = enumerate()?;
    Ok(keys.iter().all(|k| now.iter().any(|t| t.key == *k)))
}

fn position_of(cfg: &mut DisplayConfig, key: TargetKey) -> Result<&mut DISPLAYCONFIG_MODE_INFO> {
    let p = cfg
        .paths
        .iter()
        .find(|p| luid_key(p.targetInfo.adapterId, p.targetInfo.id) == key)
        .ok_or_else(|| anyhow!("target {key:?} not in active config"))?;
    let idx = unsafe { p.sourceInfo.Anonymous.modeInfoIdx } as usize;
    let m = cfg
        .modes
        .get_mut(idx)
        .ok_or_else(|| anyhow!("source mode index {idx} out of range"))?;
    if m.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
        bail!("mode {idx} is not a source mode");
    }
    Ok(m)
}

/// Apply a supplied config WITHOUT SDC_SAVE_TO_DATABASE: the moved position is
/// then not persisted, so if the process is killed Windows falls back to the
/// stored layout for the original monitor set once the virtual displays vanish.
fn apply_config(cfg: &DisplayConfig) -> Result<()> {
    let r = unsafe {
        SetDisplayConfig(
            Some(&cfg.paths),
            Some(&cfg.modes),
            SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES,
        )
    };
    if r != 0 {
        bail!("SetDisplayConfig failed: {r}");
    }
    Ok(())
}

/// Apply desktop positions by editing the source-mode positions of the
/// active config. Every key must be an active target.
pub fn apply_layout(positions: &[(TargetKey, (i32, i32))]) -> Result<()> {
    let mut cfg = query_active_config()?;
    for (key, (x, y)) in positions {
        let m = position_of(&mut cfg, *key)?;
        m.Anonymous.sourceMode.position.x = *x;
        m.Anonymous.sourceMode.position.y = *y;
    }
    apply_config(&cfg)
}

/// Snapshot of the topology, to restore on stop.
pub struct SavedLayout(DisplayConfig);

pub fn save_layout() -> Result<SavedLayout> {
    Ok(SavedLayout(query_active_config()?))
}

pub fn restore_layout(saved: &SavedLayout) -> Result<()> {
    apply_config(&saved.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(n: &str, id: u32, path: &str) -> TargetInfo {
        TargetInfo {
            key: TargetKey {
                luid_low: 1,
                luid_high: 0,
                target_id: id,
            },
            friendly_name: n.into(),
            device_path: path.into(),
            gdi_name: String::new(),
            rect: Rect::new(0, 0, 1, 1),
            hmonitor: 0,
            primary: false,
        }
    }

    fn split(id: &str, name: &str) -> SplitConfig {
        SplitConfig {
            display_id: id.into(),
            name_match: name.into(),
            ..SplitConfig::default()
        }
    }

    #[test]
    fn display_id_parsing() {
        assert_eq!(
            display_id_from_path(r"\\?\DISPLAY#RTK4707#5&1a2b&0&UID4352#{e6f07b5f}"),
            Some("RTK4707".into())
        );
        assert_eq!(display_id_from_path("garbage"), None);
        assert_eq!(display_id_from_path(""), None);
        assert_eq!(display_id_from_path(r"\\?\DISPLAY##x"), None);
    }

    fn setup() -> Vec<TargetInfo> {
        vec![
            mk("VG34VQL3A", 1, r"\\?\DISPLAY#AUS1234#5&aa&0&UID1#{g}"),
            mk("FlipGo-H", 2, r"\\?\DISPLAY#RTK4707#5&bb&0&UID2#{g}"),
            mk("ParsecVDA", 3, r"\\?\DISPLAY#PSCCDD0#5&cc&0&UID3#{g}"),
        ]
    }

    #[test]
    fn virtual_detection() {
        let t = setup();
        assert!(!t[0].is_virtual() && !t[1].is_virtual() && t[2].is_virtual());
        // by id alone
        assert!(mk("Generic", 4, r"\\?\DISPLAY#PSCCDD1#x").is_virtual());
    }

    #[test]
    fn resolve_by_id_then_name() {
        let t = setup();
        let id = |s: &SplitConfig| resolve_split(&t, s).map(|x| x.key.target_id);
        assert_eq!(id(&split("rtk4707", "")), Some(2));
        // id wins over a name that matches another display
        assert_eq!(id(&split("RTK4707", "vg34")), Some(2));
        // unknown id falls back to the name
        assert_eq!(id(&split("XXX0000", "flipgo")), Some(2));
        assert_eq!(id(&split("", "VG34")), Some(1));
        assert_eq!(id(&split("XXX0000", "")), None);
        assert_eq!(id(&split("", "")), None);
    }

    #[test]
    fn never_resolves_virtual() {
        let t = setup();
        assert!(resolve_split(&t, &split("PSCCDD0", "")).is_none());
        assert!(resolve_split(&t, &split("", "parsec")).is_none());
    }

    #[test]
    fn find_physical_by_substring() {
        let t = setup();
        assert_eq!(find_physical(&t, "flipgo").unwrap().key.target_id, 2);
        let mut after = t.clone();
        after.push(mk("New", 9, ""));
        assert_eq!(new_targets(&t, &after).len(), 1);
    }
}
