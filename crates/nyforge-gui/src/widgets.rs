//! Shared building blocks: buttons, panels, progress bars, banners, dialogs. Every screen uses these,
//! so every screen looks and behaves the same.

use crate::icons::{self, Icon};
use crate::theme::*;
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType,
};

// ------------------------------------------------------------------ buttons

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Destructive,
}

pub struct Btn<'a> {
    text: &'a str,
    icon: Option<Icon>,
    kind: Kind,
    enabled: bool,
    min_width: f32,
    tip: Option<&'a str>,
    big: bool,
    small: bool,
}

impl<'a> Btn<'a> {
    pub fn new(text: &'a str) -> Self {
        Btn { text, icon: None, kind: Kind::Secondary, enabled: true, min_width: 0.0, tip: None, big: false, small: false }
    }
    pub fn primary(text: &'a str) -> Self {
        Self::new(text).kind(Kind::Primary)
    }
    pub fn destructive(text: &'a str) -> Self {
        Self::new(text).kind(Kind::Destructive)
    }
    pub fn kind(mut self, k: Kind) -> Self {
        self.kind = k;
        self
    }
    pub fn icon(mut self, i: Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn enabled(mut self, e: bool) -> Self {
        self.enabled = e;
        self
    }
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }
    /// The large call-to-action pill (Flash!, Select image…): taller, wider, bigger type.
    pub fn big(mut self) -> Self {
        self.big = true;
        self.min_width = self.min_width.max(190.0);
        self
    }
    /// A compact pill for the title bar.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }
    fn font_size(&self) -> f32 {
        if self.big { 16.0 } else if self.small { 13.5 } else { 14.5 }
    }
    fn height(&self) -> f32 {
        if self.big { 46.0 } else if self.small { 26.0 } else { 34.0 }
    }
    /// Shown on hover; for a disabled button it says why it is disabled.
    pub fn tip(mut self, t: &'a str) -> Self {
        self.tip = Some(t);
        self
    }

    /// How wide this button will be.
    pub fn width(&self, ui: &Ui) -> f32 {
        let g = ui.painter().layout_no_wrap(self.text.to_string(), FontId::new(self.font_size(), semibold()), Color32::WHITE);
        (g.size().x + if self.icon.is_some() { 24.0 } else { 0.0 } + if self.big { 44.0 } else if self.small { 28.0 } else { 36.0 }).max(self.min_width)
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let fs = self.font_size();
        let font = FontId::new(fs, semibold());
        let galley = ui.painter().layout_no_wrap(self.text.to_string(), font, Color32::WHITE);
        let icon_w = if self.icon.is_some() { 24.0 } else { 0.0 };
        let w = self.width(ui);
        let size = vec2(w, self.height());
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(size, sense);
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, self.enabled, self.text));
        let id = resp.id;
        let ctx = ui.ctx().clone();
        let hover = hover_t(&ctx, id.with("h"), self.enabled && resp.hovered());
        let down = self.enabled && resp.is_pointer_button_down_on();
        let (fill, border, fg) = if !self.enabled {
            (FIELD, FIELD, DIM)
        } else {
            match self.kind {
                Kind::Primary => {
                    let base = mix(ACCENT, ACCENT_HI, hover * 0.65);
                    (if down { ACCENT_DEEP } else { base }, if down { ACCENT_DEEP } else { base }, ON_ACCENT)
                }
                Kind::Secondary => {
                    // An outlined pill that fills softly on hover.
                    let base = with_alpha(PANEL_HI, (hover * 255.0) as u8);
                    (if down { FIELD } else { base }, mix(BORDER_HI, mix(TEXT, ACCENT_HI, 0.35), hover * 0.75), TEXT)
                }
                Kind::Destructive => {
                    let base = mix(DANGER_DEEP, DANGER, hover);
                    (if down { mix(DANGER_DEEP, Color32::BLACK, 0.2) } else { base }, base, Color32::WHITE)
                }
            }
        };
        // hover: the pill rises a little and grows; pressing sinks it back
        let lift = if self.enabled { -hover * if self.big { 2.0 } else { 1.2 } } else { 0.0 };
        let grow = if self.enabled { hover * if self.big { 2.5 } else { 1.2 } } else { 0.0 };
        let r = if down { rect.translate(vec2(0.0, 1.0)).shrink(0.8) } else { rect.translate(vec2(0.0, lift)).expand(grow) };
        let p = ui.painter();
        let pill = CornerRadius::same((self.height() / 2.0) as u8);
        let filled = self.enabled && matches!(self.kind, Kind::Primary | Kind::Destructive);
        if filled {
            // a plain, soft drop shadow that deepens a little as the button rises
            let a = if down { 25.0 } else { 40.0 + 25.0 * hover };
            p.add(egui::Shadow { offset: [0, (2.0 + 2.0 * hover) as i8], blur: (6.0 + 4.0 * hover) as u8, spread: 0, color: Color32::from_black_alpha(a as u8) }.as_shape(r, pill));
        }
        p.rect(r, pill, fill, Stroke::new(1.5_f32, border), StrokeKind::Inside);
        let total = galley.size().x + icon_w;
        let mut x = r.center().x - total / 2.0;
        if let Some(ic) = self.icon {
            let isz = (if self.big { 20.0 } else { 16.0 }) * (1.0 + 0.08 * hover);
            // the "back" arrow leans left, the others swell slightly
            let nudge = if ic == Icon::Back { -2.5 * hover } else { 0.0 };
            icons::paint(p, ic, Rect::from_center_size(pos2(x + 8.0 + nudge + if self.big { 2.0 } else { 0.0 }, r.center().y), vec2(isz, isz)), fg);
            x += icon_w;
        }
        let galley = p.layout_no_wrap(self.text.to_string(), FontId::new(fs, semibold()), fg);
        p.galley(pos2(x, r.center().y - galley.size().y / 2.0), galley, fg);
        if resp.has_focus() {
            focus_ring_r(p, rect, self.height() / 2.0);
        }
        let resp = if resp.hovered() && self.enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
        match self.tip {
            Some(t) if !self.enabled => resp.on_disabled_hover_text(t),
            Some(t) => resp.on_hover_text(t),
            None => resp,
        }
    }
}

