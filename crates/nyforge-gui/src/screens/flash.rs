//! NyFlash: write an image to a USB drive.

use super::{file_name, permission_notice, pick_files, run_panel, Screen, Shell};
use crate::checkdlg::ChecksumDialog;
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
    /// What the ISO holds, when it is a Windows installer (install image, size, own answer file).
    win: Option<nyforge_core::nyboot::probe::WindowsInfo>,
    /// Set for images (Void, the BSDs) whose own boot partition other systems rewrite, so a read-back may differ there.
    verify_heads_up: Option<String>,
}

/// The first bytes of an image (decompressed), enough to read its partition table. `None` when it cannot be read.
fn read_head(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut src = image::open(&path.to_string_lossy()).ok()?;
    let mut head = vec![0u8; 64 * 1024];
    let mut n = 0;
    while n < head.len() {
        match src.reader.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return None,
        }
    }
    head.truncate(n);
    Some(head)
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

/// What the engine is doing in the current run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    /// Writing an image.
    Flash,
    /// Making a Windows installer from a Windows ISO.
    Windows,
    /// Only reading a finished drive back and comparing it.
    VerifyOnly,
}

/// How the last write was made, so the drive can be read back the same way.
#[derive(Clone)]
struct Written {
    windows: bool,
    /// The setup-option flags the installer was made with (read back the same way).
    answers: Vec<String>,
    bios: bool,
}

/// The Windows setup options ticked on the screen.
#[derive(Clone, Default)]
struct WinAnswers {
    skip_online: bool,
    local: bool,
    local_name: String,
    privacy: bool,
    bitlocker: bool,
    locale: bool,
}

impl WinAnswers {
    fn name_problem(&self) -> Option<String> {
        if !self.local {
            return None;
        }
        nyforge_core::winmedia::check_account_name(&self.local_name).err().map(|e| e.message)
    }

