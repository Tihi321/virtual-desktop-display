//! Splits panel: list on the left, preview + editor of the selected split.
use super::app::SettingsApp;
use super::model::{self, PieceStatus, MAX_STEPPER, REFRESH_CHOICES};
use super::preview;
use crate::config::{LayoutKind, SplitConfig};
use crate::displays::{self, TargetInfo};
use crate::geometry;
use crate::modes::{Mode, SlotPlan};
use eframe::egui;

fn display_label(t: &TargetInfo) -> String {
    let mut s = format!(
        "{} \u{2014} {}\u{d7}{}",
        t.friendly_name, t.rect.w, t.rect.h
    );
    if t.primary {
        s.push_str(" \u{2014} primary");
    }
    s
}

/// `-` / `+` stepper within `1..=max`. True if the value changed.
fn stepper(ui: &mut egui::Ui, label: &str, v: &mut u32, max: u32) -> bool {
    let before = *v;
    ui.horizontal(|ui| {
        ui.label(label);
        if ui
            .add_enabled(*v > 1, egui::Button::new("\u{2212}"))
            .clicked()
        {
            *v -= 1;
        }
        ui.label(format!("{v}"));
        if ui.add_enabled(*v < max, egui::Button::new("+")).clicked() {
            *v += 1;
        }
    });
    *v != before
}

fn add_split(app: &mut SettingsApp) {
    let used: Vec<_> = app
        .cfg
        .splits
        .iter()
        .filter_map(|sc| displays::resolve_split(&app.targets, sc).map(|t| t.key))
        .collect();
    let free = app.real().find(|t| !used.contains(&t.key)).cloned();
    let mut sc = SplitConfig {
        name: format!("Split {}", app.cfg.splits.len() + 1),
        ..SplitConfig::default()
    };
    if let Some(t) = free {
        sc.name = t.friendly_name.clone();
        sc.display_id = t.display_id().unwrap_or_default();
        sc.name_match = t.friendly_name.clone();
        // Wide displays split into columns, tall ones into rows.
        if t.rect.w > t.rect.h {
            model::set_layout(&mut sc, LayoutKind::Columns);
        }
    }
    model::normalize(&mut sc);
    app.cfg.splits.push(sc);
    app.selected = app.cfg.splits.len() - 1;
}

pub fn show(ui: &mut egui::Ui, app: &mut SettingsApp) {
    egui::Panel::left("split_list")
        .default_size(190.0)
        .show(ui, |ui| list(ui, app));
    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Layout preview");
            preview::show(ui, app);
            ui.add_space(8.0);
            editor(ui, app);
        });
    });
}

fn list(ui: &mut egui::Ui, app: &mut SettingsApp) {
    ui.add_space(4.0);
    ui.heading("Splits");
    ui.add_space(4.0);
    if app.first_run && app.cfg.splits.is_empty() {
        ui.weak("Welcome! Add a split to cut one of your displays into several virtual ones.");
        ui.add_space(4.0);
    }
    for i in 0..app.cfg.splits.len() {
        let sc = &app.cfg.splits[i];
        let mut text = if sc.name.trim().is_empty() {
            format!("Split {}", i + 1)
        } else {
            sc.name.clone()
        };
        if !sc.enabled {
            text.push_str(" (off)");
        }
        if ui.selectable_label(app.selected == i, text).clicked() {
            app.selected = i;
        }
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Add split").clicked() {
            add_split(app);
        }
        let can_remove = app.selected < app.cfg.splits.len();
        if ui
            .add_enabled(can_remove, egui::Button::new("Remove"))
            .clicked()
        {
            app.cfg.splits.remove(app.selected);
            app.selected = app.selected.min(app.cfg.splits.len().saturating_sub(1));
        }
    });
    ui.add_space(8.0);
    let total = model::total_pieces(&app.cfg);
    ui.label(format!(
        "Virtual displays: {total} of {}",
        crate::vdd::parsec::VDD_MAX_DISPLAYS
    ));
}

fn editor(ui: &mut egui::Ui, app: &mut SettingsApp) {
    let idx = app.selected;
    if idx >= app.cfg.splits.len() {
        ui.label("No split selected. Use \"Add split\".");
        return;
    }
    let (_, plan) = app.slot_plan();
    let targets = app.targets.clone();
    let registered = app.registered.clone();
    let mut sc = app.cfg.splits[idx].clone();
    edit_split(ui, idx, &mut sc, &targets, &registered, &plan);
    app.cfg.splits[idx] = sc;
}

