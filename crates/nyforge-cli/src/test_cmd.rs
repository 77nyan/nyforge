//! `nyforge test`: how fast is this drive, is it healthy, is it what it claims to be.

use crate::ui::Ui;
use crate::{confirm_erase, install_ctrlc, is_root, print_event, rerun_hint, Res};
use clap::Args;
use nyforge_core::device::{self, Drive};
use nyforge_core::error::{ErrorKind, NyError};
use nyforge_core::format::{human_duration, human_size};
use nyforge_core::nytest::util::parse_size;
use nyforge_core::nytest::{self, engine::Pattern, Options, Preset, Severity, Step, Subject, TestEvent};
use nyforge_core::usbformat::capacity::CapacityTest;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct TestArgs {
    /// The drive to test (/dev/sdb, \\.\PhysicalDrive2) or a folder on it (/media/me/USB)
    pub target: String,
    /// How thorough: quick (about 30 s), standard (a few minutes), full (reads the whole drive and checks its capacity: can take hours)
    #[arg(long, value_name = "LEVEL", default_value = "standard")]
    pub preset: String,
    /// Which tests to run: info, speed, sustained, scan, integrity (comma-separated). Overrides the preset's list
    #[arg(long, value_name = "LIST")]
    pub tests: Option<String>,
    /// Folder on the drive for the speed and sustained-write files (default: the drive's biggest mounted partition)
    #[arg(long, value_name = "DIR")]
    pub folder: Option<PathBuf>,
    /// Allow tests that overwrite the whole drive: speed tests go straight to the device and the capacity check can run
    #[arg(long)]
    pub erase: bool,
    /// Do not ask for confirmation (required with --erase when not run from a terminal)
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Allow loop/virtual devices (a testing aid; NyTest otherwise only takes removable drives)
    #[arg(long)]
    pub allow_virtual: bool,

    /// Speed test: size of the test file, e.g. 256M, 1G, 4G
    #[arg(long, value_name = "SIZE")]
    pub size: Option<String>,
    /// Speed test: timed runs per measurement
    #[arg(long, value_name = "N")]
    pub runs: Option<u32>,
    /// Speed test: seconds per run
    #[arg(long, value_name = "S")]
    pub seconds: Option<f64>,
    /// Speed test: seconds of rest between runs
    #[arg(long, value_name = "S")]
    pub interval: Option<f64>,
    /// Speed test: which measurements, e.g. "SEQ1M Q8T1,RND4K Q1T1" (also SEQ1M Q1T1, SEQ128K Q32T1, RND4K Q32T1)
    #[arg(long, value_name = "LIST")]
    pub profiles: Option<String>,
    /// Speed test: write zeros instead of random data
    #[arg(long)]
    pub zeros: bool,
    /// Speed test: make the drive put every write on the medium before answering (strict, slow)
    #[arg(long)]
    pub write_through: bool,
    /// Sustained write: how much to write, e.g. 4G, 16G
    #[arg(long, value_name = "SIZE")]
    pub sustained_size: Option<String>,
    /// Sustained write: stop after this many seconds
    #[arg(long, value_name = "S")]
    pub sustained_seconds: Option<f64>,
    /// Surface scan: read every block (default: about 2 000 blocks spread over the drive)
    #[arg(long)]
    pub scan_full: bool,
    /// Surface scan: block size, e.g. 1M
    #[arg(long, value_name = "SIZE")]
    pub scan_block: Option<String>,
    /// Surface scan: first byte to read
    #[arg(long, value_name = "BYTES")]
    pub scan_from: Option<String>,
    /// Surface scan: stop reading here
    #[arg(long, value_name = "BYTES")]
    pub scan_to: Option<String>,
    /// Surface scan: stop at the first unreadable block
    #[arg(long)]
    pub stop_on_error: bool,
    /// Capacity check: quick (samples spread over the drive) or full (exact)
    #[arg(long, value_name = "MODE")]
    pub integrity: Option<String>,
    /// Use the system cache (only for file systems that cannot do uncached I/O)
    #[arg(long)]
    pub no_direct: bool,
    /// Save the report to this file (.json for JSON, anything else for text)
    #[arg(long, value_name = "FILE")]
    pub save: Option<PathBuf>,
    /// One JSON event per line on stdout
    #[arg(long)]
    pub json: bool,
}

fn parse_steps(list: &str) -> Result<Vec<Step>, NyError> {
    list.split(',').filter(|s| !s.trim().is_empty()).map(|s| Step::parse(s.trim()).ok_or_else(|| NyError::invalid(format!("Unknown test '{}'.", s.trim())).with_hint("Use info, speed, sustained, scan, integrity."))).collect()
}

