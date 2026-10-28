//! NyFormat: erase a removable drive and give it a fresh filesystem.
//!
//! Only what the engine really does on this platform is offered. Filesystems it cannot make here are listed
//! greyed out with the engine's own reason.

use super::{permission_notice, run_panel, Screen, Shell};
use crate::drives::{self, Candidate, Purpose};
use crate::gate::{Gate, GateEvent};
use crate::icons::Icon;
use crate::run::{Outcome, Run};
use crate::selector::DriveSelector;
use crate::theme::*;
use crate::widgets::{self, banner, dialog, dialog_buttons, drive_card, heading, panel, status_mark, step_header, Btn, Tone};
use crate::worker;
use eframe::egui::{self, vec2, Id, RichText, Ui};
use nyforge_core::format::human_size;
use nyforge_core::usbformat::{self, fsmake, Fs};

#[derive(Clone, Copy, PartialEq, Eq)]
enum SchemeChoice {
    Auto,
    Mbr,
    Gpt,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WipeChoice {
    Quick,
    Zero,
    Random,
    Discard,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapChoice {
    Off,
    Quick,
    Full,
}

enum Stage {
    Setup,
    Running,
    Done,
}

const ALL_FS: [Fs; 7] = [Fs::Fat32, Fs::ExFat, Fs::Ntfs, Fs::Ext4, Fs::Ext3, Fs::Ext2, Fs::Fat16];

pub struct FormatScreen {
    sel: DriveSelector,
    stage: Stage,
    fs: Fs,
    fs_touched: bool,
    label: String,
    cluster: Option<u64>,
    scheme: SchemeChoice,
    wipe: WipeChoice,
    wipe_passes: u8,
    verify_wipe: bool,
    cap: CapChoice,
    fit_real: bool,
    fs_help: Option<(Fs, crate::fshelp::Reason)>,
    passes: u8,
    advanced: bool,
    /// The run being asked for (or running) is a Repair: look at the old layout, deep-clean, then format.
    repair: bool,
    /// Partition alignment in MiB (1, 4 or 8).
    align_mib: u64,
    /// MBR: mark the partition active (bootable).
    bootable: bool,
    /// Spare space at the end of the drive, in GB (text as typed).
    reserve_gb: String,
    /// Two partitions: the first one's size in GB (text), and what the second one is.
    second: bool,
    first_gb: String,
    second_fs: Fs,
    second_label: String,
    /// Read the partition table and filesystems back when done.
    verify_result: bool,
    gate: Option<Gate<()>>,
    notice: Option<String>,
    run: Option<Run>,
    target: Option<Candidate>,
    summary: String,
    outcome: Option<Outcome>,
    report: Option<serde_json::Value>,
    ask_stop: bool,
}

impl FormatScreen {
    pub fn new() -> Self {
        FormatScreen {
            sel: DriveSelector::new(Purpose::Format, "format-drives"),
            stage: Stage::Setup,
            fs: Fs::Fat32,
            fs_touched: false,
            label: String::new(),
            cluster: None,
            scheme: SchemeChoice::Auto,
            wipe: WipeChoice::Quick,
            wipe_passes: 1,
            verify_wipe: false,
            cap: CapChoice::Off,
            fit_real: false,
            fs_help: None,
            passes: 1,
            advanced: false,
            repair: false,
            align_mib: 1,
            bootable: false,
            reserve_gb: String::new(),
            second: false,
            first_gb: String::new(),
            second_fs: Fs::ExFat,
            second_label: String::new(),
            verify_result: true,
            gate: None,
            notice: None,
            run: None,
            target: None,
            summary: String::new(),
            outcome: None,
            report: None,
            ask_stop: false,
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Running) || self.gate.as_ref().map(|g| g.is_rechecking()).unwrap_or(false)
    }

    pub fn abort(&mut self) {
        if let Some(r) = &mut self.run {
            r.job.cancel();
        }
    }

    /// Why `fs` cannot be chosen for this drive (None = fine).
    fn fs_problem(fs: Fs, drive_bytes: u64) -> Option<String> {
        if let Err(e) = fsmake::check_tool(fs) {
            return Some(e.hint.clone().map(|h| format!("{} {}", e.message, h)).unwrap_or(e.message));
        }
        if let Some(max) = fs.max_bytes() {
            if drive_bytes > max {
                return Some(format!("{} is limited to {}; this drive is {}.", fs.name(), human_size(max), human_size(drive_bytes)));
            }
        }
        if drive_bytes < fs.min_bytes() {
            return Some(format!("The drive is too small for {} (needs {}).", fs.name(), human_size(fs.min_bytes())));
        }
        None
    }

    fn compat(fs: Fs) -> &'static str {
        match fs {
            Fs::Fat32 => "Works on Windows, macOS, Linux, game consoles, TVs and cameras. Cannot hold a single file larger than 4 GB.",
            Fs::ExFat => "Works on Windows, macOS and current Linux, with no 4 GB file limit. Some older TVs, cameras and consoles cannot read it.",
            Fs::Ntfs => "Native on Windows. macOS can read NTFS but not write to it without extra software. Not read by most TVs and cameras.",
            Fs::Ext4 => "For Linux. Windows and macOS cannot read ext4 without extra software.",
            Fs::Ext3 => "Older Linux format with a journal. Windows and macOS cannot read it without extra software.",
            Fs::Ext2 => "Oldest Linux format, no journal (gentle on flash memory). Windows and macOS cannot read it without extra software.",
            Fs::Fat16 => "Old and widely readable, but limited to 2 GB. Only useful for small drives or old devices.",
        }
    }

    fn cluster_options(fs: Fs) -> Vec<u64> {
        match fs {
            Fs::Fat32 | Fs::Fat16 | Fs::Ntfs => vec![512, 1024, 2048, 4096, 8192, 16384, 32768, 65536],
            Fs::ExFat => vec![4096, 8192, 16384, 32768, 65536, 131072, 262144, 524288, 1 << 20],
            Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => vec![1024, 2048, 4096],
        }
    }

    fn cluster_name(fs: Fs) -> &'static str {
        if fs.is_ext() {
            "Block size"
        } else {
            "Allocation unit size"
        }
    }