    fn flags(&self) -> Vec<String> {
        let mut v = vec![];
        if self.local {
            v.extend(["--local-account".to_string(), self.local_name.clone()]);
        } else if self.skip_online {
            v.push("--skip-online-account".into());
        }
        if self.privacy {
            v.push("--skip-privacy-questions".into());
        }
        if self.bitlocker {
            v.push("--disable-bitlocker".into());
        }
        if self.locale {
            v.extend(["--locale".to_string(), "auto".to_string()]);
        }
        v
    }
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
    /// A Windows ISO: write it byte for byte instead of making an installer from it.
    win_as_is: bool,
    /// Windows 11: skip the TPM, Secure Boot and RAM checks in Setup.
    win_bypass: bool,
    win_answers: WinAnswers,
    /// Also make the installer start on old BIOS computers.
    win_bios: bool,
    /// Read the drive back when the write is done (can be switched on while it runs).
    verify_after: bool,
    /// Detach the drive when the write (and check) is done. Not offered on Windows.
    eject_after: bool,
    check: ChecksumDialog,
    kind: RunKind,
    written: Option<Written>,
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
            win_as_is: false,
            win_bypass: false,
            win_answers: WinAnswers::default(),
            win_bios: true,
            verify_after: false,
            eject_after: false,
            check: ChecksumDialog::new(),
            kind: RunKind::Flash,
            written: None,
        };
        // Testing aid: preselect an image without the native picker.
        if let Some(p) = std::env::var_os("NYFORGE_GUI_TEST_IMAGE") {
            s.load_image(PathBuf::from(p), &egui::Context::default());
        }
        if std::env::var_os("NYFORGE_GUI_TEST_CHECKSUM").is_some() {
            if let Some(p) = std::env::var_os("NYFORGE_GUI_TEST_IMAGE") {
                s.check.open(PathBuf::from(p), &egui::Context::default());
            }
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
                let report = if name.to_ascii_lowercase().ends_with(".iso") { probe::analyze_file(&path).ok().filter(|r| r.verdict == Verdict::WindowsInstaller) } else { None };
                let windows_installer = report.is_some();
                let win = report.and_then(|r| r.windows);
                let verify_heads_up = if windows_installer { None } else { read_head(&path).and_then(|h| nyforge_core::verify::verify_heads_up(&h, &name, info.decompressed_size)) };
                Ok(ImageSel { name, file_size: meta.len(), info, windows_installer, win, verify_heads_up, path })
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

    /// Is the chosen image a Windows installer that NyForge will turn into an installer (rather than copy as it is)?
    fn windows_mode(&self) -> bool {
        self.image.as_ref().is_some_and(|i| i.windows_installer) && !self.win_as_is
    }

    fn start_flash(&mut self, cand: Candidate, ctx: &egui::Context) {
        let Some(img) = &self.image else { return };
        let win = self.windows_mode();
        let own_answers = img.win.as_ref().is_some_and(|w| w.own_unattend);
        let bypass = self.win_bypass && !own_answers;
        let mut args = vec![if win { "windows" } else { "flash" }.to_string(), img.path.to_string_lossy().into_owned(), cand.drive.path.clone(), "-y".to_string()];
        let mut answers: Vec<String> = vec![];
        if win {
            if bypass {
                answers.push("--bypass-checks".into());
            }
            if !own_answers {
                answers.extend(self.win_answers.flags());
            }
            args.extend(answers.iter().cloned());
            if !self.win_bios {
                args.push("--uefi-only".into());
            }
        }
        if self.verify_after {
            args.push("--verify".into());
        }
        if self.eject_after && !cfg!(windows) {
            args.push("--eject".into());
        }
        args.extend(drives::engine_flags(&cand));
        self.written = Some(Written { windows: win, answers, bios: self.win_bios });
        self.kind = if win { RunKind::Windows } else { RunKind::Flash };
        self.run = Some(Run::new(worker::spawn(args, ctx.clone()), true));
        self.target = Some(cand);
        self.stage = Stage::Running;
        self.outcome = None;
        self.ask_stop = false;
        self.step = None;
    }

    /// Read the drive back and compare it with the image it was written from (no writing).
    fn start_verify(&mut self, ctx: &egui::Context) {
        let (Some(img), Some(cand), Some(w)) = (&self.image, &self.target, self.written.clone()) else { return };
        let mut args = vec!["verify".to_string(), img.path.to_string_lossy().into_owned(), cand.drive.path.clone()];
        if w.windows {
            args.push("--windows".into());
            args.extend(w.answers.iter().cloned());
            if !w.bios {
                args.push("--uefi-only".into());
            }
        }
        self.run = Some(Run::new(worker::spawn(args, ctx.clone()), false));
        self.kind = RunKind::VerifyOnly;
        self.stage = Stage::Running;
        self.outcome = None;
        self.ask_stop = false;
    }

    fn reset_for_another(&mut self, ctx: &egui::Context) {
        self.stage = Stage::Setup;
        self.run = None;
        self.target = None;
        self.outcome = None;
        self.step = None;
        self.written = None;
        self.kind = RunKind::Flash;
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
                let (ok, what) = match self.kind {
                    RunKind::VerifyOnly => ("Drive verified", "The drive matches the image."),
                    RunKind::Windows => ("Windows installer ready", "You can unplug the drive."),
                    RunKind::Flash => ("Flash complete", "You can unplug the drive."),
                };
                if let Some(o) = &self.outcome {
                    crate::notify::job_done(&ctx, "NyFlash", ok, what, o);
                }
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
        } else if self.windows_mode() && cfg!(target_os = "macos") && self.image.as_ref().and_then(|i| i.win.as_ref()).is_some_and(|w| w.exceeds_fat32 && w.install_image.as_deref() == Some("install.esd")) {
            Some("This install.esd needs an NTFS drive, which macOS cannot write (see above).".into())
        } else if let Some(p) = self.windows_mode().then(|| self.win_answers.name_problem()).flatten() {
            Some(p)
        } else if self.step.is_some() {
            Some("Checking the drive…".into())
        } else {
            None
        };
        let img_ok = self.image.is_some();
        let drv_ok = self.sel.selected().is_some();
        let ready = blocker.is_none();
        let (mut pick_image, mut remove, mut pick_drive, mut flash, mut open_check) = (false, false, false, false, false);

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
                            let need = 236.0;
                            ui.add_space(((w - need) / 2.0).max(0.0));
                            if widgets::link_button(ui, "Change", true).clicked() {
                                pick_image = true;
                            }
                            if widgets::link_button(ui, "Remove", true).clicked() {
                                remove = true;
                            }
                            if widgets::link_button(ui, "Checksum", true).on_hover_text("Check the download against the checksum on its web page").clicked() {
                                open_check = true;
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
                let label = if self.step.is_some() { "Checking…" } else if self.windows_mode() { "Make installer" } else { "Flash!" };
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
        if img_ok {
            // Small and optional: read the drive back when the write is done (also switchable while it runs).
            ui.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    // Centre the two small options as a pair.
                    let w = ui.available_width();
                    let pair = if cfg!(windows) { 140.0 } else { 300.0 };
                    ui.add_space(((w - pair) / 2.0).max(0.0));
                    ui.checkbox(&mut self.verify_after, "Verify when done").on_hover_text("Reads the drive back afterwards and compares it with the image. Takes about as long as the write.");
                    if !cfg!(windows) {
                        ui.add_space(10.0);
                        ui.checkbox(&mut self.eject_after, "Eject when done").on_hover_text("Detaches the drive once everything is written (and verified), so you can unplug it right away.");
                    }
                });
            });
            ui.add_space(6.0);
        }
        if let Some(note) = self.image.as_ref().and_then(|i| i.verify_heads_up.clone()) {
            let w = ui.available_width().min(560.0);
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(vec2(w, 10.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    banner(ui, Tone::Info, "Verify may show small differences on this image", &note, None);
                });
            });
            ui.add_space(8.0);
        }
        if self.image.as_ref().is_some_and(|i| i.windows_installer) {
            let w = ui.available_width().min(560.0);
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(vec2(w, 10.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    if self.win_as_is {
                        banner(ui, Tone::Warn, "Writing this Windows ISO as it is", "A Windows ISO written byte for byte usually does not give a bootable USB drive.", None);
                        if widgets::link_button(ui, "Make a Windows installer instead", true).clicked() {
                            self.win_as_is = false;
                        }
                    } else {
                        let win = self.image.as_ref().and_then(|i| i.win.clone());
                        let own = win.as_ref().is_some_and(|w| w.own_unattend);
                        let esd_too_big = win.as_ref().is_some_and(|w| w.exceeds_fat32 && w.install_image.as_deref() == Some("install.esd"));
                        let mut text = String::from("NyForge builds a drive that boots on UEFI computers (Secure Boot too) and on old BIOS ones.");
                        if let Some(w) = &win {
                            if let Some(img) = &w.install_image {
                                if w.exceeds_fat32 && img == "install.wim" {
                                    text.push_str(&format!(" The install.wim ({}) is too big for FAT32 and is split automatically.", human_size(w.install_bytes)));
                                } else if !w.exceeds_fat32 {
                                    text.push_str(&format!(" It holds {img} ({}).", human_size(w.install_bytes)));
                                }
                            }
                        }
                        banner(ui, Tone::Info, "This is a Windows installer", &text, None);
                        if esd_too_big {
                            ui.add_space(6.0);
                            let size = win.as_ref().map(|w| human_size(w.install_bytes)).unwrap_or_default();
                            if cfg!(target_os = "macos") {
                                banner(
                                    ui,
                                    Tone::Error,
                                    "This installer needs an NTFS drive",
                                    &format!("Its install.esd is {size}: too big for FAT32, and the compact ESD format cannot be split. macOS cannot write NTFS."),
                                    Some("Make this installer on Windows or Linux, or use a build with install.wim."),
                                );
                            } else {
                                banner(
                                    ui,
                                    Tone::Info,
                                    "This installer is made on an NTFS drive",
                                    &format!("Its install.esd is {size}: too big for FAT32, and the compact ESD format cannot be split. NyForge uses NTFS plus a small UEFI:NTFS boot partition instead. It starts on UEFI computers only (not legacy BIOS)."),
                                    None,
                                );
                            }
                        }
                        ui.add_space(6.0);
                        banner(ui, Tone::Warn, "Some Windows ISOs may not work", nyforge_core::winmedia::WINDOWS_CAVEAT, None);
                        if let Some(w) = &win {
                            let extra: Vec<&String> = w.warnings.iter().filter(|m| !m.contains("install.esd") && !m.contains("autounattend") && !m.contains("over 4 GB")).collect();
                            if !extra.is_empty() {
                                ui.add_space(6.0);
                                let text = extra.iter().map(|m| m.as_str()).collect::<Vec<_>>().join("\n\n");
                                banner(ui, Tone::Warn, &format!("About this disc ({})", w.kind), &text, None);
                            }
                        }
                        if own {
                            ui.add_space(6.0);
                            banner(
                                ui,
                                Tone::Info,
                                "This build brings its own setup answers",
                                "Custom builds such as Tiny10 and Tiny11 include an autounattend.xml. NyForge leaves it exactly as it is.",
                                None,
                            );
                        }
                        ui.add_space(6.0);
                        ui.add_enabled_ui(!own, |ui| {
                            ui.checkbox(&mut self.win_bypass, "Skip the Windows 11 hardware checks (TPM, Secure Boot, RAM)")
                                .on_hover_text(if own { "Not needed: this ISO already has its own setup answers." } else { "Adds an answer file so Setup installs on computers it would normally refuse. Only for Windows 11." });
                        });
                        ui.add_enabled_ui(!own, |ui| {
                            ui.collapsing("More setup options", |ui| {
                                let a = &mut self.win_answers;
                                ui.add_enabled_ui(!a.local, |ui| {
                                    ui.checkbox(&mut a.skip_online, "Skip the Microsoft account screens")
                                        .on_hover_text("Windows starts without asking for a Microsoft account. Newer Windows builds may ignore this; the local account below always works.");
                                });
                                ui.checkbox(&mut a.local, "Create a local account").on_hover_text("An administrator account without a password, so first start needs no sign-in. Set a password in Windows afterwards.");
                                if a.local {
                                    ui.horizontal(|ui| {
                                        ui.label(muted("Name"));
                                        ui.add(egui::TextEdit::singleline(&mut a.local_name).desired_width(160.0).margin(egui::Margin::symmetric(8, 6)).char_limit(20));
                                    });
                                }
                                ui.checkbox(&mut a.privacy, "Skip the privacy questions").on_hover_text("Windows does not ask the data-sharing questions at first start; they are switched off.");
                                ui.checkbox(&mut a.bitlocker, "Turn off automatic BitLocker encryption").on_hover_text("Stops Windows from encrypting the drive by itself on devices that support it.");
                                ui.checkbox(&mut a.locale, "Use this computer's language and region").on_hover_text("Sets the language and region of the new Windows to the ones this computer uses.");
                            });
                        });
                        ui.add_enabled_ui(!esd_too_big, |ui| {
                            ui.checkbox(&mut self.win_bios, "Also start on old BIOS computers (legacy boot)").on_hover_text(if esd_too_big { "Not possible with the NTFS layout this image needs." } else { "Untick for a UEFI-only drive." });
                        });
                        if widgets::link_button(ui, "Write the ISO as it is instead", true).clicked() {
                            self.win_as_is = true;
                        }
                    }
                });
            });
            ui.add_space(8.0);
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
        if open_check {
            if let Some(i) = &self.image {
                self.check.open(i.path.clone(), &ctx);
            }
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
        let verifying = self.kind == RunKind::VerifyOnly || self.run.as_ref().is_some_and(|r| r.is_verifying());
        let (title, icon) = match (verifying, self.kind) {
            (true, _) => ("Verifying", Icon::Check),
            (_, RunKind::Windows) => ("Making the installer", Icon::Flash),
            _ => ("Flashing", Icon::Flash),
        };
        widgets::run_header(ui, icon, title);
        let rows = self.rows();
        if let Some(run) = &self.run {
            run_panel(ui, Id::new("flash-run"), run, run.job.cancel_requested, &rows);
        }
        ui.add_space(8.0);
        let stopping = self.run.as_ref().map(|r| r.job.cancel_requested).unwrap_or(false);
        // Still writing: a small, optional switch to read the drive back afterwards. Once on, it stays on.
        let still_writing = self.kind != RunKind::VerifyOnly && self.run.as_ref().is_some_and(|r| r.finished.is_none() && !r.is_verifying());
        ui.vertical_centered(|ui| {
            if still_writing && !stopping {
                let mut v = self.verify_after;
                let r = ui.add_enabled(!self.verify_after, egui::Checkbox::new(&mut v, "Verify when done")).on_hover_text("Reads the drive back afterwards and compares it with the image.");
                if r.changed() && v {
                    self.verify_after = true;
                    if let Some(run) = &mut self.run {
                        run.job.request_verify();
                    }
                }
                ui.add_space(4.0);
            }
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
        let kind = self.kind;
        let win = kind == RunKind::Windows;
        let checking = kind == RunKind::VerifyOnly;
        // The write itself finished (even if the read-back after it did not go well).
        let write_finished = !checking && self.run.as_ref().is_some_and(|r| r.finished.is_some());
        let verified = self.run.as_ref().and_then(|r| r.verified);
        let (mut again, mut verify, mut check) = (false, false, false);
        ui.vertical_centered(|ui| {
            ui.add_space(18.0);
            match &outcome {
                Outcome::Success => {
                    status_mark(ui, Id::new("flash-mark"), Tone::Success, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text(
                        match kind {
                            RunKind::VerifyOnly => "Drive verified",
                            RunKind::Windows => "Windows installer ready!",
                            RunKind::Flash => "Flash complete!",
                        },
                        28.0,
                    ));
                    ui.add_space(10.0);
                    let mut rows = vec![("Image", img.clone()), ("Drive", tgt.clone())];
                    if !checking {
                        if let Some((secs, speed)) = self.run.as_ref().and_then(|r| r.finished) {
                            if secs >= 0.0 {
                                let sp = if speed > 0.0 { format!(" · {} average", super::fmt_speed(speed)) } else { String::new() };
                                rows.push(("Time", format!("{}{}", super::fmt_duration(secs), sp)));
                            }
                        }
                    }
                    if let Some((secs, _)) = verified {
                        let exact = self.run.as_ref().is_none_or(|r| r.verified_note.is_none());
                        rows.push(("Verified", format!("{} · read back in {}", if exact { "Matches the image" } else { "Matches, apart from one part (see below)" }, super::fmt_duration(secs))));
                    }
                    widgets::result_card(ui, &rows);
                    if let Some(note) = self.run.as_ref().and_then(|r| r.verified_note.clone()) {
                        ui.add_space(8.0);
                        ui.allocate_ui(vec2(460.0, 10.0), |ui| {
                            banner(ui, Tone::Info, "Not an exact match, and that is expected for this image", &note, Some("This is normal for images such as Void Linux and the BSDs. The drive is very likely fine."));
                        });
                    }
                }
                Outcome::Cancelled => {
                    status_mark(ui, Id::new("flash-mark"), Tone::Warn, true);
                    ui.add_space(6.0);
                    if checking || write_finished {
                        ui.label(semibold_text("Verification stopped", 22.0));
                        ui.label(muted(if checking { format!("{tgt} was not checked to the end.") } else { format!("{img} is on {tgt}; it was just not read back to the end.") }));
                    } else {
                        ui.label(semibold_text(if win { "Installer cancelled" } else { "Flash cancelled" }, 22.0));
                        ui.label(muted(format!("Writing to {tgt} was stopped.")));
                        ui.add_space(6.0);
                        ui.allocate_ui(vec2(460.0, 10.0), |ui| {
                            banner(ui, Tone::Warn, "The drive is probably unusable now", "It may hold a partly written image. Flash it again, or format it, before using it.", None);
                        });
                    }
                }
                Outcome::Failed(f) => {
                    status_mark(ui, Id::new("flash-mark"), Tone::Error, true);
                    ui.add_space(6.0);
                    let bad_copy = f.kind == Some(nyforge_core::error::ErrorKind::VerifyFailed);
                    if bad_copy {
                        ui.label(semibold_text(if checking { "The drive does not match" } else { "Written, but it does not verify" }, 22.0));
                        ui.label(muted(format!("What {tgt} holds is not what {img} contains.")));
                    } else if checking {
                        ui.label(semibold_text("Could not verify", 22.0));
                        ui.label(muted(format!("{tgt} could not be checked.")));
                    } else if write_finished {
                        ui.label(semibold_text("Written, but not verified", 22.0));
                        ui.label(muted(format!("{img} was written to {tgt}; reading it back failed.")));
                    } else {
                        ui.label(semibold_text(if win { "Could not make the installer" } else { "Flash failed" }, 22.0));
                        ui.label(muted(format!("{img} was not written to {tgt}.")));
                    }
                    ui.add_space(6.0);
                    ui.allocate_ui(vec2(520.0, 10.0), |ui| {
                        banner(ui, Tone::Error, &f.title, &f.message, f.hint.as_deref());
                    });
                    ui.label(small_muted(if bad_copy {
                        "Do not rely on this drive. Write it again, or try another drive."
                    } else if write_finished || checking {
                        "The image itself was written; only the check did not finish."
                    } else {
                        "The drive may hold a partly written image. Try again, or format it before using it."
                    }));
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
            // Small and optional, after the fact: read the drive back, or check the download's checksum.
            let can_verify = (write_finished || checking) && verified.is_none() && self.written.is_some() && self.image.is_some() && self.target.is_some();
            let can_check = self.image.is_some();
            if can_verify || can_check {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let need = if can_verify && can_check { 240.0 } else { 120.0 };
                    ui.add_space(((ui.available_width() - need) / 2.0).max(0.0));
                    if can_verify {
                        let label = if matches!(outcome, Outcome::Success) && !checking { "Verify drive" } else { "Verify again" };
                        let tip = if cfg!(target_os = "macos") {
                            "Reads the drive back and compares it with the image. macOS may ask for your password again."
                        } else {
                            "Reads the drive back and compares it with the image. Nothing is written."
                        };
                        if widgets::link_button(ui, label, true).on_hover_text(tip).clicked() {
                            verify = true;
                        }
                    }
                    if can_check && widgets::link_button(ui, "Check checksum", true).on_hover_text("Check the download against the checksum on its web page").clicked() {
                        check = true;
                    }
                });
            }
        });
        let ctx = ui.ctx().clone();
        if verify {
            self.start_verify(&ctx);
        }
        if check {
            if let Some(i) = &self.image {
                self.check.open(i.path.clone(), &ctx);
            }
        }
        if again {
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
        self.check.ui(ctx);
        let win = self.windows_mode();
        // confirmation
        let mut accept = false;
        let mut cancel_confirm = false;
        if let Some(g) = &self.step {
            if let Some(c) = g.candidate() {
                let rechecking = g.is_rechecking();
                let img_name = self.image.as_ref().map(|i| i.name.clone()).unwrap_or_default();
                let img_size = self.image.as_ref().map(|i| human_size(i.file_size)).unwrap_or_default();
                let (_, close) = dialog(ctx, "flash-confirm", 440.0, |ui| {
                    ui.label(heading(if win { "Erase this drive and make the installer?" } else { "Erase this drive and flash?" }));
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
                        if Btn::destructive(if rechecking { "Checking…" } else if win { "Erase and make installer" } else { "Erase and flash" }).enabled(!rechecking).show(ui).clicked() {
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
            let reading = self.kind == RunKind::VerifyOnly || self.run.as_ref().is_some_and(|r| r.is_verifying());
            let mut stop = false;
            let mut keep = false;
            let (_, close) = dialog(ctx, "flash-stop", 400.0, |ui| {
                if reading {
                    ui.label(heading("Stop checking?"));
                    ui.add_space(6.0);
                    ui.label(muted("The image is already on the drive. It just will not be read back to the end."));
                } else {
                    ui.label(heading(if win { "Stop making the installer?" } else { "Stop flashing?" }));
                    ui.add_space(6.0);
                    ui.label(muted("The drive will be left with a partly written image and will not work until it is flashed or formatted again."));
                }
                dialog_buttons(ui, |ui| {
                    if Btn::destructive(if reading { "Stop checking" } else if win { "Stop" } else { "Stop flashing" }).show(ui).clicked() {
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
