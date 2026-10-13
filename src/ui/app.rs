//! Settings window state, periodic polling, save / apply and the frame layout.
use super::driver::{InstallState, RegisterState};
use super::model;
use crate::config::{Config, SplitConfig};
use crate::displays::{self, TargetInfo};
use crate::driver::DriverStatus;
use crate::hotkeys::Action;
use crate::ipc::{self, Msg, StatusFile};
use crate::modes::{self, Mode, SlotPlan};
use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Splits,
    Driver,
    Hotkeys,
    General,
}

pub struct SettingsApp {
    pub cfg: Config,
    /// What is on disk (for the "unsaved changes" marker).
    pub saved: Config,
    /// All active display targets (virtual ones are filtered by `real()`).
    pub targets: Vec<TargetInfo>,
    pub selected: usize,
    pub tab: Tab,
    /// status.json of the tray app (None: never written).
    pub status: Option<StatusFile>,
    pub tray_running: bool,
    pub driver: DriverStatus,
    /// Custom modes currently registered in the driver.
    pub registered: Vec<Mode>,
    pub install: Arc<Mutex<InstallState>>,
    pub register: Arc<Mutex<RegisterState>>,
    /// The hotkey action whose key combination is being recorded.
    pub recording: Option<Action>,
    /// Footer message: (text, is_error).
    pub message: Option<(String, bool)>,
    pub first_run: bool,
    last_poll: Instant,
    last_displays: Instant,
}

impl SettingsApp {
    pub fn new(first_run: bool) -> Self {
        let cfg = Config::load();
        let mut app = Self {
            saved: cfg.clone(),
            cfg,
            targets: Vec::new(),
            selected: 0,
            tab: Tab::Splits,
            status: None,
            tray_running: false,
            driver: DriverStatus {
                installed: false,
                version: None,
                adapter_present: false,
            },
            registered: Vec::new(),
            install: Arc::new(Mutex::new(InstallState::Idle)),
            register: Arc::new(Mutex::new(RegisterState::Idle)),
            recording: None,
            message: None,
            first_run,
            last_poll: Instant::now(),
            last_displays: Instant::now(),
        };
        app.refresh_displays();
        app.poll_status();
        app.recheck_driver();
        app
    }

    /// Detected real (non-virtual) displays.
    pub fn real(&self) -> impl Iterator<Item = &TargetInfo> {
        self.targets.iter().filter(|t| !t.is_virtual())
    }

    pub fn refresh_displays(&mut self) {
        match displays::enumerate() {
            Ok(t) => self.targets = t,
            Err(e) => tracing::warn!("enumerate displays failed: {e:#}"),
        }
        self.last_displays = Instant::now();
    }

    pub fn poll_status(&mut self) {
        self.tray_running = ipc::find_tray().is_some();
        self.status = ipc::read_status();
        self.last_poll = Instant::now();
    }

    /// Driver state and custom modes. While the tray app runs splits the
    /// driver is not opened a second time: its reported state is used.
    pub fn recheck_driver(&mut self) {
        let busy = self.tray_running
            && self
                .status
                .as_ref()
                .is_some_and(|s| matches!(s.state.as_str(), "starting" | "running" | "stopping"));
        if busy {
            if let Some(s) = &self.status {
                self.driver = DriverStatus {
                    installed: s.driver.installed,
                    version: s.driver.version,
                    adapter_present: s.driver.installed,
                };
            }
        } else {
            self.driver = crate::driver::status();
        }
        self.registered = modes::read_custom_modes().unwrap_or_default();
    }

    /// Required modes and slot plan for the current (edited) config.
    pub fn slot_plan(&self) -> (Vec<Mode>, SlotPlan) {
        model::slot_plan(&self.cfg, &self.targets, &self.registered)
    }