pub fn focus_ring(p: &egui::Painter, rect: Rect) {
    focus_ring_r(p, rect, RADIUS as f32);
}

pub fn focus_ring_r(p: &egui::Painter, rect: Rect, radius: f32) {
    p.rect_stroke(rect.expand(3.0), CornerRadius::same((radius + 3.0) as u8), Stroke::new(2.0_f32, with_alpha(Color32::WHITE, 210)), StrokeKind::Outside);
}

/// A plain text button (Etcher-style "Change", "Remove"): no box, brightens and underlines on hover.
pub fn link_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let font = FontId::new(14.0, semibold());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 8.0, 24.0), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, text));
    let hover = hover_t(ui.ctx(), resp.id.with("h"), enabled && resp.hovered());
    let col = if enabled { mix(MUTED, TEXT, hover) } else { DIM };
    let p = ui.painter();
    let g = p.layout_no_wrap(text.to_string(), font, col);
    let pos = pos2(rect.center().x - g.size().x / 2.0, rect.center().y - g.size().y / 2.0);
    p.galley(pos, g.clone(), col);
    if hover > 0.01 {
        // the underline draws itself out from the middle
        let y = pos.y + g.size().y + 0.5;
        let half = g.size().x / 2.0 * hover;
        let mx = pos.x + g.size().x / 2.0;
        p.line_segment([pos2(mx - half, y), pos2(mx + half, y)], Stroke::new(1.4_f32, with_alpha(ACCENT_HI, (180.0 + 75.0 * hover) as u8)));
    }
    if resp.has_focus() {
        focus_ring(p, rect);
    }
    if enabled && resp.hovered() { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// A square icon-only button with a tooltip (which is also its accessible label).
/// A small round "i" for "tell me why". Always clickable.
pub fn info_button(ui: &mut Ui, label: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    let hover = hover_t(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.has_focus());
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(12), with_alpha(PANEL_HI, (hover * 255.0) as u8), Stroke::new(1.5_f32, mix(BORDER_HI, ACCENT_HI, hover)), StrokeKind::Inside);
    icons::paint(p, Icon::Info, Rect::from_center_size(rect.center(), vec2(14.0 + 1.5 * hover, 14.0 + 1.5 * hover)), mix(MUTED, ACCENT_HI, hover));
    if resp.has_focus() {
        focus_ring_r(p, rect, 12.0);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(label)
}

pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, enabled: bool) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 32.0), sense);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    let hover = hover_t(ui.ctx(), resp.id.with("h"), enabled && resp.hovered());
    let down = enabled && resp.is_pointer_button_down_on();
    let p = ui.painter();
    let fill = if down { FIELD } else { with_alpha(PANEL_HI, (hover * 255.0) as u8) };
    p.rect(rect, CornerRadius::same(16), fill, Stroke::new(1.5_f32, mix(BORDER_HI, mix(TEXT, ACCENT_HI, 0.3), hover * 0.6)), StrokeKind::Inside);
    let fg = if !enabled { DIM } else { mix(MUTED, TEXT, hover) };
    icons::paint(p, icon, Rect::from_center_size(rect.center(), vec2(16.0 + 1.5 * hover, 16.0 + 1.5 * hover)), fg);
    if resp.has_focus() {
        focus_ring_r(p, rect, 16.0);
    }
    let resp = if enabled && resp.hovered() { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    resp.on_hover_text(label)
}

// ------------------------------------------------------------------ containers

pub fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, with_alpha(BORDER, 150)))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::same(18))
        .shadow(egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(50) })
}

pub fn panel<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let idx = next_panel_index();
    staged_by(ui, &format!("panel{idx}"), 0.07 * idx as f32, 6.0, |ui| {
        let out = panel_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
        // a hairline of light along the top edge
        let r = out.response.rect;
        ui.painter().line_segment([pos2(r.left() + 16.0, r.top() + 0.8), pos2(r.right() - 16.0, r.top() + 0.8)], Stroke::new(1.0_f32, with_alpha(Color32::WHITE, 16)));
        out.inner
    })
}