    fn cluster_text(c: u64) -> String {
        if c >= 1024 {
            format!("{} KB", c / 1024)
        } else {
            format!("{c} bytes")
        }
    }

    /// A size typed in GB ("2", "0.5") as bytes; `None` for empty text.
    fn gb(text: &str) -> Result<Option<u64>, String> {
        let t = text.trim();
        if t.is_empty() {
            return Ok(None);
        }
        let v: f64 = t.replace(',', ".").parse().map_err(|_| format!("\"{t}\" is not a size in GB."))?;
        if !v.is_finite() || v <= 0.0 {
            return Err("A size must be more than zero.".into());
        }
        Ok(Some((v * 1_000_000_000.0) as u64))
    }

    /// Everything that stops the Format button, in plain words.
    fn problem(&self, sh: &Shell, c: Option<&Candidate>) -> Option<String> {
        if let Some(w) = sh.why_not() {
            return Some(w);
        }
        let c = c?;
        if let Some(p) = Self::fs_problem(self.fs, c.drive.size) {
            return Some(p);
        }
        if let Err(e) = fsmake::validate_label(self.fs, self.label.trim()) {
            return Some(e.message);
        }
        if let Some(cl) = self.cluster {
            if let Err(e) = fsmake::validate_cluster(self.fs, cl).and_then(|_| fsmake::check_cluster_fits(self.fs, c.drive.size, cl)) {
                return Some(e.hint.map(|h| format!("{} {}", e.message, h)).unwrap_or(e.message));
            }
        }
        if cfg!(windows) && self.fs == Fs::Ntfs && self.scheme == SchemeChoice::None {
            return Some("On Windows an NTFS drive needs a partition table. Choose MBR, GPT or Automatic.".into());
        }
        if self.fit_real && self.cap == CapChoice::Full && self.scheme == SchemeChoice::Gpt {
            return Some("“Format only the part that works” cannot be used with GPT (its backup table has to sit at the real end of the drive). Choose MBR or Automatic.".into());
        }
        let reserve = match Self::gb(&self.reserve_gb) {
            Ok(r) => r,
            Err(e) => return Some(format!("Spare space: {e}")),
        };
        if let Some(r) = reserve {
            if r + 64 * 1024 * 1024 >= c.drive.size {
                return Some("The spare space is as big as the drive.".into());
            }
        }
        if self.second {
            if self.scheme == SchemeChoice::None {
                return Some("Two partitions need a partition table. Choose MBR, GPT or Automatic.".into());
            }
            match Self::gb(&self.first_gb) {
                Ok(Some(f)) if f + reserve.unwrap_or(0) + 80 * 1024 * 1024 < c.drive.size => {}
                Ok(Some(_)) => return Some("The first partition leaves no room for the second one.".into()),
                Ok(None) => return Some("Type the size of the first partition (in GB).".into()),
                Err(e) => return Some(format!("First partition: {e}")),
            }
            if let Some(p) = Self::fs_problem(self.second_fs, c.drive.size) {
                return Some(format!("Second partition: {p}"));
            }
            if let Err(e) = fsmake::validate_label(self.second_fs, self.second_label.trim()) {
                return Some(format!("Second partition: {}", e.message));
            }
            if self.label.contains(':') || self.second_label.contains(':') {
                return Some("With two partitions a volume label cannot contain a colon.".into());
            }
        }
        None
    }

