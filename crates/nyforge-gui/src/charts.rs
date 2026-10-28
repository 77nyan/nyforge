//! The pictures NyTest draws: speed bars (the CrystalDiskMark table), the sustained-write graph and the surface map.
//! Everything is painted directly, so it follows the theme and needs no extra libraries.

use crate::theme::*;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui};
use nyforge_core::format::human_size;
use nyforge_core::nytest::engine::Op;
use nyforge_core::nytest::speed::SpeedResult;
use nyforge_core::nytest::sustained::Cliff;

/// "1 483 MB/s", "86.4 MB/s", "0.52 MB/s": three significant figures, never a long tail of decimals.
pub fn mbps_text(v: f64) -> String {
    if v >= 100.0 {
        format!("{:.0}", v)
    } else if v >= 10.0 {
        format!("{:.1}", v)
    } else {
        format!("{:.2}", v)
    }
}

pub fn iops_text(v: f64) -> String {
    if v >= 10_000.0 {
        format!("{:.1}k", v / 1000.0)
    } else {
        format!("{:.0}", v)
    }
}

pub fn micros_text(us: u64) -> String {
    if us >= 10_000 {
        format!("{:.0} ms", us as f64 / 1000.0)
    } else if us >= 1000 {
        format!("{:.1} ms", us as f64 / 1000.0)
    } else {
        format!("{us} µs")
    }
}

fn profile_line(name: &str) -> &'static str {
    match name {
        "SEQ1M Q8T1" => "1 MiB blocks · 8 at a time",
        "SEQ1M Q1T1" => "1 MiB blocks · one at a time",
        "SEQ128K Q32T1" => "128 KiB blocks · 32 at a time",
        "RND4K Q32T1" => "4 KiB random · 32 at a time",
        "RND4K Q1T1" => "4 KiB random · one at a time",
        _ => "",
    }
}

/// The bar scale: logarithmic, so a drive that does 400 MB/s in a row and 0.5 MB/s at random still shows both.
fn bar_fraction(v: f64, max: f64) -> f32 {
    if v <= 0.0 || max <= 0.0 {
        return 0.0;
    }
    ((1.0 + v).ln() / (1.0 + max).ln()).clamp(0.0, 1.0) as f32
}