/// A section title with a small green check once that part is complete. (`number` is kept for callers; sections are not numbered.)
pub fn step_header(ui: &mut Ui, _number: usize, title: &str, done: bool) {
    ui.horizontal(|ui| {
        ui.label(semibold_text(title, 16.0));
        if done {
            let (rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            let p = ui.painter();
            p.circle_filled(rect.center(), 9.0, OK);
            icons::paint(p, Icon::Check, rect.shrink(4.0), Color32::WHITE);
        }
    });
}

// ------------------------------------------------------------------ Etcher-style steps

/// The big round icon that heads a step column. `done` adds a green check badge that pops in; `active` lights the ring
/// and sends one ripple outwards when it lights.
pub fn step_icon(ui: &mut Ui, icon: Icon, done: bool, active: f32, enabled: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::hover());
    let id = Id::new(("stepicon", icon as u8, ui.id()));
    let pop = pop_t(ui.ctx(), id.with("done"), done, 0.35);
    let ripple = ripple_t(ui.ctx(), id.with("ripple"), active > 0.5 && enabled);
    paint_step_icon(ui.painter(), rect, icon, pop, active, enabled, ripple);
}

/// Seconds since `on` last turned true (also when it was already true the first time it is asked); `None` while it is false.
fn since_on(ctx: &egui::Context, id: Id, on: bool) -> Option<f32> {
    let now = ctx.input(|i| i.time);
    let pass = ctx.cumulative_pass_nr();
    let start: Option<f64> = ctx.data_mut(|d| {
        let cur: Option<Option<(f64, u64)>> = d.get_temp(id);
        // a break in drawing (another screen, a closed dialog) starts the animation again
        let next = match (on, cur.flatten()) {
            (true, Some((t, last))) if pass <= last + 1 => Some((t, pass)),
            (true, _) => Some((now, pass)),
            (false, _) => None,
        };
        d.insert_temp(id, next);
        next.map(|(t, _)| t)
    });
    start.map(|t| (now - t) as f32)
}

/// 0 → 1 (with a small overshoot on the way) after `on` turns true; 0 while it is false.
pub fn pop_t(ctx: &egui::Context, id: Id, on: bool, secs: f32) -> f32 {
    if reduce_motion() {
        return if on { 1.0 } else { 0.0 };
    }
    match since_on(ctx, id, on) {
        None => 0.0,
        Some(s) => {
            let t = (s / secs).clamp(0.0, 1.0);
            if t < 1.0 {
                ctx.request_repaint();
            }
            ease_out_back(t)
        }
    }
}

/// 0 → 1 once after `on` turns true (a one-shot ripple); 1 while off, when finished, and with reduced motion.
pub fn ripple_t(ctx: &egui::Context, id: Id, on: bool) -> f32 {
    ripple_t_secs(ctx, id, on, 0.6)
}

pub fn ripple_t_secs(ctx: &egui::Context, id: Id, on: bool, secs: f32) -> f32 {
    if reduce_motion() {
        return 1.0;
    }
    match since_on(ctx, id, on) {
        None => 1.0,
        Some(s) => {
            let t = (s / secs).clamp(0.0, 1.0);
            if t < 1.0 {
                ctx.request_repaint();
            }
            t
        }
    }
}

pub fn paint_step_icon(p: &egui::Painter, rect: Rect, icon: Icon, done: f32, active: f32, enabled: bool, ripple: f32) {
    paint_step_icon_tinted(p, rect, icon, done, active, enabled, ripple, ACCENT_HI, ACCENT_HI);
}

/// Same, with the colours the ring and the glyph take when `active` is 1 (the home screen uses light grey for hover).
#[allow(clippy::too_many_arguments)]
pub fn paint_step_icon_tinted(p: &egui::Painter, rect: Rect, icon: Icon, done: f32, active: f32, enabled: bool, ripple: f32, ring: Color32, glyph: Color32) {
    let k = rect.width() / 96.0; // everything is designed at 96 px and scales from there
    let c = rect.center();
    let fade = if enabled { 1.0 } else { 0.45 };
    if ripple < 0.999 {
        let e = ease_out(ripple);
        p.circle_stroke(c, (44.0 + 20.0 * e) * k, Stroke::new((2.0 * k).max(1.0), with_alpha(ACCENT_HI, ((1.0 - e) * 55.0) as u8)));
    }
    for (grow, alpha) in [(5.0_f32, 10u8), (3.0, 14), (1.5, 18)] {
        p.circle_filled(c + vec2(0.0, 4.0 * k), (44.0 + grow) * k, Color32::from_black_alpha((alpha as f32 * fade) as u8));
    }
    p.circle_filled(c, 44.0 * k, with_alpha(FIELD, (255.0 * fade) as u8));
    // soft light inside the disc, strongest near the top
    {
        let mut m = egui::epaint::Mesh::default();
        let centre = c + vec2(0.0, -16.0 * k);
        m.colored_vertex(centre, with_alpha(Color32::WHITE, (8.0 * fade) as u8));
        let n = 40;
        for i in 0..n {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            m.colored_vertex(c + vec2(a.cos(), a.sin()) * 43.0 * k, with_alpha(Color32::WHITE, 0));
        }
        for i in 0..n as u32 {
            m.add_triangle(0, 1 + i, 1 + (i + 1) % n as u32);
        }
        p.add(egui::Shape::mesh(m));
    }
    p.circle_stroke(c, 44.0 * k, Stroke::new((2.0 * k).max(1.5), mix(with_alpha(BORDER_HI, (255.0 * fade) as u8), ring, active)));
    let ic = mix(mix(MUTED, TEXT, 0.5), glyph, active);
    icons::paint(p, icon, Rect::from_center_size(c, vec2(44.0 * k, 44.0 * k)), with_alpha(ic, (255.0 * fade) as u8));
    if done > 0.01 {
        let b = pos2(rect.right() - 10.0 * k, rect.bottom() - 10.0 * k);
        let sc = done.max(0.0);
        p.circle_filled(b, 15.0 * k * sc, BG);
        p.circle_filled(b, 12.0 * k * sc, OK);
        icons::paint(p, Icon::Check, Rect::from_center_size(b, vec2(14.0 * k * sc, 14.0 * k * sc)), Color32::WHITE);
    }
}