    fn args(&self, c: &Candidate) -> Vec<String> {
        let mut a: Vec<String> = vec!["format".into(), c.drive.path.clone()];
        if self.second {
            // fs:size:label:cluster for each partition; the second takes the rest
            let first = Self::gb(&self.first_gb).ok().flatten().unwrap_or(0);
            let cl = self.cluster.map(|c| c.to_string()).unwrap_or_default();
            a.extend(["--partition".into(), format!("{}:{}M:{}:{}", fs_arg(self.fs), (first / (1 << 20)).max(1), self.label.trim(), cl)]);
            a.extend(["--partition".into(), format!("{}::{}", fs_arg(self.second_fs), self.second_label.trim())]);
        } else {
            a.extend(["--fs".into(), fs_arg(self.fs).into(), "--label".into(), self.label.trim().to_string()]);
        }
        a.push("--scheme".into());
        a.push(match self.scheme {
            SchemeChoice::Auto => "auto",
            SchemeChoice::Mbr => "mbr",
            SchemeChoice::Gpt => "gpt",
            SchemeChoice::None => "none",
        }
        .into());
        a.push("--wipe".into());
        a.push(match self.wipe {
            WipeChoice::Quick => "quick",
            WipeChoice::Zero => "zero",
            WipeChoice::Random => "random",
            WipeChoice::Discard => "discard",
        }
        .into());
        if matches!(self.wipe, WipeChoice::Zero | WipeChoice::Random) {
            if self.wipe_passes > 1 {
                a.extend(["--wipe-passes".into(), self.wipe_passes.to_string()]);
            }
            if self.verify_wipe {
                a.push("--verify-wipe".into());
            }
        }
        match self.cap {
            CapChoice::Off => {}
            CapChoice::Quick => a.extend(["--test-capacity".into(), "quick".into()]),
            CapChoice::Full => {
                a.extend(["--test-capacity".into(), "full".into()]);
                if self.passes > 1 {
                    a.extend(["--passes".into(), self.passes.to_string()]);
                }
                if self.fit_real {
                    a.push("--fit-real-capacity".into());
                }
            }
        }
        if let Some(cl) = self.cluster {
            if !self.second {
                a.extend(["--cluster-size".into(), cl.to_string()]);
            }
        }
        if self.align_mib > 1 {
            a.extend(["--align".into(), format!("{}M", self.align_mib)]);
        }
        if self.bootable && self.scheme != SchemeChoice::Gpt && self.scheme != SchemeChoice::None {
            a.push("--bootable".into());
        }
        if let Ok(Some(r)) = Self::gb(&self.reserve_gb) {
            a.extend(["--reserve".into(), format!("{}M", (r / (1 << 20)).max(1))]);
        }
        if !self.verify_result {
            a.push("--no-verify-result".into());
        }
        if self.repair {
            a.push("--repair".into());
        }
        if c.test_file {
            a.extend(["--size".into(), c.drive.size.to_string()]);
        }
        a.push("-y".into());
        a
    }

