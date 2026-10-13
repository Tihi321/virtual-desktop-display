//! General options panel.
use super::app::SettingsApp;
use eframe::egui;

pub fn show(ui: &mut egui::Ui, app: &mut SettingsApp) {
    ui.heading("General");
    ui.add_space(6.0);
    ui.checkbox(&mut app.cfg.autostart, "Start with Windows");
    ui.label("Adds the app to your Windows startup (current user only, no admin rights).");
    ui.add_space(8.0);
    ui.checkbox(
        &mut app.cfg.start_on_launch,
        "Start splitting when the app launches",
    );
    ui.label("Splits that are enabled and whose display is connected start right away.");
    ui.add_space(12.0);
    if let Some(p) = crate::config::Config::path() {
        ui.weak(format!("Config file: {}", p.display()));
    }
}
