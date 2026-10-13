//! Global hotkeys: parsing and formatting of key combinations such as
//! "Ctrl+Alt+S", and registration on the tray window (`RegisterHotKey`).
//!
//! Combination text: modifiers `Ctrl`, `Alt`, `Shift`, `Win` (any order, case
//! insensitive) followed by exactly one key, joined by `+`. Formatting always
//! uses the order `Ctrl+Alt+Shift+Win+<Key>`.
use crate::config::Hotkeys;
use anyhow::{anyhow, bail, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CTRL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

/// A parsed combination: `MOD_*` bits and a Windows virtual-key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Combo {
    pub mods: u32,
    pub vk: u32,
}

/// What a hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ToggleAll,
    CycleOrder,
    OpenSettings,
}

impl Action {
    pub const ALL: [Action; 3] = [Action::ToggleAll, Action::CycleOrder, Action::OpenSettings];

    /// The `RegisterHotKey` id (1..=3).
    pub fn id(self) -> i32 {
        match self {
            Action::ToggleAll => 1,
            Action::CycleOrder => 2,
            Action::OpenSettings => 3,
        }
    }

    pub fn from_id(id: i32) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }

    /// Stable name used in the config keys and status.json.
    pub fn key(self) -> &'static str {
        match self {
            Action::ToggleAll => "toggle_all",
            Action::CycleOrder => "cycle_order",
            Action::OpenSettings => "open_settings",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::ToggleAll => "Toggle splits (start / stop)",
            Action::CycleOrder => "Cycle pieces",
            Action::OpenSettings => "Open settings",
        }
    }
}

/// (token, vk) pairs for the named (non letter/digit/F-key) keys. The first
/// token of a vk is the canonical one used when formatting.
const NAMED: &[(&str, u32)] = &[
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Tab", 0x09),
    ("Backspace", 0x08),
    ("Escape", 0x1B),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("Minus", 0xBD),
    ("Equals", 0xBB),
    ("Comma", 0xBC),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Backtick", 0xC0),
    ("OpenBracket", 0xDB),
    ("Backslash", 0xDC),
    ("CloseBracket", 0xDD),
    ("Quote", 0xDE),
    ("Semicolon", 0xBA),
];

/// Extra accepted spellings.
const ALIASES: &[(&str, &str)] = &[
    ("PgUp", "PageUp"),
    ("PgDn", "PageDown"),
    ("PgDown", "PageDown"),
    ("Ins", "Insert"),
    ("Del", "Delete"),
    ("Esc", "Escape"),
    ("Return", "Enter"),
    ("ArrowLeft", "Left"),
    ("ArrowRight", "Right"),
    ("ArrowUp", "Up"),
    ("ArrowDown", "Down"),
];

/// Virtual-key code of a key token ("S", "5", "F11", "PageUp", "Num3"), if known.
pub fn vk_from_token(tok: &str) -> Option<u32> {
    let t = tok.trim();
    if t.is_empty() {
        return None;
    }
    let tl = t.to_ascii_lowercase();
    let mut chars = t.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(n) = tl.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) && !tl.starts_with("f0") {
            return Some(0x70 + n - 1);
        }
    }
    if let Some(n) = tl.strip_prefix("num").and_then(|n| n.parse::<u32>().ok()) {
        if n <= 9 && tl.len() == 4 {
            return Some(0x60 + n);
        }
    }
    let canonical = ALIASES
        .iter()
        .find(|(a, _)| a.eq_ignore_ascii_case(t))
        .map(|(_, c)| *c)
        .unwrap_or(t);
    NAMED
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(canonical))
        .map(|(_, vk)| *vk)
}

