//! Driver panel: install state, download & install (background thread),
//! links, and the custom resolution slots.
use super::app::SettingsApp;
use crate::driver as drv;
use crate::modes::{self, Mode};
use eframe::egui;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallState {
    Idle,
    /// A step is running (text shown next to a spinner).
    Working(String),
    Done(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterState {
    Idle,
    /// Waiting for the UAC prompt / the elevated helper.
    Working,
    Done(bool),
    Failed(String),
}

fn set_install(state: &std::sync::Mutex<InstallState>, ctx: &egui::Context, v: InstallState) {
    *state.lock().unwrap() = v;
    ctx.request_repaint();
}

/// Download, verify and launch the official installer on a worker thread.
fn start_install(app: &SettingsApp, ctx: &egui::Context) {
    let state = app.install.clone();
    let ctx = ctx.clone();
    set_install(&state, &ctx, InstallState::Working("Downloading...".into()));
    std::thread::spawn(move || {
        let run = || -> anyhow::Result<String> {
            let path = drv::download_installer(&drv::default_download_dir())?;
            set_install(
                &state,
                &ctx,
                InstallState::Working("Verifying the signature...".into()),
            );
            let signer = drv::verify_signature(&path)?;
            set_install(
                &state,
                &ctx,
                InstallState::Working(format!(
                    "Signature OK ({signer}); starting the installer..."
                )),
            );
            drv::launch_installer(&path)?;
            Ok(signer)
        };
        match run() {
            Ok(signer) => set_install(
                &state,
                &ctx,
                InstallState::Done(format!(
                    "Verified signer: {signer}. The installer was started (accept the UAC prompt). \
                     When it finishes, press Re-check."
                )),
            ),
            Err(e) => set_install(&state, &ctx, InstallState::Failed(format!("{e:#}"))),
        }
    });
}

/// Run the elevated helper that writes the custom modes (UAC prompt).
fn start_register(app: &SettingsApp, ctx: &egui::Context, list: Vec<Mode>) {
    let state = app.register.clone();
    let ctx = ctx.clone();
    *state.lock().unwrap() = RegisterState::Working;
    std::thread::spawn(move || {
        let r = match modes::request_register(&list) {
            Ok(ok) => RegisterState::Done(ok),
            Err(e) => RegisterState::Failed(format!("{e:#}")),
        };
        *state.lock().unwrap() = r;
        ctx.request_repaint();
    });
}

fn link(ui: &mut egui::Ui, app: &mut SettingsApp, text: &str, url: &str) {
    if ui.link(text).on_hover_text(url).clicked() {
        if let Err(e) = drv::open_url(url) {
            app.error(format!("Could not open the link: {e:#}"));
        }
    }
}

pub fn show(ui: &mut egui::Ui, app: &mut SettingsApp) {
    ui.heading("Parsec Virtual Display Driver");
    ui.add_space(6.0);
    match (app.driver.installed, app.driver.version) {
        (true, Some(v)) => {
            ui.colored_label(
                egui::Color32::from_rgb(120, 200, 120),
                format!("Installed (driver interface version {v})"),
            );
        }
        (true, None) => {
            ui.colored_label(egui::Color32::from_rgb(120, 200, 120), "Installed");
        }
        (false, _) => {
            ui.colored_label(egui::Color32::from_rgb(230, 90, 90), "Not installed");
        }
    }
    ui.add_space(6.0);
    ui.label(
        "The splits are made with virtual displays from the Parsec VDD driver. It is not bundled \
         with this app (no redistribution grant): it is downloaded from Parsec.",
    );
    ui.add_space(6.0);

    let install = app.install.lock().unwrap().clone();
    ui.horizontal(|ui| {
        let busy = matches!(install, InstallState::Working(_));
        if ui
            .add_enabled(!busy, egui::Button::new("Download & install driver"))
            .on_hover_text(format!(
                "Downloads {} , verifies the Authenticode signature (must be \"{}\") and starts it.",
                drv::INSTALLER_URL,
                drv::EXPECTED_SIGNER
            ))
            .clicked()
        {
            start_install(app, ui.ctx());
        }
        if ui.button("Re-check").clicked() {
            app.poll_status();
            app.recheck_driver();
        }
        if busy {
            ui.spinner();
        }
    });
    match &install {
        InstallState::Idle => {}
        InstallState::Working(t) => {
            ui.label(t);
        }
        InstallState::Done(t) => {
            ui.colored_label(egui::Color32::from_rgb(120, 200, 120), t);
        }
        InstallState::Failed(t) => {
            ui.colored_label(egui::Color32::from_rgb(230, 90, 90), format!("Failed: {t}"));
        }
    }
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        link(
            ui,
            app,
            "Installer (parsec-vdd-0.45.0.0.exe)",
            drv::INSTALLER_URL,
        );
        link(ui, app, "Project page", drv::PROJECT_URL);
        link(ui, app, "Parsec support article", drv::SUPPORT_URL);
    });

    ui.add_space(14.0);
    ui.separator();
    ui.add_space(6.0);
    ui.heading("Custom resolution slots");
    ui.label(
        "The driver has a built-in list of resolutions and room for 5 custom ones. Pieces whose \
         size is not built in (or that use a refresh rate other than 60 Hz) need a custom slot.",
    );
    ui.add_space(6.0);

    let (req, plan) = app.slot_plan();
    ui.label(format!(
        "{} of {} custom slots used.",
        app.registered.len(),
        modes::MAX_CUSTOM
    ));
    if req.is_empty() {
        ui.label("The splits need no resolutions (no connected display is configured).");
    } else if !plan.needs_write() && plan.overflow.is_empty() {
        ui.colored_label(
            egui::Color32::from_rgb(120, 200, 120),
            "All resolutions the splits need are available.",
        );
    } else {
        let list = |v: &[Mode]| {
            v.iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        if plan.needs_write() {
            ui.colored_label(
                egui::Color32::from_rgb(230, 190, 80),
                format!("Need to register: {}", list(&plan.to_register)),
            );
        }
        if !plan.overflow.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(230, 90, 90),
                format!(
                    "No slot left for: {} (a larger built-in mode is used and stretched).",
                    list(&plan.overflow)
                ),
            );
        }
    }
    if !plan.registered.is_empty() {
        ui.weak(format!(
            "Already registered: {}",
            plan.registered
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    ui.add_space(6.0);
    let reg_state = app.register.lock().unwrap().clone();
    ui.horizontal(|ui| {
        let busy = reg_state == RegisterState::Working;
        if ui
            .add_enabled(
                plan.needs_write() && !busy,
                egui::Button::new("Register custom resolutions (admin)"),
            )
            .on_hover_text(
                "Asks for administrator rights (UAC) and writes HKLM\\SOFTWARE\\Parsec\\vdd.",
            )
            .clicked()
        {
            start_register(app, ui.ctx(), plan.new_registry.clone());
        }
        if busy {
            ui.spinner();
            ui.label("Waiting for the administrator prompt...");
        }
    });
    match reg_state {
        RegisterState::Done(true) => {
            app.registered = modes::read_custom_modes().unwrap_or_default();
            *app.register.lock().unwrap() = RegisterState::Idle;
            app.info("Custom resolutions registered. They apply the next time the splits start.");
        }
        RegisterState::Done(false) => {
            *app.register.lock().unwrap() = RegisterState::Idle;
            app.error("The custom resolutions were not written (the prompt was cancelled or the helper failed).");
        }
        RegisterState::Failed(e) => {
            *app.register.lock().unwrap() = RegisterState::Idle;
            app.error(format!("Registering failed: {e}"));
        }
        _ => {}
    }
    ui.add_space(4.0);
    ui.weak(
        "Warning: this rewrites the Parsec VDD custom modes (the 5 slots). Modes you registered \
         before are kept only while slots are left over; Parsec's own custom modes may be replaced.",
    );
}
