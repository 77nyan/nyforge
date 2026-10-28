//! The surface scan: read the whole drive, read-only, and time every block.
//!
//! A drive that is failing rarely stops working all at once. First some areas answer slowly (the controller is retrying and
//! correcting), then some do not answer. This finds them while the data is still there. It writes nothing.

use super::{check, TestEvent};
use crate::alignedbuf::AlignedBuf;
use crate::error::{ErrorKind, NyError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Read about 2 000 blocks spread over the drive instead of all of it
    pub quick: bool,
    pub block: usize,
    pub from: u64,
    pub to: Option<u64>,
    pub stop_on_error: bool,
    /// Length of the map shown while it runs
    pub segments: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions { quick: true, block: 1 << 20, from: 0, to: None, stop_on_error: false, segments: 400 }
    }
}

const QUICK_BLOCKS: u64 = 2048;
const UNIT: u64 = 4096;
const MAX_REPORTED_ERRORS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BadSpot {
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub quick: bool,
    pub scanned_bytes: u64,
    pub blocks: u64,
    pub seconds: f64,
    pub avg_mbps: f64,
    /// the slowest 1% of blocks, as a speed
    pub min_mbps: f64,
    pub max_mbps: f64,
    /// 5% of the blocks were slower than this
    pub p5_mbps: f64,
    pub slow_blocks: u64,
    pub error_blocks: u64,
    /// Bytes that could not be read (found by re-reading the failed blocks in small pieces)
    pub bad_bytes: u64,
    pub first_error: Option<u64>,
    pub errors: Vec<BadSpot>,
    /// 0 fine, 1 a little slow, 2 slow, 3 unreadable; one entry per segment of the scanned range
    pub map: Vec<u8>,
    pub map_mbps: Vec<f32>,
}

/// How a block's time compares with its neighbours'.
pub fn classify(dt: Duration, baseline: Option<Duration>) -> u8 {
    let ms = dt.as_secs_f64() * 1000.0;
    if ms >= 250.0 {
        return 2;
    }
    let Some(b) = baseline else { return 0 };
    let ratio = dt.as_secs_f64() / b.as_secs_f64().max(1e-6);
    if ms < 20.0 || ratio < 3.0 {
        0
    } else if ratio < 10.0 {
        1
    } else {
        2
    }
}

fn median_of(win: &[Duration]) -> Option<Duration> {
    if win.len() < 8 {
        return None;
    }
    let mut v = win.to_vec();
    v.sort();
    Some(v[v.len() / 2])
}

pub fn run(dev: &Path, size: u64, o: &ScanOptions, direct: bool, cancel: &AtomicBool, emit: &mut dyn FnMut(TestEvent)) -> Result<ScanResult> {
    let mut direct_on = direct;
    let file = match crate::sys::open_read(dev, crate::sys::TargetKind::BlockDevice, direct_on) {
        Ok(f) => f,
        Err(e) if direct_on && e.raw_os_error() == Some(22) => {
            direct_on = false;
            crate::sys::open_read(dev, crate::sys::TargetKind::BlockDevice, false).map_err(|e| NyError::from_io(&e, &format!("opening {}", dev.display())))?
        }
        Err(e) => return Err(NyError::from_io(&e, &format!("opening {}", dev.display()))),
    };
    let _ = direct_on;
    run_with(&mut |buf, off| crate::sys::read_exact_at(&file, buf, off), size, o, cancel, emit)
}