/// Lays `n` equal columns across the available width, draws a thin connector between neighbouring icons
/// (bright when `lit[i]`), and calls `add(i, ui)` for each column. Columns are at most 250 px wide.
pub fn step_columns(ui: &mut Ui, n: usize, lit: &[bool], min_height: f32, mut add: impl FnMut(usize, &mut Ui)) {
    let total = ui.available_width();
    if total < 560.0 {
        return step_stack(ui, n, lit, total, add);
    }
    let col_w = (total / n as f32).min(250.0);
    let used = col_w * n as f32;
    let left = ui.cursor().left() + (total - used) / 2.0;
    let top = ui.cursor().top();
    let mut bottom = top + min_height;
    // connectors first, so the icons paint over their ends
    let icon_y = top + 48.0;
    for i in 0..n.saturating_sub(1) {
        let x0 = left + col_w * (i as f32 + 0.5) + 58.0;
        let x1 = left + col_w * (i as f32 + 1.5) - 58.0;
        if x1 > x0 {
            let k = anim(ui.ctx(), ui.id().with(("connector", i)), lit.get(i).copied().unwrap_or(false), 0.35);
            let vis = reveal(ui.ctx(), &format!("conn{i}"), 0.10 + 0.08 * i as f32);
            let mut p = ui.painter().clone();
            p.set_opacity(vis);
            p.line_segment([pos2(x0, icon_y), pos2(x1, icon_y)], Stroke::new(2.0_f32, with_alpha(BORDER_HI, 160)));
            if k > 0.0 {
                p.line_segment([pos2(x0, icon_y), pos2(x0 + (x1 - x0) * k, icon_y)], Stroke::new(2.0_f32, ACCENT_HI));
            }
        }
    }
    for i in 0..n {
        let rect = Rect::from_min_size(pos2(left + col_w * i as f32, top), vec2(col_w, 10.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.with_max_y(top + 2000.0)).layout(Layout::top_down(Align::Center)));
        child.spacing_mut().item_spacing.y = 10.0;
        // each column fades in and rises a little, one after the other; the height stays the same throughout
        let t = reveal(ui.ctx(), &format!("col{i}"), 0.06 * i as f32);
        let rise = (1.0 - t) * 14.0;
        child.set_opacity(t);
        child.add_space(rise);
        add(i, &mut child);
        bottom = bottom.max(child.min_rect().bottom() - rise);
    }
    ui.allocate_space(vec2(total, (bottom - top).max(min_height)));
}

/// The same steps one below the other, for narrow windows (a quarter-screen tile, a phone-sized window).
fn step_stack(ui: &mut Ui, n: usize, lit: &[bool], total: f32, mut add: impl FnMut(usize, &mut Ui)) {
    let w = total.min(340.0);
    let left = ui.cursor().left() + (total - w) / 2.0;
    let gap = 26.0;
    let mut y = ui.cursor().top();
    let start = y;
    for i in 0..n {
        let rect = Rect::from_min_size(pos2(left, y), vec2(w, 10.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.with_max_y(y + 2000.0)).layout(Layout::top_down(Align::Center)));
        child.spacing_mut().item_spacing.y = 10.0;
        let t = reveal(ui.ctx(), &format!("col{i}"), 0.06 * i as f32);
        let rise = (1.0 - t) * 14.0;
        child.set_opacity(t);
        child.add_space(rise);
        add(i, &mut child);
        let bottom = child.min_rect().bottom() - rise;
        if i + 1 < n {
            // a short vertical connector down to the next icon
            let k = anim(ui.ctx(), ui.id().with(("vconnector", i)), lit.get(i).copied().unwrap_or(false), 0.35);
            let x = left + w / 2.0;
            let (y0, y1) = (bottom + 4.0, bottom + gap - 4.0);
            let mut p = ui.painter().clone();
            p.set_opacity(reveal(ui.ctx(), &format!("conn{i}"), 0.10 + 0.08 * i as f32));
            p.line_segment([pos2(x, y0), pos2(x, y1)], Stroke::new(2.0_f32, with_alpha(BORDER_HI, 160)));
            if k > 0.0 {
                p.line_segment([pos2(x, y0), pos2(x, y0 + (y1 - y0) * k)], Stroke::new(2.0_f32, ACCENT_HI));
            }
        }
        y = bottom + gap;
    }
    ui.allocate_space(vec2(total, (y - gap - start).max(10.0)));
}

// ------------------------------------------------------------------ text helpers

