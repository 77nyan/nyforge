//! Settings: put the tools of the start screen in the order you like, and switch the optional NyTest on.
//! Everything is saved the moment it changes.

use super::{Screen, Shell};
use crate::icons::Icon;
use crate::settings::Settings;
use crate::theme::*;
use crate::widgets::{self, panel, paint_step_icon_tinted, step_header, Btn};
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, FontId, Id, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType};

fn tool_icon(s: Screen) -> Icon {
    match s {
        Screen::Flash => Icon::Flash,
        Screen::Boot => Icon::Boot,
        Screen::Format => Icon::Format,
        _ => Icon::Test,
    }
}

pub fn tool_name(s: Screen) -> &'static str {
    match s {
        Screen::Flash => "NyFlash",
        Screen::Boot => "NyBoot",
        Screen::Format => "NyFormat",
        _ => "NyTest",
    }
}

pub fn ui(ui: &mut Ui, sh: &mut Shell) {
    widgets::page_title(ui, "Settings", "Choose which tools you see and in what order.");
    let before = sh.settings.clone();

    // ---- the start screen
    panel(ui, |ui| {
        ui.horizontal(|ui| {
            step_header(ui, 1, "Start screen", false);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let changed = sh.settings.order != Settings::default().order;
                if widgets::link_button(ui, "Reset order", changed).clicked() {
                    sh.settings.order = Settings::default().order;
                }
            });
        });
        ui.add_space(2.0);
        ui.label(small_muted("Drag a tool to a new place, or use the arrows under it. This is the order of the start screen."));
        ui.add_space(12.0);
        tiles(ui, &mut sh.settings);
    });
    ui.add_space(12.0);

    // ---- optional tools
    panel(ui, |ui| {
        step_header(ui, 2, "Optional tools", false);
        ui.add_space(8.0);
        optional_tool(ui, &mut sh.settings);
    });
    ui.add_space(14.0);
    ui.vertical_centered(|ui| ui.label(small_muted("Your choices are saved automatically.")));
    ui.add_space(14.0);
    let open_id = Id::new("whats-new-open");
    if version_badge(ui).clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(open_id, true));
    }
    ui.add_space(10.0);
    if ui.ctx().data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false) && whats_new(ui.ctx()) {
        ui.ctx().data_mut(|d| d.insert_temp(open_id, false));
    }

    if sh.settings != before {
        sh.settings.save();
    }
}

/// A small, quiet "NyForge v0.1.8" label at the bottom: which version of the program is running.
fn version_badge(ui: &mut Ui) -> egui::Response {
    let name = "NyForge";
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let name_font = FontId::new(12.5, semibold());
    let ver_font = FontId::new(12.5, FontId::proportional(12.5).family);
    let (name_w, ver_w) = ui.fonts(|f| (f.layout_no_wrap(name.into(), name_font.clone(), TEXT).size().x, f.layout_no_wrap(version.clone(), ver_font.clone(), TEXT).size().x));
    let (pad, dot, gap) = (14.0, 6.0, 8.0);
    let size = vec2(pad * 2.0 + dot + gap + name_w + gap + 1.0 + gap + ver_w, 28.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size.y), Sense::hover());
    let pill = Rect::from_center_size(rect.center(), size);
    let resp = ui.interact(pill, Id::new("version-badge"), Sense::click());
    let hover = hover_t(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.has_focus());
    let p = ui.painter();
    p.rect(pill, CornerRadius::same(14), mix(with_alpha(FIELD, 120), PANEL_HI, hover), Stroke::new(1.0_f32, mix(with_alpha(BORDER, 140), HOVER_LINE, hover)), StrokeKind::Inside);
    let y = pill.center().y;
    let mut x = pill.left() + pad;
    p.circle_filled(pos2(x + dot / 2.0, y), dot / 2.0, ACCENT);
    x += dot + gap;
    p.text(pos2(x, y), egui::Align2::LEFT_CENTER, name, name_font, TEXT);
    x += name_w + gap;
    p.line_segment([pos2(x, y - 7.0), pos2(x, y + 7.0)], Stroke::new(1.0_f32, with_alpha(BORDER_HI, 160)));
    x += 1.0 + gap;
    p.text(pos2(x, y), egui::Align2::LEFT_CENTER, &version, ver_font, MUTED);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{name} version {}: show what changed in every version", env!("CARGO_PKG_VERSION"))));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text("What changed in every version")
}

/// Every version since the first, newest first (changelog.txt next to this program's source).
fn history() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    let mut out: Vec<(&str, &str, Vec<&str>)> = vec![];
    for line in include_str!("../changelog.txt").lines() {
        if let Some(h) = line.strip_prefix("## ") {
            let (v, t) = h.split_once(" | ").unwrap_or((h, ""));
            out.push((v, t, vec![]));
        } else if let (Some(b), Some(last)) = (line.strip_prefix("- "), out.last_mut()) {
            last.2.push(b);
        }
    }
    out
}