    pub fn ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        let ctx = ui.ctx().clone();
        if let Some(g) = &mut self.gate {
            match g.poll() {
                GateEvent::Waiting => {}
                GateEvent::Failed(e) => {
                    self.gate = None;
                    self.notice = Some(e);
                    self.sel.refresh(&ctx);
                }
                GateEvent::Start(c) => {
                    self.gate = None;
                    let args = self.args(&c);
                    self.summary = format!(
                        "{}{}{}",
                        self.fs.name(),
                        if self.label.trim().is_empty() { String::new() } else { format!(" · “{}”", self.label.trim()) },
                        if self.second { format!(" + {}", self.second_fs.name()) } else { String::new() }
                    );
                    self.run = Some(Run::new(worker::spawn(args, ctx.clone()), true));
                    self.target = Some(c);
                    self.stage = Stage::Running;
                    self.outcome = None;
                    self.report = None;
                    self.ask_stop = false;
                }
            }
        }
        if let Some(run) = &mut self.run {
            run.pump();
            if matches!(self.stage, Stage::Running) && run.is_done() {
                self.report = run.find_json("partitions").cloned();
                self.outcome = Some(run.outcome());
                self.stage = Stage::Done;
                if let Some(o) = &self.outcome {
                    crate::notify::job_done(&ctx, "NyFormat", if self.repair { "Repair complete" } else { "Format complete" }, "You can unplug the drive.", o);
                }
            }
        }
        if matches!(self.stage, Stage::Setup) {
            if self.sel.poll(&ctx, self.gate.is_none()) {
                self.gate = None;
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

    fn setup_ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        let ctx = ui.ctx().clone();
        permission_notice(ui, sh);
        if let Some(n) = self.notice.clone() {
            banner(ui, Tone::Error, "Drive check failed", &n, None);
        }
        widgets::page_title(ui, "NyFormat", "Erase a USB drive or SD card and give it a fresh filesystem.");
        if widgets::drive_slot(ui, self.sel.selected(), self.gate.is_none()) {
            self.sel.open_picker(&ctx);
        }
        ui.add_space(12.0);
        let sel = self.sel.selected().cloned();
        panel(ui, |ui| {
            step_header(ui, 2, "Options", false);
            ui.add_space(6.0);
            let size = sel.as_ref().map(|c| c.drive.size).unwrap_or(0);
            ui.label(small_muted("Filesystem"));
            ui.horizontal_wrapped(|ui| {
                for fs in ALL_FS {
                    let problem = if sel.is_some() { Self::fs_problem(fs, size) } else { fsmake::check_tool(fs).err().map(|e| e.message) };
                    let selected = self.fs == fs;
                    let mut b = Btn::new(fs.name()).kind(if selected { widgets::Kind::Primary } else { widgets::Kind::Secondary }).enabled(problem.is_none()).min_width(74.0);
                    if let Some(p) = &problem {
                        b = b.tip(p);
                    }
                    if b.show(ui).clicked() {
                        self.fs = fs;
                        self.fs_touched = true;
                        self.cluster = None;
                    }
                    if problem.is_some() {
                        let name = fs.name();
                        if widgets::info_button(ui, &format!("Why is {name} greyed out?")).clicked() {
                            let tool_problem = fsmake::check_tool(fs).is_err();
                            if let Some(r) = crate::fshelp::reason_for(fs, size, crate::fshelp::Os::current(), tool_problem) {
                                self.fs_help = Some((fs, r));
                            }
                        }
                    }
                }
            });
            ui.label(small_muted(Self::compat(self.fs)));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(small_muted("Volume label"));
                    let max = match self.fs {
                        Fs::Fat32 | Fs::Fat16 | Fs::ExFat => 11,
                        Fs::Ntfs => 32,
                        Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => 16,
                    };
                    ui.add(egui::TextEdit::singleline(&mut self.label).desired_width(200.0).margin(egui::Margin::symmetric(8, 7)).hint_text(RichText::new("optional").color(DIM)).char_limit(max));
                });
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.label(small_muted(Self::cluster_name(self.fs)));
                    let shown = self.cluster.map(Self::cluster_text).unwrap_or_else(|| "Default (recommended)".to_string());
                    egui::ComboBox::from_id_salt("cluster").selected_text(shown).width(200.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.cluster, None, "Default (recommended)");
                        for c in Self::cluster_options(self.fs) {
                            ui.selectable_value(&mut self.cluster, Some(c), Self::cluster_text(c));
                        }
                    });
                });
            });
            if let Err(e) = fsmake::validate_label(self.fs, self.label.trim()) {
                ui.label(RichText::new(e.message).size(12.0).color(DANGER));
            } else if matches!(self.fs, Fs::Fat32 | Fs::Fat16) && self.label != self.label.to_uppercase() {
                ui.label(small_muted("FAT labels are stored in capital letters."));
            }
            ui.add_space(8.0);
            let mut quick = self.wipe == WipeChoice::Quick;
            if ui.checkbox(&mut quick, "Quick format").on_hover_text("Clears the old partition tables and starts fresh in seconds. Untick to overwrite every byte with zeros (slow).").changed() {
                self.wipe = if quick { WipeChoice::Quick } else { WipeChoice::Zero };
            }
            ui.horizontal(|ui| {
                let mut bad = self.cap == CapChoice::Full;
                if ui.checkbox(&mut bad, "Check device for bad blocks").on_hover_text("Writes test data over the whole drive and reads it back. Finds worn-out flash and fake capacity. Slow.").changed() {
                    self.cap = if bad { CapChoice::Full } else { CapChoice::Off };
                }
                if bad {
                    ui.add_space(8.0);
                    egui::ComboBox::from_id_salt("passes").selected_text(if self.passes == 1 { "1 pass".to_string() } else { format!("{} passes", self.passes) }).width(110.0).show_ui(ui, |ui| {
                        for n in 1..=4u8 {
                            ui.selectable_value(&mut self.passes, n, if n == 1 { "1 pass".to_string() } else { format!("{n} passes") });
                        }
                    });
                }
            });
            ui.add_space(8.0);
            let repair_ok = sel.is_some() && self.gate.is_none() && self.problem(&*sh, sel.as_ref()).is_none();
            ui.horizontal_wrapped(|ui| {
                let tip = if sel.is_none() { "Select the drive first." } else if !repair_ok { "Fix the problem with the options above first." } else { "" };
                if Btn::new("Repair drive…").icon(Icon::Refresh).enabled(repair_ok).tip(tip).show(ui).clicked() {
                    if let Some(c) = &sel {
                        self.repair = true;
                        self.gate = Some(Gate::new(Purpose::Format, &c.id, (), &ctx));
                        self.notice = None;
                    }
                }
                ui.label(small_muted("Stuck at a few MB, shows up as a CD, will not format or says write-protected? Repair looks at what is wrong, cleans the drive thoroughly, then formats it with the options above."));
            });
            ui.add_space(6.0);
            let arrow = if self.advanced { "Hide advanced options" } else { "Advanced options" };
            if widgets::link_button(ui, arrow, true).clicked() {
                self.advanced = !self.advanced;
            }
            let t = anim(&ctx, Id::new("fmt-adv"), self.advanced, 0.2);
            if t > 0.01 {
                ui.set_opacity(t);
                self.advanced_ui(ui, size);
                ui.set_opacity(1.0);
            }
        });
        ui.add_space(14.0);
        let why = self.problem(sh, sel.as_ref());
        let blocker = if sel.is_none() { Some("Select the drive to format.".to_string()) } else if self.gate.is_some() { Some("Checking the drive…".to_string()) } else { why.clone() };
        if let (Some(_), Some(w)) = (&sel, &why) {
            if sh.why_not().is_none() {
                banner(ui, Tone::Error, "Cannot format with these options", w, None);
                ui.add_space(8.0);
            }
        }
        ui.vertical_centered(|ui| {
            let b = Btn::destructive(if self.gate.is_some() { "Checking drive…" } else { "Format!" }).big().enabled(blocker.is_none()).tip(blocker.as_deref().unwrap_or(""));
            if b.show(ui).clicked() {
                if let Some(c) = &sel {
                    self.repair = false;
                    self.gate = Some(Gate::new(Purpose::Format, &c.id, (), &ctx));
                    self.notice = None;
                }
            }
            ui.add_space(4.0);
            ui.label(small_muted(blocker.as_deref().filter(|b| !b.starts_with("Checking") && sel.is_none()).unwrap_or("You will be asked to confirm. Formatting erases the whole drive.")));
            ui.add_space(8.0);
        });
    }

    fn advanced_ui(&mut self, ui: &mut Ui, size: u64) {
        ui.add_space(4.0);
        egui::Grid::new("fmt-adv-grid").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            ui.label(small_muted("Partition scheme"));
            let name = |s: SchemeChoice| match s {
                SchemeChoice::Auto => "Automatic (recommended)",
                SchemeChoice::Mbr => "MBR (works everywhere)",
                SchemeChoice::Gpt => "GPT (drives over 2 TB, modern PCs)",
                SchemeChoice::None => "None (filesystem only, for cameras and TVs)",
            };
            egui::ComboBox::from_id_salt("scheme").selected_text(name(self.scheme)).width(300.0).show_ui(ui, |ui| {
                for s in [SchemeChoice::Auto, SchemeChoice::Mbr, SchemeChoice::Gpt, SchemeChoice::None] {
                    ui.selectable_value(&mut self.scheme, s, name(s));
                }
            });
            ui.end_row();
            ui.label(small_muted("Erase mode"));
            let wname = |w: WipeChoice| match w {
                WipeChoice::Quick => "Quick (clear the partition tables)",
                WipeChoice::Zero => "Full: overwrite every byte with zeros (slow)",
                WipeChoice::Random => "Full: overwrite every byte with random data (slow)",
                WipeChoice::Discard => "Discard (TRIM): tell the drive its blocks are unused (instant, where supported)",
            };
            egui::ComboBox::from_id_salt("wipe").selected_text(wname(self.wipe)).width(300.0).show_ui(ui, |ui| {
                for w in [WipeChoice::Quick, WipeChoice::Discard, WipeChoice::Zero, WipeChoice::Random] {
                    ui.selectable_value(&mut self.wipe, w, wname(w));
                }
            });
            ui.end_row();
            if matches!(self.wipe, WipeChoice::Zero | WipeChoice::Random) {
                ui.label(small_muted("Erase passes"));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("wipe-passes").selected_text(if self.wipe_passes == 1 { "1 pass".to_string() } else { format!("{} passes", self.wipe_passes) }).width(110.0).show_ui(ui, |ui| {
                        for n in [1u8, 2, 3, 5, 7] {
                            ui.selectable_value(&mut self.wipe_passes, n, if n == 1 { "1 pass".to_string() } else { format!("{n} passes") });
                        }
                    })
                    .response
                    .on_hover_text("Every pass but the last writes random data; the last writes what the erase mode says. More passes take proportionally longer. One pass is enough for a modern flash drive; more only matters for old magnetic disks.");
                    ui.checkbox(&mut self.verify_wipe, "Read it back").on_hover_text("Reads the whole drive afterwards and checks that the erase really stuck. Takes about as long as one pass.");
                });
                ui.end_row();
            }
            ui.label(small_muted("Capacity check"));
            let cname = |c: CapChoice| match c {
                CapChoice::Off => "Off",
                CapChoice::Quick => "Quick: sample the drive for fake capacity",
                CapChoice::Full => "Full: write and read back the whole drive, finds bad blocks (very slow)",
            };
            egui::ComboBox::from_id_salt("cap").selected_text(cname(self.cap)).width(300.0).show_ui(ui, |ui| {
                for c in [CapChoice::Off, CapChoice::Quick, CapChoice::Full] {
                    ui.selectable_value(&mut self.cap, c, cname(c));
                }
            });
            ui.end_row();
            ui.label(small_muted("Partition alignment"));
            let aname = |m: u64| match m {
                1 => "1 MiB (default)".to_string(),
                4 => "4 MiB (recommended for SD cards)".to_string(),
                m => format!("{m} MiB"),
            };
            egui::ComboBox::from_id_salt("align").selected_text(aname(self.align_mib)).width(300.0).show_ui(ui, |ui| {
                for m in [1u64, 4, 8] {
                    ui.selectable_value(&mut self.align_mib, m, aname(m));
                }
            })
            .response
            .on_hover_text("Partitions start on multiples of this size. SD cards erase in big blocks (often 4 MiB), so aligned partitions write faster and wear less. Costs at most a few MB.");
            ui.end_row();
            ui.label(small_muted("Spare space at the end"));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.reserve_gb).desired_width(80.0).hint_text(RichText::new("none").color(DIM)).margin(egui::Margin::symmetric(8, 6)))
                    .on_hover_text("Leave this much of the drive unpartitioned. Flash drives and SD cards use free space to spread wear, so a bit of spare space makes them last longer.");
                ui.label(small_muted("GB"));
            });
            ui.end_row();
        });
        let can_boot = self.scheme != SchemeChoice::Gpt && self.scheme != SchemeChoice::None;
        ui.add_enabled_ui(can_boot, |ui| {
            ui.checkbox(&mut self.bootable, "Make the partition bootable (active flag)")
                .on_hover_text("MBR only. Marks the first partition active, which some old BIOS computers need before they will start a drive's own boot code. Not needed for storage.");
        });
        ui.checkbox(&mut self.verify_result, "Check the result when done")
            .on_hover_text("Reads the partition table and each filesystem's signature back from the drive, and tells you if the drive did not keep what was written. Takes a second.");
        ui.add_space(4.0);
        ui.checkbox(&mut self.second, "Split into two partitions").on_hover_text("For example a Windows-readable partition plus a Linux (ext4) one, or a small FAT32 partition for a bootloader and a big data partition.");
        if self.second {
            if self.first_gb.is_empty() && size > 0 {
                self.first_gb = format!("{:.0}", (size as f64 / 2.0 / 1e9).max(1.0));
            }
            egui::Grid::new("fmt-second-grid").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label(small_muted("First partition (the filesystem above)"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.first_gb).desired_width(80.0).margin(egui::Margin::symmetric(8, 6)));
                    ui.label(small_muted("GB"));
                });
                ui.end_row();
                ui.label(small_muted("Second partition (the rest)"));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("second-fs").selected_text(self.second_fs.name()).width(110.0).show_ui(ui, |ui| {
                        for fs in ALL_FS {
                            if Self::fs_problem(fs, size).is_none() {
                                ui.selectable_value(&mut self.second_fs, fs, fs.name());
                            }
                        }
                    });
                    let max = match self.second_fs {
                        Fs::Fat32 | Fs::Fat16 | Fs::ExFat => 11,
                        Fs::Ntfs => 32,
                        Fs::Ext2 | Fs::Ext3 | Fs::Ext4 => 16,
                    };
                    ui.add(egui::TextEdit::singleline(&mut self.second_label).desired_width(110.0).hint_text(RichText::new("label").color(DIM)).char_limit(max).margin(egui::Margin::symmetric(8, 6)));
                });
                ui.end_row();
            });
        }
        if self.cap == CapChoice::Full {
            ui.checkbox(&mut self.fit_real, "If the drive turns out to be fake, format only the part that works");
        }
    }

    fn running_ui(&mut self, ui: &mut Ui) {
        widgets::run_header(ui, Icon::Format, if self.repair { "Repairing" } else { "Formatting" });
        let t = self.target.clone();
        let rows = vec![("Drive", t.map(|c| format!("{} · {} · {}", c.name, c.size_text(), c.drive.path)).unwrap_or_default()), ("Result", self.summary.clone())];
        let stopping = self.run.as_ref().map(|r| r.job.cancel_requested).unwrap_or(false);
        if let Some(run) = &self.run {
            run_panel(ui, Id::new("format-run"), run, stopping, &rows);
        }
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            if Btn::new(if stopping { "Stopping…" } else { "Cancel" }).enabled(!stopping).show(ui).clicked() {
                self.ask_stop = true;
            }
        });
    }

    fn done_ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        let outcome = self.outcome.clone().unwrap_or(Outcome::Running);
        let tgt = self.target.as_ref().map(|c| format!("{} ({}, {})", c.name, c.size_text(), c.drive.path)).unwrap_or_default();
        let mut again = false;
        ui.vertical_centered(|ui| {
            ui.add_space(18.0);
            match &outcome {
                Outcome::Success => {
                    status_mark(ui, Id::new("fmt-mark"), Tone::Success, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text(if self.repair { "Repair complete!" } else { "Format complete!" }, 28.0));
                    ui.label(muted(format!("{tgt} was {}.", if self.repair { "repaired and formatted" } else { "formatted" })));
                    if let Some(rep) = &self.report {
                        ui.add_space(8.0);
                        ui.allocate_ui(vec2(480.0, 10.0), |ui| report_ui(ui, rep));
                    }
                }
                Outcome::Cancelled => {
                    status_mark(ui, Id::new("fmt-mark"), Tone::Warn, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text("Format cancelled", 22.0));
                    ui.label(muted(format!("Formatting {tgt} was stopped.")));
                    ui.add_space(6.0);
                    ui.allocate_ui(vec2(460.0, 10.0), |ui| {
                        banner(ui, Tone::Warn, "The drive may be half erased", "It may not mount until it is formatted again.", None);
                    });
                }
                Outcome::Failed(f) => {
                    status_mark(ui, Id::new("fmt-mark"), Tone::Error, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text(if self.repair { "Repair failed" } else { "Format failed" }, 22.0));
                    ui.label(muted(format!("{tgt} was not formatted.")));
                    ui.add_space(6.0);
                    ui.allocate_ui(vec2(520.0, 10.0), |ui| {
                        banner(ui, Tone::Error, &f.title, &f.message, f.hint.as_deref());
                    });
                    ui.label(small_muted("The drive may be partly erased. Try again once the problem is fixed."));
                }
                Outcome::Running => {}
            }
            ui.add_space(14.0);
            let main = if matches!(outcome, Outcome::Success) { "Format another" } else { "Try again" };
            let (p, h) = widgets::centered_pair(ui, Btn::primary(main).icon(Icon::Refresh).big(), Btn::new("Home").icon(Icon::Home));
            again = p;
            if h {
                sh.go = Some(Screen::Home);
            }
        });
        if again {
            let ctx = ui.ctx().clone();
            self.stage = Stage::Setup;
            self.repair = false;
            self.run = None;
            self.target = None;
            self.outcome = None;
            self.sel.deselect();
            self.sel.refresh(&ctx);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if matches!(self.stage, Stage::Setup) && self.sel.modal(ctx) {
            self.notice = None;
            self.cluster = None;
            if !self.fs_touched {
                if let Some(c) = self.sel.selected() {
                    self.fs = usbformat::auto_fs(c.drive.size);
                }
            }
        }
        if let Some((fs, reason)) = self.fs_help {
            let size = self.sel.selected().map(|c| c.drive.size).unwrap_or(0);
            let h = crate::fshelp::help(fs, reason, size, crate::fshelp::Os::current());
            let (_, close) = dialog(ctx, "fs-help", 520.0, |ui| {
                ui.label(heading(&h.title));
                ui.add_space(8.0);
                ui.label(RichText::new(&h.why).color(TEXT));
                if !h.commands.is_empty() {
                    ui.add_space(8.0);
                    ui.label(small_muted("Install it with:"));
                    for c in &h.commands {
                        ui.label(RichText::new(c).monospace().size(12.5).color(TEXT));
                    }
                }
                if !h.systems.is_empty() {
                    ui.add_space(10.0);
                    ui.label(small_muted(format!("{} on each system", fs.name())));
                    for (os, text) in &h.systems {
                        let this = if *os == crate::fshelp::Os::current() { "  (this computer)" } else { "" };
                        ui.label(RichText::new(format!("{}{}: {}", os.name(), this, text)).size(13.0).color(if this.is_empty() { MUTED } else { TEXT }));
                    }
                }
                if !h.after.is_empty() {
                    ui.add_space(10.0);
                    ui.label(muted(&h.after));
                }
                dialog_buttons(ui, |ui| {
                    if Btn::new("Close").show(ui).clicked() {
                        self.fs_help = None;
                    }
                });
            });
            if close {
                self.fs_help = None;
            }
        }
        let (mut accept, mut cancel) = (false, false);
        if let Some(g) = &self.gate {
            if let Some(c) = g.candidate() {
                let rechecking = g.is_rechecking();
                let what = format!("{}{}", self.fs.name(), if self.label.trim().is_empty() { String::new() } else { format!(", named “{}”", self.label.trim()) });
                let wipe = match self.wipe {
                    WipeChoice::Quick => "",
                    WipeChoice::Zero => " Every byte will be overwritten with zeros, which can take a long time.",
                    WipeChoice::Random => " Every byte will be overwritten with random data, which can take a long time.",
                    WipeChoice::Discard => " The drive will be told that all its blocks are unused.",
                };
                let repair = self.repair;
                let (_, close) = dialog(ctx, "format-confirm", 460.0, |ui| {
                    ui.label(heading(if repair { "Repair and format this drive?" } else { "Erase and format this drive?" }));
                    ui.add_space(8.0);
                    drive_card(ui, c);
                    ui.add_space(6.0);
                    widgets::kv(ui, "New filesystem", RichText::new(what));
                    if self.second {
                        widgets::kv(ui, "Second partition", RichText::new(format!("{}{}, the rest of the drive", self.second_fs.name(), if self.second_label.trim().is_empty() { String::new() } else { format!(", named “{}”", self.second_label.trim()) })));
                    }
                    if repair {
                        ui.add_space(4.0);
                        ui.label(small_muted("Repair first reads the old layout and says what is wrong, clears the write-protect flag where the system allows it, and wipes leftover partition tables, labels and ISO data from both ends of the drive and from every old partition. Then it formats."));
                    }
                    ui.add_space(6.0);
                    banner(ui, Tone::Error, "Everything on this drive will be permanently erased.", &format!("This cannot be undone. Check that the name and size above are the drive you mean.{wipe}"), None);
                    for w in &c.warnings {
                        ui.add_space(4.0);
                        banner(ui, Tone::Warn, "", w, None);
                    }
                    dialog_buttons(ui, |ui| {
                        if Btn::destructive(if rechecking { "Checking…" } else if repair { "Repair and format" } else { "Erase and format" }).enabled(!rechecking).show(ui).clicked() {
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
        if cancel {
            self.gate = None;
        }
        if accept {
            if let Some(g) = &mut self.gate {
                g.accept(ctx);
            }
        }
        if self.ask_stop && matches!(self.stage, Stage::Running) {
            let (mut stop, mut keep) = (false, false);
            let (_, close) = dialog(ctx, "format-stop", 420.0, |ui| {
                ui.label(heading("Stop formatting?"));
                ui.add_space(6.0);
                ui.label(muted("The drive may be left partly erased and may not mount until it is formatted again."));
                dialog_buttons(ui, |ui| {
                    if Btn::destructive("Stop formatting").show(ui).clicked() {
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
        let _ = drives::allow_virtual;
    }
}

fn fs_arg(fs: Fs) -> &'static str {
    match fs {
        Fs::Fat16 => "fat16",
        Fs::Fat32 => "fat32",
        Fs::ExFat => "exfat",
        Fs::Ntfs => "ntfs",
        Fs::Ext2 => "ext2",
        Fs::Ext3 => "ext3",
        Fs::Ext4 => "ext4",
    }
}

/// The engine's own report: partition table, partitions, capacity check, notes.
fn report_ui(ui: &mut Ui, rep: &serde_json::Value) {
    panel(ui, |ui| {
        let scheme = rep["scheme"].as_str().unwrap_or("");
        let scheme = match scheme {
            "mbr" => "MBR",
            "gpt" => "GPT",
            "none" => "none (filesystem only)",
            o => o,
        };
        widgets::kv(ui, "Partition table", RichText::new(scheme.to_string()));
        if let Some(u) = rep["usable_bytes"].as_u64() {
            widgets::kv(ui, "Usable space", RichText::new(human_size(u)));
        }
        if let Some(parts) = rep["partitions"].as_array() {
            for p in parts {
                let fs = match p["fs"].as_str().unwrap_or("") {
                    "fat16" => "FAT16",
                    "fat32" => "FAT32",
                    "exfat" => "exFAT",
                    "ntfs" => "NTFS",
                    "ext4" => "ext4",
                    o => o,
                };
                let label = p["label"].as_str().unwrap_or("");
                let name = if label.is_empty() { "no label".to_string() } else { format!("“{label}”") };
                widgets::kv(ui, &format!("Partition {}", p["number"].as_u64().unwrap_or(0)), RichText::new(format!("{fs} · {} · {name}", human_size(p["size_bytes"].as_u64().unwrap_or(0)))));
            }
        }
        if let Some(c) = rep["capacity"].as_object() {
            let genuine = c.get("genuine").and_then(|g| g.as_bool()).unwrap_or(true);
            if genuine {
                widgets::kv(ui, "Capacity check", RichText::new("Passed: the drive holds what it claims").color(OK_HI));
            } else {
                let est = c.get("estimated_real").and_then(|v| v.as_u64()).unwrap_or(0);
                widgets::kv(ui, "Capacity check", RichText::new(format!("Failed: the drive appears to hold only about {}", human_size(est))).color(DANGER));
            }
        }
        if let Some(s) = rep["elapsed_secs"].as_f64() {
            if s > 0.0 {
                widgets::kv(ui, "Took", RichText::new(super::fmt_duration(s)));
            }
        }
        if let Some(notes) = rep["notes"].as_array() {
            for n in notes.iter().filter_map(|n| n.as_str()) {
                ui.label(small_muted(n));
            }
        }
    });
}