fn edit_split(
    ui: &mut egui::Ui,
    idx: usize,
    sc: &mut SplitConfig,
    targets: &[TargetInfo],
    registered: &[Mode],
    plan: &SlotPlan,
) {
    let real: Vec<&TargetInfo> = targets.iter().filter(|t| !t.is_virtual()).collect();
    let cur = displays::resolve_split(targets, sc);

    egui::Grid::new(("split_grid", idx))
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut sc.name);
            ui.end_row();

            ui.label("Display");
            let selected_text = match cur {
                Some(t) => display_label(t),
                None if sc.display_id.is_empty() && sc.name_match.is_empty() => {
                    "Choose a display...".to_string()
                }
                None => format!(
                    "{} (not connected)",
                    if sc.name_match.is_empty() {
                        &sc.display_id
                    } else {
                        &sc.name_match
                    }
                ),
            };
            egui::ComboBox::from_id_salt(("display", idx))
                .width(360.0)
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for t in &real {
                        let is_cur = cur.is_some_and(|c| c.key == t.key);
                        if ui.selectable_label(is_cur, display_label(t)).clicked() {
                            let auto_named = sc.name.trim().is_empty()
                                || sc.name == sc.name_match
                                || sc.name.starts_with("Split ");
                            sc.display_id = t.display_id().unwrap_or_default();
                            sc.name_match = t.friendly_name.clone();
                            if auto_named {
                                sc.name = t.friendly_name.clone();
                            }
                        }
                    }
                });
            ui.end_row();

            ui.label("Enabled");
            ui.checkbox(&mut sc.enabled, "");
            ui.end_row();

            ui.label("Auto-split on connect");
            ui.checkbox(&mut sc.auto_on_connect, "")
                .on_hover_text("Start this split automatically when its display is connected");
            ui.end_row();

            ui.label("Layout");
            ui.horizontal(|ui| {
                for (k, name) in [
                    (LayoutKind::Columns, "Columns"),
                    (LayoutKind::Rows, "Rows"),
                    (LayoutKind::Grid, "Grid"),
                    (LayoutKind::Custom, "Custom"),
                ] {
                    if ui.selectable_label(sc.layout == k, name).clicked() {
                        model::set_layout(sc, k);
                    }
                }
            });
            ui.end_row();

            ui.label("Pieces");
            ui.vertical(|ui| counts(ui, sc));
            ui.end_row();

            ui.label("Order");
            ui.horizontal(|ui| {
                let n = model::piece_count(sc);
                let order = geometry::repair_order(&sc.order, n);
                let text = order
                    .iter()
                    .map(|c| (c + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                if ui
                    .add_enabled(n > 1, egui::Button::new("Rotate"))
                    .on_hover_text("Rotate which part of the display each virtual display shows")
                    .clicked()
                {
                    sc.order = geometry::rotate_order(&order, n);
                }
                ui.label(format!("virtual display 1, 2, ... show cells {text}"));
            });
            ui.end_row();

            if cur.is_some_and(|t| t.primary) {
                ui.label("Main piece");
                let n = model::piece_count(sc);
                egui::ComboBox::from_id_salt(("main", idx))
                    .selected_text(format!("Cell {}", sc.main_piece + 1))
                    .show_ui(ui, |ui| {
                        for c in 0..n {
                            if ui
                                .selectable_label(sc.main_piece == c, format!("Cell {}", c + 1))
                                .clicked()
                            {
                                sc.main_piece = c;
                            }
                        }
                    });
                ui.end_row();
            }

            ui.label("Refresh rate");
            egui::ComboBox::from_id_salt(("hz", idx))
                .selected_text(format!("{} Hz", sc.refresh_hz))
                .show_ui(ui, |ui| {
                    let mut choices = REFRESH_CHOICES.to_vec();
                    if !choices.contains(&sc.refresh_hz) {
                        choices.push(sc.refresh_hz);
                        choices.sort_unstable();
                    }
                    for hz in choices {
                        if ui
                            .selectable_label(sc.refresh_hz == hz, format!("{hz} Hz"))
                            .clicked()
                        {
                            sc.refresh_hz = hz;
                        }
                    }
                });
            ui.end_row();
        });

    model::normalize(sc);

    ui.add_space(10.0);
    ui.strong("Pieces");
    let Some(t) = cur else {
        ui.weak("The display is not connected: piece sizes are unknown.");
        return;
    };
    let rows = model::piece_rows(sc, (t.rect.w, t.rect.h), t.primary, registered, plan);
    egui::Grid::new(("pieces", idx))
        .striped(true)
        .num_columns(5)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for h in ["#", "Size", "Position", "Resolution", ""] {
                ui.strong(h);
            }
            ui.end_row();
            for p in &rows {
                ui.label(if p.main {
                    format!("{} \u{2605}", p.index + 1)
                } else {
                    format!("{}", p.index + 1)
                });
                ui.label(format!("{}\u{d7}{}", p.rect.w, p.rect.h));
                ui.label(format!("{}, {}", p.rect.x, p.rect.y));
                ui.label(format!(
                    "{}@{}",
                    format_args!("{}x{}", p.mode.w, p.mode.h),
                    p.mode.hz
                ));
                let (text, color) = match p.status {
                    PieceStatus::Builtin => (
                        "built-in \u{2713}".to_string(),
                        egui::Color32::from_rgb(120, 200, 120),
                    ),
                    PieceStatus::Registered => (
                        "registered \u{2713}".to_string(),
                        egui::Color32::from_rgb(120, 200, 120),
                    ),
                    PieceStatus::NeedsSlot => (
                        "needs custom slot (Driver tab)".to_string(),
                        egui::Color32::from_rgb(230, 190, 80),
                    ),
                    PieceStatus::Overflow(m) => (
                        format!(
                            "overflow \u{2192} fallback {}\u{d7}{} (stretched)",
                            m.w, m.h
                        ),
                        egui::Color32::from_rgb(230, 90, 90),
                    ),
                };
                ui.colored_label(color, text);
                ui.end_row();
            }
        });
}