/// Wrapped text centred on `cx`, starting at `y`. Returns the height it used.
pub fn centered_text(p: &egui::Painter, cx: f32, y: f32, text: &str, font: FontId, color: Color32, wrap_w: f32) -> f32 {
    let mut job = egui::text::LayoutJob::simple(text.to_string(), font, color, wrap_w);
    job.halign = Align::Center;
    let g = p.layout_job(job);
    let h = g.size().y;
    // with a centred layout the galley's own rect is centred on the anchor
    let pos = pos2(cx - g.rect.center().x, y);
    p.galley(pos, g, color);
    h
}

/// Shortens `text` in the middle so it fits `max_w`, keeping the start and the end (and so a file extension).
pub fn elide_middle(ui: &Ui, text: &str, font: &FontId, max_w: f32) -> String {
    let width = |s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x;
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    // largest number of kept characters that still fits
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let head = mid.div_ceil(2).max(1);
        let tail = mid / 2;
        let cand: String = chars[..head.min(chars.len())].iter().collect::<String>() + "…" + &chars[chars.len().saturating_sub(tail)..].iter().collect::<String>();
        if width(&cand) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let head = lo.div_ceil(2).max(1);
    let tail = lo / 2;
    chars[..head.min(chars.len())].iter().collect::<String>() + "…" + &chars[chars.len().saturating_sub(tail)..].iter().collect::<String>()
}

/// A label that never breaks the layout: too long → shortened in the middle, with the full text on hover.
pub fn fit_label(ui: &mut Ui, text: &str, rich: impl FnOnce(String) -> RichText, font: FontId, max_w: f32) -> Response {
    let shown = elide_middle(ui, text, &font, max_w.max(40.0));
    let cut = shown != text;
    let r = ui.add(egui::Label::new(rich(shown)).wrap_mode(egui::TextWrapMode::Extend));
    if cut {
        r.on_hover_text(text)
    } else {
        r
    }
}

// ------------------------------------------------------------------ progress

/// A determinate bar (smoothly eased) or, when `fraction` is None, an honest indeterminate one.
pub fn progress_bar(ui: &mut Ui, id: Id, fraction: Option<f32>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
    resp.widget_info(|| {
        let mut w = WidgetInfo::new(WidgetType::ProgressIndicator);
        w.value = fraction.map(|f| (f * 100.0) as f64);
        w.label = Some("Progress".into());
        w
    });
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(7), FIELD, Stroke::NONE, StrokeKind::Inside);
    let inner = rect.shrink(2.0);
    match fraction {
        Some(f) => {
            let shown = if reduce_motion() { f } else { ui.ctx().animate_value_with_time(id, f, 0.30) };
            let w = (inner.width() * shown.clamp(0.0, 1.0)).max(if shown > 0.0 { inner.height() } else { 0.0 });
            if w > 0.0 {
                let bar = Rect::from_min_size(inner.min, vec2(w, inner.height()));
                p.rect_filled(bar, CornerRadius::same(5), ACCENT);
                if !reduce_motion() && w > 24.0 && fraction.is_some_and(|f| f < 1.0) {
                    // a soft highlight that sweeps over the filled part
                    let t = ui.input(|i| i.time) as f32;
                    let band = 46.0_f32.min(w);
                    let x = bar.left() - band + (w + band) * ((t * 0.8) % 1.0);
                    let clip = p.with_clip_rect(bar);
                    let mut m = egui::epaint::Mesh::default();
                    let (a, b) = (pos2(x, bar.top()), pos2(x + band, bar.bottom()));
                    let c0 = with_alpha(Color32::WHITE, 0);
                    let c1 = with_alpha(Color32::WHITE, 46);
                    m.colored_vertex(a, c0);
                    m.colored_vertex(pos2(x + band / 2.0, bar.top()), c1);
                    m.colored_vertex(pos2(x + band, bar.top()), c0);
                    m.colored_vertex(pos2(x, bar.bottom()), c0);
                    m.colored_vertex(pos2(x + band / 2.0, bar.bottom()), c1);
                    m.colored_vertex(b, c0);
                    m.add_triangle(0, 1, 4);
                    m.add_triangle(0, 4, 3);
                    m.add_triangle(1, 2, 5);
                    m.add_triangle(1, 5, 4);
                    clip.add(egui::Shape::mesh(m));
                    ui.ctx().request_repaint();
                }
            }
        }
        None => {
            let t = ui.input(|i| i.time) as f32;
            let seg = inner.width() * 0.28;
            let phase = if reduce_motion() { 0.35 } else { (t * 0.9) % 1.0 };
            let x = inner.left() + (inner.width() + seg) * phase - seg;
            let bar = Rect::from_min_max(pos2(x.max(inner.left()), inner.top()), pos2((x + seg).min(inner.right()), inner.bottom()));
            if bar.width() > 0.0 {
                p.rect_filled(bar, CornerRadius::same(5), ACCENT);
            }
            if !reduce_motion() {
                ui.ctx().request_repaint();
            }
        }
    }
}

// ------------------------------------------------------------------ banners and states

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warn,
    Error,
    Success,
}

impl Tone {
    fn color(self) -> Color32 {
        match self {
            Tone::Info => MUTED,
            Tone::Warn => WARN,
            Tone::Error => DANGER,
            Tone::Success => OK,
        }
    }
    fn icon(self) -> Icon {
        match self {
            Tone::Info => Icon::Info,
            Tone::Warn => Icon::Warn,
            Tone::Error => Icon::Error,
            Tone::Success => Icon::Check,
        }
    }
}

