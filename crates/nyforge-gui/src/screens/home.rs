use super::{Screen, Shell};
use crate::icons::Icon;
use crate::logo;
use crate::theme::*;
use crate::widgets::paint_step_icon;
use eframe::egui::{self, pos2, vec2, Align2, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType};

struct Tool {
    screen: Screen,
    icon: Icon,
    name: &'static str,
    line: &'static str,
    detail: &'static str,
}

const TOOLS: [Tool; 3] = [
    Tool { screen: Screen::Flash, icon: Icon::Flash, name: "NyFlash", line: "Flash an image to a USB drive.", detail: "Write ISO and IMG files, including compressed ones." },
    Tool { screen: Screen::Boot, icon: Icon::Boot, name: "NyBoot", line: "Manage a multiboot USB drive.", detail: "Keep several ISOs on one drive and choose at startup." },
    Tool { screen: Screen::Format, icon: Icon::Format, name: "NyFormat", line: "Format a drive and choose a filesystem.", detail: "FAT32, exFAT and more, with an optional full wipe." },
];

pub fn ui(ui: &mut Ui, sh: &mut Shell) {
    let w = (ui.available_width() - 20.0).max(200.0); // keep clear of the scrollbar
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(104.0, 104.0), Sense::hover());
        let t = reveal(ui.ctx(), "home-logo", 0.0);
        let mut p = ui.painter().clone();
        p.set_opacity(t);
        p.add(egui::Shadow { offset: [0, 8], blur: 22, spread: 0, color: with_alpha(ACCENT_DEEP, 90) }.as_shape(r.shrink(9.0), CornerRadius::same(24)));
        // the mark grows in from 80 % with a small overshoot
        logo::paint(&p, Rect::from_center_size(r.center(), r.size() * (0.8 + 0.2 * ease_out_back(t))), true);
        ui.add_space(10.0);
        staged(ui, "home-title", 0.06, |ui| {
            ui.label(semibold_text("NyForge", 30.0));
        });
        ui.add_space(26.0);
    });
    let stack = w < 660.0;
    if stack {
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            for (i, t) in TOOLS.iter().enumerate() {
                if row(ui, t, w.min(440.0), 0.10 + 0.05 * i as f32).clicked() {
                    sh.go = Some(t.screen);
                }
            }
        });
    } else {
        let col_w = (w / 3.0).min(250.0);
        let total = col_w * 3.0;
        let left = ui.cursor().left() + (ui.available_width() - total) / 2.0;
        let top = ui.cursor().top();
        for (i, t) in TOOLS.iter().enumerate() {
            let rect = Rect::from_min_size(pos2(left + col_w * i as f32, top), vec2(col_w, 284.0));
            if column(ui, t, rect, 0.10 + 0.05 * i as f32).clicked() {
                sh.go = Some(t.screen);
            }
        }
        ui.allocate_space(vec2(total, 284.0));
    }
}

/// One tool as a column: round icon, name, description, and a pill that fills with blue when the column is hovered.
fn column(ui: &mut Ui, t: &Tool, rect: Rect, delay: f32) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with(t.name), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{}: {}", t.name, t.line)));
    let hover = hover_t(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.has_focus());
    let down = resp.is_pointer_button_down_on();
    let t_in = reveal(ui.ctx(), t.name, delay);
    let mut p = ui.painter().clone();
    p.set_opacity(t_in);
    let p = &p;
    let rise = (1.0 - t_in) * 14.0;
    let rect = rect.translate(vec2(0.0, rise));
    let cx = rect.center().x;
    let lift = if down { 1.0 } else { -3.0 * hover };
    let icon_rect = Rect::from_center_size(pos2(cx, rect.top() + 50.0 + lift), vec2(96.0, 96.0));
    paint_step_icon(p, icon_rect, t.icon, 0.0, hover, true, 1.0);
    p.text(pos2(cx, rect.top() + 118.0), Align2::CENTER_TOP, t.name, FontId::new(20.0, semibold()), TEXT);
    let gh = crate::widgets::centered_text(p, cx, rect.top() + 148.0, t.line, FontId::new(14.0, egui::FontFamily::Proportional), TEXT, rect.width() - 36.0);
    crate::widgets::centered_text(p, cx, rect.top() + 154.0 + gh, t.detail, FontId::new(12.5, egui::FontFamily::Proportional), MUTED, rect.width() - 44.0);
    // the "Open" pill
    let pill = Rect::from_center_size(pos2(cx, rect.top() + 256.0), vec2(120.0, 36.0));
    let fill = with_alpha(ACCENT, (hover * 255.0) as u8);
    p.rect(pill, CornerRadius::same(18), if down { ACCENT_DEEP } else { fill }, Stroke::new(1.5_f32, mix(BORDER_HI, ACCENT, hover)), StrokeKind::Inside);
    p.text(pill.center(), Align2::CENTER_CENTER, "Open", FontId::new(14.5, semibold()), mix(MUTED, egui::Color32::WHITE, hover));
    if resp.has_focus() {
        crate::widgets::focus_ring_r(p, pill, 18.0);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// The narrow layout: one row per tool.
fn row(ui: &mut Ui, t: &Tool, width: f32, delay: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 96.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{}: {}", t.name, t.line)));
    let hover = hover_t(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.has_focus());
    let t_in = reveal(ui.ctx(), t.name, delay);
    let mut p = ui.painter().clone();
    p.set_opacity(t_in);
    let p = &p;
    let rect = rect.translate(vec2((1.0 - t_in) * -16.0, 0.0));
    p.rect(rect, CornerRadius::same(16), mix(PANEL, PANEL_HI, hover), Stroke::new(1.5_f32, mix(with_alpha(BORDER, 140), ACCENT_HI, hover)), StrokeKind::Inside);
    paint_step_icon(p, Rect::from_center_size(pos2(rect.left() + 52.0, rect.center().y), vec2(64.0, 64.0)), t.icon, 0.0, hover, true, 1.0);
    p.text(pos2(rect.left() + 100.0, rect.center().y - 22.0), Align2::LEFT_TOP, t.name, FontId::new(18.0, semibold()), TEXT);
    p.text(pos2(rect.left() + 100.0, rect.center().y + 2.0), Align2::LEFT_TOP, t.line, FontId::new(13.5, egui::FontFamily::Proportional), MUTED);
    if resp.has_focus() {
        crate::widgets::focus_ring_r(p, rect, 16.0);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}
