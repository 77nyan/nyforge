//! Terminal rendering: colours, the progress bar and tables.

use nyforge_core::error::NyError;
use nyforge_core::format::{human_duration, human_size, human_speed};
use nyforge_core::progress::{Event, Phase, Progress};
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

pub struct Ui {
    color: bool,
    tty: bool,
    line_open: bool,
    last_plain: Option<Instant>,
}

impl Ui {
    pub fn new() -> Self {
        let tty = std::io::stderr().is_terminal();
        let color = tty && std::env::var_os("NO_COLOR").is_none();
        Ui { color, tty, line_open: false, last_plain: None }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }

    pub fn print_error(e: &NyError) {
        let ui = Ui::new();
        eprintln!("{} {}", ui.red("Error:"), e.message);
        if let Some(h) = &e.hint {
            eprintln!("       {h}");
        }
    }

    pub fn table(&self, rows: &[Vec<String>]) {
        let cols = rows[0].len();
        let widths: Vec<usize> =
            (0..cols).map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0)).collect();
        for (i, r) in rows.iter().enumerate() {
            let mut line = String::new();
            for (c, cell) in r.iter().enumerate() {
                let pad = widths[c].saturating_sub(cell.chars().count());
                line.push_str(cell);
                if c + 1 < cols {
                    line.push_str(&" ".repeat(pad + 2));
                }
            }
            if i == 0 {
                println!("{}", self.dim(&line));
            } else {
                println!("{line}");
            }
        }
    }

    /// End the in-place progress line, if one is showing.
    pub fn finish_line(&mut self) {
        if self.line_open {
            eprintln!();
            self.line_open = false;
        }
    }

    fn bar(fraction: f64, width: usize) -> String {
        let filled = ((fraction.clamp(0.0, 1.0)) * width as f64).round() as usize;
        format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
    }

    fn progress_text(p: &Progress, bar: bool) -> String {
        let pct = (p.fraction * 100.0).floor() as u32;
        let size = match p.bytes_total {
            Some(t) => format!("{} / {}", human_size(p.bytes_done), human_size(t)),
            None => human_size(p.bytes_done),
        };
        let eta = p.eta_secs.map(|e| format!("  ETA {}", human_duration(e))).unwrap_or_default();
        let drives = if p.targets.len() > 1 { format!("  {} drives", p.targets.len()) } else { String::new() };
        if bar {
            format!("{} {:>3}%  {}  {}{}{}", Self::bar(p.fraction, 28), pct, size, human_speed(p.speed_bps), eta, drives)
        } else {
            format!("{pct}%  {size}  {}{eta}{drives}", human_speed(p.speed_bps))
        }
    }

    pub fn render(&mut self, ev: &Event) {
        match ev {
            Event::Start { image, container, total_bytes, targets } => {
                let size = total_bytes.map(|t| format!(", {}", human_size(t))).unwrap_or_default();
                eprintln!("{} {} ({}{}) → {}", self.bold("Writing"), image, container, size, targets.join(", "));
            }
            Event::Phase { phase, detail } => {
                if matches!(phase, Phase::Preparing | Phase::Finalizing) {
                    self.finish_line();
                    eprintln!("{}", self.dim(detail));
                }
            }
            Event::Log { message } => {
                self.finish_line();
                eprintln!("{}", self.dim(message));
            }
            Event::Progress(p) => {
                if self.tty {
                    let mut err = std::io::stderr();
                    let _ = write!(err, "\r\x1b[2K{}", Self::progress_text(p, true));
                    let _ = err.flush();
                    self.line_open = true;
                } else if self.last_plain.map_or(true, |t| t.elapsed() >= Duration::from_secs(5)) {
                    self.last_plain = Some(Instant::now());
                    eprintln!("{}", Self::progress_text(p, false));
                }
            }
            Event::TargetFinished { target, ok, error, .. } => {
                if *ok {
                    return; // the summary line covers success
                }
                self.finish_line();
                if let Some(e) = error {
                    eprintln!("{} {}: {}", self.red("✘"), target, e.message);
                }
            }
            Event::Finished { .. } | Event::Error { .. } => {}
        }
    }
}
