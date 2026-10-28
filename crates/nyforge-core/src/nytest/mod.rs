//! NyTest: how fast is this drive, is it healthy, and is it what it claims to be?
//!
//! * **Speed** ([`speed`]): the same four measurements CrystalDiskMark made famous (sequential 1 MiB at queue depth 8 and 1,
//!   random 4 KiB at queue depth 32 and 1; reads and writes), done the same careful way (uncached I/O, data that cannot be
//!   compressed, the final flush counted), plus what it leaves out: the spread of the runs, latency percentiles, and IOPS.
//! * **Sustained write** ([`sustained`]): a long write with a speed graph, which shows where the fast cache ends. Short
//!   benchmarks of a few hundred MB never see that, and it is the number that decides how long a big copy really takes.
//! * **Surface scan** ([`scan`]): reads the whole drive (read-only) and times every block, so failing and slow areas
//!   show up before the data is lost.
//! * **Capacity check** ([`integrity`]): finds drives that claim more than they hold.
//! * **Info** ([`info`]): what the drive is, how it is connected (the USB link speed explains many "slow" drives) and,
//!   when the drive reports it, its own health (SMART, SD/eMMC register data).
//! * **Verdict** ([`verdict`]): turns it all into plain statements, never claiming more than was tested.
//!
//! Nothing here writes to a drive unless the caller says the drive may be erased (`Options::erase`). Speed and sustained tests
//! on a mounted drive use a temporary file that is removed afterwards.

pub mod engine;
pub mod info;
pub mod integrity;
pub mod scan;
pub mod speed;
pub mod sustained;
pub mod util;
pub mod verdict;