    pub fn dirty(&self) -> bool {
        self.cfg != self.saved
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), false));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), true));
    }

    pub fn selected_split(&self) -> Option<&SplitConfig> {
        self.cfg.splits.get(self.selected)
    }

    fn save_and_apply(&mut self) {
        let errs = model::validate(&self.cfg, &self.targets);
        if let Some(e) = errs.first() {
            self.error(format!("Not saved: {e}"));
            return;
        }
        self.cfg.splits.iter_mut().for_each(model::normalize);
        self.cfg.version = crate::config::CONFIG_VERSION;
        if let Err(e) = self.cfg.save() {
            self.error(format!("Could not save the config: {e:#}"));
            return;
        }
        self.saved = self.cfg.clone();
        let mut note = String::new();
        if self.cfg.autostart != crate::autostart::is_enabled() {
            if let Err(e) = crate::autostart::set_enabled(self.cfg.autostart) {
                note = format!(" (Start with Windows could not be changed: {e:#})");
            }
        }
        if ipc::post_to_tray(Msg::Reload) {
            self.info(format!("Saved and applied.{note}"));
        } else {
            self.info(format!(
                "Saved. The app is not running: click \"Start app\" to apply it.{note}"
            ));
        }
        self.poll_status();
    }

    fn start_app(&mut self) {
        match std::env::current_exe().and_then(|e| std::process::Command::new(e).spawn()) {
            Ok(_) => self.info("Starting the app..."),
            Err(e) => self.error(format!("Could not start the app: {e}")),
        }
    }

    fn send(&mut self, msg: Msg, what: &str) {
        if ipc::post_to_tray(msg) {
            self.info(what.to_string());
        } else {
            self.error("The app is not running. Click \"Start app\" first.");
        }
    }

    fn status_line(&self) -> String {
        if !self.tray_running {
            return "App not running".into();
        }
        let Some(s) = &self.status else {
            return "App running".into();
        };
        let mut t = match s.state.as_str() {
            "running" => "Running".to_string(),
            "starting" => "Starting...".to_string(),
            "stopping" => "Stopping...".to_string(),
            "error" => "Stopped (error)".to_string(),
            _ => "Idle".to_string(),
        };
        let running: Vec<&str> = s
            .splits
            .iter()
            .filter(|p| p.running)
            .map(|p| p.name.as_str())
            .collect();
        if !running.is_empty() {
            t.push_str(&format!(": {}", running.join(", ")));
        }
        if s.state == "error" && !s.message.is_empty() {
            t.push_str(&format!(" - {}", s.message));
        }
        t
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        let errs = model::validate(&self.cfg, &self.targets);
        for e in &errs {
            ui.colored_label(egui::Color32::from_rgb(230, 90, 90), e);
        }
        if let Some((m, is_err)) = &self.message {
            let c = if *is_err {
                egui::Color32::from_rgb(230, 90, 90)
            } else {
                egui::Color32::from_rgb(120, 200, 120)
            };
            ui.colored_label(c, m);
        }
        ui.horizontal(|ui| {
            let save = ui.add_enabled(errs.is_empty(), egui::Button::new("Save & apply"));
            if save.clicked() {
                self.save_and_apply();
            }
            if self.dirty() {
                ui.colored_label(egui::Color32::from_rgb(230, 190, 80), "unsaved changes");
            }
            ui.separator();
            if !self.tray_running && ui.button("Start app").clicked() {
                self.start_app();
            }
            if ui
                .add_enabled(self.tray_running, egui::Button::new("Start all"))
                .clicked()
            {
                self.send(Msg::StartAll, "Start requested.");
            }
            if ui
                .add_enabled(self.tray_running, egui::Button::new("Stop all"))
                .clicked()
            {
                self.send(Msg::StopAll, "Stop requested.");
            }
            ui.separator();
            ui.label(self.status_line());
        });
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.last_poll.elapsed() >= Duration::from_secs(1) {
            self.poll_status();
        }
        if self.last_displays.elapsed() >= Duration::from_secs(2) {
            self.refresh_displays();
        }
        ui.ctx().request_repaint_after(Duration::from_millis(500));

        egui::Panel::bottom("footer").show(ui, |ui| {
            ui.add_space(4.0);
            self.footer(ui);
            ui.add_space(4.0);
        });
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (t, label) in [
                    (Tab::Splits, "Splits"),
                    (Tab::Driver, "Driver"),
                    (Tab::Hotkeys, "Hotkeys"),
                    (Tab::General, "General"),
                ] {
                    if ui.selectable_label(self.tab == t, label).clicked() {
                        self.tab = t;
                    }
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Splits => super::splits::show(ui, self),
            Tab::Driver => super::driver::show(ui, self),
            Tab::Hotkeys => super::hotkeys::show(ui, self),
            Tab::General => super::general::show(ui, self),
        });
    }
}
