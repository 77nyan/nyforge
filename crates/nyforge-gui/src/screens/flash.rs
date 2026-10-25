//! NyFlash: write an image to a USB drive.

use super::{file_name, permission_notice, pick_files, run_panel, Screen, Shell};
use crate::drives::{self, Candidate, Purpose};
use crate::gate::{Gate, GateEvent};
use crate::icons::Icon;
use crate::run::{failure_from, Failure, Outcome, Run};
use crate::selector::DriveSelector;
use crate::theme::*;
use crate::widgets::{self, banner, dialog, dialog_buttons, heading, kv, status_mark, Btn, Tone};
use crate::worker;
use eframe::egui::{self, vec2, FontId, Id, RichText, Ui};
use nyforge_core::format::human_size;
use nyforge_core::image::{self, Container, ImageInfo};
use nyforge_core::nyboot::probe::{self, Verdict};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

pub const IMAGE_FILTERS: &[(&str, &[&str])] = &[
    ("Disk images", &["iso", "img", "raw", "bin", "dd", "gz", "xz", "bz2", "zst", "zip"]),
    ("All files", &["*"]),
];

struct ImageSel {
    path: PathBuf,
    name: String,
    file_size: u64,
    info: ImageInfo,
    windows_installer: bool,
}

impl ImageSel {
    /// Bytes this image will occupy on the drive, when that is known for certain.
    fn written_size(&self) -> Option<u64> {
        self.info.decompressed_size.or(if self.info.container == Container::Raw { Some(self.file_size) } else { None })
    }
}

enum Stage {
    Setup,
    Running,
    Done,
}

pub struct FlashScreen {
    stage: Stage,
    image: Option<ImageSel>,
    image_error: Option<Failure>,
    loading_image: Option<Receiver<Result<ImageSel, Failure>>>,
    pub sel: DriveSelector,
    step: Option<Gate<()>>,
    notice: Option<String>,
    run: Option<Run>,
    target: Option<Candidate>,
    ask_stop: bool,
    outcome: Option<Outcome>,
}

