//! The speed test: CrystalDiskMark's measurements, done honestly.
//!
//! What CrystalDiskMark does (and what is kept): a test file of N bytes, then for each profile — sequential 1 MiB reads and
//! writes at queue depth 8 and 1, random 4 KiB at queue depth 32 and 1 — a timed run, repeated, with a pause between runs.
//! Uncached I/O and data that cannot be compressed, so nothing is served from RAM or squeezed on the way.
//!
//! What is added, because a single number hides what matters on USB sticks and SD cards:
//! * every run is kept and the *median* is reported (one lucky run does not make the headline), with the spread visible;
//! * latency percentiles (p50 / p90 / p99 / p99.9 / max), because a drive that answers in 0.2 ms usually and 800 ms
//!   sometimes feels broken, whatever its MB/s;
//! * IOPS next to MB/s;
//! * the write test counts the final flush, so a write cache cannot flatter the result;
//! * it tells when the numbers cannot be trusted (no uncached I/O on this file system; an empty drive whose unwritten
//!   areas read back without touching the flash).

use super::engine::{fill_file, open_test_file, run_job, Access, Hist, Job, Latency, Limits, Op, Pattern};
use super::{check, Place, Subject, TestEvent};
use crate::error::{ErrorKind, NyError, Result};
use crate::format::human_size;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: &'static str,
    pub access: Access,
    pub block: usize,
    pub qd: usize,
}

pub const PROFILES: [Profile; 5] = [
    Profile { name: "SEQ1M Q8T1", access: Access::Sequential, block: 1 << 20, qd: 8 },
    Profile { name: "SEQ1M Q1T1", access: Access::Sequential, block: 1 << 20, qd: 1 },
    Profile { name: "SEQ128K Q32T1", access: Access::Sequential, block: 128 << 10, qd: 32 },
    Profile { name: "RND4K Q32T1", access: Access::Random, block: 4096, qd: 32 },
    Profile { name: "RND4K Q1T1", access: Access::Random, block: 4096, qd: 1 },
];

/// The four CrystalDiskMark shows by default.
pub fn default_profiles() -> Vec<&'static Profile> {
    [0usize, 1, 3, 4].iter().map(|&i| &PROFILES[i]).collect()
}

pub fn find_profile(name: &str) -> Option<&'static Profile> {
    let key: String = name.to_ascii_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    PROFILES.iter().find(|p| p.name.to_ascii_lowercase().replace(' ', "") == key || p.name.to_ascii_lowercase().replace(' ', "").starts_with(&format!("{key}t")))
}

#[derive(Debug, Clone)]
pub struct SpeedOptions {
    /// Size of the test file (or of the part of the device that is used)
    pub size: u64,
    /// Timed runs per measurement
    pub runs: u32,
    /// Length of one run in seconds
    pub seconds: f64,
    /// Pause between runs in seconds
    pub interval: f64,
    /// Profile names; empty means the default four
    pub profiles: Vec<String>,
    pub pattern: Pattern,
    /// Make the drive put every write on the medium before answering (strict, slow)
    pub write_through: bool,
}

