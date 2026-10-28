//! Look and feel: palette, fonts, spacing, and the small animation helpers everything shares.

use eframe::egui::{self, vec2, Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Id, Stroke};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// A mid-tone slate with white text and one friendly blue, in the spirit of balenaEtcher: light enough to feel
// calm and approachable, dark enough for a USB tool. Green is only ever used for "it worked".
pub const BG: Color32 = Color32::from_rgb(0x3f, 0x43, 0x4d);
pub const BAR: Color32 = BG;
pub const PANEL: Color32 = Color32::from_rgb(0x36, 0x3a, 0x43);
pub const PANEL_HI: Color32 = Color32::from_rgb(0x4b, 0x50, 0x5c);
pub const FIELD: Color32 = Color32::from_rgb(0x2e, 0x31, 0x39);
pub const BORDER: Color32 = Color32::from_rgb(0x50, 0x55, 0x62);
pub const BORDER_HI: Color32 = Color32::from_rgb(0x6d, 0x73, 0x82);
pub const TEXT: Color32 = Color32::from_rgb(0xf6, 0xf8, 0xfb);
pub const MUTED: Color32 = Color32::from_rgb(0xcf, 0xd4, 0xdd);
pub const DIM: Color32 = Color32::from_rgb(0xa2, 0xa9, 0xb6);
pub const ACCENT: Color32 = Color32::from_rgb(0x1f, 0x97, 0xea);
pub const ACCENT_HI: Color32 = Color32::from_rgb(0x56, 0xb8, 0xf8);
pub const ACCENT_DEEP: Color32 = Color32::from_rgb(0x14, 0x79, 0xc4);
/// Cards and rows hover in a lighter grey (ring, icon, border); buttons and selections are blue.
pub const HOVER_LINE: Color32 = Color32::from_rgb(0x8c, 0x93, 0xa2);
pub const HOVER_GLYPH: Color32 = Color32::from_rgb(0xc4, 0xc9, 0xd3);
pub const ON_ACCENT: Color32 = Color32::WHITE;
pub const OK: Color32 = Color32::from_rgb(0x5f, 0xb8, 0x35);
pub const OK_HI: Color32 = Color32::from_rgb(0x82, 0xd0, 0x57);
pub const DANGER: Color32 = Color32::from_rgb(0xe8, 0x5d, 0x54);
pub const DANGER_DEEP: Color32 = Color32::from_rgb(0xc4, 0x44, 0x3c);
pub const WARN: Color32 = Color32::from_rgb(0xfc, 0xa3, 0x21);

pub const RADIUS: u8 = 8;

static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);

pub fn set_reduce_motion(v: bool) {
    REDUCE_MOTION.store(v, Ordering::Relaxed);
}

pub fn reduce_motion() -> bool {
    REDUCE_MOTION.load(Ordering::Relaxed)
}

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn install(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("inter".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Regular.ttf"))));
    fonts.font_data.insert("inter-semibold".into(), Arc::new(FontData::from_static(include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"))));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "inter".into());
    fonts.families.insert(semibold(), vec!["inter-semibold".into(), "inter".into()]);
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = FIELD;
    v.faint_bg_color = PANEL;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.30);
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.window_stroke = Stroke::new(1.0_f32, BORDER_HI);
    v.window_corner_radius = CornerRadius::same(14);
    v.window_shadow = egui::Shadow { offset: [0, 12], blur: 34, spread: 0, color: Color32::from_black_alpha(120) };
    v.popup_shadow = egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(130) };
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(RADIUS);
    }
    v.widgets.inactive.bg_fill = PANEL_HI;
    v.widgets.inactive.weak_bg_fill = PANEL_HI;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.hovered.bg_fill = PANEL_HI;
    v.widgets.hovered.weak_bg_fill = PANEL_HI;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, BORDER_HI);
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    v.text_cursor.stroke = Stroke::new(1.5_f32, ACCENT);
    style.spacing.item_spacing = vec2(8.0, 8.0);
    style.spacing.button_padding = vec2(16.0, 8.0);
    style.spacing.interact_size = vec2(36.0, 30.0);
    style.spacing.scroll = egui::style::ScrollStyle::floating();
    style.animation_time = 0.18;
    style.text_styles.insert(egui::TextStyle::Body, FontId::new(14.5, FontFamily::Proportional));
    style.text_styles.insert(egui::TextStyle::Button, FontId::new(14.5, semibold()));
    style.text_styles.insert(egui::TextStyle::Small, FontId::new(12.0, FontFamily::Proportional));
    style.text_styles.insert(egui::TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace));
    style.text_styles.insert(egui::TextStyle::Heading, FontId::new(20.0, semibold()));
    ctx.set_style(style);
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