impl FlashScreen {
    pub fn new() -> Self {
        let mut s = FlashScreen {
            stage: Stage::Setup,
            image: None,
            image_error: None,
            loading_image: None,
            sel: DriveSelector::new(Purpose::Flash, "flash-drives"),
            step: None,
            notice: None,
            run: None,
            target: None,
            ask_stop: false,
            outcome: None,
        };
        // Testing aid: preselect an image without the native picker.
        if let Some(p) = std::env::var_os("NYFORGE_GUI_TEST_IMAGE") {
            s.load_image(PathBuf::from(p), &egui::Context::default());
        }
        s
    }

    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Running) || self.step.as_ref().map(|g| g.is_rechecking()).unwrap_or(false)
    }

    pub fn abort(&mut self) {
        if let Some(r) = &mut self.run {
            r.job.cancel();
        }
    }

    pub fn set_image(&mut self, path: PathBuf, ctx: &egui::Context) {
        if matches!(self.stage, Stage::Setup) && self.step.is_none() {
            self.load_image(path, ctx);
        }
    }

    fn load_image(&mut self, path: PathBuf, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let r = (|| -> Result<ImageSel, Failure> {
                let meta = std::fs::metadata(&path).map_err(|e| failure_from(&nyforge_core::NyError::from_io(&e, &format!("reading {}", path.display()))))?;
                if !meta.is_file() {
                    return Err(Failure { title: "Not a file".into(), message: format!("{} is not a file.", path.display()), hint: Some("Choose an .iso or .img file.".into()), kind: None });
                }
                let info = image::describe_file(&path).map_err(|e| failure_from(&e))?;
                let name = file_name(&path);
                let windows_installer = name.to_ascii_lowercase().ends_with(".iso") && matches!(probe::analyze_file(&path), Ok(r) if r.verdict == Verdict::WindowsInstaller);
                Ok(ImageSel { name, file_size: meta.len(), info, windows_installer, path })
            })();
            let _ = tx.send(r);
            c.request_repaint();
        });
        self.loading_image = Some(rx);
        self.image_error = None;
    }

    fn start_check(&mut self, ctx: &egui::Context) {
        let Some(id) = self.sel.selected().map(|c| c.id.clone()) else { return };
        self.step = Some(Gate::new(Purpose::Flash, &id, (), ctx));
        self.notice = None;
    }

    fn start_flash(&mut self, cand: Candidate, ctx: &egui::Context) {
        let Some(img) = &self.image else { return };
        let mut args = vec!["flash".to_string(), img.path.to_string_lossy().into_owned(), cand.drive.path.clone(), "-y".to_string()];
        args.extend(drives::engine_flags(&cand));
        self.run = Some(Run::new(worker::spawn(args, ctx.clone()), true));
        self.target = Some(cand);
        self.stage = Stage::Running;
        self.outcome = None;
        self.ask_stop = false;
        self.step = None;
    }

    fn reset_for_another(&mut self, ctx: &egui::Context) {
        self.stage = Stage::Setup;
        self.run = None;
        self.target = None;
        self.outcome = None;
        self.step = None;
        self.sel.deselect();
        self.sel.refresh(ctx);
    }

    pub fn ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        let ctx = ui.ctx().clone();
        // --- results from background work
        if let Some(rx) = &self.loading_image {
            if let Ok(r) = rx.try_recv() {
                self.loading_image = None;
                match r {
                    Ok(i) => {
                        self.image = Some(i);
                        self.image_error = None;
                    }
                    Err(f) => {
                        self.image_error = Some(f);
                    }
                }
            }
        }
        if let Some(g) = &mut self.step {
            match g.poll() {
                GateEvent::Waiting => {}
                GateEvent::Start(c) => self.start_flash(c, &ctx),
                GateEvent::Failed(e) => {
                    self.step = None;
                    self.notice = Some(e);
                    self.sel.refresh(&ctx);
                }
            }
        }
        if let Some(run) = &mut self.run {
            run.pump();
            if matches!(self.stage, Stage::Running) && run.is_done() {
                self.outcome = Some(run.outcome());
                self.stage = Stage::Done;
            }
        }
        if matches!(self.stage, Stage::Setup) {
            let idle = self.step.is_none();
            if self.sel.poll(&ctx, idle) {
                self.step = None;
            }
            if !self.sel.loaded && !self.sel.loading() {
                self.sel.refresh(&ctx);
            }
        }

        match self.stage {
            Stage::Setup => self.setup_ui(ui, sh),
            Stage::Running => self.running_ui(ui),
            Stage::Done => self.done_ui(ui, sh),
        }
        self.dialogs(&ctx);
    }

    // ------------------------------------------------------------------ setup

    fn setup_ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        permission_notice(ui, sh);
        if let Some(n) = self.notice.clone() {
            banner(ui, Tone::Error, "Drive check failed", &n, None);
            ui.add_space(8.0);
        }
        let too_small = match (&self.image, self.sel.selected()) {
            (Some(i), Some(c)) => i.written_size().filter(|&n| n > c.drive.size).map(|n| (n, c.drive.size)),
            _ => None,
        };
        let blocker: Option<String> = if let Some(w) = sh.why_not() {
            Some(w)
        } else if self.image.is_none() {
            Some("Select an image first.".into())
        } else if self.sel.selected().is_none() {
            Some("Select the drive to write to.".into())
        } else if let Some((need, have)) = too_small {
            Some(format!("The image needs {} but this drive holds {}.", human_size(need), human_size(have)))
        } else if self.step.is_some() {
            Some("Checking the drive…".into())
        } else {
            None
        };
        let img_ok = self.image.is_some();
        let drv_ok = self.sel.selected().is_some();
        let ready = blocker.is_none();
        let (mut pick_image, mut remove, mut pick_drive, mut flash) = (false, false, false, false);

        widgets::page_title(ui, "NyFlash", "Write a disk image to a USB drive.");
        ui.add_space(10.0);
        let col_w = (ui.available_width() / 3.0).min(250.0);
        widgets::step_columns(ui, 3, &[img_ok, img_ok && drv_ok], 250.0, |i, ui| match i {
            0 => {
                widgets::step_icon(ui, Icon::File, img_ok, if img_ok { 0.0 } else { 1.0 }, true);
                match &self.image {
                    None => {
                        let loading = self.loading_image.is_some();
                        let b = Btn::new(if loading { "Reading…" } else { "Select image" }).kind(widgets::Kind::Primary).big().enabled(!loading);
                        if b.show(ui).clicked() {
                            pick_image = true;
                        }
                        ui.label(small_muted("ISO, IMG or compressed"));
                    }
                    Some(img) => {
                        widgets::fit_label(ui, &img.name, |s| semibold_text(s, 15.0), FontId::new(15.0, semibold()), col_w - 24.0);
                        let mut sub = human_size(img.file_size);
                        if img.info.container != Container::Raw {
                            sub = format!("{sub} · {}", img.info.container.name());
                        }
                        ui.label(small_muted(sub));
                        ui.horizontal(|ui| {
                            let w = ui.available_width();
                            let need = 150.0;
                            ui.add_space(((w - need) / 2.0).max(0.0));
                            if widgets::link_button(ui, "Change", true).clicked() {
                                pick_image = true;
                            }
                            if widgets::link_button(ui, "Remove", true).clicked() {
                                remove = true;
                            }
                        });
                    }
                }
            }
            1 => {
                let enabled = true;
                widgets::step_icon(ui, Icon::Usb, drv_ok, if img_ok && !drv_ok { 1.0 } else { 0.0 }, enabled);
                match self.sel.selected() {
                    None => {
                        let kind = if img_ok { widgets::Kind::Primary } else { widgets::Kind::Secondary };
                        if Btn::new("Select drive").kind(kind).big().show(ui).clicked() {
                            pick_drive = true;
                        }
                        ui.label(small_muted("Only removable drives are shown"));
                    }
                    Some(c) => {
                        widgets::fit_label(ui, &c.name, |s| semibold_text(s, 15.0), FontId::new(15.0, semibold()), col_w - 24.0);
                        widgets::fit_label(ui, &format!("{} · {}", c.size_text(), c.drive.path), |s| small_muted(s), FontId::new(12.0, egui::FontFamily::Proportional), col_w - 16.0);
                        if widgets::link_button(ui, "Change", true).clicked() {
                            pick_drive = true;
                        }
                    }
                }
            }
            _ => {
                widgets::step_icon(ui, Icon::Flash, false, if ready { 1.0 } else { 0.0 }, ready);
                let label = if self.step.is_some() { "Checking…" } else { "Flash!" };
                let b = Btn::primary(label).big().enabled(ready).tip(blocker.as_deref().unwrap_or(""));
                if b.show(ui).clicked() {
                    flash = true;
                }
                let hint = match &blocker {
                    Some(b) if b.starts_with("Checking") => "Making sure it is still the same drive…".to_string(),
                    Some(b) => b.clone(),
                    None => "You will be asked to confirm.".to_string(),
                };
                ui.add(egui::Label::new(small_muted(hint)).wrap());
            }
        });
        ui.add_space(14.0);
        // problems with the image or the match, below the steps
        if let Some(img) = &self.image {
            if img.windows_installer {
                banner(
                    ui,
                    Tone::Warn,
                    "This looks like a Windows installer",
                    "Writing a Windows ISO straight to a drive usually does not give a bootable USB drive.",
                    Some("NyForge can build one from the command line: nyforge windows. It is not in the app yet."),
                );
                ui.add_space(8.0);
            }
        }
        if let Some(f) = &self.image_error {
            banner(ui, Tone::Error, &f.title, &f.message, f.hint.as_deref());
            ui.add_space(8.0);
        }
        if let Some((need, have)) = too_small {
            banner(ui, Tone::Error, "Image does not fit", &format!("The image needs {} but the selected drive holds {}.", human_size(need), human_size(have)), Some("Choose a larger drive."));
        }

        let ctx = ui.ctx().clone();
        if remove {
            self.image = None;
            self.image_error = None;
        }
        if pick_image {
            if let Some(p) = pick_files("Select an image", IMAGE_FILTERS, false).into_iter().next() {
                self.load_image(p, &ctx);
            }
        }
        if pick_drive {
            self.sel.open_picker(&ctx);
        }
        if flash {
            self.start_check(&ctx);
        }
    }

    // ------------------------------------------------------------------ running

    fn rows(&self) -> Vec<(&'static str, String)> {
        let img = self.image.as_ref().map(|i| i.name.clone()).unwrap_or_default();
        let t = self.target.as_ref().map(|c| format!("{} · {} · {}", c.name, c.size_text(), c.drive.path)).unwrap_or_default();
        vec![("Image", img), ("Drive", t)]
    }

    fn running_ui(&mut self, ui: &mut Ui) {
        widgets::run_header(ui, Icon::Flash, "Flashing");
        let rows = self.rows();
        if let Some(run) = &self.run {
            run_panel(ui, Id::new("flash-run"), run, run.job.cancel_requested, &rows);
        }
        ui.add_space(8.0);
        let stopping = self.run.as_ref().map(|r| r.job.cancel_requested).unwrap_or(false);
        ui.vertical_centered(|ui| {
            if Btn::new(if stopping { "Stopping…" } else { "Cancel" }).enabled(!stopping).tip("Already stopping").show(ui).clicked() {
                self.ask_stop = true;
            }
        });
    }

    // ------------------------------------------------------------------ done

    fn done_ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        let outcome = self.outcome.clone().unwrap_or(Outcome::Running);
        let img = self.image.as_ref().map(|i| i.name.clone()).unwrap_or_default();
        let tgt = self.target.as_ref().map(|c| format!("{} ({}, {})", c.name, c.size_text(), c.drive.path)).unwrap_or_default();
        let mut again = false;
        ui.vertical_centered(|ui| {
            ui.add_space(18.0);
            match &outcome {
                Outcome::Success => {
                    status_mark(ui, Id::new("flash-mark"), Tone::Success, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text("Flash complete!", 28.0));
                    ui.add_space(10.0);
                    let mut rows = vec![("Image", img.clone()), ("Drive", tgt.clone())];
                    if let Some((secs, speed)) = self.run.as_ref().and_then(|r| r.finished) {
                        if secs >= 0.0 {
                            let sp = if speed > 0.0 { format!(" · {} average", super::fmt_speed(speed)) } else { String::new() };
                            rows.push(("Time", format!("{}{}", super::fmt_duration(secs), sp)));
                        }
                    }
                    widgets::result_card(ui, &rows);
                }
                Outcome::Cancelled => {
                    status_mark(ui, Id::new("flash-mark"), Tone::Warn, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text("Flash cancelled", 22.0));
                    ui.label(muted(format!("Writing to {tgt} was stopped.")));
                    ui.add_space(6.0);
                    ui.allocate_ui(vec2(460.0, 10.0), |ui| {
                        banner(ui, Tone::Warn, "The drive is probably unusable now", "It may hold a partly written image. Flash it again, or format it, before using it.", None);
                    });
                }
                Outcome::Failed(f) => {
                    status_mark(ui, Id::new("flash-mark"), Tone::Error, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text("Flash failed", 22.0));
                    ui.label(muted(format!("{img} was not written to {tgt}.")));
                    ui.add_space(6.0);
                    ui.allocate_ui(vec2(520.0, 10.0), |ui| {
                        banner(ui, Tone::Error, &f.title, &f.message, f.hint.as_deref());
                    });
                    ui.label(small_muted("The drive may hold a partly written image. Try again, or format it before using it."));
                }
                Outcome::Running => {}
            }
            ui.add_space(14.0);
            let main = if matches!(outcome, Outcome::Success) { "Flash another" } else { "Try again" };
            let (p, h) = widgets::centered_pair(ui, Btn::primary(main).icon(Icon::Refresh).big(), Btn::new("Home").icon(Icon::Home));
            again = p;
            if h {
                sh.go = Some(Screen::Home);
            }
        });
        if again {
            let ctx = ui.ctx().clone();
            if matches!(outcome, Outcome::Success) {
                self.image = None;
            }
            self.reset_for_another(&ctx);
        }
    }

    // ------------------------------------------------------------------ dialogs

    fn dialogs(&mut self, ctx: &egui::Context) {
        if matches!(self.stage, Stage::Setup) {
            self.sel.modal(ctx);
        }
        // confirmation
        let mut accept = false;
        let mut cancel_confirm = false;
        if let Some(g) = &self.step {
            if let Some(c) = g.candidate() {
                let rechecking = g.is_rechecking();
                let img_name = self.image.as_ref().map(|i| i.name.clone()).unwrap_or_default();
                let img_size = self.image.as_ref().map(|i| human_size(i.file_size)).unwrap_or_default();
                let (_, close) = dialog(ctx, "flash-confirm", 440.0, |ui| {
                    ui.label(heading("Erase this drive and flash?"));
                    ui.add_space(8.0);
                    kv(ui, "Image", RichText::new(format!("{img_name}  ({img_size})")));
                    ui.add_space(4.0);
                    widgets::drive_card(ui, c);
                    ui.add_space(8.0);
                    banner(ui, Tone::Error, "Everything on this drive will be permanently erased.", "This cannot be undone. Check that the name and size above are the drive you mean.", None);
                    for w in &c.warnings {
                        ui.add_space(4.0);
                        banner(ui, Tone::Warn, "", w, None);
                    }
                    dialog_buttons(ui, |ui| {
                        if Btn::destructive(if rechecking { "Checking…" } else { "Erase and flash" }).enabled(!rechecking).show(ui).clicked() {
                            accept = true;
                        }
                        if Btn::new("Cancel").enabled(!rechecking).show(ui).clicked() {
                            cancel_confirm = true;
                        }
                    });
                });
                if close && !rechecking {
                    cancel_confirm = true;
                }
            }
        }
        if cancel_confirm {
            self.step = None;
        }
        if accept {
            if let Some(g) = &mut self.step {
                g.accept(ctx);
            }
        }
        // stop confirmation
        if self.ask_stop && matches!(self.stage, Stage::Running) {
            let mut stop = false;
            let mut keep = false;
            let (_, close) = dialog(ctx, "flash-stop", 400.0, |ui| {
                ui.label(heading("Stop flashing?"));
                ui.add_space(6.0);
                ui.label(muted("The drive will be left with a partly written image and will not work until it is flashed or formatted again."));
                dialog_buttons(ui, |ui| {
                    if Btn::destructive("Stop flashing").show(ui).clicked() {
                        stop = true;
                    }
                    if Btn::new("Keep going").show(ui).clicked() {
                        keep = true;
                    }
                });
            });
            if stop {
                self.abort();
                self.ask_stop = false;
            } else if keep || close {
                self.ask_stop = false;
            }
        }
    }
}