/// Column / row steppers (and the custom ratio sliders).
fn counts(ui: &mut egui::Ui, sc: &mut SplitConfig) {
    match sc.layout {
        LayoutKind::Columns => {
            if stepper(ui, "Columns", &mut sc.columns, MAX_STEPPER) {
                sc.order.clear();
            }
        }
        LayoutKind::Rows => {
            if stepper(ui, "Rows", &mut sc.rows, MAX_STEPPER) {
                sc.order.clear();
            }
        }
        LayoutKind::Grid => {
            let a = stepper(ui, "Columns", &mut sc.columns, MAX_STEPPER);
            let b = stepper(ui, "Rows", &mut sc.rows, MAX_STEPPER);
            if a || b {
                sc.order.clear();
            }
        }
        LayoutKind::Custom => {
            model::normalize(sc);
            let mut nx = sc.ratios_x.len() as u32;
            let mut ny = sc.ratios_y.len() as u32;
            if stepper(ui, "Columns", &mut nx, MAX_STEPPER) {
                sc.ratios_x = model::equal_ratios(nx as usize);
                sc.order.clear();
            }
            axis_sliders(ui, "Column boundaries", &mut sc.ratios_x);
            if stepper(ui, "Rows", &mut ny, MAX_STEPPER) {
                sc.ratios_y = model::equal_ratios(ny as usize);
                sc.order.clear();
            }
            axis_sliders(ui, "Row boundaries", &mut sc.ratios_y);
        }
    }
}

/// One slider per inner boundary; every piece keeps at least 10 %.
fn axis_sliders(ui: &mut egui::Ui, label: &str, ratios: &mut [f64]) {
    let n = ratios.len();
    if n < 2 {
        return;
    }
    ui.weak(label);
    for k in 0..n - 1 {
        let (lo, hi) = model::boundary_range(ratios, k);
        let mut v = model::boundaries(ratios)[k];
        let resp = ui.add(
            egui::Slider::new(&mut v, lo..=hi)
                .custom_formatter(|x, _| format!("{:.0}%", x * 100.0))
                .custom_parser(|s| {
                    s.trim()
                        .trim_end_matches('%')
                        .parse::<f64>()
                        .ok()
                        .map(|p| p / 100.0)
                }),
        );
        if resp.changed() {
            model::set_boundary(ratios, k, v);
        }
    }
}