fn size_arg(name: &str, v: &Option<String>) -> Result<Option<u64>, NyError> {
    match v {
        None => Ok(None),
        Some(s) => parse_size(s).map(Some).ok_or_else(|| NyError::invalid(format!("'{s}' is not a size for {name}.")).with_hint("Write it like 256M or 4G.")),
    }
}

/// The partition of `d` with the most free space that is mounted somewhere we can write.
fn best_mount(d: &Drive) -> Option<PathBuf> {
    d.partitions
        .iter()
        .flat_map(|p| p.mountpoints.iter())
        .filter(|m| !m.is_empty() && Path::new(m.as_str()).is_dir())
        .filter_map(|m| nyforge_core::nytest::util::free_space(Path::new(m)).map(|f| (f, PathBuf::from(m))))
        .max_by_key(|(f, _)| *f)
        .map(|(_, p)| p)
}

/// Which drive holds this folder?
fn drive_of_folder(dir: &Path) -> Option<Drive> {
    let canon = std::fs::canonicalize(dir).ok()?;
    let drives = device::list_drives().ok()?;
    drives
        .into_iter()
        .filter_map(|d| {
            let best = d.partitions.iter().flat_map(|p| p.mountpoints.iter()).filter(|m| m.as_str() != "/" || canon == Path::new("/")).filter(|m| canon.starts_with(m.as_str())).map(|m| m.len()).max()?;
            Some((best, d))
        })
        .max_by_key(|(l, _)| *l)
        .map(|(_, d)| d)
}

