//! The "Check the download" window: compares an image file with the SHA-256, SHA-1 or MD5 its publisher gives.
//!
//! It reads the file in a background thread and never touches a drive, so it needs no administrator rights. When a
//! checksum file sits next to the image (`SHA256SUMS`, `x.iso.sha256`, …) it is found and used straight away.

use crate::screens::pick_files;
use crate::theme::*;
use crate::widgets::{self, banner, dialog, dialog_buttons, heading, Btn, Tone};
use eframe::egui::{self, Id, RichText};
use nyforge_core::checksum::{self, Algo, CheckResult};
use nyforge_core::error::NyError;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

enum Outcome {
    Match(CheckResult),
    Mismatch(CheckResult),
    Hash(Algo, String),
}

struct Running {
    rx: Receiver<Result<Outcome, NyError>>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

enum State {
    Idle,
    Running(Running),
    Done(Outcome),
    Failed(String),
}

pub struct ChecksumDialog {
    open: bool,
    file: PathBuf,
    name: String,
    input: String,
    /// The checksum file that was found beside the image, if any.
    beside: Option<String>,
    state: State,
    copied: bool,
}

impl ChecksumDialog {
    pub fn new() -> Self {
        ChecksumDialog { open: false, file: PathBuf::new(), name: String::new(), input: String::new(), beside: None, state: State::Idle, copied: false }
    }

    /// Open the window for this file. A checksum file next to it is used right away.
    pub fn open(&mut self, file: PathBuf, ctx: &egui::Context) {
        self.cancel();
        self.name = crate::screens::file_name(&file);
        self.input.clear();
        self.beside = None;
        self.state = State::Idle;
        self.copied = false;
        if let Some((from, e)) = checksum::find_beside(&file) {
            self.input = e.hex;
            self.beside = Some(crate::screens::file_name(&from));
            self.file = file;
            self.open = true;
            self.start(ctx);
            return;
        }
        self.file = file;
        self.open = true;
    }

    fn cancel(&mut self) {
        if let State::Running(r) = &self.state {
            r.cancel.store(true, Ordering::SeqCst);
        }
    }

    pub fn close(&mut self) {
        self.cancel();
        self.open = false;
        self.state = State::Idle;
    }