use crate::device::Drive;
use crate::error::{NyError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

pub use engine::{Access, Latency, Op, Pattern};

/// What is being tested.
#[derive(Debug, Clone)]
pub struct Subject {
    /// The physical drive, when it is one (not a folder on a disk the program was pointed at).
    pub drive: Option<Drive>,
    /// The raw device node (for the scan, the capacity check and raw speed tests).
    pub device: Option<PathBuf>,
    /// Size of the drive in bytes (0 when only a folder is known).
    pub size: u64,
    /// A folder on the drive for the file-based speed tests.
    pub folder: Option<PathBuf>,
}

impl Subject {
    pub fn from_drive(d: &Drive, folder: Option<PathBuf>) -> Subject {
        Subject { device: Some(PathBuf::from(&d.path)), size: d.size, drive: Some(d.clone()), folder }
    }
    pub fn from_folder(folder: impl Into<PathBuf>) -> Subject {
        Subject { drive: None, device: None, size: 0, folder: Some(folder.into()) }
    }
    pub fn display(&self) -> String {
        match (&self.drive, &self.device, &self.folder) {
            (Some(d), _, _) => d.path.clone(),
            (_, Some(p), _) => p.display().to_string(),
            (_, _, Some(f)) => f.display().to_string(),
            _ => "?".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Info,
    Speed,
    Sustained,
    Scan,
    Integrity,
}

impl Step {
    pub fn id(self) -> &'static str {
        match self {
            Step::Info => "info",
            Step::Speed => "speed",
            Step::Sustained => "sustained",
            Step::Scan => "scan",
            Step::Integrity => "integrity",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Step::Info => "Drive information",
            Step::Speed => "Speed test",
            Step::Sustained => "Sustained write",
            Step::Scan => "Surface scan",
            Step::Integrity => "Capacity check",
        }
    }
    pub fn parse(s: &str) -> Option<Step> {
        Some(match s.to_ascii_lowercase().as_str() {
            "info" => Step::Info,
            "speed" => Step::Speed,
            "sustained" | "write" => Step::Sustained,
            "scan" | "surface" => Step::Scan,
            "integrity" | "capacity" | "fake" => Step::Integrity,
            _ => return None,
        })
    }
    /// Overwrites the drive?
    pub fn destructive(self) -> bool {
        matches!(self, Step::Integrity)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Quick,
    Standard,
    Full,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub steps: Vec<Step>,
    /// The drive may be erased: speed and sustained tests then go straight to the device (the purest numbers) and the
    /// capacity check can run. Without it nothing on the drive is touched.
    pub erase: bool,
    pub speed: speed::SpeedOptions,
    pub sustained: sustained::SustainedOptions,
    pub scan: scan::ScanOptions,
    pub integrity: crate::usbformat::capacity::CapacityTest,
    /// uncached I/O (the honest setting; off only for file systems that cannot do it)
    pub direct: bool,
    /// unmount the drive before the tests that erase it
    pub unmount: bool,
}

impl Options {
    pub fn preset(p: Preset) -> Options {
        use crate::usbformat::capacity::CapacityTest;
        match p {
            Preset::Quick => Options {
                steps: vec![Step::Info, Step::Speed],
                erase: false,
                speed: speed::SpeedOptions { size: 256 << 20, runs: 1, seconds: 3.0, interval: 0.5, ..Default::default() },
                sustained: sustained::SustainedOptions::default(),
                scan: scan::ScanOptions { quick: true, ..Default::default() },
                integrity: CapacityTest::Quick,
                direct: true,
                unmount: true,
            },
            Preset::Standard => Options {
                steps: vec![Step::Info, Step::Speed, Step::Sustained, Step::Scan],
                erase: false,
                speed: speed::SpeedOptions { size: 1 << 30, runs: 3, seconds: 5.0, interval: 1.0, ..Default::default() },
                sustained: sustained::SustainedOptions { size: 4 << 30, max_seconds: 300.0, ..Default::default() },
                scan: scan::ScanOptions { quick: true, ..Default::default() },
                integrity: CapacityTest::Quick,
                direct: true,
                unmount: true,
            },
            Preset::Full => Options {
                steps: vec![Step::Info, Step::Speed, Step::Sustained, Step::Scan, Step::Integrity],
                erase: false,
                speed: speed::SpeedOptions { size: 4 << 30, runs: 5, seconds: 8.0, interval: 2.0, ..Default::default() },
                sustained: sustained::SustainedOptions { size: 16 << 30, max_seconds: 900.0, ..Default::default() },
                scan: scan::ScanOptions { quick: false, ..Default::default() },
                integrity: CapacityTest::Full,
                direct: true,
                unmount: true,
            },
        }
    }
}

// ---------------------------------------------------------------------------------------- results

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Good,
    Info,
    Warning,
    Bad,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    /// machine-readable name ("fake_capacity", "bad_sectors", …)
    pub id: String,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub level: Severity,
    pub headline: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub tool: String,
    pub started: String,
    pub elapsed_secs: f64,
    pub target: String,
    /// "file" (a test file on the drive), "raw_read" (reads straight from the device) or "raw_write" (erased)
    pub mode: String,
    pub steps: Vec<String>,
    pub info: Option<info::DeviceInfo>,
    pub speed: Vec<speed::SpeedResult>,
    pub sustained: Option<sustained::SustainedResult>,
    pub scan: Option<scan::ScanResult>,
    pub integrity: Option<integrity::IntegrityResult>,
    pub sd_class: Option<verdict::SdClass>,
    pub findings: Vec<Finding>,
    pub verdict: Verdict,
    pub notes: Vec<String>,
    /// Tests that were asked for but could not run (named as in the steps list).
    #[serde(default)]
    pub skipped: Vec<String>,
}

// ---------------------------------------------------------------------------------------- events

/// What a running test tells its watcher (one JSON object per line with `--json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TestEvent {
    NytestStart { target: String, size: u64, steps: Vec<String>, mode: String },
    NytestStep { step: String, label: String, index: usize, of: usize },
    /// Progress inside a step (0–1), and the current rate when there is one.
    NytestLive { step: String, label: String, fraction: f64, mbps: f64, iops: f64 },
    NytestInfo { info: Box<info::DeviceInfo> },
    NytestSpeed { result: speed::SpeedResult },
    /// A point of the sustained-write graph.
    NytestSample { t: f64, mbps: f64, bytes: u64 },
    /// One segment of the surface map is finished (`state`: 0 fine, 1 a little slow, 2 slow, 3 error).
    NytestMap { index: usize, of: usize, state: u8, mbps: f64 },
    NytestNote { message: String },
    NytestDone { report: Box<Report> },
}

pub(crate) fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(NyError::cancelled())
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------- where tests run

/// Where the speed and sustained tests do their I/O.
#[derive(Debug, Clone)]
pub enum Place {
    /// A temporary file in this folder
    File(PathBuf),
    /// Reads straight from the device (nothing is written)
    RawRead(PathBuf),
    /// Reads and writes straight to the device (it is erased)
    RawWrite(PathBuf),
}

impl Place {
    pub fn mode(&self) -> &'static str {
        match self {
            Place::File(_) => "file",
            Place::RawRead(_) => "raw_read",
            Place::RawWrite(_) => "raw_write",
        }
    }
    pub fn writable(&self) -> bool {
        !matches!(self, Place::RawRead(_))
    }
}

