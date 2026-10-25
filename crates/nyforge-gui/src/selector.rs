//! The one drive picker used by NyFlash, NyBoot and NyFormat.
//!
//! * A drive is selected by identity (`DriveId`), never by its row.
//! * Nothing is ever selected for the person, even when only one drive is plugged in.
//! * The list refreshes itself while the tool is idle, and a selected drive that disappears is dropped
//!   with a notice instead of being silently kept.
//! * While an operation runs, the tool stops calling `poll` and shows its own locked copy of the target.

use crate::drives::{self, Candidate, DriveId, Purpose};
use crate::icons::{self, Icon};
use crate::theme::*;
use crate::widgets::{self, banner, empty_state, icon_button, Tone};
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Id, Rect, RichText, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType};
use nyforge_core::error::NyError;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

pub struct DriveSelector {
    pub purpose: Purpose,
    pub candidates: Vec<Candidate>,
    pub selected: Option<DriveId>,
    pub error: Option<String>,
    /// "SanDisk Ultra was unplugged" and similar; stays until the person selects something.
    pub notice: Option<String>,
    rx: Option<Receiver<Result<Vec<Candidate>, NyError>>>,
    last: Instant,
    pub loaded: bool,
    id: Id,
    /// Is the "Select drive" window open?
    pub open: bool,
    before: Option<DriveId>,
    auto_done: bool,
}

impl DriveSelector {
    pub fn new(purpose: Purpose, id: &str) -> Self {
        DriveSelector { purpose, candidates: vec![], selected: None, error: None, notice: None, rx: None, last: Instant::now() - Duration::from_secs(60), loaded: false, id: Id::new(id), open: false, before: None, auto_done: false }
    }

    pub fn loading(&self) -> bool {
        self.rx.is_some()
    }

