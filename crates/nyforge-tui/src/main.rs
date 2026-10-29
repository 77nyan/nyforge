//! NyForge in the terminal. A left menu, one tool on the right. The work is done by the same `nyforge` engine the window uses.

mod app;
mod drives;
mod engine;
mod installer;
mod ui;

use app::App;
use ratatui::crossterm::event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use std::io::stdout;
use std::time::Duration;

pub type Term = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

fn enter() -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)
}

fn leave() {
    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// Asks for the administrator password in the normal terminal, once, before the first job that needs it.
fn authorise(term: &mut Term) -> Result<(), String> {
    #[cfg(unix)]
    {
        let ok = std::process::Command::new("sudo").args(["-n", "true"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if ok {
            return Ok(());
        }
        leave();
        println!("NyForge needs administrator access to work with drives.");
        let r = std::process::Command::new("sudo").arg("-v").status();
        let _ = enter();
        let _ = term.clear();
        return match r {
            Ok(s) if s.success() => Ok(()),
            Ok(_) => Err("The password was not accepted.".into()),
            Err(_) => Err("`sudo` is not installed. Start nyforge -t as root instead.".into()),
        };
    }
    #[allow(unreachable_code)]
    {
        let _ = term;
        Ok(())
    }
}

fn run(term: &mut Term) -> std::io::Result<()> {
    let pal = ui::Pal::detect();
    let mut app = App::new();
    let mut dirty = true;
    loop {
        if dirty {
            term.draw(|f| ui::draw(f, &app, &pal))?;
            dirty = false;
        }
        if app.launch.is_some() {
            let auth = if app.wants_sudo() { authorise(term) } else { Ok(()) };
            app.launch_now(auth);
            dirty = true;
            continue;
        }
        let wait = if app.busy() { Duration::from_millis(100) } else { Duration::from_millis(500) };
        if event::poll(wait)? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => app.key(k),
                Event::Paste(s) => app.paste(&s),
                Event::Mouse(e) if matches!(e.kind, MouseEventKind::Down(MouseButton::Left)) => app.click(e.column, e.row, term.size()?.height),
                _ => {}
            }
            dirty = true;
        }
        if app.tick() {
            dirty = true;
        }
        if app.quit {
            return Ok(());
        }
    }
}

fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).is_some_and(|a| a == "--installer") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        enter()?;
        let mut term = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout()))?;
        let r = installer::run(&mut term, installer::parse(&args));
        leave();
        std::process::exit(r.unwrap_or(1));
    }
    if std::env::args().nth(1).is_some_and(|a| matches!(a.as_str(), "-h" | "--help")) {
        println!("nyforge-tui {}\nNyForge in the terminal. Also started by: nyforge -t", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if std::env::args().nth(1).is_some_and(|a| matches!(a.as_str(), "-V" | "--version")) {
        println!("nyforge-tui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        leave();
        hook(i);
    }));
    enter()?;
    let mut term = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout()))?;
    let r = run(&mut term);
    leave();
    r
}
