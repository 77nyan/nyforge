//! NyBoot: a multiboot USB drive. Install it, copy ISOs onto it, remove them again.
//!
//! Everything here maps to a real `nyforge boot …` command. What NyBoot is not (Ventoy-compatible, updatable in
//! place) is said on the screen, and the controls for it are shown disabled rather than faked.

use super::{file_name, permission_notice, pick_files, run_panel, Shell};
use crate::drives::{self, Candidate, DriveId, Purpose};
use crate::gate::{Gate, GateEvent};
use crate::icons::{self, Icon};
use crate::run::{failure_from, Failure, Outcome, Run};
use crate::selector::DriveSelector;
use crate::theme::*;
use crate::widgets::{self, banner, dialog, dialog_buttons, drive_card, heading, icon_button, panel, step_header, Btn, Tone};
use crate::worker;
use eframe::egui::{self, vec2, FontId, Id, RichText, Sense, Ui};
use nyforge_core::format::human_size;
use nyforge_core::nyboot::layout::MIN_DISK_BYTES;
use nyforge_core::nyboot::probe::{self, Verdict};
use nyforge_core::usbformat::{fsmake, Fs};

/// NyBoot's boot files are not signed, so a computer with Secure Boot on skips the drive. Said once, where people will read it.
const SECURE_BOOT_NOTE: &str = "If a computer lists this drive but starts its own system instead, turn Secure Boot off in its firmware settings (NyBoot is not signed).";

const ISO_FILTERS: &[(&str, &[&str])] = &[("ISO images", &["iso", "xz", "gz", "zst"]), ("All files", &["*"])];

#[derive(Clone)]
struct AddFile {
    path: std::path::PathBuf,
    name: String,
    size: u64,
    compressed: bool,
    note: (Tone, String),
}

enum Action {
    Install,
    /// Put the newest boot code and menu on the drive; the images stay.
    Update,
    Add(Vec<AddFile>),
    Remove(String),
}

/// The menu settings shown in their dialog (read from the drive first, written back with Save).
#[derive(Clone)]
struct MenuForm {
    /// 0 = wait for a choice.
    timeout: u32,
    /// `None`: the first entry.
    default: Option<String>,
    text_mode: bool,
    /// Name of a custom theme folder; empty: the built-in theme.
    theme: String,
}

/// Extra kernel options for one image.
#[derive(Clone)]
struct ParamsForm {
    path: String,
    text: String,
}

#[derive(Clone)]
enum Status {
    Unknown,
    /// A partition called NYBOOT-EFI exists. Not proof; reading the drive is.
    Likely,
    Installed,
    NotInstalled,
    ReadFailed(Failure),
}

struct Item {
    path: String,
    size: u64,
    dir: bool,
}

struct Contents {
    free: u64,
    total: u64,
    items: Vec<Item>,
}

#[derive(PartialEq, Clone)]
enum OpKind {
    Install,
    Update,
    Add(usize),
    Remove(String),
    List,
    ReadConfig,
    SaveConfig,
    ReadParams(String),
    SaveParams,
}

impl OpKind {
    /// Short reads that happen in the background without taking over the screen.
    fn quiet(&self) -> bool {
        matches!(self, OpKind::List | OpKind::ReadConfig | OpKind::ReadParams(_))
    }
}

struct Active {
    kind: OpKind,
    run: Run,
    target: Candidate,
}

pub struct BootScreen {
    sel: DriveSelector,
    status: Status,
    contents: Option<Contents>,
    active: Option<Active>,
    gate: Option<Gate<Action>>,
    notice: Option<String>,
    result: Option<(Tone, String, String)>,
    install_label: String,
    ask_stop: bool,
    last_list_id: Option<DriveId>,
    menu_form: Option<MenuForm>,
    params_form: Option<ParamsForm>,
}