impl Default for SpeedOptions {
    fn default() -> Self {
        SpeedOptions { size: 1 << 30, runs: 3, seconds: 5.0, interval: 1.0, profiles: vec![], pattern: Pattern::Random, write_through: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedResult {
    /// "SEQ1M Q8T1 read"
    pub name: String,
    /// "SEQ1M Q8T1"
    pub profile: String,
    pub op: Op,
    pub access: Access,
    pub block: u64,
    pub qd: u32,
    /// Median of the runs
    pub mbps: f64,
    pub iops: f64,
    /// MB/s of every run
    pub runs: Vec<f64>,
    pub lat: Latency,
}

impl SpeedResult {
    pub fn spread_pct(&self) -> f64 {
        let (lo, hi) = self.runs.iter().fold((f64::MAX, 0f64), |(l, h), &v| (l.min(v), h.max(v)));
        if self.runs.len() < 2 || hi <= 0.0 {
            0.0
        } else {
            (hi - lo) / hi * 100.0
        }
    }
}

pub fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

// ---------------------------------------------------------------------------------------- the thing tests run on

/// An open file or device with a region to work in. A temporary file is removed when this is dropped.
pub struct Bench {
    pub file: File,
    /// uncached I/O is really on
    pub direct: bool,
    pub start: u64,
    pub len: u64,
    pub writable: bool,
    remove: Option<PathBuf>,
    /// keeps the drive claimed (Windows volume locks) for as long as the bench is open
    _prep: Option<crate::sys::DevicePrep>,
}

impl Bench {
    /// A temporary file that is removed with the bench.
    pub fn temp_file(file: File, direct: bool, len: u64, path: PathBuf) -> Bench {
        Bench { file, direct, start: 0, len, writable: true, remove: Some(path), _prep: None }
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        if let Some(p) = self.remove.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}-{:08x}.tmp", std::process::id(), (crate::usbformat::prng::random_seed() & 0xFFFF_FFFF) as u32)
}

/// Open the test place: create and fill a temporary file, or open the device.
/// `fill` writes data into the region first (a file always; a device only when it is being erased anyway).
pub fn open_bench(s: &Subject, place: &Place, size: u64, direct: bool, unmount: bool, fill: bool, step: &str, cancel: &AtomicBool, emit: &mut dyn FnMut(TestEvent), notes: &mut Vec<String>) -> Result<Bench> {
    const MIB: u64 = 1 << 20;
    match place {
        Place::File(folder) => {
            if !folder.is_dir() {
                return Err(NyError::invalid(format!("{} is not a folder.", folder.display())).with_hint("Pick a folder on the drive that is being tested (the drive has to be mounted)."));
            }
            let mut size = size / MIB * MIB;
            if let Some(free) = crate::nytest::util::free_space(folder) {
                let room = free.saturating_sub(64 * MIB) / MIB * MIB;
                if size > room {
                    if room < 16 * MIB {
                        return Err(NyError::new(ErrorKind::NoSpace, format!("{} has only {} free: not enough for a speed test.", folder.display(), human_size(free))).with_hint("Free some space on the drive, or test the drive itself."));
                    }
                    notes.push(format!("The test file was made {} instead of {} because that is what the drive has free.", human_size(room), human_size(size)));
                    size = room;
                }
            }
            if size < 16 * MIB {
                return Err(NyError::invalid("The test size must be at least 16 MiB."));
            }
            let path = folder.join(unique_name("nytest"));
            let opened = open_test_file(&path, true, true, direct).map_err(|e| NyError::from_io(&e, &format!("creating the test file in {}", folder.display())))?;
            let bench = Bench { file: opened.file, direct: opened.direct, start: 0, len: size, writable: true, remove: Some(path), _prep: None };
            if direct && !opened.direct {
                notes.push("This file system cannot do uncached I/O, so the system cache may flatter the results. Test the drive itself (it has to be erasable) for exact numbers.".into());
            }
            emit(TestEvent::NytestLive { step: step.into(), label: "Preparing the test file".into(), fraction: 0.0, mbps: 0.0, iops: 0.0 });
            let t = std::time::Instant::now();
            fill_file(&bench.file, size, cancel, &mut |done| {
                let el = t.elapsed().as_secs_f64().max(0.001);
                emit(TestEvent::NytestLive { step: step.into(), label: "Preparing the test file".into(), fraction: done as f64 / size as f64, mbps: done as f64 / el / 1e6, iops: 0.0 });
            })?;
            Ok(bench)
        }
        Place::RawRead(dev) => {
            let size = size.min(s.size) / MIB * MIB;
            let mut dir = direct;
            let file = match crate::sys::open_read(dev, crate::sys::TargetKind::BlockDevice, dir) {
                Ok(f) => f,
                Err(e) if dir && e.raw_os_error() == Some(22) => {
                    dir = false;
                    crate::sys::open_read(dev, crate::sys::TargetKind::BlockDevice, false).map_err(|e| NyError::from_io(&e, &format!("opening {}", dev.display())))?
                }
                Err(e) => return Err(NyError::from_io(&e, &format!("opening {}", dev.display()))),
            };
            if size < 16 * MIB {
                return Err(NyError::invalid("The drive is too small for a speed test."));
            }
            let b = Bench { file, direct: dir, start: 0, len: size, writable: false, remove: None, _prep: None };
            if looks_empty(&b) {
                notes.push("The drive looks empty. Drives answer reads of areas that were never written without touching the flash, so read speeds on an empty drive can be far too high. Test with some files on it, or allow erasing it so the test can fill it first.".into());
            }
            Ok(b)
        }
        Place::RawWrite(dev) => {
            let prep = match &s.drive {
                Some(d) => Some(crate::sys::prepare_device_for_erase(d, unmount)?),
                None => None,
            };
            let size = size.min(s.size) / MIB * MIB;
            if size < 16 * MIB {
                return Err(NyError::invalid("The drive is too small for a speed test."));
            }
            let file = crate::sys::open_rw_direct(dev).map_err(|e| NyError::from_io(&e, &format!("opening {}", dev.display())))?;
            let b = Bench { file, direct: true, start: 0, len: size, writable: true, remove: None, _prep: prep };
            if fill {
                emit(TestEvent::NytestLive { step: step.into(), label: "Filling the drive area with data".into(), fraction: 0.0, mbps: 0.0, iops: 0.0 });
                let t = std::time::Instant::now();
                fill_file(&b.file, size, cancel, &mut |done| {
                    let el = t.elapsed().as_secs_f64().max(0.001);
                    emit(TestEvent::NytestLive { step: step.into(), label: "Filling the drive area with data".into(), fraction: done as f64 / size as f64, mbps: done as f64 / el / 1e6, iops: 0.0 });
                })?;
            }
            Ok(b)
        }
    }
}

/// A drive with nothing written reads back zeros from areas it never stored.
fn looks_empty(b: &Bench) -> bool {
    let mut buf = crate::alignedbuf::AlignedBuf::new(64 << 10);
    let n = (b.len / (64 << 10)).max(1);
    let mut seen = 0;
    for i in 0..32u64 {
        let off = (i * 2654435761 % n) * (64 << 10);
        if crate::sys::read_exact_at(&b.file, buf.as_mut_slice(), off).is_err() {
            continue;
        }
        seen += 1;
        if !crate::alignedbuf::is_all_zero(buf.as_slice()) {
            return false;
        }
    }
    seen > 0
}

fn drop_cache(f: &File) {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
    }
    let _ = f;
}

fn pause(secs: f64, cancel: &AtomicBool) -> Result<()> {
    let end = std::time::Instant::now() + Duration::from_secs_f64(secs.max(0.0));
    while std::time::Instant::now() < end {
        check(cancel)?;
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------- the run

pub fn run(s: &Subject, place: &Place, o: &SpeedOptions, direct: bool, unmount: bool, cancel: &AtomicBool, emit: &mut dyn FnMut(TestEvent)) -> Result<(Vec<SpeedResult>, Vec<String>)> {
    let mut profiles: Vec<&Profile> = vec![];
    for n in &o.profiles {
        profiles.push(find_profile(n).ok_or_else(|| NyError::invalid(format!("Unknown test '{n}'.")).with_hint("Use SEQ1M Q8T1, SEQ1M Q1T1, SEQ128K Q32T1, RND4K Q32T1 or RND4K Q1T1."))?);
    }
    if profiles.is_empty() {
        profiles = default_profiles();
    }
    let mut notes = vec![];
    let bench = open_bench(s, place, o.size, direct, unmount, true, "speed", cancel, emit, &mut notes)?;
    let ops: Vec<Op> = if bench.writable { vec![Op::Read, Op::Write] } else { vec![Op::Read] };
    let units = (profiles.len() * ops.len()) as f64 * o.runs.max(1) as f64;
    let mut done_units = 0f64;
    let mut out = vec![];
    for p in &profiles {
        for &op in &ops {
            check(cancel)?;
            let label = format!("{} {}", p.name, if op == Op::Read { "read" } else { "write" });
            let job = Job {
                op,
                access: p.access,
                block: p.block,
                qd: p.qd,
                start: bench.start,
                len: bench.len,
                pattern: o.pattern,
                wrap: true,
                sync_each_write: o.write_through && op == Op::Write,
                seed: crate::usbformat::prng::random_seed(),
            };
            let mut runs = vec![];
            let mut iops = vec![];
            let mut hist = Hist::default();
            for r in 0..o.runs.max(1) {
                check(cancel)?;
                if !bench.direct && op == Op::Read {
                    drop_cache(&bench.file);
                }
                let dur = Duration::from_secs_f64(o.seconds.max(0.5));
                let base = done_units;
                let m = run_job(&bench.file, &job, &Limits { duration: dur, max_bytes: None, cancel }, Duration::from_millis(200), &mut |el, b, n| {
                    let secs = el.as_secs_f64().max(0.001);
                    emit(TestEvent::NytestLive {
                        step: "speed".into(),
                        label: label.clone(),
                        fraction: ((base + (secs / dur.as_secs_f64()).min(1.0)) / units).min(1.0),
                        mbps: b as f64 / secs / 1e6,
                        iops: n as f64 / secs,
                    });
                })?;
                runs.push(m.mbps());
                iops.push(m.iops());
                hist.merge(&m.hist);
                done_units += 1.0;
                if r + 1 < o.runs.max(1) {
                    pause(o.interval, cancel)?;
                }
            }
            let res = SpeedResult {
                name: label.clone(),
                profile: p.name.to_string(),
                op,
                access: p.access,
                block: p.block as u64,
                qd: p.qd as u32,
                mbps: median(&runs),
                iops: median(&iops),
                runs,
                lat: hist.summary(),
            };
            emit(TestEvent::NytestSpeed { result: res.clone() });
            out.push(res);
            pause(o.interval.min(1.0), cancel)?;
        }
    }
    Ok((out, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians_and_names() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0]), 2.5);
        assert_eq!(median(&[]), 0.0);
        assert_eq!(find_profile("rnd4k q1t1").unwrap().qd, 1);
        assert_eq!(find_profile("SEQ1M Q8").unwrap().qd, 8);
        assert_eq!(find_profile("seq1m q1").unwrap().qd, 1);
        assert!(find_profile("nope").is_none());
        assert_eq!(default_profiles().len(), 4);
    }
}