/// The speed table. `pending` names the measurement running now, which is drawn as an animated empty bar.
pub fn speed_table(ui: &mut Ui, id: Id, results: &[SpeedResult], pending: Option<(&str, Op)>) {
    let mut profiles: Vec<&str> = vec![];
    for r in results {
        if !profiles.contains(&r.profile.as_str()) {
            profiles.push(r.profile.as_str());
        }
    }
    if let Some((p, _)) = pending {
        if !profiles.contains(&p) {
            profiles.push(p);
        }
    }
    if profiles.is_empty() {
        return;
    }
    let max = results.iter().map(|r| r.mbps).fold(1.0, f64::max);
    let w = ui.available_width();
    let label_w = (w * 0.27).clamp(120.0, 200.0);
    let col_w = ((w - label_w - 12.0) / 2.0).max(80.0);
    // column headers
    let (head, _) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
    let p = ui.painter();
    p.text(pos2(head.left() + label_w + 12.0, head.center().y), Align2::LEFT_CENTER, "Read", FontId::new(12.0, semibold()), MUTED);
    p.text(pos2(head.left() + label_w + 12.0 + col_w, head.center().y), Align2::LEFT_CENTER, "Write", FontId::new(12.0, semibold()), MUTED);
    for (i, prof) in profiles.iter().enumerate() {
        let (row, resp) = ui.allocate_exact_size(vec2(w, 54.0), Sense::hover());
        let hov = hover_t(ui.ctx(), id.with(("row", i)), resp.hovered());
        let p = ui.painter();
        p.rect_filled(row, CornerRadius::same(10), with_alpha(PANEL_HI, (hov * 70.0) as u8));
        p.text(pos2(row.left() + 8.0, row.top() + 12.0), Align2::LEFT_TOP, *prof, FontId::new(13.5, semibold()), TEXT);
        // wraps (rather than runs into the bars) when the window is narrow
        let line = p.layout(profile_line(prof).to_string(), FontId::new(11.0, egui::FontFamily::Proportional), DIM, label_w - 8.0);
        p.galley(pos2(row.left() + 8.0, row.top() + 30.0), line, DIM);
        for (k, op) in [Op::Read, Op::Write].into_iter().enumerate() {
            let x = row.left() + label_w + 12.0 + col_w * k as f32;
            let cell = Rect::from_min_size(pos2(x, row.top() + 6.0), vec2(col_w - 12.0, 24.0));
            let r = results.iter().find(|r| r.profile == *prof && r.op == op);
            p.rect_filled(cell, CornerRadius::same(7), FIELD);
            match r {
                Some(r) => {
                    let frac = bar_fraction(r.mbps, max);
                    let shown = ui.ctx().animate_value_with_time(id.with(("bar", i, k)), frac, if reduce_motion() { 0.0 } else { 0.5 });
                    let bw = (cell.width() * shown).max(4.0);
                    let col = mix(FIELD, ACCENT_DEEP, 0.75);
                    p.rect_filled(Rect::from_min_size(cell.min, vec2(bw, cell.height())), CornerRadius::same(7), col);
                    let txt = format!("{} MB/s", mbps_text(r.mbps));
                    p.text(pos2(cell.left() + 9.0, cell.center().y), Align2::LEFT_CENTER, txt, FontId::new(13.0, semibold()), TEXT);
                    p.text(pos2(cell.left() + 9.0, cell.bottom() + 10.0), Align2::LEFT_CENTER, format!("{} IOPS · p99 {}", iops_text(r.iops), micros_text(r.lat.p99_us)), FontId::new(11.0, egui::FontFamily::Proportional), MUTED);
                }
                None => {
                    if pending.map(|(pp, po)| pp == *prof && po == op).unwrap_or(false) {
                        let t = ui.input(|i| i.time) as f32;
                        let seg = cell.width() * 0.25;
                        let ph = if reduce_motion() { 0.3 } else { (t * 0.8) % 1.0 };
                        let x0 = cell.left() + (cell.width() + seg) * ph - seg;
                        let bar = Rect::from_min_max(pos2(x0.max(cell.left()), cell.top()), pos2((x0 + seg).min(cell.right()), cell.bottom()));
                        if bar.width() > 0.0 {
                            p.rect_filled(bar, CornerRadius::same(7), with_alpha(MUTED, 38));
                        }
                        ui.ctx().request_repaint();
                    } else {
                        p.text(cell.center(), Align2::CENTER_CENTER, "–", FontId::new(13.0, egui::FontFamily::Proportional), DIM);
                    }
                }
            }
        }
        let rd = results.iter().find(|r| r.profile == *prof && r.op == Op::Read);
        let wr = results.iter().find(|r| r.profile == *prof && r.op == Op::Write);
        if rd.is_some() || wr.is_some() {
            resp.on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(320.0);
                ui.label(semibold_text(*prof, 13.5));
                for r in [rd, wr].into_iter().flatten() {
                    ui.add_space(4.0);
                    ui.label(RichTextExt::strong(if r.op == Op::Read { "Read" } else { "Write" }));
                    let runs = r.runs.iter().map(|v| mbps_text(*v)).collect::<Vec<_>>().join(", ");
                    ui.label(small_muted(format!("{} MB/s (median of {} run{}: {} MB/s)", mbps_text(r.mbps), r.runs.len(), if r.runs.len() == 1 { "" } else { "s" }, runs)));
                    ui.label(small_muted(format!("{} operations per second", iops_text(r.iops))));
                    ui.label(small_muted(format!("Latency: typical {} · 9 in 10 under {} · 99 in 100 under {} · 999 in 1000 under {} · worst {}", micros_text(r.lat.p50_us), micros_text(r.lat.p90_us), micros_text(r.lat.p99_us), micros_text(r.lat.p999_us), micros_text(r.lat.max_us))));
                    if r.runs.len() > 1 && r.spread_pct() > 15.0 {
                        ui.label(small_muted(format!("The runs differ by {:.0}%: this drive is not consistent.", r.spread_pct())));
                    }
                }
            });
        }
        ui.add_space(8.0);
    }
}

// Tiny helper so the tooltip header can be bold without a new import.
struct RichTextExt;
impl RichTextExt {
    fn strong(t: &str) -> egui::RichText {
        semibold_text(t, 12.5)
    }
}