pub fn run(a: TestArgs) -> Res {
    let preset = match a.preset.to_ascii_lowercase().as_str() {
        "quick" => Preset::Quick,
        "standard" => Preset::Standard,
        "full" => Preset::Full,
        o => return Err(NyError::invalid(format!("Unknown level '{o}'.")).with_hint("Use quick, standard or full.")),
    };
    let mut o = Options::preset(preset);
    if let Some(t) = &a.tests {
        o.steps = parse_steps(t)?;
    }
    o.erase = a.erase;
    o.direct = !a.no_direct;
    if let Some(v) = size_arg("--size", &a.size)? {
        o.speed.size = v;
    }
    if let Some(v) = a.runs {
        o.speed.runs = v.clamp(1, 20);
    }
    if let Some(v) = a.seconds {
        o.speed.seconds = v.clamp(0.5, 120.0);
    }
    if let Some(v) = a.interval {
        o.speed.interval = v.clamp(0.0, 60.0);
    }
    if let Some(p) = &a.profiles {
        o.speed.profiles = p.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    if a.zeros {
        o.speed.pattern = Pattern::Zero;
    }
    o.speed.write_through = a.write_through;
    if let Some(v) = size_arg("--sustained-size", &a.sustained_size)? {
        o.sustained.size = v;
    }
    if let Some(v) = a.sustained_seconds {
        o.sustained.max_seconds = v.max(5.0);
    }
    if a.scan_full {
        o.scan.quick = false;
    }
    if let Some(v) = size_arg("--scan-block", &a.scan_block)? {
        o.scan.block = v.clamp(64 << 10, 64 << 20) as usize;
    }
    if let Some(v) = size_arg("--scan-from", &a.scan_from)? {
        o.scan.from = v;
    }
    if let Some(v) = size_arg("--scan-to", &a.scan_to)? {
        o.scan.to = Some(v);
    }
    o.scan.stop_on_error = a.stop_on_error;
    if let Some(m) = &a.integrity {
        o.integrity = match m.to_ascii_lowercase().as_str() {
            "quick" => CapacityTest::Quick,
            "full" => CapacityTest::Full,
            _ => return Err(NyError::invalid(format!("Unknown capacity check '{m}'.")).with_hint("Use quick or full.")),
        };
    }

    // ---- what is being tested
    let target_is_dir = Path::new(&a.target).is_dir();
    let subject = if target_is_dir {
        let dir = PathBuf::from(&a.target);
        // a folder counts only when it lies on a removable drive: nothing on an internal disk is ever tested
        match drive_of_folder(&dir) {
            Some(d) => {
                if let Some(why) = device::test_refusal(&d, a.allow_virtual) {
                    return Err(NyError::new(ErrorKind::Unsafe, why).with_hint("Name a folder on the USB stick or card."));
                }
                Subject::from_drive(&d, Some(dir))
            }
            None => return Err(NyError::new(ErrorKind::Unsafe, format!("{} is not on a removable drive NyForge can see.", dir.display())).with_hint("NyTest only tests USB sticks, SD cards and other removable drives.")),
        }
    } else {
        let d = device::resolve_stick(&a.target, a.allow_virtual)?;
        let folder = a.folder.clone().or_else(|| best_mount(&d));
        Subject::from_drive(&d, folder)
    };
    if a.erase && subject.drive.is_none() {
        return Err(NyError::invalid("--erase needs a drive.").with_hint("Name the drive itself, not a folder on it."));
    }
    let place = nytest::choose_place(&subject, &o);
    let needs_root = o.steps.iter().any(|s| matches!(s, Step::Scan | Step::Integrity)) || matches!(place, Some(nytest::Place::RawRead(_)) | Some(nytest::Place::RawWrite(_)));
    if needs_root && !is_root() {
        return Err(NyError::new(ErrorKind::Permission, "Reading a whole drive needs administrator rights.").with_hint(rerun_hint(&format!("nyforge test {}", a.target))));
    }
    let ui0 = Ui::new();
    let erasing = o.erase && (o.steps.iter().any(|s| s.destructive()) || matches!(place, Some(nytest::Place::RawWrite(_))));
    if erasing && !a.yes {
        if a.json {
            return Err(NyError::new(ErrorKind::InvalidInput, "--json needs --yes: there is nobody to ask.").with_hint("Add --yes once you have checked the target."));
        }
        if let Some(d) = &subject.drive {
            confirm_erase(&[d], "test data (the tests write over the whole drive)", &ui0)?;
        }
    }

    // ---- run
    let cancel = install_ctrlc();
    let json = a.json;
    let mut ui = Ui::new();
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let mut step_line = String::new();
    let mut last_plain = std::time::Instant::now() - std::time::Duration::from_secs(10);
    let r = {
        let mut emit = |ev: TestEvent| {
            if json {
                println!("{}", serde_json::to_string(&ev).unwrap_or_default());
                let _ = std::io::stdout().flush();
                return;
            }
            match &ev {
                TestEvent::NytestStart { target, size, mode, .. } => {
                    let how = match mode.as_str() {
                        "file" => "with a test file on the drive",
                        "raw_read" => "reading straight from the device (nothing is written)",
                        "raw_write" => "straight on the device (it is being erased)",
                        _ => "",
                    };
                    eprintln!("Testing {} {} {}", ui0.bold(target), if *size > 0 { format!("({})", human_size(*size)) } else { String::new() }, ui0.dim(how));
                }
                TestEvent::NytestStep { label, index, of, .. } => {
                    ui.finish_line();
                    if tty {
                        eprint!("\r\x1b[2K");
                    }
                    eprintln!("[{index}/{of}] {label}");
                    step_line = label.clone();
                }
                TestEvent::NytestLive { label, fraction, mbps, iops, .. } => {
                    let line = format!("  {label}: {:.0}%  {:.1} MB/s{}", fraction * 100.0, mbps, if *iops > 0.0 { format!("  {:.0} IOPS", iops) } else { String::new() });
                    if tty {
                        eprint!("\r\x1b[2K{line}");
                        let _ = std::io::stderr().flush();
                    } else if last_plain.elapsed().as_secs() >= 10 {
                        last_plain = std::time::Instant::now();
                        eprintln!("{line}");
                    }
                }
                TestEvent::NytestSpeed { result } => {
                    if tty {
                        eprint!("\r\x1b[2K");
                    }
                    eprintln!("  {:<24} {:>9.1} MB/s  {:>9.0} IOPS   p50 {} µs · p99 {} µs", result.name, result.mbps, result.iops, result.lat.p50_us, result.lat.p99_us);
                }
                TestEvent::NytestNote { message } => eprintln!("  {}", ui0.dim(message)),
                _ => {}
            }
        };
        nytest::run(&subject, &o, &cancel, &mut emit)
    };
    let _ = step_line;
    if tty && !json {
        eprint!("\r\x1b[2K");
    }
    let rep = match r {
        Ok(r) => r,
        Err(e) => {
            if json {
                print_event(&nyforge_core::progress::Event::Error { error: e.clone() });
            }
            return Err(e);
        }
    };
    if let Some(p) = &a.save {
        let text = if p.extension().map(|e| e.eq_ignore_ascii_case("json")).unwrap_or(false) { serde_json::to_string_pretty(&rep).unwrap_or_default() } else { nytest::report_text(&rep) };
        std::fs::write(p, text).map_err(|e| NyError::from_io(&e, &format!("saving the report to {}", p.display())))?;
        if !json {
            eprintln!("Report saved to {}", p.display());
        }
    }
    if !json {
        eprintln!();
        println!("{}", nytest::report_text(&rep));
        eprintln!("{} in {}", ui.dim("Done"), human_duration(rep.elapsed_secs));
    }
    Ok(if rep.verdict.level == Severity::Bad { 2 } else { 0 })
}