/// The version history in a dialog. Returns true when it should close.
fn whats_new(ctx: &egui::Context) -> bool {
    let mut done = false;
    let max_h = (ctx.input(|i| i.screen_rect().height()) * 0.62).clamp(200.0, 520.0);
    let (_, close) = widgets::dialog(ctx, "whats-new", 520.0, |ui| {
        ui.label(widgets::heading("What changed in every version"));
        ui.add_space(8.0);
        egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (i, (v, title, bullets)) in history().into_iter().enumerate() {
                if i > 0 {
                    ui.add_space(12.0);
                }
                ui.horizontal(|ui| {
                    ui.label(semibold_text(format!("v{v}"), 15.0));
                    if i == 0 {
                        ui.label(RichText::new("this version").color(OK_HI).size(12.0));
                    }
                    ui.label(small_muted(title));
                });
                ui.add_space(2.0);
                for b in bullets {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(RichText::new("•").color(DIM));
                        ui.add(egui::Label::new(RichText::new(b).color(MUTED).size(13.0)).wrap());
                    });
                }
            }
        });
        widgets::dialog_buttons(ui, |ui| {
            if Btn::new("Close").show(ui).clicked() {
                done = true;
            }
        });
    });
    done || close
}

// ------------------------------------------------------------------ the tiles

const TILE_H: f32 = 118.0;
const ARROWS_H: f32 = 34.0;