impl BootScreen {
    pub fn new() -> Self {
        BootScreen {
            sel: DriveSelector::new(Purpose::Boot, "boot-drives"),
            status: Status::Unknown,
            contents: None,
            active: None,
            gate: None,
            notice: None,
            result: None,
            install_label: "NYBOOT".into(),
            ask_stop: false,
            last_list_id: None,
            menu_form: None,
            params_form: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.active.as_ref().map(|a| !a.kind.quiet()).unwrap_or(false) || self.gate.as_ref().map(|g| g.is_rechecking()).unwrap_or(false)
    }

    pub fn abort(&mut self) {
        if let Some(a) = &mut self.active {
            a.run.job.cancel();
        }
    }

    fn reset_drive_state(&mut self) {
        self.contents = None;
        self.result = None;
        self.status = match self.sel.selected() {
            Some(c) if c.drive.is_nyboot() => Status::Likely,
            _ => Status::Unknown,
        };
        self.last_list_id = None;
    }

    fn start(&mut self, kind: OpKind, args: Vec<String>, target: Candidate, ctx: &egui::Context) {
        let needs_finished = matches!(kind, OpKind::Install | OpKind::Update | OpKind::Add(_));
        let mut args = args;
        args.extend(drives::engine_flags(&target));
        self.active = Some(Active { kind, run: Run::new(worker::spawn(args, ctx.clone()), needs_finished), target });
        self.ask_stop = false;
    }

    fn start_list(&mut self, ctx: &egui::Context) {
        if let Some(c) = self.sel.selected().cloned() {
            self.last_list_id = Some(c.id.clone());
            let args = vec!["boot".into(), "ls".into(), c.drive.path.clone()];
            self.start(OpKind::List, args, c, ctx);
        }
    }

    fn launch(&mut self, action: Action, cand: Candidate, ctx: &egui::Context) {
        let t = cand.drive.path.clone();
        match action {
            Action::Install => {
                let mut args: Vec<String> = vec!["boot".into(), "install".into(), t, "--label".into(), self.install_label.trim().to_string(), "-y".into()];
                if cand.test_file {
                    args.extend(["--size".into(), cand.drive.size.to_string()]);
                }
                self.contents = None;
                self.start(OpKind::Install, args, cand, ctx);
            }
            Action::Update => {
                self.start(OpKind::Update, vec!["boot".into(), "update".into(), t], cand, ctx);
            }
            Action::Add(files) => {
                let mut args = vec!["boot".into(), "add".into(), t];
                args.extend(files.iter().map(|f| f.path.to_string_lossy().into_owned()));
                self.start(OpKind::Add(files.len()), args, cand, ctx);
            }
            Action::Remove(path) => {
                let args = vec!["boot".into(), "rm".into(), t, path.clone()];
                self.start(OpKind::Remove(path), args, cand, ctx);
            }
        }
        self.result = None;
    }

    fn finish_active(&mut self, ctx: &egui::Context) {
        let Some(a) = self.active.take() else { return };
        let outcome = a.run.outcome();
        if !a.kind.quiet() {
            let ok = match &a.kind {
                OpKind::Install => "NyBoot installed",
                OpKind::Update => "NyBoot updated",
                OpKind::Add(_) => "Images added",
                _ => "Done",
            };
            crate::notify::job_done(ctx, "NyBoot", ok, "You can unplug the drive.", &outcome);
        }
        let name = a.target.name.clone();
        let mut relist = false;
        match (&a.kind, &outcome) {
            (OpKind::List, Outcome::Success) => {
                if let Some(v) = a.run.find_json("items") {
                    let items = v["items"].as_array().map(|arr| arr.iter().map(|i| Item { path: i["path"].as_str().unwrap_or("").to_string(), size: i["size"].as_u64().unwrap_or(0), dir: i["dir"].as_bool().unwrap_or(false) }).collect()).unwrap_or_default();
                    self.contents = Some(Contents { free: v["free"].as_u64().unwrap_or(0), total: v["total"].as_u64().unwrap_or(0), items });
                    self.status = Status::Installed;
                }
            }
            (OpKind::List, Outcome::Failed(f)) => {
                self.contents = None;
                self.status = if f.message.contains("not a NyBoot drive") { Status::NotInstalled } else { Status::ReadFailed(f.clone()) };
            }
            (OpKind::List, _) => {}
            (OpKind::Update, Outcome::Success) => {
                self.result = Some((Tone::Success, "NyBoot updated".into(), format!("{name} now has the newest boot code and menu. Your images and settings were not touched. {SECURE_BOOT_NOTE}")));
                relist = true;
            }
            (OpKind::Update, Outcome::Cancelled) => {
                self.result = Some((Tone::Warn, "Update stopped".into(), "Run the update again to finish it. Your images are untouched.".into()));
            }
            (OpKind::Update, Outcome::Failed(f)) => {
                self.result = Some((Tone::Error, f.title.clone(), join(&f.message, &f.hint)));
            }
            (OpKind::ReadConfig, Outcome::Success) => {
                if let Some(v) = a.run.find_json("timeout_secs") {
                    self.menu_form = Some(MenuForm {
                        timeout: v["timeout_secs"].as_u64().unwrap_or(0) as u32,
                        default: v["default_entry"].as_str().map(str::to_string),
                        text_mode: v["text_mode"].as_bool().unwrap_or(false),
                        theme: v["theme"].as_str().unwrap_or("").to_string(),
                    });
                }
            }
            (OpKind::ReadConfig, Outcome::Failed(f)) | (OpKind::ReadParams(_), Outcome::Failed(f)) => {
                self.result = Some((Tone::Error, f.title.clone(), join(&f.message, &f.hint)));
            }
            (OpKind::ReadConfig, _) | (OpKind::ReadParams(_), Outcome::Cancelled) => {}
            (OpKind::ReadParams(path), Outcome::Success) => {
                let text = a.run.find_json("params").and_then(|v| v["params"].as_str()).unwrap_or("").to_string();
                self.params_form = Some(ParamsForm { path: path.clone(), text });
            }
            (OpKind::ReadParams(_), _) => {}
            (OpKind::SaveConfig, Outcome::Success) => {
                self.result = Some((Tone::Success, "Menu settings saved".into(), "They apply the next time the drive starts.".into()));
            }
            (OpKind::SaveParams, Outcome::Success) => {
                self.result = Some((Tone::Success, "Boot options saved".into(), "They are added the next time this image starts.".into()));
            }
            (OpKind::SaveConfig, Outcome::Failed(f)) | (OpKind::SaveParams, Outcome::Failed(f)) => {
                self.result = Some((Tone::Error, f.title.clone(), join(&f.message, &f.hint)));
            }
            (OpKind::SaveConfig, _) | (OpKind::SaveParams, _) => {}
            (OpKind::Install, Outcome::Success) => {
                self.result = Some((Tone::Success, "NyBoot installed".into(), format!("{name} is now a NyBoot drive. Add ISO images below. {SECURE_BOOT_NOTE}")));
                self.status = Status::Installed;
                relist = true;
            }
            (OpKind::Install, Outcome::Cancelled) => {
                self.result = Some((Tone::Warn, "Installation cancelled".into(), format!("{name} may be half set up. Install NyBoot again, or format it, before using it.")));
                self.status = Status::Unknown;
            }
            (OpKind::Install, Outcome::Failed(f)) => {
                self.result = Some((Tone::Error, f.title.clone(), join(&f.message, &f.hint)));
                self.status = Status::Unknown;
            }
            (OpKind::Add(n), Outcome::Success) => {
                self.result = Some((Tone::Success, if *n == 1 { "Image added".into() } else { format!("{n} images added") }, format!("Copied to {name}.")));
                relist = true;
            }
            (OpKind::Add(_), Outcome::Cancelled) => {
                self.result = Some((Tone::Warn, "Copy cancelled".into(), "The image being copied is incomplete. Remove it from the list below if it appears there.".into()));
                relist = true;
            }
            (OpKind::Add(_), Outcome::Failed(f)) => {
                self.result = Some((Tone::Error, f.title.clone(), join(&f.message, &f.hint)));
                relist = true;
            }
            (OpKind::Remove(p), Outcome::Success) => {
                self.result = Some((Tone::Success, "File removed".into(), format!("{p} was deleted from the drive. The bootloader was not touched.")));
                relist = true;
            }
            (OpKind::Remove(_), Outcome::Failed(f)) => {
                self.result = Some((Tone::Error, f.title.clone(), join(&f.message, &f.hint)));
                relist = true;
            }
            _ => {}
        }
        if relist {
            self.start_list(ctx);
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        let ctx = ui.ctx().clone();
        // --- background work
        if let Some(a) = &mut self.active {
            a.run.pump();
            if a.run.is_done() {
                self.finish_active(&ctx);
            }
        }
        if let Some(g) = &mut self.gate {
            match g.poll() {
                GateEvent::Waiting => {}
                GateEvent::Failed(e) => {
                    self.gate = None;
                    self.notice = Some(e);
                    self.sel.refresh(&ctx);
                }
                GateEvent::Start(c) => {
                    if let Some(g) = self.gate.take() {
                        self.launch(g.action, c, &ctx);
                    }
                }
            }
        }
        if self.active.is_none() {
            if self.sel.poll(&ctx, self.gate.is_none()) {
                self.reset_drive_state();
            }
            if self.sel.auto_select_nyboot() {
                self.reset_drive_state();
            }
            if !self.sel.loaded && !self.sel.loading() {
                self.sel.refresh(&ctx);
            }
            // With administrator rights already in hand, reading the drive costs nothing: do it right away.
            if sh.can_write() && sh.privilege == crate::elevation::Privilege::Elevated {
                if let Some(c) = self.sel.selected() {
                    if self.last_list_id.as_ref() != Some(&c.id) {
                        self.start_list(&ctx);
                    }
                }
            }
        }

        if self.active.as_ref().map(|a| !a.kind.quiet()).unwrap_or(false) {
            self.running_ui(ui);
        } else {
            self.main_ui(ui, sh);
        }
        self.dialogs(&ctx);
    }

    fn running_ui(&mut self, ui: &mut Ui) {
        let Some(a) = &self.active else { return };
        let (title, can_stop) = match &a.kind {
            OpKind::Install => (format!("Installing NyBoot on {}", a.target.name), true),
            OpKind::Add(1) => ("Adding an image".to_string(), true),
            OpKind::Add(n) => (format!("Adding {n} images"), true),
            OpKind::Remove(p) => (format!("Removing {p}"), false),
            OpKind::Update => (format!("Updating NyBoot on {}", a.target.name), false),
            OpKind::SaveConfig => ("Saving the menu settings".to_string(), false),
            OpKind::SaveParams => ("Saving the boot options".to_string(), false),
            OpKind::List | OpKind::ReadConfig | OpKind::ReadParams(_) => (String::new(), false),
        };
        widgets::run_header(ui, Icon::Boot, &title);
        let rows = vec![("Drive", format!("{} · {} · {}", a.target.name, a.target.size_text(), a.target.drive.path))];
        run_panel(ui, Id::new("boot-run"), &a.run, a.run.job.cancel_requested, &rows);
        ui.add_space(8.0);
        if can_stop {
            let stopping = a.run.job.cancel_requested;
            ui.vertical_centered(|ui| {
                if Btn::new(if stopping { "Stopping…" } else { "Cancel" }).enabled(!stopping).show(ui).clicked() {
                    self.ask_stop = true;
                }
            });
        }
    }

    fn main_ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        let ctx = ui.ctx().clone();
        permission_notice(ui, sh);
        if let Some(n) = self.notice.clone() {
            banner(ui, Tone::Error, "Drive check failed", &n, None);
        }
        if let Some((tone, title, body)) = self.result.clone() {
            banner(ui, tone, &title, &body, None);
            ui.add_space(6.0);
        }
        // --- drive
        widgets::page_title(ui, "NyBoot", "Put several ISOs on one USB drive and choose at startup.");
        if widgets::drive_slot(ui, self.sel.selected(), self.gate.is_none()) {
            self.sel.open_picker(&ctx);
        }
        ui.add_space(12.0);
        // --- bootloader
        let sel = self.sel.selected().cloned();
        panel(ui, |ui| {
            step_header(ui, 2, "Bootloader", matches!(self.status, Status::Installed));
            ui.add_space(6.0);
            match &sel {
                None => {
                    ui.label(muted("Select a drive to see whether NyBoot is installed on it."));
                }
                Some(c) => {
                    let (text, color) = match &self.status {
                        Status::Installed => ("NyBoot is installed".to_string(), OK_HI),
                        Status::Likely => ("NyBoot drive recognised (images not read yet)".to_string(), OK_HI),
                        Status::NotInstalled => ("NyBoot is not installed on this drive".to_string(), WARN),
                        Status::ReadFailed(_) => ("Could not read the drive".to_string(), DANGER),
                        Status::Unknown => ("Not checked yet".to_string(), MUTED),
                    };
                    widgets::kv(ui, "Status", RichText::new(text).color(color));
                    if let Status::ReadFailed(f) = &self.status {
                        banner(ui, Tone::Error, &f.title, &f.message, f.hint.as_deref());
                    }
                    ui.add_space(6.0);
                    let too_small = c.drive.size < MIN_DISK_BYTES;
                    let why = sh.why_not().or_else(|| too_small.then(|| format!("NyBoot needs a drive of at least {}; this one is {}.", human_size(MIN_DISK_BYTES), c.size_text())));
                    if too_small {
                        banner(ui, Tone::Error, "This drive is too small for NyBoot", &format!("NyBoot needs at least {}. This drive holds {}.", human_size(MIN_DISK_BYTES), c.size_text()), None);
                    }
                    let installed = matches!(self.status, Status::Installed | Status::Likely);
                    ui.horizontal(|ui| {
                        let label = if installed { "Reinstall NyBoot…" } else { "Install NyBoot…" };
                        let mut b = Btn::destructive(label).icon(Icon::Boot).enabled(why.is_none() && self.gate.is_none());
                        if let Some(w) = &why {
                            b = b.tip(w);
                        }
                        if b.show(ui).clicked() {
                            self.gate = Some(Gate::new(Purpose::Boot, &c.id, Action::Install, &ctx));
                            self.notice = None;
                        }
                        let mut up = Btn::new("Update bootloader").enabled(installed && why.is_none() && self.gate.is_none());
                        up = up.tip(if !installed { "NyBoot has to be installed on this drive first." } else { "Puts the newest boot code and menu on the drive. Your images and settings stay." });
                        if up.show(ui).clicked() {
                            self.gate = Some(Gate::new(Purpose::Boot, &c.id, Action::Update, &ctx));
                            self.notice = None;
                        }
                        let mut ms = Btn::new("Menu settings…").enabled(installed && why.is_none() && self.gate.is_none());
                        ms = ms.tip(if !installed { "NyBoot has to be installed on this drive first." } else { "How long the menu waits, which image is preselected, and plain text mode." });
                        if ms.show(ui).clicked() {
                            let args = vec!["boot".into(), "config".into(), c.drive.path.clone()];
                            self.start(OpKind::ReadConfig, args, c.clone(), &ctx);
                        }
                    });
                    ui.add_space(2.0);
                    ui.label(small_muted("Installing or reinstalling erases the whole drive, including every image on it. Updating and removing a single image (below) never erase anything else."));
                }
            }
        });
        ui.add_space(12.0);
        // --- images
        panel(ui, |ui| {
            step_header(ui, 3, "Images on this drive", self.contents.as_ref().map(|c| !c.items.is_empty()).unwrap_or(false));
            ui.add_space(6.0);
            self.images_ui(ui, sh, sel.as_ref());
        });
        ui.add_space(8.0);
        ui.label(dim("NyBoot is NyForge's own multiboot system. It is not Ventoy and does not read Ventoy drives. Each image is checked when you add it, and some (for example Windows installers) cannot boot from it."));
    }

    fn images_ui(&mut self, ui: &mut Ui, sh: &mut Shell, sel: Option<&Candidate>) {
        let ctx = ui.ctx().clone();
        let Some(c) = sel else {
            widgets::empty_state(ui, Icon::Usb, "No drive selected", "Select a drive above to see its images.");
            return;
        };
        let installed = matches!(self.status, Status::Installed);
        let can = sh.can_write();
        let why = sh.why_not();
        let listing = self.active.as_ref().map(|a| a.kind == OpKind::List).unwrap_or(false);
        ui.horizontal(|ui| {
            let mut add = Btn::primary("Add images…").icon(Icon::Plus).enabled(installed && can && self.gate.is_none() && !listing);
            if !installed {
                add = add.tip("NyBoot has to be installed on this drive first.");
            } else if let Some(w) = &why {
                add = add.tip(w);
            }
            if add.show(ui).clicked() {
                let picked = pick_files("Add images to the drive", ISO_FILTERS, true);
                if !picked.is_empty() {
                    let files: Vec<AddFile> = picked.into_iter().map(|p| inspect_add(&p)).collect();
                    self.gate = Some(Gate::new(Purpose::Boot, &c.id, Action::Add(files), &ctx));
                }
            }
            if icon_button(ui, Icon::Refresh, "Refresh the list of images", can && !listing && self.gate.is_none()).clicked() {
                self.start_list(&ctx);
            }
            if listing {
                ui.label(dim("Reading the drive…"));
            }
        });
        ui.add_space(6.0);
        let mut options: Option<String> = None;
        match (&self.contents, &self.status) {
            (Some(ct), _) => {
                if ct.items.is_empty() {
                    widgets::empty_state(ui, Icon::File, "No images yet", "Choose Add images… to copy ISO files onto this drive.");
                } else {
                    let mut remove: Option<String> = None;
                    egui::ScrollArea::vertical().id_salt("boot-images").min_scrolled_height(220.0).max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for it in &ct.items {
                            let w = ui.available_width();
                            egui::Frame::new().fill(FIELD).stroke(egui::Stroke::new(1.0_f32, BORDER)).corner_radius(egui::CornerRadius::same(RADIUS)).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
                                ui.set_width(w - 22.0);
                                ui.horizontal(|ui| {
                                    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                                    icons::paint(ui.painter(), if it.dir { Icon::Folder } else { Icon::File }, r, MUTED);
                                    widgets::fit_label(ui, &it.path, |s| RichText::new(s), FontId::new(14.0, egui::FontFamily::Proportional), w - 190.0);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if !it.dir {
                                            if it.path.to_ascii_lowercase().ends_with(".iso") && Btn::new("Options").small().enabled(can && self.gate.is_none() && !listing).tip("Extra boot options for this image, such as nomodeset").show(ui).clicked() {
                                                options = Some(it.path.clone());
                                            }
                                            if icon_button(ui, Icon::Trash, &format!("Remove {} from the drive", it.path), can && self.gate.is_none()).clicked() {
                                                remove = Some(it.path.clone());
                                            }
                                            ui.label(small_muted(human_size(it.size)));
                                        }
                                    });
                                });
                            });
                        }
                    });
                    if let Some(p) = remove {
                        self.gate = Some(Gate::new(Purpose::Boot, &c.id, Action::Remove(p), &ctx));
                    }
                }
                ui.add_space(6.0);
                let used = ct.total.saturating_sub(ct.free);
                let frac = if ct.total > 0 { used as f32 / ct.total as f32 } else { 0.0 };
                widgets::progress_bar(ui, Id::new("boot-space"), Some(frac));
                ui.label(small_muted(format!("{} used of {} · {} free", human_size(used), human_size(ct.total), human_size(ct.free))));
            }
            (None, Status::NotInstalled) => {
                widgets::empty_state(ui, Icon::Boot, "This drive is not a NyBoot drive yet", "Install NyBoot above, then add images. This erases the drive.");
            }
            (None, Status::ReadFailed(_)) => {
                widgets::empty_state(ui, Icon::Warn, "Could not read the drive", "See the message above, then press the refresh button to try again.");
            }
            (None, _) => {
                if listing {
                    widgets::empty_state(ui, Icon::Usb, "Reading the drive…", "");
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        ui.label(muted(if sh.privilege == crate::elevation::Privilege::AskPerTask { "Reading the drive needs administrator permission, so the system will ask for it." } else { "The drive has not been read yet." }));
                        ui.add_space(6.0);
                        let mut b = Btn::new("Read the drive").icon(Icon::Refresh).enabled(can);
                        if let Some(w) = &why {
                            b = b.tip(w);
                        }
                        if b.show(ui).clicked() {
                            self.start_list(&ctx);
                        }
                        ui.add_space(10.0);
                    });
                }
            }
        }
        if let Some(p) = options {
            let args = vec!["boot".into(), "params".into(), c.drive.path.clone(), p.clone()];
            self.start(OpKind::ReadParams(p), args, c.clone(), &ctx);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.active.is_none() && self.sel.modal(ctx) {
            self.notice = None;
            self.reset_drive_state();
        }
        let mut accept = false;
        let mut cancel = false;
        let label_err: Option<String>;
        if let Some(g) = &self.gate {
            if let Some(c) = g.candidate() {
                let rechecking = g.is_rechecking();
                match &g.action {
                    Action::Install => {
                        label_err = fsmake::validate_label(Fs::ExFat, self.install_label.trim()).err().map(|e| e.message);
                        let label_ok = label_err.is_none() && !self.install_label.trim().is_empty();
                        let (_, close) = dialog(ctx, "boot-install", 460.0, |ui| {
                            ui.label(heading("Install NyBoot and erase this drive?"));
                            ui.add_space(8.0);
                            drive_card(ui, c);
                            ui.add_space(8.0);
                            banner(ui, Tone::Error, "Everything on this drive will be permanently erased.", "That includes any images already on it. The drive is repartitioned: a small UEFI boot partition, and the rest for your images.", None);
                            for w in &c.warnings {
                                ui.add_space(4.0);
                                banner(ui, Tone::Warn, "", w, None);
                            }
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                ui.label(small_muted("Name of the image partition"));
                                ui.add(egui::TextEdit::singleline(&mut self.install_label).desired_width(140.0).margin(egui::Margin::symmetric(8, 7)).char_limit(11));
                            });
                            if let Some(e) = &label_err {
                                ui.label(RichText::new(e).size(12.0).color(DANGER));
                            }
                            dialog_buttons(ui, |ui| {
                                if Btn::destructive(if rechecking { "Checking…" } else { "Erase and install" }).enabled(!rechecking && label_ok).show(ui).clicked() {
                                    accept = true;
                                }
                                if Btn::new("Cancel").enabled(!rechecking).show(ui).clicked() {
                                    cancel = true;
                                }
                            });
                        });
                        if close && !rechecking {
                            cancel = true;
                        }
                    }
                    Action::Add(files) => {
                        let total: u64 = files.iter().filter(|f| !f.compressed).map(|f| f.size).sum();
                        let free = self.contents.as_ref().map(|c| c.free);
                        let too_big = free.map(|f| total > f).unwrap_or(false);
                        let (_, close) = dialog(ctx, "boot-add", 520.0, |ui| {
                            ui.label(heading(&if files.len() == 1 { "Add this image?".to_string() } else { format!("Add {} images?", files.len()) }));
                            ui.label(small_muted(format!("To {} · {}", c.name, c.drive.path)));
                            ui.add_space(8.0);
                            egui::ScrollArea::vertical().id_salt("boot-add-list").min_scrolled_height(220.0).max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                                for f in files {
                                    ui.horizontal(|ui| {
                                        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                                        icons::paint(ui.painter(), Icon::File, r, MUTED);
                                        ui.vertical(|ui| {
                                            ui.spacing_mut().item_spacing.y = 1.0;
                                            widgets::fit_label(ui, &f.name, |s| semibold_text(s, 13.5), FontId::new(13.5, semibold()), 380.0);
                                            let col = match f.note.0 {
                                                Tone::Success => OK_HI,
                                                Tone::Warn => WARN,
                                                Tone::Error => DANGER,
                                                Tone::Info => MUTED,
                                            };
                                            ui.label(RichText::new(format!("{} · {}", human_size(f.size), f.note.1)).size(12.0).color(col));
                                        });
                                    });
                                    ui.add_space(2.0);
                                }
                            });
                            if too_big {
                                ui.add_space(6.0);
                                banner(ui, Tone::Error, "Not enough free space", &format!("These images need {} but only {} is free on the drive.", human_size(total), human_size(free.unwrap_or(0))), None);
                            } else {
                                ui.add_space(6.0);
                                ui.label(small_muted("Existing images are kept. Images NyBoot cannot start are still copied; they just will not boot."));
                            }
                            dialog_buttons(ui, |ui| {
                                if Btn::primary(if rechecking { "Checking…" } else { "Add to drive" }).enabled(!rechecking && !too_big).show(ui).clicked() {
                                    accept = true;
                                }
                                if Btn::new("Cancel").enabled(!rechecking).show(ui).clicked() {
                                    cancel = true;
                                }
                            });
                        });
                        if close && !rechecking {
                            cancel = true;
                        }
                    }
                    Action::Update => {
                        let (_, close) = dialog(ctx, "boot-update", 460.0, |ui| {
                            ui.label(heading("Update NyBoot on this drive?"));
                            ui.add_space(8.0);
                            drive_card(ui, c);
                            ui.add_space(8.0);
                            banner(ui, Tone::Info, "Your images are not touched.", "Only the boot code and the menu are replaced with the newest ones. Your images, your menu settings and the partitions stay exactly as they are. Do not unplug the drive while it works.", None);
                            dialog_buttons(ui, |ui| {
                                if Btn::primary(if rechecking { "Checking…" } else { "Update" }).enabled(!rechecking).show(ui).clicked() {
                                    accept = true;
                                }
                                if Btn::new("Cancel").enabled(!rechecking).show(ui).clicked() {
                                    cancel = true;
                                }
                            });
                        });
                        if close && !rechecking {
                            cancel = true;
                        }
                    }
                    Action::Remove(path) => {
                        let (_, close) = dialog(ctx, "boot-remove", 440.0, |ui| {
                            ui.label(heading("Remove this file from the drive?"));
                            ui.add_space(6.0);
                            ui.label(semibold_text(path.clone(), 14.0));
                            ui.label(small_muted(format!("On {} · {}", c.name, c.drive.path)));
                            ui.add_space(8.0);
                            banner(ui, Tone::Warn, "The file is deleted from the drive.", "Only this file is removed. NyBoot and your other images stay as they are.", None);
                            dialog_buttons(ui, |ui| {
                                if Btn::destructive(if rechecking { "Checking…" } else { "Remove file" }).enabled(!rechecking).show(ui).clicked() {
                                    accept = true;
                                }
                                if Btn::new("Cancel").enabled(!rechecking).show(ui).clicked() {
                                    cancel = true;
                                }
                            });
                        });
                        if close && !rechecking {
                            cancel = true;
                        }
                    }
                }
            }
        }
        self.form_dialogs(ctx);
        if cancel {
            self.gate = None;
        }
        if accept {
            if let Some(g) = &mut self.gate {
                g.accept(ctx);
            }
        }
        if self.ask_stop && self.active.as_ref().map(|a| !a.kind.quiet()).unwrap_or(false) {
            let installing = matches!(self.active.as_ref().map(|a| &a.kind), Some(OpKind::Install));
            let (mut stop, mut keep) = (false, false);
            let (_, close) = dialog(ctx, "boot-stop", 420.0, |ui| {
                ui.label(heading(if installing { "Stop installing NyBoot?" } else { "Stop copying?" }));
                ui.add_space(6.0);
                ui.label(muted(if installing { "The drive will be left half set up and will not work until NyBoot is installed again or the drive is formatted." } else { "The image being copied will be incomplete. You can remove it from the list afterwards." }));
                dialog_buttons(ui, |ui| {
                    if Btn::destructive("Stop").show(ui).clicked() {
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

impl BootScreen {
    fn form_dialogs(&mut self, ctx: &egui::Context) {
        let Some(c) = self.sel.selected().cloned() else {
            self.menu_form = None;
            self.params_form = None;
            return;
        };
        let images: Vec<String> = self
            .contents
            .as_ref()
            .map(|ct| ct.items.iter().filter(|i| !i.dir && i.path.to_ascii_lowercase().ends_with(".iso")).map(|i| i.path.rsplit('/').next().unwrap_or(&i.path)[..i.path.rsplit('/').next().unwrap_or(&i.path).len() - 4].to_string()).collect())
            .unwrap_or_default();
        if let Some(mut f) = self.menu_form.take() {
            let f = &mut f;
            let (mut save, mut cancel) = (false, false);
            let (_, close) = dialog(ctx, "boot-menu", 460.0, |ui| {
                ui.label(heading("Menu settings"));
                ui.label(small_muted(format!("For {} · applies the next time it starts", c.name)));
                ui.add_space(10.0);
                egui::Grid::new("boot-menu-grid").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                    ui.label(small_muted("Start by itself after"));
                    let tname = |t: u32| if t == 0 { "Never: wait for my choice".to_string() } else { format!("{t} seconds") };
                    egui::ComboBox::from_id_salt("boot-timeout").selected_text(tname(f.timeout)).width(250.0).show_ui(ui, |ui| {
                        for t in [0u32, 3, 5, 10, 30, 60] {
                            ui.selectable_value(&mut f.timeout, t, tname(t));
                        }
                        if ![0u32, 3, 5, 10, 30, 60].contains(&f.timeout) {
                            let t = f.timeout;
                            ui.selectable_value(&mut f.timeout, t, tname(t));
                        }
                    });
                    ui.end_row();
                    ui.label(small_muted("Preselected image"));
                    let dname = |d: &Option<String>| d.clone().unwrap_or_else(|| "The first one".to_string());
                    egui::ComboBox::from_id_salt("boot-default").selected_text(dname(&f.default)).width(250.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut f.default, None, "The first one");
                        for n in &images {
                            ui.selectable_value(&mut f.default, Some(n.clone()), n.as_str());
                        }
                        if let Some(d) = f.default.clone() {
                            if !images.contains(&d) {
                                ui.selectable_value(&mut f.default, Some(d.clone()), d);
                            }
                        }
                    });
                    ui.end_row();
                    ui.label(small_muted("Theme"));
                    ui.add(egui::TextEdit::singleline(&mut f.theme).desired_width(250.0).margin(egui::Margin::symmetric(8, 6)).hint_text(RichText::new("empty: the NyBoot theme").color(DIM)))
                        .on_hover_text("The name of a theme folder in /nyboot/themes on the drive. Add one with: nyforge boot theme <drive> <folder>");
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.checkbox(&mut f.text_mode, "Plain text menu").on_hover_text("Use this if the graphical menu shows nothing or looks wrong on an old computer.");
                dialog_buttons(ui, |ui| {
                    if Btn::primary("Save").show(ui).clicked() {
                        save = true;
                    }
                    if Btn::new("Cancel").show(ui).clicked() {
                        cancel = true;
                    }
                });
            });
            if save {
                let mut args: Vec<String> = vec!["boot".into(), "config".into(), c.drive.path.clone()];
                if f.timeout == 0 {
                    args.push("--no-timeout".into());
                } else {
                    args.extend(["--timeout".into(), f.timeout.to_string()]);
                }
                match &f.default {
                    Some(d) => args.extend(["--default".into(), d.clone()]),
                    None => args.push("--clear-default".into()),
                }
                args.extend(["--text-mode".into(), if f.text_mode { "on" } else { "off" }.into()]);
                if f.theme.trim().is_empty() {
                    args.push("--clear-theme".into());
                } else {
                    args.extend(["--theme".into(), f.theme.trim().to_string()]);
                }
                self.start(OpKind::SaveConfig, args, c.clone(), ctx);
            } else if !(cancel || close) {
                self.menu_form = Some(f.clone());
            }
        }
        if let Some(mut f) = self.params_form.take() {
            let f = &mut f;
            let (mut save, mut cancel) = (false, false);
            let bad = nyforge_core::nyboot::config::check_quoted("The boot options", &f.text).err().map(|e| e.message);
            let (_, close) = dialog(ctx, "boot-params", 500.0, |ui| {
                ui.label(heading("Boot options for this image"));
                ui.label(small_muted(f.path.clone()));
                ui.add_space(10.0);
                ui.add(egui::TextEdit::singleline(&mut f.text).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(8, 7)).hint_text(RichText::new("for example: nomodeset quiet").color(DIM)));
                if let Some(e) = &bad {
                    ui.label(RichText::new(e).size(12.0).color(DANGER));
                }
                ui.add_space(4.0);
                ui.label(small_muted("Added to the kernel's command line when this image starts. Leave it empty for none. Most images need nothing here."));
                dialog_buttons(ui, |ui| {
                    if Btn::primary("Save").enabled(bad.is_none()).show(ui).clicked() {
                        save = true;
                    }
                    if Btn::new("Cancel").show(ui).clicked() {
                        cancel = true;
                    }
                });
            });
            if save {
                let mut args: Vec<String> = vec!["boot".into(), "params".into(), c.drive.path.clone(), f.path.clone()];
                if f.text.trim().is_empty() {
                    args.push("--clear".into());
                } else {
                    args.push(f.text.trim().to_string());
                }
                self.start(OpKind::SaveParams, args, c.clone(), ctx);
            } else if !(cancel || close) {
                self.params_form = Some(f.clone());
            }
        }
    }
}

fn join(msg: &str, hint: &Option<String>) -> String {
    match hint {
        Some(h) => format!("{msg} {h}"),
        None => msg.to_string(),
    }
}

fn inspect_add(p: &std::path::Path) -> AddFile {
    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let name = file_name(p);
    let lower = name.to_ascii_lowercase();
    let compressed = !lower.ends_with(".iso");
    let note = if compressed {
        (Tone::Info, "Compressed: unpacked while copying, checked after".to_string())
    } else {
        match probe::analyze_file(p) {
            Ok(r) => match r.verdict {
                Verdict::Expected => (Tone::Success, match &r.distro {
                    Some(d) => format!("{d}: should boot"),
                    None => "Should boot".to_string(),
                }),
                Verdict::BestEffort => (Tone::Warn, "Might boot (best effort, not guaranteed)".to_string()),
                Verdict::WindowsInstaller => (Tone::Error, "Windows installer: will not boot from NyBoot".to_string()),
                Verdict::Unknown => (Tone::Warn, "Not recognised: may not boot".to_string()),
            },
            Err(e) => (Tone::Error, format!("Cannot read as an ISO: {}", e.message)),
        }
    };
    AddFile { path: p.to_path_buf(), name, size, compressed, note }
}

#[allow(dead_code)]
fn _unused(_: &Failure) {
    let _ = failure_from;
}