/// Decide where the speed tests go. A folder on the drive wins unless erasing is allowed (then the device itself is used).
pub fn choose_place(s: &Subject, o: &Options) -> Option<Place> {
    if o.erase {
        if let Some(d) = &s.device {
            return Some(Place::RawWrite(d.clone()));
        }
    }
    if let Some(f) = &s.folder {
        return Some(Place::File(f.clone()));
    }
    s.device.as_ref().map(|d| Place::RawRead(d.clone()))
}

/// Run the chosen steps in order.
pub fn run(s: &Subject, o: &Options, cancel: &AtomicBool, emit: &mut dyn FnMut(TestEvent)) -> Result<Report> {
    let started = std::time::Instant::now();
    let started_iso = crate::nytest::util::now_iso();
    let place = choose_place(s, o);
    let mut steps = o.steps.clone();
    // order they make sense in: information first; the one that erases last
    steps.sort_by_key(|st| match st {
        Step::Info => 0,
        Step::Speed => 1,
        Step::Sustained => 2,
        Step::Scan => 3,
        Step::Integrity => 4,
    });
    steps.dedup();
    let mut notes: Vec<String> = vec![];
    if steps.iter().any(|st| st.destructive()) && !o.erase {
        return Err(NyError::invalid("The capacity check overwrites the whole drive.").with_hint("Allow erasing the drive to run it, or leave that test out."));
    }
    if (steps.contains(&Step::Scan) || steps.contains(&Step::Integrity)) && s.device.is_none() {
        return Err(NyError::invalid("The surface scan and the capacity check need a whole drive, not a folder.").with_hint("Pick the drive."));
    }
    if steps.contains(&Step::Speed) || steps.contains(&Step::Sustained) {
        match &place {
            None => return Err(NyError::invalid("Nothing to run the speed tests on.").with_hint("Pick a drive or a folder.")),
            Some(Place::RawRead(_)) if steps.contains(&Step::Sustained) => {
                steps.retain(|x| *x != Step::Sustained);
                notes.push("The sustained write test was left out: it needs a mounted drive with free space, or permission to erase the drive.".into());
            }
            _ => {}
        }
    }
    let mode = place.as_ref().map(|p| p.mode()).unwrap_or("none").to_string();
    emit(TestEvent::NytestStart { target: s.display(), size: s.size, steps: steps.iter().map(|x| x.id().to_string()).collect(), mode: mode.clone() });

    let mut rep = Report {
        tool: format!("NyForge {}", env!("CARGO_PKG_VERSION")),
        started: started_iso,
        elapsed_secs: 0.0,
        target: s.display(),
        mode,
        steps: steps.iter().map(|x| x.id().to_string()).collect(),
        skipped: vec![],
        info: None,
        speed: vec![],
        sustained: None,
        scan: None,
        integrity: None,
        sd_class: None,
        findings: vec![],
        verdict: Verdict { level: Severity::Good, headline: String::new() },
        notes,
    };
    let of = steps.len();
    // The tests that erase come last, so a failure in one of them cannot make the earlier results unavailable: each
    // step's failure is reported but the report still holds what the others found. A cancel ends everything.
    for (i, st) in steps.iter().enumerate() {
        check(cancel)?;
        emit(TestEvent::NytestStep { step: st.id().into(), label: st.label().into(), index: i + 1, of });
        let r: Result<()> = (|| {
            match st {
                Step::Info => {
                    let inf = info::gather(s);
                    emit(TestEvent::NytestInfo { info: Box::new(inf.clone()) });
                    rep.info = Some(inf);
                }
                Step::Speed => {
                    let place = place.as_ref().unwrap();
                    let (res, notes) = speed::run(s, place, &o.speed, o.direct, o.unmount, cancel, emit)?;
                    rep.speed = res;
                    rep.notes.extend(notes);
                }
                Step::Sustained => {
                    let place = place.as_ref().unwrap();
                    let (res, notes) = sustained::run(s, place, &o.sustained, o.direct, o.unmount, cancel, emit)?;
                    rep.sustained = Some(res);
                    rep.notes.extend(notes);
                }
                Step::Scan => {
                    let dev = s.device.as_ref().unwrap();
                    rep.scan = Some(scan::run(dev, s.size, &o.scan, o.direct, cancel, emit)?);
                }
                Step::Integrity => {
                    rep.integrity = Some(integrity::run(s, o.integrity, o.unmount, cancel, emit)?);
                }
            }
            Ok(())
        })();
        match r {
            Ok(()) => {}
            Err(e) if e.kind == crate::error::ErrorKind::Cancelled => return Err(e),
            Err(e) if e.kind == crate::error::ErrorKind::DeviceGone => return Err(e),
            Err(e) => {
                let msg = format!("{} could not be completed: {}{}", st.label(), e.message, e.hint.as_ref().map(|h| format!(" {h}")).unwrap_or_default());
                emit(TestEvent::NytestNote { message: msg.clone() });
                rep.notes.push(msg);
                rep.skipped.push(st.label().to_string());
            }
        }
    }
    verdict::assess(&mut rep);
    rep.elapsed_secs = started.elapsed().as_secs_f64();
    emit(TestEvent::NytestDone { report: Box::new(rep.clone()) });
    Ok(rep)
}

