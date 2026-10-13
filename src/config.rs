//! Configuration (v2), stored as TOML in `%APPDATA%\virtual-desktop-display\config.toml`.
//!
//! A v1 file (single `physical_match` / `swap`) is migrated on load into one
//! split; the file itself is only rewritten when the app saves.
use crate::geometry::Spec;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CONFIG_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LayoutKind {
    Columns,
    #[default]
    Rows,
    Grid,
    Custom,
}

fn yes() -> bool {
    true
}

/// One hotkey: key combination text ("Ctrl+Alt+S") plus its own switch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyBinding {
    pub keys: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        Self {
            keys: String::new(),
            enabled: true,
        }
    }
}

impl HotkeyBinding {
    pub fn new(keys: &str, enabled: bool) -> Self {
        Self {
            keys: keys.into(),
            enabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    /// Master switch.
    pub enabled: bool,
    pub toggle_all: HotkeyBinding,
    pub cycle_order: HotkeyBinding,
    pub open_settings: HotkeyBinding,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            enabled: true,
            toggle_all: HotkeyBinding::new("Ctrl+Alt+S", true),
            cycle_order: HotkeyBinding::new("Ctrl+Alt+W", true),
            open_settings: HotkeyBinding::new("Ctrl+Alt+D", false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SplitConfig {
    pub name: String,
    pub enabled: bool,
    pub auto_on_connect: bool,
    /// EDID manufacturer + product from the device path, e.g. "RTK4707".
    pub display_id: String,
    /// Optional fallback: friendly-name substring (case-insensitive).
    pub name_match: String,
    pub layout: LayoutKind,
    pub columns: u32,
    pub rows: u32,
    /// Custom layout only: fractions summing to 1.
    pub ratios_x: Vec<f64>,
    pub ratios_y: Vec<f64>,
    /// `order[i]` = geometric cell shown by the i-th virtual display.
    pub order: Vec<usize>,
    /// Geometric cell that becomes the primary display (only when the split
    /// display is the primary one).
    pub main_piece: usize,
    pub refresh_hz: u32,
}

impl Default for SplitConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            auto_on_connect: true,
            display_id: String::new(),
            name_match: String::new(),
            layout: LayoutKind::Rows,
            columns: 1,
            rows: 2,
            ratios_x: Vec::new(),
            ratios_y: Vec::new(),
            order: vec![0, 1],
            main_piece: 0,
            refresh_hz: 60,
        }
    }
}

impl SplitConfig {
    /// The geometry inputs of this split.
    pub fn spec(&self) -> Spec {
        Spec {
            layout: self.layout,
            columns: self.columns,
            rows: self.rows,
            ratios_x: self.ratios_x.clone(),
            ratios_y: self.ratios_y.clone(),
            order: self.order.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    /// Register in HKCU Run so the tray app starts with Windows.
    pub autostart: bool,
    /// Start the splits immediately when the tray app launches.
    pub start_on_launch: bool,
    pub hotkeys: Hotkeys,
    pub splits: Vec<SplitConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            autostart: false,
            start_on_launch: true,
            hotkeys: Hotkeys::default(),
            splits: Vec::new(),
        }
    }
}

/// The v0.1 file format, only used for migration.
#[derive(Deserialize)]
#[serde(default)]
struct ConfigV1 {
    physical_match: String,
    refresh_hz: u32,
    swap: bool,
    autostart: bool,
    start_on_launch: bool,
}

impl Default for ConfigV1 {
    fn default() -> Self {
        Self {
            physical_match: "FlipGo".into(),
            refresh_hz: 60,
            swap: false,
            autostart: false,
            start_on_launch: true,
        }
    }
}

impl From<ConfigV1> for Config {
    fn from(v: ConfigV1) -> Self {
        let mut c = Config {
            autostart: v.autostart,
            start_on_launch: v.start_on_launch,
            ..Config::default()
        };
        if !v.physical_match.trim().is_empty() {
            c.splits.push(SplitConfig {
                name: v.physical_match.clone(),
                name_match: v.physical_match,
                layout: LayoutKind::Rows,
                columns: 1,
                rows: 2,
                order: if v.swap { vec![1, 0] } else { vec![0, 1] },
                refresh_hz: v.refresh_hz,
                ..SplitConfig::default()
            });
        }
        c
    }
}

impl Config {
    /// Directory holding config and logs.
    pub fn dir() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("virtual-desktop-display"))
    }

    pub fn path() -> Option<PathBuf> {
        Self::dir().map(|d| d.join("config.toml"))
    }

    /// Parse a config file's text; v1 files are migrated.
    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        let v: toml::Value = toml::from_str(s)?;
        let version = v.get("version").and_then(|x| x.as_integer()).unwrap_or(1);
        if version >= CONFIG_VERSION as i64 {
            toml::from_str(s)
        } else {
            Ok(toml::from_str::<ConfigV1>(s)?.into())
        }
    }

    /// Write the config (creating the directory).
    pub fn save(&self) -> anyhow::Result<()> {
        let p = Self::path().ok_or_else(|| anyhow::anyhow!("%APPDATA% not set"))?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Load from the config path if present, else defaults (no splits).
    pub fn load() -> Self {
        let Some(p) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&p) {
            Ok(s) => match Self::parse(&s) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("bad config {}: {e}; using defaults", p.display());
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.version, 2);
        assert!(c.splits.is_empty());
        assert!(c.start_on_launch && !c.autostart);
        assert!(c.hotkeys.enabled);
        assert_eq!(c.hotkeys.toggle_all, HotkeyBinding::new("Ctrl+Alt+S", true));
        assert!(!c.hotkeys.open_settings.enabled);
    }

    #[test]
    fn v2_roundtrip() {
        let mut c = Config::default();
        c.splits.push(SplitConfig {
            name: "Asus".into(),
            display_id: "AUS1234".into(),
            layout: LayoutKind::Custom,
            ratios_x: vec![0.25, 0.75],
            ratios_y: vec![],
            order: vec![1, 0],
            ..SplitConfig::default()
        });
        c.splits.push(SplitConfig::default());
        let s = toml::to_string_pretty(&c).unwrap();
        assert_eq!(Config::parse(&s).unwrap(), c);
    }

    #[test]
    fn v2_partial_uses_defaults() {
        let c = Config::parse(
            "version = 2\n[hotkeys]\nenabled = false\n[hotkeys.toggle_all]\nkeys = \"Ctrl+F1\"\n\
             [[splits]]\nname = \"x\"\nlayout = \"grid\"\ncolumns = 2\nrows = 2\n",
        )
        .unwrap();
        assert!(!c.hotkeys.enabled);
        assert_eq!(c.hotkeys.toggle_all, HotkeyBinding::new("Ctrl+F1", true));
        assert_eq!(c.hotkeys.cycle_order.keys, "Ctrl+Alt+W");
        assert_eq!(c.splits.len(), 1);
        let s = &c.splits[0];
        assert_eq!(s.layout, LayoutKind::Grid);
        assert!(s.enabled && s.auto_on_connect);
        assert_eq!(s.refresh_hz, 60);
    }

    #[test]
    fn migrates_v1() {
        let c = Config::parse(
            "physical_match = \"FlipGo\"\nwidth = 1920\nheight = 1080\nrefresh_hz = 75\n\
             swap = true\nautostart = true\nstart_on_launch = false\n",
        )
        .unwrap();
        assert_eq!(c.version, 2);
        assert!(c.autostart && !c.start_on_launch);
        assert_eq!(c.splits.len(), 1);
        let s = &c.splits[0];
        assert_eq!(s.name, "FlipGo");
        assert_eq!(s.name_match, "FlipGo");
        assert_eq!(s.layout, LayoutKind::Rows);
        assert_eq!((s.columns, s.rows), (1, 2));
        assert_eq!(s.order, vec![1, 0]);
        assert_eq!(s.refresh_hz, 75);
        assert!(s.display_id.is_empty());
    }

    #[test]
    fn migrates_v1_without_swap_and_defaults() {
        let c = Config::parse("swap = false").unwrap();
        assert_eq!(c.splits[0].order, vec![0, 1]);
        assert_eq!(c.splits[0].name_match, "FlipGo");
        let c = Config::parse("").unwrap();
        assert_eq!(c.splits.len(), 1);
    }
}
