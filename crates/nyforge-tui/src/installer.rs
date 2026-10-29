//! The installer screen, started by `install.sh`. It asks, then runs `install.sh --run` and shows each step as it finishes.
//! Exit code: 0 installed, 3 the person said no, 1 something failed.

use crate::ui::Pal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Ask,
    Parts,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Yes,
    No,
    Tui,
    Gui,
    Go,
    Close,
}

pub struct Opts {
    pub script: String,
    pub prefix: String,
    pub gui: bool,
}

struct Inst {
    stage: Stage,
    yes: bool,
    tui: bool,
    gui: bool,
    gui_avail: bool,
    focus: usize,
    script: String,
    prefix: String,
    steps: Vec<String>,
    notes: Vec<String>,
    err: String,
    rx: Option<Receiver<String>>,
    hits: Vec<(Rect, Hit)>,
    tick: usize,
}

pub fn parse(args: &[String]) -> Opts {
    let mut o = Opts { script: "install.sh".into(), prefix: "/usr/local".into(), gui: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--script" => o.script = it.next().cloned().unwrap_or_default(),
            "--prefix" => o.prefix = it.next().cloned().unwrap_or_default(),
            "--gui" => o.gui = it.next().is_some_and(|v| v == "1"),
            _ => {}
        }
    }
    o
}

pub fn run(term: &mut crate::Term, o: Opts) -> std::io::Result<i32> {
    let pal = Pal::detect();
    let mut s = Inst { stage: Stage::Ask, yes: true, tui: true, gui: o.gui, gui_avail: o.gui, focus: 0, script: o.script, prefix: o.prefix, steps: vec![], notes: vec![], err: String::new(), rx: None, hits: vec![], tick: 0 };
    loop {
        term.draw(|f| draw(f, &mut s, &pal))?;
        let wait = if s.stage == Stage::Running { Duration::from_millis(90) } else { Duration::from_millis(500) };
        if event::poll(wait)? {
            let ev = event::read()?;
            if let Some(code) = handle(&mut s, ev) {
                return Ok(code);
            }
        }
        if s.stage == Stage::Running {
            s.tick += 1;
            pump(&mut s);
        }
    }
}

fn start(s: &mut Inst) {
    let (tx, rx) = channel();
    let mut cmd = Command::new("sh");
    cmd.arg(&s.script).args(["--run", "--prefix", &s.prefix, "--tui", if s.tui { "1" } else { "0" }, "--gui", if s.gui { "1" } else { "0" }]);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    match cmd.spawn() {
        Ok(mut c) => {
            let out = c.stdout.take().expect("piped");
            let err = c.stderr.take().expect("piped");
            let t2 = tx.clone();
            std::thread::spawn(move || {
                for l in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                    let _ = t2.send(format!("err {l}"));
                }
            });
            std::thread::spawn(move || {
                for l in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    let _ = tx.send(l);
                }
                let code = c.wait().ok().and_then(|s| s.code()).unwrap_or(1);
                let _ = tx.send(format!("exit {code}"));
            });
            s.rx = Some(rx);
            s.stage = Stage::Running;
        }
        Err(e) => {
            s.err = format!("Could not start the installer: {e}");
            s.stage = Stage::Failed;
        }
    }
}

fn pump(s: &mut Inst) {
    let Some(rx) = s.rx.as_ref() else { return };
    let mut next = None;
    while let Ok(l) = rx.try_recv() {
        if let Some(t) = l.strip_prefix("step ") {
            s.steps.push(t.to_string());
        } else if let Some(t) = l.strip_prefix("note ") {
            s.notes.push(t.to_string());
        } else if let Some(t) = l.strip_prefix("err ") {
            s.err = t.to_string();
        } else if let Some(c) = l.strip_prefix("exit ") {
            next = Some(if c.trim() == "0" { Stage::Done } else { Stage::Failed });
        }
    }
    if let Some(n) = next {
        s.stage = n;
    }
}

fn rows(s: &Inst) -> usize {
    if s.gui_avail { 3 } else { 2 }
}