/// A status message: icon, bold title, plain explanation, optional hint. Used for every warning and error.
pub fn banner(ui: &mut Ui, tone: Tone, title: &str, body: &str, hint: Option<&str>) {
    let c = tone.color();
    // a message fades in (and out is instant, since it simply stops being drawn)
    let fade = reveal(ui.ctx(), &format!("banner-{title}-{body}"), 0.0);
    ui.scope(|ui| {
    ui.set_opacity(fade);
    egui::Frame::new()
        .fill(with_alpha(c, 30))
        .stroke(Stroke::new(1.0_f32, with_alpha(c, 110)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin { left: 12, right: 12, top: 9, bottom: 9 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                icons::paint(ui.painter(), tone.icon(), r, c);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    if !title.is_empty() {
                        ui.label(semibold_text(title, 13.5));
                    }
                    if !body.is_empty() {
                        ui.label(RichText::new(body).color(if title.is_empty() { TEXT } else { MUTED }));
                    }
                    if let Some(h) = hint {
                        ui.label(RichText::new(h).size(12.5).color(MUTED));
                    }
                });
            });
        });
    });
}

pub fn empty_state(ui: &mut Ui, icon: Icon, title: &str, body: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(14.0);
        let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
        icons::paint(ui.painter(), icon, r, DIM);
        ui.add_space(4.0);
        ui.label(semibold_text(title, 14.0));
        ui.label(small_muted(body));
        ui.add_space(14.0);
    });
}

/// Big round status mark for the result screens: a solid disc with a white symbol.
pub fn status_mark(ui: &mut Ui, id: Id, tone: Tone, show: bool) {
    let t = anim(ui.ctx(), id, show, 0.25);
    let pop = pop_t(ui.ctx(), id.with("pop"), show, 0.45);
    let ripple = ripple_t(ui.ctx(), id.with("ripple"), show);
    let (rect, _) = ui.allocate_exact_size(vec2(90.0, 90.0), Sense::hover());
    let c = tone.color();
    let p = ui.painter();
    let scale = 0.55 + 0.45 * pop;
    let r = 40.7 * scale;
    if ripple < 0.999 {
        let e = ease_out(ripple);
        p.circle_stroke(rect.center(), 40.7 + 23.5 * e, Stroke::new(3.0_f32, with_alpha(c, ((1.0 - e) * 70.0) as u8)));
    }
    p.circle_filled(rect.center(), r, with_alpha(c, (255.0 * t) as u8));
    icons::paint(p, tone.icon(), Rect::from_center_size(rect.center(), vec2(42.9 * scale, 42.9 * scale)), with_alpha(Color32::WHITE, (255.0 * t) as u8));
}

/// The summary under a success title: a narrow card of label / value rows, centred.
pub fn result_card(ui: &mut Ui, rows: &[(&str, String)]) {
    let w = ui.available_width().min(520.0);
    ui.allocate_ui(vec2(w, 10.0), |ui| {
        panel(ui, |ui| {
            for (k, v) in rows {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(60.0, 20.0), Sense::hover());
                    ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, *k, FontId::new(12.5, egui::FontFamily::Proportional), MUTED);
                    let max = ui.available_width() - 4.0;
                    fit_label(ui, v, |s| RichText::new(s), FontId::new(14.5, egui::FontFamily::Proportional), max);
                });
            }
        });
    });
}

/// A key / value row: small muted label on the left, value on the right.
pub fn kv(ui: &mut Ui, key: &str, value: impl Into<RichText>) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(112.0, 18.0), Sense::hover());
        ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, key, FontId::new(12.0, egui::FontFamily::Proportional), MUTED);
        ui.label(value.into());
    });
}

// ------------------------------------------------------------------ dialogs

/// A confirmation / message dialog over a dimmed window. Returns what `add` returned and whether Esc or a click outside asked to close.
pub fn dialog<R>(ctx: &egui::Context, id: &str, width: f32, add: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    // a quick fade-in, with the contents rising a few pixels into place (the dialog's size never changes)
    let t = reveal(ctx, &format!("dialog-{id}"), 0.0);
    // never wider than the window (quarter-screen tiles, small laptops)
    let width = width.min((ctx.input(|i| i.screen_rect().width()) - 56.0).max(240.0));
    let a = |v: u8| (v as f32 * t) as u8;
    let frame = egui::Frame::new()
        .fill(with_alpha(PANEL, a(255)))
        .stroke(Stroke::new(1.0_f32, with_alpha(BORDER_HI, a(255))))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(egui::Margin::same(24))
        .shadow(egui::Shadow { offset: [0, 14], blur: 34, spread: 0, color: Color32::from_black_alpha(a(120)) });
    let m = egui::Modal::new(Id::new(id)).frame(frame).backdrop_color(Color32::from_black_alpha(a(120))).show(ctx, |ui| {
        ui.set_width(width);
        ui.set_opacity(t);
        add(ui)
    });
    let close = m.should_close();
    (m.inner, close)
}

pub fn dialog_buttons<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.add_space(6.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| add(ui)).inner
}

pub fn heading(text: &str) -> RichText {
    semibold_text(text, 18.0)
}