/// The tools as they appear on the start screen, small. Dragging one moves it; the others slide out of the way.
fn tiles(ui: &mut Ui, settings: &mut Settings) {
    let vis = settings.visible();
    let n = vis.len();
    let avail = ui.available_width();
    let tile_w = (avail / n as f32).min(170.0);
    let total = tile_w * n as f32;
    let left = ui.cursor().left() + (avail - total) / 2.0;
    let top = ui.cursor().top();
    let ctx = ui.ctx().clone();
    let drag_id = Id::new("settings-drag");
    let dragging: Option<Screen> = ctx.data(|d| d.get_temp(drag_id));
    let pointer = ctx.input(|i| i.pointer.interact_pos());

    // where the dragged tool would land
    let from = dragging.and_then(|s| vis.iter().position(|v| *v == s));
    let target = match (from, pointer) {
        (Some(_), Some(p)) => Some((((p.x - left) / tile_w).floor().max(0.0) as usize).min(n - 1)),
        _ => None,
    };
    let mut preview = vis.clone();
    if let (Some(f), Some(t)) = (from, target) {
        let s = preview.remove(f);
        preview.insert(t, s);
    }

    let mut moved: Option<(usize, usize)> = None;
    let mut new_drag: Option<Option<Screen>> = None;
    let mut dragged_paint: Option<(Screen, f32)> = None;
    for (i, s) in vis.iter().copied().enumerate() {
        // the slot this tool occupies right now (in the preview while something is being dragged)
        let slot = preview.iter().position(|v| *v == s).unwrap_or(i);
        let x_target = left + tile_w * slot as f32;
        let x = ctx.animate_value_with_time(Id::new(("settings-tile-x", s as u8)), x_target, if dragging.is_some() { 0.12 } else { 0.0 });
        let rect = Rect::from_min_size(pos2(x + 5.0, top), vec2(tile_w - 10.0, TILE_H));
        let is_dragged = dragging == Some(s);
        let resp = ui.interact(rect, Id::new(("settings-tile", s as u8)), Sense::click_and_drag());
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{}: position {} of {}", tool_name(s), i + 1, n)));
        if resp.drag_started() {
            new_drag = Some(Some(s));
        }
        if resp.drag_stopped() {
            if let (Some(f), Some(t)) = (from, target) {
                if f != t {
                    moved = Some((f, t));
                }
            }
            new_drag = Some(None);
        }
        if is_dragged {
            dragged_paint = Some((s, x_target));
            continue;
        }
        let hover = hover_t(&ctx, resp.id.with("h"), (resp.hovered() || resp.has_focus()) && dragging.is_none());
        paint_tile(ui, rect, s, hover, 1.0, false);
        if resp.hovered() && dragging.is_none() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    // the dragged tool follows the pointer, above the others
    if let (Some((s, _)), Some(p)) = (dragged_paint, pointer) {
        let x = (p.x - tile_w / 2.0).clamp(left - 8.0, left + total - tile_w + 8.0);
        paint_tile(ui, Rect::from_min_size(pos2(x + 5.0, top - 4.0), vec2(tile_w - 10.0, TILE_H)), s, 1.0, 0.96, true);
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        ctx.request_repaint();
    }
    ui.allocate_space(vec2(total, TILE_H));

    // arrows: the same move without dragging
    ui.add_space(4.0);
    let ay = ui.cursor().top();
    let mut step: Option<(usize, usize)> = None;
    for (i, _) in vis.iter().enumerate() {
        let cx = left + tile_w * (i as f32 + 0.5);
        for (k, dir) in [(-1.0f32, -1i32), (1.0, 1)] {
            let r = Rect::from_center_size(pos2(cx + k * 20.0, ay + ARROWS_H / 2.0), vec2(30.0, 30.0));
            let to = i as i32 + dir;
            let ok = dragging.is_none() && to >= 0 && (to as usize) < n;
            if arrow_button(ui, r, dir < 0, ok, i, tool_name(vis[i])).clicked() {
                step = Some((i, to as usize));
            }
        }
    }
    ui.allocate_space(vec2(total, ARROWS_H));

    if let Some(d) = new_drag {
        ctx.data_mut(|m| match d {
            Some(s) => m.insert_temp(drag_id, s),
            None => m.remove::<Screen>(drag_id),
        });
    }
    if let Some((f, t)) = moved.or(step) {
        settings.move_tool(f, t);
    }
}

fn paint_tile(ui: &Ui, rect: Rect, s: Screen, hover: f32, opacity: f32, lifted: bool) {
    let mut p = ui.painter().clone();
    p.set_opacity(opacity);
    let p = &p;
    if lifted {
        p.add(egui::Shadow { offset: [0, 8], blur: 18, spread: 0, color: Color32::from_black_alpha(90) }.as_shape(rect.shrink(6.0), CornerRadius::same(14)));
        p.rect(rect, CornerRadius::same(14), PANEL_HI, Stroke::new(1.5_f32, HOVER_LINE), StrokeKind::Inside);
    } else {
        p.rect(rect, CornerRadius::same(14), with_alpha(FIELD, 120), Stroke::new(1.0_f32, mix(with_alpha(BORDER, 140), HOVER_LINE, hover)), StrokeKind::Inside);
    }
    paint_step_icon_tinted(p, Rect::from_center_size(pos2(rect.center().x, rect.top() + 46.0), vec2(64.0, 64.0)), tool_icon(s), 0.0, hover, true, 1.0, HOVER_LINE, HOVER_GLYPH);
    p.text(pos2(rect.center().x, rect.top() + 90.0), egui::Align2::CENTER_TOP, tool_name(s), FontId::new(14.5, semibold()), TEXT);
}

fn arrow_button(ui: &mut Ui, rect: Rect, left: bool, enabled: bool, index: usize, name: &str) -> egui::Response {
    let id = Id::new(("settings-arrow", index, left));
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let label = format!("Move {name} {}", if left { "left" } else { "right" });
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label.clone()));
    let hover = hover_t(ui.ctx(), id.with("h"), enabled && resp.hovered());
    let down = enabled && resp.is_pointer_button_down_on();
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(15), if down { FIELD } else { with_alpha(PANEL_HI, (hover * 255.0) as u8) }, Stroke::new(1.2_f32, if enabled { mix(BORDER_HI, HOVER_LINE, hover) } else { with_alpha(BORDER, 90) }), StrokeKind::Inside);
    let c = rect.center();
    let d = if left { 1.0 } else { -1.0 };
    let col = if enabled { mix(MUTED, TEXT, hover) } else { with_alpha(DIM, 150) };
    let st = Stroke::new(1.8_f32, col);
    p.line_segment([pos2(c.x + 2.5 * d, c.y - 5.0), pos2(c.x - 2.5 * d, c.y)], st);
    p.line_segment([pos2(c.x - 2.5 * d, c.y), pos2(c.x + 2.5 * d, c.y + 5.0)], st);
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

// ------------------------------------------------------------------ NyTest

fn optional_tool(ui: &mut Ui, settings: &mut Settings) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
        let on = settings.nytest;
        paint_step_icon_tinted(ui.painter(), r, Icon::Test, if on { 1.0 } else { 0.0 }, 0.0, true, 1.0, if on { ACCENT } else { BORDER_HI }, if on { TEXT } else { MUTED });
        ui.add_space(6.0);
        let btn_w = 150.0;
        let text_w = (ui.available_width() - btn_w - 12.0).max(120.0);
        ui.vertical(|ui| {
            ui.set_width(text_w);
            ui.horizontal(|ui| {
                ui.label(semibold_text("NyTest", 16.0));
                if on {
                    ui.label(RichText::new("On").color(OK_HI).size(12.5));
                }
            });
            ui.label(small_muted("Tests the speed and health of a USB stick or SD card and checks that it really has the size it claims. It only tests removable drives, never your computer's own disks."));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let clicked = if on { Btn::new("Turn off").show(ui).clicked() } else { Btn::primary("Enable NyTest").icon(Icon::Check).show(ui).clicked() };
            if clicked {
                settings.nytest = !on;
            }
        });
    });
    ui.add_space(6.0);
    ui.label(small_muted(if settings.nytest { "NyTest is shown on the start screen. It is in the list above, so you can move it too." } else { "Off by default. Turn it on and it appears on the start screen." }));
}