/// Some(exit code) when the installer should close.
fn handle(s: &mut Inst, ev: Event) -> Option<i32> {
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => {
            if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                return Some(if s.stage == Stage::Done { 0 } else { 3 });
            }
            match s.stage {
                Stage::Ask => match k.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Char('h' | 'l' | 'j' | 'k') | KeyCode::Tab => s.yes = !s.yes,
                    KeyCode::Char('y') => s.yes = true,
                    KeyCode::Char('n') => s.yes = false,
                    KeyCode::Enter | KeyCode::Char(' ') => return choose(s, s.yes),
                    KeyCode::Esc | KeyCode::Char('q') => return Some(3),
                    _ => {}
                },
                Stage::Parts => match k.code {
                    KeyCode::Up | KeyCode::Char('k') => s.focus = s.focus.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => s.focus = (s.focus + 1).min(rows(s) - 1),
                    KeyCode::Char(' ') => toggle(s, s.focus),
                    KeyCode::Enter => start(s),
                    KeyCode::Esc | KeyCode::Char('q') => return Some(3),
                    _ => {}
                },
                Stage::Running => {}
                Stage::Done => return Some(0),
                Stage::Failed => return Some(1),
            }
        }
        Event::Mouse(m) if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) => {
            let hit = s.hits.iter().find(|(r, _)| m.column >= r.x && m.column < r.right() && m.row == r.y).map(|(_, h)| *h);
            match (s.stage, hit) {
                (Stage::Ask, Some(Hit::Yes)) => return choose(s, true),
                (Stage::Ask, Some(Hit::No)) => return choose(s, false),
                (Stage::Parts, Some(Hit::Tui)) => s.tui = !s.tui,
                (Stage::Parts, Some(Hit::Gui)) => s.gui = !s.gui,
                (Stage::Parts, Some(Hit::Go)) => start(s),
                (Stage::Done, Some(Hit::Close)) => return Some(0),
                (Stage::Failed, Some(Hit::Close)) => return Some(1),
                _ => {}
            }
        }
        _ => {}
    }
    None
}

fn choose(s: &mut Inst, yes: bool) -> Option<i32> {
    s.yes = yes;
    if !yes {
        return Some(3);
    }
    s.stage = Stage::Parts;
    None
}

fn toggle(s: &mut Inst, row: usize) {
    match row {
        0 => s.tui = !s.tui,
        1 if s.gui_avail => s.gui = !s.gui,
        _ => {}
    }
}

