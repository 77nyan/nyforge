//! The installer screen, started by `install.sh`. It asks what to do (install, update, uninstall, quit), then runs
//! `install.sh --run` and shows each step as it finishes.
//! Exit code: 0 done, 3 nothing was changed (quit or "no"), 1 something failed.

use crate::ui::Pal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::io::BufRead;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Menu,
    Parts,
    Confirm,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Install,
    Update,
    Uninstall,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Opt(usize),
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
    pub src: String,
    pub assets: String,
}

struct Inst {
    stage: Stage,
    action: Action,
    /// The version found in the install folder, if NyForge is installed there.
    installed: Option<String>,
    sel: usize,
    yes: bool,
    tui: bool,
    gui: bool,
    gui_avail: bool,
    focus: usize,
    script: String,
    prefix: String,
    src: String,
    assets: String,
    steps: Vec<String>,
    notes: Vec<String>,
    err: String,
    rx: Option<Receiver<String>>,
    hits: Vec<(Rect, Hit)>,
    tick: usize,
}

pub fn parse(args: &[String]) -> Opts {
    let mut o = Opts { script: "install.sh".into(), prefix: "/usr/local".into(), gui: false, src: String::new(), assets: String::new() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--script" => o.script = it.next().cloned().unwrap_or_default(),
            "--prefix" => o.prefix = it.next().cloned().unwrap_or_default(),
            "--src" => o.src = it.next().cloned().unwrap_or_default(),
            "--assets" => o.assets = it.next().cloned().unwrap_or_default(),
            "--gui" => o.gui = it.next().is_some_and(|v| v == "1"),
            _ => {}
        }
    }
    o
}

/// The version of the NyForge installed under `prefix`, if any.
fn installed_version(prefix: &str) -> Option<String> {
    let out = Command::new(Path::new(prefix).join("bin/nyforge")).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1).map(|v| v.to_string())
}

pub fn run(term: &mut crate::Term, o: Opts) -> std::io::Result<i32> {
    let pal = Pal::detect();
    let installed = installed_version(&o.prefix);
    let mut s = Inst {
        stage: Stage::Menu,
        action: Action::Install,
        sel: if installed.is_some() { 1 } else { 0 },
        installed,
        yes: true,
        tui: true,
        gui: o.gui,
        gui_avail: o.gui,
        focus: 0,
        script: o.script,
        prefix: o.prefix,
        src: o.src,
        assets: o.assets,
        steps: vec![],
        notes: vec![],
        err: String::new(),
        rx: None,
        hits: vec![],
        tick: 0,
    };
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
    cmd.arg(&s.script).args(["--run", "--prefix", &s.prefix]);
    match s.action {
        Action::Install => {
            cmd.args(["--tui", if s.tui { "1" } else { "0" }, "--gui", if s.gui { "1" } else { "0" }]);
            if !s.src.is_empty() {
                cmd.args(["--src", &s.src, "--assets", &s.assets]);
            }
        }
        Action::Update => {
            cmd.arg("--update");
        }
        Action::Uninstall => {
            cmd.arg("--uninstall");
        }
    }
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
    if s.gui_avail {
        2
    } else {
        1
    }
}

fn available(s: &Inst, i: usize) -> bool {
    i != 1 && i != 2 || s.installed.is_some()
}

/// An option in the first question. Some(exit code) when the installer should close.
fn pick(s: &mut Inst, i: usize) -> Option<i32> {
    if !available(s, i) {
        return None;
    }
    s.sel = i;
    match i {
        0 => {
            s.action = Action::Install;
            s.stage = Stage::Parts;
        }
        1 => {
            s.action = Action::Update;
            start(s);
        }
        2 => {
            s.action = Action::Uninstall;
            s.yes = false;
            s.stage = Stage::Confirm;
        }
        _ => return Some(3),
    }
    None
}