/// The scan over any reader: `read(buf, offset)` fills the whole buffer or fails.
pub fn run_with(read: &mut dyn FnMut(&mut [u8], u64) -> std::io::Result<()>, size: u64, o: &ScanOptions, cancel: &AtomicBool, emit: &mut dyn FnMut(TestEvent)) -> Result<ScanResult> {
    let block = (o.block.max(64 << 10) as u64 / UNIT * UNIT) as usize;
    let end = (o.to.unwrap_or(size).min(size)) / UNIT * UNIT;
    let from = o.from / UNIT * UNIT;
    if end <= from + UNIT {
        return Err(NyError::invalid("There is nothing to scan: the range is empty."));
    }
    let total_blocks = (end - from).div_ceil(block as u64);
    let planned = if o.quick { total_blocks.min(QUICK_BLOCKS) } else { total_blocks };
    let seg_n = (o.segments.max(8) as u64).min(planned).max(1) as usize;
    let mut buf = AlignedBuf::new(block);

    let started = Instant::now();
    let mut window: Vec<Duration> = Vec::with_capacity(128);
    let mut speeds: Vec<f32> = Vec::with_capacity(planned as usize);
    let mut map = vec![0u8; seg_n];
    let mut map_bytes = vec![0u64; seg_n];
    let mut map_time = vec![0f64; seg_n];
    let mut seg_emitted = 0usize;
    let (mut slow, mut errs, mut bad_bytes) = (0u64, 0u64, 0u64);
    let mut first_error = None;
    let mut spots: Vec<BadSpot> = vec![];
    let mut scanned = 0u64;
    let mut recent: Vec<(Instant, u64)> = vec![];
    let mut probes_left = 40_000u64;
    let mut last_live = Instant::now() - Duration::from_secs(1);

    for i in 0..planned {
        check(cancel)?;
        let idx = if o.quick { i * total_blocks / planned } else { i };
        let off = from + idx * block as u64;
        let n = (block as u64).min(end - off) as usize;
        let n = n / UNIT as usize * UNIT as usize;
        if n == 0 {
            continue;
        }
        let t0 = Instant::now();
        let r = read(&mut buf.as_mut_slice()[..n], off);
        let dt = t0.elapsed();
        let seg = (i * seg_n as u64 / planned) as usize;
        let state;
        match r {
            Ok(()) => {
                state = classify(dt, median_of(&window));
                if window.len() >= 128 {
                    window.remove(0);
                }
                window.push(dt);
                speeds.push((n as f64 / dt.as_secs_f64().max(1e-6) / 1e6) as f32);
                if state >= 2 {
                    slow += 1;
                }
            }
            Err(e) => {
                if crate::error::io_is_gone(&e) {
                    return Err(NyError::new(ErrorKind::DeviceGone, "The drive disappeared during the scan.").with_hint("Reconnect it and run the scan again."));
                }
                state = 3;
                errs += 1;
                first_error.get_or_insert(off);
                // Where inside the block is the trouble? Read it again in small pieces (time-boxed: a dying drive can take
                // seconds for every failed read).
                let probe_start = Instant::now();
                let mut bad_here = 0u64;
                let mut p = 0usize;
                let mut spot_start: Option<u64> = None;
                while p < n {
                    let pn = UNIT as usize;
                    let bad = if probes_left == 0 || probe_start.elapsed() > Duration::from_secs(30) {
                        true
                    } else {
                        probes_left -= 1;
                        read(&mut buf.as_mut_slice()[..pn], off + p as u64).is_err()
                    };
                    if bad {
                        bad_here += pn as u64;
                        spot_start.get_or_insert(off + p as u64);
                    } else if let Some(s) = spot_start.take() {
                        if spots.len() < MAX_REPORTED_ERRORS {
                            spots.push(BadSpot { offset: s, len: off + p as u64 - s });
                        }
                    }
                    p += pn;
                    check(cancel)?;
                }
                if let Some(s) = spot_start.take() {
                    if spots.len() < MAX_REPORTED_ERRORS {
                        spots.push(BadSpot { offset: s, len: off + n as u64 - s });
                    }
                }
                bad_bytes += bad_here;
                if o.stop_on_error {
                    map[seg] = 3;
                    scanned += n as u64;
                    break;
                }
            }
        }
        scanned += n as u64;
        map[seg] = map[seg].max(state);
        map_bytes[seg] += n as u64;
        map_time[seg] += dt.as_secs_f64();
        // segments before this one are finished
        while seg_emitted < seg {
            let mb = if map_time[seg_emitted] > 0.0 { map_bytes[seg_emitted] as f64 / map_time[seg_emitted] / 1e6 } else { 0.0 };
            emit(TestEvent::NytestMap { index: seg_emitted, of: seg_n, state: map[seg_emitted], mbps: mb });
            seg_emitted += 1;
        }
        let now = Instant::now();
        recent.push((now, scanned));
        recent.retain(|(t, _)| now.duration_since(*t) < Duration::from_secs(2));
        if last_live.elapsed() >= Duration::from_millis(200) {
            last_live = now;
            let mbps = match (recent.first(), recent.last()) {
                (Some(a), Some(b)) if b.0 > a.0 => (b.1 - a.1) as f64 / b.0.duration_since(a.0).as_secs_f64() / 1e6,
                _ => 0.0,
            };
            emit(TestEvent::NytestLive { step: "scan".into(), label: "Reading the drive".into(), fraction: (i + 1) as f64 / planned as f64, mbps, iops: 0.0 });
        }
    }
    while seg_emitted < seg_n {
        let mb = if map_time[seg_emitted] > 0.0 { map_bytes[seg_emitted] as f64 / map_time[seg_emitted] / 1e6 } else { 0.0 };
        emit(TestEvent::NytestMap { index: seg_emitted, of: seg_n, state: map[seg_emitted], mbps: mb });
        seg_emitted += 1;
    }
    let seconds = started.elapsed().as_secs_f64();
    let mut sorted = speeds.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| -> f64 {
        if sorted.is_empty() {
            0.0
        } else {
            sorted[((sorted.len() - 1) as f64 * p) as usize] as f64
        }
    };
    Ok(ScanResult {
        quick: o.quick && planned < total_blocks,
        scanned_bytes: scanned,
        blocks: planned,
        seconds,
        avg_mbps: if seconds > 0.0 { scanned as f64 / seconds / 1e6 } else { 0.0 },
        min_mbps: pct(0.01),
        max_mbps: sorted.last().copied().unwrap_or(0.0) as f64,
        p5_mbps: pct(0.05),
        slow_blocks: slow,
        error_blocks: errs,
        bad_bytes,
        first_error,
        errors: spots,
        map_mbps: (0..seg_n).map(|s| if map_time[s] > 0.0 { (map_bytes[s] as f64 / map_time[s] / 1e6) as f32 } else { 0.0 }).collect(),
        map,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_with(size: u64, bad: Vec<(u64, u64)>, slow: Vec<(u64, u64)>, o: ScanOptions) -> (ScanResult, Vec<TestEvent>) {
        let mut ev = vec![];
        let r = run_with(
            &mut |buf, off| {
                let end = off + buf.len() as u64;
                if bad.iter().any(|&(a, b)| off < b && end > a) {
                    return Err(std::io::Error::from_raw_os_error(5));
                }
                if slow.iter().any(|&(a, b)| off < b && end > a) {
                    std::thread::sleep(Duration::from_millis(300));
                }
                buf.fill(7);
                Ok(())
            },
            size,
            &o,
            &AtomicBool::new(false),
            &mut |e| ev.push(e),
        )
        .unwrap();
        (r, ev)
    }

    #[test]
    fn a_clean_drive_reads_clean_and_the_map_is_complete() {
        let (r, ev) = scan_with(64 << 20, vec![], vec![], ScanOptions { quick: false, ..Default::default() });
        assert_eq!((r.blocks, r.error_blocks, r.slow_blocks, r.bad_bytes), (64, 0, 0, 0));
        assert_eq!(r.scanned_bytes, 64 << 20);
        assert!(r.map.iter().all(|&s| s == 0));
        let maps = ev.iter().filter(|e| matches!(e, TestEvent::NytestMap { .. })).count();
        assert_eq!(maps, r.map.len(), "every segment is announced exactly once");
        assert!(r.first_error.is_none() && r.errors.is_empty());
    }

    #[test]
    fn bad_areas_are_found_located_and_measured() {
        // 8 KiB unreadable at 10 MiB + 4096, and a whole block at 40 MiB
        let bad = vec![((10 << 20) + 4096, (10 << 20) + 12288), (40 << 20, 41 << 20)];
        let (r, _) = scan_with(64 << 20, bad, vec![], ScanOptions { quick: false, segments: 64, ..Default::default() });
        assert_eq!(r.error_blocks, 2);
        assert_eq!(r.first_error, Some(10 << 20));
        assert_eq!(r.bad_bytes, 8192 + (1 << 20), "found by reading the failed blocks in pieces");
        assert_eq!(r.errors[0].offset, (10 << 20) + 4096);
        assert_eq!(r.errors[0].len, 8192);
        assert_eq!((r.errors[1].offset, r.errors[1].len), (40 << 20, 1 << 20));
        assert_eq!(r.map[10], 3);
        assert_eq!(r.map[40], 3);
        assert_eq!(r.map.iter().filter(|&&s| s == 3).count(), 2);
    }

    #[test]
    fn slow_blocks_are_flagged_and_stop_on_error_stops() {
        let (r, _) = scan_with(32 << 20, vec![], vec![(16 << 20, 17 << 20)], ScanOptions { quick: false, segments: 32, ..Default::default() });
        assert_eq!(r.slow_blocks, 1);
        assert_eq!(r.map[16], 2);
        let (r, _) = scan_with(64 << 20, vec![(5 << 20, 6 << 20)], vec![], ScanOptions { quick: false, stop_on_error: true, ..Default::default() });
        assert_eq!(r.error_blocks, 1);
        assert!(r.scanned_bytes < 64 << 20);
    }

    #[test]
    fn quick_scan_samples_and_says_so() {
        let (r, _) = scan_with(8 << 30, vec![], vec![], ScanOptions { quick: true, ..Default::default() });
        assert!(r.quick);
        assert_eq!(r.blocks, QUICK_BLOCKS);
        let (r, _) = scan_with(64 << 20, vec![], vec![], ScanOptions { quick: true, ..Default::default() });
        assert!(!r.quick, "a drive this small is read whole");
        assert_eq!(r.blocks, 64);
    }

    #[test]
    fn empty_range_is_refused() {
        let e = run_with(&mut |_, _| Ok(()), 1 << 20, &ScanOptions { from: 1 << 20, ..Default::default() }, &AtomicBool::new(false), &mut |_| {}).err().unwrap();
        assert_eq!(e.kind, ErrorKind::InvalidInput);
    }

    #[test]
    fn classification() {
        let ms = Duration::from_millis;
        let base = Some(ms(30));
        assert_eq!(classify(ms(32), base), 0);
        assert_eq!(classify(ms(80), base), 0, "under 3x");
        assert_eq!(classify(ms(120), base), 1);
        assert_eq!(classify(ms(400), base), 2);
        assert_eq!(classify(ms(2), Some(ms(1))), 0, "fast drives: a few ms is noise");
        assert_eq!(classify(ms(300), None), 2, "a quarter second is slow whatever the neighbours do");
        assert_eq!(classify(ms(100), None), 0);
    }

    #[test]
    fn median_needs_enough() {
        assert!(median_of(&[Duration::from_millis(1); 7]).is_none());
        assert_eq!(median_of(&[Duration::from_millis(5); 8]), Some(Duration::from_millis(5)));
    }
}