    pub fn refresh(&mut self, ctx: &egui::Context) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let (purpose, c) = (self.purpose, ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(drives::scan(purpose));
            c.request_repaint();
        });
        self.rx = Some(rx);
        self.last = Instant::now();
    }

    pub fn selected(&self) -> Option<&Candidate> {
        let id = self.selected.as_ref()?;
        self.candidates.iter().find(|c| c.id == *id && c.is_selectable())
    }

    /// NyBoot screen: when nothing is chosen and exactly one plugged-in drive is a NyBoot drive, choose it.
    /// Happens once per run, so it never fights the person's own choice. Returns true when it chose one.
    pub fn auto_select_nyboot(&mut self) -> bool {
        if self.auto_done || !self.loaded || self.selected.is_some() {
            return false;
        }
        self.auto_done = true;
        let mut it = self.candidates.iter().filter(|c| c.drive.is_nyboot() && c.is_selectable());
        match (it.next(), it.next()) {
            (Some(c), None) => {
                self.selected = Some(c.id.clone());
                true
            }
            _ => false,
        }
    }

    pub fn deselect(&mut self) {
        self.selected = None;
    }

    /// Call every frame while the tool is visible. `idle` is false while an operation runs (no refreshing then).
    /// Returns true when the selection was dropped because the drive went away.
    pub fn poll(&mut self, ctx: &egui::Context, idle: bool) -> bool {
        let mut dropped = false;
        if let Some(rx) = &self.rx {
            if let Ok(res) = rx.try_recv() {
                self.rx = None;
                self.loaded = true;
                match res {
                    Ok(list) => {
                        self.error = None;
                        if let Some(sel) = self.selected.clone() {
                            if !list.iter().any(|c| c.id == sel && c.is_selectable()) {
                                let name = self.candidates.iter().find(|c| c.id == sel).map(|c| c.name.clone()).unwrap_or_else(|| sel.path.clone());
                                self.notice = Some(format!("{name} ({}) was unplugged, changed or became unavailable. Select a drive again.", sel.path));
                                self.selected = None;
                                dropped = true;
                            }
                        }
                        self.candidates = list;
                    }
                    Err(e) => self.error = Some(match &e.hint {
                        Some(h) => format!("{} {}", e.message, h),
                        None => e.message.clone(),
                    }),
                }
            }
        }
        if idle {
            let every = if cfg!(target_os = "linux") { 2500 } else { 6000 };
            if self.rx.is_none() && self.last.elapsed() > Duration::from_millis(every) {
                self.refresh(ctx);
            }
            ctx.request_repaint_after(Duration::from_millis(1000));
        }
        dropped
    }

    /// Opens the "Select drive" window (a click on the step's button).
    pub fn open_picker(&mut self, ctx: &egui::Context) {
        self.before = self.selected.clone();
        self.open = true;
        self.refresh(ctx);
    }

    /// Draws the "Select drive" window when it is open. Returns true when a drive was confirmed with Continue.
    pub fn modal(&mut self, ctx: &egui::Context) -> bool {
        if !self.open {
            return false;
        }
        let (mut go, mut cancel) = (false, false);
        let (_, close) = widgets::dialog(ctx, &format!("{:?}-picker", self.id), 540.0, |ui| {
            ui.label(widgets::heading("Select drive"));
            ui.add_space(8.0);
            self.ui(ui);
            ui.add_space(4.0);
            widgets::dialog_buttons(ui, |ui| {
                let ok = self.selected().is_some();
                if widgets::Btn::primary("Continue").enabled(ok).tip("Select a drive first").show(ui).clicked() {
                    go = true;
                }
                if widgets::Btn::new("Cancel").show(ui).clicked() {
                    cancel = true;
                }
            });
        });
        if go {
            self.open = false;
            return true;
        }
        if cancel || close {
            self.open = false;
            self.selected = self.before.take();
        }
        false
    }

    /// Draws the picker list. Returns true when the person chose a different drive.
    pub fn ui(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(small_muted(match self.purpose {
                Purpose::Format => "Removable drives (USB and SD only)",
                _ => "Removable drives",
            }));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Refresh, "Refresh the drive list", !self.loading()).clicked() {
                    self.refresh(ui.ctx());
                }
                if self.loading() {
                    ui.label(dim("Refreshing…"));
                }
            });
        });
        if let Some(n) = self.notice.clone() {
            if self.selected.is_none() {
                banner(ui, Tone::Warn, "Drive no longer available", &n, None);
            }
        }
        if let Some(e) = self.error.clone() {
            banner(ui, Tone::Error, "Could not look for drives", &e, Some("Press Refresh to try again."));
        } else if !self.loaded {
            ui.add_space(6.0);
            ui.label(muted("Looking for drives…"));
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else if self.candidates.is_empty() {
            let body = match self.purpose {
                Purpose::Format => "Plug in a USB drive or SD card, then press Refresh. Internal disks are never listed here.",
                _ => "Plug in a USB drive or SD card, then press Refresh. Internal disks are never offered.",
            };
            widgets::panel_frame().fill(FIELD).shadow(egui::Shadow::NONE).show(ui, |ui| {
                ui.set_width(ui.available_width());
                empty_state(ui, Icon::Usb, "No compatible drive found", body);
            });
        } else {
            let sel = self.selected.clone();
            let mut pick: Option<DriveId> = None;
            // shorter in short windows, so the dialog's buttons stay on screen
            let list_h = (ui.ctx().input(|i| i.screen_rect().height()) - 250.0).clamp(96.0, 236.0);
            egui::ScrollArea::vertical().id_salt(self.id.with("list")).min_scrolled_height(list_h).max_height(list_h).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for (i, c) in self.candidates.iter().enumerate() {
                    let is_sel = sel.as_ref() == Some(&c.id);
                    if drive_row(ui, c, is_sel, i).clicked() && c.is_selectable() && !is_sel {
                        pick = Some(c.id.clone());
                    }
                }
            });
            if let Some(id) = pick {
                self.selected = Some(id);
                self.notice = None;
                changed = true;
            }
            if let Some(c) = self.selected() {
                for w in &c.warnings {
                    banner(ui, Tone::Warn, "Check this drive carefully", w, None);
                }
            }
        }
        let _ = self.id;
        changed
    }
}