fn draw(f: &mut ratatui::Frame, s: &mut Inst, pal: &Pal) {
    let a = f.area();
    let area = Rect { x: a.x + 3, y: a.y + 1, width: a.width.saturating_sub(6).min(72), height: a.height.saturating_sub(2) };
    let dim = Style::new().fg(pal.dim);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let accent = Style::new().fg(pal.accent).add_modifier(Modifier::BOLD);
    let ok = Style::new().fg(pal.ok).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = vec![Line::from(vec![Span::styled("NyForge", accent), Span::styled(format!("  {}", env!("CARGO_PKG_VERSION")), dim)]), Line::from("")];
    let mut hits: Vec<(usize, u16, u16, Hit)> = vec![];
    let bar = || Span::styled("│  ", dim);

    // 1. install?
    let asking = s.stage == Stage::Ask;
    if asking {
        lines.push(Line::from(vec![Span::styled("◆  ", accent), Span::styled("Install NyForge?", bold)]));
        let y = if s.yes { ("●", ok) } else { ("○", dim) };
        let n = if !s.yes { ("●", Style::new().fg(pal.accent)) } else { ("○", dim) };
        hits.push((lines.len(), 3, 7, Hit::Yes));
        hits.push((lines.len(), 13, 6, Hit::No));
        lines.push(Line::from(vec![bar(), Span::styled(y.0, y.1), Span::styled(" Yes", if s.yes { bold } else { dim }), Span::raw("     "), Span::styled(n.0, n.1), Span::styled(" No", if !s.yes { bold } else { dim })]));
    } else {
        lines.push(Line::from(vec![Span::styled("✔  ", ok), Span::raw("Install NyForge  "), Span::styled("Yes", dim)]));
    }
    lines.push(Line::from(Span::styled("│", dim)));

    // 2. what to install
    match s.stage {
        Stage::Ask => lines.push(Line::from(vec![Span::styled("◇  ", dim), Span::styled("Choose what to install", dim)])),
        Stage::Parts => {
            lines.push(Line::from(vec![Span::styled("◆  ", accent), Span::styled("Choose what to install", bold)]));
            let item = |on: bool, focus: bool, name: &str, note: &str| -> Line<'static> {
                let (g, gs) = if on { ("✔", ok) } else { ("○", dim) };
                Line::from(vec![Span::styled("│  ", dim), Span::styled(g, gs), Span::styled(format!(" {name}"), if focus { bold } else { Style::new() }), Span::styled(format!("  {note}"), dim)])
            };
            lines.push(Line::from(vec![Span::styled("│  ", dim), Span::styled("✔", Style::new().fg(pal.ok)), Span::styled(" Command line  nyforge", dim)]));
            hits.push((lines.len(), 3, 40, Hit::Tui));
            lines.push(item(s.tui, s.focus == 0, "Terminal app", "nyforge -t"));
            if s.gui_avail {
                hits.push((lines.len(), 3, 40, Hit::Gui));
                lines.push(item(s.gui, s.focus == 1, "Desktop app", "menu entry and icon"));
            }
            lines.push(Line::from(Span::styled("│", dim)));
            hits.push((lines.len(), 3, 11, Hit::Go));
            lines.push(Line::from(vec![Span::styled("│  ", dim), Span::styled(" Install ", Style::new().fg(ratatui::style::Color::Black).bg(pal.accent).add_modifier(Modifier::BOLD)), Span::styled("  enter", dim)]));
        }
        _ => {
            let mut what = vec!["command line"];
            if s.tui {
                what.push("terminal app");
            }
            if s.gui {
                what.push("desktop app");
            }
            lines.push(Line::from(vec![Span::styled("✔  ", ok), Span::raw("Chosen  "), Span::styled(what.join(", "), dim)]));
        }
    }
    lines.push(Line::from(Span::styled("│", dim)));

    // 3. install
    match s.stage {
        Stage::Ask | Stage::Parts => lines.push(Line::from(vec![Span::styled("◇  ", dim), Span::styled(format!("Install to {}", s.prefix), dim)])),
        Stage::Running => {
            const SPIN: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
            lines.push(Line::from(vec![Span::styled(format!("{}  ", SPIN[s.tick % 8]), accent), Span::styled(format!("Installing to {}", s.prefix), bold)]));
            for t in &s.steps {
                lines.push(Line::from(vec![bar(), Span::styled("✔", ok), Span::raw(format!(" {t}"))]));
            }
        }
        Stage::Done | Stage::Failed => {
            let good = s.stage == Stage::Done;
            lines.push(Line::from(vec![Span::styled(if good { "✔  " } else { "✘  " }, if good { ok } else { Style::new().fg(pal.err).add_modifier(Modifier::BOLD) }), Span::styled(if good { format!("Installed to {}", s.prefix) } else { "The install stopped".to_string() }, bold)]));
            for t in &s.steps {
                lines.push(Line::from(vec![bar(), Span::styled("✔", ok), Span::raw(format!(" {t}"))]));
            }
            if !good {
                lines.push(Line::from(vec![bar(), Span::styled(if s.err.is_empty() { "Nothing more was changed.".to_string() } else { s.err.clone() }, Style::new().fg(pal.err))]));
            }
            lines.push(Line::from(Span::styled("│", dim)));
            if good {
                let cmd = |c: &str, what: &str| Line::from(vec![bar(), Span::styled(format!("{c:<19}"), accent), Span::styled(what.to_string(), dim)]);
                if s.tui {
                    lines.push(cmd("nyforge -t", "the terminal app"));
                }
                if s.gui {
                    lines.push(cmd("nyforge-gui", "the desktop app"));
                }
                lines.push(cmd("nyforge list", "the command line"));
                lines.push(cmd("nyforge-uninstall", "remove NyForge (with sudo)"));
                for n in &s.notes {
                    lines.push(Line::from(vec![bar(), Span::styled(n.clone(), Style::new().fg(pal.warn))]));
                }
                lines.push(Line::from(Span::styled("│", dim)));
            }
            hits.push((lines.len(), 3, 9, Hit::Close));
            lines.push(Line::from(vec![Span::styled("└  ", dim), Span::styled(" Close ", Style::new().fg(ratatui::style::Color::Black).bg(pal.accent).add_modifier(Modifier::BOLD))]));
        }
    }
    s.hits = hits.into_iter().map(|(i, x, w, h)| (Rect { x: area.x + x, y: area.y + i as u16, width: w, height: 1 }, h)).collect();
    f.render_widget(Paragraph::new(lines), area);
}