/// A "nice" step for an axis that spans `span` with about `n` ticks.
fn nice_step(span: f64, n: f64) -> f64 {
    let raw = (span / n).max(1e-9);
    let mag = 10f64.powf(raw.log10().floor());
    let norm = raw / mag;
    let nice = if norm < 1.5 { 1.0 } else if norm < 3.5 { 2.0 } else if norm < 7.5 { 5.0 } else { 10.0 };
    nice * mag
}

/// Write speed over time, with the point where a fast cache ran out marked. `samples` are (seconds, MB/s).
pub fn speed_graph(ui: &mut Ui, id: Id, samples: &[(f64, f64)], cliff: Option<&Cliff>, height: f32) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(10), FIELD, Stroke::NONE, StrokeKind::Inside);
    if samples.len() < 2 {
        p.text(rect.center(), Align2::CENTER_CENTER, "Waiting for the first measurements…", FontId::new(12.5, egui::FontFamily::Proportional), DIM);
        return;
    }
    let plot = Rect::from_min_max(pos2(rect.left() + 46.0, rect.top() + 12.0), pos2(rect.right() - 14.0, rect.bottom() - 24.0));
    let t_max = samples.last().map(|s| s.0).unwrap_or(1.0).max(1.0);
    let v_top = samples.iter().map(|s| s.1).fold(1.0, f64::max) * 1.12;
    let to = |t: f64, v: f64| pos2(plot.left() + (t / t_max) as f32 * plot.width(), plot.bottom() - (v / v_top) as f32 * plot.height());
    // grid and labels
    let step = nice_step(v_top, 4.0);
    let mut v = 0.0;
    while v <= v_top {
        let y = to(0.0, v).y;
        p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0_f32, with_alpha(BORDER, 110)));
        p.text(pos2(plot.left() - 6.0, y), Align2::RIGHT_CENTER, format!("{}", v as i64), FontId::new(10.5, egui::FontFamily::Proportional), DIM);
        v += step;
    }
    p.text(pos2(rect.left() + 8.0, rect.top() + 3.0), Align2::LEFT_TOP, "MB/s", FontId::new(10.0, egui::FontFamily::Proportional), DIM);
    let tstep = nice_step(t_max, 6.0);
    let mut t = 0.0;
    while t <= t_max + 1e-6 {
        let x = to(t, 0.0).x;
        p.text(pos2(x, plot.bottom() + 6.0), Align2::CENTER_TOP, if t_max >= 120.0 { format!("{:.0} min", t / 60.0) } else { format!("{:.0} s", t) }, FontId::new(10.5, egui::FontFamily::Proportional), DIM);
        t += tstep;
    }
    // the area and the line
    let pts: Vec<Pos2> = samples.iter().map(|s| to(s.0, s.1.max(0.0))).collect();
    p.add(Shape::line(pts.clone(), Stroke::new(1.8_f32, ACCENT)));
    // where the fast part ended
    if let Some(c) = cliff {
        let x = to(c.at_secs, 0.0).x;
        for s in Shape::dashed_line(&[pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.5_f32, WARN), 5.0, 4.0) {
            p.add(s);
        }
        let label = format!("{} written, then {} MB/s", human_size(c.at_bytes), mbps_text(c.after_mbps));
        let g = p.layout_no_wrap(label, FontId::new(11.5, semibold()), WARN);
        let lx = (x + 8.0).min(plot.right() - g.size().x);
        let ly = plot.top() + 4.0;
        p.rect_filled(Rect::from_min_size(pos2(lx - 5.0, ly - 2.0), g.size() + vec2(10.0, 4.0)), CornerRadius::same(6), PANEL);
        p.galley(pos2(lx, ly), g, WARN);
    }
    // a read-out under the pointer
    if let Some(h) = resp.hover_pos() {
        if plot.contains(h) {
            let t = ((h.x - plot.left()) / plot.width()) as f64 * t_max;
            let i = samples.partition_point(|s| s.0 < t).min(samples.len() - 1);
            let s = samples[i];
            let q = to(s.0, s.1);
            let p = ui.painter();
            p.line_segment([pos2(q.x, plot.top()), pos2(q.x, plot.bottom())], Stroke::new(1.0_f32, with_alpha(Color32::WHITE, 40)));
            p.circle_filled(q, 3.0, ACCENT);
            let g = p.layout_no_wrap(format!("{} MB/s at {:.0} s", mbps_text(s.1), s.0), FontId::new(11.5, semibold()), TEXT);
            let lx = (q.x + 10.0).min(rect.right() - g.size().x - 10.0);
            p.rect_filled(Rect::from_min_size(pos2(lx - 5.0, plot.top() + 22.0), g.size() + vec2(10.0, 6.0)), CornerRadius::same(6), PANEL);
            p.galley(pos2(lx, plot.top() + 25.0), g, TEXT);
        }
    }
    let _ = id;
}