fn handle(s: &mut Inst, ev: Event) -> Option<i32> {
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => {
            if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                return Some(if s.stage == Stage::Done { 0 } else { 3 });
            }
            match s.stage {
                Stage::Menu => match k.code {
                    KeyCode::Up | KeyCode::Char('k') => s.sel = prev_available(s),
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => s.sel = next_available(s),
                    KeyCode::Char(c @ '1'..='4') => return pick(s, c as usize - '1' as usize),
                    KeyCode::Enter | KeyCode::Char(' ') => return pick(s, s.sel),
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
                Stage::Confirm => match k.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Char('h' | 'l' | 'j' | 'k') | KeyCode::Tab => s.yes = !s.yes,
                    KeyCode::Char('y') => s.yes = true,
                    KeyCode::Char('n') => s.yes = false,
                    KeyCode::Enter | KeyCode::Char(' ') => return confirm(s, s.yes),
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
                (Stage::Menu, Some(Hit::Opt(i))) => return pick(s, i),
                (Stage::Confirm, Some(Hit::Yes)) => return confirm(s, true),
                (Stage::Confirm, Some(Hit::No)) => return confirm(s, false),
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

fn next_available(s: &Inst) -> usize {
    (1..=4).map(|d| (s.sel + d) % 4).find(|&i| available(s, i)).unwrap_or(s.sel)
}

fn prev_available(s: &Inst) -> usize {
    (1..=4).map(|d| (s.sel + 4 - d % 4) % 4).find(|&i| available(s, i)).unwrap_or(s.sel)
}

fn confirm(s: &mut Inst, yes: bool) -> Option<i32> {
    if !yes {
        return Some(3);
    }
    start(s);
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
    let mut head = vec![Span::styled("NyForge", accent), Span::styled(format!("  {}", env!("CARGO_PKG_VERSION")), dim)];
    if let Some(v) = &s.installed {
        if s.stage == Stage::Menu {
            head.push(Span::styled(format!("   installed {v}"), dim));
        }
    }
    let mut lines: Vec<Line> = vec![Line::from(head), Line::from("")];
    let mut hits: Vec<(usize, u16, u16, Hit)> = vec![];
    let bar = || Span::styled("│  ", dim);
    let rail = || Line::from(Span::styled("│", dim));
    let button = |t: &str| Span::styled(format!(" {t} "), Style::new().fg(Color::Black).bg(pal.accent).add_modifier(Modifier::BOLD));

    // 1. what to do
    if s.stage == Stage::Menu {
        lines.push(Line::from(vec![Span::styled("◆  ", accent), Span::styled("What do you want to do?", bold)]));
        let names = ["Install", "Update", "Uninstall", "Quit"];
        for (i, n) in names.iter().enumerate() {
            let on = available(s, i);
            let (dot, ds) = if i == s.sel && on { ("●", ok) } else { ("○", dim) };
            let ns = if !on { dim } else if i == s.sel { bold } else { Style::new() };
            let mut v = vec![bar(), Span::styled(dot, ds), Span::styled(format!(" {}  ", i + 1), dim), Span::styled(format!("{n:<10}"), ns)];
            if !on {
                v.push(Span::styled("not installed", dim));
            }
            hits.push((lines.len(), 3, 28, Hit::Opt(i)));
            lines.push(Line::from(v));
        }
    } else {
        let what = match s.action {
            Action::Install => "Install NyForge",
            Action::Update => "Update NyForge",
            Action::Uninstall => "Uninstall NyForge",
        };
        lines.push(Line::from(vec![Span::styled("✔  ", ok), Span::raw(what)]));
    }
    if s.stage == Stage::Menu {
        lines.push(Line::from(Span::styled("└", dim)));
        s.hits = hits.into_iter().map(|(i, x, w, h)| (Rect { x: area.x + x, y: area.y + i as u16, width: w, height: 1 }, h)).collect();
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    lines.push(rail());

    match s.action {
        Action::Install => {
            // 2. what to install
            match s.stage {
                Stage::Menu => lines.push(Line::from(vec![Span::styled("◇  ", dim), Span::styled("Choose what to install", dim)])),
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
                    lines.push(rail());
                    hits.push((lines.len(), 3, 11, Hit::Go));
                    lines.push(Line::from(vec![Span::styled("│  ", dim), button("Install"), Span::styled("  enter", dim)]));
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
            lines.push(rail());
        }
        Action::Uninstall => {
            if s.stage == Stage::Confirm {
                lines.push(Line::from(vec![Span::styled("◆  ", accent), Span::styled(format!("Remove NyForge from {}?", s.prefix), bold)]));
                let y = if s.yes { ("●", Style::new().fg(pal.err)) } else { ("○", dim) };
                let n = if !s.yes { ("●", ok) } else { ("○", dim) };
                hits.push((lines.len(), 3, 7, Hit::Yes));
                hits.push((lines.len(), 13, 6, Hit::No));
                lines.push(Line::from(vec![bar(), Span::styled(y.0, y.1), Span::styled(" Yes", if s.yes { bold } else { dim }), Span::raw("     "), Span::styled(n.0, n.1), Span::styled(" No", if !s.yes { bold } else { dim })]));
                lines.push(Line::from(vec![bar(), Span::styled("Your own files and drives are never touched.", dim)]));
                lines.push(rail());
            } else {
                lines.push(Line::from(vec![Span::styled("✔  ", ok), Span::raw("Remove NyForge  "), Span::styled("Yes", dim)]));
                lines.push(rail());
            }
        }
        Action::Update => {}
    }

    // the last step: the work itself
    let title = |running: bool| match (s.action, running) {
        (Action::Install, true) => format!("Installing to {}", s.prefix),
        (Action::Install, false) => format!("Installed to {}", s.prefix),
        (Action::Update, true) => "Updating NyForge".to_string(),
        (Action::Update, false) => "Update finished".to_string(),
        (Action::Uninstall, true) => "Removing NyForge".to_string(),
        (Action::Uninstall, false) => "NyForge was removed".to_string(),
    };
    match s.stage {
        Stage::Menu => {}
        Stage::Parts | Stage::Confirm => {
            if s.action == Action::Install {
                lines.push(Line::from(vec![Span::styled("◇  ", dim), Span::styled(format!("Install to {}", s.prefix), dim)]));
            }
        }
        Stage::Running => {
            const SPIN: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
            lines.push(Line::from(vec![Span::styled(format!("{}  ", SPIN[s.tick % 8]), accent), Span::styled(title(true), bold)]));
            for t in &s.steps {
                lines.push(Line::from(vec![bar(), Span::styled("✔", ok), Span::raw(format!(" {t}"))]));
            }
        }
        Stage::Done | Stage::Failed => {
            let good = s.stage == Stage::Done;
            lines.push(Line::from(vec![Span::styled(if good { "✔  " } else { "✘  " }, if good { ok } else { Style::new().fg(pal.err).add_modifier(Modifier::BOLD) }), Span::styled(if good { title(false) } else { "It stopped".to_string() }, bold)]));
            for t in &s.steps {
                lines.push(Line::from(vec![bar(), Span::styled("✔", ok), Span::raw(format!(" {t}"))]));
            }
            if !good {
                lines.push(Line::from(vec![bar(), Span::styled(if s.err.is_empty() { "Nothing more was changed.".to_string() } else { s.err.clone() }, Style::new().fg(pal.err))]));
            }
            lines.push(rail());
            if good && s.action != Action::Uninstall {
                let cmd = |c: &str, what: &str| Line::from(vec![bar(), Span::styled(format!("{c:<19}"), accent), Span::styled(what.to_string(), dim)]);
                lines.push(cmd("nyforge -t", "the terminal app"));
                if s.gui_avail {
                    lines.push(cmd("nyforge-gui", "the desktop app"));
                }
                lines.push(cmd("nyforge list", "the command line"));
                lines.push(cmd("nyforge-uninstall", "remove NyForge (with sudo)"));
                for n in &s.notes {
                    lines.push(Line::from(vec![bar(), Span::styled(n.clone(), Style::new().fg(pal.warn))]));
                }
                lines.push(rail());
            }
            hits.push((lines.len(), 3, 9, Hit::Close));
            lines.push(Line::from(vec![Span::styled("└  ", dim), button("Close")]));
        }
    }
    s.hits = hits.into_iter().map(|(i, x, w, h)| (Rect { x: area.x + x, y: area.y + i as u16, width: w, height: 1 }, h)).collect();
    f.render_widget(Paragraph::new(lines), area);
}
