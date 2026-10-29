//! Drawing. Everything is plain text in the terminal's own background, so it looks right in any theme.

use crate::app::{status, App, Kind, Mode, Page, TOOLS};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

#[derive(Clone, Copy)]
pub struct Pal {
    pub accent: Color,
    pub ok: Color,
    pub err: Color,
    pub warn: Color,
    pub dim: Color,
}

impl Pal {
    pub fn detect() -> Pal {
        let truecolor = std::env::var("COLORTERM").map(|v| v.contains("truecolor") || v.contains("24bit")).unwrap_or(false) || cfg!(windows);
        if truecolor {
            Pal { accent: Color::Rgb(61, 147, 245), ok: Color::Rgb(88, 196, 130), err: Color::Rgb(237, 100, 100), warn: Color::Rgb(232, 178, 80), dim: Color::Rgb(128, 134, 148) }
        } else {
            Pal { accent: Color::Indexed(75), ok: Color::Indexed(78), err: Color::Indexed(203), warn: Color::Indexed(179), dim: Color::Indexed(244) }
        }
    }
}

pub const SIDEBAR_W: u16 = 17;
pub const TOOLS_Y: u16 = 3;

pub fn draw(f: &mut Frame, app: &App, pal: &Pal) {
    let [side, main] = Layout::horizontal([Constraint::Length(SIDEBAR_W), Constraint::Min(20)]).areas(f.area());
    sidebar(f, app, pal, side);
    let main = Rect { x: main.x + 2, width: main.width.saturating_sub(3), y: main.y + 1, height: main.height.saturating_sub(1) };
    let page = app.page();
    if app.about {
        return about(f, app, pal, main);
    }
    match &page.mode {
        Mode::Form => form(f, page, app, pal, main),
        Mode::Confirm(_) => confirm(f, app, pal, main),
        Mode::Running => running(f, page, pal, main),
        Mode::Done(d) => done(f, page, d, pal, main),
    }
}

fn sidebar(f: &mut Frame, app: &App, pal: &Pal, area: Rect) {
    let on_menu = |i: usize| i == app.cur && !app.about;
    let mut lines = vec![Line::from(""), Line::from(vec![Span::raw("  "), Span::styled("NyForge", Style::new().fg(pal.accent).add_modifier(Modifier::BOLD))]), Line::from("")];
    for (i, (_, name)) in TOOLS.iter().enumerate() {
        let sel = on_menu(i);
        let bar = if sel { Span::styled("▌ ", Style::new().fg(if app.side { pal.accent } else { pal.dim })) } else { Span::raw("  ") };
        let style = if sel { Style::new().add_modifier(Modifier::BOLD) } else { Style::new().fg(pal.dim) };
        let mut spans = vec![bar, Span::styled(*name, style)];
        if matches!(app.pages[i].mode, Mode::Running) {
            spans.push(Span::styled(" ●", Style::new().fg(pal.accent)));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), Rect { width: area.width.saturating_sub(1), ..area });
    let v = Rect { x: area.x, y: area.bottom().saturating_sub(2), width: area.width.saturating_sub(1), height: 1 };
    let vs = if app.about { Style::new().fg(pal.accent).add_modifier(Modifier::BOLD) } else { Style::new().fg(pal.dim) };
    let bar = if app.about { Span::styled("▌ ", Style::new().fg(if app.side { pal.accent } else { pal.dim })) } else { Span::raw("  ") };
    f.render_widget(Paragraph::new(Line::from(vec![bar, Span::styled(concat!("v", env!("CARGO_PKG_VERSION")), vs)])), v);
    // one solid line down the right edge of the menu
    f.render_widget(Block::new().borders(Borders::RIGHT).border_style(Style::new().fg(pal.dim)), area);
}

fn title<'a>(page: &Page, pal: &Pal) -> Vec<Line<'a>> {
    let name = TOOLS.iter().find(|(t, _)| *t == page.tool).map(|(_, n)| *n).unwrap_or("");
    vec![Line::from(Span::styled(name, Style::new().add_modifier(Modifier::BOLD).fg(pal.accent))), Line::from("")]
}