/// The surface map: one square per slice of the drive, coloured by how it read. `total` is the scanned range in bytes and
/// `from` where it starts; `state` is 0 fine, 1 a little slow, 2 slow, 3 unreadable; 255 not read yet.
pub fn surface_map(ui: &mut Ui, id: Id, state: &[u8], mbps: &[f32], from: u64, total: u64, expected: usize) {
    let n = expected.max(state.len()).max(1);
    let w = ui.available_width();
    let cell = if n > 600 { 10.0 } else { 13.0 };
    let gap = 2.0;
    let per_row = (((w - 4.0 + gap) / (cell + gap)).floor() as usize).max(8);
    let rows = n.div_ceil(per_row);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, rows as f32 * (cell + gap) + 4.0), Sense::hover());
    let p = ui.painter();
    let fine = mix(FIELD, OK, 0.45);
    let hover_cell = resp.hover_pos().map(|h| {
        let cx = ((h.x - rect.left() - 2.0) / (cell + gap)).floor();
        let cy = ((h.y - rect.top() - 2.0) / (cell + gap)).floor();
        (cx, cy)
    });
    let mut hovered: Option<usize> = None;
    for i in 0..n {
        let (cx, cy) = ((i % per_row) as f32, (i / per_row) as f32);
        let r = Rect::from_min_size(pos2(rect.left() + 2.0 + cx * (cell + gap), rect.top() + 2.0 + cy * (cell + gap)), vec2(cell, cell));
        let st = state.get(i).copied().unwrap_or(255);
        let col = match st {
            0 => fine,
            1 => mix(FIELD, WARN, 0.55),
            2 => WARN,
            3 => DANGER,
            _ => mix(FIELD, PANEL_HI, 0.5),
        };
        p.rect_filled(r, CornerRadius::same(3), col);
        if let Some((hx, hy)) = hover_cell {
            if hx == cx && hy == cy {
                hovered = Some(i);
                p.rect_stroke(r.expand(1.0), CornerRadius::same(3), Stroke::new(1.5_f32, Color32::WHITE), StrokeKind::Outside);
            }
        }
    }
    if let Some(i) = hovered {
        let a = from + total * i as u64 / n as u64;
        let b = from + total * (i as u64 + 1) / n as u64;
        let st = state.get(i).copied().unwrap_or(255);
        let speed = mbps.get(i).copied().unwrap_or(0.0);
        resp.on_hover_ui_at_pointer(|ui| {
            ui.label(semibold_text(format!("{} – {}", human_size(a), human_size(b)), 12.5));
            ui.label(small_muted(match st {
                0 => format!("Read cleanly at {} MB/s", mbps_text(speed as f64)),
                1 => format!("A little slow: {} MB/s", mbps_text(speed as f64)),
                2 => format!("Slow: {} MB/s", mbps_text(speed as f64)),
                3 => "Could not be read".to_string(),
                _ => "Not read yet".to_string(),
            }));
        });
    }
    let _ = id;
}

/// The key under a surface map.
pub fn surface_key(ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        for (col, text) in [(mix(FIELD, OK, 0.45), "Reads fine"), (mix(FIELD, WARN, 0.55), "A little slow"), (WARN, "Slow"), (DANGER, "Unreadable"), (mix(FIELD, PANEL_HI, 0.5), "Not read")] {
            let (r, _) = ui.allocate_exact_size(vec2(12.0, 14.0), Sense::hover());
            ui.painter().rect_filled(Rect::from_center_size(r.center(), vec2(11.0, 11.0)), CornerRadius::same(3), col);
            ui.label(small_muted(text));
            ui.add_space(8.0);
        }
    });
}