fn drive_row(ui: &mut Ui, c: &Candidate, selected: bool, idx: usize) -> egui::Response {
    let w = ui.available_width();
    let blocked = c.blocked.is_some();
    let sense = if blocked { Sense::hover() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 56.0), sense);
    let label = format!("{}, {}, {}{}", c.name, c.size_text(), c.drive.path, if selected { ", selected" } else { "" });
    resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, !blocked, selected, &label));
    let sel_t = anim(ui.ctx(), resp.id.with("s"), selected, 0.15);
    let hov_t = hover_t(ui.ctx(), resp.id.with("h"), !blocked && resp.hovered());
    let t_in = reveal(ui.ctx(), &format!("row{idx}"), 0.05 * idx.min(6) as f32);
    let mut painter = ui.painter().clone();
    painter.set_opacity(t_in);
    let p = &painter;
    let base = mix(FIELD, PANEL_HI, hov_t);
    let fill = mix(base, mix(FIELD, ACCENT, 0.16), sel_t);
    let border = mix(mix(with_alpha(BORDER, 120), BORDER_HI, hov_t), ACCENT_HI, sel_t);
    p.rect(rect, CornerRadius::same(12), fill, Stroke::new(1.5_f32, border), StrokeKind::Inside);
    let fg = if blocked { DIM } else { TEXT };
    let sub = if blocked { DIM } else { MUTED };
    // icon tile
    let tile = Rect::from_min_size(rect.min + vec2(10.0, 10.0), vec2(36.0, 36.0));
    p.rect(tile, CornerRadius::same(18), mix(PANEL, with_alpha(ACCENT, 70), sel_t), Stroke::NONE, StrokeKind::Inside);
    icons::paint(p, Icon::Usb, tile.shrink(6.0), if blocked { DIM } else { mix(MUTED, Color32::WHITE, sel_t) });
    // right edge: capacity and the selected check
    let right = rect.right() - 12.0;
    let size_g = p.layout_no_wrap(c.size_text(), FontId::new(14.0, semibold()), fg);
    let check_w = 22.0 * sel_t;
    let size_x = right - check_w - size_g.size().x;
    p.galley(pos2(size_x, rect.top() + 11.0), size_g, fg);
    if sel_t > 0.01 {
        icons::paint(p, Icon::Check, Rect::from_center_size(pos2(right - 8.0, rect.top() + 19.0), vec2(16.0, 16.0)), with_alpha(ACCENT_HI, (255.0 * sel_t) as u8));
    }
    // left text, clipped so it never runs under the capacity
    let text_x = tile.right() + 12.0;
    let max_w = (size_x - text_x - 12.0).max(60.0);
    let name = widgets::elide_middle(ui, &c.name, &FontId::new(14.0, semibold()), max_w);
    let line2 = match &c.blocked {
        Some(why) => format!("{} · {}", c.drive.path, why),
        None if c.drive.is_nyboot() => format!("{} · {} · NyBoot drive", c.drive.path, c.kind_text()),
        None => format!("{} · {}", c.drive.path, c.kind_text()),
    };
    let line2s = widgets::elide_middle(ui, &line2, &FontId::new(12.0, egui::FontFamily::Proportional), rect.right() - 12.0 - text_x);
    let p = ui.painter();
    p.text(pos2(text_x, rect.top() + 12.0), Align2::LEFT_TOP, name, FontId::new(14.0, semibold()), fg);
    p.text(pos2(text_x, rect.top() + 33.0), Align2::LEFT_TOP, line2s, FontId::new(12.0, egui::FontFamily::Proportional), sub);
    if !c.warnings.is_empty() && !blocked {
        icons::paint(p, Icon::Warn, Rect::from_center_size(pos2(size_x - 14.0, rect.top() + 19.0), vec2(14.0, 14.0)), WARN);
    }
    if resp.has_focus() {
        widgets::focus_ring(p, rect);
    }
    let tip = format!("{}\n{}\n{} · {}{}", c.name, c.drive.path, c.size_text(), c.kind_text(), if c.drive.partitions.is_empty() { String::new() } else { format!("\nContains: {}", c.drive.contents_summary()) });
    let resp = if !blocked && resp.hovered() { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    let _ = RichText::new("");
    resp.on_hover_text(tip)
}