pub fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// A gentler ease-out (quartic): fast start, long soft landing. For entrances.
pub fn ease_out_soft(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(4)
}

/// Smootherstep: slow start and slow end. For things that go back and forth, like hover.
pub fn ease_smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Eases out with a small overshoot, for things that "pop" into place.
pub fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    let c = 1.70158;
    1.0 + (c + 1.0) * t * t * t + c * t * t
}

static PANELS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Called once per frame so that panels are numbered 0, 1, 2… in the order they are drawn.
pub fn start_frame(screen: u32) {
    PANELS.store(0, Ordering::Relaxed);
    SALT.store(screen, Ordering::Relaxed);
}

static SALT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn next_panel_index() -> u32 {
    PANELS.fetch_add(1, Ordering::Relaxed)
}

/// The time at which `key` started to be shown without a break. An element that was not drawn in the previous
/// pass (another screen, a closed dialog) starts again; one that stays on screen keeps its start time.
pub fn shown_since(ctx: &Context, key: Id) -> f64 {
    let now = ctx.input(|i| i.time);
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let e = d.get_temp_mut_or_insert_with(key, || (now, pass));
        if pass > e.1 + 1 {
            *e = (now, pass);
        } else {
            e.1 = pass;
        }
        e.0
    })
}

/// Entrance progress (0 → 1, eased) of the element called `name`, starting `delay` seconds after it appeared.
/// With reduced motion it is always 1.
pub fn reveal(ctx: &Context, name: &str, delay: f32) -> f32 {
    if reduce_motion() {
        return 1.0;
    }
    let start = shown_since(ctx, Id::new(("reveal", name, SALT.load(Ordering::Relaxed))));
    let t = ((ctx.input(|i| i.time) - start) as f32 - delay) / 0.38;
    if t < 1.0 {
        ctx.request_repaint();
    }
    ease_out_soft(t)
}

/// Draws `add` fading in and rising a few pixels. The element keeps the same total height all the way, so nothing below it jumps.
pub fn staged<R>(ui: &mut egui::Ui, name: &str, delay: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    staged_by(ui, name, delay, 12.0, add)
}

pub fn staged_by<R>(ui: &mut egui::Ui, name: &str, delay: f32, rise: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let t = reveal(ui.ctx(), name, delay);
    ui.scope(|ui| {
        ui.set_opacity(t);
        ui.add_space((1.0 - t) * rise);
        let r = add(ui);
        ui.add_space(t * rise);
        r
    })
    .inner
}

/// 0.0 → 1.0 over `secs` while `on` is true (and back again when it is not). Instant with reduced motion.
pub fn anim(ctx: &Context, id: Id, on: bool, secs: f32) -> f32 {
    if reduce_motion() {
        return if on { 1.0 } else { 0.0 };
    }
    ctx.animate_bool_with_time_and_easing(id, on, secs, ease_smooth)
}

/// Hover feedback: 170 ms, smoothly in and out.
pub fn hover_t(ctx: &Context, id: Id, on: bool) -> f32 {
    anim(ctx, id, on, 0.17)
}

pub fn semibold_text(text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text).font(FontId::new(size, semibold()))
}

pub fn muted(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).color(MUTED)
}

pub fn small_muted(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(12.0).color(MUTED)
}

pub fn dim(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(12.0).color(DIM)
}