/// A readable text version of a report (for "save report" and for pasting into a forum post).
pub fn report_text(r: &Report) -> String {
    use crate::format::human_size;
    let mut s = String::new();
    s.push_str(&format!("NyTest report — {}\n{} · started {} · {:.0} s\n\n", r.target, r.tool, r.started, r.elapsed_secs));
    if let Some(i) = &r.info {
        s.push_str(&format!("Drive: {}\n", i.name));
        if i.size > 0 {
            s.push_str(&format!("Size: {}\n", human_size(i.size)));
        }
        for (k, v) in &i.details {
            s.push_str(&format!("{k}: {v}\n"));
        }
        s.push('\n');
    }
    if !r.speed.is_empty() {
        s.push_str(&format!("Speed ({})\n", match r.mode.as_str() {
            "file" => "file on the drive",
            "raw_read" => "straight from the device, read only",
            _ => "straight to and from the device",
        }));
        for x in &r.speed {
            s.push_str(&format!(
                "  {:<14} {:<5} {:>9.1} MB/s {:>10.0} IOPS   latency p50 {} µs · p99 {} µs · max {} µs\n",
                x.profile,
                if x.op == Op::Read { "read" } else { "write" },
                x.mbps,
                x.iops,
                x.lat.p50_us,
                x.lat.p99_us,
                x.lat.max_us
            ));
        }
        s.push('\n');
    }
    if let Some(x) = &r.sustained {
        s.push_str(&format!("Sustained write: {} in {:.1} s, average {:.1} MB/s, start {:.1} MB/s, end {:.1} MB/s, lowest {:.1} MB/s\n", human_size(x.bytes), x.seconds, x.avg_mbps, x.start_mbps, x.end_mbps, x.min_mbps));
        if let Some(c) = &x.cliff {
            s.push_str(&format!("  The fast part ends after {} ({:.0} s): {:.1} MB/s before, {:.1} MB/s after.\n", human_size(c.at_bytes), c.at_secs, c.before_mbps, c.after_mbps));
        }
        s.push('\n');
    }
    if let Some(x) = &r.scan {
        s.push_str(&format!(
            "Surface scan{}: {} read at {:.1} MB/s on average (lowest {:.1}); {} slow blocks, {} unreadable blocks ({} unreadable)\n\n",
            if x.quick { " (sampled)" } else { "" },
            human_size(x.scanned_bytes),
            x.avg_mbps,
            x.min_mbps,
            x.slow_blocks,
            x.error_blocks,
            human_size(x.bad_bytes)
        ));
    }
    if let Some(x) = &r.integrity {
        s.push_str(&format!(
            "Capacity check ({:?}): claims {}, {} \n\n",
            x.report.mode,
            human_size(x.report.claimed),
            if x.report.genuine { "all test blocks came back correct".to_string() } else { format!("{} bad blocks; probably holds about {}", x.report.bad_blocks, human_size(x.report.estimated_real)) }
        ));
    }
    s.push_str(&format!("Verdict: {}\n", r.verdict.headline));
    for f in &r.findings {
        let mark = match f.severity {
            Severity::Good => "✔",
            Severity::Info => "i",
            Severity::Warning => "!",
            Severity::Bad => "✘",
        };
        s.push_str(&format!("  {mark} {} — {}\n", f.title, f.detail));
    }
    for n in &r.notes {
        s.push_str(&format!("  note: {n}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::engine::*;
    use super::*;
    use crate::nytest::util::testutil::tdir;
    use std::time::Duration;

    fn quick_speed() -> speed::SpeedOptions {
        speed::SpeedOptions { size: 32 << 20, runs: 2, seconds: 0.6, interval: 0.05, profiles: vec!["SEQ1M Q8T1".into(), "RND4K Q1T1".into()], ..Default::default() }
    }

    #[test]
    fn jobs_move_data_in_every_mode_and_count_it() {
        let dir = tdir("jobs");
        let p = dir.join("f.bin");
        let o = open_test_file(&p, true, true, true).unwrap();
        let cancel = AtomicBool::new(false);
        fill_file(&o.file, 16 << 20, &cancel, &mut |_| {}).unwrap();
        for (op, access, block, qd) in [(Op::Read, Access::Sequential, 1 << 20, 4), (Op::Write, Access::Sequential, 1 << 20, 2), (Op::Read, Access::Random, 4096, 8), (Op::Write, Access::Random, 4096, 1)] {
            let job = Job { op, access, block, qd, start: 0, len: 16 << 20, pattern: Pattern::Random, wrap: true, sync_each_write: false, seed: 1 };
            let m = run_job(&o.file, &job, &Limits { duration: Duration::from_millis(400), max_bytes: None, cancel: &cancel }, Duration::from_millis(100), &mut |_, _, _| {}).unwrap();
            assert!(m.bytes > 0 && m.ops > 0, "{op:?} {access:?}");
            assert_eq!(m.bytes, m.ops * block as u64);
            assert_eq!(m.hist.count(), m.ops);
            assert!(m.mbps() > 0.0 && m.iops() > 0.0);
            assert!(m.elapsed >= Duration::from_millis(400) && m.elapsed < Duration::from_secs(5), "{:?}", m.elapsed);
        }
        // a job without wrap stops after covering its range, exactly once
        let job = Job { op: Op::Read, access: Access::Sequential, block: 1 << 20, qd: 3, start: 0, len: 16 << 20, pattern: Pattern::Random, wrap: false, sync_each_write: false, seed: 1 };
        let m = run_job(&o.file, &job, &Limits { duration: Duration::from_secs(20), max_bytes: None, cancel: &cancel }, Duration::from_millis(100), &mut |_, _, _| {}).unwrap();
        assert_eq!(m.bytes, 16 << 20);
        assert!(m.elapsed < Duration::from_secs(10));
        // byte limit
        let m = run_job(&o.file, &Job { wrap: true, ..job.clone() }, &Limits { duration: Duration::from_secs(20), max_bytes: Some(5 << 20), cancel: &cancel }, Duration::from_millis(100), &mut |_, _, _| {}).unwrap();
        assert!(m.bytes >= 5 << 20 && m.elapsed < Duration::from_secs(5), "{} bytes in {:?}", m.bytes, m.elapsed);
        // cancel
        let stop = AtomicBool::new(true);
        let e = run_job(&o.file, &job, &Limits { duration: Duration::from_secs(20), max_bytes: None, cancel: &stop }, Duration::from_millis(100), &mut |_, _, _| {}).err().unwrap();
        assert_eq!(e.kind, crate::error::ErrorKind::Cancelled);
    }

    #[test]
    fn writes_write_what_they_say_and_zero_pattern_is_zero() {
        let dir = tdir("pattern");
        let p = dir.join("f.bin");
        let o = open_test_file(&p, true, true, false).unwrap();
        o.file.set_len(4 << 20).unwrap();
        let cancel = AtomicBool::new(false);
        let job = Job { op: Op::Write, access: Access::Sequential, block: 1 << 20, qd: 2, start: 0, len: 4 << 20, pattern: Pattern::Random, wrap: false, sync_each_write: true, seed: 9 };
        run_job(&o.file, &job, &Limits { duration: Duration::from_secs(5), max_bytes: None, cancel: &cancel }, Duration::from_millis(100), &mut |_, _, _| {}).unwrap();
        let d = std::fs::read(&p).unwrap();
        assert_eq!(d.len(), 4 << 20);
        // data a drive cannot compress
        let packed = zstd::bulk::compress(&d, 3).unwrap();
        assert!(packed.len() as f64 > d.len() as f64 * 0.95, "{} -> {}", d.len(), packed.len());
        run_job(&o.file, &Job { pattern: Pattern::Zero, ..job }, &Limits { duration: Duration::from_secs(5), max_bytes: None, cancel: &cancel }, Duration::from_millis(100), &mut |_, _, _| {}).unwrap();
        assert!(std::fs::read(&p).unwrap().iter().all(|&b| b == 0));
    }

    #[test]
    fn a_folder_run_gives_results_cleans_up_and_makes_a_report() {
        let dir = tdir("folder");
        let s = Subject::from_folder(&dir);
        let o = Options {
            steps: vec![Step::Speed, Step::Sustained],
            speed: quick_speed(),
            sustained: sustained::SustainedOptions { size: 48 << 20, max_seconds: 20.0, ..Default::default() },
            ..Options::preset(Preset::Quick)
        };
        let mut ev = vec![];
        let rep = run(&s, &o, &AtomicBool::new(false), &mut |e| ev.push(e)).unwrap();
        assert_eq!(rep.mode, "file");
        assert_eq!(rep.speed.len(), 4, "two profiles, read and write");
        for r in &rep.speed {
            assert!(r.mbps > 0.0 && r.iops > 0.0 && r.runs.len() == 2, "{r:?}");
            assert!(r.lat.p50_us <= r.lat.p99_us && r.lat.p99_us <= r.lat.max_us, "{:?}", r.lat);
        }
        let s = rep.sustained.as_ref().unwrap();
        assert_eq!(s.bytes, 48 << 20);
        assert!(!s.samples.is_empty() || s.seconds < 0.3);
        // the temporary files are gone
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "test files must be removed");
        assert!(matches!(ev.first(), Some(TestEvent::NytestStart { .. })));
        assert!(matches!(ev.last(), Some(TestEvent::NytestDone { .. })));
        assert!(ev.iter().filter(|e| matches!(e, TestEvent::NytestSpeed { .. })).count() == 4);
        let txt = report_text(&rep);
        assert!(txt.contains("SEQ1M Q8T1") && txt.contains("Verdict:"), "{txt}");
        // and every event round-trips through JSON, as the GUI will read it
        for e in &ev {
            let j = serde_json::to_string(e).unwrap();
            let _: TestEvent = serde_json::from_str(&j).unwrap();
        }
    }

    #[test]
    fn failures_and_refusals() {
        let dir = tdir("refuse");
        let s = Subject::from_folder(&dir);
        // erasing tests need permission, folders cannot be scanned
        let o = Options { steps: vec![Step::Integrity], ..Options::preset(Preset::Quick) };
        assert_eq!(run(&s, &o, &AtomicBool::new(false), &mut |_| {}).err().unwrap().kind, crate::error::ErrorKind::InvalidInput);
        let o = Options { steps: vec![Step::Scan], ..Options::preset(Preset::Quick) };
        assert_eq!(run(&s, &o, &AtomicBool::new(false), &mut |_| {}).err().unwrap().kind, crate::error::ErrorKind::InvalidInput);
        // a folder that is not there: the step fails, the report still comes back with a note
        let s2 = Subject::from_folder(dir.join("nope"));
        let o = Options { steps: vec![Step::Speed], speed: quick_speed(), ..Options::preset(Preset::Quick) };
        let rep = run(&s2, &o, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert!(rep.speed.is_empty());
        assert!(rep.notes.iter().any(|n| n.contains("not a folder")), "{:?}", rep.notes);
        // cancel
        let stop = AtomicBool::new(true);
        let o = Options { steps: vec![Step::Speed], speed: quick_speed(), ..Options::preset(Preset::Quick) };
        assert_eq!(run(&s, &o, &stop, &mut |_| {}).err().unwrap().kind, crate::error::ErrorKind::Cancelled);
        assert_eq!(std::fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("nytest")).count(), 0);
    }

    #[test]
    fn a_test_file_is_made_smaller_when_the_drive_has_no_room() {
        // Only meaningful where we can know free space; the check itself is the arithmetic of open_bench.
        let dir = tdir("room");
        let free = crate::nytest::util::free_space(&dir).unwrap_or(0);
        if free < (200 << 20) {
            return; // nothing to assert on a nearly full disk
        }
        let s = Subject::from_folder(&dir);
        let mut notes = vec![];
        let b = speed::open_bench(&s, &Place::File(dir.clone()), free * 2, true, true, false, "speed", &AtomicBool::new(true), &mut |_| {}, &mut notes);
        // cancelled while filling (the flag is up), but the size decision was made first
        assert!(b.is_err());
        assert!(notes.iter().any(|n| n.contains("instead of")), "{notes:?}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "a cancelled run removes its file");
    }
}