/// Canonical token of a virtual-key code.
pub fn token_from_vk(vk: u32) -> Option<String> {
    match vk {
        0x41..=0x5A | 0x30..=0x39 => char::from_u32(vk).map(|c| c.to_string()),
        0x70..=0x87 => Some(format!("F{}", vk - 0x70 + 1)),
        0x60..=0x69 => Some(format!("Num{}", vk - 0x60)),
        _ => NAMED
            .iter()
            .find(|(_, v)| *v == vk)
            .map(|(n, _)| n.to_string()),
    }
}

/// Parse "Ctrl+Alt+S" (at least a key; modifiers optional).
pub fn parse(s: &str) -> Result<Combo> {
    let mut mods = 0u32;
    let mut vk: Option<u32> = None;
    if s.trim().is_empty() {
        bail!("no keys");
    }
    for part in s.split('+') {
        let p = part.trim();
        if p.is_empty() {
            bail!("empty key name in {s:?}");
        }
        let bit = match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "ctl" => MOD_CTRL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" | "windows" | "super" | "meta" => MOD_WIN,
            _ => 0,
        };
        if bit != 0 {
            if mods & bit != 0 {
                bail!("modifier {p:?} repeated in {s:?}");
            }
            mods |= bit;
        } else {
            let k = vk_from_token(p).ok_or_else(|| anyhow!("unknown key {p:?} in {s:?}"))?;
            if vk.replace(k).is_some() {
                bail!("more than one key in {s:?}");
            }
        }
    }
    let vk = vk.ok_or_else(|| anyhow!("no key in {s:?}"))?;
    Ok(Combo { mods, vk })
}

/// Format as "Ctrl+Alt+Shift+Win+<Key>"; None for an unknown key code.
pub fn format(mods: u32, vk: u32) -> Option<String> {
    let mut out = String::new();
    for (bit, name) in [
        (MOD_CTRL, "Ctrl"),
        (MOD_ALT, "Alt"),
        (MOD_SHIFT, "Shift"),
        (MOD_WIN, "Win"),
    ] {
        if mods & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&token_from_vk(vk)?);
    Some(out)
}

/// True when the combination is safe to register globally: it has a modifier,
/// or is a function key (a bare letter would steal normal typing).
pub fn is_acceptable(c: Combo) -> bool {
    c.mods != 0 || (0x70..=0x87).contains(&c.vk)
}

/// Human text for a `RegisterHotKey` failure (Win32 error code).
pub fn describe_error(code: u32) -> String {
    match code {
        1409 => "in use by another app".to_string(),
        87 => "invalid key combination".to_string(),
        c => format!("error {c}"),
    }
}

/// Outcome of registering one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub action: Action,
    pub keys: String,
    pub enabled: bool,
    pub registered: bool,
    pub error: Option<String>,
}

/// The binding of `action` in the config.
pub fn binding(hk: &Hotkeys, action: Action) -> &crate::config::HotkeyBinding {
    match action {
        Action::ToggleAll => &hk.toggle_all,
        Action::CycleOrder => &hk.cycle_order,
        Action::OpenSettings => &hk.open_settings,
    }
}

/// Unregister every hotkey of the window (ids 1..=3).
pub fn unregister_all(hwnd: HWND) {
    for a in Action::ALL {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), a.id());
        }
    }
}

