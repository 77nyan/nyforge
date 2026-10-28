//! NyTest: how fast is this drive, is it healthy, and is it what it claims to be?
//!
//! The window builds an `nyforge test …` command, runs it through the engine and draws what comes back: a speed table in the style of
//! CrystalDiskMark (plus IOPS and latency), a sustained-write graph, a surface map, a capacity check, and a verdict in plain words.
//! Basic: pick a drive and how thorough to be. Advanced: choose every test and its settings.

use super::{fmt_duration, permission_notice, pick_save, Screen, Shell};
use crate::charts;
use crate::drives::{self, Candidate, Purpose};
use crate::gate::{Gate, GateEvent};
use crate::icons::{self, Icon};
use crate::run::{Outcome, Run};
use crate::selector::DriveSelector;
use crate::theme::*;
use crate::widgets::{self, banner, choice_cards, dialog, dialog_buttons, drive_card, drive_slot_ex, heading, kv, panel, progress_bar, status_mark, step_header, Btn, Tone};
use crate::worker;
use eframe::egui::{self, vec2, Color32, Id, RichText, Sense, Ui};
use nyforge_core::device::Safety;
use nyforge_core::format::human_size;
use nyforge_core::nytest::engine::Op;
use nyforge_core::nytest::info::DeviceInfo;
use nyforge_core::nytest::speed::SpeedResult;
use nyforge_core::nytest::{self, Options, Preset, Report, Severity, Step, TestEvent};
use nyforge_core::usbformat::capacity::CapacityTest;
use std::path::PathBuf;

enum Stage {
    Setup,
    Running,
    Done,
}

const STEPS: [Step; 5] = [Step::Info, Step::Speed, Step::Sustained, Step::Scan, Step::Integrity];
const PROFILES: [(&str, &str); 5] = [
    ("SEQ1M Q8T1", "Big files, 1 MiB at a time, 8 requests in flight. The headline number most drive makers print."),
    ("SEQ1M Q1T1", "Big files with one request at a time: what a simple copy does."),
    ("SEQ128K Q32T1", "Streaming 128 KiB blocks, 32 in flight. Mostly useful for fast SSDs."),
    ("RND4K Q32T1", "Tiny random reads and writes with many in flight. Shows the drive's best small-file rate."),
    ("RND4K Q1T1", "Tiny random reads and writes one by one: what an operating system or a program running from the drive feels like."),
];
const SIZES: [u64; 6] = [64 << 20, 256 << 20, 1 << 30, 4 << 30, 8 << 30, 16 << 30];
const SUSTAINED: [u64; 6] = [1 << 30, 2 << 30, 4 << 30, 8 << 30, 16 << 30, 32 << 30];
const BLOCKS: [u64; 4] = [256 << 10, 1 << 20, 4 << 20, 16 << 20];

fn size_name(v: u64) -> String {
    if v >= 1 << 30 {
        format!("{} GB", v >> 30)
    } else {
        format!("{} MB", v >> 20)
    }
}

/// What the running test has shown so far.
#[derive(Default)]
struct Live {
    step: String,
    label: String,
    index: usize,
    of: usize,
    fraction: f64,
    mbps: f64,
    iops: f64,
    mode: String,
    speed: Vec<SpeedResult>,
    samples: Vec<(f64, f64)>,
    map: Vec<u8>,
    map_mbps: Vec<f32>,
    map_of: usize,
    info: Option<DeviceInfo>,
    notes: Vec<String>,
    seen: usize,
}

pub struct TestScreen {
    stage: Stage,
    sel: DriveSelector,
    preset: usize,
    erase: bool,
    advanced: bool,
    steps: [bool; 5],
    size: u64,
    runs: u32,
    seconds: f64,
    interval: f64,
    profiles: [bool; 5],
    zeros: bool,
    write_through: bool,
    sus_size: u64,
    sus_secs: f64,
    scan_full: bool,
    scan_block: u64,
    stop_on_error: bool,
    cap_full: bool,
    no_direct: bool,
    folder: Option<PathBuf>,
    gate: Option<Gate<()>>,
    notice: Option<String>,
    run: Option<Run>,
    target: Option<Candidate>,
    live: Live,
    outcome: Option<Outcome>,
    report: Option<Report>,
    ask_stop: bool,
    copied: Option<std::time::Instant>,
    saved: Option<String>,
}

fn preset_of(i: usize) -> Preset {
    match i {
        0 => Preset::Quick,
        1 => Preset::Standard,
        _ => Preset::Full,
    }
}

