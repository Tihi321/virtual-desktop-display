//! Configuration, stored as TOML in `%APPDATA%\virtual-desktop-display\config.toml`.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Substring of the physical monitor friendly name.
    pub physical_match: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    /// Swap top/bottom halves.
    pub swap: bool,
    /// Register in HKCU Run so the tray app starts with Windows.
    pub autostart: bool,
    /// Start the split immediately when the tray app launches.
    pub start_on_launch: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            physical_match: "FlipGo".into(),
            width: 1920,
            height: 1080,
            refresh_hz: 60,
            swap: false,
            autostart: false,
            start_on_launch: true,
        }
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

    /// Write the config (creating the directory).
    pub fn save(&self) -> anyhow::Result<()> {
        let p = Self::path().ok_or_else(|| anyhow::anyhow!("%APPDATA% not set"))?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Load from the config path if present, else defaults.
    pub fn load() -> Self {
        let Some(p) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&p) {
            Ok(s) => match toml::from_str(&s) {
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
        assert_eq!(
            (c.width, c.height, c.refresh_hz, c.swap),
            (1920, 1080, 60, false)
        );
        assert_eq!(c.physical_match, "FlipGo");
        assert!(c.start_on_launch && !c.autostart);
    }

    #[test]
    fn parse_partial_and_roundtrip() {
        let c: Config = toml::from_str("swap = true\nwidth = 1280").unwrap();
        assert!(c.swap);
        assert_eq!(c.width, 1280);
        assert_eq!(c.height, 1080);
        let s = toml::to_string(&c).unwrap();
        assert_eq!(toml::from_str::<Config>(&s).unwrap(), c);
    }
}