/// (Re-)register all hotkeys on `hwnd` honouring the master switch and the
/// per-action `enabled` flags. Failures are reported, not fatal.
pub fn register_all(hwnd: HWND, hk: &Hotkeys) -> Vec<Registration> {
    unregister_all(hwnd);
    Action::ALL
        .into_iter()
        .map(|a| {
            let b = binding(hk, a);
            let mut r = Registration {
                action: a,
                keys: b.keys.clone(),
                enabled: hk.enabled && b.enabled,
                registered: false,
                error: None,
            };
            if !r.enabled {
                return r;
            }
            match parse(&b.keys) {
                Err(e) => r.error = Some(format!("{e}")),
                Ok(c) => {
                    let res = unsafe {
                        RegisterHotKey(
                            Some(hwnd),
                            a.id(),
                            HOT_KEY_MODIFIERS(c.mods) | MOD_NOREPEAT,
                            c.vk,
                        )
                    };
                    match res {
                        Ok(()) => r.registered = true,
                        Err(e) => {
                            let code = (e.code().0 as u32) & 0xFFFF;
                            tracing::warn!("hotkey {} ({}): {e}", a.key(), b.keys);
                            r.error = Some(describe_error(code));
                        }
                    }
                }
            }
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let c = parse("Ctrl+Alt+S").unwrap();
        assert_eq!(c.mods, MOD_CTRL | MOD_ALT);
        assert_eq!(c.vk, 'S' as u32);
        let c = parse(" shift + win + f11 ").unwrap();
        assert_eq!(c.mods, MOD_SHIFT | MOD_WIN);
        assert_eq!(c.vk, 0x7A);
        assert_eq!(parse("Ctrl+PgUp").unwrap().vk, 0x21);
        assert_eq!(parse("Alt+5").unwrap().vk, '5' as u32);
        assert_eq!(parse("Ctrl+F24").unwrap().vk, 0x87);
        assert_eq!(parse("F1").unwrap(), Combo { mods: 0, vk: 0x70 });
        assert_eq!(parse("Ctrl+Num3").unwrap().vk, 0x63);
    }

    #[test]
    fn parse_rejects_bad() {
        for s in [
            "",
            "Ctrl+",
            "Ctrl",
            "Ctrl+Ctrl+S",
            "Ctrl+S+D",
            "Ctrl+F25",
            "Ctrl+F0",
            "Ctrl+Foo",
            "+S",
            "Ctrl+Num10",
        ] {
            assert!(parse(s).is_err(), "{s:?} should not parse");
        }
    }

    #[test]
    fn format_order_and_names() {
        assert_eq!(
            format(MOD_WIN | MOD_SHIFT | MOD_ALT | MOD_CTRL, 'D' as u32).unwrap(),
            "Ctrl+Alt+Shift+Win+D"
        );
        assert_eq!(format(MOD_CTRL, 0x22).unwrap(), "Ctrl+PageDown");
        assert_eq!(format(0, 0x7B).unwrap(), "F12");
        assert!(format(MOD_CTRL, 0xE8).is_none());
    }

    #[test]
    fn roundtrip_text_and_keys() {
        for s in [
            "Ctrl+Alt+S",
            "Ctrl+Alt+Shift+Win+F24",
            "Alt+Home",
            "Ctrl+Space",
            "Shift+Insert",
            "Win+Left",
            "Ctrl+Alt+Delete",
            "Ctrl+0",
            "Ctrl+Minus",
            "Ctrl+Num7",
            "F5",
        ] {
            let c = parse(s).unwrap();
            assert_eq!(format(c.mods, c.vk).unwrap(), s);
        }
        // every named/letter/digit/F key survives vk -> token -> vk
        let mut vks: Vec<u32> = (0x41..=0x5A)
            .chain(0x30..=0x39)
            .chain(0x70..=0x87)
            .collect();
        vks.extend(NAMED.iter().map(|(_, v)| *v));
        for vk in vks {
            let t = token_from_vk(vk).unwrap();
            assert_eq!(vk_from_token(&t), Some(vk), "{t}");
        }
    }

    #[test]
    fn acceptable_and_errors() {
        assert!(is_acceptable(parse("Ctrl+S").unwrap()));
        assert!(is_acceptable(parse("F9").unwrap()));
        assert!(!is_acceptable(parse("S").unwrap()));
        assert_eq!(describe_error(1409), "in use by another app");
        assert_eq!(describe_error(5), "error 5");
    }

    #[test]
    fn action_ids() {
        for a in Action::ALL {
            assert_eq!(Action::from_id(a.id()), Some(a));
        }
        assert_eq!(Action::from_id(0), None);
        assert_eq!(Action::from_id(4), None);
    }
}