/// The drive named prominently, for confirmation dialogs: name and capacity large, path and contents beneath.
pub fn drive_card(ui: &mut Ui, c: &crate::drives::Candidate) {
    panel_frame().fill(FIELD).shadow(egui::Shadow::NONE).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
            icons::paint(ui.painter(), Icon::Usb, r, ACCENT_HI);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(semibold_text(format!("{}  ·  {}", c.name, c.size_text()), 16.0));
                ui.label(small_muted(format!("{} · {}", c.drive.path, c.kind_text())));
            });
        });
        if !c.drive.partitions.is_empty() {
            ui.add_space(4.0);
            ui.label(small_muted(format!("Currently contains: {}", c.drive.contents_summary())));
        }
    });
}

/// Two buttons side by side, centred in the available width. Returns (clicked a, clicked b).
pub fn centered_pair(ui: &mut Ui, a: Btn, b: Btn) -> (bool, bool) {
    let total = a.width(ui) + b.width(ui) + ui.spacing().item_spacing.x;
    let mut out = (false, false);
    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
        out.0 = a.show(ui).clicked();
        out.1 = b.show(ui).clicked();
    });
    out
}

// ------------------------------------------------------------------ page furniture

/// The centred title block at the top of a tool.
pub fn page_title(ui: &mut Ui, name: &str, line: &str) {
    ui.add_space(6.0);
    ui.vertical_centered(|ui| {
        staged(ui, "page-title", 0.0, |ui| {
            ui.label(semibold_text(name, 22.0));
            ui.label(muted(line));
        });
    });
    ui.add_space(8.0);
}

/// The same header for every running operation: round icon, what is happening, and a reminder not to unplug.
pub fn run_header(ui: &mut Ui, icon: Icon, title: &str) {
    run_header_ex(ui, icon, title, "Do not unplug the drive until this finishes.");
}

/// `run_header` with its own second line (a check of an image file has no drive to unplug).
pub fn run_header_ex(ui: &mut Ui, icon: Icon, title: &str, line: &str) {
    ui.add_space(6.0);
    ui.vertical_centered(|ui| {
        step_icon(ui, icon, false, 1.0, true);
        ui.add_space(4.0);
        staged(ui, "run-title", 0.08, |ui| {
            ui.label(semibold_text(title, 22.0));
            ui.label(small_muted(line));
        });
    });
    ui.add_space(4.0);
}

/// The drive choice for NyBoot and NyFormat: a round icon, the drive (or an invitation to pick one) and a pill that opens
/// the "Select drive" window. Returns true when that pill was clicked.
pub fn drive_slot(ui: &mut Ui, sel: Option<&crate::drives::Candidate>, enabled: bool) -> bool {
    drive_slot_ex(ui, sel, enabled, "No drive selected", "Only removable drives are shown. Nothing is chosen for you.")
}

/// `drive_slot` with its own wording for the empty state (NyTest says which drives it takes).
pub fn drive_slot_ex(ui: &mut Ui, sel: Option<&crate::drives::Candidate>, enabled: bool, empty_title: &str, empty_line: &str) -> bool {
    let mut clicked = false;
    panel(ui, |ui| {
        // in a narrow window the button moves under the text instead of squeezing it
        let narrow = ui.available_width() < 600.0;
        let button = |ui: &mut Ui| -> bool {
            let b = if sel.is_some() { Btn::new("Change") } else { Btn::primary("Select drive") };
            b.enabled(enabled).tip("Not while an operation is running").show(ui).clicked()
        };
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
            let p = ui.painter();
            let c = rect.center();
            p.circle_filled(c, 30.0, FIELD);
            p.circle_stroke(c, 30.0, Stroke::new(2.0_f32, if sel.is_some() { MUTED } else { ACCENT_HI }));
            icons::paint(p, Icon::Usb, Rect::from_center_size(c, vec2(30.0, 30.0)), if sel.is_some() { TEXT } else { ACCENT_HI });
            let pop = pop_t(ui.ctx(), ui.id().with("slot-badge"), sel.is_some(), 0.35);
            if pop > 0.01 {
                let b = pos2(rect.right() - 4.0, rect.bottom() - 4.0);
                p.circle_filled(b, 11.0 * pop, PANEL);
                p.circle_filled(b, 9.0 * pop, OK);
                icons::paint(p, Icon::Check, Rect::from_center_size(b, vec2(11.0 * pop, 11.0 * pop)), Color32::WHITE);
            }
            ui.add_space(6.0);
            let text_w = (ui.available_width() - if narrow { 8.0 } else { 190.0 }).max(80.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                match sel {
                    Some(c) => {
                        fit_label(ui, &c.name, |s| semibold_text(s, 17.0), FontId::new(17.0, semibold()), text_w);
                        fit_label(ui, &format!("{} · {} · {}", c.size_text(), c.drive.path, c.kind_text()), |s| small_muted(s), FontId::new(12.0, egui::FontFamily::Proportional), text_w);
                    }
                    None => {
                        ui.label(semibold_text(empty_title, 17.0));
                        ui.label(small_muted(empty_line));
                    }
                }
            });
            if !narrow {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if button(ui) {
                        clicked = true;
                    }
                });
            }
        });
        if narrow {
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                if button(ui) {
                    clicked = true;
                }
            });
        }
    });
    clicked
}


