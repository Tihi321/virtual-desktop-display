//! Hotkeys panel: master switch, per-action enable, key recorder, live status.
use super::app::SettingsApp;
use crate::config::HotkeyBinding;
use crate::hotkeys::{self, Action};
use eframe::egui;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LWIN, VK_RWIN};

/// Result of one frame of key recording.
#[derive(Debug, PartialEq, Eq)]
pub enum Recorded {
    Nothing,
    Cancel,
    /// A key was pressed without an acceptable modifier set.
    Rejected,
    Combo(String),
}

fn win_down() -> bool {
    unsafe {
        (GetAsyncKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0
            || (GetAsyncKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0
    }
}

/// Look at this frame's key events and turn the first key press into a combo.
pub fn capture(ctx: &egui::Context) -> Recorded {
    let win = win_down();
    ctx.input(|i| {
        for e in &i.events {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = e
            else {
                continue;
            };
            let mut mods = 0;
            if modifiers.ctrl {
                mods |= hotkeys::MOD_CTRL;
            }
            if modifiers.alt {
                mods |= hotkeys::MOD_ALT;
            }
            if modifiers.shift {
                mods |= hotkeys::MOD_SHIFT;
            }
            if win {
                mods |= hotkeys::MOD_WIN;
            }
            if *key == egui::Key::Escape && mods == 0 {
                return Recorded::Cancel;
            }
            let Some(vk) = hotkeys::vk_from_token(key.name()) else {
                continue;
            };
            let c = hotkeys::Combo { mods, vk };
            if !hotkeys::is_acceptable(c) {
                return Recorded::Rejected;
            }
            return match hotkeys::format(mods, vk) {
                Some(s) => Recorded::Combo(s),
                None => Recorded::Rejected,
            };
        }
        Recorded::Nothing
    })
}

fn binding_mut(app: &mut SettingsApp, a: Action) -> &mut HotkeyBinding {
    match a {
        Action::ToggleAll => &mut app.cfg.hotkeys.toggle_all,
        Action::CycleOrder => &mut app.cfg.hotkeys.cycle_order,
        Action::OpenSettings => &mut app.cfg.hotkeys.open_settings,
    }
}

/// Live status text of an action from the tray's status.json.
fn live_status(app: &SettingsApp, a: Action) -> (String, egui::Color32) {
    let grey = egui::Color32::GRAY;
    if !app.tray_running {
        return ("app not running".into(), grey);
    }
    let Some(h) = app
        .status
        .as_ref()
        .and_then(|s| s.hotkeys.iter().find(|h| h.action == a.key()))
    else {
        return ("unknown".into(), grey);
    };
    let cur = hotkeys::binding(&app.cfg.hotkeys, a);
    if h.keys != cur.keys || h.enabled != (app.cfg.hotkeys.enabled && cur.enabled) {
        return ("changed: Save & apply to use it".into(), grey);
    }
    if !h.enabled {
        ("disabled".into(), grey)
    } else if h.registered {
        ("registered".into(), egui::Color32::from_rgb(120, 200, 120))
    } else {
        (
            h.error.clone().unwrap_or_else(|| "not registered".into()),
            egui::Color32::from_rgb(230, 90, 90),
        )
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut SettingsApp) {
    ui.heading("Hotkeys");
    ui.add_space(6.0);
    ui.checkbox(&mut app.cfg.hotkeys.enabled, "Enable global hotkeys");
    ui.add_space(8.0);

    if let Some(a) = app.recording {
        match capture(ui.ctx()) {
            Recorded::Nothing => {}
            Recorded::Cancel => app.recording = None,
            Recorded::Rejected => {
                app.error("Hold Ctrl, Alt, Shift or Win while pressing the key.");
            }
            Recorded::Combo(s) => {
                binding_mut(app, a).keys = s;
                app.recording = None;
                app.message = None;
            }
        }
    }

    let master = app.cfg.hotkeys.enabled;
    egui::Grid::new("hotkeys")
        .num_columns(5)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.strong("Action");
            ui.strong("Enabled");
            ui.strong("Keys");
            ui.strong("");
            ui.strong("Status");
            ui.end_row();
            for a in Action::ALL {
                let recording = app.recording == Some(a);
                let (status, color) = live_status(app, a);
                let mut b = binding_mut(app, a).clone();
                let mut toggle_rec = false;
                ui.label(a.label());
                ui.add_enabled(master, egui::Checkbox::without_text(&mut b.enabled));
                let text = if recording {
                    "press the new keys... (Esc cancels)".to_string()
                } else if b.keys.is_empty() {
                    "(none)".to_string()
                } else {
                    b.keys.clone()
                };
                ui.monospace(text);
                ui.horizontal(|ui| {
                    let label = if recording { "Cancel" } else { "Record" };
                    let rec = ui.add_enabled(master, egui::Button::new(label));
                    let clear = ui.add_enabled(master, egui::Button::new("Clear"));
                    if clear.clicked() {
                        b.keys.clear();
                        b.enabled = false;
                    }
                    toggle_rec = rec.clicked();
                });
                *binding_mut(app, a) = b;
                if toggle_rec {
                    app.recording = if recording { None } else { Some(a) };
                }
                ui.colored_label(color, status);
                ui.end_row();
            }
        });
    ui.add_space(10.0);
    ui.weak(
        "Click Record, then press the key combination. Hotkeys are registered by the running app; \
         \"in use by another app\" means another program already owns that combination.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egui_key_names_map_to_vk() {
        assert_eq!(
            hotkeys::vk_from_token(egui::Key::A.name()),
            Some('A' as u32)
        );
        assert_eq!(
            hotkeys::vk_from_token(egui::Key::Num5.name()),
            Some('5' as u32)
        );
        assert_eq!(hotkeys::vk_from_token(egui::Key::F11.name()), Some(0x7A));
        assert_eq!(
            hotkeys::vk_from_token(egui::Key::ArrowDown.name()),
            Some(0x28)
        );
        assert_eq!(hotkeys::vk_from_token(egui::Key::PageUp.name()), Some(0x21));
        assert_eq!(hotkeys::vk_from_token(egui::Key::Space.name()), Some(0x20));
        assert_eq!(hotkeys::vk_from_token(egui::Key::Minus.name()), Some(0xBD));
        assert_eq!(hotkeys::vk_from_token(egui::Key::Copy.name()), None);
    }
}