    fn start(&mut self, ctx: &egui::Context) {
        let text = self.input.trim().to_string();
        let expected = if text.is_empty() {
            None
        } else {
            match checksum::parse_expected(&text, Some(&self.name)) {
                Some(e) => Some(e),
                None => {
                    self.state = State::Failed("That is not a SHA-256, SHA-1 or MD5 checksum. A SHA-256 is 64 characters of 0-9 and a-f.".into());
                    return;
                }
            }
        };
        let (tx, rx) = channel();
        let (cancel, done, total) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (c2, d2, t2, path, ctx2) = (cancel.clone(), done.clone(), total.clone(), self.file.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut progress = |d: u64, t: u64| {
                d2.store(d, Ordering::Relaxed);
                t2.store(t, Ordering::Relaxed);
                ctx2.request_repaint();
            };
            let r = match expected {
                Some(e) => checksum::check_file(&path, &e, &c2, &mut progress).map(|r| if r.matches { Outcome::Match(r) } else { Outcome::Mismatch(r) }),
                None => checksum::hash_file(&path, Algo::Sha256, &c2, &mut progress).map(|h| Outcome::Hash(Algo::Sha256, h)),
            };
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        self.state = State::Running(Running { rx, cancel, done, total });
        self.copied = false;
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        // results from the background thread
        let mut finished = None;
        if let State::Running(r) = &self.state {
            if let Ok(res) = r.rx.try_recv() {
                finished = Some(res);
            }
        }
        if let Some(res) = finished {
            self.state = match res {
                Ok(o) => State::Done(o),
                Err(e) if e.kind == nyforge_core::error::ErrorKind::Cancelled => State::Idle,
                Err(e) => State::Failed(e.message),
            };
        }

        let running = matches!(self.state, State::Running(_));
        let mut go = false;
        let mut pick = false;
        let mut copy: Option<String> = None;
        let mut want_close = false;
        let parsed = if self.input.trim().is_empty() { None } else { checksum::parse_expected(&self.input, Some(&self.name)) };
        let (_, close) = dialog(ctx, "checksum-dialog", 480.0, |ui| {
            ui.label(heading("Check the download"));
            ui.label(small_muted(self.name.clone()));
            ui.add_space(10.0);
            ui.label(muted("Paste the checksum from the download page. Leave it empty to just show the file's SHA-256."));
            ui.add_space(6.0);
            ui.add_enabled_ui(!running, |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.input)
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(8, 7))
                        .font(egui::FontId::monospace(12.0))
                        .hint_text(RichText::new("SHA-256, SHA-1 or MD5").color(DIM)),
                );
                if r.changed() {
                    self.state = State::Idle;
                    self.beside = None;
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
            });
            ui.horizontal(|ui| {
                match (&parsed, self.input.trim().is_empty()) {
                    (Some(e), _) => {
                        ui.label(small_muted(format!("{} recognised", e.algo.name())));
                    }
                    (None, false) => {
                        ui.label(small_muted("Not a checksum yet"));
                    }
                    _ => {}
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::link_button(ui, "Use a checksum file…", !running).clicked() {
                        pick = true;
                    }
                });
            });
            if let Some(b) = &self.beside {
                ui.label(small_muted(format!("Found {b} next to the image.")));
            }
            ui.add_space(8.0);
            match &self.state {
                State::Idle => {}
                State::Running(r) => {
                    let (d, t) = (r.done.load(Ordering::Relaxed), r.total.load(Ordering::Relaxed));
                    let frac = if t > 0 { Some((d as f64 / t as f64).clamp(0.0, 1.0) as f32) } else { None };
                    widgets::progress_bar(ui, Id::new("checksum-bar"), frac);
                    ui.label(small_muted(match frac {
                        Some(f) => format!("Reading the file… {:.0}%", f * 100.0),
                        None => "Reading the file…".to_string(),
                    }));
                }
                State::Done(Outcome::Match(r)) => {
                    banner(ui, Tone::Success, "The checksum matches", &format!("This file is exactly what the publisher released ({}).", r.algo.name()), None);
                }
                State::Done(Outcome::Mismatch(r)) => {
                    banner(
                        ui,
                        Tone::Error,
                        "The checksum does not match",
                        &format!("The file's {} is {}, but you gave {}.", r.algo.name(), r.actual, r.expected),
                        Some("The download is damaged, or it is not the file that checksum belongs to. Download it again before writing it."),
                    );
                }
                State::Done(Outcome::Hash(a, h)) => {
                    banner(ui, Tone::Info, &format!("{} of this file", a.name()), h, None);
                    ui.add_space(4.0);
                    if Btn::new(if self.copied { "Copied" } else { "Copy" }).small().show(ui).clicked() {
                        copy = Some(h.clone());
                    }
                }
                State::Failed(m) => {
                    banner(ui, Tone::Error, "Could not check the file", m, None);
                }
            }
            dialog_buttons(ui, |ui| {
                let label = if self.input.trim().is_empty() { "Show SHA-256" } else { "Check" };
                if running {
                    if Btn::new("Stop").show(ui).clicked() {
                        want_close = true;
                    }
                } else {
                    if Btn::primary(label).show(ui).clicked() {
                        go = true;
                    }
                    if Btn::new("Close").show(ui).clicked() {
                        want_close = true;
                    }
                }
            });
        });
        if pick {
            if let Some(p) = pick_files("Select a checksum file", &[("Checksum files", &["sha256", "sha256sum", "sha1", "sha1sum", "md5", "md5sum", "txt"]), ("All files", &["*"])], false).into_iter().next() {
                match std::fs::metadata(&p) {
                    Ok(m) if m.len() <= 1 << 20 => match std::fs::read_to_string(&p).ok().and_then(|t| checksum::parse_expected(&t, Some(&self.name))) {
                        Some(e) => {
                            self.input = e.hex;
                            self.beside = Some(crate::screens::file_name(&p));
                            self.state = State::Idle;
                            go = true;
                        }
                        None => self.state = State::Failed(format!("{} has no checksum for {}.", crate::screens::file_name(&p), self.name)),
                    },
                    _ => self.state = State::Failed("That file is too big to be a checksum file.".into()),
                }
            }
        }
        if let Some(t) = copy {
            ctx.copy_text(t);
            self.copied = true;
        }
        if go && !running {
            self.start(ctx);
        }
        if want_close || (close && !running) || (close && running) {
            self.close();
        }
    }
}