/// A row of big choice cards: the chosen one is outlined in blue, the others light up grey under the pointer.
/// `items` are (icon, title, one line). Returns true when the choice changed.
pub fn choice_cards(ui: &mut Ui, id: Id, current: &mut usize, items: &[(Icon, &str, &str)], enabled: bool) -> bool {
    let n = items.len().max(1);
    let gap = 10.0;
    let w = ui.available_width();
    let stacked = w < 150.0 * n as f32 + gap * (n as f32 - 1.0) + 120.0;
    let card_w = if stacked { w } else { (w - gap * (n as f32 - 1.0)) / n as f32 };
    // tall enough for the longest description (three lines in a narrow card), never shorter than the usual card
    let card_h = if stacked {
        66.0
    } else {
        let text_h = items
            .iter()
            .map(|(_, _, l)| ui.painter().layout(l.to_string(), FontId::new(12.5, egui::FontFamily::Proportional), MUTED, card_w - 36.0).size().y)
            .fold(0.0_f32, f32::max);
        (54.0 + text_h + 16.0).max(104.0)
    };
    let mut changed = false;
    let mut cur = *current;
    let origin = ui.cursor().min;
    let total_h = if stacked { card_h * n as f32 + gap * (n as f32 - 1.0) } else { card_h };
    let (area, _) = ui.allocate_exact_size(vec2(w, total_h), Sense::hover());
    let _ = origin;
    // One title size for the whole row: the longest title decides, so the cards stay even.
    let title_size = if stacked {
        15.0
    } else {
        let room = card_w - 56.0 - 28.0;
        let widest = items
            .iter()
            .map(|(_, t, _)| ui.painter().layout_no_wrap(t.to_string(), FontId::new(15.5, semibold()), TEXT).size().x)
            .fold(0.0_f32, f32::max);
        if widest > room { (15.5 * room / widest).max(12.0) } else { 15.5 }
    };
    for (i, (icon, title, line)) in items.iter().enumerate() {
        let rect = if stacked {
            Rect::from_min_size(pos2(area.left(), area.top() + (card_h + gap) * i as f32), vec2(card_w, card_h))
        } else {
            Rect::from_min_size(pos2(area.left() + (card_w + gap) * i as f32, area.top()), vec2(card_w, card_h))
        };
        let resp = ui.interact(rect, id.with(("card", i)), if enabled { Sense::click() } else { Sense::hover() });
        let chosen = cur == i;
        resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, enabled, chosen, format!("{title}: {line}")));
        let sel_t = anim(ui.ctx(), resp.id.with("s"), chosen, 0.18);
        let hov_t = hover_t(ui.ctx(), resp.id.with("h"), enabled && resp.hovered());
        let down = enabled && resp.is_pointer_button_down_on();
        let r = if down { rect.shrink(0.8) } else { rect.translate(vec2(0.0, -1.5 * hov_t * (1.0 - sel_t))) };
        let p = ui.painter();
        let fill = mix(mix(PANEL, PANEL_HI, hov_t * 0.8), mix(FIELD, ACCENT, 0.16), sel_t);
        let border = mix(mix(with_alpha(BORDER, 150), HOVER_LINE, hov_t), ACCENT_HI, sel_t);
        p.rect(r, CornerRadius::same(14), fill, Stroke::new(1.5_f32, border), StrokeKind::Inside);
        let fg = if enabled { TEXT } else { DIM };
        let glyph = mix(mix(MUTED, HOVER_GLYPH, hov_t), ACCENT_HI, sel_t);
        if stacked {
            let ic = Rect::from_center_size(pos2(r.left() + 34.0, r.center().y), vec2(26.0, 26.0));
            icons::paint(p, *icon, ic, glyph);
            p.text(pos2(r.left() + 64.0, r.top() + 12.0), Align2::LEFT_TOP, *title, FontId::new(15.0, semibold()), fg);
            let g = p.layout(line.to_string(), FontId::new(12.5, egui::FontFamily::Proportional), if enabled { MUTED } else { DIM }, r.width() - 80.0);
            p.galley(pos2(r.left() + 64.0, r.top() + 33.0), g, MUTED);
        } else {
            let ic = Rect::from_center_size(pos2(r.left() + 30.0, r.top() + 30.0), vec2(28.0, 28.0));
            icons::paint(p, *icon, ic, glyph);
            p.text(pos2(r.left() + 56.0, r.top() + 20.0 + (15.5 - title_size) * 0.5), Align2::LEFT_TOP, *title, FontId::new(title_size, semibold()), fg);
            let g = p.layout(line.to_string(), FontId::new(12.5, egui::FontFamily::Proportional), if enabled { MUTED } else { DIM }, r.width() - 36.0);
            p.galley(pos2(r.left() + 18.0, r.top() + 54.0), g, MUTED);
        }
        if sel_t > 0.02 {
            let b = pos2(r.right() - 16.0, r.top() + 16.0);
            p.circle_filled(b, 8.5 * sel_t, ACCENT);
            icons::paint(p, Icon::Check, Rect::from_center_size(b, vec2(10.0 * sel_t, 10.0 * sel_t)), Color32::WHITE);
        }
        if resp.has_focus() {
            focus_ring_r(p, rect, 14.0);
        }
        if enabled && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() && !chosen {
            cur = i;
            changed = true;
        }
    }
    *current = cur;
    changed
}