fn form(f: &mut Frame, page: &Page, app: &App, pal: &Pal, area: Rect) {
    let mut lines = title(page, pal);
    let vis = page.visible();
    for (n, &i) in vis.iter().enumerate() {
        let fld = &page.fields[i];
        let focused = n == page.focus && !app.side;
        let mark = if focused { Span::styled("› ", Style::new().fg(pal.accent)) } else { Span::raw("  ") };
        let label = Span::styled(format!("{:<14}", fld.label), if focused { Style::new().add_modifier(Modifier::BOLD) } else { Style::new().fg(pal.dim) });
        match &fld.kind {
            Kind::Text(s) => {
                let mut v = vec![mark, label];
                if page.editing && focused {
                    v.push(Span::raw(s.clone()));
                    v.push(Span::styled("▏", Style::new().fg(pal.accent)));
                } else if s.is_empty() {
                    v.push(Span::styled(fld.hint, Style::new().fg(pal.dim).add_modifier(Modifier::ITALIC)));
                } else {
                    v.push(Span::raw(s.clone()));
                }
                lines.push(Line::from(v));
            }
            Kind::Pick(o, sel) => {
                let mut v = vec![mark, label];
                if focused {
                    v.push(Span::styled("‹ ", Style::new().fg(pal.dim)));
                    v.push(Span::styled(o[*sel], Style::new().fg(pal.accent).add_modifier(Modifier::BOLD)));
                    v.push(Span::styled(" ›", Style::new().fg(pal.dim)));
                } else {
                    v.push(Span::raw(o[*sel]));
                }
                lines.push(Line::from(v));
            }
            Kind::Toggle(b) => {
                let (sym, txt, c) = if *b { ("●", "on", pal.ok) } else { ("○", "off", pal.dim) };
                let mut v = vec![mark, label, Span::styled(format!("{sym} {txt}"), Style::new().fg(c))];
                if focused && !fld.hint.is_empty() {
                    v.push(Span::styled(format!("   {}", fld.hint), Style::new().fg(pal.dim)));
                }
                lines.push(Line::from(v));
            }
            Kind::Drive => {
                lines.push(Line::from(vec![mark, label]));
                if let Some(e) = &page.drive_err {
                    lines.push(Line::from(Span::styled(format!("                {e}"), Style::new().fg(pal.err))));
                } else if page.drives.is_empty() {
                    lines.push(Line::from(Span::styled("                No drive found. Plug one in and press r.", Style::new().fg(pal.dim))));
                }
                for (j, c) in page.drives.iter().enumerate() {
                    let sel = page.drive_sel == Some(j);
                    let (sym, st) = match (&c.blocked, sel) {
                        (Some(_), _) => ("–", Style::new().fg(pal.dim)),
                        (None, true) => ("●", Style::new().fg(pal.accent).add_modifier(Modifier::BOLD)),
                        (None, false) => ("○", Style::new()),
                    };
                    let mut v = vec![Span::raw("                "), Span::styled(format!("{sym} {}  ", c.name), st), Span::styled(format!("{}  {}", c.size_text(), c.drive.path), Style::new().fg(pal.dim))];
                    if let Some(b) = &c.blocked {
                        v.push(Span::styled(format!("  {b}"), Style::new().fg(pal.warn)));
                    }
                    lines.push(Line::from(v));
                    if sel {
                        if let Some(w) = &c.warn {
                            for l in wrap(w, (area.width as usize).saturating_sub(20)) {
                                lines.push(Line::from(Span::styled(format!("                  {l}"), Style::new().fg(pal.warn))));
                            }
                        }
                    }
                }
            }
        }
    }
    lines.push(Line::from(""));
    let on_start = page.focus + 1 == vis.len() + 1 && !app.side;
    let btn = if on_start { Style::new().fg(Color::Black).bg(pal.accent).add_modifier(Modifier::BOLD) } else { Style::new().fg(pal.accent).add_modifier(Modifier::BOLD) };
    lines.push(Line::from(vec![Span::raw("  "), Span::styled(format!(" {} ", page.start_label()), btn)]));
    if let Some(m) = &page.msg {
        lines.push(Line::from(""));
        let c = if m.ends_with("refreshed.") { pal.dim } else { pal.err };
        lines.push(Line::from(vec![Span::raw("  "), Span::styled(m.clone(), Style::new().fg(c))]));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn confirm(f: &mut Frame, app: &App, pal: &Pal, area: Rect) {
    let mut lines = title(app.page(), pal);
    if let Some((rows, destructive, verb)) = app.confirm_rows() {
        if destructive {
            lines.push(Line::from(Span::styled("  Everything on this drive will be erased.", Style::new().fg(pal.err).add_modifier(Modifier::BOLD))));
            lines.push(Line::from(""));
        }
        for (k, v) in rows {
            lines.push(Line::from(vec![Span::styled(format!("  {k:<10}"), Style::new().fg(pal.dim)), Span::raw(v.clone())]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::raw("  "), Span::styled(" y ", Style::new().fg(Color::Black).bg(pal.accent).add_modifier(Modifier::BOLD)), Span::raw(format!(" {verb}    ")), Span::styled(" n ", Style::new().fg(pal.dim).add_modifier(Modifier::REVERSED)), Span::raw(" cancel")]));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn bar(width: usize, frac: f64, pal: &Pal) -> Line<'static> {
    let on = ((width as f64) * frac).round() as usize;
    Line::from(vec![Span::raw("  "), Span::styled("━".repeat(on.min(width)), Style::new().fg(pal.accent)), Span::styled("━".repeat(width - on.min(width)), Style::new().fg(pal.dim).add_modifier(Modifier::DIM))])
}

fn running(f: &mut Frame, page: &Page, pal: &Pal, area: Rect) {
    let mut lines = title(page, pal);
    let s = status(page);
    lines.push(Line::from(vec![Span::raw("  "), Span::styled(s.heading, Style::new().add_modifier(Modifier::BOLD))]));
    lines.push(Line::from(""));
    let w = (area.width as usize).saturating_sub(4).min(72);
    match s.fraction {
        Some(fr) => lines.push(bar(w, fr, pal)),
        None => {
            // no real progress yet: a slow sweep instead of a made-up number
            let t = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) / 120) as usize;
            let seg = 8.min(w);
            let span = w - seg;
            let pos = if span == 0 { 0 } else { let m = t % (2 * span); if m <= span { m } else { 2 * span - m } };
            lines.push(Line::from(vec![Span::raw("  "), Span::styled("━".repeat(pos), Style::new().fg(pal.dim).add_modifier(Modifier::DIM)), Span::styled("━".repeat(seg), Style::new().fg(pal.accent)), Span::styled("━".repeat(w - pos - seg), Style::new().fg(pal.dim).add_modifier(Modifier::DIM))]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::raw("  "), Span::styled(s.stats, Style::new().fg(pal.dim))]));
    if !s.detail.is_empty() {
        lines.push(Line::from(vec![Span::raw("  "), Span::styled(s.detail, Style::new().fg(pal.dim))]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::raw("  "), Span::styled("c", Style::new().fg(pal.accent).add_modifier(Modifier::BOLD)), Span::styled(" cancel", Style::new().fg(pal.dim))]));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn done(f: &mut Frame, page: &Page, d: &crate::app::Done, pal: &Pal, area: Rect) {
    let mut lines = title(page, pal);
    let (sym, c) = match d.ok {
        Some(true) => ("✔", pal.ok),
        Some(false) => ("✘", pal.err),
        None => ("–", pal.warn),
    };
    lines.push(Line::from(vec![Span::raw("  "), Span::styled(format!("{sym} {}", d.title), Style::new().fg(c).add_modifier(Modifier::BOLD))]));
    lines.push(Line::from(""));
    for l in &d.lines {
        for w in wrap(l, area.width as usize - 4) {
            lines.push(Line::from(vec![Span::raw("  "), Span::raw(w)]));
        }
    }
    f.render_widget(Paragraph::new(lines).scroll((page.scroll, 0)), area);
}

fn about(f: &mut Frame, app: &App, pal: &Pal, area: Rect) {
    let mut lines = vec![
        Line::from(vec![Span::styled("NyForge ", Style::new().add_modifier(Modifier::BOLD).fg(pal.accent)), Span::styled(env!("CARGO_PKG_VERSION"), Style::new().add_modifier(Modifier::BOLD))]),
        Line::from(vec![Span::styled(if app.engine_ok { "● engine ready" } else { "● engine not found (install nyforge next to nyforge-tui)" }, Style::new().fg(if app.engine_ok { pal.ok } else { pal.err }))]),
        Line::from(""),
    ];
    for r in &app.changelog {
        lines.push(Line::from(vec![Span::styled(r.version.clone(), Style::new().add_modifier(Modifier::BOLD).fg(pal.accent)), Span::styled(format!("  {}", r.title), Style::new().add_modifier(Modifier::BOLD))]));
        for it in &r.items {
            for (n, l) in wrap(it, area.width as usize - 4).into_iter().enumerate() {
                lines.push(Line::from(vec![Span::styled(if n == 0 { "  · " } else { "    " }, Style::new().fg(pal.dim)), Span::raw(l)]));
            }
        }
        lines.push(Line::from(""));
    }
    f.render_widget(Paragraph::new(lines).scroll((app.about_scroll, 0)), area);
}

/// Word-wraps `text` to `width` columns.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let (mut out, mut cur) = (vec![], String::new());
    for w in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > width {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}
