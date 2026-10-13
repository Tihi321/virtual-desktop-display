//! Layout preview: the real displays drawn to scale at their desktop
//! positions; the selected split's display shows its pieces.
use super::app::SettingsApp;
use super::model::{self, PieceStatus};
use crate::displays::{self, TargetInfo};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

const HEIGHT: f32 = 230.0;

fn display_title(t: &TargetInfo) -> String {
    let mut s = format!("{}  {}x{}", t.friendly_name, t.rect.w, t.rect.h);
    if t.primary {
        s.push_str("  (primary)");
    }
    s
}

pub fn show(ui: &mut egui::Ui, app: &SettingsApp) {
    let real: Vec<&TargetInfo> = app
        .real()
        .filter(|t| t.rect.w > 0 && t.rect.h > 0)
        .collect();
    let width = ui.available_width();
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, HEIGHT), Sense::hover());
    painter.rect_filled(resp.rect, 4.0, Color32::from_gray(24));
    if real.is_empty() {
        painter.text(
            resp.rect.center(),
            Align2::CENTER_CENTER,
            "No displays detected",
            FontId::proportional(14.0),
            Color32::GRAY,
        );
        return;
    }

    // Bounding box of all displays in desktop coordinates.
    let min_x = real.iter().map(|t| t.rect.x).min().unwrap_or(0);
    let min_y = real.iter().map(|t| t.rect.y).min().unwrap_or(0);
    let max_x = real.iter().map(|t| t.rect.right()).max().unwrap_or(1);
    let max_y = real.iter().map(|t| t.rect.bottom()).max().unwrap_or(1);
    let (bw, bh) = ((max_x - min_x).max(1) as f32, (max_y - min_y).max(1) as f32);
    let pad = 12.0;
    let avail = resp.rect.shrink(pad);
    let scale = (avail.width() / bw).min(avail.height() / bh);
    let origin = Pos2::new(
        avail.center().x - bw * scale / 2.0,
        avail.center().y - bh * scale / 2.0,
    );
    let to_screen = |x: i32, y: i32| {
        Pos2::new(
            origin.x + (x - min_x) as f32 * scale,
            origin.y + (y - min_y) as f32 * scale,
        )
    };

    let selected_target = app
        .selected_split()
        .and_then(|sc| displays::resolve_split(&app.targets, sc));
    let (_, plan) = app.slot_plan();

    for t in &real {
        let r = Rect::from_min_size(
            to_screen(t.rect.x, t.rect.y),
            Vec2::new(t.rect.w as f32 * scale, t.rect.h as f32 * scale),
        );
        let is_sel = selected_target.is_some_and(|s| s.key == t.key);
        let split_here =
            app.cfg.splits.iter().any(|sc| {
                displays::resolve_split(&app.targets, sc).is_some_and(|x| x.key == t.key)
            });
        let fill = if is_sel {
            Color32::from_rgb(36, 52, 74)
        } else if split_here {
            Color32::from_rgb(40, 44, 54)
        } else {
            Color32::from_gray(44)
        };
        painter.rect_filled(r, 2.0, fill);
        painter.rect_stroke(
            r,
            2.0,
            Stroke::new(if is_sel { 2.0 } else { 1.0 }, Color32::from_gray(150)),
            StrokeKind::Inside,
        );
        painter.text(
            r.left_top() + Vec2::new(6.0, 4.0),
            Align2::LEFT_TOP,
            display_title(t),
            FontId::proportional(12.0),
            Color32::from_gray(210),
        );

        if !is_sel {
            continue;
        }
        let Some(sc) = app.selected_split() else {
            continue;
        };
        let rows = model::piece_rows(sc, (t.rect.w, t.rect.h), t.primary, &app.registered, &plan);
        for p in &rows {
            let pr = Rect::from_min_size(
                r.min + Vec2::new(p.rect.x as f32 * scale, p.rect.y as f32 * scale),
                Vec2::new(p.rect.w as f32 * scale, p.rect.h as f32 * scale),
            );
            if p.status.is_problem() {
                let tint = match p.status {
                    PieceStatus::Overflow(_) => Color32::from_rgba_unmultiplied(220, 60, 60, 70),
                    _ => Color32::from_rgba_unmultiplied(230, 170, 40, 70),
                };
                painter.rect_filled(pr, 0.0, tint);
            }
            painter.rect_stroke(
                pr,
                0.0,
                Stroke::new(1.5, Color32::from_rgb(110, 170, 240)),
                StrokeKind::Inside,
            );
            let label = if p.main {
                format!("{} \u{2605}", p.index + 1)
            } else {
                format!("{}", p.index + 1)
            };
            painter.text(
                pr.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(22.0),
                Color32::WHITE,
            );
        }
    }
}