impl TestScreen {
    pub fn new() -> Self {
        let mut s = TestScreen {
            stage: Stage::Setup,
            sel: DriveSelector::new(Purpose::Test, "test-drive"),
            preset: 1,
            erase: false,
            advanced: false,
            steps: [true; 5],
            size: 1 << 30,
            runs: 3,
            seconds: 5.0,
            interval: 1.0,
            profiles: [true, true, false, true, true],
            zeros: false,
            write_through: false,
            sus_size: 4 << 30,
            sus_secs: 300.0,
            scan_full: false,
            scan_block: 1 << 20,
            stop_on_error: false,
            cap_full: false,
            no_direct: false,
            folder: None,
            gate: None,
            notice: None,
            run: None,
            target: None,
            live: Live::default(),
            outcome: None,
            report: None,
            ask_stop: false,
            copied: None,
            saved: None,
        };
        s.apply_preset();
        s
    }

    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Running) || self.gate.as_ref().map(|g| g.is_rechecking()).unwrap_or(false)
    }

    pub fn abort(&mut self) {
        if let Some(r) = &mut self.run {
            r.job.cancel();
        }
    }

    /// Load the settings of the chosen thoroughness into the advanced controls.
    fn apply_preset(&mut self) {
        let o = Options::preset(preset_of(self.preset));
        for (i, st) in STEPS.iter().enumerate() {
            self.steps[i] = o.steps.contains(st);
        }
        self.size = o.speed.size;
        self.runs = o.speed.runs;
        self.seconds = o.speed.seconds;
        self.interval = o.speed.interval;
        self.sus_size = o.sustained.size;
        self.sus_secs = o.sustained.max_seconds;
        self.scan_full = !o.scan.quick;
        self.cap_full = o.integrity == CapacityTest::Full;
        self.profiles = [true, true, false, true, true];
        if self.preset == 2 {
            self.erase = self.sel.selected().map(can_erase).unwrap_or(true);
        } else if self.preset == 0 {
            self.erase = false;
        }
    }

    /// Steps that will really run.
    fn active_steps(&self) -> Vec<Step> {
        STEPS.iter().enumerate().filter(|(i, st)| self.steps[*i] && (**st != Step::Integrity || self.erase)).map(|(_, s)| *s).collect()
    }

    fn needs_admin(&self) -> bool {
        self.erase || self.active_steps().iter().any(|s| matches!(s, Step::Scan | Step::Integrity))
    }

    fn folder_for(&self, c: &Candidate) -> Option<PathBuf> {
        self.folder.clone().or_else(|| auto_folder(c))
    }

    fn problem(&self, sh: &Shell, c: Option<&Candidate>) -> Option<String> {
        if self.needs_admin() {
            if let Some(w) = sh.why_not() {
                return Some(w);
            }
        } else if !sh.engine_ok {
            return sh.why_not();
        }
        let c = c?;
        let steps = self.active_steps();
        if steps.is_empty() {
            return Some("Choose at least one test in the advanced options.".into());
        }
        if self.erase && !can_erase(c) {
            return Some("This drive cannot be erased for testing. Only removable drives can.".into());
        }
        if steps.contains(&Step::Speed) && !self.profiles.iter().any(|p| *p) {
            return Some("Choose at least one speed measurement in the advanced options.".into());
        }
        if c.drive.size == 0 {
            return Some("No media in the drive.".into());
        }
        None
    }

    fn args(&self, c: &Candidate) -> Vec<String> {
        let mut a: Vec<String> = vec!["test".into(), c.drive.path.clone()];
        let steps = self.active_steps();
        a.extend(["--tests".into(), steps.iter().map(|s| s.id()).collect::<Vec<_>>().join(",")]);
        if let Some(f) = self.folder_for(c) {
            if !self.erase {
                a.extend(["--folder".into(), f.display().to_string()]);
            }
        }
        if self.erase {
            a.extend(["--erase".into(), "--yes".into()]);
        }
        a.extend(drives::engine_flags(c));
        a.extend(["--size".into(), self.size.to_string(), "--runs".into(), self.runs.to_string(), "--seconds".into(), format!("{}", self.seconds), "--interval".into(), format!("{}", self.interval)]);
        let names: Vec<&str> = PROFILES.iter().enumerate().filter(|(i, _)| self.profiles[*i]).map(|(_, p)| p.0).collect();
        if !names.is_empty() {
            a.extend(["--profiles".into(), names.join(",")]);
        }
        if self.zeros {
            a.push("--zeros".into());
        }
        if self.write_through {
            a.push("--write-through".into());
        }
        a.extend(["--sustained-size".into(), self.sus_size.to_string(), "--sustained-seconds".into(), format!("{}", self.sus_secs)]);
        if self.scan_full {
            a.push("--scan-full".into());
        }
        a.extend(["--scan-block".into(), self.scan_block.to_string()]);
        if self.stop_on_error {
            a.push("--stop-on-error".into());
        }
        a.extend(["--integrity".into(), if self.cap_full { "full" } else { "quick" }.into()]);
        if self.no_direct {
            a.push("--no-direct".into());
        }
        a
    }

    /// A rough idea of how long the chosen tests take, as a range in words.
    fn estimate(&self, c: Option<&Candidate>) -> String {
        let size = c.map(|c| c.drive.size).unwrap_or(32 << 30) as f64;
        let mut hi = 0.0;
        for st in self.active_steps() {
            hi += match st {
                Step::Info => 3.0,
                Step::Speed => {
                    let n = self.profiles.iter().filter(|p| **p).count() as f64;
                    n * 2.0 * self.runs as f64 * (self.seconds + self.interval) + 15.0 + self.size as f64 / 60e6
                }
                Step::Sustained => self.sus_secs.min(self.sus_size as f64 / 8e6) + 5.0,
                Step::Scan => {
                    if self.scan_full {
                        size / 40e6
                    } else {
                        25.0
                    }
                }
                Step::Integrity => {
                    if self.cap_full {
                        size * 2.0 / 25e6
                    } else {
                        45.0
                    }
                }
            };
        }
        let lo = hi * 0.45;
        let fmt = |s: f64| -> String {
            if s < 60.0 {
                format!("{:.0} seconds", (s / 5.0).ceil() * 5.0)
            } else if s < 5400.0 {
                format!("{:.0} minutes", (s / 60.0).ceil())
            } else {
                format!("{:.1} hours", s / 3600.0)
            }
        };
        if hi < 40.0 {
            "under a minute".to_string()
        } else if hi < 5400.0 {
            format!("{}–{}", fmt(lo).trim_end_matches(" minutes").trim_end_matches(" seconds"), fmt(hi))
        } else {
            format!("{} to {}", fmt(lo), fmt(hi))
        }
    }

    // ------------------------------------------------------------------ frame

    pub fn ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        let ctx = ui.ctx().clone();
        let erasing = self.erase;
        if let Some(g) = &mut self.gate {
            match g.poll() {
                GateEvent::Waiting => {
                    if !erasing && !g.is_rechecking() && g.candidate().is_some() {
                        g.accept(&ctx); // nothing is erased: no need to ask
                    }
                }
                GateEvent::Failed(e) => {
                    self.gate = None;
                    self.notice = Some(e);
                    self.sel.refresh(&ctx);
                }
                GateEvent::Start(c) => {
                    self.gate = None;
                    let args = self.args(&c);
                    let job = if self.needs_admin() { worker::spawn(args, ctx.clone()) } else { worker::spawn_as_user(args, ctx.clone()) };
                    self.run = Some(Run::new(job, false));
                    self.target = Some(c);
                    self.live = Live::default();
                    self.stage = Stage::Running;
                    self.outcome = None;
                    self.report = None;
                    self.ask_stop = false;
                    self.saved = None;
                }
            }
        }
        if self.run.is_some() {
            self.fold_events();
            if let Some(run) = &mut self.run {
                run.pump();
                if matches!(self.stage, Stage::Running) && run.is_done() {
                    self.fold_events();
                    let run = self.run.as_ref().unwrap();
                    // a verdict of "bad" exits with 2 by design: that is a finished test, not a failed one
                    self.outcome = Some(if self.report.is_some() { Outcome::Success } else { run.outcome() });
                    self.stage = Stage::Done;
                    if let Some(o) = &self.outcome {
                        crate::notify::job_done(&ctx, "NyTest", "Test finished", "The results are ready.", o);
                    }
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

    /// Fold the engine's JSON lines into what the screen shows.
    fn fold_events(&mut self) {
        let Some(run) = &mut self.run else { return };
        run.pump();
        while self.live.seen < run.json.len() {
            let v = run.json[self.live.seen].clone();
            self.live.seen += 1;
            let Ok(ev) = serde_json::from_value::<TestEvent>(v) else { continue };
            let l = &mut self.live;
            match ev {
                TestEvent::NytestStart { mode, .. } => l.mode = mode,
                TestEvent::NytestStep { step, label, index, of } => {
                    l.step = step;
                    l.label = label;
                    l.index = index;
                    l.of = of;
                    l.fraction = 0.0;
                    l.mbps = 0.0;
                    l.iops = 0.0;
                }
                TestEvent::NytestLive { step, label, fraction, mbps, iops } => {
                    l.step = step;
                    l.label = label;
                    l.fraction = fraction;
                    l.mbps = mbps;
                    l.iops = iops;
                }
                TestEvent::NytestInfo { info } => l.info = Some(*info),
                TestEvent::NytestSpeed { result } => l.speed.push(result),
                TestEvent::NytestSample { t, mbps, .. } => l.samples.push((t, mbps)),
                TestEvent::NytestMap { index, of, state, mbps } => {
                    l.map_of = of;
                    if l.map.len() <= index {
                        l.map.resize(index + 1, 255);
                        l.map_mbps.resize(index + 1, 0.0);
                    }
                    l.map[index] = state;
                    l.map_mbps[index] = mbps as f32;
                }
                TestEvent::NytestNote { message } => l.notes.push(message),
                TestEvent::NytestDone { report } => self.report = Some(*report),
            }
        }
    }

    // ------------------------------------------------------------------ setup

    fn setup_ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        sh.anchor_top = true;
        let ctx = ui.ctx().clone();
        if self.needs_admin() {
            permission_notice(ui, sh);
        }
        if let Some(n) = self.notice.clone() {
            banner(ui, Tone::Error, "Drive check failed", &n, None);
        }
        widgets::page_title(ui, "NyTest", "How fast is it, is it healthy, and is it really the size it says?");
        let locked = self.gate.is_some();
        if drive_slot_ex(ui, self.sel.selected(), !locked, "No drive selected", "USB sticks and SD cards. Internal disks are never offered.") {
            self.sel.open_picker(&ctx);
        }
        let sel = self.sel.selected().cloned();
        ui.add_space(12.0);
        let items = [
            (Icon::Flash, "Quick", "Speed only. A temporary file, nothing on the drive is changed."),
            (Icon::Test, "Standard", "Speed, a long write that shows where it slows down, and a read check."),
            (Icon::Shield, "Full", "Everything, including reading the whole drive and proving its capacity. Erases the drive."),
        ];
        let mut p = self.preset;
        if choice_cards(ui, Id::new("test-presets"), &mut p, &items, !locked) {
            self.preset = p;
            self.apply_preset();
        }
        ui.add_space(12.0);
        panel(ui, |ui| {
            step_header(ui, 1, "Options", false);
            ui.add_space(6.0);
            let can = sel.as_ref().map(can_erase).unwrap_or(true);
            ui.add_enabled_ui(can, |ui| {
                let was = self.erase;
                ui.checkbox(&mut self.erase, "Allow erasing the drive for the deepest tests").on_hover_text("Speed tests then go straight to the drive, with no file system in the way (the purest numbers), and the capacity check can find drives that claim more than they hold. Everything on the drive is lost.");
                if self.erase != was && self.preset == 2 && !self.erase {
                    self.steps[4] = false;
                }
                if self.erase != was && self.erase {
                    self.steps[4] = self.steps[4] || self.preset == 2;
                }
            });
            if !can && sel.is_some() {
                ui.label(small_muted("This drive cannot be erased for testing, so the tests that need that are off."));
            }
            if self.erase {
                ui.add_space(4.0);
                banner(ui, Tone::Warn, "Everything on the drive will be erased", "You will be asked to confirm. Copy anything you want to keep first.", None);
            } else if let Some(c) = &sel {
                ui.add_space(4.0);
                match self.folder_for(c) {
                    Some(f) => {
                        ui.label(small_muted(format!("Speed tests use a temporary file in {} and remove it afterwards. Nothing on the drive is changed.", f.display())));
                    }
                    None => {
                        ui.label(small_muted("This drive is not mounted, so the speed tests only read from it. Mount it (open it in your file manager), or allow erasing, to test writing too."));
                    }
                }
            }
            ui.add_space(6.0);
            ui.label(small_muted(format!("Tests: {}. About {}.", self.active_steps().iter().map(|s| s.label().to_lowercase()).collect::<Vec<_>>().join(", "), self.estimate(sel.as_ref()))));
            ui.add_space(6.0);
            if widgets::link_button(ui, if self.advanced { "Hide advanced options" } else { "Advanced options" }, true).clicked() {
                self.advanced = !self.advanced;
            }
            let t = anim(&ctx, Id::new("test-adv"), self.advanced, 0.2);
            if t > 0.01 {
                ui.set_opacity(t);
                ui.add_space(4.0);
                self.advanced_ui(ui, sel.as_ref());
                ui.set_opacity(1.0);
            }
        });
        ui.add_space(14.0);
        let why = self.problem(sh, sel.as_ref());
        let blocker = if sel.is_none() { Some("Choose the drive to test.".to_string()) } else if self.gate.is_some() { Some("Checking the drive…".to_string()) } else { why.clone() };
        if let (Some(_), Some(w)) = (&sel, &why) {
            if sh.why_not().is_none() {
                banner(ui, Tone::Error, "Cannot start with these options", w, None);
                ui.add_space(8.0);
            }
        }
        ui.vertical_centered(|ui| {
            let label = if self.gate.is_some() { "Checking drive…" } else { "Start test!" };
            let b = (if self.erase { Btn::destructive(label) } else { Btn::primary(label) }).big().enabled(blocker.is_none()).tip(blocker.as_deref().unwrap_or(""));
            if b.show(ui).clicked() {
                if let Some(c) = &sel {
                    self.gate = Some(Gate::new(Purpose::Test, &c.id, (), &ctx));
                    self.notice = None;
                }
            }
            ui.add_space(4.0);
            ui.label(small_muted(if sel.is_none() { "Choose the drive to test." } else if self.erase { "You will be asked to confirm. The drive is erased." } else { "Nothing on the drive is changed." }));
            ui.add_space(8.0);
        });
    }

    fn advanced_ui(&mut self, ui: &mut Ui, sel: Option<&Candidate>) {
        ui.label(small_muted("Tests to run"));
        ui.horizontal_wrapped(|ui| {
            for (i, st) in STEPS.iter().enumerate() {
                let locked = *st == Step::Integrity && !self.erase;
                ui.add_enabled_ui(!locked, |ui| {
                    let mut on = self.steps[i] && !locked;
                    let tip = match st {
                        Step::Info => "What the drive is and how it is connected. A USB 2 port is the most common reason a fast drive looks slow.",
                        Step::Speed => "Reads and writes at different block sizes and queue depths, like CrystalDiskMark, with IOPS and latency.",
                        Step::Sustained => "Writes a lot of data in one go and graphs the speed. Cheap drives write fast into a small cache and then slow down sharply.",
                        Step::Scan => "Reads the drive and times every block, to find slow or unreadable areas before data is lost.",
                        _ => "Writes test data across the whole drive and reads it back, to find drives that claim more than they hold. Erases the drive.",
                    };
                    if ui.checkbox(&mut on, st.label()).on_hover_text(if locked { "Needs “Allow erasing the drive”." } else { tip }).changed() {
                        self.steps[i] = on;
                    }
                });
                ui.add_space(6.0);
            }
        });
        ui.add_space(6.0);
        if self.steps[1] {
            ui.label(semibold_text("Speed test", 13.5));
            egui::Grid::new("test-adv-speed").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label(small_muted("Test size"));
                egui::ComboBox::from_id_salt("test-size").selected_text(size_name(self.size)).width(130.0).show_ui(ui, |ui| {
                    for s in SIZES {
                        ui.selectable_value(&mut self.size, s, size_name(s));
                    }
                })
                .response
                .on_hover_text("How much of the drive the speed test uses. Bigger sizes defeat a drive's small fast cache and give honest numbers.");
                ui.end_row();
                ui.label(small_muted("Runs"));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("test-runs").selected_text(format!("{} run{}", self.runs, if self.runs == 1 { "" } else { "s" })).width(130.0).show_ui(ui, |ui| {
                        for n in [1u32, 2, 3, 5, 7, 9] {
                            ui.selectable_value(&mut self.runs, n, format!("{n} run{}", if n == 1 { "" } else { "s" }));
                        }
                    })
                    .response
                    .on_hover_text("Every measurement is repeated and the median is shown, so one unlucky moment does not decide the number.");
                    ui.label(small_muted("of"));
                    egui::ComboBox::from_id_salt("test-secs").selected_text(format!("{:.0} s", self.seconds)).width(80.0).show_ui(ui, |ui| {
                        for n in [2.0, 3.0, 5.0, 8.0, 12.0, 20.0, 30.0] {
                            ui.selectable_value(&mut self.seconds, n, format!("{n:.0} s"));
                        }
                    });
                });
                ui.end_row();
                ui.label(small_muted("Rest between runs"));
                egui::ComboBox::from_id_salt("test-rest").selected_text(format!("{:.0} s", self.interval)).width(80.0).show_ui(ui, |ui| {
                    for n in [0.0, 1.0, 2.0, 5.0, 10.0] {
                        ui.selectable_value(&mut self.interval, n, format!("{n:.0} s"));
                    }
                })
                .response
                .on_hover_text("Lets the drive tidy up between runs. A longer rest shows the speed of a drive that is not already busy.");
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label(small_muted("Measurements"));
            for (i, (name, tip)) in PROFILES.iter().enumerate() {
                ui.checkbox(&mut self.profiles[i], *name).on_hover_text(*tip);
            }
            ui.add_space(2.0);
            ui.checkbox(&mut self.zeros, "Write zeros instead of random data").on_hover_text("Some controllers compress what they write, so zeros look unrealistically fast. Random data is the honest default.");
            ui.checkbox(&mut self.write_through, "Make every write reach the medium before it counts").on_hover_text("Strict and slow: the drive may not answer from its cache. Shows what the flash itself can do.");
            ui.add_space(6.0);
        }
        if self.steps[2] {
            ui.label(semibold_text("Sustained write", 13.5));
            egui::Grid::new("test-adv-sus").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label(small_muted("Amount to write"));
                egui::ComboBox::from_id_salt("test-sus-size").selected_text(size_name(self.sus_size)).width(130.0).show_ui(ui, |ui| {
                    for s in SUSTAINED {
                        ui.selectable_value(&mut self.sus_size, s, size_name(s));
                    }
                });
                ui.end_row();
                ui.label(small_muted("Stop after"));
                egui::ComboBox::from_id_salt("test-sus-secs").selected_text(if self.sus_secs >= 60.0 { format!("{:.0} min", self.sus_secs / 60.0) } else { format!("{:.0} s", self.sus_secs) }).width(130.0).show_ui(ui, |ui| {
                    for n in [30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0] {
                        ui.selectable_value(&mut self.sus_secs, n, if n >= 60.0 { format!("{:.0} min", n / 60.0) } else { format!("{n:.0} s") });
                    }
                });
                ui.end_row();
            });
            ui.add_space(6.0);
        }
        if self.steps[3] {
            ui.label(semibold_text("Surface scan", 13.5));
            ui.checkbox(&mut self.scan_full, "Read every block (slow)").on_hover_text("Off: about two thousand blocks spread over the drive, which finds most problems in seconds. On: the whole drive.");
            ui.horizontal(|ui| {
                ui.label(small_muted("Block size"));
                egui::ComboBox::from_id_salt("test-scan-block").selected_text(size_name(self.scan_block).replace("0 MB", "0 KB").replace("256 MB", "256 KB")).width(110.0).show_ui(ui, |ui| {
                    for b in BLOCKS {
                        ui.selectable_value(&mut self.scan_block, b, if b < 1 << 20 { format!("{} KB", b >> 10) } else { size_name(b) });
                    }
                });
            });
            ui.checkbox(&mut self.stop_on_error, "Stop at the first unreadable block");
            ui.add_space(6.0);
        }
        if self.steps[4] && self.erase {
            ui.label(semibold_text("Capacity check", 13.5));
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.cap_full, false, "Quick: samples spread over the drive");
                ui.radio_value(&mut self.cap_full, true, "Full: every block (exact, slow)");
            });
            ui.add_space(6.0);
        }
        if !self.erase {
            if let Some(c) = sel {
                ui.label(small_muted("Folder for the test files"));
                ui.horizontal(|ui| {
                    ui.label(small_muted(self.folder_for(c).map(|f| f.display().to_string()).unwrap_or_else(|| "none (the drive is not mounted)".into())));
                    if Btn::new("Choose…").small().show(ui).clicked() {
                        if let Some(f) = rfd::FileDialog::new().set_title("Folder on the drive").pick_folder() {
                            self.folder = Some(f);
                        }
                    }
                    if self.folder.is_some() && Btn::new("Automatic").small().show(ui).clicked() {
                        self.folder = None;
                    }
                });
            }
        }
        ui.checkbox(&mut self.no_direct, "Use the system's cache (not recommended)").on_hover_text("Normally the tests bypass the operating system's cache to measure the drive itself. Only turn this on for a file system that cannot do that; the numbers will look better than the drive really is.");
    }

    // ------------------------------------------------------------------ running

    fn running_ui(&mut self, ui: &mut Ui) {
        widgets::run_header(ui, Icon::Test, "Testing");
        let stopping = self.run.as_ref().map(|r| r.job.cancel_requested).unwrap_or(false);
        let l = &self.live;
        let t = self.target.clone();
        let w = ui.available_width().min(680.0);
        ui.vertical_centered(|ui| {
            ui.allocate_ui(vec2(w, 10.0), |ui| {
                panel(ui, |ui| {
                    kv(ui, "Drive", RichText::new(t.map(|c| format!("{} · {} · {}", c.name, c.size_text(), c.drive.path)).unwrap_or_default()));
                    kv(ui, "Method", RichText::new(match l.mode.as_str() {
                        "file" => "A temporary file on the drive",
                        "raw_read" => "Reading straight from the drive (nothing is written)",
                        "raw_write" => "Straight on the drive (it is being erased)",
                        _ => "…",
                    }));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if l.of > 0 {
                            ui.label(semibold_text(format!("Step {} of {}: {}", l.index, l.of, l.label), 15.0));
                        } else {
                            ui.label(semibold_text("Starting…", 15.0));
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(semibold_text(if l.mbps > 0.0 { charts::mbps_text(l.mbps) } else { "–".into() }, 34.0));
                        ui.label(muted(" MB/s"));
                        if l.iops > 0.0 {
                            ui.add_space(14.0);
                            ui.label(small_muted(format!("{} IOPS", charts::iops_text(l.iops))));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(muted(if stopping { "Stopping…".to_string() } else if l.of > 0 { format!("{:.0}%", overall(l) * 100.0) } else { String::new() }));
                        });
                    });
                    progress_bar(ui, Id::new("test-bar"), if l.of > 0 { Some(overall(l)) } else { None });
                    ui.add_space(2.0);
                    ui.label(small_muted(running_line(l)));
                });
                if let Some((p, op)) = pending_of(l) {
                    ui.add_space(10.0);
                    panel(ui, |ui| {
                        step_header(ui, 1, "Speed", false);
                        ui.add_space(4.0);
                        charts::speed_table(ui, Id::new("test-live-speed"), &l.speed, Some((p.as_str(), op)));
                    });
                } else if !l.speed.is_empty() {
                    ui.add_space(10.0);
                    panel(ui, |ui| {
                        step_header(ui, 1, "Speed", false);
                        ui.add_space(4.0);
                        charts::speed_table(ui, Id::new("test-live-speed"), &l.speed, None);
                    });
                }
                if l.step == "sustained" || !l.samples.is_empty() {
                    ui.add_space(10.0);
                    panel(ui, |ui| {
                        step_header(ui, 1, "Sustained write", false);
                        ui.add_space(4.0);
                        charts::speed_graph(ui, Id::new("test-live-graph"), &l.samples, None, 170.0);
                    });
                }
                if l.step == "scan" || !l.map.is_empty() {
                    ui.add_space(10.0);
                    panel(ui, |ui| {
                        step_header(ui, 1, "Surface scan", false);
                        ui.add_space(4.0);
                        let total = self.target.as_ref().map(|c| c.drive.size).unwrap_or(0);
                        charts::surface_map(ui, Id::new("test-live-map"), &l.map, &l.map_mbps, 0, total, l.map_of.max(l.map.len()));
                        ui.add_space(4.0);
                        charts::surface_key(ui);
                    });
                }
                for n in &l.notes {
                    ui.add_space(6.0);
                    banner(ui, Tone::Info, "", n, None);
                }
            });
        });
        ui.add_space(10.0);
        ui.vertical_centered(|ui| {
            if Btn::new(if stopping { "Stopping…" } else { "Cancel" }).enabled(!stopping).show(ui).clicked() {
                self.ask_stop = true;
            }
        });
    }

    // ------------------------------------------------------------------ done

    fn done_ui(&mut self, ui: &mut Ui, sh: &mut Shell) {
        let outcome = self.outcome.clone().unwrap_or(Outcome::Running);
        let mut again = false;
        let rep = self.report.clone();
        let tgt = self.target.as_ref().map(|c| format!("{} ({}, {})", c.name, c.size_text(), c.drive.path)).unwrap_or_default();
        let w = ui.available_width().min(720.0);
        ui.vertical_centered(|ui| {
            ui.add_space(14.0);
            match (&outcome, &rep) {
                (Outcome::Success, Some(r)) => {
                    let tone = match r.verdict.level {
                        Severity::Bad => Tone::Error,
                        Severity::Warning => Tone::Warn,
                        _ => Tone::Success,
                    };
                    status_mark(ui, Id::new("test-mark"), tone, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text(match r.verdict.level {
                        Severity::Bad => "Problems found",
                        Severity::Warning => "Works, with something to look at",
                        _ => "Test complete",
                    }, 26.0));
                    ui.allocate_ui(vec2(w.min(560.0), 10.0), |ui| {
                        ui.label(muted(&r.verdict.headline));
                    });
                    ui.label(small_muted(format!("{tgt} · tested in {}", fmt_duration(r.elapsed_secs))));
                    ui.add_space(10.0);
                    ui.allocate_ui(vec2(w, 10.0), |ui| {
                        ui.vertical(|ui| report_ui(ui, r));
                    });
                }
                (Outcome::Cancelled, _) => {
                    status_mark(ui, Id::new("test-mark"), Tone::Warn, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text("Test cancelled", 22.0));
                    ui.label(muted("The test file was removed."));
                    if self.erase {
                        ui.allocate_ui(vec2(480.0, 10.0), |ui| {
                            ui.add_space(6.0);
                            banner(ui, Tone::Warn, "The drive may have been partly overwritten", "Format it again before you use it.", None);
                        });
                    }
                }
                (Outcome::Failed(f), _) => {
                    status_mark(ui, Id::new("test-mark"), Tone::Error, true);
                    ui.add_space(6.0);
                    ui.label(semibold_text("The test could not run", 22.0));
                    ui.add_space(6.0);
                    ui.allocate_ui(vec2(540.0, 10.0), |ui| banner(ui, Tone::Error, &f.title, &f.message, f.hint.as_deref()));
                }
                _ => {}
            }
            ui.add_space(14.0);
            if let Some(r) = &rep {
                ui.horizontal(|ui| {
                    let total = Btn::new("Copy report").width(ui) + Btn::new("Save report…").width(ui) + 12.0;
                    ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
                    if Btn::new("Copy report").show(ui).clicked() {
                        ui.ctx().copy_text(nytest::report_text(r));
                        self.copied = Some(std::time::Instant::now());
                    }
                    if Btn::new("Save report…").show(ui).clicked() {
                        if let Some(p) = pick_save("Save the report", "nytest-report.txt") {
                            let text = if p.extension().map(|e| e.eq_ignore_ascii_case("json")).unwrap_or(false) { serde_json::to_string_pretty(r).unwrap_or_default() } else { nytest::report_text(r) };
                            self.saved = Some(match std::fs::write(&p, text) {
                                Ok(()) => format!("Saved to {}", p.display()),
                                Err(e) => format!("Could not save: {e}"),
                            });
                        }
                    }
                });
                if self.copied.map(|t| t.elapsed().as_secs() < 3).unwrap_or(false) {
                    ui.label(small_muted("Report copied."));
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                }
                if let Some(s) = &self.saved {
                    ui.label(small_muted(s.as_str()));
                }
                ui.add_space(8.0);
            }
            let (p, h) = widgets::centered_pair(ui, Btn::primary("Test again").icon(Icon::Refresh).big(), Btn::new("Home").icon(Icon::Home));
            again = p;
            if h {
                sh.go = Some(Screen::Home);
            }
            ui.add_space(10.0);
        });
        if again {
            let ctx = ui.ctx().clone();
            self.stage = Stage::Setup;
            self.run = None;
            self.target = None;
            self.outcome = None;
            self.report = None;
            self.sel.refresh(&ctx);
        }
    }

    // ------------------------------------------------------------------ dialogs

    fn dialogs(&mut self, ctx: &egui::Context) {
        if matches!(self.stage, Stage::Setup) && self.sel.modal(ctx) {
            self.notice = None;
            if let Some(c) = self.sel.selected() {
                if !can_erase(c) {
                    self.erase = false;
                    self.steps[4] = false;
                }
            }
            self.folder = None;
        }
        let (mut accept, mut cancel) = (false, false);
        if self.erase {
            if let Some(g) = &self.gate {
                if let Some(c) = g.candidate() {
                    let rechecking = g.is_rechecking();
                    let (_, close) = dialog(ctx, "test-confirm", 470.0, |ui| {
                        ui.label(heading("Erase this drive to test it?"));
                        ui.add_space(8.0);
                        drive_card(ui, c);
                        ui.add_space(6.0);
                        banner(ui, Tone::Error, "Everything on this drive will be permanently erased.", "The tests write over the drive, from the first byte to the last. This cannot be undone. Check that the name and size above are the drive you mean.", None);
                        for w in &c.warnings {
                            ui.add_space(4.0);
                            banner(ui, Tone::Warn, "", w, None);
                        }
                        dialog_buttons(ui, |ui| {
                            if Btn::destructive(if rechecking { "Checking…" } else { "Erase and test" }).enabled(!rechecking).show(ui).clicked() {
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
            let erase = self.erase;
            let (_, close) = dialog(ctx, "test-stop", 440.0, |ui| {
                ui.label(heading("Stop the test?"));
                ui.add_space(6.0);
                ui.label(muted(if erase { "The drive has been partly overwritten and will need to be formatted again." } else { "Nothing on the drive is changed. The temporary test file is removed." }));
                dialog_buttons(ui, |ui| {
                    if (if erase { Btn::destructive("Stop") } else { Btn::primary("Stop") }).show(ui).clicked() {
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

fn can_erase(c: &Candidate) -> bool {
    matches!(c.drive.safety, Safety::Safe | Safety::Virtual) && !c.drive.read_only
}

/// The mounted partition with the most free space (where a test file is put).
fn auto_folder(c: &Candidate) -> Option<PathBuf> {
    c.drive
        .partitions
        .iter()
        .flat_map(|p| p.mountpoints.iter())
        .filter(|m| !m.is_empty() && std::path::Path::new(m.as_str()).is_dir())
        .filter_map(|m| nyforge_core::nytest::util::free_space(std::path::Path::new(m)).map(|f| (f, PathBuf::from(m))))
        .max_by_key(|(f, _)| *f)
        .map(|(_, p)| p)
}

/// How far along the whole test is: finished steps plus the fraction of this one.
fn overall(l: &Live) -> f32 {
    if l.of == 0 {
        return 0.0;
    }
    (((l.index.saturating_sub(1)) as f64 + l.fraction.clamp(0.0, 1.0)) / l.of as f64).clamp(0.0, 1.0) as f32
}

/// "SEQ1M Q8T1 read" while the speed step is running: which measurement is under way.
fn pending_of(l: &Live) -> Option<(String, Op)> {
    if l.step != "speed" {
        return None;
    }
    let (prof, op) = l.label.rsplit_once(' ')?;
    Some((prof.to_string(), match op {
        "write" => Op::Write,
        "read" => Op::Read,
        _ => return None,
    }))
}

fn running_line(l: &Live) -> String {
    match l.step.as_str() {
        "info" => "Looking at the drive and how it is connected…".into(),
        "speed" => format!("Measuring {}. Each measurement is repeated, and the median is shown.", l.label),
        "sustained" => "Writing without a break and watching for the moment the drive slows down…".into(),
        "scan" => "Reading the drive and timing every block…".into(),
        "integrity" => "Writing test data across the drive and reading it back…".into(),
        _ => "Getting ready…".into(),
    }
}

// ---------------------------------------------------------------------------------------- the report

fn severity_color(s: Severity) -> Color32 {
    match s {
        Severity::Good => OK,
        Severity::Info => ACCENT_HI,
        Severity::Warning => WARN,
        Severity::Bad => DANGER,
    }
}

fn report_ui(ui: &mut Ui, r: &Report) {
    // what the verdict is made of
    if !r.findings.is_empty() {
        panel(ui, |ui| {
            step_header(ui, 1, "What was found", false);
            ui.add_space(6.0);
            for f in &r.findings {
                ui.horizontal_top(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                    let c = severity_color(f.severity);
                    ui.painter().circle_filled(rect.center(), 9.0, with_alpha(c, 45));
                    icons::paint(
                        ui.painter(),
                        match f.severity {
                            Severity::Good => Icon::Check,
                            Severity::Info => Icon::Info,
                            Severity::Warning => Icon::Warn,
                            Severity::Bad => Icon::Error,
                        },
                        rect.shrink(4.0),
                        c,
                    );
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(semibold_text(&f.title, 14.0));
                        if !f.detail.is_empty() {
                            ui.label(small_muted(&f.detail));
                        }
                    });
                });
                ui.add_space(6.0);
            }
        });
        ui.add_space(10.0);
    }
    if let Some(sd) = &r.sd_class {
        if sd.speed_class.is_some() || sd.app_class.is_some() {
            panel(ui, |ui| {
                step_header(ui, 1, "Memory-card rating (estimated)", false);
                ui.add_space(4.0);
                for (k, v) in [("Speed class", sd.speed_class.clone()), ("Application class", sd.app_class.clone())] {
                    if let Some(v) = v {
                        kv(ui, k, egui::RichText::new(v));
                    }
                }
                ui.add_space(4.0);
                ui.label(small_muted(format!("From {} MB/s sustained writing and {} / {} random read / write operations per second. Cards that print a rating on the label have been certified; this is only what this card does now.", charts::mbps_text(sd.seq_write_mbps), charts::iops_text(sd.rnd_read_iops), charts::iops_text(sd.rnd_write_iops))));
            });
            ui.add_space(10.0);
        }
    }
    if !r.speed.is_empty() {
        panel(ui, |ui| {
            ui.horizontal(|ui| {
                step_header(ui, 1, "Speed", false);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(small_muted(match r.mode.as_str() {
                        "file" => "temporary file on the drive",
                        "raw_read" => "straight from the drive, read only",
                        "raw_write" => "straight on the drive",
                        _ => "",
                    }));
                });
            });
            ui.add_space(4.0);
            charts::speed_table(ui, Id::new("test-done-speed"), &r.speed, None);
            ui.label(small_muted("MB/s is the median of the runs. Hover a row for every run and the latency."));
        });
        ui.add_space(10.0);
    }
    if let Some(s) = &r.sustained {
        panel(ui, |ui| {
            step_header(ui, 1, "Sustained write", false);
            ui.add_space(4.0);
            if s.samples.len() >= 2 {
                charts::speed_graph(ui, Id::new("test-done-graph"), &s.samples, s.cliff.as_ref(), 190.0);
                ui.add_space(4.0);
                ui.label(small_muted(format!("{} written in {} · average {} MB/s · started at {} MB/s · ended at {} MB/s · slowest {} MB/s", human_size(s.bytes), fmt_duration(s.seconds), charts::mbps_text(s.avg_mbps), charts::mbps_text(s.start_mbps), charts::mbps_text(s.end_mbps), charts::mbps_text(s.min_mbps))));
            } else {
                // finished before the first graph point: a drawing would be one dot, so just say what happened
                ui.label(small_muted(format!("{} written in {} at {} MB/s. That was too quick to draw a graph: the drive took all of it without slowing down.", human_size(s.bytes), fmt_duration(s.seconds), charts::mbps_text(s.avg_mbps))));
            }
        });
        ui.add_space(10.0);
    }
    if let Some(s) = &r.scan {
        panel(ui, |ui| {
            step_header(ui, 1, if s.quick { "Surface scan (sampled)" } else { "Surface scan" }, false);
            ui.add_space(4.0);
            charts::surface_map(ui, Id::new("test-done-map"), &s.map, &s.map_mbps, 0, s.scanned_bytes.max(1), s.map.len());
            ui.add_space(4.0);
            charts::surface_key(ui);
            ui.add_space(4.0);
            ui.label(small_muted(format!(
                "{} read at {} MB/s on average (slowest 1%: {} MB/s) · {} slow block{} · {} unreadable block{}{}",
                human_size(s.scanned_bytes),
                charts::mbps_text(s.avg_mbps),
                charts::mbps_text(s.min_mbps),
                s.slow_blocks,
                if s.slow_blocks == 1 { "" } else { "s" },
                s.error_blocks,
                if s.error_blocks == 1 { "" } else { "s" },
                if s.bad_bytes > 0 { format!(" ({} lost)", human_size(s.bad_bytes)) } else { String::new() }
            )));
            if let Some(first) = s.first_error {
                ui.label(RichText::new(format!("First unreadable place: {} into the drive.", human_size(first))).size(12.0).color(DANGER));
            }
        });
        ui.add_space(10.0);
    }
    if let Some(i) = &r.integrity {
        let c = &i.report;
        panel(ui, |ui| {
            step_header(ui, 1, "Capacity check", false);
            ui.add_space(4.0);
            kv(ui, "Claimed size", RichText::new(human_size(c.claimed)));
            kv(ui, "Tested", RichText::new(format!("{} ({})", human_size(c.tested_bytes), if c.mode == CapacityTest::Full { "every block" } else { "samples spread over the drive" })));
            if c.genuine {
                kv(ui, "Result", RichText::new("The drive holds what it claims").color(OK_HI));
            } else {
                kv(ui, "Result", RichText::new(format!("Fake capacity: it holds about {}", human_size(c.estimated_real))).color(DANGER));
            }
            if c.bad_blocks > 0 {
                kv(ui, "Bad blocks", RichText::new(c.bad_blocks.to_string()).color(DANGER));
            }
            kv(ui, "Write / read", RichText::new(format!("{} / {} MB/s", charts::mbps_text(i.write_mbps), charts::mbps_text(i.read_mbps))));
        });
        ui.add_space(10.0);
    }
    if let Some(info) = &r.info {
        panel(ui, |ui| {
            step_header(ui, 1, "The drive", false);
            ui.add_space(4.0);
            for (k, v) in &info.details {
                kv(ui, k, RichText::new(v.as_str()));
            }
            if let Some(sm) = &info.smart {
                if sm.available {
                    kv(ui, "Health (SMART)", RichText::new(match sm.passed {
                        Some(true) => "Passed",
                        Some(false) => "FAILED",
                        None => "Reported",
                    }));
                    if let Some(t) = sm.temperature_c {
                        kv(ui, "Temperature", RichText::new(format!("{t} °C")));
                    }
                    if let Some(h) = sm.power_on_hours {
                        kv(ui, "Powered on", RichText::new(format!("{h} hours")));
                    }
                    if let Some(w) = sm.wear_percent {
                        kv(ui, "Life used", RichText::new(format!("{w}%")));
                    }
                    if let Some(n) = sm.reallocated {
                        kv(ui, "Replaced sectors", RichText::new(n.to_string()));
                    }
                    if let Some(n) = sm.pending {
                        kv(ui, "Waiting sectors", RichText::new(n.to_string()));
                    }
                    if let Some(n) = sm.media_errors {
                        kv(ui, "Media errors", RichText::new(n.to_string()));
                    }
                } else if let Some(n) = &sm.note {
                    ui.label(small_muted(n.as_str()));
                }
            }
            if let Some(c) = &info.card {
                kv(ui, "Card", RichText::new(format!("{} {} {}", c.manufacturer, c.product, c.revision).trim().to_string()));
                if let Some(l) = &c.life_time {
                    kv(ui, "Memory life", RichText::new(l.as_str()));
                }
            }
        });
        ui.add_space(10.0);
    }
    for n in &r.notes {
        ui.label(small_muted(n.as_str()));
    }
}

